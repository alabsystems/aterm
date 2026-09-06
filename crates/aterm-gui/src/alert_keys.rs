// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// Native confirmations exist only where native alerts do; elsewhere the router
// compiles for the shared key path and the rest is intentionally idle.
#![cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]

//! Key routing for the native CONFIRMATION alerts — the multi-line-paste sheet
//! ([`crate::App::present_multiline_paste_sheet`]) and the app-modal close/quit alert
//! ([`crate::menu::confirm`]).
//!
//! # The bug this module exists for
//!
//! An `NSAlert`'s default button carries the key equivalent `"\r"` with an EMPTY
//! `keyEquivalentModifierMask`, and AppKit's `performKeyEquivalent:` matches the
//! characters AND the mask, EXACTLY. So `⌘Return` matches nothing at all and the
//! confirmation just sits there. Measured on a real winit + AppKit probe that mirrors
//! the production sheet call byte-for-byte (2026-07-29):
//!
//! ```text
//! MODS=cmd    (modifierFlags 0x100000) → handlerRan=false response=0    sheetStillAttached=true
//! MODS=shift  (modifierFlags  0x20000) → handlerRan=true  response=1000 sheetStillAttached=false
//! ```
//!
//! That is precisely the owner's gesture: ⌘V raises the confirmation instantly, the
//! finger is still on ⌘, the Return that follows arrives as ⌘Return — dead key. It is
//! NOT a focus or key-window problem: the same probe measured `appActive=true
//! keyWindow=_NSAlertPanel attachedSheet=_NSAlertPanel sheetIsKey=true
//! sheetFirstResponder=_NSAlertPanel` immediately after attaching the sheet.
//!
//! # Why the keystroke is intercepted HERE and not in `App::on_key`
//!
//! aterm owns the winit key path, but that path CANNOT see this keystroke: an alert —
//! sheet or app-modal — puts its own `_NSAlertPanel` on screen as the key window, so
//! AppKit delivers the keyDown to the panel, never to the parent window's winit
//! `NSView`. (The same measurement above is the evidence.) Nor can the button's key
//! equivalent be made modifier-tolerant: a key-equivalent mask is a single exact set,
//! and that exactness IS the bug. And deferring the confirmation until the modifiers
//! are released would make the guard arrive late for the one gesture it exists to
//! protect (⌘V at a bare prompt), while still leaving ⌘Return dead for anyone who
//! re-presses ⌘ meanwhile.
//!
//! So the interception happens at the one layer that does see the event: an
//! `NSEvent` LOCAL MONITOR installed for the confirmation's lifetime. The monitor
//! answers a PURE decision function ([`confirm_key`], unit-tested below without any
//! AppKit) and, for Accept/Cancel, `performClick:`s the alert's own button — literally
//! the mouse path — then swallows the event so nothing can leak past the modal.
//!
//! # Why a leaked monitor cannot make the terminal deaf
//!
//! [`confirm_key`]'s FIRST term is "is a confirmation actually attached". The monitor
//! re-derives that from AppKit on every keystroke (`attachedSheet` for a sheet,
//! `isVisible` for an app-modal panel) rather than from a latched flag, so a monitor
//! that somehow outlived its alert passes every key through untouched. The
//! `Drop`-removed [`ConfirmKeyWatch`] token is the primary cleanup; this is the
//! backstop that makes the failure mode benign instead of catastrophic.

/// What a key pressed while a native confirmation is up should DO.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfirmKey {
    /// Fire the DEFAULT (affirmative) button — the "Paste" / proceed answer.
    Accept,
    /// Fire the Cancel button.
    Cancel,
    /// Not ours: hand the event straight back to AppKit, unmodified.
    PassThrough,
}

/// `kVK_Return` — the physical Return key. Hardware key codes are layout-independent,
/// so they are the identity used when `charactersIgnoringModifiers` is unusable.
const KVK_RETURN: u16 = 0x24;
/// `kVK_ANSI_KeypadEnter` — the numeric keypad's Enter, which AppKit's default button
/// also answers to, so aterm must too.
const KVK_KEYPAD_ENTER: u16 = 0x4c;
/// `kVK_Escape`.
const KVK_ESCAPE: u16 = 0x35;

