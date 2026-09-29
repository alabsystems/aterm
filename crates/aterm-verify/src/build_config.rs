// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BUILD CONFIGURATION IN FILES (2026-09-28): the cargo config files a
//! run's builds read, recorded as a receipt's `build-config` line.
//!
//! WHY. A receipt's `build-env` line ([`crate::differential::build_env`])
//! records what the CALLER'S ENVIRONMENT gave the run's builds — `RUSTFLAGS`,
//! a wrapper, a profile override — so a base made under other flags is no
//! base. But cargo takes as much from FILES: every `.cargo/config` and
//! `.cargo/config.toml` in the directory it runs in and in every ancestor of
//! it, and `$CARGO_HOME`'s own (`~/.cargo` when `CARGO_HOME` is unset). A
//! `rustflags`, a `[profile]` override, a `runner` or an `[env]` table in any
//! of them builds or runs other code, exactly as the variable would — and none
//! of it reached any receipt, so a base made under a `~/.cargo/config.toml`
//! that switched `debug-assertions` off, or an ancestor directory's config
//! adding `--cfg`, judged a branch built without it: fail OPEN, the gap the
//! `build-env` line closed for variables, left open for files.
//!
//! WHAT IS RECORDED ([`record`]): every config file cargo would read for a
//! build run in the gate's root, in cargo's order (the root's own, then each
//! ancestor's, then `$CARGO_HOME`'s when the walk did not already pass it), and
//! every file one of them `include`s, each as `<label>=<git blob id>` of its
//! bytes. The LABEL is the file's role, never its absolute path, so two
//! checkouts — two worktrees, two machines, the snapshot and the caller —
//! with identical files record identical lines: `repo/.cargo/config.toml`
//! (the repository's own, whose CONTENT is recorded like any other's), an
//! `ancestor/.cargo/<name>` for each one above it, `cargo-home/<name>`, and
//! `<label>+include:<path as written>` for an included file (`=missing` when
//! it does not exist). `none` when cargo would read no file at all. Includes
//! are read as TOML, in both of cargo's shapes (the `include` key and
//! `[[include]]` tables, [`includes`]); one the reader cannot name makes the
//! line unsayable, never shorter.
//!
//! ONLY THE ROOT'S BUILDS (a stated gap). The walk starts at the gate's root,
//! so a lane that runs cargo from a subdirectory holding a `.cargo/config*`
//! of its own (`libc-oracle/` has one) also reads that file, and it is not on
//! the line. It is part of the source state — tracked, or copied into the
//! snapshot when untracked and not ignored — so a change to it is in the diff
//! the verdict judges, but not in the tools a base must share: that lane's
//! reds can be inherited across a change to it, matched by message hash.
//!
//! A FILE THAT CANNOT BE HASHED MAKES THE LINE UNSAYABLE: [`record`] answers
//! `None`, the receipt carries no `build-config` line, and such a receipt
//! serves no run as a base ([`crate::differential::tools_differ`]) — the same
//! fail-closed shape as a receipt without `build-env`. The digest is git's
//! blob id (`git hash-object --no-filters`), because it is the one id the
//! release cutter can compute for the committed file without reading the
//! disk: its MEASURE check requires the line to be exactly the tree's own
//! config (`aterm-release`'s `gates::measure_report`).
//!
//! NO CALLER `--config`. The gate's children take no `--config` from the
//! caller; the one it passes itself (the recording runner,
//! [`crate::testrun::recording_cmd`]) is the gate's own constant, the same on
//! every run. A `CARGO_*` variable that names or overrides config
//! (`CARGO_HOME`, `CARGO_BUILD_*`, `CARGO_PROFILE_*`) is the `build-env`
//! line's.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The names cargo reads in each `.cargo` directory — the extension-less one
/// first, as cargo prefers it when both exist (both are recorded).
pub const CONFIG_NAMES: [&str; 2] = ["config", "config.toml"];

/// How deep `include`s are followed before the line is refused: cargo's own
/// includes are shallow, and a chain this long is a cycle or a mistake.
const INCLUDE_DEPTH: usize = 8;

