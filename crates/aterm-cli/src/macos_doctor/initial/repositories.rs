// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The selected vault's repository inputs and their admission to GitHub work.

use super::{Plan, Row, Stage, item_rows, readiness_row};
use aterm_machines::init_github::{RepositoryReadback, RepositoryRun};
use aterm_machines::items::Ctx;
use aterm_machines::model::Mode;
use aterm_machines::port::System;
use std::sync::Arc;

type Current = Arc<dyn Fn() -> Result<bool, String> + Send + Sync>;

pub(super) fn run(
    sys: &dyn System,
    ctx: &Ctx,
    vault: Option<&Plan>,
    mode: Mode,
    readback: Option<Arc<RepositoryReadback>>,
) -> Result<Vec<Row>, String> {
    items_current(sys, ctx, vault, mode, false, None, readback).map(|rows| reading(rows).rows)
}

/// What a Repositories stage [`run_reading_guarded`] ran came to.
pub(super) struct Ran {
    pub rows: Vec<Row>,
    /// The reason a part that failed gives ([`failure_note`]).
    pub failure: Option<&'static str>,
    /// It stopped for want of a GitHub token to sign in with ([`want_token`]).
    pub token: bool,
    /// Actual native empty-origin evidence owned only by this setup host/pass.
    pub readback: Option<Arc<RepositoryReadback>>,
}

/// Run with the current setup choice checked at each clone boundary, including when the
/// Secrets group is left as it is and the inputs are this account's local files.
pub(super) fn run_reading_guarded(
    sys: &dyn System,
    ctx: &Ctx,
    vault: Option<&Plan>,
    mode: Mode,
    current: impl Fn() -> Result<bool, String> + Send + Sync + 'static,
) -> Result<Ran, String> {
    let current: Current = Arc::new(current);
    let result = items_current(
        sys,
        ctx,
        vault,
        mode,
        false,
        Some(Arc::clone(&current)),
        None,
    );
    if let Err(why) = current().and_then(|ready| {
        ready
            .then_some(())
            .ok_or_else(|| "repository setup was held while cloning".to_owned())
    }) {
        return Ok(Ran {
            rows: vec![Row {
                name: Stage::Repositories.name().into(),
                status: super::WAITING.into(),
                detail: why,
                elapsed_ms: None,
            }],
            failure: None,
            token: false,
            readback: None,
        });
    }
    result.map(reading)
}

fn reading(rows: Option<RepositoryRun>) -> Ran {
    match rows {
        Some(run) => Ran {
            failure: failure_note(&run.rows),
            token: want_token(&run.rows),
            rows: item_rows(run.rows),
            readback: run.readback.map(Arc::new),
        },
        None => Ran {
            rows: vec![readiness_row(
                Stage::Repositories.name(),
                false,
                "the selected secrets vault changed or is still syncing; repository inputs wait for a complete version".into(),
            )],
            failure: None,
            token: false,
            readback: None,
        },
    }
}

/// Why the repositories' item `rows` failed, where the step that stopped decided it: the
/// reading their item took, in System State's words (`GitHub can't be reached`), never the
/// engine's English, which `aterm doctor` and the log keep (System State design §3.6).
pub(super) fn failure_note(rows: &[aterm_machines::model::ItemReport]) -> Option<&'static str> {
    aterm_machines::init_observe::failure_note(&rows.iter().collect::<Vec<_>>())
}

/// Whether the repositories' item `rows` stopped because this Mac has no GitHub token to sign in
/// with: the file the vault's sync writes, or the one init.toml's `[github]` names, is not there
/// ([`aterm_machines::init_observe::Wait::Token`]).
pub(super) fn want_token(rows: &[aterm_machines::model::ItemReport]) -> bool {
    use aterm_machines::init_observe::{Reading, Wait};
    rows.iter()
        .any(|row| Reading::of(row) == Some(Reading::Waits(Wait::Token)))
}

