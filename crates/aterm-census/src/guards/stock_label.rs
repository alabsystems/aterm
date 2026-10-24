// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! stock-label: code that runs on a stock Rust toolchain carries its label.
//!
//! AGENTS.md allows stock Rust only where Trust cannot do the job, labelled `STOCK EXCEPTION`
//! or `PUBLIC BOUNDARY`, and no lint names that rule. Three shapes are stock by construction:
//!
//! - a cargo-fuzz target (cargo-fuzz and its sanitizer runtime are stock nightly's);
//! - shipped code that selects a stock channel by a literal: rustup's `"+nightly"` override as
//!   one argument, or `"RUSTUP_TOOLCHAIN"` set to `"stable"`;
//! - a Cargo target whose code builds only for a target Trust ships no std for (Trust ships its
//!   host's std alone, [`TRUST_HOST`]): a target root gated whole
//!   (`#![cfg(target_os = "linux")]`), or one whose `fn main` keeps an arm for every target but
//!   that one (`#[cfg(not(windows))]`). A library module or a test-suite member gated to Linux
//!   belongs to a target Trust builds, so it is not one. The Linux and Windows lanes
//!   (`aterm-lab`'s guests, `verify cells`) carry the label where they select the stock compiler.
//!
//! Each file of these shapes carries a label in a comment; a label only inside a string (an
//! error message) labels no code. A remedy that names a stock command to a person runs nothing.

use std::collections::BTreeSet;

use super::rules::{PRODUCT, test_path};
use super::{Finding, Line, Rule, SourceFile, Tree, is_word, joined, mod_decl};

pub(super) const STOCK_LABEL: Rule = Rule {
    id: "stock-label",
    why: "code that runs on a stock Rust toolchain (a cargo-fuzz target, a `+nightly` argument, \
          `RUSTUP_TOOLCHAIN` set to `stable`, a target that builds only for a target Trust \
          ships no std for) carries a `STOCK EXCEPTION` or `PUBLIC BOUNDARY` comment saying why \
          Trust cannot do the job (AGENTS.md)",
    scope: &[],
    check: stock_label,
};

/// What the one target Trust ships a std for (aarch64-apple-darwin, the host its releases
/// carry) answers to the `cfg` names that fix a target. A predicate these make false holds
/// only on a target Trust has no std for; `unix` and `windows` are `target_family`'s.
const TRUST_HOST: &[(&str, &str)] = &[
    ("target_os", "macos"),
    ("target_family", "unix"),
    ("target_arch", "aarch64"),
    ("target_vendor", "apple"),
];

/// The labels AGENTS.md names.
const LABELS: &[&str] = &["STOCK EXCEPTION", "PUBLIC BOUNDARY"];

/// The stock channels rustup selects by name.
const CHANNELS: &[&str] = &["stable", "beta", "nightly"];

/// The literal that names the environment variable whose value picks the toolchain.
const TOOLCHAIN_VAR: &str = "\"RUSTUP_TOOLCHAIN\"";

fn stock_label(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let members: BTreeSet<String> = tree
        .corpus
        .under(PRODUCT)
        .filter(|file| target_root(&file.rel) && file.mentions(&["mod "]))
        .flat_map(members)
        .collect();
    for file in tree.corpus.under(PRODUCT) {
        let found = if file.rel.contains("/fuzz/fuzz_targets/") {
            Some((0, "a cargo-fuzz target runs on stock nightly".to_string()))
        } else if let Some(gate) = (target_root(&file.rel) && !members.contains(&file.rel))
            .then(|| foreign_target(file))
            .flatten()
        {
            Some(gate)
        } else if test_path(&file.rel) || !file.mentions(CHANNELS) {
            None
        } else {
            selector(file).map(|(no, seen)| (no, format!("selects a stock toolchain: `{seen}`")))
        };
        if let Some((no, seen)) = found.filter(|_| !labelled(file)) {
            out.push(file.finding(
                STOCK_LABEL.id,
                no,
                format!("{seen}, and no comment labels it `STOCK EXCEPTION` or `PUBLIC BOUNDARY`"),
            ));
        }
    }
}

