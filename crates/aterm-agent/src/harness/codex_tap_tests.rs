// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the Codex rollout tap's build gate, incremental reads, file replacement, and history floor/watermark.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use super::*;
use crate::harness::codex_rollout::PAGINATED;
use crate::harness::codex_rollout::fixtures::{count, meta_as, stamp};
use crate::harness::codex_steps::CODEX_MEASURED_BUILDS;
use crate::harness::footer::FOLD_BUDGET;
use crate::harness::steps::{StepEvent, StepId, StepState};
use aterm_phase::anchors::rollout_reduction_stem;

const THREAD: &str = "019a0000-0000-7000-8000-00000000000a";

const DAY: &str = "sessions/2025/10/19";
const FAR_DAY: &str = "sessions/2025/10/18";

const ALAB: &str = "0.164.0-dev.12521+g3d3eb12d8";

const ALAB_MANAGED: &str = "0.164.0-dev.12479+g07a25d86a";
const UNMEASURED: &str = "0.164.0-dev.99999+g0123abcd";

const T0: i64 = 1_790_000_000_000;

fn scratch_home(name: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix(&format!("codex-tap-{name}-"))
        .tempdir()
        .expect("temp dir")
}

fn rollout_in(home: &Path, day: &str, thread: &str) -> PathBuf {
    // Joined name by name, so the path is spelled as `rollout_path` spells it
    // (its fingerprint is the watermark's) on Windows too.
    let dir = day
        .split('/')
        .fold(home.to_path_buf(), |dir, name| dir.join(name));
    std::fs::create_dir_all(&dir).expect("day folder");
    dir.join(format!("rollout-2025-10-19T22-03-11-{thread}.jsonl"))
}

fn append(path: &Path, text: &str) {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .expect("append");
}

fn opening(id: &str, build: &str) -> String {
    meta_as(T0, id, id, None, Some(PAGINATED), None).replace(
        "\"cli_version\":\"0.163.0\"",
        &format!("\"cli_version\":\"{build}\""),
    )
}

fn ctx(ms: i64, id: &str) -> String {
    format!(
        "{{\"timestamp\":\"{}\",\"type\":\"turn_context\",\"payload\":{{\"turn_id\":\"{id}\",\
         \"root_turn_id\":\"{id}\",\"cwd\":\"/stand-in/project\",\"approval_policy\":\
         \"on-request\",\"model\":\"gpt-5.5\",\"summary\":\"auto\"}}}}\n",
        stamp(ms)
    )
}

fn done(ms: i64, id: &str, dur: u64) -> String {
    format!(
        "{{\"timestamp\":\"{}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"task_complete\",\
         \"turn_id\":\"{id}\",\"last_agent_message\":\"SENTINEL-REPLY\",\"duration_ms\":{dur}}}}}\n",
        stamp(ms)
    )
}

fn call(ms: i64, id: &str) -> String {
    format!(
        "{{\"timestamp\":\"{}\",\"type\":\"response_item\",\"payload\":{{\"type\":\
         \"function_call\",\"name\":\"exec_command\",\"arguments\":\
         \"{{\\\"cmd\\\":\\\"SENTINEL-INPUT\\\"}}\",\"call_id\":\"{id}\"}}}}\n\
         {{\"timestamp\":\"{}\",\"type\":\"response_item\",\"payload\":{{\"type\":\
         \"function_call_output\",\"call_id\":\"{id}\",\"output\":\"SENTINEL-OUTPUT\"}}}}\n",
        stamp(ms),
        stamp(ms + 1)
    )
}

fn said(tap: &mut RolloutTap) -> Vec<(String, StepState)> {
    tap.take()
        .steps
        .iter()
        .map(|s| {
            let w = match &s.ev {
                StepEvent::Bind { len } => format!("bind {len}"),
                StepEvent::Prompt { vref } => format!("prompt {}", vref.as_str()),
                StepEvent::ToolUse { id, tool, .. } => {
                    format!("call {} {}", id.as_str(), tool.as_str())
                }
                StepEvent::ToolResult { id, .. } => format!("result {}", id.as_str()),
                StepEvent::TurnEnd {
                    vdur_ms,
                    tok_in,
                    tok_out,
                } => format!("end {vdur_ms} {tok_in} {tok_out}"),
                StepEvent::Model { model, ver, .. } => format!(
                    "model {} {}",
                    model.as_str(),
                    ver.as_ref().map_or("-", StepId::as_str)
                ),
                other => format!("{other:?}"),
            };
            (w, s.state)
        })
        .collect()
}

