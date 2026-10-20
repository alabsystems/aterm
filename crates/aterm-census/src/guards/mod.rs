// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The source guards: rules a type, a visibility or a clippy lint cannot
//! express, each a small function over one shared, lexed corpus.
//!
//! The corpus is every `.rs` under `crates/` and `apps/` (the absorbed winit,
//! `crates/aterm-window`, among them), read once and lexed on demand
//! ([`crate::lex`]): rules read the CODE view (comments and literal contents
//! blanked) or the TEXT view (comments blanked, literals kept), never prose. A
//! rule about shipped code also drops every item whose `cfg` proves it
//! test-only (the mask the censuses use). Beside it, from one walk of the
//! checkout ([`walk`]: `.git`, build output and what `.gitignore` keeps out
//! by directory aside): every file's path, dot directories included
//! (`key-material`'s key file names: `.cargo/machine.key` is one), and every
//! shell, Python and JavaScript script outside `vendor/` and the dot
//! directories, read as plain lines ([`Script`]) for the rules whose shapes
//! live in scripts too.
//!
//! | rule                | keeps out |
//! |---------------------|-----------|
//! | `engine-platform`   | a platform API, a thread or a native link in an engine crate's shipped code (grouped `std::{…}` imports too) |
//! | `renderer-terminal` | `aterm_core::Terminal` in the renderers (they read `RenderInput`) |
//! | `dev-read`          | `fs::read*` of a `/dev/…` literal (a device never EOFs) |
//! | `input-probe`       | the WindowServer-backed input-probe APIs |
//! | `test-appkit-door`  | a `#[test]` reaching an AppKit call that opens WindowServer |
//! | `shared-application`| `+[NSApplication sharedApplication]` in a fn that neither takes nor first fetches the `Windowed` proof |
//! | `protected-folder`  | a TCC-protected folder path in aterm-gui or aterm-cli |
//! | `runloop-timer`     | a run-loop timer outside `aterm_objc::wake_timer` |
//! | `keeper-gui`        | a GUI framework, an ObjC send or winit in the PTY keeper's source |
//! | `keeper-gui-deps`   | a GUI crate in the PTY keeper's process closure, `Cargo.lock` walked transitively |
//! | `keeper-no-mmap`    | a mapping call named in the PTY keeper's own source (a local `mmap` declaration; the region API is its clippy.toml's): it holds crash-restore regions unmapped |
//! | `restore-stays-local` | aterm-restore in aterm-link's closure, or a socket named in aterm-restore's source: a crash-restore region never leaves the host |
//! | `headless-lifeline` | a `--headless` launch in tests, tools or scripts that holds no lifeline |
//! | `std-command`       | the `StdCommand` alias built, renamed or re-aliased outside aterm-uds |
//! | `flock-door`        | an `flock` the clippy lint cannot name |
//! | `supervise-fx`      | a clock read, a sleep, the pid, a delivery, the goal record or the ledger by path that skips the supervisor's `Fx` door; a look at the filesystem, a process, the kernel, the zone or the environment that skips a derived door (the policy probe or `Fx`'s) |
//! | `store-current`     | a store `current` path run unresolved |
//! | `key-material`      | a master phrase on the key surface, a key file in the checkout |
//! | `capture-privacy`   | in an encoded test capture's decoded bytes (a pty log's hex, a `.reads` file's escapes), what the export forbids in text, a home path, a banner's plan or a session id |
//! | `devtools-door`     | a developer-mode change outside the owner's gesture; an admin-rights API |
//! | `machines-root-door`| a root tool (`installer`, `pmset`, `/etc/sudoers.d`, …) or the machines agent outside aterm-machines |
//! | `ffi-door`          | a WindowServer or process-creation `extern` declared outside its one file |
//! | `handoff-name`      | a live-upgrade handoff variable spelled outside `HandoffName::new` |
//! | `core-signal`       | a kill, abort or crash helper that ends a process by a core-dump signal |
//! | `launcher-mint`     | the root-authority mint reached or enabled outside the launchers |
//! | `checked-mktemp`    | a scratch directory from an unchecked `$(cd "$(mktemp …)")` |
//! | `installer-network` | a fetch in `tools/install.sh` outside `web_curl`, the GitHub API, `gh` |
//! | `agent-gate`        | an agent tested by `==`, `matches!` or `if let` instead of an exhaustive `match` |
//! | `slog-inert`        | a PTY write path or an input-verb request in the session log's code; `dispatch_mark` past its calls |
//! | `eager-drop`        | `then_some` building a value whose type has a `Drop` (built and dropped on false) |
//! | `licence-header`    | a `.rs` without its SPDX and copyright lines |
//!
//! [`run_guards`] returns typed [`Finding`]s; `aterm-census --guards` (and
//! bare `aterm-census`, which runs every census) prints them.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::OnceLock;

