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
//! # WHAT IS STILL ON `objc2`, AND WHY — the inventory, not a gesture
//!
//! W2 moved all six remaining `declare_class!` sites and eight whole files.
//! What is left is listed here rather than left to be discovered, because a
//! seam nobody wrote down is indistinguishable from an oversight. The
//! authoritative check is `grep -rn 'use objc2\|objc2::\|objc2_app_kit::\|objc2_foundation::\|block2::' crates/aterm-gui/src`;
//! this is that list with its reasons.
//!
//! * ~~**`toolbar.rs`**~~ — PORTED (W7). It was the largest entry here: four
//!   first-party declared classes whose ~250 AppKit BINDING calls were still
//!   `objc2`'s, crossed at [`objc2_ref`] / `id_of` / the classes' own `view()`.
//!   `objc2-send-sites-v1` over that file is now **0 raw / 0 code**, down from
//!   268 / 267, and `sed 's://.*$::' toolbar.rs | grep objc2` finds nothing:
//!   the file holds no `objc2` token outside prose. `id_of` had no callers left
//!   anywhere and is deleted; `objc2_ref` kept exactly one, in
//!   `app_introspect.rs` below, reached through `toolbar::native_strip_container`
//!   (which now returns [`aterm_objc::Obj`]). The seam deliberately lives in
//!   the UNPORTED module rather than as the last `objc2` type in a ported one.
//! * **`alert_keys.rs` + `menu.rs::confirm` + `lib.rs`'s paste sheet** — ONE
//!   subsystem, the modal alert and its `RcBlock` key interceptor, shared
//!   between two callers. Porting either caller alone would leave two
//!   spellings of the same event monitor.
//! * **`lib.rs`, TWO one-liners that are NOT that subsystem** and that this
//!   list omitted until the tenth pass ran its own grep: `lib.rs:1505`
//!   (`objc2_foundation::MainThreadMarker::new()`, the launch-failure alert's
//!   main-thread test) and `lib.rs:15697` (`objc2_app_kit::NSBeep()`, the
//!   bell). Neither is "goes whole or not at all": [`MainThread::new`] is a
//!   drop-in for the first and `NSBeep` is a plain C function needing no
//!   binding at all. They are named here rather than left to be found, which
//!   is this list's whole purpose.
//! * **`app_introspect.rs`** — the `chrome` verb's AppKit readback and the
//!   window-CAPTURE path, which shares its `NSBitmapImageRep` machinery with
//!   `cg_capture.rs`. Mixed, so it goes whole or not at all.
//!
//! This module's OWN `objc2` reference is test-only and deliberate:
//! [`consts_tests`] diffs every ported constant against the `objc2-app-kit`
//! expression it replaced. It is an oracle against the crate being retired, so
//! it departs with it — see that module's note for what survives it.
//!
//! Nothing here removes a PACKAGE. `objc2`, `block2` and the `objc2-*`
//! bindings are still parents of `aterm-gui` through the entries above,
//! and vendored `winit` reaches them independently. `forge survey` on the
//! `mac-arm` cell was byte-identical to before W2's wave (47 third-party
//! packages, 561,477 LOC, 24,778 unsafe tokens). That is the design: dominator
//! arithmetic, not sequencing preference.
//!
//! THE PACKAGE COUNT IS STILL 47 AND THE LOC LINE IS NOT STILL 561,477, and
//! the difference is worth stating rather than quietly restating. W3 moved
//! `vendor/winit/src/platform_impl/macos/window_delegate.rs`'s declared class
//! onto `aterm_objc`, and the survey counts vendored `winit`'s OWN source in
//! the third-party column — so the line moved. It read 561,755 / 24,786 when
//! that sentence was written and it does NOT read that today: `cargo forge
//! survey` on this commit answers **47 third-party / 562,346 LOC / 24,811
//! unsafe**, byte-identical to the same command at `origin/main`, so this wave
//! moved none of it. A number with "today" in it goes stale by the next
//! commit; re-run the command rather than trust this line. Porting a fork's file makes that number go UP until the package it
//! was porting away from actually leaves, and it leaves only when every winit
//! file has stopped using it. `winit`'s dominator row is unchanged at 12
//! packages, which is the number that would move if anything had.
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