/// The first shipped line that selects a stock channel, and what selects it.
fn selector(file: &SourceFile) -> Option<(usize, String)> {
    let lines: Vec<Line<'_>> = file.shipped_lines().collect();
    for (at, line) in lines.iter().enumerate() {
        if let Some(seen) = in_literal(line) {
            return Some((line.no, seen));
        }
        if let Some(from) = line.text.find(TOOLCHAIN_VAR) {
            let rest: String = std::iter::once(&line.text[from + TOOLCHAIN_VAR.len()..])
                .chain(lines[at + 1..].iter().take(2).map(|l| l.text))
                .collect::<Vec<_>>()
                .join(" ");
            let value = rest
                .trim_start()
                .strip_prefix(',')
                .unwrap_or("")
                .trim_start();
            if let Some(channel) = value.strip_prefix('"').and_then(channel_at) {
                return Some((line.no, format!("RUSTUP_TOOLCHAIN = \"{channel}\"")));
            }
        }
    }
    None
}

/// A literal that is rustup's `+<channel>` override, whole (`"+nightly"`,
/// `"+nightly-2026-01-01"`; not the prose `"rustup +stable target add"`).
fn in_literal(line: &Line<'_>) -> Option<String> {
    for (at, _) in line.text.match_indices("\"+") {
        if line.code.as_bytes().get(at + 1) != Some(&b' ') {
            continue;
        }
        let word = &line.text[at + 2..];
        let whole =
            |channel: &&str| matches!(word.as_bytes().get(channel.len()), Some(b'"' | b'-'));
        if let Some(channel) = channel_at(word).filter(whole) {
            return Some(format!("\"+{channel}\""));
        }
    }
    None
}

/// The channel `s` opens with, when the name ends there (`nightly`, `nightly-2026-01-01`; not
/// `stable_count`).
fn channel_at(s: &str) -> Option<&'static str> {
    CHANNELS.iter().copied().find(|channel| {
        s.strip_prefix(channel)
            .is_some_and(|rest| rest.bytes().next().is_none_or(|b| !is_word(b)))
    })
}

/// Is `rel` where Cargo finds the root file of a target: a library or binary (`src/lib.rs`,
/// `src/main.rs`, `src/bin/`), a test, a bench or an example? A file there that another root
/// declares as a module is that target's member instead ([`members`]).
fn target_root(rel: &str) -> bool {
    let parts: Vec<&str> = rel.split('/').collect();
    match parts.as_slice() {
        [.., "src", "lib.rs" | "main.rs"]
        | [.., "src", "bin", _, "main.rs"]
        | [.., "tests" | "benches" | "examples", _, "main.rs"] => true,
        [.., "src", "bin", file] | [.., "tests" | "benches" | "examples", file] => {
            file.ends_with(".rs")
        }
        _ => false,
    }
}

/// What makes a target root stock by construction, as its line and a description: a gate on
/// the whole file that holds only off Trust's host, else a `#[cfg(not(P))]` arm of its
/// `fn main` whose `P` holds only off Trust's host (the arm every other target, Trust's host
/// among them, compiles in place of the work).
fn foreign_target(file: &SourceFile) -> Option<(usize, String)> {
    if !file.mentions(&["cfg("]) {
        return None;
    }
    let lines: Vec<Line<'_>> = file.lines().collect();
    let attrs = attributes(&lines);
    let foreign = |pred: &str| on_trust(pred) == Some(false);
    if let Some((attr, pred)) = attrs
        .iter()
        .filter(|a| a.file)
        .find_map(|a| a.cfg().filter(|p| foreign(p)).map(|p| (a, p)))
    {
        return Some((
            attr.no,
            format!(
                "a Cargo target that builds only for `{}`, which Trust ships no std for",
                tidy(pred)
            ),
        ));
    }
    let mains = mains(&lines, &attrs);
    attrs.iter().filter(|a| !a.inner).find_map(|a| {
        let negated = a.cfg()?.strip_prefix("not(")?.strip_suffix(')')?;
        (foreign(negated) && mains.iter().any(|m| m.contains(&a.at))).then(|| {
            (
                a.no,
                format!(
                    "a Cargo target whose `fn main` does its work only for `{}`, which Trust \
                     ships no std for",
                    tidy(negated)
                ),
            )
        })
    })
}

