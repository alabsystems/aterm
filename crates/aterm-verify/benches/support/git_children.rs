// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Safe labels and interval accounting for a serial publisher's accepted Git calls.

use agh::Github;
use agh::fx::child::ChildSpec;
use agh::fx::{GitChildTiming, NativePushTiming};
use agh::git::NativePushPhase;
use aterm_json::json;
use std::time::Instant;

// Inspect arguments only here, after timing. Never emit an unknown argument or spec.
fn verb(spec: &ChildSpec) -> &'static str {
    let mut args = spec.args.iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("-C" | "-c" | "--git-dir" | "--work-tree") => {
                if args.next().is_none() {
                    return "other";
                }
            }
            Some("--no-optional-locks" | "--no-replace-objects" | "--literal-pathspecs") => {}
            Some("init") => return "init",
            Some("config") => return "config",
            Some("rev-parse") => return "rev-parse",
            Some("ls-remote") => return "ls-remote",
            Some("for-each-ref") => return "for-each-ref",
            Some("cat-file") => return "cat-file",
            Some("rev-list") => return "rev-list",
            Some("hash-object") => return "hash-object",
            Some("symbolic-ref") => return "symbolic-ref",
            Some("read-tree") => return "read-tree",
            Some("write-tree") => return "write-tree",
            Some("check-ref-format") => return "check-ref-format",
            Some("fetch") => return "fetch",
            Some("pack-objects") => return "pack-objects",
            Some("index-pack") => return "index-pack",
            Some("fsck") => return "fsck",
            Some("update-ref") => return "update-ref",
            Some("merge-base") => return "merge-base",
            Some("push") => return "push",
            _ => return "other",
        }
    }
    "other"
}

pub(crate) fn observation(
    client: &Github,
    children: &[GitChildTiming],
    phases: &[NativePushTiming],
    began: Instant,
    finished: Instant,
) -> (aterm_json::Value, bool) {
    let specs = client.fx().children_run();
    let mut children = children.to_vec();
    children.sort_by_key(|s| s.ordinal);
    let ordered = !children.is_empty()
        && children.len() == specs.len()
        && children.iter().enumerate().all(|(i, s)| {
            s.ordinal == i
                && began <= s.started
                && s.started <= s.finished
                && s.finished <= finished
        })
        && children.windows(2).all(|p| p[0].finished <= p[1].started);
    let mut memberships = vec![0usize; children.len()];
    let mut phase_child_sum = Some(0u128);
    let mut phase_sum = Some(0u128);
    let mut parents_valid = true;
    let parents: Vec<_> = phases
        .iter()
        .map(|phase| {
            let nested: Vec<_> = children
                .iter()
                .enumerate()
                .filter(|(_, s)| phase.started <= s.started && s.finished <= phase.finished)
                .collect();
            let ordinals: Vec<_> = nested
                .iter()
                .map(|(i, s)| {
                    memberships[*i] += 1;
                    s.ordinal
                })
                .collect();
            let partial = children.iter().any(|s| {
                s.started < phase.finished
                    && phase.started < s.finished
                    && !(phase.started <= s.started && s.finished <= phase.finished)
            });
            let sum = nested.iter().try_fold(0u128, |total, (_, s)| {
                total.checked_add(s.finished.checked_duration_since(s.started)?.as_nanos())
            });
            let wall = phase
                .finished
                .checked_duration_since(phase.started)
                .map(|d| d.as_nanos());
            let remainder = wall.and_then(|wall| wall.checked_sub(sum?));
            phase_child_sum = phase_child_sum.and_then(|total| total.checked_add(sum?));
            phase_sum = phase_sum.and_then(|total| total.checked_add(wall?));
            parents_valid &= !partial
                && !ordinals.is_empty()
                && remainder.is_some()
                && began <= phase.started
                && phase.finished <= finished;
            let name = match phase.phase {
                NativePushPhase::MetadataPack => "metadata_pack",
                NativePushPhase::MetadataVerify => "metadata_verify",
                NativePushPhase::Connectivity => "connectivity",
            };
            json!({"phase":name,"ordinals":ordinals,"child_ns":sum,"non_child_ns":remainder})
        })
        .collect();
    let child_sum = children.iter().try_fold(0u128, |total, s| {
        total.checked_add(s.finished.checked_duration_since(s.started)?.as_nanos())
    });
    let whole = finished.checked_duration_since(began).map(|d| d.as_nanos());
    let non_child = whole.and_then(|wall| wall.checked_sub(child_sum?));
    let child_outside = child_sum.and_then(|sum| sum.checked_sub(phase_child_sum?));
    let phase_non_child = phase_sum.and_then(|sum| sum.checked_sub(phase_child_sum?));
    let outside_non_child = non_child.and_then(|sum| sum.checked_sub(phase_non_child?));
    let rows: Vec<_> = children
        .iter()
        .map(|s| {
            json!({"ordinal":s.ordinal,
        "verb":specs.get(s.ordinal).map_or("other",verb),
        "start_offset_ns":s.started.checked_duration_since(began).map(|d|d.as_nanos()),
        "end_offset_ns":s.finished.checked_duration_since(began).map(|d|d.as_nanos()),
        "wall_ns":s.finished.checked_duration_since(s.started).map(|d|d.as_nanos()),
        "code":s.code,"error_kind":s.error.map(|kind|format!("{kind:?}"))})
        })
        .collect();
    let valid = ordered
        && parents_valid
        && memberships.iter().all(|n| *n <= 1)
        && non_child.is_some()
        && child_outside.is_some()
        && outside_non_child.is_some();
    (
        json!({"spans":rows,"child_ns":child_sum,"non_child_ns":non_child,
        "children_inside_native_phases_ns":phase_child_sum,"children_outside_native_phases_ns":child_outside,
        "native_phase_non_child_ns":phase_non_child,"outside_native_non_child_ns":outside_non_child,
        "phase_children":parents,"valid":valid}),
        valid,
    )
}
