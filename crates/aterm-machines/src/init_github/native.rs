// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The unified setup's native GitHub transport. agh owns authenticated HTTP;
//! Apple's keychain helper receives private stdin and clone URLs contain only IDs.

use super::{GIT, Github, INPUT_MAX, decode, login, read_token, repos, scrub};
use crate::engine::Place;
use crate::items::{Ctx, Item};
use crate::model::{ItemReport, Mode, Probe, User};
use crate::port::{Cmd, Secret, System};
use aterm_converge::{Class, Layer, Remedy, Requires};
use std::io;
use std::path::Path;
use std::time::Duration;

// GitHub's authenticated endpoint includes owned, collaborator and organization
// repositories. Never add `type`: GitHub refuses it alongside affiliation.
// https://docs.github.com/en/rest/repos/repos#list-repositories-for-the-authenticated-user
// Omit affiliation to use its documented all-three default; this also avoids
// comma-encoding differences in the API's pagination links.
const ACCESSIBLE: &str = "/user/repos?visibility=all&per_page=30&sort=full_name&direction=asc";
const HELPER: &str = "credential.https://github.com.helper";
const USERNAME: &str = "credential.https://github.com.username";
const USE_PATH: &str = "credential.https://github.com.useHttpPath";

pub(super) fn run(sys: &dyn System, ctx: &Ctx, mode: Mode) -> Result<Vec<ItemReport>, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("initial repository setup requires exactly one local user".into());
    };
    let item = NativeRepositories { user: user.clone() };
    let bound = crate::engine::bind(
        [(
            &item as &dyn Item,
            Place {
                layer: Layer::Workstation,
                class: Class::Install,
                requires: Requires::Console,
                remedy: Remedy::Command("aterm init".into()),
            },
        )],
        sys,
        ctx,
    );
    if mode == Mode::Apply {
        aterm_converge::run(&bound, mode, &mut crate::engine::account_journal()?)
    } else {
        // A plan or a check changes nothing, so it records nothing: the
        // read-only journal, which refuses a record (`NoJournal` is a test's
        // and is not in a release build).
        aterm_converge::run(&bound, mode, &mut aterm_converge::ReadOnly)
    }
    .map_err(|why| why.to_string())
}

struct NativeRepositories {
    user: User,
}

impl Item for NativeRepositories {
    fn id(&self) -> String {
        format!("init.github.native@{}", self.user.name)
    }
    fn family(&self) -> &'static str {
        "github"
    }
    fn title(&self) -> String {
        "GitHub repositories from synced development credentials".into()
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        let check = (|| {
            let plan = self.plan(sys)?;
            let missing = repos::missing(sys, &self.user, &plan.repositories)?;
            let ready = helper_ready(sys, &self.user, &plan.account)?
                && credential_ready(sys, &self.user, &plan.account, &plan.token)?;
            Ok::<_, String>((plan.repositories.len(), missing.len(), ready))
        })();
        match check {
            Ok((count, 0, true)) => Probe::Ok {
                now: format!("{count} matching worktrees present; native GitHub credentials verified"),
            },
            Ok((count, missing, _)) => Probe::Change {
                from: format!("{missing} of {count} repositories missing, or native GitHub credentials need setup"),
                to: "configure native GitHub credentials and clone missing repositories; preserve existing worktrees".into(),
            },
            Err(why) => Probe::Blocked { why },
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        let plan = self.plan(sys)?;
        // Resolve every name/collision before persisting credentials or cloning.
        let missing = repos::missing(sys, &self.user, &plan.repositories)?;
        if !helper_ready(sys, &self.user, &plan.account)?
            || !credential_ready(sys, &self.user, &plan.account, &plan.token)?
        {
            install_credentials(sys, &self.user, &plan.account, &plan.token)?;
        }
        for repo in missing {
            repos::clone_one(sys, &self.user, &repo.owner, &repo)?;
        }
        Ok(())
    }
}

struct Plan {
    account: String,
    token: Secret,
    repositories: Vec<repos::Repo>,
}