/// `$CARGO_HOME` as cargo resolves it for a build run in `cwd`: the variable
/// when it is set and non-empty (a relative one against `cwd`), else
/// `<home>/.cargo`; `None` when neither names a directory.
#[must_use]
pub fn cargo_home(
    cwd: &Path,
    cargo_home: Option<OsString>,
    home: Option<OsString>,
) -> Option<PathBuf> {
    match cargo_home.filter(|h| !h.is_empty()) {
        Some(h) => Some(cwd.join(h)),
        None => home
            .filter(|h| !h.is_empty())
            .map(|h| PathBuf::from(h).join(".cargo")),
    }
}

/// The config files cargo reads for a build run in `cwd` with `home` as its
/// `$CARGO_HOME`, in cargo's order, each with its label (see the module
/// docs). Existence is `symlink_metadata`: a dangling link is listed, so
/// hashing it fails and the line is refused rather than the file skipped.
#[must_use]
pub fn config_files(cwd: &Path, home: Option<&Path>) -> Vec<(String, PathBuf)> {
    let exists = |p: &Path| p.symlink_metadata().is_ok();
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let home_files: Vec<(String, PathBuf)> = home
        .map(|h| {
            CONFIG_NAMES
                .iter()
                .map(|n| (format!("cargo-home/{n}"), h.join(n)))
                .filter(|(_, p)| exists(p))
                .collect()
        })
        .unwrap_or_default();
    let home_canon: Vec<PathBuf> = home_files.iter().map(|(_, p)| canon(p)).collect();
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut out = Vec::new();
    for (depth, dir) in cwd.ancestors().enumerate() {
        for name in CONFIG_NAMES {
            let p = dir.join(".cargo").join(name);
            if !exists(&p) {
                continue;
            }
            let c = canon(&p);
            if seen.contains(&c) {
                continue;
            }
            let label = if home_canon.contains(&c) {
                format!("cargo-home/{name}")
            } else if depth == 0 {
                format!("repo/.cargo/{name}")
            } else {
                format!("ancestor/.cargo/{name}")
            };
            seen.push(c);
            out.push((label, p));
        }
    }
    for ((label, p), c) in home_files.into_iter().zip(home_canon) {
        if !seen.contains(&c) {
            seen.push(c);
            out.push((label, p));
        }
    }
    out
}

