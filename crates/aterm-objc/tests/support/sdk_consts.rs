// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ONE `_Static_assert` PROBE behind both constant censuses:
//! `winit_seam_constants.rs` (the vendored fork's seam) and
//! `gui_appkit_constants.rs` (`aterm-gui`'s `consts`). The two files are one
//! instrument over two subjects — same parser grammar, same both-arches probe,
//! same verdicts — so the grammar, the compiler call and the verdicts live
//! here once, and each file keeps its own table, its own scope and its own
//! probe source.

use std::process::Command;

/// A `const` declaration's body, whatever VISIBILITY it is spelled with.
///
/// # This function is the fourteenth pass's finding
///
/// It was `line.trim().strip_prefix("pub(crate) const ")`, and all 42 rows in
/// the seam happen to be spelled that way. **That armed the guard at one
/// SPELLING of its rule rather than at the rule.** `pub const` inside the
/// seam's private `consts` module is the same declaration with the same
/// visibility in practice — arguably the more idiomatic spelling, since the
/// module is already `pub(crate)` — and a `pub const` carrying a WRONG value
/// was planted here and passed all three tests in the seam's file, including
/// the coverage test whose entire job is to say that every seam constant has a
/// row.
///
/// That is F1's shape, recurring inside the fix for the constant-recipe gap:
/// the previous pass found a `load_borrowed` rebuilt with its lifetime behind a
/// `PhantomData` wrapper slipping through a guard armed at `&`. The question to
/// ask of any new guard is what the same defect looks like spelled differently,
/// so this parser is written against the GRAMMAR — `pub`, `pub(crate)`,
/// `pub(super)`, `pub(in path)` or nothing — and
/// [`the_parser_reads_every_visibility_spelling`] proves it reads all of them.
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

