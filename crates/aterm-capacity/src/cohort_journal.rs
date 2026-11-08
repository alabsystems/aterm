// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Strict, newline-framed custody journal. Observations decoded here never mint spawn proofs.

use crate::{
    cohort::{Lifetime, RecordedCohort, Scope},
    ledger::Record,
};
use aterm_json::Value;
use aterm_uds::{
    process::{self, Identity},
    spawnfd::BirthKey,
};
use std::io;

fn bad() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "incomplete or invalid cohort custody journal",
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Holder {
    identity: Identity,
    scope: Scope,
}

impl Holder {
    fn capture() -> io::Result<Self> {
        let identity = process::read_identity(std::process::id() as i32).ok_or_else(bad)?;
        let scope = Scope::read().ok_or_else(bad)?;
        let holder = Self { identity, scope };
        if !holder.current() {
            return Err(bad());
        }
        Ok(holder)
    }

    fn current(&self) -> bool {
        self.scope.ended_or_unknown().is_none()
            && process::read_identity(self.identity.pid) == Some(self.identity)
    }

    fn owner(&self) -> bool {
        self.identity.pid == std::process::id() as i32 && self.current()
    }

    fn encode(&self) -> Value {
        aterm_json::json!({ "identity": self.identity.token(), "scope": self.scope.encode() })
    }

