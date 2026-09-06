// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm-gui's own AppKit/Foundation layer, over [`aterm_objc`].
//!
//! # Why this module exists
//!
//! `crates/aterm-objc` (W1) is the RUNTIME: typed sends, ownership, class
//! creation, the block ABI. It deliberately binds nothing from Foundation or
//! AppKit beyond the five shared value types (`CGRect`/`CGPoint`/`CGSize`/
//! `Bool`/`NSRange`), because a binding crate is exactly what the campaign is
//! deleting. This module is the thin first-party replacement for the part of
//! `objc2-app-kit` / `objc2-foundation` that `aterm-gui` actually reaches — and
//! it is deliberately NOT one Rust method per Objective-C method. It is:
//!
//! * [`MainThread`] — aterm's answer to `objc2`'s `MainThreadMarker`. W1 named
//!   it as W2's design question and W2 defined it HERE; W3 moved the type into
//!   `aterm-objc`, because `alloc_init` is where the witness has to be spent,
//!   and this module re-exports it under the same name.
//! * ONE `unsafe fn` per C PROTOTYPE, not per selector. The rule
//!   `aterm-objc` imposes is that every send casts [`aterm_objc::msg`] to the
//!   EXACT prototype of the selector; a selector's prototype is a property of
//!   its C signature, and the 206 distinct selectors `aterm-gui` sends through
//!   this module collapse to the 35 shapes below (`~30` selectors and `~20`
//!   shapes was true before W7 brought `toolbar.rs`'s 112 in; the ratio is the
//!   claim, and it went from 1.5 to 5.9 sites per prototype). `send_v_id(recv, sel!(setMenu:), menu)` is the
//!   same typed cast the W1 worked example writes out longhand — it is written
//!   once, here, instead of once per call site.
//!
//! That second point is a MEASUREMENT decision as much as an engineering one:
//! the campaign's band prices a call-site conversion at 3–7 lines, from the
//! longhand form in `platform.rs`. The helpers amortise that, and the wave
//! reports both the fixed cost of this module and the marginal cost of a site
//! that goes through it, because winit's 358 sites will pay whichever of the
//! two shapes that port chooses.
//!
//! # WHAT IS STILL ON `objc2` IN THIS CRATE: NOTHING — and this is what the
//! inventory that stood here used to say
//!
//! This heading was "WHAT IS STILL ON `objc2`, AND WHY", and it listed the
//! remaining files with a reason each, because a seam nobody wrote down is
//! indistinguishable from an oversight. W13 emptied the list. The check is
//! unchanged and is what to run rather than trust this paragraph:
//!
//! ```text
//! $ grep -rn 'use objc2\|objc2::\|objc2_app_kit::\|objc2_foundation::\|block2::' crates/aterm-gui/src
//! ```
//!
//! It answers nothing outside prose. `crates/aterm-objc/tests/objc2_exit_condition.rs`
//! is the same rule as a TEST, over a scope derived by subtraction rather than
//! remembered, and its `GUI_FILES` count is 0.
//!
//! The list as it stood, and where each entry went:
//!
//! * ~~**`toolbar.rs`**~~ — PORTED (W7). The largest entry: four first-party
//!   declared classes whose ~250 AppKit BINDING calls were still `objc2`'s,
//!   crossed at `objc2_ref` / `id_of` / the classes' own `view()`. `id_of` had
//!   no callers left and was deleted then; `objc2_ref` kept exactly one, in
//!   `app_introspect.rs` — and with that file ported it has none either and is
//!   deleted here, for the same reason and by the same rule.
//! * ~~**`alert_keys.rs` + `menu.rs::confirm` + `lib.rs`'s paste sheet**~~ —
//!   PORTED (W13), together, because they are ONE subsystem: the modal alert
//!   and its `RcBlock` key interceptor, shared between two callers. Porting
//!   either caller alone would have left two spellings of the same event
//!   monitor, which is exactly why the entry named all three.
//! * ~~**`lib.rs`'s two one-liners that are NOT that subsystem**~~ — PORTED.
//!   `MainThreadMarker::new()` became [`MainThread::new`], a drop-in; `NSBeep`
//!   is a plain C function and became [`beep`], bound in this module beside the
//!   `NSAppearanceName` globals because it is the same kind of thing they are.
//! * ~~**`app_introspect.rs`**~~ — PORTED (W13). The `chrome` verb's AppKit
//!   readback and the window-CAPTURE path, whole, as its entry said it would
//!   have to go.
//!
//! This module's own test-only reference went with them: `consts_tests` diffed
//! every ported constant against the `objc2-app-kit` expression it replaced,
//! and said itself that it could only live while that crate was a dependency.
//! Its replacement is stronger and is described at the foot of this file.
//!
//! **What this does NOT do is remove a package**, and the distinction matters
//! as much now as when this note said it about W2. `aterm-gui`'s four manifest
//! rows are still here after this wave by design — retiring them is a separate,
//! single commit, because that is where the package set moves — and vendored
//! `winit` reaches all four crates independently through EIGHT files of its own
//! macOS backend that are NOT ported (`app.rs`, `app_state.rs`,
//! `aterm_objc_seam.rs`, `event_loop.rs`, `menu.rs`, `monitor.rs`,
//! `observer.rs`, `window.rs`). Measured with `cargo forge survey --cell
//! mac-arm` at the commit this wave branched from: 47 third-party packages,
//! 563,759 LOC, 24,865 unsafe tokens. Re-run the command rather than trust the
//! number — a figure with "today" in it goes stale by the next commit, which is
//! why the sentence this replaces had to be corrected twice already.
//!
//! # Ownership
//!
//! These helpers return a RAW [`Id`]. They cannot know whether the selector
//! they were handed returns +1 or +0, so the caller decides, exactly as it must
//! in Objective-C: an `alloc`/`new`/`copy`/`mutableCopy` result goes into
//! [`aterm_objc::Obj`] or [`aterm_objc::Retained`]; anything else is BORROWED
//! and lives until the enclosing [`aterm_objc::autoreleasepool`] pops.
//!
//! # Safety
//!
//! Every function here is `unsafe` for one reason: the caller asserts that the
//! selector it passes really has the C prototype named in the function's name,
//! and that `recv` is a live receiver that responds to it. Picking the wrong
//! helper is the same defect as writing the wrong cast by hand, and has the
//! same consequence — corrupted registers on both Apple ABIs.