use crate::lex::{Lexed, lex};
use crate::{CensusOutcome, Floor};

mod agent_gate;
mod captures;
mod doors;
mod eager_drop;
mod graph;
mod installer;
mod keys;
mod lifeline;
mod machines;
mod mint;
mod mktemp;
mod rules;
mod slog_inert;

/// One violation: where, which rule, and what the rule saw there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    /// The rule's id ([`Rule::id`]).
    pub rule: &'static str,
    /// Repo-relative path, `/`-separated.
    pub path: String,
    /// 1-based line; 0 when the finding is about the file as a whole.
    pub line: usize,
    /// What was seen, in one line.
    pub detail: String,
}

/// One guard: its id, the defect it keeps out, and its check.
pub struct Rule {
    /// The id findings carry.
    pub id: &'static str,
    /// The defect, and why it matters, in one line.
    pub why: &'static str,
    /// The path prefixes the rule reads by name. Each must hold a file of the
    /// corpus, or a renamed crate would leave the rule reading nothing there.
    scope: &'static [&'static str],
    check: fn(&Tree<'_>, &mut Vec<Finding>),
}

/// Every guard, in the order findings print.
pub const GUARDS: &[Rule] = &[
    rules::ENGINE_PLATFORM,
    rules::RENDERER_TERMINAL,
    rules::DEV_READ,
    rules::INPUT_PROBE,
    graph::TEST_APPKIT_DOOR,
    graph::SHARED_APPLICATION,
    rules::PROTECTED_FOLDER,
    rules::RUNLOOP_TIMER,
    rules::KEEPER_GUI,
    rules::KEEPER_GUI_DEPS,
    rules::KEEPER_NO_MMAP,
    rules::RESTORE_STAYS_LOCAL,
    lifeline::HEADLESS_LIFELINE,
    rules::STD_COMMAND,
    rules::FLOCK_DOOR,
    rules::SUPERVISE_FX,
    rules::STORE_CURRENT,
    keys::KEY_MATERIAL,
    captures::CAPTURE_PRIVACY,
    doors::DEVTOOLS_DOOR,
    machines::MACHINES_ROOT_DOOR,
    doors::FFI_DOOR,
    doors::HANDOFF_NAME,
    doors::CORE_SIGNAL,
    mint::LAUNCHER_MINT,
    mktemp::CHECKED_MKTEMP,
    installer::INSTALLER_NETWORK,
    agent_gate::AGENT_GATE,
    slog_inert::SLOG_INERT,
    eager_drop::EAGER_DROP,
    rules::LICENCE_HEADER,
];

/// The guards' outcome over one tree.
pub struct GuardReport {
    /// The `.rs` files read.
    pub files: usize,
    /// The scripts read ([`Script`]).
    pub scripts: usize,
    /// Every finding, in [`GUARDS`] order, then by path and line.
    pub findings: Vec<Finding>,
}

/// Run every guard over the checkout at `root`. `Err` is a tree that could not
/// be read.
pub fn run_guards(root: &Path) -> Result<GuardReport, String> {
    let corpus = Corpus::read(root)?;
    let tree = Tree {
        root,
        corpus: &corpus,
    };
    let mut findings = scope_findings(&corpus, GUARDS);
    findings.extend(run_rules(&tree, GUARDS));
    Ok(GuardReport {
        files: corpus.files.len(),
        scripts: corpus.scripts.len(),
        findings,
    })
}

/// One finding per scope prefix of `rules` that no file of `corpus` lies under.
fn scope_findings(corpus: &Corpus, rules: &[Rule]) -> Vec<Finding> {
    let mut out = Vec::new();
    for rule in rules {
        for prefix in rule.scope {
            if corpus.under(&[prefix]).next().is_none() {
                out.push(Finding {
                    rule: rule.id,
                    path: (*prefix).to_string(),
                    line: 0,
                    detail: "no Rust file lies under this scope, so the rule reads nothing \
                             there (renamed? name its new path in the rule)"
                        .to_string(),
                });
            }
        }
    }
    out
}

