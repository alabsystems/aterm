// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Saved sweep observations only; no process lookup, custody or cleanup operation.

use serde::de::{Deserialize, Deserializer, Error as _, MapAccess, Visitor};

pub(super) const ROW_CAP: usize = 64;
const IDENTITY_BYTES: usize = 256;
const STAGE_BYTES: usize = 4096;
pub(super) const SCOPE: &str =
    "saved sweep observations only; no current liveness, custody or cleanup authority";

/// Preserve JSON unsigned integers without accepting floats or signed spellings of zero.
struct Unsigned(u64);

impl<'de> Deserialize<'de> for Unsigned {
    fn deserialize<D: Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        struct Integer;
        impl Visitor<'_> for Integer {
            type Value = Unsigned;
            fn expecting(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                out.write_str("an unsigned JSON integer")
            }
            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(Unsigned(value))
            }
        }
        decoder.deserialize_any(Integer)
    }
}

struct Version(i32);

impl<'de> Deserialize<'de> for Version {
    fn deserialize<D: Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        struct Integer;
        impl Visitor<'_> for Integer {
            type Value = Version;
            fn expecting(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                out.write_str("a JSON integer within i32")
            }
            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
                i32::try_from(value).map(Version).map_err(E::custom)
            }
            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
                i32::try_from(value).map(Version).map_err(E::custom)
            }
        }
        decoder.deserialize_any(Integer)
    }
}

