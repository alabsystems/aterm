// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::ledger::CohortFixture;

fn fixture(count: usize) -> (String, Journal) {
    let fixture = CohortFixture::new();
    let claim = fixture.claim();
    let text = std::fs::read_to_string(claim.path()).unwrap();
    let (head, tail) = text.split_once('\n').unwrap();
    let scope = Scope::decode(&aterm_json::json!({
        "host": "linux:ffffffffffffffffffffffffffffffff",
        "boot": "ffffffffffffffffffffffffffffffff",
        "namespace": u64::MAX.to_string(),
    }))
    .unwrap();
    let holder = Holder {
        identity: Identity::parse_shaped("2147483647@18446744073709551615", false).unwrap(),
        scope,
    };
    let observation = |index: usize| {
        RecordedCohort::decode(&aterm_json::json!({
            "v": 1, "captured": {"scope": holder.scope.encode(),
                "group": i32::MAX as u64 - index as u64,
                "leader_serial": u64::MAX - index as u64,
            }
        }))
        .unwrap()
    };
    let birth = |index: usize| Birth {
        nonce: format!("{:032x}", u128::MAX - index as u128),
        holder: holder.clone(),
    };
    let mut head: Value = aterm_json::from_str(head).unwrap();
    let map = head.as_object_mut().unwrap();
    map.insert("owner".into(), holder.encode());
    map.insert("pid".into(), Value::from(i32::MAX as u64));
    // Native Unix path components may contain U+0001 (but not NUL or slash).
    // Two 255-byte components give the full 512-byte allowance and worst JSON escaping.
    let path = format!("/{}/{}", "\u{1}".repeat(255), "\u{1}".repeat(255));
    map.insert("what".into(), Value::from("\u{1}".repeat(512)));
    map.insert("checkout".into(), Value::from(path.clone()));
    map.insert("volume".into(), Value::from(path));
    for field in ["mem", "margin", "disk", "start_ms", "seq"] {
        map.insert(field.into(), Value::from(u64::MAX));
    }
    map.insert("cores".into(), Value::from(u32::MAX));
    map.insert("rank".into(), Value::from("background"));
    map.insert("quiet".into(), Value::from(f64::MAX));
    map.insert("alone".into(), Value::from(true));
    map.insert("covers".into(), Value::from(true));
    map.insert(
        "patience".into(),
        aterm_json::json!({"reserve_ms": u64::MAX, "start_ms": u64::MAX, "hold_ms": u64::MAX}),
    );
    let _ = tail;
    let base = format!(
        "{}\n{}\n",
        aterm_json::to_string(&head).unwrap(),
        aterm_json::json!({"admitted_ms": u64::MAX, "cores": u32::MAX, "alone": true})
    );
    let journal = Journal {
        holder: holder.clone(),
        state: State::Bound(birth(0), observation(0)),
        policy: ClosurePolicy::BootOnly,
        cohort_owned: false,
        armed_once: true,
        gnu: None,
        subgroups: (1..=count)
            .map(|index| Subgroup {
                key: birth(index),
                captured: Some(observation(index)),
            })
            .collect(),
    };
    (base, journal)
}

fn namespace(text: &str) -> String {
    let head: Value = aterm_json::from_str(text.lines().next().unwrap()).unwrap();
    head["namespace"].as_str().unwrap().to_owned()
}

#[test]
fn finish_refusal_retains_the_observation_that_decided_it_without_another_read() {
    use std::cell::Cell;

    let (_, mut journal) = fixture(2);
    journal.policy = ClosurePolicy::Group;
    let original = journal.clone();
    for observed in [Lifetime::Present, Lifetime::Unknown, Lifetime::Drained] {
        let reads = Cell::new(0);
        let result = journal.finish_with(|| {
            reads.set(reads.get() + 1);
            observed
        });
        assert_eq!(reads.get(), 1);
        assert_eq!(
            journal, original,
            "FINISH proposes an event without applying it"
        );
        if observed == Lifetime::Drained {
            let (event, next) = result.unwrap().unwrap();
            assert_eq!(
                aterm_json::from_str::<Value>(&event).unwrap()["cohort"]["phase"].as_str(),
                Some("finish")
            );
            assert!(next.description());
            assert!(next.subgroups.is_empty());
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
            let evidence = error
                .get_ref()
                .unwrap()
                .downcast_ref::<DrainRefusal>()
                .unwrap();
            assert_eq!(evidence.observed, Some(observed));
            assert_eq!(evidence.policy, ClosurePolicy::Group);
            assert_eq!(evidence.subgroups, 2);
        }
    }
}

