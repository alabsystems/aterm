// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-dispatch POLICY-GATE cost, measured through a real `Terminal` that
//! carries the shipping session's policy engine.
//!
//! ## Why this bench had to be written
//!
//! Four capability gates consult `PolicyEngine::evaluate` once per dispatched
//! sequence — the OSC 52 clipboard gate, the single `send_response` sink, the
//! OSC 133/633 shell-integration gate, and the XTWINOPS `CSI t` gate. Before
//! this file NOTHING in the tree priced any of them end to end:
//!
//! * `aterm-policy/benches/policy_engine_hotpath` times `evaluate` on a probe
//!   HOISTED OUT of the timed loop, so it prices neither the per-dispatch probe
//!   construction (2-3 heap allocations at three of the four sites) nor the
//!   terminal dispatch path that reaches it.
//! * every `aterm-bench` corpus is SGR / text / OSC 8, none of which produces a
//!   terminal response, and `Terminal::new` attaches NO policy engine — so under
//!   the existing benches the engine branch is never taken at all.
//!
//! The shipping GUI is the opposite: `aterm-gui/src/spawn.rs` installs
//! `PolicyEngine::new(profiles::standard())` on EVERY session, unconditionally,
//! before the reader thread produces a byte. [`shipping_session`] mirrors that
//! configuration exactly (clipboard WRITE authorized, shell-integration nonce
//! authorized AND required, `allow_window_ops` left at its `false` default), so
//! the `engine` arms below are the configuration users actually run.
//!
//! ## Arms
//!
//! Every workload is measured twice:
//!
//! * `<workload>/engine` — shipping configuration, standard-profile engine
//!   installed. Executes the gate.
//! * `<workload>/none` — identical terminal with NO engine installed. This is
//!   the CONTROL: `engine_decision` short-circuits to `Fallback` without
//!   evaluating, and three of the four sites skip the probe build entirely.
//!   The engine-minus-none delta is the gate's true per-dispatch price.
//!
//! ## Reach guard
//!
//! [`verify_gate_reach`] runs before any timing and is TWO-SIDED for every
//! workload: it pins the EXACT number of gate dispatches the corpus drives
//! (via a policy that flips that gate's decision, which is observable in the
//! response bytes / clipboard callbacks / shell drop counter), AND it pins the
//! control arm's count on the other side. An empty or collapsed corpus fails
//! both halves instead of quietly measuring nothing.
//!
//! ## Fresh terminal per iteration
//!
//! Each iteration gets a fresh `Terminal` + engine through `iter_batched_ref`
//! (setup is NOT timed). This is not cosmetic: the standard profile's `palette`
//! bucket is 64 tokens refilling at 16/s, so a REUSED terminal would drain it
//! within the first iteration and every later OSC 4 query would be dropped at
//! `palette_rate_limit_consume_one` — one gate short of the `send_response`
//! sink this workload exists to price. The same trap applies to the 64 KiB
//! `response` bucket. Fresh state per iteration keeps every workload inside the
//! regime it claims to measure, and `verify_gate_reach` asserts that regime.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use aterm_core::terminal::{ClipboardAccess, ClipboardOperation, Terminal};
use aterm_policy::engine::PolicyEngine;
use aterm_policy::{
    Defaults, OriginTag, Policy, Profile, Response, Rule, SCHEMA_VERSION, profiles,
};
use criterion::{BatchSize, Criterion, black_box, criterion_group, criterion_main};

const ROWS: u16 = 24;
const COLS: u16 = 80;

