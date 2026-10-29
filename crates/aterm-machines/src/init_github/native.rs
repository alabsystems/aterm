// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native GitHub transport using `agh` and [`crate::git_credential`].

use super::{
    Asked, Github, INPUT_MAX, TokenUnread, asked, decode, get, identity, read_token, repos,
};
use crate::engine::Place;
use crate::git_credential::{GITHUB, Git, Lookups, Standing, VaultStanding};
use crate::init_observe::{LastReading, Reading, Wait};
use crate::items::{Ctx, Item};
use crate::model::{ItemReport, Mode, Probe, User};
use crate::port::{GithubError, GithubFailure, Secret, System};
use aterm_converge::{Class, Layer, Remedy, Requires};
use aterm_types::macos_setup::FailureKind;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

/// The repositories the signed-in account owns, private ones included. Its forks and archived
/// repositories are left out of the default set ([`NativeRepositories::plan`]); an init.toml
/// `[github]` owner widens it.
const ACCESSIBLE: &str =
    "/user/repos?affiliation=owner&visibility=all&per_page=30&sort=full_name&direction=asc";

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

/// `aterm init`'s repository item for `user` with no `[github]` token source in init.toml: the
/// same stage the first launch runs, after the secrets item `after` names when one does (the
/// vault writes the token it reads). With no `[github]` table at all (`unasked`), a Mac with no
/// token is no stage of this run.
pub(super) fn item(user: &User, after: Option<String>, unasked: bool) -> Box<dyn Item> {
    Box::new(NativeRepositories {
        after,
        unasked,
        ..NativeRepositories::new(user.clone(), None)
    })
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
    if mode == Mode::Apply {
        // What a clone killed mid-way left (design §6.5): never a clone still running.
        repos::sweep_staging(sys, user);
    }
    let item = NativeRepositories::new(user.clone(), captured);
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
    /// The secrets item it waits for in `aterm init` ([`item`]).
    after: Option<String>,
    /// `aterm init`'s default, which nobody configured: no token on this Mac is no stage here,
    /// not a person's step.
    unasked: bool,
    /// What the last probe or apply read, for the step after it: the apply reuses the probe's
    /// inventory and verdicts, and the read-back reads again only what the apply cloned.
    carried: Mutex<Option<Carried>>,
    /// The commands that put back git's GitHub settings, read by the probe before an apply that
    /// replaces them ([`Standing::revert`]): the engine journals it before the apply runs.
    revert: Mutex<Option<String>>,
}

/// What one step read for the next ([`NativeRepositories::carried`]).
enum Carried {
    /// The probe's inventory and verdicts, for the apply.
    Probed(Plan, repos::Partition),
    /// The apply's, with the repositories it cloned, which the read-back verifies.
    Applied(Plan, repos::Partition, Vec<repos::Repo>),
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
            return Err(Block::of(Wait::CommandLineTools)(repos::no_git(sys).into()));
        }
        repos::admit_home(sys, user)?;
        let configured = configuration(sys, user).map_err(Block::of(Wait::Settings))?;
        let source = configured.clone().unwrap_or_else(|| Github {
            account: String::new(),
            owner: String::new(),
            token_file: Path::new(&user.home).join(super::TOKEN_FILE),
            token_key: None,
        });
        let token = Arc::new(read_token(sys, user, &source).map_err(Block::token)?);
        Ok(LocalInputs { configured, token })
    }
}

/// How git's github.com lookups stand for the plan's token ([`NativeRepositories::credentials`]).
struct Credentials {
    /// git's own helpers already hand back the token for every repository alike
    /// ([`lookups_answer`]): nothing is written.
    answered: bool,
    /// Else how git's settings stand, and whether the login keychain holds the token.
    standing: Option<(Standing, bool)>,
}

impl Credentials {
    /// Clones get the token: git's helpers hand it back, or the keychain holds it and git asks
    /// the keychain, or git's settings are someone else's and the clones pin the keychain.
    fn ready(&self) -> bool {
        self.answered
            || self.standing.as_ref().is_some_and(|(standing, held)| {
                *held && (standing.lookups.ready() || standing.foreign.is_some())
            })
    }

    /// The setting of someone else's that the run leaves as it is, when there is one.
    fn foreign(&self) -> Option<&str> {
        self.standing
            .as_ref()
            .and_then(|(standing, _)| standing.foreign.as_deref())
    }
}

impl NativeRepositories {
    fn new(user: User, captured: Option<Captured>) -> Self {
        Self {
            user,
            reading: LastReading::default(),
            captured,
            after: None,
            unasked: false,
            carried: Mutex::new(None),
            revert: Mutex::new(None),
        }
    }

