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
use aterm_types::macos_setup::FailureKind;
use std::io;
use std::path::Path;
use std::sync::Arc;

const ACCESSIBLE: &str = "/user/repos?visibility=all&per_page=30&sort=full_name&direction=asc";

/// The step a person takes for the GitHub sign-in or settings a repository needs, as the
/// engine names it beside their remedy.
const GITHUB_STEP: &str = "github.sign-in";

pub(super) fn run(sys: &dyn System, ctx: &Ctx, mode: Mode) -> Result<Vec<ItemReport>, String> {
    run_with(sys, ctx, mode, None)
}

pub(super) fn run_captured(
    sys: &dyn System,
    ctx: &Ctx,
    mode: Mode,
    inputs: InitialRepositoryInputs,
    current: Box<dyn Fn() -> Result<bool, String> + Send + Sync>,
) -> Result<Vec<ItemReport>, String> {
    run_with(sys, ctx, mode, Some(Captured { inputs, current }))
}

fn run_with(
    sys: &dyn System,
    ctx: &Ctx,
    mode: Mode,
    captured: Option<Captured>,
) -> Result<Vec<ItemReport>, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("initial repository setup requires exactly one local user".into());
    };
    let item = NativeRepositories {
        user: user.clone(),
        reading: LastReading::default(),
        captured,
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
    captured: Option<Captured>,
}

/// One local GitHub profile/token reading or its typed blocker, without contacting
/// GitHub. Its secret is scrubbed when the final owner drops it; it is never part
/// of a command.
#[derive(Clone)]
pub struct InitialRepositoryInputs {
    user: User,
    local: Result<LocalInputs, Block>,
}

#[derive(Clone)]
struct LocalInputs {
    configured: Option<Github>,
    token: Arc<Secret>,
}

struct Captured {
    inputs: InitialRepositoryInputs,
    current: Box<dyn Fn() -> Result<bool, String> + Send + Sync>,
}

impl InitialRepositoryInputs {
    /// Read only local admitted profile/token files, for a caller holding its
    /// vault's ready materialization against writers. A blocked local reading is
    /// kept for the item's probe to report without reading those files again.
    /// # Errors
    /// The context does not name exactly one local user.
    pub fn capture(sys: &dyn System, ctx: &Ctx) -> Result<Self, String> {
        let [user] = ctx.users.as_slice() else {
            return Err("initial repository setup requires exactly one local user".into());
        };
        Ok(Self {
            user: user.clone(),
            local: Self::read(sys, user),
        })
    }

    fn read(sys: &dyn System, user: &User) -> Result<LocalInputs, Block> {
        crate::port::no_core_dumps();
        if sys.euid() != user.uid {
            return Err(String::from("repository setup must run as its local user").into());
        }
        if !repos::git_installed(sys) {
            return Err(Block::of(Wait::CommandLineTools)(repos::NO_GIT.into()));
        }
        repos::admit_home(sys, user)?;
        let configured = configuration(sys, user).map_err(Block::of(Wait::Settings))?;
        let source = configured.clone().unwrap_or_else(|| Github {
            account: String::new(),
            owner: String::new(),
            token_file: Path::new(&user.home).join(".secrets/gh_access_token"),
            token_key: None,
        });
        let token = Arc::new(read_token(sys, user, &source).map_err(Block::token)?);
        Ok(LocalInputs { configured, token })
    }
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
                || (Git::keychain(sys)
                    .configured(sys, &self.user, &plan.account)?
                    .ready()
                    && credential_ready(sys, &self.user, &plan.account, &plan.token)?);
            self.ensure_current().map_err(Block::from)?;
            // Signed in, with no repository left that fits: the row waits for room instead of
            // a pass that would clone nothing.
            let volume = sys.volume(Path::new(&self.user.home));
            let left: Vec<String> = missing
                .iter()
                .filter_map(|repo| repos::no_room(volume.as_ref(), repo))
                .collect();
            if ready && !missing.is_empty() && left.len() == missing.len() {
                let why =
                    repos::short_of_room(volume.as_ref(), &left, CHECK_AGAIN).unwrap_or_default();
                return Err(Block::of(Wait::DiskSpace)(why));
            }
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
            Err(block) => block.probe(),
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        let plan = self.plan(sys).map_err(|block| {
            // Why it stopped, where this step decided it: the failed row keeps it for System
            // State's page (`init_observe::failure_note`), the engine's line for the log.
            if let Some(wait) = block.wait {
                self.reading.set(Some(Reading::Waits(wait)));
            }
            block.why
        })?;
        let missing = repos::missing(sys, &self.user, &plan.repositories)?;
        let current = || self.ensure_current();
        current()?;
        install_credentials(sys, &self.user, &plan.account, &plan.token, &current)?;
        if self.captured.is_some() && !missing.is_empty() {
            // A clone must not ask a helper that can read a half-written newer
            // synced token. The invocation's pinned keychain helper uses this
            // captured value, even when the normal live helper is already good.
            current()?;
            if !credential_ready(sys, &self.user, &plan.account, &plan.token)? {
                current()?;
                Git::keychain(sys).store(
                    sys,
                    Some(&self.user),
                    GITHUB,
                    &plan.account,
                    &plan.token,
                )?;
            }
        }
        let home = Path::new(&self.user.home);
        let mut left = Vec::new();
        for repo in missing {
            if let Some(name) = repos::no_room(sys.volume(home).as_ref(), &repo) {
                left.push(name);
                continue;
            }
            if self.captured.is_some() {
                repos::clone_one_guarded(sys, &self.user, &repo, &plan.account, &current)?;
            } else {
                repos::clone_one(sys, &self.user, &repo.owner, &repo)?;
            }
        }
        current()?;
        let Some(why) = repos::short_of_room(sys.volume(home).as_ref(), &left, CHECK_AGAIN) else {
            return Ok(());
        };
        // The failed row keeps the page's words for it (`init_observe::failure_note`).
        self.reading.set(Some(Reading::Waits(Wait::DiskSpace)));
        Err(why)
    }

    fn detail(&self, _sys: &dyn System, _ctx: &Ctx) -> Option<String> {
        self.reading.detail()
    }
}

