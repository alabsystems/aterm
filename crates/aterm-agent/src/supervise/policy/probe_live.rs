// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LIVE PROBE.

use std::cell::{Cell, RefCell};
use std::collections::{HashSet, VecDeque};
use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use aterm_types::domain::ENV_PARENT_SESSION_ID;

use super::approval::{SecretRule, walk_hit};
use super::claude_settings::OwnerAsk;
use super::git_config::{GitView, WorkerEnv};
use super::probe::{Below, LiveProbe, OnDisk, PolicyProbe, WorkerWrites};
use super::rm_breaker::abs_components;
use super::shell_startup::ShellStartup;

impl PolicyProbe for LiveProbe {
    fn hidden_twin_in(
        &self,
        comps: &[String],
        home: Option<&Path>,
    ) -> Result<Option<String>, String> {
        let (comps, home) = (comps.to_vec(), home.map(Path::to_path_buf));
        bounded(
            "a Read's folders",
            move || hidden_twin_within(&comps, HIDDEN_TWIN_ENTRIES, home.as_deref(), &folder_names),
            Err,
        )
    }

    fn real_components(&self, comps: &[String]) -> Option<Vec<String>> {
        let comps = comps.to_vec();
        bounded(
            "a Read's symbolic links",
            move || real_components(&comps),
            |_| None,
        )
    }

    fn rm_physical(&self, comps: &[String]) -> Result<OnDisk, String> {
        let comps = comps.to_vec();
        bounded("an rm's path on the disk", move || physical(&comps), Err)
    }

    fn below_tree(
        &self,
        root: &[String],
        below: usize,
        secrets: &[SecretRule],
        home: Option<&Path>,
    ) -> Result<Below, String> {
        let (root, secrets, home) = (root.to_vec(), secrets.to_vec(), home.map(Path::to_path_buf));
        bounded(
            "a tree a reader descends",
            move || below_within(&root, below, &secrets, home.as_deref(), TREE_ENTRIES),
            Err,
        )
    }

    /// The process leading `group`, read through the kernel
    /// ([`atpkg::caller_shell::process_args`]) and taken for the worker only
    /// when the `ATERM_PARENT_SESSION_ID` every aterm session hands its shell
    /// names `sid`: a process whose variable names another session, or none,
    /// is not the worker.
    fn worker_proc(&self, group: u32, sid: &str) -> Result<WorkerEnv, String> {
        let args = atpkg::caller_shell::process_args(group).ok_or_else(|| {
            format!("the environment of {sid}'s foreground process {group} could not be read")
        })?;
        bound_to(WorkerEnv::new(args).with_pid(group), group, sid)
    }

    /// [`WorkerEnv::raw_git_view`], the worker's writable places judged with every
    /// path resolved (`canonicalize`), then PROJECTED ([`GitView::project`]):
    /// the policy judges the very view a tape can hold.
    fn git_view(
        &self,
        worker: &WorkerEnv,
        dir: &Path,
        writes: &WorkerWrites<'_>,
    ) -> Result<GitView, String> {
        let (worker, dir) = (worker.clone(), dir.to_path_buf());
        let (cwd, shell_cwds, scratch_roots) = (
            writes.cwd.to_path_buf(),
            writes.shell_cwds.to_vec(),
            writes.scratch_roots.to_vec(),
        );
        bounded(
            "a git read's configuration",
            move || {
                let writes = WorkerWrites {
                    cwd: &cwd,
                    shell_cwds: &shell_cwds,
                    scratch_roots: &scratch_roots,
                };
                let resolved =
                    |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
                worker
                    .raw_git_view(&dir, &|d| writes.holds(d, &resolved))
                    .map(|view| view.project())
            },
            Err,
        )
    }

    fn settings_env(&self, worker: &WorkerEnv, launch: &Path) -> Result<(), String> {
        let (worker, launch) = (worker.clone(), launch.to_path_buf());
        bounded(
            "the agent's settings",
            move || worker.git_settings_env(&launch),
            Err,
        )
    }

    fn shell_startup(&self, worker: &WorkerEnv, launch: &Path) -> Result<ShellStartup, String> {
        let (worker, launch) = (worker.clone(), launch.to_path_buf());
        bounded(
            "the shell's startup",
            move || super::shell_startup::read(&worker, &launch, cfg!(windows)),
            Err,
        )
    }

