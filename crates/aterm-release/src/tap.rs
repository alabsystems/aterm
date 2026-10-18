// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `ship tap deploy <tap checkout> [--dry-run]`: the public Homebrew tap's structure,
//! from this repo's source of truth ([`SOURCE`]), mirrored into a checkout of
//! `alabsystems/homebrew-tap` and committed there.
//!
//! What it writes: `README.md`, `MAINTENANCE.md` and each `Casks/*.rb`, nothing else
//! (no deletion: a file the tap holds and the source does not is left alone). A cask
//! the tap already serves keeps its live `version` and `sha256` lines — those are
//! `pub tap --bump`'s, in the publication engine — and one it does not is seeded as
//! the source has it.
//!
//! What it refuses: a tap path that is no git checkout, a tap whose index already
//! holds staged changes (they would ride in the deploy's commit), a source that has
//! grown a `.github/` (the tap carries no workflow: MAINTENANCE.md), and a source
//! carrying internal-only wording ([`INTERNAL_WORDING`]), which must never reach the
//! public tree.
//!
//! What it leaves to the owner: the push. The commit is made in the tap checkout with
//! its own identity, holds only the files it planned, and the deploy names the push to
//! run with the owner's credential.

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

/// Wording only an internal document carries (lowercase): a source holding any of it
/// is refused, so an internal doc never leaks into the public tap.
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
///
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

/// `source`, a cask stanza, with `live`'s `version` and `sha256` lines kept: the first
/// `  version "<digits and dots>"` and `  sha256 "<lowercase hex>"` lines of the live
/// cask replace the source's own.
///
/// # Errors
/// The live cask has no such line to keep.
pub fn keep_live(source: &str, live: &str) -> Result<String, String> {
    let quoted = |line: &str, key: &str, ok: fn(char) -> bool| -> Option<String> {
        let v = line
            .strip_prefix(&format!("  {key} \""))?
            .strip_suffix('"')?;
        (!v.is_empty() && v.chars().all(ok)).then(|| v.to_string())
    };
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
            out.push_str(&format!("  version \"{version}\"{end}"));
        } else if quoted(body, "sha256", |c| c.is_ascii_hexdigit()).is_some() {
            out.push_str(&format!("  sha256 \"{sha}\"{end}"));
        } else {
            out.push_str(line);
        }
    }
    Ok(out)
}

