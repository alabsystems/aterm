// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg-publish publish-group rustc --seal <dir> [--dry-run | --upload]`: the trust
//! bundle and its siblings (trust-ir, trust-cg, trust-vc) from one sealed toolchain, as one
//! act: pack, sign, upload, index.
//!
//! [`preflight`] collects every refusal before anything runs:
//!
//! * the seal: bin/trustc, bin/targo and lib/, one commit and a host from `-Vv`; not inside
//!   the atpkg store, not a live `build/*/stage2`; every [`crate::programs::TRUST_EXPOSES`]
//!   tool present; room beside the output;
//! * its SEED-MINT receipt (`atpkg::lineage::seed_mint_refusals`, unless the switch is
//!   dormant) and, to publish, the re-pin on the trust repo's origin main
//!   ([`super::seed`]) and both owners' tokens;
//! * the trust build and every sibling's (`rev-list --count`, skipping tags published on
//!   either owner when the compiler moved) against the floor, the verified head's pin for
//!   that target (`pin` on [`crate::programs::PIN_TRIPLE`], else that target's overlay):
//!   below is always refused, equal only with `--force-same-build` and never when the
//!   compiler moved. Staging or publishing without a readable floor is refused.
//!
//! Then it packs the bundle ([`crate::api::BundlePacker`]); re-checks each sibling (same
//! commit, still clean, a build no lower than judged, still above the floor) before any
//! packs and again before its own ([`crate::api::Packer`] with the seal's toolchain); and
//! publishes one index ([`crate::api::Indexer`]) whose delta is the four builds (`pin` on
//! the pin triple, a `pin_by_target` overlay elsewhere). Every other pin is carried from
//! the verified head.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use atpkg::Channel;

use super::{Log, Scratch, flag_value, seed};
use crate::api::{
    Baseline, BundlePacker, BundleRequest, Delta, HelloGate, IndexRequest, Indexer, PackRequest,
    Packer, Signing,
};
use crate::artifact::lineage_switch::{self, Switch};
use crate::artifact::upload;
use crate::ctx::Ctx;
use crate::error::{Error, Result};
use crate::host::HostError;
use crate::programs::{self, PIN_TRIPLE, RUSTC_GROUP, TRUST_EXPOSES};
use crate::quad::{self, Freshness};
use crate::triple::Triple;

/// The bundle the group is named for.
pub const BUNDLE: &str = "trust";

/// How the publish runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Preflight and the plan; packs nothing, writes nothing to any host.
    DryRun,
    /// Pack and sign locally (unsigned on a keyless box), index locally; nothing
    /// uploaded.
    Stage,
    /// Every step with its uploads.
    Upload,
}

/// One group publish.
#[derive(Clone, Debug)]
pub struct GroupRequest {
    /// The sealed toolchain (bin/ + lib/), or a staged copy of one carrying the trust-wp
    /// drivers.
    pub seal: PathBuf,
    /// The trust checkout the seal's commit is counted in.
    pub trust_repo: PathBuf,
    /// A sibling's checkout when it is not `~/<name>`.
    pub checkouts: BTreeMap<String, PathBuf>,
    /// How it runs.
    pub mode: Mode,
    /// Repack a build equal to the floor's pin (never a lower one).
    pub force_same_build: bool,
    /// Pack from a live `build/*/stage2` on purpose.
    pub allow_build_tree_seal: bool,
    /// Where the packs and the index go; `None` is
    /// `<state>/rustc-group/trust-<commit>`.
    pub out: Option<PathBuf>,
}

/// What the preflight reads off the seal that only running it can tell, and the receipt
/// switch — behind a trait so the group's tests answer for them.
pub trait SealProbe {
    /// `<seal>/bin/trustc -Vv`'s stdout.
    ///
    /// # Errors
    ///
    /// Why it did not answer.
    fn trustc_vv(&self, seal: &Path) -> std::result::Result<String, String>;

    /// The lineage receipt switch, read from `trust_repo`'s live origin main.
    fn lineage_switch(&self, ctx: &Ctx, trust_repo: &Path) -> Switch;
}

/// The seal run for real, and the switch the packers read.
#[derive(Clone, Copy, Debug, Default)]
pub struct RealProbe;

impl SealProbe for RealProbe {
    fn trustc_vv(&self, seal: &Path) -> std::result::Result<String, String> {
        let mut cmd = aterm_uds::spawnfd::command(seal.join("bin/trustc"));
        cmd.arg("-Vv");
        match aterm_uds::deadline::output_until(&mut cmd, Instant::now() + Duration::from_secs(60))
        {
            Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
            Ok(o) => Err(format!(
                "exit {:?}: {}",
                o.status.code(),
                String::from_utf8_lossy(&o.stderr).trim()
            )),
            Err(e) => Err(format!("{e:?}")),
        }
    }

    fn lineage_switch(&self, ctx: &Ctx, trust_repo: &Path) -> Switch {
        lineage_switch::read(ctx, trust_repo).unwrap_or_else(|e| Switch::Unknown(e.to_string()))
    }
}