use aterm_objc::Id;

/// A witness that the current thread is the process main thread.
///
/// # It moved, and the move is the point
///
/// This module DEFINED its own `MainThread` for one wave: aterm's answer to
/// objc2's `MainThreadMarker`, minted by `+[NSThread isMainThread]`, `!Send`
/// so it could not be smuggled onto another thread. It did exactly one job —
/// be checked at an entry point — because `aterm_objc::declare_class!` asked
/// for no witness to instantiate a class, having dropped objc2's
/// `mutability::MainThreadOnly` on the argument that
/// `aterm_objc::Retained` is already `!Send`.
///
/// That argument is about where an instance may TRAVEL, not where one may be
/// BORN, and W3's judge built the difference: a declared class instantiated
/// and deallocated on a spawned thread with no witness and no `unsafe`.
/// [`aterm_objc::MainThread`] is the witness moved down to where the birth
/// happens, so `alloc_init`/`alloc_ivars` cannot be reached without one — and
/// a type that lives in two crates with the same meaning is one type too many,
/// so this is now that type under this module's name. The `MainThread::new()`
/// spelling at every `aterm-gui` entry point is unchanged.
pub(crate) use aterm_objc::MainThread;

/// The instantiation witness for a UNIT TEST, which is never on the main
/// thread.
///
/// libtest runs every test on a worker — `pthread_main_np()` is 0 there even
/// under `--test-threads=1`, measured twice this campaign — so
/// [`MainThread::new`] correctly answers `None` and the checked constructor is
/// unusable in `#[test]`. The probe classes these tests instantiate are
/// `NSObject`/`NSView` subclasses that touch no AppKit state and whose `Ivars`
/// are born and dropped on the same worker, which is the second form of
/// `new_unchecked`'s obligation. Written once, here, rather than as an
/// `unsafe` block in each of the THIRTEEN test bodies that call it —
/// `toolbar.rs` 6, `menu.rs` 3, `platform.rs` 2, `status_item.rs` 2. (It said
/// "nine" from W2 until the tenth pass counted them.)
#[cfg(test)]
pub(crate) fn test_witness() -> MainThread {
    // SAFETY: see this function's doc comment — no main-thread affinity, and
    // the ivars never leave the worker that created them.
    unsafe { MainThread::new_unchecked() }
}

