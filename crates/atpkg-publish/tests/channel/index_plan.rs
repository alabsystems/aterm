// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Index plan numbering, downgrade, yank, overlay, and metadata tests.

use std::collections::BTreeMap;

use atpkg::{Channel, Program};
use atpkg_publish::api::{Delta, Yank};
use atpkg_publish::channel::Log;
use atpkg_publish::channel::index::{Base, Plan, parse_delta, parse_yank, plan, render};
use atpkg_publish::error::Error;
use atpkg_publish::triple::{
    AARCH64_APPLE_DARWIN, AARCH64_UNKNOWN_LINUX_GNU, X86_64_UNKNOWN_LINUX_GNU,
};

const LINUX: &str = "x86_64-unknown-linux-gnu";
const LINUX_ARM: &str = "aarch64-unknown-linux-gnu";
const VU: &str = "2099-01-01T00:00:00Z";

fn block(repo: &str, group: Option<&str>) -> Program {
    Program {
        repo: repo.to_string(),
        policy: "prebuilt-only".to_string(),
        coherence_group: group.map(str::to_string),
        system: None,
        unavailable_hint: None,
        requires: Vec::new(),
    }
}

fn base(n: u64, pin: &[(&str, u64)]) -> Base {
    let programs = pin
        .iter()
        .map(|(p, _)| {
            let def = atpkg_publish::programs::get(p);
            (
                (*p).to_string(),
                block(def.map_or(*p, |d| d.repo), def.and_then(|d| d.group)),
            )
        })
        .collect();
    Base {
        index_build: n,
        roster_seq: 3,
        valid_until: Some(VU.to_string()),
        channel: Channel {
            name: "stable".into(),
            channel_build: n,
            min_build: 0,
            min_build_by_program: BTreeMap::new(),
            yanked: Vec::new(),
            pin: pin.iter().map(|(p, b)| ((*p).to_string(), *b)).collect(),
            pin_by_target: BTreeMap::new(),
        },
        programs,
    }
}

fn with_overlay(mut b: Base, triple: &str, cells: &[(&str, u64)]) -> Base {
    b.channel.pin_by_target.insert(
        triple.to_string(),
        cells.iter().map(|(p, v)| ((*p).to_string(), *v)).collect(),
    );
    b
}

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| (*s).to_string()).collect()
}

fn delta(a: &[&str]) -> Delta {
    parse_delta(&args(a)).expect("the delta parses")
}

fn run(b: &Base, d: &Delta) -> (Result<Plan, Error>, Log) {
    let mut log = Log::capture("test");
    let p = plan(b, d, "m3", 3, VU, &mut log);
    (p, log)
}

fn ok(b: &Base, d: &Delta) -> (Plan, Log) {
    let (p, log) = run(b, d);
    (p.unwrap_or_else(|e| panic!("{e}\n{}", log.text())), log)
}

fn refused(b: &Base, d: &Delta) -> String {
    match run(b, d).0 {
        Ok(p) => panic!("planned, but must be refused: {p:?}"),
        Err(e) => e.to_string(),
    }
}

#[test]
fn the_number_is_the_heads_plus_one_and_everything_unnamed_is_carried() {
    let mut b = with_overlay(base(21, &[("ay", 5), ("nn", 108)]), LINUX, &[("ay", 9)]);
    b.channel.min_build = 4;
    b.channel.yanked = vec!["ay@3".into()];
    b.channel.min_build_by_program.insert("nn".into(), 100);
    let (p, _) = ok(&b, &Delta::default());
    assert_eq!(p.index_build, 22);
    assert_eq!(p.channel.channel_build, 22);
    assert_eq!(p.channel.pin, b.channel.pin);
    assert_eq!(p.channel.pin_by_target, b.channel.pin_by_target);
    assert_eq!(p.channel.yanked, vec!["ay@3".to_string()]);
    assert_eq!(p.channel.min_build, 4);
    assert_eq!(p.channel.min_build_by_program.get("nn"), Some(&100));
    assert_eq!(p.valid_until, VU);
}

