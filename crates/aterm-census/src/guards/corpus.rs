// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The source the guards read and the tree a rule is handed. Every accessor a rule reads them by
//! records its question in the memo unit the rule runs as ([`crate::memo`]): a file's content,
//! whether it mentions a needle set, what a prefix lists, whether a file is in the corpus,
//! whether one is test code, and the corpus-wide facts (every path, the scripts, the manifests,
//! the engine's baseline). A view one rule computed and another reads (a lexing, a file's loads)
//! is recorded by the reader too, since each accessor records before it reads its cache.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use super::{Bound, Finding, hits, is_word, rules};
use crate::lex::{Lexed, lex};
use crate::memo::{self, Digest, Shared, Snapshot, Trace, global};

/// What a rule reads: the checkout root (for what it reads through [`crate::tree`]) and the
/// corpus.
pub(crate) struct Tree<'a> {
    pub(crate) root: &'a Path,
    pub(crate) corpus: &'a Corpus,
    /// The publication engine's baseline the caller read ([`crate::EngineBaseline`]).
    engine: &'a crate::EngineBaseline,
}

impl<'a> Tree<'a> {
    pub(crate) const fn new(
        root: &'a Path,
        corpus: &'a Corpus,
        engine: &'a crate::EngineBaseline,
    ) -> Self {
        Self {
            root,
            corpus,
            engine,
        }
    }

    /// The publication engine's baseline the caller read.
    pub(crate) fn engine(&self) -> &'a crate::EngineBaseline {
        self.corpus.note(|t| t.global(global::ENGINE));
        self.engine
    }

    /// The text at root-relative `rel`: the corpus's copy when it holds the file (as source, a
    /// manifest or a script), else the checkout's; `None` when the checkout's cannot be read.
    pub(crate) fn text(&self, rel: &str) -> Option<Cow<'a, str>> {
        if let Some(file) = self.corpus.get(rel) {
            return Some(Cow::Borrowed(file.raw()));
        }
        if let Some((_, raw)) = self.corpus.manifests().iter().find(|(p, _)| p == rel) {
            return Some(Cow::Borrowed(raw.as_str()));
        }
        if let Some(script) = self.corpus.scripts().iter().find(|s| s.rel == rel) {
            return Some(Cow::Borrowed(script.raw.as_str()));
        }
        crate::tree::read_to_string(&self.root.join(rel))
            .ok()
            .map(Cow::Owned)
    }
}

/// The source the guards read, each file lexed at most once.
pub(crate) struct Corpus {
    /// By path.
    files: Vec<SourceFile>,
    /// Every script of the checkout ([`Script::is_script`]).
    scripts: Vec<Script>,
    /// Every file of the checkout, root-relative ([`walk`]).
    paths: Vec<String>,
    /// Every Cargo manifest under [`CORPUS_DIRS`] ([`is_manifest`]), root-relative, with its
    /// text.
    manifests: Vec<(String, String)>,
    /// Which files are test code that nothing ships ([`Corpus::test_code`]), read once.
    test_code: OnceLock<rules::TestCode>,
    /// The memo run whose units' questions the corpus answers, when one runs.
    owner: Option<Arc<Shared>>,
}

/// The directories whose `.rs` files make the corpus.
const CORPUS_DIRS: &[&str] = memo::CORPUS_DIRS;

