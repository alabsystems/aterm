// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The census of the SEND ABI: no `unsafe extern "C" fn(` prototype may remain
//! on any path a foreign exception can take.
//!
//! # Why a text census when the type system already refuses it
//!
//! [`aterm_objc::MsgFn`] is implemented for `unsafe extern "C-unwind" fn(..)`
//! only, so a `"C"` prototype assigned from `msg()` is `E0277` today. This
//! test is the record of WHY, in the place a future "let's add the `"C"` impl
//! back, it is shorter" would have to pass: it was MEASURED
//! (`docs/measured/2026-09-03-delivery-and-objc-exception-containment.md`, §2)
//! that ONE `nounwind` callee on the path an `NSException` takes silently
//! skips that frame's `Drop`s, or aborts with `failed to initiate panic, error
//! 3`, or lets LLVM delete the `catch_unwind` above it. The ABI change itself
//! costs nothing: 1.534 ns/send under both spellings.
//!
//! The files read are the ones a raise inside AppKit can unwind through on
//! its way to a trampoline: this crate, the vendored winit macOS backend, and
//! `aterm-gui`'s sources and drivers.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Every file on a raise's path.
fn census_files() -> Vec<PathBuf> {
    let root = repo_root();
    let mut files = Vec::new();
    for dir in [
        "crates/aterm-objc/src",
        "crates/aterm-objc/tests",
        "crates/aterm-objc/examples",
        "vendor/winit/src/platform_impl/macos",
        "crates/aterm-gui/src",
        "crates/aterm-gui/examples",
    ] {
        rust_files(&root.join(dir), &mut files);
    }
    files.sort();
    assert!(
        files.len() > 100,
        "the census read only {} files under {}",
        files.len(),
        root.display()
    );
    files
}

/// Whether the occurrence at `at` sits on a comment line (`//`, `///`,
/// `//!`) — prose about the old spelling, including this crate's own
/// `compile_fail` doctest that proves it is refused, is not a prototype.
fn on_comment_line(src: &str, at: usize) -> bool {
    let line_start = src[..at].rfind('\n').map_or(0, |i| i + 1);
    src[line_start..at].trim_start().starts_with("//")
}

/// `(file, line)` of every `extern "C" fn(` type that is assigned from a
/// `msg()` / `msg_super()` cast — a send prototype in the old spelling.
fn nounwind_send_prototypes() -> Vec<String> {
    let mut found = Vec::new();
    for file in census_files() {
        let src = std::fs::read_to_string(&file).expect("readable source");
        let mut from = 0;
        while let Some(at) = src[from..].find("extern \"C\" fn(") {
            let start = from + at;
            from = start + 1;
            if on_comment_line(&src, start) {
                continue;
            }
            let stmt_end = src[start..].find(';').map_or(src.len(), |i| start + i);
            let stmt = &src[start..stmt_end];
            if stmt.contains("msg()") || stmt.contains("msg_super()") || stmt.contains("msg::<") {
                let line = src[..start].matches('\n').count() + 1;
                found.push(format!("{}:{line}", file.display()));
            }
        }
    }
    found
}

#[test]
fn no_send_prototype_is_spelled_extern_c() {
    let found = nounwind_send_prototypes();
    assert!(
        found.is_empty(),
        "these send prototypes are `extern \"C\"` (nounwind): an NSException raised \
         inside such a send skips the frame's destructors — measured. Spell them \
         `extern \"C-unwind\"`:\n{}",
        found.join("\n")
    );
}

