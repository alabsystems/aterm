// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The harness versions' increment rule: the release gate `harness versions` and
//! `targo --unverified ship harness-bump`.
//!
//! A harness's minor goes up by one once per public release window in which its sources
//! ([`aterm_phase::CliHarness::sources`]) changed. The gate refuses a candidate whose harness
//! sources moved since the previous public release while that harness's `HARNESS_VER` text did
//! not; `ship harness-bump` writes exactly the bumps the gate would ask for, and a line for each
//! under the CHANGELOG's `[Unreleased]`.

use std::fmt::Write as _;
use std::path::Path;

use aterm_phase::CliHarness;

use crate::ledger::{Error, GitRunner, Result, git_ok};

/// The marker a harness's version line starts with, in either shape.
const VER_LINE: &str = "pub const HARNESS_VER: crate::HarnessVersion = crate::HarnessVersion::new(";

/// The `(major, minor)` a harness version file declares: the one-line shape
/// (`HarnessVersion::new(0, 4, "0.4")`) or the three-constant shape it replaced
/// (`HARNESS_VERSION_MAJOR`, `HARNESS_VERSION_MINOR` and `HARNESS_VERSION`).
#[must_use]
pub fn declared(text: &str) -> Option<(u16, u16)> {
    let at = text.find(VER_LINE)?;
    let args = text[at + VER_LINE.len()..].split(");").next()?;
    let text_arg = args.split('"').nth(1);
    let numbers: Vec<u16> = args
        .split(',')
        .take(2)
        .filter_map(|a| a.trim().parse().ok())
        .collect();
    if let ([major, minor], Some(spelled)) = (numbers.as_slice(), text_arg) {
        return (aterm_phase::HarnessVersion::parse_major_minor(spelled) == Some((*major, *minor)))
            .then_some((*major, *minor));
    }
    let constant = |name: &str| -> Option<&str> {
        let key = format!("pub const {name}: ");
        let rest = &text[text.find(&key)? + key.len()..];
        Some(rest.split(';').next()?.split('=').nth(1)?.trim())
    };
    let spelled = constant("HARNESS_VERSION")?.trim_matches('"');
    let parsed = aterm_phase::HarnessVersion::parse_major_minor(spelled)?;
    let major: u16 = constant("HARNESS_VERSION_MAJOR")?.parse().ok()?;
    let minor: u16 = constant("HARNESS_VERSION_MINOR")?.parse().ok()?;
    (parsed == (major, minor)).then_some(parsed)
}

/// The one-line shape of a harness version at `(major, minor)`.
#[must_use]
pub fn version_line((major, minor): (u16, u16)) -> String {
    format!("{VER_LINE}{major}, {minor}, \"{major}.{minor}\");")
}

/// `text` with its declared version rewritten to `to` in the one-line shape (the three-constant
/// shape collapsed into it); `None` where `text` declares none.
#[must_use]
pub fn rewritten(text: &str, to: (u16, u16)) -> Option<String> {
    declared(text)?;
    let start = text.find(VER_LINE)?;
    let end = start + text[start..].find(");")? + 2;
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..start]);
    out.push_str(&version_line(to));
    out.push_str(&text[end..]);
    for name in [
        "HARNESS_VERSION_MAJOR",
        "HARNESS_VERSION_MINOR",
        "HARNESS_VERSION",
    ] {
        let key = format!("pub const {name}: ");
        if let Some(at) = out.find(&key) {
            let line_start = out[..at].rfind("\n\n").map_or(0, |p| p + 1);
            let line_end = at + out[at..].find(";\n").map_or(out.len() - at, |p| p + 2);
            out.replace_range(line_start..line_end, "");
        }
    }
    Some(out)
}

/// One harness whose sources moved in a release window under an unchanged version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owed {
    pub harness: CliHarness,
    /// The version the window opened (and still is) at.
    pub at: (u16, u16),
    /// The version the window's change must carry.
    pub to: (u16, u16),
    /// The changed source paths that moved it, sorted.
    pub moved: Vec<String>,
}

impl Owed {
    /// The refusal's detail line: the file and the exact new line.
    #[must_use]
    pub fn line(&self) -> String {
        format!(
            "alab {} harness {}.{} → {}.{}: {} changed ({}); write in {}: {}",
            self.harness.cli(),
            self.at.0,
            self.at.1,
            self.to.0,
            self.to.1,
            self.moved.len(),
            self.moved.first().map_or("", String::as_str),
            self.harness.version_file(),
            version_line(self.to)
        )
    }
}

