// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `agh release wait`: poll a release until every condition holds.

use std::time::Duration;

use super::args::Invocation;
use super::verbs::Noun;
use super::{Answer, Ctx, Outcome, Receipt, out};
use crate::error::{Error, Kind, Result, quote};
use crate::policy::Access;
use crate::wait::{
    Backoff, Condition, Poller, ReleaseCond, WaitEnd, WaitEvent, refuse_tag, release_condition,
};

fn field(value: &str) -> String {
    let mut field = String::new();
    out::receipt_value(&mut field, value);
    field
}

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(1800);
const HEARTBEAT: Duration = Duration::from_secs(60);
const BLIND_LIMIT: Duration = Duration::from_secs(60);

/// `agh release wait REPO TAG COND… [--include-drafts] [--timeout DUR]`.
///
/// # Errors
///
/// Usage for a bad condition; CouldNotRun when blind for 60 s; a condition's error when it can
/// never hold. A timeout is the state on stdout and Pending (exit 8).
pub(crate) fn release_wait(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let timeout = ctx.timeout.take().unwrap_or(DEFAULT_TIMEOUT);
    if inv.global.no_probe {
        return Err(inv.usage(
            "release wait reads a 404 as no release only after the repository probe: drop \
             --no-probe",
        ));
    }
    let gh = ctx.github()?;
    let (slug, named) = inv.repo(gh.router())?;
    let rest = inv.rest();
    let Some((tag, conds)) = rest.split_first() else {
        return Err(inv.usage("TAG is required"));
    };
    refuse_tag(tag, &slug)?;
    if conds.is_empty() {
        return Err(inv.usage("COND is required"));
    }
    let conds: Vec<ReleaseCond> = conds
        .iter()
        .map(|text| ReleaseCond::parse(text))
        .collect::<Result<_>>()?;
    let access = if inv.has("--include-drafts") {
        Access::WriterRead
    } else {
        Access::Read
    };
    let how = inv.how(access, named);
    let fors: Vec<String> = conds.iter().map(ReleaseCond::slug).collect();
    let mut conditions: Vec<Box<dyn Condition>> = conds
        .into_iter()
        .map(|cond| release_condition(&slug, tag, cond, how.clone()))
        .collect();
    let poller = Poller {
        fx: gh.fx().as_ref(),
        backoff: Backoff::STANDARD,
        timeout,
        heartbeat: HEARTBEAT,
        blind_limit: BLIND_LIMIT,
    };
    let repo = slug.to_string();
    let fors = fors.join(",");
    let end = poller.run(&gh, &mut conditions, &mut |event| {
        if let WaitEvent::Heartbeat { elapsed, last } = event {
            let line = format!(
                "agh: waiting verb=release.wait repo={} for={} elapsed={}s last={}",
                field(&ctx.redactor.apply(&repo)),
                field(&ctx.redactor.apply(&fors)),
                elapsed.as_secs(),
                quote(last)
            );
            out::note(ctx, &line);
        }
    });
    let receipt = Receipt::for_repo(inv, &gh, &slug);
    let answer = match end {
        WaitEnd::Held(state) => Answer::Object {
            noun: Noun::Wait,
            value: state,
        },
        WaitEnd::TimedOut { last } => {
            let pending: Vec<&str> = last
                .get("conditions")
                .and_then(aterm_json::Value::as_array)
                .map(|conditions| {
                    conditions
                        .iter()
                        .filter(|c| {
                            c.get("holds").and_then(aterm_json::Value::as_bool) != Some(true)
                        })
                        .filter_map(|c| c.get("cond").and_then(aterm_json::Value::as_str))
                        .collect()
                })
                .unwrap_or_default();
            let error = Error::new(
                Kind::Pending,
                format!(
                    "the wait timed out after {}s: {}",
                    timeout.as_secs(),
                    pending.join(", ")
                ),
            )
            .with_next(format!(
                "wait longer (--timeout), or look: agh release view {repo} {tag}"
            ))
            .with_repo(repo.clone());
            Answer::FailedWith { value: last, error }
        }
        WaitEnd::Blind { last_error } => {
            let mut error = Error::could_not_run(format!(
                "no successful observation for {}s: {}",
                BLIND_LIMIT.as_secs(),
                last_error.why
            ))
            .with_repo(repo);
            error.http.clone_from(&last_error.http);
            error.identity.clone_from(&last_error.identity);
            error.rate.clone_from(&last_error.rate);
            error.reset = last_error.reset;
            error.retry_after = last_error.retry_after;
            return Err(error);
        }
        WaitEnd::Impossible(error) => return Err(error),
    };
    Ok(Outcome { answer, receipt })
}
