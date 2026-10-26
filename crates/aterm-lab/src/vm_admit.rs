// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A LANE VM'S ADMISSION (`docs/DESIGN-adaptive-build-2026-10-24.md`, VM-ADMIT). Before a
//! Linux (lima or vz) or Windows VM boots, its run claims the guest's memory and vCPUs in the
//! machine's capacity ledger ([`aterm_capacity`]) and waits while they do not fit beside the
//! claims already held, naming them once a minute; it is NOT RUN at once only where the guest
//! and the memory macOS keeps beside it ([`aterm_vz::plan::HOST_MARGIN`], the margin, never
//! reserved) reach what this Mac can ever have available, its memory less what is wired
//! ([`aterm_capacity::never`]), and an interrupt ends the wait as NOT RUN, nothing booted.
//! A run on a VM already up claims its vCPUs alone, which never waits. The guest's builds run
//! as many jobs as cores the claim was granted (`CARGO_BUILD_JOBS`); the guest's shape stays
//! the owner's.
//!
//! Once booted, the VM's processes ([`vm_procs`]: QEMU, lima's host agent, the
//! Virtualization.framework process or krunkit holding its disk) are credited to the claim
//! ([`Admitted::credit`]). The system parents them to launchd, not to this run, so uncredited
//! the memory they hold would count twice: missing from what is available, and reserved again.
//!
//! The run that boots a lima or Windows VM is recorded beside it ([`Booter`]) before the boot,
//! and the VM process its boot started (lima's host agent, QEMU) once the boot is up
//! ([`record_vm`]). A VM found running whose booter was a one-shot run that has died is that
//! run's leftover ([`Fate::Leftover`]): the next run stops it through the lane's own `down` and
//! boots afresh. A VM a run kept up (`up`, `--keep-up`) is adopted and left running, as before,
//! and so is one whose record names a VM process that has ended: that boot is over, and the VM
//! running now was started since outside these records. (The vz arm's helper already powers its
//! VM off when the last session closes.)

use std::path::{Path, PathBuf};

use aterm_capacity::{Admit, Claim, Error, Ledger, Machine, Need, Pace};
use aterm_json::{Map, Value};

use crate::Fail;

/// A lane VM's fixed shape: the owner's sizes, which admission never changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Guest {
    /// The lane, as its lines begin: `linux`, `windows`.
    pub lane: &'static str,
    /// The VM's name.
    pub vm: &'static str,
    /// Its memory, bytes.
    pub memory: u64,
    /// Its vCPUs.
    pub cpus: u32,
}

/// lima's `aterm-linux` ([`crate::linux::CPUS`], [`crate::linux::MEMORY_GIB`]).
pub const LIMA: Guest = Guest {
    lane: "linux",
    vm: crate::linux::VM,
    memory: (crate::linux::MEMORY_GIB as u64) << 30,
    cpus: crate::linux::CPUS,
};

/// The vz arm's `aterm-linux` ([`aterm_vz::plan::MEMORY`], [`aterm_vz::plan::CPUS`]).
pub const VZ: Guest = Guest {
    lane: "linux",
    vm: crate::linux::VM,
    memory: aterm_vz::plan::MEMORY,
    cpus: aterm_vz::plan::CPUS,
};

/// The Windows lane's `aterm-windows` ([`crate::windows::CPUS`], [`crate::windows::MEMORY_GIB`]).
pub const WINDOWS: Guest = Guest {
    lane: "windows",
    vm: crate::windows::VM,
    memory: (crate::windows::MEMORY_GIB as u64) << 30,
    cpus: crate::windows::CPUS,
};

impl Guest {
    /// lima's instance `vm` ([`LIMA`]'s shape: the lane's VM, or its GPU instance under `--gpu`,
    /// which lima makes the same size).
    #[must_use]
    pub const fn lima(vm: &'static str) -> Guest {
        Guest { vm, ..LIMA }
    }

    /// The claim `verb` (`lab linux proof`) takes on this VM: to boot it (`booting`), the
    /// guest's memory with the host margin spare beside it, and its vCPUs; on a VM already up,
    /// its vCPUs alone, which only sizes the guest's jobs and never waits.
    #[must_use]
    pub fn need(&self, verb: &str, booting: bool) -> Need {
        if booting {
            Need::new(format!(
                "{verb}: {} boot ({}, {} vCPUs)",
                self.vm,
                aterm_capacity::gib(self.memory),
                self.cpus
            ))
            .mem(self.memory)
            .margin(aterm_vz::plan::HOST_MARGIN)
            .cores(self.cpus)
        } else {
            Need::new(format!("{verb}: guest jobs on {}", self.vm)).cores(self.cpus)
        }
    }
}

/// A run's claim on its VM, held until the run ends, and the jobs its guest's builds run.
#[derive(Debug)]
pub struct Admitted {
    /// The claim; `None` where the ledger could not be used (the run goes on unclaimed). It is
    /// let go when the run's `Up` drops.
    pub claim: Option<Claim>,
    /// `CARGO_BUILD_JOBS` for the guest: the cores the claim was granted, else every vCPU.
    pub jobs: u32,
}

impl Admitted {
    /// Credit the boot's claim with the booted VM's processes `pids` ([`vm_procs`]), so the
    /// memory they hold counts against it rather than twice. A claim of no memory (a run on a VM
    /// already up) takes none; a credit that cannot be written is said, and the run goes on.
    pub fn credit(&mut self, lane: &str, pids: &[u32]) {
        let Some(claim) = self.claim.as_mut().filter(|c| c.record().need.mem > 0) else {
            return;
        };
        if let Err(e) = claim.credit(pids) {
            eprintln!(
                "aterm-lab: {lane}: the VM's processes are not credited to its claim ({}: {e}); \
                 its memory counts twice in later admissions while this run holds it",
                claim.path().display()
            );
        }
    }
}

