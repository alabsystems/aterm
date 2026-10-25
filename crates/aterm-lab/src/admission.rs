// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE QUIET MACHINE FOR THE LAB'S TIMED WORK (`docs/DESIGN-adaptive-build-2026-10-24.md`,
//! QUIET-ADMIT). A windowed instance ([`crate::instance::Instance::start`]) and the Linux lane's
//! GUI rows each hold a quiet claim in the per-user capacity ledger while they run: alone among
//! quiet claims, admitted once the 1-minute load is low, saying once a minute what it waits for.
//! That is admission, not a retry. `--admit now`, given to any verb, starts them at once; a
//! starved run then shows as starved. The same flag is the run's for every claim it makes: the
//! Linux and Windows lanes' boots take it for their disk claims ([`mode`]).

use std::sync::OnceLock;

use aterm_capacity::{Admit, Claim, Ledger, Machine, Need, Pace};

use crate::Fail;

/// The run's `--admit` mode and its verb.
static RUN: OnceLock<(Admit, String)> = OnceLock::new();

/// Take `--admit now|wait` (or `--admit=now|wait`) out of `args` (any place before a `--`) for
/// the run of `verb`.
/// # Errors
/// A value that is neither, or none.
pub fn take(verb: &str, args: &mut Vec<String>) -> Result<(), Fail> {
    let mode = strip(args)?;
    let _ = RUN.set((mode, verb.to_owned()));
    Ok(())
}

/// The run's `--admit` mode, as [`take`] read it: [`Admit::Wait`] where none was given.
#[must_use]
pub fn mode() -> Admit {
    RUN.get().map_or(Admit::Wait, |r| r.0)
}

/// [`take`]'s parse: the mode, with the flag and its value removed from `args`.
pub(crate) fn strip(args: &mut Vec<String>) -> Result<Admit, Fail> {
    let end = args.iter().position(|a| a == "--").unwrap_or(args.len());
    let joined = format!("{}=", Admit::FLAG);
    let Some(at) = args[..end]
        .iter()
        .position(|a| a == Admit::FLAG || a.starts_with(&joined))
    else {
        return Ok(Admit::Wait);
    };
    let (word, taken) = match args[at].strip_prefix(&joined) {
        Some(word) => (Some(word.to_owned()), 1),
        None => (args.get(at + 1).filter(|_| at + 1 < end).cloned(), 2),
    };
    let mode = word.as_deref().and_then(Admit::parse).ok_or_else(|| {
        Fail::usage(format!(
            "--admit takes now or wait, not `{}`",
            word.unwrap_or_default()
        ))
    })?;
    args.drain(at..at + taken);
    Ok(mode)
}

/// A quiet claim for `what`, part of this run's verb, as [`take`] admits it. A ledger that cannot
/// be used is said, and the work runs unadmitted (`None`).
/// # Errors
/// NOT RUN when an interrupt ended the wait, or came before it ([`quiet_in`]).
pub fn quiet(what: &str) -> Result<Option<Claim>, Fail> {
    let (mode, verb) = RUN
        .get()
        .cloned()
        .unwrap_or_else(|| (Admit::Wait, String::new()));
    quiet_in(
        Ledger::user().as_ref(),
        &aterm_capacity::Live,
        Need::timing(format!("lab {verb}: {what}")),
        what,
        mode,
        Pace::default(),
        &mut |line| eprintln!("{line}"),
    )
}

