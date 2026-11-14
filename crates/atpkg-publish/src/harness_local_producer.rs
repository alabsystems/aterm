// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Existing producer/stager adapter. Its constructor requires a permit from the actual
//! same-image guardian gate, held locks and admitted covering capacity claim.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use aterm_uds::process::{self, Identity};
use atpkg::local_harness::{self, CurrentLocal, ExpectedCurrent, LocalAuthority, StagedLocal};
use atpkg::store::Layout;

use super::{Config, Driver, LocalProfile, SelectionError, Version};
use crate::artifact::build_local::{self, Plan};
use crate::artifact::tools::{Run, Tools};
use crate::ctx::Ctx;
use crate::error::{Error, Result};
use crate::git::{
    GIT_TLS_PINS, git_environment_removals_from, git_tls_pins_at, git_tracing_environment,
};

/// The names a harness-checkout git's environment drops when this process's environment holds
/// `names`: every ambient repository selector, then [`git_environment_removals_from`] (ambient
/// tokens, injected command-scope config, TLS overrides, trace and shell start-up selectors, and
/// agh's runner-only `AGH_GIT_*`). The variables [`source_git_with`] then sets are git and SSH
/// hygiene, and for a fetch whose helper answers as a `legacy_loose` profile `AGH_GIT_LOOSE=1`
/// ([`AghHelper`]), so this process's environment without these names is the one git, and the
/// credential helper git starts, read agh's routing from ([`FetchEnv`]).
fn source_git_removals_from(names: impl IntoIterator<Item = OsString>) -> Vec<String> {
    let mut removed: Vec<String> = [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_CONFIG",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    removed.extend(git_environment_removals_from(names));
    removed
}

/// [`source_git_removals_from`] this process's environment: the one list a git run removes
/// ([`source_git_with`]), and for a fetch the list its credential helper is judged with
/// ([`fetch_source`]).
fn source_git_removals() -> Vec<String> {
    source_git_removals_from(std::env::vars_os().map(|(name, _)| name))
}

/// `inner` as a harness-checkout git, and the credential helper it starts, sees it: without
/// each name in `removed` (the fetch's own [`source_git_removals`]). An agh.toml profile whose
/// `env` names a removed variable (`GH_TOKEN`, say) is absent here, as it is to the helper.
struct FetchEnv<'a> {
    inner: &'a dyn agh::identity::EnvSource,
    removed: &'a [String],
}

impl agh::identity::EnvSource for FetchEnv<'_> {
    fn var_os(&self, name: &str) -> Option<OsString> {
        if self.removed.iter().any(|removed| removed == name) {
            return None;
        }
        self.inner.var_os(name)
    }
}

/// One bounded git in the harness checkout ([`source_git_with`]) without
/// [`source_git_removals`], setting hygiene alone.
fn source_git(tools: &dyn Tools, config: &Config, args: &[&str]) -> Result<String> {
    source_git_with(tools, config, args, &source_git_removals(), &[])
}

/// One bounded git in the harness checkout, through `tools` (the guardian's worker group),
/// without each name in `removed` (its caller's [`source_git_removals`]: no ambient repository,
/// trace, TLS, prompt, config or credential selector), then the hygiene variables and `set`,
/// which `tools` sets after its removals. Its error text is redacted of every token shape
/// (`agh::redact::Redactor::shapes`): a helper's answer is git's to hold, and this process knows
/// no token to redact by value.
fn source_git_with(
    tools: &dyn Tools,
    config: &Config,
    args: &[&str],
    removed: &[String],
    set: &[(String, OsString)],
) -> Result<String> {
    let mut run = Run::new("git", args.iter().copied()).bounded();
    run.cwd = Some(config.checkout.clone());
    run.env_remove.extend(removed.iter().cloned());
    run.env.extend(git_tracing_environment());
    run.env.extend([
        ("GIT_TERMINAL_PROMPT".into(), "0".into()),
        ("GIT_ASKPASS".into(), "/usr/bin/false".into()),
        ("SSH_ASKPASS".into(), "/usr/bin/false".into()),
        ("SSH_ASKPASS_REQUIRE".into(), "never".into()),
        (
            "GIT_SSH_COMMAND".into(),
            "ssh -oBatchMode=yes -oNumberOfPasswordPrompts=0".into(),
        ),
    ]);
    run.env.extend(set.iter().cloned());
    let redact = |text: String| agh::redact::Redactor::shapes(&text);
    let ran = tools
        .run(&run)
        .map_err(|e| Error::cannot_run(redact(format!("local source git: {e}"))))?;
    if !ran.success() {
        return Err(Error::refused(redact(format!(
            "local source git {}: {}",
            args.join(" "),
            ran.text()
        ))));
    }
    String::from_utf8(ran.stdout)
        .map(|text| text.trim_end_matches('\n').to_string())
        .map_err(|_| Error::refused("local source git returned non-UTF-8 identity"))
}