/// Whether `path` is one of `harness`'s sources (a trailing `/` names a directory).
fn is_source(harness: CliHarness, path: &str) -> bool {
    harness
        .sources()
        .iter()
        .any(|src| match src.strip_suffix('/') {
            Some(_) => path.starts_with(src),
            None => path == *src,
        })
}

/// What each harness owes given the window's `changed` paths and each version file's text at the
/// window's start (`before`) and at the candidate (`after`).
pub fn owed(
    changed: &[String],
    mut before: impl FnMut(CliHarness) -> Result<Option<String>>,
    mut after: impl FnMut(CliHarness) -> Result<String>,
) -> Result<Vec<Owed>> {
    let mut out = Vec::new();
    for harness in CliHarness::ALL {
        let mut moved: Vec<String> = changed
            .iter()
            .filter(|p| is_source(harness, p))
            .cloned()
            .collect();
        if moved.is_empty() {
            continue;
        }
        moved.sort();
        let now_text = after(harness)?;
        let now = declared(&now_text).ok_or_else(|| {
            Error::new(format!(
                "harness versions: {} declares no HARNESS_VER line",
                harness.version_file()
            ))
        })?;
        let Some(then) = before(harness)?.as_deref().and_then(declared) else {
            continue;
        };
        if now <= then {
            out.push(Owed {
                harness,
                at: then,
                to: (then.0, then.1.saturating_add(1)),
                moved,
            });
        }
    }
    Ok(out)
}

/// The paths changed between commits `from` and `to`.
fn changed_between(git: &dyn GitRunner, from: &str, to: &str) -> Result<Vec<String>> {
    Ok(git_ok(git, &["diff", "--name-only", "-z", from, to])?
        .stdout_utf8()
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect())
}

/// A tracked file's text at `commit`, `None` where it is not there.
fn text_at(git: &dyn GitRunner, commit: &str, path: &str) -> Result<Option<String>> {
    let out = git.git(&["cat-file", "blob", &format!("{commit}:{path}")])?;
    Ok(out.success().then(|| out.stdout_utf8()))
}

/// What the harnesses owe between public release commit `released` and candidate `commit`.
///
/// # Errors
/// git cannot diff the two commits or read a version file.
pub fn owed_between(git: &dyn GitRunner, released: &str, commit: &str) -> Result<Vec<Owed>> {
    let changed = changed_between(git, released, commit)?;
    owed(
        &changed,
        |h| text_at(git, released, h.version_file()),
        |h| {
            text_at(git, commit, h.version_file())?.ok_or_else(|| {
                Error::new(format!(
                    "harness versions: {} is not in {commit}",
                    h.version_file()
                ))
            })
        },
    )
}

/// The gate's verdict: the passing line, or the refusal naming each file and its new line.
///
/// # Errors
/// A harness's sources moved under an unchanged version, or git could not answer.
pub fn gate(git: &dyn GitRunner, released: (&str, &str), commit: &str) -> Result<String> {
    let (release, released_commit) = released;
    let owed = owed_between(git, released_commit, commit)?;
    if owed.is_empty() {
        let shown: Vec<String> = CliHarness::ALL
            .iter()
            .filter_map(|h| {
                let text = text_at(git, commit, h.version_file()).ok()??;
                let (major, minor) = declared(&text)?;
                Some(format!("{} {major}.{minor}", h.cli()))
            })
            .collect();
        return Ok(format!(
            "harness versions since v{release}: {}",
            shown.join(", ")
        ));
    }
    let mut why = format!(
        "harness sources changed since v{release} under an unchanged harness version — bump \
         each (`targo --unverified ship harness-bump` writes it):"
    );
    for o in &owed {
        let _ = write!(why, "\n{}", o.line());
    }
    Err(Error::new(why))
}

