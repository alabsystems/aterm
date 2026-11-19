// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn record() -> Record {
    Record {
        holder: "m16 (node 7c1e, session s-1, pid 41)".into(),
        line: "v0.201.0@9afa50b42".into(),
        step: "wait".into(),
        own: true,
        at: 1_000,
        decision: "line v0.201.0 at 9afa50b42 is red: its run ended at a failing gate".into(),
        pid: 4242,
        boot: Some("ab".repeat(16)),
        start: Some(77),
    }
}

/// THE PUMP'S FACT CROSSES THE SURVEY WHOLE (UB-15, `pump=`). The record the pump writes reads
/// back as itself; its row carries the holder, line, step, whether the pump holds the line's
/// journal (what a standby judges before it adopts), the age the answering host measured and the
/// decision, words with spaces included, and the kernel's word on its pid; the survey's JSON of
/// the row reads back as the same pump.
/// NEGATIVE CONTROLS: a row with a field twice, another row's prefix and a record missing a field
/// read as nothing; a row's unknown liveness stays unknown, never live or dead; a row that does
/// not say whether the pump holds the journal says unknown, never that it does not.
#[test]
fn a_pump_record_and_its_survey_row_round_trip() {
    let record = record();
    assert_eq!(Record::from_json(&record.json()), Some(record.clone()));
    let row = record.row(Some(true), 1_045);
    assert!(row.starts_with(ROW), "{row}");
    let pump = Pump::parse(&row).expect("the row reads");
    assert_eq!(
        pump,
        Pump {
            alive: Some(true),
            pid: 4242,
            holder: Some(record.holder.clone()),
            line: record.line.clone(),
            step: "wait".into(),
            own: Some(true),
            at: 1_000,
            age_s: 45,
            decision: record.decision.clone(),
        }
    );
    let shown: aterm_json::Value = aterm_json::from_str(&pump.json()).unwrap();
    assert_eq!(Pump::from_value(&shown), Some(pump.clone()));
    assert_eq!(Pump::from_value(&aterm_json::Value::Null), None);
    let unsaid = Pump::parse(&row.replace(" own=1 ", " ")).unwrap();
    assert_eq!(unsaid.own, None);
    let other = Pump::parse(
        &Record {
            own: false,
            ..record.clone()
        }
        .row(Some(true), 1_045),
    )
    .unwrap();
    assert_eq!(other.own, Some(false));
    assert!(
        pump.summary()
            .starts_with("  pump=wait line=v0.201.0@9afa50b42 ")
    );
    assert!(
        pump.summary()
            .contains(" age=45s alive=live pid=4242 last=line v0.201.0 at 9afa50b42 is red"),
        "{}",
        pump.summary()
    );
    let json: aterm_json::Value = aterm_json::from_str(&pump.json()).unwrap();
    assert_eq!(json["age_s"].as_u64(), Some(45));
    assert_eq!(json["holder"].as_str(), Some(record.holder.as_str()));

    let unknown = Pump::parse(
        &Record {
            holder: String::new(),
            ..record.clone()
        }
        .row(None, 999),
    )
    .unwrap();
    assert_eq!(
        (unknown.alive, unknown.holder, unknown.age_s),
        (None, None, 0)
    );
    assert!(Pump::parse(&format!("{row} step=prepare")).is_none());
    assert!(Pump::parse(&row.replacen(ROW, "release_line ", 1)).is_none());
    assert!(Record::from_json(r#"{"holder":"","line":"v1","step":"wait","at":1}"#).is_none());
}
