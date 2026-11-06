// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! `build-path-value`: a source or build path is a run-time fact, never a compiled-in value.
//!
//! Cargo hands each compile the absolute paths of its checkout and build directory
//! (`CARGO_MANIFEST_DIR`, `CARGO_BIN_EXE_*`, `OUT_DIR`, `CARGO_TARGET_TMPDIR`, …). An `env!` or
//! `option_env!` of one bakes that path into the unit, yet Cargo leaves the variables it sets out
//! of the unit's freshness, so a build directory reused at another path (a second checkout, a
//! seeded one) runs the unit against the first checkout's files and helpers. Inside `include!`,
//! `include_str!` or `include_bytes!` the path only names a file whose content dep-info tracks,
//! so the compiled bytes do not depend on where the checkout is; everywhere else the value is read
//! at run time (`aterm_tempfile::manifest_dir`, `bin_exe`, `target_tmpdir`).

use super::rules::PRODUCT;
use super::{Bound, Finding, Rule, Tree, hits, is_word};

pub(super) const BUILD_PATH_VALUE: Rule = Rule {
    id: "build-path-value",
    why: "an `env!`/`option_env!` of a path Cargo sets (`CARGO_MANIFEST_DIR`, `CARGO_BIN_EXE_*`, \
          `OUT_DIR`, `CARGO_TARGET_TMPDIR`, …) compiles this checkout's absolute path into the \
          unit, and Cargo does not rebuild it when that path moves, so another checkout or a \
          seeded build directory reads the first one's files and runs its helpers; read it at run \
          time (`aterm_tempfile::manifest_dir`, `bin_exe`, `target_tmpdir`), or name the file in \
          `include!`/`include_str!`/`include_bytes!`, whose content dep-info tracks",
    scope: &[],
    check: build_path_value,
};

/// The path variables Cargo sets for a compile, exactly.
const PATH_VARS: &[&str] = &[
    "CARGO_MANIFEST_DIR",
    "CARGO_MANIFEST_PATH",
    "CARGO_TARGET_TMPDIR",
    "CARGO_RUSTC_CURRENT_DIR",
    "OUT_DIR",
];

/// The path variables Cargo sets for a compile, by prefix: a package's binaries, and an artifact
/// dependency's directory and files.
const PATH_VAR_PREFIXES: &[&str] = &[
    "CARGO_BIN_EXE_",
    "CARGO_BIN_DIR_",
    "CARGO_BIN_FILE_",
    "CARGO_CDYLIB_DIR_",
    "CARGO_CDYLIB_FILE_",
    "CARGO_STATICLIB_DIR_",
    "CARGO_STATICLIB_FILE_",
];

/// The macros whose path argument names a file the compile reads, tracked by dep-info.
const INCLUDES: &[&str] = &["include", "include_str", "include_bytes"];

fn is_path_var(name: &str) -> bool {
    PATH_VARS.contains(&name) || PATH_VAR_PREFIXES.iter().any(|p| name.starts_with(p))
}

fn build_path_value(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    // Every name the rule reads, as a prefix: a file that spells none reads none.
    const NEEDLES: &[&str] = &[
        "CARGO_MANIFEST_",
        "CARGO_TARGET_TMPDIR",
        "CARGO_RUSTC_CURRENT_DIR",
        "OUT_DIR",
        "CARGO_BIN_",
        "CARGO_CDYLIB_",
        "CARGO_STATICLIB_",
    ];
    for file in tree.corpus.under(PRODUCT) {
        if !file.mentions(NEEDLES) {
            continue;
        }
        let lexed = file.lexed();
        for (at, macro_name, var) in path_reads(&lexed.code, &lexed.text) {
            if inside_include(&lexed.code, at) {
                continue;
            }
            let line = lexed.code[..at].bytes().filter(|&b| b == b'\n').count() + 1;
            out.push(file.finding(
                BUILD_PATH_VALUE.id,
                line,
                format!(
                    "`{macro_name}!(\"{var}\")` compiles a path into the unit; read it at run time"
                ),
            ));
        }
    }
}

/// Every `env!("<path var>")` and `option_env!("<path var>")` in a file: the macro name's offset
/// in the CODE view, the macro, and the variable read from the TEXT view.
fn path_reads<'a>(code: &'a str, text: &'a str) -> Vec<(usize, &'static str, &'a str)> {
    let mut found = Vec::new();
    for macro_name in ["env", "option_env"] {
        for at in hits(code, macro_name, Bound::Word) {
            let Some(var) = literal_argument(code, text, at + macro_name.len()) else {
                continue;
            };
            if is_path_var(var) {
                found.push((at, macro_name, var));
            }
        }
    }
    found.sort_unstable_by_key(|&(at, _, _)| at);
    found
}

