// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::sys::json::contract_violations;

fn sizes(alloc: u64) -> RootSizes {
    RootSizes {
        alloc,
        logical: alloc / 2,
        entries: 63_039,
        direct_entries_max: 900,
        growth_bps: Some(7_400_000),
        age_s: Some(3),
    }
}

#[test]
fn a_root_without_a_walker_says_not_sampled_never_zero() {
    let r = RootSize::NotSampled;
    assert_eq!(r.state_word(), None);
    assert_eq!(r.alloc_text(), "unavailable(not_sampled)");
    assert_eq!(r.entries_text(), "unavailable(not_sampled)");
    assert_eq!(r.growth_text(), "unavailable(not_sampled)");
    let mut o = Jv::obj::<&str>([]);
    r.json_fields(&mut o);
    assert!(contract_violations(&o).is_empty(), "{}", o.to_text());
    assert_eq!(o.get("alloc_bytes"), Some(&Jv::why("not_sampled")));
}

#[test]
fn a_walking_root_is_a_lower_bound_with_its_progress() {
    let r = RootSize::Walking {
        walked_entries: 12_000,
        partial_alloc: Some(2_100_000_000),
        last: Some(sizes(7_050_000_000)),
    };
    assert_eq!(r.state_word(), Some("walking"));
    assert_eq!(r.alloc_text(), "\u{2265}2100000000");
    assert_eq!(r.entries_text(), "unavailable(walking)");
    assert_eq!(r.growth_text(), "7400000");
    let mut o = Jv::obj::<&str>([]);
    r.json_fields(&mut o);
    assert!(contract_violations(&o).is_empty(), "{}", o.to_text());
    assert_eq!(
        o.get("alloc_bytes"),
        Some(&Jv::at_least(2_100_000_000, "walking"))
    );
    assert_eq!(o.get("walked_entries"), Some(&Jv::u(12_000)));
    assert_eq!(
        o.get("last_complete_alloc_bytes"),
        Some(&Jv::u(7_050_000_000))
    );
    let q = RootSize::Walking {
        walked_entries: 0,
        partial_alloc: None,
        last: None,
    };
    assert_eq!(q.alloc_text(), "unavailable(walking)");
}

#[test]
fn a_complete_root_prints_bare_figures_and_a_refused_one_its_word() {
    let r = RootSize::Complete(sizes(7_050_000_000));
    assert_eq!(r.alloc_text(), "7050000000");
    assert_eq!(r.entries_text(), "63039");
    let mut o = Jv::obj::<&str>([]);
    r.json_fields(&mut o);
    assert!(contract_violations(&o).is_empty(), "{}", o.to_text());
    assert_eq!(o.get("complete"), Some(&Jv::Bool(true)));
    let p = RootSize::NotMeasured("protected");
    assert_eq!(p.alloc_text(), "unavailable(protected)");
    let mut o = Jv::obj::<&str>([]);
    p.json_fields(&mut o);
    assert!(contract_violations(&o).is_empty(), "{}", o.to_text());
}

#[test]
fn the_totals_clause_and_json_keep_the_four_shapes() {
    let bps = |b: f64| format!("{:.1} MB/s", b / 1e6);
    assert_eq!(totals_clause(None, bps), "sizes unavailable(not_sampled)");
    let t = DiskTotals {
        total: 15_300_000_000,
        exclusive: 15_300_000_000,
        shared: 0,
        walking: false,
        not_measured: Vec::new(),
        growth_bps: Some(7_400_000),
        warnings: vec![HugeDirFact {
            path: "/x/target/debug/deps".into(),
            entries: 120_000,
            root: Some(1),
        }],
        ..DiskTotals::default()
    };
    assert_eq!(
        totals_clause(Some(&t), bps),
        "15.3 GB exclusive · growing 7.4 MB/s"
    );
    let w = DiskTotals {
        walking: true,
        shared: 2_000_000_000,
        growth_bps: None,
        ..t.clone()
    };
    assert_eq!(
        totals_clause(Some(&w), bps),
        "\u{2265}15.3 GB exclusive, \u{2265}2.0 GB shared (walking)"
    );
    for totals in [None, Some(&t), Some(&w)] {
        let mut o = Jv::obj::<&str>([]);
        totals_json(totals, &mut o);
        assert!(contract_violations(&o).is_empty(), "{}", o.to_text());
    }
}

