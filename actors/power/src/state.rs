// Copyright 2019-2022 ChainSafe Systems
// SPDX-License-Identifier: Apache-2.0, MIT

use std::ops::Neg;

use anyhow::{anyhow, Context};
use cid::Cid;
use fil_actors_runtime::{
    actor_error, make_empty_map, make_map_with_root, make_map_with_root_and_bitwidth,
    ActorDowncast, ActorError, Map, Multimap,
};
use fvm_ipld_hamt::BytesKey;
use fvm_shared::address::Address;
use fvm_shared::bigint::{bigint_ser, BigInt};
use fvm_shared::blockstore::Blockstore;
use fvm_shared::clock::ChainEpoch;
use fvm_shared::econ::TokenAmount;
use fvm_shared::encoding::tuple::*;
use fvm_shared::encoding::{Cbor, RawBytes};
use fvm_shared::error::ExitCode;
use fvm_shared::sector::{RegisteredPoStProof, StoragePower};
use fvm_shared::smooth::{AlphaBetaFilter, FilterEstimate, DEFAULT_ALPHA, DEFAULT_BETA};
use fvm_shared::HAMT_BIT_WIDTH;
use integer_encoding::VarInt;
use lazy_static::lazy_static;
use num_traits::Signed;

use super::{CONSENSUS_MINER_MIN_MINERS, CRON_QUEUE_AMT_BITWIDTH, CRON_QUEUE_HAMT_BITWIDTH};

lazy_static! {
    /// genesis power in bytes = 750,000 GiB
    static ref INITIAL_QA_POWER_ESTIMATE_POSITION: BigInt = BigInt::from(750_000) * (1 << 30);
    /// max chain throughput in bytes per epoch = 120 ProveCommits / epoch = 3,840 GiB
    static ref INITIAL_QA_POWER_ESTIMATE_VELOCITY: BigInt = BigInt::from(3_840) * (1 << 30);
}

/// Storage power actor state
#[derive(Default, Serialize_tuple, Deserialize_tuple)]
pub struct State {
    #[serde(with = "bigint_ser")]
    pub total_raw_byte_power: StoragePower,
    #[serde(with = "bigint_ser")]
    pub total_bytes_committed: StoragePower,
    #[serde(with = "bigint_ser")]
    pub total_quality_adj_power: StoragePower,
    #[serde(with = "bigint_ser")]
    pub total_qa_bytes_committed: StoragePower,
    #[serde(with = "bigint_ser")]
    pub total_pledge_collateral: TokenAmount,

    #[serde(with = "bigint_ser")]
    pub this_epoch_raw_byte_power: StoragePower,
    #[serde(with = "bigint_ser")]
    pub this_epoch_quality_adj_power: StoragePower,
    #[serde(with = "bigint_ser")]
    pub this_epoch_pledge_collateral: TokenAmount,
    pub this_epoch_qa_power_smoothed: FilterEstimate,

    pub miner_count: i64,
    /// Number of miners having proven the minimum consensus power.
    pub miner_above_min_power_count: i64,

    /// A queue of events to be triggered by cron, indexed by epoch.
    pub cron_event_queue: Cid, // Multimap, (HAMT[ChainEpoch]AMT[CronEvent]

    /// First epoch in which a cron task may be stored. Cron will iterate every epoch between this
    /// and the current epoch inclusively to find tasks to execute.
    pub first_cron_epoch: ChainEpoch,

    /// Claimed power for each miner.
    pub claims: Cid, // Map, HAMT[address]Claim

    pub proof_validation_batch: Option<Cid>,
}

impl State {
    pub fn new<BS: Blockstore>(store: &BS) -> anyhow::Result<State> {
        let empty_map = make_empty_map::<_, ()>(store, HAMT_BIT_WIDTH)
            .flush()
            .map_err(|e| anyhow!("Failed to create empty map: {}", e))?;

        let empty_mmap = Multimap::new(store, CRON_QUEUE_HAMT_BITWIDTH, CRON_QUEUE_AMT_BITWIDTH)
            .root()
            .map_err(|e| {
                e.downcast_default(ExitCode::ErrIllegalState, "Failed to get empty multimap cid")
            })?;
        Ok(State {
            cron_event_queue: empty_mmap,
            claims: empty_map,
            this_epoch_qa_power_smoothed: FilterEstimate {
                position: INITIAL_QA_POWER_ESTIMATE_POSITION.clone(),
                velocity: INITIAL_QA_POWER_ESTIMATE_VELOCITY.clone(),
            },
            ..Default::default()
        })
    }

