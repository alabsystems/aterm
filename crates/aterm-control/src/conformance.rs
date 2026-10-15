// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The verb-matrix suite a [`SessionHost`] must pass, and the reference
//! in-memory host it is proven non-vacuous against.
//!
//! Every check drives the verbs through the trait and asserts the wire shape
//! the protocol promises, never the host's internals, so "aterm and the daemon
//! answer the same protocol" is checkable.
//!
//! The suite may run against a live session, so no check leaves state altered:
//! the `select` checks snapshot and restore the selection (`SelectionRestore`),
//! and [`run_read_only`] never writes at all.
//!
//! Passing must mean the host has a session, not merely that it answers about
//! one. [`check_select`] establishes a selection and reads it back through a
//! later call; [`run_all_declared`] lets the caller name the capabilities the
//! host should have (a host advertising none otherwise takes the easy arm of
//! each gated check); and [`run_read_only_witnessed`] lets the caller name text
//! the session holds, since plain [`run_read_only`] cannot carry a floor (see
//! [`check_screen_witness`]).

#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(test)]
use std::sync::{Arc, Condvar, Mutex};
#[cfg(test)]
use std::time::Duration;

use aterm_core::selection::TextSelection;
use aterm_core::terminal::Terminal;

#[cfg(test)]
use crate::host::{ChangeWait, SessionEntry, SessionState};
use crate::host::{HostCapabilities, Selector, SessionHost};
use crate::selection;

/// One check's verdict. `failure` is `None` on a pass and carries the observed
/// bytes on a failure, so a report names what the host answered.
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

/// Every check ([`run_read_only`], then [`run_selection_state`], then
/// [`run_input`]) against `host`'s session `sid`, in order. Never panics.
///
/// Safe on a live session: the selection checks restore what they found and
/// the input check writes a zero-byte frame. Use [`run_read_only`] when the
/// session must not be written at all.
pub fn run_all<H: SessionHost>(host: &H, sid: u64) -> Vec<Outcome> {
    let mut outcomes = run_read_only(host, sid);
    outcomes.extend(run_selection_state(host, sid));
    outcomes.extend(run_input(host, sid));
    outcomes
}

/// [`run_all`] against a host whose capabilities the caller vouches for:
/// `declared` names the capabilities it should have, and a host answering as if
/// it lacks one fails.
///
/// Every gated check reads `host.capabilities()`, so a host advertising none
/// takes the easy arm of each (`ERR unsupported` for `copy`, an empty roster,
/// `Some(false)` for a write). Only the caller knows whether that absence is
/// honest. Use this wherever the host's shape is known (a shipped host, a
/// gate); [`run_all`] is for a host discovered over the wire.
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

/// The checks that only read: no selection, repaint request, input or
/// clipboard write. Always safe against a live session, including one a human
/// is mid-drag in.
///
/// These are wire-shape checks only: a host serving a fresh empty `Terminal`
/// from every accessor with no capabilities passes all six (pinned by
/// `the_unwitnessed_read_only_subset_passes_an_inert_host`). So this shows the
/// host speaks the protocol, not that it has a session. For that, name the
/// session's content ([`run_read_only_witnessed`]) or the host's capabilities
/// ([`run_all_declared`], which writes an empty frame).
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

/// [`run_read_only`] against a session the caller vouches for: `witness` is
/// text session `sid` is known to hold, and a host that cannot report it fails.
/// Still writes nothing. Use it wherever the caller knows the session's
/// content (a gate over a fixture, a driver that just read the screen).
pub fn run_read_only_witnessed<H: SessionHost>(host: &H, sid: u64, witness: &str) -> Vec<Outcome> {
    // First, so a report reads "the floor was not met" before six shape checks
    // that an empty terminal also satisfies.
    let mut outcomes = vec![check_screen_witness(host, sid, witness)];
    outcomes.extend(run_read_only(host, sid));
    outcomes
}

/// The checks that must drive the selection to pin their verb's contract. Each
/// restores the selection it found, on failure paths too (`SelectionRestore`).
pub fn run_selection_state<H: SessionHost>(host: &H, sid: u64) -> Vec<Outcome> {
    vec![
        check_select(host, sid),
        check_selection(host, sid),
        check_copy(host, sid),
    ]
}

/// The checks that reach the input sink, separate from [`run_read_only`]
/// because they call the one method that can move bytes toward a child process
/// (though the only frame written is empty; see [`check_write_input`]).
pub fn run_input<H: SessionHost>(host: &H, sid: u64) -> Vec<Outcome> {
    vec![check_write_input(host, sid)]
}

