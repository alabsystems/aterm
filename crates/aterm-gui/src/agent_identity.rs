// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Session identities: a session can carry its own agent identity, set at spawn.
//!
//! `identity` is a spawn-time, immutable session property (unlike the `meta set` fields). It
//! names `<state>/identities/<name>/` ([`aterm_types::dirs::identities_dir`]), and every agent
//! in [`aterm_primer::agent_homes`] gets its home variable pointed at its own subdirectory
//! there (`CLAUDE_CONFIG_DIR=<idir>/.claude`, `CODEX_HOME=<idir>/.codex`). The subdirs carry
//! the agents' own names, so [`ensure`] primes the identity dir unchanged: a worker under
//! `identity=worker` has the aterm skills, the manual pointer and its own login.
//!
//! aterm never opens a login: `agents=<prog>:present|absent` (the `identities` verb) is a
//! `read_dir` name listing. The macOS keychain is the one login channel outside the dir; aterm
//! never reads, names or deletes a keychain item it did not create. Claude Code names its item
//! `Claude Code-credentials`, plus `-<first 8 hex of sha256(CLAUDE_CONFIG_DIR)>` when that
//! variable is set, so an identity's login is a different item from the default tab's. That is
//! read from the vendor code: re-read it when the claude pin moves
//! (`docs/AGENT-EXPERIENCE-2026-08-26.md` §7.3).
//!
//! `spawn identity=<name>` (Owner-only) and the session picker's new-identity
//! row are the only create paths: the spawner makes the identity's folders
//! before the fork, once the spawn is admitted ([`check_for_spawn`] with
//! [`SpawnAdmit::Named`]), and primes them off the spawn's path. An inherited
//! identity's admission, the spawn seam and a cold restore never create an
//! identity (an inherited admission makes only a missing agent subdir, inside
//! an identity still there): a missing identity is refused, or on a cold
//! restore falls back to a default shell with one stderr line
//! ([`restorable`]). A downgrade to a build without the field keeps the
//! shell's env and drops the label (`sessions` says `identity=-`).
//!
//! A spawn holds the identity it is admitted under ([`Admission`]) from before
//! it sights the folder until it fails or the session that wears it drops, and
//! `identities forget` refuses an identity a spawn holds. A confirmed forget
//! holds it in turn from its checks to its rename ([`Forgetting`]), and no spawn
//! is admitted under it meanwhile. So no shell this process spawns starts, or
//! runs while its session lives, with agent homes in a folder a forget in this
//! process removed. A process that outlives its session is not covered: the
//! session gives its hold back as it drops, which only hangs the shell up and
//! leaves its reap to a background thread, so an agent still writing its exit
//! state, a child that ignores the hang-up, or a disowned job can name the
//! folder after a forget's `OK`. A shell adopted across an update holds the
//! identity its record labels from its adoption on ([`hold_adopted`]), unless
//! a forget was already taking it; one adopted from a build without the label
//! holds nothing.
//!
//! Names fold to lowercase at parse ([`parse_name`]), since case-insensitive
//! APFS would make `Worker` and `worker` one directory with two labels. Grammar
//! after folding: `[a-z0-9][a-z0-9._-]{0,63}`; `-` (the wire's absent mark) and
//! `forget` (the `identities` verb's sub-form word) are refused.

use std::io;
use std::path::{Path, PathBuf};

/// The longest name: a directory name, and one that fits an `ls` row.
pub(crate) const NAME_MAX: usize = 64;

/// Parse and fold a spawn-time identity name; the folded spelling is the
/// directory name. Refused: an empty name, `-` (the absent mark), `forget` (the
/// verb's sub-form word), a first character outside `[a-z0-9]`, any character
/// outside `[a-z0-9._-]`, and anything past [`NAME_MAX`] bytes, so a name is
/// never `..`, a path, or a shell word.
pub(crate) fn parse_name(raw: &str) -> Result<String, String> {
    let name = raw.to_ascii_lowercase();
    if name.is_empty() {
        return Err("identity: a name is required".to_string());
    }
    if name == "-" {
        return Err("identity: `-` is the absent mark, not a name".to_string());
    }
    if name == "forget" {
        return Err("identity: `forget` is the `identities` verb's word, not a name".to_string());
    }
    if name.len() > NAME_MAX {
        return Err(format!(
            "identity: a name is at most {NAME_MAX} characters, got {}",
            name.len()
        ));
    }
    let first = name.chars().next().unwrap_or('-');
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return Err(format!(
            "identity: a name starts with a letter or digit, got {name:?}"
        ));
    }
    if let Some(bad) = name
        .chars()
        .find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')))
    {
        return Err(format!(
            "identity: {bad:?} is not allowed in a name (letters, digits, `.`, `_`, `-`)"
        ));
    }
    Ok(name)
}

/// The directory a (parsed) name resolves to under this instance's state
/// root; `None` when no state root resolves at all.
#[cfg(test)]
pub(crate) fn dir_for(name: &str) -> Option<PathBuf> {
    aterm_types::dirs::identities_dir().map(|root| root.join(name))
}

/// Resolve `name` (parsed and folded here) to its identity directory. With
/// `create`, provision `<identities>/<name>/` and each agent subdir at 0700,
/// owned by us (a foreign owner is refused:
/// [`aterm_types::fs_restricted::ensure_private_dir`]), and prime it,
/// idempotently. Without `create`, only what the `identities` verb calls an
/// identity counts ([`identity_dir`]: a real directory, never a symlink);
/// anything else is `NotFound` and nothing is touched.
pub(crate) fn ensure(name: &str, create: bool) -> io::Result<PathBuf> {
    let root = answering_root()?;
    ensure_in(&root, name, create)
}

/// The identities folder ([`aterm_types::dirs::identities_dir`]) once its
/// volume answered ([`crate::spawn_folder::volume_answers`]). Every read of
/// the roster or of an identity starts here, on a thread someone waits on:
/// the spawner, ahead of every tab asked after this one; the event loop, for
/// the identity picker and a restored pane; a control lane, for the
/// `identities` verb. On a network volume that stopped answering each read
/// held its thread for the mount's whole timeout — the spawner's later tabs,
/// and for the picker every window, until the server came back (measured in a
/// macOS 27 guest, 2026-10-20) — where now it is refused after the folder
/// check's budget, and at once after that.
fn answering_root() -> io::Result<PathBuf> {
    answering(
        aterm_types::dirs::identities_dir(),
        crate::spawn_folder::volume_answers,
    )
}

/// This instance's identities dir ([`aterm_types::dirs::identities_dir`]), or
/// why there is none, with no look at its volume: for [`hold_adopted`], which
/// reads nothing from disk.
fn identities_root() -> io::Result<PathBuf> {
    aterm_types::dirs::identities_dir().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "no state root resolves (HOME is unset)",
        )
    })
}

/// [`answering_root`], PURE over the root and its volume's answer: a root
/// whose volume did not answer is refused by name (`TimedOut`) before
/// anything under it is looked at.
fn answering(root: Option<PathBuf>, answers: impl FnOnce(&Path) -> bool) -> io::Result<PathBuf> {
    let root = root.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "no state root resolves (HOME is unset)",
        )
    })?;
    if answers(&root) {
        Ok(root)
    } else {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("{} did not answer", root.display()),
        ))
    }
}

/// [`ensure`] with its root injected: `root` is the identities dir.
pub(crate) fn ensure_in(root: &Path, name: &str, create: bool) -> io::Result<PathBuf> {
    let name = parse_name(name).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    if !create {
        return identity_dir(root, &name).ok_or_else(|| no_such_identity(&name));
    }
    let (dir, _) = make_dirs_in(root, &name)?;
    prime_dir(&name, &dir);
    Ok(dir)
}

/// The refusal for an identity that is not there, or that went while an
/// admission readied it: `no such identity <name>`, the `identities` verb's
/// words.
fn no_such_identity(name: &str) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, format!("no such identity {name}"))
}

/// Write the primer's files into the identity `name` at `dir`, fail-soft per
/// agent (one warning each): exactly the agents whose subdirs exist get their
/// context file and skills. It makes no folder at or above an agent subdir, so
/// never `dir` or the subdir itself. Below an agent subdir that is there it
/// makes each missing folder (`.claude/skills/`, `.claude/skills/<skill>/`,
/// `.codex/prompts/`) one level at a time under a parent already there
/// ([`aterm_primer::auto_prime_identity`]), so once an `identities forget`'s
/// rename lands every later create fails, the identity stays forgotten, and its
/// rows say why nothing more was written. `auto_prime_identity`, not
/// `auto_prime`, which follows the human's `$XDG_CONFIG_HOME` for the
/// `.config/` row and would write into the human's tree. `true` when no agent's
/// row failed.
fn prime_dir(name: &str, dir: &Path) -> bool {
    let pass = aterm_primer::auto_prime_identity(dir);
    let mut clean = true;
    for (agent, err) in pass.errors() {
        aterm_log::warn!("identity {name}: primer: {agent}: {err}");
        clean = false;
    }
    clean
}

/// The folders half of [`ensure_in`] with `create`: `<root>/<name>/` and each
/// agent subdir at 0700, owned by us ([`make_identity_dir`], then
/// [`harden_identity`]), and whether this call made `<root>/<name>/`. `name` is
/// already parsed.
fn make_dirs_in(root: &Path, name: &str) -> io::Result<(PathBuf, bool)> {
    let made = make_identity_dir(root, name)?;
    let dir = identity_dir(root, name).ok_or_else(|| no_such_identity(name))?;
    harden_identity(root, &dir, name)?;
    Ok((dir, made))
}

/// Make `<root>/<name>/` for a named admission, and `root` first when it is
/// missing (0700, owned by us): `Ok(true)` iff this call's own `create_dir`
/// made the identity's folder, `Ok(false)` when a directory, or nothing any
/// more, was there. The answer is the create's, never a look taken before it:
/// an `identities forget` that lands between such a look and the create would
/// hide a folder this call made, and the per-name throttle would skip its first
/// prime ([`reprime_due`]). A file or symlink in the folder's place keeps the
/// create's error. `name` is already parsed.
fn make_identity_dir(root: &Path, name: &str) -> io::Result<bool> {
    aterm_types::fs_restricted::ensure_private_dir(root)?;
    let dir = root.join(name);
    match make_private_dir(&dir) {
        Ok(()) => Ok(true),
        Err(e)
            if e.kind() == io::ErrorKind::AlreadyExists
                && !std::fs::symlink_metadata(&dir).is_ok_and(|m| !m.is_dir()) =>
        {
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

/// Make the one folder `dir`, its parent already there, born 0700 on unix:
/// `create_dir`, never `create_dir_all`, so a parent taken meanwhile is
/// `NotFound` and never made again.
fn make_private_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(dir)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir(dir)
    }
}

/// Make the identity `name` at `dir`, sighted under `root`, safe to hand a
/// child, making no folder at or above `dir`. `root` and `dir` are hardened
/// where they are ([`crate::control_auth::harden_private_dir`]: a missing one
/// is never created). A missing agent subdir is made one level below `dir`,
/// which must still be there ([`make_private_dir`]): that is an identity made
/// before its agent's row joined [`aterm_primer::agent_homes`], which a row
/// does once its variable is measured (the table's `var` column), so a
/// missing subdir is made rather than refused; an agent subdir that is a
/// symlink to nothing is refused as what it is, never as a missing identity
/// (`harden_private_dir` says `NotADirectory`). A dir already private costs
/// one `stat`. This process's `identities forget` cannot take the folder from
/// under an admission, which holds it ([`Admission`]); a folder that goes
/// anyway (removed by hand, or by another process's forget) anywhere in here
/// leaves the spawn refused as [`no_such_identity`], and nothing comes back.
/// Until 2026-10-17 this was `ensure_private_dir` throughout, whose
/// `create_dir_all` made an inherited identity again behind a forget's `OK`.
fn harden_identity(root: &Path, dir: &Path, name: &str) -> io::Result<()> {
    let gone = |e: io::Error| {
        if e.kind() == io::ErrorKind::NotFound {
            no_such_identity(name)
        } else {
            e
        }
    };
    crate::control_auth::harden_private_dir(root).map_err(gone)?;
    crate::control_auth::harden_private_dir(dir).map_err(gone)?;
    for row in aterm_primer::agent_homes() {
        let sub = dir.join(row.sub);
        match crate::control_auth::harden_private_dir(&sub) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if let Err(e) = make_private_dir(&sub)
                    && e.kind() != io::ErrorKind::AlreadyExists
                {
                    return Err(gone(e));
                }
                crate::control_auth::harden_private_dir(&sub).map_err(gone)?;
            }
            hardened => hardened?,
        }
    }
    Ok(())
}

