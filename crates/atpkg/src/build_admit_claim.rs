// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One account-ledger claim for a command selected by Trust's build-slot. A successful
//! status line commits the front to exec in this PID. The holder keeps the claim on exec.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use aterm_capacity::{Ledger, Need, Polled, Rank, Waiter};
use aterm_toolchain::interrupt::Watch;

use super::{Held, Key, Records, Seat, becoming, hand_over};
use crate::claim_command::{ClaimCommand, PublicClaim};

pub(crate) fn claim_exec(cmd: ClaimCommand) -> ExitCode {
    let (code, said) = claimed(cmd, &Seat::live());
    if let Some(line) = said {
        eprintln!("{line}");
    }
    code
}

/// `cmd` on `seat`: the claim's exit status, with the line it says on stderr where it started
/// nothing (75, `atpkg claim: <why>`).
fn claimed(cmd: ClaimCommand, seat: &Seat<'_>) -> (ExitCode, Option<String>) {
    let result = (|| {
        let ledger = seat
            .ledger
            .as_ref()
            .ok_or_else(|| io::Error::other("no account ledger"))?;
        if ledger.cohort_aware()? {
            return Err(io::Error::other(
                "the account ledger does not support ordinary claim handoff",
            ));
        }
        match cmd {
            ClaimCommand::Capabilities => {
                println!("aterm-claim-exec-v1");
                Ok(ExitCode::SUCCESS)
            }
            ClaimCommand::Exec(request) => run(request, seat, ledger),
        }
    })();
    match result {
        Ok(code) => (code, None),
        Err(err) => (ExitCode::from(75), Some(format!("atpkg claim: {err}"))),
    }
}

/// An optional caller lock held for this admission. The guard locks a kept-out
/// duplicate of the inherited open file description. On a refused exec it
/// explicitly unlocks that description. On successful exec the duplicate
/// closes and the original inherited descriptor continues to hold the lock.
struct WorktreeLock {
    _held: Option<crate::lock::Flock>,
}

impl WorktreeLock {
    fn acquire(file: Option<&File>) -> io::Result<Option<Self>> {
        let Some(file) = file else {
            return Ok(Some(Self { _held: None }));
        };
        let duplicate = aterm_uds::kept_out(|| file.try_clone())?;
        match crate::lock::Flock::try_lock(duplicate) {
            Ok(lock) => Ok(Some(Self { _held: Some(lock) })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(error)) => Err(error),
        }
    }
}

struct Waiting<'a> {
    watch: Watch,
    status: &'a File,
    started: Instant,
    wait: Option<Duration>,
    committed: bool,
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        // The public protocol reports an interrupted wait as exit 75. An untaken
        // interrupt would instead be delivered again when the final Watch is dropped.
        if !self.committed {
            let _ = self.watch.take();
        }
    }
}

impl Waiting<'_> {
    fn stopped(&self) -> bool {
        if self.watch.caught().is_some()
            || self
                .wait
                .is_some_and(|limit| self.started.elapsed() >= limit)
        {
            return true;
        }
        let mut fd = libc::pollfd {
            fd: self.status.as_raw_fd(),
            events: libc::POLLOUT,
            revents: 0,
        };
        // SAFETY: exactly one writable pollfd refers to our live status file throughout the call.
        let rc = unsafe { libc::poll(&raw mut fd, 1, 0) };
        (rc < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted)
            || fd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0
    }

    fn check(&self) -> io::Result<()> {
        if self.stopped() {
            Err(io::Error::other("wait ended before the command started"))
        } else {
            Ok(())
        }
    }

    fn retry(&self, once: bool) -> io::Result<()> {
        if once {
            return Err(io::Error::other(
                "the command could not be admitted at this look",
            ));
        }
        self.watch
            .nap(Duration::from_millis(200), &|| self.stopped());
        self.check()
    }
}

