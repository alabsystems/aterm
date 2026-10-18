// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `_Static_assert` probe behind both constant censuses, `winit_seam_constants.rs` (the
//! vendored fork's seam) and `gui_appkit_constants.rs` (`aterm-gui`'s `consts`): the parser
//! grammar, the compiler call and the verdicts live here once; each census keeps its own table,
//! scope and probe source.
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use std::process::Command;

/// A `const` declaration's body, at any visibility (`pub`, `pub(crate)`, `pub(super)`,
/// `pub(in path)` or none): a parser that matched one spelling would let a constant spelled
/// another way pass every census test unparsed.
fn strip_const_decl(line: &str) -> Option<&str> {
    let mut s = line.trim();
    if let Some(rest) = s.strip_prefix("pub") {
        // `pub(crate)`, `pub(super)`, `pub(in crate::x)` — skip the qualifier,
        // then the whitespace before `const`.
        s = match rest.strip_prefix('(') {
            Some(q) => q.split_once(')')?.1,
            None => rest,
        };
        // A bare `pub` must be followed by whitespace, or this matched the
        // prefix of some other identifier (`pubfoo`).
        if !s.is_empty() && !s.starts_with(char::is_whitespace) {
            return None;
        }
        s = s.trim_start();
    }
    s.strip_prefix("const ")
}

/// `(name, literal)` of a `const` declaration line, at any visibility; `None`
/// for any other line. The literal is the Rust spelling, `;` dropped.
pub fn parse_const_decl(line: &str) -> Option<(String, String)> {
    let rest = strip_const_decl(line)?;
    let (name, rest) = rest.split_once(':').expect("a typed const");
    let (_ty, value) = rest.split_once('=').expect("an initialised const");
    Some((
        name.trim().to_owned(),
        value.trim().trim_end_matches(';').trim().to_owned(),
    ))
}

/// A Rust integer literal as C: only `_` separators differ (`0x7fff_ffff…`).
pub fn as_c_literal(rust: &str) -> String {
    rust.replace('_', "")
}