#[test]
fn a_pin_moves_up_never_down_and_an_equal_build_is_a_carry() {
    let b = base(21, &[("ay", 5), ("nn", 108)]);
    let err = refused(&b, &delta(&["--pin", "ay=4"]));
    assert!(
        err.contains("REFUSING pin downgrade for ay: head build 5 > requested 4"),
        "{err}"
    );
    let (p, _) = ok(&b, &delta(&["--pin", "ay=5"]));
    assert!(p.moved.is_empty() && p.carried.iter().any(|(n, v)| n == "ay" && *v == 5));
    let (p, _) = ok(&b, &delta(&["--pin", "ay=6"]));
    assert_eq!(p.moved.len(), 1);
    assert_eq!((p.moved[0].from, p.moved[0].to), (5, 6));
}

#[test]
fn a_repo_the_table_moves_needs_a_new_build_above_the_heads_pin() {
    let mut moved = base(21, &[("ay", 5), ("nn", 108)]);
    moved.programs.insert("ay".into(), block("ay-old", None));
    for d in [&["--pin", "nn=109"][..], &["--pin", "ay=5"]] {
        let err = refused(&moved, &delta(d));
        assert!(
            err.contains("REFUSING [programs.ay] repo \"ay-old\" -> \"ay\" with ay pinned at 5")
                && err.contains("atpkg::succession"),
            "{d:?}: {err}"
        );
    }
    let (p, log) = ok(&moved, &delta(&["--pin", "ay=6"]));
    let ay = p.programs.iter().find(|(n, _)| n == "ay").map(|(_, b)| b);
    assert_eq!(ay.map(|b| b.repo.as_str()), Some("ay"));
    assert!(
        log.contains("metadata: ay: repo=\"ay-old\""),
        "{}",
        log.text()
    );
    // The control: the head already names the table's repo, so nothing switches.
    let (p, _) = ok(
        &base(21, &[("ay", 5), ("nn", 108)]),
        &delta(&["--pin", "nn=109"]),
    );
    assert_eq!(p.channel.pin.get("ay"), Some(&5));
}

#[test]
fn a_repo_the_table_moves_needs_a_new_build_on_each_target_cell_it_keeps() {
    let mut moved = with_overlay(base(21, &[("ay", 5), ("nn", 108)]), LINUX, &[("ay", 9)]);
    moved.programs.insert("ay".into(), block("ay-old", None));
    let target_pin = format!("{LINUX}:ay=9");
    for d in [
        &["--pin", "ay=6"][..],
        &["--pin", "ay=6", "--target-pin", &target_pin],
    ] {
        let err = refused(&moved, &delta(d));
        assert!(
            err.contains(&format!(
                "REFUSING [programs.ay] repo \"ay-old\" -> \"ay\" with ay pinned on {LINUX} at 9"
            )),
            "{d:?}: {err}"
        );
    }
    // The controls: the cell names a new build, or the overlay goes.
    let raised = format!("{LINUX}:ay=10");
    let (p, _) = ok(&moved, &delta(&["--pin", "ay=6", "--target-pin", &raised]));
    assert_eq!(p.channel.pin_by_target[LINUX].get("ay"), Some(&10));
    let retired = format!("{LINUX}:ay");
    let (p, _) = ok(
        &moved,
        &delta(&["--pin", "ay=6", "--retire-target", &retired]),
    );
    assert!(p.channel.pin_by_target.is_empty(), "{:?}", p.channel);
}

#[test]
fn a_program_leaves_only_when_retired_by_name() {
    let mut b = with_overlay(
        base(21, &[("ay", 1), ("ny", 2), ("codex", 3)]),
        LINUX,
        &[("ny", 7)],
    );
    b.channel.yanked = vec!["ny@1".into()];
    let (p, _) = ok(&b, &delta(&["--pin", "ay=2"]));
    assert!(p.channel.pin.contains_key("codex") && p.channel.pin.contains_key("ny"));

    let (p, _) = ok(&b, &delta(&["--retire", "ny"]));
    assert!(!p.channel.pin.contains_key("ny"));
    assert!(
        p.channel.pin_by_target.is_empty(),
        "the retired program's overlay goes with it"
    );
    assert!(p.channel.yanked.is_empty());
    assert!(p.programs.iter().all(|(n, _)| n != "ny"));
    assert_eq!(p.retired, vec!["ny".to_string()]);

    let err = refused(&b, &delta(&["--retire", "ghost"]));
    assert!(err.contains("the head does not pin ghost"), "{err}");
    let err = refused(&b, &delta(&["--retire", "ay", "--pin", "ay=3"]));
    assert!(err.contains("both moved and retired"), "{err}");
}

