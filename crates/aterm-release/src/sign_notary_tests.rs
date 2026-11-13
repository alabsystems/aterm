// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn fixture(body: &str) -> (aterm_tempfile::TempDir, PathBuf, PathBuf) {
    let dir = aterm_tempfile::tempdir().unwrap();
    let artifact = dir.path().join("test.dmg");
    std::fs::write(&artifact, b"exact uploaded bytes").unwrap();
    let body = body.replace("TEST_SHA", &crate::dmg::sha256_file(&artifact).unwrap());
    let shell = aterm_uds::deadline::stand_in(Path::new("/bin/sh"));
    let tool = dir.path().join("notarytool");
    // The shell is an in-place, ad-hoc signed Apple stand-in, never an
    // unlinked copied Mach-O. Each fixture owns its state beside the script.
    aterm_tempfile::stand_in(
        &tool,
        &format!(
            "#!{}\ncd -- \"$(dirname -- \"$0\")\"\nprintf '%s\\n' \"$2\" >> calls\n{body}\n",
            shell.display()
        ),
    )
    .unwrap();
    (dir, tool, artifact)
}

fn run(tool: &Path, artifact: &Path, bound: Duration) -> Result<(), String> {
    notary_accept(
        artifact,
        &NotaryAuth::KeychainProfile("fake".into()),
        tool.to_str().unwrap(),
        bound,
        Duration::ZERO,
    )
}

const RECOVER: &str = r#"
case "$2" in
submit) basename -- "$3" > name; echo 'Error: gatewayTimeout(statusCode: Optional(504))' >&2; exit 1;;
history) printf '{"history":[{"id":"owned-id","name":"%s","status":"In Progress"}]}\n' "$(cat name)";;
info) echo '{"id":"owned-id","status":"Accepted"}';;
log) printf '{"jobId":"owned-id","status":"Accepted","sha256":"TEST_SHA","archiveFilename":"%s"}\n' "$(cat name)";;
esac
"#;

#[test]
fn hostile_clock_advance_at_accepted_response_refuses() {
    let (dir, tool, artifact) = fixture(RECOVER);
    let start = Instant::now();
    let bound = Duration::from_secs(60);
    let error = notary_accept_with_clock(
        &artifact,
        &NotaryAuth::KeychainProfile("fake".into()),
        tool.to_str().unwrap(),
        bound,
        Duration::ZERO,
        || {
            if std::fs::read_to_string(dir.path().join("calls"))
                .unwrap_or_default()
                .contains("info")
            {
                start + bound
            } else {
                start
            }
        },
    )
    .unwrap_err();
    assert!(error.contains("deadline"));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("calls")).unwrap(),
        "submit\nhistory\ninfo\n"
    );
}

#[test]
fn hostile_log_evidence_must_agree_and_remain_bounded() {
    for (old, new) in [
        ("\"jobId\":\"owned-id\"", "\"jobId\":\"other-id\""),
        ("\"sha256\":\"TEST_SHA\"", "\"missing\":\"TEST_SHA\""),
        (
            "\"archiveFilename\":\"%s\"",
            "\"archiveFilename\":\"stale\"",
        ),
        (
            "log) printf",
            "log) echo 'HTTP 403 authentication HTTP 504' >&2; exit 1; printf",
        ),
        ("log) printf", "log) echo 'garbled'; exit 0; printf"),
    ] {
        let (dir, tool, artifact) = fixture(&RECOVER.replace(old, new));
        assert!(run(&tool, &artifact, Duration::from_secs(60)).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("calls")).unwrap(),
            "submit\nhistory\ninfo\nlog\n"
        );
    }
    let (dir, tool, artifact) =
        fixture(&RECOVER.replace("log) printf", "log) echo 'HTTP 504' >&2; exit 1; printf"));
    assert!(
        run(&tool, &artifact, Duration::from_secs(60))
            .unwrap_err()
            .contains("exhausted")
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("calls"))
            .unwrap()
            .lines()
            .count(),
        160
    );
}