/// THE SEAM between the first-party runtime and the `objc2` bindings.
///
/// # Where it is, and it is now ONE site
///
/// This used to be a PAIR of functions crossed ~250 times, because
/// `toolbar.rs`'s four declared classes were first-party while every AppKit
/// BINDING call in the same module was `objc2`'s. W7 ported those bindings, so
/// the other direction ([`id_of`]) has no callers at all and is deleted, and
/// this one is crossed exactly ONCE:
///
/// ```text
/// $ grep -rn 'appkit::objc2_ref(' crates/aterm-gui/src
/// crates/aterm-gui/src/app_introspect.rs:6584
/// ```
///
/// The trailing `(` in that pattern is not cosmetic and the claim was WRONG
/// without it: three comments in this crate write the name in prose, so the
/// paren-less grep answers four lines while the sentence above it says one.
/// A CALL is what `(` matches.
///
/// That site is `toolbar::native_strip_container`'s only caller. The seam sits
/// in `app_introspect.rs` — which is still `objc2` and goes whole or not at
/// all, sharing its `NSBitmapImageRep` machinery with `cg_capture.rs` — rather
/// than in `toolbar.rs`, so the ported file holds no `objc2` type at all.
/// `alert_keys.rs` + `menu.rs::confirm` and the paste sheet are the other
/// entries on that list; none of them cross here.
///
/// # IT TAKES THE OWNER, NOT THE POINTER, AND THAT IS THE WHOLE POINT
///
/// The first spelling was `objc2_ref<'a, T>(id: Id) -> &'a T`: an
/// UNCONSTRAINED output lifetime, which borrowck will unify with anything the
/// caller wants. The one call site paired it with an owning [`Obj`] and then
/// SHADOWED that owner, so the retain outlived the reference and the code was
/// sound — but only by a coincidence of naming. Nothing stopped a later editor
/// from renaming the owner and dropping it, and the resulting use-after-free
/// would compile silently, because the link from the reference back to the
/// retain that keeps it alive ran through a raw pointer the compiler cannot
/// see.
///
/// Taking `&Obj` and eliding the lifetime puts that link back INTO THE TYPE:
/// the returned `&T` borrows the owner, so dropping or moving the owner while
/// the reference is live is a borrow-check error rather than a silent one. The
/// tenth pass raised the shadowing; this is the answer that does not depend on
/// a name.
///
/// # Safety
///
/// `owner` must hold a live instance of a class `T` is a correct `objc2`
/// binding for. `objc2`'s binding types are `#[repr(C)]` chains bottoming out
/// in a zero-sized `AnyObject`, so the reference borrows no bytes of its own
/// and the cast is a reinterpretation of the pointer only. Lifetime is no
/// longer part of the obligation: the signature carries it.
#[must_use]
pub(crate) unsafe fn objc2_ref<T>(owner: &aterm_objc::Obj) -> &T {
    let id = owner.id();
    debug_assert!(!id.is_null(), "objc2_ref of nil");
    // SAFETY: the caller guarantees `owner` holds a live instance of `T`'s
    // class; `T` is a zero-sized opaque binding marker, and `owner`'s retain
    // outlives the returned borrow by the signature above.
    unsafe { &*id.as_ptr().cast_const().cast::<T>() }
}

