// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The production signed-service backend, shared by the shipped CLI and dev acceptance lab.
//! This module contains no caller-rejection option or fault injection policy.

use aterm_json::{Value, json};
use aterm_secrets::{
    Error, Result,
    client::{self, Paths},
    service::{self, Job},
    service_repair::{
        self as transaction, Backend, HealthRequirement, Observed, Outcome, Request, Unit,
    },
};
use aterm_update::service_artifact::{Receipt, Store};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Signature domain of the exact canonical activation request.
pub const REQUEST_DOMAIN: &str = "aterm-service-activation/1";
/// Signature domain of an unchanged canonical terminal result.
pub const OUTCOME_DOMAIN: &str = "aterm-service-outcome/1";
/// Maximum serialized signed terminal result accepted by the offline verifier.
pub const MAX_SIGNED_OUTCOME_BYTES: usize = 1024 * 1024;
/// A full request plus the existing request signature envelope.
pub const MAX_EXPECTED_REQUEST_BYTES: usize = transaction::MAX_REQUEST_BYTES + 1024;
/// This application's explicit service protocol.
pub const PROTOCOL: u64 = 1;
fn fail(e: impl std::fmt::Display) -> Error {
    Error::new(e.to_string())
}
/// Permanent content-addressed image store of this login.
pub fn store_root(paths: &Paths) -> PathBuf {
    paths.home.join(".local/share/aterm/service-images")
}
fn now() -> Result<i64> {
    i64::try_from(aterm_time::unix_millis().ok_or_else(|| fail("clock unavailable"))? / 1000)
        .map_err(fail)
}
fn read(path: &Path) -> Result<Vec<u8>> {
    aterm_secrets::fsx::read_owned(path, 16 * 1024 * 1024)?
        .ok_or_else(|| fail(format!("{} is missing", path.display())))
}
fn value(path: &Path) -> Result<Value> {
    aterm_json::from_slice(&read(path)?).map_err(fail)
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn signature(v: &Value) -> Result<[u8; 64]> {
    let s = v["signature"]
        .as_str()
        .ok_or_else(|| fail("missing signature"))?;
    let mut out = [0; 64];
    if !aterm_codec::hex::decode_into(s.as_bytes(), &mut out) {
        return Err(fail("invalid signature encoding"));
    }
    Ok(out)
}
/// Resolve the live fleet-state context used by authority checks.
/// # Errors
/// This process has no configured fleet-state directory.
pub fn state_dir() -> Result<PathBuf> {
    aterm_types::dirs::state_dir().ok_or_else(|| fail("no fleet state directory"))
}
/// Read the existing authenticated fleet identity; never create a new one.
/// # Errors
/// Fleet state or the existing node identity is unavailable.
pub fn identity() -> Result<aterm_net::fleet::NodeIdentity> {
    aterm_net::fleet_member::read_node_identity(&state_dir()?)?
        .ok_or_else(|| fail("service repair requires existing fleet identity"))
}
fn grants() -> Result<Vec<aterm_net::fleet::DriveGrant>> {
    let path =
        aterm_types::dirs::aterm_config_path().ok_or_else(|| fail("no aterm configuration"))?;
    let bytes = aterm_secrets::fsx::read_aterm_config(&path, 1024 * 1024)?
        .ok_or_else(|| fail("no fleet grants"))?;
    let text = std::str::from_utf8(&bytes).map_err(fail)?;
    let table: aterm_toml::Table = aterm_toml::from_str(text).map_err(fail)?;
    if !aterm_secrets::swarm::enabled_in_table(&table)? {
        return Err(fail("sharing is Off"));
    }
    let fleet = table
        .get("fleet")
        .ok_or_else(|| fail("fleet not configured"))?;
    if fleet.get("allow").and_then(aterm_toml::Value::as_str) != Some("drive") {
        return Err(fail("fleet drive is not granted"));
    }
    fleet
        .get("drive_from")
        .and_then(aterm_toml::Value::as_array)
        .ok_or_else(|| fail("fleet drive grants missing"))?
        .iter()
        .map(|v| {
            v.as_str()
                .and_then(aterm_net::fleet::DriveGrant::from_word)
                .ok_or_else(|| fail("invalid fleet drive grant"))
        })
        .collect()
}
fn check_denied(paths: &Paths, node: &str, key: &aterm_net::fleet::NodeKey) -> Result<()> {
    let denied = client::swarm_denied_nodes(paths)?
        .ok_or_else(|| fail("current fleet deny list unavailable"))?;
    if aterm_net::fleet::is_denied_node(node, &denied)
        || aterm_net::fleet::is_denied_key(key, &denied)
    {
        return Err(fail(
            "service repair requester is denied by the current fleet roster",
        ));
    }
    Ok(())
}
/// Native service operations with the same authority and health checks in every caller.
pub struct NativeBackend {
    paths: Paths,
    signed: Option<Value>,
    challenge: Option<String>,
    running_build: u64,
    proof: Option<NativeProof>,
}

/// Immutable evidence produced only after native image, process, challenge and signed health
/// checks succeed, followed by an exact process identity recheck.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeProof {
    job: Job,
    unit: Unit,
    pid: u32,
    process_identity: String,
    challenge: String,
    health: HealthRequirement,
    server_endpoint: Option<String>,
}

