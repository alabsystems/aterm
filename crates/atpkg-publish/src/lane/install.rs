// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg-publish lane install <alab|linux>` and `lane disarm`: snapshot and arm.
//!
//! * refuse a box with no machine key or record (the lane signs; a Linux builder that only
//!   stages for another machine's ceremony says `--keyless`), and a checkout whose build
//!   inputs differ from HEAD unless `--allow-dirty` (recorded);
//! * hold the lane's lock across build → swap → arm, so no run is torn down mid-publish;
//! * build `atpkg-publish` with the store's `targo --unverified build --release --locked`
//!   into `<state>/build/target.noindex` (Spotlight skips it), copy it to a unique
//!   `<state>/lane.<stamp>.<suffix>/` and
//!   record `INSTALLED-FROM` (commit, dirty list, toolchain); only a successful arm swaps
//!   the `lane` link (one rename) and prunes older snapshots, keeping the previous one for
//!   rollback; `--no-arm` only builds and renders;
//! * render the units ([`super::units`]) naming the snapshot path and its sha256, lint them
//!   (`plutil -lint`, `systemd-analyze --user verify`), and arm them: launchd booted out
//!   and bootstrapped into `gui/<uid>`; systemd reloaded and the timer enabled `--now`;
//! * `disarm` boots the agent out or disables the timer and removes the units, leaving the
//!   snapshot for inspection; it also removes the retired vendor agent.
//!
//! The build and the service manager are injected ([`Machine`]), so every step but the
//! real `launchctl`/`systemctl` is tested on rendered files in a scratch home.

use std::path::{Path, PathBuf};

use super::LaneKind;
use super::report::Transcript;
use super::state::{LaneState, Lock};
use super::units::{self, Unit, UnitSpec};
use crate::ctx::Ctx;
use crate::error::{Error, Result};
use crate::triple::Os;

/// What the build reads: a change to any of these is a change to the signer.
pub const BUILD_INPUTS: &[&str] = &[
    "crates",
    "vendor",
    "Cargo.lock",
    "Cargo.toml",
    "rust-toolchain.toml",
    ".cargo",
];

/// The snapshot's binary name.
pub const BINARY: &str = "atpkg-publish";

/// `lane install`'s flags.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InstallFlags {
    /// Snapshot a checkout whose build inputs differ from HEAD (recorded).
    pub allow_dirty: bool,
    /// Snapshot and render, keeping the live link and armed units as they are.
    pub no_arm: bool,
    /// A Linux builder with no machine key: it stages for a rostered machine.
    pub keyless: bool,
}

impl InstallFlags {
    /// Parse `lane install <lane>`'s flags.
    ///
    /// # Errors
    ///
    /// A usage error naming the flag.
    pub fn parse(args: &[String]) -> Result<Self> {
        let mut f = Self::default();
        for a in args {
            match a.as_str() {
                "--allow-dirty" => f.allow_dirty = true,
                "--no-arm" => f.no_arm = true,
                "--keyless" => f.keyless = true,
                other => {
                    return Err(Error::usage(format!(
                        "`lane install` does not take `{other}` (--allow-dirty, --no-arm, \
                         --keyless)"
                    )));
                }
            }
        }
        Ok(f)
    }
}

/// A built binary, and the toolchain that built it.
#[derive(Clone, Debug)]
pub struct Built {
    /// The binary.
    pub binary: PathBuf,
    /// `targo --unverified --version` / `trustc -V`.
    pub toolchain: String,
}

/// Builds the snapshot's binary.
pub trait SnapshotBuilder {
    /// Build `atpkg-publish` from `repo` into `target_dir`.
    ///
    /// # Errors
    ///
    /// Why the build could not run or failed.
    fn build(&self, repo: &Path, target_dir: &Path, log: &Path) -> Result<Built>;
}

/// What a linter answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lint {
    /// It read the unit and passed it.
    Passed,
    /// No linter here, and why — never a pass.
    NotRun(String),
}

