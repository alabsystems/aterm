// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Who wrote this code: the provenance classifier.

use std::path::{Component, Path, PathBuf};

use aterm_toml::edit::{DocumentMut, Item, TableLike};

/// One directory under `vendor/` that holds aterm's own code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FirstPartyVendored {
    /// Repo-relative directory, always `vendor/<dir>`, no trailing slash.
    pub dir: &'static str,
    /// The upstream repository this tree comes from. Names the repository,
    /// never a commit; any pin is read from the tree (`[OB-1]` prints a
    /// submodule's gitlink).
    pub upstream: &'static str,
    /// Why it is aterm's own code rather than a redistribution, and why it is under `vendor/` at
    /// all.
    pub why: &'static str,
}

/// Reviewed original first-party components, one row per directory.
pub const FIRST_PARTY_VENDORED: &[FirstPartyVendored] = &[
    FirstPartyVendored {
        dir: "vendor/astream",
        upstream: "github.com/alabsystems/astream, a git submodule pinned by this tree's gitlink",
        why: "Same owner as github.com/alabsystems/aterm — the same account, not a similar \
          name — so this is aterm's own code, not a redistribution. It is under vendor/ for a \
          BUILD reason: crates/aterm-link path-depended on a sibling checkout most clones do \
          not have, so cargo build never built the fabric bridge and no release ever shipped \
          it. It is a git submodule (vendor/README-astream.md), and aterm resolves the astream \
          crates its members use by path, outside this workspace ([OB-1] names each one it \
          finds in the graph and holds it to the submodule; vendor/README-astream.md's table \
          says what each is for); astream-term only as aterm-conformance's dev-dependency \
          (the differential VT oracle's fold, zero dependencies). Not on crates.io, so no \
          [patch.crates-io] entry could ever name it: a patch entry replaces a registry \
          package and there is none to replace. The third-party packages it DRAGS IN (none \
          since astream-cap's SHA-256 became astream-digest and the sealed transport, with \
          its astream-aead, was removed, 2026-10-15) are unaffected by this row and stay on \
          the ratchet.",
    },
    FirstPartyVendored {
        dir: "vendor/trust-build-cpu",
        upstream: "Trust crates/trust-build-cpu; the manifest records its exact source commit",
        why: "Original Andrew Yates code shared with the Trust compiler. Its Rust sources and \
          README are byte-identical to the recorded Trust commit; vendor/README-trust-build-cpu.md \
          records their hashes and the retained licences. The standalone manifest materializes \
          upstream workspace properties. A path dependency keeps hermetic releases and public \
          clones complete without fetching an unpublished Git commit. This does not exempt \
          its registry dependencies from the third-party ratchet.",
    },
    FirstPartyVendored {
        dir: "vendor/trust-jobserver-utils",
        upstream: "Trust src/jobserver_utils at 263e40c542b1; the manifest's \
          [package.metadata.aterm-origin] records the commit and path",
        why: "Original Andrew Yates code: Trust's own scheduling descriptor handoff, added to \
          Trust on 2026-10-24, which depends on the registry `jobserver` crate rather than \
          copying it. Runtime and test sources are unchanged from the recorded commit \
          (vendor/trust-jobserver-utils/README.aterm.md); the manifest only isolates the package \
          and drops an upstream workspace-relative path. A path dependency keeps hermetic \
          releases and public clones complete. This does not exempt its registry dependencies \
          from the third-party ratchet.",
    },
    FirstPartyVendored {
        dir: "vendor/trust-exec-policy",
        upstream: "Trust crates/trust-exec-policy at cae6f4b58466; the manifest's \
          [package.metadata.aterm-vendor] records the full commit and path",
        why: "Original Andrew Yates code: the maintained native final-exec policy leaf. \
          Its Rust sources, README, lockfile and retained licences are exact upstream \
          (vendor/trust-exec-policy/README.aterm.md); the standalone manifest only adds \
          this provenance record. A path dependency keeps public clones hermetic. \
          This does not exempt its registry dependencies from the third-party ratchet.",
    },
    FirstPartyVendored {
        dir: "vendor/trust-resource-controller",
        upstream: "Trust crates/trust-resource-controller at 5db4b0acbd29; the manifest's \
          [package.metadata.aterm-vendor] records the repository and commit",
        why: "Original Andrew Yates code shared with the Trust compiler: the private native \
          resource member proxy codec. Its Rust source is exact upstream; the manifest only \
          redirects libc to aterm-libc (vendor/trust-resource-controller/README.aterm.md). A \
          path dependency keeps hermetic releases and public clones complete. This does not \
          exempt its registry dependencies from the third-party ratchet.",
    },
    FirstPartyVendored {
        dir: "vendor/trust-resource-provenance",
        upstream: "Trust crates/trust-resource-provenance at 1f4f39cd0f69; the manifest's \
          [package.metadata.aterm-vendor] records the repository and commit",
        why: "Original Andrew Yates code shared with the Trust compiler: read-only native \
          provenance for retained resource custodians. Its Rust sources, README and licences \
          are exact upstream bytes, whose SHA-256 values \
          vendor/trust-resource-provenance/README.aterm.md records; the manifest only redirects \
          dependencies to maintained first-party crates. A path dependency keeps hermetic \
          releases and public clones complete. This does not exempt its registry dependencies \
          from the third-party ratchet.",
    },
    FirstPartyVendored {
        dir: "vendor/trust-sha2",
        upstream: "Trust crates/trust-sha2 at 21a4afd96990; the manifest's \
          [package.metadata.aterm-origin] records the commit and path",
        why: "Original Andrew Yates code: Trust's own SHA-2 family, transcribed from FIPS PUB \
          180-4, with no dependencies. vendor/trust-build-cpu's manifest points its hash \
          dependency here (vendor/README-trust-build-cpu.md). A path dependency keeps hermetic \
          releases and public clones complete.",
    },
];

