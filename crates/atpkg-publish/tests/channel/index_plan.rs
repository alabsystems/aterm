// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The index PLAN over typed data: numbering, downgrades, omissions, yanks, `min_build`,
//! per-target overlays carried / retired / duplicated, vendor-direct programs and metadata.

use std::collections::BTreeMap;

use atpkg::{Channel, Program};
use atpkg_publish::api::{Delta, Yank};
use atpkg_publish::channel::Log;
use atpkg_publish::channel::index::{Base, Plan, parse_delta, parse_yank, plan};
use atpkg_publish::error::Error;
use atpkg_publish::triple::{
    AARCH64_APPLE_DARWIN, AARCH64_UNKNOWN_LINUX_GNU, Triple, X86_64_UNKNOWN_LINUX_GNU,
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
    let (p, log) = ok(&b, &Delta::default());
    assert_eq!(p.index_build, 22);
    assert_eq!(p.channel.channel_build, 22);
    assert_eq!(p.channel.pin, b.channel.pin);
    assert_eq!(p.channel.pin_by_target, b.channel.pin_by_target);
    assert_eq!(p.channel.yanked, vec!["ay@3".to_string()]);
    assert_eq!(p.channel.min_build, 4);
    assert_eq!(p.channel.min_build_by_program.get("nn"), Some(&100));
    assert_eq!(p.valid_until, VU);
    assert!(log.contains("per-target pins carried from the head: x86_64-unknown-linux-gnu:ay=9"));
    assert!(log.contains("no pin moves"), "{}", log.text());
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
    let (p, log) = ok(&b, &delta(&["--pin", "ay=6"]));
    assert_eq!(p.moved.len(), 1);
    assert_eq!((p.moved[0].from, p.moved[0].to), (5, 6));
    assert!(log.contains("updated (new build, must be public before the index is): ay@5->6"));
    assert!(log.contains("carry-forward (unchanged build, the published pack reused): nn@108"));
}

/// The omission gate is structural: a pin the head carries and the delta does not
/// retire is never dropped; a retirement is typed, and takes its overlays and yanks.
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

    let (p, log) = ok(&b, &delta(&["--retire", "ny"]));
    assert!(!p.channel.pin.contains_key("ny"));
    assert!(
        p.channel.pin_by_target.is_empty(),
        "the retired program's overlay goes with it"
    );
    assert!(p.channel.yanked.is_empty());
    assert!(p.programs.iter().all(|(n, _)| n != "ny"));
    assert_eq!(p.retired, vec!["ny".to_string()]);
    assert!(log.contains(
        "per-target pins DROPPED, not carried forward: x86_64-unknown-linux-gnu:ny=7 (retired)"
    ));
    assert!(log.contains("the head's yank ny@1 goes with ny"));
    assert!(log.contains("retiring head program(s) as asked: ny"));

    let err = refused(&b, &delta(&["--retire", "ghost"]));
    assert!(err.contains("the head does not pin ghost"), "{err}");
    let err = refused(&b, &delta(&["--retire", "ay", "--pin", "ay=3"]));
    assert!(err.contains("both moved and retired"), "{err}");
}

/// A new program is a table row; a vendor-direct program is never pinned, whether the head
/// pins it (claude) or not (agy), on `pin` or on a target. A head pin of one is carried
/// until `--retire` drops it.
#[test]
fn a_new_program_is_a_table_row_and_a_vendor_program_is_never_pinned() {
    let b = base(21, &[("ay", 1), ("claude", 2_026_092_401)]);
    let (p, log) = ok(&b, &delta(&["--pin", "ty=3007"]));
    assert_eq!(p.channel.pin.get("ty"), Some(&3007));
    assert_eq!(p.channel.pin.get("claude"), Some(&2_026_092_401));
    assert!(log.contains("new programs (not in the head): ty@3007"));
    assert!(log.contains("claude is vendor-direct — its head pin and block are carried"));
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

#[test]
fn a_head_program_the_table_lacks_keeps_the_heads_block_and_metadata_moves_are_said() {
    let mut b = base(21, &[("ay", 1), ("orca-alab", 4)]);
    b.programs.insert("ay".into(), block("ay-old", None));
    let (p, log) = ok(&b, &delta(&["--pin", "orca-alab=5"]));
    let names: Vec<&str> = p.programs.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["ay", "orca-alab"], "table order, then the rest");
    assert_eq!(p.programs[0].1.repo, "ay", "the table's block is rendered");
    assert_eq!(
        p.programs[1].1.repo, "orca-alab",
        "the head's block is carried"
    );
    assert!(log.contains("metadata: ay:"), "{}", log.text());
    assert!(log.contains("orca-alab is not a row of programs::PROGRAMS"));
}