/// agh's git credential helper, as a fetch mounts it once it will answer.
#[derive(Clone, Debug, PartialEq, Eq)]
struct AghHelper {
    /// Its `credential.helper` value (`agh::git::HelperCommand::config_value`).
    command: String,
    /// Whether the profile it answers as is `legacy_loose` (`Profile::legacy_loose`, which only
    /// the library host's loader honours): the fetch then sets `AGH_GIT_LOOSE=1`
    /// (`agh::git::LOOSE_VAR`), as agh's own git runner does for such a profile, so the helper
    /// reads the token file by the policy the check judged it by.
    loose: bool,
}

/// agh's git credential helper for a fetch of `slug` that removes `removed` ([`fetch_helper`])
/// over this process: its agh directories (`Dirs::from_env`), the production client before
/// routing (`AghHost::base_client`), its environment, which the fetch's git inherits, and the
/// retained executable's helper (`agh::git::HelperCommand::current()`, then `agh auth
/// git-credential`). Else why not.
fn agh_helper(slug: &agh::Slug, removed: &[String]) -> std::result::Result<AghHelper, String> {
    let dirs = agh::config::Dirs::from_env().ok_or_else(|| {
        "agh's directories cannot be resolved: no absolute home, config or state directory"
            .to_string()
    })?;
    fetch_helper(
        &dirs,
        &crate::host::agh::AghHost::base_client(&dirs),
        &agh::identity::ProcessEnv,
        removed,
        slug,
        &|| {
            agh::git::HelperCommand::current()
                .and_then(|helper| helper.config_value())
                .map_err(|e| e.to_string())
        },
    )
}

/// [`routed_helper`] for a fetch of `slug` over `dirs`, learning through `base` as
/// atpkg-publish's own agh host routes (`agh::routing::adopt_for_host`, which
/// `AghHost::configured_client` uses): one worker routes one way, so a `legacy_loose` profile
/// (design §6.6, M2 only) its host reads loose is read loose here too, where agh's strict
/// `routing::adopt` would refuse a fetch the host answers. It judges in the fetch's environment
/// ([`FetchEnv`]): `inner` without `removed`, the very list the fetch's git removes
/// ([`fetch_source`] hands both the one list). The helper git starts routes from that
/// environment (the fetch adds only hygiene variables, which route nothing, and
/// `AGH_GIT_LOOSE=1` for a loose profile) and from `dirs`, since the fetch hands it no
/// `AGH_GIT_*` directory or grant.
fn fetch_helper(
    dirs: &agh::config::Dirs,
    base: &agh::Github,
    inner: &dyn agh::identity::EnvSource,
    removed: &[String],
    slug: &agh::Slug,
    helper: &dyn Fn() -> std::result::Result<String, String>,
) -> std::result::Result<AghHelper, String> {
    let env = FetchEnv { inner, removed };
    routed_helper(
        dirs,
        &env,
        slug,
        &|dirs| agh::routing::adopt_for_host(base, dirs),
        helper,
    )
}

