// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The harness versions' increment rule: the release gate `harness versions` and
//! `targo --unverified ship harness-bump`.
//!
//! A harness's minor goes up by one once per public release window in which its sources
//! ([`aterm_phase::CliHarness::sources`]) changed. The gate refuses a candidate whose harness
//! sources moved since the previous public release while that harness's `HARNESS_VER` text did
//! not; `ship harness-bump` writes exactly the bumps the gate would ask for, into the version
//! files alone. `ship release --bump` opens a release window, so its version commit carries the
//! same bumps ([`apply`]) and its candidate meets this gate.
//!
//! The changelog's announcement of the raised versions is no file anyone writes: the release
//! derives it from the version files of the previous public release and of the published commit
//! ([`announcement`]) and renders it first under the release's `### Changed`, so it is a
//! function of objects that the reservation's recheck repeats.

use std::fmt::Write as _;
use std::path::Path;

use aterm_phase::CliHarness;

pub use aterm_phase::harness_version::{
    declared_in_source as declared, rewrite_source as rewritten, source_line as version_line,
};

use crate::ledger::{Error, GitRunner, Origin, Result, git_ok};

/// Check syntax without treating the candidate's copies of older prose as
/// shipment evidence. Version coverage is checked against Git objects below.
pub fn notes_gate(text: &str, section: &str) -> Result<()> {
    for entry in crate::changelog::note_entries(text, section) {
        note_versions(&entry)?;
        note_anchor(&entry)?;
    }
    Ok(())
}

/// The harness-versions announcement a release of `commit` ships: one line naming each
/// harness whose declared version rose since the public release `released` (its version and
/// commit), `None` where none rose or no release precedes. It leads the release's
/// `### Changed` ([`crate::changelog::Window::assembled`]).
///
/// # Errors
/// git cannot read a version file.
pub fn announcement(
    git: &dyn GitRunner,
    released: Option<(&str, &str)>,
    commit: &str,
) -> Result<Option<String>> {
    let Some((release, released_commit)) = released else {
        return Ok(None);
    };
    let raised = increased_versions(git, released_commit, commit)?;
    let names: Vec<String> = CliHarness::ALL
        .iter()
        .filter_map(|h| {
            let (_, (major, minor)) = raised.iter().find(|(cli, _)| *cli == h.cli())?;
            Some(format!("`alab {} harness {major}.{minor}`", h.cli()))
        })
        .collect();
    Ok((!names.is_empty()).then(|| {
        format!(
            "{}{release}: {}.** Each harness's minor moves once per release in which it changed.",
            crate::changelog::HARNESS_ENTRY_HEAD,
            names.join(", ")
        )
    }))
}

/// The release `version`'s section of `text` names its announcement since: the anchor of its
/// harness-versions line, `None` where the section has none.
///
/// # Errors
/// The line is malformed.
pub fn announcement_anchor(text: &str, version: &str) -> Result<Option<String>> {
    for entry in crate::changelog::note_entries(text, version) {
        if let Some(anchor) = note_anchor(&entry)? {
            return Ok(Some(anchor.to_string()));
        }
    }
    Ok(None)
}