    pub fn into_total_locked(self) -> TokenAmount {
        self.total_pledge_collateral
    }

    /// Checks power actor state for if miner meets minimum consensus power.
    pub fn miner_nominal_power_meets_consensus_minimum<BS: Blockstore>(
        &self,
        s: &BS,
        miner: &Address,
    ) -> anyhow::Result<bool> {
        let claims = make_map_with_root_and_bitwidth(&self.claims, s, HAMT_BIT_WIDTH)?;

        let claim =
            get_claim(&claims, miner)?.ok_or_else(|| anyhow!("no claim for actor: {}", miner))?;

        let miner_nominal_power = &claim.raw_byte_power;
        let miner_min_power = consensus_miner_min_power(claim.window_post_proof_type)
            .context("could not get miner min power from proof type: {}")?;

        if miner_nominal_power >= &miner_min_power {
            // If miner is larger than min power requirement, valid
            Ok(true)
        } else if self.miner_above_min_power_count >= CONSENSUS_MINER_MIN_MINERS {
            // if min consensus miners requirement met, return false
            Ok(false)
        } else {
            // if fewer miners than consensus minimum, return true if non-zero power
            Ok(miner_nominal_power.is_positive())
        }
    }

    pub fn miner_power<BS: Blockstore>(
        &self,
        s: &BS,
        miner: &Address,
    ) -> anyhow::Result<Option<Claim>> {
        let claims = make_map_with_root(&self.claims, s)?;
        get_claim(&claims, miner).map(|s| s.cloned())
    }

    pub(super) fn add_to_claim<BS: Blockstore>(
        &mut self,
        claims: &mut Map<BS, Claim>,
        miner: &Address,
        power: &StoragePower,
        qa_power: &StoragePower,
    ) -> anyhow::Result<()> {
        let old_claim = get_claim(claims, miner)?
            .ok_or_else(|| actor_error!(ErrNotFound, "no claim for actor {}", miner))?;

        self.total_qa_bytes_committed += qa_power;
        self.total_bytes_committed += power;

        let new_claim = Claim {
            raw_byte_power: old_claim.raw_byte_power.clone() + power,
            quality_adj_power: old_claim.quality_adj_power.clone() + qa_power,
            window_post_proof_type: old_claim.window_post_proof_type,
        };

        let min_power: StoragePower = consensus_miner_min_power(old_claim.window_post_proof_type)?;
        let prev_below: bool = old_claim.raw_byte_power < min_power;
        let still_below: bool = new_claim.raw_byte_power < min_power;

        if prev_below && !still_below {
            // Just passed min miner size
            self.miner_above_min_power_count += 1;
            self.total_quality_adj_power += &new_claim.quality_adj_power;
            self.total_raw_byte_power += &new_claim.raw_byte_power;
        } else if !prev_below && still_below {
            // just went below min miner size
            self.miner_above_min_power_count -= 1;
            self.total_quality_adj_power = self
                .total_quality_adj_power
                .checked_sub(&old_claim.quality_adj_power)
                .expect("Negative nominal power");
            self.total_raw_byte_power = self
                .total_raw_byte_power
                .checked_sub(&old_claim.raw_byte_power)
                .expect("Negative raw byte power");
        } else if !prev_below && !still_below {
            // Was above the threshold, still above
            self.total_quality_adj_power += qa_power;
            self.total_raw_byte_power += power;
        }

        if new_claim.raw_byte_power.is_negative() {
            return Err(anyhow!(actor_error!(
                ErrIllegalState,
                "negative claimed raw byte power: {}",
                new_claim.raw_byte_power
            )));
        }
        if new_claim.quality_adj_power.is_negative() {
            return Err(anyhow!(actor_error!(
                ErrIllegalState,
                "negative claimed quality adjusted power: {}",
                new_claim.quality_adj_power
            )));
        }
        if self.miner_above_min_power_count < 0 {
            return Err(anyhow!(actor_error!(
                ErrIllegalState,
                "negative amount of miners lather than min: {}",
                self.miner_above_min_power_count
            )));
        }

        set_claim(claims, miner, new_claim)
    }