/// The paths a config file `include`s, as written, in the file's order — or
/// why they cannot be named, which makes the line unsayable ([`record`]).
///
/// READ AS TOML, NOT AS LINES (2026-09-28, review). Cargo takes includes in
/// two shapes, both measured on this toolchain: the top-level key
/// (`include = ["a.toml", { path = "b.toml", optional = true }]`, the key
/// bare or quoted) and the array of tables (`[[include]]` then
/// `path = "a.toml"`). A line reader that stopped at the first `[` never saw
/// the second, so an included file's flags changed with the line
/// byte-identical — a base built under other flags still served. This crate
/// takes no dependencies (Cargo.toml says why), so the file is parsed by the
/// small TOML reader below, and it FAILS CLOSED: a file it cannot parse, an
/// `include` in any shape but those two (a bare string, which cargo refuses
/// too; `[include]`; a dotted `include.path`; an entry with no `path`), or a
/// path spelled with escapes is an `Err`, never a silently shorter list.
///
/// # Errors
/// What in the file could not be read as TOML, or the `include` it cannot
/// name.
pub fn includes(text: &str) -> Result<Vec<String>, String> {
    let mut p = Toml {
        b: text.as_bytes(),
        i: 0,
    };
    let mut out = Vec::new();
    // `Some(path)` while inside a `[[include]]` table: the `path` it has named
    // so far. Every such table must name one.
    let mut table: Option<Option<String>> = None;
    let close = |table: &mut Option<Option<String>>, out: &mut Vec<String>| match table.take() {
        Some(Some(path)) => {
            out.push(path);
            Ok(())
        }
        Some(None) => Err("an `[[include]]` table names no `path`".to_string()),
        None => Ok(()),
    };
    // Whether keys are at the top level: before any header.
    let mut top = true;
    loop {
        p.blank();
        let Some(c) = p.peek() else { break };
        if c == b'[' {
            let array = p.b.get(p.i + 1) == Some(&b'[');
            p.i += if array { 2 } else { 1 };
            let key = p.key()?;
            p.ws();
            let closing: &[u8] = if array { b"]]" } else { b"]" };
            if !p.b[p.i..].starts_with(closing) {
                return Err(p.at("a table header is not closed"));
            }
            p.i += closing.len();
            p.eol()?;
            close(&mut table, &mut out)?;
            top = false;
            if key.first().map(String::as_str) == Some("include") {
                if !(array && key.len() == 1) {
                    return Err(format!(
                        "`include` as `{}{}{}` is not a shape cargo's includes are read in",
                        if array { "[[" } else { "[" },
                        key.join("."),
                        if array { "]]" } else { "]" },
                    ));
                }
                table = Some(None);
            }
            continue;
        }
        let key = p.key()?;
        p.ws();
        if p.peek() != Some(b'=') {
            return Err(p.at("a key is not followed by `=`"));
        }
        p.i += 1;
        let value = p.value()?;
        p.eol()?;
        if let Some(named) = &mut table {
            if key.first().map(String::as_str) == Some("path") {
                match (key.len(), value, named.is_none()) {
                    (1, Value::Str(path), true) => *named = Some(path),
                    _ => {
                        return Err(
                            "an `[[include]]` table's `path` is not one plain string".to_string()
                        );
                    }
                }
            }
        } else if top && key.first().map(String::as_str) == Some("include") {
            let Value::Array(items) = value else {
                return Err("`include` is not an array".to_string());
            };
            if key.len() != 1 {
                return Err(format!("`{}` is not a key cargo reads", key.join(".")));
            }
            for item in items {
                match item {
                    Value::Str(path) => out.push(path),
                    Value::Table(entries) => {
                        let mut path = None;
                        for (k, v) in entries {
                            if k.first().map(String::as_str) == Some("path") {
                                match (k.len(), v, path.is_none()) {
                                    (1, Value::Str(s), true) => path = Some(s),
                                    _ => {
                                        return Err("an `include` entry's `path` is not one \
                                                    plain string"
                                            .to_string());
                                    }
                                }
                            }
                        }
                        out.push(path.ok_or("an `include` entry names no `path`")?);
                    }
                    _ => return Err("an `include` entry is neither a string nor a table".into()),
                }
            }
        }
    }
    close(&mut table, &mut out)?;
    Ok(out)
}

/// One `key = value` statement of a TOML document, with the table header it
/// sits under (`[target.'cfg(unix)'.dependencies]` is `["target",
/// "cfg(unix)", "dependencies"]`, and `[[bin]]` is `["bin"]`; empty at the
/// top level).
pub(crate) struct Statement {
    pub(crate) table: Vec<String>,
    pub(crate) key: Vec<String>,
    pub(crate) value: Value,
}

/// Every statement of a TOML document, in order, read by the same small
/// reader [`includes`] uses — and with the same rule: what it cannot parse is
/// an `Err`, never a shorter list. The nearest-base gate reads main's
/// manifests with it ([`crate::nearest`], 2026-09-28).
///
/// # Errors
/// What in the document could not be read as TOML.
pub(crate) fn statements(text: &str) -> Result<Vec<Statement>, String> {
    let mut p = Toml {
        b: text.as_bytes(),
        i: 0,
    };
    let mut out = Vec::new();
    let mut table: Vec<String> = Vec::new();
    loop {
        p.blank();
        let Some(c) = p.peek() else { break };
        if c == b'[' {
            let array = p.b.get(p.i + 1) == Some(&b'[');
            p.i += if array { 2 } else { 1 };
            let key = p.key()?;
            p.ws();
            let closing: &[u8] = if array { b"]]" } else { b"]" };
            if !p.b[p.i..].starts_with(closing) {
                return Err(p.at("a table header is not closed"));
            }
            p.i += closing.len();
            p.eol()?;
            table = key;
            continue;
        }
        let key = p.key()?;
        p.ws();
        if p.peek() != Some(b'=') {
            return Err(p.at("a key is not followed by `=`"));
        }
        p.i += 1;
        let value = p.value()?;
        p.eol()?;
        out.push(Statement {
            table: table.clone(),
            key,
            value,
        });
    }
    Ok(out)
}