/// Snapshots session `sid`'s selection and puts it back when dropped, so the
/// suite can run against a live session with a known selection state.
///
/// The restore rides `Drop` because every mutating check returns early on the
/// first mismatch (and a panic takes the same path); a put-back at the end of
/// the body would be skipped exactly when a check fails.
///
/// Anchors are live-screen rows, so output scrolling during a check ages them
/// by the scroll delta.
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

/// Every capability `declared` names is advertised by the host and answers: a
/// roster listing and resolving the session under test, an input sink taking
/// the empty frame. A host may advertise more (a floor, not an equality).
///
/// `clipboard` is flag-only because probing it would run `copy` and could
/// write the user's real clipboard ([`check_copy`]'s empty-selection arm
/// exercises it safely). `frame_source` and `event_loop` are flag-only because
/// neither is observable through this trait (aterm-gui's redraw-conformance
/// binary covers them).
///
/// Writes the same empty frame [`check_write_input`] does, so it belongs with
/// [`run_input`].
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

/// The read-only floor: `witness` is text the caller knows session `sid`
/// holds, and the host must report it. A host manufacturing an empty
/// `Terminal` per accessor fails.
///
/// The caller must supply it because every other read-only check is satisfied
/// by a coherent empty terminal, which is also what a real session looks like
/// the moment it opens; no invariant can reject the inert host without
/// rejecting that, and this subset may not write.
///
/// Searched on the live screen, then one screenful of history (output landing
/// during a run scrolls rows off). The witness must fit within one row.
///
/// Limit: this shows the host serves the session's real content, not that it
/// retains what a verb writes (a fresh engine replaying the same bytes looks
/// the same). [`check_select`]'s probe is the retention floor, and it writes.
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
        // A count, not the rows: a report must not carry the user's screen off
        // the machine.
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

/// `blocks` -> `OK <n>\n` then exactly `n` `block …` lines, each carrying the
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
/// array, so a `--json` client never has to guess whether the body is framed.
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

/// `sessions` -> a non-empty roster, strictly ascending by sid, carrying the
/// session under test, with every row's `@<id>` reading back through
/// [`Selector::parse`] as a stable id (an all-digit `id` would address another
/// session's local sid).
///
/// A host keeping no roster ([`HostCapabilities::roster`] false) must answer
/// empty, the only legal empty roster, so an empty roster never reads as "no
/// sessions".
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

/// `resolve` -> every roster row round-trips through both selector forms
/// (`@<sid>` and `@<id>`), and a selector no row carries answers `None`, so an
/// unknown target never becomes whichever session the host holds. A host with
/// no roster resolves nothing, including its own sid.
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
/// name a real neighbour session and read its miss as a lie.
fn unrostered_sid<H: SessionHost>(host: &H, sid: u64) -> u64 {
    let roster = host.sessions();
    let mut candidate = sid.wrapping_add(1);
    while roster.iter().any(|e| e.sid == candidate) {
        candidate = candidate.wrapping_add(1);
    }
    candidate
}

/// A sid this host does not serve (not the same as "unrostered"), as
/// [`check_write_input`]'s foreign arm needs. `with_terminal` is the read-only
/// oracle, since it and `write_input` share the "serves `sid`" definition.
/// `None` when every probed sid is served.
fn unserved_sid<H: SessionHost>(host: &H, sid: u64) -> Option<u64> {
    let roster = host.sessions();
    let serves = |c: u64| host.with_terminal(c, |_: &Terminal| ()).is_some();
    // A rostered sibling first: `resolve` hands those out fleet-wide, so it is the
    // misroute a driver can actually make.
    if let Some(sibling) = roster
        .iter()
        .map(|e| e.sid)
        .find(|&c| c != sid && !serves(c))
    {
        return Some(sibling);
    }
    // Then past the last rostered sid, so a host serving its whole roster (a
    // daemon) still has one to refuse; bounded for a host serving every sid.
    const PROBES: u64 = 64;
    let top = roster.iter().map(|e| e.sid).max().unwrap_or(sid).max(sid);
    (1..=PROBES)
        .map(|n| top.wrapping_add(n))
        .find(|&c| !serves(c))
}

