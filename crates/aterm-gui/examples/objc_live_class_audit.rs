// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LIVE-CLASS AUDITOR: what `vendor/winit`'s macOS backend actually
//! registered — all four of the classes it declares plus the one row it
//! swizzles into a framework class — asked of the running Objective-C runtime
//! and answered by the runtime's own authority.
//!
//! # The defect this exists to close, and why nothing else closed it
//!
//! W3 ported `vendor/winit/src/platform_impl/macos/window_delegate.rs` off
//! `objc2::declare_class!` onto [`aterm_objc::declare_class!`] — 23 declared
//! methods, and NOTHING IN CI READ THE REGISTERED CLASS. The census in
//! `crates/aterm-objc/tests/winit_seam.rs` is a strong test of the ENCODING
//! CONTRACT, but it checks a `PortedShapes` class it declares in the test file
//! from Rust signatures it also writes. It is a mirror. Two plants, each
//! compile-verified against the real fork, proved what a mirror is worth:
//!
//! * `windowDidResize:`'s argument retyped [`aterm_objc::Id`] ->
//!   [`aterm_objc::Bool`], so the class registers `v@:B` where
//!   `NSWindowDelegate` says `v@:@`. `cargo build -p aterm-gui` exit 0,
//!   `cargo test -p aterm-objc --test winit_seam` 6/6 PASS, and the DRIVEN
//!   EVENT LOG was byte-equivalent to clean head — a delegate row AppKit
//!   reaches by direct `objc_msgSend` with an argument the body never reads
//!   cannot show the damage by being called.
//! * `NSWindowDelegate` deleted from the `protocols:` list. Build 0, tests 6/6
//!   PASS. Every encoding still correct; the class simply stopped SAYING it was
//!   a window delegate, which is the question AppKit asks before it will treat
//!   one as such.
//!
//! Both were caught only by an out-of-tree driver. This file is that driver,
//! in the tree, wired into `aterm verify`'s ladder — see
//! `aterm_verify::stages::objc_audit_outcome` for the exit-code contract.
//!
//! # How it refuses to be a mirror
//!
//! It never says what a class should contain. It asks
//! [`aterm_objc::class_methods`] for the WHOLE registered table of each class
//! AppKit is holding on a real `NSWindow` — its delegate, and its content view
//! — and then, for each row it finds, goes looking for that selector's
//! authority: [`aterm_objc::protocol_method_types`] over the protocols
//! [`derived_authority_protocols`] reads off the runtime, then
//! [`aterm_objc::method_types`] over its `authority_classes` (the superclass
//! chain, where an OVERRIDE's original lives). A row whose registered encoding
//! disagrees with its authority is a finding. A row with NO authority is also a
//! finding — and the finding NAMES the protocol that declares it, if one does,
//! after searching every protocol loaded in the process; see
//! [`protocols_declaring`] and the section on the false universal that
//! search exists to make impossible.
//!
//! Every selector, every encoding and every authority in the verdict comes from
//! `libobjc`. The only things written down are each [`Target`]'s `class_name`,
//! which is how the audit finds the class at all, and its `claimed` list, whose
//! role is described below.
//!
//! # FIVE targets, because a port that outruns its gate is the D1 failure
//!
//! `view.rs` — 43 declared rows, the whole `NSTextInputClient` surface, three
//! struct returns and the first non-`NSObject` superclass this crate registers
//! — landed the wave after this file was written for `window_delegate.rs`
//! alone. Auditing one class and porting another is D1 again, one file later,
//! so [`Target`] exists and the classes are audited from the same code.
//!
//! W6 ported the last two shipping classes and added their targets IN THE SAME
//! WAVE, plus a fifth for the one row that is not a declared class at all.
//! Every instance is reachable in the process this example already builds:
//!
//! | target | instance | rows |
//! |---|---|---|
//! | [`DELEGATE`] `WinitWindowDelegate` | `-[[view window] delegate]` | 23 + `dealloc` |
//! | [`VIEW`] `WinitView` | the window handle winit hands out | 43 + `dealloc` |
//! | [`APP_DELEGATE`] `WinitApplicationDelegate` | `-[NSApp delegate]` | 3 + `dealloc` |
//! | [`WINDOW`] `WinitWindow` | `-[view window]` | 2 + `dealloc` |
//! | [`APP`] `NSApplication` | `NSApp` | 1 of 791 — see [`Rows::Patched`] |
//!
//! # THE ONE EXCEPTION TO "THE GATE MUST READ THE REGISTERED CLASS"
//!
//! That rule — every declared row is read off the live class, never off a
//! table — has exactly one exception in this tree, and nothing named it until
//! W6. `vendor/winit/src/platform_impl/macos/app.rs:185` declares a
//! `sendEvent:` row on a `TestApplication` class, and it is NOT PRODUCT CODE:
//! it sits inside `#[cfg(test)] mod tests`, in `fn test_custom_class()`, with a
//! `todo!()` body, instantiated by that one test and by nothing else. No
//! live-class audit can ever read it, because no instance of it exists in a
//! shipping process — `declare_class!` registers lazily on first `::class()`,
//! so the class is not even present. A roadmap that priced it as a sixth
//! remaining declared row was pricing a row that cannot be audited and whose
//! port would move nothing off `objc2`; the number was FIVE.
//!
//! The exception is narrow and it is ENFORCED rather than trusted:
//! `crates/aterm-objc/tests/winit_seam.rs` asserts that the remaining
//! declaration sits after `#[cfg(test)]` in the file, so promoting it to
//! product code goes RED and re-incurs both the port and a target here. And
//! what `app.rs` really SHIPS is audited — see [`APP`].
//!
//! # WHERE THE AUTHORITY LIST COMES FROM, and the false universal it printed
//!
//! It is DERIVED — see [`derived_authority_protocols`] — and until this wave it
//! was four hand-written constants, which is the defect that produced the
//! finding this section replaces.
//!
//! A plant on `WinitWindow`'s `gestureRecognizerShouldBegin:` was reported RED
//! by the old form, with the text "nothing in the runtime declares it, no
//! sibling family covers it, and part D holds it to no shape — this row cannot
//! be checked". THE VERDICT WAS RIGHT AND THE SENTENCE WAS FALSE.
//! `NSGestureRecognizerDelegate` declares that selector `B24@0:8@16`, it is one
//! of the SEVENTEEN protocols `NSWindow` claims in its own protocol list, and
//! the only reason the audit could not see it is that [`WINDOW`]'s
//! `authority_protocols` was typed out as `&[c"NSObject"]`. The text sent a
//! maintainer to the [`Unchecked`] list — an exemption, with a shape written
//! down beside it — when a one-name addition would have checked the row against
//! AppKit's own declaration. Measured over EVERY protocol the process has
//! loaded — 10,475 of them in this binary — exactly one declares that selector,
//! and it is one the class under audit already conforms to.
//!
//! So the list is derived, from three sources, IN THIS ORDER:
//!
//!  1. the target's `claimed` list — the fork's own `protocols:` line,
//!  2. the names on its `informal` list, which nothing claims (below),
//!  3. every protocol the SUPERCLASS CHAIN claims,
//!
//! each expanded transitively through protocol inheritance (`NSTouchBarProvider`
//! arrives this way, from `NSTouchBarProviderContainer` as well as directly).
//!
//! # Why it starts at the SUPERCLASS, and how plant two stays fatal
//!
//! Deriving the list from [`aterm_objc::class_protocols`] OF THE AUDITED CLASS
//! would defeat the whole instrument against plant two: a class that drops
//! `NSWindowDelegate` would also drop the authority its 18 window rows are
//! checked against, every row would fall through to "no authority", and the two
//! findings would be the same finding. That argument is about the SUBCLASS's own
//! list, and it survives intact — source 3 begins at `superclass_of(cls)`, and
//! the audited class's own claims enter only through source 1, which is written
//! down. Delete `NSWindowDelegate` from the fork's `protocols:` line and every
//! one of those 18 rows is still checked against it, exactly as before.
//!
//! The widening is verdict-neutral TODAY and that is measured rather than
//! hoped: over all 72 declared rows plus `-dealloc`, on all five targets, the
//! derived list resolves every row to the same authority and the same encoding
//! as the four constants it replaces — zero differences. Two rows gain a SECOND
//! agreeing declaration (`touchBar` from `NSTouchBarProviderContainer` as well
//! as `NSTouchBarProvider`, `doCommandBySelector:` from
//! `NSStandardKeyBindingResponding` as well as `NSTextInputClient`), which is
//! why [`Audit::authority_of`] requires ALL hits to agree and reports a
//! disagreement as a finding rather than silently taking the first. And
//! `NSApplication`'s 23 inherited protocols declare `sendEvent:` nowhere, so
//! [`APP`]'s authority is still the class itself and the tautology argument
//! under [`Audit::patched_rows`] is unchanged.
//!
//! The CLAIM is then checked separately and twice:
//!
//! * DERIVED, with nothing written down: if any registered row's authority came
//!   from protocol `P`, the INSTANCE must answer YES to `-conformsToProtocol:P`.
//!   A class that implements `NSWindowDelegate`'s rows without conforming to it
//!   is exactly plant two, and this tooth needs no list to find it.
//!
//!   The tooth was first written as "the CLASS must CLAIM `P`", i.e. against
//!   `class_copyProtocolList`. That was right for `WindowDelegate` and wrong in
//!   general, and `WinitView` refuted it the day it was audited: `touchBar`'s
//!   authority is `NSTouchBarProvider`, which `NSView` conforms to by
//!   inheritance, so the subclass neither claims nor needs to. Asking the
//!   INSTANCE is both the weaker-sounding and the stronger question — it is the
//!   one AppKit itself asks — and plant two still fails it, because `NSObject`
//!   does not conform to `NSWindowDelegate` by any route.
//!
//!   A second refutation came out of the same run and is handled by NAME rather
//!   than by weakening: `NSStandardKeyBindingResponding` supplies the authority
//!   for `insertTab:` and `cancelOperation:`, and NOTHING ON THIS CHAIN
//!   conforms to it — not `WinitView`, not `NSView`, not `NSResponder`, not
//!   `NSObject`. The width of that sentence is the point, and it used to be
//!   written as a universal that is FALSE: over the 26,318 classes loaded with
//!   AppKit, exactly ONE names the protocol in its own list — `NSTextView`, the
//!   class that implements the key-binding surface for real — and both
//!   `class_conformsToProtocol` and `-conformsToProtocol:` answer YES for it.
//!   It is an INFORMAL protocol ON THE RESPONDER CHAIN, dispatched there by
//!   `-respondsToSelector:`. Each target therefore carries an `informal` list,
//!   with the reason written down, and an entry that stops being true
//!   (something on THIS chain starts conforming) is itself a finding.
//! * DECLARED, as a backstop: each target's `claimed` list is the `protocols:`
//!   list of the fork's `declare_class!` site. It is the only mirror in this
//!   file, it catches the case the derived tooth cannot (a protocol dropped
//!   together with every method that would have named it), and it is a list of
//!   THREE NAMES for the delegate and ONE for the view — not of 23 or 43
//!   signatures. It is also source 1 of the authority list above, which is what
//!   keeps plant two fatal after the derivation.
//!
//! Both are then re-asked of the INSTANCE through `-conformsToProtocol:`, which
//! is the actual question AppKit asks.
//!
//! # WHAT FOUNDATION ACTUALLY READS, and the two teeth that were tautologies
//!
//! This file used to carry two elaborate checks on
//! `firstRectForCharacterRange:actualRange:` — that
//! `-[NSMethodSignature methodReturnLength]` is 32, and that a direct send and
//! an `NSInvocation` through that signature answer the same rectangle — under
//! a comment saying "this number comes from OUR registered string" and
//! "Foundation's frame layout, computed from the REGISTERED encoding". BOTH
//! SENTENCES ARE FALSE, and both checks were incapable of failing on the row
//! they were written for.
//!
//! `-[NSObject methodSignatureForSelector:]` answers from the PROTOCOL, not
//! from the registered encoding, whenever the class conforms to a protocol
//! declaring that selector. Measured, and re-measured in this process every run
//! by [`Audit::teeth_are_live`]: a class claiming `NSTextInputClient` that
//! registers `{_Lie=qq}` for `firstRectForCharacterRange:` still reports
//! `retLen=32` and `{CGRect={CGPoint=dd}{CGSize=dd}}`, while the same class
//! WITHOUT the protocol reports the lie. `WinitView` claims the protocol, so
//! for its eleven `NSTextInputClient` rows Foundation never looks at what
//! `class_addMethod` was handed at all.
//!
//! Proved from both ends before it was believed: a plant registering
//! `{_PlantRect=qqqq}` for that row — a 32-byte NON-HFA, returned indirectly
//! through `x8`, against an IMP that returns an HFA in `d0`-`d3`, which is the
//! textbook garbage-rectangle setup — passed both teeth, and so did a 16-byte
//! version of the same lie. What caught them was part A, comparing the
//! registered string against the protocol's directly.
//!
//! So the teeth were RE-AIMED rather than deleted, at the rows where the same
//! mechanism says they bite:
//!
//! * On a protocol row, the question worth asking is whether the string
//!   Foundation will use (the protocol's) and the string the class registered
//!   are the SAME string — because when they differ it is the IME's candidate
//!   window that is laid out from one while the IMP was compiled for the other.
//!   That is what [`Audit::first_rect_through_foundation`] asks now, and it is
//!   a real question with a real failing answer.
//! * On a row NO protocol declares, `methodSignatureForSelector:` genuinely IS
//!   computed from our string — measured in the same probe — and there the
//!   `NSInvocation`-against-direct comparison detects a lying encoding
//!   exactly as the old comment claimed: the honest twin answers
//!   `{704, 839, 11, 22}` both ways and the lying one answers `{0, 0, 0, 0}`
//!   through `NSInvocation`. [`Audit::teeth_are_live`] asserts that
//!   DIVERGENCE, in this process, on every run. A tooth that cannot fail is
//!   the defect this whole file exists to remove, and the only defence against
//!   writing another one is to make each tooth draw blood on a control before
//!   it is trusted on the real class.
//!
//! # PART D, and the two rows that were excused for being uncheckable
//!
//! `frameDidChange:` and `insertBackTab:` are the rows nothing in the runtime
//! declares. They used to be listed as exemptions from the "every row must be
//! checkable" rule and then checked by NOTHING — while the row this file could
//! not check (`firstRectForCharacterRange:`, protocol-answered) was the one it
//! checked most elaborately. Two compile-verified plants settled what that was
//! worth: `insert_back_tab` retyped to take a `Bool` (registering `v@:B` where
//! AppKit's key-binding dispatch passes an object pointer) and
//! `frame_did_change` retyped to RETURN one, on the path every window resize
//! posts through. Auditor exit 0, build exit 0, census 6/6, IME drive exit 0.
//!
//! Both are checked now, by the cheapest thing that is not a mirror:
//!
//! * `insertBackTab:` gets a DERIVED authority and leaves the exemption list.
//!   `NSStandardKeyBindingResponding` declares `insertTab:` and
//!   `cancelOperation:` — `v@:@` both, measured, and asserted to agree with
//!   each other — and does not declare `insertBackTab:`. AppKit reaches all
//!   three through the same `doCommandBySelector:` dispatch, so the two it
//!   declares ARE this row's authority; see [`Family`].
//! * `frameDidChange:` is the fork's own `NSNotificationCenter` selector and
//!   has no sibling anywhere. Its expected shape is generated by
//!   `aterm_objc::method_encoding!` from the Rust types the fork's `fn` writes,
//!   which is the one written-down shape in this file — ONE row, with the Rust
//!   signature it mirrors printed beside it. Part D also drives it through
//!   `NSMethodSignature`, which for a row no protocol declares really does read
//!   our string.
//!
//! # What it does NOT check, on purpose
//!
//! CARDINALITY. A method silently deleted from the fork leaves a smaller table
//! that is still entirely correct, and this file would pass it. That axis
//! belongs to `crates/aterm-objc/tests/winit_seam.rs`, whose staleness guard
//! counts the `@sel(`/`#[method(` sites in the fork's own source against its
//! own table and goes RED when they diverge. Two guards, one job each, rather
//! than two half-answers to both.
//!
//! BEHAVIOUR beyond the handful of live sends in part C. What an IME does to
//! this view over a whole composition — marked text, cursor ranges, commit,
//! `doCommandBySelector:` — is driven by
//! `crates/aterm-gui/examples/objc_ime_drive.rs`, which is a separate gate
//! because it is a separate question: this file asks whether the class is
//! SHAPED right, that one asks whether it BEHAVES right.
//!
//! # Why an example and not a `#[test]`
//!
//! `libtest` cannot host AppKit: it runs every `#[test]` body on a spawned
//! thread, `pthread_main_np()` answers 0 there, and `EventLoop` construction
//! panics — measured, and `--test-threads=1` does not change it. Only a target
//! owning `fn main` can create the `NSWindow` whose delegate this audits. That
//! is the same reason `src/bin/aterm-redraw-conformance.rs` is a binary, and
//! the exit-code discipline here is deliberately identical to that gate's.
//!
//! # Exit codes — the ladder gates on these, not on the prose
//!
//! * `0` — the live class was found and every row agreed with its authority.
//! * `1` — a finding: a disagreeing encoding, an unauthoritative row, a
//!   protocol the class implements but does not claim, or a live send that
//!   answered with the wrong shape.
//! * `2` — NOT RUN: no event loop is constructible here (headless, no window
//!   server), or no delegate was installed. Never a pass.

