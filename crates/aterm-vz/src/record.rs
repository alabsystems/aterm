// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The helper's run files and its record (`docs/DESIGN-vz-guest-2026-10-23.md` §5.1).
//!
//! | File | What |
//! | --- | --- |
//! | `run/aterm-linux.sock` | the control socket the helper binds |
//! | `run/aterm-linux.json` | the record, written by rename once the socket is bound |
//! | `run/helper.log` | the helper's stdout and stderr, appended |
//! | `run/console.log` | hvc0, as the guest wrote it |
//! | `run/key.img` | the boot's key disk (0600), attached read-only; removed when the helper exits |
//! | `live/aterm-linux.img.lock` | the live disk's `flock`, held for the helper's whole life |
//!
//! A record names its helper by pid, unique id (`p_uniqueid`, never reused within a boot) *and*
//! kernel start time, so a reused pid is never taken for the helper and never signalled
//! ([`Holder`], [`is_live`]). The start time carries the name across boots: a record outlives a
//! restart that killed its helper, and every process a boot makes before its first pid wrap
//! has a unique id equal to its pid (measured), so pid and unique id alone match whichever
//! process holds that early pid in the next boot.

use std::path::{Path, PathBuf};

use crate::layers::VM;

/// `sun_path`'s size on macOS: a socket path must fit, with its NUL.
pub const SUN_PATH: usize = 104;

/// The run files under one state directory (`vz.noindex`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    root: PathBuf,
}

impl Paths {
    /// The run files under `root`.
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The state directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `run/`.
    #[must_use]
    pub fn run_dir(&self) -> PathBuf {
        self.root.join("run")
    }

    /// `run/aterm-linux.sock`.
    #[must_use]
    pub fn socket(&self) -> PathBuf {
        self.run_dir().join(format!("{VM}.sock"))
    }

    /// `run/aterm-linux.json`.
    #[must_use]
    pub fn record(&self) -> PathBuf {
        self.run_dir().join(format!("{VM}.json"))
    }

    /// `run/helper.log`.
    #[must_use]
    pub fn helper_log(&self) -> PathBuf {
        self.run_dir().join("helper.log")
    }

    /// `run/console.log`.
    #[must_use]
    pub fn console_log(&self) -> PathBuf {
        self.run_dir().join("console.log")
    }

    /// `run/key.img`: the boot's key disk ([`crate::key`]).
    #[must_use]
    pub fn key_disk(&self) -> PathBuf {
        self.run_dir().join("key.img")
    }

    /// `live/aterm-linux.img`.
    #[must_use]
    pub fn live_disk(&self) -> PathBuf {
        self.root.join("live").join(format!("{VM}.img"))
    }

    /// `live/aterm-linux.img.lock`.
    #[must_use]
    pub fn disk_lock(&self) -> PathBuf {
        self.root.join("live").join(format!("{VM}.img.lock"))
    }

    /// The socket path, refused by name when it does not fit `sun_path`.
    ///
    /// # Errors
    /// It is [`SUN_PATH`] bytes or longer.
    pub fn checked_socket(&self) -> Result<PathBuf, String> {
        let path = self.socket();
        let len = path.as_os_str().len();
        if len >= SUN_PATH {
            return Err(format!(
                "the helper's control socket {} is {len} bytes, past sun_path's {SUN_PATH} \
                 (with its NUL): move the vz state somewhere shorter",
                path.display()
            ));
        }
        Ok(path)
    }
}

/// What the helper writes at start and again once the agent says hello.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// Its pid.
    pub pid: u32,
    /// Its unique id, which the kernel never reuses within a boot.
    pub uniq: u64,
    /// Its kernel start time in microseconds since the Unix epoch ([`Pids::birth_us`]), which
    /// names it across boots; 0 when the record names none (an earlier build's record, or a
    /// start the helper could not read), which leaves the helper [`Holder::Unproven`].
    pub birth_us: u64,
    /// Its executable's content address (`host/<sha256>/`).
    pub helper_sha256: String,
    /// [`crate::proto::PROTO`].
    pub proto: u32,
    /// The live disk it holds.
    pub live_disk: PathBuf,
    /// When it started (Unix seconds).
    pub started: u64,
    /// The guest's boot, once the agent said hello.
    pub boot_id: Option<String>,
    /// The guest's agent, once it said hello.
    pub agent_sha256: Option<String>,
}

