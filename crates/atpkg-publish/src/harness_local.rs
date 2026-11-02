// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Local harness update orchestration in the owner-side publication tool. Selection changes
//! only a program's current link; adopting an OwnImage in a live harness is a separate feature.
//! A detached same-executable guardian owns cancellation, capacity admission and whole-pass
//! worker retirement. This module supplies no live-image adoption or signed authority.

use std::path::{Path, PathBuf};
use std::time::Duration;

use atpkg::local_harness::{CurrentLocal, ExpectedCurrent, LocalIdentity};
use serde::{Deserialize, Serialize};

use crate::artifact::build_local::{Policy, Profile, Request};
use crate::ctx::Ctx;
use crate::error::{Error, Result};

#[path = "harness_local_fs.rs"]
mod fs;
#[path = "harness_local_guardian.rs"]
mod guardian;
#[path = "harness_local_producer.rs"]
pub mod producer;

/// The existing publication tool's local-manager interface. `status` needs only a prefix.
pub const SYNOPSIS: &str = "harness-local <plan|once|watch> <program> --checkout DIR --out DIR --prefix DIR --native --unverified [--profile release|dev] [--jobs N] [--toolchain BIN] [--poll-seconds N] [--memory-mib N] [--vm-mib N] [--disk-mib N] [--pass-seconds N] | harness-local status <program> --prefix DIR";
/// A first pass is immediate; this is the subsequent successful poll interval.
pub const DEFAULT_POLL_SECONDS: u64 = 4 * 60 * 60;
/// Shortest supported poll, also the first failure's retry delay.
pub const MIN_POLL_SECONDS: u64 = 60;
/// Longest supported poll and failure delay.
pub const MAX_POLL_SECONDS: u64 = 24 * 60 * 60;
/// Early same-executable guardian/worker dispatch, before a binary starts any thread.
pub use guardian::early_entry;
/// Controlled same-image native ownership checks, unavailable in production builds.
#[cfg(any(test, debug_assertions))]
pub use guardian::{Fixture, FixtureLaunch, prepare_fixture};

/// The explicit local build policy. Deserialization cannot quietly select a signed lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LocalPolicy {
    /// Trust's unverified build, with no proof claim.
    Unverified,
}

/// The only Cargo profiles the producer accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LocalProfile {
    /// Optimized development artifact.
    Release,
    /// Debug artifact.
    Dev,
}

/// Exact manager configuration. One program/prefix has one lifetime lock and stable config.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Configuration format.
    pub schema: u32,
    /// Package, whose executable may have a different name.
    pub program: String,
    /// Explicit owned clean main checkout; never reset, stashed or replaced.
    pub checkout: PathBuf,
    /// Explicit owned producer output root, outside the checkout and package prefix.
    pub out: PathBuf,
    /// Explicit local package prefix.
    pub prefix: PathBuf,
    /// Explicit native target resolved by `--native`, never a cross target.
    pub target: String,
    /// Explicit unverified policy.
    pub policy: LocalPolicy,
    /// Cargo profile.
    pub profile: LocalProfile,
    /// Bounded compiler parallelism.
    pub jobs: u32,
    /// Poll after a completed pass, in seconds.
    pub poll_seconds: u64,
    /// Optional explicit immutable Trust bin directory.
    pub toolchain: Option<PathBuf>,
    /// Aggregate resident-memory watchdog and capacity claim, MiB.
    pub memory_mib: u64,
    /// Per-process virtual-memory ceiling, MiB (sampled on macOS, inherited RLIMIT_AS on Linux).
    pub vm_mib: u64,
    /// Maximum duration of one complete pass; a timeout retires its worker group.
    pub pass_seconds: u64,
    /// Disposable compiler-target budget, MiB; completed artifacts and store stages are separate.
    pub disk_mib: u64,
}

