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
    /// The claim; `None` where the ledger could not be used, or a cohort-aware one refused
    /// `--admit now`'s look (the run goes on unclaimed). It is let go when the run's `Up` drops.
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
    let admitted = match Ledger::user() {
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
    };
    if admitted.is_ok() {
        GRANTED.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    admitted
}

/// Whether this run's VM was admitted ([`admit`]): set once it was, so the boot's disk claim
/// keeps what the VM's directory took only for a run that went on to its VM
/// (`crate::linux::LaneLock`).
static GRANTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether this run's VM was admitted: false for a run whose admission an interrupt or a NOT
/// RUN ended, or that never asked, and which therefore booted nothing and ran nothing on it.
#[must_use]
pub fn granted() -> bool {
    GRANTED.load(std::sync::atomic::Ordering::Relaxed)
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
        Err(e @ Error::Unmet { .. }) => Err(crate::linux::not_run(format!(
            "its claim for {} was refused: {e}, {}; this run {}",
            guest.vm,
            crate::linux::AT_ONCE,
            if booting {
                "booted nothing"
            } else {
                "ran nothing on it"
            }
        ))),
        // A ledger that could not be used, or a cohort-aware one that admits only what fits
        // refusing `--admit now`'s look: the boot goes on at once, as `--admit now` promises
        // (`crate::linux::AT_ONCE`), unclaimed.
        Err(Error::Ledger(e)) => Ok(unclaimed(guest, &e.to_string())),
        Err(e @ Error::Refused(_)) => Ok(unclaimed(guest, &e.to_string())),
        Err(Error::Timing(why)) => Err(crate::linux::not_run(format!(
            "timing admission for {} refused: {why}; this run started nothing",
            guest.vm
        ))),
        Err(e @ Error::Stuck { .. }) => Err(crate::linux::not_run(format!(
            "its claim for {} {e}; this run {}",
            guest.vm,
            if booting {
                "booted nothing"
            } else {
                "ran nothing on it"
            }
        ))),
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
        Booter::parse(&crate::util::read_regular_text(path).ok()?)
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
        let _ = std::fs::remove_file(&staged);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        opts.open(&staged)
            .and_then(|mut f| {
                use std::io::Write as _;
                f.write_all(self.line().as_bytes())
            })
            .and_then(|()| std::fs::rename(&staged, path))
            .inspect_err(|_| {
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
            credit: Vec::new(),
            stalled: None,
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
#[path = "vm_admit_tests.rs"]
mod tests;
