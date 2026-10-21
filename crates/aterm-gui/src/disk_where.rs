// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHERE THE SPACE IS: the Disk lens's read-only survey
//! (`docs/DESIGN-stats-deck-v2-2026-10-20.md` §7.4).
//!
//! When the Disk lens opens ([`crate::App::disk_lens_opened`]) and no survey
//! started in the last hour (on the wall clock as well as the monotonic one,
//! so time asleep counts), a thread of its own, at a low QoS and never the
//! loop's or the vitals probe's, finds the build directories under `$HOME`,
//! the open panes' working directories and the agents' `CARGO_TARGET_DIR`s
//! (`aterm_agent::harness::disk::discover::discover` at the shipped bounds,
//! which never enters `HOME_SKIP`'s guarded folders) within half its 20 s
//! budget, then sizes them
//! (`aterm_agent::harness::disk::target::total_bytes_until`) until the budget
//! is spent. Three or more that sit in one folder under `$HOME` make one row
//! (`~/aterm-worktrees, 9 target folders`), and the largest eight rows are
//! kept as the deck's `Hog`s. A size its walk cut short is a floor
//! (`partial`, printed "≥ 96 GB"); a survey whose search or sizing ran out of
//! time says so (`cut`).
//!
//! A working directory or build directory is a root only where its spelling
//! alone places it on `$HOME`'s own local volume and outside every place
//! never looked into ([`roots`]): a path on a removable or network volume, or
//! in a privacy-guarded folder, is never looked at, not even resolved, since
//! the look can raise a consent dialog on the owner's screen or block on a
//! stalled mount.
//!
//! It only reads: nothing is removed, and it runs whether or not the harness
//! is on. It is not the disk watch, so its walks beat nothing
//! (`aterm_agent::harness::disk::beat::quietly`): a stalled watch is not read
//! as alive while a survey runs. The vitals probe copies the rows, and whether
//! a survey runs, into each reading's `DiskFacts` ([`Where::read`]); a cleanup
//! pass's own hog list wins unless the survey is newer and whole
//! (`crate::vitals_host`).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_agent::harness::disk::discover::{self, Budget, Origin, Roots, checkout};
use aterm_sysprobe::volumes::Table;
use aterm_vitals::model::Hog;

/// A survey starts at most once an hour.
pub(crate) const EVERY: Duration = Duration::from_secs(3600);
/// What one survey may spend, finding and sizing together.
pub(crate) const BUDGET: Duration = Duration::from_secs(20);
/// What finding may spend of it: the rest is the sizing's.
const FIND_BUDGET: Duration = Duration::from_secs(10);
/// The rows kept.
pub(crate) const ROWS: usize = 8;
/// Build directories sharing a folder under `$HOME` that print as one row.
const GROUP_AT: usize = 3;
/// The survey thread's name.
pub(crate) const THREAD: &str = "aterm-disk-where";

/// What a survey found: its rows, largest first, when it ended, and whether
/// it ran out of time.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Found {
    /// The rows, at most [`ROWS`].
    pub(crate) hogs: Vec<Hog>,
    /// When it ended, unix ms (`None` where the wall clock is unread).
    pub(crate) at_unix_ms: Option<i64>,
    /// Its home/device could not be admitted, or its search or sizing did
    /// not complete: the rows may miss the largest folders.
    pub(crate) cut: bool,
}

#[derive(Debug, Default)]
struct State {
    /// The last survey's rows.
    last: Option<Found>,
    /// A survey runs now.
    running: bool,
    /// When the last survey started, and its wall-clock unix ms where read.
    started: Option<(Instant, Option<i64>)>,
}

/// The survey's slot: the loop claims it to start one, the survey's thread
/// leaves its rows in it, and the vitals probe reads them. Its lock is a
/// leaf, held only to copy.
#[derive(Debug, Default)]
pub(crate) struct Where {
    state: Mutex<State>,
}