impl NativeProof {
    /// Exact service-manager job proved.
    pub fn job(&self) -> Job {
        self.job
    }
    /// Exact complete unit whose executable was proved.
    pub fn unit(&self) -> &Unit {
        &self.unit
    }
    /// PID which produced the challenge response.
    pub fn pid(&self) -> u32 {
        self.pid
    }
    /// Kernel process identity token rechecked after signed service health.
    pub fn process_identity(&self) -> &str {
        &self.process_identity
    }
    /// Fresh challenge answered by the service process.
    pub fn challenge(&self) -> &str {
        &self.challenge
    }
    /// Authenticated vault and minimum membership frontier proved.
    pub fn health(&self) -> &HealthRequirement {
        &self.health
    }
    /// Server endpoint supplying the signed vault, absent for a subscriber proof.
    pub fn server_endpoint(&self) -> Option<&str> {
        self.server_endpoint.as_deref()
    }
    /// Value-free native evidence for a caller's durable record. This is not a transaction
    /// outcome and grants no authorization to change a unit.
    pub fn value(&self) -> Value {
        json!({"job":self.job.label(),"unit":{"program":self.unit.program,"args":self.unit.args,
            "definition":self.unit.definition,"image":self.unit.image,"executable_sha256":self.unit.executable_sha256},
            "pid":self.pid,"process_identity":self.process_identity,
            "challenge_sha256":hex(&aterm_digest::Sha256::digest(self.challenge.as_bytes())),
            "health":{"vault":self.health.vault,"signer":self.health.signer,"records":self.health.records,
                "frontier":self.health.frontier},"server_endpoint":self.server_endpoint})
    }
}

impl NativeBackend {
    /// Compose the live configured paths, exact signed request, and running application's
    /// build floor. Construction performs no recovery or service mutation.
    pub fn new(paths: Paths, signed: Option<Value>, running_build: u64) -> Self {
        Self {
            paths,
            signed,
            running_build,
            challenge: None,
            proof: None,
        }
    }

    fn check_context(&self) -> Result<()> {
        require_live_context(&self.paths)
    }

    /// Set the authenticated request envelope before activation; this grants no authority.
    pub fn set_signed_request(&mut self, signed: Value) {
        self.signed = Some(signed);
        self.proof = None;
    }

    /// Last successful native proof, only for this exact job and complete unit.
    /// A failed or new proof attempt leaves no previous proof available.
    pub fn proof_for(&self, job: Job, unit: &Unit) -> Option<&NativeProof> {
        self.proof
            .as_ref()
            .filter(|proof| proof.job == job && proof.unit == *unit)
    }

    /// Reverify stored bytes, current release authority, qualification and native signature.
    /// # Errors
    /// Stored evidence is missing, refused, revoked, stale or does not match the image.
    pub fn receipt(&self, image: &str) -> Result<Receipt> {
        verified_receipt(&self.paths, image)
    }
}

fn require_live_context(paths: &Paths) -> Result<()> {
    if &Paths::live()? != paths {
        return Err(fail("native backend context differs from live services"));
    }
    Ok(())
}

