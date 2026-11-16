// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::alab::registry::{PROVIDER, REGISTRY, SEALED_ADDRESS};
use crate::typed::secret::{Fixed, Secret};

fn op(n: u8) -> Digest {
    [n; 32]
}

/// An entry of generation 1 at clock `ms`, writing one provider cell.
fn entry(n: u8, ms: u64, site: &str) -> Entry {
    Entry {
        operation: op(n),
        generation: 1,
        actor: [n; 48],
        machine: Some([n; 16]),
        authored_ms: ms,
        hlc: Hlc { ms, counter: 0 },
        request: Some([n; 16]),
        reverts: None,
        changes: vec![Written {
            collection: PROVIDER.name.into(),
            key: "anthropic.site".into(),
            value: Some(json!({"r": 1, "x": site})),
            withheld: false,
        }],
        trimmed: false,
    }
}

/// [`plan`] by the Mac `u` of generation 1 of the registry.
fn plan_u(owed: &[Entry], target: &Carried, budget: Budget) -> Planned {
    plan(
        &REGISTRY,
        owed.iter().cloned(),
        &BTreeMap::new(),
        target,
        1,
        "u",
        budget,
    )
    .unwrap()
}

/// What `rows` say, read back as `carried` reads the rows the Mac `u` wrote.
fn read_back(rows: &[Vec<Change>]) -> Carried {
    let mut out = Carried::default();
    read_into(&mut out, rows);
    out
}

fn read_into(out: &mut Carried, rows: &[Vec<Change>]) {
    for change in rows.iter().flatten() {
        let key = CellKey::parse(&change.key).unwrap();
        if key.col != "entry" {
            continue;
        }
        let value = CellValue::decode(change.value.as_ref().unwrap()).unwrap();
        let bytes = aterm_secrets::b64::decode_padded(value.x.as_str().unwrap()).unwrap();
        let body: Value = aterm_json::from_slice(&bytes).unwrap();
        out.take(
            key.pk,
            stated(key.pk, &body, Some("u")).expect("a row under its own key"),
        );
    }
}

/// An entry goes in as one row and reads back exactly; what the target carries already is not
/// written again.
#[test]
fn an_entry_reads_back_as_it_was_carried() {
    let owed = vec![
        entry(1, 10, "https://one.example.test"),
        entry(2, 20, "two"),
    ];
    let mut target = Carried::default();
    target.entries.insert(op(2), entry(2, 20, "two"));
    let planned = plan_u(&owed, &target, Budget::MOST);
    assert_eq!((planned.carried, planned.not_carried), (1, 0));
    let back = read_back(&planned.rows);
    assert_eq!(back.entries.len(), 1);
    assert_eq!(back.entries[&op(1)], owed[0]);
    assert!(back.horizons.is_empty());
}

/// The budget for exactly the rows of `entries`, as this Mac carries them.
fn budget_for(entries: &[&Entry]) -> Budget {
    let rows: Vec<_> = entries.iter().map(|e| entry_row(e, "u").unwrap()).collect();
    let (records, bytes) = appended(&rows).unwrap();
    Budget { bytes, records }
}

/// Past its bound a roll carries the newest entries and one horizon naming the newest it left
/// out; the release gate then asks nothing of what the horizon answers and names what neither
/// answers. NEGATIVE CONTROL: with the horizon compared by `<` instead of `<=`, the newest entry
/// left out is named uncarried.
#[test]
fn past_its_bound_a_roll_carries_the_newest_and_states_what_it_left_out() {
    let owed: Vec<Entry> = (1..=6).map(|n| entry(n, u64::from(n) * 10, "x")).collect();
    let planned = plan_u(
        &owed,
        &Carried::default(),
        budget_for(&[&owed[5], &owed[4]]),
    );
    assert_eq!((planned.carried, planned.not_carried), (2, 4));
    let back = read_back(&planned.rows);
    assert_eq!(
        back.entries.keys().copied().collect::<Vec<_>>(),
        vec![op(5), op(6)]
    );
    assert_eq!(
        back.horizons.values().cloned().collect::<Vec<_>>(),
        vec![Horizon {
            generation: 1,
            by: "u".into(),
            through: owed[3].order(),
            not_carried: 4,
        }]
    );
    assert!(
        back.horizons
            .contains_key(&keys::history_horizon_pk(1, "u"))
    );
    assert!(uncarried(&owed, &back).is_empty());

    // Without the horizon the four left out are owed; a late entry newer than the horizon is
    // owed whatever the horizon says.
    let mut no_horizon = back.clone();
    no_horizon.horizons.clear();
    assert_eq!(uncarried(&owed, &no_horizon).len(), 4);
    let late = entry(7, 45, "late");
    assert_eq!(uncarried(std::slice::from_ref(&late), &back), vec![&late]);
}

