// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Program table ([`PROGRAMS`]) defining each program's repo, coherence group, and pack shape.

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
        /// `Some` for an ALab harness row (`atpkg::harness`), packed to [`Harness`]'s rule.
        harness: Option<Harness>,
    },
    /// A whole sysroot (bin/ + lib/), packed from a staged toolchain. The agent cutting it
    /// packs it on a release-credential machine; the lanes only say what is owed.
    Bundle(BundleDef),
    /// A prebuilt PACKAGE TREE another pipeline built, signed and smoke-tested (layout v1:
    /// `codex-package.json` + `alab-build.json` at its root), packed whole by
    /// `atpkg-publish pack-package` with no rebuild and no restrip. Today one: codex-alab,
    /// ALab's drop-in codex (`alab/package.sh` in its repo). The row's program is an
    /// ALab-built agent program of the client (`atpkg::alab::ALAB_AGENTS`), vendor-direct
    /// everywhere ALab does not build it; no lane reads it, and the index switches its block
    /// to this row only on an explicit `--pin` (`channel::index::plan`).
    Package(PackageDef),
    /// An upstream formula bottle or release archive relocated into a self-contained store
    /// payload by `atpkg-publish pack-bottle` (`git`, `gh`). No unattended lane reads it;
    /// `index --pin` pins it once staged.
    Bottle(BottleDef),
}

/// What a harness row's pack is held to beyond a plain row's
/// (`docs/DESIGN-harness-repos-2026-10-24.md` §5, §6.1). The pack refuses, before it builds:
/// a repo `expose` naming anything but [`Self::tool`], any `bundle`, a program-named package
/// that is absent or not at `MAJOR.MINOR.0`, a checkout that is not clean, a `HEAD` that is not
/// on `origin/main` of the staging owner's repo, and a build number that is not the checkout's
/// `HEAD` commit count. It builds `--locked` and calls the pack `MAJOR.MINOR.0+g<9-hex HEAD>`,
/// the publication rule's spelling of a dev build. Its pin installs it on every client, as any
/// program's does (owner ruling 2026-10-27, that page's §11 D45).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Harness {
    /// The one tool it ships and shims: the repo's binary. A repo that declares no `expose`
    /// ships this one, never the uncurated name-token fallback.
    pub tool: &'static str,
}

/// How a prebuilt package tree is packed.
#[derive(Clone, Copy, Debug)]
pub struct PackageDef {
    /// The tools it shims: the package's entrypoint is `bin/<the one tool>`.
    pub exposes: &'static [&'static str],
    /// The Apple targets one build carries (the client's `AlabAgent::targets`).
    pub darwin: &'static [Triple],
}

/// How an upstream bottle or archive tree is packed.
#[derive(Clone, Copy, Debug)]
pub struct BottleDef {
    /// The tools it shims (`bin/<tool>`).
    pub exposes: &'static [&'static str],
    /// The Apple targets one build carries.
    pub darwin: &'static [Triple],
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
    assert!(
        !atpkg::harness::is_harness(name),
        "a harness is a `harness` row: a plain row would pack it to the looser rule"
    );
    ProgramDef {
        name,
        repo: name,
        group,
        system: None,
        unavailable_hint: None,
        shape: Shape::Plain {
            darwin,
            harness: None,
        },
    }
}

/// An ALab harness: repo = program (the one-word rule), Apple silicon only, ungrouped, packed
/// to [`Harness`]'s rule, and in the client's default set like every program the index pins.
const fn harness(name: &'static str, tool: &'static str) -> ProgramDef {
    assert!(
        atpkg::harness::is_harness(name),
        "a harness row is named `<agent>-alab`, or no client knows it is one"
    );
    ProgramDef {
        name,
        repo: name,
        group: None,
        system: None,
        unavailable_hint: None,
        shape: Shape::Plain {
            darwin: ARM_DARWIN,
            harness: Some(Harness { tool }),
        },
    }
}

/// Every program the channel pins, in index order.
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
    ProgramDef {
        name: "codex",
        repo: "codex-alab",
        group: None,
        system: None,
        unavailable_hint: None,
        shape: Shape::Package(PackageDef {
            exposes: &["codex"],
            darwin: ARM_DARWIN,
        }),
    },
    // The ALab harnesses, each its own repo's one binary
    // (`docs/DESIGN-harness-repos-2026-10-24.md` §6.1). The tool is what the repo's
    // `[workspace.metadata.atpkg] expose` names; kimi-alab declares none, so its row names
    // its one `[[bin]]`.
    harness("claude-code-alab", "claude-alab"),
    harness("muse-alab", "muse-alab"),
    harness("agy-alab", "agy-alab"),
    harness("grok-alab", "grok-alab"),
    harness("kimi-alab", "kimi-alab"),
    ProgramDef {
        name: "gh",
        repo: "aterm",
        group: None,
        system: Some("gh"),
        unavailable_hint: None,
        shape: Shape::Bottle(BottleDef {
            exposes: &["gh"],
            darwin: DUAL_DARWIN,
        }),
    },
    ProgramDef {
        name: "git",
        repo: "aterm",
        group: None,
        system: None,
        unavailable_hint: None,
        shape: Shape::Bottle(BottleDef {
            exposes: &["git"],
            darwin: DUAL_DARWIN,
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
    pub const fn darwin_triples(&self) -> &'static [Triple] {
        match self.shape {
            Shape::Plain { darwin, .. } => darwin,
            Shape::Bundle(b) => b.darwin,
            Shape::Package(p) => p.darwin,
            Shape::Bottle(b) => b.darwin,
        }
    }

    /// Whether the lanes pack it themselves.
    #[must_use]
    pub const fn is_plain(&self) -> bool {
        matches!(self.shape, Shape::Plain { .. })
    }

    /// Its harness rule, when it is a harness row.
    #[must_use]
    pub const fn harness(&self) -> Option<Harness> {
        match self.shape {
            Shape::Plain { harness, .. } => harness,
            Shape::Bundle(_) | Shape::Package(_) | Shape::Bottle(_) => None,
        }
    }

    /// Whether the unattended lanes read it at all: a [`Shape::Package`] or [`Shape::Bottle`]
    /// row is packed and pinned only by a person (`pack-package` / `pack-bottle`, then
    /// `index --pin`), so no lane looks for its releases, reports it owed or pins it.
    #[must_use]
    pub const fn lane_tracked(&self) -> bool {
        !matches!(self.shape, Shape::Package(_) | Shape::Bottle(_))
    }
}

/// `name`'s row when it is the PACKAGE ROW of an ALab-built agent program: a
/// [`Shape::Package`] row whose repo is the one the client routes from
/// (`atpkg::alab::AlabAgent::repo`). `None` for every other program, and for a row the
/// client would never route (so no index this writer signs names a repo the client ignores).
#[must_use]
pub fn alab_package(name: &str) -> Option<&'static ProgramDef> {
    let agent = atpkg::alab::agent(name)?;
    get(name).filter(|d| matches!(d.shape, Shape::Package(_)) && d.repo == agent.repo)
}
