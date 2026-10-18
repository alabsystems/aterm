// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::ai::connector::{CLAUDE, CODEX};
use crate::ai::testing::machine;

fn gather_with(
    b: &crate::ai::testing::Box3,
    vars: &[(&str, &str)],
    exclude: &[&str],
    c: &'static Connector,
) -> Facts {
    let home = b.home.display().to_string();
    let mut all: Vec<(String, String)> = vec![("HOME".into(), home)];
    all.extend(
        vars.iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string())),
    );
    let var = move |k: &str| all.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let m = Machine {
        now: 1_800_000_000,
        home: Some(b.home.clone()),
        var: &var,
        pkg: Some(b.pkg.clone()),
        exclude: exclude.iter().map(|s| (*s).to_string()).collect(),
        records: Records::default(),
        harness_state: None,
        roster: None,
    };
    gather(&m, c)
}

#[test]
fn codex_is_read_from_its_login_file_by_stat_alone() {
    let b = machine(&["codex"]);
    let f = gather_with(&b, &[], &[], &CODEX);
    assert!(f.managed.is_some());
    assert!(!f.home_exists && f.login_file.is_none());
    std::fs::create_dir_all(b.home.join(".codex")).expect("home");
    // Not JSON at all: aterm never opens it, so its content cannot matter.
    std::fs::write(b.home.join(".codex/auth.json"), "\u{0}\u{1}not json").expect("auth");
    let f = gather_with(&b, &[], &[], &CODEX);
    assert!(f.home_exists);
    let (path, written) = f.login_file.expect("seen");
    assert!(path.ends_with(".codex/auth.json"));
    assert!(written.is_some());
}

#[test]
fn a_keyring_store_in_codexs_config_hides_its_login() {
    let b = machine(&["codex"]);
    std::fs::create_dir_all(b.home.join(".codex")).expect("home");
    std::fs::write(
        b.home.join(".codex/config.toml"),
        "cli_auth_credentials_store = \"keyring\"\n",
    )
    .expect("cfg");
    assert!(gather_with(&b, &[], &[], &CODEX).hidden_store);
    std::fs::write(
        b.home.join(".codex/config.toml"),
        "cli_auth_credentials_store = \"file\"\n",
    )
    .expect("cfg");
    assert!(!gather_with(&b, &[], &[], &CODEX).hidden_store);
}

#[test]
fn the_home_variable_moves_the_home_and_a_relative_one_is_not_placed() {
    let b = machine(&["codex"]);
    let elsewhere = b.dir.path().join("codex-home");
    let f = gather_with(
        &b,
        &[("CODEX_HOME", elsewhere.to_str().expect("utf8"))],
        &[],
        &CODEX,
    );
    assert_eq!(f.home, Ok(elsewhere));
    let f = gather_with(&b, &[("CODEX_HOME", "rel/codex")], &[], &CODEX);
    assert_eq!(f.home, Err("rel/codex".to_string()));
    let f = gather_with(&b, &[("CODEX_HOME", "")], &[], &CODEX);
    assert_eq!(
        f.home,
        Ok(b.home.join(".codex")),
        "an empty variable is unset"
    );
}

#[test]
fn exclude_and_credential_variables_are_read() {
    let b = machine(&["codex"]);
    let f = gather_with(&b, &[("OPENAI_API_KEY", "sk-x")], &["codex"], &CODEX);
    assert_eq!(f.kept, Some(Kept::Excluded));
    assert_eq!(f.env_credentials, vec!["OPENAI_API_KEY"]);
}

#[test]
fn a_program_uninstalled_on_purpose_is_kept_off() {
    let b = machine(&[]);
    assert_eq!(gather_with(&b, &[], &[], &CODEX).kept, None);
    std::fs::write(b.pkg.removed(), "codex\n").expect("removed");
    assert_eq!(gather_with(&b, &[], &[], &CODEX).kept, Some(Kept::Removed));
}