/// Every file under `dir`, recursively.
fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            files_under(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// THE PLAN: the tap's files as `src` says them, each against what `tap` holds.
///
/// # Errors
/// A source with a `.github/` or internal-only wording, a document or cask that
/// cannot be read, a source with no cask, or a live cask with nothing to keep.
pub fn plan(src: &Path, tap: &Path) -> Result<Vec<Planned>, String> {
    if src.join(".github").exists() {
        return Err(format!(
            "{} has a .github/ directory — the tap carries no workflow (cask bumps are `pub tap \
             --bump`)",
            src.display()
        ));
    }
    let mut all = Vec::new();
    files_under(src, &mut all);
    for path in &all {
        let text = std::fs::read_to_string(path)
            .unwrap_or_default()
            .to_lowercase();
        if let Some(w) = INTERNAL_WORDING.iter().find(|w| text.contains(*w)) {
            return Err(format!(
                "{} carries internal-only wording ({w:?}) — scrub it before deploying",
                path.display()
            ));
        }
    }
    let read = |p: &Path| std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()));
    let against = |path: String, bytes: Vec<u8>, seed: bool| {
        let change = match std::fs::read(tap.join(&path)) {
            Ok(now) if now == bytes => Change::Same,
            Ok(_) => Change::Update,
            Err(_) if seed => Change::Seed,
            Err(_) => Change::Update,
        };
        Planned {
            path,
            bytes,
            change,
        }
    };
    let mut out = Vec::new();
    for doc in DOCS {
        out.push(against(doc.to_string(), read(&src.join(doc))?, false));
    }
    let mut casks: Vec<PathBuf> = std::fs::read_dir(src.join("Casks"))
        .map_err(|e| format!("{}: {e}", src.join("Casks").display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "rb"))
        .collect();
    casks.sort();
    if casks.is_empty() {
        return Err(format!("{} holds no cask", src.join("Casks").display()));
    }
    for cask in casks {
        let name = cask
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let rel = format!("Casks/{name}");
        let source = String::from_utf8(read(&cask)?)
            .map_err(|_| format!("{} is not UTF-8", cask.display()))?;
        let bytes = match std::fs::read_to_string(tap.join(&rel)) {
            Ok(live) => keep_live(&source, &live).map_err(|e| format!("{rel} in the tap: {e}"))?,
            Err(_) => source,
        };
        out.push(against(rel, bytes.into_bytes(), true));
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
///
/// # Errors
/// What [`plan`] refuses, a tap that is no git checkout or whose index already holds
/// staged changes, a write or a git step that failed.
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
            .and_then(|()| std::fs::write(&path, &p.bytes))
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    // Only the planned files are staged and committed: nothing else of the tap rides in.
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
mod tests {
    use super::*;

    const LIVE: &str =
        "cask \"aterm\" do\n  version \"0.63.0\"\n  sha256 \"fb16\"\n\n  url \"u\"\nend\n";

    fn scratch(label: &str) -> aterm_tempfile::TempDir {
        aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-tap-{label}-"))
            .tempdir()
            .unwrap()
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = GitCli::new(dir).git(args).unwrap();
        assert!(out.success(), "git {args:?}: {}", out.stderr_utf8());
        out.stdout_utf8().trim().to_string()
    }

    /// A source of truth: the two docs and two casks, `alab` one the tap lacks.
    fn source(root: &Path) -> PathBuf {
        let src = root.join("homebrew-tap");
        std::fs::create_dir_all(src.join("Casks")).unwrap();
        std::fs::write(src.join("README.md"), "# tap\n").unwrap();
        std::fs::write(src.join("MAINTENANCE.md"), "# maintenance\n").unwrap();
        std::fs::write(
            src.join("Casks/aterm.rb"),
            "cask \"aterm\" do\n  version \"0.1.0\"\n  sha256 \"0000\"\n\n  url \"new\"\nend\n",
        )
        .unwrap();
        std::fs::write(
            src.join("Casks/alab.rb"),
            "cask \"alab\" do\n  version \"0.1.0\"\n  sha256 \"0000\"\nend\n",
        )
        .unwrap();
        src
    }

    /// A tap checkout holding the live aterm cask, an old README and a file the source
    /// does not have.
    fn tap(root: &Path) -> PathBuf {
        let tap = root.join("tap");
        std::fs::create_dir_all(tap.join("Casks")).unwrap();
        git(&tap, &["init", "-q", "-b", "main"]);
        git(&tap, &["config", "user.name", "t"]);
        git(&tap, &["config", "user.email", "t@example.invalid"]);
        git(&tap, &["config", "commit.gpgsign", "false"]);
        std::fs::write(tap.join("README.md"), "# old\n").unwrap();
        std::fs::write(tap.join("Casks/aterm.rb"), LIVE).unwrap();
        std::fs::write(tap.join("LICENSE"), "mine\n").unwrap();
        git(&tap, &["add", "-A"]);
        git(&tap, &["commit", "-q", "-m", "live"]);
        tap
    }

    #[test]
    fn a_live_cask_keeps_its_version_and_sha256_and_nothing_else() {
        let src = "cask \"aterm\" do\n  version \"0.1.0\"\n  sha256 \"0000\"\n  url \"new\"\nend\n";
        assert_eq!(
            keep_live(src, LIVE).unwrap(),
            "cask \"aterm\" do\n  version \"0.63.0\"\n  sha256 \"fb16\"\n  url \"new\"\nend\n"
        );
        assert!(keep_live(src, "cask \"aterm\" do\nend\n").is_err());
        assert!(keep_live(src, "  version \"0.63.0\"\n  sha256 \"NOTHEX\"\n").is_err());
    }

    /// THE DEPLOY, against a scratch tap: a dry run prints the plan and writes nothing;
    /// a deploy mirrors the docs, keeps the live cask's version and sha256, seeds the
    /// new cask, leaves the tap's own files, commits once and pushes nothing; a second
    /// is already current.
    #[test]
    fn a_deploy_mirrors_the_source_keeps_the_live_lines_and_commits_once() {
        let d = scratch("deploy");
        let (src, tap) = (source(d.path()), tap(d.path()));
        let before = git(&tap, &["rev-parse", "HEAD"]);
        let mut out = Vec::new();
        let dry = Options {
            tap: tap.clone(),
            dry_run: true,
        };
        let Deployed::Planned(planned) = deploy(&src, &dry, &mut out).unwrap() else {
            panic!("a dry run plans");
        };
        let said = String::from_utf8_lossy(&out);
        assert!(said.contains("seed   Casks/alab.rb"), "{said}");
        assert!(said.contains("update Casks/aterm.rb"), "{said}");
        assert!(said.contains("update README.md"), "{said}");
        assert_eq!(planned.len(), 4);
        assert_eq!(git(&tap, &["status", "--porcelain"]), "", "nothing written");
        assert_eq!(git(&tap, &["rev-parse", "HEAD"]), before);

        let real = Options {
            dry_run: false,
            ..dry
        };
        let Deployed::Committed(sha) = deploy(&src, &real, &mut Vec::new()).unwrap() else {
            panic!("a deploy commits");
        };
        assert_eq!(git(&tap, &["rev-parse", "--short", "HEAD"]), sha);
        assert_eq!(git(&tap, &["rev-list", "--count", "HEAD"]), "2");
        assert_eq!(git(&tap, &["log", "-1", "--format=%s"]), COMMIT_SUBJECT);
        assert_eq!(git(&tap, &["status", "--porcelain"]), "");
        let cask = std::fs::read_to_string(tap.join("Casks/aterm.rb")).unwrap();
        assert!(
            cask.contains("  version \"0.63.0\"\n  sha256 \"fb16\"\n"),
            "{cask}"
        );
        assert!(cask.contains("url \"new\""), "{cask}");
        assert_eq!(
            std::fs::read(tap.join("Casks/alab.rb")).unwrap(),
            std::fs::read(src.join("Casks/alab.rb")).unwrap()
        );
        assert_eq!(
            std::fs::read_to_string(tap.join("README.md")).unwrap(),
            "# tap\n"
        );
        assert_eq!(
            std::fs::read_to_string(tap.join("LICENSE")).unwrap(),
            "mine\n"
        );
        assert_eq!(
            git(&tap, &["remote"]),
            "",
            "no remote was added or pushed to"
        );

        assert_eq!(
            deploy(&src, &real, &mut Vec::new()).unwrap(),
            Deployed::Current
        );
        assert_eq!(git(&tap, &["rev-list", "--count", "HEAD"]), "2");
    }

    #[test]
    fn a_workflow_internal_wording_or_no_checkout_is_refused() {
        let d = scratch("refuse");
        let (src, tap) = (source(d.path()), tap(d.path()));
        let real = Options {
            tap: tap.clone(),
            dry_run: false,
        };
        let not_a_checkout = Options {
            tap: d.path().join("nowhere"),
            dry_run: true,
        };
        assert!(deploy(&src, &not_a_checkout, &mut Vec::new()).is_err());
        std::fs::write(
            src.join("NOTES.md"),
            "Internal doc for the PRIVATE aterm DEV REPO.\n",
        )
        .unwrap();
        assert!(deploy(&src, &real, &mut Vec::new()).is_err());
        std::fs::remove_file(src.join("NOTES.md")).unwrap();
        std::fs::create_dir_all(src.join(".github/workflows")).unwrap();
        assert!(deploy(&src, &real, &mut Vec::new()).is_err());
        assert_eq!(
            git(&tap, &["rev-list", "--count", "HEAD"]),
            "1",
            "nothing committed"
        );
        assert_eq!(git(&tap, &["status", "--porcelain"]), "");
    }

    /// The deploy's commit holds only what it writes: an index that already holds a
    /// staged change is refused (a dry run too) before anything is written, and a
    /// change of the tap's own it did not stage stays out of the commit.
    #[test]
    fn a_staged_index_is_refused_and_nothing_else_rides_in_the_commit() {
        let d = scratch("staged");
        let (src, tap) = (source(d.path()), tap(d.path()));
        std::fs::write(tap.join("LICENSE"), "staged\n").unwrap();
        git(&tap, &["add", "LICENSE"]);
        for dry_run in [true, false] {
            let opts = Options {
                tap: tap.clone(),
                dry_run,
            };
            let refused = deploy(&src, &opts, &mut Vec::new()).unwrap_err();
            assert!(
                refused.contains("already holds staged changes"),
                "{refused}"
            );
        }
        assert_eq!(git(&tap, &["status", "--porcelain"]), "M  LICENSE");

        git(&tap, &["reset", "-q", "LICENSE"]);
        std::fs::write(tap.join("Casks/other.rb"), "cask \"other\" do\nend\n").unwrap();
        let opts = Options {
            tap: tap.clone(),
            dry_run: false,
        };
        let Deployed::Committed(_) = deploy(&src, &opts, &mut Vec::new()).unwrap() else {
            panic!("a deploy commits");
        };
        let committed = git(&tap, &["show", "--name-only", "--format=", "HEAD"]);
        assert_eq!(
            committed, "Casks/alab.rb\nCasks/aterm.rb\nMAINTENANCE.md\nREADME.md",
            "only what it planned"
        );
        assert_eq!(
            git(&tap, &["diff", "--name-only"]),
            "LICENSE",
            "the tap's own change is left as it was"
        );
        assert_eq!(
            git(&tap, &["ls-files", "--others", "--exclude-standard"]),
            "Casks/other.rb"
        );
    }

    #[test]
    fn the_command_line_is_deploy_a_checkout_and_maybe_dry_run() {
        let p = |a: &[&str]| parse_args(a.iter().copied());
        assert_eq!(
            p(&["deploy", "/t", "--dry-run"]).unwrap(),
            Some(Options {
                tap: PathBuf::from("/t"),
                dry_run: true
            })
        );
        assert_eq!(p(&["--help"]).unwrap(), None);
        assert!(p(&["deploy"]).is_err());
        assert!(p(&["deploy", "/a", "/b"]).is_err());
        assert!(p(&["push", "/a"]).is_err());
        assert!(p(&["deploy", "/a", "--force"]).is_err());
    }
}
