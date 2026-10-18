// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The interfaces between the producer's parts: the lanes ([`crate::lane`]) and the
//! `rustc` group publish ([`crate::channel`]) pack through [`Packer`] / [`BundlePacker`]
//! ([`crate::artifact`]), and the lanes publish an index through [`Indexer`]
//! ([`crate::channel`]) with a [`Delta`] over the verified channel head. The lanes' tests
//! drive fakes of these traits, so the decision logic is tested without a build or a
//! network.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use crate::ctx::Ctx;
use crate::error::Result;
use crate::triple::Triple;

/// A program build number (`git rev-list --count` of the packed commit, or a vendor stamp).
pub type Build = u64;

/// How a pack's manifest is signed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Signing {
    /// With the machine key (`Ctx::identity.key`).
    #[default]
    MachineKey,
    /// Not at all: the manifest is staged for a signing ceremony on a rostered machine
    /// (`atpkg-publish sign`). An unsigned pack is never uploaded.
    Unsigned,
}

/// How the pack's lineage was proven, as its `[artifact.provenance] lineage` records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lineage {
    /// The bundle's own LINEAGE receipt.
    Lineage,
    /// The trust repo's SEED-MINT receipt.
    SeedMint,
    /// No receipt: admitted by content only, while the receipt switch reads dormant.
    Unproven,
}

/// One plain program build, packed for one target.
#[derive(Clone, Debug)]
pub struct PackRequest {
    /// The program (a [`crate::programs::Shape::Plain`] row).
    pub program: String,
    /// The source checkout, at the commit to pack.
    pub checkout: PathBuf,
    /// The target. The build passes `--target` and the arch gate holds the bytes to it.
    pub triple: Triple,
    /// The build number; `None` is `git rev-list --count HEAD` of the checkout. A repack
    /// of unchanged source under a new compiler needs its own.
    pub build: Option<Build>,
    /// Extend this manifest (its bytes verbatim, this target's row appended) instead of
    /// starting one: one build, every target.
    pub merge_from: Option<PathBuf>,
    /// A Trust bin dir to build with; `None` resolves the store's (physically, leased
    /// for the whole pack).
    pub toolchain: Option<PathBuf>,
    /// Where the stage, the asset and the manifest go.
    pub out: PathBuf,
    /// How the manifest is signed.
    pub signing: Signing,
    /// Upload the asset and the signed manifest to the staging owner's
    /// `atpkg-<program>-<build>` release.
    pub upload: bool,
    /// Extra build environment (a cross linker: `CARGO_TARGET_<TRIPLE>_LINKER`).
    pub env: Vec<(String, String)>,
}

/// What a pack produced.
#[derive(Clone, Debug)]
pub struct PackOutcome {
    /// The program.
    pub program: String,
    /// Its build number.
    pub build: Build,
    /// Its version (the program's own).
    pub version: String,
    /// The target packed.
    pub triple: Triple,
    /// The `.tar.zst` asset.
    pub asset: PathBuf,
    /// The `pkg-<program>-<build>.toml` manifest (every row it carries).
    pub manifest: PathBuf,
    /// Its signature, when signed.
    pub signature: Option<PathBuf>,
    /// The tools it shims.
    pub exposes: Vec<String>,
    /// How the compiler's lineage was proven.
    pub lineage: Lineage,
    /// Whether it was uploaded to staging.
    pub uploaded: bool,
}

/// Packs plain programs ([`crate::artifact::pack`]).
pub trait Packer {
    /// Whether this machine can pack at all, probed before a lane builds anything: a pack
    /// that dies deep inside a build would latch a program with nothing wrong with it.
    ///
    /// # Errors
    ///
    /// What the machine lacks, worded for the lane's log.
    fn can_pack(&self) -> std::result::Result<(), String>;

    /// Build, gate, stage, sign and (maybe) upload one target of one build.
    ///
    /// # Errors
    ///
    /// Any gate's refusal; nothing is signed or uploaded after one.
    fn pack(&self, ctx: &Ctx, request: &PackRequest) -> Result<PackOutcome>;
}

/// The hello-world probe switch and the Trust sysroot that admits the packed compiler.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelloGate {
    /// Run the hello-world probe (disabled only for a local debugging pack).
    pub enabled: bool,
    /// The Trust sysroot the packed compiler was built from (trust-mc); used for
    /// admission and the payload scan even when the probe is disabled.
    pub sysroot: Option<PathBuf>,
}

impl Default for HelloGate {
    fn default() -> Self {
        Self {
            enabled: true,
            sysroot: None,
        }
    }
}

