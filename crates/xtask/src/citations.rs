// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `xtask gate citations` — A CLAIM WITH NO WITNESS.
//!
//! # The class
//!
//! 2026-09-17 produced four separate cases of a document confidently describing
//! behaviour the code does not have, each of which cost real time at the worst
//! moment — in the middle of shipping v0.87.0:
//!
//! * `docs/RELEASING.md`'s headline release sequence named `pub promote`, which
//!   cannot run without a destination passed on the command line, so the
//!   documented one-liner failed at its second step every time it was typed.
//! * `.cargo/config.toml` asserted `tools/verify.sh` was the Trust campaign's
//!   strict driver; every stage in `crates/aterm-verify/src/stages.rs` passes
//!   `--unverified`, so nothing in the repository ran the proof lane at all.
//! * `publish/config.sh` asserted its public-clone gate ran
//!   `publish/trust-source-gate.sh`, in a file whose own next paragraph records
//!   that `publish/` is force-excluded from the export and therefore cannot be
//!   reached from the clone.
//! * `crates/aterm-update-core/src/pins.rs` sent the operator to delete a
//!   tripwire assertion (`the_shipped_anchor_is_unset_so_the_tier_is_inert`)
//!   that had been deleted in August — the instruction outlived the test it
//!   named — and a retired-key table named a consumer crate that no longer
//!   existed.
//!
//! That is a class, not four accidents, and the repository already has the two
//! gates that bracket it: `gate help-surfaces` hashes help prose and refuses
//! until a human re-reads it against its handler, and `gate drift` proves an
//! advertised capability has an implementation witness. What neither can see is
//! prose that CITES SOMETHING BY NAME as its proof. Three of the four cases
//! above cited a file, a test or a crate that no longer exists, and a gate that
//! merely RESOLVES such citations would have caught all three on the day they
//! rotted — for the cost of a directory walk.
//!
//! # What is checked, and what deliberately is not
//!
//! Only citations whose truth is decidable by lookup. This gate has no opinion
//! on whether prose is *right*; it asks the one question a machine can answer:
//! **does the thing this sentence names exist?**
//!
//! * **[`Rule::Path`]** — a backticked repo path ([`PATH_ROOTS`], or a
//!   `crates/`-relative spelling like `aterm-release/src/sign.rs`) must resolve
//!   in the tree. For a `.rs` file, a citation is also tried relative to the
//!   citing crate, so `tests/plist_stamp.rs` in `crates/aterm-release/src`
//!   resolves.
//! * **[`Rule::Name`]** — a backticked identifier of at least
//!   [`NAME_MIN_SEGMENTS`] snake_case segments — the shape of a test name, which
//!   is why this rule is cheap and precise — must be DEFINED somewhere in the
//!   tree: a Rust `fn`/`const`/`static`/`struct`/field, a shell function, or a
//!   Python `def`. A four-word name that is defined nowhere is almost always a
//!   test that was renamed or deleted under the sentence citing it.
//!
//! Not checked, on purpose: bare words, two-word identifiers (`build_app`
//! collides with English), external crate paths, and anything outside
//! [`ROSTER_DIRS`]/[`ROSTER_FILES`]. Precision is worth more than recall in a
//! gate that REFUSES: one false red teaches people to add waivers, and a
//! waiver list is how a gate becomes decoration.
//!
//! # The two escapes, and why they are not holes
//!
//! A citation legitimately names something that is not in the tree in exactly
//! two situations, and each has to SAY SO in the prose — which is the same edit
//! that makes the sentence useful to a reader:
//!
//! 1. **The thing is gone, and that is the point.** "Port of the retired
//!    `apps/aterm-mac/build-app.sh`", "`verify_index` was retired with the
//!    package-specific root". [`ABSENCE_MARKERS`] in the surrounding
//!    [`CONTEXT_LINES`] admits it. This is not a keyword escape hatch bolted on
//!    for convenience: an absence claim is itself a claim, and stating it is
//!    strictly better prose than a dangling pointer.
//! 2. **The thing lives in another tree.** trust's `targo`, the publication
//!    engine's `bin/pub`. [`FOREIGN_MARKERS`] admits it — and the refusal says
//!    so, because "name the repository you are citing" is the fix that helps the
//!    next reader find it.
//!
//! There is deliberately **no waiver file and no environment opt-out.** The
//! roster is green today with zero exceptions (the run prints how many citations
//! it resolved, so a roster that silently stopped checking anything is visible),
//! and the moment an exception list exists, the cheapest way past a red gate is
//! to append to it.
//!
//! # Growing the roster
//!
//! [`ROSTER_DIRS`] is the release and packaging surface — the pipeline whose
//! documentation failing is what this gate was built after. The repo-wide sweep
//! that produced it found the same rot outside that surface (seven `docs/*.md`
//! files cited by "See X for complete coverage" that exist nowhere; a
//! `.github/workflows/*.yml` cited by a test in a repo whose standing rule is
//! that there is NO CI). Those are other lanes' files. Add a directory here when
//! its citations resolve; do not add one with a waiver.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::workspace_root;