/// Decide what a keystroke means to a native confirmation alert (PURE — unit-tested
/// below, no AppKit involved).
///
/// * `confirmation_attached` — whether THIS keystroke belongs to a confirmation that
///   is still on screen. `false` short-circuits to [`ConfirmKey::PassThrough`], which
///   is what keeps a stale interceptor harmless (see the module docs).
/// * `_modifier_flags` — the event's `NSEventModifierFlags`. Deliberately UNUSED, and
///   present only so that fact is part of the tested contract: Return means "accept"
///   whether or not ⌘/⇧/⌃/⌥ are down. Requiring an empty mask is the AppKit
///   key-equivalent rule this whole module exists to work around; a future edit that
///   reintroduces a mask test here has to delete a test to do it.
/// * `chars_ignoring_modifiers` — the event's `charactersIgnoringModifiers` (`"\r"`
///   for Return under every modifier, `"\u{3}"` for keypad Enter, `"\u{1b}"` for
///   Escape).
/// * `key_code` — the physical `kVK_*` code, used when the characters are absent or
///   unrecognised (a dead-key/IME state must not be able to hide Return).
///
/// Everything else passes through, so the menu bar, ⌘. , the alert's own mouse path
/// and every non-confirmation key behave exactly as they did before.
pub(crate) fn confirm_key(
    confirmation_attached: bool,
    _modifier_flags: u64,
    chars_ignoring_modifiers: Option<&str>,
    key_code: u16,
) -> ConfirmKey {
    if !confirmation_attached {
        return ConfirmKey::PassThrough;
    }
    match chars_ignoring_modifiers {
        // CR (Return), LF (defensive — some sources report Return as \n), ETX (the
        // keypad's Enter).
        Some("\r" | "\n" | "\u{3}") => ConfirmKey::Accept,
        Some("\u{1b}") => ConfirmKey::Cancel,
        // Unrecognised or absent characters: fall back to the physical key.
        _ => match key_code {
            KVK_RETURN | KVK_KEYPAD_ENTER => ConfirmKey::Accept,
            KVK_ESCAPE => ConfirmKey::Cancel,
            _ => ConfirmKey::PassThrough,
        },
    }
}

#[cfg(target_os = "macos")]
pub(crate) use macos::{PasteConfirm, next_confirm_id, watch_alert_keys};

#[cfg(target_os = "macos")]
mod macos {
    use aterm_objc::{BlockPtr, Id, Obj, RcBlock, Sel, sel};

    use super::{ConfirmKey, confirm_key};
    use crate::appkit::consts::NS_EVENT_MASK_KEY_DOWN;
    use crate::appkit::{self, MainThread};

    /// A live local key monitor watching ONE confirmation alert. Dropping this removes
    /// the monitor from AppKit, so the interceptor's lifetime is exactly the lifetime
    /// of this value — the app-modal caller keeps it in a local (a scope, so it cannot
    /// leak at all) and the sheet caller keeps it inside [`PasteConfirm`].
    pub(crate) struct ConfirmKeyWatch {
        /// The opaque token `addLocalMonitorForEventsMatchingMask:handler:` returned,
        /// retained. That token is the only handle AppKit accepts back, and it is +0
        /// on return — see [`watch_alert_keys`], where the retain happens.
        monitor: Obj,
    }

    impl Drop for ConfirmKeyWatch {
        fn drop(&mut self) {
            // SAFETY: `+[NSEvent removeMonitor:]` is `-(void)(id)` — a CLASS method,
            // which is why the receiver is `NSEvent` itself and not the token — with
            // the exact token AppKit returned from
            // `+addLocalMonitorForEventsMatchingMask:handler:`, which is retained here
            // and removed at most once (this is the only owner). Both calls are made
            // on the main thread: the monitor is installed from a main-thread-proven
            // path and this value never crosses threads (it holds an `Obj`, which is
            // `!Send` because `Id` is a raw pointer).
            unsafe {
                appkit::send_v_id(
                    aterm_objc::class(c"NSEvent").as_id(),
                    sel!(removeMonitor:),
                    self.monitor.id(),
                );
            }
        }
    }

