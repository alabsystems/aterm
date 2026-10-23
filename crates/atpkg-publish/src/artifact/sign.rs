// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg-publish sign`: manifest signing and verification under [`Authority`]
//! ([`sign_checked`], [`super::lineage_switch`]).

use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError};

use super::lineage_switch::{self, Switch};
use super::pkgdoc;
use crate::channel::Log;
use crate::channel::index::newest_roster;
use crate::ctx::Ctx;
use crate::error::{Error, Result};
use crate::host::HostError;
use crate::identity::MachineKey;
use crate::quad::{self, QUAD_FILES};

/// The roster generation a package signature is verified under before it counts.
pub struct Authority {
    roster: atpkg::TrustedRoster,
    /// Where the elected pair was found, as printed.
    pub source: String,
}

impl Authority {
    /// Its `roster_seq`.
    #[must_use]
    pub const fn seq(&self) -> u64 {
        self.roster.seq()
    }

    /// Verify and parse a package manifest under this generation's roster.
    ///
    /// # Errors
    /// A refusal when no live machine of this generation signed these bytes, or they do not parse.
    pub fn verify_pkg(&self, pkg: &[u8], sig: &[u8]) -> Result<atpkg::PkgManifest> {
        let (verified, _) = self
            .roster
            .authorize_bytes(pkg.to_vec(), sig)
            .map_err(|why| {
                Error::refused(format!(
                    "the client's verifier refuses the package manifest under roster_seq {} \
                 ({}): {why:?}",
                    self.seq(),
                    self.source
                ))
            })?;
        atpkg::parse_pkg(&verified).map_err(|why| {
            Error::refused(format!(
                "the package manifest verified but does not parse: {why:?}"
            ))
        })
    }
}

/// The channel head's roster pair, downloaded into `dir`.
fn head_roster(ctx: &Ctx, dir: &Path) -> std::result::Result<Option<PathBuf>, String> {
    let slug = ctx.index_slug();
    let n = match quad::head_build(ctx, &slug) {
        Ok(Some(n)) => n,
        Ok(None) | Err(HostError::NotFound) => return Ok(None),
        Err(e) => return Err(format!("listing {slug}: {e}")),
    };
    let tag = quad::index_tag(n);
    quad::download_quad_files(ctx, &slug, &tag, &QUAD_FILES[2..], dir)
        .map_err(|e| format!("{tag} on {slug}: {e}"))?;
    Ok(Some(dir.join(QUAD_FILES[2])))
}

/// The newest master-verified roster pair across the channel head and local copies.
///
/// # Errors
/// A refusal for an empty anchor, no pair that verifies, or a fork.
pub fn authority(ctx: &Ctx) -> Result<Authority> {
    if ctx.anchor.is_empty() {
        return Err(Error::refused(
            "pins::PAPER_MASTER_PUBKEYS is EMPTY — this build trusts no paper master, so \
             nothing it signs could be verified by any client built from it",
        ));
    }
    let scratch = aterm_tempfile::Builder::new()
        .prefix("atpkg-publish-roster-")
        .tempdir()
        .map_err(|e| Error::io("create a scratch dir for the channel head's roster", &e))?;
    let mut candidates: Vec<(String, PathBuf)> = Vec::new();
    match head_roster(ctx, scratch.path()) {
        Ok(Some(path)) => candidates.push((format!("the head of {}", ctx.index_slug()), path)),
        Ok(None) => eprintln!(
            "atpkg-publish: {} serves no index yet — the rosters on this machine decide who \
             may sign",
            ctx.index_slug()
        ),
        Err(why) => eprintln!(
            "atpkg-publish: the channel head's roster could not be read ({why}) — signatures \
             are checked under the newest roster on this machine; the index that pins them \
             checks them again under the roster it ships"
        ),
    }
    candidates.extend(local_candidates(ctx));
    elect(ctx, &candidates)
}

