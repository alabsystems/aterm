// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native GitHub transport using `agh` and [`crate::git_credential`].

use super::{Github, INPUT_MAX, TokenUnread, decode, get, identity, read_token, repos};
use crate::engine::Place;
use crate::git_credential::{GITHUB, Git, Lookups};
use crate::init_observe::{LastReading, Reading, Wait};
use crate::items::{Ctx, Item};
use crate::model::{ItemReport, Mode, Probe, User};
use crate::port::{GithubError, GithubFailure, Secret, System};
use aterm_converge::{Class, Layer, Remedy, Requires};
use std::io;
use std::path::Path;

const ACCESSIBLE: &str = "/user/repos?visibility=all&per_page=30&sort=full_name&direction=asc";

pub(super) fn run(sys: &dyn System, ctx: &Ctx, mode: Mode) -> Result<Vec<ItemReport>, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("initial repository setup requires exactly one local user".into());
    };
    let item = NativeRepositories {
        user: user.clone(),
        reading: LastReading::default(),
    };
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
        let mut journal = sys.journal()?;
        aterm_converge::run(&bound, mode, journal.as_mut())
    } else {
        aterm_converge::run(&bound, mode, &mut aterm_converge::ReadOnly)
    }
    .map_err(|why| why.to_string())
}

struct NativeRepositories {
    user: User,
    /// How many repositories the last probe found and whether it signed in, for
    /// System State.
    reading: LastReading,
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
        self.reading.set(None);
        let check = (|| {
            let plan = self.plan(sys)?;
            let missing = repos::missing(sys, &self.user, &plan.repositories)
                .map_err(Block::of(Wait::InTheWay))?;
            let ready = lookups_answer(sys, &self.user, &plan.token)
                || (Git::keychain()
                    .configured(sys, &self.user, &plan.account)?
                    .ready()
                    && credential_ready(sys, &self.user, &plan.account, &plan.token)?);
            Ok::<_, Block>((plan.repositories.len(), missing.len(), ready))
        })();
        self.reading.set(match &check {
            Ok((count, missing, signed_in)) => Some(Reading::Repositories {
                present: count.saturating_sub(*missing),
                count: *count,
                signed_in: *signed_in,
            }),
            // Why it cannot go on, where this probe decided it: System State's note.
            Err(block) => block.wait.map(Reading::Waits),
        });
        match check {
            Ok((count, 0, true)) => Probe::Ok {
                now: format!("{count} matching worktrees present; native GitHub credentials verified"),
            },
            Ok((count, missing, _)) => Probe::Change {
                from: format!("{missing} of {count} repositories missing, or native GitHub credentials need setup"),
                to: "configure native GitHub credentials and clone missing repositories; preserve existing worktrees".into(),
            },
            Err(block) => Probe::Blocked { why: block.why },
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        let plan = self.plan(sys).map_err(|block| block.why)?;
        let missing = repos::missing(sys, &self.user, &plan.repositories)?;
        install_credentials(sys, &self.user, &plan.account, &plan.token)?;
        for repo in missing {
            repos::clone_one(sys, &self.user, &repo.owner, &repo)?;
        }
        Ok(())
    }

    fn detail(&self, _sys: &dyn System, _ctx: &Ctx) -> Option<String> {
        self.reading.detail()
    }
}

struct Plan {
    account: String,
    token: Secret,
    repositories: Vec<repos::Repo>,
}

/// Why [`NativeRepositories::plan`] cannot go on: the engine's line, and the reason System
/// State words for a person where the step that stopped decided one. A step that decided
/// none leaves the page its own words for a waiting row.
#[derive(Debug)]
struct Block {
    why: String,
    wait: Option<Wait>,
}

impl Block {
    /// A step that stopped for `wait`, with its line.
    fn of(wait: Wait) -> impl FnOnce(String) -> Self {
        move |why| Self {
            why,
            wait: Some(wait),
        }
    }

