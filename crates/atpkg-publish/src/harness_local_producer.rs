// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Existing producer/stager adapter. Its constructor requires a permit from the actual
//! same-image guardian gate, held locks and admitted covering capacity claim.

use std::path::{Path, PathBuf};

use atpkg::local_harness::{self, CurrentLocal, ExpectedCurrent, LocalAuthority, StagedLocal};
use atpkg::store::Layout;

use super::{Config, Driver, LocalProfile, Version};
use crate::artifact::build_local::{self, Plan};
use crate::artifact::tools::{Run, Tools};
use crate::ctx::Ctx;
use crate::error::{Error, Result};
use crate::host::rest::{GitAuth, git_environment_removals, git_tracing_environment};

fn source_git(
    tools: &dyn Tools,
    config: &Config,
    args: &[&str],
    auth: Option<&GitAuth>,
) -> Result<String> {
    let mut run = Run::new("git", args.iter().copied()).bounded();
    run.cwd = Some(config.checkout.clone());
    run.env_remove.extend(
        [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_NAMESPACE",
            "GIT_CONFIG",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_PARAMETERS",
        ]
        .into_iter()
        .map(String::from),
    );
    run.env_remove.extend(git_environment_removals());
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
    if let Some(auth) = auth {
        run.args.splice(0..0, auth.arguments());
        run.env.extend(
            auth.environment()
                .map_err(|e| Error::refused(e.to_string()))?,
        );
    }
    let redact = |text: String| match auth {
        Some(auth) => auth.redact(&text),
        None => text,
    };
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

fn fetch_source(tools: &dyn Tools, config: &Config, origin: &str, token: &Path) -> Result<()> {
    // The caller already matched origin against its configured development repository.
    let auth = if origin.starts_with("https://") {
        Some(
            GitAuth::required(token, "https://github.com")
                .map_err(|e| Error::refused(format!("local source credential: {e}")))?,
        )
    } else {
        None
    };
    let mut args = vec![
        "-c",
        "core.askPass=/usr/bin/false",
        "-c",
        "protocol.ext.allow=never",
        "-c",
        "protocol.file.allow=never",
        "fetch",
        "--no-tags",
        "origin",
        "refs/heads/main:refs/remotes/origin/main",
    ];
    // Authenticated requests reset helpers before installing the known scoped helper.
    if auth.is_none() {
        args.splice(0..0, ["-c", "credential.helper="]);
    }
    source_git(tools, config, &args, auth.as_ref()).map(|_| ())
}

/// Minted only inside the verified worker gate. The detached owner holds its lifeline,
/// exact locks and capacity claim until the whole worker group retires.
pub struct QualifiedGuardian {
    package: PackageScope,
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
        source_git(self.tools, config, args, None)
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
        fetch_source(self.tools, config, &origin, &self.ctx.tokens.staging)?;
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
        let profile = match config.profile {
            LocalProfile::Release => "release",
            LocalProfile::Dev => "dev",
        };
        let target = self._guardian.package.cache.join(format!(
            "{}-{}-{profile}.noindex",
            plan.source, plan.compiler
        ));
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
        candidate: &StagedLocal,
        expected: ExpectedCurrent,
    ) -> Result<()> {
        let lock = Self::store(config)?;
        local_harness::select_current(&lock, candidate, expected)
            .map_err(|e| Error::io("select local current", &e))
    }
}

#[cfg(test)]
mod auth_tests {
    use std::cell::RefCell;

    use super::*;
    use crate::artifact::tools::{Ran, RunError};
    use crate::host::rest::GIT_TOKEN_ENV;

    struct Recording {
        calls: RefCell<Vec<Run>>,
        answer: Ran,
    }

