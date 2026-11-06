// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native API-key preparation from the current signed account decision. Local
//! records are evidence only; every use rereads root and execution authority.

use super::*;
use aterm_agent::harness::native_keys::{self, Access, Inputs, Provider};
use aterm_json::{Value, json};
use aterm_secrets::commons::accounts::Action;
use aterm_secrets::{fsx, text::Audience};
use native_keys::policy::{Assignment, ENTRY, Policy, Scope, Subject};
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[path = "shared_native_allocation.rs"]
mod allocation;
#[path = "shared_native_input.rs"]
mod input;

const RECORDS: &str = "native-account-preparations.json";
const SELECTION: &str = "native-account-selection.json";
static PREPARATIONS: OnceLock<Mutex<native_keys::PreparationQueue>> = OnceLock::new();
static INPUT_FIRST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// How long one native step (a policy write, a key intake, a launch's checks, a preparation
/// pass) waits on custody, the vault and Commons: aterm-agent's check budget in product. In
/// this crate's tests every holder a step races is the test's own, which releases it, so
/// there the wait is the one-minute hang detector: a loaded parallel run starved
/// `set_policy` past 10 s and refused it ("application pass deadline expired", "vault
/// publication deadline expired").
#[cfg(not(test))]
const STEP_BUDGET: Duration = native_keys::CHECK_BUDGET;
#[cfg(test)]
const STEP_BUDGET: Duration = aterm_spec::wait::MUST_HAPPEN;

struct Observation {
    subject: Subject,
    provider: Provider,
    network: [u8; 32],
    generation: u64,
    configuration: [u8; 32],
    input: Inputs,
}

fn check_time(deadline: Instant) -> Result<(), String> {
    if Instant::now() >= deadline {
        Err("native preparation deadline expired".into())
    } else {
        Ok(())
    }
}

fn observation(approved: &Approved, deadline: Instant) -> Result<Observation, String> {
    #[cfg(test)]
    tests::OBSERVATIONS.with(|count| count.set(count.get() + 1));
    check_time(deadline)?;
    if client::config(&approved.paths)
        .map_err(|e| e.to_string())?
        .as_ref()
        != Some(&approved.expected)
    {
        return Err("native configured authority changed".into());
    }
    let prepared = Activations::at_until(approved.paths.clone(), deadline)
        .prepare(approved.account, &approved.expected)
        .map_err(|e| e.to_string())?;
    if prepared.decision().id() != approved.decision {
        return Err("native root decision changed".into());
    }
    let value = prepared.configuration().account();
    let provider = value["provider"]
        .as_str()
        .and_then(Provider::of)
        .ok_or("unsupported native provider")?;
    if value["method"].as_str() != Some("api-key")
        || requirement(value, "login-input-format")?.is_some_and(|format| format != "api-key-v1")
    {
        return Err("native preparation requires explicit api-key-v1 inputs".into());
    }
    if has_unsupported_requirements(value, &["login-input-format"])
        || value.get("realm").is_some_and(|v| !v.is_null())
        || value
            .get("auth")
            .is_some_and(|v| v.as_array().is_none_or(|v| !v.is_empty()))
    {
        return Err("native key preparation has unsupported route or identity requirements".into());
    }
    let input = Inputs::decode(
        prepared
            .credential()
            .ok_or("native preparation has no bound credential")?
            .value()
            .as_slice(),
        provider,
    )?;
    let record = prepared.decision();
    let Action::Prepare(terms) = &record.request().action else {
        return Err("native decision is not a preparation".into());
    };
    let subject = match terms.intent {
        Some(ref intent) => Subject {
            scope: Scope::Intent,
            account: intent.account,
            revision: intent.decision,
            provider,
        },
        None => Subject {
            scope: Scope::Preparation,
            account: approved.account,
            revision: approved.decision,
            provider,
        },
    };
    let trust = record.trust();
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm/shared-native-network/1\0");
    hash.update(trust.network);
    hash.update(trust.authority);
    hash.update(trust.genesis);
    check_time(deadline)?;
    Ok(Observation {
        subject,
        provider,
        network: hash.finalize(),
        generation: record.request().generation,
        configuration: prepared.configuration().digest(),
        input,
    })
}

fn current(
    approved: &Approved,
    expected: &Observation,
    config: &Path,
    deadline: Instant,
) -> Result<Observation, String> {
    if !native_keys::preparation_enabled(config)? {
        return Err("automatic native preparation is off".into());
    }
    let now = observation(approved, deadline)?;
    if now.subject != expected.subject
        || now.network != expected.network
        || now.generation != expected.generation
        || now.configuration != expected.configuration
    {
        return Err("native preparation changed".into());
    }
    // Both observations are decoded from currently authenticated, root-bound bytes.
    if !expected.input.same_bytes(&now.input) {
        return Err("native credential changed".into());
    }
    Ok(now)
}