impl Corpus {
    /// Every `.rs` under [`CORPUS_DIRS`] at `root` (`target/` and dot directories skipped) as
    /// `snapshot` read it, failing as a walk in path order fails first; then every file of the
    /// checkout, the scripts among them, and the manifests among them under [`CORPUS_DIRS`].
    /// With `owner`, each accessor records its question in the unit its caller runs as.
    pub(crate) fn read(
        root: &Path,
        snapshot: &Snapshot,
        owner: Option<Arc<Shared>>,
    ) -> Result<Self, String> {
        for dir in CORPUS_DIRS {
            if let Some((_, why)) = snapshot.unwalked.iter().find(|(d, _)| d == dir) {
                return Err(why.clone());
            }
        }
        if let Some(e) = snapshot
            .path_order
            .iter()
            .map(|&at| &snapshot.files[at])
            .filter(|f| memo::in_corpus(&f.rel))
            .find_map(|f| f.text.as_ref().err())
        {
            return Err(e.clone());
        }
        let files = snapshot
            .files
            .iter()
            .enumerate()
            .filter(|(_, f)| memo::in_corpus(&f.rel))
            .filter_map(|(at, f)| {
                let raw = f.text.as_ref().ok()?;
                Some(SourceFile::new(
                    at,
                    f.rel.clone(),
                    Arc::clone(raw),
                    owner.clone(),
                ))
            })
            .collect();
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
                let text = crate::tree::read_to_string(&root.join(rel))
                    .map_err(|e| format!("cannot read {rel}: {e}"))?;
                manifests.push((rel.clone(), text));
            }
        }
        Ok(Self {
            files,
            scripts,
            paths,
            manifests,
            test_code: OnceLock::new(),
            owner,
        })
    }

    /// The corpus of the checkout at `root`, read with no memo run, for tests.
    #[cfg(test)]
    pub(crate) fn at(root: &Path) -> Result<Self, String> {
        Self::read(root, &Snapshot::read(root), None)
    }

    /// A corpus of the given `(path, contents)` files, for tests: the `.rs` ones as source, each
    /// [`is_manifest`] one as a manifest, the rest as scripts.
    #[cfg(test)]
    pub(crate) fn of(files: &[(&str, &str)]) -> Self {
        let (rs, other): (Vec<_>, Vec<_>) = files.iter().partition(|(rel, _)| rel.ends_with(".rs"));
        let (manifests, other): (Vec<_>, Vec<_>) =
            other.into_iter().partition(|(rel, _)| is_manifest(rel));
        let mut rs: Vec<(&str, &str)> = rs;
        rs.sort_unstable();
        let rs_files: Vec<SourceFile> = rs
            .iter()
            .enumerate()
            .map(|(at, (rel, raw))| SourceFile::new(at, (*rel).to_string(), Arc::from(*raw), None))
            .collect();
        Self {
            files: rs_files,
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
            test_code: OnceLock::new(),
            owner: None,
        }
    }

    /// Records `ask` in the unit this thread runs as, when a memo run reads the corpus.
    pub(crate) fn note(&self, ask: impl FnOnce(&mut Trace)) {
        memo::note(self.owner.as_ref(), ask);
    }

    /// Which files are test code that nothing ships ([`rules::TestCode::read`]), read once for
    /// every guard that leaves test code out; each answer is a question of the unit that asks.
    pub(crate) fn test_code(&self) -> TestCodeView<'_> {
        TestCodeView {
            corpus: self,
            code: self.test_code_now(),
        }
    }

    /// The corpus's [`rules::TestCode`], computed outside any unit's questions: each unit asks
    /// about the files it names ([`TestCodeView::holds`]).
    pub(super) fn test_code_now(&self) -> &rules::TestCode {
        self.test_code
            .get_or_init(|| memo::unrecorded(|| rules::TestCode::read(self)))
    }

    /// The `.rs` files read.
    pub(crate) fn len(&self) -> usize {
        self.files.len()
    }

    /// Every script of the checkout ([`Script::is_script`]).
    pub(crate) fn scripts(&self) -> &[Script] {
        self.note(|t| t.global(global::SCRIPTS));
        &self.scripts
    }

    /// Every file of the checkout, root-relative ([`walk`]), in path order.
    pub(crate) fn paths(&self) -> &[String] {
        self.note(|t| t.global(global::PATHS));
        &self.paths
    }

    /// Every Cargo manifest under [`CORPUS_DIRS`] ([`is_manifest`]), root-relative, with its
    /// text.
    pub(crate) fn manifests(&self) -> &[(String, String)] {
        self.note(|t| t.global(global::MANIFESTS));
        &self.manifests
    }

    /// The digests of [`Self::paths`], [`Self::scripts`] and [`Self::manifests`], which a unit
    /// that read one is judged by.
    pub(crate) fn facts(&self) -> [Digest; 3] {
        let mut paths: Vec<&[u8]> = vec![b"paths"];
        paths.extend(self.paths.iter().map(|p| p.as_bytes()));
        let mut scripts: Vec<&[u8]> = vec![b"scripts"];
        for s in &self.scripts {
            scripts.push(s.rel.as_bytes());
            scripts.push(s.raw.as_bytes());
        }
        let mut manifests: Vec<&[u8]> = vec![b"manifests"];
        for (rel, text) in &self.manifests {
            manifests.push(rel.as_bytes());
            manifests.push(text.as_bytes());
        }
        [
            memo::digest(&paths),
            memo::digest(&scripts),
            memo::digest(&manifests),
        ]
    }

    /// Every file whose path starts with one of `prefixes`.
    pub(crate) fn under<'a>(
        &'a self,
        prefixes: &'a [&'a str],
    ) -> impl Iterator<Item = &'a SourceFile> + 'a {
        self.note(|t| t.listing(prefixes));
        let slice = if let [p] = prefixes {
            let start = self.files.partition_point(|f| f.rel.as_str() < *p);
            let end = start + self.files[start..].partition_point(|f| f.rel.starts_with(p));
            &self.files[start..end]
        } else {
            &self.files[..]
        };
        slice
            .iter()
            .filter(move |f| prefixes.len() == 1 || prefixes.iter().any(|p| f.rel.starts_with(p)))
    }

    /// The file at `rel`, if the corpus holds it.
    pub(crate) fn get(&self, rel: &str) -> Option<&SourceFile> {
        let found = self
            .files
            .binary_search_by(|f| f.rel.as_str().cmp(rel))
            .ok()
            .map(|i| &self.files[i]);
        self.note(|t| t.has(rel, found.is_some()));
        found
    }
}