/// Directories whose every prose-bearing file is checked, recursively.
///
/// The release + packaging pipeline: the cutter, the package manager, the key
/// tool, the updater (both halves), and the publication policy this repo
/// commits. These are the files an operator reads WHILE A RELEASE IS FAILING,
/// which is the worst possible moment to be reading a sentence that is not true.
const ROSTER_DIRS: &[&str] = &[
    "crates/aterm-release/src",
    "crates/aterm-release/tests",
    "crates/atpkg/src",
    "crates/atpkg-keys/src",
    "crates/aterm-update/src",
    "crates/aterm-update-core/src",
    "publish",
];

/// Individual rostered files outside [`ROSTER_DIRS`].
const ROSTER_FILES: &[&str] = &[
    "docs/RELEASING.md",
    ".cargo/config.toml",
    "tools/check-release-shape.sh",
    "tools/atpkg-publish.sh",
    "tools/atpkg-publish-lib.sh",
    "tools/atpkg-index.sh",
    "tools/atpkg-pack.sh",
    "tools/atpkg-pack-bundle.sh",
    "tools/atpkg-mirror-public.sh",
];

/// Top-level directories a repo-path citation may start with. A token that
/// starts with one of these and contains a `/` is a claim about this tree.
const PATH_ROOTS: &[&str] = &[
    "crates/", "docs/", "tools/", "publish/", "apps/", "assets/", "scripts/", "vendor/", ".cargo/",
    ".github/", ".claude/", ".codex/",
];

/// Crate-name prefixes that make `<crate>/src/x.rs` a `crates/`-relative
/// citation — the spelling half this repository's prose uses.
const CRATE_PREFIXES: &[&str] = &["aterm", "atpkg", "trust", "xtask"];

/// Directory segments under a crate that a `crates/`-relative citation may name.
const CRATE_SUBDIRS: &[&str] = &[
    "src/",
    "tests/",
    "benches/",
    "examples/",
    "assets/",
    "proofs/",
];

/// Paths this repository GENERATES rather than commits. A citation of one is a
/// claim about a runtime artifact, not about the tree, and no walk of a clean
/// checkout can resolve it — `publish/.out/` is the publication engine's
/// gitignored work area, where `pub stage` leaves the attestation. Kept short
/// and explicit: every entry is a place the repo's own `.gitignore` covers.
const RUNTIME_PREFIXES: &[&str] = &["publish/.out/"];

/// Snake_case segments a name citation needs before it is checked. Three
/// underscores (four words) is the measured floor: below it, ordinary field and
/// variable names dominate and the rule stops being about proofs.
const NAME_MIN_SEGMENTS: usize = 4;

/// Lines of surrounding context searched for an escape marker — enough to reach
/// the start of a wrapped sentence in an 80-column doc comment.
const CONTEXT_LINES: usize = 3;

/// Prose that ADMITS the cited thing is not in the tree. Lowercase; matched
/// case-insensitively as substrings.
const ABSENCE_MARKERS: &[&str] = &[
    "retired",
    "deleted",
    "removed",
    "is gone",
    "are gone",
    "was gone",
    "gone from",
    "gone with",
    "no longer",
    "never existed",
    "never was",
    "defined nowhere",
    "nowhere in the tree",
    "not in this tree",
    "does not exist",
    "former",
    "formerly",
    "used to",
    "replaced",
    "supersed",
    "ported from",
    "port of",
    "the old ",
    "legacy",
    "would be",
    "if it existed",
];

