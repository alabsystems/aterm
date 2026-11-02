// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Reconcile the legacy GitHub keychain override with an already delivered vault helper.
//! This path inventories no repositories and never installs a keychain credential.

use super::{Asked, Github, TOKEN_FILE, asked, identity, native, profile, read_token};
use crate::git_credential::{Git, VaultStanding};
use crate::model::User;
use crate::port::{Secret, System};
use crate::sync::GithubRead;
use std::path::Path;

struct Prepared {
    user: User,
    account: String,
    owner: Option<String>,
    token: Secret,
}

impl Prepared {
    fn owners(&self) -> Vec<&str> {
        std::iter::once(self.account.as_str())
            .chain(self.owner.as_deref())
            .collect()
    }
}

fn prepare(sys: &dyn System) -> Result<Prepared, GithubRead> {
    let user = sys.identity().map_err(GithubRead::Deferred)?.user;
    if user.uid != sys.euid() || user.uid == 0 {
        return Err(GithubRead::NotApplicable(
            "GitHub credentials belong to this login alone".into(),
        ));
    }
    let profile = profile(sys, &user)
        .map_err(GithubRead::Deferred)?
        .unwrap_or_default();
    let configured = match asked(profile.github, &user).map_err(GithubRead::Deferred)? {
        Asked::Off => {
            return Err(GithubRead::NotApplicable(
                "off: [github] enabled = false".into(),
            ));
        }
        Asked::Profile(config)
            if config.token_file == Path::new(&user.home).join(TOKEN_FILE)
                && config.token_key.is_none() =>
        {
            Some(config)
        }
        Asked::Profile(_) => {
            return Err(GithubRead::NotApplicable(
                "a custom GitHub token source is left unchanged".into(),
            ));
        }
        Asked::Scoped(config) => Some(config),
        Asked::Default => None,
    };
    let git = Git::keychain(sys);
    if !git.installed(sys) {
        return Err(GithubRead::Deferred(super::NEEDS_GIT.into()));
    }
    let source = Github {
        account: String::new(),
        owner: String::new(),
        token_file: Path::new(&user.home).join(TOKEN_FILE),
        token_key: None,
    };
    let token = read_token(sys, &user, &source).map_err(|error| GithubRead::Deferred(error.why))?;
    let account = identity(sys, &user, &token).map_err(|error| GithubRead::Deferred(error.why))?;
    if configured
        .as_ref()
        .is_some_and(|config| !config.account.eq_ignore_ascii_case(&account))
    {
        return Err(GithubRead::NotApplicable(
            "the configured GitHub account differs from the vault's account; settings unchanged"
                .into(),
        ));
    }
    Ok(Prepared {
        user,
        account,
        owner: configured.map(|config| config.owner),
        token,
    })
}

fn inspect(sys: &dyn System, git: &Git, ready: &Prepared) -> Result<GithubRead, String> {
    let owners = ready.owners();
    // Inspect the managed override before invoking any effective helper: a legacy
    // login-keychain lookup is unnecessary when the XDG helper proves the token.
    let standing = git.standing_for(
        sys,
        &ready.user,
        &ready.account,
        &owners,
        Some(&ready.token),
    )?;
    if standing.convert_to_vault {
        return Ok(GithubRead::Due(
            "a legacy GitHub keychain override shadows the verified vault helper".into(),
        ));
    }
    if standing.foreign.is_none() {
        return Ok(GithubRead::NotApplicable(
            "no verified vault-helper migration; existing GitHub settings unchanged".into(),
        ));
    }
    match git.vault_standing(sys, &ready.user, &ready.account, &owners, &ready.token) {
        VaultStanding::Ready => {
            return Ok(GithubRead::Current(
                "vault GitHub development lookup verified".into(),
            ));
        }
        VaultStanding::Convert { .. } => {
            return Ok(GithubRead::Due(
                "a legacy GitHub keychain override shadows the verified vault helper".into(),
            ));
        }
        VaultStanding::Absent => {}
    }
    Ok(GithubRead::NotApplicable(
        "no verified vault-helper migration; existing GitHub settings unchanged".into(),
    ))
}

