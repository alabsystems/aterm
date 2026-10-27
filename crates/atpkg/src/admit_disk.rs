// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DISK A HEAVY RUN WILL WRITE, CLAIMED BEFORE IT STARTS (`docs/DESIGN-adaptive-build-2026-10-24.md`,
//! DISK-ADMIT). verify, `ship trust run` and the lab's VM boots each claim, in the capacity
//! ledger (`aterm_capacity`), what their last runs of that kind wrote ([`aterm_capacity::Footprints`]),
//! or, where none was measured here, the kind's cold budget less what its directories hold. A
//! volume short of it is relieved first: the build cache's pass under pressure
//! ([`crate::build_cache::RECLAIM_GONE_UNDER_PRESSURE`]) and, inside aterm, the window's disk
//! watch asked for the room (`await sys free>=`). Then the run waits, saying once a minute what
//! it needs, what is free and what holds the most, instead of failing at `ENOSPC` hours in.
//! SIGINT, SIGTERM or SIGHUP ends the claim, its measuring and its relief within a second
//! ([`aterm_toolchain::interrupt`]), with [`Error::Stopped`] and nothing left in the ledger.
//! While it runs, its own measure of its writes is its claim's progress
//! ([`aterm_capacity::Meter`]), and what it took is kept for the next run.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use aterm_capacity::{
    Footprints, Ledger, Live, Machine, Meter, Need, Pace, Relief, Short, Shortfall, admit_relieved,
    allocated_until,
};

/// What a caller passes and reads back, so verify, the trust lane and the lab claim through
/// this module alone.
pub use aterm_capacity::{Admit, Error, Estimate, Footprint, gib, size};

/// One run's disk, as it is claimed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    /// What the run is, as a wait line names it: `verify lint test -p atpkg`.
    pub what: String,
    /// The directories it writes: measured for what they hold now, and while it runs.
    pub dirs: Vec<PathBuf>,
    /// Where the footprints of this kind of run are kept.
    pub record: PathBuf,
    /// What a run of this kind writes into empty directories: where none was measured here, it
    /// claims this less what [`Self::dirs`] hold already.
    pub cold: u64,
    /// Where its writes land: the claim is on the volume holding it.
    pub on: PathBuf,
}

/// An admitted run: its claim held, and its writes measured, until [`Self::finish`].
#[derive(Debug)]
pub struct Admitted {
    meter: Meter,
    record: PathBuf,
    /// What it claimed, and why.
    pub estimate: Estimate,
    /// How long it waited for room.
    pub waited: Duration,
}

impl Admitted {
    /// The run is over: let its claim go, and keep what it took for the next run of its kind.
    pub fn finish(self) -> Footprint {
        let run = self.meter.finish();
        let mut kept = Footprints::load(&self.record);
        kept.push(run);
        let _ = kept.save(&self.record);
        run
    }
}

/// Claim `run`'s disk in this user's ledger on the live machine, relieving a short volume
/// ([`Reclaim::live`]) before it waits and saying through `say` what it does and waits for.
///
/// # Errors
/// [`Error::Never`]: the need is more than the volume holds above its floor; [`Error::Ledger`]:
/// no home, or the ledger could not be written (the caller runs without a claim, and says so);
/// [`Error::Stopped`]: an interrupt ended the wait (the caller ends as NOT RUN).
pub fn admit(run: Run, mode: Admit, say: &mut dyn FnMut(&str)) -> Result<Admitted, Error> {
    let ledger = Ledger::user().ok_or_else(|| {
        Error::Ledger(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no home directory to keep the ledger under",
        ))
    })?;
    let mut relief = Reclaim::live(&run.dirs);
    admit_on(run, &ledger, &Live, mode, Pace::default(), &mut relief, say)
}