/// Prose that names ANOTHER tree as the citation's owner. A citation admitted by
/// one of these is out of this gate's reach by construction — no walk of this
/// repository can resolve it.
const FOREIGN_MARKERS: &[&str] = &[
    "~/",
    "trust's",
    "targo",
    "trustc",
    "publication engine",
    "~/publication",
    "in trust",
    "upstream",
    "cargo's",
    "rustup",
    "another repo",
    "another tree",
    "outside this repo",
];

/// Which rule a violation broke — the refusal names it, so the remedy is
/// specific rather than "something here is wrong".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Rule {
    /// A backticked repo path that resolves to nothing.
    Path,
    /// A backticked test-shaped identifier that is defined nowhere.
    Name,
}

impl Rule {
    fn tag(self) -> &'static str {
        match self {
            Self::Path => "PATH",
            Self::Name => "NAME",
        }
    }

    fn remedy(self) -> &'static str {
        match self {
            Self::Path => {
                "point it at a path that exists, say it is gone (\"the retired \
                 `x`\", \"deleted with …\"), or name the repository it lives in"
            }
            Self::Name => {
                "name the item that exists now, say this one is gone (\"defined \
                 nowhere in the tree\", \"retired with …\"), or name the \
                 repository it lives in"
            }
        }
    }
}

/// One unresolved citation.
#[derive(Clone, Debug)]
pub(crate) struct Violation {
    pub(crate) rule: Rule,
    pub(crate) file: String,
    pub(crate) line: usize,
    pub(crate) cited: String,
    pub(crate) text: String,
}

/// `xtask gate citations`.
pub(crate) fn gate_citations() -> bool {
    let (ok, log) = citations_report(&workspace_root(), ROSTER_DIRS, ROSTER_FILES);
    eprint!("{log}");
    ok
}

/// The verb over an arbitrary root and roster, so a fixture can plant a
/// violation in a temp tree and watch the REAL reporting function go red.
pub(crate) fn citations_report(root: &Path, dirs: &[&str], files: &[&str]) -> (bool, String) {
    let mut log = String::new();
    let _ = writeln!(log, "=== gate citations (a claim with no witness) ===");

    let defined = defined_names(root);
    let mut rostered: Vec<PathBuf> = Vec::new();
    let mut missing_roster: Vec<String> = Vec::new();
    for f in files {
        let p = root.join(f);
        if p.is_file() {
            rostered.push(p);
        } else {
            missing_roster.push((*f).to_string());
        }
    }
    for d in dirs {
        let p = root.join(d);
        if p.is_dir() {
            walk_prose(&p, &mut rostered);
        } else {
            missing_roster.push((*d).to_string());
        }
    }
    rostered.sort();

    let mut checked = 0usize;
    let mut violations: Vec<Violation> = Vec::new();
    for path in &rostered {
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let Ok(src) = std::fs::read_to_string(path) else {
            continue;
        };
        let lines: Vec<&str> = src.lines().collect();
        let rusty = rel.ends_with(".rs");
        for (idx, line) in lines.iter().enumerate() {
            if rusty && !is_comment_line(line) {
                continue;
            }
            let ctx = context(&lines, idx);
            let absent = has_marker(&ctx, ABSENCE_MARKERS);
            let foreign = has_marker(&ctx, FOREIGN_MARKERS);
            for cited in backticked(line) {
                let Some(rule) = classify(&cited) else {
                    continue;
                };
                checked += 1;
                let resolved = match rule {
                    Rule::Path => resolves_path(root, &rel, &cited),
                    Rule::Name => defined.contains(&cited),
                };
                if resolved || absent {
                    continue;
                }
                if foreign {
                    continue;
                }
                violations.push(Violation {
                    rule,
                    file: rel.clone(),
                    line: idx + 1,
                    cited,
                    text: line.trim().chars().take(140).collect(),
                });
            }
        }
    }

    for m in &missing_roster {
        let _ = writeln!(
            log,
            "  ROSTER {m}: rostered here but absent from the tree — a roster that \
             names nothing checks nothing"
        );
    }
    for v in &violations {
        let _ = writeln!(
            log,
            "  {} {}:{}  `{}` resolves to nothing\n      {}\n      remedy: {}",
            v.rule.tag(),
            v.file,
            v.line,
            v.cited,
            v.text,
            v.rule.remedy()
        );
    }

    let ok = violations.is_empty() && missing_roster.is_empty();
    if ok {
        let _ = writeln!(
            log,
            "gate citations: GREEN — {checked} citation(s) resolved over {} rostered file(s).",
            rostered.len()
        );
    } else {
        let _ = writeln!(
            log,
            "gate citations: FAILED — {} unresolved citation(s) and {} missing roster \
             entr(y/ies) over {checked} checked.",
            violations.len(),
            missing_roster.len()
        );
    }
    (ok, log)
}

