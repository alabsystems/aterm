// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The source guards: rules a type, a visibility or a clippy lint cannot express, each a small
//! function over one shared, lexed corpus.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;
use std::sync::OnceLock;

use crate::lex::{Lexed, lex};
use crate::{CensusOutcome, Floor};

mod agent_gate;
mod aos_step;
mod bridge_fx;
mod captures;
mod closure_anchor;
mod doc_sentence;
mod doors;
mod eager_drop;
mod export_content;
mod export_includes;
mod generated;
mod github;
mod grants;
mod graph;
mod installer;
mod keys;
mod lifeline;
mod machines;
mod mint;
mod mktemp;
mod rules;
mod rustup_keg;
mod shown_term;
mod slog_inert;
mod stock_label;
mod term_write;
mod win_resources;

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
    /// The path prefixes the rule reads by name.
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
    bridge_fx::BRIDGE_FX,
    aos_step::AOS_STEP_CLOCK,
    rules::STORE_CURRENT,
    keys::KEY_MATERIAL,
    captures::CAPTURE_PRIVACY,
    export_content::EXPORT_CONTENT,
    export_includes::EXPORT_INCLUDES,
    doors::DEVTOOLS_DOOR,
    machines::MACHINES_ROOT_DOOR,
    doors::FFI_DOOR,
    doors::BOOT_ID_DOOR,
    doors::HANDOFF_NAME,
    doors::CORE_SIGNAL,
    mint::LAUNCHER_MINT,
    grants::PROMPT_GRANT,
    mktemp::CHECKED_MKTEMP,
    rustup_keg::RUSTUP_KEG,
    installer::INSTALLER_NETWORK,
    agent_gate::AGENT_GATE,
    term_write::TERM_WRITE,
    shown_term::SHOWN_TERM,
    slog_inert::SLOG_INERT,
    eager_drop::EAGER_DROP,
    doc_sentence::DOC_SENTENCE,
    stock_label::STOCK_LABEL,
    github::GITHUB_AUTH_DOOR,
    github::GH_SPAWN,
    github::AGH_FX,
    closure_anchor::CLOSURE_ANCHOR,
    rules::LICENCE_HEADER,
    generated::GENERATED_SEAL,
    win_resources::WIN_RESOURCES,
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

/// Run every guard over the checkout at `root`.
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
/// whose lexing is computed once per file whichever rule asks first).
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
        let floors = vec![Floor::at_least(
            "Rust files the guards read",
            report.files,
            1500,
            "the walk lost most of the tree (a wrong root or a moved crates/), so every \
                 guard held vacuously",
        )];
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
    /// Every Cargo manifest under [`CORPUS_DIRS`] ([`is_manifest`]), root-relative, with its
    /// text.
    pub(crate) manifests: Vec<(String, String)>,
}

/// The directories whose `.rs` files make the corpus.
const CORPUS_DIRS: &[&str] = &["crates", "apps"];

