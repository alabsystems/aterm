// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm init --develop` layer ([`layer`]) and [`crate::checkout`] alignment, and the same
//! items as `aterm doctor` reads them on a develop machine ([`doctor`], [`read`]).

use crate::checkout::{self, Origin, Remote};
use crate::engine::Place;
use crate::items::{Ctx, Item};
use crate::model::{ItemReport, Mode, Probe, User};
use crate::port::{Cmd, RunFailed, System};
use aterm_converge::{Class, Layer, Remedy, Requires, Scope, StoodAside};
use serde::Deserialize;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// The record's id.
pub const RECORD: &str = "develop.record";

const PER_CHECKOUT: [&str; 3] = [
    "develop.checkout",
    "develop.submodules",
    "develop.workspace-tools",
];

const FAMILIES: [&str; 3] = ["profile", "checkout", "tooling"];

const PROFILE: &str = ".config/aterm/init.toml";
const PROFILE_MAX: usize = 64 * 1024;
const MANIFEST_MAX: usize = 1024 * 1024;
const SUBMODULE: &str = "vendor/astream";
const LONG_BUDGET: Duration = Duration::from_secs(1800);
/// Doctor's wait for a checkout's tools `--check` before stopping it.
const DOCTOR_BUDGET: Duration = Duration::from_secs(120);

/// The command that finds the toolset's `targo`.
pub type Targo = Arc<dyn Fn() -> Result<PathBuf, String> + Send + Sync>;

/// Who runs a develop item, which decides what its probe may reach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reader {
    /// `aterm init`: probes ask origin (`git ls-remote`), apply fetches and fast-forwards, and
    /// cargo may download what the checkout's lock names.
    Init,
    /// `aterm doctor`: reads this Mac alone against origin as last fetched, runs cargo
    /// `--offline`, and applies nothing.
    Doctor,
}

const DOCTOR_CHANGES_NOTHING: &str = "aterm doctor reads the develop items and changes none of \
                                      them: `aterm init --develop` does";

/// One develop item and where it sits in the run.
pub type Placed = (Box<dyn Item>, Place);

/// Whether the `--only` word `word` can name a develop item.
#[must_use]
pub fn names(word: &str) -> bool {
    let stem = word.split_once('@').map_or(word, |(stem, _)| stem);
    FAMILIES.contains(&word)
        || aterm_converge::selects(word, RECORD, "profile", Some(Layer::Develop))
        || PER_CHECKOUT.iter().any(|id| {
            aterm_converge::selects(word, id, "checkout", Some(Layer::Develop))
                || (word.contains('@') && stem == *id)
        })
}

/// Where init places a develop item.
#[must_use]
pub const fn place(class: Class) -> Place {
    Place {
        layer: Layer::Develop,
        class,
        requires: Requires::Account,
        remedy: Remedy::Develop,
    }
}

/// `[develop]`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Table {
    /// The checkouts, each `~/<path>` or absolute.
    #[serde(default)]
    pub(crate) checkouts: Vec<String>,
}

/// The whole profile, as far as this module reads it.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(rename = "github", default)]
    _github: Option<aterm_toml::Value>,
    #[serde(default)]
    develop: Option<Table>,
}

const ADMISSION: &str = "cannot safely read ~/.config/aterm/init.toml (requires an owned \
                         regular file, no links or group/other writes, at most 64 KiB)";
const INVALID: &str = "invalid ~/.config/aterm/init.toml: it must be TOML holding at most a \
                       [github] table (account, owner, token_file, token_key) and a [develop] \
                       table (checkouts = [\"~/<path>\", …])";

struct Profile {
    path: PathBuf,
    text: Option<String>,
    mode: u32,
    table: Option<Table>,
}

fn profile_path(user: &User) -> PathBuf {
    Path::new(&user.home).join(PROFILE)
}

fn read_profile(sys: &dyn System, user: &User) -> Result<Profile, String> {
    let path = profile_path(user);
    let bytes = match sys.read_owned_file(&path, user.uid, PROFILE_MAX, false) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(Profile {
                path,
                text: None,
                mode: 0o600,
                table: None,
            });
        }
        Err(_) => return Err(ADMISSION.into()),
    };
    let mode = sys.stat(&path).map_or(0o600, |stat| stat.mode & 0o777);
    let text = String::from_utf8(bytes)
        .map_err(|_| "~/.config/aterm/init.toml must be UTF-8".to_owned())?;
    let file: File = aterm_toml::from_str(&text).map_err(|_| INVALID.to_owned())?;
    if let Some(table) = &file.develop {
        for spelled in &table.checkouts {
            expand(user, spelled)?;
        }
    }
    Ok(Profile {
        path,
        text: Some(text),
        mode,
        table: file.develop,
    })
}

/// Expanded checkouts recorded in the develop profile.
///
/// # Errors
/// A recorded spelling is neither `~/<path>` nor absolute, or has a `..` component or a NUL.
pub(crate) fn recorded(user: &User, table: Option<&Table>) -> Result<Vec<PathBuf>, String> {
    table.map_or_else(
        || Ok(Vec::new()),
        |table| {
            table
                .checkouts
                .iter()
                .map(|spelled| expand(user, spelled))
                .collect()
        },
    )
}

/// `~/<path>` under the home, else the absolute path.
fn spell(user: &User, path: &Path) -> Result<String, String> {
    let text = path
        .to_str()
        .ok_or("a checkout path must be UTF-8 to be recorded")?;
    Ok(match path.strip_prefix(&user.home) {
        Ok(rest) if !rest.as_os_str().is_empty() => format!("~/{}", rest.display()),
        _ => text.to_owned(),
    })
}

