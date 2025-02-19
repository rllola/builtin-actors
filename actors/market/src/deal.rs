// Copyright 2019-2022 ChainSafe Systems
// SPDX-License-Identifier: Apache-2.0, MIT

use cid::{Cid, Version};
use fil_actors_runtime::DealWeight;
use fvm_shared::address::Address;
use fvm_shared::bigint::bigint_ser;
use fvm_shared::clock::ChainEpoch;
use fvm_shared::commcid::{FIL_COMMITMENT_UNSEALED, SHA2_256_TRUNC254_PADDED};
use fvm_shared::crypto::signature::Signature;
use fvm_shared::econ::TokenAmount;
use fvm_shared::encoding::tuple::*;
use fvm_shared::encoding::Cbor;
use fvm_shared::piece::PaddedPieceSize;

/// Cid prefix for piece Cids
pub fn is_piece_cid(c: &Cid) -> bool {
    // TODO: Move FIL_COMMITMENT etc, into a better place
    c.version() == Version::V1
        && c.codec() == FIL_COMMITMENT_UNSEALED
        && c.hash().code() == SHA2_256_TRUNC254_PADDED
        && c.hash().size() == 32
}

/// Note: Deal Collateral is only released and returned to clients and miners
/// when the storage deal stops counting towards power. In the current iteration,
/// it will be released when the sector containing the storage deals expires,
/// even though some storage deals can expire earlier than the sector does.
/// Collaterals are denominated in PerEpoch to incur a cost for self dealing or
/// minimal deals that last for a long time.
/// Note: ClientCollateralPerEpoch may not be needed and removed pending future confirmation.
/// There will be a Minimum value for both client and provider deal collateral.
#[derive(Clone, Debug, PartialEq, Serialize_tuple, Deserialize_tuple)]
pub struct DealProposal {
    pub piece_cid: Cid,
    pub piece_size: PaddedPieceSize,
    pub verified_deal: bool,
    pub client: Address,
    pub provider: Address,

    /// Arbitrary client chosen label to apply to the deal
    // ! This is the field that requires unsafe unchecked utf8 deserialization
    pub label: String,

    // Nominal start epoch. Deal payment is linear between StartEpoch and EndEpoch,
    // with total amount StoragePricePerEpoch * (EndEpoch - StartEpoch).
    // Storage deal must appear in a sealed (proven) sector no later than StartEpoch,
    // otherwise it is invalid.
    pub start_epoch: ChainEpoch,
    pub end_epoch: ChainEpoch,
    #[serde(with = "bigint_ser")]
    pub storage_price_per_epoch: TokenAmount,

    #[serde(with = "bigint_ser")]
    pub provider_collateral: TokenAmount,
    #[serde(with = "bigint_ser")]
    pub client_collateral: TokenAmount,
}

impl Cbor for DealProposal {}

impl DealProposal {
    pub fn duration(&self) -> ChainEpoch {
        self.end_epoch - self.start_epoch
    }
    /// Computes weight for a deal proposal, which is a function of its size and duration.
    pub fn weight(&self) -> DealWeight {
        DealWeight::from(self.duration()) * self.piece_size.0
    }
    pub fn total_storage_fee(&self) -> TokenAmount {
        self.storage_price_per_epoch.clone() * self.duration() as u64
    }
    pub fn client_balance_requirement(&self) -> TokenAmount {
        &self.client_collateral + self.total_storage_fee()
    }
    pub fn provider_balance_requirement(&self) -> &TokenAmount {
        &self.provider_collateral
    }
}

/// ClientDealProposal is a DealProposal signed by a client
#[derive(Clone, Debug, PartialEq, Serialize_tuple, Deserialize_tuple)]
pub struct ClientDealProposal {
    pub proposal: DealProposal,
    pub client_signature: Signature,
}

impl Cbor for ClientDealProposal {}

#[derive(Clone, Debug, PartialEq, Copy, Serialize_tuple, Deserialize_tuple)]
pub struct DealState {
    pub sector_start_epoch: ChainEpoch, // -1 if not yet included in proven sector
    pub last_updated_epoch: ChainEpoch, // -1 if deal state never updated
    pub slash_epoch: ChainEpoch,        // -1 if deal never slashed
}