/// The processes that run a booted VM: `also` (QEMU's pid, lima's host agent, the vz helper)
/// and every process of this user holding the VM's `disk` open (Virtualization.framework's VM
/// process, krunkit, QEMU), this one left out. Where the holders cannot be scanned, `also`.
#[must_use]
pub fn vm_procs(disk: &Path, also: &[u32]) -> Vec<u32> {
    let me = std::process::id();
    let mut pids: Vec<u32> = also.to_vec();
    match aterm_uds::holders::scan_file_holders(disk, &[me]) {
        Ok(scan) => pids.extend(scan.holders),
        Err(e) => eprintln!(
            "aterm-lab: the processes holding {} were not read ({e}): the VM is credited with \
             {also:?} alone",
            disk.display()
        ),
    }
    pids.retain(|p| *p != me);
    pids.sort_unstable();
    pids.dedup();
    pids
}

/// Claim `guest` for this run ([`Guest::need`]) in this user's ledger on the live machine,
/// waiting while it does not fit, each line said as `<lane>: <line>`, until an interrupt
/// ([`crate::stop`]). The claim names this checkout, not the lab's working directory (`/`).
///
/// # Errors
/// NOT RUN where the guest can never fit on this Mac, or an interrupt ended the wait.
pub fn admit(guest: &Guest, booting: bool) -> Result<Admitted, Fail> {
    let verb = crate::linux::run_verb(guest.lane);
    let need = guest.need(&verb, booting).checkout(crate::linux::repo());
    let mode = crate::linux::run_admit();
    let say = &mut |line: &str| println!("{}: {line}", guest.lane);
    match Ledger::user() {
        Some(ledger) => admit_in(
            &ledger,
            &aterm_capacity::Live,
            need,
            guest,
            mode,
            Pace::default(),
            say,
            &crate::stop::check,
        ),
        None => Ok(unclaimed(
            guest,
            "no home to keep the capacity ledger under",
        )),
    }
}

/// [`admit`] in `ledger` on `machine`, at `pace`, its wait ended when `stopped` says an
/// interrupt came.
///
/// # Errors
/// As [`admit`]; an interrupt's NOT RUN carries its signal.
#[expect(
    clippy::too_many_arguments,
    reason = "admission's own seven, and the interrupt that ends its wait"
)]
pub fn admit_in(
    ledger: &Ledger,
    machine: &dyn Machine,
    need: Need,
    guest: &Guest,
    mode: Admit,
    pace: Pace,
    say: &mut dyn FnMut(&str),
    stopped: &dyn Fn() -> Result<(), crate::stop::Stopped>,
) -> Result<Admitted, Fail> {
    let booting = need.mem > 0;
    match aterm_capacity::admit_until(ledger, machine, need, mode, pace, say, &|| {
        stopped().is_err()
    }) {
        Ok(claim) => {
            let jobs = claim.cores().clamp(1, guest.cpus);
            say(&format!(
                "the guest's builds run CARGO_BUILD_JOBS={jobs}: the cores this run was admitted, \
                 of the VM's {} vCPUs",
                guest.cpus
            ));
            Ok(Admitted {
                claim: Some(claim),
                jobs,
            })
        }
        Err(Error::Never(why)) => Err(crate::linux::not_run(format!(
            "{why}; the VM can never boot here"
        ))),
        Err(Error::Ledger(e)) => Ok(unclaimed(guest, &e.to_string())),
        Err(Error::Stopped { signal, short }) => Err(Fail {
            stopped: signal
                .inspect(|sig| crate::stop::note(*sig))
                .or_else(|| stopped().err().map(|s| s.0)),
            ..crate::linux::not_run(format!(
                "interrupted while waiting for capacity for {}{}; this run {}",
                guest.vm,
                short.map_or_else(String::new, |s| format!(" ({s})")),
                if booting {
                    "booted nothing"
                } else {
                    "ran nothing on it"
                }
            ))
        }),
    }
}

/// A run that goes on without a claim, saying why: its guest builds on every vCPU, as before.
fn unclaimed(guest: &Guest, why: &str) -> Admitted {
    eprintln!(
        "aterm-lab: {}: {} is not claimed in the capacity ledger ({why}); the run goes on \
         unclaimed",
        guest.lane, guest.vm
    );
    Admitted {
        claim: None,
        jobs: guest.cpus,
    }
}

/// The run that booted a lane VM, recorded beside the VM before the boot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Booter {
    /// Its pid.
    pub pid: u32,
    /// Its process's start (`seconds`, `microseconds`), so a pid another process took since is
    /// not read as it.
    pub birth: Option<(u64, u64)>,
    /// When it booted the VM, unix seconds.
    pub since: u64,
    /// What it runs (`lab windows smoke`).
    pub verb: String,
    /// The checkout it runs from.
    pub checkout: String,
    /// Whether it kept the VM up for later runs (`up`, `--keep-up`): such a VM is adopted, never
    /// stopped as a leftover.
    pub kept: bool,
    /// The VM process its boot started (lima's host agent, QEMU), once the boot is up
    /// ([`record_vm`]): a VM running under another is another boot, which this record does not
    /// describe.
    pub vm_pid: Option<u32>,
    /// That process's start, so a pid another process took since is not read as it.
    pub vm_birth: Option<(u64, u64)>,
}

