// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Self-registration: the adoption rule over fixtures, then real labs through `Registry::register`
//! on scratch Commons state. Each host reading is synthetic; `Passes` takes its probe from here.

use super::{
    super::{
        Registry, checks,
        registry::{MACHINE, MACHINE_FACTS, MODEL},
        tests::{apply, machine_of},
    },
    run::Passes,
    *,
};
use crate::{
    database::Change,
    table::tests::{Lab, repair_commons},
    typed::{
        cell::{self, CellKey, CellValue, Cells},
        keys,
        owned::{OwnedRead, OwnedRow},
    },
};
use aterm_json::{Value, json};
use aterm_secrets::commons::roster::Role;
use std::{collections::BTreeSet, time::Instant};

const NOW: u64 = 400 * 86_400_000;
const NODE: &str = "0123456789abcdef0123456789abcdef";
const OTHER_NODE: &str = "fedcba9876543210fedcba9876543210";
const HINT: &str = "aaaaaaaaaaaaaaaa";
const ME: &str = "11111111111111111111111111111111";
const OLD: &str = "22222222222222222222222222222222";

fn me() -> Me {
    Me {
        uuid: ME.into(),
        label: Some("m7".into()),
        hw_hint: HINT.into(),
        enrolled_ms: None,
    }
}

fn probe() -> Probe {
    Probe {
        hw_model: "Mac16,8".into(),
        chip: Some("Apple M4 Max".into()),
        cores_p: Some(12),
        cores_e: Some(4),
        gpu_cores: Some(40),
        ram_gb: Some(128),
        disk_gb: Some(2000),
        arch: Some("aarch64".into()),
        os_major: Some(27),
        hosts: vec!["mac-seven".into()],
        node: Some(NODE.into()),
        platform_uuid: "C3396674-FCD3-52E5-A1C1-BD5B9E8F5C43".into(),
    }
}

fn machine(pk: &str, cells: &[(&str, Value)]) -> Vec<Change> {
    cells
        .iter()
        .map(|(col, x)| {
            cell::change(
                &MACHINE,
                CellKey {
                    pk,
                    col,
                    elem: None,
                },
                &CellValue::owner(1, x.clone()),
            )
            .unwrap()
        })
        .collect()
}

fn alias(pk: &str, label: &str) -> Change {
    cell::change(
        &MACHINE,
        CellKey {
            pk,
            col: "aliases",
            elem: Some(label),
        },
        &CellValue::owner(1, json!(true)),
    )
    .unwrap()
}

fn facts(rows: &[(&str, Value)]) -> OwnedRead {
    OwnedRead {
        rows: rows
            .iter()
            .map(|(uuid, value)| {
                (
                    (*uuid).to_owned(),
                    OwnedRow {
                        machine: [0; 16],
                        actor: [0; 48],
                        value: value.clone(),
                        rank: (0, 1, 1),
                        operation: [0; 32],
                    },
                )
            })
            .collect(),
        inert: 0,
    }
}

fn run(
    me: &Me,
    probe: &Probe,
    live: &[&str],
    heard: bool,
    cells: &Cells,
    read: &OwnedRead,
) -> Plan {
    let live: BTreeSet<String> = live.iter().map(|uuid| (*uuid).to_owned()).collect();
    plan(
        &Input {
            me,
            probe,
            live: &live,
            heard,
            now_ms: NOW,
        },
        cells,
        read,
    )
    .unwrap()
}

fn written_row(plan: &Plan) -> &Value {
    plan.changes
        .iter()
        .find(|c| c.collection == MACHINE_FACTS.name)
        .and_then(|c| c.value.as_ref())
        .unwrap()
}

fn kinds(plan: &Plan) -> Vec<&'static str> {
    plan.findings.iter().map(|f| f.kind).collect()
}

/// A row an earlier enrollment left: gone from the roster, seen long before this one enrolled.
fn old_row(mac: &str, seen: u64) -> Value {
    json!({"r": 1, "n": 3, "mac": mac, "label": "m7", "hw_hint": HINT, "seen": seen, "hosts": []})
}

/// The rule runs in the design's order, each rule beating every later one: with a reinstall's
/// evidence, a node's row, a host's row and a planned label's row all on offer, the reinstall
/// wins and links its predecessor; without it the node; then the host; then the planned label.
/// NEGATIVE CONTROL: with `pick` ignoring the reinstall, the first assertion reads
/// `Adopted { rule: Node, .. }`.
#[test]
fn adoption_runs_the_rules_in_order() {
    let (by_hint, by_node, by_host, by_label) = (
        "mc-1000000000000001",
        "mc-1000000000000002",
        "mc-1000000000000003",
        "mc-1000000000000004",
    );
    let cells = |rows: &[&str]| {
        let mut records = Vec::new();
        if rows.contains(&by_hint) {
            records.push(machine(
                by_hint,
                &[("label", json!("m70")), ("status", json!("active"))],
            ));
        }
        if rows.contains(&by_node) {
            records.push(machine(
                by_node,
                &[
                    ("label", json!("m71")),
                    ("status", json!("active")),
                    ("fleet_node", json!(NODE)),
                ],
            ));
        }
        if rows.contains(&by_host) {
            records.push(machine(
                by_host,
                &[
                    ("label", json!("m72")),
                    ("status", json!("active")),
                    ("hosts", json!(["MAC-SEVEN"])),
                ],
            ));
        }
        if rows.contains(&by_label) {
            records.push(machine(
                by_label,
                &[("label", json!("m7")), ("status", json!("planned"))],
            ));
        }
        apply(&records)
    };
    let all = cells(&[by_hint, by_node, by_host, by_label]);
    let earlier = facts(&[(OLD, old_row(by_hint, 300))]);
    let first = run(&me(), &probe(), &[ME], true, &all, &earlier);
    assert_eq!(
        first.outcome,
        Outcome::Adopted {
            rule: Rule::Reinstall,
            mac: by_hint.into()
        }
    );
    assert_eq!(written_row(&first)["predecessor"], json!(OLD));
    assert_eq!(written_row(&first)["mac"], json!(by_hint));
    let none = facts(&[]);
    let second = run(&me(), &probe(), &[ME], true, &all, &none);
    assert_eq!(
        second.outcome,
        Outcome::Adopted {
            rule: Rule::Node,
            mac: by_node.into()
        }
    );
    assert!(written_row(&second).get("predecessor").is_none());
    let third = run(
        &me(),
        &probe(),
        &[ME],
        true,
        &cells(&[by_host, by_label]),
        &none,
    );
    assert_eq!(
        third.outcome,
        Outcome::Adopted {
            rule: Rule::Host,
            mac: by_host.into()
        }
    );
    let fourth = run(&me(), &probe(), &[ME], true, &cells(&[by_label]), &none);
    assert_eq!(
        fourth.outcome,
        Outcome::Adopted {
            rule: Rule::Planned,
            mac: by_label.into()
        }
    );
    // No Mac edits an existing machine row, in any of these.
    for adopted in [&first, &second, &third, &fourth] {
        assert!(adopted.changes.iter().all(|c| c.collection != MACHINE.name));
    }
}

