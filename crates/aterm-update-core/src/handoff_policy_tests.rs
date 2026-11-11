// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// A wait for something that must happen is a hang detector, never a latency budget.
#[cfg(unix)]
const HANG_DETECTOR: std::time::Duration = std::time::Duration::from_secs(60);

fn policy(text: &str) -> HandoffPolicy {
    HandoffPolicy::parse(text).unwrap_or_else(|why| panic!("{text:?}: {why}"))
}

#[test]
fn every_v1_key_parses_and_every_key_is_optional() {
    assert_eq!(policy("schema = 1\n"), HandoffPolicy::default());
    assert_eq!(
        policy(
            "schema = 1\napplies_to_producers = [1790000000, 1790999999]\ncarry = \
             \"repaint\"\npark_quiet_gate_at_land = \"relaxed\"\n"
        ),
        HandoffPolicy {
            applies_to_producers: Some(ProducerRange {
                min_build: 1_790_000_000,
                max_build: 1_790_999_999,
            }),
            carry: Some(CarryCeiling::Repaint),
            park_quiet_gate_at_land: Some(LandQuietGate::Relaxed),
        }
    );
    for (spelling, ceiling) in [
        ("full", CarryCeiling::Full),
        ("visible", CarryCeiling::Visible),
        ("repaint", CarryCeiling::Repaint),
    ] {
        assert_eq!(
            policy(&format!("schema = 1\ncarry = \"{spelling}\"\n")).carry_ceiling(),
            ceiling
        );
    }
    assert!(
        !policy("schema = 1\npark_quiet_gate_at_land = \"strict\"\n").relaxes_land_gate(),
        "strict is the gate as it ships"
    );
}

#[test]
fn only_a_full_ceiling_carries_scrollback_and_greater_is_more_conservative() {
    assert!(CarryCeiling::Full.carries_scrollback());
    assert!(!CarryCeiling::Visible.carries_scrollback());
    assert!(!CarryCeiling::Repaint.carries_scrollback());
    assert!(CarryCeiling::Full < CarryCeiling::Visible);
    assert!(CarryCeiling::Visible < CarryCeiling::Repaint);
    assert_eq!(
        CarryCeiling::Full.max(CarryCeiling::Repaint),
        CarryCeiling::Repaint,
        "the max of two ceilings is the more conservative"
    );
}

#[test]
fn unknown_keys_are_ignored_by_a_producer_and_refused_by_the_cutter() {
    let text = "schema = 1\ncarry = \"visible\"\nsome_future_knob = 3\n[future]\nx = 1\n";
    assert_eq!(policy(text).carry, Some(CarryCeiling::Visible));
    let refused = PolicyFile::parse_for_cut(text).expect_err("a typo refuses the cut");
    assert!(refused.contains("unknown key"), "{refused}");
    let reserved = "schema = 1\nseamless = false\n";
    assert_eq!(policy(reserved), HandoffPolicy::default());
    let refused = PolicyFile::parse_for_cut(reserved).expect_err("reserved");
    assert!(refused.contains("`seamless` is reserved"), "{refused}");
    let future_rescue = format!("{HULL_AB}\n{RESCUE_A}future_knob = 1\n\n{RESCUE_B}");
    assert_eq!(
        file(&future_rescue)
            .for_build(150)
            .map(|p| p.carry_ceiling()),
        Some(CarryCeiling::Repaint),
        "a producer reads the rescue, its unknown key ignored"
    );
    let refused = PolicyFile::parse_for_cut(&future_rescue).expect_err("a typo in a table");
    assert!(
        refused.contains("[[ranges]] table 1: unknown key `future_knob`"),
        "{refused}"
    );
    assert_eq!(
        PolicyFile::parse_for_cut(&two_rescues())
            .expect("a ranged v1 policy passes the cutter")
            .legacy
            .carry,
        Some(CarryCeiling::Repaint)
    );
}

