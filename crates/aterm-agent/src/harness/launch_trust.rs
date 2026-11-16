// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! FOLDER TRUST SETTLED BEFORE A CLAUDE CODE LAUNCH, so the `Accessing workspace` dialog never
//! draws in a folder the harness's own rules would have trusted.
//!
//! Claude Code draws that dialog, with `No, exit` focused, in every folder its profile holds no
//! trust record for, whatever its permission mode. Answered after it draws, it stands on the
//! screen for a second or more in front of a person who answers in about one. [`settle`] has
//! Claude Code record the trust itself before the launch: the build is started headless in the
//! launch's own folder and environment, handed one `initialize` control request whose
//! `workspaceTrust` attests the folder, and it writes its own profile under its own lock. aterm
//! writes no byte of that profile.
//!
//! The judgment is the dialog rule's own ([`approval::trust_folder_rule`]), so the ledger row
//! names the rule that would have answered the dialog, and a session whose Autopilot is off or
//! whose policy trusts no folder settles nothing. The headless start runs with the arguments
//! atpkg warms a build with ([`atpkg::vendor_direct::Warm::argv`]): Claude Code applies a
//! folder's project settings to a `-p` run before any trust exists, and those arguments are what
//! keep the folder's hooks and MCP servers from running (measured on 2.1.323: without them a
//! project's SessionStart hook and `.mcp.json` server both ran). So the gate comes before the
//! start, never after.
//!
//! What it cannot settle is left to the dialog's rule: the home directory, whose trust Claude
//! Code holds for one session and never records; a build that refuses or does not know the
//! attestation; a start that outlives [`DEADLINE`]; a session a hold stands on or another
//! supervisor claims; a launch with no terminal on its output; a program that is no Claude Code
//! build of atpkg's own store.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use alab_policy::config::Autopilot;
use serde::Deserialize;

use super::{profile, token_route};
use crate::supervise::SupervisorConfig;
use crate::supervise::policy::approval;

/// The `request_id` of the one control request [`settle`] sends, and of the answer it reads.
pub const REQUEST_ID: &str = "aterm-trust";

/// When the headless start's process group is killed, counted from its start: a hard bound on
/// what a launch waits for, not what the attestation takes. The start is not ended at its
/// answer: its stdin is closed there and it exits by itself, so whatever it was writing (its
/// profile under its own lock, a login it was refreshing) is finished by it and not cut short.
/// Measured on Claude Code 2.1.323 in a lab's scratch home (`lab launch-trust`, 2026-11-12, on
/// a Mac busy with other builds, three runs): its answer 160 to 207 ms after the start, its exit
/// 18 to 19 ms after its stdin closed, status 0.
pub const DEADLINE: Duration = Duration::from_secs(5);

/// How long an answer of Claude Code's own that it records nothing for a folder stands in this
/// process before the same build is asked again for that folder under that home
/// ([`StoreVendor`]).
pub const DECLINE_STANDS: Duration = Duration::from_secs(600);

/// How long a start that gave no answer (one that could not be started, ended unanswered or
/// outlived its deadline) stands before it is tried again: long enough that a relaunch pass,
/// which plans again every few seconds, does not hold each plan for [`DEADLINE`], and short
/// enough that a start missed once on a loaded machine is not a folder never settled.
pub const FAILURE_STANDS: Duration = Duration::from_secs(30);

/// The most bytes of the headless start's output [`StoreVendor`] reads for its answer (the
/// answer is one line of about 18 kB on 2.1.323).
#[cfg(unix)]
const ANSWER_MAX: usize = 1024 * 1024;

/// One launch, as [`settle`] reads it.
#[derive(Clone, Copy, Debug)]
pub struct Launch<'a> {
    /// The launch's argv, `argv[0]` first.
    pub argv: &'a [String],
    /// The folder it starts in.
    pub cwd: &'a Path,
    /// Its environment.
    pub env: &'a [(String, String)],
    /// The account's home directory, where the environment names no `HOME`.
    pub account_home: Option<&'a Path>,
    /// The user home the profile is resolved under ([`profile::effective_of`]).
    pub user_home: &'a Path,
    /// The harness is off for this launch (`ATERM_NO_HARNESS`).
    pub no_harness: bool,
    /// The launch's standard output is known to be no terminal ([`STDOUT_TTY_ENV`] says `0`):
    /// Claude Code prints and exits there whatever its arguments, and draws no dialog.
    pub no_terminal: bool,
    /// The Claude Code build the launch runs, where it could be resolved.
    pub build: Option<&'a Path>,
}