/// Reverify a stored qualified image without choosing a controller build floor.
/// This reads the same native Store evidence as the production backend. A lab can
/// derive its held baseline's build policy from this receipt instead of caller data.
/// # Errors
/// The context, stored evidence, release authority, native seal or image is unavailable
/// or refused by current revocation and roster policy.
pub fn verified_receipt(paths: &Paths, image: &str) -> Result<Receipt> {
    require_live_context(paths)?;
    let dir = store_root(paths);
    let authority = value(&dir.join("authority.json"))?;
    let roster: Vec<u8> = aterm_json::from_value(authority["bytes"].clone()).map_err(fail)?;
    let sig: Vec<u8> = aterm_json::from_value(authority["signature"].clone()).map_err(fail)?;
    let floor = authority["roster"]
        .as_u64()
        .ok_or_else(|| fail("no durable roster floor"))?;
    Store::new(dir)
        .map_err(fail)?
        .verify_against(image, &roster, &sig, now()?, floor)
        .map_err(fail)
}
impl Backend for NativeBackend {
    fn authorize(&mut self, r: &Request) -> Result<()> {
        self.proof = None;
        self.check_context()?;
        if identity()?.node() != r.node {
            return Err(fail("activation targets another node"));
        }
        let signed = self
            .signed
            .as_ref()
            .ok_or_else(|| fail("activation has no signed request"))?;
        let key = signed["key"]
            .as_str()
            .and_then(aterm_net::fleet::NodeKey::from_hex)
            .ok_or_else(|| fail("invalid requester key"))?;
        let proof = aterm_net::fleet::NodeProof {
            node: r.requester.clone(),
            key,
        };
        check_denied(&self.paths, &r.requester, &key)?;
        let body = aterm_json::to_vec(&transaction::request_value(r)).map_err(fail)?;
        let membership = crate::fleet_log::LocalLog::of(&self.paths)
            .keys()
            .ok_or_else(|| fail("current signed membership unavailable"))?;
        if !membership.admits(&key) || !membership.admits(&identity()?.key()) {
            return Err(fail("requester or target lacks current signed membership"));
        }
        aterm_net::fleet_member::signed_record_driver(
            &proof,
            REQUEST_DOMAIN,
            &body,
            &signature(signed)?,
            &state_dir()?,
            &grants()?,
            Some(&membership),
        )
        .map_err(fail)?;
        Ok(())
    }
    fn observe(&mut self, job: Job) -> Result<Observed> {
        self.proof = None;
        self.check_context()?;
        let mut observed = service::repair_observe(&self.paths.home, job)?;
        if let Some(unit) = &mut observed.unit
            && let Ok(relative) = Path::new(&unit.program).strip_prefix(store_root(&self.paths))
        {
            let mut components = relative.components();
            if let (Some(image), Some(image_dir)) = (
                components.next().and_then(|p| p.as_os_str().to_str()),
                components.next(),
            ) {
                if image.len() != 64
                    || !image
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    || image_dir.as_os_str()
                        != if cfg!(target_os = "macos") {
                            "image.app"
                        } else {
                            "image"
                        }
                    || Path::new(&unit.program).canonicalize()? != Path::new(&unit.program)
                {
                    return Err(fail("service executable has an invalid store identity"));
                }
                // Observation must remain possible after the candidate is revoked so
                // crash recovery can restore its predecessor. Authentication happens
                // before activation; the journal binds the exact loaded-image hash.
                unit.image = image.to_owned();
            }
        }
        Ok(observed)
    }
    fn verify_candidate(&mut self, r: &Request) -> Result<()> {
        self.proof = None;
        self.check_context()?;
        let receipt = self.receipt(&r.desired_image)?;
        let protocols = r
            .required_protocols
            .iter()
            .copied()
            .map(u32::try_from)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(fail)?;
        if !r.required_protocols.contains(&PROTOCOL)
            || !receipt.service.protocol.supports(&protocols)
        {
            return Err(fail("service protocol excludes retained GUI or peer"));
        }
        let native = if cfg!(target_os = "macos") {
            if cfg!(target_arch = "aarch64") {
                "aarch64-apple-darwin"
            } else {
                "x86_64-apple-darwin"
            }
        } else if cfg!(target_arch = "aarch64") {
            "aarch64-unknown-linux-gnu"
        } else {
            "x86_64-unknown-linux-gnu"
        };
        if receipt.service.platform != native
            && !(cfg!(target_os = "macos") && receipt.service.platform == "universal-apple-darwin")
        {
            return Err(fail("service artifact platform mismatch"));
        }
        if receipt.build < self.running_build {
            return Err(fail("service artifact predates running application"));
        }
        let store = Store::new(store_root(&self.paths)).map_err(fail)?;
        if store.executable(&receipt).map_err(fail)? != Path::new(&r.unit.program)
            || r.unit.executable_sha256 != receipt.service.executable_sha256
        {
            return Err(fail("unit does not name verified artifact"));
        }
        // Executing the authenticated image's headless protocol verb also proves
        // this host can load its native slice before any service is stopped.
        let mut probe = aterm_uds::spawnfd::command(&r.unit.program);
        probe.args(["secrets", "service-repair", "protocol"]);
        let output = aterm_uds::deadline::output_within(&mut probe, Duration::from_secs(60))
            .map_err(fail)?;
        if !output.status.success() {
            return Err(fail(
                "candidate cannot run its native service protocol probe",
            ));
        }
        let protocol: Value = aterm_json::from_slice(&output.stdout).map_err(fail)?;
        if protocol["schema"].as_u64() != Some(1)
            || protocol["min"].as_u64() != Some(u64::from(receipt.service.protocol.min))
            || protocol["max"].as_u64() != Some(u64::from(receipt.service.protocol.max))
        {
            return Err(fail("candidate protocol differs from signed declaration"));
        }
        if r.precondition
            .unit
            .as_ref()
            .is_none_or(|u| u.args != r.unit.args)
        {
            return Err(fail("activation changes service arguments"));
        }
        let expected = service::repair_unit(
            &self.paths.home,
            r.job,
            r.precondition
                .unit
                .as_ref()
                .ok_or_else(|| fail("no previous unit"))?,
            Path::new(&r.unit.program),
            &r.desired_image,
        )?;
        if expected != r.unit {
            return Err(fail("activation changes service definition"));
        }
        if r.health.records.is_empty() || r.health.signer.is_empty() {
            return Err(fail("activation lacks authenticated frontier"));
        }
        let config = client::config(&self.paths)?.ok_or_else(|| fail("no configured vault"))?;
        let current = client::status(&self.paths)?;
        if !client::materialization_ready(&self.paths)?
            || !matches!(current.membership,client::Membership::In{seq,..} if r.health.frontier>=seq)
        {
            return Err(fail(
                "activation health requirement predates the verified local vault",
            ));
        }
        if config.name != r.health.vault
            || config.signer.map(|s| hex(&s)).as_deref() != Some(&r.health.signer)
        {
            return Err(fail(
                "activation health target differs from configured vault",
            ));
        }
        let frontier = client::load_records(&self.paths)?.frontier();
        if frontier.is_empty()
            || frontier
                .iter()
                .any(|id| !r.health.records.contains(&hex(id)))
        {
            return Err(fail(
                "activation omits the current signed membership frontier",
            ));
        }
        Ok(())
    }
    fn install(&mut self, job: Job, unit: &Unit) -> Result<()> {
        self.proof = None;
        self.check_context()?;
        if Path::new(&unit.program).starts_with(store_root(&self.paths)) {
            let receipt = self.receipt(&unit.image)?;
            if receipt.service.executable_sha256 != unit.executable_sha256
                || Store::new(store_root(&self.paths))
                    .map_err(fail)?
                    .executable(&receipt)
                    .map_err(fail)?
                    != Path::new(&unit.program)
            {
                return Err(fail(
                    "activation or rollback image no longer matches its signed receipt",
                ));
            }
        }
        self.challenge = Some(if job == Job::Subscriber {
            aterm_secrets::service_health::prepare(&self.paths)?
        } else {
            aterm_secrets::service_health::prepare_server(&self.paths)?
        });
        service::repair_install(&self.paths.home, job, unit)
    }
    fn prove(&mut self, job: Job, unit: &Unit, health: &HealthRequirement) -> Result<()> {
        self.proof = None;
        self.check_context()?;
        if self.challenge.is_none() {
            self.challenge = Some(if job == Job::Subscriber {
                aterm_secrets::service_health::prepare(&self.paths)?
            } else {
                aterm_secrets::service_health::prepare_server(&self.paths)?
            });
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            self.proof = None;
            let check = (|| {
                service::repair_prove_image(&self.paths.home, job, unit)?;
                let service::State::Installed { pid: Some(pid), .. } =
                    service::state(&self.paths.home, job)?
                else {
                    return Err(fail("service has no running PID"));
                };
                let process_pid = i32::try_from(pid).map_err(fail)?;
                let process = aterm_uds::process::read_identity(process_pid)
                    .ok_or_else(|| fail("service process identity unavailable"))?;
                let challenge = self
                    .challenge
                    .as_deref()
                    .ok_or_else(|| fail("no service challenge"))?;
                let mut server_endpoint = None;
                if job == Job::Subscriber {
                    let (program_bytes, _) = aterm_uds::atomic_file::read_regular(
                        std::path::Path::new(&unit.program),
                        512 * 1024 * 1024,
                    )?;
                    let executable = hex(&aterm_digest::Sha256::digest(program_bytes));
                    aterm_secrets::service_health::subscriber(
                        &self.paths,
                        challenge,
                        pid,
                        &executable,
                        health,
                    )?;
                } else {
                    let (_, url) = aterm_secrets::service_health::server_address(
                        &self.paths,
                        &unit.args,
                        &health.vault,
                    )?;
                    let port = url
                        .split('/')
                        .nth(2)
                        .and_then(|s| s.rsplit(':').next())
                        .and_then(|s| s.parse::<u16>().ok())
                        .ok_or_else(|| fail("server URL has no explicit port"))?;
                    aterm_secrets::service_health::server_process(
                        &self.paths,
                        challenge,
                        pid,
                        port,
                    )?;
                    aterm_secrets::service_health::server(&url, health)?;
                    server_endpoint = Some(url);
                }
                service::repair_prove_image(&self.paths.home, job, unit)?;
                if aterm_uds::process::read_identity(process_pid) != Some(process)
                    || !matches!(service::state(&self.paths.home, job)?,
                        service::State::Installed { pid: Some(current), .. } if current == pid)
                {
                    return Err(fail("service process changed during native health proof"));
                }
                Ok(NativeProof {
                    job,
                    unit: unit.clone(),
                    pid,
                    process_identity: process.token(),
                    challenge: challenge.to_owned(),
                    health: health.clone(),
                    server_endpoint,
                })
            })();
            match check {
                Ok(proof) => {
                    self.proof = Some(proof);
                    return Ok(());
                }
                Err(e) if Instant::now() >= deadline => return Err(e),
                Err(_) => std::thread::sleep(Duration::from_millis(200)),
            }
        }
    }
}

