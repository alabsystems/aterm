// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Key routing for the native confirmation alerts.

/// What a key pressed while a native confirmation is up should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfirmKey {
    /// Fire the default (affirmative) button.
    Accept,
    /// Fire the Cancel button.
    Cancel,
    /// Not ours: hand the event straight back to AppKit, unmodified.
    PassThrough,
    /// A key a control client posted (`aterm ctl hwkey`): swallowed, so it neither presses a
    /// button nor reaches AppKit's own key handling of the alert (Return's default button,
    /// Space on a focused button). A confirmation is the person's to answer.
    Drop,
}

const KVK_RETURN: u16 = 0x24;
const KVK_KEYPAD_ENTER: u16 = 0x4c;
const KVK_ESCAPE: u16 = 0x35;

/// What a keystroke means to a native confirmation alert, `posted` being whether a control
/// client posted it (`crate::hwkey::event_posted`).
pub(crate) fn confirm_key(
    confirmation_attached: bool,
    posted: bool,
    _modifier_flags: u64,
    chars_ignoring_modifiers: Option<&str>,
    key_code: u16,
) -> ConfirmKey {
    if !confirmation_attached {
        return ConfirmKey::PassThrough;
    }
    if posted {
        return ConfirmKey::Drop;
    }
    match chars_ignoring_modifiers {
        Some("\r" | "\n" | "\u{3}") => ConfirmKey::Accept,
        Some("\u{1b}") => ConfirmKey::Cancel,
        _ => match key_code {
            KVK_RETURN | KVK_KEYPAD_ENTER => ConfirmKey::Accept,
            KVK_ESCAPE => ConfirmKey::Cancel,
            _ => ConfirmKey::PassThrough,
        },
    }
}

/// Whether a key a confirmation's key monitor sees could reach that confirmation, so a key a
/// control client posted is dropped there: one that names the confirmation's own window
/// (`attached`), or, while the confirmation is up (`outstanding`), one that names no window
/// (`hwkey … shape=press`: AppKit hands it to the key window) or any key at all during an
/// app-modal alert (`app_modal`, a Quit or Close question), which takes every key. A posted key
/// that names another window while a sheet is up passes on to that window.
#[cfg(any(target_os = "macos", test))]
pub(crate) const fn may_reach_confirmation(
    attached: bool,
    outstanding: bool,
    names_window: bool,
    app_modal: bool,
) -> bool {
    attached || (outstanding && (!names_window || app_modal))
}

#[cfg(not(test))]
static CONFIRMATIONS_UP: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
thread_local! {
    /// The test build's count: one per test thread, so parallel tests never see each other's.
    static CONFIRMATIONS_UP: std::sync::atomic::AtomicUsize =
        const { std::sync::atomic::AtomicUsize::new(0) };
}

fn with_confirmations<R>(f: impl FnOnce(&std::sync::atomic::AtomicUsize) -> R) -> R {
    #[cfg(not(test))]
    {
        f(&CONFIRMATIONS_UP)
    }
    #[cfg(test)]
    {
        CONFIRMATIONS_UP.with(f)
    }
}

/// Held while one of the person's confirmations is on screen (a multi-line paste sheet, a Quit
/// or Close alert, an owner's question): `hwkey` starts no burst and posts no further key while
/// any is held ([`confirmation_up`], read under the posting lock), and the question's opener
/// fences the key queue once it holds one (`hwkey::fence_queued_keys`), so no posted key answers
/// it, whatever window it names, queued before it opened or not, with or without the
/// confirmation's key monitor.
#[cfg(any(target_os = "macos", test))]
pub(crate) struct ConfirmationUp(());

#[cfg(any(target_os = "macos", test))]
impl ConfirmationUp {
    /// Count a confirmation as on screen until this value drops.
    pub(crate) fn hold() -> Self {
        with_confirmations(|up| up.fetch_add(1, std::sync::atomic::Ordering::AcqRel));
        Self(())
    }
}

