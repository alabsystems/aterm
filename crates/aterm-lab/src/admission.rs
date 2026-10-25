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

use aterm_capacity::{Admit, Claim, Need};

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
#[must_use]
pub fn quiet(what: &str) -> Option<Claim> {
    let (mode, verb) = RUN
        .get()
        .cloned()
        .unwrap_or_else(|| (Admit::Wait, String::new()));
    let need = Need::timing(format!("lab {verb}: {what}"));
    match aterm_capacity::admit(need, mode) {
        Ok(claim) => Some(claim),
        Err(e) => {
            eprintln!("aterm-lab: {e}; {what} runs unadmitted");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