fn expand(user: &User, spelled: &str) -> Result<PathBuf, String> {
    let path = spelled.strip_prefix("~/").map_or_else(
        || PathBuf::from(spelled),
        |rest| Path::new(&user.home).join(rest),
    );
    if spelled.contains('\0')
        || !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(format!(
            "init.toml's [develop] checkout {spelled:?} must be ~/<path> or absolute, without \
             dot components"
        ));
    }
    Ok(path)
}

/// A TOML basic string.
fn basic(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn array(checkouts: &[String]) -> String {
    let items: Vec<String> = checkouts.iter().map(|c| basic(c)).collect();
    format!("[{}]", items.join(", "))
}

/// `text` with its `[develop]` table holding `want`, or with no such table.
fn edited(text: &str, want: Option<&[String]>) -> Result<String, String> {
    use aterm_toml::edit::{DocumentMut, Item as Entry};
    const NOT_A_HEADER: &str = "init.toml spells develop other than as a [develop] table; \
                                edit it by hand";
    let doc: DocumentMut = text.parse().map_err(|_| INVALID.to_owned())?;
    let out = match (doc.as_table().get("develop"), want) {
        (None, None) => text.to_owned(),
        (None, Some(want)) => {
            let mut out = text.to_owned();
            let unterminated = !out.is_empty() && !out.ends_with('\n');
            if unterminated {
                out.push('\n');
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str("[develop]\ncheckouts = ");
            out.push_str(&array(want));
            if !unterminated {
                out.push('\n');
            }
            out
        }
        (Some(Entry::Table(table)), want) if !table.is_implicit() => {
            let header = table.span().ok_or(NOT_A_HEADER)?;
            let line_start = text[..header.start].rfind('\n').map_or(0, |at| at + 1);
            let line_end = |from: usize| {
                text[from..]
                    .find('\n')
                    .map_or(text.len(), |at| from + at + 1)
            };
            match want {
                None => {
                    let last = table
                        .iter()
                        .filter_map(|(_, entry)| entry.span())
                        .map(|span| span.end)
                        .max()
                        .unwrap_or(header.end);
                    let end = line_end(last);
                    let mut start = line_start;
                    if text[..start].ends_with("\n\n") {
                        start -= 1;
                        if end == text.len() && !text.ends_with('\n') {
                            start -= 1;
                        }
                    }
                    format!("{}{}", &text[..start], &text[end..])
                }
                Some(want) => match table.get("checkouts").and_then(Entry::span) {
                    Some(span) => format!(
                        "{}{}{}",
                        &text[..span.start],
                        array(want),
                        &text[span.end..]
                    ),
                    None => {
                        let at = line_end(header.end);
                        let mut out = text[..at].to_owned();
                        if !out.ends_with('\n') {
                            out.push('\n');
                        }
                        out.push_str("checkouts = ");
                        out.push_str(&array(want));
                        out.push('\n');
                        out.push_str(&text[at..]);
                        out
                    }
                },
            }
        }
        (Some(_), _) => return Err(NOT_A_HEADER.into()),
    };
    let before: File = aterm_toml::from_str(text).map_err(|_| INVALID.to_owned())?;
    let after: File = aterm_toml::from_str(&out)
        .map_err(|_| "editing init.toml's [develop] table would break the file; left as it is")?;
    let wanted = want.map(|want| Table {
        checkouts: want.to_vec(),
    });
    if after.develop.map(|table| table.checkouts) != wanted.map(|table| table.checkouts)
        || after._github != before._github
    {
        return Err(
            "editing init.toml's [develop] table would change more than it; left as it is".into(),
        );
    }
    Ok(out)
}

fn describe(table: Option<&[String]>) -> String {
    match table {
        None => "no [develop] table: the user profile".to_owned(),
        Some([]) => "a [develop] table with no checkout".to_owned(),
        Some(checkouts) => format!("the develop profile, for {}", checkouts.join(", ")),
    }
}

/// The develop record item.
struct Record {
    user: User,
    want: Option<Vec<String>>,
    reader: Reader,
}

impl Item for Record {
    fn id(&self) -> String {
        RECORD.to_owned()
    }
    fn family(&self) -> &'static str {
        "profile"
    }
    fn title(&self) -> String {
        "This Mac's profile ([develop] in ~/.config/aterm/init.toml)".to_owned()
    }
    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        let profile = match read_profile(sys, &self.user) {
            Ok(profile) => profile,
            Err(why) => return Probe::Unusable { why },
        };
        let now = profile.table.map(|table| table.checkouts);
        // Doctor wants what is recorded, read now: it has nothing of its own to lay.
        if self.reader == Reader::Doctor {
            let mut said = describe(now.as_deref());
            if now.is_none() {
                said.push_str(" (`aterm init --develop` records one)");
            }
            return Probe::Ok { now: said };
        }
        if now == self.want {
            let mut now = describe(now.as_deref());
            if self.want.is_none() {
                now.push_str(" (`aterm init --develop` records one)");
            }
            return Probe::Ok { now };
        }
        Probe::Change {
            from: describe(now.as_deref()),
            to: describe(self.want.as_deref()),
        }
    }
    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        if self.reader == Reader::Doctor {
            return Err(DOCTOR_CHANGES_NOTHING.into());
        }
        let profile = read_profile(sys, &self.user)?;
        let text = profile.text.clone().unwrap_or_default();
        let out = edited(&text, self.want.as_deref())?;
        if profile.text.as_deref() == Some(out.as_str())
            || (profile.text.is_none() && out.is_empty())
        {
            return Ok(());
        }
        if profile.text.is_none()
            && let Some(dir) = profile.path.parent()
        {
            sys.create_dir_all(dir, 0o700)
                .map_err(|e| format!("cannot create ~/.config/aterm: {e}"))?;
        }
        sys.write_user_file(&self.user, &profile.path, out.as_bytes(), profile.mode)
            .map_err(|e| format!("cannot write ~/.config/aterm/init.toml: {e}"))
    }
    fn revert(&self) -> Option<String> {
        Some(if self.want.is_some() {
            "aterm init --no-develop".to_owned()
        } else {
            "aterm init --develop".to_owned()
        })
    }
}