/// `rules` over `tree`, each on its own thread (they share only the corpus,
/// whose lexing is computed once per file whichever rule asks first). A rule
/// that panics comes back as one finding.
fn run_rules(tree: &Tree<'_>, rules: &[Rule]) -> Vec<Finding> {
    std::thread::scope(|scope| {
        let running: Vec<_> = rules
            .iter()
            .map(|rule| {
                std::thread::Builder::new()
                    .name(format!("guard-{}", rule.id))
                    .stack_size(8 << 20)
                    .spawn_scoped(scope, move || {
                        let mut found = Vec::new();
                        (rule.check)(tree, &mut found);
                        found
                    })
            })
            .collect();
        let mut all = Vec::new();
        for (handle, rule) in running.into_iter().zip(rules) {
            let joined = handle
                .map_err(|e| e.to_string())
                .and_then(|h| h.join().map_err(|_| "it panicked".to_string()));
            match joined {
                Ok(mut found) => {
                    found.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
                    all.extend(found);
                }
                Err(why) => all.push(Finding {
                    rule: rule.id,
                    path: String::new(),
                    line: 0,
                    detail: format!("the rule could not run: {why}"),
                }),
            }
        }
        all
    })
}

/// The guards as a census: the transcript `aterm-census --guards` prints.
pub(crate) fn run_guard_census(root: &Path) -> CensusOutcome {
    const TITLE: &str = "guards";
    let ids: Vec<&str> = GUARDS.iter().map(|r| r.id).collect();
    let mut log = format!(
        "=== guards (source rules over crates/, apps/) ===\n    root: {}\n    \
         rules: {}\n",
        root.display(),
        ids.join(", ")
    );
    let report = match run_guards(root) {
        Ok(report) => report,
        Err(why) => {
            return CensusOutcome::red(TITLE, log, format!("the tree could not be read: {why}"));
        }
    };
    let notes = captures::notes(root);
    for note in &notes {
        let _ = writeln!(log, "    note: {note}");
    }
    let mut by_rule: BTreeMap<usize, Vec<&Finding>> = BTreeMap::new();
    for f in &report.findings {
        let at = GUARDS.iter().position(|r| r.id == f.rule).unwrap_or(0);
        by_rule.entry(at).or_default().push(f);
    }
    for (at, found) in &by_rule {
        for f in found {
            let _ = writeln!(
                log,
                "  ✗ FAIL [{}] {}:{}: {}",
                f.rule, f.path, f.line, f.detail
            );
        }
        let _ = writeln!(log, "      why: {}", GUARDS[*at].why);
    }
    if report.findings.is_empty() {
        let floors = vec![
            Floor::at_least(
                "Rust files the guards read",
                report.files,
                1500,
                "the walk lost most of the tree (a wrong root or a moved crates/), so every \
                 guard held vacuously",
            ),
            // The scripts carry no count: installer-network's anchor
            // (`tools/install.sh`) fails a walk that lost them.
        ];
        CensusOutcome::green(
            TITLE,
            log,
            format!(
                "{} rules over {} Rust files and {} scripts; no finding.{}",
                GUARDS.len(),
                report.files,
                report.scripts,
                notes.iter().map(|n| format!(" ({n})")).collect::<String>()
            ),
        )
        .with_floors(floors)
    } else {
        CensusOutcome::red(
            TITLE,
            log,
            format!(
                "{} finding(s) from {} rule(s).",
                report.findings.len(),
                by_rule.len()
            ),
        )
    }
}

/// What a rule reads: the checkout root (for manifests) and the corpus.
pub(crate) struct Tree<'a> {
    pub(crate) root: &'a Path,
    pub(crate) corpus: &'a Corpus,
}

/// The source the guards read, each file lexed at most once.
pub(crate) struct Corpus {
    files: Vec<SourceFile>,
    /// Every script of the checkout ([`Script::is_script`]).
    pub(crate) scripts: Vec<Script>,
    /// Every file of the checkout, root-relative ([`walk`]).
    pub(crate) paths: Vec<String>,
}

