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
    run::{Passes, Registrar, evidence, news, table_at},
    *,
};
use crate::{
    database::Change,
    table::tests::{Lab, repair_commons},
    typed::{
        cell::{self, CellKey, CellValue, Cells},
        grid::tests::Book,
        keys,
        owned::{OwnedRead, OwnedRow},
    },
};
use aterm_json::{Value, json};
use aterm_secrets::commons::roster::Role;
use std::{collections::BTreeSet, time::Instant};

const NOW: u64 = 400 * 86_400_000;
const NODE: &str = "0123456789abcdef";
const OTHER_NODE: &str = "fedcba9876543210";
const HINT: &str = "aaaaaaaaaaaaaaaa";
const ME: &str = "11111111111111111111111111111111";
const OLD: &str = "22222222222222222222222222222222";

#[test]
fn a_chosen_number_updates_the_bound_registry_row_without_waiting_a_day() {
    let own = keys::machine_pk(ME);
    let other = keys::machine_pk(OLD);
    let initial = vec![
        machine(
            &own,
            &[
                ("label", json!("m87")),
                ("node", json!(NODE)),
                ("row", json!("live")),
            ],
        ),
        machine(
            &other,
            &[
                ("label", json!("m87")),
                ("node", json!(OTHER_NODE)),
                ("row", json!("live")),
            ],
        ),
    ];
    let held = facts(&[(ME, old_row(&own, day_of(NOW)))]);
    let chosen = Me {
        label: Some("m3".into()),
        chosen: Some("m3".into()),
        ..me()
    };
    let plan = run(&chosen, &probe(), &[ME, OLD], true, &apply(&initial), &held);
    assert!(matches!(plan.outcome, Outcome::Refreshed { .. }));
    assert_eq!(written_row(&plan)["label"], json!("m3"));
    let mut records = initial;
    records.push(plan.changes);
    let rows = cell::project(&MACHINE, &apply(&records));
    assert_eq!(rows.rows[&own].get("label"), Some(&json!("m3")));
    assert_eq!(rows.rows[&other].get("label"), Some(&json!("m87")));

    let other_holds = apply(&[
        machine(&own, &[("label", json!("m87")), ("row", json!("live"))]),
        machine(
            &other,
            &[
                ("label", json!("m3")),
                ("node", json!(OTHER_NODE)),
                ("row", json!("live")),
            ],
        ),
    ]);
    let live = [ME.to_owned(), OLD.to_owned()].into();
    assert!(
        super::plan(
            &Input {
                me: &chosen,
                probe: &probe(),
                live: &live,
                heard: true,
                now_ms: NOW
            },
            &other_holds,
            &held
        )
        .is_err()
    );
    let mut future = rows;
    future
        .rows
        .get_mut(&own)
        .unwrap()
        .raw
        .insert("label".into());
    assert!(
        chosen_number(
            &Input {
                me: &chosen,
                probe: &probe(),
                live: &live,
                heard: true,
                now_ms: NOW,
            },
            &future,
            &held,
            &own
        )
        .is_err()
    );
}

#[test]
fn a_signed_choice_adopts_an_unbound_sheet_row_even_if_imported_active() {
    let sheet = "mc-ce20a1499bba52af";
    let cells = apply(&[machine(
        sheet,
        &[
            ("label", json!("m3")),
            ("status", json!("active")),
            ("row", json!("live")),
        ],
    )]);
    let chosen = Me {
        label: Some("m3".into()),
        chosen: Some("m3".into()),
        ..me()
    };
    let plan = run(&chosen, &probe(), &[ME], true, &cells, &facts(&[]));
    assert!(matches!(
        plan.outcome,
        Outcome::Adopted {
            rule: Rule::Planned,
            ..
        }
    ));
    assert_eq!(written_row(&plan)["mac"], json!(sheet));
    // Without a holder-signed choice, an active sheet row is not an adoption hint.
    let unsigned = Me {
        label: Some("m3".into()),
        ..me()
    };
    assert!(matches!(
        run(&unsigned, &probe(), &[ME], true, &cells, &facts(&[])).outcome,
        Outcome::Born { .. }
    ));
}

