// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The harness versions' increment rule: the release gate `harness versions` and
//! `targo --unverified ship harness-bump`.
//!
//! A harness's minor goes up by one once per public release window in which its sources
//! ([`aterm_phase::CliHarness::sources`]) changed. The gate refuses a candidate whose harness
//! sources moved since the previous public release while that harness's `HARNESS_VER` text did
//! not; `ship harness-bump` writes exactly the bumps the gate would ask for, and a line for each
//! under the CHANGELOG's `[Unreleased]`. `ship release --bump` opens a release window, so its
//! version commit carries the same bumps ([`apply`]) and its candidate meets this gate.

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
    let uint = |s: &str| {
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse::<u16>().ok())
            .flatten()
    };
    let at = text.find(VER_LINE)?;
    let args = text[at + VER_LINE.len()..].split(");").next()?;
    let text_arg = args.split('"').nth(1);
    let numbers: Vec<u16> = args
        .split(',')
        .take(2)
        .filter_map(|a| uint(a.trim()))
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
    let major = uint(constant("HARNESS_VERSION_MAJOR")?)?;
    let minor = uint(constant("HARNESS_VERSION_MINOR")?)?;
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
    for args in [
        &["diff", "--name-only", "-z", "HEAD"][..],
        &["ls-files", "-z", "--others", "--exclude-standard"][..],
    ] {
        for p in git_ok(git, args)?
            .stdout_utf8()
            .split('\0')
            .filter(|p| !p.is_empty())
        {
            if !changed.iter().any(|c| c == p) {
                changed.push(p.to_string());
            }
        }
    }
    let read = |path: &str| {
        crate::durable::read_text(&root.join(path))
            .map_err(|e| Error::new(format!("cannot read {path}: {e}")))
    };
    let mut before_cache: [Option<Option<String>>; CliHarness::ALL.len()] = Default::default();
    let mut after_cache: [Option<String>; CliHarness::ALL.len()] = Default::default();
    let idx = |h: CliHarness| {
        CliHarness::ALL
            .iter()
            .position(|candidate| *candidate == h)
            .unwrap_or(0)
    };
    let owed = {
        let mut read_before = |h: CliHarness| -> Result<Option<String>> {
            let slot = &mut before_cache[idx(h)];
            if let Some(cached) = slot {
                return Ok(cached.clone());
            }
            let loaded = text_at(git, released_commit, h.version_file())?;
            *slot = Some(loaded.clone());
            Ok(loaded)
        };
        let mut read_after = |h: CliHarness| -> Result<String> {
            let slot = &mut after_cache[idx(h)];
            if let Some(cached) = slot {
                return Ok(cached.clone());
            }
            let loaded = read(h.version_file())?;
            *slot = Some(loaded.clone());
            Ok(loaded)
        };
        owed(&changed, &mut read_before, &mut read_after)?
    };
    for o in &owed {
        let path = o.harness.version_file();
        let slot = &mut after_cache[idx(o.harness)];
        let text = match slot {
            Some(cached) => cached.as_str(),
            None => slot.insert(read(path)?).as_str(),
        };
        let new = rewritten(text, o.to).ok_or_else(|| {
            Error::new(format!("harness-bump: {path} declares no HARNESS_VER line"))
        })?;
        crate::durable::write_fresh(&root.join(path), &new)
            .map_err(|e| Error::new(format!("cannot write {path}: {e}")))?;
        *slot = Some(new);
    }
    if !owed.is_empty() {
        let path = crate::changelog::CHANGELOG_FILE;
        let text = read(path)?;
        let mut names = Vec::new();
        for (i, h) in CliHarness::ALL.into_iter().enumerate() {
            let before = match &mut before_cache[i] {
                Some(cached) => cached.as_deref(),
                slot @ None => slot
                    .insert(text_at(git, released_commit, h.version_file())?)
                    .as_deref(),
            };
            let after = match &mut after_cache[i] {
                Some(cached) => cached.as_str(),
                slot @ None => slot.insert(read(h.version_file())?).as_str(),
            };
            if let Some(then) = before.and_then(declared)
                && let Some(now) = declared(after)
                && now > then
            {
                names.push(format!("`alab {} harness {}.{}`", h.cli(), now.0, now.1));
            }
        }
        let entry = format!(
            "{}{release}: {}.** Each harness's minor moves once per release in which it \
             changed.",
            crate::changelog::HARNESS_ENTRY_HEAD,
            names.join(", ")
        );
        let new = upsert_unreleased_harness_entry(&text, release, &entry).ok_or_else(|| {
            Error::new(format!(
                "harness-bump: {path} has no `## [Unreleased]` section"
            ))
        })?;
        crate::durable::write_fresh(&root.join(path), new)
            .map_err(|e| Error::new(format!("cannot write {path}: {e}")))?;
    }
    Ok(owed)
}

