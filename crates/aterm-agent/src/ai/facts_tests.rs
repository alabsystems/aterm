// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::ai::connector::{CLAUDE, CODEX, GROK};
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

/// AN OPT-IN PROGRAM NOBODY ASKED FOR IS KEPT OFF: Grok Build, outside atpkg's default set, is
/// off while neither installed nor found on `PATH`, where a default-set program's absence is an
/// install owed; installed, or a copy aterm does not manage found, it is read as any other.
#[test]
fn an_opt_in_program_is_kept_off_until_installed_or_found() {
    if !atpkg::vendor_direct::served_here(GROK.id) {
        let b = machine(&[]);
        assert_eq!(
            gather_with(&b, &[], &[], &GROK).kept,
            Some(Kept::Unavailable)
        );
        return;
    }
    let b = machine(&[]);
    assert_eq!(gather_with(&b, &[], &[], &GROK).kept, Some(Kept::OptIn));
    assert_eq!(
        gather_with(&b, &[], &[], &CODEX).kept,
        None,
        "a default-set program owes its install"
    );
    let bin = b.home.join("bin");
    std::fs::create_dir_all(&bin).expect("bin");
    std::fs::write(bin.join("grok"), "#!/bin/sh\n").expect("a copy");
    let path = bin.display().to_string();
    let found = gather_with(&b, &[("PATH", path.as_str())], &[], &GROK);
    assert_eq!(
        (found.kept, found.foreign),
        (None, Some(bin.join("grok"))),
        "a copy aterm does not manage"
    );
    let b = machine(&["grok"]);
    let installed = gather_with(&b, &[], &[], &GROK);
    assert_eq!(installed.kept, None);
    assert!(installed.managed.is_some());
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
    let roster = store
        .read_usage_roster(Some(&roster_path), now)
        .expect("load roster");
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

#[test]
fn unreadable_account_state_or_bindings_yield_no_actionable_facts() {
    use crate::harness::accounts::{Cli, Store};
    for cli in [Cli::Claude, Cli::Kimi] {
        for damaged in ["state", "logins"] {
            let b = machine(&[]);
            let config = b.dir.path().join("config");
            std::fs::create_dir_all(&config).unwrap();
            let path = config.join(cli.accounts().roster_file);
            std::fs::write(&path,
                "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\nemail = \"main@example.test\"\n"
            ).unwrap();
            let store = Store::at_for(&b.state, cli, &b.home);
            store.ensure().unwrap();
            let path = if damaged == "state" {
                store.state_file()
            } else {
                store.logins_file()
            };
            std::fs::write(&path, "{").unwrap();
            let var = |_: &str| None;
            let m = Machine {
                now: 1_800_000_000,
                home: Some(b.home.clone()),
                var: &var,
                pkg: Some(b.pkg.clone()),
                exclude: Vec::new(),
                records: Records::default(),
                harness_state: Some(b.state.clone()),
                roster: Some(config.join("accounts.toml")),
            };
            if cli == Cli::Claude {
                let view = claude_accounts(&m).unwrap();
                assert!(view.refused.is_some());
                assert!(view.login.is_none());
                assert!(!view.made && !view.renews);
                assert!(view.next_sign_in.is_none());
            } else {
                let view = pool_for(&m, cli).unwrap();
                assert!(view.refused.is_some());
                assert_eq!(view.rows, 0);
                assert_eq!(view.enrolled, 0);
                assert!(view.next.is_none() && view.due.is_empty());
            }
            assert_eq!(std::fs::read(path).unwrap(), b"{");
        }
    }
}

#[test]
fn pooled_codex_serves_enrolled_staging_login_without_default_auth_and_rotates_past_limit() {
    use crate::ai::state::{Mark, State, assess};
    use crate::harness::accounts::{self, Cli, Store};
    use crate::harness::codex_rollout::fixtures::{meta_as, rate};

    const ROOT: &str = "01900000-0000-7000-8000-000000000011";
    let b = machine(&["codex"]);
    let now = 1_800_000_000_i64;
    let config = b.dir.path().join("config");
    std::fs::create_dir_all(&config).expect("config");
    let claude_roster = config.join("accounts.toml");
    let codex_roster = config.join("accounts-codex.toml");
    let work_1_base = b.home.join(".aterm/logins/codex/work-1");
    let work_2_base = b.home.join(".aterm/logins/codex/work-2");
    std::fs::write(
        &codex_roster,
        format!(
            "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n\
             [[account]]\nlabel = \"work-1\"\nclass = \"dedicated\"\ndir = {:?}\nemail = \"work-1@example.test\"\n\n\
             [[account]]\nlabel = \"work-2\"\nclass = \"dedicated\"\ndir = {:?}\nemail = \"work-2@example.test\"\n",
            work_1_base.display().to_string(),
            work_2_base.display().to_string(),
        ),
    )
    .expect("codex roster");

    let home_str = b.home.display().to_string();
    let var = |k: &str| (k == "HOME").then(|| home_str.clone());
    let m = Machine {
        now,
        home: Some(b.home.clone()),
        var: &var,
        pkg: Some(b.pkg.clone()),
        exclude: Vec::new(),
        records: Records::default(),
        harness_state: Some(b.state.clone()),
        roster: Some(claude_roster),
    };

    // 1. Before any sign-in: default ~/.codex/auth.json does not exist either, so 0 enrolled.
    let f0 = gather(&m, &CODEX);
    let pool0 = f0.pool.as_ref().expect("codex pool");
    assert_eq!((pool0.rows, pool0.enrolled), (3, 0));
    assert_eq!(pool0.next.as_deref(), Some("work-1"));
    assert_eq!(f0.login_file, None);
    let r0 = assess(&f0, true, now);
    assert_eq!((r0.state, r0.mark), (State::SignedOut, Mark::Fail));
    assert!(
        r0.fact.contains("pool: 3 accounts (0 signed in here)"),
        "{}",
        r0.fact
    );

    // 2. Renew work-1 into staging work-1+1/auth.json while ~/.codex/auth.json still never existed.
    let store = Store::at_for(&b.state, Cli::Codex, &b.home);
    let staging_1 = b.home.join(".aterm/logins/codex/work-1+1");
    std::fs::create_dir_all(&staging_1).expect("staging_1");
    std::fs::write(staging_1.join("auth.json"), "\u{0}opaque-token").expect("auth 1");
    let mut note = None;
    store
        .transact_logins(now, &mut note, |state, logins| {
            Ok(accounts::note_renewed(
                state,
                logins,
                "work-1",
                Some(&work_1_base.display().to_string()),
                &staging_1.display().to_string(),
                None,
                now - 60,
            ))
        })
        .expect("renew work-1");

    let f1 = gather(&m, &CODEX);
    let pool1 = f1.pool.as_ref().expect("codex pool after renew");
    let auth_1 = staging_1.join("auth.json").display().to_string();
    assert_eq!(pool1.enrolled, 1);
    assert_eq!(
        pool1.serving_login,
        Some(("CODEX_AUTH_PATH", Some(auth_1.clone())))
    );
    assert_eq!(
        f1.login_file.as_ref().map(|(p, _)| p.as_path()),
        Some(staging_1.join("auth.json").as_path())
    );
    let r1 = assess(&f1, true, now);
    assert_eq!((r1.state, r1.mark), (State::Ok, Mark::Ok), "{}", r1.fact);

    // 3. work-1 hits a usage wall while work-2 is still unenrolled -> Due + next_sign_in = work-2.
    store
        .transact_logins(now, &mut note, |state, _| {
            Ok(accounts::note_wall(
                state,
                "work-1",
                "seven_day",
                Some(now + 3600),
                now,
                now,
            ))
        })
        .expect("wall on work-1");
    let f2 = gather(&m, &CODEX);
    let r2 = assess(&f2, true, now);
    assert!(matches!(r2.state, State::Due(until) if until >= now + 3600));
    assert_eq!(r2.mark, Mark::Warn);
    assert!(
        r2.fact.contains("next account `work-2` needs sign-in"),
        "{}",
        r2.fact
    );

    // 4. Enroll work-2 into work-2+1/auth.json while shared ~/.codex/sessions holds work-1's 100%
    //    rollout -> serving row rotates to work-2 and is Ok (not blocked by shared rollout).
    let day = b.home.join(".codex/sessions/2026/12/15");
    std::fs::create_dir_all(&day).expect("day");
    let rollout_body = [
        meta_as(
            (now - 300) * 1000,
            ROOT,
            ROOT,
            None,
            Some("paginated"),
            Some(("acct-work-1", "user-work-1")),
        ),
        rate(
            (now - 120) * 1000,
            "codex",
            Some((100.0, 300, now + 3600)),
            None,
        ),
    ]
    .concat();
    std::fs::write(
        day.join(format!("rollout-2026-12-15T10-00-00-{ROOT}.jsonl")),
        rollout_body,
    )
    .expect("rollout");
    let staging_2 = b.home.join(".aterm/logins/codex/work-2+1");
    std::fs::create_dir_all(&staging_2).expect("staging_2");
    std::fs::write(staging_2.join("auth.json"), "\u{0}opaque-token-2").expect("auth 2");
    store
        .transact_logins(now, &mut note, |state, logins| {
            Ok(accounts::note_renewed(
                state,
                logins,
                "work-2",
                Some(&work_2_base.display().to_string()),
                &staging_2.display().to_string(),
                None,
                now - 10,
            ))
        })
        .expect("renew work-2");

    let f3 = gather(&m, &CODEX);
    let pool3 = f3.pool.as_ref().expect("codex pool with work-2");
    assert_eq!(pool3.enrolled, 2);
    assert_eq!(
        pool3.serving_login,
        Some((
            "CODEX_AUTH_PATH",
            Some(staging_2.join("auth.json").display().to_string())
        ))
    );
    assert_eq!(f3.limited_until, None);
    let r3 = assess(&f3, true, now);
    assert_eq!((r3.state, r3.mark), (State::Ok, Mark::Ok), "{}", r3.fact);

    // 5. With [harness] accounts = "off" and no pin, pool overrides nothing.
    std::fs::write(config.join("aterm.toml"), "[harness]\naccounts = \"off\"\n").expect("cfg");
    let f4 = gather(&m, &CODEX);
    assert_eq!(f4.pool.as_ref().and_then(|p| p.serving_login.clone()), None);
    assert_eq!(f4.login_file, None);

    // 6. A custom CODEX_HOME inspects only that home and leaves the default-home pool aside.
    std::fs::remove_file(config.join("aterm.toml")).expect("rm cfg");
    let custom_home = b.dir.path().join("custom-codex");
    let custom_str = custom_home.display().to_string();
    let var_custom = |k: &str| match k {
        "HOME" => Some(home_str.clone()),
        "CODEX_HOME" => Some(custom_str.clone()),
        _ => None,
    };
    let m_custom = Machine {
        now,
        home: m.home.clone(),
        var: &var_custom,
        pkg: m.pkg.clone(),
        exclude: m.exclude.clone(),
        records: m.records.clone(),
        harness_state: m.harness_state.clone(),
        roster: m.roster.clone(),
    };
    let f5 = gather(&m_custom, &CODEX);
    assert_eq!(f5.pool, None);
    assert_eq!(f5.home, Ok(custom_home));

    // 7. When a Shared row precedes an unenrolled Dedicated successor, both next_sign_in and next
    //    name the Dedicated successor so `aterm ai login codex` renews what `assess` asked for.
    let shared_base = b.home.join(".aterm/logins/codex/shared-1");
    let ded_base = b.home.join(".aterm/logins/codex/ded-1");
    std::fs::write(
        &codex_roster,
        format!(
            "[[account]]\nlabel = \"shared-1\"\nclass = \"shared\"\ndir = {:?}\nemail = \"shared-1@example.test\"\n\n\
             [[account]]\nlabel = \"work-1\"\nclass = \"dedicated\"\ndir = {:?}\nemail = \"work-1@example.test\"\n\n\
             [[account]]\nlabel = \"ded-1\"\nclass = \"dedicated\"\ndir = {:?}\nemail = \"ded-1@example.test\"\n",
            shared_base.display().to_string(),
            work_1_base.display().to_string(),
            ded_base.display().to_string(),
        ),
    )
    .expect("codex class roster");
    let f6 = gather(&m, &CODEX);
    let pool6 = f6.pool.as_ref().expect("class pool");
    assert_eq!(pool6.next_sign_in.as_deref(), Some("ded-1"));
    assert_eq!(pool6.next.as_deref(), Some("ded-1"));
}
