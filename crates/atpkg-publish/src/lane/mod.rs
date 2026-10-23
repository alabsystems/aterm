// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Unattended publication lanes (`atpkg-publish lane run|install|disarm <alab|linux>`,
//! [`alab`], [`linux`], [`resolve`], [`gap`], [`state`], [`hold`], [`rows`], [`report`],
//! [`install`], [`units`], [`self_check`]).

pub mod alab;
pub mod gap;
pub mod hold;
pub mod install;
pub mod linux;
pub mod report;
pub mod resolve;
pub mod rows;
pub mod state;
pub mod units;

use std::path::{Path, PathBuf};

use crate::ctx::Ctx;
use crate::error::{Error, Result};
use crate::triple::Triple;

/// Which lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LaneKind {
    /// The Apple lane: re-packs ALab programs at their newest signed source release.
    Alab,
    /// The Linux lane: adds Linux rows to the pinned builds.
    Linux,
}

impl LaneKind {
    /// `alab` | `linux`.
    /// # Errors
    ///
    /// A usage error for anything else.
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "alab" => Ok(Self::Alab),
            "linux" => Ok(Self::Linux),
            other => Err(Error::usage(format!(
                "`{other}` is not a lane (alab | linux)"
            ))),
        }
    }

    /// Its name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Alab => "alab",
            Self::Linux => "linux",
        }
    }
}

/// `lane run`'s flags.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunFlags {
    /// Decide and print; change nothing.
    pub dry_run: bool,
    /// The sha256 the unit names for this binary.
    pub sha256: Option<String>,
    /// Write the transcript to this file instead of stdout (what a unit passes).
    pub log: Option<PathBuf>,
    /// The Linux targets to serve (default: every Linux target the client ships).
    pub triples: Vec<Triple>,
}

impl RunFlags {
    /// Parse `lane run <lane>`'s flags.
    /// # Errors
    ///
    /// A usage error naming the flag.
    pub fn parse(kind: LaneKind, args: &[String]) -> Result<Self> {
        let mut flags = Self::default();
        let mut it = args.iter();
        while let Some(a) = it.next() {
            let mut value = |name: &str| {
                it.next()
                    .cloned()
                    .ok_or_else(|| Error::usage(format!("{name} needs a value")))
            };
            match a.as_str() {
                "--dry-run" => flags.dry_run = true,
                "--sha256" => {
                    let v = value("--sha256")?;
                    if v.len() != 64 || !v.bytes().all(|b| b.is_ascii_hexdigit()) {
                        return Err(Error::usage(format!(
                            "--sha256 takes 64 hex digits, not `{v}`"
                        )));
                    }
                    flags.sha256 = Some(v.to_ascii_lowercase());
                }
                "--log" => flags.log = Some(PathBuf::from(value("--log")?)),
                "--triple" if kind == LaneKind::Linux => {
                    flags.triples.push(Triple::parse(&value("--triple")?)?);
                }
                other => {
                    return Err(Error::usage(format!(
                        "`lane run {}` does not take `{other}` (--dry-run, --sha256 <hex>, \
                         --log <file>{})",
                        kind.as_str(),
                        if kind == LaneKind::Linux {
                            ", --triple <linux target>"
                        } else {
                            ""
                        }
                    )));
                }
            }
        }
        Ok(flags)
    }
}

/// `atpkg-publish lane <run|install|disarm> <alab|linux> …`.
/// # Errors
///
/// A usage error for an unknown subcommand or lane; otherwise what the subcommand
/// answers.
pub fn run(ctx: &Ctx, args: &[String]) -> Result<()> {
    let (Some(sub), Some(kind)) = (args.first(), args.get(1)) else {
        return Err(Error::usage(
            "lane <run|install|disarm> <alab|linux> [flags]",
        ));
    };
    let kind = LaneKind::parse(kind)?;
    let flags = &args[2..];
    match sub.as_str() {
        "run" => run_lane(ctx, kind, &RunFlags::parse(kind, flags)?),
        "install" => install::install(ctx, kind, &install::InstallFlags::parse(flags)?),
        "disarm" => {
            if let Some(extra) = flags.first() {
                return Err(Error::usage(format!("`lane disarm` takes no `{extra}`")));
            }
            install::disarm(ctx, kind, &install::Machine::production(ctx)?)
        }
        other => Err(Error::usage(format!(
            "`lane {other}` is not a lane command (run | install | disarm)"
        ))),
    }
}