#[test]
fn the_census_is_not_vacuous() {
    // The new spelling must be everywhere the old one was: the crate, winit
    // and aterm-gui together carried ~175 send prototypes before the change.
    let mut unwind = 0usize;
    let mut unwind_sends = 0usize;
    for file in census_files() {
        let src = std::fs::read_to_string(&file).expect("readable source");
        let mut from = 0;
        while let Some(at) = src[from..].find("extern \"C-unwind\" fn(") {
            let start = from + at;
            unwind += 1;
            let stmt_end = src[start..].find(';').map_or(src.len(), |i| start + i);
            if src[start..stmt_end].contains("msg()") {
                unwind_sends += 1;
            }
            from = start + 1;
        }
    }
    assert!(
        unwind >= 150,
        "only {unwind} `extern \"C-unwind\" fn(` prototypes were found"
    );
    assert!(
        unwind_sends >= 100,
        "only {unwind_sends} of them are `msg()` casts"
    );
}

#[test]
fn the_entry_points_and_the_msgfn_impl_are_declared_c_unwind() {
    let runtime = std::fs::read_to_string(repo_root().join("crates/aterm-objc/src/runtime.rs"))
        .expect("runtime.rs");
    for symbol in [
        "fn objc_msgSend();",
        "fn objc_msgSendSuper();",
        "fn objc_msgSend_stret();",
        "fn objc_msgSendSuper_stret();",
    ] {
        let at = runtime
            .find(symbol)
            .unwrap_or_else(|| panic!("{symbol} is declared"));
        let head = &runtime[..at];
        let block = head
            .rfind("unsafe extern \"")
            .expect("an extern block above it");
        let abi = &head[block..];
        assert!(
            abi.starts_with("unsafe extern \"C-unwind\""),
            "{symbol} is declared in a block spelled {:?}",
            abi.lines().next().unwrap_or("")
        );
    }
    assert!(
        runtime.contains("MsgFn for unsafe extern \"C-unwind\" fn($($arg),*) -> Ret"),
        "MsgFn is implemented for the C-unwind spelling"
    );
    assert!(
        !runtime.contains("MsgFn for unsafe extern \"C\" fn("),
        "MsgFn must have NO impl for the nounwind spelling"
    );
}

/// Whether the `extern "C" fn(` at `at` is the right-hand side of a type
/// alias or a `let` binding (`= extern "C" fn(` or `= unsafe extern "C" fn(`)
/// rather than a function definition.
fn is_alias_or_binding(src: &str, at: usize) -> bool {
    let line_start = src[..at].rfind('\n').map_or(0, |i| i + 1);
    let before = src[line_start..at].trim_end();
    let before = before.strip_suffix("unsafe").unwrap_or(before).trim_end();
    before.ends_with('=')
}

/// `(file, line)` of every `extern "C" fn(` type ALIAS or binding (`= extern
/// "C" fn(` / `= unsafe extern "C" fn(`) in the vendored winit macOS backend — the prototypes that are
/// `transmute`d rather than `msg()`-typed, which the send detector above
/// cannot see. Two of them sit on the most exposed raise path: `SendEvent`
/// (the prototype the ORIGINAL `-[NSApplication sendEvent:]` is called
/// through — AppKit's own routing raises behind it) and `ObserverCallBack`.
fn nounwind_transmuted_prototypes() -> Vec<String> {
    let mut found = Vec::new();
    let mut files = Vec::new();
    rust_files(
        &repo_root().join("vendor/winit/src/platform_impl/macos"),
        &mut files,
    );
    for file in files {
        let src = std::fs::read_to_string(&file).expect("readable source");
        let mut from = 0;
        while let Some(at) = src[from..].find("extern \"C\" fn(") {
            let start = from + at;
            from = start + 1;
            if on_comment_line(&src, start) || !is_alias_or_binding(&src, start) {
                continue;
            }
            let line = src[..start].matches('\n').count() + 1;
            found.push(format!("{}:{line}", file.display()));
        }
    }
    found
}