struct Plan {
    account: String,
    token: Arc<Secret>,
    repositories: Vec<repos::Repo>,
}

/// Why [`NativeRepositories::plan`] cannot go on: the engine's line, and the reason System
/// State words for a person where the step that stopped decided one. A step that decided
/// none leaves the page its own words for a waiting row.
#[derive(Clone, Debug)]
struct Block {
    why: String,
    wait: Option<Wait>,
}

impl Block {
    /// The probe's answer for this block (System State design §3.6): a person's step where only
    /// their act ends the wait ([`Wait::kind`]), so its row needs them and no pass retries it,
    /// else a wait that ends by itself.
    fn probe(self) -> Probe {
        match self.wait {
            Some(wait) if wait.kind() == FailureKind::NeedsPerson => Probe::Person {
                step: GITHUB_STEP.into(),
                how: self.why,
            },
            _ => Probe::Blocked { why: self.why },
        }
    }

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

/// What a first-launch pass that left repositories for room tells the person to do once there is
/// room ([`repos::short_of_room`]): `aterm init` clones nothing without a `[github]` profile,
/// and System State's run tries again by itself only for its first tries.
const CHECK_AGAIN: &str = "press Check again in System State, or launch aterm again";

impl NativeRepositories {
    fn ensure_current(&self) -> Result<(), String> {
        if let Some(captured) = &self.captured
            && !(captured.current)()?
        {
            return Err("the selected secrets vault changed or is still syncing; repository setup waits for a new ready snapshot".into());
        }
        Ok(())
    }

    fn plan(&self, sys: &dyn System) -> Result<Plan, Block> {
        self.ensure_current().map_err(Block::from)?;
        let inputs = match &self.captured {
            Some(captured) if captured.inputs.user == self.user => captured.inputs.local.clone()?,
            Some(_) => return Err("the captured repository account changed".to_owned().into()),
            None => InitialRepositoryInputs::read(sys, &self.user)?,
        };
        let LocalInputs { configured, token } = inputs;
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
        self.ensure_current().map_err(Block::from)?;
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
    Ok(Git::keychain(sys)
        .fill(sys, Some(user), GITHUB, account)?
        .is_some_and(|stored| stored.bytes() == token.bytes()))
}

/// Whether git's own github.com lookups, through the helpers `user`'s configuration names and
/// asked as a clone asks, already hand back exactly `token` for every repository alike
/// ([`Git::uniform`]). The vault's helper then reads the synced token itself, so a rotation
/// reaches git with nothing to redo, and the keychain is neither written nor made git's only
/// helper. A lookup git cannot run, or settings that answer some repositories otherwise, is no
/// answer: the keychain path runs.
pub(super) fn lookups_answer(sys: &dyn System, user: &User, token: &Secret) -> bool {
    let git = Git::keychain(sys);
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
    current: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    if lookups_answer(sys, user, token) {
        return Ok(());
    }
    let git = Git::keychain(sys);
    if !credential_ready(sys, user, account, token)? {
        current()?;
        git.store(sys, Some(user), GITHUB, account, token)?;
    }
    if !git.configured(sys, user, account)?.ready() {
        current()?;
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
            captured: None,
        };
        sys.stat_of(&item.user.home, stat(true, 0o700));
        (sys, item)
    }