#[test]
fn the_cutter_refuses_a_knob_that_is_not_a_rescue_with_its_builds_and_reason() {
    for loose in [
        "schema = 1\ncarry = \"repaint\"\n",
        "schema = 1\npark_quiet_gate_at_land = \"relaxed\"\n",
        "schema = 1\napplies_to_producers = [100, 199]\ncarry = \"repaint\"\n",
    ] {
        let refused = PolicyFile::parse_for_cut(loose).expect_err(loose);
        assert!(refused.contains("a knob outside `[[ranges]]`"), "{refused}");
    }
    assert!(policy("schema = 1\ncarry = \"repaint\"\n").applies_to(7));
    for (table, why) in [
        (
            "carry = \"repaint\"\nreason = \"x\"\n",
            "no `producers = [min_build, max_build]`",
        ),
        (
            "producers = [100, 199]\ncarry = \"repaint\"\n",
            "no `reason`",
        ),
        (
            "producers = [100, 199]\ncarry = \"repaint\"\nreason = \"  \"\n",
            "no `reason`",
        ),
        (
            "producers = [100, 199]\ncarry = \"full\"\nreason = \"x\"\n",
            "asks nothing",
        ),
        (
            "producers = [199, 100]\ncarry = \"repaint\"\nreason = \"x\"\n",
            "`producers` is [199, 100], an empty range",
        ),
        (
            "producers = [100, 199]\ncarry = \"sideways\"\nreason = \"x\"\n",
            "`carry`",
        ),
    ] {
        let text = format!(
            "schema = 1\napplies_to_producers = [100, 199]\ncarry = \"repaint\"\n\n\
             [[ranges]]\n{table}"
        );
        let refused = PolicyFile::parse_for_cut(&text).expect_err(&text);
        assert!(
            refused.contains("[[ranges]] table 1") && refused.contains(why),
            "{text}: {refused}"
        );
    }
    let refused = PolicyFile::parse_for_cut("schema = 1\nranges = []\n").expect_err("empty");
    assert!(refused.contains("lists no rescue"), "{refused}");
    for asks_nothing in [
        "schema = 1\n",
        "schema = 1\ncarry = \"full\"\npark_quiet_gate_at_land = \"strict\"\n",
    ] {
        PolicyFile::parse_for_cut(asks_nothing).expect(asks_nothing);
    }
}

const RESCUE_A: &str =
    "[[ranges]]\nproducers = [100, 199]\ncarry = \"repaint\"\nreason = \"capture panics\"\n";
const RESCUE_B: &str = "[[ranges]]\nproducers = [250, 299]\npark_quiet_gate_at_land = \
                        \"relaxed\"\nreason = \"Land gate never opens\"\n";
const HULL_AB: &str = "schema = 1\napplies_to_producers = [100, 299]\ncarry = \
                       \"repaint\"\npark_quiet_gate_at_land = \"relaxed\"\n";

fn two_rescues() -> String {
    format!("{HULL_AB}\n{RESCUE_A}\n{RESCUE_B}")
}

fn file(text: &str) -> PolicyFile {
    PolicyFile::parse(text).unwrap_or_else(|why| panic!("{text:?}: {why}"))
}

#[test]
fn the_top_level_keys_beside_ranges_are_the_hull_of_the_rescues() {
    let Ranges::Listed(rescues) = file(&two_rescues()).ranges else {
        panic!("two rescues listed");
    };
    assert_eq!(
        PolicyFile::hull(&rescues),
        Some(HandoffPolicy {
            applies_to_producers: Some(ProducerRange {
                min_build: 100,
                max_build: 299,
            }),
            carry: Some(CarryCeiling::Repaint),
            park_quiet_gate_at_land: Some(LandQuietGate::Relaxed),
        })
    );
    assert_eq!(PolicyFile::hull(&[]), None);
    let visible = "[[ranges]]\nproducers = [300, 310]\ncarry = \"visible\"\nreason = \"x\"\n";
    let Ranges::Listed(rescues) = file(&format!("schema = 1\n{RESCUE_A}\n{visible}")).ranges else {
        panic!("listed");
    };
    let hull = PolicyFile::hull(&rescues).expect("a hull");
    assert_eq!(hull.carry_ceiling(), CarryCeiling::Repaint);
    assert!(!hull.relaxes_land_gate());

    PolicyFile::parse_for_cut(&two_rescues()).expect("the hull, written out");
    for not_the_hull in [
        two_rescues().replace("[100, 299]", "[100, 199]"),
        two_rescues().replace("park_quiet_gate_at_land = \"relaxed\"\n\n", "\n"),
        two_rescues().replace("carry = \"repaint\"\npark", "carry = \"visible\"\npark"),
        format!("schema = 1\n\n{RESCUE_A}\n{RESCUE_B}"),
    ] {
        let refused = PolicyFile::parse_for_cut(&not_the_hull).expect_err(&not_the_hull);
        assert!(
            refused.contains("not the hull of `[[ranges]]`")
                && refused.contains(
                    "write `applies_to_producers = [100, 299]`, `carry = \"repaint\"`, \
                     `park_quiet_gate_at_land = \"relaxed\"`"
                ),
            "{not_the_hull}: {refused}"
        );
    }
}

