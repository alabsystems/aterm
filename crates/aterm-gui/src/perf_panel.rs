// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The native macOS PERFORMANCE control panel (View ▸ Performance Panel…).
//!
//! aterm's "performance GUI" is the bottom HUD: a stack of streaming panels (performance
//! fps/frame-time, system CPU/memory load, network rx/tx, and app-fed metric streams),
//! each gated by a `show_*_hud` key in `aterm.toml` (see [`crate::app_config`]) and
//! toggleable live through [`crate::App::set_panel`]. This window is a dedicated CONTROL
//! PANEL over those toggles: one checkbox per HUD panel, seeded from the panel's CURRENT
//! enabled state, that toggles the panel LIVE the instant it is clicked AND persists the
//! choice to `aterm.toml` so it survives a restart.
//!
//! It is the GUI sibling of the four `View ▸ Show … HUD` menu items, gathered into one
//! place. Each checkbox carries its [`crate::hud_bar::PanelId`] in its AppKit `tag`; a
//! single `togglePanel:` selector on the retained [`PerfTarget`] decodes the tag + the
//! checkbox's new state and posts [`Wake::SetHudPanel`](crate::Wake), which the main loop
//! routes to `App::user_set_panel` (re-grids the window so the band appears/disappears
//! immediately, writes the `show_*_hud` key, and revives a switched-off master
//! `show_hud` when a panel is turned on). No parallel toggle logic lives here — like
//! `prefs.rs`, every control is a thin DISPATCH stub.
//!
//! The window is modelled on `prefs.rs` (non-raising factory initializers, manual frame
//! layout, every call on the main thread behind a `MainThreadMarker`). The PURE,
//! AppKit-free [`perf_panel_lines`] renders the same panel/state list as text for the
//! `controls perf` introspection verb and is unit-tested. Off macOS
//! [`open_performance_panel`] is a graceful no-op so the workspace builds everywhere.

use crate::hud_bar::PanelId;

/// Render the Performance control panel's panel/state list as plain text — the single
/// source for the `controls perf` introspection dump (so what an AI reads matches what
/// the window shows). `toggles` is each panel's [`PanelId`] + current enabled state, in
/// registry order. PURE + unit-tested (no AppKit).
pub(crate) fn perf_panel_lines(toggles: &[(PanelId, bool)]) -> Vec<String> {
    let mut out = Vec::with_capacity(toggles.len() + 1);
    out.push(format!("perf-panel toggles={}", toggles.len()));
    for (id, on) in toggles {
        out.push(format!(
            "toggle key={} label={:?} enabled={on}",
            id.config_key(),
            id.label(),
        ));
    }
    out
}

#[cfg(target_os = "macos")]
pub use macos::{PerfPanelHandle, open_performance_panel};

/// Non-macOS no-op handle: there is no native Performance window off macOS. Held by `App`
/// in the same field on every target so the struct shape is platform-independent (mirrors
/// [`crate::menu::MenuHandle`]).
#[cfg(not(target_os = "macos"))]
pub type PerfPanelHandle = ();

/// Non-macOS stub: the native control panel is macOS-only; off macOS there is no window
/// toolkit wired here, so opening it is a no-op. Returns `Option<PerfPanelHandle>` so the
/// call site in `App::open_performance_panel` is identical on every target.
/// ([`perf_panel_lines`] is still built + unit-tested off macOS.)
#[cfg(not(target_os = "macos"))]
pub fn open_performance_panel(
    _proxy: &winit::event_loop::EventLoopProxy<crate::Wake>,
    _toggles: &[(PanelId, bool)],
) -> Option<PerfPanelHandle> {
    None
}