#[test]
fn finish_reads_no_lifetime_for_an_unbound_root_or_a_pending_gnu_epoch() {
    let (_, mut journal) = fixture(0);
    let bound = journal.state.clone();
    let State::Bound(key, _) = &journal.state else {
        panic!("the fixture is bound");
    };
    let key = key.clone();
    journal.state = State::Description;
    assert!(
        journal
            .finish_with(|| panic!("no bound cohort to read"))
            .unwrap()
            .is_none()
    );
    journal.state = State::Armed(key);
    let error = journal
        .finish_with(|| panic!("no bound cohort to read"))
        .unwrap_err();
    let evidence = error
        .get_ref()
        .unwrap()
        .downcast_ref::<DrainRefusal>()
        .unwrap();
    assert_eq!(evidence.observed, None);
    journal.state = bound;
    journal.gnu = Some(("unretired-epoch".into(), false));
    let error = journal
        .finish_with(|| panic!("the GNU epoch refuses first"))
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    assert!(
        error
            .get_ref()
            .and_then(|error| error.downcast_ref::<DrainRefusal>())
            .is_none()
    );
}

#[test]
fn cohort_owned_header_survives_compaction_without_law_downgrade() {
    let (base, mut journal) = fixture(2);
    let (head, tail) = base.split_once('\n').unwrap();
    let mut head: Value = aterm_json::from_str(head).unwrap();
    head.as_object_mut()
        .unwrap()
        .insert("v".into(), Value::from(3u64));
    head.as_object_mut()
        .unwrap()
        .insert("reservation_law".into(), Value::from("cohort_owned"));
    let base = format!("{}{tail}", line(&head).unwrap());
    journal.cohort_owned = true;
    let old = journal.compact_as(&base, false).unwrap();
    let compact = journal.compact(&old).unwrap();
    let namespace = namespace(&base);
    let (record, decoded) = Journal::decode(&compact, true, &namespace).unwrap();
    assert_eq!(record.accounting, crate::Accounting::CohortOwned);
    assert_eq!(decoded, journal);
    assert!(Journal::known_subgroup_compaction(&old, &compact));
    let downgraded = compact
        .replacen("\"v\":3", "\"v\":2", 1)
        .replace("\"reservation_law\":\"cohort_owned\",", "");
    assert!(Journal::decode(&downgraded, true, &namespace).is_ok());
    assert!(!Journal::known_subgroup_compaction(&old, &downgraded));
}

fn split_snapshot(text: &str) -> (String, Value) {
    let rows = text.split_inclusive('\n').collect::<Vec<_>>();
    (
        rows[..rows.len() - 1].concat(),
        aterm_json::from_str(rows.last().unwrap()).unwrap(),
    )
}

fn event(snapshot: &mut Value) -> &mut Value {
    snapshot.as_object_mut().unwrap().get_mut("cohort").unwrap()
}

fn groups(snapshot: &mut Value) -> &mut Vec<Value> {
    event(snapshot)
        .as_object_mut()
        .unwrap()
        .get_mut("subgroups")
        .unwrap()
        .as_array_mut()
        .unwrap()
}

#[test]
fn compact_snapshot_v2_roundtrips_28_max_width_observations_within_existing_bytes() {
    let (base, journal) = fixture(28);
    let compact = journal.compact(&base).unwrap();
    let v1 = journal.compact_as(&base, false).unwrap();
    let (_, mut snapshot) = split_snapshot(&compact);
    assert_eq!(snapshot["cohort"]["v"].as_u64(), Some(2));
    assert_eq!(groups(&mut snapshot).len(), 28);
    assert!(compact.len() + 2048 < crate::ledger::RECORD_MAX as usize);
    assert!(compact.len() < v1.len());
    let (_, decoded) = Journal::decode(&compact, true, &namespace(&base)).unwrap();
    assert_eq!(
        decoded, journal,
        "all nonces, groups, serials, scope and policy survive"
    );
    eprintln!(
        "28 max-width bound subgroups: v2={} bytes, v1={} bytes, record cap={}",
        compact.len(),
        v1.len(),
        crate::ledger::RECORD_MAX
    );
    let limit = crate::ledger::RECORD_MAX as usize;
    let padded = format!(
        "{}{}\n",
        compact.trim_end_matches('\n'),
        " ".repeat(limit - compact.len())
    );
    assert_eq!(padded.len(), limit);
    assert!(Journal::decode(&padded, true, &namespace(&base)).is_ok());
    let oversized = format!("{} \n", padded.trim_end_matches('\n'));
    assert_eq!(oversized.len(), limit + 1);
    assert!(Journal::decode(&oversized, true, &namespace(&base)).is_err());
}

#[test]
fn compact_snapshot_v2_factors_only_exact_common_holder_and_scope() {
    let (base, mut journal) = fixture(2);
    journal.subgroups[0].key.holder.identity.pid -= 1;
    let text = journal.compact(&base).unwrap();
    assert_eq!(text, journal.compact_as(&base, false).unwrap());
    let (_, snapshot) = split_snapshot(&text);
    assert_eq!(snapshot["cohort"]["v"].as_u64(), Some(1));
    assert_eq!(
        snapshot["cohort"]["subgroups"][0]["key"],
        journal.subgroups[0].key.encode()
    );
    assert!(Journal::decode(&text, true, &namespace(&base)).is_err());

    let (base, mut journal) = fixture(2);
    let mut capture = journal.subgroups[0].captured.as_ref().unwrap().encode();
    capture.as_object_mut().unwrap().get_mut("captured").unwrap().as_object_mut().unwrap().insert("scope".into(), aterm_json::json!({
        "host": "linux:ffffffffffffffffffffffffffffffff", "boot": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "namespace": u64::MAX.to_string(),
    }));
    journal.subgroups[0].captured = Some(RecordedCohort::decode(&capture).unwrap());
    let text = journal.compact(&base).unwrap();
    assert_eq!(text, journal.compact_as(&base, false).unwrap());
    let (_, snapshot) = split_snapshot(&text);
    assert_eq!(snapshot["cohort"]["subgroups"][0]["captured"], capture);
    assert!(Journal::decode(&text, true, &namespace(&base)).is_err());
}