impl Where {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Claim a survey at `now` (`wall_ms` on the wall clock, where read):
    /// true, and running from now, when none runs and none started within
    /// [`EVERY`] of `now` on either clock. The monotonic clock stands still
    /// while the Mac sleeps; the wall clock counts the night.
    fn claim(&self, now: Instant, wall_ms: Option<i64>) -> bool {
        let mut s = self.lock();
        let every_ms = EVERY.as_millis() as i64;
        let due = s.started.is_none_or(|(t, wall)| {
            now.saturating_duration_since(t) >= EVERY
                || wall
                    .zip(wall_ms)
                    .is_some_and(|(then, at)| at.saturating_sub(then) >= every_ms)
        });
        if s.running || !due {
            return false;
        }
        s.running = true;
        s.started = Some((now, wall_ms));
        true
    }

    /// The last survey's rows, and whether one runs now.
    pub(crate) fn read(&self) -> (Option<Found>, bool) {
        let s = self.lock();
        (s.last.clone(), s.running)
    }
}

/// A running survey's end, however its thread ends: one that panics frees
/// the slot too, and the last rows stay.
struct Running {
    slot: Arc<Where>,
    found: Option<Found>,
}

impl Drop for Running {
    fn drop(&mut self) {
        let mut s = self.slot.lock();
        if let Some(found) = self.found.take() {
            s.last = Some(found);
        }
        s.running = false;
    }
}

/// Where a survey looks, as the loop knows it: `$HOME`, and the processes
/// whose working directories and `CARGO_TARGET_DIR`s it reads on its own
/// thread (a process's directory and environment are the kernel's to read,
/// never the loop's).
#[derive(Clone, Debug, Default)]
pub(crate) struct Ask {
    /// `$HOME`.
    pub(crate) home: Option<PathBuf>,
    /// Each tab's shell: its working directory is a root.
    pub(crate) shells: Vec<u32>,
    /// Each agent in front of a tab (its group's leader): its working
    /// directory and its `CARGO_TARGET_DIR` are roots.
    pub(crate) agents: Vec<u32>,
}

/// Start a survey of `ask` on its own thread at `now`, unless one runs or
/// one started within the hour. Whether one started.
pub(crate) fn start(slot: &Arc<Where>, ask: Ask, now: Instant, wall_ms: Option<i64>) -> bool {
    // A unit test's App opens the Disk lens too: its survey finds nothing
    // rather than walking the developer's home.
    start_with(slot, (now, wall_ms), move || {
        if cfg!(test) {
            Found::default()
        } else {
            survey(&ask)
        }
    })
    .is_some()
}

/// [`start`] with the survey `run` stands for: the thread's handle where one
/// started.
fn start_with(
    slot: &Arc<Where>,
    (now, wall_ms): (Instant, Option<i64>),
    run: impl FnOnce() -> Found + Send + 'static,
) -> Option<std::thread::JoinHandle<()>> {
    if !slot.claim(now, wall_ms) {
        return None;
    }
    // Below the loop and the probe: it is useful work nobody is blocked on,
    // and the lens shows the last rows while it runs.
    let spawned = crate::qos::spawn_lane_thread(THREAD, crate::qos::Role::Background, {
        let slot = Arc::clone(slot);
        move || {
            let mut end = Running { slot, found: None };
            end.found = Some(run());
        }
    });
    match spawned {
        Ok(handle) => Some(handle),
        Err(error) => {
            // No thread, no guard: the claim ends here. The hour stands, so
            // a machine out of threads is not asked again at every open.
            aterm_log::warn!("disk survey: the survey thread did not start: {error}");
            slot.lock().running = false;
            None
        }
    }
}