/// An aterm workspace.
pub(crate) struct Workspace {
    origin: Origin,
}

/// The aterm workspace whose root is `dir`.
///
/// # Errors
/// The manifest cannot be read safely (not the user's, a link, too large) or parsed.
pub(crate) fn workspace(
    sys: &dyn System,
    user: &User,
    dir: &Path,
) -> Result<Option<Workspace>, String> {
    let manifest = dir.join("Cargo.toml");
    let bytes = match sys.read_owned_file(&manifest, user.uid, MANIFEST_MAX, false) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(format!(
                "cannot safely read {} (requires your own regular file)",
                manifest.display()
            ));
        }
    };
    let value: aterm_toml::Value = std::str::from_utf8(&bytes)
        .ok()
        .and_then(|text| aterm_toml::from_str(text).ok())
        .ok_or_else(|| format!("{} is not a TOML manifest", manifest.display()))?;
    Ok(workspace_of(&value))
}

fn workspace_of(manifest: &aterm_toml::Value) -> Option<Workspace> {
    let workspace = manifest.get("workspace")?;
    let aterm = workspace.get("metadata")?.get("aterm")?;
    let mut remotes = Vec::new();
    if let Some(repository) = workspace
        .get("package")
        .and_then(|package| package.get("repository"))
        .and_then(aterm_toml::Value::as_str)
    {
        remotes.push(Remote::of(repository));
    }
    if let Some(channel) = aterm
        .get("update_channel")
        .and_then(aterm_toml::Value::as_str)
        && let Some((owner, name)) = channel.split_once('/')
    {
        let public = Remote::Github {
            owner: owner.to_owned(),
            name: name.to_owned(),
        };
        if !remotes.contains(&public) {
            remotes.push(public);
        }
    }
    Some(Workspace {
        origin: Origin(remotes),
    })
}

/// The main worktree the aterm workspace at `root` belongs to, when it is a linked worktree.
fn linked_worktree_of(sys: &dyn System, user: &User, root: &Path) -> Option<PathBuf> {
    let ran = sys
        .run_complete_stdout(&checkout::git(user, Some(root)).args([
            "rev-parse",
            "--path-format=relative",
            "--git-dir",
            "--git-common-dir",
        ]))
        .ok()
        .filter(crate::port::Ran::success)?;
    let text = String::from_utf8_lossy(&ran.stdout).into_owned();
    let mut lines = text.lines();
    let (dir, common) = (lexical(root, lines.next()?), lexical(root, lines.next()?));
    if dir == common {
        return None;
    }
    Some(match common.file_name() {
        Some(name) if name == ".git" => common.parent().unwrap_or(&common).to_path_buf(),
        _ => common,
    })
}

/// Joins `relative` under `base` with relative components resolved by name.
fn lexical(base: &Path, relative: &str) -> PathBuf {
    let mut out = base.to_path_buf();
    for component in Path::new(relative).components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// The aterm workspace `cwd` is in: the nearest ancestor whose manifest has a `[workspace]` table.
fn enclosing(sys: &dyn System, user: &User, cwd: &Path) -> Result<Option<PathBuf>, String> {
    for dir in cwd.ancestors() {
        let manifest = dir.join("Cargo.toml");
        if sys.stat(&manifest).is_err() {
            continue;
        }
        let bytes = sys
            .read_owned_file(&manifest, user.uid, MANIFEST_MAX, false)
            .map_err(|_| format!("cannot safely read {}", manifest.display()))?;
        let Some(value) = std::str::from_utf8(&bytes)
            .ok()
            .and_then(|text| aterm_toml::from_str::<aterm_toml::Value>(text).ok())
        else {
            return Err(format!("{} is not a TOML manifest", manifest.display()));
        };
        if value.get("workspace").is_none() {
            continue;
        }
        return Ok(workspace_of(&value).map(|_| dir.to_path_buf()));
    }
    Ok(None)
}

/// Where the recorded checkout `path` is missing or is not an aterm workspace: the probe that
/// says so; else its origin.
fn admitted(sys: &dyn System, user: &User, path: &Path, spelled: &str) -> Result<Origin, Probe> {
    match sys.stat(path) {
        Ok(stat) if stat.dir && !stat.symlink => {}
        Ok(_) => {
            return Err(Probe::Blocked {
                why: format!("{spelled} is not a directory; init.toml's [develop] names it"),
            });
        }
        Err(_) => {
            return Err(Probe::Blocked {
                why: format!(
                    "{spelled} is missing: clone it there (a bare `aterm init` does, when \
                     init.toml's [github] owns it), or take it out of init.toml's [develop] \
                     checkouts (`aterm init --no-develop` removes the whole table)"
                ),
            });
        }
    }
    match workspace(sys, user, path) {
        Ok(Some(workspace)) => Ok(workspace.origin),
        Ok(None) => Err(Probe::Blocked {
            why: format!(
                "{spelled} is not an aterm workspace (no Cargo.toml with [workspace.metadata.aterm])"
            ),
        }),
        Err(why) => Err(Probe::Unusable { why }),
    }
}

/// The per-checkout fetch and fast-forward item.
struct Checkout {
    user: User,
    path: PathBuf,
    spelled: String,
    /// Origin's host could not be reached by the last probe ([`Item::read_aside`]).
    unread: Aside,
    /// Origin's host could not be reached by the last apply ([`Item::stood_aside`]).
    stood: Aside,
    reader: Reader,
}

/// `spelled` as one shell word that still expands a leading `~/`: as it is when every byte is
/// one a shell takes literally, else single-quoted after that `~/`.
fn shell_word(spelled: &str) -> String {
    let plain = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/._-+@%=:,".contains(&b))
    };
    let (tilde, rest) = spelled
        .strip_prefix("~/")
        .map_or(("", spelled), |rest| ("~/", rest));
    if plain(rest) {
        format!("{tilde}{rest}")
    } else {
        format!("{tilde}'{}'", rest.replace('\'', "'\\''"))
    }
}