/// The corpus's test code, as one unit asks about it ([`Corpus::test_code`]).
#[derive(Clone, Copy)]
pub(crate) struct TestCodeView<'a> {
    corpus: &'a Corpus,
    code: &'a rules::TestCode,
}

impl TestCodeView<'_> {
    /// Is `rel` test code that nothing ships ([`rules::TestCode::holds`])?
    pub(crate) fn holds(&self, rel: &str) -> bool {
        let answer = self.code.holds(rel);
        self.corpus.note(|t| t.test_code(rel, answer));
        answer
    }
}

impl Corpus {
    /// The files a shipped line of a shipped file under `prefixes` loads
    /// ([`SourceFile::loads`]), where `test` says which files are test code
    /// by their parents' say. Such a file, one [`named_for_tests`] and one
    /// [`in_dev_target`] ship nothing they load until a shipped file loads
    /// them, and then so does what they load: a test target's, a bench's or
    /// an example's `mod` or `#[path]` ships nothing.
    pub(crate) fn shipped_loads(
        &self,
        prefixes: &[&str],
        test: &dyn Fn(&str) -> bool,
    ) -> BTreeSet<String> {
        let loads: Vec<(&str, &[Load])> = self
            .under(prefixes)
            .map(|f| (f.rel.as_str(), f.loads()))
            .collect();
        let mut shipped = BTreeSet::new();
        loop {
            let before = shipped.len();
            for (rel, loads) in &loads {
                let unshipped = test(rel) || named_for_tests(rel) || in_dev_target(rel);
                if unshipped && !shipped.contains(*rel) {
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

/// Does `rel`'s path name test code: a file in a `tests` directory (a test target's, or a
/// `tests` module's), a `tests.rs` or a `<name>_tests.rs`? Such a file is test code unless a
/// shipped line loads it ([`rules::TestCode::holds`]). A name that only opens with `test` says
/// nothing: `testrun.rs` and `test_binary.rs` can ship.
pub(crate) fn named_for_tests(rel: &str) -> bool {
    let (dirs, name) = rel.rsplit_once('/').unwrap_or(("", rel));
    dirs.split('/').any(|dir| dir == "tests") || name == "tests.rs" || name.ends_with("_tests.rs")
}

/// Is `rel` in a `benches` or `examples` directory: a dev target's, which only `cargo bench` or
/// `cargo run --example` builds? What it loads ships only once a shipped line loads the file
/// itself ([`Corpus::shipped_loads`]); the file is no test code, so a rule that reads dev targets
/// still reads it.
pub(crate) fn in_dev_target(rel: &str) -> bool {
    let dirs = rel.rsplit_once('/').map_or("", |(dirs, _)| dirs);
    dirs.split('/')
        .any(|dir| dir == "benches" || dir == "examples")
}

/// How a [`Load`] brings its files into the crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LoadKind {
    /// A body-less `mod x;` without an explicit `path` attribute.
    Mod,
    /// A body-less `mod x;` whose path is given by `#[path = "…"]` or `#[cfg_attr(…, path = "…")]`.
    PathMod,
    /// An `include!("…")` splice.
    Include,
}

/// A file one source file brings into its crate ([`SourceFile::loads`]): an
/// out-of-line `mod`, or an `include!` of a literal path.
pub(crate) struct Load {
    /// The files it can name: `x.rs` and `x/mod.rs` for a `mod x;` without a
    /// `path`, else the files its `path` attributes or `include!` literal name.
    pub(crate) files: Vec<String>,
    /// How the loader brings [`Self::files`] in.
    pub(crate) kind: LoadKind,
    /// Declared logical module segments, including enclosing inline modules. Includes
    /// splice into the caller and have no independently proven module identity.
    pub(crate) module: Option<Vec<String>>,
    /// Outside the shipped lines: inside an item whose `cfg` proves it
    /// test-only.
    pub(crate) gated: bool,
}

/// A directory no walk enters: `.git`, build output (`target`, `target-*`,
/// `target.*`: what `.gitignore` keeps out by pattern) and `node_modules`.
pub(crate) fn skipped_dir(name: &str) -> bool {
    name == ".git"
        || name == "target"
        || name.starts_with("target-")
        || name.starts_with("target.")
        || name == "node_modules"
}

/// Is `rel` a Cargo manifest under [`CORPUS_DIRS`]?
pub(crate) fn is_manifest(rel: &str) -> bool {
    rel.ends_with("/Cargo.toml")
        && CORPUS_DIRS
            .iter()
            .any(|dir| rel.strip_prefix(dir).is_some_and(|r| r.starts_with('/')))
}

/// Is `rel` under a dot directory (`.cargo/…`, `crates/<name>/.scratch/…`)?
pub(crate) fn in_dot_dir(rel: &str) -> bool {
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
        Self::parse(&crate::tree::read_to_string(&root.join(".gitignore")).unwrap_or_default())
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
        let bytes = if by_name.is_none() {
            use std::io::Read as _;
            let mut head = [0u8; 2];
            let (mut file, cap) = crate::tree::open_regular(&path).map_err(unreadable)?;
            if file.read_exact(&mut head).is_err() || head != *b"#!" {
                return Ok(None);
            }
            let mut buf = Vec::with_capacity(cap.saturating_add(1));
            buf.extend_from_slice(&head);
            file.take(crate::tree::MAX_FILE_BYTES.saturating_sub(1))
                .read_to_end(&mut buf)
                .map_err(unreadable)?;
            if u64::try_from(buf.len()).unwrap_or(u64::MAX) > crate::tree::MAX_FILE_BYTES {
                return Err(format!(
                    "cannot read {rel}: file exceeds {} bytes",
                    crate::tree::MAX_FILE_BYTES
                ));
            }
            buf
        } else {
            crate::tree::read_bytes(&path).map_err(unreadable)?
        };
        if bytes.contains(&0) {
            return Ok(None);
        }
        let raw = String::from_utf8(bytes)
            .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
        Ok(Some(Self {
            rel: rel.to_string(),
            raw,
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
    /// Its index in the corpus, which is its index in the memo run's snapshot.
    at: u32,
    pub(crate) rel: String,
    raw: Arc<str>,
    lexed: OnceLock<Lexed>,
    shipped: OnceLock<Vec<bool>>,
    loads: OnceLock<Vec<Load>>,
    owner: Option<Arc<Shared>>,
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
    fn new(at: usize, rel: String, raw: Arc<str>, owner: Option<Arc<Shared>>) -> Self {
        Self {
            at: u32::try_from(at).unwrap_or(u32::MAX),
            rel,
            raw,
            lexed: OnceLock::new(),
            shipped: OnceLock::new(),
            loads: OnceLock::new(),
            owner,
        }
    }

    /// A file outside the corpus, read by its caller through [`crate::tree`] (which recorded
    /// that read): its views answer no further question.
    pub(crate) fn outside(rel: String, raw: impl Into<Arc<str>>) -> Self {
        Self::new(0, rel, raw.into(), None)
    }

    /// Records that the unit this thread runs as read the file's content.
    fn read(&self) {
        let at = self.at;
        memo::note(self.owner.as_ref(), |t| t.content(at));
    }

    /// The file's bytes.
    pub(crate) fn raw(&self) -> &str {
        self.read();
        &self.raw
    }

    pub(crate) fn lexed(&self) -> &Lexed {
        self.read();
        self.lexed.get_or_init(|| lex(&self.raw))
    }

    /// Does the raw file hold any of `needles`? A cheap prefilter before lexing.
    pub(crate) fn mentions(&self, needles: &[&str]) -> bool {
        let answer = memo::mentions_in(&self.raw, needles);
        let at = self.at;
        memo::note(self.owner.as_ref(), |t| t.mentions(at, needles, answer));
        answer
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
        let shipped = self.shipped_flags();
        self.lines()
            .filter(move |l| shipped.get(l.no - 1).copied().unwrap_or(true))
    }

    /// Whether each line ships, by its 0-based index (a line past the end ships): the flags
    /// [`Self::shipped_lines`] filters by.
    pub(crate) fn shipped_flags(&self) -> &[bool] {
        self.read();
        self.shipped.get_or_init(|| crate::shipped_lines(&self.raw))
    }

    /// The CODE view with every test-only line blanked, so an offset in it reads one line of the
    /// file and a rule can read a statement rustfmt wrapped as one.
    pub(crate) fn shipped_code(&self) -> String {
        let mut shipped = self.shipped_lines().map(|l| l.no).peekable();
        let mut code = String::with_capacity(self.raw().len());
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
    pub(crate) fn loads(&self) -> &[Load] {
        self.read();
        self.loads.get_or_init(|| {
            let dir_owner = matches!(self.name(), "lib.rs" | "main.rs" | "mod.rs");
            let manifest_dir = self
                .rel
                .find("/src/")
                .or_else(|| self.rel.find("/tests/"))
                .or_else(|| self.rel.find("/benches/"))
                .or_else(|| self.rel.find("/examples/"))
                .map(|i| &self.rel[..i]);
            self.loads_in(dir_owner, manifest_dir)
        })
    }

    /// Like [`Self::loads`], with `dir_owner` true when `self` owns its parent directory directly
    /// (a crate root such as `tests/suite.rs`, `mod.rs`, or a `#[path]`-mounted module) and
    /// `manifest_dir` naming the compiling package's `CARGO_MANIFEST_DIR`.
    pub(crate) fn loads_in(&self, dir_owner: bool, manifest_dir: Option<&str>) -> Vec<Load> {
        if !self.mentions(&["mod", "include"]) {
            return Vec::new();
        }
        let Lexed { code, text } = self.lexed();
        // Every `mod <name>`: a declaration, or an inline module's body (its
        // braces, and the directory its children resolve in).
        let mut decls: Vec<(usize, &str, Vec<&str>)> = Vec::new();
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
            let paths = attr_paths(code, text, at);
            if after.starts_with(';') {
                decls.push((at, name, paths));
            } else if after.starts_with('{') {
                let open = code.len() - after.len();
                if let Some(close) = brace_close(code, open) {
                    inline.push((open, close, paths.first().copied().unwrap_or(name)));
                }
            }
        }
        if decls.is_empty() && !code.contains("include") {
            return Vec::new();
        }
        let shipped = self.shipped.get_or_init(|| crate::shipped_lines(&self.raw));
        let starts: Vec<usize> = std::iter::once(0)
            .chain(code.match_indices('\n').map(|(at, _)| at + 1))
            .collect();
        let gated = |at: usize| {
            let line = starts.partition_point(|&start| start <= at) - 1;
            !shipped.get(line).copied().unwrap_or(true)
        };
        let dir = self.rel.rsplit_once('/').map_or("", |(dir, _)| dir);
        let own = if dir_owner || self.name() == "mod.rs" {
            dir.to_string()
        } else {
            format!("{dir}/{}", self.name().trim_end_matches(".rs"))
        };
        let mut out = Vec::new();
        for (at, name, paths) in &decls {
            let chain: Vec<&str> = inline
                .iter()
                .filter(|&&(open, close, _)| open < *at && *at < close)
                .map(|&(_, _, dir)| dir)
                .collect();
            let base = if chain.is_empty() {
                own.clone()
            } else {
                format!("{own}/{}", chain.join("/"))
            };
            let (files, kind) = if paths.is_empty() {
                (
                    vec![format!("{base}/{name}.rs"), format!("{base}/{name}/mod.rs")],
                    LoadKind::Mod,
                )
            } else {
                let root_dir = if chain.is_empty() { dir } else { base.as_str() };
                (
                    paths.iter().map(|p| joined(root_dir, p)).collect(),
                    LoadKind::PathMod,
                )
            };
            out.push(Load {
                files,
                kind,
                module: Some(
                    chain
                        .iter()
                        .copied()
                        .chain(std::iter::once(*name))
                        .map(str::to_owned)
                        .collect(),
                ),
                gated: gated(*at),
            });
        }
        include_loads(code, text, dir, manifest_dir, &gated, &mut out);
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

/// Every literal-path or `CARGO_MANIFEST_DIR`-rooted `include!(…)` in `code`/`text`.
fn include_loads(
    code: &str,
    text: &str,
    dir: &str,
    manifest_dir: Option<&str>,
    gated: &dyn Fn(usize) -> bool,
    out: &mut Vec<Load>,
) {
    for at in hits(code, "include", Bound::Word) {
        let Some(args) = code[at + 7..].trim_start().strip_prefix('!') else {
            continue;
        };
        let args = args.trim_start();
        if !args.starts_with(['(', '[', '{']) {
            continue;
        }
        let lit = args[1..].trim_start();
        if let Some((suffix, _)) = manifest_concat_include(code, text, lit) {
            if let Some(mdir) = manifest_dir {
                out.push(Load {
                    files: vec![joined(mdir, suffix.trim_start_matches('/'))],
                    kind: LoadKind::Include,
                    module: None,
                    gated: gated(at),
                });
            }
            continue;
        }
        // A literal first: `"…"`, or a raw `r"…"`, `r#"…"#`.
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
            kind: LoadKind::Include,
            module: None,
            gated: gated(at),
        });
    }
}

/// Matches `concat!(env!("CARGO_MANIFEST_DIR"), "…")` at suffix `lit` of `code`.
fn manifest_concat_include<'a>(
    code: &'a str,
    text: &'a str,
    lit: &'a str,
) -> Option<(&'a str, &'a str)> {
    let s = lit
        .strip_prefix("concat")?
        .trim_start()
        .strip_prefix('!')?
        .trim_start();
    let s = s.strip_prefix(['(', '[', '{'])?.trim_start();
    let s = s
        .strip_prefix("env")?
        .trim_start()
        .strip_prefix('!')?
        .trim_start();
    let s = s.strip_prefix(['(', '[', '{'])?.trim_start();
    let (var, rest) = plain_str_at(code, text, s)?;
    if var != "CARGO_MANIFEST_DIR" {
        return None;
    }
    let rest = rest
        .trim_start()
        .strip_prefix([')', ']', '}'])?
        .trim_start()
        .strip_prefix(',')?
        .trim_start();
    let (suffix, tail) = plain_str_at(code, text, rest)?;
    let tail = tail.trim_start();
    let tail = tail.strip_prefix(',').map_or(tail, str::trim_start);
    tail.starts_with([')', ']', '}']).then_some((suffix, tail))
}

/// Reads a plain `"…"` literal at suffix `s` of `code`, returning `(literal_text, after_close_in_code)`.
fn plain_str_at<'a>(code: &'a str, text: &'a str, s: &'a str) -> Option<(&'a str, &'a str)> {
    s.starts_with('"').then_some(())?;
    let open = code.len() - s.len() + 1;
    let len = code[open..].find('"')?;
    Some((&text[open..open + len], &code[open + len + 1..]))
}

/// Every `path = "…"` literal on the attributes of the item whose keyword is at
/// `at` (`#[path = "x.rs"]`, `#[cfg_attr(unix, path = "x.rs")]`), found in the
/// CODE view and read from the TEXT view. The attributes are what lies between
/// the item before (its `;`, `}` or a block's `{`) and the keyword.
fn attr_paths<'a>(code: &str, text: &'a str, at: usize) -> Vec<&'a str> {
    let start = code[..at].rfind([';', '}', '{']).map_or(0, |b| b + 1);
    hits(&code[..at], "path", Bound::Word)
        .filter(|&p| p >= start)
        .filter_map(|p| {
            let value = code[p + 4..at].trim_start().strip_prefix('=')?.trim_start();
            // Notice `code[p + 4..at]` may hold later attributes after the closing `"`:
            // `value` opens with `"` at `at - value.len()`, and `text` holds the literal.
            value.starts_with('"').then_some(())?;
            let open = at - value.len() + 1;
            text[open..at].split_once('"').map(|(path, _)| path)
        })
        .collect()
}

/// The `}` that closes the `{` at `open` in a CODE view (whose literals and
/// comments hold no brace), if it closes.
pub(crate) fn brace_close(code: &str, open: usize) -> Option<usize> {
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

/// `path` under the directory `dir`, `..` and `.` resolved.
pub(crate) fn joined(dir: &str, path: &str) -> String {
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