impl Config {
    /// Validate scope and ownership without creating directories or running tools.
    /// # Errors
    /// Unknown schema/program, invalid native scope/poll/jobs, linked or foreign ancestry,
    /// or overlapping checkout/output/prefix paths.
    pub fn validate(&self, home: &Path) -> Result<()> {
        if self.schema != 1
            || crate::programs::get(&self.program)
                .and_then(|def| def.harness())
                .is_none()
            || Some(self.target.as_str())
                != crate::triple::Triple::host().map(crate::triple::Triple::as_str)
            || !(MIN_POLL_SECONDS..=MAX_POLL_SECONDS).contains(&self.poll_seconds)
            || !(1..=256).contains(&self.jobs)
            || !(256..=1_048_576).contains(&self.memory_mib)
            || !(1024..=16_777_216).contains(&self.vm_mib)
            || !(60..=86_400).contains(&self.pass_seconds)
            || !(1024..=1_048_576).contains(&self.disk_mib)
        {
            return Err(Error::usage("invalid local harness configuration scope"));
        }
        fs::ancestry(home, &self.checkout, false)?;
        fs::ancestry(home, &self.out, true)?;
        fs::ancestry(home, &self.prefix, true)?;
        for (left, right) in [
            (&self.checkout, &self.out),
            (&self.checkout, &self.prefix),
            (&self.out, &self.prefix),
        ] {
            if left.starts_with(right) || right.starts_with(left) {
                return Err(Error::usage(
                    "checkout, output and prefix must be separate owned directories",
                ));
            }
        }
        if let Some(toolchain) = &self.toolchain {
            fs::ancestry(home, toolchain, false)?;
        }
        Ok(())
    }

    /// The existing producer request; it cannot modify shims or vendor selection.
    #[must_use]
    pub fn request(&self) -> Request {
        Request {
            program: self.program.clone(),
            checkout: self.checkout.clone(),
            plan: false,
            policy: Policy::Unverified,
            profile: match self.profile {
                LocalProfile::Release => Profile::Release,
                LocalProfile::Dev => Profile::Dev,
            },
            jobs: self.jobs,
            link: false,
            toolchain: self.toolchain.clone(),
            out: Some(self.out.clone()),
        }
    }

    /// Parse explicit owner choices; unknown and duplicate flags refuse.
    /// # Errors
    /// Missing explicit checkout/output/prefix/native/unverified, malformed values or flags.
    pub fn parse(args: &[String]) -> Result<Self> {
        let Some((program, args)) = args.split_first() else {
            return Err(Error::usage(SYNOPSIS));
        };
        let mut config = Self {
            schema: 1,
            program: program.clone(),
            checkout: PathBuf::new(),
            out: PathBuf::new(),
            prefix: PathBuf::new(),
            target: String::new(),
            policy: LocalPolicy::Unverified,
            profile: LocalProfile::Release,
            jobs: 1,
            poll_seconds: DEFAULT_POLL_SECONDS,
            toolchain: None,
            memory_mib: 32_768,
            vm_mib: 4_194_304,
            pass_seconds: 6 * 60 * 60,
            disk_mib: 65_536,
        };
        let mut seen = std::collections::BTreeSet::new();
        let mut words = args.iter();
        while let Some(flag) = words.next() {
            if !seen.insert(flag.as_str()) {
                return Err(Error::usage(format!("duplicate {flag}")));
            }
            if flag == "--native" {
                config.target = crate::triple::Triple::host()
                    .ok_or_else(|| Error::usage("this host has no supported native target"))?
                    .as_str()
                    .into();
                continue;
            }
            if flag == "--unverified" {
                continue;
            }
            let value = words
                .next()
                .ok_or_else(|| Error::usage(format!("{flag} needs a value")))?;
            match flag.as_str() {
                "--checkout" => config.checkout = value.into(),
                "--out" => config.out = value.into(),
                "--prefix" => config.prefix = value.into(),
                "--toolchain" => config.toolchain = Some(value.into()),
                "--profile" => {
                    config.profile = match value.as_str() {
                        "release" => LocalProfile::Release,
                        "dev" => LocalProfile::Dev,
                        _ => return Err(Error::usage("--profile requires release or dev")),
                    }
                }
                "--jobs" => {
                    config.jobs = number(value, flag)?
                        .try_into()
                        .map_err(|_| Error::usage("--jobs is too large"))?
                }
                "--poll-seconds" => config.poll_seconds = number(value, flag)?,
                "--memory-mib" => config.memory_mib = number(value, flag)?,
                "--vm-mib" => config.vm_mib = number(value, flag)?,
                "--pass-seconds" => config.pass_seconds = number(value, flag)?,
                "--disk-mib" => config.disk_mib = number(value, flag)?,
                _ => return Err(Error::usage(format!("unknown local manager flag {flag}"))),
            }
        }
        if ![
            "--native",
            "--unverified",
            "--checkout",
            "--out",
            "--prefix",
        ]
        .iter()
        .all(|flag| seen.contains(flag))
        {
            return Err(Error::usage(
                "local updates require explicit --checkout, --out, --prefix, --native and --unverified",
            ));
        }
        Ok(config)
    }
}

