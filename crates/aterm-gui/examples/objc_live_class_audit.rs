// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The live-class auditor: what `crates/aterm-window`'s macOS backend actually
//! registered (its four declared classes and the one row it swizzles into
//! `NSApplication`), asked of the running Objective-C runtime and answered by the
//! runtime's own authority.
//!
//! An encoding census over a mirror class the test declares itself cannot see a
//! row retyped in the fork (`windowDidResize:`
//! registering `v@:B` where `NSWindowDelegate` says `v@:@`) or a protocol dropped
//! from a `protocols:` list; both leave the build and every unit test green. This
//! reads the live classes AppKit holds on a real `NSWindow`.
//!
//! # Method
//!
//! It never says what a class should contain. For each target it reads the whole
//! registered table ([`aterm_objc::class_methods`]) and looks up each row's
//! authority: the protocols [`derived_authority_protocols`] reads off the runtime,
//! then the superclass chain (where an override's original lives), then a sibling
//! [`Family`]. A disagreeing encoding is a finding; so is a row with no authority,
//! and the finding names any loaded protocol that does declare it. The only things
//! written down are each [`Target`]'s `class_name`, its `claimed` list (the fork's
//! `protocols:` line, the one mirror) and its short `informal` list.
//!
//! | target | instance | rows |
//! |---|---|---|
//! | [`DELEGATE`] `WinitWindowDelegate` | `-[[view window] delegate]` | 21 + `dealloc` |
//! | [`VIEW`] `WinitView` | the `NSView` winit hands out | 39 + `dealloc` |
//! | [`APP_DELEGATE`] `WinitApplicationDelegate` | `-[NSApp delegate]` | 3 + `dealloc` |
//! | [`WINDOW`] `WinitWindow` | `-[view window]` | 2 + `dealloc` |
//! | [`APP`] `NSApplication` | `NSApp` | 1 swizzled row, see [`Rows::Patched`] |
//!
//! The authority list starts at the superclass, never the audited class's own
//! protocol list: a class that stops claiming `NSWindowDelegate` must not delete
//! the authority its window rows are checked against. Conformance is then checked
//! twice, on the instance through `-conformsToProtocol:` (the question AppKit
//! asks): every protocol that supplied an authority must be conformed to (unless
//! informal on this chain, like `NSStandardKeyBindingResponding`), and every
//! `claimed` name must be claimed.
//!
//! # Parts
//!
//! * A: the registered table, each row against its authority.
//! * B: protocol conformance, derived and declared.
//! * C: live sends through the return and argument shapes the port changed.
//! * D: the rows nothing declares (`frameDidChange:`), held to the encoding
//!   `aterm_objc::method_encoding!` generates from the fork's Rust signature, and
//!   read back through `NSMethodSignature`.
//! * E: the instrument's own premises, measured on control classes first.
//!   `-methodSignatureForSelector:` answers from a conformed-to protocol's
//!   declaration, not the registered string, so for `WinitView`'s
//!   `NSTextInputClient` rows only part A's string comparison can catch a lie; for
//!   an undeclared row it reads our string, and an `NSInvocation` round trip
//!   catches a lying encoding.
//! * F: the written [`HostLacks`] shapes, held to the protocol wherever this host
//!   registers it.
//!
//! Not checked here: cardinality (a deleted method leaves a smaller, correct
//! table), and behaviour
//! beyond part C (`objc_ime_drive.rs`).
//!
//! An example, not a `#[test]`: libtest runs every test off the main thread,
//! where `EventLoop` construction panics, so only a target owning `fn main` can
//! create the `NSWindow` (as for `src/bin/aterm-redraw-conformance.rs`).
//!
//! # Exit codes (read by `aterm_verify::stages::OBJC_DRIVES`)
//!
//! * `0`: every row agreed with its runtime authority.
//! * `1`: a finding: a disagreeing encoding, a row with no authority, a protocol
//!   implemented but not conformed to, or a live send with the wrong answer.
//! * `2`: not run: no event loop, window or delegate, or a row whose protocol this
//!   host's AppKit does not register and whose shape the fork did not write down.
//!   Never a pass.
//! * `3`: every row agreed, and some were checked against the fork's own
//!   declaration because this host's AppKit does not register their protocol.

/// Every row agreed with the runtime's own authority.
#[cfg(target_os = "macos")]
const PASS: i32 = 0;
/// At least one finding. See the transcript.
#[cfg(target_os = "macos")]
const FAIL: i32 = 1;
/// The audit could not execute here. Not a pass.
const NOT_RUN: i32 = 2;
/// Every row agreed, and at least one was checked against this fork's own
/// declaration because this host's AppKit does not register its protocol
/// ([`HostLacks`]). A separate code so the pass line `verify appkit` prints does
/// not claim runtime authority nobody had; its label lives in this driver's
/// `OBJC_DRIVES` row (`crates/aterm-verify/src/stages.rs`).
#[cfg(target_os = "macos")]
const FORK_DECLARED: i32 = 3;