#[test]
fn hostile_ambiguous_history_refuses_before_polling() {
    let body = RECOVER.replace(
        "\"status\":\"In Progress\"}]}",
        "\"status\":\"In Progress\"},{\"id\":\"concurrent-id\",\"name\":\"%s\",\"status\":\"Accepted\"}]}",
    ).replace("\"$(cat name)\";;", "\"$(cat name)\" \"$(cat name)\";;");
    let (dir, tool, artifact) = fixture(&body);
    assert!(
        run(&tool, &artifact, Duration::from_secs(60))
            .unwrap_err()
            .contains("ambiguous")
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("calls")).unwrap(),
        "submit\nhistory\n"
    );
}

#[test]
fn hostile_expired_budget_starts_no_command() {
    let (dir, tool, artifact) = fixture(RECOVER);
    assert!(
        run(&tool, &artifact, Duration::ZERO)
            .unwrap_err()
            .contains("exhausted")
    );
    assert!(!dir.path().join("calls").exists());
}

#[test]
fn hostile_truncated_prefix_must_not_become_accepted_json() {
    let body = RECOVER.replace(
        "info) echo",
        "info) printf 'garbled'; /usr/bin/head -c 1048576 /dev/zero | /usr/bin/tr '\\000' ' '; echo",
    );
    let (_dir, tool, artifact) = fixture(&body);
    assert!(run(&tool, &artifact, Duration::from_secs(60)).is_err());
}

#[test]
fn hostile_same_name_other_bytes_refused() {
    for other in ["earlier-id", "concurrent-id"] {
        let body = RECOVER
            .replace("owned-id", other)
            .replace("TEST_SHA", "other-bytes");
        let (dir, tool, artifact) = fixture(&body);
        assert!(
            run(&tool, &artifact, Duration::from_secs(60))
                .unwrap_err()
                .contains("does not attest")
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("calls"))
                .unwrap()
                .lines()
                .filter(|v| *v == "submit")
                .count(),
            1
        );
    }
}

#[test]
fn hostile_changed_source_refused() {
    let body = RECOVER.replace("info) echo", "info) echo changed > test.dmg; echo");
    let (_dir, tool, artifact) = fixture(&body);
    assert!(
        run(&tool, &artifact, Duration::from_secs(60))
            .unwrap_err()
            .contains("artifact changed")
    );
}

#[test]
fn hostile_credential_echo_is_not_logged() {
    const MARKER: &str = "NOTARY_HOSTILE_LOG_CHILD";
    if std::env::var_os(MARKER).is_some() {
        let (_dir, tool, artifact) = fixture("echo 'echoed-sensitive-value HTTP 401' >&2; exit 1");
        assert!(run(&tool, &artifact, Duration::from_secs(60)).is_err());
        return;
    }
    let out = command(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "sign::notary_tests::hostile_credential_echo_is_not_logged",
            "--nocapture",
        ])
        .env(MARKER, "1")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(!String::from_utf8_lossy(&out.stdout).contains("echoed-sensitive-value"));
    assert!(!String::from_utf8_lossy(&out.stderr).contains("echoed-sensitive-value"));
}

#[test]
fn hostile_malformed_and_mid_poll_credentials_refuse() {
    for reply in [
        "echo '{'",
        "echo garbage",
        "echo 'HTTP 401 authentication HTTP 504' >&2; exit 1",
    ] {
        let body = RECOVER.replace(
            "echo '{\"id\":\"owned-id\",\"status\":\"Accepted\"}'",
            reply,
        );
        let (dir, tool, artifact) = fixture(&body);
        assert!(run(&tool, &artifact, Duration::from_secs(60)).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("calls")).unwrap(),
            "submit\nhistory\ninfo\n"
        );
    }
}

#[test]
fn notary_504_existing_submission_is_adopted_without_resubmit() {
    let (dir, tool, artifact) = fixture(RECOVER);
    run(&tool, &artifact, Duration::from_secs(60)).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("calls")).unwrap(),
        "submit\nhistory\ninfo\nlog\n"
    );
}