/// Dispatches per workload corpus. Sized so that every reply the workload
/// produces fits inside the standard profile's 64 KiB `response` burst on a
/// FRESH engine — see the module docs.
const CPR_N: usize = 2_000;
/// DA1+DA2 pairs (two replies each).
const DA_PAIRS: usize = 1_000;
/// OSC 4 palette-query sequences. `OSC4_SEQS * OSC4_INDICES` must stay within
/// the standard profile's [`PALETTE_BUCKET_CAPACITY`] or the later queries never
/// reach `send_response`; `verify_gate_reach` asserts the resulting reply count.
const OSC4_SEQS: usize = 9;
/// Query indices per OSC 4 sequence, i.e. the response-sink amplification one
/// short sequence can drive.
///
/// The policy profile's `palette.per_sequence_max` is 16, but the PARSER caps an
/// OSC payload at `aterm_parser::MAX_OSC_PARAMS` = 16 parameters, and an OSC 4
/// query spends one on the command number and two on every index/`?` pair. So a
/// single dispatch can carry at most `(16 - 1) / 2` = 7 queries and the profile's
/// 16 is unreachable from the wire. This is 7 and not 16 on purpose — the guard
/// below fails loudly if the arithmetic ever drifts.
const OSC4_INDICES: usize = (MAX_OSC_PARAMS - 1) / 2;
/// Parser-side cap on OSC parameters (`aterm-parser/src/lib.rs:212`). Mirrored
/// here because it is `pub(crate)` in the parser.
const MAX_OSC_PARAMS: usize = 16;
/// Standard profile's `palette` token-bucket capacity (`profiles.rs`). Queries
/// past it are dropped BEFORE the response sink, so the workload must stay under.
const PALETTE_BUCKET_CAPACITY: usize = 64;
/// OSC 52 dispatches (query workload and set workload alike).
const OSC52_N: usize = 2_000;
/// Shell-integration command cycles; each emits [`MARKS_PER_CYCLE`] marks.
const SHELL_CYCLES: usize = 200;
/// Marks per command cycle, matching the shipped preambles (133;D, 133;A,
/// 133;B, 633;E, 133;C — `aterm_shell_integration.bash:81`).
const MARKS_PER_CYCLE: usize = 5;
/// `CSI 18 t` dispatches.
const CSI_T_N: usize = 2_000;

/// Session nonce, mirroring the per-tab CSPRNG nonce `spawn.rs` installs.
const NONCE: [u8; 32] = [0xA5; 32];

// The OSC 4 workload has two sizing constraints, asserted rather than assumed.
// Both failure modes are silent: over the parser's parameter cap and the tail of
// every sequence is truncated before the handler sees it; over the palette
// bucket and the tail of the workload is dropped at
// `palette_rate_limit_consume_one`, ONE GATE SHORT of the response sink this
// workload exists to price. `verify_gate_reach` then checks the reply count these
// bounds are supposed to produce.
//
// `OSC4_INDICES * 2 < MAX_OSC_PARAMS` is `1 + OSC4_INDICES * 2 <= MAX_OSC_PARAMS`:
// one parameter for the command number, two for each index/`?` pair.
const _: () = assert!(
    OSC4_INDICES * 2 < MAX_OSC_PARAMS,
    "osc4_palette: the index/? pairs plus the command number exceed the parser's OSC parameter cap; every sequence would be truncated"
);
const _: () = assert!(
    OSC4_SEQS * OSC4_INDICES <= PALETTE_BUCKET_CAPACITY,
    "osc4_palette: more queries than the palette token bucket holds on a fresh engine; the overflow never reaches the response sink"
);

/// Wire form of [`NONCE`] (`id=` + 64 hex chars).
fn nonce_param() -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(3 + 64);
    s.push_str("id=");
    for b in NONCE {
        let _ = write!(s, "{b:02x}");
    }
    s
}

// ---------------------------------------------------------------------------
// Policies
// ---------------------------------------------------------------------------

/// Which policy (if any) the terminal under test carries.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Arm {
    /// Shipping configuration: `profiles::standard()`.
    Standard,
    /// Control: no engine installed at all.
    NoEngine,
    /// Guard-only: a single `*` = `Drop` rule. Denies the response sink and the
    /// shell-integration gate, which is how the guard counts their dispatches.
    DenyAll,
    /// Guard-only: `OSC 52 query` = `Execute`, which OPENS a gate the shipping
    /// profile leaves shut — the positive half of the OSC 52 reach guard.
    AllowOsc52Query,
    /// Guard-only: `OSC 52 set` = `Drop`, the negative half of the OSC 52 set
    /// reach guard (the shipping profile lets set through on the legacy bool).
    DenyOsc52Set,
    /// Guard-only: `CSI t` = `Execute`, which opens XTWINOPS even though
    /// `allow_window_ops` is false — the positive half of the `CSI t` guard.
    AllowCsiT,
}