impl NativeRepositories {
    fn plan(&self, sys: &dyn System) -> Result<Plan, String> {
        if sys.euid() != self.user.uid {
            return Err("repository setup must run as its local user".into());
        }
        repos::admit_home(sys, &self.user)?;
        crate::port::no_core_dumps();
        let configured = configuration(sys, &self.user)?;
        let source = configured.clone().unwrap_or_else(|| Github {
            account: String::new(),
            owner: String::new(),
            token_file: Path::new(&self.user.home).join(".secrets/gh_access_token"),
            token_key: None,
        });
        let token = read_token(sys, &self.user, &source)?;
        let (identity, _) = api(sys, &self.user, &token, "/user")?;
        let account = identity["login"]
            .as_str()
            .filter(|account| login(account))
            .ok_or("GitHub returned an invalid authenticated account")?
            .to_owned();
        if configured
            .as_ref()
            .is_some_and(|config| !account.eq_ignore_ascii_case(&config.account))
        {
            return Err("the synced token belongs to a different configured GitHub account; nothing changed".into());
        }
        let (base, owner) = if let Some(config) = &configured {
            if config.owner.eq_ignore_ascii_case(&account) {
                ("/user/repos?affiliation=owner&visibility=all&per_page=30&sort=full_name&direction=asc".to_owned(), Some(config.owner.as_str()))
            } else {
                let (owner, _) = api(sys, &self.user, &token, &format!("/users/{}", config.owner))?;
                if owner["login"]
                    .as_str()
                    .is_none_or(|owner| !owner.eq_ignore_ascii_case(&config.owner))
                    || owner["type"].as_str() != Some("Organization")
                {
                    return Err(
                        "a different configured repository owner must be a GitHub organization"
                            .into(),
                    );
                }
                (
                    format!(
                        "/orgs/{}/repos?type=all&per_page=30&sort=full_name&direction=asc",
                        config.owner
                    ),
                    Some(config.owner.as_str()),
                )
            }
        } else {
            (ACCESSIBLE.into(), None)
        };
        let repositories = repos::enumerate_pages(&base, owner, |endpoint| {
            api(sys, &self.user, &token, endpoint)
        })?;
        Ok(Plan {
            account,
            token,
            repositories,
        })
    }
}

fn configuration(sys: &dyn System, user: &User) -> Result<Option<Github>, String> {
    let path = Path::new(&user.home).join(".config/aterm/init.toml");
    let bytes = match sys.read_owned_file(&path, user.uid, INPUT_MAX, false) {
        Ok(bytes) => bytes,
        Err(why) if why.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(
                "cannot safely read ~/.config/aterm/init.toml; no repositories changed".into(),
            );
        }
    };
    let configured = decode(&bytes, user)?.github;
    if configured.as_ref().is_some_and(|config| {
        config
            .token_file
            .file_name()
            .is_some_and(|name| name == "gh_access_token_alabsystems")
    }) {
        return Err("public release credentials are not a development repository source".into());
    }
    Ok(configured)
}

fn api(
    sys: &dyn System,
    user: &User,
    token: &Secret,
    endpoint: &str,
) -> Result<repos::Response, String> {
    let reply = sys.github_api(user, token, endpoint)?;
    repos::response_parts(reply.status, reply.headers, &reply.body)
}

fn git(user: &User) -> Cmd {
    Cmd::new(GIT)
        .as_user(user)
        .env("GIT_TERMINAL_PROMPT", "0")
        .budget(Duration::from_secs(60))
}

fn helper_ready(sys: &dyn System, user: &User, account: &str) -> Result<bool, String> {
    let helper = sys
        .run_complete_stdout(&git(user).args(["config", "--global", "--get-all", HELPER]))
        .map_err(|_| "cannot inspect native GitHub credential helper")?;
    if helper.status == Some(1) && helper.stdout.is_empty() {
        return Ok(false);
    }
    if !helper.success() {
        return Err("cannot inspect native GitHub credential helper".into());
    }
    if helper.stdout != b"\nosxkeychain\n" {
        return Ok(false);
    }
    let username = sys
        .run_complete_stdout(&git(user).args(["config", "--global", "--get", USERNAME]))
        .map_err(|_| "cannot inspect native GitHub account selection")?;
    if !(username.success()
        && username
            .stdout
            .strip_suffix(b"\n")
            .unwrap_or(&username.stdout)
            .eq_ignore_ascii_case(account.as_bytes()))
    {
        return Ok(false);
    }
    // The stored token is host-wide. A generic useHttpPath=true must not make
    // Git request an absent per-repository keychain entry during cloning.
    let use_path = sys
        .run_complete_stdout(&git(user).args(["config", "--global", "--get", USE_PATH]))
        .map_err(|_| "cannot inspect native GitHub credential path selection")?;
    Ok(use_path.success() && use_path.stdout == b"false\n")
}