/// A hint match without §6's evidence is a finding and never an adoption: an old uuid still live
/// (a clone running now), or gone but seen within 30 days of this enrollment, or a row a later
/// enrollment of that machine outdates. Each Mac then goes on down the rule, here to a birth of
/// its own, and the owner sees why. The enrollment date is the roster's, not today's: an old row
/// seen after this Mac was admitted ran beside it.
/// NEGATIVE CONTROL: with `reinstall` not consulting `input.live`, the live clone is adopted:
/// `` left: Adopted { rule: Reinstall, .. } ``.
#[test]
fn a_hint_match_without_the_evidence_is_a_finding_never_an_adoption() {
    let mac = "mc-1000000000000001";
    let cells = apply(&[machine(
        mac,
        &[("label", json!("m70")), ("status", json!("active"))],
    )]);
    let clone = run(
        &me(),
        &probe(),
        &[ME, OLD],
        true,
        &cells,
        &facts(&[(OLD, old_row(mac, 10))]),
    );
    assert!(
        matches!(clone.outcome, Outcome::Born { .. }),
        "{:?}",
        clone.outcome
    );
    assert_eq!(kinds(&clone), ["hw-hint-live"]);
    let recent = run(
        &me(),
        &probe(),
        &[ME],
        true,
        &cells,
        &facts(&[(OLD, old_row(mac, 380))]),
    );
    assert!(matches!(recent.outcome, Outcome::Born { .. }));
    assert_eq!(kinds(&recent), ["hw-hint-unproven"]);
    // Exactly 30 days is not more than 30.
    let edge = run(
        &me(),
        &probe(),
        &[ME],
        true,
        &cells,
        &facts(&[(OLD, old_row(mac, 370))]),
    );
    assert!(matches!(edge.outcome, Outcome::Born { .. }));
    // Past 30 days by today, but this Mac was admitted before that row was last seen.
    let admitted = Me {
        enrolled_ms: Some(290 * 86_400_000),
        ..me()
    };
    let beside = run(
        &admitted,
        &probe(),
        &[ME],
        true,
        &cells,
        &facts(&[(OLD, old_row(mac, 300))]),
    );
    assert!(matches!(beside.outcome, Outcome::Born { .. }));
    assert_eq!(kinds(&beside), ["hw-hint-unproven"]);
    // An old row outdated by a later enrollment of that machine proves nothing.
    let third = "33333333333333333333333333333333";
    let outdated = run(
        &me(),
        &probe(),
        &[ME],
        true,
        &cells,
        &facts(&[(OLD, old_row(mac, 10)), (third, old_row(mac, 390))]),
    );
    assert!(matches!(outdated.outcome, Outcome::Born { .. }));
    // A different hint is no match at all.
    let stranger = Me {
        hw_hint: "bbbbbbbbbbbbbbbb".into(),
        ..me()
    };
    let unrelated = run(
        &stranger,
        &probe(),
        &[ME],
        true,
        &cells,
        &facts(&[(OLD, old_row(mac, 10))]),
    );
    assert!(unrelated.findings.is_empty());
}