/// Everything a group publish calls out to.
pub struct Parts<'a> {
    /// The seal and the switch.
    pub probe: &'a dyn SealProbe,
    /// Packs the bundle.
    pub bundle: &'a dyn BundlePacker,
    /// Packs the siblings.
    pub packer: &'a dyn Packer,
    /// Publishes the index.
    pub indexer: &'a dyn Indexer,
}

/// The verified head a build is judged against.
#[derive(Clone, Debug)]
pub struct Floor {
    /// Its number.
    pub index_build: u64,
    /// Its channel.
    pub channel: Channel,
}

/// One sibling, as the preflight judged it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Judged {
    /// Its checkout.
    pub dir: PathBuf,
    /// The commit judged.
    pub head: String,
    /// The build judged (what step 2 packs).
    pub build: u64,
}

/// What the preflight found.
#[derive(Clone, Debug, Default)]
pub struct Preflight {
    /// Every refusal; any one stops the publish.
    pub problems: Vec<String>,
    /// What a person should read.
    pub notes: Vec<String>,
    /// The judgment lines, one per sibling.
    pub lines: Vec<String>,
    /// The seal's trustc commit.
    pub seal_commit: String,
    /// Its release string.
    pub seal_version: String,
    /// Its host — the target the group is published for.
    pub triple: Option<Triple>,
    /// `rev-list --count <commit>` in the trust repo.
    pub trust_build: Option<u64>,
    /// The verified head.
    pub floor: Option<Floor>,
    /// The trust build the channel's `pin` carries.
    pub public_trust_pin: Option<u64>,
    /// The siblings, judged.
    pub judged: BTreeMap<String, Judged>,
    /// A machine key is here to sign with.
    pub have_key: bool,
    /// The SEED-MINT receipt, described.
    pub seed_line: Option<String>,
}

impl Preflight {
    /// Whether this cut carries a newer trust build than the channel pins — and so each
    /// sibling takes the first build number not yet published.
    #[must_use]
    pub fn compiler_moved(&self) -> bool {
        matches!((self.trust_build, self.public_trust_pin), (Some(b), Some(p)) if b > p)
    }

    /// The pin the index step compares `program`'s new build with: `pin` when the seal's
    /// target owns it, else that target's overlay cell.
    #[must_use]
    pub fn floor_pin(&self, program: &str) -> Option<u64> {
        let floor = self.floor.as_ref()?;
        let triple = self.triple?;
        if triple == PIN_TRIPLE {
            floor.channel.pin.get(program).copied()
        } else {
            floor
                .channel
                .pin_by_target
                .get(triple.as_str())
                .and_then(|c| c.get(program))
                .copied()
        }
    }

    fn label(&self) -> String {
        self.floor.as_ref().map_or_else(
            || "the channel head".to_string(),
            |f| format!("the channel head atpkg-index-{}", f.index_build),
        )
    }

    fn scope(&self) -> String {
        match self.triple {
            Some(t) if t != PIN_TRIPLE => format!(" for {t}"),
            _ => String::new(),
        }
    }

    /// Why the index step would refuse `program` packed at `build`, or `None`: below the
    /// floor's pin always; equal unless forced, and never when the compiler moved (then
    /// an equal number means the published-tag probe answered wrong, and packing would
    /// put new bytes under a published number).
    #[must_use]
    pub fn floor_refusal(&self, program: &str, build: u64, force: bool) -> Option<String> {
        let p = self.floor_pin(program)?;
        let (label, scope) = (self.label(), self.scope());
        if build < p {
            Some(format!(
                "{program}: this cut would pack build {build}, BELOW the {p} that {label} pins\
                 {scope} — the index step refuses that downgrade only after every pack has run, \
                 and --force-same-build does not lift it. Update the checkout, or point \
                 --checkout {program}=<dir> at one at build {p} or newer"
            ))
        } else if build == p && self.compiler_moved() {
            Some(format!(
                "{program}: this cut would pack build {build}, the SAME build that {label} pins\
                 {scope}, from a NEW compiler (trust {} over the published {}) — the published \
                 tag probe answered wrong, and packing would put this compiler's bytes under a \
                 published build number; --force-same-build does not lift that",
                self.trust_build.unwrap_or(0),
                self.public_trust_pin.unwrap_or(0)
            ))
        } else if build == p && !force {
            Some(format!(
                "{program}: this cut would pack build {build}, the SAME build that {label} pins\
                 {scope} — nothing new to publish for it (--force-same-build repacks an equal \
                 build, which the index step carries; it never admits a lower one)"
            ))
        } else {
            None
        }
    }
}

/// The siblings: the group's members other than the bundle.
#[must_use]
pub fn siblings() -> Vec<&'static str> {
    programs::group_members(RUSTC_GROUP)
        .map(|p| p.name)
        .filter(|n| *n != BUNDLE)
        .collect()
}

fn checkout_of(ctx: &Ctx, req: &GroupRequest, program: &str) -> PathBuf {
    req.checkouts
        .get(program)
        .cloned()
        .unwrap_or_else(|| ctx.home.join(program))
}

