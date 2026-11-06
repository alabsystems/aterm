// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Root-delegated native warm allocation. Queue order is a private hint;
//! current signed policy, preparation and membership authorize every change.

use super::*;
use aterm_db::accounts::native_requests::{Requests, Target};
use aterm_secrets::commons::binding::Binding;
use native_keys::policy::Allocator;

const FILE: &str = "native-allocation-queue.json";
const COOLDOWN: Duration = Duration::from_secs(60);
static DUE: OnceLock<Mutex<BTreeMap<(PathBuf, &'static str), Instant>>> = OnceLock::new();

fn target(subject: Subject, grant: [u8; 16]) -> Target {
    Target {
        intent: subject.scope == Scope::Intent,
        account: subject.account,
        revision: subject.revision,
        provider: subject.provider.name().into(),
        grant,
    }
}
fn digest(policy: &Policy) -> [u8; 32] {
    aterm_digest::Sha256::digest(policy.text().as_bytes())
}

pub(super) fn request(
    paths: &Paths,
    policy: &Policy,
    subject: Subject,
    deadline: Instant,
) -> Result<(), String> {
    let grant = policy
        .automatic(subject.provider)?
        .filter(|row| row.subject == subject)
        .and_then(|row| row.automatic.as_ref())
        .ok_or("native request has no current automatic delegation")?
        .grant;
    Requests::at_until(paths.clone(), deadline)
        .request(&target(subject, grant), digest(policy))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn label(roster: &aterm_secrets::swarm_roster::SwarmRoster, binding: &Binding) -> Option<String> {
    let row = roster.member_by_recipient(&binding.recipient().to_bech32())?;
    (!roster.is_member_denied(row)
        && !roster.members.iter().any(|other| {
            other.label == row.label
                && !roster.is_member_denied(other)
                && other.recipient != row.recipient
        }))
    .then(|| row.label.clone())
}

fn available(
    paths: &Paths,
    config: &Config,
    subject: Subject,
    deadline: Instant,
) -> Result<std::collections::BTreeSet<Binding>, String> {
    let (record, configuration, members) = Activations::at_until(paths.clone(), deadline)
        .current_configuration_members(subject.account)
        .map_err(|e| e.to_string())?;
    if record.id() != subject.revision
        || configuration.account()["provider"].as_str() != Some(subject.provider.name())
        || configuration.account()["method"].as_str() != Some("api-key")
        || !matches!(
            (&record.request().action, subject.scope),
            (Action::Intent(_), Scope::Intent) | (Action::Prepare(_), Scope::Preparation)
        )
    {
        return Err("native allocation subject changed".into());
    }
    let reference = &configuration.account()["secret_ref"];
    if reference["vault"].as_str() != Some(config.name.as_str()) {
        return Err("native allocation credential has another vault".into());
    }
    let entry = reference["entry"]
        .as_str()
        .ok_or("native allocation credential is not supplied")?;
    with_custody(paths, config, deadline, |active, config| {
        let credential = client::read_verified_entry(active, config, entry)
            .map_err(|e| e.to_string())?
            .ok_or("native allocation credential is unavailable")?;
        Inputs::decode(credential.value().as_slice(), subject.provider).map(|_| ())
    })?;
    check_time(deadline)?;
    Ok(members.into_iter().collect())
}

fn state(paths: &Paths) -> Result<Vec<Value>, String> {
    let Some(bytes) =
        fsx::read_private(&paths.dir.join(FILE), 64 * 1024).map_err(|e| e.to_string())?
    else {
        return Ok(Vec::new());
    };
    let mut value: Value = aterm_json::from_slice(bytes.as_slice())
        .map_err(|_| "native allocation queue is malformed")?;
    if value["queues"].as_array().is_none_or(|rows| rows.len() > 2) {
        return Err("native allocation queue is invalid".into());
    }
    if value.as_object().is_none_or(|object| object.len() != 2)
        || value["schema"].as_u64() != Some(1)
    {
        return Err("native allocation queue schema is invalid".into());
    }
    let Some(Value::Array(rows)) = value
        .as_object_mut()
        .and_then(|object| object.remove("queues"))
    else {
        return Err("native allocation queue is invalid".into());
    };
    for row in &rows {
        if row.as_object().is_none_or(|object| object.len() != 5)
            || row["provider"].as_str().and_then(Provider::of).is_none()
            || row["key"]
                .as_str()
                .is_none_or(|key| super::super::digest::<32>(key).is_err())
            || row["serial"].as_u64().is_none()
            || row["tickets"]
                .as_array()
                .is_none_or(|tickets| tickets.len() > 256)
            || row["grants"]
                .as_array()
                .is_none_or(|grants| grants.len() > 3)
        {
            return Err("native allocation queue row is invalid".into());
        }
        let mut actors = std::collections::BTreeSet::new();
        for ticket in row["tickets"].as_array().unwrap() {
            if ticket.as_object().is_none_or(|object| object.len() != 2)
                || ticket["actor"].as_str().is_none_or(|actor| {
                    super::super::digest::<48>(actor).is_err() || !actors.insert(actor)
                })
                || ticket["order"].as_u64().is_none()
            {
                return Err("native allocation ticket is invalid".into());
            }
        }
        let mut labels = std::collections::BTreeSet::new();
        for grant in row["grants"].as_array().unwrap() {
            if grant.as_object().is_none_or(|object| object.len() != 2)
                || grant["member"].as_str().is_none_or(|member| {
                    aterm_agent::harness::ai_pool::leading_m_label(member)
                        .is_none_or(|(_, canonical)| canonical != member)
                        || !labels.insert(member)
                })
                || grant["order"].as_u64().is_none()
            {
                return Err("native allocation grant hint is invalid".into());
            }
        }
    }
    Ok(rows)
}

fn save(paths: &Paths, rows: &[Value]) -> Result<(), String> {
    let bytes = aterm_json::to_vec(&json!({"schema":1,"queues":rows}))
        .map_err(|_| "native queue could not be encoded")?;
    fsx::write_atomic(&paths.dir.join(FILE), &bytes, 0o600).map_err(|e| e.to_string())
}

fn queue_key(local: &Binding, subject: Subject, allocator: &Allocator) -> String {
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm/native-allocation-queue/1\0");
    hash.update(local.network());
    hash.update(local.authority());
    hash.update(allocator.grant);
    hash.update(subject.account);
    hash.update(subject.revision);
    hash.update(subject.provider.name().as_bytes());
    hash.update([u8::from(subject.scope == Scope::Intent)]);
    aterm_codec::hex::encode_lower(&hash.finalize())
}

pub(super) fn delegated(
    allocator: &Allocator,
    local: &Binding,
    members: &[Binding],
    lineage: &[Binding],
) -> bool {
    if lineage.last() != Some(local)
        || members
            .iter()
            .filter(|member| member.machine() == local.machine())
            .count()
            != 1
    {
        return false;
    }
    if local.machine() == &allocator.machine {
        return true;
    }
    lineage[..lineage.len().saturating_sub(1)]
        .iter()
        .any(|old| old.machine() == &allocator.machine && old.actor() == allocator.actor)
}

fn next(row: &mut Value) -> Result<u64, String> {
    let order = row["serial"]
        .as_u64()
        .unwrap()
        .checked_add(1)
        .ok_or("native allocation ticket counter exhausted")?;
    row.as_object_mut()
        .unwrap()
        .insert("serial".into(), json!(order));
    Ok(order)
}

fn items_mut<'a>(row: &'a mut Value, name: &str) -> &'a mut Vec<Value> {
    row.as_object_mut()
        .unwrap()
        .get_mut(name)
        .unwrap()
        .as_array_mut()
        .unwrap()
}