/// The typed [`aterm_objc::msg`] casts — ONE `unsafe fn` per C PROTOTYPE.
///
/// # These 35 functions were DEFINED here, and W8 moved them
///
/// They are the module note's second bullet: `send_v_id(recv, sel!(setMenu:),
/// menu)` written once instead of once per call site, 35 shapes serving this
/// crate's 206 distinct selectors. Every call site in `aterm-gui` —
/// `toolbar.rs`'s 268 among them — is unchanged by the move and still says
/// `appkit::send_v_id`.
///
/// WHY THEY LEFT: `vendor/winit`'s `window_delegate.rs` and `view.rs` need the
/// same shapes and CANNOT import this module, which is `pub(crate)` to
/// `aterm-gui`. The two honest options were a second copy inside the fork or
/// one copy somewhere both can reach. A second copy would have cost the
/// ratcheted `third_party_loc` ~500 lines of first-party ABI plumbing — in the
/// budget the campaign exists to SHRINK — and left two copies of a
/// safety-critical layer free to drift apart.
///
/// WHY `aterm-objc` IS WHERE THEY WENT, and not a new binding crate: not one of
/// these functions names a framework class, a selector, or a framework symbol.
/// `send_v_usize(recv, sel, a)` knows nothing about AppKit; it is a cast of
/// [`aterm_objc::msg`], which is that crate's own stated rule ("every send
/// therefore casts `msg` to the EXACT prototype of the selector"). The things
/// that DO name AppKit — [`consts`], the `NSAppearanceName*` globals,
/// [`appearance_name`] — stayed here, and the fork's equivalents stayed in the
/// fork. That line is what keeps `aterm-objc`'s "zero framework bindings" rule
/// true rather than merely restated.
///
/// The glob is deliberate: it re-exports the shapes this crate uses and the
/// ones only the fork uses, without a name list that would go stale on one side
/// of the tree every time the other side ports a file.
pub(crate) use aterm_objc::send::*;

/// An `NSString` for `s`, +1, or `None` if Foundation refused it.
///
/// A one-line alias for [`aterm_objc::ns_string`] so the ported call sites read
/// the way the `objc2` ones did (`NSString::from_str(s)`).
#[must_use]
pub(crate) fn nsstring(s: &str) -> Option<aterm_objc::Obj> {
    aterm_objc::ns_string(s)
}

/// The Rust `String` inside an `NSString`, or `String::new()` for nil.
///
/// # Safety
/// `s` must be nil or a live `NSString`.
#[must_use]
pub(crate) unsafe fn nsstring_to_rust(s: Id) -> String {
    if s.is_null() {
        return String::new();
    }
    // SAFETY: the caller guarantees `s` is a live `NSString`.
    unsafe { aterm_objc::ns_string_to_rust(s) }
}