/// Read the delivered vault helper's readiness without listing or cloning repositories.
/// Explicitly disabled, custom-token and foreign GitHub settings remain unchanged.
///
/// # Errors
/// Git's settings or credential reply cannot be read completely.
pub fn vault_git_credentials(sys: &dyn System) -> Result<GithubRead, String> {
    match prepare(sys) {
        Ok(ready) => inspect(sys, &Git::keychain(sys), &ready),
        Err(status) => Ok(status),
    }
}

/// Remove only the legacy keychain override that shadows an authenticated vault helper.
/// The profile and token are checked again immediately before the existing migration runs.
///
/// # Errors
/// The admitted inputs changed, Git cannot migrate its settings, or the vault helper does
/// not return the authenticated token afterwards.
pub fn sync_vault_git_credentials(sys: &dyn System) -> Result<String, String> {
    synchronize(sys, &Git::keychain(sys))
}

fn synchronize(sys: &dyn System, git: &Git) -> Result<String, String> {
    let ready = prepare(sys).map_err(|status| format!("GitHub credentials changed: {status:?}"))?;
    synchronize_prepared(sys, git, ready, || prepare(sys))
}

fn synchronize_prepared(
    sys: &dyn System,
    git: &Git,
    ready: Prepared,
    refresh: impl Fn() -> Result<Prepared, GithubRead>,
) -> Result<String, String> {
    if !matches!(inspect(sys, git, &ready)?, GithubRead::Due(_)) {
        return Ok("GitHub settings already current or left unchanged".into());
    }
    let current = || {
        let now = refresh().map_err(|_| "GitHub profile or token is no longer ready")?;
        if now.user == ready.user
            && now.account.eq_ignore_ascii_case(&ready.account)
            && now.owner == ready.owner
            && now.token.bytes() == ready.token.bytes()
            && matches!(inspect(sys, git, &now)?, GithubRead::Due(_))
        {
            Ok(())
        } else {
            Err("GitHub profile, token or settings changed before migration".into())
        }
    };
    native::unconfigure_to_vault(
        git,
        sys,
        &ready.user,
        &ready.account,
        &ready.owners(),
        &ready.token,
        &current,
    )?;
    Ok("removed the legacy GitHub override; vault helper verified; no repositories changed".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::items::defaults::tests::user;
    use crate::port::{Fake, GithubResponse, Live, Scratch, Stat};
    use std::cell::{Cell, RefCell};

    const DEV: &str = "fixture_dev_token";
    const RELEASE: &str = "fixture_release_token";

    fn fake(who: &User) -> Fake {
        let mut sys = Fake::root();
        sys.euid = who.uid;
        sys.logged_in(who);
        sys.file(crate::items::github::CLT_GIT, b"git");
        let path = Path::new(&who.home).join(TOKEN_FILE);
        sys.file(path.to_str().unwrap(), DEV.as_bytes()).stat_of(
            path.to_str().unwrap(),
            Stat {
                uid: who.uid,
                gid: 20,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: DEV.len() as u64,
            },
        );
        sys.on_github_api(
            who,
            "/user",
            Ok(GithubResponse {
                status: 200,
                headers: Vec::new(),
                body: b"{\"login\":\"alice\"}".to_vec(),
            }),
        );
        sys
    }

    #[test]
    fn disabled_and_custom_sources_do_not_read_git_or_credentials() {
        for text in [
            "[github]\nenabled = false\n",
            "[github]\naccount = 'alice'\ntoken_file = '~/.secrets/custom'\n",
        ] {
            let who = user("alice", 501);
            let sys = fake(&who);
            let path = Path::new(&who.home).join(".config/aterm/init.toml");
            sys.file(path.to_str().unwrap(), text.as_bytes()).stat_of(
                path.to_str().unwrap(),
                Stat {
                    uid: who.uid,
                    gid: 20,
                    mode: 0o600,
                    dir: false,
                    symlink: false,
                    len: text.len() as u64,
                },
            );
            assert!(matches!(
                vault_git_credentials(&sys).unwrap(),
                GithubRead::NotApplicable(_)
            ));
            assert!(
                sys.calls()
                    .iter()
                    .all(|call| !call.contains("git ") && !call.contains("agh GET")),
                "{:?}",
                sys.calls()
            );
        }
    }

    #[test]
    fn explicit_default_source_keeps_its_scope_but_custom_formats_and_other_accounts_stay() {
        for (account, source, accepted) in [
            ("alice", "token_file='~/.secrets/gh_access_token'", true),
            (
                "alice",
                "token_file='~/.secrets/gh_access_token'\ntoken_key='TOKEN'",
                false,
            ),
            ("alice", "token_file='~/.secrets/custom'", false),
            (
                "someone-else",
                "token_file='~/.secrets/gh_access_token'",
                false,
            ),
        ] {
            let who = user("alice", 501);
            let sys = fake(&who);
            let text = format!("[github]\naccount='{account}'\nowner='team'\n{source}\n");
            let path = Path::new(&who.home).join(".config/aterm/init.toml");
            sys.file(path.to_str().unwrap(), text.as_bytes()).stat_of(
                path.to_str().unwrap(),
                Stat {
                    uid: who.uid,
                    gid: 20,
                    mode: 0o600,
                    dir: false,
                    symlink: false,
                    len: text.len() as u64,
                },
            );
            match prepare(&sys) {
                Ok(ready) => {
                    assert!(accepted);
                    assert_eq!(ready.account, "alice");
                    assert_eq!(ready.owner.as_deref(), Some("team"));
                }
                Err(GithubRead::NotApplicable(_)) => assert!(!accepted),
                Err(other) => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn a_keychain_without_a_vault_helper_is_never_queried_or_changed() {
        let who = user("alice", 501);
        let sys = fake(&who);
        crate::git_credential::script::configured(&sys, &who, "alice");
        let ready = prepare(&sys).unwrap_or_else(|why| panic!("{why:?}"));
        assert!(matches!(
            inspect(&sys, &Git::keychain(&sys), &ready).unwrap(),
            GithubRead::NotApplicable(_)
        ));
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.contains("credential fill") && !call.contains("--unset-all")),
            "{:?}",
            sys.calls()
        );
    }

    #[test]
    fn automatic_migration_restores_normal_dev_and_release_lookups_without_cloning() {
        crate::items::github::need_clt_git();
        let live = Live::new();
        let mut scratch = Scratch::new(&live, "github-auto-sync").unwrap();
        let home = scratch.path().to_owned();
        let mut who = user("fixture", live.euid());
        who.home = home.to_string_lossy().into_owned();
        let legacy = Git::isolated(&format!("store --file={}", home.join("legacy").display()));
        let vault_helper = format!("store --file={}", home.join("vault").display());
        let vault = Git::isolated(&vault_helper);
        let dev = Secret::new(DEV.as_bytes()).unwrap();
        let release = Secret::new(RELEASE.as_bytes()).unwrap();
        legacy
            .store(&live, Some(&who), "github.com", "alice", &dev)
            .unwrap();
        legacy.configure(&live, &who, "alice").unwrap();
        vault
            .store(&live, Some(&who), "github.com", "alice", &dev)
            .unwrap();
        vault
            .store(&live, Some(&who), "github.com", "release", &release)
            .unwrap();
        std::fs::create_dir_all(home.join(".config/git")).unwrap();
        live.write_file(&home.join(".config/git/config"), format!(
            "[credential \"https://github.com\"]\n\thelper =\n\thelper = {vault_helper}\n\tusername = alice\n[user]\n\tname = Preserved\n"
        ).as_bytes(), 0o600).unwrap();
        // Change each guarded input after the XDG helper proved the original token,
        // before the current-input check may remove anything from the global file.
        for drift in ["off", "token", "foreign"] {
            let changed = Cell::new(false);
            let drift_sys = fake(&who);
            let expected = RefCell::new(std::fs::read(home.join(".gitconfig")).unwrap());
            let ready = prepare(&drift_sys).unwrap_or_else(|why| panic!("{why:?}"));
            let refresh = || {
                changed.set(true);
                if drift == "foreign" {
                    for args in [
                        [
                            "config",
                            "--global",
                            "--replace-all",
                            "credential.https://github.com.helper",
                            "",
                        ],
                        [
                            "config",
                            "--global",
                            "--add",
                            "credential.https://github.com.helper",
                            "!exit 1",
                        ],
                    ] {
                        assert!(
                            live.run_complete_stdout(&legacy.cmd(Some(&who)).args(args))
                                .unwrap()
                                .success()
                        );
                    }
                    *expected.borrow_mut() = std::fs::read(home.join(".gitconfig")).unwrap();
                } else {
                    let (path, bytes) = if drift == "off" {
                        (
                            home.join(".config/aterm/init.toml"),
                            b"[github]\nenabled=false\n".as_slice(),
                        )
                    } else {
                        (home.join(TOKEN_FILE), b"changed_vault_token".as_slice())
                    };
                    drift_sys.file(path.to_str().unwrap(), bytes).stat_of(
                        path.to_str().unwrap(),
                        Stat {
                            uid: who.uid,
                            gid: 20,
                            mode: 0o600,
                            dir: false,
                            symlink: false,
                            len: bytes.len() as u64,
                        },
                    );
                }
                prepare(&drift_sys)
            };
            assert!(
                synchronize_prepared(&live, &legacy, ready, refresh).is_err(),
                "{drift}"
            );
            assert!(changed.get(), "{drift}");
            assert_eq!(
                *expected.borrow(),
                std::fs::read(home.join(".gitconfig")).unwrap()
            );
            legacy.configure(&live, &who, "alice").unwrap();
        }
        let sys = fake(&who);
        let ready = prepare(&sys).unwrap_or_else(|why| panic!("{why:?}"));
        assert!(matches!(
            inspect(&live, &legacy, &ready).unwrap(),
            GithubRead::Due(_)
        ));
        // These are ordinary configured Git lookups, with no -c helper override.
        let lookup = |account: &str| {
            let cmd = legacy.cmd(Some(&who)).args(["credential", "fill"]);
            let input = format!("protocol=https\nhost=github.com\nusername={account}\n\n");
            live.run_complete_stdout_input(&cmd, input.as_bytes())
                .unwrap()
        };
        assert!(!lookup("release").success());
        assert!(
            synchronize_prepared(
                &live,
                &legacy,
                prepare(&sys).unwrap_or_else(|why| panic!("{why:?}")),
                || prepare(&sys)
            )
            .unwrap()
            .contains("no repositories changed")
        );
        for (account, token) in [("alice", DEV), ("release", RELEASE)] {
            let mut answer = lookup(account);
            assert!(answer.success());
            assert!(
                answer
                    .stdout
                    .windows(token.len())
                    .any(|part| part == token.as_bytes())
            );
            answer.stdout.fill(0);
        }
        assert!(
            std::fs::read_to_string(home.join(".config/git/config"))
                .unwrap()
                .contains("Preserved")
        );
        assert!(
            sys.calls().iter().all(|call| !call.contains("/repos")
                && !call.contains(" clone ")
                && !call.contains("credential approve")),
            "{:?}",
            sys.calls()
        );
        // A foreign global helper must not be removed even when the vault token exists.
        for args in [
            [
                "config",
                "--global",
                "--replace-all",
                "credential.https://github.com.helper",
                "",
            ],
            [
                "config",
                "--global",
                "--add",
                "credential.https://github.com.helper",
                "!exit 1",
            ],
        ] {
            assert!(
                live.run_complete_stdout(&legacy.cmd(Some(&who)).args(args))
                    .unwrap()
                    .success()
            );
        }
        assert!(matches!(
            inspect(&live, &legacy, &ready).unwrap(),
            GithubRead::NotApplicable(_)
        ));
        let before = std::fs::read(home.join(".gitconfig")).unwrap();
        synchronize_prepared(
            &live,
            &legacy,
            prepare(&sys).unwrap_or_else(|why| panic!("{why:?}")),
            || prepare(&sys),
        )
        .unwrap();
        assert_eq!(before, std::fs::read(home.join(".gitconfig")).unwrap());
        scratch.close().unwrap();
    }
}