fn number(text: &str, flag: &str) -> Result<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Error::usage(format!(
            "{flag} requires an unsigned decimal integer"
        )));
    }
    text.parse()
        .map_err(|_| Error::usage(format!("{flag} is too large")))
}

/// Checked local identity, kept separate for compiled and selected versions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Version {
    /// Package name.
    pub program: String,
    /// Executable command.
    pub command: String,
    /// Full source commit.
    pub source: String,
    /// Full compiler commit.
    pub compiler: String,
    /// Producer version stamp.
    pub version: String,
    /// Numeric store build.
    pub build: u64,
    /// Native target.
    pub target: String,
    /// Producer lane.
    pub policy: String,
    /// Cargo profile.
    pub profile: String,
}

impl From<LocalIdentity> for Version {
    fn from(identity: LocalIdentity) -> Self {
        Self {
            program: identity.program,
            command: identity.command,
            source: identity.source,
            compiler: identity.compiler,
            version: identity.version,
            build: identity.build,
            target: identity.target,
            policy: identity.policy,
            profile: identity.profile,
        }
    }
}

/// One operation whose interrupted result must be reconciled before it is repeated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Operation {
    /// Bounded fetch and fast-forward of the explicit source.
    Fetch,
    /// Source/compiler plan.
    Plan,
    /// Compiler owns its immutable artifact reservation.
    Build,
    /// Candidate may be partially staged.
    Stage,
    /// Current may already have changed despite a lost acknowledgement.
    Select,
}

/// Persisted local status. This is observation, never authority to install recorded paths.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    /// Status format.
    pub schema: u32,
    /// Last checked source revision.
    pub source: Option<String>,
    /// Version declared by checked source before compilation.
    pub source_version: Option<Version>,
    /// Last complete compiled artifact.
    pub compiled: Option<Version>,
    /// Last checked current selection.
    pub selected: Option<Version>,
    /// Persisted before each external side effect; retained across an unacknowledged exit.
    pub pending: Option<Operation>,
    /// Human-readable local condition.
    pub reason: String,
    /// Consecutive failed passes.
    pub failures: u32,
    /// Seconds before the next pass; zero means the immediate first pass.
    pub retry_seconds: u64,
    /// Whether a live harness adopted this image. This implementation never claims it did.
    pub live_adoption: bool,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            schema: 1,
            source: None,
            source_version: None,
            compiled: None,
            selected: None,
            pending: None,
            reason: "configured; no update has run".into(),
            failures: 0,
            retry_seconds: 0,
            live_adoption: false,
        }
    }
}

/// Bounded retry with a fresh failure starting at one minute and a success resetting it.
#[must_use]
pub fn retry_delay(config: &Config, failures: u32) -> Duration {
    if failures == 0 {
        return Duration::from_secs(config.poll_seconds);
    }
    Duration::from_secs(
        MIN_POLL_SECONDS
            .saturating_mul(1_u64 << failures.saturating_sub(1).min(16))
            .min(config.poll_seconds),
    )
}