/// One attribute (`#[..]` or `#![..]`), read over every line rustfmt wrapped it across.
struct Attr {
    /// The index of its first line among the file's lines, and of its last.
    at: usize,
    end: usize,
    /// Its first line's number.
    no: usize,
    /// `#![..]`; `file` when it stands at column 0, so it is the file's own.
    inner: bool,
    file: bool,
    /// Its TEXT inside the brackets (`cfg(not(windows))`), each run of whitespace one space.
    body: String,
}

impl Attr {
    /// The predicate of a `cfg` attribute.
    fn cfg(&self) -> Option<&str> {
        self.body
            .strip_prefix("cfg(")?
            .strip_suffix(')')
            .map(str::trim)
    }
}

/// Every attribute that opens a line, in file order. The brackets are counted in the CODE view,
/// where a literal's contents are blank, and the body is read from the TEXT view beside it.
fn attributes(lines: &[Line<'_>]) -> Vec<Attr> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let code = lines[i].code;
        let from = code.len() - code.trim_start().len();
        let inner = code[from..].starts_with("#![");
        if !inner && !code[from..].starts_with("#[") {
            i += 1;
            continue;
        }
        let mut body = String::new();
        let mut depth = 0usize;
        let (mut j, mut start) = (i, from);
        let end = 'read: loop {
            let (code, text) = (lines[j].code.as_bytes(), lines[j].text);
            for (k, byte) in code.iter().enumerate().skip(start) {
                match byte {
                    b'[' => depth += 1,
                    b']' => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            body.push_str(&text[start..k]);
                            break 'read Some(j);
                        }
                    }
                    _ => {}
                }
            }
            body.push_str(&text[start..]);
            body.push(' ');
            (j, start) = (j + 1, 0);
            if j == lines.len() {
                break None;
            }
        };
        let Some(end) = end else { break };
        let open = if inner { "#![" } else { "#[" };
        out.push(Attr {
            at: i,
            end,
            no: lines[i].no,
            inner,
            file: inner && from == 0,
            body: body
                .trim_start()
                .strip_prefix(open)
                .unwrap_or_default()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
        });
        i = end + 1;
    }
    out
}

/// The outer attributes standing on the item at line index `at`, nearest first (lines blank in
/// the CODE view, a comment's, may sit between them).
fn chain<'a>(lines: &[Line<'_>], attrs: &'a [Attr], at: usize) -> Vec<&'a Attr> {
    let mut out = Vec::new();
    let mut first = at;
    loop {
        let mut above = first;
        while above > 0 && lines[above - 1].code.trim().is_empty() {
            above -= 1;
        }
        match attrs.iter().find(|a| !a.inner && a.end + 1 == above) {
            Some(attr) => {
                out.push(attr);
                first = attr.at;
            }
            None => return out,
        }
    }
}

