// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn raw_fallback_witness_keeps_logical_resume_and_conversation_across_new_processes() {
    let entry = Path::new("/private/pkg/store/codex/exec/codex");
    let record = |pid, launch, resume, session, tab| {
        J::obj([
            ("pid", J::Int(pid)),
            ("launchArgv0", J::s(launch)),
            (
                "resumeArgv",
                J::Arr(vec![J::s(resume), J::s("resume"), J::s(session)]),
            ),
            ("sessionId", J::s(session)),
            ("atermSession", J::s(tab)),
        ])
    };
    let logical = entry.to_str().expect("fixture path");
    let before = record(41, logical, logical, "conversation", "owned tab");
    let after = record(42, logical, logical, "conversation", "owned tab");
    assert!(logical_resume(&before, entry));
    assert!(same_conversation(&before, &after));
    for bad in [
        record(42, "/physical/A", logical, "conversation", "owned tab"),
        record(42, logical, "/physical/A", "conversation", "owned tab"),
    ] {
        assert!(!logical_resume(&bad, entry));
    }
    for bad in [
        record(42, logical, logical, "other conversation", "owned tab"),
        record(42, logical, logical, "conversation", "other tab"),
        record(42, logical, "/other/resume", "conversation", "owned tab"),
    ] {
        assert!(!same_conversation(&before, &bad));
    }
}

#[test]
fn handoff_witness_rejects_new_conversation_process_or_act_and_shortened_deadline() {
    let record = J::obj([
        ("pid", J::Int(42)),
        ("identity", J::s("kernel identity")),
        ("sessionId", J::s("conversation")),
        ("launchArgv0", J::s("/private/launch")),
        (
            "resumeArgv",
            J::Arr(vec![J::s("resume"), J::s("conversation")]),
        ),
        ("carriedOn", J::Int(0)),
        ("atermSession", J::s("s-private")),
    ]);
    assert!(same_custody(&record, &record));
    let J::Obj(fields) = &record else {
        unreachable!()
    };
    for (key, _) in fields {
        let mut changed = fields.clone();
        changed.iter_mut().find(|(k, _)| k == key).expect("field").1 = J::s("different");
        assert!(!same_custody(&record, &J::Obj(changed)), "{key}");
        let missing = J::Obj(fields.iter().filter(|(k, _)| k != key).cloned().collect());
        assert!(!same_custody(&record, &missing), "missing {key}");
    }
    let owed = |text: &str, at| {
        J::obj([
            ("thread", J::s("conversation")),
            ("rule", J::s("continue@v1")),
            ("text", J::s(text)),
            ("notBefore", J::Int(at)),
        ])
    };
    assert!(same_owed(&owed("continue", 100), &owed("continue", 101)));
    let J::Obj(mut reordered) = owed("continue", 101) else {
        unreachable!()
    };
    reordered.reverse();
    assert!(same_owed(&owed("continue", 100), &J::Obj(reordered)));
    assert!(!same_owed(&owed("continue", 100), &owed("continue", 99)));
    assert!(!same_owed(
        &owed("continue", 100),
        &owed("retry refused action", 101)
    ));
    assert!(!same_owed(&J::Null, &J::Null));
    let request = |text: &str, at| J::obj([("u", J::s(text)), ("t", J::Int(at))]);
    let original = owed("continue", 100);
    let at_deadline = request("continue", 200);
    assert!(one_carried_request(
        std::slice::from_ref(&at_deadline),
        &original,
        200
    ));
    assert!(!one_carried_request(
        &[request("continue", 150)],
        &original,
        200
    ));
    assert!(!one_carried_request(
        &[request("different action", 200)],
        &original,
        200
    ));
    assert!(!one_carried_request(
        &[at_deadline.clone(), at_deadline],
        &original,
        200
    ));
    assert!(!one_carried_request(&[], &original, 200));
}

