// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read public epoch recipients and the key evidence in an epoch's exact causal
//! past. No network, publication, identity creation or credential output.
//! `history` locates causal first drops in raw retained wrap sets without a key;
//! `epoch <id>` inspects one historical epoch with the existing local identity.

#[cfg(unix)]
mod probe {
    use aterm_json::{Value, json};
    use aterm_secrets::{
        age::{Identity, Recipient},
        client::{self, Paths},
        membership_log::{Body, EpochAudience, Log, Record, RecordId, Store},
    };
    use std::{
        collections::{BTreeMap, BTreeSet},
        error::Error,
        io,
    };

    type Result<T> = std::result::Result<T, Box<dyn Error>>;
    const MAX_EPOCHS: usize = 8;
    const MAX_HISTORY_EPOCHS: usize = 256;
    const MAX_CONTEXT_RECORDS: usize = 256;
    const USAGE: &str = "usage: commons_epoch_probe [history | epoch <record-id>]";

    enum Selection {
        Current,
        History,
        Epoch(RecordId),
    }

    fn label(log: &Log, recipient: Recipient) -> Value {
        let membership = log.membership();
        let current = membership
            .members()
            .values()
            .find(|member| member.binding.recipient() == recipient);
        let listed = membership
            .unproved()
            .iter()
            .find(|founder| founder.recipient == recipient);
        let historical: BTreeSet<_> = log
            .records()
            .flat_map(|record| match record.body() {
                Body::Genesis { founders, .. } => founders
                    .iter()
                    .filter(|founder| founder.recipient == recipient)
                    .map(|founder| founder.label.clone())
                    .collect::<Vec<_>>(),
                Body::Admit { entrant, .. }
                    if entrant.request.binding().recipient() == recipient =>
                {
                    vec![entrant.label.clone()]
                }
                _ => Vec::new(),
            })
            .collect();
        json!({
            "recipient": recipient.to_bech32(),
            "current_member": current.is_some(),
            "current_label": current.map(|member| &member.label),
            "listed_member": listed.is_some(),
            "listed_label": listed.map(|founder| &founder.label),
            "historical_labels": historical.into_iter().collect::<Vec<_>>(),
            "removed": log.recipient_is_removed(&recipient),
        })
    }

    fn epoch(log: &Log, identity: &Identity, record: &Record) -> Result<Value> {
        let Body::Epoch {
            audience: EpochAudience::Commons,
            generation,
            ..
        } = record.body()
        else {
            return Err(io::Error::other("expected a Commons epoch").into());
        };
        // A file's preceding records may include a concurrent branch the author
        // never saw. Only the signed parent closure describes this epoch's past.
        let ancestors = log.ancestor_closure(record.parents());
        let expected = ancestors.len();
        let mut prior = Store::with_compiled(Some(*log.network()));
        prior.ingest_records(ancestors)?;
        let prior = prior
            .log()
            .ok_or_else(|| io::Error::other("causal past has no genesis"))?;
        if prior.held_count() != expected || prior.waiting() != 0 {
            return Err(io::Error::other("causal past was not retained completely").into());
        }
        let membership = prior.membership();
        let author = membership.get(record.actor());
        let local_recipient_matches_author =
            author.is_some_and(|author| author.binding.recipient() == identity.recipient());
        let local_key_evidence = if local_recipient_matches_author {
            let opened = prior.open_audience_entries(identity)?;
            opened.map(|opened| {
                let required = opened
                    .maximal_epochs
                    .get(&EpochAudience::Commons)
                    .cloned()
                    .unwrap_or_default();
                let all_keys = !required.is_empty()
                    && required
                        .iter()
                        .all(|id| opened.unwrapped_keys.contains_key(id));
                json!({
                    "scope": "present-local-key-opens-signed-causal-past",
                    "all_current_commons_keys": all_keys,
                    "commons_control_visible": opened.commons_control.is_some(),
                    "current_epochs": required.iter().map(|id| json!({
                        "id": aterm_codec::hex::encode_lower(id),
                        "key_openable": opened.unwrapped_keys.contains_key(id),
                        "raw_local_wraps": prior.records().filter(|record| matches!(
                            record.body(), Body::Wrap { epoch, recipient, .. }
                                if epoch == id && *recipient == identity.recipient()
                        )).count(),
                        "effective_local_wrap": opened.wrapped_recipients.get(id)
                            .is_some_and(|recipients| recipients.contains(&identity.recipient())),
                    })).collect::<Vec<_>>(),
                })
            })
        } else {
            None
        };
        let recipients: BTreeSet<_> = log
            .records()
            .filter_map(|candidate| match candidate.body() {
                Body::Wrap {
                    epoch, recipient, ..
                } if epoch == record.id() => Some(*recipient),
                _ => None,
            })
            .collect();
        Ok(json!({
            "id": aterm_codec::hex::encode_lower(record.id()), "generation": generation,
            "authored_ms": record.authored_ms(), "author_label": author.map(|author| &author.label),
            "causal_records_before": prior.held_count(),
            "local_recipient_matches_author": local_recipient_matches_author,
            "local_key_evidence": local_key_evidence,
            "raw_wrapped_recipients": recipients.into_iter().map(|recipient| label(log, recipient)).collect::<Vec<_>>(),
            "context": context(log, record),
            "grants_membership": false,
        }))
    }