fn credential_input(account: &str) -> Vec<u8> {
    format!("protocol=https\nhost=github.com\nusername={account}\n").into_bytes()
}

fn credential_ready(
    sys: &dyn System,
    user: &User,
    account: &str,
    token: &Secret,
) -> Result<bool, String> {
    let mut input = credential_input(account);
    input.push(b'\n');
    let result =
        sys.run_complete_stdout_input(&git(user).args(["credential-osxkeychain", "get"]), &input);
    scrub(&mut input);
    let mut ran =
        result.map_err(|_| "native GitHub keychain read did not complete (output withheld)")?;
    let mut passwords = ran
        .stdout
        .split(|byte| *byte == b'\n')
        .filter_map(|line| line.strip_prefix(b"password="));
    let ready =
        ran.success() && passwords.next() == Some(token.bytes()) && passwords.next().is_none();
    scrub(&mut ran.stdout);
    scrub(&mut ran.stderr);
    Ok(ready)
}

fn install_credentials(
    sys: &dyn System,
    user: &User,
    account: &str,
    token: &Secret,
) -> Result<(), String> {
    let mut input = credential_input(account);
    input.extend_from_slice(b"password=");
    input.extend_from_slice(token.bytes());
    input.extend_from_slice(b"\n\n");
    let result =
        sys.run_complete_stdout_input(&git(user).args(["credential-osxkeychain", "store"]), &input);
    scrub(&mut input);
    let mut stored =
        result.map_err(|_| "native GitHub keychain write did not complete (output withheld)")?;
    let success = stored.success();
    scrub(&mut stored.stdout);
    scrub(&mut stored.stderr);
    if !success {
        return Err("native GitHub keychain write failed (output withheld)".into());
    }
    for args in [
        vec!["config", "--global", "--replace-all", HELPER, ""],
        vec!["config", "--global", "--add", HELPER, "osxkeychain"],
        vec!["config", "--global", "--replace-all", USERNAME, account],
        vec!["config", "--global", "--replace-all", USE_PATH, "false"],
    ] {
        let result = sys
            .run_complete_stdout(&git(user).args(args))
            .map_err(|_| "native GitHub git configuration did not complete")?;
        if !result.success() {
            return Err("native GitHub git configuration failed (output withheld)".into());
        }
    }
    if !helper_ready(sys, user, account)? || !credential_ready(sys, user, account, token)? {
        return Err(
            "native GitHub credentials did not read back correctly (output withheld)".into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::items::defaults::tests::user;
    use crate::port::{Fake, GithubResponse, Ran, Stat};

    const TOKEN: &str = "fixture_development_token_only";

    fn stat(directory: bool, mode: u32) -> Stat {
        Stat {
            uid: 501,
            gid: 20,
            mode,
            dir: directory,
            symlink: false,
            len: TOKEN.len() as u64,
        }
    }

    fn fixture() -> (Fake, NativeRepositories) {
        let mut sys = Fake::root();
        sys.euid = 501;
        let item = NativeRepositories {
            user: user("alice", 501),
        };
        sys.stat_of(&item.user.home, stat(true, 0o700));
        sys.file(GIT, b"synthetic git");
        source(&sys, "/Users//alice/.secrets/gh_access_token");
        answer(
            &sys,
            &item.user,
            "/user",
            aterm_json::json!({"login":"alice"}),
        );
        (sys, item)
    }

    fn source(sys: &Fake, path: &str) {
        sys.file(path, TOKEN.as_bytes())
            .stat_of(path, stat(false, 0o600));
    }

    fn answer(sys: &Fake, user: &User, endpoint: &str, body: aterm_json::Value) {
        sys.on_github_api(
            user,
            endpoint,
            Ok(GithubResponse {
                status: 200,
                headers: Vec::new(),
                body: aterm_json::to_vec(&body).unwrap(),
            }),
        );
    }

    fn repository(owner: &str, name: &str, id: u64) -> aterm_json::Value {
        aterm_json::json!({"id":id,"name":name,"owner":{"login":owner},"full_name":format!("{owner}/{name}"),"private":true,"clone_url":format!("https://github.com/{owner}/{name}.git"),"default_branch":"main"})
    }

    #[test]
    fn default_credential_infers_identity_and_inventories_all_accessible_owners_without_gh() {
        let (sys, item) = fixture();
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::json!([
                repository("alice", "personal", 1),
                repository("team", "work", 2),
            ]),
        );
        let plan = item.plan(&sys).unwrap();
        assert_eq!(plan.account, "alice");
        assert_eq!(
            plan.repositories
                .iter()
                .map(|repo| (repo.owner.as_str(), repo.name.as_str()))
                .collect::<Vec<_>>(),
            [("alice", "personal"), ("team", "work")]
        );
        assert!(sys.calls().iter().all(|call| !call.contains("/gh ")
            && !call.contains("auth login")
            && !call.contains(TOKEN)
            && !call.contains("alabsystems")
            && !call.contains("release-credentials")));
        assert!(
            sys.fed().is_empty(),
            "the complete-input pipe retains no token copy"
        );
    }

    #[test]
    fn complete_inventory_rejects_cross_owner_home_collisions_before_mutation() {
        let (sys, item) = fixture();
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::json!([
                repository("alice", "shared", 1),
                repository("team", "SHARED", 2),
            ]),
        );
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let why = item.apply(&sys, &context).unwrap_err();
        assert!(why.contains("colliding"));
        assert!(sys.calls().iter().all(|call| !call.contains(" clone ")
            && !call.contains("--replace-all")
            && !call.contains("credential-osxkeychain")));
    }

    #[test]
    fn native_inventory_follows_agh_pagination_before_any_mutation() {
        let (sys, item) = fixture();
        let page = (0..30)
            .map(|index| repository("alice", &format!("repo-{index}"), index + 1))
            .collect::<Vec<_>>();
        let next = format!("https://api.github.com{ACCESSIBLE}&page=2");
        sys.on_github_api(
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            Ok(GithubResponse {
                status: 200,
                headers: vec![(
                    "Link".into(),
                    format!("<{next}>; rel=\"next\", <{next}>; rel=\"last\""),
                )],
                body: aterm_json::to_vec(&page).unwrap(),
            }),
        );
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=2"),
            aterm_json::json!([repository("team", "work", 31)]),
        );
        let plan = item.plan(&sys).unwrap();
        assert_eq!(plan.repositories.len(), 31);
        assert_eq!(plan.repositories.last().unwrap().owner, "team");
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.contains("credential-osxkeychain") && !call.contains(" clone "))
        );
    }

    #[test]
    fn failed_native_inventory_never_persists_credentials_or_clones() {
        let (sys, item) = fixture();
        sys.on_github_api(
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            Err("synthetic incomplete GitHub response".into()),
        );
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        assert!(
            item.apply(&sys, &context)
                .unwrap_err()
                .contains("incomplete")
        );
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.contains("credential-osxkeychain")
                    && !call.contains("--replace-all")
                    && !call.contains(" clone "))
        );
    }

    #[test]
    fn explicit_admitted_profile_uses_its_source_and_scope_and_refuses_identity_mismatch() {
        let (sys, item) = fixture();
        let profile = "/Users//alice/.config/aterm/init.toml";
        let bytes = b"[github]\naccount='alice'\nowner='team'\ntoken_file='~/.secrets/custom_development'\n";
        sys.file(profile, bytes).stat_of(
            profile,
            Stat {
                len: bytes.len() as u64,
                ..stat(false, 0o600)
            },
        );
        source(&sys, "/Users//alice/.secrets/custom_development");
        answer(
            &sys,
            &item.user,
            "/users/team",
            aterm_json::json!({"login":"team","type":"Organization"}),
        );
        answer(
            &sys,
            &item.user,
            "/orgs/team/repos?type=all&per_page=30&sort=full_name&direction=asc&page=1",
            aterm_json::json!([repository("team", "work", 1)]),
        );
        assert_eq!(item.plan(&sys).unwrap().repositories[0].owner, "team");
        assert!(!sys.calls().iter().any(|call| call.contains(ACCESSIBLE)));

        let (sys, item) = fixture();
        let bytes = b"[github]\naccount='someone-else'\nowner='someone-else'\ntoken_file='~/.secrets/gh_access_token'\n";
        sys.file(profile, bytes).stat_of(
            profile,
            Stat {
                len: bytes.len() as u64,
                ..stat(false, 0o600)
            },
        );
        let why = item.plan(&sys).err().unwrap();
        assert!(why.contains("different configured"));
        assert!(!why.contains(TOKEN));
        assert!(
            !sys.calls()
                .iter()
                .any(|call| call.contains("/repos?") || call.contains("credential-osxkeychain"))
        );
    }

    fn missing_helper() -> Ran {
        Ran {
            status: Some(1),
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    }

    fn credential_commands(sys: &Fake, user: &User, keychain: Ran) {
        let helper = git(user)
            .args(["config", "--global", "--get-all", HELPER])
            .display();
        sys.on(&helper, missing_helper())
            .on(&helper, Ran::ok("\nosxkeychain\n"));
        sys.on(
            &git(user)
                .args(["config", "--global", "--get", USERNAME])
                .display(),
            Ran::ok("alice\n"),
        );
        sys.on(
            &git(user)
                .args(["config", "--global", "--get", USE_PATH])
                .display(),
            Ran::ok("false\n"),
        );
        sys.on(
            &git(user)
                .args(["credential-osxkeychain", "store"])
                .display(),
            Ran::ok(""),
        );
        sys.on(
            &git(user).args(["credential-osxkeychain", "get"]).display(),
            keychain,
        );
        for args in [
            vec!["config", "--global", "--replace-all", HELPER, ""],
            vec!["config", "--global", "--add", HELPER, "osxkeychain"],
            vec!["config", "--global", "--replace-all", USERNAME, "alice"],
            vec!["config", "--global", "--replace-all", USE_PATH, "false"],
        ] {
            sys.on(&git(user).args(args).display(), Ran::ok(""));
        }
    }

    #[test]
    fn path_scoped_credentials_are_not_ready_for_the_host_wide_token() {
        let (sys, item) = fixture();
        sys.on(
            &git(&item.user)
                .args(["config", "--global", "--get-all", HELPER])
                .display(),
            Ran::ok("\nosxkeychain\n"),
        );
        sys.on(
            &git(&item.user)
                .args(["config", "--global", "--get", USERNAME])
                .display(),
            Ran::ok("alice\n"),
        );
        sys.on(
            &git(&item.user)
                .args(["config", "--global", "--get", USE_PATH])
                .display(),
            Ran::ok("true\n"),
        );
        assert!(!helper_ready(&sys, &item.user, "alice").unwrap());
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.contains("credential-osxkeychain"))
        );
    }

    #[test]
    fn native_keychain_configuration_is_verified_and_secret_never_enters_command_records() {
        let (sys, item) = fixture();
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::json!([]),
        );
        credential_commands(
            &sys,
            &item.user,
            Ran::ok(&format!("username=alice\npassword={TOKEN}\n")),
        );
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        item.apply(&sys, &context).unwrap();
        let calls = sys.calls();
        assert!(
            calls
                .iter()
                .any(|call| call.contains("credential-osxkeychain store"))
        );
        assert!(
            calls
                .iter()
                .any(|call| call.contains("credential-osxkeychain get"))
        );
        assert!(calls.iter().all(|call| !call.contains(TOKEN)
            && !call.contains("/gh ")
            && !call.contains("auth login")));
        assert!(sys.fed().is_empty());
    }

    #[test]
    fn native_keychain_readback_failure_stops_before_any_clone_and_withholds_output() {
        let (sys, item) = fixture();
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::json!([]),
        );
        credential_commands(
            &sys,
            &item.user,
            Ran::ok("password=unrelated_private_value\n"),
        );
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let why = item.apply(&sys, &context).unwrap_err();
        assert!(why.contains("read back"));
        assert!(!why.contains("unrelated_private_value") && !why.contains(TOKEN));
        assert!(!sys.calls().iter().any(|call| call.contains(" clone ")));
    }
}