/// The service manager.
pub trait ServiceManager {
    /// Lint the rendered units at `paths`.
    ///
    /// # Errors
    ///
    /// The linter's refusal.
    fn lint(&self, kind: LaneKind, paths: &[PathBuf]) -> Result<Lint>;

    /// (Re)arm the installed units at `paths`.
    ///
    /// # Errors
    ///
    /// Why the manager refused.
    fn arm(&self, kind: LaneKind, paths: &[PathBuf]) -> Result<()>;

    /// Stop the lane's units (and the retired vendor agent).
    ///
    /// # Errors
    ///
    /// Why the manager refused.
    fn disarm(&self, kind: LaneKind) -> Result<()>;
}

/// The machine a lane is installed on.
pub struct Machine {
    /// Its operating system.
    pub os: Os,
    /// Where launchd user agents live (`~/Library/LaunchAgents`).
    pub agents_dir: PathBuf,
    /// Where systemd user units live (`$XDG_CONFIG_HOME/systemd/user`).
    pub systemd_dir: PathBuf,
    /// `$XDG_CONFIG_HOME` (else `~/.config`), pinned into the units.
    pub config_home: PathBuf,
    /// Builds the binary.
    pub builder: Box<dyn SnapshotBuilder>,
    /// Arms the units.
    pub services: Box<dyn ServiceManager>,
}

impl Machine {
    /// The machine this process runs on.
    ///
    /// # Errors
    ///
    /// A refusal on a host that is neither macOS nor Linux.
    pub fn production(ctx: &Ctx) -> Result<Self> {
        let os = if cfg!(target_os = "macos") {
            Os::Darwin
        } else if cfg!(target_os = "linux") {
            Os::Linux
        } else {
            return Err(Error::refused(
                "the lanes run on macOS (alab) and Linux (linux)",
            ));
        };
        // systemd's own rule for the user unit directory.
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map_or_else(|| ctx.home.join(".config"), PathBuf::from);
        let services: Box<dyn ServiceManager> = match os {
            Os::Darwin => Box::new(Launchd {
                agents_dir: ctx.home.join("Library/LaunchAgents"),
                state_dir: ctx.state_dir.clone(),
            }),
            _ => Box::new(Systemd {
                unit_dir: config.join("systemd/user"),
            }),
        };
        Ok(Self {
            os,
            agents_dir: ctx.home.join("Library/LaunchAgents"),
            systemd_dir: config.join("systemd/user"),
            config_home: config,
            builder: Box::new(TargoBuild {
                bin: atpkg::toolchain::store_stage2_bin(&ctx.pkg_prefix),
            }),
            services,
        })
    }

    /// Refuse a lane this machine's service manager does not run.
    ///
    /// # Errors
    ///
    /// The refusal naming the OS the lane runs on.
    pub fn check_os(&self, kind: LaneKind) -> Result<()> {
        let wants = match kind {
            LaneKind::Alab => Os::Darwin,
            LaneKind::Linux => Os::Linux,
        };
        if self.os == wants {
            return Ok(());
        }
        Err(Error::refused(format!(
            "the {} lane runs on {}; this machine is not",
            kind.as_str(),
            if wants == Os::Darwin {
                "macOS"
            } else {
                "Linux"
            }
        )))
    }

    fn unit_dir(&self, kind: LaneKind) -> &Path {
        match kind {
            LaneKind::Alab => &self.agents_dir,
            LaneKind::Linux => &self.systemd_dir,
        }
    }
}

/// What an install laid down.
#[derive(Clone, Debug)]
pub struct Installed {
    /// The new snapshot directory.
    pub snapshot: PathBuf,
    /// Its binary.
    pub binary: PathBuf,
    /// Its sha256 — what the unit names.
    pub sha256: String,
    /// The snapshot it replaced, kept for a rollback.
    pub previous: Option<PathBuf>,
    /// The rendered units (`<state>/units/`).
    pub rendered: Vec<PathBuf>,
    /// What the linter answered.
    pub lint: Lint,
    /// The installed units, when armed.
    pub armed: Vec<PathBuf>,
}