/// What an item met that it stands aside on (the network down), kept for the engine to ask.
#[derive(Default)]
struct Aside(Mutex<Option<StoodAside>>);

impl Aside {
    fn set(&self, aside: Option<StoodAside>) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = aside;
    }

    fn get(&self) -> Option<StoodAside> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// The aside an offline read or move of `spelled` stands on.
fn offline(spelled: &str, unread: &checkout::Unread) -> Option<StoodAside> {
    unread.offline.then(|| {
        StoodAside::Offline(format!(
            "{spelled}: {}; the next `aterm init` reads it again",
            unread.why
        ))
    })
}

impl Item for Checkout {
    fn id(&self) -> String {
        format!("develop.checkout@{}", self.spelled)
    }
    fn family(&self) -> &'static str {
        "checkout"
    }
    fn title(&self) -> String {
        format!(
            "{} fast-forwarded to origin's default branch when nothing in it is lost",
            self.spelled
        )
    }
    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        self.unread.set(None);
        let origin = match admitted(sys, &self.user, &self.path, &self.spelled) {
            Ok(origin) => origin,
            Err(probe) => return probe,
        };
        if self.reader == Reader::Doctor {
            return match checkout::read_fetched(sys, &self.user, &self.path, &origin) {
                Ok(stands) => stands.probe_as_fetched(),
                Err(checkout::NotFetched::Unread(unread)) => Probe::Unusable { why: unread.why },
                Err(checkout::NotFetched::Unrecorded { why, act }) => Probe::Blocked {
                    why: format!(
                        "{why}: `git -C {} {act}` records it (it asks origin once)",
                        shell_word(&self.spelled)
                    ),
                },
            };
        }
        match checkout::read(sys, &self.user, &self.path, &origin) {
            Ok(stands) => stands.probe(),
            Err(unread) => {
                self.unread.set(offline(&self.spelled, &unread));
                Probe::Unusable { why: unread.why }
            }
        }
    }
    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        self.stood.set(None);
        if self.reader == Reader::Doctor {
            return Err(DOCTOR_CHANGES_NOTHING.into());
        }
        let origin =
            admitted(sys, &self.user, &self.path, &self.spelled).map_err(|probe| match probe {
                Probe::Blocked { why } | Probe::Unusable { why } => why,
                _ => format!("{} cannot be read", self.spelled),
            })?;
        match checkout::align(sys, &self.user, &self.path, &origin) {
            Ok(_) => Ok(()),
            Err(unread) => match offline(&self.spelled, &unread) {
                Some(aside) => {
                    self.stood.set(Some(aside));
                    Ok(())
                }
                None => Err(unread.why),
            },
        }
    }
    fn read_aside(&self) -> Option<StoodAside> {
        self.unread.get()
    }
    fn stood_aside(&self) -> Option<StoodAside> {
        self.stood.get()
    }
}

/// What `git submodule status -- vendor/astream` says.
enum Submodule {
    Absent,
    Uninitialized,
    Current,
    Moved,
    Conflicted,
}

/// The per-checkout submodule alignment item.
struct Submodules {
    user: User,
    path: PathBuf,
    spelled: String,
    /// The submodule's remote could not be reached by the last apply.
    stood: Aside,
    reader: Reader,
}

/// What [`Submodules`] would change, and what it leaves as it is.
struct Needs {
    recurse: bool,
    update: bool,
    kept: Option<String>,
}

impl Submodules {
    fn git(&self) -> Cmd {
        checkout::git(&self.user, Some(&self.path))
    }

    fn read(&self, sys: &dyn System) -> Result<(Submodule, bool), String> {
        let recurse = sys
            .run_complete_stdout(&self.git().args(["config", "--get", "submodule.recurse"]))
            .map_err(|_| "cannot run git config in the checkout")?;
        let recurse = match recurse.status {
            Some(0) => String::from_utf8_lossy(&recurse.stdout).trim() == "true",
            Some(1) => false,
            _ => return Err("cannot read the checkout's submodule.recurse".into()),
        };
        let status = sys
            .run_complete_stdout(&self.git().args(["submodule", "status", "--", SUBMODULE]))
            .map_err(|_| "cannot run git submodule status in the checkout")?;
        let line = String::from_utf8_lossy(&status.stdout).into_owned();
        let state = if !status.success() || line.trim().is_empty() {
            Submodule::Absent
        } else {
            match line.chars().next() {
                Some('-') => Submodule::Uninitialized,
                Some(' ') => Submodule::Current,
                Some('+') => Submodule::Moved,
                Some('U') => Submodule::Conflicted,
                _ => return Err("git submodule status answered a line init does not read".into()),
            }
        };
        Ok((state, recurse))
    }