/// The roster pairs on this machine: the aterm checkout's `dist/` and the installed one.
fn local_candidates(ctx: &Ctx) -> Vec<(String, PathBuf)> {
    [
        ctx.repo_root.join("dist").join(QUAD_FILES[2]),
        ctx.identity.roster.clone(),
    ]
    .into_iter()
    .map(|path| (ctx.shown(&path), path))
    .collect()
}

/// Elect this context's generation ([`elected`]) from an already-verified channel head.
pub fn elect_from_head(ctx: &Ctx, head: &crate::quad::VerifiedQuad) {
    let mut slot = ctx.elected.lock().unwrap_or_else(PoisonError::into_inner);
    if slot.is_some() || ctx.anchor.is_empty() {
        return;
    }
    let mut candidates = vec![(
        format!("the head of {}", ctx.index_slug()),
        head.file(QUAD_FILES[2]),
    )];
    candidates.extend(local_candidates(ctx));
    if let Ok(authority) = elect(ctx, &candidates) {
        *slot = Some(Arc::new(authority));
    }
}

/// Elect [`authority`] once per context and share it across clones.
///
/// # Errors
/// [`authority`]'s.
pub fn elected(ctx: &Ctx) -> Result<Arc<Authority>> {
    let mut slot = ctx.elected.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(held) = slot.as_ref() {
        return Ok(Arc::clone(held));
    }
    let fresh = Arc::new(authority(ctx)?);
    *slot = Some(Arc::clone(&fresh));
    Ok(fresh)
}

/// The election is the index's ([`newest_roster`]).
fn elect(ctx: &Ctx, candidates: &[(String, PathBuf)]) -> Result<Authority> {
    let paths: Vec<PathBuf> = candidates.iter().map(|(_, p)| p.clone()).collect();
    let pair = newest_roster(ctx, &paths, &mut Log::echo("atpkg-publish"))?;
    let label = candidates
        .iter()
        .find(|(_, p)| *p == pair.path)
        .map_or_else(|| ctx.shown(&pair.path), |(l, _)| l.clone());
    let anchor = atpkg::Anchor::of(ctx.anchor.clone(), 0);
    let roster =
        atpkg::admit_roster(&anchor, pair.bytes, &pair.sig, ctx.now_unix).map_err(|why| {
            Error::refused(format!(
                "the client's verifier refuses the roster pair at {label}: {why:?}"
            ))
        })?;
    Ok(Authority {
        roster,
        source: label,
    })
}

/// `<doc>.sig`.
#[must_use]
pub fn sig_path(doc: &Path) -> PathBuf {
    let mut s = doc.as_os_str().to_owned();
    s.push(".sig");
    PathBuf::from(s)
}

/// Sign `bytes` with `key` and verify the pair under `authority` with the client's rule.
///
/// # Errors
/// The client's refusal (this machine is not on the roster, or is revoked).
pub fn sign_checked(
    key: &MachineKey,
    bytes: &[u8],
    authority: &Authority,
) -> Result<(Vec<u8>, atpkg::PkgManifest)> {
    let sig = key.sign(bytes)?;
    let parsed = authority.verify_pkg(bytes, &sig)?;
    Ok((sig, parsed))
}

/// Write `<pkg>.sig`.
///
/// # Errors
/// An I/O failure.
pub fn write_sig(pkg: &Path, sig: &[u8]) -> Result<PathBuf> {
    let out = sig_path(pkg);
    std::fs::write(&out, sig).map_err(|e| Error::io(format!("write {}", out.display()), &e))?;
    Ok(out)
}

/// Refuse `unproven` rows once the lineage receipt switch is live.
///
/// # Errors
/// A refusal naming the remedy.
pub fn unproven_gate(ctx: &Ctx, what: &str) -> Result<()> {
    let trust_repo = lineage_switch::default_trust_repo(ctx);
    let why = match lineage_switch::read(ctx, &trust_repo)? {
        Switch::Dormant => return Ok(()),
        Switch::Live => format!(
            "{}'s origin main carries a minted re-pin, so the receipt is required. Pack \
             again with a receipt-carrying compiler",
            ctx.shown(&trust_repo)
        ),
        Switch::Unknown(why) => why,
    };
    Err(Error::refused(format!(
        "{what} records lineage=unproven (a compiler with no LINEAGE or SEED-MINT receipt, \
         admitted only while the receipt switch is dormant), and the switch does not read \
         dormant now: {why}. Nothing was signed or staged"
    )))
}

