// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The verb-matrix suite a [`SessionHost`] must pass, and the reference
//! in-memory host it is proven non-vacuous against.
//!
//! Every check drives the verbs THROUGH THE TRAIT and asserts the WIRE shape the
//! protocol promises — never the host's internals. That is what makes "aterm and
//! the daemon answer the same protocol" a checkable claim rather than a hope.
//!
//! LIVE HOSTS: the suite is meant to be pointed at a REAL session, so no check
//! may leave the user's state altered. The `select`-driving checks snapshot and
//! restore the selection (`SelectionRestore`); [`run_read_only`] is the subset
//! that never writes at all.
//!
//! A FLOOR, NOT A SHAPE CHECK: passing must mean the host HAS a session, not
//! merely that it answers about one. Three things carry that. [`check_select`]
//! ESTABLISHES a selection and reads it back through a later call, which a host
//! handing out a fresh `Terminal` per accessor cannot survive; [`run_all_declared`]
//! lets the caller name the capabilities the host is supposed to have, because
//! every capability-gated check otherwise reads `host.capabilities()` and a host
//! advertising nothing takes the easy arm of each; and — for the subset that may
//! not write at all — [`run_read_only_witnessed`] lets the caller name text the
//! session is holding, because plain [`run_read_only`] carries NO floor and cannot
//! be given one. See [`check_screen_witness`] for why that is a property of
//! reading, not a gap someone forgot to close.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use aterm_core::selection::TextSelection;
use aterm_core::terminal::Terminal;

use crate::host::{
    ChangeWait, HostCapabilities, Selector, SessionEntry, SessionHost, SessionState,
};
use crate::selection;

/// One check's verdict. `failure` is `None` on a pass and carries the observed
/// bytes on a failure, so a report names WHAT the host answered.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// Stable name of the check (a verb + the property it pins).
    pub check: &'static str,
    /// The mismatch, or `None` when the check passed.
    pub failure: Option<String>,
}

impl Outcome {
    fn pass(check: &'static str) -> Self {
        Self {
            check,
            failure: None,
        }
    }

    fn fail(check: &'static str, detail: impl Into<String>) -> Self {
        Self {
            check,
            failure: Some(detail.into()),
        }
    }

    /// Whether this check passed.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.failure.is_none()
    }
}

/// EVERY check — [`run_read_only`], then [`run_selection_state`], then
/// [`run_input`] — against `host`'s session `sid`, in order. Never panics: a host
/// under test reports, it does not abort the harness running it.
///
/// Safe on a live session: the selection checks put back what they found and the
/// input check writes a zero-byte frame. Prefer [`run_read_only`] when the session
/// must not be written at all.
pub fn run_all<H: SessionHost>(host: &H, sid: u64) -> Vec<Outcome> {
    let mut outcomes = run_read_only(host, sid);
    outcomes.extend(run_selection_state(host, sid));
    outcomes.extend(run_input(host, sid));
    outcomes
}

/// [`run_all`] against a host the caller VOUCHES for: `declared` names the
/// capabilities this host is SUPPOSED to have, and a host that answers as if it
/// lacks one FAILS — where plain [`run_all`] accepts it.
///
/// WHY THE PARAMETER EXISTS. Every capability-gated check reads
/// `host.capabilities()`, so a host advertising none takes the easy arm of each
/// (`ERR unsupported` for `copy`, an empty roster, `Some(false)` for a write) and
/// passes the matrix while serving nothing. Self-declared absence is HONEST for a
/// host that genuinely lacks a facility and a FREE PASS for one that does not —
/// only the caller knows which, so the caller says. Use this entry point wherever
/// the host's shape is known (a shipped host, a gate); [`run_all`] stays the
/// answer for a host discovered over the wire.
pub fn run_all_declared<H: SessionHost>(
    host: &H,
    sid: u64,
    declared: HostCapabilities,
) -> Vec<Outcome> {
    // First, so a report reads "the floor was not met" before the gated checks
    // below quietly pass on their easy arm.
    let mut outcomes = vec![check_declared_capabilities(host, sid, declared)];
    outcomes.extend(run_all(host, sid));
    outcomes
}

/// The checks that only READ: no selection is touched, no repaint is asked for,
/// nothing is written to the input sink or the clipboard. ALWAYS safe against a
/// live session — including one a human is mid-drag in, which restore-and-put-back
/// cannot claim.
///
/// WHAT PASSING HERE DOES NOT ESTABLISH — read this before using it as a gate.
/// Every check below is a WIRE-SHAPE check, and a host that serves a fresh empty
/// `Terminal` from every accessor and advertises no capability at all passes all
/// six (pinned by `the_unwitnessed_read_only_subset_passes_an_inert_host`). So
/// this alone is evidence that the host SPEAKS the protocol, never that it has a
/// session behind it. Two things close that, and both need the caller to say
/// something the suite cannot find out by reading: name the session's content
/// ([`run_read_only_witnessed`] — the read-only floor) and name the host's
/// capabilities ([`run_all_declared`], which writes an empty frame and so is not
/// in this subset).
pub fn run_read_only<H: SessionHost>(host: &H, sid: u64) -> Vec<Outcome> {
    vec![
        check_blocks(host, sid),
        check_blocks_json(host, sid),
        check_blocktext(host, sid),
        check_wait(host, sid),
        check_sessions(host, sid),
        check_resolve(host, sid),
    ]
}

/// [`run_read_only`] against a session the caller VOUCHES for: `witness` is text
/// they know session `sid` is holding, and a host that cannot report it FAILS —
/// where plain [`run_read_only`] accepts it. Still writes nothing, so this remains
/// the entry point for a session a human is mid-drag in.
///
/// Use this wherever the caller knows what is on the session (a gate over a
/// fixture, a driver that just read the screen); [`run_read_only`] stays the answer
/// for a session discovered over the wire, with the limits documented on it.
pub fn run_read_only_witnessed<H: SessionHost>(host: &H, sid: u64, witness: &str) -> Vec<Outcome> {
    // First, so a report reads "the floor was not met" before six shape checks
    // that an empty terminal also satisfies.
    let mut outcomes = vec![check_screen_witness(host, sid, witness)];
    outcomes.extend(run_read_only(host, sid));
    outcomes
}

/// The checks that must DRIVE the selection to pin their verb's contract. Each
/// one restores the selection it found, on its failure paths too; see
/// `SelectionRestore`.
pub fn run_selection_state<H: SessionHost>(host: &H, sid: u64) -> Vec<Outcome> {
    vec![
        check_select(host, sid),
        check_selection(host, sid),
        check_copy(host, sid),
    ]
}

/// The checks that reach the INPUT SINK. Separate from [`run_read_only`] because
/// they call the one method that can move bytes toward a child process — even
/// though the only frame they write is empty (see [`check_write_input`]).
pub fn run_input<H: SessionHost>(host: &H, sid: u64) -> Vec<Outcome> {
    vec![check_write_input(host, sid)]
}

/// Snapshots session `sid`'s selection and puts it back when dropped.
///
/// Restore rather than an opt-in flag: a suite nobody dares point at a live
/// session proves nothing about the live host, and the checks below need a KNOWN
/// selection state to assert anything.
///
/// The restore rides `Drop` because every mutating check `return`s early on the
/// first mismatch — a put-back at the end of the body would be skipped by exactly
/// the runs that matter, leaving a user whose host FAILED the suite with their
/// selection destroyed as well. A panic in a check takes the same path.
///
/// Anchors are live-screen rows, so output scrolling DURING a check ages them by
/// the scroll delta — the one gap [`run_read_only`] avoids by never writing.
struct SelectionRestore<'h, H: SessionHost> {
    host: &'h H,
    sid: u64,
    /// `None` when the host does not resolve `sid`: nothing was read, so nothing
    /// is written back.
    saved: Option<TextSelection>,
}

impl<'h, H: SessionHost> SelectionRestore<'h, H> {
    fn new(host: &'h H, sid: u64) -> Self {
        let saved = host.with_terminal(sid, |t: &Terminal| t.text_selection().clone());
        Self { host, sid, saved }
    }
}

impl<H: SessionHost> Drop for SelectionRestore<'_, H> {
    fn drop(&mut self) {
        let Some(saved) = self.saved.take() else {
            return;
        };
        // Whole-state assignment, not a `select` form: no verb can spell back an
        // in-progress drag or a semantic/block selection's exact anchors.
        if self
            .host
            .with_terminal_mut(self.sid, |t: &mut Terminal| *t.text_selection_mut() = saved)
            .is_some()
        {
            self.host.request_redraw(self.sid);
        }
    }
}

/// Every capability `declared` names is ADVERTISED by the host AND ANSWERS: a
/// roster that lists the session under test and resolves it, an input sink that
/// takes the empty frame. A host may advertise MORE than it was declared with —
/// this is a floor, not an equality.
///
/// `clipboard` is flag-only ON PURPOSE: probing it means running `copy`, which
/// over a live selection would write the user's real clipboard; [`check_copy`]'s
/// empty-selection arm is where that capability is exercised safely.
/// `frame_source` and `event_loop` are flag-only because neither is observable
/// through this trait — nothing here can see a pixel or a delivered repaint, which
/// is what aterm-gui's redraw-conformance binary exists to prove.
///
/// Writes the same EMPTY frame [`check_write_input`] does, so this check belongs
/// with [`run_input`]'s group rather than [`run_read_only`]'s.
pub fn check_declared_capabilities<H: SessionHost>(
    host: &H,
    sid: u64,
    declared: HostCapabilities,
) -> Outcome {
    const CHECK: &str = "capabilities: every declared facility is advertised and answers";
    let advertised = host.capabilities();
    for (name, want, got) in [
        (
            "frame_source",
            declared.frame_source,
            advertised.frame_source,
        ),
        ("event_loop", declared.event_loop, advertised.event_loop),
        ("clipboard", declared.clipboard, advertised.clipboard),
        ("roster", declared.roster, advertised.roster),
        ("input_sink", declared.input_sink, advertised.input_sink),
    ] {
        if want && !got {
            return Outcome::fail(CHECK, format!("{name} declared, host advertises none"));
        }
    }
    if declared.roster {
        if !host.sessions().iter().any(|e| e.sid == sid) {
            return Outcome::fail(
                CHECK,
                format!("roster declared, yet it omits session {sid}"),
            );
        }
        if host.resolve(Selector::Local(sid)) != Some(sid) {
            return Outcome::fail(
                CHECK,
                format!("roster declared, yet @{sid} does not resolve"),
            );
        }
    }
    if declared.input_sink {
        let wrote = host.write_input(sid, b"");
        if wrote != Some(true) {
            return Outcome::fail(
                CHECK,
                format!("input_sink declared, yet the empty frame answered {wrote:?}"),
            );
        }
    }
    Outcome::pass(CHECK)
}