    fn decode(value: &Value) -> Option<Self> {
        (value.as_object()?.len() == 2).then_some(())?;
        let scope = Scope::decode(value.get("scope")?)?;
        let identity =
            Identity::parse_shaped(value.get("identity")?.as_str()?, scope.versioned_pid())?;
        Some(Self { identity, scope })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Birth {
    nonce: String,
    holder: Holder,
}

impl Birth {
    fn of_key(key: &BirthKey) -> Self {
        Self {
            nonce: key.nonce().to_owned(),
            holder: Holder {
                identity: key.owner(),
                scope: Scope::of_key(key),
            },
        }
    }

    fn encode(&self) -> Value {
        aterm_json::json!({ "nonce": self.nonce, "holder": self.holder.encode() })
    }

    fn decode(value: &Value) -> Option<Self> {
        (value.as_object()?.len() == 2).then_some(())?;
        let nonce = value.get("nonce")?.as_str()?;
        (nonce.len() == NONCE_BYTES
            && nonce
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
        .then_some(())?;
        Some(Self {
            nonce: nonce.to_owned(),
            holder: Holder::decode(value.get("holder")?)?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum State {
    Description,
    Armed(Birth),
    Bound(Birth, RecordedCohort),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClosurePolicy {
    Group,
    BootOnly,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Subgroup {
    key: Birth,
    captured: Option<RecordedCohort>,
}

const NONCE_BYTES: usize = 32;
// Every subgroup needs at least its nonce in the bounded input. This is only a defensive
// parser bound; the durable record's existing byte budget decides admission before spawn.
const SUBGROUP_MAX: usize = crate::ledger::RECORD_MAX as usize / NONCE_BYTES;

impl ClosurePolicy {
    fn decode(value: &Value) -> Option<Self> {
        let map = value.as_object()?;
        if map.len() != 2 || value.get("v")?.as_u64()? != 1 {
            return None;
        }
        match value.get("kind")?.as_str()? {
            "group" => Some(Self::Group),
            "boot_only" => Some(Self::BootOnly),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Journal {
    holder: Holder,
    state: State,
    policy: ClosurePolicy,
    armed_once: bool,
    cohort_owned: bool,
    subgroups: Vec<Subgroup>,
    gnu: Option<(String, bool)>,
}

impl Journal {
    pub(crate) fn funded_scope(&self, claim: crate::CohortClosureRef) -> crate::FundedScope {
        let mut unresolved = matches!(self.state, State::Armed(_))
            || self.cohort_owned && matches!(self.state, State::Description)
            || self.armed_once && self.policy == ClosurePolicy::BootOnly
            || self.gnu.as_ref().is_some_and(|(_, retired)| !retired)
                && self.native_lifetime() != Lifetime::Present;
        let mut groups = Vec::new();
        if let State::Bound(_, group) = &self.state {
            groups.push(group.clone());
        }
        for subgroup in &self.subgroups {
            match &subgroup.captured {
                Some(group) => groups.push(group.clone()),
                None => unresolved = true,
            }
        }
        crate::FundedScope {
            claim,
            scope: self.holder.scope.clone(),
            holder: (!self.cohort_owned).then_some(self.holder.identity),
            groups,
            unresolved,
        }
    }
    #[cfg(test)]
    pub(crate) fn observation(&self) -> (i64, Lifetime, bool) {
        let phase = match self.state {
            State::Description => 0,
            State::Armed(_) => 1,
            State::Bound(_, _) => 2,
        };
        (phase, self.lifetime(), self.holder.current())
    }
    pub(crate) fn description(&self) -> bool {
        matches!(self.state, State::Description)
    }
    pub(crate) fn head(text: &str, namespace: &str) -> io::Result<(String, Self)> {
        Self::head_accounted(text, namespace, crate::Accounting::Holder)
    }

    pub(crate) fn head_accounted(
        text: &str,
        namespace: &str,
        accounting: crate::Accounting,
    ) -> io::Result<(String, Self)> {
        let cohort_owned = accounting == crate::Accounting::CohortOwned;
        let holder = Holder::capture()?;
        let mut head: Value = aterm_json::from_str(text).map_err(|_| bad())?;
        let map = head.as_object_mut().ok_or_else(bad)?;
        map.insert(
            "v".into(),
            Value::from(if cohort_owned { 3u64 } else { 2u64 }),
        );
        if cohort_owned {
            map.insert("reservation_law".into(), Value::from("cohort_owned"));
        }
        map.insert("namespace".into(), Value::from(namespace));
        map.insert("checkout_complete".into(), Value::from(true));
        map.insert("owner".into(), holder.encode());
        map.insert(
            "closure_policy".into(),
            aterm_json::json!({"v": 1, "kind": "group"}),
        );
        Ok((
            line(&head)?,
            Self {
                holder,
                state: State::Description,
                policy: ClosurePolicy::Group,
                armed_once: false,
                cohort_owned,
                subgroups: Vec::new(),
                gnu: None,
            },
        ))
    }

    /// Never skip a torn or unrecognized tail, including ordinary grant/write/credit lines.
    pub(crate) fn decode(
        text: &str,
        admitted: bool,
        namespace: &str,
    ) -> io::Result<(Record, Self)> {
        if text.len() as u64 > crate::ledger::RECORD_MAX || !text.ends_with('\n') {
            return Err(bad());
        }
        let mut values = text
            .lines()
            .map(|line| aterm_json::from_str::<Value>(line).map_err(|_| bad()));
        let mut head = values.next().ok_or_else(bad)??;
        if !matches!(head.get("v").and_then(Value::as_u64), Some(2 | 3))
            || head.get("namespace").and_then(Value::as_str) != Some(namespace)
        {
            return Err(bad());
        }
        if !strict_head(&head) {
            return Err(bad());
        }
        let cohort_owned = head.get("v").and_then(Value::as_u64) == Some(3);
        let holder = Holder::decode(head.get("owner").ok_or_else(bad)?).ok_or_else(bad)?;
        if head.get("pid").and_then(Value::as_u64) != u64::try_from(holder.identity.pid).ok() {
            return Err(bad());
        }
        let mut journal = Self {
            holder,
            state: State::Description,
            policy: ClosurePolicy::decode(head.get("closure_policy").ok_or_else(bad)?)
                .ok_or_else(bad)?,
            armed_once: false,
            cohort_owned,
            subgroups: Vec::new(),
            gnu: None,
        };
        head.as_object_mut()
            .ok_or_else(bad)?
            .insert("v".into(), Value::from(1u64));
        let mut ordinary = line(&head)?;
        let mut grants = 0usize;
        let mut custody_events = 0usize;
        for value in values {
            let value = value?;
            let map = value.as_object().ok_or_else(bad)?;
            if let Some(event) = map.get("cohort") {
                if !admitted || grants != 1 || map.len() != 1 {
                    return Err(bad());
                }
                if event.get("phase").and_then(Value::as_str) == Some("snapshot") {
                    if custody_events != 0 {
                        return Err(bad());
                    }
                    journal.read_snapshot(event)?;
                } else {
                    journal.apply(event)?;
                }
                custody_events += 1;
            } else {
                let valid = if map.contains_key("admitted_ms") {
                    grants += 1;
                    (map.len() == 2
                        || (map.len() == 3 && map.get("alone").and_then(Value::as_bool).is_some()))
                        && map.get("admitted_ms").and_then(Value::as_u64).is_some()
                        && map
                            .get("cores")
                            .and_then(Value::as_u64)
                            .is_some_and(|c| u32::try_from(c).is_ok())
                } else if map.len() == 1
                    && (map.contains_key("next_ms") || map.contains_key("wrote"))
                {
                    map.values().next().and_then(Value::as_u64).is_some()
                } else if map.len() == 1 && map.contains_key("timing") && grants == 1 {
                    // Observation only: malformed proof decodes as unknown in Record without
                    // discarding the live reservation. It changes no cohort authority/state.
                    true
                } else if map.len() == 1 && map.contains_key("credit") {
                    map.get("credit")
                        .and_then(Value::as_array)
                        .is_some_and(|pids| {
                            pids.len() <= crate::admit::CREDIT_MAX
                                && pids
                                    .iter()
                                    .all(|p| p.as_u64().is_some_and(|p| u32::try_from(p).is_ok()))
                        })
                } else {
                    false
                };
                if !valid {
                    return Err(bad());
                }
                ordinary.push_str(&line(&value)?);
            }
        }
        if grants > 1 || (admitted && grants != 1) {
            return Err(bad());
        }
        let mut record = Record::decode(&ordinary, admitted).ok_or_else(bad)?;
        if journal.cohort_owned {
            record.accounting = crate::Accounting::CohortOwned;
        } else if !matches!(journal.state, State::Description) || !journal.holder.current() {
            record.accounting = crate::ledger::Accounting::Whole;
        }
        Ok((record, journal))
    }

    fn apply(&mut self, event: &Value) -> io::Result<()> {
        let phase = event.get("phase").and_then(Value::as_str).ok_or_else(bad)?;
        if matches!(phase, "gnu_prepare" | "gnu_retire") {
            let id = event.get("id").and_then(Value::as_str).ok_or_else(bad)?;
            if event.as_object().is_none_or(|map| map.len() != 2)
                || id.len() != 32
                || !id
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(bad());
            }
            match (phase, &mut self.gnu) {
                ("gnu_prepare", None)
                    if !self.armed_once && matches!(self.state, State::Description) =>
                {
                    self.gnu = Some((id.to_owned(), false));
                }
                ("gnu_retire", Some((original, retired))) if original == id && !*retired => {
                    *retired = true;
                }
                _ => return Err(bad()),
            }
            return Ok(());
        }
        if phase == "boot_only" {
            if event.as_object().is_none_or(|map| map.len() != 1)
                || self.armed_once
                || !self.description()
                || self.policy != ClosurePolicy::Group
            {
                return Err(bad());
            }
            self.policy = ClosurePolicy::BootOnly;
            return Ok(());
        }
        let key = Birth::decode(event.get("key").ok_or_else(bad)?).ok_or_else(bad)?;
        if key.holder != self.holder {
            return Err(bad());
        }
        let size = event.as_object().ok_or_else(bad)?.len();
        if phase.ends_with("_subgroup") {
            return self.apply_subgroup(phase, key, event, size);
        }
        self.state = match (&self.state, phase, size) {
            (State::Description, "arm", 2) => State::Armed(key),
            (State::Armed(armed), "cancel", 2) if armed == &key => State::Description,
            (State::Bound(bound, _), "finish", 2)
                if bound == &key
                    && self.subgroups.iter().all(|group| group.captured.is_some())
                    && (self.policy == ClosurePolicy::Group
                        || self.holder.scope.ended_or_unknown() == Some(Lifetime::Drained)) =>
            {
                State::Description
            }
            (State::Armed(armed), "bind", 3) if armed == &key => {
                let captured = RecordedCohort::decode(event.get("captured").ok_or_else(bad)?)
                    .ok_or_else(bad)?;
                if !captured.in_scope(&key.holder.scope) {
                    return Err(bad());
                }
                State::Bound(key, captured)
            }
            _ => return Err(bad()),
        };
        if phase == "arm" {
            self.armed_once = true;
        } else if phase == "finish" {
            self.subgroups.clear();
        }
        Ok(())
    }

    fn apply_subgroup(
        &mut self,
        phase: &str,
        key: Birth,
        event: &Value,
        size: usize,
    ) -> io::Result<()> {
        let State::Bound(root, _) = &self.state else {
            return Err(bad());
        };
        if key == *root {
            return Err(bad());
        }
        let index = self.subgroups.iter().position(|group| group.key == key);
        match (phase, size, index) {
            ("arm_subgroup", 2, None) if self.subgroups.len() < SUBGROUP_MAX => {
                self.subgroups.push(Subgroup {
                    key,
                    captured: None,
                });
            }
            ("bind_subgroup", 3, Some(index)) if self.subgroups[index].captured.is_none() => {
                let captured = RecordedCohort::decode(event.get("captured").ok_or_else(bad)?)
                    .ok_or_else(bad)?;
                if captured.encode() != *event.get("captured").ok_or_else(bad)?
                    || !captured.in_scope(&key.holder.scope)
                    || self.bound(&captured)
                {
                    return Err(bad());
                }
                self.subgroups[index].captured = Some(captured);
            }
            ("cancel_subgroup", 2, Some(index)) if self.subgroups[index].captured.is_none() => {
                self.subgroups.remove(index);
            }
            ("retire_subgroup", 2, Some(index)) if self.subgroups[index].captured.is_some() => {
                self.subgroups.remove(index);
            }
            _ => return Err(bad()),
        }
        Ok(())
    }

    pub(crate) fn arm_subgroup(
        &self,
        root: &RecordedCohort,
        key: &BirthKey,
    ) -> io::Result<(String, Self)> {
        if !self.holder.owner() || !key.is_current() || !self.root_bound(root) {
            return Err(bad());
        }
        if self.subgroups.len() >= SUBGROUP_MAX {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "cohort subgroup parser bound exhausted",
            ));
        }
        self.event("arm_subgroup", key, None)
    }

    pub(crate) fn bind_subgroup(
        &self,
        key: &BirthKey,
        cohort: &RecordedCohort,
    ) -> io::Result<(String, Self)> {
        self.match_subgroup_arm(key)?;
        self.event("bind_subgroup", key, Some(cohort))
    }

    fn match_subgroup_arm(&self, key: &BirthKey) -> io::Result<()> {
        if !self.holder.owner()
            || !key.is_current()
            || !self
                .subgroups
                .iter()
                .any(|group| group.key == Birth::of_key(key) && group.captured.is_none())
        {
            return Err(bad());
        }
        Ok(())
    }

    pub(crate) fn cancel_subgroup(&self, key: &BirthKey) -> io::Result<(String, Self)> {
        self.match_subgroup_arm(key)?;
        self.event("cancel_subgroup", key, None)
    }

    pub(crate) fn has_subgroup(&self, key: &BirthKey) -> bool {
        self.subgroups
            .iter()
            .any(|group| group.key == Birth::of_key(key))
    }

    pub(crate) fn subgroup_bound(&self, key: &BirthKey, cohort: &RecordedCohort) -> bool {
        self.subgroups
            .iter()
            .any(|group| group.key == Birth::of_key(key) && group.captured.as_ref() == Some(cohort))
    }

    pub(crate) fn retire_subgroup(
        &self,
        key: &BirthKey,
        cohort: &RecordedCohort,
    ) -> io::Result<(String, Self)> {
        if !self.holder.owner() || !key.is_current() || !self.subgroup_bound(key, cohort) {
            return Err(bad());
        }
        if cohort.lifetime() != Lifetime::Drained {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "subgroup has no conclusive scoped drain",
            ));
        }
        self.event("retire_subgroup", key, None)
    }

    pub(crate) fn require_boot_closure(&self) -> io::Result<(String, Self)> {
        if !self.holder.owner() || self.armed_once || !self.description() {
            return Err(bad());
        }
        let event = aterm_json::json!({"phase": "boot_only"});
        let mut next = self.clone();
        next.apply(&event)?;
        Ok((line(&aterm_json::json!({"cohort": event}))?, next))
    }

    pub(crate) fn arm(&self, key: &BirthKey) -> io::Result<(String, Self)> {
        if !self.holder.owner()
            || !key.is_current()
            || !matches!(self.state, State::Description)
            || Birth::of_key(key).holder != self.holder
        {
            return Err(bad());
        }
        self.event("arm", key, None)
    }

    pub(crate) fn bind(
        &self,
        key: &BirthKey,
        cohort: &RecordedCohort,
    ) -> io::Result<(String, Self)> {
        self.match_arm(key)?;
        self.event("bind", key, Some(cohort))
    }

    pub(crate) fn cancel(&self, key: &BirthKey) -> io::Result<(String, Self)> {
        self.match_arm(key)?;
        self.event("cancel", key, None)
    }

    pub(crate) fn finish(&self) -> io::Result<Option<(String, Self)>> {
        if self.gnu.as_ref().is_some_and(|(_, retired)| !retired) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "original GNU epoch remains pending",
            ));
        }
        match &self.state {
            State::Description => Ok(None),
            State::Bound(key, _) if self.lifetime() == Lifetime::Drained => {
                let event = aterm_json::json!({"phase": "finish", "key": key.encode()});
                let mut next = self.clone();
                next.apply(&event)?;
                Ok(Some((line(&aterm_json::json!({"cohort": event}))?, next)))
            }
            _ => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "cohort custody has no conclusive drain; retain its complete reservation",
            )),
        }
    }

    fn match_arm(&self, key: &BirthKey) -> io::Result<()> {
        if !self.holder.owner()
            || !key.is_current()
            || !matches!(&self.state, State::Armed(armed) if armed == &Birth::of_key(key))
        {
            return Err(bad());
        }
        Ok(())
    }

    fn event(
        &self,
        phase: &str,
        key: &BirthKey,
        cohort: Option<&RecordedCohort>,
    ) -> io::Result<(String, Self)> {
        let mut event = aterm_json::json!({"phase": phase, "key": Birth::of_key(key).encode()});
        if let Some(cohort) = cohort {
            event
                .as_object_mut()
                .ok_or_else(bad)?
                .insert("captured".into(), cohort.encode());
        }
        let mut next = self.clone();
        next.apply(&event)?;
        Ok((line(&aterm_json::json!({"cohort": event}))?, next))
    }

    pub(crate) fn lifetime(&self) -> Lifetime {
        let native = self.native_lifetime();
        if native == Lifetime::Drained && self.gnu.as_ref().is_some_and(|(_, retired)| !retired) {
            return Lifetime::Unknown;
        }
        native
    }

    pub(crate) fn native_lifetime(&self) -> Lifetime {
        self.lifetime_with(self.holder.scope.ended_or_unknown())
    }

    pub(crate) fn gnu_event(&self, phase: &str, id: &str) -> io::Result<(String, Self)> {
        let event = aterm_json::json!({"phase":phase,"id":id});
        let mut next = self.clone();
        next.apply(&event)?;
        Ok((line(&aterm_json::json!({"cohort":event}))?, next))
    }

    pub(crate) fn gnu_matches(&self, id: &str) -> bool {
        self.gnu
            .as_ref()
            .is_some_and(|(original, _)| original == id)
    }

    pub(crate) fn gnu_retired(&self, id: &str) -> bool {
        self.gnu
            .as_ref()
            .is_some_and(|(original, retired)| original == id && *retired)
    }

    fn lifetime_with(&self, scope: Option<Lifetime>) -> Lifetime {
        if let Some(lifetime) = scope {
            return lifetime;
        }
        let root = match &self.state {
            State::Description => Lifetime::Drained,
            State::Armed(_) => Lifetime::Unknown,
            State::Bound(_, _) if self.policy == ClosurePolicy::BootOnly => Lifetime::Unknown,
            State::Bound(_, cohort) => cohort.lifetime(),
        };
        self.subgroups.iter().fold(root, |lifetime, group| {
            match (
                lifetime,
                group
                    .captured
                    .as_ref()
                    .map_or(Lifetime::Unknown, RecordedCohort::lifetime),
            ) {
                (Lifetime::Present, _) | (_, Lifetime::Present) => Lifetime::Present,
                (Lifetime::Unknown, _) | (_, Lifetime::Unknown) => Lifetime::Unknown,
                _ => Lifetime::Drained,
            }
        })
    }

    pub(crate) fn bound(&self, cohort: &RecordedCohort) -> bool {
        self.root_bound(cohort)
            || self
                .subgroups
                .iter()
                .any(|group| group.captured.as_ref() == Some(cohort))
    }

    pub(crate) fn root_bound(&self, cohort: &RecordedCohort) -> bool {
        matches!(&self.state, State::Bound(_, captured) if captured == cohort)
    }

    /// A complete semantic snapshot, covered by the same checkpoint transaction as events.
    /// Only bound campaigns compact; the original root and every unknown ARM survive.
    pub(crate) fn compact(&self, text: &str) -> io::Result<String> {
        self.compact_as(text, self.common_subgroup_scope())
    }

    fn common_subgroup_scope(&self) -> bool {
        let State::Bound(key, _) = &self.state else {
            return false;
        };
        self.subgroups.iter().all(|group| {
            group.key.holder == key.holder
                && group
                    .captured
                    .as_ref()
                    .is_none_or(|captured| captured.in_scope(&key.holder.scope))
        })
    }

    fn compact_as(&self, text: &str, common: bool) -> io::Result<String> {
        let State::Bound(key, root) = &self.state else {
            return Ok(text.to_owned());
        };
        let groups = self.subgroups.iter().map(|group| {
            if common {
                let captured = group.captured.as_ref().map(|captured| {
                    let full = captured.encode();
                    aterm_json::json!({"group": full["captured"]["group"], "leader_serial": full["captured"]["leader_serial"]})
                });
                aterm_json::json!({"nonce": group.key.nonce, "captured": captured})
            } else {
                aterm_json::json!({"key": group.key.encode(), "captured": group.captured.as_ref().map(RecordedCohort::encode)})
            }
        }).collect::<Vec<_>>();
        let policy = match self.policy {
            ClosurePolicy::Group => "group",
            ClosurePolicy::BootOnly => "boot_only",
        };
        let mut event = aterm_json::json!({
            "phase": "snapshot", "v": if self.gnu.is_some() { 3 } else if common { 2 } else { 1 },
            "policy": {"v": 1, "kind": policy},
            "root": {"key": key.encode(), "captured": root.encode()},
            "subgroups": groups,
        });
        if let Some((id, retired)) = &self.gnu {
            event.as_object_mut().ok_or_else(bad)?.insert(
                "gnu".into(),
                aterm_json::json!({"id":id,"retired":retired,"common":common}),
            );
        }
        let mut compact = ordinary_lines(text).ok_or_else(bad)?;
        compact.push_str(&line(&aterm_json::json!({"cohort": event}))?);
        Ok(compact)
    }

    fn read_snapshot(&mut self, event: &Value) -> io::Result<()> {
        let version = event.get("v").and_then(Value::as_u64).ok_or_else(bad)?;
        if event
            .as_object()
            .is_none_or(|map| map.len() != if version == 3 { 6 } else { 5 })
            || !matches!(version, 1..=3)
        {
            return Err(bad());
        }
        let common = if version == 3 {
            let gnu = event.get("gnu").ok_or_else(bad)?;
            if gnu.as_object().is_none_or(|map| map.len() != 3) {
                return Err(bad());
            }
            let id = gnu.get("id").and_then(Value::as_str).ok_or_else(bad)?;
            self.apply(&aterm_json::json!({"phase":"gnu_prepare","id":id}))?;
            if gnu
                .get("retired")
                .and_then(Value::as_bool)
                .ok_or_else(bad)?
            {
                self.apply(&aterm_json::json!({"phase":"gnu_retire","id":id}))?;
            }
            gnu.get("common").and_then(Value::as_bool).ok_or_else(bad)?
        } else {
            version == 2
        };
        let policy = ClosurePolicy::decode(event.get("policy").ok_or_else(bad)?).ok_or_else(bad)?;
        if self.policy == ClosurePolicy::BootOnly && policy != ClosurePolicy::BootOnly {
            return Err(bad());
        }
        let root = event.get("root").ok_or_else(bad)?;
        if root.as_object().is_none_or(|map| map.len() != 2) {
            return Err(bad());
        }
        let key = root.get("key").ok_or_else(bad)?;
        let original = Birth::decode(key)
            .filter(|birth| birth.encode() == *key)
            .ok_or_else(bad)?;
        let captured = root.get("captured").ok_or_else(bad)?;
        if RecordedCohort::decode(captured).is_none_or(|root| root.encode() != *captured) {
            return Err(bad());
        }
        self.apply(&aterm_json::json!({"phase": "arm", "key": key}))?;
        self.apply(&aterm_json::json!({"phase": "bind", "key": key, "captured": root.get("captured").ok_or_else(bad)?}))?;
        self.policy = policy;
        let groups = event
            .get("subgroups")
            .and_then(Value::as_array)
            .ok_or_else(bad)?;
        if groups.len() > SUBGROUP_MAX {
            return Err(bad());
        }
        for group in groups {
            if group.as_object().is_none_or(|map| map.len() != 2) {
                return Err(bad());
            }
            let mut captured = group.get("captured").ok_or_else(bad)?.clone();
            let key = if !common {
                group.get("key").ok_or_else(bad)?.clone()
            } else {
                let nonce = group.get("nonce").and_then(Value::as_str).ok_or_else(bad)?;
                if !matches!(captured, Value::Null) {
                    if captured.as_object().is_none_or(|map| map.len() != 2) {
                        return Err(bad());
                    }
                    captured = aterm_json::json!({"v": 1, "captured": {
                        "scope": original.holder.scope.encode(),
                        "group": captured.get("group").ok_or_else(bad)?,
                        "leader_serial": captured.get("leader_serial").ok_or_else(bad)?,
                    }});
                }
                aterm_json::json!({"nonce": nonce, "holder": original.holder.encode()})
            };
            self.apply(&aterm_json::json!({"phase": "arm_subgroup", "key": key}))?;
            if !matches!(captured, Value::Null) {
                self.apply(&aterm_json::json!({"phase": "bind_subgroup", "key": key, "captured": captured}))?;
            }
        }
        Ok(())
    }

    /// Cold recovery may accept a compacted pending checkpoint only when no semantic
    /// custody or accounting changed. Retirement itself is a preceding committed event.
    pub(crate) fn known_subgroup_compaction(before: &str, after: &str) -> bool {
        let Some(namespace) = before
            .lines()
            .next()
            .and_then(|head| aterm_json::from_str::<Value>(head).ok())
            .and_then(|head| head.get("namespace")?.as_str().map(str::to_owned))
        else {
            return false;
        };
        let (Ok((_, old)), Ok((_, new))) = (
            Self::decode(before, true, &namespace),
            Self::decode(after, true, &namespace),
        ) else {
            return false;
        };
        old == new
            && (old.compact(before).is_ok_and(|compact| compact == after)
                || old
                    .compact_as(before, false)
                    .is_ok_and(|compact| compact == after))
    }
}

fn ordinary_lines(text: &str) -> Option<String> {
    if text.len() as u64 > crate::ledger::RECORD_MAX || !text.ends_with('\n') {
        return None;
    }
    let mut ordinary = String::new();
    for row in text.split_inclusive('\n') {
        let value = aterm_json::from_str::<Value>(row).ok()?;
        if value.get("cohort").is_none() {
            ordinary.push_str(row);
        }
    }
    Some(ordinary)
}

fn line(value: &Value) -> io::Result<String> {
    let mut text = aterm_json::to_string(value).map_err(|_| bad())?;
    text.push('\n');
    Ok(text)
}

fn strict_head(head: &Value) -> bool {
    if !head
        .get("checkout")
        .and_then(Value::as_str)
        .is_some_and(|s| crate::ledger::checkout_exact(s).is_ok())
    {
        return false;
    }
    let Some(map) = head.as_object() else {
        return false;
    };
    if head.get("checkout_complete").and_then(Value::as_bool) != Some(true) {
        return false;
    }
    let cohort_owned = head.get("v").and_then(Value::as_u64) == Some(3);
    if cohort_owned {
        if head.get("reservation_law").and_then(Value::as_str) != Some("cohort_owned") {
            return false;
        }
    } else if head.get("reservation_law").is_some() {
        return false;
    }
    let known = [
        "checkout_complete",
        "v",
        "namespace",
        "owner",
        "closure_policy",
        "reservation_law",
        "pid",
        "start_ms",
        "seq",
        "checkout",
        "what",
        "mem",
        "margin",
        "cores",
        "rank",
        "disk",
        "quiet",
        "quiet_policy",
        "volume",
        "alone",
        "covers",
        "patience",
    ];
    if map.keys().any(|key| !known.contains(&key.as_str())) {
        return false;
    }
    for key in ["pid", "start_ms", "seq", "mem", "margin", "cores", "disk"] {
        if head.get(key).and_then(Value::as_u64).is_none() {
            return false;
        }
    }
    if !head
        .get("cores")
        .and_then(Value::as_u64)
        .is_some_and(|n| u32::try_from(n).is_ok())
    {
        return false;
    }
    for key in ["checkout", "what"] {
        if !head
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|s| s.len() <= 512)
        {
            return false;
        }
    }
    if !matches!(head.get("volume"), Some(Value::Null))
        && !head
            .get("volume")
            .and_then(Value::as_str)
            .is_some_and(|s| s.len() <= 512)
    {
        return false;
    }
    if !matches!(head.get("quiet"), Some(Value::Null))
        && !head
            .get("quiet")
            .and_then(Value::as_f64)
            .is_some_and(|n| n.is_finite() && n >= 0.0)
    {
        return false;
    }
    for key in ["alone", "covers"] {
        if head.get(key).is_some_and(|v| v.as_bool().is_none()) {
            return false;
        }
    }
    if let Some(patience) = head.get("patience")
        && (patience.as_object().is_none_or(|p| p.len() != 3)
            || ["reserve_ms", "start_ms", "hold_ms"]
                .into_iter()
                .any(|key| patience.get(key).and_then(Value::as_u64).is_none()))
    {
        return false;
    }
    true
}

#[cfg(test)]
#[path = "cohort_policy_tests.rs"]
mod policy_tests;

#[cfg(test)]
mod compact_snapshot_tests {
    use super::*;
    use crate::ledger::CohortFixture;

