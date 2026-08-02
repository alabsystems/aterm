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

/// EVERY check — [`run_read_only`] then [`run_selection_state`] — against
/// `host`'s session `sid`, in order. Never panics: a host under test reports, it
/// does not abort the harness running it.
///
/// Safe on a live session: the selection checks put back what they found. Prefer
/// [`run_read_only`] when the session must not be written at all.
pub fn run_all<H: SessionHost>(host: &H, sid: u64) -> Vec<Outcome> {
    let mut outcomes = run_read_only(host, sid);
    outcomes.extend(run_selection_state(host, sid));
    outcomes
}

/// The checks that only READ: no selection is touched, no repaint is asked for,
/// nothing is written to the input sink or the clipboard. ALWAYS safe against a
/// live session — including one a human is mid-drag in, which restore-and-put-back
/// cannot claim.
pub fn run_read_only<H: SessionHost>(host: &H, sid: u64) -> Vec<Outcome> {
    vec![
        check_blocks(host, sid),
        check_blocks_json(host, sid),
        check_blocktext(host, sid),
        check_wait(host, sid),
    ]
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

/// `select` -> `OK\n` for the forms that apply, and the EXACT usage / bad-arg
/// strings for the forms that do not. The error text is wire, not diagnostics.
///
/// Drives the selection; restores the prior one however it exits.
pub fn check_select<H: SessionHost>(host: &H, sid: u64) -> Outcome {
    const CHECK: &str = "select: OK on clear, exact usage/bad-args otherwise";
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
    Outcome::pass(CHECK)
}

/// `selection` -> `OK 0\n` with nothing selected (the same empty framing `text`
/// uses), checked right after a `select clear` so the state is known.
///
/// Drives the selection; restores the prior one however it exits.
pub fn check_selection<H: SessionHost>(host: &H, sid: u64) -> Outcome {
    const CHECK: &str = "selection: OK 0 after select clear";
    let _restore = SelectionRestore::new(host, sid);
    let cleared = selection::cmd_select(host, sid, "clear");
    if cleared != "OK\n" {
        return Outcome::fail(CHECK, cleared);
    }
    let out = selection::cmd_selection(host, sid);
    if out == "OK 0\n" {
        Outcome::pass(CHECK)
    } else {
        Outcome::fail(CHECK, out)
    }
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
    /// A host owning one 24x80 session numbered `sid`, WITH a clipboard.
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
        (sid == self.sid).then(|| {
            self.input
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .extend_from_slice(bytes);
            true
        })
    }

    fn request_redraw(&self, _sid: u64) {
        self.redraws.fetch_add(1, Ordering::Relaxed);
    }

    fn subscribe(&self, _sid: u64) -> Box<dyn ChangeWait + '_> {
        let (lock, _) = &*self.changed;
        let registered_at = *lock.lock().unwrap_or_else(|p| p.into_inner());
        Box::new(MemoryWait {
            changed: Arc::clone(&self.changed),
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
        assert_eq!(outcomes.len(), 7, "the matrix lost a check");
        for o in &outcomes {
            assert!(o.passed(), "{}: {:?}", o.check, o.failure);
        }
        // `run_all` is the two groups, whole: no check belongs to neither.
        let grouped: Vec<&str> = run_read_only(&host, 7)
            .iter()
            .chain(run_selection_state(&host, 7).iter())
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
    }

    /// The read-only subset writes NOTHING — not the selection, not the input
    /// sink, not even a repaint request. That is what makes it unconditionally
    /// safe, including against a session being dragged in right now.
    #[test]
    fn the_read_only_subset_writes_nothing() {
        let host = MemoryHost::new(0);
        host.feed(b"hello world");
        assert_eq!(selection::cmd_select(&host, 0, "0 0 0 4"), "OK\n");
        let before = selection::cmd_selection(&host, 0);
        let redraws = host.redraws();
        let outcomes = run_read_only(&host, 0);
        assert_eq!(outcomes.len(), 4, "the read-only subset lost a check");
        for o in &outcomes {
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
}