/// The roster row whose directory contains `rel`, if any.
#[must_use]
pub fn roster_row(rel: &str) -> Option<&'static FirstPartyVendored> {
    FIRST_PARTY_VENDORED
        .iter()
        .find(|r| rel == r.dir || rel.starts_with(&format!("{}/", r.dir)))
}

/// Is this absolute package directory inside a first-party vendored root?
#[must_use]
pub fn is_first_party_vendored(root: &Path, dir: &Path) -> bool {
    FIRST_PARTY_VENDORED
        .iter()
        .any(|r| dir.starts_with(root.join(r.dir)))
}

/// Is this absolute package directory aterm's own code.
#[must_use]
pub fn is_first_party(root: &Path, dir: &Path) -> bool {
    dir.starts_with(root.join("crates")) || is_first_party_vendored(root, dir)
}

// Reviewed original components, not arbitrary repositories under the same
// account. trust-build-cpu is original Trust code shared by the compiler and
// aterm; a same-account mirror or fork retains its upstream authorship.
const FIRST_PARTY_GIT_COMPONENTS: &[(&str, &str)] = &[("trust-build-cpu", "trust")];

/// A reviewed original component from this workspace's repository owner,
/// pinned to its full commit. Account ownership alone does not establish code
/// authorship. The source is supplied by the locked resolver, not the checkout.
pub(crate) fn is_first_party_git(root: &Path, package: &str, source: &str) -> bool {
    let Some(repository) = crate::read_regular_text(root.join("Cargo.toml"))
        .ok()
        .and_then(|text| text.parse::<aterm_toml::edit::DocumentMut>().ok())
        .and_then(|doc| {
            doc.get("workspace")?
                .get("package")?
                .get("repository")?
                .as_str()
                .map(str::to_owned)
        })
    else {
        return false;
    };
    pinned_first_party_git(&repository, package, source)
}

fn pinned_first_party_git(repository: &str, package: &str, source: &str) -> bool {
    fn parts(repository: &str) -> Option<(&str, &str)> {
        let path = repository.strip_prefix("https://github.com/")?;
        let (owner, project) = path.split_once('/')?;
        let valid = |part: &str| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        };
        (valid(owner) && valid(project) && project != "." && project != "..")
            .then_some((owner, project.strip_suffix(".git").unwrap_or(project)))
    }
    let Some((url, query)) = source
        .strip_prefix("git+")
        .and_then(|value| value.split_once('?'))
    else {
        return false;
    };
    let Some((revision, resolved)) = query
        .strip_prefix("rev=")
        .and_then(|value| value.split_once('#'))
    else {
        return false;
    };
    if revision.len() != 40
        || !revision.bytes().all(|byte| byte.is_ascii_hexdigit())
        || revision != resolved
    {
        return false;
    }
    match (parts(repository), parts(url)) {
        (Some((workspace, _)), Some((dependency, project))) => {
            workspace.eq_ignore_ascii_case(dependency)
                && FIRST_PARTY_GIT_COMPONENTS
                    .iter()
                    .any(|&(name, repo)| name == package && repo == project)
        }
        _ => false,
    }
}