/// Prepare the exact compare-and-swap request without changing a unit or granting authority.
/// # Errors
/// The nonce, current service, vault, signed frontier or qualified image is unavailable.
pub fn prepare_request(
    paths: &Paths,
    job: Job,
    desired: &str,
    nonce: &str,
    requester: &str,
    running_build: u64,
) -> Result<Request> {
    if !(16..=128).contains(&nonce.len())
        || !nonce
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(fail(
            "service repair nonce must be 16..128 ASCII letters, digits or hyphens",
        ));
    }
    let mut live = NativeBackend::new(paths.clone(), None, running_build);
    let receipt = live.receipt(desired)?;
    let precondition = live.observe(job)?;
    let old = precondition
        .unit
        .as_ref()
        .ok_or_else(|| fail("service is absent"))?;
    let config = client::config(paths)?.ok_or_else(|| fail("no configured vault"))?;
    if !client::materialization_ready(paths)? {
        return Err(fail("vault is not currently materialized"));
    }
    let seq = match client::status(paths)?.membership {
        client::Membership::In { seq, .. } => seq,
        _ => return Err(fail("not a vault member")),
    };
    let records = client::load_records(paths)?
        .frontier()
        .iter()
        .map(|r| hex(r))
        .collect();
    let store = Store::new(store_root(paths)).map_err(fail)?;
    Ok(Request {
        node: identity()?.node().into(),
        requester: requester.into(),
        nonce: nonce.into(),
        job,
        current_image: old.image.clone(),
        desired_image: desired.into(),
        unit: service::repair_unit(
            &paths.home,
            job,
            old,
            &store.executable(&receipt).map_err(fail)?,
            desired,
        )?,
        precondition,
        health: HealthRequirement {
            vault: config.name,
            signer: hex(&config.signer.ok_or_else(|| fail("vault is unsigned"))?),
            frontier: seq,
            records,
        },
        required_protocols: vec![PROTOCOL],
    })
}

