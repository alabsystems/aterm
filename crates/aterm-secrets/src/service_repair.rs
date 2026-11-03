// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Durable, nonce-bound service activation. The backend supplies authenticated artifact,
//! fleet authority, process identity and service behavior evidence; this module orders
//! those operations with the same decision lock used by On, Off and pin changes.

use crate::{Error, Result, service::Job};
use aterm_json::{Value, json};
use std::path::{Path, PathBuf};

/// A request leaves room for both its active record and a retained recovery result.
pub const MAX_REQUEST_BYTES: usize = 256 * 1024;
const MAX_JOURNAL_BYTES: usize = 1024 * 1024;
const MAX_DETAIL_BYTES: usize = 16 * 1024;

fn bounded_detail(detail: String) -> String {
    bounded_detail_to(detail, MAX_DETAIL_BYTES)
}

fn bounded_detail_to(mut detail: String, limit: usize) -> String {
    const SUFFIX: &str = "\n[diagnostic truncated]";
    if detail.len() > limit {
        let mut end = limit - SUFFIX.len();
        while !detail.is_char_boundary(end) {
            end -= 1;
        }
        detail.truncate(end);
        detail.push_str(SUFFIX);
    }
    detail
}

/// Exact service-manager definition and the executable identity it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    /// Absolute executable path in a lasting image.
    pub program: String,
    /// Exact arguments after the executable.
    pub args: Vec<String>,
    /// Exact platform service-manager definition.
    pub definition: String,
    /// Authenticated complete release-image identity, or legacy executable digest.
    pub image: String,
    /// SHA-256 digest of the executable bytes within that image.
    pub executable_sha256: String,
}

/// Complete state which must still hold under the decision lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    /// Installed service, absent when no supported unit exists.
    pub unit: Option<Unit>,
    /// Explicit user Off record.
    pub off: bool,
    /// Explicit image pin, if present.
    pub pin: Option<String>,
}

/// Authenticated service behavior required after activation and rollback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthRequirement {
    /// Expected vault identity.
    pub vault: String,
    /// Trusted membership signer identity.
    pub signer: String,
    /// Required signed membership frontier record identities.
    pub records: Vec<String>,
    /// Minimum authenticated applied sequence.
    pub frontier: u64,
}

/// Every security-relevant input is retained in the journal and nonce result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// Receiving fleet node identity.
    pub node: String,
    /// Authenticated requesting fleet node identity.
    pub requester: String,
    /// Unique transaction nonce, retained durably with its result.
    pub nonce: String,
    /// Service to activate.
    pub job: Job,
    /// Observed executable digest.
    pub current_image: String,
    /// Requested executable digest.
    pub desired_image: String,
    /// Complete compare-and-swap precondition.
    pub precondition: Observed,
    /// Desired exact service definition.
    pub unit: Unit,
    /// Authenticated post-start health expectations.
    pub health: HealthRequirement,
    /// Every retained GUI or peer protocol the candidate must serve.
    pub required_protocols: Vec<u64>,
}

/// A terminal transaction state. A failed rollback blocks subsequent activation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// New image and health proven.
    Committed,
    /// Activation failed and old image and health proven.
    RolledBack,
    /// Request refused before touching the service.
    Refused,
    /// Restoration or its proof failed; no new transaction may start.
    RollbackFailed,
}

/// A result binds the complete request, not merely a caller-supplied nonce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// Fully bound original request.
    pub request: Request,
    /// Durable result state.
    pub verdict: Verdict,
    /// Evidence or failure description, containing no secret values and bounded to
    /// 16 KiB at a UTF-8 boundary so every terminal result fits its durable journal.
    pub detail: String,
}

/// Production adapters must verify current authority and revocation on every request,
/// including a replay; candidate verification includes signature, receipt, image bytes,
/// qualification and every required GUI/peer protocol. `prove` must check the running
/// executable hash and authenticated vault/frontier behavior, not just a PID or port.
pub trait Backend {
    /// Check current authenticated membership and drive authority for both nodes.
    fn authorize(&mut self, request: &Request) -> Result<()>;
    /// Read the complete installed unit, Off record and explicit pin.
    fn observe(&mut self, job: Job) -> Result<Observed>;
    /// Verify the signed, qualified candidate bytes and retained protocol range.
    fn verify_candidate(&mut self, request: &Request) -> Result<()>;
    /// Install and start this exact definition without mutating executable images.
    fn install(&mut self, job: Job, unit: &Unit) -> Result<()>;
    /// Prove executable identity and current signed vault behavior.
    fn prove(&mut self, job: Job, unit: &Unit, health: &HealthRequirement) -> Result<()>;
}