/// THE READ-ONLY FLOOR: `witness` is text the CALLER knows session `sid` is
/// holding, and the host must report it. A host manufacturing an EMPTY `Terminal`
/// per accessor fails, which is what makes a witnessed run mean the host is
/// answering from the session the caller meant rather than a plausible blank one.
///
/// WHY THE CALLER HAS TO SUPPLY IT, rather than the suite deriving a floor.
/// Every other read-only check is satisfied by a coherent EMPTY terminal — blank
/// screen, cursor at the origin, no blocks, self-consistent across as many reads
/// as you like. That is not a defect in the host; it is EXACTLY what a real
/// session looks like the moment it opens. So any invariant strong enough to
/// reject the inert host would also reject a legitimate blank live session, and
/// this subset may not write the state it would need. The knowledge has to come
/// from outside, and only the caller has it.
///
/// SEARCHED on the live screen first, then one screenful of history, because
/// output landing DURING a run scrolls rows off — otherwise a live gate would fail
/// at random. The witness must sit within ONE row: nothing here can tell a wrapped
/// row from two rows.
///
/// LIMIT: this establishes the host is serving the session's real CONTENT, not
/// that it RETAINS what a verb writes — no read can tell a live engine from a
/// fresh one replaying the same bytes. [`check_select`]'s probe is the retention
/// floor, and it writes.
pub fn check_screen_witness<H: SessionHost>(host: &H, sid: u64, witness: &str) -> Outcome {
    const CHECK: &str = "witness: the text the caller vouches for is on the session";
    if witness.trim().is_empty() {
        return Outcome::fail(CHECK, "an empty witness asserts nothing");
    }
    let found = host.with_terminal(sid, |t: &Terminal| {
        let rows = i32::from(t.rows());
        // Live rows, then history newest-first; a `None` row is past the end of
        // history, not a match, so it is skipped rather than ending the scan.
        (0..rows).chain((1..=rows).map(|back| -back)).any(|row| {
            t.get_line_text(row, None)
                .is_some_and(|l| l.contains(witness))
        })
    });
    match found {
        Some(true) => Outcome::pass(CHECK),
        // A COUNT, not the rows: a report on a live host must not carry the
        // user's screen off the machine, and blank-vs-populated is the whole
        // diagnosis anyway.
        Some(false) => Outcome::fail(
            CHECK,
            format!(
                "{witness:?} is on no live row and no history row ({} non-blank rows on screen)",
                non_blank_rows(host, sid)
            ),
        ),
        None => Outcome::fail(CHECK, format!("the host does not serve session {sid}")),
    }
}

/// How many live rows carry anything at all — the failure detail for a missing
/// witness, and 0 for a host that does not serve `sid`.
fn non_blank_rows<H: SessionHost>(host: &H, sid: u64) -> usize {
    host.with_terminal(sid, |t: &Terminal| {
        (0..i32::from(t.rows()))
            .filter(|&row| {
                t.get_line_text(row, None)
                    .is_some_and(|l| !l.trim().is_empty())
            })
            .count()
    })
    .unwrap_or(0)
}

/// `blocks` -> `OK <n>\n` then EXACTLY `n` `block …` lines, each carrying the
/// nine positional/keyed fields in order.
pub fn check_blocks<H: SessionHost>(host: &H, sid: u64) -> Outcome {
    const CHECK: &str = "blocks: OK <n> header + n `block` lines";
    let out = selection::cmd_blocks(host, sid, "");
    let Some(n) = block_count(&out) else {
        return Outcome::fail(CHECK, out);
    };
    let lines: Vec<&str> = out.lines().skip(1).collect();
    if lines.len() != n {
        return Outcome::fail(CHECK, format!("header says {n}, body has {}", lines.len()));
    }
    const FIELDS: [&str; 7] = [
        "exit=", "prompt=", "cmd=", "out=", "end=", "cwd=", "cmdline=",
    ];
    for line in lines {
        if !line.starts_with("block ") {
            return Outcome::fail(CHECK, format!("body line is not a block: {line}"));
        }
        if let Some(missing) = FIELDS.iter().find(|f| !line.contains(*f)) {
            return Outcome::fail(CHECK, format!("line missing {missing}: {line}"));
        }
    }
    Outcome::pass(CHECK)
}

/// `blocks --json` -> the single-line `OK 1\n{json}\n` framing with a `blocks`
/// ARRAY, so a `--json` client never has to guess whether the body is framed.
pub fn check_blocks_json<H: SessionHost>(host: &H, sid: u64) -> Outcome {
    const CHECK: &str = "blocks --json: OK 1 framing, blocks array";
    let out = selection::cmd_blocks_json(host, sid, "");
    let Some(body) = out
        .strip_prefix("OK 1\n")
        .and_then(|b| b.strip_suffix('\n'))
    else {
        return Outcome::fail(CHECK, out);
    };
    if body.contains('\n') {
        return Outcome::fail(CHECK, format!("body is not one line: {body}"));
    }
    if !body.starts_with("{\"blocks\":[") || !body.ends_with("]}") {
        return Outcome::fail(CHECK, format!("not a blocks array: {body}"));
    }
    Outcome::pass(CHECK)
}

/// `blocktext` -> an explicit `ERR` for an unknown id and for a non-numeric one;
/// never an empty `OK` a client would read as "the command printed nothing".
pub fn check_blocktext<H: SessionHost>(host: &H, sid: u64) -> Outcome {
    const CHECK: &str = "blocktext: ERR on unknown id and on a bad arg";
    let unknown = selection::cmd_blocktext(host, sid, &u64::MAX.to_string());
    if unknown != "ERR no such block\n" {
        return Outcome::fail(CHECK, unknown);
    }
    let bad = selection::cmd_blocktext(host, sid, "not-a-number");
    if bad != "ERR usage: blocktext <id>\n" {
        return Outcome::fail(CHECK, bad);
    }
    Outcome::pass(CHECK)
}

/// `wait 0` -> `OK timeout` on a session with no command blocks at all;
/// otherwise the `OK complete <id> exit=…` shape. A zero deadline must still
/// answer, never park.
pub fn check_wait<H: SessionHost>(host: &H, sid: u64) -> Outcome {
    const CHECK: &str = "wait 0: OK timeout with no blocks, else OK complete";
    let out = selection::cmd_wait(host, sid, "0");
    let no_blocks = block_count(&selection::cmd_blocks(host, sid, "")) == Some(0);
    if no_blocks {
        return if out == "OK timeout\n" {
            Outcome::pass(CHECK)
        } else {
            Outcome::fail(CHECK, out)
        };
    }
    if out == "OK timeout\n" || (out.starts_with("OK complete ") && out.contains(" exit=")) {
        Outcome::pass(CHECK)
    } else {
        Outcome::fail(CHECK, out)
    }
}

/// `sessions` -> a NON-EMPTY roster, STRICTLY ascending by sid (the order the wire
/// lists them), carrying the session under test, and every row spelling an `@<id>`
/// that [`Selector::parse`] reads back as a stable id — an all-digit `id` would
/// address some other session's LOCAL sid instead.
///
/// A host that keeps no roster ([`HostCapabilities::roster`] false) must answer
/// EMPTY. Pinning that arm is the point: it is the only legal empty roster, so a
/// caller can never read one as "this host has no sessions".
pub fn check_sessions<H: SessionHost>(host: &H, sid: u64) -> Outcome {
    const CHECK: &str = "sessions: ascending non-empty roster, or empty without one";
    let roster = host.sessions();
    if !host.capabilities().roster {
        return if roster.is_empty() {
            Outcome::pass(CHECK)
        } else {
            Outcome::fail(
                CHECK,
                format!("no roster kept, yet {} listed", roster.len()),
            )
        };
    }
    if roster.is_empty() {
        return Outcome::fail(CHECK, "empty roster from a host that keeps one");
    }
    if let Some(pair) = roster.windows(2).find(|w| w[0].sid >= w[1].sid) {
        return Outcome::fail(
            CHECK,
            format!("not ascending: {} then {}", pair[0].sid, pair[1].sid),
        );
    }
    for e in &roster {
        if Selector::parse(&e.id) != Some(Selector::Id(&e.id)) {
            return Outcome::fail(
                CHECK,
                format!("sid {} has an unaddressable id: {:?}", e.sid, e.id),
            );
        }
        if e.parent.as_ref().is_some_and(String::is_empty) {
            return Outcome::fail(CHECK, format!("sid {} carries an empty parent", e.sid));
        }
    }
    if !roster.iter().any(|e| e.sid == sid) {
        return Outcome::fail(
            CHECK,
            format!("roster omits the session under test ({sid})"),
        );
    }
    Outcome::pass(CHECK)
}