/// `select` -> `OK\n` for the forms that apply, the exact usage / bad-arg
/// strings for those that do not (the error text is wire), and a selection
/// established by one call still present for the next.
///
/// The state probe is the `extend` pair: `select extend` answers
/// `ERR no selection` when cleared and `OK` when established, reading the
/// state back through the wire without needing screen text. A host handing out
/// a fresh `Terminal` per accessor answers `ERR no selection` for both and
/// fails.
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
    // Still cleared: every form above was a rejection.
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

/// `selection` -> `OK 0\n` with nothing selected (checked right after
/// `select clear`), then the round trip: a row of the session's own text,
/// selected and read back. A host with no session between calls answers `OK 0`.
///
/// The round trip needs text already on screen, since the suite may not write
/// any; a blank session skips it ([`check_select`]'s probe always runs).
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
    // A live session can scroll between the read and the select; an exact
    // mismatch is the host's only if the row still reads the same, otherwise a
    // non-empty selection suffices.
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

/// The first live-screen row with non-blank text, and the text `select line`
/// selects there: the only content a check can select and read back without
/// writing.
///
/// A logical line, not a physical row: `select line` selects the whole
/// soft-wrapped run and the copy joins its rows with no newline, so the
/// expectation concatenates `logical_line_span`'s rows as
/// `selection_to_string` does.
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
/// a clipboard and nothing selected.
///
/// Drives the selection; restores the prior one however it exits. The selection
/// must stay empty: `copy` then leaves the clipboard untouched, so a live
/// system clipboard is never clobbered.
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

/// `write_input` -> the three answers a driver must be able to trust:
///   * a sid this host does not serve -> `None`, never its own session under a
///     borrowed number;
///   * its own sid with no sink -> `Some(false)`;
///   * its own sid with a sink -> `Some(true)`.
///
/// Every frame written is empty, since no byte is safe to inject into a live
/// shell; an empty frame still traverses the sid check and the sink. That
/// bytes land is proven per host (`write_input_reaches_the_sink` here).
///
/// The foreign sid is probed ([`unserved_sid`]), so this works for
/// session-scoped and fleet hosts alike.
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
#[cfg(test)]
type ChangeSignal = Arc<(Mutex<u64>, Condvar)>;

/// The reference [`SessionHost`]: one `Arc<Mutex<Terminal>>`, a counted redraw
/// no-op, a condvar change signal, a recording input sink and an in-memory
/// clipboard.
///
/// It keeps [`run_all`] honest (a suite run only against the host it was
/// written for proves nothing) and is small enough for a new implementor to
/// read. Test-only: downstream hosts run the suite against themselves.
#[cfg(test)]
pub struct MemoryHost {
    sid: u64,
    /// The stable fabric id this host rosters `sid` under — the `s-<20 hex>` shape
    /// a real session mints, derived from `sid` so a check can predict it.
    id: String,
    term: Arc<Mutex<Terminal>>,
    /// Everything written to the session's input sink, in order. A recording
    /// sink, not a loopback (echoing into the engine would fake a shell).
    input: Mutex<Vec<u8>>,
    clipboard: Mutex<Option<String>>,
    capabilities: HostCapabilities,
    changed: ChangeSignal,
    redraws: AtomicU64,
}

#[cfg(test)]
impl MemoryHost {
    /// A host owning one 24x80 session numbered `sid`, with a clipboard, a roster
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

    /// The same host keeping no roster (`aterm-gui`'s session-scoped shape), so
    /// the empty-roster arm of [`check_sessions`] is exercised.
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
#[cfg(test)]
struct MemoryWait {
    changed: ChangeSignal,
    registered_at: u64,
}

#[cfg(test)]
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

#[cfg(test)]
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
        // The sid is checked before the sink, so a foreign sid can never reach it.
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
        // A foreign sid parks on a signal nothing bumps: it times out rather than
        // waking on this session's changes.
        let changed = if sid == self.sid {
            Arc::clone(&self.changed)
        } else {
            Arc::new((Mutex::new(0), Condvar::new()))
        };
        // Bound to a name rather than used through `changed.0`: the L0
        // lock-order census resolves a lock's identity from its receiver name,
        // and a tuple field would be an unknown node that can never join a
        // cycle. `change_epoch`, not `lock`, because identities are keyed by
        // name across the scan set and a generic name would merge with every
        // other `lock`.
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

    /// The suite is safe to point at a session someone is using: every check
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

    /// The read-only subset, witnessed or not, writes nothing: not the
    /// selection, the input sink, or a repaint request.
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