fn err(message: impl Into<String>) -> Error {
    Error::new(message.into())
}
fn root(home: &Path) -> PathBuf {
    home.join(".config/aterm/secrets/service-repair")
}
fn active(home: &Path, job: Job) -> PathBuf {
    root(home).join(format!("{}.json", job.label()))
}
fn receipt(home: &Path, r: &Request) -> PathBuf {
    root(home).join(format!("{}-{}.json", r.job.label(), r.nonce))
}
fn nonce_valid(nonce: &str) -> bool {
    (16..=128).contains(&nonce.len())
        && nonce
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
fn unit_value(u: &Unit) -> Value {
    json!({"program":u.program,"args":u.args,"definition":u.definition,"image":u.image,"executable_sha256":u.executable_sha256})
}
fn observed_value(o: &Observed) -> Value {
    json!({"unit":o.unit.as_ref().map(unit_value),"off":o.off,"pin":o.pin})
}
/// Canonical signed-request payload, also retained in the activation journal.
pub fn request_value(r: &Request) -> Value {
    json!({"node":r.node,"requester":r.requester,"nonce":r.nonce,"job":r.job.label(),
        "current_image":r.current_image,"desired_image":r.desired_image,
        "precondition":observed_value(&r.precondition),"unit":unit_value(&r.unit),
        "health":{"vault":r.health.vault,"signer":r.health.signer,"records":r.health.records,"frontier":r.health.frontier},
        "required_protocols":r.required_protocols})
}
fn word<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .ok_or_else(|| err(format!("service repair journal: missing {key}")))
}
fn strings(v: &Value) -> Result<Vec<String>> {
    v.as_array()
        .ok_or_else(|| err("service repair journal: expected array"))?
        .iter()
        .map(|x| {
            x.as_str()
                .map(str::to_owned)
                .ok_or_else(|| err("service repair journal: expected string"))
        })
        .collect()
}
fn parse_unit(v: &Value) -> Result<Unit> {
    Ok(Unit {
        program: word(v, "program")?.into(),
        args: strings(&v["args"])?,
        definition: word(v, "definition")?.into(),
        image: word(v, "image")?.into(),
        executable_sha256: word(v, "executable_sha256")?.into(),
    })
}
/// Decode a signed-request payload without granting authority or validating its image.
pub fn parse_request(v: &Value) -> Result<Request> {
    if !nonce_valid(word(v, "nonce")?) {
        return Err(err("service repair journal: invalid nonce"));
    }
    let job = match word(v, "job")? {
        "systems.alab.aterm-secrets" => Job::Subscriber,
        "systems.alab.aterm-secrets-server" => Job::Server,
        _ => return Err(err("service repair journal: unknown job")),
    };
    let o = &v["precondition"];
    let request = Request {
        node: word(v, "node")?.into(),
        requester: word(v, "requester")?.into(),
        nonce: word(v, "nonce")?.into(),
        job,
        current_image: word(v, "current_image")?.into(),
        desired_image: word(v, "desired_image")?.into(),
        precondition: Observed {
            unit: if o["unit"].is_null() {
                None
            } else {
                Some(parse_unit(&o["unit"])?)
            },
            off: o["off"]
                .as_bool()
                .ok_or_else(|| err("service repair journal: missing off"))?,
            pin: if o["pin"].is_null() {
                None
            } else {
                Some(word(o, "pin")?.into())
            },
        },
        unit: parse_unit(&v["unit"])?,
        health: HealthRequirement {
            vault: word(&v["health"], "vault")?.into(),
            signer: word(&v["health"], "signer")?.into(),
            records: strings(&v["health"]["records"])?,
            frontier: v["health"]["frontier"]
                .as_u64()
                .ok_or_else(|| err("service repair journal: missing frontier"))?,
        },
        required_protocols: v["required_protocols"]
            .as_array()
            .ok_or_else(|| err("service repair journal: missing protocols"))?
            .iter()
            .map(|p| {
                p.as_u64()
                    .ok_or_else(|| err("service repair journal: invalid protocol"))
            })
            .collect::<Result<_>>()?,
    };
    check_request_size(&request)?;
    Ok(request)
}
fn check_request_size(request: &Request) -> Result<()> {
    if aterm_json::to_vec(&request_value(request))
        .map_err(|e| err(e.to_string()))?
        .len()
        > MAX_REQUEST_BYTES
    {
        return Err(err(
            "service repair request exceeds the current supported journal request limit",
        ));
    }
    Ok(())
}
fn verdict_word(v: Verdict) -> &'static str {
    match v {
        Verdict::Committed => "committed",
        Verdict::RolledBack => "rolled-back",
        Verdict::Refused => "refused",
        Verdict::RollbackFailed => "rollback-failed",
    }
}
fn parse_verdict(s: &str) -> Result<Verdict> {
    match s {
        "committed" => Ok(Verdict::Committed),
        "rolled-back" => Ok(Verdict::RolledBack),
        "refused" => Ok(Verdict::Refused),
        "rollback-failed" => Ok(Verdict::RollbackFailed),
        _ => Err(err("service repair journal: invalid verdict")),
    }
}
fn write(home: &Path, path: &Path, value: &Value) -> Result<()> {
    let bytes = aterm_json::to_vec(value).map_err(|e| err(e.to_string()))?;
    if bytes.len() > MAX_JOURNAL_BYTES {
        return Err(err(
            "service repair journal exceeds its readable size limit",
        ));
    }
    crate::fsx::ensure_private_dir(&root(home))?;
    // The atomic writer flushes the file and its immediate directory. These parent
    // entries may themselves have just been created by the service decision lock.
    for directory in [
        home.to_path_buf(),
        home.join(".config"),
        home.join(".config/aterm"),
        home.join(".config/aterm/secrets"),
    ] {
        aterm_durable::durable_dir(&directory)?;
    }
    crate::fsx::write_atomic(path, &bytes, 0o600)
}
fn read(path: &Path) -> Result<Option<Value>> {
    crate::fsx::read_owned(path, MAX_JOURNAL_BYTES)?
        .map(|b| aterm_json::from_slice(&b).map_err(|e| err(e.to_string())))
        .transpose()
}
/// Machine-readable result binding the full request to its durable verdict.
pub fn outcome_value(o: &Outcome) -> Value {
    json!({"schema":1,"request":request_value(&o.request),"verdict":verdict_word(o.verdict),"detail":o.detail})
}
/// Decode a bounded terminal result without authenticating its source or freshness.
pub fn parse_outcome(v: &Value) -> Result<Outcome> {
    check_schema(v)?;
    let detail = word(v, "detail")?;
    if detail.len() > MAX_DETAIL_BYTES {
        return Err(err(
            "service repair outcome exceeds the current supported diagnostic limit",
        ));
    }
    Ok(Outcome {
        request: parse_request(&v["request"])?,
        verdict: parse_verdict(word(v, "verdict")?)?,
        detail: detail.into(),
    })
}
fn check_schema(v: &Value) -> Result<()> {
    if v["schema"].as_u64() != Some(1) {
        return Err(err("unsupported service repair journal schema"));
    }
    Ok(())
}
fn finish(home: &Path, mut o: Outcome) -> Result<Outcome> {
    o.detail = bounded_detail(o.detail);
    // Persist the terminal active record first: a crash between writes can replay it.
    write(home, &active(home, o.request.job), &outcome_value(&o))?;
    retain_receipt(home, &o)?;
    Ok(o)
}
fn retain_receipt(home: &Path, original: &Outcome) -> Result<()> {
    let path = receipt(home, &original.request);
    if let Some(value) = read(&path)? {
        if parse_outcome(&value)? != *original {
            return Err(err(
                "service repair nonce receipt conflicts with its original result",
            ));
        }
        return Ok(());
    }
    write(home, &path, &outcome_value(original))
}
fn original_result(value: &Value, request: &Request) -> Result<Option<Outcome>> {
    value
        .get("original_result")
        .map(|value| {
            let original = parse_outcome(value)?;
            if original.request != *request || original.verdict != Verdict::RollbackFailed {
                return Err(err(
                    "service repair recovery has an invalid original result",
                ));
            }
            Ok(original)
        })
        .transpose()
}
fn rollback(
    home: &Path,
    r: Request,
    backend: &mut impl Backend,
    detail: String,
    original: Option<Outcome>,
) -> Result<Outcome> {
    let old = r
        .precondition
        .unit
        .as_ref()
        .ok_or_else(|| err("service repair journal has no rollback unit"))?;
    let restored = backend
        .install(r.job, old)
        .and_then(|()| backend.prove(r.job, old, &r.health));
    let detail = bounded_detail(detail);
    let (verdict, detail) = match restored {
        Ok(()) => (Verdict::RolledBack, detail),
        Err(e) => (
            Verdict::RollbackFailed,
            format!(
                "{}; rollback: {}",
                bounded_detail_to(detail, MAX_DETAIL_BYTES / 2 - 16),
                bounded_detail_to(e.to_string(), MAX_DETAIL_BYTES / 2 - 16)
            ),
        ),
    };
    let outcome = Outcome {
        request: r,
        verdict,
        detail: bounded_detail(detail),
    };
    if let Some(original) = original {
        let mut value = outcome_value(&outcome);
        value
            .as_object_mut()
            .expect("outcome is an object")
            .insert("original_result".into(), outcome_value(&original));
        write(home, &active(home, outcome.request.job), &value)?;
        retain_receipt(home, &original)?;
        Ok(outcome)
    } else {
        finish(home, outcome)
    }
}
fn recovery_precondition(request: &Request, backend: &mut impl Backend) -> Result<()> {
    let observed = backend.observe(request.job)?;
    if observed.off != request.precondition.off || observed.pin != request.precondition.pin {
        return Err(err(
            "service repair recovery held: Off or pin changed after interruption",
        ));
    }
    if observed.unit != request.precondition.unit && observed.unit.as_ref() != Some(&request.unit) {
        return Err(err(
            "service repair recovery held: service unit changed after interruption",
        ));
    }
    Ok(())
}
fn recover_locked(
    home: &Path,
    job: Job,
    backend: &mut impl Backend,
    retry_failed: bool,
) -> Result<Option<Outcome>> {
    let Some(v) = read(&active(home, job))? else {
        return Ok(None);
    };
    check_schema(&v)?;
    if v["verdict"].as_str().is_some() {
        let o = parse_outcome(&v)?;
        if o.request.job != job {
            return Err(err("service repair journal job mismatch"));
        }
        let original = original_result(&v, &o.request)?.unwrap_or_else(|| o.clone());
        retain_receipt(home, &original)?;
        if retry_failed && o.verdict == Verdict::RollbackFailed {
            recovery_precondition(&o.request, backend)?;
            // The original failure is durable before a retry can change the service.
            // An interrupted retry remains a recovery, never a new activation nonce.
            write(
                home,
                &active(home, job),
                &json!({"schema":1,"phase":"recovering",
                "request":request_value(&o.request),"original_result":outcome_value(&original)}),
            )?;
            return rollback(
                home,
                o.request,
                backend,
                "retried failed rollback".into(),
                Some(original),
            )
            .map(Some);
        }
        return Ok(Some(o));
    }
    if !matches!(v["phase"].as_str(), Some("prepared" | "recovering")) {
        return Err(err("unknown service repair journal phase"));
    }
    let r = parse_request(&v["request"])?;
    if r.job != job {
        return Err(err("service repair journal job mismatch"));
    }
    let original = original_result(&v, &r)?;
    if (v["phase"].as_str() == Some("recovering")) != original.is_some() {
        return Err(err(
            "service repair recovery phase lacks its original result",
        ));
    }
    recovery_precondition(&r, backend)?;
    rollback(
        home,
        r,
        backend,
        "recovered interrupted activation".into(),
        original,
    )
    .map(Some)
}