/// What the policy says of a launch's folder ([`gate`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Gate {
    /// The rule that trusts it ([`approval::trust_folder_rule`]).
    Trusts(&'static str),
    /// Nothing trusts it here, and why.
    Refuses(&'static str),
    /// What would say could not be read, and why: never read as trust.
    Unread(&'static str),
}

/// The policy a session's folder-trust dialog would be answered under.
#[derive(Clone, Copy, Debug)]
pub struct Policy<'a> {
    /// `[harness]`.
    pub config: &'a SupervisorConfig,
    /// The session's own Autopilot word (`None`: it has none, and the table's stands).
    pub autopilot: Option<Autopilot>,
    /// The folder the session's starter trusted (`meta trusted_cwd`).
    pub trusted_cwd: Option<&'a Path>,
    /// The home `trust_roots` resolves `~` under.
    pub home: Option<&'a Path>,
    /// The user whose `claude-*` scratch root a trust root names.
    pub uid: u32,
}

/// THE GATE: whether the policy that would answer `cwd`'s folder-trust dialog trusts it, by the
/// dialog rule's own judgment ([`approval::trust_folder_rule`]), under the conditions the loop
/// answers a dialog at all: the harness on for Claude Code and the session's Autopilot not off.
#[must_use]
pub fn gate(policy: &Policy<'_>, cwd: &Path) -> Gate {
    if !policy.config.on_for(aterm_phase::Program::Claude) {
        return Gate::Refuses("the harness is off for Claude Code");
    }
    if policy.config.autopilot_for(policy.autopilot) == Autopilot::Off {
        return Gate::Refuses("Autopilot is off");
    }
    let roots = approval::trust_roots_of(&policy.config.trust_roots, policy.home, policy.uid);
    match approval::trust_folder_rule(policy.config.approve, &roots, policy.trusted_cwd, cwd) {
        Some(rule) => Gate::Trusts(rule),
        None => Gate::Refuses("the policy trusts no folder here"),
    }
}

/// What [`settle`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Settled {
    /// No interactive launch: one that prints and exits, a subcommand, a line that does not
    /// parse, one with no terminal on its output, or the harness off for it.
    NotInteractive,
    /// A relaunch line whose program is neither a Claude Code build of atpkg's own store nor
    /// atpkg's twin of one ([`store_build`]): a person's wrapper, another install, a test's
    /// stand-in. Never started headless, and nothing of the machine is read for it.
    NotStoreBuild,
    /// The folder is the home directory, whose trust Claude Code never records.
    Home,
    /// The folder, its profile, its build or its session could not be read, and why
    /// (journaled: the step was skipped for no reason of the policy's).
    Unread(&'static str),
    /// The profile already records the folder's exact trust key.
    Already,
    /// The policy trusts nothing here, and why ([`Gate::Refuses`]).
    Gated(&'static str),
    /// Claude Code recorded the trust under `rule`.
    Recorded {
        /// The rule ([`approval::trust_folder_rule`]).
        rule: &'static str,
        /// Who recorded it (`Claude Code <version>`).
        by: String,
    },
    /// Claude Code recorded nothing, and why: the dialog's rule answers.
    Refused(String),
}

/// What a headless start answered ([`Vendor::attest`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Attested {
    /// `workspace_trust_recorded: true`, and the version the answer names.
    Recorded {
        /// `claude_code_version`, empty where the answer names none.
        version: String,
    },
    /// Claude Code's own answer that it records nothing, and why: `workspace_trust_recorded:
    /// false`, an answer without the field, or the request refused.
    Declined(String),
    /// No answer, and why: a start that could not be made, ended unanswered, answered what
    /// could not be read, or outlived its deadline.
    Failed(String),
    /// Nothing was started: an earlier [`Self::Declined`] or [`Self::Failed`] for the same ask
    /// still stands ([`DECLINE_STANDS`], [`FAILURE_STANDS`]), and why. Not journaled again.
    Stands(String),
}

/// The headless start.
pub trait Vendor {
    /// Start `build` in `cwd` under `env`, attest `directory`, and read its answer within
    /// `deadline`.
    fn attest(
        &self,
        build: &Path,
        cwd: &Path,
        env: &[(String, String)],
        directory: &str,
        deadline: Duration,
    ) -> Attested;
}

/// Where what [`settle`] did is written down.
pub trait Book {
    /// A trust Claude Code recorded: the approvals-ledger row the dialog's answer would have
    /// left, and a journal line.
    fn recorded(&mut self, rule: &str, folder: &str, by: &str);
    /// A trust Claude Code did not record, or a launch the step could not read: a journal line
    /// alone.
    fn refused(&mut self, folder: &str, why: &str);
}

/// SETTLE THE FOLDER OF `launch` BEFORE IT STARTS. In order, and nothing after the first that
/// holds: a launch that is not interactive; the home directory; a folder whose exact trust key
/// the launch's profile already records (one bounded read, the common case); a policy that
/// trusts nothing here (`gate`, asked of the resolved folder and only now: it may cost a round
/// trip to the window); then the headless start ([`Vendor::attest`]), believed only where the
/// profile then holds the record. Only a recorded trust is booked as an approval. A step skipped
/// because something could not be read ([`Settled::Unread`]), and a start that recorded nothing,
/// are journaled with their reason; a policy's refusal is not.
pub fn settle(
    launch: &Launch<'_>,
    gate: &dyn Fn(&Path) -> Gate,
    vendor: &dyn Vendor,
    book: &mut dyn Book,
    deadline: Duration,
) -> Settled {
    if launch.no_harness || launch.no_terminal || !super::tap::launch_interactive(launch.argv) {
        return Settled::NotInteractive;
    }
    let unread = |book: &mut dyn Book, folder: &str, why: &'static str| {
        book.refused(folder, why);
        Settled::Unread(why)
    };
    let Ok(cwd) = std::fs::canonicalize(launch.cwd) else {
        return unread(
            book,
            &launch.cwd.to_string_lossy(),
            "the folder could not be resolved",
        );
    };
    if is_home(&cwd, launch) {
        return Settled::Home;
    }
    let Some(folder) = cwd.to_str() else {
        return unread(
            book,
            &cwd.to_string_lossy(),
            "the folder's path is not UTF-8",
        );
    };
    let env: Vec<String> = launch
        .env
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
    let Some(profile) = profile::effective_of(&env, launch.user_home) else {
        return unread(book, folder, "the launch's profile could not be named");
    };
    let key = trust_key(&cwd);
    if recorded(&profile, &key) {
        return Settled::Already;
    }
    let rule = match gate(&cwd) {
        Gate::Trusts(rule) => rule,
        Gate::Refuses(why) => return Settled::Gated(why),
        Gate::Unread(why) => return unread(book, folder, why),
    };
    let Some(build) = launch.build else {
        return unread(book, folder, "the launch's build could not be resolved");
    };
    let refused = |book: &mut dyn Book, why: String| {
        book.refused(folder, &why);
        Settled::Refused(why)
    };
    match vendor.attest(build, &cwd, launch.env, folder, deadline) {
        Attested::Declined(why) | Attested::Failed(why) => refused(book, why),
        Attested::Stands(why) => Settled::Refused(why),
        Attested::Recorded { .. } if !recorded(&profile, &key) => refused(
            book,
            format!(
                "Claude Code answered that it recorded the trust, and {} holds none for {}",
                profile.display(),
                key.display()
            ),
        ),
        Attested::Recorded { version } => {
            let by = if version.is_empty() {
                "Claude Code".to_string()
            } else {
                format!("Claude Code {version}")
            };
            book.recorded(rule, folder, &by);
            Settled::Recorded { rule, by }
        }
    }
}

