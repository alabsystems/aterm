// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Member-signed table cutoffs (docs/DESIGN-alab-one-mesh-2026-11-05.md §4.10, §11 B11): a
//! removal cuts tables too. Before its `Remove`, the remover signs, in each lab table it holds, a
//! table operation of its own naming the removed writer's last operation held there, the table
//! analog of the log cutoff. A reader counts the removed writer's operations only inside that
//! cutoff's past, and only once its log shows that writer removed, so a cutoff signed before a
//! refused or failed `Remove` changes nothing. Cutoffs from several removers intersect, as the
//! control history's `Fact::Cutoff`s do, through the same walk
//! (`aterm_secrets::commons::constraints::Constraints::catalog_with`).
//!
//! The cutoff rides the table itself, so it replicates as every operation does, and needs no log
//! record kind, which R1 readers refuse. Its payload is one no application decodes (each reader
//! skips a payload it cannot read), and [`super::Store::catalog`] leaves it out.

use astream_wire::sync::Actor;
use aterm_secrets::commons::constraints::Frontier;

/// The payload's tag: no application payload (JSON commands, catalog descriptors) starts so.
const MAGIC: &[u8; 7] = b"ATCUT1\0";
const LEN: usize = MAGIC.len() + 48 + 8 + 32;

/// One removed writer's cutoff in one table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cut {
    /// The removed writer.
    pub(crate) actor: Actor,
    /// Its last operation the signer held in this table; `None` withholds all of them.
    pub(crate) through: Option<Frontier>,
}

/// The cutoff's payload.
pub(crate) fn encode(cut: &Cut) -> Vec<u8> {
    let mut payload = Vec::with_capacity(LEN);
    payload.extend_from_slice(MAGIC);
    payload.extend_from_slice(&cut.actor);
    let frontier = cut.through.unwrap_or(Frontier {
        sequence: 0,
        digest: [0; 32],
    });
    payload.extend_from_slice(&frontier.sequence.to_le_bytes());
    payload.extend_from_slice(&frontier.digest);
    payload
}

/// The cutoff `payload` carries, if it is one.
pub(crate) fn decode(payload: &[u8]) -> Option<Cut> {
    if payload.len() != LEN || !payload.starts_with(MAGIC) {
        return None;
    }
    let rest = &payload[MAGIC.len()..];
    let actor: Actor = rest[..48].try_into().ok()?;
    let sequence = u64::from_le_bytes(rest[48..56].try_into().ok()?);
    let digest: [u8; 32] = rest[56..].try_into().ok()?;
    let through = match (sequence, digest == [0; 32]) {
        (0, true) => None,
        (0, false) | (_, true) => return None,
        _ => Some(Frontier { sequence, digest }),
    };
    (actor[..32] != [0; 32]).then_some(Cut { actor, through })
}

/// How [`super::Store::sign_cutoff`] signs.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Signing {
    /// Before the `Remove`: at the removed writer's last operation held now.
    Fresh,
    /// The remover's repair, after the `Remove`: at `saved` (the frontier read before the
    /// `Remove`, [`save`]) unless this Mac's newest cutoff of the writer names it already; with
    /// none saved, only where this Mac holds no cutoff of the writer yet.
    Repair { saved: Option<Option<Frontier>> },
    /// A share of another member's removal: only replace a cutoff of this Mac's own an earlier,
    /// refused attempt left, never sign a first one.
    Supersede,
}

/// Where a cutoff this Mac could not sign before its `Remove` keeps the frontier it read then,
/// one file per table and removed writer, so the repair signs that frontier and not one that
/// takes in the removed Mac's later operations.
#[cfg(unix)]
fn saved_path(
    paths: &aterm_secrets::client::Paths,
    scope: crate::table::Scope,
    member: &Actor,
) -> std::path::PathBuf {
    paths.dir.join("commons/table-cutoffs").join(format!(
        "{}-{}",
        aterm_codec::hex::encode_lower(&scope.digest()),
        aterm_codec::hex::encode_lower(member)
    ))
}

/// The frontier a removal read and could not sign in `scope` ([`saved_path`]), if any.
#[cfg(unix)]
fn saved(
    paths: &aterm_secrets::client::Paths,
    scope: crate::table::Scope,
    member: &Actor,
) -> Option<Option<Frontier>> {
    let bytes = std::fs::read(saved_path(paths, scope, member)).ok()?;
    decode(&bytes)
        .filter(|cut| cut.actor == *member)
        .map(|cut| cut.through)
}