/// [`quiet`]'s admission of `need` for `what` in `ledger` on `machine`, at `pace`, its wait
/// lines said through `say`. No ledger, or one that cannot be used, is said, and the work runs
/// unadmitted (`None`).
/// # Errors
/// NOT RUN, carrying the signal, when an interrupt ended the wait: `what` started nothing, and
/// the interrupt is the run's ([`crate::stop::note`]), so it ends what it started its own way and
/// ends NOT RUN. An interrupt the run's own handling took before the wait began
/// ([`crate::stop::check`]) ends it too, before or during its first look: once a run is ending,
/// none of its work waits anew.
pub(crate) fn quiet_in(
    ledger: Option<&Ledger>,
    machine: &dyn Machine,
    need: Need,
    what: &str,
    mode: Admit,
    pace: Pace,
    say: &mut dyn FnMut(&str),
) -> Result<Option<Claim>, Fail> {
    let stopped = |e: &dyn std::fmt::Display, signal: Option<i32>| Fail {
        stopped: signal
            .inspect(|sig| crate::stop::note(*sig))
            .or_else(|| crate::stop::check().err().map(|s| s.0)),
        ..crate::linux::not_run(format!(
            "{what}: its quiet claim was {e}; it started nothing"
        ))
    };
    if crate::stop::check().is_err() {
        return Err(stopped(&"not asked for: the run was interrupted", None));
    }
    let Some(ledger) = ledger else {
        eprintln!("aterm-lab: no home to keep the capacity ledger under; {what} runs unadmitted");
        return Ok(None);
    };
    match aterm_capacity::admit_until(ledger, machine, need, mode, pace, say, &|| {
        crate::stop::check().is_err()
    }) {
        Ok(claim) => Ok(Some(claim)),
        Err(e @ aterm_capacity::Error::Stopped { signal, .. }) => Err(stopped(&e, signal)),
        Err(e) => {
            eprintln!("aterm-lab: {e}; {what} runs unadmitted");
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;
    use std::time::Instant;

    use aterm_capacity::{Fixed, Reading};
    use aterm_spec::wait::{MUST_HAPPEN, until_within};

    /// Set, the test binary is [`standin_waits_for_the_quiet_machine`], its ledger under this
    /// directory.
    const STANDIN: &str = "ATERM_LAB_QUIET_INTERRUPT_STANDIN";

    /// The machine both sides read: idle, so only the quiet claim the test holds keeps the
    /// stand-in waiting.
    fn machine() -> Fixed {
        Fixed::new(Reading::of_gib(48, 40, 14))
    }

    /// The stand-in's quiet claim for the x86_64 GUI rows, looking every two minutes (twice the
    /// parent's hang detector, so only an interrupt ends the wait in time).
    fn gui_rows(dir: &std::path::Path, say: &mut dyn FnMut(&str)) -> Result<Option<Claim>, Fail> {
        quiet_in(
            Some(&Ledger::at(dir.join("ledger"))),
            &machine(),
            Need::timing("lab proof: x86_64 GUI rows"),
            "x86_64 GUI rows",
            Admit::Wait,
            Pace {
                poll: MUST_HAPPEN * 2,
                ..Pace::default()
            },
            say,
        )
    }

    /// Run by [`an_interrupt_ends_the_runs_quiet_claims`] in a process of its own, its
    /// interrupts the lab's own ([`crate::stop::install`]) with SIGINT at its default action, as
    /// a foreground run has it: waits for the quiet machine, writing `waiting` at its first
    /// wait line; once an interrupt ends that wait, a later quiet claim of the run is refused
    /// without a wait, and it exits as the lab's main would ([`crate::stop::status`]), saying
    /// what a FAIL found before, and a refusal, would have exited.
    #[test]
    fn standin_waits_for_the_quiet_machine() {
        let Some(dir) = std::env::var_os(STANDIN).map(PathBuf::from) else {
            return;
        };
        crate::sys::start_signal(libc::SIGINT, false);
        crate::stop::install();
        let waiting = dir.join("waiting");
        let first = gui_rows(&dir, &mut |line| {
            if !waiting.exists() {
                std::fs::write(&waiting, line).expect("said waiting");
            }
        })
        .expect_err("an interrupt ended the wait");
        assert!(
            crate::stop::ended_a_wait(),
            "the wait handed its interrupt on"
        );
        let sig = first.stopped.expect("the interrupt's signal");
        assert_eq!(crate::stop::check(), Err(crate::stop::Stopped(sig)));
        let again = gui_rows(&dir, &mut |line| panic!("a second wait: {line}"))
            .expect_err("no quiet claim once the run is ending");
        println!("{}", first.msg.unwrap_or_default());
        println!("{}", again.msg.unwrap_or_default());
        println!(
            "a FAIL found before exits {}, a refusal {}",
            crate::stop::status(1, true),
            crate::stop::status(1, false)
        );
        std::process::exit(crate::stop::status(first.code, false));
    }

    /// A stand-in, killed when it is dropped still running (a test that failed before it
    /// ended), so none outlives its test.
    struct Standin(aterm_uds::spawnfd::Child);

    impl Drop for Standin {
        fn drop(&mut self) {
            if matches!(self.0.try_wait(), Ok(None)) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }

    /// The lab's quiet claim (a windowed instance's, the Linux lane's GUI rows') ends on SIGTERM,
    /// or SIGINT, as NOT RUN (exit 3) well inside the one-minute hang detector (the time is
    /// reported), naming the signal; the interrupt becomes the run's, so its next quiet claim is
    /// refused without a wait, and a FAIL the verb found before stays exit 1 (a refusal's exit 1
    /// does not). Only the test's claim is left in the ledger.
    #[test]
    fn an_interrupt_ends_the_runs_quiet_claims() {
        for (sig, word) in [(libc::SIGTERM, "SIGTERM"), (libc::SIGINT, "SIGINT")] {
            let dir = aterm_tempfile::Builder::new()
                .prefix("lab-quiet-int-")
                .tempdir()
                .expect("a scratch ledger");
            let ledger = Ledger::at(dir.path().join("ledger"));
            let held = aterm_capacity::admit_with(
                &ledger,
                &machine(),
                Need::new("holder").quiet(28.0),
                Admit::Wait,
                Pace::default(),
                &mut |_| {},
            )
            .expect("the holder's claim");
            let mut child = Standin(
                aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"))
                    .args([
                        "--exact",
                        "admission::tests::standin_waits_for_the_quiet_machine",
                        "--test-threads=1",
                        "--nocapture",
                    ])
                    .env(STANDIN, dir.path())
                    .stdin(aterm_uds::spawnfd::Stdio::null())
                    .stdout(aterm_uds::spawnfd::Stdio::piped())
                    .stderr(aterm_uds::spawnfd::Stdio::null())
                    .spawn()
                    .expect("the stand-in"),
            );
            until_within(MUST_HAPPEN, "the stand-in's wait", || {
                std::fs::metadata(dir.path().join("waiting")).ok()
            });
            let pid = i32::try_from(child.0.id()).expect("a pid");
            let sent = Instant::now();
            crate::sys::kill(pid, sig).expect("signalled the stand-in");
            let status = until_within(MUST_HAPPEN, "the stand-in's end", || {
                child.0.try_wait().ok().flatten()
            });
            let took = sent.elapsed();
            let mut out = String::new();
            if let Some(mut o) = child.0.stdout.take() {
                let _ = std::io::Read::read_to_string(&mut o, &mut out);
            }
            assert_eq!(status.code(), Some(3), "{word}: {status}\n{out}");
            assert!(
                out.contains(&format!(
                    "NOT RUN: x86_64 GUI rows: its quiet claim was interrupted ({word}) while it \
                     waited"
                )),
                "{word}: {out}"
            );
            assert!(
                out.contains("NOT RUN: x86_64 GUI rows: its quiet claim was not asked for"),
                "{word}: {out}"
            );
            assert!(
                out.contains("a FAIL found before exits 1, a refusal 3"),
                "{word}: {out}"
            );
            let left: Vec<String> = ledger
                .dir()
                .read_dir()
                .expect("the ledger")
                .filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".claim") || n.ends_with(".wait"))
                .collect();
            assert_eq!(left.len(), 1, "{word}: only the holder's claim: {left:?}");
            eprintln!("{word} ended the lab's quiet claim in {took:?}");
            drop(held);
        }
    }

    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|w| (*w).to_owned()).collect()
    }

    /// `--admit` is taken out wherever it stands before a `--`, and a word after `--` belongs to
    /// the program the verb runs.
    #[test]
    fn admit_is_taken_out_before_the_verb_reads_its_arguments() {
        let mut a = words(&["--window", "--admit", "now", "--trials", "3"]);
        assert_eq!(strip(&mut a).ok(), Some(Admit::Now));
        assert_eq!(a, words(&["--window", "--trials", "3"]));
        let mut a = words(&["--window"]);
        assert_eq!(strip(&mut a).ok(), Some(Admit::Wait));
        assert_eq!(a, words(&["--window"]));
        let mut a = words(&["--", "prog", "--admit", "now"]);
        assert_eq!(strip(&mut a).ok(), Some(Admit::Wait));
        assert_eq!(a.len(), 4, "the program's own words stay");
        let mut a = words(&["--admit=now", "proof"]);
        assert_eq!(strip(&mut a).ok(), Some(Admit::Now));
        assert_eq!(a, words(&["proof"]));
        assert!(strip(&mut words(&["--admit=soon"])).is_err());
        assert!(strip(&mut words(&["--admit", "soon"])).is_err());
        assert!(strip(&mut words(&["--admit"])).is_err());
        assert!(strip(&mut words(&["--admit", "--", "now"])).is_err());
    }
}
