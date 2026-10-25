// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CONTRACT v3's PROOF: a rostered harness's scoped claim (`grok-alab@<pid> contract=3
//! scope=relaunch,upgrade`, [`aterm_agent::harness::contract`]), proven on the kernel's facts and
//! the row's image rule, and kept beside the session's claim apart from codex-alab's v2 proof.
//!
//! The kernel rules are codex-alab's (`crates/aterm-gui/src/control.rs`,
//! `prove_self_claim_by`): the claim arrives on a direct local connection whose kernel peer pid is
//! `<pid>`, and `<pid>` leads the session's foreground process group. The image rule is the row's
//! (`docs/DESIGN-harness-repos-2026-10-24.md` §8.1 item 3): `<pid>`'s executable resolves inside
//! this user's atpkg `store/<package>/<build>/` with that build complete (`<build>.ready`), or
//! inside the checkout of an `aterm pkg link <package>` dev link. The holder string alone proves
//! nothing.
//!
//! A proven scoped claim is never a v2 self-claim: it sits in
//! [`crate::session_timeline::SupervisorClaim::scoped`], never in `proven`, so nothing that acts
//! for codex-alab's self-claim (its record copies, its crash rule, its wall answer, its acts)
//! acts for it. What it changes is what its scope says: `status supervisor_proven=1` (the
//! claimant's own confirmation), and a claim the window's host holds replaced, as a proof does.

use std::path::Path;

use aterm_agent::harness::contract::{Claim, Row};

/// A proven v3 claim: the process the kernel named, and what it claimed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Scoped {
    /// The claimant's identity (pid and kernel start), as the proof captured it.
    pub(crate) proven: crate::self_claim::Proven,
    /// The claim as read: its row, scope and versions.
    pub(crate) claim: Claim,
}

/// Return the claimed pid if `facts` prove rostered `holder`'s contract 3 claim: the kernel's
/// peer is the pid the holder names, which leads the session's foreground group, and its image is
/// a build of the row's program (`facts.alab_image`, read by [`runs_harness_build`]).
#[must_use]
pub(crate) fn proves(holder: &str, facts: crate::self_claim::Facts) -> Option<u32> {
    let (_, pid) = aterm_agent::harness::contract::holder(holder)?;
    let leads = i64::from(facts.foreground) == i64::from(pid);
    (facts.peer == Some(pid) && leads && facts.alab_image).then_some(pid)
}

/// Prove rostered `holder`'s claim on `facts` ([`proves`]) and capture its kernel start.
#[must_use]
pub(crate) fn prove(
    holder: &str,
    facts: crate::self_claim::Facts,
) -> Option<crate::self_claim::Proven> {
    let pid = proves(holder, facts)?;
    let start = aterm_agent::harness::restart_stamp::kernel_start(pid)?;
    Some(crate::self_claim::Proven { pid, start })
}

/// Whether `exe` (already resolved) is a build of `row`'s program in `layout`: inside
/// `store/<package>/<build>/` of a complete build, or inside the checkout `package` is dev-linked
/// to.
#[must_use]
pub(crate) fn image_in(layout: &atpkg::store::Layout, row: &Row, exe: &Path) -> bool {
    let Ok(exe) = std::fs::canonicalize(exe) else {
        return false;
    };
    let store = layout.prefix.join("store").join(row.package);
    if let Ok(store) = std::fs::canonicalize(&store)
        && let Ok(rest) = exe.strip_prefix(&store)
        && let Some(std::path::Component::Normal(build)) = rest.components().next()
        && build
            .to_str()
            .is_some_and(|b| !b.is_empty() && b.bytes().all(|c| c.is_ascii_digit()))
    {
        let build_dir = store.join(build);
        let marker = store.join(format!("{}.ready", build.to_string_lossy()));
        if std::fs::symlink_metadata(&marker).is_ok_and(|m| m.file_type().is_file())
            && atpkg::store::build_is_complete(&build_dir)
        {
            return true;
        }
    }
    atpkg::linked_checkout(layout, row.package)
        .and_then(|checkout| std::fs::canonicalize(checkout).ok())
        .is_some_and(|checkout| checkout.parent().is_some() && exe.starts_with(&checkout))
}

