// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The cut's PAINT SMOKE: policy, fail-closed verdict, escape discipline — and
//! THE PIPELINE LINES THAT WIRE THEM.
//!
//! Born from the 2026-08-24 blackout audit (docs/RELEASE-PROOF-DISCIPLINE.md):
//! v0.48.0 and v0.49.0 shipped the rainbow cursor trail dark while every gate
//! and the self-check were green, because the self-check proves signatures and
//! versions, never a pixel. The smoke closes that: a typed line against the
//! JUST-BUILT bundle, headless, through its own control socket, pixels
//! asserted — before any signing verdict is pronounced and before any
//! publish-facing step runs.
//!
//! Everything here runs WITHOUT launching a GUI and without an Apple account:
//! the probe sits behind [`publish::PaintProbe`] and the signing gate behind
//! [`sign::AppleTools`] (the `tests/apple_tier.rs` discipline), so these tests
//! drive the real decision code — `publish::selfcheck_paint_then_signing` and
//! `publish::paint_smoke_policy` — with recording fakes and assert what it
//! DID, in what ORDER. Each wiring test names the mutation it kills; each
//! killed mutation is one more way the next dark release cannot ship.

// The release crate is a binary on purpose (the spec's §9 file plan has no
// lib.rs), so the integration tests compile the modules under test directly —
// the same mount list tests/apple_tier.rs and the model tests carry.
#[path = "../src/apple.rs"]
#[allow(dead_code)]
mod apple;
#[path = "../src/buildplan.rs"]
#[allow(dead_code)]
mod buildplan;
#[path = "../src/bundle.rs"]
#[allow(dead_code)]
mod bundle;
#[path = "../src/changelog.rs"]
#[allow(dead_code)]
mod changelog;
#[path = "../src/cli.rs"]
#[allow(dead_code)]
mod cli;
#[path = "../src/dmg.rs"]
#[allow(dead_code)]
mod dmg;
#[path = "../src/gates.rs"]
#[allow(dead_code)]
mod gates;
#[path = "../src/ledger.rs"]
#[allow(dead_code)]
mod ledger;
#[path = "../src/machines.rs"]
#[allow(dead_code)]
mod machines;
#[path = "../src/manifest_out.rs"]
#[allow(dead_code)]
mod manifest_out;
#[path = "../src/mirror.rs"]
#[allow(dead_code)]
mod mirror;
#[path = "../src/provision.rs"]
#[allow(dead_code)]
mod provision;
#[path = "../src/publish.rs"]
#[allow(dead_code)]
mod publish;
#[path = "../src/sign.rs"]
#[allow(dead_code)]
mod sign;
#[path = "../src/verify.rs"]
#[allow(dead_code)]
mod verify;

use publish::CutKind;
use sign::{AppleTools, DevIdIdentity, GatekeeperKind, NotaryAuth};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

// --- the recording fakes -----------------------------------------------------

/// The ordered transcript of everything the fakes were asked to do, shared
/// across BOTH seams — the property under test is the order ACROSS them: the
/// bundle must be seen to paint before the signing gate spends a spawn.
type Log = Rc<RefCell<Vec<String>>>;

fn log() -> Log {
    Rc::new(RefCell::new(Vec::new()))
}

fn entries(log: &Log) -> Vec<String> {
    log.borrow().clone()
}

fn record(log: &Log, what: &str, target: &Path) {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    log.borrow_mut().push(format!("{what}:{name}"));
}

/// Records the paint probe call and answers a scripted verdict.
struct FakePaintProbe {
    log: Log,
    verdict: Result<&'static str, &'static str>,
}

impl publish::PaintProbe for FakePaintProbe {
    fn paint(&self, bundle_binary: &Path) -> Result<String, String> {
        record(&self.log, "paint", bundle_binary);
        self.verdict.map(str::to_string).map_err(str::to_string)
    }
}

