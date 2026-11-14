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
fn two_macs_born_apart_on_one_free_number_settle_to_one_holder_on_every_replica() {
    disconnected_number_claims(false, false);
}

#[test]
fn a_generation_roll_preserves_the_settled_number_claim_after_releasing_its_history() {
    disconnected_number_claims(true, false);
}

#[test]
fn a_persons_number_choice_stays_a_clash_label_after_repair_and_roll() {
    disconnected_number_claims(true, true);
}

fn disconnected_number_claims(roll_generation: bool, person_choice: bool) {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let registries = [
        Registry::at(lab.paths[0].clone()),
        Registry::at(lab.paths[1].clone()),
    ];
    registries[0].open().unwrap();
    registries[0].seed().unwrap();
    repair_commons(&lab, 6);
    let probes = [
        Probe {
            hosts: vec!["m777777.local".into()],
            ..probe()
        },
        Probe {
            hosts: vec!["m777777.local".into()],
            node: Some(OTHER_NODE.into()),
            platform_uuid: "C3396674-FCD3-52E5-A1C1-BD5B9E8F5C44".into(),
            ..probe()
        },
    ];
    let mut births = Vec::new();
    let mut claims = Vec::new();
    for i in 0..2 {
        // There is no repair between births. Local observation times run backwards, and
        // neither writer reserves the number on its peer before committing its own row.
        let registered = registries[i]
            .register(
                &lab_config(&lab, i),
                &probes[i],
                true,
                NOW - i as u64 * 86_400_000,
            )
            .unwrap();
        let Outcome::Born { mac } = registered.outcome else {
            panic!(
                "the disconnected machine must be born: {:?}",
                registered.outcome
            );
        };
        let view = registries[i].view().unwrap();
        assert_eq!(
            view.project(&MACHINE).rows[&mac].get("label"),
            Some(&json!("m777777"))
        );
        let key = format!("{mac}.label");
        let claim = view
            .ops
            .iter()
            .find(|operation| {
                operation.changes.iter().any(|change| {
                    change.collection == MACHINE.name
                        && change.key == key
                        && change.value.as_ref().and_then(|value| value.get("x"))
                            == Some(&json!("m777777"))
                })
            })
            .expect("the local birth has a label claim")
            .id;
        births.push(mac);
        claims.push(claim);
    }
    let winner = usize::from(claims[1] < claims[0]);
    repair_commons(&lab, 6);
    let mut agreed = None;
    for registry in &registries {
        let machines = registry.view().unwrap().project(&MACHINE);
        let labels: Vec<_> = births
            .iter()
            .map(|mac| {
                machines.rows[mac]
                    .get("label")
                    .and_then(Value::as_str)
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(labels.iter().filter(|label| *label == "m777777").count(), 1);
        assert_eq!(labels[winner], "m777777");
        assert!(labels[1 - winner].starts_with("m777777-"));
        if let Some(agreed) = &agreed {
            assert_eq!(&labels, agreed);
        } else {
            agreed = Some(labels);
        }
    }
    let command = |words: &[&str]| {
        let words: Vec<_> = words.iter().map(|word| (*word).to_owned()).collect();
        match crate::att::command::run(
            &lab.paths[0],
            &words,
            &mut |_, _, _| panic!("a read opens no editor"),
            &mut |_| {},
        )
        .unwrap()
        {
            crate::att::command::Output::Json(value) => value,
            _ => panic!("a JSON read returns JSON"),
        }
    };
    let scanned = command(&[
        "scan",
        "alab/registry",
        "machine",
        "--json",
        "--where",
        "label=m777777",
    ]);
    assert_eq!(scanned["rows"].as_array().unwrap().len(), 1);
    assert_eq!(scanned["rows"][0]["pk"], json!(births[winner]));
    let shown = command(&["get", "alab/registry", "machine", &births[1 - winner]]);
    assert_eq!(
        shown["row"]["label"],
        json!(agreed.as_ref().unwrap()[1 - winner])
    );
    assert_eq!(shown["claim_candidates"]["label"], json!("m777777"));
    if roll_generation {
        registries[0].roll(true, NOW).unwrap();
        repair_commons(&lab, 6);
        registries[1].release(NOW).unwrap();
        repair_commons(&lab, 6);
        assert!(registries[0].roll(true, NOW).unwrap().sealed);
        repair_commons(&lab, 6);
        registries[1].release(NOW).unwrap();
        for registry in &registries {
            assert_eq!(registry.generation().generation, 2);
            let machines = registry.view().unwrap().project(&MACHINE);
            for (mac, label) in births.iter().zip(agreed.as_ref().unwrap()) {
                assert_eq!(
                    machines.rows[mac].get("label"),
                    Some(&json!(label)),
                    "rolling and releasing the old generation must preserve claim order"
                );
            }
        }
    }
    let loser = 1 - winner;
    if person_choice {
        registries[loser]
            .write(vec![vec![
                cell::change(
                    &MACHINE,
                    CellKey {
                        pk: &births[loser],
                        col: "label",
                        elem: None,
                    },
                    &CellValue::owner(MACHINE.version, json!("m777777")),
                )
                .unwrap(),
            ]])
            .unwrap();
        repair_commons(&lab, 6);
    }
    registries[loser]
        .register(
            &lab_config(&lab, loser),
            &probes[loser],
            true,
            // A converged collision is identity work, not a daily facts refresh.
            NOW,
        )
        .unwrap();
    repair_commons(&lab, 6);
    for registry in &registries {
        let machines = registry.view().unwrap().project(&MACHINE);
        assert_eq!(
            machines.rows[&births[winner]].get("label"),
            Some(&json!("m777777"))
        );
        let fresh = machines.rows[&births[loser]]
            .get("label")
            .and_then(Value::as_str)
            .unwrap();
        assert_ne!(fresh, "m777777");
        if person_choice {
            assert_eq!(fresh, agreed.as_ref().unwrap()[loser]);
            assert_eq!(
                machines.rows[&births[loser]].fields["label"].value(),
                Some(&json!("m777777"))
            );
        } else {
            assert!(
                !fresh.contains('-'),
                "the automatic loser chooses a fresh bare number"
            );
        }
    }
}

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
        super::plan_projected(
            &Input {
                me: &chosen,
                probe: &probe(),
                live: &live,
                heard: true,
                now_ms: NOW
            },
            &other_holds,
            &held,
            &cell::project(&MACHINE, &other_holds),
            &|_| false,
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
        number_change(
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
        former: Vec::new(),
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

/// A pass over fixture rows, which carry no history: no birth record saw anything.
fn run(
    me: &Me,
    probe: &Probe,
    live: &[&str],
    heard: bool,
    cells: &Cells,
    read: &OwnedRead,
) -> Plan {
    run_saw(me, probe, live, heard, cells, read, &|_| false)
}

/// [`run`], with `saw` answering which facts rows the record that birthed `me`'s own row saw.
fn run_saw(
    me: &Me,
    probe: &Probe,
    live: &[&str],
    heard: bool,
    cells: &Cells,
    read: &OwnedRead,
    saw: &dyn Fn(&str) -> bool,
) -> Plan {
    let live: BTreeSet<String> = live.iter().map(|uuid| (*uuid).to_owned()).collect();
    plan_projected(
        &Input {
            me,
            probe,
            live: &live,
            heard,
            now_ms: NOW,
        },
        cells,
        read,
        &cell::project(&MACHINE, cells),
        saw,
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

/// An earlier writer of a lineage, seated under `label`.
fn writer(uuid: &str, label: Option<&str>) -> Former {
    Former {
        uuid: uuid.into(),
        label: label.map(str::to_owned),
    }
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

/// A bound Mac whose machine row's label is in conflict keeps the label its facts row states,
/// whatever its seat is called: a writer successor is seated under a later label (`m381` for
/// m22) and its lineage is read only until it is bound, so its seat's label must not be written
/// over the row's, at a record of the quota each way.
/// NEGATIVE CONTROL: with the refresh falling back to `Me::label`, the outcome is
/// `Refreshed { .. }`, which the `assert_eq!` prints.
#[test]
fn a_bound_rows_label_outlasts_a_conflicted_machine_label() {
    use crate::typed::cell::{Candidate, Field};
    let born = run(&me(), &probe(), &[ME], true, &Cells::new(), &facts(&[]));
    let Outcome::Born { mac } = born.outcome.clone() else {
        panic!("{:?}", born.outcome);
    };
    let mut stored = written_row(&born).clone();
    if let Value::Object(object) = &mut stored {
        object.insert("seen".into(), json!(399));
    }
    let held = stored["label"].as_str().unwrap().to_owned();
    let cells = apply(&[born.changes]);
    let mut machines = cell::project(&MACHINE, &cells);
    machines.rows.get_mut(&mac).unwrap().fields.insert(
        "label".into(),
        Field::Conflict {
            candidates: [held.as_str(), "m381"]
                .into_iter()
                .map(|label| Candidate {
                    operation: [1; 32],
                    value: CellValue::owner(MACHINE.version, json!(label)),
                })
                .collect(),
            provisional: Some(json!(held)),
        },
    );
    let successor = Me {
        label: Some("m381".into()),
        ..me()
    };
    let live = [ME.to_owned()].into();
    let plan = plan_projected(
        &Input {
            me: &successor,
            probe: &probe(),
            live: &live,
            heard: true,
            now_ms: NOW,
        },
        &cells,
        &facts(&[(ME, stored)]),
        &machines,
        &|_| false,
    )
    .unwrap();
    assert_eq!(plan.outcome, Outcome::Unchanged);
    assert!(plan.changes.is_empty());
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
/// NEGATIVE CONTROLS: with `checks::facts` not filtering to `live`, the gone uuid's row joins and
/// the `one` assertion fails; with it counting every enrollment of a group as a root, the
/// `succeeded` assertion lists `facts-bind-one-machine` and `hw-hint-clone`; with it following
/// one `predecessor` only, the `chain` assertion lists them; with it not counting `forked`, the
/// `fork` assertion is empty; with it reading only each member's own `predecessor` for the fork
/// (a chain's later links left out), the `hidden` assertion is empty.
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
    // A writer successor names the enrollment it succeeded, which stays seated: one Mac, twice
    // enrolled, shares nothing. A third enrollment that succeeded neither is another Mac.
    let third = "33333333333333333333333333333333";
    let later = json!({"n": 1, "mac": mac, "hw_hint": HINT, "ram_gb": 128, "predecessor": OLD});
    let succeeded = facts(&[(ME, later.clone()), (OLD, row(mac))]);
    let found: Vec<&str> = checks::facts(&succeeded, &live, &machines)
        .iter()
        .map(|f| f.kind)
        .collect();
    assert_eq!(found, ["facts-differ", "facts-differ"]);
    let cloned = facts(&[(ME, later), (OLD, row(mac)), (third, row(mac))]);
    let three: BTreeSet<String> = [ME, OLD, third].into_iter().map(str::to_owned).collect();
    let shared: Vec<_> = checks::facts(&cloned, &three, &machines)
        .into_iter()
        .filter(|f| f.kind != "facts-differ")
        .map(|f| (f.kind, f.pks.len()))
        .collect();
    assert_eq!(
        shared,
        [("facts-bind-one-machine", 3), ("hw-hint-clone", 3)]
    );
    // Twice succeeded, and the vault removed the middle seat: its row still links the two ends.
    let newest = json!({"n": 1, "mac": mac, "hw_hint": HINT, "ram_gb": 128, "predecessor": third});
    let middle = json!({"n": 1, "mac": mac, "hw_hint": HINT, "ram_gb": 128, "predecessor": OLD});
    let chain = facts(&[(ME, newest), (third, middle), (OLD, row(mac))]);
    let found: Vec<&str> = checks::facts(&chain, &live, &machines)
        .iter()
        .map(|f| f.kind)
        .collect();
    assert_eq!(found, ["facts-differ", "facts-differ"]);
    // Two enrollments that both succeeded one writer: a home has one successor, so one of them
    // is a copy of it, which states the original's hint where it is a VM's clone.
    let later = json!({"n": 1, "mac": mac, "hw_hint": HINT, "ram_gb": 128, "predecessor": OLD});
    let fork = facts(&[(ME, later.clone()), (third, later), (OLD, row(mac))]);
    let shared: Vec<_> = checks::facts(&fork, &three, &machines)
        .into_iter()
        .filter(|f| f.kind != "facts-differ")
        .map(|f| (f.kind, f.pks.len()))
        .collect();
    assert_eq!(
        shared,
        [("facts-bind-one-machine", 3), ("hw-hint-clone", 3)]
    );
    // The fork shows through a removed seat too: the Mac's middle seat is gone, and a copy
    // succeeded the same writer that seat did.
    let fourth = "44444444444444444444444444444444";
    let newest = json!({"n": 1, "mac": mac, "hw_hint": HINT, "ram_gb": 128, "predecessor": third});
    let gone = json!({"n": 1, "mac": mac, "hw_hint": HINT, "ram_gb": 128, "predecessor": OLD});
    let hidden = facts(&[
        (ME, newest),
        (third, gone.clone()),
        (fourth, gone),
        (OLD, row(mac)),
    ]);
    let seated: BTreeSet<String> = [ME, OLD, fourth].into_iter().map(str::to_owned).collect();
    let shared: Vec<_> = checks::facts(&hidden, &seated, &machines)
        .into_iter()
        .filter(|f| f.kind != "facts-differ")
        .map(|f| (f.kind, f.pks.len()))
        .collect();
    assert_eq!(
        shared,
        [("facts-bind-one-machine", 3), ("hw-hint-clone", 3)]
    );
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
        SchemaState::One { version, .. } if version == MACHINE.version
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
fn check_previews_a_settled_node_claim_without_writing() {
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
        json!("adopts"),
        "{}",
        report["registration"]
    );
    assert_eq!(
        report["registration"]["machine"],
        json!("mc-1000000000000001")
    );
    let projected = registry.view().unwrap().project(&MACHINE);
    assert_eq!(
        projected
            .rows
            .values()
            .filter(|row| row.get("node").is_some())
            .count(),
        1
    );
    assert_eq!(
        projected
            .rows
            .values()
            .filter(|row| row.fields["node"].value() == Some(&json!(NODE)))
            .count(),
        2
    );
    let lab_findings = report["lab"].as_array().unwrap();
    assert!(
        !lab_findings
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

/// A writer succession gives a Mac a new Commons uuid while the one it succeeded is still seated
/// and was seen yesterday: without the lineage that earlier row is a clone's (`hw-hint-live`) and
/// its machine another Mac's (`node-bound-elsewhere`), so the Mac births a second row. Its verified
/// lineage names the earlier uuid as this Mac: it adopts that machine, links the predecessor and
/// has nothing to report. The newest earlier writer with a row decides, and a machine row the
/// owner ended holds the Mac exactly as its own row would. The lineage proves one home, not one
/// Mac: where the earlier writer's row states other hardware, the home was copied, and rule 0
/// gives the copy nothing of the original's (while the original is seated, no other rule does).
/// NEGATIVE CONTROLS: with `pick` not calling `succession`, the successor is `Born { .. }`, which
/// the first `assert_eq!` on `kept.outcome` prints; with `resolved` not comparing the hints, the
/// copied home is `Adopted { rule: Succession, .. }`, which the `matches!` on `copied.outcome`
/// prints; with it voiding the whole lineage at any other hardware, `again` is `Held`
/// (`node-ambiguous`: both rows name this node); with it dropping every writer before the
/// other-hardware one, so is `back`.
#[test]
fn a_writer_successor_keeps_the_machine_its_earlier_writer_bound() {
    let (mac, later) = ("mc-1000000000000001", "mc-1000000000000002");
    let third = "33333333333333333333333333333333";
    let row = |pk, label: &str, lifecycle: &str| {
        machine(
            pk,
            &[
                ("label", json!(label)),
                ("status", json!("active")),
                ("node", json!(NODE)),
                ("row", json!(lifecycle)),
            ],
        )
    };
    let cells = apply(&[row(mac, "m7", "live")]);
    let earlier = facts(&[(OLD, old_row(mac, 399))]);
    let unproven = run(&me(), &probe(), &[ME, OLD], true, &cells, &earlier);
    assert!(
        matches!(unproven.outcome, Outcome::Born { .. }),
        "{:?}",
        unproven.outcome
    );
    assert_eq!(kinds(&unproven), ["hw-hint-live", "node-bound-elsewhere"]);
    let successor = Me {
        former: vec![writer(OLD, None)],
        ..me()
    };
    let kept = run(&successor, &probe(), &[ME, OLD], true, &cells, &earlier);
    assert_eq!(
        kept.outcome,
        Outcome::Adopted {
            rule: Rule::Succession,
            mac: mac.into()
        }
    );
    assert_eq!(written_row(&kept)["predecessor"], json!(OLD));
    assert_eq!(written_row(&kept)["mac"], json!(mac));
    assert!(kept.findings.is_empty(), "{:?}", kept.findings);
    // Two successions: the newest earlier writer's machine, not the first one's.
    let twice = Me {
        former: vec![writer(OLD, None), writer(third, None)],
        ..me()
    };
    let both = apply(&[row(mac, "m7", "live"), row(later, "m70", "live")]);
    let moved = run(
        &twice,
        &probe(),
        &[ME, OLD],
        true,
        &both,
        &facts(&[(OLD, old_row(mac, 300)), (third, old_row(later, 399))]),
    );
    assert_eq!(
        moved.outcome,
        Outcome::Adopted {
            rule: Rule::Succession,
            mac: later.into()
        }
    );
    assert_eq!(written_row(&moved)["predecessor"], json!(third));
    // An earlier writer that never registered binds nothing: the rule goes on, here to the node.
    let unregistered = run(&successor, &probe(), &[ME, OLD], true, &cells, &facts(&[]));
    assert_eq!(
        unregistered.outcome,
        Outcome::Adopted {
            rule: Rule::Node,
            mac: mac.into()
        }
    );
    // The owner ended the machine: the binding is for good, so the successor waits.
    let ended = run(
        &successor,
        &probe(),
        &[ME, OLD],
        true,
        &apply(&[row(mac, "m7", "retired")]),
        &earlier,
    );
    assert_eq!(ended.outcome, Outcome::Held);
    assert_eq!(kinds(&ended), ["bound-machine-retired"]);
    assert!(ended.changes.is_empty());
    // A home copied to another Mac: its writer succeeds the original's by itself, and the
    // original's row states the original's hardware.
    let original = json!({"r": 1, "n": 3, "mac": mac, "label": "m7", "hw_hint": "bbbbbbbbbbbbbbbb", "seen": 399, "hosts": []});
    let copied = run(
        &Me {
            label: Some("m8".into()),
            former: vec![writer(OLD, Some("m7"))],
            ..me()
        },
        &probe(),
        &[ME, OLD],
        true,
        &cells,
        &facts(&[(OLD, original)]),
    );
    assert!(
        matches!(copied.outcome, Outcome::Born { .. }),
        "{:?}",
        copied.outcome
    );
    assert_eq!(
        kinds(&copied),
        ["succession-other-hardware", "node-bound-elsewhere"]
    );
    assert_eq!(written_row(&copied).get("predecessor"), None);
    // The copy's own later succession: the original's writer is still first in its lineage, and
    // the copy's earlier writer, on this hardware, is this Mac: only the original's is dropped.
    let original = json!({"r": 1, "n": 3, "mac": mac, "label": "m7", "hw_hint": "bbbbbbbbbbbbbbbb", "seen": 399, "hosts": []});
    let again = run(
        &Me {
            former: vec![writer(OLD, Some("m7")), writer(third, Some("m70"))],
            ..me()
        },
        &probe(),
        &[ME, OLD, third],
        true,
        &both,
        &facts(&[(OLD, original), (third, old_row(later, 399))]),
    );
    assert_eq!(
        again.outcome,
        Outcome::Adopted {
            rule: Rule::Succession,
            mac: later.into()
        }
    );
    assert_eq!(written_row(&again)["predecessor"], json!(third));
    assert_eq!(kinds(&again), ["succession-other-hardware"]);
    // The home went to another Mac and came back: the first writer's row states this hardware,
    // so its machine is this Mac's, whatever the writer between them registered elsewhere.
    let away = json!({"r": 1, "n": 3, "mac": later, "label": "m70", "hw_hint": "bbbbbbbbbbbbbbbb", "seen": 399, "hosts": []});
    let back = run(
        &Me {
            former: vec![writer(OLD, Some("m7")), writer(third, Some("m70"))],
            ..me()
        },
        &probe(),
        &[ME, OLD, third],
        true,
        &both,
        &facts(&[(OLD, old_row(mac, 399)), (third, away)]),
    );
    assert_eq!(
        back.outcome,
        Outcome::Adopted {
            rule: Rule::Succession,
            mac: mac.into()
        }
    );
    assert_eq!(written_row(&back)["predecessor"], json!(OLD));
    assert_eq!(kinds(&back), ["succession-other-hardware"]);
}

/// A writer successor is seated under a new label (m22's sat in the roster as m381 beside m22)
/// while the machine row an import planned for the Mac carries the first seat's. The Mac goes by
/// the first label along its lineage, so it adopts that row; read from its own seat alone it
/// finds no row and births one. A seat the vault removed states no label, and a number the Mac
/// signed stands over every seat's.
/// NEGATIVE CONTROLS: with `resolved` taking the newest earlier seat's label (`.rev()`), the
/// `twice` assertion reads `Born { .. }`; with it keeping the Mac's own seat's, so does `first`;
/// with it not comparing the hints, `copied` is `Adopted { rule: Planned, .. }`; with it keeping
/// an unregistered writer beside an other-hardware one (`true` for the `None` arm of
/// `resolved`'s filter), `before` and `after` are `Adopted { .. }`.
#[test]
fn a_writer_successor_goes_by_its_first_seats_label() {
    let third = "33333333333333333333333333333333";
    let planned = keys::machine_pk("m22");
    let cells = apply(&[machine(
        &planned,
        &[("label", json!("m22")), ("status", json!("planned"))],
    )]);
    let none = facts(&[]);
    let seated = |former: Vec<Former>| Me {
        label: Some("m381".into()),
        former,
        ..me()
    };
    let outcome = |me: &Me| run(me, &probe(), &[ME, OLD], true, &cells, &none).outcome;
    let adopted = Outcome::Adopted {
        rule: Rule::Planned,
        mac: planned.clone(),
    };
    let own_seat = outcome(&seated(Vec::new()));
    assert!(matches!(own_seat, Outcome::Born { .. }), "{own_seat:?}");
    let first = outcome(&seated(vec![writer(OLD, Some("m22"))]));
    assert_eq!(first, adopted);
    // Two successions: the first seat's label, not the newest earlier one's.
    let twice = outcome(&seated(vec![
        writer(OLD, Some("m22")),
        writer(third, Some("m99")),
    ]));
    assert_eq!(twice, adopted);
    // The first seat is gone from the vault: the next writer's stands.
    let removed = outcome(&seated(vec![writer(OLD, None), writer(third, Some("m22"))]));
    assert_eq!(removed, adopted);
    let unseated = outcome(&seated(vec![writer(OLD, None)]));
    assert!(matches!(unseated, Outcome::Born { .. }), "{unseated:?}");
    // A home copied to other hardware takes the original's label no more than its machine.
    let original = json!({"r": 1, "n": 3, "mac": "mc-1000000000000001", "label": "m22", "hw_hint": "bbbbbbbbbbbbbbbb", "seen": 399, "hosts": []});
    let copied = run(
        &seated(vec![writer(OLD, Some("m22"))]),
        &probe(),
        &[ME, OLD],
        true,
        &cells,
        &facts(&[(OLD, original)]),
    );
    assert!(
        matches!(copied.outcome, Outcome::Born { .. }),
        "{:?}",
        copied.outcome
    );
    assert_eq!(kinds(&copied), ["succession-other-hardware"]);
    // A writer that never registered lends no label once any writer states other hardware,
    // whichever side of it the writer sits: nothing says which Mac the home was on when it ran
    // (the original's second writer and the copy's first look the same).
    let original = json!({"r": 1, "n": 3, "mac": "mc-1000000000000001", "label": "m9", "hw_hint": "bbbbbbbbbbbbbbbb", "seen": 399, "hosts": []});
    let after = run(
        &seated(vec![writer(OLD, Some("m9")), writer(third, Some("m22"))]),
        &probe(),
        &[ME, OLD, third],
        true,
        &cells,
        &facts(&[(OLD, original)]),
    );
    assert!(
        matches!(after.outcome, Outcome::Born { .. }),
        "{:?}",
        after.outcome
    );
    let fourth = "44444444444444444444444444444444";
    let elsewhere = json!({"r": 1, "n": 3, "mac": "mc-1000000000000001", "label": "m9", "hw_hint": "bbbbbbbbbbbbbbbb", "seen": 399, "hosts": []});
    let before = run(
        &seated(vec![writer(fourth, Some("m22")), writer(OLD, Some("m9"))]),
        &probe(),
        &[ME, OLD, fourth],
        true,
        &cells,
        &facts(&[(OLD, elsewhere)]),
    );
    assert!(
        matches!(before.outcome, Outcome::Born { .. }),
        "{:?}",
        before.outcome
    );
    // A number the Mac signed is its label whatever its seats say.
    let signed = run(
        &Me {
            chosen: Some("m381".into()),
            ..seated(vec![writer(OLD, Some("m22"))])
        },
        &probe(),
        &[ME, OLD],
        true,
        &cells,
        &none,
    );
    assert!(
        matches!(signed.outcome, Outcome::Born { .. }),
        "{:?}",
        signed.outcome
    );
    assert_eq!(written_row(&signed)["label"], json!("m381"));
}

/// A pass that failed is tried again after `RETRY_EVERY`, not after the six hours a pass that
/// decided something rests: m22's pass of 2026-11-12 failed on a table that moved under its read
/// and the Mac stayed out of the registry until the next rest ended. Each failure after another
/// doubles the rest, up to the long one, so a standing error costs no more host reads than it
/// did. Within a rest no round wakes a pass, and once one decides the long rest is back.
/// NEGATIVE CONTROLS: with `turn` never counting a failed pass, the first retry is at rest: the
/// `expect` on `again` panics; with `rest_after` not doubling, the turn at `twice + RETRY_EVERY`
/// runs a pass and its `is_none` assertion fails.
#[test]
fn a_failed_pass_is_tried_again_soon_and_a_pass_that_decided_rests() {
    use super::run::{PASS_EVERY, RETRY_EVERY, rest_after};
    assert_eq!(rest_after(0), PASS_EVERY);
    assert_eq!(rest_after(1), RETRY_EVERY);
    assert_eq!(rest_after(2), RETRY_EVERY * 2);
    assert_eq!(rest_after(7), RETRY_EVERY * 64);
    assert_eq!(rest_after(8), PASS_EVERY);
    assert_eq!(rest_after(u32::MAX), PASS_EVERY);
    let lab = Lab::new(&[Role::Writer]);
    name_macs(&lab);
    let paths = lab.paths[0].clone();
    let registry = Registry::at(paths.clone());
    registry.open().unwrap();
    registry.seed().unwrap();
    let table = registry.table().unwrap().unwrap();
    let config = lab_config(&lab, 0);
    let held = std::fs::read(&config).ok();
    std::fs::write(&config, "[commons\n").unwrap();
    let mut passes = Passes::new(
        paths.clone(),
        config.clone(),
        || Some(probe()),
        table_at(&paths),
    );
    let at = Instant::now();
    let drained = BTreeSet::from([table]);
    let failed = passes.turn(&drained, at, NOW).unwrap();
    assert!(failed.is_err());
    let soon = at + RETRY_EVERY - std::time::Duration::from_secs(1);
    assert!(passes.turn(&drained, soon, NOW).is_none());
    let twice = at + RETRY_EVERY;
    let again = passes
        .turn(&drained, twice, NOW)
        .expect("a failed pass is due again after the short rest");
    assert!(again.is_err());
    match held {
        Some(bytes) => std::fs::write(&config, bytes).unwrap(),
        None => std::fs::remove_file(&config).unwrap(),
    }
    // Two failures in a row: twice the short rest.
    assert!(passes.turn(&drained, twice + RETRY_EVERY, NOW).is_none());
    let decided = twice + RETRY_EVERY * 2;
    let retried = passes
        .turn(&drained, decided, NOW)
        .expect("a pass that failed twice is due after twice the short rest")
        .unwrap();
    assert!(
        matches!(retried.outcome, Outcome::Adopted { .. }),
        "{:?}",
        retried.outcome
    );
    // That pass decided: the long rest again, counted from it.
    assert!(
        passes
            .turn(&drained, decided + RETRY_EVERY * 4, NOW)
            .is_none()
    );
    assert!(passes.turn(&drained, decided + PASS_EVERY, NOW).is_some());
}

/// End to end on a real lab: a Mac registers, its writer is succeeded, and the successor's pass
/// adopts the machine the earlier writer bound and links it as predecessor, whether the pass is
/// given the successor's own home or the original one. The successor holds a seat of its own
/// (`m2` here), and the earlier writer's uuid is still seated with the same hardware hint, so
/// without the lineage the pass reads a clone and adopts the row planned for `m2`.
/// NEGATIVE CONTROLS: with `Registry::register`'s `lineage` closure answering `Ok(Vec::new())`,
/// or reading the lineage from `self.paths` in place of `origin`, the first `assert_eq!` reads
/// `` left: Adopted { rule: Planned, mac: "mc-…" } ``, another row.
#[test]
fn a_writer_successor_registers_onto_the_machine_its_earlier_writer_bound() {
    let mut lab = Lab::new(&[Role::Writer, Role::Writer]);
    name_macs(&lab);
    let original = lab.paths[0].clone();
    let config = lab_config(&lab, 0);
    if !config.exists() {
        aterm_uds::atomic_file::write(&config, b"", 0o600).unwrap();
    }
    let registry = Registry::at(original.clone());
    registry.open().unwrap();
    registry.seed().unwrap();
    let first = registry.register(&config, &probe(), true, NOW).unwrap();
    let Outcome::Adopted {
        rule: Rule::Planned,
        mac,
    } = first.outcome
    else {
        panic!("{:?}", first.outcome);
    };
    let earlier = keys::uuid32(&machine_of(&lab, 0));
    let successor = keys::uuid32(&machine_of(&lab, 1));
    let selected = crate::generation::test_complete_successor(
        lab._home.path(),
        &original,
        &lab.paths[1],
        &config,
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap();
    lab.paths[1] = selected.clone();
    let later = NOW + 86_400_000;
    let second = Registry::at(selected)
        .register(&config, &probe(), true, later)
        .unwrap();
    assert_eq!(
        second.outcome,
        Outcome::Adopted {
            rule: Rule::Succession,
            mac: mac.clone()
        }
    );
    assert!(second.findings.is_empty(), "{:?}", second.findings);
    let view = registry.view().unwrap();
    let row = &view.owned[MACHINE_FACTS.name].rows[&successor].value;
    assert_eq!(row["mac"], json!(mac));
    assert_eq!(row["predecessor"], json!(earlier));
    // The original home selects the same writer: nothing more to decide or to write.
    let again = registry.register(&config, &probe(), true, later).unwrap();
    assert_eq!(again.outcome, Outcome::Unchanged);
    assert_eq!(again.records, 0);
}

/// End to end where the earlier writer never registered (m22's had been fenced since before the
/// registry held its row): the successor is seated as `m2`, and its lineage's first seat is `m1`,
/// so its pass adopts the row planned for `m1`, the one the earlier writer's own pass would have.
/// NEGATIVE CONTROL: with `store::me` giving each earlier writer no label, the successor adopts
/// the row planned for `m2`: the `assert_eq!` on the adopted row's label reads `Some("m2")`.
#[test]
fn a_writer_successor_adopts_the_row_planned_for_its_first_seat() {
    let mut lab = Lab::new(&[Role::Writer, Role::Writer]);
    name_macs(&lab);
    let original = lab.paths[0].clone();
    let config = lab_config(&lab, 0);
    if !config.exists() {
        aterm_uds::atomic_file::write(&config, b"", 0o600).unwrap();
    }
    let registry = Registry::at(original.clone());
    registry.open().unwrap();
    registry.seed().unwrap();
    let successor = keys::uuid32(&machine_of(&lab, 1));
    let selected = crate::generation::test_complete_successor(
        lab._home.path(),
        &original,
        &lab.paths[1],
        &config,
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap();
    lab.paths[1] = selected.clone();
    let registered = Registry::at(selected)
        .register(&config, &probe(), true, NOW)
        .unwrap();
    let Outcome::Adopted {
        rule: Rule::Planned,
        mac,
    } = registered.outcome
    else {
        panic!("{:?}", registered.outcome);
    };
    assert!(registered.findings.is_empty(), "{:?}", registered.findings);
    let view = registry.view().unwrap();
    let machines = view.project(&MACHINE);
    assert_eq!(
        machines.rows[&mac].get("label").and_then(Value::as_str),
        Some("m1")
    );
    let row = &view.owned[MACHINE_FACTS.name].rows[&successor].value;
    assert_eq!(row["mac"], json!(mac));
    assert_eq!(row.get("predecessor"), None);
}

/// m2 on 2026-11-13 (0.199.0, a build before rule 0), as fixtures: its writer successor `ME`
/// birthed a machine row under `m34`, a label the row of another Mac (`m19`'s, at `ELSEWHERE`)
/// also holds, beside the machine `PRED` its earlier writer `OLD`, still seated, is bound to and
/// which names this Mac's label `m2` and node.
struct M2 {
    probe: Probe,
    born: String,
    /// What the unfixed birth wrote besides the facts row, its `m34` label included.
    birth: Vec<Vec<Change>>,
    old: Value,
    theirs: Value,
    /// `ME`'s facts row as the unfixed birth wrote it.
    births: Value,
    /// `ME` as the unfixed build read it, with no lineage.
    seat: Me,
    /// `ME` as this build reads it, its lineage naming `OLD`.
    successor: Me,
}

const PRED: &str = "mc-1000000000000001";
const OTHER: &str = "mc-1000000000000002";
const ELSEWHERE: &str = "33333333333333333333333333333333";
const THIRD: &str = "44444444444444444444444444444444";
const STRANGER: &str = "55555555555555555555555555555555";

impl M2 {
    /// The rows before the unfixed birth, `PRED` in `pred_lifecycle`.
    fn rows(pred_lifecycle: &str) -> Vec<Vec<Change>> {
        vec![
            machine(
                PRED,
                &[
                    ("label", json!("m2")),
                    ("status", json!("active")),
                    ("node", json!(NODE)),
                    ("fleet_node", json!("00112233445566778899aabbccddeeff")),
                    ("row", json!(pred_lifecycle)),
                ],
            ),
            machine(
                OTHER,
                &[
                    ("label", json!("m34")),
                    ("status", json!("active")),
                    ("node", json!(OTHER_NODE)),
                    ("hosts", json!(["m19"])),
                    ("row", json!("live")),
                ],
            ),
        ]
    }

    /// The unfixed build's pass, checked, and what it left.
    fn new() -> Self {
        let born = keys::machine_pk(ME);
        let probe = Probe {
            hosts: vec!["m2".into()],
            ..probe()
        };
        let old = json!({"r": 1, "n": 3, "mac": PRED, "label": "m2", "hw_hint": HINT,
            "node": NODE, "seen": 399, "hosts": ["m2"]});
        let theirs = json!({"r": 1, "n": 2, "mac": OTHER, "label": "m34",
            "hw_hint": "bbbbbbbbbbbbbbbb", "node": OTHER_NODE, "seen": 399, "hosts": ["m19"]});
        // The unfixed build's pass: no lineage, so the earlier writer reads as a clone.
        let seat = Me {
            label: Some("m34".into()),
            ..me()
        };
        let before = facts(&[(OLD, old.clone()), (ELSEWHERE, theirs.clone())]);
        let unfixed = run(
            &seat,
            &probe,
            &[ME, OLD, ELSEWHERE],
            true,
            &apply(&Self::rows("live")),
            &before,
        );
        assert_eq!(unfixed.outcome, Outcome::Born { mac: born.clone() });
        assert_eq!(kinds(&unfixed), ["hw-hint-live", "node-bound-elsewhere"]);
        // Its birth landed under `m34`, as m19's row had: the number both took on their own.
        let m34 = cell::change(
            &MACHINE,
            CellKey {
                pk: &born,
                col: "label",
                elem: None,
            },
            &CellValue::automatic(MACHINE.version, json!("m34")),
        )
        .unwrap();
        let birth = vec![
            unfixed
                .changes
                .iter()
                .filter(|c| c.collection != MACHINE_FACTS.name)
                .cloned()
                .collect(),
            vec![m34],
        ];
        let births = with(written_row(&unfixed).clone(), "label", json!("m34"));
        let successor = Me {
            former: vec![writer(OLD, Some("m2"))],
            ..seat.clone()
        };
        Self {
            probe,
            born,
            birth,
            old,
            theirs,
            births,
            seat,
            successor,
        }
    }

    /// The registry after the unfixed birth, `PRED` in `pred_lifecycle` and the birth's row in
    /// `born_lifecycle`.
    fn cells(&self, pred_lifecycle: &str, born_lifecycle: &str) -> Cells {
        let mut records = Self::rows(pred_lifecycle);
        records.extend(self.birth.iter().cloned());
        records.push(vec![
            cell::change(
                &MACHINE,
                CellKey {
                    pk: &self.born,
                    col: "row",
                    elem: None,
                },
                &CellValue::owner(MACHINE.version, json!(born_lifecycle)),
            )
            .unwrap(),
        ]);
        apply(&records)
    }

    /// The facts rows: `OLD`'s and m19's, and `ME`'s as `own` states it.
    fn read(&self, own: Value) -> OwnedRead {
        facts(&[
            (OLD, self.old.clone()),
            (ELSEWHERE, self.theirs.clone()),
            (ME, own),
        ])
    }

    /// `ME`'s facts row once this build's heard pass folded it.
    fn folded(&self) -> Value {
        let folded = run_saw(
            &self.successor,
            &self.probe,
            &[ME, OLD, ELSEWHERE],
            true,
            &self.cells("live", "live"),
            &self.read(self.births.clone()),
            &|writer| writer == OLD,
        );
        assert_eq!(folded.outcome, Outcome::Refreshed { mac: PRED.into() });
        written_row(&folded).clone()
    }
}

/// The next heard pass of m2's successor, its lineage naming `OLD` and its birth record having
/// seen `OLD`'s facts row, folds the birth: this Mac's own facts row keeps its binding and states
/// `merged_into` `PRED` and `predecessor` `OLD`, and no machine row is written. `LabView` then
/// answers `PRED` and `m2` for this Mac, no facts row answers the born row, `check` raises
/// neither shared finding, the next pass writes nothing, and a refresh keeps the fold.
///
/// Nothing folds on an unheard pass, since a partial history can show a machine the owner
/// ended as live; nor where the birth record had not seen `OLD`'s row, a birth rule 0 would have
/// made too (the clone finding stays for the owner); nor for a home whose lineage names no
/// writer bound elsewhere. A merge the Mac already acknowledged stays its word, a row it adopted
/// is never moved, and a fold onto a row the owner ended does not happen.
/// NEGATIVE CONTROL: with `fold` always `None`, the folding pass keeps the born binding:
/// `` left: Unchanged right: Refreshed { mac: "mc-1000000000000001" } ``. With `fold` not
/// asking `input.heard`, the unheard pass folds:
/// `` left: (Refreshed { mac: "mc-1000000000000001" }, 1) right: (Unchanged, 0) ``; with `fold`
/// not asking `saw`, the unseen pass folds the same way.
#[test]
fn a_writer_successor_whose_row_was_born_before_the_fix_folds_it_into_its_predecessors_machine() {
    let m2 = M2::new();
    let born = m2.born.clone();
    let live = [ME, OLD, ELSEWHERE];
    let cells = m2.cells("live", "live");
    let after = m2.read(m2.births.clone());
    let labview = |read: &OwnedRead| {
        crate::alab::view::LabView::read(&cells, read, &OwnedRead::default(), Some(ME))
    };
    let shown = labview(&after);
    assert_eq!(
        (shown.me.as_deref(), shown.my_label()),
        (Some(born.as_str()), Some("m34")),
        "PRECONDITION: the unfixed birth answers this Mac"
    );
    let machines = cell::project(&MACHINE, &cells);
    let live_set: BTreeSet<String> = live.iter().map(|uuid| (*uuid).to_owned()).collect();
    let check = |read: &OwnedRead| -> Vec<&'static str> {
        checks::facts(read, &live_set, &machines)
            .into_iter()
            .map(|f| f.kind)
            .collect()
    };
    assert_eq!(check(&after), ["hw-hint-clone"], "PRECONDITION");
    let saw_old = |writer: &str| writer == OLD;
    // An unheard pass, or a birth record that had not seen the earlier writer's row, refreshes
    // the row as it stands.
    for (heard, saw) in [
        (false, &saw_old as &dyn Fn(&str) -> bool),
        (true, &|_: &str| false),
    ] {
        let kept = run_saw(&m2.successor, &m2.probe, &live, heard, &cells, &after, saw);
        assert_eq!(
            (kept.outcome, kept.changes.len()),
            (Outcome::Unchanged, 0),
            "heard {heard}"
        );
    }
    let folded = run_saw(
        &m2.successor,
        &m2.probe,
        &live,
        true,
        &cells,
        &after,
        &saw_old,
    );
    assert_eq!(folded.outcome, Outcome::Refreshed { mac: PRED.into() });
    assert!(folded.findings.is_empty(), "{:?}", folded.findings);
    assert!(folded.changes.iter().all(|c| c.collection != MACHINE.name));
    let row = written_row(&folded).clone();
    assert_eq!(row, m2.folded());
    assert_eq!(row["mac"], json!(born));
    assert_eq!(row["merged_into"], json!(PRED));
    assert_eq!(row["predecessor"], json!(OLD));
    assert_eq!(row["label"], json!("m2"));
    assert_eq!(row["n"], json!(2));
    let rebound = m2.read(row.clone());
    let shown = labview(&rebound);
    assert_eq!(
        (shown.me.as_deref(), shown.my_label()),
        (Some(PRED), Some("m2"))
    );
    assert!(
        shown.bound.values().all(|mac| *mac != born),
        "{:?}",
        shown.bound
    );
    assert!(check(&rebound).is_empty(), "{:?}", check(&rebound));
    // Folded, the Mac is quiet; a refresh carries the fold.
    let again = run_saw(
        &m2.successor,
        &m2.probe,
        &live,
        true,
        &cells,
        &rebound,
        &saw_old,
    );
    assert_eq!(
        (again.outcome, again.changes.len()),
        (Outcome::Unchanged, 0)
    );
    let aged = with(row.clone(), "seen", json!(300));
    let refreshed = run(
        &m2.successor,
        &m2.probe,
        &live,
        true,
        &cells,
        &m2.read(aged),
    );
    assert_eq!(refreshed.outcome, Outcome::Refreshed { mac: PRED.into() });
    assert_eq!(written_row(&refreshed)["merged_into"], json!(PRED));
    assert_eq!(written_row(&refreshed)["label"], json!("m2"));
    // No earlier writer in the lineage: the birth stands.
    let alone = run_saw(&m2.seat, &m2.probe, &live, true, &cells, &after, &saw_old);
    assert_eq!(
        (alone.outcome, alone.changes.len()),
        (Outcome::Unchanged, 0)
    );
    // A merge the Mac acknowledged is its word, and a row it adopted is its binding.
    let acknowledged = with(m2.births.clone(), "merged_into", json!(OTHER));
    let kept = run_saw(
        &m2.successor,
        &m2.probe,
        &live,
        true,
        &cells,
        &m2.read(acknowledged),
        &saw_old,
    );
    assert!(
        matches!(&kept.outcome, Outcome::Unchanged)
            || matches!(&kept.outcome, Outcome::Refreshed { mac } if mac == OTHER),
        "{:?}",
        kept.outcome
    );
    let adopted = with(m2.births.clone(), "mac", json!(OTHER));
    let stays = run_saw(
        &m2.successor,
        &m2.probe,
        &live,
        true,
        &cells,
        &m2.read(adopted),
        &saw_old,
    );
    assert!(
        matches!(&stays.outcome, Outcome::Unchanged)
            || matches!(&stays.outcome, Outcome::Refreshed { mac } if mac == OTHER),
        "{:?}",
        stays.outcome
    );
    // The owner ended the earlier writer's machine: nothing to fold into, and the birth stands
    // (it publishes this Mac's node, which no live row names any more).
    let ended = run_saw(
        &m2.successor,
        &m2.probe,
        &live,
        true,
        &m2.cells("retired", "live"),
        &after,
        &saw_old,
    );
    assert_eq!(ended.outcome, Outcome::Refreshed { mac: born.clone() });
    assert!(
        ended
            .changes
            .iter()
            .all(|c| c.collection != MACHINE_FACTS.name),
        "{:?}",
        ended.changes
    );
}

/// After m2's fold, every reader of a facts row takes the folded row for the machine it was
/// folded into (`registry::bound_machine`), so the fold holds past the Mac's own row:
///
/// - a later writer succession (`THIRD`, lineage `OLD`, `ME`) adopts `PRED` by rule 0, not the
///   born row, and still does once the owner retired the born row, where rule 0 over `mac` held
///   it at `bound-machine-retired`; a later pre-fix birth of that writer folds into `PRED` too;
/// - with `OLD`'s seat gone, a stranger carrying this Mac's fleet node (a disk clone) is not
///   adopted onto `PRED` by its node: `PRED` is bound elsewhere, so it births, and `check` names a
///   clone that did bind `PRED`;
/// - a reinstall long after both seats are gone finds one machine, `PRED`, by its hint.
///
/// NEGATIVE CONTROL: with `succession` reading `mac` alone, `THIRD` adopts the born row:
/// `` left: Adopted { rule: Succession, mac: "mc-1d866dd060273165" } ``; with `bound_elsewhere`
/// reading `mac` alone, the stranger is `` left: Adopted { rule: Node, mac: "mc-1000000000000001" }
/// ``; with `checks::facts` grouping by `mac` alone, the clone that bound `PRED` raises `[]`;
/// with `reinstall` grouping by `mac`, the reinstall is `` left: Held `` at `hw-hint-ambiguous`.
#[test]
fn a_folded_successor_is_its_predecessors_machine_to_every_rule_after_it() {
    let m2 = M2::new();
    let folded = m2.folded();
    let rebound = m2.read(folded.clone());
    let live = [ME, OLD, ELSEWHERE, THIRD];
    let third = Me {
        uuid: THIRD.into(),
        former: vec![writer(OLD, Some("m2")), writer(ME, Some("m34"))],
        ..m2.seat.clone()
    };
    for born_lifecycle in ["live", "retired"] {
        let cells = m2.cells("live", born_lifecycle);
        let next = run(&third, &m2.probe, &live, true, &cells, &rebound);
        assert_eq!(
            next.outcome,
            Outcome::Adopted {
                rule: Rule::Succession,
                mac: PRED.into()
            },
            "born row {born_lifecycle}"
        );
        assert_eq!(written_row(&next)["predecessor"], json!(ME));
        assert_eq!(written_row(&next)["label"], json!("m2"));
    }
    // A later writer's own pre-fix birth folds into the machine its earlier writer was folded
    // into, never the earlier writer's birth.
    let cells = m2.cells("live", "live");
    let its_birth = json!({"r": 1, "n": 1, "mac": keys::machine_pk(THIRD), "label": "m35",
        "hw_hint": HINT, "node": NODE, "seen": 399, "hosts": ["m2"]});
    let mut both = rebound.clone();
    both.rows.insert(
        THIRD.into(),
        facts(&[(THIRD, its_birth)]).rows[THIRD].clone(),
    );
    let mut third_birth = m2.birth.clone();
    third_birth.push(machine(
        &keys::machine_pk(THIRD),
        &[("label", json!("m35")), ("row", json!("live"))],
    ));
    let mut records = M2::rows("live");
    records.extend(third_birth);
    let refold = run_saw(
        &third,
        &m2.probe,
        &live,
        true,
        &apply(&records),
        &both,
        &|writer| writer == ME,
    );
    assert_eq!(refold.outcome, Outcome::Refreshed { mac: PRED.into() });
    assert_eq!(written_row(&refold)["merged_into"], json!(PRED));
    // `OLD`'s seat is gone; a disk clone of this Mac on other hardware carries its fleet node.
    let stranger = Me {
        uuid: STRANGER.into(),
        label: Some("m40".into()),
        hw_hint: "cccccccccccccccc".into(),
        ..me()
    };
    let gone = [ME, ELSEWHERE, STRANGER];
    let clone = run(&stranger, &m2.probe, &gone, true, &cells, &rebound);
    assert_eq!(
        clone.outcome,
        Outcome::Born {
            mac: keys::machine_pk(STRANGER)
        }
    );
    assert!(
        kinds(&clone).contains(&"node-bound-elsewhere"),
        "{:?}",
        clone.findings
    );
    let machines = cell::project(&MACHINE, &cells);
    let gone_set: BTreeSet<String> = gone.iter().map(|uuid| (*uuid).to_owned()).collect();
    let mut adopted = rebound.clone();
    adopted.rows.insert(
        STRANGER.into(),
        facts(&[(
            STRANGER,
            json!({"r": 1, "n": 1, "mac": PRED, "label": "m2", "hw_hint": "cccccccccccccccc",
                "node": NODE, "seen": 400, "hosts": ["m2"]}),
        )])
        .rows[STRANGER]
            .clone(),
    );
    let found: Vec<&str> = checks::facts(&adopted, &gone_set, &machines)
        .into_iter()
        .map(|f| f.kind)
        .collect();
    assert!(found.contains(&"facts-bind-one-machine"), "{found:?}");
    // Both seats gone, the Mac wiped and enrolled again long after: one machine by its hint.
    let reinstalled = Me {
        uuid: STRANGER.into(),
        label: Some("m41".into()),
        enrolled_ms: Some(NOW),
        ..me()
    };
    let wiped = Probe {
        node: None,
        hosts: Vec::new(),
        ..m2.probe.clone()
    };
    let long_gone = facts(&[
        (OLD, with(m2.old.clone(), "seen", json!(250))),
        (ELSEWHERE, m2.theirs.clone()),
        (ME, with(folded, "seen", json!(300))),
    ]);
    let back = run(
        &reinstalled,
        &wiped,
        &[ELSEWHERE, STRANGER],
        true,
        &cells,
        &long_gone,
    );
    assert_eq!(
        back.outcome,
        Outcome::Adopted {
            rule: Rule::Reinstall,
            mac: PRED.into()
        },
        "{:?}",
        back.findings
    );
    assert_eq!(written_row(&back)["predecessor"], json!(ME));
}

/// `row` with `column` set to `x`.
fn with(mut row: Value, column: &str, x: Value) -> Value {
    if let Some(object) = row.as_object_mut() {
        object.insert(column.into(), x);
    }
    row
}

/// End to end on a real lab, the same shape: a Mac registers under its first writer, its writer
/// is succeeded, and a build before rule 0 registers the successor's own seat (`m34` here, which
/// no row is planned under) with no lineage, so it births a row beside the earlier writer's, in a
/// record whose causal past holds the earlier writer's facts row. This build's unheard pass
/// writes nothing and reads no lineage; its next heard pass reads the lineage for that unfolded
/// birth (`store::planned`, gated by `register::may_fold`), folds it in one record, and the lab's
/// view names the earlier writer's machine and label for this Mac; the pass after writes nothing
/// and reads no lineage. A row bound to a machine the Mac adopted is never worth the read.
/// NEGATIVE CONTROL: with `store::planned` reading no lineage for a row the Mac already has (its
/// `may_fold` arm answering `Ok(Vec::new())`, as before the fold), the folding pass reads
/// `` left: (Unchanged, 0) ``; with `birth_saw` answering false, `may_fold` refuses the birth
/// before any pass runs.
#[test]
fn a_writer_successors_birth_from_before_the_fix_is_folded_by_its_next_pass() {
    let mut lab = Lab::new(&[Role::Writer, Role::Writer]);
    name_macs(&lab);
    let original = lab.paths[0].clone();
    let config = lab_config(&lab, 0);
    if !config.exists() {
        aterm_uds::atomic_file::write(&config, b"", 0o600).unwrap();
    }
    let registry = Registry::at(original.clone());
    registry.open().unwrap();
    registry.seed().unwrap();
    let first = registry.register(&config, &probe(), true, NOW).unwrap();
    let Outcome::Adopted {
        rule: Rule::Planned,
        mac,
    } = first.outcome
    else {
        panic!("{:?}", first.outcome);
    };
    let earlier = keys::uuid32(&machine_of(&lab, 0));
    let successor = keys::uuid32(&machine_of(&lab, 1));
    let selected = crate::generation::test_complete_successor(
        lab._home.path(),
        &original,
        &lab.paths[1],
        &config,
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap();
    lab.paths[1] = selected.clone();
    let day = 86_400_000;
    let trust = lab.bootstrap(0).trust();
    let seat = Me {
        uuid: successor.clone(),
        label: Some("m34".into()),
        chosen: None,
        hw_hint: keys::hw_hint(&trust.network, &probe().platform_uuid),
        enrolled_ms: None,
        former: Vec::new(),
    };
    let live = registry.live_uuids().unwrap();
    assert!(live.contains(&earlier) && live.contains(&successor));
    let unfixed = super::plan(
        &Input {
            me: &seat,
            probe: &probe(),
            live: &live,
            heard: true,
            now_ms: NOW + day,
        },
        &registry.view().unwrap(),
    )
    .unwrap();
    let born = keys::machine_pk(&successor);
    assert_eq!(unfixed.outcome, Outcome::Born { mac: born.clone() });
    registry.write(vec![unfixed.changes]).unwrap();
    let before = registry.view().unwrap();
    assert!(super::may_fold(&before, &successor, &seat.hw_hint, true));
    assert!(!super::may_fold(&before, &successor, &seat.hw_hint, false));
    assert!(!super::may_fold(&before, &earlier, &seat.hw_hint, true));
    let unheard = Registry::at(selected.clone())
        .register(&config, &probe(), false, NOW + 2 * day)
        .unwrap();
    assert_eq!((unheard.outcome, unheard.records), (Outcome::Unchanged, 0));
    let pass = Registry::at(selected.clone())
        .register(&config, &probe(), true, NOW + 2 * day)
        .unwrap();
    assert_eq!(
        (pass.outcome, pass.records),
        (Outcome::Refreshed { mac: mac.clone() }, 1)
    );
    let view = registry.view().unwrap();
    assert!(!super::may_fold(&view, &successor, &seat.hw_hint, true));
    let read = &view.owned[MACHINE_FACTS.name];
    let row = &read.rows[&successor].value;
    assert_eq!(row["mac"], json!(born));
    assert_eq!(row["merged_into"], json!(mac));
    assert_eq!(row["predecessor"], json!(earlier));
    let shown =
        crate::alab::view::LabView::read_view(&view, read, &OwnedRead::default(), Some(&successor));
    assert_eq!(
        (shown.me.as_deref(), shown.my_label()),
        (Some(mac.as_str()), Some("m1"))
    );
    let again = Registry::at(selected)
        .register(&config, &probe(), true, NOW + 2 * day)
        .unwrap();
    assert_eq!((again.outcome, again.records), (Outcome::Unchanged, 0));
}