/// One survey of `ask`, on the survey's thread, beating nothing: the roots
/// read now, the build directories found within [`FIND_BUDGET`] and sized
/// until [`BUDGET`] is spent.
fn survey(ask: &Ask) -> Found {
    aterm_agent::harness::disk::beat::quietly(|| {
        let deadline = Instant::now() + BUDGET;
        let table = aterm_sysprobe::volumes::table();
        // Admit the spelling before resolving it: a remote or unknown
        // HOME must not become the walk's measured device by being read.
        let home = ask
            .home
            .as_deref()
            .filter(|h| placed(h, h, &table))
            .and_then(|h| std::fs::canonicalize(h).ok())
            .filter(|h| placed(h, h, &table));
        let (hogs, cut) = match home {
            Some(home) => {
                let Some(device) = discover::device_of(&home) else {
                    return Found {
                        cut: true,
                        ..Found::default()
                    };
                };
                let cwds: Vec<PathBuf> = ask
                    .shells
                    .iter()
                    .chain(&ask.agents)
                    .filter_map(|&pid| aterm_sysprobe::process_cwd(pid))
                    .collect();
                let targets: Vec<PathBuf> = ask
                    .agents
                    .iter()
                    .filter_map(|&pid| atpkg::caller_shell::process_args(pid))
                    .filter_map(|args| args.env_var("CARGO_TARGET_DIR").map(PathBuf::from))
                    .collect();
                let Some(roots) = roots(&home, cwds, targets, &table, device) else {
                    return Found {
                        cut: true,
                        ..Found::default()
                    };
                };
                let found = discover::discover(
                    &roots,
                    Budget {
                        wall: FIND_BUDGET,
                        ..Budget::SHIPPED
                    },
                );
                let (hogs, spent) = rows(
                    &found.found,
                    &home,
                    || Instant::now() >= deadline,
                    |dir| aterm_agent::harness::disk::target::total_bytes_until(dir, deadline),
                );
                (hogs, spent || found.cut.is_some())
            }
            // An unread or unplaced HOME is not a completed empty survey.
            None => {
                return Found {
                    cut: true,
                    ..Found::default()
                };
            }
        };
        Found {
            hogs,
            at_unix_ms: aterm_time::unix_millis().and_then(|ms| i64::try_from(ms).ok()),
            cut,
        }
    })
}

/// Where the survey looks under `home` (canonical): its checkouts, the
/// working directories `cwds` and the build directories `targets`, each only
/// where its spelling places it ([`placed`]), on `home`'s device, never
/// entering a mount point of `table`. No Claude Code scratch root: the
/// survey reports the folders a person keeps. `device` was read only after
/// home admission; an unplaced home gives no roots. This helper reads no path.
fn roots(
    home: &Path,
    cwds: Vec<PathBuf>,
    targets: Vec<PathBuf>,
    table: &Table,
    device: u64,
) -> Option<Roots> {
    if !placed(home, home, table) {
        return None;
    }
    let keep = |p: &PathBuf| placed(p, home, table);
    Some(Roots {
        cwds: cwds.into_iter().filter(keep).collect(),
        home: Some(home.to_path_buf()),
        agent_target_dirs: targets.into_iter().filter(keep).collect(),
        device: Some(device),
        mounts: table.points(),
        ..Roots::default()
    })
}

/// Whether the survey may look at `path`, by its spelling alone (nothing is
/// looked at, not even to resolve it): absolute, outside every place never
/// looked into ([`checkout::never_looked_at`]: `home`'s privacy-guarded
/// folders, where removable volumes mount), and on the mount `home` is on in
/// a complete `table`, one on a local device and not automounted. A path on
/// another volume (a USB disk, a network share, an automount) can raise a
/// consent dialog or block on a stalled server at the first look.
fn placed(path: &Path, home: &Path, table: &Table) -> bool {
    use aterm_sysprobe::volumes::mount_of;
    let local =
        |m: &aterm_sysprobe::volumes::Mount| m.local && !m.automounted && m.fstype != "autofs";
    path.is_absolute()
        && checkout::never_looked_at(path, Some(home)).is_none()
        && table.complete
        && mount_of(home, table).is_some_and(|on_home| {
            local(on_home) && mount_of(path, table).is_some_and(|m| m.on == on_home.on)
        })
}