/// A healthy Apple-side machine that records every spawn. With an EMPTY team — a fork's
/// tier, and this tree's before `APPLE_TEAM_ID` was armed on 2026-08-15, which is what
/// this doc used to call "the tier every cut ships today" — `selfcheck_signing` runs
/// exactly one of these: `verify_strict`.
struct RecordingTools {
    log: Log,
}

impl AppleTools for RecordingTools {
    fn sign_dmg(&self, dmg: &Path, _id: &DevIdIdentity) -> Result<(), String> {
        record(&self.log, "sign_dmg", dmg);
        Ok(())
    }
    fn check_devid_signed(&self, target: &Path) -> Result<(), String> {
        record(&self.log, "preflight", target);
        Ok(())
    }
    fn notarize(&self, artifact: &Path, _auth: &NotaryAuth) -> Result<(), String> {
        record(&self.log, "notarize", artifact);
        Ok(())
    }
    fn codesign_verify_strict(&self, target: &Path) -> Result<(), String> {
        record(&self.log, "verify_strict", target);
        Ok(())
    }
    fn codesign_dv(&self, target: &Path) -> Result<String, String> {
        record(&self.log, "dv", target);
        Ok(String::new())
    }
    fn stapler_validate(&self, target: &Path) -> Result<bool, String> {
        record(&self.log, "stapled?", target);
        Ok(true)
    }
    fn gatekeeper_ok(&self, target: &Path, _kind: GatekeeperKind) -> Result<bool, String> {
        record(&self.log, "gatekeeper?", target);
        Ok(true)
    }
}

fn app() -> PathBuf {
    PathBuf::from("/cut/dist/cut-app/aterm.app")
}

fn dmg_path() -> PathBuf {
    PathBuf::from("/cut/dist/aterm-0.50.0.dmg")
}

// --- the ordered wiring ------------------------------------------------------

/// KILLS: reordering the smoke after the signing gate (or `if false`-ing it).
/// The smoke judges the bundle's own binary FIRST; only then does the signing
/// gate spend its codesign spawn — and the transcript words come back from the
/// probe's own report, so the claim carries its evidence.
#[test]
fn the_paint_smoke_runs_before_the_signing_gate() {
    let log = log();
    let (paint_note, apple_note) = publish::selfcheck_paint_then_signing(
        CutKind::Real,
        "",
        &app(),
        &dmg_path(),
        false,
        None,
        &FakePaintProbe {
            log: Rc::clone(&log),
            verdict: Ok("PAINT shape=fake-claude verdict=PASS"),
        },
        &RecordingTools {
            log: Rc::clone(&log),
        },
    )
    .expect("a painting bundle self-checks clean");
    assert_eq!(
        entries(&log),
        vec![
            // The probe receives the BUNDLE's binary, not some workspace build.
            "paint:aterm".to_string(),
            "verify_strict:aterm.app".to_string(),
        ],
        "the paint smoke must run against the bundle binary BEFORE the signing gate"
    );
    assert!(
        paint_note.contains("PAINT shape=fake-claude verdict=PASS"),
        "the transcript's paint line must carry the probe's own report: {paint_note:?}"
    );
    assert_eq!(apple_note, "", "an empty team claims no Tier APPLE suffix");
}

/// KILLS: downgrading a paint failure to a warning. A bundle that does not
/// paint REFUSES the cut, with the message naming this exact failure class —
/// and it dies before a single Apple tool is spawned, so a dark artifact never
/// even reaches the signing verdict.
#[test]
fn a_dark_bundle_refuses_the_cut_before_any_apple_tool_runs() {
    let log = log();
    let err = publish::selfcheck_paint_then_signing(
        CutKind::Real,
        "",
        &app(),
        &dmg_path(),
        false,
        None,
        &FakePaintProbe {
            log: Rc::clone(&log),
            verdict: Err("PAINT shape=fake-claude total_ink=0 verdict=FAIL"),
        },
        &RecordingTools {
            log: Rc::clone(&log),
        },
    )
    .expect_err("a dark bundle must refuse the cut");
    let msg = err.to_string();
    assert!(
        msg.contains("the shipped artifact does not paint its flagship effect")
            && msg.contains("docs/RELEASE-PROOF-DISCIPLINE.md"),
        "the refusal must name the blackout failure class and the audit doc: {msg}"
    );
    assert!(
        msg.contains("total_ink=0"),
        "the refusal must carry the probe's measurement: {msg}"
    );
    assert_eq!(
        entries(&log),
        vec!["paint:aterm".to_string()],
        "a paint failure must abort before any Apple tool runs"
    );
}

