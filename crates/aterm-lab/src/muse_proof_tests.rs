// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const STUB_LOG: &str = "\
18:13:10.100 stub listening on 127.0.0.1:50581 (proxy black hole: 50582) mode=ok pid=46492
18:13:10.820 GET /muse-code/models -> 200 body=0B mode=ok kind=muse-models auth=key models=1
18:13:26.349 POST /responses -> 200 body=133593B mode=selftest kind=muse auth=key model=lab-stub stream=true items=2
18:13:26.349 POST /responses -> 200 body=65500B mode=selftest kind=muse-side role=reminder:skill-reminder auth=key model=lab-stub
18:13:30.870 POST /responses -> 200 body=2235B mode=ok kind=muse-side role=prompt-hint auth=key model=lab-stub
18:13:31.000 PROXY refused: CONNECT graph.facebook.com:443 HTTP/1.1
";

/// Every request to the stub's Meta routes names the key it carried: the
/// turn, its side calls and the catalog.
#[test]
fn every_meta_request_names_the_key_it_carried() {
    assert_eq!(muse_auths(STUB_LOG), (4, Vec::new()));
    let login = format!(
        "{STUB_LOG}18:14:00.000 POST /responses -> 200 mode=ok kind=muse auth=jwt:owner model=m\n\
         18:14:01.000 POST /responses -> 401 mode=ok kind=muse-side role=judge model=m\n"
    );
    assert_eq!(
        muse_auths(&login),
        (6, vec!["jwt:owner".to_string(), "none".to_string()])
    );
    let claude_only = "13:00:00.000 POST /v1/messages -> 200 mode=ok kind=main auth=key\n";
    assert_eq!(muse_auths(claude_only), (0, Vec::new()));
}

/// THE KEY CHECK FAILS ON ANY CREDENTIAL BUT THE LAB'S KEY: the
/// selftest's own log passes it; a Meta login — a bearer token of
/// another shape, a request with none, each labelled as the stub labels
/// it ([`crate::stub::auth_label`]) — fails it.
#[test]
fn the_key_check_fails_on_any_credential_but_the_labs_key() {
    use crate::stub::auth_label;
    assert_eq!(key_verdict(STUB_LOG), Verdict::Ok);
    let with = |label: &str| {
        format!("{STUB_LOG}18:14:00.000 POST /responses -> 200 mode=ok kind=muse auth={label}\n")
    };
    let key = format!("Bearer {}", crate::stub::LAB_CODEX_KEY);
    assert_eq!(key_verdict(&with(&auth_label(Some(&key)))), Verdict::Ok);
    for label in [
        auth_label(Some("Bearer meta-session-token-of-a-login")),
        auth_label(None),
    ] {
        assert_eq!(key_verdict(&with(&label)), Verdict::Fail, "{label}");
    }
    let quiet: String = STUB_LOG
        .lines()
        .filter(|l| !l.contains(" kind=muse"))
        .map(|l| format!("{l}\n"))
        .collect();
    assert_eq!(key_verdict(&quiet), Verdict::Fail, "the control");
}

/// A file of the owner's Muse Code directories open in a Muse Code
/// process is named, its config and its sessions alike.
#[test]
fn a_file_open_under_the_owners_muse_dirs_is_named() {
    let home = &["/Users//owner".to_string()];
    let lsof = "p4242\nfcwd\nn/private/tmp/at.lab0/proj\nf3\nn/Users//owner/.config/muse/auth.json\n\
                f4\nn/Users//owner/.local/share/muse/sessions/a.jsonl\nf5\nn/Users//owner/.config/muse\n\
                f6\nn/Users//owner/.config/muse-other/x\nf7\nn/s/runs/t/scratch/home/.config/muse/.auth.json.lock\n\
                f8\nn/Users//owner/Documents/notes.txt\n\
                f9\nn/Users//owner/Library/Application Support/Muse/session-name-authority/session-names.db\n\
                f10\nn/Users//owner/Library/Application Support/Museum/x\n";
    assert_eq!(
        held_under(lsof, home),
        [
            "/Users//owner/.config/muse/auth.json",
            "/Users//owner/.local/share/muse/sessions/a.jsonl",
            "/Users//owner/.config/muse",
            "/Users//owner/Library/Application Support/Muse/session-name-authority/session-names.db",
        ]
    );
    let scratch_only = "p1\nn/s/runs/t/scratch/home/.config/muse/x\nn/Users//owner/.config/muse-other\n\
                        n/s/runs/t/scratch/home/Library/Application Support/Muse/x\n";
    assert!(held_under(scratch_only, home).is_empty(), "the control");
}

/// A kernel refusal naming the owner's Muse Code directories is named;
/// one naming another directory of the owner's, or a sibling whose name
/// only begins the same, is not.
#[test]
fn a_refusal_naming_the_owners_muse_dirs_is_named() {
    let home = &["/Users//owner".to_string()];
    let refusals = vec![
        "deny(1) file-read-data /Users//owner/.config/muse/auth.json".to_string(),
        "deny(1) file-read-metadata /Users//owner/.local/share/muse".to_string(),
        "deny(1) file-read-data /Users//owner/Library/Application Support/Muse/session-name-authority/session-names.db"
            .to_string(),
        "deny(1) file-read-data /Users//owner/.config/muse-other/x".to_string(),
        "deny(1) file-read-data /Users//owner/Library/Keychains/login.keychain-db".to_string(),
        "deny(1) file-read-data /Users//owner/Library/Application Support/Museum".to_string(),
    ];
    assert_eq!(naming_own_dirs(&refusals, home), refusals[..3].to_vec());
    assert!(naming_own_dirs(&[], home).is_empty(), "the control");
    assert_eq!(refusals_verdict(&refusals[..1], true), Verdict::Fail);
    assert_eq!(refusals_verdict(&[], false), Verdict::Unproven);
}