    fn fixture(count: usize) -> (String, Journal) {
        let fixture = CohortFixture::new();
        let claim = fixture.claim();
        let text = std::fs::read_to_string(claim.path()).unwrap();
        let (head, tail) = text.split_once('\n').unwrap();
        let scope = Scope::decode(&aterm_json::json!({
            "host": "linux:ffffffffffffffffffffffffffffffff",
            "boot": "ffffffffffffffffffffffffffffffff",
            "namespace": u64::MAX.to_string(),
        }))
        .unwrap();
        let holder = Holder {
            identity: Identity::parse_shaped("2147483647@18446744073709551615", false).unwrap(),
            scope,
        };
        let observation = |index: usize| {
            RecordedCohort::decode(&aterm_json::json!({
                "v": 1, "captured": {"scope": holder.scope.encode(),
                    "group": i32::MAX as u64 - index as u64,
                    "leader_serial": u64::MAX - index as u64,
                }
            }))
            .unwrap()
        };
        let birth = |index: usize| Birth {
            nonce: format!("{:032x}", u128::MAX - index as u128),
            holder: holder.clone(),
        };
        let mut head: Value = aterm_json::from_str(head).unwrap();
        let map = head.as_object_mut().unwrap();
        map.insert("owner".into(), holder.encode());
        map.insert("pid".into(), Value::from(i32::MAX as u64));
        // Native Unix path components may contain U+0001 (but not NUL or slash).
        // Two 255-byte components give the full 512-byte allowance and worst JSON escaping.
        let path = format!("/{}/{}", "\u{1}".repeat(255), "\u{1}".repeat(255));
        map.insert("what".into(), Value::from("\u{1}".repeat(512)));
        map.insert("checkout".into(), Value::from(path.clone()));
        map.insert("volume".into(), Value::from(path));
        for field in ["mem", "margin", "disk", "start_ms", "seq"] {
            map.insert(field.into(), Value::from(u64::MAX));
        }
        map.insert("cores".into(), Value::from(u32::MAX));
        map.insert("rank".into(), Value::from("background"));
        map.insert("quiet".into(), Value::from(f64::MAX));
        map.insert("alone".into(), Value::from(true));
        map.insert("covers".into(), Value::from(true));
        map.insert(
            "patience".into(),
            aterm_json::json!({"reserve_ms": u64::MAX, "start_ms": u64::MAX, "hold_ms": u64::MAX}),
        );
        let _ = tail;
        let base = format!(
            "{}\n{}\n",
            aterm_json::to_string(&head).unwrap(),
            aterm_json::json!({"admitted_ms": u64::MAX, "cores": u32::MAX, "alone": true})
        );
        let journal = Journal {
            holder: holder.clone(),
            state: State::Bound(birth(0), observation(0)),
            policy: ClosurePolicy::BootOnly,
            cohort_owned: false,
            armed_once: true,
            gnu: None,
            subgroups: (1..=count)
                .map(|index| Subgroup {
                    key: birth(index),
                    captured: Some(observation(index)),
                })
                .collect(),
        };
        (base, journal)
    }