/// Bind a recut's announcement to version declarations in the public predecessor selected by
/// the existing harness gate. Candidate copies of older notes cannot change what shipped. Reads
/// are local, bounded and strict UTF-8. A fresh window (`[Unreleased]`) carries no announcement
/// to judge: the release derives it ([`announcement`]) and drops any line written there by hand.
pub fn notes_gate_at(
    git: &dyn GitRunner,
    released: (&str, &str),
    commit: &str,
    version: &str,
) -> Result<()> {
    let read = |at: &str| -> Result<String> {
        let spec = format!("{at}:{}", crate::changelog::CHANGELOG_FILE);
        let out = crate::ledger::git_read_ok(
            git,
            &[
                "--no-lazy-fetch",
                "--no-replace-objects",
                "cat-file",
                "blob",
                &spec,
            ],
        )?;
        String::from_utf8(out.stdout)
            .map_err(|_| Error::new(format!("harness notes: non-UTF-8 {spec}")))
    };
    let (release, released_commit) = released;
    let current = read(commit)?;
    if !crate::changelog::has_section(&current, version) {
        return Ok(());
    }
    let section = version;
    let body = crate::changelog::note_entries(&current, section);
    notes_gate(&current, section)?;
    // Prose is not evidence of what shipped. Compare the announcement with the
    // actual version declarations, including when the preceding notes omitted one.
    let expected = increased_versions(git, released_commit, commit)?;
    let mut announced = std::collections::BTreeSet::new();
    for entry in &body {
        announced.extend(note_versions(entry)?);
    }
    if announced != expected {
        return Err(Error::new(format!(
            "harness notes: [{section}] announces {announced:?}, but versions since v{release} require {expected:?}\n\
             fix: run `targo --unverified ship harness-bump` and commit the repaired announcement"
        )));
    }
    // A historical announcement may truthfully span an intervening release
    // that did not change these versions (v0.191.0 since v0.189.0 is real main).
    // Every announced increase must hold over that wider interval too. An
    // intervening release's already-shipped increases need not be repeated:
    // the public predecessor above governs exactly what this cut announces.
    for entry in &body {
        if let Some(anchor) = note_anchor(entry)?
            && anchor != release
        {
            if crate::gates::version_triple(anchor) > crate::gates::version_triple(release) {
                return Err(Error::new(format!(
                    "harness notes: anchor v{anchor} is newer than public predecessor v{release}\nfix: run `targo --unverified ship harness-bump`"
                )));
            }
            let resolved = crate::ledger::git_read_ok(git, &[
                "--no-lazy-fetch", "--no-replace-objects", "rev-parse", "--verify",
                &format!("refs/tags/v{anchor}^{{commit}}"),
            ]).map_err(|error| Error::new(format!("harness notes: cannot resolve announcement anchor v{anchor}: {error}\nfix: run `targo --unverified ship harness-bump`")))?.stdout_utf8();
            let wider = increased_versions(git, resolved.trim(), commit)?;
            if !announced.is_subset(&wider) {
                return Err(Error::new(format!(
                    "harness notes: announcement since v{anchor} names versions that did not increase over that interval\nfix: run `targo --unverified ship harness-bump`"
                )));
            }
        }
    }
    Ok(())
}

fn increased_versions(
    git: &dyn GitRunner,
    before: &str,
    after: &str,
) -> Result<std::collections::BTreeSet<(&'static str, (u16, u16))>> {
    let mut versions = std::collections::BTreeSet::new();
    for harness in CliHarness::ALL {
        let before = text_at(git, before, harness.version_file())?;
        let after = text_at(git, after, harness.version_file())?;
        if let Some(then) = before.as_deref().and_then(declared)
            && let Some(now) = after.as_deref().and_then(declared)
            && now > then
        {
            versions.insert((harness.cli(), now));
        }
    }
    Ok(versions)
}

fn note_anchor(entry: &str) -> Result<Option<&str>> {
    let Some(tail) = entry.strip_prefix(crate::changelog::HARNESS_ENTRY_HEAD) else {
        return Ok(None);
    };
    let anchor = tail
        .split_once(": ")
        .map(|(anchor, _)| anchor)
        .filter(|anchor| crate::ledger::check_version_shape(anchor).is_ok());
    match anchor {
        Some(anchor) if !note_versions(entry)?.is_empty() => Ok(Some(anchor)),
        _ => Err(Error::new(
            "harness notes: malformed version announcement or anchor\nfix: run `targo --unverified ship harness-bump`",
        )),
    }
}