/// Every prose-bearing file under `dir`, recursively. `.out/` (the publication
/// engine's gitignored work area) and build output are never prose.
fn walk_prose(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if matches!(name.as_str(), ".out" | "target" | "target.noindex" | ".git") {
                continue;
            }
            walk_prose(&path, out);
        } else if is_prose_file(&name) {
            out.push(path);
        }
    }
}

/// Extension test for a prose-bearing file. An extension-less file in `publish/`
/// is a script (`after-cut`, `post-promote`) and carries the same prose.
fn is_prose_file(name: &str) -> bool {
    if name.starts_with('.') {
        return false;
    }
    match name.rsplit_once('.') {
        Some((_, ext)) => matches!(ext, "rs" | "md" | "sh" | "toml" | "py" | "txt"),
        None => true,
    }
}

/// `//`, `/*`, or a `*` continuation line.
fn is_comment_line(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("//") || t.starts_with("/*") || t.starts_with('*')
}

/// [`CONTEXT_LINES`] before through one after, lowercased, joined.
fn context(lines: &[&str], idx: usize) -> String {
    let lo = idx.saturating_sub(CONTEXT_LINES);
    let hi = (idx + 2).min(lines.len());
    lines[lo..hi].join(" ").to_lowercase()
}

fn has_marker(ctx: &str, markers: &[&str]) -> bool {
    markers.iter().any(|m| ctx.contains(m))
}

/// Every `` `token` `` on the line.
fn backticked(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(open) = rest.find('`') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('`') else { break };
        let tok = &after[..close];
        if !tok.is_empty() && !tok.contains(' ') {
            out.push(tok.to_string());
        }
        rest = &after[close + 1..];
    }
    out
}

/// Which rule, if any, a token is subject to. `None` means "not a citation this
/// gate can decide" — the majority of backticked tokens, and deliberately so.
fn classify(tok: &str) -> Option<Rule> {
    let t = tok.trim_end_matches(&['.', ',', ';', ':'][..]);
    if t.is_empty() {
        return None;
    }
    if t.contains('/') {
        if RUNTIME_PREFIXES.iter().any(|r| t.starts_with(r)) {
            return None;
        }
        if PATH_ROOTS.iter().any(|r| t.starts_with(r)) {
            return Some(Rule::Path);
        }
        if is_crate_relative(t) {
            return Some(Rule::Path);
        }
        return None;
    }
    if t.contains("::") || t.contains('.') || t.contains('!') || t.contains('<') {
        return None;
    }
    let segs: Vec<&str> = t.split('_').collect();
    if segs.len() < NAME_MIN_SEGMENTS {
        return None;
    }
    if !t.starts_with(|c: char| c.is_ascii_lowercase()) {
        return None;
    }
    if !t
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return None;
    }
    if segs.iter().any(|s| s.is_empty()) {
        return None;
    }
    Some(Rule::Name)
}

/// `aterm-release/src/sign.rs` — a `crates/`-relative spelling.
fn is_crate_relative(t: &str) -> bool {
    let Some((head, tail)) = t.split_once('/') else {
        return false;
    };
    CRATE_PREFIXES.iter().any(|p| head.starts_with(p))
        && CRATE_SUBDIRS.iter().any(|d| tail.starts_with(d))
}