    impl Recording {
        fn new(code: i32, output: &str) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                answer: Ran {
                    code: Some(code),
                    stdout: Vec::new(),
                    stderr: output.as_bytes().to_vec(),
                },
            }
        }
    }

    impl Tools for Recording {
        fn run(&self, run: &Run) -> std::result::Result<Ran, RunError> {
            self.calls.borrow_mut().push(run.clone());
            Ok(self.answer.clone())
        }
        fn strip(&self, _: &Path, _: bool) -> std::result::Result<(), String> {
            panic!("fetch never strips")
        }
        fn resign(&self, _: &Path) -> std::result::Result<(), String> {
            panic!("fetch never signs")
        }
        fn same_unsigned(&self, _: &Path, _: &Path) -> std::result::Result<bool, String> {
            panic!("fetch never compares images")
        }
        fn thin(&self, _: &Path, _: &str, _: &Path) -> std::result::Result<(), String> {
            panic!("fetch never thins")
        }
        fn relocate(
            &self,
            _: &Path,
            _: &[PathBuf],
        ) -> std::result::Result<crate::artifact::relocate::Report, String> {
            panic!("fetch never relocates")
        }
        fn compressor(&self) -> &dyn crate::artifact::tarwrite::Compressor {
            panic!("fetch never compresses")
        }
    }

    fn config(home: &Path) -> Config {
        Config::parse(&[
            "claude-code-alab".into(),
            "--checkout".into(),
            home.join("source").display().to_string(),
            "--out".into(),
            home.join("out").display().to_string(),
            "--prefix".into(),
            home.join("pkg").display().to_string(),
            "--native".into(),
            "--unverified".into(),
        ])
        .expect("explicit fixture config")
    }

    #[test]
    fn https_fetch_uses_the_configured_credential_only_in_the_known_helper_environment() {
        let dir = aterm_tempfile::TempDir::new().expect("scratch");
        let config = config(dir.path());
        let token = dir.path().join("credential");
        let secret = "github_pat_harness_fixture";
        std::fs::write(&token, secret).expect("fixture credential");
        let tools = Recording::new(0, "");
        fetch_source(
            &tools,
            &config,
            "https://github.com/fixture/claude-code-alab.git",
            &token,
        )
        .expect("recorded fetch");
        let calls = tools.calls.borrow();
        let [run] = calls.as_slice() else {
            panic!("fetch must run exactly once")
        };
        let args = run.argv();
        assert_eq!(args[..2], ["-c", "credential.helper="]);
        let helper = args
            .iter()
            .position(|arg| arg.starts_with("credential.https://github.com.helper=!"))
            .expect("known scoped helper");
        let fetch = args
            .iter()
            .position(|arg| arg == "fetch")
            .expect("actual fetch");
        assert!(helper < fetch);
        assert!(!args.iter().any(|arg| arg.contains(secret)));
        assert_eq!(run.env_of(GIT_TOKEN_ENV), Some(&secret.into()));
        assert_eq!(run.env_of("GIT_TERMINAL_PROMPT"), Some(&"0".into()));
        for name in ["GIT_TRACE2", "GIT_TRACE2_EVENT", "GIT_TRACE2_PERF"] {
            assert_eq!(run.env_of(name), Some(&"0".into()));
        }
        assert_eq!(run.env_of("GIT_TRACE_REDACT"), Some(&"1".into()));
        assert_eq!(run.env_of("GIT_ASKPASS"), Some(&"/usr/bin/false".into()));
        for name in [
            GIT_TOKEN_ENV,
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "GIT_CURL_VERBOSE",
            "SHELLOPTS",
        ] {
            assert!(run.env_remove.iter().any(|entry| entry == name));
        }
        assert_eq!(run.cwd.as_ref(), Some(&config.checkout));
        assert!(run.deadline.is_some());
    }

    #[test]
    fn absent_https_credential_starts_no_git_and_fetch_failures_redact_credentials() {
        let dir = aterm_tempfile::TempDir::new().expect("scratch");
        let config = config(dir.path());
        let token = dir.path().join("credential");
        let tools = Recording::new(0, "");
        assert!(
            fetch_source(
                &tools,
                &config,
                "https://github.com/fixture/claude-code-alab.git",
                &token
            )
            .is_err()
        );
        assert!(tools.calls.borrow().is_empty());
        let secret = ["github_", "pat_failed_fetch_fixture"].concat();
        std::fs::write(&token, &secret).expect("fixture credential");
        let failed = Recording::new(128, &format!("remote diagnostic {secret}"));
        let error = fetch_source(
            &failed,
            &config,
            "https://github.com/fixture/claude-code-alab.git",
            &token,
        )
        .expect_err("refused fetch")
        .to_string();
        assert!(!error.contains(&secret));
        assert!(error.contains("[redacted]"));
        assert_eq!(failed.calls.borrow().len(), 1);
    }

    #[test]
    fn ssh_fetch_keeps_batch_mode_and_never_reads_the_https_credential() {
        let dir = aterm_tempfile::TempDir::new().expect("scratch");
        let config = config(dir.path());
        let tools = Recording::new(0, "");
        fetch_source(
            &tools,
            &config,
            "git@github.com:fixture/claude-code-alab.git",
            &dir.path().join("absent-credential"),
        )
        .expect("recorded SSH fetch");
        let calls = tools.calls.borrow();
        let [run] = calls.as_slice() else {
            panic!("fetch must run exactly once")
        };
        assert!(run.env_of(GIT_TOKEN_ENV).is_none());
        assert_eq!(
            run.env_of("GIT_SSH_COMMAND"),
            Some(&"ssh -oBatchMode=yes -oNumberOfPasswordPrompts=0".into())
        );
        assert!(!run.argv().iter().any(|arg| arg.contains("helper=!")));
        assert_eq!(run.argv()[..2], ["-c", "credential.helper="]);
    }
}
