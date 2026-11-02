// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::table::tests::Lab;
use aterm_secrets::commons::roster::Role;
use aterm_spec::interp::State;

fn field_mut<'a>(value: &'a mut Value, name: &str) -> &'a mut Value {
    value.as_object_mut().unwrap().get_mut(name).unwrap()
}

fn add_late_conflict(snapshot: &mut Snapshot) {
    let ((collection, original), mut cell) = snapshot
        .cells
        .iter()
        .next()
        .map(|(k, c)| (k.clone(), c.clone()))
        .unwrap();
    let key = "f".repeat(64);
    assert!(original < key);
    cell.candidates.push(cell.candidates[0].clone());
    for candidate in &mut cell.candidates {
        candidate
            .value
            .as_object_mut()
            .unwrap()
            .insert("source_sha256".into(), json!(key.as_str()));
    }
    snapshot.cells.insert((collection, key), cell);
}

fn fixture() -> (Lab, Snapshot, Value) {
    let lab = Lab::new(&[Role::Writer]);
    let source = lab._home.path().join("source.csv");
    std::fs::write(
        &source,
        "email,computer,google,notes\na@example.test,m1.1,1,PRIVATE-NOTE\nb@example.test,m2.1,1,\nc@example.test,m3.1,1,\n",
    ).unwrap();
    let db = Imports::at(lab.paths[0].clone());
    db.apply(&source, [61; 16]).unwrap();
    let snapshot = db.snapshot().unwrap();
    std::fs::write(
        &source,
        "email,computer,google,notes\na@example.test,m1.1,0,PRIVATE-NOTE\nb@example.test,m2.1,0,\nc@example.test,m3.1,0,\n",
    ).unwrap();
    (lab, snapshot, preview(&source).unwrap())
}

fn projection(scenario: i64, snapshot: &Snapshot, report: &Value) -> State {
    let comparisons = report["comparisons"].as_array().unwrap();
    // The fixture's condition is independent of the production completeness predicate.
    let history = matches!(scenario, 1 | 2 | 6 | 7 | 8 | 9);
    let conflict = snapshot
        .cells
        .values()
        .any(|cell| cell.candidates.len() > 1);
    let bool_at = |value: &Value, field: &str| value[field].as_bool().unwrap();
    State::from([
        ("scenario", scenario),
        (
            "source_bounded",
            i64::from(comparisons.len() as u64 <= report["source_limit"].as_u64().unwrap()),
        ),
        (
            "row_bounded",
            i64::from(comparisons.iter().all(|row| {
                row["row_details"].as_array().unwrap().len() as u64
                    <= row["row_detail_limit"].as_u64().unwrap()
            })),
        ),
        (
            "source_omitted",
            i64::from((comparisons.len() as u64) < report["source_candidates"].as_u64().unwrap()),
        ),
        (
            "source_truncated",
            i64::from(bool_at(report, "sources_truncated")),
        ),
        (
            "row_omitted",
            i64::from(comparisons.iter().any(|row| {
                (row["row_details"].as_array().unwrap().len() as u64)
                    < row["changed_rows"].as_u64().unwrap()
            })),
        ),
        (
            "row_truncated",
            i64::from(
                comparisons
                    .iter()
                    .any(|row| bool_at(row, "row_details_truncated")),
            ),
        ),
        ("history", i64::from(history)),
        (
            "history_shown",
            i64::from(bool_at(report, "history_complete")),
        ),
        ("conflict", i64::from(conflict)),
        (
            "conflict_shown",
            i64::from(report["conflicted_sources"].as_u64().unwrap() > 0),
        ),
        (
            "authority",
            i64::from(report["authority"].as_str() != Some("none")),
        ),
    ])
}

pub(crate) fn transition(state: &State, action: &str) -> State {
    assert_eq!(state["scenario"], 0);
    let (lab, mut snapshot, current) = fixture();
    let mut source_limit = SOURCE_LIMIT;
    let mut detail_limit = DETAIL_LIMIT;
    let scenario = match action {
        "Complete" => 1,
        "SourcesCapped" => {
            let second = lab._home.path().join("second.csv");
            std::fs::write(&second, "email,google\nd@example.test,1\n").unwrap();
            let db = Imports::at(lab.paths[0].clone());
            db.apply(&second, [62; 16]).unwrap();
            snapshot = db.snapshot().unwrap();
            source_limit = 1;
            2
        }
        "Withheld" => {
            snapshot.withheld = 1;
            3
        }
        "Opaque" => {
            snapshot.operations[0].available = false;
            4
        }
        "Rejected" => {
            snapshot.operations[0].refusal = Some("invalid retained operation".into());
            5
        }
        "RowsCapped" => {
            detail_limit = 1;
            6
        }
        "Conflict" => {
            let cell = snapshot.cells.values_mut().next().unwrap();
            let mut other = cell.candidates[0].clone();
            let rows = field_mut(&mut other.value, "rows").as_array_mut().unwrap();
            let providers = field_mut(&mut rows[0], "providers").as_array_mut().unwrap();
            providers[0]
                .as_object_mut()
                .unwrap()
                .insert("presence_marker".into(), json!(false));
            cell.candidates.push(other);
            7
        }
        "Duplicate" => {
            let mut op = snapshot.operations[0].clone();
            op.duplicate_of = Some(op.header.id);
            op.header.id[0] ^= 1;
            op.eligible = false;
            snapshot.operations.push(op);
            8
        }
        "ConflictCapped" => {
            add_late_conflict(&mut snapshot);
            source_limit = 1;
            9
        }
        _ => panic!("unknown comparison action"),
    };
    let before = crate::tests::files(&lab.paths[0].home);
    let report = report_with_limits(&current, &snapshot, source_limit, detail_limit).unwrap();
    assert_eq!(crate::tests::files(&lab.paths[0].home), before);
    projection(scenario, &snapshot, &report)
}