/// Does a path citation resolve? Three spellings are accepted, in order: as
/// written from the root, `crates/`-prefixed, and — for a `.rs` file only —
/// relative to the citing crate. A trailing line spec (`b.rs:739`,
/// `b.rs:739-744`, `b.rs:739..744`, `b.rs:739:12`) names a place IN the file
/// and is dropped before the file is looked for: the gate decides whether the
/// file exists, and says nothing about the line — measured 2026-09-18, when
/// `crates/aterm-pty/src/unix.rs:739-744`, a file that exists, was reported as
/// resolving to nothing.
fn resolves_path(root: &Path, citing: &str, cited: &str) -> bool {
    let t = cited.trim_end_matches(&['.', ',', ';', ':'][..]);
    // `crates/a/src/b.rs::item` — a path plus the item in it. The path half is
    // what this rule decides; the item half is a Rust path, which [`Rule::Name`]
    // deliberately does not claim to resolve.
    let t = t.split("::").next().unwrap_or(t);
    let t = strip_line_spec(t);
    // A FAMILY: `tools/atpkg-*.sh` names every file matching, and it is a true
    // citation exactly when at least one exists. Deleting the whole family is
    // still caught; renaming one member of it is not, which is the honest bound
    // of a glob and the reason prose should name a file when it means one.
    if t.contains('*') || t.contains('?') {
        return glob_matches(root, t);
    }
    if root.join(t).exists() {
        return true;
    }
    if is_crate_relative(t) && root.join("crates").join(t).exists() {
        return true;
    }
    if citing.ends_with(".rs") {
        let mut parts = citing.split('/');
        if let (Some(a), Some(b)) = (parts.next(), parts.next())
            && root.join(a).join(b).join(t).exists()
        {
            return true;
        }
    }
    false
}

/// `b.rs:739`, `b.rs:739-744`, `b.rs:739..744`, `b.rs:739:12` → `b.rs`. Only a
/// suffix made of digits and the range/column punctuation is a line spec; any
/// other `:` tail (a Windows drive, a URL) is left alone and fails as written.
fn strip_line_spec(t: &str) -> &str {
    let Some((head, tail)) = t.rsplit_once(':') else {
        return t;
    };
    let is_spec = !tail.is_empty()
        && tail.starts_with(|c: char| c.is_ascii_digit())
        && tail
            .chars()
            .all(|c| c.is_ascii_digit() || c == '-' || c == '.');
    if !is_spec {
        return t;
    }
    // `b.rs:739:12` — a column after the line; peel once more.
    strip_line_spec(head)
}

/// Does any file match a citation carrying `*` / `?`? Only the LAST segment may
/// carry the wildcard — a glob spanning directories is not a shape this
/// repository's prose uses, and refusing it keeps the walk one directory deep.
fn glob_matches(root: &Path, pattern: &str) -> bool {
    let Some((dir, leaf)) = pattern.rsplit_once('/') else {
        return false;
    };
    if dir.contains('*') || dir.contains('?') {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
        return false;
    };
    entries
        .flatten()
        .any(|e| glob_leaf_matches(leaf, &e.file_name().to_string_lossy()))
}

/// `*` (any run, including empty) and `?` (exactly one char), anchored at both
/// ends. Written out rather than pulled in: one dependency-free function, and
/// the shapes in this repository's prose are `pre*`, `*post`, `pre*post`.
fn glob_leaf_matches(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    // Classic two-pointer wildcard match with backtracking on the last `*`.
    let (mut pi, mut ni) = (0usize, 0usize);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = pi;
            mark = ni;
            pi += 1;
        } else if star != usize::MAX {
            pi = star + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Every name DEFINED in the tree: Rust items and fields, shell functions and
/// variables, Python defs. Built once per run.
fn defined_names(root: &Path) -> HashSet<String> {
    let mut out = HashSet::new();
    collect_defs(root, &mut out, 0);
    out
}

fn collect_defs(dir: &Path, out: &mut HashSet<String>, depth: usize) {
    if depth > 16 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if matches!(
                name.as_str(),
                "target" | "target.noindex" | ".git" | "node_modules" | "scratchpad"
            ) {
                continue;
            }
            collect_defs(&path, out, depth + 1);
        } else if name.ends_with(".rs") || name.ends_with(".sh") || name.ends_with(".py") {
            if let Ok(src) = std::fs::read_to_string(&path) {
                defs_in(&src, out);
            }
        } else if !name.contains('.') {
            // An extension-less executable (publish/after-cut, tools/freeze-safety-gate)
            // holds shell functions like any `.sh`.
            if let Ok(src) = std::fs::read_to_string(&path) {
                defs_in(&src, out);
            }
        }
    }
}

