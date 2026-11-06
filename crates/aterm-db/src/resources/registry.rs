// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Durable logical request bindings, independent of allocation and policy
//! revisions. This is replay protection, not provider or execution authority.

use super::{Allocation, BoundRequest};
use crate::{
    Error, Result,
    catalog::{Descriptor, Directory},
    crypto::Reader,
    table::{
        Session,
        store::{Receipt, Store},
    },
};
use astream_wire::sync::{Actor, Digest};
use aterm_digest::Sha256;
use aterm_secrets::{client::Paths, commons::bootstrap::Bootstrap};
use std::{collections::BTreeMap, time::Instant};

/// Client identity must be authenticated and retained by the composing broker.
/// Rotating a launch capability must not rotate this producer namespace. A new
/// logical id means new work; construction alone authenticates neither value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RequestKey {
    producer: Digest,
    logical: [u8; 16],
}

impl RequestKey {
    pub fn new(producer: Digest, logical: [u8; 16]) -> Result<Self> {
        let key = Self { producer, logical };
        if producer == [0; 32] || logical == [0; 16] || key.ledger_id() == [0; 16] {
            return Err(Error::new(
                "request needs nonzero producer and logical identities",
            ));
        }
        Ok(key)
    }

    pub const fn producer(self) -> Digest {
        self.producer
    }
    pub const fn logical(self) -> [u8; 16] {
        self.logical
    }

    /// Stable across account, credential, policy, allocation and capability
    /// changes. Hash collisions fail closed at the ledger's operation binding.
    pub fn ledger_id(self) -> [u8; 16] {
        let mut hash = Sha256::new();
        hash.update(b"aterm/commons/logical-request/1\0");
        hash.update(self.producer);
        hash.update(self.logical);
        hash.finalize()[..16].try_into().unwrap()
    }
}

/// Retained metadata only. A claim never authorizes a provider invocation.
#[derive(Clone, Debug)]
pub struct Claim {
    pub key: RequestKey,
    pub writer: Actor,
    pub allocation: Digest,
    pub operation: Digest,
    pub receipt: Receipt,
}

pub struct Claimed {
    pub claim: Claim,
    pub fresh: bool,
}

/// An immutable request-key registry in the default Commons application
/// network. Same-writer restarts can read/retry a claim. A successor must hold
/// ancestor claims; it cannot take over an uncertain provider effect.
pub struct RequestRegistry {
    paths: Paths,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pin {
    Pending(Digest),
    Bound(Digest),
}

impl RequestRegistry {
    pub fn at(paths: Paths) -> Self {
        Self { paths }
    }

    /// Leave the exact durable prefix of an interrupted first publication.
    #[cfg(test)]
    pub(crate) fn test_pending_enrollment(&self, deadline: Instant) -> Result<()> {
        let bootstrap =
            Bootstrap::required_until(&self.paths, deadline).map_err(crate::table::failure)?;
        let session = Session::open_until(&bootstrap, deadline)?;
        let descriptor = descriptor(&bootstrap)?;
        write_pending(&self.paths, descriptor.scope.digest())?;
        Directory::new(&self.paths, bootstrap.trust()).publish(&session, &descriptor)?;
        Ok(())
    }