fn choose(
    row: &mut Value,
    warm: &[String],
    candidates: &[(String, String)],
    valid: &std::collections::BTreeSet<String>,
    actors: &std::collections::BTreeSet<String>,
    allocate: bool,
) -> Result<Option<Vec<String>>, String> {
    items_mut(row, "tickets").retain(|ticket| {
        ticket["actor"]
            .as_str()
            .is_some_and(|actor| actors.contains(actor))
    });
    for (actor, _) in candidates {
        if !row["tickets"]
            .as_array()
            .unwrap()
            .iter()
            .any(|ticket| ticket["actor"].as_str() == Some(actor.as_str()))
        {
            if row["tickets"].as_array().unwrap().len() == 256 {
                continue;
            }
            let order = next(row)?;
            items_mut(row, "tickets").push(json!({"actor":actor,"order":order}));
        }
    }
    let mut kept: Vec<_> = warm
        .iter()
        .filter(|member| valid.contains(*member))
        .cloned()
        .collect();
    items_mut(row, "grants").retain(|grant| {
        kept.iter()
            .any(|member| grant["member"].as_str() == Some(member.as_str()))
    });
    for member in &kept {
        if !row["grants"]
            .as_array()
            .unwrap()
            .iter()
            .any(|grant| grant["member"].as_str() == Some(member.as_str()))
        {
            let order = next(row)?;
            items_mut(row, "grants").push(json!({"member":member,"order":order}));
        }
    }
    if !allocate {
        return Ok(None);
    }
    let waiting = row["tickets"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|ticket| {
            let actor = ticket["actor"].as_str()?;
            let (_, member) = candidates
                .iter()
                .find(|(candidate, member)| candidate == actor && !kept.contains(member))?;
            Some((ticket["order"].as_u64()?, actor, member.as_str()))
        })
        .min()
        .map(|(order, actor, member)| (order, actor.to_owned(), member));
    if let Some((_, actor, member)) = waiting {
        if kept.len() == 3 {
            let oldest = row["grants"]
                .as_array()
                .unwrap()
                .iter()
                .min_by_key(|grant| grant["order"].as_u64())
                .unwrap()["member"]
                .as_str()
                .unwrap()
                .to_owned();
            kept.retain(|member| *member != oldest);
            items_mut(row, "grants")
                .retain(|grant| grant["member"].as_str() != Some(oldest.as_str()));
        }
        kept.push(member.to_owned());
        let order = next(row)?;
        items_mut(row, "grants").push(json!({"member":member,"order":order}));
        for ticket in items_mut(row, "tickets") {
            if ticket["actor"].as_str() == Some(actor.as_str()) {
                ticket
                    .as_object_mut()
                    .unwrap()
                    .insert("order".into(), json!(order));
            }
        }
    }
    Ok((kept != warm).then_some(kept))
}