impl Record {
    /// The record as `run/aterm-linux.json` holds it.
    #[must_use]
    pub fn json(&self) -> String {
        let value = aterm_json::json!({
            "pid": self.pid,
            "uniq": self.uniq,
            "birth_us": self.birth_us,
            "helper_sha256": self.helper_sha256,
            "proto": self.proto,
            "live_disk": self.live_disk.to_string_lossy().into_owned(),
            "started": self.started,
            "boot_id": self.boot_id,
            "agent_sha256": self.agent_sha256,
        });
        let mut text = aterm_json::to_string(&value).unwrap_or_default();
        text.push('\n');
        text
    }

    /// A whole record, or `None`. A record naming pid 0 or 1 is none: no helper has either
    /// ([`helper_pid`]). One without a start time, which an earlier build wrote, reads with
    /// `birth_us` 0: its helper is [`Holder::Unproven`], never taken for live or signalled.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let v = aterm_json::from_str::<aterm_json::Value>(text).ok()?;
        let text_of = |key: &str| v.get(key).and_then(aterm_json::Value::as_str);
        let opt = |key: &str| text_of(key).map(str::to_string);
        Some(Self {
            pid: u32::try_from(v.get("pid")?.as_u64()?)
                .ok()
                .filter(|pid| helper_pid(*pid))?,
            uniq: v.get("uniq")?.as_u64()?,
            birth_us: match v.get("birth_us") {
                None => 0,
                Some(birth) => birth.as_u64()?,
            },
            helper_sha256: opt("helper_sha256")?,
            proto: u32::try_from(v.get("proto")?.as_u64()?).ok()?,
            live_disk: PathBuf::from(text_of("live_disk")?),
            started: v.get("started")?.as_u64()?,
            boot_id: opt("boot_id"),
            agent_sha256: opt("agent_sha256"),
        })
    }

    /// Write this record at `path` by rename, so a reader sees the old record or the new one,
    /// never part of either.
    ///
    /// # Errors
    /// The write or the rename failed.
    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        aterm_uds::atomic_file::write(path, self.json().as_bytes(), 0o600)
    }

    /// The record at `path`, where one is whole.
    #[must_use]
    pub fn read(path: &Path) -> Option<Self> {
        Self::parse(&std::fs::read_to_string(path).ok()?)
    }
}

/// Whether `pid` can be a helper's: never 0, which `kill` reads as the caller's whole
/// process group and whose unique id `proc_pidinfo` reads as 0, and never 1 (launchd).
#[must_use]
pub const fn helper_pid(pid: u32) -> bool {
    pid > 1
}

/// What a pid's unique id and start time are now, for [`holder`]: the kernel's in the
/// helper's arm, a script in the tests'.
pub trait Pids {
    /// `pid`'s unique id, or `None` when no process has it.
    fn uniq(&self, pid: u32) -> Option<u64>;
    /// `pid`'s kernel start time in microseconds since the Unix epoch, or `None` when no
    /// process has it or it cannot be read.
    fn birth_us(&self, pid: u32) -> Option<u64>;
}

/// This machine's processes (`proc_pidinfo`'s `PROC_PIDUNIQIDENTIFIERINFO`).
#[derive(Clone, Copy, Debug, Default)]
pub struct Kernel;

impl Pids for Kernel {
    fn uniq(&self, pid: u32) -> Option<u64> {
        #[cfg(target_vendor = "apple")]
        {
            let pid = i32::try_from(pid).ok()?;
            aterm_uds::process::uniq_identifier(pid).map(|info| info.p_uniqueid)
        }
        #[cfg(not(target_vendor = "apple"))]
        {
            let _ = pid;
            None
        }
    }

    fn birth_us(&self, pid: u32) -> Option<u64> {
        let birth = aterm_uds::process::read_process_birth(i32::try_from(pid).ok()?)?;
        birth
            .seconds
            .checked_mul(1_000_000)?
            .checked_add(birth.microseconds)
    }
}

