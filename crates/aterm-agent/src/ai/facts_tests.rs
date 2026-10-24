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
    std::fs::remove_file(path(OTHER)).expect("rm");
    std::fs::remove_file(path(SIDE)).expect("rm");
    write(ROOT, limited(now - 100), 600);
    assert_eq!(limit(&b), None);
}

#[test]
fn claude_accounts_surfaces_next_sign_in_and_prefers_ready_enrolled_row() {
    use crate::harness::{accounts, upgrade};
    let b = machine(&["claude"]);
    let now = 1_800_000_000_i64;
    let h_state = b.dir.path().join("harness-state");
    let store = accounts::Store::at(&h_state);
    let m27_dir = store.login_dir("m27").expect("m27 dir");
    std::fs::create_dir_all(&m27_dir).expect("mkdir m27");
    let roster_path = b.dir.path().join("accounts.toml");
    let plain_dir = store.login_dir("plain").expect("plain dir");
    std::fs::create_dir_all(&plain_dir).expect("mkdir plain");
    let roster_toml = format!(
        "[[account]]\nlabel = \"m27-3\"\nclass = \"dedicated\"\nemail = \"m27-3@example.test\"\n\n\
         [[account]]\nlabel = \"plain\"\nclass = \"dedicated\"\ndir = {:?}\n\n\
         [[account]]\nlabel = \"m27\"\nclass = \"dedicated\"\ndir = {:?}\nemail = \"m27@example.test\"\n",
        plain_dir.display().to_string(),
        m27_dir.display().to_string()
    );
    std::fs::write(&roster_path, &roster_toml).expect("roster");
    let roster = accounts::Roster::load(&roster_path).expect("load roster");
    let mut note = None;
    store
        .transact_logins(now, &mut note, |state, logins| {
            accounts::note_served_login(logins, "m27-3", now - 60);
            Ok(accounts::note_auth(
                state,
                "m27-3",
                accounts::Auth::Ok,
                now - 60,
            ))
        })
        .expect("transact logins");
    let wall = upgrade::StandingWall {
        at: u64::try_from(now).unwrap(),
        until: Some(u64::try_from(now + 86_400).unwrap()),
        window: Some("seven_day".to_owned()),
        credits: None,
    };
    let who = accounts::ProcessLogin {
        login: None,
        default_home: true,
        stored_login: true,
    };
    let _ = accounts::record_wall(&store, &roster, "s-test", &who, &wall, now - 60, now);

    let home_str = b.home.display().to_string();
    let var = |k: &str| (k == "HOME").then(|| home_str.clone());
    let m = Machine {
        now,
        home: Some(b.home.clone()),
        var: &var,
        pkg: Some(b.pkg.clone()),
        exclude: Vec::new(),
        records: Records::default(),
        harness_state: Some(h_state.clone()),
        roster: Some(roster_path.clone()),
    };

    let f = gather(&m, &CLAUDE);
    let view = f.accounts.expect("accounts view");
    assert_eq!(view.label, "m27-3");
    assert!(view.limited_until.is_some_and(|u| u >= now + 86_400));
    assert_eq!(view.next_sign_in.as_deref(), Some("m27"));

    std::fs::write(m27_dir.join(".credentials.json"), "{}").expect("enroll m27");
    let f_after = gather(&m, &CLAUDE);
    let view_after = f_after.accounts.expect("accounts view after enroll");
    assert_eq!(view_after.label, "m27");
    assert_eq!(view_after.limited_until, None);
    assert_eq!(view_after.next_sign_in, None);
    assert!(view_after.made);

    // Also verify when all rows have explicit `dir` and the serving row is SignedOut.
    let m27_3_dir = store.login_dir("m27-3").expect("m27-3 dir");
    std::fs::create_dir_all(&m27_3_dir).expect("mkdir m27-3");
    let all_dirs_toml = format!(
        "[[account]]\nlabel = \"m27\"\nclass = \"dedicated\"\ndir = {:?}\nemail = \"m27@example.test\"\n\n\
         [[account]]\nlabel = \"m27-3\"\nclass = \"dedicated\"\ndir = {:?}\nemail = \"m27-3@example.test\"\n",
        m27_dir.display().to_string(),
        m27_3_dir.display().to_string()
    );
    std::fs::write(&roster_path, &all_dirs_toml).expect("all-dirs roster");
    store
        .transact_logins(now + 10, &mut note, |state, _| {
            Ok(accounts::note_auth(
                state,
                "m27",
                accounts::Auth::SignedOut,
                now + 10,
            ))
        })
        .expect("mark m27 signed out");
    let f_signed_out = gather(&m, &CLAUDE);
    let view_signed_out = f_signed_out
        .accounts
        .expect("accounts view with all explicit dirs");
    assert_eq!(view_signed_out.label, "m27");
    assert_eq!(
        view_signed_out.next_sign_in.as_deref(),
        Some("m27"),
        "when m27 is signed out and m27-3 is limited, next_sign_in falls back to m27 itself"
    );

    // Also verify when all rows have explicit `dir` and `m27` is NotEnrolled (no `.credentials.json`).
    std::fs::remove_file(m27_dir.join(".credentials.json")).expect("unenroll m27");
    let f_unenrolled = gather(&m, &CLAUDE);
    let view_unenrolled = f_unenrolled
        .accounts
        .expect("accounts view with unenrolled explicit dir");
    assert_eq!(view_unenrolled.label, "m27");
    assert!(!view_unenrolled.made);
    assert_eq!(
        view_unenrolled.next_sign_in.as_deref(),
        Some("m27"),
        "when m27 is NotEnrolled and m27-3 is limited, next_sign_in falls back to m27"
    );
}