fn words(got: &[(String, StepState)]) -> Vec<&str> {
    got.iter().map(|(w, _)| w.as_str()).collect()
}

#[test]
fn a_measured_codex_alab_thread_binds_at_its_build_and_is_read_on() {
    let home = scratch_home("read-on");
    let path = rollout_in(home.path(), DAY, THREAD);
    append(
        &path,
        &[opening(THREAD, ALAB), ctx(T0 + 1000, "turn-1")].concat(),
    );
    let mut tap = RolloutTap::new(THREAD, home.path()).expect("a thread id");
    tap.arm(0, None, Some(ALAB));
    assert!(tap.read(FOLD_BUDGET).is_some_and(|r| r.complete));
    assert!(tap.is_bound() && tap.refused().is_none());
    let got = said(&mut tap);
    assert_eq!(
        words(&got),
        ["bind 0", "prompt turn-1", "model gpt-5.5 0.164.0-dev.12521"],
        "{got:?}"
    );
    assert!(got.iter().all(|(_, s)| *s == StepState::On), "{got:?}");
    let torn = ctx(T0 + 5000, "turn-2");
    append(
        &path,
        &[
            call(T0 + 1500, "call_r0010"),
            count(T0 + 2000, (100, 0, 10), (100, 0, 10)),
            done(T0 + 3000, "turn-1", 2000),
            torn[..torn.len() / 2].to_string(),
        ]
        .concat(),
    );
    let _ = tap.read(FOLD_BUDGET);
    assert_eq!(
        words(&said(&mut tap)),
        [
            "call call_r0010 exec_command",
            "result call_r0010",
            "end 2000 100 10"
        ]
    );
    append(&path, &torn[torn.len() / 2..]);
    let _ = tap.read(FOLD_BUDGET);
    assert_eq!(words(&said(&mut tap)), ["prompt turn-2"]);
    let _ = tap.read(FOLD_BUDGET);
    assert!(said(&mut tap).is_empty(), "nothing is queued twice");
    let held = std::fs::read_to_string(&path).expect("the rollout");
    let swap = path.with_extension("swap");
    std::fs::write(&swap, &held).expect("the replacement");
    std::fs::rename(&swap, &path).expect("renamed over it");
    let _ = tap.read(FOLD_BUDGET);
    assert_eq!(
        words(&said(&mut tap)),
        [format!("bind {}", held.len())],
        "a replaced file is history up to its length"
    );
    append(&path, &done(T0 + 6000, "turn-2", 900));
    let _ = tap.read(FOLD_BUDGET);
    assert_eq!(words(&said(&mut tap)), ["end 900 0 0"]);
}