/// A probe that COULD NOT RUN is not a pass — "we could not tell" refuses the
/// cut exactly like "it did not paint", under the same failure-class words.
/// This is the vacuity the audit found (proofs that measured nothing reading
/// as green), pinned shut at the cut.
#[test]
fn a_probe_that_could_not_run_is_not_a_pass() {
    let log = log();
    let err = publish::selfcheck_paint_then_signing(
        CutKind::DryRun,
        "",
        &app(),
        &dmg_path(),
        false,
        None,
        &FakePaintProbe {
            log: Rc::clone(&log),
            verdict: Err("PAINT-COULD-NOT-RUN control socket never appeared"),
        },
        &RecordingTools {
            log: Rc::clone(&log),
        },
    )
    .expect_err("an unproven bundle must refuse the cut");
    assert!(
        err.to_string()
            .contains("does not paint its flagship effect"),
        "could-not-run refuses under the same fail-closed class: {err}"
    );
}

/// An acknowledged skip is LOUD, and it still reaches the signing gate: the
/// probe is never called, the transcript says NO pixel proof shipped, and the
/// codesign gate runs exactly as before the smoke existed.
#[test]
fn an_acknowledged_skip_is_loud_and_still_reaches_the_signing_gate() {
    let log = log();
    let (paint_note, _) = publish::selfcheck_paint_then_signing(
        CutKind::DryRun,
        "",
        &app(),
        &dmg_path(),
        true,
        None,
        &FakePaintProbe {
            log: Rc::clone(&log),
            verdict: Ok("must never be consulted"),
        },
        &RecordingTools {
            log: Rc::clone(&log),
        },
    )
    .expect("a dry-run may skip the smoke without an ack");
    assert_eq!(
        entries(&log),
        vec!["verify_strict:aterm.app".to_string()],
        "a skipped smoke must not touch the probe, and must still run the signing gate"
    );
    assert!(
        paint_note.contains("SKIPPED") && paint_note.contains("NO pixel proof"),
        "the skip must be printed in its own words: {paint_note:?}"
    );
}

// --- the escape policy -------------------------------------------------------

/// The default: no flag, the smoke runs — on every cut kind, notarized or not.
#[test]
fn without_the_flag_the_smoke_runs_everywhere() {
    for kind in [CutKind::Real, CutKind::DryRun, CutKind::Rehearse] {
        for notarized in [false, true] {
            assert_eq!(
                publish::paint_smoke_policy(kind, notarized, false, None)
                    .expect("no flag is never an error"),
                None,
                "{kind:?}/notarized={notarized}: without --no-paint-smoke the smoke must run"
            );
        }
    }
}

/// KILLS: quietly honoring `--no-paint-smoke` on the artifact tier that ships
/// to the whole fleet. A notarized REAL cut refuses the flag without the exact
/// env acknowledgement — and the refusal tells the operator its spelling.
#[test]
fn the_skip_is_refused_on_a_notarized_real_cut_without_the_exact_ack() {
    for ack in [None, Some("1"), Some("yes"), Some("THIS-CUT-MAY-SHIP-DARK")] {
        let err = publish::paint_smoke_policy(CutKind::Real, true, true, ack)
            .expect_err("a notarized real cut must refuse an unacknowledged skip");
        let msg = err.to_string();
        assert!(
            msg.contains(publish::NO_PAINT_SMOKE_ACK_VAR)
                && msg.contains(publish::NO_PAINT_SMOKE_ACK_VALUE),
            "the refusal must name the exact acknowledgement ({ack:?}): {msg}"
        );
    }
}

