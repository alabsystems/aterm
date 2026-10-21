// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The verb-matrix suite a [`SessionHost`] must pass. This crate's tests run
//! it against a reference in-memory host (`MemoryHost`, test-only).
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
/// [`check_unserved_sid`]) against `host`'s session `sid`, in order. Never
/// panics.
///
/// Safe on a live session: the selection checks restore what they found, and
/// the unserved-sid check hands the host only a closure that changes nothing.
/// Use [`run_read_only`] when the session must not be written at all.
pub fn run_all<H: SessionHost>(host: &H, sid: u64) -> Vec<Outcome> {
    let mut outcomes = run_read_only(host, sid);
    outcomes.extend(run_selection_state(host, sid));
    outcomes.push(check_unserved_sid(host, sid));
    outcomes
}

/// [`run_all`] against a host whose capabilities the caller vouches for:
/// `declared` names the capabilities it should have, and a host answering as if
/// it lacks one fails.
///
/// Every gated check reads `host.capabilities()`, so a host advertising none
/// takes the easy arm of each (`ERR unsupported` for `copy`, an empty roster).
/// Only the caller knows whether that absence is honest. Use this wherever the
/// host's shape is known (a shipped host, a gate); [`run_all`] is for a host
/// discovered over the wire.
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

/// The checks that only read: no selection, repaint request or clipboard
/// write. Always safe against a live session, including one a human
/// is mid-drag in.
///
/// These are wire-shape checks only: a host serving a fresh empty `Terminal`
/// from every accessor with no capabilities passes all six. So this shows the
/// host speaks the protocol, not that it has a session. For that, name the
/// session's content ([`run_read_only_witnessed`]) or the host's capabilities
/// ([`run_all_declared`]).
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
/// roster listing and resolving the session under test. A host may advertise
/// more (a floor, not an equality).
///
/// `clipboard` is flag-only because probing it would run `copy` and could
/// write the user's real clipboard ([`check_copy`]'s empty-selection arm
/// exercises it safely). `frame_source` and `event_loop` are flag-only because
/// neither is observable through this trait (aterm-gui's redraw-conformance
/// binary covers them).
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
/// session under test, with every row's `@<id>` reading back through the wire's
/// grammar ([`aterm_types::control_request::Sid::parse`]) as a stable id (an
/// all-digit `id` would address another session's local sid, and any other
/// spelling names no session).
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
        if aterm_types::control_request::Sid::parse(&e.id).is_none() {
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
/// [`check_unserved_sid`] needs. `with_terminal` is the read-only oracle: every
/// per-session method shares its "serves `sid`" definition. `None` when every
/// probed sid is served.
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

/// A sid this host does not serve -> the mutable accessor declines it as the
/// read accessor does, so a sid `resolve` hands out fleet-wide never reaches
/// the session this host holds under a borrowed number.
///
/// The foreign sid is probed ([`unserved_sid`]), so this works for
/// session-scoped and fleet hosts alike; a host serving every sid probed fails,
/// since nothing on it can fail closed. The closure handed over changes
/// nothing, so a host that does serve the sid is left as it was.
pub fn check_unserved_sid<H: SessionHost>(host: &H, sid: u64) -> Outcome {
    const CHECK: &str = "unserved sid: refused by the accessors, never a borrowed session";
    let Some(foreign) = unserved_sid(host, sid) else {
        return Outcome::fail(
            CHECK,
            format!("serves every sid probed near {sid}: nothing here can fail closed"),
        );
    };
    if host
        .with_terminal_mut(foreign, |_: &mut Terminal| ())
        .is_some()
    {
        return Outcome::fail(
            CHECK,
            format!("foreign sid {foreign} was served by with_terminal_mut"),
        );
    }
    Outcome::pass(CHECK)
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
/// no-op, a condvar change signal and an in-memory clipboard.
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
    clipboard: Mutex<Option<String>>,
    capabilities: HostCapabilities,
    changed: ChangeSignal,
    redraws: AtomicU64,
}

#[cfg(test)]
impl MemoryHost {
    /// A host owning one 24x80 session numbered `sid`, with a clipboard and a
    /// roster.
    #[must_use]
    pub fn new(sid: u64) -> Self {
        Self {
            sid,
            id: format!("s-{sid:020x}"),
            term: Arc::new(Mutex::new(Terminal::new(24, 80))),
            clipboard: Mutex::new(None),
            capabilities: HostCapabilities {
                frame_source: false,
                event_loop: false,
                clipboard: true,
                roster: true,
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
}
