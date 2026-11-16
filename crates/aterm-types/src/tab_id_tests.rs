// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_tab_id_is_t_and_ten_crockford_characters() {
    for good in [
        "tqn5r6e0148",
        "t0000000000",
        "tzzzzzzzzzz",
        "tabcdefghjk",
        "tmnpqrstvwx",
    ] {
        let id = TabId::parse(good).expect(good);
        assert_eq!(id.as_str(), good);
        assert!(is_tab_id(good));
        assert_eq!(id.to_string(), good);
        assert_eq!(good.parse::<TabId>(), Ok(id));
    }
    // Negative controls: each breaks exactly one rule of the grammar.
    for bad in [
        "",
        "t",
        "tqn5r6e014",             // 10 bytes
        "tqn5r6e01488",           // 12 bytes
        "sqn5r6e0148",            // not `t`
        "Tqn5r6e0148",            // upper-case `T`
        "tQN5R6E0148",            // upper-case characters
        "tqn5r6e014i",            // Crockford's alias for 1
        "tqn5r6e014l",            // Crockford's alias for 1
        "tqn5r6e014o",            // Crockford's alias for 0
        "tqn5r6e014u",            // excluded by Crockford
        "tqn5r6e014-",            // punctuation
        "tqn5r6e014 ",            // whitespace
        "s-0123456789abcdef0123", // a legacy tab's ID is not a tab ID
        "s-0f1e",
        "S-0123456789abcdef0123",
        "s_0123456789abcdef0123",
        "t-qn5r6e014",
        "tqn5r6e01\u{e9}", // a multi-byte character at the end
    ] {
        assert_eq!(TabId::parse(bad), None, "{bad:?}");
        assert!(!is_tab_id(bad), "{bad:?}");
        assert_eq!(bad.parse::<TabId>(), Err(NotATabId), "{bad:?}");
    }
}

#[test]
fn a_legacy_s_id_is_read_only_by_is_legacy() {
    let legacy = "s-0123456789abcdef0123";
    assert!(is_legacy(legacy));
    assert_eq!(TabId::parse(legacy), None, "no tab ID is in the s- form");
    assert!(!is_tab_id(legacy));
    assert_eq!(short(legacy), legacy, "it is shown as it is");
    // Negative controls: each breaks exactly one rule of the legacy form.
    for bad in [
        "s-0123456789abcdef012",   // 19
        "s-0123456789abcdef01234", // 21
        "s-0123456789ABCDEF0123",  // upper-case hex
        "s-0123456789abcdeg0123",  // not hex
        "S-0123456789abcdef0123",
        "s_0123456789abcdef0123",
        "tqn5r6e0148", // a tab ID is not legacy
    ] {
        assert!(!is_legacy(bad), "{bad:?}");
    }
}

#[test]
fn bits_round_trip_and_order_matches_the_text() {
    let mut x = 0x9e37_79b9_7f4a_7c15_u64;
    let mut prev: Option<TabId> = None;
    for _ in 0..2000 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let id = TabId::from_bits(x);
        assert_eq!(id.bits(), x & MASK);
        assert_eq!(TabId::parse(id.as_str()), Some(id));
        if let Some(p) = prev {
            assert_eq!(p.cmp(&id), p.bits().cmp(&id.bits()));
            assert_eq!(p.cmp(&id), p.as_str().cmp(id.as_str()));
        }
        prev = Some(id);
    }
    assert_eq!(TabId::from_bits(0).as_str(), "t0000000000");
    assert_eq!(TabId::from_bits(MASK).as_str(), "tzzzzzzzzzz");
    // Bits above the 50th are dropped, never folded in.
    assert_eq!(TabId::from_bits(1 << 50), TabId::from_bits(0));
    assert_eq!(TabId::from_bits(31).as_str(), "t000000000z");
}

#[test]
fn a_tab_id_is_shown_as_its_first_five() {
    let id = TabId::parse("tqn5r6e0148").expect("an id");
    assert_eq!(id.short(), "tqn5r");
    assert_eq!(short("tqn5r6e0148"), "tqn5r");
    // Anything that is not a tab ID is shown as it is.
    for word in [
        "ctl",
        "human",
        "fleet:abcd",
        "tqn5r6e014",
        "s-0123456789abcdef0123",
    ] {
        assert_eq!(short(word), word);
    }
}