#[test]
fn a_new_program_is_a_table_row_and_a_vendor_program_is_never_pinned() {
    let b = base(21, &[("ay", 1), ("claude", 2_026_092_401)]);
    let (p, _) = ok(&b, &delta(&["--pin", "ty=3007"]));
    assert_eq!(p.channel.pin.get("ty"), Some(&3007));
    assert_eq!(p.channel.pin.get("claude"), Some(&2_026_092_401));
    let err = refused(&b, &delta(&["--pin", "ghost=1"]));
    assert!(err.contains("not a program this build knows"), "{err}");
    for v in ["claude", "agy"] {
        let err = refused(&b, &delta(&["--pin", &format!("{v}=2026100802")]));
        assert!(err.contains(&format!("{v} is vendor-direct")), "{err}");
        let err = refused(
            &b,
            &delta(&["--target-pin", &format!("{LINUX}:{v}=2026100802")]),
        );
        assert!(err.contains(&format!("{v} is vendor-direct")), "{err}");
    }
}

const HARNESSES: [(&str, &str); 5] = [
    ("claude-code-alab", "claude-alab"),
    ("muse-alab", "muse-alab"),
    ("agy-alab", "agy-alab"),
    ("grok-alab", "grok-alab"),
    ("kimi-alab", "kimi-alab"),
];

/// THE FIVE HARNESS ROWS (`docs/DESIGN-harness-repos-2026-10-24.md` §6.1): each is its own
/// repo's Apple-silicon plain row, held to the harness rule with the repo's one binary as its
/// one tool (a tool a client lays and no vendor row's), and no other row is named like one.
#[test]
fn the_harness_rows_are_the_five_harnesses_each_shipping_its_one_binary() {
    use atpkg_publish::programs::{ARM_DARWIN, PROGRAMS};
    let rows: Vec<(&str, &str)> = PROGRAMS
        .iter()
        .filter_map(|d| d.harness().map(|h| (d.name, h.tool)))
        .collect();
    assert_eq!(rows, HARNESSES);
    for (name, tool) in HARNESSES {
        let d = atpkg_publish::programs::get(name).expect("a row");
        assert_eq!((d.repo, d.group, d.system), (name, None, None));
        assert!(d.is_plain() && d.lane_tracked(), "{name}");
        assert_eq!(d.darwin_triples(), ARM_DARWIN, "{name}");
        assert!(atpkg_publish::programs::alab_package(name).is_none());
        assert!(atpkg::store::shim_allowed(tool), "{tool}");
        assert!(
            atpkg::vendor_direct::VENDORS
                .iter()
                .all(|v| !v.exposes.contains(&tool)),
            "{tool} would shadow a vendor's shim"
        );
    }
    assert!(
        PROGRAMS
            .iter()
            .filter(|d| d.harness().is_none())
            .all(|d| !atpkg::harness::is_harness(d.name)),
        "a program named like a harness is packed as one"
    );
}

/// `--pin <harness>=7` for each of the five.
fn harness_pins() -> Delta {
    let pins: Vec<String> = HARNESSES
        .iter()
        .flat_map(|(h, _)| ["--pin".to_string(), format!("{h}=7")])
        .collect();
    let pins: Vec<&str> = pins.iter().map(String::as_str).collect();
    delta(&pins)
}