/// The GitHub repositories row, read as [`run`] reads them for `vault`. Where the ALab vault is off
/// this network (`not_here`) and this Mac is not in `vault`, they are read on this Mac's own GitHub
/// credentials, as their stage then runs them, and wait for the vault's token, as the stage does,
/// only where this Mac has none to sign in with.
pub(super) fn observe(
    sys: &dyn System,
    ctx: &Ctx,
    vault: Option<&Plan>,
    not_here: bool,
) -> Result<aterm_types::macos_setup::Observation, String> {
    use aterm_types::macos_setup::{Observation, Setting, Standing, Subject};
    Ok(match items(sys, ctx, vault, Mode::Plan, not_here)? {
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

/// The repositories' item rows, read through `vault` where one is given, else on this Mac's own
/// GitHub credentials: `None` while that vault is not this Mac's, changed, or is still syncing.
/// Where it is not this Mac's and the Mac signs in on its own credentials instead (`own`), they are
/// read with those, `None` where it has no token to sign in with ([`want_token`]).
fn items(
    sys: &dyn System,
    ctx: &Ctx,
    vault: Option<&Plan>,
    mode: Mode,
    own: bool,
) -> Result<Option<RepositoryRun>, String> {
    items_current(sys, ctx, vault, mode, own, None, None)
}

fn local_items(
    sys: &dyn System,
    ctx: &Ctx,
    mode: Mode,
    current: Option<Current>,
    readback: Option<Arc<RepositoryReadback>>,
) -> Result<RepositoryRun, String> {
    if current.is_some()
        && let Some(rows) = withdrawal_items(sys, ctx, mode, current.clone(), None)?
    {
        return Ok(rows);
    }
    if current.is_none() && readback.is_none() {
        return aterm_machines::init_github::run_initial_repositories(sys, ctx, mode);
    }
    let work = if readback.is_some() {
        aterm_secrets::converge::RepositoryWork::Repair
    } else {
        aterm_secrets::converge::RepositoryWork::Refresh
    };
    let inputs = aterm_machines::init_github::InitialRepositoryInputs::capture_ready(sys, ctx)?
        .with_repository_readback(readback);
    aterm_machines::init_github::run_initial_repositories_captured(
        sys,
        ctx,
        mode,
        work,
        inputs,
        move || current.as_ref().map_or(Ok(true), |current| current()),
    )
}

fn withdrawal_items(
    sys: &dyn System,
    ctx: &Ctx,
    mode: Mode,
    current: Option<Current>,
    selected: Option<(
        &aterm_secrets::client::Paths,
        &aterm_secrets::client::Config,
    )>,
) -> Result<Option<RepositoryRun>, String> {
    let Ok(Some(withdrawal)) =
        aterm_machines::init_github::RepositoryCredentialWithdrawal::capture(sys, ctx)
    else {
        return Ok(None);
    };
    let selected = selected.map(|(paths, config)| (paths.clone(), config.clone()));
    aterm_machines::init_github::run_repository_credential_withdrawal(
        sys,
        ctx,
        mode,
        withdrawal,
        move || {
            if let Some(current) = &current
                && !current()?
            {
                return Ok(false);
            }
            match &selected {
                Some((paths, expected)) => Ok(aterm_secrets::client::config(paths)
                    .map_err(|why| why.to_string())?
                    .as_ref()
                    == Some(expected)),
                None => Ok(true),
            }
        },
    )
    .map(|rows| {
        Some(RepositoryRun {
            rows,
            readback: None,
        })
    })
}

fn items_current(
    sys: &dyn System,
    ctx: &Ctx,
    vault: Option<&Plan>,
    mode: Mode,
    own: bool,
    current: Option<Current>,
    readback: Option<Arc<RepositoryReadback>>,
) -> Result<Option<RepositoryRun>, String> {
    let Some(vault) = vault else {
        return local_items(sys, ctx, mode, current, readback).map(Some);
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
        if !own {
            return Ok(None);
        }
        return local_items(sys, ctx, mode, current, readback)
            .map(|rows| (!want_token(&rows)).then_some(rows));
    };
    // This selected vault still admits its current local empty/disabled intent when the
    // paired credential has not arrived. This path grants no ordinary repository effects.
    if let Some(rows) = withdrawal_items(sys, ctx, mode, current.clone(), Some((&paths, expected)))?
    {
        return Ok(Some(rows));
    }
    let entries = aterm_machines::init_github::uses_vault_inventory(sys, ctx)?
        .then_some(aterm_machines::init_github::VAULT_INPUT_ENTRIES.as_slice());
    let read = || {
        aterm_machines::init_github::InitialRepositoryInputs::capture(sys, ctx)
            .map_err(aterm_secrets::Error::new)
    };
    let captured = match entries {
        Some(entries) => aterm_secrets::client::with_materialization_ready_for_entries(
            &paths, expected, entries, read,
        ),
        None => aterm_secrets::client::with_materialization_ready(&paths, expected, read),
    }
    .map_err(|error| error.to_string())?;
    let Some(captured) = captured else {
        return Ok(None);
    };
    let work = if readback.is_some() {
        aterm_secrets::converge::RepositoryWork::Repair
    } else {
        aterm_secrets::converge::RepositoryWork::Refresh
    };
    aterm_machines::init_github::run_initial_repositories_captured(
        sys,
        ctx,
        mode,
        work,
        captured.inputs.with_repository_readback(readback),
        move || {
            if let Some(current) = &current
                && !current()?
            {
                return Ok(false);
            }
            captured
                .snapshot
                .is_current_now(&paths)
                .map_err(|error| error.to_string())
        },
    )
    .map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_machines::model::{HostFacts, User};
    use aterm_machines::port::{Fake, GithubResponse, Ran, Stat};
    use aterm_secrets::client::{self, JoinOptions, Paths};
    use aterm_types::macos_setup::{Selection, Setting};
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::Path;
    use std::time::Duration;

    const TOKEN: &str = "fixture_repository_token_only";
    const INVENTORY: &str = r#"{"schema":2,"account":"alice","repositories":[{"id":1,"name":"fixture","owner":{"login":"alice"},"full_name":"alice/fixture","private":true,"clone_url":"https://github.com/alice/fixture.git","default_branch":"main"}]}"#;

    fn joined(home: &Path, source: &Path) -> Paths {
        joined_inventory(home, source, INVENTORY)
    }

    fn joined_inventory(home: &Path, source: &Path, inventory: &str) -> Paths {
        std::fs::write(
            source,
            format!(
                "gh_access_token={TOKEN}\ngithub-repositories.json={inventory}\ntool-other=before\n"
            ),
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
                password: aterm_secrets::client::JoinPassword::Given(None),
                fetched: None,
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
        sys.logged_in(&user);
        let stat = |dir, len| Stat {
            uid: user.uid,
            gid: user.gid,
            mode: if dir { 0o700 } else { 0o600 },
            dir,
            symlink: false,
            len,
        };
        sys.stat_of(home.to_str().unwrap(), stat(true, 0));
        sys.stat_of(
            aterm_machines::items::github::CLT_GIT,
            Stat {
                mode: 0o755,
                ..stat(false, 1)
            },
        );
        // git's credential settings hold nothing of an earlier init's.
        sys.simulate(|_, command| {
            command
                .display()
                .contains(" config --null --show-origin --get-regexp ")
                .then(|| Ran::ok(""))
        });
        let token = home.join(".secrets/gh_access_token");
        let bytes = std::fs::read(&token).unwrap();
        sys.file(token.to_str().unwrap(), &bytes)
            .stat_of(token.to_str().unwrap(), stat(false, bytes.len() as u64));
        let inventory = home.join(".secrets/github-repositories.json");
        match std::fs::read(&inventory) {
            Ok(bytes) => {
                sys.file(inventory.to_str().unwrap(), &bytes)
                    .stat_of(inventory.to_str().unwrap(), stat(false, bytes.len() as u64));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("reading fixture inventory: {error}"),
        }
        for (endpoint, body) in [
            ("/user", aterm_json::json!({"login":"alice"})),
            (
                "/user/repos?affiliation=owner&visibility=all&per_page=30&sort=full_name&direction=asc&page=1",
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
        let other = home.join(".secrets/tool-other");
        std::fs::remove_file(&other).unwrap();
        std::fs::create_dir(&other).unwrap();
        std::fs::write(
            &source,
            "gh_access_token=fixture_new_repository_token\ntool-other=after\n",
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
        let _result = run(&sys, &ctx, Some(&plan), Mode::Plan, None);
        assert!(
            !sys.calls().iter().any(|call| call.contains(" agh GET ")),
            "incomplete selected vault reached GitHub: {:?}",
            sys.calls()
        );
    }

    #[test]
    fn leaving_the_secrets_group_as_is_still_checks_the_configured_default_vault() {
        let (_root, _plan, sys, ctx) = after_sync_failure();
        let ran = run_reading_guarded(&sys, &ctx, None, Mode::Apply, || Ok(true)).unwrap();
        assert!(
            ran.rows
                .iter()
                .any(|row| row.status == "blocked" || row.status == super::super::WAITING),
            "an unready source is unfinished: {:?}",
            ran.rows
        );
        assert!(
            !sys.calls()
                .iter()
                .any(|call| call.contains(" clone ") || call.contains(" agh GET "))
        );
    }

    #[test]
    fn leaving_secrets_as_is_uses_the_existing_local_repository_token() {
        let (_root, _plan, sys, ctx) = after_sync_failure();
        let _result = run(&sys, &ctx, None, Mode::Plan, None);
        assert!(!sys.calls().iter().any(|call| call.contains(" agh GET ")));
    }

    #[test]
    fn a_ready_selected_vault_allows_repository_observation() {
        let root = aterm_tempfile::tempdir().unwrap();
        let source = root.path().join("team.secrets");
        let home = root.path().join("home");
        let paths = joined(&home, &source);
        let plan = Plan {
            vault_url: source.to_string_lossy().into_owned(),
            ..Plan::default()
        };
        let (sys, ctx) = fake(&home);
        let result = items(&sys, &ctx, Some(&plan), Mode::Plan, false)
            .unwrap()
            .unwrap();
        assert!(
            result.iter().any(|row| matches!(
                aterm_machines::init_observe::Reading::of(row),
                Some(aterm_machines::init_observe::Reading::Repositories { count: 1, .. })
            )),
            "{result:?}"
        );
        assert!(!sys.calls().iter().any(|call| call.contains(" agh GET ")));

        // Apply's actual empty advertisement must survive this adapter until the same
        // host's fresh final observation. Independent observations retain no such fact.
        use aterm_machines::port::{Cmd, Live};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let live = Live::new();
        let git = aterm_machines::items::github::installed_git_without_atpkg(&live).unwrap_or_else(
            || {
                aterm_uds::host::could_not_run(
                    "native Git is not installed for the private repository fixture",
                )
            },
        );
        let (mut sys, mut ctx) = fake(&home);
        sys.euid = live.euid();
        sys.linux = !live.macos();
        let user = &mut ctx.users[0];
        user.uid = live.euid();
        user.gid = live.stat(&home).unwrap().gid;
        ctx.console = Some(user.clone());
        sys.logged_in(user)
            .executable(git.to_str().unwrap(), b"selected fixture Git");
        let launcher = aterm_types::installed_command::legacy(&home)[0].clone();
        sys.executable(launcher.to_str().unwrap(), b"fixture launcher");
        let user = user.clone();
        let command = || {
            Cmd::new(&git)
                .as_user(&user)
                .env("HOME", home.to_string_lossy())
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_NO_LAZY_FETCH", "1")
        };
        let bare = root.path().join("empty.git");
        let checkout = home.join("fixture");
        for cmd in [
            command()
                .args([
                    "init",
                    "--quiet",
                    "--bare",
                    "--template=",
                    "--initial-branch=main",
                ])
                .arg(&bare),
            command()
                .args(["clone", "--quiet", "--template="])
                .arg(&bare)
                .arg(&checkout),
            command().arg("-C").arg(&checkout).args([
                "remote",
                "set-url",
                "origin",
                "https://github.com/alice/fixture.git",
            ]),
        ] {
            let ran = live.run_complete_stdout(&cmd).unwrap();
            assert!(ran.success(), "{}: {}", cmd.display(), ran.describe());
        }
        for path in [
            home.clone(),
            home.join(".secrets/gh_access_token"),
            home.join(".secrets/github-repositories.json"),
            checkout.clone(),
            checkout.join(".git"),
            checkout.join(".git/objects"),
        ] {
            sys.stat_of(path.to_str().unwrap(), live.stat(&path).unwrap());
            sys.file_id_of(&path, live.file_id(&path).unwrap());
        }
        let advertisements = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&advertisements);
        let at = checkout.clone();
        let native_git = git.clone();
        sys.simulate(move |_, cmd| {
            if cmd.args.iter().any(|arg| arg == "fill") {
                return Some(Ran::ok(&format!(
                    "protocol=https\nhost=github.com\nusername=alice\npassword={TOKEN}\n"
                )));
            }
            let displayed = cmd.display();
            if displayed.contains(" config --null --show-origin --get-regexp ") {
                return Some(Ran::ok(""));
            }
            let in_checkout = cmd
                .args
                .windows(2)
                .any(|pair| pair[0] == "-C" && pair[1] == at.as_os_str());
            if !in_checkout
                && !cmd
                    .args
                    .iter()
                    .any(|arg| arg == "https://github.com/alice/fixture.git")
            {
                return None;
            }
            let remote = cmd.args.iter().any(|arg| arg == "ls-remote")
                && !cmd.args.iter().any(|arg| arg == "--get-url");
            let mut cmd = cmd.clone();
            // The Fake's selected tool executes this fixture's admitted native Git,
            // including on a Mac whose fallback comes from Xcode rather than the CLT.
            cmd.program = native_git.clone();
            if remote {
                seen.fetch_add(1, Ordering::SeqCst);
                for arg in &mut cmd.args {
                    if arg == "origin" || arg == "https://github.com/alice/fixture.git" {
                        *arg = bare.as_os_str().to_owned();
                    }
                }
            }
            Some(Live::new().run_complete_stdout(&cmd).unwrap())
        });
        let applied =
            run_reading_guarded(&sys, &ctx, Some(&plan), Mode::Apply, || Ok(true)).unwrap();
        assert!(
            super::super::rows_ready(&applied.rows),
            "{:?}",
            applied.rows
        );
        let proof = applied
            .readback
            .expect("actual native empty admission crossed the adapter");
        let current = run(
            &sys,
            &ctx,
            Some(&plan),
            Mode::Plan,
            Some(Arc::clone(&proof)),
        )
        .unwrap();
        assert!(super::super::rows_ready(&current), "{current:?}");
        assert_eq!(
            advertisements.load(Ordering::SeqCst),
            1,
            "the final owned observation repeats no advertisement"
        );
        let independent = run(&sys, &ctx, Some(&plan), Mode::Plan, None).unwrap();
        assert!(super::super::rows_ready(&independent), "{independent:?}");
        assert_eq!(
            advertisements.load(Ordering::SeqCst),
            2,
            "an independent admitted observation needs a fresh full advertisement"
        );
        let held = run_reading_guarded(&sys, &ctx, Some(&plan), Mode::Apply, || Ok(false)).unwrap();
        assert!(held.readback.is_none());
        assert!(!super::super::rows_ready(&held.rows), "{:?}", held.rows);
        assert_eq!(
            advertisements.load(Ordering::SeqCst),
            2,
            "owned read-back and held work repeat no acquisition"
        );
        assert!(
            !live
                .run_complete_stdout(&command().arg("-C").arg(&checkout).args([
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    "HEAD^{commit}"
                ]))
                .unwrap()
                .success(),
            "read-only empty observation never hydrates a checkout"
        );
        let changed_inventory = INVENTORY.replace("\"id\":1", "\"id\":2");
        std::fs::write(&source, format!("gh_access_token={TOKEN}\ngithub-repositories.json={changed_inventory}\ntool-other=before\n")).unwrap();
        client::sync(&paths, None).unwrap();
        let inventory = home.join(".secrets/github-repositories.json");
        sys.file(
            inventory.to_str().unwrap(),
            &std::fs::read(&inventory).unwrap(),
        )
        .stat_of(inventory.to_str().unwrap(), live.stat(&inventory).unwrap());
        let changed = run(&sys, &ctx, Some(&plan), Mode::Plan, Some(proof)).unwrap();
        assert!(!super::super::rows_ready(&changed), "{changed:?}");
        assert_eq!(
            advertisements.load(Ordering::SeqCst),
            2,
            "a stale owned receipt does not admit a new acquisition"
        );
    }

    #[test]
    fn an_empty_selected_inventory_needs_no_git_or_repository_work() {
        use aterm_types::macos_setup::Standing;

        let root = aterm_tempfile::tempdir().unwrap();
        let source = root.path().join("team.secrets");
        let home = root.path().join("home");
        let paths = joined_inventory(
            &home,
            &source,
            r#"{"schema":2,"account":"alice","repositories":[]}"#,
        );
        let plan = Plan {
            vault_url: source.to_string_lossy().into_owned(),
            ..Plan::default()
        };
        let (sys, ctx) = fake(&home);
        sys.stat_fails(
            aterm_machines::items::github::CLT_GIT,
            std::io::ErrorKind::NotFound,
        );
        let seen = observe(&sys, &ctx, Some(&plan), false).unwrap();
        assert_eq!(seen.standing, Standing::NotNeeded, "{seen:?}");
        assert!(
            !sys.calls().iter().any(|call| call.contains(" agh GET ")
                || call.contains("credential fill")
                || call.contains(" clone ")),
            "{:?}",
            sys.calls()
        );
        // The selected-vault admission must still reach local cleanup when its default
        // credential has not arrived. This is an unpublished intent, not delivery proof.
        aterm_secrets::need::test_alab_policy(&paths);
        std::fs::write(&source, "github-repositories.json={\"schema\":2,\"account\":\"alice\",\"repositories\":[]}\ntool-other=before\n").unwrap();
        client::sync(&paths, None).unwrap();
        sys.remove(&home.join(".secrets/gh_access_token")).unwrap();
        let pending = aterm_secrets::need::raise(
            &paths,
            aterm_secrets::need::Want::Gh(aterm_secrets::need::GhProfile::Dev),
            aterm_secrets::need::Purpose::AghDev,
        )
        .unwrap();
        assert!(!pending.state.ended());
        let ledger = paths.dir.join(aterm_secrets::need::LEDGER);
        let before = std::fs::read(&ledger).unwrap();
        let planned = items(&sys, &ctx, Some(&plan), Mode::Plan, false)
            .unwrap()
            .unwrap();
        assert_eq!(planned.len(), 1);
        assert_eq!(std::fs::read(&ledger).unwrap(), before);
        let held = run_reading_guarded(&sys, &ctx, Some(&plan), Mode::Apply, || Ok(false)).unwrap();
        assert!(!held.token);
        assert_eq!(std::fs::read(&ledger).unwrap(), before);
        let rows = items(&sys, &ctx, Some(&plan), Mode::Apply, false)
            .unwrap()
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].outcome, aterm_machines::model::Outcome::Changed);
        let retired = aterm_secrets::need::peek(&paths).unwrap().remove(0);
        assert_eq!(retired.id, pending.id);
        assert_eq!(retired.state, aterm_secrets::need::State::Retired);
        assert_eq!(
            retired.retired.unwrap().reason,
            aterm_secrets::need::Reason::NotNeeded
        );
        assert!(
            !sys.calls()
                .iter()
                .any(|call| call.contains("credential fill")
                    || call.contains("/user")
                    || call.contains(" clone "))
        );
    }

    #[test]
    fn a_ready_vault_keeps_the_repository_inputs_typed_blocker() {
        use aterm_machines::init_observe::Wait;
        use aterm_types::macos_setup::Standing;

        for (wait, standing) in [
            (Wait::Token, Standing::NeedsYou),
            (Wait::GitReadiness, Standing::Waiting),
        ] {
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
                Wait::GitReadiness => {
                    Path::new(aterm_machines::items::github::CLT_GIT).to_path_buf()
                }
                _ => unreachable!(),
            };
            sys.stat_fails(missing.to_str().unwrap(), std::io::ErrorKind::NotFound);
            let seen = observe(&sys, &ctx, Some(&plan), false).unwrap();
            // A missing token needs its vault entry; Git readiness is retried automatically.
            assert_eq!(seen.standing, standing, "{wait:?}");
            assert_eq!(seen.note.as_deref(), Some(wait.note()), "{wait:?}");
            assert!(
                !sys.calls()
                    .iter()
                    .any(|call| call.contains(" agh GET ") || call.contains("credential fill")),
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
        // A personal explicit scope still authenticates its independently configured source.
        let profile = home.join(".config/aterm/init.toml");
        let profile_bytes = b"[github]\naccount='alice'\n";
        sys.file(profile.to_str().unwrap(), profile_bytes).stat_of(
            profile.to_str().unwrap(),
            Stat {
                uid: 501,
                gid: 20,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: profile_bytes.len() as u64,
            },
        );
        sys.fail_github_api(&ctx.users[0], "/user", GithubFailure::Unauthenticated);
        // GitHub signs the token in once, then refuses it: it expired between two reads.
        let first = observe(&sys, &ctx, None, false).unwrap();
        assert_ne!(first.standing, Standing::NeedsYou, "{first:?}");
        assert!(
            sys.calls()
                .iter()
                .any(|call| call.ends_with(" agh GET /user")),
            "{:?}",
            sys.calls()
        );
        let seen = observe(&sys, &ctx, None, false).unwrap();
        assert_eq!(
            (seen.standing, seen.now.as_deref(), seen.note.as_deref()),
            (
                Standing::NeedsYou,
                Some("Needs sign-in"),
                Some(Wait::TokenRefused.note())
            )
        );
        // The settings row before it has nothing to do, so the stage's own row decides the park.
        let rows = run(&sys, &ctx, None, Mode::Plan, None).unwrap();
        assert_eq!(
            rows.last().map(|row| row.status.as_str()),
            Some(super::super::WAITING),
            "{rows:?}"
        );
        assert_eq!(
            super::super::SetupNext::of(&rows),
            super::super::SetupNext::Park
        );
    }

    /// The repositories go into the home of the account signed in, on a Mac with any number of
    /// accounts: setup's context names that account alone. A context that names more is a
    /// caller's mistake, which the stage refuses as an error the run tries again like any other
    /// failure (System State design §3.6), never as one no try changes, and it asks nothing of
    /// GitHub or the vault.
    #[test]
    fn a_context_naming_two_accounts_is_an_error_the_run_retries() {
        let root = aterm_tempfile::tempdir().unwrap();
        let source = root.path().join("team.secrets");
        let home = root.path().join("home");
        joined(&home, &source);
        let (sys, mut ctx) = fake(&home);
        let other = User {
            name: "other".into(),
            uid: 502,
            home: root.path().join("other").to_string_lossy().into_owned(),
            ..ctx.users[0].clone()
        };
        ctx.users.push(other);
        let plan = Plan {
            vault_url: source.to_string_lossy().into_owned(),
            ..Plan::default()
        };
        for vault in [None, Some(&plan)] {
            let why = run(&sys, &ctx, vault, Mode::Apply, None).unwrap_err();
            assert!(why.contains("exactly one"), "{why}");
            assert!(observe(&sys, &ctx, vault, false).is_err());
        }
        assert!(
            sys.calls().iter().all(|call| !call.contains(" agh ")),
            "{:?}",
            sys.calls()
        );
    }
}