#[test]
fn no_transmuted_prototype_in_the_winit_backend_is_spelled_extern_c() {
    let found = nounwind_transmuted_prototypes();
    assert!(
        found.is_empty(),
        "these `type X = extern \"C\" fn(` / `let f: … = extern \"C\" fn(` prototypes in the \
         winit macOS backend are nounwind; spell them `extern \"C-unwind\"`:\n{}",
        found.join("\n")
    );
    // The two named rows, by name: a detector that only says "none found" is
    // not evidence that the two it exists for are still spelled right.
    let backend = repo_root().join("vendor/winit/src/platform_impl/macos");
    let app = std::fs::read_to_string(backend.join("app.rs")).expect("app.rs");
    // W12 re-typed it `unsafe extern "C-unwind" fn(Id, Sel, Id)` — the
    // `SwizzleSite` derives the row's encoding from that type, and its
    // `original()` chains to the displaced IMP through it, so the spelling
    // is the one the raise unwinds through.
    assert!(
        app.contains("type SendEvent = unsafe extern \"C-unwind\" fn(Id, Sel, Id);"),
        "app.rs's `SendEvent` — the prototype the original `sendEvent:` is called through — \
         must be `unsafe extern \"C-unwind\" fn(Id, Sel, Id)`"
    );
    let observer = std::fs::read_to_string(backend.join("observer.rs")).expect("observer.rs");
    assert!(
        observer.contains("type ObserverCallBack =\n    extern \"C-unwind\" fn("),
        "observer.rs's `ObserverCallBack` must be `extern \"C-unwind\"`"
    );
}

/// `app.rs`'s `sendEvent:` override keeps the containment's GUARD ORDER:
/// `stop_app_on_panic` OUTSIDE, `aterm_objc::contain` INSIDE.
///
/// The order is measured, not stylistic. `@catch (id)` does not swallow a
/// Rust panic, so a panic crosses the containment and lands in winit's own
/// guard, which stores it in the event loop's `PanicInfo`, stops the app and
/// resumes the unwind on the main thread; the other way round, an
/// `NSException` raised beneath the panic guard would meet its `catch_unwind`
/// as a foreign exception and abort — the v0.72.0 shape. `objc_event_drive`'s
/// controls 1a and 1b prove the BEHAVIOUR on every gate run; this pins the
/// TEXT, because the override was re-ported once already (W12 moved it onto
/// `SwizzleSite` on a branch cut before the containment landed) and a merge
/// that inverted the two guards would compile, pass the encoding checks and
/// only be found by the driver. Refuse it here, before it is run.
#[test]
fn the_send_event_override_keeps_stop_app_on_panic_outside_and_contain_inside() {
    let app =
        std::fs::read_to_string(repo_root().join("vendor/winit/src/platform_impl/macos/app.rs"))
            .expect("app.rs");
    let start = app
        .find("unsafe extern \"C-unwind\" fn send_event(app: Id, cmd: Sel, event: Id) {")
        .expect("app.rs declares the sendEvent: IMP as `unsafe extern \"C-unwind\" fn send_event`");
    let body = &app[start..];
    let at = |needle: &str, what: &str| -> usize {
        body.find(needle)
            .unwrap_or_else(|| panic!("app.rs's send_event lost {what}: no `{needle}`"))
    };
    let contained = at(
        "let contained = AssertUnwindSafe(",
        "the closure the panic guard receives",
    );
    let contain = at(
        "aterm_objc::contain(\"sendEvent:\"",
        "the containment named for the row",
    );
    let stop = at(
        "stop_app_on_panic(mtm, panic_info, contained)",
        "the panic guard, handed that closure",
    );
    let abort = at(
        "abort_on_unwind(\"sendEvent:\")",
        "the named abort for a panic with no event loop installed",
    );
    // The closure closes before the match that hands it to the guard, and the
    // contain call sits INSIDE it: `contained < contain < close < stop`.
    let close = contained
        + body[contained..]
            .find("\n    });")
            .expect("the `contained` closure closes at the top level");
    assert!(
        contained < contain && contain < close,
        "aterm_objc::contain(\"sendEvent:\" must sit inside the `contained` closure \
         (closure opens at {contained}, contain at {contain}, closure closes at {close})"
    );
    assert!(
        close < stop && stop < abort,
        "stop_app_on_panic must receive the closed `contained` closure, OUTSIDE the \
         containment, with the no-event-loop abort arm beside it \
         (close {close}, stop {stop}, abort {abort})"
    );
    // And no second containment wraps the guard — the order is not restated
    // the other way round further down the same function.
    assert_eq!(
        body[..abort].matches("aterm_objc::contain(").count(),
        1,
        "exactly one containment in the sendEvent: override, and it is the inner one"
    );
}