/// The value of `<key>: ` in `trustc -Vv`'s output.
fn vv_field(vv: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}: ");
    vv.lines()
        .find_map(|l| l.strip_prefix(&prefix))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Bytes under `dir`, each inode once (hard links are one file).
fn tree_bytes(dir: &Path) -> u64 {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt as _;
    let mut seen = BTreeSet::new();
    let mut total = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let Ok(meta) = std::fs::symlink_metadata(e.path()) else {
                continue;
            };
            if meta.is_dir() {
                stack.push(e.path());
            } else if meta.is_file() {
                #[cfg(unix)]
                let first = seen.insert((meta.dev(), meta.ino()));
                #[cfg(not(unix))]
                let first = seen.insert(e.path());
                if first {
                    total += meta.len();
                }
            }
        }
    }
    total
}

/// The build step 2 packs `program` under, one function so the formula judged is the
/// formula packed: the checkout's revision count, and when the compiler moved, the first
/// number at or above it with no `atpkg-<program>-<n>` release on either owner.
///
/// The staging owner's repos are private, and GitHub answers 404 for a private repo to an
/// identity that cannot see it, so its tags are read from its release list, and a repo
/// that does not list is a refusal, never "no tag taken".
///
/// # Errors
///
/// When git gives no count, an owner cannot be asked, or the staging repo is not visible.
pub fn pack_build(ctx: &Ctx, program: &str, dir: &Path, compiler_moved: bool) -> Result<u64> {
    let mut b = crate::git::rev_count(dir, "HEAD")?;
    if !compiler_moved {
        return Ok(b);
    }
    let repo = programs::get(program).map_or(program, |p| p.repo);
    let staging = ctx.staging_slug(repo);
    let staged: BTreeSet<String> = match ctx.hosts.staging.releases(&staging) {
        Ok(releases) => releases.into_iter().map(|r| r.tag).collect(),
        Err(HostError::NotFound) => {
            return Err(Error::refused(format!(
                "{staging} does not list for this identity — a private repo answers 404 to a \
                 token that cannot see it, so a build number published there would read as \
                 free. The staging owner's token ({}) must see it",
                ctx.shown(&ctx.tokens.staging)
            )));
        }
        Err(e) => {
            return Err(Error::cannot_run(format!(
                "could not list the releases of {staging} ({e}) — the build this cut packs \
                 {program} under depends on them"
            )));
        }
    };
    let public = ctx.public_slug(repo);
    loop {
        let tag = upload::tag(program, b);
        let taken = staged.contains(&tag)
            || match ctx.hosts.channel.release(&public, &tag) {
                Ok(_) => true,
                Err(HostError::NotFound) => false,
                Err(e) => {
                    return Err(Error::cannot_run(format!(
                        "could not ask whether {tag} is published on {public} ({e}) — the build \
                         this cut packs {program} under depends on it"
                    )));
                }
            };
        if !taken {
            return Ok(b);
        }
        b += 1;
    }
}