/// AppKit and Foundation constants, at the SDK's own values.
///
/// # Every one of these is a `const`, and that is a MEASUREMENT, not a shortcut
///
/// The port's first attempt declared `NSVariableStatusItemLength` as an extern
/// static — the shape a framework constant usually takes — and the link failed
/// with `Undefined symbols: "_NSVariableStatusItemLength"`. Reading the SDK
/// explains it and generalises: every constant `aterm-gui` reaches is declared
/// in a HEADER, either as a `static const` with internal linkage or as an
/// enumerator, so there is no symbol to bind and `objc2-app-kit` is compiling
/// its own copy of the value exactly as this module does.
///
/// That is worth stating precisely because the campaign's own cost table already
/// leans on it: it prices winit's **78 `NS*::CONSTANT` paths** as "become a
/// first-party `const`, ~free" and charges them nothing. Measured here, on the
/// constants `aterm-gui` actually uses, that row is CORRECT — one line and one
/// header citation each, no runtime, no linkage, no ownership.
///
/// The `NSAppearanceName*` family is the EXCEPTION to the RULE, not to this
/// module: those two are real `NSString *` symbols rather than header values,
/// so they are bound as extern statics in the `#[link]` block below and served
/// by [`appearance_name`] — not as `const`s here. The parenthetical that used
/// to end this sentence, "see `toolbar.rs`, which still reaches them through
/// `objc2-app-kit`", was left behind by the wave that made it false:
/// `toolbar.rs` calls `appkit::appearance_name(dark)` and the file holds no
/// `objc2` token outside prose. A cross-reference to a seam that no longer
/// exists is worse than none — it sends the next reader to the one file the
/// wave emptied.
///
/// Each constant carries the SDK header and line it was read from, so the next
/// reader can re-derive it rather than trust it.
pub(crate) mod consts {
    /// `NSStatusBar.h:20` — `static const CGFloat NSVariableStatusItemLength = -1.0;`
    pub(crate) const NS_VARIABLE_STATUS_ITEM_LENGTH: f64 = -1.0;

    /// `NSEvent.h:169` — `NSEventModifierFlagShift = 1 << 17`.
    pub(crate) const NS_EVENT_MODIFIER_FLAG_SHIFT: usize = 1 << 17;

    /// `NSEvent.h:170` — `NSEventModifierFlagControl = 1 << 18`.
    pub(crate) const NS_EVENT_MODIFIER_FLAG_CONTROL: usize = 1 << 18;

    /// `NSEvent.h:172` — `NSEventModifierFlagCommand = 1 << 20`.
    pub(crate) const NS_EVENT_MODIFIER_FLAG_COMMAND: usize = 1 << 20;

    /// `NSWindow.h:71` — `static const NSModalResponse NSModalResponseOK = 1;`
    pub(crate) const NS_MODAL_RESPONSE_OK: isize = 1;

    // ---- the tab strip's constants (W7) ----
    //
    // Every value below was read TWICE, from two independent sources that can
    // disagree: the SDK header cited on its line, and `objc2-app-kit 0.2.2`'s
    // own generated binding — the exact code these replace. A single reading
    // is a guess with a citation attached; two agreeing readings are a
    // measurement, and one row here needed it (see `NS_TEXT_ALIGNMENT_CENTER`).

    /// `NSBezierPath.h:20` — `NSLineCapStyleRound = 1` (`NSUInteger`).
    pub(crate) const NS_LINE_CAP_STYLE_ROUND: usize = 1;

    /// `NSText.h:48` — `NSTextAlignmentLeft = 0` (`NSInteger`).
    pub(crate) const NS_TEXT_ALIGNMENT_LEFT: isize = 0;

