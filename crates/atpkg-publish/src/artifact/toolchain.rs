// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Resolving, leasing, and admitting the Trust toolchain a pack builds with.

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

/// A resolved toolchain: its physical bin directory.
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
/// # Errors
/// A refusal when an explicit bin dir lacks `targo` or `trustc` (never set aside for the store), or
/// when there is no store toolchain — naming `aterm pkg install trust` first.
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
    /// Nothing to hold: the directory is none of atpkg's, or the lease could not be taken.
    Nothing,
}

/// Leases the store build containing `dir` while the returned [`Held`] lives.
/// # Errors
/// A refusal when the directory is gone once the lease is laid.
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

/// [`lease`], failing closed.
/// # Errors
/// [`lease`]'s, and a refusal when the store build's lease could not be taken.
pub fn lease_required(ctx: &Ctx, dir: &Path, who: &str) -> Result<Held> {
    match atpkg::lease::hold_for(&ctx.pkg_prefix, dir, who, atpkg::lease::DEFAULT_WAIT) {
        Ok(atpkg::lease::Hold::Held(l)) => Ok(Held::Lease(l)),
        Ok(atpkg::lease::Hold::Unmanaged) => Ok(Held::Nothing),
        Ok(atpkg::lease::Hold::NotTaken(subject, why)) => Err(Error::refused(format!(
            "no lease on {} ({why:?}) — an update landing mid-run could reclaim it, so \
             nothing runs on it unleased",
            subject.describe()
        ))),
        Err(missing) => Err(Error::refused(format!(
            "{} is gone (reclaimed after it was resolved, or never installed) — nothing runs \
             on a toolchain that is not there",
            ctx.shown(&missing.dir)
        ))),
    }
}

/// Admits the compiler in `bin` by content and lineage under `switch`.
/// # Errors
/// A refusal carrying every reason the checker gave.
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
/// # Errors
/// A refusal when it does not answer, names no host, or names one the client does not ship on.
pub fn host_of(tools: &dyn Tools, compiler: &Path) -> Result<Triple> {
    let text = tools::version(tools, compiler).map_err(Error::refused)?;
    let host = text
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .ok_or_else(|| Error::refused(format!("`{} -vV` names no host", compiler.display())))?;
    Triple::parse(host.trim()).map_err(|e| Error::refused(e.to_string()))
}