/// A row bound by a live enrollment is nobody else's; two rows on offer adopt neither and wait
/// for the owner, because a birth beside them would make a third; a row stating another fleet
/// node is another Mac, whatever host or label it shares with this one; a machine row an owner
/// ended is never adopted; a birth never goes beside a live row holding its label; and a birth
/// names no node or host a live row names.
/// NEGATIVE CONTROLS: with `no_other_node` always true, the row naming another node is adopted by
/// host: `` left: Adopted { rule: Host, .. } ``. With the `holders` check removed from `plan`, the
/// active row's Mac is `Born`. With `machine_birth` not filtering, the birth states `NODE`.
#[test]
fn a_candidate_someone_else_holds_or_that_names_another_mac_is_not_adopted() {
    let (a, b) = ("mc-1000000000000001", "mc-1000000000000002");
    let none = facts(&[]);
    let row = |pk, node: &str| {
        machine(
            pk,
            &[
                ("label", json!("m70")),
                ("status", json!("active")),
                ("fleet_node", json!(node)),
            ],
        )
    };
    // Bound elsewhere: the Mac goes on to a birth of its own, with a finding.
    let held = apply(&[row(a, NODE)]);
    let other_mac = facts(&[(OLD, old_row(a, 399))]);
    let plan = run(
        &Me {
            hw_hint: "bbbbbbbbbbbbbbbb".into(),
            ..me()
        },
        &probe(),
        &[ME, OLD],
        true,
        &held,
        &other_mac,
    );
    assert!(matches!(plan.outcome, Outcome::Born { .. }));
    assert_eq!(kinds(&plan), ["node-bound-elsewhere"]);
    // Its birth names neither that node nor that row's hosts: the genuine Mac's row keeps them.
    let clone_cells = apply(&[machine(
        a,
        &[
            ("label", json!("m70")),
            ("status", json!("active")),
            ("fleet_node", json!(NODE)),
            ("hosts", json!(["MAC-SEVEN.local"])),
        ],
    )]);
    let plan = run(
        &Me {
            hw_hint: "bbbbbbbbbbbbbbbb".into(),
            ..me()
        },
        &Probe {
            hosts: vec!["mac-seven".into(), "mac-mine".into()],
            ..probe()
        },
        &[ME, OLD],
        true,
        &clone_cells,
        &other_mac,
    );
    let birth = cell::project(
        &MACHINE,
        &apply(&[plan
            .changes
            .iter()
            .filter(|c| c.collection == MACHINE.name)
            .cloned()
            .collect()]),
    );
    let born = &birth.rows[&keys::machine_pk(ME)];
    assert_eq!(born.get("fleet_node"), None);
    assert_eq!(born.get("hosts"), Some(&json!(["mac-mine"])));
    assert_eq!(
        written_row(&plan)["node"],
        json!(NODE),
        "the facts row states what it read"
    );
    // Two live rows naming this node hold the Mac.
    let two = apply(&[
        row(a, NODE),
        machine(b, &[("label", json!("m71")), ("fleet_node", json!(NODE))]),
    ]);
    let plan = run(&me(), &probe(), &[ME], true, &two, &none);
    assert_eq!(
        (plan.outcome.clone(), kinds(&plan)),
        (Outcome::Held, vec!["node-ambiguous"])
    );
    assert!(plan.changes.is_empty());
    // Two rows naming one host hold it too.
    let hosts = apply(&[
        machine(
            a,
            &[("label", json!("m70")), ("hosts", json!(["mac-seven"]))],
        ),
        machine(
            b,
            &[("label", json!("m71")), ("hosts", json!(["mac-seven"]))],
        ),
    ]);
    let plan = run(&me(), &probe(), &[ME], true, &hosts, &none);
    assert_eq!(
        (plan.outcome.clone(), kinds(&plan)),
        (Outcome::Held, vec!["host-ambiguous"])
    );
    // Another node's row shares this Mac's host and label.
    let foreign = apply(&[machine(
        a,
        &[
            ("label", json!("m7")),
            ("status", json!("planned")),
            ("fleet_node", json!(OTHER_NODE)),
            ("hosts", json!(["mac-seven"])),
        ],
    )]);
    let plan = run(&me(), &probe(), &[ME], true, &foreign, &none);
    assert_eq!(
        (plan.outcome.clone(), kinds(&plan)),
        (Outcome::Held, vec!["label-held"])
    );
    // An ended row stays ended.
    let ended = apply(&[machine(
        a,
        &[
            ("label", json!("m70")),
            ("fleet_node", json!(NODE)),
            ("row", json!("retired")),
        ],
    )]);
    let plan = run(&me(), &probe(), &[ME], true, &ended, &none);
    assert!(matches!(plan.outcome, Outcome::Born { .. }));
    // A planned row under an alias is adopted; an active one under that label is not rule 3's.
    let mut with_alias = machine(a, &[("label", json!("m99")), ("status", json!("planned"))]);
    with_alias.push(alias(a, "m7"));
    let plan = run(
        &me(),
        &Probe {
            node: None,
            ..probe()
        },
        &[ME],
        true,
        &apply(&[with_alias]),
        &none,
    );
    assert_eq!(
        plan.outcome,
        Outcome::Adopted {
            rule: Rule::Planned,
            mac: a.into()
        }
    );
    let active = apply(&[machine(
        a,
        &[("label", json!("m7")), ("status", json!("active"))],
    )]);
    let plan = run(
        &me(),
        &Probe {
            node: None,
            ..probe()
        },
        &[ME],
        true,
        &active,
        &none,
    );
    // It holds the label, so no birth goes beside it, and its preferences stay reachable.
    assert_eq!(
        (plan.outcome.clone(), kinds(&plan)),
        (Outcome::Held, vec!["label-held"])
    );
    assert!(plan.changes.is_empty());
    // An alias holds it too; an ended row does not.
    let mut under_alias = machine(a, &[("label", json!("m99")), ("status", json!("active"))]);
    under_alias.push(alias(a, "m7"));
    let plan = run(
        &me(),
        &Probe {
            node: None,
            ..probe()
        },
        &[ME],
        true,
        &apply(&[under_alias]),
        &none,
    );
    assert_eq!(kinds(&plan), ["label-held"]);
    let ended = apply(&[machine(
        a,
        &[("label", json!("m7")), ("row", json!("retired"))],
    )]);
    let plan = run(&me(), &probe(), &[ME], true, &ended, &none);
    assert!(matches!(plan.outcome, Outcome::Born { .. }));
}

/// A first row waits for a completed repair round with a peer holding the registry, an adoption
/// as much as a birth, and then lands in one record with its machine and model. A second Mac of
/// that model births no model; a retired row at this uuid's own key is not revived.
/// NEGATIVE CONTROL: with the `!input.heard` check removed from `plan`, the first assertion reads
/// `` Born { mac: "mc-…" } ``.
#[test]
fn a_birth_waits_for_a_round_and_lands_with_its_machine_and_model() {
    let none = facts(&[]);
    let empty = Cells::new();
    let deferred = run(&me(), &probe(), &[ME], false, &empty, &none);
    assert_eq!(deferred.outcome, Outcome::Deferred);
    assert!(deferred.changes.is_empty());
    let planned = apply(&[machine(
        "mc-1000000000000001",
        &[("label", json!("m7")), ("status", json!("planned"))],
    )]);
    assert_eq!(
        run(&me(), &probe(), &[ME], false, &planned, &none).outcome,
        Outcome::Deferred
    );
    let born = run(&me(), &probe(), &[ME], true, &empty, &none);
    let pk = keys::machine_pk(ME);
    assert_eq!(born.outcome, Outcome::Born { mac: pk.clone() });
    let cells = apply(&[born
        .changes
        .iter()
        .filter(|c| c.collection != MACHINE_FACTS.name)
        .cloned()
        .collect()]);
    let machines = cell::project(&MACHINE, &cells);
    assert_eq!(machines.rows[&pk].get("label"), Some(&json!("m7")));
    assert_eq!(machines.rows[&pk].get("status"), Some(&json!("active")));
    assert_eq!(machines.rows[&pk].get("model"), Some(&json!("Mac16-8")));
    assert_eq!(machines.rows[&pk].get("fleet_node"), Some(&json!(NODE)));
    let models = cell::project(&MODEL, &cells);
    assert_eq!(
        models.rows["Mac16-8"].get("chip_family"),
        Some(&json!("M4"))
    );
    for change in born
        .changes
        .iter()
        .filter(|c| c.collection != MACHINE_FACTS.name)
    {
        assert_eq!(
            change.value.as_ref().unwrap()["a"],
            json!(true),
            "{}",
            change.key
        );
    }
    let row = written_row(&born);
    assert_eq!(row["mac"], json!(pk));
    assert_eq!(row["n"], json!(1));
    assert_eq!(row["seen"], json!(400));
    assert_eq!(row["hw_hint"], json!(HINT));
    assert_eq!(row["hw_model"], json!("Mac16-8"));
    assert!(
        !aterm_json::to_string(row).unwrap().contains("C3396674"),
        "the platform uuid never leaves"
    );
    let second = Me {
        uuid: OLD.into(),
        label: Some("m8".into()),
        ..me()
    };
    let next = run(&second, &probe(), &[OLD], true, &cells, &none);
    assert_eq!(
        next.changes
            .iter()
            .filter(|c| c.collection == MODEL.name)
            .count(),
        0
    );
    let retired = apply(&[machine(
        &pk,
        &[("label", json!("m7")), ("row", json!("retired"))],
    )]);
    let plan = run(
        &me(),
        &Probe {
            node: None,
            ..probe()
        },
        &[ME],
        true,
        &retired,
        &none,
    );
    assert_eq!(
        (plan.outcome.clone(), kinds(&plan)),
        (Outcome::Held, vec!["own-machine-retired"])
    );
    // No label to state: the row waits, and nothing is guessed.
    let unnamed = Me {
        label: None,
        ..me()
    };
    let plan = run(&unnamed, &probe(), &[ME], true, &empty, &none);
    assert_eq!(
        (plan.outcome.clone(), kinds(&plan)),
        (Outcome::Held, vec!["no-label"])
    );
}