#[test]
fn a_clis_own_roster_beside_accounts_toml_is_its_pool_and_names_the_row_to_sign_in() {
    use crate::ai::connector::KIMI;
    let b = machine(&["kimi"]);
    let var = |_: &str| -> Option<String> { None };
    let config = b.dir.path().join("config");
    std::fs::create_dir_all(&config).expect("config");
    let roster = config.join("accounts.toml");
    let m = |roster: Option<PathBuf>| Machine {
        now: 1_800_000_000,
        home: Some(b.home.clone()),
        var: &var,
        pkg: Some(b.pkg.clone()),
        exclude: Vec::new(),
        records: Records::default(),
        harness_state: Some(b.state.clone()),
        roster,
    };
    assert_eq!(
        gather(&m(Some(roster.clone())), &KIMI).pool,
        None,
        "no kimi roster"
    );
    std::fs::write(
        config.join("accounts-kimi.toml"),
        "[[account]]\nlabel = \"plain\"\nclass = \"dedicated\"\ndir = \"/logins/kimi/plain\"\n\n\
         [[account]]\nlabel = \"kimi-one\"\nclass = \"dedicated\"\n\
         dir = \"/logins/kimi/kimi-one\"\nemail = \"kimi-one@example.com\"\n\
         region = \"global\"\n",
    )
    .expect("roster");
    let pool = gather(&m(Some(roster.clone())), &KIMI)
        .pool
        .expect("kimi's pool");
    assert_eq!(
        pool.next.as_deref(),
        Some("kimi-one"),
        "the row with an email"
    );
    assert_eq!(pool.refused, None);
    assert_eq!(
        (pool.rows, pool.enrolled, pool.due.as_slice()),
        (2, 0, &[] as &[String]),
        "two rows, neither login made here, none due"
    );
    let made = b.home.join(".aterm/logins/kimi/kimi-one");
    std::fs::create_dir_all(made.join("credentials")).expect("login dir");
    std::fs::write(made.join("credentials/kimi-code.json"), "{}").expect("placeholder login");
    std::fs::write(
        config.join("accounts-kimi.toml"),
        format!(
            "[[account]]\nlabel = \"kimi-one\"\nclass = \"dedicated\"\ndir = \"{}\"\n\
             email = \"kimi-one@example.com\"\nregion = \"global\"\n",
            made.display()
        ),
    )
    .expect("roster");
    let pool = gather(&m(Some(roster.clone())), &KIMI)
        .pool
        .expect("kimi's pool");
    assert_eq!(
        (pool.rows, pool.enrolled),
        (1, 1),
        "a row is enrolled once its login file exists"
    );
    assert_eq!(gather(&m(None), &KIMI).pool, None, "no config, no pool");
    assert_eq!(
        gather(&m(Some(roster)), &CLAUDE).pool,
        None,
        "Claude Code's pool is its accounts view"
    );
}