/// Whether `cwd`, resolved, is the home directory Claude Code reads: the launch's `HOME`, else
/// the account's.
fn is_home(cwd: &Path, launch: &Launch<'_>) -> bool {
    let named = launch
        .env
        .iter()
        .find(|(name, value)| name == super::accounts::HOMEDIR_ENV && !value.is_empty())
        .map(|(_, value)| PathBuf::from(value));
    named
        .or_else(|| launch.account_home.map(Path::to_path_buf))
        .and_then(|home| std::fs::canonicalize(home).ok())
        .is_some_and(|home| home == cwd)
}

/// `rel` against `base` by its spelling alone, as `path.resolve` reads it: `.` dropped, `..`
/// taking the component before it, no link followed.
fn lexical(base: &Path, rel: &str) -> PathBuf {
    let mut out = if Path::new(rel).is_absolute() {
        PathBuf::new()
    } else {
        base.to_path_buf()
    };
    for part in Path::new(rel).components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            part => out.push(part),
        }
    }
    out
}

/// Whether `git` (a folder's `.git`) is what marks a repository root for Claude Code: a
/// directory or a file, or a link to one.
fn marks_root(git: &Path) -> bool {
    std::fs::metadata(git).is_ok_and(|meta| meta.is_dir() || meta.is_file())
}

/// The text of the small regular file at `path` (a link is no such file), trimmed.
fn small_file(path: &Path) -> Option<String> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > 64 * 1024 {
        return None;
    }
    Some(std::fs::read_to_string(path).ok()?.trim().to_string())
}

/// THE MAIN CHECKOUT OF THE LINKED WORKTREE AT `root`, as Claude Code resolves it (its
/// worktree reader, read in 2.1.323): `root/.git` is a file naming a git directory, whose
/// `commondir` names the common directory it lies in the `worktrees/` of, and whose `gitdir`
/// points back at `root/.git`. Anything else (a submodule, whose git directory lies under
/// `modules/`; a `.git` file that names another repository's worktree with no pointer back) is
/// its own root.
fn main_checkout(root: &Path) -> Option<PathBuf> {
    let text = small_file(&root.join(".git"))?;
    let named = text.strip_prefix("gitdir:")?.trim();
    let git_dir = lexical(root, named);
    let common = lexical(&git_dir, &small_file(&git_dir.join("commondir"))?);
    if git_dir.parent()? != common.join("worktrees") {
        return None;
    }
    let back = lexical(&git_dir, &small_file(&git_dir.join("gitdir"))?);
    let own = std::fs::canonicalize(root).ok()?.join(".git");
    if std::fs::canonicalize(back).ok()? != own {
        return None;
    }
    if common.file_name() != Some(std::ffi::OsStr::new(".git")) {
        // A bare repository's worktree: the common directory itself, unless a checkout sits in
        // it.
        return (!marks_root(&common.join(".git"))).then_some(common);
    }
    common.parent().map(Path::to_path_buf)
}