/// The production `lane run`.
fn run_lane(ctx: &Ctx, kind: LaneKind, flags: &RunFlags) -> Result<()> {
    let exe = std::env::current_exe().map_err(|e| Error::io("find this binary's own path", &e))?;
    let pinned = self_check(&exe, flags.sha256.as_deref(), flags.dry_run, &ctx.state_dir)?;
    let me = match kind {
        LaneKind::Alab => alab::ME,
        LaneKind::Linux => linux::ME,
    };
    let tx = report::Transcript::new(
        me,
        flags
            .log
            .clone()
            .map_or(report::Sink::Stdout, report::Sink::File),
    );
    tx.say(match &pinned {
        Some(hex) => format!("snapshot verified: {} (sha256 {hex})", ctx.shown(&exe)),
        None => format!(
            "UNPINNED dry run of {} — no unit names this binary",
            ctx.shown(&exe)
        ),
    });
    let result = run_with_production_deps(ctx, kind, flags, &tx);
    if let Err(e) = &result {
        tx.say(format!("{e}"));
    }
    result
}

fn run_with_production_deps(
    ctx: &Ctx,
    kind: LaneKind,
    flags: &RunFlags,
    tx: &report::Transcript,
) -> Result<()> {
    let packer = crate::artifact::pack::Pack;
    let indexer = crate::channel::index::Index;
    match kind {
        LaneKind::Alab => {
            if !cfg!(target_os = "macos") {
                return Err(Error::refused(
                    "the alab lane packs Apple rows and runs on macOS only",
                ));
            }
            let notifier = report::Osascript;
            let deps = alab::Deps {
                packer: &packer,
                indexer: &indexer,
                notifier: &notifier,
                pin_commit: &alab::installed_pin_commit,
            };
            alab::run(ctx, &alab::Options::new(ctx, flags.dry_run), &deps, tx).map(|_| ())
        }
        LaneKind::Linux => {
            let host = Triple::host().ok_or_else(|| {
                Error::refused(
                    "this lane packs Linux rows and the client ships no target for this host",
                )
            })?;
            let opts = linux::Options::new(ctx, host, flags.dry_run, &flags.triples)?;
            let builder = linux::TrustBuilder::new(ctx);
            let deps = linux::Deps {
                packer: &packer,
                indexer: &indexer,
                builder: &builder,
                switch: &linux::live_switch,
            };
            linux::run(ctx, &opts, &deps, tx).map(|_| ())
        }
    }
}

/// The snapshot self-check.
/// # Errors
///
/// A refusal when the executable does not hash to what the unit names, is not in a
/// snapshot directory, or its `INSTALLED-FROM` names another hash; or when a signing run
/// names no hash at all.
pub fn self_check(
    exe: &Path,
    expected: Option<&str>,
    dry_run: bool,
    state_dir: &Path,
) -> Result<Option<String>> {
    let Some(expected) = expected else {
        if dry_run {
            return Ok(None);
        }
        return Err(Error::refused(format!(
            "{} was started with no --sha256 — the unattended lanes run only the snapshot `lane \
             install` laid down, whose unit names its hash; a build of your own may only \
             `--dry-run`",
            exe.display()
        )));
    };
    let actual = atpkg::tree::sha256_file(exe)
        .map_err(|e| Error::io(format!("hash {}", exe.display()), &e))?;
    if actual != expected.to_ascii_lowercase() {
        return Err(Error::refused(format!(
            "{} hashes to {actual}, but its unit names {expected} — the snapshot was replaced \
             in place; re-run `atpkg-publish lane install` from a reviewed checkout",
            exe.display()
        )));
    }
    let not_a_snapshot = |why: String| {
        Error::refused(format!(
            "{} is not a snapshot `atpkg-publish lane install` laid down ({why}) — the \
             unattended lanes run only that; a build of your own may only `--dry-run`",
            exe.display()
        ))
    };
    let real = |p: &Path| {
        p.canonicalize()
            .map_err(|e| not_a_snapshot(format!("{}: {e}", p.display())))
    };
    let (exe_real, state_real) = (real(exe)?, real(state_dir)?);
    let snapshot = exe_real
        .parent()
        .filter(|d| d.parent() == Some(state_real.as_path()))
        .filter(|d| {
            d.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("lane."))
        })
        .ok_or_else(|| not_a_snapshot(format!("not in {}/lane.<stamp>/", state_dir.display())))?;
    let record = snapshot.join("INSTALLED-FROM");
    let recorded = std::fs::read_to_string(&record)
        .map_err(|e| not_a_snapshot(format!("{}: {e}", record.display())))?;
    if !recorded.lines().any(|l| l == format!("sha256 {actual}")) {
        return Err(not_a_snapshot(format!(
            "{} records another sha256",
            record.display()
        )));
    }
    Ok(Some(actual))
}