/// `help sys`: the size of a folder no walk measures is `unavailable(<why>)`, never zero,
/// and a balance that leaves one out is a lower bound that says why.
#[test]
fn a_root_no_walk_measured_makes_the_balance_a_bound_never_a_bare_zero() {
    let bps = |b: f64| format!("{:.1} MB/s", b / 1e6);
    let json = |t: &DiskTotals| {
        let mut o = Jv::obj::<&str>([]);
        totals_json(Some(t), &mut o);
        assert!(contract_violations(&o).is_empty(), "{}", o.to_text());
        o
    };
    let only = DiskTotals {
        not_measured: vec!["too_broad"],
        ..DiskTotals::default()
    };
    assert_eq!(
        totals_clause(Some(&only), bps),
        "sizes unavailable(too_broad)"
    );
    let o = json(&only);
    for key in [
        "exclusive_bytes",
        "shared_bytes",
        "total_bytes",
        "growth_bytes_per_s",
    ] {
        assert_eq!(o.get(key), Some(&Jv::why("too_broad")), "{key}");
    }
    assert_eq!(
        o.get("not_measured"),
        Some(&Jv::Arr(vec![Jv::s("too_broad")]))
    );

    let part = DiskTotals {
        total: 9_150_000_000,
        exclusive: 9_150_000_000,
        growth_bps: Some(7_400_000),
        ..only.clone()
    };
    assert_eq!(
        totals_clause(Some(&part), bps),
        "\u{2265}9.2 GB exclusive (1 not measured: too_broad) · growing 7.4 MB/s"
    );
    assert_eq!(
        json(&part).get("exclusive_bytes"),
        Some(&Jv::at_least(9_150_000_000, "too_broad"))
    );

    let both = DiskTotals {
        walking: true,
        not_measured: vec!["protected", "too_broad", "protected"],
        ..part.clone()
    };
    assert_eq!(
        totals_clause(Some(&both), bps),
        "\u{2265}9.2 GB exclusive (walking; 3 not measured: protected,too_broad) · growing 7.4 MB/s"
    );
    assert_eq!(
        json(&both).get("total_bytes"),
        Some(&Jv::at_least(9_150_000_000, "walking"))
    );
    let owed = DiskTotals {
        walking: true,
        ..only
    };
    assert_eq!(
        totals_clause(Some(&owed), bps),
        "\u{2265}0 B exclusive (walking; 1 not measured: too_broad)",
        "a walk still owed may add bytes: a bound, not unavailable"
    );

    let walking = DiskTotals {
        walking: true,
        not_measured: Vec::new(),
        ..part
    };
    assert_eq!(
        totals_clause(Some(&walking), bps),
        "\u{2265}9.2 GB exclusive (walking) · growing 7.4 MB/s",
        "nothing unmeasured: the walking form is unchanged"
    );
    assert_eq!(
        json(&walking).get("not_measured"),
        Some(&Jv::Arr(Vec::new()))
    );
}

#[test]
fn a_subtree_knows_its_depth() {
    let d = |p: &str| DirSize {
        rel: p.into(),
        ..DirSize::default()
    };
    assert_eq!(d("target").depth(), 1);
    assert_eq!(d("scratchpad/target-probe/debug").depth(), 3);
    assert_eq!(bytes_word(820_000_000), "820 MB");
    assert_eq!(bytes_word(1_850_000_000_000), "1.85 TB");
}