    fn namespace(text: &str) -> String {
        let head: Value = aterm_json::from_str(text.lines().next().unwrap()).unwrap();
        head["namespace"].as_str().unwrap().to_owned()
    }

    #[test]
    fn cohort_owned_header_survives_compaction_without_law_downgrade() {
        let (base, mut journal) = fixture(2);
        let (head, tail) = base.split_once('\n').unwrap();
        let mut head: Value = aterm_json::from_str(head).unwrap();
        head.as_object_mut()
            .unwrap()
            .insert("v".into(), Value::from(3u64));
        head.as_object_mut()
            .unwrap()
            .insert("reservation_law".into(), Value::from("cohort_owned"));
        let base = format!("{}{tail}", line(&head).unwrap());
        journal.cohort_owned = true;
        let old = journal.compact_as(&base, false).unwrap();
        let compact = journal.compact(&old).unwrap();
        let namespace = namespace(&base);
        let (record, decoded) = Journal::decode(&compact, true, &namespace).unwrap();
        assert_eq!(record.accounting, crate::Accounting::CohortOwned);
        assert_eq!(decoded, journal);
        assert!(Journal::known_subgroup_compaction(&old, &compact));
        let downgraded = compact
            .replacen("\"v\":3", "\"v\":2", 1)
            .replace("\"reservation_law\":\"cohort_owned\",", "");
        assert!(Journal::decode(&downgraded, true, &namespace).is_ok());
        assert!(!Journal::known_subgroup_compaction(&old, &downgraded));
    }

