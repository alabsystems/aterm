// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg-publish lane install <alab|linux>` and `lane disarm`: snapshot and arm.

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
    /// # Errors
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
    /// # Errors
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

/// Whether replaced jobs and their installed definitions can start an older snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Retirement {
    /// The replaced jobs are absent and their definitions cannot reload them.
    Confirmed,
    /// The current job was armed, but older paths must remain available.
    Unknown(String),
}

/// The service manager.
pub trait ServiceManager {
    /// Lint the rendered units at `paths`.
    /// # Errors
    /// The linter's refusal.
    fn lint(&self, kind: LaneKind, paths: &[PathBuf]) -> Result<Lint>;

    /// (Re)arm the installed units at `paths`, separately reporting older jobs' retirement.
    /// # Errors
    /// Why the manager refused.
    fn arm(&self, kind: LaneKind, paths: &[PathBuf]) -> Result<Retirement>;

    /// Stop the lane's units (and the retired vendor agent).
    /// # Errors
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
    /// # Errors
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
    /// # Errors
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
/// # Errors
/// What [`install_with`] answers.
pub fn install(ctx: &Ctx, kind: LaneKind, flags: &InstallFlags) -> Result<()> {
    let tx = Transcript::new("lane install", super::report::Sink::Stdout);
    install_with(ctx, kind, *flags, &Machine::production(ctx)?, &tx).map(|_| ())
}

/// The install, over an injected machine.
/// # Errors
/// A refusal for the wrong OS, a missing machine key or record, a checkout git cannot read or
/// one that is dirty, an atpkg store other than the one `XDG_CONFIG_HOME` names, a lane run in
/// flight, a shell-era lane still in its first-exec hold, a build that failed, produced nothing
/// or saw its inputs change, a snapshot that does not hash to its build, a unit its linter
/// refuses, or a service manager that refuses to arm; an I/O error laying down the snapshot, its
/// units or the live link.
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
    let link = ctx.state_dir.join("lane");
    if !flags.no_arm
        && link.is_dir()
        && !is_symlink(&link)
        && let Some(left) = aterm_uds::deadline::copy_wait_left(&link, std::time::SystemTime::now())
    {
        return Err(Error::refused(format!(
            "the shell-era lane {} changed, or its job was stopped, less than {} min ago, so \
             macOS may still be checking the first start of a program in it, and moving it \
             aside now would strand that check — re-run this installer in {} min",
            ctx.shown(&link),
            aterm_uds::deadline::FIRST_EXEC_HANG.as_secs() / 60,
            left.as_secs().div_ceil(60)
        )));
    }

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
    if Inputs::read(repo)? != before {
        return Err(Error::refused(format!(
            "the build inputs of {} changed while the snapshot was building (HEAD, an edit, \
             or an untracked file) — nothing snapshotted; re-run when the tree is still",
            ctx.shown(repo)
        )));
    }
    let built_hash = atpkg::tree::sha256_file(&built.binary)
        .map_err(|e| Error::io(format!("hash {}", built.binary.display()), &e))?;

    let stamp = stamp(ctx.now_unix);
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
    let snapshot = snapshot_dir.keep();

    let previous = std::fs::read_link(&link)
        .ok()
        .or_else(|| retired_by(&link, &ctx.state_dir))
        .filter(|p| *p != snapshot);
    tx.say(format!(
        "snapshot {} — sha256 {sha256}{}",
        ctx.shown(&snapshot),
        previous
            .as_ref()
            .map(|p| format!(" (previous kept: {})", ctx.shown(p)))
            .unwrap_or_default()
    ));

