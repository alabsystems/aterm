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
}

/// `kVK_Return`. Key codes are layout-independent: the fallback when
/// `charactersIgnoringModifiers` is unusable.
const KVK_RETURN: u16 = 0x24;
/// `kVK_ANSI_KeypadEnter` — the numeric keypad's Enter, which AppKit's default button
/// also answers to, so aterm must too.
const KVK_KEYPAD_ENTER: u16 = 0x4c;
/// `kVK_Escape`.
const KVK_ESCAPE: u16 = 0x35;

/// What a keystroke means to a native confirmation alert.
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
        Some("\r" | "\n" | "\u{3}") => ConfirmKey::Accept,
        Some("\u{1b}") => ConfirmKey::Cancel,
        _ => match key_code {
            KVK_RETURN | KVK_KEYPAD_ENTER => ConfirmKey::Accept,
            KVK_ESCAPE => ConfirmKey::Cancel,
            _ => ConfirmKey::PassThrough,
        },
    }
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
            // SAFETY: `+[NSEvent removeMonitor:]` is `-(void)(id)` called once with the owned monitor token.
            unsafe {
                appkit::send_v_id(
                    aterm_objc::class(c"NSEvent").as_id(),
                    sel!(removeMonitor:),
                    self.monitor.id(),
                );
            }
        }
    }

    /// Whether `window_a` and `window_b` are the same AppKit window object (identity, not
    /// `isEqual:`).
    fn same_window(window_a: Id, window_b: Id) -> bool {
        !window_a.is_null() && window_a == window_b
    }

    /// Whether `event` belongs to the confirmation `panel` and that confirmation is still on
    /// screen.
    fn confirmation_owns_event(event: Id, panel: Id, parent: Option<Id>, _mtm: MainThread) -> bool {
        // SAFETY: plain property reads on a live event and live retained windows, on the main thread (`_mtm` proves it).
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

    /// Click one of the alert's own buttons with `performClick:`, the mouse path, so the
    /// answer travels the alert's normal action path (ending the sheet or the modal session)
    /// with no response-code plumbing of our own.
    fn click(button: Id) {
        // SAFETY: `-performClick:` is `-(void)(id)` on an `NSButton` this watch retains.
        unsafe { appkit::send_v_id(button, sel!(performClick:), Id::NIL) };
    }

    /// The monitor's decision for one keyDown: the event itself to let AppKit carry
    /// on, nil to swallow it (after clicking the button it stood for).
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

    /// Install the key interceptor for confirmation alert `panel` with `accept` and `cancel` buttons.
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
        // SAFETY: `+addLocalMonitorForEventsMatchingMask:handler:` calls the block as `NSEvent *(^)(NSEvent *)`.
        let handler = unsafe { RcBlock::new1(handler) }?;
        // SAFETY: `+[NSEvent addLocalMonitorForEventsMatchingMask:handler:]` is `@@:Q@?` taking a 64-bit mask and a `BlockPtr`.
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
        // SAFETY: these are retained live native objects borrowed on the main thread.
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
                // SAFETY: NSEvent accessors run in a native local monitor on the main thread.
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
                    if kind == 10
                        && !input.key_allowed(owns, appkit::send_u16(event, sel!(keyCode)))
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
        // SAFETY: the local monitor's block is NSEvent *(^)(NSEvent *), matching this guarded (Id)->Id closure.
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
        /// Monotonic id, echoed back by `Wake::PasteConfirmed`, so an answer clears only its own sheet's entry ([`next_confirm_id`]).
        pub(crate) id: u64,
        /// The logical window the sheet hangs off.
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
        pub(crate) const fn new(
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

        /// Whether this sheet is still on screen, per AppKit.
        pub(crate) fn is_attached(&self) -> bool {
            // SAFETY: `-attachedSheet` is `-(NSWindow *)`, a plain property read on a retained window, borrowed only for the comparison.
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
    /// `NSEventModifierFlagCommand` (0x100000).
    const COMMAND: u64 = 1 << 20;
    /// `NSEventModifierFlagFunction` (the keypad's Enter carries it).
    const FUNCTION: u64 = 1 << 23;

    /// Return, `kVK_Return`.
    const RETURN: (Option<&str>, u16) = (Some("\r"), 0x24);
    /// Keypad Enter, `kVK_ANSI_KeypadEnter`.
    const KEYPAD_ENTER: (Option<&str>, u16) = (Some("\u{3}"), 0x4c);
    /// Escape, `kVK_Escape`.
    const ESCAPE: (Option<&str>, u16) = (Some("\u{1b}"), 0x35);
    /// An ordinary character: `a`, `kVK_ANSI_A` (0x00, so the key-code fallback must not read
    /// "no code" as Return).
    const LETTER_A: (Option<&str>, u16) = (Some("a"), 0x00);

    fn action(attached: bool, mods: u64, key: (Option<&str>, u16)) -> ConfirmKey {
        confirm_key(attached, mods, key.0, key.1)
    }

    /// The modifiers include those a ⌘V/⌘Q gesture can leave down mid-release.
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

    /// A leaked interceptor is benign: with no confirmation attached nothing is intercepted.
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