#[cfg(any(target_os = "macos", test))]
impl Drop for ConfirmationUp {
    fn drop(&mut self) {
        with_confirmations(|up| up.fetch_sub(1, std::sync::atomic::Ordering::AcqRel));
    }
}

/// What `hwkey` answers while one of the person's confirmations is on screen
/// ([`confirmation_up`]).
pub(crate) const CONFIRMATION_UP_REFUSAL: &str = "ERR hwkey: a confirmation is up (a paste, Quit or Close question): its answer is the person's\n";

/// What a burst answers when it stops before its next key because one of the person's
/// confirmations came up ([`confirmation_up`]), with how many keys it had posted.
pub(crate) fn confirmation_up_stop(sent: u32) -> String {
    format!(
        "ERR hwkey: stopped after posted={sent}: a confirmation is up (a paste, Quit or Close \
         question): its answer is the person's\n"
    )
}

/// Whether one of the person's confirmations is on screen now ([`ConfirmationUp`]).
pub(crate) fn confirmation_up() -> bool {
    with_confirmations(|up| up.load(std::sync::atomic::Ordering::Acquire) > 0)
}

#[cfg(target_os = "macos")]
pub(crate) use macos::{ConfirmKeyWatch as PasswordKeyWatch, watch_password_keys};
#[cfg(target_os = "macos")]
pub(crate) use macos::{PasteConfirm, next_confirm_id, watch_alert_keys};

#[cfg(target_os = "macos")]
mod macos {
    use aterm_objc::{BlockPtr, Id, Obj, RcBlock, Sel, sel};

    use super::{ConfirmKey, confirm_key};
    use crate::appkit::consts::NS_EVENT_MASK_KEY_DOWN;
    use crate::appkit::{self, MainThread};

    /// A live local key monitor watching one confirmation alert.
    pub(crate) struct ConfirmKeyWatch {
        /// The opaque token `addLocalMonitorForEventsMatchingMask:handler:` returned, retained
        /// (it is +0 on return; see [`watch_alert_keys`]).
        monitor: Obj,
    }

    impl Drop for ConfirmKeyWatch {
        fn drop(&mut self) {
            // SAFETY: `+[NSEvent removeMonitor:]` is `-(void)(id)` called once with the owned
            // monitor token. Main thread: the monitor is installed from a main-thread path and
            // this value holds an `Obj`, which is `!Send`.
            unsafe {
                appkit::send_v_id(
                    aterm_objc::class(c"NSEvent").as_id(),
                    sel!(removeMonitor:),
                    self.monitor.id(),
                );
            }
        }
    }

    fn same_window(window_a: Id, window_b: Id) -> bool {
        !window_a.is_null() && window_a == window_b
    }

    /// Is `panel` still outstanding on `parent`: attached, or queued behind another sheet?
    /// AppKit queues a `beginSheetModalForWindow:` sent to a window that already presents a
    /// sheet, and `-attachedSheet` names only the front one, so `-sheets` is walked too.
    fn sheet_outstanding(parent: Id, panel: Id) -> bool {
        if panel.is_null() {
            return false;
        }
        // SAFETY: `-[NSWindow attachedSheet]` is `-(NSWindow *)` and `-[NSWindow sheets]` is
        // `-(NSArray<NSWindow *> *)`, both +0 property reads on a retained window, borrowed only
        // for the identity tests in this frame; `-[NSArray count]` is `-(NSUInteger)` and
        // `-objectAtIndex:` is `-(id)(NSUInteger)`, +0, with every index below that count, on
        // an array the window keeps alive for this frame. Main thread only: every caller holds
        // its `Obj`s with `App`, and `Obj` is `!Send`.
        unsafe {
            if same_window(appkit::send_id(parent, sel!(attachedSheet)), panel) {
                return true;
            }
            let sheets = appkit::send_id(parent, sel!(sheets));
            if sheets.is_null() {
                return false;
            }
            (0..appkit::send_usize(sheets, sel!(count))).any(|i| {
                same_window(
                    appkit::send_id_usize(sheets, sel!(objectAtIndex:), i),
                    panel,
                )
            })
        }
    }

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