    let spec = UnitSpec {
        kind,
        binary: binary.clone(),
        sha256: sha256.clone(),
        home: ctx.home.clone(),
        state_dir: ctx.state_dir.clone(),
        xdg_config_home: machine.config_home.clone(),
        shim_bin: ctx.pkg_prefix.join("bin"),
    };
    let unit_files = units::render(&spec);
    let rendered_dir = ctx.state_dir.join("units");
    let rendered = write_units(&rendered_dir, &unit_files)?;
    let armed = if flags.no_arm {
        Vec::new()
    } else {
        write_units(machine.unit_dir(kind), &unit_files)?
    };
    let lint = match machine
        .services
        .lint(kind, if flags.no_arm { &rendered } else { &armed })
    {
        Ok(lint) => lint,
        Err(e) => {
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
    // An armed unit may start the snapshot at once: the stamp ages that first exec for the
    // prune below, which a run's `ran` (`super::mark_ran`) ends early.
    aterm_uds::deadline::stamp_exec(&out.snapshot);
    let arm = machine.services.arm(kind, &out.armed);
    let stamped = stamp_at_bootout(&ctx.state_dir);
    if let Retirement::Unknown(why) = arm? {
        tx.warn(format!(
            "armed {}; older jobs' retirement is unconfirmed ({why}) — kept all prior \
             snapshots and the live link; log {}",
            out.armed
                .iter()
                .map(|p| ctx.shown(p))
                .collect::<Vec<_>>()
                .join(", "),
            ctx.shown(&state.log)
        ));
        return Ok(out);
    }
    // The new snapshot's job is the only one loaded and its unit the only one installed, so no
    // job can start the others again: this bootout's hold is their last.
    let armed_name = out
        .snapshot
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    for dir in stamped.iter().filter(|d| **d != out.snapshot) {
        if let Err(e) = std::fs::write(dir.join(RETIRED), &armed_name) {
            tx.warn(format!("cannot mark {} retired: {e}", ctx.shown(dir)));
        }
    }
    let now = std::time::SystemTime::now();
    let shell_era = link.is_dir() && !is_symlink(&link);
    if let Some(left) = shell_era
        .then(|| aterm_uds::deadline::copy_wait_left(&link, now))
        .flatten()
    {
        // The bootout may have stopped a first exec of a program in it: it keeps its path until
        // that hold is over (the guard above refuses until then), and the armed unit names the
        // snapshot's binary, not the link.
        tx.say(format!(
            "the shell-era lane {} stays where it is: its job was just stopped, so macOS may \
             still be checking the first start of a program in it; the first install after {} \
             min moves it aside and points it at the snapshot that install arms",
            ctx.shown(&link),
            left.as_secs().div_ceil(60)
        ));
        out.previous = Some(link.clone());
    } else {
        if shell_era {
            let legacy = aterm_tempfile::Builder::new()
                .prefix(&format!("lane.legacy.{stamp}."))
                .tempdir_in(&ctx.state_dir)
                .map_err(|e| Error::io("reserve a legacy lane snapshot", &e))?;
            std::fs::rename(&link, legacy.path())
                .map_err(|e| Error::io(format!("move {} aside", link.display()), &e))?;
            let legacy = legacy.keep();
            out.previous.get_or_insert(legacy);
        }
        swap_link(&link, &out.snapshot)?;
    }
    let mut unproven = Vec::new();
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
            match aterm_uds::deadline::copy_wait_left(&p, now) {
                None => {
                    let _ = std::fs::remove_dir_all(&p);
                }
                Some(left) => unproven.push(format!(
                    "{} ({} min left)",
                    ctx.shown(&p),
                    left.as_secs().div_ceil(60)
                )),
            }
        }
    }
    if !unproven.is_empty() {
        tx.say(format!(
            "kept {}: no run from it has shown it started, so macOS may still be checking its \
             first start; a later install removes each once a run has, or its time is up",
            unproven.join(", ")
        ));
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

/// `atpkg-publish lane disarm <lane>`: stop the units and remove them. The snapshots stay for
/// inspection; each one the stopped job could have been starting is held from the bootout.
/// # Errors
/// A refusal for the wrong OS, or why the service manager refused.
pub fn disarm(ctx: &Ctx, kind: LaneKind, machine: &Machine) -> Result<()> {
    machine.check_os(kind)?;
    let stopped = machine.services.disarm(kind);
    stamp_at_bootout(&ctx.state_dir);
    stopped?;
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
/// # Errors
/// When `repo` is not a work tree.
pub fn dirty_inputs(repo: &Path) -> Result<Vec<String>> {
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

/// Marks a snapshot held from a bootout that a successful arm of another snapshot followed: no
/// unit names it and no job can start it again, so later bootouts leave its stamp to run out.
/// It holds the armed snapshot's directory name.
pub const RETIRED: &str = "retired";

/// The snapshot whose arm retired `dir`, as its [`RETIRED`] marker names it: the predecessor of
/// an install that finds a shell-era `lane` still at its path.
fn retired_by(dir: &Path, state_dir: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(dir.join(RETIRED)).ok()?;
    let name = text.trim();
    let p = state_dir.join(name);
    (name.starts_with("lane.") && !name.contains('/') && p.is_dir()).then_some(p)
}

/// Stamp, just after a bootout, every snapshot the stopped job could have been starting: each
/// `lane.*` directory under `state_dir`, and a shell-era plain `lane`, with no `ran` and not
/// [`RETIRED`]. Which one the job named is not on disk (a failed bootstrap, a login, a removed
/// unit file), so each is held from this bootout. Returns those stamped.
fn stamp_at_bootout(state_dir: &Path) -> Vec<PathBuf> {
    let mut stamped = Vec::new();
    for entry in std::fs::read_dir(state_dir).into_iter().flatten().flatten() {
        let p = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if (name == "lane" || name.starts_with("lane."))
            && !is_symlink(&p)
            && p.is_dir()
            && !p.join(aterm_uds::deadline::RAN).exists()
            && !p.join(RETIRED).exists()
        {
            aterm_uds::deadline::stamp_exec(&p);
            stamped.push(p);
        }
    }
    stamped
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

/// The production build: the atpkg store's `targo` in the unverified lane, with `--locked` (the
/// lockfile is the reviewed dependency set) and the toolchain and flag variables scrubbed (a
/// `RUSTFLAGS` replaces the config's flags).
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
    let out = run_tool(aterm_uds::os_path!("id"), &["-u"])?;
    let uid = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || uid.is_empty() || !uid.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::cannot_run("`id -u` named no uid"));
    }
    Ok(uid)
}

/// launchd, in the operator's gui session (no root).
#[derive(Debug)]
pub struct Launchd {
    /// `~/Library/LaunchAgents`.
    pub agents_dir: PathBuf,
    /// The lanes' state root (launchd's own capture, `alab.launchd.log`, is emptied at each arm).
    pub state_dir: PathBuf,
}

impl ServiceManager for Launchd {
    fn lint(&self, _kind: LaneKind, paths: &[PathBuf]) -> Result<Lint> {
        lint_plists(paths)
    }

    fn arm(&self, _kind: LaneKind, paths: &[PathBuf]) -> Result<Retirement> {
        let uid = uid()?;
        self.arm_with(&uid, paths, |args| {
            run_tool(aterm_uds::os_path!("launchctl"), args)
        })
    }

    fn disarm(&self, _kind: LaneKind) -> Result<()> {
        let uid = uid()?;
        let mut run = |args: &[&str]| run_tool(aterm_uds::os_path!("launchctl"), args);
        if !bootout_or_absent(&uid, units::LAUNCHD_LABEL, &mut run) {
            return Err(Error::cannot_run(
                "the lane's launchd job may still be loaded",
            ));
        }
        match self.retire_vendor(&uid, &mut run) {
            Retirement::Confirmed => Ok(()),
            Retirement::Unknown(why) => Err(Error::cannot_run(why)),
        }
    }
}

impl Launchd {
    fn arm_with(
        &self,
        uid: &str,
        paths: &[PathBuf],
        mut run: impl FnMut(&[&str]) -> Result<std::process::Output>,
    ) -> Result<Retirement> {
        let retirement = self.retire_vendor(uid, &mut run);
        for p in paths {
            let domain = format!("gui/{uid}/{}", units::LAUNCHD_LABEL);
            let _ = run(&["bootout", &domain]);
            let _ = std::fs::write(self.state_dir.join("alab.launchd.log"), b"");
            let out = run(&["bootstrap", &format!("gui/{uid}"), &p.display().to_string()])?;
            if !out.status.success() {
                return Err(Error::refused(format!(
                    "launchctl bootstrap gui/{uid} {} failed: {}",
                    p.display(),
                    String::from_utf8_lossy(&out.stderr).trim()
                )));
            }
            let _ = run(&["enable", &domain]);
        }
        Ok(retirement)
    }

    fn retire_vendor(
        &self,
        uid: &str,
        run: &mut impl FnMut(&[&str]) -> Result<std::process::Output>,
    ) -> Retirement {
        if !bootout_or_absent(uid, units::RETIRED_LABEL, run) {
            return Retirement::Unknown("the legacy launchd job may still be loaded".into());
        }
        let plist = self
            .agents_dir
            .join(format!("{}.plist", units::RETIRED_LABEL));
        match std::fs::remove_file(&plist) {
            Ok(()) => Retirement::Confirmed,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Retirement::Confirmed,
            Err(e) => Retirement::Unknown(format!("cannot remove {}: {e}", plist.display())),
        }
    }
}

fn bootout_or_absent(
    uid: &str,
    label: &str,
    run: &mut impl FnMut(&[&str]) -> Result<std::process::Output>,
) -> bool {
    if run(&["bootout", &format!("gui/{uid}/{label}")]).is_ok_and(|o| o.status.success()) {
        return true;
    }
    // `launchctl list` answers for the caller's manager, not a named domain, so only an Aqua
    // manager for this non-root uid makes it the gui domain's answer. `print` stays unused:
    // launchctl documents its output as not an API.
    if uid == "0"
        || query_text(run, &["manageruid"]).as_deref() != Some(uid)
        || query_text(run, &["managername"]).as_deref() != Some("Aqua")
    {
        return false;
    }
    let Ok(out) = run(&["list"]) else {
        return false;
    };
    out.status.success()
        && out.stderr.is_empty()
        && std::str::from_utf8(&out.stdout)
            .ok()
            .and_then(|s| listed_label(s, label))
            == Some(false)
}

fn query_text(
    run: &mut impl FnMut(&[&str]) -> Result<std::process::Output>,
    args: &[&str],
) -> Option<String> {
    let out = run(args).ok()?;
    (out.status.success() && out.stderr.is_empty()).then(|| {
        String::from_utf8(out.stdout)
            .ok()
            .map(|s| s.trim().to_owned())
    })?
}

fn listed_label(text: &str, wanted: &str) -> Option<bool> {
    if !text.ends_with('\n') {
        return None;
    }
    let mut lines = text.lines();
    if lines.next()?.split_whitespace().collect::<Vec<_>>() != ["PID", "Status", "Label"] {
        return None;
    }
    let mut labels = std::collections::BTreeSet::new();
    for line in lines {
        let row = line.split_whitespace().collect::<Vec<_>>();
        let [pid, status, label] = row.as_slice() else {
            return None;
        };
        if (*pid != "-" && !pid.parse::<u32>().is_ok_and(|n| n > 0))
            || status.parse::<i32>().is_err()
            || !labels.insert(*label)
        {
            return None;
        }
    }
    Some(labels.contains(wanted))
}

/// `plutil -lint` over rendered plists — what launchd reads.
/// # Errors
/// `plutil`'s refusal, or a `plutil` that cannot run.
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

/// `systemd-analyze --user verify` over rendered units where systemd is; where it is not,
/// [`Lint::NotRun`], never a pass.
/// # Errors
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

    fn arm(&self, _kind: LaneKind, _paths: &[PathBuf]) -> Result<Retirement> {
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
        Ok(Retirement::Confirmed)
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

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::process::ExitStatusExt;

    use super::*;

    fn reply(success: bool, stdout: &[u8]) -> std::process::Output {
        std::process::Output {
            status: std::process::ExitStatus::from_raw(if success { 0 } else { 1 << 8 }),
            stdout: stdout.to_vec(),
            stderr: Vec::new(),
        }
    }

    struct Observation {
        stop: Option<std::process::Output>,
        uid: Option<std::process::Output>,
        manager: Option<std::process::Output>,
        jobs: Option<std::process::Output>,
    }

    impl Default for Observation {
        fn default() -> Self {
            Self {
                stop: Some(reply(false, b"")),
                uid: Some(reply(true, b"501\n")),
                manager: Some(reply(true, b"Aqua\n")),
                jobs: Some(reply(true, b"PID\tStatus\tLabel\n-\t0\tother.job\n")),
            }
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn legacy_retirement_needs_a_confirmed_stop_or_gui_absence_and_no_reloadable_plist() {
        let present = format!("PID\tStatus\tLabel\n-\t0\t{}\n", units::RETIRED_LABEL);
        let duplicate = b"PID\tStatus\tLabel\n-\t0\tother\n42\t0\tother\n";
        let cases = [
            (
                Observation {
                    stop: Some(reply(true, b"")),
                    ..Observation::default()
                },
                true,
            ),
            (Observation::default(), true),
            (
                Observation {
                    jobs: Some(reply(true, b"PID\tStatus\tLabel\n")),
                    ..Observation::default()
                },
                true,
            ),
            (
                Observation {
                    stop: None,
                    ..Observation::default()
                },
                true,
            ),
            (
                Observation {
                    jobs: Some(reply(true, present.as_bytes())),
                    ..Observation::default()
                },
                false,
            ),
            (
                Observation {
                    jobs: Some(reply(true, b"PID\tStatus\tLabel\n-\t0\tother")),
                    ..Observation::default()
                },
                false,
            ),
            (
                Observation {
                    jobs: Some(reply(true, duplicate)),
                    ..Observation::default()
                },
                false,
            ),
            (
                Observation {
                    jobs: Some(reply(true, b"not a job table\n")),
                    ..Observation::default()
                },
                false,
            ),
            (
                Observation {
                    jobs: Some(reply(true, b"PID\tStatus\tLabel\n?\t0\tother\n")),
                    ..Observation::default()
                },
                false,
            ),
            (
                Observation {
                    jobs: Some(reply(true, b"PID\tStatus\tLabel\n-\t?\tother\n")),
                    ..Observation::default()
                },
                false,
            ),
            (
                Observation {
                    jobs: Some(reply(true, b"PID\tStatus\tLabel\n-\t0\t\xff\n")),
                    ..Observation::default()
                },
                false,
            ),
            (
                Observation {
                    jobs: Some(reply(false, b"PID\tStatus\tLabel\n")),
                    ..Observation::default()
                },
                false,
            ),
            (
                Observation {
                    jobs: None,
                    ..Observation::default()
                },
                false,
            ),
            (
                Observation {
                    uid: Some(reply(true, b"502\n")),
                    ..Observation::default()
                },
                false,
            ),
            (
                Observation {
                    uid: None,
                    ..Observation::default()
                },
                false,
            ),
            (
                Observation {
                    manager: Some(reply(true, b"Background\n")),
                    ..Observation::default()
                },
                false,
            ),
        ];
        for (observation, confirmed) in cases {
            let scratch = aterm_tempfile::tempdir().expect("private backend fixture");
            let manager = Launchd {
                agents_dir: scratch.path().join("agents"),
                state_dir: scratch.path().join("state"),
            };
            std::fs::create_dir_all(&manager.agents_dir).expect("agents");
            std::fs::create_dir_all(&manager.state_dir).expect("state");
            let plist = manager
                .agents_dir
                .join(format!("{}.plist", units::RETIRED_LABEL));
            std::fs::write(&plist, b"legacy definition").expect("legacy plist");
            let mut calls = Vec::new();
            let result = manager
                .arm_with("501", &[scratch.path().join("current.plist")], |args| {
                    calls.push(args.join(" "));
                    let answer = match args {
                        ["bootout", target] if target.ends_with(units::RETIRED_LABEL) => {
                            &observation.stop
                        }
                        ["manageruid"] => &observation.uid,
                        ["managername"] => &observation.manager,
                        ["list"] => &observation.jobs,
                        ["bootout" | "bootstrap" | "enable", ..] => return Ok(reply(true, b"")),
                        _ => panic!("unexpected launchctl arguments: {args:?}"),
                    };
                    answer
                        .clone()
                        .ok_or_else(|| Error::cannot_run("fixture command unavailable"))
                })
                .expect("current bootstrap succeeds");
            assert_eq!(result == Retirement::Confirmed, confirmed, "{calls:?}");
            assert!(calls.iter().any(|s| s.starts_with("bootstrap gui/501 ")));
            if confirmed {
                assert!(!plist.exists());
            } else {
                assert_eq!(
                    std::fs::read(&plist).expect("unknown keeps legacy definition"),
                    b"legacy definition"
                );
            }
        }

        let scratch = aterm_tempfile::tempdir().expect("private removal fixture");
        let manager = Launchd {
            agents_dir: scratch.path().to_path_buf(),
            state_dir: scratch.path().to_path_buf(),
        };
        let plist = scratch
            .path()
            .join(format!("{}.plist", units::RETIRED_LABEL));
        let mut stopped = |_: &[&str]| Ok(reply(true, b""));
        assert_eq!(
            manager.retire_vendor("501", &mut stopped),
            Retirement::Confirmed
        );
        std::fs::create_dir(&plist).expect("obstruct plist removal");
        std::fs::write(plist.join("held"), b"legacy contents").expect("held contents");
        let result = manager
            .arm_with("501", &[scratch.path().join("current.plist")], stopped)
            .expect("current bootstrap succeeds despite removal uncertainty");
        assert!(matches!(result, Retirement::Unknown(_)));
        assert_eq!(
            std::fs::read(plist.join("held")).expect("contents retained"),
            b"legacy contents"
        );
        let mut queried = false;
        assert!(!bootout_or_absent("0", units::RETIRED_LABEL, &mut |args| {
            queried |= args != ["bootout", &format!("gui/0/{}", units::RETIRED_LABEL)];
            Ok(reply(false, b""))
        }));
        assert!(!queried, "root's legacy list would name the system domain");
    }
}