/// `atpkg-publish lane install <lane> [--allow-dirty] [--no-arm] [--keyless]`.
///
/// # Errors
///
/// What [`install_with`] answers.
pub fn install(ctx: &Ctx, kind: LaneKind, flags: &InstallFlags) -> Result<()> {
    let tx = Transcript::new("lane install", super::report::Sink::Stdout);
    install_with(ctx, kind, *flags, &Machine::production(ctx)?, &tx).map(|_| ())
}

/// The install, over an injected machine.
///
/// # Errors
///
/// A refusal for the wrong OS, a missing machine key or record, a dirty checkout, a lane run in
/// flight, a build that produced nothing, a snapshot that does not hash to its build, or a
/// unit its linter refuses.
#[allow(clippy::too_many_lines)]
pub fn install_with(
    ctx: &Ctx,
    kind: LaneKind,
    flags: InstallFlags,
    machine: &Machine,
    tx: &Transcript,
) -> Result<Installed> {
    machine.check_os(kind)?;
    let keyless_ok = kind == LaneKind::Linux && flags.keyless;
    // A run signs through `identity::signer_for`, which reads both the key and its record:
    // arm nothing whose first real run would refuse.
    let identity = [
        ("machine key", &ctx.identity.key),
        ("machine record", &ctx.identity.record),
    ];
    if !keyless_ok && let Some((what, path)) = identity.iter().find(|(_, p)| !p.is_file()) {
        return Err(Error::refused(format!(
            "no {what} at {} — the lane signs; provision this machine first (`atpkg-keys \
             join --id <id>`){}",
            ctx.shown(path),
            if kind == LaneKind::Linux {
                ", or say --keyless for a builder that only stages for a rostered machine's \
                 ceremony"
            } else {
                ""
            }
        )));
    }
    let repo = &ctx.repo_root;
    let before = Inputs::read(repo)?;
    let commit = before.commit.clone();
    let dirty = dirty_inputs(repo)?;
    if !dirty.is_empty() && !flags.allow_dirty {
        return Err(Error::refused(format!(
            "these build inputs differ from commit {} and would be snapshotted into the \
             signer:\n    {}\n  commit them first, or say --allow-dirty on purpose \
             (INSTALLED-FROM records the list)",
            &commit[..12.min(commit.len())],
            dirty.join("\n    ")
        )));
    }
    let on_main = crate::git::run(
        repo,
        &["merge-base", "--is-ancestor", "HEAD", "origin/main"],
    )
    .is_ok();
    if !on_main {
        tx.say(format!(
            "NOTE — HEAD {} is not on origin/main; push it, then re-run this installer",
            &commit[..12.min(commit.len())]
        ));
    }

    // The unit pins XDG_CONFIG_HOME: the store a run resolves through it must be the one
    // whose shims the unit's PATH names.
    if atpkg::toolchain::atpkg_prefix(&ctx.home, Some(&machine.config_home)) != ctx.pkg_prefix {
        return Err(Error::refused(format!(
            "the atpkg store this install resolved ({}) is not the one {} names — set \
             XDG_CONFIG_HOME the same way for both",
            ctx.shown(&ctx.pkg_prefix),
            ctx.shown(&machine.config_home)
        )));
    }
    let state = LaneState::of(kind, &ctx.state_dir);
    state.ensure()?;
    let _guard = match state.try_lock()? {
        Lock::Held(g) => g,
        Lock::Busy => {
            return Err(Error::refused(format!(
                "a {} lane run is in flight (log {}) — re-run this installer when it finishes",
                kind.as_str(),
                ctx.shown(&state.log)
            )));
        }
    };

    // --- build into a target dir of its own, never the checkout's ----------------------
    let build_dir = ctx.state_dir.join("build");
    std::fs::create_dir_all(&build_dir)
        .map_err(|e| Error::io(format!("mkdir {}", build_dir.display()), &e))?;
    let log = build_dir.join("targo-lane.log");
    tx.say(format!(
        "building {BINARY} (targo --unverified build --release --locked) — log: {}",
        ctx.shown(&log)
    ));
    let built = machine
        .builder
        .build(repo, &build_dir.join("target.noindex"), &log)?;
    // The checkout is shared: an edit or commit that landed during the build was compiled
    // into the signer, and INSTALLED-FROM would name a tree it was not built from.
    if Inputs::read(repo)? != before {
        return Err(Error::refused(format!(
            "the build inputs of {} changed while the snapshot was building (HEAD, an edit, \
             or an untracked file) — nothing snapshotted; re-run when the tree is still",
            ctx.shown(repo)
        )));
    }
    let built_hash = atpkg::tree::sha256_file(&built.binary)
        .map_err(|e| Error::io(format!("hash {}", built.binary.display()), &e))?;

    // --- a fresh snapshot, beside the live one -------------------------------------------
    let stamp = stamp(ctx.now_unix);
    // An incremental reinstall can start in the same second as the previous one.
    // Armed units name these immutable paths directly, so a clock stamp is only a
    // readable prefix: exclusive creation supplies the unique suffix.
    let snapshot_dir = aterm_tempfile::Builder::new()
        .prefix(&format!("lane.{stamp}."))
        .tempdir_in(&ctx.state_dir)
        .map_err(|e| Error::io("create a fresh lane snapshot", &e))?;
    let snapshot = snapshot_dir.path().to_path_buf();
    let binary = snapshot.join(BINARY);
    std::fs::copy(&built.binary, &binary)
        .map_err(|e| Error::io(format!("copy into {}", snapshot.display()), &e))?;
    set_mode(&binary, 0o755)?;
    let sha256 = atpkg::tree::sha256_file(&binary)
        .map_err(|e| Error::io(format!("hash {}", binary.display()), &e))?;
    if sha256 != built_hash {
        return Err(Error::refused(format!(
            "the snapshot {} does not hash to the binary just built — refusing to arm it",
            ctx.shown(&binary)
        )));
    }
    let installed_from = format!(
        "commit {commit}{}{} of {}, snapshotted {} by atpkg-publish lane install {}\n\
         tool build: {}\nsha256 {sha256}\n",
        if on_main {
            ""
        } else {
            " (not on origin/main when snapshotted)"
        },
        if dirty.is_empty() {
            String::new()
        } else {
            format!(" (dirty: {})", dirty.join(" "))
        },
        ctx.shown(repo),
        aterm_types::rfc3339::format_rfc3339(u64::try_from(ctx.now_unix).unwrap_or(0)),
        kind.as_str(),
        built.toolchain
    );
    super::report::write_atomic(&snapshot.join("INSTALLED-FROM"), &installed_from)?;
    // From rendering onward a unit may name this directory. Failed linting or
    // arming also leaves it available for inspection and a later successful install.
    let snapshot = snapshot_dir.keep();

    // Older (shell-lane) units name paths through the `lane` link, so it moves only once
    // the manager has switched to this binary: --no-arm and a failed arm must leave the old
    // executable reachable.
    let link = ctx.state_dir.join("lane");
    let previous = std::fs::read_link(&link).ok().filter(|p| *p != snapshot);
    tx.say(format!(
        "snapshot {} — sha256 {sha256}{}",
        ctx.shown(&snapshot),
        previous
            .as_ref()
            .map(|p| format!(" (previous kept: {})", ctx.shown(p)))
            .unwrap_or_default()
    ));

    // --- render, lint, arm -----------------------------------------------------------------
    let spec = UnitSpec {
        kind,
        binary: binary.clone(),
        sha256: sha256.clone(),
        home: ctx.home.clone(),
        state_dir: ctx.state_dir.clone(),
        xdg_config_home: machine.config_home.clone(),
        shim_bin: ctx.pkg_prefix.join("bin"),
    };
    let rendered_dir = ctx.state_dir.join("units");
    let rendered = write_units(&rendered_dir, &units::render(&spec))?;
    // Linted where the manager will read them: systemd resolves a unit's references
    // against its own search path, so an armed unit is linted in place.
    let armed = if flags.no_arm {
        Vec::new()
    } else {
        write_units(machine.unit_dir(kind), &units::render(&spec))?
    };
    let lint = match machine
        .services
        .lint(kind, if flags.no_arm { &rendered } else { &armed })
    {
        Ok(lint) => lint,
        Err(e) => {
            // A unit its linter refuses never stays where the manager would load it.
            for p in &armed {
                let _ = std::fs::remove_file(p);
            }
            return Err(e);
        }
    };
    match &lint {
        Lint::Passed => tx.say("units lint clean"),
        Lint::NotRun(why) => tx.warn(format!("units NOT linted ({why})")),
    }
    let mut out = Installed {
        snapshot,
        binary,
        sha256,
        previous,
        rendered,
        lint,
        armed,
    };
    if flags.no_arm {
        tx.say(format!(
            "--no-arm — snapshot built and units rendered under {}; live link kept",
            ctx.shown(&rendered_dir)
        ));
        return Ok(out);
    }
    machine.services.arm(kind, &out.armed)?;
    // Commit the convenience/legacy alias only once every arm step succeeds. Reserve
    // a unique empty path for an older plain directory so it is retained for rollback.
    if link.is_dir() && !is_symlink(&link) {
        let legacy = aterm_tempfile::Builder::new()
            .prefix(&format!("lane.legacy.{stamp}."))
            .tempdir_in(&ctx.state_dir)
            .map_err(|e| Error::io("reserve a legacy lane snapshot", &e))?;
        std::fs::rename(&link, legacy.path())
            .map_err(|e| Error::io(format!("move {} aside", link.display()), &e))?;
        out.previous = Some(legacy.keep());
    }
    swap_link(&link, &out.snapshot)?;
    // Until arming succeeds the manager may still hold an older unit naming any
    // retained snapshot. Failed and --no-arm installs therefore never prune.
    for entry in std::fs::read_dir(&ctx.state_dir)
        .map_err(|e| Error::io(format!("read {}", ctx.state_dir.display()), &e))?
        .flatten()
    {
        let p = entry.path();
        let is_snapshot = entry.file_name().to_string_lossy().starts_with("lane.");
        if is_snapshot
            && !is_symlink(&p)
            && p.is_dir()
            && p != out.snapshot
            && Some(&p) != out.previous.as_ref()
        {
            let _ = std::fs::remove_dir_all(&p);
        }
    }
    tx.say(format!(
        "armed {} — {}",
        out.armed
            .iter()
            .map(|p| ctx.shown(p))
            .collect::<Vec<_>>()
            .join(", "),
        match kind {
            LaneKind::Alab => format!(
                "runs every 6 h; log {}; start a run now: launchctl kickstart gui/$(id -u)/{}",
                ctx.shown(&state.log),
                units::LAUNCHD_LABEL
            ),
            LaneKind::Linux => format!(
                "runs hourly; log {}; start a run now: systemctl --user start {}",
                ctx.shown(&state.log),
                units::SYSTEMD_SERVICE
            ),
        }
    ));
    Ok(out)
}

