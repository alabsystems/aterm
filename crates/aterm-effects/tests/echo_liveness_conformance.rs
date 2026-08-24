// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ECHO-LIVENESS CONFORMANCE HARNESS — the CI face of the typed-echo liveness
//! contract, run on every `cargo test`, no window needed.
//!
//! WHY THIS FILE EXISTS. 201449c2 shipped the movement-admission gate WITH a
//! formal model that proved SAFETY (cold program output never paints trails) —
//! and nobody stated or checked LIVENESS. The model's echo environment was
//! idealized; real shells produce shapes it never contained, so a gate that
//! provably never lied provably never spoke: the rainbow trail blacked out on
//! every real box. The repair taught `CursorGlow::confirm_content_candidate`
//! how echoes actually look (E1 ghost text, E3 blank-space storage growth,
//! E4 overtype of a visible suggestion). This harness replays those REAL
//! shapes through the REAL pipeline seam — `note_typed_expected` /
//! `observe_row` / `confirm_content_candidate`, the exact seam the fix's unit
//! tests drive — so the liveness half of the contract can never again go
//! silently unchecked, and it replays the cold/swallowed/deviating shapes so
//! the safety half stays pinned in the same breath.
//!
//! The FORMAL statement of this contract lives in
//! `aterm_spec::derive::typed_echo_liveness_model` (environment adversary +
//! checked liveness obligations), and the anti-drift lock binding the code to
//! that model is `real_confirm_content_candidate_refines_the_typed_echo_liveness_model`
//! in `cursor_glow.rs`. This file is the third leg: the plain-`cargo test`
//! replay that needs neither the spec crate's Tier-0 interpreter nor a
//! headless window — the live-introspection proof was done manually and its
//! traces are archived; the unit seam suffices for CI.
//!
//! It also binds the DIAGNOSIS RING (the `trail` control verb's substrate):
//! every replayed decision must land in `CursorGlow::admission_log` with the
//! right phase, so the one-command introspection path cannot rot into a
//! sensor that misses exactly the failures it was built to report.
//!
//! REGISTERED STANDING FINDINGS (reprinted below, never waived silently):
//! E2 (split-batch echo) and E5 (burst between frames) are KNOWN liveness
//! gaps — this harness pins today's honest retiring behavior so a future fix
//! of either gap must update the model, the refinement binding, and this
//! replay in the same loud change.

use std::time::{Duration, Instant};

use aterm_effects::cursor_glow::{
    AdmissionPhase, ContentCandidateDecision, CursorGlow, Geom, GlowConfig, GlowStyle,
    RAINBOW_WAKE_PERSIST,
};
use aterm_effects::cursor_trail::{ContentGeneration, ExpectedCellSpan, ExpectedRowSnapshot};

fn geom() -> Geom {
    Geom {
        cw: 8,
        ch: 16,
        rows: 6,
        cols: 40,
        origin_x: 0,
        origin_y: 0,
        win_w: 320,
        win_h: 96,
        head: 0,
    }
}

fn cfg() -> GlowConfig {
    GlowConfig {
        enabled: true,
        dark_theme: true,
        theme_fg: 0x00C8_D3F5,
        theme_bg: 0x001A_1B26,
        style: GlowStyle::Lumen,
        color: 0x0050_FA7B,
        accent: 0x007A_A2F7,
        duration: Duration::from_millis(240),
        length: 18,
        intensity: 0.7,
        radius: 0.6,
        ring: true,
        beam: true,
        head_dx: 0.5,
        pack: None,
        wake_persist_s: RAINBOW_WAKE_PERSIST,
    }
}

fn generation(process_sequence: u32) -> ContentGeneration {
    ContentGeneration {
        process_sequence,
        terminal_id: 23,
        alternate_screen: false,
    }
}

/// The prompt row as the input side captures it: `$ gi`, caret at column 4,
/// tail-filled to the 12-column grid width — the same fixture the fix's unit
/// tests and the refinement binding use.
fn baseline_cells() -> [char; 12] {
    let mut cells = [' '; 12];
    cells[0] = '$';
    cells[2] = 'g';
    cells[3] = 'i';
    cells
}