/// One sysroot bundle, packed from a staged toolchain.
#[derive(Clone, Debug)]
pub struct BundleRequest {
    /// The program (a [`crate::programs::Shape::Bundle`] row).
    pub program: String,
    /// The staged sysroot (bin/ + lib/): a sealed toolchain, or a copy of one.
    pub sysroot: PathBuf,
    /// The source checkout whose commit names the build (and whose cleanliness is gated).
    pub src_repo: PathBuf,
    /// The build number; `None` is the source checkout's `rev-list --count HEAD`.
    pub build: Option<Build>,
    /// The version; `None` derives it from what is packed.
    pub version: Option<String>,
    /// The target; `None` is the staged compiler's own `-vV` host.
    pub triple: Option<Triple>,
    /// The hello-world gate.
    pub hello: HelloGate,
    /// Trust toolchain roots pack-time relocation may vendor a library from.
    pub donor_roots: Vec<PathBuf>,
    /// Foreign `lib/rustlib/<triple>` sysroots to keep (a local cross-capable bundle).
    pub keep_targets: Vec<String>,
    /// Pack a dirty source checkout (stamped `-dirty`; never uploaded).
    pub allow_dirty: bool,
    /// Run the per-bin smoke-exec and tippy gates.
    pub smoke: bool,
    /// Strip debug symbols from every Mach-O.
    pub strip: bool,
    /// Extend this manifest with this target's row.
    pub merge_from: Option<PathBuf>,
    /// Where the asset and manifest go.
    pub out: PathBuf,
    /// How the manifest is signed.
    pub signing: Signing,
    /// Upload to the staging owner.
    pub upload: bool,
}

/// Packs sysroot bundles ([`crate::artifact::bundle`]).
pub trait BundlePacker {
    /// Stage, prove, relocate, gate, sign and (maybe) upload one bundle.
    ///
    /// # Errors
    ///
    /// Any gate's refusal; nothing is signed or uploaded after one.
    fn pack_bundle(&self, ctx: &Ctx, request: &BundleRequest) -> Result<PackOutcome>;
}

/// One revocation in the channel's `yanked` list.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Yank {
    /// `<program>@<build>` (the client's `is_yanked`).
    Build(String, Build),
    /// `<program>@<version>` (a vendor-direct program's version latch).
    Version(String, String),
}

/// A pin move that drops an Apple target the previous build served, on purpose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TripleDrop {
    /// The program.
    pub program: String,
    /// The target its new build no longer carries.
    pub triple: Triple,
    /// Why every client on that target is held on the old build.
    pub reason: String,
}

/// One publish's change to the channel, applied to the verified head. Everything it does
/// not name is carried: pins, per-target pins, `min_build`, `yanked`, `valid_until`, every
/// program's metadata.
#[derive(Clone, Debug, Default)]
pub struct Delta {
    /// `pin` moves (or new programs, which must be rows of [`crate::programs::PROGRAMS`]).
    pub pins: BTreeMap<String, Build>,
    /// `pin_by_target` moves, per target.
    pub target_pins: BTreeMap<Triple, BTreeMap<String, Build>>,
    /// Revocations to add.
    pub yank: BTreeSet<Yank>,
    /// Revocations to lift (each must be on the head's list).
    pub unyank: BTreeSet<Yank>,
    /// A new channel-wide floor (lowering one also needs `lower_min_build`).
    pub min_build: Option<u64>,
    /// Allow `min_build` below the head's.
    pub lower_min_build: bool,
    /// Programs the head pins that this publish retires.
    pub retire: BTreeSet<String>,
    /// Per-target overlays this publish drops: a whole target (`None`) or one program.
    pub retire_target: BTreeSet<(Triple, Option<String>)>,
    /// Apple targets a moved pin may drop.
    pub triple_drops: Vec<TripleDrop>,
    /// A new freshness horizon (RFC3339 UTC); `None` carries the head's.
    pub valid_until: Option<String>,
}

/// What the index is built over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Baseline {
    /// The channel's newest published index, fetched and verified (the default).
    ChannelHead,
    /// A verified quad already on disk (its number must be the channel's head when
    /// uploading).
    Dir(PathBuf),
    /// A genuine first publish of a channel: no carry, no floors.
    First {
        /// The channel name.
        channel: String,
        /// The index number to mint.
        index_build: u64,
    },
}

/// One index publish.
#[derive(Clone, Debug)]
pub struct IndexRequest {
    /// What it is built over.
    pub baseline: Baseline,
    /// The change.
    pub delta: Delta,
    /// Where the signed quad is written.
    pub out: PathBuf,
    /// Publish: make every pinned pack public, then the quad. `false` is a dry run that
    /// still signs and self-verifies, and touches no host but to read an ALab pack it pins.
    pub upload: bool,
    /// Where an ALab pack this index pins is read from for a dry run (its manifest pair and
    /// archive), instead of the hosts: a local proof. Refused with `upload`.
    pub pack_dir: Option<PathBuf>,
}

/// A published (or staged) index.
#[derive(Clone, Debug)]
pub struct Published {
    /// Its number (the baseline's + 1).
    pub index_build: u64,
    /// The directory holding the signed quad.
    pub dir: PathBuf,
    /// Whether it went to the channel.
    pub uploaded: bool,
}

/// Publishes the channel index ([`crate::channel::index`]).
pub trait Indexer {
    /// Plan the delta over the baseline, sign, self-verify and (maybe) publish.
    ///
    /// # Errors
    ///
    /// Any refusal (a downgrade, an omission, a triple drop, a lost compare-and-swap).
    fn publish(&self, ctx: &Ctx, request: &IndexRequest) -> Result<Published>;
}
