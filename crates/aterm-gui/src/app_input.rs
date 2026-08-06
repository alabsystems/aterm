// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Keyboard + IME + action dispatch: the `App::input` convergence seam, `on_key`
//! (keybinding lookup + hardcoded chords + search-mode), IME preedit/commit,
//! action + menu dispatch, the seam-left mouse press, and `mouse_modifiers`.
//! Plus the `egress_to_outcome` reply mapper and the `base_logical_key` cfg pair.
//! A verbatim inherent-impl split of `App`.

use std::sync::{Arc, Mutex};

use aterm_core::selection::SelectionType;
use aterm_core::terminal::Terminal;
use aterm_session::sink::SinkWriter;
use winit::event::{ElementState, KeyEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::input::{self, InputEvent, InputOutcome, ScrollIntent, Source};
use crate::{App, FONT_ZOOM_STEP, Wake, WindowId, keybinding, keymap, menu, pane, term_lock};

/// How long after a keystroke the present-pacing bypass (`WindowState::input_hot`)
/// stays armed. Comfortably longer than a local echo round trip and than a fast
/// typist's inter-key gap, so a burst never drops the bypass mid-word; short enough
/// that a key which never echoes releases it within a couple of frames. Every key
/// re-arms it, so this is a tail after the LAST key, not a per-key budget.
const INPUT_HOT_WINDOW: std::time::Duration = std::time::Duration::from_millis(50);

/// A predictor mutation needs a paint when it either removes pixels that were
/// already on glass or creates/extends a visible overlay. Keeping this decision
/// in one pure seam prevents conservative flush paths from accidentally keying
/// redraw only to their (usually `false`) "new prediction added" return value.
#[inline]
fn prediction_visibility_requires_redraw(was_visible: bool, is_visible: bool) -> bool {
    was_visible || is_visible
}

/// Whether a typing click cued at the KEY could ever reach a speaker — the
/// host half of the touch-to-glass audio seam (`CursorGlow::cue_keystroke`).
///
/// The ENGINE owns the darkness law (master switch, real geometry, nonzero
/// amplitude); this seam answers "could a click cued right now reach a speaker",
/// so a build whose sound can only ever be silence — headless, a non-macOS stub,
/// a permanently failed worker, the knob off, volume 0 — never pays the
/// cue-delivering redraw on its hottest path. Same "never runs headless-muted"
/// policy as `tone_infer_active`.
///
/// IT MUST AGREE WITH THE DRAIN, not merely approximate it. Cueing at the key
/// also arms a CREDIT that the echo's `spawn` spends to stay silent (one
/// character, one click). If the drain then drops the cue for a policy this gate
/// ignored, the credit outlives the click it stood for: the key made no sound,
/// and the echo — which pre-seam would have clicked — is muted by a credit for a
/// click that never happened. So the two MUTE policies the drain folds into its
/// gain, serious mode (`SeriousEffect::TerminalSound`) and the post-resize quiet
/// window, are checked HERE too, at the moment the credit is armed.
///
/// Focus is deliberately NOT a term: the drain's raw-focus check exists to stop a
/// recording-pinned BACKGROUND window talking over the foreground one, and a
/// physical keypress by definition arrives at the focused window.
///
/// A policy that flips between the key and the drain (a resize starting inside
/// that one frame) still orphans a credit, but only for the ledger's freshness
/// window, after which it expires on its own.
#[inline]
fn keystroke_click_audible(
    worker_live: bool,
    sounds_on: bool,
    volume: f32,
    sound_allowed: bool,
    resize_quiet: bool,
) -> bool {
    worker_live && sounds_on && volume > 0.0 && sound_allowed && !resize_quiet
}

/// Whether an input event carries fresh, discrete intent that may start one new
/// bounded presentation-recovery episode. Pointer motion is deliberately a
/// stutter here: a stationary app can receive an unbounded hover/drag stream,
/// so treating every pixel as fresh fuel turns a finite retry train back into
/// an infinite render loop while a surface is persistently unavailable.
#[inline]
pub(crate) fn is_present_recovery_stimulus(ev: &InputEvent) -> bool {
    !matches!(
        ev,
        InputEvent::Key {
            event_type: aterm_types::keyboard::KeyEventType::Release,
            ..
        } | InputEvent::MouseMove { .. }
    )
}

/// Test-only observation of the REAL physical-release routing branch. This is
/// compiled out of shipping builds, so the input hot path pays no storage,
/// cloning, or synchronization cost. Tier-1 uses it to prove that a cross-window
/// key-up was swallowed for a consumed press or encoded against the exact
/// press-time session/key/modifier identity for a forwarded press.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PhysicalReleaseTrace {
    Consumed {
        arrival_window: WindowId,
        press_window: WindowId,
    },
    Forwarded {
        arrival_window: WindowId,
        press_window: WindowId,
        session: u64,
        key: aterm_types::keyboard::Key,
        mods: aterm_types::keyboard::Modifiers,
        base_layout: Option<char>,
        event_type: aterm_types::keyboard::KeyEventType,
        delivery: input::Delivery,
    },
    Literal {
        arrival_window: WindowId,
        press_window: WindowId,
        session: u64,
        event: InputEvent,
        repeated: bool,
        delivery: input::Delivery,
    },
    Local {
        arrival_window: WindowId,
        press_window: WindowId,
    },
}

#[cfg(test)]
std::thread_local! {
    static PHYSICAL_RELEASE_TRACE:
        std::cell::RefCell<Option<PhysicalReleaseTrace>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn clear_physical_release_trace() {
    PHYSICAL_RELEASE_TRACE.with(|trace| *trace.borrow_mut() = None);
}

#[cfg(test)]
fn record_physical_release_trace(observed: PhysicalReleaseTrace) {
    PHYSICAL_RELEASE_TRACE.with(|trace| *trace.borrow_mut() = Some(observed));
}

#[cfg(test)]
pub(crate) fn take_physical_release_trace() -> Option<PhysicalReleaseTrace> {
    PHYSICAL_RELEASE_TRACE.with(|trace| trace.borrow_mut().take())
}

/// Compatibility `open prefs|about|update` spellings resolve to durable
/// routes in the process-singleton Settings tab. Keeping the mapping pure makes
/// the alias contract independently testable and prevents a modal constructor
/// from creeping back into command dispatch.
fn native_settings_route_for_aux(
    target: crate::app_introspect::AuxTarget,
) -> Option<crate::native_settings::SettingsRoute> {
    use crate::app_introspect::AuxTarget;
    use crate::native_settings::SettingsRoute;
    match target {
        AuxTarget::Prefs => Some(SettingsRoute::Home),
        AuxTarget::About => Some(SettingsRoute::About),
        AuxTarget::Update => Some(SettingsRoute::SoftwareUpdate),
        AuxTarget::Front | AuxTarget::Menu => None,
    }
}

/// Native Settings destination owned by an App-menu command. The menu dispatch
/// and its regression test share this authority, so "Open aterm.toml" cannot
/// silently drift back to a platform-specific external editor.
fn native_settings_route_for_menu(
    action: crate::menu::MenuAction,
) -> Option<crate::native_settings::SettingsRoute> {
    use crate::menu::MenuAction;
    use crate::native_settings::SettingsRoute;
    match action {
        MenuAction::Preferences => Some(SettingsRoute::Manual),
        _ => None,
    }
}

/// Configurable actions that are meaningful without terminal authority. Raw
/// key sequences are never native-safe; terminal-only actions are consumed as
/// no-ops while a native view owns input.
const fn native_binding_allowed(action: keybinding::Action) -> bool {
    use keybinding::Action;
    matches!(
        action,
        Action::NewTab
            | Action::ReopenClosedTab
            | Action::CloseTab
            | Action::NewWindow
            | Action::NextTab
            | Action::PrevTab
            | Action::SwitchTab(_)
            | Action::SplitVertical
            | Action::SplitHorizontal
            | Action::Paste
            | Action::Find
            | Action::FocusPaneLeft
            | Action::FocusPaneRight
            | Action::FocusPaneUp
            | Action::FocusPaneDown
            | Action::TogglePaneZoom
            | Action::ScrollPageUp
            | Action::ScrollPageDown
            | Action::ScrollLineUp
            | Action::ScrollLineDown
            | Action::ScrollToTop
            | Action::ScrollToBottom
            | Action::ToggleSettings
            | Action::ToggleAbout
            | Action::ToggleMatrixRain
            | Action::ToggleSeriousMode
            | Action::OpenPalette
    )
}

/// Per-session ORDERED egress serializer — the fix for the paste↔keystroke race.
///
/// `input_paste` off-loads the (blocking) paste write OFF the winit UI thread so a
/// large paste into a stalled child can't freeze the event loop. But a detached
/// write races any key typed right after: the key's inline `write_frame` can win
/// the sink lock before the paste thread is scheduled, so the child sees the
/// keystroke BEFORE the pasted text (submission order violated). This routes a
/// paste — and any keystroke submitted while that paste is still in flight —
/// through ONE per-session FIFO drained by a single writer thread, so bytes reach
/// the PTY in submission order. Both run the SAME [`crate::input::seam_egress`] on
/// the writer thread (no encoder duplication, so the Human/Controller byte
/// invariant is untouched — only WHERE the write runs moves).
///
/// The keystroke HOT PATH is unchanged in the common case: [`is_ordering`] first
/// reads ONE process-wide relaxed atomic (`ACTIVE`) and, while no paste is in
/// flight ANYWHERE, returns immediately with no registry lock. All of `App::input`
/// runs on the single UI thread, so a session's `pending` count is only ever
/// raised by that thread and lowered by its writer: a keystroke that observes
/// `pending == 0` knows the paste already landed and can safely go inline.
pub(crate) mod paste_order {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::sync::{Arc, LazyLock, Mutex, Weak};

    use super::{InputEvent, SinkWriter, Terminal};

    /// A deferred egress: run `seam_egress(term, sink, ev)` on the writer thread.
    struct Job {
        term: Arc<Mutex<Terminal>>,
        sink: Arc<SinkWriter>,
        ev: InputEvent,
        pending: Arc<AtomicUsize>,
        /// The hardware key-arrival stamp CLAIMED from the metrics module at
        /// enqueue, so the writer thread can book the true key→write slice once the
        /// bytes actually leave. `0` when no keystroke is behind this job (a paste
        /// body, a control verb).
        key_ns: u64,
    }

    /// One session's serializer: the FIFO sender, its outstanding-job count, and a
    /// `Weak` to the session sink so a closed tab's entry can be pruned.
    struct Serializer {
        tx: Sender<Job>,
        pending: Arc<AtomicUsize>,
        sink: Weak<SinkWriter>,
    }

    /// Outstanding jobs across ALL sessions. The keystroke hot path reads only
    /// this; while it is 0 (no paste in flight anywhere) keys go inline, no lock.
    static ACTIVE: AtomicUsize = AtomicUsize::new(0);
    /// session master key -> serializer. Created lazily on the first paste, pruned
    /// once the session's sink is gone.
    static REG: LazyLock<Mutex<HashMap<i32, Serializer>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    /// The writer thread body: drain jobs in FIFO order, writing each through the
    /// SAME seam the inline path uses. Exits when the sender drops (session gone).
    fn run(rx: Receiver<Job>) {
        while let Ok(job) = rx.recv() {
            // The egress-order writer thread is expendable: block under SPILL_CAP so
            // a wedged foreground applies backpressure HERE, not by growing the spill.
            crate::input::seam_egress(
                &job.term,
                &job.sink,
                &job.ev,
                crate::input::EgressMode::Backpressured,
            );
            // The bytes are on the wire NOW — book the slice the UI thread could
            // not: hardware key arrival → completed write, including the time this
            // job spent queued behind the paste ahead of it.
            crate::metrics::note_pty_write_at(job.key_ns);
            job.pending.fetch_sub(1, Ordering::AcqRel);
            ACTIVE.fetch_sub(1, Ordering::AcqRel);
        }
    }

    /// The FIFO sender + pending counter for `master`, spawning the writer thread
    /// on first use. `None` iff the thread could not be spawned (caller writes
    /// inline — best effort, never wedged).
    fn writer_for(
        reg: &mut HashMap<i32, Serializer>,
        master: i32,
        sink: &Arc<SinkWriter>,
    ) -> Option<(Sender<Job>, Arc<AtomicUsize>)> {
        if let Some(s) = reg.get(&master) {
            return Some((s.tx.clone(), s.pending.clone()));
        }
        let (tx, rx) = channel::<Job>();
        std::thread::Builder::new()
            .name("aterm-egress-order".into())
            .spawn(move || run(rx))
            .ok()?;
        let pending = Arc::new(AtomicUsize::new(0));
        reg.insert(
            master,
            Serializer {
                tx: tx.clone(),
                pending: pending.clone(),
                sink: Arc::downgrade(sink),
            },
        );
        Some((tx, pending))
    }

    /// Whether egress for `master` must currently be ORDERED behind an in-flight
    /// paste. One relaxed atomic in the common (no-paste) case; the registry lock
    /// is taken only while some paste is draining.
    pub(crate) fn is_ordering(master: i32) -> bool {
        if ACTIVE.load(Ordering::Acquire) == 0 {
            return false;
        }
        REG.lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&master)
            .is_some_and(|s| s.pending.load(Ordering::Acquire) > 0)
    }

    /// Enqueue `ev` onto `master`'s FIFO so it reaches the PTY in submission order.
    /// Returns `Err(ev)` (handing the event back) when no writer is available, so
    /// the caller falls back to an inline write.
    pub(super) fn enqueue(
        term: &Arc<Mutex<Terminal>>,
        sink: &Arc<SinkWriter>,
        ev: InputEvent,
    ) -> Result<(), InputEvent> {
        let master = sink.master();
        let (tx, pending) = {
            let mut reg = REG.lock().unwrap_or_else(|p| p.into_inner());
            // Prune sessions whose sink is gone (closed tabs): dropping the stored
            // Sender lets their idle writer thread exit. Cold path (paste only).
            reg.retain(|_, s| s.sink.strong_count() > 0);
            match writer_for(&mut reg, master, sink) {
                Some(w) => w,
                None => return Err(ev),
            }
        };
        // Claim the FIFO slot BEFORE releasing to the writer: a later keystroke on
        // this same (UI) thread then observes pending > 0 and queues behind us.
        pending.fetch_add(1, Ordering::AcqRel);
        ACTIVE.fetch_add(1, Ordering::AcqRel);
        // Claim the arrival here so the UI thread's `note_pty_write` cannot
        // consume it on the way out and book a channel push as the write.
        //
        // A PASTE claims nothing. Cmd-V arms a key-arrival stamp like any other
        // press, and `paste_clipboard` runs synchronously on the UI thread before
        // the stamp is cleared — so an unconditional claim here would book the
        // ENTIRE paste, written under `EgressMode::Backpressured`, as ONE
        // key_write sample. A large paste into a slow child then files seconds
        // against a metric that means "keystroke to PTY", and `MAX_KEY_WRITE_NS`
        // is a `fetch_max` that never resets, so that reading poisons the maximum
        // for the life of the process.
        let key_ns = if matches!(ev, InputEvent::Paste(_)) {
            0
        } else {
            crate::metrics::take_key_arrival()
        };
        let job = Job {
            term: term.clone(),
            sink: sink.clone(),
            ev,
            pending,
            key_ns,
        };
        match tx.send(job) {
            Ok(()) => Ok(()),
            Err(std::sync::mpsc::SendError(job)) => {
                // Writer gone (should not happen while the entry lives): undo the
                // counters and hand the event back for an inline write. Give the
                // arrival stamp back too, or the inline fallback would measure
                // nothing — the deferral we claimed it for never happened.
                job.pending.fetch_sub(1, Ordering::AcqRel);
                ACTIVE.fetch_sub(1, Ordering::AcqRel);
                crate::metrics::restore_key_arrival(job.key_ns);
                Err(job.ev)
            }
        }
    }

    /// The shared "enqueue onto the FIFO, else write inline" tail. While a
    /// paste is draining for this sink's master, submit `ev` onto the same
    /// per-session FIFO so it cannot overtake the pasted bytes — a successful
    /// enqueue is full delivery into the ordered spill contract. Otherwise
    /// (including the no-writer fallback) write inline through `seam_egress`.
    /// Returns the egress verdict plus whether the write ran INLINE: an
    /// enqueued event wrote nothing here — the real write happens later on the
    /// writer thread. `ev` is cloned only on the (paste-draining) enqueue
    /// path, never on the common inline one.
    pub(super) fn ordered_or_inline(
        term: &Arc<Mutex<Terminal>>,
        sink: &Arc<SinkWriter>,
        ev: &InputEvent,
        mode: crate::input::EgressMode,
    ) -> (crate::input::Egress, bool) {
        if is_ordering(sink.master()) {
            match enqueue(term, sink, ev.clone()) {
                Ok(()) => (
                    crate::input::Egress::Reported(crate::input::Delivery::Full),
                    false,
                ),
                Err(ev) => (crate::input::seam_egress(term, sink, &ev, mode), true),
            }
        } else {
            (crate::input::seam_egress(term, sink, ev, mode), true)
        }
    }
}

/// Map the seam's [`input::Egress`] to the reply-bearing [`InputOutcome`]: a failed
/// PTY write becomes `WriteFailed` (→ `ERR write failed`) so a reply-bearing verb is
/// never told OK for bytes that did not land (the input-path reply-fidelity contract).
pub(crate) fn egress_to_outcome(e: input::Egress) -> InputOutcome {
    match e {
        input::Egress::Reported(input::Delivery::Failed) => InputOutcome::WriteFailed,
        _ => InputOutcome::Ok,
    }
}

/// The modifier-INDEPENDENT logical key of a winit event (the unshifted base
/// key), used for the keybinding chord lookup so a binding written as the base
/// key (`cmd+shift+]`, not `cmd+}`) matches regardless of how Shift composes the
/// glyph on the active layout (e.g. `ctrl+shift+=` for zoom-in still matches
/// once Shift has composed `+`). macOS, the X11/Wayland backends, and Windows all
/// implement `KeyEventExtModifierSupplement`, so this is `key_without_modifiers()`
/// on all three; the cfg twin below is the fallback for platforms without that
/// extension. It returns an OWNED key so the borrow on `ev` ends before
/// `on_key`'s later `&ev.logical_key` matches.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub(crate) fn base_logical_key(ev: &KeyEvent) -> Key {
    use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
    ev.key_without_modifiers()
}

/// Fallback for platforms WITHOUT the modifier-supplement extension (not macOS,
/// not Linux X11/Wayland, not Windows): the plain logical key is the closest
/// equivalent.
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub(crate) fn base_logical_key(ev: &KeyEvent) -> Key {
    ev.logical_key.clone()
}

/// The default (hardcoded) scrollback chord, if any: Shift + PageUp/PageDown/Home/End
/// → a [`ScrollIntent`], else `None` so the key falls through to the PTY encoder.
/// Shift must be the SOLE chord modifier (Ctrl/Alt/Super excluded; Caps/Num Lock are
/// in the live platform query, not `ModifiersState`, so they never appear here). This
/// is the xterm / Terminal.app convention: the Shift+ forms scroll the view, plain
/// PageUp/Home/End reach the application.
fn scrollback_chord(mods: ModifiersState, ev: &KeyEvent) -> Option<ScrollIntent> {
    if !mods.shift_key() || mods.control_key() || mods.alt_key() || mods.super_key() {
        return None;
    }
    match ev.logical_key {
        Key::Named(NamedKey::PageUp) => Some(ScrollIntent::Up),
        Key::Named(NamedKey::PageDown) => Some(ScrollIntent::Down),
        Key::Named(NamedKey::Home) => Some(ScrollIntent::Top),
        Key::Named(NamedKey::End) => Some(ScrollIntent::Bottom),
        _ => None,
    }
}

/// Whether an open FIND FIELD suspends a configured `[keybindings]` action, so the
/// keystroke drives the query instead. True for the text-level and viewport-level
/// actions (typing, copying, scrolling — the ones a focused text field owns or would be
/// desynced by); false for app-level ones (windows, tabs, panes, font, settings, and
/// find itself), which keep working with the panel open.
fn find_suspends_action(action: keybinding::Action) -> bool {
    use keybinding::Action as A;
    matches!(
        action,
        A::Copy
            | A::Paste
            | A::ScrollPageUp
            | A::ScrollPageDown
            | A::ScrollLineUp
            | A::ScrollLineDown
            | A::ScrollToTop
            | A::ScrollToBottom
            | A::JumpPrevPrompt
            | A::JumpNextPrompt
            | A::ToggleViMode
    )
}

/// Classify the terminal-only Emacs navigation chords after layout normalization.
/// Exactly bare Super-S/Super-R qualify: adding Shift, Control, or Alt leaves the
/// chord available to its existing owner. The caller intentionally runs this only
/// after native-view, configured-keybinding, and vi-mode boundaries.
fn terminal_emacs_search_direction(base: &Key, mods: ModifiersState) -> Option<bool> {
    if !mods.super_key() || mods.shift_key() || mods.control_key() || mods.alt_key() {
        return None;
    }
    match base {
        Key::Character(key) if key.eq_ignore_ascii_case("s") => Some(true),
        Key::Character(key) if key.eq_ignore_ascii_case("r") => Some(false),
        _ => None,
    }
}

fn font_zoom_repeat_action(
    mods: ModifiersState,
    ev: &KeyEvent,
) -> Option<crate::FontZoomRepeatAction> {
    if !mods.super_key() {
        return None;
    }
    let Key::Character(character) = &ev.logical_key else {
        return None;
    };
    match character.as_str() {
        "=" | "+" => Some(crate::FontZoomRepeatAction::Increase),
        "-" => Some(crate::FontZoomRepeatAction::Decrease),
        "0" => Some(crate::FontZoomRepeatAction::Reset),
        _ => None,
    }
}

/// A bare submitted-turn boundary. Modified Enter variants belong to the
/// foreground application (for example Shift+Enter's multiline composer) and
/// must not cancel the interactive-echo discount.
fn is_plain_enter(ev: &InputEvent) -> bool {
    use aterm_types::keyboard::{Key as TKey, Modifiers as TMods, NamedKey as TNamed};

    matches!(
        ev,
        InputEvent::Key {
            key: TKey::Named(TNamed::Enter),
            mods,
            ..
        } if !mods.intersects(TMods::SHIFT | TMods::CTRL | TMods::ALT | TMods::SUPER)
    )
}

/// How many terminals are currently in vi (keyboard copy-mode) — the GUI-side
/// mirror the two per-keystroke vi gates ([`App::vi_repeat_action`] and
/// [`App::on_key_vi_mode`]) consult INSTEAD of the engine. Neither needs the
/// terminal to answer, and the terminal mutex is the ONE lock a keystroke shares
/// with the PTY reader: under heavy output an acquisition queues behind the
/// reader's `process()` holds. `Terminal::vi_toggle` is the sole writer of the
/// engine's `vi.active` anywhere in the workspace (`ViMode::activate`/
/// `deactivate` have no callers) and its two non-test call sites are both in this
/// file on the GUI thread, so the GUI can simply COUNT what it toggled.
///
/// A COUNT, not a flag: two windows can hold two terminals in copy-mode at once.
/// It is only ever consulted as "is it zero?", and every update is made under the
/// SAME `term_lock` that performed the toggle (reading back `vi_is_active` there
/// costs nothing), so the mirror cannot disagree with the engine. A terminal
/// destroyed while still in copy-mode retires its `+1` through
/// [`vi_note_terminal_gone`].
static VI_ACTIVE_TERMINALS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Record the post-toggle vi state of one terminal in the mirror above. MUST be
/// called for every `vi_toggle`, with the state read back under the same lock.
fn vi_note_toggled(now_active: bool) {
    use std::sync::atomic::Ordering;
    if now_active {
        VI_ACTIVE_TERMINALS.fetch_add(1, Ordering::Relaxed);
    } else if VI_ACTIVE_TERMINALS.load(Ordering::Relaxed) > 0 {
        // Guarded rather than a bare `fetch_sub`: a mirror that somehow
        // under-counts must never WRAP to `usize::MAX` and pin the gate open
        // forever. The read-modify-write is not atomic as a pair, which is
        // deliberate and safe — both `vi_toggle` call sites run on the GUI thread,
        // so there is no second writer to race with.
        VI_ACTIVE_TERMINALS.fetch_sub(1, Ordering::Relaxed);
    }
}

/// A terminal that was in copy-mode is GOING AWAY (its last view detached), so
/// retire its contribution to the mirror.
///
/// `vi_toggle` is the only other decrement, so without this the count is a
/// one-way ratchet: closing a tab while copy-mode is on strands a `+1` that
/// nothing can clear, the gate reads "maybe active" for the rest of the process,
/// and BOTH per-keystroke vi checks fall back to the terminal mutex (worth
/// ~0.15-0.37 ms per keystroke under output load). The direction is fail-safe —
/// correctness never depended on the mirror, only speed — which is exactly why
/// the loss would go unnoticed.
pub(crate) fn vi_note_terminal_gone(was_active: bool) {
    if was_active {
        vi_note_toggled(false);
    }
}

/// Whether ANY terminal is in vi (keyboard copy-mode). One relaxed load; `false`
/// is the common path and proves no key can need vi handling, so the two vi gates
/// take ZERO terminal locks while nobody is in copy-mode.
fn vi_any_active() -> bool {
    VI_ACTIVE_TERMINALS.load(std::sync::atomic::Ordering::Relaxed) != 0
}

thread_local! {
    /// Single-slot publication of "can a key RELEASE for this session encode any
    /// bytes at all?", sampled by the press path under a lock it already holds.
    ///
    /// `(session_id + 1) << 1 | relevant`, packed into ONE word so a (session, bit)
    /// pair can never be read half-updated; `0` means "nothing sampled yet". A single
    /// slot is enough because the release this guards is always preceded by the press
    /// of the SAME key on the SAME session — the interleaving that misses is two
    /// sessions typed alternately, which merely falls back to asking the engine.
    ///
    /// THREAD-LOCAL, not a global: publisher (`App::input_to_session`) and consumer
    /// (`App::release_physical_press`) are the same winit event-loop thread by
    /// construction, so no synchronization is needed — and a reader on any OTHER
    /// thread sees "unsampled" and asks the engine, which is the fail-safe direction
    /// (it also keeps parallel unit tests from publishing into each other's slot,
    /// since headless test apps reuse session ids).
    static RELEASE_RELEVANCE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Publish the press-time release relevance of `session`.
///
/// `relevant` must be a CONSERVATIVE superset of "a release can produce bytes":
/// the encoder emits a release report only under Kitty `REPORT_EVENT_TYPES`
/// (`should_encode_kitty_event` stands down on a release without it, and the
/// legacy path returns an empty vec), so any predicate implied by that flag is
/// safe to publish and a `false` reading is a PROOF that the encoder yields
/// nothing.
fn publish_release_relevance(session: u64, relevant: bool) {
    let packed = (session.wrapping_add(1) << 1) | u64::from(relevant);
    RELEASE_RELEVANCE.with(|slot| slot.set(packed));
}

/// `Some(false)` iff the last press-path sample for `session` PROVED that a key
/// release for it encodes nothing; `None` when nothing was sampled for this
/// session (the caller must ask the engine).
fn sampled_release_relevance(session: u64) -> Option<bool> {
    let packed = RELEASE_RELEVANCE.with(std::cell::Cell::get);
    if packed == 0 || packed >> 1 != session.wrapping_add(1) {
        return None;
    }
    Some(packed & 1 == 1)
}

/// Mint a fresh App-wide kitty-summon ident: bump the shared sequence FIRST,
/// then namespace the new value under `tag`. One ordering for every minting
/// site — the sequence keeps two windows sharing one session from minting
/// colliding episodes, and the tag partitions each summon kind's ident space.
///
/// scope-waiver: "App-wide" here names an IDENT NAMESPACE, not a safety budget.
/// The value is a monotonic counter partitioned by `tag`; there is no enforcing
/// state whose duplication could put anything extra on the glass, so no
/// instance count can falsify the phrase. A second sequence would mint
/// COLLIDING idents (a correctness bug the episode map catches), never 2N of a
/// rate-limited effect — which is the hazard OB-17 exists to surface.
fn next_kitty_summon_ident(seq: &mut u64, tag: u64) -> u64 {
    *seq = seq.wrapping_add(1);
    tag ^ *seq
}

/// ONE pure classification of a committed Key/Text/KeySequence press, computed
/// once at the top of `input_to_session`'s key arm and read by every effect
/// feed on that path (predictor arming, cursor-FX hints, and the post-egress
/// cosmetic feeds). Pure over the event — no terminal or App state — and it
/// NEVER gates bytes.
struct PressClass<'ev> {
    /// Predictive-echo candidate for a BARE printable key (no ⌃/⌥/⌘):
    /// `(Some(glyph), false)` registers a speculative glyph, `(None, true)` is
    /// a Backspace retraction. The predictor's `predict_mode` gate stays at
    /// the call site.
    predict_candidate: Option<(Option<char>, bool)>,
    /// Forward-typing direction for the kitty-cursor momentum (INDEPENDENT of
    /// the predictor's mode gate, so the cat flies whether or not predictive
    /// echo is on): a visible char / space / newline advances the cursor
    /// (forward), a backspace moves it back; every other key leaves momentum
    /// to decay.
    typed_forward: Option<bool>,
    /// NAVIGATION key-hint (responsiveness): keys that MOVE the cursor without
    /// writing text — arrow keys, Home/End, Page Up/Down, and the emacs
    /// line/word motions readline binds (Ctrl-A/E/B/F, Alt-B/F). Arming the
    /// fire nav-hint tells the cursor-glow engine the paired cursor move is
    /// scrubbing, so jumping to line start/end (Ctrl-A/E) never erupts a
    /// full-width blaze and held arrows lay no fire. Style-agnostic scalar
    /// bump; only the fire style reads it.
    navigation_key: bool,
    /// KILL key-hint (the erase POOF's semantic half): keys that erase a SPAN
    /// of text in one stroke — kill-to-end (Ctrl-K), kill-line/backward
    /// (Ctrl-U), word kills (Ctrl-W, Alt-D, Alt/Ctrl-Backspace — modified
    /// Backspaces never reach the `typed_forward` matcher), and forward
    /// Delete. The glow engine only poofs when the hint pairs with a real
    /// same-row content shrink (see `CursorGlow::note_kill`), so a kill key an
    /// app ignores stays silent.
    kill_key: bool,
    /// Backward kills (Ctrl-U/W, word-backspaces) LEAP the caret — their echo
    /// move rides the nav-hint choreography (meteor, no blaze). Stationary
    /// kills (Ctrl-K, Alt-D, forward Delete) must NOT arm nav or the leaked
    /// hint eats the next typed glyph's wake.
    kill_moves: bool,
    /// Any Enter press, chorded or not (`is_plain_enter` is the bare form).
    enter_like: bool,
    /// Any Tab press (joins the typed-hint disarm set).
    tab_key: bool,
    /// Bare printable glyph for the cosmetic feeds — the SHIFTED glyph the
    /// encoder sends (`Key::Character` holds the unshifted base, so KITTY
    /// still counts once the detector folds case).
    typed: Option<char>,
    /// A committed IME run (CJK, dead keys, Option-compose) — typed text.
    /// `Text` is built only by `on_ime_commit` and the two human `on_key`
    /// fallbacks, never by paste.
    ime: Option<&'ev str>,
    /// A plain (unchorded) Backspace.
    backspace: bool,
    /// A key that edits or moves beyond one glyph (Enter, Tab, Escape,
    /// Delete, nav keys, any modified chord, raw controller byte sequences):
    /// the cosmetic feeds' BREAK boundary — a word assembled across an
    /// editing boundary was never typed as a word.
    brk: bool,
}

fn classify_press(ev: &InputEvent) -> PressClass<'_> {
    use aterm_types::keyboard::{Key as TKey, Modifiers as TMods, NamedKey as TNamed};
    let predict_candidate: Option<(Option<char>, bool)> = match ev {
        InputEvent::Key { key, mods, .. }
            if !mods.contains(TMods::CTRL)
                && !mods.contains(TMods::ALT)
                && !mods.contains(TMods::SUPER) =>
        {
            match key {
                // Predict the SHIFTED glyph the encoder will send
                // (and the shell will echo) — `Key::Character` holds
                // the unshifted base, so 'h'+Shift must predict 'H'.
                TKey::Character(c) => Some((
                    Some(aterm_types::keyboard::shifted_character(*c, *mods).unwrap_or(*c)),
                    false,
                )),
                TKey::Named(TNamed::Space) => Some((Some(' '), false)),
                TKey::Named(TNamed::Backspace) => Some((None, true)),
                _ => None,
            }
        }
        _ => None,
    };
    let typed_forward: Option<bool> = match ev {
        InputEvent::Key { key, mods, .. }
            if !mods.contains(TMods::CTRL)
                && !mods.contains(TMods::ALT)
                && !mods.contains(TMods::SUPER) =>
        {
            match key {
                TKey::Character(_) => Some(true),
                TKey::Named(TNamed::Space | TNamed::Enter) => Some(true),
                TKey::Named(TNamed::Backspace) => Some(false),
                _ => None,
            }
        }
        // A committed IME run is typed text: its echo advances the caret — and
        // can WRAP an Ink box exactly like a plain Character, so it must arm
        // the re-anchor hint.
        InputEvent::Text(t) if !t.is_empty() => Some(true),
        _ => None,
    };
    let navigation_key: bool = match ev {
        InputEvent::Key { key, mods, .. } => {
            let ctrl = mods.contains(TMods::CTRL);
            let alt = mods.contains(TMods::ALT);
            match key {
                TKey::Named(
                    TNamed::ArrowLeft
                    | TNamed::ArrowRight
                    | TNamed::ArrowUp
                    | TNamed::ArrowDown
                    | TNamed::Home
                    | TNamed::End
                    | TNamed::PageUp
                    | TNamed::PageDown,
                ) => true,
                // Ctrl-P/N join the emacs line motions: shell
                // history recall / vim-emacs line moves are the
                // SAME (suppressed) gesture as Up/Down-arrow
                // recall, so both spellings must classify alike.
                TKey::Character(c) if ctrl && matches!(c, 'a' | 'e' | 'b' | 'f' | 'p' | 'n') => {
                    true
                }
                TKey::Character(c) if alt && matches!(c, 'b' | 'f') => true,
                _ => false,
            }
        }
        _ => false,
    };
    let (kill_key, kill_moves): (bool, bool) = match ev {
        InputEvent::Key { key, mods, .. } => {
            let ctrl = mods.contains(TMods::CTRL);
            let alt = mods.contains(TMods::ALT);
            match key {
                TKey::Character(c) if ctrl && *c == 'k' => (true, false),
                TKey::Character(c) if ctrl && matches!(c, 'u' | 'w') => (true, true),
                TKey::Character(c) if alt && *c == 'd' => (true, false),
                TKey::Named(TNamed::Backspace) if ctrl || alt => (true, true),
                TKey::Named(TNamed::Delete) => (true, false),
                _ => (false, false),
            }
        }
        _ => (false, false),
    };
    let enter_like = matches!(
        ev,
        InputEvent::Key {
            key: TKey::Named(TNamed::Enter),
            ..
        }
    );
    let tab_key = matches!(
        ev,
        InputEvent::Key {
            key: TKey::Named(TNamed::Tab),
            ..
        }
    );
    let mut typed: Option<char> = None;
    let mut ime: Option<&str> = None;
    let mut backspace = false;
    let mut brk = false;
    match ev {
        InputEvent::Key { key, mods, .. } => {
            let chorded = mods.contains(TMods::CTRL)
                || mods.contains(TMods::ALT)
                || mods.contains(TMods::SUPER);
            match key {
                TKey::Character(c) if !chorded => {
                    // The SHIFTED glyph the encoder sends
                    // (`Key::Character` holds the unshifted
                    // base) — folded to lowercase inside
                    // the detector, so KITTY still counts.
                    typed = Some(aterm_types::keyboard::shifted_character(*c, *mods).unwrap_or(*c));
                }
                TKey::Named(TNamed::Space) if !chorded => typed = Some(' '),
                TKey::Named(TNamed::Backspace) if !chorded => backspace = true,
                // A chorded Character is an editing/nav/
                // kill chord (Ctrl-U/W/K, Ctrl-A/E, Alt-B/
                // F, …): it rewrote or left the word.
                TKey::Character(_) => brk = true,
                TKey::Named(
                    TNamed::Enter
                    | TNamed::Tab
                    | TNamed::Escape
                    | TNamed::Delete
                    | TNamed::Backspace
                    | TNamed::ArrowLeft
                    | TNamed::ArrowRight
                    | TNamed::ArrowUp
                    | TNamed::ArrowDown
                    | TNamed::Home
                    | TNamed::End
                    | TNamed::PageUp
                    | TNamed::PageDown,
                ) => brk = true,
                // Anything else (F-keys, lone modifiers,
                // media keys) neither types nor edits.
                _ => {}
            }
        }
        // A committed IME run is typed text.
        InputEvent::Text(t) if !t.is_empty() => ime = Some(t),
        // Raw controller byte payloads are not classified
        // typing — break the run rather than guess.
        InputEvent::KeySequence(_) => brk = true,
        _ => {}
    }
    PressClass {
        predict_candidate,
        typed_forward,
        navigation_key,
        kill_key,
        kill_moves,
        enter_like,
        tab_key,
        typed,
        ime,
        backspace,
        brk,
    }
}

/// The shared brk → backspace → typed+ime dispatch cascade of one classified
/// press into a cosmetic feed. `on_char` runs for the bare typed glyph and for
/// each char of a committed IME run; `target` threads the feed's receiver
/// through so the three callbacks never hold simultaneous borrows of it.
#[derive(Clone, Copy)]
struct CosmeticFeedPress<'ev> {
    brk: bool,
    backspace: bool,
    typed: Option<char>,
    ime: Option<&'ev str>,
}

fn feed_classified_press<T>(
    target: &mut T,
    press: CosmeticFeedPress<'_>,
    on_break: impl FnOnce(&mut T),
    on_backspace: impl FnOnce(&mut T),
    mut on_char: impl FnMut(&mut T, char),
) {
    if press.brk {
        on_break(target);
    } else if press.backspace {
        on_backspace(target);
    } else {
        if let Some(c) = press.typed {
            on_char(target, c);
        }
        if let Some(t) = press.ime {
            for c in t.chars() {
                on_char(target, c);
            }
        }
    }
}

impl App {
    /// Phase 0.5 — the App::input CONVERGENCE SEAM (design Addendum A.2).
    ///
    /// The SOLE policy site for input egress. The byte-producing core lives in the
    /// source-blind [`input::seam_egress`] (the ONLY byte-producing reader of
    /// `keyboard_mode()` / `mouse_tracking_enabled()` — the predictive-echo gate's
    /// `kitty_suppresses_predictive_echo()` sample below is a read-only DISPLAY
    /// projection that never feeds an encoder — and the ONLY caller of
    /// `encode_key_with_layout` /
    /// the `encode_mouse_*` family / `encode_committed_text` / `format_paste` / the
    /// focus-report egress, reading the relevant mode ONCE per event under a single
    /// `term_lock` — so no mode flip can land mid-event — and ending at
    /// `self.sink.write_frame`, the 0e floor). This
    /// method wraps it with the viewport/gesture/clipboard/geometry side-effects
    /// that need the renderer + window + gesture state: it is the ONLY caller of
    /// `seam_egress` / `scroll_display` / `clear_selection` / `snap_to_bottom` /
    /// `reset_blink` / `apply_term_resize`.
    ///
    /// `src` is recorded for audit and NEVER branched on: `seam_egress` takes no
    /// `Source`, so the bytes a Human and a Controller produce for the SAME
    /// `InputEvent` are byte-identical (the indistinguishability invariant, proven
    /// by `input::tests::bytes_human_eq_controller`).
    pub(crate) fn input(&mut self, wid: WindowId, ev: InputEvent, src: Source) -> InputOutcome {
        self.input_to_session(wid, ev, src, None)
    }

    /// Whether tone-of-typing inference may run AT ALL: the `tone_melody`
    /// knob, the trail-sound config gates (master toggle + nonzero volume —
    /// a muted synth needs no mood), and a LIVE trail-audio worker. The last
    /// conjunct is the "never runs headless-muted" policy: a headless app,
    /// a non-macOS build (inert audio stub), or a permanently failed audio
    /// worker never spends a cycle on the classifier, never loads its
    /// weights, never even buffers chars. Focus and reduced-motion need no
    /// check HERE — the tone only rides sound events those policies already
    /// gate at the drain seams.
    fn tone_infer_active(&self) -> bool {
        self.trail_audio.is_live()
            && self.config.tone_melody_or_default()
            && self.config.trail_sounds_or_default()
            && self.config.trail_sound_volume() > 0.0
    }

    /// Summon the typed-"kitty" cameo (detector: [`crate::kitty_summon`]).
    ///
    /// PRESENTATION reuses the terminal's existing cat machinery wholesale:
    /// the cursor companion's guaranteed bounded hello
    /// ([`crate::kitty_cursor::CursorCat::on_collect`]) — the same peeking-head
    /// appearance near the cursor a collection discovery gets. No new
    /// renderer, no new art rolls: the cameo wears the CURRENT collected
    /// companion identity (or the default peeking head over a fresh ledger),
    /// draws for EVERY trail style (`collection_hello` bypasses the
    /// Rainbow kitty-style gate in `redraw_window`), pauses while the window cannot
    /// present it, renders as the one static held pose under reduced motion,
    /// and is inert with the sparkle-words master off OR the
    /// `[sparkle_words.feline]` family off — every gate the ambient
    /// sightings already respect.
    ///
    /// LOGGING records ONE synthetic sighting through the Kitty Log's
    /// ordinary `(session, ident)` episode/dedupe rules. The registry's
    /// shown-type keys are CI-pinned for ledger compatibility
    /// (`kitty_registry` pins them; a new key would orphan persisted rows),
    /// so there is deliberately NO distinct "summoned" marker: the cameo logs
    /// as the `head_peek` type it visually is, in the same aggregation cell
    /// ambient on-screen "kitty" word-cats land in (langs resolved from the
    /// live lexicon's own "kitty" entry — the file stays bounded by the
    /// vocabulary, no new row shape).
    ///
    /// `record` is the LEDGER tier: `false` when the Kitty Log's cooldown has
    /// not elapsed. The cameo draws either way — typing the word is a direct
    /// request, and answering it only sometimes reads as broken. See
    /// `kitty_summon`.
    fn summon_typed_kitty(
        &mut self,
        wid: WindowId,
        session: u64,
        now: std::time::Instant,
        record: bool,
    ) {
        use aterm_effects::kitty_registry::{
            KittyMagic, KittyShownAs, KittySighting, KittyType, TRAIT_BOW, TRAIT_CROWN,
        };
        // EFFECTS MASTER GATE: with sparkle words off (config or the panic
        // toggle) no cat machinery can draw — `kitty_enabled` requires
        // `sparkle_on` — so the summon is wholly inert, exactly like the
        // ambient sightings it mirrors (nothing rendered ⇒ nothing logged).
        let Some(rs) = self.sparkle.as_ref() else {
            return;
        };
        // FELINE SUB-GATE: `[sparkle_words.feline]
        // enabled = false` leaves the master resolved (any other family keeps
        // `sparkle` Some) but silences every ambient cat — the scanner drops
        // `Class::Feline` matches before they can render or log, so an
        // ambient Kitty Log sighting is impossible under that config. The
        // typed summon must mirror that exactly: family off ⇒ fully inert —
        // no cameo (the `collection_hello` render gate checks only the
        // master, so the cut must happen HERE, before `on_collect` arms it)
        // and no ledger row (`feline.log` gates HOW sightings record;
        // `feline.enabled` gates WHETHER cats exist at all — otherwise the
        // ledger's aggregation cells accumulate synthetic sightings of a
        // category the user's config can never produce ambiently).
        if !rs.cfg.feline {
            return;
        }
        // Same resolution order as the per-frame sync in `app_render`: an
        // explicitly collected companion wins, otherwise this session's own
        // kitty. Never `KittyLook::default()` — that is one cat shared by
        // every session.
        let look = self
            .kitty_log
            .companion_look()
            .unwrap_or_else(|| aterm_effects::kitty_registry::KittyLook::for_session(session))
            .normalized();
        // Displayed-trait bits mirror the word renderer's recorder exactly:
        // only the overlay accessories the hello actually draws are counted.
        let traits = match look.accessory {
            Some(aterm_effects::cat_glyphs_gen::CatGlyphId::AccBow) => TRAIT_BOW,
            Some(aterm_effects::cat_glyphs_gen::CatGlyphId::AccCrown) => TRAIT_CROWN,
            _ => 0,
        };
        // LEDGER TIER. Skipped wholesale inside the cooldown: no lexicon scan,
        // no ident minted, no `observe` — the rate limit costs nothing rather
        // than doing the work and dropping it. The CAMEO below runs either way.
        let discovered = if record {
            // The same language chips an on-screen "kitty" earns, from the SAME
            // lexicon build the drain will resolve codes against. A lexicon that
            // somehow lost the word degrades to the empty set ("unknown") rather
            // than skipping the record.
            let langs = rs
                .lexicon
                .scan("kitty", &aterm_lexicon::ScanOptions::default())
                .into_iter()
                .find(|m| matches!(m.class, aterm_lexicon::Class::Feline))
                .map_or(aterm_lexicon::LangSet::EMPTY, |m| m.langs);
            // Fresh `(session, ident)` per RECORDED summon: the App-wide sequence
            // keeps two windows sharing one session from minting colliding
            // episodes, and the tag namespaces summons away from the word
            // renderer's position-bearing occurrence idents. Only bumped when a
            // row is actually minted, so the namespace stays dense.
            let ident = next_kitty_summon_ident(
                &mut self.kitty_summon_seq,
                crate::kitty_summon::TYPED_SUMMON_IDENT_TAG,
            );
            let sighting = KittySighting {
                kitty_type: KittyType::HeadPeek,
                magic: KittyMagic::None,
                shown_as: KittyShownAs::Cat,
                langs,
                traits,
                look,
                ident,
            };
            // `kitty_log_on = false` drains-and-drops here exactly as at the
            // render drain: the cameo still shows, nothing is recorded.
            let enabled = self.kitty_log_enabled();
            self.kitty_log
                .observe(session, [sighting], &rs.lexicon, now, enabled)
        } else {
            None
        };
        if let Some(ws) = self.windows.get_mut(&wid) {
            // TWO REASONS, TWO PRESENTATIONS — the companion identity may only
            // change for a reason.
            //
            // * A first-ever sighting is a genuine discovery — a real reason to
            //   swap. Present that exact unlocked identity, like the render
            //   drain would.
            // * Merely typing the word is NOT a reason. `on_collect` replaces
            //   `look` outright (the two-path rule's path 2), so routing a
            //   typed `kitty` through it would re-skin the companion out from
            //   under the session. `on_summon` gives the identical
            //   appearance/hold WITHOUT touching the identity, so the session's
            //   own kitty is the one that shows up.
            match discovered {
                Some(unlocked) => ws.cursor_cat.on_collect(now, unlocked),
                None => ws.cursor_cat.on_summon(now, 1),
            }
            // A no-echo prompt (password read) repaints nothing on its own —
            // one explicit wake lets the hello's first frame present (full
            // motion then owns the cadence; reduced motion arms its single
            // erase deadline via `static_deadline`).
            if let Some(w) = ws.os_window.as_ref() {
                w.request_redraw();
            }
        }
    }

    /// Promote the FRONT SESSION's own kitty into the durable kitty registry
    /// and pin it as the companion. The menu-bar item, the ⇧⌘P palette row, and
    /// `aterm-ctl invoke FavouriteSessionKitty` all land here.
    pub(crate) fn favourite_session_kitty(&mut self, wid: WindowId, now: std::time::Instant) {
        use aterm_effects::kitty_registry::{
            KittyLook, KittyMagic, KittyShownAs, KittySighting, KittyType,
        };
        // EFFECTS MASTER GATE, then the FELINE SUB-GATE — the exact pair
        // `summon_typed_kitty` documents: nothing can draw ⇒ nothing may log,
        // and `feline.enabled = false` means cats do not exist, so a synthetic
        // row for a category the config can never produce would poison the
        // ledger.
        let Some(rs) = self.sparkle.as_ref() else {
            return;
        };
        if !rs.cfg.feline {
            return;
        }
        let Some(session) = self.focused_session_id(wid) else {
            return;
        };
        // THE SESSION KITTY, deliberately not `companion_look()`. Once ANYTHING
        // is collected the companion wins for every window (`app_render`), so
        // favouriting "the cat I can see" could never promote a session kitty —
        // on a used ledger it would only ever re-pin the cat that already won.
        let look = KittyLook::for_session(session);
        let enabled = self.kitty_log_enabled();
        let ident = next_kitty_summon_ident(
            &mut self.kitty_summon_seq,
            crate::kitty_summon::FAVOURITE_IDENT_TAG,
        );
        let sighting = KittySighting {
            kitty_type: KittyType::HeadPeek,
            magic: KittyMagic::None,
            shown_as: KittyShownAs::Cat,
            // No matched surface — a favourite is a button press, not a word —
            // so "unknown" is the honest primary language rather than a
            // borrowed chip from a scan that never happened.
            langs: aterm_lexicon::LangSet::EMPTY,
            // `for_session` never mints an accessory, so no overlay was drawn
            // and no displayed-trait bit may be claimed.
            traits: 0,
            look,
            ident,
        };
        self.kitty_log
            .favourite(&sighting, &rs.lexicon, now, enabled);
        if let Some(ws) = self.windows.get_mut(&wid) {
            // TWO REASONS, TWO PRESENTATIONS: a favourite IS a reason to change
            // the identity, so it takes path 2 (`on_collect`) like a discovery
            // hello, rather than the identity-preserving `on_summon`.
            ws.cursor_cat.on_collect(now, look);
            // A no-echo prompt repaints nothing on its own — one explicit wake
            // lets the hello's first frame present.
            if let Some(w) = ws.os_window.as_ref() {
                w.request_redraw();
            }
        }
    }

    /// Deliver a typed-word reaction to the window's cursor companion.
    ///
    /// EXPRESSION ONLY. Neither family touches `look`, the flight lifetime, or
    /// typing momentum — the session's kitty keeps its identity through every
    /// reaction. The feline arm is handled by `summon_typed_kitty`'s
    /// `on_summon` (which also wakes a hidden companion, because typing the
    /// word must make a kitty appear); this adds the profanity wince, whose
    /// screen-side twin is `word_decorations`'s `CurseCue`. A curse typed into
    /// a NO-ECHO prompt never reaches that scanner, so the input path owes the
    /// reaction independently.
    fn react_typed_word(
        &mut self,
        wid: WindowId,
        now: std::time::Instant,
        hit: crate::kitty_summon::TypedHit,
    ) {
        // Same master + family gates the ambient sightings respect: with
        // sparkle words or the feline family off, no cat machinery may move.
        let Some(rs) = self.sparkle.as_ref() else {
            return;
        };
        if !rs.cfg.feline {
            return;
        }
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        // `on_curse` is a no-op on a hidden companion by design — a curse is a
        // reaction, never a summon. Only a redraw the reaction actually earned
        // is requested.
        if hit.profanity
            && ws.cursor_cat.on_curse(now, 1)
            && let Some(w) = ws.os_window.as_ref()
        {
            w.request_redraw();
        }
    }

    /// Deliver a held-key payload to a session that is no longer frontmost in
    /// any window. Terminal-side semantics (snap live viewport, clear terminal
    /// selection, preserve paste FIFO ordering) still apply, but no visible
    /// window animator/predictor/cursor state may be touched by hidden input.
    fn input_to_hidden_session(&self, target_session: u64, ev: InputEvent) -> InputOutcome {
        debug_assert!(matches!(
            ev,
            InputEvent::Key { .. } | InputEvent::Text(_) | InputEvent::KeySequence(_)
        ));
        let Some(session) = self.pool.get(target_session) else {
            return InputOutcome::Ok;
        };
        let (term, sink) = (session.term.clone(), session.ctx.sink.clone());
        let is_release = matches!(
            &ev,
            InputEvent::Key { event_type, .. }
                if matches!(event_type, aterm_types::keyboard::KeyEventType::Release)
        );
        if !is_release {
            let mut terminal = term_lock(&term);
            terminal.scroll_to_bottom();
            if terminal.text_selection().has_selection() {
                terminal.text_selection_mut().clear();
            }
        }
        let (egress, _) =
            paste_order::ordered_or_inline(&term, &sink, &ev, input::EgressMode::Interactive);
        egress_to_outcome(egress)
    }

    /// Route an input event through the complete convergence seam while pinning
    /// its terminal capability to `target_session`. This private override exists
    /// solely for a physical key hold whose press already established immutable
    /// session ownership: focus or tab changes must not retarget later repeats.
    /// `None` is the ordinary path and resolves the current front terminal.
    pub(crate) fn input_to_session(
        &mut self,
        wid: WindowId,
        ev: InputEvent,
        src: Source,
        target_session: Option<u64>,
    ) -> InputOutcome {
        // Every real input advances the automatic-update quiet clock, but a
        // PENDING overlap BUFFERS THROUGH byte-producing input rather than
        // revoking on it. Keys/text/raw sequences encode against the (frozen)
        // terminal modes and write to the PTY masters, which persist across
        // the whole overlap — the shell receives the bytes exactly once and
        // the echo waits in the kernel queue for the child's fresh parser.
        // ACCEPTED BOUND (encoding-mode staleness): a mode-changing escape
        // (kitty keyboard push/pop, bracketed paste) can sit unread in that
        // queue while we encode against the pre-overlap modes. The divergence
        // window is the overlap itself — admission required ≥500 ms of output
        // quiet, so a mid-handshake mode flip is already rare — and it is
        // exactly the same staleness any fast typist races against a live
        // shell; the terminal re-converges when the child drains the queue.
        // Focus reports are pure notifications under the same modes. Anything
        // that can CHANGE terminal modes or presentation mid-overlap — paste
        // (bracketed-mode implications), mouse, wheel, scroll, resize —
        // still revokes, exactly like the structural window events.
        match &ev {
            InputEvent::Key { .. }
            | InputEvent::Text(_)
            | InputEvent::KeySequence(_)
            | InputEvent::Focus(_) => self.note_update_handoff_tolerated_activity(),
            _ => self.note_update_handoff_activity(),
        }
        // AUDIT-ONLY: bind `src` so the one allowed use (a future §7.5 audit log)
        // is obvious and so a stray behavioural `match src` would stand out in
        // review. It must NEVER gate bytes. The byte-producing core
        // (`input::seam_egress`) takes NO `Source` at all — it is structurally
        // impossible for it to branch (the Tier-1 invariant; the `Buggy` mutant
        // proves the test has teeth).
        let _audit = src;
        // A discrete input attempt is an EXTERNAL recovery stimulus. If this
        // window parked after exhausting its finite surface-retry train, the
        // next user/controller action grants one fresh bounded episode so a
        // recovered drawable cannot leave the UI frozen forever. PTY OUTPUT
        // does not pass this seam and therefore cannot manufacture retry fuel
        // in a producer-driven loop. Key releases and pointer-motion streams
        // are not new intent; clicks/wheels/keys remain recovery edges.
        if is_present_recovery_stimulus(&ev)
            && let Some(ws) = self.windows.get_mut(&wid)
        {
            let recovery_window = ws.os_window.clone();
            let _ = crate::rearm_present_and_request(&mut ws.present_retry, false, || {
                if let Some(window) = recovery_window {
                    // In no-echo/app-owned-keyboard modes this input may
                    // produce no predictor pixel and no PTY output. The
                    // recovery edge must drive its promised attempt.
                    window.request_redraw();
                }
            });
        }
        // ORPHAN-RELEASE pairing at the seam (the engine-level twin of `on_key`'s
        // `consumed_press_keys`, keyed on the engine `Key` because controller events
        // carry no physical key): a Key RELEASE whose PRESS the overlay gate below
        // consumed is swallowed HERE — including one arriving AFTER the overlay closed,
        // which is exactly the case the gate itself can no longer see. Otherwise a
        // controller press swallowed under the overlay leaks a default-encoded release
        // once the overlay closes: an orphan Kitty `REPORT_EVENT_TYPES` release report
        // for a press the app never saw. Checked BEFORE the gate so the entry is always
        // removed (swallow once, leak-free); a legacy release encodes to nothing anyway,
        // so this is a byte-identical no-op outside Kitty event-type mode.
        if let InputEvent::Key {
            key, event_type, ..
        } = &ev
            && matches!(event_type, aterm_types::keyboard::KeyEventType::Release)
            && self
                .windows
                .get_mut(&wid)
                .is_some_and(|ws| ws.overlay_consumed_keys.remove(key))
        {
            return InputOutcome::Ok;
        }
        // SETTINGS MODAL: while the overlay owns this window, swallow PTY-bound input.
        // Human keys/clicks are already gated in `on_key`/`on_mouse_input`; CONTROLLER
        // bytes arrive HERE via `Wake::Input` (bypassing `on_key`), so the modal must
        // also gate this convergence seam. Gated on MODAL STATE ONLY — never on `src` —
        // so a Human and Controller producing the same event are swallowed identically
        // (the indistinguishability invariant holds). Geometry (`Resize`) and `Focus`
        // still flow so the window keeps tracking size + focus under the panel.
        if target_session.is_none()
            && self.windows.get(&wid).is_some_and(|ws| ws.overlay_open())
            && matches!(
                ev,
                InputEvent::Key { .. }
                    | InputEvent::KeySequence(_)
                    | InputEvent::Text(_)
                    | InputEvent::MouseButton { .. }
                    | InputEvent::MouseMove { .. }
                    | InputEvent::Wheel { .. }
                    | InputEvent::Paste(_)
                    | InputEvent::ScrollView(_)
            )
        {
            // A Key RELEASE reaching the gate UNTRACKED pairs with a press that PREDATES
            // the overlay (the press report already reached the PTY; the overlay opened
            // mid-hold via a menu click / aterm-ctl): let it FALL THROUGH to the normal
            // egress below so a Kitty `REPORT_EVENT_TYPES` app is not left with an orphan
            // press. Swallowing it bought nothing — all four overlay handlers ignore
            // releases. Tracked releases were already removed + swallowed above.
            let untracked_release = matches!(
                &ev,
                InputEvent::Key { event_type, .. }
                    if matches!(event_type, aterm_types::keyboard::KeyEventType::Release)
            );
            if !untracked_release {
                // Record a Key PRESS the overlay consumes so its RELEASE (possibly after
                // the overlay closes) is swallowed by the pre-gate check above. PRESS
                // only — a REPEAT of a pre-overlay press (overlay opened mid-hold) must
                // not poison the pairing: its press reached the PTY, so its release must
                // too (the same repeat discipline as `note_consumed_press`).
                if let InputEvent::Key {
                    key, event_type, ..
                } = &ev
                    && matches!(event_type, aterm_types::keyboard::KeyEventType::Press)
                    && let Some(ws) = self.windows.get_mut(&wid)
                {
                    ws.overlay_consumed_keys.insert(key.clone());
                }
                // Route key/text to the OPEN overlay (Settings, About, or the command Palette) so
                // a CONTROLLER navigates it exactly as a Human does (the winit `on_key` path
                // already drove it before reaching here; controller bytes arrive only at this
                // seam). Dispatch is by the ONE live variant — no gate ORDERING (only one overlay
                // can be open), so a hidden surface can never swallow keys. Then swallow from PTY.
                use crate::overlay::OverlayKind;
                match self
                    .windows
                    .get(&wid)
                    .and_then(|ws| ws.overlay.as_ref())
                    .map(|o| o.kind())
                {
                    Some(OverlayKind::Palette) => self.palette_input_event(wid, &ev),
                    #[cfg(test)]
                    Some(OverlayKind::About) => self.about_input_event(wid, &ev),
                    #[cfg(test)]
                    Some(OverlayKind::Update) => self.update_input_event(wid, &ev),
                    #[cfg(test)]
                    Some(OverlayKind::Settings) => self.settings_input_event(wid, &ev),
                    None => {}
                }
                return InputOutcome::Ok;
            }
        }
        // NATIVE TAB INPUT BOUNDARY: the active first-party app owns text,
        // keyboard, paste, pointer, and local scrolling. Route both Human and
        // Controller events through its reducer and never let an unhandled app
        // gesture leak bytes into the parked terminal session underneath. Focus
        // and Resize remain window properties and intentionally continue through
        // the host seam below.
        if target_session.is_none() && self.native_input_event(wid, &ev) {
            return InputOutcome::Ok;
        }
        // Presentation side effects follow an ACTUAL current view of the owned
        // session, never the window identity captured at press time after that
        // window switched tabs/native content. If no view currently fronts the
        // session, use the hidden-session seam above so bytes remain correctly
        // routed without arming phantom blink/cat/trail/predictor state elsewhere.
        let wid = if let Some(target) = target_session {
            let presented = self
                .front_terminal(wid)
                .filter(|terminal| terminal.session == target)
                .map(|_| wid)
                .or_else(|| {
                    self.windows.keys().copied().find(|candidate| {
                        self.front_terminal(*candidate)
                            .is_some_and(|terminal| terminal.session == target)
                    })
                });
            let Some(presented) = presented else {
                return self.input_to_hidden_session(target, ev);
            };
            presented
        } else {
            wid
        };
        // Resolve the optional canonical terminal capability. Native focus has
        // no fallback shell; resize remains a window property and focus simply
        // has no DEC-1004 PTY report to emit. The owning SESSION id rides
        // along: the typed-"kitty" detector keys its rolling keystroke window
        // to it (letters typed into different sessions never assemble one
        // word) — the same id the render drain hands the Kitty Log.
        let terminal = match target_session {
            Some(session) => self
                .pool
                .get(session)
                .map(|owner| (owner.term.clone(), owner.ctx.sink.clone(), session)),
            None => self
                .front_terminal_mirror(wid)
                .map(|terminal| (terminal.term, terminal.sink, terminal.session)),
        };
        let Some((term, sink, session)) = terminal else {
            return match ev {
                InputEvent::Resize {
                    rows,
                    cols,
                    echo_to_window,
                } => self.input_resize(wid, rows, cols, echo_to_window),
                InputEvent::ResizeWindowPx { width, height } => {
                    self.input_resize_window_px(wid, width, height)
                }
                InputEvent::Focus(_) => InputOutcome::Ok,
                _ => InputOutcome::Ok,
            };
        };
        match ev {
            // --- Keyboard egress (kills f/h; uniform k/g side-effects) ---------
            ev @ (InputEvent::Key { .. } | InputEvent::Text(_) | InputEvent::KeySequence(_)) => {
                // blink reset -> viewport snap -> selection clear run for BOTH
                // sources (divergences d/g/k): controller key verbs snap +
                // deselect + keep the cursor solid exactly like human typing. The
                // snap + clear are inlined under ONE term lock below (with the
                // predictor's cursor sample) rather than calling the per-concern
                // helpers, so a press costs one mutex acquisition. The ENCODE
                // (sole keyboard-mode read + encoder call) is `seam_egress`.
                // A key RELEASE report (Kitty REPORT_EVENT_TYPES) is NOT a press/typing
                // event, so it must not reset the blink, snap the viewport, or clear the
                // selection — only encode. (A Repeat IS press-like and keeps them.) The
                // encoder emits nothing for a release outside Kitty mode, so legacy
                // output is unaffected whether or not this side-effect gate runs.
                let is_release = matches!(
                    &ev,
                    InputEvent::Key { event_type, .. }
                        if matches!(event_type, aterm_types::keyboard::KeyEventType::Release)
                );
                // One clock sample per committed input event. Every coupled effect
                // (cadence, ribbon, cat, predictor, and the post-write cosmetic
                // feeds) advances from the same instant, avoiding tiny phase seams
                // and repeated clock reads on the latency-critical key path. `Some`
                // is exactly the `!is_release` gate: a release still pays no clock
                // read and runs no press side-effect.
                let input_now = (!is_release).then(std::time::Instant::now);
                // ONE pure classification of this press (see `PressClass`),
                // shared by the pre-egress hint/predictor block and the
                // post-egress cosmetic feeds.
                let PressClass {
                    predict_candidate,
                    typed_forward,
                    navigation_key,
                    kill_key,
                    kill_moves,
                    enter_like,
                    tab_key,
                    typed,
                    ime,
                    backspace,
                    brk,
                } = classify_press(&ev);
                if let Some(input_now) = input_now {
                    self.reset_blink(wid);
                    // Predictive local echo (mosh-style): register the classified
                    // `predict_candidate` glyph so it can paint before the shell
                    // echoes it. Resolved BEFORE the term lock below (pure, no term
                    // state), so a non-printable key never pays for the cursor
                    // sample. Inert unless `predictive_echo` is enabled; `pmode` is
                    // resolved once per config generation (invalidated by
                    // `reload_config`) instead of re-parsing the config string every
                    // keystroke — the shared cache the render paths read too.
                    let pmode = self.predict_mode();
                    let act: Option<(Option<char>, bool)> =
                        if pmode == crate::predict::PredictMode::Off {
                            None
                        } else {
                            predict_candidate
                        };
                    let typed_enter = is_plain_enter(&ev);
                    // Momentum can only arm the cursor cat while the trail
                    // master is ON and the selected style is rainbow kitty. The master
                    // owns both the ribbon and its ordinary flying companion;
                    // collection/typed hellos are activated separately below
                    // this gate by the log and remain intentionally independent,
                    // so they stay interactive without paying an invisible
                    // 60 fps loop.
                    // Cached like `pmode` above (invalidated by `reload_config`): the
                    // trail style only changes on a config reload.
                    let kitty_cursor_enabled = match self.kitty_cursor_enabled_cache {
                        Some(n) => n,
                        None => {
                            let n = crate::app_render::ordinary_kitty_cursor_enabled(
                                self.config.cursor_trail_or_default(),
                                crate::cursor_glow::GlowStyle::parse(
                                    self.config.cursor_trail_style_raw(),
                                ),
                            );
                            self.kitty_cursor_enabled_cache = Some(n);
                            n
                        }
                    };
                    // KEY-TIME TYPING CLICK availability (see the seam's own doc):
                    // resolved HERE because these are App-level knobs while the cue
                    // below is taken under a window borrow. Three Option reads — the
                    // same trivially-cheap accessors the per-frame drain calls.
                    let click_audible = keystroke_click_audible(
                        self.trail_audio.is_live(),
                        self.config.trail_sounds_or_default(),
                        self.config.trail_sound_volume(),
                        self.serious_mode_policy()
                            .allows(crate::motion::SeriousEffect::TerminalSound),
                        self.windows
                            .get(&wid)
                            .is_some_and(|ws| ws.resize_sound_quiet(input_now)),
                    );
                    // ONE term-lock scope for every press-path terminal touch: the
                    // viewport snap, the "typing deselects" clear, and the predictor's
                    // cursor/cols/alt sample. These were three separate acquisitions
                    // (snap_to_bottom, clear_selection, a sample lock), each queueing
                    // independently behind the PTY reader's 8 KiB process() bouts
                    // during output floods. The redraw side-effects run AFTER the
                    // guard drops — never window calls under the term lock.
                    let (scrolled, cleared, sample, is_alt, kitty_owns_keyboard) = {
                        let mut t = term_lock(&term);
                        let scrolled = if t.grid().display_offset() != 0 {
                            t.scroll_to_bottom();
                            true
                        } else {
                            false
                        };
                        let cleared = if t.text_selection().has_selection() {
                            t.text_selection_mut().clear();
                            true
                        } else {
                            false
                        };
                        // The NARROW `REPORT_EVENT_TYPES | REPORT_ALL_KEYS_AS_ESC`
                        // projection, read ONCE here (it feeds both the predictor's
                        // no-echo gate below and the release-relevance publication
                        // after this scope) — a free rider on a lock the press has
                        // to take anyway.
                        let kitty_owns_keyboard = t.kitty_suppresses_predictive_echo();
                        let sample = act.is_some().then(|| {
                            // The predictor's no-echo gate: the ALT screen (vim/less own
                            // the cursor, no line echo), Kitty REPORT_ALL_KEYS_AS_ESC, or
                            // Kitty REPORT_EVENT_TYPES. The latter is the Codex shape:
                            // main screen, app-owned composer, press/repeat/release input,
                            // and full-line repaints. A terminal ghost there can duplicate
                            // the app's text and blink out at the 250 ms self-heal. Keep
                            // disambiguate-only shells eligible: fish can push that flag at
                            // a line-echoing prompt. This narrow read-only projection is a
                            // display gate only; the seam remains the sole encoder-feeding
                            // reader of the full keyboard mode.
                            // `rows` rides along so the predictor can continue a guess
                            // ACROSS the right margin at `(row + 1, 0)` instead of
                            // declining there — long command lines are exactly where an
                            // ssh user types ahead most. Height is what bounds it: a
                            // wrap off the LAST row has nowhere to go and still declines.
                            (
                                t.cursor(),
                                (t.cols(), t.rows()),
                                t.is_alternate_screen() || kitty_owns_keyboard,
                            )
                        });
                        // Read once under the SAME lock for the PHOSPHOR PgUp/
                        // PgDn reading gate below (no second acquisition).
                        let is_alt = t.is_alternate_screen();
                        (scrolled, cleared, sample, is_alt, kitty_owns_keyboard)
                    };
                    // Publish what this press learned so the matching key RELEASE can
                    // decide LOCK-FREE whether it has anything to encode. Without a
                    // negotiated `REPORT_EVENT_TYPES` the encoder returns an empty vec
                    // for every release, yet the release path still took the terminal
                    // mutex — contended with the PTY reader — once per key-up to
                    // produce nothing. `kitty_owns_keyboard` is a conservative SUPERSET
                    // of that flag, so `false` PROVES the release is byte-silent.
                    // Sampling at the PRESS is the same pairing law the physical-press
                    // owner table already enforces (a release replays its press-time
                    // key/mods/base_layout, and a release whose press produced no bytes
                    // is swallowed outright): the release follows the negotiation its
                    // press saw, not a re-read taken a keystroke later.
                    publish_release_relevance(session, kitty_owns_keyboard);
                    // Change-gated repaint, exactly as the snap_to_bottom /
                    // clear_selection helpers gate theirs: an unconditional wake
                    // would be pure overhead on the hottest typing path.
                    if (scrolled || cleared)
                        && let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref())
                    {
                        w.request_redraw();
                    }
                    // Keystroke-priority frame pacing: the NEXT output burst (almost
                    // certainly this key's echo) presents immediately instead of
                    // waiting out the bulk-output coalescing cap — see the
                    // `Wake::Output` handler. State-only (never gates bytes), set for
                    // BOTH sources like the side-effects above, and OUTSIDE any term
                    // lock.
                    if let Some(ws) = self.windows.get_mut(&wid) {
                        ws.input_hot = true;
                        // ECHO-CORRELATION DEADLINE. Clearing the bypass on the FIRST
                        // content present after the key would not correlate it with THIS
                        // keystroke: in a session that is already streaming (a build log,
                        // a TUI) the next spew frame spends the bypass and re-stamps
                        // `last_present_at`, so the real echo — arriving milliseconds
                        // later — is paced against a FULL frame interval measured from
                        // that spew frame, and consecutive keystrokes land at 8/16/8 ms.
                        //
                        // A deadline is the honest correlation: every key re-arms it, so
                        // the bypass covers the whole typing burst rather than one frame,
                        // and it fails OPEN after the window so a key that never echoes
                        // (a password prompt, a swallowed chord) cannot pin it. Still
                        // half-interval paced by `hot_floor`, so the present rate stays
                        // bounded at 2x refresh — the pool-exhaustion guard is untouched.
                        ws.input_hot_until = Some(input_now + INPUT_HOT_WINDOW);
                        crate::metrics::note_input();
                        // Stamp the arrival for the `metrics` verb's input→present
                        // slice — the latency a human FEELS when typing. The same
                        // stamp drives `input_pending()`, which keeps the PTY reader
                        // on the short THRU-2 slices (instead of whole 64 KiB
                        // bursts under the term lock) while a human waits on the
                        // echo. The write-slice close (`note_pty_write`) is deferred
                        // to AFTER the real egress below, so it separately isolates
                        // any blocking WriteFile inside this end-to-end
                        // input→present interval.
                        // Feed the cadence-comet IGNITION heat: this key/text egress is
                        // a keystroke, so heat the typing tracker. Fast sustained typing
                        // compounds heat toward ignition (a longer, hotter comet); a few
                        // keys / slow typing barely move it, and a pause decays it back to
                        // a gentle whisper. Clockless (injected `now`); the render tick
                        // reads the decayed intensity each frame.
                        // NAVIGATION/KILL chords earn NO heat — the same law the fire
                        // nav-hint below enforces for the glow engine: held arrows /
                        // Ctrl-A/E scrubbing and line kills are not writing, so they
                        // must not ignite the comet, spin the rainbow cursor, or
                        // charge the phaser emitter's wings.
                        if !navigation_key && !kill_key {
                            ws.typing_cadence.on_keystroke(input_now);
                        }
                        // Feed the EMBERFORGE quench: a Backspace key-hint douses
                        // the fire engine (heat + coal cool, the quench meter
                        // escalates) and classifies the paired echo move as a
                        // deletion — deleting never stokes the fire. Style-agnostic
                        // to arm (cheap scalar bumps); only the fire style reads it.
                        if typed_forward == Some(false) {
                            ws.cursor_glow.note_backspace(input_now);
                            // The trail engine shares the re-anchor hint: a
                            // Backspace at an Ink wrap boundary re-anchors the
                            // caret UP a row — same repaint choreography, same
                            // "lay no comet across cells the caret never swept".
                            // Armed ALWAYS (like the typed hint below): the
                            // engine-side alt/repaint-blink conjunct is the
                            // discriminator — plain vim (alt screen, never
                            // blinks inside DEC-2026) keeps its jump drama
                            // through blink-absence, while full-redraw agent
                            // TUIs (including Codex) re-anchor through their
                            // per-keystroke repaint blink. The behavioral blink
                            // is authoritative regardless of Kitty flags.
                            ws.cursor_trail.note_typed(input_now);
                        }
                        // Arm the TYPED-GLYPH hint on plain Character/Space
                        // echoes — plus bare SHIFT+Enter (agent composers can
                        // use Shift+Enter to INSERT a newline, a wrap-shaped move
                        // that must re-anchor, not meteor; in a plain shell
                        // its echo is a plain Enter move — dr==1 to col ~0,
                        // shape-wrap-adjacent — so collapsing it to typing
                        // reads right there too). A paired one-row move beyond
                        // the typed advance is a TUI repaint RE-ANCHOR (an
                        // Ink-style box rewraps per keystroke), never a jump.
                        // Plain Enter, Tab, nav keys, and modified chords never
                        // arm — their jumps keep the owner-mandated
                        // meteors/ZOOMs. Armed ALWAYS: keyboard negotiation
                        // does not classify repaint geometry. vim safety lives
                        // in the engines — their re-anchor conjunct requires a
                        // fresh REPAINT BLINK on the alt screen (the hide-inside-
                        // DEC-2026 bracket only per-keystroke-repaint TUIs
                        // emit), so vim's hinted one-row motions keep their
                        // drama through blink-ABSENCE, not arming-absence.
                        // Both cursor engines take the hint; bracketed paste is
                        // not a Key event, so paste landings keep their jumps.
                        // Alt-gated: agent-composer insert-newline lives on the alt
                        // screen; in a plain (main-screen) shell Shift+Enter IS Enter
                        // and keeps its meteor.
                        let shift_enter_insert = enter_like && !typed_enter && is_alt;
                        if typed_forward == Some(true) && (!enter_like || shift_enter_insert) {
                            ws.cursor_glow.note_typed(input_now);
                            ws.cursor_trail.note_typed(input_now);
                            // CLICK AT THE KEY, not at the echo (touch-to-glass
                            // audio): past ~20 ms a click stops feeling attached
                            // to the key, and echo time can lag by tens to
                            // hundreds of milliseconds under a flood or on a
                            // remote link. The engine spends the credit when the
                            // echo finally spawns, so the character still clicks
                            // exactly once.
                            //
                            // EXACTLY this arm: the same printable-glyph set that
                            // earns the typed hint (bare Character/Space, a
                            // committed IME run, composer Shift+Enter). Chords,
                            // nav keys, kills, Tab, and plain Enter never reach
                            // here, so a swallowed shortcut never clicks — and
                            // Backspace/Enter keep their OWN echo-time gestures
                            // (Backspace/Jump), which this seam does not touch.
                            //
                            // The redraw is what DELIVERS the cue: cues reach the
                            // synth only through the render tick's drain. That
                            // drain runs ahead of the present early-out, so an
                            // otherwise-unchanged frame still speaks and still
                            // presents nothing; and with light alive the animator
                            // is already requesting frames, so this coalesces
                            // into one that was coming anyway. Requested only
                            // when a cue was actually RECORDED — the engine
                            // returns false whenever it is dark — so a session
                            // with the aurora off never pays a frame for it.
                            if click_audible
                                && ws.cursor_glow.cue_keystroke(input_now)
                                && let Some(w) = ws.os_window.as_ref()
                            {
                                w.request_redraw();
                            }
                        } else if enter_like && typed_forward == Some(true) {
                            // A REAL Enter (deliberately NOT a typed hint — the
                            // Rainbow Return snap needs its move to classify as
                            // a jump): arm the RETURN license so the anti-stray
                            // momentum gate lets a bare Enter at an idle prompt
                            // keep its snap. A program's CUP never lands here.
                            ws.cursor_glow.note_return(input_now);
                        }
                        // Arm the KILL hint (erase poof): the paired content
                        // change — or, for a reflowing TUI, the keypress itself
                        // — puffs smoke; for fire it also escalates the quench.
                        // Backward kills additionally ride the nav-hint
                        // choreography (Ctrl-U's leap flies the meteor, no
                        // field-wide flare); stationary kills must not.
                        if kill_key {
                            ws.cursor_glow.note_kill(input_now, kill_moves);
                            // The companion's delete "oops" fires for span
                            // kills too: Ctrl-K/U/W, Alt-D, forward Delete,
                            // and word-backspaces erase text just as surely as
                            // Backspace, and the owner-loved tongue-out must
                            // not depend on WHICH delete spelling the hand
                            // used. Same gate shape as the rainbow kitty momentum feed
                            // below: the rainbow-kitty style keeps its momentum honest
                            // even while hidden; other styles pay the O(1)
                            // call only while a companion is actually live (a
                            // collection hello). `on_kill` never summons — the
                            // reaction is expression state on an existing
                            // flight, so the lifecycle contract is untouched.
                            if kitty_cursor_enabled || ws.cursor_cat.is_active() {
                                ws.cursor_cat.on_kill(input_now);
                            }
                        }
                        // DISARM dangling typed hints on keys with their OWN
                        // move semantics (plain Enter, nav, kills): a hint left
                        // by a no-move echo (password prompt, vim x/r) must not
                        // re-anchor the NEXT legit jump. RAINBOW RETURN: plain
                        // Enter intentionally stays a real row-change jump, so
                        // Rainbow kitty turns the submitted newline into its official
                        // short rainbow snap/ZOOM. The glow's clear_typed also
                        // drops a dangling backspace quench hint, so a no-move
                        // backspace's surviving pairing cannot "re-anchor" a
                        // following Ctrl-A/E and eat its meteor.
                        // Tab joins the disarm set: with the re-anchor now
                        // accepting dr == 0 (the box-growth wrap), a completion
                        // landing after Tab must not pair with the previous
                        // char's hint — completions keep their jump look.
                        if (typed_enter && !shift_enter_insert)
                            || navigation_key
                            || kill_key
                            || tab_key
                        {
                            ws.cursor_glow.clear_typed(input_now);
                            ws.cursor_trail.clear_typed();
                        }
                        // Arm the fire NAVIGATION hint so the paired cursor move
                        // (Ctrl-A/E, Home/End, arrows) ignites no fire — keeps
                        // line-start/end navigation instant, never a blaze.
                        if navigation_key {
                            ws.cursor_glow.note_navigation(input_now);
                            ws.cursor_trail.note_navigation(input_now);
                        }
                        // Feed the kitty-cursor metric — DELETES ONLY, at the key.
                        // A backspace drains the cat's momentum at the KEY instant,
                        // byte-for-byte alongside the glow's `note_backspace` beside
                        // it (same stamp ⇒ the two instances stay in lockstep), and
                        // fires the "oops" on a live cat.
                        //
                        // FORWARD momentum never builds HERE: a keystroke alone
                        // is not proof of typed text (a password prompt echoes
                        // nothing, vim vertical navigation echoes a non-forward
                        // move). The correlated forward build rides the glow's
                        // echo pulse in `tick_cursor_fx`
                        // ([`CursorGlow::take_momentum_pulse`]): a real printable key
                        // PAIRED with its forward/wrap/coalesced echo, the exact same
                        // event the ribbon spine builds from, so the cat and the
                        // ribbon can never diverge.
                        if typed_forward == Some(false)
                            && (kitty_cursor_enabled || ws.cursor_cat.is_active())
                        {
                            ws.cursor_cat.on_key(input_now, false);
                        }
                        if typed_enter {
                            ws.cursor_cat.on_enter(input_now);
                            // End the predictive-echo confirmation epoch at the submit
                            // boundary so the NEXT line must re-confirm before any guess
                            // shows. The epoch is otherwise keyed to the physical cursor
                            // row, which is reused across logical lines on a terminal
                            // scrolled to the bottom — without this, a non-echoing
                            // password prompt on that same row inherits the just-typed
                            // command's confirmation and would flash the secret.
                            let was_displaying = ws.predictor.is_displaying(input_now);
                            ws.predictor.note_line_submit();
                            // `note_line_submit` ends the epoch WITHOUT flushing: the
                            // submitted line's in-flight guesses keep painting
                            // (flushing them makes the tail of the command vanish at
                            // the exact instant of commit, one RTT before the real
                            // echo catches up, which reads as the terminal stalling on
                            // Enter). Visibility can still CHANGE here though: the new
                            // epoch's guesses go dark while grandfathered ones keep
                            // their pixels, so the redraw request stays load-bearing —
                            // it delivers a transition rather than a wholesale erase.
                            if prediction_visibility_requires_redraw(was_displaying, false)
                                && let Some(w) = ws.os_window.as_ref()
                            {
                                w.request_redraw();
                            }
                        }
                        // PHOSPHOR: the same keystroke heat keeps the rain
                        // weather alive at CALM — typing alone never reaches
                        // WORKING (design §5); a `None` engine (rain off) costs
                        // one Option check on the hottest typing path.
                        if let Some(rain) = ws.matrix_rain.as_mut() {
                            rain.note_keystroke();
                            // Enter is a turn boundary, not another editor key:
                            // a fast shell/agent response may share the newline's
                            // first present and must not be discounted as echo.
                            if typed_enter {
                                rain.note_signal(
                                    crate::matrix_rain::RainSignal::TurnStart as u32,
                                    4,
                                );
                            }
                            // Reading gate (design §6): plain PgUp/PgDn while a
                            // fullscreen app owns the screen is the KEYBOARD's
                            // "scroll to read" gesture — the wheel funnel already
                            // stamps; paging a transcript must quiet the rain,
                            // not read as streaming (the page-echo would
                            // otherwise inflate the activity signal).
                            if is_alt
                                && matches!(
                                    &ev,
                                    InputEvent::Key {
                                        key: aterm_types::keyboard::Key::Named(
                                            aterm_types::keyboard::NamedKey::PageUp
                                                | aterm_types::keyboard::NamedKey::PageDown
                                        ),
                                        ..
                                    }
                                )
                            {
                                rain.note_alt_scroll();
                            }
                            // A SLEEPING engine has no armed timer to consume
                            // this note, and a no-echo TUI may swallow the key
                            // without any repaint — one redraw lets the next
                            // emit apply it, resume the CALM drizzle, and
                            // re-arm: a pending note must be able to wake a
                            // drained engine.
                            if !rain.is_active()
                                && rain.notes_can_wake()
                                && let Some(w) = ws.os_window.as_ref()
                            {
                                w.request_redraw();
                            }
                        }
                    }
                    if let Some(((ch, is_backspace), (cur, (cols, rows), no_echo))) =
                        act.zip(sample)
                    {
                        // Split panes predict too: the composed render path
                        // reconciles/paints the FOCUSED pane's guesses (see
                        // `redraw_compose`), and `sync_window` resets the
                        // predictor whenever the focused pane/tab changes, so a
                        // guess can never outlive its coordinate space.
                        if let Some(ws) = self.windows.get_mut(&wid) {
                            let now = input_now;
                            // Capture BEFORE every possible flush (`reset`,
                            // `set_mode`, invalid/wrapping `predict_char`, or
                            // Backspace). A previously painted ghost remains in the
                            // last presented frame until one explicit erase redraw.
                            let was_displaying = ws.predictor.is_displaying(now);
                            if no_echo {
                                // STALE-EPOCH hardening: a
                                // no-echo press (alt screen / app-owned Kitty mode)
                                // registers nothing — but a confirmed_epoch
                                // from BEFORE entering the app could otherwise
                                // survive, and on returning to the same row the
                                // first keystroke could display off that stale
                                // confirmation. reset() clears the epoch (and
                                // is a cheap no-op when already idle).
                                ws.predictor.reset();
                            } else {
                                ws.predictor.set_mode(pmode);
                                let _changed = match ch {
                                    Some(c) => ws.predictor.predict_char_in_grid(
                                        c,
                                        (cur.row, cur.col),
                                        (cols, rows),
                                        now,
                                    ),
                                    None if is_backspace => ws.predictor.predict_backspace(now),
                                    None => false,
                                };
                            }
                            // Repaint only when a guess SHOWS or WAS showing. The
                            // latter is load-bearing: several conservative predictor
                            // operations flush and return `false`, and app-owned mode
                            // resets without arming anything. Conditioning this erase
                            // on "a new guess was armed" strands the old ghost on glass.
                            // Fast Adaptive tracking remains zero-redraw because both
                            // sides of this visibility test are false.
                            if prediction_visibility_requires_redraw(
                                was_displaying,
                                ws.predictor.is_displaying(now),
                            ) && let Some(w) = ws.os_window.as_ref()
                            {
                                w.request_redraw();
                            }
                        }
                    }
                }
                // If a paste is currently draining for THIS session, submit this
                // key onto the SAME per-session FIFO so it cannot overtake the
                // pasted bytes (the submission-order fix). Otherwise the common
                // inline fast path — one relaxed atomic decides.
                //
                // `ev` is CLONED into the FIFO — the paste-drain path only, never the
                // common inline one — so the classified cosmetic feeds below can still
                // read it after the dispatch (see their note on why they now run AFTER
                // the write).
                let (egress, wrote_inline) = paste_order::ordered_or_inline(
                    &term,
                    &sink,
                    &ev,
                    input::EgressMode::Interactive,
                );
                let outcome = egress_to_outcome(egress);
                // The PTY write has now RETURNED — close the key→write latency slice
                // here (a press-path key only), so it isolates a blocking WriteFile
                // within the end-to-end input_present interval. Paired with
                // `note_input()` above.
                //
                // ONLY when the write actually ran INLINE. An enqueued key wrote
                // nothing here: `paste_order::enqueue` only pushed a Job onto the FIFO
                // and the real write happens later on the writer thread. Recording the
                // slice at enqueue time would CONSUME the arrival stamp
                // (`note_pty_write` does `LAT_KEY_NS.swap(0)`) and file a
                // few-microsecond sample for a write that had not happened, so the
                // histogram would report its BEST numbers in exactly the case the user
                // feels as its worst (a key typed while a paste drains) and the real
                // write would never be measured. Disarm instead: this is precisely
                // the documented "dispatch ended WITHOUT a PTY write" case, and it
                // also stops the stamp being inherited by an unrelated later `send`.
                if !is_release {
                    if wrote_inline {
                        crate::metrics::note_pty_write();
                    } else {
                        crate::metrics::clear_key_arrival();
                    }
                }
                // COSMETIC TYPING FEEDS — deliberately AFTER the egress above.
                //
                // None of this steers a byte: the classified press feeds the tone
                // tracker, the sing-along run, and the typed-"kitty" detector, all of
                // which are consumed by the render/sound tick, never by the encoder.
                // Running it BEFORE the write would put a full neural classifier on the
                // key path — `ToneTracker::note_char` can evaluate the mood model over its
                // 160-char window (thousands of flops plus an embedding-table walk)
                // synchronously between the keypress and the `write` syscall. Its
                // cadence is worst where it hurts most: every few keystrokes for a fast
                // typist, and on EVERY keystroke for a deliberate one (>500 ms gaps),
                // which is exactly the typing a human watches land. The predictor
                // arming and every `request_redraw` above stay where they are — those
                // must precede the write to buy perceived latency.
                if let Some(input_now) = input_now {
                    // TYPED-"kitty" CAMEO (the terminal twin of the Settings
                    // §L.4 cameo): feed this classified PRESS to the
                    // per-window detector. PRINTED characters — bare
                    // Character/Space plus committed IME Text, the same set
                    // `typed_forward` calls forward — feed the rolling window;
                    // a plain Backspace pops one letter (typo tolerance); and
                    // every key that edits or moves beyond one glyph (Enter,
                    // Tab, Escape, Delete, nav keys, any modified chord, raw
                    // controller byte sequences) CLEARS it — a word assembled
                    // across an editing boundary was never typed as a word.
                    // TYPED INPUT ONLY, never screen content: PTY output and
                    // `cat`ing a file of "kitty" never reach this press path,
                    // and pastes dispatch through a different arm (`Text` is
                    // built only by IME commits, never by paste). Source-
                    // agnostic like every effect on this path — a controller
                    // typing "kitty" summons exactly like a human (the
                    // indistinguishability invariant forbids a `src` branch).
                    // O(1) per key: a bounded 8-slot window + 5-char suffix
                    // compare, so the hottest typing path pays scalar work.
                    let summoned = {
                        let cosmetic_press = CosmeticFeedPress {
                            brk,
                            backspace,
                            typed,
                            ime,
                        };
                        // TONE-OF-TYPING feed — the SAME classified press,
                        // the same typed-provenance law (PTY output, `cat`,
                        // and pastes can never steer the melody; a break
                        // boundary clears the window rather than guessing).
                        // WINDOW MAINTENANCE RUNS REGARDLESS of activation:
                        // the O(1) note_char/note_break/note_backspace
                        // bookkeeping must keep the window coherent even while
                        // inference is off (trail sounds disabled, volume 0,
                        // worker down), or Enter/breaks would be dropped and a
                        // later re-enable would classify a window spliced
                        // across the editing that happened while it was off.
                        // Only the expensive classifier is gated: the
                        // `tone_infer_active` flag rides into `note_char`,
                        // which loads the weights and runs the model solely
                        // when it is set (see `ToneTracker::note_char`).
                        let tone_active = self.tone_infer_active();
                        if let Some(ws) = self.windows.get_mut(&wid) {
                            feed_classified_press(
                                &mut ws.tone_tracker,
                                cosmetic_press,
                                |tone| tone.note_break(),
                                |tone| tone.note_backspace(session),
                                |tone, c| tone.note_char(input_now, session, c, tone_active),
                            );
                        }
                        // SING-ALONG feed (`aterm_effects::kitty_sing`)
                        // — the SAME classified press, the same typed-
                        // provenance law as the cameo/tone feeds beside it:
                        // PTY output, `cat`, and pastes can never arm the
                        // celebration, and the detector is Source-agnostic
                        // like everything on this path. PRINTED characters
                        // extend the same-key run (16 at repeat cadence arms
                        // SING-ALONG); Backspace RELEASES and never arms; every
                        // break key releases too — each release a graceful
                        // wind-down, never a hard cut. O(1) scalar state per
                        // key. Style gating (the rainbow kitty trail) is consumption-
                        // side policy in `redraw_window`.
                        if let Some(ws) = self.windows.get_mut(&wid) {
                            feed_classified_press(
                                &mut ws.kitty_sing,
                                cosmetic_press,
                                |sing| sing.note_break(input_now),
                                |sing| sing.note_backspace(input_now),
                                |sing, c| sing.note_char(input_now, session, c),
                            );
                        }
                        // The detector needs the compiled lexicon (multilingual
                        // feline + profanity) while `windows` is borrowed mutably;
                        // `Resolved::lexicon` is an `Arc`, so one refcount bump
                        // splits the borrow without copying the vocabulary.
                        let lexicon = self.sparkle.as_ref().map(|rs| rs.lexicon.clone());
                        let opts = self
                            .sparkle
                            .as_ref()
                            .map(|rs| rs.cfg.scan_opts())
                            .unwrap_or_default();
                        match (self.windows.get_mut(&wid), lexicon) {
                            (Some(ws), lexicon) => {
                                // `merge` fold across one IME commit: a granted
                                // record outranks a cooldown-only cameo, and
                                // both family reactions are kept
                                // (`TypedHit::default()` is `merge`'s
                                // identity). Sparkle words off ⇒ no vocabulary,
                                // no reactions — break/backspace still maintain
                                // the detector window.
                                let mut fired = crate::kitty_summon::TypedHit::default();
                                feed_classified_press(
                                    &mut ws.kitty_summon,
                                    cosmetic_press,
                                    |kitty| kitty.note_break(),
                                    |kitty| kitty.note_backspace(session),
                                    |kitty, c| {
                                        if let Some(lx) = lexicon.as_ref() {
                                            fired = fired.merge(
                                                kitty.note_char(input_now, session, c, lx, &opts),
                                            );
                                        }
                                    },
                                );
                                fired
                            }
                            (None, _) => crate::kitty_summon::TypedHit::default(),
                        }
                    };
                    // The cameo is owed on EVERY completion; only the ledger row
                    // is rate-limited (see `kitty_summon`'s two-tier note).
                    if summoned.summon.shows_cameo() {
                        self.summon_typed_kitty(wid, session, input_now, summoned.summon.records());
                    }
                    // COMPANION REACTIONS (multilingual, via the lexicon scan
                    // above). Expression only — the session's kitty never changes
                    // identity for a typed word.
                    if summoned.feline || summoned.profanity {
                        self.react_typed_word(wid, input_now, summoned);
                    }
                }
                outcome
            }
            // --- Mouse button: tracking-ON report else local gesture (a/b/d/i) -
            ev @ InputEvent::MouseButton { .. } => self.input_mouse_button(wid, &ev, &term, &sink),
            // --- Mouse motion: tracking-ON report else drag the selection (c) ---
            ev @ InputEvent::MouseMove { .. } => {
                let (row, col, side) = if let InputEvent::MouseMove { row, col, side, .. } = ev {
                    (row, col, side)
                } else {
                    unreachable!()
                };
                if let Some(ws) = self.windows.get_mut(&wid) {
                    // `last_mouse_cell` is the PANE-LOCAL cell already published by
                    // `on_cursor_moved` (window cell minus the focused pane origin); do
                    // NOT clobber it with this event's coordinates — a follow-up press
                    // or wheel (winit delivers no position on those) reads it and must
                    // see the pane-local cell, not the window cell.
                    ws.last_mouse_side = side;
                }
                // A held-button drag with tracking OFF grows the local selection
                // (regardless of mode — finishing a drag the app started tracking
                // mid-gesture still settles locally).
                if self.windows.get(&wid).is_some_and(|ws| ws.selecting) {
                    self.drag_selection(wid, row, col);
                    return InputOutcome::Ok;
                }
                egress_to_outcome(input::seam_egress(
                    &term,
                    &sink,
                    &ev,
                    input::EgressMode::Interactive,
                ))
            }
            // --- Wheel: N reports/line when tracking ON else scroll viewport (e) -
            ev @ InputEvent::Wheel { .. } => self.input_wheel(wid, &ev, &term, &sink),
            // --- Explicit, tracking-agnostic scrollback nav (A.6) --------------
            InputEvent::ScrollView(intent) => self.input_scroll_view(wid, intent, &term),
            ev @ InputEvent::Paste(_) => self.input_paste(wid, ev, &term, &sink),
            // --- Geometry (range-reject reportable) ----------------------------
            InputEvent::Resize {
                rows,
                cols,
                echo_to_window,
            } => self.input_resize(wid, rows, cols, echo_to_window),
            // A WINDOW-pixel resize: nothing is applied here, the window is simply
            // asked for the size and the grid follows from the platform's `Resized`
            // — the drag path, reachable from the control surface.
            InputEvent::ResizeWindowPx { width, height } => {
                self.input_resize_window_px(wid, width, height)
            }
            // --- Focus reporting (kills j) -------------------------------------
            ev @ InputEvent::Focus(_) => {
                // SOLE focus-report egress (in `seam_egress`): identical bytes to
                // the engine's `encode_focus_state` (ESC[I / ESC[O), gated on DEC
                // 1004. The GUI-visual blink/cursor-override side-effect stays in
                // `on_focus`.
                input::seam_egress(&term, &sink, &ev, input::EgressMode::Interactive);
                InputOutcome::Ok
            }
        }
    }

    /// `InputEvent::MouseButton` arm of [`App::input`]: a tracking-ON press reports
    /// inside `seam_egress`; tracking-OFF runs the LOCAL left-button selection
    /// gesture for BOTH sources. `click_count` (1/2/3), `side`, and `block` are
    /// carried data — the Human handler ran the MULTI_CLICK_MS streak FSM; a
    /// Controller passes an authoritative count without touching `last_press`
    /// (A.2.2). `block` is the selection-TYPE intent carried ON the event so the
    /// seam never reads `self.mods` (a held human Alt can't leak into a controller
    /// press, and a controller can drive block-select). Only the left button
    /// selects. `ev` must be `InputEvent::MouseButton { .. }`.
    fn input_mouse_button(
        &mut self,
        wid: WindowId,
        ev: &InputEvent,
        term: &Arc<Mutex<Terminal>>,
        sink: &Arc<SinkWriter>,
    ) -> InputOutcome {
        // Carry the gesture-relevant fields out before `seam_egress` (which borrows
        // `ev`) for the tracking-OFF local fallback.
        let (button, pressed, row, col, click_count, side, block, suppress_copy_on_select) =
            if let &InputEvent::MouseButton {
                button,
                pressed,
                row,
                col,
                click_count,
                side,
                block,
                suppress_copy_on_select,
                ..
            } = ev
            {
                (
                    button,
                    pressed,
                    row,
                    col,
                    click_count,
                    side,
                    block,
                    suppress_copy_on_select,
                )
            } else {
                unreachable!()
            };
        let egress = input::seam_egress(term, sink, ev, input::EgressMode::Interactive);
        if let input::Egress::TrackingOff { .. } = egress
            && button == aterm_types::mouse::MouseButton::Left
        {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.last_mouse_cell = (row, col);
                ws.last_mouse_side = side;
            }
            if pressed {
                self.seam_left_press(wid, row, col, click_count, block);
            } else if self.windows.get(&wid).is_some_and(|ws| ws.selecting) {
                // The release carries the control-authority copy-on-select policy: a
                // scoped-edge-injected gesture (`suppress_copy_on_select == true`)
                // completes the selection but does NOT auto-copy to the clipboard
                // (the exfil fence). Human / Owner gestures carry `false`.
                self.finish_selection(wid, suppress_copy_on_select);
            }
        }
        egress_to_outcome(egress)
    }

    /// `InputEvent::Wheel` arm of [`App::input`]: tracking ON emitted the per-line
    /// reports inside `seam_egress`; tracking OFF scrolls the LOCAL viewport by the
    /// wheel's lines (>0, guaranteed by the handler/verb) — through the M1 smooth
    /// glide when the motion policy permits — and repaints. Positive
    /// `display_offset` = older content, so wheel up -> history. `ev` must be
    /// `InputEvent::Wheel { .. }`.
    fn input_wheel(
        &mut self,
        wid: WindowId,
        ev: &InputEvent,
        term: &Arc<Mutex<Terminal>>,
        sink: &Arc<SinkWriter>,
    ) -> InputOutcome {
        // PHOSPHOR alt-screen scroll-quiet gate (design §6), stamped HOST-side
        // at the input funnel BEFORE egress: an alt-screen wheel becomes PTY
        // bytes (DEC-1007 arrows / mouse reports) whose echo advances
        // `content_seq` — the exact EMA inversion the gate prevents. Scrolling
        // a fullscreen transcript to READ it must never summon a denser
        // downpour. The engine check comes first so a rain-off window never
        // takes the extra lock.
        if let Some(rain) = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.matrix_rain.as_mut())
            && term_lock(term).is_alternate_screen()
        {
            rain.note_alt_scroll();
        }
        let egress = input::seam_egress(term, sink, ev, input::EgressMode::Interactive);
        if let input::Egress::TrackingOff {
            wheel_lines,
            wheel_up,
        } = egress
        {
            let delta = if wheel_up { wheel_lines } else { -wheel_lines };
            self.scroll_wheel_animated(wid, term, delta);
        }
        egress_to_outcome(egress)
    }

    /// Settle every retained smooth-scroll artifact for one window at its
    /// intended whole-row target. A motion-policy edge must use this path rather
    /// than merely dropping [`crate::ScrollGlideState`]: cancellation alone
    /// abandons the pinned terminal at an intermediate row, while Reduced motion
    /// promises an immediate SNAP with no residual and no future deadline.
    ///
    /// The glide owns the terminal it began on, which may no longer be the
    /// window's front pane. Take that state before locking the engine, land that
    /// exact terminal, clear the display-only overscroll/residual, and repaint the
    /// owning window. Returns whether any retained scroll state changed.
    pub(crate) fn settle_scroll_motion_at_target(
        &mut self,
        wid: WindowId,
        now: std::time::Instant,
    ) -> bool {
        let Some((glide, redraw, changed)) = self.windows.get_mut(&wid).map(|ws| {
            let glide = ws.scroll_glide.take();
            let overscroll = ws.overscroll.take();
            let changed = glide.is_some() || overscroll.is_some() || ws.scroll_frac_px != 0;
            if changed {
                ws.scroll_frac_px = 0;
                ws.scroll_pill.touch(now);
            }
            (glide, ws.os_window.clone(), changed)
        }) else {
            return false;
        };
        if !changed {
            return false;
        }

        if let Some(st) = glide {
            let (target_row, residual) =
                crate::scroll_motion::decompose(st.glide.target_px(), st.cell_h);
            debug_assert_eq!(
                residual, 0,
                "a wheel glide target must be an exact whole-row position"
            );
            let mut term = term_lock(&st.term);
            // Scrollback can shrink while a glide is retained. Honor its intended
            // row when still reachable and otherwise land at the current engine
            // boundary; either outcome is an exact whole-row rest state.
            let max_row = i64::try_from(term.grid().scrollback_lines()).unwrap_or(i64::MAX);
            let target_row = target_row.clamp(0, max_row);
            loop {
                let current = i64::try_from(term.grid().display_offset()).unwrap_or(i64::MAX);
                let delta = target_row - current;
                if delta == 0 {
                    break;
                }
                let step = delta.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
                term.scroll_display(step);
                let landed = i64::try_from(term.grid().display_offset()).unwrap_or(i64::MAX);
                if landed == current {
                    // The terminal's live clamp is authoritative if its history
                    // changed between the bound read above and the scroll.
                    break;
                }
            }
        }
        if let Some(window) = redraw {
            window.request_redraw();
        }
        true
    }

    /// Apply the shared settle transition only when this window's currently
    /// resolved SmoothScroll policy is Reduced.
    pub(crate) fn settle_scroll_motion_if_reduced(
        &mut self,
        wid: WindowId,
        now: std::time::Instant,
    ) -> bool {
        let focused = self.motion_focus(wid, self.windows.get(&wid).is_some_and(|ws| ws.focused));
        if self
            .motion_policy(focused)
            .animate(crate::motion::MotionEffect::SmoothScroll)
        {
            return false;
        }
        self.settle_scroll_motion_at_target(wid, now)
    }

    /// Reconcile every retained window after a fact that can change the resolved
    /// motion policy (config, OS accessibility, focus, or adaptive shedding).
    /// The snapshot avoids borrowing `windows` across the terminal-locking settle.
    pub(crate) fn settle_reduced_scroll_motion(&mut self, now: std::time::Instant) {
        let retained: Vec<WindowId> = self
            .windows
            .iter()
            .filter(|(_, ws)| {
                ws.scroll_glide.is_some() || ws.overscroll.is_some() || ws.scroll_frac_px != 0
            })
            .map(|(wid, _)| *wid)
            .collect();
        for wid in retained {
            self.settle_scroll_motion_if_reduced(wid, now);
        }
    }

    /// Install one live OS accessibility sample and reconcile every affected
    /// glide in the same event-loop turn. Returns whether the sampled fact
    /// changed (the caller also uses that edge to decide whether to repaint
    /// unrelated motion consumers).
    pub(crate) fn apply_system_reduce_motion(
        &mut self,
        reduced: bool,
        now: std::time::Instant,
    ) -> bool {
        if self.system_reduce_motion == reduced {
            return false;
        }
        self.system_reduce_motion = reduced;
        self.settle_reduced_scroll_motion(now);
        true
    }

    /// M1 smooth scroll: move the LOCAL viewport by `delta_rows` (positive = into
    /// history) through the ~180 ms ease-out GLIDE when W11's motion policy
    /// permits, and INSTANTLY otherwise (config `motion=reduced`, the OS Reduce
    /// Motion flag, or an unfocused window — all snap, per the M1 accessibility
    /// clause). Either way the scroll pill wakes.
    ///
    /// SOURCE-BLIND on purpose: this sits BELOW the seam's `TrackingOff` fallback,
    /// reached identically by a Human wheel notch and a controller `mouse` wheel
    /// verb — the glide's gate is window/system state (the motion policy), never
    /// the event `Source`, so the indistinguishability invariant is untouched.
    /// (The controller's reply-bearing `scroll` verb keeps its INSTANT
    /// `ScrollView` path — its documented contract reports the applied offset.)
    ///
    /// A glide is bound to the exact engine the wheel targeted ([`crate::ScrollGlideState`]
    /// pins the `Arc`), so chained notches retarget the SAME ease while a
    /// focus/pane change simply starts a fresh glide. The eased position lives in
    /// absolute viewport px; each tick decomposes it via
    /// [`scroll_motion::decompose`] (the proven law) into whole rows for
    /// `scroll_display` — the fractional remainder is banked until the render
    /// path grows its sub-row translate.
    pub(crate) fn scroll_wheel_animated(
        &mut self,
        wid: WindowId,
        term: &Arc<Mutex<Terminal>>,
        delta_rows: i32,
    ) {
        let now = std::time::Instant::now();
        // W12: the glide's pixel domain belongs to the window that received the
        // wheel event, not whichever different-DPI window most recently activated
        // the shared renderer.  The render-side residual consumes this same
        // per-window cell height (`set_scroll_band`), so both halves must share
        // the exact authority or a background-window gesture lands between rows.
        let cell_h = self.win_cell_size(wid).1.max(1) as i64;
        let focused = self.motion_focus(wid, self.windows.get(&wid).is_some_and(|ws| ws.focused));
        let animate = self
            .motion_policy(focused)
            .animate(crate::motion::MotionEffect::SmoothScroll);
        if !animate {
            if !self.windows.contains_key(&wid) {
                return;
            }
            // First finish a retained Full-policy gesture on its own pinned
            // terminal, then apply this new Reduced-policy notch instantly. The
            // shared settle is also used by config/OS/focus edges and defensive
            // scheduler paths, so none can regress to cancel-without-landing.
            self.settle_scroll_motion_at_target(wid, now);
            term_lock(term).scroll_display(delta_rows);
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.scroll_pill.touch(now);
                if let Some(w) = ws.os_window.as_ref() {
                    w.request_redraw();
                }
            }
            return;
        }
        // The engine's current viewport + clamp bound, under ONE short lock.
        let (cur_rows, max_rows) = {
            let t = term_lock(term);
            (
                i64::try_from(t.grid().display_offset()).unwrap_or(i64::MAX),
                i64::try_from(t.grid().scrollback_lines()).unwrap_or(i64::MAX),
            )
        };
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        // Chain onto an in-flight glide of the SAME engine at the SAME geometry;
        // anything else (fresh gesture, pane switch, font zoom) starts anew from
        // the engine's real position.
        let same = ws
            .scroll_glide
            .as_ref()
            .is_some_and(|st| Arc::ptr_eq(&st.term, term) && st.cell_h == cell_h);
        let base_px = if same {
            ws.scroll_glide
                .as_ref()
                .map_or(0, |st| st.glide.target_px())
        } else {
            cur_rows * cell_h
        };
        let want_px = base_px + i64::from(delta_rows) * cell_h;
        let target_px = want_px.clamp(0, max_rows * cell_h);
        if same {
            // A chained notch that still has room to move: retarget the ease and
            // clear any stale bounce (we are no longer parked at an edge).
            ws.overscroll = None;
            if let Some(st) = ws.scroll_glide.as_mut() {
                st.glide.retarget(target_px, now);
            }
        } else if target_px == cur_rows * cell_h {
            // Parked at a history end (the clamp ate the whole notch): nothing to
            // ease — never arm a zero-length glide (no deadline, 0% idle) — but
            // RELEASE an elastic-overscroll bounce so the rubber-band renders. The
            // clamped-away excess `want_px - target_px` (signed: + past the top, −
            // past the live bottom) is resisted 0.3× and negated so a top overscroll
            // bounces the band DOWN (negative frac) and a bottom overscroll bounces
            // it UP (positive frac). Clamped to one cell (the sub-row translate's
            // domain), the bounce decays to rest via the proven spring and
            // self-disarms (`tick_overscroll`). `set_scroll_band` still gates the
            // presented frac on the SmoothScroll policy.
            ws.scroll_glide = None;
            let excess_px = want_px - target_px;
            let impulse = -(crate::scroll_motion::overscroll_resist(excess_px) as f64);
            let max_px = f64::from((cell_h as i32 - 1).max(0));
            if impulse != 0.0 && max_px > 0.0 {
                match ws.overscroll.as_mut() {
                    Some(sp) => sp.add_impulse(impulse, max_px, now),
                    None => {
                        ws.overscroll = Some(crate::scroll_motion::OverscrollSpring::new(
                            impulse.clamp(-max_px, max_px),
                            now,
                        ));
                    }
                }
                // Present the bounce PEAK on this very frame (the tick advances the
                // decay on subsequent wakes) so the rubber-band renders immediately.
                if let Some(sp) = ws.overscroll.as_ref() {
                    ws.scroll_frac_px = sp.sample(now).0.round() as i32;
                }
            }
        } else {
            // A fresh ease with room to move: cancel any bounce and arm the glide.
            ws.overscroll = None;
            ws.scroll_frac_px = 0;
            ws.scroll_glide = Some(crate::ScrollGlideState {
                glide: crate::scroll_motion::Glide::new(cur_rows * cell_h, target_px, now),
                term: term.clone(),
                cell_h,
            });
        }
        ws.scroll_pill.touch(now);
        if let Some(w) = ws.os_window.as_ref() {
            w.request_redraw();
        }
    }

    /// One deadline wake of an in-flight M1 glide: sample the ease at `now`,
    /// decompose the eased px into whole rows (the proven [`scroll_motion::decompose`]
    /// law), apply the ROW DELTA to the glide's own pinned engine, and drop the
    /// state once the sample lands on the target — the self-disarm that returns
    /// the loop to pure `Wait`. Called from `new_events` after the borrow loop
    /// (it takes the engine lock), mirroring the selection-autoscroll ticks.
    pub(crate) fn tick_scroll_glide(&mut self, wid: WindowId, now: std::time::Instant) {
        // The M1b sub-row residual is PRESENTED only under a Full motion policy for
        // SmoothScroll; a mid-glide flip to Reduced (e.g. the window lost focus)
        // must snap whole-row, so the pairing below drops to the floor offset.
        let focused = self.motion_focus(wid, self.windows.get(&wid).is_some_and(|ws| ws.focused));
        let animate = self
            .motion_policy(focused)
            .animate(crate::motion::MotionEffect::SmoothScroll);
        if !animate {
            // Defensive convergence: even if a policy source changed without its
            // normal edge reducer running, the first pending tick lands and drops
            // the glide immediately instead of sampling/re-arming its old ease.
            self.settle_scroll_motion_at_target(wid, now);
            return;
        }
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let Some((term, cell_h, pos_px, done)) = ws.scroll_glide.as_ref().map(|st| {
            let (pos, done) = st.glide.sample(now);
            (st.term.clone(), st.cell_h, pos, done)
        }) else {
            return;
        };
        // M1b: split the eased absolute position into a whole-row engine OFFSET plus
        // the sub-row residual to shift the grid band UP by at present time. The
        // present translate reveals the row scrolling in from BELOW at the exposed
        // bottom strip, so the shift-up pairing is the CEIL offset with
        // `frac = offset*cell_h - pos_px` (NOT the floor residual `decompose` banks):
        // the engine sits one row deeper into history and the up-shift pulls it back
        // by `frac`, so motion is continuous and lands cleanly (`frac == 0`) on every
        // row boundary. `frac ∈ [0, cell_h)` by the
        // Euclidean split (the proven `scroll_px_decomposition_law`).
        let (row_floor, s) = crate::scroll_motion::decompose(pos_px, cell_h);
        let (offset, frac) = if s == 0 {
            (row_floor, 0)
        } else {
            (row_floor + 1, cell_h - s)
        };
        {
            let mut t = term_lock(&term);
            let cur = i64::try_from(t.grid().display_offset()).unwrap_or(i64::MAX);
            let delta = offset - cur;
            if delta != 0 {
                t.scroll_display(delta.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32);
            }
        }
        ws.scroll_frac_px = i32::try_from(frac).unwrap_or(0);
        if done {
            ws.scroll_glide = None;
            // A completed glide lands on a whole row (the target is a `cell_h`
            // multiple), so the residual is already 0 — make it explicit so no stale
            // frac survives the glide's disarm.
            ws.scroll_frac_px = 0;
        }
        // Keep the pill opaque for the whole glide (the fade hold restarts).
        ws.scroll_pill.touch(now);
        if let Some(w) = ws.os_window.as_ref() {
            w.request_redraw();
        }
    }

    /// One deadline wake of an in-flight elastic-overscroll BOUNCE: sample the
    /// spring at `now`, PRESENT its signed sub-cell displacement as `scroll_frac_px`
    /// (the bidirectional grid-band translate renders the rubber-band), and DROP the
    /// state once it settles — the self-disarm that returns the loop to pure `Wait`.
    /// A mid-bounce flip to Reduced motion (e.g. the window lost focus) snaps to rest
    /// (frac 0, no residual). The bounce never moves the ENGINE (it is parked at a
    /// history end — display-only), unlike the glide tick. Called from `new_events`
    /// after the borrow loop, mirroring [`Self::tick_scroll_glide`].
    pub(crate) fn tick_overscroll(&mut self, wid: WindowId, now: std::time::Instant) {
        let focused = self.motion_focus(wid, self.windows.get(&wid).is_some_and(|ws| ws.focused));
        let animate = self
            .motion_policy(focused)
            .animate(crate::motion::MotionEffect::SmoothScroll);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let Some((disp, done)) = ws.overscroll.as_ref().map(|sp| sp.sample(now)) else {
            return;
        };
        if !animate {
            // Reduced motion snaps the bounce to rest — whole-row, no residual.
            ws.overscroll = None;
            ws.scroll_frac_px = 0;
            if let Some(w) = ws.os_window.as_ref() {
                w.request_redraw();
            }
            return;
        }
        // Sub-cell displacement, rounded to the nearest device px (the translate
        // consumes integer px). `set_scroll_band` clamps it into `(-cell_h, cell_h)`.
        ws.scroll_frac_px = disp.round() as i32;
        if done {
            // Settled: drop the spring (disarm) and rest whole-row.
            ws.overscroll = None;
            ws.scroll_frac_px = 0;
        }
        // Keep the pill opaque while the bounce shows (the fade hold restarts).
        ws.scroll_pill.touch(now);
        if let Some(w) = ws.os_window.as_ref() {
            w.request_redraw();
        }
    }

    /// `InputEvent::ScrollView` arm of [`App::input`] (A.6): pure history nav — even
    /// when the app is mouse-tracking it touches only the LOCAL viewport (never emits
    /// wheel bytes), so a read-only edge can't drive a tracking app through it. The
    /// SEAM is the sole `scroll_display`/`scroll_to_*` caller.
    fn input_scroll_view(
        &mut self,
        wid: WindowId,
        intent: ScrollIntent,
        term: &Arc<Mutex<Terminal>>,
    ) -> InputOutcome {
        // PHOSPHOR alt-screen scroll-quiet gate (design §6): PgUp/PgDn (and
        // the keybinding/controller scroll verbs) are reading intent exactly
        // like the wheel — stamp the same host-side quiet deadline when the
        // pane is on the alt screen (where local viewport scroll is inert but
        // the INTENT still means "human reading"). See `input_wheel`.
        if let Some(rain) = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.matrix_rain.as_mut())
            && term_lock(term).is_alternate_screen()
        {
            rain.note_alt_scroll();
        }
        {
            let mut term = term_lock(term);
            let page = i32::from(term.rows()).max(1);
            match intent {
                ScrollIntent::Up => term.scroll_display(page),
                ScrollIntent::Down => term.scroll_display(-page),
                ScrollIntent::By(n) => term.scroll_display(n),
                ScrollIntent::Top => term.scroll_to_top(),
                ScrollIntent::Bottom => term.scroll_to_bottom(),
                // Jump-to-prompt: lift the nearest OSC-133 command mark above
                // (Prev) or below (Next) the current top visible row to the top.
                // The target is resolved under the immutable `command_marks()`
                // borrow and copied out BEFORE the `&mut` scroll, so the borrows
                // never overlap. No mark in the requested direction → no scroll
                // (mirrors the wheel hitting an edge); a bare shell with no
                // integration marks is inertly a no-op.
                ScrollIntent::PrevPrompt | ScrollIntent::NextPrompt => {
                    if let Some(row) = crate::input::jump_prompt_target(
                        &term,
                        matches!(intent, ScrollIntent::PrevPrompt),
                    ) {
                        term.scroll_to_absolute_row(row);
                    }
                }
            }
        }
        // M1: an instant ScrollView jump overrides any in-flight glide tail (a
        // Top/Bottom/page jump mid-glide must NOT be re-eased away), and wakes
        // the scroll pill so the new position shows. The verb path stays
        // instant on purpose — the controller `scroll` reply reports the
        // APPLIED offset (its documented contract), and keybindings share it.
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.scroll_glide = None;
            // M1b: an instant `ScrollView` jump is whole-row (the controller `scroll`
            // reply reports the APPLIED offset — no eased tail, no banked residual),
            // so clear any sub-row frac and any elastic bounce left by a prior gesture.
            ws.overscroll = None;
            ws.scroll_frac_px = 0;
            ws.scroll_pill.touch(std::time::Instant::now());
        }
        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            w.request_redraw();
        }
        InputOutcome::Ok
    }

    /// `InputEvent::Paste` arm of [`App::input`]: a paste, like typing, jumps the
    /// viewport back to live; the `format_paste` bytes come from `seam_egress`.
    ///
    /// Offload the PTY write OFF the winit UI thread. A large paste (up to
    /// MAX_PASTE_BYTES = 16 MiB) into a foreground program that is not currently
    /// reading stdin parks its writer until the consumer drains (the tty input
    /// buffer is only ~8 KiB): a paste-sized frame exceeds the sink's non-parking
    /// small-frame limit, so `seam_egress` routes it to the BLOCKING `write_frame`
    /// path (where the sink's spill-capacity backpressure applies) — this thread,
    /// not the event loop that serves rendering AND input for EVERY window/tab, is
    /// what absorbs that park. The bytes are still produced by the SAME
    /// `seam_egress`, so Human and Controller paste stay byte-identical (the
    /// indistinguishability invariant is untouched — only WHERE the write runs
    /// moves, and only for the Human/GUI path). The detached thread holds `Arc`
    /// clones of the term + sink, so the PTY master fd stays open for the whole
    /// write (the OwnedFd-closes-on-last-clone-drop contract) and whole-frame
    /// atomicity is the sink's own guarantee (direct writes serialize under its
    /// lock; a wedged-tty spill stays frame-contiguous). On session teardown the
    /// slave closes and the parked write returns an error, so the thread always
    /// ends — no leak. `ev` must be `InputEvent::Paste(_)`.
    fn input_paste(
        &mut self,
        wid: WindowId,
        ev: InputEvent,
        term: &Arc<Mutex<Terminal>>,
        sink: &Arc<SinkWriter>,
    ) -> InputOutcome {
        self.snap_to_bottom(wid);
        // Enqueue the paste on the session's ordered FIFO: it writes OFF the UI
        // thread (a 16 MiB paste into a stalled child must never block the event
        // loop) AND any keystroke submitted while it drains queues BEHIND it, so
        // the child sees the paste before that later input. Falls back to a
        // detached write only if the FIFO writer thread could not be spawned.
        if let Err(ev) = paste_order::enqueue(term, sink, ev) {
            let term = term.clone();
            let sink = sink.clone();
            std::thread::spawn(move || {
                // Detached paste fallback: expendable thread, block under SPILL_CAP.
                input::seam_egress(&term, &sink, &ev, input::EgressMode::Backpressured);
            });
        }
        InputOutcome::Ok
    }

    /// `InputEvent::Resize` arm of [`App::input`] (range-reject reportable):
    /// `echo_to_window` picks the apply path WITHOUT branching on `Source` (it is
    /// keyed on WHERE the geometry came from). The control `resize` verb (no window
    /// event) echoes the new size to the window (`apply_grid_resize` ->
    /// `request_inner_size`); the winit `Resized` handler (window already this size)
    /// applies just the term+PTY+framebuffer (`apply_term_resize`) so it never fights
    /// an interactive edge-drag. A `Resized` for a SHARED
    /// (Cmd-Shift-O) session is driven to the element-wise min across co-viewers
    /// inside `apply_term_resize` so it can't corrupt the other viewer's display.
    fn input_resize(
        &mut self,
        wid: WindowId,
        rows: u16,
        cols: u16,
        echo_to_window: bool,
    ) -> InputOutcome {
        if !(1..=aterm_core::grid::MAX_GRID_ROWS).contains(&rows)
            || !(1..=aterm_core::grid::MAX_GRID_COLS).contains(&cols)
        {
            return InputOutcome::RangeRejected;
        }
        if echo_to_window {
            self.apply_grid_resize(rows, cols);
        } else {
            self.apply_term_resize(wid, rows, cols);
        }
        // Native editor caret reveal is a renderer-geometry transition too.
        // Reconcile after the authoritative rows/cols update so compact,
        // landscape, and accessibility-scaled views never retain the old
        // desktop row-capacity guess.
        let target = if echo_to_window {
            self.frontmost_window.unwrap_or(wid)
        } else {
            wid
        };
        let _ = self.reconcile_active_editor_viewport(target);
        InputOutcome::Ok
    }

    /// [`InputEvent::ResizeWindowPx`] arm of [`App::input`]: ask the OS window for
    /// an inner size and apply NOTHING — the grid, the PTY and the swapchain all
    /// follow from the `Resized` event the platform delivers, through the same
    /// `on_resize_throttled` path a human's edge drag runs.
    ///
    /// That inversion is the whole reason this exists. [`Self::input_resize`]
    /// (the `resize <r> <c>` verb) applies the grid first and echoes the pixel size
    /// after, so the window event arrives with the columns already correct and
    /// `resize_changes_columns` reports no reflow — the width throttle, its
    /// coalescing and its trailing settle were unreachable from the control socket,
    /// and every driven resize measured the one arm a drag never takes.
    ///
    /// Bounds are the WINDOW's business, not the grid's, so there is no
    /// `MAX_GRID_*` check here: a request the window manager clamps or refuses is
    /// reported honestly by the `Resized` (or absent `Resized`) that follows, and
    /// `pad_split` already floors the derived grid at one cell. Zero in either axis
    /// is rejected — nothing is presentable at zero, and the `Resized` handler
    /// discards such an event anyway, so accepting it would return `Ok` for a
    /// request that provably does nothing.
    ///
    /// Headless (no `os_window`) is a no-op rather than an error: the same shape as
    /// every other window-directed verb in a windowless run.
    fn input_resize_window_px(&mut self, wid: WindowId, width: u32, height: u32) -> InputOutcome {
        if width == 0 || height == 0 {
            return InputOutcome::RangeRejected;
        }
        // The control verb follows the front window, matching `apply_grid_resize`.
        let target = self.frontmost_window.unwrap_or(wid);
        if let Some(w) = self.windows.get(&target).and_then(|ws| ws.os_window.as_ref()) {
            // Best-effort by contract: the WM may clamp or ignore it. Deliberately
            // NOT followed by any local geometry write — believing our own request
            // is exactly the pre-application this variant exists to avoid.
            let _ = w.request_inner_size(winit::dpi::PhysicalSize::new(width, height));
        }
        InputOutcome::Ok
    }

    /// Seam-internal left-press gesture dispatch shared by both sources (the
    /// tracking-OFF branch of `InputEvent::MouseButton`). `click_count` is
    /// authoritative (Human: from the streak FSM; Controller: carried 1..=3); it
    /// does NOT touch `App.last_press` here — the streak state belongs to the
    /// Human handler, which owns it (A.2.2).
    ///
    /// SOURCE-BLIND: the single-click selection TYPE (Block vs Simple) comes from
    /// the `block` flag carried ON the event, NOT from `self.mods` — so a held
    /// human Alt can't leak into a controller-driven press, and a controller can
    /// drive a block selection by sending `block=1`. The Human builder snapshots
    /// `self.mods.alt_key()` into `block` at event-build time in `on_mouse_input`.
    pub(crate) fn seam_left_press(
        &mut self,
        wid: WindowId,
        row: u16,
        col: u16,
        click_count: u8,
        block: bool,
    ) {
        let Some(term) = self
            .front_terminal(wid)
            .map(|terminal| terminal.term.clone())
        else {
            return;
        };
        let sel_row = i32::from(row) - term_lock(&term).grid().display_offset() as i32;
        match click_count {
            2 => self.select_word_click(wid, sel_row, col),
            3 => self.select_line_click(wid, sel_row, col),
            _ => self.begin_selection(
                wid,
                if block {
                    SelectionType::Block
                } else {
                    SelectionType::Simple
                },
            ),
        }
    }

    /// Record that this key's PRESS was CONSUMED by a GUI gate (it produced no
    /// PTY key-press report), so [`on_key`]'s release branch swallows the matching
    /// RELEASE instead of leaking an orphan Kitty `REPORT_EVENT_TYPES` release report.
    /// Keyed on the winit PHYSICAL key (stable across the press↔release pair regardless
    /// of how modifiers compose the logical key), so the swallow can't be defeated by a
    /// modifier released between press and release.
    ///
    /// A REPEAT never notes: the GUI gates run for auto-repeat events too, so a chord
    /// formed MID-HOLD (plain PageUp held — its press already reported to the app —
    /// then Shift pressed, so the repeats now match the scrollback chord) would
    /// otherwise poison the release disposition and swallow a RELEASE the app is owed
    /// (an orphan press under Kitty `REPORT_EVENT_TYPES`). The press-time decision
    /// alone owns the release.
    fn note_consumed_press(&mut self, wid: WindowId, ev: &KeyEvent) {
        self.note_consumed_press_key(wid, ev.physical_key, ev.repeat);
    }

    /// [`note_consumed_press`]'s field-level core, split from the winit event so it is
    /// unit-testable: a winit `KeyEvent` cannot be constructed in tests (its
    /// `platform_specific` field is `pub(crate)` — the same limitation the Tier-1
    /// `builders_converge` test documents).
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "input_release_pairing",
            action = "ConsumePhysicalPress",
            project = "aterm_gui::app_input::input_release_pairing_conformance::project"
        )
    )]
    fn note_consumed_press_key(
        &mut self,
        wid: WindowId,
        physical_key: winit::keyboard::PhysicalKey,
        repeat: bool,
    ) {
        if repeat {
            return; // a repeat must not (re-)decide the release disposition
        }
        self.retire_previous_physical_press_owner(wid, physical_key);
        self.physical_press_owners.insert(
            physical_key,
            crate::PhysicalPressOwner::Consumed { window: wid },
        );
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.consumed_press_keys.insert(physical_key);
        }
    }

    /// Record the exact terminal/key identity that received a physical PRESS.
    /// Focus, tab, modifier, and layout state may all change before winit reports
    /// RELEASE; the release therefore follows this immutable press-time route.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "input_release_pairing",
            action = "ForwardPress",
            project = "aterm_gui::app_input::input_release_pairing_conformance::project"
        )
    )]
    fn note_forwarded_press_key(
        &mut self,
        wid: WindowId,
        physical_key: winit::keyboard::PhysicalKey,
        repeat: bool,
        key: aterm_types::keyboard::Key,
        mods: aterm_types::keyboard::Modifiers,
        base_layout: Option<char>,
    ) {
        if repeat {
            return;
        }
        let Some(session) = self.front_terminal(wid).map(|terminal| terminal.session) else {
            self.note_consumed_press_key(wid, physical_key, false);
            return;
        };
        self.retire_previous_physical_press_owner(wid, physical_key);
        self.physical_press_owners.insert(
            physical_key,
            crate::PhysicalPressOwner::Forwarded {
                window: wid,
                session,
                key,
                mods,
                base_layout,
            },
        );
    }

    /// Record and deliver literal text/raw bytes as one immutable physical-key
    /// episode. Unlike an encoded key press it has no CSI-u release peer, but
    /// any auto-repeat must retain this exact session and payload.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "input_release_pairing",
            action = "ForwardLiteralPress",
            project = "aterm_gui::app_input::input_release_pairing_conformance::project"
        )
    )]
    fn forward_literal_press(
        &mut self,
        wid: WindowId,
        physical_key: winit::keyboard::PhysicalKey,
        event: InputEvent,
    ) {
        debug_assert!(matches!(
            event,
            InputEvent::Text(_) | InputEvent::KeySequence(_)
        ));
        let Some(session) = self.front_terminal(wid).map(|terminal| terminal.session) else {
            self.note_consumed_press_key(wid, physical_key, false);
            return;
        };
        self.retire_previous_physical_press_owner(wid, physical_key);
        self.physical_press_owners.insert(
            physical_key,
            crate::PhysicalPressOwner::Literal {
                window: wid,
                session,
                event: event.clone(),
            },
        );
        #[cfg(test)]
        clear_physical_release_trace();
        #[cfg(test)]
        let trace_event = event.clone();
        let outcome = self.input_to_session(wid, event, Source::Human, Some(session));
        #[cfg(test)]
        record_physical_release_trace(PhysicalReleaseTrace::Literal {
            arrival_window: wid,
            press_window: wid,
            session,
            event: trace_event,
            repeated: false,
            delivery: match outcome {
                InputOutcome::Ok => input::Delivery::Full,
                InputOutcome::WriteFailed => input::Delivery::Failed,
                InputOutcome::RangeRejected => {
                    unreachable!("a raw key sequence cannot produce a resize range verdict")
                }
            },
        });
        #[cfg(not(test))]
        let _ = outcome;
    }

    /// Repeat the literal payload captured by [`forward_literal_press`].
    /// Returns `true` whenever that owner exists, even if its session has closed,
    /// so a stale hold can never inject the bytes into the replacement tab.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "input_release_pairing",
            action = "ForwardRepeatOfLiteralPress",
            project = "aterm_gui::app_input::input_release_pairing_conformance::project"
        )
    )]
    fn repeat_literal(
        &mut self,
        repeat_window: WindowId,
        physical_key: winit::keyboard::PhysicalKey,
    ) -> bool {
        #[cfg(test)]
        clear_physical_release_trace();
        let Some(crate::PhysicalPressOwner::Literal {
            window,
            session,
            event,
        }) = self.physical_press_owners.get(&physical_key).cloned()
        else {
            return false;
        };
        if self.pool.get(session).is_none() {
            return true;
        }
        #[cfg(test)]
        let trace_event = event.clone();
        let outcome = self.input_to_session(window, event, Source::Human, Some(session));
        #[cfg(test)]
        record_physical_release_trace(PhysicalReleaseTrace::Literal {
            arrival_window: repeat_window,
            press_window: window,
            session,
            event: trace_event,
            repeated: true,
            delivery: match outcome {
                InputOutcome::Ok => input::Delivery::Full,
                InputOutcome::WriteFailed => input::Delivery::Failed,
                InputOutcome::RangeRejected => {
                    unreachable!("a raw key sequence cannot produce a resize range verdict")
                }
            },
        });
        #[cfg(not(test))]
        let _ = (repeat_window, outcome);
        true
    }

    /// Capture one repeatable GUI/native action after its genuine press was
    /// classified. The normalized action, original window, and any content/
    /// session identity inside it become immutable authority for the hold.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "input_release_pairing",
            action = "CaptureLocalRepeatPress",
            project = "aterm_gui::app_input::input_release_pairing_conformance::project"
        )
    )]
    fn note_local_repeat_press(
        &mut self,
        wid: WindowId,
        physical_key: winit::keyboard::PhysicalKey,
        action: crate::LocalRepeatAction,
    ) {
        self.retire_previous_physical_press_owner(wid, physical_key);
        self.physical_press_owners.insert(
            physical_key,
            crate::PhysicalPressOwner::Local {
                window: wid,
                action,
            },
        );
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.consumed_press_keys.insert(physical_key);
        }
    }

    /// One press-disposition tail for [`on_key`]'s gate chain: a gate that
    /// consumed a genuine press either captures a repeatable action
    /// ([`Self::note_local_repeat_press`]) or records a plain consumed press
    /// ([`Self::note_consumed_press`]).
    fn note_press_disposition(
        &mut self,
        wid: WindowId,
        ev: &KeyEvent,
        repeat: Option<crate::LocalRepeatAction>,
    ) {
        match repeat {
            Some(action) => self.note_local_repeat_press(wid, ev.physical_key, action),
            None => self.note_consumed_press(wid, ev),
        }
    }

    /// Apply one captured local repeat only while its original owner identity
    /// remains live. A stale owner returns `false` and is silently retained until
    /// release; it can never fall through into newly focused content.
    fn apply_local_repeat_action(
        &mut self,
        wid: WindowId,
        action: crate::LocalRepeatAction,
    ) -> bool {
        use crate::{FontZoomRepeatAction, LocalRepeatAction};

        match action {
            LocalRepeatAction::Native { view, mut event } => {
                if self.active_native_view(wid).map(|(_, active)| active) != Some(view) {
                    return false;
                }
                if let InputEvent::Key { event_type, .. } = &mut event {
                    *event_type = aterm_types::keyboard::KeyEventType::Repeat;
                }
                self.note_update_handoff_activity();
                self.reset_blink(wid);
                self.native_input_event(wid, &event)
            }
            LocalRepeatAction::Search { session, action } => {
                if self.front_terminal(wid).map(|terminal| terminal.session) != Some(session)
                    || self.windows.get(&wid).is_none_or(|ws| ws.search.is_none())
                {
                    return false;
                }
                self.note_update_handoff_activity();
                self.reset_blink(wid);
                self.apply_search_repeat_action(wid, action);
                true
            }
            LocalRepeatAction::Vi { session, action } => {
                let Some(term) = self
                    .front_terminal(wid)
                    .filter(|terminal| terminal.session == session)
                    .map(|terminal| terminal.term.clone())
                else {
                    return false;
                };
                if !term_lock(&term).vi_is_active() {
                    return false;
                }
                self.note_update_handoff_activity();
                self.reset_blink(wid);
                match action {
                    crate::vi_keys::ViAction::Motion(motion) => {
                        term_lock(&term).vi_motion(motion, aterm_core::ViBoundary::Grid);
                    }
                    crate::vi_keys::ViAction::RepeatInline { reverse } => {
                        let mut terminal = term_lock(&term);
                        if reverse {
                            terminal.vi_inline_search_repeat_reverse();
                        } else {
                            terminal.vi_inline_search_repeat();
                        }
                    }
                    _ => return false,
                }
                self.vi_after_key(wid);
                true
            }
            LocalRepeatAction::Scroll { session, intent } => {
                if self.front_terminal(wid).map(|terminal| terminal.session) != Some(session) {
                    return false;
                }
                let _ = self.input_to_session(
                    wid,
                    InputEvent::ScrollView(intent),
                    Source::Human,
                    Some(session),
                );
                true
            }
            LocalRepeatAction::FontZoom(action) => {
                if !self.windows.contains_key(&wid) {
                    return false;
                }
                self.note_update_handoff_activity();
                match action {
                    FontZoomRepeatAction::Increase => {
                        self.set_font_px(self.font_px + FONT_ZOOM_STEP);
                    }
                    FontZoomRepeatAction::Decrease => {
                        self.set_font_px(self.font_px - FONT_ZOOM_STEP);
                    }
                    FontZoomRepeatAction::Reset => self.set_font_px(self.default_font_px),
                }
                true
            }
            LocalRepeatAction::Palette(event) => {
                if self
                    .windows
                    .get(&wid)
                    .and_then(|ws| ws.overlay.as_ref())
                    .map(|overlay| overlay.kind())
                    != Some(crate::overlay::OverlayKind::Palette)
                {
                    return false;
                }
                self.note_update_handoff_activity();
                self.reset_blink(wid);
                self.palette_input_event(wid, &event);
                true
            }
        }
    }

    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "input_release_pairing",
            action = "ForwardLocalRepeat",
            project = "aterm_gui::app_input::input_release_pairing_conformance::project"
        )
    )]
    fn repeat_local_press(
        &mut self,
        arrival_window: WindowId,
        physical_key: winit::keyboard::PhysicalKey,
    ) -> bool {
        #[cfg(test)]
        clear_physical_release_trace();
        let Some(crate::PhysicalPressOwner::Local { window, action }) =
            self.physical_press_owners.get(&physical_key).cloned()
        else {
            return false;
        };
        if self.apply_local_repeat_action(window, action) {
            #[cfg(test)]
            record_physical_release_trace(PhysicalReleaseTrace::Local {
                arrival_window,
                press_window: window,
            });
        }
        #[cfg(not(test))]
        let _ = arrival_window;
        true
    }

    /// Close a stale press episode before a fresh non-repeat press takes its
    /// physical key. This is the recovery path when the OS lost key-up during a
    /// focus epoch: consumed/literal/local owners retire silently, while a
    /// forwarded owner emits its still-owed release to the exact original
    /// session. Merely dropping that owner would leave Kitty applications with
    /// a permanently held key even though the new key-down is authoritative.
    fn retire_previous_physical_press_owner(
        &mut self,
        replacement_window: WindowId,
        physical_key: winit::keyboard::PhysicalKey,
    ) {
        if self.physical_press_owners.contains_key(&physical_key) {
            // The ordinary release seam already carries every proven rule:
            // exact press-time session/key/modifiers, paste FIFO ordering, and
            // byte-silent retirement for non-protocol owners.
            self.release_physical_press(replacement_window, physical_key);
        }
    }

    /// Whether `physical_key`'s press is currently tracked as
    /// GUI-consumed — a PEEK ([`take_consumed_release`] without the remove), for
    /// [`on_key`]'s repeat fall-through swallow. The repeat must NOT un-track the key:
    /// the eventual RELEASE still needs the entry to be swallowed itself.
    fn press_was_consumed(
        &self,
        _wid: WindowId,
        physical_key: winit::keyboard::PhysicalKey,
    ) -> bool {
        matches!(
            self.physical_press_owners.get(&physical_key),
            Some(crate::PhysicalPressOwner::Consumed { .. })
        )
    }

    /// A repeat without an observed physical press has no legitimate protocol
    /// peer or immutable destination. This occurs after an OS focus epoch begins
    /// mid-hold or loses key-down; fail closed so it cannot fabricate a Kitty
    /// repeat or literal payload in whichever terminal is focused now.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "input_release_pairing",
            action = "SwallowUntrackedRepeat",
            project = "aterm_gui::app_input::input_release_pairing_conformance::project"
        )
    )]
    fn repeat_is_untracked(&self, physical_key: winit::keyboard::PhysicalKey) -> bool {
        !self.physical_press_owners.contains_key(&physical_key)
    }

    /// Resolve an OS auto-repeat entirely from press-time physical ownership.
    /// Called at the top of [`on_key`], before current-window blink, native, or
    /// shortcut gates, so a content/focus/modifier change cannot reinterpret a
    /// held key. Every repeat is consumed here: encoded/literal owners route to
    /// their original session, GUI-consumed owners stay silent, and an
    /// untracked focus-epoch repeat fails closed.
    fn route_physical_repeat(
        &mut self,
        arrival_window: WindowId,
        physical_key: winit::keyboard::PhysicalKey,
    ) {
        if self.repeat_local_press(arrival_window, physical_key) {
            return;
        }
        if self.press_was_consumed(arrival_window, physical_key) {
            return;
        }
        if self.repeat_literal(arrival_window, physical_key) {
            return;
        }
        if self.repeat_forwarded_press(arrival_window, physical_key) {
            return;
        }
        if self.repeat_is_untracked(physical_key) {
            return;
        }
        unreachable!("every physical press-owner variant was handled above");
    }

    /// Forward an auto-repeat according to the immutable owner established by
    /// the physical press. This runs before live GUI/content gates, so a
    /// focus/tab/modifier change cannot redirect, rebuild, or dispatch the held
    /// event. The owner remains installed so every later repeat and the final
    /// release use the same route.
    ///
    /// Returns `true` whenever a forwarded owner exists, including when its
    /// session has just closed: such a repeat is terminally stale and must be
    /// swallowed rather than fabricated in the newly focused session.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "input_release_pairing",
            action = "ForwardRepeatOfForwardedPress",
            project = "aterm_gui::app_input::input_release_pairing_conformance::project"
        )
    )]
    fn repeat_forwarded_press(
        &mut self,
        repeat_window: WindowId,
        physical_key: winit::keyboard::PhysicalKey,
    ) -> bool {
        #[cfg(test)]
        clear_physical_release_trace();
        let Some(crate::PhysicalPressOwner::Forwarded {
            window,
            session,
            key,
            mods,
            base_layout,
        }) = self.physical_press_owners.get(&physical_key).cloned()
        else {
            return false;
        };
        if self.pool.get(session).is_none() {
            return true;
        }

        #[cfg(test)]
        let trace_key = key.clone();
        let outcome = self.input_to_session(
            window,
            InputEvent::Key {
                key,
                mods,
                base_layout,
                event_type: aterm_types::keyboard::KeyEventType::Repeat,
            },
            Source::Human,
            Some(session),
        );
        #[cfg(test)]
        {
            let delivery = match outcome {
                InputOutcome::Ok => input::Delivery::Full,
                InputOutcome::WriteFailed => input::Delivery::Failed,
                InputOutcome::RangeRejected => {
                    unreachable!("a key repeat cannot produce a resize range verdict")
                }
            };
            record_physical_release_trace(PhysicalReleaseTrace::Forwarded {
                arrival_window: repeat_window,
                press_window: window,
                session,
                key: trace_key,
                mods,
                base_layout,
                event_type: aterm_types::keyboard::KeyEventType::Repeat,
                delivery,
            });
        }
        #[cfg(not(test))]
        let _ = (repeat_window, outcome);
        true
    }

    /// If `physical_key`'s PRESS was recorded as GUI-consumed (by
    /// [`note_consumed_press`]), REMOVE it and return `true` so [`on_key`]'s release
    /// branch swallows the matching RELEASE without encoding — suppressing the orphan
    /// Kitty `REPORT_EVENT_TYPES` release report. Removing on the FIRST release keeps
    /// the set leak-free and ensures a later, unrelated press of the same physical key
    /// is unaffected. Returns `false` when the owner is forwarded or absent.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "input_release_pairing",
            action = "ReleaseConsumedPress",
            project = "aterm_gui::app_input::input_release_pairing_conformance::project"
        )
    )]
    fn take_consumed_release(
        &mut self,
        release_window: WindowId,
        physical_key: winit::keyboard::PhysicalKey,
    ) -> bool {
        #[cfg(not(test))]
        let _ = release_window;
        let Some(crate::PhysicalPressOwner::Consumed { window }) =
            self.physical_press_owners.get(&physical_key).cloned()
        else {
            return false;
        };
        self.physical_press_owners.remove(&physical_key);
        if let Some(ws) = self.windows.get_mut(&window) {
            ws.consumed_press_keys.remove(&physical_key);
        }
        #[cfg(test)]
        record_physical_release_trace(PhysicalReleaseTrace::Consumed {
            arrival_window: release_window,
            press_window: window,
        });
        true
    }

    /// Retire a literal-sequence episode without fabricating a protocol release.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "input_release_pairing",
            action = "ReleaseLiteralPress",
            project = "aterm_gui::app_input::input_release_pairing_conformance::project"
        )
    )]
    fn take_literal_release(
        &mut self,
        release_window: WindowId,
        physical_key: winit::keyboard::PhysicalKey,
    ) -> bool {
        #[cfg(not(test))]
        let _ = release_window;
        let Some(crate::PhysicalPressOwner::Literal { window, .. }) =
            self.physical_press_owners.get(&physical_key).cloned()
        else {
            return false;
        };
        #[cfg(not(test))]
        let _ = window;
        self.physical_press_owners.remove(&physical_key);
        #[cfg(test)]
        record_physical_release_trace(PhysicalReleaseTrace::Consumed {
            arrival_window: release_window,
            press_window: window,
        });
        true
    }

    /// Retire a repeatable GUI/native episode. Local actions never have a Kitty
    /// key-press peer, so release is byte-silent just like a one-shot GUI gate.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "input_release_pairing",
            action = "ReleaseLocalRepeatPress",
            project = "aterm_gui::app_input::input_release_pairing_conformance::project"
        )
    )]
    fn take_local_repeat_release(
        &mut self,
        release_window: WindowId,
        physical_key: winit::keyboard::PhysicalKey,
    ) -> bool {
        #[cfg(not(test))]
        let _ = release_window;
        let Some(crate::PhysicalPressOwner::Local { window, .. }) =
            self.physical_press_owners.get(&physical_key).cloned()
        else {
            return false;
        };
        self.physical_press_owners.remove(&physical_key);
        if let Some(ws) = self.windows.get_mut(&window) {
            ws.consumed_press_keys.remove(&physical_key);
        }
        #[cfg(test)]
        record_physical_release_trace(PhysicalReleaseTrace::Consumed {
            arrival_window: release_window,
            press_window: window,
        });
        true
    }

    /// Finish one physical hold according to its press-time disposition. A
    /// release with no observed press is byte-silent; a forwarded release goes
    /// to the original session with the original logical key/modifiers/layout,
    /// never to whichever window happens to have focus now.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "input_release_pairing",
            action = "ReleaseForwardedPress",
            project = "aterm_gui::app_input::input_release_pairing_conformance::project"
        )
    )]
    fn release_physical_press(
        &mut self,
        release_window: WindowId,
        physical_key: winit::keyboard::PhysicalKey,
    ) {
        #[cfg(test)]
        clear_physical_release_trace();
        if self.take_consumed_release(release_window, physical_key) {
            return;
        }
        if self.take_literal_release(release_window, physical_key) {
            return;
        }
        if self.take_local_repeat_release(release_window, physical_key) {
            return;
        }
        let Some(owner) = self.physical_press_owners.remove(&physical_key) else {
            return;
        };
        match owner {
            crate::PhysicalPressOwner::Consumed { .. } => {
                unreachable!("consumed physical owner must be removed by take_consumed_release")
            }
            crate::PhysicalPressOwner::Forwarded {
                window,
                session,
                key,
                mods,
                base_layout,
            } => {
                // A key RELEASE is the same tolerated key traffic as its press:
                // `input_to_session` and the window-event seam both buffer
                // `Key`/`KeyboardInput` events (all event types) through a
                // pending seamless-update overlap, so this out-of-band release
                // path must not bump the activity epoch (which falsifies the
                // `exact_activity` Commit fact) or poke the overlap's cancel
                // channel.
                self.note_update_handoff_tolerated_activity();
                #[cfg(test)]
                let trace_key = key.clone();
                #[cfg(not(test))]
                let _ = window;
                // BYTE-SILENT RELEASE: unless the app negotiated Kitty
                // `REPORT_EVENT_TYPES`, `encode_key_with_layout` returns an EMPTY
                // vec for a Release, so in essentially every shell taking the seam
                // costs the terminal mutex (the one lock the main thread shares with
                // the PTY reader's `process()` bouts) once per key-up purely to
                // encode nothing. The press published that answer lock-free
                // (`publish_release_relevance`), so a proven-silent release skips
                // the seam entirely. Byte-identical by construction: the seam
                // writes nothing and reports `Delivery::Full` ("nothing to deliver
                // and nothing lost") for exactly this case, which is the verdict
                // synthesized here. `None` (nothing sampled for this session) keeps
                // the ask-the-engine path.
                let byte_silent = sampled_release_relevance(session) == Some(false);
                let Some(session) = self.pool.get(session) else {
                    return;
                };
                let event = InputEvent::Key {
                    key,
                    mods,
                    base_layout,
                    event_type: aterm_types::keyboard::KeyEventType::Release,
                };
                // Preserve the convergence seam's per-session ordering rule:
                // a release observed while a paste is draining must queue behind
                // that paste, even though focus moved and we cannot call `input`
                // (which would re-resolve the now-current session). A successful
                // enqueue is full delivery into the ordered spill contract.
                let egress = if byte_silent {
                    // Nothing to order behind a draining paste either: an event that
                    // encodes to zero bytes cannot overtake anything.
                    input::Egress::Reported(input::Delivery::Full)
                } else {
                    paste_order::ordered_or_inline(
                        &session.term,
                        &session.ctx.sink,
                        &event,
                        input::EgressMode::Interactive,
                    )
                    .0
                };
                #[cfg(test)]
                {
                    let input::Egress::Reported(delivery) = egress else {
                        unreachable!("a key release always returns Reported")
                    };
                    record_physical_release_trace(PhysicalReleaseTrace::Forwarded {
                        arrival_window: release_window,
                        press_window: window,
                        session: session.id,
                        key: trace_key,
                        mods,
                        base_layout,
                        event_type: aterm_types::keyboard::KeyEventType::Release,
                        delivery,
                    });
                }
                #[cfg(not(test))]
                let _ = egress;
            }
            crate::PhysicalPressOwner::Literal { .. } => {
                unreachable!("literal physical owner must be removed by take_literal_release")
            }
            crate::PhysicalPressOwner::Local { .. } => {
                unreachable!("local physical owner must be removed by take_local_repeat_release")
            }
        }
    }

    /// Route one physical key press while a native view owns the window.
    ///
    /// Returns `false` only when no native view is active. Every native press is
    /// otherwise consumed exactly once: app/window-scoped commands may execute,
    /// editor/Settings keys enter the neutral [`InputEvent`] seam, and
    /// terminal-only bindings/raw byte sequences are swallowed rather than
    /// reaching the parked PTY.
    fn on_key_native_mode(&mut self, wid: WindowId, mods: ModifiersState, ev: &KeyEvent) -> bool {
        let Some((_, native_view)) = self.active_native_view(wid) else {
            return false;
        };

        // Configured commands remain useful over app tabs, but an explicit raw
        // sequence is terminal authority and can never cross this boundary.
        if !self.keybindings.is_empty() || !self.key_sequences.is_empty() {
            let base = base_logical_key(ev);
            match keybinding::resolve_chord(&base, mods, &self.keybindings, &self.key_sequences) {
                keybinding::ChordResolution::Action(action) => {
                    if action == keybinding::Action::Copy {
                        let _ = self.copy_native_selection(wid);
                    } else if native_binding_allowed(action) {
                        self.dispatch_action(wid, action);
                    }
                    self.note_consumed_press(wid, ev);
                    return true;
                }
                keybinding::ChordResolution::Sequence(_) => {
                    self.note_consumed_press(wid, ev);
                    return true;
                }
                keybinding::ChordResolution::FallThrough => {}
            }
        }

        // These are content-agnostic host commands (or explicitly fenced no-ops
        // for unsupported native splits). Keep their canonical implementations,
        // but run them before editor command lowering.
        if self.on_key_super_shift_chord(mods, ev)
            || self.on_key_super_chord(mods, ev)
            || self.on_key_pane_focus(mods, ev)
        {
            self.note_consumed_press(wid, ev);
            return true;
        }

        // Clipboard and find need host capabilities rather than reducer-only key
        // events. Their implementations resolve the active native view and never
        // read the parked terminal here.
        if mods.super_key()
            && let Key::Character(character) = &ev.logical_key
        {
            if character.eq_ignore_ascii_case("f") {
                self.find_requested();
                self.note_consumed_press(wid, ev);
                return true;
            }
            if character.eq_ignore_ascii_case("c") {
                let _ = self.copy_native_selection(wid);
                self.note_consumed_press(wid, ev);
                return true;
            }
            if character.eq_ignore_ascii_case("v") {
                self.paste_clipboard();
                self.note_consumed_press(wid, ev);
                return true;
            }
        }

        // While a platform composition is live, its eventual Ime::Commit owns
        // the text. Direct key lowering would duplicate the composed grapheme.
        if self.native_preedit_active(wid) {
            self.note_consumed_press(wid, ev);
            return true;
        }

        let km_mods = keymap::modifiers_from_winit(mods) | keymap::lock_modifiers();
        if let Some((key, km_mods, base_layout)) = keymap::build_key_input(ev, km_mods) {
            let input = InputEvent::Key {
                key,
                mods: km_mods,
                base_layout,
                event_type: aterm_types::keyboard::KeyEventType::Press,
            };
            self.note_local_repeat_press(
                wid,
                ev.physical_key,
                crate::LocalRepeatAction::Native {
                    view: native_view,
                    event: input.clone(),
                },
            );
            self.input(wid, input, Source::Human);
            return true;
        }

        let bare = !mods.control_key() && !mods.alt_key() && !mods.super_key();
        if let Some(text) = &ev.text
            && bare
            && !text.is_empty()
        {
            let input = InputEvent::Text(text.to_string());
            self.note_local_repeat_press(
                wid,
                ev.physical_key,
                crate::LocalRepeatAction::Native {
                    view: native_view,
                    event: input.clone(),
                },
            );
            self.input(wid, input, Source::Human);
        } else {
            self.note_consumed_press(wid, ev);
        }
        true
    }

    fn native_preedit_active(&self, wid: WindowId) -> bool {
        let Some((_, view)) = self.active_native_view(wid) else {
            return false;
        };
        match self.native_runtime.view_state(view) {
            Some(crate::native_app::AppViewState::Editor(state)) => !state.preedit.is_empty(),
            Some(crate::native_app::AppViewState::Settings(state)) => state
                .editing_field
                .as_ref()
                .and_then(|key| state.field_inputs.get(key))
                .unwrap_or(&state.search_input)
                .preedit()
                .is_some(),
            Some(
                crate::native_app::AppViewState::Markdown(_)
                | crate::native_app::AppViewState::Recovery(_),
            )
            | None => false,
        }
    }

    /// Resolve only text selected by the active native view. This is the shared
    /// authority for Cmd-C and command-palette enablement; neither may consult the
    /// selection of a terminal hidden beneath a native tab.
    pub(crate) fn native_selection_text(&self, wid: WindowId) -> Option<String> {
        let (instance, view) = self.active_native_view(wid)?;
        let text = match self.native_runtime.view_state(view) {
            Some(crate::native_app::AppViewState::Editor(state)) => {
                let buffer = state.buffer.as_ref()?;
                let snapshot = self.document_store.snapshot(buffer.document)?;
                let selected = buffer
                    .selections
                    .iter()
                    .filter_map(|selection| {
                        let range = selection.range();
                        (!range.is_empty()
                            && range.end <= snapshot.text.len()
                            && snapshot.text.is_char_boundary(range.start)
                            && snapshot.text.is_char_boundary(range.end))
                        .then(|| snapshot.text[range].to_string())
                    })
                    .collect::<Vec<_>>();
                selected.join("\n")
            }
            Some(crate::native_app::AppViewState::Markdown(state)) => {
                let range = state.selection.clone()?;
                let document = self.native_runtime.document_id(instance)?;
                let snapshot = self.document_store.snapshot(document)?;
                if range.end > snapshot.text.len()
                    || !snapshot.text.is_char_boundary(range.start)
                    || !snapshot.text.is_char_boundary(range.end)
                {
                    return None;
                }
                snapshot.text[range].to_string()
            }
            Some(crate::native_app::AppViewState::Settings(state)) => {
                let input = state
                    .editing_field
                    .as_ref()
                    .and_then(|key| state.field_inputs.get(key))
                    .unwrap_or(&state.search_input);
                input.selected_text().to_string()
            }
            Some(crate::native_app::AppViewState::Recovery(_)) => return None,
            None => return None,
        };
        (!text.is_empty()).then_some(text)
    }

    /// Copy only text selected by the active native view.
    fn copy_native_selection(&self, wid: WindowId) -> bool {
        self.native_selection_text(wid)
            .is_some_and(|text| crate::control::pbcopy(&text))
    }

    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "FocusModifierCache",
            action = "PressL",
            project = "aterm_gui::multi_window_tests::project_focus_modifier_cache"
        )
    )]
    pub(crate) fn on_key(&mut self, wid: WindowId, ev: KeyEvent) {
        if ev.state != ElementState::Pressed {
            // Key RELEASE. Only the Kitty keyboard protocol's event-type reporting
            // (REPORT_EVENT_TYPES) consumes it; forward it straight to the seam encoder,
            // which emits a release report ONLY in that mode and NOTHING in
            // legacy/default mode. The shortcut / keybinding / IME / Cmd handling below
            // is press-only and is intentionally skipped; the seam's press side-effects
            // are gated off a release.
            //
            // ORPHAN-RELEASE SUPPRESSION: if this key's PRESS was CONSUMED by a
            // GUI gate (settings/search mode, a keybinding `Action`, a `[key_sequences]`
            // raw-byte rule, a Cmd/Super shortcut, a scrollback / pane-focus / font-zoom
            // chord, or an IME-suppressed key) it produced NO PTY key-press report, so
            // its RELEASE must not be encoded either — otherwise a Kitty
            // `REPORT_EVENT_TYPES` app receives a release for a key it never saw
            // pressed (breaking the protocol's press/release pairing). The disposition
            // is tracked from the PHYSICAL key at press time and must NEVER be
            // re-derived here from the live gates: modifier state can differ between
            // press and release, so a chord formed mid-hold would swallow a release the
            // app is owed and a chord broken mid-hold would leak a release for a press
            // the app never saw. Checked FIRST so the entry is always removed (no stale
            // key can survive to mis-gate a later press's release). Legacy/non-Kitty
            // releases encode to nothing, so this is a byte-identical no-op there.
            // `[key_sequences]` presses are tracked here too, noted at the press site
            // that actually sent the mapped raw bytes.
            self.release_physical_press(wid, ev.physical_key);
            return;
        }
        // A repeat belongs to the physical PRESS epoch, not to the content,
        // focus, modifiers, or keybinding state visible when winit delivers it.
        // Route it before even resetting this arrival window's blink: routing any
        // later lets a terminal-A hold type/animate/dispatch into terminal-B or a
        // native tab.
        if ev.repeat {
            self.route_physical_repeat(wid, ev.physical_key);
            return;
        }
        // The current modifier state for this window (a `Copy` snapshot, so the
        // borrow does not outlive the read). No such window ⇒ nothing to do.
        let Some(mods) = self.windows.get(&wid).map(|ws| ws.mods) else {
            return;
        };
        // Typing makes the cursor solid and restarts the blink period.
        self.reset_blink(wid);
        // While the Settings overlay is open it OWNS the keyboard: swallow every key —
        // move / activate / edit / close — BEFORE any keybinding, `[key_sequences]` rule,
        // hardcoded Cmd chord, scrollback chord, or Cmd-F can fire. Checked first, mirroring
        // the settings-first gate in `on_mouse_input`; without this a `[key_sequences]` rule
        // would write RAW bytes to the PTY (and chords would mutate the window) under the
        // modal. `on_key_settings_mode` returns `true` for every key while the panel is up.
        let overlay_repeat = self.palette_repeat_event(wid, &ev);
        if self.on_key_overlay_mode(wid, mods, &ev) {
            self.note_press_disposition(
                wid,
                &ev,
                overlay_repeat.map(crate::LocalRepeatAction::Palette),
            );
            return;
        }
        // NATIVE KEYBOARD OWNERSHIP: once a native view is frontmost, classify
        // host commands and lower the key into the engine-neutral input seam
        // BEFORE any terminal vi/find/raw-sequence/PTY shortcut path can observe
        // it. This is the physical-winit counterpart of `App::input`'s native
        // boundary: it claims Cmd-S/Cmd-Z for the native view and stops a
        // configured `[key_sequences]` rule writing raw bytes to the parked
        // terminal beneath an editor or Settings tab.
        if self.on_key_native_mode(wid, mods, &ev) {
            return;
        }
        // User-rebindable shortcuts (config `[keybindings]`) take precedence. The
        // lookup is O(1) and SKIPPED entirely when no bindings are configured
        // (the empty-map default), so the hardcoded path below is byte-identical
        // with no config. A configured chord dispatches its action and returns; a
        // MISS falls through to the hardcoded matches. Keybindings are GLOBAL;
        // dispatch is threaded with the routed `wid`.
        if !self.keybindings.is_empty() || !self.key_sequences.is_empty() {
            // Match on the modifier-independent BASE key (e.g. `]` under Shift, not `}`)
            // so a binding the user wrote matches across layouts — the same base key
            // `build_key_input` encodes with. The keybindings-first-then-key_sequences-
            // else-fallthrough MAP precedence is the pure `keybinding::resolve_chord`;
            // the match-arm ORDERING here — this whole block runs BEFORE the hardcoded
            // Cmd shortcut block below, so a key_sequences rule SHADOWS the built-in
            // chord — is policy on_key owns and the helper cannot capture.
            let base = base_logical_key(&ev);
            match keybinding::resolve_chord(&base, mods, &self.keybindings, &self.key_sequences) {
                // A configured chord that would TYPE, COPY, or MOVE THE VIEWPORT is
                // suspended while the find field owns input — otherwise a user who bound
                // `ctrl+a` or `cmd+v` loses that chord inside the field, and `Paste`
                // writes the clipboard to the PTY from under a focused text field (the
                // same modal-focus leak the `[key_sequences]` guard below exists to stop).
                // Everything else — window/tab/pane/font/settings, and Find itself —
                // still fires: those are app-level, not text-level.
                keybinding::ChordResolution::Action(action)
                    if find_suspends_action(action)
                        && self.windows.get(&wid).is_some_and(|ws| ws.search.is_some()) => {}
                keybinding::ChordResolution::Action(action) => {
                    let repeat_action = match action {
                        keybinding::Action::ScrollPageUp => Some(ScrollIntent::Up),
                        keybinding::Action::ScrollPageDown => Some(ScrollIntent::Down),
                        keybinding::Action::ScrollLineUp => Some(ScrollIntent::By(1)),
                        keybinding::Action::ScrollLineDown => Some(ScrollIntent::By(-1)),
                        keybinding::Action::ScrollToTop => Some(ScrollIntent::Top),
                        keybinding::Action::ScrollToBottom => Some(ScrollIntent::Bottom),
                        keybinding::Action::JumpPrevPrompt => Some(ScrollIntent::PrevPrompt),
                        keybinding::Action::JumpNextPrompt => Some(ScrollIntent::NextPrompt),
                        _ => None,
                    }
                    .and_then(|intent| {
                        self.front_terminal(wid)
                            .map(|terminal| crate::LocalRepeatAction::Scroll {
                                session: terminal.session,
                                intent,
                            })
                    });
                    let repeat_action = repeat_action.or(match action {
                        keybinding::Action::FontIncrease => {
                            Some(crate::LocalRepeatAction::FontZoom(
                                crate::FontZoomRepeatAction::Increase,
                            ))
                        }
                        keybinding::Action::FontDecrease => {
                            Some(crate::LocalRepeatAction::FontZoom(
                                crate::FontZoomRepeatAction::Decrease,
                            ))
                        }
                        keybinding::Action::FontReset => Some(crate::LocalRepeatAction::FontZoom(
                            crate::FontZoomRepeatAction::Reset,
                        )),
                        _ => None,
                    });
                    self.dispatch_action(wid, action);
                    self.note_press_disposition(wid, &ev, repeat_action);
                    return;
                }
                keybinding::ChordResolution::Sequence(bytes) => {
                    // aterm INPUT POLICY: a `[key_sequences]` rule sends RAW bytes to the
                    // PTY, overriding the default encoder — an explicit rule always wins.
                    // SKIPPED while the find overlay is open so the keystroke drives the
                    // search instead of leaking raw bytes (on_key_search_mode captures it).
                    let search_active =
                        self.windows.get(&wid).is_some_and(|ws| ws.search.is_some());
                    if !search_active {
                        // Literal sequences have their own press-time owner. A genuine
                        // press captures the exact session + bytes; its repeats reuse
                        // that route and its release stays silent (raw bytes have no
                        // Kitty release peer). Repeats never reach this arm: `on_key`
                        // routes every repeat through `route_physical_repeat` at the
                        // top, which owns the mid-hold chord-change case (a repeat
                        // must neither replace an existing encoded-key disposition
                        // nor inject raw bytes into a newly focused tab).
                        self.forward_literal_press(
                            wid,
                            ev.physical_key,
                            InputEvent::KeySequence(bytes),
                        );
                        return;
                    }
                }
                keybinding::ChordResolution::FallThrough => {}
            }
        }
        // VI-1: while keyboard copy-mode is active, intercept motion keys HERE — after the
        // rebindable chords (so `toggle_vi_mode` and other bindings still fire) but before
        // the built-in chords + the PTY encoder, so `h`/`j`/`k`/`l`/… drive the vi cursor
        // and never leak to the shell. Modified (⌃/⌘/⌥) keys pass through so ⌘C / paste /
        // pane chords still work on the vi selection. A no-op when vi mode is inactive.
        let vi_repeat = self.vi_repeat_action(wid, mods, &ev);
        if self.on_key_vi_mode(wid, mods, &ev) {
            self.note_press_disposition(
                wid,
                &ev,
                vi_repeat.map(|(session, action)| crate::LocalRepeatAction::Vi { session, action }),
            );
            return;
        }
        // Terminal Emacs navigation: native content, configured bindings, and vi copy
        // mode have already had first refusal. A genuine press captures a LOCAL repeat
        // owner, so a held Cmd-S/Cmd-R repeats search against the press-time terminal
        // while every Kitty release remains byte-silent. This host boundary is above
        // all shell/TUI encoders, so normal shells, Claude, and Codex see zero bytes.
        let base = base_logical_key(&ev);
        if let Some(forward) = terminal_emacs_search_direction(&base, mods) {
            self.terminal_emacs_search_pressed(wid, ev.physical_key, forward);
            return;
        }
        if self.on_key_super_shift_chord(mods, &ev) {
            self.note_consumed_press(wid, &ev);
            return;
        }
        if self.on_key_super_chord(mods, &ev) {
            self.note_consumed_press(wid, &ev);
            return;
        }
        if self.on_key_pane_focus(mods, &ev) {
            self.note_consumed_press(wid, &ev);
            return;
        }
        // Scrollback navigation (xterm / Terminal.app convention): Shift+PageUp /
        // PageDown page the viewport through history, Shift+Home / End jump to the
        // oldest line / live bottom. Intercepted here so they scroll the view instead
        // of reaching the PTY — every mainstream terminal reserves the Shift+ forms for
        // this. Plain (un-shifted) PageUp/Home/End still encode to the app below. Runs
        // BEFORE snap_to_bottom so scrolling up isn't immediately undone.
        // …but NOT while the find field owns input: ⇧Home would jump the viewport to the
        // oldest scrollback line while the panel still highlights a match somewhere else,
        // desyncing the view from the find. Unshifted Home/End reach the field below as
        // the caret motions they are in any text field.
        if let Some(intent) = scrollback_chord(mods, &ev)
            .filter(|_| self.windows.get(&wid).is_none_or(|ws| ws.search.is_none()))
        {
            if let Some(session) = self.front_terminal(wid).map(|terminal| terminal.session) {
                self.note_local_repeat_press(
                    wid,
                    ev.physical_key,
                    crate::LocalRepeatAction::Scroll { session, intent },
                );
            } else {
                self.note_consumed_press(wid, &ev);
            }
            self.input(wid, InputEvent::ScrollView(intent), Source::Human);
            return;
        }
        // Cmd-F enters find mode; while active, keystrokes drive the find (query
        // edit + match navigation) instead of reaching the PTY.
        if mods.super_key()
            && let Key::Character(s) = &ev.logical_key
            && s.eq_ignore_ascii_case("f")
        {
            self.search_enter();
            self.note_consumed_press(wid, &ev);
            return;
        }
        let search_repeat = self.search_repeat_action(wid, mods, &ev);
        let search_session = self.front_terminal(wid).map(|terminal| terminal.session);
        if self.on_key_search_mode(wid, mods, &ev) {
            self.note_press_disposition(
                wid,
                &ev,
                search_session
                    .zip(search_repeat)
                    .map(|(session, action)| crate::LocalRepeatAction::Search { session, action }),
            );
            return;
        }
        // Cmd-C -> copy the selection to the system clipboard (before the
        // snap-to-bottom: copying must neither clear the selection nor move
        // the viewport). With no selection it falls through to normal handling.
        if mods.super_key()
            && let Key::Character(s) = &ev.logical_key
            && s.eq_ignore_ascii_case("c")
            && self.copy_selection()
        {
            self.note_consumed_press(wid, &ev);
            return;
        }
        // Any key press past this point jumps the viewport back to the live view
        // if scrolled into history. The TERMINAL half of that snap is deliberately
        // NOT taken here for keys that continue into the seam: `self.input` /
        // `forward_literal_press` reach the consolidated "ONE term-lock scope for
        // every press-path terminal touch", which performs byte-for-byte the same
        // `display_offset() != 0` test under a lock it has to take anyway — a
        // `snap_to_bottom` here would cost a plain keystroke a whole extra
        // terminal-mutex acquisition, queued behind the PTY reader's `process()`
        // holds during an output flood. The arms that END the press here (Cmd-V,
        // font zoom, IME suppression, the bare-Cmd swallow, the un-encodable tail)
        // never reach the seam and so keep an explicit `snap_to_bottom`, each
        // placed relative to its own side-effects.
        //
        // The WINDOW half stays unconditional and needs NO terminal lock: M1's
        // "typing snaps INSTANTLY" cancels any in-flight wheel glide, elastic
        // overscroll bounce, and banked sub-row residual, so an eased momentum tail
        // cannot scroll the viewport back off the prompt a moment after the key
        // landed. The seam only touches the terminal, so this half must stay here.
        self.cancel_press_scroll_motion(wid);
        //
        // Cmd-V -> paste the system clipboard (bracketed when the app enabled
        // it). Pasting does not clear the selection. (The `Paste` seam arm snaps
        // separately, but this key can also be swallowed by an empty clipboard, so
        // it keeps its own snap exactly as before.)
        if mods.super_key()
            && let Key::Character(s) = &ev.logical_key
            && s.eq_ignore_ascii_case("v")
        {
            self.snap_to_bottom(wid);
            self.paste_clipboard();
            self.note_consumed_press(wid, &ev);
            return;
        }
        let zoom_repeat = font_zoom_repeat_action(mods, &ev);
        // Snapped off the SAME classifier `on_key_font_zoom` matches on, not off
        // its return value: the snap has to precede the zoom's re-grid.
        if zoom_repeat.is_some() {
            self.snap_to_bottom(wid);
        }
        if self.on_key_font_zoom(mods, &ev) {
            self.note_press_disposition(
                wid,
                &ev,
                zoom_repeat.map(crate::LocalRepeatAction::FontZoom),
            );
            return;
        }
        // IME-1: while a composition (CJK / dead key) is in flight, SUPPRESS the
        // direct key send — the keystrokes belong to the composer; the resulting
        // text arrives via `Ime::Commit` (encoded through the same engine path).
        // Without this the composing keys would ALSO emit raw bytes (double
        // input). ASCII typing with no active composition is unaffected (preedit
        // is empty), so normal keys still send below.
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| keymap::suppress_direct_send(&ws.preedit))
        {
            // A composing key never reaches the seam, so it snaps here: typing into
            // an IME composition still jumps back to the live view.
            self.snap_to_bottom(wid);
            self.note_consumed_press(wid, &ev);
            return;
        }
        // option_as_meta = false (config opt-out): Option on macOS — or Alt on a
        // platform/layout where winit supplies composed text — types that OS-composed
        // character (Option+a → "å") instead of the ESC-prefixed Meta sequence the
        // engine encoder produces by default. Only when Alt/Option is the SOLE relevant
        // modifier (no Ctrl/Super, which keep their engine encoding) and winit resolved
        // non-empty `text`; a bare Alt+arrow or an Alt chord with no text still falls
        // through to the encoder below. With the default (`option_as_meta = true`), and
        // on the no-config path, this block is skipped entirely, so the encode path is
        // byte-identical. The native control is therefore named Alt/Option, not macOS-only.
        if !self.option_as_meta
            && mods.alt_key()
            && !mods.control_key()
            && !mods.super_key()
            && let Some(text) = &ev.text
            && !text.is_empty()
        {
            self.forward_literal_press(wid, ev.physical_key, InputEvent::Text(text.to_string()));
            return;
        }
        // Any Cmd (Super) chord that reaches here was NOT claimed by a keybinding
        // or a hardcoded shortcut above, so it is an app-level chord the OS reserves
        // for the application — real macOS terminals never forward Cmd combos to the
        // PTY. Encoding it would leak a stray byte (Cmd-K → "k") or a spurious
        // `ESC[1;9D` into the shell/TUI with no beep and no escape hatch. Swallow it
        // — snapping the viewport first, since a swallowed chord never reaches the
        // seam that would otherwise do it. The text-fallback below is already
        // Super-guarded; this closes the build_key_input encode path too.
        if mods.super_key() {
            self.snap_to_bottom(wid);
            self.note_consumed_press(wid, &ev);
            return;
        }
        // Phase 0.5: BUILD an engine-neutral InputEvent and call the seam in-thread
        // (no hop, no latency cost). The seam is the sole byte-producing reader of
        // keyboard_mode() (the predictor's kitty_suppresses_predictive_echo()
        // sample is a read-only display gate) and the sole caller of the encoder +
        // reset_blink/snap_to_bottom/clear_selection — so a human key and the
        // `key`/`ctrl` verbs that build the
        // SAME (Key, mods, base_layout) triple produce byte-identical PTY output
        // (kills divergences f/h; uniform g/k side-effects). The keymap is demoted
        // to a BUILDER (`build_key_input`) that fills `base_layout` from the
        // physical key for Kitty REPORT_ALTERNATE_KEYS.
        // Caps/Num Lock are not in winit's `ModifiersState`; fold the live
        // platform lock state into the Kitty modifier byte (WIRE-MODIFIERS).
        let km_mods = keymap::modifiers_from_winit(mods) | keymap::lock_modifiers();
        if let Some((key, km_mods, base_layout)) = keymap::build_key_input(&ev, km_mods) {
            // Always a genuine PRESS: `on_key` routes every auto-repeat through
            // `route_physical_repeat` and every RELEASE through
            // `release_physical_press` at the top of this fn, so neither can
            // reach this encoder call.
            self.note_forwarded_press_key(
                wid,
                ev.physical_key,
                false,
                key.clone(),
                km_mods,
                base_layout,
            );
            self.input(
                wid,
                InputEvent::Key {
                    key,
                    mods: km_mods,
                    base_layout,
                    event_type: aterm_types::keyboard::KeyEventType::Press,
                },
                Source::Human,
            );
            return;
        }
        // IME/dead-key fallback: the keymap mapped no engine key (an unencodable
        // key, or a layout-composed character that `key_without_modifiers`
        // stripped). Honor winit's resolved `text` so a plain layout character
        // still types when no IME composition is active — but NEVER for
        // Ctrl/Alt/Super, whose ESC/control encoding the engine already owns above.
        let bare = !mods.control_key() && !mods.alt_key() && !mods.super_key();
        if let Some(text) = &ev.text
            && bare
            && !text.is_empty()
        {
            self.forward_literal_press(wid, ev.physical_key, InputEvent::Text(text.to_string()));
            return;
        }
        // No byte-producing press was observed, so a later CSI-u release has
        // no valid peer and must be swallowed even if its release-time logical
        // mapping differs. This tail is REACHED by every bare modifier press
        // (winit reports Shift/Ctrl/… as key events and `build_key_input` maps
        // them to nothing), which never reaches the seam — so it must snap here:
        // the human parity is "any key press jumps to live".
        self.snap_to_bottom(wid);
        self.note_consumed_press(wid, &ev);
    }

    /// Hardcoded Cmd-Shift chords of [`on_key`], handled FIRST among the Cmd combos
    /// because they need Shift (which the `!shift_key()` block excludes). Returns
    /// `true` when a chord fired (the caller must then return); `false` (incl. an
    /// unrecognized Cmd-Shift character) falls through to the rest of `on_key`. On a
    /// US layout Shift maps `]`/`[`/`d`/`o`/`m`/`n` to `}`/`{`/`D`/`O`/`M`/`N`, so
    /// both forms are accepted.
    fn on_key_super_shift_chord(&mut self, mods: ModifiersState, ev: &KeyEvent) -> bool {
        if mods.super_key()
            && mods.shift_key()
            && let Key::Character(s) = &ev.logical_key
        {
            match s.as_str() {
                // Cmd-Shift-] / Cmd-Shift-[ cycle to the next / previous in-window
                // TAB (wrapping).
                "]" | "}" => {
                    self.cycle_tab(true);
                    return true;
                }
                "[" | "{" => {
                    self.cycle_tab(false);
                    return true;
                }
                // Cmd-Shift-T reopens a stable descriptor with fresh runtime ids.
                "t" | "T" => {
                    let _ = self.reopen_last_closed_tab();
                    return true;
                }
                // Cmd-Shift-N "Move Tab to New Window": pull the frontmost
                // window's active tab out into a fresh in-process window.
                // `on_key` has no `ActiveEventLoop`, so post a Wake; the
                // `user_event` arm (which has `el`) runs the move + OS attach.
                "n" | "N" => {
                    if let Some(proxy) = self.proxy.as_ref() {
                        let _ = proxy.send_event(Wake::DetachActiveTab);
                    }
                    return true;
                }
                // Cmd-Shift-D: split the FOCUSED pane HORIZONTALLY (panes stacked
                // top/bottom). This is the default chord for `Action::SplitHorizontal`
                // (keybinding parity).
                "d" | "D" => {
                    self.split_focused_pane(pane::SplitDir::Horizontal);
                    return true;
                }
                // Cmd-Shift-O "Open Active Session in New Window": show the
                // frontmost window's active session in a SECOND window (same live
                // grid in two windows). `on_key` has no `ActiveEventLoop`, so post a
                // Wake; the `user_event` arm (which has `el`) runs the attach +
                // OS-window create.
                "o" | "O" => {
                    if let Some(proxy) = self.proxy.as_ref() {
                        let _ = proxy.send_event(Wake::ViewActiveSessionInNewWindow);
                    }
                    return true;
                }
                // Cmd-Shift-M "Move Tab to Next Window": move the frontmost window's
                // active tab into the NEXT existing window (wrapping). The destination
                // already exists, so there is no OS-window attach and no `el` is
                // needed — call the move directly (no Wake round-trip). A <2-window
                // app is a no-op.
                "m" | "M" => {
                    self.migrate_active_tab_to_next_window();
                    return true;
                }
                _ => {}
            }
        }
        false
    }

    /// Hardcoded Cmd (no Shift) chords of [`on_key`]: Cmd-N opens a new IN-PROCESS
    /// WINDOW (the standard macOS "new window", sharing this process's
    /// renderer/device); Cmd-T opens a new in-window TAB (a fresh shell session
    /// sharing this window); Cmd-D splits vertically; Cmd-W closes the focused pane
    /// (escalating to the window close on the LAST pane of the LAST tab); Cmd-Q is
    /// the quit FALLBACK for a declined menu key equivalent; Cmd-1..Cmd-9
    /// jump straight to that tab (1-based). Returns `true` when a chord fired.
    fn on_key_super_chord(&mut self, mods: ModifiersState, ev: &KeyEvent) -> bool {
        if mods.super_key()
            && !mods.shift_key()
            && let Key::Character(s) = &ev.logical_key
        {
            let lc = s.to_ascii_lowercase();
            match lc.as_str() {
                "n" => {
                    // Cmd-N opens a real IN-PROCESS window. `on_key` has no
                    // `ActiveEventLoop`, so post a `Wake::CreateWindow`; the
                    // `user_event` arm (which has `el`) runs the creation.
                    if let Some(proxy) = self.proxy.as_ref() {
                        let _ = proxy.send_event(Wake::CreateWindow);
                    }
                    return true;
                }
                "t" => {
                    self.open_tab();
                    return true;
                }
                // Cmd-D: split the FOCUSED pane VERTICALLY (panes side by side).
                "d" => {
                    self.split_focused_pane(pane::SplitDir::Vertical);
                    return true;
                }
                // Cmd-Q quit FALLBACK. The app-menu key equivalent (menu.rs) is
                // the primary path and consumes the keyDown before it reaches
                // on_key; this arm fires only when AppKit declines the
                // equivalent (menu not installed, item disabled by a lost weak
                // target, a future menu-system change) — without it the chord
                // dies silently in the Cmd swallow below, and Quit was the ONLY
                // common Cmd chord with no keyboard fallback. Post the same
                // Wake the menu item posts: on_quit_requested (needs the `el`
                // only user_event has) shows the same confirm dialog, so both
                // entries stay behaviorally identical. A held Cmd-Q cannot
                // queue a second confirm behind the modal: `on_key` routes
                // every repeat through `route_physical_repeat` before any
                // chord path, which replays the consumed-press disposition.
                "q" => {
                    if let Some(proxy) = self.proxy.as_ref() {
                        let _ = proxy.send_event(Wake::MenuAction {
                            action: crate::menu::MenuAction::Quit,
                        });
                    }
                    return true;
                }
                "w" => {
                    // Close the FOCUSED PANE of this (frontmost) window's active
                    // tab. A split tab's Cmd-W collapses one pane onto its sibling;
                    // the only pane of a non-last tab closes the tab in-place. The
                    // LAST pane of the LAST tab's close sets `pending_close` so
                    // `window_event` (which has the `ActiveEventLoop`) escalates to
                    // closing the WINDOW after `on_key` returns — `on_key` itself
                    // has no `el` to do so. The app exits only when that was the
                    // last window.
                    // Escalate on the window `close_active_tab` actually closed
                    // (the FRONTMOST), not the event-stamped `wid` — they can
                    // differ when the keypress was routed to a non-front window.
                    if let Some(closed) = self.close_active_tab()
                        && let Some(ws) = self.windows.get_mut(&closed)
                    {
                        ws.pending_close = true;
                    }
                    return true;
                }
                // Cmd-1..Cmd-9 → switch to that tab (1-based → 0-based index).
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" => {
                    if let Some(d) = lc.chars().next().and_then(|c| c.to_digit(10)) {
                        self.switch_tab(d as usize - 1);
                    }
                    return true;
                }
                _ => {}
            }
        }
        false
    }

    /// Pane chords (iTerm2 parity), checked together because both use `Key::Named`
    /// (so they never overlap the `Key::Character` Cmd chords). Returns `true` when
    /// one fired; each is a silent no-op for a single-pane tab / window-less app.
    ///   - Cmd-Opt-Arrow: directional pane focus.
    ///   - Cmd-Shift-Enter: toggle zoom of the focused pane.
    fn on_key_pane_focus(&mut self, mods: ModifiersState, ev: &KeyEvent) -> bool {
        // Cmd-Shift-Enter (no Alt): toggle pane zoom.
        if mods.super_key()
            && mods.shift_key()
            && !mods.alt_key()
            && matches!(&ev.logical_key, Key::Named(NamedKey::Enter))
        {
            self.toggle_pane_zoom();
            return true;
        }
        // Cmd-Opt-Arrow: directional focus.
        if !(mods.super_key() && mods.alt_key()) {
            return false;
        }
        let dir = match &ev.logical_key {
            Key::Named(NamedKey::ArrowLeft) => pane::FocusDir::Left,
            Key::Named(NamedKey::ArrowRight) => pane::FocusDir::Right,
            Key::Named(NamedKey::ArrowUp) => pane::FocusDir::Up,
            Key::Named(NamedKey::ArrowDown) => pane::FocusDir::Down,
            _ => return false,
        };
        self.focus_pane(dir);
        true
    }

    /// Classify a find-mode keystroke into the repeatable action it drives: a match
    /// step/repeat, or one text-field [`SearchEdit`] (motion, kill, or insert). The
    /// find field is a REAL single-line text field, so it answers the standard
    /// macOS/readline chords — `^A`/`Home`/`⌘←` to the start, `^E`/`End`/`⌘→` to the
    /// end, `^B`/`^F` and `←`/`→` by character, `⌥←`/`⌥→` by word, `^K`/`^U`/`^W`/
    /// `⌥⌫`/`⌘⌫` to kill, `^D`/`⌦` forward-delete — and every one of them repeats on a
    /// held key through this classification.
    ///
    /// Two deliberate exclusions: `↑`/`↓` step MATCHES (they are the non-emacs muscle
    /// memory for `^R`/`^S`, and a one-line field has no vertical motion to lose), and
    /// plain `⌥`+letter is NOT bound — macOS composes query characters there (é, ß, ƒ),
    /// so word motion lives on `⌥`+ARROW only.
    ///
    /// The trailing text arm accepts only PRINTABLE text: `ev.text` carries `"\u{1b}"`
    /// for ⎋, `"\r"` for ⏎ and `"\t"` for ⇥ (winit's `NamedKey::to_text`), and those
    /// named keys are commands here, never content — inserting their payloads would
    /// type an escape into the query instead of closing the bar.
    fn search_repeat_action(
        &self,
        wid: WindowId,
        mods: ModifiersState,
        ev: &KeyEvent,
    ) -> Option<crate::SearchRepeatAction> {
        use crate::SearchRepeatAction as Action;
        use crate::app_search::SearchEdit;

        if self.windows.get(&wid).is_none_or(|ws| ws.search.is_none()) {
            return None;
        }
        let base = base_logical_key(ev);
        let base_is =
            |want: &str| matches!(&base, Key::Character(c) if c.eq_ignore_ascii_case(want));
        let ctrl = mods.control_key() && !mods.super_key() && !mods.alt_key();
        let bare_cmd =
            mods.super_key() && !mods.shift_key() && !mods.control_key() && !mods.alt_key();
        // macOS text-field convention: ⌥ = by word, ⌘ = to the end of the line.
        let by_word = mods.alt_key() && !mods.control_key() && !mods.super_key();
        let to_edge = mods.super_key() && !mods.control_key() && !mods.alt_key();
        let edit = |e: SearchEdit| Some(Action::Edit(e));
        match &ev.logical_key {
            Key::Named(NamedKey::ArrowDown) => Some(Action::Step(true)),
            Key::Named(NamedKey::ArrowUp) => Some(Action::Step(false)),
            Key::Named(NamedKey::ArrowLeft) if to_edge => edit(SearchEdit::MoveStart),
            Key::Named(NamedKey::ArrowRight) if to_edge => edit(SearchEdit::MoveEnd),
            Key::Named(NamedKey::ArrowLeft) if by_word => edit(SearchEdit::MoveWordLeft),
            Key::Named(NamedKey::ArrowRight) if by_word => edit(SearchEdit::MoveWordRight),
            Key::Named(NamedKey::ArrowLeft) => edit(SearchEdit::MoveCharLeft),
            Key::Named(NamedKey::ArrowRight) => edit(SearchEdit::MoveCharRight),
            Key::Named(NamedKey::Home) => edit(SearchEdit::MoveStart),
            Key::Named(NamedKey::End) => edit(SearchEdit::MoveEnd),
            Key::Named(NamedKey::Backspace) if to_edge => edit(SearchEdit::KillToStart),
            Key::Named(NamedKey::Backspace) if by_word => edit(SearchEdit::DeleteWordBack),
            Key::Named(NamedKey::Backspace) => edit(SearchEdit::DeleteBack),
            Key::Named(NamedKey::Delete) if by_word => edit(SearchEdit::DeleteWordForward),
            Key::Named(NamedKey::Delete) => edit(SearchEdit::DeleteForward),
            // Match navigation keeps first claim on ^S/^R (emacs isearch-repeat).
            _ if ctrl && base_is("s") => Some(Action::Repeat(true)),
            _ if ctrl && base_is("r") => Some(Action::Repeat(false)),
            _ if bare_cmd && base_is("s") => Some(Action::Repeat(true)),
            _ if bare_cmd && base_is("r") => Some(Action::Repeat(false)),
            _ if ctrl && base_is("a") => edit(SearchEdit::MoveStart),
            _ if ctrl && base_is("e") => edit(SearchEdit::MoveEnd),
            _ if ctrl && base_is("b") => edit(SearchEdit::MoveCharLeft),
            _ if ctrl && base_is("f") => edit(SearchEdit::MoveCharRight),
            _ if ctrl && base_is("d") => edit(SearchEdit::DeleteForward),
            _ if ctrl && base_is("h") => edit(SearchEdit::DeleteBack),
            _ if ctrl && base_is("k") => edit(SearchEdit::KillToEnd),
            _ if ctrl && base_is("u") => edit(SearchEdit::KillToStart),
            _ if ctrl && base_is("w") => edit(SearchEdit::DeleteWordBack),
            _ if !mods.super_key() && !mods.control_key() => ev
                .text
                .as_ref()
                .map(|text| -> String { text.chars().filter(|c| !c.is_control()).collect() })
                .filter(|text| !text.is_empty())
                .map(|text| Action::Edit(SearchEdit::Insert(text))),
            _ => None,
        }
    }

    /// Open a directed terminal search or repeat an already-open one, then bind the
    /// complete physical hold to that local action. Repeats never re-run live shortcut
    /// routing, and release never reaches the Kitty encoder.
    fn terminal_emacs_search_pressed(
        &mut self,
        wid: WindowId,
        physical_key: winit::keyboard::PhysicalKey,
        forward: bool,
    ) {
        let session = self.front_terminal(wid).map(|terminal| terminal.session);
        if self
            .windows
            .get(&wid)
            .is_some_and(|window| window.search.is_some())
        {
            self.search_repeat_in(wid, forward);
        } else {
            self.search_enter_direction_in(wid, forward);
        }
        if let Some(session) = session {
            self.note_local_repeat_press(
                wid,
                physical_key,
                crate::LocalRepeatAction::Search {
                    session,
                    action: crate::SearchRepeatAction::Repeat(forward),
                },
            );
        } else {
            self.note_consumed_press_key(wid, physical_key, false);
        }
    }

    fn apply_search_repeat_action(&mut self, wid: WindowId, action: crate::SearchRepeatAction) {
        match action {
            crate::SearchRepeatAction::Step(forward) => self.search_step_in(wid, forward),
            crate::SearchRepeatAction::Repeat(forward) => self.search_repeat_in(wid, forward),
            crate::SearchRepeatAction::Edit(edit) => self.search_edit_in(wid, edit),
        }
    }

    /// Find-mode keystroke dispatch of [`on_key`]: while a window's `search` is
    /// active, keystrokes drive the find (query edit + match navigation) instead of
    /// reaching the PTY. Returns `true` whenever search is active (so the caller
    /// returns — matching the inline block's unconditional `return`); `false` when no
    /// search is in flight.
    ///
    /// The keymap is EMACS-ISEARCH shaped, so find doubles as fast navigation through
    /// a big buffer: `⌘S`/`⌘R` and `^S`/`^R` step to the next/previous match
    /// (on an empty query they
    /// RECALL the last accepted search — the `C-s C-s` idiom), `⏎` ACCEPTS (exit,
    /// staying at the match, highlight kept), `⎋`/`^G` CANCEL (exit, restoring the
    /// pre-find viewport), `↓`/`↑` mirror `^S`/`^R` for non-emacs muscle memory. The
    /// case/regex toggles live on `⌥⌘C`/`⌥⌘R` — NOT plain ⌥ chords, which must stay
    /// free for Option-composed query characters (é, ß, …) on macOS.
    ///
    /// Everything else about the query is TEXT-FIELD editing, classified (and made
    /// key-repeatable) by [`Self::search_repeat_action`]: caret motion, word/line kills,
    /// and printable insertion. `⌘V` pastes the clipboard into the FIELD (never the
    /// PTY). ⎋ and ⏎ are commands here, so they must not survive as inserted text —
    /// see the control-character filter on that classifier's text arm.
    fn on_key_search_mode(&mut self, wid: WindowId, mods: ModifiersState, ev: &KeyEvent) -> bool {
        if self.windows.get(&wid).is_none_or(|ws| ws.search.is_none()) {
            return false;
        }
        // A live IME composition owns its keystrokes: the composed result arrives once,
        // as `Ime::Commit` (which `on_ime_commit` routes into the field). Inserting the
        // composing keys here as well would type every dead-key press AND its result.
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| !ws.preedit.is_empty())
        {
            return true;
        }
        // Chords match on the modifier-independent BASE key — macOS ⌥ composes a
        // different glyph into `logical_key` (⌥⌘C arrives as "ç"), exactly the drift
        // `base_logical_key` exists to undo for the rebindable-chord path.
        let base = base_logical_key(ev);
        let base_is =
            |want: &str| matches!(&base, Key::Character(c) if c.eq_ignore_ascii_case(want));
        let ctrl = mods.control_key() && !mods.super_key() && !mods.alt_key();
        let cmd_alt = mods.super_key() && mods.alt_key() && !mods.control_key();
        let bare_cmd =
            mods.super_key() && !mods.shift_key() && !mods.control_key() && !mods.alt_key();
        // ⌘C is NOT the field's: `search_accept`'s contract is that the current match is
        // left selected "ready for ⌘C", which is a lie if the find bar swallows it. Fall
        // through to the terminal's copy so the match can be copied WITHOUT closing find.
        if bare_cmd && base_is("c") {
            return false;
        }
        if let Some(action) = self.search_repeat_action(wid, mods, ev) {
            self.apply_search_repeat_action(wid, action);
            return true;
        }
        match &ev.logical_key {
            Key::Named(NamedKey::Escape) => self.search_cancel_in(wid),
            // ⏎ accepts (emacs RET): leave find mode, STAYING at the current match.
            Key::Named(NamedKey::Enter) => self.search_accept_in(wid),
            // ^G: the emacs abort chord — cancel, restoring the pre-find viewport.
            _ if ctrl && base_is("g") => self.search_cancel_in(wid),
            // ⌘V pastes into the FIELD (one line, control bytes stripped) — the find
            // bar owns the keystroke, so nothing reaches the shell behind it.
            _ if bare_cmd && base_is("v") => self.search_paste_in(wid),
            // ⌥⌘C / ⌥⌘R: match-case / regex toggles (remembered app-sticky for the next
            // find, and shared with the clickable `[Aa]`/`[.*]` indicators via
            // `search_toggle_*`).
            _ if cmd_alt && base_is("c") => self.search_toggle_case_in(wid),
            _ if cmd_alt && base_is("r") => self.search_toggle_regex_in(wid),
            _ => {}
        }
        true
    }

    /// VI-1: toggle keyboard copy-mode on window `wid`'s terminal. On entry the vi cursor
    /// starts at the live terminal cursor; on exit it clears. Resets the two-key pending
    /// state and repaints (the vi-cursor render override keys off `vi_is_active`).
    pub(crate) fn toggle_vi_mode(&mut self, wid: WindowId) {
        let Some(term) = self
            .front_terminal(wid)
            .map(|terminal| terminal.term.clone())
        else {
            return;
        };
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        ws.vi_pending_g = false;
        ws.vi_pending_inline = None;
        // Read the resulting state back under the SAME lock and mirror it: the two
        // per-keystroke vi gates answer from `vi_any_active()` and never touch the
        // terminal while copy-mode is off (see `VI_ACTIVE_TERMINALS`).
        let now_active = {
            let mut t = term_lock(&term);
            t.vi_toggle();
            t.vi_is_active()
        };
        vi_note_toggled(now_active);
        if let Some(w) = ws.os_window.as_ref() {
            w.request_redraw();
        }
    }

    /// The WINDOW half of the press-path viewport snap: cancel any in-flight wheel
    /// glide, elastic-overscroll bounce, and banked sub-row residual (M1/M1b), so a
    /// momentum tail cannot ease the viewport back off the prompt just after a key
    /// landed. Split out of [`App::snap_to_bottom`] because the TERMINAL half
    /// (`display_offset` → bottom) is done by the seam's consolidated term-lock
    /// scope for every key that reaches it — this half needs no lock at all, and a
    /// press must never pay a second contended acquisition for it. The arms that
    /// end a press before the seam still call the full `snap_to_bottom`.
    fn cancel_press_scroll_motion(&mut self, wid: WindowId) {
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.scroll_glide = None;
            ws.overscroll = None;
            ws.scroll_frac_px = 0;
        }
    }

    /// Repaint after a vi keystroke (the vi-cursor override re-reads the position).
    fn vi_after_key(&self, wid: WindowId) {
        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            w.request_redraw();
        }
    }

    fn vi_repeat_action(
        &self,
        wid: WindowId,
        mods: ModifiersState,
        ev: &KeyEvent,
    ) -> Option<(u64, crate::vi_keys::ViAction)> {
        // Copy-mode is off for every terminal ⇒ no key can be a vi repeat. Answered
        // from the GUI-side mirror so the common keystroke takes NO terminal lock
        // here (which would queue behind the PTY reader during a flood).
        if !vi_any_active() {
            return None;
        }
        let terminal = self.front_terminal(wid)?;
        if !term_lock(&terminal.term).vi_is_active() {
            return None;
        }
        let ws = self.windows.get(&wid)?;
        if ws.vi_pending_g || ws.vi_pending_inline.is_some() {
            return None;
        }
        let action = crate::vi_keys::key_to_vi_action(&ev.logical_key, mods)?;
        matches!(
            action,
            crate::vi_keys::ViAction::Motion(_) | crate::vi_keys::ViAction::RepeatInline { .. }
        )
        .then_some((terminal.session, action))
    }

    /// VI-1: while vi (keyboard copy-mode) is active on `wid`, drive the vi engine from
    /// the keyboard and SWALLOW the key (return `true`) so `h`/`j`/`k`/`l` etc. never
    /// reach the PTY. `false` when vi is inactive (keys flow normally). Two-key sequences
    /// (`g` prefix, and `f`/`F`/`t`/`T` awaiting a target char) use the per-window pending
    /// state. Called from `on_key` right after the keybinding block, so `toggle_vi_mode`'s
    /// own chord is handled before this gate ever sees a key.
    fn on_key_vi_mode(&mut self, wid: WindowId, mods: ModifiersState, ev: &KeyEvent) -> bool {
        // The SECOND of the two per-keystroke `vi_is_active` gates (this one also
        // clones the terminal `Arc`). Same mirror, same reason: with copy-mode off
        // the gate must cost nothing, least of all a mutex shared with the PTY
        // reader's output bursts.
        if !vi_any_active() {
            return false;
        }
        let Some(term) = self
            .front_terminal(wid)
            .map(|terminal| terminal.term.clone())
        else {
            return false;
        };
        if !term_lock(&term).vi_is_active() {
            return false;
        }
        let Some(ws) = self.windows.get(&wid) else {
            return false;
        };
        let (pending_g, pending_inline) = (ws.vi_pending_g, ws.vi_pending_inline);
        // The character this key produced (for `f{char}` targets and `g{e/E}`).
        let key_char = match &ev.logical_key {
            Key::Character(s) => s.chars().next(),
            _ => None,
        };
        // 1) Complete a pending `f`/`F`/`t`/`T`: this key is the search target.
        if let Some(kind) = pending_inline {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.vi_pending_inline = None;
            }
            if let Some(c) = key_char {
                term_lock(&term).vi_inline_search_execute(c, kind);
            }
            self.vi_after_key(wid);
            return true;
        }
        // 2) Complete a pending `g` prefix: `ge`/`gE` (anything else cancels).
        if pending_g {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.vi_pending_g = false;
            }
            if let Some(m) = key_char.and_then(crate::vi_keys::g_prefix_motion) {
                term_lock(&term).vi_motion(m, aterm_core::ViBoundary::Grid);
            }
            self.vi_after_key(wid);
            return true;
        }
        // 3) A fresh vi keystroke.
        let Some(action) = crate::vi_keys::key_to_vi_action(&ev.logical_key, mods) else {
            // Unmapped BARE key in vi mode: swallow it (never leak `x`, digits, … to the
            // PTY while navigating). A MODIFIED key (Ctrl/Alt/Super) mapped to `None` so
            // the app's own chords still work → let it flow.
            return !(mods.control_key() || mods.alt_key() || mods.super_key());
        };
        use crate::vi_keys::ViAction;
        match action {
            ViAction::Motion(m) => {
                term_lock(&term).vi_motion(m, aterm_core::ViBoundary::Grid);
            }
            ViAction::BeginInline(kind) => {
                if let Some(ws) = self.windows.get_mut(&wid) {
                    ws.vi_pending_inline = Some(kind);
                }
            }
            ViAction::RepeatInline { reverse } => {
                let mut terminal = term_lock(&term);
                if reverse {
                    terminal.vi_inline_search_repeat_reverse();
                } else {
                    terminal.vi_inline_search_repeat();
                }
            }
            ViAction::ToggleVisual(vt) => {
                term_lock(&term).vi_mode_mut().toggle_visual(vt);
            }
            ViAction::GPrefix => {
                if let Some(ws) = self.windows.get_mut(&wid) {
                    ws.vi_pending_g = true;
                }
            }
            ViAction::Exit => {
                // exit copy-mode — mirrored exactly like `toggle_vi_mode`'s toggle
                // (the mirror must see BOTH writers or the gate could stay latched
                // open, or worse, shut while a terminal is still in vi).
                let now_active = {
                    let mut t = term_lock(&term);
                    t.vi_toggle();
                    t.vi_is_active()
                };
                vi_note_toggled(now_active);
            }
        }
        self.vi_after_key(wid);
        true
    }

    fn palette_repeat_event(&self, wid: WindowId, ev: &KeyEvent) -> Option<InputEvent> {
        use aterm_types::keyboard::{Key as TKey, KeyEventType, Modifiers, NamedKey as TNamed};

        if self
            .windows
            .get(&wid)
            .and_then(|ws| ws.overlay.as_ref())
            .map(|overlay| overlay.kind())
            != Some(crate::overlay::OverlayKind::Palette)
        {
            return None;
        }
        let named = |key| InputEvent::Key {
            key: TKey::Named(key),
            mods: Modifiers::empty(),
            base_layout: None,
            event_type: KeyEventType::Repeat,
        };
        match &ev.logical_key {
            Key::Named(NamedKey::ArrowUp) => Some(named(TNamed::ArrowUp)),
            Key::Named(NamedKey::ArrowDown) => Some(named(TNamed::ArrowDown)),
            Key::Named(NamedKey::Backspace) => Some(named(TNamed::Backspace)),
            Key::Named(NamedKey::Space) => Some(InputEvent::Text(" ".to_string())),
            Key::Character(text) => Some(InputEvent::Text(text.to_string())),
            Key::Named(NamedKey::Escape | NamedKey::Enter) => None,
            _ => ev
                .text
                .as_ref()
                .filter(|text| !text.is_empty())
                .map(|text| InputEvent::Text(text.to_string())),
        }
    }

    /// The SINGLE modal-overlay key gate: if an overlay is open on `wid`, delegate to the
    /// ACTIVE variant's handler and SWALLOW the key (return `true`); closed ⇒ `false` (keys
    /// flow normally). Dispatch is by the one live variant, so there is no gate ORDERING:
    /// one slot holds one surface, and a hidden surface can never swallow keys.
    fn on_key_overlay_mode(&mut self, wid: WindowId, _mods: ModifiersState, ev: &KeyEvent) -> bool {
        use crate::overlay::OverlayKind;
        match self
            .windows
            .get(&wid)
            .and_then(|ws| ws.overlay.as_ref())
            .map(|o| o.kind())
        {
            Some(OverlayKind::Palette) => self.on_key_palette_mode(wid, ev),
            #[cfg(test)]
            Some(OverlayKind::About) => self.on_key_about_mode(wid, ev),
            #[cfg(test)]
            Some(OverlayKind::Update) => self.on_key_update_mode(wid, ev),
            #[cfg(test)]
            Some(OverlayKind::Settings) => self.on_key_settings_mode(wid, _mods, ev),
            None => false,
        }
    }

    /// While the Settings overlay is open on `wid`, drive it from the keyboard and
    /// SWALLOW every key (return `true`) so nothing reaches the PTY (design §6):
    /// Tab/⇧Tab toggle the sidebar/content panes; sidebar ↑/↓ move the category and
    /// →/↵ focus content; content ↑/↓ move the selection, ←/→ adjust in place (←
    /// NEVER leaves the pane — Tab is the switcher), ↵/Space activates, ⌫ resets.
    /// Esc walks one level per press: colour wheel > open menu > text edit >
    /// search query > content→sidebar > close. Closed ⇒ `false` (keys flow
    /// normally). Mirrors [`on_key_search_mode`]; keep IDENTICAL to the controller
    /// twin [`Self::settings_input_event`].
    #[cfg(test)]
    fn on_key_settings_mode(&mut self, wid: WindowId, mods: ModifiersState, ev: &KeyEvent) -> bool {
        let (editing, searching, has_query, menu_open, wheel_open, in_sidebar, landing) =
            match self.windows.get(&wid).and_then(|ws| ws.settings()) {
                Some(s) => (
                    s.editing.is_some(),
                    s.searching,
                    !s.query.trim().is_empty(),
                    s.menu.is_some(),
                    s.wheel.is_some(),
                    s.pane == crate::settings::SettingsPane::Sidebar && !s.filtering(),
                    s.landing,
                ),
                None => return false, // panel closed → keys flow normally
            };
        if landing {
            // The §L landing page owns the keys: typing edits the suggestion
            // box, ↵ sends a non-empty box (else Get started), Tab/↓ skip
            // straight to the panel, Esc closes. No popover/edit state can be
            // open while the hero is up. Keep IDENTICAL to the controller twin
            // [`Self::settings_input_event`].
            match &ev.logical_key {
                Key::Named(NamedKey::Escape) => self.settings_exit(),
                Key::Named(NamedKey::Enter) => self.settings_landing_confirm(),
                Key::Named(NamedKey::Backspace) => self.settings_comment_backspace(),
                Key::Named(NamedKey::Tab | NamedKey::ArrowDown) => {
                    self.settings_landing_get_started();
                }
                _ => {
                    if let Some(t) = &ev.text {
                        for ch in t.chars().filter(|c| !c.is_control()) {
                            self.settings_comment_push(ch);
                        }
                    }
                }
            }
            return true;
        }
        if wheel_open {
            // The colour wheel owns the keys (its Esc level precedes every other):
            // Tab cycles Wheel→Value→Hex, arrows scrub the focused control (Shift =
            // coarse), typed hex digits edit the readout (no-ops off the hex field),
            // ↵ commits ONCE through the shared seam, Esc discards.
            match &ev.logical_key {
                Key::Named(NamedKey::Escape) => self.settings_wheel_cancel(),
                Key::Named(NamedKey::Enter) => self.settings_wheel_commit(),
                Key::Named(NamedKey::Tab) => self.settings_wheel_focus_next(),
                Key::Named(NamedKey::ArrowLeft) => {
                    self.settings_wheel_arrow(-1.0, 0.0, mods.shift_key());
                }
                Key::Named(NamedKey::ArrowRight) => {
                    self.settings_wheel_arrow(1.0, 0.0, mods.shift_key());
                }
                Key::Named(NamedKey::ArrowUp) => {
                    self.settings_wheel_arrow(0.0, 1.0, mods.shift_key());
                }
                Key::Named(NamedKey::ArrowDown) => {
                    self.settings_wheel_arrow(0.0, -1.0, mods.shift_key());
                }
                Key::Named(NamedKey::Backspace) => self.settings_wheel_hex_backspace(),
                _ => {
                    if let Some(t) = &ev.text {
                        for ch in t.chars().filter(|c| !c.is_control()) {
                            self.settings_wheel_hex_push(ch);
                        }
                    }
                }
            }
            return true;
        }
        if menu_open {
            // The popup menu owns the keys: ↑/↓ move the highlight (clamped), Enter/
            // Space commit it, Esc closes with NO change (the menu Esc level precedes
            // the filter/close levels), and a typed letter jumps to the next matching
            // option.
            match &ev.logical_key {
                Key::Named(NamedKey::Escape) => self.settings_menu_cancel(),
                Key::Named(NamedKey::ArrowUp) => self.settings_menu_move(-1),
                Key::Named(NamedKey::ArrowDown) => self.settings_menu_move(1),
                Key::Named(NamedKey::Enter | NamedKey::Space) => self.settings_menu_commit(),
                _ => {
                    if ev.text.as_deref() == Some(" ") {
                        self.settings_menu_commit();
                    } else if let Some(c) = ev
                        .text
                        .as_deref()
                        .and_then(|t| t.chars().next())
                        .filter(|c| c.is_alphanumeric())
                    {
                        self.settings_menu_jump(c);
                    }
                }
            }
            return true;
        }
        if editing {
            // In the in-panel text editor: keys edit the buffer, not the selection.
            match &ev.logical_key {
                Key::Named(NamedKey::Escape) => self.settings_edit_cancel(),
                Key::Named(NamedKey::Enter) => self.settings_edit_commit(),
                Key::Named(NamedKey::Backspace) => self.settings_edit_backspace(),
                // Everything printable (incl. Space, which arrives as text) is inserted;
                // control chars (arrows, fn keys) are swallowed without effect.
                _ => {
                    if let Some(t) = &ev.text {
                        for ch in t.chars().filter(|c| !c.is_control()) {
                            self.settings_edit_push(ch);
                        }
                    }
                }
            }
            return true;
        }
        if searching {
            // Search-bar focus: typing filters; Enter/↓ drops into the list; Esc clears.
            match &ev.logical_key {
                Key::Named(NamedKey::Escape) => self.settings_search_clear(),
                Key::Named(NamedKey::Enter | NamedKey::ArrowDown) => self.settings_search_confirm(),
                Key::Named(NamedKey::Backspace) => self.settings_search_backspace(),
                _ => {
                    if let Some(t) = &ev.text {
                        for ch in t.chars().filter(|c| !c.is_control()) {
                            self.settings_search_push(ch);
                        }
                    }
                }
            }
            return true;
        }
        match &ev.logical_key {
            // Esc, one level per press: clear an active filter, step content back to
            // the sidebar, and only from the sidebar close the panel/window.
            Key::Named(NamedKey::Escape) => {
                if has_query {
                    self.settings_search_clear();
                } else if !in_sidebar {
                    self.settings_focus_sidebar();
                } else {
                    self.settings_exit();
                }
            }
            // Tab / ⇧Tab toggle the two panes (macOS full-keyboard-access order).
            Key::Named(NamedKey::Tab) => self.settings_toggle_pane(),
            Key::Named(NamedKey::ArrowUp) => self.settings_move(-1),
            Key::Named(NamedKey::ArrowDown) => self.settings_move(1),
            // ← adjusts IN PLACE and NEVER leaves the content pane (Tab switches
            // panes, so adjust and navigate can't collide); in the sidebar it idles.
            Key::Named(NamedKey::ArrowLeft) => {
                if !in_sidebar {
                    self.settings_step(-1, mods.shift_key());
                }
            }
            // → from the sidebar drops focus into the content pane; in content it
            // adjusts in place (Shift = ×10), each press persisting (design §6).
            Key::Named(NamedKey::ArrowRight) => {
                if in_sidebar {
                    self.settings_focus_content();
                } else {
                    self.settings_step(1, mods.shift_key());
                }
            }
            // ↵/Space: sidebar → focus content; content → activate (toggle / cycle /
            // open the popup menu / open the free-form editor).
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                if in_sidebar {
                    self.settings_focus_content();
                } else {
                    self.settings_activate();
                }
            }
            // `/` focuses the search bar from either pane; ⌘F is its macOS-
            // conventional twin (design §4.4) — without this arm the chord is a
            // dead key under the swallow-everything overlay gate. (On macOS the
            // Edit ▸ Find… key equivalent usually intercepts ⌘F ahead of keyDown
            // and lands in `find_requested`, which diverts here too.)
            Key::Character(s) if s.as_str() == "/" => self.settings_search_begin(),
            Key::Character(s) if mods.super_key() && s.eq_ignore_ascii_case("f") => {
                self.settings_search_begin();
            }
            // Reset the focused row to its built-in default: Del, Cmd-Backspace, or —
            // in the grouped content pane, where the focused control is on glass —
            // plain ⌫ (design §6). The flat filtered list keeps Cmd-⌫ only.
            Key::Named(NamedKey::Delete) => self.settings_reset_selected(),
            Key::Named(NamedKey::Backspace) if mods.super_key() || (!in_sidebar && !has_query) => {
                self.settings_reset_selected();
            }
            _ => {
                if ev.text.as_deref() == Some(" ") {
                    if in_sidebar {
                        self.settings_focus_content();
                    } else {
                        self.settings_activate();
                    }
                } else if ev.text.as_deref() == Some("/") {
                    self.settings_search_begin();
                }
            }
        }
        true
    }

    /// Drive the settings overlay from an ENGINE-NEUTRAL [`InputEvent`] — the convergence
    /// seam ([`Self::input`]) reached by CONTROLLER `key`/`text`/`paste` verbs, which
    /// bypass the winit `on_key` path. Mirrors [`Self::on_key_settings_mode`] so a
    /// Controller navigates + edits the panel EXACTLY as a Human does (completing
    /// introspection CONTROL of the overlay — `aterm-ctl settings open` then `key Down` /
    /// `key Enter` / `text …` work). The caller still swallows the event from the PTY.
    #[cfg(test)]
    fn settings_input_event(&mut self, wid: WindowId, ev: &InputEvent) {
        use aterm_types::keyboard::{
            Key as TKey, KeyEventType, Modifiers as TMods, NamedKey as TNamed,
        };
        let (editing, searching, has_query, menu_open, wheel_open, in_sidebar, landing) =
            match self.windows.get(&wid).and_then(|ws| ws.settings()) {
                Some(s) => (
                    s.editing.is_some(),
                    s.searching,
                    !s.query.trim().is_empty(),
                    s.menu.is_some(),
                    s.wheel.is_some(),
                    s.pane == crate::settings::SettingsPane::Sidebar && !s.filtering(),
                    s.landing,
                ),
                None => return,
            };
        match ev {
            InputEvent::Key {
                key,
                mods,
                event_type,
                ..
            } => {
                // A key RELEASE is not a command (matches the press-driven winit path).
                if matches!(event_type, KeyEventType::Release) {
                    return;
                }
                if landing {
                    // The §L landing page owns the keys — mirrors the winit
                    // branch exactly (typing edits the suggestion box, ↵
                    // sends-or-starts, Tab/↓ skip to the panel, Esc closes).
                    match key {
                        TKey::Named(TNamed::Escape) => self.settings_exit(),
                        TKey::Named(TNamed::Enter) => self.settings_landing_confirm(),
                        TKey::Named(TNamed::Backspace) => self.settings_comment_backspace(),
                        TKey::Named(TNamed::Tab | TNamed::ArrowDown) => {
                            self.settings_landing_get_started();
                        }
                        TKey::Named(TNamed::Space) => self.settings_comment_push(' '),
                        TKey::Character(c) if !c.is_control() => self.settings_comment_push(*c),
                        _ => {}
                    }
                    return;
                }
                if wheel_open {
                    // The colour wheel owns the keys (Esc precedence: wheel > menu >
                    // edit > search > panes > close) — mirrors the winit branch.
                    match key {
                        TKey::Named(TNamed::Escape) => self.settings_wheel_cancel(),
                        TKey::Named(TNamed::Enter) => self.settings_wheel_commit(),
                        TKey::Named(TNamed::Tab) => self.settings_wheel_focus_next(),
                        TKey::Named(TNamed::ArrowLeft) => {
                            self.settings_wheel_arrow(-1.0, 0.0, mods.contains(TMods::SHIFT));
                        }
                        TKey::Named(TNamed::ArrowRight) => {
                            self.settings_wheel_arrow(1.0, 0.0, mods.contains(TMods::SHIFT));
                        }
                        TKey::Named(TNamed::ArrowUp) => {
                            self.settings_wheel_arrow(0.0, 1.0, mods.contains(TMods::SHIFT));
                        }
                        TKey::Named(TNamed::ArrowDown) => {
                            self.settings_wheel_arrow(0.0, -1.0, mods.contains(TMods::SHIFT));
                        }
                        TKey::Named(TNamed::Backspace) => self.settings_wheel_hex_backspace(),
                        TKey::Character(c) if !c.is_control() => {
                            self.settings_wheel_hex_push(*c);
                        }
                        _ => {}
                    }
                    return;
                }
                if menu_open {
                    // The popup menu owns the keys (Esc precedence: menu > filter >
                    // close) — mirrors the winit menu branch exactly.
                    match key {
                        TKey::Named(TNamed::Escape) => self.settings_menu_cancel(),
                        TKey::Named(TNamed::ArrowUp) => self.settings_menu_move(-1),
                        TKey::Named(TNamed::ArrowDown) => self.settings_menu_move(1),
                        TKey::Named(TNamed::Enter | TNamed::Space) => self.settings_menu_commit(),
                        TKey::Character(' ') => self.settings_menu_commit(),
                        TKey::Character(c) if c.is_alphanumeric() => self.settings_menu_jump(*c),
                        _ => {}
                    }
                    return;
                }
                if editing {
                    match key {
                        TKey::Named(TNamed::Escape) => self.settings_edit_cancel(),
                        TKey::Named(TNamed::Enter) => self.settings_edit_commit(),
                        TKey::Named(TNamed::Backspace) => self.settings_edit_backspace(),
                        TKey::Named(TNamed::Space) => self.settings_edit_push(' '),
                        TKey::Character(c) if !c.is_control() => self.settings_edit_push(*c),
                        _ => {}
                    }
                } else if searching {
                    match key {
                        TKey::Named(TNamed::Escape) => self.settings_search_clear(),
                        TKey::Named(TNamed::Enter | TNamed::ArrowDown) => {
                            self.settings_search_confirm()
                        }
                        TKey::Named(TNamed::Backspace) => self.settings_search_backspace(),
                        TKey::Named(TNamed::Space) => self.settings_search_push(' '),
                        TKey::Character(c) if !c.is_control() => self.settings_search_push(*c),
                        _ => {}
                    }
                } else {
                    // Nav mode — keep IDENTICAL to the winit branch above.
                    match key {
                        TKey::Named(TNamed::Escape) => {
                            if has_query {
                                self.settings_search_clear();
                            } else if !in_sidebar {
                                self.settings_focus_sidebar();
                            } else {
                                self.settings_exit();
                            }
                        }
                        TKey::Named(TNamed::Tab) => self.settings_toggle_pane(),
                        TKey::Named(TNamed::ArrowUp) => self.settings_move(-1),
                        TKey::Named(TNamed::ArrowDown) => self.settings_move(1),
                        // ← never leaves content; in the sidebar it idles
                        // (design §6 — falls to the no-op catch-all).
                        TKey::Named(TNamed::ArrowLeft) if !in_sidebar => {
                            self.settings_step(-1, mods.contains(TMods::SHIFT));
                        }
                        TKey::Named(TNamed::ArrowRight) => {
                            if in_sidebar {
                                self.settings_focus_content();
                            } else {
                                self.settings_step(1, mods.contains(TMods::SHIFT));
                            }
                        }
                        TKey::Named(TNamed::Enter | TNamed::Space) => {
                            if in_sidebar {
                                self.settings_focus_content();
                            } else {
                                self.settings_activate();
                            }
                        }
                        TKey::Named(TNamed::Delete) => self.settings_reset_selected(),
                        // Plain ⌫ resets only in the grouped content pane (the winit
                        // branch's Cmd-⌫ arrives as a shortcut, not through here).
                        TKey::Named(TNamed::Backspace) if !in_sidebar && !has_query => {
                            self.settings_reset_selected();
                        }
                        TKey::Character('/') => self.settings_search_begin(),
                        // ⌘F focuses the search exactly like `/` (design §4.4) —
                        // keep IDENTICAL to the winit branch above.
                        TKey::Character('f' | 'F') if mods.contains(TMods::SUPER) => {
                            self.settings_search_begin();
                        }
                        TKey::Character(' ') => {
                            if in_sidebar {
                                self.settings_focus_content();
                            } else {
                                self.settings_activate();
                            }
                        }
                        _ => {}
                    }
                }
            }
            // Typed text / paste feeds the in-panel editor or the search box; a lone space
            // activates in nav (or commits the open menu; a letter jumps its highlight),
            // a lone `/` opens search. With the colour wheel up, text feeds its hex
            // readout (no-ops off the hex field).
            InputEvent::Text(t) | InputEvent::Paste(t) => {
                if wheel_open {
                    for ch in t.chars().filter(|c| !c.is_control()) {
                        self.settings_wheel_hex_push(ch);
                    }
                } else if menu_open {
                    if t == " " {
                        self.settings_menu_commit();
                    } else if let Some(c) = t.chars().next().filter(|c| c.is_alphanumeric()) {
                        self.settings_menu_jump(c);
                    }
                } else if editing {
                    for ch in t.chars().filter(|c| !c.is_control()) {
                        self.settings_edit_push(ch);
                    }
                } else if searching {
                    for ch in t.chars().filter(|c| !c.is_control()) {
                        self.settings_search_push(ch);
                    }
                } else if t == " " {
                    if in_sidebar {
                        self.settings_focus_content();
                    } else {
                        self.settings_activate();
                    }
                } else if t == "/" {
                    self.settings_search_begin();
                }
            }
            // The wheel scrolls the open menu's option window, else the body band —
            // the human `on_mouse_wheel` path converges HERE too (the modal gate in
            // [`Self::input`] routes a swallowed Wheel to this handler), so mouse and
            // controller wheel scrolling are one code path.
            InputEvent::Wheel { dir_up, lines, .. } => {
                let delta = if *dir_up {
                    -(*lines as isize)
                } else {
                    *lines as isize
                };
                if wheel_open {
                    // Swallowed: scrolling the band under the anchored colour wheel
                    // would detach the popover from its row mid-scrub.
                } else if menu_open {
                    self.settings_menu_scroll(delta);
                } else {
                    self.settings_scroll_body(delta);
                }
            }
            _ => {}
        }
    }

    /// Cmd-= / Cmd-+ / Cmd-- / Cmd-0 live font zoom (grow / shrink / reset) of
    /// [`on_key`]. Returns `true` when a zoom chord fired.
    fn on_key_font_zoom(&mut self, mods: ModifiersState, ev: &KeyEvent) -> bool {
        match font_zoom_repeat_action(mods, ev) {
            Some(crate::FontZoomRepeatAction::Increase) => {
                self.set_font_px(self.font_px + FONT_ZOOM_STEP);
            }
            Some(crate::FontZoomRepeatAction::Decrease) => {
                self.set_font_px(self.font_px - FONT_ZOOM_STEP);
            }
            Some(crate::FontZoomRepeatAction::Reset) => {
                self.set_font_px(self.default_font_px);
            }
            None => return false,
        }
        true
    }

    /// Run a user-bound [`keybinding::Action`] — the configurable trigger for an
    /// existing hardcoded `on_key` command. Each arm calls the SAME method the
    /// built-in key calls, so a binding does exactly what the default did (no new
    /// behavior, just a configurable chord). Keybindings are GLOBAL but dispatch is
    /// routed with the originating window `wid`; Cmd-W's close result sets that
    /// window's per-window `pending_close` exactly as the hardcoded path does.
    pub(crate) fn dispatch_action(&mut self, wid: WindowId, action: keybinding::Action) {
        use keybinding::Action;
        // A CONSUMED keybinding action is app-level intent, not PTY bytes.
        // While an update overlap is pending, every action here revokes it:
        // structural actions (tabs/windows) change the proven topology, visual
        // actions (font zoom, scroll chords) change presentation state the
        // carried frame would contradict, and paste can straddle a bracketed
        // mode flip. Plain typing never reaches this seam — it flows through
        // `input_to_session`, whose per-event policy tolerates it.
        self.note_update_handoff_activity();
        match action {
            Action::NewTab => self.open_tab(),
            Action::ReopenClosedTab => {
                let _ = self.reopen_last_closed_tab();
            }
            Action::CloseTab => {
                // Set `pending_close` on the window whose last tab closed (the
                // FRONTMOST that `close_active_tab` operated on), not the event `wid`.
                let _ = wid;
                if let Some(closed) = self.close_active_tab()
                    && let Some(ws) = self.windows.get_mut(&closed)
                {
                    ws.pending_close = true;
                }
            }
            Action::NewWindow => {
                // In-process, consistent with the hardcoded Cmd-N and the menu
                // (the multi-window flip: a new window lives in THIS process, not a
                // fresh subprocess). dispatch_action has no `ActiveEventLoop`, so
                // post Wake::CreateWindow; user_event runs create_window_internal.
                if let Some(proxy) = self.proxy.as_ref() {
                    let _ = proxy.send_event(Wake::CreateWindow);
                }
            }
            Action::NextTab => self.cycle_tab(true),
            Action::PrevTab => self.cycle_tab(false),
            // 1-based as the user wrote it → 0-based index (Cmd-1..Cmd-9 parity).
            Action::SwitchTab(n) => self.switch_tab(usize::from(n).saturating_sub(1)),
            Action::SplitVertical => self.split_focused_pane(pane::SplitDir::Vertical),
            Action::SplitHorizontal => self.split_focused_pane(pane::SplitDir::Horizontal),
            Action::FocusPaneLeft => self.focus_pane(pane::FocusDir::Left),
            Action::FocusPaneRight => self.focus_pane(pane::FocusDir::Right),
            Action::FocusPaneUp => self.focus_pane(pane::FocusDir::Up),
            Action::FocusPaneDown => self.focus_pane(pane::FocusDir::Down),
            Action::TogglePaneZoom => self.toggle_pane_zoom(),
            // Copy is a no-op with no selection (matches the hardcoded fall-through).
            Action::Copy => {
                self.copy_selection();
            }
            Action::Paste => {
                // A paste, like the hardcoded Cmd-V, jumps the viewport to live.
                self.snap_to_bottom(wid);
                self.paste_clipboard();
            }
            Action::Find => self.find_requested(),
            Action::FontIncrease => self.set_font_px(self.font_px + FONT_ZOOM_STEP),
            Action::FontDecrease => self.set_font_px(self.font_px - FONT_ZOOM_STEP),
            Action::FontReset => self.set_font_px(self.default_font_px),
            // Scrollback viewport navigation (the SAME machinery the wheel/trackpad
            // and the control socket drive via InputEvent::ScrollView).
            Action::ScrollPageUp => {
                self.input(wid, InputEvent::ScrollView(ScrollIntent::Up), Source::Human);
            }
            Action::ScrollPageDown => {
                self.input(
                    wid,
                    InputEvent::ScrollView(ScrollIntent::Down),
                    Source::Human,
                );
            }
            Action::ScrollLineUp => {
                self.input(
                    wid,
                    InputEvent::ScrollView(ScrollIntent::By(1)),
                    Source::Human,
                );
            }
            Action::ScrollLineDown => {
                self.input(
                    wid,
                    InputEvent::ScrollView(ScrollIntent::By(-1)),
                    Source::Human,
                );
            }
            Action::ScrollToTop => {
                self.input(
                    wid,
                    InputEvent::ScrollView(ScrollIntent::Top),
                    Source::Human,
                );
            }
            Action::ScrollToBottom => {
                self.input(
                    wid,
                    InputEvent::ScrollView(ScrollIntent::Bottom),
                    Source::Human,
                );
            }
            Action::JumpPrevPrompt => {
                self.input(
                    wid,
                    InputEvent::ScrollView(ScrollIntent::PrevPrompt),
                    Source::Human,
                );
            }
            Action::JumpNextPrompt => {
                self.input(
                    wid,
                    InputEvent::ScrollView(ScrollIntent::NextPrompt),
                    Source::Human,
                );
            }
            // Windowed ⌘, relays through the menu wake so every platform command
            // converges on the native Settings-tab opener. Headless has no event-loop
            // proxy in tests, so it invokes that same opener directly.
            Action::ToggleSettings => {
                if self.headless {
                    let _ = self.open_settings_tab(crate::native_settings::SettingsRoute::Home);
                } else if let Some(proxy) = self.proxy.as_ref() {
                    let _ = proxy.send_event(Wake::MenuAction {
                        action: crate::menu::MenuAction::ToggleSettings,
                    });
                }
            }
            Action::ToggleAbout => {
                let _ = self.open_settings_tab(crate::native_settings::SettingsRoute::About);
            }
            // Per-session: flips the FRONT session of the window the chord
            // landed in, not an app-global latch.
            Action::ToggleMatrixRain => self.toggle_matrix_rain(wid),
            Action::ToggleSeriousMode => {
                self.user_toggle_serious_mode();
            }
            Action::OpenPalette => self.toggle_palette(),
            Action::ToggleViMode => self.toggle_vi_mode(wid),
        }
    }

    /// IME-1: a composition update (`Ime::Preedit`) — track the marked text so a
    /// preedit indicator can render and direct key sends stay suppressed while
    /// composing. An empty preedit ends the composition. Requests a repaint so
    /// the (minimal) on-screen indicator follows the composition.
    pub(crate) fn on_ime_preedit(&mut self, wid: WindowId, text: String) {
        // Track the composition on the WINDOW before any native-tab routing:
        // `ws.preedit` feeds `on_key`'s suppress_direct_send gate, so a preedit
        // that resolves while a native tab is frontmost must still update it —
        // left stale, the gate would swallow every later terminal key press.
        let mut changed = false;
        if let Some(ws) = self.windows.get_mut(&wid) {
            changed = ws.preedit != text;
            ws.preedit.clone_from(&text);
        }
        if self.active_native_view(wid).is_some() {
            let _ = self.dispatch_native_event(
                wid,
                crate::native_app::AppEvent::TextInput(crate::native_app::TextInputEvent::Preedit(
                    text,
                )),
            );
            if let Some(window) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                window.request_redraw();
            }
            return;
        }
        if changed && let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            w.request_redraw();
        }
    }

    /// IME-1: tell the platform WHERE the text cursor is (winit
    /// `set_ime_cursor_area`) so the CJK / dead-key / compose CANDIDATE window
    /// appears AT the caret rather than pinned to the window origin (which is
    /// where X11/Wayland put it otherwise). Idempotent + cheap: only calls into winit when the
    /// fully resolved PIXEL rectangle changes; a hidden cursor is left untouched.
    /// `cur` is the focused pane's cursor in pane
    /// sub-coords and `off` its window-space origin (matching the present path).
    pub(crate) fn report_ime_cursor_area(
        &mut self,
        wid: WindowId,
        cur: (u16, u16),
        off: (u16, u16),
        vis: bool,
    ) {
        if !vis {
            return;
        }
        let win_cell = (cur.0.saturating_add(off.0), cur.1.saturating_add(off.1));
        // Read geometry off `&self` BEFORE the `&mut self.windows` borrow below.
        // W12: THIS window's own metrics (mixed-DPI) — the caret rect must track the
        // window's cell box even while the shared renderer is activated to another.
        let (cw, ch) = self.win_cell_size(wid);
        let strip_px = self.tab_strip_rows as usize * ch.max(1);
        let pad = self.win_pad(wid);
        // The caret Y is the inverse of `pixel_to_term_cell`'s vertical inset, so
        // it uses the tighter top pad (`pad_top + head`) to keep the IME candidate
        // box on the cell; X carries no band and stays on `pad`.
        let pad_top = self.win_pad_top(wid);
        // Chrome headroom above the padded grid — the y-axis inverse of
        // `pixel_to_term_cell`'s `pad_top + head` (x carries no band).
        let head = self.win_head(wid);
        // W1: the frame sits behind the leading remainder bands — add the frame
        // origin so the caret rect is in true WINDOW coordinates (the inverse of
        // `window_to_frame`). `(0, 0)` at exact grid fit — byte-identical there.
        let (band_x, band_y) = self.frame_origin(wid);
        // Inverse of `app_render::pixel_to_term_cell`: cell → caret top-left px.
        // Resolve BEFORE consulting the cache so same-cell DPI/padding/remainder
        // changes are observable instead of being suppressed as false duplicates.
        let x = i64::try_from((win_cell.1 as usize) * cw.max(1) + pad)
            .unwrap_or(i64::MAX)
            .saturating_add(band_x);
        let y = i64::try_from((win_cell.0 as usize) * ch.max(1) + strip_px + pad_top + head)
            .unwrap_or(i64::MAX)
            .saturating_add(band_y);
        let rect = (
            i32::try_from(x).unwrap_or(i32::MAX),
            i32::try_from(y).unwrap_or(i32::MAX),
            u32::try_from(cw.max(1)).unwrap_or(u32::MAX),
            u32::try_from(ch.max(1)).unwrap_or(u32::MAX),
        );
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        if ws.last_ime_rect == Some(rect) {
            return;
        }
        ws.last_ime_rect = Some(rect);
        if let Some(window) = ws.os_window.as_ref() {
            window.set_ime_cursor_area(
                winit::dpi::PhysicalPosition::new(rect.0, rect.1),
                winit::dpi::PhysicalSize::new(rect.2, rect.3),
            );
        }
    }

    /// IME-1: composition committed (`Ime::Commit`) — the finished CJK/dead-key
    /// text. End the composition and send the committed text to the PTY via the
    /// engine path (each grapheme encoded as a `Character` key, NOT `& 0x1f`), so
    /// it goes out exactly as typed text. Clears the selection like any typing.
    pub(crate) fn on_ime_commit(&mut self, wid: WindowId, text: String) {
        // End the composition on the WINDOW even when a native tab consumes the
        // committed text — the same stale-preedit hazard as `on_ime_preedit`.
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.preedit.clear();
        }
        if self.active_native_view(wid).is_some() {
            if !text.is_empty() {
                let _ = self.dispatch_native_event(
                    wid,
                    crate::native_app::AppEvent::TextInput(
                        crate::native_app::TextInputEvent::Commit(text),
                    ),
                );
            }
            if let Some(window) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                window.request_redraw();
            }
            return;
        }
        if text.is_empty() {
            return;
        }
        // FIND MODE owns composed text too. On macOS the ⌥-dead-key sequences the find
        // keymap deliberately leaves unbound (⌥e e → é, ⌥s → ß) arrive HERE as an IME
        // commit, not as `KeyEvent::text` — without this they would land in the SHELL
        // behind an open find bar, which is both a lost query character and a leak.
        if self.windows.get(&wid).is_some_and(|ws| ws.search.is_some()) {
            self.search_edit_in(wid, crate::app_search::SearchEdit::Insert(text));
            if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                w.request_redraw();
            }
            return;
        }
        // Phase 0.5: committed text goes through the seam's Text path (the sole
        // keyboard-mode reader + `encode_committed_text` caller), converging with
        // the controller's text egress.
        self.input(wid, InputEvent::Text(text), Source::Human);
        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            w.request_redraw();
        }
    }

    /// Current keyboard modifiers as a mouse-report modifier mask (shift/alt/ctrl
    /// bits the engine ORs into the button byte).
    pub(crate) fn mouse_modifiers(&self, wid: WindowId) -> u8 {
        use aterm_types::mouse::{ALT_MASK, CTRL_MASK, SHIFT_MASK};
        let Some(mods) = self.windows.get(&wid).map(|ws| ws.mods) else {
            return 0;
        };
        let mut m = 0u8;
        if mods.shift_key() {
            m |= SHIFT_MASK;
        }
        if mods.alt_key() {
            m |= ALT_MASK;
        }
        if mods.control_key() {
            m |= CTRL_MASK;
        }
        m
    }

    /// `invoke <action>` (control socket): fire a menu action BY NAME through the
    /// SAME single sink the native menu bar and the ⌘K palette land in
    /// ([`Self::dispatch_menu_action`]), gated by the live palette row's `enabled`
    /// — the exact `validateMenuItem:` conditions — so a disabled row is refused
    /// with a reason, never silently fired. Names are the `action=` tokens
    /// `controls menu` prints. Headless-safe: the snapshot is live-resolved
    /// without any window.
    pub(crate) fn invoke_menu_action_by_name(
        &mut self,
        el: &ActiveEventLoop,
        name: &str,
    ) -> Result<String, String> {
        let mut p = crate::palette::PaletteState::new();
        p.resolve(&self.palette_live());
        let action = p.action_by_name(name)?;
        self.dispatch_menu_action(el, action);
        Ok(format!("invoked {name}"))
    }

    /// Convert one exact picker-approved path into the existing capability-bounded
    /// native document opener. This helper deliberately does not read the path: the
    /// document host canonicalizes it, validates a regular bounded UTF-8 file, and
    /// mints the sole grant consumed by the Markdown/editor runtime.
    fn open_local_document_path(
        &mut self,
        kind: crate::native_app::AppKind,
        path: &std::path::Path,
    ) -> Result<String, String> {
        let uri = crate::native_document_host::path_to_file_uri(path)
            .map_err(|error| format!("document open failed: {error}"))?;
        self.open_document_tab(kind, &uri)
    }

    fn choose_and_open_document(&mut self, kind: crate::native_app::AppKind) {
        let (title, prompt) = match kind {
            crate::native_app::AppKind::Markdown => ("Open Markdown", "Open as Markdown"),
            crate::native_app::AppKind::Editor => ("Open File in Editor", "Open"),
            crate::native_app::AppKind::Settings | crate::native_app::AppKind::Recovery => return,
        };
        let Some(path) = menu::choose_local_file(title, prompt) else {
            return;
        };
        if let Err(error) = self.open_local_document_path(kind, &path) {
            eprintln!("aterm-gui: selected document was not opened: {error}");
            menu::notify("Couldn’t Open File", &error);
        }
    }

    /// The window whose FIND FIELD currently owns text input, if any. Menu key
    /// equivalents (⌘V / ⌘A) reach the app through AppKit rather than `on_key`, so the
    /// menu dispatcher has to ask this the same question find mode asks.
    fn find_field_window(&self) -> Option<WindowId> {
        let wid = self.frontmost_window?;
        self.windows
            .get(&wid)
            .is_some_and(|ws| ws.search.is_some())
            .then_some(wid)
    }

    /// Dispatch a macOS menu-bar click into the EXISTING `App` command method the
    /// matching keybinding already uses — the menu adds an entry point, never a
    /// parallel implementation. Anything the user could do from the menu, they can
    /// still do from the keyboard (handled in `on_key`), byte-for-byte the same.
    /// `el` is needed only for the items that must exit the loop (Quit, and Close
    /// Tab when it closes the last tab). Off macOS this is reachable code (the
    /// `Wake::MenuAction` arm calls it) but never actually fired (no platform menu
    /// ever constructs the variant), so it stays warning-clean on every target.
    pub(crate) fn dispatch_menu_action(&mut self, el: &ActiveEventLoop, action: menu::MenuAction) {
        use menu::MenuAction;
        match action {
            // App menu --------------------------------------------------------
            // About and Software Update are routes inside the process-singleton
            // native Settings tab. Settings… (⌘,) focuses or creates that tab. "Open
            // aterm.toml" opens the same file in Settings ▸ Manual's native assisted
            // editor on every platform; Help opens the project documentation.
            MenuAction::About => {
                let _ = self.open_settings_tab(crate::native_settings::SettingsRoute::About);
            }
            // The menu-bar version item opens the same About route (full build &
            // version details) — the version title is the glance, About is the detail.
            MenuAction::Version => {
                let _ = self.open_settings_tab(crate::native_settings::SettingsRoute::About);
            }
            MenuAction::SoftwareUpdate => {
                let _ = self.open_software_update_route_and_check();
            }
            // ONE-CLICK UPDATE: the version-menu ⬆️ item / palette row / notice pill /
            // tab-strip ↻ all fire this. Strictly-newer staged ⇒ apply IMMEDIATELY
            // (the same `apply_staged_update_now` path `Wake::ApplyStagedUpdate` takes;
            // re-exec never returns) — no intermediate route change, per the owner's
            // "click-upgrade" ask. Nothing actually staged (stale nudge / QA seam) ⇒
            // fall back to the Software Update route: details, never a dead click.
            MenuAction::ApplyUpdate => self.apply_update_or_details(),
            MenuAction::Preferences => {
                let route = native_settings_route_for_menu(action)
                    .expect("Preferences has one native Settings destination");
                let _ = self.open_settings_tab(route);
            }
            MenuAction::Help => menu::open_help_url(),
            MenuAction::Quit => self.on_quit_requested(el),
            // File ------------------------------------------------------------
            // Window ▸ New Window opens a real in-process window. `dispatch_menu_action`
            // already has `el`, so create it directly (no Wake round-trip needed).
            MenuAction::NewWindow => {
                self.create_window_internal(el, None, None);
            }
            MenuAction::NewTab => self.open_tab(),
            MenuAction::OpenMarkdown => {
                self.choose_and_open_document(crate::native_app::AppKind::Markdown);
            }
            MenuAction::OpenEditor => {
                self.choose_and_open_document(crate::native_app::AppKind::Editor);
            }
            MenuAction::ReopenClosedTab => {
                let _ = self.reopen_last_closed_tab();
            }
            MenuAction::ReopenClosedView => {
                let _ = self.reopen_last_closed_view();
            }
            // Window ▸ Move Tab to New Window: pull the active tab out into a fresh
            // in-process window. `dispatch_menu_action` already has `el`, so the
            // logical move + OS-window attach run directly (no Wake round-trip).
            MenuAction::MoveTabToNewWindow => self.detach_active_tab(el),
            // Window ▸ Move Tab to Next Window: move the active tab into the NEXT
            // EXISTING window (wrapping). The destination already exists, so there is
            // no OS-window attach and no `el` is needed.
            MenuAction::MoveTabToNextWindow => self.migrate_active_tab_to_next_window(),
            // Window ▸ Open Session in New Window: show the active session in a SECOND
            // window (same live grid in two windows). `dispatch_menu_action` already
            // has `el`, so the logical attach + OS-window create run directly.
            MenuAction::ViewSessionInNewWindow => self.open_active_session_in_new_window(el),
            MenuAction::CloseTab => {
                // Same rule as Cmd-W: close the frontmost window's active tab; when
                // that was its LAST tab, escalate to closing THAT window (which exits
                // the app IFF it was the last window).
                if let Some(closed) = self.close_active_tab() {
                    self.close_window(el, closed);
                }
            }
            // Edit ------------------------------------------------------------
            // Copy with no selection is a harmless no-op (the bool is ignored here,
            // exactly like the Cmd-C fall-through in on_key).
            MenuAction::Copy => {
                let _ = self.copy_selection();
            }
            // ⌘V while the FIND FIELD is open belongs to the field, not the shell. The
            // AppKit key equivalent consumes the keyDown before `on_key` ever runs (see
            // the Cmd-Q fallback note there), so without this branch the find bar's own
            // paste arm is unreachable on macOS and the clipboard lands in the PTY
            // BEHIND the panel — the classic modal-focus leak.
            MenuAction::Paste => match self.find_field_window() {
                Some(wid) => self.search_paste_in(wid),
                None => self.paste_clipboard(),
            },
            // Tab-context copies invoked WITHOUT a tab context (the `invoke`
            // verb / a future palette row): act on the frontmost window's
            // ACTIVE tab — the same "the front tab is the subject" convention
            // every other bar action uses. The tab-strip right-click path posts
            // `Wake::TabMenuAction` instead, which carries the exact clicked
            // tab's STABLE id into the same per-tab dispatcher; here the
            // subject is resolved NOW, so the active tab's id is that capture.
            MenuAction::CopySessionId | MenuAction::CopyCwd => {
                if let Some(window) = self.frontmost_window
                    && let Some(tab) = self
                        .windows
                        .get(&window)
                        .and_then(|ws| ws.tab_set.active_id())
                {
                    self.dispatch_tab_menu_action(el, window, tab, action);
                }
            }
            // Select All belongs to the TERMINAL's selection, which the find bar is
            // currently using to mark its match — running it under an open find would
            // replace that highlight and snap the viewport to the live bottom while the
            // panel still reads `3/7`. The field has no selection model of its own yet,
            // so under find this is deliberately inert rather than wrong.
            MenuAction::SelectAll => {
                if self.find_field_window().is_none() {
                    self.select_all();
                }
            }
            // Diverts to the settings search when the focused window shows the
            // Settings card — the ⌘F key equivalent lands here, not in on_key.
            MenuAction::Find => self.find_requested(),
            // Find Next/Previous step an open search or resume the last accepted
            // query after Enter closed the bar (standard Cmd-G behavior).
            MenuAction::FindNext => self.search_find_again(true),
            MenuAction::FindPrev => self.search_find_again(false),
            // View ------------------------------------------------------------
            MenuAction::ToggleFullScreen => self.toggle_fullscreen(),
            // Font size — identical to on_key_font_zoom (⌘= / ⌘- / ⌘0).
            MenuAction::FontIncrease => self.set_font_px(self.font_px + FONT_ZOOM_STEP),
            MenuAction::FontDecrease => self.set_font_px(self.font_px - FONT_ZOOM_STEP),
            MenuAction::FontActualSize => self.set_font_px(self.default_font_px),
            MenuAction::SplitVertical => self.split_focused_pane(pane::SplitDir::Vertical),
            MenuAction::SplitHorizontal => self.split_focused_pane(pane::SplitDir::Horizontal),
            // Per-session matrix rain: toggle the FRONT session of the
            // frontmost window (the same sink the `toggle_matrix_rain`
            // keybinding and `aterm-ctl rain toggle` converge on). No-op with
            // no frontmost terminal (native tab — the item is greyed there).
            MenuAction::ToggleMatrixRain => {
                if let Some(wid) = self.frontmost_window {
                    self.toggle_matrix_rain(wid);
                }
            }
            // Promote the front session's own kitty into the durable registry.
            // No-op with no frontmost terminal (the item is greyed there).
            MenuAction::FavouriteSessionKitty => {
                if let Some(wid) = self.frontmost_window {
                    self.favourite_session_kitty(wid, std::time::Instant::now());
                }
            }
            MenuAction::ToggleSeriousMode => {
                self.user_toggle_serious_mode();
            }
            // Focus or create the process-singleton native Settings tab.
            MenuAction::ToggleSettings => {
                self.open_settings_tab(crate::native_settings::SettingsRoute::Home);
            }
            // Same singleton tab, opened AT the Packages route — the menu-bar
            // path to the batteries-included toolchain surface.
            MenuAction::Packages => {
                self.open_settings_tab(crate::native_settings::SettingsRoute::Packages);
            }
            // Toggle the own-rendered, cross-platform command palette.
            MenuAction::OpenPalette => self.toggle_palette(),
            // Window ----------------------------------------------------------
            MenuAction::NextTab => self.cycle_tab(true),
            MenuAction::PrevTab => self.cycle_tab(false),
            MenuAction::Minimize => {
                if let Some(w) = self.front().and_then(|ws| ws.os_window.as_ref()) {
                    w.set_minimized(true);
                }
            }
            MenuAction::Zoom => {
                // Zoom toggles maximised, like the green-button / Window ▸ Zoom.
                if let Some(w) = self.front().and_then(|ws| ws.os_window.as_ref()) {
                    w.set_maximized(!w.is_maximized());
                }
            }
        }
    }

    /// Dispatch one tab-strip CONTEXT-MENU action against the EXACT tab it was
    /// popped on — `Wake::TabMenuAction { window, tab, action }`, posted by the
    /// macOS strip's right-click/ctrl-click `NSMenu` (`toolbar.rs`). The clicked
    /// tab need NOT be the active one, which is why this cannot ride the plain
    /// `Wake::MenuAction` path (that convention targets the front active tab).
    ///
    /// TARGETING: `tab` is the STABLE [`crate::tab_model::TabId`] captured when
    /// the menu popped, re-resolved to a CURRENT position here — the menu can
    /// stay open across tab mutations (a background exit, a control-socket
    /// `tab close`/`tab move`), and wakes are FIFO, so any mutation queued
    /// while the menu tracked has already been applied by the time this runs.
    /// A reorder therefore re-targets the SAME tab at its new position; a tab
    /// that no longer exists makes the whole action a logged no-op. NEVER fall
    /// back to a positional guess: closing / copying from "whatever sits there
    /// now" is precisely the wrong-session bug the stable id exists to prevent.
    ///
    /// * `CopySessionId` / `CopyCwd` resolve the tab's terminal session and put
    ///   the registry `sid` / the RAW engine cwd on the OS pasteboard via the
    ///   SAME [`crate::control::pbcopy`] seam every copy path uses
    ///   (Cmd-C, copy-on-select, OSC 52, the `copy` verb). Values are resolved
    ///   FRESH here, not from the composed menu snapshot, so a copy after a
    ///   long-open menu still pastes the current truth.
    /// * `CloseTab` closes that tab through the byte-same body as the strip's
    ///   `✕` (`Wake::CloseTab`): whole-tab close + last-tab window escalation.
    /// * Anything else (defensive — the composer only mints the three above)
    ///   falls through to the plain menu dispatcher; termination is guaranteed
    ///   because the Copy* arms there never re-enter with a non-Copy action.
    pub(crate) fn dispatch_tab_menu_action(
        &mut self,
        el: &ActiveEventLoop,
        window: WindowId,
        tab: crate::tab_model::TabId,
        action: menu::MenuAction,
    ) {
        use menu::MenuAction;
        let Some(index) = self.tab_index_for_id(window, tab) else {
            // The clicked tab closed between menu pop and item click (or its
            // window went away). Acting on any OTHER tab here would be the
            // stale-index misdirection this id exists to prevent — drop the
            // action and say so.
            aterm_log::info!(
                "tab context-menu {action:?} dropped: tab {tab} no longer exists in its window"
            );
            return;
        };
        match action {
            MenuAction::CopySessionId => {
                if let Some(text) = self.tab_session_id_text(window, index) {
                    let _ = crate::control::pbcopy(&text);
                }
            }
            MenuAction::CopyCwd => {
                if let Some(cwd) = self.tab_session_cwd(window, index) {
                    let _ = crate::control::pbcopy(&cwd);
                }
            }
            MenuAction::CloseTab => {
                // Byte-same as the `Wake::CloseTab` handler: close the tab as a
                // unit; if it was the window's LAST tab, flag + escalate so the
                // window tears down (we have `el` here).
                if self.close_tab_at(window, index)
                    && let Some(ws) = self.windows.get_mut(&window)
                {
                    ws.pending_close = true;
                }
                self.escalate_pending_close(el);
            }
            other => self.dispatch_menu_action(el, other),
        }
    }

    /// The registry `sid` string for the terminal session labeling tab `index`
    /// of `window`, or `None` for a native tab / unregistered stub — the
    /// `Copy Session ID` payload. Store READ lock only (a leaf, briefly).
    pub(crate) fn tab_session_id_text(&self, window: WindowId, index: usize) -> Option<String> {
        let session = self.tab_terminal_session(window, index)?;
        self.store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_local(session)
            .map(|h| h.sid.as_str().to_string())
    }

    /// The RAW shell-reported cwd of the terminal session labeling tab `index`
    /// of `window` (never `~`-abbreviated — a pasted path must be real), or
    /// `None` when absent — the `Copy CWD` payload. One brief term lock.
    pub(crate) fn tab_session_cwd(&self, window: WindowId, index: usize) -> Option<String> {
        let session = self.tab_terminal_session(window, index)?;
        let s = self.pool.get(session)?;
        let term = crate::term_lock(&s.term);
        term.current_working_directory()
            .filter(|c| !c.is_empty())
            .map(str::to_string)
    }

    /// The CURRENT position of the canonical tab with stable id `tab` in
    /// `window`, or `None` when that tab no longer exists there. This is the
    /// context-menu dispatch resolver: a stable id captured at menu-pop time
    /// survives any reorder (it finds the tab's NEW position) and turns a
    /// close into an honest `None` — unlike a raw position, which silently
    /// re-binds to whatever tab inherited the slot. `TabId`s are minted by a
    /// never-reusing allocator, so a `None` can never be a recycled identity.
    pub(crate) fn tab_index_for_id(
        &self,
        window: WindowId,
        tab: crate::tab_model::TabId,
    ) -> Option<usize> {
        let ws = self.windows.get(&window)?;
        ws.tab_set.tabs().iter().position(|t| t.id == tab)
    }

    /// The terminal session labeling tab `index` of `window` (its FOCUSED leaf,
    /// the same resolution `tab_titles` uses), or `None` for a native tab / an
    /// out-of-range index.
    pub(crate) fn tab_terminal_session(&self, window: WindowId, index: usize) -> Option<u64> {
        let ws = self.windows.get(&window)?;
        let tab = ws.tab_set.tabs().get(index)?;
        self.view_store
            .get(tab.focus)
            .copied()
            .and_then(crate::tab_model::View::terminal_session)
    }

    /// Request a process-global Serious Mode transition through the same versioned,
    /// compare-and-swap config lane as native Settings. The live policy changes only
    /// after the durable completion is reconciled, so a conflict or failed write can
    /// never leave the renderer/audio policy ahead of `aterm.toml`.
    pub(crate) fn user_toggle_serious_mode(&mut self) {
        if let Err(error) = self.queue_serious_mode_toggle() {
            self.config_notice = crate::config_notice::ConfigNotice::new(
                vec![format!("Serious Mode was not changed: {error}")],
                std::time::Instant::now(),
            );
            self.request_redraw_all_windows();
        }
    }

    /// Open a compatibility GUI target, reusing the SAME paths as human menu items.
    /// `prefs`, `about`, and `update` retain their wire spellings but resolve to routes
    /// in the native Settings tab; `menu` remains a transient overlay.
    /// The front terminal window is always open, so opening it is an error. Native
    /// Settings and the palette render through the ordinary virtual frame in headless mode.
    pub(crate) fn open_aux_window(
        &mut self,
        _el: &winit::event_loop::ActiveEventLoop,
        target: crate::app_introspect::AuxTarget,
    ) -> Result<(), String> {
        use crate::app_introspect::AuxTarget;
        if target == AuxTarget::Update {
            return self.open_software_update_route_and_check().map(|_| ());
        }
        if let Some(route) = native_settings_route_for_aux(target) {
            return if self.open_settings_tab(route) {
                Ok(())
            } else {
                Err(format!(
                    "could not open the native Settings {} route",
                    route.path()
                ))
            };
        }
        match target {
            AuxTarget::Menu => {
                // The command palette is opened ON the front window (own-rendered), so
                // `open menu` brings it up; `image`/`window`/`controls menu` then read it.
                self.palette_enter();
                Ok(())
            }
            AuxTarget::Front => {
                Err("the front terminal window is already open (use window/chrome)".to_string())
            }
            AuxTarget::Prefs | AuxTarget::About | AuxTarget::Update => {
                Err("native Settings alias routing failed".to_string())
            }
        }
    }

    /// The `open <target> close` counterpart. Durable Settings-route aliases close
    /// Settings tab presentations through the native close path; the command palette
    /// uses its transient overlay exit. A not-open target is an idempotent `Ok`.
    pub(crate) fn close_aux_overlay(
        &mut self,
        target: crate::app_introspect::AuxTarget,
    ) -> Result<(), String> {
        use crate::app_introspect::AuxTarget;
        match target {
            AuxTarget::Prefs | AuxTarget::About | AuxTarget::Update => {
                self.close_settings_tabs();
                Ok(())
            }
            AuxTarget::Menu => {
                self.palette_exit();
                Ok(())
            }
            // `Front` is the terminal itself and is not a closable app surface here.
            AuxTarget::Front => Err(
                "close supports native Settings routes (prefs | about | update) and menu"
                    .to_string(),
            ),
        }
    }
}

#[cfg(all(test, unix))]
mod forwarded_release_handoff_activity_tests {
    use crate::{App, WindowId};
    use winit::keyboard::{KeyCode, PhysicalKey};

    /// Arm a minimal pending overlap so the tolerated/revoking distinction is
    /// observable: the revoking activity form bumps the epoch and pokes the
    /// cancel channel; the tolerated form does neither while one is pending.
    fn arm_pending_overlap(app: &mut App) -> std::sync::mpsc::Receiver<()> {
        let (cancel, cancelled) = std::sync::mpsc::sync_channel(1);
        app.pending_update_handoff = Some(crate::PendingUpdateHandoff {
            attempt_id: 1,
            nonce: None,
            live: Vec::new(),
            adoption: Vec::new(),
            child_pid: None,
            mode: crate::native_updater_service::ApplyMode::Immediate,
            apply_attempt: None,
            target_build: 0,
            target_commit: String::new(),
            layout: crate::restore::RestoreManifest::new(Vec::new()),
            layout_digest: [0; 32],
            screen_digest: [0; 32],
            activity_epoch: app.update_handoff_activity_epoch,
            cancel,
            arbiter: crate::HandoffAttemptArbiter::new(),
            teardown: crate::DeferredHandoffTeardown::None,
            commit_drain_started: None,
            revoked_by_activity: false,
        });
        cancelled
    }

    /// A forwarded key RELEASE is the same tolerated key traffic as its press:
    /// `input_to_session` and the window-event seam both buffer key events
    /// through a pending seamless-update overlap, so the out-of-band release
    /// path must neither bump the activity epoch (which would falsify the
    /// `exact_activity` Commit fact) nor poke the overlap's cancel channel.
    #[test]
    fn forwarded_release_is_tolerated_while_overlap_pending() {
        use aterm_types::keyboard::{Key as TKey, Modifiers};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let physical = PhysicalKey::Code(KeyCode::KeyA);
        app.note_forwarded_press_key(
            wid,
            physical,
            false,
            TKey::Character('a'),
            Modifiers::empty(),
            None,
        );
        let cancelled = arm_pending_overlap(&mut app);
        let epoch = app.update_handoff_activity_epoch;
        app.release_physical_press(wid, physical);
        assert!(
            !app.physical_press_owners.contains_key(&physical),
            "the forwarded episode is closed by its release"
        );
        assert_eq!(
            app.update_handoff_activity_epoch, epoch,
            "a key release must not bump the activity epoch while an overlap is pending"
        );
        assert!(
            cancelled.try_recv().is_err(),
            "a key release must not poke the pending overlap's cancel channel"
        );
    }
}

#[cfg(test)]
mod native_aux_alias_tests {
    use super::native_settings_route_for_aux;
    use crate::app_introspect::AuxTarget;
    use crate::native_app::AppViewState;
    use crate::native_settings::SettingsRoute;
    use crate::{App, WindowId};

    #[test]
    fn legacy_about_and_update_spellings_navigate_native_settings_routes() {
        for (spelling, route) in [
            ("about", SettingsRoute::About),
            ("update", SettingsRoute::SoftwareUpdate),
            ("software-update", SettingsRoute::SoftwareUpdate),
        ] {
            let target = AuxTarget::parse(spelling).expect("compatibility spelling");
            assert_eq!(native_settings_route_for_aux(target), Some(route));

            let mut app = App::headless_for_test();
            assert!(app.open_settings_tab(route));
            let (_, view) = app
                .active_native_view(WindowId(0))
                .expect("native Settings view");
            assert!(matches!(
                app.native_runtime.view_state(view),
                Some(AppViewState::Settings(state)) if state.route == route
            ));
            assert!(app.windows[&WindowId(0)].overlay.is_none());
            assert!(app.close_settings_tabs());
            assert!(app.active_native_view(WindowId(0)).is_none());
        }
        assert_eq!(AuxTarget::parse("perf"), None);
        assert_eq!(AuxTarget::parse("performance"), None);
    }
}

#[cfg(test)]
mod native_preferences_menu_tests {
    use super::native_settings_route_for_menu;
    use crate::menu::MenuAction;
    use crate::native_app::AppKind;
    use crate::native_settings::SettingsRoute;
    use crate::{App, WindowId};

    #[test]
    fn open_aterm_toml_menu_command_is_the_native_manual_editor() {
        const CHILD: &str = "ATERM_NATIVE_PREFERENCES_MENU_CHILD";
        const ROOT: &str = "ATERM_NATIVE_PREFERENCES_MENU_ROOT";
        const EXACT: &str = concat!(
            "app_input::native_preferences_menu_tests::",
            "open_aterm_toml_menu_command_is_the_native_manual_editor"
        );
        if std::env::var_os(CHILD).is_none() {
            let root = std::env::temp_dir().join(format!(
                "aterm-native-preferences-menu-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", EXACT, "--nocapture"])
                .env(CHILD, "1")
                .env(ROOT, &root)
                .env("XDG_CONFIG_HOME", &root)
                .env("RUST_TEST_THREADS", "1")
                .status()
                .expect("launch isolated native Preferences-menu test");
            let _ = std::fs::remove_dir_all(root);
            assert!(status.success());
            return;
        }

        let root = std::path::PathBuf::from(std::env::var_os(ROOT).unwrap());
        let route = native_settings_route_for_menu(MenuAction::Preferences);
        assert_eq!(route, Some(SettingsRoute::Manual));

        let mut app = App::headless_for_test();
        assert!(app.open_settings_tab(route.expect("Manual route")));
        assert!(
            root.join("aterm/aterm.toml").is_file(),
            "Manual must resolve only the isolated config file"
        );
        let (instance, _) = app
            .active_native_view(WindowId(0))
            .expect("native Manual editor");
        assert_eq!(
            app.native_runtime.app(instance).map(|app| app.kind()),
            Some(AppKind::Editor)
        );
        assert!(app.windows[&WindowId(0)].front_terminal().is_none());
        assert!(app.windows[&WindowId(0)].overlay.is_none());
    }
}

#[cfg(test)]
mod serious_mode_command_tests {
    use crate::App;

    #[test]
    fn unavailable_config_lane_leaves_runtime_and_service_unchanged_and_reports_failure() {
        let mut app = App::headless_for_test();
        let before = app.native_config_service.snapshot();
        assert!(!app.serious_mode_enabled());
        assert_eq!(app.config.serious_mode, None);
        assert!(
            app.proxy.is_none(),
            "negative control needs no event-loop host"
        );

        app.user_toggle_serious_mode();

        assert!(
            !app.serious_mode_enabled(),
            "the live suppression policy must not move before durability"
        );
        assert_eq!(app.config.serious_mode, None);
        let after = app.native_config_service.snapshot();
        assert_eq!(after.revision, before.revision);
        assert_eq!(after.text, before.text);
        assert!(
            app.native_config_pending.is_empty(),
            "a failed preflight must not leave a command that can apply later"
        );
        assert!(
            app.config_notice.as_ref().is_some_and(|notice| notice
                .lines
                .iter()
                .any(|line| line.contains("Serious Mode was not changed"))),
            "Finder-launched users need visible failure feedback"
        );
    }
}

#[cfg(test)]
mod native_keyboard_boundary_tests {
    use std::fs;

    use super::native_binding_allowed;
    use crate::input::{InputEvent, InputOutcome, Source};
    use crate::native_app::AppKind;
    use crate::native_settings::SettingsRoute;
    use crate::{App, WindowId, keybinding};
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers};

    fn key(character: char, mods: Modifiers) -> InputEvent {
        InputEvent::Key {
            key: Key::Character(character),
            mods,
            base_layout: None,
            event_type: KeyEventType::Press,
        }
    }

    fn file_uri(path: &std::path::Path) -> String {
        format!("file://{}", path.to_string_lossy().replace(' ', "%20"))
    }

    #[test]
    fn native_binding_partition_keeps_terminal_only_actions_out() {
        for allowed in [
            keybinding::Action::NewTab,
            keybinding::Action::ReopenClosedTab,
            keybinding::Action::CloseTab,
            keybinding::Action::NextTab,
            keybinding::Action::SwitchTab(3),
            keybinding::Action::Paste,
            keybinding::Action::Find,
            keybinding::Action::ScrollPageDown,
            keybinding::Action::ToggleSettings,
        ] {
            assert!(native_binding_allowed(allowed), "{allowed:?}");
        }
        for terminal_only in [
            keybinding::Action::FontIncrease,
            keybinding::Action::FontReset,
            keybinding::Action::JumpPrevPrompt,
            keybinding::Action::JumpNextPrompt,
            keybinding::Action::ToggleViMode,
        ] {
            assert!(!native_binding_allowed(terminal_only), "{terminal_only:?}");
        }
        // Copy is capability-routed separately so it can never consult the
        // parked terminal selection beneath a native tab.
        assert!(!native_binding_allowed(keybinding::Action::Copy));
    }

    #[test]
    fn picker_path_opens_only_the_exact_local_document_grant() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-native-picker-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let chosen = dir.join("chosen.md");
        let unchosen = dir.join("unchosen.md");
        fs::write(&chosen, "# Chosen\n").unwrap();
        fs::write(&unchosen, "# Must remain unopened\n").unwrap();

        let mut app = App::headless_for_test();
        let opened = app
            .open_local_document_path(AppKind::Markdown, &chosen)
            .expect("picker-approved file opens");
        assert!(opened.starts_with("app markdown file://"), "{opened}");
        let (instance, _) = app
            .active_native_view(WindowId(0))
            .expect("Markdown became the active native tab");
        assert_eq!(
            app.native_runtime.app(instance).map(|app| app.kind()),
            Some(AppKind::Markdown)
        );

        let chosen_uri =
            crate::native_document_host::path_to_file_uri(&std::fs::canonicalize(&chosen).unwrap())
                .unwrap();
        let unchosen_uri = crate::native_document_host::path_to_file_uri(
            &std::fs::canonicalize(&unchosen).unwrap(),
        )
        .unwrap();
        assert!(app.document_store.id_for_uri(&chosen_uri).is_some());
        assert_eq!(
            app.document_store.id_for_uri(&unchosen_uri),
            None,
            "choosing one file must not acquire ambient directory authority"
        );
        assert!(
            app.open_local_document_path(AppKind::Editor, &dir).is_err(),
            "directories are never document grants"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn native_settings_cmd_f_focuses_its_own_search() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(SettingsRoute::Appearance));
        let _ = app.input(wid, key('f', Modifiers::SUPER), Source::Human);
        let (_, view) = app.active_native_view(wid).expect("Settings view");
        assert!(matches!(
            app.native_runtime.view_state(view),
            Some(crate::native_app::AppViewState::Settings(state))
                if state.common.last_focus.as_ref().is_some_and(
                    |key| key.as_str() == "settings/search"
                )
        ));
        assert!(
            app.windows[&wid].search.is_none(),
            "terminal find stays off"
        );
    }

    #[test]
    fn editor_cmd_s_and_cmd_z_reduce_before_the_terminal_boundary() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-native-keyboard-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("keyboard.md");
        fs::write(&path, "before\n").unwrap();

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        assert_eq!(
            app.input(wid, InputEvent::Text("safe ".to_string()), Source::Human),
            InputOutcome::Ok
        );
        let (instance, _) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            "safe before\n"
        );

        assert_eq!(
            app.input(wid, key('s', Modifiers::SUPER), Source::Human),
            InputOutcome::Ok
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "safe before\n");
        assert_eq!(app.document_store.dirty(document), Some(false));

        assert_eq!(
            app.input(wid, key('z', Modifiers::SUPER), Source::Human),
            InputOutcome::Ok
        );
        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            "before\n"
        );
        assert_eq!(app.document_store.dirty(document), Some(true));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn native_repeat_replays_exact_press_view_and_never_the_replacement_tab() {
        use winit::keyboard::{KeyCode, PhysicalKey};

        let dir = std::env::temp_dir().join(format!(
            "aterm-native-repeat-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("repeat.md");
        fs::write(&path, "abc").unwrap();

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let (instance, view) = app.active_native_view(wid).expect("Editor view");
        let document = app.native_runtime.document_id(instance).unwrap();
        let typed = InputEvent::Text("x".to_string());

        // Model the genuine key-down, then capture the normalized repeat owner
        // exactly as `on_key_native_mode` does.
        assert_eq!(
            app.input(wid, typed.clone(), Source::Human),
            InputOutcome::Ok
        );
        let physical = PhysicalKey::Code(KeyCode::KeyX);
        app.note_local_repeat_press(
            wid,
            physical,
            crate::LocalRepeatAction::Native { view, event: typed },
        );
        app.route_physical_repeat(wid, physical);
        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            "xxabc",
            "the repeat is delivered to the captured Editor view"
        );

        // Replacing the active tab while the key remains held invalidates the
        // captured view. Later repeats are swallowed, never reinterpreted as a
        // Settings key or redirected back through current-content resolution.
        assert!(app.open_settings_tab(SettingsRoute::Home));
        app.route_physical_repeat(wid, physical);
        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            "xxabc",
            "a stale native repeat cannot mutate either replacement content"
        );
        assert!(app.take_local_repeat_release(wid, physical));
        assert!(!app.take_local_repeat_release(wid, physical));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn markdown_cmd_a_and_escape_own_exact_source_selection() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-native-markdown-keyboard-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("reader.md");
        let source = "# Héllo\n\nBody 🦀 and [link](https://example.com).\n";
        fs::write(&path, source).unwrap();

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Markdown, &file_uri(&path))
            .unwrap();
        assert_eq!(
            app.input(wid, key('a', Modifiers::SUPER), Source::Human),
            InputOutcome::Ok
        );
        let (_, view) = app.active_native_view(wid).expect("Markdown view");
        assert!(matches!(
            app.native_runtime.view_state(view),
            Some(crate::native_app::AppViewState::Markdown(state))
                if state.selection == Some(0..source.len())
        ));
        assert_eq!(app.native_selection_text(wid).as_deref(), Some(source));

        assert_eq!(
            app.input(
                wid,
                InputEvent::Key {
                    key: Key::Named(aterm_types::keyboard::NamedKey::Escape),
                    mods: Modifiers::empty(),
                    base_layout: None,
                    event_type: KeyEventType::Press,
                },
                Source::Human,
            ),
            InputOutcome::Ok
        );
        assert!(matches!(
            app.native_runtime.view_state(view),
            Some(crate::native_app::AppViewState::Markdown(state))
                if state.selection.is_none()
        ));
        assert_eq!(app.native_selection_text(wid), None);
        let _ = fs::remove_dir_all(dir);
    }

    /// A configured raw sequence is explicit terminal authority. The physical
    /// key path resolves and swallows it before this seam; this lower boundary
    /// independently proves that even a controller-supplied `KeySequence`
    /// cannot cross an active native tab. The terminal pass is the negative
    /// control, proving the pipe observes real writes rather than a vacuous zero.
    #[cfg(unix)]
    #[test]
    fn raw_key_sequence_never_reaches_parked_pty_under_native_tab() {
        use std::sync::Arc;

        use aterm_session::sink::SinkWriter;

        let mut pipe = [0; 2];
        assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
        let flags = unsafe { libc::fcntl(pipe[0], libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(
            unsafe { libc::fcntl(pipe[0], libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0
        );

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.windows
            .get_mut(&wid)
            .unwrap()
            .active_terminal
            .as_mut()
            .unwrap()
            .sink = Arc::new(SinkWriter::new(pipe[1]));
        assert!(app.open_settings_tab(SettingsRoute::Home));
        let raw = b"\x1b[99~".to_vec();
        assert_eq!(
            app.input(wid, InputEvent::KeySequence(raw.clone()), Source::Human),
            InputOutcome::Ok
        );
        let mut bytes = [0u8; 32];
        assert_eq!(
            unsafe { libc::read(pipe[0], bytes.as_mut_ptr().cast(), bytes.len()) },
            -1
        );

        assert!(app.close_settings_tabs());
        // Closing the native tab deliberately re-mirrors the terminal session,
        // including its real sink; restore the observing pipe for the terminal
        // negative control.
        app.windows
            .get_mut(&wid)
            .unwrap()
            .active_terminal
            .as_mut()
            .unwrap()
            .sink = Arc::new(SinkWriter::new(pipe[1]));
        assert_eq!(
            app.input(wid, InputEvent::KeySequence(raw.clone()), Source::Human),
            InputOutcome::Ok
        );
        let read = unsafe { libc::read(pipe[0], bytes.as_mut_ptr().cast(), bytes.len()) };
        assert_eq!(
            read,
            raw.len() as isize,
            "negative control must observe PTY bytes"
        );
        assert_eq!(&bytes[..read as usize], raw.as_slice());
        unsafe {
            libc::close(pipe[0]);
            libc::close(pipe[1]);
        }
    }
}

#[cfg(test)]
mod local_repeat_behavior_tests {
    use crate::input::InputEvent;
    use crate::{App, LocalRepeatAction, SearchRepeatAction, WindowId, term_lock};
    use winit::keyboard::{KeyCode, PhysicalKey};

    #[test]
    fn search_repeat_uses_captured_session_and_normalized_edit() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.search_enter();
        let session = app.front_terminal(wid).expect("terminal").session;
        let physical = PhysicalKey::Code(KeyCode::KeyQ);
        app.note_local_repeat_press(
            wid,
            physical,
            LocalRepeatAction::Search {
                session,
                action: SearchRepeatAction::Edit(crate::app_search::SearchEdit::Insert(
                    "q".to_string(),
                )),
            },
        );

        app.route_physical_repeat(wid, physical);
        app.route_physical_repeat(wid, physical);
        assert_eq!(
            app.windows[&wid]
                .search
                .as_ref()
                .map(|search| search.query.as_str()),
            Some("qq")
        );
        assert!(app.take_local_repeat_release(wid, physical));
    }

    #[test]
    fn search_repeat_stays_on_press_window_after_focus_changes() {
        let mut app = App::headless_for_test();
        let press_window = WindowId(0);
        let terminal = app
            .front_terminal(press_window)
            .expect("press terminal")
            .term
            .clone();
        term_lock(&terminal).process(b"focus_routing_needle");
        app.search_enter();

        let session = app
            .front_terminal(press_window)
            .expect("press terminal")
            .session;
        let physical = PhysicalKey::Code(KeyCode::KeyQ);
        app.note_local_repeat_press(
            press_window,
            physical,
            LocalRepeatAction::Search {
                session,
                action: SearchRepeatAction::Edit(crate::app_search::SearchEdit::Insert(
                    "focus_routing_needle".to_string(),
                )),
            },
        );

        let next_session = app.next_session_id;
        let arrival_window = app.insert_logical_window(crate::stub_session(next_session), 24, 80);
        assert_eq!(app.frontmost_window, Some(arrival_window));

        app.route_physical_repeat(arrival_window, physical);

        let search = app.windows[&press_window]
            .search
            .as_ref()
            .expect("press-window search remains open");
        assert_eq!(search.query, "focus_routing_needle");
        assert!(
            !search.matches.is_empty(),
            "repeat edit must recompute and navigate its press-time window"
        );
        assert!(app.windows[&arrival_window].search.is_none());
        assert!(app.take_local_repeat_release(arrival_window, physical));
    }

    #[test]
    fn palette_repeat_stops_when_its_press_time_overlay_closes() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.palette_enter();
        let physical = PhysicalKey::Code(KeyCode::KeyN);
        app.note_local_repeat_press(
            wid,
            physical,
            LocalRepeatAction::Palette(InputEvent::Text("n".to_string())),
        );
        app.route_physical_repeat(wid, physical);
        let before_close = app.windows[&wid]
            .palette()
            .expect("palette")
            .controls_lines();
        assert!(before_close[0].contains("query=\"n\""), "{before_close:?}");

        app.palette_exit();
        app.route_physical_repeat(wid, physical);
        assert!(app.windows[&wid].palette().is_none());
        assert!(app.take_local_repeat_release(wid, physical));
    }

    #[test]
    fn vi_repeat_moves_only_the_captured_terminal() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let terminal = app.front_terminal(wid).expect("terminal").clone();
        app.toggle_vi_mode(wid);
        let before = term_lock(&terminal.term).vi_cursor_point();
        let physical = PhysicalKey::Code(KeyCode::KeyL);
        app.note_local_repeat_press(
            wid,
            physical,
            LocalRepeatAction::Vi {
                session: terminal.session,
                action: crate::vi_keys::ViAction::Motion(aterm_core::ViMotion::Right),
            },
        );
        app.route_physical_repeat(wid, physical);
        let after = term_lock(&terminal.term).vi_cursor_point();
        assert_eq!(after.line, before.line);
        assert_eq!(after.col, before.col + 1);
        assert!(app.take_local_repeat_release(wid, physical));
    }
}

#[cfg(test)]
mod terminal_emacs_search_input_tests {
    use super::terminal_emacs_search_direction;
    use crate::{App, PhysicalPressOwner, SearchRepeatAction, WindowId, term_lock};
    use winit::keyboard::{Key, KeyCode, ModifiersState, PhysicalKey};

    fn character(value: &str) -> Key {
        Key::Character(value.into())
    }

    /// Exhaust all 16 modifier subsets for both navigation keys (plus case and a
    /// non-navigation negative control). Only the exact bare-Super subset qualifies.
    #[test]
    fn bare_super_s_r_classifier_is_exhaustive_over_modifier_power_set() {
        let flags = [
            ModifiersState::SHIFT,
            ModifiersState::CONTROL,
            ModifiersState::ALT,
            ModifiersState::SUPER,
        ];
        for mask in 0u8..16 {
            let mut mods = ModifiersState::empty();
            for (bit, flag) in flags.into_iter().enumerate() {
                if mask & (1 << bit) != 0 {
                    mods |= flag;
                }
            }
            let expected = mods == ModifiersState::SUPER;
            assert_eq!(
                terminal_emacs_search_direction(&character("s"), mods),
                expected.then_some(true),
                "S mask={mask:04b}"
            );
            assert_eq!(
                terminal_emacs_search_direction(&character("R"), mods),
                expected.then_some(false),
                "R mask={mask:04b}"
            );
            assert_eq!(
                terminal_emacs_search_direction(&character("f"), mods),
                None,
                "Cmd-F keeps its legacy path, mask={mask:04b}"
            );
        }
    }

    #[cfg(unix)]
    fn observe_pty(app: &mut App, wid: WindowId) -> [i32; 2] {
        use std::sync::Arc;

        use aterm_session::sink::SinkWriter;

        let mut pipe = [0; 2];
        assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
        let flags = unsafe { libc::fcntl(pipe[0], libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(
            unsafe { libc::fcntl(pipe[0], libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0
        );
        app.windows
            .get_mut(&wid)
            .unwrap()
            .active_terminal
            .as_mut()
            .unwrap()
            .sink = Arc::new(SinkWriter::new(pipe[1]));
        pipe
    }

    #[cfg(unix)]
    fn assert_pty_silent_and_close(pipe: [i32; 2]) {
        let mut bytes = [0u8; 64];
        assert_eq!(
            unsafe { libc::read(pipe[0], bytes.as_mut_ptr().cast(), bytes.len()) },
            -1,
            "host-owned search press/repeat/release emitted PTY bytes"
        );
        assert_eq!(
            std::io::Error::last_os_error().kind(),
            std::io::ErrorKind::WouldBlock
        );
        unsafe {
            libc::close(pipe[0]);
            libc::close(pipe[1]);
        }
    }

    /// Normal-shell route through the shipping physical-owner seams: Cmd-S opens,
    /// typing searches, a held key repeats locally, Cmd-R reverses and wraps, and both
    /// releases are swallowed even with Kitty event-type reporting enabled.
    #[cfg(unix)]
    #[test]
    fn normal_terminal_cmd_s_r_press_repeat_release_emit_zero_kitty_bytes() {
        let _serial = crate::control::search_cap_test_guard();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let pipe = observe_pty(&mut app, wid);
        let term = app.front_terminal(wid).expect("terminal").term.clone();
        term_lock(&term).process(b"\x1b[>2uhit one\r\nhit two");

        let cmd_s = PhysicalKey::Code(KeyCode::KeyS);
        app.terminal_emacs_search_pressed(wid, cmd_s, true);
        assert!(matches!(
            app.physical_press_owners.get(&cmd_s),
            Some(PhysicalPressOwner::Local { .. })
        ));
        app.apply_search_repeat_action(
            wid,
            SearchRepeatAction::Edit(crate::app_search::SearchEdit::Insert("hit".into())),
        );
        assert_eq!(app.windows[&wid].search.as_ref().unwrap().current, 0);
        app.route_physical_repeat(wid, cmd_s);
        assert_eq!(app.windows[&wid].search.as_ref().unwrap().current, 1);
        app.release_physical_press(wid, cmd_s);
        assert!(!app.physical_press_owners.contains_key(&cmd_s));

        let cmd_r = PhysicalKey::Code(KeyCode::KeyR);
        app.terminal_emacs_search_pressed(wid, cmd_r, false);
        assert_eq!(app.windows[&wid].search.as_ref().unwrap().current, 0);
        app.route_physical_repeat(wid, cmd_r);
        assert_eq!(
            app.windows[&wid].search.as_ref().unwrap().current,
            1,
            "backward repeat wraps first → last"
        );
        app.release_physical_press(wid, cmd_r);
        assert_pty_silent_and_close(pipe);
    }

    /// Codex's protected-footer mutation may force a search recompute, but the local
    /// physical owner remains authoritative across the refresh: reverse direction and
    /// last-match selection survive, while Kitty observes no press/repeat/release.
    #[cfg(unix)]
    #[test]
    fn codex_footer_refresh_during_cmd_r_hold_is_directional_and_pty_silent() {
        let _serial = crate::control::search_cap_test_guard();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let rows = app.windows[&wid].rows;
        let pipe = observe_pty(&mut app, wid);
        let term = app.front_terminal(wid).expect("terminal").term.clone();
        term_lock(&term).process(b"\x1b[>2u");
        term_lock(&term).process(
            format!("\x1b[{};1HCODEX_NEEDLE\x1b[{rows};1HCODEX_NEEDLE", rows - 1).as_bytes(),
        );

        let cmd_r = PhysicalKey::Code(KeyCode::KeyR);
        app.terminal_emacs_search_pressed(wid, cmd_r, false);
        app.apply_search_repeat_action(
            wid,
            SearchRepeatAction::Edit(crate::app_search::SearchEdit::Insert("CODEX_NEEDLE".into())),
        );
        assert_eq!(app.windows[&wid].search.as_ref().unwrap().current, 1);

        let region_bottom = rows - 2;
        term_lock(&term).process(
            format!("\x1b[1;{region_bottom}r\x1b[{region_bottom};1H\r\nX\x1b[r").as_bytes(),
        );
        app.search_refresh_for_output(0);
        let refreshed = app.windows[&wid].search.as_ref().unwrap();
        assert_eq!(refreshed.current, refreshed.matches.len() - 1);
        app.route_physical_repeat(wid, cmd_r);
        app.release_physical_press(wid, cmd_r);
        assert_pty_silent_and_close(pipe);
    }

    /// Claude classic and alternate-screen layouts both remain behind the same host
    /// intercept. Each initial Cmd-R query searches only the active grid, captures its
    /// repeat locally, and retires without a Kitty byte.
    #[cfg(unix)]
    #[test]
    fn claude_classic_and_alt_cmd_r_route_active_grid_and_emit_zero_kitty_bytes() {
        let _serial = crate::control::search_cap_test_guard();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let pipe = observe_pty(&mut app, wid);
        let term = app.front_terminal(wid).expect("terminal").term.clone();
        term_lock(&term).process(b"\x1b[>2uCLAUDE_CLASSIC\r\nCLAUDE_CLASSIC");

        let classic_key = PhysicalKey::Code(KeyCode::KeyR);
        app.terminal_emacs_search_pressed(wid, classic_key, false);
        app.apply_search_repeat_action(
            wid,
            SearchRepeatAction::Edit(crate::app_search::SearchEdit::Insert(
                "CLAUDE_CLASSIC".into(),
            )),
        );
        assert_eq!(app.windows[&wid].search.as_ref().unwrap().current, 1);
        app.release_physical_press(wid, classic_key);
        app.search_cancel();

        term_lock(&term).process(b"\x1b[?1049h\x1b[HCLAUDE_ALT\r\nCLAUDE_ALT");
        let alt_key = PhysicalKey::Code(KeyCode::KeyR);
        app.terminal_emacs_search_pressed(wid, alt_key, false);
        app.apply_search_repeat_action(
            wid,
            SearchRepeatAction::Edit(crate::app_search::SearchEdit::Insert("CLAUDE_ALT".into())),
        );
        let alternate = app.windows[&wid].search.as_ref().unwrap();
        assert_eq!(alternate.matches.len(), 2);
        assert_eq!(alternate.current, 1);
        app.route_physical_repeat(wid, alt_key);
        app.release_physical_press(wid, alt_key);
        assert_pty_silent_and_close(pipe);
    }
}

#[cfg(test)]
mod rain_turn_boundary_tests {
    use super::is_plain_enter;
    use crate::input::InputEvent;
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

    fn enter(mods: Modifiers) -> InputEvent {
        InputEvent::Key {
            key: Key::Named(NamedKey::Enter),
            mods,
            base_layout: None,
            event_type: KeyEventType::Press,
        }
    }

    #[test]
    fn only_unmodified_enter_starts_an_agent_turn() {
        assert!(is_plain_enter(&enter(Modifiers::empty())));
        for mods in [
            Modifiers::SHIFT,
            Modifiers::CTRL,
            Modifiers::ALT,
            Modifiers::SUPER,
            Modifiers::SHIFT | Modifiers::CTRL,
        ] {
            assert!(
                !is_plain_enter(&enter(mods)),
                "modified Enter {mods:?} stays an application key"
            );
        }
    }
}

#[cfg(test)]
mod settings_cmd_f_tests {
    use crate::input::{InputEvent, Source};
    use crate::{App, WindowId};
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers};

    fn settings_searching(app: &App) -> bool {
        app.front()
            .and_then(|ws| ws.settings())
            .is_some_and(|s| s.searching)
    }

    /// ⌘F while Settings is open focuses the settings SEARCH exactly like `/`
    /// (design §4.4). The overlay gate swallows every key, so without this arm
    /// the chord is dead. Driven through the engine-neutral input seam (the
    /// controller twin, kept identical to the winit branch).
    #[test]
    fn cmd_f_focuses_settings_search() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.settings_enter();
        assert!(!settings_searching(&app));
        let _ = app.input(
            wid,
            InputEvent::Key {
                key: Key::Character('f'),
                mods: Modifiers::SUPER,
                base_layout: None,
                event_type: KeyEventType::Press,
            },
            Source::Human,
        );
        assert!(settings_searching(&app), "⌘F focuses the settings search");
        // A plain `f` (no ⌘) must NOT re-trigger: leave search, then check.
        app.settings_search_clear();
        assert!(!settings_searching(&app));
        let _ = app.input(
            wid,
            InputEvent::Key {
                key: Key::Character('f'),
                mods: Modifiers::empty(),
                base_layout: None,
                event_type: KeyEventType::Press,
            },
            Source::Human,
        );
        assert!(!settings_searching(&app), "a bare `f` stays a nav no-op");
    }

    /// The macOS Edit ▸ Find… key equivalent fires AHEAD of keyDown and lands in
    /// `find_requested` (Wake::MenuAction → dispatch): with the focused window
    /// showing Settings it must divert to the settings search rather than arm an
    /// invisible, unreachable terminal-find state on the host's session; with
    /// Settings closed it arms terminal find exactly as before.
    #[test]
    fn menu_find_diverts_to_settings_search() {
        let mut app = App::headless_for_test();
        app.settings_enter();
        app.find_requested();
        assert!(
            settings_searching(&app),
            "Find diverts to the settings search"
        );
        assert!(
            app.front().is_some_and(|ws| ws.search.is_none()),
            "no terminal-find state armed under the settings card"
        );
    }

    #[test]
    fn menu_find_arms_terminal_find_when_settings_closed() {
        let mut app = App::headless_for_test();
        app.find_requested();
        assert!(
            app.front().is_some_and(|ws| ws.search.is_some()),
            "without Settings, Find still enters terminal find mode"
        );
    }
}

#[cfg(test)]
mod mouse_cell_clobber_tests {
    use crate::App;
    use crate::WindowId;
    use crate::input::{InputEvent, PixelOffset, Source};
    use aterm_core::selection::SelectionSide;

    /// A `MouseMove` reaching the `App::input` seam must NOT overwrite the
    /// PANE-LOCAL `last_mouse_cell` that `on_cursor_moved` already published. A
    /// follow-up press/wheel (which carries no winit position) reads `last_mouse_cell`,
    /// so clobbering it with the event's window-relative row/col misreports the cell
    /// to a mouse-tracking app in a split — here the window cell is (30, 30) and the
    /// pane-local sentinel that must survive is (5, 7).
    #[test]
    fn mouse_move_does_not_clobber_pane_local_cell() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        // Pretend `on_cursor_moved` already mapped the pointer to pane-local (5, 7).
        app.windows.get_mut(&wid).unwrap().last_mouse_cell = (5, 7);
        // A hover MouseMove whose row/col differ from the published pane-local cell.
        let _ = app.input(
            wid,
            InputEvent::MouseMove {
                buttons: 3,
                row: 30,
                col: 30,
                mods: 0,
                side: SelectionSide::Left,
                px_off: PixelOffset::CELL_ORIGIN,
            },
            Source::Human,
        );
        assert_eq!(
            app.windows.get(&wid).unwrap().last_mouse_cell,
            (5, 7),
            "the seam must not overwrite the pane-local last_mouse_cell"
        );
    }
}

#[cfg(test)]
mod kitty_orphan_release_tests {
    use super::{PhysicalReleaseTrace, take_physical_release_trace};
    use crate::{App, WindowId, term_lock};
    use winit::keyboard::{KeyCode, PhysicalKey};

    fn pairing_step(
        model: &aterm_spec::derive::Model,
        state: &mut aterm_spec::interp::State,
        action: &'static str,
    ) {
        assert!(model.fire(action, state), "{action}: {state:?}");
        for invariant in &model.invariants {
            assert!(
                model.check_invariant(invariant.name, state),
                "shipping tracker violates {} after {action}: {state:?}",
                invariant.name,
            );
        }
    }

    /// A key RELEASE whose PRESS a GUI gate CONSUMED (settings/search mode, a
    /// keybinding `Action`, a Cmd/Super shortcut, a scrollback/pane/zoom chord, an
    /// IME-suppressed key) must be SWALLOWED exactly once — `take_consumed_release`
    /// removes the tracked physical key and returns `true`, so `on_key` returns WITHOUT
    /// reaching the seam encoder (no orphan Kitty `REPORT_EVENT_TYPES` release report
    /// to the PTY). A second release for the same key, or any untracked key, is NOT
    /// swallowed, so a normal key's release still encodes as before.
    #[test]
    fn consumed_press_release_is_swallowed_exactly_once() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let key = PhysicalKey::Code(KeyCode::KeyA);
        let other = PhysicalKey::Code(KeyCode::KeyB);
        let model = aterm_spec::derive::input_release_pairing_model();
        let mut state = model.init_state();
        // No tracked press yet: a normal key's release is forwarded (encodes as today).
        assert!(
            !app.take_consumed_release(wid, key),
            "an untracked release is not swallowed"
        );
        assert!(model.successors("ReleaseConsumedPress", &state).is_empty());
        assert!(model.successors("ReleaseForwardedPress", &state).is_empty());
        // A GUI gate consumed this physical key's PRESS.
        app.note_consumed_press_key(wid, key, false);
        pairing_step(&model, &mut state, "ConsumePhysicalPress");

        // Tier-1 non-vacuity: the removed branch emits a release after a consumed
        // press and violates both byte-silence and orphan-report obligations.
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let mut mutant = state.clone();
        assert!(buggy.fire("ReleaseConsumedPress", &mut mutant));
        assert!(!buggy.check_invariant("NoOrphanCsiUBytes", &mutant));
        // The matching RELEASE is swallowed and its entry removed.
        assert!(
            app.take_consumed_release(wid, key),
            "the matching release is swallowed (no orphan release report)"
        );
        pairing_step(&model, &mut state, "ReleaseConsumedPress");
        assert!(
            app.windows
                .get(&wid)
                .unwrap()
                .consumed_press_keys
                .is_empty(),
            "the entry is removed on the first release"
        );
        // A second release for the SAME key is not swallowed (no stale double-swallow),
        // and an unrelated key is never affected.
        assert!(
            !app.take_consumed_release(wid, key),
            "no stale double-swallow"
        );
        assert!(
            !app.take_consumed_release(wid, other),
            "unrelated key untouched"
        );
    }

    /// A `[key_sequences]` press captures its literal payload and session, repeats
    /// through that immutable owner, then swallows its release exactly once (raw
    /// bytes have no Kitty key-press peer). Release-time chord lookup is forbidden.
    #[test]
    fn sequence_press_release_swallowed_once() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let key = PhysicalKey::Code(KeyCode::KeyP);
        app.forward_literal_press(
            wid,
            key,
            crate::input::InputEvent::KeySequence(b"\x1b[99~".to_vec()),
        );
        assert!(
            app.take_literal_release(wid, key),
            "the matching release is swallowed (no orphan release for the raw-byte press)"
        );
        assert!(
            !app.take_literal_release(wid, key),
            "swallowed exactly once — a later unrelated release encodes as today"
        );
    }

    /// Defensive field-level rule: even if a future caller presents a REPEAT to
    /// the consumed-note seam, it cannot rewrite an existing forwarded episode.
    /// Shipping `on_key` routes repeats before all live GUI gates.
    #[test]
    fn repeat_does_not_note_and_release_is_not_swallowed() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let key = PhysicalKey::Code(KeyCode::PageUp);
        let model = aterm_spec::derive::input_release_pairing_model();
        let mut state = model.init_state();
        pairing_step(&model, &mut state, "ForwardPress");
        // Plain press fell through to the encoder. Any later repeat-note attempt
        // is a no-op, so the app remains owed the matching release.
        app.note_consumed_press_key(wid, key, true);
        app.note_consumed_press_key(wid, key, true);
        assert!(
            app.windows
                .get(&wid)
                .unwrap()
                .consumed_press_keys
                .is_empty(),
            "a repeat never records a consumed press"
        );
        assert!(
            !app.take_consumed_release(wid, key),
            "the release reaches the encoder — the app saw the press, it gets the release"
        );
        pairing_step(&model, &mut state, "ReleaseForwardedPress");
    }

    /// The repeat fall-through swallow PEEKS
    /// (`press_was_consumed`) without removing — a chord broken mid-hold (Shift+PageUp
    /// pressed and consumed, Shift released, PageUp still repeating) has its repeats
    /// swallowed at the egress fall-through, and the eventual RELEASE must still find
    /// the entry to be swallowed itself.
    #[test]
    fn tracked_repeat_swallow_peeks_without_removing() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let key = PhysicalKey::Code(KeyCode::PageUp);
        let model = aterm_spec::derive::input_release_pairing_model();
        let mut state = model.init_state();
        app.note_consumed_press_key(wid, key, false); // the consumed chord press
        pairing_step(&model, &mut state, "ConsumePhysicalPress");
        assert!(
            app.press_was_consumed(wid, key),
            "a fall-through repeat of the consumed press is swallowed"
        );
        pairing_step(&model, &mut state, "RepeatOfConsumedPress");
        assert!(
            app.press_was_consumed(wid, key),
            "the peek does not remove — every repeat of the hold is swallowed"
        );
        pairing_step(&model, &mut state, "RepeatOfConsumedPress");
        assert!(
            app.take_consumed_release(wid, key),
            "the release still finds the entry and is swallowed"
        );
        pairing_step(&model, &mut state, "ReleaseConsumedPress");
        assert!(
            !app.press_was_consumed(wid, key),
            "after the release the key is untracked again"
        );
    }

    /// Focus transfer must preserve press ownership: winit may deliver the matching
    /// RELEASE to the newly focused window, and that release still belongs to the
    /// consumed press in the old window. The process-wide map is authoritative and
    /// removes the old window's diagnostic mirror when the release arrives.
    #[test]
    fn consumed_press_survives_focus_transfer_and_release_in_new_window() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let receiving = WindowId(999);
        let key = PhysicalKey::Code(KeyCode::KeyA);
        app.note_consumed_press_key(wid, key, false);
        app.on_focus(wid, false);
        assert!(
            app.windows
                .get(&wid)
                .unwrap()
                .consumed_press_keys
                .contains(&key),
            "focus loss cannot erase ownership before cross-window release"
        );
        assert!(
            app.take_consumed_release(receiving, key),
            "release delivered to a different window is still swallowed"
        );
        assert!(app.windows[&wid].consumed_press_keys.is_empty());
        assert!(!app.physical_press_owners.contains_key(&key));
    }

    /// A forwarded release is pinned to the press-time session and encoded key
    /// identity. Switching tabs/windows before key-up cannot redirect it to the
    /// newly frontmost PTY or rebuild it from release-time modifiers/layout.
    #[test]
    fn forwarded_press_owner_pins_original_session_across_focus_change() {
        use aterm_types::keyboard::{Key as TKey, Modifiers};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let physical = PhysicalKey::Code(KeyCode::KeyA);
        let original = app.front_terminal(wid).unwrap().session;
        app.note_forwarded_press_key(
            wid,
            physical,
            false,
            TKey::Character('a'),
            Modifiers::SHIFT,
            Some('q'),
        );
        let replacement = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(replacement));
        assert_eq!(app.front_terminal(wid).unwrap().session, replacement);
        match app.physical_press_owners.get(&physical) {
            Some(crate::PhysicalPressOwner::Forwarded {
                window,
                session,
                key,
                mods,
                base_layout,
            }) => {
                assert_eq!(*window, wid);
                assert_eq!(*session, original);
                assert_eq!(*key, TKey::Character('a'));
                assert_eq!(*mods, Modifiers::SHIFT);
                assert_eq!(*base_layout, Some('q'));
            }
            other => panic!("forwarded owner missing or corrupted: {other:?}"),
        }
        app.release_physical_press(WindowId(777), physical);
        assert!(!app.physical_press_owners.contains_key(&physical));
    }

    /// If the platform loses key-up and later reports a fresh key-down for the
    /// same physical key, the new press is proof the old episode ended. Retire
    /// the old forwarded owner through the normal exact-release path before
    /// installing the new disposition; silently replacing the map entry leaves
    /// REPORT_EVENT_TYPES applications with a permanently held key.
    #[test]
    fn fresh_press_closes_stale_forwarded_episode_before_replacement() {
        use aterm_types::keyboard::{Key as TKey, Modifiers};

        let mut app = App::headless_for_test();
        let original_window = WindowId(0);
        let replacement_window = WindowId(777);
        let physical = PhysicalKey::Code(KeyCode::KeyA);
        let original_session = app.front_terminal(original_window).unwrap().session;
        app.note_forwarded_press_key(
            original_window,
            physical,
            false,
            TKey::Character('a'),
            Modifiers::SHIFT,
            Some('a'),
        );

        app.note_consumed_press_key(replacement_window, physical, false);
        assert!(matches!(
            app.physical_press_owners.get(&physical),
            Some(crate::PhysicalPressOwner::Consumed { window })
                if *window == replacement_window
        ));
        assert_eq!(
            take_physical_release_trace(),
            Some(PhysicalReleaseTrace::Forwarded {
                arrival_window: replacement_window,
                press_window: original_window,
                session: original_session,
                key: TKey::Character('a'),
                mods: Modifiers::SHIFT,
                base_layout: Some('a'),
                event_type: aterm_types::keyboard::KeyEventType::Release,
                delivery: crate::input::Delivery::Full,
            }),
            "the stale release uses old identity before new ownership installs"
        );
        assert!(app.take_consumed_release(replacement_window, physical));
    }

    /// A repeat pinned to a now-hidden session must update that terminal only;
    /// it cannot heat or animate the replacement tab that happens to occupy the
    /// press window. This is the presentation half of immutable routing.
    #[test]
    fn hidden_owned_repeat_does_not_arm_replacement_tab_effects() {
        use aterm_types::keyboard::{Key as TKey, Modifiers};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let physical = PhysicalKey::Code(KeyCode::KeyH);
        let original = app.front_terminal(wid).unwrap().session;
        term_lock(&app.pool.get(original).unwrap().term).process(b"\x1b[>2u");
        app.note_forwarded_press_key(
            wid,
            physical,
            false,
            TKey::Character('h'),
            Modifiers::empty(),
            Some('h'),
        );

        let replacement = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(replacement));
        assert_eq!(app.front_terminal(wid).unwrap().session, replacement);
        app.windows.get_mut(&wid).unwrap().input_hot = false;
        assert!(!app.windows[&wid].cursor_cat.is_active());

        app.route_physical_repeat(wid, physical);
        assert!(
            !app.windows[&wid].input_hot,
            "hidden-session repeat must not mark replacement presentation hot"
        );
        assert!(
            !app.windows[&wid].cursor_cat.is_active(),
            "hidden-session repeat must not summon the cat in replacement tab"
        );
        assert_eq!(app.front_terminal(wid).unwrap().session, replacement);
        assert!(app.physical_press_owners.contains_key(&physical));
        let PhysicalReleaseTrace::Forwarded { session, .. } =
            take_physical_release_trace().expect("hidden repeat trace")
        else {
            panic!("encoded owner must stay encoded")
        };
        assert_eq!(session, original, "bytes remain pinned to hidden session A");
    }
}

/// Tier-1 conformance for the process-wide physical press owner. The derived
/// model proves the complete bounded protocol; this module binds the five
/// shipping ownership/routing seams to its `Next` relation while a real
/// two-window `App` changes focus between press, repeat, and release.
#[cfg(test)]
pub(crate) mod input_release_pairing_conformance {
    use std::collections::BTreeMap;

    use aterm_spec::derive::input_release_pairing_model;
    use aterm_types::keyboard::{Key as TKey, Modifiers, NamedKey};
    use winit::keyboard::{KeyCode, PhysicalKey};

    use super::{PhysicalReleaseTrace, clear_physical_release_trace, take_physical_release_trace};
    use crate::input::InputEvent;
    use crate::{App, PhysicalPressOwner, WindowId, stub_session, term_lock};

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Disposition {
        None,
        Consumed,
        Forwarded,
        Literal,
        Local,
    }

    /// Ghost fields that remain observable at the protocol boundary after the
    /// real owner-map entry has been removed. All live-hold fields are projected
    /// directly from `App::physical_press_owners`; only completed-episode history
    /// (which the shipping map intentionally forgets) is retained here.
    #[derive(Clone, Copy, Debug)]
    pub(crate) struct ProjectionWitness {
        phase: i64,
        disposition: Disposition,
        press_window: Option<WindowId>,
        repeat_routed: Option<WindowId>,
        release_arrival: Option<WindowId>,
        release_routed: Option<WindowId>,
        repeat_observed: bool,
        repeat_emitted: bool,
        release_emitted: bool,
    }

    impl ProjectionWitness {
        fn initial() -> Self {
            Self {
                phase: 0,
                disposition: Disposition::None,
                press_window: None,
                repeat_routed: None,
                release_arrival: None,
                release_routed: None,
                repeat_observed: false,
                repeat_emitted: false,
                release_emitted: false,
            }
        }

        fn held(disposition: Disposition, press_window: WindowId) -> Self {
            Self {
                phase: 1,
                disposition,
                press_window: Some(press_window),
                repeat_routed: None,
                release_arrival: None,
                release_routed: None,
                repeat_observed: false,
                repeat_emitted: false,
                release_emitted: false,
            }
        }

        fn repeated(mut self, routed: WindowId) -> Self {
            self.repeat_routed = Some(routed);
            self.repeat_observed = true;
            self.repeat_emitted = true;
            self
        }

        fn silent_repeat(mut self) -> Self {
            self.repeat_observed = true;
            self
        }

        fn untracked_repeat(mut self) -> Self {
            self.repeat_observed = true;
            self
        }

        fn released(mut self, arrival: WindowId, routed: Option<WindowId>, emitted: bool) -> Self {
            self.phase = 2;
            self.release_arrival = Some(arrival);
            self.release_routed = routed;
            self.release_emitted = emitted;
            self
        }
    }

    fn model_window(actual: WindowId, window_a: WindowId, window_b: WindowId) -> i64 {
        if actual == window_a {
            1
        } else if actual == window_b {
            2
        } else {
            panic!("window {actual:?} is outside the two-window projection")
        }
    }

    fn focused_window(app: &App, window_a: WindowId, window_b: WindowId) -> i64 {
        let a = app.windows[&window_a].focused;
        let b = app.windows[&window_b].focused;
        match (a, b) {
            (true, false) => 1,
            (false, true) => 2,
            other => panic!("two-window conformance requires exactly one focus: {other:?}"),
        }
    }

    /// Structural projection named by all ownership `#[refines]` anchors. Owner kind,
    /// original press window, and outstanding-forwarded authority come from the
    /// real process-wide map; focus comes from the two real `WindowState`s.
    pub(crate) fn project(
        app: &App,
        physical: PhysicalKey,
        window_a: WindowId,
        window_b: WindowId,
        witness: ProjectionWitness,
    ) -> aterm_spec::interp::State {
        let (tracker, outstanding) = match app.physical_press_owners.get(&physical) {
            Some(PhysicalPressOwner::Consumed { window }) => {
                assert_eq!(witness.phase, 1, "consumed owner exists only while held");
                assert_eq!(witness.disposition, Disposition::Consumed);
                assert_eq!(Some(*window), witness.press_window);
                (1, 0)
            }
            Some(PhysicalPressOwner::Forwarded { window, .. }) => {
                assert_eq!(witness.phase, 1, "forwarded owner exists only while held");
                assert_eq!(witness.disposition, Disposition::Forwarded);
                assert_eq!(Some(*window), witness.press_window);
                (0, 1)
            }
            Some(PhysicalPressOwner::Literal { window, .. }) => {
                assert_eq!(witness.phase, 1, "literal owner exists only while held");
                assert_eq!(witness.disposition, Disposition::Literal);
                assert_eq!(Some(*window), witness.press_window);
                (3, 0)
            }
            Some(PhysicalPressOwner::Local { window, .. }) => {
                assert_eq!(witness.phase, 1, "local owner exists only while held");
                assert_eq!(witness.disposition, Disposition::Local);
                assert_eq!(Some(*window), witness.press_window);
                (4, 0)
            }
            None => {
                assert_ne!(witness.phase, 1, "held projection lost its real owner");
                (0, 0)
            }
        };
        let press_consumed = i64::from(witness.disposition == Disposition::Consumed);
        let press_forwarded = i64::from(witness.disposition == Disposition::Forwarded);
        let press_literal = i64::from(witness.disposition == Disposition::Literal);
        let press_local = i64::from(witness.disposition == Disposition::Local);
        let map_optional = |window: Option<WindowId>| {
            window.map_or(0, |window| model_window(window, window_a, window_b))
        };
        [
            ("phase", witness.phase),
            ("tracker", tracker),
            ("overlay_open", 0),
            ("press_consumed", press_consumed),
            ("press_forwarded", press_forwarded),
            ("press_literal", press_literal),
            ("press_local", press_local),
            ("pty_press_outstanding", outstanding),
            ("repeat_observed", i64::from(witness.repeat_observed)),
            ("repeat_emitted", i64::from(witness.repeat_emitted)),
            ("release_emitted", i64::from(witness.release_emitted)),
            ("untracked_release_swallowed", 0),
            ("orphan_csi_u", 0),
            ("focused_window", focused_window(app, window_a, window_b)),
            ("press_window", map_optional(witness.press_window)),
            ("repeat_routed_window", map_optional(witness.repeat_routed)),
            (
                "release_arrival_window",
                map_optional(witness.release_arrival),
            ),
            (
                "release_routed_window",
                map_optional(witness.release_routed),
            ),
        ]
        .into_iter()
        .collect::<BTreeMap<_, _>>()
    }

    fn validate_transition(
        action: &'static str,
        prev: &aterm_spec::interp::State,
        next: &aterm_spec::interp::State,
    ) -> (bool, String) {
        aterm_spec::verify::validate_transition_tiered(
            &input_release_pairing_model(),
            &[],
            prev,
            next,
            Some(action),
            "App physical input release pairing conformance",
        )
    }

    fn require_transition(
        action: &'static str,
        prev: &aterm_spec::interp::State,
        next: &aterm_spec::interp::State,
    ) {
        let (ok, out) = validate_transition(action, prev, next);
        assert!(
            ok,
            "real {action} transition must conform:\nprev={prev:?}\nnext={next:?}\n{out}"
        );
    }

    fn set_focus(app: &mut App, window_a: WindowId, window_b: WindowId, focus_a: bool) {
        app.on_focus(window_a, focus_a);
        app.on_focus(window_b, !focus_a);
    }

    fn two_window_app() -> (App, WindowId, WindowId, u64, u64) {
        let mut app = App::headless_for_test();
        let window_a = WindowId(0);
        let session_a = app
            .front_terminal(window_a)
            .expect("window A terminal")
            .session;
        let session_b = app.next_session_id;
        let window_b = app.insert_logical_window(stub_session(session_b), 24, 80);
        assert_ne!(session_a, session_b, "windows must own distinct sessions");
        set_focus(&mut app, window_a, window_b, true);
        (app, window_a, window_b, session_a, session_b)
    }

    #[test]
    fn real_two_window_press_release_routing_conforms() {
        run_conformance();
    }

    pub(crate) fn run_conformance() {
        let model = input_release_pairing_model();

        // A repeat arriving without any observed press epoch is fail-closed.
        let (mut untracked_app, untracked_a, untracked_b, _, _) = two_window_app();
        let untracked_physical = PhysicalKey::Code(KeyCode::KeyZ);
        let untracked_initial = project(
            &untracked_app,
            untracked_physical,
            untracked_a,
            untracked_b,
            ProjectionWitness::initial(),
        );
        clear_physical_release_trace();
        untracked_app.route_physical_repeat(untracked_b, untracked_physical);
        assert_eq!(
            take_physical_release_trace(),
            None,
            "untracked repeat is byte-silent"
        );
        let untracked_silent = project(
            &untracked_app,
            untracked_physical,
            untracked_a,
            untracked_b,
            ProjectionWitness::initial().untracked_repeat(),
        );
        require_transition(
            "SwallowUntrackedRepeat",
            &untracked_initial,
            &untracked_silent,
        );

        // Consumed A press -> focus B -> release delivered through B. The real
        // release branch removes A's owner/mirror and returns before seam egress.
        let (mut app, window_a, window_b, _, _) = two_window_app();
        let physical = PhysicalKey::Code(KeyCode::KeyA);
        let initial_witness = ProjectionWitness::initial();
        let initial = project(&app, physical, window_a, window_b, initial_witness);
        assert_eq!(
            initial,
            model.init_state(),
            "real projection matches model Init"
        );

        app.note_consumed_press_key(window_a, physical, false);
        let consumed_witness = ProjectionWitness::held(Disposition::Consumed, window_a);
        let consumed = project(&app, physical, window_a, window_b, consumed_witness);
        require_transition("ConsumePhysicalPress", &initial, &consumed);

        set_focus(&mut app, window_a, window_b, false);
        let consumed_transferred = project(&app, physical, window_a, window_b, consumed_witness);
        require_transition("TransferFocusWhileHeld", &consumed, &consumed_transferred);

        app.route_physical_repeat(window_b, physical);
        let consumed_repeated_witness = consumed_witness.silent_repeat();
        let consumed_repeated = project(
            &app,
            physical,
            window_a,
            window_b,
            consumed_repeated_witness,
        );
        require_transition(
            "RepeatOfConsumedPress",
            &consumed_transferred,
            &consumed_repeated,
        );

        app.release_physical_press(window_b, physical);
        assert_eq!(
            take_physical_release_trace(),
            Some(PhysicalReleaseTrace::Consumed {
                arrival_window: window_b,
                press_window: window_a,
            }),
            "consumed cross-window release returns before any PTY egress"
        );
        let consumed_released_witness = consumed_repeated_witness.released(window_b, None, false);
        let consumed_released = project(
            &app,
            physical,
            window_a,
            window_b,
            consumed_released_witness,
        );
        require_transition(
            "ReleaseConsumedPress",
            &consumed_repeated,
            &consumed_released,
        );
        assert!(!app.physical_press_owners.contains_key(&physical));
        assert!(
            !app.windows[&window_a]
                .consumed_press_keys
                .contains(&physical)
        );

        // Forwarded A/session-A press -> focus B/session-B -> release through B.
        // Enable Kitty event types on A so the sentinel sink's Failed delivery
        // proves that the exact release was encoded/attempted, not a silent legacy no-op.
        let (mut app, window_a, window_b, session_a, session_b) = two_window_app();
        let physical = PhysicalKey::Code(KeyCode::KeyB);
        let key = TKey::Named(NamedKey::ArrowUp);
        let mods = Modifiers::SHIFT;
        let base_layout = Some('w');
        term_lock(&app.pool.get(session_a).expect("session A").term).process(b"\x1b[>2u");
        let initial = project(
            &app,
            physical,
            window_a,
            window_b,
            ProjectionWitness::initial(),
        );

        app.note_forwarded_press_key(window_a, physical, false, key.clone(), mods, base_layout);
        match app.physical_press_owners.get(&physical) {
            Some(PhysicalPressOwner::Forwarded {
                window,
                session,
                key: stored_key,
                mods: stored_mods,
                base_layout: stored_layout,
            }) => {
                assert_eq!(*window, window_a);
                assert_eq!(*session, session_a);
                assert_eq!(stored_key, &key);
                assert_eq!(*stored_mods, mods);
                assert_eq!(*stored_layout, base_layout);
            }
            other => panic!("real forwarded owner missing/corrupt: {other:?}"),
        }
        let forwarded_witness = ProjectionWitness::held(Disposition::Forwarded, window_a);
        let forwarded = project(&app, physical, window_a, window_b, forwarded_witness);
        require_transition("ForwardPress", &initial, &forwarded);

        set_focus(&mut app, window_a, window_b, false);
        let forwarded_transferred = project(&app, physical, window_a, window_b, forwarded_witness);
        require_transition("TransferFocusWhileHeld", &forwarded, &forwarded_transferred);

        app.route_physical_repeat(window_b, physical);
        let repeat_trace = take_physical_release_trace().expect("forwarded repeat trace");
        let PhysicalReleaseTrace::Forwarded {
            arrival_window,
            press_window,
            session,
            key: routed_key,
            mods: routed_mods,
            base_layout: routed_layout,
            event_type,
            delivery,
        } = repeat_trace
        else {
            panic!("forwarded owner cannot repeat as consumed")
        };
        assert_eq!(arrival_window, window_b);
        assert_eq!(press_window, window_a);
        assert_eq!(session, session_a);
        assert_ne!(session, session_b, "repeat must not route to focused B");
        assert_eq!(routed_key, key);
        assert_eq!(routed_mods, mods);
        assert_eq!(routed_layout, base_layout);
        assert_eq!(event_type, aterm_types::keyboard::KeyEventType::Repeat);
        assert_eq!(
            delivery,
            crate::input::Delivery::Failed,
            "Kitty repeat was encoded against original session A's sentinel sink"
        );
        assert!(
            app.physical_press_owners.contains_key(&physical),
            "repeat must retain ownership for the final release"
        );
        let forwarded_repeated_witness = forwarded_witness.repeated(window_a);
        let forwarded_repeated = project(
            &app,
            physical,
            window_a,
            window_b,
            forwarded_repeated_witness,
        );
        require_transition(
            "ForwardRepeatOfForwardedPress",
            &forwarded_transferred,
            &forwarded_repeated,
        );

        app.release_physical_press(window_b, physical);
        let trace = take_physical_release_trace().expect("forwarded release trace");
        let PhysicalReleaseTrace::Forwarded {
            arrival_window,
            press_window,
            session,
            key: routed_key,
            mods: routed_mods,
            base_layout: routed_layout,
            event_type,
            delivery,
        } = trace
        else {
            panic!("forwarded owner cannot settle as consumed")
        };
        assert_eq!(arrival_window, window_b);
        assert_eq!(press_window, window_a);
        assert_eq!(session, session_a);
        assert_ne!(session, session_b, "release must not route to focused B");
        assert_eq!(routed_key, key);
        assert_eq!(routed_mods, mods);
        assert_eq!(routed_layout, base_layout);
        assert_eq!(event_type, aterm_types::keyboard::KeyEventType::Release);
        assert_eq!(
            delivery,
            crate::input::Delivery::Failed,
            "Kitty release was encoded against original session A's sentinel sink"
        );

        let forwarded_released_witness =
            forwarded_repeated_witness.released(window_b, Some(window_a), true);
        let forwarded_released = project(
            &app,
            physical,
            window_a,
            window_b,
            forwarded_released_witness,
        );
        require_transition(
            "ReleaseForwardedPress",
            &forwarded_repeated,
            &forwarded_released,
        );
        assert!(!app.physical_press_owners.contains_key(&physical));

        // A configured raw sequence has different release semantics (silent),
        // but its repeated bytes obey the same immutable cross-focus target.
        let (mut raw_app, raw_window_a, raw_window_b, raw_session_a, raw_session_b) =
            two_window_app();
        let raw_physical = PhysicalKey::Code(KeyCode::KeyC);
        let raw_bytes = b"\x1b[99~".to_vec();
        let raw_initial = project(
            &raw_app,
            raw_physical,
            raw_window_a,
            raw_window_b,
            ProjectionWitness::initial(),
        );
        raw_app.forward_literal_press(
            raw_window_a,
            raw_physical,
            InputEvent::KeySequence(raw_bytes.clone()),
        );
        assert_eq!(
            take_physical_release_trace(),
            Some(PhysicalReleaseTrace::Literal {
                arrival_window: raw_window_a,
                press_window: raw_window_a,
                session: raw_session_a,
                event: InputEvent::KeySequence(raw_bytes.clone()),
                repeated: false,
                delivery: crate::input::Delivery::Failed,
            }),
            "initial literal sequence is pinned to session A"
        );
        let raw_witness = ProjectionWitness::held(Disposition::Literal, raw_window_a);
        let raw_held = project(
            &raw_app,
            raw_physical,
            raw_window_a,
            raw_window_b,
            raw_witness,
        );
        require_transition("ForwardLiteralPress", &raw_initial, &raw_held);

        set_focus(&mut raw_app, raw_window_a, raw_window_b, false);
        let raw_transferred = project(
            &raw_app,
            raw_physical,
            raw_window_a,
            raw_window_b,
            raw_witness,
        );
        require_transition("TransferFocusWhileHeld", &raw_held, &raw_transferred);
        raw_app.route_physical_repeat(raw_window_b, raw_physical);
        assert_eq!(
            take_physical_release_trace(),
            Some(PhysicalReleaseTrace::Literal {
                arrival_window: raw_window_b,
                press_window: raw_window_a,
                session: raw_session_a,
                event: InputEvent::KeySequence(raw_bytes),
                repeated: true,
                delivery: crate::input::Delivery::Failed,
            }),
            "raw repeat must use original session/bytes"
        );
        assert_ne!(
            raw_session_a, raw_session_b,
            "raw test needs distinct destinations"
        );
        let raw_repeated_witness = raw_witness.repeated(raw_window_a);
        let raw_repeated = project(
            &raw_app,
            raw_physical,
            raw_window_a,
            raw_window_b,
            raw_repeated_witness,
        );
        require_transition(
            "ForwardRepeatOfLiteralPress",
            &raw_transferred,
            &raw_repeated,
        );
        assert!(raw_app.take_literal_release(raw_window_b, raw_physical));
        assert_eq!(
            take_physical_release_trace(),
            Some(PhysicalReleaseTrace::Consumed {
                arrival_window: raw_window_b,
                press_window: raw_window_a,
            }),
            "literal sequence release is byte-silent"
        );
        let raw_released_witness = raw_repeated_witness.released(raw_window_b, None, false);
        let raw_released = project(
            &raw_app,
            raw_physical,
            raw_window_a,
            raw_window_b,
            raw_released_witness,
        );
        require_transition("ReleaseLiteralPress", &raw_repeated, &raw_released);

        // Repeatable GUI actions are also bound to their press window. A real
        // font-zoom hold crosses focus A→B without becoming B's key episode.
        let (mut local_app, local_a, local_b, _, _) = two_window_app();
        let local_physical = PhysicalKey::Code(KeyCode::KeyD);
        let local_initial = project(
            &local_app,
            local_physical,
            local_a,
            local_b,
            ProjectionWitness::initial(),
        );
        local_app.note_local_repeat_press(
            local_a,
            local_physical,
            crate::LocalRepeatAction::FontZoom(crate::FontZoomRepeatAction::Increase),
        );
        let local_witness = ProjectionWitness::held(Disposition::Local, local_a);
        let local_held = project(&local_app, local_physical, local_a, local_b, local_witness);
        require_transition("CaptureLocalRepeatPress", &local_initial, &local_held);
        set_focus(&mut local_app, local_a, local_b, false);
        let local_transferred =
            project(&local_app, local_physical, local_a, local_b, local_witness);
        require_transition("TransferFocusWhileHeld", &local_held, &local_transferred);
        let font_before = local_app.font_px;
        local_app.route_physical_repeat(local_b, local_physical);
        assert!(
            local_app.font_px > font_before,
            "captured zoom repeat applied"
        );
        assert_eq!(
            take_physical_release_trace(),
            Some(PhysicalReleaseTrace::Local {
                arrival_window: local_b,
                press_window: local_a,
            })
        );
        let local_repeated_witness = local_witness.repeated(local_a);
        let local_repeated = project(
            &local_app,
            local_physical,
            local_a,
            local_b,
            local_repeated_witness,
        );
        require_transition("ForwardLocalRepeat", &local_transferred, &local_repeated);
        assert!(local_app.take_local_repeat_release(local_b, local_physical));
        let local_released_witness = local_repeated_witness.released(local_b, None, false);
        let local_released = project(
            &local_app,
            local_physical,
            local_a,
            local_b,
            local_released_witness,
        );
        require_transition("ReleaseLocalRepeatPress", &local_repeated, &local_released);

        // NON-VACUITY: the historical resolver sends both repeats and release
        // to current focus B. Each mutant must violate its target invariant and
        // be rejected as a transition by the healthy derived model used above.
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let orphan_repeat = buggy
            .successors("SwallowUntrackedRepeat", &untracked_initial)
            .into_iter()
            .next()
            .expect("Buggy untracked repeat transition");
        assert_eq!(orphan_repeat["repeat_emitted"], 1);
        assert!(!buggy.check_invariant("NoOrphanCsiUBytes", &orphan_repeat));
        let (accepted, out) =
            validate_transition("SwallowUntrackedRepeat", &untracked_initial, &orphan_repeat);
        assert!(
            !accepted,
            "NEGATIVE CONTROL: orphan untracked repeat must be rejected\n{out}"
        );

        let repeat_misrouted = buggy
            .successors("ForwardRepeatOfForwardedPress", &forwarded_transferred)
            .into_iter()
            .next()
            .expect("Buggy cross-focus repeat transition");
        assert_eq!(repeat_misrouted["press_window"], 1);
        assert_eq!(repeat_misrouted["repeat_routed_window"], 2);
        assert!(!buggy.check_invariant("EmittedRepeatUsesOriginalPressTarget", &repeat_misrouted,));
        let (accepted, out) = validate_transition(
            "ForwardRepeatOfForwardedPress",
            &forwarded_transferred,
            &repeat_misrouted,
        );
        assert!(
            !accepted,
            "NEGATIVE CONTROL: current-focus repeat route must be rejected\n{out}"
        );

        let raw_repeat_misrouted = buggy
            .successors("ForwardRepeatOfLiteralPress", &raw_transferred)
            .into_iter()
            .next()
            .expect("Buggy cross-focus raw repeat transition");
        assert_eq!(raw_repeat_misrouted["press_window"], 1);
        assert_eq!(raw_repeat_misrouted["repeat_routed_window"], 2);
        assert!(!buggy.check_invariant(
            "EmittedRepeatUsesOriginalPressTarget",
            &raw_repeat_misrouted,
        ));
        let (accepted, out) = validate_transition(
            "ForwardRepeatOfLiteralPress",
            &raw_transferred,
            &raw_repeat_misrouted,
        );
        assert!(
            !accepted,
            "NEGATIVE CONTROL: current-focus raw repeat route must be rejected\n{out}"
        );

        let local_repeat_misrouted = buggy
            .successors("ForwardLocalRepeat", &local_transferred)
            .into_iter()
            .next()
            .expect("Buggy cross-focus local repeat transition");
        assert_eq!(local_repeat_misrouted["press_window"], 1);
        assert_eq!(local_repeat_misrouted["repeat_routed_window"], 2);
        assert!(!buggy.check_invariant(
            "EmittedRepeatUsesOriginalPressTarget",
            &local_repeat_misrouted,
        ));
        let (accepted, out) = validate_transition(
            "ForwardLocalRepeat",
            &local_transferred,
            &local_repeat_misrouted,
        );
        assert!(
            !accepted,
            "NEGATIVE CONTROL: current-focus local repeat route must be rejected\n{out}"
        );

        let misrouted = buggy
            .successors("ReleaseForwardedPress", &forwarded_repeated)
            .into_iter()
            .next()
            .expect("Buggy cross-focus release transition");
        assert_eq!(misrouted["press_window"], 1);
        assert_eq!(misrouted["release_arrival_window"], 2);
        assert_eq!(misrouted["release_routed_window"], 2);
        assert!(!buggy.check_invariant("ForwardedReleaseUsesOriginalPressTarget", &misrouted,));
        let (accepted, out) =
            validate_transition("ReleaseForwardedPress", &forwarded_repeated, &misrouted);
        assert!(
            !accepted,
            "NEGATIVE CONTROL: current-focus release route must be rejected\n{out}"
        );

        eprintln!(
            "InputReleasePairing Tier-1: consumed, encoded, literal, and local A→focus-B \
             repeat/release routes conform; original identities preserved; mutants rejected."
        );
    }

    // The active machine has 21 actions. Twelve ownership/routing seams above carry
    // real `#[refines]` anchors. These nine actions are explicit scope boundaries,
    // not silent coverage holes.
    #[allow(dead_code)]
    #[aterm_spec::spec_unmodeled(
        machine = "input_release_pairing",
        action = "TransferFocusWhileHeld",
        reason = "OS/winit environment transition: Tier-1 drives real App::on_focus(A,false) then \
                  App::on_focus(B,true) and validates that physical ownership is unchanged; no \
                  key-owner mutator implements the focus arrival itself."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "input_release_pairing",
        action = "OpenOverlay",
        reason = "Orthogonal overlay environment step; cross-window physical-owner conformance \
                  fixes overlays closed, while overlay_gate_pairing_tests drives the real open."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "input_release_pairing",
        action = "CloseOverlay",
        reason = "Orthogonal overlay environment step; cross-window physical-owner conformance \
                  fixes overlays closed, while overlay_gate_pairing_tests drives the real close."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "input_release_pairing",
        action = "ConsumeOverlayPress",
        reason = "Controller/engine-key overlay ownership uses overlay_consumed_keys, not the \
                  process-wide winit PhysicalKey owner map bound by this Tier-1 runner."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "input_release_pairing",
        action = "RepeatOfConsumedPress",
        reason = "Repeat is a read-only peek of the retained physical owner; dedicated shipping \
                  tests prove it neither removes the owner nor emits bytes."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "input_release_pairing",
        action = "GateConsumesRepeatOfForwardedPress",
        reason = "Controller/engine-key repeats enter App::input without a winit PhysicalKey; \
                  overlay_gate_pairing_tests drives the real pre-overlay press/open/repeat/release \
                  seam. Physical repeats bypass current-content gates through route_physical_repeat."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "input_release_pairing",
        action = "PhysicalFocusLoss",
        reason = "Terminal OS epoch where no matching key-up is delivered. A focus transfer that \
                  does deliver key-up is the separately validated TransferFocusWhileHeld action."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "input_release_pairing",
        action = "SettledRelease",
        reason = "Abstract terminal stutter after owner removal; no shipping state mutation exists."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "input_release_pairing",
        action = "SettledFocusEpoch",
        reason = "Abstract terminal stutter after an OS-cancelled epoch; no shipping mutation exists."
    )]
    fn explicit_scope_waivers() {}
}

#[cfg(test)]
mod overlay_gate_pairing_tests {
    //! The `App::input` overlay gate must preserve Kitty press/release
    //! pairing across overlay open/close boundaries. The observable is the seam's
    //! reply on the stub session's INVALID (-1) sink with Kitty REPORT_EVENT_TYPES
    //! (`CSI > 2 u`) negotiated: an event the seam ENCODES + writes reports
    //! `WriteFailed` (the write can't land), while a SWALLOWED event returns `Ok`
    //! without touching the sink — so the two dispositions are distinguishable
    //! without a readable PTY. `Source` is audit-only (the seam never branches on
    //! it), so driving with `Source::Human` exercises the identical gate a
    //! controller event hits.
    use crate::input::{InputEvent, InputOutcome, Source};
    use crate::{App, WindowId, term_lock};
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

    fn named(key: NamedKey, event_type: KeyEventType) -> InputEvent {
        InputEvent::Key {
            key: Key::Named(key),
            mods: Modifiers::empty(),
            base_layout: None,
            event_type,
        }
    }

    fn pairing_step(
        model: &aterm_spec::derive::Model,
        state: &mut aterm_spec::interp::State,
        action: &'static str,
    ) {
        assert!(model.fire(action, state), "{action}: {state:?}");
        for invariant in &model.invariants {
            assert!(
                model.check_invariant(invariant.name, state),
                "shipping overlay gate violates {} after {action}: {state:?}",
                invariant.name,
            );
        }
    }
    /// An app that negotiated Kitty event-type reporting. Functional-key RELEASES
    /// encode bytes (and a swallow is observable as `Ok` vs `WriteFailed`); plain
    /// text releases intentionally stay silent unless REPORT_ALL_KEYS is active.
    fn app_with_event_types() -> App {
        let app = App::headless_for_test();
        let term = app
            .front_terminal(WindowId(0))
            .expect("front terminal")
            .term
            .clone();
        term_lock(&term).process(b"\x1b[>2u");
        app
    }

    /// A RELEASE whose press PREDATES the overlay (press delivered to the PTY, then
    /// the overlay opened mid-hold via menu click / aterm-ctl) must FALL THROUGH the
    /// gate to the seam encoder — swallowing it would leave the app an orphan press,
    /// and buys nothing (every overlay handler ignores releases).
    #[test]
    fn release_of_pre_overlay_press_falls_through_the_gate() {
        let mut app = app_with_event_types();
        let wid = WindowId(0);
        let model = aterm_spec::derive::input_release_pairing_model();
        let mut state = model.init_state();
        // The press reached the seam (WriteFailed on the -1 sink proves it encoded).
        assert_eq!(
            app.input(
                wid,
                named(NamedKey::ArrowUp, KeyEventType::Press),
                Source::Human,
            ),
            InputOutcome::WriteFailed,
            "no overlay: the press reaches the PTY"
        );
        pairing_step(&model, &mut state, "ForwardPress");
        app.settings_enter();
        pairing_step(&model, &mut state, "OpenOverlay");
        assert!(app.windows.get(&wid).unwrap().overlay_open());

        // Tier-1 negative control for the exact old decision: swallowing this
        // UNTRACKED release under the now-open overlay strands the press already
        // observed by the Kitty app.
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let mut swallowed = state.clone();
        assert!(buggy.fire("ReleaseForwardedPress", &mut swallowed));
        assert!(!buggy.check_invariant("UntrackedReleaseNeverSwallowed", &swallowed,));
        // The in-flight release must still reach the seam (encode + fail), not be
        // swallowed to Ok by the gate.
        assert_eq!(
            app.input(
                wid,
                named(NamedKey::ArrowUp, KeyEventType::Release),
                Source::Human,
            ),
            InputOutcome::WriteFailed,
            "the release pairs with the pre-overlay press: it falls through to the encoder"
        );
        pairing_step(&model, &mut state, "ReleaseForwardedPress");
        assert!(
            app.windows.get(&wid).unwrap().overlay_open(),
            "the fall-through never closes/steals the overlay"
        );
    }

    /// A (controller) PRESS consumed by the overlay gate is recorded, and its RELEASE
    /// arriving AFTER the overlay closed is swallowed exactly once — otherwise it
    /// encodes an orphan Kitty release report for a press the app never saw.
    #[test]
    fn press_under_overlay_release_swallowed_after_close() {
        let mut app = app_with_event_types();
        let wid = WindowId(0);
        let model = aterm_spec::derive::input_release_pairing_model();
        let mut state = model.init_state();
        app.settings_enter();
        pairing_step(&model, &mut state, "OpenOverlay");
        assert_eq!(
            app.input(
                wid,
                named(NamedKey::ArrowRight, KeyEventType::Press),
                Source::Human,
            ),
            InputOutcome::Ok,
            "the overlay consumes the press (routed, not encoded)"
        );
        pairing_step(&model, &mut state, "ConsumeOverlayPress");
        assert!(
            app.windows
                .get(&wid)
                .unwrap()
                .overlay_consumed_keys
                .contains(&Key::Named(NamedKey::ArrowRight)),
            "the consumed press is recorded at the seam"
        );
        app.settings_exit();
        pairing_step(&model, &mut state, "CloseOverlay");
        assert!(!app.windows.get(&wid).unwrap().overlay_open());
        assert_eq!(
            app.input(
                wid,
                named(NamedKey::ArrowRight, KeyEventType::Release),
                Source::Human,
            ),
            InputOutcome::Ok,
            "the release is swallowed even though the overlay already closed"
        );
        pairing_step(&model, &mut state, "ReleaseConsumedPress");
        assert!(
            app.windows
                .get(&wid)
                .unwrap()
                .overlay_consumed_keys
                .is_empty(),
            "swallow-once: the entry is removed"
        );
        // A later unrelated release of the same key encodes as today (reaches the
        // seam and fails on the stub sink) — no stale double-swallow.
        assert_eq!(
            app.input(
                wid,
                named(NamedKey::ArrowRight, KeyEventType::Release),
                Source::Human,
            ),
            InputOutcome::WriteFailed
        );
    }

    /// The same pairing while the overlay STAYS open: press consumed + recorded,
    /// release swallowed + entry removed. And a REPEAT under the overlay must NOT
    /// record — a repeat of a pre-overlay press (overlay opened mid-hold) poisoning
    /// the set would swallow a release the app is owed.
    #[test]
    fn overlay_repeat_does_not_record_and_open_overlay_release_pairs() {
        let mut app = app_with_event_types();
        let wid = WindowId(0);
        let model = aterm_spec::derive::input_release_pairing_model();
        let mut pre_overlay_hold = model.init_state();
        pairing_step(&model, &mut pre_overlay_hold, "ForwardPress");
        app.settings_enter();
        pairing_step(&model, &mut pre_overlay_hold, "OpenOverlay");
        // Repeat routed to the overlay: swallowed but NOT recorded.
        assert_eq!(
            app.input(
                wid,
                named(NamedKey::ArrowUp, KeyEventType::Repeat),
                Source::Human,
            ),
            InputOutcome::Ok
        );
        pairing_step(
            &model,
            &mut pre_overlay_hold,
            "GateConsumesRepeatOfForwardedPress",
        );
        assert!(
            app.windows
                .get(&wid)
                .unwrap()
                .overlay_consumed_keys
                .is_empty(),
            "a repeat never records a consumed press at the gate"
        );
        // Its release (press predated the overlay) falls through to the encoder.
        assert_eq!(
            app.input(
                wid,
                named(NamedKey::ArrowUp, KeyEventType::Release),
                Source::Human,
            ),
            InputOutcome::WriteFailed,
            "the repeat did not poison the release disposition"
        );
        pairing_step(&model, &mut pre_overlay_hold, "ReleaseForwardedPress");
        // Press+release both under the open overlay: consumed, then swallowed.
        let mut under_overlay = model.init_state();
        pairing_step(&model, &mut under_overlay, "OpenOverlay");
        assert_eq!(
            app.input(
                wid,
                named(NamedKey::ArrowDown, KeyEventType::Press),
                Source::Human,
            ),
            InputOutcome::Ok
        );
        pairing_step(&model, &mut under_overlay, "ConsumeOverlayPress");
        assert_eq!(
            app.input(
                wid,
                named(NamedKey::ArrowDown, KeyEventType::Release),
                Source::Human,
            ),
            InputOutcome::Ok,
            "an overlay-consumed press has its release swallowed while the overlay is open"
        );
        pairing_step(&model, &mut under_overlay, "ReleaseConsumedPress");
        assert!(
            app.windows
                .get(&wid)
                .unwrap()
                .overlay_consumed_keys
                .is_empty(),
            "the release removed its entry (leak-free)"
        );
    }
}

#[cfg(test)]
mod smooth_scroll_tests {
    use crate::{App, WindowId, term_lock};
    use std::time::Duration;

    /// Seed the window's engine with enough output that scrollback exists.
    fn seed_history(
        app: &App,
        wid: WindowId,
    ) -> std::sync::Arc<std::sync::Mutex<aterm_core::terminal::Terminal>> {
        let term = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        {
            let mut t = term_lock(&term);
            for i in 0..60 {
                t.process(format!("line {i}\r\n").as_bytes());
            }
            assert!(
                t.grid().scrollback_lines() >= 10,
                "test needs real scrollback"
            );
        }
        term
    }

    /// W12 mixed-DPI authority: wheel easing and the terminal's pixel-cell
    /// contract must read the target window's metric record even while the
    /// shared renderer is activated at another size.  The explicit inequality is
    /// the negative control for the historical `self.cell_size()` shortcut.
    #[test]
    fn wheel_and_terminal_pixel_geometry_use_the_target_windows_cell_size() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let shared = app.cell_size();
        app.windows.get_mut(&wid).unwrap().metrics.font_px += 18.0;
        let owned = app.win_cell_size(wid);
        assert_ne!(
            owned, shared,
            "fixture must keep the shared renderer at another window's geometry"
        );

        let term = seed_history(&app, wid);
        app.scroll_wheel_animated(wid, &term, 3);
        let glide = app.windows[&wid]
            .scroll_glide
            .as_ref()
            .expect("full motion arms the per-window glide");
        assert_eq!(glide.cell_h, owned.1 as i64);
        assert_eq!(glide.glide.target_px(), 3 * owned.1 as i64);
        assert_ne!(
            glide.glide.target_px(),
            3 * shared.1 as i64,
            "the old shared-renderer projection must be observably different"
        );

        // The no-op grid-size case is deliberate: apply_term_resize must refresh
        // the engine's pixel-cell contract even when rows/cols are unchanged.
        let (rows, cols) = {
            let ws = &app.windows[&wid];
            (ws.rows, ws.cols)
        };
        assert!(!app.apply_term_resize(wid, rows, cols));
        assert_eq!(
            term_lock(&term).cell_pixel_size(),
            (owned.0 as u16, owned.1 as u16)
        );
        assert_ne!(
            term_lock(&term).cell_pixel_size(),
            (shared.0 as u16, shared.1 as u16)
        );
    }

    /// M1(b) regression — the wheel's tracking-OFF fallback GLIDES under a Full
    /// motion policy: the notch arms a self-disarming ease (no instant jump),
    /// chained notches RETARGET the same ease, the final tick lands the
    /// viewport EXACTLY on the target row, and the state is dropped (no armed
    /// deadline — the ty-model `ScrollGlide` discipline on the real seam).
    #[test]
    fn wheel_scroll_glides_and_lands_exactly() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = seed_history(&app, wid);
        let cell_h = app.cell_size().1.max(1) as i64;

        // Default headless policy: auto + OS flag off + focused ⇒ Full ⇒ glide.
        app.scroll_wheel_animated(wid, &term, 5);
        {
            let ws = app.windows.get(&wid).unwrap();
            let st = ws.scroll_glide.as_ref().expect("Full policy arms a glide");
            assert_eq!(st.glide.target_px(), 5 * cell_h, "target = 5 rows in px");
            assert_eq!(
                term_lock(&term).grid().display_offset(),
                0,
                "no instant jump — the ease moves the viewport"
            );
        }
        // A chained notch retargets the SAME ease (2 more rows into history).
        app.scroll_wheel_animated(wid, &term, 2);
        let end = {
            let ws = app.windows.get(&wid).unwrap();
            let st = ws.scroll_glide.as_ref().expect("still armed");
            assert_eq!(st.glide.target_px(), 7 * cell_h, "retargeted to 7 rows");
            st.glide.end()
        };
        // Mid-flight tick: the viewport may sit anywhere on the path, but the
        // glide is still armed (no premature disarm).
        app.tick_scroll_glide(wid, end - Duration::from_millis(60));
        assert!(
            app.windows.get(&wid).unwrap().scroll_glide.is_some(),
            "mid-flight tick keeps the glide armed"
        );
        // The tick at/after the end LANDS EXACTLY and self-disarms.
        app.tick_scroll_glide(wid, end + Duration::from_millis(1));
        assert_eq!(
            term_lock(&term).grid().display_offset(),
            7,
            "the ease lands exactly on its target row"
        );
        assert!(
            app.windows.get(&wid).unwrap().scroll_glide.is_none(),
            "self-disarm: the state is dropped at the end (no perpetual wake)"
        );
        // The pill woke with the gesture.
        assert!(
            app.windows
                .get(&wid)
                .unwrap()
                .scroll_pill
                .is_active(std::time::Instant::now(), true),
            "scroll activity shows the pill"
        );
    }

    /// M1 + W11 OS-edge regression — a live Reduce Motion change settles an
    /// in-flight Full-policy glide immediately at its intended target, clears
    /// its deadline/residual, and a later wheel notch snaps from that landed row.
    #[test]
    fn reduced_motion_wheel_snaps_instantly() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = seed_history(&app, wid);

        // Arm a glide under the default Full policy…
        app.scroll_wheel_animated(wid, &term, 4);
        assert!(app.windows.get(&wid).unwrap().scroll_glide.is_some());
        // …then the OS Reduce Motion flag flips (Wake::ReduceMotionChanged).
        assert!(app.apply_system_reduce_motion(true, std::time::Instant::now()));
        assert_eq!(
            term_lock(&term).grid().display_offset(),
            4,
            "the policy edge itself lands the retained target"
        );
        assert!(app.windows.get(&wid).unwrap().scroll_glide.is_none());
        assert_eq!(app.windows.get(&wid).unwrap().scroll_frac_px, 0);

        app.scroll_wheel_animated(wid, &term, 3);
        assert!(
            app.windows.get(&wid).unwrap().scroll_glide.is_none(),
            "Reduced motion never re-arms"
        );
        assert_eq!(
            term_lock(&term).grid().display_offset(),
            7,
            "the Reduced notch applies instantly from the settled target"
        );
    }

    /// Defensive tick convergence: even if a policy fact is installed without its
    /// normal edge reducer, the next due tick must jump to the intended target and
    /// disarm. Sampling the old ease at this early timestamp would leave both an
    /// intermediate row and another deadline.
    #[test]
    fn reduced_motion_tick_defensively_lands_instead_of_sampling() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = seed_history(&app, wid);
        app.scroll_wheel_animated(wid, &term, 4);
        let end = app.windows[&wid]
            .scroll_glide
            .as_ref()
            .expect("Full motion arms a glide")
            .glide
            .end();

        // Intentionally bypass `apply_system_reduce_motion` to exercise the
        // tick's defensive convergence path in isolation.
        app.system_reduce_motion = true;
        app.tick_scroll_glide(wid, end - Duration::from_millis(150));
        assert_eq!(
            term_lock(&term).grid().display_offset(),
            4,
            "Reduced tick lands at target rather than sampling an early row"
        );
        assert!(app.windows[&wid].scroll_glide.is_none());
        assert_eq!(app.windows[&wid].scroll_frac_px, 0);
    }

    /// Tier-1 binding for `scroll_glide_model::SetReduced`: a config
    /// Full→Reduced edge on the shipping App reducer projects to the model's
    /// atomic `{pos=target, armed=0, reduced=1}` successor. The cancel-only
    /// mutant is an explicit negative control: dropping the state at the old row
    /// violates `ReducedSettled`, so this test cannot pass vacuously.
    #[test]
    fn reduced_motion_settle_conforms_to_scroll_glide_model() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = seed_history(&app, wid);
        app.config.motion = Some("full".into());
        app.scroll_wheel_animated(wid, &term, 3);

        let model = aterm_spec::derive::scroll_glide_model();
        let mut before = model.init_state();
        before.insert("pos", term_lock(&term).grid().display_offset() as i64);
        before.insert("target", 3);
        before.insert("armed", 1);
        before.insert("wakes", 0);
        before.insert("reduced", 0);
        assert!(model.action_enabled("SetReduced", &before));
        let expected = model
            .successors("SetReduced", &before)
            .into_iter()
            .next()
            .expect("SetReduced has one deterministic successor");

        let mut cancel_only = before.clone();
        cancel_only.insert("armed", 0);
        cancel_only.insert("reduced", 1);
        assert!(
            !model.check_invariant("ReducedSettled", &cancel_only),
            "negative control: cancel-without-landing must be rejected"
        );

        // This is the exact reconciliation invoked immediately after config
        // generation admission in `apply_prepared_config_generation_unfenced`.
        app.config.motion = Some("reduced".into());
        assert!(app.settle_scroll_motion_if_reduced(wid, std::time::Instant::now()));
        let mut actual = before;
        actual.insert("pos", term_lock(&term).grid().display_offset() as i64);
        actual.insert("armed", i64::from(app.windows[&wid].scroll_glide.is_some()));
        actual.insert("reduced", 1);
        assert_eq!(actual, expected, "shipping reducer must match SetReduced");
        assert!(model.check_invariant("DisarmedAtTarget", &actual));
        assert!(model.check_invariant("ReducedSettled", &actual));
        assert_eq!(app.windows[&wid].scroll_frac_px, 0);
    }

    /// M1 regression — typing's snap-to-bottom stays INSTANT and cancels an
    /// in-flight glide, so the eased tail cannot scroll the viewport back away
    /// from the prompt after a keystroke.
    #[test]
    fn typing_snap_cancels_glide_tail() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = seed_history(&app, wid);
        // Scrolled into history with a further glide in flight.
        term_lock(&term).scroll_display(5);
        app.scroll_wheel_animated(wid, &term, 3);
        assert!(app.windows.get(&wid).unwrap().scroll_glide.is_some());

        app.snap_to_bottom(wid);
        assert_eq!(
            term_lock(&term).grid().display_offset(),
            0,
            "keyboard input still snaps instantly"
        );
        assert!(
            app.windows.get(&wid).unwrap().scroll_glide.is_none(),
            "the glide tail is cancelled — nothing re-scrolls after the snap"
        );
    }

    /// M1b regression — the glide tick BANKS a sub-row residual (`scroll_frac_px`)
    /// so scrolling glides by the pixel, not the whole row. Across the ease into
    /// history: (1) the residual is always in `[0, cell_h)`; (2) the shift-up
    /// pairing holds — when the residual is nonzero the engine sits at the CEIL
    /// offset and the reconstructed position `offset*cell_h - frac` stays a valid
    /// eased point in `[0, target]` and is monotone non-decreasing; (3) some tick
    /// genuinely banks a NONZERO residual (sub-row motion, non-vacuity); and
    /// (4) the final tick lands on the whole-row target with the residual back at 0.
    #[test]
    fn glide_banks_sub_row_residual_and_lands_whole_row() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = seed_history(&app, wid);
        let cell_h = app.cell_size().1.max(1) as i64;

        app.scroll_wheel_animated(wid, &term, 5); // ease 0 → 5 rows into history
        let end = app
            .windows
            .get(&wid)
            .unwrap()
            .scroll_glide
            .as_ref()
            .expect("Full policy arms a glide")
            .glide
            .end();

        let mut saw_frac = false;
        let mut prev_pos = 0i64;
        // Sample the whole ease on a fine cadence.
        for ms in (0..=200).step_by(7) {
            app.tick_scroll_glide(
                wid,
                end - Duration::from_millis(200) + Duration::from_millis(ms),
            );
            let ws = app.windows.get(&wid).unwrap();
            let frac = i64::from(ws.scroll_frac_px);
            let offset = term_lock(&term).grid().display_offset() as i64;
            assert!(
                (0..cell_h).contains(&frac),
                "residual out of [0,cell_h): {frac}"
            );
            // The shift-up pairing: content position = engine offset shifted UP by
            // frac. It must stay a valid eased point and never retreat.
            let pos = offset * cell_h - frac;
            assert!(
                (0..=5 * cell_h).contains(&pos),
                "reconstructed pos {pos} out of range"
            );
            assert!(
                pos >= prev_pos,
                "position must not retreat ({prev_pos} -> {pos})"
            );
            prev_pos = pos;
            if frac > 0 {
                saw_frac = true;
                assert!(offset >= 1, "a nonzero residual sits at the CEIL offset");
            }
            if ws.scroll_glide.is_none() {
                break;
            }
        }
        assert!(
            saw_frac,
            "non-vacuity: the glide must bank a nonzero sub-row residual"
        );
        assert_eq!(
            term_lock(&term).grid().display_offset(),
            5,
            "the ease lands on the whole-row target"
        );
        assert_eq!(
            app.windows.get(&wid).unwrap().scroll_frac_px,
            0,
            "a landed glide clears the residual (whole-row rest)"
        );
    }

    /// M1b regression — every INSTANT snap clears the banked residual so the frame
    /// is whole-row: Reduced motion (SmoothScroll proven-zero), typing's
    /// snap-to-bottom, and the controller/keyboard `ScrollView` verb path all
    /// force `scroll_frac_px == 0`.
    #[test]
    fn instant_snaps_clear_the_sub_row_residual() {
        use crate::input::{InputEvent, ScrollIntent, Source};
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = seed_history(&app, wid);

        // Bank a residual via a mid-glide tick, then a Reduced-motion notch snaps.
        app.scroll_wheel_animated(wid, &term, 5);
        let end = app
            .windows
            .get(&wid)
            .unwrap()
            .scroll_glide
            .as_ref()
            .unwrap()
            .glide
            .end();
        app.tick_scroll_glide(wid, end - Duration::from_millis(90));
        app.system_reduce_motion = true;
        app.scroll_wheel_animated(wid, &term, 2);
        assert_eq!(
            app.windows.get(&wid).unwrap().scroll_frac_px,
            0,
            "Reduced motion snaps whole-row — no residual"
        );

        // Re-bank under Full motion, then typing snaps to bottom.
        app.system_reduce_motion = false;
        app.scroll_wheel_animated(wid, &term, 5);
        let end = app
            .windows
            .get(&wid)
            .unwrap()
            .scroll_glide
            .as_ref()
            .unwrap()
            .glide
            .end();
        app.tick_scroll_glide(wid, end - Duration::from_millis(90));
        app.snap_to_bottom(wid);
        assert_eq!(
            app.windows.get(&wid).unwrap().scroll_frac_px,
            0,
            "typing's snap-to-bottom clears the residual"
        );

        // Re-bank, then an instant ScrollView verb (controller/keyboard) clears it —
        // the applied-offset reply contract stays whole-row (source-blind).
        app.scroll_wheel_animated(wid, &term, 5);
        let end = app
            .windows
            .get(&wid)
            .unwrap()
            .scroll_glide
            .as_ref()
            .unwrap()
            .glide
            .end();
        app.tick_scroll_glide(wid, end - Duration::from_millis(90));
        app.input(
            wid,
            InputEvent::ScrollView(ScrollIntent::Top),
            Source::Human,
        );
        assert_eq!(
            app.windows.get(&wid).unwrap().scroll_frac_px,
            0,
            "an instant ScrollView jump is whole-row (no eased tail, no residual)"
        );
    }

    /// M1 edge behavior — a notch against a history END (target clamps to the
    /// current position) never arms a zero-length GLIDE (no engine motion) but
    /// still wakes the pill to SHOW the edge. M1b: it RELEASES an elastic bounce
    /// instead of doing nothing (see `overscroll_bounce_springs_and_self_disarms`).
    #[test]
    fn edge_notch_arms_no_glide_but_shows_pill() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = seed_history(&app, wid);
        // Already at the live bottom; a downward notch has nowhere to go.
        // Captured BEFORE the action on purpose. `scroll_wheel_animated` touches the
        // pill with its own internal `Instant::now()`, so sampling `now` after it
        // opened a ~1.2s window that a deschedule could cross. `is_active` is
        // monotone-decreasing in `now` and the touch lands at t >= `before`, so
        // asserting with `before` is true by construction whenever a touch happened —
        // and still false when none did, which is the property under test.
        let before = std::time::Instant::now();
        app.scroll_wheel_animated(wid, &term, -3);
        let ws = app.windows.get(&wid).unwrap();
        assert!(
            ws.scroll_glide.is_none(),
            "a clamped-to-current target must not arm a zero-length glide"
        );
        assert_eq!(
            term_lock(&term).grid().display_offset(),
            0,
            "the bounce is display-only — the engine stays parked at the edge"
        );
        assert!(
            ws.scroll_pill.is_active(before, true),
            "the pill still wakes to show the edge"
        );
    }

    /// M1b elastic overscroll — a wheel notch PAST a history end releases a
    /// rubber-band bounce that feeds the SIGNED `scroll_frac_px` and self-disarms:
    /// (1) at the live bottom a downward notch bounces the band UP (positive frac);
    /// (2) at the scrollback top an upward notch bounces it DOWN (negative frac);
    /// (3) neither moves the engine (display-only); (4) frame-paced ticks decay the
    /// bounce and DROP the spring (frac back to 0 — the 0%-idle disarm).
    #[test]
    fn overscroll_bounce_springs_and_self_disarms() {
        use std::time::Instant;
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = seed_history(&app, wid);

        // (1) BOTTOM edge: at the live bottom, a downward notch bounces UP (+frac).
        app.scroll_wheel_animated(wid, &term, -4);
        {
            let ws = app.windows.get(&wid).unwrap();
            assert!(
                ws.overscroll.is_some(),
                "a bottom overscroll releases the spring"
            );
            assert!(
                ws.scroll_frac_px > 0,
                "bottom bounce shifts the band UP (+frac)"
            );
            assert_eq!(term_lock(&term).grid().display_offset(), 0, "engine parked");
        }
        // Frame-paced ticks decay the bounce to rest and disarm.
        let end = app
            .windows
            .get(&wid)
            .unwrap()
            .overscroll
            .as_ref()
            .unwrap()
            .end();
        let mut now = Instant::now();
        let mut settled = false;
        for _ in 0..64 {
            now += Duration::from_millis(16);
            app.tick_overscroll(wid, now);
            if app.windows.get(&wid).unwrap().overscroll.is_none() {
                settled = true;
                break;
            }
        }
        assert!(settled, "the bounce self-disarms within its settle cap");
        assert!(
            now <= end + Duration::from_millis(16),
            "disarms by the settle bound"
        );
        assert_eq!(
            app.windows.get(&wid).unwrap().scroll_frac_px,
            0,
            "a settled bounce rests whole-row (no residual)"
        );

        // (2) TOP edge: scrolled to the top, an upward notch bounces DOWN (−frac).
        term_lock(&term).scroll_to_top();
        let top = term_lock(&term).grid().display_offset();
        assert!(top > 0, "test needs the viewport parked in history");
        app.scroll_wheel_animated(wid, &term, 4);
        {
            let ws = app.windows.get(&wid).unwrap();
            assert!(
                ws.overscroll.is_some(),
                "a top overscroll releases the spring"
            );
            assert!(
                ws.scroll_frac_px < 0,
                "top bounce shifts the band DOWN (−frac)"
            );
            assert_eq!(
                term_lock(&term).grid().display_offset(),
                top,
                "engine parked at the scrollback top"
            );
        }

        // (3) Reduced motion cancels the bounce and never re-arms (whole-row snap).
        app.system_reduce_motion = true;
        app.scroll_wheel_animated(wid, &term, 4);
        let ws = app.windows.get(&wid).unwrap();
        assert!(
            ws.overscroll.is_none(),
            "Reduced motion never arms a bounce"
        );
        assert_eq!(ws.scroll_frac_px, 0, "Reduced motion rests whole-row");
    }
}

#[cfg(test)]
mod keystroke_press_side_effect_tests {
    use crate::input::{InputEvent, Source};
    use crate::{App, WindowId, term_lock};
    use aterm_core::selection::{SelectionSide, SelectionType};
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers};

    fn key(event_type: KeyEventType) -> InputEvent {
        InputEvent::Key {
            key: Key::Character('a'),
            mods: Modifiers::empty(),
            base_layout: None,
            event_type,
        }
    }

    /// The press path's snap + deselect + predictor sample run under ONE term
    /// lock. This pins their semantics: a key PRESS at the seam snaps a
    /// history-scrolled viewport back to the live bottom AND clears an active
    /// selection ("typing deselects"); a key RELEASE (Kitty REPORT_EVENT_TYPES)
    /// is not a typing event and must do neither — only encode.
    #[test]
    fn press_snaps_and_deselects_release_does_not() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        {
            let mut t = term_lock(&term);
            // Enough output to build scrollback, then scroll into history and select.
            t.process("x\r\n".repeat(64).as_bytes());
            t.scroll_to_top();
            assert_ne!(t.grid().display_offset(), 0, "viewport is in history");
            t.text_selection_mut().start_selection(
                0,
                0,
                SelectionSide::Left,
                SelectionType::Simple,
            );
            t.text_selection_mut()
                .update_selection(0, 5, SelectionSide::Left);
            t.text_selection_mut().complete_selection();
            assert!(t.text_selection().has_selection());
        }
        // RELEASE first: no snap, no deselect (a legacy release also encodes nothing,
        // so the whole event is a no-op at the PTY).
        let _ = app.input(wid, key(KeyEventType::Release), Source::Human);
        {
            let t = term_lock(&term);
            assert_ne!(t.grid().display_offset(), 0, "release must not snap");
            assert!(
                t.text_selection().has_selection(),
                "release must not deselect"
            );
        }
        // PRESS: snaps to the live bottom and clears the selection in one pass.
        let _ = app.input(wid, key(KeyEventType::Press), Source::Human);
        let t = term_lock(&term);
        assert_eq!(t.grid().display_offset(), 0, "press snaps to bottom");
        assert!(!t.text_selection().has_selection(), "press deselects");
    }
}

/// The press/release path's LOCK-ELISION guards. The terminal mutex is the one
/// lock a keystroke shares with the PTY reader, so the press path must take it
/// EXACTLY once (the seam's consolidated scope) and a byte-silent release must not
/// take it at all — while every human-visible side-effect still happens.
#[cfg(test)]
mod press_path_lock_elision_tests {
    use super::{publish_release_relevance, sampled_release_relevance};
    use crate::input::{InputEvent, Source};
    use crate::{App, WindowId, term_lock};
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers};
    use winit::event::ElementState;
    use winit::keyboard::{KeyCode, PhysicalKey};

    fn press(ch: char) -> InputEvent {
        InputEvent::Key {
            key: Key::Character(ch),
            mods: Modifiers::empty(),
            base_layout: None,
            event_type: KeyEventType::Press,
        }
    }

    /// A real winit key event, so the tests below drive the shipping `on_key`
    /// routing (where the press-path snap lives) rather than the seam alone.
    fn character_event(ch: char, state: ElementState) -> winit::event::KeyEvent {
        let text = winit::keyboard::SmolStr::new(ch.to_string());
        winit::event::KeyEvent::synthetic_for_test(
            PhysicalKey::Code(KeyCode::KeyA),
            winit::keyboard::Key::Character(text.clone()),
            (state == ElementState::Pressed).then_some(text),
            winit::keyboard::KeyLocation::Standard,
            state,
            false,
        )
    }

    /// An App whose SESSION CAPABILITY (not merely the window's mirror) writes to
    /// an observer pipe: the release path routes through
    /// `pool.get(session).ctx.sink`, so a window-only mirror would report silence
    /// for bytes that really flowed.
    #[cfg(unix)]
    fn app_observing_pty() -> (App, [i32; 2]) {
        use std::sync::Arc;

        use aterm_session::sink::SinkWriter;

        let mut pipe = [0; 2];
        assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
        let flags = unsafe { libc::fcntl(pipe[0], libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(
            unsafe { libc::fcntl(pipe[0], libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0
        );
        (
            App::headless_for_test_with_sink(Arc::new(SinkWriter::new(pipe[1]))),
            pipe,
        )
    }

    #[cfg(unix)]
    fn drain(pipe: [i32; 2]) -> Vec<u8> {
        let mut bytes = [0u8; 64];
        let read = unsafe { libc::read(pipe[0], bytes.as_mut_ptr().cast(), bytes.len()) };
        if read <= 0 {
            return Vec::new();
        }
        bytes[..read as usize].to_vec()
    }

    /// A seam-bound keystroke must land the WHOLE press-path snap without any
    /// caller-side `snap_to_bottom` (which would be a third terminal-mutex
    /// acquisition per key): the seam's consolidated lock scope pulls the viewport back to the
    /// live bottom, and the lock-free window half cancels the wheel glide's banked
    /// residual so no momentum tail can ease the view back off the prompt (M1/M1b).
    #[cfg(unix)]
    #[test]
    fn a_seam_bound_press_snaps_the_viewport_and_kills_the_glide_residual() {
        let (mut app, pipe) = app_observing_pty();
        let wid = WindowId(0);
        let term = app.front_terminal(wid).expect("terminal").term.clone();
        {
            let mut t = term_lock(&term);
            t.process("x\r\n".repeat(64).as_bytes());
            t.scroll_to_top();
            assert_ne!(t.grid().display_offset(), 0, "viewport is in history");
        }
        app.windows.get_mut(&wid).expect("window").scroll_frac_px = 7;

        app.on_key(wid, character_event('a', ElementState::Pressed));

        assert_eq!(
            term_lock(&term).grid().display_offset(),
            0,
            "the seam's own snap must still pull the view to the live bottom"
        );
        assert_eq!(
            app.windows[&wid].scroll_frac_px, 0,
            "the glide residual must still be dropped at the press"
        );
        assert_eq!(drain(pipe), b"a".to_vec(), "and the key still types");
        unsafe {
            libc::close(pipe[0]);
            libc::close(pipe[1]);
        }
    }

    /// The arms that END a press before the seam keep their own snap. A bare Cmd
    /// chord no shortcut claims is swallowed (real terminals never forward Cmd
    /// combos), and swallowing it must still jump the viewport back to live — the
    /// human parity "any key press jumps to live".
    #[cfg(unix)]
    #[test]
    fn a_swallowed_cmd_chord_still_snaps_the_viewport() {
        let (mut app, pipe) = app_observing_pty();
        let wid = WindowId(0);
        let term = app.front_terminal(wid).expect("terminal").term.clone();
        {
            let mut t = term_lock(&term);
            t.process("x\r\n".repeat(64).as_bytes());
            t.scroll_to_top();
            assert_ne!(t.grid().display_offset(), 0, "viewport is in history");
        }
        app.windows.get_mut(&wid).expect("window").scroll_frac_px = 7;
        app.on_modifiers_changed(wid, winit::keyboard::ModifiersState::SUPER);

        app.on_key(wid, character_event('j', ElementState::Pressed));

        assert_eq!(
            term_lock(&term).grid().display_offset(),
            0,
            "a swallowed Cmd chord still snaps"
        );
        assert_eq!(app.windows[&wid].scroll_frac_px, 0);
        assert_eq!(
            drain(pipe),
            Vec::<u8>::new(),
            "and it must never leak a byte to the PTY"
        );
        unsafe {
            libc::close(pipe[0]);
            libc::close(pipe[1]);
        }
    }

    /// The slot is keyed on the SESSION: a sample published for one session must
    /// never answer for another (that would let a legacy shell's "releases are
    /// byte-silent" verdict swallow a Kitty app's release reports next door).
    #[test]
    fn a_sample_answers_only_for_the_session_it_was_taken_on() {
        publish_release_relevance(41, false);
        assert_eq!(sampled_release_relevance(41), Some(false));
        assert_eq!(
            sampled_release_relevance(42),
            None,
            "no cross-session answer"
        );
        publish_release_relevance(41, true);
        assert_eq!(sampled_release_relevance(41), Some(true));
        // Session 0 is a real id and must not collide with the "unsampled" encoding.
        publish_release_relevance(0, false);
        assert_eq!(sampled_release_relevance(0), Some(false));
    }

    /// LEGACY SHELL (no Kitty flags): the press publishes "releases encode
    /// nothing", so the forwarded release skips `seam_egress` — and therefore the
    /// terminal mutex — entirely. Byte-identical: the seam would write nothing and
    /// report `Delivery::Full`, which is exactly what the release path reports.
    #[cfg(unix)]
    #[test]
    fn legacy_release_is_byte_silent_and_skips_the_seam() {
        let (mut app, pipe) = app_observing_pty();
        let wid = WindowId(0);
        let session = app.front_terminal(wid).expect("terminal").session;
        let physical = PhysicalKey::Code(KeyCode::KeyA);

        let _ = app.input(wid, press('a'), Source::Human);
        assert_eq!(drain(pipe), b"a".to_vec(), "the press itself still types");
        assert_eq!(
            sampled_release_relevance(session),
            Some(false),
            "a legacy press must publish that its release is byte-silent"
        );

        app.note_forwarded_press_key(
            wid,
            physical,
            false,
            Key::Character('a'),
            Modifiers::empty(),
            None,
        );
        app.release_physical_press(wid, physical);
        assert_eq!(
            drain(pipe),
            Vec::<u8>::new(),
            "a legacy release emits nothing"
        );
        assert!(
            matches!(
                super::take_physical_release_trace(),
                Some(super::PhysicalReleaseTrace::Forwarded {
                    delivery: crate::input::Delivery::Full,
                    ..
                })
            ),
            "the elided release still reports the seam's own verdict"
        );
        unsafe {
            libc::close(pipe[0]);
            libc::close(pipe[1]);
        }
    }

    /// NEGATIVE CONTROL — the guard must never eat a real release report. With
    /// Kitty `REPORT_EVENT_TYPES` negotiated (`CSI > 2 u`) the press publishes
    /// "relevant", the release takes the seam as before, and the CSI-u release
    /// report reaches the PTY. Ctrl+A, because a BARE printable key produces text
    /// and has no representable release even in this mode — the non-text chord is
    /// the case where a skipped release would actually lose bytes.
    #[cfg(unix)]
    #[test]
    fn kitty_event_type_release_still_reports() {
        let (mut app, pipe) = app_observing_pty();
        let wid = WindowId(0);
        let session = app.front_terminal(wid).expect("terminal").session;
        let term = app.front_terminal(wid).expect("terminal").term.clone();
        term_lock(&term).process(b"\x1b[>2u");
        let physical = PhysicalKey::Code(KeyCode::KeyA);

        let _ = app.input(
            wid,
            InputEvent::Key {
                key: Key::Character('a'),
                mods: Modifiers::CTRL,
                base_layout: None,
                event_type: KeyEventType::Press,
            },
            Source::Human,
        );
        let _ = drain(pipe);
        assert_eq!(
            sampled_release_relevance(session),
            Some(true),
            "an event-type press must keep its release on the seam"
        );

        app.note_forwarded_press_key(
            wid,
            physical,
            false,
            Key::Character('a'),
            Modifiers::CTRL,
            None,
        );
        app.release_physical_press(wid, physical);
        assert_eq!(
            drain(pipe),
            b"\x1b[97;5:3u".to_vec(),
            "REPORT_EVENT_TYPES releases must still reach the app"
        );
        unsafe {
            libc::close(pipe[0]);
            libc::close(pipe[1]);
        }
    }
}

#[cfg(test)]
mod predictive_echo_input_gate_tests {
    use std::time::{Duration, Instant};

    use super::{keystroke_click_audible, prediction_visibility_requires_redraw};
    use crate::input::{InputEvent, Source};
    use crate::{App, WindowId, term_lock};
    use aterm_predict::PredictMode;
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers};

    fn printable(ch: char) -> InputEvent {
        InputEvent::Key {
            key: Key::Character(ch),
            mods: Modifiers::empty(),
            base_layout: None,
            event_type: KeyEventType::Press,
        }
    }

    #[test]
    fn codex_report_event_types_never_arms_the_expiry_erase() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();

        let model = aterm_spec::derive::predictive_echo_visibility_model();
        let mut model_state = model.init_state();

        // Start from the dangerous state: this line has already proved a slow
        // echo, so an Adaptive guess is both pending and visible before Codex
        // takes ownership of the composer.
        app.config.predictive_echo = Some("adaptive".to_string());
        let now = Instant::now();
        {
            let predictor = &mut app.windows.get_mut(&wid).expect("window").predictor;
            predictor.set_mode(PredictMode::Adaptive);
            assert!(predictor.predict_char('a', (0, 0), 80, now));
            let first_confirmed = now + Duration::from_millis(60);
            predictor.reconcile(Some((0, 1)), false, first_confirmed, |row, col| {
                ((row, col) == (0, 0)).then_some('a')
            });
            assert!(predictor.predict_char('b', (0, 1), 80, first_confirmed));
            let second_confirmed = first_confirmed + Duration::from_millis(60);
            predictor.reconcile(Some((0, 2)), false, second_confirmed, |row, col| {
                ((row, col) == (0, 1)).then_some('b')
            });
            assert!(predictor.predict_char('c', (0, 2), 80, second_confirmed));
            assert!(predictor.is_displaying(second_confirmed));
        }
        assert!(model.fire("ConfirmSlow", &mut model_state));
        assert!(model.fire("Key", &mut model_state));
        assert_eq!(model_state["pending"], 1);
        assert_eq!(model_state["visible"], 1);

        assert!(
            app.windows[&wid].predictor.next_deadline().is_some(),
            "control: the pre-Codex visible guess armed the 250 ms self-heal"
        );

        // Codex's observed main-screen flags are 1|2|4: disambiguate, report event
        // types, and report alternate keys. It does not set REPORT_ALL_KEYS_AS_ESC,
        // so a gate keyed only on that flag would arm and later erase ghost text.
        term_lock(&term).process(b"\x1b[>7u");
        assert!(model.fire("EnterComposer", &mut model_state));
        let _ = app.input(wid, printable('c'), Source::Human);
        assert!(model.fire("Key", &mut model_state));
        let predictor = &app.windows[&wid].predictor;
        assert!(
            predictor.idle(),
            "the app-owned composer flushes prior guesses"
        );
        assert_eq!(model_state["pending"], 0);
        assert_eq!(model_state["visible"], 0);
        assert!(
            predictor.next_deadline().is_none(),
            "Codex input must not arm a delayed erase"
        );
    }

    /// The key-time click's host gate. Every conjunct is a case where the
    /// cue could only ever be silence, so paying its delivering redraw on
    /// the hottest path would be pure cost: no audio host (headless, a
    /// non-macOS stub, a permanently failed worker), the `trail_sounds`
    /// knob off, or a muted volume.
    ///
    /// The last two conjuncts additionally close a CREDIT LEAK, which is why
    /// this gate may not merely approximate the drain's: cueing at the key also
    /// arms a credit the echo spends to stay silent, so a cue the drain drops
    /// leaves a credit standing for a click that never sounded, muting an echo
    /// that should have clicked.
    #[test]
    fn a_click_that_cannot_be_heard_is_never_cued() {
        assert!(keystroke_click_audible(true, true, 0.4, true, false));
        assert!(
            !keystroke_click_audible(false, true, 0.4, true, false),
            "no live worker ⇒ no redraw for a click nothing can play"
        );
        assert!(
            !keystroke_click_audible(true, false, 0.4, true, false),
            "trail sounds off ⇒ silent by the user's own knob"
        );
        assert!(
            !keystroke_click_audible(true, true, 0.0, true, false),
            "volume 0 is mute, exactly as the render drain's gain law reads it"
        );
        assert!(
            !keystroke_click_audible(true, true, 0.4, false, false),
            "serious mode mutes terminal sound at the drain, so it must not arm a credit here"
        );
        assert!(
            !keystroke_click_audible(true, true, 0.4, true, true),
            "the post-resize quiet window drains silently — a credit armed inside it \
             would mute the NEXT echo instead"
        );
    }

    #[test]
    fn every_visible_predictor_flush_requires_an_erase_redraw() {
        let now = Instant::now();
        let visible = || {
            let mut predictor = aterm_predict::Predictor::new(PredictMode::Always);
            assert!(predictor.predict_char('a', (0, 0), 80, now));
            assert!(predictor.is_displaying(now));
            predictor
        };

        // Enter/submission.
        let mut predictor = visible();
        let was = predictor.is_displaying(now);
        predictor.note_line_submit();
        assert!(prediction_visibility_requires_redraw(
            was,
            predictor.is_displaying(now)
        ));

        // App-owned/no-echo transition.
        let mut predictor = visible();
        let was = predictor.is_displaying(now);
        predictor.reset();
        assert!(prediction_visibility_requires_redraw(
            was,
            predictor.is_displaying(now)
        ));

        // Unsupported wide input flush.
        let mut predictor = visible();
        let was = predictor.is_displaying(now);
        assert!(!predictor.predict_char('日', (0, 0), 80, now));
        assert!(prediction_visibility_requires_redraw(
            was,
            predictor.is_displaying(now)
        ));

        // Right-margin wrap refusal flush.
        let mut predictor = aterm_predict::Predictor::new(PredictMode::Always);
        assert!(predictor.predict_char('a', (0, 79), 80, now));
        let was = predictor.is_displaying(now);
        assert!(!predictor.predict_char('b', (0, 79), 80, now));
        assert!(prediction_visibility_requires_redraw(
            was,
            predictor.is_displaying(now)
        ));

        // Negative control: hidden Adaptive bookkeeping before and after a
        // mutation does not request a useless frame.
        assert!(!prediction_visibility_requires_redraw(false, false));
    }
}

#[cfg(test)]
mod vi_dispatch_tests {
    use super::{VI_ACTIVE_TERMINALS, vi_any_active};
    use crate::keybinding::Action;
    use crate::{App, WindowId, term_lock};
    use std::sync::Mutex;
    use std::sync::atomic::Ordering;

    /// `VI_ACTIVE_TERMINALS` is process-wide (it must be: a stray cross-thread
    /// toggle has to be visible to the key path, which a thread-local would hide).
    /// Every test that TOGGLES therefore serializes here, so one test's transient
    /// +1 can never be observed as another's count.
    static VI_MIRROR_SERIAL: Mutex<()> = Mutex::new(());

    /// VI-1: `dispatch_action(ToggleViMode)` flips keyboard copy-mode on the window's
    /// terminal (off → on → off) via the same seam a bound chord hits. Headless — no
    /// window/pixels, just the engine state the render override keys off.
    #[test]
    fn toggle_vi_mode_flips_active() {
        let _serial = VI_MIRROR_SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let mut app = App::headless_for_test();
        let wid = WindowId(7);
        app.install_window_state(wid, crate::stub_session(7), 24, 80);
        let active = |app: &App| {
            app.front_terminal(wid)
                .is_some_and(|terminal| term_lock(&terminal.term).vi_is_active())
        };
        assert!(!active(&app), "vi mode starts off");
        app.dispatch_action(wid, Action::ToggleViMode);
        assert!(active(&app), "toggle turns vi mode on");
        app.dispatch_action(wid, Action::ToggleViMode);
        assert!(!active(&app), "a second toggle turns it off");
    }

    /// Native focus cannot inherit terminal authority from a still-live hidden
    /// shell. This is the negative half of the terminal toggle test above: the
    /// real terminal remains in the pool, but no resolver, global handle, or
    /// terminal-only action may select it implicitly.
    #[test]
    fn native_focus_has_no_terminal_capability_or_vi_fallback() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let hidden = app
            .front_terminal(wid)
            .expect("initial terminal")
            .term
            .clone();

        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        assert!(app.front_terminal(wid).is_none());
        assert_eq!(app.focused_session_id(wid), None);
        assert!(app.windows[&wid].active_terminal.is_none());
        assert!(app.active_handle.lock().unwrap().is_none());

        app.dispatch_action(wid, Action::ToggleViMode);
        assert!(
            !term_lock(&hidden).vi_is_active(),
            "terminal-only command must not fall back to the hidden shell"
        );
    }

    /// VI-1: while active, a motion drives the vi cursor (the engine moves — the GUI's
    /// on_key_vi_mode calls `vi_motion`). Drives the accessor directly to prove the
    /// Terminal API the dispatcher uses actually moves the copy-mode cursor.
    #[test]
    fn vi_motion_moves_the_cursor() {
        let mut app = App::headless_for_test();
        let wid = WindowId(7);
        app.install_window_state(wid, crate::stub_session(7), 24, 80);
        let terminal = app.front_terminal(wid).expect("front terminal");
        let mut t = term_lock(&terminal.term);
        t.process(b"hello world\r\nsecond line\r\n");
        t.vi_toggle();
        assert!(t.vi_is_active());
        let start = t.vi_cursor_point();
        t.vi_motion(aterm_core::ViMotion::Up, aterm_core::ViBoundary::Grid);
        assert_eq!(
            t.vi_cursor_point().line,
            start.line - 1,
            "k (Up) moves the vi cursor up one line"
        );
    }

    /// The GUI-side vi mirror must track the ENGINE exactly, because the two
    /// per-keystroke vi gates answer from it instead of taking the terminal
    /// mutex twice per key. Off ⇒ `vi_any_active()` is false and neither gate can
    /// touch a terminal; a toggle ON must raise it (or a key would leak to the PTY
    /// while the user is navigating copy-mode), and the toggle OFF must lower it
    /// again (or every keystroke keeps paying both locks forever).
    #[test]
    fn vi_mirror_tracks_the_engine_so_the_key_path_can_skip_the_term_lock() {
        let _serial = VI_MIRROR_SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let mut app = App::headless_for_test();
        let wid = WindowId(7);
        app.install_window_state(wid, crate::stub_session(7), 24, 80);
        let engine_active = |app: &App| {
            app.front_terminal(wid)
                .is_some_and(|terminal| term_lock(&terminal.term).vi_is_active())
        };
        let base = VI_ACTIVE_TERMINALS.load(Ordering::Relaxed);

        assert!(!engine_active(&app));
        app.dispatch_action(wid, Action::ToggleViMode);
        assert!(engine_active(&app));
        assert_eq!(
            VI_ACTIVE_TERMINALS.load(Ordering::Relaxed),
            base + 1,
            "entering copy-mode must open the gate"
        );
        assert!(vi_any_active());

        app.dispatch_action(wid, Action::ToggleViMode);
        assert!(!engine_active(&app));
        assert_eq!(
            VI_ACTIVE_TERMINALS.load(Ordering::Relaxed),
            base,
            "leaving copy-mode must close the gate again"
        );
    }

    /// The mirror is a COUNT (two windows can hold two terminals in copy-mode),
    /// and it must never wrap: an extra "now inactive" report saturates at zero
    /// rather than latching the gate open at `usize::MAX`.
    #[test]
    fn vi_mirror_never_wraps_below_zero() {
        let _serial = VI_MIRROR_SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let base = VI_ACTIVE_TERMINALS.load(Ordering::Relaxed);
        for _ in 0..(base + 2) {
            super::vi_note_toggled(false);
        }
        assert_eq!(VI_ACTIVE_TERMINALS.load(Ordering::Relaxed), 0);
        assert!(!vi_any_active());
        // Restore whatever the process-wide count was, so a leaked +1 from an
        // unrelated test is not silently swallowed by this one.
        for _ in 0..base {
            super::vi_note_toggled(true);
        }
    }

    /// A terminal that DIES in copy-mode must retire its contribution.
    ///
    /// `vi_toggle` is the mirror's only other decrement, so without this the count
    /// is a one-way ratchet: close a tab while copy-mode is on and the stranded
    /// `+1` pins the gate "maybe active" for the rest of the PROCESS, sending both
    /// per-keystroke vi checks back to the terminal mutex — the exact lock elision
    /// this mirror exists to provide, given away by one ordinary user action, in
    /// the fail-safe direction that guarantees nobody notices.
    #[test]
    fn a_terminal_dying_in_copy_mode_does_not_strand_the_mirror() {
        let _serial = VI_MIRROR_SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let base = VI_ACTIVE_TERMINALS.load(Ordering::Relaxed);

        // A terminal enters copy-mode, then is torn down while still in it.
        super::vi_note_toggled(true);
        assert_eq!(VI_ACTIVE_TERMINALS.load(Ordering::Relaxed), base + 1);
        super::vi_note_terminal_gone(true);

        assert_eq!(
            VI_ACTIVE_TERMINALS.load(Ordering::Relaxed),
            base,
            "the dead terminal's +1 must not outlive it"
        );

        // A terminal torn down while NOT in copy-mode contributed nothing and must
        // not over-decrement someone else's live copy-mode.
        super::vi_note_toggled(true);
        super::vi_note_terminal_gone(false);
        assert_eq!(
            VI_ACTIVE_TERMINALS.load(Ordering::Relaxed),
            base + 1,
            "a terminal that was not in vi must not decrement a live one"
        );
        assert!(
            vi_any_active(),
            "the still-live copy-mode terminal holds the gate"
        );
        super::vi_note_toggled(false);
        assert_eq!(VI_ACTIVE_TERMINALS.load(Ordering::Relaxed), base);
    }
}

/// TYPED-"kitty" cameo plumbing (task: typing the word at the prompt summons
/// a kitty in the terminal). The detector's own laws (once-per-completion,
/// cooldown without restamp, backspace tolerance, session keying) are proven
/// in `crate::kitty_summon`; this module binds the App seams: the press path
/// feeds ONLY typed keys (never screen bytes), a granted summon reaches the
/// cursor companion's hello and the Kitty Log's ordinary episode rules, and
/// the effects master gate keeps everything inert when nothing could draw.
#[cfg(test)]
mod typed_kitty_summon_tests {
    use crate::input::{InputEvent, Source};
    use crate::{App, WindowId, term_lock};
    use aterm_spec::derive::cursor_cat_model;
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
    use std::time::{Duration, Instant};

    fn key(character: char) -> InputEvent {
        InputEvent::Key {
            key: Key::Character(character),
            mods: Modifiers::empty(),
            base_layout: None,
            event_type: KeyEventType::Press,
        }
    }

    fn named(named: NamedKey) -> InputEvent {
        InputEvent::Key {
            key: Key::Named(named),
            mods: Modifiers::empty(),
            base_layout: None,
            event_type: KeyEventType::Press,
        }
    }

    fn type_word(app: &mut App, wid: WindowId, word: &str) {
        for c in word.chars() {
            app.input(wid, key(c), Source::Human);
        }
    }

    /// Drive enough echo-correlated pulses to cross every real earn threshold.
    /// The owner gate is applied to every pulse, so the disabled fixture
    /// remains cold while the enabled negative control must eventually arm.
    fn drive_cursor_cat_momentum(app: &mut App, wid: WindowId, enabled: bool) {
        let base = Instant::now();
        for i in 0..160 {
            crate::app_render::forward_kitty_cursor_momentum(
                enabled,
                crate::cursor_glow::GlowStyle::RainbowKitty,
                Some(base + Duration::from_millis(i * 40)),
                &mut app.windows.get_mut(&wid).unwrap().cursor_cat,
            );
        }
    }

    /// Tier-1 host binding for the derived cursor-cat owner gate. The shipping
    /// echo-correlated momentum seam must not forward the decisive pulse while
    /// `cursor_trail = false`, and the exact glass/capture predicate must not
    /// draw that ordinary branch. Enabling the master proves the fixture is
    /// capable of arming. A typed/collection hello remains independently
    /// presentable with the master off.
    #[test]
    fn cursor_trail_master_owns_ordinary_kitty_but_not_typed_hello() {
        let model = cursor_cat_model();
        let wid = WindowId(0);

        let mut off_app = App::headless_for_test();
        off_app.config.cursor_trail = Some(false);
        off_app.config.cursor_trail_style = Some("rainbow kitty".into());
        off_app.kitty_cursor_enabled_cache = None;
        off_app.recompute_sparkle();
        assert!(
            off_app.sparkle.is_some(),
            "Sparkle is on for this gate probe"
        );
        drive_cursor_cat_momentum(&mut off_app, wid, false);
        assert!(
            !off_app.windows[&wid].cursor_cat.is_active(),
            "trail master off must withhold the decisive ordinary momentum key"
        );
        assert!(
            !crate::app_render::cursor_cat_presentation_enabled(
                true,
                false,
                crate::cursor_glow::GlowStyle::RainbowKitty,
                false,
            ),
            "trail master off must also close the ordinary render branch"
        );

        let mut off_spec = model.init_state();
        assert!(model.fire("TypeWhileTrailOff", &mut off_spec));
        assert_eq!(
            off_spec[&"ordinary_armed"],
            i64::from(off_app.windows[&wid].cursor_cat.is_active())
        );
        assert_eq!(off_spec[&"ordinary_visible"], 0);

        // Negative control: the historical style-only gate arms the exact
        // state that Buggy=1 admits and the healthy invariant rejects.
        let mut buggy = cursor_cat_model();
        for cst in &mut buggy.consts {
            if cst.0 == "Buggy" {
                cst.1 = 1;
            }
        }
        let mut leaked = buggy.init_state();
        assert!(buggy.fire("TypeWhileTrailOff", &mut leaked));
        assert_eq!(leaked[&"ordinary_armed"], 1);
        assert_eq!(leaked[&"ordinary_visible"], 1);
        assert!(!buggy.check_invariant("TrailMasterOwnsOrdinary", &leaked));

        // The same preheated real engine MUST arm when its owner is enabled;
        // otherwise the master-off assertion above could pass vacuously.
        let mut on_app = App::headless_for_test();
        on_app.config.cursor_trail = Some(true);
        on_app.config.cursor_trail_style = Some("rainbow kitty".into());
        on_app.kitty_cursor_enabled_cache = None;
        on_app.recompute_sparkle();
        drive_cursor_cat_momentum(&mut on_app, wid, true);
        assert!(
            on_app.windows[&wid].cursor_cat.is_active(),
            "enabled Nyan owner forwards the decisive momentum key"
        );
        assert!(crate::app_render::cursor_cat_presentation_enabled(
            true,
            true,
            crate::cursor_glow::GlowStyle::RainbowKitty,
            false,
        ));
        let mut on_spec = model.init_state();
        assert!(model.fire("EnableTrail", &mut on_spec));
        assert!(model.fire("TypeOrdinary", &mut on_spec));
        assert_eq!(on_spec[&"ordinary_armed"], 1);
        assert_eq!(on_spec[&"ordinary_visible"], 1);

        // Typed summons intentionally use the independent bounded hello. They
        // stay visible even with reduced animation and the trail owner off.
        let session = off_app.front_terminal(wid).unwrap().session;
        let hello_at = Instant::now();
        off_app.summon_typed_kitty(wid, session, hello_at, true);
        let hello = off_app
            .windows
            .get_mut(&wid)
            .unwrap()
            .cursor_cat
            .static_frame(hello_at);
        assert!(hello.collection_hello && hello.alpha > 0);
        assert!(crate::app_render::cursor_cat_presentation_enabled(
            false,
            false,
            crate::cursor_glow::GlowStyle::RainbowKitty,
            hello.collection_hello,
        ));
        assert!(model.fire("Collect", &mut off_spec));
        assert_eq!(off_spec[&"trail_master"], 0);
        assert_eq!(off_spec[&"visible"], i64::from(hello.alpha > 0));
        assert!(model.check_invariant("HelloIndependentOfTrailMaster", &off_spec));
    }

    /// Typing `kitty` summons the cameo through the cursor companion's hello
    /// and logs EXACTLY one episode as the type it visually is (`head_peek`,
    /// ordinary magic) — and the consumed completion's tail adds nothing.
    #[test]
    fn typing_kitty_summons_the_cameo_and_logs_one_episode() {
        let mut app = App::headless_for_test();
        app.recompute_sparkle();
        assert!(
            app.sparkle.is_some(),
            "the default config resolves sparkle words ON"
        );
        let wid = WindowId(0);
        assert!(!app.windows[&wid].cursor_cat.is_active());

        type_word(&mut app, wid, "kitty");
        assert!(
            app.windows[&wid].cursor_cat.is_active(),
            "the typed word summons the companion hello"
        );
        assert!(
            app.windows[&wid]
                .cursor_cat
                .static_deadline(std::time::Instant::now())
                .is_some(),
            "the reduced-motion erase wake is armed (the hello is a real \
             collection-style presentation, not a nyan-only flight)"
        );
        assert_eq!(
            app.kitty_log.log().sightings,
            1,
            "one episode through the ordinary Kitty Log rules"
        );
        let entry = &app.kitty_log.log().entries[0];
        assert_eq!(
            entry.kitty_type, "head_peek",
            "no schema churn: the summon logs as the pinned type it renders"
        );
        assert_eq!(entry.magic, "none");

        type_word(&mut app, wid, "y");
        assert_eq!(
            app.kitty_log.log().sightings,
            1,
            "the completion was consumed — its tail re-triggers nothing"
        );
    }

    /// SCREEN CONTENT NEVER SUMMONS: a PTY payload full of "kitty" (the
    /// `cat somefile` case) reaches the terminal grid, not the detector —
    /// no cameo, no typed-summon log entry. (On-screen occurrences remain
    /// the ambient word-cat renderer's separate, unchanged domain.)
    #[test]
    fn screen_output_of_kitty_never_summons() {
        let mut app = App::headless_for_test();
        app.recompute_sparkle();
        let wid = WindowId(0);
        let session = app.front_terminal(wid).unwrap().session;
        term_lock(&app.pool.get(session).unwrap().term).process(b"kitty kitty kitty\r\n");
        assert!(
            !app.windows[&wid].cursor_cat.is_active(),
            "PTY output must never reach the typed detector"
        );
        assert_eq!(app.kitty_log.log().sightings, 0);
    }

    /// M2 HOST WIRING PROOF — keys alone build NO cat momentum: the cursor
    /// cat's metric builds only from a real keystroke correlated with its
    /// forward ECHO (the glow's `take_momentum_pulse`, fed in `tick_cursor_fx`),
    /// never from key presses at the input seam. A printable keystream through
    /// the real press path with no rendered forward echo — the non-echoing case,
    /// e.g. a password prompt — leaves the metric at zero,
    /// so it can never summon the cat over a dark, non-advancing ribbon. (The
    /// correlated case building BOTH instances identically is pinned at the
    /// effects layer by `momentum_unifies_glow_and_cat_metrics`.)
    #[test]
    fn key_only_input_builds_no_cat_momentum() {
        let mut app = App::headless_for_test();
        app.recompute_sparkle();
        let wid = WindowId(0);
        // The style the cat rides, so a key-only feed would build momentum here.
        app.config.cursor_trail_style = Some("nyan".to_string());
        let now = std::time::Instant::now();
        // 40 printable presses through the real seam. No present/tick runs, so
        // the glow never observes a forward echo and never pulses the cat.
        for _ in 0..40 {
            app.input(wid, key('j'), Source::Human);
        }
        assert_eq!(
            app.windows[&wid].cursor_cat.momentum(now),
            0.0,
            "keys with no correlated forward echo build zero cat momentum"
        );
        assert!(
            !app.windows[&wid].cursor_cat.is_active(),
            "…so a key-only stream never summons the cat over a dark ribbon"
        );
    }

    /// The rate limit holds on the real input path: back-to-back completions
    /// land inside [`crate::kitty_summon::TYPED_SUMMON_COOLDOWN`], so
    /// kitty-spam yields one LEDGER episode, not a flood (the CAMEO itself is not
    /// rate-limited — see `kitty_summon`'s two tiers).
    #[test]
    fn kitty_spam_is_rate_limited_to_one_episode() {
        let mut app = App::headless_for_test();
        app.recompute_sparkle();
        let wid = WindowId(0);
        type_word(&mut app, wid, "kittykittykitty");
        assert_eq!(
            app.kitty_log.log().sightings,
            1,
            "spam inside the cooldown logs exactly once"
        );
    }

    /// A word-breaking key (plain Enter) clears the run on the real path:
    /// `kit` ⏎ `ty` was never the typed word.
    #[test]
    fn enter_breaks_the_typed_run() {
        let mut app = App::headless_for_test();
        app.recompute_sparkle();
        let wid = WindowId(0);
        type_word(&mut app, wid, "kit");
        app.input(wid, named(NamedKey::Enter), Source::Human);
        type_word(&mut app, wid, "ty");
        assert!(!app.windows[&wid].cursor_cat.is_active());
        assert_eq!(app.kitty_log.log().sightings, 0);
        // The orphaned `ty` is still in the window, so the next word needs its
        // delimiter: completion is the lexicon's WORD-boundary match, not a
        // substring suffix, so `tykitty` is not the word `kitty`.
        type_word(&mut app, wid, " kitty");
        assert_eq!(
            app.kitty_log.log().sightings,
            1,
            "a contiguous word still summons"
        );
    }

    /// EFFECTS MASTER GATE: with sparkle words unresolved (off) nothing could
    /// draw a cat, so the summon is wholly inert — no hello, no log entry —
    /// exactly like the ambient sightings the cameo mirrors.
    #[test]
    fn summon_is_inert_with_sparkle_words_off() {
        let mut app = App::headless_for_test();
        assert!(app.sparkle.is_none(), "headless default: not yet resolved");
        let wid = WindowId(0);
        type_word(&mut app, wid, "kitty");
        assert!(!app.windows[&wid].cursor_cat.is_active());
        assert_eq!(app.kitty_log.log().sightings, 0);
    }

    /// FELINE SUB-GATE: `[sparkle_words.feline] enabled = false` disables every
    /// ambient cat decoration — and with it every ambient Kitty Log sighting —
    /// while the OTHER families keep the sparkle master resolved ON. The typed
    /// summon must be equally inert under that config: no cameo, and no ledger
    /// row, since a `head_peek` episode would land in a category the user's
    /// config can never produce ambiently. Gating on the master alone is not
    /// enough.
    #[test]
    fn summon_is_inert_with_feline_family_off() {
        let mut app = App::headless_for_test();
        app.config.sparkle_words = Some(crate::app_config::SparkleWordsConfig {
            feline: Some(crate::app_config::SparkleFelineConfig {
                enabled: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        });
        // Runtime activation is deliberately memory-only: config mutations
        // become visible only after the worker-prepared generation is
        // installed. Model that shipping handoff instead of asking
        // `recompute_sparkle` to reopen/reparse config on the input path.
        app.prepared_sparkle = app.config.prepare_sparkle_runtime();
        app.sparkle_dirty = true;
        app.recompute_sparkle();
        let rs = app
            .sparkle
            .as_ref()
            .expect("the remaining families keep the sparkle master resolved ON");
        assert!(
            !rs.cfg.feline,
            "the resolved config carries the feline opt-out"
        );
        let wid = WindowId(0);
        type_word(&mut app, wid, "kitty");
        assert!(
            !app.windows[&wid].cursor_cat.is_active(),
            "feline off: no cameo may arm"
        );
        assert_eq!(
            app.kitty_log.log().sightings,
            0,
            "feline off: nothing rendered means nothing logged"
        );
    }

    /// Every GRANTED summon is a FRESH `(session, ident)` episode: the dedupe
    /// ring (which absorbs recounts of one episode for its whole TTL) must
    /// not eat a later legitimate summon of the same session.
    #[test]
    fn each_granted_summon_logs_a_fresh_episode() {
        let mut app = App::headless_for_test();
        app.recompute_sparkle();
        let wid = WindowId(0);
        let session = app.front_terminal(wid).unwrap().session;
        let now = std::time::Instant::now();
        app.summon_typed_kitty(wid, session, now, true);
        assert_eq!(app.kitty_log.log().sightings, 1);
        assert!(app.windows[&wid].cursor_cat.is_active());
        // Bypassing the detector's cooldown on purpose: this seam proves the
        // ident sequence, not the rate limit (the detector owns that proof).
        app.summon_typed_kitty(wid, session, now, true);
        assert_eq!(
            app.kitty_log.log().sightings,
            2,
            "a reused ident would have been absorbed by the ring for RING_TTL"
        );
    }

    /// THE TWO TIERS AT THE APP SEAM. A ledger-suppressed summon
    /// (`record = false`) must still present the cameo, and must not touch the
    /// log — not the sighting count, not the ident sequence.
    #[test]
    fn a_ledger_suppressed_summon_still_shows_the_cameo() {
        let mut app = App::headless_for_test();
        app.recompute_sparkle();
        let wid = WindowId(0);
        let session = app.front_terminal(wid).unwrap().session;
        let now = std::time::Instant::now();

        let seq_before = app.kitty_summon_seq;
        app.summon_typed_kitty(wid, session, now, false);

        assert!(
            app.windows[&wid].cursor_cat.is_active(),
            "the cat must come when called, cooldown or not"
        );
        assert_eq!(
            app.kitty_log.log().sightings,
            0,
            "the ledger tier stayed closed"
        );
        assert_eq!(
            app.kitty_summon_seq, seq_before,
            "a suppressed record must not consume an ident"
        );
    }
}

/// FAVOURITE-THE-SESSION-KITTY App seams. The ledger laws (composition
/// transfer, the restart election, ring bypass) are proven in `kitty_log`; this
/// module binds the App seam: WHICH cat is promoted, the two gates, and the
/// surface wiring.
///
/// `dispatch_menu_action` takes an `&ActiveEventLoop` and is not
/// headless-callable, so — exactly as the summon tests do — the action is
/// called directly and `action_by_name` proves the surface reachability.
#[cfg(test)]
mod favourite_session_kitty_tests {
    use crate::{App, WindowId};
    use aterm_effects::kitty_registry::{KittyLook, glyph_key};

    /// The promoted cat is THIS SESSION's own kitty — never `companion_look()`,
    /// which on any non-empty ledger is whatever was last collected — and the
    /// press presents it through the collection hello.
    #[test]
    fn favourite_promotes_the_session_kitty() {
        let mut app = App::headless_for_test();
        app.recompute_sparkle();
        let wid = WindowId(0);
        let session = app
            .front_terminal(wid)
            .expect("headless front terminal")
            .session;
        let look = KittyLook::for_session(session);
        let now = std::time::Instant::now();

        // Collect something ELSE first, so the App-global companion — which
        // wins for EVERY window in `app_render` — is a different cat.
        // Favouriting "what I can see" could then never promote a session
        // kitty.
        let other = KittyLook {
            variant: aterm_effects::cat_glyphs_gen::CatGlyphId::SpecWitch,
            ..KittyLook::default()
        }
        .normalized();
        assert_ne!(
            other, look,
            "the fixture must differ from the session kitty"
        );
        app.kitty_log.observe(
            session,
            [aterm_effects::kitty_registry::KittySighting {
                kitty_type: aterm_effects::kitty_registry::KittyType::HeadPeek,
                magic: aterm_effects::kitty_registry::KittyMagic::None,
                shown_as: aterm_effects::kitty_registry::KittyShownAs::Cat,
                langs: aterm_lexicon::LangSet::EMPTY,
                traits: 0,
                look: other,
                ident: 0xFEED,
            }],
            aterm_lexicon::Lexicon::builtin(),
            now,
            true,
        );
        assert_eq!(
            app.kitty_log.companion_look(),
            Some(other),
            "the collected companion is displayed everywhere until we favourite"
        );

        app.favourite_session_kitty(wid, now);

        assert_eq!(
            app.kitty_log.companion_look(),
            Some(look),
            "the session kitty is the one that got promoted, not the displayed cat"
        );
        let row = app
            .kitty_log
            .log()
            .collectibles
            .iter()
            .find(|item| item.key == glyph_key(look.variant))
            .expect("the session kitty's head is now a durable roster row")
            .clone();
        assert!(!row.favourite.is_empty(), "stamped as the user's pick");
        assert_eq!(row.coat, look.coat, "wearing the pinned composition");
        assert_eq!(row.iris, look.iris);

        assert!(app.windows[&wid].cursor_cat.is_active());
        let frame = app
            .windows
            .get_mut(&wid)
            .expect("the window is live")
            .cursor_cat
            .static_frame(now);
        assert!(
            frame.collection_hello,
            "a favourite IS a reason to change the identity, so it says hello"
        );
        assert_eq!(frame.look, look, "and the hello wears the promoted cat");
    }

    /// The two hard gates, in order. Neither may leave a trace: nothing can
    /// draw ⇒ nothing may log, and `feline.enabled = false` means cats do not
    /// exist, so a synthetic row for a category the config can never produce
    /// ambiently would poison the ledger.
    #[test]
    fn favourite_respects_the_sparkle_master_and_feline_gates() {
        let wid = WindowId(0);
        let now = std::time::Instant::now();

        // (a) SPARKLE MASTER unresolved.
        let mut app = App::headless_for_test();
        assert!(app.sparkle.is_none(), "headless default: not yet resolved");
        app.favourite_session_kitty(wid, now);
        assert_eq!(app.kitty_log.companion_look(), None, "master off: no pin");
        assert_eq!(app.kitty_log.log().sightings, 0);
        assert!(!app.windows[&wid].cursor_cat.is_active());

        // (b) FELINE SUB-GATE off while the other families keep the master ON.
        let mut app = App::headless_for_test();
        app.config.sparkle_words = Some(crate::app_config::SparkleWordsConfig {
            feline: Some(crate::app_config::SparkleFelineConfig {
                enabled: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        });
        app.prepared_sparkle = app.config.prepare_sparkle_runtime();
        app.sparkle_dirty = true;
        app.recompute_sparkle();
        assert!(
            !app.sparkle
                .as_ref()
                .expect("the remaining families keep the sparkle master resolved ON")
                .cfg
                .feline
        );
        app.favourite_session_kitty(wid, now);
        assert_eq!(app.kitty_log.companion_look(), None, "feline off: no pin");
        assert_eq!(app.kitty_log.log().sightings, 0);
        assert!(!app.windows[&wid].cursor_cat.is_active());
    }

    /// The whole menu/palette/`invoke` wiring in one assertion, plus the
    /// checkmark that reports the pin back to the user.
    #[test]
    fn the_favourite_row_is_invoke_reachable_and_checks_once_pinned() {
        let mut app = App::headless_for_test();
        app.recompute_sparkle();
        let wid = WindowId(0);

        let before = app.palette_snapshot(wid);
        assert_eq!(
            before.action_by_name("FavouriteSessionKitty"),
            Ok(crate::menu::MenuAction::FavouriteSessionKitty),
            "the row must be reachable by `aterm-ctl invoke` over a front terminal"
        );
        // `controls_lines` is the same text `controls menu` prints, so this
        // covers the introspection surface as well as the checkmark.
        let row = |state: &crate::palette::PaletteState| {
            state
                .controls_lines()
                .into_iter()
                .find(|line| line.contains("action=FavouriteSessionKitty"))
                .expect("the View menu contributes the row")
        };
        let listed = row(&before);
        assert!(listed.contains("enabled=true"), "{listed}");
        assert!(
            listed.contains("checked=false"),
            "nothing pinned yet: {listed}"
        );
        assert!(
            !app.palette_live().session_kitty_favourited,
            "and the live predicate agrees"
        );

        app.favourite_session_kitty(wid, std::time::Instant::now());

        let listed = row(&app.palette_snapshot(wid));
        assert!(
            listed.contains("checked=true"),
            "the row reports THIS session's kitty as the pin: {listed}"
        );
    }
}

/// SING-ALONG press-path seams. The detector's own laws (repeat
/// arm, interleave/backspace never arm, the wind-down crossfade, session
/// keying, the note ring) are proven in `aterm_effects::kitty_sing`; this
/// module binds the App seam: the press path feeds ONLY typed keys —
/// screen bytes can never arm the celebration — and the release keys
/// release through the same classified press the cameo/tone feeds ride.
#[cfg(test)]
mod full_kitty_sing_seam_tests {
    use crate::input::{InputEvent, Source};
    use crate::{App, WindowId, term_lock};
    use aterm_effects::kitty_sing::SING_ARM_REPEATS;
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

    fn key(character: char) -> InputEvent {
        InputEvent::Key {
            key: Key::Character(character),
            mods: Modifiers::empty(),
            base_layout: None,
            event_type: KeyEventType::Press,
        }
    }

    /// Holding a key through the REAL input path arms SING-ALONG at the configured
    /// repeat (the calls land far inside the repeat-gap window), and a PTY
    /// payload of the same repeated byte reaches the grid, not the detector.
    #[test]
    fn held_key_arms_through_the_press_path_and_screen_bytes_never_do() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        // Same implicit window as `backspace_and_enter_release_the_hold_gracefully`
        // below, and STRICTLY WORSE: that one only needed the final sample inside
        // `SING_REPEAT_GAP` (250ms) of the last repeat, while this needs all sixteen
        // inter-key gaps under it too — `note_char` resets `count` to 1 whenever a gap
        // exceeds it (kitty_sing.rs:171-193). `App::input` reads `Instant::now()`
        // internally (:1960), so a single mid-loop deschedule un-arms it legitimately
        // and the assertion then reports a product regression that did not happen.
        // Re-arm instead; `note_char` re-anchors at `count >= SING_ARM_REPEATS`.
        let now = 'arm: {
            for attempt in 0..8 {
                // The "not before repeat 16" claim is only meaningful on a burst that
                // actually stayed inside the cadence, so it is checked on the burst
                // that ends up arming — the retry below discards descheduled runs.
                for i in 0..SING_ARM_REPEATS {
                    if i == 0 {
                        assert!(
                            !app.windows[&wid]
                                .kitty_sing
                                .is_armed(std::time::Instant::now()),
                            "no arm before the burst begins (attempt={attempt})"
                        );
                    }
                    app.input(wid, key('a'), Source::Human);
                }
                let sample = std::time::Instant::now();
                if app.windows[&wid].kitty_sing.is_armed(sample) {
                    break 'arm sample;
                }
                assert!(
                    attempt < 7,
                    "sixteen at-cadence repeats never armed through the press path in \
                     8 tries — that is a regression, not scheduler noise"
                );
                // A partially-armed run must not leak into the next attempt's
                // "no arm before the burst" check.
                app = App::headless_for_test();
            }
            unreachable!("the bounded loop either breaks or asserts")
        };
        assert_eq!(app.windows[&wid].kitty_sing.drive(now), 1.0);

        // TYPED PROVENANCE: `cat` of a repeated character floods the SCREEN,
        // never the detector (fresh app so no prior arm lingers).
        let fresh = App::headless_for_test();
        let session = fresh.front_terminal(wid).unwrap().session;
        term_lock(&fresh.pool.get(session).unwrap().term).process(b"aaaaaaaaaaaaaaaa");
        assert!(
            !fresh.windows[&wid]
                .kitty_sing
                .is_armed(std::time::Instant::now()),
            "PTY output must never arm the celebration"
        );
    }

    /// Backspace and break keys RELEASE through the press path — never a
    /// hard cut: right after the release the drive is already off 1.0 but
    /// still inside its crossfade (the detector's wind-down law, bound to
    /// the App's classified Backspace/Enter arms).
    #[test]
    fn backspace_and_enter_release_the_hold_gracefully() {
        for release in [
            InputEvent::Key {
                key: Key::Named(NamedKey::Backspace),
                mods: Modifiers::empty(),
                base_layout: None,
                event_type: KeyEventType::Press,
            },
            InputEvent::Key {
                key: Key::Named(NamedKey::Enter),
                mods: Modifiers::empty(),
                base_layout: None,
                event_type: KeyEventType::Press,
            },
        ] {
            let mut app = App::headless_for_test();
            let wid = WindowId(0);
            // The arm decays `SING_REPEAT_GAP` (250ms) after the LAST repeat —
            // `is_armed(now)` is false once `now >= last + gap`, which is the product
            // behaving correctly. But this drives the arm through `App::input`, which
            // reads `Instant::now()` itself, so the sixteen repeats AND the sample
            // below have to land inside one 250ms window of real time. A machine that
            // deschedules this thread mid-loop legitimately un-arms it: observed
            // failing under 2x-core spinners at load average 36. Re-arm instead of
            // asserting a wall-clock window this test does not own — `note_char`
            // re-anchors when `count >= SING_ARM_REPEATS`, so repeating is enough.
            // The armed instant itself is spent where it is taken (the `is_armed`
            // check that ends the retry IS the old `assert!(is_armed(now))`); every
            // assertion below samples `after`, i.e. post-release. So this block arms
            // for effect and yields nothing — unlike its twin above, whose `drive`
            // assertion has to be made at the armed instant.
            'arm: {
                for attempt in 0..8 {
                    for _ in 0..SING_ARM_REPEATS {
                        app.input(wid, key('x'), Source::Human);
                    }
                    let sample = std::time::Instant::now();
                    if app.windows[&wid].kitty_sing.is_armed(sample) {
                        break 'arm;
                    }
                    assert!(
                        attempt < 7,
                        "sixteen repeats never armed the sing in 8 tries — that is a \
                         regression, not scheduler noise"
                    );
                }
                unreachable!("the bounded loop either breaks or asserts")
            }
            app.input(wid, release, Source::Human);
            let after = std::time::Instant::now();
            assert!(
                !app.windows[&wid].kitty_sing.is_armed(after),
                "the release key ends the armed hold"
            );
            assert!(
                app.windows[&wid].kitty_sing.drive(after) > 0.0,
                "…into the crossfade, never a hard cut"
            );
        }
    }
}

/// THE FIND FIELD IS A TEXT FIELD. These drive the shipping `on_key` routing with real
/// winit events (including the `text` payloads winit actually reports for the named
/// keys), so they pin the whole chain — classification, repeat ownership, and the
/// `App`-side edit — not just the pure editing model in `app_search`.
#[cfg(test)]
mod find_field_key_tests {
    use crate::{App, WindowId, term_lock};
    use winit::event::{ElementState, KeyEvent};
    use winit::keyboard::{
        Key, KeyCode, KeyLocation, ModifiersState, NamedKey, PhysicalKey, SmolStr,
    };

    /// A pressed key event shaped like the platform's: `text` is what winit reports,
    /// INCLUDING the control strings it produces for ⎋ (`\u{1b}`), ⏎ (`\r`) and ⇥
    /// (`\t`) — the payloads that must never reach the query as text.
    fn press(logical: Key, text: Option<&str>) -> KeyEvent {
        KeyEvent::synthetic_for_test(
            PhysicalKey::Code(KeyCode::KeyA),
            logical,
            text.map(SmolStr::new),
            KeyLocation::Standard,
            ElementState::Pressed,
            false,
        )
    }

    fn named(key: NamedKey) -> KeyEvent {
        press(Key::Named(key), key.to_text())
    }

    fn character(ch: &str) -> KeyEvent {
        press(Key::Character(SmolStr::new(ch)), Some(ch))
    }

    fn set_mods(app: &mut App, wid: WindowId, mods: ModifiersState) {
        app.windows.get_mut(&wid).unwrap().mods = mods;
    }

    fn query(app: &App, wid: WindowId) -> Option<(String, usize)> {
        app.windows[&wid]
            .search
            .as_ref()
            .map(|search| (search.query.clone(), search.cursor))
    }

    fn type_str(app: &mut App, wid: WindowId, text: &str) {
        for ch in text.chars() {
            app.on_key(wid, character(&ch.to_string()));
        }
    }

    /// ⎋ CLOSES the find bar — it is a command, never a character. winit reports
    /// `text = "\u{1b}"` for it, so a classifier that inserts every `text` payload
    /// types an escape into the query and leaves the bar open.
    #[test]
    fn escape_closes_the_bar_and_types_nothing() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.search_enter();
        type_str(&mut app, wid, "abc");
        assert_eq!(query(&app, wid), Some(("abc".to_string(), 3)));

        app.on_key(wid, named(NamedKey::Escape));
        assert!(app.windows[&wid].search.is_none(), "⎋ closed the find bar");
        // And the cancelled query never became text: reopening starts empty.
        app.search_enter();
        assert_eq!(query(&app, wid), Some((String::new(), 0)));
    }

    /// ⏎ ACCEPTS (closes, keeping the match), and ⇥ is inert — neither leaves its
    /// control character in the query.
    #[test]
    fn enter_accepts_and_tab_is_not_typed() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.pool.get(0).expect("session 0").term.clone();
        term_lock(&term).process(b"needle here\r\n");
        app.search_enter();
        type_str(&mut app, wid, "needle");

        app.on_key(wid, named(NamedKey::Tab));
        assert_eq!(
            query(&app, wid),
            Some(("needle".to_string(), 6)),
            "⇥ leaves no tab in the query"
        );

        app.on_key(wid, named(NamedKey::Enter));
        assert!(app.windows[&wid].search.is_none(), "⏎ accepted and closed");
        assert_eq!(app.search_last_query, "needle", "accept remembered it");
    }

    /// ^A / ^E / ←→ / ⌥← / ^K / ^U / ^W behave like they do in any macOS text field,
    /// and typing lands at the caret rather than being appended.
    #[test]
    fn standard_text_field_chords_edit_the_query() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.search_enter();
        type_str(&mut app, wid, "beta");

        // ^A → start of line, then typing inserts THERE.
        set_mods(&mut app, wid, ModifiersState::CONTROL);
        app.on_key(wid, character("a"));
        assert_eq!(query(&app, wid), Some(("beta".to_string(), 0)), "^A");
        set_mods(&mut app, wid, ModifiersState::empty());
        type_str(&mut app, wid, "al");
        assert_eq!(query(&app, wid), Some(("albeta".to_string(), 2)));

        // ^E → end of line.
        set_mods(&mut app, wid, ModifiersState::CONTROL);
        app.on_key(wid, character("e"));
        assert_eq!(query(&app, wid), Some(("albeta".to_string(), 6)), "^E");

        // ^B / ^F step by character.
        app.on_key(wid, character("b"));
        assert_eq!(query(&app, wid).unwrap().1, 5, "^B");
        app.on_key(wid, character("f"));
        assert_eq!(query(&app, wid).unwrap().1, 6, "^F");

        // ^K from the middle kills to the end; ^U kills to the start.
        set_mods(&mut app, wid, ModifiersState::empty());
        app.on_key(wid, named(NamedKey::ArrowLeft));
        app.on_key(wid, named(NamedKey::ArrowLeft));
        assert_eq!(query(&app, wid), Some(("albeta".to_string(), 4)), "←←");
        set_mods(&mut app, wid, ModifiersState::CONTROL);
        app.on_key(wid, character("k"));
        assert_eq!(query(&app, wid), Some(("albe".to_string(), 4)), "^K");
        app.on_key(wid, character("u"));
        assert_eq!(query(&app, wid), Some((String::new(), 0)), "^U");

        // ^W deletes the previous word.
        set_mods(&mut app, wid, ModifiersState::empty());
        type_str(&mut app, wid, "one two");
        set_mods(&mut app, wid, ModifiersState::CONTROL);
        app.on_key(wid, character("w"));
        assert_eq!(query(&app, wid), Some(("one ".to_string(), 4)), "^W");

        // ⌥← walks back a word (a plain ⌥+letter must stay free for composed input,
        // so word motion lives on the arrows).
        set_mods(&mut app, wid, ModifiersState::empty());
        type_str(&mut app, wid, "two three");
        set_mods(&mut app, wid, ModifiersState::ALT);
        app.on_key(wid, named(NamedKey::ArrowLeft));
        assert_eq!(
            query(&app, wid),
            Some(("one two three".to_string(), 8)),
            "⌥←"
        );
        // ⌥⌫ eats that word; ⌘⌫ clears back to the start.
        app.on_key(wid, named(NamedKey::Backspace));
        assert_eq!(query(&app, wid), Some(("one three".to_string(), 4)), "⌥⌫");
        set_mods(&mut app, wid, ModifiersState::SUPER);
        app.on_key(wid, named(NamedKey::Backspace));
        assert_eq!(query(&app, wid), Some(("three".to_string(), 0)), "⌘⌫");
        // ⌘→ / ⌘← are the line-edge motions.
        app.on_key(wid, named(NamedKey::ArrowRight));
        assert_eq!(query(&app, wid).unwrap().1, 5, "⌘→");
        app.on_key(wid, named(NamedKey::ArrowLeft));
        assert_eq!(query(&app, wid).unwrap().1, 0, "⌘←");

        // Home / End / ⌦ round out the field's vocabulary.
        set_mods(&mut app, wid, ModifiersState::empty());
        app.on_key(wid, named(NamedKey::End));
        assert_eq!(query(&app, wid).unwrap().1, 5, "End");
        app.on_key(wid, named(NamedKey::Home));
        assert_eq!(query(&app, wid).unwrap().1, 0, "Home");
        app.on_key(wid, named(NamedKey::Delete));
        assert_eq!(query(&app, wid), Some(("hree".to_string(), 0)), "⌦");
    }

    /// COMPOSED text (macOS ⌥-dead keys, CJK IME) reaches the FIELD, not the shell.
    /// The keymap leaves plain ⌥+letter unbound precisely so those compose; on macOS
    /// they arrive as an IME commit rather than as key text, so the find bar has to
    /// claim that path too.
    #[test]
    fn ime_commit_types_into_the_field_not_the_shell() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.pool.get(0).expect("session 0").term.clone();
        let before = term_lock(&term).content_seq();
        app.search_enter();
        type_str(&mut app, wid, "caf");
        app.on_ime_commit(wid, "\u{e9}".to_string());
        assert_eq!(
            query(&app, wid),
            Some(("caf\u{e9}".to_string(), 5)),
            "the composed character landed in the query at the caret"
        );
        assert_eq!(
            term_lock(&term).content_seq(),
            before,
            "and never reached the shell"
        );
        // With find closed the commit takes the ordinary PTY path again (headless has
        // no shell to echo it, so this pins only that the find branch stopped claiming
        // it — the terminal content is untouched either way).
        app.search_cancel();
        app.on_ime_commit(wid, "\u{e9}".to_string());
        assert!(app.windows[&wid].search.is_none());
    }

    /// The COMMAND chords, exercised through the real key routing rather than by
    /// calling the App methods: ^G aborts (restoring the pre-find viewport, like ⎋),
    /// and ⌥⌘C / ⌥⌘R flip match-case / regex — including from the composed glyph macOS
    /// puts in `logical_key` for an ⌥ chord (⌥⌘C arrives as "ç").
    #[test]
    fn command_chords_abort_and_toggle_through_the_key_path() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.pool.get(0).expect("session 0").term.clone();
        let mut lines = Vec::new();
        for i in 0..80 {
            lines.extend_from_slice(format!("line {i} needle\r\n").as_bytes());
        }
        term_lock(&term).process(&lines);
        // The user was reading 5 lines above the live bottom when they pressed ⌘F.
        term_lock(&term).scroll_display(5);
        let origin = term_lock(&term).grid().display_offset();

        app.search_enter();
        type_str(&mut app, wid, "needle");
        assert!(
            term_lock(&term).grid().display_offset() != origin,
            "find navigated away from the origin viewport"
        );
        // ⌥⌘C / ⌥⌘R through the key path. macOS puts the ⌥-COMPOSED glyph in the
        // event's text (ç / ®) while the logical key stays the base letter under a Cmd
        // chord — which is exactly why these arms match on `base_logical_key`.
        set_mods(&mut app, wid, ModifiersState::SUPER | ModifiersState::ALT);
        app.on_key(wid, press(Key::Character(SmolStr::new("c")), Some("ç")));
        assert!(
            app.windows[&wid].search.as_ref().unwrap().case_sensitive,
            "⌥⌘C"
        );
        app.on_key(wid, press(Key::Character(SmolStr::new("r")), Some("®")));
        assert!(app.windows[&wid].search.as_ref().unwrap().is_regex, "⌥⌘R");

        // ^G: the emacs abort — closes AND restores the viewport the find started from.
        set_mods(&mut app, wid, ModifiersState::CONTROL);
        app.on_key(wid, character("g"));
        assert!(app.windows[&wid].search.is_none(), "^G closed the find bar");
        assert_eq!(
            term_lock(&term).grid().display_offset(),
            origin,
            "^G restored the pre-find viewport"
        );
    }

    /// The field claims TEXT input, not the whole keyboard: ⌘C still copies the match
    /// the find bar highlighted (its accept contract promises exactly that), and ⇧Home
    /// must not yank the viewport out from under an open find.
    #[test]
    fn the_field_claims_text_input_not_every_chord() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.pool.get(0).expect("session 0").term.clone();
        let mut lines = Vec::new();
        for i in 0..60 {
            lines.extend_from_slice(format!("line {i} needle\r\n").as_bytes());
        }
        term_lock(&term).process(&lines);
        app.search_enter();
        type_str(&mut app, wid, "needle");
        let found = term_lock(&term).grid().display_offset();

        // ⇧Home would jump to the oldest line; with find open it is suspended.
        set_mods(&mut app, wid, ModifiersState::SHIFT);
        app.on_key(wid, named(NamedKey::Home));
        assert_eq!(
            term_lock(&term).grid().display_offset(),
            found,
            "a scrollback chord cannot desync the viewport from the find"
        );
        assert!(app.windows[&wid].search.is_some(), "…and find stays open");

        // ⌘C is NOT swallowed: it falls through to the terminal copy of the match.
        set_mods(&mut app, wid, ModifiersState::SUPER);
        assert!(
            !app.on_key_search_mode(wid, ModifiersState::SUPER, &character("c")),
            "⌘C falls through to the terminal's copy"
        );
        assert!(
            term_lock(&term).selection_to_string().is_some(),
            "the find left a match selected for that copy"
        );
    }

    /// The routing predicates the OTHER input doors consult: AppKit menu equivalents
    /// (⌘V / ⌘A never reach `on_key`) and configured `[keybindings]` actions.
    #[test]
    fn other_input_doors_defer_to_an_open_field() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(
            app.find_field_window().is_none(),
            "no find ⇒ the menu keeps its ordinary targets"
        );
        app.search_enter();
        assert_eq!(
            app.find_field_window(),
            Some(wid),
            "an open find claims ⌘V / ⌘A from the menu bar"
        );
        // Text- and viewport-level configured actions are suspended; app-level ones
        // (windows, tabs, panes, font, find itself) keep working with the panel open.
        use crate::keybinding::Action;
        for action in [
            Action::Paste,
            Action::Copy,
            Action::ScrollToTop,
            Action::ScrollPageUp,
            Action::JumpPrevPrompt,
            Action::ToggleViMode,
        ] {
            assert!(super::find_suspends_action(action), "{action:?}");
        }
        for action in [
            Action::Find,
            Action::NewTab,
            Action::CloseTab,
            Action::FontIncrease,
            Action::SplitVertical,
            Action::ToggleSettings,
        ] {
            assert!(!super::find_suspends_action(action), "{action:?}");
        }
    }

    /// The find field owns the keystroke: none of its editing chords reach the PTY,
    /// and the query still drives the real search (the caret moves are not searches).
    #[test]
    fn field_editing_re_runs_the_search_but_never_reaches_the_shell() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.pool.get(0).expect("session 0").term.clone();
        term_lock(&term).process(b"alpha beta gamma\r\n");
        let before = term_lock(&term).content_seq();

        app.search_enter();
        type_str(&mut app, wid, "beta");
        assert_eq!(
            app.windows[&wid].search.as_ref().unwrap().matches.len(),
            1,
            "typing re-ran the real search"
        );
        // Backspacing to a prefix widens the match set again.
        app.on_key(wid, named(NamedKey::Backspace));
        assert_eq!(app.windows[&wid].search.as_ref().unwrap().query, "bet");
        assert_eq!(app.windows[&wid].search.as_ref().unwrap().matches.len(), 1);
        set_mods(&mut app, wid, ModifiersState::CONTROL);
        app.on_key(wid, character("a"));
        set_mods(&mut app, wid, ModifiersState::empty());
        assert_eq!(
            term_lock(&term).content_seq(),
            before,
            "no find-field keystroke reached the shell"
        );
    }
}