/// Does the roster describe this root?
#[must_use]
pub fn applies_to(root: &Path) -> bool {
    root.join("crates/aterm-forge/src/provenance.rs").is_file()
}

/// How many roster rows apply to this root, so a log line cannot claim an exemption not in force.
#[must_use]
pub fn roster_len(root: &Path) -> usize {
    if applies_to(root) {
        FIRST_PARTY_VENDORED.len()
    } else {
        0
    }
}

/// Roster rows whose directory does not exist.
#[must_use]
pub fn stale_rows(root: &Path) -> Vec<&'static FirstPartyVendored> {
    FIRST_PARTY_VENDORED
        .iter()
        .filter(|r| !root.join(r.dir).is_dir())
        .collect()
}

/// Consumers whose dependency resolves inside `rel_root`, sorted and deduplicated: each member
/// under `crates/` (by its directory name, paths inherited from the workspace included), and
/// each roster root (by its `vendor/…` path) whose own manifest depends on it while that root is
/// itself consumed. A first-party crate reached only through another one (`trust-sha2` through
/// `trust-build-cpu`) is in the graph; a roster root that only other unconsumed roots reach,
/// a cycle among them included, is not.
#[must_use]
pub fn path_dependants(root: &Path, rel_root: &str) -> Vec<String> {
    path_dependants_outside(root, rel_root, &mut vec![rel_root.to_owned()])
}

fn path_dependants_outside(root: &Path, rel_root: &str, visiting: &mut Vec<String>) -> Vec<String> {
    let target = normalize(&root.join(rel_root));
    let workspace = crate::read_regular_text(root.join("Cargo.toml"))
        .ok()
        .and_then(|text| text.parse::<DocumentMut>().ok());
    let mut out: Vec<String> = Vec::new();
    let Ok(entries) = std::fs::read_dir(root.join("crates")) else {
        return out;
    };
    for entry in entries.flatten() {
        let member = entry.path();
        let manifest = member.join("Cargo.toml");
        let Ok(text) = crate::read_regular_text(&manifest) else {
            continue;
        };
        let Ok(doc) = text.parse::<DocumentMut>() else {
            continue;
        };
        if !dependency_paths(&doc, workspace.as_ref(), root, &member)
            .iter()
            .any(|path| path.starts_with(&target))
        {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        out.push(name);
    }
    for row in FIRST_PARTY_VENDORED {
        if visiting.iter().any(|seen| seen == row.dir) {
            continue;
        }
        let vendored = root.join(row.dir);
        let Some(doc) = crate::read_regular_text(vendored.join("Cargo.toml"))
            .ok()
            .and_then(|text| text.parse::<DocumentMut>().ok())
        else {
            continue;
        };
        // A roster root is excluded from the workspace, so it inherits nothing from it.
        if !dependency_paths(&doc, None, root, &vendored)
            .iter()
            .any(|path| path.starts_with(&target))
        {
            continue;
        }
        visiting.push(row.dir.to_owned());
        let consumed = !path_dependants_outside(root, row.dir, visiting).is_empty();
        visiting.pop();
        if consumed {
            out.push(row.dir.to_owned());
        }
    }
    out.sort();
    out.dedup();
    out
}

fn dependency_paths(
    manifest: &DocumentMut,
    workspace: Option<&DocumentMut>,
    root: &Path,
    member: &Path,
) -> Vec<PathBuf> {
    let kinds = ["dependencies", "dev-dependencies", "build-dependencies"];
    let mut tables: Vec<&dyn TableLike> = kinds
        .iter()
        .filter_map(|key| manifest.get(key).and_then(Item::as_table_like))
        .collect();
    if let Some(targets) = manifest.get("target").and_then(Item::as_table_like) {
        for (_, target) in targets.iter() {
            for kind in kinds {
                if let Some(table) = target.get(kind).and_then(Item::as_table_like) {
                    tables.push(table);
                }
            }
        }
    }
    tables
        .into_iter()
        .flat_map(TableLike::iter)
        .filter_map(|(alias, entry)| {
            let spec = entry.as_table_like()?;
            let (base, path) = if spec.get("workspace").and_then(Item::as_bool) == Some(true) {
                let path = workspace?
                    .get("workspace")?
                    .get("dependencies")?
                    .get(alias)?
                    .get("path")?
                    .as_str()?;
                (root, path)
            } else {
                (member, spec.get("path")?.as_str()?)
            };
            Some(normalize(&base.join(path)))
        })
        .collect()
}

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
#[path = "provenance_tests.rs"]
mod tests;