    fn context(log: &Log, epoch: &Record) -> Value {
        let same: Vec<_> = log
            .records()
            .filter(|record| {
                record.actor() == epoch.actor() && record.authored_ms() == epoch.authored_ms()
            })
            .collect();
        let records: Vec<_> = same
            .iter()
            .take(MAX_CONTEXT_RECORDS)
            .map(|record| {
                let (target, recipient, removed) = match record.body() {
                    Body::Wrap {
                        epoch, recipient, ..
                    } => (Some(*epoch), Some(recipient.to_bech32()), None),
                    Body::Put { epoch, .. } | Body::Delete { epoch, .. } => {
                        (Some(*epoch), None, None)
                    }
                    Body::Remove { member, .. } => {
                        (None, None, Some(aterm_codec::hex::encode_lower(member)))
                    }
                    _ => (None, None, None),
                };
                json!({
                    "id": aterm_codec::hex::encode_lower(record.id()),
                    "kind": format!("{:?}", record.body().kind()),
                    "epoch": target.map(|id| aterm_codec::hex::encode_lower(&id)),
                    "recipient": recipient, "removed_member": removed,
                })
            })
            .collect();
        json!({
            "scope": "same-actor-and-signed-timestamp-not-a-transaction",
            "count": same.len(), "truncated": same.len() > records.len(), "records": records,
        })
    }

    /// Raw signed structure only: a retained wrap can have arrived after the epoch,
    /// and neither its presence nor a missing recipient establishes authorization.
    fn history(log: &Log) -> Result<Value> {
        if log.waiting() != 0 {
            return Err(
                io::Error::other("epoch history requires complete causal retention").into(),
            );
        }
        let epochs: BTreeMap<RecordId, &Record> = log
            .records()
            .filter_map(|record| {
                matches!(
                    record.body(),
                    Body::Epoch {
                        audience: EpochAudience::Commons,
                        ..
                    }
                )
                .then_some((*record.id(), record))
            })
            .collect();
        if epochs.len() > MAX_HISTORY_EPOCHS {
            return Err(io::Error::other("too many epochs for one history diagnostic").into());
        }
        let ancestors: BTreeMap<RecordId, BTreeSet<RecordId>> = epochs
            .iter()
            .map(|(id, record)| {
                (
                    *id,
                    log.ancestor_closure(record.parents())
                        .iter()
                        .map(|record| *record.id())
                        .collect(),
                )
            })
            .collect();
        let mut missing = BTreeMap::new();
        let mut values = BTreeMap::new();
        for (id, record) in &epochs {
            let prior: Vec<_> = epochs
                .keys()
                .copied()
                .filter(|other| ancestors[id].contains(other))
                .collect();
            let nearest: BTreeSet<_> = prior
                .iter()
                .copied()
                .filter(|earlier| {
                    !prior
                        .iter()
                        .any(|later| earlier != later && ancestors[later].contains(earlier))
                })
                .collect();
            let mut known = BTreeSet::new();
            let mut retained = BTreeSet::new();
            for wrap in log.records() {
                if let Body::Wrap {
                    epoch, recipient, ..
                } = wrap.body()
                {
                    if nearest.contains(epoch) && ancestors[id].contains(wrap.id()) {
                        known.insert(*recipient);
                    }
                    if epoch == id {
                        retained.insert(*recipient);
                    }
                }
            }
            let lost: BTreeSet<_> = known.difference(&retained).copied().collect();
            let Body::Epoch { generation, .. } = record.body() else {
                unreachable!()
            };
            values.insert(*id, json!({
                "id": aterm_codec::hex::encode_lower(id), "generation": generation,
                "actor": aterm_codec::hex::encode_lower(record.actor()),
                "current_author_label": log.membership().get(record.actor()).map(|member| member.label.clone()),
                "authored_ms": record.authored_ms(),
                "causal_records_before": ancestors[id].len(),
                "nearest_prior_epochs": nearest.iter().map(|id| aterm_codec::hex::encode_lower(id)).collect::<Vec<_>>(),
                "known_prior_recipients": known.len(), "retained_recipients": retained.len(),
                "missing_prior_recipients": lost.len(),
                "missing_recipients": lost.iter().map(|recipient| label(log, *recipient)).collect::<Vec<_>>(),
                "context": if lost.is_empty() { Value::Null } else { context(log, record) },
            }));
            if !lost.is_empty() {
                missing.insert(*id, lost);
            }
        }
        let first: Vec<_> = missing.iter().filter(|(id, _)| {
            !missing.keys().any(|prior| ancestors[*id].contains(prior))
        }).map(|(id, lost)| json!({
            "epoch": values[id].clone(),
            "missing_recipients": lost.iter().map(|recipient| label(log, *recipient)).collect::<Vec<_>>(),
            "context": values[id]["context"].clone(),
        })).collect();
        let mut by_recipient: BTreeMap<Recipient, Vec<RecordId>> = BTreeMap::new();
        for (id, lost) in &missing {
            for recipient in lost {
                by_recipient.entry(*recipient).or_default().push(*id);
            }
        }
        let first_by_recipient: Vec<_> = by_recipient
            .iter()
            .map(|(recipient, ids)| {
                let first: Vec<_> = ids
                    .iter()
                    .filter(|id| !ids.iter().any(|prior| ancestors[*id].contains(prior)))
                    .map(|id| aterm_codec::hex::encode_lower(id))
                    .collect();
                json!({"recipient": label(log, *recipient), "epoch_ids": first})
            })
            .collect();
        Ok(json!({
            "schema": 1, "kind": "commons-epoch-wrap-history", "available": true,
            "scope": "raw-signed-ancestry-and-currently-retained-wraps-not-authorization",
            "ordering": "causal-minima-not-timestamp-order",
            "complete_records": log.held_count(), "epoch_count": epochs.len(),
            "narrowed_epoch_count": missing.len(), "first_narrowed_epochs": first,
            "first_drop_by_recipient": first_by_recipient,
            "epochs": values.into_values().collect::<Vec<_>>(), "grants_membership": false,
        }))
    }