/// A row is rewritten only on a change or when `seen` is 30 days old: a same reading writes
/// nothing; a changed one writes once, at most once a day; a read that failed keeps the cell; the
/// binding, the predecessor and the Mac's merge acknowledgment always carry over; and the order
/// host names were read in is no change. A changed host name alone waits for that rewrite.
/// NEGATIVE CONTROLS: with `due` being only `changed`, the same reading 30 days on writes nothing:
/// `` left: Unchanged ``. With `hosts` counted in `changed`, the renamed Mac reads `Refreshed`.
#[test]
fn a_facts_row_is_written_only_on_a_change_or_at_thirty_days() {
    let empty = Cells::new();
    let born = run(&me(), &probe(), &[ME], true, &empty, &facts(&[]));
    let stored = written_row(&born).clone();
    let read = |seen: u64| {
        let mut row = stored.clone();
        if let Value::Object(object) = &mut row {
            object.insert("seen".into(), json!(seen));
            object.insert("predecessor".into(), json!(OLD));
            object.insert("merged_into".into(), json!("mc-1000000000000009"));
        }
        facts(&[(ME, row)])
    };
    assert_eq!(
        run(&me(), &probe(), &[ME], true, &empty, &read(399)).outcome,
        Outcome::Unchanged
    );
    // Whether or not a round was heard: a Mac with a row keeps it fresh.
    let aged = run(&me(), &probe(), &[ME], false, &empty, &read(370));
    assert!(
        matches!(aged.outcome, Outcome::Refreshed { .. }),
        "{:?}",
        aged.outcome
    );
    let aged_row = written_row(&aged);
    assert_eq!(aged_row["n"], json!(2));
    assert_eq!(aged_row["seen"], json!(400));
    assert_eq!(aged_row["predecessor"], json!(OLD));
    assert_eq!(aged_row["merged_into"], json!("mc-1000000000000009"));
    assert_eq!(aged_row["mac"], stored["mac"]);
    assert_eq!(
        run(&me(), &probe(), &[ME], false, &empty, &read(371)).outcome,
        Outcome::Unchanged
    );
    let upgraded = Probe {
        os_major: Some(28),
        ..probe()
    };
    let changed = run(&me(), &upgraded, &[ME], true, &empty, &read(399));
    assert!(matches!(changed.outcome, Outcome::Refreshed { .. }));
    assert_eq!(written_row(&changed)["os_major"], json!(28));
    assert_eq!(
        run(&me(), &upgraded, &[ME], true, &empty, &read(400)).outcome,
        Outcome::Unchanged
    );
    let failed = Probe {
        chip: None,
        ram_gb: None,
        gpu_cores: None,
        hosts: Vec::new(),
        node: None,
        ..probe()
    };
    assert_eq!(
        run(&me(), &failed, &[ME], true, &empty, &read(399)).outcome,
        Outcome::Unchanged
    );
    let more = Probe {
        hosts: vec!["zeta".into(), "mac-seven".into()],
        ..probe()
    };
    // A host name alone spends no record: a network or a Bonjour clash moves it, and it rides the
    // 30-day rewrite.
    assert_eq!(
        run(&me(), &more, &[ME], true, &empty, &read(399)).outcome,
        Outcome::Unchanged
    );
    let two = run(&me(), &more, &[ME], true, &empty, &read(370));
    assert_eq!(written_row(&two)["hosts"], json!(["mac-seven", "zeta"]));
    let reversed = Probe {
        hosts: vec!["mac-seven".into(), "zeta".into()],
        ..probe()
    };
    let again = run(&me(), &reversed, &[ME], true, &empty, &read(370));
    assert_eq!(written_row(&two)["hosts"], written_row(&again)["hosts"]);
    // A chip name its column refuses keeps what the row held, and the row still goes out.
    let long = Probe {
        chip: Some("x".repeat(40)),
        os_major: Some(28),
        ..probe()
    };
    let refused = run(&me(), &long, &[ME], true, &empty, &read(399));
    assert_eq!(written_row(&refused)["chip"], stored["chip"]);
}