/// Collect every refusal, and what the publish would do.
#[must_use]
pub fn preflight(ctx: &Ctx, req: &GroupRequest, parts: &Parts<'_>) -> Preflight {
    let mut pf = Preflight {
        have_key: ctx.identity.key.is_file(),
        ..Preflight::default()
    };
    let seal = &req.seal;
    let upload = req.mode == Mode::Upload;

    // The seal.
    let exec = |p: &Path| {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt as _;
        #[cfg(unix)]
        return std::fs::metadata(p)
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
        #[cfg(not(unix))]
        return p.is_file();
    };
    if !exec(&seal.join("bin/trustc")) {
        pf.problems.push(format!(
            "the seal has no executable bin/trustc: {}",
            ctx.shown(seal)
        ));
    }
    if !exec(&seal.join("bin/targo")) {
        pf.problems.push(format!(
            "the seal has no executable bin/targo: {}",
            ctx.shown(seal)
        ));
    }
    if !seal.join("lib").is_dir() {
        pf.problems.push(format!(
            "the seal has no lib/ (not a sysroot): {}",
            ctx.shown(seal)
        ));
    }
    if seal.join("bin/trustc").is_file() {
        match parts.probe.trustc_vv(seal) {
            Ok(vv) => {
                pf.seal_commit = vv_field(&vv, "commit-hash").unwrap_or_default();
                pf.seal_version = vv_field(&vv, "release").unwrap_or_default();
                if pf.seal_commit.is_empty() {
                    pf.problems
                        .push("`trustc -Vv` reports no commit-hash line".to_string());
                }
                match vv_field(&vv, "host") {
                    None => pf.problems.push(
                        "`trustc -Vv` reports no host line — the seal's target decides which \
                         pin every program is judged against and what the index step writes"
                            .to_string(),
                    ),
                    Some(h) => match Triple::parse(&h) {
                        Ok(t) => pf.triple = Some(t),
                        Err(_) => pf.problems.push(format!(
                            "the seal's host `{h}` is not a target the client ships on"
                        )),
                    },
                }
            }
            Err(why) => pf
                .problems
                .push(format!("`trustc -Vv` did not answer: {why}")),
        }
    }
    let real = seal.canonicalize().unwrap_or_else(|_| seal.clone());
    if let Ok(store) = ctx.pkg_prefix.join("store").canonicalize()
        && real.starts_with(&store)
    {
        pf.problems.push(format!(
            "the seal resolves INSIDE the atpkg store ({}): that IS a published build — \
             nothing newer to publish from this machine. Point --seal at a sealed toolchain \
             (~/toolchains/trust-<rev>)",
            ctx.shown(&real)
        ));
    }
    let comps: Vec<String> = real
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    // `*/build/*/stage2`: a `stage2` with a `build` directory anywhere above its parent.
    let live = comps.len() >= 3
        && comps[comps.len() - 1] == "stage2"
        && comps[..comps.len() - 2].iter().any(|c| c == "build");
    if live {
        if req.allow_build_tree_seal {
            pf.notes.push(format!(
                "--allow-build-tree-seal: packing from the live build tree {} — nothing may run \
                 x.py there until the bundle is packed",
                ctx.shown(&real)
            ));
        } else {
            pf.problems.push(format!(
                "the seal is a live build tree ({}): x.py empties stage2/bin for the duration of \
                 every run, and this publish reads it for minutes. Seal it first \
                 (scripts/promote-toolchain.sh in the trust repo) and point --seal at the \
                 sealed ~/toolchains/trust-<rev> (--allow-build-tree-seal overrides, for a tree \
                 nothing is building)",
                ctx.shown(&real)
            ));
        }
    }
    if seal.join("bin").is_dir() {
        let missing: Vec<&str> = TRUST_EXPOSES
            .iter()
            .copied()
            .filter(|t| !seal.join("bin").join(t).is_file())
            .collect();
        if !missing.is_empty() {
            pf.problems.push(format!(
                "the seal's bin/ is missing {} exposed tool(s) — {}. Build them against THIS \
                 seal, copy the seal to a staging dir (a sealed artifact is immutable), drop \
                 the new bins in its bin/ and point --seal at the staging dir. Never drop a \
                 name from TRUST_EXPOSES: the client prunes the shim of a name an update stops \
                 exposing",
                missing.len(),
                missing.join(" ")
            ));
        }
    }

    // The trust repo, for the build number that names the seal's commit.
    match crate::git::run(&req.trust_repo, &["rev-parse", "--is-inside-work-tree"]) {
        Ok(_) if !pf.seal_commit.is_empty() => {
            let object = format!("{}^{{commit}}", pf.seal_commit);
            if crate::git::run(&req.trust_repo, &["cat-file", "-e", &object]).is_ok() {
                match crate::git::rev_count(&req.trust_repo, &pf.seal_commit) {
                    Ok(b) => pf.trust_build = Some(b),
                    Err(e) => pf.problems.push(e.to_string()),
                }
            } else {
                pf.problems.push(format!(
                    "the seal's commit {} is not in {} (fetch it: the build number is \
                     `git rev-list --count <commit>` there)",
                    pf.seal_commit,
                    ctx.shown(&req.trust_repo)
                ));
            }
        }
        Ok(_) => {}
        Err(_) => pf.problems.push(format!(
            "the trust repo is not a checkout: {} (--trust-repo)",
            ctx.shown(&req.trust_repo)
        )),
    }

    // The seed pin.
    let receipt_path = seal.join(atpkg::lineage::SEED_MINT_FILE);
    let receipt = std::fs::read_to_string(&receipt_path).ok();
    if !pf.seal_commit.is_empty() {
        let dormant = receipt.is_none()
            && parts.probe.lineage_switch(ctx, &req.trust_repo) == Switch::Dormant;
        if dormant {
            pf.notes.push(format!(
                "the seed pin: NOT ENFORCED — {}'s origin main carries no minted re-pin yet; \
                 once the first minted re-pin lands there, a seal without a SEED-MINT receipt \
                 is refused",
                ctx.shown(&req.trust_repo)
            ));
        } else {
            pf.problems
                .extend(atpkg::lineage::seed_mint_refusals(seal, &pf.seal_commit));
            if let Some(text) = &receipt {
                pf.seed_line = Some(seed::describe(text));
                if upload {
                    pf.problems
                        .extend(seed::publish_refusals(ctx, &req.trust_repo, seal));
                } else {
                    pf.notes.push(format!(
                        "the seed pin: publishing (--upload) waits until {}'s origin main \
                         contains the re-pin {}, pins {} through it, and its archive release {} \
                         exists",
                        ctx.shown(&req.trust_repo),
                        seed::field(text, "repin_commit")
                            .unwrap_or_default()
                            .chars()
                            .take(10)
                            .collect::<String>(),
                        pf.seal_commit,
                        seed::field(text, "repin_release_tag").unwrap_or_default()
                    ));
                }
            }
        }
    }

    // The floor: the verified channel head, its horizon ignored (a lapsed head still pins
    // what it pins). Packing without one judges nothing and skips no published tag.
    let floor = Scratch::new("floor").and_then(|scratch| {
        quad::fetch_head_with(ctx, &ctx.index_slug(), scratch.path(), Freshness::Ignore)
    });
    match floor {
        Ok((n, q)) => {
            // The packs this group drives sign under the generation this head elects.
            crate::artifact::sign::elect_from_head(ctx, &q);
            if let Some(c) = q.index.channels.first() {
                pf.public_trust_pin = c.pin.get(BUNDLE).copied();
                pf.floor = Some(Floor {
                    index_build: n,
                    channel: c.clone(),
                });
            }
        }
        Err(e) if req.mode == Mode::DryRun => pf.notes.push(format!(
            "no floor index to judge the builds against ({e}): they are NOT compared with the \
             public pins here, and a moved compiler skips no published build number — staging \
             or publishing refuses until the channel head reads"
        )),
        Err(e) => pf.problems.push(format!(
            "no floor index to judge the builds against ({e}) — without the channel head's \
             pins no build is compared with what is published and no published build number \
             is skipped, so the packs would run for nothing (the index step re-reads the head \
             and refuses after them)"
        )),
    }

    // The trust build against the floor.
    if let (Some(b), Some(p)) = (pf.trust_build, pf.floor_pin(BUNDLE)) {
        let (label, scope) = (pf.label(), pf.scope());
        if b < p {
            pf.problems.push(format!(
                "the seal's trust build {b} is BELOW the {p} that {label} pins{scope} — the \
                 index step refuses that downgrade, but only after every pack has run, and \
                 --force-same-build does not lift it. Point --seal at a seal of trust build {p} \
                 or newer"
            ));
        } else if b == p && !req.force_same_build {
            pf.problems.push(format!(
                "the seal's trust build {b} is the SAME build that {label} pins{scope} — \
                 nothing to publish (--force-same-build repacks an equal build, which the index \
                 step carries; it never admits a lower one)"
            ));
        }
    }

    // The siblings: present, clean, and packing a build the floor does not already cover.
    let moved = pf.compiler_moved();
    for program in siblings() {
        let dir = checkout_of(ctx, req, program);
        if crate::git::run(&dir, &["rev-parse", "--is-inside-work-tree"]).is_err() {
            pf.problems.push(format!(
                "{program}: no checkout at {} (--checkout {program}=<dir>)",
                ctx.shown(&dir)
            ));
            continue;
        }
        if !crate::git::is_clean(&dir).unwrap_or(false) {
            pf.problems.push(format!(
                "{program}: the checkout at {} is dirty — a dirty tree cannot carry its \
                 commit's build number",
                ctx.shown(&dir)
            ));
        }
        let build = match pack_build(ctx, program, &dir, moved) {
            Ok(b) => b,
            Err(e) => {
                pf.problems.push(format!("{program}: {e}"));
                continue;
            }
        };
        pf.judged.insert(
            program.to_string(),
            Judged {
                dir: dir.clone(),
                head: crate::git::head(&dir).unwrap_or_default(),
                build,
            },
        );
        if pf.floor.is_none() || pf.triple.is_none() {
            continue;
        }
        let (label, scope) = (pf.label(), pf.scope());
        match pf.floor_pin(program) {
            None => pf.lines.push(format!(
                "sibling {program}: this cut packs build {build} ({label} pins no {program}\
                 {scope} — the index step adds it as new)"
            )),
            Some(p) => {
                pf.lines.push(format!(
                    "sibling {program}: this cut packs build {build} ({label} pins {p}{scope})"
                ));
                match pf.floor_refusal(program, build, req.force_same_build) {
                    Some(r) => pf.problems.push(r),
                    None if build == p => pf.notes.push(format!(
                        "--force-same-build: {program} repacks build {build}, the same build \
                         that {label} pins{scope} (the index step carries it)"
                    )),
                    None => {}
                }
            }
        }
    }

    // Identity and tokens, by mode.
    if upload {
        if !pf.have_key {
            pf.problems.push(format!(
                "--upload needs a rostered machine key at {} (this box has none: stage here, \
                 sign and upload on a rostered machine)",
                ctx.shown(&ctx.identity.key)
            ));
        }
        if !std::fs::metadata(&ctx.tokens.channel).is_ok_and(|m| m.len() > 0) {
            pf.problems.push(format!(
                "--upload needs the public owner's token at {} (the packs and the index are \
                 published there)",
                ctx.shown(&ctx.tokens.channel)
            ));
        }
        if !std::fs::metadata(&ctx.tokens.staging).is_ok_and(|m| m.len() > 0) {
            pf.problems.push(format!(
                "--upload needs the staging owner's token at {} (every pack is uploaded to {} \
                 first, and gh's own login never writes)",
                ctx.shown(&ctx.tokens.staging),
                ctx.accounts.staging
            ));
        }
    } else if !pf.have_key {
        pf.notes.push(format!(
            "no machine key at {}: the packs are STAGED UNSIGNED and no index is signed; a \
             rostered machine signs and uploads",
            ctx.shown(&ctx.identity.key)
        ));
    }

    // Room: one staged copy of the sysroot plus its tarball, beside the output.
    let out = out_dir(ctx, req, &pf);
    let mut probe_at = out.as_path();
    while !probe_at.exists() {
        match probe_at.parent() {
            Some(p) => probe_at = p,
            None => break,
        }
    }
    if seal.is_dir()
        && let Some(free) = atpkg::freespace::available_bytes(probe_at)
    {
        let need = tree_bytes(seal).saturating_mul(2);
        if free < need {
            pf.problems.push(format!(
                "{} has {} GiB free; the pack needs about {} GiB there (one staged copy of the \
                 sysroot plus its tarball). Free space, or point --out at a volume that has it",
                ctx.shown(probe_at),
                free >> 30,
                need >> 30
            ));
        }
    }
    pf
}