/// [`admit`] in `ledger` on `machine`, relieved by `relief`. Under `--admit wait` the measuring
/// of the run's directories is part of the wait: an interrupt then ends it as one during the
/// wait does, before its first look, whether or not the run would claim anything.
///
/// # Errors
/// As [`admit`].
pub fn admit_on(
    run: Run,
    ledger: &Ledger,
    machine: &dyn Machine,
    mode: Admit,
    pace: Pace,
    relief: &mut dyn Relief,
    say: &mut dyn FnMut(&str),
) -> Result<Admitted, Error> {
    let watch = (mode == Admit::Wait).then(aterm_toolchain::interrupt::Watch::enter);
    let holds = allocated_until(&run.dirs, &|| {
        watch.as_ref().is_some_and(|w| w.caught().is_some())
    });
    if let Some(signal) = watch
        .as_ref()
        .and_then(aterm_toolchain::interrupt::Watch::take)
    {
        return Err(Error::Stopped {
            signal: Some(signal),
            short: None,
        });
    }
    let estimate = Footprints::load(&run.record).need(holds, run.cold);
    let claim = if estimate.bytes == 0 {
        None
    } else {
        let need = Need::new(run.what.clone()).disk(estimate.bytes, run.on.clone());
        Some(admit_relieved(
            ledger, machine, need, mode, pace, relief, say,
        )?)
    };
    let waited = claim
        .as_ref()
        .map_or(Duration::ZERO, aterm_capacity::Claim::waited);
    Ok(Admitted {
        meter: Meter::start(run.dirs, holds, claim),
        record: run.record,
        estimate,
        waited,
    })
}

/// A volume's free bytes that the build cache's pass under pressure gave back, and the
/// directories it removed.
pub type Passed = (u64, Vec<PathBuf>);

/// Whether an interrupt ended the wait a relief works for ([`Relief`]): each part gives up what
/// is left of its work once it says so.
pub type Stopped<'a> = &'a dyn Fn() -> bool;