/// Where a spawn's identity came from, which decides whether its admission may
/// make it ([`check_for_spawn`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpawnAdmit {
    /// Passed down from the aimed session (an aimed spawn's default): it exists
    /// by construction, and a missing one is refused, never made.
    Inherited,
    /// Named by the request (`spawn identity=<name>`) or the picker's
    /// new-identity row: its folders are made first if they are not there.
    Named,
}

/// The spawner's half of a spawn under `name`, before its fork
/// (`tab_spawn::IdentityPrep`, never in an event-loop turn: the identities
/// folder can sit on a slow volume), once the main thread has admitted the
/// spawn itself (its window, its `of=`), so a refused spawn makes nothing. A
/// [`SpawnAdmit::Named`] identity's folder is made here; an inherited one
/// must exist, and nothing at or above its folder is ever made for it.
/// Either way its required directories must be private before a child
/// receives them ([`harden_identity`]); the gate uses a fresh metadata fast
/// path when no hardening is needed. The primer's files are written off this
/// thread ([`reprime_detached`], throttled per name, but always for an
/// identity this admission made): the shell's env names only the folders, and
/// a primer write that is lost or late is made again by the next spawn's
/// prime, so the spawn's reply waits for no file write. `Ok` is the spawn's
/// [`Admission`], which the spawner keeps until the session has its own
/// ([`admit_existing`]); an `Err` holds nothing.
pub(crate) fn check_for_spawn(name: &str, admit: SpawnAdmit) -> io::Result<Admission> {
    let root = answering_root()?;
    check_for_spawn_in(&root, name, admit, reprime_detached)
}

/// The spawn admission with its state root and background-primer handoff
/// injected. Tests observe the handoff without racing a detached writer. The
/// handoff hears whether this admission made the identity's folder (`made`,
/// [`make_identity_dir`]'s own answer): a new one, or one remade after
/// `identities forget`, has no primer files yet, so its prime is never skipped
/// by the per-name throttle.
pub(crate) fn check_for_spawn_in(
    root: &Path,
    name: &str,
    admit: SpawnAdmit,
    reprime: impl FnOnce(&Path, bool),
) -> io::Result<Admission> {
    check_for_spawn_at(root, name, admit, |_| {}, reprime)
}

/// [`check_for_spawn_in`] with its moment between sighting the identity's
/// folder and hardening it injected: `sighted` hears the folder. Tests take
/// the folder away there.
fn check_for_spawn_at(
    root: &Path,
    name: &str,
    admit: SpawnAdmit,
    sighted: impl FnOnce(&Path),
    reprime: impl FnOnce(&Path, bool),
) -> io::Result<Admission> {
    let name = parse_name(name).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    // Admitted before anything is sighted: a forget holding the identity now
    // is waited out, and one that comes later is refused while this stands, so
    // the folder sighted below is the one the child's agent homes name.
    let admission = Admission::begin(&root.join(&name));
    let made = match admit {
        SpawnAdmit::Named => make_identity_dir(root, &name)?,
        SpawnAdmit::Inherited => false,
    };
    let dir = identity_dir(root, &name).ok_or_else(|| no_such_identity(&name))?;
    sighted(&dir);
    // Directory safety is an admission condition, not a best-effort primer
    // outcome. Reuse the control dir's fresh-stat fast path and hardening gate:
    // valid private directories incur no writes, while damage must be repaired
    // or refused before the background handoff and the child's spawn.
    harden_identity(root, &dir, &name)?;
    reprime(&dir, made);
    Ok(admission)
}

/// The shell's own check of the identity `name` it is spawned under
/// ([`crate::spawn::spawn_session`]), as the [`Admission`] its session keeps:
/// `name` must be an identity ([`identity_dir`]) that no forget is taking.
/// It never waits, since a restore spawns in an event-loop turn: a forget
/// holding the identity is on its way to removing it, so that is
/// [`no_such_identity`] too. A spawner's admission ([`check_for_spawn`]) held
/// across the spawn keeps every forget out until this one stands.
pub(crate) fn admit_existing(name: &str) -> io::Result<Admission> {
    admit_existing_in(&answering_root()?, name)
}

/// [`admit_existing`] with its root injected.
fn admit_existing_in(root: &Path, name: &str) -> io::Result<Admission> {
    let name = parse_name(name).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let admission = Admission::begin_unless_forgotten(&root.join(&name))
        .ok_or_else(|| no_such_identity(&name))?;
    identity_dir(root, &name).ok_or_else(|| no_such_identity(&name))?;
    Ok(admission)
}

/// The [`Admission`] an adopted shell's session keeps under the identity
/// `name` its record labels ([`crate::spawn::spawn_session`] on adoption). The
/// process that admitted the shell may have exited with a committed update,
/// and the registry counts the label only once the tab is installed, so this
/// holds the identity from the adoption on. It never waits and never fails
/// the adoption, whose env is what it was whatever is on disk: `None` when no
/// state root resolves, the label is no name, or a forget holds the identity,
/// which is then on its way out. Nothing is read from disk.
pub(crate) fn hold_adopted(name: &str) -> Option<Admission> {
    hold_adopted_in(&identities_root().ok()?, name)
}

/// [`hold_adopted`] with its root injected.
fn hold_adopted_in(root: &Path, name: &str) -> Option<Admission> {
    let name = parse_name(name).ok()?;
    Admission::begin_unless_forgotten(&root.join(name))
}

/// What holds one identity's folder now ([`HOLDS`]): the [`Admission`]s under
/// it that have not dropped (`spawns`; a spawn holds two while its shell's own
/// check stands, its spawner's and its shell's, and a session still dropping
/// holds its own), and the confirmed forgets between their checks and their
/// rename ([`Forgetting`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Holds {
    spawns: usize,
    forgets: usize,
    /// The admissions waiting a forget out now ([`Admission::begin`]), counted
    /// in tests only: a test holds its forget until one waits
    /// ([`ADMISSION_WAITS`]).
    #[cfg(test)]
    waiting: usize,
}

/// Every identity folder something holds, by its path (`<root>/<name>`, so two
/// roots never share an entry).
type HoldsByDir = std::collections::BTreeMap<PathBuf, Holds>;

/// [`HoldsByDir`] for the process. A leaf lock: held only to read or change
/// the counts, never across a file operation or another lock.
static HOLDS: std::sync::Mutex<HoldsByDir> = std::sync::Mutex::new(HoldsByDir::new());

/// Signalled as a forget's hold ends, for the admissions waiting it out
/// ([`Admission::begin`]).
static FORGET_ENDED: std::sync::Condvar = std::sync::Condvar::new();

/// Signalled, in tests, as an admission starts waiting a forget out
/// ([`Holds::waiting`]).
#[cfg(test)]
static ADMISSION_WAITS: std::sync::Condvar = std::sync::Condvar::new();

fn identity_holds() -> std::sync::MutexGuard<'static, HoldsByDir> {
    HOLDS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Whether a forget holds the identity at `dir`.
fn forgotten(holds: &HoldsByDir, dir: &Path) -> bool {
    holds.get(dir).is_some_and(|held| held.forgets > 0)
}

/// Give back one of `dir`'s holds (`give` takes it off the counts), dropping
/// the entry once nothing holds the folder.
fn release(dir: &Path, give: impl FnOnce(&mut Holds)) {
    let mut holds = identity_holds();
    if let Some(held) = holds.get_mut(dir) {
        give(held);
        if *held == Holds::default() {
            holds.remove(dir);
        }
    }
}

/// A spawn's admission under the identity at its folder, from before the spawn
/// sights the folder until this drops: with the spawn when it fails, or with
/// the session that wears it (`spawn_session` keeps the shell's own,
/// [`admit_existing`], or an adopted shell's, [`hold_adopted`], on the
/// `Session`). While one stands, a confirmed `identities forget` of the
/// identity is refused as in use (`spawning=<k>`, `k` the admissions
/// standing), so no forget in this process takes the folder from the spawn's
/// admission until it fails or its session drops. The session drops before
/// its processes are gone (its shell is hung up and reaped in the background),
/// and a process that outlives it is held by nothing. The registry counts a
/// session as a user only once the main thread installs its tab; this counts
/// the spawn from its admission, through the spawner's readying and fork,
/// until the session is gone.
#[derive(Debug)]
#[must_use = "an admission holds its identity only while it is kept"]
pub(crate) struct Admission {
    dir: PathBuf,
}

impl Admission {
    /// Admit a spawn under the identity at `dir` (`<root>/<name>`), before the
    /// folder is sighted. A forget holding it is waited out, so the spawn then
    /// sights the folder as that forget's rename left it. The spawner's: the
    /// wait lasts as long as that one rename, and the forget holding it waits
    /// on nothing else. Only when no spawner thread could start does a turn
    /// run this, with the rest of the spawn's folder work.
    fn begin(dir: &Path) -> Self {
        let mut holds = identity_holds();
        if forgotten(&holds, dir) {
            // Counted under the lock the wait gives up, so a test that sees
            // the count knows this admission is parked in the wait.
            #[cfg(test)]
            {
                holds.entry(dir.to_path_buf()).or_default().waiting += 1;
                ADMISSION_WAITS.notify_all();
            }
            holds = FORGET_ENDED
                .wait_while(holds, |holds| forgotten(holds, dir))
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            #[cfg(test)]
            if let Some(held) = holds.get_mut(dir) {
                held.waiting -= 1;
            }
        }
        Self::count(holds, dir)
    }

    /// [`Self::begin`] that never waits: `None` while a forget holds the
    /// identity, which is then on its way out.
    fn begin_unless_forgotten(dir: &Path) -> Option<Self> {
        let holds = identity_holds();
        if forgotten(&holds, dir) {
            return None;
        }
        Some(Self::count(holds, dir))
    }

    fn count(mut holds: std::sync::MutexGuard<'_, HoldsByDir>, dir: &Path) -> Self {
        holds.entry(dir.to_path_buf()).or_default().spawns += 1;
        Self {
            dir: dir.to_path_buf(),
        }
    }