/// `helper()`'s value, once agh's git credential helper over `dirs` and `env` (the fetch's
/// environment) will answer a fetch of `slug`; else why not. What needs no request is judged
/// first, so a refusal knowable here sends nothing and records nothing: `helper()` (a frontend
/// whose recorded image custody is unavailable refuses), then the tier by the recorded routing
/// (`agh::routing::view_for_host`, whose refusal of an agh.toml or a record learning would
/// meet too refuses here): a release-tier owner refuses, since the helper answers one only
/// under a grant agh's own git runner writes, and learning cannot change that tier, as it adds
/// only the vault's rows, whose release owner is on agh's release floor already.
/// `learn(dirs)` then resolves agh's routing with learning ([`fetch_helper`]'s is the agh
/// host's), so a vault token whose login agh has not recorded (no agh call has routed it yet,
/// or it was rotated since) is learned with one `GET /user` and recorded: the helper reads only
/// the record. The fetch speaks as the profile that routing sends a read of `slug` as, and a
/// routing that reads it anonymously or refuses it refuses the fetch, which has always needed
/// an identity, naming what adoption noted (`Adoption::note`, such as a vault token's login
/// GitHub could not be reached for, or a token it refused), so an outage does not read as
/// misconfiguration. Then the helper must answer as that profile,
/// ungranted, by the file policy the routing read it by (`agh::git::credential::require_answer`,
/// loose for a `legacy_loose` profile, else strict), so a login that could not be recorded, or
/// an agh.toml that changed, refuses with agh's reason and next step, never with git's
/// "credential helper told us to quit".
fn routed_helper(
    dirs: &agh::config::Dirs,
    env: &dyn agh::identity::EnvSource,
    slug: &agh::Slug,
    learn: &dyn Fn(&agh::config::Dirs) -> agh::Result<agh::routing::Configured>,
    helper: &dyn Fn() -> std::result::Result<String, String>,
) -> std::result::Result<AghHelper, String> {
    use agh::policy::{Decision, RouteRequest, RouteTarget, Tier};
    let with_next = |e: &agh::Error| format!("{}; next: {}", e.why, e.next);
    let command = helper()?;
    let recorded = agh::routing::view_for_host(dirs, env).map_err(|e| with_next(&e))?;
    if recorded
        .router()
        .map_err(|e| with_next(&e))?
        .tier_of(slug.owner())
        == Tier::Release
    {
        return Err(format!(
            "{slug} is release tier, and agh's git credential helper answers a release-tier \
             repository only under a grant agh's own git runner writes"
        ));
    }
    let learned = learn(dirs).map_err(|e| with_next(&e))?;
    let router = learned.router().map_err(|e| with_next(&e))?;
    let adoption = learned.adoption();
    let noted = || {
        let lines = adoption.map_or_else(Vec::new, |adoption| adoption.note(&dirs.home));
        if lines.is_empty() {
            String::new()
        } else {
            format!("; {}", lines.join("; "))
        }
    };
    let read = RouteRequest::read(RouteTarget::Repo(slug.clone()));
    let profile = match router.decide(&read, &|name| router.presence(name, env)) {
        Decision::Send { profile, .. } => profile,
        Decision::Refuse(refusal) => {
            return Err(format!(
                "{}{}; next: {}",
                refusal.why,
                noted(),
                refusal.next
            ));
        }
        _ => {
            let next = adoption
                .and_then(agh::identity::vault::Adoption::next_step)
                .unwrap_or_else(|| format!("agh whoami --for {slug}"));
            return Err(format!(
                "agh routes {slug} with no identity, and the harness source is fetched as \
                 one{}; next: {next}",
                noted()
            ));
        }
    };
    let loose = router
        .profile(&profile)
        .is_some_and(agh::identity::Profile::legacy_loose);
    agh::git::credential::require_answer(env, dirs, slug, &profile, false, loose)
        .map_err(|e| with_next(&e))?;
    Ok(AghHelper { command, loose })
}