    /// A moved submodule is checked out again only when it merely lags the tree.
    fn moved_safely(&self, sys: &dyn System) -> Result<Option<String>, String> {
        let inner = self.path.join(SUBMODULE);
        let git = checkout::git(&self.user, Some(&inner));
        let status = sys
            .run_complete_stdout(&git.clone().args([
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=normal",
            ]))
            .map_err(|_| "cannot run git status in vendor/astream")?;
        if !status.success() {
            return Err("cannot read vendor/astream's worktree".into());
        }
        if !status.stdout.is_empty() {
            return Ok(Some(
                "vendor/astream has uncommitted changes, so it stays at the commit it is on".into(),
            ));
        }
        let branch = sys
            .run_complete_stdout(
                &git.clone()
                    .args(["symbolic-ref", "--quiet", "--short", "HEAD"]),
            )
            .map_err(|_| "cannot run git symbolic-ref in vendor/astream")?;
        match branch.status {
            Some(0) => {
                return Ok(Some(format!(
                    "vendor/astream is on its branch {}, so it stays where it is",
                    String::from_utf8_lossy(&branch.stdout).trim()
                )));
            }
            Some(1) => {}
            _ => return Err("cannot read vendor/astream's HEAD".into()),
        }
        let head = sys
            .run_complete_stdout(&git.args(["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]))
            .map_err(|_| "cannot run git rev-parse in vendor/astream")?;
        let head = String::from_utf8_lossy(&head.stdout).trim().to_owned();
        if head.len() < 40 || !head.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("cannot read vendor/astream's commit".into());
        }
        let recorded = sys
            .run_complete_stdout(&self.git().args([
                "log",
                "-1",
                "--format=%H",
                &format!("--find-object={head}"),
                "HEAD",
                "--",
                SUBMODULE,
            ]))
            .map_err(|_| "cannot run git log in the checkout")?;
        if !recorded.success() {
            return Err("cannot read which commits of the checkout record vendor/astream".into());
        }
        Ok(recorded.stdout.trim_ascii().is_empty().then(|| {
            format!(
                "vendor/astream is at {}, which no commit of {} records (astream work whose \
                 bump is not committed yet), so it stays where it is",
                head.get(..12).unwrap_or(&head),
                self.spelled
            )
        }))
    }

    fn needs(&self, sys: &dyn System) -> Result<Option<Needs>, String> {
        let (state, recurse) = self.read(sys)?;
        let (update, kept) = match state {
            Submodule::Absent => return Ok(None),
            Submodule::Uninitialized => (true, None),
            Submodule::Current => (false, None),
            Submodule::Moved => match self.moved_safely(sys)? {
                None => (true, None),
                Some(kept) => (false, Some(kept)),
            },
            Submodule::Conflicted => (
                false,
                Some("vendor/astream has a merge conflict in the checkout".to_owned()),
            ),
        };
        Ok(Some(Needs {
            recurse: !recurse,
            update,
            kept,
        }))
    }
}

impl Item for Submodules {
    fn id(&self) -> String {
        format!("develop.submodules@{}", self.spelled)
    }
    fn family(&self) -> &'static str {
        "checkout"
    }
    fn title(&self) -> String {
        format!(
            "{}'s vendor/astream checked out, and submodule.recurse true",
            self.spelled
        )
    }
    fn after(&self) -> Vec<String> {
        vec![format!("develop.checkout@{}", self.spelled)]
    }
    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        if let Err(probe) = admitted(sys, &self.user, &self.path, &self.spelled) {
            return probe;
        }
        let needs = match self.needs(sys) {
            Ok(Some(needs)) => needs,
            Ok(None) => {
                return Probe::Ok {
                    now: format!(
                        "{}'s tree records no vendor/astream submodule",
                        self.spelled
                    ),
                };
            }
            Err(why) => return Probe::Unusable { why },
        };
        if !needs.recurse && !needs.update {
            return Probe::Ok {
                now: needs.kept.map_or_else(
                    || {
                        "vendor/astream at the commit the tree records; submodule.recurse true"
                            .into()
                    },
                    |kept| format!("submodule.recurse true; left as it is: {kept}"),
                ),
            };
        }
        let (mut from, mut to) = (Vec::new(), Vec::new());
        if needs.update {
            from.push("vendor/astream is not at the commit the tree records".to_owned());
            to.push("`git submodule update --init --checkout -- vendor/astream`");
        }
        if needs.recurse {
            from.push("submodule.recurse is not true".to_owned());
            to.push("`git config submodule.recurse true`");
        }
        if let Some(kept) = needs.kept {
            from.push(format!("left as it is: {kept}"));
        }
        Probe::Change {
            from: from.join("; "),
            to: to.join("; "),
        }
    }
    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        self.stood.set(None);
        if self.reader == Reader::Doctor {
            return Err(DOCTOR_CHANGES_NOTHING.into());
        }
        let needs = self
            .needs(sys)?
            .ok_or("vendor/astream is gone from the checkout")?;
        if needs.recurse {
            let ran = sys
                .run_complete_stdout(&self.git().args(["config", "submodule.recurse", "true"]))
                .map_err(|_| "cannot run git config in the checkout")?;
            if !ran.success() {
                return Err("git config submodule.recurse true failed".into());
            }
        }
        if needs.update {
            let ran = sys
                .run_complete_stdout(
                    &self
                        .git()
                        .budget(LONG_BUDGET)
                        .args(checkout::NO_SIDE_RUNS)
                        .args([
                            "submodule",
                            "update",
                            "--init",
                            "--checkout",
                            "--",
                            SUBMODULE,
                        ]),
                )
                .map_err(|_| "cannot run git submodule update in the checkout")?;
            if !ran.success() {
                if checkout::unreachable(&ran.stderr) {
                    self.stood.set(Some(StoodAside::Offline(format!(
                        "{}: cannot reach vendor/astream's remote (offline); the next `aterm \
                         init` checks it out",
                        self.spelled
                    ))));
                    return Ok(());
                }
                return Err(
                    "git submodule update --init --checkout -- vendor/astream failed (no access \
                     to its remote, or no such commit there)"
                        .into(),
                );
            }
        }
        Ok(())
    }
    fn stood_aside(&self) -> Option<StoodAside> {
        self.stood.get()
    }
}