fn with_custody<T>(
    paths: &Paths,
    expected: &Config,
    deadline: Instant,
    read: impl FnOnce(&Paths, &Config) -> Result<T, String>,
) -> Result<T, String> {
    check_time(deadline)?;
    if client::config(paths).map_err(|e| e.to_string())?.as_ref() != Some(expected) {
        return Err("native configured authority changed".into());
    }
    let selected = aterm_db::application_paths_until(paths, deadline).map_err(|e| e.to_string())?;
    let active = client::config(&selected)
        .map_err(|e| e.to_string())?
        .ok_or("native active vault is unavailable")?;
    if active.name != expected.name || !client::pin_admitted(&active, expected.signer) {
        return Err("native active vault has another authority".into());
    }
    let result = read(&selected, &active)?;
    if client::config(paths).map_err(|e| e.to_string())?.as_ref() != Some(expected)
        || client::config(&selected)
            .map_err(|e| e.to_string())?
            .as_ref()
            != Some(&active)
        || aterm_db::application_paths_until(paths, deadline).map_err(|e| e.to_string())?
            != selected
    {
        return Err("native custody changed during observation".into());
    }
    check_time(deadline)?;
    Ok(result)
}

#[cfg(test)]
fn policy(
    paths: &Paths,
    expected: &Config,
) -> Result<Option<(Policy, aterm_secrets::client::EntrySnapshot)>, String> {
    policy_at(paths, expected, Instant::now() + STEP_BUDGET)
}

fn policy_at(
    paths: &Paths,
    expected: &Config,
    deadline: Instant,
) -> Result<Option<(Policy, aterm_secrets::client::EntrySnapshot)>, String> {
    if expected.signer.is_none() {
        return Ok(None);
    }
    with_custody(paths, expected, deadline, |paths, active| {
        client::read_verified_entry(paths, active, ENTRY)
            .map_err(|e| e.to_string())?
            .map(|entry| {
                let raw = entry
                    .value()
                    .as_str()
                    .ok_or("native execution policy is not text")?;
                Ok((Policy::parse(raw)?, entry.snapshot().clone()))
            })
            .transpose()
    })
}