fn one_rule_policy(sequence: &str, origin_min: OriginTag, response: Response) -> Policy {
    Policy {
        schema_version: SCHEMA_VERSION,
        profile: Profile::Standard,
        defaults: Defaults {
            unmatched: Response::Warn,
            shell_integration_require_nonce: true,
        },
        rules: vec![Rule {
            sequence: sequence.to_owned(),
            origin_min,
            response,
            rate_limit: None,
            prompt_id: None,
        }],
        rate_limits: vec![],
    }
}

impl Arm {
    fn policy(self) -> Option<Policy> {
        match self {
            Self::Standard => Some(profiles::standard()),
            Self::NoEngine => None,
            Self::DenyAll => Some(one_rule_policy(
                "*",
                OriginTag::NetworkUntrusted,
                Response::Drop,
            )),
            Self::AllowOsc52Query => Some(one_rule_policy(
                "OSC 52 query",
                OriginTag::Pty,
                Response::Execute,
            )),
            Self::DenyOsc52Set => Some(one_rule_policy(
                "OSC 52 set",
                OriginTag::Pty,
                Response::Drop,
            )),
            Self::AllowCsiT => Some(one_rule_policy("CSI t", OriginTag::Pty, Response::Execute)),
        }
    }
}

// ---------------------------------------------------------------------------
// Terminal factory — mirrors aterm-gui/src/spawn.rs
// ---------------------------------------------------------------------------

/// Clipboard-callback call counters, so the guard can observe gates whose only
/// effect is whether the host delegate is reached.
#[derive(Clone, Default)]
struct Counters {
    sets: Arc<AtomicUsize>,
    queries: Arc<AtomicUsize>,
}

impl Counters {
    fn sets(&self) -> usize {
        self.sets.load(Ordering::Relaxed)
    }

    fn queries(&self) -> usize {
        self.queries.load(Ordering::Relaxed)
    }
}

/// Build a terminal configured exactly like a shipping session
/// (`aterm-gui/src/spawn.rs`): clipboard WRITE authorized (query is NOT — the
/// shipping app never grants it), shell-integration nonce authorized and
/// required, a live clipboard delegate, and the arm's policy engine installed
/// last. `allow_window_ops` is deliberately left at its `false` default.
fn shipping_session(arm: Arm, counters: &Counters) -> Terminal {
    let mut term = Terminal::new(ROWS, COLS);

    term.authorize_clipboard_access(ClipboardAccess::Write);
    let sets = Arc::clone(&counters.sets);
    let queries = Arc::clone(&counters.queries);
    term.set_clipboard_callback(move |op| match op {
        ClipboardOperation::Set { .. } | ClipboardOperation::Clear { .. } => {
            sets.fetch_add(1, Ordering::Relaxed);
            None
        }
        ClipboardOperation::Query { .. } => {
            queries.fetch_add(1, Ordering::Relaxed);
            Some(String::from("x"))
        }
    });

    term.authorize_shell_integration(NONCE);
    term.set_require_shell_integration_nonce(true);

    if let Some(policy) = arm.policy() {
        term.apply_policy_engine(PolicyEngine::new(policy));
    }
    term
}

// ---------------------------------------------------------------------------
// Corpora
// ---------------------------------------------------------------------------

/// `CPR_N` cursor-position reports. Each reply is built into a `StackResponse`
/// by `handle_dsr` specifically to avoid ONE heap allocation — and then handed
/// to the response sink, which is the site under measurement.
fn corpus_dsr_cpr() -> Vec<u8> {
    b"\x1b[6n".repeat(CPR_N)
}

/// `DA_PAIRS` primary+secondary device-attribute requests: the classic
/// response-amplification flood.
fn corpus_da_burst() -> Vec<u8> {
    b"\x1b[c\x1b[>c".repeat(DA_PAIRS)
}