#[test]
fn a_producer_that_reads_ranges_follows_only_the_rescues_naming_its_build() {
    let overlap = "[[ranges]]\nproducers = [180, 260]\ncarry = \"visible\"\nreason = \"x\"\n";
    let read = PolicyRead::Parsed(file(&format!("{}\n{overlap}", two_rescues())));
    let follows = |build: u64| Adoption::of(&read, build).policy();
    let at = |build: u64| {
        follows(build).map(|policy| (policy.carry_ceiling(), policy.relaxes_land_gate()))
    };
    assert_eq!(at(100), Some((CarryCeiling::Repaint, false)));
    assert_eq!(at(150), Some((CarryCeiling::Repaint, false)));
    assert_eq!(
        at(190),
        Some((CarryCeiling::Repaint, false)),
        "A and the overlap"
    );
    assert_eq!(
        at(220),
        Some((CarryCeiling::Visible, false)),
        "the overlap alone"
    );
    assert_eq!(
        at(255),
        Some((CarryCeiling::Visible, true)),
        "B and the overlap"
    );
    assert_eq!(at(280), Some((CarryCeiling::Full, true)), "B alone");
    assert_eq!(
        follows(190).and_then(|policy| policy.applies_to_producers),
        Some(ProducerRange {
            min_build: 180,
            max_build: 199,
        })
    );
    for outside in [99, 300, u64::MAX] {
        assert!(
            matches!(Adoption::of(&read, outside), Adoption::NotForThisBuild(_)),
            "build {outside}"
        );
    }
    let gap = PolicyRead::Parsed(file(&two_rescues()));
    assert!(
        matches!(Adoption::of(&gap, 220), Adoption::NotForThisBuild(_)),
        "a build in the gap between the two rescues follows neither"
    );
}

#[test]
fn a_producer_before_ranges_reads_the_hull_and_is_never_refused() {
    let text = format!(
        "{}\n[[retired]]\nproducers = [1, 2]\nreason = \"x\"\ndate = \"2026-10-03\"\n",
        two_rescues()
    );
    let hull = policy(&text);
    assert_eq!(hull, file(&text).legacy);
    assert_eq!(
        hull,
        HandoffPolicy::parse(HULL_AB).expect("the top-level keys alone"),
        "the lists change nothing an older build reads"
    );
    let older = PolicyRead::Parsed(PolicyFile {
        legacy: hull,
        ranges: Ranges::Absent,
    });
    for build in [100, 150, 220, 280, 299] {
        let followed = Adoption::of(&older, build)
            .policy()
            .unwrap_or_else(|| panic!("build {build} follows the hull"));
        assert_eq!(followed.carry_ceiling(), CarryCeiling::Repaint);
        assert!(followed.relaxes_land_gate());
        let precise = file(&text).for_build(build);
        assert!(
            precise.is_none_or(|own| own.carry_ceiling() <= followed.carry_ceiling()
                && (!own.relaxes_land_gate() || followed.relaxes_land_gate())),
            "build {build}: the hull asks at least its own rescue"
        );
    }
    assert!(matches!(
        Adoption::of(&older, 300),
        Adoption::NotForThisBuild(_)
    ));
}

#[test]
fn a_ranges_list_this_build_cannot_read_leaves_it_on_the_hull() {
    for unreadable in [
        format!("{HULL_AB}\n[[ranges]]\nproducers = [100, 199]\ncarry = \"minimal\"\n"),
        format!("{HULL_AB}\n[[ranges]]\ncarry = \"repaint\"\n"),
        format!("{HULL_AB}ranges = 3\n"),
    ] {
        let read = file(&unreadable);
        assert!(
            matches!(&read.ranges, Ranges::Unreadable(_)),
            "{unreadable}: {read:?}"
        );
        let followed = Adoption::of(&PolicyRead::Parsed(read.clone()), 220)
            .policy()
            .unwrap_or_else(|| panic!("{unreadable}: the hull is followed"));
        assert_eq!(followed, read.legacy, "{unreadable}");
        assert!(read.rescues().is_err(), "the cutter cannot judge it either");
    }
}

