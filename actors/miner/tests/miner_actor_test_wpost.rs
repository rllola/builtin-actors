use fil_actors_runtime::test_utils::*;

mod util;

#[test]
fn basic_post_and_dispute() {}

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