/// The per-checkout workspace-tools provision item.
struct WorkspaceTools {
    user: User,
    path: PathBuf,
    spelled: String,
    targo: Option<Targo>,
    reader: Reader,
}

/// The base items the tools wait for: the C toolchain, and the toolset.
const TOOLS_AFTER: [&str; 2] = [crate::init_clt::ID, "base.pkg.toolset"];

impl WorkspaceTools {
    /// The checkout's own verb: under `--locked`, and for doctor `--offline` too, so cargo
    /// downloads nothing the lock names that this Mac does not hold, and with doctor's shorter
    /// wait ([`DOCTOR_BUDGET`]).
    fn command(&self, targo: &Path, check: bool) -> Cmd {
        let bin = targo.parent().unwrap_or_else(|| Path::new("/"));
        let mut cmd = Cmd::new(targo)
            .as_user(&self.user)
            .current_dir(&self.path)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", bin.display()),
            )
            .budget(match self.reader {
                Reader::Init => LONG_BUDGET,
                Reader::Doctor => DOCTOR_BUDGET,
            })
            .args(["--unverified", "--locked"]);
        if self.reader == Reader::Doctor {
            cmd = cmd.arg("--offline");
        }
        cmd = cmd.args(["verify", "provision"]);
        if check {
            cmd = cmd.arg("--check");
        }
        cmd
    }

    fn targo(&self) -> Result<PathBuf, String> {
        self.targo.as_ref().map_or_else(
            || Err("this aterm cannot find the toolset's targo".to_owned()),
            |targo| targo(),
        )
    }

    /// Whether init may run the checkout's code ([`checkout::kept_here`]).
    fn runnable(&self, sys: &dyn System) -> Result<(), Probe> {
        let origin = admitted(sys, &self.user, &self.path, &self.spelled)?;
        match checkout::kept_here(sys, &self.user, &self.path, &origin) {
            Ok(None) => Ok(()),
            Ok(Some(why)) => Err(Probe::NotApplicable {
                why: format!(
                    "{} is left as it is ({why}), and init runs no code from a tree it leaves \
                     as it is: `targo --unverified verify provision` there provisions it",
                    self.spelled
                ),
            }),
            Err(why) => Err(Probe::Unusable { why }),
        }
    }
}

/// Whether a cargo run refused to rewrite the lock file `--locked` protects.
fn lock_refused(stderr: &[u8]) -> bool {
    String::from_utf8_lossy(stderr).contains("because --locked was passed")
}

const STALE_LOCK: &str = "the checkout's Cargo.lock does not match its manifests, and init \
                          never rewrites it (cargo refused under --locked)";

/// Whether a cargo run under `--offline` stopped on a download it would have needed.
fn offline_refused(stderr: &[u8]) -> bool {
    let said = String::from_utf8_lossy(stderr);
    said.contains("--offline was specified") || said.contains("offline mode (--offline)")
}

const NOT_DOWNLOADED: &str = "the checkout's lock names crates this Mac has not downloaded, and \
                              aterm doctor reads no network (cargo --offline): `aterm init \
                              --develop` fetches them";

/// What a `verify provision` run said.
fn said(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = text
        .lines()
        .filter_map(|line| line.strip_prefix("verify provision: "))
        .collect();
    let summary = lines.last().copied().unwrap_or("no summary line");
    let rows: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|line| {
            *line != summary && (line.starts_with("MISSING") || line.starts_with("FAIL"))
        })
        .take(3)
        .collect();
    if rows.is_empty() {
        summary.to_owned()
    } else {
        format!("{summary} ({})", rows.join("; "))
    }
}