    pub fn run() -> Result<Value> {
        let args = std::env::args_os()
            .skip(1)
            .map(|arg| arg.into_string().map_err(|_| io::Error::other(USAGE)))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let selection = match args.as_slice() {
            [] => Selection::Current,
            [mode] if mode == "history" => Selection::History,
            [mode, id] if mode == "epoch" => Selection::Epoch(
                aterm_codec::hex::decode_vec(id.as_bytes())
                    .and_then(|bytes| bytes.try_into().ok())
                    .ok_or_else(|| io::Error::other(USAGE))?,
            ),
            _ => return Err(io::Error::other(USAGE).into()),
        };
        let paths = Paths::live()?;
        let Some(_guard) = client::try_commons_source_read(&paths)? else {
            return Ok(
                json!({"available": false, "reason": "vault observation lock absent or busy", "grants_membership": false}),
            );
        };
        let store = client::load_records(&paths)?;
        let log = store
            .log()
            .ok_or_else(|| io::Error::other("existing membership log unavailable"))?;
        if matches!(selection, Selection::History) {
            return history(log);
        }
        let identity = client::read_machine_identity(&paths)?
            .ok_or_else(|| io::Error::other("existing identity unavailable"))?;
        let selected = match selection {
            Selection::Current => log
                .open_audience_entries(&identity)?
                .ok_or_else(|| io::Error::other("existing epoch observation unavailable"))?
                .maximal_epochs
                .get(&EpochAudience::Commons)
                .cloned()
                .unwrap_or_default(),
            Selection::Epoch(id) => vec![id],
            Selection::History => unreachable!(),
        };
        if selected.len() > MAX_EPOCHS {
            return Err(
                io::Error::other("too many current epochs for one causal diagnostic").into(),
            );
        }
        let epochs = selected
            .iter()
            .map(|id| {
                epoch(
                    log,
                    &identity,
                    log.get(id)
                        .ok_or_else(|| io::Error::other("epoch record unavailable"))?,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(
            json!({"schema": 1, "kind": "commons-epoch-causal-evidence", "available": true,
            "scope": "one-local-vault-observation", "complete_records": log.held_count(),
            "epochs": epochs, "grants_membership": false}),
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use aterm_secrets::{
            commons::binding::Request,
            membership_log::{Entrant, MemberId},
        };

        fn entrant(tag: u8) -> (Identity, [u8; 32], [u8; 16], MemberId, Entrant) {
            let identity = Identity::from_secret_bytes([tag + 64; 32]);
            let seed = [tag; 32];
            let incarnation = [tag; 16];
            let request = Request::create(
                [17; 16],
                [34; 32],
                [tag + 32; 16],
                incarnation,
                &identity,
                &seed,
            )
            .unwrap();
            let actor = request.binding().actor();
            (
                identity,
                seed,
                incarnation,
                actor,
                Entrant {
                    request,
                    node_key: [tag + 128; 32],
                    name: format!("m{tag}"),
                    label: format!("m{tag}"),
                },
            )
        }

        fn add_epoch(
            log: &mut Log,
            seed: &[u8; 32],
            incarnation: [u8; 16],
            actor: &MemberId,
            generation: u64,
            recipients: &[Recipient],
        ) -> Record {
            let (draft, key) = log
                .draft_epoch(actor, 2_000, EpochAudience::Commons, generation)
                .unwrap();
            let record = draft.sign(seed, incarnation).unwrap();
            log.insert(record.clone()).unwrap();
            for recipient in recipients {
                let wrap = log
                    .draft_wrap(actor, 2_000, *record.id(), &key, *recipient)
                    .unwrap()
                    .sign(seed, incarnation)
                    .unwrap();
                log.insert(wrap).unwrap();
            }
            record
        }

        #[test]
        fn causal_past_excludes_an_unseen_concurrent_epoch_but_includes_a_known_one() {
            let (id1, seed1, inc1, actor1, first) = entrant(1);
            let (id2, seed2, inc2, actor2, second) = entrant(2);
            let mut base =
                Log::new(Record::found(&[7; 32], vec![first, second], 1_000).unwrap()).unwrap();
            let shared = add_epoch(
                &mut base,
                &seed1,
                inc1,
                &actor1,
                1,
                &[id1.recipient(), id2.recipient()],
            );
            let mut author_branch = base.clone();
            let target = add_epoch(&mut author_branch, &seed1, inc1, &actor1, 2, &[]);
            let mut concurrent = base.clone();
            add_epoch(
                &mut concurrent,
                &seed2,
                inc2,
                &actor2,
                2,
                &[id2.recipient()],
            );
            concurrent.insert(target.clone()).unwrap();
            let observed = epoch(&concurrent, &id1, &target).unwrap();
            assert_eq!(observed["causal_records_before"], json!(base.held_count()));
            assert_eq!(
                observed["local_key_evidence"]["all_current_commons_keys"],
                json!(true)
            );
            assert_eq!(
                observed["local_key_evidence"]["current_epochs"][0]["id"],
                json!(aterm_codec::hex::encode_lower(shared.id()))
            );

            let mut knew_other = base.clone();
            add_epoch(
                &mut knew_other,
                &seed2,
                inc2,
                &actor2,
                2,
                &[id2.recipient()],
            );
            let target = add_epoch(&mut knew_other, &seed1, inc1, &actor1, 3, &[]);
            let observed = epoch(&knew_other, &id1, &target).unwrap();
            assert_eq!(
                observed["local_key_evidence"]["all_current_commons_keys"],
                json!(false)
            );
        }

        #[test]
        fn unmatched_recipient_has_no_local_key_evidence() {
            let (id1, seed1, inc1, actor1, first) = entrant(1);
            let (id2, _, _, _, second) = entrant(2);
            let mut log =
                Log::new(Record::found(&[7; 32], vec![first, second], 1_000).unwrap()).unwrap();
            add_epoch(
                &mut log,
                &seed1,
                inc1,
                &actor1,
                1,
                &[id1.recipient(), id2.recipient()],
            );
            let target = add_epoch(&mut log, &seed1, inc1, &actor1, 2, &[]);
            let observed = epoch(&log, &id2, &target).unwrap();
            assert_eq!(observed["local_recipient_matches_author"], json!(false));
            assert_eq!(observed["local_key_evidence"], Value::Null);
        }

        #[test]
        fn key_evidence_does_not_credit_a_concurrent_wrap_to_the_matching_author() {
            let (id1, seed1, inc1, actor1, first) = entrant(1);
            let (id2, seed2, inc2, actor2, second) = entrant(2);
            let mut base =
                Log::new(Record::found(&[7; 32], vec![first, second], 1_000).unwrap()).unwrap();
            let prior = add_epoch(&mut base, &seed1, inc1, &actor1, 1, &[id1.recipient()]);
            let key = base
                .open_audience_entries(&id1)
                .unwrap()
                .unwrap()
                .unwrapped_keys[prior.id()]
            .clone();
            let mut authored = base.clone();
            let target = add_epoch(&mut authored, &seed2, inc2, &actor2, 2, &[]);
            let concurrent_wrap = base
                .draft_wrap(&actor1, 3_000, *prior.id(), &key, id2.recipient())
                .unwrap()
                .sign(&seed1, inc1)
                .unwrap();
            authored.insert(concurrent_wrap).unwrap();
            assert!(
                authored
                    .open_audience_entries(&id2)
                    .unwrap()
                    .unwrap()
                    .unwrapped_keys
                    .contains_key(prior.id())
            );

            let observed = epoch(&authored, &id2, &target).unwrap();
            assert_eq!(observed["local_recipient_matches_author"], json!(true));
            let before = &observed["local_key_evidence"]["current_epochs"][0];
            assert_eq!(before["key_openable"], json!(false));
            assert_eq!(before["raw_local_wraps"], json!(0));
            assert_eq!(before["effective_local_wrap"], json!(false));
        }

        #[test]
        fn history_reports_causal_first_drops_across_concurrent_branches() {
            let (id1, seed1, inc1, actor1, first) = entrant(1);
            let (id2, seed2, inc2, actor2, second) = entrant(2);
            let (id3, _, _, _, third) = entrant(3);
            let mut base =
                Log::new(Record::found(&[7; 32], vec![first, second, third], 1_000).unwrap())
                    .unwrap();
            add_epoch(
                &mut base,
                &seed1,
                inc1,
                &actor1,
                1,
                &[id1.recipient(), id2.recipient(), id3.recipient()],
            );
            let mut left = base.clone();
            let left_first = add_epoch(
                &mut left,
                &seed1,
                inc1,
                &actor1,
                90,
                &[id1.recipient(), id2.recipient()],
            );
            let descendant = add_epoch(&mut left, &seed1, inc1, &actor1, 91, &[id1.recipient()]);
            let mut right = base.clone();
            let right_first = add_epoch(
                &mut right,
                &seed2,
                inc2,
                &actor2,
                2,
                &[id2.recipient(), id3.recipient()],
            );
            let unseen: Vec<_> = right
                .records()
                .filter(|record| left.get(record.id()).is_none())
                .cloned()
                .collect();
            for record in unseen {
                left.insert(record).unwrap();
            }
            let observed = history(&left).unwrap();
            assert_eq!(observed["narrowed_epoch_count"], json!(3));
            let first: BTreeSet<_> = observed["first_narrowed_epochs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["epoch"]["id"].as_str().unwrap().to_owned())
                .collect();
            assert_eq!(
                first,
                BTreeSet::from([
                    aterm_codec::hex::encode_lower(left_first.id()),
                    aterm_codec::hex::encode_lower(right_first.id()),
                ])
            );
            assert!(!first.contains(&aterm_codec::hex::encode_lower(descendant.id())));
            let dropped_second = observed["first_drop_by_recipient"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["recipient"]["recipient"] == json!(id2.recipient().to_bech32()))
                .unwrap();
            assert_eq!(
                dropped_second["epoch_ids"],
                json!([aterm_codec::hex::encode_lower(descendant.id())])
            );
        }

        #[test]
        fn history_does_not_attribute_an_unseen_concurrent_wrap_to_the_author() {
            let (id1, seed1, inc1, actor1, first) = entrant(1);
            let (id2, seed2, inc2, actor2, second) = entrant(2);
            let mut base =
                Log::new(Record::found(&[7; 32], vec![first, second], 1_000).unwrap()).unwrap();
            let prior = add_epoch(&mut base, &seed1, inc1, &actor1, 1, &[id1.recipient()]);
            let key = base
                .open_audience_entries(&id1)
                .unwrap()
                .unwrap()
                .unwrapped_keys[prior.id()]
            .clone();
            let mut author = base.clone();
            let target = add_epoch(&mut author, &seed1, inc1, &actor1, 2, &[id1.recipient()]);
            let unseen = base
                .draft_wrap(&actor2, 3_000, *prior.id(), &key, id2.recipient())
                .unwrap()
                .sign(&seed2, inc2)
                .unwrap();
            author.insert(unseen).unwrap();
            let observed = history(&author).unwrap();
            let target_id = aterm_codec::hex::encode_lower(target.id());
            let row = observed["epochs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["id"].as_str() == Some(target_id.as_str()))
                .unwrap();
            assert_eq!(row["known_prior_recipients"], json!(1));
            assert_eq!(row["missing_prior_recipients"], json!(0));
            assert_eq!(observed["narrowed_epoch_count"], json!(0));
        }
    }
}

#[cfg(unix)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", probe::run()?);
    Ok(())
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("commons_epoch_probe requires Unix vault support");
    std::process::ExitCode::from(2)
}