    /// The identity's folder, the one the shell's agent homes are named under
    /// ([`env`]).
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }
}

impl Drop for Admission {
    fn drop(&mut self) {
        release(&self.dir, |held| held.spawns -= 1);
    }
}

/// A confirmed `identities forget`'s hold on the identity at its folder, from
/// its checks until its rename returns ([`forget_reply`]): while it stands no
/// spawn is admitted under the identity ([`Admission`]), so none sights the
/// folder the rename is taking. Dropped, it wakes the admissions waiting it
/// out.
struct Forgetting {
    dir: PathBuf,
}

impl Forgetting {
    /// Hold the identity at `dir` for a forget, or `Err(k)` while `k`
    /// admissions under it stand ([`Holds`]), and then nothing is held.
    fn begin(dir: &Path) -> Result<Self, usize> {
        let mut holds = identity_holds();
        let held = holds.entry(dir.to_path_buf()).or_default();
        if held.spawns > 0 {
            return Err(held.spawns);
        }
        held.forgets += 1;
        Ok(Self {
            dir: dir.to_path_buf(),
        })
    }
}

impl Drop for Forgetting {
    fn drop(&mut self) {
        release(&self.dir, |held| held.forgets -= 1);
        FORGET_ENDED.notify_all();
    }
}

/// How often one process re-primes the same inherited identity, at most: a
/// fleet's burst of aimed spawns costs one pass, and the primer's text is this
/// build's, so a later pass only repairs a file edited or removed since. The
/// human home's throttle (`spawn::AGENT_PRIME_INTERVAL`) has the same width.
const REPRIME_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// When each identity was last re-primed in this process, by name.
static REPRIMED: std::sync::Mutex<std::collections::BTreeMap<String, std::time::Instant>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Whether `name` is due a re-prime at `now`, recording it when it is: one
/// whose folder the spawn's admission just `made` (no primer file exists
/// there, whatever this process primed under the name before), one never
/// re-primed, or one not re-primed for [`REPRIME_INTERVAL`]. A clock that
/// stepped back is not due.
fn reprime_due(
    last: &mut std::collections::BTreeMap<String, std::time::Instant>,
    name: &str,
    now: std::time::Instant,
    made: bool,
) -> bool {
    let due = made
        || last
            .get(name)
            .is_none_or(|at| now.saturating_duration_since(*at) >= REPRIME_INTERVAL);
    if due {
        last.insert(name.to_string(), now);
    }
    due
}

/// The re-prime passes running now ([`reprime_detached`], and the launch's
/// [`reprime_roster`] while it writes): a spawn's counted from the admission
/// that hands it off, before its spawn replies, until its thread is done
/// writing. `metrics` prints it as `identity_primes_running=`, so a reader
/// that sees 0 after a spawn's reply knows no primer is writing into an
/// identity's folder.
static PRIMES_RUNNING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// One pass's place in [`PRIMES_RUNNING`], given back as it drops: at the end
/// of its thread, or with the thread's closure when the thread never started.
struct PrimeRunning;

impl PrimeRunning {
    fn begin() -> Self {
        PRIMES_RUNNING.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Self
    }
}