/// The directories whose `.rs` files make the corpus.
const CORPUS_DIRS: &[&str] = &["crates", "apps"];

impl Corpus {
    /// Every `.rs` under [`CORPUS_DIRS`] at `root` (`target/` and dot
    /// directories skipped), in path order; then every file of the checkout and
    /// every script among them.
    pub(crate) fn read(root: &Path) -> Result<Self, String> {
        let mut paths = Vec::new();
        for dir in CORPUS_DIRS {
            crate::collect_rs_files(&root.join(dir), &mut paths)
                .map_err(|e| format!("cannot walk {dir}: {e}"))?;
        }
        paths.sort();
        let mut files = Vec::with_capacity(paths.len());
        for path in paths {
            let raw = std::fs::read_to_string(&path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            files.push(SourceFile::new(rel, raw));
        }
        let mut paths = Vec::new();
        walk(root, "", &Ignored::read(root), &mut paths)?;
        paths.sort();
        let mut scripts = Vec::new();
        for rel in paths.iter().filter(|rel| !in_dot_dir(rel)) {
            if let Some(script) = Script::read(root, rel)? {
                scripts.push(script);
            }
        }
        Ok(Self {
            files,
            scripts,
            paths,
        })
    }

    /// A corpus of the given `(path, contents)` files, for tests: the `.rs`
    /// ones as source, the rest as scripts.
    #[cfg(test)]
    pub(crate) fn of(files: &[(&str, &str)]) -> Self {
        let (rs, other): (Vec<_>, Vec<_>) = files.iter().partition(|(rel, _)| rel.ends_with(".rs"));
        Self {
            files: rs
                .iter()
                .map(|(rel, raw)| SourceFile::new((*rel).to_string(), (*raw).to_string()))
                .collect(),
            scripts: other
                .iter()
                .map(|(rel, raw)| Script {
                    rel: (*rel).to_string(),
                    raw: (*raw).to_string(),
                })
                .collect(),
            paths: files.iter().map(|(rel, _)| (*rel).to_string()).collect(),
        }
    }

    /// Every file whose path starts with one of `prefixes`.
    pub(crate) fn under<'a>(
        &'a self,
        prefixes: &'a [&'a str],
    ) -> impl Iterator<Item = &'a SourceFile> + 'a {
        self.files
            .iter()
            .filter(move |f| prefixes.iter().any(|p| f.rel.starts_with(p)))
    }

    /// The file at `rel`, if the corpus holds it.
    pub(crate) fn get(&self, rel: &str) -> Option<&SourceFile> {
        self.files.iter().find(|f| f.rel == rel)
    }
}

/// A directory no walk enters: `.git`, build output (`target`, `target-*`,
/// `target.*`: what `.gitignore` keeps out by pattern) and `node_modules`.
/// Other dot directories are walked: a key file is as public in `.cargo/` as
/// anywhere.
fn skipped_dir(name: &str) -> bool {
    name == ".git"
        || name == "target"
        || name.starts_with("target-")
        || name.starts_with("target.")
        || name == "node_modules"
}

/// Is `rel` under a dot directory (`.cargo/…`, `crates/x/.scratch/…`)? Its
/// files are paths to the walk, never scripts: tool state, not the tree's code.
fn in_dot_dir(rel: &str) -> bool {
    rel.rsplit_once('/')
        .is_some_and(|(dirs, _)| dirs.split('/').any(|d| d.starts_with('.')))
}

/// The directories the root `.gitignore` keeps out by a plain directory row
/// (`name/` at any depth, `/path/` or `a/b/` from the root; no glob, no
/// negation): tool state and caches (`.claude/`, `/.trust-cache/`), whose
/// files no commit takes without `-f`, so no walk needs them. An absent or
/// unreadable `.gitignore` keeps nothing out, which walks more, never less.
#[derive(Default)]
pub(crate) struct Ignored {
    /// Rows without a `/` but their last: a directory of that name anywhere.
    anywhere: Vec<String>,
    /// Rows anchored at the root: that root-relative directory.
    rooted: Vec<String>,
}

impl Ignored {
    pub(crate) fn read(root: &Path) -> Self {
        Self::parse(&std::fs::read_to_string(root.join(".gitignore")).unwrap_or_default())
    }