/// The lines of each top-level `fn main`, from its first attribute through its closing brace, as
/// ranges of line indices.
fn mains(lines: &[Line<'_>], attrs: &[Attr]) -> Vec<std::ops::RangeInclusive<usize>> {
    let mut out = Vec::new();
    for (at, line) in lines.iter().enumerate() {
        if !opens_main(line.code) {
            continue;
        }
        let first = chain(lines, attrs, at).last().map_or(at, |attr| attr.at);
        let last = if line.code.trim_end().ends_with('}') {
            at
        } else {
            lines[at + 1..]
                .iter()
                .position(|l| l.code.starts_with('}'))
                .map_or(lines.len() - 1, |p| at + 1 + p)
        };
        out.push(first..=last);
    }
    out
}

/// The files a target root's top-level `mod x;` declarations name (`x.rs` or `x/mod.rs` beside
/// it, or its `#[path]`): members of that target, not targets of their own, as each test of a
/// crate's suite is under `autotests = false`.
fn members(root: &SourceFile) -> Vec<String> {
    let dir = root.rel.rsplit_once('/').map_or("", |(dir, _)| dir);
    let lines: Vec<Line<'_>> = root.lines().collect();
    let attrs = attributes(&lines);
    let mut out = Vec::new();
    for (at, line) in lines.iter().enumerate() {
        if line.code.starts_with([' ', '\t']) {
            continue;
        }
        let Some(name) = mod_decl(line.code) else {
            continue;
        };
        let path = chain(&lines, &attrs, at).into_iter().find_map(|attr| {
            let value = attr
                .body
                .strip_prefix("path")?
                .trim_start()
                .strip_prefix('=')?;
            value.trim().strip_prefix('"')?.strip_suffix('"')
        });
        match path {
            Some(path) => out.push(joined(dir, path)),
            None => out.extend([format!("{dir}/{name}.rs"), format!("{dir}/{name}/mod.rs")]),
        }
    }
    out
}

/// Does this CODE line open a top-level `fn main` (`fn main(`, `pub fn main(`, `pub(crate) fn
/// main(`)?
fn opens_main(code: &str) -> bool {
    let rest = match code.strip_prefix("pub") {
        Some(vis) if vis.starts_with('(') => vis.split_once(')').map_or("", |(_, r)| r),
        Some(vis) if vis.starts_with(' ') => vis,
        Some(_) => return false,
        None => return code.starts_with("fn main("),
    };
    rest.trim_start().starts_with("fn main(")
}

/// `pred` (a `cfg` predicate) on Trust's host: `Some(false)` when no build there compiles what
/// it gates, `None` when the answer turns on a name the host does not fix (`feature`, `test`,
/// `debug_assertions`).
fn on_trust(pred: &str) -> Option<bool> {
    let pred = pred.trim();
    if let Some(inner) = pred.strip_prefix("not(").and_then(|r| r.strip_suffix(')')) {
        return on_trust(inner).map(|holds| !holds);
    }
    for (head, any) in [("all(", false), ("any(", true)] {
        if let Some(inner) = pred.strip_prefix(head).and_then(|r| r.strip_suffix(')')) {
            let mut open = false;
            for child in crate::split_top_level_commas(inner) {
                match on_trust(child) {
                    Some(holds) if holds == any => return Some(any),
                    Some(_) => {}
                    None => open = true,
                }
            }
            return (!open).then_some(!any);
        }
    }
    let (name, value) = match pred.split_once('=') {
        Some((name, value)) => (
            name.trim(),
            value.trim().strip_prefix('"')?.strip_suffix('"')?,
        ),
        None if pred == "unix" || pred == "windows" => ("target_family", pred),
        None => return None,
    };
    TRUST_HOST
        .iter()
        .find(|(fixed, _)| *fixed == name)
        .map(|(_, host)| *host == value)
}

/// A predicate as rustfmt writes it on one line: no space inside a parenthesis, no trailing
/// comma.
fn tidy(pred: &str) -> String {
    pred.replace("( ", "(")
        .replace(" )", ")")
        .replace(",)", ")")
}

/// Does a comment of `file` carry a label (the text view blank where the raw file has it)?
fn labelled(file: &SourceFile) -> bool {
    if !file.mentions(LABELS) {
        return false;
    }
    file.raw.lines().zip(file.lines()).any(|(raw, line)| {
        LABELS.iter().any(|label| {
            raw.match_indices(label).any(|(at, _)| {
                line.text
                    .get(at..at + label.len())
                    .is_some_and(|seen| seen.trim().is_empty())
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::super::tests::{run, spots};
    use super::*;

    const UNLABELLED_FUZZ: &str = "#![no_main]\nfuzz_target!(|data: &[u8]| { let _ = data; });\n";

    #[test]
    fn stock_code_without_a_label_is_found() {
        let labelled_fuzz =
            format!("//! STOCK EXCEPTION: Trust ships no cargo-fuzz.\n{UNLABELLED_FUZZ}");
        let env = "fn f(cmd: &mut Cmd) {\n    cmd.env(\"RUSTUP_TOOLCHAIN\", \"stable\");\n}\n";
        let wrapped = "fn f(cmd: &mut Cmd) {\n    cmd.env(\n        \"RUSTUP_TOOLCHAIN\",\n        \
                       \"nightly-2026-01-01\",\n    );\n}\n";
        let argv = "fn f(cmd: &mut Cmd) {\n    cmd.args([\"+nightly\", \"fuzz\", \"run\"]);\n}\n";
        let in_string = "fn f(cmd: &mut Cmd) {\n    cmd.env(\"RUSTUP_TOOLCHAIN\", \"beta\");\n    \
                         panic!(\"needs stock beta (STOCK EXCEPTION)\");\n}\n";
        let labelled = "// PUBLIC BOUNDARY: the compat slice builds on stable.\n\
                        fn f(cmd: &mut Cmd) {\n    cmd.env(\"RUSTUP_TOOLCHAIN\", \"stable\");\n}\n";
        let not_stock = "fn f(cmd: &mut Cmd, t: &str) {\n    \
                         cmd.env(\"RUSTUP_TOOLCHAIN\", t);\n    \
                         let n = a +stable_count;\n    \
                         // cargo +nightly fuzz run decode\n    \
                         let s = \"runs stable\";\n    \
                         let fix = \"rustup +stable target add x86_64-apple-darwin\";\n    \
                         let fix = \"rustup run stable cargo check\";\n}\n\
                         #[cfg(test)]\nmod tests {\n    \
                         fn t() { run(&[\"+nightly\", \"metadata\"]); }\n}\n";
        let found = run(
            &STOCK_LABEL,
            &[
                (
                    "crates/aterm-a/fuzz/fuzz_targets/decode.rs",
                    UNLABELLED_FUZZ,
                ),
                (
                    "crates/aterm-b/fuzz/fuzz_targets/decode.rs",
                    labelled_fuzz.as_str(),
                ),
                ("crates/aterm-c/src/env.rs", env),
                ("crates/aterm-c/src/wrapped.rs", wrapped),
                ("crates/aterm-c/src/argv.rs", argv),
                ("crates/aterm-c/src/in_string.rs", in_string),
                ("crates/aterm-c/src/labelled.rs", labelled),
                ("crates/aterm-c/src/not_stock.rs", not_stock),
                ("crates/aterm-c/src/lane_tests.rs", env),
            ],
        );
        assert_eq!(
            spots(&found),
            [
                "crates/aterm-a/fuzz/fuzz_targets/decode.rs:0",
                "crates/aterm-c/src/argv.rs:2",
                "crates/aterm-c/src/env.rs:2",
                "crates/aterm-c/src/in_string.rs:2",
                "crates/aterm-c/src/wrapped.rs:3",
            ]
        );
        assert_eq!(
            found[2].detail,
            "selects a stock toolchain: `RUSTUP_TOOLCHAIN = \"stable\"`, and no comment labels it \
             `STOCK EXCEPTION` or `PUBLIC BOUNDARY`"
        );
    }

    #[test]
    fn a_target_with_no_work_on_trusts_host_is_found() {
        let whole =
            "//! A live bus test.\n\n#![cfg(target_os = \"linux\")]\n\n#[test]\nfn t() {}\n";
        let whole_labelled = format!("//! STOCK EXCEPTION: Linux only.\n{whole}");
        let bench = "#[path = \"support.rs\"]\nmod support;\n\n#[cfg(target_os = \"linux\")]\n\
                     mod live {\n    pub fn run() {}\n}\n\nfn main() {\n    \
                     #[cfg(target_os = \"linux\")]\n    live::run();\n    \
                     #[cfg(not(target_os = \"linux\"))]\n    eprintln!(\"requires Linux\");\n}\n";
        let per_os_main = "#[cfg(windows)]\nfn main() {}\n\n#[cfg(not(windows))]\nfn main() {\n    \
                           eprintln!(\"Windows only\");\n}\n";
        let wrapped = "fn main() {\n    let ran = {\n        \
                       #[cfg(all(windows, debug_assertions))]\n        {\n            1\n        }\n        \
                       #[cfg(not(all(\n            windows,\n            debug_assertions,\n        )))]\n        \
                       {\n            0\n        }\n    };\n    let _ = ran;\n}\n";
        let feature_gated = "#![cfg(all(target_os = \"linux\", feature = \"x\"))]\nfn main() {}\n";
        let lane_bus = "fn main() {\n    #[cfg(unix)]\n    {\n        run();\n    }\n    \
                        #[cfg(not(unix))]\n    {\n        eprintln!(\"unix only\");\n    }\n}\n";
        let portable_main = "fn main() {\n    #[cfg(not(target_os = \"macos\"))]\n    \
                             eprintln!(\"macOS only\");\n    run(&[]);\n}\n\n\
                             fn run(args: &[u8]) {\n    #[cfg(windows)]\n    windows(args);\n    \
                             #[cfg(not(windows))]\n    let _ = args;\n}\n";
        let module = "#![cfg(target_os = \"linux\")]\npub fn f() {}\n";
        let open = "#![cfg(any(target_os = \"linux\", feature = \"x\"))]\n#[test]\nfn t() {}\n";
        let unix = "#![cfg(unix)]\n#[test]\nfn t() {}\n";
        let nested = "mod w {\n    #![cfg(windows)]\n    pub fn f() {}\n}\n\n\
                      mod lane {\n    pub fn main() {\n        \
                      #[cfg(not(target_os = \"linux\"))]\n        eprintln!(\"x\");\n    }\n}\n\n\
                      fn main() {\n    lane::main();\n}\n";
        let in_string = "const S: &str = \"#![cfg(windows)]\";\nfn main() {\n    \
                         let s = \"#[cfg(not(windows))]\";\n}\n";
        let suite =
            "mod live;\n#[path = \"gated.rs\"]\n#[cfg(unix)]\nmod renamed;\n#[test]\nfn t() {}\n";
        let found = run(
            &STOCK_LABEL,
            &[
                ("crates/aterm-d/tests/live_bus.rs", whole),
                (
                    "crates/aterm-d/tests/live_labelled.rs",
                    whole_labelled.as_str(),
                ),
                ("crates/aterm-d/benches/accept.rs", bench),
                ("crates/aterm-d/benches/per_os_main.rs", per_os_main),
                ("crates/aterm-d/benches/wrapped.rs", wrapped),
                ("crates/aterm-d/examples/feature_gated.rs", feature_gated),
                ("crates/aterm-d/src/bin/lane-bus/main.rs", lane_bus),
                ("crates/aterm-d/src/main.rs", portable_main),
                ("crates/aterm-d/src/linux.rs", module),
                ("crates/aterm-d/tests/open.rs", open),
                ("crates/aterm-d/tests/unix.rs", unix),
                ("crates/aterm-d/examples/nested.rs", nested),
                ("crates/aterm-d/benches/in_string.rs", in_string),
                ("crates/aterm-e/tests/suite.rs", suite),
                ("crates/aterm-e/tests/live.rs", whole),
                ("crates/aterm-e/tests/gated.rs", whole),
                ("crates/aterm-e/tests/alone.rs", whole),
            ],
        );
        assert_eq!(
            spots(&found),
            [
                "crates/aterm-d/benches/accept.rs:12",
                "crates/aterm-d/benches/per_os_main.rs:4",
                "crates/aterm-d/benches/wrapped.rs:7",
                "crates/aterm-d/examples/feature_gated.rs:1",
                "crates/aterm-d/tests/live_bus.rs:3",
                "crates/aterm-e/tests/alone.rs:3",
            ]
        );
        assert_eq!(
            found[2].detail,
            "a Cargo target whose `fn main` does its work only for `all(windows, \
             debug_assertions)`, which Trust ships no std for, and no comment labels it `STOCK \
             EXCEPTION` or `PUBLIC BOUNDARY`"
        );
        assert_eq!(
            found[4].detail,
            "a Cargo target that builds only for `target_os = \"linux\"`, which Trust ships no std \
             for, and no comment labels it `STOCK EXCEPTION` or `PUBLIC BOUNDARY`"
        );
    }

    #[test]
    fn a_predicate_is_false_on_trusts_host_only_when_the_host_fixes_every_name_it_turns_on() {
        for (pred, holds) in [
            ("target_os = \"linux\"", Some(false)),
            ("target_os = \"macos\"", Some(true)),
            ("windows", Some(false)),
            ("unix", Some(true)),
            ("not(windows)", Some(true)),
            ("not(unix)", Some(false)),
            ("all(windows, debug_assertions)", Some(false)),
            ("all(unix, debug_assertions)", None),
            (
                "any(target_os = \"linux\", target_os = \"macos\")",
                Some(true),
            ),
            ("any(target_os = \"linux\", feature = \"x\")", None),
            (
                "all(target_arch = \"wasm32\", target_os = \"unknown\")",
                Some(false),
            ),
            ("target_arch = \"x86_64\"", Some(false)),
            ("target_family = \"wasm\"", Some(false)),
            ("target_vendor = \"apple\"", Some(true)),
            ("all()", Some(true)),
            ("any()", Some(false)),
            ("test", None),
            ("target_os = linux", None),
        ] {
            assert_eq!(on_trust(pred), holds, "{pred}");
        }
    }
}