/// Recheck the proven RPC caller against current local membership, denies and drive grants.
/// # Errors
/// Either node is no longer admitted, its key is denied, or current drive authority refuses it.
pub fn authorize_rpc(paths: &Paths, requester: &str, key: &str) -> Result<()> {
    if Paths::live()? != *paths {
        return Err(fail("RPC backend context differs from live services"));
    }
    let key = aterm_net::fleet::NodeKey::from_hex(key)
        .ok_or_else(|| fail("RPC has no authenticated driver key"))?;
    check_denied(paths, requester, &key)?;
    let membership = crate::fleet_log::LocalLog::of(paths)
        .keys()
        .ok_or_else(|| fail("current signed membership unavailable"))?;
    if !membership.admits(&key) || !membership.admits(&identity()?.key()) {
        return Err(fail("RPC requester or target is no longer a member"));
    }
    aterm_net::fleet_member::recheck_driver(
        &aterm_net::fleet::NodeProof {
            node: requester.into(),
            key,
        },
        &state_dir()?,
        &grants()?,
        Some(&membership),
    )
    .map_err(fail)?;
    Ok(())
}

/// Sign the production transaction outcome with the target's existing fleet identity.
/// The signature attests to the supplied outcome; it does not turn caller-side lab
/// acceptance rejection into a native service-health failure.
/// # Errors
/// The local identity is unavailable, the outcome names another node, or encoding fails.
pub fn signed_outcome(outcome: &Outcome) -> Result<Value> {
    let own = identity()?;
    signed_outcome_as(outcome, &own)
}

