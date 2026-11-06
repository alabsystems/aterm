// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only custody through the shared native ancestor/image/peer reader.
//! A locator or unvalidated program's JSON establishes no resource authority.
use aterm_ctl::resource_admission_transport as wire;
use aterm_json::Value;
use aterm_types::resource_inspection::{BirthKind, Cpu, Inspection, Manifest, Memory, Subject};
use std::{
    io,
    path::Path,
    time::{Duration, Instant},
};
use trust_resource_provenance::{Host, Store as ProvenanceStore};

const MAX_REPLY: usize = 16 * 1024;
fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid native custody observation",
    )
}
/// Read the actual ancestor custodian without creating or repairing account state.
/// # Errors
/// No unique native ancestor/cached image exists, provenance or peer checks fail,
/// the endpoint is unavailable, or its bounded custody response is malformed.
pub fn lookup(session: &str) -> io::Result<Value> {
    lookup_at(&ProvenanceStore::account_path()?, session)
}
fn lookup_at(root: &Path, session: &str) -> io::Result<Value> {
    canonical_session(session)?;
    let deadline = Instant::now() + Duration::from_secs(4);
    let host = ProvenanceStore::open_at(root)?.connect_ancestor(deadline)?;
    query(&host, session, deadline)
}
fn query(host: &Host, session: &str, deadline: Instant) -> io::Result<Value> {
    host.revalidate(deadline)?;
    let stream = host.stream();
    let request =
        aterm_json::to_vec(&aterm_json::json!({"version":3,"role":"inspect","session":session}))
            .map_err(io::Error::other)?;
    wire::write_frame(stream, &request, deadline)?;
    let bytes = read_reply(stream, deadline)?;
    let value: Value = aterm_json::from_slice(&bytes).map_err(|_| invalid())?;
    validate(&value, session)?;
    host.revalidate(deadline)?;
    // Keep the original checked connection alive through the native-peer reread
    // and ACK. This exchange observes custody; it grants no admission.
    wire::write_frame(stream, &[1], deadline)?;
    Ok(value)
}
fn read_reply(mut stream: &aterm_uds::CtlStream, deadline: Instant) -> io::Result<Vec<u8>> {
    use std::{io::Read as _, os::fd::AsFd as _};
    stream.set_nonblocking(true)?;
    let mut exact = |mut bytes: &mut [u8]| -> io::Result<()> {
        while !bytes.is_empty() {
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            match stream.read(bytes) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => bytes = &mut bytes[n..],
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    aterm_uds::ready::until(
                        stream.as_fd(),
                        aterm_uds::ready::Interest::Read,
                        deadline,
                    )?;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    };
    let mut header = [0; 4];
    exact(&mut header)?;
    let size = u32::from_be_bytes(header) as usize;
    if size == 0 || size > MAX_REPLY {
        return Err(invalid());
    }
    let mut bytes = vec![0; size];
    exact(&mut bytes)?;
    Ok(bytes)
}
fn canonical_session(session: &str) -> io::Result<()> {
    let sid = session.strip_prefix("s-").ok_or_else(invalid)?;
    if sid.is_empty() || sid.len() > 62 || !aterm_codec::hex::is_lower(sid, sid.len()) {
        return Err(invalid());
    }
    Ok(())
}
fn object(value: &Value, fields: &[&str]) -> io::Result<()> {
    let map = value.as_object().ok_or_else(invalid)?;
    if map.len() != fields.len() || fields.iter().any(|field| !map.contains_key(*field)) {
        return Err(invalid());
    }
    Ok(())
}
fn text(value: &Value) -> io::Result<String> {
    value.as_str().map(str::to_owned).ok_or_else(invalid)
}
fn number(value: &Value) -> io::Result<u64> {
    value.as_u64().ok_or_else(invalid)
}
fn manifest(value: &Value) -> io::Result<Manifest> {
    object(value, &["device", "inode"])?;
    Ok(Manifest {
        device: number(&value["device"])?,
        inode: number(&value["inode"])?,
    })
}
fn validate(value: &Value, session: &str) -> io::Result<()> {
    object(
        value,
        &[
            "version",
            "ready",
            "session",
            "attempt",
            "host",
            "boot",
            "root_pid",
            "root_birth",
            "root_birth_kind",
            "subject",
            "cpu",
            "memory",
        ],
    )?;
    if value["version"].as_u64() != Some(3)
        || value["ready"].as_bool() != Some(true)
        || value["session"].as_str() != Some(session)
    {
        return Err(invalid());
    }
    let attempt = value["attempt"]
        .as_str()
        .filter(|text| aterm_codec::hex::is_lower(text, 32))
        .ok_or_else(invalid)?;
    let mut attempt_bytes = [0; 16];
    if !aterm_codec::hex::decode_into(attempt.as_bytes(), &mut attempt_bytes) {
        return Err(invalid());
    }
    let cpu = &value["cpu"];
    let native_manifest = manifest(&cpu["native_manifest"])?;
    let cpu = match cpu["mode"].as_str() {
        Some("retained_fixed") => {
            object(cpu, &["mode", "units", "native_manifest"])?;
            Cpu::RetainedFixed {
                units: u32::try_from(number(&cpu["units"])?).map_err(|_| invalid())?,
                native_manifest,
            }
        }
        Some("cooperative_shared") => {
            object(cpu, &["mode", "ceiling", "native_manifest"])?;
            Cpu::CooperativeShared {
                ceiling: u32::try_from(number(&cpu["ceiling"])?).map_err(|_| invalid())?,
                native_manifest,
            }
        }
        _ => return Err(invalid()),
    };
    let memory = &value["memory"];
    object(memory, &["bytes", "claim"])?;
    let claim = &memory["claim"];
    object(claim, &["v", "namespace", "marker", "epoch", "guard"])?;
    if claim["v"].as_u64() != Some(3) {
        return Err(invalid());
    }
    let guard = manifest(&claim["guard"])?;
    let root_birth_kind = match value["root_birth_kind"].as_str() {
        Some("macos_wall_microseconds") if cfg!(target_os = "macos") => {
            BirthKind::MacosWallMicroseconds
        }
        Some("linux_start_ticks") if cfg!(target_os = "linux") => BirthKind::LinuxStartTicks,
        _ => return Err(invalid()),
    };
    let subject = &value["subject"];
    let pid = u32::try_from(number(&subject["pid"])?).map_err(|_| invalid())?;
    let birth = number(&subject["birth"])?;
    let subject = match subject["mode"].as_str() {
        Some("root") => {
            object(subject, &["mode", "pid", "birth"])?;
            Subject::Root { pid, birth }
        }
        Some("registered_member") => {
            object(subject, &["mode", "pid", "birth", "attempt"])?;
            let encoded = subject["attempt"]
                .as_str()
                .filter(|text| aterm_codec::hex::is_lower(text, 32))
                .ok_or_else(invalid)?;
            let mut attempt = [0; 16];
            if !aterm_codec::hex::decode_into(encoded.as_bytes(), &mut attempt) {
                return Err(invalid());
            }
            Subject::RegisteredMember {
                pid,
                birth,
                attempt,
            }
        }
        _ => return Err(invalid()),
    };
    let inspection = Inspection {
        session: text(&value["session"])?,
        attempt: attempt_bytes,
        host: text(&value["host"])?,
        boot: text(&value["boot"])?,
        root_pid: u32::try_from(number(&value["root_pid"])?).map_err(|_| invalid())?,
        root_birth: number(&value["root_birth"])?,
        root_birth_kind,
        subject,
        cpu,
        memory: Memory {
            bytes: number(&memory["bytes"])?,
            namespace: text(&claim["namespace"])?,
            marker: text(&claim["marker"])?,
            epoch: text(&claim["epoch"])?,
            guard_device: guard.device,
            guard_inode: guard.inode,
        },
    };
    inspection.json().map(|_| ())
}

#[cfg(test)]
#[path = "inspection/tests.rs"]
mod tests;
