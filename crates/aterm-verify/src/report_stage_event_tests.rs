// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn event(report: &Report) -> aterm_json::Value {
    let line = stage_event(
        3,
        Some(Check::Cells),
        report,
        Duration::ZERO,
        Duration::ZERO,
        None,
    );
    aterm_json::from_str(line.strip_prefix("verify-event: ").unwrap()).unwrap()
}

/// Replay exact bytes from a retired historical compiler run, not a new compiler invocation.
#[test]
fn historical_cells_warning_reaches_the_stage_diagnostic_without_changing_its_result() {
    let captured = include_str!("../tests/fixtures/cells-linux-arm-warning-2026-11-08.txt");
    // The human renderer prepended the ladder row. cells::record_cell records that
    // row separately; its Raw entries contain only the following timing/diagnostic bytes.
    let (_, raw) = captured.split_once('\n').unwrap();
    let mut report = Report::new("historical cells replay");
    report.fail("historical Linux ARM test-target warning");
    report.raw(raw);
    let findings = report.findings();
    let tally = crate::ladder::tally(std::slice::from_ref(&report));
    let value = event(&report);
    let message = value["finding_diagnostics"]["rows"][0]["message"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(aterm_json::Value::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        message.contains("warning:") && message.contains("producer_returns"),
        "{message}"
    );
    assert!(
        message.contains("resource_admission_member/tests.rs:44:5"),
        "{message}"
    );
    assert_eq!(value["findings"][0].as_str(), Some(findings[0].id.as_str()));
    assert_eq!(report.findings(), findings);
    assert_eq!(crate::ladder::tally(std::slice::from_ref(&report)), tally);
    assert_eq!(value["decisions"][0]["outcome"].as_str(), Some("FAIL"));
    assert_eq!(tally.gate_failures.len(), 1);
}

#[test]
fn stage_diagnostics_reuse_failed_row_excerpts_without_crossing_row_boundaries() {
    let mut report = Report::new("cross cells");
    report.fail("linux-arm — TYPE-ERROR");
    report.raw("2.0s\nwarning: first owned compiler warning\n  --> crates/fixture/src/a.rs:7:9");
    report.pass("mac-x64 — checked");
    report.raw("error: passing raw text is not a new failure");
    report.fail("windows-arm — TYPE-ERROR");
    report.raw("error[E0123]: second owned compiler diagnostic\n  --> crates/fixture/src/b.rs:8:4");
    let before = report.findings();
    let value = event(&report);
    assert_eq!(value["check"].as_str(), Some("cells"));
    assert_eq!(value["stage_index"].as_u64(), Some(3));
    let ids: Vec<_> = value["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        before
            .iter()
            .map(|finding| finding.id.as_str())
            .collect::<Vec<_>>()
    );
    let details = &value["finding_diagnostics"];
    assert_eq!(details["rows_total"].as_u64(), Some(2));
    assert_eq!(details["rows_omitted"].as_u64(), Some(0));
    for (row, original) in details["rows"].as_array().unwrap().iter().zip(&before) {
        assert_eq!(row["id"].as_str(), Some(original.id.as_str()));
        assert_eq!(row["message"], aterm_json::json!(original.message));
        assert_eq!(row["message_truncated_lines"].as_u64(), Some(0));
    }
    let first = details["rows"][0].to_string();
    let second = details["rows"][1].to_string();
    assert!(first.contains("first owned compiler warning"));
    assert!(!first.contains("second owned"));
    assert!(second.contains("second owned compiler diagnostic"));
    assert!(!value.to_string().contains("passing raw text"));
    assert_eq!(report.findings(), before);
    assert_eq!(value["decisions"][0]["outcome"].as_str(), Some("FAIL"));
    assert_eq!(value["decisions"][1]["outcome"].as_str(), Some("ok"));
}

#[test]
fn added_diagnostic_object_is_encoded_bounded_with_truthful_omissions() {
    let huge = "界\n\u{1b}\\\"".repeat(2000);
    let finding = Finding {
        id: huge.clone(),
        rerun: Some("not projected".into()),
        message: vec![huge; STAGE_MESSAGE_LINES + 3],
    };
    let mut report = Report::new("large diagnostics");
    report.fail_with_findings("failed", vec![finding; FINDINGS_SHOWN + 5]);
    let value = event(&report);
    let details = &value["finding_diagnostics"];
    assert!(details.to_string().len() <= STAGE_DIAGNOSTIC_BYTES);
    let rows = details["rows"].as_array().unwrap();
    assert!(!rows.is_empty());
    assert!(rows.len() <= FINDINGS_SHOWN);
    assert_eq!(
        details["rows_total"].as_u64(),
        Some(u64::try_from(FINDINGS_SHOWN + 5).unwrap())
    );
    assert_eq!(
        details["rows_omitted"].as_u64(),
        Some(u64::try_from(FINDINGS_SHOWN + 5 - rows.len()).unwrap())
    );
    assert_eq!(details["complete"].as_bool(), Some(false));
    for row in rows {
        assert_eq!(row["id_truncated"].as_bool(), Some(true));
        assert_eq!(
            row["message_lines_total"].as_u64(),
            Some(u64::try_from(STAGE_MESSAGE_LINES + 3).unwrap())
        );
        assert_eq!(row["message_lines_omitted"].as_u64(), Some(3));
        assert_eq!(
            row["message_truncated_lines"].as_u64(),
            Some(u64::try_from(STAGE_MESSAGE_LINES).unwrap())
        );
        for line in row["message"].as_array().unwrap() {
            assert!(line.to_string().len() <= EVENT_ROW_BYTES + 2);
        }
    }
    assert!(!details.to_string().contains("not projected"));
    // Only the new object is bounded: unchanged legacy finding IDs are still complete.
    assert_eq!(
        value["findings"].as_array().unwrap().len(),
        FINDINGS_SHOWN + 5
    );
    assert!(value["findings"][0].as_str().unwrap().len() > EVENT_ROW_BYTES);
}

#[test]
fn passing_raw_error_text_does_not_create_finding_diagnostics() {
    let mut report = Report::new("passed");
    report.pass("checked");
    report.raw("error: fake failure text from a successful tool");
    let value = event(&report);
    assert_eq!(value["finding_diagnostics"]["rows_total"].as_u64(), Some(0));
    assert!(
        value["finding_diagnostics"]["rows"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(value["decisions"][0]["outcome"].as_str(), Some("ok"));
    assert!(!value.to_string().contains("fake failure"));
}
