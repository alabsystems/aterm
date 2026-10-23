// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Root-owned machine agent state (`floors.toml`, run lock, and latest run records).

use super::Layout;
use crate::model::Read;
use crate::port::System;
use crate::protocol::Floor;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FloorsFile {
    #[serde(default)]
    floor: Vec<Floor>,
}

/// The floors.
#[must_use]
pub fn floors(sys: &dyn System, layout: &Layout) -> Read<Vec<Floor>> {
    match sys.read_file(&layout.floors()) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Read::Absent,
        Err(e) => Read::Unusable(format!("{}: {e}", layout.floors().display())),
        Ok(bytes) => {
            let parsed = std::str::from_utf8(&bytes)
                .map_err(|_| "not UTF-8".to_owned())
                .and_then(|t| aterm_toml::from_str::<FloorsFile>(t).map_err(|e| e.to_string()));
            match parsed {
                Ok(f) => Read::Value(f.floor),
                Err(why) => Read::Unusable(format!("{}: {why}", layout.floors().display())),
            }
        }
    }
}

/// `principal`'s floor in `floors` (0 when it has none).
#[must_use]
pub fn floor_of(floors: &[Floor], principal: &str) -> u64 {
    floors
        .iter()
        .filter(|f| f.principal == principal)
        .map(|f| f.serial)
        .max()
        .unwrap_or(0)
}

/// The floors as a list, or why they cannot be trusted.
/// # Errors
/// One line when the file exists and cannot be read.
pub fn floors_or_refuse(sys: &dyn System, layout: &Layout) -> Result<Vec<Floor>, String> {
    match floors(sys, layout) {
        Read::Value(f) => Ok(f),
        Read::Absent => Ok(Vec::new()),
        Read::Unusable(why) => Err(format!(
            "the serial floors cannot be read ({why}); every envelope is refused until \
             state/floors.toml is repaired"
        )),
    }
}

/// Raise `principal`'s floor to `serial` (never lowers it).
/// # Errors
/// One line; the caller must not act on the envelope then.
pub fn raise_floor(
    sys: &dyn System,
    layout: &Layout,
    principal: &str,
    serial: u64,
) -> Result<(), String> {
    let mut list = floors_or_refuse(sys, layout)?;
    match list.iter_mut().find(|f| f.principal == principal) {
        Some(f) => f.serial = f.serial.max(serial),
        None => list.push(Floor {
            principal: principal.to_owned(),
            serial,
        }),
    }
    let text =
        aterm_toml::to_string(&FloorsFile { floor: list }).map_err(|e| format!("floors: {e}"))?;
    sys.write_file(&layout.floors(), text.as_bytes(), 0o644)
        .map_err(|e| format!("{}: {e}", layout.floors().display()))
}

/// Raise each of `principals` to at least the highest floor any principal has.
/// # Errors
/// One line when `floors.toml` cannot be read or written; the caller must not go on as if
/// they had been raised.
pub fn inherit_floors(
    sys: &dyn System,
    layout: &Layout,
    principals: &[String],
) -> Result<(), String> {
    let list = floors_or_refuse(sys, layout)?;
    let top = list.iter().map(|f| f.serial).max().unwrap_or(0);
    if top == 0 {
        return Ok(());
    }
    for p in principals {
        if floor_of(&list, p) < top {
            raise_floor(sys, layout, p, top)?;
        }
    }
    Ok(())
}

/// Who holds `state/run.lock`: `pid=<n> run=<id> [start=<when>]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    /// The holding process.
    pub pid: u32,
    /// Its run.
    pub run: String,
    /// When the holding process started, as [`started`] read it (`None` when it could not be read).
    pub start: Option<String>,
}

impl Holder {
    fn text(&self) -> String {
        match &self.start {
            Some(start) => format!("pid={} run={} start={start}\n", self.pid, self.run),
            None => format!("pid={} run={}\n", self.pid, self.run),
        }
    }