/// A facts row bound to a machine row the owner ended holds the Mac and writes nothing, however
/// old its `seen`, with a finding naming that row; while the row is live, or not yet received,
/// the same facts row is refreshed as usual.
/// NEGATIVE CONTROL: with the retired-row check removed from `plan`, the Mac keeps restating its
/// facts under the ended row: `` left: Refreshed { mac: "mc-3333333333333333" } right: Held ``.
#[test]
fn a_mac_bound_to_an_ended_row_holds_and_says_so() {
    const MAC: &str = "mc-3333333333333333";
    let read = facts(&[(ME, old_row(MAC, 300))]);
    let ended = apply(&[machine(
        MAC,
        &[("label", json!("m7")), ("row", json!("retired"))],
    )]);
    let plan = run(&me(), &probe(), &[ME], true, &ended, &read);
    assert_eq!(plan.outcome, Outcome::Held);
    assert_eq!(kinds(&plan), vec!["bound-machine-retired"]);
    assert_eq!(plan.findings[0].pks, vec![MAC.to_owned()]);
    assert!(plan.changes.is_empty());
    let live = apply(&[machine(MAC, &[("label", json!("m7"))])]);
    for cells in [&live, &Cells::new()] {
        let plan = run(&me(), &probe(), &[ME], true, cells, &read);
        assert_eq!(
            plan.outcome,
            Outcome::Refreshed {
                mac: MAC.to_owned()
            }
        );
    }
}

/// The hint is keyed by the network: stable for one Mac in one network whatever the uuid's case,
/// different in another network and for another Mac, and shaped for its 16-hex column. It holds
/// no part of the uuid.
/// NEGATIVE CONTROL: with `hw_hint` not mixing the network into its key, the second assertion
/// fails.
#[test]
fn the_hardware_hint_is_keyed_by_the_network() {
    let uuid = "C3396674-FCD3-52E5-A1C1-BD5B9E8F5C43";
    let hint = keys::hw_hint(&[1; 16], uuid);
    assert_eq!(hint, keys::hw_hint(&[1; 16], &uuid.to_ascii_lowercase()));
    assert_ne!(hint, keys::hw_hint(&[2; 16], uuid));
    assert_ne!(
        hint,
        keys::hw_hint(&[1; 16], "C3396674-FCD3-52E5-A1C1-BD5B9E8F5C44")
    );
    assert_eq!(hint.len(), 16);
    assert!(hint.bytes().all(|b| b.is_ascii_hexdigit()));
    assert!(!uuid.to_ascii_lowercase().contains(&hint));
}

/// A published `hw_hint` is the key `check` and a Mac's own registration match rows by, so the
/// hint one network gives one Mac never moves: these 16 hex are what ring's HMAC-SHA256 wrote,
/// and `openssl dgst -sha256 -mac HMAC` gives the same under the key this function derives.
/// NEGATIVE CONTROL: with the message's domain or the key's seed spelled otherwise, this fails.
#[test]
fn the_hardware_hint_keeps_the_bytes_ring_computed() {
    assert_eq!(
        keys::hw_hint(&[1; 16], "C3396674-FCD3-52E5-A1C1-BD5B9E8F5C43"),
        "4b0ccd5bc5c64927"
    );
}

/// `check` names two live enrollments bound to one machine, one hint on two live uuids, a row
/// that binds no known machine, and facts that differ from the owner's statement; gone uuids
/// are history, not findings.
/// NEGATIVE CONTROL: with `checks::facts` not filtering to `live`, the gone uuid's row joins and
/// the last assertion fails.
#[test]
fn check_reports_shared_bindings_and_hints_among_live_macs() {
    let mac = "mc-1000000000000001";
    let cells = apply(&[machine(
        mac,
        &[("label", json!("m7")), ("ram_gb", json!(64))],
    )]);
    let machines = cell::project(&MACHINE, &cells);
    let row = |mac: &str| json!({"n": 1, "mac": mac, "hw_hint": HINT, "ram_gb": 128});
    let read = facts(&[(ME, row(mac)), (OLD, row(mac))]);
    let live: BTreeSet<String> = [ME, OLD].into_iter().map(str::to_owned).collect();
    let found: Vec<&str> = checks::facts(&read, &live, &machines)
        .iter()
        .map(|f| f.kind)
        .collect();
    assert_eq!(
        found,
        [
            "facts-bind-one-machine",
            "facts-differ",
            "facts-differ",
            "hw-hint-clone"
        ]
    );
    let unknown = facts(&[(ME, row("mc-1000000000000002"))]);
    let found: Vec<&str> = checks::facts(&unknown, &live, &machines)
        .iter()
        .map(|f| f.kind)
        .collect();
    assert_eq!(found, ["facts-machine-unknown"]);
    let one: BTreeSet<String> = [ME.to_owned()].into_iter().collect();
    let found: Vec<&str> = checks::facts(&read, &one, &machines)
        .iter()
        .map(|f| f.kind)
        .collect();
    assert_eq!(found, ["facts-differ"]);
}

fn lab_config(lab: &Lab, n: usize) -> std::path::PathBuf {
    lab.paths[n].home.join("config.toml")
}

/// Give each Mac of `lab` its vault roster row, which names it `m1`, `m2`, ...: a lab's Commons log
/// carries no label of its own.
fn name_macs(lab: &Lab) {
    aterm_secrets::client::admin_seed_swarm_roster(&lab.paths[0], &[]).unwrap();
    for paths in &lab.paths[1..] {
        aterm_secrets::client::sync(paths, None).unwrap();
    }
}