/// Whether `record` names a live helper: its pid ([`helper_pid`]) holds the record's unique id
/// and start time now, by `pids`. A record with no start time names none ([`Holder::Unproven`]).
#[must_use]
pub fn is_live(record: &Record, pids: &dyn Pids) -> bool {
    helper_pid(record.pid)
        && record.birth_us != 0
        && pids.uniq(record.pid) == Some(record.uniq)
        && pids.birth_us(record.pid) == Some(record.birth_us)
}

/// Whether the helper `record` names may still run, for a wait on its end and never for a
/// signal: [`is_live`], or, for a record with no start time ([`Holder::Unproven`]), its pid
/// still holding the record's unique id, which no other process takes within a boot. A record
/// from an earlier boot can match a stranger there, so such a wait ends at its hang detector,
/// never in a signal.
#[must_use]
pub fn may_run(record: &Record, pids: &dyn Pids) -> bool {
    is_live(record, pids)
        || (record.birth_us == 0
            && helper_pid(record.pid)
            && pids.uniq(record.pid) == Some(record.uniq))
}

/// Who a record names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Holder {
    /// No record.
    None,
    /// A live helper: its pid's unique id and start time are the record's ([`is_live`]).
    Live(Record),
    /// A record with no start time (an earlier build wrote it): its helper may run, in this
    /// boot, or the record may be an earlier boot's, so it is never taken for live or signalled;
    /// the client asks it to `Stop` over its socket ([`may_run`]).
    Unproven(Record),
    /// A dead helper: its pid is gone, or now names another process (its unique id or start
    /// time differs).
    Dead(Record),
}

/// Who the record at `paths` names, by `pids`.
#[must_use]
pub fn holder(paths: &Paths, pids: &dyn Pids) -> Holder {
    match Record::read(&paths.record()) {
        None => Holder::None,
        Some(r) if r.birth_us == 0 => Holder::Unproven(r),
        Some(r) if is_live(&r, pids) => Holder::Live(r),
        #[cfg(any(test, debug_assertions))]
        Some(r)
            if crate::fault::armed(crate::fault::Fault::PidOnly) && pids.uniq(r.pid).is_some() =>
        {
            Holder::Live(r)
        }
        Some(r) => Holder::Dead(r),
    }
}

/// A dead helper's leftovers: its record, its socket and its boot's key disk, removed so the
/// next bring-up boots and no key outlives its VM (a helper SIGKILLed by the escalation
/// cannot remove its own, measured in V4). Only the leftovers of the helper named `dead` are
/// removed, so what a new helper wrote meanwhile is kept.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "vz_lifecycle",
        action = "ClearDead",
        project = "conformance_lifecycle::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "vz_lifecycle",
        action = "Escalate",
        project = "conformance_lifecycle::project"
    )
)]
pub fn remove_leftovers(paths: &Paths, dead: &Record) {
    if Record::read(&paths.record()).as_ref() == Some(dead) {
        let _ = std::fs::remove_file(paths.record());
        let _ = std::fs::remove_file(paths.socket());
        let _ = std::fs::remove_file(paths.key_disk());
    }
}