/// THE KEY CLAUDE CODE FILES A FOLDER'S TRUST UNDER, for the resolved folder `cwd`: the nearest
/// folder at or above it that holds a `.git` (a repository root), a linked worktree's root read
/// as its main checkout ([`main_checkout`]); with no repository above it, `cwd` itself. Measured
/// on 2.1.323: a repository's subfolder and a linked worktree are filed under the main root, a
/// submodule and a folder with a forged `.git` file under themselves, and a folder outside any
/// repository under itself even below a trusted one.
///
/// Claude Code also spells the key in Unicode's composed form (NFC), which this does not;
/// [`recorded`] reads a key that is not ASCII by the folder it names.
#[must_use]
pub fn trust_key(cwd: &Path) -> PathBuf {
    let root = cwd.ancestors().find(|dir| marks_root(&dir.join(".git")));
    match root {
        Some(root) => main_checkout(root).unwrap_or_else(|| root.to_path_buf()),
        None => cwd.to_path_buf(),
    }
}

/// Whether `a` and `b` name one directory.
fn same_dir(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let id = |path: &Path| {
            std::fs::metadata(path)
                .ok()
                .filter(std::fs::Metadata::is_dir)
                .map(|meta| (meta.dev(), meta.ino()))
        };
        id(a).is_some_and(|a| Some(a) == id(b))
    }
    #[cfg(not(unix))]
    {
        let _ = (a, b);
        false
    }
}

/// Whether the profile at `profile` records trust for exactly `key` ([`trust_key`]): a
/// `projects` entry under that key with `hasTrustDialogAccepted: true`. A trusted folder above
/// it is no record: Claude Code still draws the dialog under one where the folder's own
/// settings grant permissions, and its attestation writes the exact key anyway. A key that is
/// not ASCII is also read by the folder it names, since Claude Code may spell it in another
/// Unicode form than the disk does.
#[must_use]
pub fn recorded(profile: &Path, key: &Path) -> bool {
    let Ok(trusted) = profile::trusted_folders(profile) else {
        return false;
    };
    let Some(spelled) = key.to_str() else {
        return false;
    };
    trusted.iter().any(|folder| folder == spelled)
        || (!spelled.is_ascii()
            && trusted
                .iter()
                .any(|folder| !folder.is_ascii() && same_dir(Path::new(folder), key)))
}

