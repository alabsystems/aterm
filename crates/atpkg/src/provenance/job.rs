// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The heal's launchd job ([`super::heal`]): `/bin/sh -c <script>`, submitted with
//! `launchctl submit` so that launchd — never the submitter, which may be tracked — is its
//! parent, and made only of platform binaries, which cannot carry the tag. The script
//! records its status in `$1` (temp + rename) and removes its own label (`$2`) as its last
//! acts, and exits 0: `launchctl submit` keeps a failed job alive and re-spawns it. The
//! submitter waits a bounded time for that status ([`Job::wait_for_status`]) and removes the
//! label and the job's scratch when the [`Job`] drops, whatever happened.
//!
//! A submitter killed before the drop (a `kill -9`, a test under a timeout) leaves its label
//! registered, so [`Job::prepare`] first stops every job of ours whose owning pid is gone and
//! removes its scratch. The stems of the untracked lanes retired on 2026-09-24 are still
//! recognized, so what an older build left behind — a bundle replica was about 51 MB — is
//! reclaimed too.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The label prefix every job of this crate carries; the sweeps look for it.
const LABEL_PREFIX: &str = "systems.alab.atpkg.";

/// The stems a job of ours carries — the heal's, and the retired lanes' — and the ONLY
/// names the dead-pid sweeps remove: a name whose stem is not one of these is not ours,
/// whatever the rest of it looks like, and the sweep DELETES what it accepts.
const JOB_STEMS: &[&str] = &[
    "heal",
    "stage-helper",
    "lay-helper",
    "view-helper",
    "replica",
];

/// How long a teardown waits for launchd to forget a removed job: `/bin/sh` dies at the
/// signal and launchd unloads the label in well under a second.
const TEARDOWN_BOUND: Duration = Duration::from_secs(5);

/// One submitted job: its `0700` scratch, its label and the status file its script writes.
pub(super) struct Job {
    dir: PathBuf,
    label: String,
    status: PathBuf,
}