    /// Whether `window_a` and `window_b` are the SAME AppKit window object (identity,
    /// not equality — `NSWindow` has no meaningful `isEqual:` for this).
    ///
    /// The `objc2` form compared two `&NSWindow` with `std::ptr::eq`. Those references
    /// were zero-sized markers AT the instance address, so the comparison was always
    /// of the two `id`s — which is what this now says outright. `Id` derives `PartialEq`
    /// over the raw pointer, and comparing nil to nil is not a hazard here because
    /// every caller has already rejected nil.
    fn same_window(window_a: Id, window_b: Id) -> bool {
        !window_a.is_null() && window_a == window_b
    }

    /// Does `event` belong to the confirmation `panel` — and is that confirmation
    /// still on screen? Derived from AppKit on EVERY keystroke (never from a latched
    /// flag) so an interceptor that outlives its alert is inert.
    ///
    /// `parent` is `Some` for a window SHEET (the sheet must still be reported
    /// attached to it) and `None` for an app-modal alert (whose watch is scoped to the
    /// blocking `runModal` call, so panel visibility is the liveness test).
    ///
    /// Scoping by the event's target window is what keeps this app-global monitor from
    /// touching anyone else's keys: a keystroke aimed at a DIFFERENT window (another
    /// aterm window typing away while window A holds a sheet) is not ours.
    ///
    /// `_mtm` is the main-thread witness, taken and deliberately unused. `objc2` made
    /// it an ARGUMENT of `-[NSEvent window]` because its `NSWindow` is
    /// main-thread-only; a raw send has no such parameter, so requiring the witness
    /// here is what keeps that obligation stated rather than dropped in the port.
    fn confirmation_owns_event(event: Id, panel: Id, parent: Option<Id>, _mtm: MainThread) -> bool {
        // SAFETY: plain property reads on a live event and live retained windows, on
        // the main thread (`_mtm` proves it). `-[NSEvent window]` and
        // `-[NSWindow attachedSheet]` are `-(NSWindow *)` (`send_id`, +0 — borrowed
        // for this frame, which is all these identity tests need) and
        // `-[NSWindow isVisible]` is `-(BOOL)` (`send_bool`).
        unsafe {
            let target = appkit::send_id(event, sel!(window));
            if target.is_null() {
                return false;
            }
            // The alert panel is key while it is up, so that is where AppKit sends the
            // keystroke. A key addressed to the sheet's PARENT is accepted too: while
            // a sheet is attached the parent cannot legitimately be typed into, so
            // such an event is exactly the wedged-Return case — and consuming it also
            // means Return can never leak through to the terminal underneath.
            if !(same_window(target, panel) || parent.is_some_and(|p| same_window(target, p))) {
                return false;
            }
            match parent {
                Some(parent) => {
                    let sheet = appkit::send_id(parent, sel!(attachedSheet));
                    same_window(sheet, panel)
                }
                None => appkit::send_bool(panel, sel!(isVisible)),
            }
        }
    }

    /// Click one of the alert's own buttons — the SAME `performClick:` an accessibility
    /// press or a mouse click performs, so the accept/cancel answer travels the alert's
    /// normal action path (ending the sheet with `NSAlertFirstButtonReturn` /
    /// `…SecondButtonReturn`, or stopping the modal session with it) with no
    /// response-code plumbing of our own to get wrong.
    fn click(button: Id) {
        // SAFETY: `-performClick:` is `-(void)(id)` on an `NSButton` this watch
        // retains — one of the two buttons added to the alert that is currently on
        // screen (liveness checked by `confirmation_owns_event` before we get here) —
        // on the main thread. `Id::NIL` is the conventional nil sender.
        unsafe { appkit::send_v_id(button, sel!(performClick:), Id::NIL) };
    }