    /// A GitHub read that failed, by its kind: no answer, a token GitHub does not take, or
    /// anything else.
    fn github(error: GithubError) -> Self {
        Self::of(match error.failure {
            GithubFailure::Unreached => Wait::Unreached,
            GithubFailure::Unauthenticated => Wait::TokenRefused,
            GithubFailure::Other => Wait::GitHub,
        })(error.why)
    }

    /// The token source that could not be read: not there yet, or there and unusable.
    fn token(unread: TokenUnread) -> Self {
        Self::of(if unread.absent {
            Wait::Token
        } else {
            Wait::TokenUnusable
        })(unread.why)
    }
}

impl From<String> for Block {
    /// A step that decided no reason.
    fn from(why: String) -> Self {
        Self { why, wait: None }
    }
}

impl NativeRepositories {
    fn plan(&self, sys: &dyn System) -> Result<Plan, Block> {
        if sys.euid() != self.user.uid {
            return Err(String::from("repository setup must run as its local user").into());
        }
        if !repos::git_installed(sys) {
            return Err(Block::of(Wait::CommandLineTools)(repos::NO_GIT.into()));
        }
        repos::admit_home(sys, &self.user)?;
        crate::port::no_core_dumps();
        let configured = configuration(sys, &self.user).map_err(Block::of(Wait::Settings))?;
        let source = configured.clone().unwrap_or_else(|| Github {
            account: String::new(),
            owner: String::new(),
            token_file: Path::new(&self.user.home).join(".secrets/gh_access_token"),
            token_key: None,
        });
        let token = read_token(sys, &self.user, &source).map_err(Block::token)?;
        let account = identity(sys, &self.user, &token).map_err(Block::github)?;
        if configured
            .as_ref()
            .is_some_and(|config| !account.eq_ignore_ascii_case(&config.account))
        {
            return Err(Block::of(Wait::OtherAccount)(
                "the synced token belongs to a different configured GitHub account; nothing changed".into(),
            ));
        }
        let (base, owner) = if let Some(config) = &configured {
            if config.owner.eq_ignore_ascii_case(&account) {
                ("/user/repos?affiliation=owner&visibility=all&per_page=30&sort=full_name&direction=asc".to_owned(), Some(config.owner.as_str()))
            } else {
                let (owner, _) = get(sys, &self.user, &token, &format!("/users/{}", config.owner))
                    .map_err(Block::github)?;
                if owner["login"]
                    .as_str()
                    .is_none_or(|owner| !owner.eq_ignore_ascii_case(&config.owner))
                    || owner["type"].as_str() != Some("Organization")
                {
                    return Err(Block::of(Wait::Settings)(
                        "a different configured repository owner must be a GitHub organization"
                            .into(),
                    ));
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
        // A page that failed keeps its kind; one read whole but not usable as an
        // inventory is GitHub's answer that could not be used.
        let failed = std::cell::Cell::new(GithubFailure::Other);
        let repositories = repos::enumerate_pages(&base, owner, |endpoint| {
            get(sys, &self.user, &token, endpoint).map_err(|error| {
                failed.set(error.failure);
                error.why
            })
        })
        .map_err(|why| {
            Block::github(GithubError {
                failure: failed.get(),
                why,
            })
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

/// Whether the login keychain holds exactly `token` for `account` at github.com.
fn credential_ready(
    sys: &dyn System,
    user: &User,
    account: &str,
    token: &Secret,
) -> Result<bool, String> {
    Ok(Git::keychain()
        .fill(sys, Some(user), GITHUB, account)?
        .is_some_and(|stored| stored.bytes() == token.bytes()))
}

/// Whether git's own github.com lookups, through the helpers `user`'s configuration names and
/// asked as a clone asks, already hand back exactly `token` for every repository alike
/// ([`Git::uniform`]): the secrets vault's helper reads the synced token itself, so a rotation
/// reaches git with nothing here to redo, and the keychain is neither written nor made git's
/// only helper. A lookup git cannot run, or settings that ask for some repositories otherwise,
/// is no answer: the keychain path then runs as before and reports its own failure.
pub(super) fn lookups_answer(sys: &dyn System, user: &User, token: &Secret) -> bool {
    let git = Git::keychain();
    git.fill_as_configured(sys, Some(user), GITHUB)
        .is_ok_and(|answer| answer.is_some_and(|got| got.bytes() == token.bytes()))
        && git.uniform(sys, user).is_ok_and(|lookups| lookups.ready())
}

/// Store the synced token through `git credential approve`, unless git's own lookups already
/// answer it ([`lookups_answer`]).
fn install_credentials(
    sys: &dyn System,
    user: &User,
    account: &str,
    token: &Secret,
) -> Result<(), String> {
    if lookups_answer(sys, user, token) {
        return Ok(());
    }
    let git = Git::keychain();
    if !credential_ready(sys, user, account, token)? {
        git.store(sys, Some(user), GITHUB, account, token)?;
    }
    if !git.configured(sys, user, account)?.ready() {
        git.configure(sys, user, account)?;
        if let Lookups::Not(why) = git.configured(sys, user, account)? {
            return Err(format!(
                "native GitHub credentials did not read back after setup: {why}"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_credential::script;
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
        let (sys, item) = bare();
        sys.file(crate::items::github::CLT_GIT, b"synthetic git");
        source(&sys, "/Users//alice/.secrets/gh_access_token");
        answer(
            &sys,
            &item.user,
            "/user",
            aterm_json::json!({"login":"alice"}),
        );
        (sys, item)
    }

    /// Alice's own home and nothing else: no git, no token, no GitHub answer.
    fn bare() -> (Fake, NativeRepositories) {
        let mut sys = Fake::root();
        sys.euid = 501;
        let item = NativeRepositories {
            user: user("alice", 501),
            reading: LastReading::default(),
        };
        sys.stat_of(&item.user.home, stat(true, 0o700));
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
            && !call.contains("credential approve")));
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
                .all(|call| !call.contains("credential approve") && !call.contains(" clone "))
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
                .all(|call| !call.contains("credential approve")
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
        let block = item.plan(&sys).err().unwrap();
        assert!(block.why.contains("different configured"));
        assert!(!block.why.contains(TOKEN));
        assert_eq!(block.wait, Some(Wait::OtherAccount));
        assert!(
            !sys.calls()
                .iter()
                .any(|call| call.contains("/repos?") || call.contains("credential approve"))
        );
    }

    /// EVERY BLOCKED STEP SAYS WHY IN SYSTEM STATE'S WORDS (design §3.3): the probe
    /// decides the reason at the step that stopped, from what that step knows (no git, the
    /// token file absent or unusable, GitHub's kind of failure, another account, the
    /// settings, a folder in the way), never from the engine's line; the row the plan pass
    /// reads carries it, and the Repositories row's note is its words. A step that decides
    /// none gives no note here, and the page words that row itself.
    #[test]
    fn every_blocked_step_says_why_in_system_states_words() {
        use crate::init_observe::observation;
        use aterm_types::macos_setup::{Setting, Standing};
        let profile = "/Users//alice/.config/aterm/init.toml";
        let with_profile = |sys: &Fake, bytes: &[u8]| {
            sys.file(profile, bytes).stat_of(
                profile,
                Stat {
                    len: bytes.len() as u64,
                    ..stat(false, 0o600)
                },
            );
        };
        // Git and the token, without GitHub's answer to `/user`.
        let signed_out = || {
            let (sys, item) = bare();
            sys.file(crate::items::github::CLT_GIT, b"synthetic git");
            source(&sys, "/Users//alice/.secrets/gh_access_token");
            (sys, item)
        };
        let cases: Vec<(&str, (Fake, NativeRepositories), Option<Wait>)> = vec![
            ("no git", bare(), Some(Wait::CommandLineTools)),
            (
                "no token yet",
                {
                    let (sys, item) = bare();
                    sys.file(crate::items::github::CLT_GIT, b"synthetic git");
                    (sys, item)
                },
                Some(Wait::Token),
            ),
            (
                "a token others can read",
                {
                    let (sys, item) = signed_out();
                    sys.stat_of("/Users//alice/.secrets/gh_access_token", stat(false, 0o644));
                    (sys, item)
                },
                Some(Wait::TokenUnusable),
            ),
            (
                "GitHub unreached",
                {
                    let (sys, item) = signed_out();
                    sys.fail_github_api(&item.user, "/user", GithubFailure::Unreached);
                    (sys, item)
                },
                Some(Wait::Unreached),
            ),
            (
                "a token GitHub refuses",
                {
                    let (sys, item) = signed_out();
                    sys.fail_github_api(&item.user, "/user", GithubFailure::Unauthenticated);
                    (sys, item)
                },
                Some(Wait::TokenRefused),
            ),
            (
                "GitHub's other failure",
                {
                    let (sys, item) = signed_out();
                    sys.fail_github_api(&item.user, "/user", GithubFailure::Other);
                    (sys, item)
                },
                Some(Wait::GitHub),
            ),
            (
                "another account",
                {
                    let (sys, item) = fixture();
                    with_profile(
                        &sys,
                        b"[github]\naccount='someone-else'\nowner='someone-else'\ntoken_file='~/.secrets/gh_access_token'\n",
                    );
                    (sys, item)
                },
                Some(Wait::OtherAccount),
            ),
            (
                "settings it cannot read",
                {
                    let (sys, item) = fixture();
                    with_profile(&sys, b"[github]\naccount='alice'\n");
                    (sys, item)
                },
                Some(Wait::Settings),
            ),
            (
                "the inventory unreached",
                {
                    let (sys, item) = fixture();
                    sys.fail_github_api(
                        &item.user,
                        &format!("{ACCESSIBLE}&page=1"),
                        GithubFailure::Unreached,
                    );
                    (sys, item)
                },
                Some(Wait::Unreached),
            ),
            (
                "a file where a repository goes",
                {
                    let (sys, item) = fixture();
                    answer(
                        &sys,
                        &item.user,
                        &format!("{ACCESSIBLE}&page=1"),
                        aterm_json::json!([repository("alice", "work", 1)]),
                    );
                    sys.file("/Users//alice/work", b"not a worktree");
                    (sys, item)
                },
                Some(Wait::InTheWay),
            ),
            (
                "another login",
                {
                    let (mut sys, item) = fixture();
                    sys.euid = 502;
                    (sys, item)
                },
                None,
            ),
        ];
        for (case, (sys, item), wait) in cases {
            // The pass System State's reading runs: a plan.
            let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
            let rows = run(&sys, &context, Mode::Plan).unwrap();
            assert_eq!(rows[0].outcome, crate::model::Outcome::Blocked, "{case}");
            assert_eq!(
                Reading::of(&rows[0]),
                wait.map(Reading::Waits),
                "{case}: {rows:?}"
            );
            let seen = observation(Setting::Repositories, &[&rows[0]]);
            assert_eq!(seen.standing, Standing::Waiting, "{case}");
            assert_eq!(seen.note.as_deref(), wait.map(Wait::note), "{case}");
            let line = rows[0].why.as_deref().unwrap();
            assert!(
                seen.note.as_deref().is_none_or(|note| !note.contains(line)),
                "{case}"
            );
        }
    }

    /// git's github.com settings unset, then as `configure` leaves them.
    fn credential_commands(sys: &Fake, user: &User, keychain: &str) {
        script::unconfigured(sys, user);
        script::configured(sys, user, "alice");
        script::configure(sys, user, "alice");
        script::holds_nothing(sys, Some(user));
        script::holds(sys, Some(user), GITHUB, "alice", keychain);
        script::approves(sys, Some(user));
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
        credential_commands(&sys, &item.user, TOKEN);
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        item.apply(&sys, &context).unwrap();
        let calls = sys.calls();
        let approve = script::credential(Some(&item.user), "approve");
        let at = |key: &str| calls.iter().position(|call| call == key);
        assert!(at(&approve).is_some(), "{calls:?}");
        let writes = script::configure(&sys, &item.user, "alice");
        assert!(at(&approve) < at(&writes[0]), "{calls:?}");
        assert_eq!(
            calls
                .iter()
                .filter(|call| **call == script::helpers(&item.user))
                .count(),
            2,
            "{calls:?}"
        );
        assert!(calls.iter().all(|call| !call.contains(TOKEN)
            && !call.contains("/gh ")
            && !call.contains("credential-osxkeychain")
            && !call.contains("auth login")));
        assert!(sys.fed().is_empty());

        let (sys, item) = fixture();
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::json!([]),
        );
        script::configured(&sys, &item.user, "alice");
        script::holds(&sys, Some(&item.user), GITHUB, "alice", TOKEN);
        item.apply(&sys, &context).unwrap();
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.contains("credential approve") && !call.contains("--global")),
            "{:?}",
            sys.calls()
        );
    }

    #[test]
    fn an_apply_journals_in_the_journal_of_the_system_it_runs_on_and_a_plan_in_none() {
        let journaled = |sys: &Fake| -> Vec<String> {
            sys.written(Fake::JOURNAL)
                .map(|bytes| String::from_utf8(bytes).unwrap())
                .unwrap_or_default()
                .lines()
                .map(|line| {
                    let line: aterm_json::Value = aterm_json::from_str(line).unwrap();
                    line["id"].as_str().unwrap().to_owned()
                })
                .collect()
        };
        for mode in [Mode::Plan, Mode::Apply] {
            let (sys, item) = fixture();
            answer(
                &sys,
                &item.user,
                &format!("{ACCESSIBLE}&page=1"),
                aterm_json::json!([]),
            );
            credential_commands(&sys, &item.user, TOKEN);
            let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
            let rows = run(&sys, &context, mode).unwrap();
            let want = if mode == Mode::Apply {
                (crate::model::Outcome::Changed, vec![item.id()])
            } else {
                (crate::model::Outcome::Planned, Vec::new())
            };
            assert_eq!(
                (rows[0].outcome, journaled(&sys)),
                want,
                "{mode:?}: {rows:?}"
            );
        }
    }

    /// git's own lookups already answering the synced token alike for every repository (the
    /// secrets vault's helper, which reads it itself) are ready as they are: nothing goes into
    /// the keychain and git's settings are not rewritten, so the next rotation of the synced
    /// token reaches git. Ones that answer another token, or that some repositories' settings
    /// could answer otherwise, still get the keychain.
    #[test]
    fn lookups_that_already_answer_the_synced_token_are_kept_as_they_are() {
        let context = |item: &NativeRepositories| {
            crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user))
        };
        let (sys, item) = fixture();
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::json!([]),
        );
        script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
        script::alike(&sys, &item.user);
        assert!(
            matches!(item.probe(&sys, &context(&item)), Probe::Ok { .. }),
            "{:?}",
            sys.calls()
        );
        item.apply(&sys, &context(&item)).unwrap();
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.contains("credential approve") && !call.contains("--global")),
            "{:?}",
            sys.calls()
        );

        let (sys, item) = fixture();
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::json!([]),
        );
        script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
        let owners = format!("{}/.gitconfig", item.user.home);
        sys.on(
            &script::helpers(&item.user),
            Ran::ok(&script::printed(&[(
                &owners,
                "credential.https://github.com/alice.helper",
                "store",
            )])),
        );
        credential_commands(&sys, &item.user, TOKEN);
        item.apply(&sys, &context(&item)).unwrap();
        let approve = script::credential(Some(&item.user), "approve");
        assert!(sys.calls().contains(&approve), "{:?}", sys.calls());

        let (sys, item) = fixture();
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::json!([]),
        );
        script::lookups_hold(&sys, &item.user, GITHUB, "alice", "a_rotated_out_value");
        credential_commands(&sys, &item.user, TOKEN);
        item.apply(&sys, &context(&item)).unwrap();
        let approve = script::credential(Some(&item.user), "approve");
        assert!(sys.calls().contains(&approve), "{:?}", sys.calls());
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
        credential_commands(&sys, &item.user, "unrelated_private_value");
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let why = item.apply(&sys, &context).unwrap_err();
        assert!(why.contains("did not hand back"), "{why}");
        assert!(!why.contains("unrelated_private_value") && !why.contains(TOKEN));
        assert!(!sys.calls().iter().any(|call| call.contains(" clone ")));
    }
}
