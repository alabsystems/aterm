// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The selected vault's repository inputs and their admission to GitHub work.

use super::{Plan, Row, Stage, item_rows, readiness_row};
use aterm_machines::items::Ctx;
use aterm_machines::model::Mode;
use aterm_machines::port::System;

/// Why aterm sets up no repositories on this Mac, whatever it tries again: it sets them up for
/// the one user account a Mac has, and this one has another number of them. In the page's
/// words, which `aterm doctor` prints too.
const ONE_USER: &str = "aterm sets up GitHub repositories only on a Mac with one user account.";

/// Whether the repositories failed for good on this Mac ([`ONE_USER`]): no retry, no vault and
/// no sign-in changes the number of its user accounts.
fn failed_for_good(ctx: &Ctx) -> bool {
    ctx.users.len() != 1
}

pub(super) fn run(
    sys: &dyn System,
    ctx: &Ctx,
    vault: Option<&Plan>,
    mode: Mode,
) -> Result<Vec<Row>, String> {
    if failed_for_good(ctx) {
        return Ok(vec![Row {
            name: Stage::Repositories.name().into(),
            status: aterm_types::macos_setup::FAILED_FOR_GOOD.into(),
            detail: ONE_USER.into(),
        }]);
    }
    Ok(match items(sys, ctx, vault, mode)? {
        Some(rows) => item_rows(rows),
        None => vec![readiness_row(
            Stage::Repositories.name(),
            false,
            "the selected secrets vault changed or is still syncing; repository inputs wait for a complete version".into(),
        )],
    })
}

pub(super) fn observe(
    sys: &dyn System,
    ctx: &Ctx,
    vault: Option<&Plan>,
) -> Result<aterm_types::macos_setup::Observation, String> {
    use aterm_types::macos_setup::{Observation, Setting, Standing, Subject};
    if failed_for_good(ctx) {
        return Ok(Observation {
            standing: Standing::Failed,
            detail: Some(ONE_USER.into()),
            ..Observation::unknown(Subject::Setting(Setting::Repositories))
        });
    }
    Ok(match items(sys, ctx, vault, Mode::Plan)? {
        Some(rows) => aterm_machines::init_observe::observation(
            Setting::Repositories,
            &rows.iter().collect::<Vec<_>>(),
        ),
        None => Observation {
            standing: Standing::Waiting,
            note: Some("Waits for the vault to finish syncing this Mac".into()),
            ..Observation::unknown(Subject::Setting(Setting::Repositories))
        },
    })
}