/// Roll back an interrupted transaction, or retry a failed rollback, under the shared
/// service decision lock. The original activation nonce retains its first result.
pub fn recover(home: &Path, job: Job, backend: &mut impl Backend) -> Result<Option<Outcome>> {
    recover_checked(home, job, backend, || Ok(()))
}

/// Recover after rechecking a remote caller's authority under the service lock.
/// Local startup recovery uses [`recover`] and requires no remote grant.
/// # Errors
/// Locking, authorization, journal or recovery failures leave the journal retained.
pub fn recover_checked(
    home: &Path,
    job: Job,
    backend: &mut impl Backend,
    check: impl FnOnce() -> Result<()>,
) -> Result<Option<Outcome>> {
    let _lock = crate::service::JobDecision::take(home, job)?;
    check()?;
    recover_locked(home, job, backend, true)
}

/// Images referenced by either job's durable journal. A store collector must also
/// retain all live executable identities and explicit service pins while holding the
/// service decision locks; a journal parse failure must abort collection.
pub fn journal_images(home: &Path) -> Result<Vec<String>> {
    let mut images = Vec::new();
    for job in [Job::Server, Job::Subscriber] {
        if let Some(v) = read(&active(home, job))? {
            check_schema(&v)?;
            let r = if v["verdict"].as_str().is_some() {
                let outcome = parse_outcome(&v)?;
                let _ = original_result(&v, &outcome.request)?;
                outcome.request
            } else {
                if !matches!(v["phase"].as_str(), Some("prepared" | "recovering")) {
                    return Err(err("unknown service repair journal phase"));
                }
                let r = parse_request(&v["request"])?;
                let original = original_result(&v, &r)?;
                if (v["phase"].as_str() == Some("recovering")) != original.is_some() {
                    return Err(err(
                        "service repair recovery phase lacks its original result",
                    ));
                }
                r
            };
            if r.job != job {
                return Err(err("service repair journal job mismatch"));
            }
            for candidate in [
                Some(r.current_image),
                Some(r.desired_image),
                r.precondition.unit.map(|u| u.image),
                Some(r.unit.image),
            ]
            .into_iter()
            .flatten()
            .filter(|image| !image.is_empty())
            {
                images.push(candidate);
            }
        }
    }
    images.sort();
    images.dedup();
    Ok(images)
}