/// A TOML value, as far as [`includes`] needs one: a string it can read
/// verbatim (no escapes), an array, an inline table, or anything else.
pub(crate) enum Value {
    Str(String),
    /// A string spelled with escapes, as written (undecoded): never a value
    /// a reader may use as the string it means, only one it may look inside.
    Escaped(String),
    Array(Vec<Value>),
    Table(Vec<(Vec<String>, Value)>),
    Other,
}

/// A cursor over a TOML document: enough of TOML 1.0 to walk every
/// statement of a cargo config and read the values `include` takes. What it
/// does not model is an error, never a skip.
struct Toml<'a> {
    b: &'a [u8],
    i: usize,
}

impl Toml<'_> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn at(&self, what: &str) -> String {
        let line = 1 + self.b[..self.i.min(self.b.len())]
            .iter()
            .filter(|&&c| c == b'\n')
            .count();
        format!("line {line}: {what}")
    }

    /// Spaces and tabs.
    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.i += 1;
        }
    }

    /// A comment, when one starts here: to the end of its line.
    fn comment(&mut self) {
        if self.peek() == Some(b'#') {
            while !matches!(self.peek(), None | Some(b'\n')) {
                self.i += 1;
            }
        }
    }

    /// Whitespace, newlines and comments.
    fn blank(&mut self) {
        loop {
            self.ws();
            self.comment();
            match self.peek() {
                Some(b'\n' | b'\r') => self.i += 1,
                _ => return,
            }
        }
    }

    /// The end of a statement: whitespace, a comment, then a newline or EOF.
    fn eol(&mut self) -> Result<(), String> {
        self.ws();
        self.comment();
        if self.peek() == Some(b'\r') {
            self.i += 1;
        }
        match self.peek() {
            None => Ok(()),
            Some(b'\n') => {
                self.i += 1;
                Ok(())
            }
            Some(_) => Err(self.at("a statement runs on past its value")),
        }
    }

    /// A (dotted) key: bare or quoted segments.
    fn key(&mut self) -> Result<Vec<String>, String> {
        let mut out = Vec::new();
        loop {
            self.ws();
            match self.peek() {
                Some(b'"' | b'\'') => {
                    let (s, escaped) = self.string()?;
                    if escaped {
                        return Err(self.at("a key is spelled with escapes"));
                    }
                    out.push(s);
                }
                _ => {
                    let from = self.i;
                    while matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
                    {
                        self.i += 1;
                    }
                    if from == self.i {
                        return Err(self.at("expected a key"));
                    }
                    out.push(String::from_utf8_lossy(&self.b[from..self.i]).into_owned());
                }
            }
            self.ws();
            if self.peek() == Some(b'.') {
                self.i += 1;
            } else {
                return Ok(out);
            }
        }
    }

    /// A string of any of TOML's four kinds, and whether it held an escape
    /// (its text is then not decoded, and must not be used).
    fn string(&mut self) -> Result<(String, bool), String> {
        let q = self.b[self.i];
        let multi = self.b[self.i..].starts_with(&[q, q, q]);
        self.i += if multi { 3 } else { 1 };
        let from = self.i;
        let mut escaped = false;
        loop {
            match self.peek() {
                None => return Err(self.at("a string is not closed")),
                Some(b'\\') if q == b'"' => {
                    escaped = true;
                    self.i += 2;
                }
                Some(b'\n') if !multi => return Err(self.at("a string runs past its line")),
                Some(c) if c == q && (!multi || self.b[self.i..].starts_with(&[q, q, q])) => {
                    let to = self.i;
                    self.i += if multi { 3 } else { 1 };
                    // A multi-line string may end in one or two of its quotes.
                    let mut extra = 0;
                    while multi && extra < 2 && self.peek() == Some(q) {
                        self.i += 1;
                        extra += 1;
                    }
                    let text = String::from_utf8_lossy(&self.b[from..to + extra]).into_owned();
                    return Ok((text, escaped));
                }
                Some(_) => self.i += 1,
            }
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        self.ws();
        match self.peek() {
            Some(b'"' | b'\'') => {
                let (s, escaped) = self.string()?;
                Ok(if escaped {
                    Value::Escaped(s)
                } else {
                    Value::Str(s)
                })
            }
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                loop {
                    self.blank();
                    if self.peek() == Some(b']') {
                        self.i += 1;
                        return Ok(Value::Array(items));
                    }
                    items.push(self.value()?);
                    self.blank();
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        Some(b']') => {}
                        _ => return Err(self.at("an array is not closed")),
                    }
                }
            }
            Some(b'{') => {
                self.i += 1;
                let mut entries = Vec::new();
                loop {
                    self.blank();
                    if self.peek() == Some(b'}') {
                        self.i += 1;
                        return Ok(Value::Table(entries));
                    }
                    let key = self.key()?;
                    self.ws();
                    if self.peek() != Some(b'=') {
                        return Err(self.at("a key is not followed by `=`"));
                    }
                    self.i += 1;
                    let value = self.value()?;
                    entries.push((key, value));
                    self.blank();
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {}
                        _ => return Err(self.at("an inline table is not closed")),
                    }
                }
            }
            _ => {
                // A number, a boolean, a date: one token — two for a date and
                // a time a space separates.
                let token = |t: &mut Self| {
                    let from = t.i;
                    while !matches!(
                        t.peek(),
                        None | Some(b' ' | b'\t' | b',' | b']' | b'}' | b'#' | b'\n' | b'\r')
                    ) {
                        t.i += 1;
                    }
                    from
                };
                let from = token(self);
                if from == self.i {
                    return Err(self.at("expected a value"));
                }
                let date = self.i - from == 10 && self.b[from + 4] == b'-';
                if date
                    && self.peek() == Some(b' ')
                    && self.b.get(self.i + 1).is_some_and(u8::is_ascii_digit)
                {
                    self.i += 1;
                    token(self);
                }
                Ok(Value::Other)
            }
        }
    }
}

