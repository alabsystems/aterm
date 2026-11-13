// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Background read-only disk-space survey for the Disk stats lens.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_agent::harness::disk::discover::{self, Budget, Origin, Roots, checkout};
use aterm_sysprobe::volumes::Table;
use aterm_vitals::model::Hog;

/// Minimum interval between disk surveys.
pub(crate) const EVERY: Duration = Duration::from_secs(3600);
/// Total wall-clock budget for discovering and sizing build directories.
pub(crate) const BUDGET: Duration = Duration::from_secs(20);
const FIND_BUDGET: Duration = Duration::from_secs(10);
/// Maximum number of largest build-directory rows retained.
pub(crate) const ROWS: usize = 8;
const GROUP_AT: usize = 3;
/// Survey worker thread name.
pub(crate) const THREAD: &str = "aterm-disk-where";

/// Completed disk survey results.
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

/// Shared slot holding the latest disk survey results and running flag.
#[derive(Debug, Default)]
pub(crate) struct Where {
    state: Mutex<State>,
}

impl Where {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

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

    /// Return the latest survey results and whether a survey is currently running.
    pub(crate) fn read(&self) -> (Option<Found>, bool) {
        let s = self.lock();
        (s.last.clone(), s.running)
    }
}

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

/// Root paths and process PIDs inspected by a disk survey.
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

/// Start a background disk survey on a dedicated thread if none is running or ran within [`EVERY`].
pub(crate) fn start(slot: &Arc<Where>, ask: Ask, now: Instant, wall_ms: Option<i64>) -> bool {
    start_with(slot, (now, wall_ms), move || {
        if cfg!(test) {
            Found::default()
        } else {
            survey(&ask)
        }
    })
    .is_some()
}

fn start_with(
    slot: &Arc<Where>,
    (now, wall_ms): (Instant, Option<i64>),
    run: impl FnOnce() -> Found + Send + 'static,
) -> Option<std::thread::JoinHandle<()>> {
    if !slot.claim(now, wall_ms) {
        return None;
    }
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
            aterm_log::warn!("disk survey: the survey thread did not start: {error}");
            slot.lock().running = false;
            None
        }
    }
}

fn survey(ask: &Ask) -> Found {
    let deadline = Instant::now() + BUDGET;
    let table = aterm_sysprobe::volumes::table();
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
            found_rows(
                &found,
                &home,
                || Instant::now() >= deadline,
                |dir| aterm_agent::harness::disk::target::total_bytes_until(dir, deadline),
            )
        }
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
}

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
        // Windows' walk stays on this volume and enters nothing without it.
        volume: aterm_agent::harness::disk::target::VolumeGuid::of_path(home, table),
        mounts: table.points(),
        ..Roots::default()
    })
}

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

/// The rows of a discovery ([`rows`]), and whether they are cut: the sizing ran out of time or
/// the search itself was cut short.
fn found_rows(
    found: &discover::Discovered,
    home: &Path,
    spent: impl FnMut() -> bool,
    size: impl FnMut(&Path) -> (u64, bool),
) -> (Vec<Hog>, bool) {
    let (hogs, spent) = rows(&found.found, home, spent, size);
    (hogs, spent || found.cut.is_some())
}

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

const fn remedy(origin: &Origin) -> &'static str {
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

/// Format `path` for display (`~/…` when under `home`, else full path).
pub(crate) fn shown(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rel) if !rel.as_os_str().is_empty() => {
            let names: Vec<_> = rel
                .components()
                .map(|name| name.as_os_str().to_string_lossy())
                .collect();
            format!("~/{}", names.join("/"))
        }
        _ => aterm_sysprobe::paths::plain(path).display().to_string(),
    }
}

#[cfg(test)]
#[path = "disk_where_tests.rs"]
mod tests;