    /// `NSText.h:50/54` — `NSTextAlignmentCenter`: **1 on arm64 macOS**, 2 on
    /// the x86_64 compat slice.
    ///
    /// THE ROW THAT SHIPPED A BUG, AND THE PIXELS ARE WHAT CAUGHT IT. The
    /// declaration sits inside `#if TARGET_ABI_USES_IOS_VALUES`, and this
    /// constant was first written `2` by reading that `#if/#else` as
    /// "iOS/macOS". It is not. `TargetConditionals.h:505` defines
    ///
    /// ```text
    /// TARGET_ABI_USES_IOS_VALUES  (!TARGET_CPU_X86_64 || (TARGET_OS_IPHONE && !TARGET_OS_MACCATALYST))
    /// ```
    ///
    /// so on macOS it reduces to `!TARGET_CPU_X86_64` — **true on Apple
    /// Silicon**. Measured with clang on this box: `TARGET_ABI_USES_IOS_VALUES
    /// = 1`, `NSTextAlignmentCenter = 1`, `NSTextAlignmentRight = 2`. The `2`
    /// was therefore RIGHT alignment, and every tab label in the strip is
    /// `Center`: an A/B capture of the live titlebar showed the labels of the
    /// two unselected chips displaced +47px inside cells whose pill, traffic
    /// lights and "+" had not moved a pixel. Nothing else could see it — it
    /// compiled, every test passed, and the encoding was correct.
    ///
    /// Two readings of a value are not a measurement when the second is read as
    /// agreement rather than evaluated. [`super::consts_tests`] now evaluates
    /// `objc2-app-kit`'s own constants and compares, so this cannot drift again
    /// while that crate is still in the tree.
    ///
    /// BOTH ARMS ARE MEASURED, including the one this box cannot execute. These
    /// are compile-time constants, so COMPILING a `_Static_assert` for an arch
    /// is the measurement and no x86_64 binary has to run:
    ///
    /// ```text
    /// $ cat > /tmp/ta.m <<'EOF'
    /// #import <Cocoa/Cocoa.h>
    /// #include <TargetConditionals.h>
    /// #if TARGET_CPU_X86_64
    /// _Static_assert(TARGET_ABI_USES_IOS_VALUES == 0, "");
    /// _Static_assert(NSTextAlignmentCenter == 2, "");
    /// #else
    /// _Static_assert(TARGET_ABI_USES_IOS_VALUES == 1, "");
    /// _Static_assert(NSTextAlignmentCenter == 1, "");
    /// #endif
    /// int main(void){return 0;}
    /// EOF
    /// $ cc -arch arm64  -fsyntax-only /tmp/ta.m   # passes
    /// $ cc -arch x86_64 -fsyntax-only /tmp/ta.m   # passes
    /// ```
    ///
    /// Inverting either assertion fails to compile, so the check is
    /// load-bearing rather than vacuous. `gate cells` has no
    /// `x86_64-apple-darwin` cell — its `win` cell is x86_64 but not Apple — so
    /// this is the only thing standing behind the `= 2` arm.
    #[cfg(target_arch = "x86_64")]
    pub(crate) const NS_TEXT_ALIGNMENT_CENTER: isize = 2;
    /// See the `x86_64` twin above: `TARGET_ABI_USES_IOS_VALUES` is TRUE here.
    #[cfg(not(target_arch = "x86_64"))]
    pub(crate) const NS_TEXT_ALIGNMENT_CENTER: isize = 1;

    /// `NSCell.h:49` — `NSNoImage = 0` (`NSCellImagePosition`, `NSUInteger`).
    pub(crate) const NS_NO_IMAGE: usize = 0;

    /// `NSTrackingArea.h:19,29,35` — `NSTrackingMouseEnteredAndExited` (0x01)
    /// `| NSTrackingActiveAlways` (0x80) `| NSTrackingInVisibleRect` (0x200),
    /// the one combination the strip installs: hover in/out, regardless of app
    /// activation, following the view's visible rect across resizes.
    pub(crate) const NS_TRACKING_HOVER_IN_VISIBLE_RECT: usize = 0x01 | 0x80 | 0x200;

    /// `NSView.h:35,40` — `NSViewMinXMargin` (1) `| NSViewMaxYMargin` (32):
    /// right-anchored, top-anchored. The "+" button's mask between re-layouts.
    pub(crate) const NS_VIEW_MIN_X_MARGIN_MAX_Y_MARGIN: usize = 1 | 32;

    /// `NSView.h:36` — `NSViewWidthSizable = 2`. The strip container's mask.
    pub(crate) const NS_VIEW_WIDTH_SIZABLE: usize = 2;

    /// `NSWindow.h:215-217` — `NSWindowCloseButton` (0),
    /// `NSWindowMiniaturizeButton` (1), `NSWindowZoomButton` (2), in the order
    /// `strip_metrics` measures them. `NSUInteger`, so the send is
    /// [`super::send_id_usize`].
    pub(crate) const NS_WINDOW_CLOSE_BUTTON: usize = 0;
    pub(crate) const NS_WINDOW_MINIATURIZE_BUTTON: usize = 1;
    pub(crate) const NS_WINDOW_ZOOM_BUTTON: usize = 2;