/// Fetch the checkout's `origin` main into `refs/remotes/origin/main`, inside the worker group,
/// without [`source_git_removals`]: one list, which an `https://` origin's `helper` is judged
/// with too, so the environment the check reads is the one git and its helper get. Every
/// helper the user's or the system's config names is reset first. An `https://` origin then
/// gets the one helper `helper` names, scoped to `https://github.com`, with
/// `credential.useHttpPath=true` so it can route by repository, and `AGH_GIT_LOOSE=1` in its
/// environment when that helper answers as a `legacy_loose` profile ([`AghHelper`]); it reads
/// agh.toml's routing and its own token files, so no token is in the fetch's argv or the
/// child's environment, and this module reads no token file. No helper means no git: there is
/// no token fallback. An SSH origin gets no helper and keeps batch mode. Either way the fetch
/// carries agh's unscoped TLS pins ([`GIT_TLS_PINS`]: verification on, no redirect), and an
/// `https://` origin the same pins at `origin` itself ([`git_tls_pins_at`]), so they hold over
/// any key the user's, the system's or the checkout's config sets, unscoped or scoped to a URL
/// `origin` matches. `origin` is the URL git fetches: the caller matched it, as `git remote
/// get-url` reads it (every `url.*.insteadOf` rewrite expanded, in the config this fetch reads),
/// against its configured development repository, and the fetch drops every inherited
/// command-scope config variable that could add a rewrite. `origin` is the only repository it
/// fetches: `--recurse-submodules=no`, as agh's own git runner passes it, overrides
/// `fetch.recurseSubmodules` (`on-demand` by default) and `submodule.recurse` in any config, so
/// no fetch starts in a populated submodule, which would inherit the helper and the unscoped
/// pins through `GIT_CONFIG_PARAMETERS` but not the pins at `origin`, and would ask the helper
/// for a repository [`routed_helper`] never judged.
fn fetch_source(
    tools: &dyn Tools,
    config: &Config,
    origin: &str,
    helper: &dyn Fn(&[String]) -> std::result::Result<AghHelper, String>,
) -> Result<()> {
    let removed = source_git_removals();
    let mut args = vec!["-c".to_string(), "credential.helper=".to_string()];
    let mut set = Vec::new();
    if origin.starts_with("https://") {
        let helper = helper(&removed).map_err(|why| {
            Error::refused(agh::redact::Redactor::shapes(&format!(
                "local source credential: {why}"
            )))
        })?;
        args.extend([
            "-c".to_string(),
            format!("credential.https://github.com.helper={}", helper.command),
            "-c".to_string(),
            "credential.useHttpPath=true".to_string(),
        ]);
        if helper.loose {
            set.push((agh::git::LOOSE_VAR.to_string(), OsString::from("1")));
        }
    }
    args.extend(GIT_TLS_PINS.map(String::from));
    args.extend(
        git_tls_pins_at(origin)
            .map_err(|why| Error::refused(format!("local source origin: {why}")))?,
    );
    args.extend(
        [
            "-c",
            "core.askPass=/usr/bin/false",
            "-c",
            "protocol.ext.allow=never",
            "-c",
            "protocol.file.allow=never",
            "fetch",
            "--no-tags",
            "--recurse-submodules=no",
            "origin",
            "refs/heads/main:refs/remotes/origin/main",
        ]
        .map(String::from),
    );
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    source_git_with(tools, config, &args, &removed, &set).map(|_| ())
}

/// Minted only inside the verified worker gate. The detached owner holds its lifeline,
/// exact locks and capacity claim until the whole worker group retires.
pub struct QualifiedGuardian {
    package: PackageScope,
    source: SourceScope,
}

pub(super) struct SourceScope {
    pub(super) home: PathBuf,
    pub(super) config: Config,
    pub(super) lease: super::fs::LeaseProof,
    pub(super) guardian: Identity,
}

impl SourceScope {
    fn check(&self, home: &Path, config: &Config) -> Result<()> {
        if self.home != home || self.config != *config {
            return Err(Error::refused(
                "local selection differs from its original guardian source scope",
            ));
        }
        super::fs::verify_lease(home, config, &self.lease)?;
        if process::process_presence(self.guardian.pid) != process::ProcessPresence::Present
            || process::read_identity(self.guardian.pid).as_ref() != Some(&self.guardian)
        {
            return Err(Error::refused(
                "local selection's original guardian is no longer live; current and candidate are preserved",
            ));
        }
        Ok(())
    }
}