/// The one line [`settle`]'s headless start is sent: an `initialize` control request whose
/// `workspaceTrust` attests `directory`, an absolute path.
#[must_use]
pub fn request_line(directory: &str) -> String {
    format!(
        "{{\"type\":\"control_request\",\"request_id\":\"{REQUEST_ID}\",\"request\":{{\"subtype\":\
         \"initialize\",\"workspaceTrust\":{{\"accepted\":true,\"directory\":{}}}}}}}\n",
        crate::supervise::journal::json_str(directory)
    )
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    response: Option<RawOuter>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawOuter {
    subtype: Option<String>,
    request_id: Option<String>,
    response: Option<RawInner>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawInner {
    workspace_trust_recorded: Option<bool>,
    claude_code_version: Option<String>,
}

/// What one line of the headless start's output answers: `None` for a line that is not the
/// answer to [`REQUEST_ID`]. Only `workspace_trust_recorded: true` in a `success` is a recorded
/// trust: `false` is Claude Code's refusal (the home directory, a folder that resolves to
/// another key), and an answer without the field is a build that does not take the attestation;
/// both are its own answer ([`Attested::Declined`]). An answer that cannot be read is none
/// ([`Attested::Failed`]).
#[must_use]
pub fn answer_of(line: &str) -> Option<Attested> {
    let line = line.trim();
    if !line.starts_with('{') {
        return None;
    }
    let raw = match aterm_json::from_slice::<RawLine>(line.as_bytes()) {
        Ok(raw) => raw,
        // A line that names the request and cannot be read is its answer, malformed.
        Err(_) if line.contains(REQUEST_ID) && line.contains("control_response") => {
            return Some(Attested::Failed(
                "Claude Code's answer could not be read".to_string(),
            ));
        }
        Err(_) => return None,
    };
    if raw.kind.as_deref() != Some("control_response") {
        return None;
    }
    let outer = raw.response?;
    if outer.request_id.as_deref() != Some(REQUEST_ID) {
        return None;
    }
    let inner = outer.response.unwrap_or_default();
    let version = inner.claude_code_version.unwrap_or_default();
    let who = if version.is_empty() {
        "Claude Code".to_string()
    } else {
        format!("Claude Code {version}")
    };
    Some(
        match (outer.subtype.as_deref(), inner.workspace_trust_recorded) {
            (Some("success"), Some(true)) => Attested::Recorded { version },
            (Some("success"), Some(false)) => {
                Attested::Declined(format!("{who} recorded nothing for this folder"))
            }
            (Some("success"), None) => Attested::Declined(format!(
                "{who} answered with no workspace_trust_recorded: it does not take the attestation"
            )),
            _ => Attested::Declined(format!("{who} refused the request")),
        },
    )
}

/// The Claude Code build in atpkg's store, started headless ([`Vendor`]).
///
/// A start that recorded nothing is remembered in this process, so the window's relaunch pass,
/// which plans again every few seconds, does not start Claude Code again at each plan for the
/// same build, folder and home: Claude Code's own answer for [`DECLINE_STANDS`], a start that
/// gave none for [`FAILURE_STANDS`]. The twin's verb is one process per launch and remembers
/// nothing: a folder Claude Code declines there is asked once per launch, until its dialog's
/// answer records the key.
#[derive(Clone, Copy, Debug, Default)]
pub struct StoreVendor;

/// One ask of [`StoreVendor`]: the build, the folder, and the home variables it ran under.
#[cfg(unix)]
type Ask = (PathBuf, PathBuf, String);

/// The asks that recorded nothing, each with when it may be made again and why it stands.
#[cfg(unix)]
#[derive(Debug, Default)]
struct Standing(std::collections::BTreeMap<Ask, (std::time::Instant, String)>);

#[cfg(unix)]
impl Standing {
    /// Why `ask` is not made at `now`, where an earlier one still stands. Every entry whose
    /// time has come is dropped first.
    fn stands(&mut self, ask: &Ask, now: std::time::Instant) -> Option<String> {
        self.0.retain(|_, (until, _)| now < *until);
        self.0.get(ask).map(|(_, why)| why.clone())
    }

    /// `ask` recorded nothing for the reason `why`, and is not made again before `until`.
    fn note(&mut self, ask: Ask, until: std::time::Instant, why: String) {
        self.0.insert(ask, (until, why));
    }
}

/// What this process's [`StoreVendor`] asked and was not given.
#[cfg(unix)]
static STANDING: std::sync::Mutex<Standing> =
    std::sync::Mutex::new(Standing(std::collections::BTreeMap::new()));

impl StoreVendor {
    /// [`Vendor::attest`] with the clock read as `now`: nothing is started for an ask an
    /// earlier answer still stands for ([`Attested::Stands`]), and an answer that recorded
    /// nothing stands from `now` for [`DECLINE_STANDS`] (Claude Code's own) or
    /// [`FAILURE_STANDS`] (none given).
    #[cfg(unix)]
    fn attest_at(
        build: &Path,
        cwd: &Path,
        env: &[(String, String)],
        directory: &str,
        deadline: Duration,
        now: std::time::Instant,
    ) -> Attested {
        let home: String = env
            .iter()
            .filter(|(name, _)| {
                name == super::accounts::HOME_ENV || name == super::accounts::HOMEDIR_ENV
            })
            .map(|(name, value)| format!("{name}={value}\n"))
            .collect();
        let ask = (build.to_path_buf(), cwd.to_path_buf(), home);
        let standing = || {
            STANDING
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        };
        if let Some(why) = standing().stands(&ask, now) {
            return Attested::Stands(why);
        }
        let attested = start(build, cwd, env, directory, deadline);
        let stands = match &attested {
            Attested::Declined(why) => Some((DECLINE_STANDS, why)),
            Attested::Failed(why) => Some((FAILURE_STANDS, why)),
            Attested::Recorded { .. } | Attested::Stands(_) => None,
        };
        if let Some((held, why)) = stands {
            standing().note(
                ask,
                now + held,
                format!("not asked again within {} s of: {why}", held.as_secs()),
            );
        }
        attested
    }
}

impl Vendor for StoreVendor {
    #[cfg(unix)]
    fn attest(
        &self,
        build: &Path,
        cwd: &Path,
        env: &[(String, String)],
        directory: &str,
        deadline: Duration,
    ) -> Attested {
        Self::attest_at(
            build,
            cwd,
            env,
            directory,
            deadline,
            std::time::Instant::now(),
        )
    }

    #[cfg(not(unix))]
    fn attest(&self, _: &Path, _: &Path, _: &[(String, String)], _: &str, _: Duration) -> Attested {
        Attested::Failed("the headless start is Unix's".to_string())
    }
}

/// Start `build` headless in `cwd` under `env`, send it [`request_line`] and read its answer:
/// atpkg's warm arguments and variables over the launch's own environment and the build's shim
/// variables, a session of its own (no terminal to draw or prompt on), its output read through
/// pipes of its own, so nothing it prints reaches this process's stdout. Once the answer is
/// read its stdin is closed and it is left to exit by itself, as atpkg's warm start is: what it
/// was writing is its own to finish. Its group is killed only on a failure of this side's and
/// at `deadline`; an answer read before the deadline stands either way.
#[cfg(unix)]
fn start(
    build: &Path,
    cwd: &Path,
    env: &[(String, String)],
    directory: &str,
    deadline: Duration,
) -> Attested {
    use std::io::Write as _;
    let Some(warm) = atpkg::vendor_direct::spec("claude").and_then(|spec| spec.warm) else {
        return Attested::Failed("atpkg names no headless start for Claude Code".to_string());
    };
    let mut command = aterm_uds::spawnfd::command(build);
    command
        .args(warm.argv)
        .current_dir(cwd)
        .env_clear()
        .envs(env.iter().map(|(name, value)| (name, value)));
    let table =
        atpkg::vendor_direct::spec("claude").map(atpkg::vendor_direct::VendorSpec::shim_env);
    let beside = atpkg::shim_env::of_store_binary(build);
    for (name, value) in table
        .iter()
        .flat_map(atpkg::shim_env::ShimEnv::entries)
        .chain(beside.entries())
    {
        command.env(name, value);
    }
    for (name, value) in warm.env {
        command.env(name, value);
    }
    command.setsid(true);
    let (reader, mut writer) = match aterm_uds::pipe() {
        Ok(pipe) => pipe,
        Err(e) => return Attested::Failed(format!("no pipe for the request: {e}")),
    };
    // One short line: it fits the pipe, so it is written before there is a reader.
    if let Err(e) = writer.write_all(request_line(directory).as_bytes()) {
        return Attested::Failed(format!("the request could not be written: {e}"));
    }
    // The request's writer stays open until the answer is read: Claude Code exits when its
    // stdin closes. Dropped at the answer, it is the build's leave to finish and go.
    let writer = std::cell::RefCell::new(Some(writer));
    let answer = std::cell::RefCell::new(None::<Attested>);
    let mut read = Vec::new();
    let ended = aterm_uds::deadline::stream_with_stdin_until_stopped(
        &mut command,
        aterm_uds::spawnfd::Stdio::from(reader),
        Some(std::time::Instant::now() + deadline),
        |_| {},
        |from, chunk| {
            if from != aterm_uds::deadline::ChildOutput::Stdout || answer.borrow().is_some() {
                return Ok(());
            }
            read.extend_from_slice(chunk);
            while let Some(end) = read.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = read.drain(..=end).collect();
                if let Some(said) = answer_of(&String::from_utf8_lossy(&line)) {
                    *answer.borrow_mut() = Some(said);
                    break;
                }
            }
            if answer.borrow().is_none() && read.len() > ANSWER_MAX {
                *answer.borrow_mut() = Some(Attested::Failed(
                    "Claude Code printed more than an answer".to_string(),
                ));
            }
            if answer.borrow().is_some() {
                drop(writer.borrow_mut().take());
            }
            Ok(())
        },
        || false,
        None,
    );
    drop(writer.into_inner());
    let answered = answer.into_inner();
    match (answered, ended) {
        (Some(said), _) => said,
        (None, Ok(_)) => {
            // It ended without a line break after its answer, or without an answer.
            answer_of(&String::from_utf8_lossy(&read)).unwrap_or_else(|| {
                Attested::Failed("Claude Code ended without answering".to_string())
            })
        }
        (None, Err(e)) if e.kind() == std::io::ErrorKind::TimedOut => Attested::Failed(format!(
            "Claude Code did not answer within {} ms",
            deadline.as_millis()
        )),
        (None, Err(e)) => Attested::Failed(format!("Claude Code could not be started: {e}")),
    }
}

/// The session's approvals ledger and journal under the aterm state root ([`Book`]).
#[derive(Clone, Debug)]
pub struct DriveBook {
    /// The aterm state root (`<root>/drive/<sid>.jsonl`), where it could be located.
    pub state: Option<PathBuf>,
    /// The session.
    pub sid: String,
}

impl DriveBook {
    fn ledger(&self) -> Option<PathBuf> {
        crate::supervise::approvals::path_under(self.state.as_deref()?, Some(&self.sid))
    }

    fn journal(&self, ledger: &Path, t_ms: i64, line: &str) {
        let mut warn = std::io::sink();
        let journal = crate::supervise::approvals::journal_beside(ledger);
        let _ = crate::supervise::Journal::open(Some(&journal), Some(&self.sid), &mut warn)
            .record(t_ms, line, &mut warn);
    }
}

impl Book for DriveBook {
    fn recorded(&mut self, rule: &str, folder: &str, by: &str) {
        let Some(ledger) = self.ledger() else {
            return;
        };
        let t_ms = crate::supervise::journal::unix_ms();
        let _ = crate::supervise::approvals::book_launch_trust(
            &ledger,
            Some(&self.sid),
            t_ms,
            rule,
            folder,
            by,
            &mut std::io::sink(),
        );
        self.journal(
            &ledger,
            t_ms,
            &format!("HOST launch trust rule={rule} {folder}: recorded before launch by {by}"),
        );
    }

    fn refused(&mut self, folder: &str, why: &str) {
        let Some(ledger) = self.ledger() else {
            return;
        };
        self.journal(
            &ledger,
            crate::supervise::journal::unix_ms(),
            &format!(
                "HOST launch trust {folder}: not recorded before launch ({why}): its dialog is \
                 the rule's to answer"
            ),
        );
    }
}

/// What a session's window said of it ([`session_seen`]): its `meta` and `status` replies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seen {
    /// The `meta` reply: the session's own Autopilot word and the folder its starter trusted.
    pub meta: String,
    /// The `status` reply: the hold on the session and who supervises it.
    pub status: String,
}