/// `atpkg-publish lane disarm <lane>`: stop the units and remove them; the snapshot is
/// left for inspection.
///
/// # Errors
///
/// Why the service manager refused.
pub fn disarm(ctx: &Ctx, kind: LaneKind, machine: &Machine) -> Result<()> {
    machine.check_os(kind)?;
    machine.services.disarm(kind)?;
    let dir = machine.unit_dir(kind);
    let names: Vec<String> = match kind {
        LaneKind::Alab => vec![
            format!("{}.plist", units::LAUNCHD_LABEL),
            format!("{}.plist", units::RETIRED_LABEL),
        ],
        LaneKind::Linux => vec![units::SYSTEMD_SERVICE.into(), units::SYSTEMD_TIMER.into()],
    };
    for n in names {
        let _ = std::fs::remove_file(dir.join(n));
    }
    println!(
        "lane disarm: the {} lane is stopped and its units removed (the snapshot under {} is \
         left for inspection)",
        kind.as_str(),
        ctx.shown(&ctx.state_dir.join("lane"))
    );
    Ok(())
}

/// What a build of `repo` reads, as of now: HEAD, the build inputs' diff against it, and
/// each untracked input's hash.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Inputs {
    commit: String,
    diff: String,
    untracked: Vec<(String, String)>,
}

