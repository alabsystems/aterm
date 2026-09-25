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
///
/// The error side is a [`publish::PaintRefusal`] and not a string because the
/// two refusals are different claims: "it did not paint" is about the ARTIFACT,
/// "this take is not evidence" is about the TAKE. Conflating them is what let a
/// starved instrument be read as a verdict about paint from the day the
/// label was added (2026-09-10) to the v0.87.0 cut.
struct FakePaintProbe {
    log: Log,
    verdict: Result<&'static str, publish::PaintRefusal>,
}

impl publish::PaintProbe for FakePaintProbe {
    fn paint(&self, bundle_binary: &Path) -> Result<String, publish::PaintRefusal> {
        record(&self.log, "paint", bundle_binary);
        self.verdict.clone().map(str::to_string)
    }
}

/// The line every take of the v0.87.0 cut printed: the disqualification and the
/// green on the SAME LINE. Verbatim from that cut's transcript, trimmed to the
/// fields that decide.
const STARVED_GREEN_TAKE: &str = "PAINT shape=fake-claude total_ink=1553 union_hues=9 \
     sched_late_max_us=88046 sched_qos=utility starved=yes evidence=unproved \
     reason=\"the probe's own driver was descheduled for 88046us (>= 25000us) and this process \
     family runs at QoS utility ... This take is not evidence about paint; run the smoke from an \
     interactive shell or a LaunchAgent with ProcessType=Interactive\" verdict=PASS";

/// The same shape on a sound instrument — a take that may be believed.
const SOUND_GREEN_TAKE: &str = "PAINT shape=fake-claude total_ink=1553 union_hues=9 \
     sched_late_max_us=3012 sched_qos=default starved=no evidence=sound \
     reason=\"the DRIVER kept its cadence\" verdict=PASS";

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
    PathBuf::from("/cut/dist/cut-1790000000.noindex/aterm.app")
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
            verdict: Err(publish::PaintRefusal::DidNotPaint(
                "PAINT shape=fake-claude total_ink=0 verdict=FAIL".to_string(),
            )),
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
/// cut exactly as hard as "it did not paint". This is the vacuity the audit
/// found (proofs that measured nothing reading as green), pinned shut at the
/// cut. Since 2026-09-17 it refuses in its OWN words: a take that never ran is
/// not evidence that the artifact is dark, and the transcript may not say it
/// is.
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
            verdict: Err(publish::PaintRefusal::NotEvidence(
                "PAINT-COULD-NOT-RUN control socket never appeared".to_string(),
            )),
        },
        &RecordingTools {
            log: Rc::clone(&log),
        },
    )
    .expect_err("an unproven bundle must refuse the cut");
    let msg = err.to_string();
    assert!(
        msg.contains("NOTHING WAS PROVEN"),
        "could-not-run refuses as UNPROVEN, not as a paint failure: {msg}"
    );
    assert!(
        !msg.contains("does not paint its flagship effect"),
        "a take that never ran may not be reported as a dark artifact: {msg}"
    );
}

// --- a take that is not evidence --------------------------------------------

/// THE DEFECT, AT THE SEAM THAT RECORDED IT AS SATISFIED. Every paint take of
/// the v0.87.0 cut printed `starved=yes ... "This take is not evidence about
/// paint" ... verdict=PASS` and exited 0, and the cut wrote each one into its
/// transcript as a passed obligation.
///
/// KILLS: reading the probe's exit code alone. A green exit whose own line
/// disowns the take is UNPROVED here, whatever the script said.
#[test]
fn a_starved_take_is_not_a_pass_even_when_the_probe_exits_green() {
    let refusal = publish::paint_probe_disposition(Some(0), STARVED_GREEN_TAKE.to_string())
        .expect_err("a take that disowns itself must never satisfy the paint obligation");
    assert!(
        matches!(refusal, publish::PaintRefusal::NotEvidence(_)),
        "a starved take is UNPROVED, not a paint failure: {refusal:?}"
    );
    assert!(
        refusal.report().contains("ProcessType=Interactive"),
        "the refusal must name the remedy: {refusal:?}"
    );
    assert!(
        refusal.report().contains("88046"),
        "the refusal must carry the take's own measurement: {refusal:?}"
    );
}

/// N STARVED TAKES ARE N TAKES OF ZERO EVIDENCE. The disposition is a function
/// of one take's own exit code and line, so nothing accumulates across takes —
/// re-running the smoke under the same starved tier (which is the tier the job
/// runs in, not a matter of luck) can never promote it to a pass.
///
/// KILLS: a best-of-N retry, a "seen it pass once" memo, or any caller that
/// hopes a flake will clear.
#[test]
fn repeating_a_starved_take_never_launders_it_into_a_pass() {
    for attempt in 1..=8 {
        let outcome = publish::paint_probe_disposition(Some(0), STARVED_GREEN_TAKE.to_string());
        assert!(
            matches!(outcome, Err(publish::PaintRefusal::NotEvidence(_))),
            "attempt {attempt} must refuse, not pass: {outcome:?}"
        );
    }
}