/// A BUILD NOBODY MEASURED IS READ PARTIAL, AN UNKNOWN META BINDS NOTHING: a thread whose
/// `session_meta` names a build `CODEX_MEASURED_BUILDS` does not hold (a vendor Codex's release,
/// a count nobody measured, a measured build's release matched short of whole) is bound and read,
/// every step [`StepState::Partial`]; one whose `session_meta` names no build the log can say, or
/// a `history_mode` the fold does not know, is refused `unknown-meta`, for the tap's life.
/// NEGATIVE CONTROL: the same rows with a measured build, or a `history_mode` the fold knows (or
/// none), bind; the measured one reads `On`.
#[test]
fn a_build_nobody_measured_is_read_partial_and_an_unknown_meta_binds_nothing() {
    let tap_on = |meta: String| {
        let home = scratch_home("gate");
        let path = rollout_in(home.path(), DAY, THREAD);
        append(&path, &[meta, ctx(T0 + 1000, "turn-1")].concat());
        let mut tap = RolloutTap::new(THREAD, home.path()).expect("a thread id");
        tap.arm(0, None, Some(ALAB));
        let read = tap.read(FOLD_BUDGET);
        let mut got = said(&mut tap);
        let at = tap.reads_at();
        append(&path, &done(T0 + 3000, "turn-1", 5));
        let again = tap.read(FOLD_BUDGET);
        got.extend(said(&mut tap));
        (read.is_some() && again.is_some(), tap.refused(), at, got)
    };
    let (read, refused, at, got) = tap_on(opening(THREAD, ALAB));
    assert!(read && refused.is_none(), "{refused:?}");
    assert_eq!(at, Some(StepState::On));
    assert_eq!(
        words(&got),
        [
            "bind 0",
            "prompt turn-1",
            "model gpt-5.5 0.164.0-dev.12521",
            "end 5 0 0"
        ]
    );
    assert!(got.iter().all(|(_, s)| *s == StepState::On), "{got:?}");
    for build in [
        "0.163.0",
        "0.164.0-dev.99999+g0123abcd",
        "0.164.0-dev.12521",
    ] {
        let (read, refused, at, got) = tap_on(opening(THREAD, build));
        assert!(read && refused.is_none(), "{build}: {refused:?}");
        assert_eq!(at, Some(StepState::Partial), "{build}");
        assert_eq!(
            words(&got),
            [
                "bind 0",
                "prompt turn-1",
                "model gpt-5.5 0.164.0-dev.12521",
                "end 5 0 0"
            ],
            "{build}: the model is said at the writer's build"
        );
        assert!(
            got.iter().all(|(_, s)| *s == StepState::Partial),
            "{build}: {got:?}"
        );
    }
    let measured = opening(THREAD, ALAB);
    let cli = format!("\"cli_version\":\"{ALAB}\",");
    assert!(measured.contains(&cli), "the plant's anchor");
    for (what, meta) in [
        ("no build", measured.replacen(&cli, "", 1)),
        (
            "a build that is no string",
            measured.replacen(&cli, "\"cli_version\":164,", 1),
        ),
        (
            "a build the log cannot say",
            measured.replacen(&cli, "\"cli_version\":\"0 164+x\",", 1),
        ),
        (
            "a history mode the fold does not know",
            measured.replacen(PAGINATED, "segmented", 1),
        ),
    ] {
        assert_ne!(meta, measured, "{what}: the plant took");
        let (read, refused, at, got) = tap_on(meta);
        assert!(!read, "{what}");
        assert_eq!(refused, Some(Refusal::UnknownMeta), "{what}");
        assert!(Refusal::UnknownMeta.is_final(), "{what}");
        assert_eq!(at, None, "{what}");
        assert!(got.is_empty(), "{what}: {got:?}");
    }
    for (what, meta) in [
        ("legacy", measured.replacen(PAGINATED, "legacy", 1)),
        (
            "none",
            measured.replacen(&format!(",\"history_mode\":\"{PAGINATED}\""), "", 1),
        ),
    ] {
        assert_ne!(meta, measured, "{what}: the plant took");
        let (read, refused, at, got) = tap_on(meta);
        assert!(
            read && refused.is_none(),
            "NEGATIVE CONTROL {what}: {refused:?}"
        );
        assert_eq!(at, Some(StepState::On), "NEGATIVE CONTROL {what}");
        assert_eq!(got.len(), 4, "NEGATIVE CONTROL {what}: {got:?}");
    }
    assert_eq!(Refusal::UnknownMeta.word(), "unknown-meta");
    assert_eq!(
        Refusal::ALL.iter().map(|r| r.word()).collect::<Vec<_>>(),
        ["no-rollout", "no-meta", "other-thread", "unknown-meta"]
    );
}