/// Every row agreed with the runtime's own authority.
///
/// The `allow` is scoped to the builds where the constant is genuinely
/// unreachable — off macOS the only outcome is `NOT_RUN` — rather than to the
/// whole file, so the three codes can still be declared together where the
/// contract is stated.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const PASS: i32 = 0;
/// At least one finding. See the transcript.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const FAIL: i32 = 1;
/// The audit could not execute here. NOT a pass; see the module docs.
const NOT_RUN: i32 = 2;

#[cfg(not(target_os = "macos"))]
fn main() -> std::process::ExitCode {
    eprintln!(
        "objc-live-class-audit: NOT RUN — this audit is about \
         vendor/winit/src/platform_impl/macos/window_delegate.rs, which does not \
         exist off macOS."
    );
    std::process::ExitCode::from(NOT_RUN as u8)
}

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(macos::run() as u8)
}

#[cfg(target_os = "macos")]
mod macos {
    use std::collections::{BTreeMap, BTreeSet};
    use std::ffi::CStr;
    use std::time::{Duration, Instant};

    use aterm_objc::{
        Bool, Id, Sel, class, class_methods, class_name, class_of, class_protocols, method_types,
        msg, protocol, protocol_method_types, sel, strip_method_offsets, superclass_of,
    };
    use winit::application::ApplicationHandler;
    use winit::event::WindowEvent;
    use winit::event_loop::{ActiveEventLoop, EventLoop};
    use winit::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::window::{Window, WindowId};

    use super::{FAIL, NOT_RUN, PASS};