/// A HARNESS PINS LIKE ANY ROW, INTO THE CLIENT'S DEFAULT SET (owner ruling 2026-10-27,
/// `docs/DESIGN-harness-repos-2026-10-24.md` §11 D45): `plan` admits `--pin` of each of the five,
/// its block names its own repo with no group and the prebuilt-only policy, and the index it
/// makes installs it unasked; a head's harness pin, one the table lacks included, is carried
/// like any other.
#[test]
fn each_harness_pins_under_its_own_repo_into_the_default_set() {
    let b = base(21, &[("ay", 1)]);
    let (p, _) = ok(&b, &harness_pins());
    for (h, _) in HARNESSES {
        assert_eq!(p.channel.pin.get(h), Some(&7), "{h}");
        let (_, block) = p
            .programs
            .iter()
            .find(|(n, _)| n == h)
            .unwrap_or_else(|| panic!("no [programs.{h}]"));
        assert_eq!(block.repo, h);
        assert_eq!(block.coherence_group, None);
        assert_eq!(block.policy, "prebuilt-only");
    }
    let wanted = index_of(&p).installable(&[]);
    for (h, _) in HARNESSES {
        assert!(wanted.contains(h), "{h}: every client installs it unasked");
    }
    assert!(wanted.contains("ay"));

    let held = base(21, &[("ay", 1), ("muse-alab", 4), ("orca-alab", 2)]);
    let (p, _) = ok(&held, &delta(&["--pin", "ay=2"]));
    assert_eq!(p.channel.pin.get("ay"), Some(&2));
    assert_eq!(p.channel.pin.get("muse-alab"), Some(&4), "carried");
    assert_eq!(p.channel.pin.get("orca-alab"), Some(&2), "carried");
    assert!(p.retired.is_empty());
}

#[test]
fn a_head_program_the_table_lacks_keeps_the_heads_block() {
    let mut b = base(21, &[("ay", 1), ("orca", 4)]);
    b.programs.insert("ay".into(), block("ay-old", None));
    // ay's repo moves to the table's, so its pin names a new build (the repo-switch rule).
    let (p, _) = ok(&b, &delta(&["--pin", "orca=5", "--pin", "ay=2"]));
    let names: Vec<&str> = p.programs.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["ay", "orca"], "table order, then the rest");
    assert_eq!(p.programs[0].1.repo, "ay", "the table's block is rendered");
    assert_eq!(p.programs[1].1.repo, "orca", "the head's block is carried");
}

#[test]
fn per_target_overlays_render_carry_and_floor_their_own_cells() {
    let b = base(21, &[("ay", 5)]);
    let (p, _) = ok(&b, &delta(&["--target-pin", &format!("{LINUX}:ay=9")]));
    assert_eq!(
        p.channel.pin.get("ay"),
        Some(&5),
        "an overlay never touches `pin`"
    );
    assert_eq!(p.channel.pin_by_target[LINUX].get("ay"), Some(&9));

    let (p, _) = ok(
        &b,
        &delta(&[
            "--target-pin",
            &format!("{LINUX}:ay=9"),
            "--target-pin",
            &format!("{LINUX_ARM}:ay=7"),
        ]),
    );
    assert_eq!(p.channel.pin_by_target.len(), 2);

    let lin = with_overlay(base(22, &[("ay", 5)]), LINUX, &[("ay", 9)]);
    let err = refused(&lin, &delta(&["--target-pin", &format!("{LINUX}:ay=8")]));
    assert!(
        err.contains(&format!(
            "REFUSING per-target pin downgrade for ay on {LINUX}: head 9 > requested 8"
        )),
        "{err}"
    );
    ok(&lin, &delta(&["--target-pin", &format!("{LINUX}:ay=9")]));
    let (up, _) = ok(&lin, &delta(&["--target-pin", &format!("{LINUX}:ay=10")]));
    assert_eq!(up.channel.pin_by_target[LINUX].get("ay"), Some(&10));

    let (carried, _) = ok(&lin, &delta(&["--pin", "ay=6"]));
    assert_eq!(carried.channel.pin_by_target[LINUX].get("ay"), Some(&9));

    let two = with_overlay(lin.clone(), LINUX_ARM, &[("ay", 7)]);
    let (p, _) = ok(&two, &delta(&["--target-pin", &format!("{LINUX}:ay=11")]));
    assert_eq!(p.channel.pin_by_target[LINUX].get("ay"), Some(&11));
    assert_eq!(p.channel.pin_by_target[LINUX_ARM].get("ay"), Some(&7));

    let err = refused(&b, &delta(&["--target-pin", &format!("{LINUX}:trust=1")]));
    assert!(
        err.contains("names trust, which this index does not pin"),
        "{err}"
    );
}

