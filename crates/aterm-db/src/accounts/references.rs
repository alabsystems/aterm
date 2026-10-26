// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Diagnostics for explicit proposal references. This is neither a credential
//! resolver nor an admission decision: it reads no secret and grants no rights.

use super::proposals::Snapshot;
use crate::database::Cell;
use aterm_codec::hex;
use aterm_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const MAX_OBJECTS: usize = 8192;
const MAX_STEPS: usize = 65_536;
const MAX_ISSUES: usize = 2048;
const MAX_ALIAS_DEPTH: usize = 64;
const MAX_EVIDENCE: usize = 32;
type Key = (String, String);

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Issue {
    object: Key,
    field: String,
    code: &'static str,
    target: Option<Key>,
}

struct Graph<'a> {
    snapshot: &'a Snapshot,
    issues: BTreeSet<Issue>,
    limits: BTreeSet<&'static str>,
    steps: usize,
}
impl<'a> Graph<'a> {
    fn issue(&mut self, object: &Key, field: &str, code: &'static str, target: Option<Key>) {
        let issue = Issue {
            object: object.clone(),
            field: field.into(),
            code,
            target,
        };
        if self.issues.contains(&issue) {
            return;
        }
        if self.issues.len() == MAX_ISSUES {
            self.limits.insert("issues");
        } else {
            self.issues.insert(issue);
        }
    }

    fn step(&mut self) -> bool {
        if self.steps == MAX_STEPS {
            self.limits.insert("steps");
            return false;
        }
        self.steps += 1;
        true
    }

    fn target_issue(
        &mut self,
        object: &Key,
        field: &str,
        target: &Key,
        own: &'static str,
        linked: &'static str,
    ) {
        let root = field.is_empty() && object == target;
        self.issue(
            object,
            field,
            if root { own } else { linked },
            (!root).then(|| target.clone()),
        );
    }