    fn source(sys: &Fake, path: &str) {
        sys.file(path, TOKEN.as_bytes())
            .stat_of(path, stat(false, 0o600));
    }

    #[test]
    fn a_captured_local_blocker_is_not_reread_after_capture() {
        let (sys, item) = bare();
        sys.file(crate::items::github::CLT_GIT, b"synthetic git");
        let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let captured = InitialRepositoryInputs::capture(&sys, &ctx).unwrap();
        // A later local reading could succeed. The frozen capture must still
        // report its original blocker, without reading files again.
        source(&sys, "/Users//alice/.secrets/gh_access_token");
        answer(
            &sys,
            &item.user,
            "/user",
            aterm_json::json!({"login":"alice"}),
        );
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::json!([]),
        );
        let rows = run_captured(&sys, &ctx, Mode::Plan, captured, Box::new(|| Ok(true))).unwrap();
        assert_eq!(Reading::of(&rows[0]), Some(Reading::Waits(Wait::Token)));
        // The vault this Mac reads holds no GitHub token: only a person puts one there.
        assert_eq!(rows[0].outcome, crate::model::Outcome::Person);
        assert!(!sys.calls().iter().any(|call| call.contains(" agh GET ")));
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

    /// Every blocked step's reason is decided where it stopped, never from the engine's line, and
    /// the Repositories row's note is its words; a step that decides none gives no note. Where
    /// only a person ends it (a sign-in, aterm's GitHub settings, a folder in the way, Apple's
    /// Command Line Tools), the step is theirs and the row needs them, with their remedy.
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
                "no room on the disk",
                {
                    let (mut sys, item) = fixture();
                    sys.volume = Some(volume(9 * GIB));
                    answer(&sys, &item.user, &format!("{ACCESSIBLE}&page=1"), huge());
                    script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
                    script::alike(&sys, &item.user);
                    (sys, item)
                },
                Some(Wait::DiskSpace),
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
            // Only a person ends a wait for their sign-in, settings or folder: their step, which
            // parks a run (design §3.6); GitHub not answering ends by itself.
            let person = wait.is_some_and(|wait| wait.kind() == FailureKind::NeedsPerson);
            let (outcome, standing) = if person {
                (crate::model::Outcome::Person, Standing::NeedsYou)
            } else {
                (crate::model::Outcome::Blocked, Standing::Waiting)
            };
            assert_eq!(rows[0].outcome, outcome, "{case}");
            assert_eq!(
                Reading::of(&rows[0]),
                wait.map(Reading::Waits),
                "{case}: {rows:?}"
            );
            let seen = observation(Setting::Repositories, &[&rows[0]]);
            assert_eq!(seen.standing, standing, "{case}");
            assert_eq!(seen.note.as_deref(), wait.map(Wait::note), "{case}");
            let line = rows[0].why.as_deref().unwrap();
            assert!(
                seen.note.as_deref().is_none_or(|note| !note.contains(line)),
                "{case}"
            );
        }
    }

    /// A step that fails as it applies keeps the reason it decided (System State design §3.6):
    /// GitHub not answering as the run lists the repositories leaves the failed row the page's
    /// words for it (`init_observe::failure_note`), never GitHub's own.
    #[test]
    fn an_apply_github_did_not_answer_keeps_the_reason_it_decided() {
        let (sys, item) = bare();
        sys.file(crate::items::github::CLT_GIT, b"synthetic git");
        source(&sys, "/Users//alice/.secrets/gh_access_token");
        sys.fail_github_api(&item.user, "/user", GithubFailure::Unreached);
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        assert!(item.apply(&sys, &context).is_err(), "GitHub did not answer");
        assert_eq!(
            item.detail(&sys, &context),
            Some(Reading::Waits(Wait::Unreached).token())
        );
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

    /// Lookups that already answer the synced token alike for every repository are kept: nothing
    /// goes into the keychain and git's settings are not rewritten. Ones that answer another
    /// token, or that some repositories' settings could answer otherwise, still get the keychain.
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

    const GIB: u64 = aterm_capacity::floor::GIB;

    /// A data volume of 100 GiB with `free` bytes free and a floor of 8 GiB.
    fn volume(free: u64) -> aterm_capacity::reading::Volume {
        aterm_capacity::reading::Volume {
            mount: "/System/Volumes/Data".into(),
            total: 100 * GIB,
            free,
            floor: 8 * GIB,
            room: 8 * GIB,
        }
    }

    /// One repository GitHub sizes at 2 GiB, whose clone takes about 10 GiB.
    fn huge() -> aterm_json::Value {
        aterm_json::json!([{"id":1,"name":"huge","owner":{"login":"alice"},
            "full_name":"alice/huge","private":true,
            "clone_url":"https://github.com/alice/huge.git",
            "default_branch":"main","size":2 * 1024 * 1024}])
    }

    /// The first-launch inventory leaves a repository the disk has no room for above the disk
    /// watch's floor and names it; with no reading of the volume it tries the clone. Before
    /// the sign-in is set up, the plan still changes, so the credentials are set up.
    #[test]
    fn a_repository_the_disk_has_no_room_for_is_not_cloned() {
        let (mut sys, item) = fixture();
        sys.volume = Some(volume(12 * GIB));
        answer(&sys, &item.user, &format!("{ACCESSIBLE}&page=1"), huge());
        credential_commands(&sys, &item.user, TOKEN);
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        assert!(
            matches!(item.probe(&sys, &context), Probe::Change { .. }),
            "{:?}",
            sys.calls()
        );

        // One that fits beside one that does not: the plan changes, so the one that fits arrives.
        let (mut sys, item) = fixture();
        sys.volume = Some(volume(12 * GIB));
        let mut both = huge();
        both.as_array_mut()
            .unwrap()
            .push(repository("alice", "small", 2));
        answer(&sys, &item.user, &format!("{ACCESSIBLE}&page=1"), both);
        script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
        script::alike(&sys, &item.user);
        let probed = item.probe(&sys, &context);
        assert!(matches!(probed, Probe::Change { .. }), "{probed:?}");

        for read in [true, false] {
            let (mut sys, item) = fixture();
            sys.volume = read.then(|| volume(12 * GIB));
            answer(&sys, &item.user, &format!("{ACCESSIBLE}&page=1"), huge());
            script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
            script::alike(&sys, &item.user);
            let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
            // Room comes back by itself, so the row waits and the run tries again; it never
            // parks the run as a person's step would (design §3.6).
            let probed = item.probe(&sys, &context);
            assert_eq!(matches!(probed, Probe::Blocked { .. }), read, "{probed:?}");
            let why = item.apply(&sys, &context).unwrap_err();
            let tried = sys.calls().iter().any(|call| call.contains("alice/huge"));
            if read {
                assert_eq!(
                    why,
                    "1 repository not cloned, for want of disk space: ~/huge (about 10 GiB); \
                     /System/Volumes/Data has 12 GiB free and keeps 8 GiB free; free space, \
                     then press Check again in System State, or launch aterm again"
                );
                assert!(!tried, "{:?}", sys.calls());
            } else {
                assert!(tried, "{why}: {:?}", sys.calls());
            }
        }

        // The disk fills between the plan and the pass: the failed row keeps the reason the
        // pass decided, for the page (`init_observe::failure_note`).
        let (mut sys, item) = fixture();
        sys.volume = Some(volume(60 * GIB));
        answer(&sys, &item.user, &format!("{ACCESSIBLE}&page=1"), huge());
        script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
        script::alike(&sys, &item.user);
        let probed = item.probe(&sys, &context);
        assert!(matches!(probed, Probe::Change { .. }), "{probed:?}");
        sys.volume = Some(volume(12 * GIB));
        assert!(item.apply(&sys, &context).is_err());
        assert_eq!(
            item.detail(&sys, &context),
            Some(Reading::Waits(Wait::DiskSpace).token())
        );

        // A vault snapshot that went stale outranks the room: the run waits for the new one,
        // whose repositories and token may differ, instead of for disk space.
        let (mut sys, mut item) = fixture();
        sys.volume = Some(volume(12 * GIB));
        let inputs = InitialRepositoryInputs::capture(&sys, &context).unwrap();
        answer(&sys, &item.user, &format!("{ACCESSIBLE}&page=1"), huge());
        script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
        script::alike(&sys, &item.user);
        let sys = Arc::new(sys);
        let observed = Arc::clone(&sys);
        // Stale from the sign-in lookup on, which the plan's own checks come before.
        item.captured = Some(Captured {
            inputs,
            current: Box::new(move || {
                Ok(!observed
                    .calls()
                    .iter()
                    .any(|call| call.contains("credential fill")))
            }),
        });
        match item.probe(&*sys, &context) {
            Probe::Blocked { why } => assert!(why.contains("vault changed"), "{why}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(item.detail(&*sys, &context), None);
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

    #[test]
    fn captured_repository_inputs_do_not_reread_a_new_synced_token() {
        let (sys, mut item) = fixture();
        let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let inputs = InitialRepositoryInputs::capture(&sys, &ctx).unwrap();
        let token_path = format!("{}/.secrets/gh_access_token", item.user.home);
        let newer = "fixture_partial_sync_token_only";
        sys.file(&token_path, newer.as_bytes()).stat_of(
            &token_path,
            Stat {
                len: newer.len() as u64,
                ..stat(false, 0o600)
            },
        );
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::json!([]),
        );
        item.captured = Some(Captured {
            inputs,
            current: Box::new(|| Ok(true)),
        });
        let plan = item.plan(&sys).unwrap();
        assert_eq!(plan.token.bytes(), TOKEN.as_bytes());
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.contains(TOKEN) && !call.contains(newer))
        );
    }

    #[test]
    fn a_vault_version_changed_during_inventory_refuses_all_local_effects() {
        let (sys, mut item) = fixture();
        let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let inputs = InitialRepositoryInputs::capture(&sys, &ctx).unwrap();
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::json!([]),
        );
        let sys = Arc::new(sys);
        let observed = Arc::clone(&sys);
        item.captured = Some(Captured {
            inputs,
            current: Box::new(move || {
                Ok(!observed
                    .calls()
                    .iter()
                    .any(|call| call.contains("/user/repos?")))
            }),
        });
        let why = item.apply(&*sys, &ctx).unwrap_err();
        assert!(why.contains("changed"), "{why}");
        assert!(sys.calls().iter().any(|call| call.contains("/user/repos?")));
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.contains("credential approve")
                    && !call.contains("--global")
                    && !call.contains(" clone "))
        );
    }

    #[test]
    fn a_ready_captured_run_without_clones_preserves_compatible_live_helpers() {
        let (sys, mut item) = fixture();
        let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let inputs = InitialRepositoryInputs::capture(&sys, &ctx).unwrap();
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::json!([]),
        );
        script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
        script::alike(&sys, &item.user);
        item.captured = Some(Captured {
            inputs,
            current: Box::new(|| Ok(true)),
        });
        item.apply(&sys, &ctx).unwrap();
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.contains("credential approve") && !call.contains("--global"))
        );
    }

    #[test]
    fn captured_clones_store_the_snapshot_through_private_keychain_input_only() {
        let (sys, mut item) = fixture();
        let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let inputs = InitialRepositoryInputs::capture(&sys, &ctx).unwrap();
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::json!([repository("alice", "new", 1)]),
        );
        script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
        script::alike(&sys, &item.user);
        script::holds_nothing(&sys, Some(&item.user));
        script::holds(&sys, Some(&item.user), GITHUB, "alice", TOKEN);
        script::approves(&sys, Some(&item.user));
        item.captured = Some(Captured {
            inputs,
            current: Box::new(|| Ok(true)),
        });
        // The snapshot reaches the keychain; this fake intentionally supplies
        // no remote URL command, so no clone can start.
        assert!(item.apply(&sys, &ctx).is_err());
        let approve = script::credential(Some(&item.user), "approve");
        assert!(sys.calls().contains(&approve), "{:?}", sys.calls());
        assert!(sys.calls().iter().all(|call| !call.contains(TOKEN)
            && !call.contains("--global")
            && !call.contains(" clone ")));
    }
}