    /// The monitor's decision for one keyDown: the event itself to let AppKit carry
    /// on, nil to swallow it (after clicking the button it stood for).
    ///
    /// Split out of the block so that [`watch_alert_keys`] can run it under
    /// [`aterm_objc::contain`] and choose the inert answer itself.
    fn decide(
        event: Id,
        panel: &Obj,
        parent: Option<&Obj>,
        accept: &Obj,
        cancel: &Obj,
        mtm: MainThread,
    ) -> Id {
        let attached = confirmation_owns_event(event, panel.id(), parent.map(Obj::id), mtm);
        // SAFETY: plain accessor reads on the live event, main thread.
        // `-charactersIgnoringModifiers` is `-(NSString *)` and MAY be nil (a
        // dead-key/IME state), which is why the nil is preserved as `None`
        // instead of collapsing to an empty string: `confirm_key`'s documented
        // fallback to the physical key code is keyed on absent characters.
        // `-modifierFlags` is `-(NSEventModifierFlags)`, an `NSUInteger`;
        // `-keyCode` is `-(unsigned short)`. All three are valid on a keyDown
        // event, which is the only kind the mask admits.
        let (chars, modifier_flags, key_code) = unsafe {
            let chars = appkit::send_id(event, sel!(charactersIgnoringModifiers));
            (
                (!chars.is_null()).then(|| appkit::nsstring_to_rust(chars)),
                appkit::send_usize(event, sel!(modifierFlags)) as u64,
                appkit::send_u16(event, sel!(keyCode)),
            )
        };
        match confirm_key(attached, modifier_flags, chars.as_deref(), key_code) {
            ConfirmKey::PassThrough => event,
            ConfirmKey::Accept => {
                click(accept.id());
                Id::NIL
            }
            ConfirmKey::Cancel => {
                click(cancel.id());
                Id::NIL
            }
        }
    }

    /// Install the key interceptor for the confirmation alert whose panel is `panel`
    /// and whose two buttons are `accept` (added first — the default/affirmative one)
    /// and `cancel`. `parent` is the sheet's parent window, or `None` for an app-modal
    /// alert. Returns `None` when AppKit declines to install a monitor, in which case
    /// the alert simply keeps its stock behaviour (plain Return / Escape / the mouse).
    ///
    /// The returned [`ConfirmKeyWatch`] OWNS the interception: drop it and the monitor
    /// is gone.
    ///
    /// The handler is an `aterm_objc::RcBlock`: AppKit invokes it from inside
    /// `-[NSApplication sendEvent:]`, and its `invoke` is the guarded trampoline
    /// every declared method runs under, so an `NSException` raised while deciding
    /// is CONTAINED here rather than aborting the process from a `nounwind` frame.
    /// The inert answer for THIS block is chosen explicitly: pass the event through
    /// ("AppKit, carry on"), the same answer it gives when it cannot decide, rather
    /// than the block's default nil, which would swallow the keystroke.
    ///
    /// # Every argument is an OWNING handle, and that is the port's whole ownership
    /// argument
    ///
    /// The `objc2` form took four `Retained<…>` and moved them into the block. These
    /// are [`aterm_objc::Obj`], which is the same +1 reference under a first-party
    /// name: the block owns them for as long as AppKit holds the block, and the
    /// identity tests above stay valid even if AppKit lets go of the objects. Handing
    /// this function a borrowed `Id` instead would compile and would be a
    /// use-after-free the first time an alert was torn down under its own monitor.
    pub(crate) fn watch_alert_keys(
        panel: Obj,
        parent: Option<Obj>,
        accept: Obj,
        cancel: Obj,
    ) -> Option<ConfirmKeyWatch> {
        // Bound FIRST and wrapped SECOND, so the body's own sends keep their own
        // `unsafe` blocks rather than inheriting the constructor's — the shape
        // `app_launch_successor.rs` records, and the reason its SAFETY comments stay
        // load-bearing.
        let handler = move |event: Id| -> Id {
            // Returning the event unchanged = "AppKit, carry on"; returning nil
            // swallows it.
            let pass_through = event;
            if event.is_null() {
                return pass_through;
            }
            let Some(mtm) = MainThread::new() else {
                return pass_through;
            };
            match aterm_objc::contain("alert key monitor", || {
                decide(event, &panel, parent.as_ref(), &accept, &cancel, mtm)
            }) {
                Ok(answer) => answer,
                Err(_) => pass_through,
            }
        };
        // SAFETY: `+addLocalMonitorForEventsMatchingMask:handler:` calls the block
        // as `NSEvent *(^)(NSEvent *)` — the event to carry on with, or nil to
        // swallow it — which is exactly the `(id) -> id` prototype `new1` builds
        // here (`Id` is `Encode`-`"@"` on both sides); nothing unwinds out of its
        // `invoke`.
        let handler = unsafe { RcBlock::new1(handler) }?;
        // SAFETY: `+[NSEvent addLocalMonitorForEventsMatchingMask:handler:]` is
        // `@@:Q@?` on `NSEvent` — a CLASS method taking an `unsigned long long`
        // mask (`NSUInteger` here; both are 64-bit on every Apple target this
        // compiles for) and a BLOCK, which is why the last parameter is a
        // `BlockPtr`: a block is `@?` to the runtime, not `@`. AppKit COPIES the
        // block, so `handler` may drop at the end of this frame; the copy is kept
        // alive by the token below, which is what `removeMonitor:` releases.
        //
        // The result is +0 (autoreleased — the selector is not `new`/`alloc`/`copy`),
        // so it is RETAINED into `Obj` rather than adopted. Adopting it would
        // over-release the moment the pool popped, and `objc2`'s `Retained` return
        // carried exactly this retain.
        let monitor = unsafe {
            let add: unsafe extern "C-unwind" fn(Id, Sel, usize, BlockPtr) -> Id =
                aterm_objc::msg();
            let token = add(
                aterm_objc::class(c"NSEvent").as_id(),
                sel!(addLocalMonitorForEventsMatchingMask:handler:),
                NS_EVENT_MASK_KEY_DOWN,
                handler.as_block_ptr(),
            );
            Obj::retain(token)
        }?;
        Some(ConfirmKeyWatch { monitor })
    }