    fn get(&mut self, object: &Key, field: &str, target: &Key) -> Option<&'a Value> {
        if !self.step() {
            return None;
        }
        let Some(cell) = self.snapshot.cells.get(target) else {
            self.target_issue(object, field, target, "missing-object", "missing-reference");
            return None;
        };
        if cell.stopped {
            self.target_issue(object, field, target, "stopped-object", "stopped-reference");
            return None;
        }
        if cell.candidates.is_empty() {
            self.target_issue(object, field, target, "empty-object", "empty-reference");
            return None;
        }
        if cell.candidates.len() != 1 {
            self.target_issue(
                object,
                field,
                target,
                "conflicted-object",
                "conflicted-reference",
            );
            return None;
        }
        let value = &cell.candidates[0].value;
        if value.is_null() {
            self.issue(object, field, "deleted-reference", Some(target.clone()));
            return None;
        }
        let valid = match target.0.as_str() {
            "accounts" => super::validate(value),
            "mailboxes" | "email-addresses" | "phones" => {
                crate::contacts::validate(&target.0, value)
            }
            "machine-proposals" => crate::machines::validate(&target.1, value),
            _ => Err(crate::Error::new("unknown proposal collection")),
        };
        if valid.is_err() {
            self.target_issue(object, field, target, "invalid-object", "invalid-reference");
            return None;
        }
        Some(value)
    }

    /// Follow only declared links. The result is a mailbox claim for detecting
    /// contradictions, even if a linked record is broken. It is never a route.
    fn email(
        &mut self,
        object: &Key,
        field: &str,
        id: &str,
        mut initial: Option<&'a Value>,
    ) -> Option<String> {
        let mut next = Some(id.to_owned());
        let mut seen = BTreeSet::new();
        let mut mailboxes = BTreeSet::new();
        while let Some(id) = next.take() {
            let target = ("email-addresses".into(), id.clone());
            if !seen.insert(id.clone()) {
                self.issue(object, field, "alias-cycle", Some(target));
                break;
            }
            if seen.len() > MAX_ALIAS_DEPTH {
                self.limits.insert("alias-depth");
                self.issue(object, field, "alias-depth-exceeded", Some(target));
                break;
            }
            let value = if let Some(value) = initial.take() {
                value
            } else {
                let Some(value) = self.get(object, field, &target) else {
                    break;
                };
                value
            };
            if let Some(mailbox) = value.get("mailbox_id").and_then(Value::as_str) {
                let target = ("mailboxes".into(), mailbox.into());
                let _ = self.get(object, field, &target);
                mailboxes.insert(mailbox.to_owned());
            }
            next = value
                .get("alias_of")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        if mailboxes.len() > 1 {
            self.issue(object, field, "alias-mailbox-conflict", None);
            return None;
        }
        // An absent mailbox is valid partial metadata. Do not infer delivery.
        mailboxes.into_iter().next()
    }

    fn identity(&mut self, object: &Key, field: &str, kind: &str, id: &str) -> Option<String> {
        match kind {
            "email" => self.email(object, field, id, None),
            "phone" => {
                let _ = self.get(object, field, &("phones".into(), id.into()));
                None
            }
            _ => {
                if !self.step() {
                    return None;
                }
                let email = self
                    .snapshot
                    .cells
                    .contains_key(&("email-addresses".into(), id.into()));
                if !self.step() {
                    return None;
                }
                let phone = self
                    .snapshot
                    .cells
                    .contains_key(&("phones".into(), id.into()));
                match (email, phone) {
                    (true, false) => self.email(object, field, id, None),
                    (false, true) => {
                        let _ = self.get(object, field, &("phones".into(), id.into()));
                        None
                    }
                    (true, true) => {
                        self.issue(object, field, "identity-namespace-ambiguous", None);
                        None
                    }
                    (false, false) => {
                        self.issue(object, field, "missing-identity", None);
                        None
                    }
                }
            }
        }
    }

    fn auth(&mut self, object: &Key, value: &Value) {
        let Some(routes) = value.get("auth").and_then(Value::as_array) else {
            return;
        };
        for (index, route) in routes.iter().enumerate() {
            let field = format!("auth[{index}].identity");
            let mailbox = route
                .get("identity")
                .and_then(Value::as_str)
                .and_then(|id| {
                    let kind = match route.get("kind").and_then(Value::as_str) {
                        Some("email-link") => "email",
                        Some("phone") => "phone",
                        _ => "unspecified",
                    };
                    self.identity(object, &field, kind, id)
                });
            if let Some(declared) = route.get("mailbox").and_then(Value::as_str) {
                let field = format!("auth[{index}].mailbox");
                let target = ("mailboxes".into(), declared.into());
                let _ = self.get(object, &field, &target);
                if mailbox
                    .as_deref()
                    .is_some_and(|resolved| resolved != declared)
                {
                    self.issue(object, &field, "route-mailbox-conflict", Some(target));
                }
            }
        }
    }

    fn details(&mut self, object: &Key, field: &str, value: &Value) {
        let Some(details) = value.get(field).and_then(Value::as_object) else {
            return;
        };
        for (name, detail) in details {
            let (kind, collection) = match detail.get("kind").and_then(Value::as_str) {
                Some("phone-ref") => ("phone", "phones"),
                Some("email-ref") => ("email", "email-addresses"),
                // Secrets and source identifiers have no contact-table authority.
                _ => continue,
            };
            if !self.step() {
                break;
            }
            let field = format!("{field}[{name}]");
            let Some(reference) = detail.get("value").and_then(Value::as_str) else {
                continue;
            };
            let id = match reference.split_once(':') {
                None if crate::crypto::name(reference) => reference,
                Some((prefix, id)) if prefix == collection && crate::crypto::name(id) => id,
                _ => {
                    self.issue(object, &field, "unsupported-contact-reference", None);
                    continue;
                }
            };
            let _ = self.identity(object, &field, kind, id);
        }
    }

    fn object(&mut self, key: &Key, value: &'a Value) {
        match key.0.as_str() {
            "email-addresses" => {
                let _ = self.email(key, "delivery", &key.1, Some(value));
            }
            "accounts" => {
                if let Some(identities) = value.get("identities").and_then(Value::as_array) {
                    for (index, identity) in identities.iter().enumerate() {
                        if let (Some(kind), Some(id)) = (
                            identity.get("kind").and_then(Value::as_str),
                            identity.get("id").and_then(Value::as_str),
                        ) {
                            let _ = self.identity(key, &format!("identities[{index}]"), kind, id);
                        }
                    }
                }
                self.auth(key, value);
            }
            "mailboxes" => self.auth(key, value),
            _ => {}
        }
        // Follow typed contact references only at the root inspection. A linked
        // contact's details are inspected in its own turn, never recursively.
        self.details(key, "details", value);
        if key.0 == "accounts" {
            self.details(key, "source", value);
        }
    }
}