fn items(
    sys: &dyn System,
    ctx: &Ctx,
    vault: Option<&Plan>,
    mode: Mode,
) -> Result<Option<Vec<aterm_machines::model::ItemReport>>, String> {
    let Some(vault) = vault else {
        return aterm_machines::init_github::run_initial_repositories(sys, ctx, mode).map(Some);
    };
    let [user] = ctx.users.as_slice() else {
        return Err("repository setup needs exactly one console account".into());
    };
    let paths = aterm_secrets::client::Paths::for_home(std::path::Path::new(&user.home));
    let configured = aterm_secrets::client::config(&paths).map_err(|error| error.to_string())?;
    let selected = aterm_machines::init_secrets::selected_vault(
        &vault.vault_url,
        &paths.home,
        configured.as_ref(),
    )?;
    let Some(expected) = selected else {
        return Ok(None);
    };
    let captured = aterm_secrets::client::with_materialization_ready(&paths, expected, || {
        aterm_machines::init_github::InitialRepositoryInputs::capture(sys, ctx)
            .map_err(aterm_secrets::Error::new)
    })
    .map_err(|error| error.to_string())?;
    let Some(captured) = captured else {
        return Ok(None);
    };
    aterm_machines::init_github::run_initial_repositories_captured(
        sys,
        ctx,
        mode,
        captured.inputs,
        move || {
            captured
                .snapshot
                .is_current(&paths)
                .map_err(|error| error.to_string())
        },
    )
    .map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_machines::model::{HostFacts, User};
    use aterm_machines::port::{Fake, GithubResponse, Stat};
    use aterm_secrets::client::{self, JoinOptions, Paths};
    use aterm_types::macos_setup::{Selection, Setting};
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::Path;
    use std::time::Duration;

    const TOKEN: &str = "fixture_repository_token_only";

    fn joined(home: &Path, source: &Path) -> Paths {
        std::fs::write(
            source,
            format!("gh_access_token={TOKEN}\n~/.tool/other=before\n"),
        )
        .unwrap();
        std::fs::set_permissions(source, std::fs::Permissions::from_mode(0o600)).unwrap();
        let paths = Paths::for_home(home);
        client::join(
            &paths,
            &JoinOptions {
                source: source.to_string_lossy().into_owned(),
                host: Some("fixture".into()),
                interactive: false,
                wait: Duration::ZERO,
                confirm: &|_| false,
                password: &|| None,
                requested: &|| {},
                say: &|_| {},
            },
        )
        .unwrap();
        assert!(client::materialization_ready(&paths).unwrap());
        paths
    }

    fn fake(home: &Path) -> (Fake, Ctx) {
        let user = User {
            name: "alice".into(),
            uid: 501,
            gid: 20,
            home: home.to_string_lossy().into_owned(),
            shell: "/bin/zsh".into(),
            admin: false,
        };
        let mut sys = Fake::root();
        sys.euid = user.uid;
        let stat = |dir, len| Stat {
            uid: user.uid,
            gid: user.gid,
            mode: if dir { 0o700 } else { 0o600 },
            dir,
            symlink: false,
            len,
        };
        sys.stat_of(home.to_str().unwrap(), stat(true, 0));
        sys.stat_of(aterm_machines::items::github::CLT_GIT, stat(false, 1));
        let token = home.join(".secrets/gh_access_token");
        let bytes = std::fs::read(&token).unwrap();
        sys.file(token.to_str().unwrap(), &bytes)
            .stat_of(token.to_str().unwrap(), stat(false, bytes.len() as u64));
        for (endpoint, body) in [
            ("/user", aterm_json::json!({"login":"alice"})),
            (
                "/user/repos?visibility=all&per_page=30&sort=full_name&direction=asc&page=1",
                aterm_json::json!([]),
            ),
        ] {
            sys.on_github_api(
                &user,
                endpoint,
                Ok(GithubResponse {
                    status: 200,
                    headers: Vec::new(),
                    body: aterm_json::to_vec(&body).unwrap(),
                }),
            );
        }
        let ctx = Ctx {
            facts: HostFacts::default(),
            users: vec![user.clone()],
            console: Some(user),
            host_name: "fixture".into(),
            cache_dir: home.join("cache"),
            disrupt: false,
        };
        (sys, ctx)
    }

    fn after_sync_failure() -> (aterm_tempfile::TempDir, Plan, Fake, Ctx) {
        let root = aterm_tempfile::tempdir().unwrap();
        let source = root.path().join("team.secrets");
        let home = root.path().join("home");
        let paths = joined(&home, &source);
        // Readiness was true. The subscriber now writes a newer token while another
        // entry fails, before the repository stage captures its local inputs.
        let other = home.join(".tool/other");
        std::fs::remove_file(&other).unwrap();
        std::fs::create_dir(&other).unwrap();
        std::fs::write(
            &source,
            "gh_access_token=fixture_new_repository_token\n~/.tool/other=after\n",
        )
        .unwrap();
        client::sync(&paths, None).unwrap();
        assert!(!client::materialization_ready(&paths).unwrap());
        let mut selection = Selection::none();
        selection.set(Setting::Secrets, true);
        selection.set(Setting::Repositories, true);
        let plan = Plan {
            selection,
            vault_url: source.to_string_lossy().into_owned(),
        };
        let (sys, ctx) = fake(&home);
        (root, plan, sys, ctx)
    }

    #[test]
    fn a_partial_sync_after_readiness_blocks_repository_inputs() {
        let (_root, plan, sys, ctx) = after_sync_failure();
        let _result = run(&sys, &ctx, Some(&plan), Mode::Plan);
        assert!(
            !sys.calls().iter().any(|call| call.contains(" agh GET ")),
            "incomplete selected vault reached GitHub: {:?}",
            sys.calls()
        );
    }

    #[test]
    fn leaving_secrets_as_is_uses_the_existing_local_repository_token() {
        let (_root, _plan, sys, ctx) = after_sync_failure();
        let _result = run(&sys, &ctx, None, Mode::Plan);
        assert!(sys.calls().iter().any(|call| call.contains(" agh GET ")));
    }

    #[test]
    fn a_ready_selected_vault_allows_repository_observation() {
        let root = aterm_tempfile::tempdir().unwrap();
        let source = root.path().join("team.secrets");
        let home = root.path().join("home");
        joined(&home, &source);
        let plan = Plan {
            vault_url: source.to_string_lossy().into_owned(),
            ..Plan::default()
        };
        let (sys, ctx) = fake(&home);
        let _result = run(&sys, &ctx, Some(&plan), Mode::Plan);
        assert!(sys.calls().iter().any(|call| call.contains(" agh GET ")));
    }

    #[test]
    fn a_ready_vault_keeps_the_repository_inputs_typed_blocker() {
        use aterm_machines::init_observe::Wait;
        use aterm_types::macos_setup::Standing;

        for wait in [Wait::Token, Wait::CommandLineTools] {
            let root = aterm_tempfile::tempdir().unwrap();
            let source = root.path().join("team.secrets");
            let home = root.path().join("home");
            joined(&home, &source);
            let plan = Plan {
                vault_url: source.to_string_lossy().into_owned(),
                ..Plan::default()
            };
            let (sys, ctx) = fake(&home);
            let missing = match wait {
                Wait::Token => home.join(".secrets/gh_access_token"),
                Wait::CommandLineTools => {
                    Path::new(aterm_machines::items::github::CLT_GIT).to_path_buf()
                }
                _ => unreachable!(),
            };
            sys.stat_fails(missing.to_str().unwrap(), std::io::ErrorKind::NotFound);
            let seen = observe(&sys, &ctx, Some(&plan)).unwrap();
            // Each needs a person (a token in the vault, Apple's tools): never a wait the run
            // retries (System State design §3.6).
            assert_eq!(seen.standing, Standing::NeedsYou, "{wait:?}");
            assert_eq!(seen.note.as_deref(), Some(wait.note()), "{wait:?}");
            assert!(
                !sys.calls()
                    .iter()
                    .any(|call| call.contains(" agh GET ") || call.contains("credential")),
                "{wait:?}: {:?}",
                sys.calls()
            );
        }
    }

    /// A token GitHub refuses is a person's to renew in the vault, never a wait the run retries
    /// (System State design §3.6): the row needs you, signing in, with the remedy as its note,
    /// and the run's row for it parks the run instead of trying it again a minute later.
    #[test]
    fn a_token_github_refuses_needs_you_and_parks_the_run() {
        use aterm_machines::init_observe::Wait;
        use aterm_machines::port::GithubFailure;
        use aterm_types::macos_setup::Standing;

        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        joined(&home, &root.path().join("team.secrets"));
        let (sys, ctx) = fake(&home);
        sys.fail_github_api(&ctx.users[0], "/user", GithubFailure::Unauthenticated);
        // GitHub signs the token in once, then refuses it: it expired between two reads.
        let first = observe(&sys, &ctx, None).unwrap();
        assert_ne!(first.standing, Standing::NeedsYou, "{first:?}");
        assert!(
            sys.calls()
                .iter()
                .any(|call| call.ends_with(" agh GET /user")),
            "{:?}",
            sys.calls()
        );
        let seen = observe(&sys, &ctx, None).unwrap();
        assert_eq!(
            (seen.standing, seen.now.as_deref(), seen.note.as_deref()),
            (
                Standing::NeedsYou,
                Some("Needs sign-in"),
                Some(Wait::TokenRefused.note())
            )
        );
        let rows = run(&sys, &ctx, None, Mode::Plan).unwrap();
        assert!(
            !rows.is_empty() && rows.iter().all(|row| row.status == super::super::WAITING),
            "{rows:?}"
        );
        assert_eq!(
            super::super::SetupNext::of(&rows),
            super::super::SetupNext::Park
        );
    }

    /// A Mac with another number of user accounts than one is no Mac aterm sets repositories
    /// up on, whatever it tries again (System State design §3.6): the run says it failed for
    /// good, which parks it, and the row reads failed with the same words, asking nothing of
    /// GitHub or the vault.
    #[test]
    fn a_mac_with_two_user_accounts_fails_the_repositories_for_good() {
        use aterm_types::macos_setup::{FAILED_FOR_GOOD, FailureKind, Standing};

        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        joined(&home, &root.path().join("team.secrets"));
        let (sys, mut ctx) = fake(&home);
        let other = User {
            name: "other".into(),
            uid: 502,
            home: root.path().join("other").to_string_lossy().into_owned(),
            ..ctx.users[0].clone()
        };
        ctx.users.push(other);
        let rows = run(&sys, &ctx, None, Mode::Apply).unwrap();
        assert_eq!(
            rows.iter()
                .map(|row| (row.name.as_str(), row.status.as_str(), row.detail.as_str()))
                .collect::<Vec<_>>(),
            [("GitHub repositories", FAILED_FOR_GOOD, ONE_USER)]
        );
        assert_eq!(
            FailureKind::of_status(&rows[0].status),
            Some(FailureKind::ForGood)
        );
        let seen = observe(&sys, &ctx, None).unwrap();
        assert_eq!(
            (seen.standing, seen.detail.as_deref()),
            (Standing::Failed, Some(ONE_USER))
        );
        assert!(
            sys.calls().iter().all(|call| !call.contains(" agh ")),
            "{:?}",
            sys.calls()
        );
    }
}