/// `resolve` -> every roster row round-trips through BOTH selector forms (`@<sid>`
/// and `@<id>` both answer that row's sid), and a selector NO row carries answers
/// `None`. The fail-closed half is the load-bearing one: it is what stops an
/// unknown target quietly becoming whichever session the host happens to hold.
///
/// A host with no roster resolves nothing at all — including its own session's sid.
pub fn check_resolve<H: SessionHost>(host: &H, sid: u64) -> Outcome {
    const CHECK: &str = "resolve: roster rows round-trip, unknown selectors fail closed";
    let roster = host.sessions();
    let unknown_local = unrostered_sid(host, sid);
    // Derived, not assumed: extended until no row carries it.
    let mut unknown_id = String::from("s-conformance-absent");
    while roster.iter().any(|e| e.id == unknown_id) {
        unknown_id.push('x');
    }
    if !host.capabilities().roster {
        let own = host.resolve(Selector::Local(sid));
        let bogus = host.resolve(Selector::Id(&unknown_id));
        return if own.is_none() && bogus.is_none() {
            Outcome::pass(CHECK)
        } else {
            Outcome::fail(
                CHECK,
                format!("no roster kept, yet resolved {own:?}/{bogus:?}"),
            )
        };
    }
    for e in &roster {
        if host.resolve(Selector::Local(e.sid)) != Some(e.sid) {
            return Outcome::fail(CHECK, format!("@{} does not resolve to itself", e.sid));
        }
        if host.resolve(Selector::Id(&e.id)) != Some(e.sid) {
            return Outcome::fail(
                CHECK,
                format!("@{} does not resolve to sid {}", e.id, e.sid),
            );
        }
    }
    if let Some(hit) = host.resolve(Selector::Local(unknown_local)) {
        return Outcome::fail(CHECK, format!("unknown @{unknown_local} resolved to {hit}"));
    }
    if let Some(hit) = host.resolve(Selector::Id(&unknown_id)) {
        return Outcome::fail(CHECK, format!("unknown @{unknown_id} resolved to {hit}"));
    }
    Outcome::pass(CHECK)
}

/// A sid no roster row carries, so `resolve`'s fail-closed arm cannot accidentally
/// name a REAL neighbour session and read its miss as a lie.
fn unrostered_sid<H: SessionHost>(host: &H, sid: u64) -> u64 {
    let roster = host.sessions();
    let mut candidate = sid.wrapping_add(1);
    while roster.iter().any(|e| e.sid == candidate) {
        candidate = candidate.wrapping_add(1);
    }
    candidate
}

/// A sid this host does not SERVE — a different question from "unrostered", and the
/// one [`check_write_input`]'s foreign arm actually asks. `with_terminal` is the
/// read-only oracle, since it and `write_input` are defined by the same "serves
/// `sid`"; neither the roster nor `sid + 1` can be assumed to answer it.
///
/// `None` when nothing probed is foreign: a host answering for sids nobody gave it
/// cannot demonstrate failing closed at all.
fn unserved_sid<H: SessionHost>(host: &H, sid: u64) -> Option<u64> {
    let roster = host.sessions();
    let serves = |c: u64| host.with_terminal(c, |_: &Terminal| ()).is_some();
    // A rostered SIBLING first: `resolve` hands those out fleet-wide, so it is the
    // misroute a driver can actually make.
    if let Some(sibling) = roster
        .iter()
        .map(|e| e.sid)
        .find(|&c| c != sid && !serves(c))
    {
        return Some(sibling);
    }
    // Otherwise past the last rostered sid, so a host serving its WHOLE roster (a
    // daemon does) still has one to refuse — a scan from `sid` alone finds none.
    // Bounded so it also ends against a host that answers for every sid.
    const PROBES: u64 = 64;
    let top = roster.iter().map(|e| e.sid).max().unwrap_or(sid).max(sid);
    (1..=PROBES)
        .map(|n| top.wrapping_add(n))
        .find(|&c| !serves(c))
}

/// `select` -> `OK\n` for the forms that apply, the EXACT usage / bad-arg strings
/// for the forms that do not (the error text is wire, not diagnostics), and — the
/// part no stateless host can fake — a selection ESTABLISHED by one call still
/// being there for the NEXT one.
///
/// THE STATE PROBE is the `extend` pair: `select extend` answers `ERR no
/// selection` on a cleared selection and `OK` on an established one, so it reads
/// the session's selection state back THROUGH THE WIRE without needing any text on
/// the screen (a live session may legitimately be blank). A host that hands out a
/// fresh `Terminal` per accessor answers `ERR no selection` for both and fails
/// here — which is what makes passing this suite mean the host HAS a session
/// rather than merely answers about one.
///
/// Drives the selection; restores the prior one however it exits.
pub fn check_select<H: SessionHost>(host: &H, sid: u64) -> Outcome {
    const CHECK: &str = "select: OK on clear, exact usage/bad-args, established selection persists";
    let _restore = SelectionRestore::new(host, sid);
    let cleared = selection::cmd_select(host, sid, "clear");
    if cleared != "OK\n" {
        return Outcome::fail(CHECK, cleared);
    }
    let empty = selection::cmd_select(host, sid, "");
    if !empty.starts_with("ERR usage: select <r1> <c1> <r2> <c2> |") {
        return Outcome::fail(CHECK, empty);
    }
    let word = selection::cmd_select(host, sid, "word");
    if word != "ERR usage: select word <r> <c>\n" {
        return Outcome::fail(CHECK, word);
    }
    let bad = selection::cmd_select(host, sid, "a b c d");
    if bad != "ERR bad args\n" {
        return Outcome::fail(CHECK, bad);
    }
    // Still cleared: the forms above are all rejections, so nothing since the
    // `clear` selected anything. The arm a stateless host gets right by accident.
    let unselected = selection::cmd_select(host, sid, "extend 0 1");
    if unselected != "ERR no selection\n" {
        return Outcome::fail(CHECK, format!("extend with nothing selected: {unselected}"));
    }
    let established = selection::cmd_select(host, sid, "0 0 0 4");
    if established != "OK\n" {
        return Outcome::fail(CHECK, format!("establishing a range: {established}"));
    }
    let extended = selection::cmd_select(host, sid, "extend 0 5");
    if extended != "OK\n" {
        return Outcome::fail(
            CHECK,
            format!("the selection the previous call established was gone: {extended}"),
        );
    }
    Outcome::pass(CHECK)
}

/// `selection` -> `OK 0\n` with nothing selected (the same empty framing `text`
/// uses), checked right after a `select clear` so the state is known — and then
/// the ROUND TRIP: a row of the session's OWN text, selected and read back as
/// that text. A host with no session between calls answers `OK 0` here.
///
/// The round trip is conditional on the screen already HOLDING text because the
/// suite may not write any into a live session; a blank session skips it, and
/// [`check_select`]'s `extend` probe is the state check that always runs.
///
/// Drives the selection; restores the prior one however it exits.
pub fn check_selection<H: SessionHost>(host: &H, sid: u64) -> Outcome {
    const CHECK: &str = "selection: OK 0 after clear, own text reads back";
    let _restore = SelectionRestore::new(host, sid);
    let cleared = selection::cmd_select(host, sid, "clear");
    if cleared != "OK\n" {
        return Outcome::fail(CHECK, cleared);
    }
    let out = selection::cmd_selection(host, sid);
    if out != "OK 0\n" {
        return Outcome::fail(CHECK, out);
    }
    let Some((row, text)) = first_text_row(host, sid) else {
        return Outcome::pass(CHECK);
    };
    let selected = selection::cmd_select(host, sid, &format!("line {row}"));
    if selected != "OK\n" {
        return Outcome::fail(CHECK, format!("select line {row}: {selected}"));
    }
    let read_back = selection::cmd_selection(host, sid);
    // A live session can scroll between the read and the select, ageing the row,
    // so an exact mismatch is only the HOST's when the row still reads the same;
    // when it moved, a host holding a session still answers a NON-EMPTY selection.
    let moved = first_text_row(host, sid).is_none_or(|(r, t)| r != row || t != text);
    if read_back == format!("OK 1\n{text}\n") || (moved && read_back.starts_with("OK 1\n")) {
        Outcome::pass(CHECK)
    } else {
        Outcome::fail(
            CHECK,
            format!("row {row} ({text:?}) selected, read back {read_back:?}"),
        )
    }
}

/// The first live-screen row carrying non-blank text, with the text `select
/// line` will select there. The only content a check can select and read back
/// WITHOUT writing to the session — hence the round trip above being
/// conditional rather than assumed.
///
/// LOGICAL line, not physical row: `select line` selects the whole soft-wrapped
/// run, and the copy joins its rows with NO newline, so the expectation is the
/// rows of `logical_line_span` concatenated exactly the way
/// `selection_to_string` concatenates them. Comparing against the clicked row
/// alone would fail this check on any screen whose first line happens to wrap.
fn first_text_row<H: SessionHost>(host: &H, sid: u64) -> Option<(i32, String)> {
    host.with_terminal(sid, |t: &Terminal| {
        (0..i32::from(t.rows())).find_map(|row| {
            let text = t.get_line_text(row, None)?;
            if text.trim().is_empty() {
                return None;
            }
            let (first, last) = t.logical_line_span(row);
            let mut joined = String::new();
            for r in first..=last {
                joined.push_str(&t.get_line_text(r, None).unwrap_or_default());
            }
            Some((row, joined))
        })
    })?
}

/// `copy` -> `ERR unsupported` on a host with no clipboard, `OK 0` on one with
/// a clipboard and nothing selected. This is the capability contract: the host
/// that cannot do it SAYS so instead of reporting a write that went nowhere.
///
/// Drives the selection; restores the prior one however it exits. The EMPTY
/// selection is not incidental — `copy` leaves the clipboard untouched on that
/// arm, so the suite can never clobber a live system clipboard. Do not "improve"
/// this check by selecting something first.
pub fn check_copy<H: SessionHost>(host: &H, sid: u64) -> Outcome {
    const CHECK: &str = "copy: ERR unsupported without a clipboard, else OK 0";
    if !host.capabilities().clipboard {
        let out = selection::cmd_copy(host, sid);
        return if out == "ERR unsupported\n" {
            Outcome::pass(CHECK)
        } else {
            Outcome::fail(CHECK, out)
        };
    }
    let _restore = SelectionRestore::new(host, sid);
    let cleared = selection::cmd_select(host, sid, "clear");
    if cleared != "OK\n" {
        return Outcome::fail(CHECK, cleared);
    }
    let out = selection::cmd_copy(host, sid);
    if out == "OK 0\n" {
        Outcome::pass(CHECK)
    } else {
        Outcome::fail(CHECK, out)
    }
}