/// The rows of the build directories `found` (canonical paths) under `home`:
/// each one not inside another sized by `size` in turn until `spent` says the
/// budget is gone, three or more that share a folder under `home` (the folder
/// holding each one's checkout) as one row, largest first, at most [`ROWS`];
/// and whether the budget ran out before every one was sized in full.
fn rows(
    found: &[discover::Found],
    home: &Path,
    mut spent: impl FnMut() -> bool,
    mut size: impl FnMut(&Path) -> (u64, bool),
) -> (Vec<Hog>, bool) {
    let outer = found.iter().filter(|f| {
        !found
            .iter()
            .any(|o| o.path != f.path && f.path.starts_with(&o.path))
    });
    let mut sized: Vec<(&discover::Found, u64, bool)> = Vec::new();
    for f in outer {
        if spent() {
            break;
        }
        let (bytes, partial) = size(&f.path);
        if bytes > 0 {
            sized.push((f, bytes, partial));
        }
    }
    let folder = |p: &Path| -> Option<PathBuf> {
        let folder = p.parent()?.parent()?;
        (folder.starts_with(home) && folder != home).then(|| folder.to_path_buf())
    };
    let mut groups: std::collections::BTreeMap<PathBuf, (usize, u64, bool)> =
        std::collections::BTreeMap::new();
    for (f, bytes, partial) in &sized {
        if let Some(key) = folder(&f.path) {
            let g = groups.entry(key).or_default();
            g.0 += 1;
            g.1 = g.1.saturating_add(*bytes);
            g.2 |= *partial;
        }
    }
    groups.retain(|_, g| g.0 >= GROUP_AT);
    let mut out: Vec<Hog> = groups
        .iter()
        .map(|(dir, &(n, bytes, partial))| Hog {
            path: shown(dir, home),
            bytes,
            partial,
            remedy: format!("{n} target folders"),
        })
        .collect();
    out.extend(
        sized
            .iter()
            .filter(|(f, _, _)| folder(&f.path).is_none_or(|k| !groups.contains_key(&k)))
            .map(|&(f, bytes, partial)| Hog {
                path: shown(&f.path, home),
                bytes,
                partial,
                remedy: remedy(&f.origin).to_owned(),
            }),
    );
    out.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
    out.truncate(ROWS);
    (out, spent())
}

/// What frees a build directory found by `origin`, in words.
fn remedy(origin: &Origin) -> &'static str {
    match origin {
        Origin::Bootstrap { slim: true, .. } => "x.py bootstrap: scripts/slim_build.py",
        Origin::Bootstrap { slim: false, .. } => "x.py bootstrap: `./x.py clean`",
        Origin::AgentTargetDir => "an agent's CARGO_TARGET_DIR: `targo clean`",
        Origin::BuildCache { .. } => "a workspace's Rust build cache: `targo clean` there",
        Origin::ClaudeScratch { .. } => "a Claude Code session's scratch build",
        Origin::Named | Origin::Checkout { .. } | Origin::Linked { .. } => {
            "cargo build output: `targo clean`"
        }
    }
}