#[cfg(target_os = "macos")]
mod macos {
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObjectProtocol};
    use objc2::{ClassType, DeclaredClass, declare_class, msg_send_id, mutability, sel};
    use objc2_app_kit::{
        NSBackingStoreType, NSButton, NSColor, NSControlStateValueOn, NSFont, NSTextField, NSView,
        NSWindow, NSWindowStyleMask,
    };
    use objc2_foundation::{CGFloat, CGPoint, CGRect, CGSize, MainThreadMarker, NSString};
    use winit::event_loop::EventLoopProxy;

    use crate::Wake;
    use crate::hud_bar::PanelId;

    /// Window geometry (points). A compact fixed-size utility panel: a header, one
    /// checkbox row per HUD panel, a help line, and a Done button. Manual frame layout
    /// (no Auto Layout), exactly like `prefs.rs`.
    const WIN_W: CGFloat = 380.0;
    /// Inset of the content from the window edges.
    const MARGIN: CGFloat = 20.0;
    /// Height (points) of one checkbox row.
    const ROW_H: CGFloat = 24.0;
    /// Vertical gap between rows.
    const ROW_GAP: CGFloat = 8.0;
    /// Header line height.
    const HEADER_H: CGFloat = 22.0;
    /// Button geometry.
    const BUTTON_H: CGFloat = 30.0;
    const BUTTON_W: CGFloat = 100.0;

    /// What [`open_performance_panel`] returns: the retained backing objects. AppKit holds
    /// a window's control targets only WEAKLY and a controller-less retained `NSWindow`
    /// must be kept alive by its owner, so `App` stashes this for the window's life
    /// (mirrors [`crate::menu::MenuHandle`]).
    pub struct PerfPanelHandle {
        /// The Performance `NSWindow`. Retained so it is not freed when this returns.
        _window: Retained<NSWindow>,
        /// The checkbox action target (owns the `EventLoopProxy<Wake>`); referenced only
        /// weakly by the checkboxes, so retain it here.
        _target: Retained<PerfTarget>,
    }

    // SAFETY: `PerfPanelHandle` is only ever created, read, and dropped on the main thread
    // (the event loop). It holds main-thread-only AppKit objects; `App` stores it in a
    // field and never sends it across threads — the auto-derived non-Send is correct.

    impl PerfPanelHandle {
        /// The on-screen CoreGraphics window number (`CGWindowID`) of the live Performance
        /// panel, for the `window perf` introspection capture — or `None` when off-screen
        /// / not yet committed (`windowNumber() <= 0`). Reads the retained `NSWindow`
        /// DIRECTLY (no winit `NSView -> window()` hop). Main-thread only.
        pub(crate) fn window_number(&self) -> Option<isize> {
            // SAFETY: `windowNumber()` is a side-effect-free getter on the live retained
            // NSWindow, called on the main thread.
            let n = unsafe { self._window.windowNumber() };
            (n > 0).then_some(n)
        }
    }

    declare_class!(
        /// The target object for the Performance panel's checkboxes. Owns the
        /// `EventLoopProxy<Wake>` and exposes one selector, `togglePanel:`, which reads the
        /// sender checkbox's `tag` (an index into [`PanelId::ALL`]) + its new on/off state
        /// and posts `Wake::SetHudPanel { id, on }` so the main loop toggles the panel live
        /// and persists the choice.
        pub(crate) struct PerfTarget;

        // SAFETY:
        // - NSObject imposes no subclassing requirements.
        // - InteriorMutable is the safe default; the ivar (the proxy) is never mutated.
        // - PerfTarget has no Drop impl beyond the auto-generated ivar drop.
        unsafe impl ClassType for PerfTarget {
            type Super = objc2::runtime::NSObject;
            type Mutability = mutability::InteriorMutable;
            const NAME: &'static str = "ATermPerfTarget";
        }

        impl DeclaredClass for PerfTarget {
            type Ivars = EventLoopProxy<Wake>;
        }

        unsafe impl NSObjectProtocol for PerfTarget {}

        unsafe impl PerfTarget {
            /// `togglePanel:` — decode the sender checkbox's `tag` into a [`PanelId`] and
            /// its NEW state (the checkbox flips before the action fires), then post
            /// `Wake::SetHudPanel` so the main loop applies it live + persists. A stray tag
            /// (out of range) is ignored. Fire-and-forget: a closed loop just drops it.
            #[method(togglePanel:)]
            fn toggle_panel(&self, sender: Option<&AnyObject>) {
                let Some(sender) = sender else { return };
                // SAFETY: the sender is the checkbox NSButton that fired this action; `tag`
                // and `state` are plain side-effect-free getters, on the main thread.
                let button: &NSButton = unsafe { &*(sender as *const AnyObject as *const NSButton) };
                let tag = unsafe { button.tag() };
                let on = unsafe { button.state() } == NSControlStateValueOn;
                let Ok(idx) = usize::try_from(tag) else { return };
                let Some(&id) = PanelId::ALL.get(idx) else {
                    return;
                };
                let _ = self.ivars().send_event(Wake::SetHudPanel { id, on });
            }
        }
    );

    impl PerfTarget {
        /// Allocate a checkbox target owning `proxy`. `mtm` proves we are on the main
        /// thread (AppKit requirement), which the winit loop guarantees.
        fn new(mtm: MainThreadMarker, proxy: EventLoopProxy<Wake>) -> Retained<Self> {
            let this = mtm.alloc().set_ivars(proxy);
            // SAFETY: plain `[super init]` on a freshly allocated instance.
            unsafe { msg_send_id![super(this), init] }
        }
    }

    /// Open (or re-open) the native Performance control panel: one checkbox per HUD panel
    /// in `toggles` (its [`PanelId`] plus current enabled state), each toggling the panel
    /// live and persisting on click, plus a Done button. Returns the retained backing
    /// objects for `App` to keep alive.
    ///
    /// Best-effort: off the main thread the window is simply not built (`None`) — never a
    /// panic. A fresh window is built each call (the previous handle is dropped by the
    /// caller, closing the old window); the checkboxes are re-seeded from live state.
    pub fn open_performance_panel(
        proxy: &EventLoopProxy<Wake>,
        toggles: &[(PanelId, bool)],
    ) -> Option<PerfPanelHandle> {
        let mtm = MainThreadMarker::new()?;
        let target = PerfTarget::new(mtm, proxy.clone());

        // Height: header + N rows + a help line + the Done button, with margins.
        let rows = toggles.len() as CGFloat;
        let win_h = MARGIN
            + HEADER_H
            + ROW_GAP
            + rows * (ROW_H + ROW_GAP)
            + ROW_H        // help line
            + ROW_GAP
            + BUTTON_H
            + MARGIN;

        // SAFETY: `initWithContentRect:styleMask:backing:defer:` is the documented
        // designated initializer (non-raising); plain setters follow on the fresh instance,
        // on the main thread (`mtm`). The content rect is a valid CGRect.
        let window = unsafe {
            let content_rect = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(WIN_W, win_h));
            let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable;
            let window = NSWindow::initWithContentRect_styleMask_backing_defer(
                mtm.alloc(),
                content_rect,
                style,
                NSBackingStoreType::NSBackingStoreBuffered,
                false,
            );
            window.setTitle(&NSString::from_str("aterm Performance"));
            // We own the window via the returned handle; do NOT let AppKit free it on close.
            window.setReleasedWhenClosed(false);
            window
        };

        let content_w = WIN_W - 2.0 * MARGIN;
        let mut y = win_h - MARGIN - HEADER_H;

        let mut subviews: Vec<Retained<NSView>> = Vec::with_capacity(toggles.len() + 3);

        // Header.
        let header = make_label(mtm, "Performance HUD panels", true);
        place(&header, MARGIN, y, content_w, HEADER_H);
        subviews.push(to_view_label(header));
        y -= HEADER_H + ROW_GAP;

        // One checkbox per panel, seeded from live state. The checkbox's `tag` is its index
        // into `PanelId::ALL` so `togglePanel:` can recover the id.
        for (id, on) in toggles {
            let check = make_checkbox(mtm, id.label(), *on, &target);
            // SAFETY: `setTag:` is a plain main-thread setter on the fresh checkbox; the
            // index is the checkbox's PanelId position (decoded back in `togglePanel:`).
            let tag = PanelId::ALL.iter().position(|p| p == id).unwrap_or(0) as isize;
            unsafe { check.setTag(tag) };
            place(&check, MARGIN, y, content_w, ROW_H);
            subviews.push(to_view_button(check));
            y -= ROW_H + ROW_GAP;
        }

        // Help line — toggles apply live + persist, so no Save button is needed.
        let help = make_label(
            mtm,
            "Changes apply instantly and are saved to aterm.toml.",
            false,
        );
        // SAFETY: `setTextColor:` is a plain main-thread setter on the fresh label.
        unsafe { help.setTextColor(Some(&NSColor::secondaryLabelColor())) };
        place(&help, MARGIN, y, content_w, ROW_H);
        subviews.push(to_view_label(help));

        // Done button (bottom-right), closing the window via the standard `performClose:`
        // (targeting the window itself, so the target object needs no window reference).
        let done = make_window_button(mtm, "Done", &window);
        place(&done, WIN_W - MARGIN - BUTTON_W, MARGIN, BUTTON_W, BUTTON_H);
        subviews.push(to_view_button(done));

        // Attach + show + center.
        // SAFETY: `contentView` is non-null for a titled window; `addSubview:` / `center` /
        // `makeKeyAndOrderFront:` are plain main-thread calls on views built on this thread.
        unsafe {
            if let Some(content) = window.contentView() {
                for v in &subviews {
                    content.addSubview(v);
                }
            }
            window.center();
            window.makeKeyAndOrderFront(None);
        }

        Some(PerfPanelHandle {
            _window: window,
            _target: target,
        })
    }

    /// Set a view's frame to `(x, y, w, h)` (points, origin bottom-left).
    fn place(view: &NSView, x: CGFloat, y: CGFloat, w: CGFloat, h: CGFloat) {
        let frame = CGRect::new(CGPoint::new(x, y), CGSize::new(w, h));
        // SAFETY: `setFrame:` is a plain main-thread setter on the live view.
        unsafe { view.setFrame(frame) };
    }

    /// Lift an `NSTextField` to its common `NSView` superclass.
    fn to_view_label(field: Retained<NSTextField>) -> Retained<NSView> {
        Retained::into_super(Retained::into_super(field))
    }

    /// Lift an `NSButton` to its common `NSView` superclass.
    fn to_view_button(button: Retained<NSButton>) -> Retained<NSView> {
        Retained::into_super(Retained::into_super(button))
    }

    /// Build a non-editable label (`labelWithString:`). `bold` selects a bold system font.
    fn make_label(mtm: MainThreadMarker, text: &str, bold: bool) -> Retained<NSTextField> {
        // SAFETY: `labelWithString:` is the documented non-raising factory; plain setters
        // follow on the fresh label; all on the main thread.
        unsafe {
            let field = NSTextField::labelWithString(&NSString::from_str(text), mtm);
            field.setDrawsBackground(false);
            field.setBezeled(false);
            field.setEditable(false);
            field.setSelectable(false);
            let size: CGFloat = 13.0;
            let font = if bold {
                NSFont::boldSystemFontOfSize(size)
            } else {
                NSFont::systemFontOfSize(size)
            };
            field.setFont(Some(&font));
            field
        }
    }

    /// Build a CHECKBOX titled `title`, seeded `on`/off, that fires `togglePanel:` on
    /// `target` when clicked (so each click applies + persists live). The caller sets the
    /// checkbox's `tag` to its [`PanelId`] index afterwards.
    fn make_checkbox(
        mtm: MainThreadMarker,
        title: &str,
        on: bool,
        target: &PerfTarget,
    ) -> Retained<NSButton> {
        use objc2_app_kit::NSControlStateValueOff;
        // SAFETY: `checkboxWithTitle:target:action:` is the documented factory; the `target`
        // outlives the call (retained in the handle); `setState:` is a plain main-thread
        // setter; on the main thread.
        unsafe {
            let target_obj: &AnyObject = target;
            let check = NSButton::checkboxWithTitle_target_action(
                &NSString::from_str(title),
                Some(target_obj),
                Some(sel!(togglePanel:)),
                mtm,
            );
            check.setState(if on {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
            check
        }
    }

    /// Build the window's DEFAULT push button titled `title` that sends `performClose:`
    /// to `window` (the standard close action), so clicking it — OR pressing Return —
    /// closes the panel. A `"\r"` key equivalent is what AppKit uses to designate the
    /// window's default button (rendered accent-filled/pulsing and fired by a plain
    /// Return), so the panel honors the "Return triggers the default option" convention;
    /// every checkbox already applies + persists live on click, so "Done" is the safe,
    /// non-destructive Return-default. The window is the action target.
    fn make_window_button(
        mtm: MainThreadMarker,
        title: &str,
        window: &NSWindow,
    ) -> Retained<NSButton> {
        // SAFETY: `buttonWithTitle:target:action:` is the documented factory; the `window`
        // is retained by the handle and responds to `performClose:`; `setKeyEquivalent:`
        // is a plain main-thread setter; on the main thread.
        unsafe {
            let target_obj: &AnyObject = window;
            let button = NSButton::buttonWithTitle_target_action(
                &NSString::from_str(title),
                Some(target_obj),
                Some(sel!(performClose:)),
                mtm,
            );
            // Return fires the default button: bind it so a plain Return closes the panel.
            button.setKeyEquivalent(&NSString::from_str("\r"));
            button
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PanelId, perf_panel_lines};

    /// The text dump lists every toggle with its config key, label, and live state — the
    /// exact source the `controls perf` introspection verb returns.
    #[test]
    fn perf_panel_lines_render_each_toggle() {
        let toggles = [(PanelId::Resources, true), (PanelId::Engine, false)];
        let lines = perf_panel_lines(&toggles);
        assert_eq!(lines[0], "perf-panel toggles=2");
        assert!(lines[1].contains("key=show_resources_hud"));
        assert!(lines[1].contains("enabled=true"));
        assert!(lines[2].contains("key=show_engine_hud"));
        assert!(lines[2].contains("enabled=false"));
        assert_eq!(lines.len(), 3);
    }
}