#[test]
fn per_target_overlays_render_carry_and_floor_their_own_cells() {
    let b = base(21, &[("ay", 5)]);
    let (p, log) = ok(&b, &delta(&["--target-pin", &format!("{LINUX}:ay=9")]));
    assert_eq!(
        p.channel.pin.get("ay"),
        Some(&5),
        "an overlay never touches `pin`"
    );
    assert_eq!(p.channel.pin_by_target[LINUX].get("ay"), Some(&9));
    assert!(log.contains(&format!("per-target pins for {LINUX}: ay = 9")));

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

    // Every Mac cut passes no overlay: the head's carries, and still floors the next.
    let (carried, log) = ok(&lin, &delta(&["--pin", "ay=6"]));
    assert_eq!(carried.channel.pin_by_target[LINUX].get("ay"), Some(&9));
    assert!(log.contains(&format!("carried from the head: {LINUX}:ay=9")));

    // A partial re-state keeps every other target's overlay.
    let two = with_overlay(lin.clone(), LINUX_ARM, &[("ay", 7)]);
    let (p, _) = ok(&two, &delta(&["--target-pin", &format!("{LINUX}:ay=11")]));
    assert_eq!(p.channel.pin_by_target[LINUX].get("ay"), Some(&11));
    assert_eq!(p.channel.pin_by_target[LINUX_ARM].get("ay"), Some(&7));

    // An overlay rides `pin`: it never lists a program alone.
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
    let (p, log) = ok(&lin, &delta(&["--retire-target", LINUX]));
    assert!(p.channel.pin_by_target.is_empty());
    assert!(log.contains(&format!(
        "per-target pins DROPPED, not carried forward: {LINUX}:ay=9 {LINUX}:nn=2"
    )));
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

/// The parse refuses what would sign a duplicate key, or a value no client reads.
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
    let _: Triple = AARCH64_APPLE_DARWIN;
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
    let (p, log) = ok(&carried, &delta(&["--unyank", "ay@1"]));
    assert!(p.channel.yanked.is_empty());
    assert!(log.contains("--unyank lifts ay@1"));
    let err = refused(&carried, &delta(&["--unyank", "ay@9"]));
    assert!(err.contains("the head does not yank it"), "{err}");
    let err = refused(&b, &delta(&["--yank", "ghost@1"]));
    assert!(err.contains("which this index does not pin"), "{err}");
    let (p, _) = ok(&carried, &delta(&["--yank", "codex@2.1.279"]));
    assert_eq!(p.channel.yanked, ["ay@1", "codex@2.1.279"]);
    // A version yank in a grammar its program's client never holds matches nothing.
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
    // A vendor-direct program's yank outlives its pin.
    let vendor = Base {
        channel: Channel {
            yanked: p.channel.yanked.clone(),
            ..b.channel.clone()
        },
        ..b.clone()
    };
    let (p, _) = ok(&vendor, &delta(&["--retire", "codex"]));
    assert_eq!(p.channel.yanked, ["ay@1", "codex@2.1.279"]);

    // The runbook's spelling normalizes; a yank of the PINNED build is said, not refused.
    assert_eq!(
        parse_yank("ay=1", true).expect("="),
        Yank::Build("ay".into(), 1)
    );
    let (_, quiet) = ok(&b, &delta(&["--yank", "ay=1"]));
    assert!(!quiet.contains("TOMBSTONES"));
    let (_, loud) = ok(&b, &delta(&["--yank", "ay@3"]));
    assert!(
        loud.contains("yanked ay@3 names the build this index PINS for ay"),
        "{}",
        loud.text()
    );
    assert!(loud.contains("TOMBSTONES"));
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