#[test]
fn an_overlay_leaves_only_when_retired_by_name() {
    let lin = with_overlay(
        base(22, &[("ay", 5), ("nn", 1)]),
        LINUX,
        &[("ay", 9), ("nn", 2)],
    );
    let (p, _) = ok(&lin, &delta(&["--retire-target", LINUX]));
    assert!(p.channel.pin_by_target.is_empty());
    let (p, _) = ok(&lin, &delta(&["--retire-target", &format!("{LINUX}:ay")]));
    assert_eq!(
        p.channel.pin_by_target[LINUX].keys().collect::<Vec<_>>(),
        ["nn"]
    );
    let err = run(&lin, &delta(&["--retire-target", LINUX_ARM]))
        .0
        .expect_err("absent");
    assert!(err.to_string().contains("nothing to retire"), "{err}");
    let err = run(
        &lin,
        &delta(&[
            "--retire-target",
            LINUX,
            "--target-pin",
            &format!("{LINUX}:ay=10"),
        ]),
    )
    .0
    .expect_err("both");
    assert!(err.to_string().contains("both pinned and retired"), "{err}");
}

#[test]
fn the_command_line_refuses_duplicates_and_malformed_values() {
    let dup = parse_delta(&args(&[
        "--target-pin",
        &format!("{LINUX}:ay=9"),
        "--target-pin",
        &format!("{LINUX}:ay=10"),
    ]))
    .expect_err("one build per program per target");
    assert!(
        dup.to_string()
            .contains(&format!("pins ay on {LINUX} twice")),
        "{dup}"
    );
    let nine = format!("{LINUX}:ay=nine");
    for bad in [
        vec!["--target-pin", "notatriple:ay=9"],
        vec!["--target-pin", nine.as_str()],
        vec!["--pin", "ay"],
        vec!["--pin", "ay=-"],
        vec!["--pin", "ay=v0.2.0"],
        vec!["--pin", "ay=1", "--pin", "ay=2"],
        vec!["--allow-triple-drop", "ay=x86_64-apple-darwin"],
        vec!["--reason", "why"],
        vec!["--bogus"],
    ] {
        let err = parse_delta(&args(&bad)).expect_err("refused");
        assert_eq!(err.exit_code(), 2, "{bad:?}: {err}");
    }
    let d = delta(&[
        "--allow-triple-drop",
        "ay=x86_64-apple-darwin",
        "--reason",
        "Apple Silicon only",
    ]);
    assert_eq!(d.triple_drops.len(), 1);
    assert_eq!(d.triple_drops[0].reason, "Apple Silicon only");
    let d = delta(&[
        "--retire-target",
        LINUX,
        "--retire-target",
        &format!("{LINUX_ARM}:ay"),
    ]);
    assert!(d.retire_target.contains(&(X86_64_UNKNOWN_LINUX_GNU, None)));
    assert!(
        d.retire_target
            .contains(&(AARCH64_UNKNOWN_LINUX_GNU, Some("ay".to_string())))
    );
}