/// The exit protocol, arm by arm — including exit 3, the disposition
/// `paint_probe.sh` gained for "the take ran and is not evidence".
///
/// KILLS: mapping 3 to the abnormal-death arm (which would still refuse, but
/// would blame the protocol instead of the tier), and mapping 1 to
/// `NotEvidence` (which would let a REAL blackout be reported as "we could not
/// tell" — starvation may only ever take a green away, never rescue a red).
#[test]
fn the_exit_protocol_maps_each_code_to_its_own_disposition() {
    assert_eq!(
        publish::paint_probe_disposition(Some(0), SOUND_GREEN_TAKE.to_string()),
        Ok(SOUND_GREEN_TAKE.to_string()),
        "a sound green take passes, exactly as before"
    );
    assert!(
        matches!(
            publish::paint_probe_disposition(Some(1), "PAINT total_ink=0 verdict=FAIL".to_string()),
            Err(publish::PaintRefusal::DidNotPaint(_))
        ),
        "exit 1 is a claim about PAINT"
    );
    for code in [Some(2), Some(publish::PAINT_EXIT_UNPROVED), Some(7), None] {
        assert!(
            matches!(
                publish::paint_probe_disposition(code, "PAINT verdict=UNPROVED".to_string()),
                Err(publish::PaintRefusal::NotEvidence(_))
            ),
            "exit {code:?} proves nothing about paint"
        );
    }
}

/// THE OTHER HALF OF THE RULE: a sound take behaves EXACTLY as it did before
/// this lane. The gate must still be passable — a check nothing can satisfy is
/// no more honest than one nothing can refute.
#[test]
fn a_sound_take_still_passes_and_still_carries_its_measurement() {
    let log = log();
    let (paint_note, _) = publish::selfcheck_paint_then_signing(
        CutKind::Real,
        "",
        &app(),
        &dmg_path(),
        false,
        None,
        &FakePaintProbe {
            log: Rc::clone(&log),
            verdict: Ok(SOUND_GREEN_TAKE),
        },
        &RecordingTools {
            log: Rc::clone(&log),
        },
    )
    .expect("a sound green take must self-check clean");
    assert!(
        paint_note.contains("ink asserted") && paint_note.contains("evidence=sound"),
        "the transcript keeps the take's own line: {paint_note:?}"
    );
    publish::paint_take_is_evidence(SOUND_GREEN_TAKE)
        .expect("a take labelled evidence=sound is evidence");
}

/// A line with NO evidence token at all is unproven, never sound — the
/// fail-closed direction. This is the older-probe / edited-probe case: the
/// release obligation is owned at this seam, so it is re-derived here rather
/// than trusted from an exit code.
#[test]
fn a_line_that_says_nothing_about_its_own_standing_is_not_evidence() {
    for line in [
        "PAINT shape=fake-claude total_ink=1553 verdict=PASS",
        "<no PAINT report line>",
        "PAINT starved=unknown evidence=unproved verdict=PASS",
        "PAINT evidence=sound verdict=UNPROVED",
    ] {
        let why = publish::paint_take_is_evidence(line)
            .expect_err("a take that does not stand behind itself may not satisfy the gate");
        assert!(
            why.contains("ProcessType=Interactive"),
            "every refusal names the remedy: {why}"
        );
    }
}

/// THE CUT REFUSES, IN THE RIGHT WORDS, AND SPENDS NO APPLE TOOL. The operator
/// is told what to do (the tier) and told that the only way past is the loud
/// acknowledged escape — there is no quiet one.
#[test]
fn an_unproven_take_refuses_the_cut_and_names_the_remedy() {
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
            verdict: Err(publish::PaintRefusal::NotEvidence(
                STARVED_GREEN_TAKE.to_string(),
            )),
        },
        &RecordingTools {
            log: Rc::clone(&log),
        },
    )
    .expect_err("an unproven take must refuse the cut");
    let msg = err.to_string();
    assert!(
        msg.contains("NOTHING WAS PROVEN") && msg.contains("UNPROVED"),
        "the refusal must say the obligation was not met, not that the artifact is dark: {msg}"
    );
    assert!(
        !msg.contains("does not paint its flagship effect"),
        "an unproven take is not a claim that the artifact is dark: {msg}"
    );
    assert!(
        msg.contains("ProcessType=Interactive") && msg.contains("interactive shell"),
        "the refusal must name the remedy, or it will be switched off: {msg}"
    );
    assert!(
        msg.contains(publish::NO_PAINT_SMOKE_ACK_VALUE),
        "the only way past must be the loud, acknowledged one: {msg}"
    );
    assert_eq!(
        entries(&log),
        vec!["paint:aterm".to_string()],
        "an unproven take must abort before any Apple tool runs"
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