fn upsert_unreleased_harness_entry(text: &str, release: &str, entry: &str) -> Option<String> {
    let head = "## [Unreleased]";
    let mut offset = 0;
    // The heading as the changelog gate reads it, as [`with_unreleased_entry`] finds it.
    let body_start = text.split_inclusive('\n').find_map(|line| {
        let start = offset;
        offset += line.len();
        line.starts_with(head)
            .then(|| start + line.trim_end().len())
    })?;
    let body_end = text[body_start..]
        .find("\n## ")
        .map_or(text.len(), |p| body_start + p + 1);
    let prefix = format!("{}{release}: ", crate::changelog::HARNESS_ENTRY_HEAD);
    let mut body_offset = body_start;
    for line in text[body_start..body_end].split_inclusive('\n') {
        let start = body_offset;
        body_offset += line.len();
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.starts_with(&prefix) {
            let ending = &line[trimmed.len()..];
            let mut out = String::with_capacity(text.len() + entry.len());
            out.push_str(&text[..start]);
            out.push_str(entry);
            out.push_str(ending);
            out.push_str(&text[body_offset..]);
            return Some(out);
        }
    }
    with_unreleased_entry(text, entry)
}

/// `text` with `entry` added first under `[Unreleased]`'s `### Changed` (made where missing);
/// `None` where `text` has no `## [Unreleased]` (a line starting so, as the changelog gate
/// reads it).
#[must_use]
pub fn with_unreleased_entry(text: &str, entry: &str) -> Option<String> {
    let head = "## [Unreleased]";
    let mut offset = 0;
    let (line_end, body_start) = text.split_inclusive('\n').find_map(|line| {
        let start = offset;
        offset += line.len();
        line.starts_with(head)
            .then(|| (offset, start + line.trim_end().len()))
    })?;
    let body_end = text[body_start..]
        .find("\n## ")
        .map_or(text.len(), |p| body_start + p + 1);
    let body = &text[body_start..body_end];
    let mut out = String::with_capacity(text.len() + entry.len() + 32);
    out.push_str(&text[..body_start]);
    let mut body_offset = 0;
    let changed_line = body.split_inclusive('\n').find_map(|line| {
        let start = body_offset;
        body_offset += line.len();
        (line.trim_end() == "### Changed").then_some((start, body_offset, line.ends_with('\n')))
    });
    match changed_line {
        Some((_, line_end, has_nl)) => {
            let after = body_start + line_end;
            out.push_str(&text[body_start..after]);
            if !has_nl {
                out.push('\n');
            }
            out.push('\n');
            out.push_str(entry);
            out.push('\n');
            out.push_str(&text[after..]);
        }
        None => {
            out.push_str("\n\n### Changed\n\n");
            out.push_str(entry);
            out.push('\n');
            // The heading line's own end (its newline, a CR or blanks before it) is replaced.
            out.push_str(&text[line_end..]);
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

/// The files [`bump`] wrote for `owed`: each harness's version file, and the CHANGELOG when
/// any harness was owed.
#[must_use]
pub fn written(owed: &[Owed]) -> Vec<String> {
    let mut paths: Vec<String> = owed
        .iter()
        .map(|o| o.harness.version_file().to_string())
        .collect();
    if !owed.is_empty() {
        paths.push(crate::changelog::CHANGELOG_FILE.to_string());
    }
    paths
}

/// Write the bumps checkout `root`'s release window owes ([`bump`]) against the public release
/// that opened it (the checkout's version if it shipped, else the newest earlier one that did),
/// and name that release with what was written: `None` where the ledger names no earlier
/// release, so nothing is owed. `ship harness-bump` and `ship release --bump`'s version commit
/// both write through here.
///
/// # Errors
/// The ledger or manifest cannot be read, the ledger cannot be parsed or names earlier releases
/// none of which shipped, or the manifest's version is no release version whose minor can be
/// raised; origin's tags cannot be listed, or the release's tag fetched; git cannot diff the
/// window; or a written file cannot be read or written, a version file has no `HARNESS_VER`
/// line, or the CHANGELOG has no `## [Unreleased]`.
pub fn apply(root: &Path) -> Result<Option<(String, Vec<Owed>)>> {
    let git = crate::ledger::GitCli::new(root);
    let ledger_text = crate::durable::read_text(&root.join(crate::ledger::LEDGER_FILE))
        .map_err(|e| Error::new(format!("cannot read {}: {e}", crate::ledger::LEDGER_FILE)))?;
    let manifest = crate::durable::read_text(&root.join("Cargo.toml"))
        .map_err(|e| Error::new(format!("cannot read Cargo.toml: {e}")))?;
    let version = crate::publish::release_version_from_workspace(
        &crate::publish::workspace_version(&manifest)?,
    )?;
    let tags = git_ok(&git, &["ls-remote", "--tags", "origin"])?.stdout_utf8();
    let Some((release, commit)) = bump_base_release(&ledger_text, &version, &tags)? else {
        return Ok(None);
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
    Ok(Some((release, owed)))
}

/// `ship harness-bump` at checkout `root`.
///
/// # Errors
/// As [`apply`].
pub fn run(root: &Path) -> Result<()> {
    let Some((release, owed)) = apply(root)? else {
        println!("harness-bump: the ledger names no earlier release; nothing is owed");
        return Ok(());
    };
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
        assert_eq!(declared(&one(4).replace("0, 4,", "+0, 4,")), None);
        assert_eq!(
            declared(&THREE.replace("MAJOR: u16 = 0;", "MAJOR: u16 = +0;")),
            None
        );
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
        let none = owed(
            &[
                "README.md".into(),
                "crates/aterm-agent/src/harness/footer.rs".into(),
                "crates/aterm-agent/src/harness/codex_alab.rs".into(),
            ],
            before(4),
            texts(4),
        )
        .unwrap();
        assert!(
            none.is_empty(),
            "no CLI harness source moved; shared infrastructure does not lockstep-bump harnesses"
        );
        for (harness, sample) in [
            (
                CliHarness::ClaudeCode,
                "crates/aterm-agent/src/harness/cli_shared_claude.rs",
            ),
            (
                CliHarness::Agy,
                "crates/aterm-agent/src/harness/upgrade_agy.rs",
            ),
            (
                CliHarness::Grok,
                "crates/aterm-agent/src/harness/grok_signin.rs",
            ),
            (
                CliHarness::Kimi,
                "crates/aterm-agent/src/harness/cli_shared_kimi.rs",
            ),
            (
                CliHarness::Muse,
                "crates/aterm-agent/src/harness/upgrade_muse.rs",
            ),
        ] {
            let single = owed(&[sample.into()], before(4), texts(4)).unwrap();
            assert_eq!(
                single.iter().map(|o| o.harness).collect::<Vec<_>>(),
                vec![harness],
                "{sample} moves only {harness:?}"
            );
            assert_eq!((single[0].at, single[0].to), ((0, 4), (0, 5)));
        }
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
            &[
                "crates/aterm-phase/src/muse.rs".into(),
                "crates/aterm-agent/src/harness/upgrade_muse.rs".into(),
            ],
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
        assert_eq!(
            new,
            "# Changelog\n\n## [Unreleased]\n\n### Changed\n\n- **New.**\n\n## [0.1.0] - x\n"
        );
        assert!(crate::changelog::gate_unreleased(&new).is_ok(), "{new}");
        // One blank line before the section that follows, as before any other section.
        let added = "# Changelog\n\n## [Unreleased]\n\n### Added\n\n- **Old.**\n";
        assert_eq!(
            with_unreleased_entry(added, "- **New.**").unwrap(),
            "# Changelog\n\n## [Unreleased]\n\n### Changed\n\n- **New.**\n\n### Added\n\n- **Old.**\n"
        );
        assert_eq!(
            with_unreleased_entry("# Changelog\n\n## [Unreleased]", "- **New.**").unwrap(),
            "# Changelog\n\n## [Unreleased]\n\n### Changed\n\n- **New.**\n"
        );
        // The heading as the changelog gate reads it: a suffix stays, and a CR or blanks
        // ending its line leave no blank-looking line behind the entry.
        assert_eq!(
            with_unreleased_entry("## [Unreleased] - next\n\n## [0.1.0] - x\n", "- **New.**")
                .unwrap(),
            "## [Unreleased] - next\n\n### Changed\n\n- **New.**\n\n## [0.1.0] - x\n"
        );
        assert_eq!(
            with_unreleased_entry("## [Unreleased] \r\n\n## [0.1.0] - x\n", "- **New.**").unwrap(),
            "## [Unreleased]\n\n### Changed\n\n- **New.**\n\n## [0.1.0] - x\n"
        );
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

    /// A scratch history: a release tag, then changes to two harnesses (`claude` and `muse`) and a
    /// non-harness file (`footer.rs`), leaving `agy`, `grok` and `kimi` untouched.
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
        let footer = dir.join("crates/aterm-agent/src/harness/footer.rs");
        let claude_src = dir.join("crates/aterm-agent/src/harness/cli_shared_claude.rs");
        let muse_src = dir.join("crates/aterm-agent/src/harness/upgrade_muse.rs");
        std::fs::create_dir_all(footer.parent().unwrap()).unwrap();
        std::fs::write(&footer, "fn a() {}\n").unwrap();
        std::fs::write(&claude_src, "fn c() {}\n").unwrap();
        std::fs::write(&muse_src, "fn m() {}\n").unwrap();
        std::fs::write(
            dir.join("CHANGELOG.md"),
            "# Changelog\n\n## [Unreleased]\n\n## [0.1.0] - x\n",
        )
        .unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-qm", "v0.1.0"]);
        let released = run(&["rev-parse", "HEAD"]);
        std::fs::write(&footer, "fn a() { b() }\n").unwrap();
        std::fs::write(&claude_src, "fn c() { b() }\n").unwrap();
        std::fs::write(&muse_src, "fn m() { b() }\n").unwrap();
        if bump_too {
            for h in [CliHarness::ClaudeCode, CliHarness::Muse] {
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
        for h in [CliHarness::ClaudeCode, CliHarness::Muse] {
            assert!(refused.contains(h.version_file()), "{h:?}: {refused}");
        }
        for h in [CliHarness::Agy, CliHarness::Grok, CliHarness::Kimi] {
            assert!(
                !refused.contains(h.version_file()),
                "{h:?} did not move: {refused}"
            );
        }
        assert!(refused.contains(&version_line((0, 2))), "{refused}");
        let tmp = aterm_tempfile::TempDir::new().unwrap();
        let (released, head) = history(tmp.path(), true);
        let git = Git::new(tmp.path());
        let passed = gate(&git, ("0.1.0", &released), &head).unwrap();
        assert!(
            passed.contains("claude 0.2")
                && passed.contains("muse 0.2")
                && passed.contains("agy 0.1")
                && passed.contains("grok 0.1")
                && passed.contains("kimi 0.1"),
            "{passed}"
        );
    }

    #[test]
    fn harness_bump_writes_what_the_gate_asks_for() {
        let tmp = aterm_tempfile::TempDir::new().unwrap();
        let (released, _) = history(tmp.path(), false);
        let git = Git::new(tmp.path());
        let owed = bump(tmp.path(), &git, ("0.1.0", &released)).unwrap();
        assert_eq!(
            owed.iter().map(|o| o.harness).collect::<Vec<_>>(),
            vec![CliHarness::ClaudeCode, CliHarness::Muse]
        );
        for h in [CliHarness::ClaudeCode, CliHarness::Muse] {
            let text = std::fs::read_to_string(tmp.path().join(h.version_file())).unwrap();
            assert_eq!(declared(&text), Some((0, 2)), "{h:?}");
            assert!(text.contains(&version_line((0, 2))), "{h:?}: one line");
        }
        for h in [CliHarness::Agy, CliHarness::Grok, CliHarness::Kimi] {
            let text = std::fs::read_to_string(tmp.path().join(h.version_file())).unwrap();
            assert_eq!(declared(&text), Some((0, 1)), "{h:?} stays at 0.1");
        }
        let log = std::fs::read_to_string(tmp.path().join("CHANGELOG.md")).unwrap();
        assert!(log.contains("`alab claude harness 0.2`"), "{log}");
        assert!(log.contains("`alab muse harness 0.2`"), "{log}");
        assert!(!log.contains("`alab agy harness"), "{log}");
        assert!(
            !log.contains("codex harness"),
            "Codex versions its own binary: {log}"
        );
        assert_eq!(
            written(&owed),
            vec![
                CliHarness::ClaudeCode.version_file().to_string(),
                CliHarness::Muse.version_file().to_string(),
                crate::changelog::CHANGELOG_FILE.to_string(),
            ],
            "the files a bump commit carries"
        );
        let again = bump(tmp.path(), &git, ("0.1.0", &released)).unwrap();
        assert!(again.is_empty(), "a second run owes nothing");
        assert!(written(&again).is_empty(), "and writes nothing");
        let run = |args: &[&str]| {
            let out = git_cmd(tmp.path(), args);
            assert!(out.status.success(), "{args:?}");
        };
        run(&["commit", "-qam", "bump"]);
        let head = crate::ledger::rev_parse(&git, "HEAD").unwrap();
        assert!(gate(&git, ("0.1.0", &released), &head).is_ok());

        // NEGATIVE CONTROL: when another harness moves later in the same release window (including
        // an untracked file under a harness source directory), `ship harness-bump` updates the
        // existing `Harness versions since v0.1.0:` bullet in place with all bumped harnesses in
        // canonical order rather than inserting a second duplicate bullet.
        let dir_prefix = CliHarness::ALL.iter().find_map(|h| {
            h.sources()
                .iter()
                .find(|s| s.ends_with('/'))
                .map(|s| (*h, *s))
        });
        if let Some((dir_harness, prefix)) = dir_prefix
            && !matches!(dir_harness, CliHarness::ClaudeCode | CliHarness::Muse)
        {
            let untracked = tmp.path().join(format!("{prefix}new_untracked_helper.rs"));
            std::fs::create_dir_all(untracked.parent().unwrap()).unwrap();
            std::fs::write(&untracked, "fn helper() {}\n").unwrap();
            let third = bump(tmp.path(), &git, ("0.1.0", &released)).unwrap();
            assert_eq!(third.len(), 1);
            assert_eq!(third[0].harness, dir_harness);
        } else {
            let agy_src = tmp
                .path()
                .join("crates/aterm-agent/src/harness/upgrade_agy.rs");
            std::fs::write(&agy_src, "fn agy() {}\n").unwrap();
            run(&["add", "crates/aterm-agent/src/harness/upgrade_agy.rs"]);
            let third = bump(tmp.path(), &git, ("0.1.0", &released)).unwrap();
            assert_eq!(third.len(), 1);
            assert_eq!(third[0].harness, CliHarness::Agy);
        }
        let updated_log = std::fs::read_to_string(tmp.path().join("CHANGELOG.md")).unwrap();
        assert_eq!(
            updated_log
                .matches("Harness versions since v0.1.0:")
                .count(),
            1,
            "must update existing bullet in place: {updated_log}"
        );
        assert!(
            updated_log.contains("`alab claude harness 0.2`")
                && updated_log.contains("`alab muse harness 0.2`"),
            "{updated_log}"
        );
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