#[test]
fn notary_wait_504_then_success() {
    let body = RECOVER
        .replace(
            "echo 'Error: gatewayTimeout(statusCode: Optional(504))' >&2; exit 1",
            "echo '{\"id\":\"owned-id\",\"status\":\"In Progress\"}'",
        )
        .replace(
            "info) echo",
            "info) if [ ! -f waited ]; then touch waited; echo 'HTTP 504' >&2; exit 1; fi; echo",
        );
    let (dir, tool, artifact) = fixture(&body);
    run(&tool, &artifact, Duration::from_secs(60)).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("calls")).unwrap(),
        "submit\ninfo\ninfo\nlog\n"
    );
}

#[test]
fn notary_504_then_invalid_is_terminal() {
    let (dir, tool, artifact) = fixture(&RECOVER.replace("Accepted", "Invalid"));
    assert!(
        run(&tool, &artifact, Duration::from_secs(60))
            .unwrap_err()
            .contains("definitive rejection")
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("calls")).unwrap(),
        "submit\nhistory\ninfo\n"
    );
}

#[test]
fn notary_history_visibility_can_lag_and_unrelated_acceptance_is_ignored() {
    let body = RECOVER.replace("history) printf", "history) if [ ! -f listed ]; then touch listed; echo '{\"history\":[{\"id\":\"other\",\"name\":\"test.dmg\",\"status\":\"Accepted\"}]}'; exit 0; fi; printf");
    let (dir, tool, artifact) = fixture(&body);
    run(&tool, &artifact, Duration::from_secs(60)).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("calls")).unwrap(),
        "submit\nhistory\nhistory\ninfo\nlog\n"
    );
}

#[test]
fn notary_history_rejected_stops_before_info() {
    let (dir, tool, artifact) = fixture(&RECOVER.replace("In Progress", "Rejected"));
    assert!(
        run(&tool, &artifact, Duration::from_secs(60))
            .unwrap_err()
            .contains("definitive rejection")
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("calls")).unwrap(),
        "submit\nhistory\n"
    );
}

#[test]
fn notary_acceptance_requires_same_id_and_known_verdict() {
    for replacement in [
        "{\"id\":\"other\",\"status\":\"Accepted\"}",
        "{\"id\":\"owned-id\",\"status\":\"Unknown\"}",
    ] {
        let (_dir, tool, artifact) =
            fixture(&RECOVER.replace("{\"id\":\"owned-id\",\"status\":\"Accepted\"}", replacement));
        assert!(run(&tool, &artifact, Duration::from_secs(60)).is_err());
    }
}

#[test]
fn notary_credentials_are_terminal_even_with_gateway_text() {
    let (dir, tool, artifact) =
        fixture("echo 'HTTP 401 authentication failed; HTTP 504' >&2; exit 1");
    assert!(
        run(&tool, &artifact, Duration::from_secs(60))
            .unwrap_err()
            .contains("credential/authorization")
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("calls")).unwrap(),
        "submit\n"
    );
}

#[test]
fn notary_unknown_upload_exhausts_bound_without_resubmit() {
    let (dir, tool, artifact) = fixture(
        "case \"$2\" in submit) echo 'HTTP 504' >&2; exit 1;; history) echo '{\"history\":[]}' ;; esac",
    );
    let error = run(&tool, &artifact, Duration::from_secs(60)).unwrap_err();
    assert!(error.contains("exhausted"), "{error}");
    let calls = std::fs::read_to_string(dir.path().join("calls")).unwrap();
    assert_eq!(calls.lines().count(), 160);
    assert_eq!(calls.lines().filter(|s| *s == "submit").count(), 1);
    assert!(!calls.contains("info"));
}

#[test]
fn negative_control_old_streamed_submit_refuses_first_504() {
    let (dir, tool, artifact) = fixture(RECOVER);
    let error = run_streamed(
        command(&tool).args(["notarytool", "submit"]).arg(artifact),
        "notarytool submit",
        Duration::from_secs(60),
    )
    .unwrap_err();
    assert!(error.contains("notarytool submit failed"));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("calls")).unwrap(),
        "submit\n"
    );
}