/// `OSC4_SEQS` palette queries, each asking for `OSC4_INDICES` indices. One
/// ~30-byte sequence drives `OSC4_INDICES` separate response-sink gates — the
/// largest per-dispatch amplification of that sink the wire format allows.
fn corpus_osc4_palette() -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..OSC4_SEQS {
        out.extend_from_slice(b"\x1b]4");
        for idx in 0..OSC4_INDICES {
            out.extend_from_slice(format!(";{idx};?").as_bytes());
        }
        out.push(0x07);
    }
    out
}

/// `OSC52_N` clipboard QUERIES — the only OSC 52 class the shipping app leaves
/// unauthorized, hence the one whose cost is gate-only.
fn corpus_osc52_query() -> Vec<u8> {
    b"\x1b]52;c;?\x07".repeat(OSC52_N)
}

/// `OSC52_N` clipboard SETS — authorized in the shipping app, so the gate is
/// followed by a base64 decode and a delegate call that dwarf it. Present so
/// the measurement can say so with numbers instead of assuming it.
fn corpus_osc52_set() -> Vec<u8> {
    b"\x1b]52;c;SGVsbG8sIHdvcmxkIQ==\x07".repeat(OSC52_N)
}

/// `SHELL_CYCLES` full command cycles of nonced shell-integration marks.
fn corpus_shell_marks() -> Vec<u8> {
    let id = nonce_param();
    let cycle = format!(
        "\x1b]133;D;0;{id}\x07\x1b]133;A;{id}\x07\x1b]133;B;{id}\x07\
         \x1b]633;E;ls -la;{id}\x07\x1b]133;C;{id}\x07"
    );
    cycle.repeat(SHELL_CYCLES).into_bytes()
}

/// `CSI_T_N` XTWINOPS text-area-size reports. `18` is chosen because its
/// handler needs no window delegate, so the guard can observe the gate purely
/// through the response bytes.
fn corpus_csi_t() -> Vec<u8> {
    b"\x1b[18t".repeat(CSI_T_N)
}

// ---------------------------------------------------------------------------
// Observation helpers
// ---------------------------------------------------------------------------

/// What one corpus run produced. Everything here is observable from OUTSIDE the
/// terminal, so the guard needs no instrumentation inside the gate.
struct RunResult {
    response: Vec<u8>,
    clipboard_sets: usize,
    clipboard_queries: usize,
    shell_dropped: u64,
}

fn run_corpus(arm: Arm, corpus: &[u8]) -> RunResult {
    let counters = Counters::default();
    let mut term = shipping_session(arm, &counters);
    term.process(corpus);
    RunResult {
        response: term.take_response().unwrap_or_default(),
        clipboard_sets: counters.sets(),
        clipboard_queries: counters.queries(),
        shell_dropped: term.shell_integration_dropped_count(),
    }
}

/// Count non-overlapping occurrences of `needle` in `hay`.
fn count_occurrences(hay: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() || hay.len() < needle.len() {
        return 0;
    }
    let mut n = 0;
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        if &hay[i..i + needle.len()] == needle {
            n += 1;
            i += needle.len();
        } else {
            i += 1;
        }
    }
    n
}

// ---------------------------------------------------------------------------
// TWO-SIDED REACH GUARD
// ---------------------------------------------------------------------------