/// On a real lab, a pass declares nothing: with no registry it writes nothing and leaves none
/// declared, under `[commons] exclude_tables` it does not touch a held registry, and once a peer's
/// repair brings the registry, each Mac writes its own row at its own key, with a birth of its
/// own, and a second pass appends no record.
/// NEGATIVE CONTROL: with the policy check removed from `Registry::register`, the excluded pass
/// writes: `` left: Born { mac: "mc-…" } ``.
#[test]
fn a_pass_declares_nothing_respects_exclusions_and_writes_once() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    name_macs(&lab);
    let second = Registry::at(lab.paths[1].clone());
    // With no catalog here yet a pass refuses, and enrolls nothing.
    assert!(
        second
            .register(&lab_config(&lab, 1), &probe(), true, NOW)
            .is_err()
    );
    assert!(second.view().is_err(), "a pass declares nothing");
    let first = Registry::at(lab.paths[0].clone());
    first.open().unwrap();
    first.seed().unwrap();
    repair_commons(&lab, 6);
    let table = second.table().unwrap().unwrap();
    std::fs::write(
        lab_config(&lab, 1),
        format!(
            "[commons]\nexclude_tables = [\"{}\"]\n",
            aterm_codec::hex::encode_lower(&table)
        ),
    )
    .unwrap();
    let before = second.view().unwrap().ops.len();
    assert_eq!(
        second
            .register(&lab_config(&lab, 1), &probe(), true, NOW)
            .unwrap()
            .outcome,
        Outcome::Excluded
    );
    assert_eq!(second.view().unwrap().ops.len(), before);
    std::fs::write(lab_config(&lab, 1), "").unwrap();
    let deferred = second
        .register(&lab_config(&lab, 1), &probe(), false, NOW)
        .unwrap();
    assert_eq!((deferred.outcome, deferred.records), (Outcome::Deferred, 0));
    let uuid = keys::uuid32(&machine_of(&lab, 1));
    let done = second
        .register(&lab_config(&lab, 1), &probe(), true, NOW)
        .unwrap();
    assert_eq!(
        done.outcome,
        Outcome::Born {
            mac: keys::machine_pk(&uuid)
        }
    );
    assert_eq!(done.records, 1);
    let view = second.view().unwrap();
    let row = &view.owned[MACHINE_FACTS.name].rows[&uuid];
    assert_eq!(row.value["mac"], json!(keys::machine_pk(&uuid)));
    assert_eq!(row.value["label"], json!("m2"));
    let trust = lab.bootstrap(1).trust();
    assert_eq!(
        row.value["hw_hint"],
        json!(keys::hw_hint(&trust.network, &probe().platform_uuid))
    );
    assert_eq!(
        view.project(&MODEL).rows["Mac16-8"].get("chip_family"),
        Some(&json!("M4"))
    );
    assert!(
        view.project(&MACHINE)
            .rows
            .contains_key(&keys::machine_pk(&uuid))
    );
    let again = second
        .register(&lab_config(&lab, 1), &probe(), true, NOW)
        .unwrap();
    assert_eq!((again.outcome, again.records), (Outcome::Unchanged, 0));
    assert_eq!(second.view().unwrap().ops.len(), view.ops.len());
    // The other Mac learns the binding by repair, and its own hardware binds its own row.
    repair_commons(&lab, 6);
    let view = first.view().unwrap();
    assert_eq!(
        view.owned[MACHINE_FACTS.name].rows[&uuid].value["mac"],
        json!(keys::machine_pk(&uuid))
    );
    let mine = keys::uuid32(&machine_of(&lab, 0));
    let hardware = Probe {
        platform_uuid: "C3396674-FCD3-52E5-A1C1-BD5B9E8F5C44".into(),
        node: Some(OTHER_NODE.into()),
        hosts: vec!["mac-eight".into()],
        ..probe()
    };
    let done = first
        .register(&lab_config(&lab, 0), &hardware, true, NOW)
        .unwrap();
    assert_eq!(
        done.outcome,
        Outcome::Born {
            mac: keys::machine_pk(&mine)
        }
    );
    assert!(done.findings.is_empty(), "{:?}", done.findings);
}

/// A pass stops at this Mac's quota of automatic records and appends nothing.
/// NEGATIVE CONTROL: with `quota_room` always true, the record lands past the quota, inert:
/// `` left: Born { .. } ``.
#[test]
fn a_pass_stays_within_the_quota() {
    let lab = Lab::new(&[Role::Writer]);
    name_macs(&lab);
    let registry = Registry::at(lab.paths[0].clone());
    registry.open().unwrap();
    let records: Vec<Vec<Change>> = (1..=24)
        .map(|n| {
            vec![
                cell::change(
                    &MODEL,
                    CellKey {
                        pk: &format!("vm-{n}"),
                        col: "row",
                        elem: None,
                    },
                    &CellValue::automatic(1, json!("live")),
                )
                .unwrap(),
            ]
        })
        .collect();
    registry.write(records).unwrap();
    let before = registry.view().unwrap().ops.len();
    let pass = registry
        .register(&lab_config(&lab, 0), &probe(), true, NOW)
        .unwrap();
    assert_eq!((pass.outcome, pass.records), (Outcome::QuotaSpent, 0));
    assert_eq!(registry.view().unwrap().ops.len(), before);
}

/// Rounds wake passes, and passes rest: the first round passes (and defers, no evidence yet), the
/// next is at rest without reading the host, a round that left nothing to fetch of the registry
/// passes at once and births, and another table's evidence is no evidence.
/// NEGATIVE CONTROL: with `news` always false, the evidence round is at rest: `` turn(..) `` is
/// `None`.
#[test]
fn rounds_wake_passes_and_only_the_registrys_evidence_ends_a_rest() {
    let lab = Lab::new(&[Role::Writer]);
    name_macs(&lab);
    let paths = lab.paths[0].clone();
    let registry = Registry::at(paths.clone());
    registry.open().unwrap();
    registry.seed().unwrap();
    let table = registry.table().unwrap().unwrap();
    let reads = std::rc::Rc::new(std::cell::Cell::new(0));
    let counted = reads.clone();
    let mut passes = Passes::new(paths, lab_config(&lab, 0), move || {
        counted.set(counted.get() + 1);
        Some(probe())
    });
    let at = Instant::now();
    let nothing = BTreeSet::new();
    let first = passes.turn(&nothing, at, NOW).unwrap().unwrap();
    assert_eq!(first.outcome, Outcome::Deferred);
    assert!(passes.turn(&nothing, at, NOW).is_none());
    assert!(passes.turn(&BTreeSet::from([[9; 32]]), at, NOW).is_none());
    assert_eq!(reads.get(), 1);
    let heard = passes
        .turn(&BTreeSet::from([table]), at, NOW)
        .unwrap()
        .unwrap();
    assert!(
        matches!(heard.outcome, Outcome::Born { .. }),
        "{:?}",
        heard.outcome
    );
    assert_eq!(reads.get(), 2);
    assert!(passes.turn(&BTreeSet::from([table]), at, NOW).is_none());
    let rested = at + super::run::PASS_EVERY;
    let later = passes.turn(&nothing, rested, NOW).unwrap().unwrap();
    assert_eq!(later.outcome, Outcome::Unchanged);
}