fn out_dir(ctx: &Ctx, req: &GroupRequest, pf: &Preflight) -> PathBuf {
    req.out.clone().unwrap_or_else(|| {
        ctx.state_dir.join("rustc-group").join(format!(
            "trust-{}",
            pf.seal_commit.chars().take(10).collect::<String>()
        ))
    })
}

/// What a group publish did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupOutcome {
    /// The builds packed (empty for a dry run).
    pub builds: BTreeMap<String, u64>,
    /// The index number signed, when one was.
    pub index_build: Option<u64>,
    /// Where it all went.
    pub out: PathBuf,
}

/// The preflight's judgment of `program`, taken again now (peer sessions reset and switch
/// shared checkouts while the bundle packs): the checkout is at the judged commit and
/// clean, the build step 2 would pack is no lower than judged (a lower one can be a
/// published number), and it still passes the floor.
///
/// # Errors
///
/// A refusal naming what moved; nothing of `program` is packed after one.
pub fn recheck(ctx: &Ctx, pf: &Preflight, program: &str, force: bool) -> Result<u64> {
    let judged = pf
        .judged
        .get(program)
        .ok_or_else(|| Error::refused(format!("{program} was never judged by the preflight")))?;
    let dir = &judged.dir;
    let now = crate::git::head(dir).unwrap_or_default();
    if now != judged.head {
        return Err(Error::refused(format!(
            "{program}: {} moved since the preflight (HEAD {} -> {}), so the build the \
             preflight judged is not the build step 2 would pack. Nothing of {program} is \
             packed; put the checkout back (or point --checkout at one nothing else moves) and \
             re-run",
            ctx.shown(dir),
            judged.head,
            if now.is_empty() { "<none>" } else { &now }
        )));
    }
    if !crate::git::is_clean(dir).unwrap_or(false) {
        return Err(Error::refused(format!(
            "{program}: {} became dirty since the preflight — a dirty tree cannot carry its \
             commit's build number. Nothing of {program} is packed; clean it and re-run",
            ctx.shown(dir)
        )));
    }
    let build = pack_build(ctx, program, dir, pf.compiler_moved())?;
    if build < judged.build {
        return Err(Error::refused(format!(
            "{program}: step 2 would now pack build {build}, BELOW the {} the preflight judged \
             at the same commit — the published-tag probe answered differently, and a lower \
             number can be a published one. Nothing of {program} is packed; re-run",
            judged.build
        )));
    }
    if let Some(r) = pf.floor_refusal(program, build, force) {
        return Err(Error::refused(r));
    }
    Ok(build)
}