    fn parse(text: &str) -> Option<Self> {
        let mut pid = None;
        let mut run = None;
        let mut start = None;
        for word in text.split_whitespace() {
            if let Some(p) = word.strip_prefix("pid=") {
                pid = p.parse().ok();
            } else if let Some(r) = word.strip_prefix("run=") {
                run = Some(r.to_owned());
            } else if let Some(s) = word.strip_prefix("start=") {
                start = Some(s.to_owned()).filter(|s| !s.is_empty());
            }
        }
        Some(Self {
            pid: pid?,
            run: run.filter(|r| super::is_run_id(r))?,
            start,
        })
    }
}

/// When `pid` started, as one word.
#[must_use]
pub fn started(sys: &dyn System, pid: u32) -> Option<String> {
    sys.process_started(pid).map(lstart_word)
}

/// `unix` seconds as `ps -o lstart=` prints them in UTC under the C locale (`%a %b %e %T
/// %Y`, the day unpadded once the spaces are `_`): `Fri_Oct_16_22:36:10_2026`.
#[must_use]
pub fn lstart_word(unix: u64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (days, secs) = (unix / 86_400, unix % 86_400);
    let (year, month, day) =
        aterm_types::rfc3339::civil_from_days(i64::try_from(days).unwrap_or(0));
    let weekday = usize::try_from(days % 7)
        .ok()
        .and_then(|i| DAYS.get(i))
        .unwrap_or(&"?");
    let month = usize::try_from(month - 1)
        .ok()
        .and_then(|i| MONTHS.get(i))
        .unwrap_or(&"?");
    format!(
        "{weekday}_{month}_{day}_{:02}:{:02}:{:02}_{year}",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

/// Whether `h`'s process is still the one that took the lock.
#[must_use]
pub fn holder_live(sys: &dyn System, h: &Holder) -> bool {
    if !sys.pid_alive(h.pid) {
        return false;
    }
    match (&h.start, started(sys, h.pid)) {
        (Some(then), Some(now)) => *then == now,
        _ => true,
    }
}

/// The lock's holder, if the file names one.
#[must_use]
pub fn holder(sys: &dyn System, layout: &Layout) -> Option<Holder> {
    let bytes = sys.read_file(&layout.lock()).ok()?;
    Holder::parse(&String::from_utf8_lossy(&bytes))
}

/// The run in progress: the lock's run when its holder is live.
#[must_use]
pub fn busy(sys: &dyn System, layout: &Layout) -> Option<String> {
    holder(sys, layout)
        .filter(|h| holder_live(sys, h))
        .map(|h| h.run)
}

/// Whether `run`'s job is still going: the run lock names it and its holder is live.
#[must_use]
pub fn running(sys: &dyn System, layout: &Layout, run: &str) -> bool {
    holder(sys, layout).is_some_and(|h| h.run == run && holder_live(sys, &h))
}

const GUARD_LIMIT: std::time::Duration = std::time::Duration::from_secs(30);

fn guard(sys: &dyn System, layout: &Layout) -> Result<Box<dyn std::any::Any>, String> {
    sys.exclusive(&layout.lock_guard(), GUARD_LIMIT)
        .map_err(|e| format!("{}: {e}", layout.lock_guard().display()))
}

/// What [`take_lock`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lock {
    /// This process holds it now.
    Taken,
    /// Another live run holds it.
    Busy {
        /// Its run id.
        run: String,
    },
}

/// Take `state/run.lock` for `run`, held by `pid`: an exclusive create.
/// # Errors
/// One line when the guard or the lock file cannot be had.
pub fn take_lock(sys: &dyn System, layout: &Layout, run: &str, pid: u32) -> Result<Lock, String> {
    let mine = Holder {
        pid,
        run: run.to_owned(),
        start: started(sys, pid),
    };
    let _guard = guard(sys, layout)?;
    match sys.create_new(&layout.lock(), mine.text().as_bytes(), 0o644) {
        Ok(()) => return Ok(Lock::Taken),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(format!("{}: {e}", layout.lock().display())),
    }
    if let Some(h) = holder(sys, layout).filter(|h| holder_live(sys, h)) {
        return Ok(Lock::Busy { run: h.run });
    }
    sys.remove(&layout.lock())
        .map_err(|e| format!("{}: {e}", layout.lock().display()))?;
    sys.create_new(&layout.lock(), mine.text().as_bytes(), 0o644)
        .map_err(|e| format!("{}: {e}", layout.lock().display()))?;
    Ok(Lock::Taken)
}

/// Hand the lock for `run` to `pid` (the detached job).
/// # Errors
/// One line.
pub fn hand_lock(sys: &dyn System, layout: &Layout, run: &str, pid: u32) -> Result<(), String> {
    let start = started(sys, pid);
    let _guard = guard(sys, layout)?;
    match holder(sys, layout) {
        Some(h) if h.run == run => {}
        other => {
            return Err(format!(
                "the run lock names {} instead of run {run}",
                other.map_or_else(|| "nothing".to_owned(), |h| h.run)
            ));
        }
    }
    let to = Holder {
        pid,
        run: run.to_owned(),
        start,
    };
    sys.write_file(&layout.lock(), to.text().as_bytes(), 0o644)
        .map_err(|e| format!("{}: {e}", layout.lock().display()))
}

/// Hand only the exact observed holder to a known, still-uncommitted child.
/// # Errors
/// The holder changed, the child lacks a start identity, or the atomic write failed.
pub(super) fn hand_lock_from(
    sys: &dyn System,
    layout: &Layout,
    from: &Holder,
    to: &Holder,
) -> Result<(), String> {
    if from.run != to.run || to.pid == 0 || to.start.is_none() {
        return Err("a gated job needs the same run and a known child start identity".into());
    }
    let _guard = guard(sys, layout)?;
    if holder(sys, layout).as_ref() != Some(from) {
        return Err("the exact run-lock holder changed before the job handoff".into());
    }
    sys.write_file(&layout.lock(), to.text().as_bytes(), 0o644)
        .map_err(|e| format!("{}: {e}", layout.lock().display()))
}

/// Release only this exact holder, never a later owner with the same run name.
/// # Errors
/// The lock guard or removal failed; no unowned replacement is removed.
pub(super) fn release_holder(
    sys: &dyn System,
    layout: &Layout,
    expected: &Holder,
) -> Result<(), String> {
    let _guard = guard(sys, layout)?;
    if holder(sys, layout).as_ref() == Some(expected) {
        sys.remove(&layout.lock())
            .map_err(|e| format!("{}: {e}", layout.lock().display()))?;
    }
    Ok(())
}

/// Join an internal gated job to its current PID and known recorded start identity.
/// # Errors
/// Missing/changed holder, another PID, or an unmeasured/mismatched start identity.
pub(super) fn job_holder(
    sys: &dyn System,
    layout: &Layout,
    run: &str,
    pid: u32,
) -> Result<Holder, String> {
    let start = started(sys, pid)
        .ok_or_else(|| "the run job's start identity could not be read".to_owned())?;
    let expected = Holder {
        pid,
        run: run.to_owned(),
        start: Some(start),
    };
    if holder(sys, layout).as_ref() != Some(&expected) {
        return Err("the run lock does not name this exact job process".into());
    }
    Ok(expected)
}

/// Release the lock if it names `run`.
pub fn release_lock(sys: &dyn System, layout: &Layout, run: &str) {
    let Ok(_guard) = guard(sys, layout) else {
        return;
    };
    if holder(sys, layout).is_some_and(|h| h.run == run) {
        let _ = sys.remove(&layout.lock());
    }
}

/// The newest finished run.
#[must_use]
pub fn last_run(sys: &dyn System, layout: &Layout) -> Option<String> {
    let bytes = sys.read_file(&layout.last_run()).ok()?;
    let run = String::from_utf8_lossy(&bytes).trim().to_owned();
    super::is_run_id(&run).then_some(run)
}

/// Record `run` as the newest finished run.
pub fn set_last_run(sys: &dyn System, layout: &Layout, run: &str) {
    let _ = sys.write_file(&layout.last_run(), format!("{run}\n").as_bytes(), 0o644);
}

/// The newest signed request refused `busy` (by the id its run would have had).
#[must_use]
pub fn last_busy(sys: &dyn System, layout: &Layout) -> Option<String> {
    let bytes = sys.read_file(&layout.last_busy()).ok()?;
    let run = String::from_utf8_lossy(&bytes).trim().to_owned();
    super::is_run_id(&run).then_some(run)
}

/// Record `run` as the newest signed request refused `busy`.
pub fn set_last_busy(sys: &dyn System, layout: &Layout, run: &str) {
    let _ = sys.write_file(&layout.last_busy(), format!("{run}\n").as_bytes(), 0o644);
}

/// `managed.toml`: what enrollment recorded, and what the agent has managed since.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Managed {
    /// [`crate::SCHEMA`].
    pub schema: u32,
    /// Admin account the fleet key is authorized for.
    pub admin: String,
    /// SHA-256 of the agent archive enrollment installed.
    pub payload_sha256: String,
    /// The agent's version.
    pub agent_version: String,
    /// The agent executable, relative to the agent root.
    pub executable: String,
    /// Unix seconds of the install.
    pub installed: u64,
    /// Unix seconds of the last upgrade.
    #[serde(default)]
    pub upgraded: u64,
    /// Every item id a run has managed on this host.
    #[serde(default)]
    pub items: Vec<String>,
}