/// Whether any row of a parsed manifest records `lineage = "unproven"`.
#[must_use]
pub fn has_unproven_row(parsed: &atpkg::PkgManifest) -> bool {
    parsed.artifacts.iter().any(|a| {
        a.provenance
            .as_ref()
            .is_some_and(|p| p.lineage == "unproven")
    })
}

/// Re-check manifest provenance ([`pkgdoc::check_provenance`]) and version cleanliness.
///
/// # Errors
/// A refusal naming the row or the version.
pub fn check_signable(parsed: &atpkg::PkgManifest) -> Result<()> {
    if parsed.version.ends_with("-dirty") {
        return Err(Error::refused(format!(
            "{}@{} is version {} — a dirty checkout's pack is never published, so it is never \
             signed",
            parsed.program, parsed.build_number, parsed.version
        )));
    }
    for a in &parsed.artifacts {
        let p = a.provenance.as_ref().ok_or_else(|| {
            Error::refused(format!(
                "the {} row carries no [artifact.provenance] — nothing says which compiler \
                 built it",
                a.target
            ))
        })?;
        pkgdoc::check_provenance(p)
            .map_err(|e| Error::refused(format!("the {} row: {e}", a.target)))?;
    }
    Ok(())
}

/// Sign every manifest named — the ceremony ([`authority`]).
///
/// # Errors
/// Returns the first manifest refusal, leaving that manifest's directory unchanged.
pub fn sign_all(ctx: &Ctx, pkgs: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let key = MachineKey::load(ctx, &ctx.identity.key)?;
    let authority = authority(ctx)?;
    let mut sigs = Vec::new();
    for pkg in pkgs {
        let shown = ctx.shown(pkg);
        let bytes = std::fs::read(pkg).map_err(|e| Error::io(format!("read {shown}"), &e))?;
        let (sig, parsed) = sign_checked(&key, &bytes, &authority)
            .map_err(|e| Error::refused(format!("{shown}: {e}")))?;
        check_signable(&parsed).map_err(|e| Error::refused(format!("{shown}: {e}")))?;
        if has_unproven_row(&parsed) {
            unproven_gate(ctx, &shown)?;
        }
        let out = write_sig(pkg, &sig)?;
        println!(
            "atpkg-publish: signed {shown} ({}@{}, {} row(s)) — verified under roster_seq {} \
             ({})",
            parsed.program,
            parsed.build_number,
            parsed.artifacts.len(),
            authority.seq(),
            authority.source
        );
        sigs.push(out);
    }
    Ok(sigs)
}

/// `atpkg-publish sign <pkg.toml>… [--trust-repo DIR]`.
///
/// # Errors
/// A usage error for no manifest or an unknown flag; [`sign_all`]'s refusals.
pub fn run(ctx: &Ctx, args: &[String]) -> Result<()> {
    let mut ctx = ctx.clone();
    let mut pkgs = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--trust-repo" => {
                ctx.trust_repo = it
                    .next()
                    .map(PathBuf::from)
                    .ok_or_else(|| Error::usage("--trust-repo needs a directory"))?;
            }
            s if s.starts_with('-') => {
                return Err(Error::usage(format!("sign: unknown flag `{s}`")));
            }
            s => pkgs.push(PathBuf::from(s)),
        }
    }
    if pkgs.is_empty() {
        return Err(Error::usage(
            "sign <pkg.toml>… [--trust-repo DIR] — name the staged manifests to sign",
        ));
    }
    sign_all(&ctx, &pkgs).map(|_| ())
}