/// `ship harness-bump` in checkout `root` against public release commit `released`: write each
/// owed bump into its version file and a line for it under the CHANGELOG's `[Unreleased]`, and
/// say what was written.
///
/// # Errors
/// The checkout's files cannot be read or written, or git cannot diff the window.
pub fn bump(root: &Path, git: &dyn GitRunner, released: (&str, &str)) -> Result<Vec<Owed>> {
    let (release, released_commit) = released;
    let head_changed = changed_between(git, released_commit, "HEAD")?;
    let mut changed = head_changed;
    for p in git_ok(git, &["diff", "--name-only", "-z", "HEAD"])?
        .stdout_utf8()
        .split('\0')
        .filter(|p| !p.is_empty())
    {
        if !changed.iter().any(|c| c == p) {
            changed.push(p.to_string());
        }
    }
    let read = |path: &str| {
        std::fs::read_to_string(root.join(path))
            .map_err(|e| Error::new(format!("cannot read {path}: {e}")))
    };
    let owed = owed(
        &changed,
        |h| text_at(git, released_commit, h.version_file()),
        |h| read(h.version_file()),
    )?;
    for o in &owed {
        let path = o.harness.version_file();
        let text = read(path)?;
        let new = rewritten(&text, o.to).ok_or_else(|| {
            Error::new(format!("harness-bump: {path} declares no HARNESS_VER line"))
        })?;
        std::fs::write(root.join(path), new)
            .map_err(|e| Error::new(format!("cannot write {path}: {e}")))?;
    }
    if !owed.is_empty() {
        let path = crate::changelog::CHANGELOG_FILE;
        let text = read(path)?;
        let names: Vec<String> = owed
            .iter()
            .map(|o| format!("`alab {} harness {}.{}`", o.harness.cli(), o.to.0, o.to.1))
            .collect();
        let entry = format!(
            "- **Harness versions since v{release}: {}.** Each harness's minor moves once per \
             release in which it changed.",
            names.join(", ")
        );
        let new = with_unreleased_entry(&text, &entry).ok_or_else(|| {
            Error::new(format!(
                "harness-bump: {path} has no `## [Unreleased]` section"
            ))
        })?;
        std::fs::write(root.join(path), new)
            .map_err(|e| Error::new(format!("cannot write {path}: {e}")))?;
    }
    Ok(owed)
}

/// `text` with `entry` added first under `[Unreleased]`'s `### Changed` (made where missing);
/// `None` where `text` has no `## [Unreleased]`.
#[must_use]
pub fn with_unreleased_entry(text: &str, entry: &str) -> Option<String> {
    let head = "## [Unreleased]";
    let mut offset = 0;
    let at = text.split_inclusive('\n').find_map(|line| {
        let start = offset;
        offset += line.len();
        (line.trim_end() == head).then_some(start)
    })?;
    let body_start = at + head.len();
    let body_end = text[body_start..]
        .find("\n## [")
        .map_or(text.len(), |p| body_start + p + 1);
    let body = &text[body_start..body_end];
    let mut out = String::with_capacity(text.len() + entry.len() + 32);
    out.push_str(&text[..body_start]);
    match body.find("\n### Changed\n") {
        Some(p) => {
            let after = body_start + p + "\n### Changed\n".len();
            out.push_str(&text[body_start..after]);
            out.push('\n');
            out.push_str(entry);
            out.push('\n');
            out.push_str(&text[after..]);
        }
        None => {
            out.push_str("\n\n### Changed\n\n");
            out.push_str(entry);
            out.push('\n');
            out.push_str(&text[body_start..]);
        }
    }
    Some(out)
}

/// The previous public release and its commit, for the window that ends at the candidate:
/// the newest release `RELEASES.ledger` (`ledger_text`) records before `version` that origin
/// has a tag for (`tags`: `ls-remote --tags origin`).
///
/// # Errors
/// The ledger cannot be parsed, or names earlier releases none of which shipped.
pub fn previous_release(
    ledger_text: &str,
    version: &str,
    tags: &str,
) -> Result<Option<(String, String)>> {
    let tags = crate::gates::tag_commits(tags);
    let shipped = |v: &str| tags.contains_key(&format!("v{v}"));
    let Some(release) = crate::gates::predecessor_release(ledger_text, version, &shipped)
        .map_err(|e| Error::new(format!("harness versions: {e}")))?
    else {
        return Ok(None);
    };
    let commit = tags
        .get(&format!("v{release}"))
        .cloned()
        .unwrap_or_default();
    Ok(Some((release, commit)))
}

/// The public release that starts a checkout's next harness release window.
/// A checkout still at a shipped version compares against that version; one already
/// bumped to an unshipped version compares against the last version that shipped.
fn bump_base_release(
    ledger_text: &str,
    version: &str,
    tags: &str,
) -> Result<Option<(String, String)>> {
    previous_release(
        ledger_text,
        &crate::publish::bump_minor_release(version)?,
        tags,
    )
}