/// UNDER A SCRATCH `HOME` THE ACCOUNT'S MUSE CODE DIRECTORIES ARE
/// WATCHED: Muse Code 1.4.10 opened the account's own `session-names.db`
/// whatever `HOME` said, so a file held, or a refusal named, under the
/// account's home (the account database's) counts, as does one under the
/// scratch home's own.
#[test]
fn under_a_scratch_home_the_account_muse_dirs_are_watched() {
    let homes = crate::paths::guarded("/Users//owner", "/private/tmp/s/home");
    let db =
        "/Users//owner/Library/Application Support/Muse/session-name-authority/session-names.db";
    let lsof = format!(
        "p7\nf3\nn{db}\nf4\nn/private/tmp/s/home/.config/muse/auth.json\nf5\nn/Users//owner/Documents/x\n"
    );
    assert_eq!(
        held_under(&lsof, &homes),
        [db, "/private/tmp/s/home/.config/muse/auth.json"]
    );
    let refusals = vec![format!("deny(1) file-read-data {db}")];
    assert_eq!(naming_own_dirs(&refusals, &homes), refusals);
    let env_only = crate::paths::guarded("", "/private/tmp/s/home");
    assert_eq!(
        held_under(&lsof, &env_only),
        ["/private/tmp/s/home/.config/muse/auth.json"]
    );
    assert!(naming_own_dirs(&refusals, &env_only).is_empty());
}

/// THE OWNER'S-DATA CHECK FAILS ON A HELD FILE whatever its control saw,
/// holds when Muse Code wrote under its own scratch HOME and held
/// nothing, and is UNPROVEN when it wrote nothing there or no Muse Code
/// process was seen.
#[test]
fn the_owner_check_fails_on_a_held_file_and_needs_its_control() {
    let held = vec!["/Users//owner/.config/muse/auth.json".to_string()];
    let wrote = vec![".config/muse/.auth.json.lock".to_string()];
    assert_eq!(owner_verdict(&held, &wrote, &[7], true), Verdict::Fail);
    assert_eq!(owner_verdict(&held, &[], &[], false), Verdict::Fail);
    assert_eq!(owner_verdict(&[], &wrote, &[7], true), Verdict::Ok);
    assert_eq!(owner_verdict(&[], &[], &[7], true), Verdict::Unproven);
    assert_eq!(owner_verdict(&[], &wrote, &[], true), Verdict::Unproven);
    assert_eq!(owner_verdict(&[], &wrote, &[7], false), Verdict::Unproven);
    assert_eq!(owner_verdict(&held, &wrote, &[7], false), Verdict::Fail);
}

/// Only a whole read of a process's table proves an absence: a read that
/// failed is unknown, never a clean observer.
#[test]
fn an_open_file_observer_must_read_the_whole_table() {
    use crate::selftest::open_files_report;
    let wrote = vec![".config/muse/session".to_string()];
    let judge = |paths: std::io::Result<&[&str]>| {
        let read = paths.map(|p| crate::procs::OpenPaths {
            paths: p.iter().map(std::path::PathBuf::from).collect(),
            complete: true,
        });
        let seen = open_files_report(7, read);
        owner_verdict(
            &held_under(&seen.text, &["/Users//owner".to_string()]),
            &wrote,
            &[7],
            seen.complete,
        )
    };
    let clean: &[&str] = &["/private/tmp/owned/home/.config/muse/session"];
    assert_eq!(judge(Ok(clean)), Verdict::Ok);
    let held: &[&str] = &["/Users//owner/.config/muse/auth.json"];
    assert_eq!(judge(Ok(held)), Verdict::Fail);
    for kind in [
        std::io::ErrorKind::NotFound,
        std::io::ErrorKind::PermissionDenied,
        std::io::ErrorKind::Other,
    ] {
        assert_eq!(judge(Err(kind.into())), Verdict::Unproven, "{kind:?}");
    }
}

/// THE RUNTIME CHECK FAILS ON ANYTHING LEFT of what the tab's Muse Code
/// made in the shared runtime directory, holds when its lease, socket and
/// session record were all seen and none is left, and is UNPROVEN when the
/// witness saw fewer (a Muse Code that made none, or a registry of another
/// shape).
#[test]
fn the_runtime_check_fails_on_a_leftover_and_needs_all_three_seen() {
    let r = "/private/tmp/tbh-501-rt/muse";
    let made: Vec<String> = ["ms-aa.sock", "ms-aa.sock.lease", "sessions/01a1.json"]
        .iter()
        .map(|n| format!("{r}/{n}"))
        .collect();
    assert_eq!(runtime_verdict(&made, &[], true), Verdict::Ok);
    assert_eq!(runtime_verdict(&made, &made[1..2], true), Verdict::Fail);
    assert_eq!(runtime_verdict(&[], &made[..1], true), Verdict::Fail);
    assert_eq!(runtime_verdict(&made[1..], &[], true), Verdict::Unproven);
    assert_eq!(runtime_verdict(&made[..2], &[], true), Verdict::Unproven);
    assert_eq!(
        runtime_verdict(&[], &[], true),
        Verdict::Unproven,
        "the control"
    );
}
