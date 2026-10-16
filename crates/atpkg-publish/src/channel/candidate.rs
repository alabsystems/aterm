// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The candidate gate (D27), as the `rustc` group's publish asks it: a trust seal is
//! published only when the gate compared it with the published seal and answered
//! PUBLISH-OK, recently, on this aterm's history, under the committed retraction ledger.
//!
//! The gate, its receipt and the admission rules are aterm-release's (`ship trust
//! candidate`, `trust_candidate::refusals`). This module only asks it and turns each
//! refusal line into a preflight problem; the group publish reaches it through
//! [`CandidateGate`] so its tests stub the answer.
//!
//! The command is [`command`], run in the aterm checkout: exit 0 admitted (stdout names the
//! receipt), 1 refused (one refusal per stdout line), anything else could not check (its
//! reason on stdout too). stderr is targo's own build output, never the verdict. `ship`
//! builds the release cutter before it answers, so the call is bounded by [`GATE_BOUND`],
//! and a dry run does not ask at all.

use std::ffi::OsString;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::ctx::Ctx;

/// What the gate answered for one seal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GateAnswer {
    /// PUBLISH-OK, by the newest run for this seal: the receipt it read.
    Admitted(String),
    /// Refused, with every reason.
    Refused(Vec<String>),
    /// The gate could not be asked, or could not check.
    CannotCheck(String),
}

/// Asks the candidate gate about a seal.
pub trait CandidateGate {
    /// Whether the newest run of the gate for `seal` admits publishing it over the trust
    /// build the public channel pins (`None` when the channel could not be read).
    fn admits(&self, ctx: &Ctx, seal: &Path, public_pin: Option<u64>) -> GateAnswer;
}

/// The gate, as a process: [`command`] in the aterm checkout.
#[derive(Clone, Copy, Debug, Default)]
pub struct ShipTrustCandidate;

/// The program the gate is asked through.
pub const PROGRAM: &str = "targo";

/// A hang detector on the gate, its cutter build included: a cold release build of the
/// cutter takes minutes.
pub const GATE_BOUND: Duration = Duration::from_secs(30 * 60);

/// Whether a stderr line is targo's or a compiler's chatter rather than something the
/// gate said.
fn chatter(line: &str) -> bool {
    const PREFIXES: [&str; 9] = [
        "warning:",
        "note:",
        "Compiling",
        "Finished",
        "Running",
        "Blocking",
        "Updating",
        "Downloading",
        "Downloaded",
    ];
    PREFIXES.iter().any(|p| line.starts_with(p))
}

/// The gate's argv after [`PROGRAM`].
#[must_use]
pub fn command(seal: &Path, public_pin: Option<u64>) -> Vec<OsString> {
    let mut v: Vec<OsString> = ["--unverified", "ship", "trust", "candidate", "--admits"]
        .iter()
        .map(OsString::from)
        .collect();
    v.push(seal.as_os_str().to_owned());
    if let Some(pin) = public_pin {
        v.push("--base-build".into());
        v.push(pin.to_string().into());
    }
    v
}

/// What the gate's exit status and output mean.
#[must_use]
pub fn answer(code: Option<i32>, stdout: &str, stderr: &str) -> GateAnswer {
    let lines = |s: &str| -> Vec<String> {
        s.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect()
    };
    match code {
        Some(0) => GateAnswer::Admitted(
            lines(stdout)
                .into_iter()
                .next()
                .unwrap_or_else(|| "PUBLISH-OK".to_string()),
        ),
        Some(1) => {
            let mut why = lines(stdout);
            if why.is_empty() {
                why = lines(stderr);
            }
            if why.is_empty() {
                why.push("the candidate gate refused and said nothing".to_string());
            }
            GateAnswer::Refused(why)
        }
        other => {
            let why = lines(stdout)
                .into_iter()
                .chain(lines(stderr).into_iter().filter(|l| !chatter(l)))
                .next()
                .unwrap_or_else(|| "it said nothing but build output".to_string());
            GateAnswer::CannotCheck(format!(
                "the candidate gate could not check this seal (exit {}): {why}",
                other.map_or_else(|| "by signal".to_string(), |c| c.to_string()),
            ))
        }
    }
}

impl CandidateGate for ShipTrustCandidate {
    fn admits(&self, ctx: &Ctx, seal: &Path, public_pin: Option<u64>) -> GateAnswer {
        let mut cmd = aterm_uds::spawnfd::command(PROGRAM);
        cmd.args(command(seal, public_pin))
            .current_dir(&ctx.repo_root);
        match aterm_uds::deadline::output_until(&mut cmd, Instant::now() + GATE_BOUND) {
            Ok(o) => answer(
                o.status.code(),
                &String::from_utf8_lossy(&o.stdout),
                &String::from_utf8_lossy(&o.stderr),
            ),
            Err(aterm_uds::deadline::RunError::TimedOut) => GateAnswer::CannotCheck(format!(
                "`{PROGRAM} --unverified ship trust candidate` did not answer within {} minutes \
                 (killed)",
                GATE_BOUND.as_secs() / 60
            )),
            Err(e) => GateAnswer::CannotCheck(format!(
                "`{PROGRAM} --unverified ship trust candidate`: {e}"
            )),
        }
    }
}