fn member(paths: &Paths, deadline: Instant) -> Result<String, String> {
    let expected = client::config(paths)
        .map_err(|e| e.to_string())?
        .ok_or("native authority is unavailable")?;
    with_custody(paths, &expected, deadline, |paths, _| {
        let recipient = client::read_machine_identity(paths)
            .map_err(|e| e.to_string())?
            .ok_or("native execution has no local identity")?
            .recipient()
            .to_bech32();
        let roster = client::swarm_roster_until(paths, deadline)
            .map_err(|e| e.to_string())?
            .ok_or("native execution needs current swarm membership")?;
        let row = roster
            .member_by_recipient(&recipient)
            .ok_or("native execution member is unbound")?;
        if roster.is_member_denied(row) {
            return Err("native execution member is denied".into());
        }
        if roster
            .members
            .iter()
            .filter(|other| other.label == row.label && !roster.is_member_denied(other))
            .map(|other| other.recipient.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != 1
        {
            return Err("native execution member label is ambiguous".into());
        }
        Ok(row.label.clone())
    })
}

fn permits(
    approved: &Approved,
    expected: &Observation,
    config: &Path,
    deadline: Instant,
) -> Result<(), String> {
    let now = current(approved, expected, config, deadline)?;
    let Some((policy, _)) = policy_at(&approved.paths, &approved.expected, deadline)? else {
        return Err("native execution is held: no signed assignment".into());
    };
    let who = member(&approved.paths, deadline)?;
    if !policy.permits(now.subject, &who) {
        return Err("native execution is held: this member has no current warm slot".into());
    }
    current(approved, expected, config, deadline)?;
    check_time(deadline)?;
    Ok(())
}

fn candidates(home: &Path, deadline: Instant) -> Result<Vec<Approved>, String> {
    let paths = Paths::for_home(home);
    let Some(expected) = client::config(&paths).map_err(|e| e.to_string())? else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (account, decision) in Activations::at_until(paths.clone(), deadline)
        .local_preparations()
        .map_err(|e| e.to_string())?
    {
        let approved = Approved {
            paths: paths.clone(),
            expected: expected.clone(),
            account,
            decision,
        };
        if observation(&approved, deadline).is_ok() {
            out.push(approved);
        }
    }
    check_time(deadline)?;
    Ok(out)
}

fn records(paths: &Paths) -> Result<Vec<Value>, String> {
    let Some(raw) =
        fsx::read_private(&paths.dir.join(RECORDS), 256 * 1024).map_err(|e| e.to_string())?
    else {
        return Ok(Vec::new());
    };
    let mut value: Value = aterm_json::from_slice(raw.as_slice())
        .map_err(|_| "native preparation records are malformed")?;
    if value.as_object().is_none_or(|object| object.len() != 2)
        || value["schema"].as_u64() != Some(1)
    {
        return Err("unsupported native preparation record schema".into());
    }
    let Some(Value::Array(rows)) = value
        .as_object_mut()
        .and_then(|object| object.remove("records"))
    else {
        return Err("native preparation records are not an array".into());
    };
    let mut seen = std::collections::BTreeSet::new();
    for row in &rows {
        if row.as_object().is_none_or(|object| object.len() != 12)
            || ["account", "network", "decision", "configuration"]
                .iter()
                .any(|key| {
                    row[*key]
                        .as_str()
                        .is_none_or(|v| super::digest::<32>(v).is_err())
                })
            || row["request"]
                .as_str()
                .is_none_or(|v| super::digest::<16>(v).is_err())
            || row["generation"].as_u64().is_none_or(|v| v == 0)
            || row["provider"].as_str().and_then(Provider::of).is_none()
            || !matches!(row["state"].as_str(), Some("pending" | "access-verified"))
            || row["observed_at_unix"].as_u64().is_none()
            || row["current_validity"].as_str() != Some("unknown")
            || row["evidence"].as_str()
                != Some(if row["state"].as_str() == Some("access-verified") {
                    "historical-credential-access-only"
                } else {
                    "no-access-proof"
                })
            || row["execution"].as_str() != Some("requires-current-signed-assignment")
            || !seen.insert((row["network"].as_str(), row["account"].as_str()))
        {
            return Err("native preparation record is invalid".into());
        }
    }
    Ok(rows)
}

fn selection_scope(paths: &Paths, deadline: Instant) -> Result<Value, String> {
    let expected = client::config(paths)
        .map_err(|e| e.to_string())?
        .ok_or("native authority is unavailable")?;
    with_custody(paths, &expected, deadline, |paths, _| {
        let bootstrap =
            aterm_secrets::commons::bootstrap::Bootstrap::required_until(paths, deadline)
                .map_err(|e| e.to_string())?;
        let trust = bootstrap.trust();
        let request = bootstrap.request().map_err(|e| e.to_string())?;
        let binding = request.binding();
        Ok(
            json!({"network":aterm_codec::hex::encode_lower(&trust.network),
        "authority":aterm_codec::hex::encode_lower(&trust.authority),
        "genesis":aterm_codec::hex::encode_lower(&trust.genesis),
        "actor":aterm_codec::hex::encode_lower(&binding.actor()),"recipient":binding.recipient().to_bech32()}),
        )
    })
}

fn selections(paths: &Paths) -> Result<Vec<Value>, String> {
    let Some(raw) =
        fsx::read_private(&paths.dir.join(SELECTION), 16 * 1024).map_err(|e| e.to_string())?
    else {
        return Ok(Vec::new());
    };
    let mut value: Value =
        aterm_json::from_slice(raw.as_slice()).map_err(|_| "native selections are malformed")?;
    if value.as_object().is_none_or(|o| o.len() != 2) || value["schema"].as_u64() != Some(1) {
        return Err("native selection schema is unsupported".into());
    }
    let Some(Value::Array(rows)) = value
        .as_object_mut()
        .and_then(|o| o.remove("selections"))
        .filter(|v| v.as_array().is_some_and(|rows| rows.len() <= 2))
    else {
        return Err("native selections exceed their bound".into());
    };
    for row in &rows {
        if row.as_object().is_none_or(|o| o.len() != 2)
            || row["provider"].as_str().and_then(Provider::of).is_none()
            || row["scope"].as_object().is_none_or(|o| o.len() != 5)
            || row["scope"]["network"]
                .as_str()
                .is_none_or(|s| super::digest::<16>(s).is_err())
            || ["authority", "genesis"].iter().any(|key| {
                row["scope"][*key]
                    .as_str()
                    .is_none_or(|s| super::digest::<32>(s).is_err())
            })
            || row["scope"]["actor"].as_str().is_none_or(|s| {
                super::digest::<{ std::mem::size_of::<aterm_secrets::membership_log::MemberId>() }>(
                    s,
                )
                .is_err()
            })
            || row["scope"]["recipient"]
                .as_str()
                .is_none_or(|s| s.is_empty())
        {
            return Err("native selection is invalid".into());
        }
    }
    Ok(rows)
}

fn retain_selection(paths: &Paths, provider: Provider, scope: &Value) -> Result<(), String> {
    let _lock = aterm_uds::private_file::PrivateLock::take(
        &paths.dir,
        "native-account-selection.lock",
        Duration::ZERO,
        "native selections are busy",
    )
    .map_err(|e| e.to_string())?;
    let mut rows = selections(paths)?;
    let row = json!({"provider":provider.name(),"scope":scope});
    if let Some(existing) = rows
        .iter_mut()
        .find(|row| row["provider"].as_str() == Some(provider.name()))
    {
        if *existing == row {
            return Ok(());
        }
        *existing = row;
    } else {
        rows.push(row);
    }
    let bytes = aterm_json::to_vec(&json!({"schema":1,"selections":rows}))
        .map_err(|_| "native selection could not be encoded")?;
    fsx::write_atomic(&paths.dir.join(SELECTION), &bytes, 0o600).map_err(|e| e.to_string())
}

fn retain(
    approved: &Approved,
    observed: &Observation,
    config: &Path,
    status: &str,
    deadline: Instant,
) -> Result<(), String> {
    let _lock = aterm_uds::private_file::PrivateLock::take(
        &approved.paths.dir,
        "native-account-preparations.lock",
        Duration::ZERO,
        "native preparation records are busy",
    )
    .map_err(|e| e.to_string())?;
    let _ = current(approved, observed, config, deadline)?;
    let mut rows = records(&approved.paths)?;
    let account = aterm_codec::hex::encode_lower(&approved.account);
    let network = aterm_codec::hex::encode_lower(&observed.network);
    let same = |r: &Value| {
        r["account"].as_str() == Some(account.as_str())
            && r["network"].as_str() == Some(network.as_str())
    };
    let previous = rows.iter().find(|r| same(r));
    if previous
        .and_then(|r| r["generation"].as_u64())
        .is_some_and(|generation| generation > observed.generation)
    {
        return Err("native preparation is older than retained evidence".into());
    }
    let decision = aterm_codec::hex::encode_lower(&approved.decision);
    let request = match previous
        .filter(|r| r["decision"].as_str() == Some(decision.as_str()))
        .and_then(|r| r["request"].as_str())
    {
        Some(request) => request.to_owned(),
        None => {
            let mut request = [0; 16];
            aterm_uds::rand::fill(&mut request)
                .map_err(|_| "native preparation request could not be created")?;
            aterm_codec::hex::encode_lower(&request)
        }
    };
    if request == "00000000000000000000000000000000" {
        return Err("native preparation request could not be created".into());
    }
    let row = json!({"account":account,"network":network,"decision":decision,
        "configuration":aterm_codec::hex::encode_lower(&observed.configuration),"generation":observed.generation,
        "provider":observed.provider.name(),"request":request,"state":status,"observed_at_unix":aterm_time::unix_secs().unwrap_or(0),"current_validity":"unknown","evidence":if status=="access-verified" {"historical-credential-access-only"}else{"no-access-proof"},"execution":"requires-current-signed-assignment"});
    if let Some(old) = rows.iter_mut().find(|r| same(r)) {
        *old = row;
    } else {
        rows.push(row);
    }
    let bytes = aterm_json::to_vec(&json!({"schema":1,"records":rows}))
        .map_err(|_| "native records could not be encoded")?;
    fsx::write_atomic(&approved.paths.dir.join(RECORDS), &bytes, 0o600).map_err(|e| e.to_string())
}

fn prepare_one(
    approved: &Approved,
    config: &Path,
    probe: &mut dyn native_keys::Probe,
    deadline: Instant,
) -> Result<Access, String> {
    if !native_keys::preparation_enabled(config)? {
        return Err("automatic native preparation is off".into());
    }
    let observed = observation(approved, deadline)?;
    retain(approved, &observed, config, "pending", deadline)?;
    let input = observation(approved, deadline)?.input;
    let access = Access::check(input, probe, deadline, || {
        current(approved, &observed, config, deadline).map(|_| ())
    })?;
    let now = current(approved, &observed, config, deadline)?;
    if !access.matches(&now.input) {
        return Err("native credential changed after access proof".into());
    }
    retain(approved, &observed, config, "access-verified", deadline)?;
    Ok(access)
}

fn discover(
    home: &Path,
    _state: &Path,
    config: &Path,
    admit: &mut dyn FnMut(&native_keys::PreparationId) -> bool,
) -> Result<Option<native_keys::PreparationTask>, String> {
    if !native_keys::preparation_enabled(config)? {
        return Ok(None);
    }
    let candidates = candidates(home, Instant::now() + STEP_BUDGET)?;
    let selected = select_preparation(
        &mut PREPARATIONS
            .get_or_init(|| Mutex::new(native_keys::PreparationQueue::default()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        home,
        candidates,
        Instant::now(),
        admit,
    );
    if let Some(approved) = selected {
        let config = config.to_owned();
        return Ok(Some(native_keys::PreparationTask {
            preparation: preparation_id(approved.account, approved.decision),
            run: Box::new(move || {
                prepare_one(
                    &approved,
                    &config,
                    &mut native_keys::LiveProbe,
                    Instant::now() + STEP_BUDGET,
                )
                .map(|_| ())
            }),
        }));
    }
    Ok(None)
}

fn select_preparation(
    queue: &mut native_keys::PreparationQueue,
    home: &Path,
    candidates: Vec<Approved>,
    now: Instant,
    admit: &mut dyn FnMut(&native_keys::PreparationId) -> bool,
) -> Option<Approved> {
    let revisions = candidates
        .iter()
        .map(|candidate| (candidate.account, candidate.decision))
        .collect();
    let selected = queue.pick_revisions_if(home, &revisions, now, |account, revision| {
        admit(&preparation_id(account, revision))
    })?;
    candidates
        .into_iter()
        .find(|candidate| (candidate.account, candidate.decision) == selected)
}

fn preparation_id(account: [u8; 32], revision: [u8; 32]) -> native_keys::PreparationId {
    native_keys::PreparationId {
        key: preparation_key(account),
        revision,
    }
}

fn preparation_key(account: [u8; 32]) -> String {
    format!("native:{}", aterm_codec::hex::encode_lower(&account))
}

fn launcher(provider: Provider, config: &Path) -> Result<Option<PathBuf>, String> {
    if !native_keys::preparation_enabled(config)?
        || std::env::vars_os().any(|(name, _)| {
            name.to_str()
                .is_none_or(|name| native_keys::explicit_env(provider, name))
        })
    {
        return Ok(None);
    }
    let paths = Paths::live().map_err(|e| e.to_string())?;
    if !selected_launch(&paths, config, provider, Instant::now() + STEP_BUDGET)? {
        return Ok(None);
    }
    std::env::current_exe()
        .map(Some)
        .map_err(|_| "native launcher executable is unavailable".into())
}

fn selected_launch(
    paths: &Paths,
    local_config: &Path,
    provider: Provider,
    deadline: Instant,
) -> Result<bool, String> {
    if !native_keys::preparation_enabled(local_config)? {
        return Ok(false);
    }
    let Some(config) = client::config(paths).map_err(|e| e.to_string())? else {
        return Ok(false);
    };
    let selections = selections(paths)?;
    let policy = policy_at(paths, &config, deadline)?;
    if !selections
        .iter()
        .any(|row| row["provider"].as_str() == Some(provider.name()))
        && policy.as_ref().is_none_or(|(policy, _)| {
            !policy
                .assignments
                .iter()
                .any(|row| row.subject.provider == provider)
        })
    {
        return Ok(false);
    }
    let scope = selection_scope(paths, deadline)?;
    let retained = selections
        .iter()
        .any(|row| row["provider"].as_str() == Some(provider.name()) && row["scope"] == scope);
    if let Some((policy, _)) = policy {
        let who = member(paths, deadline)?;
        if policy.assigned(provider, &who)?.is_some() {
            let approved = selected(&paths.home, provider, deadline)?;
            let observed = observation(&approved, deadline)?;
            permits(&approved, &observed, local_config, deadline)?;
            retain_selection(paths, provider, &scope)?;
            return Ok(true);
        }
        if policy.automatic(provider)?.is_some() {
            retain_selection(paths, provider, &scope)?;
            return Ok(true);
        }
    }
    Ok(retained)
}

pub(super) fn install() {
    native_keys::install(discover, launcher);
}

pub(super) fn root_upkeep(
    paths: &Paths,
    admin: &client::Admin,
    deadline: Instant,
) -> Result<usize, String> {
    let env = cli::Env::from_process()?;
    let Some(config) = env.config.as_deref() else {
        return Ok(0);
    };
    if !native_keys::preparation_enabled(config)? {
        return Ok(0);
    }
    // One slow bounded publication must not consume every later allocation pass.
    let (input, allocation) = if INPUT_FIRST.fetch_xor(true, std::sync::atomic::Ordering::Relaxed) {
        let input = input::recover_root(paths, admin, config, deadline);
        (input, allocation::step(paths, admin, config, deadline))
    } else {
        let allocation = allocation::step(paths, admin, config, deadline);
        (
            input::recover_root(paths, admin, config, deadline),
            allocation,
        )
    };
    match (input, allocation) {
        (Ok(input), Ok(allocated)) => Ok(usize::from(input) + allocated),
        (Err(error), _) | (_, Err(error)) => Err(error),
    }
}

fn selected(home: &Path, provider: Provider, deadline: Instant) -> Result<Approved, String> {
    let paths = Paths::for_home(home);
    let expected = client::config(&paths)
        .map_err(|e| e.to_string())?
        .ok_or("no configured native authority")?;
    let (policy, _) = policy_at(&paths, &expected, deadline)?
        .ok_or("native execution is held: no signed assignment")?;
    let subject = policy
        .assigned(provider, &member(&paths, deadline)?)?
        .ok_or("native execution is held: no warm assignment")?;
    let mut matching = candidates(home, deadline)?.into_iter().filter(|approved| {
        observation(approved, deadline).is_ok_and(|observation| observation.subject == subject)
    });
    let approved = matching
        .next()
        .ok_or("native assignment has no current addressed preparation")?;
    if matching.next().is_some() {
        return Err("native addressed preparation is ambiguous".into());
    }
    Ok(approved)
}

fn program(provider: Provider, requested: &Path) -> Result<PathBuf, String> {
    let layout = atpkg::store::resolve_configured().ok_or("native package store is unavailable")?;
    let managed = aterm_agent::ai::facts::managed_of(&layout, provider.connector())
        .ok_or("native connector is not installed")?;
    let canonical =
        std::fs::canonicalize(&managed.path).map_err(|_| "native connector path is unavailable")?;
    if requested != Path::new(provider.connector())
        && std::fs::canonicalize(requested).ok().as_ref() != Some(&canonical)
    {
        return Err("native key cannot be delivered to an unrelated executable".into());
    }
    Ok(managed.path)
}

fn native_args(provider: Provider, args: &[OsString]) -> Result<(), String> {
    let words = args
        .iter()
        .map(|arg| arg.to_str().map(str::to_owned))
        .collect::<Option<Vec<_>>>()
        .ok_or("native key launch arguments are not UTF-8")?;
    if native_keys::default_route(provider, &words) {
        Ok(())
    } else {
        Err("native key launch refuses an overriding or unknown profile or provider route".into())
    }
}

fn child_environment(name: &std::ffi::OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        matches!(
            name,
            "PATH"
                | "TERM"
                | "COLORTERM"
                | "LANG"
                | "LC_ALL"
                | "LC_CTYPE"
                | "TMPDIR"
                | "USER"
                | "LOGNAME"
                | "TZ"
                | "NO_COLOR"
                | "CLICOLOR"
                | "CLICOLOR_FORCE"
        ) || name.starts_with("ATERM_")
    })
}

fn own_profile(provider: Provider, profile: &Path) -> Result<(), String> {
    let parent = profile
        .parent()
        .ok_or("native profile has no private parent")?;
    fsx::ensure_private_dir(parent).map_err(|e| e.to_string())?;
    let marker = profile.join(".aterm-native-owner");
    let existing = match std::fs::create_dir(profile) {
        Ok(()) => {
            fsx::ensure_private_dir(profile).map_err(|e| e.to_string())?;
            fsx::read_private(&marker, 256).map_err(|e| e.to_string())?
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            aterm_uds::private_file::check_private_dir(profile)
                .map_err(|_| "native profile is not an owned private directory")?;
            let existing = fsx::read_private(&marker, 256).map_err(|e| e.to_string())?;
            if existing.is_none()
                && std::fs::read_dir(profile)
                    .map_err(|_| "native profile cannot be inspected")?
                    .next()
                    .is_some()
            {
                return Err("native profile has unrelated existing data".into());
            }
            existing
        }
        Err(_) => return Err("native profile could not be created".into()),
    };
    let expected = format!("aterm-native-profile-1:{}\n", provider.name());
    if let Some(bytes) = existing {
        if bytes.as_slice() != expected.as_bytes() {
            return Err("native profile belongs to a different provider".into());
        }
    } else if !fsx::write_new(&marker, expected.as_bytes(), 0o600).map_err(|e| e.to_string())? {
        return Err("native profile ownership changed".into());
    }
    if provider == Provider::Google {
        let data = profile.join(aterm_agent::harness::upgrade_agy::APP_DATA_DIR);
        fsx::ensure_private_dir(&data).map_err(|e| e.to_string())?;
        let settings = data.join("settings.json");
        if let Some(bytes) = fsx::read_owned(&settings, 64 * 1024).map_err(|e| e.to_string())? {
            let value: Value = aterm_json::from_slice(&bytes)
                .map_err(|_| "native API-key settings are malformed")?;
            if value.as_object().is_none_or(|object| object.len() != 1)
                || value["modelProvider"].as_str() != Some("gemini")
            {
                return Err("native profile selected a different authentication method".into());
            }
        } else if !fsx::write_new(&settings, b"{\"modelProvider\":\"gemini\"}\n", 0o600)
            .map_err(|e| e.to_string())?
        {
            return Err("native API-key settings changed".into());
        }
    }
    Ok(())
}

/// `--gemini_dir=<profile>`, the profile's path byte for byte: `display()` would hand the CLI
/// another folder for a path that is not UTF-8.
fn gemini_dir(profile: &Path) -> OsString {
    let mut arg = OsString::from("--gemini_dir=");
    arg.push(profile.as_os_str());
    arg
}

fn child_command(
    provider: Provider,
    program: &Path,
    profile: &Path,
    args: &[OsString],
    environment: impl IntoIterator<Item = (OsString, OsString)>,
) -> Result<aterm_uds::spawnfd::Command, String> {
    native_args(provider, args)?;
    own_profile(provider, profile)?;
    let mut command = aterm_uds::spawnfd::command(program);
    command.env_clear();
    for (name, value) in environment {
        if child_environment(&name) {
            command.env(name, value);
        }
    }
    command.env("HOME", profile);
    match provider {
        Provider::Google => {
            command.arg(gemini_dir(profile));
            command.env(
                "GOOGLE_GEMINI_BASE_URL",
                "https://generativelanguage.googleapis.com",
            );
        }
        Provider::Meta => {
            let data = profile.join("data");
            let config = profile.join("config");
            fsx::ensure_private_dir(&data).map_err(|e| e.to_string())?;
            fsx::ensure_private_dir(&config).map_err(|e| e.to_string())?;
            command
                .env("XDG_DATA_HOME", data)
                .env("XDG_CONFIG_HOME", config);
            if args.first().is_some_and(|arg| arg == "exec") {
                command.arg("exec");
            }
            command.args(["--provider", "meta", "--base-url", "https://api.meta.ai/v1"]);
        }
    }
    command.args(
        if provider == Provider::Meta && args.first().is_some_and(|arg| arg == "exec") {
            &args[1..]
        } else {
            args
        },
    );
    Ok(command)
}

fn run(
    provider: Provider,
    requested: &Path,
    config: &Path,
    args: &[OsString],
) -> Result<std::convert::Infallible, String> {
    let env = cli::Env::from_process()?;
    let home = env.home.as_deref().ok_or("native key launch has no home")?;
    if !config.is_absolute() {
        return Err("native policy path must be absolute".into());
    }
    native_args(provider, args)?;
    let program = program(provider, requested)?;
    let approved = wait_selected(home, provider, config, || {
        std::thread::sleep(Duration::from_secs(1))
    })?;
    let deadline = Instant::now() + STEP_BUDGET;
    let observed = observation(&approved, deadline)?;
    permits(&approved, &observed, config, deadline)?;
    let access = prepare_one(&approved, config, &mut native_keys::LiveProbe, deadline)?;
    let now = current(&approved, &observed, config, deadline)?;
    let profile = approved.paths.dir.join("native-profiles").join(format!(
        "{}-{}",
        aterm_codec::hex::encode_lower(&observed.network),
        aterm_codec::hex::encode_lower(&approved.account)
    ));
    let mut command = child_command(provider, &program, &profile, args, std::env::vars_os())?;
    access.bind_child(&now.input, &mut command, || {
        permits(&approved, &observed, config, deadline)
    })?;
    // No check result is a lease. Reopen both current root bytes and the
    // independently signed execution policy at the final process boundary.
    permits(&approved, &observed, config, deadline)?;
    replace_with_connector(command)
}

// This module is Unix-only: a successful launch replaces the process, so the
// connector itself delivers its exit status. There is no successful return
// value for the command dispatcher to convert into a different status.
fn replace_with_connector(
    mut command: aterm_uds::spawnfd::Command,
) -> Result<std::convert::Infallible, String> {
    Err(format!(
        "native connector did not start: {}",
        command.exec()
    ))
}

fn wait_selected(
    home: &Path,
    provider: Provider,
    config: &Path,
    mut pause: impl FnMut(),
) -> Result<Approved, String> {
    let paths = Paths::for_home(home);
    let mut waiting: Option<(Config, Subject, [u8; 16])> = None;
    loop {
        let deadline = Instant::now() + STEP_BUDGET;
        let (settings, issues) =
            aterm_agent::supervise::config::SupervisorConfig::from_path(Some(config));
        if !issues.is_empty() {
            if waiting.is_none() {
                return Err("native launch configuration is unavailable or invalid".into());
            }
            pause();
            continue;
        }
        if !settings.enabled || settings.accounts == accounts::Mode::Off || !settings.logins {
            return Err("shared native launch was turned off while waiting".into());
        }
        let expected = match client::config(&paths) {
            Ok(Some(expected)) => expected,
            _ if waiting.is_some() => {
                pause();
                continue;
            }
            _ => return Err("native authority is unavailable".into()),
        };
        if waiting
            .as_ref()
            .is_some_and(|(original, _, _)| *original != expected)
        {
            return Err("native configured authority changed while waiting".into());
        }
        let (policy, _) = match policy_at(&paths, &expected, deadline) {
            Ok(Some(policy)) => policy,
            _ if waiting.is_some() => {
                pause();
                continue;
            }
            _ => return Err("native execution is held: no signed policy".into()),
        };
        let automatic = policy.automatic(provider)?;
        if let Some((_, subject, grant)) = waiting
            && automatic.is_none_or(|row| {
                row.subject != subject || row.automatic.as_ref().unwrap().grant != grant
            })
        {
            return Err("automatic native execution was withdrawn or changed".into());
        }
        if let Ok(approved) = selected(home, provider, deadline) {
            return Ok(approved);
        }
        let assignment =
            automatic.ok_or("native execution is held: no current addressed assignment")?;
        waiting = Some((
            expected,
            assignment.subject,
            assignment.automatic.as_ref().unwrap().grant,
        ));
        // An unavailable local writer or a changing vault means wait, never
        // replay provider work or abandon an already established auto request.
        let _ = allocation::request(&paths, &policy, assignment.subject, deadline);
        pause();
    }
}

fn set_policy(home: &Path, account: [u8; 32], warm: Vec<String>) -> Result<Value, String> {
    set_policy_mode(home, account, warm, false)
}

fn set_policy_mode(
    home: &Path,
    account: [u8; 32],
    warm: Vec<String>,
    automatic: bool,
) -> Result<Value, String> {
    let deadline = Instant::now() + STEP_BUDGET;
    let paths = Paths::for_home(home);
    let expected = client::config(&paths)
        .map_err(|e| e.to_string())?
        .ok_or("native authority is unavailable")?;
    let db = Activations::at_until(paths.clone(), deadline);
    let (record, configuration) = db
        .current_configuration(account)
        .map_err(|e| e.to_string())?;
    let value = configuration.account();
    let provider = value["provider"]
        .as_str()
        .and_then(Provider::of)
        .ok_or("native policy needs a supported provider")?;
    if value["method"].as_str() != Some("api-key")
        || requirement(value, "login-input-format")?.is_some_and(|format| format != "api-key-v1")
    {
        return Err("native policy needs an explicit API-key preparation".into());
    }
    let subject = match &record.request().action {
        Action::Intent(_) => Subject {
            scope: Scope::Intent,
            account,
            revision: record.id(),
            provider,
        },
        Action::Prepare(terms) => terms.intent.map_or(
            Subject {
                scope: Scope::Preparation,
                account,
                revision: record.id(),
                provider,
            },
            |intent| Subject {
                scope: Scope::Intent,
                account: intent.account,
                revision: intent.decision,
                provider,
            },
        ),
        Action::Stop => return Err("native account is stopped".into()),
    };
    let allocator = if automatic {
        let binding = with_custody(&paths, &expected, deadline, |active, _| {
            let bootstrap =
                aterm_secrets::commons::bootstrap::Bootstrap::required_until(active, deadline)
                    .map_err(|e| e.to_string())?;
            Ok(bootstrap
                .request()
                .map_err(|e| e.to_string())?
                .binding()
                .clone())
        })?;
        let mut grant = [0; 16];
        aterm_uds::rand::fill(&mut grant).map_err(|_| "native allocation grant is unavailable")?;
        if grant == [0; 16] {
            return Err("native allocation grant is unavailable".into());
        }
        Some(native_keys::policy::Allocator {
            actor: binding.actor(),
            machine: *binding.machine(),
            grant,
        })
    } else {
        None
    };
    let mut assignment = Assignment {
        subject,
        warm,
        automatic: allocator,
    };
    let roster = with_custody(&paths, &expected, deadline, |active, _| {
        client::swarm_roster_until(active, deadline)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "native policy needs current member bindings".into())
    })?;
    let admin = client::admin_cached_until(&paths, deadline).map_err(|e| e.to_string())?;
    let doc = &admin.doc;
    let mut policy = match doc.entry(&Audience::Everyone, ENTRY) {
        Some(entry) => Policy::parse(entry.value.as_str().ok_or("native policy is not text")?)?,
        None => Policy::default(),
    };
    if automatic
        && let Some(previous) = policy.assignments.iter().find(|row| row.subject == subject)
    {
        assignment.warm = previous.warm.clone();
    }
    assignment.validate()?;
    for label in &assignment.warm {
        let recipients: std::collections::BTreeSet<_> = roster
            .members
            .iter()
            .filter(|member| member.label == *label && !roster.is_member_denied(member))
            .map(|member| member.recipient.as_str())
            .collect();
        if recipients.len() != 1 {
            return Err("native warm slot is not a unique current member".into());
        }
    }
    policy.set(assignment)?;
    let policy_text = policy.text();
    let (now, current) = db
        .current_configuration(account)
        .map_err(|e| e.to_string())?;
    if now.id() != record.id() || current.digest() != configuration.digest() {
        return Err("native account changed before policy publication".into());
    }
    let expected = admin
        .native_entry_digest(ENTRY)
        .map_err(|e| e.to_string())?;
    client::admin_set_entry_until(
        &paths,
        &admin,
        ENTRY,
        expected,
        policy_text.as_bytes(),
        deadline,
    )
    .map_err(|e| e.to_string())?;
    Ok(
        json!({"kind":"native-execution-policy","account":aterm_codec::hex::encode_lower(&subject.account),"revision":aterm_codec::hex::encode_lower(&subject.revision),"provider":provider.name(),"scope":subject.scope.word(),"state":"published"}),
    )
}