    fn split_snapshot(text: &str) -> (String, Value) {
        let rows = text.split_inclusive('\n').collect::<Vec<_>>();
        (
            rows[..rows.len() - 1].concat(),
            aterm_json::from_str(rows.last().unwrap()).unwrap(),
        )
    }

    fn event(snapshot: &mut Value) -> &mut Value {
        snapshot.as_object_mut().unwrap().get_mut("cohort").unwrap()
    }

    fn groups(snapshot: &mut Value) -> &mut Vec<Value> {
        event(snapshot)
            .as_object_mut()
            .unwrap()
            .get_mut("subgroups")
            .unwrap()
            .as_array_mut()
            .unwrap()
    }

    #[test]
    fn compact_snapshot_v2_roundtrips_28_max_width_observations_within_existing_bytes() {
        let (base, journal) = fixture(28);
        let compact = journal.compact(&base).unwrap();
        let v1 = journal.compact_as(&base, false).unwrap();
        let (_, mut snapshot) = split_snapshot(&compact);
        assert_eq!(snapshot["cohort"]["v"].as_u64(), Some(2));
        assert_eq!(groups(&mut snapshot).len(), 28);
        assert!(compact.len() + 2048 < crate::ledger::RECORD_MAX as usize);
        assert!(compact.len() < v1.len());
        let (_, decoded) = Journal::decode(&compact, true, &namespace(&base)).unwrap();
        assert_eq!(
            decoded, journal,
            "all nonces, groups, serials, scope and policy survive"
        );
        eprintln!(
            "28 max-width bound subgroups: v2={} bytes, v1={} bytes, record cap={}",
            compact.len(),
            v1.len(),
            crate::ledger::RECORD_MAX
        );
        let limit = crate::ledger::RECORD_MAX as usize;
        let padded = format!(
            "{}{}\n",
            compact.trim_end_matches('\n'),
            " ".repeat(limit - compact.len())
        );
        assert_eq!(padded.len(), limit);
        assert!(Journal::decode(&padded, true, &namespace(&base)).is_ok());
        let oversized = format!("{} \n", padded.trim_end_matches('\n'));
        assert_eq!(oversized.len(), limit + 1);
        assert!(Journal::decode(&oversized, true, &namespace(&base)).is_err());
    }

