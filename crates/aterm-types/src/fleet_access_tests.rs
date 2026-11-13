// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn levels_are_ordered_and_their_words_round_trip() {
    assert!(FleetAccess::ReadOnly < FleetAccess::ReadWrite);
    assert!(FleetAccess::ReadWrite < FleetAccess::Full);
    assert_eq!(FleetAccess::default(), FleetAccess::ReadOnly);
    for level in FleetAccess::ALL {
        assert_eq!(FleetAccess::from_word(level.word()), Some(level));
        assert_eq!(level.to_string(), level.word());
    }
    for word in ["", "Read", "READ", "read ", "readonly", "rw", "all", "none"] {
        assert_eq!(FleetAccess::from_word(word), None, "{word:?}");
    }
}

#[test]
fn a_click_rotates_through_every_level_and_round() {
    let mut level = FleetAccess::ReadOnly;
    let mut seen = Vec::new();
    for _ in 0..3 {
        seen.push(level);
        level = level.rotated();
    }
    assert_eq!(seen, FleetAccess::ALL);
    assert_eq!(level, FleetAccess::ReadOnly);
}

/// The level each kind needs, kind by kind: the table of the design's §2.3.
#[test]
fn each_kind_needs_its_level() {
    use DriveKind as K;
    use FleetAccess as A;
    let table = [
        (K::Read, A::ReadOnly),
        (K::ShellRead, A::ReadOnly),
        (K::Vault, A::ReadOnly),
        (K::ServiceRepair, A::ReadOnly),
        (K::AccountsRenew, A::ReadOnly),
        (K::Post, A::ReadWrite),
        (K::MailSeen, A::ReadWrite),
        (K::Turn, A::ReadWrite),
        (K::Key, A::ReadWrite),
        (K::Send, A::ReadWrite),
        (K::Hold, A::ReadWrite),
        (K::Close, A::ReadWrite),
        (K::Claim, A::ReadWrite),
        (K::BusLog, A::ReadWrite),
        (K::Spawn, A::Full),
    ];
    assert_eq!(table.map(|(kind, _)| kind), DriveKind::ALL);
    for (kind, needs) in table {
        assert_eq!(kind.needs(), needs, "{kind:?}");
        for level in FleetAccess::ALL {
            assert_eq!(level.admits(kind), level >= needs, "{level:?} {kind:?}");
        }
    }
}

/// A refusal names the level in force and the level needed, and reads back; an admitted act
/// has none. CONTROL: a grant refusal and a cut head read as no lock refusal.
#[test]
fn a_refusal_names_both_levels_and_reads_back() {
    for level in FleetAccess::ALL {
        for kind in DriveKind::ALL {
            let refusal = level.refusal(kind);
            assert_eq!(refusal.is_none(), level.admits(kind), "{level:?} {kind:?}");
            if let Some(refusal) = refusal {
                assert!(
                    refusal.starts_with(crate::control_verbs::DRIVE_LOCKED),
                    "{refusal}"
                );
                assert_eq!(
                    FleetAccess::refused_at(&refusal),
                    Some((level, kind.needs())),
                    "{refusal}"
                );
                assert_eq!(
                    FleetAccess::refused_at(&format!("{refusal}; not `x`\n")),
                    Some((level, kind.needs()))
                );
            }
        }
    }
    for other in [
        "ERR denied grant=none: no grant",
        "ERR denied: a drive serves",
        "ERR denied access=",
        "ERR denied access=read",
        "ERR denied access=read needs=",
        "ERR denied access=open needs=full: x",
        "OK",
    ] {
        assert_eq!(FleetAccess::refused_at(other), None, "{other}");
    }
}

#[test]
fn a_header_without_the_token_is_an_older_aterm_and_an_unknown_word_is_locked() {
    assert_eq!(FleetAccess::of_header(None), FleetAccess::Full);
    assert_eq!(
        FleetAccess::of_header(Some("write")),
        FleetAccess::ReadWrite
    );
    assert_eq!(FleetAccess::of_header(Some("wide")), FleetAccess::ReadOnly);
}

/// A drive's claim writes this Mac's request journal and widens what a cleanup may delete: it
/// needs read and write. NEGATIVE: read-only refuses it, naming both levels.
#[test]
fn claim_needs_read_and_write() {
    assert_eq!(DriveKind::Claim.needs(), FleetAccess::ReadWrite);
    assert_eq!(DriveKind::Claim.word(), "request claim");
    assert!(FleetAccess::ReadWrite.admits(DriveKind::Claim));
    assert!(FleetAccess::Full.admits(DriveKind::Claim));
    let refusal = FleetAccess::ReadOnly
        .refusal(DriveKind::Claim)
        .expect("refused");
    assert_eq!(
        FleetAccess::refused_at(&refusal),
        Some((FleetAccess::ReadOnly, FleetAccess::ReadWrite))
    );
}