#[test]
fn a_retirement_is_read_strictly_by_the_cutter_and_ignored_by_a_producer() {
    let entry = "[[retired]]\nproducers = [10, 20]\nreason = \"all past EOL\"\ndate = \
                 \"2026-10-03\"\n";
    let text = format!("schema = 1\n\n{entry}");
    assert_eq!(
        Retired::parse_for_cut(&text),
        Ok(vec![Retired {
            producers: ProducerRange {
                min_build: 10,
                max_build: 20,
            },
            reason: "all past EOL".to_string(),
            date: "2026-10-03".to_string(),
        }])
    );
    assert_eq!(PolicyFile::parse_for_cut(&text), Ok(PolicyFile::default()));
    assert_eq!(file(&text), PolicyFile::default());
    assert_eq!(Retired::parse_for_cut("schema = 1\n"), Ok(Vec::new()));
    for malformed in [
        "producers = [10, 20]\nreason = \"x\"\n",
        "producers = [10, 20]\ndate = \"2026-10-03\"\n",
        "producers = [10, 20]\nreason = \"  \"\ndate = \"2026-10-03\"\n",
        "producers = [20, 10]\nreason = \"x\"\ndate = \"2026-10-03\"\n",
        "producers = [10]\nreason = \"x\"\ndate = \"2026-10-03\"\n",
        "producers = [10, 20]\nreason = \"x\"\ndate = \"2026-13-03\"\n",
        "producers = [10, 20]\nreason = \"x\"\ndate = \"3 October\"\n",
        "producers = [10, 20]\nreason = \"x\"\ndate = \"2026-10-03\"\nextra = 1\n",
    ] {
        let text = format!("schema = 1\n\n[[retired]]\n{malformed}");
        let refused = PolicyFile::parse_for_cut(&text).expect_err(&text);
        assert!(
            refused.contains("[[retired]] table 1 is not")
                && refused.contains("`date = \"YYYY-MM-DD\"`"),
            "{text}: {refused}"
        );
    }
    let refused = PolicyFile::parse_for_cut("schema = 1\nretired = 3\n").expect_err("3");
    assert!(
        refused.contains("not a list of [[retired]] tables"),
        "{refused}"
    );
    let refused = PolicyFile::parse_for_cut(
        "schema = 1\nretire = { producers = [10, 20], reason = \"x\" }\n",
    )
    .expect_err("the old spelling");
    assert!(
        refused.contains("`retire` is now a `[[retired]]` table"),
        "{refused}"
    );
    let both = format!(
        "{}\n[[retired]]\nproducers = [250, 299]\nreason = \"x\"\ndate = \"2026-10-03\"\n",
        two_rescues()
    );
    let refused = PolicyFile::parse_for_cut(&both).expect_err("rescued and retired");
    assert!(
        refused.contains("builds [250, 299] are both rescued"),
        "{refused}"
    );
}

#[test]
fn a_malformed_policy_is_refused_whole_and_says_why() {
    for (text, why) in [
        ("carry = \"repaint\"\n", "no `schema = 1` line"),
        ("schema = 2\ncarry = \"repaint\"\n", "schema 2"),
        ("schema = \"1\"\n", "not an integer"),
        ("schema = 1\ncarry = \"sideways\"\n", "`carry`"),
        ("schema = 1\ncarry = 3\n", "`carry`"),
        (
            "schema = 1\npark_quiet_gate_at_land = \"loose\"\n",
            "park_quiet",
        ),
        (
            "schema = 1\napplies_to_producers = [5]\n",
            "applies_to_producers",
        ),
        (
            "schema = 1\napplies_to_producers = [1, 2, 3]\n",
            "applies_to",
        ),
        ("schema = 1\napplies_to_producers = [-1, 2]\n", "applies_to"),
        (
            "schema = 1\napplies_to_producers = [\"a\", 2]\n",
            "applies_to",
        ),
        ("schema = 1\napplies_to_producers = [9, 2]\n", "empty range"),
        ("schema = 1\napplies_to_producers = 7\n", "applies_to"),
        ("schema = 1\ncarry = \"repaint\n", "not TOML"),
    ] {
        let refused = HandoffPolicy::parse(text).expect_err(text);
        assert!(refused.contains(why), "{text:?}: {refused}");
        assert_eq!(
            PolicyFile::parse(text).expect_err(text),
            refused,
            "{text:?}"
        );
    }
}