    #[test]
    fn compact_snapshot_v2_factors_only_exact_common_holder_and_scope() {
        let (base, mut journal) = fixture(2);
        journal.subgroups[0].key.holder.identity.pid -= 1;
        let text = journal.compact(&base).unwrap();
        assert_eq!(text, journal.compact_as(&base, false).unwrap());
        let (_, snapshot) = split_snapshot(&text);
        assert_eq!(snapshot["cohort"]["v"].as_u64(), Some(1));
        assert_eq!(
            snapshot["cohort"]["subgroups"][0]["key"],
            journal.subgroups[0].key.encode()
        );
        assert!(Journal::decode(&text, true, &namespace(&base)).is_err());

        let (base, mut journal) = fixture(2);
        let mut capture = journal.subgroups[0].captured.as_ref().unwrap().encode();
        capture.as_object_mut().unwrap().get_mut("captured").unwrap().as_object_mut().unwrap().insert("scope".into(), aterm_json::json!({
            "host": "linux:ffffffffffffffffffffffffffffffff", "boot": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "namespace": u64::MAX.to_string(),
        }));
        journal.subgroups[0].captured = Some(RecordedCohort::decode(&capture).unwrap());
        let text = journal.compact(&base).unwrap();
        assert_eq!(text, journal.compact_as(&base, false).unwrap());
        let (_, snapshot) = split_snapshot(&text);
        assert_eq!(snapshot["cohort"]["subgroups"][0]["captured"], capture);
        assert!(Journal::decode(&text, true, &namespace(&base)).is_err());
    }