#[test]
fn yanks_are_carried_added_lifted_and_held_to_the_pins() {
    let b = base(1, &[("ay", 3), ("codex", 3)]);
    let (p, _) = ok(&b, &delta(&["--yank", "codex@3", "--yank", "ay@1"]));
    assert_eq!(p.channel.yanked, ["ay@1", "codex@3"]);
    let carried = Base {
        channel: Channel {
            yanked: vec!["ay@1".into()],
            ..b.channel.clone()
        },
        ..b.clone()
    };
    let (p, _) = ok(&carried, &Delta::default());
    assert_eq!(p.channel.yanked, ["ay@1"], "a yank nobody re-passed stays");
    let (p, _) = ok(&carried, &delta(&["--unyank", "ay@1"]));
    assert!(p.channel.yanked.is_empty());
    let err = refused(&carried, &delta(&["--unyank", "ay@9"]));
    assert!(err.contains("the head does not yank it"), "{err}");
    let err = refused(&b, &delta(&["--yank", "ghost@1"]));
    assert!(err.contains("which this index does not pin"), "{err}");
    let (p, _) = ok(&carried, &delta(&["--yank", "codex@2.1.279"]));
    assert_eq!(p.channel.yanked, ["ay@1", "codex@2.1.279"]);
    let muse_plain_id = format!(
        "muse@{}",
        atpkg::vendor_direct::Version::parse("1.4.9")
            .expect("x.y.z")
            .build_id()
    );
    for (entry, why) in [
        ("muse@1.4.9", "muse's versions are spelt x.y.z-R<n>[.<s>]"),
        (muse_plain_id.as_str(), "muse's versions are spelt x.y.z-R"),
        ("agy@1.4.0-R1", "agy's versions are spelt x.y.z"),
        (
            "claude@2999999999999999999",
            "claude's versions are spelt x.y.z",
        ),
        (
            "claude@3000000000000000000",
            "claude's versions are spelt x.y.z",
        ),
        ("ay@1.2.3", "ay is not vendor-direct"),
    ] {
        let err = refused(&b, &delta(&["--yank", entry]));
        assert!(err.contains("matches no version any client holds"), "{err}");
        assert!(err.contains(why), "{entry}: {err}");
    }
    let vendor = Base {
        channel: Channel {
            yanked: p.channel.yanked.clone(),
            ..b.channel.clone()
        },
        ..b.clone()
    };
    let (p, _) = ok(&vendor, &delta(&["--retire", "codex"]));
    assert_eq!(p.channel.yanked, ["ay@1", "codex@2.1.279"]);

    assert_eq!(
        parse_yank("ay=1", true).expect("="),
        Yank::Build("ay".into(), 1)
    );
    ok(&b, &delta(&["--yank", "ay=1"]));
    ok(&b, &delta(&["--yank", "ay@3"]));
    for bad in [
        "codex3",
        "codex@x",
        "ay-3",
        "codex@2.01.3",
        "codex@2.1",
        "codex@v2.1.3",
        "codex@1.2.3.4",
    ] {
        assert!(parse_yank(bad, true).is_err(), "{bad} must be refused");
    }
}

#[test]
fn the_floor_is_carried_and_lowered_only_on_purpose() {
    let mut b = base(2, &[("ay", 1)]);
    b.channel.min_build = 5;
    let (p, _) = ok(&b, &Delta::default());
    assert_eq!(p.channel.min_build, 5);
    let err = refused(&b, &delta(&["--min-build", "4"]));
    assert!(err.contains("REFUSING min_build 4 < the head's 5"), "{err}");
    let (p, _) = ok(&b, &delta(&["--min-build", "0", "--lower-min-build"]));
    assert_eq!(p.channel.min_build, 0);
    let (p, _) = ok(&b, &delta(&["--min-build", "9"]));
    assert_eq!(p.channel.min_build, 9);
}

#[test]
fn a_roster_older_than_the_heads_generation_is_refused() {
    let b = base(2, &[("ay", 1)]);
    let mut log = Log::capture("test");
    let err = plan(&b, &Delta::default(), "m3", 2, VU, &mut log).expect_err("older");
    assert!(
        err.to_string()
            .contains("REFUSING roster_seq 2 < the head's 3"),
        "{err}"
    );
}

fn index_of(p: &Plan) -> atpkg::Index {
    atpkg::Index {
        schema: 2,
        index_build: p.index_build,
        generated_at: String::new(),
        valid_until: p.valid_until.clone(),
        machine_id: Some(p.machine_id.clone()),
        roster_seq: Some(p.roster_seq),
        programs: p.programs.iter().cloned().collect(),
        channels: vec![p.channel.clone()],
    }
}

fn live_head() -> Base {
    let mut b = base(
        71,
        &[
            ("ay", 5),
            ("claude", 2_026_092_401),
            ("codex", 2_026_101_501),
        ],
    );
    b.programs.insert("claude".into(), block("aterm", None));
    b.programs.insert("codex".into(), block("aterm", None));
    b
}