/// Whether process `pid` runs a build of `row`'s program ([`image_in`], this user's configured
/// atpkg prefix).
#[must_use]
pub(crate) fn runs_harness_build(row: &Row, pid: u32) -> bool {
    let Some(exe) = i32::try_from(pid).ok().and_then(aterm_uds::sudo::exe_path) else {
        return false;
    };
    atpkg::store::resolve_configured().is_some_and(|layout| image_in(&layout, row, &exe))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_agent::harness::contract;

    fn grok_alab() -> &'static Row {
        contract::row_named("grok-alab").expect("rostered")
    }

    /// A layout in a scratch prefix holding `store/grok-alab/<build>/bin/grok-alab`, complete
    /// (`<build>.ready` as atpkg writes it) when `ready`.
    fn store_with_build(
        root: &Path,
        build: u64,
        ready: bool,
    ) -> (atpkg::store::Layout, std::path::PathBuf) {
        let layout = atpkg::store::Layout {
            prefix: root.join("pkg"),
        };
        let dir = layout.build_dir("grok-alab", build);
        std::fs::create_dir_all(dir.join("bin")).expect("build dir");
        let exe = dir.join("bin").join("grok-alab");
        std::fs::write(&exe, b"#!/bin/sh\n").expect("exe");
        if ready {
            atpkg::store::mark_build_ready(&dir).expect("ready");
        }
        (layout, exe)
    }

    /// THE KERNEL RULES are codex-alab's, keyed on the roster: the peer that leads the
    /// foreground and runs a build of the row's program proves `grok-alab@<pid>`; any other peer,
    /// foreground or image does not, and neither does a holder off the roster or codex-alab's.
    #[test]
    fn only_the_rostered_peer_that_leads_the_foreground_on_its_build_proves() {
        let facts = |peer, foreground, image| crate::self_claim::Facts {
            peer,
            foreground,
            alab_image: image,
        };
        assert_eq!(
            proves("grok-alab@500", facts(Some(500), 500, true)),
            Some(500)
        );
        assert_eq!(proves("grok-alab@500", facts(Some(500), 500, false)), None);
        assert_eq!(proves("grok-alab@500", facts(Some(501), 500, true)), None);
        assert_eq!(proves("grok-alab@500", facts(Some(500), 499, true)), None);
        assert_eq!(proves("grok-alab@500", facts(None, 500, true)), None);
        assert_eq!(proves("grok-alab@500", facts(Some(500), -1, true)), None);
        assert_eq!(proves("codex-alab@500", facts(Some(500), 500, true)), None);
        assert_eq!(proves("kimi-alab@500", facts(Some(500), 500, true)), None);
        let me = std::process::id();
        let proven = prove(
            &format!("grok-alab@{me}"),
            facts(Some(me), i32::try_from(me).expect("pid"), true),
        )
        .expect("this process proves its own claim");
        assert_eq!(proven.pid, me);
        assert!(proven.alive());
    }

    /// THE IMAGE RULE: a complete store build of the row's program proves; the same file in a
    /// build that is not ready, in another program's store, or anywhere else does not; a dev
    /// link's checkout proves.
    #[test]
    fn only_a_complete_store_build_or_the_dev_link_of_the_rows_program_proves() {
        let dir = aterm_tempfile::TempDir::new().expect("dir");
        let root = std::fs::canonicalize(dir.path()).expect("canonical");
        let (layout, exe) = store_with_build(&root, 412, true);
        assert!(image_in(&layout, grok_alab(), &exe), "positive control");

        let (unready, unready_exe) = store_with_build(&root.join("b"), 413, false);
        assert!(!image_in(&unready, grok_alab(), &unready_exe), "not ready");

        let other = layout.build_dir("grok", 7);
        std::fs::create_dir_all(other.join("bin")).expect("dir");
        let vendor = other.join("bin").join("grok");
        std::fs::write(&vendor, b"x").expect("vendor");
        atpkg::store::mark_build_ready(&other).expect("ready");
        assert!(
            !image_in(&layout, grok_alab(), &vendor),
            "the vendor's store is not the harness's"
        );

        let stray = root.join("stray-grok-alab");
        std::fs::write(&stray, b"x").expect("stray");
        assert!(
            !image_in(&layout, grok_alab(), &stray),
            "a file named like the harness"
        );
        assert!(
            !image_in(&layout, grok_alab(), &root.join("missing")),
            "nothing there"
        );

        let checkout = root.join("checkout");
        let built = checkout.join("target").join("release").join("grok-alab");
        std::fs::create_dir_all(built.parent().expect("dir")).expect("dir");
        std::fs::write(&built, b"x").expect("built");
        assert!(!image_in(&layout, grok_alab(), &built), "not linked yet");
        atpkg::link(
            &layout,
            "grok-alab",
            &checkout,
            &[std::path::PathBuf::from("target/release/grok-alab")],
        )
        .expect("dev link");
        assert!(
            image_in(&layout, grok_alab(), &built),
            "the dev link's checkout"
        );
        assert!(
            !image_in(&layout, grok_alab(), &stray),
            "still nothing outside it"
        );
    }
}