/// `ship harness-bump` at checkout `root`.
///
/// # Errors
/// The checkout's ledger, tags or files cannot be read, or no public release precedes it.
pub fn run(root: &Path) -> Result<()> {
    let git = crate::ledger::GitCli::new(root);
    let ledger_text = std::fs::read_to_string(root.join(crate::ledger::LEDGER_FILE))
        .map_err(|e| Error::new(format!("cannot read {}: {e}", crate::ledger::LEDGER_FILE)))?;
    let manifest = std::fs::read_to_string(root.join("Cargo.toml"))
        .map_err(|e| Error::new(format!("cannot read Cargo.toml: {e}")))?;
    let version = crate::publish::release_version_from_workspace(
        &crate::publish::workspace_version(&manifest)?,
    )?;
    let tags = git_ok(&git, &["ls-remote", "--tags", "origin"])?.stdout_utf8();
    let Some((release, commit)) = bump_base_release(&ledger_text, &version, &tags)? else {
        println!("harness-bump: no public release precedes v{version}; nothing is owed");
        return Ok(());
    };
    if !git
        .git(&["cat-file", "-e", &format!("{commit}^{{commit}}")])?
        .success()
    {
        git_ok(
            &git,
            &["fetch", "-q", "origin", "tag", &format!("v{release}")],
        )?;
    }
    let owed = bump(root, &git, (&release, &commit))?;
    if owed.is_empty() {
        println!(
            "harness-bump: every harness's version already covers its changes since v{release}"
        );
    }
    for o in &owed {
        println!("harness-bump: {}", o.line());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::testing::{Git, git, init};

    const THREE: &str = "\
//! A reader.

/// Major component.
pub const HARNESS_VERSION_MAJOR: u16 = 0;

/// Minor component.
pub const HARNESS_VERSION_MINOR: u16 = 1;

/// Text.
pub const HARNESS_VERSION: &str = \"0.1\";

/// Structured.
pub const HARNESS_VER: crate::HarnessVersion = crate::HarnessVersion::new(
    HARNESS_VERSION_MAJOR,
    HARNESS_VERSION_MINOR,
    HARNESS_VERSION,
);

fn read() {}
";

    fn one(minor: u16) -> String {
        format!(
            "//! A reader.\n\n/// The version.\n{}\n\nfn read() {{}}\n",
            version_line((0, minor))
        )
    }

    #[test]
    fn both_shapes_declare_their_version_and_a_third_part_is_none() {
        assert_eq!(declared(THREE), Some((0, 1)));
        assert_eq!(declared(&one(4)), Some((0, 4)));
        assert_eq!(declared(&one(12)), Some((0, 12)));
        assert_eq!(declared("fn read() {}"), None);
        let lying = one(4).replace("\"0.4\"", "\"0.5\"");
        assert_eq!(declared(&lying), None, "the text must be the numbers");
        let third = one(4).replace("\"0.4\"", "\"0.4.0\"");
        assert_eq!(declared(&third), None, "never a third part");
        for harness in CliHarness::ALL {
            let real = std::fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../..")
                    .join(harness.version_file()),
            )
            .unwrap();
            assert_eq!(
                declared(&real),
                Some(harness.version().tuple()),
                "{harness:?}: the gate reads what the build compiles"
            );
        }
    }

    #[test]
    fn a_rewrite_collapses_the_old_shape_into_one_line() {
        let new = rewritten(THREE, (0, 2)).unwrap();
        assert_eq!(declared(&new), Some((0, 2)));
        assert!(!new.contains("HARNESS_VERSION_MAJOR"), "{new}");
        assert!(!new.contains("HARNESS_VERSION:"), "{new}");
        assert!(new.contains(&version_line((0, 2))), "{new}");
        assert!(new.ends_with("fn read() {}\n"), "{new}");
        let again = rewritten(&one(4), (0, 5)).unwrap();
        assert_eq!(again, one(5));
    }

    #[test]
    fn only_a_moved_harness_under_an_unchanged_version_owes() {
        let texts = |minor: u16| move |_: CliHarness| Ok::<_, Error>(one(minor));
        let before = |minor: u16| move |_: CliHarness| Ok::<_, Error>(Some(one(minor)));
        let none = owed(&["README.md".into()], before(4), texts(4)).unwrap();
        assert!(none.is_empty(), "no harness source moved");
        let lane = owed(
            &["crates/aterm-agent/src/harness/footer.rs".into()],
            before(4),
            texts(4),
        )
        .unwrap();
        assert_eq!(
            lane.iter().map(|o| o.harness).collect::<Vec<_>>(),
            CliHarness::ALL.to_vec(),
            "the shared lane moves every harness"
        );
        assert!(lane.iter().all(|o| (o.at, o.to) == ((0, 4), (0, 5))));
        let reader = owed(
            &["crates/aterm-phase/src/muse.rs".into()],
            before(4),
            texts(4),
        )
        .unwrap();
        assert_eq!(reader.len(), 1);
        assert_eq!(reader[0].harness, CliHarness::Muse);
        assert!(reader[0].line().contains("crates/aterm-phase/src/muse.rs"));
        assert!(reader[0].line().contains(&version_line((0, 5))));
        let bumped = owed(
            &["crates/aterm-agent/src/harness/footer.rs".into()],
            before(4),
            texts(5),
        )
        .unwrap();
        assert!(
            bumped.is_empty(),
            "a window's one bump covers every change in it"
        );
        let born = owed(
            &["crates/aterm-phase/src/kimi.rs".into()],
            |_| Ok(None),
            texts(1),
        )
        .unwrap();
        assert!(born.is_empty(), "a harness new in the window owes nothing");
    }

    #[test]
    fn an_entry_lands_under_unreleased_changed() {
        let log = "# Changelog\n\n## [Unreleased]\n\n### Changed\n\n- **Old.**\n\n## [0.1.0] - x\n";
        let new = with_unreleased_entry(log, "- **New.**").unwrap();
        assert!(
            new.contains("### Changed\n\n- **New.**\n\n- **Old.**"),
            "{new}"
        );
        let bare = "# Changelog\n\n## [Unreleased]\n\n## [0.1.0] - x\n";
        let new = with_unreleased_entry(bare, "- **New.**").unwrap();
        assert!(
            new.contains("## [Unreleased]\n\n### Changed\n\n- **New.**\n"),
            "{new}"
        );
        assert!(crate::changelog::gate_unreleased(&new).is_ok(), "{new}");
        assert_eq!(with_unreleased_entry("# Changelog\n", "- x"), None);
    }

    #[test]
    fn heading_examples_in_the_preamble_are_not_sections() {
        let preamble = "# Changelog\n\nWrite under `## [Unreleased]`, in `### Changed`.\n\n";
        assert_eq!(with_unreleased_entry(preamble, "- new"), None);
        let log = format!("{preamble}## [Unreleased]\n\n### Changed\n\n- old\n");
        assert_eq!(
            with_unreleased_entry(&log, "- new").unwrap(),
            format!("{preamble}## [Unreleased]\n\n### Changed\n\n- new\n\n- old\n")
        );
    }

    /// A scratch history: a release tag, then a change to the shared lane.
    fn history(dir: &Path, bump_too: bool) -> (String, String) {
        init(dir, &[("user.name", "t"), ("user.email", "t@example.com")]);
        let run = |args: &[&str]| {
            let out = git().arg("-C").arg(dir).args(args).output().unwrap();
            assert!(
                out.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        for h in CliHarness::ALL {
            let path = dir.join(h.version_file());
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                &path,
                if h == CliHarness::Muse {
                    THREE.to_string()
                } else {
                    one(1)
                },
            )
            .unwrap();
        }
        let lane = dir.join("crates/aterm-agent/src/harness/footer.rs");
        std::fs::create_dir_all(lane.parent().unwrap()).unwrap();
        std::fs::write(&lane, "fn a() {}\n").unwrap();
        std::fs::write(
            dir.join("CHANGELOG.md"),
            "# Changelog\n\n## [Unreleased]\n\n## [0.1.0] - x\n",
        )
        .unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-qm", "v0.1.0"]);
        let released = run(&["rev-parse", "HEAD"]);
        std::fs::write(&lane, "fn a() { b() }\n").unwrap();
        if bump_too {
            for h in CliHarness::ALL {
                std::fs::write(dir.join(h.version_file()), one(2)).unwrap();
            }
        }
        run(&["commit", "-qam", "lane"]);
        (released, run(&["rev-parse", "HEAD"]))
    }

    #[test]
    fn the_gate_refuses_a_moved_lane_and_passes_its_bump() {
        let tmp = aterm_tempfile::TempDir::new().unwrap();
        let (released, head) = history(tmp.path(), false);
        let git = Git::new(tmp.path());
        let refused = gate(&git, ("0.1.0", &released), &head)
            .unwrap_err()
            .to_string();
        assert!(refused.contains("since v0.1.0"), "{refused}");
        for h in CliHarness::ALL {
            assert!(refused.contains(h.version_file()), "{h:?}: {refused}");
        }
        assert!(refused.contains(&version_line((0, 2))), "{refused}");
        let tmp = aterm_tempfile::TempDir::new().unwrap();
        let (released, head) = history(tmp.path(), true);
        let git = Git::new(tmp.path());
        let passed = gate(&git, ("0.1.0", &released), &head).unwrap();
        assert!(
            passed.contains("claude 0.2") && passed.contains("muse 0.2"),
            "{passed}"
        );
    }

    #[test]
    fn harness_bump_writes_what_the_gate_asks_for() {
        let tmp = aterm_tempfile::TempDir::new().unwrap();
        let (released, _) = history(tmp.path(), false);
        let git = Git::new(tmp.path());
        let owed = bump(tmp.path(), &git, ("0.1.0", &released)).unwrap();
        assert_eq!(owed.len(), CliHarness::ALL.len());
        for h in CliHarness::ALL {
            let text = std::fs::read_to_string(tmp.path().join(h.version_file())).unwrap();
            assert_eq!(declared(&text), Some((0, 2)), "{h:?}");
            assert!(text.contains(&version_line((0, 2))), "{h:?}: one line");
        }
        let log = std::fs::read_to_string(tmp.path().join("CHANGELOG.md")).unwrap();
        assert!(log.contains("`alab claude harness 0.2`"), "{log}");
        assert!(log.contains("`alab codex harness 0.2`"), "{log}");
        let again = bump(tmp.path(), &git, ("0.1.0", &released)).unwrap();
        assert!(again.is_empty(), "a second run owes nothing");
        let run = |args: &[&str]| {
            let out = git_cmd(tmp.path(), args);
            assert!(out.status.success(), "{args:?}");
        };
        run(&["commit", "-qam", "bump"]);
        let head = crate::ledger::rev_parse(&git, "HEAD").unwrap();
        assert!(gate(&git, ("0.1.0", &released), &head).is_ok());
    }

    /// A LEDGER NONE OF WHOSE RELEASES SHIPPED IS REFUSED BY THIS GATE, BY NAME: its error says
    /// `harness versions:`, never another gate's name. CONTROL: a tag for the newest earlier
    /// release is the window's start.
    #[test]
    fn a_ledger_none_of_whose_releases_shipped_is_refused_by_this_gate() {
        let ledger = "# seed\n1 0.1.0\n2 0.2.0\n";
        let refused = previous_release(ledger, "0.3.0", "")
            .unwrap_err()
            .to_string();
        let unshipped = crate::gates::predecessor_release(ledger, "0.3.0", &|_| false)
            .expect_err("no earlier version shipped")
            .to_string();
        assert_eq!(refused, format!("harness versions: {unshipped}"));
        assert_eq!(
            previous_release(ledger, "0.3.0", "abc123\trefs/tags/v0.2.0\n").unwrap(),
            Some(("0.2.0".to_owned(), "abc123".to_owned())),
            "CONTROL"
        );
    }

    #[test]
    fn bump_uses_the_latest_shipped_release_before_and_after_the_version_bump() {
        let ledger = "# seed\n1 0.1.0\n2 0.2.0\n3 0.3.0\n";
        let tags = "abc123\trefs/tags/v0.1.0\ndef456\trefs/tags/v0.2.0\n";
        let expected = Some(("0.2.0".to_owned(), "def456".to_owned()));
        assert_eq!(bump_base_release(ledger, "0.2.0", tags).unwrap(), expected);
        assert_eq!(bump_base_release(ledger, "0.3.0", tags).unwrap(), expected);
        assert_eq!(
            previous_release(ledger, "0.2.0", tags).unwrap(),
            Some(("0.1.0".to_owned(), "abc123".to_owned())),
            "the release gate still compares a candidate against its predecessor"
        );
    }

    fn git_cmd(dir: &Path, args: &[&str]) -> std::process::Output {
        git().arg("-C").arg(dir).args(args).output().unwrap()
    }
}