/// A row's label is the membership log's, else the roster's, the first that is a label.
/// NEGATIVE CONTROL: with `chosen_label` preferring the roster, the first assertion reads `m8`.
#[test]
fn the_membership_log_names_a_mac_before_the_roster() {
    use super::super::store::chosen_label;
    let word = |s: &str| Some(s.to_owned());
    assert_eq!(chosen_label(word("m7-3fa2"), word("m8")), word("m7-3fa2"));
    assert_eq!(chosen_label(None, word("m8")), word("m8"));
    assert_eq!(chosen_label(word("not a label!"), word("m8")), word("m8"));
    assert_eq!(chosen_label(None, None), None);
}

/// A host name is not an identity: with no log label and no roster row for this Mac, a host called
/// `m12-studio` still has no label, so it holds and writes nothing, whatever a roster name
/// resolver would have read off that name.
/// NEGATIVE CONTROL: with `me` taking `m12` when the log and the roster name nothing, as a resolver
/// reading the host name does, the pass is `Born`.
#[test]
fn a_host_name_never_names_a_mac() {
    let lab = Lab::new(&[Role::Writer]);
    let registry = Registry::at(lab.paths[0].clone());
    registry.open().unwrap();
    let hosts = Probe {
        hosts: vec!["m12-studio".into()],
        ..probe()
    };
    let pass = registry
        .register(&lab_config(&lab, 0), &hosts, true, NOW)
        .unwrap();
    assert_eq!((pass.outcome, pass.records), (Outcome::Held, 0));
    assert_eq!(pass.findings[0].kind, "no-label");
}

/// `aterm db alab check` says why this Mac has no row, which the registry alone cannot since there
/// is no row to read: with two live rows naming its node the answer is `held` and
/// `node-ambiguous`, nothing is written, and a build that reads no host reports none.
/// NEGATIVE CONTROL: with `check` ignoring `this_mac`, `registration` is null and the first
/// assertion fails.
#[test]
fn check_answers_why_this_mac_is_unregistered_and_writes_nothing() {
    struct Reader(std::path::PathBuf);
    impl super::super::import::Vendors for Reader {
        fn mark(&self, _: &str, _: &str, _: &str) -> super::super::import::Mark {
            super::super::import::Mark::Absent
        }
        fn this_mac(&self) -> Option<ThisMac> {
            Some(ThisMac {
                probe: probe(),
                config: self.0.clone(),
            })
        }
    }
    let lab = Lab::new(&[Role::Writer]);
    name_macs(&lab);
    let registry = Registry::at(lab.paths[0].clone());
    registry.open().unwrap();
    registry
        .write(vec![
            machine(
                "mc-1000000000000001",
                &[("label", json!("m70")), ("fleet_node", json!(NODE))],
            ),
            machine(
                "mc-1000000000000002",
                &[("label", json!("m71")), ("fleet_node", json!(NODE))],
            ),
        ])
        .unwrap();
    let before = registry.view().unwrap().ops.len();
    let words = ["check".to_owned()];
    let reader = Reader(lab_config(&lab, 0));
    let mut review = |_: &std::path::Path| Ok(());
    let report =
        super::super::command::run(&lab.paths[0], &words, Some(&reader), &mut review, NOW).unwrap();
    assert_eq!(
        report["registration"]["outcome"],
        json!("held"),
        "{}",
        report["registration"]
    );
    let lab_findings = report["lab"].as_array().unwrap();
    assert!(
        lab_findings
            .iter()
            .any(|found| found["finding"] == json!("node-ambiguous")),
        "{lab_findings:?}"
    );
    assert_eq!(registry.view().unwrap().ops.len(), before);
    let bare = super::super::command::run(&lab.paths[0], &words, None, &mut review, NOW).unwrap();
    assert_eq!(bare["registration"], Value::Null);
}

/// The whole path on real rounds: a pair of scratch Macs repairs over loopback, a round that still
/// brought records is no evidence, a later one on the registry is, and handed that round the
/// running `Registrar` (its thread, its wake) births this Mac's row, after deferring on a round
/// without evidence.
/// NEGATIVE CONTROL: with `drained_table` answering `None` for a real report, no round is evidence
/// and `expect("a drained round")` fails.
#[test]
fn a_real_drained_round_wakes_the_registrar_and_it_registers() {
    use crate::runtime::RepairRound;
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    name_macs(&lab);
    let first = Registry::at(lab.paths[0].clone());
    first.open().unwrap();
    first.seed().unwrap();
    let reports = crate::table::tests::repair_commons_reports(&lab, 12);
    let second = Registry::at(lab.paths[1].clone());
    let table = second.table().unwrap().unwrap();
    let rounds: Vec<RepairRound> = reports[1]
        .iter()
        .map(|report| RepairRound::completed_with(*report))
        .collect();
    let brought = rounds
        .iter()
        .find(|round| {
            let report = round.application.as_ref().unwrap();
            report.table == Some(table) && report.received > 0
        })
        .expect("a round that brought the registry");
    assert_eq!(brought.drained_table(), None);
    let drained = rounds
        .iter()
        .find(|round| round.drained_table() == Some(table))
        .expect("a drained round");
    let (sent, heard) = std::sync::mpsc::channel();
    let registrar = Registrar::start(
        lab.paths[1].clone(),
        lab_config(&lab, 1),
        || Some(probe()),
        move |pass: &Result<Registered>| {
            let _ = sent.send(pass.as_ref().map(|done| done.outcome.clone()).ok());
        },
    )
    .unwrap();
    let hang = std::time::Duration::from_secs(60);
    registrar.round(&RepairRound::completed_with(Default::default()));
    assert_eq!(heard.recv_timeout(hang).unwrap(), Some(Outcome::Deferred));
    registrar.round(drained);
    let uuid = keys::uuid32(&machine_of(&lab, 1));
    assert_eq!(
        heard.recv_timeout(hang).unwrap(),
        Some(Outcome::Born {
            mac: keys::machine_pk(&uuid)
        })
    );
    drop(registrar);
    assert!(
        second.view().unwrap().owned[MACHINE_FACTS.name]
            .rows
            .contains_key(&uuid)
    );
}
/// A transient host-probe failure (`gather() == None`) does not spend `heard` or start the
/// six-hour rest window, so the next round when the Mac can state itself still registers.
/// NEGATIVE CONTROL: with `Passes::turn` setting `self.heard |= heard` and `self.last = Some(now)`
/// before `(self.gather)()?`, the second `turn` returns `None` for the next six hours.
#[test]
fn a_transient_empty_probe_does_not_spend_evidence_or_start_rest() {
    let lab = Lab::new(&[Role::Writer]);
    name_macs(&lab);
    let paths = lab.paths[0].clone();
    let registry = Registry::at(paths.clone());
    registry.open().unwrap();
    registry.seed().unwrap();
    let table = registry.table().unwrap().unwrap();
    let mut ready = false;
    let mut passes = Passes::new(paths, lab_config(&lab, 0), move || {
        if !ready {
            ready = true;
            None
        } else {
            Some(probe())
        }
    });
    let at = Instant::now();
    let drained = BTreeSet::from([table]);
    assert!(passes.turn(&drained, at, NOW).is_none());
    let done = passes
        .turn(&drained, at, NOW)
        .expect("probe now ready")
        .unwrap();
    assert!(
        matches!(done.outcome, Outcome::Born { .. }),
        "{:?}",
        done.outcome
    );
}