    fn parse(text: &str) -> Self {
        let mut out = Self::default();
        for row in text.lines().map(str::trim) {
            let Some(dir) = row.strip_suffix('/') else {
                continue;
            };
            if dir.is_empty() || row.starts_with(['#', '!']) || dir.contains(['*', '?', '[', '\\'])
            {
                continue;
            }
            match dir.strip_prefix('/') {
                Some(path) => out.rooted.push(path.to_string()),
                None if dir.contains('/') => out.rooted.push(dir.to_string()),
                None => out.anywhere.push(dir.to_string()),
            }
        }
        out
    }

    /// Does a row keep out the directory `name` at root-relative `rel`?
    fn skips(&self, rel: &str, name: &str) -> bool {
        self.anywhere.iter().any(|n| n == name) || self.rooted.iter().any(|p| p == rel)
    }
}

/// Every file under `root/rel` ([`skipped_dir`]s and `ignored` directories
/// aside) as a root-relative, `/`-separated path. A missing `rel` adds nothing.
pub(crate) fn walk(
    root: &Path,
    rel: &str,
    ignored: &Ignored,
    out: &mut Vec<String>,
) -> Result<(), String> {
    let dir = root.join(rel);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let child = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        let kind = entry.file_type().map_err(|e| format!("{child}: {e}"))?;
        if kind.is_dir() {
            if !skipped_dir(&name) && !ignored.skips(&child, &name) {
                walk(root, &child, ignored, out)?;
            }
        } else {
            out.push(child);
        }
    }
    Ok(())
}

/// A script the guards read as plain lines (comments included: a rule says
/// when it skips them).
pub(crate) struct Script {
    pub(crate) rel: String,
    pub(crate) raw: String,
}

/// The extensions that make a file a script.
const SCRIPT_EXTENSIONS: &[&str] = &["sh", "bash", "zsh", "py", "js", "mjs", "cjs"];

impl Script {
    /// Is the file at `rel` a script by its name: outside `vendor/` and with a
    /// script extension? (An extensionless file is one when it starts `#!`.)
    fn is_script(rel: &str) -> Option<bool> {
        if rel.starts_with("vendor/") {
            return Some(false);
        }
        let name = rel.rsplit('/').next().unwrap_or(rel);
        match name.rsplit_once('.') {
            Some((stem, ext)) if !stem.is_empty() => Some(SCRIPT_EXTENSIONS.contains(&ext)),
            _ => None,
        }
    }

    /// The script at `rel`, or `None` when it is not one (or is not text).
    fn read(root: &Path, rel: &str) -> Result<Option<Self>, String> {
        let path = root.join(rel);
        let by_name = Self::is_script(rel);
        if by_name == Some(false) || !path.is_file() {
            return Ok(None);
        }
        let unreadable = |e: std::io::Error| format!("cannot read {rel}: {e}");
        if by_name.is_none() {
            use std::io::Read as _;
            let mut head = [0u8; 2];
            let mut file = std::fs::File::open(&path).map_err(unreadable)?;
            if file.read_exact(&mut head).is_err() || head != *b"#!" {
                return Ok(None);
            }
        }
        let bytes = std::fs::read(&path).map_err(unreadable)?;
        if bytes.contains(&0) {
            return Ok(None);
        }
        Ok(Some(Self {
            rel: rel.to_string(),
            raw: String::from_utf8_lossy(&bytes).into_owned(),
        }))
    }

    /// Every line, 1-based.
    pub(crate) fn lines(&self) -> impl Iterator<Item = (usize, &str)> {
        self.raw.lines().enumerate().map(|(i, l)| (i + 1, l))
    }

    /// A Python script: a `.py`, or a `#!` naming python.
    pub(crate) fn is_python(&self) -> bool {
        self.rel.ends_with(".py")
            || self
                .raw
                .lines()
                .next()
                .is_some_and(|l| l.starts_with("#!") && l.contains("python"))
    }

    pub(crate) fn finding(&self, rule: &'static str, line: usize, detail: String) -> Finding {
        Finding {
            rule,
            path: self.rel.clone(),
            line,
            detail,
        }
    }
}

/// One source file: its path, its bytes, and (on demand) its views.
pub(crate) struct SourceFile {
    pub(crate) rel: String,
    pub(crate) raw: String,
    lexed: OnceLock<Lexed>,
    shipped: OnceLock<Vec<bool>>,
}