/// NINETEEN OF THE TWENTY PORTED CONSTANTS, DIFFED AGAINST THE CRATE THEY
/// REPLACED. It said EVERY and it was 17 of 20 until the tenth pass counted
/// them; the twentieth has no objc2 expression to diff against at all.
///
/// # Why this exists
///
/// [`consts`]'s own note says each value was read twice — from the SDK header
/// cited on its line, and from `objc2-app-kit 0.2.2`'s generated binding. That
/// discipline still shipped a wrong value, because the second reading was
/// performed by EYE and recorded as agreement: `NSTextAlignment::Center` is
/// spelled `Self(if TARGET_ABI_USES_IOS_VALUES { 1 } else { 2 })`, which
/// "confirms" whichever branch the reader already believed. Only evaluating it
/// distinguishes them, and on this arch it evaluates to the branch the prose had
/// ruled out. The cost was a live tab strip whose labels were right-aligned.
///
/// So the second reading is done by the COMPILER here instead. Every row is a
/// constant this module now owns against the `objc2` expression it replaced, on
/// whatever arch the test is built for — which is also why the `x86_64` slice
/// gets the same check for free when it is built, without this box being able to
/// execute it.
///
/// The three that were missing are NAMED rather than quietly added, because
/// WHICH three is the finding. `NS_MODAL_RESPONSE_OK` and
/// `NS_VARIABLE_STATUS_ITEM_LENGTH` were simply omitted and are covered below.
///
/// `NS_LINE_BREAK_BY_TRUNCATING_TAIL` is the interesting one and it is
/// COVERED HERE BY NOTHING, because it CANNOT be: `NSLineBreakMode` lives
/// behind `objc2-app-kit`'s `NSParagraphStyle` feature, which `aterm-gui` does
/// not enable — which is why even the pre-port code sent that selector with
/// this module's own constant through a raw `objc2::msg_send!` rather than a
/// typed binding. So the crate this oracle diffs against has no expression for
/// it, and "EVERY PORTED CONSTANT, DIFFED AGAINST THE CRATE IT REPLACED" was
/// never achievable for that row. It is also a W7 row (the tab label's
/// ellipsis) and was the ONE constant in this module whose doc cited a header
/// with NO line number — least-checked on both instruments at once, which is
/// the exact profile of `NS_TEXT_ALIGNMENT_CENTER`, the row that shipped
/// wrong. Its line number is now cited, and its only oracle is the SDK:
///
/// ```text
/// $ printf '#import <Cocoa/Cocoa.h>\n_Static_assert(NSLineBreakByTruncatingTail == 4, "");\nint main(void){return 0;}\n' > /tmp/lb.m
/// $ cc -arch arm64 -fsyntax-only /tmp/lb.m   # passes; == 3 fails
/// ```
///
/// All three values are in fact correct — a `_Static_assert` over all 20
/// against the SDK compiles for `arm64` and `x86_64` and fails when any row is
/// inverted — so the defect was in the guard's claim about itself, not in a
/// value.
///
/// These asserts are only possible while `objc2-app-kit` is still a dependency
/// of `aterm-gui` (through `app_introspect.rs`, `alert_keys.rs` and `menu.rs`'s
/// confirm sheet — the named seam list). When the last of those is ported the
/// crate leaves and this module must go with it; the SDK header citations on
/// each constant are what survive it, and the `clang` measurement in
/// `NS_TEXT_ALIGNMENT_CENTER`'s doc is the reproduction recipe.
#[cfg(test)]
mod consts_tests {
    use objc2_app_kit::{
        NSAutoresizingMaskOptions, NSCellImagePosition, NSEventModifierFlags, NSLineCapStyle,
        NSTextAlignment, NSToolbarDisplayMode, NSTrackingAreaOptions, NSWindowButton,
        NSWindowOrderingMode, NSWindowTitleVisibility, NSWindowToolbarStyle,
    };

    use super::consts::*;

    /// The signed (`NSInteger`) enumerators.
    #[test]
    fn every_signed_constant_equals_the_objc2_value_it_replaced() {
        assert_eq!(NS_MODAL_RESPONSE_OK, objc2_app_kit::NSModalResponseOK);
        assert_eq!(
            NS_TEXT_ALIGNMENT_CENTER,
            NSTextAlignment::Center.0,
            "the tab labels' alignment — the row that shipped wrong; see its doc"
        );
        assert_eq!(NS_TEXT_ALIGNMENT_LEFT, NSTextAlignment::Left.0);
        assert_eq!(NS_WINDOW_ABOVE, NSWindowOrderingMode::NSWindowAbove.0);
        assert_eq!(
            NS_WINDOW_TOOLBAR_STYLE_UNIFIED_COMPACT,
            NSWindowToolbarStyle::UnifiedCompact.0
        );
        assert_eq!(
            NS_WINDOW_TITLE_HIDDEN,
            NSWindowTitleVisibility::NSWindowTitleHidden.0
        );
    }