#[cfg(not(target_os = "macos"))]
fn main() -> std::process::ExitCode {
    eprintln!(
        "objc-live-class-audit: NOT RUN — this audit is about \
         crates/aterm-window/src/platform_impl/macos/window_delegate.rs, which does not \
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
        Bool, Id, Sel, checked_msg, class, class_methods, class_name, class_of, class_protocols,
        method_types, protocol, protocol_method_types, sel, strip_method_offsets, superclass_of,
    };
    use winit::application::ApplicationHandler;
    use winit::event::WindowEvent;
    use winit::event_loop::ActiveEventLoop;
    use winit::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};
    use winit::window::{Window, WindowId};

    use super::{FAIL, FORK_DECLARED, NOT_RUN, PASS};

    /// One class this audit is responsible for.
    struct Target {
        /// The class name `declare_class!` registers: the one thing this file must
        /// know; everything else is read from the runtime.
        class_name: &'static str,
        /// The `protocols:` list of the fork's `declare_class!` site: the declared
        /// backstop, names not signatures. Also source 1 of the derived authority
        /// list, which reads the superclass chain's claims, never the audited
        /// class's own, so deleting a name from the fork's list cannot delete the
        /// authority its rows are checked against.
        claimed: &'static [&'static CStr],
        /// Consulted after every protocol: the superclass chain, where the
        /// overrides live (`-dealloc`, and every `NSResponder` row `view.rs`
        /// overrides).
        authority_classes: &'static [&'static CStr],
        /// Selectors nothing in the runtime declares, with the shape part D holds
        /// each to (generated from the fork's Rust signature).
        unchecked: &'static [Unchecked],
        /// Rows whose authority is a protocol this host may not register, with the
        /// fork's shape written down ([`HostLacks`]). Where the host has the
        /// protocol, part F holds the written shape to it.
        host_lacks: &'static [HostLacks],
        /// Rows whose authority is derived from declared siblings ([`Family`]),
        /// consulted after the protocols and the superclass chain.
        families: &'static [Family],
        /// Authority protocols informal on this target's chain: they declare the
        /// signatures the encoding check uses, nothing the target inherits from
        /// conforms to them, and AppKit dispatches their rows by
        /// `-respondsToSelector:`. Exempt from the derived-conformance check, with
        /// the reason; [`Audit::protocols`] re-asks the instance every run.
        ///
        /// Also source 2 of the derived authority list, since nothing claims
        /// them. Keep it short: each name is a protocol whose disappearance from a
        /// `protocols:` list this audit would no longer notice.
        informal: &'static [(&'static str, &'static str)],
        /// Which of the class's registered rows are the fork's ([`Rows`]).
        rows: Rows,
    }

    /// Whose method table a [`Target`] is reading. For a class the fork declares,
    /// all of it. `NSApplication` is a target because `app.rs` swizzles one of its
    /// rows; auditing Apple's whole table would be auditing AppKit against itself.
    #[derive(Clone, Copy)]
    enum Rows {
        /// A class the fork declares: every registered row is audited.
        Declared,
        /// A framework class the fork patched: only these selectors are audited,
        /// each must be present, and each must run the fork's own code
        /// ([`Audit::patched_rows`]).
        Patched(&'static [&'static str]),
    }

    /// `window_delegate.rs`'s class. 21 declared rows plus `-dealloc`, all resolved
    /// by the runtime.
    const DELEGATE: Target = Target {
        class_name: "WinitWindowDelegate",
        claimed: &[c"NSObject", c"NSWindowDelegate", c"NSDraggingDestination"],
        // `NSObject` the class is where `-dealloc` and KVO's
        // `observeValueForKeyPath:ofObject:change:context:` live.
        authority_classes: &[c"NSObject"],
        unchecked: &[],
        host_lacks: &[],
        families: &[],
        // Empty: `NSWindowDelegate` is formal and `NSObject` does not conform, so
        // a class that implements its rows and stops claiming it is caught.
        informal: &[],
        rows: Rows::Declared,
    };

    /// `view.rs`'s class. 39 declared rows plus `-dealloc`. It claims only
    /// `NSTextInputClient`, as the objc2 class did.
    const VIEW: Target = Target {
        class_name: "WinitView",
        // One name; the derivation supplies the rest. `NSStandardKeyBindingResponding`
        // (`insertTab:`, `cancelOperation:`) arrives through `informal`, since
        // nothing on this chain claims it. `NSTouchBarProvider` arrives from the
        // chain (`NSResponder` claims it, and `NSWindow` claims
        // `NSTouchBarProviderContainer`, which inherits it); it declares `touchBar`
        // `@16@0:8`, as `NSResponder` implements it, and protocols are consulted
        // first, so the row reports `proto …`.
        claimed: &[c"NSTextInputClient"],
        // The whole chain, in override order:
        // `WinitView -> NSView -> NSResponder -> NSObject`.
        authority_classes: &[c"NSView", c"NSResponder", c"NSObject"],
        unchecked: VIEW_UNCHECKED,
        host_lacks: &[],
        families: VIEW_FAMILIES,
        informal: VIEW_INFORMAL,
        rows: Rows::Declared,
    };

    /// `app_state.rs`'s class: the `NSApplicationDelegate` on `NSApp`. Three
    /// declared rows plus `-dealloc`, all `@optional`, so reading the registered
    /// table matters most here: AppKit reaches an `@optional` row only after
    /// `-respondsToSelector:`, so one that never registered fails silently (no
    /// activation policy, no clean termination) and no send would notice.
    ///
    /// `NSApplicationDelegate` is the protocol a host may not register (see
    /// [`HostLacks`]), so the three rows carry the fork's own shapes, transcribed
    /// from `crates/aterm-window/src/platform_impl/macos/app_state.rs` and
    /// generated, never typed. `applicationShouldTerminate:` returns `usize`, not
    /// `Bool`: `NSApplicationTerminateReply` is eight bytes (`Q24@0:8@16` where the
    /// host has the protocol).
    const APP_DELEGATE_HOST_LACKS: &[HostLacks] = &[
        HostLacks {
            sel: "applicationDidFinishLaunching:",
            protocol: c"NSApplicationDelegate",
            rust: "fn app_did_finish_launching(&self, _notification: Id)",
            expected: || aterm_objc::method_encoding!(() ; Id),
            why: "an @optional row of NSApplicationDelegate, which macOS 14.4.1 \
                  and macOS 13.7.8 do not register (macOS 26 does); \
                  aterm_objc::protocol_or_register supplies a name-only stand-in \
                  there and a stand-in declares nothing",
        },
        HostLacks {
            sel: "applicationWillTerminate:",
            protocol: c"NSApplicationDelegate",
            rust: "fn app_will_terminate(&self, _notification: Id)",
            expected: || aterm_objc::method_encoding!(() ; Id),
            why: "the same protocol, the same @optional table, the same \
                  stand-in on a host that lacks it",
        },
        HostLacks {
            sel: "applicationShouldTerminate:",
            protocol: c"NSApplicationDelegate",
            rust: "fn app_should_terminate(&self, _sender: Id) -> usize",
            expected: || aterm_objc::method_encoding!(usize ; Id),
            why: "the same protocol and stand-in; the RETURN is the part worth \
                  writing down — NSApplicationTerminateReply is eight bytes, \
                  which app_state.rs measured as Q24@0:8@16 from a host that \
                  registers the protocol, so a Bool here would be a defect",
        },
    ];

    const APP_DELEGATE: Target = Target {
        class_name: "WinitApplicationDelegate",
        claimed: &[c"NSObject", c"NSApplicationDelegate"],
        // `NSObject` the class is where `-dealloc` lives.
        authority_classes: &[c"NSObject"],
        unchecked: &[],
        host_lacks: APP_DELEGATE_HOST_LACKS,
        families: &[],
        // Empty: `NSApplicationDelegate` is formal and `NSObject` does not conform,
        // so a class that stops claiming it is caught by the derived check.
        informal: &[],
        rows: Rows::Declared,
    };

    /// `window.rs`'s class: the `NSWindow` every winit window is. Two declared rows
    /// plus `-dealloc`, no ivars and no protocols, so `claimed` is empty. Its
    /// authority list still comes from the chain (`NSWindow` claims seventeen
    /// protocols, `NSGestureRecognizerDelegate` among them); neither row is declared
    /// by any of them, so both resolve against `NSWindow` the class.
    const WINDOW: Target = Target {
        class_name: "WinitWindow",
        claimed: &[],
        // The whole chain, in override order:
        // `WinitWindow -> NSWindow -> NSResponder -> NSObject`.
        authority_classes: &[c"NSWindow", c"NSResponder", c"NSObject"],
        unchecked: &[],
        host_lacks: &[],
        families: &[],
        informal: &[],
        rows: Rows::Declared,
    };

    /// `app.rs`'s one row, the only target that is not a class this fork declares.
    ///
    /// `override_send_event` replaces `-[NSApplication sendEvent:]`'s IMP through
    /// `method_setImplementation`. It is on every keystroke path (it makes Cmd+key
    /// deliver `keyUp:`) and leaves the type encoding untouched, so the only live
    /// evidence is the address of the function the runtime will call, which
    /// [`Audit::patched_rows`] reads. `app.rs` declares no methods, and this fork
    /// installs no `NSPrincipalClass`.
    const APP: Target = Target {
        class_name: "NSApplication",
        claimed: &[],
        authority_classes: &[c"NSApplication"],
        unchecked: &[],
        host_lacks: &[],
        families: &[],
        informal: &[],
        rows: Rows::Patched(&["sendEvent:"]),
    };

    /// One informal protocol. AppKit declares `NSStandardKeyBindingResponding` so
    /// the signatures exist, and dispatches `insertTab:`/`cancelOperation:` by
    /// `-respondsToSelector:`; nothing on `WinitView`'s chain conforms to it (of the
    /// 26,318 classes loaded with AppKit only `NSTextView` claims it). The
    /// exemption is about this chain, re-asked of the instance every run, so it
    /// goes red if `NSView` or `NSResponder` ever conforms.
    ///
    /// `NSTouchBarProvider` is not here: it is formal, `NSResponder` claims it,
    /// and `NSView` conforms by inheritance (`class_conformsToProtocol(NSView, …)`
    /// is false, `-conformsToProtocol:` true), so `touchBar` is checked with the
    /// conformance check armed.
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

    /// A row nothing in the runtime declares, and the shape part D holds it to
    /// anyway.
    struct Unchecked {
        /// The selector, as the fork's `@sel(…)` spells it.
        sel: &'static str,
        /// Why no protocol and no class on the chain declares it. Measured: part A
        /// reaches this list only after asking every authority the target has.
        why: &'static str,
        /// The fork's Rust signature for this row, as source text.
        rust: &'static str,
        /// The encoding [`Self::rust`] produces, built by
        /// `aterm_objc::method_encoding!` over the same types: a `fn` because
        /// `BOOL` is `B` on arm64 and `c` on x86_64, so a literal would be wrong on
        /// one slice.
        expected: fn() -> String,
    }

    /// A row whose authority is a protocol this host may not register, with the
    /// fork's declared shape written down so the row is still checked where the
    /// protocol is absent, and held to the protocol where it is present.
    ///
    /// `NSApplicationDelegate` is that protocol: macOS 13.7.8 and 14.4.1 do not
    /// register it, macOS 26 does. `aterm_objc::protocol_or_register` supplies a
    /// name-only stand-in so the class can claim it, and a stand-in declares
    /// nothing; without this the three `WinitApplicationDelegate` rows, whose
    /// failure mode is silence, would have no authority there and the audit could
    /// never pass on such a host.
    ///
    /// Where the host registers the protocol, it is the authority as usual and
    /// [`Audit::host_lacks_pins`] holds this written shape to it. Where it does
    /// not, the row is checked against [`Self::expected`] and printed as such. The
    /// shape is generated by `aterm_objc::method_encoding!`, as for
    /// [`Unchecked::expected`]. A retyped argument changes the registered string but
    /// not this one, and a dropped protocol claim is refused by `host_lacks_is_active`
    /// and part F.
    struct HostLacks {
        /// The selector, as the fork's `@sel(…)` spells it.
        sel: &'static str,
        /// The protocol that declares it on a host that has one; must be one of
        /// the target's `claimed` names (checked).
        protocol: &'static CStr,
        /// The fork's Rust signature for this row, as source text, so the
        /// expectation below can be read against the thing it describes.
        rust: &'static str,
        /// The encoding [`Self::rust`] produces, built by
        /// `aterm_objc::method_encoding!` over those same types.
        expected: fn() -> String,
        /// Why a host may lack the protocol, and where that was measured.
        why: &'static str,
    }

    /// A row nothing declares whose shape is derived from siblings that something
    /// does. `insertTab:`, `insertBackTab:` and `cancelOperation:` are one
    /// key-binding family, reached the same way with the same argument;
    /// `NSStandardKeyBindingResponding` declares the first and last but not
    /// `insertBackTab:`. Only the membership is written down: the encoding comes
    /// from the runtime, and the siblings must agree before it is used.
    struct Family {
        /// The row with no declaration of its own.
        sel: &'static str,
        /// Where the siblings are declared.
        protocol: &'static CStr,
        /// The siblings, which must all agree: two or more, or agreement is vacuous.
        siblings: &'static [&'static str],
        /// Why these siblings are the right authority for that row.
        why: &'static str,
    }

    /// One family: the key-binding actions.
    const VIEW_FAMILIES: &[Family] = &[Family {
        sel: "insertBackTab:",
        protocol: c"NSStandardKeyBindingResponding",
        siblings: &["insertTab:", "cancelOperation:"],
        // The derivation needs no class on this view's chain to implement any of
        // the three (so there is no override to inherit from) and every existing
        // implementation to agree: all ten register `v24@0:8@16`, the shape the
        // protocol declares for the two it has.
        why: "AppKit turns a key equivalent into one of these three selectors \
              and sends it to the first responder the same way for all three; \
              NSStandardKeyBindingResponding declares the other two and not \
              this one. NOTHING ON THIS VIEW'S CHAIN implements any of the \
              three (NSView, NSResponder, NSObject — measured), so there is no \
              override to inherit a signature from; elsewhere in the runtime \
              insertTab: has 3 implementations and cancelOperation: 7, every \
              one of them v@:@, while insertBackTab: has 0 (measured over the \
              26,318 classes loaded with AppKit)",
    }];

    /// The one row no protocol, class or sibling family can speak for.
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

    /// Foundation's `NSNotFound` (`NSIntegerMax`), which `NSTextInputClient`
    /// documents `markedRange` to answer when there is none.
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
        /// it (`-dealloc`, and every `NSResponder` row `view.rs` declares). The
        /// name is the class asked, which `class_getInstanceMethod` answers for
        /// by walking up; [`Source::label`] also prints the defining class.
        Class(&'static CStr),
        /// Nothing declares this row, but a protocol declares its agreeing
        /// siblings ([`Family`]). The name is that protocol's.
        Siblings(&'static CStr),
        /// Nothing in the runtime declares it.
        Nowhere,
    }

    impl Source {
        /// `sel` is the row this source was found for: [`Self::Class`] prints the
        /// class it asked and, when they differ, the class that defines the method.
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

    /// The first class at or above `cls` whose own method table holds `sel`.
    /// `class_getInstanceMethod` walks the chain without saying where the method
    /// came from; `class_methods` reports only a class's own rows, so this can.
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

    /// Intern a selector whose name is known only as a `&str`. `sel_uncached`
    /// takes a `&'static CStr`, so each name leaks one small allocation, in a
    /// process that audits one window and exits.
    fn sel_named(name: &str) -> Sel {
        let owned: &'static CStr = Box::leak(
            std::ffi::CString::new(name)
                .expect("a selector name has no interior NUL")
                .into_boxed_c_str(),
        );
        aterm_objc::sel_uncached(owned)
    }

    /// Intern a class or protocol name known only as a `&str` (the derived
    /// authority list's names come out of the runtime), leaking it as
    /// [`sel_named`] does.
    fn cstr_named(name: &str) -> &'static CStr {
        Box::leak(
            std::ffi::CString::new(name)
                .expect("a runtime-supplied name has no interior NUL")
                .into_boxed_c_str(),
        )
    }

    /// Every protocol reachable from `seed` through protocol inheritance, in seed
    /// order without duplicates. `NSWindow` claims `NSTouchBarProviderContainer`,
    /// which inherits `NSTouchBarProvider`, where `touchBar` is declared.
    fn expand_protocols(into: &mut Vec<String>, seed: impl IntoIterator<Item = String>) {
        for name in seed {
            // Depth-first from one seed at a time, so the caller's order is the
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

    /// The protocols a [`Target`]'s rows are checked against, read off the runtime.
    ///
    /// Three sources, in order, each expanded through protocol inheritance: the
    /// fork's own `protocols:` line (`claimed`), the `informal` names (which
    /// nothing claims), and every protocol the superclass chain claims. Starting
    /// at the superclass is the design: reading `cls`'s own list would let a class
    /// that stops claiming `NSWindowDelegate` delete the authority its rows are
    /// checked against.
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

    /// Whether a [`HostLacks`] entry is the authority for its row on this host:
    /// the target still claims the protocol (else dropping the name from the fork's
    /// `protocols:` list would pass), and the object under that name is this
    /// process's own stand-in, not the framework's (a nil check on
    /// `objc_getProtocol` would read a stand-in as AppKit).
    fn host_lacks_is_active(target: &Target, pin: &HostLacks) -> bool {
        target.claimed.contains(&pin.protocol)
            && aterm_objc::protocols_registered_by_aterm().contains(&pin.protocol)
    }

    /// Every protocol loaded in this process that declares `sel`, with its
    /// encoding: what turns "nothing declares it" into a measurement.
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

    /// How many protocols are loaded, so a finding states the width of its search.
    fn loaded_protocol_count() -> usize {
        loaded_protocols().len()
    }

    /// The runtime's whole protocol table (declared here, as for
    /// [`protocol_parents`]).
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

    /// The protocols a protocol itself inherits. `aterm-objc` grows only for the
    /// port, and no shipping call site needs `protocol_copyProtocolList`, so it is
    /// declared here.
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

    /// Which loaded image a code address belongs to, per the dynamic loader: the
    /// one answer to "is this function ours or AppKit's?". `dladdr` is in
    /// `libSystem`, so it is declared here and the audit adds no dependency.
    fn image_of(imp: *const std::ffi::c_void) -> Option<String> {
        /// `<dlfcn.h>`'s `Dl_info`: four pointer-sized fields, the first the image
        /// path.
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
        // SAFETY: `imp` is a code address the runtime handed back and is never
        // dereferenced; `dladdr` reads the loader's tables and fills `info`,
        // answering 0 for an address in no loaded image.
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

    /// One protocol's declaration of an instance method named `name`.
    /// `protocol_method_types` consults the required table and then the optional
    /// one, which matters: every `NSWindowDelegate` and
    /// `NSStandardKeyBindingResponding` row here is `@optional`.
    fn protocol_declares(p: &'static CStr, name: &str) -> Option<String> {
        // SAFETY: `protocol` answers a live protocol object or nil (nil for a
        // framework that is not loaded, which `protocol_method_types`
        // tolerates), and `sel_named` interns the selector.
        let types = unsafe { protocol_method_types(protocol(p), sel_named(name), true) };
        types.map(|t| strip_method_offsets(&t))
    }

    /// Split a method type encoding into its return type and its arguments,
    /// balancing braces (`{CGRect={CGPoint=dd}{CGSize=dd}}@:{_NSRange=QQ}^{_NSRange=QQ}`
    /// is four types). `None` for a string that does not parse.
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

    /// The rectangle part E's rect rows return: four distinct non-zero doubles, so
    /// neither a misread frame nor a zeroed one can produce it.
    const PROBE_RECT: aterm_objc::CGRect = aterm_objc::CGRect {
        origin: aterm_objc::CGPoint { x: 704.0, y: 839.0 },
        size: aterm_objc::CGSize {
            width: 11.0,
            height: 22.0,
        },
    };

    /// The IMP behind part E's rect rows, returning a `CGRect`: an HFA, which
    /// arm64 returns in `d0`-`d3`, never through `x8`.
    extern "C" fn probe_rect(_this: Id, _cmd: Sel) -> aterm_objc::CGRect {
        PROBE_RECT
    }

    /// The lying encoding: a 32-byte struct of four `long long`s, the size of an
    /// `NSRect` but not an HFA, so arm64 returns it through `x8` while the IMP
    /// answers in `d0`-`d3`.
    const PROBE_LIE: &str = "{_ATermAuditLie=qqqq}@:";

    /// The register-class probe: a `CGPoint`, which both ABIs return in float
    /// registers (`xmm0`/`xmm1`, `d0`/`d1`), never through a hidden pointer.
    const PROBE_POINT: aterm_objc::CGPoint = aterm_objc::CGPoint { x: 12.5, y: -3.25 };

    /// The IMP behind part E's point rows, returning [`PROBE_POINT`] in registers,
    /// so a lying signature changes only which registers NSInvocation reads.
    extern "C" fn probe_point(_this: Id, _cmd: Sel) -> aterm_objc::CGPoint {
        PROBE_POINT
    }

    /// The register-class lie: a 16-byte struct of two `long long`s, the size of a
    /// `CGPoint` but integer class (`rax`:`rdx`, `x0`:`x1`) while the IMP answers in
    /// float registers. x86_64 returns every 32-byte struct through a hidden
    /// pointer, so [`PROBE_LIE`] cannot be told from the honest rect there, but
    /// register class it does distinguish: on macOS 13.7.8 x86_64, NSInvocation
    /// read `{12.5, -3.25}` back as `{-3.25, 0.0}`. On arm64 the same disagreement
    /// is the ABI's expectation (the lie is read from `x0`:`x1`, still holding
    /// `self` and `_cmd`), not yet observed on a run.
    const PROBE_LIE_POINT: &str = "{_ATermAuditLie=qq}@:";

    /// The audit's state: the findings it exits on and the host-limited rows.
    #[derive(Default)]
    struct Audit {
        findings: Vec<String>,
        /// Why the audit could not run at all, if it could not.
        blocked: Option<String>,
        /// Rows that could not be checked on this host: their protocol is one its
        /// AppKit does not register and the fork wrote down no shape. Not findings
        /// and not passes: any of these makes the verdict NOT RUN.
        host_unchecked: Vec<String>,
        /// Rows checked against this fork's own declaration rather than the
        /// runtime, because this host's AppKit does not register their protocol
        /// ([`HostLacks`]). Any of these makes the verdict [`FORK_DECLARED`].
        fork_declared: Vec<String>,
    }

    impl Audit {
        fn fail(&mut self, what: String) {
            println!("  FINDING: {what}");
            self.findings.push(what);
        }

        /// Find `sel`'s authority by asking the runtime: protocols first, then the
        /// superclass chain, then the [`Target::families`] derivation, whose
        /// siblings must still be declared and agree.
        ///
        /// Every protocol hit is collected and they must agree: a derived list can
        /// hold two declarations of one row (`touchBar` in `NSTouchBarProvider` and
        /// `NSTouchBarProviderContainer`, `doCommandBySelector:` in
        /// `NSTextInputClient` and `NSStandardKeyBindingResponding`), and taking the
        /// first of two that disagree would be picking an answer.
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
            // The row itself must still be undeclared; if AppKit ever declares it,
            // the derivation is stale.
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

        /// Part A: the whole registered table, each row against its authority.
        ///
        /// Returns the protocols that supplied an authority (for part B's derived
        /// check) and every registered row with its encoding (for parts C and D).
        fn methods(
            &mut self,
            target: &Target,
            authorities: &[&'static CStr],
            cls: aterm_objc::ClassPtr,
        ) -> (BTreeSet<String>, BTreeMap<String, String>) {
            // SAFETY: `cls` is the live class of an object AppKit handed back.
            let mut rows = unsafe { class_methods(cls) };
            rows.sort_by_key(|(s, _)| s.name().to_string_lossy().into_owned());
            let registered_count = rows.len();
            if let Rows::Patched(ours) = target.rows {
                // A framework class: audit only the rows the fork installed. A
                // missing one means the selector was renamed out from under the
                // swizzle.
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
                // A row whose protocol this host does not register and whose shape
                // the fork wrote down: checked below against that, and in part F
                // on a host that has the protocol.
                let covered_by_part_f = target
                    .host_lacks
                    .iter()
                    .any(|h| h.sel == name.as_str() && host_lacks_is_active(target, h));
                let verdict = match &authority {
                    Some(a) if *a == registered => "ok ",
                    Some(_) => "BAD",
                    // `D` and `F` mark rows with no authority that parts D and F
                    // still hold to a shape; `??` is truly unknown.
                    None if covered_by_part_d => "D  ",
                    None if covered_by_part_f => "F  ",
                    None => "?? ",
                };
                println!(
                    "  {verdict} {name:<52} {registered:<16} {:<16} {}",
                    authority.as_deref().unwrap_or("-"),
                    if covered_by_part_f {
                        "fork declaration".to_owned()
                    } else {
                        source.label(selector)
                    }
                );
                if matches!(source, Source::Siblings(_))
                    && let Some(family) = target.families.iter().find(|f| f.sel == name.as_str())
                {
                    // The derivation, printed where the row is judged.
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
                            // Part D holds this row to a shape; this prints why
                            // the authority lookup came back empty.
                            println!("      no authority: {}", row.why);
                            println!("      checked in part D against {}", row.rust);
                        } else {
                            // The claimed protocols this host's AppKit does not
                            // register (aterm's name-only stand-ins declare
                            // nothing). Consulted only after the search below
                            // comes back empty.
                            let supplied: Vec<String> = aterm_objc::protocols_registered_by_aterm()
                                .iter()
                                .filter(|p| target.claimed.contains(p))
                                .map(|p| p.to_string_lossy().into_owned())
                                .collect();
                            // Search every loaded protocol before saying nothing
                            // declares the row, so the finding names the
                            // one-line fix when one exists.
                            let declared = protocols_declaring(selector);
                            if !declared.is_empty() {
                                // All of them: `applicationShouldTerminate:` is
                                // declared by `NSApplicationDelegate` and by a
                                // private `NSApplicationTestingDelegate`.
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
                            } else if let Some(pin) = target
                                .host_lacks
                                .iter()
                                .find(|h| h.sel == name.as_str())
                                .filter(|h| {
                                    supplied.iter().any(|s| {
                                        s.as_str() == h.protocol.to_string_lossy().as_ref()
                                    })
                                })
                            {
                                // Nothing loaded declares it, the target claims a
                                // protocol this host does not register, and the fork
                                // wrote down the row's shape: checked against that,
                                // said so, and held to the protocol by part F on
                                // every host that has it.
                                let expected = (pin.expected)();
                                println!(
                                    "      CHECKED AGAINST THE FORK'S OWN DECLARATION, not this \
                                     host's runtime: its AppKit does not register {} (aterm \
                                     supplied a name-only stand-in) and none of the {} protocols \
                                     loaded here declares this row",
                                    supplied.join(", "),
                                    loaded_protocol_count()
                                );
                                println!("      expected  {expected}   from `{}`", pin.rust);
                                println!("      {}", pin.why);
                                self.fork_declared.push(format!(
                                    "{}: {name} — checked against `{}` ({expected}), not against \
                                     {} which this host does not register",
                                    target.class_name,
                                    pin.rust,
                                    supplied.join(", ")
                                ));
                                if registered != expected {
                                    self.fail(format!(
                                        "{name}: registered {registered} but the fork declares \
                                         `{}`, which encodes {expected}. This host does not \
                                         register {} so the runtime cannot arbitrate, and the \
                                         registered shape disagrees with the source it was \
                                         generated from",
                                        pin.rust,
                                        supplied.join(", ")
                                    ));
                                }
                            } else if !supplied.is_empty() {
                                // Nothing loaded declares it and the target claims
                                // a protocol this host does not register: not
                                // checked here, counted, and the verdict is not a pass.
                                println!(
                                    "      NOT CHECKED on this host: its AppKit does not register {} \
                                     (aterm supplied a name-only stand-in), and none of the {} \
                                     protocols loaded here declares this row",
                                    supplied.join(", "),
                                    loaded_protocol_count()
                                );
                                self.host_unchecked.push(format!(
                                    "{}: {name} — {} is not registered by this host's AppKit",
                                    target.class_name,
                                    supplied.join(", ")
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

        /// Part B: the conformance claim, derived and declared, asked of the
        /// instance.
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

            // The derived check: the instance must conform (`-conformsToProtocol:`
            // walks the superclass chain) to every protocol that supplied a row's
            // authority. Whether AppKit asks depends on the protocol: application
            // delegate rows are reached by `-respondsToSelector:`, but
            // `-[NSView _inputContext]` creates a context only for a view conforming
            // to `NSTextInputClient` and then sends its required rows. Where the
            // conformance comes from (own, inherited, or exempt as informal) is
            // printed.
            for p in authorities {
                let name = p.to_string_lossy().into_owned();
                if !used.contains(&name) {
                    continue;
                }
                // SAFETY: `-conformsToProtocol:` is `B@:@` on every Apple
                // runtime; `instance` is live and `protocol` answers a live
                // protocol or nil (for which the runtime answers NO).
                let conforms: Bool = unsafe {
                    let send: unsafe extern "C-unwind" fn(
                        Id,
                        Sel,
                        aterm_objc::ProtocolPtr,
                    ) -> Bool = checked_msg();
                    send(instance, sel!(conformsToProtocol:), protocol(p))
                };
                let informal = target.informal.iter().find(|(n, _)| *n == name.as_str());
                let supplied_by_aterm = aterm_objc::protocols_registered_by_aterm()
                    .iter()
                    .any(|s| s.to_string_lossy() == name);
                let how = if claimed.contains(&name) && supplied_by_aterm {
                    "claimed by this class — against a name-only protocol aterm supplied, this \
                     host's AppKit not registering it"
                } else if claimed.contains(&name) {
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
                        // Something now conforms: the exemption is stale and
                        // disarms a check that would pass.
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
                // runtime; `instance` is live and `protocol` answers a live
                // protocol or nil (for which the runtime answers NO).
                let conforms: Bool = unsafe {
                    let send: unsafe extern "C-unwind" fn(
                        Id,
                        Sel,
                        aterm_objc::ProtocolPtr,
                    ) -> Bool = checked_msg();
                    send(instance, sel!(conformsToProtocol:), protocol(p))
                };
                println!("  -conformsToProtocol:{name} = {}", conforms.as_bool());
                if !conforms.as_bool() {
                    self.fail(format!("the INSTANCE does not conform to {name}"));
                }
            }
        }

        /// Part C for the window delegate: a live send through the `BOOL` return
        /// shape. The delegate's `NSUInteger` row, `draggingEntered:`, is sent by
        /// `objc_window_drive`'s drag-and-drop stage.
        fn delegate_live_sends(
            &mut self,
            delegate: Id,
            window: Id,
            registered: &BTreeMap<String, String>,
        ) {
            println!("\n=== C. LIVE SENDS THROUGH THE REGISTERED IMPS ===");
            // An unregistered selector would crash the process in
            // `-doesNotRecognizeSelector:` (no exit status, a useless diagnosis);
            // report the absence instead.
            let want = "windowShouldClose:";
            if !registered.contains_key(want) {
                self.fail(format!(
                    "{want} is not in the registered table, so the live send that would \
                     exercise its return shape cannot be made"
                ));
                return;
            }
            // SAFETY: `windowShouldClose:` is registered `B@:@` (audited in part
            // A); `delegate` and `window` are live objects.
            let should_close: Bool = unsafe {
                let send: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Bool = checked_msg();
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
        }

        /// Part C for the application delegate: one live send through
        /// `applicationShouldTerminate:`, the one row whose return is not `void`,
        /// registered `Q@:@` (an eight-byte `NSApplicationTerminateReply`).
        ///
        /// With no quit-confirm hook registered (this process installs none) the
        /// fork answers `NSTerminateNow` (1), so the row is live and dispatches with
        /// this prototype. It does not prove the return width: a `-> bool` IMP
        /// answering `true` can read as exactly 1. Part A's encoding check guards
        /// that (`B@:@` or `c@:@`, never `Q@:@`). No side effects: the handler reads
        /// `QUIT_CONFIRM_HOOK` and returns; only `-terminate:` acts on the reply.
        fn app_delegate_live_sends(
            &mut self,
            app_delegate: Id,
            app: Id,
            registered: &BTreeMap<String, String>,
        ) {
            println!("\n=== C. LIVE SENDS THROUGH THE REGISTERED IMPS ===");
            let want = "applicationShouldTerminate:";
            if !registered.contains_key(want) {
                // An unregistered selector would abort in
                // `-doesNotRecognizeSelector:`; report the absence instead.
                self.fail(format!(
                    "{want} is not in the registered table, so the live send that would exercise \
                     its return shape cannot be made"
                ));
                return;
            }
            // NSTerminateCancel = 0, NSTerminateNow = 1, NSTerminateLater = 2.
            const NS_TERMINATE_NOW: usize = 1;
            // SAFETY: `applicationShouldTerminate:` is registered `Q@:@` (audited
            // in part A, against the protocol where this host has it and the
            // fork's declaration where it does not); `app_delegate` and `app` are
            // the live objects AppKit is holding; the handler consults a hook this
            // process never installs and performs no send of its own.
            let reply: usize = unsafe {
                let send: unsafe extern "C-unwind" fn(Id, Sel, Id) -> usize = checked_msg();
                send(app_delegate, sel!(applicationShouldTerminate:), app)
            };
            println!("  -applicationShouldTerminate: = {reply:#018x}");
            if reply != NS_TERMINATE_NOW {
                self.fail(format!(
                    "applicationShouldTerminate: answered {reply:#018x}; with no quit-confirm hook \
                     registered the fork returns NSTerminateNow ({NS_TERMINATE_NOW:#018x}), so \
                     either the veto path is wired wrong or the IMP is not returning the \
                     eight-byte NSApplicationTerminateReply its registered Q@:@ promises"
                ));
            }
        }

        /// Part C for the view: the struct shapes `view.rs` registers, sent for real.
        /// A direct typed send proves only that trampoline and caller agree about
        /// registers; the encoding is checked by part A and
        /// [`Self::first_rect_through_foundation`].
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
                let send: unsafe extern "C-unwind" fn(Id, Sel) -> Bool = checked_msg();
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

            // 2. A 16-byte struct return by value (x0/x1 on arm64). `NSNotFound` is
            //    `NSIntegerMax`, so a truncated or swapped return cannot produce it.
            // SAFETY: `markedRange` is registered `{_NSRange=QQ}@:`.
            let marked: aterm_objc::NSRange = unsafe {
                let send: unsafe extern "C-unwind" fn(Id, Sel) -> aterm_objc::NSRange =
                    checked_msg();
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

            // 3. A struct argument by value, answering an NSUInteger.
            // SAFETY: `characterIndexForPoint:` is registered
            // `Q@:{CGPoint=dd}`; the fork answers 0 for every point.
            let idx: usize = unsafe {
                let send: unsafe extern "C-unwind" fn(Id, Sel, aterm_objc::CGPoint) -> usize =
                    checked_msg();
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

            // 4. The 32-byte struct return, against the signature an input method
            //    is handed for it.
            let first_rect = registered["firstRectForCharacterRange:actualRange:"].clone();
            self.first_rect_through_foundation(view, &first_rect);
        }

        /// `firstRectForCharacterRange:actualRange:` asked of Foundation.
        ///
        /// `-[NSObject methodSignatureForSelector:]` answers from a conformed-to
        /// protocol's declaration, not the registered encoding, and `WinitView`
        /// claims `NSTextInputClient`; so the return length and an `NSInvocation`
        /// round trip cannot fail on this row for any registered string (part E
        /// measures this). What can fail: an input method lays out the frame from
        /// the protocol's signature while the IMP was compiled for the class's
        /// own, so the class's registered return type must be the one Foundation
        /// hands out. Part A compares the strings; this checks the string through
        /// the API AppKit calls.
        fn first_rect_through_foundation(&mut self, view: Id, registered: &str) {
            let sel = sel!(firstRectForCharacterRange:actualRange:);
            // SAFETY: `view` is live; `-methodSignatureForSelector:` is
            // `-(id)(SEL)` on NSObject and returns an autoreleased signature or
            // nil.
            let sig: Id = unsafe {
                let f: unsafe extern "C-unwind" fn(Id, Sel, Sel) -> Id = checked_msg();
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
                let n: unsafe extern "C-unwind" fn(Id, Sel) -> usize = checked_msg();
                let t: unsafe extern "C-unwind" fn(Id, Sel) -> *const std::ffi::c_char =
                    checked_msg();
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

            // The row, actually sent. Printed, not asserted: it is the fork's IME
            // position in screen coordinates, which depend on where the window landed.
            let range = aterm_objc::NSRange {
                location: 0,
                length: 1,
            };
            // SAFETY: the row is registered
            // `{CGRect=…}@:{_NSRange=QQ}^{_NSRange=QQ}` (audited in part A); a
            // null `actualRange:` is what AppKit itself passes when it does not
            // want one back, and the fork ignores the argument entirely.
            let direct: aterm_objc::CGRect = unsafe {
                let send: unsafe extern "C-unwind" fn(
                    Id,
                    Sel,
                    aterm_objc::NSRange,
                    *mut aterm_objc::NSRange,
                ) -> aterm_objc::CGRect = checked_msg();
                send(view, sel, range, std::ptr::null_mut())
            };
            println!(
                "  -firstRectForCharacterRange: direct = {:?} {:?}",
                direct.origin, direct.size
            );
        }

        /// Part D: the rows the runtime declares nowhere ([`Unchecked`]). Per row:
        ///
        /// * the registered encoding must be what the fork's Rust signature
        ///   produces through `aterm_objc::method_encoding!`, which a retyped
        ///   argument or return trips;
        /// * Foundation must read the registered string back as written, which for
        ///   these rows it really does read (part E, case 3).
        ///
        /// The direct-send-against-`NSInvocation` comparison needs a return value
        /// and this row returns void, so that tooth is drawn in part E on a control
        /// row with the same property.
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

        /// Foundation's parse of one row's registered string, compared against that
        /// string. Called only for a row no protocol declares, so it is a question
        /// about our encoding, not a header.
        fn foundation_reads_our_string(&mut self, instance: Id, name: &str, registered: &str) {
            let sel = sel_named(name);
            // SAFETY: `instance` is live and `-methodSignatureForSelector:` is
            // `-(id)(SEL)` on NSObject, answering an autoreleased signature or
            // nil.
            let sig: Id = unsafe {
                let f: unsafe extern "C-unwind" fn(Id, Sel, Sel) -> Id = checked_msg();
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
                let n: unsafe extern "C-unwind" fn(Id, Sel) -> usize = checked_msg();
                let t: unsafe extern "C-unwind" fn(Id, Sel) -> *const std::ffi::c_char =
                    checked_msg();
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
                    let g: unsafe extern "C-unwind" fn(Id, Sel, usize) -> *const std::ffi::c_char =
                        checked_msg();
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

        /// Part E: the instrument's own teeth, drawn on control classes this
        /// function builds before they are trusted on the fork. Each measures a
        /// premise the rest of the file stands on:
        ///
        /// 1. For a selector no protocol declares, `NSMethodSignature` is built from
        ///    the class's registered string. One IMP backs an honest `{CGRect=…}`
        ///    row and a 32-byte non-HFA lie: the honest row must answer the same
        ///    rectangle through `NSInvocation` and a direct send, the lie must not
        ///    (arm64 only; see the ABI note in the body). A `CGPoint` IMP registered
        ///    honestly and as a 16-byte integer-class `{qq}` is armed on every arch:
        ///    honest agrees, lie disagrees. That verdict is measured on x86_64; on
        ///    arm64 it is the ABI's expectation (the lie reads `x0`:`x1`, which still
        ///    hold `self`/`_cmd`), and a different answer there is a finding.
        /// 2. For a selector a conformed-to protocol declares, the protocol wins: a
        ///    class claiming `NSTextInputClient` that registers a lie for
        ///    `firstRectForCharacterRange:actualRange:` still reports `NSRect`.
        /// 3. For a selector a conformed-to protocol does not declare, the class's
        ///    own table wins again. That is `frameDidChange:` on `WinitView`, the
        ///    premise of part D's second check.
        fn teeth_are_live(&mut self) {
            println!("\n########## E. THE INSTRUMENT'S OWN TEETH ##########");
            let honest = format!("{}@:", <aterm_objc::CGRect as aterm_objc::Encode>::ENCODING);
            let imp = probe_rect as *const std::ffi::c_void;

            // 1. A class nothing declares anything for.
            let mut builder = aterm_objc::begin(c"NSObject", c"ATermAuditProbe");
            builder.add_rust_ivar::<()>();
            // SAFETY: `probe_rect` is `extern "C"`, has the exact `(id, SEL)`
            // prototype both encodings describe, returns by value and cannot
            // unwind. The second registration is deliberately wrong (the control)
            // and is reached only through this function.
            unsafe {
                builder.add_method(sel!(atermAuditHonestRect), imp, &honest);
                builder.add_method(sel!(atermAuditLyingRect), imp, PROBE_LIE);
            }
            let probe = builder.register();
            // SAFETY: `+alloc`/`-init` on a freshly registered `NSObject`
            // subclass whose only ivar is a zero-sized Rust payload.
            let obj = unsafe {
                let send: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
                send(send(probe.class().as_id(), sel!(alloc)), sel!(init))
            };
            if obj.is_null() {
                self.fail("the audit could not allocate its own probe object".to_owned());
                return;
            }
            // The lie is per ABI. arm64 returns a CGRect (an HFA) in d0-d3 and a
            // 32-byte non-HFA through x8, so the {qqqq} row bites there. x86_64
            // returns every 32-byte struct through a hidden pointer, so the rect
            // tooth is inert there by ABI; the register-class tooth below (INTEGER
            // rax:rdx against SSE xmm0:xmm1) runs on every arch.
            let rect_tooth_live = cfg!(target_arch = "aarch64");
            let honest_ok = self.probe_row(obj, sel!(atermAuditHonestRect), &honest, true);
            let lying_ok = if rect_tooth_live {
                self.probe_row(obj, sel!(atermAuditLyingRect), PROBE_LIE, false)
            } else {
                println!(
                    "  -atermAuditLyingRect: inert on x86_64 — a 32-byte struct returns through \
                     a hidden pointer whatever its class, so a non-HFA lie is laid out exactly \
                     like the honest row; the register-class tooth below is the live one here"
                );
                true
            };
            if honest_ok && lying_ok && rect_tooth_live {
                println!(
                    "  the NSInvocation tooth is LIVE: it agrees with a direct send on an honest \
                     row and disagrees on a lying one"
                );
            }

            // The register-class tooth, on every arch.
            let honest_point = format!(
                "{}@:",
                <aterm_objc::CGPoint as aterm_objc::Encode>::ENCODING
            );
            let point_imp = probe_point as *const std::ffi::c_void;
            let mut builder = aterm_objc::begin(c"NSObject", c"ATermAuditPointProbe");
            builder.add_rust_ivar::<()>();
            // SAFETY: `probe_point` is `extern "C"`, has the exact `(id, SEL)`
            // prototype both encodings describe, returns a CGPoint in registers on
            // both ABIs and cannot unwind. The second registration is deliberately
            // wrong (a 16-byte integer-class struct), which changes only which
            // registers NSInvocation reads.
            unsafe {
                builder.add_method(sel!(atermAuditHonestPoint), point_imp, &honest_point);
                builder.add_method(sel!(atermAuditLyingPoint), point_imp, PROBE_LIE_POINT);
            }
            let point_probe = builder.register();
            // SAFETY: `+alloc`/`-init` on a freshly registered `NSObject`
            // subclass whose only ivar is a zero-sized Rust payload.
            let pobj = unsafe {
                let send: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
                send(send(point_probe.class().as_id(), sel!(alloc)), sel!(init))
            };
            if pobj.is_null() {
                self.fail("the audit could not allocate its point probe object".to_owned());
                return;
            }
            let honest_point_ok =
                self.probe_point_row(pobj, sel!(atermAuditHonestPoint), &honest_point, true);
            let lying_point_ok =
                self.probe_point_row(pobj, sel!(atermAuditLyingPoint), PROBE_LIE_POINT, false);
            if honest_point_ok && lying_point_ok {
                println!(
                    "  the register-class tooth is LIVE: NSInvocation agrees with a direct send \
                     on an honest float pair and disagrees when the row claims an integer pair"
                );
            }

            // 2. The same experiment against a claimed protocol.
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
            // SAFETY: the IMP's prototype matches neither encoding and is never
            // invoked through them: the rows exist to be asked about, and nothing
            // outside this function reaches the class. The second row is case 3,
            // the same 32-byte lie under a selector `NSTextInputClient` does not
            // declare.
            unsafe {
                builder.add_method(sel, imp, lie);
                builder.add_method(sel!(atermAuditUndeclaredRect), imp, PROBE_LIE);
            }
            let claimer = builder.register();
            // SAFETY: as above.
            let obj = unsafe {
                let send: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
                send(send(claimer.class().as_id(), sel!(alloc)), sel!(init))
            };
            // SAFETY: `obj` is live; `-methodSignatureForSelector:` answers an
            // autoreleased signature or nil, and `-methodReturnType` is
            // `-(const char *)` on it.
            let answered = unsafe {
                let f: unsafe extern "C-unwind" fn(Id, Sel, Sel) -> Id = checked_msg();
                let sig = f(obj, sel!(methodSignatureForSelector:), sel);
                if sig.is_null() {
                    None
                } else {
                    let t: unsafe extern "C-unwind" fn(Id, Sel) -> *const std::ffi::c_char =
                        checked_msg();
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

            // 3. The same protocol-claiming class, asked about a selector the
            //    protocol does not declare: our string must win, or part D's
            //    `foundation_reads_our_string` reads something else.
            let undeclared = sel!(atermAuditUndeclaredRect);
            assert!(
                protocol_declares(c"NSTextInputClient", "atermAuditUndeclaredRect").is_none(),
                "this probe's whole point is a selector NSTextInputClient does not declare"
            );
            // SAFETY: `obj` is live; `-methodSignatureForSelector:` answers an
            // autoreleased signature or nil, and `-methodReturnType` is
            // `-(const char *)` on it.
            let answered_undeclared = unsafe {
                let f: unsafe extern "C-unwind" fn(Id, Sel, Sel) -> Id = checked_msg();
                let sig = f(obj, sel!(methodSignatureForSelector:), undeclared);
                if sig.is_null() {
                    None
                } else {
                    let t: unsafe extern "C-unwind" fn(Id, Sel) -> *const std::ffi::c_char =
                        checked_msg();
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

        /// [`Self::probe_row`] over a `CGPoint`, whose honest and lying encodings
        /// differ in register class rather than indirection.
        fn probe_point_row(
            &mut self,
            obj: Id,
            sel: Sel,
            encoding: &str,
            expect_agreement: bool,
        ) -> bool {
            let name = sel.name().to_string_lossy().into_owned();
            // SAFETY: `probe_point` really is `(id, SEL) -> CGPoint`; this is the
            // COMPILER's view of the row, correct for both registrations because
            // both are backed by that one function.
            //
            // The bare cast, on purpose: the lying row's registered string is
            // the experiment, and `checked_msg` would refuse it before the send.
            #[expect(
                clippy::disallowed_methods,
                reason = "the control sends through a deliberately mis-registered row, which the checked send would refuse"
            )]
            let direct: aterm_objc::CGPoint = unsafe {
                let send: unsafe extern "C-unwind" fn(Id, Sel) -> aterm_objc::CGPoint =
                    aterm_objc::msg();
                send(obj, sel)
            };
            // SAFETY: `obj` is live; `-methodSignatureForSelector:` answers an
            // autoreleased signature or nil.
            let sig: Id = unsafe {
                let f: unsafe extern "C-unwind" fn(Id, Sel, Sel) -> Id = checked_msg();
                f(obj, sel!(methodSignatureForSelector:), sel)
            };
            if sig.is_null() {
                self.fail(format!(
                    "Foundation could not build a signature from {encoding}, which this audit \
                     registered itself"
                ));
                return false;
            }
            // SAFETY: as in `probe_row`; `-getReturnValue:` writes exactly
            // `methodReturnLength` bytes, 16 for both encodings, into a 16-byte
            // `CGPoint`.
            let through: aterm_objc::CGPoint = unsafe {
                let new: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Id = checked_msg();
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
                let set_id: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
                set_id(inv, sel!(setTarget:), obj);
                let set_sel: unsafe extern "C-unwind" fn(Id, Sel, Sel) = checked_msg();
                set_sel(inv, sel!(setSelector:), sel);
                let go: unsafe extern "C-unwind" fn(Id, Sel) = checked_msg();
                go(inv, sel!(invoke));
                let mut out = aterm_objc::CGPoint::default();
                let get: unsafe extern "C-unwind" fn(Id, Sel, *mut std::ffi::c_void) =
                    checked_msg();
                get(
                    inv,
                    sel!(getReturnValue:),
                    std::ptr::from_mut(&mut out).cast(),
                );
                out
            };
            let agreed = direct == through;
            println!(
                "  -{name} registered {encoding}\n    direct = {direct:?}\n    NSInvocation = \
                 {through:?}   agree={agreed}"
            );
            if agreed != expect_agreement {
                self.fail(if expect_agreement {
                    format!(
                        "{name} is registered honestly and still answered differently through \
                         NSInvocation — this audit cannot round-trip a correct 16-byte row, so \
                         none of its verdicts about layout mean anything"
                    )
                } else {
                    format!(
                        "{name} is registered {encoding} — a 16-byte INTEGER-class lie against an \
                         IMP that answers in the float registers — and NSInvocation still \
                         answered what a direct send did. The register-class comparison this \
                         file uses to detect a lying encoding no longer detects one"
                    )
                });
                return false;
            }
            true
        }

        /// One probe row, sent directly and through `NSInvocation`. An honest row
        /// must round-trip and a lying one must not; answers whether it behaved so.
        fn probe_row(&mut self, obj: Id, sel: Sel, encoding: &str, expect_agreement: bool) -> bool {
            let name = sel.name().to_string_lossy().into_owned();
            // SAFETY: `probe_rect` really is `(id, SEL) -> CGRect`; this is the
            // COMPILER's view of the row, which is correct for both
            // registrations because both are backed by that one function.
            //
            // The bare cast, on purpose: the lying row's registered string is
            // the experiment, and `checked_msg` would refuse it before the send.
            #[expect(
                clippy::disallowed_methods,
                reason = "the control sends through a deliberately mis-registered row, which the checked send would refuse"
            )]
            let direct: aterm_objc::CGRect = unsafe {
                let send: unsafe extern "C-unwind" fn(Id, Sel) -> aterm_objc::CGRect =
                    aterm_objc::msg();
                send(obj, sel)
            };
            // SAFETY: `obj` is live; `-methodSignatureForSelector:` answers an
            // autoreleased signature or nil.
            let sig: Id = unsafe {
                let f: unsafe extern "C-unwind" fn(Id, Sel, Sel) -> Id = checked_msg();
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
            // invocation for a live signature; `-setTarget:`/`-setSelector:` are
            // `-(void)(id)` and `-(void)(SEL)`; `-getReturnValue:` writes
            // `methodReturnLength` bytes, 32 for both encodings, into a 32-byte
            // `CGRect`.
            let through: aterm_objc::CGRect = unsafe {
                let new: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Id = checked_msg();
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
                let set_id: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
                set_id(inv, sel!(setTarget:), obj);
                let set_sel: unsafe extern "C-unwind" fn(Id, Sel, Sel) = checked_msg();
                set_sel(inv, sel!(setSelector:), sel);
                let go: unsafe extern "C-unwind" fn(Id, Sel) = checked_msg();
                go(inv, sel!(invoke));
                let mut out = aterm_objc::CGRect::default();
                let get: unsafe extern "C-unwind" fn(Id, Sel, *mut std::ffi::c_void) =
                    checked_msg();
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

        /// The class whose registered table describes `instance`, plus the isa's
        /// name when the two differ (see the call site).
        fn audited_class(&mut self, instance: Id) -> (aterm_objc::ClassPtr, Option<String>) {
            // SAFETY: `instance` is a live object; `-class` is `#@:` on every
            // Apple runtime and `object_getClass` reads the isa directly.
            let (reported, isa) = unsafe {
                let send: unsafe extern "C-unwind" fn(Id, Sel) -> aterm_objc::ClassPtr =
                    checked_msg();
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

        /// Parts A, B and F for one target, given a live instance of it. Answers
        /// the registered rows, or `None` if the instance is not of the target's
        /// class (a finding).
        fn table_and_conformance(
            &mut self,
            target: &Target,
            instance: Id,
        ) -> Option<BTreeMap<String, String>> {
            // The isa is not always the class: KVO subclasses an observed instance
            // as `NSKVONotifying_<Name>`, and `WindowDelegate::new` observes every
            // window's `effectiveAppearance`, so `object_getClass` on a `WinitWindow`
            // answers a class whose own table holds KVO's setters, not the fork's
            // rows. `-class` is what KVO overrides to hide itself; it is used, and
            // the isa must be it or a subclass of it.
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
            // The superclass chain decides where the ivar slot sits and where
            // `-dealloc` super-sends, so it is printed.
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

            // Derived from `cls`'s superclass, never `cls` itself; see
            // [`derived_authority_protocols`].
            let authorities = derived_authority_protocols(target, cls);
            let (used, registered) = self.methods(target, &authorities, cls);
            self.protocols(target, &authorities, cls, instance, &used);
            self.host_lacks_pins(target, &registered);
            Some(registered)
        }

        /// Part F: the written [`HostLacks`] shapes, held to the runtime wherever it
        /// has the protocol (the release-cutting host does). Each is a finding when
        /// it fails:
        ///
        /// * the entry's protocol is one this target claims, so deleting the name
        ///   from the fork's `protocols:` list is a finding, not an unchecked row;
        /// * the entry's selector is registered, so a renamed or dropped row leaves
        ///   no stale free pass;
        /// * where the protocol is the host's (not aterm's stand-in), it declares
        ///   exactly the written shape, so a retyped fork signature cannot rot the
        ///   entry unnoticed.
        ///
        /// On a host that lacks the protocol the third cannot be asked, and the
        /// transcript says so.
        fn host_lacks_pins(&mut self, target: &Target, registered: &BTreeMap<String, String>) {
            if target.host_lacks.is_empty() {
                return;
            }
            println!("\n=== F. THE WRITTEN SHAPES, AGAINST THE RUNTIME THAT MAY HAVE THEM ===");
            let supplied = aterm_objc::protocols_registered_by_aterm();
            for pin in target.host_lacks {
                let proto_name = pin.protocol.to_string_lossy().into_owned();
                if !target.claimed.contains(&pin.protocol) {
                    self.fail(format!(
                        "{}: the written shape for {} names {proto_name}, which this target does \
                         not claim — so the row it speaks for is not this protocol's, and the \
                         entry would check a row against a declaration nothing ties it to",
                        target.class_name, pin.sel
                    ));
                    continue;
                }
                if !registered.contains_key(pin.sel) {
                    self.fail(format!(
                        "{}: the written shape for {} is stale — the class registers no such \
                         selector, so the entry holds nothing to any shape at all",
                        target.class_name, pin.sel
                    ));
                    continue;
                }
                let expected = (pin.expected)();
                let sel = sel_named(pin.sel);
                // The host has it only if the runtime knows the name and the
                // object under it is not this process's own stand-in.
                let host_has = !supplied.contains(&pin.protocol)
                    && loaded_protocols()
                        .iter()
                        .any(|(name, _)| *name == proto_name);
                if !host_has {
                    println!(
                        "  {:<46} {expected:<10} not cross-checkable here: {proto_name} is \
                         aterm's stand-in on this host",
                        pin.sel
                    );
                    continue;
                }
                match protocols_declaring(sel)
                    .into_iter()
                    .find(|(name, _)| *name == proto_name)
                {
                    Some((_, declared)) if declared == expected => println!(
                        "  {:<46} {expected:<10} agrees with {proto_name}, which THIS host \
                         registers",
                        pin.sel
                    ),
                    Some((_, declared)) => self.fail(format!(
                        "{}: the written shape for {} says {expected} (from `{}`) but \
                         {proto_name}, which this host registers, declares {declared}. The \
                         written shape is what a host LACKING this protocol checks the row \
                         against, so it may not disagree with the protocol where one exists",
                        target.class_name, pin.sel, pin.rust
                    )),
                    None => self.fail(format!(
                        "{}: the written shape for {} claims {proto_name} declares it, and this \
                         host registers {proto_name} but it declares no such row — so on a host \
                         that lacks the protocol this entry would check the row against a \
                         signature the protocol never had",
                        target.class_name, pin.sel
                    )),
                }
            }
        }

        /// Every part, given the objects AppKit is holding. Part E runs first, on
        /// this file's own classes, so the premises are measured before use.
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
            // `NSApp`, its delegate and the audited view's window: every instance
            // is one this process already holds.
            if let Some(registered) = self.table_and_conformance(&APP_DELEGATE, app_delegate) {
                self.app_delegate_live_sends(app_delegate, app, &registered);
            }
            self.table_and_conformance(&WINDOW, window);
            if self.table_and_conformance(&APP, app).is_some() {
                self.patched_rows(&APP, app);
            }
        }

        /// The one tooth that can bite on a [`Rows::Patched`] target.
        ///
        /// For a patched class part A is a tautology: it reads both sides off the
        /// same `Method`, and no swizzle can move that string
        /// (`method_setImplementation` takes no types, `class_replaceMethod` ignores
        /// them for an existing row, and `class_addMethod` refuses one). Part A
        /// still catches the selector being renamed away. A change to
        /// `-[NSApplication sendEvent:]`'s own signature is invisible here;
        /// `SwizzleSite::install`'s launch-time refusal covers it.
        ///
        /// The evidence is whose code the runtime will call. `override_send_event`
        /// is on every keystroke and changes nothing an encoding can see; if it
        /// stopped running, Cmd+key `keyUp:` delivery would vanish with every other
        /// check green. So the IMP's image is asked of the dynamic loader: the
        /// fork's code is in the executable, AppKit's in AppKit.
        fn patched_rows(&mut self, target: &Target, instance: Id) {
            let Rows::Patched(ours) = target.rows else {
                return;
            };
            println!("\n=== A2. THE ROWS THIS FORK PATCHED INTO A FRAMEWORK CLASS ===");
            // The isa, not `-class`: this asks what the runtime will dispatch, and
            // dispatch starts at the isa (`NSKVONotifying_NSApplication` here, since
            // NSApp is KVO-observed), walking up to the swizzled row.
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
                         passes either way, and what is lost is Cmd+key keyUp: delivery",
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

    /// The winit side: make one real window, audit the objects behind it, leave.
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
            let view = aterm_objc::HasNsView::ns_view(window);
            if view.is_null() {
                self.audit.blocked = Some("the window answered no content view".to_owned());
                el.exit();
                return;
            }
            // SAFETY: `-[NSView window]` is `@@:`; `view` is the live NSView
            // winit just handed out.
            let ns_window: Id = unsafe {
                let send: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
                send(view, sel!(window))
            };
            if ns_window.is_null() {
                self.audit.blocked = Some("the view has no window".to_owned());
                el.exit();
                return;
            }
            // SAFETY: `-[NSWindow delegate]` is `@@:` and `ns_window` is live.
            let delegate: Id = unsafe {
                let send: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
                send(ns_window, sel!(delegate))
            };
            if delegate.is_null() {
                self.audit.blocked = Some("the NSWindow has no delegate installed".to_owned());
                el.exit();
                return;
            }
            // NSApp and its delegate, both built by `EventLoop::new`.
            // SAFETY: `+[NSApplication sharedApplication]` is `@#:`, called on the
            // main thread.
            let app: Id = unsafe {
                let send: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
                send(class(c"NSApplication").as_id(), sel!(sharedApplication))
            };
            if app.is_null() {
                self.audit.blocked = Some("there is no shared NSApplication".to_owned());
                el.exit();
                return;
            }
            // SAFETY: `-[NSApplication delegate]` is `@@:` and `app` is live.
            let app_delegate: Id = unsafe {
                let send: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
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
        // The quiet builder (Accessory, no launch activation): `verify appkit` runs
        // this drive beside a developer who is typing somewhere else.
        let mut el = match aterm_gui::quiet_driver_event_loop_builder::<()>().build() {
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
        // Dropping the window runs the ported `-dealloc`.
        drop(driver.window.take());

        if let Some(why) = driver.audit.blocked {
            eprintln!("objc-live-class-audit: NOT RUN — {why}");
            return NOT_RUN;
        }
        println!("\n=== VERDICT ===");
        for row in &driver.audit.host_unchecked {
            println!("  NOT CHECKED on this host: {row}");
        }
        if !driver.audit.findings.is_empty() {
            for f in &driver.audit.findings {
                println!("  FAIL: {f}");
            }
            println!(
                "objc-live-class-audit: {} FINDING(S)",
                driver.audit.findings.len()
            );
            FAIL
        } else if !driver.audit.host_unchecked.is_empty() {
            // Every checked row agreed and some could not be checked here: not a
            // pass, and not the port's fault.
            eprintln!(
                "objc-live-class-audit: NOT RUN in full — {} row(s) have no authority on this \
                 host because its AppKit does not register the protocol that declares them; \
                 every other row agreed. A verdict needs a host that registers it.",
                driver.audit.host_unchecked.len()
            );
            NOT_RUN
        } else if !driver.audit.fork_declared.is_empty() {
            // Every row was held to a shape, some to the fork's own because this
            // host's AppKit could not speak for them: a pass under its own label.
            for row in &driver.audit.fork_declared {
                println!("  CHECKED AGAINST THE FORK'S OWN DECLARATION: {row}");
            }
            println!(
                "objc-live-class-audit: OK — every registered row agrees, and {} of them were \
                 checked against this fork's own declaration because this host's AppKit does not \
                 register the protocol that declares them. Part F held those written shapes to \
                 nothing here; a host that registers the protocol checks them against it on every \
                 run.",
                driver.audit.fork_declared.len()
            );
            FORK_DECLARED
        } else {
            println!("objc-live-class-audit: OK — every registered row agrees with the runtime.");
            PASS
        }
    }
}
