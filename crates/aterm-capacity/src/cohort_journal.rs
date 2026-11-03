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
        (nonce.len() == 32
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Journal {
    holder: Holder,
    state: State,
}

impl Journal {
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
        let holder = Holder::capture()?;
        let mut head: Value = aterm_json::from_str(text).map_err(|_| bad())?;
        let map = head.as_object_mut().ok_or_else(bad)?;
        map.insert("v".into(), Value::from(2u64));
        map.insert("namespace".into(), Value::from(namespace));
        map.insert("checkout_complete".into(), Value::from(true));
        map.insert("owner".into(), holder.encode());
        Ok((
            line(&head)?,
            Self {
                holder,
                state: State::Description,
            },
        ))
    }

    /// Never skip a torn or unrecognized tail, including ordinary grant/write/credit lines.
    pub(crate) fn decode(
        text: &str,
        admitted: bool,
        namespace: &str,
    ) -> io::Result<(Record, Self)> {
        if !text.ends_with('\n') {
            return Err(bad());
        }
        let mut values = text
            .lines()
            .map(|line| aterm_json::from_str::<Value>(line).map_err(|_| bad()));
        let mut head = values.next().ok_or_else(bad)??;
        if head.get("v").and_then(Value::as_u64) != Some(2)
            || head.get("namespace").and_then(Value::as_str) != Some(namespace)
        {
            return Err(bad());
        }
        if !strict_head(&head) {
            return Err(bad());
        }
        let holder = Holder::decode(head.get("owner").ok_or_else(bad)?).ok_or_else(bad)?;
        if head.get("pid").and_then(Value::as_u64) != u64::try_from(holder.identity.pid).ok() {
            return Err(bad());
        }
        let mut journal = Self {
            holder,
            state: State::Description,
        };
        head.as_object_mut()
            .ok_or_else(bad)?
            .insert("v".into(), Value::from(1u64));
        let mut ordinary = line(&head)?;
        let mut grants = 0usize;
        for value in values {
            let value = value?;
            let map = value.as_object().ok_or_else(bad)?;
            if let Some(event) = map.get("cohort") {
                if !admitted || grants != 1 || map.len() != 1 {
                    return Err(bad());
                }
                journal.apply(event)?;
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
        if !matches!(journal.state, State::Description) || !journal.holder.current() {
            record.accounting = crate::ledger::Accounting::Whole;
        }
        Ok((record, journal))
    }

    fn apply(&mut self, event: &Value) -> io::Result<()> {
        let phase = event.get("phase").and_then(Value::as_str).ok_or_else(bad)?;
        let key = Birth::decode(event.get("key").ok_or_else(bad)?).ok_or_else(bad)?;
        if key.holder != self.holder {
            return Err(bad());
        }
        let size = event.as_object().ok_or_else(bad)?.len();
        self.state = match (&self.state, phase, size) {
            (State::Description, "arm", 2) => State::Armed(key),
            (State::Armed(armed), "cancel", 2) if armed == &key => State::Description,
            (State::Bound(bound, _), "finish", 2) if bound == &key => State::Description,
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
        Ok(())
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
        if let Some(lifetime) = self.holder.scope.ended_or_unknown() {
            return lifetime;
        }
        match &self.state {
            State::Description => Lifetime::Drained,
            State::Armed(_) => Lifetime::Unknown,
            State::Bound(_, cohort) => cohort.lifetime(),
        }
    }

    pub(crate) fn bound(&self, cohort: &RecordedCohort) -> bool {
        matches!(&self.state, State::Bound(_, captured) if captured == cohort)
    }
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
    let known = [
        "checkout_complete",
        "v",
        "namespace",
        "owner",
        "pid",
        "start_ms",
        "seq",
        "checkout",
        "what",
        "mem",
        "margin",
        "cores",
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