/// The files `files` read, with every file they include (recursively,
/// [`INCLUDE_DEPTH`] deep), in order: `Err` when a listed file cannot be read,
/// its includes cannot be named ([`includes`]), or they go deeper than that.
fn with_includes(files: Vec<(String, PathBuf)>) -> Result<Vec<(String, Option<PathBuf>)>, String> {
    let mut out = Vec::new();
    let mut stack: Vec<(String, PathBuf, usize)> =
        files.into_iter().rev().map(|(l, p)| (l, p, 0)).collect();
    while let Some((label, path, depth)) = stack.pop() {
        if depth > INCLUDE_DEPTH {
            return Err(format!("`{label}` includes past {INCLUDE_DEPTH} levels"));
        }
        let text = std::fs::read(&path)
            .map_err(|e| format!("`{label}` ({}) cannot be read: {e}", path.display()))?;
        let text = String::from_utf8(text)
            .map_err(|_| format!("`{label}` ({}) is not UTF-8", path.display()))?;
        let dir = path.parent().unwrap_or(Path::new("/")).to_path_buf();
        out.push((label.clone(), Some(path)));
        let mut nested = Vec::new();
        for inc in includes(&text).map_err(|e| format!("`{label}`: {e}"))? {
            let l = format!("{label}+include:{inc}");
            let p = dir.join(&inc);
            if p.symlink_metadata().is_ok() {
                nested.push((l, p, depth + 1));
            } else {
                out.push((l, None));
            }
        }
        stack.extend(nested.into_iter().rev());
    }
    Ok(out)
}