fn evidence(cell: Option<&Cell>) -> Value {
    let count = cell.map_or(0, |cell| cell.candidates.len());
    let operations = cell
        .into_iter()
        .flat_map(|cell| &cell.candidates)
        .map(|candidate| candidate.operation)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .take(MAX_EVIDENCE)
        .map(|operation| hex::encode_lower(&operation))
        .collect::<Vec<_>>();
    json!({"operations":operations,"candidate_count":count,"truncated":count>MAX_EVIDENCE})
}

/// Inspect only observed local proposals. Missing optional metadata is not an
/// error. Neither issue absence nor a named secret proves a usable login, a
/// delivery route, provider support, machine assignment or spend authority.
/// Typed phone/email details use exact local contact IDs; other qualifications
/// are reported without echoing their value. Secret/source references stay opaque.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAccountReferences",
        action = "Clean",
        project = "aterm_db::accounts::references::tests::reference_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAccountReferences",
        action = "Cycle",
        project = "aterm_db::accounts::references::tests::reference_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAccountReferences",
        action = "Conflict",
        project = "aterm_db::accounts::references::tests::reference_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAccountReferences",
        action = "Deleted",
        project = "aterm_db::accounts::references::tests::reference_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAccountReferences",
        action = "Route",
        project = "aterm_db::accounts::references::tests::reference_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAccountReferences",
        action = "Duplicate",
        project = "aterm_db::accounts::references::tests::reference_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAccountReferences",
        action = "Namespace",
        project = "aterm_db::accounts::references::tests::reference_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAccountReferences",
        action = "Limit",
        project = "aterm_db::accounts::references::tests::reference_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAccountReferences",
        action = "DetailMissing",
        project = "aterm_db::accounts::references::tests::reference_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAccountReferences",
        action = "DetailAlias",
        project = "aterm_db::accounts::references::tests::reference_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAccountReferences",
        action = "DetailClean",
        project = "aterm_db::accounts::references::tests::reference_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAccountReferences",
        action = "DetailSyntax",
        project = "aterm_db::accounts::references::tests::reference_transition"
    )
)]
pub(super) fn inspect(snapshot: &Snapshot) -> Value {
    let mut graph = Graph {
        snapshot,
        issues: BTreeSet::new(),
        limits: BTreeSet::new(),
        steps: 0,
    };
    let mut addresses = BTreeMap::<String, Vec<Key>>::new();
    let mut inspected = 0;
    for (key, cell) in &snapshot.cells {
        if graph.steps == MAX_STEPS {
            graph.limits.insert("steps");
            break;
        }
        if inspected == MAX_OBJECTS {
            graph.limits.insert("objects");
            break;
        }
        inspected += 1;
        // An intentional deletion is not itself a dangling reference.
        if !cell.stopped && cell.candidates.len() == 1 && cell.candidates[0].value.is_null() {
            continue;
        }
        let Some(value) = graph.get(key, "", key) else {
            continue;
        };
        if key.0 == "email-addresses"
            && let Some(address) = value.get("address").and_then(Value::as_str)
        {
            addresses
                .entry(address.into())
                .or_default()
                .push(key.clone());
        }
        graph.object(key, value);
    }
    for keys in addresses.values().filter(|keys| keys.len() > 1) {
        for key in keys {
            graph.issue(key, "address", "duplicate-exact-address", None);
        }
    }
    let issues = graph.issues.iter().map(|issue| json!({
        "collection":issue.object.0,"key":issue.object.1,"field":issue.field,"code":issue.code,
        "evidence":evidence(snapshot.cells.get(&issue.object)),
        "target":issue.target.as_ref().map(|key|json!({"collection":key.0,"key":key.1,
            "evidence":evidence(snapshot.cells.get(key))}))
    })).collect::<Vec<_>>();
    json!({
        "schema":1,"kind":"account-reference-check","scope":"observed-local-proposals",
        "table":hex::encode_lower(&snapshot.scope.digest()),
        "authority":"none","machine_assignments":"proposals-only",
        "inspected_objects":inspected,"issues":issues,
        "truncated":!graph.limits.is_empty(),"limits_hit":graph.limits.into_iter().collect::<Vec<_>>(),
        "observed_history":{
            "retained_records":snapshot.retained,"withheld_records":snapshot.withheld,
            "unavailable_records":snapshot.operations.iter().filter(|op|!op.available).count(),
            "refused_records":snapshot.operations.iter().filter(|op|op.refusal.is_some()).count()
        }
    })
}

#[cfg(test)]
mod tests;
