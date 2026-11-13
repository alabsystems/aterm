// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn program_completion_requires_the_requested_exit_status() {
    assert!(program_finished("OK state=done why=exit-status exit=0", 0));
    assert!(program_finished(
        "OK state=failed why=exit-status exit=3",
        3
    ));
    for wrong in [
        "OK state=working why=exit-status exit=0",
        "OK state=done why=no-exit-status",
        "OK state=done why=exit-status exit=3",
        "OK state=failed why=exit-status exit=0",
    ] {
        assert!(!program_finished(wrong, 0));
    }
}

#[test]
fn stream_line_reads_the_single_line_cli_reply() {
    assert_eq!(
        parse_stream_line("OK HANDOFF_END 123\n").unwrap(),
        "HANDOFF_END 123"
    );
    assert!(parse_stream_line("ERR evicted\n").is_err());
    assert!(parse_stream_line("HANDOFF_END 123\n").is_err());
}

#[test]
fn stream_search_requires_the_complete_cli_count_and_reads_stdout_rows() {
    let mut reply = instance::Reply {
        code: 0,
        stdout: "92 0 14\n93 0 14\n".into(),
        stderr: "aterm-ctl: OK 2 screen_first=90\n".into(),
    };
    assert_eq!(parse_search_rows(&reply).unwrap(), [92, 93]);
    reply.stderr = "aterm-ctl: OK 2 incomplete moving screen_first=90\n".into();
    assert!(parse_search_rows(&reply).is_err());
    reply.stderr = "aterm-ctl: OK 3 screen_first=90\n".into();
    assert!(parse_search_rows(&reply).is_err());
    reply.stderr.clear();
    assert!(parse_search_rows(&reply).is_err());
}

#[test]
fn streamed_history_must_keep_every_record_once_in_order() {
    let good: Vec<String> = (0..4)
        .map(|i| format!("HANDOFF_SEQ {i:06} café λ 界 🙂"))
        .collect();
    assert!(verify_stream_lines(&good, 4).is_ok());
    let mut missing = good.clone();
    missing.remove(1);
    assert!(verify_stream_lines(&missing, 4).is_err());
    let mut duplicated = good.clone();
    duplicated[2].clone_from(&good[1]);
    assert!(verify_stream_lines(&duplicated, 4).is_err());
    let mut reordered = good.clone();
    reordered.swap(1, 2);
    assert!(verify_stream_lines(&reordered, 4).is_err());
    let mut corrupted = good.clone();
    corrupted[1] = corrupted[1].replace('🙂', "�");
    assert!(verify_stream_lines(&corrupted, 4).is_err());
    assert!(verify_stream_lines(&[], 0).is_err());
    assert!(verify_stream_lines(&good, STREAM_LIMIT).is_err());
}

#[test]
fn handoff_chain_is_explicitly_bounded() {
    assert_eq!(handoff_count(&mut vec![]).unwrap(), 2);
    for count in ["1", "10", "32"] {
        assert_eq!(
            handoff_count(&mut vec!["--handoffs".into(), count.into()]).unwrap(),
            count.parse::<usize>().unwrap()
        );
    }
    for count in ["0", "33", "-1", "unbounded"] {
        assert!(handoff_count(&mut vec!["--handoffs".into(), count.into()]).is_err());
    }
}

#[test]
fn a_changed_image_cannot_produce_a_passing_report() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("image");
    std::fs::write(&path, "first").unwrap();
    let image = ProbeImage::read(&path).unwrap();
    assert!(image.recheck().is_ok());
    std::fs::write(&path, "second").unwrap();
    assert!(image.recheck().is_err());
}

#[test]
fn evidence_created_after_admission_is_never_replaced() {
    fn failed(_: &Path, out: &Path) -> Result<(), String> {
        std::fs::write(out.join("failed.json"), "keep existing record").unwrap();
        Err("a later failure".into())
    }
    let out = aterm_tempfile::tempdir().unwrap();
    assert!(execute(Path::new("unused"), out.path(), failed).is_err());
    assert_eq!(
        std::fs::read_to_string(out.path().join("failed.json")).unwrap(),
        "keep existing record"
    );
    assert!(out.path().join("run.json").is_file());
}