/// What [`Seen`] says of the conditions a session's dialog is answered under ([`session_facts`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Facts {
    /// The session's own Autopilot word (`None`: unset).
    pub autopilot: Option<Autopilot>,
    /// The folder the session's starter trusted.
    pub trusted_cwd: Option<PathBuf>,
    /// A hold stands on the session (`status hold=` is anything but `0`: a person took it over,
    /// or a local or fleet halt): every key of the harness's is refused there, so its dialog is
    /// the person's.
    pub held: bool,
    /// A supervisor that is no window's own host claims the session (`status supervisor=`): its
    /// dialog is that supervisor's to answer, under a policy this does not read.
    pub claimed: bool,
}

/// [`Facts`] of a session's replies. `None` for a `meta` or `status` reply that is no `OK`, an
/// Autopilot word this build does not know, or a `status` that names no `hold=`: what cannot be
/// read is never read as not off, or as not held.
#[must_use]
pub fn session_facts(seen: &Seen) -> Option<Facts> {
    let line = seen.meta.lines().find(|line| crate::is_ok_reply(line))?;
    let field = |key: &str| {
        line.split_whitespace()
            .find_map(|word| word.strip_prefix(key))
            .map(aterm_codec::pct::decode)
    };
    let autopilot = match field("autopilot=").as_deref() {
        None | Some("-" | "") => None,
        Some(word) => Some(Autopilot::parse(word)?),
    };
    let trusted_cwd = field("trusted_cwd=")
        .filter(|folder| folder != "-" && !folder.is_empty())
        .map(PathBuf::from)
        .filter(|folder| folder.is_absolute());
    let held = crate::supervise::screen::status_field(&seen.status, "hold")? != "0";
    let claimed =
        crate::supervise::screen::status_field(&seen.status, "supervisor").is_some_and(|holder| {
            !super::upgrade_drive::is_host_holder(&aterm_codec::pct::decode(holder))
        });
    Some(Facts {
        autopilot,
        trusted_cwd,
        held,
        claimed,
    })
}