#[test]
fn an_adopted_mac_publishes_its_node_once_and_keeps_an_owner_correction() {
    let identity = me();
    let probe = probe();
    let live = [ME.to_owned()].into();
    let input = Input {
        me: &identity,
        probe: &probe,
        live: &live,
        heard: true,
        now_ms: NOW,
    };
    let pk = "mc-0000000000000007";
    let cells = apply(&[machine(pk, &[("label", json!("m7"))])]);
    let machines = cell::project(&MACHINE, &cells);
    let first = own_node(&input, &machines, pk).unwrap();
    assert_eq!(first.len(), 1);
    let cells = apply(&[machine(pk, &[("label", json!("m7"))]), first]);
    assert!(
        own_node(&input, &cell::project(&MACHINE, &cells), pk)
            .unwrap()
            .is_empty()
    );
    let corrected = apply(&[machine(pk, &[("node", json!(OTHER_NODE))])]);
    assert!(
        own_node(&input, &cell::project(&MACHINE, &corrected), pk)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn fresh_registry_labels_respect_retired_rows_and_aliases_without_a_log_hint() {
    let mut reserved = machine(
        "mc-0000000000000001",
        &[("label", json!("m1")), ("row", json!("retired"))],
    );
    reserved.push(alias("mc-0000000000000001", "m2"));
    let cells = apply(&[reserved]);
    let plan = run(
        &Me {
            label: None,
            ..me()
        },
        &Probe {
            hosts: vec!["m2-studio".into()],
            ..probe()
        },
        &[ME],
        true,
        &cells,
        &facts(&[]),
    );
    assert!(matches!(plan.outcome, Outcome::Born { .. }));
    assert_eq!(written_row(&plan)["label"], json!("m3"));
    assert_eq!(written_row(&plan)["mac"], json!(keys::machine_pk(ME)));
}

#[test]
fn a_conflicted_registry_label_reserves_each_candidate() {
    use crate::typed::cell::{Candidate, Field, Projection, Row};
    let mut machines = Projection::default();
    machines.rows.insert(
        "mc-0000000000000001".into(),
        Row {
            fields: [(
                "label".into(),
                Field::Conflict {
                    candidates: ["m1", "m2"]
                        .into_iter()
                        .map(|label| Candidate {
                            operation: [1; 32],
                            value: CellValue::owner(MACHINE.version, json!(label)),
                        })
                        .collect(),
                    provisional: Some(json!("m1")),
                },
            )]
            .into(),
            ..Row::default()
        },
    );
    let me = me();
    let probe = probe();
    let live = [ME.to_owned()].into();
    assert_eq!(
        birth_label(
            &Input {
                me: &me,
                probe: &probe,
                live: &live,
                heard: true,
                now_ms: NOW
            },
            &machines
        )
        .unwrap(),
        "m3"
    );
}

fn me() -> Me {
    Me {
        uuid: ME.into(),
        label: Some("m7".into()),
        chosen: None,
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
                &CellValue::owner(MACHINE.version, x.clone()),
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
        &CellValue::owner(MACHINE.version, json!(true)),
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
                    ("node", json!(NODE)),
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
    // Adoption can publish only the adopting Mac's node on an existing machine row.
    for adopted in [&first, &second, &third, &fourth] {
        assert!(
            adopted
                .changes
                .iter()
                .filter(|c| c.collection == MACHINE.name)
                .all(|change| change.key.ends_with(".node"))
        );
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
                ("node", json!(node)),
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
            ("node", json!(NODE)),
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
    assert_eq!(born.get("node"), None);
    assert_eq!(born.get("hosts"), Some(&json!(["mac-mine"])));
    assert!(
        written_row(&plan).get("node").is_none(),
        "the legacy facts node column cannot carry the mesh principal; the signed mac binding does"
    );
    // Two live rows naming this node hold the Mac.
    let two = apply(&[
        row(a, NODE),
        machine(b, &[("label", json!("m71")), ("node", json!(NODE))]),
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
            ("node", json!(OTHER_NODE)),
            ("hosts", json!(["mac-seven"])),
        ],
    )]);
    let plan = run(&me(), &probe(), &[ME], true, &foreign, &none);
    assert!(matches!(plan.outcome, Outcome::Born { .. }));
    assert!(
        plan.changes
            .iter()
            .filter(|change| change.collection == MACHINE.name)
            .all(|change| !change.key.starts_with(a))
    );
    // An ended row stays ended.
    let ended = apply(&[machine(
        a,
        &[
            ("label", json!("m70")),
            ("node", json!(NODE)),
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
    // A fresh registry label leaves the other Mac's row and preferences alone.
    assert!(matches!(plan.outcome, Outcome::Born { .. }));
    assert!(
        plan.changes
            .iter()
            .filter(|change| change.collection == MACHINE.name)
            .all(|change| !change.key.starts_with(a))
    );
    // An alias reserves its number too.
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
    assert!(matches!(plan.outcome, Outcome::Born { .. }));
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
    assert_eq!(machines.rows[&pk].get("label"), Some(&json!("m1")));
    assert_eq!(machines.rows[&pk].get("status"), Some(&json!("active")));
    assert_eq!(machines.rows[&pk].get("model"), Some(&json!("Mac16-8")));
    assert_eq!(machines.rows[&pk].get("node"), Some(&json!(NODE)));
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
    // A missing log hint does not prevent this enrollment from creating its registry row.
    let unnamed = Me {
        label: None,
        ..me()
    };
    let plan = run(&unnamed, &probe(), &[ME], true, &empty, &none);
    assert!(matches!(plan.outcome, Outcome::Born { .. }));
    assert_eq!(written_row(&plan)["label"], json!("m1"));
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
    let empty = apply(&[born.changes]);
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

/// Negative control: skipping migration gives this Mac the local hostname's m999; replaying
/// migration after the marker overwrites the owner's later m5 edit.
#[test]
fn legacy_labels_and_aliases_migrate_once_before_own_node_publication() {
    use aterm_secrets::{
        client,
        swarm_roster::{ROSTER_ENTRY, SwarmRoster},
        text::Audience,
    };
    let lab = Lab::new(&[Role::Writer]);
    name_macs(&lab);
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let mut doc = admin.doc.clone();
    let mut old = SwarmRoster::from_document(&doc).unwrap().unwrap();
    old.members[0].aliases.push("m101".into());
    doc.set_entry(&Audience::Everyone, ROSTER_ENTRY, old.to_toml().as_bytes())
        .unwrap();
    client::admin_publish(&lab.paths[0], &admin, &doc).unwrap();
    client::sync(&lab.paths[0], None).unwrap();
    let registry = Registry::at(lab.paths[0].clone());
    registry.open().unwrap();
    let probe = Probe {
        hosts: vec!["m999.local".into()],
        ..probe()
    };
    let first = registry
        .register(&lab_config(&lab, 0), &probe, true, NOW)
        .unwrap();
    assert!(first.records > 0);
    let view = registry.view().unwrap();
    let machines = view.project(&MACHINE);
    let (pk, row) = machines
        .rows
        .iter()
        .find(|(_, row)| row.get("label") == Some(&json!("m1")))
        .unwrap();
    assert_eq!(machines.rows.len(), 1);
    assert!(row.fields["aliases"].has_member("m101"));
    assert_eq!(row.get("node"), Some(&json!(NODE)));
    registry
        .write(vec![machine(pk, &[("label", json!("m5"))])])
        .unwrap();
    let again = registry
        .register(&lab_config(&lab, 0), &probe, true, NOW + 1)
        .unwrap();
    assert_eq!(again.records, 0);
    assert_eq!(
        registry.view().unwrap().project(&MACHINE).rows[pk].get("label"),
        Some(&json!("m5"))
    );
}

#[test]
fn a_lab_registry_moves_machine_schema_and_publishes_its_own_node_once() {
    use crate::typed::{Collection, SCHEMA, owned::SchemaState};
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    name_macs(&lab);
    let registry = Registry::at(lab.paths[0].clone());
    registry.open().unwrap();
    let old = Collection {
        version: 1,
        unique: &[&["label"], &["fleet_node"]],
        ..MACHINE
    };
    let pk = "mc-0000000000000007";
    registry
        .write(vec![vec![
            Change {
                collection: SCHEMA.into(),
                key: MACHINE.name.into(),
                value: Some(old.schema_row()),
            },
            cell::change(
                &MACHINE,
                CellKey {
                    pk,
                    col: "label",
                    elem: None,
                },
                &CellValue::owner(1, json!("m1")),
            )
            .unwrap(),
            cell::change(
                &MACHINE,
                CellKey {
                    pk,
                    col: "status",
                    elem: None,
                },
                &CellValue::owner(1, json!("planned")),
            )
            .unwrap(),
            cell::change(
                &MACHINE,
                CellKey {
                    pk,
                    col: "fleet_node",
                    elem: None,
                },
                &CellValue::owner(1, json!("00112233445566778899aabbccddeeff")),
            )
            .unwrap(),
        ]])
        .unwrap();
    assert!(matches!(
        registry.view().unwrap().schema(MACHINE.name),
        SchemaState::One { version: 1, .. }
    ));
    assert!(registry.publish_schema().unwrap() > 0);
    assert_eq!(registry.publish_schema().unwrap(), 0);
    assert!(matches!(
        registry.view().unwrap().schema(MACHINE.name),
        SchemaState::One { version: 2, .. }
    ));
    let done = registry
        .register(&lab_config(&lab, 0), &probe(), true, NOW)
        .unwrap();
    assert!(matches!(
        done.outcome,
        Outcome::Adopted {
            rule: Rule::Planned,
            ..
        }
    ));
    let view = registry.view().unwrap();
    assert_eq!(
        view.project(&MACHINE).rows[pk].get("node"),
        Some(&json!(NODE))
    );
    assert_eq!(
        view.owned[MACHINE_FACTS.name].rows[&keys::uuid32(&machine_of(&lab, 0))].value["mac"],
        json!(pk)
    );
    assert_eq!(
        registry
            .register(&lab_config(&lab, 0), &probe(), true, NOW)
            .unwrap()
            .records,
        0
    );
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
    aterm_uds::atomic_file::write(
        &lab_config(&lab, 1),
        format!(
            "[commons]\nexclude_tables = [\"{}\"]\n",
            aterm_codec::hex::encode_lower(&table)
        )
        .as_bytes(),
        0o600,
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
    aterm_uds::atomic_file::write(&lab_config(&lab, 1), b"", 0o600).unwrap();
    let deferred = second
        .register(&lab_config(&lab, 1), &probe(), false, NOW)
        .unwrap();
    assert_eq!((deferred.outcome, deferred.records), (Outcome::Deferred, 0));
    let uuid = keys::uuid32(&machine_of(&lab, 1));
    let done = second
        .register(&lab_config(&lab, 1), &probe(), true, NOW)
        .unwrap();
    let Outcome::Adopted { mac, .. } = &done.outcome else {
        panic!("the historical row should be adopted: {:?}", done.outcome);
    };
    assert!(done.records > 0);
    let view = second.view().unwrap();
    let row = &view.owned[MACHINE_FACTS.name].rows[&uuid];
    assert_eq!(row.value["mac"], json!(mac));
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
    assert!(view.project(&MACHINE).rows.contains_key(mac));
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
        json!(mac)
    );
    let hardware = Probe {
        platform_uuid: "C3396674-FCD3-52E5-A1C1-BD5B9E8F5C44".into(),
        node: Some(OTHER_NODE.into()),
        hosts: vec!["mac-eight".into()],
        ..probe()
    };
    let done = first
        .register(&lab_config(&lab, 0), &hardware, true, NOW)
        .unwrap();
    assert!(matches!(done.outcome, Outcome::Adopted { .. }));
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
/// passes at once and births, and another table's evidence is no evidence. The registry's table
/// is read only for a wake with a drained table before the evidence is heard.
/// NEGATIVE CONTROL: with `news` always false, the evidence round is at rest: `` turn(..) `` is
/// `None`; with `turn` reading `self.table` before it calls `news`, the first `tables.get()`
/// assertion fails.
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
    let tables = std::rc::Rc::new(std::cell::Cell::new(0));
    let looked = tables.clone();
    let mut read_table = table_at(&paths);
    let mut passes = Passes::new(
        paths,
        lab_config(&lab, 0),
        move || {
            counted.set(counted.get() + 1);
            Some(probe())
        },
        move || {
            looked.set(looked.get() + 1);
            read_table()
        },
    );
    let at = Instant::now();
    let nothing = BTreeSet::new();
    let first = passes.turn(&nothing, at, NOW).unwrap().unwrap();
    assert_eq!(first.outcome, Outcome::Deferred);
    assert!(passes.turn(&nothing, at, NOW).is_none());
    assert_eq!(tables.get(), 0, "no table drained, no read");
    assert!(passes.turn(&BTreeSet::from([[9; 32]]), at, NOW).is_none());
    assert_eq!(tables.get(), 1);
    assert_eq!(reads.get(), 1);
    let heard = passes
        .turn(&BTreeSet::from([table]), at, NOW)
        .unwrap()
        .unwrap();
    assert!(
        matches!(
            heard.outcome,
            Outcome::Adopted {
                rule: Rule::Planned,
                ..
            }
        ),
        "{:?}",
        heard.outcome
    );
    assert_eq!(reads.get(), 2);
    assert_eq!(tables.get(), 2);
    assert!(passes.turn(&BTreeSet::from([table]), at, NOW).is_none());
    assert_eq!(tables.get(), 2, "evidence heard, no read");
    let rested = at + super::run::PASS_EVERY;
    let later = passes.turn(&nothing, rested, NOW).unwrap().unwrap();
    assert_eq!(later.outcome, Outcome::Unchanged);
}

/// Migration preserves the owner's historical roster label ahead of a log display hint.
/// NEGATIVE CONTROL: preferring the log loses the historical owner's label to a display hint.
#[test]
fn migration_preserves_the_roster_label_ahead_of_a_log_hint() {
    use super::super::store::chosen_label;
    let word = |s: &str| Some(s.to_owned());
    assert_eq!(chosen_label(word("m7-3fa2"), word("m8")), word("m8"));
    assert_eq!(chosen_label(None, word("m8")), word("m8"));
    assert_eq!(chosen_label(word("not a label!"), word("m8")), word("m8"));
    assert_eq!(chosen_label(None, None), None);
}

/// A hostname chooses a free display label, while the enrollment's authenticated uuid still
/// determines the machine row and the signed facts key. No log or roster label is needed.
#[test]
fn a_hostname_supplies_a_label_without_replacing_the_enrollment_identity() {
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
    let uuid = keys::uuid32(&machine_of(&lab, 0));
    assert_eq!(
        (pass.outcome, pass.records),
        (
            Outcome::Born {
                mac: keys::machine_pk(&uuid)
            },
            1
        )
    );
    let view = registry.view().unwrap();
    assert_eq!(
        view.project(&MACHINE).rows[&keys::machine_pk(&uuid)].get("label"),
        Some(&json!("m12"))
    );
    assert_eq!(
        view.owned[MACHINE_FACTS.name].rows[&uuid].value["mac"],
        json!(keys::machine_pk(&uuid))
    );
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
                &[("label", json!("m70")), ("node", json!(NODE))],
            ),
            machine(
                "mc-1000000000000002",
                &[("label", json!("m71")), ("node", json!(NODE))],
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
    assert!(matches!(
        heard.recv_timeout(hang).unwrap(),
        Some(Outcome::Adopted {
            rule: Rule::Planned,
            ..
        })
    ));
    drop(registrar);
    assert!(
        second.view().unwrap().owned[MACHINE_FACTS.name]
            .rows
            .contains_key(&uuid)
    );
}
/// The registry's table, a read of custody and the catalog, is read for a pass only while no
/// evidence was heard and a round drained a table, and only a drained table that is the
/// registry's is news.
/// NEGATIVE CONTROL: with `news` reading `table()` before it looks at `heard`, the first row's
/// `unread` panics.
#[test]
fn a_pass_reads_the_registrys_table_only_until_its_evidence_is_heard() {
    const HELD: [u8; 32] = [1; 32];
    const OTHER: [u8; 32] = [2; 32];
    let unread =
        || -> Option<[u8; 32]> { panic!("the registry's table is read only before evidence") };
    assert!(!news(true, &BTreeSet::from([HELD]), unread), "heard before");
    assert!(!news(false, &BTreeSet::new(), unread), "nothing drained");
    assert!(news(false, &BTreeSet::from([OTHER, HELD]), || Some(HELD)));
    assert!(
        !news(false, &BTreeSet::from([OTHER]), || Some(HELD)),
        "another table"
    );
    assert!(
        !news(false, &BTreeSet::from([HELD]), || None),
        "no registry here"
    );
}

/// A scope is one of the registry's generations only where it is that generation's table under
/// its own trust: each generation's is, under any trust, and the catalog's, another
/// application's, and a registry table's id under another generation number are not.
/// NEGATIVE CONTROL: with `is_generation` comparing only `scope.table()`, the renumbered scope
/// is judged a generation and the `renumbered` assertion fails.
#[test]
fn only_a_registry_generations_table_is_one() {
    use aterm_secrets::commons::journal::Trust;
    for trust in [
        Trust {
            network: [3; 16],
            authority: [4; 32],
            genesis: [5; 32],
        },
        Trust {
            network: [6; 16],
            authority: [7; 32],
            genesis: [8; 32],
        },
    ] {
        for compiled in crate::att::REGISTRY.chain() {
            let scope = compiled.descriptor(trust).unwrap().scope;
            assert!(Registry::is_generation(scope), "{}", compiled.name());
            let renumbered =
                crate::table::Scope::new(trust, *scope.table(), scope.generation() + 1).unwrap();
            assert!(!Registry::is_generation(renumbered), "renumbered");
        }
        assert!(
            !Registry::is_generation(crate::catalog::scope(trust)),
            "the catalog"
        );
        let other = crate::catalog::Descriptor::application(trust, "aterm.lab", "tables", 1)
            .unwrap()
            .scope;
        assert!(!Registry::is_generation(other), "another application");
    }
}

/// A quiet round of the table `scope` names, from a peer that holds two of its operations, with
/// the peer's matching inventory as its witness.
fn quiet_round(scope: crate::table::Scope) -> crate::runtime::RepairRound {
    use crate::replica::{InventoryObservation, InventorySummary, MatchingInventory, Report};
    let at = Instant::now();
    let before = InventoryObservation {
        scope,
        local_actor: [5; 48],
        peer_actor: [6; 48],
        control_observation: [7; 64],
        inventory: InventorySummary {
            count: 2,
            fingerprint: [8; 32],
        },
        observed_at_ms: 200,
        sampled_at: at,
    };
    let after = InventoryObservation {
        sampled_at: at + std::time::Duration::from_secs(1),
        ..before
    };
    let mut report = Report {
        table: Some(scope.digest()),
        scope: Some(scope),
        local_known: Some(before.inventory),
        peer_known: Some(before.inventory),
        local_offered: Some(before.inventory),
        ..Report::default()
    };
    report.matching_inventory = MatchingInventory::from_observations(&report, before, after);
    let mut round = crate::runtime::RepairRound::completed_with(report);
    round.actor = Some(before.peer_actor);
    assert!(round.matching_inventory().is_some());
    assert_eq!(round.drained_table(), Some(scope.digest()));
    round
}

/// Real rounds: each round a pair of scratch Macs repairs over loopback names its table's scope,
/// and of the two tables they drain only the registry's reaches a pass. Given the peer's actor,
/// as the runtime gives it, a quiet round carries its witness, and the witness's scope is the
/// registry's generation exactly where the round's table is the registry's. A round whose report
/// names no scope is handed on as drained.
/// NEGATIVE CONTROL: with `evidence` answering `drained_table()` whatever its scope, the other
/// table's drained rounds are evidence and the `another table` assertion fails.
#[test]
fn only_a_drained_registry_table_is_handed_to_a_pass() {
    use crate::runtime::RepairRound;
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    name_macs(&lab);
    let first = Registry::at(lab.paths[0].clone());
    first.open().unwrap();
    first.seed().unwrap();
    let reports = crate::table::tests::repair_commons_reports(&lab, 12);
    let table = Registry::at(lab.paths[1].clone()).table().unwrap().unwrap();
    let (mut registry_rounds, mut other_rounds, mut witnessed) = (0, 0, 0);
    let mut unnamed = None;
    for report in reports.iter().flatten() {
        assert_eq!(
            report.scope.map(|scope| scope.digest()),
            report.table,
            "a round names its table's scope"
        );
        let mut round = RepairRound::completed_with(*report);
        if let Some(witness) = report.matching_inventory {
            round.actor = Some(witness.peer_actor());
        }
        if let Some(witness) = round.matching_inventory() {
            assert_eq!(
                Registry::is_generation(witness.scope()),
                report.table == Some(table)
            );
            witnessed += 1;
        }
        match round.drained_table() {
            Some(drained) if drained == table => {
                assert_eq!(evidence(&round), Some(table));
                registry_rounds += 1;
            }
            Some(_) => {
                assert_eq!(evidence(&round), None, "another table");
                other_rounds += 1;
                unnamed.get_or_insert(round);
            }
            None => assert_eq!(evidence(&round), None),
        }
    }
    assert!(registry_rounds > 0, "no round drained the registry");
    assert!(other_rounds > 0, "no round drained another table");
    assert!(witnessed > 0, "no real round carried a witness");
    let mut unnamed = unnamed.unwrap();
    if let Some(report) = unnamed.application.as_mut() {
        report.scope = None;
    }
    assert_eq!(
        evidence(&unnamed),
        unnamed.drained_table(),
        "no scope named"
    );
}

/// The registrar's own thread: a quiet round of another table brings its pass no read of the
/// registry's table, and the registry's next quiet round brings one, which is news.
/// NEGATIVE CONTROL: with `Registrar::round` handing on `round.drained_table()` in place of
/// `evidence(round)`, the first pass reads the registry's table and `assert_eq!(reads(), 0)`
/// fails.
#[test]
fn a_registrar_reads_the_registrys_table_only_for_a_round_that_may_be_it() {
    use aterm_secrets::commons::journal::Trust;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let lab = Lab::new(&[Role::Writer]);
    name_macs(&lab);
    let trust = Trust {
        network: [1; 16],
        authority: [2; 32],
        genesis: [3; 32],
    };
    let registry = crate::att::REGISTRY.descriptor(trust).unwrap().scope;
    let other = crate::catalog::scope(trust);
    let count = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&count);
    let reads = || count.load(Ordering::SeqCst);
    let (sent, heard) = std::sync::mpsc::channel();
    let registrar = Registrar::start_with(
        lab.paths[0].clone(),
        lab_config(&lab, 0),
        || Some(probe()),
        move || {
            counted.fetch_add(1, Ordering::SeqCst);
            Some(registry.digest())
        },
        move |pass: &Result<Registered>| {
            let _ = sent.send(pass.is_ok());
        },
    )
    .unwrap();
    let hang = std::time::Duration::from_secs(60);
    registrar.round(&quiet_round(other));
    assert!(heard.recv_timeout(hang).is_ok(), "the first round's pass");
    assert_eq!(reads(), 0);
    registrar.round(&quiet_round(registry));
    assert!(
        heard.recv_timeout(hang).is_ok(),
        "the registry's round is news"
    );
    assert_eq!(reads(), 1);
    drop(registrar);
}

/// A transient host-probe failure (`gather() == None`) does not spend `heard` or start the
/// six-hour rest window, so the next round when the Mac can state itself still registers.
/// NEGATIVE CONTROL: with `Passes::turn` setting `self.heard |= news` and `self.last = Some(now)`
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
    let read_table = table_at(&paths);
    let mut passes = Passes::new(
        paths,
        lab_config(&lab, 0),
        move || {
            if !ready {
                ready = true;
                None
            } else {
                Some(probe())
            }
        },
        read_table,
    );
    let at = Instant::now();
    let drained = BTreeSet::from([table]);
    assert!(passes.turn(&drained, at, NOW).is_none());
    let done = passes
        .turn(&drained, at, NOW)
        .expect("probe now ready")
        .unwrap();
    assert!(
        matches!(
            done.outcome,
            Outcome::Adopted {
                rule: Rule::Planned,
                ..
            }
        ),
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
/// findings; without reading `node_id`, a member's `fleet_node` is `None`.
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
    assert_eq!(
        macs[0].fleet_node.as_deref(),
        Some("fedcba9876543210fedcba9876543210")
    );

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
                    value: CellValue::owner(MACHINE.version, json!("Mac99-1")).encode(),
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

/// A duplicate its Mac's own facts row is bound to stays until that Mac moves, through the facts a
/// real registration wrote; a row the Mac is bound to is the row that stays.
/// NEGATIVE CONTROL: with `command::merge_plan` passing an empty `bound`, the Mac's row is merged
/// under it, and the `plan.merges.is_empty()` assertion prints that merge.
#[test]
fn merge_reads_the_macs_own_facts() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    name_macs(&lab);
    let registry = Registry::at(lab.paths[0].clone());
    registry.open().unwrap();
    let done = registry
        .register(&lab_config(&lab, 0), &probe(), true, NOW)
        .unwrap();
    let bound = match done.outcome {
        Outcome::Born { mac } | Outcome::Adopted { mac, .. } => mac,
        other => panic!("{other:?}"),
    };
    let view = registry.view().unwrap();
    let held_name = view.project(&MACHINE).rows[&bound]
        .get("label")
        .and_then(Value::as_str)
        .unwrap()
        .to_owned();
    let other = keys::machine_pk("m899");
    let write = |pk: &str, col: &str, elem: Option<&str>, x: Value| {
        cell::change(
            &MACHINE,
            CellKey { pk, col, elem },
            &CellValue::owner(MACHINE.version, x),
        )
        .unwrap()
    };
    registry
        .write(vec![vec![
            write(&other, "label", None, json!("m899")),
            write(&other, "aliases", Some(&held_name), json!(true)),
            write(
                &other,
                "fleet_node",
                None,
                json!("00112233445566778899aabbccddeeff"),
            ),
            write(&other, "status", None, json!("active")),
            write(&other, "row", None, json!("live")),
            write(&bound, "node", None, Value::Null),
        ]])
        .unwrap();
    let view = registry.view().unwrap();
    let plan = crate::alab::command::merge_plan(&view, &BTreeMap::new()).unwrap();
    assert!(plan.merges.is_empty(), "{:?}", plan.merges);
    assert_eq!(plan.held[0].why, "bound-by-its-mac");
}

/// A Mac whose row an old import called `active` births a row of its own under another label, and
/// once `merge` set that row back it adopts it, through `pick` itself.
/// NEGATIVE CONTROL: with `wait_for_macs` writing another word (`parked`), the Mac births again:
/// `Born { mac: "mc-1d866dd060273165" }`.
#[test]
fn a_mac_adopts_the_row_merge_set_waiting() {
    let row = keys::machine_pk("m7");
    let imported = |col: &str, x: Value| {
        cell::change(
            &MACHINE,
            CellKey {
                pk: &row,
                col,
                elem: None,
            },
            &CellValue::imported(MACHINE.version, x, "sr-0123456789abcdef"),
        )
        .unwrap()
    };
    let mut book = Book::new();
    book.write(
        2,
        &[],
        vec![
            imported("label", json!("m7")),
            imported("status", json!("active")),
            imported("row", json!("live")),
        ],
    );
    let before = run(&me(), &probe(), &[ME], true, &book.cells(), &facts(&[]));
    assert!(
        matches!(before.outcome, Outcome::Born { .. }),
        "{:?}",
        before.outcome
    );
    let cells = book.cells();
    let merged = crate::alab::merge::plan(
        &cell::project(&MACHINE, &cells),
        &cell::project(&crate::alab::registry::PREFERENCE, &cells),
        &crate::alab::merge::Facts::default(),
        &[("m7".to_owned(), Default::default())].into(),
    )
    .unwrap();
    assert_eq!(merged.waiting.len(), 1, "{merged:?}");
    for changes in &merged.rows {
        let seen = book.all();
        book.write(2, &seen, changes.clone());
    }
    let adopted = run(&me(), &probe(), &[ME], true, &book.cells(), &facts(&[]));
    match adopted.outcome {
        Outcome::Adopted { mac, .. } => assert_eq!(mac, row),
        other => panic!("{other:?}"),
    }
}

/// A Mac that could adopt a duplicate by name adopts the row that stays, through `pick` itself: a
/// merge leaves it no reason to birth a third row.
/// NEGATIVE CONTROL: with `status_moves` never moving `planned` onto a row that waits, the Mac births:
/// `Born { .. }`, which the panic prints.
#[test]
fn a_mac_adopts_the_row_a_merge_kept() {
    let keep = keys::machine_pk("m36");
    let gone = keys::machine_pk("m7");
    let imported = |pk: &str, col: &str, elem: Option<&str>, x: Value| {
        cell::change(
            &MACHINE,
            CellKey { pk, col, elem },
            &CellValue::imported(MACHINE.version, x, "sr-0123456789abcdef"),
        )
        .unwrap()
    };
    let mut book = Book::new();
    book.write(
        2,
        &[],
        vec![
            imported(&keep, "label", None, json!("m36")),
            imported(&keep, "aliases", Some("m7"), json!(true)),
            imported(
                &keep,
                "fleet_node",
                None,
                json!("00112233445566778899aabbccddeeff"),
            ),
            imported(&keep, "status", None, json!("active")),
            imported(&keep, "row", None, json!("live")),
            imported(&gone, "label", None, json!("m7")),
            imported(&gone, "status", None, json!("planned")),
            imported(&gone, "row", None, json!("live")),
        ],
    );
    let cells = book.cells();
    let merged = crate::alab::merge::plan(
        &cell::project(&MACHINE, &cells),
        &cell::project(&crate::alab::registry::PREFERENCE, &cells),
        &crate::alab::merge::Facts::default(),
        &[("m7".to_owned(), Default::default())].into(),
    )
    .unwrap();
    assert_eq!(merged.merges.len(), 1, "{:?}", merged.held);
    for changes in &merged.rows {
        let seen = book.all();
        book.write(2, &seen, changes.clone());
    }
    let adopted = run(&me(), &probe(), &[ME], true, &book.cells(), &facts(&[]));
    match adopted.outcome {
        Outcome::Adopted { mac, .. } => assert_eq!(mac, keep),
        other => panic!("{other:?}"),
    }
}