#[test]
fn existing_log_is_not_truncated_by_capture() {
    let out = aterm_tempfile::tempdir().unwrap();
    let source = out.path().join("source");
    std::fs::write(&source, "new log").unwrap();
    std::fs::write(out.path().join("instance.log"), "keep old log").unwrap();
    let mut evidence = EvidenceLog::new(out.path());
    evidence.source = Some(source);
    assert!(evidence.capture().is_err());
    assert_eq!(
        std::fs::read_to_string(out.path().join("instance.log")).unwrap(),
        "keep old log"
    );
}

#[test]
fn a_failed_probe_keeps_its_error_and_log() {
    fn fails(_: &Path, out: &Path) -> Result<(), String> {
        let private = aterm_tempfile::tempdir().unwrap();
        let source = private.path().join("instance.log");
        std::fs::write(&source, "stand-in diagnostics\n").unwrap();
        let mut evidence = EvidenceLog::new(out);
        evidence.source = Some(source);
        Err("stand-in refusal".into())
    }
    let out = aterm_tempfile::tempdir().unwrap();
    assert!(execute(Path::new("unused"), out.path(), fails).is_err());
    let report: aterm_json::Value =
        aterm_json::from_str(&std::fs::read_to_string(out.path().join("failed.json")).unwrap())
            .unwrap();
    assert_eq!(report["error"].as_str(), Some("stand-in refusal"));
    assert_eq!(
        std::fs::read_to_string(out.path().join("instance.log")).unwrap(),
        "stand-in diagnostics\n"
    );
    assert!(!out.path().join("passed.json").exists());
}

#[test]
fn previous_evidence_is_never_overwritten_or_run_again() {
    let out = aterm_tempfile::tempdir().unwrap();
    let previous = out.path().join("previous");
    std::fs::write(&previous, "keep").unwrap();
    assert!(
        execute(Path::new("unused"), out.path(), |_, _| panic!(
            "must not run"
        ))
        .is_err()
    );
    assert_eq!(std::fs::read_to_string(previous).unwrap(), "keep");
    assert!(!out.path().join("failed.json").exists());
}

#[test]
fn in_place_exec_keeps_birth_but_changes_the_exec_version() {
    let identity = |text| aterm_uds::process::Identity::parse(text).unwrap();
    let before = identity("23@45:3");
    assert!(reexec_of(before, identity("23@45:4")));
    assert!(!reexec_of(before, before));
    assert!(!reexec_of(before, identity("24@45:4")));
    assert!(!reexec_of(before, identity("23@46:4")));
}

#[test]
fn cold_precondition_requires_an_exited_session_row() {
    assert!(exited_session(
        "OK 1\n0 s-0123456789abcdefabcd - exited held"
    ));
    assert!(!exited_session(
        "OK 1\n0 s-0123456789abcdefabcd - running exited"
    ));
    assert!(!exited_session("OK 1\n0 invalid - exited held"));
}

#[test]
fn fixture_records_require_a_live_exact_identity_and_the_requested_stage() {
    let line = "aterm-lab-installer stage=adopted gui=17 sid=s-0123456789abcdefabcd child=23 birth=45 active=true";
    assert_eq!(
        record(line, "adopted", 17),
        Some(Custody {
            sid: "s-0123456789abcdefabcd".into(),
            child: 23,
            birth: 45,
        })
    );
    assert!(record(line, "seeded", 17).is_none());
    assert!(record(line, "adopted", 18).is_none());
    for changed in [
        line.replace("birth=45", "birth=0"),
        line.replace("active=true", "active=false"),
        line.replace("child=23", "child=1"),
    ] {
        assert!(record(&changed, "adopted", 17).is_none());
    }
}