#[test]
fn a_routine_publish_carries_the_live_codex_row_and_routes_nothing_to_alab() {
    let b = live_head();
    let (p, log) = ok(&b, &delta(&["--pin", "ay=6"]));
    assert_eq!(p.channel.pin.get("codex"), Some(&2_026_101_501));
    let order: Vec<&str> = p.programs.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(order, ["ay", "claude", "codex"], "the head-only order");
    let codex = p
        .programs
        .iter()
        .find(|(n, _)| n == "codex")
        .map(|(_, b)| b);
    assert_eq!(
        format!("{codex:?}"),
        format!("{:?}", b.programs.get("codex")),
        "the head's block, as it stands"
    );
    assert!(
        render(&p).contains("\n[programs.codex]\nrepo = \"aterm\"\npolicy = \"prebuilt-only\"\n"),
        "{}",
        render(&p)
    );
    assert!(
        log.contains("codex: the head's [programs.codex] (repo \"aterm\") and its pin are carried")
    );
    assert!(
        !log.contains("metadata: codex"),
        "no metadata move is said or made"
    );
    let err = refused(
        &b,
        &delta(&[
            "--target-pin",
            &format!("{}:codex=2026101601", AARCH64_APPLE_DARWIN.as_str()),
        ]),
    );
    assert!(err.contains("an overlay never pins it"), "{err}");
    let index = index_of(&p);
    for triple in [
        AARCH64_APPLE_DARWIN.as_str(),
        "x86_64-apple-darwin",
        LINUX,
        LINUX_ARM,
    ] {
        assert_eq!(
            atpkg::alab::index_pin(&index, "codex", triple),
            None,
            "{triple}"
        );
    }
    let mut turned = index.clone();
    turned.programs.get_mut("codex").unwrap().repo = "codex-alab".into();
    assert_eq!(
        atpkg::alab::index_pin(&turned, "codex", AARCH64_APPLE_DARWIN.as_str()),
        None
    );
}

#[test]
fn an_explicit_pin_switches_the_codex_row_to_alab_and_nothing_else_does() {
    let b = live_head();
    let (p, log) = ok(&b, &delta(&["--pin", "codex=2026101601"]));
    assert_eq!(p.channel.pin.get("codex"), Some(&2_026_101_601));
    let order: Vec<&str> = p.programs.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(order, ["ay", "codex", "claude"], "the table's order");
    let codex = p
        .programs
        .iter()
        .find(|(n, _)| n == "codex")
        .map(|(_, b)| b);
    assert_eq!(codex.map(|b| b.repo.as_str()), Some("codex-alab"));
    assert!(
        log.contains("metadata: codex: repo=\"aterm\""),
        "{}",
        log.text()
    );
    let index = index_of(&p);
    assert_eq!(
        atpkg::alab::index_pin(&index, "codex", AARCH64_APPLE_DARWIN.as_str()),
        Some(2_026_101_601)
    );
    for triple in ["x86_64-apple-darwin", LINUX, LINUX_ARM] {
        assert_eq!(
            atpkg::alab::index_pin(&index, "codex", triple),
            None,
            "{triple}"
        );
    }

    for (value, needle) in [
        ("codex=2026101501", "ALab store window"),
        ("codex=2026101500", "ALab store window"),
        ("codex=2026092401", "ALab store window"),
        ("codex=1000000000000000000", "ALab store window"),
        ("claude=2026101601", "claude is vendor-direct"),
    ] {
        let err = refused(&b, &delta(&["--pin", value]));
        assert!(err.contains(needle), "{value}: {err}");
    }

    let mut alab = live_head();
    alab.channel.pin.insert("codex".into(), 2_026_101_601);
    alab.programs
        .insert("codex".into(), block("codex-alab", None));
    let (p, log) = ok(&alab, &delta(&["--pin", "ay=6"]));
    let codex = p
        .programs
        .iter()
        .find(|(n, _)| n == "codex")
        .map(|(_, b)| b);
    assert_eq!(
        format!("{codex:?}"),
        format!("{:?}", alab.programs.get("codex"))
    );
    assert!(!log.contains("metadata: codex"));
    let (p, _) = ok(&alab, &delta(&["--pin", "codex=2026101601"]));
    assert_eq!(p.channel.pin.get("codex"), Some(&2_026_101_601));
    let err = refused(&alab, &delta(&["--pin", "codex=2026101600"]));
    assert!(err.contains("REFUSING pin downgrade for codex"), "{err}");
    let (p, _) = ok(&alab, &delta(&["--pin", "codex=2026101701"]));
    assert_eq!(p.channel.pin.get("codex"), Some(&2_026_101_701));

    let mut bare = live_head();
    bare.programs.remove("codex");
    let err = refused(&bare, &delta(&["--pin", "ay=6"]));
    assert!(err.contains("refusing to render ALab's row"), "{err}");
}