#[test]
fn mint_draws_again_while_the_id_is_taken() {
    let draws = [1u64, 2, 3, 4];
    let mut next = draws.iter().copied();
    let taken = [TabId::from_bits(1), TabId::from_bits(2)];
    let id = TabId::mint(|| next.next().unwrap_or(0), |id| taken.contains(id));
    assert_eq!(id, TabId::from_bits(3), "the first draw nobody knows");

    // Negative control: with nothing taken, the first draw is the ID.
    let mut next = draws.iter().copied();
    assert_eq!(
        TabId::mint(|| next.next().unwrap_or(0), |_| false),
        TabId::from_bits(1)
    );

    // A `taken` that refuses every ID ends after the bound instead of spinning.
    let mut n = 0u64;
    let mut asked = 0u32;
    let _ = TabId::mint(
        || {
            n += 1;
            n
        },
        |_| {
            asked += 1;
            true
        },
    );
    assert_eq!(n, u64::from(MINT_DRAWS));
    assert_eq!(asked, MINT_DRAWS - 1);
}

#[test]
fn a_map_keyed_by_tab_id_is_found_by_its_text() {
    use std::collections::HashMap;
    let id = TabId::from_bits(12345);
    let mut map = HashMap::new();
    map.insert(id, 7);
    assert_eq!(map.get(id.as_str()), Some(&7));
    assert_eq!(map.get("t0000000000"), None);
}

#[test]
fn prefix_names_the_one_tab_it_begins_and_refuses_one_two_begin() {
    let ids = ["tqn5r6e0148", "tqn5rzz0000", "tab12345678"];
    assert_eq!(by_prefix("tab12", ids), Ok("tab12345678"));
    assert_eq!(by_prefix("tqn5r6", ids), Ok("tqn5r6e0148"));
    assert_eq!(by_prefix("tqn5r6e0148", ids), Ok("tqn5r6e0148"));
    // Ambiguous: refused, naming both.
    assert_eq!(
        by_prefix("tqn5r", ids),
        Err(vec!["tqn5r6e0148", "tqn5rzz0000"])
    );
    // Nothing begins it, or it is no prefix at all: shorter than the shown form, not lower-case
    // Crockford, longer than an ID, or not a `t` ID.
    assert_eq!(by_prefix("txxxx", ids), Err(Vec::<&str>::new()));
    for word in [
        "t",
        "",
        "ta",
        "tab1",
        "qn5r6",
        "tQN5R",
        "tqn5ri",
        "tqn5r6e01480",
        "s-0123",
    ] {
        assert!(!is_prefix(word), "{word}");
        assert_eq!(by_prefix(word, ids), Err(Vec::<&str>::new()), "{word}");
    }
    // Only a tab ID is begun: another word in the set never matches.
    assert_eq!(
        by_prefix("tqn5r", ["tqn5r", "tqn5r6e0148"]),
        Ok("tqn5r6e0148")
    );
}

#[test]
fn prefix_display_is_five_and_longer_only_where_two_share_them() {
    let a = TabId::parse("tqn5r6e0148").expect("an id");
    let b = TabId::parse("tqn5rzz0000").expect("an id");
    let c = TabId::parse("tqn5r6f0000").expect("an id");
    let d = TabId::parse("tab12345678").expect("an id");
    assert_eq!(a.shown([&d]), "tqn5r");
    assert_eq!(a.shown([&a]), "tqn5r");
    assert_eq!(a.shown([&a, &b, &d]), "tqn5r6");
    assert_eq!(b.shown([&a, &b, &d]), "tqn5rz");
    assert_eq!(a.shown([&a, &b, &c, &d]), "tqn5r6e");
    assert_eq!(c.shown([&a, &b, &c, &d]), "tqn5r6f");
    // Each shown form is a prefix that names its own tab and no other.
    let all = [a, b, c, d];
    for id in &all {
        assert_eq!(
            by_prefix(id.shown(&all), all.iter().map(TabId::as_str)),
            Ok(id.as_str())
        );
    }
}