    /// Bind before resource admission. The first claim enrolls this fixed table;
    /// established missing/damaged custody refuses instead of resetting it.
    /// Exact retries keep their first binding and receipt. This reserves no
    /// units and creates no invocation permit, even when `fresh` is true.
    pub fn bind_until(
        &self,
        key: RequestKey,
        allocation: &Allocation,
        request: &BoundRequest,
        deadline: Instant,
    ) -> Result<Claimed> {
        if key.ledger_id() != request.id() {
            return Err(Error::new("request id differs from its producer namespace"));
        }
        let operation = request.operation(allocation)?;
        let origin = crate::generation::logical_paths_until(&self.paths, deadline)?;
        crate::documents::application::with_store_guarded_until(
            &origin,
            true,
            |bootstrap| {
                if bootstrap.trust() != allocation.trust() {
                    return Err(Error::new("request allocation has another Commons root"));
                }
                descriptor(bootstrap)
            },
            deadline,
            |session, directory, descriptor| {
                session.writer()?;
                let pin = read_pin(session.paths(), descriptor.scope.digest())?;
                if let Some(Pin::Bound(pin)) = pin {
                    let primary = directory.observe_store(session, descriptor.scope)?;
                    if primary.primary_custody_id(session)? != pin {
                        return Err(Error::new(
                            "request registry custody differs from its retained pin",
                        ));
                    }
                } else if pin.is_none()
                    && directory
                        .snapshot_existing_if_exists(session)?
                        .is_some_and(|snapshot| {
                            snapshot
                                .entries
                                .get(&descriptor.scope.digest())
                                .is_some_and(|entry| {
                                    entry
                                        .declarations
                                        .iter()
                                        .any(|d| d.actor == session.actor())
                                })
                        })
                {
                    return Err(Error::new(
                        "request registry has prior local publication but no custody pin",
                    ));
                }
                if pin.is_none() {
                    write_pending(session.paths(), descriptor.scope.digest())?;
                }
                Ok(())
            },
            |session, store, scope| {
                let directory = Directory::new(session.paths(), scope.trust());
                if let Some(recovery) = directory
                    .recovery()
                    .observe_store_if_exists(session, scope)?
                {
                    store.reconcile_recovery(session, recovery)?;
                }
                let actors = Self::actors(&origin, session, deadline)?;
                let claims = read(session, store, &actors)?;
                let pinned = read_pin(session.paths(), scope.digest())?
                    .ok_or_else(|| Error::new("request registry pin disappeared"))?;
                let custody = store.primary_custody_id(session)?;
                if matches!(pinned, Pin::Bound(pin) if pin != custody) {
                    return Err(Error::new(
                        "request registry custody differs from its retained pin",
                    ));
                }
                if matches!(pinned, Pin::Pending(_))
                    && claims.keys().any(|(actor, _)| *actor == session.actor())
                {
                    return Err(Error::new(
                        "request registry has local claims but no custody pin",
                    ));
                }
                if let Some(claim) = selected(&claims, &actors, key)? {
                    if claim.writer != session.actor() {
                        return Err(Error::new(
                            "request belongs to a predecessor writer; no takeover",
                        ));
                    }
                    if claim.allocation != allocation.id() || claim.operation != operation {
                        return Err(Error::new(
                            "logical request already has a different binding",
                        ));
                    }
                    return Ok(Claimed {
                        claim: claim.clone(),
                        fresh: false,
                    });
                }
                let mut bytes = [0u8; 117];
                bytes[..5].copy_from_slice(b"ARRQ1");
                bytes[5..37].copy_from_slice(&key.producer);
                bytes[37..53].copy_from_slice(&key.logical);
                bytes[53..85].copy_from_slice(&allocation.id());
                bytes[85..117].copy_from_slice(&operation);
                if let Pin::Pending(nonce) = pinned {
                    finish_pending(session.paths(), scope.digest(), nonce, custody)?;
                }
                let receipt = store.commit_liability(session, &bytes)?;
                Ok(Claimed {
                    claim: Claim {
                        key,
                        writer: session.actor(),
                        allocation: allocation.id(),
                        operation,
                        receipt,
                    },
                    fresh: true,
                })
            },
        )
    }

    /// Read retained bindings across this installation's certified lineage.
    /// An absent claim is no execution permission. Reading initializes nothing.
    pub fn lookup_until(&self, key: RequestKey, deadline: Instant) -> Result<Option<Claim>> {
        let origin = crate::generation::observed_logical_paths_until(&self.paths, deadline)?;
        crate::documents::application::observe_store_until(
            &origin,
            descriptor,
            deadline,
            |session, store, scope| {
                let actors = Self::actors(&origin, session, deadline)?;
                let claims = read(session, store, &actors)?;
                let found = selected(&claims, &actors, key)?.cloned();
                match read_pin(session.paths(), scope.digest())? {
                    Some(Pin::Bound(pin)) if pin == store.primary_custody_id(session)? => Ok(found),
                    None | Some(Pin::Pending(_)) if found.is_some() => Ok(found), // Positive evidence only, never absence.
                    _ => Err(Error::new(
                        "request registry custody pin is missing or changed",
                    )),
                }
            },
        )
    }