    /// The next [`PasteConfirm::id`]. Monotonic and never reused, so an answer posted
    /// by an OLD sheet's completion handler can never be mistaken for the answer to a
    /// newer one (which is what stops a stale answer from tearing down a live sheet's
    /// key interceptor). Ids are minted on the main thread; the atomic is simply the
    /// least-ceremony counter with no `App` field to thread through.
    pub(crate) fn next_confirm_id() -> u64 {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    /// The ONE outstanding multi-line-paste confirmation sheet (macOS). Held by `App`
    /// in `Option<PasteConfirm>`: `Some` means a sheet was presented and its answer is
    /// still owed, which is what enforces "at most one confirmation at a time".
    ///
    /// Every field is main-thread AppKit state, retained so the identity checks below
    /// stay valid even if AppKit lets go of the objects.
    pub(crate) struct PasteConfirm {
        /// Monotonic id, echoed back by `Wake::PasteConfirmed` so the answer to THIS
        /// sheet can only ever clear THIS entry (a stale answer arriving after a newer
        /// sheet went up cannot tear the newer one's interceptor down).
        pub(crate) id: u64,
        /// The logical window the sheet hangs off — cleared when that window closes.
        pub(crate) wid: crate::WindowId,
        /// That window's `NSWindow`, for the `attachedSheet` liveness test.
        parent: Obj,
        /// The alert's own panel.
        panel: Obj,
        /// The live key interceptor; dropping this entry removes it from AppKit.
        _keys: Option<ConfirmKeyWatch>,
    }

    impl PasteConfirm {
        /// Record a just-presented sheet.
        pub(crate) fn new(
            id: u64,
            wid: crate::WindowId,
            parent: Obj,
            panel: Obj,
            keys: Option<ConfirmKeyWatch>,
        ) -> Self {
            Self {
                id,
                wid,
                parent,
                panel,
                _keys: keys,
            }
        }

        /// Is this sheet STILL on screen, per AppKit? The self-healing half of the
        /// one-at-a-time rule: an entry whose sheet is gone (its completion handler
        /// never ran, e.g. the window was torn down under it) is discarded rather than
        /// blocking every later paste.
        pub(crate) fn is_attached(&self) -> bool {
            // SAFETY: `-attachedSheet` is `-(NSWindow *)`, a plain property read on a
            // retained window, borrowed only for the comparison; `PasteConfirm` lives
            // on the main thread with `App` (it holds `Obj`s, which are `!Send`).
            let sheet = unsafe { appkit::send_id(self.parent.id(), sel!(attachedSheet)) };
            same_window(sheet, self.panel.id())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ConfirmKey, confirm_key};

    /// `NSEventModifierFlagShift`.
    const SHIFT: u64 = 1 << 17;
    /// `NSEventModifierFlagControl`.
    const CONTROL: u64 = 1 << 18;
    /// `NSEventModifierFlagOption`.
    const OPTION: u64 = 1 << 19;
    /// `NSEventModifierFlagCommand` — 0x100000, the exact mask the probe measured on
    /// the dead ⌘Return.
    const COMMAND: u64 = 1 << 20;
    /// `NSEventModifierFlagFunction` (the keypad's Enter carries it).
    const FUNCTION: u64 = 1 << 23;

    /// Return, `kVK_Return`.
    const RETURN: (Option<&str>, u16) = (Some("\r"), 0x24);
    /// Keypad Enter, `kVK_ANSI_KeypadEnter`.
    const KEYPAD_ENTER: (Option<&str>, u16) = (Some("\u{3}"), 0x4c);
    /// Escape, `kVK_Escape`.
    const ESCAPE: (Option<&str>, u16) = (Some("\u{1b}"), 0x35);
    /// An ordinary character: `a`, `kVK_ANSI_A` (which is 0x00 — a good check that the
    /// key-code fallback does not treat "no code" as Return).
    const LETTER_A: (Option<&str>, u16) = (Some("a"), 0x00);

    fn action(attached: bool, mods: u64, key: (Option<&str>, u16)) -> ConfirmKey {
        confirm_key(attached, mods, key.0, key.1)
    }

    #[test]
    fn plain_return_accepts() {
        assert_eq!(action(true, 0, RETURN), ConfirmKey::Accept);
    }

    /// THE BUG: ⌘Return (modifierFlags 0x100000) must accept. AppKit's own
    /// key-equivalent match rejects it because the default button's mask is empty.
    #[test]
    fn command_return_accepts() {
        assert_eq!(action(true, COMMAND, RETURN), ConfirmKey::Accept);
    }

    #[test]
    fn shift_return_accepts() {
        assert_eq!(action(true, SHIFT, RETURN), ConfirmKey::Accept);
    }

    /// The mask is IRRELEVANT, exhaustively over the modifiers a keyboard can hold —
    /// including the combinations a ⌘V/⌘Q gesture can leave behind mid-release.
    #[test]
    fn every_modifier_combination_still_accepts_return() {
        for mods in [
            0,
            SHIFT,
            CONTROL,
            OPTION,
            COMMAND,
            FUNCTION,
            COMMAND | SHIFT,
            COMMAND | OPTION,
            COMMAND | CONTROL | OPTION | SHIFT,
        ] {
            assert_eq!(
                action(true, mods, RETURN),
                ConfirmKey::Accept,
                "Return with modifierFlags {mods:#x} must accept",
            );
        }
    }

    #[test]
    fn keypad_enter_accepts() {
        assert_eq!(action(true, FUNCTION, KEYPAD_ENTER), ConfirmKey::Accept);
    }

    #[test]
    fn escape_cancels() {
        assert_eq!(action(true, 0, ESCAPE), ConfirmKey::Cancel);
    }

    #[test]
    fn command_escape_still_cancels() {
        assert_eq!(action(true, COMMAND, ESCAPE), ConfirmKey::Cancel);
    }

    #[test]
    fn an_ordinary_character_passes_through() {
        assert_eq!(action(true, 0, LETTER_A), ConfirmKey::PassThrough);
        assert_eq!(action(true, COMMAND, LETTER_A), ConfirmKey::PassThrough);
    }

    /// The anti-deafness invariant: with NO confirmation attached, NOTHING is
    /// intercepted — not Return, not Escape, not a character. This is the property
    /// that makes a leaked interceptor benign instead of leaving the terminal unable
    /// to accept a Return.
    #[test]
    fn nothing_is_intercepted_when_no_confirmation_is_attached() {
        for key in [RETURN, KEYPAD_ENTER, ESCAPE, LETTER_A] {
            for mods in [0, COMMAND, SHIFT, COMMAND | SHIFT] {
                assert_eq!(
                    action(false, mods, key),
                    ConfirmKey::PassThrough,
                    "{key:?} with modifierFlags {mods:#x} must reach the terminal when \
                     no confirmation is up",
                );
            }
        }
    }

    /// A dead-key / IME state that reports no characters must not be able to hide
    /// Return or Escape: the physical key code decides.
    #[test]
    fn missing_characters_fall_back_to_the_physical_key() {
        assert_eq!(action(true, COMMAND, (None, 0x24)), ConfirmKey::Accept);
        assert_eq!(action(true, 0, (None, 0x4c)), ConfirmKey::Accept);
        assert_eq!(action(true, 0, (None, 0x35)), ConfirmKey::Cancel);
        assert_eq!(action(true, 0, (None, 0x00)), ConfirmKey::PassThrough);
    }
}
