// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Safe provisional compiler metadata, shared by its producer and diagnostic reader.

use aterm_json::Value;

pub(crate) const STAGE_BYTES: usize = 4096;

pub(crate) fn dispatch_mode(mode: &str) -> bool {
    matches!(
        mode,
        "test"
            | "build"
            | "check-test"
            | "check"
            | "doc"
            | "doctest"
            | "docscrape"
            | "run-custom-build"
    )
}

pub(crate) fn compile_kind(kind: &str) -> bool {
    matches!(kind, "host" | "target")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Metadata {
    Dispatch { mode: String, kind: String },
    Artifact { name: String, kinds: Vec<String> },
}

impl Metadata {
    pub(crate) fn dispatch(mode: &str, kind: &str) -> Option<Self> {
        (dispatch_mode(mode) && compile_kind(kind)).then(|| Self::Dispatch {
            mode: mode.into(),
            kind: kind.into(),
        })
    }

    pub(crate) fn artifact(name: &str, kinds: &[&str]) -> Option<Self> {
        if name.is_empty()
            || name.len() > 256
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            || kinds.is_empty()
            || kinds.len() > 16
            || kinds.iter().any(|kind| {
                !matches!(
                    *kind,
                    "lib"
                        | "rlib"
                        | "dylib"
                        | "cdylib"
                        | "staticlib"
                        | "proc-macro"
                        | "bin"
                        | "example"
                        | "test"
                        | "bench"
                        | "custom-build"
                )
            })
        {
            return None;
        }
        Some(Self::Artifact {
            name: name.into(),
            kinds: kinds.iter().map(|kind| (*kind).into()).collect(),
        })
    }

    pub(crate) fn json(&self) -> Value {
        match self {
            Self::Dispatch { mode, kind } => aterm_json::json!({
                "message_kind":"dispatch", "compile_mode":mode, "compile_kind":kind,
                "target_name":null, "target_kinds":null,
            }),
            Self::Artifact { name, kinds } => aterm_json::json!({
                "message_kind":"artifact", "compile_mode":null, "compile_kind":null,
                "target_name":name, "target_kinds":kinds,
            }),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Observation {
    pub(crate) stage: String,
    pub(crate) metadata: Metadata,
}

// Deriving this record rejects duplicate known fields before a general JSON map can erase
// conflicting provisional/sample flags. Extra fields are ignored, never copied to output.
#[derive(serde::Deserialize)]
struct Wire {
    schema: u64,
    event: crate::event_log::EventKind,
    stage: String,
    provisional: bool,
    sample_complete: bool,
    message_kind: String,
    compile_mode: Value,
    compile_kind: Value,
    target_name: Value,
    target_kinds: Value,
}

impl Observation {
    pub(crate) fn new(stage: &str, metadata: Metadata) -> Option<Self> {
        (!stage.is_empty() && stage.len() <= STAGE_BYTES).then(|| Self {
            stage: stage.into(),
            metadata,
        })
    }

    pub(crate) fn read(json: &str) -> Option<Self> {
        let wire: Wire = aterm_json::from_str(json).ok()?;
        if wire.schema != 1
            || wire.event != crate::event_log::EventKind::CompileMessageObserved
            || !wire.provisional
            || wire.sample_complete
        {
            return None;
        }
        let metadata = match wire.message_kind.as_str() {
            "dispatch" if wire.target_name.is_null() && wire.target_kinds.is_null() => {
                Metadata::dispatch(wire.compile_mode.as_str()?, wire.compile_kind.as_str()?)?
            }
            "artifact" if wire.compile_mode.is_null() && wire.compile_kind.is_null() => {
                let kinds: Vec<_> = wire
                    .target_kinds
                    .as_array()?
                    .iter()
                    .map(Value::as_str)
                    .collect::<Option<_>>()?;
                Metadata::artifact(wire.target_name.as_str()?, &kinds)?
            }
            _ => return None,
        };
        Self::new(&wire.stage, metadata)
    }

    pub(crate) fn event(&self) -> String {
        let mut value = self.metadata.json();
        let fields = value.as_object_mut().expect("metadata is an object");
        fields.insert("stage".into(), self.stage.clone().into());
        fields.insert("provisional".into(), true.into());
        fields.insert("sample_complete".into(), false.into());
        let value = crate::event_log::event_value(
            crate::event_log::EventKind::CompileMessageObserved,
            value,
        );
        format!("verify-event: {value}")
    }
}