impl Drop for PrimeRunning {
    fn drop(&mut self) {
        PRIMES_RUNNING.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

/// The summary's primer field, `identity_primes_running` ([`PRIMES_RUNNING`]),
/// through the one emitter both forms read.
pub(crate) fn emit_metrics(s: &mut dyn crate::metrics::wire::FieldSink) {
    s.count(
        "identity_primes_running",
        PRIMES_RUNNING.load(std::sync::atomic::Ordering::Acquire) as u64,
    );
}

/// Prime the existing identity at `dir` again, the primer's files only
/// ([`reprime_existing`]), on a detached `aterm-agent-prime` thread, throttled
/// per name. The pass is counted in [`PRIMES_RUNNING`] from this call until its
/// thread ends. An inherited spawn ran this on the event loop every time until
/// 2026-10-15, and a named one on the control thread, before its spawn was even
/// admitted, until 2026-10-17. The root is taken from `dir`, resolved by the
/// caller, so the pass writes where the check looked. It makes no folder at or
/// above an agent dir: the admission made or hardened the identity's folder
/// and its agent dirs before this handoff, and the pass writes only below
/// agent dirs that are there. Below one it makes each missing folder one level
/// at a time under a parent already there ([`prime_dir`]), so once an
/// `identities forget`'s rename lands, every later create fails: the identity
/// stays forgotten wherever the rename lands in the pass, since the verb's one
/// rename takes the whole tree from every path the pass holds
/// ([`forget_reply`]). Until 2026-10-17 the pass made the folders again first,
/// and a forget that won the race came back behind its `OK`.
fn reprime_detached(dir: &Path, made: bool) {
    let (Some(root), Some(name)) = (dir.parent(), dir.file_name().and_then(|n| n.to_str())) else {
        return;
    };
    {
        let mut last = REPRIMED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !reprime_due(&mut last, name, std::time::Instant::now(), made) {
            return;
        }
    }
    let (root, name) = (root.to_path_buf(), name.to_string());
    let running = PrimeRunning::begin();
    let spawned = std::thread::Builder::new()
        .name("aterm-agent-prime".to_string())
        .spawn(move || {
            let _running = running;
            reprime_existing(&root, &name);
        });
    if let Err(e) = spawned {
        aterm_log::warn!("identity primer: could not start the re-prime thread: {e}");
    }
}

/// The detached re-prime's pass ([`reprime_detached`]): `name` under `root`,
/// while it is still an identity, gets the primer's files ([`prime_dir`]); a
/// forgotten one gets nothing. `false` only when a row of its pass failed.
fn reprime_existing(root: &Path, name: &str) -> bool {
    identity_dir(root, name).is_none_or(|dir| prime_dir(name, &dir))
}

/// Prime every identity on disk again, the primer's files only, on the
/// calling thread: the window launch's pass (`spawn::PrimeCue::Launch`) at
/// the first launch of each build, already detached and gated. A tab under an
/// identity reads its agent's primer from the identity's folder, which only a
/// spawner's admission under it primes ([`check_for_spawn`]). A shell a
/// seamless update adopts ([`hold_adopted`]), or one a cold restore forks
/// there ([`restorable`], then the shell's own [`admit_existing`]), holds the
/// identity but primes nothing there, so without this its agent, and one the
/// harness relaunches there with `--resume`, read the block the build before
/// wrote. Counted in [`PRIMES_RUNNING`] while it writes. A forgotten identity
/// gets nothing ([`reprime_existing`]), and a missing root is an empty roster.
/// `true` when every identity on the roster was primed whole: `false` when a
/// row failed, the root could not be read, or its volume did not answer, so
/// the launch records no build and the next one tries again
/// (`spawn::prime_identities_once_per_build`).
pub(crate) fn reprime_roster() -> bool {
    answering_root().is_ok_and(|root| reprime_roster_in(&root))
}

/// [`reprime_roster`] with its root injected: `root` is the identities dir.
/// Every identity is primed, a failed one included, before it answers (`&=`
/// does not short-circuit).
pub(crate) fn reprime_roster_in(root: &Path) -> bool {
    let _running = PrimeRunning::begin();
    let Ok(names) = list_names(root) else {
        return false;
    };
    let mut whole = true;
    for name in &names {
        whole &= reprime_existing(root, name);
    }
    whole
}

/// The env additions for a session under the identity at `dir`: one pair per
/// table row, `<VAR>=<dir>/<sub>`. Applied by `build_child_env` after the deny
/// pass, so these are the only agent-home variables the child sees.
pub(crate) fn env(dir: &Path) -> Vec<(String, String)> {
    aterm_primer::agent_homes()
        .map(|row| (row.var.to_string(), dir.join(row.sub).display().to_string()))
        .collect()
}

/// The restore rule: a cold restore of a leaf naming `name` re-injects the
/// identity's env only when the identity still exists (`create = false`);
/// otherwise a default shell, with one stderr line. Restore never creates.
pub(crate) fn restorable(name: &str) -> Option<String> {
    match ensure(name, false) {
        Ok(_) => Some(name.to_string()),
        Err(e) => {
            crate::logging::stderr_line!(
                "aterm-gui: session restore: identity {name}: {e}; the restored tab starts a \
                 default shell"
            );
            None
        }
    }
}

/// [`restorable`] without the stderr line, for the second read of the same leaf
/// on a cold restore (`App::restore_terminal_leaf` checking that the bootstrap
/// shell wears what the pane names, after `main_entry` already reported it).
pub(crate) fn existing(name: &str) -> Option<String> {
    ensure(name, false).ok().map(|_| name.to_string())
}

/// The `identities` verb's usage line — the whole grammar in one row.
const IDENTITIES_USAGE: &str = "ERR usage: identities [<name>|forget <name> [confirm=<name>]]\n";

/// `identities [<name>|forget <name> [confirm=<name>]]` -> the on-disk roster of
/// agent identities under [`aterm_types::dirs::identities_dir`], which live
/// sessions carry each, and which agents have files there. Owner-only like
/// `sessions`. Lines-framed (`OK <n>` + n lines, or one `ERR` line), except
/// `forget`, whose replies are one status line (`control_verbs::framing_of`
/// flips that sub-form to `Status`).
///
/// | form | reply |
/// |---|---|
/// | `identities` | `OK <n>` then `<name> dir=<pct> sessions=<n> agents=claude:present,codex:absent` per identity, by name |
/// | `identities <name>` | `OK <1+agents>`: that row, then `agent=<prog> var=<VAR> home=<pct> files=present\|absent` per agent |
/// | `identities forget <name>` | `ERR confirm: identities forget <name> confirm=<name> removes <pct>` — nothing touched |
/// | `… confirm=<name>`, a live session carries it | `ERR identity in use sessions=<sid>,…` — nothing touched |
/// | `… confirm=<name>`, `k` admissions under it stand ([`Admission`]) | `ERR identity in use spawning=<k>` — nothing touched |
/// | `… confirm=<name>` | `OK removed=<pct> left=keychain` |
/// | `… confirm=<name>`, the tombstone would not go | `OK removed=<pct> left=keychain tombstone=<pct> why=<pct>` |
/// | unknown name | `ERR no such identity <name>` |
/// | anything else | `ERR usage: identities [<name>\|forget <name> [confirm=<name>]]` |
///
/// `present` means the agent's subdirectory has at least one entry (a name
/// listing; nothing is opened). `forget` removes the tree and the agents' file
/// credentials with it; a keychain login stays (`left=keychain`, always), so
/// sign out in the agent first. The live count is the registry's: a shell
/// adopted from a build without the label is not counted. A spawn admitted
/// under the identity holds it from its admission until it fails or its session
/// drops ([`Admission`]), and an adopted shell with the label from its adoption
/// ([`hold_adopted`]), so a session the registry does not list yet (the spawner
/// readying the folder and forking, the tab not yet installed) refuses the
/// forget as `spawning=<k>`. `k` counts the admissions standing, not spawns: a
/// spawn holds two while its shell's own check stands, and a session still
/// dropping, or installed after the registry was read, counts too. A process
/// that outlives its session (an agent still exiting, a disowned job) holds
/// nothing. A confirmed forget holds the identity in turn from its checks to
/// its rename ([`Forgetting`]): a spawn arriving meanwhile is admitted after
/// the rename (a named one makes a new, empty identity) or, from a check that
/// cannot wait, refused as `no such identity`. The removal is one rename to a
/// tombstone, then the tombstone's removal ([`forget_reply`]), so a primer pass
/// still writing under the old name makes nothing. A tombstone that will not go
/// is named in the reply; it is no identity to any reader, and the next forget
/// in its aterm, or once that aterm has exited any aterm's next forget or
/// launch ([`sweep_abandoned_tombstones`]), sweeps it. A name the grammar
/// refuses is usage and is never joined to a path. Only real directories are
/// identities; a symlink or file in the root is ignored.
pub(crate) fn cmd_identities(store: &crate::session_store::Store, rest: &str) -> String {
    // The live users, from the registry snapshot: `(identity, sid)` in local-id
    // order. Cloned out under a short read guard, never held across the
    // directory walk.
    let live: Vec<(String, String)> = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        g.snapshot()
            .into_iter()
            .filter_map(|h| {
                h.identity
                    .as_deref()
                    .map(|name| (name.to_string(), h.sid.as_str().to_string()))
            })
            .collect()
    };
    match answering_root() {
        Ok(root) => identities_reply(&root, &live, rest),
        Err(error) => format!("ERR identities: {error}\n"),
    }
}

/// The pure half of [`cmd_identities`]: `root` is the identities dir, `live`
/// the registry's `(identity, sid)` pairs in listing order.
pub(crate) fn identities_reply(root: &Path, live: &[(String, String)], rest: &str) -> String {
    let toks: Vec<&str> = rest.split_whitespace().collect();
    match toks.as_slice() {
        [] => list_reply(root, live),
        ["forget", name] => forget_reply(root, live, name, None),
        ["forget", name, confirm] => match confirm.strip_prefix("confirm=") {
            Some(word) => forget_reply(root, live, name, Some(word)),
            None => IDENTITIES_USAGE.to_string(),
        },
        [name] if *name != "forget" && !name.contains('=') => one_reply(root, live, name),
        _ => IDENTITIES_USAGE.to_string(),
    }
}

/// The directory `name` (already folded) names under `root`, when it is an
/// identity: a real directory, never a symlink.
fn identity_dir(root: &Path, name: &str) -> Option<PathBuf> {
    let dir = root.join(name);
    std::fs::symlink_metadata(&dir)
        .ok()
        .filter(std::fs::Metadata::is_dir)
        .map(|_| dir)
}

/// Every identity on disk, by name — the `identities` verb's roster, for the
/// identity picker. A missing or unreadable root is an empty roster, and so is
/// one whose volume did not answer ([`answering_root`]).
pub(crate) fn roster() -> Vec<String> {
    answering_root()
        .ok()
        .and_then(|root| list_names(&root).ok())
        .unwrap_or_default()
}

/// Every identity under `root`, by name: entries whose name is its own folded
/// spelling and which are real directories. A missing root is an empty roster.
fn list_names(root: &Path) -> io::Result<Vec<String>> {
    let rd = match std::fs::read_dir(root) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut names = Vec::new();
    for entry in rd {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if parse_name(&name).ok().as_deref() != Some(name.as_str()) {
            continue;
        }
        if entry.file_type()?.is_dir() {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

fn presence_word(present: bool) -> &'static str {
    if present { "present" } else { "absent" }
}

/// The agents' files under `dir`, per table row: `present` iff the subdirectory is non-empty.
fn agent_presence(dir: &Path) -> Vec<(aterm_primer::AgentHome, bool)> {
    aterm_primer::agent_homes()
        .map(|row| {
            let present = dir_non_empty(&dir.join(row.sub));
            (row, present)
        })
        .collect()
}

/// At least one directory entry — the listing is read, no entry is opened.
fn dir_non_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .ok()
        .is_some_and(|mut rd| rd.next().is_some())
}

fn pct_path(path: &Path) -> String {
    aterm_control::wire::pct_encode(&path.display().to_string())
}

/// One roster line: `<name> dir=<pct> sessions=<n> agents=<prog>:<word>,…`.
fn identity_row(root: &Path, live: &[(String, String)], name: &str) -> String {
    let dir = root.join(name);
    let sessions = live.iter().filter(|(i, _)| i == name).count();
    let agents: Vec<String> = agent_presence(&dir)
        .into_iter()
        .map(|(row, present)| format!("{}:{}", row.agent, presence_word(present)))
        .collect();
    let agents = if agents.is_empty() {
        "-".to_string()
    } else {
        agents.join(",")
    };
    format!(
        "{name} dir={} sessions={sessions} agents={agents}",
        pct_path(&dir)
    )
}

fn list_reply(root: &Path, live: &[(String, String)]) -> String {
    let names = match list_names(root) {
        Ok(names) => names,
        Err(e) => return format!("ERR identities: {}: {e}\n", root.display()),
    };
    let mut out = format!("OK {}\n", names.len());
    for name in &names {
        out.push_str(&identity_row(root, live, name));
        out.push('\n');
    }
    out
}

fn one_reply(root: &Path, live: &[(String, String)], raw: &str) -> String {
    let Ok(name) = parse_name(raw) else {
        return IDENTITIES_USAGE.to_string();
    };
    let Some(dir) = identity_dir(root, &name) else {
        return format!("ERR no such identity {name}\n");
    };
    let agents = agent_presence(&dir);
    let mut out = format!("OK {}\n", 1 + agents.len());
    out.push_str(&identity_row(root, live, &name));
    out.push('\n');
    for (row, present) in agents {
        out.push_str(&format!(
            "agent={} var={} home={} files={}\n",
            row.agent,
            row.var,
            pct_path(&dir.join(row.sub)),
            presence_word(present)
        ));
    }
    out
}

/// What a forgotten identity's tombstone is called before its pid and count
/// ([`tombstone_for`]): a dot-name, which [`parse_name`] never gives, so no
/// verb, spawn or restore that names an identity reaches one, and the roster
/// lists none.
const TOMBSTONE_PREFIX: &str = ".forgotten-";

/// The tombstone `name` is renamed to under `root`: new for every call (this
/// process's pid and its count of forgets), beside the identity, so the rename
/// stays inside one directory.
fn tombstone_for(root: &Path, name: &str) -> PathBuf {
    static FORGETS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    root.join(format!(
        "{TOMBSTONE_PREFIX}{name}-{}-{}",
        std::process::id(),
        FORGETS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ))
}

/// The `<pid>` in a tombstone's name ([`tombstone_for`]:
/// `.forgotten-<name>-<pid>-<k>`, `<name>` a folded identity name), when the
/// name is one.
fn tombstone_pid(file_name: &str) -> Option<u32> {
    let mut tail = file_name.strip_prefix(TOMBSTONE_PREFIX)?.rsplitn(3, '-');
    let (count, pid, name) = (tail.next()?, tail.next()?, tail.next()?);
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(count) || !digits(pid) || parse_name(name).ok().as_deref() != Some(name) {
        return None;
    }
    pid.parse().ok()
}

/// The tombstones this process's forgets are emptying now, each from before
/// its rename until its forget replies ([`Emptying`]). No sweep in this
/// process takes one, so a forget never answers `tombstone=` for a tree
/// another forget is still removing. A leaf lock like [`HOLDS`]: held only to
/// read or change the set, never across a file operation or another lock.
static EMPTYING: std::sync::Mutex<std::collections::BTreeSet<PathBuf>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

fn emptying() -> std::sync::MutexGuard<'static, std::collections::BTreeSet<PathBuf>> {
    EMPTYING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A forget's tombstone in [`EMPTYING`], given back as it drops.
struct Emptying {
    tombstone: PathBuf,
}

impl Emptying {
    fn begin(tombstone: &Path) -> Self {
        emptying().insert(tombstone.to_path_buf());
        Self {
            tombstone: tombstone.to_path_buf(),
        }
    }
}

impl Drop for Emptying {
    fn drop(&mut self) {
        emptying().remove(&self.tombstone);
    }
}

/// Whether the entry `file_name` in `root` is a tombstone no forget may still
/// be emptying: its name is a tombstone's ([`tombstone_pid`]), and its `<pid>`
/// is this process's with no forget of ours emptying it now ([`EMPTYING`]), or
/// is not `alive`. A live aterm's forget may be emptying its own, so another
/// aterm's tombstone stays until that aterm has exited; a reused pid only
/// keeps a dead one's until a later sweep.
fn abandoned(root: &Path, file_name: &str, alive: impl Fn(u32) -> bool) -> bool {
    match tombstone_pid(file_name) {
        Some(pid) if pid == std::process::id() => !emptying().contains(&root.join(file_name)),
        Some(pid) => !alive(pid),
        None => false,
    }
}

/// Remove the tombstones in `root` ([`TOMBSTONE_PREFIX`]) whose names `sweeps`
/// picks, best effort: each one's reply already named it.
fn sweep_tombstones(root: &Path, sweeps: impl Fn(&str) -> bool) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.starts_with(TOMBSTONE_PREFIX) && sweeps(n))
        {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Sweep `root` of the tombstones no forget may still be emptying
/// ([`abandoned`]), `alive` injected: a forget's sweep after its rename
/// ([`forget_reply`]) and the launch's ([`sweep_tombstones_at_launch`]) are
/// this one pass.
fn sweep_abandoned_tombstones(root: &Path, alive: impl Fn(u32) -> bool) {
    sweep_tombstones(root, |name| abandoned(root, name, &alive));
}

/// Sweep the identities folder once at launch, on a detached housekeeping
/// thread, of the abandoned tombstones ([`sweep_abandoned_tombstones`]): their
/// trees still hold a forgotten identity's agent files, a login an agent keeps
/// as a file among them, and the next forget, which sweeps them too, may
/// never come. Nothing waits on the thread, so a slow volume costs the launch
/// nothing. It removes only `.forgotten-<name>-<pid>-<k>` entries directly in
/// the folder ([`tombstone_pid`]), and none a forget may still be emptying.
pub(crate) fn sweep_tombstones_at_launch() {
    let Some(root) = aterm_types::dirs::identities_dir() else {
        return;
    };
    let spawned = crate::qos::spawn_lane_thread(
        "aterm-identity-sweep",
        crate::qos::Role::Housekeeping,
        move || sweep_abandoned_tombstones(&root, crate::control_auth::pid_alive),
    );
    if let Err(e) = spawned {
        aterm_log::warn!("identity tombstones: could not start the launch sweep: {e}");
    }
}

/// `identities forget <name> [confirm=<name>]`. Confirmed and unused, the
/// identity goes in one rename to a fresh tombstone ([`tombstone_for`]): from
/// then on it is gone for every reader and writer that names it by path, so a
/// primer pass's late write fails instead of making it again
/// ([`reprime_detached`]), and nothing lands in the tree being removed, so the
/// removal never meets a folder filling up under it. Unused means no live
/// session carries it and no spawn holds it ([`Admission`]); the forget holds
/// it from that check until its rename returns ([`Forgetting`]), so no spawn
/// sights the folder the rename is taking. The reply says the identity is
/// gone once the rename is done; a tombstone the removal left is named in it,
/// with why.
fn forget_reply(
    root: &Path,
    live: &[(String, String)],
    raw: &str,
    confirm: Option<&str>,
) -> String {
    forget_reply_in(root, live, raw, confirm, |_| {}, |_| {})
}

/// [`forget_reply`] with two moments injected: `held` hears the folder once
/// the forget holds it, before the rename, and `buried` hears the tombstone
/// between the rename and the removal. Tests land a spawn's admission at the
/// first and a primer's late write at the second.
fn forget_reply_in(
    root: &Path,
    live: &[(String, String)],
    raw: &str,
    confirm: Option<&str>,
    held: impl FnOnce(&Path),
    buried: impl FnOnce(&Path),
) -> String {
    let Ok(name) = parse_name(raw) else {
        return IDENTITIES_USAGE.to_string();
    };
    let Some(dir) = identity_dir(root, &name) else {
        return format!("ERR no such identity {name}\n");
    };
    // The confirm word folds like the name: `confirm=Worker` confirms `worker`.
    if confirm.map(str::to_ascii_lowercase).as_deref() != Some(name.as_str()) {
        return format!(
            "ERR confirm: identities forget {name} confirm={name} removes {}\n",
            pct_path(&dir)
        );
    }
    let users: Vec<&str> = live
        .iter()
        .filter(|(i, _)| *i == name)
        .map(|(_, sid)| sid.as_str())
        .collect();
    if !users.is_empty() {
        return format!("ERR identity in use sessions={}\n", users.join(","));
    }
    let hold = match Forgetting::begin(&dir) {
        Ok(hold) => hold,
        Err(spawning) => return format!("ERR identity in use spawning={spawning}\n"),
    };
    held(&dir);
    let tombstone = tombstone_for(root, &name);
    // Counted before the rename, so no sweep in this process, another forget's
    // or the launch's, takes the tombstone while this one empties it.
    let _emptying = Emptying::begin(&tombstone);
    let renamed = std::fs::rename(&dir, &tombstone);
    drop(hold);
    match renamed {
        Ok(()) => {}
        // Gone before the rename (another forget's, or by hand): no identity.
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return format!("ERR no such identity {name}\n");
        }
        Err(e) => return format!("ERR identity {name}: {e}\n"),
    }
    sweep_abandoned_tombstones(root, crate::control_auth::pid_alive);
    buried(&tombstone);
    match std::fs::remove_dir_all(&tombstone) {
        Err(e) if std::fs::symlink_metadata(&tombstone).is_ok() => format!(
            "OK removed={} left=keychain tombstone={} why={}\n",
            pct_path(&dir),
            pct_path(&tombstone),
            aterm_control::wire::pct_encode(&e.to_string())
        ),
        _ => format!("OK removed={} left=keychain\n", pct_path(&dir)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Block until an admission under `dir` waits a forget out
    /// ([`Holds::waiting`]), for at most a minute: a hang detector, never a
    /// latency budget. The count is read under the lock the admission's wait
    /// gives up, so once this returns that admission is parked in its wait.
    fn until_an_admission_waits(dir: &Path) {
        let (_holds, waited) = ADMISSION_WAITS
            .wait_timeout_while(
                identity_holds(),
                std::time::Duration::from_secs(60),
                |holds| holds.get(dir).is_none_or(|held| held.waiting == 0),
            )
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            !waited.timed_out(),
            "no admission waited the forget out (a one-minute hang detector)"
        );
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aterm-identity-{tag}-{}-{}",
            std::process::id(),
            aterm_time::since_epoch().map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The inherited re-prime's throttle is per name: the first spawn under a
    /// name is due, a burst under it is not until the interval has passed (the
    /// edge inclusive), another name is due on its own, and a clock that stepped
    /// back is not due.
    #[test]
    fn an_inherited_identity_is_re_primed_once_per_interval_per_name() {
        let mut last = std::collections::BTreeMap::new();
        let t0 = std::time::Instant::now();
        let ms = std::time::Duration::from_millis;
        assert!(
            reprime_due(&mut last, "worker", t0, false),
            "never re-primed"
        );
        assert!(
            !reprime_due(&mut last, "worker", t0 + ms(5), false),
            "a burst"
        );
        assert!(
            reprime_due(&mut last, "reviewer", t0 + ms(5), false),
            "per name"
        );
        assert!(!reprime_due(
            &mut last,
            "worker",
            t0 + REPRIME_INTERVAL - ms(1),
            false
        ));
        assert!(reprime_due(
            &mut last,
            "worker",
            t0 + REPRIME_INTERVAL,
            false
        ));
        assert!(
            !reprime_due(&mut last, "worker", t0, false),
            "a clock that stepped back is not due, never a panic"
        );
    }

    /// An identity the spawn's admission just made (`identities forget worker`,
    /// then `spawn identity=worker` inside the interval) has no primer file, so
    /// its prime is due however recently the name was primed, and it restarts
    /// the interval for the burst behind it. NEGATIVE CONTROL: the same name
    /// not made is throttled.
    #[test]
    fn a_remade_identity_is_primed_inside_the_interval() {
        let mut last = std::collections::BTreeMap::new();
        let t0 = std::time::Instant::now();
        let ms = std::time::Duration::from_millis;
        assert!(reprime_due(&mut last, "worker", t0, false), "first prime");
        assert!(!reprime_due(&mut last, "worker", t0 + ms(5), false));
        assert!(
            reprime_due(&mut last, "worker", t0 + ms(10), true),
            "remade: due inside the interval"
        );
        assert!(
            !reprime_due(&mut last, "worker", t0 + REPRIME_INTERVAL, false),
            "the interval runs from the remade one's prime"
        );
    }

    /// Admission must finish the directory gate before it hands off a primer
    /// or returns a path a child will receive. The primer is deliberately an
    /// inert observation here: scheduling a writer is not evidence that its
    /// hardening happened before the child was admitted.
    #[cfg(unix)]
    #[test]
    fn an_inherited_spawn_hardens_its_identity_directories_before_admission() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let root = scratch.path().join("identities");
        let dir = root.join("worker");
        let dirs: Vec<PathBuf> = std::iter::once(root.clone())
            .chain(std::iter::once(dir.clone()))
            .chain(aterm_primer::agent_homes().map(|row| dir.join(row.sub)))
            .collect();
        for path in &dirs {
            aterm_types::fs_restricted::ensure_private_dir(path).expect("private fixture");
        }
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o7777;
        let stamp = |p: &Path| {
            let m = std::fs::metadata(p).unwrap();
            (m.ctime(), m.ctime_nsec(), m.mode())
        };
        let before: Vec<_> = dirs.iter().map(|p| stamp(p)).collect();
        let mut handed_off = 0;
        assert_eq!(
            check_for_spawn_in(&root, "worker", SpawnAdmit::Inherited, |got, made| {
                assert_eq!(got, dir.as_path());
                assert!(!made, "an inherited admission makes nothing");
                handed_off += 1;
            })
            .expect("valid identity")
            .dir(),
            dir.as_path()
        );
        assert_eq!(handed_off, 1);
        assert_eq!(
            dirs.iter().map(|p| stamp(p)).collect::<Vec<_>>(),
            before,
            "valid private directories need no writes"
        );
        for drifted in &dirs {
            std::fs::set_permissions(drifted, std::fs::Permissions::from_mode(0o777))
                .expect("mode drift");
            let mut handed_off = false;
            assert_eq!(
                check_for_spawn_in(&root, "worker", SpawnAdmit::Inherited, |got, _| {
                    assert_eq!(got, dir.as_path());
                    for path in &dirs {
                        assert_eq!(mode(path), 0o700, "{} before handoff", path.display());
                    }
                    handed_off = true;
                })
                .expect("our drifted directory can be hardened")
                .dir(),
                dir.as_path()
            );
            assert!(handed_off);
            for path in &dirs {
                assert_eq!(mode(path), 0o700, "{} before return", path.display());
            }
        }
    }

    /// An unreadable/damaged agent home cannot be deferred to a logging-only
    /// background failure. A regular file gives the same deterministic refusal
    /// without changing owners or depending on the executing user's privileges.
    #[cfg(unix)]
    #[test]
    fn an_inherited_spawn_refuses_damaged_agent_directories_before_admission() {
        for row in aterm_primer::agent_homes() {
            let scratch = aterm_tempfile::tempdir().expect("scratch");
            let root = scratch.path().join("identities");
            let dir = root.join("worker");
            for agent in aterm_primer::agent_homes() {
                aterm_types::fs_restricted::ensure_private_dir(&dir.join(agent.sub))
                    .expect("private fixture");
            }
            let broken = dir.join(row.sub);
            std::fs::remove_dir(&broken).expect("empty agent home");
            std::fs::write(&broken, b"not a directory").expect("damaged agent home");
            let mut handed_off = false;
            let result = check_for_spawn_in(&root, "worker", SpawnAdmit::Inherited, |_, _| {
                handed_off = true
            });
            assert!(
                result.is_err(),
                "{} must refuse admission",
                broken.display()
            );
            assert!(!handed_off, "a refusal must not hand off a primer");
            assert_eq!(std::fs::read(&broken).unwrap(), b"not a directory");
        }
    }

    /// A NAMED identity (`spawn identity=<name>`, the picker's new-identity
    /// row) is made by the spawn's own admission, folded, with every folder
    /// private, and its primer is handed off, never written before the admission
    /// returns: the reply waits for no primer file. The handoff hears that this
    /// admission made it, so the throttle cannot skip its first prime; a named
    /// spawn of one that exists made nothing. A name the grammar refuses, or a
    /// root a file stands in, is refused with no primer handed off.
    /// NEGATIVE CONTROL: the inherited admission of the same missing name (the
    /// test below) makes nothing.
    #[cfg(unix)]
    #[test]
    fn a_named_spawn_makes_its_identity_and_leaves_the_primer_to_the_handoff() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let root = scratch.path().join("identities");
        let dir = root.join("worker");
        let mut handed = Vec::new();
        let mut admit = |raw: &str| {
            check_for_spawn_in(&root, raw, SpawnAdmit::Named, |got, made| {
                handed.push((got.to_path_buf(), made));
            })
        };
        let first = admit("Worker").expect("a new named identity is made");
        assert_eq!(first.dir(), dir.as_path());
        let again = admit("worker").expect("and admitted again");
        assert_eq!(again.dir(), dir.as_path());
        assert_eq!(
            handed,
            [(dir.clone(), true), (dir.clone(), false)],
            "each primer handed off once, saying whether the admission made it"
        );
        for path in std::iter::once(root.clone())
            .chain(std::iter::once(dir.clone()))
            .chain(aterm_primer::agent_homes().map(|row| dir.join(row.sub)))
        {
            let mode = std::fs::metadata(&path).expect("made").permissions().mode();
            assert_eq!(mode & 0o7777, 0o700, "{}", path.display());
        }
        assert!(
            !dir.join(".claude/CLAUDE.md").exists(),
            "nothing primed before the admission returned"
        );
        assert_eq!(identity_dir(&root, "worker"), Some(dir.clone()));

        let mut handed_off = false;
        let refused = check_for_spawn_in(&root, "Bad Name", SpawnAdmit::Named, |_, _| {
            handed_off = true;
        })
        .expect_err("the grammar refuses it");
        assert_eq!(refused.kind(), io::ErrorKind::InvalidInput);
        std::fs::remove_dir_all(&root).expect("clear the root");
        std::fs::write(&root, b"").expect("a file in the root's place");
        assert!(
            check_for_spawn_in(&root, "worker", SpawnAdmit::Named, |_, _| handed_off = true)
                .is_err(),
            "a root that cannot hold folders refuses"
        );
        assert!(!handed_off, "a refusal hands off no primer");
    }

    #[test]
    fn spawn_admission_never_creates_a_missing_identity() {
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let root = scratch.path().join("identities");
        let mut handed_off = false;
        let result = check_for_spawn_in(&root, "missing", SpawnAdmit::Inherited, |_, _| {
            handed_off = true
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotFound);
        assert!(!handed_off);
        assert!(!root.exists(), "admission creates no identity tree");
    }

    /// An inherited identity's admission makes nothing at or above its folder.
    /// This process's `identities forget` cannot take the folder from under
    /// it: the admission holds the identity, so a forget there is refused as
    /// in use and the folder stays. A folder that goes anyway between the
    /// admission's sighting and its hardening (removed by hand, or by another
    /// aterm's forget) refuses the spawn in the words a missing identity gets,
    /// hands off no prime, and leaves the root empty. NEGATIVE CONTROL: the
    /// hardening this replaced (`control_auth::ensure_private_dir` on the
    /// folder and each agent subdir) at the same point brings the identity
    /// back, and the admission then admits it and hands its prime off.
    #[test]
    fn an_inherited_admission_whose_folder_goes_refuses_and_makes_nothing() {
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let root = scratch.path().join("identities");
        let (dir, _) = make_dirs_in(&root, "worker").expect("an identity");
        let take = || {
            assert_eq!(
                identities_reply(&root, &[], "forget worker confirm=worker"),
                "ERR identity in use spawning=1\n",
                "this process's forget is held off"
            );
            assert!(dir.join(".claude").is_dir(), "and touches nothing");
            std::fs::remove_dir_all(&dir).expect("taken from outside the process");
        };
        let mut handed_off = false;
        let refused = check_for_spawn_at(
            &root,
            "worker",
            SpawnAdmit::Inherited,
            |_| take(),
            |_, _| handed_off = true,
        )
        .expect_err("its folder went under the admission");
        assert_eq!(refused.kind(), io::ErrorKind::NotFound);
        assert_eq!(refused.to_string(), "no such identity worker");
        assert!(!handed_off, "a refusal hands off no prime");
        assert!(
            std::fs::read_dir(&root).unwrap().next().is_none(),
            "nothing is made again"
        );

        make_dirs_in(&root, "worker").expect("made again");
        let admitted = check_for_spawn_at(
            &root,
            "worker",
            SpawnAdmit::Inherited,
            |dir| {
                take();
                crate::control_auth::ensure_private_dir(dir).expect("the old hardening");
                for row in aterm_primer::agent_homes() {
                    crate::control_auth::ensure_private_dir(&dir.join(row.sub))
                        .expect("the old hardening");
                }
            },
            |_, _| handed_off = true,
        );
        assert_eq!(
            admitted.expect("the old hardening admits it").dir(),
            dir.as_path()
        );
        assert!(handed_off, "and hands its prime off");
        assert!(
            dir.join(".claude").is_dir(),
            "the creating hardening brings the identity back after it went"
        );
    }

    /// An existing inherited identity missing an agent subdir (one made by a
    /// build whose table lacked that agent's row) has the subdir made, 0700,
    /// one level below its folder, and is admitted.
    #[cfg(unix)]
    #[test]
    fn an_inherited_identity_missing_an_agent_subdir_has_it_made() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let root = scratch.path().join("identities");
        let (dir, _) = make_dirs_in(&root, "worker").expect("an identity");
        let row = aterm_primer::agent_homes().last().expect("an agent row");
        std::fs::remove_dir(dir.join(row.sub)).expect("an empty agent subdir");
        assert_eq!(
            check_for_spawn_in(&root, "worker", SpawnAdmit::Inherited, |_, _| {})
                .expect("admitted")
                .dir(),
            dir.as_path()
        );
        let mode = std::fs::metadata(dir.join(row.sub))
            .expect("made")
            .permissions()
            .mode();
        assert_eq!(mode & 0o7777, 0o700, "{}", row.sub);
    }

    /// An agent subdir that is a symlink to nothing refuses the spawn for what
    /// it is, not as an identity a forget took: the identity is there, its
    /// agent folder is no folder. The link is left as it was and nothing is
    /// made where it points. NEGATIVE CONTROL: a followed `stat` alone, which
    /// `harden_private_dir` read until 2026-10-18, says `NotFound` for it, the
    /// word the admission reads as a forgotten identity.
    #[cfg(unix)]
    #[test]
    fn an_agent_subdir_linked_to_nothing_is_refused_as_what_it_is() {
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let root = scratch.path().join("identities");
        let (dir, _) = make_dirs_in(&root, "worker").expect("an identity");
        let row = aterm_primer::agent_homes().next().expect("an agent row");
        let sub = dir.join(row.sub);
        let nowhere = scratch.path().join("nowhere");
        std::fs::remove_dir(&sub).expect("an empty agent subdir");
        std::os::unix::fs::symlink(&nowhere, &sub).expect("a link to nothing");
        let mut handed_off = false;
        let refused = check_for_spawn_in(&root, "worker", SpawnAdmit::Inherited, |_, _| {
            handed_off = true;
        })
        .expect_err("refused");
        assert_eq!(refused.kind(), io::ErrorKind::NotADirectory);
        assert_eq!(
            refused.to_string(),
            format!("{}: a symlink to nothing", sub.display())
        );
        assert!(!handed_off, "a refusal hands off no prime");
        assert!(
            std::fs::symlink_metadata(&sub).is_ok_and(|m| m.file_type().is_symlink()),
            "the link is left as it was"
        );
        assert!(!nowhere.exists(), "and nothing is made where it points");
        assert_eq!(
            std::fs::metadata(&sub).map(drop).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    /// A named admission's `made` is its own create's answer: a new name is
    /// made, one that is there is not, and one an `identities forget` took
    /// after a look that saw it is made again and says so, so its first prime
    /// is never throttled away. NEGATIVE CONTROL: the look this replaced,
    /// taken before that forget, says not made.
    #[test]
    fn a_named_admission_says_made_iff_its_own_create_made_the_folder() {
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let root = scratch.path().join("identities");
        let mut made = Vec::new();
        let mut admit = || {
            check_for_spawn_in(&root, "worker", SpawnAdmit::Named, |_, m| made.push(m))
                .expect("admitted")
                .dir()
                .to_path_buf()
        };
        let dir = admit();
        admit();
        let old_made = identity_dir(&root, "worker").is_none();
        assert_eq!(
            identities_reply(&root, &[], "forget worker confirm=worker"),
            format!("OK removed={} left=keychain\n", pct_path(&dir))
        );
        admit();
        assert_eq!(made, [true, false, true]);
        assert!(!old_made, "the look before the forget said not made");
        assert!(dir.join(".claude").is_dir(), "and the folder is there");
    }

    /// A spawn admitted under an identity holds it until its admission drops,
    /// with the spawn's failure or the session that wears it: a confirmed
    /// forget meanwhile is refused as in use, counting every such spawn, and
    /// leaves the folder the child's agent homes name. That holds whichever
    /// door admitted it: the spawner's, named or inherited, the shell's own
    /// check ([`admit_existing_in`]), or an adopted shell's label
    /// ([`hold_adopted_in`]). Once the last one drops the forget goes through.
    /// NEGATIVE CONTROL: a path handed on without its admission, as the
    /// admission returned it until 2026-10-18, holds nothing: the same forget
    /// answers `OK` and the path names nothing.
    #[test]
    fn a_spawn_admitted_under_an_identity_holds_off_its_forget() {
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let root = scratch.path().join("identities");
        let forget = || identities_reply(&root, &[], "forget worker confirm=worker");
        let named = check_for_spawn_in(&root, "worker", SpawnAdmit::Named, |_, _| {})
            .expect("made and admitted");
        let dir = named.dir().to_path_buf();
        let inherited = check_for_spawn_in(&root, "worker", SpawnAdmit::Inherited, |_, _| {})
            .expect("admitted");
        let shell = admit_existing_in(&root, "Worker").expect("admitted");
        assert_eq!(shell.dir(), dir.as_path(), "the name folds");
        let adopted = hold_adopted_in(&root, "worker").expect("held");
        assert_eq!(adopted.dir(), dir.as_path());
        assert_eq!(forget(), "ERR identity in use spawning=4\n");
        drop(named);
        drop(inherited);
        drop(adopted);
        assert_eq!(forget(), "ERR identity in use spawning=1\n");
        assert!(dir.join(".claude").is_dir(), "its agent homes stay");
        drop(shell);
        assert_eq!(
            forget(),
            format!("OK removed={} left=keychain\n", pct_path(&dir))
        );

        let handed = check_for_spawn_in(&root, "worker", SpawnAdmit::Named, |_, _| {})
            .expect("made again")
            .dir()
            .to_path_buf();
        assert_eq!(
            forget(),
            format!("OK removed={} left=keychain\n", pct_path(&handed))
        );
        assert!(!handed.join(".claude").exists(), "it names nothing");
    }

    /// A confirmed forget holds the identity from its checks until its rename,
    /// so no spawn arriving in between sights the folder the rename is taking.
    /// The shell's own check, which never waits, is refused at once as `no such
    /// identity` while the folder is still there, and an adopted shell's label
    /// is held by nothing. The spawner's admission, started while the forget
    /// holds the identity, waits: the forget renames only once that admission
    /// is parked in its wait ([`until_an_admission_waits`]), and the admission
    /// then makes the identity anew, empty, and says it made it. One-minute
    /// hang detectors bound both waits. NEGATIVE CONTROLS: with
    /// `Admission::begin` counting at once instead of waiting, no admission
    /// ever waits and the first detector fails the test; and the shell's same
    /// check once the forget is done is admitted, so its refusal was the
    /// hold's.
    #[test]
    fn a_forget_holds_off_a_spawn_until_its_rename() {
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let root = scratch.path().join("identities");
        let dir = ensure_in(&root, "worker", true).expect("a primed identity");
        let (tx, rx) = std::sync::mpsc::channel();
        let reply = forget_reply_in(
            &root,
            &[],
            "worker",
            Some("worker"),
            |held| {
                assert_eq!(held, dir.as_path());
                assert!(held.join(".claude").is_dir(), "not renamed yet");
                let refused = admit_existing_in(&root, "worker").expect_err("held");
                assert_eq!(refused.kind(), io::ErrorKind::NotFound);
                assert_eq!(refused.to_string(), "no such identity worker");
                assert!(
                    hold_adopted_in(&root, "worker").is_none(),
                    "an adoption holds nothing a forget is taking"
                );
                let root = root.clone();
                std::thread::spawn(move || {
                    let mut made = None;
                    let named = check_for_spawn_in(&root, "worker", SpawnAdmit::Named, |_, m| {
                        made = Some(m);
                    });
                    let named = named.map(|admission| admission.dir().to_path_buf());
                    let _ = tx.send((named.map_err(|e| e.to_string()), made));
                });
                until_an_admission_waits(held);
            },
            |_| {},
        );
        assert_eq!(
            reply,
            format!("OK removed={} left=keychain\n", pct_path(&dir))
        );
        let (named, made) = rx
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("the spawner's admission ends (a one-minute hang detector)");
        assert_eq!(
            named.as_deref(),
            Ok(dir.as_path()),
            "admitted after the rename"
        );
        assert_eq!(made, Some(true), "and made anew");
        assert!(dir.join(".claude").is_dir());
        assert!(
            !dir.join(".claude/CLAUDE.md").exists(),
            "empty: nothing of the forgotten identity came back"
        );
        assert!(admit_existing_in(&root, "worker").is_ok(), "unheld again");
    }

    #[test]
    fn names_fold_to_lowercase_and_follow_the_grammar() {
        assert_eq!(parse_name("Worker").as_deref(), Ok("worker"));
        assert_eq!(parse_name("WORKER-2.a_b").as_deref(), Ok("worker-2.a_b"));
        assert_eq!(parse_name("9lives").as_deref(), Ok("9lives"));
        let max = "a".repeat(NAME_MAX);
        assert_eq!(parse_name(&max).as_deref(), Ok(max.as_str()));
        for bad in [
            "", "-", "-x", ".hidden", "_under", "a/b", "a b", "..", "a\u{e9}", "worker\n",
            " worker", "worker ", "ä",
        ] {
            assert!(parse_name(bad).is_err(), "{bad:?} must be refused");
        }
        // The verb's own word: `spawn identity=forget` would create an identity
        // no verb could describe or remove. Folded first, so `FORGET` too.
        for word in ["forget", "FORGET", "Forget"] {
            assert!(parse_name(word).is_err(), "{word:?} must be refused");
        }
        assert!(parse_name("forget2").is_ok() && parse_name("forgetful").is_ok());
        assert!(parse_name(&"a".repeat(NAME_MAX + 1)).is_err());
        // The two spellings are one identity: same folded name, same dir.
        assert_eq!(parse_name("Worker"), parse_name("wOrKeR"));
    }

    #[test]
    fn ensure_creates_0700_dirs_once_primes_them_and_never_creates_on_restore() {
        let root = scratch("ensure").join("identities");
        let dir = ensure_in(&root, "Worker", true).expect("created");
        assert_eq!(
            dir,
            root.join("worker"),
            "folded, under the identities root"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&root), 0o700);
            assert_eq!(mode(&dir), 0o700);
            for row in aterm_primer::agent_homes() {
                assert_eq!(mode(&dir.join(row.sub)), 0o700, "{}", row.agent);
            }
        }
        // The primer reached every agent: its context file and skills.
        assert!(dir.join(".claude/CLAUDE.md").is_file());
        assert!(dir.join(".claude/skills/aterm-fabric/SKILL.md").is_file());
        assert!(
            dir.join(".claude/skills/supervise-agent/SKILL.md")
                .is_file()
        );
        assert!(dir.join(".codex/AGENTS.md").is_file());
        assert!(dir.join(".codex/prompts/aterm-fabric.md").is_file());
        assert!(
            !dir.join(".claude/settings.json").exists(),
            "no human settings given: no settings file is invented"
        );
        // Once: the same name (any case) is the same dir, and nothing errs.
        let again = ensure_in(&root, "WORKER", true).expect("idempotent");
        assert_eq!(again, dir);
        assert_eq!(
            std::fs::read_dir(&root).unwrap().count(),
            1,
            "one directory for the one identity"
        );
        // Two names, two dirs.
        let other = ensure_in(&root, "reviewer", true).expect("second identity");
        assert_ne!(other, dir);
        assert!(other.join(".claude").is_dir() && other.join(".codex").is_dir());
        // The restore rule: existing is found; missing is NotFound and stays missing.
        assert_eq!(ensure_in(&root, "worker", false).unwrap(), dir);
        let missing = ensure_in(&root, "ghost", false).unwrap_err();
        assert_eq!(missing.kind(), io::ErrorKind::NotFound);
        assert!(!root.join("ghost").exists(), "create=false never creates");
        // A bad name is refused before anything is touched.
        assert_eq!(
            ensure_in(&root, "-", true).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn env_points_every_table_var_into_the_identity_dir() {
        let dir = PathBuf::from("/state/identities/worker");
        let env = env(&dir);
        // The subdir is joined by the host's own separator (`\` on Windows),
        // so the expectation is spelled through the same join.
        let under = |sub: &str| dir.join(sub).to_string_lossy().into_owned();
        assert_eq!(
            env,
            vec![
                ("CLAUDE_CONFIG_DIR".to_string(), under(".claude")),
                ("CODEX_HOME".to_string(), under(".codex")),
            ]
        );
        assert_eq!(env.len(), aterm_primer::agent_homes().count());
    }

    /// The public `ensure` resolves through the state root (the dev build's
    /// `ATERM_STATE_HOME` seam): the identities root is `<state>/identities`.
    #[test]
    fn ensure_resolves_the_identities_root_through_the_state_home() {
        let state = scratch("state");
        crate::test_env::scoped("ATERM_STATE_HOME", &state, || {
            assert_eq!(
                dir_for("worker"),
                Some(state.join("identities").join("worker"))
            );
            let dir = ensure("Test", true).expect("created under the state home");
            assert_eq!(dir, state.join("identities").join("test"));
            assert!(dir.join(".claude").is_dir() && dir.join(".codex").is_dir());
            assert_eq!(restorable("test").as_deref(), Some("test"));
            assert_eq!(
                restorable("ghost"),
                None,
                "a missing identity is not restorable"
            );
            assert!(
                !state.join("identities").join("ghost").exists(),
                "and restore created nothing"
            );
        });
        let _ = std::fs::remove_dir_all(&state);
    }

    /// A file, a symlink and a directory whose name is not its own folded spelling are not
    /// listed; `present` means a non-empty subdirectory.
    #[test]
    fn identities_lists_names_live_counts_and_presence_by_non_empty_subdir() {
        let root = scratch("verb-list");
        ensure_in(&root, "worker", true).unwrap();
        let bare = root.join("bare");
        std::fs::create_dir_all(bare.join(".claude")).unwrap();
        std::fs::create_dir_all(bare.join(".codex")).unwrap();
        std::fs::write(root.join("notes.txt"), b"not an identity").unwrap();
        std::fs::create_dir_all(root.join("Not Folded")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("worker"), root.join("link")).unwrap();
        let live = vec![
            ("worker".to_string(), "s-a".to_string()),
            ("worker".to_string(), "s-b".to_string()),
            ("other".to_string(), "s-c".to_string()),
        ];
        let out = identities_reply(&root, &live, "");
        assert_eq!(
            out,
            format!(
                "OK 2\nbare dir={} sessions=0 agents=claude:absent,codex:absent\n\
                 worker dir={} sessions=2 agents=claude:present,codex:present\n",
                pct_path(&bare),
                pct_path(&root.join("worker")),
            )
        );
        // A root that does not exist yet is an empty roster, not an error.
        assert_eq!(identities_reply(&root.join("none"), &[], ""), "OK 0\n");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `present` is a name listing: the sentinel is mode 0000, so opening it would fail the
    /// test. The name folds; an unknown name is named back.
    #[test]
    fn identities_one_names_each_agents_var_and_home_and_never_opens_a_file() {
        let root = scratch("verb-one");
        let dir = root.join("sealed");
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        std::fs::create_dir_all(dir.join(".codex")).unwrap();
        let sentinel = dir.join(".claude").join(".credentials.json");
        std::fs::write(&sentinel, b"{\"secret\":\"never read\"}").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&sentinel, std::fs::Permissions::from_mode(0o000)).unwrap();
        }
        let want = format!(
            "OK 3\nsealed dir={} sessions=0 agents=claude:present,codex:absent\n\
             agent=claude var=CLAUDE_CONFIG_DIR home={} files=present\n\
             agent=codex var=CODEX_HOME home={} files=absent\n",
            pct_path(&dir),
            pct_path(&dir.join(".claude")),
            pct_path(&dir.join(".codex")),
        );
        assert_eq!(identities_reply(&root, &[], "sealed"), want);
        assert_eq!(
            identities_reply(&root, &[], "SEALED"),
            want,
            "the name folds"
        );
        assert_eq!(
            identities_reply(&root, &[], "ghost"),
            "ERR no such identity ghost\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&sentinel, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Both refusals touch nothing; the removal reply says `left=keychain`.
    #[test]
    fn identities_forget_needs_the_confirm_word_refuses_a_live_user_then_removes_the_tree() {
        let root = scratch("verb-forget");
        let dir = ensure_in(&root, "worker", true).unwrap();
        let confirm_line = format!(
            "ERR confirm: identities forget worker confirm=worker removes {}\n",
            pct_path(&dir)
        );
        assert_eq!(identities_reply(&root, &[], "forget worker"), confirm_line);
        assert_eq!(
            identities_reply(&root, &[], "forget worker confirm=other"),
            confirm_line
        );
        assert_eq!(
            identities_reply(&root, &[], "forget Worker"),
            confirm_line,
            "the name folds before the confirm line names it"
        );
        assert!(dir.is_dir(), "nothing removed without the word");

        let live = vec![
            ("worker".to_string(), "s-a".to_string()),
            ("other".to_string(), "s-b".to_string()),
            ("worker".to_string(), "s-c".to_string()),
        ];
        assert_eq!(
            identities_reply(&root, &live, "forget worker confirm=worker"),
            "ERR identity in use sessions=s-a,s-c\n"
        );
        assert!(dir.is_dir(), "nothing removed while a session carries it");

        assert_eq!(
            identities_reply(&root, &[], "forget worker confirm=Worker"),
            format!("OK removed={} left=keychain\n", pct_path(&dir)),
            "the confirm word folds like the name"
        );
        assert!(!dir.exists(), "the tree is gone");
        assert!(
            std::fs::read_dir(&root).unwrap().next().is_none(),
            "and no tombstone is left beside it"
        );
        assert_eq!(
            identities_reply(&root, &[], "forget worker confirm=worker"),
            "ERR no such identity worker\n"
        );
        assert_eq!(identities_reply(&root, &[], ""), "OK 0\n");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Forget takes the identity away in one rename, so a primer pass whose
    /// check passed before the forget and whose writes land after the rename
    /// writes nothing anywhere. Its writes land here between the rename and the
    /// removal, the worst moment: the removal by name used to meet the folders
    /// filling up under it (`Directory not empty`), and the pass that made its
    /// folders first put the identity back behind the `OK`. The reply is the
    /// plain `OK`, the identity stays gone, and no tombstone is left.
    /// NEGATIVE CONTROL: the pass this replaced (`ensure_in` with `create`,
    /// the folders and then the files) at the same moment brings it back.
    #[test]
    fn a_primer_writing_while_forget_removes_cannot_bring_the_identity_back() {
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let root = scratch.path().join("identities");
        let dir = ensure_in(&root, "worker", true).expect("a primed identity");
        let ok = format!("OK removed={} left=keychain\n", pct_path(&dir));
        let mut buried_at = None;
        let reply = forget_reply_in(
            &root,
            &[],
            "worker",
            Some("worker"),
            |_| {},
            |tombstone| {
                assert!(!dir.exists(), "gone by its name once buried");
                assert!(
                    tombstone.join(".claude/CLAUDE.md").is_file(),
                    "the tombstone holds what the pass wrote before"
                );
                prime_dir("worker", &dir);
                buried_at = Some(tombstone.to_path_buf());
            },
        );
        assert_eq!(reply, ok);
        assert!(!dir.exists(), "the late writes made nothing");
        assert!(!buried_at.expect("buried").exists(), "the tombstone went");
        assert!(
            std::fs::read_dir(&root).unwrap().next().is_none(),
            "nothing is left in the root"
        );

        let dir = ensure_in(&root, "worker", true).expect("made again");
        let reply = forget_reply_in(
            &root,
            &[],
            "worker",
            Some("worker"),
            |_| {},
            |_| {
                ensure_in(&root, "worker", true).expect("the old pass makes its folders");
            },
        );
        assert_eq!(reply, ok);
        assert!(
            dir.join(".claude/CLAUDE.md").is_file(),
            "the old pass brings the identity back behind the same OK"
        );
    }

    /// A primer pass after the forget makes nothing, on either side of its
    /// check: the detached pass's own body ([`reprime_existing`]) finds no
    /// identity, and a pass that passed its check before the forget
    /// ([`prime_dir`] on the path it holds) finds no folder to write under.
    /// NEGATIVE CONTROL: the same body over an identity a spawn's admission
    /// just made (its folders, no files yet) writes the primer's files.
    #[test]
    fn a_primer_pass_after_forget_makes_nothing() {
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let root = scratch.path().join("identities");
        let (dir, _) = make_dirs_in(&root, "worker").expect("admitted");
        reprime_existing(&root, "worker");
        assert!(
            dir.join(".claude/CLAUDE.md").is_file(),
            "the pass primes it"
        );
        assert!(dir.join(".codex/AGENTS.md").is_file());
        assert_eq!(
            identities_reply(&root, &[], "forget worker confirm=worker"),
            format!("OK removed={} left=keychain\n", pct_path(&dir))
        );
        reprime_existing(&root, "worker");
        prime_dir("worker", &dir);
        assert!(!dir.exists(), "no pass made it again");
        assert!(std::fs::read_dir(&root).unwrap().next().is_none());
    }

    /// A tombstone the removal cannot take leaves the identity gone all the
    /// same: the reply says so and names the tombstone and why it stayed, and
    /// no reader sees an identity there (it is no name). The next forget in
    /// this process, of any identity, sweeps it. The fixture is a folder whose
    /// own mode lacks write, so its entries cannot be unlinked, except by root,
    /// which the mode does not stop: as root there is no fixture, and the test
    /// ends COULD NOT RUN.
    #[cfg(unix)]
    #[test]
    fn a_tombstone_forget_cannot_remove_is_named_and_swept_by_the_next_forget() {
        use std::os::unix::fs::PermissionsExt;
        aterm_uds::host::could_not_run_as_root("a folder whose mode lacks write");
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let root = scratch.path().join("identities");
        let dir = ensure_in(&root, "worker", true).expect("a primed identity");
        let seal = |p: &Path, mode| {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).expect("mode")
        };
        seal(&dir.join(".claude"), 0o500);
        let reply = identities_reply(&root, &[], "forget worker confirm=worker");
        let left: Vec<PathBuf> = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        let [tombstone] = left.as_slice() else {
            panic!("one tombstone in the root, got {left:?}");
        };
        assert!(
            tombstone
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(".forgotten-worker-")),
            "{tombstone:?}"
        );
        let head = format!(
            "OK removed={} left=keychain tombstone={} why=",
            pct_path(&dir),
            pct_path(tombstone)
        );
        assert!(
            reply.starts_with(&head) && reply.len() > head.len() + 1 && reply.ends_with('\n'),
            "{reply:?}"
        );
        assert!(!dir.exists(), "the identity is gone");
        assert_eq!(identities_reply(&root, &[], ""), "OK 0\n");
        assert_eq!(
            identities_reply(&root, &[], "worker"),
            "ERR no such identity worker\n"
        );

        seal(&tombstone.join(".claude"), 0o700);
        let other = ensure_in(&root, "other", true).expect("another identity");
        assert_eq!(
            identities_reply(&root, &[], "forget other confirm=other"),
            format!("OK removed={} left=keychain\n", pct_path(&other))
        );
        assert!(
            std::fs::read_dir(&root).unwrap().next().is_none(),
            "the next forget swept the tombstone"
        );
    }

    /// A sweep, a forget's or the launch's, takes only the tombstones no forget
    /// may still be emptying: one a forget of this process left, once no forget
    /// is emptying it, and a dead process's. One a forget of this process is
    /// emptying, a live process's (its forget may be emptying it), a dot-name
    /// that is no tombstone of a forget's, and an identity all stay. The pid is
    /// read from the name the forget gives ([`tombstone_for`]), a name with
    /// dashes included. NEGATIVE CONTROL: a forget's sweep until 2026-10-18,
    /// every tombstone but its own, takes the one another forget is emptying,
    /// the live one's and the dot-name too, and still no identity.
    #[test]
    fn a_sweep_takes_only_tombstones_no_forget_may_be_emptying() {
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let root = scratch.path().join("identities");
        let (worker, _) = make_dirs_in(&root, "worker").expect("an identity");
        let own = std::process::id();
        let (live, dead) = (own.wrapping_add(1), own.wrapping_add(2));
        let named = tombstone_for(&root, "my-worker.2");
        let named = named.file_name().and_then(|n| n.to_str()).expect("a name");
        assert_eq!(tombstone_pid(named), Some(own));
        let tombstone = |name: String| {
            let path = root.join(name);
            std::fs::create_dir_all(path.join(".claude")).expect("a tombstone");
            std::fs::write(path.join(".claude/.credentials.json"), b"{}").expect("a login");
            path
        };
        let left = tombstone(format!(".forgotten-worker-{own}-0"));
        let ours = tombstone(format!(".forgotten-worker-{own}-1"));
        let theirs = tombstone(format!(".forgotten-worker-{live}-0"));
        let gone = tombstone(format!(".forgotten-old-name-{dead}-7"));
        let odd = tombstone(".forgotten-notes".to_string());
        let emptying = Emptying::begin(&ours);
        sweep_abandoned_tombstones(&root, |pid| pid == live);
        assert!(!left.exists(), "one a forget of ours left goes");
        assert!(!gone.exists(), "a dead process's goes");
        for stays in [&ours, &theirs, &odd, &worker] {
            assert!(stays.exists(), "{} stays", stays.display());
        }
        drop(emptying);
        sweep_abandoned_tombstones(&root, |pid| pid == live);
        assert!(!ours.exists(), "once its forget is done with it, it goes");
        assert!(theirs.exists() && odd.exists());

        let ours = tombstone(format!(".forgotten-worker-{own}-2"));
        let _emptying = Emptying::begin(&ours);
        let its_own = tombstone_for(&root, "other");
        sweep_tombstones(&root, |n| root.join(n) != its_own);
        assert!(!ours.exists() && !theirs.exists() && !odd.exists());
        assert!(worker.join(".claude").is_dir(), "nor an identity");
    }

    /// Junk is usage, and so is a name the grammar refuses, so `..` and a path
    /// are never joined to the root.
    #[test]
    fn identities_junk_and_ungrammatical_names_are_usage() {
        let root = scratch("verb-usage");
        for rest in [
            "forget",
            "x y",
            "confirm=x",
            "forget x y",
            "forget x confirm=x extra",
            "../x",
            "-",
            "forget ../x confirm=../x",
            "forget - confirm=-",
        ] {
            assert_eq!(
                identities_reply(&root, &[], rest),
                IDENTITIES_USAGE,
                "{rest:?}"
            );
        }
        assert!(
            std::fs::read_dir(&root).unwrap().next().is_none(),
            "usage touches nothing"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Provisioning an identity writes under the identity dir only, even when
    /// the human's `$XDG_CONFIG_HOME` holds an `opencode` directory.
    #[test]
    fn ensure_never_writes_into_the_humans_xdg_config_home() {
        let scratch = scratch("xdg");
        let xdg = scratch.join("xdg");
        let humans_opencode = xdg.join("opencode");
        std::fs::create_dir_all(&humans_opencode).unwrap();
        let root = scratch.join("identities");
        crate::test_env::scoped("XDG_CONFIG_HOME", &xdg, || {
            let dir = ensure_in(&root, "worker", true).expect("created");
            assert!(
                dir.join(".claude/CLAUDE.md").is_file(),
                "the identity IS primed"
            );
            let leaked: Vec<String> = std::fs::read_dir(&humans_opencode)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            assert!(
                leaked.is_empty(),
                "identity provisioning wrote into the human's $XDG_CONFIG_HOME/opencode: {leaked:?}"
            );
            assert!(
                !dir.join(".config").exists(),
                "and nothing of an agent without a var is invented under the identity"
            );
        });
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// A symlink in the root is an identity to neither restore nor the verb (both read
    /// [`identity_dir`]), so restore never points env outside the identities tree.
    #[cfg(unix)]
    #[test]
    fn restore_and_the_verb_agree_on_what_an_identity_is() {
        let root = scratch("symlink").join("identities");
        std::fs::create_dir_all(&root).unwrap();
        let elsewhere = root.parent().unwrap().join("elsewhere");
        std::fs::create_dir_all(elsewhere.join(".claude")).unwrap();
        std::fs::create_dir_all(elsewhere.join(".codex")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join("link")).unwrap();
        assert_eq!(
            identities_reply(&root, &[], "link"),
            "ERR no such identity link\n",
            "the verb: a symlink is not an identity"
        );
        assert_eq!(
            identities_reply(&root, &[], "forget link confirm=link"),
            "ERR no such identity link\n"
        );
        let restore = ensure_in(&root, "link", false);
        assert_eq!(
            restore.as_ref().map_err(io::Error::kind),
            Err(io::ErrorKind::NotFound),
            "restore: the same answer — got {restore:?}, whose env would have been {:?}",
            restore.as_ref().map(|d| env(d)).ok()
        );
        assert!(
            root.join("link").exists() && elsewhere.is_dir(),
            "nothing touched: the link and its target both stand"
        );
        // A file in the root is no identity to either, and a real directory
        // is one to both.
        std::fs::write(root.join("file"), b"").unwrap();
        assert_eq!(
            identities_reply(&root, &[], "file"),
            "ERR no such identity file\n"
        );
        assert_eq!(
            ensure_in(&root, "file", false).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        let real = ensure_in(&root, "real", true).unwrap();
        assert!(identities_reply(&root, &[], "real").starts_with("OK 3\nreal dir="));
        assert_eq!(ensure_in(&root, "real", false).unwrap(), real);
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    /// THE IDENTITIES' FOLDER IS READ ONLY ONCE ITS VOLUME ANSWERED: a root
    /// whose volume did not answer is refused by name (`TimedOut`, "did not
    /// answer"), and the volume is all that is asked of it — nothing under it
    /// is looked at, made or listed — so a spawn's admission, a restore's
    /// check, the picker's roster and the `identities` verb end at once
    /// instead of waiting on it. A root that answered is the root; none at all
    /// is still `NotFound`. NEGATIVE CONTROL: a gate that does not ask the
    /// volume hands the silent root on.
    #[test]
    fn an_identities_folder_whose_volume_did_not_answer_is_refused_by_name() {
        let root = scratch("silent").join("identities");
        let asked = std::cell::Cell::new(None);
        let refused = answering(Some(root.clone()), |at| {
            asked.set(Some(at.to_path_buf()));
            false
        })
        .expect_err("a silent volume is refused");
        assert_eq!(refused.kind(), io::ErrorKind::TimedOut);
        assert_eq!(
            refused.to_string(),
            format!("{} did not answer", root.display())
        );
        assert_eq!(asked.take(), Some(root.clone()), "the volume was asked");
        assert!(!root.exists(), "nothing under it was made");
        assert_eq!(answering(Some(root.clone()), |_| true).unwrap(), root);
        assert_eq!(
            answering(None, |_| true).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }
}