/// One armed real candidate: type 't' at (2,4) -> (2,5) over the baseline.
fn armed_glow(t0: Instant, seq: u32) -> CursorGlow {
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    glow.tick(Some((2, 4)), t0, &cfg(), geom(), &mut out);
    glow.note_typed_expected(
        t0 + Duration::from_millis(1),
        ExpectedCellSpan::from_cells(['t']).unwrap(),
        (2, 5),
        (2, 4),
        ExpectedRowSnapshot::from_slice(&baseline_cells()).unwrap(),
        generation(seq),
    );
    glow
}

/// The newest ring entry must record this decision: phase + reason + the
/// generation pair, so the `trail` verb reports what actually happened.
fn assert_ring_tail(glow: &CursorGlow, label: &str, phase: AdmissionPhase, reason: &str) {
    let tail = glow
        .admission_log()
        .last()
        .unwrap_or_else(|| panic!("{label}: the admission ring is empty"));
    assert_eq!(tail.phase, phase, "{label}: ring phase");
    assert_eq!(tail.reason, reason, "{label}: ring reason");
    let line = tail.line(Instant::now());
    assert!(
        line.starts_with("admission seq=") && line.contains(&format!("phase={}", phase.as_str())),
        "{label}: ring line shape drifted: {line}"
    );
}

/// LIVENESS, the half nobody checked in 201449c2: for every environment shape
/// the shipped code claims to handle, an armed typed keystroke whose echo
/// arrives EVENTUALLY CONFIRMS — the gate speaks, not merely never lies.
#[test]
fn handled_echo_shapes_confirm_through_the_real_seam() {
    let t0 = Instant::now();

    // PLAIN ECHO: the batch materializes exactly the typed cell.
    let mut plain_row = baseline_cells();
    plain_row[4] = 't';
    let mut glow = armed_glow(t0, 10);
    assert_ring_tail(&glow, "plain arm", AdmissionPhase::Armed, "armed");
    glow.observe_row(2, 5, &plain_row, t0 + Duration::from_millis(2));
    let decision =
        glow.confirm_content_candidate(Some((2, 5)), t0 + Duration::from_millis(2), generation(11));
    assert!(
        matches!(decision, Some(ContentCandidateDecision::Confirmed { .. })),
        "plain echo must confirm: {decision:?}"
    );
    assert_ring_tail(&glow, "plain", AdmissionPhase::Confirmed, "confirmed");

    // E1 GHOST TEXT: the same echo batch paints an autosuggest tail at/after
    // the caret (zsh-autosuggestions POSTDISPLAY — the shape that blacked out
    // every real interactive box). The presentation zone carries no veto.
    let suggested = ['$', ' ', 'g', 'i', 't', ' ', 's', 't', 'a', 't', 'u', 's'];
    let mut glow = armed_glow(t0, 20);
    glow.observe_row(2, 5, &suggested, t0 + Duration::from_millis(2));
    let decision =
        glow.confirm_content_candidate(Some((2, 5)), t0 + Duration::from_millis(2), generation(21));
    assert!(
        matches!(decision, Some(ContentCandidateDecision::Confirmed { .. })),
        "E1 suggestion-tailed echo must confirm: {decision:?}"
    );
    assert_ring_tail(&glow, "E1", AdmissionPhase::Confirmed, "confirmed");

    // E3 TYPED SPACE onto tail-filled implicit blanks: content-invisible under
    // the implicit-blank lens; the stored row growing over the owned span is
    // the honest witness (a swallowed echo on a trimmed row cannot fake it).
    let mut space_baseline = baseline_cells();
    space_baseline[4] = 't';
    let grown = ['$', ' ', 'g', 'i', 't', ' '];
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    glow.tick(Some((2, 5)), t0, &cfg(), geom(), &mut out);
    glow.note_typed_expected(
        t0 + Duration::from_millis(1),
        ExpectedCellSpan::from_cells([' ']).unwrap(),
        (2, 6),
        (2, 5),
        ExpectedRowSnapshot::from_slice(&space_baseline).unwrap(),
        generation(30),
    );
    glow.observe_row(2, 6, &grown, t0 + Duration::from_millis(2));
    let decision =
        glow.confirm_content_candidate(Some((2, 6)), t0 + Duration::from_millis(2), generation(31));
    assert!(
        matches!(decision, Some(ContentCandidateDecision::Confirmed { .. })),
        "E3 typed space over implicit blanks must confirm: {decision:?}"
    );
    assert_ring_tail(&glow, "E3", AdmissionPhase::Confirmed, "confirmed");

    // E4 OVERTYPE of a VISIBLE suggestion: the typed glyph already sits
    // painted at the caret, the echo is a null diff — the null diff plus the
    // exact landing under an attributable generation IS the proof.
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    glow.tick(Some((2, 4)), t0, &cfg(), geom(), &mut out);
    glow.note_typed_expected(
        t0 + Duration::from_millis(1),
        ExpectedCellSpan::from_cells(['t']).unwrap(),
        (2, 5),
        (2, 4),
        ExpectedRowSnapshot::from_slice(&suggested).unwrap(),
        generation(40),
    );
    glow.observe_row(2, 5, &suggested, t0 + Duration::from_millis(2));
    let decision =
        glow.confirm_content_candidate(Some((2, 5)), t0 + Duration::from_millis(2), generation(41));
    assert!(
        matches!(decision, Some(ContentCandidateDecision::Confirmed { .. })),
        "E4 overtype of a visible suggestion must confirm: {decision:?}"
    );
    assert_ring_tail(&glow, "E4", AdmissionPhase::Confirmed, "confirmed");
}

