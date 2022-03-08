use fil_actor_miner::allow_post_proof_type;

use fvm_shared::clock::ChainEpoch;
use fvm_shared::sector::{RegisteredSealProof,RegisteredPoStProof};
use fvm_shared::econ::TokenAmount;

mod util;

#[test]
fn basic_post_and_dispute() {
    allow_post_proof_type(RegisteredPoStProof::StackedDRGWindow2KiBV1);

    let period_offset = ChainEpoch::from(100);
    let precommit_epoch = ChainEpoch::from(1);

    let mut h = util::ActorHarness::new(period_offset);
    h.set_proof_type(RegisteredSealProof::StackedDRG2KiBV1P1);

    let mut rt = h.new_runtime();
    rt.epoch = precommit_epoch;
    rt.balance.replace(TokenAmount::from(1_000_000_000_000_000_000_000_000i128));

    h.construct_and_verify(&mut rt);

    // XXX rest of the test...
}

#[test]
fn invalid_submissions() {}

#[test]
fn duplicate_proof_rejected() {}

#[test]
fn duplicate_proof_rejected_with_many_partitions() {}

#[test]
fn successful_recoveries_recover_power() {}

#[test]
fn skippled_faults_adjust_power() {}

#[test]
fn skipping_all_sectors_in_a_partition_rejected() {}

#[test]
fn skipped_recoveries_are_penalized_and_do_not_recover_power() {}

#[test]
fn skipping_a_fault_from_the_wrong_partition_is_an_error() {}

#[test]
fn cannot_dispute_posts_when_the_challenge_window_is_open() {}

#[test]
fn can_dispute_up_till_window_end_but_not_after() {}

#[test]
fn cant_dispute_up_with_an_invalid_deadline() {}

#[test]
fn can_dispute_test_after_proving_period_changes() {}