impl Booter {
    /// This process, running `verb` from `checkout`, booting now.
    #[must_use]
    pub fn me(verb: &str, checkout: &Path) -> Booter {
        let pid = std::process::id();
        Booter {
            pid,
            birth: birth_of(pid),
            since: u64::try_from(crate::util::now_secs()).unwrap_or(0),
            verb: verb.to_owned(),
            checkout: checkout.to_string_lossy().into_owned(),
            kept: false,
            vm_pid: None,
            vm_birth: None,
        }
    }

    /// The record: one JSON object.
    #[must_use]
    pub fn line(&self) -> String {
        let mut o = Map::new();
        o.insert("pid".to_owned(), Value::from(self.pid));
        if let Some((s, us)) = self.birth {
            o.insert("birth_s".to_owned(), Value::from(s));
            o.insert("birth_us".to_owned(), Value::from(us));
        }
        o.insert("since".to_owned(), Value::from(self.since));
        o.insert("verb".to_owned(), Value::from(self.verb.as_str()));
        o.insert("checkout".to_owned(), Value::from(self.checkout.as_str()));
        o.insert("kept".to_owned(), Value::from(self.kept));
        if let Some(pid) = self.vm_pid {
            o.insert("vm_pid".to_owned(), Value::from(pid));
        }
        if let Some((s, us)) = self.vm_birth {
            o.insert("vm_birth_s".to_owned(), Value::from(s));
            o.insert("vm_birth_us".to_owned(), Value::from(us));
        }
        let mut line = aterm_json::to_string(&Value::Object(o)).unwrap_or_default();
        line.push('\n');
        line
    }

    /// The booter `text` ([`Booter::line`]) names.
    #[must_use]
    pub fn parse(text: &str) -> Option<Booter> {
        let v: Value = aterm_json::from_str(text.lines().next()?).ok()?;
        let num = |k: &str| v.get(k).and_then(Value::as_u64);
        let text = |k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        Some(Booter {
            pid: u32::try_from(num("pid")?).ok().filter(|p| *p > 1)?,
            birth: num("birth_s").zip(num("birth_us")),
            since: num("since").unwrap_or(0),
            verb: text("verb"),
            checkout: text("checkout"),
            kept: v.get("kept").and_then(Value::as_bool).unwrap_or(false),
            vm_pid: num("vm_pid").and_then(|p| u32::try_from(p).ok()),
            vm_birth: num("vm_birth_s").zip(num("vm_birth_us")),
        })
    }

    /// The booter the record at `path` names.
    #[must_use]
    pub fn read(path: &Path) -> Option<Booter> {
        Booter::parse(&std::fs::read_to_string(path).ok()?)
    }

    /// Write this record at `path`, whole or not at all (staged beside it, then renamed over).
    ///
    /// # Errors
    /// The directory, the staged file or the rename failed.
    pub fn record(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let staged = path.with_extension(format!("staged-{}", std::process::id()));
        std::fs::write(&staged, self.line())?;
        std::fs::rename(&staged, path).inspect_err(|_| {
            let _ = std::fs::remove_file(&staged);
        })
    }

    /// Whether this booter's process still runs: its pid, born when the record says.
    #[must_use]
    pub fn alive(&self) -> bool {
        lives(self.pid, self.birth)
    }

    /// Whether the VM process its boot started has ended, so the VM running now (if any) is
    /// another boot. `false` where none was recorded (the run died before its boot was up).
    #[must_use]
    pub fn vm_ended(&self) -> bool {
        self.vm_pid.is_some_and(|pid| !lives(pid, self.vm_birth))
    }

    /// `pid N (verb, checkout, since HH:MM)`, on this Mac's clock.
    #[must_use]
    pub fn describe(&self) -> String {
        let holder = aterm_toolchain::named_lock::Holder {
            pid: self.pid,
            since: self.since,
            verb: self.verb.clone(),
            checkout: self.checkout.clone(),
        };
        let offset = i64::try_from(self.since)
            .ok()
            .and_then(aterm_uds::zone::local_offset_at)
            .unwrap_or(0);
        holder.describe(offset)
    }
}

/// Whether `pid` runs, born at `birth` where that is known.
fn lives(pid: u32, birth: Option<(u64, u64)>) -> bool {
    match (birth_of(pid), birth) {
        (None, _) => false,
        (Some(now), Some(then)) => now == then,
        (Some(_), None) => aterm_uds::process::pid_alive(pid),
    }
}

/// `pid`'s start, `(seconds, microseconds)`.
fn birth_of(pid: u32) -> Option<(u64, u64)> {
    aterm_uds::process::read_process_birth(i32::try_from(pid).ok()?)
        .map(|b| (b.seconds, b.microseconds))
}

/// Record this run as the booter of the VM whose record is `path`, before it boots. A record
/// that cannot be written is said, and the boot goes on: its VM is then adopted, as before
/// booters were recorded, should this run die before its end.
pub fn record_boot(lane: &str, path: &Path) -> Option<PathBuf> {
    let me = Booter::me(
        &crate::linux::run_verb(lane),
        Path::new(&crate::linux::repo()),
    );
    match me.record(path) {
        Ok(()) => Some(path.to_path_buf()),
        Err(e) => {
            eprintln!(
                "aterm-lab: {lane}: this run's boot is not recorded ({}: {e})",
                path.display()
            );
            None
        }
    }
}

/// Record the VM process `pid` this run's boot started (lima's host agent, QEMU) in its booter
/// record at `path`, once the boot is up: a later run then stops as this run's leftover only
/// that boot, never a VM started since outside these records (stopped and started by hand, or
/// by a lab that keeps none). Said where it cannot be written; the record then describes any
/// VM running, as before.
pub fn record_vm(path: Option<&Path>, pid: Option<u32>) {
    let (Some(path), Some(pid)) = (path, pid) else {
        return;
    };
    let Some(mut b) = Booter::read(path).filter(|b| b.pid == std::process::id()) else {
        return;
    };
    b.vm_pid = Some(pid);
    b.vm_birth = birth_of(pid);
    if let Err(e) = b.record(path) {
        eprintln!(
            "aterm-lab: the boot's VM process (pid {pid}) is not recorded ({}: {e})",
            path.display()
        );
    }
}