/// SAFETY, the half 201449c2 did prove — kept pinned in the same harness so
/// the liveness repair can never quietly widen the gate: cold, swallowed and
/// deviating shapes never admit.
#[test]
fn cold_swallowed_and_deviating_shapes_never_admit() {
    let t0 = Instant::now();

    // COLD SPINNER: no keystroke, output only — no candidate exists to admit,
    // and the ring stays empty (nothing armed, nothing decided).
    let spinner = ['$', ' ', 'g', 'i', ' ', ' ', '|'];
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    glow.tick(Some((2, 4)), t0, &cfg(), geom(), &mut out);
    glow.observe_row(2, 5, &spinner, t0 + Duration::from_millis(2));
    let decision =
        glow.confirm_content_candidate(Some((2, 5)), t0 + Duration::from_millis(2), generation(81));
    assert_eq!(decision, None, "cold output has no candidate to decide");
    assert_eq!(
        glow.admission_log().count(),
        0,
        "cold output must leave no admission record — nothing armed"
    );

    // SWALLOWED ECHO: the generation crossed but the typed cell never
    // materialized — the batch was someone else's output.
    let mut glow = armed_glow(t0, 90);
    glow.observe_row(2, 5, &baseline_cells(), t0 + Duration::from_millis(2));
    let decision =
        glow.confirm_content_candidate(Some((2, 5)), t0 + Duration::from_millis(2), generation(91));
    assert!(
        matches!(decision, Some(ContentCandidateDecision::Retired { .. })),
        "a swallowed echo must retire: {decision:?}"
    );
    assert_ring_tail(&glow, "swallowed", AdmissionPhase::Retired, "row-unchanged");

    // DEVIATING ECHO: the shell echoed a different glyph at the caret than
    // the keystroke predicted.
    let mut deviating_row = baseline_cells();
    deviating_row[4] = 'x';
    let mut glow = armed_glow(t0, 100);
    glow.observe_row(2, 5, &deviating_row, t0 + Duration::from_millis(2));
    let decision = glow.confirm_content_candidate(
        Some((2, 5)),
        t0 + Duration::from_millis(2),
        generation(101),
    );
    assert!(
        matches!(decision, Some(ContentCandidateDecision::Retired { .. })),
        "a deviating echo must retire: {decision:?}"
    );
    assert_ring_tail(&glow, "deviating", AdmissionPhase::Retired, "row-mismatch");
}

