// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The bounded `updateRefs` protocol: validate every expected OID before any
//! ref changes. The fake has no commit-parent graph and does not model GitHub's
//! ancestry checks; it requires non-forced requests and known destination OIDs.

use std::collections::BTreeSet;

use aterm_json::{Value, json};

use super::{Answer, Ctx, Request, State, unwritable, well_formed_ref};

const ZERO: &str = "0000000000000000000000000000000000000000";

fn refused(message: &str) -> Answer {
    Answer::json(
        200,
        &json!({
            "data": {"updateRefs": null},
            "errors": [{"type": "UNPROCESSABLE", "message": message}],
        }),
    )
}

pub(super) fn update(state: &mut State, req: &Request, ctx: &Ctx) -> Answer {
    let Ok(body) = aterm_json::from_slice::<Value>(&req.body) else {
        return Answer::error(400, "Problems parsing JSON");
    };
    if !body
        .get("query")
        .and_then(Value::as_str)
        .is_some_and(|q| q.contains("updateRefs(input: $input)"))
    {
        return Answer::error(400, "Unsupported GraphQL operation");
    }
    let Some(input) = body.get("variables").and_then(|v| v.get("input")) else {
        return refused("Missing input");
    };
    let Some(node) = input.get("repositoryId").and_then(Value::as_str) else {
        return refused("Missing repository ID");
    };
    let Some(index) = node
        .strip_prefix("R_")
        .and_then(crate::parse_uint::<u64>)
        .and_then(|id| state.repo_by_id(id))
    else {
        return Answer::not_found();
    };
    let repo = &state.repos[index];
    if let Some(error) = unwritable(repo, ctx) {
        return error;
    }
    let Some(updates) = input
        .get("refUpdates")
        .and_then(Value::as_array)
        .filter(|u| !u.is_empty() && u.len() <= 128)
    else {
        return refused("Invalid update count");
    };
    let Some(echo) = input.get("clientMutationId").and_then(Value::as_str) else {
        return refused("Missing mutation ID");
    };
    let mut seen = BTreeSet::new();
    let mut staged = Vec::with_capacity(updates.len());
    for update in updates {
        let (Some(name), Some(before), Some(after)) = (
            update.get("name").and_then(Value::as_str),
            update.get("beforeOid").and_then(Value::as_str),
            update.get("afterOid").and_then(Value::as_str),
        ) else {
            return refused("Invalid update");
        };
        if !name.starts_with("refs/")
            || !well_formed_ref(name)
            || !seen.insert(name)
            || !crate::state::is_sha(before)
            || !crate::state::is_sha(after)
            || after == ZERO
            || update.get("force") != Some(&Value::Bool(false))
            || !repo.knows_sha(after)
        {
            return refused("Invalid or forced ref update");
        }
        if repo.ref_sha(name).unwrap_or(ZERO) != before {
            return refused("Expected OID differs");
        }
        staged.push((name, after));
    }
    for (name, after) in staged {
        state.repos[index].set_ref(name, after);
    }
    Answer::json(
        200,
        &json!({"data": {"updateRefs": {"clientMutationId": echo}}}),
    )
}