/// Read generated and historical prose spellings. Punctuation/backticks end the
/// major.minor token. A malformed named version is an error, never a bypass.
fn note_versions(line: &str) -> Result<Vec<(&'static str, (u16, u16))>> {
    let mut versions = Vec::new();
    for harness in CliHarness::ALL {
        let prefix = format!("alab {} harness ", harness.cli());
        for tail in line.split(&prefix).skip(1) {
            if !tail.starts_with(|c: char| c.is_ascii_digit()) {
                continue;
            }
            let token = tail
                .split(|c: char| !c.is_ascii_digit() && c != '.')
                .next()
                .unwrap_or("");
            let token = token.trim_end_matches('.');
            let version =
                aterm_phase::HarnessVersion::parse_major_minor(token).ok_or_else(|| {
                    Error::new(format!(
                        "harness notes: invalid {} version {token:?}",
                        harness.cli()
                    ))
                })?;
            versions.push((harness.cli(), version));
        }
    }
    Ok(versions)
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
        moved.sort_unstable();
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

/// What main's `tip` owes the harness window that release commit `release` opens, with each
/// owed version file's text at `tip` rewritten to its new version: what the release's landing
/// on main writes ([`crate::ledger::claim`]).
///
/// A release's tag becomes the landing check's baseline (`verify census`, "harness landing
/// debt") in the commit that brings the release commit into main's history, which is that
/// landing. Every harness whose sources main changed past the release commit then owes a
/// version above the release's; main last raised them for the window this release closes. The
/// landing is the first commit in which that debt exists and the one in which it is exactly
/// known, so it carries the raised versions and main is never red for it.
///
/// The diff reads renames as a removal and an addition, as the landing check's does. A version
/// file `tip` no longer holds, or one with no `HARNESS_VER` declaration, is left as it is: the
/// landing check names it to the next landing, and the release is not refused for it.
///
/// # Errors
/// git cannot diff the two commits or read a version file.
pub fn landing(git: &dyn GitRunner, release: &str, tip: &str) -> Result<Vec<(Owed, String)>> {
    let changed: Vec<String> = git_ok(
        git,
        &[
            "diff",
            "--name-only",
            "-z",
            "--no-renames",
            release,
            tip,
            "--",
        ],
    )?
    .stdout_utf8()
    .split('\0')
    .filter(|p| !p.is_empty())
    .map(str::to_string)
    .collect();
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
        moved.sort_unstable();
        let path = harness.version_file();
        let Some(then) = text_at(git, release, path)?.as_deref().and_then(declared) else {
            continue;
        };
        let Some(text) = text_at(git, tip, path)? else {
            continue;
        };
        if declared(&text).is_some_and(|now| now > then) {
            continue;
        }
        let to = (then.0, then.1.saturating_add(1));
        let Some(raised) = rewritten(&text, to) else {
            continue;
        };
        out.push((
            Owed {
                harness,
                at: then,
                to,
                moved,
            },
            raised,
        ));
    }
    Ok(out)
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
/// owed bump into its version file and return the owed bumps. The changelog is not written: the
/// release derives the announcement from the version files ([`announcement`]).
///
/// # Errors
/// The checkout's files cannot be read or written, or git cannot diff the window.
pub fn bump(root: &Path, git: &dyn GitRunner, released: (&str, &str)) -> Result<Vec<Owed>> {
    let (_, released_commit) = released;
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
    Ok(owed)
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
    recorded_release(ledger_text, version, tags, false)
}

fn recorded_release(
    ledger_text: &str,
    version: &str,
    tags: &str,
    include_current: bool,
) -> Result<Option<(String, String)>> {
    let mut tags = crate::gates::tag_commits(tags);
    let shipped = |v: &str| tags.contains_key(&format!("v{v}"));
    let selected = if include_current {
        crate::gates::release_through(ledger_text, version, &shipped)
    } else {
        crate::gates::predecessor_release(ledger_text, version, &shipped)
    };
    let Some(release) = selected.map_err(|e| Error::new(format!("harness versions: {e}")))? else {
        return Ok(None);
    };
    let commit = tags.remove(&format!("v{release}")).unwrap_or_default();
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
    recorded_release(ledger_text, version, tags, true)
}

/// The files [`bump`] writes: each owed harness's version file.
#[must_use]
pub fn written(owed: &[Owed]) -> Vec<String> {
    owed.iter()
        .map(|o| o.harness.version_file().to_string())
        .collect()
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
/// window; or a written file cannot be read or written, or a version file has no `HARNESS_VER`
/// line.
pub fn apply(root: &Path, origin: &dyn Origin) -> Result<Option<(String, Vec<Owed>)>> {
    apply_with(root, &crate::ledger::GitCli::new(root), origin)
}

fn apply_with(
    root: &Path,
    git: &dyn GitRunner,
    origin: &dyn Origin,
) -> Result<Option<(String, Vec<Owed>)>> {
    let ledger_text = crate::durable::read_text(&root.join(crate::ledger::LEDGER_FILE))
        .map_err(|e| Error::new(format!("cannot read {}: {e}", crate::ledger::LEDGER_FILE)))?;
    let manifest = crate::durable::read_text(&root.join("Cargo.toml"))
        .map_err(|e| Error::new(format!("cannot read Cargo.toml: {e}")))?;
    let version = crate::publish::release_version_from_workspace(
        &crate::publish::workspace_version(&manifest)?,
    )?;
    let names = crate::gates::release_candidates_through(&ledger_text, &version)?
        .into_iter()
        .map(|version| format!("v{version}"))
        .collect::<Vec<_>>();
    let Some((tag, commit)) = origin.first_tag_commit(&names).map_err(Error::new)? else {
        // Retain the ledger's distinction between no earlier window and an
        // earlier window none of whose versions has actually shipped.
        return bump_base_release(&ledger_text, &version, "").map(|_| None);
    };
    let release = tag
        .strip_prefix('v')
        .ok_or_else(|| Error::new("harness baseline is not a release tag"))?
        .to_owned();
    let owed = bump(root, git, (&release, &commit))?;
    Ok(Some((release, owed)))
}

/// `ship harness-bump` at checkout `root`.
///
/// # Errors
/// As [`apply`].
pub fn run(root: &Path) -> Result<()> {
    let origin = crate::publish::native_origin(root)?;
    let Some((release, owed)) = apply(root, &origin)? else {
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
#[path = "harness_versions_tests.rs"]
mod tests;