impl Corpus {
    /// Every `.rs` under [`CORPUS_DIRS`] at `root` (`target/` and dot directories skipped), in
    /// path order; then every file of the checkout, the scripts among them, and the manifests
    /// among them under [`CORPUS_DIRS`].
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
        let mut manifests = Vec::new();
        for rel in paths.iter().filter(|rel| !in_dot_dir(rel)) {
            if let Some(script) = Script::read(root, rel)? {
                scripts.push(script);
            }
            if is_manifest(rel) {
                let text = std::fs::read_to_string(root.join(rel))
                    .map_err(|e| format!("cannot read {rel}: {e}"))?;
                manifests.push((rel.clone(), text));
            }
        }
        Ok(Self {
            files,
            scripts,
            paths,
            manifests,
        })
    }

    /// A corpus of the given `(path, contents)` files, for tests: the `.rs` ones as source, each
    /// [`is_manifest`] one as a manifest, the rest as scripts.
    #[cfg(test)]
    pub(crate) fn of(files: &[(&str, &str)]) -> Self {
        let (rs, other): (Vec<_>, Vec<_>) = files.iter().partition(|(rel, _)| rel.ends_with(".rs"));
        let (manifests, other): (Vec<_>, Vec<_>) =
            other.into_iter().partition(|(rel, _)| is_manifest(rel));
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
            manifests: manifests
                .iter()
                .map(|(rel, text)| ((*rel).to_string(), (*text).to_string()))
                .collect(),
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

    /// The files under `prefix` that compile only under `test`: each file a test-gated `mod`
    /// declaration names (`#[cfg(all(test, unix))] #[path = "x.rs"] mod x;`), and every module
    /// such a file declares, unless a shipped line also loads it.
    pub(crate) fn test_only(&self, prefix: &str) -> BTreeSet<String> {
        let prefixes = [prefix];
        let loads: Vec<(&str, Vec<Load>)> = self
            .under(&prefixes)
            .map(|f| (f.rel.as_str(), f.loads()))
            .collect();
        let mut out = BTreeSet::new();
        loop {
            let before = out.len();
            for (rel, loads) in &loads {
                let whole = out.contains(*rel);
                for load in loads.iter().filter(|l| whole || l.gated) {
                    for child in load.files.iter().filter(|c| self.get(c).is_some()) {
                        out.insert(child.clone());
                    }
                }
            }
            if out.len() == before {
                break;
            }
        }
        let shipped = self.shipped_loads(&prefixes, &|rel| out.contains(rel));
        out.retain(|file| !shipped.contains(file));
        out
    }

    /// The files a shipped line of a shipped file under `prefixes` loads
    /// ([`SourceFile::loads`]), where `test` says which files are test code
    /// by their parents' say: a file it names ships too once a shipped file
    /// loads it, and so does what that one loads.
    pub(crate) fn shipped_loads(
        &self,
        prefixes: &[&str],
        test: &dyn Fn(&str) -> bool,
    ) -> BTreeSet<String> {
        let loads: Vec<(&str, Vec<Load>)> = self
            .under(prefixes)
            .map(|f| (f.rel.as_str(), f.loads()))
            .collect();
        let mut shipped = BTreeSet::new();
        loop {
            let before = shipped.len();
            for (rel, loads) in &loads {
                if test(rel) && !shipped.contains(*rel) {
                    continue;
                }
                for load in loads.iter().filter(|l| !l.gated) {
                    for file in load.files.iter().filter(|f| self.get(f).is_some()) {
                        shipped.insert(file.clone());
                    }
                }
            }
            if shipped.len() == before {
                return shipped;
            }
        }
    }
}

/// A file one source file brings into its crate ([`SourceFile::loads`]): an
/// out-of-line `mod`, or an `include!` of a literal path.
pub(crate) struct Load {
    /// The files it can name: `x.rs` and `x/mod.rs` for a `mod x;` without a
    /// `path`, else the one file its `path` or literal names.
    pub(crate) files: Vec<String>,
    /// Outside the shipped lines: inside an item whose `cfg` proves it
    /// test-only.
    pub(crate) gated: bool,
    /// A `mod` at the file's top level: not one in an inline module or a
    /// block, and not an `include!`.
    pub(crate) top: bool,
}

/// A directory no walk enters: `.git`, build output (`target`, `target-*`,
/// `target.*`: what `.gitignore` keeps out by pattern) and `node_modules`.
fn skipped_dir(name: &str) -> bool {
    name == ".git"
        || name == "target"
        || name.starts_with("target-")
        || name.starts_with("target.")
        || name == "node_modules"
}

/// Is `rel` a Cargo manifest under [`CORPUS_DIRS`]?
fn is_manifest(rel: &str) -> bool {
    rel.ends_with("/Cargo.toml")
        && CORPUS_DIRS
            .iter()
            .any(|dir| rel.strip_prefix(dir).is_some_and(|r| r.starts_with('/')))
}

/// Is `rel` under a dot directory (`.cargo/…`, `crates/<name>/.scratch/…`)?
fn in_dot_dir(rel: &str) -> bool {
    rel.rsplit_once('/')
        .is_some_and(|(dirs, _)| dirs.split('/').any(|d| d.starts_with('.')))
}

/// The directories the root `.gitignore` keeps out by a plain directory row (`name/` at any
/// depth, `/path/` or `a/b/` from the root; no glob, no negation): tool state and caches
/// (`.claude/`, `/.trust-cache/`), whose files no commit takes without `-f`, so no walk needs
/// them.
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