    /// One class this audit is responsible for.
    ///
    /// The fork declares two, and W3 shipped the second (`view.rs`) into a gate
    /// that only read the first — which is the exact defect D1 exists to
    /// prevent, one file later. A `Target` is what stops that being a
    /// copy-paste of the whole audit per class.
    struct Target {
        /// The class name `declare_class!` registers: the ONE thing this file
        /// has to know to find anything at all. Everything else is read out of
        /// the runtime.
        class_name: &'static str,
        /// The `protocols:` list of the fork's `declare_class!` site — the
        /// DECLARED backstop described in the module docs. Names, not
        /// signatures.
        ///
        /// It is also SOURCE 1 of the derived authority list
        /// ([`derived_authority_protocols`]), and that is what keeps plant two
        /// fatal: the derivation reads the SUPERCLASS chain's claims, never the
        /// audited class's own, so deleting a name from the fork's `protocols:`
        /// line does not delete the authority its rows are checked against.
        claimed: &'static [&'static CStr],
        /// Consulted after every protocol: the superclass chain, where the
        /// OVERRIDES live. `-dealloc` is here, and so is every `NSResponder`
        /// row `view.rs` overrides.
        authority_classes: &'static [&'static CStr],
        /// Selectors nothing in the runtime declares, WITH the shape each is
        /// held to anyway. This is not an exemption list: a row here is
        /// checked by part D, against an encoding generated from the fork's
        /// own Rust signature, and driven through `NSMethodSignature`.
        unchecked: &'static [Unchecked],
        /// Rows whose authority is DERIVED from siblings the runtime does
        /// declare — see [`Family`]. Consulted after the protocols and the
        /// superclass chain and before a row is called unauthoritative.
        families: &'static [Family],
        /// Authority protocols that are INFORMAL ON THIS TARGET'S CHAIN: they
        /// declare signatures the encoding check uses, nothing the target
        /// inherits from conforms to them, and AppKit dispatches their rows to
        /// this object by `-respondsToSelector:` instead. Exempt from the
        /// derived-conformance tooth, with the reason written down.
        ///
        /// "ON THIS TARGET'S CHAIN" is load-bearing and was missing. The
        /// sentence used to say "nothing conforms to them", which is a claim
        /// about the runtime and is false for the one entry that exists:
        /// `NSStandardKeyBindingResponding` is claimed by `NSTextView` and
        /// conformed to by its 8-class subtree, of the 26,318 loaded with
        /// AppKit. What makes the exemption sound is the narrower fact — that
        /// `WinitView`, `NSView`, `NSResponder` and `NSObject` all answer NO —
        /// and that is what [`Audit::protocols`] re-asks of the INSTANCE on
        /// every run.
        ///
        /// These names are also SOURCE 2 of the derived authority list: nothing
        /// on the chain claims them, so no derivation can find them, and a
        /// deletion here silently un-checks the rows they speak for.
        ///
        /// This list must stay SHORT and per-target: every name on it is a
        /// protocol whose disappearance from a `protocols:` list this gate
        /// would no longer notice.
        informal: &'static [(&'static str, &'static str)],
        /// Which of the class's registered rows are the FORK's — see [`Rows`].
        rows: Rows,
    }

    /// Whose method table a [`Target`] is reading.
    ///
    /// Four of the five targets are classes the fork DECLARES, and for those
    /// the answer is "all of it": every row in `class_copyMethodList` was put
    /// there by `declare_class!` and every row is this gate's business.
    ///
    /// `NSApplication` is not like that, and it is the fifth target because
    /// `vendor/winit/src/platform_impl/macos/app.rs` REACHES INTO IT.
    /// `override_send_event` swizzles `-[NSApplication sendEvent:]` with
    /// `method_setImplementation` — no subclass, no `declare_class!`, no new
    /// class name — so the fork owns exactly one row of a table with hundreds
    /// of Apple's in it. Auditing that table entire would be auditing AppKit,
    /// and every row would resolve against `NSApplication` itself, which is a
    /// tautology, not a check.
    #[derive(Clone, Copy)]
    enum Rows {
        /// A class the fork declares: every registered row is audited.
        Declared,
        /// A framework class the fork PATCHED: only these selectors are
        /// audited, each of them must be present, and each is additionally
        /// checked to be running the fork's own code — see
        /// [`Audit::patched_rows`].
        Patched(&'static [&'static str]),
    }

    /// `window_delegate.rs`'s class. 23 declared rows plus `-dealloc`; every one
    /// resolves, so the exemption list is empty and MEASURED empty.
    const DELEGATE: Target = Target {
        class_name: "WinitWindowDelegate",
        claimed: &[c"NSObject", c"NSWindowDelegate", c"NSDraggingDestination"],
        // `NSObject` the CLASS is where `-dealloc` and KVO's
        // `observeValueForKeyPath:ofObject:change:context:` live.
        authority_classes: &[c"NSObject"],
        unchecked: &[],
        families: &[],
        // EMPTY, and that is what keeps plant B fatal: `NSWindowDelegate` is a
        // formal protocol, `NSObject` does not conform to it, so a class that
        // implements its rows and stops claiming it is caught here.
        informal: &[],
        rows: Rows::Declared,
    };

    /// `view.rs`'s class. 43 declared rows plus `-dealloc`.
    ///
    /// The claimed list is ONE name and that is measured, not assumed: objc2's
    /// `unsafe impl NSTextInputClient for WinitView` called `class_addProtocol`
    /// exactly once, and the pre-port class claimed `["NSTextInputClient"]` and
    /// nothing else. There is no `NSObjectProtocol` here, unlike the delegate.
    const VIEW: Target = Target {
        class_name: "WinitView",
        // ONE name, and the derivation supplies the rest.
        //
        // `NSStandardKeyBindingResponding` — where `insertTab:` and
        // `cancelOperation:` are declared — arrives through `informal` below,
        // because nothing on this chain claims it and nothing would put it in
        // the list otherwise. `NSTouchBarProvider` arrives from the CHAIN, twice
        // over: `NSResponder` claims it directly and `NSWindow` claims
        // `NSTouchBarProviderContainer`, which inherits it. It declares
        // `touchBar` `@16@0:8` as a required instance method and `NSResponder`
        // IMPLEMENTS it with the identical `@16@0:8` (measured; `NSView` and
        // `NSObject` do not) — so protocol and chain agree to the byte, and the
        // row is reported as `proto …` because protocols are consulted first
        // everywhere in this file. `winit_seam.rs:249` reports the CLASS for the
        // same row because it asks the chain; both are right, and the
        // disagreement was only ever in the prose.
        claimed: &[c"NSTextInputClient"],
        // The whole chain, in override order:
        // `WinitView -> NSView -> NSResponder -> NSObject`.
        authority_classes: &[c"NSView", c"NSResponder", c"NSObject"],
        unchecked: VIEW_UNCHECKED,
        families: VIEW_FAMILIES,
        informal: VIEW_INFORMAL,
        rows: Rows::Declared,
    };

    /// `app_state.rs`'s class: the `NSApplicationDelegate` on `NSApp`.
    ///
    /// Three declared rows plus `-dealloc`, and ALL THREE ARE `@optional` —
    /// which is why reading the registered table matters more here than
    /// anywhere else in this file. An `@optional` protocol row is reached only
    /// after `-respondsToSelector:` says yes, so a row that never registered
    /// does not crash and does not raise: the application simply never finishes
    /// launching, never applies its activation policy and never terminates
    /// cleanly. There is no first message to fail at, and no test that sends
    /// one would notice either.
    ///
    /// The `claimed` list is TWO names, transcribed from the fork's
    /// `protocols:` list, which is itself what objc2's
    /// `unsafe impl NSObjectProtocol` + `unsafe impl NSApplicationDelegate`
    /// called `class_addProtocol` for.
    const APP_DELEGATE: Target = Target {
        class_name: "WinitApplicationDelegate",
        claimed: &[c"NSObject", c"NSApplicationDelegate"],
        // `NSObject` the CLASS is where `-dealloc` lives.
        authority_classes: &[c"NSObject"],
        unchecked: &[],
        families: &[],
        // EMPTY, and formal: `NSApplicationDelegate` is a real protocol that
        // `NSObject` does not conform to, so a class that implements its rows
        // and stops claiming it is caught by the derived tooth.
        informal: &[],
        rows: Rows::Declared,
    };

    /// `window.rs`'s class: the `NSWindow` every winit window is.
    ///
    /// Two declared rows plus `-dealloc`, no ivars and NO PROTOCOLS — objc2's
    /// `declare_class!` called `class_addProtocol` zero times here, so `claimed`
    /// is empty and `class_copyProtocolList` is expected to be empty too. Both
    /// of its rows take their authority from `NSWindow` the CLASS, which is the
    /// only target in this file whose whole table does.
    ///
    /// AN EMPTY `claimed` USED TO MEAN AN EMPTY AUTHORITY LIST, and that is the
    /// finding this wave closed. The constant here read `&[c"NSObject"]` under a
    /// comment saying "no protocol declares either window row (measured against
    /// all 17 protocols `NSWindow` claims)" — true of THOSE TWO ROWS and read by
    /// the next reader as a fact about the class. It is not: `NSWindow` claims
    /// seventeen protocols in its own list, and among them
    /// `NSGestureRecognizerDelegate` declares `gestureRecognizerShouldBegin:`
    /// `B24@0:8@16`. A row the fork might add there was reported as one nothing
    /// in the runtime declares. The derivation supplies the whole chain instead
    /// — 36 names in the auditor's own process, and the count is a property of
    /// which frameworks are loaded, so the transcript prints it rather than this
    /// comment asserting it — and the two rows still resolve against `NSWindow`
    /// the class, because none of those names declares either of them.
    const WINDOW: Target = Target {
        class_name: "WinitWindow",
        claimed: &[],
        // The whole chain, in override order:
        // `WinitWindow -> NSWindow -> NSResponder -> NSObject`.
        authority_classes: &[c"NSWindow", c"NSResponder", c"NSObject"],
        unchecked: &[],
        families: &[],
        informal: &[],
        rows: Rows::Declared,
    };

    /// `app.rs`'s ONE row, and the only target here that is not a class this
    /// fork declares.
    ///
    /// # Why `NSApplication` is audited at all, and what the exception is
    ///
    /// This file's rule is "the gate must read the REGISTERED CLASS", and
    /// `app.rs` has a `declare_class!` in it — `TestApplication`, an
    /// `NSApplication` subclass with one `sendEvent:` row whose body is
    /// `todo!()`. It is NOT PRODUCT CODE: it sits inside `#[cfg(test)] mod
    /// tests`, in `fn test_custom_class()`, and it is instantiated with
    /// `msg_send_id![TestApplication::class(), new]` by that test and by
    /// nothing else. NO LIVE-CLASS AUDIT CAN EVER READ IT, because no instance
    /// of it exists in a shipping process — the class is not even registered
    /// there, since `declare_class!`'s registration is lazy on first
    /// `::class()`. That is the exception to the rule, and it is named HERE,
    /// beside the rule, rather than left for a future wave to rediscover as a
    /// gap.
    ///
    /// A related claim was checked and is FALSE for this tree, so it is written
    /// down rather than repeated: an `NSApplication` subclass installed through
    /// `NSPrincipalClass` would fail SILENTLY AT LAUNCH rather than at first
    /// message, which would be a strong argument for auditing it. `grep -rn
    /// NSPrincipalClass` over the whole repository — sources, plists, manifests
    /// — finds NOTHING. This fork installs no principal class; upstream winit
    /// deliberately stopped subclassing `NSApplication` for exactly that reason
    /// ("we would like to give the user full control over their
    /// NSApplication"), and swizzles instead.
    ///
    /// # What IS product code in `app.rs`, and why nothing read it
    ///
    /// `override_send_event`, which replaces `-[NSApplication sendEvent:]`'s
    /// IMP with the fork's own through `method_setImplementation`. It is on
    /// every keystroke path in the process (it is what makes Cmd+key deliver a
    /// `keyUp:` at all, and it is where every `DeviceEvent` comes from), and it
    /// leaves the type encoding UNTOUCHED by construction — so not one encoding
    /// check in this tree can tell whether it ran. The only live evidence is
    /// the ADDRESS of the function the runtime will call, which is what
    /// [`Audit::patched_rows`] reads.
    const APP: Target = Target {
        class_name: "NSApplication",
        claimed: &[],
        authority_classes: &[c"NSApplication"],
        unchecked: &[],
        families: &[],
        informal: &[],
        rows: Rows::Patched(&["sendEvent:"]),
    };

    /// ONE protocol, and it is a MEASUREMENT that refuted the rule around it.
    ///
    /// The derived tooth was written as "if a protocol supplied a row's
    /// authority, the object must conform to it". Asked of `WinitView`, the
    /// runtime answers that `-conformsToProtocol:NSStandardKeyBindingResponding`
    /// is FALSE — not for the subclass, not for `NSView`, not for `NSResponder`.
    /// `NSStandardKeyBindingResponding` is an INFORMAL protocol in practice:
    /// AppKit declares it so the signatures exist to check against, dispatches
    /// `insertTab:`/`cancelOperation:` by `-respondsToSelector:`, and does not
    /// declare conformance on the responder chain. objc2's class had exactly
    /// the same property — the RULE was wrong, not the port.
    ///
    /// THE SENTENCE WAS ALSO WRONG, and its correction is why the entry now
    /// says what it MEASURES. It used to read "nothing in the runtime conforms
    /// to it", which is false: over all 26,318 classes loaded with AppKit,
    /// exactly ONE names it in its own protocol list — `NSTextView`, the class
    /// that implements the key-binding surface for real. The armed half of the
    /// check was always the right one and is unchanged: it asks THIS INSTANCE,
    /// and the answer is NO for `WinitView` and for everything on its chain.
    /// The exemption is therefore about a chain, not about a runtime, and it is
    /// stated at that width — which also makes the staleness tooth below sharp:
    /// if `NSView` or `NSResponder` ever conforms, this entry goes RED.
    ///
    /// `NSTouchBarProvider` is deliberately NOT here: it is formal, `NSResponder`
    /// CLAIMS it in its own protocol list, and `NSView` therefore conforms by
    /// inheritance — so `touchBar`'s authority is checked with the tooth fully
    /// armed.
    ///
    /// THE PARENTHESIS THAT USED TO CLOSE THAT SENTENCE READ "Five classes claim
    /// it: NSResponder, NSWindow, NSApplication, NSAlert,
    /// NSApplicationFunctionRowContainer", and it is a wrong answer to a
    /// question it did not name. "Claim" is the own-list instrument — the one
    /// the paragraph thirteen lines above uses correctly for
    /// `NSStandardKeyBindingResponding` — and there are THREE instruments here,
    /// which answer 2, 5 and 757 over the same 26,318 classes:
    ///
    /// * `class_copyProtocolList`, the class's OWN list: **2** — `NSAlert` and
    ///   `NSResponder`. That is what "claim" means, and it is the number that
    ///   belongs in a sentence beginning "NSResponder CLAIMS it".
    /// * `class_conformsToProtocol`, which adds protocol INHERITANCE but does
    ///   NOT walk superclasses: **5** — the two above plus `NSWindow`,
    ///   `NSApplication` and `NSApplicationFunctionRowContainer`, each of which
    ///   claims `NSTouchBarProviderContainer`, which inherits
    ///   `NSTouchBarProvider`. This is the list the old parenthesis printed,
    ///   under the wrong verb; note that `NSView` is NOT in it.
    /// * `-conformsToProtocol:`, which is that walked up the superclass chain
    ///   and is the question AppKit and part B actually ask: **757**, `NSView`
    ///   and `WinitView` among them.
    ///
    /// The verdict was never in doubt — all three answers are non-zero, and the
    /// tooth is armed either way — but the three numbers are what make
    /// "`NSResponder` claims it, `NSView` conforms by inheritance" a MEASUREMENT
    /// rather than a story: `class_conformsToProtocol(NSView, …)` is FALSE and
    /// `-conformsToProtocol:` is TRUE, on the same pair, in the same run.
    const VIEW_INFORMAL: &[(&str, &str)] = &[(
        "NSStandardKeyBindingResponding",
        "informal ON THIS CHAIN: neither WinitView nor NSView nor NSResponder \
         nor NSObject conforms (measured, and re-asked of the instance every \
         run), because AppKit dispatches its rows by -respondsToSelector: here \
         and declares the protocol only so the signatures exist. Exactly one \
         loaded class of 26,318 CLAIMS it — NSTextView — and exactly 8 CONFORM \
         to it, that class and its subtree, so this is an exemption about a \
         superclass chain and not a statement about the runtime",
    )];

    /// A row nothing in the runtime declares, and the shape it is held to
    /// anyway.
    ///
    /// The previous form of this was a `(selector, reason)` pair on a list
    /// called `unauthoritative`, and the reason was the whole of it: the row
    /// was named, excused, and then checked by nothing at all. A plant that
    /// retyped such a row's argument passed the auditor, the build, the census
    /// and the IME driver. An exemption that nothing else covers is a hole with
    /// a comment in it.
    struct Unchecked {
        /// The selector, as the fork's `@sel(…)` spells it.
        sel: &'static str,
        /// Why NO protocol and NO class on the chain declares it. This is a
        /// statement about the runtime and it is measured — by
        /// `winit_seam.rs`'s `the_key_binding_family_is_measured_not_asserted`
        /// for the key-binding family, and by part A here, which reaches this
        /// list only after asking every authority the target has.
        why: &'static str,
        /// The fork's Rust signature for this row, as source text, so the
        /// expectation below can be read against the thing it claims to
        /// describe.
        rust: &'static str,
        /// The encoding [`Self::rust`] produces, built by
        /// `aterm_objc::method_encoding!` over those same types rather than
        /// typed out as a string. A `fn` pointer and not a `&str` because the
        /// macro expands to a `String`: `BOOL` is `B` on arm64 and `c` on the
        /// x86_64 compat slice, and a literal would be wrong on one of them.
        expected: fn() -> String,
    }

    /// A row nothing declares whose shape is DERIVED from siblings that
    /// something does.
    ///
    /// `insertBackTab:` is the case this exists for and the only one today.
    /// AppKit's key-binding dispatch turns a key equivalent into a selector and
    /// sends it to the first responder; `insertTab:`, `insertBackTab:` and
    /// `cancelOperation:` are three actions of that one family, reached the
    /// same way with the same argument. `NSStandardKeyBindingResponding`
    /// declares two of them and not the third — an asymmetry in AppKit's own
    /// headers, measured (`NSResponder` implements NONE of the three, so there
    /// is no override to inherit a signature from either).
    ///
    /// The two it declares are therefore this row's authority, and nothing here
    /// is written down but the family membership: the ENCODING comes out of the
    /// runtime, and the siblings are required to agree with each other before
    /// their answer is used for anything.
    struct Family {
        /// The row with no declaration of its own.
        sel: &'static str,
        /// Where the siblings are declared.
        protocol: &'static CStr,
        /// The siblings, which must all agree. TWO or more, or the "they agree"
        /// half is vacuous.
        siblings: &'static [&'static str],
        /// Why these siblings are the right authority for that row.
        why: &'static str,
    }

    /// ONE family: the key-binding actions.
    const VIEW_FAMILIES: &[Family] = &[Family {
        sel: "insertBackTab:",
        protocol: c"NSStandardKeyBindingResponding",
        siblings: &["insertTab:", "cancelOperation:"],
        // THE REASON IS A CORRECTION. This used to say "no loaded class
        // implements any of them", which is FALSE and was never measured over
        // more than two classes. Over all 26,318 classes loaded with AppKit:
        // `insertTab:` is implemented by THREE (NSCollectionView,
        // NSColorPickerPencilView, NSTextView), `cancelOperation:` by SEVEN
        // (NSWindow, NSTableView, NSPopover, _NSPopoverWindow,
        // _NSDatePickerOverlayPanel, NSTitlebarRenamingSession,
        // NSVisualTabPickerRootView), and only `insertBackTab:` by NONE.
        //
        // THE DERIVATION SURVIVES THE CORRECTION, and that is why the verdict
        // is unchanged: what it needs is (1) that no class on THIS view's chain
        // — NSView, NSResponder, NSObject — implements any of the three, so
        // there is no override whose signature could be inherited instead, and
        // (2) that every implementation that does exist agrees on the shape.
        // Both measured: zero of the three on the chain, and all ten
        // implementations register `v24@0:8@16` = `v@:@`, the same string the
        // protocol declares for the two it declares.
        why: "AppKit turns a key equivalent into one of these three selectors \
              and sends it to the first responder the same way for all three; \
              NSStandardKeyBindingResponding declares the other two and not \
              this one. NOTHING ON THIS VIEW'S CHAIN implements any of the \
              three (NSView, NSResponder, NSObject — measured), so there is no \
              override to inherit a signature from; elsewhere in the runtime \
              insertTab: has 3 implementations and cancelOperation: 7, every \
              one of them v@:@, while insertBackTab: has 0 (measured over the \
              26,318 classes loaded with AppKit — see \
              winit_seam.rs::the_key_binding_family_is_measured_not_asserted)",
    }];

    /// ONE row, and it is the last one in the surface that no protocol, no
    /// class and no sibling can speak for.
    ///
    /// The count is a CORRECTION twice over. The comment this replaces
    /// predicted THREE (`frameDidChange:`, `insertBackTab:`, `cancelOperation:`)
    /// before the class existed to ask; asking gave TWO, because
    /// `cancelOperation:` IS declared by `NSStandardKeyBindingResponding`. The
    /// second of those two — `insertBackTab:` — left the list when [`Family`]
    /// gave it a derived authority, and the reason it used to carry here
    /// ("NSResponder implements it but declares it nowhere") was false in both
    /// halves: `NSResponder` implements neither it nor its siblings.
    const VIEW_UNCHECKED: &[Unchecked] = &[Unchecked {
        sel: "frameDidChange:",
        why: "not an AppKit method at all — it is this fork's own \
              NSNotificationCenter selector, registered by `WinitView::new` \
              with -addObserver:selector:name:object: and named by nothing in \
              the runtime. A notification callback's shape is a convention \
              (void return, one NSNotification argument) that no header states \
              in a form the runtime can be asked for",
        rust: "fn frame_did_change(&self, _note: Id)",
        expected: || aterm_objc::method_encoding!(() ; Id),
    }];

    /// Foundation's `NSNotFound`, which is `NSIntegerMax` — the value
    /// `NSTextInputClient` documents `markedRange`/`selectedRange` to answer
    /// when there is none. Written as the arithmetic rather than as a literal
    /// so it cannot be mistyped.
    const NS_NOT_FOUND: usize = isize::MAX as usize;

    /// Long enough for macOS to launch its `NSApplication` and hand back a
    /// window; past it the audit reports NOT RUN rather than passing.
    const BUDGET: Duration = Duration::from_secs(30);

    /// Where one row's encoding came from.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Source {
        /// A protocol declared it. The name is the protocol's.
        Protocol(&'static CStr),
        /// A class on the superclass chain implements it and the fork overrides
        /// it. `-dealloc` is always this, and every `NSResponder` row `view.rs`
        /// declares is too.
        ///
        /// THE NAME IS THE CLASS THAT WAS ASKED, NOT THE CLASS THAT DEFINES.
        /// [`aterm_objc::method_types`] bottoms out in `class_getInstanceMethod`,
        /// which WALKS the superclass chain, so asking `NSView` for `keyDown:`
        /// answers `NSResponder`'s method and this variant prints
        /// "class NSView". The label used to say only "class NSView" and a
        /// reader had no way to tell the two apart — which is the exact reading
        /// error that produced two earlier findings about this file. It is
        /// COSMETIC and not a defect in the verdict, and that is measured: no
        /// encoding differs between chain levels for any of the 29 rows the
        /// view overrides (`NSView`/`NSResponder`/`NSObject` all agree wherever
        /// more than one defines a row), so the answer is the same whichever
        /// level supplied it. [`Source::label`] now prints both when they
        /// differ.
        Class(&'static CStr),
        /// Nothing declares this row, but a protocol declares its SIBLINGS and
        /// they agree with each other — see [`Family`]. The name is the
        /// protocol the siblings came from.
        Siblings(&'static CStr),
        /// Nothing in the runtime declares it.
        Nowhere,
    }

    impl Source {
        /// `sel` is the row this source was found for: [`Self::Class`] prints
        /// the class it ASKED and, when they differ, the class that actually
        /// DEFINES the method.
        fn label(self, sel: Sel) -> String {
            match self {
                Self::Protocol(p) => format!("proto {}", p.to_string_lossy()),
                Self::Class(c) => {
                    let asked = c.to_string_lossy();
                    match defining_class(c, sel) {
                        Some(def) if def != asked => format!("class {asked} -> {def} defines it"),
                        _ => format!("class {asked}"),
                    }
                }
                Self::Siblings(p) => format!("siblings in {}", p.to_string_lossy()),
                Self::Nowhere => "NO AUTHORITY".to_owned(),
            }
        }
    }

    /// The first class at or above `cls` whose OWN method table holds `sel`.
    ///
    /// `class_getInstanceMethod` — which [`aterm_objc::method_types`] and
    /// [`aterm_objc::method_imp`] both use — walks the chain and reports the
    /// inherited method without saying where it came from. This walks the same
    /// chain one level at a time against `class_methods`, which reports only
    /// what a class declares ITSELF, and so can say.
    fn defining_class(cls: &'static CStr, sel: Sel) -> Option<String> {
        // SAFETY: `class` answers a live class object or nil, and
        // `superclass_of`/`class_methods`/`class_name` all tolerate and
        // terminate at nil.
        unsafe {
            let mut walk = class(cls);
            while !walk.is_null() {
                if class_methods(walk).iter().any(|(s, _)| *s == sel) {
                    return Some(class_name(walk).to_string_lossy().into_owned());
                }
                walk = superclass_of(walk);
            }
        }
        None
    }

    /// Intern a selector whose name is only known as a `&str`.
    ///
    /// `sel_uncached` takes a `&'static CStr` because every ordinary call site
    /// writes a `c"…"` literal; the handful of names that arrive from a table
    /// leak one small allocation each, for the life of a process that audits
    /// one window and exits. That is the cheapest honest bridge.
    fn sel_named(name: &str) -> Sel {
        let owned: &'static CStr = Box::leak(
            std::ffi::CString::new(name)
                .expect("a selector name has no interior NUL")
                .into_boxed_c_str(),
        );
        aterm_objc::sel_uncached(owned)
    }

    /// Intern a CLASS or PROTOCOL name that is only known as a `&str`.
    ///
    /// [`aterm_objc::protocol`] and [`aterm_objc::class`] take `&'static CStr`
    /// for the same reason [`aterm_objc::sel_uncached`] does — every ordinary
    /// call site writes a `c"…"` literal — and the derived authority list is the
    /// case that is not ordinary: its names come out of the runtime. Same bridge
    /// as [`sel_named`], same cost, and both are bounded by the handful of
    /// protocols on five superclass chains in a process that audits one window
    /// and exits.
    fn cstr_named(name: &str) -> &'static CStr {
        Box::leak(
            std::ffi::CString::new(name)
                .expect("a runtime-supplied name has no interior NUL")
                .into_boxed_c_str(),
        )
    }

    /// Every protocol reachable from `seed`, following protocol INHERITANCE, in
    /// seed order and without duplicates.
    ///
    /// A protocol list is a DAG, not a set of leaves: `NSWindow` claims
    /// `NSTouchBarProviderContainer`, which inherits `NSTouchBarProvider`, which
    /// is where `touchBar` is actually declared. Reading only the claimed names
    /// would miss every declaration one level up, which is the same class of
    /// blindness as the hand-written lists this replaces.
    fn expand_protocols(into: &mut Vec<String>, seed: impl IntoIterator<Item = String>) {
        for name in seed {
            // Depth-first from ONE seed at a time, so the caller's order is the
            // order the authority is looked for in.
            let mut stack = vec![name];
            while let Some(next) = stack.pop() {
                if into.contains(&next) {
                    continue;
                }
                // SAFETY: `protocol` answers a live protocol object or nil, and
                // `protocol_parents` tolerates nil.
                stack.extend(protocol_parents(protocol(cstr_named(&next))));
                into.push(next);
            }
        }
    }

    /// The protocols a [`Target`]'s rows are checked against — read off the
    /// runtime rather than typed out.
    ///
    /// Three sources, in this order: the fork's own `protocols:` line
    /// (`claimed`, written down), the names on the `informal` list (which
    /// NOTHING claims, so no derivation can find them), and every protocol the
    /// SUPERCLASS CHAIN claims. Each expanded through protocol inheritance.
    ///
    /// STARTING AT THE SUPERCLASS IS THE WHOLE DESIGN. Reading `cls`'s own
    /// protocol list would make plant two — a class that silently stops claiming
    /// `NSWindowDelegate` — delete the authority its own 18 rows are checked
    /// against, and this file's two findings would collapse into one. The
    /// audited class's claims enter only through `claimed`, which is a
    /// transcription and cannot move when the fork does.
    fn derived_authority_protocols(
        target: &Target,
        cls: aterm_objc::ClassPtr,
    ) -> Vec<&'static CStr> {
        let mut names: Vec<String> = Vec::new();
        expand_protocols(
            &mut names,
            target
                .claimed
                .iter()
                .map(|c| c.to_string_lossy().into_owned()),
        );
        expand_protocols(
            &mut names,
            target.informal.iter().map(|(n, _)| (*n).to_owned()),
        );
        // SAFETY: `cls` is a live class object; `superclass_of` tolerates and
        // terminates at nil, and `class_protocols` tolerates nil.
        let mut walk = unsafe { superclass_of(cls) };
        while !walk.is_null() {
            // SAFETY: `walk` is a live class while non-null.
            expand_protocols(&mut names, unsafe { class_protocols(walk) });
            // SAFETY: as above.
            walk = unsafe { superclass_of(walk) };
        }
        names.iter().map(|n| cstr_named(n)).collect()
    }

    /// EVERY protocol loaded in this process, with the encoding each gives
    /// `sel` — the measurement that turns "nothing declares it" from an
    /// assumption into a fact.
    ///
    /// Only ever called on the failure path, once per unauthoritative row, over
    /// every protocol the process has loaded — 10,475 in this binary, and the
    /// finding prints the number it searched rather than trusting this comment.
    /// The cost is irrelevant and the answer is the difference between a finding
    /// that names the one-line fix and a finding that sends its reader to write
    /// down a signature by hand.
    fn protocols_declaring(sel: Sel) -> Vec<(String, String)> {
        let mut found = Vec::new();
        for (name, proto) in loaded_protocols() {
            // SAFETY: `proto` came from `objc_copyProtocolList` and protocol
            // objects are immortal; `sel` is interned.
            if let Some(types) = unsafe { protocol_method_types(proto, sel, true) } {
                found.push((name, strip_method_offsets(&types)));
            }
        }
        found
    }

    /// How many protocols are loaded, so the finding can state the width of the
    /// search it just did.
    fn loaded_protocol_count() -> usize {
        loaded_protocols().len()
    }

    /// The runtime's whole protocol table.
    ///
    /// Same argument as [`protocol_parents`] for declaring the prototype here:
    /// `aterm-objc` grows for the PORT, and no shipping call site enumerates
    /// protocols.
    fn loaded_protocols() -> Vec<(String, aterm_objc::ProtocolPtr)> {
        unsafe extern "C" {
            fn objc_copyProtocolList(count: *mut std::ffi::c_uint) -> *mut aterm_objc::ProtocolPtr;
            fn protocol_getName(proto: aterm_objc::ProtocolPtr) -> *const std::ffi::c_char;
            fn free(ptr: *mut std::ffi::c_void);
        }
        let mut count: std::ffi::c_uint = 0;
        // SAFETY: the runtime writes the count through the pointer and hands
        // back a malloc'd array this function owns, or null with a zero count.
        let list = unsafe { objc_copyProtocolList(&raw mut count) };
        if list.is_null() {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count as usize {
            // SAFETY: `i < count`; protocol objects and their names are
            // immortal.
            let proto = unsafe { *list.add(i) };
            // SAFETY: as above.
            let name = unsafe { CStr::from_ptr(protocol_getName(proto)) };
            out.push((name.to_string_lossy().into_owned(), proto));
        }
        // SAFETY: the array is this function's to free; the protocol objects it
        // held are immortal and are not touched.
        unsafe { free(list.cast()) };
        out
    }

    /// The protocols a protocol itself inherits.
    ///
    /// `protocol_copyProtocolList` has no wrapper in `aterm-objc` because no
    /// SHIPPING call site needs one — this gate is the only reader — and the
    /// crate's rule is that it grows for the port, not for the auditor. Declared
    /// here for the same reason `dladdr` is: proving something about the running
    /// process must not add a dependency to it.
    fn protocol_parents(proto: aterm_objc::ProtocolPtr) -> Vec<String> {
        unsafe extern "C" {
            fn protocol_copyProtocolList(
                proto: aterm_objc::ProtocolPtr,
                count: *mut std::ffi::c_uint,
            ) -> *mut aterm_objc::ProtocolPtr;
            fn protocol_getName(proto: aterm_objc::ProtocolPtr) -> *const std::ffi::c_char;
            fn free(ptr: *mut std::ffi::c_void);
        }
        if proto.is_null() {
            return Vec::new();
        }
        let mut count: std::ffi::c_uint = 0;
        // SAFETY: `proto` is a live protocol object; the runtime writes the
        // count through the pointer and hands back a malloc'd array this
        // function owns, or null with a zero count.
        let list = unsafe { protocol_copyProtocolList(proto, &raw mut count) };
        if list.is_null() {
            return Vec::new();
        }
        let mut names = Vec::with_capacity(count as usize);
        for i in 0..count as usize {
            // SAFETY: `i < count`; protocol objects and their names are
            // immortal.
            let name = unsafe { CStr::from_ptr(protocol_getName(*list.add(i))) };
            names.push(name.to_string_lossy().into_owned());
        }
        // SAFETY: the array is this function's to free; the protocol objects it
        // held are immortal and are not touched.
        unsafe { free(list.cast()) };
        names
    }

    /// Which loaded image a code address belongs to, per the dynamic loader.
    ///
    /// `dladdr` is the only thing in the process that can answer "is this
    /// function ours or AppKit's?", and the answer is a fact about the running
    /// binary rather than about any table this file could hold. It lives in
    /// `libSystem`, which is linked into every macOS process, so the prototype
    /// is declared here rather than pulled from a crate — `aterm-objc`'s rule,
    /// applied one file over: this audit adds no dependency to prove anything.
    fn image_of(imp: *const std::ffi::c_void) -> Option<String> {
        /// `<dlfcn.h>`'s `Dl_info`, exactly: four pointer-sized fields, the
        /// second of which is the image path.
        #[repr(C)]
        struct DlInfo {
            dli_fname: *const std::ffi::c_char,
            dli_fbase: *mut std::ffi::c_void,
            dli_sname: *const std::ffi::c_char,
            dli_saddr: *mut std::ffi::c_void,
        }
        unsafe extern "C" {
            fn dladdr(addr: *const std::ffi::c_void, info: *mut DlInfo) -> std::ffi::c_int;
        }
        // SAFETY: `DlInfo` is four raw pointers, for which an all-zero bit
        // pattern is a valid (null) value.
        let mut info: DlInfo = unsafe { std::mem::zeroed() };
        // SAFETY: `imp` is a code address the Objective-C runtime handed back
        // and is never dereferenced here; `dladdr` reads the loader's own
        // tables and fills `info`, answering 0 when the address is in no loaded
        // image. `dli_fname` points into the loader's storage, which outlives
        // the copy made below.
        let ok = unsafe { dladdr(imp, &raw mut info) };
        if ok == 0 || info.dli_fname.is_null() {
            return None;
        }
        // SAFETY: non-null and NUL-terminated, owned by the loader.
        Some(
            unsafe { CStr::from_ptr(info.dli_fname) }
                .to_string_lossy()
                .into_owned(),
        )
    }

    /// One protocol's declaration of an INSTANCE method named `name`.
    ///
    /// `protocol_method_types`'s third argument selects instance versus class
    /// method, and it already consults the required table and then the optional
    /// one on its own — which matters here, because every `NSWindowDelegate`
    /// and `NSStandardKeyBindingResponding` row this file cares about is
    /// `@optional`.
    fn protocol_declares(p: &'static CStr, name: &str) -> Option<String> {
        // SAFETY: `protocol` answers a live protocol object or nil (nil for a
        // framework that is not loaded, which `protocol_method_types`
        // tolerates), and `sel_named` interns the selector.
        let types = unsafe { protocol_method_types(protocol(p), sel_named(name), true) };
        types.map(|t| strip_method_offsets(&t))
    }

    /// Split a method type encoding into its return type and its arguments.
    ///
    /// `"{CGRect={CGPoint=dd}{CGSize=dd}}@:{_NSRange=QQ}^{_NSRange=QQ}"` is
    /// four types, and only a scanner that balances braces can say so — which
    /// is why this exists rather than a `split` on a character. `None` for a
    /// string that does not parse, which is itself a finding at every call
    /// site: the runtime accepted it, so somebody wrote it.
    fn split_encoding(enc: &str) -> Option<(String, Vec<String>)> {
        let mut rest = enc;
        let ret = next_type(&mut rest)?;
        let mut args = Vec::new();
        while !rest.is_empty() {
            args.push(next_type(&mut rest)?);
        }
        Some((ret, args))
    }

    /// Consume one complete `@encode` type from the front of `rest`.
    ///
    /// Qualifiers (`r`, `n`, `N`, `o`, `O`, `R`, `V`) and the pointer prefix
    /// `^` bind to the type that follows them; `{}`, `[]` and `()` nest; `@?`
    /// is a block and two characters where `@` alone is one.
    fn next_type(rest: &mut &str) -> Option<String> {
        let s = *rest;
        let b = s.as_bytes();
        let mut i = 0;
        while i < b.len() && matches!(b[i], b'r' | b'n' | b'N' | b'o' | b'O' | b'R' | b'V' | b'^') {
            i += 1;
        }
        if i >= b.len() {
            return None;
        }
        match b[i] {
            open @ (b'{' | b'[' | b'(') => {
                let close = match open {
                    b'{' => b'}',
                    b'[' => b']',
                    _ => b')',
                };
                let mut depth = 0_usize;
                loop {
                    if i >= b.len() {
                        // An unbalanced opener: the string is not an encoding.
                        return None;
                    }
                    if b[i] == open {
                        depth += 1;
                    } else if b[i] == close {
                        depth -= 1;
                        if depth == 0 {
                            i += 1;
                            break;
                        }
                    }
                    i += 1;
                }
            }
            b'@' => {
                i += 1;
                if i < b.len() && b[i] == b'?' {
                    i += 1;
                }
            }
            _ => i += 1,
        }
        let (token, tail) = s.split_at(i);
        *rest = tail;
        Some(token.to_owned())
    }

    /// The rectangle both probe rows in part E return, in `d0`-`d3`.
    ///
    /// Four distinct non-zero doubles, so a frame laid out the wrong way cannot
    /// produce this by accident and a zeroed one cannot be mistaken for it.
    const PROBE_RECT: aterm_objc::CGRect = aterm_objc::CGRect {
        origin: aterm_objc::CGPoint { x: 704.0, y: 839.0 },
        size: aterm_objc::CGSize {
            width: 11.0,
            height: 22.0,
        },
    };

    /// The IMP behind both of part E's probe rows: one compiled function,
    /// returning a `CGRect` — a homogeneous float aggregate, which arm64
    /// returns in `d0`-`d3` and never through `x8`.
    extern "C" fn probe_rect(_this: Id, _cmd: Sel) -> aterm_objc::CGRect {
        PROBE_RECT
    }

    /// The lying encoding: a 32-byte struct of four `long long`s. Same SIZE as
    /// an `NSRect` and not an HFA, so the ABI returns it indirectly through
    /// `x8` while the IMP above is putting its answer in `d0`-`d3`. This is the
    /// shape of the plant that walked through this file's old teeth.
    const PROBE_LIE: &str = "{_ATermAuditLie=qqqq}@:";

    /// The audit's whole state: the transcript it prints and the findings it
    /// exits on.
    #[derive(Default)]
    struct Audit {
        findings: Vec<String>,
        /// Why the audit could not run at all, if it could not.
        blocked: Option<String>,
    }

    impl Audit {
        fn fail(&mut self, what: String) {
            println!("  FINDING: {what}");
            self.findings.push(what);
        }

        /// Find `sel`'s authority, asking the runtime rather than a table.
        ///
        /// Protocols first, then the superclass chain, then the family: a row
        /// that a protocol DECLARES is checked against the declaration, a row
        /// nothing declares falls through to what some superclass happens to
        /// IMPLEMENT, and only then — for the one row in [`Target::families`] —
        /// to what the runtime says about its siblings.
        ///
        /// A method rather than a free function because the sibling arm can
        /// have a finding of its own to report: siblings that stop agreeing,
        /// or a sibling that stops being declared, mean the derivation is no
        /// longer sound and the row has quietly become unchecked again — and
        /// because a DERIVED protocol list can hold two declarations of one row.
        ///
        /// EVERY protocol hit is collected rather than the first taken, and they
        /// are required to AGREE. The hand-written lists this replaces were
        /// short enough that the question never arose; the derived ones run 12
        /// to 39 names, and two rows really do have two declarations —
        /// `touchBar` (`NSTouchBarProvider` and `NSTouchBarProviderContainer`)
        /// and `doCommandBySelector:` (`NSTextInputClient` and
        /// `NSStandardKeyBindingResponding`). Both pairs agree to the byte
        /// today, measured; if a pair ever stops agreeing, taking whichever came
        /// first would be picking an answer, so it is a finding instead.
        fn authority_of(
            &mut self,
            target: &Target,
            authorities: &[&'static CStr],
            sel: Sel,
        ) -> (Source, Option<String>) {
            let mut hits: Vec<(&'static CStr, String)> = Vec::new();
            for name in authorities {
                // SAFETY: `protocol` returns a live protocol object or nil (nil
                // for a framework that is not loaded, which
                // `protocol_method_types` tolerates), and `sel` is interned.
                if let Some(types) = unsafe { protocol_method_types(protocol(name), sel, true) } {
                    hits.push((name, strip_method_offsets(&types)));
                }
            }
            if let Some((first, encoding)) = hits.first() {
                for (other, other_encoding) in &hits[1..] {
                    if other_encoding != encoding {
                        self.fail(format!(
                            "{sel_name}: {a} declares it {encoding} and {b} \
                             declares it {other_encoding} — two authorities \
                             this class answers to disagree, so there is no \
                             one shape to hold the row to",
                            sel_name = sel.name().to_string_lossy(),
                            a = first.to_string_lossy(),
                            b = other.to_string_lossy()
                        ));
                    }
                }
                return (Source::Protocol(first), Some(encoding.clone()));
            }
            for name in target.authority_classes {
                // SAFETY: every name here is a framework class that is linked
                // and live; `sel` is interned. `method_types` answers `None`
                // for a selector the class does not implement.
                if let Some(types) = unsafe { method_types(class(name), sel) } {
                    return (Source::Class(name), Some(strip_method_offsets(&types)));
                }
            }
            let name = sel.name().to_string_lossy().into_owned();
            let Some(family) = target.families.iter().find(|f| f.sel == name) else {
                return (Source::Nowhere, None);
            };
            // The row itself must still be undeclared: if AppKit ever declares
            // it, the derivation is not merely unnecessary, it is a WEAKER
            // answer than the one now available, and saying so is how this
            // stops being permanent.
            if let Some(direct) = protocol_declares(family.protocol, family.sel) {
                self.fail(format!(
                    "{} is now declared by {} ({direct}) — the sibling derivation is stale and \
                     should be replaced by the declaration itself",
                    family.sel,
                    family.protocol.to_string_lossy()
                ));
            }
            let mut agreed: Option<String> = None;
            for sibling in family.siblings {
                let Some(types) = protocol_declares(family.protocol, sibling) else {
                    self.fail(format!(
                        "{}'s shape is derived from {sibling}, which {} no longer declares — the \
                         derivation has lost a leg",
                        family.sel,
                        family.protocol.to_string_lossy()
                    ));
                    return (Source::Nowhere, None);
                };
                match &agreed {
                    None => agreed = Some(types),
                    Some(first) if *first == types => {}
                    Some(first) => {
                        self.fail(format!(
                            "{}'s siblings disagree — {first} against {types} — so there is no one \
                             family shape to hold it to",
                            family.sel
                        ));
                        return (Source::Nowhere, None);
                    }
                }
            }
            assert!(
                family.siblings.len() >= 2,
                "a one-sibling family makes the agreement check vacuous"
            );
            (Source::Siblings(family.protocol), agreed)
        }

        /// PART A — the whole registered table, each row against its authority.
        ///
        /// Returns the set of protocols that supplied an authority — the
        /// DERIVED half of the conformance check — and every row the class
        /// registered WITH its encoding, which is what keeps part C from
        /// sending a message to a method that is no longer there and what part
        /// D holds the undeclared rows to.
        fn methods(
            &mut self,
            target: &Target,
            authorities: &[&'static CStr],
            cls: aterm_objc::ClassPtr,
        ) -> (BTreeSet<String>, BTreeMap<String, String>) {
            // SAFETY: `cls` is the live class of the delegate AppKit handed back.
            let mut rows = unsafe { class_methods(cls) };
            rows.sort_by_key(|(s, _)| s.name().to_string_lossy().into_owned());
            let registered_count = rows.len();
            if let Rows::Patched(ours) = target.rows {
                // A FRAMEWORK class: audit the rows the fork installed, not
                // Apple's table. Every named row must be present — a missing
                // one means the selector was renamed out from under the
                // swizzle, which is the loudest thing this target can find
                // short of the IMP check.
                for name in ours {
                    if !rows
                        .iter()
                        .any(|(s, _)| s.name().to_string_lossy() == **name)
                    {
                        self.fail(format!(
                            "{} does not register {name} at all — the fork patches that row, so \
                             either the framework moved it or the patch is aimed at a selector \
                             that no longer exists",
                            target.class_name
                        ));
                    }
                }
                rows.retain(|(s, _)| ours.contains(&&*s.name().to_string_lossy().into_owned()));
            }
            println!(
                "\n=== A. REGISTERED METHOD TABLE ({} of {registered_count} rows{}) ===",
                rows.len(),
                match target.rows {
                    Rows::Declared => "",
                    Rows::Patched(_) => " — the fork's, of a framework class's",
                }
            );
            println!(
                "     {:<52} {:<16} {:<16} SOURCE",
                "SELECTOR", "REGISTERED", "AUTHORITY"
            );
            let mut used = BTreeSet::new();
            let mut registered_rows = BTreeMap::new();
            for (selector, encoding) in rows {
                let name = selector.name().to_string_lossy().into_owned();
                let Some(registered) = encoding else {
                    registered_rows.insert(name.clone(), String::new());
                    self.fail(format!("{name}: registered with NO type encoding at all"));
                    continue;
                };
                let registered = strip_method_offsets(&registered);
                registered_rows.insert(name.clone(), registered.clone());
                let (source, authority) = self.authority_of(target, authorities, selector);
                match source {
                    Source::Protocol(p) | Source::Siblings(p) => {
                        used.insert(p.to_string_lossy().into_owned());
                    }
                    Source::Class(_) | Source::Nowhere => {}
                }
                let covered_by_part_d = target.unchecked.iter().any(|u| u.sel == name.as_str());
                let verdict = match &authority {
                    Some(a) if *a == registered => "ok ",
                    Some(_) => "BAD",
                    // `?? ` would say "unknown", and for a row on the
                    // [`Unchecked`] list that is the old lie: it has no
                    // AUTHORITY, and it is still checked, in part D.
                    None if covered_by_part_d => "D  ",
                    None => "?? ",
                };
                println!(
                    "  {verdict} {name:<52} {registered:<16} {:<16} {}",
                    authority.as_deref().unwrap_or("-"),
                    source.label(selector)
                );
                if matches!(source, Source::Siblings(_))
                    && let Some(family) = target.families.iter().find(|f| f.sel == name.as_str())
                {
                    // The derivation, printed where the row is judged: a
                    // reader should not have to take "siblings in P" on trust
                    // when the reason for it is a constant away.
                    println!(
                        "      derived from {} — {}",
                        family.siblings.join(" and "),
                        family.why
                    );
                }
                match authority {
                    Some(a) if a == registered => {}
                    Some(a) => self.fail(format!(
                        "{name}: registered {registered} but {} says {a}",
                        source.label(selector)
                    )),
                    None => {
                        if let Some(row) = target.unchecked.iter().find(|u| u.sel == name.as_str())
                        {
                            // NOT an exemption: part D holds this row to a
                            // shape and drives it through Foundation. What is
                            // printed here is why the authority lookup came
                            // back empty.
                            println!("      no authority: {}", row.why);
                            println!("      checked in part D against {}", row.rust);
                        } else {
                            // THE UNIVERSAL IS MEASURED BEFORE IT IS PRINTED.
                            // The sentence that stood here — "nothing in the
                            // runtime declares it" — was a statement about the
                            // runtime made from a hand-written list of four
                            // names, and a plant on
                            // `gestureRecognizerShouldBegin:` proved what that
                            // is worth: `NSGestureRecognizerDelegate` declares
                            // that row, `NSWindow` claims that protocol, and the
                            // finding sent its reader to the `Unchecked` list to
                            // write down a shape AppKit already publishes.
                            let declared = protocols_declaring(selector);
                            if !declared.is_empty() {
                                // ALL of them, not the first the runtime's
                                // table happened to hold: a plant on
                                // `applicationShouldTerminate:` is declared by
                                // `NSApplicationDelegate` AND by a private
                                // `NSApplicationTestingDelegate`, and a finding
                                // that named only whichever came first would
                                // point at the private one half the time.
                                let named: Vec<String> =
                                    declared.iter().map(|(p, e)| format!("{p} ({e})")).collect();
                                self.fail(format!(
                                    "{name}: none of the {} protocols {} answers to declares it, \
                                     but {} of the {} loaded in this process DO — {} — so add one \
                                     to this target's `claimed` or `informal` seed and the row is \
                                     checked against AppKit's own declaration. An `Unchecked` \
                                     entry here would write down by hand a shape the runtime \
                                     already holds",
                                    authorities.len(),
                                    target.class_name,
                                    declared.len(),
                                    loaded_protocol_count(),
                                    named.join(", ")
                                ));
                            } else {
                                self.fail(format!(
                                    "{name}: no protocol declares it — searched all {} loaded in \
                                     this process, not just the {} {} answers to — no class on its \
                                     chain implements it, no sibling family covers it, and part D \
                                     holds it to no shape. This row cannot be checked",
                                    loaded_protocol_count(),
                                    authorities.len(),
                                    target.class_name
                                ));
                            }
                        }
                    }
                }
            }
            (used, registered_rows)
        }

        /// PART B — the conformance claim, derived and declared, then re-asked
        /// of the instance.
        fn protocols(
            &mut self,
            target: &Target,
            authorities: &[&'static CStr],
            cls: aterm_objc::ClassPtr,
            instance: Id,
            used: &BTreeSet<String>,
        ) {
            // SAFETY: `cls` is the live class of a live object.
            let claimed = unsafe { class_protocols(cls) };
            println!("\n=== B. PROTOCOL CONFORMANCE ===");
            println!("  class_copyProtocolList = {claimed:?}");
            println!(
                "  authority protocols derived for this target = {}",
                authorities.len()
            );
            println!("  authorities actually used by the table above = {used:?}");

            // THE DERIVED TOOTH. The question is whether the INSTANCE conforms,
            // not whether this class is the link that makes it conform:
            // `-conformsToProtocol:` walks the superclass chain, and that is
            // the exact call AppKit makes before it will treat an object as a
            // delegate, a text input client or a responder.
            //
            // The rule used to be "must appear in `class_copyProtocolList`",
            // which was right for `WindowDelegate` and WRONG in general, as
            // `WinitView` proved the day it was audited. THE SENTENCE THAT USED
            // TO SIT HERE WAS WRONG IN BOTH ITS HALVES and is corrected in
            // place, because it contradicted this file's own `VIEW_INFORMAL`
            // 500 lines above AND the transcript printed three lines below it
            // on every single run:
            //
            //  * "two of its rows" — it is FOUR rows across the two protocols.
            //    THREE take their authority directly (`insertTab:` and
            //    `cancelOperation:` from `NSStandardKeyBindingResponding`,
            //    `touchBar` from `NSTouchBarProvider`) and a fourth,
            //    `insertBackTab:`, is derived from the first protocol's
            //    siblings, which puts that protocol in `derived` too.
            //  * "which NSView already conforms to" — true of exactly ONE of
            //    them. `NSView` conforms to `NSTouchBarProvider` by inheritance
            //    (`NSResponder` claims it) and does NOT conform to
            //    `NSStandardKeyBindingResponding`; the run below prints
            //    `-conformsToProtocol: = false` for it, which is the whole
            //    reason `VIEW_INFORMAL` exists.
            //
            // What survives is the RULE, which was always the point: the
            // question is whether the INSTANCE conforms, not whether this class
            // is the link that makes it. Where the conformance comes from is
            // printed, because "own" versus "inherited" versus "nowhere, and
            // exempt with a reason" is the interesting half of the answer even
            // when it passes.
            // The derived list is already `&'static CStr` — `cstr_named` leaks
            // each runtime-supplied name once — so this stays on `protocol()`'s
            // contract without widening the crate's API for a gate.
            for p in authorities {
                let name = p.to_string_lossy().into_owned();
                if !used.contains(&name) {
                    continue;
                }
                // SAFETY: `-conformsToProtocol:` is `B@:@` on every Apple
                // runtime; `instance` is live and `protocol` answers a live
                // protocol or nil (for which the runtime answers NO).
                let conforms: Bool = unsafe {
                    let send: unsafe extern "C" fn(Id, Sel, aterm_objc::ProtocolPtr) -> Bool =
                        msg();
                    send(instance, sel!(conformsToProtocol:), protocol(p))
                };
                let informal = target.informal.iter().find(|(n, _)| *n == name.as_str());
                let how = if claimed.contains(&name) {
                    "claimed by this class"
                } else if conforms.as_bool() {
                    "inherited from a superclass"
                } else {
                    "not conformed to anywhere"
                };
                println!(
                    "  authority {name}: -conformsToProtocol: = {} ({how})",
                    conforms.as_bool()
                );
                if let Some((_, why)) = informal {
                    println!("      exempt: {why}");
                    if conforms.as_bool() {
                        // The exemption has outlived its reason: something now
                        // conforms, so the tooth could be armed here and this
                        // entry is silently weakening the gate.
                        self.fail(format!(
                            "{name} is listed as INFORMAL, but -conformsToProtocol: now answers \
                             YES — the exemption is stale and is disarming a check that would \
                             pass"
                        ));
                    }
                    continue;
                }
                if !conforms.as_bool() {
                    self.fail(format!(
                        "the class implements rows whose authority is {name}, but neither it nor \
                         any superclass conforms to {name} — conformsToProtocol: answers NO to \
                         AppKit for a class that implements {name}'s rows"
                    ));
                }
            }
            for p in target.claimed {
                let name = p.to_string_lossy().into_owned();
                if !claimed.contains(&name) {
                    self.fail(format!(
                        "this audit's `claimed` constant names {name} — transcribed from the \
                         fork's `protocols:` list, and the ONE mirror in this file — but the \
                         registered class does not claim it"
                    ));
                }
                // SAFETY: `-conformsToProtocol:` is `B@:@` on every Apple
                // runtime; `delegate` is a live object and `protocol` answers a
                // live protocol or nil (for which the runtime answers NO).
                let conforms: Bool = unsafe {
                    let send: unsafe extern "C" fn(Id, Sel, aterm_objc::ProtocolPtr) -> Bool =
                        msg();
                    send(instance, sel!(conformsToProtocol:), protocol(p))
                };
                println!("  -conformsToProtocol:{name} = {}", conforms.as_bool());
                if !conforms.as_bool() {
                    self.fail(format!("the INSTANCE does not conform to {name}"));
                }
            }
        }

        /// PART C (delegate) — two live sends, so the verdict is about code
        /// and not only about a table. These are the two RETURN SHAPES that
        /// port changed: the `BOOL` that stayed a `BOOL`, and the `NSUInteger`
        /// that P3 made one.
        fn delegate_live_sends(
            &mut self,
            delegate: Id,
            window: Id,
            registered: &BTreeMap<String, String>,
        ) {
            println!("\n=== C. LIVE SENDS THROUGH THE REGISTERED IMPS ===");
            // A selector the class no longer registers would reach
            // `-doesNotRecognizeSelector:` and ABORT the process — which the
            // ladder reads as "no exit status", a correct RED with a useless
            // diagnosis. Ask the table part A already read, and report the
            // absence as the finding it is.
            let mut absent = false;
            for want in [
                "windowShouldClose:",
                "window:willUseFullScreenPresentationOptions:",
            ] {
                if !registered.contains_key(want) {
                    absent = true;
                    self.fail(format!(
                        "{want} is not in the registered table, so the live send that would \
                         exercise its return shape cannot be made"
                    ));
                }
            }
            if absent {
                return;
            }
            // SAFETY: `windowShouldClose:` is registered `B@:@` (audited in part
            // A); `delegate` and `window` are live objects.
            let should_close: Bool = unsafe {
                let send: unsafe extern "C" fn(Id, Sel, Id) -> Bool = msg();
                send(delegate, sel!(windowShouldClose:), window)
            };
            println!("  -windowShouldClose: = {}", should_close.as_bool());
            if should_close.as_bool() {
                self.fail(
                    "windowShouldClose: answered YES; the fork returns NO and routes the close \
                     through CloseRequested"
                        .to_owned(),
                );
            }

            // A value with a bit set in every byte of the low word, so a
            // one-byte or four-byte return would lose part of it.
            const PROBE: usize = 0x0102_0304_0506_0708;
            // SAFETY: `window:willUseFullScreenPresentationOptions:` is
            // registered `Q@:@Q` (audited in part A); the fork returns the
            // proposed options unchanged unless the window is in EXCLUSIVE
            // fullscreen, which a freshly created window is not.
            let echoed: usize = unsafe {
                let send: unsafe extern "C" fn(Id, Sel, Id, usize) -> usize = msg();
                send(
                    delegate,
                    sel!(window:willUseFullScreenPresentationOptions:),
                    window,
                    PROBE,
                )
            };
            println!(
                "  -window:willUseFullScreenPresentationOptions:({PROBE:#018x}) = {echoed:#018x}"
            );
            if echoed != PROBE {
                self.fail(format!(
                    "the NSUInteger row lost bits: sent {PROBE:#018x}, got {echoed:#018x}"
                ));
            }
        }

        /// PART C (view) — the three shapes `view.rs` is the first in this
        /// crate to register, sent for real.
        ///
        /// # Why one of these also goes through `NSMethodSignature`
        ///
        /// A direct typed send proves the trampoline and the caller agree about
        /// REGISTERS. It carries no encoding at all, so it cannot notice a
        /// wrong one — P1 measured exactly that.
        ///
        /// This used to say that `NSMethodSignature` is where our string gets
        /// read — that Foundation "parses what `class_addMethod` was handed,
        /// computes the return length from it, lays out the frame and calls the
        /// IMP itself". FOR THIS ROW IT DOES NOT. `WinitView` conforms to
        /// `NSTextInputClient`, and `-methodSignatureForSelector:` answers from
        /// a conformed-to protocol's declaration in preference to the class's
        /// own table (measured every run — see [`Audit::teeth_are_live`]). What
        /// the input method gets when it places a candidate window is therefore
        /// the PROTOCOL's layout, whatever we registered, and the question
        /// worth asking is whether our string is that string. See
        /// [`Audit::first_rect_through_foundation`].
        fn view_live_sends(&mut self, view: Id, registered: &BTreeMap<String, String>) {
            println!("\n=== C. LIVE SENDS THROUGH THE REGISTERED IMPS (WinitView) ===");
            let mut absent = false;
            for want in [
                "hasMarkedText",
                "markedRange",
                "characterIndexForPoint:",
                "firstRectForCharacterRange:actualRange:",
            ] {
                if !registered.contains_key(want) {
                    absent = true;
                    self.fail(format!(
                        "{want} is not in the registered table, so the live send that would \
                         exercise its return shape cannot be made"
                    ));
                }
            }
            if absent {
                return;
            }

            // 1. BOOL. A fresh view has no marked text.
            // SAFETY: `hasMarkedText` is registered `B@:` (audited in part A)
            // and `view` is the live view AppKit handed back.
            let has_marked: Bool = unsafe {
                let send: unsafe extern "C" fn(Id, Sel) -> Bool = msg();
                send(view, sel!(hasMarkedText))
            };
            println!("  -hasMarkedText = {}", has_marked.as_bool());
            if has_marked.as_bool() {
                self.fail(
                    "hasMarkedText answered YES on a view that has never composed; the ivar it \
                     reads is an empty NSMutableAttributedString"
                        .to_owned(),
                );
            }

            // 2. A 16-BYTE STRUCT RETURN, by value. On arm64 this comes back in
            //    x0/x1 and never touches the indirect path; the value is what
            //    proves the two halves agree about which register holds which
            //    field. `NSNotFound` is `NSIntegerMax`, so a truncating or
            //    swapped return cannot produce it by accident.
            // SAFETY: `markedRange` is registered `{_NSRange=QQ}@:`.
            let marked: aterm_objc::NSRange = unsafe {
                let send: unsafe extern "C" fn(Id, Sel) -> aterm_objc::NSRange = msg();
                send(view, sel!(markedRange))
            };
            println!(
                "  -markedRange = {{location: {:#x}, length: {}}}",
                marked.location, marked.length
            );
            if marked.location != NS_NOT_FOUND || marked.length != 0 {
                self.fail(format!(
                    "markedRange answered {{{:#x}, {}}}; NSTextInputClient documents \
                     {{NSNotFound, 0}} = {{{NS_NOT_FOUND:#x}, 0}} when there is no marked range",
                    marked.location, marked.length
                ));
            }

            // 3. A STRUCT ARGUMENT by value, answering an NSUInteger.
            // SAFETY: `characterIndexForPoint:` is registered
            // `Q@:{CGPoint=dd}`; the fork answers 0 for every point.
            let idx: usize = unsafe {
                let send: unsafe extern "C" fn(Id, Sel, aterm_objc::CGPoint) -> usize = msg();
                send(
                    view,
                    sel!(characterIndexForPoint:),
                    aterm_objc::CGPoint { x: 12.0, y: 34.0 },
                )
            };
            println!("  -characterIndexForPoint:{{12,34}} = {idx}");
            if idx != 0 {
                self.fail(format!(
                    "characterIndexForPoint: answered {idx}; the fork answers 0 unconditionally"
                ));
            }

            // 4. THE 32-BYTE STRUCT RETURN, against the signature an input
            //    method is handed for it.
            let first_rect = registered["firstRectForCharacterRange:actualRange:"].clone();
            self.first_rect_through_foundation(view, &first_rect);
        }

        /// `firstRectForCharacterRange:actualRange:` asked of Foundation —
        /// AND THE QUESTION IS NOT THE ONE THIS USED TO ASK.
        ///
        /// # What this function used to claim, and why it could not fail
        ///
        /// It asserted `methodReturnLength == 32` under the comment "this
        /// number comes from OUR registered string", and that a direct send and
        /// an `NSInvocation` agreed under "Foundation's frame layout, computed
        /// from the REGISTERED encoding". Neither sentence is true here.
        /// `-[NSObject methodSignatureForSelector:]` answers from the PROTOCOL
        /// whenever the class conforms to one declaring the selector, and
        /// `WinitView` claims `NSTextInputClient`. So the 32 was the protocol's
        /// 32 no matter what the class registered, and both sides of the
        /// `NSInvocation` comparison were laid out from that same protocol
        /// signature — the two checks could not fail on this row for any
        /// registered string at all. Two plants proved it: `{_PlantRect=qqqq}`
        /// (32 bytes, non-HFA, `x8`-indirect, against an HFA-returning IMP) and
        /// `{_PlantRect=qq}` (16 bytes) both walked straight through them.
        /// [`Audit::teeth_are_live`] measures the mechanism in this process
        /// rather than citing it.
        ///
        /// # The question that does have a failing answer
        ///
        /// Foundation will use the PROTOCOL's layout. The class was compiled
        /// against its own. If those two strings differ, an input method lays
        /// out the frame for this row one way while the IMP was built the
        /// other, and the candidate window goes wherever the mismatch puts it.
        /// So: ask Foundation for the signature the input method will get, and
        /// require the class's own registered return type to BE that one.
        ///
        /// That is part A's comparison arriving through the API AppKit actually
        /// calls rather than through `protocol_getMethodDescription`, and it is
        /// deliberately kept: part A proves the strings match, this proves the
        /// string Foundation hands an IME is the matching one.
        fn first_rect_through_foundation(&mut self, view: Id, registered: &str) {
            let sel = sel!(firstRectForCharacterRange:actualRange:);
            // SAFETY: `view` is live; `-methodSignatureForSelector:` is
            // `-(id)(SEL)` on NSObject and returns an autoreleased signature or
            // nil.
            let sig: Id = unsafe {
                let f: unsafe extern "C" fn(Id, Sel, Sel) -> Id = msg();
                f(view, sel!(methodSignatureForSelector:), sel)
            };
            if sig.is_null() {
                self.fail(
                    "Foundation could not build an NSMethodSignature for \
                     firstRectForCharacterRange:actualRange: — neither from NSTextInputClient's \
                     declaration nor from the registered encoding, which is the failure that puts \
                     a candidate window nowhere"
                        .to_owned(),
                );
                return;
            }
            let Some((ret, args)) = split_encoding(registered) else {
                self.fail(format!(
                    "the registered encoding {registered} does not parse as a method encoding, so \
                     nothing can be compared against it"
                ));
                return;
            };
            // SAFETY: `-methodReturnLength` and `-numberOfArguments` are
            // `-(NSUInteger)` on a live signature; `-methodReturnType` is
            // `-(const char *)` and its buffer belongs to the signature, which
            // is alive for this scope.
            let (ret_len, argc, foundation_ret) = unsafe {
                let n: unsafe extern "C" fn(Id, Sel) -> usize = msg();
                let t: unsafe extern "C" fn(Id, Sel) -> *const std::ffi::c_char = msg();
                (
                    n(sig, sel!(methodReturnLength)),
                    n(sig, sel!(numberOfArguments)),
                    CStr::from_ptr(t(sig, sel!(methodReturnType)))
                        .to_string_lossy()
                        .into_owned(),
                )
            };
            println!(
                "  NSMethodSignature: returnType = {foundation_ret} ({ret_len} bytes), arguments \
                 = {argc}"
            );
            println!(
                "  registered:        returnType = {ret} , arguments = {}",
                args.len()
            );
            if foundation_ret != ret {
                self.fail(format!(
                    "an input method asking Foundation for this row's signature is told the return \
                     is {foundation_ret}, and the class registered {ret}. Foundation answers from \
                     NSTextInputClient here, not from our string (measured — see part E), so this \
                     is the layout an IME will use against an IMP compiled for another one"
                ));
            }
            if argc != args.len() {
                self.fail(format!(
                    "Foundation reads {argc} arguments where the registered encoding has {}; \
                     firstRectForCharacterRange:actualRange: has four counting self and _cmd",
                    args.len()
                ));
            }

            // And the row, actually sent, because a table that agrees with
            // itself is still not a rectangle. The value is printed rather than
            // asserted: it is the fork's IME position converted to screen
            // coordinates, which depends on where the window landed.
            let range = aterm_objc::NSRange {
                location: 0,
                length: 1,
            };
            // SAFETY: the row is registered
            // `{CGRect=…}@:{_NSRange=QQ}^{_NSRange=QQ}` (audited in part A); a
            // null `actualRange:` is what AppKit itself passes when it does not
            // want one back, and the fork ignores the argument entirely.
            let direct: aterm_objc::CGRect = unsafe {
                let send: unsafe extern "C" fn(
                    Id,
                    Sel,
                    aterm_objc::NSRange,
                    *mut aterm_objc::NSRange,
                ) -> aterm_objc::CGRect = msg();
                send(view, sel, range, std::ptr::null_mut())
            };
            println!(
                "  -firstRectForCharacterRange: direct = {:?} {:?}",
                direct.origin, direct.size
            );
        }

        /// PART D — the rows the runtime declares NOWHERE, which were the rows
        /// nothing checked.
        ///
        /// See [`Unchecked`]. Two things happen per row, and they fail on
        /// different defects:
        ///
        /// * The registered encoding must be what the fork's own Rust signature
        ///   produces through `aterm_objc::method_encoding!`. That is the
        ///   tooth a retyped argument or return trips — the two plants that
        ///   walked through every gate in the tree were exactly that.
        /// * Foundation must read the registered string back the way it was
        ///   written. For THESE rows it genuinely does read our string, and
        ///   part E measures exactly that on three probes rather than on two:
        ///   a class with NO protocol at all, a protocol-claiming class asked
        ///   about a row its protocol DOES declare, and — the case this
        ///   sentence used to cite while nothing in the tree measured it — a
        ///   protocol-claiming class asked about a row its protocol does NOT
        ///   declare, which is the shape `frameDidChange:` has on a
        ///   `WinitView` that claims `NSTextInputClient`. So a string the
        ///   runtime accepted and Foundation parses differently is visible
        ///   here, and the premise that it would be is measured next door.
        ///
        /// What is NOT done here, and the reason is the row rather than the
        /// instrument: the direct-send-against-`NSInvocation` comparison that
        /// catches a lying encoding needs a RETURN VALUE to compare, and this
        /// row returns void. So that tooth is drawn in part E instead, on a
        /// control row with the same property (no protocol declares it) and a
        /// rectangle to disagree about — which is what proves it would bite
        /// here if there were anything to see.
        fn unchecked_rows(
            &mut self,
            target: &Target,
            instance: Id,
            rows: &BTreeMap<String, String>,
        ) {
            if target.unchecked.is_empty() {
                return;
            }
            println!("\n=== D. THE ROWS NOTHING IN THE RUNTIME DECLARES ===");
            for row in target.unchecked {
                let Some(registered) = rows.get(row.sel) else {
                    self.fail(format!(
                        "{}: part D holds a shape for it, but the class does not register it at \
                         all — either the fork deleted the row or this list names a selector that \
                         never existed",
                        row.sel
                    ));
                    continue;
                };
                let expected = (row.expected)();
                println!("  {} registered {registered}", row.sel);
                println!("      expected  {expected}   from `{}`", row.rust);
                if *registered != expected {
                    self.fail(format!(
                        "{}: registered {registered}, but the fork's own signature `{}` encodes to \
                         {expected}. Nothing in the runtime declares this row, so the encoding is \
                         the only description of it that exists",
                        row.sel, row.rust
                    ));
                }
                self.foundation_reads_our_string(instance, row.sel, registered);
            }
        }

        /// Foundation's parse of one row's REGISTERED string, compared against
        /// that string.
        ///
        /// Only ever called for a row no protocol declares, which is what makes
        /// it a question about our encoding rather than about a header.
        fn foundation_reads_our_string(&mut self, instance: Id, name: &str, registered: &str) {
            let sel = sel_named(name);
            // SAFETY: `instance` is live and `-methodSignatureForSelector:` is
            // `-(id)(SEL)` on NSObject, answering an autoreleased signature or
            // nil.
            let sig: Id = unsafe {
                let f: unsafe extern "C" fn(Id, Sel, Sel) -> Id = msg();
                f(instance, sel!(methodSignatureForSelector:), sel)
            };
            if sig.is_null() {
                self.fail(format!(
                    "{name}: Foundation could not build an NSMethodSignature from the registered \
                     encoding {registered} — the runtime accepted a string Foundation cannot parse"
                ));
                return;
            }
            let Some((ret, args)) = split_encoding(registered) else {
                self.fail(format!(
                    "{name}: the registered encoding {registered} does not parse as a method \
                     encoding"
                ));
                return;
            };
            // SAFETY: `-numberOfArguments` is `-(NSUInteger)`,
            // `-methodReturnType` is `-(const char *)` and
            // `-getArgumentTypeAtIndex:` is `-(const char *)(NSUInteger)`, all
            // on a live signature whose buffers outlive this scope.
            let (argc, foundation_ret) = unsafe {
                let n: unsafe extern "C" fn(Id, Sel) -> usize = msg();
                let t: unsafe extern "C" fn(Id, Sel) -> *const std::ffi::c_char = msg();
                (
                    n(sig, sel!(numberOfArguments)),
                    CStr::from_ptr(t(sig, sel!(methodReturnType)))
                        .to_string_lossy()
                        .into_owned(),
                )
            };
            println!("      Foundation reads: {foundation_ret} <- {argc} argument(s)");
            if foundation_ret != ret {
                self.fail(format!(
                    "{name}: registered a {ret} return and Foundation read {foundation_ret} out of \
                     the same string"
                ));
            }
            if argc != args.len() {
                self.fail(format!(
                    "{name}: registered {} arguments and Foundation read {argc} out of the same \
                     string",
                    args.len()
                ));
                return;
            }
            for (i, want) in args.iter().enumerate() {
                // SAFETY: as above; `i` is below `numberOfArguments`.
                let got = unsafe {
                    let g: unsafe extern "C" fn(Id, Sel, usize) -> *const std::ffi::c_char = msg();
                    CStr::from_ptr(g(sig, sel!(getArgumentTypeAtIndex:), i))
                        .to_string_lossy()
                        .into_owned()
                };
                if got != *want {
                    self.fail(format!(
                        "{name}: argument {i} is registered {want} and Foundation read {got}"
                    ));
                }
            }
        }

        /// PART E — THE INSTRUMENT'S OWN TEETH, DRAWN ON A CONTROL BEFORE THEY
        /// ARE TRUSTED ON THE FORK.
        ///
        /// Every check in this file is an argument about what Foundation does
        /// with a registered encoding, and for one whole wave two of them were
        /// arguments about something Foundation does not do. The comments were
        /// confident, the code compiled, the transcript printed numbers, and
        /// the checks could not fail for any input. Nothing in a gate is more
        /// expensive than that, because it reads exactly like a gate that
        /// passes.
        ///
        /// So the mechanism is MEASURED, here, in this process, on classes this
        /// function builds for the purpose — and the measurements are the
        /// premises the rest of the file stands on:
        ///
        /// 1. For a selector NO protocol declares, `NSMethodSignature` is built
        ///    from the class's own registered string. Two rows share one IMP:
        ///    one registered honestly as `{CGRect=…}`, one registered as a
        ///    32-byte non-HFA lie. The honest row answers the same rectangle
        ///    through `NSInvocation` and through a direct send; the LYING one
        ///    must NOT — Foundation lays out an `x8` indirect return for an IMP
        ///    that answered in `d0`-`d3`. If those two ever agree, this file's
        ///    part D has quietly become decoration and says so.
        /// 2. For a selector a CONFORMED-TO protocol declares, the protocol
        ///    wins and the registered string is not consulted. A class claiming
        ///    `NSTextInputClient` registers a 16-byte lie for
        ///    `firstRectForCharacterRange:actualRange:` and Foundation still
        ///    reports `NSRect`. THAT is why the old teeth could not fire, and
        ///    if it ever stops being true the finding says which check can be
        ///    re-armed on the real class.
        /// 3. For a selector a conformed-to protocol does NOT declare, the
        ///    protocol-claiming class answers FROM ITS OWN TABLE again. That is
        ///    the shape `WinitView` actually has — it claims
        ///    `NSTextInputClient` and registers `frameDidChange:`, which no
        ///    protocol declares — and it is the premise part D's second tooth
        ///    stands on. Part D cited part E for it while part E measured only
        ///    (1) and (2): a no-protocol probe and a protocol-claiming probe
        ///    asked about a row the protocol DOES declare. Neither is this
        ///    case. It is closed by ONE more `add_method` on the same
        ///    protocol-claiming class, registering the same 32-byte lie under a
        ///    selector `NSTextInputClient` has never heard of; if the protocol
        ///    ever started winning there too, part D would be checking a string
        ///    Foundation does not read and would say so here.
        fn teeth_are_live(&mut self) {
            println!("\n########## E. THE INSTRUMENT'S OWN TEETH ##########");
            let honest = format!("{}@:", <aterm_objc::CGRect as aterm_objc::Encode>::ENCODING);
            let imp = probe_rect as *const std::ffi::c_void;

            // 1. A class NOTHING declares anything for.
            let mut builder = aterm_objc::begin(c"NSObject", c"ATermAuditProbe");
            builder.add_rust_ivar::<()>();
            // SAFETY: `probe_rect` is `extern "C"`, has the exact `(id, SEL)`
            // prototype both encodings describe, returns by value and cannot
            // unwind. The second registration is DELIBERATELY a wrong encoding
            // for that prototype — that is the control — and it is only ever
            // reached through this function.
            unsafe {
                builder.add_method(sel!(atermAuditHonestRect), imp, &honest);
                builder.add_method(sel!(atermAuditLyingRect), imp, PROBE_LIE);
            }
            let probe = builder.register();
            // SAFETY: `+alloc`/`-init` on a freshly registered `NSObject`
            // subclass whose only ivar is a zero-sized Rust payload.
            let obj = unsafe {
                let send: unsafe extern "C" fn(Id, Sel) -> Id = msg();
                send(send(probe.class().as_id(), sel!(alloc)), sel!(init))
            };
            if obj.is_null() {
                self.fail("the audit could not allocate its own probe object".to_owned());
                return;
            }
            let honest_ok = self.probe_row(obj, sel!(atermAuditHonestRect), &honest, true);
            let lying_ok = self.probe_row(obj, sel!(atermAuditLyingRect), PROBE_LIE, false);
            if honest_ok && lying_ok {
                println!(
                    "  the NSInvocation tooth is LIVE: it agrees with a direct send on an honest \
                     row and disagrees on a lying one"
                );
            }

            // 2. The same experiment against a CLAIMED protocol.
            if protocol(c"NSTextInputClient").is_null() {
                self.fail(
                    "NSTextInputClient is absent from this process, so the mechanism the view's \
                     checks depend on cannot be measured"
                        .to_owned(),
                );
                return;
            }
            let sel = sel!(firstRectForCharacterRange:actualRange:);
            let lie = "{_ATermAuditLie=qq}@:{_NSRange=QQ}^{_NSRange=QQ}";
            let mut builder = aterm_objc::begin(c"NSObject", c"ATermAuditProtoProbe");
            builder.add_rust_ivar::<()>();
            builder.add_protocol(c"NSTextInputClient");
            // SAFETY: the IMP's prototype does not match either encoding and
            // is never invoked through them — the rows exist to be ASKED ABOUT,
            // not called, and nothing outside this function can reach the
            // class. The second row is the (3) case: the SAME 32-byte lie the
            // no-protocol probe used, under a selector `NSTextInputClient` does
            // not declare, on a class that claims `NSTextInputClient`.
            unsafe {
                builder.add_method(sel, imp, lie);
                builder.add_method(sel!(atermAuditUndeclaredRect), imp, PROBE_LIE);
            }
            let claimer = builder.register();
            // SAFETY: as above.
            let obj = unsafe {
                let send: unsafe extern "C" fn(Id, Sel) -> Id = msg();
                send(send(claimer.class().as_id(), sel!(alloc)), sel!(init))
            };
            // SAFETY: `obj` is live; `-methodSignatureForSelector:` answers an
            // autoreleased signature or nil, and `-methodReturnType` is
            // `-(const char *)` on it.
            let answered = unsafe {
                let f: unsafe extern "C" fn(Id, Sel, Sel) -> Id = msg();
                let sig = f(obj, sel!(methodSignatureForSelector:), sel);
                if sig.is_null() {
                    None
                } else {
                    let t: unsafe extern "C" fn(Id, Sel) -> *const std::ffi::c_char = msg();
                    Some(
                        CStr::from_ptr(t(sig, sel!(methodReturnType)))
                            .to_string_lossy()
                            .into_owned(),
                    )
                }
            };
            let registered_lie = split_encoding(lie).map(|(r, _)| r);
            let protocol_says = protocol_declares(
                c"NSTextInputClient",
                "firstRectForCharacterRange:actualRange:",
            )
            .and_then(|e| split_encoding(&e).map(|(r, _)| r));
            println!("  a class CLAIMING NSTextInputClient registered {registered_lie:?}");
            println!("    NSTextInputClient declares  {protocol_says:?}");
            println!("    methodSignatureForSelector: answers {answered:?}");
            if answered == registered_lie {
                self.fail(
                    "Foundation answered from the REGISTERED string for a row a conformed-to \
                     protocol declares. That is the opposite of what was measured when the \
                     firstRectForCharacterRange: checks were re-aimed: the return-length and \
                     NSInvocation teeth would bite on the real view again and should be restored \
                     there"
                        .to_owned(),
                );
            } else if answered != protocol_says {
                self.fail(format!(
                    "methodSignatureForSelector: answered {answered:?} for a row registered \
                     {registered_lie:?} against a protocol declaring {protocol_says:?} — neither \
                     source, so this file no longer knows where Foundation reads a signature from"
                ));
            } else {
                println!(
                    "  the protocol wins: WinitView's eleven NSTextInputClient rows are checked \
                     by part A against the same declaration, not by their return length"
                );
            }

            // 3. THE CASE PART D CITES: the same protocol-claiming class, asked
            //    about a selector the protocol does NOT declare. Our string
            //    must win, or part D's `foundation_reads_our_string` is reading
            //    something else.
            let undeclared = sel!(atermAuditUndeclaredRect);
            assert!(
                protocol_declares(c"NSTextInputClient", "atermAuditUndeclaredRect").is_none(),
                "this probe's whole point is a selector NSTextInputClient does not declare"
            );
            // SAFETY: `obj` is live; `-methodSignatureForSelector:` answers an
            // autoreleased signature or nil, and `-methodReturnType` is
            // `-(const char *)` on it.
            let answered_undeclared = unsafe {
                let f: unsafe extern "C" fn(Id, Sel, Sel) -> Id = msg();
                let sig = f(obj, sel!(methodSignatureForSelector:), undeclared);
                if sig.is_null() {
                    None
                } else {
                    let t: unsafe extern "C" fn(Id, Sel) -> *const std::ffi::c_char = msg();
                    Some(
                        CStr::from_ptr(t(sig, sel!(methodReturnType)))
                            .to_string_lossy()
                            .into_owned(),
                    )
                }
            };
            let lie_ret = split_encoding(PROBE_LIE).map(|(r, _)| r);
            println!(
                "  the SAME class, asked about a row NSTextInputClient does not declare:\n    \
                 registered {lie_ret:?}  methodSignatureForSelector: answers {answered_undeclared:?}"
            );
            if answered_undeclared != lie_ret {
                self.fail(format!(
                    "a class CLAIMING NSTextInputClient answered {answered_undeclared:?} for a \
                     selector that protocol does not declare, where it registered {lie_ret:?}. \
                     Part D's second tooth assumes Foundation reads OUR string for exactly this \
                     shape — a protocol-claiming class and an undeclared row, which is what \
                     `frameDidChange:` is on WinitView — so that tooth is now checking a string \
                     Foundation does not use"
                ));
            } else {
                println!(
                    "  our string wins for an undeclared row even on a protocol-claiming class: \
                     part D's frameDidChange: check reads what the class registered"
                );
            }
        }

        /// One probe row, sent directly and through `NSInvocation`.
        ///
        /// `expect_agreement` is what the row's encoding entitles it to: an
        /// honest row must round-trip, a lying one must not. Answers whether
        /// the row behaved as its encoding says it should.
        fn probe_row(&mut self, obj: Id, sel: Sel, encoding: &str, expect_agreement: bool) -> bool {
            let name = sel.name().to_string_lossy().into_owned();
            // SAFETY: `probe_rect` really is `(id, SEL) -> CGRect`; this is the
            // COMPILER's view of the row, which is correct for both
            // registrations because both are backed by that one function.
            let direct: aterm_objc::CGRect = unsafe {
                let send: unsafe extern "C" fn(Id, Sel) -> aterm_objc::CGRect = msg();
                send(obj, sel)
            };
            // SAFETY: `obj` is live; `-methodSignatureForSelector:` answers an
            // autoreleased signature or nil.
            let sig: Id = unsafe {
                let f: unsafe extern "C" fn(Id, Sel, Sel) -> Id = msg();
                f(obj, sel!(methodSignatureForSelector:), sel)
            };
            if sig.is_null() {
                self.fail(format!(
                    "Foundation could not build a signature from {encoding}, which this audit \
                     registered itself"
                ));
                return false;
            }
            // SAFETY: `+invocationWithMethodSignature:` answers an autoreleased
            // invocation for a live signature; `-setTarget:`/`-setSelector:`
            // are `-(void)(id)` and `-(void)(SEL)`; `-invoke` then
            // `-getReturnValue:` writes exactly `methodReturnLength` bytes,
            // which is 32 for both of these encodings, into a 32-byte
            // `CGRect`.
            let through: aterm_objc::CGRect = unsafe {
                let new: unsafe extern "C" fn(Id, Sel, Id) -> Id = msg();
                let inv = new(
                    class(c"NSInvocation").as_id(),
                    sel!(invocationWithMethodSignature:),
                    sig,
                );
                if inv.is_null() {
                    self.fail(
                        "NSInvocation refused a signature Foundation had just built".to_owned(),
                    );
                    return false;
                }
                let set_id: unsafe extern "C" fn(Id, Sel, Id) = msg();
                set_id(inv, sel!(setTarget:), obj);
                let set_sel: unsafe extern "C" fn(Id, Sel, Sel) = msg();
                set_sel(inv, sel!(setSelector:), sel);
                let go: unsafe extern "C" fn(Id, Sel) = msg();
                go(inv, sel!(invoke));
                let mut out = aterm_objc::CGRect::default();
                let get: unsafe extern "C" fn(Id, Sel, *mut std::ffi::c_void) = msg();
                get(
                    inv,
                    sel!(getReturnValue:),
                    std::ptr::from_mut(&mut out).cast(),
                );
                out
            };
            let agreed = direct == through;
            println!(
                "  -{name} registered {encoding}\n    direct = {:?} {:?}\n    NSInvocation = {:?} \
                 {:?}   agree={agreed}",
                direct.origin, direct.size, through.origin, through.size
            );
            if agreed != expect_agreement {
                self.fail(if expect_agreement {
                    format!(
                        "{name} is registered honestly and still answered differently through \
                         NSInvocation — this audit cannot round-trip a correct row, so none of its \
                         verdicts about layout mean anything"
                    )
                } else {
                    format!(
                        "{name} is registered {encoding} — a 32-byte non-HFA against an IMP that \
                         returns an HFA — and NSInvocation still answered what a direct send did. \
                         The comparison this file uses to detect a lying encoding no longer \
                         detects one, which is precisely the defect it was re-aimed to fix"
                    )
                });
                return false;
            }
            true
        }

        /// The class whose registered table describes `instance`, plus the isa
        /// when the two differ.
        ///
        /// See the note at the call site for why this is not `object_getClass`.
        fn audited_class(&mut self, instance: Id) -> (aterm_objc::ClassPtr, Option<String>) {
            // SAFETY: `instance` is a live object; `-class` is `#@:` on every
            // Apple runtime and `object_getClass` reads the isa directly.
            let (reported, isa) = unsafe {
                let send: unsafe extern "C" fn(Id, Sel) -> aterm_objc::ClassPtr = msg();
                (send(instance, sel!(class)), class_of(instance))
            };
            if reported.is_null() {
                // Nothing can be audited from a nil class; the caller's name
                // check will report it against the target.
                return (isa, None);
            }
            if reported == isa {
                return (reported, None);
            }
            // SAFETY: both are live class objects; `superclass_of` tolerates
            // and terminates at nil.
            let (isa_name, reaches) = unsafe {
                let mut walk = isa;
                let mut reaches = false;
                while !walk.is_null() {
                    if walk == reported {
                        reaches = true;
                        break;
                    }
                    walk = superclass_of(walk);
                }
                (class_name(isa).to_string_lossy().into_owned(), reaches)
            };
            if !reaches {
                // SAFETY: `reported` is a live class object.
                let reported_name = unsafe { class_name(reported) }
                    .to_string_lossy()
                    .into_owned();
                self.fail(format!(
                    "an instance whose isa is {isa_name} answers -class with {reported_name}, \
                     which is not on its superclass chain — the object is lying about its own \
                     identity and this audit cannot tell which table describes it"
                ));
            }
            (reported, Some(isa_name))
        }

        /// Parts A and B for one target, given a live instance of it.
        ///
        /// Answers the registered selector set so the caller's live sends can
        /// be guarded by it, or `None` if the instance was not of the class
        /// this target names — in which case the audit measured the wrong
        /// object and says so rather than reporting a clean table for it.
        fn table_and_conformance(
            &mut self,
            target: &Target,
            instance: Id,
        ) -> Option<BTreeMap<String, String>> {
            // THE ISA IS NOT ALWAYS THE CLASS, and this file learned it the
            // hard way the moment it started auditing the window: KVO
            // dynamically subclasses any observed instance and points its isa
            // at `NSKVONotifying_<Name>`. THIS FORK CREATES THAT ITSELF —
            // `WindowDelegate::new` sends
            // `-addObserver:forKeyPath:@"effectiveAppearance"` to every window
            // it makes — so `object_getClass` on a live `WinitWindow` answers
            // `NSKVONotifying_WinitWindow`, whose OWN method table holds KVO's
            // setters and NOT the two rows the fork registered. Reading it
            // would have reported an empty, entirely correct table.
            //
            // `-class` is the message KVO overrides precisely to hide itself,
            // and it is what AppKit and every other client sees. It is used
            // here, and then CHECKED rather than trusted: the isa must be the
            // reported class or a subclass of it, so a class that lies about
            // its own identity is a finding rather than a silently smaller
            // audit.
            let (cls, isa_name) = self.audited_class(instance);
            // SAFETY: `cls` came from `-class` on a live object.
            let name = unsafe { class_name(cls) }.to_string_lossy().into_owned();
            println!("\n########## {} ##########", target.class_name);
            println!("live class = {name}");
            if let Some(isa) = &isa_name {
                println!(
                    "  isa = {isa} (KVO has subclassed this instance; the registered rows are on \
                     {name})"
                );
            }
            if name != target.class_name {
                self.fail(format!(
                    "the object audited as {} is a {name} — this audit measured the wrong class",
                    target.class_name
                ));
                return None;
            }
            // The superclass chain, read rather than assumed: it is what
            // decides where the ivar slot sits and where `-dealloc`
            // super-sends, and `view.rs` is the first non-`NSObject` subclass
            // this crate registers.
            let mut chain = Vec::new();
            // SAFETY: `cls` is live; `superclass_of` tolerates and terminates
            // at nil.
            let mut walk = unsafe { superclass_of(cls) };
            while !walk.is_null() {
                // SAFETY: `walk` is a live class while non-null.
                chain.push(unsafe { class_name(walk) }.to_string_lossy().into_owned());
                // SAFETY: as above.
                walk = unsafe { superclass_of(walk) };
            }
            println!("superclass chain = {}", chain.join(" -> "));

            // THE AUTHORITY LIST, derived here because this is where the chain
            // is already in hand — and derived from `cls`'s SUPERCLASS, never
            // from `cls` itself. See [`derived_authority_protocols`].
            let authorities = derived_authority_protocols(target, cls);
            let (used, registered) = self.methods(target, &authorities, cls);
            self.protocols(target, &authorities, cls, instance, &used);
            Some(registered)
        }

        /// Everything, given the two objects AppKit is holding.
        ///
        /// Part E runs FIRST and on classes of this file's own making: the
        /// premises the fork's verdict rests on are measured before they are
        /// used, not cited from a comment.
        fn run_against(&mut self, delegate: Id, window: Id, view: Id, app: Id, app_delegate: Id) {
            self.teeth_are_live();
            if let Some(registered) = self.table_and_conformance(&DELEGATE, delegate) {
                self.delegate_live_sends(delegate, window, &registered);
                self.unchecked_rows(&DELEGATE, delegate, &registered);
            }
            if let Some(registered) = self.table_and_conformance(&VIEW, view) {
                self.view_live_sends(view, &registered);
                self.unchecked_rows(&VIEW, view, &registered);
            }
            // THE THREE THIS WAVE ADDS. Every instance is one the process this
            // example already builds is holding: `NSApp`, the delegate `NSApp`
            // answers, and the window the audited view is in. Shipping a port
            // whose classes no gate reads is the D1 failure this campaign has
            // repeated once already, and the fix is the same code reading three
            // more objects, not three more copies of this file.
            self.table_and_conformance(&APP_DELEGATE, app_delegate);
            self.table_and_conformance(&WINDOW, window);
            if self.table_and_conformance(&APP, app).is_some() {
                self.patched_rows(&APP, app);
            }
        }

        /// The extra tooth a [`Rows::Patched`] target gets, and the ONLY one
        /// that can bite on it.
        ///
        /// For a class the fork DECLARES, part A's encoding check is the tooth:
        /// the fork wrote the string, an authority elsewhere says what it
        /// should be, and they can disagree. For a class the fork PATCHES, that
        /// check is a TAUTOLOGY and saying so is the point —
        /// `method_setImplementation` replaces the function and leaves the
        /// `types` string exactly as AppKit wrote it, and the target's only
        /// authority is the class itself, so part A is comparing `NSApplication`
        /// with `NSApplication`. It runs anyway (a row that lost its encoding
        /// entirely is still a finding, and part A is where that is reported),
        /// but it is not evidence.
        ///
        /// "TAUTOLOGY" IS TOO WEAK, and the strong form is measured. Part A here
        /// is not merely comparing two strings that happen to match: it reads
        /// both sides off THE SAME `Method` OBJECT — `class_methods` walks
        /// `NSApplication`'s own table for the registered string, and
        /// `authority_classes` is `[NSApplication]`, so `method_types` returns
        /// the same pointer's contents for the authority. And no swizzle in the
        /// runtime can move that string. Measured against a throwaway
        /// `NSApplication` subclass, all three mutators:
        ///
        /// * `method_setImplementation` — what `override_send_event` calls —
        ///   takes no `types` argument at all and leaves the encoding
        ///   byte-identical.
        /// * `class_replaceMethod` with a deliberately lying
        ///   `{_Lie=qqqq}@:B` IGNORES the argument when the class already has
        ///   the row: it returns the previous IMP and the encoding stays `v@:@`.
        /// * `class_addMethod` with the same lie returns 0 and changes nothing.
        ///   It is the only call that can install a `types` string, and only for
        ///   a selector the class does not already have.
        ///
        /// So part A's PART on this target cannot be made to fail by any patch
        /// the fork could apply, and [`Audit::patched_rows`] below is not merely
        /// the better tooth, it is the ONLY one. What part A still catches here
        /// is the framework moving underneath the swizzle — the selector being
        /// renamed away, which `methods` reports as a missing row.
        ///
        /// THE RESIDUAL, on the record rather than assumed away: if a future
        /// AppKit changed `-[NSApplication sendEvent:]`'s OWN signature, part A
        /// would compare `NSApplication` against `NSApplication`, agree, and
        /// pass — while `override_send_event`'s Rust `extern "C" fn` kept the
        /// old shape — `app.rs:16`'s
        /// `extern "C" fn(&NSApplication, Sel, &NSEvent)`. Nothing in this file
        /// can see that, because both sides of the comparison move together. The
        /// instrument that can is `crates/aterm-objc/tests/winit_seam.rs`'s
        /// `no_declared_row_disagrees_with_the_runtimes_own_authority`, whose
        /// `app.rs:185` row carries `v@:@` as a WRITTEN-DOWN transcription and
        /// compares it against `-[NSApplication sendEvent:]`'s live encoding —
        /// two readings that do NOT move together, which is the whole reason
        /// that census still earns its place after the port.
        ///
        /// The evidence is WHOSE CODE the runtime will call. `override_send_event`
        /// is on every keystroke and every device event in the process and it
        /// changes nothing a type encoding can see; if it silently stopped
        /// running — an early return that fires wrongly, a call site deleted, an
        /// `NSApp` that is a different object than the one it patched — the
        /// behaviour lost is Cmd+key `keyUp:` delivery and the whole
        /// `DeviceEvent` stream, and every gate in this tree would still be
        /// green. So the IMP is read off the live class and its IMAGE is asked
        /// of the dynamic loader: the fork's own code is in the running
        /// executable, and AppKit's is in AppKit.
        fn patched_rows(&mut self, target: &Target, instance: Id) {
            let Rows::Patched(ours) = target.rows else {
                return;
            };
            println!("\n=== A2. THE ROWS THIS FORK PATCHED INTO A FRAMEWORK CLASS ===");
            // THE ISA, deliberately, and NOT the `-class` the audit reads a
            // table from: this asks what the runtime will DISPATCH, and
            // dispatch starts at the isa. `NSApp` is KVO-observed in this
            // process, so its isa is `NSKVONotifying_NSApplication` —
            // `class_getInstanceMethod` walks up from there and finds the
            // swizzled row on `NSApplication` itself, which is exactly the
            // answer a real `objc_msgSend` would reach. Reading the reported
            // class instead would still work today and would stop being the
            // true answer the moment anything overrode the row lower down.
            // SAFETY: `instance` is the live object AppKit handed back.
            let cls = unsafe { class_of(instance) };
            for name in ours {
                let sel = sel_named(name);
                // SAFETY: `cls` is live and `sel` is interned; `method_imp`
                // answers `None` for a selector the class does not respond to.
                let Some(imp) = (unsafe { aterm_objc::method_imp(cls, sel) }) else {
                    self.fail(format!(
                        "{}: the class has no implementation of it at all",
                        name
                    ));
                    continue;
                };
                let image = image_of(imp);
                println!(
                    "  -{name} IMP = {imp:p}  in {}",
                    image.as_deref().unwrap_or("?")
                );
                let Some(image) = image else {
                    self.fail(format!(
                        "{name}: the dynamic loader does not know which image its IMP is in, so \
                         this audit cannot tell the fork's code from AppKit's"
                    ));
                    continue;
                };
                if image.contains("/AppKit.framework/") || image.contains("/System/Library/") {
                    self.fail(format!(
                        "{name} on {} still runs {image}'s implementation — \
                         `override_send_event` did not take. Nothing else in this tree can see \
                         that: the swizzle preserves the type encoding, so every encoding check \
                         passes either way, and what is lost is Cmd+key keyUp: delivery and the \
                         entire DeviceEvent stream",
                        target.class_name
                    ));
                } else {
                    println!(
                        "      that image is this process's own executable, not a framework — \
                         the fork's implementation is the one the runtime will call"
                    );
                }
            }
        }
    }

    /// The winit side: make one real window, audit its delegate, leave.
    struct Driver {
        window: Option<Window>,
        audit: Audit,
        done: bool,
    }

    impl ApplicationHandler for Driver {
        fn resumed(&mut self, el: &ActiveEventLoop) {
            if self.window.is_some() {
                return;
            }
            let attrs = Window::default_attributes()
                .with_title("objc-live-class-audit")
                .with_inner_size(winit::dpi::LogicalSize::new(320.0, 200.0))
                .with_visible(false);
            match el.create_window(attrs) {
                Ok(w) => self.window = Some(w),
                Err(e) => {
                    self.audit.blocked = Some(format!("no window could be created: {e}"));
                    self.done = true;
                    el.exit();
                }
            }
        }

        fn window_event(&mut self, _el: &ActiveEventLoop, _id: WindowId, _e: WindowEvent) {}

        fn about_to_wait(&mut self, el: &ActiveEventLoop) {
            if self.done {
                return;
            }
            let Some(window) = self.window.as_ref() else {
                return;
            };
            self.done = true;
            let handle = match window.window_handle() {
                Ok(h) => h.as_raw(),
                Err(e) => {
                    self.audit.blocked = Some(format!("no window handle: {e}"));
                    el.exit();
                    return;
                }
            };
            let RawWindowHandle::AppKit(h) = handle else {
                self.audit.blocked = Some("the window handle is not an AppKit one".to_owned());
                el.exit();
                return;
            };
            let view = Id::from_ptr(h.ns_view.as_ptr());
            // SAFETY: `-[NSView window]` is `@@:`; `view` is the live NSView
            // winit just handed out.
            let ns_window: Id = unsafe {
                let send: unsafe extern "C" fn(Id, Sel) -> Id = msg();
                send(view, sel!(window))
            };
            if ns_window.is_null() {
                self.audit.blocked = Some("the view has no window".to_owned());
                el.exit();
                return;
            }
            // SAFETY: `-[NSWindow delegate]` is `@@:` and `ns_window` is live.
            let delegate: Id = unsafe {
                let send: unsafe extern "C" fn(Id, Sel) -> Id = msg();
                send(ns_window, sel!(delegate))
            };
            if delegate.is_null() {
                self.audit.blocked = Some("the NSWindow has no delegate installed".to_owned());
                el.exit();
                return;
            }
            // NSApp, and the delegate it is holding: the two remaining
            // instances this wave's targets need, both already alive in this
            // process because `EventLoop::new` built them.
            // SAFETY: `+[NSApplication sharedApplication]` is `@#:` and is the
            // documented accessor for the one global application object; it is
            // called on the main thread, which is where this driver runs.
            let app: Id = unsafe {
                let send: unsafe extern "C" fn(Id, Sel) -> Id = msg();
                send(class(c"NSApplication").as_id(), sel!(sharedApplication))
            };
            if app.is_null() {
                self.audit.blocked = Some("there is no shared NSApplication".to_owned());
                el.exit();
                return;
            }
            // SAFETY: `-[NSApplication delegate]` is `@@:` and `app` is live.
            let app_delegate: Id = unsafe {
                let send: unsafe extern "C" fn(Id, Sel) -> Id = msg();
                send(app, sel!(delegate))
            };
            if app_delegate.is_null() {
                self.audit.blocked = Some(
                    "NSApp has no delegate installed, so the application \
                          delegate cannot be audited"
                        .to_owned(),
                );
                el.exit();
                return;
            }
            self.audit
                .run_against(delegate, ns_window, view, app, app_delegate);
            el.exit();
        }
    }

    /// Drive the loop until the audit has run, then report.
    pub fn run() -> i32 {
        let mut el = match EventLoop::new() {
            Ok(el) => el,
            Err(e) => {
                eprintln!("objc-live-class-audit: NOT RUN — no event loop: {e}");
                return NOT_RUN;
            }
        };
        let mut driver = Driver {
            window: None,
            audit: Audit::default(),
            done: false,
        };
        let started = Instant::now();
        loop {
            if let PumpStatus::Exit(_) =
                el.pump_app_events(Some(Duration::from_millis(4)), &mut driver)
            {
                break;
            }
            if driver.done {
                break;
            }
            if started.elapsed() > BUDGET {
                eprintln!("objc-live-class-audit: NOT RUN — no window arrived within {BUDGET:?}");
                return NOT_RUN;
            }
        }
        // The window must die before the delegate is described as surviving
        // anything; dropping it here also runs the ported `-dealloc`.
        drop(driver.window.take());

        if let Some(why) = driver.audit.blocked {
            eprintln!("objc-live-class-audit: NOT RUN — {why}");
            return NOT_RUN;
        }
        println!("\n=== VERDICT ===");
        if driver.audit.findings.is_empty() {
            println!("objc-live-class-audit: OK — every registered row agrees with the runtime.");
            PASS
        } else {
            for f in &driver.audit.findings {
                println!("  FAIL: {f}");
            }
            println!(
                "objc-live-class-audit: {} FINDING(S)",
                driver.audit.findings.len()
            );
            FAIL
        }
    }
}