/// `SIGKILL` the helper `record` names, only after its pid's unique id and start time are read
/// again and are the record's (§5.1): a reused pid is never signalled, in this boot or a later
/// one. Its XPC VM exits by itself within half a second (fact 44).
///
/// # Errors
/// The pid is 0 or 1 ([`helper_pid`]), the record names no start time ([`Holder::Unproven`]),
/// the unique id or start time differs, the start time cannot be read, or the pid is gone
/// (nothing was sent), or the kernel refused the signal.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "vz_lifecycle",
        action = "Escalate",
        project = "conformance_lifecycle::project"
    )
)]
pub fn kill_helper(record: &Record, pids: &dyn Pids) -> Result<(), String> {
    if !helper_pid(record.pid) {
        return Err(format!(
            "pid {} is never a helper's: not signalled",
            record.pid
        ));
    }
    if record.birth_us == 0 {
        return Err(format!(
            "helper {}'s record names no start time (an earlier build wrote it), so it cannot be \
             told from a later process at that pid: not signalled",
            record.pid
        ));
    }
    match pids.uniq(record.pid) {
        Some(uniq) if uniq == record.uniq => {}
        #[cfg(any(test, debug_assertions))]
        Some(_) if crate::fault::armed(crate::fault::Fault::PidOnly) => {}
        Some(uniq) => {
            return Err(format!(
                "pid {} is now another process (unique id {uniq}, the record's {}): not signalled",
                record.pid, record.uniq
            ));
        }
        None => return Err(format!("pid {} is gone: not signalled", record.pid)),
    }
    match pids.birth_us(record.pid) {
        Some(birth) if birth == record.birth_us => {}
        #[cfg(any(test, debug_assertions))]
        Some(_) if crate::fault::armed(crate::fault::Fault::PidOnly) => {}
        Some(birth) => {
            return Err(format!(
                "pid {} is now another process (started {birth} us, the record's {}): not \
                 signalled",
                record.pid, record.birth_us
            ));
        }
        None => {
            return Err(format!(
                "pid {}'s start cannot be read: not signalled",
                record.pid
            ));
        }
    }
    let pid = i32::try_from(record.pid).map_err(|_| format!("pid {} past i32", record.pid))?;
    // SAFETY: two integers; one pid, never a group (`helper_pid` held it above 1, and its
    // unique id and start time were just read again).
    if unsafe { libc::kill(pid, libc::SIGKILL) } == 0 {
        Ok(())
    } else {
        Err(format!(
            "SIGKILL to helper {pid}: {}",
            std::io::Error::last_os_error()
        ))
    }
}

/// The live disk's `flock`, held for the helper's whole life and let go by `LOCK_UN` (the
/// workspace's flock rule) when this is dropped.
#[derive(Debug)]
pub struct DiskLock {
    file: std::fs::File,
}

impl DiskLock {
    /// Take the lock at `path` without waiting: `Ok(None)` while another process holds it.
    ///
    /// # Errors
    /// The lock file could not be opened, or the lock failed otherwise.
    pub fn try_take(path: &Path) -> std::io::Result<Option<Self>> {
        use std::os::unix::fs::OpenOptionsExt as _;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC)
            .open(path)?;
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's own take: its Drop releases by LOCK_UN"
        )]
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { file })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(e)) => Err(e),
        }
    }
}

impl DiskLock {
    /// The lock file opened without the lock: the negative control's helper that does not
    /// stop at a held `flock` ([`crate::fault::Fault::NoFlock`]).
    #[cfg(any(test, debug_assertions))]
    pub(crate) fn unheld(path: &Path) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?;
        Ok(Self { file })
    }
}