/// A roll run again over what it carried writes nothing: what its horizon left out is not
/// carried on the second run either. A later roll that leaves out more moves the horizon on,
/// counting both. NEGATIVE CONTROL: with `plan` skipping only what the target carries as entries
/// (not what its horizons answer), the second run writes the four left out again, past the bound
/// the first run kept.
#[test]
fn a_roll_run_again_writes_nothing_again() {
    let owed: Vec<Entry> = (1..=6).map(|n| entry(n, u64::from(n) * 10, "x")).collect();
    let first = plan_u(
        &owed,
        &Carried::default(),
        budget_for(&[&owed[5], &owed[4]]),
    );
    let mut target = read_back(&first.rows);
    let again = plan_u(&owed, &target, Budget::MOST);
    assert!(again.rows.is_empty(), "{again:?}");

    let more: Vec<Entry> = owed
        .iter()
        .cloned()
        .chain([entry(7, 70, "x"), entry(8, 80, "x")])
        .collect();
    let later = plan_u(&more, &target, budget_for(&[&more[7]]));
    assert_eq!((later.carried, later.not_carried), (1, 1));
    read_into(&mut target, &later.rows);
    let horizon = &target.horizons[&keys::history_horizon_pk(1, "u")];
    assert_eq!((horizon.through, horizon.not_carried), (more[6].order(), 5));
    assert!(uncarried(&more, &target).is_empty());
}

/// A horizon an earlier roll stated is carried on with the entries, under its roller's key, and
/// not again once the target holds it.
#[test]
fn a_stated_horizon_is_carried_on() {
    let stated = BTreeMap::from([(
        keys::history_horizon_pk(1, "u"),
        Horizon {
            generation: 1,
            by: "u".into(),
            through: (Hlc { ms: 5, counter: 0 }, op(9)),
            not_carried: 3,
        },
    )]);
    let planned = plan(
        &REGISTRY,
        Vec::new(),
        &stated,
        &Carried::default(),
        2,
        "w",
        Budget::MOST,
    )
    .unwrap();
    assert_eq!(read_back(&planned.rows).horizons, stated);
    let target = Carried {
        horizons: stated.clone(),
        ..Carried::default()
    };
    assert!(
        plan(
            &REGISTRY,
            Vec::new(),
            &stated,
            &target,
            2,
            "w",
            Budget::MOST
        )
        .unwrap()
        .rows
        .is_empty()
    );
}

/// A row states something only under the key its statement makes: an entry under the key of
/// the Mac it names and its operation, written by that Mac where the row is as it was written,
/// and a horizon under its roller's and generation's key. NEGATIVE CONTROL: with `stated` not
/// comparing the writer to the Mac the entry names, the row `w` wrote naming `u` is read.
#[test]
fn a_row_states_only_what_its_key_and_writer_bind() {
    let one = entry(1, 10, "x");
    let body = body(&one, "u", false);
    let own = keys::history_pk("u", &op(1));
    assert_eq!(
        stated(&own, &body, Some("u")),
        Some(Stated::Entry(one.clone()))
    );
    assert_eq!(stated(&own, &body, None), Some(Stated::Entry(one)));
    assert_eq!(
        stated(&own, &body, Some("w")),
        None,
        "another Mac's word for u"
    );
    assert_eq!(
        stated(&keys::history_pk("w", &op(1)), &body, Some("u")),
        None
    );
    assert_eq!(
        stated(&keys::history_pk("u", &op(2)), &body, Some("u")),
        None
    );

    let horizon = Horizon {
        generation: 1,
        by: "u".into(),
        through: (Hlc { ms: 5, counter: 0 }, op(9)),
        not_carried: 1,
    };
    let body = horizon_body(&horizon);
    assert_eq!(
        stated(&keys::history_horizon_pk(1, "u"), &body, Some("w")),
        Some(Stated::Horizon(horizon))
    );
    assert_eq!(stated(&keys::history_horizon_pk(2, "u"), &body, None), None);
    assert_eq!(stated(&keys::history_horizon_pk(1, "w"), &body, None), None);
}