/// Definitions in one file. Keyword-anchored rather than parsed: the question is
/// only "is this name written down as a definition anywhere", and a missed
/// definition costs a false red that the roster's own green proves absent.
fn defs_in(src: &str, out: &mut HashSet<String>) {
    for line in src.lines() {
        let t = line.trim_start();
        for kw in [
            "fn ", "def ", "const ", "static ", "struct ", "enum ", "trait ", "type ", "mod ",
        ] {
            if let Some(rest) = strip_item_kw(t, kw) {
                push_ident(rest, out);
            }
        }
        // `pub fn`, `pub(crate) fn`, `async fn`, `pub async fn`, …
        if let Some(rest) = t.strip_prefix("pub") {
            let rest =
                rest.trim_start_matches(|c: char| c == '(' || c == ')' || c.is_alphanumeric());
            let rest = rest.trim_start();
            for kw in [
                "fn ", "const ", "static ", "struct ", "enum ", "trait ", "type ", "mod ",
            ] {
                if let Some(r) = rest.strip_prefix(kw) {
                    push_ident(r, out);
                }
            }
            if let Some(r) = rest.strip_prefix("async fn ") {
                push_ident(r, out);
            }
            // a pub struct field: `pub name: Type`
            push_field(rest, out);
        }
        if let Some(r) = t.strip_prefix("async fn ") {
            push_ident(r, out);
        }
        // a struct field or a named struct-literal key: `name: Type` / `name: value`
        push_field(t, out);
        // a shell function: `name() {`
        if let Some(paren) = t.find("()") {
            push_ident(&t[..paren], out);
        }
        // a shell/env assignment: `NAME=value`, `local name=…`
        if let Some(eq) = t.find('=') {
            let lhs = t[..eq].trim().trim_start_matches("local ").trim();
            push_ident(lhs, out);
        }
    }
}

fn strip_item_kw<'a>(t: &'a str, kw: &str) -> Option<&'a str> {
    t.strip_prefix(kw)
}

fn push_ident(s: &str, out: &mut HashSet<String>) {
    let ident: String = s
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if ident.len() > 1 {
        out.insert(ident);
    }
}

