// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Bounded census metadata for the already authenticated reservation owner.

use super::registry;

pub(super) fn value(page: registry::CensusPage) -> aterm_json::Value {
    let rows = page
        .rows
        .into_iter()
        .map(|row| {
            let condition = match row.condition {
                registry::CensusCondition::Unresolved => "unresolved",
                registry::CensusCondition::Unknown => "unknown",
                registry::CensusCondition::Manager => "manager",
                registry::CensusCondition::Reader => "reader",
                registry::CensusCondition::Quiescent => "quiescent",
                registry::CensusCondition::Retired => "retired",
                registry::CensusCondition::Disjoint => "disjoint",
            };
            let auth = row.auth.map(|auth| {
                aterm_json::json!({
                    "generation":auth.generation,
                    "owner":auth.owner,
                    "source":auth.source,
                    "login":auth.login,
                })
            });
            aterm_json::json!({
                "member":row.member,
                "registration":row.registration,
                "condition":condition,
                "credential_generation":row.credential_generation,
                "digest":row.digest,
                "auth":auth,
            })
        })
        .collect::<Vec<_>>();
    aterm_json::json!({"revision":page.revision,"rows":rows,"next":page.next})
}