/// The build cache's pass under pressure, given the short volume's mount.
pub type Pass = Box<dyn FnMut(&Path, Stopped<'_>) -> Passed>;

/// The window's disk watch asked for a number of bytes free on the volume mounted at a path:
/// its answer, or `None` outside aterm.
pub type Ask = Box<dyn FnMut(u64, &Path, Stopped<'_>) -> Option<String>>;

/// The largest holders of the volume mounted at a path, with their sizes, largest first.
pub type Largest = Box<dyn FnMut(&Path, Stopped<'_>) -> Vec<(PathBuf, u64)>>;

/// WHAT A SHORT VOLUME IS RELIEVED BY before a run waits on it, and who a wait line names as
/// holding the rest.
pub struct Reclaim {
    /// The build cache's pass under pressure, given the short volume's mount.
    pass: Pass,
    /// The window's disk watch asked for `bytes` free on the volume mounted at `mount`: its
    /// answer, or `None` outside aterm.
    ask: Ask,
    /// The largest holders of the volume mounted at the path, largest first.
    largest: Largest,
    /// The holders' words, read once a wait begins.
    named: Option<String>,
}

impl Reclaim {
    /// The relief made of these parts.
    #[must_use]
    pub fn of(pass: Pass, ask: Ask, largest: Largest) -> Reclaim {
        Reclaim {
            pass,
            ask,
            largest,
            named: None,
        }
    }

    /// The live relief: [`crate::build_cache::reclaim_home_watched`] with
    /// [`crate::build_cache::RECLAIM_GONE_UNDER_PRESSURE`] where the build cache is on the short
    /// volume ([`cache_on`]), then inside aterm `aterm ctl @self
    /// await sys free>=<bytes> vol=<mount>` for up to [`ASK_FOR`]; the largest holders are the
    /// build cache's directories on the volume and `own` (the run's directories). Once an
    /// interrupt came, the pass is left behind removing nothing more ([`on_thread`]), the ask
    /// is ended and the sizing stops.
    #[must_use]
    pub fn live(own: &[PathBuf]) -> Reclaim {
        let own = own.to_vec();
        Reclaim::of(
            Box::new(|mount, stopped| {
                if !cache_on(mount) {
                    return (0, Vec::new());
                }
                let before = free_on(mount);
                let removed = on_thread(stopped, |halt| {
                    let mut removed = Vec::new();
                    crate::build_cache::reclaim_home_watched(
                        crate::build_cache::RECLAIM_GONE_UNDER_PRESSURE,
                        None,
                        &mut |path, _why| {
                            if halt.load(Ordering::SeqCst) {
                                return false;
                            }
                            removed.push(path.to_path_buf());
                            true
                        },
                    );
                    removed
                })
                .unwrap_or_default();
                (free_on(mount).saturating_sub(before), removed)
            }),
            Box::new(ask_watch),
            Box::new(move |mount, stopped| largest_on(mount, &own, stopped)),
        )
    }
}

impl Relief for Reclaim {
    fn relieve(&mut self, short: &Shortfall, say: &mut dyn FnMut(&str), stopped: Stopped<'_>) {
        let Some((need, mount, floor, reserved)) = disk_short(short) else {
            return;
        };
        let (freed, removed) = (self.pass)(&mount, stopped);
        if stopped() {
            return;
        }
        say(&if removed.is_empty() {
            format!(
                "disk: {} is short; the build cache had nothing unused to reclaim on it",
                mount.display()
            )
        } else {
            format!(
                "disk: {} is short; reclaimed {} from {} unused build director{}: {}",
                mount.display(),
                gib(freed),
                removed.len(),
                if removed.len() == 1 { "y" } else { "ies" },
                removed
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        });
        let want = floor.saturating_add(reserved).saturating_add(need);
        if let Some(answer) = (self.ask)(want, &mount, stopped) {
            say(&format!(
                "disk: asked the window's disk watch for {} free on {}: {answer}",
                gib(want),
                mount.display()
            ));
        }
    }

    fn holders(&mut self, short: &Shortfall, stopped: Stopped<'_>) -> Option<String> {
        let (_, mount, _, _) = disk_short(short)?;
        let named = self.named.get_or_insert_with(|| {
            let found = (self.largest)(&mount, stopped);
            if found.is_empty() {
                return String::new();
            }
            let each: Vec<String> = found
                .iter()
                .map(|(p, b)| format!("{} {}", p.display(), gib(*b)))
                .collect();
            format!("largest on {}: {}", mount.display(), each.join(", "))
        });
        (!named.is_empty()).then(|| named.clone())
    }
}

/// The disk a shortfall is short of: `(need, mount, floor, reserved)`.
fn disk_short(short: &Shortfall) -> Option<(u64, PathBuf, u64, u64)> {
    short.shorts.iter().find_map(|s| match s {
        Short::Disk {
            need,
            mount,
            floor,
            reserved,
            ..
        } => Some((*need, mount.clone(), *floor, *reserved)),
        _ => None,
    })
}

/// How long the window's disk watch is waited for when asked for room: one pass of it. The run
/// waits on in its own admission after, with no deadline.
pub const ASK_FOR: Duration = Duration::from_secs(60);

/// The volume mounted at `mount`'s free bytes now; 0 where unread.
fn free_on(mount: &Path) -> u64 {
    Live.read(Some(mount)).volume.map_or(0, |v| v.free)
}

/// `work` (the build cache's pass) on a thread of its own, waited for until `stopped` says an
/// interrupt came: `work`'s `halt` then says `true`, so it removes nothing more, and it is left
/// to end the step it is in while this returns `None` at once. That step can be a whole build
/// directory's removal; one the process's end cuts short leaves its stage, which the next pass
/// finishes. `None` too when the thread cannot start or `work` panics: the relief is skipped.
fn on_thread<T: Send + 'static>(
    stopped: Stopped<'_>,
    work: impl FnOnce(&AtomicBool) -> T + Send + 'static,
) -> Option<T> {
    let halt = Arc::new(AtomicBool::new(false));
    let told = Arc::clone(&halt);
    let (done, result) = mpsc::channel();
    std::thread::Builder::new()
        .name("disk-relief".to_owned())
        .spawn(move || {
            let _ = done.send(work(&told));
        })
        .ok()?;
    loop {
        if stopped() {
            halt.store(true, Ordering::SeqCst);
            return None;
        }
        match result.recv_timeout(aterm_toolchain::interrupt::NAP) {
            Ok(got) => return Some(got),
            Err(mpsc::RecvTimeoutError::Disconnected) => return None,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

/// Whether this user's build cache, or the root it moved from, is on the volume mounted at
/// `mount`: a pass over it frees nothing on any other volume, so a short scratch image or
/// external disk is not reclaimed for.
fn cache_on(mount: &Path) -> bool {
    aterm_types::dirs::home_dir().is_some_and(|h| {
        [
            crate::build_cache::cache_root(&h),
            crate::build_cache::moved_from(&h),
        ]
        .iter()
        .filter(|c| c.is_dir())
        .any(|c| Live.read(Some(c)).volume.is_some_and(|v| v.mount == mount))
    })
}

/// Inside aterm (`ATERM_SESSION_ID` names this session), ask the window's disk watch for `bytes`
/// free on the volume mounted at `mount` (`aterm ctl @self await sys free>=<bytes>B vol=<mount>`),
/// waiting for it up to [`ASK_FOR`], or until `stopped` says an interrupt came, which ends the
/// ask (this process's own child) at once: its answer, or `None` outside aterm, with no `aterm`
/// on `PATH`, or once stopped.
fn ask_watch(bytes: u64, mount: &Path, stopped: Stopped<'_>) -> Option<String> {
    std::env::var_os("ATERM_SESSION_ID").filter(|v| !v.is_empty())?;
    let path = std::env::var_os("PATH")?;
    let aterm = std::env::split_paths(&path)
        .map(|d| d.join("aterm"))
        .find(|p| p.is_file())?;
    let mut child = aterm_uds::spawnfd::command(&aterm)
        .args([
            "ctl".to_owned(),
            "@self".to_owned(),
            "await".to_owned(),
            "sys".to_owned(),
            format!("free>={bytes}B"),
            format!("vol={}", pct_encode(&mount.to_string_lossy())),
            format!("timeout={}", ASK_FOR.as_millis()),
        ])
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .ok()?;
    // Each pipe is read on a thread of its own while the ask lasts, so no answer, however long,
    // stalls it; an ask ended by an interrupt leaves them, which end with its pipes.
    let reader = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_string(&mut text);
            }
            text
        })
    };
    let out = reader(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
    );
    let err = reader(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
    );
    let status = loop {
        if stopped() {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(aterm_toolchain::interrupt::NAP),
            Err(_) => return None,
        }
    };
    let out = out.join().unwrap_or_default();
    let err = err.join().unwrap_or_default();
    let said = if out.is_empty() { err } else { out }
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    Some(if said.is_empty() {
        format!("no answer ({status})")
    } else {
        said
    })
}

/// `text` with every byte but `[A-Za-z0-9/._~-]` written `%XX`: a control request's word.
fn pct_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'~' | b'-') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The three largest holders of the volume mounted at `mount` among those a build tool may
/// size: the run's `own` directories and the build cache's directories there. Nothing else on
/// the volume is walked: its other folders are the person's (Documents, Desktop, other apps'
/// data), and a look into those can put a privacy dialog on their screen. A directory inside
/// another named is left out, so no bytes are named twice. The sizing stops once `stopped`
/// says an interrupt came.
fn largest_on(mount: &Path, own: &[PathBuf], stopped: Stopped<'_>) -> Vec<(PathBuf, u64)> {
    let on = |p: &Path| Live.read(Some(p)).volume.is_some_and(|v| v.mount == mount);
    let mut found: Vec<PathBuf> = own.iter().filter(|d| on(d)).cloned().collect();
    let home = aterm_types::dirs::home_dir();
    let caches = home.iter().flat_map(|h| {
        [
            crate::build_cache::cache_root(h),
            crate::build_cache::moved_from(h),
        ]
    });
    for cache in caches.filter(|c| c.is_dir() && on(c)) {
        found.extend(
            crate::build_cache::workspaces(&cache)
                .iter()
                .map(|w| crate::build_cache::build_dir(&cache, w)),
        );
    }
    found.sort();
    found.dedup();
    let outer = found.clone();
    found.retain(|p| !outer.iter().any(|o| o != p && p.starts_with(o)));
    let mut sized: Vec<(PathBuf, u64)> = found
        .into_iter()
        .map(|p| {
            let b = allocated_until(std::slice::from_ref(&p), stopped);
            (p, b)
        })
        .filter(|(_, b)| *b > 0)
        .collect();
    sized.sort_by_key(|a| std::cmp::Reverse(a.1));
    sized.truncate(3);
    sized
}

#[cfg(test)]
#[path = "admit_disk_tests.rs"]
mod tests;