/// One line of a file, in both views (byte-aligned).
#[derive(Clone, Copy)]
pub(crate) struct Line<'a> {
    /// 1-based.
    pub(crate) no: usize,
    pub(crate) code: &'a str,
    pub(crate) text: &'a str,
}

impl SourceFile {
    fn new(rel: String, raw: String) -> Self {
        Self {
            rel,
            raw,
            lexed: OnceLock::new(),
            shipped: OnceLock::new(),
        }
    }

    pub(crate) fn lexed(&self) -> &Lexed {
        self.lexed.get_or_init(|| lex(&self.raw))
    }

    /// Does the raw file hold any of `needles`? A cheap prefilter before lexing.
    pub(crate) fn mentions(&self, needles: &[&str]) -> bool {
        needles.iter().any(|n| self.raw.contains(n))
    }

    /// Every line, both views.
    pub(crate) fn lines(&self) -> impl Iterator<Item = Line<'_>> {
        let lexed = self.lexed();
        lexed
            .code
            .lines()
            .zip(lexed.text.lines())
            .enumerate()
            .map(|(i, (code, text))| Line {
                no: i + 1,
                code,
                text,
            })
    }

    /// Every line outside an item whose `cfg` proves it test-only.
    pub(crate) fn shipped_lines(&self) -> impl Iterator<Item = Line<'_>> {
        let shipped = self
            .shipped
            .get_or_init(|| crate::shipped_lines(&self.lexed().code));
        self.lines()
            .filter(move |l| shipped.get(l.no - 1).copied().unwrap_or(true))
    }

    /// The file name, without its directories.
    pub(crate) fn name(&self) -> &str {
        self.rel.rsplit('/').next().unwrap_or(&self.rel)
    }

    pub(crate) fn finding(&self, rule: &'static str, line: usize, detail: String) -> Finding {
        Finding {
            rule,
            path: self.rel.clone(),
            line,
            detail,
        }
    }
}

// Token helpers over one line of a view.

pub(crate) fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// How a needle must sit in its line.
#[derive(Clone, Copy)]
pub(crate) enum Bound {
    /// Anywhere.
    Sub,
    /// Not inside a longer identifier on either side.
    Word,
}

/// The byte offsets where `needle` occurs in `hay` under `bound`.
pub(crate) fn hits<'a>(
    hay: &'a str,
    needle: &'a str,
    bound: Bound,
) -> impl Iterator<Item = usize> + 'a {
    let h = hay.as_bytes();
    hay.match_indices(needle)
        .map(|(at, _)| at)
        .filter(move |&at| {
            matches!(bound, Bound::Sub)
                || ((at == 0 || !is_word(h[at - 1]))
                    && h.get(at + needle.len()).is_none_or(|&b| !is_word(b)))
        })
}

/// Does `hay` hold `needle` under `bound`?
pub(crate) fn has(hay: &str, needle: &str, bound: Bound) -> bool {
    hits(hay, needle, bound).next().is_some()
}

/// `s` after `prefix`, past any spaces and tabs before it.
pub(crate) fn after_ws<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    s.trim_start_matches([' ', '\t']).strip_prefix(prefix)
}

