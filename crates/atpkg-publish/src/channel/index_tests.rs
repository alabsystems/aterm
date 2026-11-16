// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// `id` leading `leads`.
fn pin(id: &str, leads: &[&str]) -> (String, Vec<String>) {
    (
        id.to_string(),
        leads.iter().map(|w| (*w).to_string()).collect(),
    )
}

/// A MOVED PIN WITH NO RECORD IS HELD TO ITS SEED'S WORDS: kimi-alab pinned at a build that
/// carries no record leads `kimi`, its seed's word, so an index that moves it reads the pins it
/// leaves and refuses one whose record leads `kimi` too. A moved pin leading nothing reads no
/// other pin. NEGATIVE CONTROL (2026-11-14): while only a moved pin's record counted, the
/// recordless kimi-alab pin read no other pin and the index published two leaders of `kimi`.
#[test]
fn a_moved_pin_with_no_record_is_held_to_its_seeds_words() {
    let seed = atpkg::harness_record::pinned_leads("kimi-alab", None).expect("a seed's words");
    assert_eq!(seed, ["kimi"]);
    let refused = one_leader_among(vec![("kimi-alab".to_string(), seed)], || {
        Ok(vec![pin("agy-alab", &["kimi"]), pin("muse-alab", &[])])
    });
    assert!(
        matches!(&refused, Err(e) if e.to_string().contains("agy-alab and kimi-alab both lead `kimi`")),
        "{refused:?}"
    );
    let apart = one_leader_among(vec![pin("kimi-alab", &["kimi"])], || {
        Ok(vec![pin("grok-alab", &["grok"])])
    });
    assert!(matches!(apart, Ok(true)), "{apart:?}");
    let quiet = one_leader_among(vec![pin("agy-alab", &[])], || {
        panic!("a pin leading nothing reads no other pin")
    });
    assert!(matches!(quiet, Ok(false)), "{quiet:?}");
}