/// What the next run does with a running VM, by the record of the run that booted it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fate {
    /// A run kept it up (`up`, `--keep-up`): later runs adopt it.
    Kept,
    /// Its booter still runs it: the lane lock keeps other runs out until it ends.
    Running,
    /// Its booter was a one-shot run that has died: the next run stops it as `down` does and
    /// boots afresh.
    Leftover,
}

/// The booter the record at `path` names and its VM's [`Fate`]. `None` where no record names
/// one (booted before records were kept, or by hand), or the record names a VM process that has
/// ended ([`Booter::vm_ended`]: its boot is over): such a VM is adopted.
#[must_use]
pub fn fate(path: &Path) -> Option<(Booter, Fate)> {
    let b = Booter::read(path).filter(|b| !b.vm_ended())?;
    let fate = if b.kept {
        Fate::Kept
    } else if b.alive() {
        Fate::Running
    } else {
        Fate::Leftover
    };
    Some((b, fate))
}

/// The leftover a running VM is, from the record at `path` ([`Fate::Leftover`]): its dead
/// booter.
#[must_use]
pub fn leftover(path: &Path) -> Option<Booter> {
    fate(path).and_then(|(b, f)| (f == Fate::Leftover).then_some(b))
}

/// `status`'s line for a running VM whose record is at `path`: who booted it, and what the next
/// run does with it ([`fate`]).
#[must_use]
pub fn status_line(path: &Path) -> Option<String> {
    let (b, fate) = fate(path)?;
    let fate = match fate {
        Fate::Kept => "kept up: later runs adopt it",
        Fate::Running => "which runs it",
        Fate::Leftover => "which has ended: the next run stops it as `down` does and boots afresh",
    };
    Some(format!("booted by {}, {fate}", b.describe()))
}

/// A run's end as its booter record has it: kept up (`keep`), the record says so and later runs
/// adopt the VM; stopped (`stopped`), the record goes. A VM this run meant to stop and could not
/// keeps the record, so the next run finds it this (by then dead) run's leftover.
pub fn settle(path: Option<&Path>, keep: bool, stopped: bool) {
    let Some(path) = path else {
        return;
    };
    if keep {
        if let Some(mut b) = Booter::read(path).filter(|b| b.pid == std::process::id()) {
            b.kept = true;
            if let Err(e) = b.record(path) {
                eprintln!(
                    "aterm-lab: the VM is kept up, but its record does not say so ({}: {e}): \
                     the next run stops it as a leftover once this one has ended",
                    path.display()
                );
            }
        }
    } else if stopped {
        clear(path);
    }
}