pub(super) struct PackageScope {
    pub cache: PathBuf,
    pub attempt: String,
    pub guardian: u32,
    pub group: u32,
    pub registrar: PathBuf,
    pub budget: u64,
    pub jobs: u32,
}

pub(super) fn cache_root(config: &Config) -> PathBuf {
    config
        .out
        .join(".ubd/harness-targets.noindex")
        .join(&config.program)
}

impl QualifiedGuardian {
    pub(super) fn in_worker(permit: &super::guardian::WorkerPermit) -> Self {
        Self {
            package: permit.package_scope(),
            source: permit.source_scope(),
        }
    }
}

/// Existing Request/plan/build and local staging, with no publication or signed authority.
pub struct Producer<'a> {
    ctx: &'a Ctx,
    tools: &'a dyn Tools,
    _guardian: QualifiedGuardian,
    recovered: Option<(PathBuf, StagedLocal)>,
}

impl<'a> Producer<'a> {
    /// Compose only after the owner has qualified its process runner. `tools` must cover all
    /// compiler/Git/version children, including the calls made by the existing producer.
    #[must_use]
    pub fn new(ctx: &'a Ctx, tools: &'a dyn Tools, guardian: QualifiedGuardian) -> Self {
        Self {
            ctx,
            tools,
            _guardian: guardian,
            recovered: None,
        }
    }

    fn git(&self, config: &Config, args: &[&str]) -> Result<String> {
        source_git(self.tools, config, args)
    }

    fn clean_main(&self, config: &Config) -> Result<()> {
        if self.git(config, &["rev-parse", "--show-toplevel"])? != config.checkout.to_string_lossy()
            || self.git(config, &["symbolic-ref", "--quiet", "--short", "HEAD"])? != "main"
            || self.git(
                config,
                &[
                    "rev-parse",
                    "--abbrev-ref",
                    "--symbolic-full-name",
                    "@{upstream}",
                ],
            )? != "origin/main"
            || self.git(config, &["rev-parse", "--is-shallow-repository"])? != "false"
            || !self
                .git(
                    config,
                    &[
                        "status",
                        "--porcelain=v1",
                        "--untracked-files=all",
                        "--ignore-submodules=none",
                    ],
                )?
                .is_empty()
        {
            return Err(Error::refused(
                "local harness source must be its exact clean unshallow main checkout tracking origin/main; all work is preserved",
            ));
        }
        let flags = self.git(config, &["ls-files", "-v", "-z"])?;
        if flags
            .split('\0')
            .filter(|entry| !entry.is_empty())
            .any(|entry| !entry.starts_with("H "))
        {
            return Err(Error::refused(
                "local harness source has hidden index flags; all work is preserved",
            ));
        }
        Ok(())
    }

    fn authority(config: &Config, plan: &Plan) -> Result<LocalAuthority> {
        let row = crate::programs::get(&config.program)
            .and_then(|def| def.harness())
            .ok_or_else(|| Error::refused("local manager program has no harness declaration"))?;
        LocalAuthority::new(&config.program, row.tool, &plan.source, &config.target)
            .map_err(|e| Error::io("local authority", &e))
    }

    fn store(config: &Config) -> Result<atpkg::lock::StoreLock> {
        atpkg::lock::try_lock_store(&Layout {
            prefix: config.prefix.clone(),
        })
        .map_err(|e| Error::refused(format!("local store is unavailable: {e}")))
    }

    fn request(&self, config: &Config) -> build_local::Request {
        let mut request = config.request();
        request.jobs = request.jobs.min(self._guardian.package.jobs);
        request
    }

