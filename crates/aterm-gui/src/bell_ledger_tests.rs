// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn rang(at: u64, program: Option<&str>, sound: BellSound) -> Rang {
    Rang {
        at_unix_ms: at,
        program: program.map(str::to_owned),
        sound,
    }
}

/// A session that never rang says so in every field; one that rang
/// counts each bell, the sounded ones apart, and keeps the newest
/// [`KEPT`] first. NEGATIVE CONTROL: the quiet ledger's fields.
#[test]
fn a_ledger_counts_every_bell_and_keeps_the_newest_few() {
    let quiet = BellLedger::default();
    assert_eq!(
        quiet.wire(10_000),
        "bells=0 bells_sounded=0 last_bell=-",
        "a session that never rang"
    );

    let ledger = BellLedger::default();
    ledger.ring(rang(1_000, Some("zsh"), BellSound::Sounded));
    ledger.ring(rang(2_000, Some("zsh"), BellSound::Limited));
    ledger.ring(rang(3_000, Some("vim"), BellSound::Off));
    ledger.ring(rang(4_000, None, BellSound::Serious));
    ledger.ring(rang(5_000, Some("claude"), BellSound::Sounded));
    assert_eq!(
        ledger.wire(10_000),
        "bells=5 bells_sounded=2 \
         last_bell=5000/sounded/claude,6000/serious/-,7000/off/vim,8000/limited/zsh",
        "five rang, two sounded; the oldest of five is the one let go"
    );
    assert_eq!(ledger.read().recent.len(), KEPT);
}

/// The sweep's name while the group in front is the one it named; the
/// process in front by its own name when the sweep has not caught up;
/// the sweep's name when nothing in front can be read or named.
#[test]
fn the_program_in_front_is_the_sweeps_name_unless_another_group_holds_it() {
    let name_of = |pgid: i32| (pgid == 77).then(|| "vim".to_owned());
    let claude = || Some("claude".to_owned());
    assert_eq!(program_in_front(claude(), 42, 42, &name_of), claude());
    assert_eq!(
        program_in_front(claude(), 42, 77, &name_of).as_deref(),
        Some("vim"),
        "another group in front: its own name"
    );
    assert_eq!(program_in_front(claude(), 42, 99, &name_of), claude());
    assert_eq!(program_in_front(claude(), 42, -1, &name_of), claude());
    assert_eq!(program_in_front(None, 0, -1, &name_of), None);
}

/// A program name cannot split its entry, even for a reader that
/// decodes the whole value before it splits it: its `/` and `,` read as
/// `_`, and the rest decodes back. The value stays one token.
#[test]
fn a_program_name_never_splits_its_entry() {
    let ledger = BellLedger::default();
    ledger.ring(rang(0, Some("a/b,c d"), BellSound::Headless));
    let wire = ledger.wire(0);
    let last = wire.rsplit_once("last_bell=").expect("the field").1;
    assert!(!last.contains(' '), "one token: {wire}");
    let decoded = aterm_codec::pct::decode(last);
    let parts: Vec<&str> = decoded.split('/').collect();
    assert_eq!(parts, ["0", "headless", "a_b_c d"], "{wire}");
    assert!(!decoded.contains(','), "{wire}");
}
