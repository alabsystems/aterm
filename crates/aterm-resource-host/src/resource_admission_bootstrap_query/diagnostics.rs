// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded observations for the original Root journal. These bytes never
//! construct work closure, SDK custody, a GNU return or a proof verdict.

use super::*;
use aterm_json::{Value, json};

fn digest(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(bytes))
}

fn identity(value: control::Identity) -> Value {
    json!({"pid":value.pid,"birth":value.birth,"birth_kind":format!("{:?}",value.birth_kind)})
}

fn capture(value: &super::super::capture::CaptureObservation) -> Value {
    json!({
        "prefix_bytes":value.prefix.len(),"prefix_sha256":digest(&value.prefix),
        "bytes_seen":value.bytes_seen,"eof":value.eof,"failed":value.failed
    })
}

fn bounded_error(error: impl std::fmt::Display) -> String {
    error.to_string().chars().take(512).collect()
}

impl Retained {
    /// Only a compact observation is persisted; the SAME Retained still owns
    /// complete raw prefixes, original inputs and unabridged errors on Root.
    pub(in crate::actor::owner) fn diagnostics(&self) -> Value {
        let observation = &self.observation;
        let native = &observation.native;
        let runtime_ns = native
            .started_at
            .zip(native.ended)
            .and_then(|(start, end)| end.checked_duration_since(start))
            .and_then(|duration| u64::try_from(duration.as_nanos()).ok());
        let prepared = observation.prepared.as_ref().map(|prepared| {
            json!({
                "request_sha256":aterm_codec::hex::encode_lower(&prepared.request_sha256),
                "inventory_sha256":prepared.inventory.encode().ok().map(|bytes|digest(&bytes)),
                "deadline_passed_at_snapshot":Instant::now() >= prepared.deadline,
                "stdout":capture(&prepared.stdout),"stderr":capture(&prepared.stderr)
            })
        });
        let accounting = native.accounting.map(|accounting| json!({
            "scope":format!("{:?}",accounting.scope),
            "cpu_times":accounting.cpu_times.map(|times|json!({"user_ns":times.user_ns,"system_ns":times.system_ns})),
            "peak_rss_bytes":accounting.peak_rss_bytes
        }));
        let delivery = observation.delivery.as_ref().map(|delivery| json!({
            "write_closed":delivery.write_closed,"acknowledged":delivery.acknowledged,
            "acknowledgement_prefix":aterm_codec::hex::encode_lower(&delivery.acknowledgement_prefix),
            "failed":delivery.failed,"remaining_frames":delivery.remaining_frames,
            "current_frame_sent":delivery.current_frame_sent
        }));
        json!({
            "schema":"bootstrap-query-observation-v1","authority":"observation-only",
            "operation":aterm_codec::hex::encode_lower(&self.operation.request().key.operation),
            "call_attempt":aterm_codec::hex::encode_lower(&self.operation.context().call_attempt),
            "prepared":prepared,
            "identity":native.identity.map(identity),"go_attempted":native.go_attempted,
            "started":native.started.map(|started|json!({"identity":identity(started.identity),"monotonic_ns":started.monotonic_ns,"unix_ns":started.unix_ns})),
            "status_raw":native.status.map(std::os::unix::process::ExitStatusExt::into_raw),
            "accounting":accounting,"runtime_ns":runtime_ns,
            "cause":native.cause.map(|cause|format!("{cause:?}")),
            "inputs_unchanged":observation.inputs_unchanged.as_ref().map(|result|match result {
                Ok(())=>json!({"unchanged":true}),
                Err(error)=>json!({"unchanged":false,"error":bounded_error(error)})
            }),
            "transport":match observation.transport {
                TransportClosure::AcknowledgedAndEof=>"acknowledged-and-eof",
                TransportClosure::FailedShutdown=>"failed-shutdown"
            },
            "delivery":delivery,"failure":observation.failure.as_ref().map(bounded_error)
        })
    }
}