/// Every file under `root/rel` ([`skipped_dir`]s and `ignored` directories aside) as a
/// root-relative, `/`-separated path.
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

/// A script the guards read as plain lines (comments included: a rule says when it skips
/// them).
pub(crate) struct Script {
    pub(crate) rel: String,
    pub(crate) raw: String,
}

/// The extensions that make a file a script.
const SCRIPT_EXTENSIONS: &[&str] = &["sh", "bash", "zsh", "py", "js", "mjs", "cjs"];

impl Script {
    /// Is the file at `rel` a script by its name: outside `vendor/` and with a
    /// script extension?
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
    const fn new(rel: String, raw: String) -> Self {
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

    /// The CODE view with every test-only line blanked, so an offset in it reads one line of the
    /// file and a rule can read a statement rustfmt wrapped as one.
    pub(crate) fn shipped_code(&self) -> String {
        let mut shipped = self.shipped_lines().map(|l| l.no).peekable();
        let mut code = String::with_capacity(self.raw.len());
        for line in self.lines() {
            if shipped.next_if_eq(&line.no).is_some() {
                code.push_str(line.code);
            } else {
                code.extend(std::iter::repeat_n(' ', line.code.len()));
            }
            code.push('\n');
        }
        code
    }

    /// Every file this one loads ([`Load`]), each with whether the load is
    /// test-gated: each out-of-line `mod`, at any depth and whatever its
    /// layout (`#[path = "x.rs"] mod y;` on one line), resolved as rustc
    /// resolves it — a `path` (`#[path]`, or `cfg_attr`'s) against the file's
    /// directory, or inside an inline module against that module's directory;
    /// else `name.rs` or `name/mod.rs` in the module's own directory — and each
    /// `include!` of a literal, against the file's directory.
    pub(crate) fn loads(&self) -> Vec<Load> {
        let Lexed { code, text } = self.lexed();
        let shipped = self
            .shipped
            .get_or_init(|| crate::shipped_lines(&self.lexed().code));
        let starts: Vec<usize> = std::iter::once(0)
            .chain(code.match_indices('\n').map(|(at, _)| at + 1))
            .collect();
        let gated = |at: usize| {
            let line = starts.partition_point(|&start| start <= at) - 1;
            !shipped.get(line).copied().unwrap_or(true)
        };
        let dir = self.rel.rsplit_once('/').map_or("", |(dir, _)| dir);
        let own = match self.name() {
            "lib.rs" | "main.rs" | "mod.rs" => dir.to_string(),
            name => format!("{dir}/{}", name.trim_end_matches(".rs")),
        };
        // Every `mod <name>`: a declaration, or an inline module's body (its
        // braces, and the directory its children resolve in).
        let mut decls: Vec<(usize, &str, Option<&str>)> = Vec::new();
        let mut inline: Vec<(usize, usize, &str)> = Vec::new();
        for at in hits(code, "mod", Bound::Word) {
            let rest = &code[at + 3..];
            let name_at = at + 3 + (rest.len() - rest.trim_start().len());
            let len = code[name_at..].bytes().take_while(|&b| is_word(b)).count();
            if name_at == at + 3 || len == 0 {
                continue;
            }
            let name = &code[name_at..name_at + len];
            let after = code[name_at + len..].trim_start();
            let path = attr_path(code, text, at);
            if after.starts_with(';') {
                decls.push((at, name, path));
            } else if after.starts_with('{') {
                let open = code.len() - after.len();
                if let Some(close) = brace_close(code, open) {
                    inline.push((open, close, path.unwrap_or(name)));
                }
            }
        }
        let depth = depths_at(code, decls.iter().map(|d| d.0));
        let mut out = Vec::new();
        for (&(at, name, path), depth) in decls.iter().zip(depth) {
            let chain: Vec<&str> = inline
                .iter()
                .filter(|&&(open, close, _)| open < at && at < close)
                .map(|&(_, _, dir)| dir)
                .collect();
            let base = if chain.is_empty() {
                own.clone()
            } else {
                format!("{own}/{}", chain.join("/"))
            };
            let files = match path {
                Some(path) if chain.is_empty() => vec![joined(dir, path)],
                Some(path) => vec![joined(&base, path)],
                None => vec![format!("{base}/{name}.rs"), format!("{base}/{name}/mod.rs")],
            };
            out.push(Load {
                files,
                gated: gated(at),
                top: depth == 0,
            });
        }
        for at in hits(code, "include", Bound::Word) {
            let Some(args) = code[at + 7..].trim_start().strip_prefix('!') else {
                continue;
            };
            let args = args.trim_start();
            if !args.starts_with(['(', '[', '{']) {
                continue;
            }
            // A literal first: `"…"`, or a raw `r"…"`, `r#"…"#`.
            let lit = args[1..].trim_start();
            let raw = lit
                .strip_prefix('r')
                .map_or(lit, |r| r.trim_start_matches('#'));
            if !raw.starts_with('"') {
                continue;
            }
            let open = code.len() - raw.len() + 1;
            let Some(path) = text[open..].split_once('"').map(|(p, _)| p) else {
                continue;
            };
            out.push(Load {
                files: vec![joined(dir, path)],
                gated: gated(at),
                top: false,
            });
        }
        out
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

/// The path the attributes of the item whose keyword is at `at` name, if
/// any: `path = "…"` in any of them (`#[path = "x.rs"]`,
/// `#[cfg_attr(unix, path = "x.rs")]`), found in the CODE view and read from
/// the TEXT view. The attributes are what lies between the item before (its
/// `;`, `}` or a block's `{`) and the keyword.
fn attr_path<'a>(code: &str, text: &'a str, at: usize) -> Option<&'a str> {
    let start = code[..at].rfind([';', '}', '{']).map_or(0, |b| b + 1);
    hits(&code[..at], "path", Bound::Word)
        .filter(|&p| p >= start)
        .find_map(|p| {
            let value = code[p + 4..at].trim_start().strip_prefix('=')?.trim_start();
            // `value` runs to `at`: its `"` sits `value.len()` bytes before it.
            value.starts_with('"').then_some(())?;
            let open = at - value.len() + 1;
            text[open..at].split_once('"').map(|(path, _)| path)
        })
}

/// The `}` that closes the `{` at `open` in a CODE view (whose literals and
/// comments hold no brace), if it closes.
fn brace_close(code: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, b) in code.bytes().enumerate().skip(open) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// The brace depth at each of `positions` (ascending) in a CODE view.
fn depths_at(code: &str, positions: impl Iterator<Item = usize>) -> Vec<i64> {
    let b = code.as_bytes();
    let (mut depth, mut from) = (0i64, 0usize);
    positions
        .map(|at| {
            for &c in &b[from..at] {
                match c {
                    b'{' => depth += 1,
                    b'}' => depth -= 1,
                    _ => {}
                }
            }
            from = at;
            depth
        })
        .collect()
}

/// `path` under the directory `dir`, `..` and `.` resolved.
fn joined(dir: &str, path: &str) -> String {
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    parts.join("/")
}

pub(crate) const fn is_word(b: u8) -> bool {
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

/// Every name a line calls: the identifier before each `(` (spaces between allowed), whether it
/// is a free call, a path's last segment or a method.
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

    /// A file is test-only when its parent declares it under a test `cfg`,
    /// by its own name, its `mod.rs` or a `#[path]`, or when a test-only file
    /// declares it.
    #[test]
    fn a_module_its_parent_gates_on_test_is_test_only() {
        let corpus = Corpus::of(&[
            (
                "crates/x/src/lib.rs",
                "#[cfg(test)]\nmod a;\n#[cfg(all(test, unix))]\n#[path = \"rig_conformance.rs\"]\n\
                 pub(crate) mod rig;\n#[cfg(not(test))]\nmod live;\nmod plain;\n",
            ),
            ("crates/x/src/a.rs", "mod b;\n"),
            ("crates/x/src/a/b/mod.rs", ""),
            ("crates/x/src/rig_conformance.rs", ""),
            ("crates/x/src/live.rs", ""),
            (
                "crates/x/src/plain.rs",
                "#[cfg(test)]\nmod tests;\nmod inner;\n",
            ),
            ("crates/x/src/plain/tests.rs", ""),
            ("crates/x/src/plain/inner.rs", ""),
        ]);
        let found: Vec<String> = corpus.test_only("crates/x/").into_iter().collect();
        assert_eq!(
            found,
            [
                "crates/x/src/a.rs",
                "crates/x/src/a/b/mod.rs",
                "crates/x/src/plain/tests.rs",
                "crates/x/src/rig_conformance.rs",
            ]
        );
    }

    /// A file a shipped line also loads ships, and so does what it declares; a file only a
    /// test-only file loads stays test-only. Each load resolves as rustc resolves it: an
    /// attribute on the `mod`'s own line, a `cfg_attr` path, an inline module's directory, an
    /// `include!` literal.
    #[test]
    fn a_test_module_a_shipped_line_also_loads_ships() {
        let corpus = Corpus::of(&[
            (
                "crates/x/src/lib.rs",
                "#[cfg(test)]\nmod a;\n#[cfg(test)]\nmod b;\n#[path = \"b.rs\"] mod b_again;\n\
                 mod outer {\n    #[cfg_attr(unix, path = \"o.rs\")]\n    mod inner;\n    \
                 mod plain;\n}\ninclude!(\"inc.rs\");\n#[cfg(test)]\nmod c {\n    \
                 include!(\"c_inc.rs\");\n}\n",
            ),
            ("crates/x/src/a.rs", "mod a_child;\n"),
            ("crates/x/src/a/a_child.rs", ""),
            ("crates/x/src/b.rs", "mod b_child;\n"),
            ("crates/x/src/b/b_child.rs", ""),
            ("crates/x/src/outer/o.rs", ""),
            ("crates/x/src/outer/plain.rs", ""),
            ("crates/x/src/inc.rs", ""),
            ("crates/x/src/c_inc.rs", ""),
        ]);
        let found: Vec<String> = corpus.test_only("crates/x/").into_iter().collect();
        assert_eq!(
            found,
            [
                "crates/x/src/a.rs",
                "crates/x/src/a/a_child.rs",
                "crates/x/src/c_inc.rs",
            ]
        );
        let lib = corpus.get("crates/x/src/lib.rs").map(SourceFile::loads);
        let loads: Vec<(Vec<String>, bool, bool)> = lib
            .unwrap_or_default()
            .into_iter()
            .map(|l| (l.files, l.gated, l.top))
            .collect();
        let s = |p: &str| p.to_string();
        assert_eq!(
            loads,
            [
                (
                    vec![s("crates/x/src/a.rs"), s("crates/x/src/a/mod.rs")],
                    true,
                    true
                ),
                (
                    vec![s("crates/x/src/b.rs"), s("crates/x/src/b/mod.rs")],
                    true,
                    true
                ),
                (vec![s("crates/x/src/b.rs")], false, true),
                (vec![s("crates/x/src/outer/o.rs")], false, false),
                (
                    vec![
                        s("crates/x/src/outer/plain.rs"),
                        s("crates/x/src/outer/plain/mod.rs")
                    ],
                    false,
                    false
                ),
                (vec![s("crates/x/src/inc.rs")], false, false),
                (vec![s("crates/x/src/c_inc.rs")], true, false),
            ]
        );
    }

    /// The corpus fails closed: a source directory its `.rs` walk may not list
    /// refuses the read, never a corpus of what it could read.
    #[test]
    fn a_source_directory_it_cannot_list_refuses_the_corpus() {
        let read = |fault: fn(&crate::tree::memory::Tree)| {
            let root = crate::scan_set::test_fixtures::synth_tree(
                "aterm-census-guards",
                "unlisted",
                &[("crates/a/src/notes.md".to_string(), String::new())],
            );
            fault(&root);
            Corpus::read(&root).map(|corpus| corpus.files.len())
        };
        assert_eq!(read(|_| {}), Ok(0));
        assert!(read(|root| root.unlistable("crates/a/src")).is_err());
    }

    /// The walk enters dot directories for paths (never `.git`, nor one
    /// `.gitignore` keeps out by directory), and reads scripts only outside
    /// them.
    #[test]
    fn the_walk_lists_dot_directories_but_reads_no_script_there() {
        use crate::scan_set::test_fixtures::disk_tree;
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
        let root = disk_tree("aterm-census-guards", "walk-dot-dirs", &files);
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