/// A secret column's change and a sealed token in any column are carried with their keys and
/// no value, and so is a change an earlier roll carried with a value that is secret here; an
/// ordinary value is carried as it was. NEGATIVE CONTROL: with `holds_sealed` always false, the
/// sealed token in the provider's note reaches the row and the text assertion fails.
#[test]
fn no_secret_value_is_carried() {
    let pk = keys::address_pk("ann@example.test").unwrap();
    let token = Fixed("alab/registry").sealed(
        SEALED_ADDRESS.name,
        &pk,
        "password",
        &Secret::new("synthetic-pass-7".into()),
    );
    let noted = Fixed("alab/registry").sealed(
        PROVIDER.name,
        "anthropic",
        "note",
        &Secret::new("synthetic-note-5".into()),
    );
    let changes = vec![
        Change {
            collection: SEALED_ADDRESS.name.into(),
            key: format!("{pk}.password"),
            value: Some(json!({"r": 1, "x": token})),
        },
        Change {
            collection: PROVIDER.name.into(),
            key: "anthropic.note".into(),
            value: Some(json!({"r": 1, "x": {"kept": [noted]}})),
        },
        Change {
            collection: SEALED_ADDRESS.name.into(),
            key: format!("{pk}.addr"),
            value: Some(json!({"r": 1, "x": "ann@example.test"})),
        },
    ];
    let carried: Vec<Written> = changes
        .into_iter()
        .map(|change| written(&REGISTRY, change))
        .collect();
    assert!(carried[0].withheld && carried[0].value.is_none());
    assert!(carried[1].withheld && carried[1].value.is_none());
    assert!(!carried[2].withheld && carried[2].value.is_some());
    let mut one = entry(1, 10, "x");
    one.changes = carried;
    // An entry an earlier roll carried with the password's value, which this table holds secret.
    let mut earlier = entry(2, 20, "x");
    earlier.changes = vec![Written {
        collection: SEALED_ADDRESS.name.into(),
        key: format!("{pk}.password"),
        value: Some(json!({"r": 1, "x": "synthetic-pass-8"})),
        withheld: false,
    }];
    let planned = plan_u(&[one, earlier], &Carried::default(), Budget::MOST);
    let back = read_back(&planned.rows);
    assert!(back.entries[&op(2)].changes[0].withheld);
    let text = aterm_json::to_string(
        &back
            .entries
            .values()
            .flat_map(|entry| &entry.changes)
            .map(|w| json!({"k": w.key, "x": w.value}))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    for secret in [
        "sec1",
        "synthetic-pass-7",
        "synthetic-pass-8",
        "synthetic-note-5",
    ] {
        assert!(!text.contains(secret), "{secret} in {text}");
    }
    assert!(text.contains("ann@example.test"));
}

/// An entry whose values pass what one row carries goes in with its keys only.
#[test]
fn an_oversized_entry_keeps_its_keys() {
    let mut big = entry(1, 10, "x");
    big.changes = (0..40)
        .map(|n| Written {
            collection: PROVIDER.name.into(),
            key: format!("p{n}.note"),
            value: Some(json!({"r": 1, "x": "n".repeat(2000)})),
            withheld: false,
        })
        .collect();
    let planned = plan_u(&[big], &Carried::default(), Budget::MOST);
    let back = &read_back(&planned.rows).entries[&op(1)];
    assert!(back.trimmed);
    assert_eq!(back.changes.len(), 40);
    assert!(
        back.changes
            .iter()
            .all(|w| w.value.is_none() && !w.withheld)
    );
}

/// History takes only the room the writer has left after the rest of the roll and a reserve,
/// and a roll that passes the writer's share alone has none. With no room left, the roll still
/// writes its horizon, which states every entry it left out. NEGATIVE CONTROL: with `Meter::take`
/// not checking the record budget, the roll with no records left carries all three.
#[test]
fn history_takes_only_the_room_the_writer_has_left() {
    assert_eq!(Budget::room(WRITER_RECORDS + 1, 0), None);
    assert_eq!(Budget::room(0, WRITER_BYTES + 1), None);
    assert_eq!(
        Budget::room(0, 0),
        Some(Budget {
            bytes: MAX_HISTORY_BYTES,
            records: WRITER_RECORDS - RESERVE_RECORDS,
        })
    );
    let tight = Budget::room(
        WRITER_RECORDS - RESERVE_RECORDS,
        WRITER_BYTES - RESERVE_BYTES,
    )
    .unwrap();
    assert_eq!(
        tight,
        Budget {
            bytes: 0,
            records: 0
        }
    );

    let owed: Vec<Entry> = (1..=3).map(|n| entry(n, u64::from(n) * 10, "x")).collect();
    let none = Budget {
        bytes: MAX_HISTORY_BYTES,
        records: 0,
    };
    let planned = plan_u(&owed, &Carried::default(), none);
    assert_eq!((planned.carried, planned.not_carried), (0, 3));
    assert_eq!(planned.rows.len(), 1);
    assert!(uncarried(&owed, &read_back(&planned.rows)).is_empty());

    // Rows are counted as `att::pack` packs them: two small rows share one record.
    let rows: Vec<_> = owed
        .iter()
        .take(2)
        .map(|e| entry_row(e, "u").unwrap())
        .collect();
    assert_eq!(
        appended(&rows).unwrap().0,
        crate::att::pack(rows).unwrap().len()
    );
}

/// A roll's own records restate; they are not history.
#[test]
fn a_rolls_own_records_are_not_history() {
    let carried = Change {
        collection: PROVIDER.name.into(),
        key: "anthropic.site".into(),
        value: Some(cell::carry(&BTreeMap::from([(op(1), json!({"r": 1, "x": "a"}))])).unwrap()),
    };
    let cover = Change {
        collection: ROLL.name.into(),
        key: "rl0.from".into(),
        value: Some(json!({"r": 1, "x": 1})),
    };
    assert!(restates(&[carried.clone(), cover]));
    let edit = Change {
        collection: PROVIDER.name.into(),
        key: "anthropic.site".into(),
        value: Some(json!({"r": 1, "x": "b"})),
    };
    assert!(!restates(&[carried, edit]));
}