#[test]
fn restored_deadline_is_not_read_from_the_initial_unclaimed_writer_snapshot() {
    let record = |version, pid, autopilot, claim| {
        J::obj([
            ("version", J::s(version)),
            ("pid", J::Int(pid)),
            ("autopilot", J::s(autopilot)),
            ("claim", claim),
        ])
    };
    assert!(active_image_record(
        &record("new", 42, "active", J::s("codex-alab@42")),
        "new",
        42
    ));
    assert!(!active_image_record(
        &record("new", 42, "off", J::Null),
        "new",
        42
    ));
    assert!(!active_image_record(
        &record("old", 42, "active", J::s("codex-alab@42")),
        "new",
        42
    ));
    assert!(!active_image_record(
        &record("new", 42, "active", J::s("codex-alab@43")),
        "new",
        42
    ));
    assert!(!active_image_record(
        &record("new", 43, "active", J::s("codex-alab@42")),
        "new",
        42
    ));
}

#[test]
fn auth_path_witness_matches_exact_isolated_path() {
    let auth = Path::new("/private/scratch/isolated-auth/auth.json");
    let ok = J::obj([(
        "env",
        J::obj([
            ("CODEX_HOME", J::s("/private/scratch/codex-home")),
            (
                "CODEX_AUTH_PATH",
                J::s("/private/scratch/isolated-auth/auth.json"),
            ),
        ]),
    )]);
    let wrong = J::obj([(
        "env",
        J::obj([("CODEX_AUTH_PATH", J::s("/private/scratch/other/auth.json"))]),
    )]);
    let missing = J::obj([("env", J::obj([("CODEX_HOME", J::s("/private/scratch"))]))]);
    assert!(record_carries_auth_path(&ok, auth));
    assert!(!record_carries_auth_path(&wrong, auth));
    assert!(!record_carries_auth_path(&missing, auth));
}

#[test]
fn carried_request_reads_full_user_text_past_excerpt_cap() {
    let full = aterm_agent::supervise::config::CONTINUE_TEXT;
    assert!(full.len() > 300);
    let tail = &full[full.len() - 300..];
    let owed = J::obj([("text", J::s(full))]);
    let row = J::obj([
        ("t", J::Int(1_792_998_984_130)),
        ("u", J::s(tail)),
        ("u_full", J::s(full)),
    ]);
    assert!(one_carried_request(&[row], &owed, 1_792_998_984_055));
}

/// THE CARRIED CONTINUATION IS ITS OWN REQUEST: the stub answers it with a failing command
/// (`continue:fail`) and the request after that command, which carries its output, done
/// (`continue:continue`). The lane times and counts the continuation's own request: one typed
/// before its deadline is early whatever ran after it, and a second delivery that lands
/// while the command runs (the request after it then ends in the owed text, no output) is a
/// second request. NEGATIVE CONTROL: delivered once at its deadline, it passes.
#[test]
fn the_carried_continuation_is_timed_and_counted_by_its_own_request() {
    let text = aterm_agent::supervise::config::CONTINUE_TEXT;
    let owed = J::obj([("text", J::s(text))]);
    let row = |tag: &str, t: i64, tool: &str| {
        J::obj([
            ("t", J::Int(t)),
            ("tag", J::s(tag)),
            ("u_full", J::s(text)),
            ("tool_output", J::s(tool)),
        ])
    };
    let person = row("continue:person", 10, "null");
    let ran = |t| row("continue:continue", t, "Process exited with code 3");
    let early = carried_requests(vec![
        person.clone(),
        row("continue:fail", 100, "null"),
        ran(200),
    ]);
    assert_eq!(early.len(), 1, "{early:?}");
    assert!(
        !one_carried_request(&early, &owed, 150),
        "the act ran before its deadline, though the request after its command ran after it"
    );
    let twice = carried_requests(vec![
        row("continue:fail", 200, "null"),
        row("continue:continue", 300, "null"),
    ]);
    assert!(!one_carried_request(&twice, &owed, 150), "delivered twice");
    let once = carried_requests(vec![person, row("continue:fail", 200, "null"), ran(300)]);
    assert!(one_carried_request(&once, &owed, 150), "{once:?}");
    assert!(continued(&[
        "continue:person".to_string(),
        "continue:fail".to_string()
    ]));
    assert!(!continued(&["continue:person".to_string()]));
}