/// `write_input` -> the three answers a driver has to be able to trust:
///   * a sid this host does not serve -> `None`, never its own session under a
///     borrowed number (the misroute [`SessionHost`]'s header warns about);
///   * its own sid with NO sink -> `Some(false)`, never a false OK;
///   * its own sid WITH a sink -> `Some(true)`.
///
/// Every frame written here is EMPTY, and that is not a gap to "improve": this
/// suite is meant to run against a session a HUMAN is using, and no non-empty byte
/// is safe to inject into a live shell. An empty frame still traverses the whole
/// write path — sid check, then sink — which is where a wrong answer lives. That
/// bytes actually LAND is proven per host, next to that host's real sink
/// (`write_input_reaches_the_sink` here, on every platform).
///
/// The foreign sid is PROBED, not assumed ([`unserved_sid`]), so this arm means the
/// same thing on a session-scoped host and on a fleet one.
pub fn check_write_input<H: SessionHost>(host: &H, sid: u64) -> Outcome {
    const CHECK: &str = "write_input: refuses a foreign sid, never a false OK";
    let Some(foreign) = unserved_sid(host, sid) else {
        return Outcome::fail(
            CHECK,
            format!("serves every sid probed near {sid}: nothing here can fail closed"),
        );
    };
    if let Some(wrote) = host.write_input(foreign, b"") {
        return Outcome::fail(
            CHECK,
            format!("foreign sid {foreign} was served: Some({wrote})"),
        );
    }
    let sink = host.capabilities().input_sink;
    match host.write_input(sid, b"") {
        Some(wrote) if wrote == sink => Outcome::pass(CHECK),
        other => Outcome::fail(
            CHECK,
            format!("input_sink={sink} but the write answered {other:?}"),
        ),
    }
}

/// The `<n>` from a `blocks` header, or `None` if the reply is not that shape.
fn block_count(reply: &str) -> Option<usize> {
    reply
        .lines()
        .next()?
        .strip_prefix("OK ")?
        .parse::<usize>()
        .ok()
}

/// Shared change counter + condvar: the producer bumps and broadcasts, a
/// [`MemoryWait`] parks on a value newer than the one it registered at.
type ChangeSignal = Arc<(Mutex<u64>, Condvar)>;

/// The reference [`SessionHost`]: one `Arc<Mutex<Terminal>>`, a counted redraw
/// no-op, a condvar change signal, a recording input sink and an in-memory
/// clipboard.
///
/// It exists to keep [`run_all`] honest — a suite only ever run against the host
/// it was written for proves nothing — and to give a new host implementor
/// something small to read.
pub struct MemoryHost {
    sid: u64,
    /// The stable fabric id this host rosters `sid` under — the `s-<20 hex>` shape
    /// a real session mints, derived from `sid` so a check can predict it.
    id: String,
    term: Arc<Mutex<Terminal>>,
    /// Everything the verbs have written to the session's input sink, in order.
    /// A recording sink, NOT a loopback: echoing the bytes back into the engine
    /// would fake a shell that is not there.
    input: Mutex<Vec<u8>>,
    clipboard: Mutex<Option<String>>,
    capabilities: HostCapabilities,
    changed: ChangeSignal,
    redraws: AtomicU64,
}

impl MemoryHost {
    /// A host owning one 24x80 session numbered `sid`, WITH a clipboard, a roster
    /// and an input sink.
    #[must_use]
    pub fn new(sid: u64) -> Self {
        Self {
            sid,
            id: format!("s-{sid:020x}"),
            term: Arc::new(Mutex::new(Terminal::new(24, 80))),
            input: Mutex::new(Vec::new()),
            clipboard: Mutex::new(None),
            capabilities: HostCapabilities {
                frame_source: false,
                event_loop: false,
                clipboard: true,
                roster: true,
                input_sink: true,
            },
            changed: Arc::new((Mutex::new(0), Condvar::new())),
            redraws: AtomicU64::new(0),
        }
    }

    /// The same host advertising NO clipboard — the `ERR unsupported` side of
    /// the capability contract.
    #[must_use]
    pub fn without_clipboard(sid: u64) -> Self {
        let mut host = Self::new(sid);
        host.capabilities.clipboard = false;
        host
    }

    /// The same host keeping NO roster: `sessions` is empty and `resolve` answers
    /// nothing. The shape `aterm-gui`'s session-scoped host has, kept here so the
    /// empty-roster arm of [`check_sessions`] is proven, not assumed.
    #[must_use]
    pub fn without_roster(sid: u64) -> Self {
        let mut host = Self::new(sid);
        host.capabilities.roster = false;
        host
    }

    /// The same host with NO input sink — the `Some(false)` side of
    /// [`SessionHost::write_input`]'s contract, where a lie would look like `OK`.
    #[must_use]
    pub fn without_input_sink(sid: u64) -> Self {
        let mut host = Self::new(sid);
        host.capabilities.input_sink = false;
        host
    }

    /// Feed bytes to the session's engine and signal the change, standing in for
    /// a PTY output burst.
    pub fn feed(&self, bytes: &[u8]) {
        self.term
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .process(bytes);
        self.notify_change();
    }

    /// Wake every [`ChangeWait`] registered on this host.
    pub fn notify_change(&self) {
        let (lock, cv) = &*self.changed;
        let mut g = lock.lock().unwrap_or_else(|p| p.into_inner());
        *g = g.wrapping_add(1);
        drop(g);
        cv.notify_all();
    }

    /// A second handle on the same session, for a producer thread.
    #[must_use]
    pub fn producer(&self) -> (Arc<Mutex<Terminal>>, ChangeSignal) {
        (Arc::clone(&self.term), Arc::clone(&self.changed))
    }