/// AN UNMEASURED BUILD'S ROLLOUT IS READ PARTIAL, step for step: the newest measured build's
/// committed `approval` leg, relabelled to a count nobody measured (its `cli_version` alone moved)
/// and written as the thread's rollout, folds through a tap whose claimant runs that build to the
/// very steps the leg folds to on its own build (turns, durations, tokens, the call and its
/// result), each [`StepState::Partial`] and the model said at the relabelled build.
/// NEGATIVE CONTROL: the leg as committed, its claimant on its own build, reads them `On`.
#[test]
fn an_unmeasured_builds_rollout_is_read_partial() {
    let newest = *CODEX_MEASURED_BUILDS.last().expect("a measured build");
    let leg = format!(
        "{}/src/harness/testdata/steps/{}-approval.jsonl",
        env!("CARGO_MANIFEST_DIR"),
        rollout_reduction_stem(newest)
    );
    let committed = std::fs::read_to_string(&leg).unwrap_or_else(|e| panic!("{leg}: {e}"));
    let own: String = committed
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| format!("{l}\n"))
        .collect::<String>()
        .replace("00000000-0000-4000-8000-000000000001", THREAD);
    let (release, _) = newest.split_once('+').expect("a dev build");
    let (stem, _) = release.rsplit_once('.').expect("its count");
    let unmeasured = format!("{stem}.99999+g0123abcd");
    assert!(!CODEX_MEASURED_BUILDS.contains(&unmeasured.as_str()));
    let relabelled = own.replace(
        &format!("\"cli_version\":\"{newest}\""),
        &format!("\"cli_version\":\"{unmeasured}\""),
    );
    assert_ne!(relabelled, own, "the relabel took");
    let read_as = |text: &str, running: &str| {
        let home = scratch_home("relabelled");
        append(&rollout_in(home.path(), DAY, THREAD), text);
        let mut tap = RolloutTap::new(THREAD, home.path()).expect("a thread id");
        tap.arm(0, None, Some(running));
        assert!(
            tap.read(FOLD_BUDGET).is_some_and(|r| r.complete),
            "{running}"
        );
        assert!(tap.is_bound() && tap.refused().is_none(), "{running}");
        let at = tap.reads_at();
        let batch = tap.take();
        assert_eq!((batch.lost, batch.overflow), (0, 0), "{running}");
        (at, batch.steps)
    };
    let (on_at, on) = read_as(&own, newest);
    let (partial_at, partial) = read_as(&relabelled, &unmeasured);
    assert_eq!(
        (on_at, partial_at),
        (Some(StepState::On), Some(StepState::Partial))
    );
    let kinds = |steps: &[crate::harness::steps::Step]| {
        steps
            .iter()
            .map(|s| match s.ev {
                StepEvent::Model { model, effort, ver } => StepEvent::Model {
                    model,
                    effort,
                    ver: ver.map(|_| StepId::new("ver").expect("an id")),
                },
                ev => ev,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(kinds(&partial), kinds(&on), "the very steps");
    let read: [fn(&StepEvent) -> bool; 4] = [
        |e| matches!(e, StepEvent::Prompt { .. }),
        |e| matches!(e, StepEvent::TurnEnd { tok_in, .. } if *tok_in > 0),
        |e| matches!(e, StepEvent::ToolUse { .. }),
        |e| matches!(e, StepEvent::ToolResult { .. }),
    ];
    for ev in read {
        assert!(partial.iter().any(|s| ev(&s.ev)), "{partial:?}");
    }
    assert!(
        on.iter().all(|s| s.state == StepState::On),
        "NEGATIVE CONTROL: {on:?}"
    );
    assert!(
        partial.iter().all(|s| s.state == StepState::Partial),
        "{partial:?}"
    );
    let ver = |steps: &[crate::harness::steps::Step]| {
        steps
            .iter()
            .find_map(|s| match s.ev {
                StepEvent::Model { ver, .. } => ver.map(|v| v.as_str().to_string()),
                _ => None,
            })
            .expect("the model said")
    };
    assert_eq!(ver(&on), release);
    assert_eq!(ver(&partial), format!("{stem}.99999"));
}

#[test]
fn a_thread_binds_only_its_own_rollout_once_written_whole() {
    let home = scratch_home("own");
    let mut tap = RolloutTap::new(THREAD, home.path()).expect("a thread id");
    tap.arm(0, None, Some(ALAB));
    assert!(tap.read(FOLD_BUDGET).is_none());
    assert_eq!(tap.refused(), Some(Refusal::NoRollout));
    let path = rollout_in(home.path(), DAY, THREAD);
    let meta = opening(THREAD, ALAB);
    append(&path, &meta[..meta.len() - 1]);
    assert!(tap.read(FOLD_BUDGET).is_none());
    assert_eq!(tap.refused(), Some(Refusal::NoMeta));
    append(&path, "\n");
    assert!(tap.read(FOLD_BUDGET).is_some() && tap.is_bound());
    assert_eq!(words(&said(&mut tap)), ["bind 0"]);
    let other = scratch_home("other");
    let wrong = rollout_in(other.path(), DAY, THREAD);
    append(
        &wrong,
        &opening("019a0000-0000-7000-8000-00000000000b", ALAB),
    );
    let mut tap = RolloutTap::new(THREAD, other.path()).expect("a thread id");
    tap.arm(0, None, Some(ALAB));
    assert!(tap.read(FOLD_BUDGET).is_none());
    assert_eq!(tap.refused(), Some(Refusal::OtherThread));
    assert!(RolloutTap::new("not-a-thread", other.path()).is_none());
    assert_eq!(rollout_path(home.path(), THREAD), Some(path.clone()));
    let far = scratch_home("far");
    let far_path = rollout_in(far.path(), FAR_DAY, THREAD);
    append(&far_path, &opening(THREAD, ALAB));
    assert_eq!(rollout_path(far.path(), THREAD), None);
    assert_eq!(rollouts_by_write(far.path()).len(), 1, "NEGATIVE CONTROL");
}

#[test]
fn history_below_the_floor_or_the_watermark_is_never_queued() {
    let home = scratch_home("history");
    let path = rollout_in(home.path(), DAY, THREAD);
    let history = [
        opening(THREAD, ALAB),
        ctx(T0 + 1000, "turn-1"),
        done(T0 + 2000, "turn-1", 700),
    ]
    .concat();
    append(&path, &history);
    let mut tap = RolloutTap::new(THREAD, home.path()).expect("a thread id");
    tap.arm(u64::try_from(T0 + 2500).expect("a stamp"), None, Some(ALAB));
    let _ = tap.read(FOLD_BUDGET);
    assert_eq!(words(&said(&mut tap)), ["bind 0"]);
    append(&path, &ctx(T0 + 3000, "turn-2"));
    let _ = tap.read(FOLD_BUDGET);
    assert_eq!(
        words(&said(&mut tap)),
        ["prompt turn-2", "model gpt-5.5 0.164.0-dev.12521"]
    );
    let file = std::fs::File::open(&path).expect("the rollout");
    let id = crate::harness::session_usage::open_identity(
        &file,
        &file.metadata().expect("its metadata"),
    );
    let given = StepMark {
        fp: crate::harness::session_usage::path_fp(&path),
        id,
        off: history.len() as u64,
    };
    let mut tap = RolloutTap::new(THREAD, home.path()).expect("a thread id");
    tap.arm(0, Some(given), Some(ALAB));
    let _ = tap.read(FOLD_BUDGET);
    let got = said(&mut tap);
    assert_eq!(
        words(&got),
        [
            format!("bind {}", history.len()).as_str(),
            "prompt turn-2",
            "model gpt-5.5 0.164.0-dev.12521"
        ],
        "{got:?}"
    );
}

/// A codex-alab package of its own version line (codex-alab Decision 8, 2026-10-24) runs a
/// build whose manifest says `0.1.0-dev.<count>+g<sha9>` while its rollouts say the client
/// version: the tap is armed with what the package's marker names as the build
/// (`codex_alab::alab_build`, as `aterm-gui`'s `running_build` arms it) and binds.
#[test]
fn a_codex_alab_own_line_package_is_tapped_at_its_client_version() {
    use crate::harness::codex_alab::{ALAB_BUILD_JSON, PACKAGE_JSON, alab_build};
    let own = ALAB.replacen("0.164.0", "0.1.0", 1);
    let pkg = scratch_home("own-line-package");
    let root = pkg.path().join(&own);
    std::fs::create_dir_all(root.join("bin")).expect("bin");
    let exe = root.join("bin/codex");
    std::fs::write(&exe, "").expect("exe is never run");
    std::fs::write(
        root.join(PACKAGE_JSON),
        format!(r#"{{"layoutVersion":1,"version":"{own}","entrypoint":"bin/codex"}}"#),
    )
    .expect("manifest");
    std::fs::write(
        root.join(ALAB_BUILD_JSON),
        format!(r#"{{"source":"3d3eb12d8","version":"{own}","client_version":"{ALAB}"}}"#),
    )
    .expect("marker");
    let home = scratch_home("own-line");
    let path = rollout_in(home.path(), DAY, THREAD);
    append(
        &path,
        &[opening(THREAD, ALAB), ctx(T0 + 1000, "turn-1")].concat(),
    );
    let running = alab_build(&exe);
    let mut tap = RolloutTap::new(THREAD, home.path()).expect("a thread id");
    tap.arm(0, None, running.as_deref());
    assert!(
        tap.read(FOLD_BUDGET).is_some_and(|r| r.complete),
        "running {running:?}, refused {:?}",
        tap.refused()
    );
    assert!(tap.is_bound() && tap.refused().is_none());
    assert_eq!(
        words(&said(&mut tap)),
        ["bind 0", "prompt turn-1", "model gpt-5.5 0.164.0-dev.12521"]
    );
}

/// codex-alab's COMPOUND version (codex-alab Decision 8, amended 2026-10-24), in the forms of the
/// first build on its own line that the lab measured (aterm 51a670f35a): the manifest and the
/// marker's `version` say `0.1.0-dev.13665+codex.0.164.0.g1eb102688`, the marker's
/// `client_version` says `0.165.0-dev.13665+g1eb102688`, and the rollout's `cli_version` says
/// the client version. The
/// tap is armed with the client version and binds; a record whose client version names a
/// measured build the package does not run (another count and commit), or a compound package
/// that names no client version, arms it with no build, so its rows are read partial and the
/// model is said at no build.
#[test]
fn a_compound_package_is_tapped_at_its_client_version_and_never_at_a_build_it_does_not_run() {
    use crate::harness::codex_alab::{ALAB_BUILD_JSON, PACKAGE_JSON, alab_build};
    const CLIENT: &str = "0.165.0-dev.13665+g1eb102688";
    const OWN: &str = "0.1.0-dev.13665+codex.0.164.0.g1eb102688";
    assert!(CODEX_MEASURED_BUILDS.contains(&CLIENT) && CODEX_MEASURED_BUILDS.contains(&ALAB));
    let pkg = scratch_home("compound-package");
    let root = pkg.path().join(OWN);
    std::fs::create_dir_all(root.join("bin")).expect("bin");
    let exe = root.join("bin/codex");
    std::fs::write(&exe, "").expect("exe is never run");
    std::fs::write(
        root.join(PACKAGE_JSON),
        format!(r#"{{"layoutVersion":1,"version":"{OWN}","entrypoint":"bin/codex"}}"#),
    )
    .expect("manifest");
    let record = |client: Option<&str>| {
        let client = client.map_or_else(String::new, |c| format!(r#","client_version":"{c}""#));
        std::fs::write(
            root.join(ALAB_BUILD_JSON),
            format!(
                r#"{{"source":"1eb102688","version":"{OWN}"{client},"baseline":"0.164.0","upstream":{{"release":"rust-v0.164.0","commit":"{}"}}}}"#,
                "0".repeat(40)
            ),
        )
        .expect("marker");
    };
    let home = scratch_home("compound");
    let path = rollout_in(home.path(), DAY, THREAD);
    append(
        &path,
        &[opening(THREAD, CLIENT), ctx(T0 + 1000, "turn-1")].concat(),
    );
    record(Some(CLIENT));
    let running = alab_build(&exe);
    assert_eq!(running.as_deref(), Some(CLIENT), "the client version");
    let mut tap = RolloutTap::new(THREAD, home.path()).expect("a thread id");
    tap.arm(0, None, running.as_deref());
    assert!(
        tap.read(FOLD_BUDGET).is_some_and(|r| r.complete),
        "running {running:?}, refused {:?}",
        tap.refused()
    );
    assert!(tap.is_bound() && tap.refused().is_none());
    assert_eq!(
        words(&said(&mut tap)),
        ["bind 0", "prompt turn-1", "model gpt-5.5 0.165.0-dev.13665"]
    );
    let other = scratch_home("compound-other");
    let path = rollout_in(other.path(), DAY, THREAD);
    append(
        &path,
        &[opening(THREAD, ALAB), ctx(T0 + 1000, "turn-1")].concat(),
    );
    for client in [Some(ALAB), None, Some(OWN)] {
        record(client);
        let running = alab_build(&exe);
        assert_eq!(running, None, "{client:?}");
        let mut tap = RolloutTap::new(THREAD, other.path()).expect("a thread id");
        tap.arm(0, None, running.as_deref());
        assert!(tap.read(FOLD_BUDGET).is_some(), "{client:?}");
        assert!(tap.is_bound() && tap.refused().is_none(), "{client:?}");
        assert_eq!(tap.reads_at(), Some(StepState::Partial), "{client:?}");
        let got = said(&mut tap);
        assert_eq!(
            words(&got),
            ["bind 0", "prompt turn-1", "model gpt-5.5 -"],
            "{client:?}: a build the package does not run is never said"
        );
        assert!(
            got.iter().all(|(_, s)| *s == StepState::Partial),
            "{client:?}: {got:?}"
        );
    }
}

/// A CLAIMANT RUNNING A BUILD NOBODY MEASURED IS READ PARTIAL: a measured thread whose claimant's
/// image runs an unmeasured build (or one its reader names no build of) is bound and read, every
/// step `Partial` and the model said at the running build; a bound tap whose writer moves onto
/// such a build reads on at `Partial`, and back on a measured build at `On`, never refused.
/// NEGATIVE CONTROL: a claimant on a measured build reads the same rows `On`.
#[test]
fn a_claimant_running_an_unmeasured_build_is_read_partial() {
    let home = scratch_home("running");
    let path = rollout_in(home.path(), DAY, THREAD);
    append(
        &path,
        &[opening(THREAD, ALAB), ctx(T0 + 1000, "turn-1")].concat(),
    );
    for (running, ver) in [
        (Some(UNMEASURED), "0.164.0-dev.99999"),
        (Some("0.163.0"), "0.163.0"),
        (None, "-"),
    ] {
        let mut tap = RolloutTap::new(THREAD, home.path()).expect("a thread id");
        tap.arm(0, None, running);
        assert!(tap.read(FOLD_BUDGET).is_some(), "{running:?}");
        assert!(tap.is_bound() && tap.refused().is_none(), "{running:?}");
        assert_eq!(tap.reads_at(), Some(StepState::Partial), "{running:?}");
        let got = said(&mut tap);
        assert_eq!(
            words(&got),
            [
                "bind 0".to_string(),
                "prompt turn-1".into(),
                format!("model gpt-5.5 {ver}")
            ],
            "{running:?}"
        );
        assert!(
            got.iter().all(|(_, s)| *s == StepState::Partial),
            "{running:?}: {got:?}"
        );
    }
    let mut tap = RolloutTap::new(THREAD, home.path()).expect("a thread id");
    tap.arm(0, None, Some(ALAB_MANAGED));
    assert!(tap.read(FOLD_BUDGET).is_some());
    let got = said(&mut tap);
    assert_eq!(
        words(&got),
        ["bind 0", "prompt turn-1", "model gpt-5.5 0.164.0-dev.12479"]
    );
    assert!(
        got.iter().all(|(_, s)| *s == StepState::On),
        "NEGATIVE CONTROL: {got:?}"
    );
    append(&path, &call(T0 + 1500, "call_r0010"));
    tap.arm(0, None, Some(UNMEASURED));
    assert!(tap.read(FOLD_BUDGET).is_some());
    assert_eq!(tap.reads_at(), Some(StepState::Partial));
    let got = said(&mut tap);
    assert_eq!(
        words(&got),
        ["call call_r0010 exec_command", "result call_r0010"],
        "the writer moved: read on"
    );
    assert!(got.iter().all(|(_, s)| *s == StepState::Partial), "{got:?}");
    assert!(tap.is_bound() && tap.refused().is_none());
    append(&path, &done(T0 + 3000, "turn-1", 2000));
    tap.arm(0, None, Some(ALAB));
    assert!(tap.read(FOLD_BUDGET).is_some());
    assert_eq!(tap.reads_at(), Some(StepState::On));
    assert_eq!(
        said(&mut tap),
        [("end 2000 0 0".to_string(), StepState::On)],
        "back on a measured build: On"
    );
}

#[test]
fn a_file_replaced_at_the_watermarks_path_is_history_to_any_tap() {
    let home = scratch_home("replaced");
    let path = rollout_in(home.path(), DAY, THREAD);
    append(
        &path,
        &[opening(THREAD, ALAB), ctx(T0 + 1000, "turn-1")].concat(),
    );
    let mut tap = RolloutTap::new(THREAD, home.path()).expect("a thread id");
    tap.arm(0, None, Some(ALAB));
    let _ = tap.read(FOLD_BUDGET);
    assert_eq!(
        words(&said(&mut tap)),
        ["bind 0", "prompt turn-1", "model gpt-5.5 0.164.0-dev.12521"]
    );
    let file = std::fs::File::open(&path).expect("the rollout");
    let id = crate::harness::session_usage::open_identity(
        &file,
        &file.metadata().expect("its metadata"),
    );
    drop(file);
    let held = std::fs::read_to_string(&path).expect("the rollout");
    let given = StepMark {
        fp: crate::harness::session_usage::path_fp(&path),
        id,
        off: held.len() as u64,
    };
    let swap = path.with_extension("swap");
    std::fs::write(&swap, &held).expect("the replacement");
    std::fs::rename(&swap, &path).expect("renamed over it");
    let mut fresh = RolloutTap::new(THREAD, home.path()).expect("a thread id");
    fresh.arm(0, Some(given), Some(ALAB));
    let _ = fresh.read(FOLD_BUDGET);
    assert_eq!(
        words(&said(&mut fresh)),
        [format!("bind {}", held.len())],
        "S1: history up to its length"
    );
    append(&path, &done(T0 + 3000, "turn-1", 900));
    let _ = fresh.read(FOLD_BUDGET);
    assert_eq!(words(&said(&mut fresh)), ["end 900 0 0"]);
    let elsewhere = StepMark {
        fp: given.fp ^ 1,
        ..given
    };
    let mut other = RolloutTap::new(THREAD, home.path()).expect("a thread id");
    other.arm(0, Some(elsewhere), Some(ALAB));
    let _ = other.read(FOLD_BUDGET);
    assert_eq!(
        words(&said(&mut other)),
        [
            "bind 0",
            "prompt turn-1",
            "model gpt-5.5 0.164.0-dev.12521",
            "end 900 0 0"
        ]
    );
}

#[test]
fn a_line_longer_than_the_cap_is_skipped_once() {
    let home = scratch_home("long");
    let path = rollout_in(home.path(), DAY, THREAD);
    let meta = opening(THREAD, ALAB);
    let cap = (meta.len() as u64) * 2;
    let long = format!(
        "{{\"timestamp\":\"{}\",\"type\":\"response_item\",\"payload\":{{\"type\":\
         \"message\",\"x\":\"{}\"}}}}\n",
        stamp(T0 + 500),
        "a".repeat(usize::try_from(cap).expect("a cap") * 3)
    );
    append(&path, &[meta, long, ctx(T0 + 1000, "turn-1")].concat());
    let mut tap = RolloutTap::new(THREAD, home.path()).expect("a thread id");
    tap.arm(0, None, Some(ALAB));
    let mut got = Vec::new();
    let mut lost = 0;
    for _ in 0..8 {
        let _ = tap.read(cap);
        let batch = tap.take();
        lost += batch.lost;
        got.extend(batch.steps.into_iter().map(|s| s.ev));
    }
    assert!(
        got.iter()
            .any(|ev| matches!(ev, StepEvent::Prompt { vref } if vref.as_str() == "turn-1")),
        "{got:?}"
    );
    assert_eq!(lost, 1, "the long line is counted lost once");
}