    /// Answers reads from a decoy terminal that always has a selection, so
    /// `check_selection` fails at its last branch, after its `select clear`.
    /// Writes land on the real session, where the restore shows.
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

    /// A failing check still restores the selection.
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

    /// The restore also survives a panic mid-check: `Drop` is the only put-back
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

    /// `blocks --json` gives where the shell's `133;B` put the input's start
    /// (the column after the prompt), whatever was typed after it, which tells
    /// typeahead from the prompt. Negative control: a prompt with no `133;B`
    /// yet has `"cmdcol":null`.
    #[test]
    fn blocks_json_says_the_column_the_input_starts_at() {
        let host = MemoryHost::new(7);
        host.feed(b"\x1b]133;A\x07~/proj % ");
        let before = selection::cmd_blocks_json(&host, 7, "1");
        assert!(
            before.contains("\"state\":\"prompt\"")
                && before.contains("\"cmd\":null,\"cmdcol\":null,"),
            "{before}"
        );
        host.feed(b"\x1b]133;B\x07rm -rf build");
        let after = selection::cmd_blocks_json(&host, 7, "1");
        assert!(after.contains("\"state\":\"entering\""), "{after}");
        assert!(after.contains("\"cmdcol\":9,\"out\":null"), "{after}");
    }

    /// `blocks --json` carries a cwd (OSC 7) and a command line (OSC 633 E)
    /// that a program chose, and a person may read the reply raw on a
    /// terminal: no character a terminal acts on or reorders text around
    /// (C0, DEL, C1, bidi, U+2028/9) reaches the reply's bytes — each is a
    /// `\uXXXX` escape — and a JSON reader still decodes both fields to exactly
    /// what the program sent (round fifteen, OUT-2).
    #[test]
    fn blocks_json_escapes_what_a_terminal_acts_on_and_decodes_back() {
        let host = MemoryHost::new(0);
        host.feed(
            b"\x1b]7;file://evilhost/tmp/CWD%1B]52;c;Q1dE%07%1B[2K%0DFAKE%C2%9B31m%E2%80%AEdcba\
              %E2%80%A8%7F\x07",
        );
        host.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        host.feed("\x1b]633;E;CMD\u{9b}2J\u{9d}0 pwned\u{9c}\u{202e}txt.exe\x07".as_bytes());
        host.feed(b"\x1b]133;C\x07out\r\n\x1b]133;D;0\x07");
        let reply = selection::cmd_blocks_json(&host, 0, "");
        let body = reply.strip_prefix("OK 1\n").expect("one JSON row");
        let body = body.strip_suffix('\n').expect("one JSON row");
        // The class, stated here on its own (`aterm_codec::tty::is_active`).
        let active: Vec<String> = body
            .chars()
            .filter(|&c| {
                matches!(c as u32, 0..=0x1f | 0x7f..=0x9f | 0x61c | 0x200e | 0x200f)
                    || matches!(c as u32, 0x2028..=0x202e | 0x2066..=0x2069)
            })
            .map(|c| format!("U+{:04X}", c as u32))
            .collect();
        assert!(active.is_empty(), "raw {active:?} in {body:?}");
        assert!(
            body.contains("FAKE\\u009b31m\\u202edcba\\u2028\\u007f"),
            "{body}"
        );
        let value: aterm_json::Value = aterm_json::from_str(body).expect("valid JSON");
        let block = &value["blocks"][0];
        assert_eq!(
            block["cwd"].as_str(),
            Some("/tmp/CWD\u{1b}]52;c;Q1dE\u{7}\u{1b}[2K\rFAKE\u{9b}31m\u{202e}dcba\u{2028}\u{7f}")
        );
        assert_eq!(
            block["cmdline"].as_str(),
            Some("CMD\u{9b}2J\u{9d}0 pwned\u{9c}\u{202e}txt.exe")
        );
    }

    /// The seam itself refuses the foreign sid: a `resolve` on one host
    /// followed by a `write_input` on another must not land keystrokes in the
    /// session this host holds.
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

    /// Bytes written for the host's own sid reach the sink and the reply is
    /// `Some(true)` (the half `check_write_input` cannot assert live).
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