/// Git's blob id for each of `paths`, in order (`git hash-object
/// --no-filters`, run in `cwd`); `None` when git cannot hash one of them.
fn blob_ids(cwd: &Path, paths: &[&Path]) -> Option<Vec<String>> {
    if paths.is_empty() {
        return Some(Vec::new());
    }
    let out = Command::new("git")
        .args(["hash-object", "--no-filters", "--"])
        .args(paths)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let ids: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .collect();
    (ids.len() == paths.len()
        && ids
            .iter()
            .all(|id| id.len() >= 40 && id.bytes().all(|b| b.is_ascii_hexdigit())))
    .then_some(ids)
}

/// The `build-config` line for a build run in `cwd` with `home` as
/// `$CARGO_HOME` ([`cargo_home`]): `<label>=<blob id>` for each file cargo
/// reads ([`config_files`]) and each it includes, space-separated, or `none`.
/// `None` when any of them cannot be read or hashed — no line, which serves
/// no run as a base.
#[must_use]
pub fn record(cwd: &Path, home: Option<&Path>) -> Option<String> {
    let files = with_includes(config_files(cwd, home)).ok()?;
    let present: Vec<&Path> = files.iter().filter_map(|(_, p)| p.as_deref()).collect();
    let mut ids = blob_ids(cwd, &present)?.into_iter();
    let entries: Vec<String> = files
        .iter()
        .map(|(label, p)| match p {
            Some(_) => format!("{label}={}", ids.next().unwrap_or_default()),
            None => format!("{label}=missing"),
        })
        .collect();
    Some(if entries.is_empty() {
        "none".to_string()
    } else {
        entries.join(" ")
    })
}