/// The first argument of the macro whose name ends at `after`, when it is a string literal:
/// `!`, an opening bracket, then `"…"` or a raw `r#"…"#`.
fn literal_argument<'a>(code: &str, text: &'a str, after: usize) -> Option<&'a str> {
    let rest = code[after..].trim_start().strip_prefix('!')?.trim_start();
    let rest = rest.strip_prefix(['(', '[', '{'])?.trim_start();
    let rest = rest
        .strip_prefix('r')
        .map_or(rest, |r| r.trim_start_matches('#'));
    let rest = rest.strip_prefix('"')?;
    let open = code.len() - rest.len();
    // The CODE view blanks a literal's contents, so its next quote closes it.
    let len = code[open..].find('"')?;
    text.get(open..open + len)
}

/// Is the offset `at` inside the arguments of `include!`, `include_str!` or `include_bytes!`, at
/// any depth (`include_str!(concat!(env!(…), "…"))`)? Each enclosing opening bracket is read in
/// turn, outward, in the CODE view (whose literals and comments hold no bracket).
fn inside_include(code: &str, at: usize) -> bool {
    let bytes = code.as_bytes();
    let mut depth = 0usize;
    let mut i = at;
    while i > 0 {
        i -= 1;
        match bytes[i] {
            b')' | b']' | b'}' => depth += 1,
            b'(' | b'[' | b'{' => {
                if depth > 0 {
                    depth -= 1;
                    continue;
                }
                if INCLUDES.contains(&macro_before(code, i)) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// The macro whose invocation opens with the bracket at `open` (`name!(`), or `""` when the
/// bracket opens no macro call.
fn macro_before(code: &str, open: usize) -> &str {
    let head = code[..open].trim_end();
    let Some(head) = head.strip_suffix('!') else {
        return "";
    };
    let head = head.trim_end();
    let start = head.bytes().rposition(|b| !is_word(b)).map_or(0, |p| p + 1);
    &head[start..]
}

#[cfg(test)]
mod tests {
    use super::super::tests::{run, spots};
    use super::*;

    /// NEGATIVE CONTROL: each value read fails, whatever its variable, macro, bracket or depth;
    /// the same names inside an include, a string, a comment, or a run-time read do not.
    #[test]
    fn a_build_path_is_read_at_run_time_or_named_in_an_include() {
        let src = r##"
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures");
fn helper() -> std::process::Command {
    std::process::Command::new(env!("CARGO_BIN_EXE_aterm-lab"))
}
fn tmp() -> &'static str { env!["CARGO_TARGET_TMPDIR"] }
fn out() -> Option<&'static str> { option_env!("OUT_DIR") }
fn raw() -> &'static str { std::env!(r#"CARGO_MANIFEST_PATH"#) }
fn wrapped() -> String { format!("{}/x", env!("CARGO_MANIFEST_DIR")) }
static TEXT: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/data.txt"));
static BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/data.bin"
));
include!(concat!(env!("OUT_DIR"), "/generated.rs"));
#[doc = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md"))]
struct Documented;
const NOTE: &str = "env!(\"CARGO_MANIFEST_DIR\") in a string is no read";
// env!("CARGO_BIN_EXE_x") in a comment is no read
fn runtime() -> std::path::PathBuf { aterm_tempfile::manifest_dir() }
fn other() -> &'static str { env!("CARGO_PKG_NAME") }
fn lookalike() -> &'static str { my_env!("CARGO_MANIFEST_DIR") }
"##;
        let found = run(&BUILD_PATH_VALUE, &[("crates/x/src/lib.rs", src)]);
        assert_eq!(
            spots(&found),
            [
                "crates/x/src/lib.rs:2",
                "crates/x/src/lib.rs:4",
                "crates/x/src/lib.rs:6",
                "crates/x/src/lib.rs:7",
                "crates/x/src/lib.rs:8",
                "crates/x/src/lib.rs:9",
            ],
            "{found:#?}"
        );
        assert!(found[0].detail.contains("env!(\"CARGO_MANIFEST_DIR\")"));
        assert!(found[3].detail.contains("option_env!(\"OUT_DIR\")"));
    }

    /// Test code, a test target, an example and a build script bake the path exactly as shipped
    /// code does, so the rule reads them all.
    #[test]
    fn test_code_and_dev_targets_are_read_too() {
        let read = "fn f() -> &'static str { env!(\"CARGO_MANIFEST_DIR\") }\n";
        let found = run(
            &BUILD_PATH_VALUE,
            &[
                ("crates/x/src/a_tests.rs", read),
                ("crates/x/tests/it.rs", read),
                ("crates/x/examples/e.rs", read),
                ("crates/x/build.rs", read),
                ("apps/y/src/main.rs", read),
            ],
        );
        assert_eq!(found.len(), 5, "{found:#?}");
    }

    #[test]
    fn the_enclosing_macro_is_read_from_its_bracket() {
        let code = "let a = include_str!(concat!(x, y));";
        let open = code.find("concat").unwrap() + "concat!".len();
        assert_eq!(macro_before(code, open), "concat");
        let outer = code.find("include_str").unwrap() + "include_str!".len();
        assert_eq!(macro_before(code, outer), "include_str");
        assert_eq!(macro_before("f(", 1), "");
        assert!(inside_include(code, code.find('x').unwrap()));
        assert!(!inside_include("let a = concat!(x);", 16));
    }
}