    pub(super) fn add_pledge_total(&mut self, amount: TokenAmount) {
        self.total_pledge_collateral += amount;
    }

    pub(super) fn append_cron_event<BS: Blockstore>(
        &mut self,
        events: &mut Multimap<BS>,
        epoch: ChainEpoch,
        event: CronEvent,
    ) -> anyhow::Result<()> {
        if epoch < self.first_cron_epoch {
            self.first_cron_epoch = epoch;
        }

        events.add(epoch_key(epoch), event).map_err(|e| {
            e.downcast_wrap(format!("failed to store cron event at epoch {}", epoch))
        })?;
        Ok(())
    }

    pub fn current_total_power(&self) -> (StoragePower, StoragePower) {
        if self.miner_above_min_power_count < CONSENSUS_MINER_MIN_MINERS {
            (self.total_bytes_committed.clone(), self.total_qa_bytes_committed.clone())
        } else {
            (self.total_raw_byte_power.clone(), self.total_quality_adj_power.clone())
        }
    }

    pub(super) fn update_smoothed_estimate(&mut self, delta: ChainEpoch) {
        let filter_qa_power = AlphaBetaFilter::load(
            &self.this_epoch_qa_power_smoothed,
            &*DEFAULT_ALPHA,
            &*DEFAULT_BETA,
        );
        self.this_epoch_qa_power_smoothed =
            filter_qa_power.next_estimate(&self.this_epoch_quality_adj_power, delta);
    }

    /// Update stats on new miner creation. This is currently just used to update the miner count
    /// when new added miner starts above the minimum.
    pub(super) fn update_stats_for_new_miner(
        &mut self,
        window_post_proof: RegisteredPoStProof,
    ) -> anyhow::Result<()> {
        let min_power = consensus_miner_min_power(window_post_proof)?;

        if !min_power.is_positive() {
            self.miner_above_min_power_count += 1;
        }
        Ok(())
    }

    /// Validates that miner has
    pub(super) fn validate_miner_has_claim<BS>(
        &self,
        store: &BS,
        miner_addr: &Address,
    ) -> Result<(), ActorError>
    where
        BS: Blockstore,
    {
        let claims = make_map_with_root::<_, Claim>(&self.claims, store)
            .map_err(|e| e.downcast_default(ExitCode::ErrIllegalState, "failed to load claims"))?;

        if !claims
            .contains_key(&miner_addr.to_bytes())
            .map_err(|e| e.downcast_default(ExitCode::ErrIllegalState, "failed to look up claim"))?
        {
            return Err(actor_error!(
                ErrForbidden,
                "unknown miner {} forbidden to interact with power actor",
                miner_addr
            ));
        }
        Ok(())
    }

    pub fn get_claim<BS: Blockstore>(
        &self,
        store: &BS,
        miner: &Address,
    ) -> anyhow::Result<Option<Claim>> {
        let claims =
            make_map_with_root_and_bitwidth::<_, Claim>(&self.claims, store, HAMT_BIT_WIDTH)
                .map_err(|e| {
                    e.downcast_default(ExitCode::ErrIllegalState, "failed to load claims")
                })?;

        let claim = get_claim(&claims, miner)?;
        Ok(claim.cloned())
    }

    pub(super) fn delete_claim<BS: Blockstore>(
        &mut self,
        claims: &mut Map<BS, Claim>,
        miner: &Address,
    ) -> anyhow::Result<()> {
        let (rbp, qap) =
            match get_claim(claims, miner).map_err(|e| e.downcast_wrap("failed to get claim"))? {
                None => {
                    return Ok(());
                }
                Some(claim) => (claim.raw_byte_power.clone(), claim.quality_adj_power.clone()),
            };

        // Subtract from stats to remove power
        self.add_to_claim(claims, miner, &rbp.neg(), &qap.neg())
            .map_err(|e| e.downcast_wrap("failed to subtract miner power before deleting claim"))?;

        claims
            .delete(&miner.to_bytes())
            .map_err(|e| e.downcast_wrap(format!("failed to delete claim for address {}", miner)))?
            .ok_or_else(|| anyhow!("failed to delete claim for address: doesn't exist"))?;
        Ok(())
    }
}