/// The group publish, its transcript in `log`.
///
/// # Errors
///
/// Every preflight problem, at once; a re-check that no longer holds; whatever a pack or
/// the index refuses.
pub fn publish_group(
    ctx: &Ctx,
    req: &GroupRequest,
    parts: &Parts<'_>,
    log: &mut Log,
) -> Result<GroupOutcome> {
    // One trust repo for the whole publish: the packers read the receipt switch from
    // `ctx.trust_repo`, so a `--trust-repo` that reached only the preflight would judge the
    // seal under one switch and pack it under another.
    let ctx = &Ctx {
        trust_repo: req.trust_repo.clone(),
        ..ctx.clone()
    };
    let pf = preflight(ctx, req, parts);
    let out = out_dir(ctx, req, &pf);
    log.say(format!(
        "mode: {}",
        match req.mode {
            Mode::DryRun => "dry-run",
            Mode::Stage => "stage",
            Mode::Upload => "publish",
        }
    ));
    log.say(format!(
        "seal: {} (trustc {}, {}){}",
        ctx.shown(&req.seal),
        pf.seal_commit,
        pf.seal_version,
        pf.trust_build
            .map(|b| format!(" → trust build {b}"))
            .unwrap_or_default()
    ));
    log.say(format!(
        "channel floor: {} (pins trust {})",
        pf.floor.as_ref().map_or_else(
            || "?".to_string(),
            |f| format!("atpkg-index-{}", f.index_build)
        ),
        pf.public_trust_pin
            .map_or_else(|| "?".to_string(), |p| p.to_string())
    ));
    if let Some(line) = &pf.seed_line {
        log.say(format!("seed: {line}"));
    }
    for line in &pf.lines {
        log.say(line.clone());
    }
    for note in &pf.notes {
        log.note(note.clone());
    }
    if !pf.problems.is_empty() {
        return Err(Error::refused(format!(
            "publish-group rustc: {} problem(s), nothing ran:\n  - {}",
            pf.problems.len(),
            pf.problems.join("\n  - ")
        )));
    }
    let triple = pf
        .triple
        .ok_or_else(|| Error::refused("the seal names no target"))?;
    let trust_build = pf
        .trust_build
        .ok_or_else(|| Error::refused("the seal's trust build is unknown"))?;
    let upload = req.mode == Mode::Upload;
    let signing = if pf.have_key {
        Signing::MachineKey
    } else {
        Signing::Unsigned
    };
    let next = pf
        .floor
        .as_ref()
        .map_or_else(|| "?".to_string(), |f| (f.index_build + 1).to_string());
    let flags = format!(
        "{}{}",
        if signing == Signing::Unsigned {
            " --unsigned"
        } else {
            ""
        },
        if upload { " --upload" } else { "" }
    );
    log.say("plan:");
    log.say(format!(
        "  1. pack-bundle {BUNDLE} --sysroot {} --build {trust_build} --version {:?} --triple \
         {triple} --src-repo {} --out {}{flags}",
        ctx.shown(&req.seal),
        pf.seal_version,
        ctx.shown(&req.trust_repo),
        ctx.shown(&out.join(BUNDLE))
    ));
    for (program, j) in &pf.judged {
        log.say(format!(
            "  2. pack {program} --checkout {} --toolchain {} --build {} --triple {triple} \
             --out {}{flags}",
            ctx.shown(&j.dir),
            ctx.shown(&req.seal.join("bin")),
            j.build,
            ctx.shown(&out.join(program))
        ));
    }
    log.say(format!(
        "  3. index {next} over the verified channel head: the four builds as {}{}",
        if triple == PIN_TRIPLE {
            "`pin` moves".to_string()
        } else {
            format!("a {triple} overlay (`pin` belongs to {PIN_TRIPLE})")
        },
        if upload {
            "; the packs are made public first"
        } else {
            ""
        }
    ));
    if req.mode == Mode::DryRun {
        log.say("dry run — nothing executed");
        return Ok(GroupOutcome {
            builds: BTreeMap::new(),
            index_build: None,
            out,
        });
    }

    std::fs::create_dir_all(&out).map_err(|e| Error::io(format!("mkdir {}", out.display()), &e))?;
    let mut builds = BTreeMap::new();
    log.say("1/3 packing the trust bundle from the seal");
    let bundle = parts.bundle.pack_bundle(
        ctx,
        &BundleRequest {
            program: BUNDLE.to_string(),
            sysroot: req.seal.clone(),
            src_repo: req.trust_repo.clone(),
            build: Some(trust_build),
            version: Some(pf.seal_version.clone()),
            triple: Some(triple),
            hello: HelloGate::default(),
            donor_roots: Vec::new(),
            keep_targets: Vec::new(),
            allow_dirty: false,
            smoke: true,
            strip: true,
            merge_from: None,
            out: out.join(BUNDLE),
            signing: signing.clone(),
            upload,
        },
    )?;
    builds.insert(BUNDLE.to_string(), bundle.build);

    log.say("2/3 packing the siblings with the seal's toolchain");
    // Every sibling before any packs (one that moved must not strand the others' packs),
    // then each again just before its own (the packs before it take minutes).
    for program in pf.judged.keys() {
        recheck(ctx, &pf, program, req.force_same_build)?;
    }
    for (program, judged) in &pf.judged {
        let build = recheck(ctx, &pf, program, req.force_same_build)?;
        if pf.compiler_moved() {
            log.say(format!(
                "  {program}: this compiler's pack is build {build} (published numbers skipped)"
            ));
        }
        let outcome = parts.packer.pack(
            ctx,
            &PackRequest {
                program: program.clone(),
                checkout: judged.dir.clone(),
                triple,
                build: Some(build),
                merge_from: None,
                toolchain: Some(req.seal.join("bin")),
                out: out.join(program),
                signing: signing.clone(),
                upload,
                env: Vec::new(),
            },
        )?;
        builds.insert(program.clone(), outcome.build);
    }

    if !pf.have_key {
        log.note(
            "no machine key: the packs are staged unsigned and no index is signed — a rostered \
             machine signs them (atpkg-publish sign) and publishes",
        );
        write_ready(ctx, req, &pf, &out, &builds, None)?;
        return Ok(GroupOutcome {
            builds,
            index_build: None,
            out,
        });
    }
    let mut delta = Delta::default();
    if triple == PIN_TRIPLE {
        delta.pins = builds.clone();
    } else {
        delta.target_pins.insert(triple, builds.clone());
    }
    log.say(format!(
        "3/3 indexing over the verified channel head: {}",
        builds
            .iter()
            .map(|(p, b)| format!("{p}={b}"))
            .collect::<Vec<_>>()
            .join(" ")
    ));
    let published = parts.indexer.publish(
        ctx,
        &IndexRequest {
            baseline: Baseline::ChannelHead,
            delta,
            out: out.join("index"),
            upload,
            pack_dir: None,
        },
    )?;
    if upload {
        log.say(format!(
            "published: {} and atpkg-index-{}. Clients: aterm pkg update",
            builds
                .iter()
                .map(|(p, b)| format!("{p} {b}"))
                .collect::<Vec<_>>()
                .join(", "),
            published.index_build
        ));
    } else {
        write_ready(ctx, req, &pf, &out, &builds, Some(published.index_build))?;
    }
    Ok(GroupOutcome {
        builds,
        index_build: Some(published.index_build),
        out,
    })
}

