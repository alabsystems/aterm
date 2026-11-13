// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Partitions learned from successful libtest case timings. Planning starts no process.
//! A filter and its skip complement cover the original invocation, including new cases.

use std::collections::BTreeSet;
use std::time::Duration;

use crate::testrun::{Binary, Passed};

const LONG: Duration = Duration::from_secs(120);

fn valid(prefix: &str) -> bool {
    !prefix.is_empty()
        && prefix.len() <= crate::report::EVENT_ROW_BYTES
        && !prefix.starts_with('-')
        && !prefix.contains('\0')
        && !prefix.chars().any(char::is_whitespace)
}

/// Choose among module filters and the longest case, nearest half the observed time. A zero-time
/// case gets one nanosecond of weight, so trivial cases still get a deterministic split.
pub(crate) fn learn(timings: &[crate::exec::CaseTiming], wall: Duration) -> Option<String> {
    if wall < LONG {
        return None;
    }
    let cases: Vec<_> = timings
        .iter()
        .map(|timing| (timing.case.as_str(), timing.execution.as_nanos().max(1)))
        .collect();
    let mut prefixes = BTreeSet::new();
    if let Some((name, _)) = cases.iter().min_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)))
        && valid(name)
    {
        prefixes.insert(*name);
    }
    for (name, _) in &cases {
        for (at, _) in name.match_indices("::") {
            let prefix = &name[..at + 2];
            if valid(prefix) {
                prefixes.insert(prefix);
            }
        }
    }
    let total = cases
        .iter()
        .fold(0_u128, |sum, (_, time)| sum.saturating_add(*time));
    prefixes
        .into_iter()
        .filter_map(|prefix| {
            // Count the same substring matches that libtest's filter and --skip use.
            let (count, selected) = cases
                .iter()
                .filter(|(name, _)| name.contains(prefix))
                .fold((0, 0_u128), |(count, sum), (_, time)| {
                    (count + 1, sum.saturating_add(*time))
                });
            (count > 0 && count < cases.len())
                .then_some((selected.abs_diff(total.saturating_sub(selected)), prefix))
        })
        .min()
        .map(|(_, prefix)| prefix.to_owned())
}

fn split(binary: &Binary, prefix: &str) -> [Binary; 2] {
    let mut selected = binary.clone();
    selected.record.argv.push(prefix.into());
    selected.spec = format!("{} -- {prefix}", binary.spec);
    let mut rest = binary.clone();
    rest.record.argv.extend(["--skip".into(), prefix.into()]);
    rest.spec = format!("{} -- --skip {prefix}", binary.spec);
    [selected, rest]
}

fn spec_boundary<'a>(spec: &'a str, parent: &str) -> Option<&'a str> {
    let suffix = spec.strip_prefix(parent)?.strip_prefix(" -- ")?;
    let prefix = suffix.strip_prefix("--skip ").unwrap_or(suffix);
    valid(prefix).then_some(prefix)
}

/// Use a prior hop's boundary first, preserving its commands for carry. Otherwise use the
/// latest learned boundary. Unknown case times, custom harnesses and caller filters keep the
/// original invocation. Nothing runs, and no observation grants a verdict.
pub(crate) fn expand(
    binaries: Vec<Binary>,
    history: &crate::test_history::History,
    passed: Option<&Passed>,
) -> Vec<Binary> {
    let mut expanded = Vec::new();
    for binary in binaries {
        let long = history
            .seen(&binary.spec)
            .is_some_and(|seen| seen.wall >= LONG);
        if long && binary.harness && binary.record.argv.len() == 1 {
            let prefix = passed
                .and_then(|passed| {
                    passed
                        .keys()
                        .find_map(|spec| spec_boundary(spec, &binary.spec))
                })
                .or_else(|| {
                    history
                        .row(&binary.spec)?
                        .samples
                        .iter()
                        .rev()
                        .filter_map(|sample| sample.partition.as_deref())
                        .find(|prefix| valid(prefix))
                });
            if let Some(prefix) = prefix {
                let halves = split(&binary, prefix);
                // Events and carry identify the exact rerun spec; never introduce a clipped key.
                if halves.iter().any(|half| {
                    half.spec
                        .chars()
                        .map(crate::report::json_char_bytes)
                        .sum::<usize>()
                        > crate::report::EVENT_ROW_BYTES
                }) {
                    expanded.push(binary);
                    continue;
                }
                let short = history
                    .seen(&halves[0].spec)
                    .zip(history.seen(&halves[1].spec))
                    .is_some_and(|(a, b)| a.wall.saturating_add(b.wall) < LONG);
                if !short {
                    expanded.extend(halves);
                    continue;
                }
            }
        }
        expanded.push(binary);
    }
    expanded
}

/// Until both halves have measurements, inherit the parent's density and peak and predict half
/// its work. This estimate is never persisted as a measured sample.
pub(crate) fn seen(
    history: &crate::test_history::History,
    spec: &str,
) -> Option<crate::test_plan::Seen> {
    history.seen(spec).or_else(|| {
        let (parent, _) = spec.split_once(" -- ")?;
        let mut seen = history.seen(parent)?;
        seen.wall /= 2;
        seen.tests = seen.tests.div_ceil(2);
        Some(seen)
    })
}

#[cfg(test)]
#[path = "test_shards_tests.rs"]
mod tests;