pub(super) fn load_cron_events<BS: Blockstore>(
    mmap: &Multimap<BS>,
    epoch: ChainEpoch,
) -> anyhow::Result<Vec<CronEvent>> {
    let mut events = Vec::new();

    mmap.for_each(&epoch_key(epoch), |_, v: &CronEvent| {
        events.push(v.clone());
        Ok(())
    })?;

    Ok(events)
}

/// Gets claim from claims map by address
fn get_claim<'m, BS: Blockstore>(
    claims: &'m Map<BS, Claim>,
    a: &Address,
) -> anyhow::Result<Option<&'m Claim>> {
    claims
        .get(&a.to_bytes())
        .map_err(|e| e.downcast_wrap(format!("failed to get claim for address {}", a)))
}

pub fn set_claim<BS: Blockstore>(
    claims: &mut Map<BS, Claim>,
    a: &Address,
    claim: Claim,
) -> anyhow::Result<()> {
    if claim.raw_byte_power.is_negative() {
        return Err(anyhow!(actor_error!(
            ErrIllegalState,
            "negative claim raw power {}",
            claim.raw_byte_power
        )));
    }
    if claim.quality_adj_power.is_negative() {
        return Err(anyhow!(actor_error!(
            ErrIllegalState,
            "negative claim quality-adjusted power {}",
            claim.quality_adj_power
        )));
    }

    claims
        .set(a.to_bytes().into(), claim)
        .map_err(|e| e.downcast_wrap(format!("failed to set claim for address {}", a)))?;
    Ok(())
}

pub(super) fn epoch_key(e: ChainEpoch) -> BytesKey {
    let bz = e.encode_var_vec();
    bz.into()
}

impl Cbor for State {}

#[derive(Debug, Serialize_tuple, Deserialize_tuple, Clone, PartialEq)]
pub struct Claim {
    /// Miner's proof type used to determine minimum miner size
    pub window_post_proof_type: RegisteredPoStProof,
    /// Sum of raw byte power for a miner's sectors.
    #[serde(with = "bigint_ser")]
    pub raw_byte_power: StoragePower,
    /// Sum of quality adjusted power for a miner's sectors.
    #[serde(with = "bigint_ser")]
    pub quality_adj_power: StoragePower,
}

#[derive(Clone, Debug, Serialize_tuple, Deserialize_tuple)]
pub struct CronEvent {
    pub miner_addr: Address,
    pub callback_payload: RawBytes,
}

impl Cbor for CronEvent {}

/// Returns the minimum storage power required for each seal proof types.
pub fn consensus_miner_min_power(p: RegisteredPoStProof) -> anyhow::Result<StoragePower> {
    use RegisteredPoStProof::*;
    match p {
        StackedDRGWinning2KiBV1
        | StackedDRGWinning8MiBV1
        | StackedDRGWinning512MiBV1
        | StackedDRGWinning32GiBV1
        | StackedDRGWinning64GiBV1
        | StackedDRGWindow2KiBV1
        | StackedDRGWindow8MiBV1
        | StackedDRGWindow512MiBV1
        | StackedDRGWindow32GiBV1
        | StackedDRGWindow64GiBV1 => {
            let power: u64 = if cfg!(feature = "min-power-2k") {
                2 << 10
            } else if cfg!(feature = "min-power-2g") {
                2 << 30
            } else {
                10 << 40
            };
            Ok(StoragePower::from(power))
        }
        Invalid(i) => Err(anyhow::anyhow!("unsupported proof type: {}", i)),
    }
}

#[cfg(test)]
mod test {
    use fvm_shared::clock::ChainEpoch;

    use super::*;

    #[test]
    fn epoch_key_test() {
        let e1: ChainEpoch = 101;
        let e2: ChainEpoch = 102;
        let e3: ChainEpoch = 103;
        let e4: ChainEpoch = -1;

        let b1: BytesKey = [0xca, 0x1].to_vec().into();
        let b2: BytesKey = [0xcc, 0x1].to_vec().into();
        let b3: BytesKey = [0xce, 0x1].to_vec().into();
        let b4: BytesKey = [0x1].to_vec().into();

        assert_eq!(b1, epoch_key(e1));
        assert_eq!(b2, epoch_key(e2));
        assert_eq!(b3, epoch_key(e3));
        assert_eq!(b4, epoch_key(e4));
    }
}