fn write_ready(
    ctx: &Ctx,
    req: &GroupRequest,
    pf: &Preflight,
    out: &Path,
    builds: &BTreeMap<String, u64>,
    index: Option<u64>,
) -> Result<()> {
    let text = format!(
        "# rustc coherence group staged from seal trust-{commit} ({version})\n\n\
         Staged (NOT published) under {out}: {builds}{index}.\n\
         Seed: {seed}.\n\
         {signed}\n\n\
         To publish from a rostered machine holding this seal:\n\n    \
         atpkg-publish publish-group rustc --seal {seal} --upload\n",
        commit = pf.seal_commit.chars().take(10).collect::<String>(),
        version = pf.seal_version,
        out = ctx.shown(out),
        builds = builds
            .iter()
            .map(|(p, b)| format!("{p} {b}"))
            .collect::<Vec<_>>()
            .join(", "),
        index = index.map(|n| format!(", index {n}")).unwrap_or_default(),
        seed = pf.seed_line.as_deref().unwrap_or("no SEED-MINT receipt"),
        signed = if pf.have_key {
            "Packs and index are SIGNED by this machine's key."
        } else {
            "Packs are UNSIGNED (no machine key here); a rostered machine signs them."
        },
        seal = ctx.shown(&req.seal),
    );
    let path = out.join("READY.md");
    std::fs::write(&path, text).map_err(|e| Error::io(format!("write {}", path.display()), &e))
}