    /// Whether the watched confirmation is still up (its sheet outstanding on `parent`, or an
    /// app-modal `panel` visible) and whether `event` names any window at all.
    fn confirmation_state(
        event: Id,
        panel: Id,
        parent: Option<Id>,
        _mtm: MainThread,
    ) -> (bool, bool) {
        // SAFETY: plain property reads on a live event and a live retained panel, on the main
        // thread (`_mtm` proves it). `-[NSEvent window]` is `-(NSWindow *)` (+0, compared with
        // nil only) and `-[NSWindow isVisible]` is `-(BOOL)`.
        unsafe {
            let names_window = !appkit::send_id(event, sel!(window)).is_null();
            let outstanding = match parent {
                Some(parent) => sheet_outstanding(parent, panel),
                None => !panel.is_null() && appkit::send_bool(panel, sel!(isVisible)),
            };
            (outstanding, names_window)
        }
    }

    /// Whether `event`, held by a native key monitor before dispatch, is a key a control client
    /// posted with `aterm ctl hwkey`.
    ///
    /// # Safety
    /// `event` is a live `NSEvent` the monitor was handed, read on the main thread.
    unsafe fn posted_key(event: Id) -> bool {
        // SAFETY: `-type` is `-(NSEventType)`, valid on any event; `-keyCode` and `-timestamp`
        // are read only once `-type` said key-down or key-up, the kinds `-keyCode` is defined
        // for (it raises on others).
        unsafe {
            let kind = appkit::send_usize(event, sel!(type));
            crate::hwkey::is_key_kind(kind)
                && crate::hwkey::event_posted(crate::hwkey::PostedKey {
                    kind,
                    keycode: appkit::send_u16(event, sel!(keyCode)),
                    timestamp: appkit::send_f64(event, sel!(timestamp)),
                })
        }
    }