    /// A host keeping no roster lists and resolves nothing, and says so in its
    /// capabilities.
    #[test]
    fn a_rosterless_host_lists_nothing_and_resolves_nothing() {
        let host = MemoryHost::without_roster(5);
        for o in run_all(&host, 5) {
            assert!(o.passed(), "{}: {:?}", o.check, o.failure);
        }
        assert!(host.sessions().is_empty());
        assert_eq!(host.resolve(Selector::Local(5)), None);
        assert_eq!(host.resolve(Selector::Id(host.id())), None);
        // The session is still served, which is exactly the distinction: no index,
        // not no sessions.
        assert_eq!(selection::cmd_blocks(&host, 5, ""), "OK 0\n");
    }

    /// A host with no state: a fresh `Terminal` per accessor; no roster,
    /// clipboard, sink or event loop; a no-op redraw and a wait that only times
    /// out. `seed` feeds its throwaway engine, covering both a wholly inert host
    /// and one with content that forgets every selection.
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

    /// A host preserving no session state must fail the matrix; `select`'s
    /// state probe catches it even on a blank screen.
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

    /// The unwitnessed read-only subset passes a wholly inert host, blank and
    /// seeded (the documented limit).
    #[test]
    fn the_unwitnessed_read_only_subset_passes_an_inert_host() {
        for seed in [b"".as_slice(), b"hello world".as_slice()] {
            let host = ForgetfulHost { sid: 0, seed };
            for o in run_read_only(&host, 0) {
                assert!(o.passed(), "{}: {:?}", o.check, o.failure);
            }
        }
    }

    /// The inert host's empty roster, dead `resolve` and refused write are
    /// legal while it advertises none of them (`aterm-gui`'s `GuiHost::new`
    /// contract). Declaring the roster or the sink makes the same host fail,
    /// naming which. A host that advertises them and answers empty is caught
    /// without a declaration (`each_roster_and_input_arm_goes_red`).
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

    /// With text on screen the read-back half fails too: the next call reads an
    /// engine that never saw the selection.
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

    /// The inert host fails once the caller names what the session holds.
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

    /// The limit on [`check_screen_witness`]: a stateless host replaying the
    /// right bytes satisfies the witness; `check_select` still catches it.
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

    /// An empty witness is refused. A caller with nothing to vouch for must not get
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
    /// a sid it does not serve fails rather than answering from the session it has.
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

    /// A witness the session has scrolled OFF is still found in history: output
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

