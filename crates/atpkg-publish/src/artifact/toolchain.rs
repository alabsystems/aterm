// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The Trust toolchain a pack builds with: resolved once to a physical, build-numbered
//! directory, held by an `atpkg` lease for the whole run, and admitted by content and
//! lineage before anything is built.
//!
//! An explicit bin dir (`--toolchain`, which must hold `targo` and `trustc`, refused rather
//! than set aside) wins; else the store's `<prefix>/store/trust/current/bin`, resolved
//! physically. There is no PATH rung and no stock fallback. The lease is
//! `atpkg::lease::hold_for`; the admission is `atpkg::lineage::check_compiler_with` under
//! the policy [`super::lineage_switch`] reads.

use std::path::{Path, PathBuf};

use super::lineage_switch::Switch;
use super::tools::{self, Tools};
use crate::ctx::Ctx;
use crate::error::{Error, Result};
use crate::triple::Triple;

/// Where a toolchain was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Named by the verb (`--toolchain`): a source build, or the `rustc` group's seal.
    Explicit,
    /// The atpkg store's pinned build.
    Store,
}

/// A resolved toolchain: its physical bin directory. `store/trust/current` is a link
/// `aterm pkg install trust` repoints, so a pack holds the build-numbered directory behind
/// it, which cannot move under a running pack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toolchain {
    /// The physical `bin/`.
    pub bin: PathBuf,
    /// Where it came from.
    pub source: Source,
}

impl Toolchain {
    /// `bin/targo`.
    #[must_use]
    pub fn targo(&self) -> PathBuf {
        self.bin.join("targo")
    }

    /// `bin/trustc` — always the compiler (`RUSTC`), never PATH's `rustc`.
    #[must_use]
    pub fn trustc(&self) -> PathBuf {
        self.bin.join("trustc")
    }
}

fn executable(p: &Path) -> bool {
    atpkg::toolchain::is_executable_file(p)
}

/// Resolve the toolchain a pack builds with.
///
/// # Errors
///
/// A refusal when an explicit bin dir lacks `targo` or `trustc` (never set aside for the
/// store), or when there is no store toolchain — naming `aterm pkg install trust` first.
pub fn resolve(ctx: &Ctx, explicit: Option<&Path>) -> Result<Toolchain> {
    let (dir, source) = match explicit {
        Some(d) => (d.to_path_buf(), Source::Explicit),
        None => (
            atpkg::toolchain::store_stage2_bin(&ctx.pkg_prefix),
            Source::Store,
        ),
    };
    let fix = "fix:  aterm pkg install trust   (then: aterm pkg which targo)\n  \
               or:   --toolchain <a Trust bin dir>   (a source build, or a seal)";
    if !executable(&dir.join("targo")) {
        return Err(Error::refused(match source {
            Source::Explicit => format!(
                "--toolchain {} holds no executable targo — an explicit location is refused, \
                 not set aside for the store",
                ctx.shown(&dir)
            ),
            Source::Store => format!(
                "no Trust targo was found ({} holds none). Every program is built with \
                 Trust's targo; there is no stock-cargo fallback.\n  {fix}",
                ctx.shown(&dir)
            ),
        }));
    }
    if !executable(&dir.join("trustc")) {
        return Err(Error::refused(format!(
            "{} has no sibling trustc, so a build would compile with whatever `rustc` PATH \
             offers; RUSTC is always the targo's own trustc",
            ctx.shown(&dir.join("targo"))
        )));
    }
    let bin = std::fs::canonicalize(&dir)
        .map_err(|e| Error::io(format!("resolve {}", ctx.shown(&dir)), &e))?;
    Ok(Toolchain { bin, source })
}

/// The lease a run holds on its toolchain, released when dropped.
#[derive(Debug)]
pub enum Held {
    /// The lease on a store build.
    Lease(atpkg::lease::Lease),
    /// Nothing to hold: the directory is none of atpkg's, or the lease could not be
    /// taken (said once; the run is exactly as exposed as it would be without one).
    Nothing,
}

/// Lease the build `dir` belongs to (a toolchain's `bin/`, or a sysroot under the store)
/// while the returned [`Held`] lives: gc never reclaims a leased build and an unattended
/// trust update waits for it. Between a pack's steps no process runs from it, so only the
/// lease says it is in use.
///
/// # Errors
///
/// A refusal when the directory is gone once the lease is laid: the pack would run on a
/// toolchain that was reclaimed.
pub fn lease(ctx: &Ctx, dir: &Path, who: &str) -> Result<Held> {
    match atpkg::lease::hold_for(&ctx.pkg_prefix, dir, who, atpkg::lease::DEFAULT_WAIT) {
        Ok(atpkg::lease::Hold::Held(l)) => Ok(Held::Lease(l)),
        Ok(atpkg::lease::Hold::Unmanaged) => Ok(Held::Nothing),
        Ok(atpkg::lease::Hold::NotTaken(subject, why)) => {
            eprintln!(
                "atpkg-publish: no lease on {} ({why:?}) — an update landing mid-run can \
                 reclaim it",
                subject.describe()
            );
            Ok(Held::Nothing)
        }
        Err(missing) => Err(Error::refused(format!(
            "{} is gone (reclaimed after it was resolved, or never installed) — nothing runs \
             on a toolchain that is not there",
            ctx.shown(&missing.dir)
        ))),
    }
}

/// Admit the compiler in `bin` by content and lineage under `switch`'s policy, through
/// the client's own checker; its record is the pack's provenance.
///
/// # Errors
///
/// A refusal carrying every reason the checker gave — and, when the receipt was required
/// only because nothing could read the switch, why (the remedy may be a fetch, not
/// another compiler).
pub fn admit(tools: &dyn Tools, bin: &Path, switch: &Switch) -> Result<atpkg::lineage::Compiler> {
    let probe = |p: &Path| tools::version(tools, p);
    atpkg::lineage::check_compiler_with(bin, switch.policy(), &probe).map_err(|lines| {
        let unknown = match switch {
            Switch::Unknown(why) => {
                format!("\n  (the receipt is required because nothing shows the switch is dormant: {why})")
            }
            Switch::Dormant | Switch::Live => String::new(),
        };
        Error::refused(format!(
            "the compiler at {} is not an admitted Trust compiler — nothing was built or \
             signed:\n  {}{unknown}",
            bin.display(),
            lines.join("\n  ")
        ))
    })
}

/// The target a compiler names as its host (`-vV`'s `host:`).
///
/// # Errors
///
/// A refusal when it does not answer, names no host, or names one the client does not
/// ship on.
pub fn host_of(tools: &dyn Tools, compiler: &Path) -> Result<Triple> {
    let text = tools::version(tools, compiler).map_err(Error::refused)?;
    let host = text
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .ok_or_else(|| Error::refused(format!("`{} -vV` names no host", compiler.display())))?;
    Triple::parse(host.trim()).map_err(|e| Error::refused(e.to_string()))
}