impl Inputs {
    fn read(repo: &Path) -> Result<Self> {
        let mut diff_args = vec!["diff", "--binary", "HEAD", "--"];
        diff_args.extend(BUILD_INPUTS.iter().copied());
        let mut untracked_args = vec!["ls-files", "--others", "--exclude-standard", "--"];
        untracked_args.extend(BUILD_INPUTS.iter().copied());
        let untracked = crate::git::run(repo, &untracked_args)?
            .lines()
            .map(|f| {
                let hash = atpkg::tree::sha256_file(&repo.join(f))
                    .map_err(|e| Error::io(format!("hash {f}"), &e))?;
                Ok((f.to_string(), hash))
            })
            .collect::<Result<_>>()?;
        Ok(Self {
            commit: crate::git::head(repo)?,
            diff: crate::git::run(repo, &diff_args)?,
            untracked,
        })
    }
}

/// The build inputs that differ from HEAD: changed against it, or untracked.
///
/// # Errors
///
/// When `repo` is not a work tree.
pub fn dirty_inputs(repo: &Path) -> Result<Vec<String>> {
    // Names only: a status column would be trimmed off the first line of git's output.
    let mut dirty = Vec::new();
    for head in [
        &["diff", "--name-only", "HEAD", "--"][..],
        &["ls-files", "--others", "--exclude-standard", "--"][..],
    ] {
        let mut args = head.to_vec();
        args.extend(BUILD_INPUTS.iter().copied());
        dirty.extend(crate::git::run(repo, &args)?.lines().map(str::to_string));
    }
    dirty.sort();
    dirty.dedup();
    Ok(dirty)
}