/// [`record`] for a build run in `cwd` under this process's environment
/// (`CARGO_HOME`, `HOME`) — what the gate's children inherit. `None` when no
/// `$CARGO_HOME` can be named: cargo would read one, and what it holds cannot
/// be said.
#[must_use]
pub fn of_this_process(cwd: &Path) -> Option<String> {
    let home = cargo_home(
        cwd,
        std::env::var_os("CARGO_HOME"),
        std::env::var_os("HOME"),
    )?;
    record(cwd, Some(&home))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = crate::mktemp_dir(tag).expect("mktemp");
        d.canonicalize().expect("canonical")
    }

    fn put(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }

    fn blob(text: &str) -> String {
        let out = Command::new("git")
            .args(["hash-object", "--no-filters", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .and_then(|mut c| {
                use std::io::Write as _;
                c.stdin.take().expect("stdin").write_all(text.as_bytes())?;
                c.wait_with_output()
            })
            .expect("git hash-object");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[test]
    fn the_line_names_each_file_by_its_role_and_its_content() {
        let base = tmp("atv-bc-roles");
        let repo = base.join("work").join("repo");
        let home = base.join("home").join(".cargo");
        put(
            &repo.join(".cargo/config.toml"),
            "[build]\nrustflags = []\n",
        );
        put(&base.join(".cargo/config"), "[profile.dev]\ndebug = 0\n");
        put(&home.join("config.toml"), "[net]\noffline = true\n");
        std::fs::create_dir_all(repo.join("src")).expect("mkdir");

        let line = record(&repo, Some(&home)).expect("readable");
        assert_eq!(
            line,
            format!(
                "repo/.cargo/config.toml={} ancestor/.cargo/config={} cargo-home/config.toml={}",
                blob("[build]\nrustflags = []\n"),
                blob("[profile.dev]\ndebug = 0\n"),
                blob("[net]\noffline = true\n"),
            )
        );

        // Another checkout, elsewhere, with the same files: the same line.
        let other = tmp("atv-bc-roles-2");
        let deeper = other.join("a").join("b").join("c").join("repo");
        let other_home = other.join("h").join(".cargo");
        put(
            &deeper.join(".cargo/config.toml"),
            "[build]\nrustflags = []\n",
        );
        put(&other.join(".cargo/config"), "[profile.dev]\ndebug = 0\n");
        put(&other_home.join("config.toml"), "[net]\noffline = true\n");
        assert_eq!(
            record(&deeper, Some(&other_home)).as_deref(),
            Some(line.as_str())
        );

        // The negative control: one byte of one file outside the repo.
        put(&other_home.join("config.toml"), "[net]\noffline = false\n");
        assert_ne!(
            record(&deeper, Some(&other_home)).as_deref(),
            Some(line.as_str())
        );
        std::fs::remove_dir_all(&base).ok();
        std::fs::remove_dir_all(&other).ok();
    }

    #[test]
    fn a_cargo_home_on_the_walk_is_the_cargo_homes_and_listed_once() {
        let base = tmp("atv-bc-home");
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).expect("mkdir");
        // `~/.cargo` is both an ancestor's `.cargo` and `$CARGO_HOME`.
        put(&base.join(".cargo/config.toml"), "x = 1\n");
        let line = record(&repo, Some(&base.join(".cargo"))).expect("readable");
        assert_eq!(line, format!("cargo-home/config.toml={}", blob("x = 1\n")));
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn no_file_is_none_and_an_unreadable_one_is_no_line() {
        let base = tmp("atv-bc-none");
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).expect("mkdir");
        let home = base.join("nohome");
        // Only files this test made can be on the walk above a temp dir on a
        // clean machine; skip the "none" assertion where the machine has one.
        let ambient = config_files(&repo, Some(&home));
        if ambient.is_empty() {
            assert_eq!(record(&repo, Some(&home)).as_deref(), Some("none"));
        }
        // A dangling link where a config would be: cargo would fail on it,
        // and what it holds cannot be said.
        std::fs::create_dir_all(repo.join(".cargo")).expect("mkdir");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(base.join("gone"), repo.join(".cargo/config.toml"))
                .expect("symlink");
            assert_eq!(record(&repo, Some(&home)), None);
        }
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn an_included_file_is_recorded_under_its_includer() {
        let base = tmp("atv-bc-include");
        let repo = base.join("repo");
        let home = base.join("nohome");
        put(
            &repo.join(".cargo/config.toml"),
            "include = [\n  \"extra.toml\", # the flags\n  { path = \"gone.toml\", optional = true },\n]\n[build]\ninclude = \"not-top-level\"\n",
        );
        put(
            &repo.join(".cargo/extra.toml"),
            "[build]\nrustflags = [\"--cfg\", \"x\"]\n",
        );
        let line = record(&repo, Some(&home)).expect("readable");
        let extra = format!(
            "repo/.cargo/config.toml+include:extra.toml={}",
            blob("[build]\nrustflags = [\"--cfg\", \"x\"]\n")
        );
        assert!(line.contains(&extra), "{line}");
        assert!(
            line.contains("repo/.cargo/config.toml+include:gone.toml=missing"),
            "{line}"
        );
        assert!(!line.contains("not-top-level"), "{line}");
        // The included file's content is part of the line.
        put(&repo.join(".cargo/extra.toml"), "[build]\nrustflags = []\n");
        assert!(
            !record(&repo, Some(&home))
                .expect("readable")
                .contains(&extra)
        );
        std::fs::remove_dir_all(&base).ok();
    }

    /// The array-of-tables form cargo honours with no `-Z` (measured on
    /// targo 1.99.0-dev 321aaeda7, 2026-09-17: an `[[include]]`d file's
    /// `rustflags` reach rustc). Before the TOML reader, the line was
    /// byte-identical across a change to the included file.
    #[test]
    fn an_array_of_tables_include_is_recorded_and_its_content_moves_the_line() {
        let base = tmp("atv-bc-aot");
        let repo = base.join("repo");
        let home = base.join("nohome");
        put(
            &repo.join(".cargo/config.toml"),
            "[[include]]\npath = \"extra.toml\"\n\n[[ include ]]\noptional = true\npath = 'gone.toml'\n[build]\njobs = 1\n",
        );
        put(
            &repo.join(".cargo/extra.toml"),
            "[build]\nrustflags = [\"--cfg\", \"zzextra\"]\n",
        );
        let line = record(&repo, Some(&home)).expect("readable");
        let extra = format!(
            "repo/.cargo/config.toml+include:extra.toml={}",
            blob("[build]\nrustflags = [\"--cfg\", \"zzextra\"]\n")
        );
        assert!(line.contains(&extra), "{line}");
        assert!(
            line.contains("repo/.cargo/config.toml+include:gone.toml=missing"),
            "{line}"
        );
        put(&repo.join(".cargo/extra.toml"), "[build]\nrustflags = []\n");
        let after = record(&repo, Some(&home)).expect("readable");
        assert_ne!(
            after, line,
            "the included file changed and the line did not"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn every_spelling_of_the_key_is_read_and_nothing_else_is() {
        assert_eq!(
            includes("\"include\" = [\"a.toml\"]\n").as_deref(),
            Ok(&["a.toml".to_string()][..])
        );
        assert_eq!(
            includes("'include' = [{ path = \"b.toml\", optional = true }]\n").as_deref(),
            Ok(&["b.toml".to_string()][..])
        );
        // Not includes: a key under a table, one in an inline table, a
        // string or a comment that says the word, a multi-line string holding
        // a header.
        assert_eq!(
            includes(
                "# include = [\"c\"]\nx = \"include = ['d']\"\ny = { include = [\"e\"] }\n\
                 z = \"\"\"\n[[include]]\npath = \"f\"\n\"\"\"\n[build]\ninclude = [\"g\"]\n\
                 [[target.x.include]]\npath = \"h\"\n"
            )
            .as_deref(),
            Ok(&[][..])
        );
    }

    /// FAIL CLOSED: an `include` the reader cannot name, or a file it cannot
    /// read as TOML, is no list at all — and so no line, which serves no run
    /// as a base.
    #[test]
    fn an_include_the_reader_cannot_name_is_no_line() {
        for bad in [
            "include = \"x.toml\"\n",
            "include.path = \"x.toml\"\n",
            "[include]\npath = \"x.toml\"\n",
            "[[include.x]]\npath = \"x.toml\"\n",
            "[[include]]\noptional = true\n",
            "[[include]]\npath = \"x.toml\"\npath = \"y.toml\"\n",
            "include = [{ optional = true }]\n",
            "include = [1]\n",
            "include = [\"a\\u0041.toml\"]\n",
            "include = [\"x.toml\"\n",
            "[build\nrustflags = []\n",
            "rustflags = \"unclosed\n",
            "a = 1 b = 2\n",
        ] {
            assert!(includes(bad).is_err(), "read as includes: {bad:?}");
        }
        let base = tmp("atv-bc-badinc");
        let repo = base.join("repo");
        put(&repo.join(".cargo/config.toml"), "include = \"x.toml\"\n");
        assert_eq!(record(&repo, Some(&base.join("nohome"))), None);
        std::fs::remove_dir_all(&base).ok();
    }

    /// The reader must never refuse this repository's own cargo configs — the
    /// root's and the one a subdirectory carries — or no run here would ever
    /// have a base.
    #[test]
    fn this_repositorys_own_configs_read() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for rel in [".cargo/config.toml", "libc-oracle/.cargo/config.toml"] {
            let Ok(text) = std::fs::read_to_string(root.join(rel)) else {
                continue;
            };
            assert_eq!(
                includes(&text).map_err(|e| format!("{rel}: {e}")),
                Ok(vec![])
            );
        }
    }

    #[test]
    fn cargo_home_is_the_variable_else_home_dot_cargo() {
        let cwd = Path::new("/w/repo");
        assert_eq!(
            cargo_home(cwd, Some("/ch".into()), Some("/h".into())),
            Some(PathBuf::from("/ch"))
        );
        assert_eq!(
            cargo_home(cwd, Some("rel".into()), None),
            Some(PathBuf::from("/w/repo/rel"))
        );
        assert_eq!(
            cargo_home(cwd, Some("".into()), Some("/h".into())),
            Some(PathBuf::from("/h/.cargo"))
        );
        assert_eq!(cargo_home(cwd, None, None), None);
    }
}