/// Authenticate, recover, compare, journal, activate and prove one nonce-bound request.
/// Errors before the prepared journal leave the old unit untouched. After preparation,
/// any failure attempts exact restoration, whose failure remains durably distinguishable.
pub fn activate(home: &Path, r: &Request, backend: &mut impl Backend) -> Result<Outcome> {
    check_request_size(r)?;
    if !nonce_valid(&r.nonce) {
        return Err(err(
            "service repair nonce must be 16..128 ASCII letters, digits or hyphens",
        ));
    }
    let _lock = crate::service::JobDecision::take(home, r.job)?;
    backend.authorize(r)?;
    let recovered = recover_locked(home, r.job, backend, false)?;
    if let Some(v) = read(&receipt(home, r))? {
        let previous = parse_outcome(&v)?;
        return if previous.request == *r {
            Ok(previous)
        } else {
            Err(err("service repair nonce reused for a different request"))
        };
    }
    if recovered.is_some_and(|previous| previous.verdict == Verdict::RollbackFailed) {
        return Err(err("service repair blocked by failed rollback"));
    }
    let valid = (|| {
        if r.precondition.off || r.precondition.pin.is_some() {
            return Err(err("service repair respects Off and explicit pins"));
        }
        let old = r
            .precondition
            .unit
            .as_ref()
            .ok_or_else(|| err("service repair requires an existing service"))?;
        if old.image != r.current_image || r.unit.image != r.desired_image {
            return Err(err("service repair image binding mismatch"));
        }
        if r.required_protocols.is_empty() {
            return Err(err(
                "service repair requires retained GUI and peer protocols",
            ));
        }
        if r.node.is_empty()
            || r.requester.is_empty()
            || r.current_image.is_empty()
            || r.desired_image.is_empty()
            || old.executable_sha256.is_empty()
            || old.program.is_empty()
            || old.definition.is_empty()
            || r.unit.executable_sha256.is_empty()
            || r.unit.program.is_empty()
            || r.unit.definition.is_empty()
            || r.health.vault.is_empty()
            || r.health.signer.is_empty()
            || r.health.records.is_empty()
            || r.health.records.iter().any(String::is_empty)
        {
            return Err(err(
                "service repair request lacks identity, image or signed health evidence",
            ));
        }
        // Establish a recoverable baseline, including the actual running image:
        // hashing the unit's path alone cannot prove a previously mapped inode.
        backend.prove(r.job, old, &r.health)?;
        backend.verify_candidate(r)?;
        // Evidence acquisition can wait for a service round. A grant revoked during
        // that wait cannot authorize the subsequent executable activation.
        backend.authorize(r)?;
        if backend.observe(r.job)? != r.precondition {
            return Err(err("service repair precondition changed"));
        }
        Ok(())
    })();
    if let Err(e) = valid {
        return finish(
            home,
            Outcome {
                request: r.clone(),
                verdict: Verdict::Refused,
                detail: e.to_string(),
            },
        );
    }
    write(
        home,
        &active(home, r.job),
        &json!({"schema":1,"request":request_value(r),"phase":"prepared"}),
    )?;
    match backend
        .install(r.job, &r.unit)
        .and_then(|()| backend.prove(r.job, &r.unit, &r.health))
    {
        Ok(()) => finish(
            home,
            Outcome {
                request: r.clone(),
                verdict: Verdict::Committed,
                detail: "verified running image and authenticated service health".into(),
            },
        ),
        Err(e) => rollback(home, r.clone(), backend, e.to_string(), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        observed: Observed,
        installs: Vec<String>,
        deny: bool,
        revoke_after_baseline: bool,
        candidate_bad: bool,
        new_failure: Option<&'static str>,
        rollback_bad: bool,
        huge_failure: bool,
    }
    impl Backend for Fake {
        fn authorize(&mut self, _: &Request) -> Result<()> {
            if self.deny {
                Err(err("grant revoked"))
            } else {
                Ok(())
            }
        }
        fn observe(&mut self, _: Job) -> Result<Observed> {
            Ok(self.observed.clone())
        }
        fn verify_candidate(&mut self, _: &Request) -> Result<()> {
            if self.candidate_bad {
                Err(err("forged/revoked artifact or unsupported protocol"))
            } else {
                Ok(())
            }
        }
        fn install(&mut self, _: Job, u: &Unit) -> Result<()> {
            self.installs.push(u.image.clone());
            if u.image == "new" && self.new_failure == Some("failed start") {
                return Err(err("failed start"));
            }
            self.observed.unit = Some(u.clone());
            Ok(())
        }
        fn prove(&mut self, _: Job, u: &Unit, _: &HealthRequirement) -> Result<()> {
            if self.huge_failure
                && !self.installs.is_empty()
                && (u.image == "new" || self.rollback_bad)
            {
                return Err(err("🦀".repeat(MAX_JOURNAL_BYTES)));
            }
            if u.image == "old" && self.installs.is_empty() && self.revoke_after_baseline {
                self.deny = true;
            }
            if u.image == "new"
                && let Some(reason) = self.new_failure
            {
                return Err(err(reason));
            }
            if u.image == "old" && self.rollback_bad && !self.installs.is_empty() {
                return Err(err("old service health failed"));
            }
            Ok(())
        }
    }
    fn setup() -> (Request, Fake) {
        let old = Unit {
            program: "/store/old/aterm".into(),
            args: vec!["secrets".into(), "serve".into()],
            definition: "exact old unit".into(),
            image: "old".into(),
            executable_sha256: "old-exe".into(),
        };
        let observed = Observed {
            unit: Some(old),
            off: false,
            pin: None,
        };
        let new = Unit {
            program: "/store/new/aterm".into(),
            args: vec!["secrets".into(), "serve".into()],
            definition: "exact new unit".into(),
            image: "new".into(),
            executable_sha256: "new-exe".into(),
        };
        (
            Request {
                node: "receiver".into(),
                requester: "controller".into(),
                nonce: "0123456789abcdef".into(),
                job: Job::Server,
                current_image: "old".into(),
                desired_image: "new".into(),
                precondition: observed.clone(),
                unit: new,
                health: HealthRequirement {
                    vault: "signed vault".into(),
                    signer: "signing key".into(),
                    records: vec!["record".into()],
                    frontier: 42,
                },
                required_protocols: vec![1],
            },
            Fake {
                observed,
                installs: vec![],
                deny: false,
                revoke_after_baseline: false,
                candidate_bad: false,
                new_failure: None,
                rollback_bad: false,
                huge_failure: false,
            },
        )
    }
    #[test]
    fn committed_nonce_replays_without_restart_and_rejects_rebinding() {
        let home = aterm_tempfile::tempdir().unwrap();
        let (mut r, mut b) = setup();
        let first = activate(home.path(), &r, &mut b).unwrap();
        assert_eq!(first.verdict, Verdict::Committed);
        assert_eq!(activate(home.path(), &r, &mut b).unwrap(), first);
        assert_eq!(b.installs, ["new"]);
        r.requester = "another controller".into();
        assert!(
            activate(home.path(), &r, &mut b)
                .unwrap_err()
                .to_string()
                .contains("nonce reused")
        );
        b.deny = true;
        assert!(activate(home.path(), &first.request, &mut b).is_err());
        assert_eq!(b.installs, ["new"]);
    }
    #[test]
    fn refusal_controls_never_install() {
        for control in [
            "off",
            "pin",
            "race-off",
            "race-pin",
            "unit",
            "artifact",
            "protocol",
            "identity",
            "absent",
            "authority",
        ] {
            let home = aterm_tempfile::tempdir().unwrap();
            let (mut r, mut b) = setup();
            match control {
                "off" => r.precondition.off = true,
                "pin" => r.precondition.pin = Some("old".into()),
                "race-off" => b.observed.off = true,
                "race-pin" => b.observed.pin = Some("old".into()),
                "unit" => b
                    .observed
                    .unit
                    .as_mut()
                    .unwrap()
                    .definition
                    .push_str(" changed"),
                "artifact" => b.candidate_bad = true,
                "protocol" => r.required_protocols.clear(),
                "identity" => r.current_image = "wrong".into(),
                "absent" => r.precondition.unit = None,
                "authority" => b.deny = true,
                _ => unreachable!(),
            }
            let result = activate(home.path(), &r, &mut b);
            if control == "authority" {
                assert!(result.is_err());
            } else {
                assert_eq!(result.unwrap().verdict, Verdict::Refused, "{control}");
            }
            assert!(b.installs.is_empty(), "{control}");
        }
    }
    #[test]
    fn failed_start_identity_and_authenticated_health_restore_exact_old_unit() {
        for reason in [
            "failed start",
            "wrong executable",
            "wrong vault",
            "stale frontier",
        ] {
            let home = aterm_tempfile::tempdir().unwrap();
            let (r, mut b) = setup();
            b.new_failure = Some(reason);
            let out = activate(home.path(), &r, &mut b).unwrap();
            assert_eq!(out.verdict, Verdict::RolledBack);
            assert_eq!(out.detail, reason);
            assert_eq!(b.installs, ["new", "old"]);
            assert_eq!(b.observed, r.precondition);
            assert_eq!(activate(home.path(), &r, &mut b).unwrap(), out);
            assert_eq!(b.installs.len(), 2);
        }
    }
    #[test]
    fn restart_rolls_back_prepared_journal_before_next_activation() {
        let home = aterm_tempfile::tempdir().unwrap();
        let (r, mut b) = setup();
        write(
            home.path(),
            &active(home.path(), r.job),
            &json!({"schema":1,"phase":"prepared","request":request_value(&r)}),
        )
        .unwrap();
        b.observed.unit = Some(r.unit.clone());
        let out = recover(home.path(), r.job, &mut b).unwrap().unwrap();
        assert_eq!(out.verdict, Verdict::RolledBack);
        assert_eq!(b.installs, ["old"]);
        assert_eq!(b.observed, r.precondition);
        assert_eq!(activate(home.path(), &r, &mut b).unwrap(), out);
        assert_eq!(b.installs, ["old"]);
    }
    #[test]
    fn failed_rollback_is_durable_and_blocks_new_activation() {
        let home = aterm_tempfile::tempdir().unwrap();
        let (mut r, mut b) = setup();
        b.new_failure = Some("wrong vault");
        b.rollback_bad = true;
        assert_eq!(
            activate(home.path(), &r, &mut b).unwrap().verdict,
            Verdict::RollbackFailed
        );
        r.nonce = "differentnonce0123".into();
        assert!(
            activate(home.path(), &r, &mut b)
                .unwrap_err()
                .to_string()
                .contains("failed rollback")
        );
        assert_eq!(b.installs, ["new", "old"]);
        assert_eq!(
            recover(home.path(), r.job, &mut b)
                .unwrap()
                .unwrap()
                .verdict,
            Verdict::RollbackFailed
        );
    }
    #[test]
    fn truncated_journal_and_nonce_paths_fail_closed() {
        let home = aterm_tempfile::tempdir().unwrap();
        let (mut r, mut b) = setup();
        r.nonce = "../../escape012345".into();
        assert!(activate(home.path(), &r, &mut b).is_err());
        crate::fsx::ensure_private_dir(&root(home.path())).unwrap();
        crate::fsx::write_atomic(&active(home.path(), r.job), b"{", 0o600).unwrap();
        assert!(recover(home.path(), r.job, &mut b).is_err());
        assert!(b.installs.is_empty());
    }

    #[test]
    fn interrupted_recovery_preserves_later_off_pin_and_unit_changes() {
        for control in ["off", "pin", "unit", "removed"] {
            let home = aterm_tempfile::tempdir().unwrap();
            let (r, mut b) = setup();
            write(
                home.path(),
                &active(home.path(), r.job),
                &json!({"schema":1,"phase":"prepared","request":request_value(&r)}),
            )
            .unwrap();
            match control {
                "off" => b.observed.off = true,
                "pin" => b.observed.pin = Some("owner pin".into()),
                "unit" => b
                    .observed
                    .unit
                    .as_mut()
                    .unwrap()
                    .definition
                    .push_str(" owner change"),
                "removed" => b.observed.unit = None,
                _ => unreachable!(),
            }
            let before = b.observed.clone();
            assert!(recover(home.path(), r.job, &mut b).is_err(), "{control}");
            assert_eq!(before, b.observed);
            assert!(b.installs.is_empty());
            assert_eq!(
                read(&active(home.path(), r.job)).unwrap().unwrap()["phase"].as_str(),
                Some("prepared")
            );
        }
    }

    #[test]
    fn prepared_directory_flush_failure_never_stops_service() {
        let home = aterm_tempfile::tempdir().unwrap();
        let (r, mut b) = setup();
        crate::fsx::ensure_private_dir(&root(home.path())).unwrap();
        // Four ancestor flushes pass; the atomic journal's directory flush fails
        // after its rename. The prepared file exists, but activation must not start.
        let failure = aterm_durable::seam::FailingDirs::after(4, aterm_durable::seam::EIO);
        assert!(activate(home.path(), &r, &mut b).is_err());
        assert!(b.installs.is_empty());
        assert_eq!(b.observed, r.precondition);
        drop(failure);
        assert_eq!(
            read(&active(home.path(), r.job)).unwrap().unwrap()["phase"].as_str(),
            Some("prepared")
        );
        assert_eq!(
            recover(home.path(), r.job, &mut b)
                .unwrap()
                .unwrap()
                .verdict,
            Verdict::RolledBack
        );
    }

    #[test]
    fn revocation_during_health_proof_refuses_before_journaled_restart() {
        let home = aterm_tempfile::tempdir().unwrap();
        let (r, mut b) = setup();
        b.revoke_after_baseline = true;
        let out = activate(home.path(), &r, &mut b).unwrap();
        assert_eq!(out.verdict, Verdict::Refused);
        assert_eq!(out.detail, "grant revoked");
        assert!(b.installs.is_empty());
    }

    #[test]
    fn revoked_remote_recovery_retains_prepared_journal_without_restart() {
        let home = aterm_tempfile::tempdir().unwrap();
        let (r, mut b) = setup();
        write(
            home.path(),
            &active(home.path(), r.job),
            &json!({"schema":1,"phase":"prepared","request":request_value(&r)}),
        )
        .unwrap();
        let result = recover_checked(home.path(), r.job, &mut b, || {
            Err(err("transport key revoked"))
        });
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("transport key revoked")
        );
        assert!(b.installs.is_empty());
        assert_eq!(
            read(&active(home.path(), r.job)).unwrap().unwrap()["phase"].as_str(),
            Some("prepared")
        );
    }

    #[test]
    fn explicit_recovery_unblocks_new_activation_without_rewriting_original_nonce() {
        let home = aterm_tempfile::tempdir().unwrap();
        let (r, mut b) = setup();
        b.new_failure = Some("failed start");
        b.rollback_bad = true;
        let original = activate(home.path(), &r, &mut b).unwrap();
        assert_eq!(original.verdict, Verdict::RollbackFailed);
        b.rollback_bad = false;
        b.new_failure = None;
        let recovered = recover(home.path(), r.job, &mut b).unwrap().unwrap();
        assert_eq!(recovered.verdict, Verdict::RolledBack);
        assert_eq!(b.observed, r.precondition);
        let installs = b.installs.len();
        assert_eq!(activate(home.path(), &r, &mut b).unwrap(), original);
        assert_eq!(b.installs.len(), installs);
        assert_eq!(
            recover(home.path(), r.job, &mut b).unwrap(),
            Some(recovered)
        );
        assert_eq!(b.installs.len(), installs);
        let mut next = r.clone();
        next.nonce = "next-nonce-123456".into();
        assert_eq!(
            activate(home.path(), &next, &mut b).unwrap().verdict,
            Verdict::Committed
        );
        assert_eq!(
            parse_outcome(&read(&receipt(home.path(), &r)).unwrap().unwrap()).unwrap(),
            original
        );
    }

    #[test]
    fn interrupted_recovery_retains_failure_receipt_and_rechecks_owner_policy() {
        let home = aterm_tempfile::tempdir().unwrap();
        let (r, mut b) = setup();
        b.new_failure = Some("wrong vault");
        b.rollback_bad = true;
        let original = activate(home.path(), &r, &mut b).unwrap();
        write(
            home.path(),
            &active(home.path(), r.job),
            &json!({"schema":1,"phase":"recovering",
            "request":request_value(&r),"original_result":outcome_value(&original)}),
        )
        .unwrap();
        b.rollback_bad = false;
        b.new_failure = None;
        b.observed.off = true;
        let installs = b.installs.len();
        assert!(recover(home.path(), r.job, &mut b).is_err());
        assert_eq!(b.installs.len(), installs);
        b.observed.off = false;
        assert_eq!(
            recover(home.path(), r.job, &mut b)
                .unwrap()
                .unwrap()
                .verdict,
            Verdict::RolledBack
        );
        assert_eq!(activate(home.path(), &r, &mut b).unwrap(), original);
    }

    #[test]
    fn journal_writer_never_replaces_readable_state_with_an_oversized_record() {
        let home = aterm_tempfile::tempdir().unwrap();
        let path = active(home.path(), Job::Server);
        let empty = json!({"padding":""});
        let overhead = aterm_json::to_vec(&empty).unwrap().len();
        let edge = json!({"padding":"x".repeat(MAX_JOURNAL_BYTES-overhead)});
        write(home.path(), &path, &edge).unwrap();
        assert_eq!(read(&path).unwrap(), Some(edge.clone()));
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len(), MAX_JOURNAL_BYTES);
        let over = json!({"padding":"x".repeat(MAX_JOURNAL_BYTES-overhead+1)});
        assert!(write(home.path(), &path, &over).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let nested = json!({"schema":1,"phase":"recovering","original_result":edge,"request":over});
        assert!(write(home.path(), &path, &nested).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(read(&path).is_ok());
    }

    #[test]
    fn oversized_local_request_cannot_poison_an_existing_transaction() {
        let home = aterm_tempfile::tempdir().unwrap();
        let (mut r, mut b) = setup();
        let previous = activate(home.path(), &r, &mut b).unwrap();
        let path = active(home.path(), r.job);
        let bytes = std::fs::read(&path).unwrap();
        let installs = b.installs.len();
        r.nonce = "oversized-request-123456".into();
        r.unit.definition = "x".repeat(MAX_REQUEST_BYTES);
        assert!(activate(home.path(), &r, &mut b).is_err());
        assert_eq!(b.installs.len(), installs);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(recover(home.path(), r.job, &mut b).unwrap(), Some(previous));
    }

    #[test]
    fn recovery_refuses_readable_records_outside_current_bounds_before_mutation() {
        for kind in [
            "prepared-request",
            "terminal-detail",
            "original-detail",
            "receipt-detail",
        ] {
            let home = aterm_tempfile::tempdir().unwrap();
            let (mut r, mut b) = setup();
            let mut original = Outcome {
                request: r.clone(),
                verdict: Verdict::RollbackFailed,
                detail: "failed".into(),
            };
            let value = match kind {
                "prepared-request" => {
                    r.unit.definition = "x".repeat(MAX_REQUEST_BYTES);
                    json!({"schema":1,"phase":"prepared","request":request_value(&r)})
                }
                "terminal-detail" => {
                    original.detail = "\0".repeat(MAX_DETAIL_BYTES + 1);
                    outcome_value(&original)
                }
                "original-detail" => {
                    original.detail = "\0".repeat(MAX_DETAIL_BYTES + 1);
                    json!({"schema":1,"phase":"recovering","request":request_value(&r),"original_result":outcome_value(&original)})
                }
                "receipt-detail" => {
                    let value = outcome_value(&original);
                    original.detail = "\0".repeat(MAX_DETAIL_BYTES + 1);
                    write(
                        home.path(),
                        &receipt(home.path(), &r),
                        &outcome_value(&original),
                    )
                    .unwrap();
                    value
                }
                _ => unreachable!(),
            };
            let path = active(home.path(), r.job);
            write(home.path(), &path, &value).unwrap();
            let before = std::fs::read(&path).unwrap();
            let failure = recover(home.path(), r.job, &mut b).unwrap_err();
            assert!(
                failure.to_string().contains("current supported"),
                "{kind}: {failure}"
            );
            assert!(b.installs.is_empty(), "{kind}");
            assert_eq!(std::fs::read(&path).unwrap(), before, "{kind}");
        }
    }

    #[test]
    fn huge_failure_after_mutation_keeps_a_durable_bounded_terminal_outcome() {
        for rollback_bad in [false, true] {
            let home = aterm_tempfile::tempdir().unwrap();
            let (r, mut b) = setup();
            b.huge_failure = true;
            b.rollback_bad = rollback_bad;
            let outcome = activate(home.path(), &r, &mut b).unwrap();
            assert_eq!(
                outcome.verdict,
                if rollback_bad {
                    Verdict::RollbackFailed
                } else {
                    Verdict::RolledBack
                }
            );
            assert_eq!(b.installs, ["new", "old"]);
            assert!(outcome.detail.len() <= MAX_DETAIL_BYTES);
            assert!(outcome.detail.ends_with("[diagnostic truncated]"));
            let durable =
                parse_outcome(&read(&active(home.path(), r.job)).unwrap().unwrap()).unwrap();
            assert_eq!(durable, outcome);
            assert_eq!(activate(home.path(), &r, &mut b).unwrap(), outcome);
            assert_eq!(b.installs.len(), 2);
            if rollback_bad {
                assert!(outcome.detail.contains("; rollback:"));
                b.huge_failure = false;
                b.rollback_bad = false;
                let recovered = recover(home.path(), r.job, &mut b).unwrap().unwrap();
                assert_eq!(recovered.verdict, Verdict::RolledBack);
                let active = read(&active(home.path(), r.job)).unwrap().unwrap();
                assert_eq!(original_result(&active, &r).unwrap(), Some(outcome.clone()));
                assert!(aterm_json::to_vec(&active).unwrap().len() <= MAX_JOURNAL_BYTES);
                assert_eq!(activate(home.path(), &r, &mut b).unwrap(), outcome);
            }
        }
    }
}