/// Effects required by the one-pass state machine. The real producer runs entirely within
/// an owned worker group; the guardian retires that group before another pass or final ACK.
/// Interrupted operations keep their immutable partials.
pub trait Driver {
    /// A checked source/compiler plan, not a persisted status record.
    type Plan;
    /// A live installer-admitted handle, never reconstructed from a status path.
    type Candidate;
    /// Observe current under a short store lock. Foreign current is an error, not absence.
    fn current(&mut self, config: &Config) -> Result<CurrentLocal>;
    /// Check clean main with exactly origin/main tracking, fetch and fast-forward only.
    fn update_source(&mut self, config: &Config) -> Result<String>;
    /// Build the existing producer plan for the new full source.
    fn plan(&mut self, config: &Config, source: &str) -> Result<Self::Plan>;
    /// Exact checked identity of this plan.
    fn identity(&self, config: &Config, plan: &Self::Plan) -> Result<Version>;
    /// Build once or reuse a completed artifact only after installer validation; preserve collisions.
    fn build(&mut self, config: &Config, plan: &Self::Plan) -> Result<PathBuf>;
    /// Admit local bytes using this plan's full SHA and explicit command/native scope.
    fn stage(
        &mut self,
        config: &Config,
        plan: &Self::Plan,
        artifact: &Path,
    ) -> Result<Self::Candidate>;
    /// Select by current CAS under a short store lock.
    fn select(
        &mut self,
        config: &Config,
        candidate: &Self::Candidate,
        expected: ExpectedCurrent,
    ) -> Result<()>;
}

/// Run one immediate pass; the caller owns its exact lifetime lock and status persistence.
/// `save` runs before each external operation. A previous interrupted select can settle by
/// checked current readback; any other interrupted operation is preserved for its guardian.
/// # Errors
/// A failed observation/effect/status write, current conflict or unresolved earlier operation.
pub fn pass<D: Driver>(
    config: &Config,
    status: &mut Status,
    driver: &mut D,
    mut save: impl FnMut(&Status) -> Result<()>,
) -> Result<()> {
    status.live_adoption = false;
    let observed = match driver.current(config) {
        Ok(observed) => observed,
        Err(error) => {
            status.selected = None;
            status.failures = status.failures.saturating_add(1);
            status.retry_seconds = retry_delay(config, status.failures).as_secs();
            status.reason = format!("current is unknown: {error}; no update started");
            save(status)?;
            return Err(error);
        }
    };
    status.selected = match &observed {
        CurrentLocal::Absent => None,
        CurrentLocal::Selected(identity) => Some(identity.clone().into()),
    };
    if let Some(pending) = status.pending {
        if pending == Operation::Select
            && status.compiled.is_some()
            && status.compiled == status.selected
        {
            status.pending = None;
            status.reason = "previous selection confirmed by checked current readback; live adoption is not implemented".into();
            status.failures = 0;
            status.retry_seconds = config.poll_seconds;
            return save(status);
        }
        status.reason = format!(
            "preserved interrupted {pending:?}; guardian retirement and partial reconciliation remain required"
        );
        status.failures = status.failures.saturating_add(1);
        status.retry_seconds = retry_delay(config, status.failures).as_secs();
        save(status)?;
        return Err(Error::refused(status.reason.clone()));
    }
    let result = (|| {
        status.pending = Some(Operation::Fetch);
        status.reason = "checking clean main and fetching origin/main".into();
        save(status)?;
        let source = driver.update_source(config)?;
        status.source = Some(source.clone());
        status.pending = Some(Operation::Plan);
        status.reason = "checking source and native compiler identity".into();
        save(status)?;
        let plan = driver.plan(config, &source)?;
        let desired = driver.identity(config, &plan)?;
        let command = crate::programs::get(&config.program)
            .and_then(|def| def.harness())
            .map(|row| row.tool);
        let profile = match config.profile {
            LocalProfile::Release => "release",
            LocalProfile::Dev => "dev",
        };
        let full_sha = |text: &str| {
            text.len() == 40
                && text
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        };
        if desired.source != source
            || !full_sha(&desired.source)
            || !full_sha(&desired.compiler)
            || desired.program != config.program
            || desired.target != config.target
            || desired.policy != "unverified"
            || Some(desired.command.as_str()) != command
            || desired.profile != profile
        {
            return Err(Error::refused(
                "producer identity differs from the explicit local configuration",
            ));
        }
        status.source_version = Some(desired.clone());
        if status.selected.as_ref() == Some(&desired) {
            status.pending = None;
            status.reason = "current already selects the checked source/compiler version; live adoption is not implemented".into();
            return Ok(());
        }
        status.pending = Some(Operation::Build);
        status.reason = "building or validating the complete immutable local artifact".into();
        save(status)?;
        let artifact = driver.build(config, &plan)?;
        status.compiled = Some(desired.clone());
        status.pending = Some(Operation::Stage);
        status.reason = "staging explicit local bytes".into();
        save(status)?;
        let candidate = driver.stage(config, &plan, &artifact)?;
        status.pending = Some(Operation::Select);
        status.reason = "selecting current by compare-and-swap".into();
        save(status)?;
        let ack = driver.select(config, &candidate, observed.expected());
        // Every ACK, including success, is followed by descriptor-based current readback.
        let now = driver.current(config)?;
        status.selected = match now {
            CurrentLocal::Absent => None,
            CurrentLocal::Selected(identity) => Some(identity.into()),
        };
        if status.selected.as_ref() != Some(&desired) {
            // Readback proved a different current, so this is a settled CAS conflict.
            // Only an unknown readback retains Select across the next backoff.
            status.pending = None;
            ack?;
            return Err(Error::refused(
                "current did not select the planned local identity",
            ));
        }
        status.pending = None;
        status.reason = if ack.is_ok() {
            "selected checked local version; live adoption is not implemented"
        } else {
            "selection confirmed after uncertain acknowledgement; live adoption is not implemented"
        }
        .into();
        Ok(())
    })();
    match result {
        Ok(()) => {
            status.failures = 0;
            status.retry_seconds = config.poll_seconds;
            save(status)
        }
        Err(error) => {
            // A selection's error may have happened after rename; retain its pending marker
            // until readback can settle it. Other returned effects satisfy Driver retirement.
            if status.pending != Some(Operation::Select) {
                status.pending = None;
            }
            status.failures = status.failures.saturating_add(1);
            status.retry_seconds = retry_delay(config, status.failures).as_secs();
            status.reason = format!("{error}; partials preserved");
            save(status)?;
            Err(error)
        }
    }
}