pub(super) fn step(
    paths: &Paths,
    admin: &client::Admin,
    config: &Path,
    deadline: Instant,
) -> Result<usize, String> {
    step_at(paths, admin, config, deadline, Instant::now())
}

pub(super) fn step_at(
    paths: &Paths,
    admin: &client::Admin,
    config: &Path,
    deadline: Instant,
    now: Instant,
) -> Result<usize, String> {
    check_time(deadline)?;
    if !native_keys::preparation_enabled(config)? {
        return Ok(0);
    }
    let Some((mut policy, _)) = super::policy_at(paths, &admin.config, deadline)? else {
        return Ok(0);
    };
    let observed = policy.clone();
    let _lock = aterm_uds::private_file::PrivateLock::take(
        &paths.dir,
        "native-allocation.lock",
        Duration::ZERO,
        "native allocation is busy",
    )
    .map_err(|e| e.to_string())?;
    let mut rows = state(paths)?;
    for provider in [Provider::Google, Provider::Meta] {
        let Some(assignment) = policy.automatic(provider)?.cloned() else {
            continue;
        };
        let allocator = assignment.automatic.as_ref().unwrap();
        let view = match Requests::at_until(paths.clone(), deadline).view_for(
            &target(assignment.subject, allocator.grant),
            digest(&observed),
        ) {
            Ok(view) => view,
            Err(_) => {
                check_time(deadline)?;
                continue;
            }
        };
        let lineage =
            aterm_db::application_lineage_until(paths, deadline).map_err(|e| e.to_string())?;
        if !delegated(allocator, &view.local, &view.members, &lineage) {
            continue;
        }
        let prepared = available(paths, &admin.config, assignment.subject, deadline)?;
        let roster = with_custody(paths, &admin.config, deadline, |active, _| {
            client::swarm_roster_until(active, deadline)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "native allocation roster is unavailable".into())
        })?;
        let fixed: std::collections::BTreeSet<&str> = policy
            .assignments
            .iter()
            .filter(|row| row.subject.provider == provider && row.automatic.is_none())
            .flat_map(|row| row.warm.iter().map(String::as_str))
            .collect();
        let mut valid = std::collections::BTreeSet::new();
        let mut actors = std::collections::BTreeSet::new();
        for member in &view.members {
            if !prepared.contains(member) {
                continue;
            }
            if let Some(label) =
                label(&roster, member).filter(|label| !fixed.contains(label.as_str()))
            {
                valid.insert(label);
                actors.insert(aterm_codec::hex::encode_lower(&member.actor()));
            }
            check_time(deadline)?;
        }
        let candidates: Vec<_> = view
            .requests
            .iter()
            .filter_map(|request| {
                label(&roster, &request.requester)
                    .filter(|label| valid.contains(label))
                    .map(|label| {
                        (
                            aterm_codec::hex::encode_lower(&request.requester.actor()),
                            label,
                        )
                    })
            })
            .collect();
        let key = queue_key(&view.local, assignment.subject, allocator);
        let at = if let Some(at) = rows
            .iter()
            .position(|row| row["key"].as_str() == Some(key.as_str()))
        {
            at
        } else {
            rows.retain(|row| row["provider"].as_str() != Some(provider.name()));
            rows.push(
                json!({"key":key,"provider":provider.name(),"serial":0,"tickets":[],"grants":[]}),
            );
            rows.len() - 1
        };
        // Preserve newly observed ticket order even while the rate limit holds. Only a
        // publication starts the next cooldown: a ready sweep that changes nothing leaves the
        // next one free to publish.
        let ready = {
            let mut due = DUE
                .get_or_init(|| Mutex::new(BTreeMap::new()))
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let at_time = due
                .entry((paths.dir.clone(), provider.name()))
                .or_insert_with(|| now + COOLDOWN);
            now >= *at_time
        };
        let warm = choose(
            &mut rows[at],
            &assignment.warm,
            &candidates,
            &valid,
            &actors,
            ready,
        )?;
        if !ready {
            save(paths, &rows)?;
            continue;
        }
        let mut changed = assignment.clone();
        if let Some(warm) = warm {
            changed.warm = warm;
        }
        changed.automatic.as_mut().unwrap().actor = view.local.actor();
        changed.automatic.as_mut().unwrap().machine = *view.local.machine();
        if changed == assignment {
            save(paths, &rows)?;
            continue;
        }
        policy.set(changed)?;
        check_time(deadline)?;
        if !native_keys::preparation_enabled(config)?
            || super::policy_at(paths, &admin.config, deadline)?
                .is_none_or(|(current, _)| current != observed)
        {
            return Err("native allocation authority changed before publication".into());
        }
        // Reopen subject and membership after the queue calculation.
        let fresh = Requests::at_until(paths.clone(), deadline)
            .view_for(
                &target(assignment.subject, allocator.grant),
                digest(&observed),
            )
            .map_err(|e| e.to_string())?;
        if fresh.local != view.local || fresh.members != view.members {
            return Err("native allocation membership changed".into());
        }
        if aterm_db::application_lineage_until(paths, deadline).map_err(|e| e.to_string())?
            != lineage
        {
            return Err("native allocator lineage changed before publication".into());
        }
        let published = policy.automatic(provider)?.unwrap();
        let prepared = available(paths, &admin.config, assignment.subject, deadline)?;
        for member in &fresh.members {
            if label(&roster, member).is_some_and(|label| published.warm.contains(&label))
                && !prepared.contains(member)
            {
                return Err("native preparation changed before allocation".into());
            }
        }
        let expected = admin
            .native_entry_digest(ENTRY)
            .map_err(|e| e.to_string())?;
        client::admin_set_entry_until(
            paths,
            admin,
            ENTRY,
            expected,
            policy.text().as_bytes(),
            deadline,
        )
        .map_err(|e| e.to_string())?;
        DUE.get_or_init(|| Mutex::new(BTreeMap::new()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert((paths.dir.clone(), provider.name()), now + COOLDOWN);
        save(paths, &rows)?;
        return Ok(1);
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fifo_tickets_survive_rotation_gaps_and_prune_ineligible_members() {
        let mut row =
            json!({"key":"00".repeat(32),"provider":"google","serial":0,"tickets":[],"grants":[]});
        let valid = ["m1", "m2", "m3", "m4", "m5"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let actors = ["a4", "a5"].into_iter().map(str::to_owned).collect();
        let candidates = vec![("a4".into(), "m4".into()), ("a5".into(), "m5".into())];
        let warm = vec!["m1".into(), "m2".into(), "m3".into()];
        assert!(
            choose(&mut row, &warm, &candidates, &valid, &actors, false)
                .unwrap()
                .is_none()
        );
        let saved = row["tickets"].clone();
        assert!(
            choose(&mut row, &warm, &[], &valid, &actors, false)
                .unwrap()
                .is_none()
        );
        assert_eq!(row["tickets"], saved);
        let next = choose(&mut row, &warm, &candidates, &valid, &actors, true)
            .unwrap()
            .unwrap();
        assert_eq!(next, vec!["m2", "m3", "m4"]);
        let next = choose(&mut row, &next, &candidates, &valid, &actors, true)
            .unwrap()
            .unwrap();
        assert_eq!(next, vec!["m3", "m4", "m5"]);
        let mut valid = valid;
        valid.remove("m3");
        assert_eq!(
            choose(&mut row, &next, &[], &valid, &actors, true)
                .unwrap()
                .unwrap(),
            vec!["m4", "m5"]
        );
    }
}