    /// `NSGraphics.h:104` — `NSWindowAbove = 1` (`NSInteger`).
    pub(crate) const NS_WINDOW_ABOVE: isize = 1;

    /// `NSToolbar.h:26` — `NSToolbarDisplayModeIconOnly = 2` (`NSUInteger`).
    pub(crate) const NS_TOOLBAR_DISPLAY_MODE_ICON_ONLY: usize = 2;

    /// `NSWindow.h:248` — `NSWindowToolbarStyleUnifiedCompact = 4`
    /// (`NSInteger`). The single compact chrome row the whole strip exists to
    /// live in.
    pub(crate) const NS_WINDOW_TOOLBAR_STYLE_UNIFIED_COMPACT: isize = 4;

    /// `NSWindow.h:231` — `NSWindowTitleHidden = 1` (`NSInteger`).
    pub(crate) const NS_WINDOW_TITLE_HIDDEN: isize = 1;

    /// `NSParagraphStyle.h:26,30` — `NSLineBreakByWordWrapping = 0` and three
    /// implicit enumerators after it, so `NSLineBreakByTruncatingTail = 4`
    /// (`NSLineBreakMode`, `NSUInteger`). A title too long for its chip must
    /// end in an ELLIPSIS, not simply stop.
    pub(crate) const NS_LINE_BREAK_BY_TRUNCATING_TAIL: usize = 4;

    // ---- W13: the modal-alert subsystem's constants ----

    /// `NSEvent.h:103` — `NSEventMaskKeyDown = 1ULL << NSEventTypeKeyDown`,
    /// and `NSEvent.h:34` — `NSEventTypeKeyDown = 10`. So `1 << 10` = 1024, an
    /// `NSEventMask` (`unsigned long long`, passed as `NSUInteger` here because
    /// that is what `+addLocalMonitorForEventsMatchingMask:handler:` takes and
    /// both are 64-bit on every Apple target this compiles for).
    ///
    /// THE ONE MASK `alert_keys` INSTALLS. A wider mask would hand the
    /// interceptor mouse and flags-changed events it has no answer for; a
    /// narrower one would miss the Return this module exists to route.
    pub(crate) const NS_EVENT_MASK_KEY_DOWN: usize = 1 << 10;

    /// `NSAlert.h:50` — `static const NSModalResponse NSAlertFirstButtonReturn
    /// = 1000;` (`NSModalResponse` is `NSInteger`).
    ///
    /// The FIRST button added to an `NSAlert` is its default, so this is the
    /// affirmative answer for both confirmations: `menu::confirm`'s proceed and
    /// the paste sheet's "Paste". It was a bare `1000` at both sites with the
    /// name only in a comment.
    pub(crate) const NS_ALERT_FIRST_BUTTON_RETURN: isize = 1000;

    // ---- W13: the `chrome` introspection reader's constants ----
    //
    // `NSWindowToolbarStyle` (`NSWindow.h:243-249`) and `NSToolbarDisplayMode`
    // (`NSToolbar.h:23-28`) are both IMPLICIT enumerations — no enumerator
    // carries a value, so each is its ordinal. They are read back and turned
    // into words by `app_introspect::read_native_chrome`, which is the only
    // consumer; `UNIFIED_COMPACT` and `ICON_ONLY` already existed above because
    // `toolbar.rs` SETS them, and the pair of readers and writers agreeing is
    // itself checked by the `_Static_assert` test.

    /// `NSWindow.h:244` — `NSWindowToolbarStyleAutomatic` (`NSInteger`), the
    /// first enumerator of an implicit enum, so 0.
    pub(crate) const NS_WINDOW_TOOLBAR_STYLE_AUTOMATIC: isize = 0;
    /// `NSWindow.h:245` — `NSWindowToolbarStyleExpanded`.
    pub(crate) const NS_WINDOW_TOOLBAR_STYLE_EXPANDED: isize = 1;
    /// `NSWindow.h:246` — `NSWindowToolbarStylePreference`.
    pub(crate) const NS_WINDOW_TOOLBAR_STYLE_PREFERENCE: isize = 2;
    /// `NSWindow.h:247` — `NSWindowToolbarStyleUnified`.
    pub(crate) const NS_WINDOW_TOOLBAR_STYLE_UNIFIED: isize = 3;