    /// The unsigned (`NSUInteger`) enumerators and bitmasks.
    #[test]
    fn every_unsigned_constant_equals_the_objc2_value_it_replaced() {
        assert_eq!(NS_LINE_CAP_STYLE_ROUND, NSLineCapStyle::Round.0);
        assert_eq!(NS_NO_IMAGE, NSCellImagePosition::NSNoImage.0);
        assert_eq!(
            NS_TOOLBAR_DISPLAY_MODE_ICON_ONLY,
            NSToolbarDisplayMode::IconOnly.0
        );
        assert_eq!(
            NS_WINDOW_CLOSE_BUTTON,
            NSWindowButton::NSWindowCloseButton.0
        );
        assert_eq!(
            NS_WINDOW_MINIATURIZE_BUTTON,
            NSWindowButton::NSWindowMiniaturizeButton.0
        );
        assert_eq!(NS_WINDOW_ZOOM_BUTTON, NSWindowButton::NSWindowZoomButton.0);
        assert_eq!(
            NS_TRACKING_HOVER_IN_VISIBLE_RECT,
            (NSTrackingAreaOptions::NSTrackingMouseEnteredAndExited
                | NSTrackingAreaOptions::NSTrackingActiveAlways
                | NSTrackingAreaOptions::NSTrackingInVisibleRect)
                .0,
            "the hover tracking area the ✕ reveal depends on"
        );
        assert_eq!(
            NS_VIEW_MIN_X_MARGIN_MAX_Y_MARGIN,
            (NSAutoresizingMaskOptions::NSViewMinXMargin
                | NSAutoresizingMaskOptions::NSViewMaxYMargin)
                .0
        );
        assert_eq!(
            NS_VIEW_WIDTH_SIZABLE,
            NSAutoresizingMaskOptions::NSViewWidthSizable.0
        );
        assert_eq!(
            NS_EVENT_MODIFIER_FLAG_CONTROL,
            NSEventModifierFlags::NSEventModifierFlagControl.0,
            "the ctrl-click that pops the tab context menu"
        );
        assert_eq!(
            NS_EVENT_MODIFIER_FLAG_SHIFT,
            NSEventModifierFlags::NSEventModifierFlagShift.0
        );
        assert_eq!(
            NS_EVENT_MODIFIER_FLAG_COMMAND,
            NSEventModifierFlags::NSEventModifierFlagCommand.0
        );
    }

    /// The one `CGFloat` constant, which is neither enumerator nor bitmask.
    ///
    /// `NSVariableStatusItemLength` is the row whose FIRST spelling here was an
    /// `extern static` and failed to link (`Undefined symbols:
    /// "_NSVariableStatusItemLength"`) — the failure that produced the
    /// [`consts`] note. It is a header value, and `objc2-app-kit` compiles its
    /// own copy exactly as this module does; comparing them is what says so.
    #[test]
    fn the_float_constant_equals_the_objc2_value_it_replaced() {
        assert!(
            (NS_VARIABLE_STATUS_ITEM_LENGTH - objc2_app_kit::NSVariableStatusItemLength).abs()
                < f64::EPSILON,
            "left {NS_VARIABLE_STATUS_ITEM_LENGTH}, right {}",
            objc2_app_kit::NSVariableStatusItemLength
        );
    }

    /// The `NSAppearanceName` globals are the same POINTERS AppKit hands
    /// `objc2`, not merely equal strings.
    ///
    /// [`super::appearance_name`] binds `_NSAppearanceNameAqua` /
    /// `_NSAppearanceNameDarkAqua` as extern statics — the documented exception
    /// to `consts`'s "framework constants are header values" rule. Comparing
    /// the raw addresses proves the linker resolved this module's declaration
    /// to the same object `objc2-app-kit` reaches, which a string comparison
    /// would not: a wrong-but-equal `NSString` would pass that and still fail
    /// `+appearanceNamed:`.
    #[test]
    fn the_appearance_names_are_appkits_own_globals() {
        for (dark, want) in [
            (true, unsafe { objc2_app_kit::NSAppearanceNameDarkAqua }),
            (false, unsafe { objc2_app_kit::NSAppearanceNameAqua }),
        ] {
            let ours = super::appearance_name(dark);
            assert!(!ours.is_null(), "dark={dark}: the extern static is nil");
            assert_eq!(
                ours.as_ptr().cast_const().cast::<std::ffi::c_void>(),
                std::ptr::from_ref(want).cast::<std::ffi::c_void>(),
                "dark={dark}: not the same global objc2 binds"
            );
        }
    }
}