impl Drop for DiskLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use super::*;

    /// Each scripted pid's unique id; its start time is [`BIRTH`] unless `births` says.
    struct Script(
        RefCell<HashMap<u32, u64>>,
        RefCell<HashMap<u32, Option<u64>>>,
    );

    const BIRTH: u64 = 1_792_000_000_000_000;

    impl Script {
        fn of(pids: impl IntoIterator<Item = (u32, u64)>) -> Self {
            Self(
                RefCell::new(pids.into_iter().collect()),
                RefCell::new(HashMap::new()),
            )
        }
    }

    impl Pids for Script {
        fn uniq(&self, pid: u32) -> Option<u64> {
            self.0.borrow().get(&pid).copied()
        }

        fn birth_us(&self, pid: u32) -> Option<u64> {
            self.uniq(pid)?;
            self.1.borrow().get(&pid).copied().unwrap_or(Some(BIRTH))
        }
    }

    fn record(pid: u32, uniq: u64) -> Record {
        Record {
            pid,
            uniq,
            birth_us: BIRTH,
            helper_sha256: "ab".repeat(32),
            proto: crate::proto::PROTO,
            live_disk: "/vz/live/aterm-linux.img".into(),
            started: 1_792_000_000,
            boot_id: None,
            agent_sha256: Some("cd".repeat(32)),
        }
    }

    #[test]
    fn a_record_round_trips_and_a_partial_one_is_none() {
        let r = record(4242, 77);
        assert_eq!(Record::parse(&r.json()), Some(r.clone()));
        let mut hello = r;
        hello.boot_id = Some("b00t".into());
        assert_eq!(Record::parse(&hello.json()), Some(hello));
        assert_eq!(Record::parse("{\"pid\": 1}"), None);
        assert_eq!(Record::parse("not json"), None);
    }

    #[test]
    fn a_record_names_a_live_helper_only_by_its_unique_id() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::at(dir.path());
        std::fs::create_dir_all(paths.run_dir()).unwrap();
        let pids = Script::of([(4242, 77)]);
        assert_eq!(holder(&paths, &pids), Holder::None);
        let r = record(4242, 77);
        r.write(&paths.record()).unwrap();
        assert_eq!(holder(&paths, &pids), Holder::Live(r.clone()));
        // The negative control: the same pid, another process.
        pids.0.borrow_mut().insert(4242, 78);
        assert_eq!(holder(&paths, &pids), Holder::Dead(r.clone()));
        pids.0.borrow_mut().clear();
        assert_eq!(holder(&paths, &pids), Holder::Dead(r.clone()));
        std::fs::write(paths.socket(), b"").unwrap();
        std::fs::write(paths.key_disk(), b"").unwrap();
        remove_leftovers(&paths, &r);
        assert!(!paths.record().exists() && !paths.socket().exists());
        assert!(!paths.key_disk().exists(), "no key outlives its VM");
    }

    #[test]
    fn leftovers_of_another_record_are_kept() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::at(dir.path());
        std::fs::create_dir_all(paths.run_dir()).unwrap();
        let newer = record(5000, 1);
        newer.write(&paths.record()).unwrap();
        std::fs::write(paths.key_disk(), b"").unwrap();
        remove_leftovers(&paths, &record(4242, 77));
        assert_eq!(Record::read(&paths.record()), Some(newer));
        assert!(
            paths.key_disk().exists(),
            "the newer helper's key disk stays"
        );
    }

    #[test]
    fn a_pid_whose_unique_id_differs_is_never_signalled() {
        // The pid is this test's own: a signal would end the test, so reaching the end shows
        // none was sent.
        let me = std::process::id();
        let pids = Script::of([(me, 2)]);
        let why = kill_helper(&record(me, 1), &pids).expect_err("refused");
        assert!(
            why.contains("another process") && why.contains("not signalled"),
            "{why}"
        );
        pids.0.borrow_mut().clear();
        let why = kill_helper(&record(me, 1), &pids).expect_err("gone");
        assert!(why.contains("gone"), "{why}");
    }

    /// A record from an earlier boot: its pid and unique id match a process of this boot (every
    /// process a boot makes before its first pid wrap has a unique id equal to its pid), and
    /// only the start time tells them apart. That process is no helper and is never signalled.
    /// Negative control: judged by unique id alone, it reads live.
    #[test]
    fn a_record_from_an_earlier_boot_names_no_live_helper_and_nothing_is_signalled() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::at(dir.path());
        std::fs::create_dir_all(paths.run_dir()).unwrap();
        // The pid is this test's own: a signal would end the test.
        let me = std::process::id();
        let earlier = record(me, u64::from(me));
        earlier.write(&paths.record()).unwrap();
        let pids = Script::of([(me, u64::from(me))]);
        assert_eq!(
            holder(&paths, &pids),
            Holder::Live(earlier.clone()),
            "the same start"
        );
        pids.1
            .borrow_mut()
            .insert(me, Some(BIRTH + 4 * 86_400_000_000));
        assert_eq!(
            holder(&paths, &pids),
            Holder::Dead(earlier.clone()),
            "a later start"
        );
        assert!(!is_live(&earlier, &pids));
        let why = kill_helper(&earlier, &pids).expect_err("another process");
        assert!(
            why.contains("another process") && why.contains("not signalled"),
            "{why}"
        );
        pids.1.borrow_mut().insert(me, None);
        assert_eq!(
            holder(&paths, &pids),
            Holder::Dead(earlier.clone()),
            "start unread"
        );
        let why = kill_helper(&earlier, &pids).expect_err("unread");
        assert!(
            why.contains("cannot be read") && why.contains("not signalled"),
            "{why}"
        );
    }

    /// A record an earlier build wrote, with no start time, is unproven: never live and never
    /// signalled, though its pid holds its unique id; a wait on its end watches the unique id.
    /// A start time that is no number leaves the record unwhole. Negative control: the same
    /// record with its start time is live and may be signalled past this gate.
    #[test]
    fn a_record_without_a_start_time_is_unproven_and_never_signalled() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::at(tmp.path().join("vz.noindex"));
        std::fs::create_dir_all(paths.run_dir()).unwrap();
        let mut v = aterm_json::from_str::<aterm_json::Value>(&record(4242, 77).json()).unwrap();
        v.as_object_mut().unwrap().remove("birth_us");
        let text = aterm_json::to_string(&v).unwrap();
        let earlier = Record::parse(&text).expect("whole");
        assert_eq!(
            earlier,
            Record {
                birth_us: 0,
                ..record(4242, 77)
            }
        );
        std::fs::write(paths.record(), &text).unwrap();
        let pids = Script::of([(4242, 77)]);
        assert_eq!(holder(&paths, &pids), Holder::Unproven(earlier.clone()));
        assert!(!is_live(&earlier, &pids));
        assert!(may_run(&earlier, &pids), "its pid holds its unique id");
        assert!(
            !may_run(&earlier, &Script::of([(4242, 78)])),
            "another process"
        );
        assert!(!may_run(&earlier, &Script::of([])), "gone");
        let why = kill_helper(&earlier, &pids).expect_err("never signalled");
        assert!(
            why.contains("no start time") && why.contains("not signalled"),
            "{why}"
        );
        assert!(is_live(&record(4242, 77), &pids), "with its start time");
        v.as_object_mut()
            .unwrap()
            .insert("birth_us".into(), aterm_json::Value::from("soon"));
        assert_eq!(Record::parse(&aterm_json::to_string(&v).unwrap()), None);
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn the_kernel_reads_this_process_s_start_time() {
        let me = std::process::id();
        let birth = Kernel.birth_us(me).expect("this process's start");
        assert!(birth > 1_700_000_000_000_000, "{birth}");
        assert_eq!(Kernel.birth_us(me), Some(birth), "stable");
    }

    #[test]
    fn pid_0_and_pid_1_are_never_a_helpers_and_never_signalled() {
        // `proc_pidinfo` reads pid 0 with unique id 0 and pid 1 with 1 on macOS, so a record
        // naming either would pass the unique-id gate: it is no record at all.
        for pid in [0u32, 1] {
            assert!(!helper_pid(pid));
            let text = record(pid, u64::from(pid)).json();
            assert_eq!(Record::parse(&text), None, "pid {pid}");
        }
        // The negative control: pid 2 is a record.
        assert!(helper_pid(2));
        assert_eq!(Record::parse(&record(2, 2).json()), Some(record(2, 2)));
        // A record built in code naming pid 1 with its unique id is refused before any kill
        // (were it sent, launchd would refuse it: EPERM, other words).
        let pids = Script::of([(1, 1)]);
        let why = kill_helper(&record(1, 1), &pids).expect_err("refused");
        assert_eq!(why, "pid 1 is never a helper's: not signalled");
    }

    #[test]
    fn a_socket_path_past_sun_path_is_refused_by_name() {
        let short = Paths::at("/tmp/vz");
        assert!(short.checked_socket().is_ok());
        let long = Paths::at(format!("/tmp/{}", "d".repeat(90)));
        let why = long.checked_socket().expect_err("too long");
        assert!(why.contains("past sun_path's 104"), "{why}");
    }

    #[test]
    fn the_disk_lock_is_held_once_and_let_go_on_drop() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("aterm-linux.img.lock");
        let first = DiskLock::try_take(&path).unwrap().expect("free");
        assert!(DiskLock::try_take(&path).unwrap().is_none(), "held");
        drop(first);
        assert!(DiskLock::try_take(&path).unwrap().is_some(), "let go");
    }
}