/// `YYYYMMDDTHHMMSSZ`.
#[must_use]
pub fn stamp(now_unix: i64) -> String {
    aterm_types::rfc3339::format_rfc3339(u64::try_from(now_unix).unwrap_or(0))
        .replace(['-', ':'], "")
}

fn is_symlink(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())
}

/// Point `link` at `target` in one `rename(2)` (which replaces a symlink, never follows
/// it), so the path is never absent.
fn swap_link(link: &Path, target: &Path) -> Result<()> {
    let tmp = link.with_extension(format!("tmp-{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    symlink(target, &tmp)?;
    std::fs::rename(&tmp, link).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        Error::io(format!("swap {}", link.display()), &e)
    })?;
    match std::fs::read_link(link) {
        Ok(p) if p == target => Ok(()),
        _ => Err(Error::refused(format!(
            "{} does not point at {} after the swap",
            link.display(),
            target.display()
        ))),
    }
}

#[cfg(unix)]
fn symlink(target: &Path, at: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, at)
        .map_err(|e| Error::io(format!("symlink {}", at.display()), &e))
}

#[cfg(not(unix))]
fn symlink(_target: &Path, at: &Path) -> Result<()> {
    Err(Error::cannot_run(format!(
        "{}: the lanes run on macOS and Linux",
        at.display()
    )))
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|e| Error::io(format!("chmod {}", path.display()), &e))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

fn write_units(dir: &Path, units: &[Unit]) -> Result<Vec<PathBuf>> {
    units
        .iter()
        .map(|u| {
            let p = dir.join(&u.name);
            super::report::write_atomic(&p, &u.text)?;
            set_mode(&p, 0o644)?;
            Ok(p)
        })
        .collect()
}