#[test]
fn a_later_policy_keeps_every_knob_an_earlier_one_sealed() {
    let sealed = policy("schema = 1\napplies_to_producers = [10, 20]\ncarry = \"visible\"\n");
    for kept in [
        "schema = 1\napplies_to_producers = [10, 20]\ncarry = \"visible\"\n",
        "schema = 1\napplies_to_producers = [5, 30]\ncarry = \"visible\"\n",
        "schema = 1\napplies_to_producers = [10, 20]\ncarry = \"repaint\"\n",
        "schema = 1\ncarry = \"visible\"\n",
    ] {
        assert_eq!(file(kept).keeps(&sealed), Ok(()), "{kept}");
    }
    for (dropped, why) in [
        ("schema = 1\n", "raises `carry`"),
        (
            "schema = 1\napplies_to_producers = [11, 20]\ncarry = \"visible\"\n",
            "no longer applies",
        ),
        (
            "schema = 1\napplies_to_producers = [10, 19]\ncarry = \"repaint\"\n",
            "no longer applies",
        ),
        (
            "schema = 1\napplies_to_producers = [10, 20]\ncarry = \"full\"\n",
            "raises `carry`",
        ),
    ] {
        let refused = file(dropped).keeps(&sealed).expect_err(dropped);
        assert!(refused.contains(why), "{dropped}: {refused}");
    }
    let everyone = policy("schema = 1\ncarry = \"repaint\"\n");
    assert!(
        file("schema = 1\napplies_to_producers = [0, 99]\ncarry = \"repaint\"\n")
            .keeps(&everyone)
            .is_err()
    );
    let relaxed = policy("schema = 1\npark_quiet_gate_at_land = \"relaxed\"\n");
    assert!(
        file("schema = 1\npark_quiet_gate_at_land = \"strict\"\n")
            .keeps(&relaxed)
            .is_err()
    );
    assert_eq!(
        file("schema = 1\npark_quiet_gate_at_land = \"relaxed\"\n").keeps(&relaxed),
        Ok(())
    );
    for asks_nothing in [
        "schema = 1\n",
        "schema = 1\napplies_to_producers = [1, 2]\ncarry = \"full\"\n",
    ] {
        assert_eq!(
            file("schema = 1\n").keeps(&policy(asks_nothing)),
            Ok(()),
            "{asks_nothing}"
        );
    }
}

#[test]
fn a_sealed_rescue_is_kept_build_by_build_through_any_ranges_that_cover_it() {
    let sealed = policy(
        "schema = 1\napplies_to_producers = [100, 199]\ncarry = \"visible\"\n\
         park_quiet_gate_at_land = \"relaxed\"\n",
    );
    let ranged = |tables: &[(u64, u64, &str)]| {
        let mut text = "schema = 1\n".to_string();
        for (min, max, knobs) in tables {
            text.push_str(&format!(
                "\n[[ranges]]\nproducers = [{min}, {max}]\n{knobs}reason = \"r\"\n"
            ));
        }
        file(&text)
    };
    const KEPT: &str = "carry = \"visible\"\npark_quiet_gate_at_land = \"relaxed\"\n";
    const MORE: &str = "carry = \"repaint\"\npark_quiet_gate_at_land = \"relaxed\"\n";
    for (label, tables) in [
        ("the same", vec![(100, 199, KEPT)]),
        ("wider", vec![(50, 250, KEPT)]),
        ("split", vec![(100, 149, KEPT), (150, 199, MORE)]),
        (
            "an overlap asking more",
            vec![(100, 199, KEPT), (120, 300, "carry = \"repaint\"\n")],
        ),
        (
            "knobs from two tables",
            vec![
                (100, 199, "carry = \"visible\"\n"),
                (90, 210, "park_quiet_gate_at_land = \"relaxed\"\n"),
            ],
        ),
    ] {
        assert_eq!(ranged(&tables).keeps(&sealed), Ok(()), "{label}");
    }
    for (label, tables, why) in [
        (
            "a gap",
            vec![(100, 149, KEPT), (151, 199, KEPT)],
            "no longer applies to build 150",
        ),
        (
            "short at the top",
            vec![(100, 198, KEPT)],
            "no longer applies to build 199",
        ),
        (
            "a weaker stretch",
            vec![
                (100, 149, KEPT),
                (
                    150,
                    199,
                    "carry = \"full\"\npark_quiet_gate_at_land = \"relaxed\"\n",
                ),
            ],
            "raises `carry` for build 150",
        ),
        (
            "the gate dropped for a stretch",
            vec![(100, 159, KEPT), (160, 199, "carry = \"visible\"\n")],
            "drops `park_quiet_gate_at_land = \"relaxed\"` for build 160",
        ),
    ] {
        let refused = ranged(&tables).keeps(&sealed).expect_err(label);
        assert!(refused.contains(why), "{label}: {refused}");
    }
}