    fn admit_target(&self, config: &Config, plan: &Plan) -> Result<()> {
        let scope = &self._guardian.package;
        let target = plan
            .command
            .env_of("CARGO_TARGET_DIR")
            .ok_or_else(|| Error::refused("managed build lacks its exact target"))?;
        let args: Vec<std::ffi::OsString> = vec![
            "admit".into(),
            "--cache".into(),
            scope.cache.clone().into(),
            "--target-dir".into(),
            target.clone(),
            "--manifest-path".into(),
            config.checkout.join("Cargo.toml").into(),
            "--preserve-if-present".into(),
            scope.cache.join(".keep").into(),
            "--budget-bytes".into(),
            scope.budget.to_string().into(),
            "--attempt".into(),
            scope.attempt.clone().into(),
            "--producer-pid".into(),
            scope.guardian.to_string().into(),
            "--producer-group".into(),
            scope.group.to_string().into(),
            "--compiler-driver".into(),
            plan.command.program.clone().into(),
            "--registrar".into(),
            scope.registrar.clone().into(),
        ];
        let reply = atpkg::build_cache::registered::package::request_local(&args)
            .map_err(|reason| Error::refused(format!("compiler target admission: {reason}")))?;
        if reply.get("budgeted").and_then(aterm_json::Value::as_bool) != Some(true)
            || reply.get("attempt").and_then(aterm_json::Value::as_str)
                != Some(scope.attempt.as_str())
            || reply
                .get("path")
                .and_then(aterm_json::Value::as_str)
                .map(Path::new)
                != Some(Path::new(target))
        {
            return Err(Error::refused(
                "compiler target admission did not acknowledge its exact guarded budget",
            ));
        }
        Ok(())
    }
}