impl Item for WorkspaceTools {
    fn id(&self) -> String {
        format!("develop.workspace-tools@{}", self.spelled)
    }
    fn family(&self) -> &'static str {
        "tooling"
    }
    fn title(&self) -> String {
        format!(
            "The tools {}'s tests run against, as its tree pins them",
            self.spelled
        )
    }
    fn after(&self) -> Vec<String> {
        let mut after: Vec<String> = TOOLS_AFTER.iter().map(|&id| id.to_owned()).collect();
        after.push(format!("develop.checkout@{}", self.spelled));
        after.push(format!("develop.submodules@{}", self.spelled));
        after
    }
    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        if let Err(probe) = self.runnable(sys) {
            return probe;
        }
        let targo = match self.targo() {
            Ok(targo) => targo,
            Err(why) => return Probe::Blocked { why },
        };
        let ran = match sys.run(&self.command(&targo, true)) {
            Ok(ran) => ran,
            Err(RunFailed::TimedOut(budget)) if self.reader == Reader::Doctor => {
                return Probe::Blocked {
                    why: format!(
                        "the checkout's verify provision --check gave no answer within {}s, so \
                         doctor stopped it (cargo may be waiting while another build holds the \
                         checkout's build directory, or building aterm-verify after a pull): \
                         the next doctor reads it again, and `aterm init --check --develop` \
                         waits for it",
                        budget.as_secs()
                    ),
                };
            }
            Err(e) => {
                return Probe::Unusable {
                    why: format!("cannot run the checkout's verify provision --check: {e}"),
                };
            }
        };
        match ran.status {
            Some(0) => Probe::Ok {
                now: said(&ran.stderr),
            },
            Some(1) => Probe::Change {
                from: said(&ran.stderr),
                to: "placed by the checkout's `targo --unverified verify provision`".into(),
            },
            Some(3) => Probe::NotApplicable {
                why: said(&ran.stderr),
            },
            Some(2) => Probe::Unusable {
                why: "the checkout's verify has no provision verb (a tree older than it)".into(),
            },
            _ if lock_refused(&ran.stderr) => Probe::Unusable {
                why: STALE_LOCK.into(),
            },
            _ if self.reader == Reader::Doctor && offline_refused(&ran.stderr) => Probe::Blocked {
                why: NOT_DOWNLOADED.into(),
            },
            _ => Probe::Unusable {
                why: format!(
                    "the checkout's verify provision --check did not finish ({})",
                    ran.describe()
                ),
            },
        }
    }
    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        if self.reader == Reader::Doctor {
            return Err(DOCTOR_CHANGES_NOTHING.into());
        }
        self.runnable(sys).map_err(|probe| match probe {
            Probe::NotApplicable { why } | Probe::Blocked { why } | Probe::Unusable { why } => why,
            _ => format!("{} cannot be read", self.spelled),
        })?;
        let targo = self.targo()?;
        let ran = sys
            .run(&self.command(&targo, false))
            .map_err(|e| format!("cannot run the checkout's verify provision: {e}"))?;
        if ran.success() {
            Ok(())
        } else if lock_refused(&ran.stderr) {
            Err(STALE_LOCK.into())
        } else {
            Err(format!("verify provision: {}", said(&ran.stderr)))
        }
    }
}

/// Where init places the develop record item.
fn record_place(want: Option<&[String]>) -> Place {
    let mut place = place(Class::Relay);
    if want.is_none() {
        place.remedy = Remedy::Command("aterm init --no-develop".into());
    }
    place
}

/// The develop record item when the develop layer cannot be composed.
struct Unreadable {
    why: String,
}

impl Item for Unreadable {
    fn id(&self) -> String {
        RECORD.to_owned()
    }
    fn family(&self) -> &'static str {
        "profile"
    }
    fn title(&self) -> String {
        "This Mac's profile ([develop] in ~/.config/aterm/init.toml)".to_owned()
    }
    fn probe(&self, _sys: &dyn System, _ctx: &Ctx) -> Probe {
        Probe::Unusable {
            why: self.why.clone(),
        }
    }
    fn apply(&self, _sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        Err(self.why.clone())
    }
}

/// The develop layer as a run converges it ([`layer`]).
#[derive(Default)]
pub struct Composed {
    /// The items, each with its place, in run order.
    pub items: Vec<Placed>,
    /// The layer could not be composed.
    pub unreadable: bool,
}

impl Composed {
    /// The layer that could not be composed: one `develop.record` row saying why.
    #[must_use]
    pub fn unreadable(why: String) -> Self {
        let mut place = place(Class::Relay);
        place.remedy = Remedy::None;
        Self {
            items: vec![(Box::new(Unreadable { why }), place)],
            unreadable: true,
        }
    }

    fn of(composed: Result<Vec<Placed>, String>) -> Self {
        match composed {
            Ok(items) => Self {
                items,
                unreadable: false,
            },
            Err(why) => Self::unreadable(why),
        }
    }
}

/// [`compose`] for a run.
#[must_use]
pub fn layer(sys: &dyn System, ctx: &Ctx, inputs: &Inputs<'_>) -> Composed {
    Composed::of(compose(sys, ctx, inputs))
}

/// The develop layer as `aterm doctor` reads it for `ctx`'s one user: on a develop machine the
/// items of the checkouts `init.toml` records, nothing on a Mac that records none unless `asked`,
/// and a profile that cannot be read is one `develop.record` row saying why. Each item reads this
/// Mac alone and applies nothing.
#[must_use]
pub fn doctor(sys: &dyn System, ctx: &Ctx, asked: bool, targo: Option<Targo>) -> Composed {
    let composed = (|| {
        let [user] = ctx.users.as_slice() else {
            return Err("aterm doctor reads the develop layer of exactly one local user".into());
        };
        let recorded = read_profile(sys, user)?.table.map(|table| table.checkouts);
        if recorded.is_none() && !asked {
            return Ok(Vec::new());
        }
        items(user, recorded, Reader::Doctor, targo)
    })();
    Composed::of(composed)
}