    /// `NSToolbar.h:24` — `NSToolbarDisplayModeDefault` (`NSUInteger`).
    pub(crate) const NS_TOOLBAR_DISPLAY_MODE_DEFAULT: usize = 0;
    /// `NSToolbar.h:25` — `NSToolbarDisplayModeIconAndLabel`.
    pub(crate) const NS_TOOLBAR_DISPLAY_MODE_ICON_AND_LABEL: usize = 1;
    /// `NSToolbar.h:27` — `NSToolbarDisplayModeLabelOnly`.
    pub(crate) const NS_TOOLBAR_DISPLAY_MODE_LABEL_ONLY: usize = 3;

    // ---- W13: the titlebar snapshot's constants ----

    /// `NSBitmapImageRep.h:37` — `NSBitmapImageFileTypePNG` (`NSUInteger`), the
    /// fifth enumerator of an implicit enum, so 4.
    pub(crate) const NS_BITMAP_IMAGE_FILE_TYPE_PNG: usize = 4;

    /// `NSGraphics.h:129` — `NSColorRenderingIntentPerceptual` (`NSInteger`),
    /// the FOURTH enumerator of an implicit enum, so **3**.
    ///
    /// THIS ROW WAS WRITTEN `1` FIRST, and the `_Static_assert` probe rejected
    /// it before a line of it compiled — `expression evaluates to '3 == 1'`. It
    /// is `NS_TEXT_ALIGNMENT_CENTER`'s shape exactly: an implicit enumerator
    /// whose ordinal a reader guesses from the name's prominence rather than
    /// from its POSITION, and one that could never have been caught at run time
    /// here (a wrong intent produces a subtly different conversion of the
    /// titlebar's colours, not a failure). The instrument earned its place on
    /// its first row.
    pub(crate) const NS_COLOR_RENDERING_INTENT_PERCEPTUAL: isize = 3;
}

// The two `NSAppearanceName` constants — the EXCEPTION the [`consts`] note
// names, bound the way a real symbol has to be.
//
// Every other AppKit constant `aterm-gui` reaches is a header value with no
// symbol to bind, which is why [`consts`] is a list of `const`s. These two are
// the opposite and the difference is MEASURED, not assumed: `NSAppearance.h:63-64`
// declares them `APPKIT_EXTERN NSAppearanceName const`, `AppKit.tbd` exports
// `_NSAppearanceNameAqua` and `_NSAppearanceNameDarkAqua`, and `dlsym` on the
// live framework resolves both to non-null addresses. Writing them as `const`
// values would be inventing an `NSString` AppKit would not recognise; writing
// the header values as `extern` was the failure that produced the `consts`
// note in the first place (`Undefined symbols: "_NSVariableStatusItemLength"`).
//
// They are `NSString *` VARIABLES, so the binding is the pointer's address and
// each read is one dereference of a live framework global — not a call.
#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {
    /// `NSAppearance.h:63` — the light system appearance's name.
    #[link_name = "NSAppearanceNameAqua"]
    static NS_APPEARANCE_NAME_AQUA: Id;
    /// `NSAppearance.h:64` — the dark system appearance's name.
    #[link_name = "NSAppearanceNameDarkAqua"]
    static NS_APPEARANCE_NAME_DARK_AQUA: Id;

    /// `NSGraphics.h:222` — `APPKIT_EXTERN void NSBeep(void);`
    ///
    /// A C FUNCTION, not a message and not a header value: the third shape a
    /// framework "constant" can take, and the one the [`consts`] note's two
    /// categories had no room for. It is bound here beside the two
    /// `NSAppearanceName` globals because it is the same kind of thing they are
    /// — a real symbol `AppKit.tbd` exports (`_NSBeep`) — and for the same
    /// reason: writing it as anything else does not link.
    #[link_name = "NSBeep"]
    fn ns_beep();
}