/// Compile `body` as Objective-C for `arch`, syntax only, in a fresh scratch
/// directory labelled with `scratch`, `tag` and `arch` and removed afterwards
/// (the two censuses run concurrently and share tags). `Ok(())` if clang
/// accepted it, `Err(stderr)` if it did not.
///
/// A `cc` that cannot even be started panics: a missing compiler is a broken
/// box, and an oracle that cannot run on a broken box is not a pass. A `cc`
/// that runs but cannot fold a `static const` is a narrower case with its own
/// rule, on one measured host only: [`green_verdict`] and [`inverted_verdict`].
pub fn compile(scratch: &str, arch: &str, body: &str, tag: &str) -> Result<(), String> {
    let dir = aterm_tempfile::Builder::new()
        .prefix(&format!("{scratch}-{tag}-{arch}-"))
        .tempdir()
        .expect("a scratch directory");
    let path = dir.path().join("c.m");
    std::fs::write(&path, body).expect("the probe is writable");
    let out = Command::new("cc")
        .args(["-arch", arch, "-fsyntax-only"])
        // A wrong-but-vacuous assertion WARNS before it passes, so THAT
        // warning is made fatal — and only that one. A blanket `-Werror` was
        // tried first and broke the seam's green arm on an unrelated
        // diagnostic: `NSWindowFullScreenButton` is deprecated, so the whole
        // probe failed for a reason that has nothing to do with any constant's
        // value. `-Wparentheses` is the exact diagnostic clang emits for
        // `SDK == a | b`, which is the shape both probes parenthesise against.
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

/// The diagnostic Apple clang 14.0.3 gives for every `static const` row —
/// `NSNotFound`, `NSModalResponseOK`, `NSAppKitVersionNumber10_12`,
/// `NSVariableStatusItemLength`, `NSAlertFirstButtonReturn`,
/// `NSWindowFullScreenButton` — when asked to fold it into `_Static_assert`.
/// It is the HOST compiler's limit, not the target's: the fold fails for both
/// `-arch` arms on that clang (measured 2026-09-06 and again 2026-09-16 on an
/// Intel MacBook Pro running macOS 13.7.8 with the Command Line Tools' clang
/// 14.0.3; the enum rows fold fine). A newer clang folds them all.
#[cfg(all(target_arch = "x86_64", target_os = "macos"))]
const FOLD_DIAGNOSTIC: &str = "static_assert expression is not an integral constant expression";

/// The rows clang could not fold, in the order it named them — `Some` only
/// when EVERY `error:` line in `stderr` is [`FOLD_DIAGNOSTIC`]. Any other
/// error (a value disagreement reads `static assertion failed due to
/// requirement`), or no `error:` line at all, is `None`: not classified, so
/// the caller keeps its verdict. The row is the quoted name on the source line
/// clang prints under the diagnostic.
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

/// x86_64 macOS — the one host measured with a clang that cannot fold a
/// `static const` (see [`FOLD_DIAGNOSTIC`]): a compile whose every error is that
/// diagnostic is REPORTED, one stderr line naming the rows, and is not a
/// failure; every other row in the same probe was asserted by the same
/// compile. An error that is not the fold diagnostic, or an unclassifiable
/// failure, panics exactly as on every other target. The waiver keys on clang's
/// text, never on the cfg: an Intel Mac with a folding clang asserts every
/// row, and an Apple silicon Mac with clang 14.0.3 stays red by design — only
/// this host was measured.
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

/// The inverted arm's verdict: the probe with one row flipped must FAIL for
/// that row's VALUE on `arch`. On every target but x86_64 macOS any failure is
/// the pass, as it always was.
#[cfg(not(all(target_arch = "x86_64", target_os = "macos")))]
#[track_caller]
pub fn inverted_verdict(arch: &str, row: &str, result: Result<(), String>, vacuous: &str) {
    let _ = (arch, row);
    assert!(result.is_err(), "{vacuous}");
}

/// x86_64 macOS: a clang that cannot fold makes the inverted probe fail
/// whatever the values are, so "it failed" proves nothing here. The pass is
/// clang's value diagnostic — `static_assert failed due to requirement …` (`static assertion` on a newer clang) —
/// on the inverted `row` itself; the fold errors on the other rows are reported
/// and never counted.
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

/// The one line an x86_64 macOS run prints per probe with unfolded rows: which
/// rows, which clang, and why they are not asserted here. Written to the
/// stderr handle rather than through `eprintln!`, which libtest captures and
/// drops for a passing test: a waived row must show in every run. The line
/// names the census's own file: every caller is `#[track_caller]` back to the
/// test that ran the probe.
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

/// THE PARSER READS EVERY VISIBILITY SPELLING, not just the one either subject
/// uses.
///
/// Every live declaration in both subjects is spelled `pub(crate) const`, so
/// without this [`strip_const_decl`] could regress to a single `strip_prefix`
/// and every other test in both censuses would stay green: the coverage tests
/// cannot notice a constant they never parsed — a row they do not see is not
/// "missing", it is invisible. So the spellings are exercised against a
/// synthetic source rather than against either subject; it runs once, in the
/// `objc_static_suite` binary both censuses share.
#[test]
fn the_parser_reads_every_visibility_spelling() {
    let cases = [
        ("const A: usize = 1;", Some(("A", "1"))),
        ("pub const B: usize = 2;", Some(("B", "2"))),
        ("    pub(crate) const C: usize = 3;", Some(("C", "3"))),
        ("pub(super) const D: usize = 4;", Some(("D", "4"))),
        ("pub(in crate::x) const E: usize = 5;", Some(("E", "5"))),
        // …and things that are NOT a constant declaration stay unparsed.
        ("let id = (r as *const T).cast_mut();", None),
        ("pub(super) static F: Id;", None),
        ("pubconst G: usize = 7;", None),
    ];
    for (line, want) in cases {
        let got = parse_const_decl(line);
        let got = got.as_ref().map(|(n, v)| (n.as_str(), v.as_str()));
        assert_eq!(got, want, "the parser disagreed on {line:?}");
    }
}