/// Whether the account at `uid` and `home` may have a develop layer for doctor to read: false
/// only when its `init.toml` is absent or has no `[develop]` table.
#[must_use]
pub fn may_record(sys: &dyn System, uid: u32, home: &Path) -> bool {
    let account = User {
        name: String::new(),
        uid,
        gid: 0,
        home: home.to_string_lossy().into_owned(),
        shell: String::new(),
        admin: false,
    };
    read_profile(sys, &account).map_or(true, |profile| profile.table.is_some())
}

/// Probes the rows of `composed` ([`doctor`]) narrowed to `only` without applying or journaling.
///
/// # Errors
/// The composition cannot be ordered ([`aterm_converge::Refusal`]); nothing was read.
pub fn read(
    sys: &dyn System,
    ctx: &Ctx,
    composed: &Composed,
    only: &[String],
) -> Result<Vec<ItemReport>, String> {
    let bound = crate::engine::bind(
        composed
            .items
            .iter()
            .map(|(item, place)| (item.as_ref(), place.clone())),
        sys,
        ctx,
    );
    let bound = if only.is_empty() {
        bound
    } else {
        let mut words = only.to_vec();
        if composed.unreadable {
            words.push(RECORD.to_owned());
        }
        aterm_converge::select(bound, &words).0
    };
    aterm_converge::run_scoped(
        &bound,
        Mode::Plan,
        Scope::REPAIR,
        &mut aterm_converge::ReadOnly,
        &mut |_| {},
    )
    .map_err(|refused| refused.to_string())
}

/// What the develop layer is asked for.
pub struct Inputs<'a> {
    /// `--develop` (`Some(true)`), `--no-develop` (`Some(false)`), or neither: as recorded.
    pub want: Option<bool>,
    /// An `--only` word named a develop item, so the layer runs even unrecorded.
    pub asked: bool,
    /// The directory `aterm init` was started in, for `--develop` to find its workspace.
    pub cwd: Option<&'a Path>,
    /// The toolset's targo ([`Targo`]).
    pub targo: Option<Targo>,
}

/// The develop items for `ctx`'s one user, each with its place, in run order.
///
/// # Errors
/// `ctx` names no one user, the profile or a manifest cannot be read safely or is invalid,
/// or `--develop` runs in a linked worktree, cannot list the checkouts the GitHub item
/// verifies, or finds no checkout to record and none is recorded.
pub fn compose(sys: &dyn System, ctx: &Ctx, inputs: &Inputs<'_>) -> Result<Vec<Placed>, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("aterm init --develop requires exactly one local user".into());
    };
    let profile = read_profile(sys, user)?;
    let recorded = profile.table.map(|table| table.checkouts);
    if recorded.is_none() && inputs.want.is_none() && !inputs.asked {
        return Ok(Vec::new());
    }
    let want = match inputs.want {
        Some(false) => None,
        None => recorded,
        Some(true) => {
            let found = match inputs
                .cwd
                .map(|cwd| enclosing(sys, user, cwd))
                .transpose()?
            {
                Some(Some(root)) => {
                    if let Some(main) = linked_worktree_of(sys, user, &root) {
                        return Err(format!(
                            "aterm init --develop: {} is a linked worktree of {}, removed once \
                             its work lands, so it is not recorded; run it in {} to record that \
                             checkout",
                            spell(user, &root)?,
                            spell(user, &main)?,
                            spell(user, &main)?
                        ));
                    }
                    vec![root]
                }
                _ => crate::init_github::aterm_checkouts(sys, ctx)?,
            };
            let mut want = recorded.unwrap_or_default();
            for path in found {
                let spelled = spell(user, &path)?;
                if !want.contains(&spelled) {
                    want.push(spelled);
                }
            }
            if want.is_empty() {
                return Err(
                    "aterm init --develop: no aterm checkout to record. Run it inside one (a \
                     Cargo.toml with [workspace.metadata.aterm]), or name its GitHub owner in \
                     init.toml's [github] table so a bare `aterm init` clones it"
                        .into(),
                );
            }
            Some(want)
        }
    };
    items(user, want, Reader::Init, inputs.targo.clone())
}

/// The record wanting `want`, then each checkout's three items, for `reader`.
fn items(
    user: &User,
    want: Option<Vec<String>>,
    reader: Reader,
    targo: Option<Targo>,
) -> Result<Vec<Placed>, String> {
    let record = match reader {
        Reader::Init => record_place(want.as_deref()),
        Reader::Doctor => place(Class::Relay),
    };
    let mut items: Vec<Placed> = vec![(
        Box::new(Record {
            user: user.clone(),
            want: want.clone(),
            reader,
        }),
        record,
    )];
    for spelled in want.unwrap_or_default() {
        let path = expand(user, &spelled)?;
        items.push((
            Box::new(Checkout {
                user: user.clone(),
                path: path.clone(),
                spelled: spelled.clone(),
                unread: Aside::default(),
                stood: Aside::default(),
                reader,
            }),
            place(Class::Install),
        ));
        items.push((
            Box::new(Submodules {
                user: user.clone(),
                path: path.clone(),
                spelled: spelled.clone(),
                stood: Aside::default(),
                reader,
            }),
            place(Class::Install),
        ));
        items.push((
            Box::new(WorkspaceTools {
                user: user.clone(),
                path,
                spelled,
                targo: targo.clone(),
                reader,
            }),
            place(Class::Install),
        ));
    }
    Ok(items)
}

#[cfg(test)]
pub(crate) mod tests;