fn run(request: PublicClaim, seat: &Seat<'_>, ledger: &Ledger) -> io::Result<ExitCode> {
    let mut waiting = Waiting {
        watch: Watch::enter(),
        status: &request.status,
        started: Instant::now(),
        wait: request.wait_ms.map(Duration::from_millis),
        committed: false,
    };
    // The status reader can disappear between poll and write. This one-shot front must
    // return a refused admission on EPIPE. become_command restores the caller's signals.
    // SAFETY: SIG_IGN is a valid process disposition; no Rust data enters a signal handler.
    if unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) } == libc::SIG_ERR {
        return Err(io::Error::last_os_error());
    }
    waiting.check()?;
    let mut lock = || WorktreeLock::acquire(request.flock.as_ref());
    let covered =
        aterm_capacity::covering_command(ledger, seat.machine, seat.pid, seat.marker.as_deref())?;
    let (cores, marker, held, _worktree) = if let Some(record) = covered {
        if request.alone && !record.held.is_some_and(|held| held.alone) {
            return Err(io::Error::other(
                "the existing covering claim is not exclusive",
            ));
        }
        let cores = record.held.map_or(0, |held| held.cores).min(request.cores);
        if cores == 0 {
            return Err(io::Error::other("the covering claim has no CPU grant"));
        }
        let guard = loop {
            if let Some(guard) = lock()? {
                break guard;
            }
            waiting.retry(request.no_wait)?;
        };
        // The worktree could have kept us waiting after the original cover ended.
        let marker = record.cover_marker();
        if !aterm_capacity::covering_command(ledger, seat.machine, seat.pid, Some(&marker))?
            .is_some_and(|now| {
                now.cover_marker() == marker && now.held.is_some_and(|h| h.cores >= cores)
            })
        {
            return Err(io::Error::other(
                "the covering grant ended before execution",
            ));
        }
        (cores, marker, None, guard)
    } else {
        if ledger.of_pid(seat.pid)?.iter().any(|r| r.held.is_some()) {
            return Err(io::Error::other(
                "this process already holds a noncovering claim",
            ));
        }
        let reading = seat.machine.read(None);
        // A daily train has a new checkout for each cut. Only its explicit key binds
        // peak history. The actual checkout is kept separately in the ledger record.
        let key = Key {
            stage: "pkg claim".into(),
            checkout: String::new(),
            crates: None,
            variant: Some(request.key.clone()),
        };
        let own = seat.own.clone();
        #[cfg(debug_assertions)]
        let own = if std::env::var_os(super::LEDGER_ENV).is_some_and(|dir| !dir.is_empty()) {
            Some(ledger.dir().join("pkg-claim-peaks.json"))
        } else {
            own
        };
        let bytes = request.mem_bytes.unwrap_or_else(|| {
            let records = own.as_deref().map(Records::load).unwrap_or_default();
            records.learned(&key).map_or_else(
                || aterm_capacity::cold_claim(reading.physical),
                |known| aterm_capacity::peak_claim(known.peak),
            )
        });
        let checkout = request
            .checkout
            .to_str()
            .ok_or_else(|| io::Error::other("ledger checkout is not UTF-8"))?;
        let mut need = Need::new(format!("{} [{}]", request.what, request.key))
            .rank(request.rank)
            .cores(request.cores)
            .mem(bytes)
            .covers_command()
            .checkout_exact(checkout)?;
        if request.alone {
            need = need.alone();
        }
        if let Some(why) = aterm_capacity::never(&need, &reading) {
            return Err(io::Error::other(why));
        }
        let mut waiter = match request.since_ms {
            Some(since) => Waiter::enter_since(ledger, seat.machine, need, since)?,
            None => Waiter::enter(ledger, seat.machine, need)?,
        };
        let (claim, guard) = loop {
            waiting.check()?;
            let Ok((result, guard)) =
                waiter.poll_until_guarded(seat.machine, &|| waiting.stopped(), &mut lock)
            else {
                return Err(io::Error::other("wait ended before the command started"));
            };
            match (result, guard) {
                (Polled::Admitted(claim), Some(guard)) => break (claim, guard),
                (Polled::Waiting(next, _), None) => {
                    waiter = next;
                    waiting.retry(request.no_wait)?;
                }
                (Polled::Gone(error), _) => return Err(error),
                (Polled::Unqualified(why), _) => return Err(io::Error::other(why.to_string())),
                _ => return Err(io::Error::other("incomplete guarded admission")),
            }
        };
        let cores = claim.cores();
        let marker = claim.record().cover_marker();
        (
            cores,
            marker,
            Some(Held {
                claim,
                key,
                own,
                cohort: false,
                build_dir: None,
            }),
            guard,
        )
    };
    // The claim verb never falls back to spawning a child. Close the gate before arming
    // the holder or writing success, including when an ancestor already owns the claim.
    let _closed = aterm_sysprobe::exec_gate::close(aterm_sysprobe::exec_gate::SETTLES_WITHIN)
        .map_err(|e| io::Error::other(e.to_string()))?;
    waiting.check()?;
    let record = if let Some(held) = held {
        hand_over(&held).map_err(io::Error::other)?;
        let mem = held.claim.record().need.mem;
        let rank = if held.claim.record().need.rank == Rank::Release {
            "release"
        } else {
            "ordinary"
        };
        held.claim.hand_off();
        aterm_json::json!({"v": 1, "event":"admitted", "cores":cores, "marker":marker.as_str(), "mem_bytes":mem, "rank":rank})
    } else {
        aterm_json::json!({"v": 1, "event":"covered", "cores":cores, "marker":marker.as_str()})
    };
    waiting.check()?;
    write_admission(&request.status, &record)?;
    // From this point an interrupt belongs to the admitted process. The watch
    // restores its inherited disposition and delivers any still-pending signal.
    waiting.committed = true;
    drop(waiting);
    drop(request.status);
    let jobs = OsString::from(cores.to_string());
    let env = [
        "CARGO_BUILD_JOBS",
        "TARGO_CALLER_JOBS",
        "TARGO_PLANNED_JOBS",
    ]
    .map(|name| (name.into(), jobs.clone()));
    let (program, args) = request
        .command
        .split_first()
        .ok_or_else(|| io::Error::other("missing program"))?;
    match becoming::become_command(Path::new(program), args, &marker, &env) {
        becoming::Stayed::Failed(error) => {
            eprintln!("atpkg claim: cannot execute {program:?}: {error}");
            Ok(ExitCode::from(127))
        }
        becoming::Stayed::Reading(error) => Err(io::Error::other(error.to_string())),
    }
}

fn write_admission(mut status: &File, record: &aterm_json::Value) -> io::Result<()> {
    let mut line = aterm_json::to_vec(record).map_err(io::Error::other)?;
    line.push(b'\n');
    // One short record fits PIPE_BUF. Keep the descriptor nonblocking so a reader
    // that retained a full pipe cannot wedge the armed front past its cancellation.
    let fd = status.as_raw_fd();
    // SAFETY: both fcntl commands operate on the live status descriptor and take no pointers.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let written = status.write(&line);
    // SAFETY: restore only the status OFD's original flags; another inherited reference
    // can outlive this front. Once the complete record was sent it commits us to exec.
    unsafe { libc::fcntl(fd, libc::F_SETFL, flags) };
    if written? == line.len() {
        Ok(())
    } else {
        Err(io::Error::other("incomplete claim status write"))
    }
}

#[cfg(test)]
#[path = "build_admit_claim_tests.rs"]
mod tests;