/// Remove the booter record at `path`: its VM is stopped.
pub fn clear(path: &Path) {
    if let Err(e) = std::fs::remove_file(path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("aterm-lab: {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::{BufRead as _, BufReader, Write as _};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::Receiver;
    use std::time::{Duration, Instant};

    use aterm_capacity::{Fixed, Proc, Procs, Reading};
    use aterm_uds::spawnfd::Stdio;

    /// The one-minute hang detector for what must happen.
    const MUST: Duration = Duration::from_secs(60);

    /// The stand-in's ledger directory.
    const CLAIM_ENV: &str = "ATERM_LAB_TEST_VM_CLAIM";
    /// The stand-in holding lima's boot claim.
    const CLAIM_STAND_IN: &str = "vm_admit::tests::a_lima_boot_claim_stand_in";
    /// What the stand-in says once it holds the claim.
    const HOLDS: &str = "the stand-in holds lima's boot claim";
    /// The disk whose holders the claim stand-in credits its claim with, told [`CREDIT`].
    const DISK_ENV: &str = "ATERM_LAB_TEST_VM_DISK";
    /// The line that tells the claim stand-in to credit its claim.
    const CREDIT: &str = "credit";
    /// What the claim stand-in says once it has credited its claim, and with which pids.
    const CREDITED: &str = "the stand-in credited lima's claim with";
    /// The stand-in for a VM's process, which holds the VM's disk.
    const VM_STAND_IN: &str = "vm_admit::tests::a_vm_process_stand_in";
    /// The disk the VM stand-in holds open.
    const VM_ENV: &str = "ATERM_LAB_TEST_VM_PROCESS";
    /// What the VM stand-in says once it holds its disk.
    const VM_UP: &str = "the VM stand-in holds its disk";

    /// A pace that looks often and says its wait line once a minute.
    fn quick() -> Pace {
        Pace {
            poll: Duration::from_millis(10),
            line_every: Duration::from_secs(60),
        }
    }

    /// Not a test alone: as [`CLAIM_STAND_IN`] it takes lima's boot claim in the ledger at
    /// [`CLAIM_ENV`] on a machine with room and says [`HOLDS`]. Each [`CREDIT`] line on stdin
    /// credits the claim with the processes holding the disk at [`DISK_ENV`] ([`vm_procs`]),
    /// as a run does once its VM has booted, and says [`CREDITED`] with them; at any other line
    /// it dies by its own `SIGKILL`, the claim still held.
    #[test]
    fn a_lima_boot_claim_stand_in() {
        let Some(dir) = std::env::var_os(CLAIM_ENV) else {
            return;
        };
        let mut held = admit_in(
            &Ledger::at(dir),
            &Fixed::new(Reading::of_gib(48, 48, 14)),
            LIMA.need("lab linux proof", true),
            &LIMA,
            Admit::Wait,
            quick(),
            &mut |_| {},
            &|| Ok(()),
        )
        .unwrap_or_else(|f| panic!("lima's claim: {:?}", f.msg));
        println!("{HOLDS}");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        while std::io::stdin().read_line(&mut line).is_ok_and(|n| n > 0) && line.trim() == CREDIT {
            let disk = std::env::var_os(DISK_ENV).map(PathBuf::from);
            let pids = disk.map(|d| vm_procs(&d, &[])).unwrap_or_default();
            held.credit("linux", &pids);
            println!("{CREDITED} {pids:?}");
            let _ = std::io::stdout().flush();
            line.clear();
        }
        let _kept = held;
        aterm_uds::process::kill_self_now()
    }

    /// Not a test alone: as [`VM_STAND_IN`] it holds the disk at [`VM_ENV`] open, as a VM's
    /// process holds its disk, says [`VM_UP`], and ends at its first line on stdin.
    #[test]
    fn a_vm_process_stand_in() {
        let Some(disk) = std::env::var_os(VM_ENV) else {
            return;
        };
        let _disk = std::fs::File::open(&disk).expect("the disk");
        println!("{VM_UP}");
        let _ = std::io::stdout().flush();
        let _ = std::io::stdin().read_line(&mut String::new());
    }

    /// The test binary run as the stand-in `name` with `env`, its stdin piped, each line it says
    /// sent on the receiver.
    fn stand_in(
        name: &str,
        env: &[(&str, &std::ffi::OsStr)],
    ) -> (aterm_uds::spawnfd::Child, Receiver<String>) {
        let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("this test"));
        cmd.args(["--exact", name, "--nocapture", "--test-threads=1"]);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the stand-in starts");
        let mut said = BufReader::new(child.stdout.take().expect("its stdout"));
        let (lines, heard) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            while said.read_line(&mut line).is_ok_and(|n| n > 0) {
                let _ = lines.send(std::mem::take(&mut line));
            }
        });
        (child, heard)
    }

    /// The first line `heard` that holds `what`, within the one-minute hang detector.
    fn hear(heard: &Receiver<String>, what: &str) -> String {
        let end = Instant::now() + MUST;
        loop {
            let line = heard
                .recv_timeout(end.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|_| panic!("the stand-in says {what:?}"));
            if line.contains(what) {
                return line;
            }
        }
    }

    /// A VM's process runs outside the tree of the run that booted it. Lima's boot claim (16 GiB)
    /// is held by another process, and lima's VM process (a stand-in holding the VM's disk, not
    /// that process's child) holds 14 GiB of it on a 48 GiB Mac with 30 GiB available. With the
    /// claim not yet credited, Windows' boot waits, short by 2 GiB (the negative control), and an
    /// interrupt ends that wait as NOT RUN, carrying its signal, naming what it waited for and
    /// that nothing booted, its claim gone from the queue. Once lima's holder credits its claim
    /// with whatever holds the VM's disk ([`vm_procs`] finds the stand-in), the same boot on the
    /// same reading is admitted at once, no wait line said.
    #[test]
    fn an_interrupt_ends_the_wait_and_a_credited_vm_is_not_reserved_twice() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("lab-vm-credit-")
            .tempdir()
            .expect("a scratch ledger");
        let ledger = Ledger::at(dir.path().join("capacity"));
        let disk = dir.path().join("disk.img");
        std::fs::write(&disk, b"disk").expect("a disk");
        let (mut vm, vm_said) = stand_in(VM_STAND_IN, &[(VM_ENV, disk.as_os_str())]);
        hear(&vm_said, VM_UP);
        let (mut lima, lima_said) = stand_in(
            CLAIM_STAND_IN,
            &[
                (CLAIM_ENV, ledger.dir().as_os_str()),
                (DISK_ENV, disk.as_os_str()),
            ],
        );
        hear(&lima_said, HOLDS);
        let machine = Arc::new(Fixed::new(Reading::of_gib(48, 30, 14)));
        machine.set_procs(Some(Procs::of(vec![Proc {
            pid: vm.id(),
            ppid: 1,
            footprint: 14 << 30,
            written: 0,
        }])));

        let stop = Arc::new(AtomicBool::new(false));
        let (lines, heard) = std::sync::mpsc::channel::<String>();
        let (ended, end) = std::sync::mpsc::channel();
        {
            let (ledger, machine, stop) = (ledger.clone(), Arc::clone(&machine), Arc::clone(&stop));
            std::thread::spawn(move || {
                let got = admit_in(
                    &ledger,
                    &*machine,
                    WINDOWS.need("lab windows up", true),
                    &WINDOWS,
                    Admit::Wait,
                    quick(),
                    &mut |l| {
                        let _ = lines.send(l.to_owned());
                    },
                    &|| {
                        if stop.load(Ordering::SeqCst) {
                            Err(crate::stop::Stopped(libc::SIGINT))
                        } else {
                            Ok(())
                        }
                    },
                );
                let _ = ended.send(got.map(|a| a.jobs));
            });
        }
        let first = heard.recv_timeout(MUST).expect("Windows' first wait line");
        for named in [
            "memory short by 2 GiB",
            "30 GiB available less 16 GiB reserved",
            &format!("pid {} holds 16 GiB of memory", lima.id()),
        ] {
            assert!(first.contains(named), "{first:?} does not name {named:?}");
        }
        assert!(end.try_recv().is_err(), "Windows waits");
        stop.store(true, Ordering::SeqCst);
        let fail = end
            .recv_timeout(MUST)
            .expect("the interrupt ends the wait")
            .expect_err("an interrupted wait is NOT RUN");
        let why = fail.msg.clone().unwrap_or_default();
        assert_eq!(
            (fail.code, fail.stopped),
            (crate::linux::NOT_RUN, Some(libc::SIGINT)),
            "{why}"
        );
        for named in [
            "NOT RUN: interrupted while waiting for capacity for aterm-windows",
            "memory short by 2 GiB",
            "this run booted nothing",
        ] {
            assert!(why.contains(named), "{why:?} does not name {named:?}");
        }
        let queued = |ledger: &Ledger| {
            ledger
                .list()
                .expect("listed")
                .into_iter()
                .map(|r| (r.pid, r.credit))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            queued(&ledger),
            [(lima.id(), Vec::new())],
            "the interrupted claim left the queue"
        );

        let mut tell = lima.stdin.take().expect("its stdin");
        writeln!(tell, "{CREDIT}").expect("told to credit");
        let credited = hear(&lima_said, CREDITED);
        assert!(
            credited.contains(&format!("[{}]", vm.id())),
            "the VM's disk is held by the stand-in alone: {credited:?}"
        );
        assert_eq!(queued(&ledger), [(lima.id(), vec![vm.id()])]);
        let deadline = Instant::now() + MUST;
        let mut said = Vec::new();
        let admitted = admit_in(
            &ledger,
            &*machine,
            WINDOWS.need("lab windows up", true),
            &WINDOWS,
            Admit::Wait,
            quick(),
            &mut |l| said.push(l.to_owned()),
            &|| {
                if Instant::now() < deadline {
                    Ok(())
                } else {
                    Err(crate::stop::Stopped(libc::SIGALRM))
                }
            },
        )
        .unwrap_or_else(|f| panic!("admitted beside the credited claim: {:?}", f.msg));
        assert!(admitted.claim.is_some());
        assert!(
            said.iter().all(|l| !l.contains("waits")),
            "no wait line: {said:?}"
        );

        writeln!(tell, "die").expect("told to die");
        drop(tell);
        lima.wait().expect("the claim stand-in is reaped");
        drop(vm.stdin.take());
        let status = vm.wait().expect("the VM stand-in is reaped");
        assert!(status.success(), "{status}");
    }

    /// With lima's 16 GiB boot claim held by another process and 30 GiB available on a 48 GiB
    /// Mac (short of Windows' 8 GiB and the 8 GiB margin beside lima's 16), Windows' boot
    /// waits, its wait line naming lima's claim and its holder, and is admitted the moment room
    /// appears, its guest's jobs the cores it was granted. Negative controls: once lima's holder
    /// has died by its own hand, the same reading admits Windows at once with no wait line; and
    /// a Mac too small for the guest and its margin is NOT RUN at once, nothing waited for.
    #[test]
    fn a_boot_waits_beside_limas_claim_naming_it_and_boots_when_room_appears() {
        use std::os::unix::process::ExitStatusExt as _;
        let dir = aterm_tempfile::Builder::new()
            .prefix("lab-vm-admit-")
            .tempdir()
            .expect("a scratch ledger");
        let ledger = Ledger::at(dir.path().join("capacity"));
        let mut lima = aterm_uds::spawnfd::command(std::env::current_exe().expect("this test"))
            .args(["--exact", CLAIM_STAND_IN, "--nocapture", "--test-threads=1"])
            .env(CLAIM_ENV, ledger.dir())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the stand-in starts");
        let lima_pid = lima.id();
        let mut said = BufReader::new(lima.stdout.take().expect("its stdout"));
        let (holds, held) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            while said.read_line(&mut line).is_ok_and(|n| n > 0) {
                if line.contains(HOLDS) {
                    let _ = holds.send(());
                }
                line.clear();
            }
        });
        held.recv_timeout(MUST)
            .expect("the stand-in holds lima's claim");

        let machine = Arc::new(Fixed::new(Reading::of_gib(48, 30, 14)));
        let (lines, heard) = std::sync::mpsc::channel::<String>();
        let windows = {
            let (ledger, machine, lines) = (ledger.clone(), Arc::clone(&machine), lines.clone());
            std::thread::spawn(move || {
                admit_in(
                    &ledger,
                    &*machine,
                    WINDOWS.need("lab windows up", true),
                    &WINDOWS,
                    Admit::Wait,
                    quick(),
                    &mut |l| {
                        let _ = lines.send(l.to_owned());
                    },
                    &|| Ok(()),
                )
                .map(|a| (a.jobs, a.claim.map(|c| c.cores())))
                .map_err(|f| f.msg)
            })
        };
        let first = heard.recv_timeout(MUST).expect("Windows' first wait line");
        for named in [
            "lab windows up: aterm-windows boot (8 GiB, 6 vCPUs) waits",
            "memory short by 2 GiB: needs 8 GiB and 8 GiB spare beside it, 30 GiB available \
             less 16 GiB reserved",
            &format!("pid {lima_pid} holds 16 GiB of memory for "),
            "\"lab linux proof: aterm-linux boot (16 GiB, 8 vCPUs)\"",
        ] {
            assert!(first.contains(named), "{first:?} does not name {named:?}");
        }
        assert!(
            !windows.is_finished(),
            "Windows waits while there is no room"
        );
        machine.set(Reading::of_gib(48, 40, 14));
        let (jobs, granted) = windows
            .join()
            .expect("Windows' admission")
            .expect("admitted once room appears");
        assert_eq!(
            (jobs, granted),
            (6, Some(6)),
            "the 14 cores less lima's 8 leave Windows its 6"
        );
        let said: Vec<String> = heard.try_iter().collect();
        assert!(
            said.iter().any(|l| l.contains("admitted after"))
                && said
                    .iter()
                    .any(|l| l.contains("CARGO_BUILD_JOBS=6: the cores this run was admitted")),
            "{said:?}"
        );

        let mut stdin = lima.stdin.take().expect("its stdin");
        stdin.write_all(b"die\n").expect("told to die");
        let status = lima.wait().expect("the stand-in is reaped");
        assert_eq!(status.signal(), Some(libc::SIGKILL), "{status}");
        machine.set(Reading::of_gib(48, 30, 14));
        let mut said = Vec::new();
        let alone = admit_in(
            &ledger,
            &*machine,
            WINDOWS.need("lab windows up", true),
            &WINDOWS,
            Admit::Wait,
            quick(),
            &mut |l| said.push(l.to_owned()),
            &|| Ok(()),
        )
        .unwrap_or_else(|f| panic!("admitted alone: {:?}", f.msg));
        assert!(alone.claim.is_some());
        assert!(
            said.iter().all(|l| !l.contains("waits")),
            "no wait line: {said:?}"
        );

        let small = Fixed::new(Reading::of_gib(12, 12, 8));
        let mut said = Vec::new();
        let refused = admit_in(
            &ledger,
            &small,
            WINDOWS.need("lab windows up", true),
            &WINDOWS,
            Admit::Wait,
            quick(),
            &mut |l| said.push(l.to_owned()),
            &|| Ok(()),
        )
        .expect_err("a Mac too small is NOT RUN");
        let why = refused.msg.unwrap_or_default();
        assert_eq!(refused.code, crate::linux::NOT_RUN, "{why}");
        assert!(
            why.contains("needs 8 GiB and 8 GiB spare beside it, this Mac has 12 GiB")
                && why.contains("can never boot here"),
            "{why}"
        );
        assert!(said.is_empty(), "nothing waited for: {said:?}");
    }

    /// NEVER-REACHABLE (`docs/DESIGN-adaptive-build-2026-10-24.md`): on a 28 GiB Mac with 4 GiB
    /// wired and 18 GiB available, lima's boot (16 GiB and the 8 GiB margin) is NOT RUN at once,
    /// naming the 24 GiB that Mac can ever have available, nothing waited for and nothing left in
    /// the ledger; on this Mac's shape (48 GiB, 3.5 GiB wired, 27 GiB available) it is admitted
    /// with no wait line. Negative control: judged against all of the memory (the old rule), the
    /// small Mac's boot waits for memory it can never have, until the hang detector.
    #[test]
    fn a_boot_above_what_the_mac_can_ever_have_available_is_not_run_at_once() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("lab-vm-reach-")
            .tempdir()
            .expect("a scratch ledger");
        let ledger = Ledger::at(dir.path().join("capacity"));
        let boot = {
            let ledger = ledger.clone();
            std::thread::spawn(move || {
                let mut said = Vec::new();
                let answer = admit_in(
                    &ledger,
                    &Fixed::new(Reading::of_gib(28, 18, 14).wired_gib(4)),
                    LIMA.need("lab linux exec", true),
                    &LIMA,
                    Admit::Wait,
                    quick(),
                    &mut |l| said.push(l.to_owned()),
                    &|| Ok(()),
                );
                (answer.map(|a| a.jobs).map_err(|f| (f.code, f.msg)), said)
            })
        };
        let detector = Instant::now() + MUST;
        while !boot.is_finished() {
            assert!(
                Instant::now() < detector,
                "lima's boot on a 28 GiB Mac with 4 GiB wired still waits after {MUST:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let (answer, said) = boot.join().expect("the boot's answer");
        let (code, why) = answer.expect_err("NOT RUN");
        let why = why.unwrap_or_default();
        assert_eq!(code, crate::linux::NOT_RUN, "{why}");
        assert!(
            why.contains(
                "lab linux exec: aterm-linux boot (16 GiB, 8 vCPUs): needs 16 GiB and 8 GiB spare \
                 beside it, this Mac can never have more than 24 GiB of its 28 GiB of memory \
                 available (4 GiB of it is wired); the VM can never boot here"
            ),
            "{why}"
        );
        assert!(said.is_empty(), "nothing waited for: {said:?}");
        assert!(ledger.list().expect("listed").is_empty(), "nothing entered");

        let mac = Reading {
            wired: Some(7 << 29),
            ..Reading::of_gib(48, 27, 14)
        };
        let mut said = Vec::new();
        let admitted = admit_in(
            &ledger,
            &Fixed::new(mac),
            LIMA.need("lab linux exec", true),
            &LIMA,
            Admit::Wait,
            quick(),
            &mut |l| said.push(l.to_owned()),
            &|| Ok(()),
        )
        .unwrap_or_else(|f| panic!("admitted on this Mac's shape: {:?}", f.msg));
        assert!(admitted.claim.is_some());
        assert!(
            said.iter().all(|l| !l.contains("waits")),
            "no wait line: {said:?}"
        );
    }

    /// The guest's jobs are the cores the claim was granted: on 14 cores at a 5-minute load of
    /// 60, lima's boot is granted its share, 3, and its guest builds with `CARGO_BUILD_JOBS=3`
    /// (CAP-1's first rule granted 1); a run on the VM already up claims cores alone and is
    /// never short of memory.
    #[test]
    fn the_guests_jobs_are_the_cores_admitted() {
        let dir = aterm_tempfile::tempdir().expect("a scratch ledger");
        let ledger = Ledger::at(dir.path().join("capacity"));
        let mut loaded = Reading::of_gib(48, 48, 14);
        loaded.load5 = Some(60.0);
        let admitted = admit_in(
            &ledger,
            &Fixed::new(loaded),
            LIMA.need("lab linux exec", true),
            &LIMA,
            Admit::Wait,
            quick(),
            &mut |_| {},
            &|| Ok(()),
        )
        .unwrap_or_else(|f| panic!("{:?}", f.msg));
        assert_eq!(admitted.jobs, 3);
        assert_eq!(admitted.claim.as_ref().map(Claim::cores), Some(3));
        let adopted = LIMA.need("lab linux exec", false);
        assert_eq!(
            (adopted.mem, adopted.margin, adopted.cores),
            (0, 0, LIMA.cpus)
        );
        let none = Fixed::new(Reading::of_gib(48, 0, 14));
        let on_up = admit_in(
            &ledger,
            &none,
            adopted,
            &LIMA,
            Admit::Wait,
            quick(),
            &mut |_| {},
            &|| Ok(()),
        )
        .unwrap_or_else(|f| panic!("{:?}", f.msg));
        assert!(on_up.jobs >= 1);
    }

    /// The booter record's stand-in: as [`BOOTER_STAND_IN`] it records itself as the booter at
    /// [`BOOTER_ENV`]'s path, says so, and dies by its own `SIGKILL`.
    const BOOTER_STAND_IN: &str = "vm_admit::tests::a_dead_booter_stand_in";
    /// Where the booter stand-in records itself.
    const BOOTER_ENV: &str = "ATERM_LAB_TEST_VM_BOOTER";

    /// Not a test alone: see [`BOOTER_STAND_IN`].
    #[test]
    fn a_dead_booter_stand_in() {
        let Some(path) = std::env::var_os(BOOTER_ENV) else {
            return;
        };
        Booter::me("lab linux exec", Path::new("/checkouts/booter"))
            .record(Path::new(&path))
            .expect("recorded");
        aterm_uds::process::kill_self_now()
    }

    /// A VM whose one-shot booter died by its own hand is a leftover, named by its booter's pid,
    /// and still is once the record names the VM process its boot started while that process
    /// runs; the same record marked kept (`up`, `--keep-up`) is not, nor one whose booter still
    /// runs, nor no record at all. Nor is one whose record names a VM process that has ended: that
    /// boot is over, and a VM running now was started since outside the records (the negative
    /// control for [`Booter::vm_ended`]). A record whose pid now names a process born at another
    /// moment (a pid taken again) reads as a dead booter's.
    #[test]
    fn a_dead_one_shot_booters_vm_is_a_leftover_and_a_kept_one_is_not() {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let path = dir.path().join("booter");
        assert_eq!(leftover(&path), None, "no record: adopted");
        let mut child = aterm_uds::spawnfd::command(std::env::current_exe().expect("this test"))
            .args(["--exact", BOOTER_STAND_IN, "--test-threads=1"])
            .env(BOOTER_ENV, &path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the stand-in starts");
        let pid = child.id();
        let status = child.wait().expect("reaped");
        assert!(!status.success(), "{status}");
        let gone = leftover(&path).expect("a dead one-shot booter's VM is a leftover");
        assert_eq!(gone.pid, pid);
        assert!(
            gone.describe().starts_with(&format!(
                "pid {pid} (lab linux exec, /checkouts/booter, since "
            )),
            "{}",
            gone.describe()
        );
        let fated = |path: &Path| fate(path).map(|(_, f)| f);
        assert_eq!(fated(&path), Some(Fate::Leftover));
        assert!(
            status_line(&path).is_some_and(|l| l.contains(&format!("pid {pid} "))),
            "{:?}",
            status_line(&path)
        );
        let up = Booter {
            vm_pid: Some(std::process::id()),
            vm_birth: birth_of(std::process::id()),
            ..gone.clone()
        };
        up.record(&path).expect("recorded");
        assert_eq!(
            leftover(&path).map(|b| b.pid),
            Some(pid),
            "the VM process its boot started runs on: that boot's leftover"
        );
        let over = Booter {
            vm_pid: Some(pid),
            vm_birth: gone.birth,
            ..gone.clone()
        };
        over.record(&path).expect("recorded");
        assert_eq!(
            fated(&path),
            None,
            "its boot is over: a VM running now is adopted"
        );
        assert_eq!(status_line(&path), None);
        let kept = Booter {
            kept: true,
            ..gone.clone()
        };
        kept.record(&path).expect("recorded");
        assert_eq!(fated(&path), Some(Fate::Kept), "a VM kept up is adopted");
        assert_eq!(leftover(&path), None);
        let me = Booter::me("lab linux proof", Path::new("/checkouts/me"));
        me.record(&path).expect("recorded");
        assert_eq!(fated(&path), Some(Fate::Running), "a live booter's VM");
        assert_eq!(leftover(&path), None);
        let reused = Booter {
            birth: me.birth.map(|(s, us)| (s.saturating_sub(1), us)),
            ..me.clone()
        };
        reused.record(&path).expect("recorded");
        assert_eq!(
            leftover(&path).map(|b| b.pid),
            Some(me.pid),
            "a pid born at another moment is not the booter"
        );
        settle(Some(&path), false, false);
        assert!(
            path.exists(),
            "a VM this run could not stop keeps its record"
        );
        me.record(&path).expect("recorded");
        record_vm(Some(&path), Some(pid));
        settle(Some(&path), true, false);
        assert_eq!(
            Booter::read(&path).map(|b| (b.kept, b.vm_pid)),
            Some((true, Some(pid))),
            "kept, the VM process its boot started still named"
        );
        settle(Some(&path), false, true);
        assert!(!path.exists(), "a stopped VM's record goes");
        assert_eq!(Booter::parse("{\"pid\":1}"), None);
        assert_eq!(Booter::parse(&me.line()), Some(me));
    }
}
