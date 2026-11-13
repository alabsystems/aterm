// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read the same owner-bound recovery observation that the account menu shows.

use aterm_agent::harness::account_progress::{AccountProgress, ProgressPhase};

pub(crate) const USAGE: &str = "ERR usage: harness policy | recovery | fact for=<rule> reader=<id> kinds=<k> wall=<w> | fact told episode=<hex>\n";

pub(crate) fn snapshot(
    timeline: &crate::session_timeline::SessionTimeline,
) -> Option<AccountProgress> {
    timeline
        .claude_footer()?
        .account_progress_view()
        .map(std::borrow::Cow::into_owned)
}

/// Never refresh inventory or sign-in state as a side effect of a control read.
pub(crate) fn cmd(
    rest: &str,
    owner: bool,
    fetch: impl FnOnce() -> Result<Option<AccountProgress>, String>,
) -> String {
    if rest.split_whitespace().collect::<Vec<_>>() != ["recovery"] {
        return USAGE.into();
    }
    if !owner {
        return "ERR denied: harness recovery is owner-only\n".into();
    }
    match fetch() {
        Ok(progress) => reply(progress.as_ref()),
        Err(error) => format!("ERR {error}\n"),
    }
}

fn reply(progress: Option<&AccountProgress>) -> String {
    let Some(progress) = progress else {
        return "OK recovery version=1 available=0\n".into();
    };
    let (phase, target, route) = match &progress.phase {
        ProgressPhase::Unknown => ("unknown", None, None),
        ProgressPhase::Paused => ("paused", None, None),
        ProgressPhase::Watching => ("watching", None, None),
        ProgressPhase::Disabled => ("disabled", None, None),
        ProgressPhase::Limited => ("limited", None, None),
        ProgressPhase::Waiting => ("waiting", None, None),
        ProgressPhase::Probing => ("probing", None, None),
        ProgressPhase::Ready => ("ready", None, None),
        ProgressPhase::Switching { to } => ("switching", Some(to.as_str()), None),
        ProgressPhase::Verifying { to } => ("verifying", Some(to.as_str()), None),
        ProgressPhase::SigningIn { label, route } => {
            ("signing-in", Some(label.as_str()), route.as_deref())
        }
    };
    let text = |value: Option<&str>| value.map(crate::control::pct_encode).unwrap_or("-".into());
    let count = |value: Option<usize>| value.map(|n| n.to_string()).unwrap_or("-".into());
    let time = |value: Option<i64>| {
        value
            .filter(|at| *at > 0)
            .map(|at| at.to_string())
            .unwrap_or("-".into())
    };
    let mut line = format!(
        "OK recovery version=1 available=1 phase={phase} account={} target={} route={} known={} eligible={} wall={} observed_at={} next_at={} forecasts_paused={} detail_count={} login_diagnostic_count={} independent_work_count={}",
        text(progress.account.as_deref()),
        text(target),
        text(route),
        count(progress.known),
        count(progress.eligible),
        text(progress.wall.as_deref()),
        time(Some(progress.observed_at)),
        time(progress.next_at),
        u8::from(progress.forecasts_paused),
        progress.details.len(),
        progress.login_diagnostics.len(),
        progress.independent_work.len()
    );
    for (index, detail) in progress.details.iter().enumerate() {
        line.push_str(&format!(
            " detail_{index}={}",
            crate::control::pct_encode(detail)
        ));
    }
    for (index, diagnostic) in progress.login_diagnostics.iter().enumerate() {
        line.push_str(&format!(
            " login_diagnostic_{index}_account={} login_diagnostic_{index}_completed_at={} login_diagnostic_{index}_words={}",
            crate::control::pct_encode(diagnostic.label()),
            time(Some(diagnostic.completed_at)),
            crate::control::pct_encode(&diagnostic.words()),
        ));
    }
    for (index, work) in progress.independent_work.iter().enumerate() {
        line.push_str(&format!(
            " independent_work_{index}_phase=signing-in independent_work_{index}_account={} independent_work_{index}_route={} independent_work_{index}_observed_at={}",
            text(Some(&work.label)),
            text(work.route.as_deref()),
            time(Some(work.observed_at)),
        ));
    }
    line.push('\n');
    line
}

#[cfg(test)]
#[path = "harness_recovery_verb_tests.rs"]
mod tests;