pub(super) fn try_run(args: &[OsString]) -> Option<ExitCode> {
    if args.first().is_none_or(|arg| arg != "accounts") {
        return None;
    }
    let verb = args.get(1)?.to_str()?;
    if !matches!(
        verb,
        "native-run" | "native-policy" | "native-status" | "native-key"
    ) {
        return None;
    }
    let result = (|| -> Result<Value, String> {
        let paths = Paths::live().map_err(|e| e.to_string())?;
        if verb == "native-key" {
            return input::command(&paths, &args[2..]);
        }
        if verb == "native-status" {
            if args.len() != 2 {
                return Err("native-status takes no options".into());
            }
            return Ok(
                json!({"kind":"historical-native-preparation-observations","authority":"none","current_validity":"unknown","records":records(&paths)?}),
            );
        }
        if verb == "native-run" {
            let provider = args
                .get(2)
                .and_then(|arg| arg.to_str())
                .and_then(Provider::of)
                .ok_or("native-run needs google or meta")?;
            let program = args
                .get(3)
                .ok_or("native-run needs its requested program")?;
            if args.get(4).is_none_or(|arg| arg != "--config")
                || args.get(6).is_none_or(|arg| arg != "--")
            {
                return Err("native-run needs its exact config and argument separator".into());
            }
            let config = Path::new(args.get(5).ok_or("native-run needs its exact config")?);
            match run(provider, Path::new(program), config, &args[7..])? {}
        }
        if args.get(2).is_some_and(|arg| arg == "set")
            && args.len() == 6
            && args[3] == "--account"
            && args[5] == "--automatic"
        {
            let account = super::digest(args[4].to_str().ok_or("native account is not UTF-8")?)?;
            return set_policy_mode(&paths.home, account, Vec::new(), true);
        }
        if args.get(2).is_none_or(|arg| arg != "set")
            || args.len() != 7
            || args[3] != "--account"
            || args[5] != "--warm"
        {
            return Err("usage: accounts native-policy set --account <current root account> --warm <mN,mN|none>".into());
        }
        let account = super::digest(args[4].to_str().ok_or("native account is not UTF-8")?)?;
        let warm = args[6].to_str().ok_or("native warm list is not UTF-8")?;
        let warm = if warm == "none" {
            Vec::new()
        } else {
            warm.split(',').map(str::to_owned).collect()
        };
        set_policy(&paths.home, account, warm)
    })();
    Some(match result {
        Ok(value) => aterm_uds::stdout::answer_exit("aterm", &format!("{value}\n"), 0),
        Err(why) => {
            eprintln!("aterm: {why}");
            ExitCode::FAILURE
        }
    })
}

#[cfg(test)]
#[path = "shared_native_tests.rs"]
mod tests;