/// Every name a line calls: the identifier before each `(` (spaces between
/// allowed), whether it is a free call, a path's last segment or a method.
pub(crate) fn calls(code: &str) -> Vec<&str> {
    let b = code.as_bytes();
    let mut out = Vec::new();
    for (at, _) in code.match_indices('(') {
        let mut end = at;
        while end > 0 && matches!(b[end - 1], b' ' | b'\t') {
            end -= 1;
        }
        let mut start = end;
        while start > 0 && is_word(b[start - 1]) {
            start -= 1;
        }
        while start < end && b[start].is_ascii_digit() {
            start += 1;
        }
        if start < end {
            out.push(&code[start..end]);
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The rule's findings over `files`, with no checkout behind them.
    pub(crate) fn run(rule: &Rule, files: &[(&str, &str)]) -> Vec<Finding> {
        let corpus = Corpus::of(files);
        let tree = Tree {
            root: Path::new("/nonexistent-aterm-census-root"),
            corpus: &corpus,
        };
        run_rules(&tree, std::slice::from_ref(rule))
    }

    /// Each finding as `path:line`.
    pub(crate) fn spots(found: &[Finding]) -> Vec<String> {
        found
            .iter()
            .map(|f| format!("{}:{}", f.path, f.line))
            .collect()
    }

    #[test]
    fn calls_are_the_identifiers_before_each_paren() {
        assert_eq!(
            calls("let x = a::b(c.d (1), Some(e)) + f[0](g);"),
            ["b", "d", "Some"]
        );
        assert!(calls("(1, 2)").is_empty());
    }

    #[test]
    fn word_hits_respect_identifier_edges() {
        assert!(has("use libc::c_int;", "libc", Bound::Word));
        assert!(!has("use aterm_libc::x;", "libc", Bound::Word));
        assert!(!has("MetalLayer", "Metal", Bound::Word));
        assert!(has("MetalLayer", "Metal", Bound::Sub));
    }

    #[test]
    fn shipped_lines_drop_test_items_in_both_views() {
        let file = SourceFile::new(
            "crates/x/src/lib.rs".to_string(),
            "fn a() {}\n#[cfg(test)]\nmod tests {\n    fn b() {}\n}\nfn c() {}\n".to_string(),
        );
        let kept: Vec<usize> = file.shipped_lines().map(|l| l.no).collect();
        assert_eq!(kept, [1, 6]);
    }

    #[test]
    fn a_panicking_rule_is_one_finding_not_a_crash() {
        fn boom(_: &Tree<'_>, _: &mut Vec<Finding>) {
            panic!("planted");
        }
        let rule = Rule {
            id: "boom",
            why: "planted",
            scope: &[],
            check: boom,
        };
        let found = run(&rule, &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].rule, "boom");
    }

    /// The walk enters dot directories for paths (never `.git`, nor one
    /// `.gitignore` keeps out by directory), and reads scripts only outside
    /// them.
    #[test]
    fn the_walk_lists_dot_directories_but_reads_no_script_there() {
        use crate::scan_set::test_fixtures::synth_tree;
        let files: Vec<(String, String)> = [
            (
                ".gitignore",
                "debug\n.claude/\n/.trust-cache/\n/publish/.out/\n**/mutants.out*/\n\
                 !vendor/**/debug/\n",
            ),
            ("tools/install.sh", "#!/bin/sh\n"),
            (".cargo/config.toml", ""),
            (".cargo/machine.key", ""),
            (".config/hooks/stop.sh", "#!/bin/sh\n"),
            (".claude/worktrees/w/.cargo/machine.key", ""),
            (".trust-cache/f.json", ""),
            ("crates/a/.trust-cache/f.json", ""),
            ("publish/.out/x.sh", "#!/bin/sh\n"),
            (".git/hooks/pre-commit.sh", "#!/bin/sh\n"),
            ("target/debug/x.sh", "#!/bin/sh\n"),
            (
                "crates/a/src/lib.rs",
                "// SPDX-License-Identifier: Apache-2.0\n",
            ),
        ]
        .iter()
        .map(|(p, c)| ((*p).to_string(), (*c).to_string()))
        .collect();
        let root = synth_tree("aterm-census-guards", "walk-dot-dirs", &files);
        let corpus = Corpus::read(&root);
        let _ = std::fs::remove_dir_all(&root);
        let corpus = corpus.expect("the synth tree reads");
        assert_eq!(
            corpus.paths,
            [
                ".cargo/config.toml",
                ".cargo/machine.key",
                ".config/hooks/stop.sh",
                ".gitignore",
                "crates/a/.trust-cache/f.json",
                "crates/a/src/lib.rs",
                "tools/install.sh",
            ]
        );
        let scripts: Vec<&str> = corpus.scripts.iter().map(|s| s.rel.as_str()).collect();
        assert_eq!(scripts, ["tools/install.sh"]);
    }

    #[test]
    fn a_scope_no_file_lies_under_is_a_finding() {
        fn nothing(_: &Tree<'_>, _: &mut Vec<Finding>) {}
        let rule = Rule {
            id: "scoped",
            why: "planted",
            scope: &["crates/aterm-grid/", "crates/aterm-renamed/"],
            check: nothing,
        };
        let corpus = Corpus::of(&[("crates/aterm-grid/src/lib.rs", "")]);
        let found = scope_findings(&corpus, std::slice::from_ref(&rule));
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].path, "crates/aterm-renamed/");
    }
}