#[test]
fn compact_snapshot_v2_rejects_incomplete_common_data_and_malformed_rows() {
    let (base, mut journal) = fixture(2);
    journal.subgroups[1].captured = None;
    let text = journal.compact(&base).unwrap();
    assert_eq!(
        Journal::decode(&text, true, &namespace(&base)).unwrap().1,
        journal
    );
    let (prefix, snapshot) = split_snapshot(&text);
    let refuses = |snapshot: &Value| {
        let altered = format!("{prefix}{}\n", aterm_json::to_string(snapshot).unwrap());
        assert!(Journal::decode(&altered, true, &namespace(&base)).is_err());
    };
    for path in [
        vec!["root"],
        vec!["policy"],
        vec!["subgroups"],
        vec!["root", "key", "holder"],
        vec!["root", "key", "holder", "scope"],
        vec!["root", "key", "holder", "scope", "host"],
        vec!["root", "captured", "captured", "scope", "boot"],
    ] {
        let mut altered = snapshot.clone();
        let mut value = event(&mut altered);
        for key in &path[..path.len() - 1] {
            value = value.as_object_mut().unwrap().get_mut(*key).unwrap();
        }
        value.as_object_mut().unwrap().remove(*path.last().unwrap());
        refuses(&altered);
    }
    let mut altered = snapshot.clone();
    event(&mut altered)
        .as_object_mut()
        .unwrap()
        .get_mut("root")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .get_mut("key")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .get_mut("holder")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert(
            "identity".into(),
            Value::from("2147483646@18446744073709551615"),
        );
    refuses(&altered);
    for field in ["group", "leader_serial"] {
        let mut altered = snapshot.clone();
        groups(&mut altered)[0]
            .as_object_mut()
            .unwrap()
            .get_mut("captured")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(field);
        refuses(&altered);
    }
    for group in [
        Value::from(0u64),
        Value::from(1u64),
        Value::from(i32::MAX as u64 + 1),
        Value::Null,
    ] {
        let mut altered = snapshot.clone();
        groups(&mut altered)[0]
            .as_object_mut()
            .unwrap()
            .get_mut("captured")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("group".into(), group);
        refuses(&altered);
    }
    for nonce in [
        "bad".to_owned(),
        "F".repeat(NONCE_BYTES),
        journal.subgroups[0].key.nonce.clone(),
        match &journal.state {
            State::Bound(key, _) => key.nonce.clone(),
            _ => unreachable!(),
        },
    ] {
        let mut altered = snapshot.clone();
        groups(&mut altered)[1]
            .as_object_mut()
            .unwrap()
            .insert("nonce".into(), Value::from(nonce));
        refuses(&altered);
    }
    let mut altered = snapshot.clone();
    groups(&mut altered)[0]
        .as_object_mut()
        .unwrap()
        .insert("scope".into(), Value::Null);
    refuses(&altered);
    let mut altered = snapshot.clone();
    groups(&mut altered)[0]
        .as_object_mut()
        .unwrap()
        .get_mut("captured")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("scope".into(), Value::Null);
    refuses(&altered);
}

#[test]
fn compact_snapshot_v2_compaction_preserves_semantics_across_both_formats() {
    let (base, journal) = fixture(2);
    let v1 = journal.compact_as(&base, false).unwrap();
    let v2 = journal.compact(&base).unwrap();
    assert!(Journal::known_subgroup_compaction(&v1, &v2));
    assert!(
        Journal::known_subgroup_compaction(&v2, &v1),
        "old pending v1 snapshots still recover"
    );
    let (prefix, mut altered) = split_snapshot(&v2);
    groups(&mut altered)[0]
        .as_object_mut()
        .unwrap()
        .get_mut("captured")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("leader_serial".into(), Value::from(0u64));
    let altered = format!("{prefix}{}\n", aterm_json::to_string(&altered).unwrap());
    assert!(Journal::decode(&altered, true, &namespace(&base)).is_ok());
    assert!(!Journal::known_subgroup_compaction(&v1, &altered));
    let accounting = v2.replacen(
        "\"mem\":18446744073709551615",
        "\"mem\":18446744073709551614",
        1,
    );
    assert!(Journal::decode(&accounting, true, &namespace(&base)).is_ok());
    assert!(!Journal::known_subgroup_compaction(&v1, &accounting));
}