/// Prove every workload drives its gate the exact number of times claimed, and
/// prove the control arm does not.
///
/// The pattern is the same for all four gates: take a policy that FLIPS that
/// gate's decision and count the observable difference over the IDENTICAL
/// corpus. A flip that shows up N times proves the gate executed N times; the
/// control arm pins the other side. Both halves are exact equalities, so an
/// empty corpus, a corpus that stopped parsing, or a workload throttled into a
/// different regime fails the guard rather than silently measuring nothing.
#[allow(
    clippy::too_many_lines,
    reason = "one guard per workload, each a three-arm differential; splitting them \
              would hide the fact that every workload is guarded in one place"
)]
fn verify_gate_reach() {
    // -- Response sink (handler.rs:378), via CPR ---------------------------
    let cpr = corpus_dsr_cpr();
    let std_cpr = run_corpus(Arm::Standard, &cpr);
    let deny_cpr = run_corpus(Arm::DenyAll, &cpr);
    let none_cpr = run_corpus(Arm::NoEngine, &cpr);
    let cpr_replies = count_occurrences(&std_cpr.response, b"R");
    assert_eq!(
        cpr_replies, CPR_N,
        "dsr_cpr: {cpr_replies} CPR replies for {CPR_N} requests — the workload is \
         not driving the response sink once per dispatch (rate limiter? buffer cap?)"
    );
    assert!(
        deny_cpr.response.is_empty(),
        "dsr_cpr: a `* = Drop` policy left {} response bytes — the response-sink \
         gate is NOT being consulted, so the engine arm measures nothing",
        deny_cpr.response.len()
    );
    assert_eq!(
        none_cpr.response, std_cpr.response,
        "dsr_cpr control: the no-engine arm must produce the same replies as the \
         standard-profile arm (it takes the `engine.is_none()` short-circuit)"
    );

    // -- Response sink, DA flood -------------------------------------------
    let da = corpus_da_burst();
    let std_da = run_corpus(Arm::Standard, &da);
    let deny_da = run_corpus(Arm::DenyAll, &da);
    let da_replies = count_occurrences(&std_da.response, b"c");
    assert_eq!(
        da_replies,
        DA_PAIRS * 2,
        "da_burst: {da_replies} device-attribute replies for {} requests",
        DA_PAIRS * 2
    );
    assert!(
        deny_da.response.is_empty(),
        "da_burst: `* = Drop` did not suppress the replies — gate not reached"
    );

    // -- Response sink, OSC 4 amplifier ------------------------------------
    // (The two sizing constraints for this workload are compile-time asserts at
    // the top of the file; the runtime reply count below is what proves they
    // produced the workload they were supposed to.)
    let osc4 = corpus_osc4_palette();
    let std_osc4 = run_corpus(Arm::Standard, &osc4);
    let deny_osc4 = run_corpus(Arm::DenyAll, &osc4);
    let none_osc4 = run_corpus(Arm::NoEngine, &osc4);
    let osc4_replies = count_occurrences(&std_osc4.response, b"\x1b]4;");
    assert_eq!(
        osc4_replies,
        OSC4_SEQS * OSC4_INDICES,
        "osc4_palette: {osc4_replies} palette replies for {OSC4_SEQS} sequences x \
         {OSC4_INDICES} indices — if this is short, either the parser truncated the \
         payload or the `palette` token bucket ate the queries BEFORE the response \
         sink, and the amplifier is not being measured"
    );
    assert!(
        deny_osc4.response.is_empty(),
        "osc4_palette: `* = Drop` did not suppress the palette replies"
    );
    assert_eq!(
        none_osc4.response, std_osc4.response,
        "osc4_palette control: no-engine arm must produce identical replies"
    );

    // -- OSC 52 query gate (clipboard_auth.rs:317) -------------------------
    let q52 = corpus_osc52_query();
    let std_q52 = run_corpus(Arm::Standard, &q52);
    let open_q52 = run_corpus(Arm::AllowOsc52Query, &q52);
    let none_q52 = run_corpus(Arm::NoEngine, &q52);
    assert_eq!(
        open_q52.clipboard_queries, OSC52_N,
        "osc52_query: opening the gate with `OSC 52 query = Execute` reached the \
         clipboard delegate {} times, expected {OSC52_N} — the corpus is not \
         driving the query gate once per dispatch",
        open_q52.clipboard_queries
    );
    assert_eq!(
        std_q52.clipboard_queries, 0,
        "osc52_query: the shipping profile must NOT reach the clipboard delegate \
         (query is never authorized in spawn.rs)"
    );
    assert_eq!(
        none_q52.clipboard_queries, 0,
        "osc52_query control: the no-engine arm must not reach the delegate either"
    );

    // -- OSC 52 set gate (clipboard_auth.rs:296) ---------------------------
    let s52 = corpus_osc52_set();
    let std_s52 = run_corpus(Arm::Standard, &s52);
    let deny_s52 = run_corpus(Arm::DenyOsc52Set, &s52);
    let none_s52 = run_corpus(Arm::NoEngine, &s52);
    assert_eq!(
        std_s52.clipboard_sets, OSC52_N,
        "osc52_set: the shipping profile authorizes clipboard WRITE, so all \
         {OSC52_N} sets must reach the delegate (got {})",
        std_s52.clipboard_sets
    );
    assert_eq!(
        deny_s52.clipboard_sets, 0,
        "osc52_set: `OSC 52 set = Drop` must shut the gate — it did not, so the \
         write gate is not being consulted"
    );
    assert_eq!(
        none_s52.clipboard_sets, OSC52_N,
        "osc52_set control: the no-engine arm falls back to the legacy authorized \
         bool and must still reach the delegate"
    );

    // -- Shell-integration gate (shell_integration_auth.rs:182) ------------
    let marks = corpus_shell_marks();
    let std_marks = run_corpus(Arm::Standard, &marks);
    let deny_marks = run_corpus(Arm::DenyAll, &marks);
    let none_marks = run_corpus(Arm::NoEngine, &marks);
    let expected_marks = (SHELL_CYCLES * MARKS_PER_CYCLE) as u64;
    assert_eq!(
        deny_marks.shell_dropped, expected_marks,
        "shell_marks: a `* = Drop` policy dropped {} marks, expected {expected_marks} \
         — the OSC 133/633 gate is not seeing every mark",
        deny_marks.shell_dropped
    );
    assert_eq!(
        std_marks.shell_dropped, 0,
        "shell_marks: under the shipping profile every nonced mark must pass \
         (the standard profile has no OSC 133/633 rule)"
    );
    assert_eq!(
        none_marks.shell_dropped, 0,
        "shell_marks control: the no-engine arm defers to the nonce check, which \
         the corpus satisfies"
    );

    // -- XTWINOPS gate (window_auth.rs:199) --------------------------------
    let csit = corpus_csi_t();
    let std_csit = run_corpus(Arm::Standard, &csit);
    let open_csit = run_corpus(Arm::AllowCsiT, &csit);
    let none_csit = run_corpus(Arm::NoEngine, &csit);
    let csit_replies = count_occurrences(&open_csit.response, b"\x1b[8;");
    assert_eq!(
        csit_replies, CSI_T_N,
        "csi_t: opening the gate with `CSI t = Execute` produced {csit_replies} \
         text-area reports, expected {CSI_T_N} — the corpus is not driving the \
         XTWINOPS gate once per dispatch"
    );
    assert!(
        std_csit.response.is_empty(),
        "csi_t: the shipping profile leaves `allow_window_ops` false, so CSI 18 t \
         must emit nothing"
    );
    assert!(
        none_csit.response.is_empty(),
        "csi_t control: the no-engine arm must emit nothing either"
    );
}