    /// The session's stable id, as [`SessionHost::sessions`] reports it.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Every byte the verbs have written to the input sink so far.
    #[must_use]
    pub fn input(&self) -> Vec<u8> {
        self.input.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// The clipboard's current contents.
    #[must_use]
    pub fn clipboard(&self) -> Option<String> {
        self.clipboard
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// How many redraws the verbs have asked for.
    #[must_use]
    pub fn redraws(&self) -> u64 {
        self.redraws.load(Ordering::Relaxed)
    }
}

/// A registration on [`MemoryHost`]'s change counter, holding the value it saw
/// at registration so a bump in the register→recheck gap is not lost.
struct MemoryWait {
    changed: ChangeSignal,
    registered_at: u64,
}

impl ChangeWait for MemoryWait {
    fn wait(&self, timeout: Duration) -> bool {
        let (lock, cv) = &*self.changed;
        let g = lock.lock().unwrap_or_else(|p| p.into_inner());
        if *g != self.registered_at {
            return true;
        }
        let (g, res) = cv
            .wait_timeout(g, timeout)
            .unwrap_or_else(|p| p.into_inner());
        !res.timed_out() && *g != self.registered_at
    }
}

impl SessionHost for MemoryHost {
    fn capabilities(&self) -> HostCapabilities {
        self.capabilities
    }

    fn sessions(&self) -> Vec<SessionEntry> {
        if !self.capabilities.roster {
            return Vec::new();
        }
        vec![SessionEntry {
            sid: self.sid,
            id: self.id.clone(),
            parent: None,
            state: SessionState::Alive,
            // The engine's OSC title, so a check that feeds one sees it here.
            title: self
                .term
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .title()
                .to_string(),
            has_meta: false,
        }]
    }

    fn resolve(&self, selector: Selector<'_>) -> Option<u64> {
        if !self.capabilities.roster {
            return None;
        }
        let hit = match selector {
            Selector::Local(n) => n == self.sid,
            Selector::Id(id) => id == self.id,
        };
        hit.then_some(self.sid)
    }

    fn with_terminal<R>(&self, sid: u64, f: impl FnOnce(&Terminal) -> R) -> Option<R> {
        (sid == self.sid).then(|| f(&self.term.lock().unwrap_or_else(|p| p.into_inner())))
    }

    fn with_terminal_mut<R>(&self, sid: u64, f: impl FnOnce(&mut Terminal) -> R) -> Option<R> {
        (sid == self.sid).then(|| f(&mut self.term.lock().unwrap_or_else(|p| p.into_inner())))
    }

    fn write_input(&self, sid: u64, bytes: &[u8]) -> Option<bool> {
        // The sid is checked BEFORE the sink, so a foreign sid can never reach it.
        (sid == self.sid).then(|| {
            self.capabilities.input_sink && {
                self.input
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .extend_from_slice(bytes);
                true
            }
        })
    }

    fn request_redraw(&self, sid: u64) {
        if sid == self.sid {
            self.redraws.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn subscribe(&self, sid: u64) -> Box<dyn ChangeWait + '_> {
        // A foreign sid parks on a signal NOTHING bumps: it times out rather than
        // waking on this session's changes.
        let changed = if sid == self.sid {
            Arc::clone(&self.changed)
        } else {
            Arc::new((Mutex::new(0), Condvar::new()))
        };
        // Bound to a NAME rather than acquired through `changed.0` directly: the
        // L0 lock-order census resolves a lock's identity from its receiver, and a
        // tuple field has no name to resolve — the site becomes a per-site UNKNOWN
        // node, which by construction can never participate in a cycle, so a real
        // ABBA through this mutex would be invisible to the gate.
        //
        // `change_epoch` and not `lock`: identities are keyed by receiver NAME
        // across the whole scan set, so a generic name merges this counter with
        // every other `lock` in the workspace. That merge is not hypothetical —
        // it is half of the false {lock, spill} cycle this crate's extraction
        // triggered on 2026-08-01.
        let (change_epoch, _) = &*changed;
        let registered_at = *change_epoch.lock().unwrap_or_else(|p| p.into_inner());
        Box::new(MemoryWait {
            changed,
            registered_at,
        })
    }

    fn clipboard_set(&self, text: &str) -> bool {
        *self.clipboard.lock().unwrap_or_else(|p| p.into_inner()) = Some(text.to_string());
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The suite passes against the reference host — and, since a suite that
    /// passes vacuously proves nothing, every check must have RUN.
    #[test]
    fn reference_host_passes_the_matrix() {
        let host = MemoryHost::new(7);
        let outcomes = run_all(&host, 7);
        assert_eq!(outcomes.len(), 10, "the matrix lost a check");
        for o in &outcomes {
            assert!(o.passed(), "{}: {:?}", o.check, o.failure);
        }
        // `run_all` is the three groups, whole: no check belongs to none of them.
        let grouped: Vec<&str> = run_read_only(&host, 7)
            .iter()
            .chain(run_selection_state(&host, 7).iter())
            .chain(run_input(&host, 7).iter())
            .map(|o| o.check)
            .collect();
        assert_eq!(
            grouped,
            outcomes.iter().map(|o| o.check).collect::<Vec<_>>()
        );
    }

    /// The suite is safe to point at a session someone is USING: every check
    /// leaves the selection as it found it, and none of them writes a clipboard.
    #[test]
    fn the_matrix_leaves_a_live_selection_untouched() {
        let host = MemoryHost::new(0);
        host.feed(b"hello world");
        assert_eq!(selection::cmd_select(&host, 0, "0 0 0 4"), "OK\n");
        let before = selection::cmd_selection(&host, 0);
        assert!(before.contains("hello"), "{before}");
        for o in run_all(&host, 0) {
            assert!(o.passed(), "{}: {:?}", o.check, o.failure);
        }
        assert_eq!(selection::cmd_selection(&host, 0), before);
        assert_eq!(
            host.clipboard(),
            None,
            "the matrix must not write a clipboard"
        );
        assert!(
            host.input().is_empty(),
            "the matrix must not inject a byte into the session"
        );
    }

    /// The read-only subset writes NOTHING — not the selection, not the input
    /// sink, not even a repaint request. That is what makes it unconditionally
    /// safe, including against a session being dragged in right now. The WITNESSED
    /// variant is held to the same bar: the floor is worthless if buying it costs
    /// the property the entry point exists for.
    #[test]
    fn the_read_only_subset_writes_nothing() {
        let host = MemoryHost::new(0);
        host.feed(b"hello world");
        assert_eq!(selection::cmd_select(&host, 0, "0 0 0 4"), "OK\n");
        let before = selection::cmd_selection(&host, 0);
        let redraws = host.redraws();
        let outcomes = run_read_only(&host, 0);
        assert_eq!(outcomes.len(), 6, "the read-only subset lost a check");
        let witnessed = run_read_only_witnessed(&host, 0, "hello world");
        assert_eq!(witnessed.len(), 7, "the witnessed subset lost a check");
        for o in outcomes.iter().chain(witnessed.iter()) {
            assert!(o.passed(), "{}: {:?}", o.check, o.failure);
        }
        assert_eq!(selection::cmd_selection(&host, 0), before);
        assert_eq!(host.redraws(), redraws, "a read-only check repainted");
        assert!(host.input().is_empty(), "a read-only check wrote input");
    }

    /// Answers READS from a decoy terminal that always has a selection, so
    /// `check_selection` fails at its LAST branch — after its `select clear`.
    /// Writes still land on the real session, which is where the restore shows up.
    struct ReadsFromADecoy {
        inner: MemoryHost,
        decoy: Mutex<Terminal>,
    }

    impl SessionHost for ReadsFromADecoy {
        fn capabilities(&self) -> HostCapabilities {
            self.inner.capabilities()
        }

        fn sessions(&self) -> Vec<SessionEntry> {
            self.inner.sessions()
        }

        fn resolve(&self, selector: Selector<'_>) -> Option<u64> {
            self.inner.resolve(selector)
        }

        fn with_terminal<R>(&self, sid: u64, f: impl FnOnce(&Terminal) -> R) -> Option<R> {
            self.inner
                .resolve(Selector::Local(sid))
                .map(|_| f(&self.decoy.lock().unwrap_or_else(|p| p.into_inner())))
        }

        fn with_terminal_mut<R>(&self, sid: u64, f: impl FnOnce(&mut Terminal) -> R) -> Option<R> {
            self.inner.with_terminal_mut(sid, f)
        }

        fn write_input(&self, sid: u64, bytes: &[u8]) -> Option<bool> {
            self.inner.write_input(sid, bytes)
        }

        fn request_redraw(&self, sid: u64) {
            self.inner.request_redraw(sid);
        }

        fn subscribe(&self, sid: u64) -> Box<dyn ChangeWait + '_> {
            self.inner.subscribe(sid)
        }

        fn clipboard_set(&self, text: &str) -> bool {
            self.inner.clipboard_set(text)
        }
    }

    /// A check that FAILS still puts the selection back — the case a restore at
    /// the end of the body would miss, and the one where the user is already
    /// having a bad day.
    #[test]
    fn a_failing_check_still_restores_the_selection() {
        let inner = MemoryHost::new(0);
        inner.feed(b"hello world");
        assert_eq!(selection::cmd_select(&inner, 0, "0 0 0 4"), "OK\n");
        let before = selection::cmd_selection(&inner, 0);
        let mut decoy = Terminal::new(24, 80);
        decoy.process(b"hello world");
        selection::select_word(&mut decoy, 0, 0);
        let host = ReadsFromADecoy {
            inner,
            decoy: Mutex::new(decoy),
        };
        let outcome = check_selection(&host, 0);
        assert!(!outcome.passed(), "the decoy host must fail this check");
        assert_eq!(selection::cmd_selection(&host.inner, 0), before);
    }

    /// The restore also survives a PANIC mid-check: `Drop` is the only put-back
    /// no control flow can step around.
    #[test]
    fn the_restore_survives_a_panic() {
        let host = MemoryHost::new(0);
        host.feed(b"hello world");
        assert_eq!(selection::cmd_select(&host, 0, "0 0 0 4"), "OK\n");
        let before = selection::cmd_selection(&host, 0);
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _restore = SelectionRestore::new(&host, 0);
            selection::cmd_select(&host, 0, "clear");
            panic!("a check exploded mid-mutation");
        }));
        std::panic::set_hook(hook);
        assert!(caught.is_err(), "the panic must reach the caller");
        assert_eq!(selection::cmd_selection(&host, 0), before);
    }

    /// The clipboard-less host answers `ERR unsupported` for `copy` — the
    /// capability flag is load-bearing, not decorative.
    #[test]
    fn a_host_without_a_clipboard_refuses_copy() {
        let host = MemoryHost::without_clipboard(7);
        for o in run_all(&host, 7) {
            assert!(o.passed(), "{}: {:?}", o.check, o.failure);
        }
        assert_eq!(selection::cmd_copy(&host, 7), "ERR unsupported\n");
    }

    /// A sid the host does not resolve is refused by every verb, rather than
    /// answered with whatever session the host does have.
    #[test]
    fn an_unresolved_sid_is_refused() {
        let host = MemoryHost::new(7);
        for reply in [
            selection::cmd_blocks(&host, 9, ""),
            selection::cmd_blocks_json(&host, 9, ""),
            selection::cmd_blocktext(&host, 9, "0"),
            selection::cmd_wait(&host, 9, "0"),
            selection::cmd_select(&host, 9, "clear"),
            selection::cmd_selection(&host, 9),
            selection::cmd_copy(&host, 9),
        ] {
            assert_eq!(reply, "ERR no such session\n");
        }
    }

    /// The seam itself refuses the foreign sid — not just the verbs above. The
    /// dangerous one is `write_input`: a `resolve` on one host followed by a write
    /// on another must not land keystrokes in the session this host DOES hold.
    #[test]
    fn a_foreign_sid_never_reaches_the_session_the_host_holds() {
        let host = MemoryHost::new(7);
        assert_eq!(host.write_input(9, b"rm -rf /\r"), None);
        assert!(host.input().is_empty(), "a foreign write landed anyway");
        assert!(host.with_terminal(9, |t: &Terminal| t.rows()).is_none());
        assert!(
            host.with_terminal_mut(9, |t: &mut Terminal| t.rows())
                .is_none()
        );
        let redraws = host.redraws();
        host.request_redraw(9);
        assert_eq!(
            host.redraws(),
            redraws,
            "a foreign sid repainted this session"
        );
    }

    /// Bytes written for the host's OWN sid reach the sink, and the reply is the
    /// honest `Some(true)` — the half `check_write_input` cannot assert against a
    /// live session (see its doc), proven here where the session is ours.
    #[test]
    fn write_input_reaches_the_sink() {
        let host = MemoryHost::new(0);
        assert_eq!(host.write_input(0, b"echo hi\r"), Some(true));
        assert_eq!(host.input(), b"echo hi\r".to_vec());
        // The empty frame the conformance check uses moves nothing, and still
        // reports the sink honestly.
        assert_eq!(host.write_input(0, b""), Some(true));
        assert_eq!(host.input(), b"echo hi\r".to_vec());
    }

    /// A host with NO input sink reports the write did not happen. `Some(true)`
    /// here would be a wire `OK` for bytes that went nowhere.
    #[test]
    fn a_host_without_a_sink_never_answers_ok() {
        let host = MemoryHost::without_input_sink(3);
        for o in run_all(&host, 3) {
            assert!(o.passed(), "{}: {:?}", o.check, o.failure);
        }
        assert_eq!(host.write_input(3, b"echo hi\r"), Some(false));
        assert!(host.input().is_empty());
        assert_eq!(
            host.write_input(9, b"echo hi\r"),
            None,
            "sid first, sink second"
        );
    }