/// Play the user's configured macOS alert sound (the BEL bell).
///
/// # Why this is a function and not a send
///
/// `NSBeep()` is a free function in AppKit, so there is no receiver and no
/// selector — [`aterm_objc::msg`] has nothing to cast. The call is one `extern
/// "C"` invocation of a symbol the framework exports, which is why it needs no
/// `unsafe` at its call site: taking no arguments and returning nothing, it has
/// no prototype a caller could get wrong.
///
/// It honours the user's sound settings (including a muted system), and it is
/// safe from any thread — but aterm only ever calls it from the event-loop
/// thread, which is where `on_bell` runs.
pub(crate) fn beep() {
    // SAFETY: `_NSBeep` is exported by AppKit (verified in `AppKit.tbd`), takes
    // no arguments and returns nothing, so the declaration above IS its
    // complete prototype and there is no ABI question to get wrong.
    unsafe { ns_beep() }
}

/// The `NSAppearanceName` for the light or dark system appearance, +0.
///
/// Borrowed from AppKit's own global — never released, never retained.
#[must_use]
pub(crate) fn appearance_name(dark: bool) -> Id {
    // SAFETY: both symbols are AppKit globals that exist for the lifetime of
    // the process (verified present in `AppKit.tbd` and via `dlsym`); reading
    // one is a plain load of an initialised `NSString *`.
    unsafe {
        if dark {
            NS_APPEARANCE_NAME_DARK_AQUA
        } else {
            NS_APPEARANCE_NAME_AQUA
        }
    }
}

// # WHERE THE CONSTANTS ARE CHECKED NOW — the oracle changed, and this note
// is the record of the swap
//
// Until W13 this module ended in `consts_tests`: 19 of the 20 ported constants
// diffed at run time against the `objc2-app-kit` expression each had replaced.
// It was a real instrument — it existed because `NS_TEXT_ALIGNMENT_CENTER` had
// been "read twice" BY EYE and shipped as RIGHT alignment, and the compiler
// evaluating both sides is what would have caught it. It also said outright
// that it could only live while `objc2-app-kit` was still a dependency, and
// that when the last holdout was ported "the crate leaves and this module must
// go with it".
//
// This is that. **The oracle is not gone, it is REPLACED, and by a stronger
// one**: `crates/aterm-objc/tests/gui_appkit_constants.rs` `_Static_assert`s
// every constant in [`consts`] against the SDK ITSELF, on BOTH arches, by
// compiling the assertion with `clang -fsyntax-only`. Four things improve:
//
// * The authority is Apple's header rather than a third-party crate's
//   transcription of it — one fewer link in the chain to be wrong.
// * `x86_64` is covered, which the run-time diff never could on this box:
//   compiling an assertion for an arch needs no binary for that arch to run.
//   `NS_TEXT_ALIGNMENT_CENTER`'s two `#[cfg]` arms are BOTH checked, each
//   under the matching `#if`.
// * The three rows the old oracle could not reach are covered. Two were simply
//   omitted; `NS_LINE_BREAK_BY_TRUNCATING_TAIL` was UNREACHABLE, because
//   `NSLineBreakMode` lives behind an `objc2-app-kit` feature `aterm-gui` does
//   not enable — so "EVERY PORTED CONSTANT, DIFFED AGAINST THE CRATE IT
//   REPLACED" was never achievable for it. Against the SDK there is no such
//   gap.
// * Coverage runs BOTH WAYS off the SOURCE: every constant the parser finds in
//   [`consts`] must have a row, and every row must name a constant that is
//   really here. The old test was a hand-written list of `assert_eq!`s, so a
//   constant added without one was invisible to it.
//
// The SDK header and line on each constant ABOVE is what the new test's row
// table is checked against, so those citations went from documentation to
// input. `NS_COLOR_RENDERING_INTENT_PERCEPTUAL` is the first row it caught:
// written `1`, rejected before it compiled, correct at `3`.
