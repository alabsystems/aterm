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
        accounts_mode: None,
        config: None,
    };
    gather(&m, c)
}

#[test]
fn a_linked_codex_names_its_package_without_reusing_a_stale_installed_version() {
    const OWN: &str = "0.1.0-dev.13917+codex.0.165.0.g1aef4a3bf";
    const CLIENT: &str = "0.166.0-dev.13917+g1aef4a3bf";
    let b = machine(&[]);
    let checkout = b.dir.path().join("codex");
    let rel = PathBuf::from("bin").join(format!("codex{}", std::env::consts::EXE_SUFFIX));
    let exe = checkout.join(&rel);
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, b"package bytes, never executed").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::write(
        checkout.join("codex-package.json"),
        format!(
            r#"{{"layoutVersion":1,"entrypoint":"{}","version":"{OWN}"}}"#,
            rel.to_string_lossy().replace('\\', "/")
        ),
    )
    .unwrap();
    std::fs::write(
        checkout.join("alab-build.json"),
        format!(r#"{{"version":"{OWN}","client_version":"{CLIENT}"}}"#),
    )
    .unwrap();
    atpkg::link(&b.pkg, "codex", &checkout, &[rel]).unwrap();
    let mut status = atpkg::Status::default();
    status.programs.insert(
        "codex".into(),
        atpkg::ProgramStatus {
            state: "managed 0.164.0 — stale".into(),
            ..Default::default()
        },
    );
    atpkg::status::write(&b.pkg, &status).unwrap();
    let got = managed_of(&b.pkg, "codex").unwrap();
    assert_eq!(
        got.path,
        b.pkg.shim(&atpkg::store::ToolName::new("codex").unwrap())
    );
    assert_eq!(
        got.version,
        Some(format!("{OWN} (client {CLIENT}; dev-linked)"))
    );
    std::fs::write(
        checkout.join("alab-build.json"),
        r#"{"version":"another build"}"#,
    )
    .unwrap();
    assert_eq!(
        managed_of(&b.pkg, "codex").unwrap().version,
        None,
        "malformed current metadata cannot inherit the old installed version"
    );
    assert_eq!(
        atpkg::status::read(&b.pkg).unwrap(),
        status,
        "inspection leaves historical status intact"
    );
    std::fs::remove_file(&exe).unwrap();
    assert_eq!(managed_of(&b.pkg, "codex").and_then(|m| m.version), None);

    let installed = machine(&["claude"]);
    status.programs.insert(
        "claude".into(),
        atpkg::ProgramStatus {
            state: atpkg::state::vendor_managed("2.1.312", "Anthropic"),
            ..Default::default()
        },
    );
    atpkg::status::write(&installed.pkg, &status).unwrap();
    assert_eq!(
        managed_of(&installed.pkg, "claude")
            .unwrap()
            .version
            .as_deref(),
        Some("2.1.312")
    );
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

/// GROK BUILD AND KIMI CODE ARE IN ATPKG'S DEFAULT SET: absent, Grok Build's is an install the
/// machine owes, as Codex's is, and only a person keeps it off. CONTROLS: `[packages] exclude`
/// keeps it off as excluded, and an uninstall on purpose as removed; installed, or a copy aterm
/// does not manage found, it is read as any other.
#[test]
fn grok_owes_its_install_unless_a_person_keeps_it_off() {
    if !atpkg::vendor_direct::served_here(GROK.id) {
        let b = machine(&[]);
        assert_eq!(
            gather_with(&b, &[], &[], &GROK).kept,
            Some(Kept::Unavailable)
        );
        return;
    }
    let b = machine(&[]);
    for c in [&GROK, &CODEX] {
        let f = gather_with(&b, &[], &[], c);
        assert_eq!(
            (f.kept, f.managed),
            (None, None),
            "{} owes its install",
            c.id
        );
    }
    assert_eq!(
        gather_with(&b, &[], &["grok"], &GROK).kept,
        Some(Kept::Excluded),
        "CONTROL: excluded"
    );
    std::fs::write(b.pkg.removed(), "grok\n").expect("removed");
    assert_eq!(
        gather_with(&b, &[], &[], &GROK).kept,
        Some(Kept::Removed),
        "CONTROL: uninstalled on purpose"
    );
    let b = machine(&[]);
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
        accounts_mode: None,
        config: None,
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
        accounts_mode: None,
        config: None,
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

/// A Grok row with no email is still the one `aterm ai login grok` signs in, since `accounts
/// login` needs none (its renewals stop at accounts.x.ai's block page). CONTROL: a Kimi roster
/// of the same email-less row names none, since only a renewal by email signs one in.
#[test]
fn a_grok_pool_with_no_email_still_names_its_next_row() {
    use crate::ai::connector::KIMI;
    let b = machine(&["grok", "kimi"]);
    let var = |_: &str| -> Option<String> { None };
    let config = b.dir.path().join("config");
    std::fs::create_dir_all(&config).expect("config");
    let m = Machine {
        now: 1_800_000_000,
        home: Some(b.home.clone()),
        var: &var,
        pkg: Some(b.pkg.clone()),
        exclude: Vec::new(),
        records: Records::default(),
        harness_state: Some(b.state.clone()),
        roster: Some(config.join("accounts.toml")),
        accounts_mode: None,
        config: None,
    };
    let rows = "[[account]]\nlabel = \"plain\"\nclass = \"dedicated\"\ndir = \"/logins/plain\"\n";
    std::fs::write(config.join("accounts-grok.toml"), rows).expect("grok roster");
    std::fs::write(config.join("accounts-kimi.toml"), rows).expect("kimi roster");
    let grok = gather(&m, &GROK).pool.expect("grok's pool");
    assert_eq!(grok.next.as_deref(), Some("plain"));
    let kimi = gather(&m, &KIMI).pool.expect("kimi's pool");
    assert_eq!(kimi.refused, None);
    assert_eq!(kimi.next, None, "CONTROL");
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
                accounts_mode: None,
                config: None,
            };
            if cli == Cli::Claude {
                let view = claude_accounts(&m).unwrap();
                assert_eq!(view.refused, None, "accounts.toml itself is admitted");
                let (file, dir) = (
                    path.file_name().expect("a file").to_string_lossy(),
                    path.parent().expect("its folder").display().to_string(),
                );
                assert!(
                    view.unread
                        .as_deref()
                        .is_some_and(|why| why.starts_with(&*file) && why.ends_with(&dir)),
                    "{:?}",
                    view.unread
                );
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
    alab_launcher(&b, true);
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
        accounts_mode: None,
        config: None,
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

    // 5. With [harness] accounts = "off" and no pin, a launch takes no row and runs on this
    //    machine's own login, so the pool serves `main`, its row, whose run unsets
    //    CODEX_AUTH_PATH; the default home holds no login file.
    std::fs::write(config.join("aterm.toml"), "[harness]\naccounts = \"off\"\n").expect("cfg");
    let f4 = gather(&m, &CODEX);
    let pool4 = f4.pool.as_ref().expect("codex pool under rotation off");
    assert_eq!(pool4.serving_label.as_deref(), Some("main"));
    assert_eq!(pool4.serving_login, Some(("CODEX_AUTH_PATH", None)));
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
        accounts_mode: None,
        config: None,
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

/// This Mac on 2026-10-27: the roster's default row m2-2 (no `dir`) beside m2 (a `dir` with no
/// login in it), m2-2 on record as signed out two days ago. `aterm ai` says what accounts v2
/// recorded and when, never a wall it did not see; once the default login's heal records the
/// login made (`note_default_login_made`), claude reads signed in on m2-2. Negative control:
/// before the heal the row is signed out.
#[test]
fn claude_on_a_default_row_says_when_it_was_recorded_signed_out_and_signed_in_once_healed() {
    use crate::ai::state::{State, assess};
    use crate::harness::accounts;
    let b = machine(&["claude"]);
    let now = 1_800_000_000_i64;
    let h_state = b.dir.path().join("harness-state");
    let store = accounts::Store::at(&h_state);
    let m2_dir = store.login_dir("m2").expect("m2 dir");
    std::fs::create_dir_all(&m2_dir).expect("mkdir m2");
    std::fs::create_dir_all(b.home.join(".claude")).expect("claude's home");
    std::fs::write(
        b.home.join(".claude.json"),
        "{\"oauthAccount\":{\"accountUuid\":\"00000000-0000-4000-8000-000000000002\",\
         \"emailAddress\":\"proton.m2@example.test\"}}\n",
    )
    .expect("profile");
    let roster_path = b.dir.path().join("accounts.toml");
    std::fs::write(
        &roster_path,
        format!(
            "[[account]]\nlabel = \"m2-2\"\nclass = \"dedicated\"\nemail = \"proton.m2@example.test\"\n\n\
             [[account]]\nlabel = \"m2\"\nclass = \"dedicated\"\ndir = {:?}\nemail = \"fb@example.test\"\n",
            m2_dir.display().to_string()
        ),
    )
    .expect("roster");
    let mut note = None;
    store
        .transact_logins(now - 2 * 86_400, &mut note, |state, _| {
            let mut events =
                accounts::note_auth(state, "m2-2", accounts::Auth::SignedOut, now - 2 * 86_400);
            events.extend(accounts::note_session(
                state,
                "s-tab",
                "m2",
                None,
                now - 600,
            ));
            Ok(events)
        })
        .expect("the sign-out");
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
        accounts_mode: None,
        config: None,
    };
    let f = gather(&m, &CLAUDE);
    let view = f.accounts.as_ref().expect("accounts view");
    assert_eq!(view.label, "m2-2");
    assert_eq!(view.state, Some("expired"));
    assert_eq!(view.signed_out_at, Some(now - 2 * 86_400));
    let row = assess(&f, true, now);
    assert_eq!(row.state, State::SignedOut, "{}", row.fact);
    assert!(
        row.fact
            .starts_with("its login has ended (accounts v2 recorded it signed out 2 d ago)"),
        "{}",
        row.fact
    );
    assert!(!row.fact.contains("wall"), "{}", row.fact);

    store
        .transact_logins(now, &mut note, |state, logins| {
            Ok(accounts::note_default_login_made(
                state,
                logins,
                "m2-2",
                Some("m2"),
                now,
            ))
        })
        .expect("the heal");
    let f = gather(&m, &CLAUDE);
    let view = f.accounts.as_ref().expect("accounts view");
    assert_eq!((view.label.as_str(), view.state), ("m2-2", Some("ok")));
    assert_eq!(view.signed_out_at, None);
    let row = assess(&f, true, now);
    assert_eq!(row.state, State::Ok, "{}", row.fact);
    assert!(row.fact.starts_with("signed in"), "{}", row.fact);
}

/// Writes `b`'s atpkg launcher for Codex to exec a codex-alab package laid beside it, one that
/// keeps `CODEX_AUTH_PATH`'s login its own where `reads`.
fn alab_launcher(b: &crate::ai::testing::Box3, reads: bool) {
    let exe = crate::harness::codex_alab::lay_package(&b.dir.path().join("store/codex"), reads);
    std::fs::write(
        b.pkg.agents_dir().join("codex"),
        format!("#!/bin/sh\nexec '{}' \"$@\"\n", exe.display()),
    )
    .expect("launcher");
}

/// ON A MANAGED CODEX THAT KEEPS NO ROW'S LOGIN ITS OWN, A REVIEW SERVES THIS MACHINE'S OWN
/// LOGIN'S ROW ALONE: where the pool would serve `work` past a walled `main`, the facts keep
/// `main` as the serving row, so the review is skipped for `main`'s own limit and names no other
/// row to sign in, and the row's fact says why. The same holds where `work` has no login here
/// yet (as `accounts add` leaves it), so the pool reaches `main` by itself: no row is named next.
/// CONTROL: a build that keeps the row's login serves `work`.
#[test]
fn a_codex_review_on_a_build_that_ignores_codex_auth_path_serves_the_default_row_alone() {
    use crate::ai::state::assess;
    use crate::harness::accounts::{self, Cli, Store};
    use crate::harness::codex_alab::DefaultRow;

    let b = machine(&["codex"]);
    let now = 1_800_000_000_i64;
    let config = b.dir.path().join("config");
    std::fs::create_dir_all(&config).expect("config");
    std::fs::create_dir_all(b.home.join(".codex")).expect("default home");
    std::fs::write(b.home.join(".codex/auth.json"), "{}").expect("default login");
    let work = b.home.join(".aterm/logins/codex/work");
    std::fs::create_dir_all(&work).expect("work");
    std::fs::write(work.join("auth.json"), "{}").expect("work login");
    std::fs::write(
        config.join("accounts-codex.toml"),
        format!(
            "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n\
             [[account]]\nlabel = \"work\"\nclass = \"dedicated\"\ndir = {:?}\nemail = \"work@example.com\"\n",
            work.display().to_string()
        ),
    )
    .expect("codex roster");
    Store::at_for(&b.state, Cli::Codex, &b.home)
        .transact_logins(now, &mut None, |state, _| {
            Ok(accounts::note_wall(
                state,
                "main",
                accounts::SESSION_LIMIT,
                Some(now + 3600),
                now,
                now,
            ))
        })
        .expect("wall `main`");
    let home = b.home.display().to_string();
    let var = |k: &str| (k == "HOME").then(|| home.clone());
    let m = Machine {
        now,
        home: Some(b.home.clone()),
        var: &var,
        pkg: Some(b.pkg.clone()),
        exclude: Vec::new(),
        records: Records::default(),
        harness_state: Some(b.state.clone()),
        roster: Some(config.join("accounts.toml")),
        accounts_mode: None,
        config: None,
    };

    alab_launcher(&b, false);
    let f = gather(&m, &CODEX);
    let pool = f.pool.as_ref().expect("codex pool");
    assert_eq!(pool.serving_label.as_deref(), Some("main"));
    assert_eq!(pool.serving_login, Some(("CODEX_AUTH_PATH", None)));
    assert!(pool.default_only);
    assert!(
        matches!(&pool.default_row, Some(DefaultRow::Walled { label, .. }) if label == "main"),
        "{:?}",
        pool.default_row
    );
    assert_eq!(pool.next_sign_in, None, "no other row is offered");
    assert!(f.limited_until.is_some_and(|until| until >= now + 3600));
    let row = assess(&f, true, now);
    assert!(row.fact.contains("serves `main` alone"), "{}", row.fact);

    std::fs::remove_file(work.join("auth.json")).expect("`work` has no login here yet");
    let f = gather(&m, &CODEX);
    let pool = f.pool.as_ref().expect("codex pool");
    assert_eq!(pool.serving_label.as_deref(), Some("main"));
    assert!(pool.default_only, "{pool:?}");
    assert_eq!(
        (pool.next.as_deref(), pool.next_sign_in.as_deref()),
        (None, None)
    );
    let row = assess(&f, true, now);
    assert!(!row.fact.contains("`work`"), "{}", row.fact);
    assert_eq!(row.fix, None, "{}", row.fact);
    std::fs::write(work.join("auth.json"), "{}").expect("work login");

    alab_launcher(&b, true);
    let f = gather(&m, &CODEX);
    let pool = f.pool.as_ref().expect("codex pool");
    assert_eq!(pool.serving_label.as_deref(), Some("work"), "CONTROL");
    assert!(!pool.default_only);
}

/// ON A MANAGED CODEX THAT KEEPS NO ROW'S LOGIN ITS OWN, A DEFAULT ROW WITH NO CURRENT LOGIN IS
/// SIGNED IN AS ITSELF, NEVER BY SIGNING IN ANOTHER ROW: signed out (and with no email), or with
/// no login file in the default home, `main` is the serving row and the row `aterm ai login`
/// signs in next, by Codex's own sign-in in the default home (the row's fix, `aterm ai login
/// codex`, which the worker's refusal names too), with `work` logged in here or not. Under the
/// owner's pin on `work`, a missing default login is not offered as a pin either, and `main` is
/// still the row signed in next.
#[test]
fn a_codex_review_on_a_build_that_ignores_codex_auth_path_signs_in_only_the_default_row() {
    use crate::ai::state::assess;
    use crate::harness::accounts::{self, Cli, Store};
    use crate::harness::codex_alab::DefaultRow;

    let b = machine(&["codex"]);
    let now = 1_800_000_000_i64;
    let config = b.dir.path().join("config");
    std::fs::create_dir_all(&config).expect("config");
    std::fs::create_dir_all(b.home.join(".codex")).expect("default home");
    std::fs::write(b.home.join(".codex/auth.json"), "{}").expect("default login");
    let work = b.home.join(".aterm/logins/codex/work");
    std::fs::create_dir_all(&work).expect("work");
    std::fs::write(work.join("auth.json"), "{}").expect("work login");
    let roster_path = config.join("accounts-codex.toml");
    std::fs::write(
        &roster_path,
        format!(
            "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n\
             [[account]]\nlabel = \"work\"\nclass = \"dedicated\"\ndir = {:?}\nemail = \"work@example.com\"\n",
            work.display().to_string()
        ),
    )
    .expect("codex roster");
    let store = Store::at_for(&b.state, Cli::Codex, &b.home);
    store
        .transact_logins(now, &mut None, |state, _| {
            Ok(accounts::note_auth(
                state,
                "main",
                accounts::Auth::SignedOut,
                now,
            ))
        })
        .expect("sign `main` out");
    let home = b.home.display().to_string();
    let var = |k: &str| (k == "HOME").then(|| home.clone());
    let m = Machine {
        now,
        home: Some(b.home.clone()),
        var: &var,
        pkg: Some(b.pkg.clone()),
        exclude: Vec::new(),
        records: Records::default(),
        harness_state: Some(b.state.clone()),
        roster: Some(config.join("accounts.toml")),
        accounts_mode: None,
        config: None,
    };
    alab_launcher(&b, false);
    let sign_in = "aterm ai login codex";
    let signed_out = DefaultRow::SignedOut {
        label: "main".into(),
        stored: true,
    };

    let f = gather(&m, &CODEX);
    let pool = f.pool.as_ref().expect("codex pool");
    assert_eq!(pool.serving_label.as_deref(), Some("main"));
    assert!(pool.default_only);
    assert_eq!(pool.default_row.as_ref(), Some(&signed_out));
    assert_eq!(
        pool.next.as_deref(),
        Some("main"),
        "`work` is not what `aterm ai login` signs in"
    );
    let row = assess(&f, true, now);
    assert_eq!(row.fix.as_deref(), Some(sign_in), "{}", row.fact);
    assert!(
        row.fact.contains("serves `main` alone") && row.fact.contains("`main` is signed out"),
        "{}",
        row.fact
    );

    // `work` with no login here yet, as `accounts add` leaves it: the pool reaches `main` by
    // itself, and still signs in `main` alone.
    std::fs::remove_file(work.join("auth.json")).expect("`work` has no login here yet");
    let f = gather(&m, &CODEX);
    let pool = f.pool.as_ref().expect("codex pool");
    assert!(pool.default_only, "{pool:?}");
    assert_eq!(pool.next.as_deref(), Some("main"));
    assert_eq!(assess(&f, true, now).fix.as_deref(), Some(sign_in));
    std::fs::write(work.join("auth.json"), "{}").expect("work login");

    // The default home holds no login file, and nothing on record says `main` signed out.
    store
        .transact_logins(now + 1, &mut None, |state, _| {
            Ok(accounts::note_auth(
                state,
                "main",
                accounts::Auth::Ok,
                now + 1,
            ))
        })
        .expect("clear `main`'s sign-out");
    std::fs::remove_file(b.home.join(".codex/auth.json")).expect("drop the default login");
    let none_here = DefaultRow::SignedOut {
        label: "main".into(),
        stored: false,
    };
    let f = gather(&m, &CODEX);
    let pool = f.pool.as_ref().expect("codex pool");
    assert!(pool.default_only);
    assert_eq!(pool.default_row.as_ref(), Some(&none_here));
    assert_eq!(pool.next.as_deref(), Some("main"));
    assert_eq!(assess(&f, true, now).fix.as_deref(), Some(sign_in));

    // The owner's pin on `work`: the review's guard reads `main` as not signed in, not as a pin.
    let roster = accounts::Roster::load_for(Cli::Codex, &roster_path).expect("roster");
    let enrolled = accounts::enrolled_in(Cli::Codex, &roster, &b.home);
    store
        .transact_within(&accounts::LOCK_LADDER, now + 2, &mut None, |state| {
            accounts::pin(state, &roster, &enrolled, "work", now + 2).map(|event| vec![event])
        })
        .expect("pin `work`");
    let f = gather(&m, &CODEX);
    let pool = f.pool.as_ref().expect("codex pool");
    assert_eq!(pool.serving_label.as_deref(), Some("work"));
    assert!(pool.unread && !pool.default_only, "{pool:?}");
    assert_eq!(pool.default_row.as_ref(), Some(&none_here));
    assert_eq!(pool.next.as_deref(), Some("main"));
}

/// THE CODEX POOL'S FACTS JUDGE THE PROGRAM A LAUNCH RUNS, NOT ATPKG'S MANAGED COPY: with a
/// codex-alab that keeps `CODEX_AUTH_PATH`'s login at atpkg's `bin/codex` (the managed copy) and
/// no twin in `agents/`, a launch types the bare word `codex`, which names no build, so the pool
/// serves `main`, this machine's own login, alone: walled, it is the serving row and a launch is
/// refused; signed out, `aterm ai login codex` signs it in by Codex's own sign-in in its default
/// home. CONTROL: with atpkg's twin in `agents/` execing that codex-alab, the pool serves `work`
/// and no launch is refused.
#[test]
fn a_codex_pool_judges_the_program_a_launch_runs_not_the_managed_copy() {
    use crate::harness::accounts::{self, Cli, Store};
    use crate::harness::codex_alab::DefaultRow;

    let b = machine(&[]);
    let now = 1_800_000_000_i64;
    let exe = crate::harness::codex_alab::lay_package(&b.dir.path().join("store/codex"), true);
    std::fs::create_dir_all(b.pkg.bin_dir()).expect("bin");
    let launcher = format!("#!/bin/sh\nexec '{}' \"$@\"\n", exe.display());
    std::fs::write(b.pkg.bin_dir().join("codex"), &launcher).expect("managed copy");
    let config = b.dir.path().join("config");
    std::fs::create_dir_all(&config).expect("config");
    std::fs::create_dir_all(b.home.join(".codex")).expect("default home");
    std::fs::write(b.home.join(".codex/auth.json"), "{}").expect("default login");
    let work = b.home.join(".aterm/logins/codex/work");
    std::fs::create_dir_all(&work).expect("work");
    std::fs::write(work.join("auth.json"), "{}").expect("work login");
    std::fs::write(
        config.join("accounts-codex.toml"),
        format!(
            "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n\
             [[account]]\nlabel = \"work\"\nclass = \"dedicated\"\ndir = {:?}\nemail = \"work@example.com\"\n",
            work.display().to_string()
        ),
    )
    .expect("codex roster");
    let store = Store::at_for(&b.state, Cli::Codex, &b.home);
    store
        .transact_logins(now, &mut None, |state, _| {
            Ok(accounts::note_wall(
                state,
                "main",
                accounts::SESSION_LIMIT,
                Some(now + 3600),
                now,
                now,
            ))
        })
        .expect("wall `main`");
    let home = b.home.display().to_string();
    let var = |k: &str| (k == "HOME").then(|| home.clone());
    let at = |now: i64| Machine {
        now,
        home: Some(b.home.clone()),
        var: &var,
        pkg: Some(b.pkg.clone()),
        exclude: Vec::new(),
        records: Records::default(),
        harness_state: Some(b.state.clone()),
        roster: Some(config.join("accounts.toml")),
        accounts_mode: None,
        config: None,
    };

    let f = gather(&at(now), &CODEX);
    assert_eq!(
        f.managed.as_ref().map(|managed| managed.path.clone()),
        Some(b.pkg.bin_dir().join("codex")),
        "PRECONDITION: the managed copy is atpkg's bin/codex"
    );
    assert!(crate::harness::codex_alab::auth_path_unread(&b.pkg.bin_dir().join("codex")).is_none());
    let pool = f.pool.as_ref().expect("codex pool");
    assert!(pool.unread && pool.default_only, "{pool:?}");
    assert_eq!(pool.serving_label.as_deref(), Some("main"));
    assert!(pool.launch_refused.is_some(), "{pool:?}");

    // Past its wall, `main` signs out.
    let later = now + 7200;
    store
        .transact_logins(later, &mut None, |state, _| {
            Ok(accounts::note_auth(
                state,
                "main",
                accounts::Auth::SignedOut,
                later,
            ))
        })
        .expect("sign `main` out");
    let f = gather(&at(later), &CODEX);
    let pool = f.pool.as_ref().expect("codex pool");
    assert_eq!(
        pool.default_row,
        Some(DefaultRow::SignedOut {
            label: "main".into(),
            stored: true
        })
    );
    assert_eq!(pool.next.as_deref(), Some("main"));
    assert!(crate::ai::login::default_home_sign_in(pool), "{pool:?}");

    std::fs::write(b.pkg.agents_dir().join("codex"), &launcher).expect("atpkg's twin");
    let f = gather(&at(later), &CODEX);
    let pool = f.pool.as_ref().expect("codex pool");
    assert!(!pool.unread && !pool.default_only, "CONTROL: {pool:?}");
    assert_eq!(pool.serving_label.as_deref(), Some("work"), "CONTROL");
    assert_eq!(pool.launch_refused, None, "CONTROL");
    assert!(!crate::ai::login::default_home_sign_in(pool), "CONTROL");
}

/// A Codex pool for the doctor's judges: `main` (this machine's own login) and `work` (a login of
/// its own), each with an email and a login here, beside a codex-alab that keeps
/// `CODEX_AUTH_PATH`'s login (`reads`) or one from before it; the machine's records, read under
/// rotation `mode`.
struct DoctorPool {
    b: crate::ai::testing::Box3,
    config: PathBuf,
    roster: PathBuf,
    store: crate::harness::accounts::Store,
    mode: Option<crate::harness::accounts::Mode>,
}

impl DoctorPool {
    const NOW: i64 = 1_800_000_000;

    fn new(reads: bool, mode: Option<crate::harness::accounts::Mode>) -> DoctorPool {
        use crate::harness::accounts::{Cli, Store};
        let b = machine(&["codex"]);
        let config = b.dir.path().join("config");
        std::fs::create_dir_all(&config).expect("config");
        std::fs::create_dir_all(b.home.join(".codex")).expect("default home");
        std::fs::write(b.home.join(".codex/auth.json"), "{}").expect("default login");
        let work = b.home.join(".aterm/logins/codex/work");
        std::fs::create_dir_all(&work).expect("work");
        std::fs::write(work.join("auth.json"), "{}").expect("work login");
        let roster = config.join("accounts-codex.toml");
        std::fs::write(
            &roster,
            format!(
                "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\nemail = \"main@example.com\"\n\n\
                 [[account]]\nlabel = \"work\"\nclass = \"dedicated\"\ndir = {:?}\nemail = \"work@example.com\"\n",
                work.display().to_string()
            ),
        )
        .expect("codex roster");
        alab_launcher(&b, reads);
        let store = Store::at_for(&b.state, Cli::Codex, &b.home);
        DoctorPool {
            b,
            config,
            roster,
            store,
            mode,
        }
    }

    /// `change` applied to the pool's records.
    fn state(&self, change: impl FnOnce(&mut crate::harness::accounts::State)) {
        self.store
            .transact_logins(Self::NOW, &mut None, |state, _| {
                change(state);
                Ok(Vec::new())
            })
            .expect("records");
    }

    /// `main` recorded signed out a minute ago, its login file left in place.
    fn sign_out_main(&self) {
        self.state(|state| {
            crate::harness::accounts::note_auth(
                state,
                "main",
                crate::harness::accounts::Auth::SignedOut,
                Self::NOW - 60,
            );
        });
    }

    /// `label` recorded signed out a minute ago, its login file left in place.
    fn sign_out(&self, label: &str) {
        self.state(|state| {
            crate::harness::accounts::note_auth(
                state,
                label,
                crate::harness::accounts::Auth::SignedOut,
                Self::NOW - 60,
            );
        });
    }

    /// The roster of `main` alone, with `email` where one is given.
    fn main_alone(&self, email: Option<&str>) {
        let email = email.map_or_else(String::new, |e| format!("email = \"{e}\"\n"));
        std::fs::write(
            &self.roster,
            format!("[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n{email}"),
        )
        .expect("codex roster");
    }

    /// `main` at its five-hour limit for an hour.
    fn wall_main(&self) {
        self.state(|state| {
            crate::harness::accounts::note_wall(
                state,
                "main",
                crate::harness::accounts::SESSION_LIMIT,
                Some(Self::NOW + 3600),
                Self::NOW,
                Self::NOW,
            );
        });
    }

    /// The owner's pin on `label`, as `accounts use` sets it.
    fn pin(&self, label: &str) {
        use crate::harness::accounts::{self, Cli, Roster};
        let roster = Roster::load_for(Cli::Codex, &self.roster).expect("roster");
        let enrolled = accounts::enrolled_in(Cli::Codex, &roster, &self.b.home);
        self.store
            .transact_within(&accounts::LOCK_LADDER, Self::NOW, &mut None, |state| {
                accounts::pin(state, &roster, &enrolled, label, Self::NOW).map(|event| vec![event])
            })
            .expect("a pin");
    }

    fn facts(&self) -> Facts {
        let home = self.b.home.display().to_string();
        let var = |k: &str| (k == "HOME").then(|| home.clone());
        let m = Machine {
            now: Self::NOW,
            home: Some(self.b.home.clone()),
            var: &var,
            pkg: Some(self.b.pkg.clone()),
            exclude: Vec::new(),
            records: Records::default(),
            harness_state: Some(self.b.state.clone()),
            roster: Some(self.config.join("accounts.toml")),
            accounts_mode: self.mode,
            config: None,
        };
        gather(&m, &CODEX)
    }
}

/// UNDER THE OWNER'S PIN ON A ROW OF ITS OWN, ON A CODEX BUILD THAT KEEPS NO ROW'S LOGIN ITS OWN,
/// EVERY LAUNCH IS REFUSED, AND THE DOCTOR AND `aterm ai login` SAY SO: the doctor's row is a
/// warning that names the refusal and, as a person's step (a `NEEDS YOU:` line), the pin of
/// `main`, the row that build runs; `aterm ai login codex` refuses with the launch's refusal,
/// never "lists no email" (both rows carry one). With `main`, the only row that build runs, signed
/// out too, the row is signed out (a FAIL), and its step is `main`'s sign-in at a terminal and then
/// its pin, a person's. WHERE THE PIN IS ON A ROW THE POOL RECORDED SIGNED OUT, ON A BUILD THAT
/// KEEPS THE ROW'S LOGIN, THAT ROW'S SIGN-IN ENDS THE REFUSAL, AND ATERM RUNS IT: the row is
/// signed out (a FAIL whose fix is `aterm ai login codex`, aterm's own, with no `NEEDS YOU:`
/// line), and that sign-in signs `work` in, even with `spare`, listed before it, not signed in
/// here either. CONTROL: with no pin, the row is `ok` and asks
/// nothing, and on a build that keeps the row's login the pin on `work` is served.
#[test]
fn a_pin_that_refuses_every_codex_launch_is_the_doctors_warning_and_the_logins_refusal() {
    use crate::ai::state::{Mark, State, assess, health};
    let pool = DoctorPool::new(false, None);
    let f = pool.facts();
    let row = assess(&f, true, DoctorPool::NOW);
    assert_eq!(
        f.pool.as_ref().and_then(|p| p.launch_refused.clone()),
        None,
        "CONTROL"
    );
    assert_eq!(
        (row.mark, row.mend.as_deref()),
        (Mark::Ok, None),
        "CONTROL: {}",
        row.fact
    );
    assert!(!row.fact.contains("refused"), "CONTROL: {}", row.fact);

    pool.pin("work");
    let f = pool.facts();
    let p = f.pool.as_ref().expect("codex pool");
    let refused = p.launch_refused.clone().expect("every launch is refused");
    assert_eq!(p.emails, 2);
    let row = assess(&f, true, DoctorPool::NOW);
    assert!(matches!(row.state, State::Ok | State::Idle), "{}", row.fact);
    assert_eq!(row.mark, Mark::Warn, "{}", row.fact);
    let refusal = refused
        .strip_suffix(crate::harness::accounts_pool::NO_WORKER)
        .expect("a worker's refusal");
    assert!(
        row.fact.contains(&format!(
            "a launch aterm makes of it now is refused: {refusal}"
        )),
        "{}",
        row.fact
    );
    assert_eq!(row.fix, None);
    let mend = row.mend.clone().expect("a person's step");
    assert!(
        mend.contains("`aterm harness accounts use main --cli codex`"),
        "{mend}"
    );
    let needs = health(std::slice::from_ref(&row))
        .needs_you()
        .expect("a NEEDS YOU line");
    assert!(needs.contains(&mend), "{needs}");
    let why = crate::ai::login::pool_target(crate::harness::accounts::Cli::Codex, p)
        .expect_err("nothing to sign in");
    assert!(why.contains(refusal), "{why}");
    assert!(!why.contains("lists no email"), "{why}");

    pool.sign_out_main();
    let f = pool.facts();
    let row = assess(&f, true, DoctorPool::NOW);
    assert_eq!(
        (row.state, row.mark),
        (State::SignedOut, Mark::Fail),
        "{}",
        row.fact
    );
    assert!(
        row.fact
            .contains("a launch aterm makes of it now is refused"),
        "{}",
        row.fact
    );
    assert_eq!(row.fix, None, "{}", row.fact);
    assert!(
        row.mend.as_deref().is_some_and(|mend| {
            mend.contains("`aterm ai login codex`")
                && mend.contains("`aterm harness accounts use main --cli codex`")
        }),
        "{:?}",
        row.mend
    );

    let pool = DoctorPool::new(true, None);
    pool.pin("work");
    let f = pool.facts();
    let p = f.pool.as_ref().expect("codex pool");
    assert_eq!(p.launch_refused, None, "CONTROL: {p:?}");
    assert_eq!(p.serving_label.as_deref(), Some("work"), "CONTROL");
    assert_eq!(assess(&f, true, DoctorPool::NOW).mark, Mark::Ok, "CONTROL");

    // A recorded sign-out ends the pin, so the pin stands on a row not signed in here where its
    // login is gone (or the records say so as an older build wrote them).
    std::fs::remove_file(pool.b.home.join(".aterm/logins/codex/work/auth.json"))
        .expect("work's login");
    let f = pool.facts();
    let p = f.pool.as_ref().expect("codex pool");
    let refused = p.launch_refused.clone().expect("every launch is refused");
    let refused = refused
        .strip_suffix(crate::harness::accounts_pool::NO_WORKER)
        .unwrap_or(&refused);
    assert_eq!(
        p.launch_end,
        Some(crate::ai::facts::LaunchEnd::SignIn("work".to_owned()))
    );
    assert_eq!(p.next.as_deref(), Some("work"));
    let row = assess(&f, true, DoctorPool::NOW);
    assert_eq!(
        (row.state, row.mark),
        (State::SignedOut, Mark::Fail),
        "{}",
        row.fact
    );
    assert!(
        row.fact.starts_with(&format!(
            "a launch aterm makes of it now is refused: {refused}"
        )),
        "{}",
        row.fact
    );
    assert_eq!(row.fix.as_deref(), Some("aterm ai login codex"));
    assert_eq!(row.mend, None);
    assert_eq!(row.actor, Some(crate::ai::state::sign_in_actor(&f)));
    assert_eq!(row.actor, Some(crate::ai::state::Actor::Aterm));
    let verdict = health(std::slice::from_ref(&row));
    assert_eq!(verdict.needs_you(), None, "{verdict:?}");
    assert_eq!(verdict.by_aterm, vec!["codex"]);
    assert_eq!(
        crate::ai::login::pool_target(crate::harness::accounts::Cli::Codex, p),
        Ok(("work", "renew")),
        "the sign-in signs in the pinned row"
    );
    assert_eq!(crate::ai::login::refusal(&f, &row), None);

    let roster = std::fs::read_to_string(&pool.roster).expect("roster");
    let spare = pool.b.home.join(".aterm/logins/codex/spare");
    std::fs::write(
        &pool.roster,
        roster.replacen(
            "[[account]]\nlabel = \"work\"",
            &format!(
                "[[account]]\nlabel = \"spare\"\nclass = \"dedicated\"\ndir = {:?}\n\
                 email = \"spare@example.com\"\n\n[[account]]\nlabel = \"work\"",
                spare.display().to_string()
            ),
            1,
        ),
    )
    .expect("a roster with `spare` before `work`");
    let f = pool.facts();
    let p = f.pool.as_ref().expect("codex pool");
    assert_eq!(p.rows, 3, "{p:?}");
    assert_eq!(p.next.as_deref(), Some("work"), "{p:?}");
    assert_eq!(
        assess(&f, true, DoctorPool::NOW).fix.as_deref(),
        Some("aterm ai login codex")
    );
}

/// UNDER THE OWNER'S PIN ON `work`, A ROW OF ITS OWN, ON A CODEX BUILD THAT KEEPS NO ROW'S LOGIN
/// ITS OWN, A WALLED `main` SERVES ONLY ONCE THAT PIN ENDS AND ITS WALL ENDS: the launch's
/// refusal, which the row's fact and `aterm ai login codex`'s refusal carry, names the pin's end,
/// `main`'s pin and its wall's end. CONTROL: with no pin, `main` serves by itself once its wall
/// ends, and the refusal names its wall's end and no pin.
#[test]
fn under_a_pin_on_a_row_of_its_own_a_walled_default_row_is_told_its_pin() {
    use crate::harness::codex_alab::{DefaultRow, unread_fix};
    let pool = DoctorPool::new(false, None);
    pool.wall_main();
    let pin = unread_fix(Some("main"));
    let ends = |p: &crate::ai::facts::CliAccounts| match &p.default_row {
        Some(DefaultRow::Walled { until, .. }) => crate::harness::usage::rfc3339_utc(*until),
        other => panic!("`main` is walled: {other:?}"),
    };
    let f = pool.facts();
    let p = f.pool.as_ref().expect("codex pool");
    let refused = p
        .launch_refused
        .clone()
        .expect("CONTROL: a launch is refused");
    assert_eq!(p.pinned, None, "CONTROL");
    assert!(
        refused.contains(&ends(p)) && !refused.contains(&pin),
        "CONTROL: {refused}"
    );

    pool.pin("work");
    let f = pool.facts();
    let p = f.pool.as_ref().expect("codex pool");
    assert_eq!(p.pinned.as_deref(), Some("work"));
    let refused = p.launch_refused.clone().expect("every launch is refused");
    assert!(
        refused.contains(&pin)
            && refused.contains(&ends(p))
            && refused.contains(crate::harness::codex_alab::UNPIN_FIX),
        "{refused}"
    );
    let row = crate::ai::state::assess(&f, true, DoctorPool::NOW);
    assert!(row.fact.contains(&pin), "{}", row.fact);
    let why = crate::ai::login::pool_target(crate::harness::accounts::Cli::Codex, p)
        .expect_err("nothing to sign in");
    assert!(why.contains(&pin), "{why}");
}

/// WHERE THE ROSTER LISTS NO ROW ON THIS MACHINE'S OWN LOGIN, THE STEP THAT ENDS A PIN'S REFUSAL
/// ON A CODEX BUILD THAT KEEPS NO ROW'S LOGIN ITS OWN IS THAT LOGIN LISTED AND THEN PINNED, since
/// a listing alone leaves the pin on the row of its own: with the pin on `work` and `work` the
/// only row, the doctor's step names `adopt-default` and then `accounts use`. CONTROL: with
/// `main` listed, the step is its pin alone.
#[test]
fn with_no_row_on_this_machines_own_login_the_step_lists_it_and_then_pins_it() {
    use crate::ai::state::assess;
    let adopt = crate::harness::codex_alab::unread_fix(None);
    let pin = crate::harness::codex_alab::unread_fix(Some("<label>"));
    let pool = DoctorPool::new(false, None);
    pool.pin("work");
    let mend = assess(&pool.facts(), true, DoctorPool::NOW)
        .mend
        .expect("CONTROL: a person's step");
    assert!(
        mend.contains("`aterm harness accounts use main --cli codex`") && !mend.contains(&adopt),
        "CONTROL: {mend}"
    );
    let roster = std::fs::read_to_string(&pool.roster).expect("roster");
    let work = roster
        .find("[[account]]\nlabel = \"work\"")
        .expect("work's row");
    std::fs::write(&pool.roster, &roster[work..]).expect("a roster of `work` alone");
    let f = pool.facts();
    let p = f.pool.as_ref().expect("codex pool");
    assert_eq!(
        p.default_row,
        Some(crate::harness::codex_alab::DefaultRow::Unlisted),
        "{p:?}"
    );
    assert_eq!(
        p.launch_step.as_deref(),
        Some(format!("`{adopt}`, then `{pin}`").as_str())
    );
    let mend = assess(&f, true, DoctorPool::NOW)
        .mend
        .expect("a person's step");
    assert!(
        mend.ends_with(&format!("`{adopt}`, then `{pin}`")),
        "{mend}"
    );
}

/// A LAUNCH REFUSED BY ANOTHER MAC'S LEASE OR WARM SET ENDS BY ITSELF, SO THE DOCTOR'S ROW NAMES IT
/// AS A NOTE AND ASKS NOBODY FOR ANYTHING: on a Codex build that keeps no row's login its own, with
/// `main` (the row it runs) leased by another Mac for an hour, or held for another Mac's warm set,
/// the row says the launch is refused, with no fix, step or `NEEDS YOU:` line. CONTROL: with
/// neither, no launch is refused and the row is `ok`.
#[test]
fn a_launch_another_mac_holds_back_is_a_note_that_asks_nobody() {
    use crate::ai::facts::LaunchEnd;
    use crate::ai::state::{Mark, State, assess, health};
    let pool = DoctorPool::new(false, None);
    let row = assess(&pool.facts(), true, DoctorPool::NOW);
    assert_eq!(row.mark, Mark::Ok, "CONTROL: {}", row.fact);
    assert!(!row.fact.contains("refused"), "CONTROL: {}", row.fact);

    let leased = DoctorPool::new(false, None);
    leased.state(|state| {
        state.note_lease("main", "other-mac", DoctorPool::NOW + 3600, DoctorPool::NOW);
    });
    let held = DoctorPool::new(false, None);
    let roster = std::fs::read_to_string(&held.roster).expect("roster");
    std::fs::write(
        &held.roster,
        roster.replacen(
            "label = \"main\"\n",
            "label = \"main\"\nheld_by = \"other-mac\"\n",
            1,
        ),
    )
    .expect("a roster holding main for another Mac");
    for (case, pool) in [("leased", &leased), ("held", &held)] {
        let f = pool.facts();
        let p = f.pool.as_ref().expect("codex pool");
        assert!(p.launch_refused.is_some(), "{case}: {p:?}");
        assert_eq!(p.launch_end, Some(LaunchEnd::Pool), "{case}");
        let row = assess(&f, true, DoctorPool::NOW);
        assert!(
            matches!(row.state, State::Ok | State::Idle),
            "{case}: {}",
            row.fact
        );
        assert_eq!(row.mark, Mark::Note, "{case}: {}", row.fact);
        assert!(
            row.fact
                .contains("a launch aterm makes of it now is refused"),
            "{case}: {}",
            row.fact
        );
        assert_eq!(
            (row.fix.as_deref(), row.mend.as_deref(), row.actor),
            (None, None, None),
            "{case}"
        );
        assert_eq!(
            health(std::slice::from_ref(&row)).needs_you(),
            None,
            "{case}"
        );
    }
}

/// WHERE A CODEX ROSTER CARRIES AN EMAIL AND NO ROW HAS A SIGN-IN TO RUN, `aterm ai login codex`
/// SAYS SO, NEVER THAT THE ROSTER LISTS NO EMAIL: on a build that keeps no row's login its own,
/// with `main` alone listed, with an email, and its login working, the refusal points at the
/// report of each row's state. CONTROL: with no email on the row, it says the roster lists none.
#[test]
fn a_login_with_an_email_and_nothing_to_sign_in_says_so() {
    use crate::harness::accounts::Cli;
    let pool = DoctorPool::new(false, None);
    pool.main_alone(Some("main@example.com"));
    let f = pool.facts();
    let p = f.pool.as_ref().expect("codex pool");
    assert_eq!(
        (p.emails, p.next.as_deref(), p.launch_refused.as_deref()),
        (1, None, None)
    );
    let why = crate::ai::login::pool_target(Cli::Codex, p).expect_err("nothing to sign in");
    assert!(
        why.contains("names no row `aterm ai login codex` signs in now")
            && why.contains("says each row's state"),
        "{why}"
    );
    assert!(!why.contains("lists no email"), "{why}");

    let pool = DoctorPool::new(false, None);
    pool.main_alone(None);
    let f = pool.facts();
    let p = f.pool.as_ref().expect("codex pool");
    assert_eq!(p.emails, 0, "CONTROL");
    let why = crate::ai::login::pool_target(Cli::Codex, p).expect_err("nothing to sign in");
    assert!(why.contains("lists no email"), "CONTROL: {why}");
}

/// UNDER `[harness] accounts = "off"` NO OTHER ROW IS OFFERED FOR SIGN-IN, SINCE ROTATION OFF TAKES
/// NO ROW BUT THE OWNER'S PIN: with no pin, `main` walled and `work` recorded signed out, the
/// facts offer no next sign-in. CONTROL: under rotation, the same records offer `work`'s.
#[test]
fn under_rotation_off_no_other_row_is_offered_for_sign_in() {
    use crate::harness::accounts::Mode;
    for (mode, want) in [(Some(Mode::Off), None), (None, Some("work"))] {
        let pool = DoctorPool::new(true, mode);
        pool.wall_main();
        pool.sign_out("work");
        let f = pool.facts();
        let p = f.pool.as_ref().expect("codex pool");
        assert_eq!(p.serving_label.as_deref(), Some("main"), "{mode:?}");
        assert!(p.limited_until.is_some(), "{mode:?}: {p:?}");
        assert_eq!(p.next_sign_in.as_deref(), want, "{mode:?}");
    }
}

/// UNDER `[harness] accounts = "off"` WITH NO PIN, A LAUNCH RUNS ON THIS MACHINE'S OWN LOGIN, SO
/// THE DOCTOR JUDGES `main`'S RECORDS: signed out, the row is signed out (a FAIL with
/// `aterm ai login codex` as its fix), never `ok … signed in`; walled, it is limited. That holds
/// on a build that keeps the row's login and on one that keeps none. CONTROL: usable, it is `ok`;
/// and with the pin on `work`, which a build that keeps the row's login serves, `main`'s sign-out
/// is not the row's.
#[test]
fn under_rotation_off_with_no_pin_the_doctor_judges_this_machines_own_login() {
    use crate::ai::state::{Mark, State, assess};
    use crate::harness::accounts::Mode;
    for reads in [true, false] {
        let pool = DoctorPool::new(reads, Some(Mode::Off));
        let row = assess(&pool.facts(), true, DoctorPool::NOW);
        assert!(
            matches!(row.state, State::Ok | State::Idle) && row.mark == Mark::Ok,
            "{reads}: CONTROL: {}",
            row.fact
        );

        pool.wall_main();
        let f = pool.facts();
        assert_eq!(
            f.pool.as_ref().and_then(|p| p.serving_label.as_deref()),
            Some("main")
        );
        let row = assess(&f, true, DoctorPool::NOW);
        assert!(
            matches!(row.state, State::Limited(until) if until >= DoctorPool::NOW + 3600),
            "{reads}: {}",
            row.fact
        );

        let pool = DoctorPool::new(reads, Some(Mode::Off));
        pool.sign_out_main();
        let row = assess(&pool.facts(), true, DoctorPool::NOW);
        assert_eq!(
            (row.state, row.mark),
            (State::SignedOut, Mark::Fail),
            "{reads}: {}",
            row.fact
        );
        assert!(!row.fact.starts_with("signed in"), "{reads}: {}", row.fact);
        assert_eq!(row.fix.as_deref(), Some("aterm ai login codex"), "{reads}");
    }
    let pool = DoctorPool::new(true, Some(Mode::Off));
    pool.sign_out_main();
    pool.pin("work");
    let f = pool.facts();
    assert_eq!(
        f.pool.as_ref().and_then(|p| p.serving_label.as_deref()),
        Some("work"),
        "CONTROL"
    );
    let row = assess(&f, true, DoctorPool::NOW);
    assert!(
        matches!(row.state, State::Ok | State::Idle),
        "CONTROL: {}",
        row.fact
    );
}

/// A ROW THE POOL RECORDED SIGNED OUT IS NOT COUNTED SIGNED IN HERE, THOUGH ITS LOGIN FILE STAYS,
/// AND THE DOCTOR QUOTES THE RECORD AS IT STANDS, WITH WHEN, NAMING NO LOGIN WALL (the one word
/// covers a wall, a displacement by Codex's own sign-in and a check that found no login): on a
/// build that serves `main` alone, `pool: 2 accounts (1 signed in here)` and `its login has ended
/// (its account pool recorded it signed out 1 min ago)`. CONTROL: before the record, both rows are
/// signed in here.
#[test]
fn a_row_recorded_signed_out_is_not_signed_in_here_and_is_quoted_as_recorded() {
    use crate::ai::state::{State, assess};
    let pool = DoctorPool::new(false, None);
    let f = pool.facts();
    let p = f.pool.as_ref().expect("codex pool");
    assert_eq!((p.enrolled, p.signed_in), (2, 2), "CONTROL");
    assert!(
        assess(&f, true, DoctorPool::NOW)
            .fact
            .contains("pool: 2 accounts (2 signed in here)"),
        "CONTROL"
    );
    pool.sign_out_main();
    let f = pool.facts();
    let p = f.pool.as_ref().expect("codex pool");
    assert_eq!((p.enrolled, p.signed_in), (2, 1), "{p:?}");
    assert_eq!(p.signed_out_at, Some(DoctorPool::NOW - 60));
    let row = assess(&f, true, DoctorPool::NOW);
    assert_eq!(row.state, State::SignedOut, "{}", row.fact);
    assert!(
        row.fact
            .starts_with("its login has ended (its account pool recorded it signed out 1 min ago)"),
        "{}",
        row.fact
    );
    assert!(!row.fact.contains("login wall"), "{}", row.fact);
    assert!(
        row.fact.contains("pool: 2 accounts (1 signed in here)"),
        "{}",
        row.fact
    );
}

/// A LAUNCH STARTS NOTHING WHERE ITS PROGRAM NAMES NO FILE A SHELL FINDS: an absolute path to no
/// file, or a bare word no directory of `PATH` holds. CONTROL: the same word with a file of that
/// name in a later directory of `PATH`, an absolute path to a file, a bare word where `PATH` is
/// not known, and a relative path with a folder (the shell's folder's) start something, or are
/// not judged.
#[test]
fn a_launch_starts_nothing_where_no_file_answers_its_program() {
    let d = aterm_tempfile::tempdir().expect("tmp");
    let empty = d.path().join("empty");
    let full = d.path().join("full");
    std::fs::create_dir_all(&empty).expect("empty");
    std::fs::create_dir_all(full.join("codex-dir")).expect("full");
    std::fs::write(full.join("codex"), "#!/bin/sh\n").expect("a codex");
    let only_empty = std::env::join_paths([&empty]).expect("PATH");
    let both = std::env::join_paths([&empty, &full]).expect("PATH");
    let word = Path::new("codex");
    assert!(launch_finds_nothing(word, Some(only_empty.as_os_str())));
    assert!(launch_finds_nothing(&empty.join("codex"), None));
    assert!(
        launch_finds_nothing(Path::new("codex-dir"), Some(both.as_os_str())),
        "a directory is no program"
    );
    assert!(
        !launch_finds_nothing(word, Some(both.as_os_str())),
        "CONTROL"
    );
    assert!(!launch_finds_nothing(&full.join("codex"), None), "CONTROL");
    assert!(!launch_finds_nothing(word, None), "CONTROL: PATH not known");
    assert!(
        !launch_finds_nothing(Path::new("bin/codex"), Some(only_empty.as_os_str())),
        "CONTROL: the shell's folder's"
    );
}