#[test]
fn the_range_is_inclusive_and_absent_means_every_producer() {
    let ranged = policy("schema = 1\napplies_to_producers = [100, 200]\ncarry = \"visible\"\n");
    for (build, applies) in [
        (0, false),
        (99, false),
        (100, true),
        (150, true),
        (200, true),
        (201, false),
        (u64::MAX, false),
    ] {
        assert_eq!(ranged.applies_to(build), applies, "build {build}");
    }
    let single = policy("schema = 1\napplies_to_producers = [7, 7]\n");
    assert!(single.applies_to(7) && !single.applies_to(6) && !single.applies_to(8));
    let everyone = policy("schema = 1\ncarry = \"visible\"\n");
    assert!(everyone.applies_to(0) && everyone.applies_to(u64::MAX));
}

#[test]
fn a_producer_follows_only_a_policy_that_asks_something_of_its_own_build() {
    let parsed = |text: &str| PolicyRead::Parsed(file(text));
    let repaint = parsed("schema = 1\napplies_to_producers = [10, 20]\ncarry = \"repaint\"\n");
    assert_eq!(
        Adoption::of(&repaint, 15)
            .policy()
            .map(|p| p.carry_ceiling()),
        Some(CarryCeiling::Repaint)
    );
    assert!(matches!(
        Adoption::of(&repaint, 21),
        Adoption::NotForThisBuild(_)
    ));
    assert_eq!(Adoption::of(&repaint, 9).policy(), None);
    for nothing in [
        "schema = 1\n",
        "schema = 1\ncarry = \"full\"\npark_quiet_gate_at_land = \"strict\"\n",
    ] {
        assert_eq!(
            Adoption::of(&parsed(nothing), 15),
            Adoption::None,
            "{nothing}"
        );
    }
    assert_eq!(Adoption::of(&PolicyRead::Absent, 15), Adoption::None);
    assert_eq!(
        Adoption::of(&PolicyRead::Ignored("bad".to_string()), 15),
        Adoption::Ignored("bad".to_string())
    );
    let relaxed = parsed("schema = 1\npark_quiet_gate_at_land = \"relaxed\"\n");
    assert!(
        Adoption::of(&relaxed, 15)
            .policy()
            .is_some_and(|p| p.relaxes_land_gate() && p.carry_ceiling() == CarryCeiling::Full)
    );
}

fn scratch_bundle(label: &str, write: impl FnOnce(&Path)) -> aterm_tempfile::TempDir {
    let root = crate::test_scratch_dir(&format!("hp-{label}"));
    let resources = root.path().join("Contents/Resources");
    std::fs::create_dir_all(&resources).expect("scratch bundle");
    write(&root.path().join(BUNDLE_PATH));
    root
}