#[test]
fn bounded_comparison_conforms_with_negative_controls() {
    let model = aterm_models_db::import_comparison_model();
    aterm_spec::interp::prove_and_catch(&model);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = transition(&state, action.name);
                assert!(model.successors(action.name, &state).contains(&next));
                let mut wrong = next.clone();
                wrong.insert("authority", 1);
                assert!(!model.check_invariant("NoAuthority", &wrong));
                assert!(!model.successors(action.name, &state).contains(&wrong));
            }
        }
    }
}

#[test]
fn counts_and_coordinates_do_not_disclose_source_values_or_select_a_replacement() {
    let (_lab, snapshot, current) = fixture();
    let report = report(&current, &snapshot).unwrap();
    let compared = &report["comparisons"][0];
    assert_eq!(compared["shared_identity_rows"], json!(3));
    assert_eq!(compared["changed_fields"]["providers"], json!(3));
    assert_eq!(compared["changed_fields"]["email"], json!(0));
    assert_eq!(compared["stored"]["presence_true"], json!(3));
    assert_eq!(report["current"]["presence_false"], json!(3));
    assert_eq!(compared["source_digest_matches"], json!(false));
    assert_eq!(compared["exact_candidate_match"], json!(false));
    assert_eq!(
        compared["row_details"][0],
        json!({"record":2,"changed_fields":["providers"]})
    );
    let text = aterm_json::to_string(&report).unwrap();
    for private in [
        "a@example.test",
        "b@example.test",
        "c@example.test",
        "PRIVATE-NOTE",
        "m1.1",
    ] {
        assert!(!text.contains(private));
    }
    let original = &snapshot.cells.values().next().unwrap().candidates[0].value;
    let exact = super::report(original, &snapshot).unwrap();
    assert_eq!(
        exact["comparisons"][0]["source_digest_matches"],
        json!(true)
    );
    assert_eq!(
        exact["comparisons"][0]["exact_candidate_match"],
        json!(true)
    );
    assert_eq!(exact["comparisons"][0]["identical_rows"], json!(3));
}

#[test]
fn missing_and_shifted_rows_do_not_become_identity_matches() {
    let (lab, snapshot, _) = fixture();
    let file = lab._home.path().join("shifted.csv");
    std::fs::write(&file, "email,computer,google\nchanged@example.test,m1.1,1\n\nc@example.test,m3.1,1\nd@example.test,m4.1,1\n").unwrap();
    let current = preview(&file).unwrap();
    let report = report(&current, &snapshot).unwrap();
    let row = &report["comparisons"][0];
    assert_eq!(row["shared_identity_rows"], json!(1));
    assert_eq!(row["only_current_rows"], json!(1));
    assert_eq!(row["only_stored_rows"], json!(1));
    assert_eq!(row["changed_fields"]["email"], json!(1));
}

#[test]
fn omitted_evidence_cannot_claim_a_complete_report() {
    let (_lab, snapshot, current) = fixture();
    let mut report = report_with_limits(&current, &snapshot, 0, 0).unwrap();
    assert_eq!(report["source_candidates"], json!(1));
    assert_eq!(report["sources_truncated"], json!(true));
    report
        .as_object_mut()
        .unwrap()
        .insert("sources_truncated".into(), json!(false));
    let state = projection(2, &snapshot, &report);
    assert!(
        !aterm_models_db::import_comparison_model()
            .check_invariant("SourceOmissionsVisible", &state)
    );
    let mut report = report_with_limits(&current, &snapshot, 1, 0).unwrap();
    assert_eq!(report["comparisons"][0]["changed_rows"], json!(3));
    field_mut(&mut report, "comparisons")
        .as_array_mut()
        .unwrap()[0]
        .as_object_mut()
        .unwrap()
        .insert("row_details_truncated".into(), json!(false));
    let state = projection(6, &snapshot, &report);
    assert!(
        !aterm_models_db::import_comparison_model().check_invariant("RowOmissionsVisible", &state)
    );
}

#[test]
fn a_conflict_beyond_the_comparison_limit_is_still_reported() {
    let (_lab, mut snapshot, current) = fixture();
    add_late_conflict(&mut snapshot);
    let mut report = report_with_limits(&current, &snapshot, 1, 64).unwrap();
    assert_eq!(report["comparisons"].as_array().unwrap().len(), 1);
    assert_eq!(report["comparisons"][0]["conflicted"], json!(false));
    assert_eq!(report["conflicted_sources"], json!(1));
    assert_eq!(report["sources_truncated"], json!(true));
    report
        .as_object_mut()
        .unwrap()
        .insert("conflicted_sources".into(), json!(0));
    assert!(
        !aterm_models_db::import_comparison_model()
            .check_invariant("ConflictsVisible", &projection(9, &snapshot, &report))
    );
}

#[test]
fn invalid_documents_are_refused_before_any_values_are_reported() {
    let (_lab, mut snapshot, mut current) = fixture();
    current
        .as_object_mut()
        .unwrap()
        .insert("untrusted".into(), json!("PRIVATE-EXTRA"));
    assert!(report(&current, &snapshot).is_err());
    current.as_object_mut().unwrap().remove("untrusted");
    let value = &mut snapshot.cells.values_mut().next().unwrap().candidates[0].value;
    field_mut(value, "rows").as_array_mut().unwrap()[0]
        .as_object_mut()
        .unwrap()
        .insert("email".into(), json!("PRIVATE-BAD-EMAIL"));
    let error = report(&current, &snapshot).unwrap_err().to_string();
    assert!(!error.contains("PRIVATE"));
}
