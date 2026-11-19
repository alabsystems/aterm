// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A record round-trips in the window's own bytes, and anything else reads as none.
#[test]
fn a_record_round_trips_and_refuses_what_aterm_did_not_write() {
    let on = Observed {
        enabled: true,
        since: 1_790_000_000,
        origin: Origin::Aterm,
    };
    assert_eq!(on.encode(), "enabled 1790000000 aterm");
    assert_eq!(Observed::parse(&on.encode()), Some(on));
    let off = Observed {
        enabled: false,
        since: 7,
        origin: Origin::Unknown,
    };
    assert_eq!(off.encode(), "disabled 7 unknown");
    assert_eq!(Observed::parse("disabled 7 unknown\n"), Some(off));
    assert!(on.encode().len() <= MAX_BYTES);
    for bad in [
        "",
        "unknown 1 aterm",
        "unrecognized 1 aterm",
        "enabled x aterm",
        "enabled -1 aterm",
        "enabled 1 someone",
        "enabled 1 aterm extra",
        "enabled 1 aterm 6A1E64F2-1C3B-4D5E-8F90-0A1B2C3D4E5F",
    ] {
        assert_eq!(Observed::parse(bad), None, "{bad:?}");
    }
}

/// Developer mode reading off is the person's where the reader saw it go off, or the record says
/// it was on or names who set it. NEGATIVE CONTROLS: no record, and a mode first seen off and set
/// by no one aterm knows, are no choice.
#[test]
fn off_is_chosen_where_aterm_saw_it_on_or_knows_who_set_it() {
    let record = |enabled, origin| Observed {
        enabled,
        since: 1,
        origin,
    };
    assert!(off_is_chosen(true, None));
    assert!(off_is_chosen(false, Some(&record(true, Origin::Unknown))));
    assert!(off_is_chosen(false, Some(&record(false, Origin::Aterm))));
    assert!(off_is_chosen(false, Some(&record(false, Origin::Outside))));
    assert!(!off_is_chosen(false, None));
    assert!(!off_is_chosen(false, Some(&record(false, Origin::Unknown))));
}

/// Each Mac's record has a name of its own, made of its hardware UUID alone, beside the
/// account's `aterm.toml`; the name from before records named their Mac is none of them, and
/// where the UUID is unread or is not one UUID there is no record of this Mac. NEGATIVE CONTROL
/// (run): a name that leaves out the UUID makes every Mac's record one file.
#[test]
fn each_mac_has_a_record_of_its_own() {
    const A: &str = "6A1E64F2-1C3B-4D5E-8F90-0A1B2C3D4E5F";
    const B: &str = "0F9E8D7C-6B5A-4F3E-2D1C-0B9A8F7E6D5C";
    let config = Path::new("/Users//alice/.config/aterm/aterm.toml");
    let a = beside_on(config, Some(A)).unwrap();
    assert_eq!(
        a,
        Path::new("/Users//alice/.config/aterm").join(format!("privacy-debugger-observed-{A}"))
    );
    assert_eq!(
        beside_on(config, Some(&A.to_ascii_lowercase())),
        Some(a.clone())
    );
    let b = beside_on(config, Some(B)).unwrap();
    assert_ne!(a, b);
    for (mac, record) in [("A", &a), ("B", &b)] {
        assert_ne!(
            record.file_name().and_then(|name| name.to_str()),
            Some(OBSERVED),
            "{mac}"
        );
    }
    assert_eq!(beside_on(config, None), None);
    for bad in [
        "",
        "not a uuid",
        "6A1E64F2-1C3B-4D5E-8F90",
        "../../etc/passwd",
        "6A1E64F2-1C3B-4D5E-8F90-0A1B2C3D4E5G",
    ] {
        assert_eq!(observed_on(bad), None, "{bad:?}");
        assert_eq!(beside_on(config, Some(bad)), None, "{bad:?}");
    }
    let on = Observed {
        enabled: true,
        since: 1_790_000_000,
        origin: Origin::Outside,
    };
    assert!(on.encode().len() <= MAX_BYTES);
}