    #[test]
    fn compact_snapshot_v2_rejects_incomplete_common_data_and_malformed_rows() {
        let (base, mut journal) = fixture(2);
        journal.subgroups[1].captured = None;
        let text = journal.compact(&base).unwrap();
        assert_eq!(
            Journal::decode(&text, true, &namespace(&base)).unwrap().1,
            journal
        );
        let (prefix, snapshot) = split_snapshot(&text);
        let refuses = |snapshot: &Value| {
            let altered = format!("{prefix}{}\n", aterm_json::to_string(snapshot).unwrap());
            assert!(Journal::decode(&altered, true, &namespace(&base)).is_err());
        };
        for path in [
            vec!["root"],
            vec!["policy"],
            vec!["subgroups"],
            vec!["root", "key", "holder"],
            vec!["root", "key", "holder", "scope"],
            vec!["root", "key", "holder", "scope", "host"],
            vec!["root", "captured", "captured", "scope", "boot"],
        ] {
            let mut altered = snapshot.clone();
            let mut value = event(&mut altered);
            for key in &path[..path.len() - 1] {
                value = value.as_object_mut().unwrap().get_mut(*key).unwrap();
            }
            value.as_object_mut().unwrap().remove(*path.last().unwrap());
            refuses(&altered);
        }
        let mut altered = snapshot.clone();
        event(&mut altered)
            .as_object_mut()
            .unwrap()
            .get_mut("root")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .get_mut("key")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .get_mut("holder")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(
                "identity".into(),
                Value::from("2147483646@18446744073709551615"),
            );
        refuses(&altered);
        for field in ["group", "leader_serial"] {
            let mut altered = snapshot.clone();
            groups(&mut altered)[0]
                .as_object_mut()
                .unwrap()
                .get_mut("captured")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(field);
            refuses(&altered);
        }
        for group in [
            Value::from(0u64),
            Value::from(1u64),
            Value::from(i32::MAX as u64 + 1),
            Value::Null,
        ] {
            let mut altered = snapshot.clone();
            groups(&mut altered)[0]
                .as_object_mut()
                .unwrap()
                .get_mut("captured")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("group".into(), group);
            refuses(&altered);
        }
        for nonce in [
            "bad".to_owned(),
            "F".repeat(NONCE_BYTES),
            journal.subgroups[0].key.nonce.clone(),
            match &journal.state {
                State::Bound(key, _) => key.nonce.clone(),
                _ => unreachable!(),
            },
        ] {
            let mut altered = snapshot.clone();
            groups(&mut altered)[1]
                .as_object_mut()
                .unwrap()
                .insert("nonce".into(), Value::from(nonce));
            refuses(&altered);
        }
        let mut altered = snapshot.clone();
        groups(&mut altered)[0]
            .as_object_mut()
            .unwrap()
            .insert("scope".into(), Value::Null);
        refuses(&altered);
        let mut altered = snapshot.clone();
        groups(&mut altered)[0]
            .as_object_mut()
            .unwrap()
            .get_mut("captured")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("scope".into(), Value::Null);
        refuses(&altered);
    }

    #[test]
    fn compact_snapshot_v2_compaction_preserves_semantics_across_both_formats() {
        let (base, journal) = fixture(2);
        let v1 = journal.compact_as(&base, false).unwrap();
        let v2 = journal.compact(&base).unwrap();
        assert!(Journal::known_subgroup_compaction(&v1, &v2));
        assert!(
            Journal::known_subgroup_compaction(&v2, &v1),
            "old pending v1 snapshots still recover"
        );
        let (prefix, mut altered) = split_snapshot(&v2);
        groups(&mut altered)[0]
            .as_object_mut()
            .unwrap()
            .get_mut("captured")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("leader_serial".into(), Value::from(0u64));
        let altered = format!("{prefix}{}\n", aterm_json::to_string(&altered).unwrap());
        assert!(Journal::decode(&altered, true, &namespace(&base)).is_ok());
        assert!(!Journal::known_subgroup_compaction(&v1, &altered));
        let accounting = v2.replacen(
            "\"mem\":18446744073709551615",
            "\"mem\":18446744073709551614",
            1,
        );
        assert!(Journal::decode(&accounting, true, &namespace(&base)).is_ok());
        assert!(!Journal::known_subgroup_compaction(&v1, &accounting));
    }
}