    /// The inventory and verdicts the step before left for this one, taken.
    fn take(&self) -> Option<Carried> {
        self.carried
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    fn carry(&self, carried: Carried) {
        *self.carried.lock().unwrap_or_else(PoisonError::into_inner) = Some(carried);
    }

    /// How git's lookups stand for `plan`'s token. The settings and the keychain are read only
    /// when git's own helpers do not already hand it back, and the keychain only when git asks
    /// it or leaves someone else's settings.
    fn credentials(&self, sys: &dyn System, plan: &Plan) -> Result<Credentials, String> {
        let owners = plan.owners();
        let git = Git::keychain(sys);
        match git.vault_standing(sys, &self.user, &plan.account, &owners, &plan.token) {
            VaultStanding::Ready => {
                return Ok(Credentials {
                    answered: true,
                    standing: None,
                });
            }
            VaultStanding::Convert { revert } => {
                return Ok(Credentials {
                    answered: false,
                    standing: Some((
                        Standing {
                            lookups: Lookups::Not(
                                "git's global file overrides the XDG GitHub credential helper"
                                    .into(),
                            ),
                            foreign: None,
                            revert: Some(revert),
                            convert_to_vault: true,
                        },
                        false,
                    )),
                });
            }
            VaultStanding::Absent => {}
        }
        let standing =
            git.standing_for(sys, &self.user, &plan.account, &owners, Some(&plan.token))?;
        let held = (standing.lookups.ready() || standing.foreign.is_some())
            && credential_ready(sys, &self.user, &plan.account, &plan.token)?;
        Ok(Credentials {
            answered: false,
            standing: Some((standing, held)),
        })
    }

    /// The reading the probe answers from: the apply's, with what it cloned read back, or a
    /// new one.
    fn read(&self, sys: &dyn System) -> Result<(Plan, repos::Partition), Block> {
        match self.take() {
            Some(Carried::Applied(plan, mut standing, touched)) => {
                self.ensure_current().map_err(Block::from)?;
                standing.read_back(sys, &self.user, &touched);
                Ok((plan, standing))
            }
            _ => {
                let plan = self.plan(sys)?;
                let standing = repos::partition(sys, &self.user, &plan.repositories)
                    .map_err(Block::of(Wait::InTheWay))?;
                Ok((plan, standing))
            }
        }
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
    fn after(&self) -> Vec<String> {
        self.after.iter().cloned().collect()
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        self.reading.set(None);
        let check = (|| {
            let (plan, standing) = self.read(sys)?;
            let credentials = self.credentials(sys, &plan)?;
            self.ensure_current().map_err(Block::from)?;
            // Signed in, with no repository left that fits: the row waits for room instead of
            // a pass that would clone nothing.
            let volume = sys.volume(Path::new(&self.user.home));
            let left: Vec<String> = standing
                .missing
                .iter()
                .filter_map(|repo| repos::no_room(volume.as_ref(), repo))
                .collect();
            if credentials.ready()
                && !standing.missing.is_empty()
                && left.len() == standing.missing.len()
            {
                let why =
                    repos::short_of_room(volume.as_ref(), &left, CHECK_AGAIN).unwrap_or_default();
                return Err(Block::of(Wait::DiskSpace)(why));
            }
            Ok::<_, Block>((plan, standing, credentials))
        })();
        let (plan, standing, credentials) = match check {
            Ok(read) => read,
            Err(block) => {
                // Why it cannot go on, where this probe decided it: System State's note.
                self.reading.set(block.wait.map(Reading::Waits));
                return block.probe(self.unasked, sys.macos());
            }
        };
        let ready = credentials.ready();
        let (count, missing) = (standing.admissible(), standing.missing.len());
        self.reading.set(Some(Reading::Repositories {
            present: count.saturating_sub(missing),
            count,
            signed_in: ready,
        }));
        *self.revert.lock().unwrap_or_else(PoisonError::into_inner) = credentials
            .standing
            .as_ref()
            .and_then(|(standing, _)| standing.revert.clone());
        let mut notes = plan.notes.clone();
        notes.extend(standing.notes.iter().cloned());
        let foreign = credentials.foreign().map(|named| {
            format!("; git's own GitHub settings left as they are ({named}): clones use the login keychain's credential")
        });
        let probe = if missing == 0 && ready {
            let mut now =
                format!("{count} matching worktrees present; native GitHub credentials verified");
            now.push_str(foreign.as_deref().unwrap_or_default());
            if let Some(left) = repos::named(&notes) {
                now.push_str(&format!("; left as they are: {left}"));
            }
            Probe::Ok { now }
        } else {
            Probe::Change {
                from: format!("{missing} of {count} repositories missing, or native GitHub credentials need setup"),
                to: "configure native GitHub credentials and clone missing repositories; preserve existing worktrees".into(),
            }
        };
        self.carry(Carried::Probed(plan, standing));
        probe
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        let (plan, standing) = match self.take() {
            Some(Carried::Probed(plan, standing)) => (plan, standing),
            _ => {
                let plan = self.plan(sys).map_err(|block| {
                    // Why it stopped, where this step decided it: the failed row keeps it for
                    // System State's page (`init_observe::failure_note`), the engine's line for
                    // the log.
                    if let Some(wait) = block.wait {
                        self.reading.set(Some(Reading::Waits(wait)));
                    }
                    block.why
                })?;
                let standing = repos::partition(sys, &self.user, &plan.repositories)?;
                (plan, standing)
            }
        };
        let current = || self.ensure_current();
        current()?;
        let owners = plan.owners();
        let installed = install_credentials(
            sys,
            &self.user,
            &plan.account,
            &owners,
            &plan.token,
            &current,
        )?;
        let unchecked = || Ok::<(), String>(());
        let guard = match (installed, self.captured.is_some()) {
            (Install::Vault, true) => Some(repos::CloneGuard::current_only(&current)),
            (Install::Vault, false) => None,
            (Install::Keychain { .. }, true) => {
                Some(repos::CloneGuard::pinned(&plan.account, &current))
            }
            (Install::Keychain { pin: true }, false) => {
                Some(repos::CloneGuard::pinned(&plan.account, &unchecked))
            }
            (Install::Keychain { pin: false }, false) => None,
        };
        let cloned = repos::clone_all(sys, &self.user, &standing.missing, guard.as_ref());
        current()?;
        let home = Path::new(&self.user.home);
        let room = repos::short_of_room(sys.volume(home).as_ref(), &cloned.left, CHECK_AGAIN);
        let mut failed = cloned.failed;
        // What it cloned stays missing until the read-back reads it as its own worktree.
        self.carry(Carried::Applied(plan, standing, cloned.cloned));
        if let Some(why) = room {
            // The failed row keeps the page's words for it (`init_observe::failure_note`).
            self.reading.set(Some(Reading::Waits(Wait::DiskSpace)));
            failed.push(why);
        }
        if failed.is_empty() {
            Ok(())
        } else {
            Err(failed.join("; "))
        }
    }

    fn revert(&self) -> Option<String> {
        self.revert
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|settings| {
                format!(
                    "{settings} (puts back git's GitHub settings; the login keychain's github.com \
                     credential and the cloned repositories stay)"
                )
            })
    }

    fn detail(&self, _sys: &dyn System, _ctx: &Ctx) -> Option<String> {
        self.reading.detail()
    }
}

struct Plan {
    account: String,
    token: Arc<Secret>,
    repositories: Vec<repos::Repo>,
    /// The listed repositories left out because another takes their folder
    /// ([`repos::Inventory::notes`]).
    notes: Vec<String>,
}

impl Plan {
    fn owners(&self) -> Vec<&str> {
        let mut out = vec![self.account.as_str()];
        for repo in &self.repositories {
            if !out
                .iter()
                .any(|seen| seen.eq_ignore_ascii_case(&repo.owner))
            {
                out.push(repo.owner.as_str());
            }
        }
        out
    }
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
    /// else a wait that ends by itself. For `aterm init`'s default (`unasked`), a machine the
    /// vault wrote no token to has no stage to run, which asks nobody anything; `mac` names it.
    fn probe(self, unasked: bool, mac: bool) -> Probe {
        match self.wait {
            Some(Wait::Token) if unasked => Probe::NotApplicable {
                why: format!(
                    "no GitHub token on this {} yet: the ALab vault's sync writes ~/{} \
                     (init.toml [github] token_file names another)",
                    if mac { "Mac" } else { "machine" },
                    super::TOKEN_FILE
                ),
            },
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
/// room ([`repos::short_of_room`]): System State's run tries again by itself only for its first
/// tries.
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

    /// The token, the account it signs in as (one `GET /user`) and the repositories to have in
    /// `~`: the init.toml `[github]` owner's when one is configured, else the account's own
    /// that are neither forks nor archived.
    fn plan(&self, sys: &dyn System) -> Result<Plan, Block> {
        self.ensure_current().map_err(Block::from)?;
        let inputs = match &self.captured {
            Some(captured) if captured.inputs.user == self.user => captured.inputs.local.clone()?,
            Some(_) => return Err("the captured repository account changed".to_owned().into()),
            None if self.unasked
                && sys
                    .stat(&Path::new(&self.user.home).join(super::TOKEN_FILE))
                    .is_err_and(|error| error.kind() == io::ErrorKind::NotFound) =>
            {
                // Nothing to sign in with, so nothing else (git, the home) is read.
                return Err(Block::of(Wait::Token)(String::new()));
            }
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
                (ACCESSIBLE.to_owned(), Some(config.owner.as_str()))
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
        let mut listed = repos::enumerate_pages(&base, owner, Some(&account), |endpoint| {
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
        if configured.is_none() {
            listed
                .repositories
                .retain(|repo| !repo.fork && !repo.archived);
        }
        self.ensure_current().map_err(Block::from)?;
        Ok(Plan {
            account,
            token,
            repositories: listed.repositories,
            notes: listed.notes,
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
    // A table that turns the stage off is System State's choice there, not a scope here.
    let configured = match asked(decode(&bytes)?.github, user)? {
        Asked::Scoped(config) | Asked::Profile(config) => Some(config),
        Asked::Default | Asked::Off => None,
    };
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

/// Whether git's own non-keychain github.com lookups, through the helpers `user`'s configuration
/// names and asked as a clone asks, already hand back `token` for `account` and `owners` alike.
pub(super) fn lookups_answer_for(
    sys: &dyn System,
    user: &User,
    account: &str,
    owners: &[&str],
    token: &Secret,
) -> bool {
    let git = Git::keychain(sys);
    git.fill_as_configured(sys, Some(user), GITHUB)
        .is_ok_and(|answer| answer.is_some_and(|got| got.bytes() == token.bytes()))
        && git
            .uniform_for(sys, user, account, owners)
            .is_ok_and(|lookups| lookups.ready())
}

enum Install {
    /// git's own non-keychain (vault / XDG) helper answers the token: neither write nor pin the
    /// keychain.
    Vault,
    /// Clones use the keychain helper (`pin` when someone else's git settings stay in place).
    Keychain { pin: bool },
}

fn unconfigure_to_vault(
    git: &Git,
    sys: &dyn System,
    user: &User,
    account: &str,
    owners: &[&str],
    token: &Secret,
    current: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    current()?;
    git.unconfigure(sys, user)?;
    if !lookups_answer_for(sys, user, account, owners, token) {
        return Err(
            "git's XDG GitHub credential helper did not answer after removing the global override"
                .into(),
        );
    }
    Ok(())
}

/// Accept git's non-keychain (vault / XDG) helper when it already answers `token`, convert a
/// keychain-path `~/.gitconfig` written by an earlier pass back to that XDG helper without
/// writing the keychain, or fall back to storing `token` in the login keychain.
fn install_credentials(
    sys: &dyn System,
    user: &User,
    account: &str,
    owners: &[&str],
    token: &Secret,
    current: &dyn Fn() -> Result<(), String>,
) -> Result<Install, String> {
    let git = Git::keychain(sys);
    match git.vault_standing(sys, user, account, owners, token) {
        VaultStanding::Ready => return Ok(Install::Vault),
        VaultStanding::Convert { .. } => {
            unconfigure_to_vault(&git, sys, user, account, owners, token, current)?;
            return Ok(Install::Vault);
        }
        VaultStanding::Absent => {}
    }
    let standing = git.standing_for(sys, user, account, owners, Some(token))?;
    if standing.convert_to_vault {
        unconfigure_to_vault(&git, sys, user, account, owners, token, current)?;
        return Ok(Install::Vault);
    }
    if !credential_ready(sys, user, account, token)? {
        current()?;
        git.store(sys, Some(user), GITHUB, account, token)?;
    }
    if standing.foreign.is_some() {
        return Ok(Install::Keychain { pin: true });
    }
    if !standing.lookups.ready() {
        current()?;
        git.configure(sys, user, account)?;
        if let Lookups::Not(why) = git.configured(sys, user, account)? {
            return Err(format!(
                "native GitHub credentials did not read back after setup: {why}"
            ));
        }
    }
    Ok(Install::Keychain { pin: false })
}

#[cfg(test)]
pub(super) mod tests {
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
        let item = NativeRepositories::new(user("alice", 501), None);
        sys.stat_of(&item.user.home, stat(true, 0o700));
        (sys, item)
    }

    fn source(sys: &Fake, path: &str) {
        sys.file(path, TOKEN.as_bytes())
            .stat_of(path, stat(false, 0o600));
    }

    /// `user`'s account on `sys` listing one repository of hers, `name`, already a worktree in
    /// `~`, with her synced token and git's helpers holding it, and the git `sys` runs (the
    /// distribution's on Linux): the stage init runs with no init.toml, converged.
    pub(in crate::init_github) fn converged(sys: &Fake, user: &User, name: &str) {
        let git = if sys.macos() {
            crate::items::github::CLT_GIT
        } else {
            crate::items::github::SYSTEM_GIT
        };
        sys.file(git, b"synthetic git");
        sys.stat_of(&user.home, stat(true, 0o700));
        let home = Path::new(&user.home);
        source(
            sys,
            home.join(crate::init_github::TOKEN_FILE).to_str().unwrap(),
        );
        answer(sys, user, "/user", aterm_json::json!({"login": user.name}));
        answer(
            sys,
            user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::json!([repository(&user.name, name, 1)]),
        );
        let path = home.join(name);
        sys.stat_of(path.to_str().unwrap(), stat(true, 0o700));
        super::repos::tests::script_existing(sys, user, &path, name);
        script::lookups_hold(sys, user, GITHUB, &user.name, TOKEN);
        script::alike(sys, user);
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

    /// Two listed repositories that want the same folder in `~`: the signed-in account's own
    /// keeps it, the other is one note, and the stage is done once the kept one is present.
    #[test]
    fn a_repository_whose_folder_another_takes_is_one_note_and_the_stage_is_done() {
        use super::repos::tests::script_existing;
        let (sys, item) = fixture();
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::json!([
                repository("team", "Shared", 1),
                repository("alice", "shared", 2),
            ]),
        );
        let path = Path::new(&item.user.home).join("shared");
        sys.stat_of(path.to_str().unwrap(), stat(true, 0o700));
        script_existing(&sys, &item.user, &path, "shared");
        script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
        script::alike(&sys, &item.user);
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let rows = run(&sys, &context, Mode::Apply).unwrap();
        assert_eq!(rows[0].outcome, crate::model::Outcome::Ok, "{rows:?}");
        assert_eq!(
            rows[0].to.as_deref(),
            Some(
                "1 matching worktrees present; native GitHub credentials verified; left as they \
                 are: github.com/team/Shared: ~/shared is github.com/alice/shared's; not cloned"
            )
        );
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
                    with_profile(&sys, b"[github]\nowner='alice'\n");
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
        // Vault helper answers: captured clones never write to the login keychain.
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
        item.captured = Some(Captured {
            inputs,
            current: Box::new(|| Ok(true)),
        });
        assert!(item.apply(&sys, &ctx).is_err());
        let approve = script::credential(Some(&item.user), "approve");
        assert!(
            !sys.calls().contains(&approve),
            "vault helper path must not write to osxkeychain: {:?}",
            sys.calls()
        );
        assert!(sys.calls().iter().all(|call| !call.contains(TOKEN)
            && !call.contains("--global")
            && !call.contains(" clone ")));

        // Keychain path (no vault helper): captured clones store the snapshot through private
        // `credential approve` input only.
        let (sys, mut item) = fixture();
        let inputs = InitialRepositoryInputs::capture(&sys, &ctx).unwrap();
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::json!([repository("alice", "new", 1)]),
        );
        credential_commands(&sys, &item.user, TOKEN);
        item.captured = Some(Captured {
            inputs,
            current: Box::new(|| Ok(true)),
        });
        assert!(item.apply(&sys, &ctx).is_err());
        assert!(sys.calls().contains(&approve), "{:?}", sys.calls());
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.contains(TOKEN) && !call.contains(" clone "))
        );
    }

    /// A Mac whose GitHub account lists `listed` (each alice's, sized `0`, in GitHub's order),
    /// of which `present` are already worktrees in `~` and every other one is scripted to clone
    /// into the staging directories a pass makes in list order; git's own helpers hand back the
    /// synced token.
    fn listing(listed: &[String], present: &[String]) -> (Fake, NativeRepositories) {
        listing_with(listed, present, &[])
    }

    /// [`listing`], with the worktrees named in `theirs` cloned from someone else's origin.
    fn listing_with(
        listed: &[String],
        present: &[String],
        theirs: &[&str],
    ) -> (Fake, NativeRepositories) {
        use super::repos::tests::{script_clone_named, script_existing};
        let (sys, item) = fixture();
        let home = Path::new(&item.user.home);
        let values: Vec<aterm_json::Value> = listed
            .iter()
            .zip(1..)
            .map(|(name, id)| repository("alice", name, id))
            .collect();
        for (index, chunk) in values.chunks(30).enumerate() {
            let page = index + 1;
            let mut headers = Vec::new();
            if page * 30 < values.len() {
                let next = format!("https://api.github.com{ACCESSIBLE}&page={}", page + 1);
                headers.push(("Link".to_owned(), format!("<{next}>; rel=\"next\"")));
            }
            sys.on_github_api(
                &item.user,
                &format!("{ACCESSIBLE}&page={page}"),
                Ok(GithubResponse {
                    status: 200,
                    headers,
                    body: aterm_json::to_vec(&chunk.to_vec()).unwrap(),
                }),
            );
        }
        // A last page that is full asks for the one after it, which is empty.
        if values.len().is_multiple_of(30) {
            answer(
                &sys,
                &item.user,
                &format!("{ACCESSIBLE}&page={}", values.len() / 30 + 1),
                aterm_json::json!([]),
            );
        }
        let mut sequence = 0;
        for name in listed {
            let path = home.join(name);
            if theirs.contains(&name.as_str()) {
                sys.stat_of(path.to_str().unwrap(), stat(true, 0o700));
                sys.on(
                    &crate::checkout::git(&sys, &item.user, Some(&path))
                        .args(["config", "--get-all", "remote.origin.url"])
                        .display(),
                    Ran::ok(&format!("https://github.com/someone/{name}.git\n")),
                );
            } else if present.contains(name) {
                sys.stat_of(path.to_str().unwrap(), stat(true, 0o700));
            } else if !repos::HOME_FOLDERS
                .iter()
                .any(|folder| folder.eq_ignore_ascii_case(name))
            {
                script_clone_named(&sys, &item.user, name, sequence, false, None);
                sequence += 1;
            }
            script_existing(&sys, &item.user, &path, name);
        }
        script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
        script::alike(&sys, &item.user);
        (sys, item)
    }

    fn names(prefix: &str, count: usize) -> Vec<String> {
        (0..count)
            .map(|index| format!("{prefix}-{index:03}"))
            .collect()
    }

    /// How many of `calls` ran git as alice in `path` with `args`.
    fn ran_in(calls: &[String], path: &Path, args: &str) -> usize {
        let at = format!("-C {} {args}", path.display());
        calls.iter().filter(|call| call.ends_with(&at)).count()
    }

    const TOP_LEVEL: &str = "rev-parse --is-inside-work-tree --show-prefix --show-toplevel";

    /// One whole pass over 50 repositories already in `~` and 10 to clone: GitHub's account and
    /// listing are read once, each present worktree is read once (one rev-parse, one origin), and
    /// the read-back reads only the 10 clones. Before: three listings and twelve git runs per
    /// present worktree (836 git runs here); after: one listing and two (286).
    #[test]
    fn a_whole_pass_reads_github_and_each_worktree_once_and_reads_back_only_its_clones() {
        let (present, missing) = (names("repo", 50), names("new", 10));
        let listed: Vec<String> = present.iter().chain(&missing).cloned().collect();
        let (sys, item) = listing(&listed, &present);
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let rows = run(&sys, &context, Mode::Apply).unwrap();
        assert_eq!(rows[0].outcome, crate::model::Outcome::Changed, "{rows:?}");
        assert_eq!(
            rows[0].to.as_deref(),
            Some("60 matching worktrees present; native GitHub credentials verified")
        );
        let calls = sys.calls();
        let count = |what: &str| calls.iter().filter(|call| call.ends_with(what)).count();
        assert_eq!(count(" agh GET /user"), 1, "{calls:?}");
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.contains(" agh GET /user/repos"))
                .count(),
            3,
            "two pages and the empty one after the full last page"
        );
        let home = Path::new(&item.user.home);
        for name in &present {
            let path = home.join(name);
            assert_eq!(ran_in(&calls, &path, TOP_LEVEL), 1, "{name}");
            assert_eq!(
                ran_in(&calls, &path, "config --get-all remote.origin.url"),
                1,
                "{name}"
            );
        }
        for name in &missing {
            assert_eq!(ran_in(&calls, &home.join(name), TOP_LEVEL), 1, "{name}");
        }
        let git = calls
            .iter()
            .filter(|call| call.contains(crate::items::github::CLT_GIT))
            .count();
        assert_eq!(git, 286, "{calls:#?}");
    }

    /// Up to four clones run at once: each of the first four waits, inside its clone, for the
    /// others to start (a one-minute hang detector, never a latency budget), which a pass that
    /// clones one at a time never lets happen; a fifth never runs beside them. No more staging
    /// directories exist than clones running, so a pass cut short leaves at most four behind.
    #[test]
    fn four_clones_run_at_once() {
        use std::sync::{Arc, Condvar, Mutex};
        let missing = names("new", 6);
        let (sys, item) = listing(&missing, &[]);
        let gate = Arc::new((Mutex::new((0_usize, 0_usize)), Condvar::new()));
        let seen = Arc::clone(&gate);
        sys.simulate(move |fake, cmd| {
            if !cmd.display().contains(" clone --no-single-branch ") {
                return None;
            }
            let calls = fake.calls();
            let marked = |what: &str| calls.iter().filter(|call| call.starts_with(what)).count();
            let staged = marked("scratch allocated ") - marked("scratch closed ");
            assert!(staged <= LANES_SEEN, "{staged} staging directories at once");
            let (lock, turn) = &*seen;
            let mut state = lock.lock().unwrap();
            state.0 += 1;
            state.1 = state.1.max(state.0);
            turn.notify_all();
            let (mut state, waited) = turn
                .wait_timeout_while(state, std::time::Duration::from_secs(60), |state| {
                    state.1 < LANES_SEEN
                })
                .unwrap();
            assert!(!waited.timed_out(), "four clones never ran at once");
            state.0 -= 1;
            None
        });
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        item.apply(&sys, &context).unwrap();
        assert_eq!(gate.0.lock().unwrap().1, LANES_SEEN);
        let home = Path::new(&item.user.home);
        for name in &missing {
            assert!(sys.stat(&home.join(name)).is_ok(), "{name}");
        }
    }

    /// The most clones a pass runs at once.
    const LANES_SEEN: usize = 4;

    /// The read-back reads again only what the pass cloned, and a clone that no longer reads as
    /// the repository's own worktree there (another origin took its place once it was
    /// published) is still missing: the row fails instead of counting it present or dropping it.
    #[test]
    fn a_clone_the_read_back_cannot_verify_is_still_missing() {
        let (sys, item) = listing(&["new".to_owned()], &[]);
        let origin = format!(
            "-C {}/new config --get-all remote.origin.url",
            item.user.home
        );
        sys.simulate(move |_, cmd| {
            cmd.display()
                .ends_with(&origin)
                .then(|| Ran::ok("https://github.com/someone/new.git\n"))
        });
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let rows = run(&sys, &context, Mode::Apply).unwrap();
        assert_eq!(rows[0].outcome, crate::model::Outcome::Failed, "{rows:?}");
        let why = rows[0].why.as_deref().unwrap_or_default();
        assert!(why.contains("1 of 1 repositories missing"), "{why}");
        assert!(
            sys.calls()
                .iter()
                .any(|call| call.starts_with("rename-noreplace ")),
            "the clone was published before the read-back: {:?}",
            sys.calls()
        );
    }

    /// A worktree of another origin where a repository goes, and a repository named like a
    /// standard home folder, are each a note: the others clone, git never runs in the home
    /// folder, and the stage is done.
    #[test]
    fn a_folder_in_the_way_or_a_home_folder_is_a_note_and_the_others_clone() {
        let listed: Vec<String> = ["Documents", "infra", "new"].map(str::to_owned).to_vec();
        let (sys, item) = listing_with(&listed, &[], &["infra"]);
        let home = Path::new(&item.user.home);
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let rows = run(&sys, &context, Mode::Apply).unwrap();
        assert_eq!(rows[0].outcome, crate::model::Outcome::Changed, "{rows:?}");
        assert_eq!(
            rows[0].to.as_deref(),
            Some(
                "1 matching worktrees present; native GitHub credentials verified; left as they \
                 are: ~/Documents is a standard home folder; left untouched; ~/infra has a \
                 different or credential-bearing GitHub origin; left untouched"
            )
        );
        assert!(sys.stat(&home.join("new")).is_ok());
        let calls = sys.calls();
        let documents = home.join("Documents");
        assert!(
            calls
                .iter()
                .all(|call| !call.contains(&documents.display().to_string())),
            "{calls:?}"
        );
        assert!(
            calls
                .iter()
                .all(|call| !call.contains("rename-noreplace") || call.ends_with("/new")),
            "{calls:?}"
        );
    }

    /// `aterm init`'s default stage on a Mac the vault wrote no GitHub token to asks nobody
    /// anything: its row is not applicable, and neither git nor GitHub is asked. A `[github]`
    /// table that scopes the stage makes the same Mac the person's step (the control).
    #[test]
    fn init_s_default_stage_without_a_token_is_not_applicable_and_asks_nothing() {
        let (sys, bare) = bare();
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&bare.user));
        match item(&bare.user, None, true).probe(&sys, &context) {
            Probe::NotApplicable { why } => {
                assert!(why.contains("~/.secrets/gh_access_token"), "{why}");
                assert!(why.contains("on this Mac yet"), "{why}");
            }
            other => panic!("{other:?}"),
        }
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.contains(" agh GET ")
                    && !call.contains(crate::items::github::CLT_GIT)),
            "{:?}",
            sys.calls()
        );
        assert!(matches!(
            item(&bare.user, None, false).probe(&sys, &context),
            Probe::Person { .. }
        ));
        // On Linux the row names the machine, not a Mac.
        let (mut linux, _) = self::bare();
        linux.linux = true;
        match item(&bare.user, None, true).probe(&linux, &context) {
            Probe::NotApplicable { why } => assert!(why.contains("on this machine yet"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    /// The default set is the account's own repositories that are neither forks nor archived;
    /// an init.toml `[github]` owner keeps them all (the control).
    #[test]
    fn forks_and_archived_repositories_are_left_out_unless_init_toml_names_the_owner() {
        let mut listed = vec![
            repository("alice", "fork", 1),
            repository("alice", "kept", 2),
            repository("alice", "old", 3),
        ];
        for (index, key) in [(0, "fork"), (2, "archived")] {
            listed[index]
                .as_object_mut()
                .unwrap()
                .insert(key.into(), aterm_json::Value::Bool(true));
        }
        let names = |item: &NativeRepositories, sys: &Fake| -> Vec<String> {
            item.plan(sys)
                .unwrap()
                .repositories
                .into_iter()
                .map(|repo| repo.name)
                .collect()
        };
        let (sys, item) = fixture();
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::Value::Array(listed.clone()),
        );
        assert_eq!(names(&item, &sys), ["kept"]);

        let (sys, item) = fixture();
        let profile = "/Users//alice/.config/aterm/init.toml";
        let bytes = b"[github]\naccount = 'alice'\n";
        sys.file(profile, bytes).stat_of(
            profile,
            Stat {
                len: bytes.len() as u64,
                ..stat(false, 0o600)
            },
        );
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::Value::Array(listed),
        );
        assert_eq!(names(&item, &sys), ["fork", "kept", "old"]);
    }

    /// git's GitHub settings are replaced only once the engine journaled the line that puts the
    /// global file's old ones back (before any `--replace-all`). A helper of someone else's that
    /// git already uses (`gh`'s, here) stays as it is, the row says so, and the clones pin the
    /// keychain's credential on their own command line.
    #[test]
    fn git_settings_are_journaled_before_they_are_replaced_and_someone_elses_helper_stays() {
        let journaled = |sys: &Fake| -> Vec<aterm_json::Value> {
            sys.written(Fake::JOURNAL)
                .map(|bytes| String::from_utf8(bytes).unwrap())
                .unwrap_or_default()
                .lines()
                .map(|line| aterm_json::from_str(line).unwrap())
                .collect()
        };
        let (sys, item) = fixture();
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::json!([]),
        );
        let global = script::global(&item.user);
        sys.on(
            &global,
            Ran::ok("credential.https://github.com.username\nold-login\0"),
        );
        // The probe's reading, then the apply's own before and after it configures.
        script::unconfigured(&sys, &item.user);
        credential_commands(&sys, &item.user, TOKEN);
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let rows = run(&sys, &context, Mode::Apply).unwrap();
        assert_eq!(rows[0].outcome, crate::model::Outcome::Changed, "{rows:?}");
        let lines = journaled(&sys);
        let revert = lines[0]["revert"].as_str().unwrap();
        assert!(
            revert.starts_with(
                "git config --global --unset-all credential.https://github.com.helper; git \
                 config --global --unset-all credential.https://github.com.username; git config \
                 --global --add credential.https://github.com.username old-login; git config \
                 --global --unset-all credential.https://github.com.useHttpPath"
            ),
            "{revert}"
        );
        let calls = sys.calls();
        let read = calls.iter().position(|call| *call == global).unwrap();
        let replaced = calls
            .iter()
            .position(|call| call.contains("--replace-all"))
            .unwrap();
        assert!(read < replaced, "{calls:?}");

        // Control: gh's helper for github.com is someone else's. The one missing repository
        // clones with the keychain's credential pinned on its own command line.
        let (sys, item) = fixture();
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::json!([repository("alice", "new", 1)]),
        );
        let unchecked = || Ok::<(), String>(());
        let pinned = repos::CloneGuard::pinned("alice", &unchecked);
        super::repos::tests::script_clone_named(&sys, &item.user, "new", 0, false, Some(&pinned));
        super::repos::tests::script_existing(
            &sys,
            &item.user,
            &Path::new(&item.user.home).join("new"),
            "new",
        );
        let helper = "credential.https://github.com.helper";
        sys.on(
            &script::helpers(&item.user),
            Ran::ok(&script::printed(&[
                (script::CLT_CONFIG, "credential.helper", "osxkeychain"),
                ("/Users//alice/.gitconfig", helper, ""),
                (
                    "/Users//alice/.gitconfig",
                    helper,
                    "!/opt/homebrew/bin/gh auth git-credential",
                ),
            ])),
        );
        script::holds(&sys, Some(&item.user), GITHUB, "alice", TOKEN);
        let rows = run(&sys, &context, Mode::Apply).unwrap();
        assert_eq!(rows[0].outcome, crate::model::Outcome::Changed, "{rows:?}");
        let now = rows[0].to.as_deref().unwrap();
        assert!(
            now.contains(
                "git's own GitHub settings left as they are (credential.https://github.com.helper \
                 in /Users//alice/.gitconfig)"
            ),
            "{now}"
        );
        assert!(!now.contains("gh auth"), "{now}");
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.contains("--global") && !call.contains("credential approve")),
            "{:?}",
            sys.calls()
        );
        assert_eq!(journaled(&sys)[0]["revert"], aterm_json::Value::Null);
    }

    #[test]
    fn native_converts_shadowed_keychain_gitconfig_without_writing_keychain() {
        let journaled = |sys: &Fake| -> Vec<aterm_json::Value> {
            sys.written(Fake::JOURNAL)
                .map(|bytes| String::from_utf8(bytes).unwrap())
                .unwrap_or_default()
                .lines()
                .map(|line| aterm_json::from_str(line).unwrap())
                .collect()
        };

        // 1. Keychain holds current token, ~/.gitconfig shadows XDG helper: converts, journals
        //    revert before apply, and never runs `credential approve`.
        for keychain_token in [TOKEN, "a_rotated_out_value"] {
            let (sys, item) = fixture();
            answer(
                &sys,
                &item.user,
                &format!("{ACCESSIBLE}&page=1"),
                aterm_json::json!([]),
            );
            script::lookups_hold(&sys, &item.user, GITHUB, "alice", keychain_token);
            script::lookups_hold(&sys, &item.user, GITHUB, "alice", keychain_token);
            script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
            script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
            script::lookups_hold_xdg(&sys, &item.user, GITHUB, "alice", TOKEN);
            script::shadowed(&sys, &item.user, "alice");
            script::shadowed(&sys, &item.user, "alice");
            script::alike(&sys, &item.user);
            script::alike(&sys, &item.user);
            let unsets = script::unconfigure(&sys, &item.user);
            let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
            let rows = run(&sys, &context, Mode::Apply).unwrap();
            assert_eq!(
                rows[0].outcome,
                crate::model::Outcome::Changed,
                "{keychain_token}: {rows:?}"
            );
            let revert = journaled(&sys)[0]["revert"].as_str().unwrap().to_owned();
            assert!(
                revert.contains(
                    "git config --global --add credential.https://github.com.helper osxkeychain"
                ),
                "{revert}"
            );
            let calls = sys.calls();
            assert!(unsets.iter().all(|u| calls.contains(u)), "{calls:?}");
            assert!(
                calls
                    .iter()
                    .all(|call| !call.contains("credential approve")
                        && !call.contains("--replace-all")),
                "{calls:?}"
            );
        }

        // 2. Negative control: pure keychain `~/.gitconfig` with no XDG helper stays Ready and
        //    keeps the keychain path untouched.
        let (sys, item) = fixture();
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::json!([]),
        );
        script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
        script::configured(&sys, &item.user, "alice");
        script::holds(&sys, Some(&item.user), GITHUB, "alice", TOKEN);
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        assert!(matches!(item.probe(&sys, &context), Probe::Ok { .. }));
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.contains("--unset-all") && !call.contains("credential approve"))
        );
    }
}
