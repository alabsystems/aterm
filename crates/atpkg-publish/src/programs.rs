// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! [`PROGRAMS`]: each program's repo, coherence group, pack shape and targets — the one
//! table the producer and every lane read.
//!
//! It holds no build numbers: the pins live in the signed index, and a publish is a typed
//! change to the verified head (`api::Delta`). A changed row reaches the snapshot lanes
//! only when a person re-runs `atpkg-publish lane install`.

use crate::triple::{AARCH64_APPLE_DARWIN, Triple, X86_64_APPLE_DARWIN};

/// Every program's install policy: prebuilt-only, fail-closed. Trust and trust-mc are
/// never built from source on a user's machine.
pub const POLICY: &str = "prebuilt-only";

/// The trust verifier tuple's coherence group. One Trust seal builds every member (Rust
/// has no stable ABI) and they move all-or-nothing: one member that cannot stage aborts the
/// whole group on every client.
pub const RUSTC_GROUP: &str = "rustc";

/// The target the channel's platform-agnostic `pin` serves. A cut on any other target
/// rides a `pin_by_target` overlay for that target alone (`Channel::for_target`).
pub const PIN_TRIPLE: Triple = AARCH64_APPLE_DARWIN;

/// Both Apple targets: a plain program packs both under one build number (the second
/// row merged onto the first).
pub const DUAL_DARWIN: &[Triple] = &[AARCH64_APPLE_DARWIN, X86_64_APPLE_DARWIN];

/// Apple silicon only: the `rustc` group, because the trust bundle's exposed trust-wp
/// drivers are rustc-private and a cross-host stage2 cannot build them; the siblings
/// move with the bundle or not at all.
pub const ARM_DARWIN: &[Triple] = &[AARCH64_APPLE_DARWIN];

// The tools the trust bundle shims, from the client (the candidate gate checks the same
// list): a seal is publishable only with every one in its `bin/` (the five trust-wp
// drivers are staged into a copy of the seal).
pub use atpkg::manifest::TRUST_EXPOSES;

/// One program.
#[derive(Debug, Clone, Copy)]
pub struct ProgramDef {
    /// The index's `[programs.<name>]` key and the program's tool identity.
    pub name: &'static str,
    /// The repo its `atpkg-<name>-<build>` releases live on, under both owners.
    pub repo: &'static str,
    /// Its coherence group, if it moves with others.
    pub group: Option<&'static str>,
    /// A binary on the user's PATH that satisfies the program (`[programs.x] system`).
    pub system: Option<&'static str>,
    /// The `unavailable on <target>: <hint>` reason for a target with no row.
    pub unavailable_hint: Option<&'static str>,
    /// What a pack of it is.
    pub shape: Shape,
}

/// What a pack of a program is, and which lane makes it.
#[derive(Debug, Clone, Copy)]
pub enum Shape {
    /// Built from its repo by `targo --unverified build`, shipped as the binaries its
    /// `[workspace.metadata.atpkg] expose` declares. The lanes track it.
    Plain {
        /// The Apple targets one build carries.
        darwin: &'static [Triple],
    },
    /// A whole sysroot (bin/ + lib/), packed from a staged toolchain. A person cuts it;
    /// the lanes only say what is owed.
    Bundle(BundleDef),
}

/// How a sysroot bundle is packed and proven.
#[derive(Debug, Clone, Copy)]
pub struct BundleDef {
    /// The tools it shims.
    pub exposes: &'static [&'static str],
    /// The Apple targets one build carries.
    pub darwin: &'static [Triple],
    /// What its hello-world compiles against.
    pub hello: Hello,
    /// Whether the sealed rustc-dev + rust-src closure is frozen into it (trust).
    pub closure: bool,
    /// Whether its pin is also held to its repo's origin/main (a cut is owed when main
    /// moves past the pin).
    pub main_gap: bool,
}

/// What a bundle's hello-world gate compiles against.
#[derive(Debug, Clone, Copy)]
pub enum Hello {
    /// The staged sysroot itself: a working toolchain.
    StagedSysroot,
    /// The Trust sysroot the packed `compiler` was built from (trust-mc's own std is the
    /// panic=abort verification sysroot, which no hello links against).
    TrustSysroot {
        /// The packed compiler, under `bin/`.
        compiler: &'static str,
    },
}

const fn plain(
    name: &'static str,
    group: Option<&'static str>,
    darwin: &'static [Triple],
) -> ProgramDef {
    ProgramDef {
        name,
        repo: name,
        group,
        system: None,
        unavailable_hint: None,
        shape: Shape::Plain { darwin },
    }
}

/// Every program the channel pins, in the order the index lists them. A vendor-direct
/// program (`atpkg::vendor_direct::is_vendor`) has no row: its client follows its vendor,
/// so neither `--pin`, `--target-pin` nor a lane ever pins it.
pub static PROGRAMS: &[ProgramDef] = &[
    plain("ay", None, DUAL_DARWIN),
    plain("clean", None, DUAL_DARWIN),
    plain("nn", None, DUAL_DARWIN),
    plain("ny", None, DUAL_DARWIN),
    plain("ty", None, DUAL_DARWIN),
    ProgramDef {
        name: "trust-mc",
        repo: "trust-mc",
        group: None,
        system: None,
        unavailable_hint: None,
        shape: Shape::Bundle(BundleDef {
            exposes: &["trust-mc-driver"],
            darwin: DUAL_DARWIN,
            hello: Hello::TrustSysroot {
                compiler: "trust-mc-compiler",
            },
            closure: false,
            main_gap: false,
        }),
    },
    plain("trust-ir", Some(RUSTC_GROUP), ARM_DARWIN),
    plain("trust-cg", Some(RUSTC_GROUP), ARM_DARWIN),
    plain("trust-vc", Some(RUSTC_GROUP), ARM_DARWIN),
    ProgramDef {
        name: "trust",
        repo: "trust",
        group: Some(RUSTC_GROUP),
        system: None,
        unavailable_hint: None,
        shape: Shape::Bundle(BundleDef {
            exposes: TRUST_EXPOSES,
            darwin: ARM_DARWIN,
            hello: Hello::StagedSysroot,
            closure: true,
            main_gap: true,
        }),
    },
];

/// The program named `name`.
#[must_use]
pub fn get(name: &str) -> Option<&'static ProgramDef> {
    PROGRAMS.iter().find(|p| p.name == name)
}

/// The members of coherence group `group`, in table order.
pub fn group_members(group: &str) -> impl Iterator<Item = &'static ProgramDef> + '_ {
    PROGRAMS.iter().filter(move |p| p.group == Some(group))
}

impl ProgramDef {
    /// The Apple targets one build of it carries.
    #[must_use]
    pub fn darwin_triples(&self) -> &'static [Triple] {
        match self.shape {
            Shape::Plain { darwin } => darwin,
            Shape::Bundle(b) => b.darwin,
        }
    }

    /// Whether the lanes pack it themselves.
    #[must_use]
    pub fn is_plain(&self) -> bool {
        matches!(self.shape, Shape::Plain { .. })
    }
}