fn signed_outcome_as(outcome: &Outcome, own: &aterm_net::fleet::NodeIdentity) -> Result<Value> {
    if outcome.request.node != own.node() {
        return Err(fail("service outcome targets another node"));
    }
    let body = transaction::outcome_value(outcome);
    let signature = own.sign_record(OUTCOME_DOMAIN, &aterm_json::to_vec(&body).map_err(fail)?);
    Ok(json!({"outcome":body,"node":own.node(),"key":own.key().hex(),"signature":hex(&signature)}))
}

/// Authenticate a terminal result offline against an explicitly trusted target and request.
/// `expected_request` is the complete plain request or the existing signed request envelope;
/// its envelope signature is not a source of authority. A valid result may be historical.
/// `signed` may be the direct envelope or a lab report's top-level `signed_outcome`;
/// surrounding report claims are not authenticated by this verification.
/// This performs no identity discovery, service access, network access or mutation.
/// # Errors
/// Inputs exceed their bounds, are malformed or noncanonical, or differ from the pinned
/// target, exact request or authenticated outcome signature.
pub fn verify_outcome(
    signed: &[u8],
    expected_request: &[u8],
    expected_node: &str,
    expected_key: &str,
) -> Result<Outcome> {
    if signed.len() > MAX_SIGNED_OUTCOME_BYTES
        || expected_request.len() > MAX_EXPECTED_REQUEST_BYTES
    {
        return Err(fail(
            "offline outcome verification input exceeds supported limit",
        ));
    }
    let key = aterm_net::fleet::NodeKey::from_hex(expected_key)
        .ok_or_else(|| fail("invalid pinned target key"))?;
    let input: Value = aterm_json::from_slice(signed).map_err(fail)?;
    let envelope = input.get("signed_outcome").unwrap_or(&input);
    if envelope.as_object().is_none_or(|o| o.len() != 4)
        || envelope["node"].as_str() != Some(expected_node)
        || envelope["key"].as_str() != Some(expected_key)
    {
        return Err(fail("signed outcome does not match pinned target node/key"));
    }
    let expected: Value = aterm_json::from_slice(expected_request).map_err(fail)?;
    let expected = expected.get("request").unwrap_or(&expected);
    let request = transaction::parse_request(expected)?;
    if request.node != expected_node || transaction::request_value(&request) != *expected {
        return Err(fail(
            "expected request is not the exact canonical target request",
        ));
    }
    let outcome = transaction::parse_outcome(&envelope["outcome"])?;
    let body = transaction::outcome_value(&outcome);
    if outcome.request != request || body != envelope["outcome"] {
        return Err(fail("outcome does not bind the exact full request/result"));
    }
    if !aterm_net::fleet::verifies_record(
        &key,
        OUTCOME_DOMAIN,
        &aterm_json::to_vec(&body).map_err(fail)?,
        &signature(envelope)?,
    ) {
        return Err(fail("invalid signed service outcome"));
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit() -> Unit {
        Unit {
            program: "/inert/aterm".into(),
            args: vec!["secrets".into(), "serve".into()],
            definition: "inert test definition".into(),
            image: "a".repeat(64),
            executable_sha256: "b".repeat(64),
        }
    }
    fn health() -> HealthRequirement {
        HealthRequirement {
            vault: "team".into(),
            signer: "c".repeat(64),
            records: vec!["d".repeat(64)],
            frontier: 17,
        }
    }
    fn proof() -> NativeProof {
        // Private fixture for checking custody/accessor behavior. This never reaches a
        // production callback and is not evidence that native operations ran in the test.
        NativeProof {
            job: Job::Server,
            unit: unit(),
            pid: 42,
            process_identity: "42@7:3".into(),
            challenge: "e".repeat(64),
            health: health(),
            server_endpoint: Some("http://127.0.0.1:7484/v1/team".into()),
        }
    }
    fn foreign_backend() -> NativeBackend {
        NativeBackend::new(
            Paths::for_home(Path::new("/__aterm_foreign_service_proof__")),
            None,
            149,
        )
    }

    #[test]
    fn native_proof_is_bound_to_the_complete_unit_and_cleared_before_failed_attempts() {
        let mut native = foreign_backend();
        native.proof = Some(proof());
        assert!(native.proof_for(Job::Server, &unit()).is_some());
        assert!(native.proof_for(Job::Subscriber, &unit()).is_none());
        let mut changed = unit();
        changed.definition.push_str(" changed");
        assert!(native.proof_for(Job::Server, &changed).is_none());
        // Foreign configured paths refuse before reading services or materialized secrets.
        assert!(native.prove(Job::Server, &unit(), &health()).is_err());
        assert!(native.proof_for(Job::Server, &unit()).is_none());
        native.proof = Some(proof());
        assert!(native.install(Job::Server, &unit()).is_err());
        assert!(native.proof_for(Job::Server, &unit()).is_none());
    }

    #[test]
    fn native_request_change_discards_previous_proof_and_constructor_keeps_explicit_floor() {
        let mut native = foreign_backend();
        assert_eq!(native.running_build, 149);
        native.proof = Some(proof());
        native.set_signed_request(json!({"request":"different"}));
        assert!(native.proof_for(Job::Server, &unit()).is_none());
    }

    fn outcome_fixture() -> (aterm_net::fleet::NodeIdentity, Outcome) {
        let node = "0123456789abcdef0123456789abcdef";
        let identity = aterm_net::fleet::NodeIdentity::from_seed(node, &[7; 32]).unwrap();
        let outcome = Outcome {
            request: Request {
                node: node.into(),
                requester: node.into(),
                nonce: "outcome-signing-test".into(),
                job: Job::Server,
                current_image: "old".into(),
                desired_image: "new".into(),
                precondition: Observed {
                    unit: Some(unit()),
                    off: false,
                    pin: None,
                },
                unit: unit(),
                health: health(),
                required_protocols: vec![1],
            },
            verdict: transaction::Verdict::RolledBack,
            detail: "lab caller rejection after positive native proof".into(),
        };
        (identity, outcome)
    }

    #[test]
    fn shared_outcome_signature_binds_exact_native_result_and_target() {
        let (identity, mut outcome) = outcome_fixture();
        let signed = signed_outcome_as(&outcome, &identity).unwrap();
        let body = aterm_json::to_vec(&signed["outcome"]).unwrap();
        assert!(aterm_net::fleet::verifies_record(
            &identity.key(),
            "aterm-service-outcome/1",
            &body,
            &signature(&signed).unwrap()
        ));
        outcome.detail.push_str(" changed");
        assert!(!aterm_net::fleet::verifies_record(
            &identity.key(),
            "aterm-service-outcome/1",
            &aterm_json::to_vec(&transaction::outcome_value(&outcome)).unwrap(),
            &signature(&signed).unwrap()
        ));
        outcome.request.node = "another-node".into();
        assert!(signed_outcome_as(&outcome, &identity).is_err());
    }

    #[test]
    fn offline_outcome_pins_real_signature_target_domain_and_every_request_field() {
        fn set(value: &mut Value, path: &[&str], replacement: Value) {
            let (last, parents) = path.split_last().unwrap();
            let mut target = value;
            for key in parents {
                target = target.as_object_mut().unwrap().get_mut(*key).unwrap();
            }
            target
                .as_object_mut()
                .unwrap()
                .insert((*last).into(), replacement);
        }
        let (identity, outcome) = outcome_fixture();
        let signed = signed_outcome_as(&outcome, &identity).unwrap();
        let request = transaction::request_value(&outcome.request);
        let bytes = aterm_json::to_vec(&signed).unwrap();
        let check = |signed: &Value, request: &Value, node: &str, key: &str| {
            verify_outcome(
                &aterm_json::to_vec(signed).unwrap(),
                &aterm_json::to_vec(request).unwrap(),
                node,
                key,
            )
        };
        assert_eq!(
            check(&signed, &request, identity.node(), &identity.key().hex()).unwrap(),
            outcome
        );
        // Existing signed-request files are accepted as expected input, never as trust roots.
        let wrapped = json!({"request":request,"key":"untrusted metadata","signature":"ignored"});
        let lab_report = json!({"signed_outcome":signed,"acceptance_passed":false});
        assert_eq!(
            check(
                &lab_report,
                &wrapped,
                identity.node(),
                &identity.key().hex()
            )
            .unwrap(),
            outcome
        );
        let mut bad_report = lab_report;
        set(
            &mut bad_report,
            &["signed_outcome", "outcome", "detail"],
            json!("forged lab result"),
        );
        assert!(
            check(
                &bad_report,
                &request,
                identity.node(),
                &identity.key().hex()
            )
            .is_err()
        );
        assert_eq!(
            check(&signed, &wrapped, identity.node(), &identity.key().hex()).unwrap(),
            outcome
        );
        let stranger =
            aterm_net::fleet::NodeIdentity::from_seed(identity.node(), &[8; 32]).unwrap();
        assert!(check(&signed, &request, identity.node(), &stranger.key().hex()).is_err());
        // Substituting the envelope key too must still fail against a valid pinned key.
        let mut changed = signed.clone();
        set(&mut changed, &["key"], json!(stranger.key().hex()));
        assert!(check(&changed, &request, identity.node(), &stranger.key().hex()).is_err());
        assert!(
            check(
                &signed,
                &request,
                "fedcba9876543210fedcba9876543210",
                &identity.key().hex()
            )
            .is_err()
        );
        let mut changed = signed.clone();
        set(
            &mut changed,
            &["signature"],
            json!(hex(&identity.sign_record(
                REQUEST_DOMAIN,
                &aterm_json::to_vec(&signed["outcome"]).unwrap()
            ))),
        );
        assert!(check(&changed, &request, identity.node(), &identity.key().hex()).is_err());
        for field in [
            "nonce",
            "unit",
            "health",
            "requester",
            "precondition",
            "required_protocols",
        ] {
            let mut expected = request.clone();
            match field {
                "nonce" => set(&mut expected, &["nonce"], json!("different-nonce-123456")),
                "unit" => set(&mut expected, &["unit", "program"], json!("/another/aterm")),
                "health" => set(&mut expected, &["health", "frontier"], json!(99999)),
                "requester" => set(&mut expected, &["requester"], json!("another-requester")),
                "precondition" => set(&mut expected, &["precondition", "off"], json!(true)),
                _ => set(&mut expected, &["required_protocols"], json!([2])),
            }
            assert!(
                check(&signed, &expected, identity.node(), &identity.key().hex()).is_err(),
                "{field}"
            );
            let mut changed = signed.clone();
            set(&mut changed, &["outcome", "request"], expected);
            assert!(
                check(&changed, &request, identity.node(), &identity.key().hex()).is_err(),
                "{field}"
            );
        }
        for (field, value) in [
            ("verdict", json!("committed")),
            ("detail", json!("altered")),
            ("schema", json!(2)),
            ("extra", json!(true)),
        ] {
            let mut changed = signed.clone();
            set(&mut changed, &["outcome", field], value);
            assert!(
                check(&changed, &request, identity.node(), &identity.key().hex()).is_err(),
                "{field}"
            );
        }
        let request_bytes = aterm_json::to_vec(&request).unwrap();
        let mut at_limit = bytes.clone();
        at_limit.resize(MAX_SIGNED_OUTCOME_BYTES, b' ');
        let mut request_at_limit = request_bytes.clone();
        request_at_limit.resize(MAX_EXPECTED_REQUEST_BYTES, b' ');
        assert_eq!(
            verify_outcome(
                &at_limit,
                &request_at_limit,
                identity.node(),
                &identity.key().hex()
            )
            .unwrap(),
            outcome
        );
        for malformed in [b"{".as_slice(), b"null", b"[]", b"{}"] {
            assert!(
                verify_outcome(
                    malformed,
                    &request_bytes,
                    identity.node(),
                    &identity.key().hex()
                )
                .is_err()
            );
            assert!(
                verify_outcome(&bytes, malformed, identity.node(), &identity.key().hex()).is_err()
            );
        }
        assert!(
            verify_outcome(
                &vec![b' '; MAX_SIGNED_OUTCOME_BYTES + 1],
                &request_bytes,
                identity.node(),
                &identity.key().hex()
            )
            .is_err()
        );
        assert!(
            verify_outcome(
                &bytes,
                &vec![b' '; MAX_EXPECTED_REQUEST_BYTES + 1],
                identity.node(),
                &identity.key().hex()
            )
            .is_err()
        );
        let mut changed = signed;
        set(&mut changed, &["signature"], json!("not-hex"));
        assert!(check(&changed, &request, identity.node(), &identity.key().hex()).is_err());
    }
}