#[test]
fn a_bundle_policy_is_read_absent_or_ignored_never_followed_through_a_link() {
    let absent = scratch_bundle("absent", |_| {});
    assert_eq!(read_from_bundle(absent.path()), PolicyRead::Absent);

    let parsed = scratch_bundle("parsed", |path| {
        std::fs::write(path, "schema = 1\ncarry = \"visible\"\n").expect("write")
    });
    assert_eq!(
        read_from_bundle(parsed.path()),
        PolicyRead::Parsed(PolicyFile {
            legacy: HandoffPolicy {
                carry: Some(CarryCeiling::Visible),
                ..HandoffPolicy::default()
            },
            ranges: Ranges::Absent,
        })
    );

    let malformed = scratch_bundle("malformed", |path| {
        std::fs::write(path, "schema = 1\ncarry = \"sideways\"\n").expect("write")
    });
    assert!(
        matches!(read_from_bundle(malformed.path()), PolicyRead::Ignored(why) if why.contains("carry"))
    );

    let huge = scratch_bundle("huge", |path| {
        let mut text = "schema = 1\n".to_string();
        text.push_str(&"# padding\n".repeat(2048));
        std::fs::write(path, text).expect("write")
    });
    assert!(
        matches!(read_from_bundle(huge.path()), PolicyRead::Ignored(why) if why.contains("bound"))
    );

    let binary = scratch_bundle("binary", |path| {
        std::fs::write(path, [0xff, 0xfe, 0x00]).expect("write")
    });
    assert!(
        matches!(read_from_bundle(binary.path()), PolicyRead::Ignored(why) if why.contains("UTF-8"))
    );

    let directory = scratch_bundle("directory", |path| {
        std::fs::create_dir_all(path).expect("mkdir")
    });
    let why_directory = if cfg!(unix) { "regular" } else { "unreadable" };
    assert!(
        matches!(read_from_bundle(directory.path()), PolicyRead::Ignored(why) if why.contains(why_directory))
    );

    #[cfg(unix)]
    {
        let target_dir = crate::test_scratch_dir("hp-target");
        let elsewhere = target_dir.path().join("target.toml");
        std::fs::write(&elsewhere, "schema = 1\ncarry = \"repaint\"\n").expect("write");
        let linked = scratch_bundle("linked", |path| {
            std::os::unix::fs::symlink(&elsewhere, path).expect("symlink")
        });
        assert!(
            matches!(read_from_bundle(linked.path()), PolicyRead::Ignored(why) if why.contains("regular")),
            "a link out of the bundle is never followed"
        );
    }
}

/// A read the system refused for now (out of descriptors or memory, an I/O error) is
/// `Unread`, not `Ignored`: nothing is known about what the file asks.
#[cfg(unix)]
#[test]
fn a_read_the_system_refused_for_now_is_unread_and_a_file_error_is_ignored() {
    for errno in [
        libc::EMFILE,
        libc::ENFILE,
        libc::ENOMEM,
        libc::ENOBUFS,
        libc::EIO,
        libc::EINTR,
        libc::EAGAIN,
    ] {
        let error = std::io::Error::from_raw_os_error(errno);
        assert!(
            matches!(unreadable(&error), ReadFailure::Moment(why) if why.contains("unreadable")),
            "errno {errno} ({error}) is a moment"
        );
        assert!(is_momentary_read_error(&error), "errno {errno}");
    }
    for errno in [libc::EACCES, libc::EPERM, libc::EISDIR, libc::ENOTDIR] {
        let error = std::io::Error::from_raw_os_error(errno);
        assert!(
            matches!(unreadable(&error), ReadFailure::File(_)),
            "errno {errno} ({error}) is a fact about the file"
        );
        assert!(!is_momentary_read_error(&error), "errno {errno}");
    }
    let at = std::path::Path::new("/x/Contents/Resources/aterm-handoff-policy.toml");
    let unread = judge(
        at,
        Err(unreadable(&std::io::Error::from_raw_os_error(libc::EMFILE))),
    );
    assert!(matches!(
        judge(
            at,
            Err(unreadable(&std::io::Error::from_raw_os_error(libc::EACCES)))
        ),
        PolicyRead::Ignored(_)
    ));
    assert!(matches!(&unread, PolicyRead::Unread(why) if why.contains("Too many open files")));
    assert_eq!(Adoption::of(&unread, 1).policy(), None);
}