    /// Each host below answers honestly for what it advertises, so plain
    /// `run_all` accepts it, but declaring the facility it should have fails it.
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
        // A hang detector, a minute (AGENTS.md): the completion lands 40 ms in.
        let resp = selection::cmd_wait(&host, 0, "60000");
        h.join().unwrap();
        assert!(
            resp.starts_with("OK complete ") && resp.contains("exit=0"),
            "{resp}"
        );
    }

    /// `wait <ms>` reads its `<ms>` strictly: a tail that is not a whole number of
    /// milliseconds is the usage line. Read leniently it became the 30 s default,
    /// so `wait 2s` waited up to fifteen times what was asked — here, with a
    /// command already complete, it answered that completion as if `<ms>` had
    /// been fine. The empty tail and a number (held at 600 s) still wait.
    #[test]
    fn a_wait_ms_that_is_not_a_number_is_the_usage_line() {
        let host = MemoryHost::new(0);
        host.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07true\n\x1b]133;C\x07\x1b]133;D;0\x07");
        for tail in ["2s", "1.5", "-1", "0x10", "5 6", "timeout=5"] {
            assert_eq!(
                selection::cmd_wait(&host, 0, tail),
                "ERR usage: wait [<ms>]\n",
                "`wait {tail}`"
            );
        }
        for tail in ["", "0", "99999999999"] {
            let reply = selection::cmd_wait(&host, 0, tail);
            assert!(reply.starts_with("OK complete "), "`wait {tail}`: {reply}");
        }
        assert_eq!(
            crate::wire::wait_ms("99999999999"),
            Some(crate::wire::WAIT_MAX_MS)
        );
        assert_eq!(
            crate::wire::wait_ms(" 5"),
            None,
            "the caller trims, not the reader"
        );
    }

    /// `blocks`/`blocktext` over real OSC-133 content, so `check_blocks` sees a
    /// non-empty block list.
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

    /// A block's cwd reaches the wire through [`SessionHost::native_cwd`] in
    /// both `blocks` forms: the default prints the engine's RFC 8089 path
    /// verbatim; a host may name paths its own way (the Windows GUI prints
    /// `C:\Users\x` for pwsh's `file:///C:/Users//x`).
    #[test]
    fn a_block_cwd_is_printed_through_the_hosts_native_cwd() {
        /// [`MemoryHost`] with only the path naming changed, marked so the
        /// assertion cannot pass by coincidence on any platform.
        struct Renaming(MemoryHost);
        impl SessionHost for Renaming {
            fn capabilities(&self) -> HostCapabilities {
                self.0.capabilities()
            }
            fn sessions(&self) -> Vec<SessionEntry> {
                self.0.sessions()
            }
            fn resolve(&self, selector: Selector<'_>) -> Option<u64> {
                self.0.resolve(selector)
            }
            fn with_terminal<R>(&self, sid: u64, f: impl FnOnce(&Terminal) -> R) -> Option<R> {
                self.0.with_terminal(sid, f)
            }
            fn with_terminal_mut<R>(
                &self,
                sid: u64,
                f: impl FnOnce(&mut Terminal) -> R,
            ) -> Option<R> {
                self.0.with_terminal_mut(sid, f)
            }
            fn write_input(&self, sid: u64, bytes: &[u8]) -> Option<bool> {
                self.0.write_input(sid, bytes)
            }
            fn request_redraw(&self, sid: u64) {
                self.0.request_redraw(sid);
            }
            fn subscribe(&self, sid: u64) -> Box<dyn ChangeWait + '_> {
                self.0.subscribe(sid)
            }
            fn clipboard_set(&self, text: &str) -> bool {
                self.0.clipboard_set(text)
            }
            fn native_cwd<'p>(&self, reported: &'p str) -> std::borrow::Cow<'p, str> {
                std::borrow::Cow::Owned(format!("native:{reported}"))
            }
        }

        // What pwsh sends: an OSC 7 URI, then the prompt-start mark that
        // records the block's cwd.
        let prompt = b"\x1b]7;file:///C:/Users//x\x07\x1b]133;A\x07$ ";
        let plain = MemoryHost::new(0);
        plain.feed(prompt);
        let listed = selection::cmd_blocks(&plain, 0, "");
        assert!(listed.contains(" cwd=/C:/Users//x "), "{listed}");
        let json = selection::cmd_blocks_json(&plain, 0, "");
        assert!(json.contains("\"cwd\":\"/C:/Users//x\""), "{json}");

        let renaming = Renaming(MemoryHost::new(0));
        renaming.0.feed(prompt);
        let listed = selection::cmd_blocks(&renaming, 0, "");
        assert!(listed.contains(" cwd=native:/C:/Users//x "), "{listed}");
        let json = selection::cmd_blocks_json(&renaming, 0, "");
        assert!(json.contains("\"cwd\":\"native:/C:/Users//x\""), "{json}");
    }

    /// One contract broken, everything else honest, so each failing arm of the
    /// roster/resolve/write checks is seen going red.
    #[derive(Clone, Copy, Debug)]
    enum Deviation {
        /// The control: nothing bent, so a red below is the deviation's doing.
        Honest,
        /// Keeps no roster by its own capabilities, lists anyway.
        ListsWithoutARoster,
        /// The production BUG: advertises a roster, answers with an empty one.
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
                // The lie is the answer to a sid it does not serve, so the served
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

    /// Every failing arm of `sessions`/`resolve`/`write_input` goes red, and the
    /// report names which contract broke.
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

    /// A host that rosters one set of sids and serves another (the
    /// fleet-vs-session split). One engine stands in for every served session.
    struct ServesTheseSids {
        /// What the per-session methods answer for.
        served: Vec<u64>,
        /// What `sessions`/`resolve` publish. Empty means this host keeps no roster
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

    /// The foreign sid is the seam's answer, not the roster's: a host with no
    /// index is not failed for serving a roster-derived guess.
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

    /// When a rostered sibling is not served, that is the sid probed: `resolve`
    /// hands those out fleet-wide (the shipped `GuiHost`'s shape).
    #[test]
    fn the_foreign_sid_probe_prefers_a_rostered_sibling() {
        let host = ServesTheseSids::new([1], 0..=2);
        assert_eq!(unserved_sid(&host, 1), Some(0));
        let o = check_write_input(&host, 1);
        assert!(o.passed(), "{:?}", o.failure);
    }

    /// A daemon-shaped host serves every session it rosters, so the foreign sid
    /// is sought past the roster's end. The roster checks run over the long
    /// roster too.
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

    /// The sid check forgotten: every accessor answers for whatever number it is
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
