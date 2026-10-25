// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the Codex rollout tap's build gate, incremental reads, file replacement, and history floor/watermark.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use super::*;
use crate::harness::codex_rollout::PAGINATED;
use crate::harness::codex_rollout::fixtures::{count, meta_as, stamp};
use crate::harness::footer::FOLD_BUDGET;
use crate::harness::steps::{StepEvent, StepId, StepState};

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

#[test]
fn a_vendor_codex_and_an_unmeasured_build_bind_nothing() {
    let tap_on = |build: &str| {
        let home = scratch_home("gate");
        let path = rollout_in(home.path(), DAY, THREAD);
        append(
            &path,
            &[opening(THREAD, build), ctx(T0 + 1000, "turn-1")].concat(),
        );
        let mut tap = RolloutTap::new(THREAD, home.path()).expect("a thread id");
        tap.arm(0, None, Some(ALAB));
        let read = tap.read(FOLD_BUDGET);
        let got = said(&mut tap);
        append(&path, &done(T0 + 3000, "turn-1", 5));
        let again = tap.read(FOLD_BUDGET);
        (
            read.is_some(),
            tap.is_bound(),
            tap.refused(),
            got,
            again.is_some(),
        )
    };
    let (read, bound, refused, got, _) = tap_on(ALAB);
    assert!(
        read && bound && refused.is_none() && got.len() == 3,
        "{got:?}"
    );
    for build in [
        "0.163.0",
        "0.164.0-dev.99999+g0123abcd",
        "0.164.0-dev.12521",
    ] {
        let (read, bound, refused, got, again) = tap_on(build);
        assert!(!read && !bound && !again, "{build}");
        assert_eq!(refused, Some(Refusal::Unmeasured), "{build}");
        assert!(got.is_empty(), "{build}: {got:?}");
    }
    assert_eq!(Refusal::Unmeasured.word(), "unmeasured");
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

#[test]
fn a_claimant_running_an_unmeasured_build_binds_nothing() {
    let home = scratch_home("running");
    let path = rollout_in(home.path(), DAY, THREAD);
    append(
        &path,
        &[opening(THREAD, ALAB), ctx(T0 + 1000, "turn-1")].concat(),
    );
    for running in [Some(UNMEASURED), Some("0.163.0"), None] {
        let mut tap = RolloutTap::new(THREAD, home.path()).expect("a thread id");
        tap.arm(0, None, running);
        assert!(tap.read(FOLD_BUDGET).is_none(), "{running:?}");
        assert!(!tap.is_bound(), "{running:?}");
        assert_eq!(tap.refused(), Some(Refusal::Unmeasured), "{running:?}");
        assert!(said(&mut tap).is_empty(), "{running:?}");
        tap.arm(0, None, Some(ALAB));
        assert!(tap.read(FOLD_BUDGET).is_none(), "{running:?}");
    }
    let mut tap = RolloutTap::new(THREAD, home.path()).expect("a thread id");
    tap.arm(0, None, Some(ALAB_MANAGED));
    assert!(tap.read(FOLD_BUDGET).is_some());
    let got = said(&mut tap);
    assert_eq!(
        words(&got),
        ["bind 0", "prompt turn-1", "model gpt-5.5 0.164.0-dev.12479"]
    );
    assert!(got.iter().all(|(_, s)| *s == StepState::On), "{got:?}");
    append(&path, &call(T0 + 1500, "call_r0010"));
    tap.arm(0, None, Some(UNMEASURED));
    assert!(tap.read(FOLD_BUDGET).is_none());
    assert_eq!(
        words(&said(&mut tap)),
        ["Off { why: \"unmeasured\" }"],
        "the source says off once"
    );
    assert!(!tap.is_bound());
    assert_eq!(tap.refused(), Some(Refusal::Unmeasured));
    append(&path, &done(T0 + 3000, "turn-1", 2000));
    tap.arm(0, None, Some(ALAB));
    assert!(tap.read(FOLD_BUDGET).is_none());
    assert!(said(&mut tap).is_empty(), "read no more");
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