    fn actors(paths: &Paths, session: &Session<'_>, deadline: Instant) -> Result<Vec<Actor>> {
        Ok(crate::generation::lineage_in(paths, session, deadline)?
            .iter()
            .map(|binding| binding.actor())
            .collect())
    }
}

fn pin_path(paths: &Paths) -> std::path::PathBuf {
    paths.dir.join("commons/request-registry-pin")
}

fn read_pin(paths: &Paths, scope: Digest) -> Result<Option<Pin>> {
    let Some(pin) =
        aterm_secrets::fsx::read_private(&pin_path(paths), 69).map_err(crate::table::failure)?
    else {
        return Ok(None);
    };
    let bytes = pin.as_slice();
    if bytes.len() != 69 || bytes[5..37] != scope || bytes[37..] == [0; 32] {
        return Err(Error::new("invalid request registry custody pin"));
    }
    let value = bytes[37..].try_into().unwrap();
    match &bytes[..5] {
        b"ARRP0" => Ok(Some(Pin::Pending(value))),
        b"ARRP1" => Ok(Some(Pin::Bound(value))),
        _ => Err(Error::new("invalid request registry custody pin")),
    }
}

fn write_pending(paths: &Paths, scope: Digest) -> Result<()> {
    let path = pin_path(paths);
    let parent = path.parent().unwrap();
    aterm_journal::create_private_dir_durable(parent)?;
    let mut bytes = [0u8; 69];
    bytes[..5].copy_from_slice(b"ARRP0");
    bytes[5..37].copy_from_slice(&scope);
    bytes[37..69].copy_from_slice(&crate::crypto::random::<32>()?);
    aterm_secrets::fsx::write_new(&path, &bytes, 0o600).map_err(crate::table::failure)?;
    aterm_durable::durable_dir(parent)?;
    Ok(())
}

fn finish_pending(paths: &Paths, scope: Digest, nonce: Digest, custody: Digest) -> Result<()> {
    if read_pin(paths, scope)? != Some(Pin::Pending(nonce)) {
        return Err(Error::new("request registry initialization changed"));
    }
    let path = pin_path(paths);
    let mut bytes = [0u8; 69];
    bytes[..5].copy_from_slice(b"ARRP1");
    bytes[5..37].copy_from_slice(&scope);
    bytes[37..69].copy_from_slice(&custody);
    aterm_secrets::fsx::write_atomic(&path, &bytes, 0o600).map_err(crate::table::failure)?;
    aterm_durable::durable_dir(path.parent().unwrap())?;
    Ok(())
}

fn descriptor(bootstrap: &Bootstrap) -> Result<Descriptor> {
    Descriptor::application(bootstrap.trust(), "resources", "requests", 1)
}

fn read(
    session: &Session<'_>,
    store: &Store,
    actors: &[Actor],
) -> Result<BTreeMap<(Actor, RequestKey), Claim>> {
    let mut claims = BTreeMap::new();
    for record in store.liability_evidence(session)? {
        // Other writers' application payloads cannot claim or corrupt this
        // lineage's request namespace. Foundation evidence is still validated.
        if !actors.contains(&record.header().actor) {
            continue;
        }
        let mut bytes = Reader::new(record.payload());
        if bytes.take(5)? != b"ARRQ1" {
            return Err(Error::new("unknown logical request record"));
        }
        let key = RequestKey::new(bytes.array()?, bytes.array()?)?;
        let allocation = bytes.array()?;
        let operation = bytes.array()?;
        bytes.end()?;
        if allocation == [0; 32] || operation == [0; 32] {
            return Err(Error::new("invalid logical request binding"));
        }
        let writer = record.header().actor;
        let claim = Claim {
            key,
            writer,
            allocation,
            operation,
            receipt: store
                .receipt_unchecked(&record.header().id)
                .ok_or_else(|| Error::new("request binding receipt is missing"))?,
        };
        if claims.insert((writer, key), claim).is_some() {
            return Err(Error::new("writer has duplicate logical request claims"));
        }
    }
    Ok(claims)
}

fn selected<'a>(
    claims: &'a BTreeMap<(Actor, RequestKey), Claim>,
    actors: &[Actor],
    key: RequestKey,
) -> Result<Option<&'a Claim>> {
    let mut found = None;
    for actor in actors {
        if let Some(claim) = claims.get(&(*actor, key)) {
            if found.is_some() {
                return Err(Error::new(
                    "logical request appears in multiple writer generations",
                ));
            }
            found = Some(claim);
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests;