    /// A host that keeps NO roster lists nothing and resolves nothing — and says so
    /// in its capabilities, which is what keeps its empty `sessions()` from reading
    /// as "this host has no sessions".
    #[test]
    fn a_rosterless_host_lists_nothing_and_resolves_nothing() {
        let host = MemoryHost::without_roster(5);
        for o in run_all(&host, 5) {
            assert!(o.passed(), "{}: {:?}", o.check, o.failure);
        }
        assert!(host.sessions().is_empty());
        assert_eq!(host.resolve(Selector::Local(5)), None);
        assert_eq!(host.resolve(Selector::Id(host.id())), None);
        // The session is still SERVED, which is exactly the distinction: no index,
        // not no sessions.
        assert_eq!(selection::cmd_blocks(&host, 5, ""), "OK 0\n");
    }

    /// A host with NO STATE: a fresh `Terminal` per accessor, so nothing a verb
    /// writes is ever read back; no roster, no clipboard, no sink, no event loop; a
    /// no-op redraw and a wait that only times out. `seed` is what its throwaway
    /// engine is fed, so one shape covers both the wholly inert host and one that
    /// has content on screen and still forgets every selection.
    struct ForgetfulHost {
        sid: u64,
        seed: &'static [u8],
    }

    impl ForgetfulHost {
        fn fresh(&self) -> Terminal {
            let mut t = Terminal::new(24, 80);
            t.process(self.seed);
            t
        }
    }

    /// The inert host's whole event story: a subscription nothing ever wakes.
    struct NeverWakes;

    impl ChangeWait for NeverWakes {
        fn wait(&self, _timeout: Duration) -> bool {
            false
        }
    }

    impl SessionHost for ForgetfulHost {
        fn capabilities(&self) -> HostCapabilities {
            HostCapabilities::default()
        }

        fn sessions(&self) -> Vec<SessionEntry> {
            Vec::new()
        }

        fn resolve(&self, _selector: Selector<'_>) -> Option<u64> {
            None
        }

        fn with_terminal<R>(&self, sid: u64, f: impl FnOnce(&Terminal) -> R) -> Option<R> {
            (sid == self.sid).then(|| f(&self.fresh()))
        }

        fn with_terminal_mut<R>(&self, sid: u64, f: impl FnOnce(&mut Terminal) -> R) -> Option<R> {
            (sid == self.sid).then(|| f(&mut self.fresh()))
        }

        fn write_input(&self, sid: u64, _bytes: &[u8]) -> Option<bool> {
            (sid == self.sid).then_some(false)
        }

        fn request_redraw(&self, _sid: u64) {}

        fn subscribe(&self, _sid: u64) -> Box<dyn ChangeWait + '_> {
            Box::new(NeverWakes)
        }