#[test]
fn claude_with_no_accounts_store_reads_its_profile() {
    let b = machine(&["claude"]);
    let f = gather_with(&b, &[], &[], &CLAUDE);
    assert!(f.accounts.is_none() && !f.profile_signed_in);
    std::fs::create_dir_all(b.home.join(".claude")).expect("dir");
    std::fs::write(
        b.home.join(".claude.json"),
        "{\"oauthAccount\":{\"accountUuid\":\"00000000-0000-4000-8000-000000000001\",\"emailAddress\":\"x@example.com\"}}",
    )
    .expect("profile");
    let f = gather_with(&b, &[], &[], &CLAUDE);
    assert!(
        f.profile_signed_in,
        "an oauthAccount is a signed-in account"
    );
}

/// CODEX'S LIMIT IS READ BY ITS PICK (`ai::codex_limits::home_pick`): a window at
/// 100% whose reset is ahead names the limit's end, though another thread of the login wrote a
/// smaller share of that window since (a count written again from what it held), and a legacy
/// fork written last copied its parent's 60% under fresh stamps. Negative controls: a login
/// signed in since, whose own window is far from its limit, is the one read; and a 100% window
/// whose reset has passed names no end.
#[test]
fn codexs_limit_is_the_newest_logins_largest_share_past_a_forks_copy() {
    use crate::harness::codex_rollout::fixtures::{meta_as, rate};
    const ROOT: &str = "01900000-0000-7000-8000-000000000001";
    const SIDE: &str = "01900000-0000-7000-8000-000000000002";
    const FORK: &str = "01900000-0000-7000-8000-000000000003";
    const OTHER: &str = "01900000-0000-7000-8000-000000000004";
    let b = machine(&["codex"]);
    let now = 1_800_000_000_i64;
    let ms = now * 1000;
    let day = b.home.join(".codex/sessions/2026/12/15");
    std::fs::create_dir_all(&day).expect("day");
    let path = |thread: &str| day.join(format!("rollout-2026-12-15T10-00-00-{thread}.jsonl"));
    let write = |thread: &str, body: String, age_s: u64| {
        std::fs::write(path(thread), body).expect("write");
        std::fs::File::options()
            .append(true)
            .open(path(thread))
            .and_then(|f| {
                f.set_modified(
                    std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_800_000_000 - age_s),
                )
            })
            .expect("mtime");
    };
    let a = Some(("acct-stand-in-1", "user-stand-in"));
    let reset = now + 3600;
    let limited = |reset: i64| {
        [
            meta_as(ms - 7_200_000, ROOT, ROOT, None, Some("paginated"), a),
            rate(ms - 1_800_000, "codex", Some((97.0, 300, reset)), None),
            rate(ms - 900_000, "codex", Some((100.0, 300, reset + 20)), None),
        ]
        .concat()
    };
    write(ROOT, limited(reset), 600);
    write(
        SIDE,
        [
            meta_as(ms - 3_600_000, SIDE, SIDE, None, Some("paginated"), a),
            rate(ms - 300_000, "codex", Some((99.0, 300, reset + 40)), None),
        ]
        .concat(),
        300,
    );
    write(
        FORK,
        [
            meta_as(ms - 60_000, FORK, FORK, Some(ROOT), Some("legacy"), a),
            rate(ms - 60_000, "codex", Some((60.0, 300, reset + 40)), None),
        ]
        .concat(),
        60,
    );
    let limit = |b: &crate::ai::testing::Box3| gather_with(b, &[], &[], &CODEX).limited_until;
    assert_eq!(limit(&b), Some(reset + 40), "the window's anchor");
    // NEGATIVE CONTROL 1: another login's thread, written last.
    let other = [
        meta_as(
            ms - 30_000,
            OTHER,
            OTHER,
            None,
            Some("paginated"),
            Some(("acct-stand-in-2", "user-stand-in")),
        ),
        rate(ms - 20_000, "codex", Some((10.0, 300, now + 9000)), None),
    ]
    .concat();
    write(OTHER, other, 20);
    assert_eq!(limit(&b), None);
    // NEGATIVE CONTROL 2: the 100% window's reset passed.
    std::fs::remove_file(path(OTHER)).expect("rm");
    std::fs::remove_file(path(SIDE)).expect("rm");
    write(ROOT, limited(now - 100), 600);
    assert_eq!(limit(&b), None);
}