/// A denied swarm-roster member supplies neither a fallback label nor `enrolled_ms` for reinstall
/// adoption, and `typed::check::findings` deduplicates repeated dangling references from
/// conflicted candidates.
/// NEGATIVE CONTROLS: without filtering `enrolled_ms` by `!is_member_denied`, a denied roster row
/// still returns `Some(1_700_000_000_000)`; without `found.dedup()` in `typed::check::findings`,
/// two conflicted candidates naming the same missing target emit two identical `Dangling`
/// findings.
#[test]
fn denied_roster_entries_and_duplicate_dangling_candidates_are_filtered() {
    use super::super::{command::known_macs_from_roster, store::roster_identity};
    let roster = aterm_secrets::swarm_roster::SwarmRoster::from_toml(
        r#"
deny_labels = ["m9"]

[[member]]
label = "m9"
vault_name = "m9"
recipient = "age1denied"
admitted_ms = 1700000000000
node_id = "0123456789abcdef0123456789abcdef"

[[member]]
label = "m10"
vault_name = "m10"
recipient = "age1live"
admitted_ms = 1700000000000
node_id = "fedcba9876543210fedcba9876543210"
"#,
    )
    .unwrap();
    assert_eq!(roster_identity(Some(&roster), "age1denied"), (None, None));
    assert_eq!(
        roster_identity(Some(&roster), "age1live"),
        (Some("m10".to_owned()), Some(1_700_000_000_000))
    );
    let macs = known_macs_from_roster(&roster);
    assert_eq!(macs.len(), 1);
    assert_eq!(macs[0].label.as_ref().unwrap().canonical(), "m10");

    let mut cells = Cells::new();
    let key = (
        MACHINE.name.to_owned(),
        "mc-0000000000000009.model".to_owned(),
    );
    cells.insert(
        key,
        crate::database::Cell {
            candidates: vec![
                crate::database::Candidate {
                    operation: [1; 32],
                    value: CellValue::owner(1, json!("Mac99-1")).encode(),
                },
                crate::database::Candidate {
                    operation: [2; 32],
                    value: CellValue::automatic(1, json!("Mac99-1")).encode(),
                },
            ],
            stopped: false,
        },
    );
    let machines = cell::project(&MACHINE, &cells);
    let models = cell::project(&MODEL, &cells);
    let tables =
        std::collections::BTreeMap::from([(MACHINE.name, &machines), (MODEL.name, &models)]);
    let dangling: Vec<_> = crate::typed::check::findings(&[&MACHINE, &MODEL], &tables)
        .into_iter()
        .filter(|f| matches!(f, crate::typed::check::Finding::Dangling { .. }))
        .collect();
    assert_eq!(dangling.len(), 1, "{dangling:?}");
}

/// Dropping `Registrar` waits for an in-flight `gather` pass to finish before returning, and
/// `plan` saturates `n` instead of overflowing at `u64::MAX`.
/// NEGATIVE CONTROL: with `Registrar::drop` detaching its thread without `worker.join()`,
/// `finished.load(..)` is still `false` when `drop(registrar)` returns.
#[test]
fn registrar_drop_joins_inflight_pass_and_plan_saturates_fact_counter() {
    use crate::runtime::RepairRound;
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicBool, Ordering},
    };
    let lab = Lab::new(&[Role::Writer]);
    name_macs(&lab);
    let entered = Arc::new(Barrier::new(2));
    let entered_worker = Arc::clone(&entered);
    let release = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let release_worker = Arc::clone(&release);
    let finished = Arc::new(AtomicBool::new(false));
    let finished_worker = Arc::clone(&finished);
    let registrar = Registrar::start(
        lab.paths[0].clone(),
        lab_config(&lab, 0),
        move || {
            entered_worker.wait();
            let (lock, cvar) = &*release_worker;
            let mut go = lock.lock().unwrap();
            while !*go {
                go = cvar.wait(go).unwrap();
            }
            finished_worker.store(true, Ordering::SeqCst);
            None
        },
        |_| {},
    )
    .unwrap();
    registrar.round(&RepairRound::completed_with(Default::default()));
    entered.wait();
    let release_drop = Arc::clone(&release);
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(25));
        let (lock, cvar) = &*release_drop;
        *lock.lock().unwrap() = true;
        cvar.notify_all();
    });
    drop(registrar);
    assert!(finished.load(Ordering::SeqCst));
    releaser.join().unwrap();

    let empty = Cells::new();
    let born = run(&me(), &probe(), &[ME], true, &empty, &facts(&[]));
    let mut row = written_row(&born).clone();
    if let Value::Object(object) = &mut row {
        object.insert("seen".into(), json!(300));
        object.insert("n".into(), json!(u64::MAX));
    }
    let refreshed = run(&me(), &probe(), &[ME], true, &empty, &facts(&[(ME, row)]));
    assert_eq!(written_row(&refreshed)["n"], json!(u64::MAX));
}
