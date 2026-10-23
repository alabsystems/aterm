// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live Objective-C runtime class auditor for `aterm-window`'s macOS classes.
//!
//! Queries [`aterm_objc::class_methods`] on live `WinitWindowDelegate`, `WinitView`,
//! `WinitApplicationDelegate`, `WinitWindow`, and `NSApplication` instances, verifying method
//! encodings and protocol conformance against runtime protocol tables and superclass chains.

/// Every row agreed with the runtime's own authority.
#[cfg(target_os = "macos")]
const PASS: i32 = 0;
/// At least one finding. See the transcript.
#[cfg(target_os = "macos")]
const FAIL: i32 = 1;
/// The audit could not execute here. Not a pass.
const NOT_RUN: i32 = 2;
/// Every row agreed, and at least one was checked against this fork's own
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
    use aterm_window::application::ApplicationHandler;
    use aterm_window::event::WindowEvent;
    use aterm_window::event_loop::ActiveEventLoop;
    use aterm_window::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};
    use aterm_window::window::{Window, WindowId};

    use super::{FAIL, FORK_DECLARED, NOT_RUN, PASS};

    /// One class this audit is responsible for.
    struct Target {
        /// The class name `declare_class!` registers.
        class_name: &'static str,
        /// The `protocols:` list of the fork's `declare_class!` site.
        claimed: &'static [&'static CStr],
        /// Consulted after every protocol.
        authority_classes: &'static [&'static CStr],
        /// Selectors nothing in the runtime declares, with the shape part D holds.
        unchecked: &'static [Unchecked],
        /// Rows whose authority is a protocol this host may not register.
        host_lacks: &'static [HostLacks],
        /// Rows whose authority is derived from declared siblings ([`Family`]).
        families: &'static [Family],
        /// Authority protocols informal on this target's chain.
        informal: &'static [(&'static str, &'static str)],
        /// Which of the class's registered rows are the fork's ([`Rows`]).
        rows: Rows,
    }

    /// Whose method table a [`Target`] is reading. For a class the fork declares,
    #[derive(Clone, Copy)]
    enum Rows {
        /// A class the fork declares.
        Declared,
        /// A framework class the fork patched.
        Patched(&'static [&'static str]),
    }

    /// `window_delegate.rs`'s class. 21 declared rows plus `-dealloc`, all resolved
    const DELEGATE: Target = Target {
        class_name: "WinitWindowDelegate",
        claimed: &[c"NSObject", c"NSWindowDelegate", c"NSDraggingDestination"],
        authority_classes: &[c"NSObject"],
        unchecked: &[],
        host_lacks: &[],
        families: &[],
        informal: &[],
        rows: Rows::Declared,
    };

    /// `view.rs`'s class. 39 declared rows plus `-dealloc`. It claims only
    const VIEW: Target = Target {
        class_name: "WinitView",
        claimed: &[c"NSTextInputClient"],
        authority_classes: &[c"NSView", c"NSResponder", c"NSObject"],
        unchecked: VIEW_UNCHECKED,
        host_lacks: &[],
        families: VIEW_FAMILIES,
        informal: VIEW_INFORMAL,
        rows: Rows::Declared,
    };

    /// `app_state.rs`'s class: the `NSApplicationDelegate` on `NSApp`. Three
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
        authority_classes: &[c"NSObject"],
        unchecked: &[],
        host_lacks: APP_DELEGATE_HOST_LACKS,
        families: &[],
        informal: &[],
        rows: Rows::Declared,
    };

    /// `window.rs`'s class: the `NSWindow` every winit window is. Two declared rows
    const WINDOW: Target = Target {
        class_name: "WinitWindow",
        claimed: &[],
        authority_classes: &[c"NSWindow", c"NSResponder", c"NSObject"],
        unchecked: &[],
        host_lacks: &[],
        families: &[],
        informal: &[],
        rows: Rows::Declared,
    };

    /// `app.rs`'s one row, the only target that is not a class this fork declares.
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
    struct Unchecked {
        /// The selector, as the fork's `@sel(…)` spells it.
        sel: &'static str,
        /// Why no protocol and no class on the chain declares it.
        why: &'static str,
        /// The fork's Rust signature for this row, as source text.
        rust: &'static str,
        /// The encoding [`Self::rust`] produces.
        expected: fn() -> String,
    }

    /// A row whose authority is a protocol this host may not register, with the
    struct HostLacks {
        /// The selector, as the fork's `@sel(…)` spells it.
        sel: &'static str,
        /// The protocol that declares it on a host that has one.
        protocol: &'static CStr,
        /// The fork's Rust signature for this row, as source text.
        rust: &'static str,
        /// The encoding [`Self::rust`] produces.
        expected: fn() -> String,
        /// Why a host may lack the protocol, and where that was measured.
        why: &'static str,
    }

    /// A row nothing declares whose shape is derived from siblings that something
    struct Family {
        /// The row with no declaration of its own.
        sel: &'static str,
        /// Where the siblings are declared.
        protocol: &'static CStr,
        /// The siblings, which must all agree.
        siblings: &'static [&'static str],
        /// Why these siblings are the right authority for that row.
        why: &'static str,
    }

    /// One family: the key-binding actions.
    const VIEW_FAMILIES: &[Family] = &[Family {
        sel: "insertBackTab:",
        protocol: c"NSStandardKeyBindingResponding",
        siblings: &["insertTab:", "cancelOperation:"],
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
    const NS_NOT_FOUND: usize = isize::MAX as usize;

    /// Long enough for macOS to launch its `NSApplication` and hand back a
    const BUDGET: Duration = Duration::from_secs(30);

    /// Where one row's encoding came from.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Source {
        /// A protocol declared it.
        Protocol(&'static CStr),
        /// A class on the superclass chain implements it and the fork overrides it.
        Class(&'static CStr),
        /// Nothing declares this row, but a protocol declares its agreeing siblings ([`Family`]).
        Siblings(&'static CStr),
        /// Nothing in the runtime declares it.
        Nowhere,
    }

    impl Source {
        /// `sel` is the row this source was found for: [`Self::Class`] prints the
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
    fn defining_class(cls: &'static CStr, sel: Sel) -> Option<String> {
        // SAFETY: `class` answers a live class object or nil.
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
    fn sel_named(name: &str) -> Sel {
        let owned: &'static CStr = Box::leak(
            std::ffi::CString::new(name)
                .expect("a selector name has no interior NUL")
                .into_boxed_c_str(),
        );
        aterm_objc::sel_uncached(owned)
    }

    /// Intern a class or protocol name known only as a `&str` (the derived
    fn cstr_named(name: &str) -> &'static CStr {
        Box::leak(
            std::ffi::CString::new(name)
                .expect("a runtime-supplied name has no interior NUL")
                .into_boxed_c_str(),
        )
    }

    /// Every protocol reachable from `seed` through protocol inheritance, in seed
    fn expand_protocols(into: &mut Vec<String>, seed: impl IntoIterator<Item = String>) {
        for name in seed {
            let mut stack = vec![name];
            while let Some(next) = stack.pop() {
                if into.contains(&next) {
                    continue;
                }
                // SAFETY: `protocol` answers a live protocol object or nil.
                stack.extend(protocol_parents(protocol(cstr_named(&next))));
                into.push(next);
            }
        }
    }

    /// The protocols a [`Target`]'s rows are checked against, read off the runtime.
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
        // SAFETY: `cls` is a live class object.
        let mut walk = unsafe { superclass_of(cls) };
        while !walk.is_null() {
            // SAFETY: `walk` is a live class while non-null.
            expand_protocols(&mut names, unsafe { class_protocols(walk) });
            // SAFETY: As above.
            walk = unsafe { superclass_of(walk) };
        }
        names.iter().map(|n| cstr_named(n)).collect()
    }

    /// Whether a [`HostLacks`] entry is the authority for its row on this host:
    fn host_lacks_is_active(target: &Target, pin: &HostLacks) -> bool {
        target.claimed.contains(&pin.protocol)
            && aterm_objc::protocols_registered_by_aterm().contains(&pin.protocol)
    }

    /// Every protocol loaded in this process that declares `sel`, with its
    fn protocols_declaring(sel: Sel) -> Vec<(String, String)> {
        let mut found = Vec::new();
        for (name, proto) in loaded_protocols() {
            // SAFETY: `proto` came from `objc_copyProtocolList` and protocol objects are immortal.
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
    fn loaded_protocols() -> Vec<(String, aterm_objc::ProtocolPtr)> {
        unsafe extern "C" {
            fn objc_copyProtocolList(count: *mut std::ffi::c_uint) -> *mut aterm_objc::ProtocolPtr;
            fn protocol_getName(proto: aterm_objc::ProtocolPtr) -> *const std::ffi::c_char;
            fn free(ptr: *mut std::ffi::c_void);
        }
        let mut count: std::ffi::c_uint = 0;
        // SAFETY: The runtime writes the count through the pointer and hands back a malloc'd array.
        let list = unsafe { objc_copyProtocolList(&raw mut count) };
        if list.is_null() {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count as usize {
            // SAFETY: `i < count`; protocol objects and their names are immortal.
            let proto = unsafe { *list.add(i) };
            // SAFETY: As above.
            let name = unsafe { CStr::from_ptr(protocol_getName(proto)) };
            out.push((name.to_string_lossy().into_owned(), proto));
        }
        // SAFETY: The array is this function's to free.
        unsafe { free(list.cast()) };
        out
    }

    /// The protocols a protocol itself inherits. `aterm-objc` grows only for the
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
        // SAFETY: `proto` is a live protocol object.
        let list = unsafe { protocol_copyProtocolList(proto, &raw mut count) };
        if list.is_null() {
            return Vec::new();
        }
        let mut names = Vec::with_capacity(count as usize);
        for i in 0..count as usize {
            // SAFETY: `i < count`; protocol objects and their names are immortal.
            let name = unsafe { CStr::from_ptr(protocol_getName(*list.add(i))) };
            names.push(name.to_string_lossy().into_owned());
        }
        // SAFETY: The array is this function's to free.
        unsafe { free(list.cast()) };
        names
    }

    /// Which loaded image a code address belongs to, per the dynamic loader: the
    fn image_of(imp: *const std::ffi::c_void) -> Option<String> {
        /// `<dlfcn.h>`'s `Dl_info`: four pointer-sized fields, the first the image
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
        // SAFETY: `DlInfo` is four raw pointers, for which an all-zero bit pattern is a valid.
        let mut info: DlInfo = unsafe { std::mem::zeroed() };
        // SAFETY: `imp` is a code address the runtime handed back and is never dereferenced.
        let ok = unsafe { dladdr(imp, &raw mut info) };
        if ok == 0 || info.dli_fname.is_null() {
            return None;
        }
        // SAFETY: Non-null and NUL-terminated, owned by the loader.
        Some(
            unsafe { CStr::from_ptr(info.dli_fname) }
                .to_string_lossy()
                .into_owned(),
        )
    }

    /// One protocol's declaration of an instance method named `name`.
    fn protocol_declares(p: &'static CStr, name: &str) -> Option<String> {
        // SAFETY: `protocol` answers a live protocol object or nil.
        let types = unsafe { protocol_method_types(protocol(p), sel_named(name), true) };
        types.map(|t| strip_method_offsets(&t))
    }

    /// Split a method type encoding into its return type and its arguments,
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
    const PROBE_RECT: aterm_objc::CGRect = aterm_objc::CGRect {
        origin: aterm_objc::CGPoint { x: 704.0, y: 839.0 },
        size: aterm_objc::CGSize {
            width: 11.0,
            height: 22.0,
        },
    };

    /// The IMP behind part E's rect rows, returning a `CGRect`.
    extern "C" fn probe_rect(_this: Id, _cmd: Sel) -> aterm_objc::CGRect {
        PROBE_RECT
    }

    /// The lying encoding: a 32-byte struct of four `long long`s, the size of an
    const PROBE_LIE: &str = "{_ATermAuditLie=qqqq}@:";

    /// The register-class probe: a `CGPoint`, which both ABIs return in float
    const PROBE_POINT: aterm_objc::CGPoint = aterm_objc::CGPoint { x: 12.5, y: -3.25 };

    /// The IMP behind part E's point rows, returning [`PROBE_POINT`] in registers.
    extern "C" fn probe_point(_this: Id, _cmd: Sel) -> aterm_objc::CGPoint {
        PROBE_POINT
    }

    /// The register-class lie: a 16-byte struct of two `long long`s, the size of a
    const PROBE_LIE_POINT: &str = "{_ATermAuditLie=qq}@:";

    /// The audit's state: the findings it exits on and the host-limited rows.
    #[derive(Default)]
    struct Audit {
        findings: Vec<String>,
        /// Why the audit could not run at all, if it.
        blocked: Option<String>,
        /// Rows that could not be checked on this host.
        host_unchecked: Vec<String>,
        /// Rows checked against this fork's own declaration rather than the runtime.
        fork_declared: Vec<String>,
    }

    impl Audit {
        fn fail(&mut self, what: String) {
            println!("  FINDING: {what}");
            self.findings.push(what);
        }

        /// Find `sel`'s authority by asking the runtime: protocols first, then the
        fn authority_of(
            &mut self,
            target: &Target,
            authorities: &[&'static CStr],
            sel: Sel,
        ) -> (Source, Option<String>) {
            let mut hits: Vec<(&'static CStr, String)> = Vec::new();
            for name in authorities {
                // SAFETY: `protocol` returns a live protocol object or nil.
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
                // SAFETY: Every name here is a framework class that is linked and live.
                if let Some(types) = unsafe { method_types(class(name), sel) } {
                    return (Source::Class(name), Some(strip_method_offsets(&types)));
                }
            }
            let name = sel.name().to_string_lossy().into_owned();
            let Some(family) = target.families.iter().find(|f| f.sel == name) else {
                return (Source::Nowhere, None);
            };
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
                let covered_by_part_f = target
                    .host_lacks
                    .iter()
                    .any(|h| h.sel == name.as_str() && host_lacks_is_active(target, h));
                let verdict = match &authority {
                    Some(a) if *a == registered => "ok ",
                    Some(_) => "BAD",
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
                            println!("      no authority: {}", row.why);
                            println!("      checked in part D against {}", row.rust);
                        } else {
                            let supplied: Vec<String> = aterm_objc::protocols_registered_by_aterm()
                                .iter()
                                .filter(|p| target.claimed.contains(p))
                                .map(|p| p.to_string_lossy().into_owned())
                                .collect();
                            let declared = protocols_declaring(selector);
                            if !declared.is_empty() {
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

            for p in authorities {
                let name = p.to_string_lossy().into_owned();
                if !used.contains(&name) {
                    continue;
                }
                // SAFETY: `-conformsToProtocol:` is `B@:@` on every Apple runtime.
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
                // SAFETY: `-conformsToProtocol:` is `B@:@` on every Apple runtime.
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
        fn delegate_live_sends(
            &mut self,
            delegate: Id,
            window: Id,
            registered: &BTreeMap<String, String>,
        ) {
            println!("\n=== C. LIVE SENDS THROUGH THE REGISTERED IMPS ===");
            let want = "windowShouldClose:";
            if !registered.contains_key(want) {
                self.fail(format!(
                    "{want} is not in the registered table, so the live send that would \
                     exercise its return shape cannot be made"
                ));
                return;
            }
            // SAFETY: `windowShouldClose:` is registered `B@:@` (audited in part A).
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
        fn app_delegate_live_sends(
            &mut self,
            app_delegate: Id,
            app: Id,
            registered: &BTreeMap<String, String>,
        ) {
            println!("\n=== C. LIVE SENDS THROUGH THE REGISTERED IMPS ===");
            let want = "applicationShouldTerminate:";
            if !registered.contains_key(want) {
                self.fail(format!(
                    "{want} is not in the registered table, so the live send that would exercise \
                     its return shape cannot be made"
                ));
                return;
            }
            const NS_TERMINATE_NOW: usize = 1;
            // SAFETY: `applicationShouldTerminate:` is registered `Q@:@`.
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

            // SAFETY: `hasMarkedText` is registered `B@:` (audited in part A) and `view`.
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

            // SAFETY: `characterIndexForPoint:` is registered `Q@:{CGPoint=dd}`.
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

            let first_rect = registered["firstRectForCharacterRange:actualRange:"].clone();
            self.first_rect_through_foundation(view, &first_rect);
        }

        /// `firstRectForCharacterRange:actualRange:` asked of Foundation.
        fn first_rect_through_foundation(&mut self, view: Id, registered: &str) {
            let sel = sel!(firstRectForCharacterRange:actualRange:);
            // SAFETY: `view` is live.
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
            // SAFETY: `-methodReturnLength` and `-numberOfArguments` are `-(NSUInteger)` on a live.
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

            let range = aterm_objc::NSRange {
                location: 0,
                length: 1,
            };
            // SAFETY: The row is registered `{CGRect=…}@:{_NSRange=QQ}^{_NSRange=QQ}`.
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
        fn foundation_reads_our_string(&mut self, instance: Id, name: &str, registered: &str) {
            let sel = sel_named(name);
            // SAFETY: `instance` is live and `-methodSignatureForSelector:` is `-(id)(SEL)`.
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
            // SAFETY: `-numberOfArguments` is `-(NSUInteger)`, `-methodReturnType`.
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
                // SAFETY: As above; `i` is below `numberOfArguments`.
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
        fn teeth_are_live(&mut self) {
            println!("\n########## E. THE INSTRUMENT'S OWN TEETH ##########");
            let honest = format!("{}@:", <aterm_objc::CGRect as aterm_objc::Encode>::ENCODING);
            let imp = probe_rect as *const std::ffi::c_void;

            let mut builder = aterm_objc::begin(c"NSObject", c"ATermAuditProbe");
            builder.add_rust_ivar::<()>();
            // SAFETY: `probe_rect` is `extern "C"`, has the exact `(id, SEL)` prototype.
            unsafe {
                builder.add_method(sel!(atermAuditHonestRect), imp, &honest);
                builder.add_method(sel!(atermAuditLyingRect), imp, PROBE_LIE);
            }
            let probe = builder.register();
            // SAFETY: `+alloc`/`-init` on a freshly registered `NSObject` subclass whose only ivar.
            let obj = unsafe {
                let send: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
                send(send(probe.class().as_id(), sel!(alloc)), sel!(init))
            };
            if obj.is_null() {
                self.fail("the audit could not allocate its own probe object".to_owned());
                return;
            }
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

            let honest_point = format!(
                "{}@:",
                <aterm_objc::CGPoint as aterm_objc::Encode>::ENCODING
            );
            let point_imp = probe_point as *const std::ffi::c_void;
            let mut builder = aterm_objc::begin(c"NSObject", c"ATermAuditPointProbe");
            builder.add_rust_ivar::<()>();
            // SAFETY: `probe_point` is `extern "C"`, has the exact `(id, SEL)` prototype.
            unsafe {
                builder.add_method(sel!(atermAuditHonestPoint), point_imp, &honest_point);
                builder.add_method(sel!(atermAuditLyingPoint), point_imp, PROBE_LIE_POINT);
            }
            let point_probe = builder.register();
            // SAFETY: `+alloc`/`-init` on a freshly registered `NSObject` subclass whose only ivar.
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
            // SAFETY: The IMP's prototype matches neither encoding and is never invoked.
            unsafe {
                builder.add_method(sel, imp, lie);
                builder.add_method(sel!(atermAuditUndeclaredRect), imp, PROBE_LIE);
            }
            let claimer = builder.register();
            // SAFETY: As above.
            let obj = unsafe {
                let send: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
                send(send(claimer.class().as_id(), sel!(alloc)), sel!(init))
            };
            // SAFETY: `obj` is live.
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

            let undeclared = sel!(atermAuditUndeclaredRect);
            assert!(
                protocol_declares(c"NSTextInputClient", "atermAuditUndeclaredRect").is_none(),
                "this probe's whole point is a selector NSTextInputClient does not declare"
            );
            // SAFETY: `obj` is live.
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
        fn probe_point_row(
            &mut self,
            obj: Id,
            sel: Sel,
            encoding: &str,
            expect_agreement: bool,
        ) -> bool {
            let name = sel.name().to_string_lossy().into_owned();
            // SAFETY: `probe_point` really is `(id, SEL) -> CGPoint`, correct.
            #[expect(
                clippy::disallowed_methods,
                reason = "the control sends through a deliberately mis-registered row, which the checked send would refuse"
            )]
            let direct: aterm_objc::CGPoint = unsafe {
                let send: unsafe extern "C-unwind" fn(Id, Sel) -> aterm_objc::CGPoint =
                    aterm_objc::msg();
                send(obj, sel)
            };
            // SAFETY: `obj` is live.
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
            // SAFETY: As in `probe_row`.
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
        fn probe_row(&mut self, obj: Id, sel: Sel, encoding: &str, expect_agreement: bool) -> bool {
            let name = sel.name().to_string_lossy().into_owned();
            // SAFETY: `probe_rect` really is `(id, SEL) -> CGRect`, correct.
            #[expect(
                clippy::disallowed_methods,
                reason = "the control sends through a deliberately mis-registered row, which the checked send would refuse"
            )]
            let direct: aterm_objc::CGRect = unsafe {
                let send: unsafe extern "C-unwind" fn(Id, Sel) -> aterm_objc::CGRect =
                    aterm_objc::msg();
                send(obj, sel)
            };
            // SAFETY: `obj` is live.
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
            // SAFETY: `+invocationWithMethodSignature:` answers an autoreleased invocation.
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
        fn audited_class(&mut self, instance: Id) -> (aterm_objc::ClassPtr, Option<String>) {
            // SAFETY: `instance` is a live object.
            let (reported, isa) = unsafe {
                let send: unsafe extern "C-unwind" fn(Id, Sel) -> aterm_objc::ClassPtr =
                    checked_msg();
                (send(instance, sel!(class)), class_of(instance))
            };
            if reported.is_null() {
                // Nothing can be audited from a nil class; the caller's name
                return (isa, None);
            }
            if reported == isa {
                return (reported, None);
            }
            // SAFETY: Both are live class objects; `superclass_of` tolerates and terminates at nil.
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
        fn table_and_conformance(
            &mut self,
            target: &Target,
            instance: Id,
        ) -> Option<BTreeMap<String, String>> {
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
            let mut chain = Vec::new();
            // SAFETY: `cls` is live; `superclass_of` tolerates and terminates at nil.
            let mut walk = unsafe { superclass_of(cls) };
            while !walk.is_null() {
                // SAFETY: `walk` is a live class while non-null.
                chain.push(unsafe { class_name(walk) }.to_string_lossy().into_owned());
                // SAFETY: As above.
                walk = unsafe { superclass_of(walk) };
            }
            println!("superclass chain = {}", chain.join(" -> "));

            let authorities = derived_authority_protocols(target, cls);
            let (used, registered) = self.methods(target, &authorities, cls);
            self.protocols(target, &authorities, cls, instance, &used);
            self.host_lacks_pins(target, &registered);
            Some(registered)
        }

        /// Part F: the written [`HostLacks`] shapes, held to the runtime wherever it
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
            if let Some(registered) = self.table_and_conformance(&APP_DELEGATE, app_delegate) {
                self.app_delegate_live_sends(app_delegate, app, &registered);
            }
            self.table_and_conformance(&WINDOW, window);
            if self.table_and_conformance(&APP, app).is_some() {
                self.patched_rows(&APP, app);
            }
        }

        /// The one tooth that can bite on a [`Rows::Patched`] target.
        fn patched_rows(&mut self, target: &Target, instance: Id) {
            let Rows::Patched(ours) = target.rows else {
                return;
            };
            println!("\n=== A2. THE ROWS THIS FORK PATCHED INTO A FRAMEWORK CLASS ===");
            // SAFETY: `instance` is the live object AppKit handed back.
            let cls = unsafe { class_of(instance) };
            for name in ours {
                let sel = sel_named(name);
                // SAFETY: `cls` is live and `sel` is interned.
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
                .with_inner_size(aterm_window::dpi::LogicalSize::new(320.0, 200.0))
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
            // SAFETY: `-[NSView window]` is `@@:`; `view` is the live NSView winit just handed out.
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
            // SAFETY: `+[NSApplication sharedApplication]` is `@#:`, called on the main thread.
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
            eprintln!(
                "objc-live-class-audit: NOT RUN in full — {} row(s) have no authority on this \
                 host because its AppKit does not register the protocol that declares them; \
                 every other row agreed. A verdict needs a host that registers it.",
                driver.audit.host_unchecked.len()
            );
            NOT_RUN
        } else if !driver.audit.fork_declared.is_empty() {
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