    fn owner_asks(&self, worker: &WorkerEnv, launch: &Path) -> Result<Vec<OwnerAsk>, String> {
        let (worker, launch) = (worker.clone(), launch.to_path_buf());
        bounded(
            "the agent's own ask rules",
            move || super::claude_settings::owner_asks(&worker, &launch),
            Err,
        )
    }

    /// Where the transcripts say the worker's Bash tool stands; a look that does not answer
    /// says anywhere (`/`), so every place counts as the worker's to write and nothing it could
    /// not read is approved.
    fn shell_cwds(&self, worker: &WorkerEnv, cwd: &Path) -> Vec<PathBuf> {
        let (worker, cwd) = (worker.clone(), cwd.to_path_buf());
        bounded(
            "where the worker's shell stands",
            move || {
                worker
                    .claude_dir(&cwd)
                    .map(|claude| crate::harness::footer::shell_cwds(&claude, &cwd))
                    .unwrap_or_default()
            },
            |_| vec![PathBuf::from("/")],
        )
    }

    fn unanswered(&self) -> u64 {
        UNANSWERED.with(Cell::get)
    }

    fn look_out(&self) -> bool {
        out_on_this_thread()
    }
}

/// `worker`, the process leading `group`, taken for session `sid`'s worker only when its `ATERM_PARENT_SESSION_ID` names `sid`.
pub(crate) fn bound_to(worker: WorkerEnv, group: u32, sid: &str) -> Result<WorkerEnv, String> {
    match worker.var(ENV_PARENT_SESSION_ID) {
        Some(tab) if tab == sid => Ok(worker),
        Some(_) => Err(format!(
            "{sid}'s foreground process {group} belongs to another session (its \
             {ENV_PARENT_SESSION_ID} names another)"
        )),
        None => Err(format!(
            "{sid}'s foreground process {group} names no session \
             ({ENV_PARENT_SESSION_ID} unset, or its environment hidden)"
        )),
    }
}

/// How long the approval policy waits for one look at the disk before it refuses what the look
/// would have proved: a hang guard, not a latency budget. No look itself has been timed under
/// load; the slowest harness step measured under load is an idle step whose relaunch snapshot
/// read took up to 57 s (control-socket round trips, process-table walks and argv reads, none
/// timed apart: `docs/measured/idle-look-under-load-2026-10-23.md`), and one look is a small part
/// of such work. A look still out past every step measured is taken for a volume that does not
/// answer (a dead network mount, an automount that never mounts), and a slow one that answers
/// after it has its box decided again ([`PolicyProbe::look_out`]).
pub(crate) const LOOK_BOUND: Duration = Duration::from_secs(60);

/// The helper threads the live probe's looks run on.
static LOOKS: aterm_uds::deadline::Gate = aterm_uds::deadline::Gate::new("aterm-policy-look");

thread_local! {
    /// Whether the look this thread (one session's loop) left out past its bound has ended.
    static LEFT_OUT: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
    /// How many looks this thread refused unanswered ([`PolicyProbe::unanswered`]).
    static UNANSWERED: Cell<u64> = const { Cell::new(0) };
}

/// One more look refused unanswered on this thread.
fn count_unanswered() {
    UNANSWERED.with(|n| n.set(n.get().wrapping_add(1)));
}

/// Whether a look this thread asked is still out past its bound.
fn out_on_this_thread() -> bool {
    LEFT_OUT.with(|out| {
        let mut out = out.borrow_mut();
        if out
            .as_ref()
            .is_some_and(|ended| ended.load(Ordering::Acquire))
        {
            *out = None;
        }
        out.is_some()
    })
}

