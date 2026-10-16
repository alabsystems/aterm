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