fn push_field(t: &str, out: &mut HashSet<String>) {
    let Some(colon) = t.find(':') else { return };
    if t.as_bytes().get(colon + 1) == Some(&b':') {
        return;
    }
    let lhs = t[..colon].trim();
    if lhs.is_empty() || lhs.contains(' ') || lhs.contains('(') {
        return;
    }
    if lhs.starts_with(|c: char| c.is_ascii_lowercase() || c == '_') {
        push_ident(lhs, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_citation_that_resolves_to_nothing_fails_the_citations_verb() {
        let dir = std::env::temp_dir().join(format!(
            "xtask-citations-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("docs")).expect("mk docs");
        std::fs::create_dir_all(dir.join("tools")).expect("mk tools");
        std::fs::write(
            dir.join("tools/real.sh"),
            "#!/bin/sh\nreal_shell_fn() { :; }\n",
        )
        .expect("write tool");

        // GREEN: the cited path is there.
        std::fs::write(
            dir.join("docs/RELEASING.md"),
            "The driver is `tools/real.sh`, which is what runs.\n",
        )
        .expect("write doc");
        let (ok, log) = citations_report(&dir, &[], &["docs/RELEASING.md"]);
        assert!(ok, "a resolving citation must be GREEN:\n{log}");

        // RED: the cited path is not.
        std::fs::write(
            dir.join("docs/RELEASING.md"),
            "The driver is `tools/ghost.sh`, which is what runs.\n",
        )
        .expect("write doc");
        let (ok, log) = citations_report(&dir, &[], &["docs/RELEASING.md"]);
        assert!(!ok, "a dangling path citation must be RED:\n{log}");
        assert!(log.contains("PATH"), "the rule must be named:\n{log}");
        assert!(
            log.contains("tools/ghost.sh"),
            "the citation must be quoted:\n{log}"
        );

        // GREEN again: the same dangling citation, now ADMITTED as gone.
        std::fs::write(
            dir.join("docs/RELEASING.md"),
            "Ported from the retired `tools/ghost.sh`, which is deleted.\n",
        )
        .expect("write doc");
        let (ok, log) = citations_report(&dir, &[], &["docs/RELEASING.md"]);
        assert!(ok, "an admitted absence must be GREEN:\n{log}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `path:line` is how this repository cites a place in a file; the gate
    /// decides the FILE and must not read the line spec as part of its name.
    #[test]
    fn a_path_citation_with_a_line_spec_resolves_to_its_file() {
        let dir = std::env::temp_dir().join(format!(
            "xtask-citations-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("docs")).expect("mk docs");
        std::fs::create_dir_all(dir.join("tools")).expect("mk tools");
        std::fs::write(dir.join("tools/real.sh"), "#!/bin/sh\n:\n").expect("write tool");

        for spec in [
            "tools/real.sh:3",
            "tools/real.sh:3-4",
            "tools/real.sh:3..4",
            "tools/real.sh:3:12",
        ] {
            std::fs::write(
                dir.join("docs/RELEASING.md"),
                format!("Measured at `{spec}`, which is what runs.\n"),
            )
            .expect("write doc");
            let (ok, log) = citations_report(&dir, &[], &["docs/RELEASING.md"]);
            assert!(
                ok,
                "`{spec}` names a file that exists and must be GREEN:\n{log}"
            );
        }

        // The line spec is dropped, never the verdict: a missing file with a
        // line spec is still RED, and the citation is quoted as written.
        std::fs::write(
            dir.join("docs/RELEASING.md"),
            "Measured at `tools/ghost.sh:3-4`, which is what runs.\n",
        )
        .expect("write doc");
        let (ok, log) = citations_report(&dir, &[], &["docs/RELEASING.md"]);
        assert!(
            !ok,
            "a dangling path keeps failing with a line spec:\n{log}"
        );
        assert!(log.contains("tools/ghost.sh:3-4"), "{log}");

        assert_eq!(strip_line_spec("a/b.rs"), "a/b.rs");
        assert_eq!(strip_line_spec("C:/x"), "C:/x");
        assert_eq!(strip_line_spec("a/b.rs:12abc"), "a/b.rs:12abc");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_named_test_that_is_defined_nowhere_fails_the_citations_verb() {
        let dir = std::env::temp_dir().join(format!(
            "xtask-citations-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("crates/c/src")).expect("mk crate");
        std::fs::write(
            dir.join("crates/c/src/other.rs"),
            "#[test]\nfn the_witness_test_that_exists_here() {}\n",
        )
        .expect("write witness");

        let doc = |claim: &str| {
            std::fs::write(
                dir.join("crates/c/src/lib.rs"),
                format!("//! The exactness argument is proven in `{claim}`.\n"),
            )
            .expect("write lib");
        };

        doc("the_witness_test_that_exists_here");
        let (ok, log) = citations_report(&dir, &["crates/c/src"], &[]);
        assert!(ok, "a citation of a defined test must be GREEN:\n{log}");

        doc("the_witness_test_that_was_renamed_away");
        let (ok, log) = citations_report(&dir, &["crates/c/src"], &[]);
        assert!(!ok, "a citation of an undefined test must be RED:\n{log}");
        assert!(log.contains("NAME"), "the rule must be named:\n{log}");

        // A citation in a sentence that names ANOTHER tree is out of reach.
        std::fs::write(
            dir.join("crates/c/src/lib.rs"),
            "//! Trust's targo checks this in `validate_the_frontend_authority_path`.\n",
        )
        .expect("write lib");
        let (ok, log) = citations_report(&dir, &["crates/c/src"], &[]);
        assert!(ok, "a foreign-tree citation must be GREEN:\n{log}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_roster_entry_that_names_nothing_fails_the_citations_verb() {
        let dir = std::env::temp_dir().join(format!(
            "xtask-citations-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mk root");
        let (ok, log) = citations_report(&dir, &[], &["docs/NOT-THERE.md"]);
        assert!(!ok, "a roster naming an absent file must be RED:\n{log}");
        assert!(
            log.contains("ROSTER"),
            "the roster failure must say so:\n{log}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two-word names are NOT checked, and that bound is the reason this gate can
    /// run without a waiver file: `build_app` and `last_seen` are English as
    /// often as they are items.
    #[test]
    fn classify_checks_only_test_shaped_names_and_repo_rooted_paths() {
        assert_eq!(classify("crates/atpkg/src/sig.rs"), Some(Rule::Path));
        assert_eq!(classify("aterm-release/src/sign.rs"), Some(Rule::Path));
        assert_eq!(classify("tests/plist_stamp.rs"), None); // no root: not decidable here
        assert_eq!(classify("target/release"), None); // a runtime path, not a citation
        assert_eq!(classify("a_four_word_name_here"), Some(Rule::Name));
        assert_eq!(classify("build_app"), None);
        assert_eq!(classify("fmt::Arguments"), None);
        assert_eq!(classify("String"), None);
    }
}