/// No third-party block remains on a raise's path.
///
/// `block2 0.5`'s `invoke` is `unsafe extern "C" fn` — nounwind, with rustc's
/// abort-on-unwind shim — and it was MEASURED (a review probe against this
/// tree, release) that an `NSException` raised below such a frame aborts with
/// `panic in a function that cannot unwind` EVEN WITH a containment outside
/// it, and terminates through libc++abi with nothing outside. The three sites
/// that used it (winit's queued closure, aterm-gui's alert key monitor and
/// its paste-sheet completion handler) are [`aterm_objc::RcBlock`] now, whose
/// `invoke` contains. This keeps them there: the name may appear in prose,
/// never in code, in any file on the census.
#[test]
fn no_third_party_block_remains_on_a_raise_path() {
    let name = concat!("block", "2");
    let mut found = Vec::new();
    for file in census_files() {
        let src = std::fs::read_to_string(&file).expect("readable source");
        for (i, line) in src.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if code.contains(&format!("{name}::")) || code.contains(&format!("use {name}")) {
                found.push(format!("{}:{}: {}", file.display(), i + 1, line.trim()));
            }
        }
    }
    assert!(
        found.is_empty(),
        "third-party block constructors on a raise's path:\n{}",
        found.join("\n")
    );
}

/// The `objc2` sends that remain on a raise's path, counted, with a ceiling.
///
/// They cannot be re-spelled from here: `objc-sys 0.3.5`'s
/// `extern_c_unwind!` expands to `extern "C"` unless its `unstable-c-unwind`
/// feature is on, and that feature (and `objc2 0.5`'s) turns on
/// `#![feature(c_unwind)]` — a NIGHTLY gate that the release's x86_64 compat
/// slice, built on upstream stable, refuses with E0554. So every method call
/// through an `objc2-app-kit`/`objc2-foundation` binding and every
/// `msg_send!` in these files is a `nounwind` send. MEASURED: a raise through
/// one is still CONTAINED at the trampoline above it, but the frame that made
/// the send silently skips its own `Drop`s — invariant (b) holds for every
/// frame EXCEPT one that sends via `objc2`. This pins the number of such
/// lines so it can only go down as the port proceeds; the policy paragraph in
/// `lib.rs` and §2 of the measured doc name the hazard.
/// The identifiers a file imports from the `objc2` family — the binding
/// types (`NSApplication`, `NSEvent`, `MainThreadMarker`, …) whose
/// associated-function calls are sends the path detector cannot see.
fn objc2_imports(src: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut from = 0;
    while let Some(at) = src[from..].find("use objc") {
        let start = from + at;
        from = start + 1;
        let Some(end) = src[start..].find(';') else {
            break;
        };
        let stmt = &src[start..start + end];
        let Some(rest) = stmt.strip_prefix("use ") else {
            continue;
        };
        let crate_name = rest.split("::").next().unwrap_or("");
        if !(crate_name == concat!("objc", "2")
            || crate_name == concat!("objc", "2_app_kit")
            || crate_name == concat!("objc", "2_foundation"))
        {
            continue;
        }
        let tail = &rest[crate_name.len()..];
        let mut ident = String::new();
        for ch in tail.chars().chain(std::iter::once(' ')) {
            if ch.is_alphanumeric() || ch == '_' {
                ident.push(ch);
            } else if !ident.is_empty() {
                // Module segments and macro names are not receivers.
                if !matches!(
                    ident.as_str(),
                    "self"
                        | "rc"
                        | "runtime"
                        | "declare_class"
                        | "msg_send"
                        | "msg_send_id"
                        | "mutability"
                        | "sel"
                        | "class"
                        | "as"
                ) && ident.chars().next().is_some_and(char::is_uppercase)
                {
                    names.push(std::mem::take(&mut ident));
                } else {
                    ident.clear();
                }
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

/// Whether `code` (a comment-stripped line) contains `name::` as a path head
/// — `NSApplication::sharedApplication(`, not `MyNSApplication::`.
fn has_path_head(code: &str, name: &str) -> bool {
    let mut from = 0;
    while let Some(at) = code[from..].find(name) {
        let start = from + at;
        from = start + 1;
        let before_ok = start == 0
            || !code[..start]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
        if before_ok && code[start + name.len()..].starts_with("::") {
            return true;
        }
    }
    false
}

/// The `objc2` sends that remain on a raise's path, counted, with a ceiling.
///
/// They cannot be re-spelled from here: `objc-sys 0.3.5`'s
/// `extern_c_unwind!` expands to `extern "C"` unless its `unstable-c-unwind`
/// feature is on, and that feature (and `objc2 0.5`'s) turns on
/// `#![feature(c_unwind)]` — a NIGHTLY gate that the release's x86_64 compat
/// slice, built on upstream stable, refuses with E0554. So every method call
/// through an `objc2-app-kit`/`objc2-foundation` binding and every
/// `msg_send!` in these files is a `nounwind` send. MEASURED: a raise through
/// one is still CONTAINED at the trampoline above it, but the frame that made
/// the send silently skips its own `Drop`s — invariant (b) holds for every
/// frame EXCEPT one that sends via `objc2`. This pins the number of such
/// lines so it can only go down as the port proceeds; the policy paragraph in
/// `lib.rs` and §2 of the measured doc name the hazard.
///
/// What is counted: a comment-stripped, non-`use` line that names an
/// `objc2` path (`objc2_app_kit::…`), a `msg_send!`/`msg_send_id!`, or an
/// imported binding type as a path head (`NSApplication::sharedApplication`).
/// A method call on a binding VALUE (`app.stop(None)`) is invisible to text
/// and is not counted; the number is a floor on the real one, and a ceiling
/// on itself.
#[test]
fn the_remaining_objc2_send_lines_only_go_down() {
    const CEILING: usize = OBJC2_SEND_LINES_RECORDED;
    let mut lines = 0usize;
    let mut files = Vec::new();
    for dir in [
        "vendor/winit/src/platform_impl/macos",
        "crates/aterm-gui/src",
    ] {
        rust_files(&repo_root().join(dir), &mut files);
    }
    let mut per_file = Vec::new();
    for file in files {
        let src = std::fs::read_to_string(&file).expect("readable source");
        let here = objc2_send_lines(&src);
        if here > 0 {
            per_file.push(format!("{here:4}  {}", file.display()));
            lines += here;
        }
    }
    // THE PORT IS COMPLETE (the W12 + W13 merge), so "found nothing" is the
    // expected answer and can no longer distinguish a finished port from a
    // broken detector. The detector is proven on a sample instead.
    assert_eq!(
        objc2_send_lines(
            "use objc2_app_kit::NSApplication;\n\
             let app = NSApplication::sharedApplication(mtm);\n\
             let alert: Retained<AnyObject> = msg_send_id![objc2_class!(NSAlert), new];\n\
             let _: () = msg_send![&alert, setMessageText: &*title];\n\
             // NSApplication::sharedApplication(mtm) in prose\n\
             let x = aterm_objc::send::send_id(app, sel!(keyWindow));\n"
        ),
        3,
        "the objc2 send-line detector no longer sees the old spellings"
    );
    // The ceiling reached ZERO with the W12 + W13 merge, so "at most" and
    // "exactly" are the same test now, and the exact form is the one clippy
    // accepts for a comparison against a type's minimum.
    assert_eq!(
        lines,
        CEILING,
        "{lines} lines send through objc2 (nounwind) on a raise's path; the recorded ceiling is \
         {CEILING}. New sends belong on aterm_objc; the port is complete.\n{}",
        per_file.join("\n")
    );
    eprintln!(
        "objc2 send lines on the raise path: {lines} (ceiling {CEILING})\n{}",
        per_file.join("\n")
    );
}

/// The `objc2` send lines in one source: a comment-stripped, non-`use` line
/// that names an `objc2` path, a `msg_send!`/`msg_send_id!`, or an imported
/// binding type as a path head.
fn objc2_send_lines(src: &str) -> usize {
    let paths = [
        concat!("objc", "2_app_kit::"),
        concat!("objc", "2_foundation::"),
        concat!("objc", "2::"),
        "msg_send!",
        "msg_send_id!",
    ];
    let imports = objc2_imports(src);
    let mut here = 0usize;
    for line in src.lines() {
        let code = line.split("//").next().unwrap_or("");
        let t = code.trim_start();
        if t.starts_with("use ") || t.starts_with("pub use ") || t.starts_with("pub(crate) use ") {
            continue;
        }
        if paths.iter().any(|f| code.contains(f)) || imports.iter().any(|n| has_path_head(code, n))
        {
            here += 1;
        }
    }
    here
}

/// The ceiling for [`the_remaining_objc2_send_lines_only_go_down`]: 146 when
/// containment landed (2026-09-04); ZERO since the W12 + W13 merge (2026-09-05)
/// took the last `objc2` send out of the winit macOS backend and `aterm-gui`.
/// It cannot rise: a new `objc2` send on a raise path is a `nounwind` frame
/// whose destructors a raise skips.
const OBJC2_SEND_LINES_RECORDED: usize = 0;

#[test]
fn the_detector_sees_the_old_spelling() {
    // A detector that finds nothing must be shown finding something.
    let sample = "let f: unsafe extern \"C\" fn(Id, Sel) -> Id = msg();";
    let at = sample.find("extern \"C\" fn(").expect("the pattern");
    let stmt = &sample[at..sample.find(';').unwrap()];
    assert!(stmt.contains("msg()"));
    assert!(!on_comment_line(sample, at));
    let commented = "    // let f: unsafe extern \"C\" fn(Id, Sel) -> Id = msg();";
    let at = commented.find("extern \"C\" fn(").expect("the pattern");
    assert!(
        on_comment_line(commented, at),
        "prose about the old spelling is not a prototype"
    );
    // The transmuted-prototype detector, on the two shapes it exists for.
    let alias = "type SendEvent = extern \"C\" fn(&NSApplication, Sel, &NSEvent);";
    let at = alias.find("extern \"C\" fn(").expect("the alias pattern");
    assert!(!on_comment_line(alias, at) && is_alias_or_binding(alias, at));
    let alias = "type SendEvent = unsafe extern \"C\" fn(Id, Sel, Id);";
    let at = alias
        .find("extern \"C\" fn(")
        .expect("the unsafe alias pattern");
    assert!(
        is_alias_or_binding(alias, at),
        "the `unsafe` alias shape is matched"
    );
    let definition = "unsafe extern \"C\" fn send_event(app: Id, cmd: Sel, event: Id) {";
    assert!(
        definition.find("extern \"C\" fn(").is_none(),
        "a definition is not an alias"
    );
    let unwind = "type SendEvent = unsafe extern \"C-unwind\" fn(Id, Sel, Id);";
    assert!(
        unwind.find("extern \"C\" fn(").is_none(),
        "the new spelling is not matched"
    );
}