    fn click(button: Id) {
        // SAFETY: `-performClick:` is `-(void)(id)` on an `NSButton` this watch
        // retains — one of the two buttons added to the alert that is currently on
        // screen (liveness checked by `confirmation_owns_event` before we get here) —
        // on the main thread. `Id::NIL` is the conventional nil sender.
        unsafe { appkit::send_v_id(button, sel!(performClick:), Id::NIL) };
    }

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
        // `-charactersIgnoringModifiers` is `-(NSString *)` and may be nil (a dead-key/IME
        // state); nil stays `None` because `confirm_key` falls back to the key code on absent
        // characters. `-modifierFlags` is an `NSUInteger`, `-keyCode` an `unsigned short`; all
        // three are valid on a keyDown, the only kind the mask admits.
        let (chars, modifier_flags, key_code) = unsafe {
            let chars = appkit::send_id(event, sel!(charactersIgnoringModifiers));
            (
                (!chars.is_null()).then(|| appkit::nsstring_to_rust(chars)),
                appkit::send_usize(event, sel!(modifierFlags)) as u64,
                appkit::send_u16(event, sel!(keyCode)),
            )
        };
        let (outstanding, names_window) =
            confirmation_state(event, panel.id(), parent.map(Obj::id), mtm);
        let reaches =
            super::may_reach_confirmation(attached, outstanding, names_window, parent.is_none());
        // SAFETY: the live event this monitor holds, main thread (`mtm`). The ledger keeps a key
        // it found, so asking here leaves it a posted key for a later dispatch it passes to.
        let posted = reaches && unsafe { posted_key(event) };
        match confirm_key(
            attached || posted,
            posted,
            modifier_flags,
            chars.as_deref(),
            key_code,
        ) {
            ConfirmKey::PassThrough => event,
            ConfirmKey::Drop => Id::NIL,
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

    /// Install the key interceptor for confirmation alert `panel` with `accept` and `cancel`
    /// buttons.
    pub(crate) fn watch_alert_keys(
        panel: Obj,
        parent: Option<Obj>,
        accept: Obj,
        cancel: Obj,
    ) -> Option<ConfirmKeyWatch> {
        let handler = move |event: Id| -> Id {
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
        // SAFETY: `+addLocalMonitorForEventsMatchingMask:handler:` calls the block as `NSEvent
        // *(^)(NSEvent *)` — the event to carry on with, or nil to swallow it — which is exactly
        // the `(id) -> id` prototype `new1` builds here (`Id` is `Encode`-`"@"` on both sides);
        // nothing unwinds out of its `invoke`.
        let handler = unsafe { RcBlock::new1(handler) }?;
        // SAFETY: `+[NSEvent addLocalMonitorForEventsMatchingMask:handler:]` is `@@:Q@?` taking a
        // 64-bit mask and a `BlockPtr`. AppKit copies the block, so `handler` may drop at the end
        // of this frame; the copy lives until `removeMonitor:` releases the token. The result is
        // +0 (autoreleased: the selector is not `new`/`alloc`/`copy`), so it is retained into
        // `Obj`, not adopted; adopting would over-release when the pool pops.
        let monitor = unsafe {
            let add: unsafe extern "C-unwind" fn(Id, Sel, usize, BlockPtr) -> Id =
                aterm_objc::checked_msg();
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

    /// Keep a newly attached secure field out of unrelated typing until its own mouse hit.
    pub(crate) fn watch_password_keys(
        panel: &Obj,
        parent: &Obj,
        field: &Obj,
        input: std::sync::Arc<crate::askpass::PasswordInput>,
    ) -> Option<ConfirmKeyWatch> {
        // SAFETY: these are retained live native objects borrowed on the main thread; the block
        // needs independent owners for its full lifetime.
        let (panel, parent, field) = unsafe {
            (
                Obj::retain(panel.id())?,
                Obj::retain(parent.id())?,
                Obj::retain(field.id())?,
            )
        };
        let handler = move |event: Id| -> Id {
            let Some(mtm) = MainThread::new() else {
                return Id::NIL;
            };
            if event.is_null() {
                return event;
            }
            match aterm_objc::contain("password input monitor", || {
                let owns = confirmation_owns_event(event, panel.id(), Some(parent.id()), mtm);
                // SAFETY: NSEvent accessors run in a native local monitor on the main thread; the
                // mask admits only mouseDown/keyDown.
                unsafe {
                    let kind = appkit::send_usize(event, sel!(type));
                    if owns && kind == 1 && appkit::send_id(event, sel!(window)) == panel.id() {
                        let point: unsafe extern "C-unwind" fn(Id, Sel) -> aterm_objc::CGPoint =
                            aterm_objc::checked_msg();
                        let convert: unsafe extern "C-unwind" fn(
                            Id,
                            Sel,
                            aterm_objc::CGPoint,
                            Id,
                        )
                            -> aterm_objc::CGPoint = aterm_objc::checked_msg();
                        let hit = convert(
                            field.id(),
                            sel!(convertPoint:fromView:),
                            point(event, sel!(locationInWindow)),
                            Id::NIL,
                        );
                        let bounds = appkit::send_rect(field.id(), sel!(bounds));
                        if hit.x >= bounds.origin.x
                            && hit.y >= bounds.origin.y
                            && hit.x < bounds.origin.x + bounds.size.width
                            && hit.y < bounds.origin.y + bounds.size.height
                        {
                            input.field_clicked();
                        }
                    }
                    // A key a control client posted never types into, submits or cancels the
                    // person's password sheet.
                    if kind == 10
                        && ((owns && posted_key(event))
                            || !input.key_allowed(owns, appkit::send_u16(event, sel!(keyCode))))
                    {
                        Id::NIL
                    } else {
                        event
                    }
                }
            }) {
                Ok(answer) => answer,
                Err(_) => Id::NIL,
            }
        };
        // SAFETY: the local monitor's block is NSEvent *(^)(NSEvent *), matching this guarded
        // (Id)->Id closure; AppKit copies it and returns +0 token.
        let handler = unsafe { RcBlock::new1(handler) }?;
        let monitor = unsafe {
            let add: unsafe extern "C-unwind" fn(Id, Sel, usize, BlockPtr) -> Id =
                aterm_objc::checked_msg();
            Obj::retain(add(
                aterm_objc::class(c"NSEvent").as_id(),
                sel!(addLocalMonitorForEventsMatchingMask:handler:),
                NS_EVENT_MASK_KEY_DOWN | (1 << 1),
                handler.as_block_ptr(),
            ))
        }?;
        Some(ConfirmKeyWatch { monitor })
    }

    /// The next [`PasteConfirm::id`].
    pub(crate) fn next_confirm_id() -> u64 {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    /// The one outstanding multi-line-paste confirmation sheet.
    pub(crate) struct PasteConfirm {
        /// Monotonic id echoed back by `Wake::PasteConfirmed`.
        pub(crate) id: u64,
        /// The logical window the sheet hangs off.
        pub(crate) wid: crate::WindowId,
        /// That window's `NSWindow`, for the [`sheet_outstanding`] liveness test.
        parent: Obj,
        /// The alert's own panel.
        panel: Obj,
        /// The live key interceptor; dropping this entry removes it from AppKit.
        _keys: Option<ConfirmKeyWatch>,
        /// Counts this sheet as on screen while the entry lives ([`super::ConfirmationUp`]).
        _up: super::ConfirmationUp,
    }

    impl PasteConfirm {
        /// Record a just-presented sheet, with the guard its opener took before presenting it.
        pub(crate) fn new(
            id: u64,
            wid: crate::WindowId,
            parent: Obj,
            panel: Obj,
            keys: Option<ConfirmKeyWatch>,
            up: super::ConfirmationUp,
        ) -> Self {
            Self {
                id,
                wid,
                parent,
                panel,
                _keys: keys,
                _up: up,
            }
        }

        /// Whether this sheet is still outstanding, per AppKit: attached, or queued behind
        /// another sheet on its window ([`sheet_outstanding`]), such as the sudo password sheet
        /// (`askpass_sheet`). A queued paste sheet still counts, so a second paste is refused
        /// instead of dropping this live entry and its key interceptor.
        pub(crate) fn is_attached(&self) -> bool {
            sheet_outstanding(self.parent.id(), self.panel.id())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ConfirmKey, confirm_key};

    const SHIFT: u64 = 1 << 17;
    const CONTROL: u64 = 1 << 18;
    const OPTION: u64 = 1 << 19;
    const COMMAND: u64 = 1 << 20;
    const FUNCTION: u64 = 1 << 23;

    const RETURN: (Option<&str>, u16) = (Some("\r"), 0x24);
    const KEYPAD_ENTER: (Option<&str>, u16) = (Some("\u{3}"), 0x4c);
    const ESCAPE: (Option<&str>, u16) = (Some("\u{1b}"), 0x35);
    const LETTER_A: (Option<&str>, u16) = (Some("a"), 0x00);

    /// A POSTED KEY THAT COULD REACH A CONFIRMATION IS ASKED ABOUT, WHATEVER WINDOW IT NAMES:
    /// while the confirmation is up, a key naming no window (`hwkey … shape=press`, which AppKit
    /// hands to the key window, the paste sheet) and any key during an app-modal Quit or Close
    /// alert count as reaching it, so a posted one is dropped; a key naming another window while
    /// a sheet is up passes to it, and nothing counts once the confirmation is gone.
    /// NEGATIVE CONTROL: with the rule `attached` alone (as `decide` read it at 3a3de060b1), the
    /// window-less key and the app-modal alert's key do not count, and the first assertion fails.
    #[test]
    fn a_posted_key_that_could_reach_a_confirmation_counts_whatever_window_it_names() {
        use super::may_reach_confirmation as reaches;
        // (attached, outstanding, names_window, app_modal)
        assert!(
            reaches(false, true, false, false),
            "a window-less key, sheet up"
        );
        assert!(
            reaches(false, true, true, true),
            "any key, app-modal alert up"
        );
        assert!(
            reaches(false, true, false, true),
            "a window-less key, app-modal alert up"
        );
        assert!(
            reaches(true, true, true, false),
            "a key naming the sheet's window"
        );
        assert!(
            !reaches(false, true, true, false),
            "a key naming another window, sheet up"
        );
        assert!(
            !reaches(false, false, false, false),
            "a window-less key, nothing up"
        );
        assert!(
            !reaches(false, false, true, true),
            "any key, the alert gone"
        );
    }

    /// `hwkey` reads whether one of the person's confirmations is on screen before every key: a
    /// [`super::ConfirmationUp`] counts one while it lives, nested ones included.
    /// NEGATIVE CONTROL: with `hold` counting nothing, `confirmation_up` reads false under a
    /// held guard and the first assertion fails.
    #[test]
    fn a_confirmation_on_screen_is_counted_until_it_goes() {
        assert!(!super::confirmation_up(), "none yet");
        let paste = super::ConfirmationUp::hold();
        assert!(super::confirmation_up(), "the paste sheet");
        let quit = super::ConfirmationUp::hold();
        drop(paste);
        assert!(super::confirmation_up(), "the Quit alert still stands");
        drop(quit);
        assert!(!super::confirmation_up(), "both gone");
    }

    fn action(attached: bool, mods: u64, key: (Option<&str>, u16)) -> ConfirmKey {
        confirm_key(attached, false, mods, key.0, key.1)
    }

    /// A key a control client posted (`aterm ctl hwkey`) never answers the person's confirmation,
    /// a multi-line paste's included: Return, keypad Enter, Escape and any other key are
    /// swallowed while one is up, and pass untouched when none is.
    /// NEGATIVE CONTROL: without `confirm_key`'s `posted` arm, a posted Return answers Accept
    /// and a posted Escape Cancel, and the first assertion fails.
    #[test]
    fn a_key_a_control_client_posted_never_answers_a_confirmation() {
        for key in [RETURN, KEYPAD_ENTER, ESCAPE, LETTER_A] {
            for mods in [0, COMMAND, SHIFT] {
                assert_eq!(
                    confirm_key(true, true, mods, key.0, key.1),
                    ConfirmKey::Drop,
                    "a posted {key:?} with modifierFlags {mods:#x}"
                );
                assert_eq!(
                    confirm_key(false, true, mods, key.0, key.1),
                    ConfirmKey::PassThrough,
                    "a posted {key:?} with no confirmation up"
                );
            }
        }
        assert_eq!(
            action(true, 0, RETURN),
            ConfirmKey::Accept,
            "CONTROL: the person's Return"
        );
    }

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
    fn an_attached_confirmation_maps_enter_escape_and_a_character() {
        for (label, mods, key, expected) in [
            ("keypad Enter", FUNCTION, KEYPAD_ENTER, ConfirmKey::Accept),
            ("Escape", 0, ESCAPE, ConfirmKey::Cancel),
            ("⌘Escape", COMMAND, ESCAPE, ConfirmKey::Cancel),
            ("a", 0, LETTER_A, ConfirmKey::PassThrough),
            ("⌘a", COMMAND, LETTER_A, ConfirmKey::PassThrough),
        ] {
            assert_eq!(action(true, mods, key), expected, "{label}");
        }
    }

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

    #[test]
    fn missing_characters_fall_back_to_the_physical_key() {
        assert_eq!(action(true, COMMAND, (None, 0x24)), ConfirmKey::Accept);
        assert_eq!(action(true, 0, (None, 0x4c)), ConfirmKey::Accept);
        assert_eq!(action(true, 0, (None, 0x35)), ConfirmKey::Cancel);
        assert_eq!(action(true, 0, (None, 0x00)), ConfirmKey::PassThrough);
    }
}