/// `path` as the lens prints it: `~/aterm/target` under `home`, else whole.
pub(crate) fn shown(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rel) if !rel.as_os_str().is_empty() => format!("~/{}", rel.display()),
        _ => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A build directory cargo would lay: its signed tag and `bytes` of
    /// output in `deps/`.
    fn build_dir(dir: &Path, bytes: usize) {
        std::fs::create_dir_all(dir.join("deps")).expect("deps");
        std::fs::write(
            dir.join("CACHEDIR.TAG"),
            aterm_agent::harness::disk::CACHEDIR_SIGNATURE,
        )
        .expect("tag");
        std::fs::write(dir.join("deps").join("out.rlib"), vec![7u8; bytes]).expect("output");
    }

    /// Every path under `dir` and its length: what a survey must leave as it
    /// found it.
    fn tree(dir: &Path) -> Vec<(PathBuf, u64)> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for entry in std::fs::read_dir(&d).expect("readable").flatten() {
                let meta = entry.metadata().expect("metadata");
                if meta.is_dir() {
                    stack.push(entry.path());
                }
                out.push((entry.path(), meta.len()));
            }
        }
        out.sort();
        out
    }

    fn found(path: &Path) -> discover::Found {
        discover::Found {
            path: path.to_path_buf(),
            origin: Origin::Checkout {
                root: path.parent().expect("parent").to_path_buf(),
            },
        }
    }

    /// T-G15: at most one survey an hour on either clock, and never while
    /// one runs; each runs on its own thread, never the caller's (the loop)
    /// or the vitals probe's; and a survey that ends, or panics, frees the
    /// slot. An hour asleep counts: the wall clock went on while the
    /// monotonic one stood still.
    #[test]
    fn a_survey_runs_on_its_own_thread_at_most_hourly() {
        let slot = Arc::new(Where::default());
        let t0 = Instant::now();
        let w0 = 1_792_000_000_000_i64;
        let every_ms = EVERY.as_millis() as i64;
        let caller = std::thread::current().id();
        let handle = start_with(&slot, (t0, Some(w0)), move || {
            let me = std::thread::current();
            assert_ne!(me.id(), caller, "never on the caller's thread");
            assert_eq!(me.name(), Some(THREAD));
            Found {
                hogs: vec![Hog {
                    path: "~/a/target".into(),
                    bytes: 1,
                    ..Hog::default()
                }],
                at_unix_ms: Some(1),
                cut: false,
            }
        })
        .expect("the first survey starts");
        // Claimed until its thread ends: a second open starts nothing.
        assert!(start_with(&slot, (t0, Some(w0)), Found::default).is_none());
        handle.join().expect("the survey's assertions held");
        let (last, running) = slot.read();
        assert!(!running);
        assert_eq!(last.expect("its rows").hogs[0].path, "~/a/target");

        // Within the hour of the last start on both clocks, or on the one
        // read: none.
        let within = t0 + EVERY - Duration::from_secs(1);
        let within_ms = w0 + every_ms - 1_000;
        assert!(start_with(&slot, (within, Some(within_ms)), Found::default).is_none());
        assert!(start_with(&slot, (within, None), Found::default).is_none());
        // An hour on, one; a survey that panics keeps the last rows and frees
        // the slot for the next hour.
        let later = t0 + EVERY;
        let handle = start_with(&slot, (later, Some(within_ms)), || {
            panic!("a survey that fails")
        })
        .expect("an hour on, another");
        assert!(handle.join().is_err());
        let (last, running) = slot.read();
        assert!(!running);
        assert_eq!(last.expect("kept").hogs.len(), 1);
        assert!(start_with(&slot, (later, Some(within_ms)), Found::default).is_none());
        // A night asleep: a second on the monotonic clock, an hour on the
        // wall clock, and a survey is due.
        let woke = later + Duration::from_secs(1);
        let handle = start_with(&slot, (woke, Some(within_ms + every_ms)), Found::default)
            .expect("an hour on the wall clock, another");
        handle.join().expect("the survey ends");
    }

    /// A git worktree of the repository at `repo`, named `name`, at `wt`: its
    /// `.git` file and the repository's `gitdir` naming it.
    fn worktree(repo: &Path, name: &str, wt: &Path) {
        let record = repo.join(".git").join("worktrees").join(name);
        std::fs::create_dir_all(&record).expect("worktree record");
        std::fs::create_dir_all(wt).expect("worktree");
        std::fs::write(wt.join(".git"), "gitdir: x\n").expect(".git file");
        std::fs::write(
            record.join("gitdir"),
            format!("{}\n", wt.join(".git").display()),
        )
        .expect("gitdir");
    }

    /// T-G15: a survey of a real tree finds and sizes its build directories,
    /// groups three under one folder into a row, finds a checkout's worktree
    /// outside `$HOME`, never looks into `$HOME`'s guarded folders (not even
    /// through a tab working there, or a worktree a checkout's gitdir puts
    /// there), and removes nothing.
    #[test]
    fn a_survey_reads_only_and_never_enters_guarded_folders() {
        let dir = aterm_tempfile::tempdir().expect("scratch home");
        let root = std::fs::canonicalize(dir.path()).expect("canonical root");
        let home = root.join("home");
        let guarded = checkout::HOME_SKIP[0];
        build_dir(&home.join("proj").join("target"), 40_000);
        for wt in ["a", "b", "c"] {
            build_dir(&home.join("wt").join(wt).join("target"), 10_000);
        }
        let guarded_dir = home.join(guarded).join("proj");
        build_dir(&guarded_dir.join("target"), 90_000);
        // The checkout's worktrees: one beside `$HOME`, one in a guarded
        // folder of it.
        let outside = root.join("elsewhere").join("wt");
        worktree(&home.join("proj"), "out", &outside);
        build_dir(&outside.join("target"), 20_000);
        let inside = home.join(guarded).join("wt");
        worktree(&home.join("proj"), "in", &inside);
        build_dir(&inside.join("target"), 80_000);
        let before = tree(&root);

        let roots = roots(
            &home,
            vec![guarded_dir.clone(), PathBuf::from("relative")],
            vec![guarded_dir.join("target")],
            &aterm_sysprobe::volumes::table(),
            discover::device_of(&home).expect("scratch home's device"),
        )
        .expect("scratch home is on a known local volume");
        assert!(roots.cwds.is_empty() && roots.agent_target_dirs.is_empty());
        let found = discover::discover(&roots, Budget::SHIPPED);
        let (hogs, cut) = rows(
            &found.found,
            &home,
            || false,
            aterm_agent::harness::disk::target::total_bytes,
        );
        assert!(!cut);
        let paths: Vec<&str> = hogs.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths.len(), 3, "{hogs:?}");
        assert!(paths.contains(&"~/proj/target"), "{hogs:?}");
        let beside = outside.join("target").display().to_string();
        assert!(paths.contains(&beside.as_str()), "{hogs:?}");
        let wt = hogs.iter().find(|h| h.path == "~/wt").expect("grouped");
        assert_eq!(wt.remedy, "3 target folders");
        assert!(wt.bytes >= 30_000, "{wt:?}");
        assert!(
            !paths.iter().any(|p| p.contains(guarded)),
            "a guarded folder was entered: {hogs:?}"
        );
        assert_eq!(tree(&root), before, "the survey removed or changed nothing");
    }

    /// T-G15: a working or build directory is a root only where its spelling
    /// alone places it on `$HOME`'s own local volume, outside the places never
    /// looked into; nothing is placed by an incomplete mount table, or when
    /// `$HOME` itself is on a network volume. NEGATIVE CONTROL: a directory
    /// under `$HOME` on its volume is kept.
    #[test]
    fn roots_are_only_on_homes_own_local_volume() {
        use aterm_sysprobe::volumes::Mount;
        let mount = |on: &str, local: bool, automounted: bool, fstype: &str| Mount {
            on: PathBuf::from(on),
            from: String::new(),
            fstype: fstype.to_owned(),
            local,
            automounted,
            removable: false,
            read_only: false,
            container: None,
        };
        let table = Table {
            mounts: vec![
                mount("/", true, false, "apfs"),
                mount("/Volumes/USB", true, false, "msdos"),
                mount("/Users//x/share", false, false, "nfs"),
                mount("/net", false, true, "autofs"),
            ],
            complete: true,
            firmlinks: Vec::new(),
        };
        // Captured placement and device: no home or remote path is read.
        let home = Path::new("/Users//x");
        let kept = |p: &str| placed(Path::new(p), home, &table);
        assert!(kept("/Users//x/proj"), "on home's own volume");
        assert!(kept("/srv/build/target"), "elsewhere on home's volume");
        for p in [
            "/Volumes/USB/proj",
            "/Users//x/share/proj",
            "/net/server/proj",
            "relative/proj",
            "/Users//x/../y/proj",
        ] {
            assert!(!kept(p), "{p}");
        }
        let guarded = format!("/Users//x/{}/proj", checkout::HOME_SKIP[0]);
        assert!(!kept(&guarded), "{guarded}");
        let incomplete = Table {
            complete: false,
            ..table.clone()
        };
        assert!(!placed(Path::new("/Users//x/proj"), home, &incomplete));
        let remote_home = Path::new("/Users//x/share/me");
        assert!(!placed(
            Path::new("/Users//x/share/me/proj"),
            remote_home,
            &table
        ));
        let roots = roots(
            home,
            vec![
                PathBuf::from("/Users//x/proj"),
                PathBuf::from("/Volumes/USB/proj"),
            ],
            vec![PathBuf::from("/net/server/target")],
            &table,
            7,
        )
        .expect("captured local home");
        assert_eq!(roots.home.as_deref(), Some(home));
        assert_eq!(roots.device, Some(7));
        assert_eq!(roots.cwds, [PathBuf::from("/Users//x/proj")]);
        assert!(roots.agent_target_dirs.is_empty());
        assert!(super::roots(home, Vec::new(), Vec::new(), &incomplete, 7).is_none());
        assert!(super::roots(remote_home, Vec::new(), Vec::new(), &table, 7).is_none());
        for unplaced in [Path::new("/net/me"), Path::new("relative")] {
            assert!(super::roots(unplaced, Vec::new(), Vec::new(), &table, 7).is_none());
        }
    }

    /// T-G15: the sizing stops once its budget is spent and keeps what it
    /// sized; a size its walk cut short stays a floor ("≥"); a directory
    /// inside another is sized with it, never twice; and only the largest
    /// eight rows are kept, largest first.
    #[test]
    fn sizing_stops_at_its_budget_and_keeps_partial_sizes() {
        let home = Path::new("/h");
        let dirs: Vec<PathBuf> = (0..12).map(|i| home.join(format!("p{i}/target"))).collect();
        let mut all: Vec<discover::Found> = dirs.iter().map(|d| found(d)).collect();
        all.push(found(&home.join("p0/target/nested")));

        let mut asked = Vec::new();
        let mut calls = 0;
        let (hogs, cut) = rows(
            &all,
            home,
            || {
                calls += 1;
                calls > 3
            },
            |p| {
                asked.push(p.to_path_buf());
                (1000 * asked.len() as u64, p.ends_with("p1/target"))
            },
        );
        assert_eq!(
            asked,
            dirs[..3],
            "nothing is sized once the budget is spent"
        );
        assert!(cut, "a survey whose budget ran out says so");
        assert_eq!(
            hogs.iter()
                .map(|h| (h.path.as_str(), h.bytes, h.partial))
                .collect::<Vec<_>>(),
            [
                ("~/p2/target", 3000, false),
                ("~/p1/target", 2000, true),
                ("~/p0/target", 1000, false),
            ]
        );
        // Unbounded, twelve directories keep eight rows.
        let (hogs, cut) = rows(
            &all,
            home,
            || false,
            |p| (p.as_os_str().len() as u64, false),
        );
        assert!(!cut);
        assert_eq!(hogs.len(), ROWS);
        assert!(hogs.windows(2).all(|w| w[0].bytes >= w[1].bytes));
    }
}
