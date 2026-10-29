// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read the same owner-bound recovery observation that the account menu shows.

use aterm_agent::harness::account_progress::{AccountProgress, ProgressPhase};

pub(crate) const USAGE: &str = "ERR usage: harness policy | recovery\n";

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
        "OK recovery version=1 available=1 phase={phase} account={} target={} route={} known={} eligible={} wall={} observed_at={} next_at={} detail_count={}",
        text(progress.account.as_deref()),
        text(target),
        text(route),
        count(progress.known),
        count(progress.eligible),
        text(progress.wall.as_deref()),
        time(Some(progress.observed_at)),
        time(progress.next_at),
        progress.details.len()
    );
    for (index, detail) in progress.details.iter().enumerate() {
        line.push_str(&format!(
            " detail_{index}={}",
            crate::control::pct_encode(detail)
        ));
    }
    line.push('\n');
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(line: &str, key: &str) -> String {
        line.split_whitespace()
            .find_map(|part| part.strip_prefix(&format!("{key}=")))
            .map(aterm_control::wire::pct_decode)
            .expect("field")
    }

    #[test]
    fn unauthorized_and_malformed_reads_never_fetch() {
        assert!(
            aterm_types::control_verbs::spec_for("harness", "recovery")
                .unwrap()
                .owner_only
        );
        assert!(
            !aterm_types::control_verbs::spec_for("harness", "policy")
                .unwrap()
                .owner_only
        );
        assert!(cmd("recovery", false, || panic!("private read")).starts_with("ERR denied:"));
        for rest in ["", "recovery now", "recover"] {
            assert_eq!(cmd(rest, true, || panic!("malformed read")), USAGE);
        }
        assert_eq!(cmd("recovery", true, || Err("busy".into())), "ERR busy\n");
    }

    #[test]
    fn absence_is_distinct_from_an_observed_unknown_and_zero_capacity() {
        assert_eq!(
            cmd("recovery", true, || Ok(None)),
            "OK recovery version=1 available=0\n"
        );
        let mut progress = AccountProgress::unknown(0);
        let unknown = reply(Some(&progress));
        assert_eq!(field(&unknown, "available"), "1");
        for key in ["known", "eligible", "observed_at", "next_at"] {
            assert_eq!(field(&unknown, key), "-");
        }
        progress.known = Some(2);
        progress.eligible = Some(0);
        let known = reply(Some(&progress));
        assert_eq!(field(&known, "known"), "2");
        assert_eq!(field(&known, "eligible"), "0");
    }

    #[test]
    fn full_details_targets_and_original_times_round_trip_on_one_line() {
        let mut progress = AccountProgress::unknown(101);
        progress.next_at = Some(102);
        progress.phase = ProgressPhase::SigningIn {
            label: "account two".into(),
            route: Some("vault → browser".into()),
        };
        progress.details = vec!["a=b\nsecond line %20".repeat(100), "Last blocker".into()];
        let line = reply(Some(&progress));
        assert_eq!(line.lines().count(), 1);
        assert_eq!(field(&line, "phase"), "signing-in");
        assert_eq!(field(&line, "target"), "account two");
        assert_eq!(field(&line, "route"), "vault → browser");
        assert_eq!(field(&line, "detail_count"), "2");
        for (index, detail) in progress.details.iter().enumerate() {
            assert_eq!(field(&line, &format!("detail_{index}")), *detail);
        }
        // Timestamps describe the retained observation, not when this read happened.
        assert_eq!(field(&line, "observed_at"), "101");
        assert_eq!(field(&line, "next_at"), "102");
    }
}