/// Keep `through` for the repair of `member`'s cutoff in `scope`.
#[cfg(unix)]
fn save(
    paths: &aterm_secrets::client::Paths,
    scope: crate::table::Scope,
    member: &Actor,
    through: Option<Frontier>,
) -> bool {
    let path = saved_path(paths, scope, member);
    path.parent()
        .is_some_and(|dir| aterm_secrets::fsx::ensure_private_dir(dir).is_ok())
        && aterm_secrets::fsx::write_atomic(
            &path,
            &encode(&Cut {
                actor: *member,
                through,
            }),
            0o600,
        )
        .is_ok()
}

/// The one entry point every removal path takes (§4.10, §11 B11), for each lab table this Mac
/// holds, its catalog's and each declared one's: before the `Remove`
/// ([`aterm_secrets::client::CutPass::BeforeRemove`]) it signs a cutoff of `member`
/// ([`Signing::Fresh`]); a table it cannot cut now (a busy writer, read-only custody) keeps the
/// frontier it read then for the repair (as does every table, while another local writer holds
/// the journal), and a removal never waits on it. As the remover's node
/// service starts ([`aterm_secrets::client::CutPass::Repair`],
/// `aterm_secrets::client::cut_own_removals`) it signs each cutoff still missing, at the
/// frontier kept for it where there is one ([`Signing::Repair`]). On a share of another
/// member's removal ([`aterm_secrets::client::CutPass::Share`]) it only replaces a cutoff of
/// this Mac's own an earlier attempt left ([`Signing::Supersede`]). How many it signed.
#[cfg(unix)]
pub fn cut_removed_writer(
    paths: &aterm_secrets::client::Paths,
    member: &aterm_secrets::membership_log::MemberId,
    pass: aterm_secrets::client::CutPass,
) -> usize {
    use aterm_secrets::client::CutPass;
    let Ok(Some(bootstrap)) = aterm_secrets::commons::bootstrap::Bootstrap::cached(paths) else {
        return 0;
    };
    let directory = crate::catalog::Directory::new(paths, bootstrap.trust());
    let keep = |session: &crate::table::Session<'_>, scope| {
        if let Ok(store) = directory.observe_store(session, scope) {
            save(paths, scope, member, store.last_of(*member));
        }
    };
    let session = match crate::table::Session::open(&bootstrap) {
        Ok(session) => session,
        Err(_) => {
            // Another local writer holds the journal: read each table's frontier beside it and
            // keep it for the repair.
            if pass == CutPass::BeforeRemove
                && let Ok(session) = crate::table::Session::observe(&bootstrap)
            {
                for scope in held_scopes(&directory, &session) {
                    keep(&session, scope);
                }
            }
            return 0;
        }
    };
    let mut signed = 0;
    for scope in held_scopes(&directory, &session) {
        let signing = match pass {
            CutPass::BeforeRemove => Signing::Fresh,
            CutPass::Share => Signing::Supersede,
            CutPass::Repair => Signing::Repair {
                saved: saved(paths, scope, member),
            },
        };
        match directory
            .open_store(&session, scope)
            .and_then(|mut store| store.sign_cutoff(&session, *member, signing))
        {
            Ok(fresh) => {
                signed += usize::from(fresh);
                if pass != CutPass::Share {
                    let _ = std::fs::remove_file(saved_path(paths, scope, member));
                }
            }
            Err(_) if pass == CutPass::BeforeRemove => keep(&session, scope),
            Err(_) => {}
        }
    }
    signed
}

/// The lab tables this Mac holds: its catalog's and each declared one's with local storage.
#[cfg(unix)]
fn held_scopes(
    directory: &crate::catalog::Directory,
    session: &crate::table::Session<'_>,
) -> Vec<crate::table::Scope> {
    let mut scopes = vec![directory.catalog_scope()];
    if let Ok(snapshot) = directory.snapshot_existing(session) {
        scopes.extend(snapshot.entries.values().map(|entry| entry.scope));
    }
    scopes.retain(|scope| directory.path(*scope).is_ok_and(|path| path.is_dir()));
    scopes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cutoff_round_trips_and_nothing_else_decodes_as_one() {
        let actor = [7; 48];
        for through in [
            None,
            Some(Frontier {
                sequence: 3,
                digest: [9; 32],
            }),
        ] {
            let cut = Cut { actor, through };
            assert_eq!(decode(&encode(&cut)), Some(cut));
        }
        let mut torn = encode(&Cut {
            actor,
            through: None,
        });
        torn[LEN - 1] = 1;
        assert_eq!(decode(&torn), None, "a zero sequence names no digest");
        assert_eq!(decode(br#"{"v":1}"#), None);
        assert_eq!(decode(&torn[..LEN - 1]), None);
    }
}