// ---------------------------------------------------------------------------
// Benchmarks
// ---------------------------------------------------------------------------

fn bench_workload(c: &mut Criterion, name: &str, corpus: &[u8]) {
    for (arm, arm_name) in [(Arm::Standard, "engine"), (Arm::NoEngine, "none")] {
        c.bench_function(&format!("policy_gate/{name}/{arm_name}"), |b| {
            let counters = Counters::default();
            b.iter_batched_ref(
                || shipping_session(arm, &counters),
                |term| {
                    term.process(black_box(corpus));
                    black_box(term.take_response());
                },
                BatchSize::PerIteration,
            );
        });
    }
}

fn policy_gate(c: &mut Criterion) {
    verify_gate_reach();

    bench_workload(c, "dsr_cpr", &corpus_dsr_cpr());
    bench_workload(c, "da_burst", &corpus_da_burst());
    bench_workload(c, "osc4_palette", &corpus_osc4_palette());
    bench_workload(c, "osc52_query", &corpus_osc52_query());
    bench_workload(c, "osc52_set", &corpus_osc52_set());
    bench_workload(c, "shell_marks", &corpus_shell_marks());
    bench_workload(c, "csi_t", &corpus_csi_t());
}

criterion_group!(benches, policy_gate);
criterion_main!(benches);
