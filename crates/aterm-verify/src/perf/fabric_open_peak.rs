// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The shipped broker's open, held to C27 (KI-1111-10).
//!
//! A run of the fabric bench with its broker in a process of its own and a seeded log
//! (`--broker-process background|interactive --attach-backlog N`) starts `aterm link broker` on
//! the sealed log twice: once after the seed's clean close, and once more after the bench wrote
//! records through that first broker past the head its open wrote into the identity sidecar and
//! then `SIGKILL`ed it (`crates/aterm/tests/fabric_bench/run/outside.rs`, `Seeds::open`).
//! astream's open takes a different branch for each: it reserves the residency tier's columns at
//! once only for the count a clean close recorded, and after any other stop it grows them to
//! 1,024 records and then reserves them at the count the file's length predicts, with an eighth
//! over what that adds past the sidecar's head: the records written before the kill.
//!
//! [`judge`] reads the bench's report and fails the run when either start's peak footprint
//! (`ri_lifetime_max_phys_footprint` when its `Hello` was answered, which spans its open) is
//! above C27's 50 MB ([`C27_BYTES`]), or when the report does not show the second start followed
//! an unclean stop: the first broker's exit names `SIGKILL`, its open rewrote the sidecar the
//! seed's clean close left, the sidecar after the kill holds the bytes that open wrote (a clean
//! close replaces them), and the log took records past that sidecar's head. It judges a count,
//! never a time: the starts' times stay in the report as measured.
//!
//! Only that arm is judged: a run without both flags (the defaults run the broker in the bench's
//! process with no seed) is [`Verdict::NotRun`], so no landing or default perf run holds the
//! open's peak.

use aterm_json::Value;

/// C27: an always-on broker stays under 50 MB. Held to the physical footprint's peak, the number
/// `sys why` reports and ranks a process by.
pub(crate) const C27_BYTES: u64 = 50_000_000;

/// The signal a kill that stopped the broker uncleanly names.
const SIGKILL: u64 = 9;

/// The two starts held: their key under `attach.seeded.opens` and what they are.
const STARTS: [(&str, &str); 2] = [
    ("sealed", "the start after the seed's clean close"),
    ("sealed_unclean", "the start after the SIGKILL"),
];

/// What [`judge`] made of a report: nothing to hold (no seeded outside arm), or each start's
/// peak footprint under `ceiling`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The report has no seeded log served by `aterm link broker`.
    NotRun,
    /// Both starts held, each `(what, peak bytes)`.
    Held(Vec<(&'static str, u64)>),
}

impl Verdict {
    /// The lane's line for it, or `None` with nothing to say.
    pub(crate) fn line(&self, ceiling: u64) -> Option<String> {
        match self {
            Verdict::NotRun => None,
            Verdict::Held(peaks) => Some(format!(
                "the shipped broker's open peaks under C27's {}: {}",
                mb(ceiling),
                peaks
                    .iter()
                    .map(|(what, peak)| format!("{what} {}", mb(*peak)))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
}

fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1e6)
}

/// Hold `report`'s seeded starts of `aterm link broker` to `ceiling` bytes of peak footprint.
///
/// # Errors
/// A seeded report that lacks either start or its peak, whose stop before the second start was
/// not a `SIGKILL` after records past the head an open wrote into the identity sidecar, with the
/// sidecar left as that open wrote it, or whose start peaked above `ceiling`.
pub(crate) fn judge(report: &Value, ceiling: u64) -> Result<Verdict, String> {
    let Some(seeded) = report
        .get("attach")
        .and_then(|a| a.get("seeded"))
        .filter(|s| !s.is_null())
    else {
        return Ok(Verdict::NotRun);
    };
    let stop = seeded.get("unclean_stop");
    let field = |key: &str| stop.and_then(|s| s.get(key));
    let signal = field("exit_signal").and_then(Value::as_u64);
    let rewritten = field("sidecar_rewritten_by_open").and_then(Value::as_bool);
    let unchanged = field("sidecar_unchanged_since_open").and_then(Value::as_bool);
    let past = field("records_past_sidecar_head").and_then(Value::as_u64);
    if signal != Some(SIGKILL)
        || rewritten != Some(true)
        || unchanged != Some(true)
        || past.is_none_or(|n| n == 0)
    {
        return Err(format!(
            "the seeded log's second start did not follow an unclean stop: the first broker's \
             exit names signal {signal:?} (SIGKILL is {SIGKILL}), its open rewrote the seed's \
             closed identity sidecar is {rewritten:?}, the sidecar unchanged since that open is \
             {unchanged:?}, and the log took {past:?} records past the sidecar's head, so \
             astream's open after an unclean stop was not the one measured"
        ));
    }
    let mut peaks = Vec::with_capacity(STARTS.len());
    let mut over = Vec::new();
    for (key, what) in STARTS {
        let peak = seeded
            .get("opens")
            .and_then(|o| o.get(key))
            .and_then(|o| o.get("open"))
            .and_then(|o| o.get("at_hello"))
            .and_then(|r| r.get("peak_footprint_bytes"))
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                format!("the seeded report has no peak footprint for {what} (`opens.{key}`)")
            })?;
        if peak > ceiling {
            over.push(format!("{what} peaked at {} ({peak} bytes)", mb(peak)));
        }
        peaks.push((what, peak));
    }
    if over.is_empty() {
        Ok(Verdict::Held(peaks))
    } else {
        Err(format!(
            "the shipped broker's open is over C27's {} ({ceiling} bytes) of footprint: {}",
            mb(ceiling),
            over.join("; ")
        ))
    }
}

#[cfg(test)]
#[path = "fabric_open_peak_tests.rs"]
mod tests;