impl Driver for Producer<'_> {
    type Plan = Plan;
    type Candidate = StagedLocal;

    fn current(&mut self, config: &Config) -> Result<CurrentLocal> {
        let lock = Self::store(config)?;
        local_harness::observe_current(&lock, &config.program)
            .map_err(|e| Error::io("observe local current", &e))
    }

    fn update_source(&mut self, config: &Config) -> Result<String> {
        config.validate(&self.ctx.home)?;
        self.clean_main(config)?;
        let repo = crate::programs::get(&config.program)
            .ok_or_else(|| Error::refused("unknown harness"))?
            .repo;
        let path = format!("{}/{repo}", self.ctx.accounts.staging);
        let origin = self.git(config, &["remote", "get-url", "origin"])?;
        if ![
            format!("https://github.com/{path}"),
            format!("https://github.com/{path}.git"),
            format!("git@github.com:{path}.git"),
        ]
        .contains(&origin)
        {
            return Err(Error::refused(
                "origin is not the configured harness development repository",
            ));
        }
        let staging = &self.ctx.accounts.staging;
        fetch_source(self.tools, config, &origin, &|removed| {
            agh::Slug::new(staging, repo)
                .map_err(|e| e.why.clone())
                .and_then(|slug| agh_helper(&slug, removed))
        })?;
        self.clean_main(config)?;
        self.git(
            config,
            &[
                "merge-base",
                "--is-ancestor",
                "HEAD",
                "refs/remotes/origin/main",
            ],
        )?;
        self.git(
            config,
            &[
                "merge",
                "--ff-only",
                "--no-edit",
                "refs/remotes/origin/main",
            ],
        )?;
        self.clean_main(config)?;
        let source = self.git(config, &["rev-parse", "HEAD"])?;
        if source.len() != 40
            || !source
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || self.git(config, &["rev-parse", "refs/remotes/origin/main"])? != source
        {
            return Err(Error::refused(
                "local source did not fast-forward to exact origin/main",
            ));
        }
        Ok(source)
    }

    fn plan(&mut self, config: &Config, source: &str) -> Result<Plan> {
        let mut plan = build_local::plan(self.ctx, &self.request(config), self.tools)?;
        if plan.source != source || plan.triple.as_str() != config.target || plan.harness.is_none()
        {
            return Err(Error::refused(
                "source or native compiler changed during local planning",
            ));
        }
        let request = self.request(config);
        let target = self
            ._guardian
            .package
            .cache
            .join(build_local::harness_cache_key(
                &config.checkout,
                &plan.source,
                &plan.compiler,
                plan.triple,
                request.profile,
                request.policy,
                plan.cache_freshness,
            )?);
        plan.command
            .env
            .push(("CARGO_TARGET_DIR".into(), target.into()));
        plan.command.env.push((
            "ALAB_PACKAGE_CACHE_ATTEMPT".into(),
            self._guardian.package.attempt.clone().into(),
        ));
        Ok(plan)
    }

    fn identity(&self, config: &Config, plan: &Plan) -> Result<Version> {
        let identity = plan
            .harness
            .as_ref()
            .ok_or_else(|| Error::refused("missing checked harness source identity"))?;
        let row = crate::programs::get(&config.program)
            .and_then(|def| def.harness())
            .ok_or_else(|| Error::refused("unknown harness command"))?;
        Ok(Version {
            program: config.program.clone(),
            command: row.tool.into(),
            source: identity.source.clone(),
            version: identity.version.clone(),
            build: identity.build_number,
            compiler: plan.compiler.clone(),
            target: plan.triple.as_str().into(),
            policy: "unverified".into(),
            profile: match config.profile {
                LocalProfile::Release => "release",
                LocalProfile::Dev => "dev",
            }
            .into(),
        })
    }

    fn build(&mut self, config: &Config, plan: &Plan) -> Result<PathBuf> {
        let request = self.request(config);
        let dest = build_local::harness_artifact_path(&request, plan)?;
        match std::fs::symlink_metadata(&dest) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.admit_target(config, plan)?;
                Ok(build_local::build(self.ctx, &request, plan, self.tools)?.dest)
            }
            Err(e) => Err(Error::io("inspect prior immutable artifact", &e)),
            Ok(_) => {
                // A complete old artifact is re-admitted under this newly checked plan. An
                // incomplete or foreign reservation is preserved rather than overwritten.
                let lock = Self::store(config)?;
                let staged = local_harness::stage(&lock, &Self::authority(config, plan)?, &dest)
                    .map_err(|e| {
                        Error::refused(format!(
                            "prior artifact preserved; complete local admission failed: {e}"
                        ))
                    })?;
                if Version::from(staged.identity()) != self.identity(config, plan)? {
                    return Err(Error::refused(
                        "prior artifact compiler/profile differs from the new checked plan; preserved",
                    ));
                }
                self.recovered = Some((dest.clone(), staged));
                Ok(dest)
            }
        }
    }

    fn stage(&mut self, config: &Config, plan: &Plan, artifact: &Path) -> Result<StagedLocal> {
        if let Some((path, staged)) = self.recovered.take() {
            if path != artifact {
                return Err(Error::refused("recovered artifact address changed"));
            }
            return Ok(staged);
        }
        let lock = Self::store(config)?;
        let staged = local_harness::stage(&lock, &Self::authority(config, plan)?, artifact)
            .map_err(|e| Error::io("stage local harness", &e))?;
        if Version::from(staged.identity()) != self.identity(config, plan)? {
            return Err(Error::refused(
                "staged identity differs from the checked plan",
            ));
        }
        Ok(staged)
    }

    fn select(
        &mut self,
        config: &Config,
        plan: &Plan,
        candidate: &StagedLocal,
        expected: ExpectedCurrent,
    ) -> std::result::Result<(), SelectionError> {
        let lock = Self::store(config).map_err(SelectionError::Rejected)?;
        let check = || -> Result<()> {
            config.validate(&self.ctx.home)?;
            self._guardian.source.check(&self.ctx.home, config)?;
            if Version::from(candidate.identity()) != self.identity(config, plan)? {
                return Err(Error::refused(
                    "local selection candidate differs from the checked plan; current and candidate are preserved",
                ));
            }
            self.clean_main(config)?;
            // Staging (including admission of a recovered artifact) establishes
            // the candidate's bytes, not which source is current at selection.
            // Reuse the checked plan's source/count/pristine validator here.
            plan.harness
                .as_ref()
                .ok_or_else(|| Error::refused("local selection has no checked harness source"))?
                .recheck(&config.checkout)?;
            self._guardian.source.check(&self.ctx.home, config)
        };
        check().map_err(SelectionError::Rejected)?;
        local_harness::select_current(&lock, candidate, expected)
            .map_err(|e| SelectionError::Uncertain(Error::io("select local current", &e)))
    }
}

#[cfg(test)]
#[path = "harness_local_producer_tests.rs"]
mod selection_tests;

#[cfg(test)]
#[path = "harness_local_producer_auth_tests.rs"]
mod auth_tests;
