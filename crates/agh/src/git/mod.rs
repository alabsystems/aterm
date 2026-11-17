// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! agh's git (design §6.9): the isolated runner every git child of agh or its callers runs
//! under, the credential helper git calls back, the grants that let the helper answer a
//! release-tier owner or a push's writer, and how a git keeps a read from fetching lazily
//! ([`lazy_fetch`], asked of a git its caller runs). This module runs the `git` program. It is not
//! `agh::github::git`, M1's REST git-data API (commits, refs, trees).

mod ancestry;
mod control;
pub mod credential;
mod fetch;
pub mod grant;
#[cfg(test)]
mod grant_conformance_tests;
pub mod lazy_fetch;
mod run;
#[cfg(unix)]
mod source_config;

pub use ancestry::{AncestryQuery, CommitRead, CommitReadInput};
pub use control::{
    ControlRefChange, ControlRefTransaction, PUBLISHER_FENCE_REF, RELEASE_LEASE_REF,
};
pub use fetch::{FETCH_ROWS_MAX, FetchBatch, FetchRow};
pub use run::{
    FetchScope, GitOp, GitOut, GitRun, HelperCommand, HelperHost, OBJECT_SOURCE_WORDS, Overriding,
};

/// Admit Git's complete `rev-parse --is-shallow-repository` answer for a native push.
/// Callers may use the same admission before expensive checks; the push rechecks it.
/// # Errors
/// A shallow source or any answer other than the exact non-shallow record is refused.
pub fn require_complete_source(stdout: &[u8]) -> crate::Result<()> {
    if stdout == b"false\n" {
        Ok(())
    } else {
        Err(crate::Error::refused(
            "native push requires a complete, non-shallow source repository",
        ))
    }
}

/// Parse a complete native advertisement, optionally selecting one exact full ref.
/// Prefix neighbours never establish presence, and duplicate names never establish a value.
/// # Errors
/// Malformed lines, refs, object IDs or duplicate ref names.
pub fn advertised_refs(
    stdout: &str,
    reference: Option<&str>,
) -> crate::Result<std::collections::BTreeMap<String, String>> {
    let mut refs = std::collections::BTreeMap::new();
    for line in stdout.lines() {
        let (oid, name) = line
            .split_once('\t')
            .ok_or_else(|| crate::Error::protocol("Git ref advertisement is malformed"))?;
        let full = name.strip_suffix("^{}").unwrap_or(name);
        if !aterm_land_trailer::object_id(oid)
            || !(full == "HEAD" || (full.starts_with("refs/") && crate::path::ref_name_ok(full)))
        {
            return Err(crate::Error::protocol(
                "Git ref advertisement has an invalid ref or object ID",
            ));
        }
        if refs.insert(name.to_owned(), oid.to_owned()).is_some() {
            return Err(crate::Error::protocol(
                "Git ref advertisement has a duplicate ref",
            ));
        }
    }
    if let Some(reference) = reference {
        refs.retain(|name, _| name == reference);
    }
    Ok(refs)
}

/// One ref of an advertisement: the object it names and, for an annotated tag, the object its
/// `^{}` line names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdvertisedRef {
    /// The full ref, or `HEAD`.
    pub name: String,
    /// The object the ref names: a tag object for an annotated tag.
    pub oid: String,
    /// What an annotated tag peels to; `None` for a branch, a lightweight tag or `HEAD`.
    pub peeled: Option<String>,
}

/// [`advertised_refs`] as rows: each `^{}` line becomes its ref's `peeled`, never a row of its
/// own, and an exact `reference` selects its row alone.
/// # Errors
/// What [`advertised_refs`] refuses, and a `^{}` line whose ref the advertisement lacks.
pub fn advertised_ref_rows(
    stdout: &str,
    reference: Option<&str>,
) -> crate::Result<Vec<AdvertisedRef>> {
    let mut refs = advertised_refs(stdout, None)?;
    let peeled: std::collections::BTreeMap<String, String> = refs
        .extract_if(.., |name, _| name.ends_with("^{}"))
        .map(|(mut name, oid)| {
            name.truncate(name.len() - "^{}".len());
            (name, oid)
        })
        .collect();
    if peeled.keys().any(|name| !refs.contains_key(name)) {
        return Err(crate::Error::protocol(
            "Git ref advertisement peels a ref it does not advertise",
        ));
    }
    Ok(refs
        .into_iter()
        .filter(|(name, _)| reference.is_none_or(|r| r == name))
        .map(|(name, oid)| AdvertisedRef {
            peeled: peeled.get(&name).cloned(),
            name,
            oid,
        })
        .collect())
}

/// One validation phase of an incremental native atomic ref admission.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativePushPhase {
    /// Pack exact remote-owned roots, or complete BEFORE/AFTER metadata for
    /// an update that introduces file content.
    MetadataPack,
    /// Strictly verify that metadata pack and its original object IDs.
    MetadataVerify,
    /// Walk AFTER's connectivity, including referenced blob presence, unless
    /// a hashed parent witness already established the remote owns its closure.
    Connectivity,
}
// `run` stays a private module; its runner is re-exported to the crate alone, so
// `Github::run_git` (client.rs) calls it as `crate::git::run(self, run)`. The function and
// the module share the name in different namespaces.
#[cfg(unix)]
pub(crate) use run::{ConfigAccess, project_commit_config, project_config_names};
pub(crate) use run::{run, run_with_credential};

/// The username the helper answers: GitHub ignores it for a token, and a fixed one keeps
/// the Basic credential's shape known to the Redactor.
pub const GIT_USERNAME: &str = "x-access-token";
/// The default wall bound for local, authenticated and mutating Git children: 10 minutes.
/// Anonymous network reads use a progress-inactivity window instead.
pub const GIT_BUDGET: std::time::Duration = std::time::Duration::from_secs(600);
/// The longest a grant lives: 15 minutes.
pub const GRANT_MAX: std::time::Duration = std::time::Duration::from_secs(900);
/// The variable that carries a grant's nonce from the runner to the helper (and to
/// `whoami --git`): one spelling on both sides of the handshake.
pub(crate) const GRANT_VAR: &str = "AGH_GIT_GRANT";
/// The variable that tells the helper its profile's token file may be loose (`1`), as the
/// runner's router read it (`legacy_loose`). A caller that mounts the helper on a git it runs
/// itself ([`HelperCommand`]) and routes as a library host does (`routing::adopt_for_host`)
/// sets it for such a profile too, and checks with `credential::require_answer`'s `loose`.
pub const LOOSE_VAR: &str = "AGH_GIT_LOOSE";
/// The variables that carry the runner's `Dirs` to the helper: its config, state and home.
pub(crate) const CONFIG_DIR_VAR: &str = "AGH_GIT_CONFIG_DIR";
/// See [`CONFIG_DIR_VAR`].
pub(crate) const STATE_DIR_VAR: &str = "AGH_GIT_STATE_DIR";
/// See [`CONFIG_DIR_VAR`].
pub(crate) const HOME_VAR: &str = "AGH_GIT_HOME";
/// The variable that carries a loopback run's `127.0.0.1:<port>` host from the runner to the
/// helper, set and read only in a test or debug build.
#[cfg(any(test, debug_assertions))]
pub(crate) const LOOPBACK_VAR: &str = "AGH_GIT_LOOPBACK";
/// How far up the process tree the helper looks for the grant's spawner.
pub const ANCESTRY_DEPTH: usize = 8;
