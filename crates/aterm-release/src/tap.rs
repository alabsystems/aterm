// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `ship tap deploy <tap checkout> [--dry-run]`.

use std::path::{Path, PathBuf};

use crate::ledger::{GitCli, GitRunner, git_ok};

/// What `ship tap` takes on its command line.
pub const USAGE: &str = "usage: targo --unverified ship tap deploy <tap checkout> [--dry-run]
  mirror distribution/homebrew-tap (README.md, MAINTENANCE.md, Casks/*.rb) into a checkout of
  alabsystems/homebrew-tap, keeping each live cask's version and sha256 lines (`pub tap --bump`
  owns them), and commit it there; the push is the owner's, with the owner's credential
  --dry-run   print what would change and commit nothing
exit: 0 deployed, already current or planned · 1 refused or failed · 2 usage";

/// The tap's source of truth, relative to the repository root.
pub const SOURCE: &str = "distribution/homebrew-tap";

/// The documents mirrored as they are.
pub const DOCS: [&str; 2] = ["README.md", "MAINTENANCE.md"];

/// Wording only an internal document carries (lowercase).
pub const INTERNAL_WORDING: [&str; 3] = [
    "private dev repo",
    "private aterm dev repo",
    "one-time setup (human)",
];

/// The commit a deploy makes in the tap.
pub const COMMIT_SUBJECT: &str = "Deploy tap structure from aterm source of truth";
const COMMIT_BODY: &str =
    "Updates docs and cask stanzas. Cask versions are owned by `pub tap --bump`, not this deploy.";

/// The command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub tap: PathBuf,
    pub dry_run: bool,
}

/// Parse `tap …` (`Ok(None)` is `--help`).
/// # Errors
/// No `deploy`, no tap checkout, or an argument it does not know.
pub fn parse_args<'a>(args: impl IntoIterator<Item = &'a str>) -> Result<Option<Options>, String> {
    let mut it = args.into_iter();
    match it.next() {
        Some("deploy") => {}
        Some("--help" | "-h") | None => return Ok(None),
        Some(other) => return Err(format!("unknown tap verb {other:?}")),
    }
    let (mut tap, mut dry_run) = (None, false);
    for arg in it {
        match arg {
            "--dry-run" => dry_run = true,
            "--help" | "-h" => return Ok(None),
            flag if flag.starts_with('-') => return Err(format!("unknown flag {flag:?}")),
            path if tap.is_none() => tap = Some(PathBuf::from(path)),
            extra => return Err(format!("one tap checkout, not also {extra:?}")),
        }
    }
    let tap = tap.ok_or("name the tap checkout")?;
    Ok(Some(Options { tap, dry_run }))
}

/// How one file of the tap changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// A cask the tap does not serve yet, written as the source has it.
    Seed,
    /// Rewritten from the source.
    Update,
    /// Already as the source says.
    Same,
}

/// One file of the tap as the source says it should be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    /// Relative to the tap checkout.
    pub path: String,
    pub bytes: Vec<u8>,
    pub change: Change,
}

/// `source`, a cask stanza, with `live`'s `version` and `sha256` lines kept.
/// # Errors
/// The live cask has no such line to keep.
pub fn keep_live(source: &str, live: &str) -> Result<String, String> {
    fn quoted<'a>(line: &'a str, key: &str, ok: fn(char) -> bool) -> Option<&'a str> {
        let v = line
            .strip_prefix("  ")?
            .strip_prefix(key)?
            .strip_prefix(" \"")?
            .strip_suffix('"')?;
        (!v.is_empty() && v.chars().all(ok)).then_some(v)
    }
    let version = live
        .lines()
        .find_map(|l| quoted(l, "version", |c| c.is_ascii_digit() || c == '.'));
    let sha = live.lines().find_map(|l| {
        quoted(l, "sha256", |c| {
            c.is_ascii_digit() || ('a'..='f').contains(&c)
        })
    });
    let (Some(version), Some(sha)) = (version, sha) else {
        return Err("the live cask has no quoted version and sha256 line to keep".into());
    };
    let mut out = String::with_capacity(source.len());
    for line in source.split_inclusive('\n') {
        let (body, end) = line.strip_suffix('\n').map_or((line, ""), |b| (b, "\n"));
        if quoted(body, "version", |c| c != '"').is_some() {
            out.push_str("  version \"");
            out.push_str(version);
            out.push('"');
            out.push_str(end);
        } else if quoted(body, "sha256", |c| c.is_ascii_hexdigit()).is_some() {
            out.push_str("  sha256 \"");
            out.push_str(sha);
            out.push('"');
            out.push_str(end);
        } else {
            out.push_str(line);
        }
    }
    Ok(out)
}

fn files_under(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let why = |error| format!("cannot inspect {}: {error}", dir.display());
    if !std::fs::symlink_metadata(dir).map_err(why)?.is_dir() {
        return Err(format!("{} is not a plain directory", dir.display()));
    }
    for entry in std::fs::read_dir(dir).map_err(why)? {
        let entry = entry.map_err(why)?;
        let ft = entry.file_type().map_err(why)?;
        let path = entry.path();
        if ft.is_dir() {
            files_under(&path, out)?;
        } else if ft.is_file() {
            out.push(path);
        } else {
            return Err(format!(
                "{} is not a regular file or directory",
                path.display()
            ));
        }
    }
    Ok(())
}

/// Maximum size of any tap document or cask file read during planning (16 MiB).
const MAX_TAP_SOURCE_BYTES: u64 = 16 * 1024 * 1024;

/// THE PLAN: the tap's files as `src` says them, each against what `tap` holds.
/// # Errors
/// A source with a `.github/` or internal-only wording, a document or cask that cannot be read, a
/// source with no cask, or a live cask with nothing to keep.
pub fn plan(src: &Path, tap: &Path) -> Result<Vec<Planned>, String> {
    if crate::bundle::exists_or_symlink(&src.join(".github")) {
        return Err(format!(
            "{} has a .github/ directory — the tap carries no workflow (cask bumps are `pub tap \
             --bump`)",
            src.display()
        ));
    }
    match std::fs::symlink_metadata(tap) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("{}: {e}", tap.display())),
        Ok(meta) if meta.file_type().is_dir() => {}
        Ok(_) => return Err(format!("{} is not a plain directory", tap.display())),
    }
    let tap_casks = tap.join("Casks");
    match std::fs::symlink_metadata(&tap_casks) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("{}: {e}", tap_casks.display())),
        Ok(meta) if meta.file_type().is_dir() => {}
        Ok(_) => return Err(format!("{} is not a plain directory", tap_casks.display())),
    }
    let mut all = Vec::new();
    files_under(src, &mut all)?;
    let read = |p: &Path| {
        crate::durable::read_regular_bytes(p, MAX_TAP_SOURCE_BYTES)
            .map_err(|e| format!("{}: {e}", p.display()))
    };
    let read_optional_tap = |rel: &str| -> Result<Option<Vec<u8>>, String> {
        let p = tap.join(rel);
        match std::fs::symlink_metadata(&p) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", p.display())),
            Ok(meta) if meta.file_type().is_file() => read(&p).map(Some),
            Ok(_) => Err(format!("{} is not a regular file", p.display())),
        }
    };
    let mut read_sources = std::collections::BTreeMap::new();
    for path in all {
        let raw = read(&path)?;
        if let Some(w) = INTERNAL_WORDING.iter().find(|w| {
            raw.windows(w.len())
                .any(|win| win.eq_ignore_ascii_case(w.as_bytes()))
        }) {
            return Err(format!(
                "{} carries internal-only wording ({w:?}) — scrub it before deploying",
                path.display()
            ));
        }
        read_sources.insert(path, raw);
    }
    let against_live = |path: String, bytes: Vec<u8>, live: Option<&[u8]>, seed: bool| -> Planned {
        let change = match live {
            Some(now) if now == bytes.as_slice() => Change::Same,
            Some(_) => Change::Update,
            None if seed => Change::Seed,
            None => Change::Update,
        };
        Planned {
            path,
            bytes,
            change,
        }
    };
    let mut out = Vec::new();
    for doc in DOCS {
        let doc_path = src.join(doc);
        let bytes = match read_sources.remove(&doc_path) {
            Some(b) => b,
            None => read(&doc_path)?,
        };
        let live = read_optional_tap(doc)?;
        out.push(against_live(doc.to_string(), bytes, live.as_deref(), false));
    }
    let casks_dir = src.join("Casks");
    let mut casks = Vec::new();
    for entry in
        std::fs::read_dir(&casks_dir).map_err(|e| format!("{}: {e}", casks_dir.display()))?
    {
        let entry = entry.map_err(|e| format!("{}: {e}", casks_dir.display()))?;
        if Path::new(&entry.file_name())
            .extension()
            .is_some_and(|x| x == "rb")
        {
            casks.push(entry.path());
        }
    }
    casks.sort_unstable();
    if casks.is_empty() {
        return Err(format!("{} holds no cask", src.join("Casks").display()));
    }
    for cask in casks {
        let name = cask
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let rel = format!("Casks/{name}");
        let raw_cask = match read_sources.remove(&cask) {
            Some(b) => b,
            None => read(&cask)?,
        };
        let source =
            String::from_utf8(raw_cask).map_err(|_| format!("{} is not UTF-8", cask.display()))?;
        let live_bytes = read_optional_tap(&rel)?;
        let bytes = match &live_bytes {
            Some(live_raw) => {
                let live = std::str::from_utf8(live_raw)
                    .map_err(|_| format!("{} is not UTF-8", tap.join(&rel).display()))?;
                keep_live(&source, live).map_err(|e| format!("{rel} in the tap: {e}"))?
            }
            None => source,
        };
        out.push(against_live(
            rel,
            bytes.into_bytes(),
            live_bytes.as_deref(),
            true,
        ));
    }
    Ok(out)
}

/// How a deploy ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Deployed {
    /// `--dry-run`: what would change (nothing was written).
    Planned(Vec<Planned>),
    /// The tap already says what the source says: nothing was committed.
    Current,
    /// Committed in the tap (the short sha); the push is the owner's.
    Committed(String),
}

/// Deploy `src` into the tap checkout `opts.tap`, saying each step on `out`.
/// # Errors
/// What [`plan`] refuses, a tap that is no git checkout or whose index already holds staged
/// changes, a write or a git step that failed.
pub fn deploy(
    src: &Path,
    opts: &Options,
    out: &mut dyn std::io::Write,
) -> Result<Deployed, String> {
    let tap = &opts.tap;
    if !tap.join(".git").exists() {
        return Err(format!("{} is not a git checkout", tap.display()));
    }
    let git = GitCli::new(tap);
    let staged = git
        .git(&["diff", "--cached", "--quiet"])
        .map_err(|e| e.to_string())?;
    if !staged.success() {
        return Err(format!(
            "{}'s index already holds staged changes — commit or unstage them first: the \
             deploy's commit holds only what it writes",
            tap.display()
        ));
    }
    let planned = plan(src, tap)?;
    let _ = writeln!(out, "== {} -> {}", src.display(), tap.display());
    for p in &planned {
        let word = match p.change {
            Change::Seed => "seed",
            Change::Update => "update",
            Change::Same => "same",
        };
        let _ = writeln!(out, "  {word:<6} {}", p.path);
    }
    if opts.dry_run {
        let n = planned.iter().filter(|p| p.change != Change::Same).count();
        let _ = writeln!(
            out,
            "== dry run: {n} file(s) would change and be committed; nothing was written"
        );
        return Ok(Deployed::Planned(planned));
    }
    for p in planned.iter().filter(|p| p.change != Change::Same) {
        let path = tap.join(&p.path);
        path.parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| crate::durable::write_atomic_durable(&path, &p.bytes, 0o644))
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    let paths: Vec<&str> = planned.iter().map(|p| p.path.as_str()).collect();
    let mut add = vec!["add", "--"];
    add.extend(&paths);
    git_ok(&git, &add).map_err(|e| e.to_string())?;
    let staged = git
        .git(&["diff", "--cached", "--quiet"])
        .map_err(|e| e.to_string())?;
    if staged.success() {
        let _ = writeln!(out, "== the tap is already current; nothing to commit");
        return Ok(Deployed::Current);
    }
    let mut commit = vec![
        "commit",
        "--quiet",
        "-m",
        COMMIT_SUBJECT,
        "-m",
        COMMIT_BODY,
        "--",
    ];
    commit.extend(&paths);
    git_ok(&git, &commit).map_err(|e| e.to_string())?;
    let sha = git_ok(&git, &["rev-parse", "--short", "HEAD"])
        .map(|o| o.stdout_utf8().trim().to_string())
        .map_err(|e| e.to_string())?;
    let _ = writeln!(out, "== committed {sha}: {COMMIT_SUBJECT}");
    let _ = writeln!(
        out,
        "== the push is the owner's, with the owner's credential: git -C {} push origin \
         HEAD:main",
        tap.display()
    );
    let _ = writeln!(
        out,
        "== then `pub tap` shows the version served; `pub tap --bump` moves it"
    );
    Ok(Deployed::Committed(sha))
}

/// `ship tap …`: parse, deploy, exit ([`USAGE`] names the codes).
#[must_use]
pub fn run_cli(args: &[String], repo: &Path) -> i32 {
    let opts = match parse_args(args.iter().map(String::as_str)) {
        Ok(Some(o)) => o,
        Ok(None) => {
            println!("{USAGE}");
            return 0;
        }
        Err(e) => {
            eprintln!("ship tap: {e}\n{USAGE}");
            return 2;
        }
    };
    let opts = Options {
        tap: std::env::current_dir().map_or(opts.tap.clone(), |cwd| cwd.join(&opts.tap)),
        ..opts
    };
    let mut out = std::io::stdout();
    match deploy(&repo.join(SOURCE), &opts, &mut out) {
        Ok(_) => 0,
        Err(e) => {
            eprintln!("ship tap: FAIL: {e}");
            1
        }
    }
}

#[cfg(test)]
#[path = "tap_tests.rs"]
mod tests;