/// Read `managed.toml`.
/// # Errors
/// One line when it is missing or does not parse (the host was not enrolled by this agent).
pub fn managed(sys: &dyn System, layout: &Layout) -> Result<Managed, String> {
    let bytes = sys
        .read_file(&layout.managed())
        .map_err(|e| format!("{}: {e}", layout.managed().display()))?;
    let text = String::from_utf8(bytes).map_err(|_| "managed.toml is not UTF-8".to_owned())?;
    aterm_toml::from_str(&text).map_err(|e| format!("managed.toml: {e}"))
}

/// Write `managed.toml`.
/// # Errors
/// One line.
pub fn save_managed(sys: &dyn System, layout: &Layout, m: &Managed) -> Result<(), String> {
    let text = aterm_toml::to_string(m).map_err(|e| format!("managed.toml: {e}"))?;
    sys.write_file(&layout.managed(), text.as_bytes(), 0o644)
        .map_err(|e| format!("{}: {e}", layout.managed().display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::port::Fake;

    fn layout() -> Layout {
        Layout { root: "/A".into() }
    }

    #[test]
    fn a_floor_only_rises_and_survives_a_rewrite() {
        let fake = Fake::root();
        assert_eq!(floors(&fake, &layout()), Read::Absent);
        raise_floor(&fake, &layout(), "signing@ctl", 5).unwrap();
        raise_floor(&fake, &layout(), "recovery@ctl", 2).unwrap();
        raise_floor(&fake, &layout(), "signing@ctl", 3).unwrap();
        let Read::Value(list) = floors(&fake, &layout()) else {
            panic!("floors unreadable");
        };
        assert_eq!(floor_of(&list, "signing@ctl"), 5);
        assert_eq!(floor_of(&list, "recovery@ctl"), 2);
        assert_eq!(floor_of(&list, "nobody"), 0);
        assert_eq!(fake.mode_of("/A/state/floors.toml"), Some(0o644));
    }

    #[test]
    fn a_damaged_floors_file_is_unusable_never_zero() {
        let fake = Fake::root();
        fake.file(
            "/A/state/floors.toml",
            b"[[floor]]\nprincipal = \"x\"\nserial = \"many\"\n",
        );
        assert!(matches!(floors(&fake, &layout()), Read::Unusable(_)));
        assert!(floors_or_refuse(&fake, &layout()).is_err());
        assert!(raise_floor(&fake, &layout(), "x", 9).is_err());
    }

    #[test]
    fn a_live_holder_keeps_the_lock_and_a_dead_one_loses_it() {
        let fake = Fake::root();
        assert_eq!(take_lock(&fake, &layout(), "1-1", 10).unwrap(), Lock::Taken);
        fake.alive(10);
        assert_eq!(
            take_lock(&fake, &layout(), "2-2", 11).unwrap(),
            Lock::Busy { run: "1-1".into() }
        );
        assert_eq!(busy(&fake, &layout()).as_deref(), Some("1-1"));
        hand_lock(&fake, &layout(), "1-1", 12).unwrap();
        assert_eq!(busy(&fake, &layout()), None);
        assert_eq!(take_lock(&fake, &layout(), "3-3", 13).unwrap(), Lock::Taken);
        assert_eq!(holder(&fake, &layout()).unwrap().run, "3-3");
        release_lock(&fake, &layout(), "1-1");
        assert!(holder(&fake, &layout()).is_some());
        release_lock(&fake, &layout(), "3-3");
        assert!(holder(&fake, &layout()).is_none());
    }

    #[test]
    fn a_reused_pid_does_not_keep_a_crashed_runs_lock() {
        let stale = b"pid=10 run=1-1 start=Fri_Oct_16_22:36:10_2026\n";
        let fake = Fake::root();
        fake.file("/A/state/run.lock", stale);
        fake.alive(10);
        fake.born(10, 1_792_224_062);
        fake.born(11, 1_792_224_300);
        assert_eq!(busy(&fake, &layout()), None);
        assert!(!running(&fake, &layout(), "1-1"));
        assert_eq!(take_lock(&fake, &layout(), "2-2", 11).unwrap(), Lock::Taken);
        assert_eq!(
            holder(&fake, &layout()).unwrap().start.as_deref(),
            Some("Sat_Oct_17_08:05:00_2026")
        );
        let fake = Fake::root();
        fake.file("/A/state/run.lock", stale);
        fake.alive(10);
        fake.born(10, 1_792_190_170);
        assert_eq!(busy(&fake, &layout()).as_deref(), Some("1-1"));
        assert_eq!(
            take_lock(&fake, &layout(), "2-2", 11).unwrap(),
            Lock::Busy { run: "1-1".into() }
        );
        let fake = Fake::root();
        fake.file("/A/state/run.lock", stale);
        fake.alive(10);
        assert_eq!(busy(&fake, &layout()).as_deref(), Some("1-1"));
    }

    /// A start reads as the word `ps -o lstart=` printed under `TZ=UTC0`, which a lock of an
    /// earlier agent holds: the kernel's record, formatted here.
    #[test]
    fn a_start_is_the_word_ps_printed() {
        assert_eq!(lstart_word(0), "Thu_Jan_1_00:00:00_1970");
        assert_eq!(lstart_word(951_782_400), "Tue_Feb_29_00:00:00_2000");
        assert_eq!(lstart_word(1_792_190_170), "Fri_Oct_16_22:36:10_2026");
        assert_eq!(lstart_word(1_792_224_062), "Sat_Oct_17_08:01:02_2026");
        // NEGATIVE CONTROL: a second later is another start.
        assert_ne!(lstart_word(1_792_190_171), lstart_word(1_792_190_170));
    }

    /// `/bin/ps -o lstart= -p <pid>` under `TZ=UTC0`, the reader [`started`] replaced, as
    /// its oracle: `None` when it does not answer. `zone` is the `TZ` it runs under.
    #[cfg(target_os = "macos")]
    fn ps_says(pid: u32, zone: &str) -> Option<String> {
        let out = aterm_uds::spawnfd::command(aterm_uds::os_path!("ps"))
            .env("TZ", zone)
            .env("LC_ALL", "C")
            .args(["-o", "lstart=", "-p"])
            .arg(pid.to_string())
            .output()
            .ok()?;
        let word = String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("_");
        (out.status.success() && !word.is_empty()).then_some(word)
    }

    /// The kernel's start of this process, its parent and launchd (root's, which the old
    /// `ps` read too) is what `ps` says. NEGATIVE CONTROL: `ps` under another zone says
    /// another word, so the comparison can fail; a pid no process holds reads `None`.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_kernel_start_is_what_ps_says() {
        let live = crate::port::Live::new();
        let me = std::process::id();
        let parent = aterm_uds::process::read_process_parent(i32::try_from(me).unwrap())
            .expect("this process's parent");
        for pid in [me, parent, 1] {
            let ours = started(&live, pid).expect("the kernel's record");
            assert_eq!(Some(ours.clone()), ps_says(pid, "UTC0"), "pid {pid}");
            assert_ne!(Some(ours), ps_says(pid, "Asia/Tokyo"), "pid {pid}");
        }
        assert_eq!(started(&live, 2_147_483_647), None);
        assert_eq!(ps_says(2_147_483_647, "UTC0"), None);
    }

    #[test]
    fn a_principal_new_to_the_list_starts_at_the_highest_floor() {
        let fake = Fake::root();
        inherit_floors(&fake, &layout(), &["new@ctl".into()]).unwrap();
        assert_eq!(
            floors(&fake, &layout()),
            Read::Absent,
            "a fresh host has none"
        );
        raise_floor(&fake, &layout(), "signing@ctl", 41).unwrap();
        raise_floor(&fake, &layout(), "recovery@ctl", 3).unwrap();
        inherit_floors(
            &fake,
            &layout(),
            &["renamed@ctl".into(), "recovery@ctl".into()],
        )
        .unwrap();
        let Read::Value(list) = floors(&fake, &layout()) else {
            panic!("floors unreadable");
        };
        assert_eq!(floor_of(&list, "renamed@ctl"), 41);
        assert_eq!(floor_of(&list, "recovery@ctl"), 41);
        assert_eq!(floor_of(&list, "signing@ctl"), 41);
        let fake = Fake::root();
        fake.file("/A/state/floors.toml", b"not toml [");
        assert!(inherit_floors(&fake, &layout(), &["x".into()]).is_err());
    }

    #[test]
    fn of_takers_racing_one_stale_lock_exactly_one_takes_it() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let layout = Layout {
            root: dir.path().to_path_buf(),
        };
        std::fs::create_dir_all(layout.state()).unwrap();
        std::fs::write(layout.lock(), "pid=2147483647 run=1-1\n").unwrap();
        let live = crate::port::Live::new();
        let me = std::process::id();
        let taken = std::thread::scope(|s| {
            let racers: Vec<_> = (2..10)
                .map(|i| {
                    let (live, layout) = (&live, &layout);
                    s.spawn(move || take_lock(live, layout, &format!("{i}-{i}"), me).unwrap())
                })
                .collect();
            racers
                .into_iter()
                .map(|r| r.join().unwrap())
                .filter(|lock| *lock == Lock::Taken)
                .count()
        });
        assert_eq!(taken, 1);
    }

    #[test]
    fn the_lock_is_handed_only_for_its_own_run() {
        let fake = Fake::root();
        take_lock(&fake, &layout(), "1-1", 10).unwrap();
        assert!(hand_lock(&fake, &layout(), "9-9", 11).is_err());
    }

    #[test]
    fn the_live_lock_is_exclusive_in_a_real_directory() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let layout = Layout {
            root: dir.path().to_path_buf(),
        };
        std::fs::create_dir_all(layout.state()).unwrap();
        let live = crate::port::Live::new();
        let me = std::process::id();
        assert_eq!(take_lock(&live, &layout, "1-1", me).unwrap(), Lock::Taken);
        assert_eq!(
            take_lock(&live, &layout, "2-2", me).unwrap(),
            Lock::Busy { run: "1-1".into() }
        );
        std::fs::write(layout.lock(), "pid=2147483647 run=1-1\n").unwrap();
        assert_eq!(take_lock(&live, &layout, "2-2", me).unwrap(), Lock::Taken);
    }
}