/// Compile `body` as Objective-C for `arch`, syntax only, in a fresh scratch directory labelled
/// with `scratch`, `tag` and `arch` and removed afterwards (the two censuses run concurrently
/// and share tags). `Ok(())` if clang accepted it, `Err(stderr)` if not.
///
/// A `cc` that cannot start panics: an oracle that cannot run is not a pass. A `cc` that cannot
/// fold a `static const` is [`green_verdict`]'s and [`inverted_verdict`]'s case.
pub fn compile(scratch: &str, arch: &str, body: &str, tag: &str) -> Result<(), String> {
    let dir = aterm_tempfile::Builder::new()
        .prefix(&format!("{scratch}-{tag}-{arch}-"))
        .tempdir()
        .expect("a scratch directory");
    let path = dir.path().join("c.m");
    std::fs::write(&path, body).expect("the probe is writable");
    let out = Command::new("cc")
        .args(["-arch", arch, "-fsyntax-only"])
        // A wrong-but-vacuous `SDK == a | b` assertion warns `-Wparentheses`, so that one
        // warning is fatal; a blanket `-Werror` fails the probe on unrelated deprecations
        // (`NSWindowFullScreenButton`).
        .arg("-Werror=parentheses")
        .arg(&path)
        .output()
        .expect(
            "`cc` must be runnable: this test IS the oracle that holds these \
             constants to the SDK, and a compiler that cannot even start is a \
             broken box, not a pass",
        );
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

/// The diagnostic Apple clang 14.0.3 (Command Line Tools on an Intel Mac, macOS 13) gives for
/// every `static const` row (`NSNotFound`, `NSModalResponseOK`, `NSVariableStatusItemLength`, …)
/// asked to fold into `_Static_assert`. It is the host compiler's limit, on both `-arch` arms;
/// enum rows fold fine, and a newer clang folds them all.
#[cfg(all(target_arch = "x86_64", target_os = "macos"))]
const FOLD_DIAGNOSTIC: &str = "static_assert expression is not an integral constant expression";

/// The rows clang could not fold, in the order it named them: `Some` only when every `error:`
/// line in `stderr` is [`FOLD_DIAGNOSTIC`]. Any other error (a value disagreement), or no
/// `error:` line, is `None` and the caller keeps its verdict. The row is the quoted name on the
/// source line clang prints under the diagnostic.
#[cfg(all(target_arch = "x86_64", target_os = "macos"))]
fn unfolded_rows(stderr: &str) -> Option<Vec<String>> {
    let lines: Vec<&str> = stderr.lines().collect();
    let mut rows = Vec::new();
    let mut saw_error = false;
    for (i, line) in lines.iter().enumerate() {
        if !line.contains("error:") {
            continue;
        }
        saw_error = true;
        if !line.contains(FOLD_DIAGNOSTIC) {
            return None;
        }
        let row = lines
            .iter()
            .skip(i + 1)
            .take(3)
            .find_map(|l| {
                let (_, rest) = l.split_once('"')?;
                let (name, _) = rest.split_once('"')?;
                Some(name.to_owned())
            })
            .unwrap_or_else(|| "<a row clang did not print>".to_owned());
        rows.push(row);
    }
    saw_error.then_some(rows)
}

/// The green arm's verdict on `compile`'s answer — `assert!`-shaped: a clean
/// compile is the pass, anything else is `what` on `arch` with clang's text.
/// On every target but x86_64 macOS that is the whole function.
#[cfg(not(all(target_arch = "x86_64", target_os = "macos")))]
#[track_caller]
pub fn green_verdict(arch: &str, what: &str, result: Result<(), String>) {
    if let Err(stderr) = result {
        panic!("{what} on {arch}:\n{stderr}");
    }
}

/// x86_64 macOS, the one host measured with a clang that cannot fold a `static const` (see
/// [`FOLD_DIAGNOSTIC`]): a compile whose every error is that diagnostic is reported in one
/// stderr line naming the rows, not failed; every other row was asserted by the same compile.
/// Any other error panics as on every other target. The waiver keys on clang's text: an Intel
/// Mac with a folding clang asserts every row.
#[cfg(all(target_arch = "x86_64", target_os = "macos"))]
#[track_caller]
pub fn green_verdict(arch: &str, what: &str, result: Result<(), String>) {
    let Err(stderr) = result else {
        return;
    };
    match unfolded_rows(&stderr) {
        Some(rows) => report_unfolded(arch, &rows),
        None => panic!("{what} on {arch}:\n{stderr}"),
    }
}

/// The inverted arm's verdict: the probe with one row flipped must fail for that row's value on
/// `arch`. On every target but x86_64 macOS any failure is the pass.
#[cfg(not(all(target_arch = "x86_64", target_os = "macos")))]
#[track_caller]
pub fn inverted_verdict(arch: &str, row: &str, result: Result<(), String>, vacuous: &str) {
    let _ = (arch, row);
    assert!(result.is_err(), "{vacuous}");
}

/// x86_64 macOS: a clang that cannot fold makes the inverted probe fail whatever the values
/// are, so the pass is clang's value diagnostic (`static_assert failed due to requirement …`,
/// `static assertion` on a newer clang) on the inverted `row` itself; fold errors on the other
/// rows are reported and never counted.
#[cfg(all(target_arch = "x86_64", target_os = "macos"))]
#[track_caller]
pub fn inverted_verdict(arch: &str, row: &str, result: Result<(), String>, vacuous: &str) {
    let Err(stderr) = result else {
        panic!("{vacuous}");
    };
    let lines: Vec<&str> = stderr.lines().collect();
    let failed_for_its_value = lines.iter().enumerate().any(|(i, l)| {
        l.contains("failed due to requirement")
            && lines
                .iter()
                .skip(i)
                .take(4)
                .any(|s| s.contains(&format!("\"{row}\"")))
    });
    assert!(
        failed_for_its_value,
        "the INVERTED row {row} did not fail for its value on {arch} — this clang's fold \
         errors alone would make any inverted probe fail, which proves nothing:\n{stderr}"
    );
    let fold_only: Vec<String> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.contains(FOLD_DIAGNOSTIC))
        .filter_map(|(i, _)| {
            lines.iter().skip(i + 1).take(3).find_map(|l| {
                let (_, rest) = l.split_once('"')?;
                let (name, _) = rest.split_once('"')?;
                Some(name.to_owned())
            })
        })
        .collect();
    if !fold_only.is_empty() {
        report_unfolded(arch, &fold_only);
    }
}

/// The one line an x86_64 macOS run prints per probe with unfolded rows. It goes to the stderr
/// handle, not `eprintln!` (which libtest drops for a passing test), so a waived row shows in
/// every run; `#[track_caller]` makes it name the census's own file.
#[cfg(all(target_arch = "x86_64", target_os = "macos"))]
#[track_caller]
fn report_unfolded(arch: &str, rows: &[String]) {
    use std::io::Write as _;
    let version = Command::new("cc")
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "unknown".to_owned());
    let line = format!(
        "{}: {arch}: {} row(s) not asserted on x86_64 macOS — this host's `cc` ({version}) \
         cannot fold their `static const` into `_Static_assert` (the fold fails for both \
         -arch arms on Apple clang 14.0.3, measured 2026-09-16); every other row was \
         asserted by the same compile: {}\n",
        std::panic::Location::caller().file(),
        rows.len(),
        rows.join(", ")
    );
    let _ = std::io::stderr().write_all(line.as_bytes());
}