/// The one spelling that opens the emergency lane — and even then the
/// transcript words say what is being shipped without proof.
#[test]
fn the_exact_ack_opens_the_emergency_lane_loudly() {
    let words = publish::paint_smoke_policy(
        CutKind::Real,
        true,
        true,
        Some(publish::NO_PAINT_SMOKE_ACK_VALUE),
    )
    .expect("the exact ack must be honored")
    .expect("the ack yields a skip, not a run");
    assert!(
        words.contains("NO pixel proof"),
        "an acknowledged skip must still say what it costs: {words:?}"
    );
}

/// Where nothing notarized ships to a fleet — dry-runs, rehearsals, ad-hoc
/// real cuts — the flag works without the ack (still loudly).
#[test]
fn unnotarized_and_rehearsal_cuts_skip_without_the_ack() {
    for (kind, notarized) in [
        (CutKind::Real, false),
        (CutKind::DryRun, true),
        (CutKind::DryRun, false),
        (CutKind::Rehearse, true),
        (CutKind::Rehearse, false),
    ] {
        let words = publish::paint_smoke_policy(kind, notarized, true, None)
            .unwrap_or_else(|e| panic!("{kind:?}/notarized={notarized} must allow the skip: {e}"))
            .expect("the skip yields words, not a run");
        assert!(
            words.contains("SKIPPED"),
            "{kind:?}: the skip must be loud: {words:?}"
        );
    }
}

// --- pipeline placement ------------------------------------------------------

/// The smoke lives in "selfcheck", and "selfcheck" precedes every
/// publish-facing step — so the typed line is spent against the
/// just-built bundle BEFORE anything is drafted, uploaded, tagged or flipped.
/// (Within the step, ordering against the signing gate is pinned by
/// `the_paint_smoke_runs_before_the_signing_gate` above.)
#[test]
fn the_selfcheck_owning_the_smoke_precedes_every_publish_step() {
    let pos = |name: &str| {
        publish::STEPS
            .iter()
            .position(|s| *s == name)
            .unwrap_or_else(|| panic!("step {name} missing from publish::STEPS"))
    };
    let selfcheck = pos("selfcheck");
    for later in [
        "draft", "upload", "preflip", "tag", "flip", "verify", "mirror",
    ] {
        assert!(
            selfcheck < pos(later),
            "selfcheck (the paint smoke) must precede {later}"
        );
    }
}

// --- the CLI surface ---------------------------------------------------------

/// `--no-paint-smoke` parses into [`publish::CutOptions`], and combines with
/// nothing that already refuses other cut flags — a resume re-earns the proof.
#[test]
fn the_flag_parses_and_respects_the_exclusivity_rules() {
    let args = |list: &[&str]| -> Vec<String> { list.iter().map(|s| s.to_string()).collect() };
    match cli::parse(&args(&["cut", "--no-paint-smoke"])) {
        Ok(cli::Cmd::Cut { opts, .. }) => {
            assert!(opts.no_paint_smoke, "--no-paint-smoke must set its option")
        }
        other => panic!("cut --no-paint-smoke must parse as a cut: {other:?}"),
    }
    match cli::parse(&args(&["cut"])) {
        Ok(cli::Cmd::Cut { opts, .. }) => {
            assert!(!opts.no_paint_smoke, "the default is a running smoke")
        }
        other => panic!("bare cut must parse: {other:?}"),
    }
    assert!(
        cli::parse(&args(&["cut", "--resume", "--no-paint-smoke"])).is_err(),
        "--resume fixes the cut's parameters; the escape must be refused there"
    );
    assert!(
        cli::parse(&args(&["cut", "--abandon", "v0.50.0", "--no-paint-smoke"])).is_err(),
        "--abandon combines with no other cut flag"
    );
}