/// A cancellable clock owned by the future guardian. A stop must also interrupt its active
/// Driver child group; this clock alone is not a process owner.
pub trait WatchControl {
    /// Whether an explicit stop or parent lifeline cut requests retirement.
    fn stopping(&self) -> bool;
    /// Wait at most `delay`, returning true when stopping. No wait precedes the first pass.
    fn wait(&mut self, delay: Duration) -> bool;
}

/// Immediate first pass, then bounded polling/backoff until the owner requests a stop.
/// Driver failures keep the watch active; a failed journal write ends it before another pass.
/// # Errors
/// Status persistence failed; the owning guardian must retire before releasing its lock.
pub fn watch<D: Driver>(
    config: &Config,
    status: &mut Status,
    driver: &mut D,
    control: &mut impl WatchControl,
    mut save: impl FnMut(&Status) -> Result<()>,
) -> Result<()> {
    loop {
        if control.stopping() {
            return Ok(());
        }
        let mut persistence_failed = false;
        let result = pass(config, status, driver, |status| {
            let result = save(status);
            persistence_failed |= result.is_err();
            result
        });
        if persistence_failed {
            return result;
        }
        if control.stopping() {
            return Ok(());
        }
        let delay = Duration::from_secs(
            status
                .retry_seconds
                .clamp(MIN_POLL_SECONDS, MAX_POLL_SECONDS),
        );
        if control.wait(delay) {
            return Ok(());
        }
    }
}

/// Existing CLI entry. `once` and `watch` run under a detached same-executable guardian;
/// a live harness is not restarted or activated by selecting its local current link.
/// # Errors
/// Usage, ownership, lock/config conflict, failed guardian startup or a failed one-pass update.
pub fn run(ctx: &Ctx, args: &[String]) -> Result<()> {
    let Some((mode, args)) = args.split_first() else {
        return Err(Error::usage(SYNOPSIS));
    };
    if mode == "status" {
        return fs::show_status(ctx, args);
    }
    if !matches!(mode.as_str(), "plan" | "once" | "watch") {
        return Err(Error::usage(SYNOPSIS));
    }
    let config = Config::parse(args)?;
    config.validate(&ctx.home)?;
    if mode == "plan" {
        println!(
            "{}",
            aterm_json::to_string(&config).map_err(|e| Error::refused(e.to_string()))?
        );
        println!(
            "first_pass=immediate poll_seconds={} execution=guarded live_adoption=unimplemented",
            config.poll_seconds
        );
        return Ok(());
    }
    guardian::launch(ctx, &config, mode == "watch")
}

#[cfg(test)]
#[path = "harness_local_tests.rs"]
mod tests;