/// `atpkg-publish publish-group rustc --seal <dir> [--trust-repo DIR] [--checkout
/// NAME=DIR]… [--out DIR] [--dry-run | --upload] [--force-same-build]
/// [--allow-build-tree-seal]`.
///
/// # Errors
///
/// A usage error for a bad command line.
pub fn parse_request(ctx: &Ctx, args: &[String]) -> Result<GroupRequest> {
    let Some((group, rest)) = args.split_first() else {
        return Err(Error::usage(
            "publish-group rustc --seal <dir> [--dry-run | --upload]",
        ));
    };
    if group != RUSTC_GROUP {
        return Err(Error::usage(format!(
            "publish-group: `{group}` is not a coherence group this publishes (rustc)"
        )));
    }
    let mut req = GroupRequest {
        seal: PathBuf::new(),
        trust_repo: lineage_switch::default_trust_repo(ctx),
        checkouts: BTreeMap::new(),
        mode: Mode::Stage,
        force_same_build: false,
        allow_build_tree_seal: false,
        out: None,
    };
    let mut seal = None;
    let (mut dry, mut up) = (false, false);
    let mut i = 0;
    while i < rest.len() {
        let flag = rest[i].as_str();
        match flag {
            "--seal" => seal = Some(PathBuf::from(flag_value(rest, &mut i, flag)?)),
            "--trust-repo" => req.trust_repo = PathBuf::from(flag_value(rest, &mut i, flag)?),
            "--checkout" => {
                let v = flag_value(rest, &mut i, flag)?;
                let (name, dir) = v.split_once('=').ok_or_else(|| {
                    Error::usage(format!("--checkout {v}: expected <name>=<dir>"))
                })?;
                if !siblings().contains(&name) {
                    return Err(Error::usage(format!(
                        "--checkout {name}: not a sibling of the rustc group ({})",
                        siblings().join(", ")
                    )));
                }
                req.checkouts.insert(name.to_string(), PathBuf::from(dir));
            }
            "--out" => req.out = Some(PathBuf::from(flag_value(rest, &mut i, flag)?)),
            "--dry-run" => dry = true,
            "--upload" => up = true,
            "--force-same-build" => req.force_same_build = true,
            "--allow-build-tree-seal" => req.allow_build_tree_seal = true,
            other => {
                return Err(Error::usage(format!(
                    "publish-group: unknown argument `{other}` (atpkg-publish help)"
                )));
            }
        }
        i += 1;
    }
    req.mode = match (dry, up) {
        (true, true) => return Err(Error::usage("--dry-run and --upload are two runs")),
        (true, false) => Mode::DryRun,
        (false, true) => Mode::Upload,
        (false, false) => Mode::Stage,
    };
    req.seal = seal.ok_or_else(|| {
        Error::usage("publish-group rustc needs --seal <the sealed toolchain dir>")
    })?;
    Ok(req)
}

/// `atpkg-publish publish-group …`.
///
/// # Errors
///
/// A usage error for a bad command line.
pub fn run(ctx: &Ctx, args: &[String]) -> Result<()> {
    let req = parse_request(ctx, args)?;
    let parts = Parts {
        probe: &RealProbe,
        bundle: &crate::artifact::bundle::PackBundle,
        packer: &crate::artifact::pack::Pack,
        indexer: &super::index::Index,
    };
    publish_group(
        ctx,
        &req,
        &parts,
        &mut Log::echo("atpkg-publish publish-group"),
    )
    .map(|_| ())
}