/// REGISTERED STANDING GAPS — pinned as today's honest behavior and REPRINTED,
/// never waived silently. Fixing either gap flips these asserts, forcing the
/// model, the refinement binding, and this replay to move in one loud change.
#[test]
fn standing_gaps_e2_and_e5_retire_today_and_are_reprinted() {
    let t0 = Instant::now();
    let mut plain_row = baseline_cells();
    plain_row[4] = 't';

    // E2 SPLIT ECHO: the echo lands two generations past the baseline
    // (baseline+2); the strict next-generation law retires a REAL echo.
    let mut glow = armed_glow(t0, 50);
    glow.observe_row(2, 5, &plain_row, t0 + Duration::from_millis(2));
    let decision =
        glow.confirm_content_candidate(Some((2, 5)), t0 + Duration::from_millis(2), generation(52));
    assert!(
        matches!(decision, Some(ContentCandidateDecision::Retired { .. })),
        "E2 split-batch echo retires today (standing gap): {decision:?}"
    );
    assert_ring_tail(&glow, "E2", AdmissionPhase::Retired, "generation-skip");
    println!(
        "STANDING FINDING (registered, unresolved): E2: an echo split across two PTY \
         read batches (generation baseline+2) settles RETIRED — the strict \
         next-generation law is a known liveness gap, follow-up to the 201449c2 \
         blackout repair"
    );

    // E5 BURST: several keys between rendered frames — the only fresh probe
    // predates the keystroke, so proof capture declines the stale anchor.
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    glow.tick(Some((2, 4)), t0, &cfg(), geom(), &mut out);
    glow.observe_row(2, 5, &plain_row, t0 + Duration::from_millis(1));
    glow.note_typed_expected(
        t0 + Duration::from_millis(2),
        ExpectedCellSpan::from_cells(['t']).unwrap(),
        (2, 5),
        (2, 4),
        ExpectedRowSnapshot::from_slice(&baseline_cells()).unwrap(),
        generation(60),
    );
    let decision =
        glow.confirm_content_candidate(Some((2, 5)), t0 + Duration::from_millis(3), generation(61));
    assert!(
        matches!(decision, Some(ContentCandidateDecision::Retired { .. })),
        "E5 stale-anchor burst retires today (standing gap): {decision:?}"
    );
    assert_ring_tail(&glow, "E5", AdmissionPhase::Retired, "probe-predates-key");
    println!(
        "STANDING FINDING (registered, unresolved): E5: several keys between rendered \
         frames leave a stale proof anchor and the capture declines — known liveness \
         gap, follow-up alongside split-pane arming"
    );
}

/// The DIAGNOSIS RING's own contract: bounded, drop-oldest, monotonic seq —
/// the properties that make the `trail` verb's report trustworthy and the hot
/// path allocation-free (one resident slot write per lifecycle event).
#[test]
fn admission_ring_is_bounded_drop_oldest_with_monotonic_seq() {
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    glow.tick(Some((2, 4)), t0, &cfg(), geom(), &mut out);
    // 40 keystroke cycles = 80 lifecycle events (armed + decided each), well
    // past the cap. Each cycle's echo is SWALLOWED (row unchanged), so the
    // decision retires and clears the candidate — every cycle re-arms fresh
    // through the real seam with no by-hand state surgery between cycles.
    let cycles = 40u32;
    for i in 0..cycles {
        let at = t0 + Duration::from_millis(u64::from(i) * 4);
        glow.note_typed_expected(
            at + Duration::from_millis(1),
            ExpectedCellSpan::from_cells(['t']).unwrap(),
            (2, 5),
            (2, 4),
            ExpectedRowSnapshot::from_slice(&baseline_cells()).unwrap(),
            generation(1000 + i * 2),
        );
        glow.observe_row(2, 5, &baseline_cells(), at + Duration::from_millis(2));
        let decision = glow.confirm_content_candidate(
            Some((2, 5)),
            at + Duration::from_millis(2),
            generation(1000 + i * 2 + 1),
        );
        assert!(
            matches!(decision, Some(ContentCandidateDecision::Retired { .. })),
            "cycle {i}'s swallowed echo must retire: {decision:?}"
        );
    }
    let records: Vec<_> = glow.admission_log().copied().collect();
    assert_eq!(
        records.len(),
        aterm_effects::cursor_glow::ADMISSION_LOG_CAP,
        "the ring must hold exactly its cap after overflow"
    );
    let total = u64::from(cycles) * 2;
    for (i, pair) in records.windows(2).enumerate() {
        assert_eq!(
            pair[1].seq,
            pair[0].seq + 1,
            "seq must be monotonic without gaps at index {i}"
        );
    }
    assert_eq!(
        records.last().unwrap().seq,
        total,
        "the newest record is the last event pushed"
    );
    assert_eq!(
        records.first().unwrap().seq,
        total - (records.len() as u64) + 1,
        "drop-oldest: the ring holds exactly the newest cap-many events"
    );
}