        fn clipboard_set(&self, _text: &str) -> bool {
            false
        }
    }

    /// THE FLOOR. A host that preserves NO session state — every answer legitimately
    /// SHAPED, none of it remembered — must fail, or passing the matrix says nothing
    /// about the host having a session at all. `select`'s state probe is what
    /// catches it on a blank screen, where no text exists to read back.
    #[test]
    fn a_host_that_keeps_no_session_state_fails_the_matrix() {
        let host = ForgetfulHost { sid: 0, seed: b"" };
        let outcomes = run_all(&host, 0);
        let failed: Vec<&str> = outcomes
            .iter()
            .filter(|o| !o.passed())
            .map(|o| o.check)
            .collect();
        assert!(
            failed.iter().any(|c| c.starts_with("select:")),
            "a host holding no session state passed: {failed:?}"
        );
    }

    /// MEASURED, so the honest limit on the docs above is not a guess: the
    /// unwitnessed read-only subset passes a wholly inert host, blank AND seeded.
    #[test]
    fn the_unwitnessed_read_only_subset_passes_an_inert_host() {
        for seed in [b"".as_slice(), b"hello world".as_slice()] {
            let host = ForgetfulHost { sid: 0, seed };
            for o in run_read_only(&host, 0) {
                assert!(o.passed(), "{}: {:?}", o.check, o.failure);
            }
        }
    }

    /// NON-VACUITY FOR THE ROSTER AND INPUT CHECKS, and the honest limit on them.
    /// The inert host's empty roster, dead `resolve` and refused write are LEGAL
    /// while it advertises none of the three — that is exactly the session-scoped
    /// contract `aterm-gui`'s `GuiHost::new` ships, so no invariant strong enough to
    /// fail it here could spare that host either. What closes it is the CALLER
    /// vouching: declare the roster or the sink and the same host fails, naming
    /// which. (A host that DOES advertise them and answers empty anyway is caught
    /// with no declaration needed — `each_roster_and_input_arm_goes_red`.)
    #[test]
    fn an_inert_host_passes_only_until_the_caller_vouches_for_it() {
        let host = ForgetfulHost { sid: 0, seed: b"" };
        for o in [
            check_sessions(&host, 0),
            check_resolve(&host, 0),
            check_write_input(&host, 0),
        ] {
            assert!(o.passed(), "claiming nothing is honest: {}", o.check);
        }
        for (capability, declared) in [
            (
                "roster",
                HostCapabilities {
                    roster: true,
                    ..HostCapabilities::default()
                },
            ),
            (
                "input_sink",
                HostCapabilities {
                    input_sink: true,
                    ..HostCapabilities::default()
                },
            ),
        ] {
            let outcomes = run_all_declared(&host, 0, declared);
            let failure = outcomes
                .iter()
                .find(|o| !o.passed())
                .unwrap_or_else(|| panic!("{capability} declared, the inert host still passed"));
            let detail = failure.failure.as_deref().unwrap_or_default();
            assert!(
                detail.contains(capability),
                "{capability} declared: {detail}"
            );
        }
    }

    /// With text on the screen the READ-BACK half bites too: the row selects, and
    /// the next call reads an engine that never saw the selection.
    #[test]
    fn a_forgetful_host_cannot_read_its_own_selection_back() {
        let host = ForgetfulHost {
            sid: 0,
            seed: b"hello world",
        };
        let outcomes = run_all(&host, 0);
        let failed: Vec<&str> = outcomes
            .iter()
            .filter(|o| !o.passed())
            .map(|o| o.check)
            .collect();
        assert!(
            failed.iter().any(|c| c.starts_with("selection:")),
            "a host that forgets its selection passed the read-back: {failed:?}"
        );
    }

    /// THE READ-ONLY FLOOR. The inert host the unwitnessed subset waves through
    /// FAILS the moment the caller names what the session is supposed to hold.
    #[test]
    fn an_inert_host_fails_a_witnessed_read_only_run() {
        let host = ForgetfulHost { sid: 0, seed: b"" };
        let outcomes = run_read_only_witnessed(&host, 0, "hello world");
        assert_eq!(outcomes.len(), 7, "the witnessed subset lost a check");
        let failed: Vec<&Outcome> = outcomes.iter().filter(|o| !o.passed()).collect();
        let checks: Vec<&str> = failed.iter().map(|o| o.check).collect();
        assert!(
            checks.iter().any(|c| c.starts_with("witness:")),
            "a host with no session passed a witnessed run: {checks:?}"
        );
        let detail = failed[0].failure.as_deref().unwrap_or_default();
        assert!(
            detail.contains("0 non-blank rows"),
            "the report must name the blank screen: {detail}"
        );
    }

    /// The LIMIT documented on [`check_screen_witness`], MEASURED: a stateless host
    /// REPLAYING the right bytes satisfies the witness, because no read can tell it
    /// from a live engine. Retention stays `check_select`'s floor — and still bites.
    #[test]
    fn a_forgetful_host_replaying_the_witness_satisfies_it() {
        let host = ForgetfulHost {
            sid: 0,
            seed: b"hello world",
        };
        assert!(check_screen_witness(&host, 0, "hello world").passed());
        let failed: Vec<&str> = run_all(&host, 0)
            .iter()
            .filter(|o| !o.passed())
            .map(|o| o.check)
            .collect();
        assert!(
            failed.iter().any(|c| c.starts_with("select:")),
            "the mutating floor stopped catching a forgetful host: {failed:?}"
        );
    }

    /// An empty witness is REFUSED. A caller with nothing to vouch for must not get
    /// the floor's name on a check that asserted nothing.
    #[test]
    fn an_empty_witness_is_refused() {
        let host = MemoryHost::new(0);
        host.feed(b"hello world");
        for witness in ["", "   ", "\n"] {
            let o = check_screen_witness(&host, 0, witness);
            assert!(!o.passed(), "an empty witness passed: {witness:?}");
        }
    }

    /// The reference host, holding real content, passes a witnessed run whole — and
    /// a sid it does NOT serve fails rather than answering from the session it has.
    #[test]
    fn the_reference_host_passes_a_witnessed_run() {
        let host = MemoryHost::new(7);
        host.feed(b"hello world");
        let outcomes = run_read_only_witnessed(&host, 7, "hello world");
        assert_eq!(outcomes.len(), 7, "the witnessed subset lost a check");
        for o in &outcomes {
            assert!(o.passed(), "{}: {:?}", o.check, o.failure);
        }
        assert!(!check_screen_witness(&host, 9, "hello world").passed());
    }

    /// A witness the session has SCROLLED OFF is still found in history: output
    /// landing mid-run must not fail a live gate.
    #[test]
    fn a_witness_that_scrolled_off_is_found_in_history() {
        let host = MemoryHost::new(0);
        host.feed(b"hello world\n");
        host.feed(&b"\n".repeat(30));
        assert!(
            first_text_row(&host, 0).is_none(),
            "the witness must be off the LIVE screen for this to prove anything"
        );
        let o = check_screen_witness(&host, 0, "hello world");
        assert!(o.passed(), "{:?}", o.failure);
    }

    /// The capability profile the reference host actually implements is met, and a
    /// declared run is the matrix plus exactly the one floor check.
    #[test]
    fn the_reference_host_meets_its_declared_profile() {
        let host = MemoryHost::new(7);
        let outcomes = run_all_declared(&host, 7, MEMORY_HOST_PROFILE);
        assert_eq!(outcomes.len(), 11, "the declared matrix lost a check");
        for o in &outcomes {
            assert!(o.passed(), "{}: {:?}", o.check, o.failure);
        }
    }

    /// What `MemoryHost::new` really supports — the profile a caller who knows the
    /// host would declare.
    const MEMORY_HOST_PROFILE: HostCapabilities = HostCapabilities {
        frame_source: false,
        event_loop: false,
        clipboard: true,
        roster: true,
        input_sink: true,
    };

    /// SELF-DECLARED ABSENCE IS NO LONGER A FREE PASS. Each host below answers
    /// honestly for what it advertises — so plain `run_all` accepts it, and must —
    /// but the caller knows it is supposed to have the facility, and saying so
    /// fails it.
    #[test]
    fn a_declared_capability_the_host_answers_as_absent_fails() {
        for (capability, host) in [
            ("roster", MemoryHost::without_roster(5)),
            ("clipboard", MemoryHost::without_clipboard(5)),
            ("input_sink", MemoryHost::without_input_sink(5)),
        ] {
            for o in run_all(&host, 5) {
                assert!(
                    o.passed(),
                    "un-declared, a host that says so is legal: {}: {:?}",
                    o.check,
                    o.failure
                );
            }
            let outcomes = run_all_declared(&host, 5, MEMORY_HOST_PROFILE);
            let failure = outcomes
                .iter()
                .find(|o| !o.passed())
                .unwrap_or_else(|| panic!("{capability} declared but absent, still passed"));
            let detail = failure.failure.as_deref().unwrap_or_default();
            assert!(
                detail.contains(capability),
                "{capability} declared but absent, reported as: {detail}"
            );
        }
    }

    /// The roster is the `sessions` wire line's field set, addressable both ways:
    /// `@<sid>` and `@<id>` resolve to the same session.
    #[test]
    fn the_roster_carries_the_documented_fields() {
        let host = MemoryHost::new(4);
        host.feed(b"\x1b]0;my-title\x07");
        let roster = host.sessions();
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0].sid, 4);
        assert_eq!(roster[0].id, host.id());
        assert_eq!(roster[0].parent, None);
        assert_eq!(roster[0].state, SessionState::Alive);
        assert_eq!(roster[0].title, "my-title");
        assert!(!roster[0].has_meta);
        assert_eq!(host.resolve(Selector::Local(4)), Some(4));
        assert_eq!(host.resolve(Selector::Id(host.id())), Some(4));
        assert_eq!(host.resolve(Selector::Id("s-nope")), None);
    }

    /// The verbs are wired to the host's real terminal and clipboard: drive a
    /// selection over live content, read it back, and copy it.
    #[test]
    fn select_reads_back_and_copies_live_content() {
        let host = MemoryHost::new(0);
        host.feed(b"hello world");
        assert_eq!(selection::cmd_select(&host, 0, "0 0 0 4"), "OK\n");
        let sel = selection::cmd_selection(&host, 0);
        assert!(sel.starts_with("OK 1\n") && sel.contains("hello"), "{sel}");
        assert_eq!(selection::cmd_copy(&host, 0), "OK 5\n");
        assert_eq!(host.clipboard().as_deref(), Some("hello"));
        assert!(host.redraws() >= 1, "select must ask for a repaint");
    }

    /// `wait` parks on the host's change signal and reports the completion that
    /// lands while it is parked — the event-driven path, not a poll.
    #[test]
    fn wait_reports_a_completion_that_lands_while_parked() {
        let host = MemoryHost::new(0);
        host.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07sleep\n\x1b]133;C\x07");
        let (term, changed) = host.producer();
        let h = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            term.lock()
                .unwrap_or_else(|p| p.into_inner())
                .process(b"\x1b]133;D;0\x07");
            let (lock, cv) = &*changed;
            let mut g = lock.lock().unwrap_or_else(|p| p.into_inner());
            *g = g.wrapping_add(1);
            drop(g);
            cv.notify_all();
        });
        let resp = selection::cmd_wait(&host, 0, "5000");
        h.join().unwrap();
        assert!(
            resp.starts_with("OK complete ") && resp.contains("exit=0"),
            "{resp}"
        );
    }

    /// `blocks`/`blocktext` over real OSC-133 content: the shapes `check_blocks`
    /// asserts are reached with a NON-EMPTY block list, so that check is not
    /// trivially satisfied by a session with nothing in it.
    #[test]
    fn the_block_checks_hold_on_a_populated_session() {
        let host = MemoryHost::new(0);
        host.feed(
            b"\x1b]133;A\x07$ \x1b]633;E;echo hi\x07\x1b]133;B\x07echo hi\n\x1b]133;C\x07hi\n\x1b]133;D;0\x07",
        );
        let listed = selection::cmd_blocks(&host, 0, "");
        assert!(listed.starts_with("OK 1\n"), "{listed}");
        assert!(listed.contains("cmdline=echo%20hi"), "{listed}");
        let txt = selection::cmd_blocktext(&host, 0, "0");
        assert!(txt.starts_with("OK ") && txt.contains("hi"), "{txt}");
        for o in [check_blocks(&host, 0), check_blocks_json(&host, 0)] {
            assert!(o.passed(), "{}: {:?}", o.check, o.failure);
        }
    }

    /// ONE contract broken, everything else honest — so every failing arm of the
    /// roster/resolve/write checks can be WATCHED going red rather than merely
    /// written. A check nobody has seen fire is decoration.
    #[derive(Clone, Copy, Debug)]
    enum Deviation {
        /// The control: nothing bent, so a red below is the deviation's doing.
        Honest,
        /// Keeps no roster by its own capabilities, lists anyway.
        ListsWithoutARoster,
        /// THE PRODUCTION BUG: advertises a roster, answers with an empty one.
        RosterDeclaredButEmpty,
        /// Newest first — the wire's ascending order, inverted.
        RosterDescending,
        /// An all-digit `id`, which `@<id>` reads as some other session's LOCAL sid.
        RosterIdReadsAsALocalSid,
        /// `Some("")` for a parent: a family-tree field naming nothing.
        RosterParentIsEmpty,
        /// A populated roster that leaves out the session under test.
        RosterOmitsTheSession,
        /// Keeps no roster, resolves anyway.
        ResolvesWithoutARoster,
        /// `@<sid>` misses a row the roster carries.
        LocalSelectorMissesItsRow,
        /// `@<id>` misses a row the roster carries.
        IdSelectorMissesItsRow,
        /// An unknown `@<n>` falls back to the session this host holds.
        UnknownLocalFallsBack,
        /// An unknown `@<id>` falls back to the session this host holds.
        UnknownIdFallsBack,
        /// Answers writes for ANY sid — the misroute `SessionHost`'s header warns of.
        ServesForeignWrites,
        /// Advertises a sink, then reports every write as not having happened.
        SinkDeclaredButDrops,
        /// Advertises no sink and answers OK anyway.
        SinkAbsentButAnswersOk,
    }

    /// An honest [`MemoryHost`] with exactly one [`Deviation`] applied.
    struct DeviantHost {
        inner: MemoryHost,
        deviation: Deviation,
    }

    impl DeviantHost {
        fn new(sid: u64, deviation: Deviation) -> Self {
            Self {
                inner: MemoryHost::new(sid),
                deviation,
            }
        }
    }

    impl SessionHost for DeviantHost {
        fn capabilities(&self) -> HostCapabilities {
            let mut caps = self.inner.capabilities();
            match self.deviation {
                Deviation::ListsWithoutARoster | Deviation::ResolvesWithoutARoster => {
                    caps.roster = false;
                }
                Deviation::SinkAbsentButAnswersOk => caps.input_sink = false,
                _ => {}
            }
            caps
        }

        fn sessions(&self) -> Vec<SessionEntry> {
            let mut roster = self.inner.sessions();
            match self.deviation {
                Deviation::RosterDeclaredButEmpty => Vec::new(),
                Deviation::RosterDescending => {
                    let mut newer = roster[0].clone();
                    newer.sid = roster[0].sid.wrapping_add(1);
                    newer.id = format!("{}b", roster[0].id);
                    vec![newer, roster.remove(0)]
                }
                Deviation::RosterIdReadsAsALocalSid => {
                    roster[0].id = "12".to_string();
                    roster
                }
                Deviation::RosterParentIsEmpty => {
                    roster[0].parent = Some(String::new());
                    roster
                }
                Deviation::RosterOmitsTheSession => {
                    roster[0].sid = roster[0].sid.wrapping_add(1);
                    roster
                }
                _ => roster,
            }
        }

        fn resolve(&self, selector: Selector<'_>) -> Option<u64> {
            let held = Some(self.inner.sid);
            match (self.deviation, selector) {
                (Deviation::ResolvesWithoutARoster, _)
                | (Deviation::UnknownLocalFallsBack, Selector::Local(_))
                | (Deviation::UnknownIdFallsBack, Selector::Id(_)) => held,
                (Deviation::LocalSelectorMissesItsRow, Selector::Local(_))
                | (Deviation::IdSelectorMissesItsRow, Selector::Id(_)) => None,
                _ => self.inner.resolve(selector),
            }
        }

        fn with_terminal<R>(&self, sid: u64, f: impl FnOnce(&Terminal) -> R) -> Option<R> {
            self.inner.with_terminal(sid, f)
        }

        fn with_terminal_mut<R>(&self, sid: u64, f: impl FnOnce(&mut Terminal) -> R) -> Option<R> {
            self.inner.with_terminal_mut(sid, f)
        }

        fn write_input(&self, sid: u64, bytes: &[u8]) -> Option<bool> {
            let honest = self.inner.write_input(sid, bytes);
            match self.deviation {
                // The lie is the ANSWER to a sid it does not serve, so the served
                // sid keeps its true reply.
                Deviation::ServesForeignWrites => Some(honest.unwrap_or(true)),
                Deviation::SinkDeclaredButDrops => honest.map(|_| false),
                Deviation::SinkAbsentButAnswersOk => honest.map(|_| true),
                _ => honest,
            }
        }

        fn request_redraw(&self, sid: u64) {
            self.inner.request_redraw(sid);
        }

        fn subscribe(&self, sid: u64) -> Box<dyn ChangeWait + '_> {
            self.inner.subscribe(sid)
        }

        fn clipboard_set(&self, text: &str) -> bool {
            self.inner.clipboard_set(text)
        }
    }

    /// EVERY failing arm of `sessions`/`resolve`/`write_input`, watched going RED,
    /// and the report naming which contract broke — a check that cannot name the
    /// fault sends its reader hunting.
    #[test]
    fn each_roster_and_input_arm_goes_red() {
        type Check = fn(&DeviantHost, u64) -> Outcome;
        const SESSIONS: Check = check_sessions::<DeviantHost>;
        const RESOLVE: Check = check_resolve::<DeviantHost>;
        const WRITE: Check = check_write_input::<DeviantHost>;

        // The control: the wrapper is honest everywhere else, so each red below is
        // the deviation's doing and not the scaffolding's.
        for o in run_all(&DeviantHost::new(7, Deviation::Honest), 7) {
            assert!(o.passed(), "the undeviated host must pass: {}", o.check);
        }

        for (deviation, check, fragment) in [
            (Deviation::ListsWithoutARoster, SESSIONS, "no roster kept"),
            (
                Deviation::RosterDeclaredButEmpty,
                SESSIONS,
                "empty roster from a host that keeps one",
            ),
            (Deviation::RosterDescending, SESSIONS, "not ascending"),
            (
                Deviation::RosterIdReadsAsALocalSid,
                SESSIONS,
                "unaddressable id",
            ),
            (Deviation::RosterParentIsEmpty, SESSIONS, "empty parent"),
            (
                Deviation::RosterOmitsTheSession,
                SESSIONS,
                "omits the session under test",
            ),
            (
                Deviation::ResolvesWithoutARoster,
                RESOLVE,
                "no roster kept, yet resolved",
            ),
            (
                Deviation::LocalSelectorMissesItsRow,
                RESOLVE,
                "does not resolve to itself",
            ),
            (
                Deviation::IdSelectorMissesItsRow,
                RESOLVE,
                "does not resolve to sid",
            ),
            (Deviation::UnknownLocalFallsBack, RESOLVE, "unknown @8"),
            (
                Deviation::UnknownIdFallsBack,
                RESOLVE,
                "unknown @s-conformance-absent",
            ),
            (Deviation::ServesForeignWrites, WRITE, "was served"),
            (
                Deviation::SinkDeclaredButDrops,
                WRITE,
                "input_sink=true but the write answered",
            ),
            (
                Deviation::SinkAbsentButAnswersOk,
                WRITE,
                "input_sink=false but the write answered",
            ),
        ] {
            let host = DeviantHost::new(7, deviation);
            let outcome = check(&host, 7);
            assert!(!outcome.passed(), "{deviation:?} passed {}", outcome.check);
            let detail = outcome.failure.as_deref().unwrap_or_default();
            assert!(
                detail.contains(fragment),
                "{deviation:?} reported: {detail}"
            );
        }
    }

    /// A host that ROSTERS one set of sids and SERVES another — the two are not the
    /// same set, which is the whole fleet-vs-session split. One engine stands in for
    /// every served session, since only WHICH sids are served is under test here.
    struct ServesTheseSids {
        /// What the per-session methods answer for.
        served: Vec<u64>,
        /// What `sessions`/`resolve` publish. EMPTY means this host keeps NO roster
        /// (`roster: false`) — a different claim from serving nothing.
        rostered: Vec<u64>,
        term: Mutex<Terminal>,
    }

    impl ServesTheseSids {
        fn new(
            served: impl IntoIterator<Item = u64>,
            rostered: impl IntoIterator<Item = u64>,
        ) -> Self {
            Self {
                served: served.into_iter().collect(),
                rostered: rostered.into_iter().collect(),
                term: Mutex::new(Terminal::new(24, 80)),
            }
        }

        fn stable_id(sid: u64) -> String {
            format!("s-{sid:020x}")
        }
    }

    impl SessionHost for ServesTheseSids {
        fn capabilities(&self) -> HostCapabilities {
            HostCapabilities {
                roster: !self.rostered.is_empty(),
                input_sink: true,
                ..HostCapabilities::default()
            }
        }

        fn sessions(&self) -> Vec<SessionEntry> {
            self.rostered
                .iter()
                .map(|&sid| SessionEntry {
                    sid,
                    id: Self::stable_id(sid),
                    parent: None,
                    state: SessionState::Alive,
                    title: String::new(),
                    has_meta: false,
                })
                .collect()
        }

        fn resolve(&self, selector: Selector<'_>) -> Option<u64> {
            self.rostered.iter().copied().find(|&sid| match selector {
                Selector::Local(n) => n == sid,
                Selector::Id(id) => id == Self::stable_id(sid),
            })
        }

        fn with_terminal<R>(&self, sid: u64, f: impl FnOnce(&Terminal) -> R) -> Option<R> {
            self.served
                .contains(&sid)
                .then(|| f(&self.term.lock().unwrap_or_else(|p| p.into_inner())))
        }

        fn with_terminal_mut<R>(&self, sid: u64, f: impl FnOnce(&mut Terminal) -> R) -> Option<R> {
            self.served
                .contains(&sid)
                .then(|| f(&mut self.term.lock().unwrap_or_else(|p| p.into_inner())))
        }

        fn write_input(&self, sid: u64, _bytes: &[u8]) -> Option<bool> {
            self.served.contains(&sid).then_some(true)
        }

        fn request_redraw(&self, _sid: u64) {}

        fn subscribe(&self, _sid: u64) -> Box<dyn ChangeWait + '_> {
            Box::new(NeverWakes)
        }

        fn clipboard_set(&self, _text: &str) -> bool {
            false
        }
    }

    /// The foreign sid is the SEAM's answer, not the roster's: a host that keeps no
    /// index must not be failed for serving the sibling a roster-derived guess
    /// picks.
    #[test]
    fn the_foreign_sid_probe_skips_a_sibling_the_same_host_serves() {
        let host = ServesTheseSids::new([4, 5], []);
        assert_eq!(
            unserved_sid(&host, 4),
            Some(6),
            "5 is served, so it is not foreign"
        );
        let o = check_write_input(&host, 4);
        assert!(o.passed(), "{:?}", o.failure);
    }

    /// …and when a ROSTERED sibling is not served, that is the sid to probe with:
    /// `resolve` hands those out fleet-wide, so refusing one is the misroute a
    /// driver can actually reach — the shipped `GuiHost`'s shape.
    #[test]
    fn the_foreign_sid_probe_prefers_a_rostered_sibling() {
        let host = ServesTheseSids::new([1], 0..=2);
        assert_eq!(unserved_sid(&host, 1), Some(0));
        let o = check_write_input(&host, 1);
        assert!(o.passed(), "{:?}", o.failure);
    }

    /// A daemon-shaped host SERVES every session it rosters, so the foreign sid has
    /// to be sought past the roster's end — a fixed scan up from the session under
    /// test finds only siblings and would fail an honest fleet. The roster checks
    /// run over the long roster here too.
    #[test]
    fn a_fleet_host_serving_its_whole_roster_still_has_a_foreign_sid() {
        let host = ServesTheseSids::new(0..=99, 0..=99);
        assert_eq!(unserved_sid(&host, 0), Some(100));
        for o in [
            check_write_input(&host, 0),
            check_sessions(&host, 0),
            check_resolve(&host, 0),
        ] {
            assert!(o.passed(), "{}: {:?}", o.check, o.failure);
        }
    }

    /// The sid check FORGOTTEN: every accessor answers for whatever number it is
    /// handed — the misroute itself.
    struct ServesEverySid(Mutex<Terminal>);

    impl SessionHost for ServesEverySid {
        fn capabilities(&self) -> HostCapabilities {
            HostCapabilities {
                input_sink: true,
                ..HostCapabilities::default()
            }
        }

        fn sessions(&self) -> Vec<SessionEntry> {
            Vec::new()
        }

        fn resolve(&self, _selector: Selector<'_>) -> Option<u64> {
            None
        }

        fn with_terminal<R>(&self, _sid: u64, f: impl FnOnce(&Terminal) -> R) -> Option<R> {
            Some(f(&self.0.lock().unwrap_or_else(|p| p.into_inner())))
        }

        fn with_terminal_mut<R>(&self, _sid: u64, f: impl FnOnce(&mut Terminal) -> R) -> Option<R> {
            Some(f(&mut self.0.lock().unwrap_or_else(|p| p.into_inner())))
        }

        fn write_input(&self, _sid: u64, _bytes: &[u8]) -> Option<bool> {
            Some(true)
        }

        fn request_redraw(&self, _sid: u64) {}

        fn subscribe(&self, _sid: u64) -> Box<dyn ChangeWait + '_> {
            Box::new(NeverWakes)
        }

        fn clipboard_set(&self, _text: &str) -> bool {
            false
        }
    }

    /// Serving every sid is not a pass for want of a foreign one to probe with:
    /// nothing on such a host can fail closed, which is the finding.
    #[test]
    fn a_host_that_serves_every_sid_fails_the_write_check() {
        let host = ServesEverySid(Mutex::new(Terminal::new(24, 80)));
        assert_eq!(unserved_sid(&host, 0), None);
        let o = check_write_input(&host, 0);
        assert!(!o.passed(), "the misrouting host passed");
        let detail = o.failure.as_deref().unwrap_or_default();
        assert!(detail.contains("serves every sid probed"), "{detail}");
    }
}