fn once<T, E: serde::de::Error>(field: &mut Option<T>, value: T) -> Result<(), E> {
    if field.is_some() {
        return Err(E::custom("duplicate saved process observation field"));
    }
    *field = Some(value);
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Lineage {
    birth: u64,
    idversion: Option<i32>,
    uniqueid: u64,
}

impl<'de> Deserialize<'de> for Lineage {
    fn deserialize<D: Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        struct Object;
        impl<'de> Visitor<'de> for Object {
            type Value = Lineage;
            fn expecting(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                out.write_str("a saved lineage object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut input: A) -> Result<Self::Value, A::Error> {
                let mut birth = None;
                let mut idversion = None;
                let mut uniqueid = None;
                while let Some(key) = input.next_key::<String>()? {
                    match key.as_str() {
                        "birth" => {
                            once::<_, A::Error>(&mut birth, input.next_value::<Unsigned>()?)?
                        }
                        "idversion" => once::<_, A::Error>(
                            &mut idversion,
                            input.next_value::<Option<Version>>()?,
                        )?,
                        "uniqueid" => {
                            once::<_, A::Error>(&mut uniqueid, input.next_value::<Unsigned>()?)?
                        }
                        _ => {
                            input.next_value::<serde::de::IgnoredAny>()?;
                        }
                    }
                }
                Ok(Lineage {
                    birth: birth.ok_or_else(|| A::Error::missing_field("birth"))?.0,
                    idversion: idversion
                        .ok_or_else(|| A::Error::missing_field("idversion"))?
                        .map(|v| v.0),
                    uniqueid: uniqueid
                        .ok_or_else(|| A::Error::missing_field("uniqueid"))?
                        .0,
                })
            }
        }
        // deserialize_map refuses arrays, unlike a derived struct's sequence form.
        decoder.deserialize_map(Object)
    }
}

#[derive(Default)]
struct Wire {
    schema: Option<Unsigned>,
    event: Option<String>,
    run_id: Option<String>,
    boot_id: Option<String>,
    pid: Option<Unsigned>,
    lineage: Option<Option<Lineage>>,
    observed_ms: Option<Unsigned>,
    scope: Option<String>,
    stage: Option<Option<String>>,
    group: Option<Option<Unsigned>>,
    reason: Option<String>,
}

impl<'de> Deserialize<'de> for Wire {
    fn deserialize<D: Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        struct Object;
        impl<'de> Visitor<'de> for Object {
            type Value = Wire;
            fn expecting(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                out.write_str("a saved process survivor event object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut input: A) -> Result<Self::Value, A::Error> {
                let mut wire = Wire::default();
                while let Some(key) = input.next_key::<String>()? {
                    match key.as_str() {
                        "schema" => once::<_, A::Error>(&mut wire.schema, input.next_value()?)?,
                        "event" => once::<_, A::Error>(&mut wire.event, input.next_value()?)?,
                        "run_id" => once::<_, A::Error>(&mut wire.run_id, input.next_value()?)?,
                        "boot_id" => once::<_, A::Error>(&mut wire.boot_id, input.next_value()?)?,
                        "pid" => once::<_, A::Error>(&mut wire.pid, input.next_value()?)?,
                        "lineage" => once::<_, A::Error>(&mut wire.lineage, input.next_value()?)?,
                        "observed_ms" => {
                            once::<_, A::Error>(&mut wire.observed_ms, input.next_value()?)?
                        }
                        "scope" => once::<_, A::Error>(&mut wire.scope, input.next_value()?)?,
                        "stage" => once::<_, A::Error>(&mut wire.stage, input.next_value()?)?,
                        "group" => once::<_, A::Error>(&mut wire.group, input.next_value()?)?,
                        "reason" => once::<_, A::Error>(&mut wire.reason, input.next_value()?)?,
                        _ => {
                            input.next_value::<serde::de::IgnoredAny>()?;
                        }
                    }
                }
                Ok(wire)
            }
        }
        decoder.deserialize_map(Object)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Scope {
    Run,
    Stage { stage: String, group: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Observation {
    invocation_id: Option<String>,
    run_id: String,
    boot_id: String,
    pid: i32,
    lineage: Option<Lineage>,
    observed_ms: u64,
    scope: Scope,
    reason: &'static str,
}

impl Observation {
    fn read(json: &str, invocation_id: Option<String>) -> Option<Self> {
        if json.len() > crate::event_log::MAX_RECORD_BYTES {
            return None;
        }
        let wire: Wire = aterm_json::from_str(json).ok()?;
        if wire.schema?.0 != 1 || wire.event?.as_str() != "process-survivor" {
            return None;
        }
        let run_id = wire.run_id?;
        let run = aterm_uds::runs::RunId::parse(&run_id)?;
        let boot_id = wire.boot_id?;
        if run.pid <= 0
            || run_id.len() > IDENTITY_BYTES
            || boot_id.is_empty()
            || boot_id.len() > IDENTITY_BYTES
        {
            return None;
        }
        let pid = i32::try_from(wire.pid?.0).ok().filter(|pid| *pid > 0)?;
        let stage = wire.stage?;
        let group = wire.group?;
        let scope = match (wire.scope?.as_str(), stage, group) {
            ("run", None, None) => Scope::Run,
            ("stage", Some(stage), Some(group)) if stage.len() <= STAGE_BYTES => Scope::Stage {
                stage,
                group: u32::try_from(group.0).ok().filter(|group| *group > 1)?,
            },
            _ => return None,
        };
        let reason = match wire.reason?.as_str() {
            "custody-not-proven" => "custody-not-proven",
            "identity-unreadable" => "identity-unreadable",
            "end-refused-stale" => "end-refused-stale",
            "end-refused-not-proven" => "end-refused-not-proven",
            "end-refused-signal-failed" => "end-refused-signal-failed",
            "exit-unconfirmed-after-end" => "exit-unconfirmed-after-end",
            "execution-unobserved" => "execution-unobserved",
            _ => return None,
        };
        Some(Self {
            invocation_id,
            run_id,
            boot_id,
            pid,
            lineage: wire.lineage?,
            observed_ms: wire.observed_ms?.0,
            scope,
            reason,
        })
    }

    fn json(&self) -> aterm_json::Value {
        let (scope, stage, group) = match &self.scope {
            Scope::Run => ("run", None, None),
            Scope::Stage { stage, group } => ("stage", Some(stage.as_str()), Some(*group)),
        };
        aterm_json::json!({
            "invocation_id":self.invocation_id,"run_id":self.run_id,"boot_id":self.boot_id,"pid":self.pid,
            "lineage":self.lineage.as_ref().map(|lineage| aterm_json::json!({
                "birth":lineage.birth,"idversion":lineage.idversion,"uniqueid":lineage.uniqueid,
            })),
            "observed_ms":self.observed_ms,"scope":scope,"stage":stage,"group":group,"reason":self.reason,
        })
    }
}

/// File-order rows, including repeats. A PID is never used to merge observations.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Rows {
    pub(super) records: usize,
    pub(super) omitted_records: usize,
    rows: Vec<Observation>,
}

impl Rows {
    pub(super) fn observe(&mut self, json: &str, invocation_id: Option<String>) -> bool {
        let Some(row) = Observation::read(json, invocation_id) else {
            return false;
        };
        self.records += 1;
        if self.rows.len() < ROW_CAP {
            self.rows.push(row);
        } else {
            self.omitted_records += 1;
        }
        true
    }

    pub(super) fn json(&self) -> Vec<aterm_json::Value> {
        self.rows.iter().map(Observation::json).collect()
    }

    pub(super) fn render(&self, out: &mut String, prefix: &str) {
        if self.records == 0 {
            return;
        }
        out.push_str(&format!(
            "{prefix}Saved process survivor observations ({SCOPE}):\n"
        ));
        out.push_str(&format!(
            "{prefix}  {} records; row cap {ROW_CAP}; {} records omitted by row cap.\n",
            self.records, self.omitted_records
        ));
        for row in &self.rows {
            // Every producer string is JSON quoted; bounded identity strings stay exact.
            out.push_str(&format!("{prefix}  {}\n", row.json()));
        }
    }
}
