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

/// The `supervisor_version=` and `supervisor_vendor=` words of `status` and `sessions`: the
/// versions a proven contract v3 claim announced
/// ([`crate::session_timeline::SessionMeta::live_scoped`],
/// `docs/DESIGN-harness-repos-2026-10-24.md` §5, §11 D48), percent-encoded; `-` for a word the
/// claim did not name, and for both under any other claim or none.
#[must_use]
pub(crate) fn wire_versions(scoped: Option<&Scoped>) -> (String, String) {
    let word = |v: Option<&str>| v.map_or_else(|| "-".to_string(), aterm_control::wire::pct_encode);
    match scoped {
        Some(s) => (
            word(s.claim.version.as_ref().map(|v| v.text.as_str())),
            word(s.claim.vendor.as_deref()),
        ),
        None => ("-".to_string(), "-".to_string()),
    }
}

/// Return the claimed pid if `facts` prove rostered `holder`'s contract 3 claim: the kernel's
/// peer is the pid the holder names, which leads the session's foreground group, and its image is
/// a build of the row's program (`facts.alab_image`, read by [`runs_harness_build`]).
#[must_use]
#[cfg(test)]
pub(crate) fn proves(holder: &str, facts: crate::self_claim::Facts) -> Option<u32> {
    proves_in(aterm_agent::harness::contract::ROSTER, holder, facts)
}

fn proves_in(rows: &'static [Row], holder: &str, facts: crate::self_claim::Facts) -> Option<u32> {
    let (_, pid) = aterm_agent::harness::contract::holder_in(rows, holder)?;
    let leads = i64::from(facts.foreground) == i64::from(pid);
    (facts.peer == Some(pid) && leads && facts.alab_image).then_some(pid)
}

/// Prove rostered `holder`'s claim on `facts` ([`proves`]) and capture its kernel start.
#[must_use]
#[cfg(test)]
pub(crate) fn prove(
    holder: &str,
    facts: crate::self_claim::Facts,
) -> Option<crate::self_claim::Proven> {
    prove_in(aterm_agent::harness::contract::ROSTER, holder, facts)
}

/// The same kernel proof and birth capture under the session's explicit catalog.
pub(crate) fn prove_in(
    rows: &'static [Row],
    holder: &str,
    facts: crate::self_claim::Facts,
) -> Option<crate::self_claim::Proven> {
    let pid = proves_in(rows, holder, facts)?;
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

/// The development seam that makes this aterm answer as one predating contract 3:
/// `ATERM_DEV_NO_CONTRACT3=1` in a debug or dev-seams build's environment
/// ([`aterm_types::dev_seam`]; a shipped aterm reads no such variable). `lab harness-proof`'s
/// negative control (`docs/DESIGN-harness-repos-2026-10-24.md` §9 step 2) runs its instance under
/// it, so a harness must pass through (§8.2) where no `contract=` is served.
pub(crate) const NO_CONTRACT3_SEAM: &str = "ATERM_DEV_NO_CONTRACT3";

/// The `contract=` value this aterm answers `harness policy` with ([`served_with`] of the seam,
/// read once).
#[must_use]
pub(crate) fn served() -> Option<&'static str> {
    static SERVED: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    SERVED
        .get_or_init(|| served_with(aterm_types::dev_seam!(NO_CONTRACT3_SEAM).as_deref()))
        .as_deref()
}

/// The `contract=` value for a seam value `seam`: the contracts this aterm serves
/// ([`aterm_agent::harness::contract::served_word`], `2,3`), or `None` under
/// `ATERM_DEV_NO_CONTRACT3=1` ([`NO_CONTRACT3_SEAM`]), where the reply carries no `contract=` key
/// at all, as an aterm predating contract 3 answers. Any other value serves.
#[must_use]
pub(crate) fn served_with(seam: Option<&std::ffi::OsStr>) -> Option<String> {
    (seam != Some(std::ffi::OsStr::new("1"))).then(aterm_agent::harness::contract::served_word)
}

/// Whether a `meta set supervisor <holder> <words…>` is refused as an aterm predating contract 3
/// refuses it: it serves no contract 3 (`serves` false, [`served`]) and a word names `contract=`,
/// which the v2 grammar (`[ttl=<ms>]`) never took. A v2 claim is read as before either way.
#[must_use]
pub(crate) fn refused_before_v3(serves: bool, words: &[&str]) -> bool {
    !serves && words.iter().any(|w| w.starts_with("contract="))
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
#[path = "harness_claim_tests.rs"]
mod tests;