/// How long [`session_seen`] waits for each of the window's replies.
pub const SESSION_WAIT: Duration = Duration::from_secs(1);

/// Session `sid`'s `meta` and `status` replies, read from its window's control socket (`sock`,
/// else the one `sid` resolves to; `token` the window harness's own, on its own socket) over
/// one connection that can send read verbs alone, each exchange bounded by `wait`. `None` where
/// the window did not answer either.
#[must_use]
pub fn session_seen(
    sock: Option<&str>,
    token: Option<&super::upgrade_drive::HarnessToken>,
    sid: &str,
    wait: Duration,
) -> Option<Seen> {
    let sock = match sock {
        Some(sock) => sock.to_string(),
        None => aterm_ctl::resolve_sock_for(Some(sid)).ok()?,
    };
    let mut client = super::upgrade_drive::harness_client_within(&sock, token, wait)
        .ok()?
        .reads_only();
    let meta = client.request_line(&format!("@{sid} meta")).ok()?;
    let status = client.request_line(&format!("@{sid} status")).ok()?;
    Some(Seen { meta, status })
}

/// [`gate`] for a session under `config`, as its window says it stands ([`session_seen`]): no
/// hold on it, no supervisor but a window's own host, and its own Autopilot word and trusted
/// folder. An unanswered or unreadable read is [`Gate::Unread`]: the file alone cannot say the
/// session's own word is not off or that no hold stands, and a launch left unsettled only
/// leaves the dialog to its rule.
#[must_use]
pub fn session_gate(
    config: &SupervisorConfig,
    seen: Option<&Seen>,
    home: Option<&Path>,
    uid: u32,
    cwd: &Path,
) -> Gate {
    let Some(facts) = seen.and_then(session_facts) else {
        return Gate::Unread("the session's hold and Autopilot word could not be read");
    };
    if facts.held {
        return Gate::Refuses("a hold stands on the session");
    }
    if facts.claimed {
        return Gate::Refuses("another supervisor claims the session");
    }
    gate(
        &Policy {
            config,
            autopilot: facts.autopilot,
            trusted_cwd: facts.trusted_cwd.as_deref(),
            home,
            uid,
        },
        cwd,
    )
}

/// THE CLAUDE CODE BUILD OF ATPKG'S OWN STORE A LAUNCH OF `exe` RUNS, by its real path: the
/// build atpkg's managed twin execs where `exe` is that twin (the twin itself is never started
/// headless, since it puts flags of its own on a launch), else `exe`; and only where that build
/// lies in atpkg's own store of Claude Code builds ([`token_route::store_build_in`], the gate
/// the token route hands a login to a build by). `None` for anything else: the headless start
/// runs a program with arguments only a Claude Code build reads, in a folder nothing trusts
/// yet, so it is never a person's wrapper, another install or a test's stand-in.
#[must_use]
pub(super) fn store_build(exe: &Path) -> Option<PathBuf> {
    let build = if super::upgrade_drive::is_managed_twin_path(exe, token_route::PROGRAM) {
        super::upgrade_drive::twin_build(exe, token_route::PROGRAM)?
    } else {
        exe.to_path_buf()
    };
    token_route::store_build_in(&super::relaunch::token_builds()?, &build)
}

/// [`store_build`] of atpkg's managed twin of Claude Code: the build a launch through the twin
/// (the one that asks `aterm harness tap --launch`) runs. Unix's, as the step's caller there is.
#[cfg(unix)]
#[must_use]
pub(super) fn twin_store_build() -> Option<PathBuf> {
    store_build(&super::upgrade_drive::managed_agent_twin(
        token_route::PROGRAM,
    )?)
}