/// `look` at the machine on a helper thread, waited for [`LOOK_BOUND`] at most: its answer, or
/// `refuse` with why there is none, so a look that cannot finish never approves. While a look
/// this loop left out has not ended, every other refuses at once and starts none: one volume
/// that does not answer costs the loop one bound, not one per look, and other sessions' loops
/// not even that. A late answer lands nowhere.
fn bounded<T: Send + 'static>(
    what: &str,
    look: impl FnOnce() -> T + Send + 'static,
    refuse: impl FnOnce(String) -> T,
) -> T {
    if out_on_this_thread() {
        count_unanswered();
        return refuse(format!(
            "{what}: an earlier look at the disk has not answered yet"
        ));
    }
    #[cfg(test)]
    let (bound, gate) = (
        tests::bound_of_this_thread().unwrap_or(LOOK_BOUND),
        tests::gate_of_this_thread(),
    );
    #[cfg(not(test))]
    let bound = LOOK_BOUND;
    let ended = Arc::new(AtomicBool::new(false));
    let ends = Arc::clone(&ended);
    // No helper is ever suspect: one session's look left out must not refuse another session's.
    let started = LOOKS.start(Duration::MAX, move || {
        /// The look is over as its thread returns or unwinds.
        struct Ends(Arc<AtomicBool>);
        impl Drop for Ends {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let _ends = Ends(ends);
        #[cfg(test)]
        gate();
        look()
    });
    match started.map(|pending| pending.wait(bound)) {
        Ok(Ok(Ok(answer))) => answer,
        Ok(Err(_late)) => {
            LEFT_OUT.with(|out| *out.borrow_mut() = Some(ended));
            count_unanswered();
            refuse(format!(
                "{what}: the disk did not answer within {} s",
                bound.as_secs_f64()
            ))
        }
        Ok(Ok(Err(why))) | Err(why) => refuse(format!("{what}: the look failed ({why:?})")),
    }
}

/// Maximum directory entries [`hidden_twin_within`] lists before deferring to the user.
pub(crate) const HIDDEN_TWIN_ENTRIES: usize = 20_000;

/// One folder's entry names, as [`hidden_twin_within`] reads them.
pub(crate) type FolderNames = dyn Iterator<Item = OsString>;

/// The real [`std::fs::read_dir`] reader for [`hidden_twin_within`].
pub(crate) fn folder_names(dir: &Path) -> std::io::Result<Box<FolderNames>> {
    let entries = std::fs::read_dir(dir)?;
    Ok(Box::new(entries.flatten().map(|e| e.file_name())))
}

/// Whether `dir` lies at or under a root macOS guards with a consent dialog that
/// a held Full Disk Access grant does not cover or whose Allow is forgotten when
/// the responsible process exits (`~/Library/Containers`,
/// `~/Library/Group Containers`, `~/Library/CloudStorage`,
/// `~/Library/Mobile Documents`), as spelled or after resolving symlinks.
fn never_listed(dir: &Path, home: Option<&Path>, real_home: Option<&Path>) -> bool {
    let Some(home) = home else {
        return false;
    };
    let under = |h: &Path, p: &Path| {
        atpkg::protected::under_app_data(h, p) || atpkg::protected::under_file_provider_domain(h, p)
    };
    if under(home, dir) || real_home.is_some_and(|rh| under(rh, dir)) {
        return true;
    }
    let Ok(real_dir) = std::fs::canonicalize(dir) else {
        return false;
    };
    under(home, &real_dir) || real_home.is_some_and(|rh| under(rh, &real_dir))
}

/// An entry of a folder on `comps`' way whose name differs from the one the box draws only by characters the box may not have drawn.
pub(crate) fn hidden_twin_within(
    comps: &[String],
    max_entries: usize,
    home: Option<&Path>,
    names: &dyn Fn(&Path) -> std::io::Result<Box<FolderNames>>,
) -> Result<Option<String>, String> {
    let real_home = home.and_then(|h| std::fs::canonicalize(h).ok());
    let mut dir = PathBuf::from("/");
    for comp in comps {
        if never_listed(&dir, home, real_home.as_deref()) {
            return Err(format!(
                "a Read whose folder is under another app's data or a file-provider domain and is never listed: {dir:?}"
            ));
        }
        match names(&dir) {
            Ok(entries) => {
                for (n, file_name) in entries.enumerate() {
                    if n >= max_entries {
                        return Err(format!(
                            "a Read in a folder too big to prove (over {max_entries} entries): {dir:?}"
                        ));
                    }
                    let name = file_name.to_string_lossy().into_owned();
                    let shown: String = name.chars().filter(|c| (' '..='~').contains(c)).collect();
                    if name != shown && shown.eq_ignore_ascii_case(comp) {
                        return Ok(Some(dir.join(&name).to_string_lossy().into_owned()));
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(format!(
                    "a Read whose folder cannot be listed ({e}): {dir:?}"
                ));
            }
        }
        dir.push(comp);
        if never_listed(&dir, home, real_home.as_deref()) {
            return Err(format!(
                "a Read whose folder is under another app's data or a file-provider domain and is never listed: {dir:?}"
            ));
        }
        match std::fs::metadata(&dir) {
            Ok(m) if m.is_dir() => {}
            _ => return Ok(None),
        }
    }
    Ok(None)
}

/// `comps` with every symlink resolved: the deepest existing ancestor
/// canonicalized, plus the not-yet-existing components as written.
fn real_components(comps: &[String]) -> Option<Vec<String>> {
    let mut k = comps.len();
    loop {
        let p = format!("/{}", comps[..k].join("/"));
        match std::fs::canonicalize(&p) {
            Ok(real) => {
                let mut out = abs_components(&real.to_string_lossy())?;
                out.extend(comps[k..].iter().cloned());
                return Some(out);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && k > 0 => {
                if std::fs::symlink_metadata(&p).is_ok() {
                    return None;
                }
                k -= 1;
            }
            Err(_) => return None,
        }
    }
}

/// Most entries [`below_within`] reads below one tree before it refuses the tree as too big to
/// prove: [`HIDDEN_TWIN_ENTRIES`]' bound, counted over the whole walk.
pub(crate) const TREE_ENTRIES: usize = HIDDEN_TWIN_ENTRIES;

/// What a reader that descends the absolute path `root` may open within `below` levels of it
/// (`usize::MAX`: any), walked as the most permissive reader walks it. Nothing there is
/// [`Below::Missing`]. Else the tree's own path, every name below it (a directory's only where a
/// reader enters it, above the last level) and, for each symbolic link, where it leads (a link to
/// a directory entered as that directory) are held to `secrets` by [`walk_hit`], each path as the
/// disk resolves it and every `Under` rule also as the disk resolves its directory
/// ([`real_components`]); a hit is [`Below::Secret`], naming that path and its rule, and none is
/// [`Below::Clear`].
///
/// Every link is followed because some reader follows it: MEASURED 2026-11-05 on macOS 27.0
/// (`docs/measured/classifier-tree-reach-2026-11-05.md`), macOS grep's `-S` and `find … -exec cat
/// {} +` printed a secret through a link below the tree that its plain `-r` did not, and `diff -r`
/// compared a linked file; rg's `-L` followed both a file link and a directory link.
///
/// # Errors
///
/// The walk was cut, so the tree is unproven: a folder that cannot be listed or lies where it is
/// never listed ([`never_listed`]), more than `max_entries` entries, a path or link that cannot
/// be resolved, or a Windows host, whose paths this walk does not spell.
pub(crate) fn below_within(
    root: &[String],
    below: usize,
    secrets: &[SecretRule],
    home: Option<&Path>,
    max_entries: usize,
) -> Result<Below, String> {
    if cfg!(windows) {
        return Err("a tree on Windows, whose files this check does not list".to_string());
    }
    let tree = PathBuf::from(format!("/{}", root.join("/")));
    let real_home = home.and_then(|h| std::fs::canonicalize(h).ok());
    let mut rules = secrets.to_vec();
    rules.extend(secrets.iter().filter_map(|rule| {
        match rule {
            SecretRule::Under(dir) => abs_components(&dir.to_string_lossy())
                .and_then(|comps| real_components(&comps))
                .map(|real| SecretRule::Under(PathBuf::from(format!("/{}", real.join("/"))))),
            SecretRule::Component(_) | SecretRule::Basename(_) => None,
        }
    }));
    let met = |path: &Path, dir: bool| {
        let comps = abs_components(&path.to_string_lossy())?;
        walk_hit(&comps, &rules, dir).map(|rule| format!("{} ({rule:?})", path.display()))
    };
    let gone =
        |e: &std::io::Error| matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory);
    let is_dir = |p: &Path| {
        std::fs::metadata(p).map(|m| m.is_dir()).map_err(|e| {
            format!(
                "a path below a tree that cannot be examined ({e}): {}",
                p.display()
            )
        })
    };
    let real = match std::fs::canonicalize(&tree) {
        Ok(real) => real,
        Err(e) if gone(&e) => return Ok(Below::Missing),
        Err(e) => {
            return Err(format!(
                "a tree that cannot be resolved ({e}): {}",
                tree.display()
            ));
        }
    };
    let root_dir = is_dir(&real)?;
    if let Some(hit) = met(&real, root_dir) {
        return Ok(Below::Secret(hit));
    }
    if !root_dir || below == 0 {
        return Ok(Below::Clear);
    }
    let mut queue = VecDeque::from([(real.clone(), 0usize)]);
    let mut entered = HashSet::from([real]);
    let mut entries = 0usize;
    while let Some((dir, depth)) = queue.pop_front() {
        if never_listed(&dir, home, real_home.as_deref()) {
            return Err(format!(
                "a tree a reader descends into another app's data or a file-provider domain, \
                 which is never listed: {}",
                dir.display()
            ));
        }
        let cut = |e: std::io::Error| {
            format!(
                "a folder below a tree that cannot be listed ({e}): {}",
                dir.display()
            )
        };
        let level = depth + 1;
        for entry in std::fs::read_dir(&dir).map_err(cut)? {
            let entry = entry.map_err(cut)?;
            entries += 1;
            if entries > max_entries {
                return Err(format!(
                    "a tree too big to prove (over {max_entries} entries): {}",
                    tree.display()
                ));
            }
            let path = dir.join(entry.file_name());
            let kind = entry.file_type().map_err(cut)?;
            let (lands, dir_like) = if kind.is_symlink() {
                match std::fs::canonicalize(&path) {
                    Ok(target) => {
                        let d = is_dir(&target)?;
                        (target, d)
                    }
                    // A link that names nothing opens nothing.
                    Err(e) if gone(&e) => continue,
                    Err(e) => {
                        return Err(format!(
                            "a symbolic link below a tree that cannot be resolved ({e}): {}",
                            path.display()
                        ));
                    }
                }
            } else {
                (path.clone(), kind.is_dir())
            };
            // A directory on the last level is entered by no reader that stops there.
            if dir_like && level >= below {
                continue;
            }
            if let Some(hit) = met(&path, dir_like).or_else(|| met(&lands, dir_like)) {
                return Ok(Below::Secret(hit));
            }
            if dir_like && entered.insert(lands.clone()) {
                queue.push_back((lands, level));
            }
        }
    }
    Ok(Below::Clear)
}

/// How many symbolic links [`physical`] follows for one path before it gives
/// up (the kernel's own limit is of this order: `MAXSYMLINKS` is 32 on macOS).
const MAX_LINK_HOPS: usize = 32;

/// Where the absolute path `comps` leads on the disk, walked as the kernel
/// walks it: each component that is a symbolic link replaced by its target
/// (an absolute target from `/`, a relative one from the link's directory, a
/// `..` in it the physical parent) until one does not exist, and from there
/// the rest as written, since nothing below a missing directory can be a
/// link yet.
fn physical(comps: &[String]) -> Result<OnDisk, String> {
    let mut todo: VecDeque<OsString> = comps.iter().map(OsString::from).collect();
    let mut real = PathBuf::from("/");
    let mut hops = 0usize;
    let mut foreign_link = false;
    while let Some(c) = todo.pop_front() {
        if c.is_empty() || c == "." {
            continue;
        }
        if c == ".." {
            real.pop();
            continue;
        }
        let path = real.join(&c);
        match std::fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_symlink() => {
                hops += 1;
                if hops > MAX_LINK_HOPS {
                    return Err(format!(
                        "{} leads through more than {MAX_LINK_HOPS} symbolic links",
                        path.display()
                    ));
                }
                foreign_link |= !owned_by_root(&m);
                let to = std::fs::read_link(&path).map_err(|e| {
                    format!("the symbolic link {} cannot be read ({e})", path.display())
                })?;
                if to.has_root() {
                    real = PathBuf::from("/");
                }
                for part in to.components().rev() {
                    match part {
                        Component::Normal(n) => todo.push_front(n.to_os_string()),
                        Component::ParentDir => todo.push_front(OsString::from("..")),
                        Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
                    }
                }
            }
            Ok(_) => real.push(&c),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                real.push(&c);
                while let Some(rest) = todo.pop_front() {
                    if rest == ".." {
                        return Err(format!(
                            "{} does not exist, and a symbolic link's `..` climbs out of it",
                            path.display()
                        ));
                    }
                    if !rest.is_empty() && rest != "." {
                        real.push(&rest);
                    }
                }
            }
            Err(e) => {
                return Err(format!(
                    "{} cannot be examined on the disk ({e})",
                    path.display()
                ));
            }
        }
    }
    let comps = real
        .components()
        .filter_map(|c| match c {
            Component::Normal(n) => Some(n.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    Ok(OnDisk {
        comps,
        foreign_link,
    })
}

/// Whether root owns this link: then no one else made it or can re-point it
/// (macOS's `/tmp -> private/tmp`, `/var -> private/var`).
#[cfg(unix)]
fn owned_by_root(m: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    m.uid() == 0
}

#[cfg(not(unix))]
fn owned_by_root(_: &std::fs::Metadata) -> bool {
    false
}

#[cfg(test)]
#[path = "probe_live_tests.rs"]
pub(crate) mod tests;