/// The production build: the atpkg store's `targo`, in the explicit unverified lane, with
/// `--locked` (the lockfile is the reviewed dependency set) and the scrubbed environment
/// no stage of the workspace may inherit (a `RUSTFLAGS` replaces the config's flags).
#[derive(Debug)]
pub struct TargoBuild {
    /// The store's Trust bin dir.
    pub bin: PathBuf,
}

impl SnapshotBuilder for TargoBuild {
    fn build(&self, repo: &Path, target_dir: &Path, log: &Path) -> Result<Built> {
        let targo = self.bin.join("targo");
        let version = |tool: &Path, args: &[&str]| {
            aterm_uds::spawnfd::command(tool)
                .args(args)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        };
        let targo_v = version(&targo, &["--unverified", "--version"]).ok_or_else(|| {
            Error::refused(format!(
                "no Trust targo at {} answering `--unverified --version` — `aterm pkg install \
                 trust`, then re-run",
                targo.display()
            ))
        })?;
        let trustc_v = version(&self.bin.join("trustc"), &["-V"]).unwrap_or_default();
        let log_file = std::fs::File::create(log)
            .map_err(|e| Error::io(format!("create {}", log.display()), &e))?;
        let err_file = log_file
            .try_clone()
            .map_err(|e| Error::io(format!("open {}", log.display()), &e))?;
        let status = aterm_uds::spawnfd::command(&targo)
            .current_dir(repo)
            .args([
                "--unverified",
                "build",
                "--release",
                "--locked",
                "-p",
                BINARY,
            ])
            .env("CARGO_TARGET_DIR", target_dir)
            .env_remove("RUSTUP_TOOLCHAIN")
            .env_remove("RUSTC")
            .env_remove("RUSTFLAGS")
            .env_remove("RUSTDOCFLAGS")
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .env_remove("CARGO_BUILD_TARGET")
            .stdout(log_file)
            .stderr(err_file)
            .status()
            .map_err(|e| Error::io(format!("run {}", targo.display()), &e))?;
        if !status.success() {
            return Err(Error::refused(format!(
                "building {BINARY} failed (log: {})",
                log.display()
            )));
        }
        let binary = target_dir.join("release").join(BINARY);
        if !binary.is_file() {
            return Err(Error::refused(format!(
                "the build produced no {}",
                binary.display()
            )));
        }
        Ok(Built {
            binary,
            toolchain: format!("{targo_v} / {trustc_v}"),
        })
    }
}

fn run_tool(tool: &str, args: &[&str]) -> Result<std::process::Output> {
    aterm_uds::spawnfd::command(tool)
        .args(args)
        .output()
        .map_err(|e| Error::io(format!("run {tool}"), &e))
}

fn uid() -> Result<String> {
    let out = run_tool("id", &["-u"])?;
    let uid = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if uid.is_empty() || !uid.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::cannot_run("`id -u` named no uid"));
    }
    Ok(uid)
}

/// launchd, in the operator's gui session (no root).
#[derive(Debug)]
pub struct Launchd {
    /// `~/Library/LaunchAgents`.
    pub agents_dir: PathBuf,
    /// The lanes' state root (launchd's own capture is rotated per arm).
    pub state_dir: PathBuf,
}

impl ServiceManager for Launchd {
    fn lint(&self, _kind: LaneKind, paths: &[PathBuf]) -> Result<Lint> {
        lint_plists(paths)
    }

    fn arm(&self, _kind: LaneKind, paths: &[PathBuf]) -> Result<()> {
        let uid = uid()?;
        self.retire_vendor(&uid);
        for p in paths {
            let domain = format!("gui/{uid}/{}", units::LAUNCHD_LABEL);
            // Not loaded is the usual answer, and the lock means no run is in flight.
            let _ = run_tool("launchctl", &["bootout", &domain]);
            // launchd never rotates its own capture: fresh per arm.
            let _ = std::fs::write(self.state_dir.join("alab.launchd.log"), b"");
            let out = run_tool(
                "launchctl",
                &["bootstrap", &format!("gui/{uid}"), &p.display().to_string()],
            )?;
            if !out.status.success() {
                return Err(Error::refused(format!(
                    "launchctl bootstrap gui/{uid} {} failed: {}",
                    p.display(),
                    String::from_utf8_lossy(&out.stderr).trim()
                )));
            }
            let _ = run_tool("launchctl", &["enable", &domain]);
        }
        Ok(())
    }