impl Job {
    /// Create the job's `0700` scratch under `scratch`, named `<stem>-<pid>-<seq>-<nonce>`
    /// (the label is `systems.alab.atpkg.` + that name), after stopping the jobs dead
    /// submitters left behind.
    pub(super) fn prepare(scratch: &Path, stem: &str) -> Result<Self, String> {
        stop_orphaned_jobs();
        sweep_dead_job_dirs(scratch);
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let name = format!("{stem}-{}-{seq}-{nonce:x}", std::process::id());
        let dir = scratch.join(&name);
        // `create_dir`, not `create_dir_all`: a directory already there under this name
        // would be adopted with whatever stale `status` it holds.
        std::fs::create_dir(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        crate::platform::set_mode(&dir, 0o700)
            .map_err(|e| format!("chmod {}: {e}", dir.display()))?;
        Ok(Self {
            label: format!("{LABEL_PREFIX}{name}"),
            status: dir.join("status"),
            dir,
        })
    }

    /// `launchctl submit` `/bin/sh -c <script>` with `$0` = `name`, `$1` = the status file,
    /// `$2` = this job's label, and `args` after them.
    pub(super) fn submit(
        &mut self,
        name: &str,
        script: &str,
        args: &[&std::ffi::OsStr],
    ) -> Result<(), String> {
        let out = std::process::Command::new("/bin/launchctl")
            .arg("submit")
            .args(["-l", &self.label])
            .arg("--")
            .arg("/bin/sh")
            .arg("-c")
            .arg(script)
            .arg(name)
            .arg(&self.status)
            .arg(&self.label)
            .args(args)
            .output()
            .map_err(|e| format!("spawn /bin/launchctl: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "launchctl submit failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(())
    }

    /// Wait at most `bound` for the status the script records, and return it — `None` when
    /// the bound passed first, or when launchd no longer lists the job and it recorded
    /// nothing (the script records its status BEFORE it removes its own label, so a label
    /// gone without a status is a job that died).
    pub(super) fn wait_for_status(&self, bound: Duration) -> Option<i32> {
        let started = Instant::now();
        let mut last_look = started;
        loop {
            if let Some(status) = self.read_status() {
                return Some(status);
            }
            if started.elapsed() >= bound {
                return None;
            }
            if last_look.elapsed() >= Duration::from_millis(500) {
                last_look = Instant::now();
                if !label_is_listed(&self.label) {
                    return self.read_status();
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn read_status(&self) -> Option<i32> {
        std::fs::read_to_string(&self.status)
            .ok()?
            .lines()
            .next()?
            .trim()
            .parse()
            .ok()
    }

    /// This job's launchd label.
    #[cfg(test)]
    fn label(&self) -> &str {
        &self.label
    }
}

impl Drop for Job {
    /// Remove the label and WAIT for launchd to forget it (a job that already removed its
    /// own costs one `launchctl list`), then the scratch.
    fn drop(&mut self) {
        let _ = std::process::Command::new("/bin/launchctl")
            .args(["remove", &self.label])
            .output();
        let started = Instant::now();
        while started.elapsed() < TEARDOWN_BOUND && label_is_listed(&self.label) {
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Stop every job of ours whose submitting process is gone, and wait for launchd to forget
/// it. A label whose pid is alive is left alone (another heal in flight, or a reused pid),
/// and so is every label that is not ours ([`owner_pid_of_label`]). Best effort: a
/// `launchctl` that cannot list is simply no sweep.
fn stop_orphaned_jobs() {
    let Ok(out) = std::process::Command::new("/bin/launchctl")
        .arg("list")
        .output()
    else {
        return;
    };
    let me = std::process::id();
    let mut stopped: Vec<String> = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Some(label) = line.split('\t').nth(2) else {
            continue;
        };
        let Some(pid) = owner_pid_of_label(label) else {
            continue;
        };
        if pid == me || pid_exists(pid) {
            continue;
        }
        let _ = std::process::Command::new("/bin/launchctl")
            .args(["remove", label])
            .output();
        stopped.push(label.to_string());
    }
    let started = Instant::now();
    while !stopped.is_empty() && started.elapsed() < TEARDOWN_BOUND {
        stopped.retain(|label| label_is_listed(label));
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Remove every job directory under `scratch` whose `<pid>` no longer exists. Only
/// directories, only our stems ([`owner_pid_of_label`] enforces them), only a dead pid:
/// `scratch` can be the shared `$TMPDIR` (the release cutter heals from there), so a
/// foreign `<anything>-<dead pid>-<x>-<y>` directory beside ours is left alone.
fn sweep_dead_job_dirs(scratch: &Path) {
    let Ok(entries) = std::fs::read_dir(scratch) else {
        return;
    };
    let me = std::process::id();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(pid) = owner_pid_of_label(&format!("{LABEL_PREFIX}{name}")) else {
            continue;
        };
        if pid == me || pid_exists(pid) {
            continue;
        }
        let _ = std::fs::remove_dir_all(&path);
    }
}

/// Whether launchd still lists `label` — `launchctl list <label>` fails for a label it
/// does not have.
fn label_is_listed(label: &str) -> bool {
    std::process::Command::new("/bin/launchctl")
        .args(["list", label])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// The `<pid>` in a label of OURS — `systems.alab.atpkg.<stem>-<pid>-<seq>-<nonce>`, where
/// `<stem>` is one of [`JOB_STEMS`] (some carry a dash, so the fields are read from the
/// right) and `<seq>`/`<nonce>` are what [`Job::prepare`] mints. `None` for any other label:
/// the answer decides what gets deleted.
fn owner_pid_of_label(label: &str) -> Option<u32> {
    let rest = label.strip_prefix(LABEL_PREFIX)?;
    let mut fields = rest.rsplitn(4, '-');
    let nonce = fields.next()?;
    let seq = fields.next()?;
    let pid = fields.next()?;
    let stem = fields.next()?;
    if !JOB_STEMS.contains(&stem)
        || nonce.is_empty()
        || !nonce.bytes().all(|b| b.is_ascii_hexdigit())
        || seq.parse::<u64>().is_err()
    {
        return None;
    }
    pid.parse().ok()
}

/// Whether a process with `pid` exists (`kill(pid, 0)`: `ESRCH` is the only "no").
fn pid_exists(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return true;
    };
    // SAFETY: `kill` with signal 0 delivers nothing; it only asks the kernel whether the
    // pid is addressable.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A job that does not finish inside its bound is answered `None`, and dropping it
    /// removes its label from launchd and its scratch from disk — the heal's timeout path.
    /// One that finishes answers the status it recorded, and its own label is gone with it.
    #[test]
    fn a_job_is_waited_for_within_its_bound_and_removed_on_every_path() {
        let scratch =
            std::env::temp_dir().join(format!("atpkg.platform.job.{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).unwrap();

        // Finishes: records 7, removes its own label.
        let mut job = Job::prepare(&scratch, "heal").unwrap();
        job.submit(
            "atpkg-test",
            r#"printf '%s\n' 7 > "$1.tmp" && /bin/mv -f "$1.tmp" "$1"; /bin/launchctl remove "$2"; exit 0"#,
            &[],
        )
        .unwrap();
        assert_eq!(job.wait_for_status(Duration::from_secs(20)), Some(7));
        let label = job.label().to_string();
        drop(job);
        assert!(!label_is_listed(&label), "{label} removed itself");

        // Never finishes: the bound answers `None`, and the drop removes the label.
        let mut job = Job::prepare(&scratch, "heal").unwrap();
        job.submit("atpkg-test", "exec /bin/sleep 60", &[]).unwrap();
        let started = Instant::now();
        assert_eq!(job.wait_for_status(Duration::from_millis(700)), None);
        assert!(started.elapsed() < Duration::from_secs(5));
        let label = job.label().to_string();
        assert!(label_is_listed(&label), "still running before the drop");
        drop(job);
        assert!(!label_is_listed(&label), "{label} removed by the drop");
        let left: Vec<_> = std::fs::read_dir(&scratch).unwrap().flatten().collect();
        assert!(left.is_empty(), "no job scratch left: {left:?}");
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// The sweeps read the owning pid off our labels only — the heal's and the retired
    /// lanes' stems — never an integration test's `test.` label, a foreign label, or a
    /// foreign name that merely has our SHAPE: the stem and the seq and nonce fields are
    /// read, not discarded, because the answer decides what gets deleted.
    #[test]
    fn the_sweep_reads_the_owning_pid_off_our_labels_only() {
        for (label, pid) in [
            (
                "systems.alab.atpkg.heal-4281-2-18d4bf61975232a9",
                Some(4281),
            ),
            (
                "systems.alab.atpkg.stage-helper-4281-0-18d4bf618a493520",
                Some(4281),
            ),
            (
                "systems.alab.atpkg.replica-4281-0-18d4bf618a493520",
                Some(4281),
            ),
            ("systems.alab.atpkg.test.clean.123", None),
            ("com.apple.Finder", None),
            ("systems.alab.atpkg.odd", None),
            ("systems.alab.atpkg.mytool-48213-1-a7f3", None),
            ("systems.alab.atpkg.a-b-4281-0-ab", None),
            ("systems.alab.atpkg.com.example.tool-4281-0-ab", None),
            ("systems.alab.atpkg.lay-helper-4281-x-18d4bf61", None),
            ("systems.alab.atpkg.lay-helper-4281-0-nonce", None),
            ("systems.alab.atpkg.heal-4281-0-", None),
        ] {
            assert_eq!(owner_pid_of_label(label), pid, "{label}");
        }
        assert!(pid_exists(std::process::id()), "this process exists");
        assert!(pid_exists(1), "launchd exists (EPERM is not ESRCH)");
    }

    /// The dead-job sweep DELETES, and the scratch it walks can be the shared `$TMPDIR` —
    /// so a foreign directory that merely looks pid-stamped, left by another tool under a
    /// pid that has since exited, survives it, while our own dead job's scratch (a retired
    /// lane's included) is taken and a live sibling's is left alone.
    #[test]
    fn the_dead_job_sweep_spares_a_foreign_directory_of_the_same_shape() {
        // Dots, not dashes: this scratch itself must never read as a job name.
        let scratch =
            std::env::temp_dir().join(format!("atpkg.sweep.scratch.{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).unwrap();
        // A pid nothing runs under, measured rather than guessed and taken WITHOUT
        // spawning: macOS pids stay below 99999.
        let dead = (90_000..99_999u32)
            .rev()
            .find(|p| !pid_exists(*p))
            .expect("some pid below 99999 is unused");
        let me = std::process::id();

        let foreign = scratch.join(format!("mytool-{dead}-1-a7f3"));
        let ours_dead = scratch.join(format!("heal-{dead}-0-18d4bf618a493520"));
        let retired_dead = scratch.join(format!("replica-{dead}-0-18d4bf618a493521"));
        let ours_live = scratch.join(format!("heal-{me}-0-18d4bf61975232a8"));
        for d in [&foreign, &ours_dead, &retired_dead, &ours_live] {
            std::fs::create_dir(d).unwrap();
            std::fs::write(d.join("keep"), b"x").unwrap();
        }

        sweep_dead_job_dirs(&scratch);

        assert!(foreign.join("keep").exists(), "{}", foreign.display());
        assert!(!ours_dead.exists(), "{}", ours_dead.display());
        assert!(!retired_dead.exists(), "{}", retired_dead.display());
        assert!(ours_live.join("keep").exists(), "{}", ours_live.display());
        let _ = std::fs::remove_dir_all(&scratch);
    }
}