/// A FIFO where the policy should be is refused at once, never waited on (the open a FIFO
/// renamed onto the path would meet).
#[cfg(unix)]
#[test]
fn a_fifo_in_place_of_the_policy_is_refused_without_waiting_for_a_writer() {
    let fifo = scratch_bundle("fifo", |path| {
        let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
            .expect("no NUL in a temp path");
        // SAFETY: `name` is a valid NUL-terminated path for the call.
        let made = unsafe { libc::mkfifo(name.as_ptr(), 0o600) };
        assert_eq!(made, 0, "mkfifo: {}", std::io::Error::last_os_error());
    });
    let (tx, rx) = std::sync::mpsc::channel();
    let root = fifo.path().to_path_buf();
    std::thread::spawn(move || {
        let _ = tx.send(read_from_bundle(&root));
    });
    let read = rx
        .recv_timeout(HANG_DETECTOR)
        .expect("the reader must refuse a FIFO at once, not wait for a writer");
    assert!(
        matches!(&read, PolicyRead::Ignored(why) if why.contains("regular")),
        "{read:?}"
    );
}

#[test]
fn names_producers_checks_policy_following_floor_and_sealing_ceiling() {
    let floor = FIRST_POLICY_FOLLOWING_BUILD;
    assert!(HandoffPolicy::default().names_producers(&[]).is_ok());

    let unbounded = policy("schema = 1\ncarry = \"visible\"\n");
    let empty_err = unbounded.names_producers(&[]).unwrap_err();
    assert!(empty_err.contains("no build can hand over"), "{empty_err}");
    let past_newest = unbounded.names_producers(&[floor, floor + 10]).unwrap_err();
    assert!(
        past_newest.contains(&format!("applies to builds after {}", floor + 10)),
        "{past_newest}"
    );

    let pre_policy = policy(&format!(
        "schema = 1\napplies_to_producers = [1, {}]\ncarry = \"visible\"\n",
        floor - 1
    ));
    let pre_err = pre_policy
        .names_producers(&[floor - 1, floor + 10])
        .unwrap_err();
    assert!(pre_err.contains(&format!("from {floor} on")), "{pre_err}");

    let valid = policy(&format!(
        "schema = 1\napplies_to_producers = [{floor}, {}]\ncarry = \"visible\"\n",
        floor + 10
    ));
    valid
        .names_producers(&[floor - 5, floor, floor + 10])
        .expect("covers policy-following builds without exceeding newest");

    let multi = file(&format!(
        "schema = 1\napplies_to_producers = [{floor}, {}]\ncarry = \"repaint\"\n\n\
         [[ranges]]\nproducers = [{floor}, {}]\ncarry = \"visible\"\nreason = \"first\"\n\n\
         [[ranges]]\nproducers = [{}, {}]\ncarry = \"repaint\"\nreason = \"too high\"\n",
        floor + 20,
        floor + 10,
        floor + 11,
        floor + 20,
    ));
    let file_err = multi.names_producers(&[floor, floor + 10]).unwrap_err();
    assert!(
        file_err.contains(&format!(
            "its rescue for builds [{}, {}]",
            floor + 11,
            floor + 20
        )),
        "{file_err}"
    );
    multi
        .names_producers(&[floor, floor + 15, floor + 20])
        .expect("both rescues valid when newest reaches floor + 20");
}

#[test]
fn adopt_returns_follow_policy_only_when_it_asks_something_of_this_build() {
    assert_eq!(adopt(&PolicyRead::Absent, 150, "v0.99.0"), None);
    assert_eq!(
        adopt(&PolicyRead::Ignored("corrupt".to_string()), 150, "v0.99.0"),
        None
    );
    assert_eq!(
        adopt(&PolicyRead::Unread("busy".to_string()), 150, "v0.99.0"),
        None
    );

    let parsed = PolicyRead::Parsed(file(&two_rescues()));
    assert_eq!(
        adopt(&parsed, 150, "v0.99.0").map(|p| p.carry_ceiling()),
        Some(CarryCeiling::Repaint)
    );
    assert_eq!(adopt(&parsed, 220, "v0.99.0"), None);

    let asks_nothing = PolicyRead::Parsed(file("schema = 1\ncarry = \"full\"\n"));
    assert_eq!(adopt(&asks_nothing, 150, "v0.99.0"), None);

    let unreadable_ranges = PolicyRead::Parsed(file(
        "schema = 1\napplies_to_producers = [100, 199]\ncarry = \"visible\"\nranges = 7\n",
    ));
    assert_eq!(
        adopt(&unreadable_ranges, 150, "v0.99.0").map(|p| p.carry_ceiling()),
        Some(CarryCeiling::Visible)
    );
}