    fn disarm(&self, _kind: LaneKind) -> Result<()> {
        let uid = uid()?;
        let _ = run_tool(
            "launchctl",
            &["bootout", &format!("gui/{uid}/{}", units::LAUNCHD_LABEL)],
        );
        self.retire_vendor(&uid);
        Ok(())
    }
}

impl Launchd {
    fn retire_vendor(&self, uid: &str) {
        let _ = run_tool(
            "launchctl",
            &["bootout", &format!("gui/{uid}/{}", units::RETIRED_LABEL)],
        );
        let _ = std::fs::remove_file(
            self.agents_dir
                .join(format!("{}.plist", units::RETIRED_LABEL)),
        );
    }
}

/// `plutil -lint` over rendered plists — what launchd reads. Off macOS there is no
/// `plutil`: said, never a pass.
///
/// # Errors
///
/// `plutil`'s refusal.
pub fn lint_plists(paths: &[PathBuf]) -> Result<Lint> {
    if !Path::new("/usr/bin/plutil").is_file() {
        return Ok(Lint::NotRun("no /usr/bin/plutil on this host".into()));
    }
    for p in paths {
        let out = run_tool("/usr/bin/plutil", &["-lint", &p.display().to_string()])?;
        if !out.status.success() {
            return Err(Error::refused(format!(
                "the rendered plist does not lint: {}",
                String::from_utf8_lossy(&out.stdout).trim()
            )));
        }
    }
    Ok(Lint::Passed)
}

/// systemd, as the operator's user manager (no root).
#[derive(Debug)]
pub struct Systemd {
    /// `$XDG_CONFIG_HOME/systemd/user`.
    pub unit_dir: PathBuf,
}

/// `systemd-analyze --user verify` over rendered units, where systemd is (said, never a
/// pass, where it is not).
///
/// # Errors
///
/// The verifier's refusal.
pub fn lint_systemd(paths: &[PathBuf]) -> Result<Lint> {
    let Ok(out) = aterm_uds::spawnfd::command("systemd-analyze")
        .arg("--user")
        .arg("verify")
        .args(paths)
        .output()
    else {
        return Ok(Lint::NotRun("no systemd-analyze on this host".into()));
    };
    if out.status.success() {
        Ok(Lint::Passed)
    } else {
        Err(Error::refused(format!(
            "systemd-analyze --user verify refuses the rendered units: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

impl ServiceManager for Systemd {
    fn lint(&self, _kind: LaneKind, paths: &[PathBuf]) -> Result<Lint> {
        lint_systemd(paths)
    }

    fn arm(&self, _kind: LaneKind, _paths: &[PathBuf]) -> Result<()> {
        for args in [
            &["--user", "daemon-reload"][..],
            &["--user", "enable", "--now", units::SYSTEMD_TIMER][..],
        ] {
            let out = run_tool("systemctl", args)?;
            if !out.status.success() {
                return Err(Error::refused(format!(
                    "systemctl {} failed: {}",
                    args.join(" "),
                    String::from_utf8_lossy(&out.stderr).trim()
                )));
            }
        }
        Ok(())
    }

    fn disarm(&self, _kind: LaneKind) -> Result<()> {
        let _ = run_tool(
            "systemctl",
            &["--user", "disable", "--now", units::SYSTEMD_TIMER],
        );
        for n in [units::SYSTEMD_SERVICE, units::SYSTEMD_TIMER] {
            let _ = std::fs::remove_file(self.unit_dir.join(n));
        }
        let _ = run_tool("systemctl", &["--user", "daemon-reload"]);
        Ok(())
    }
}