/// One relaunch line, as [`settle_relaunch`] reads it.
#[derive(Clone, Copy, Debug)]
pub struct Line<'a> {
    /// The launch's argv, `argv[0]` first.
    pub argv: &'a [String],
    /// The folder the line starts in.
    pub cwd: &'a str,
    /// The variables the line itself sets (`relaunch::required_env`): the Claude home, `HOME`
    /// and login it moves the session to.
    pub env: &'a [(&'static str, &'a str)],
    /// The program the line names.
    pub exe: &'a Path,
}

/// The session a relaunch line belongs to, and the machine around it ([`settle_relaunch`]).
pub struct Relaunch<'a> {
    /// The tab the line is typed in.
    pub tab: &'a str,
    /// `[harness]`.
    pub config: &'a SupervisorConfig,
    /// The environment the headless start is built from (this process's own): the line's
    /// variables replace their namesakes, and no AI variable but the line's is handed on, as
    /// the shell the line is typed in holds none.
    pub base_env: &'a [(String, String)],
    /// The user home.
    pub user_home: &'a Path,
    /// The account's home directory.
    pub account_home: Option<&'a Path>,
    /// The user.
    pub uid: u32,
    /// The aterm state root the session's ledger lies under.
    pub state: Option<&'a Path>,
    /// The build the line's program runs ([`store_build`]), where it could be read.
    pub build: Option<&'a Path>,
}

/// SETTLE THE FOLDER TRUST OF A RELAUNCH LINE ([`settle`]) under the Claude home, `HOME` and
/// folder the line itself names, before it is built: a line that moves a session to another
/// Claude home starts it in a folder that home may hold no trust record for, and the dialog
/// would stand over the conversation it resumes. `seen` reads the tab from its window
/// ([`session_seen`]).
pub fn settle_relaunch(
    at: &Relaunch<'_>,
    line: &Line<'_>,
    vendor: &dyn Vendor,
    seen: &dyn Fn(&str) -> Option<Seen>,
) -> Settled {
    let mut env: Vec<(String, String)> = at
        .base_env
        .iter()
        .filter(|(name, _)| {
            !aterm_types::domain::is_ai_env_var(name)
                && !line.env.iter().any(|(set, _)| set == name)
        })
        .cloned()
        .collect();
    env.extend(
        line.env
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string())),
    );
    let mut book = DriveBook {
        state: at.state.map(Path::to_path_buf),
        sid: at.tab.to_string(),
    };
    settle(
        &Launch {
            argv: line.argv,
            cwd: Path::new(line.cwd),
            env: &env,
            account_home: at.account_home,
            user_home: at.user_home,
            no_harness: false,
            no_terminal: false,
            build: at.build,
        },
        &|cwd| {
            session_gate(
                at.config,
                seen(at.tab).as_ref(),
                Some(at.user_home),
                at.uid,
                cwd,
            )
        },
        vendor,
        &mut book,
        DEADLINE,
    )
}

/// The `[harness]` file a live relaunch is settled under: the machine's `aterm.toml`.
#[cfg(not(test))]
fn policy_file() -> Option<PathBuf> {
    aterm_types::dirs::aterm_config_path()
}

#[cfg(test)]
thread_local! {
    /// A test's `[harness]` file, in place of the machine's ([`policy_file`]).
    pub(super) static POLICY_FILE: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// A test's `[harness]` file ([`POLICY_FILE`]), never the machine's: with none set, the
/// table's defaults.
#[cfg(test)]
fn policy_file() -> Option<PathBuf> {
    POLICY_FILE.with(|file| file.borrow().clone())
}

/// [`settle_relaunch`] ON THIS MACHINE, for the line a relaunch plan of tab `tab` is about to
/// build (`Kernel::settle_trust` of the live kernel): under the policy file and this process's
/// own environment, the build started by [`StoreVendor`] and the tab read from the drive's own
/// window. First, and before anything of the machine is read: a line whose program is no build
/// of atpkg's own store, and no twin of one, is left alone ([`Settled::NotStoreBuild`]).
pub(super) fn settle_relaunch_live(
    opts: &super::upgrade_drive::Opts,
    tab: &str,
    line: &Line<'_>,
) -> Settled {
    let build = store_build(line.exe);
    if build.is_none()
        && !super::upgrade_drive::is_managed_twin_path(line.exe, token_route::PROGRAM)
    {
        return Settled::NotStoreBuild;
    }
    let (config, _) = SupervisorConfig::from_path(policy_file().as_deref());
    let base_env: Vec<(String, String)> = std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
        .collect();
    #[cfg(unix)]
    let account_home = aterm_uds::account::home();
    #[cfg(not(unix))]
    let account_home: Option<PathBuf> = None;
    settle_relaunch(
        &Relaunch {
            tab,
            config: &config,
            base_env: &base_env,
            user_home: &opts.home,
            account_home: account_home.as_deref(),
            uid: super::disk::discover::own_uid(),
            state: opts.aterm_state.as_deref(),
            build: build.as_deref(),
        },
        line,
        &StoreVendor,
        &|tab| session_seen(opts.sock.as_deref(), opts.token.as_ref(), tab, SESSION_WAIT),
    )
}

#[cfg(test)]
#[path = "launch_trust_tests.rs"]
mod tests;
