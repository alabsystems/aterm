// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Durable Commons foundation for an existing vault. A retained descriptor is
//! retried byte-for-byte; an already published root is finished, never replaced.

use super::*;
use crate::commons::{bootstrap::Descriptor, node::Node};

const PREPARED: &str = "commons-foundation";

/// Public outcome of a Commons foundation or its idempotent retry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommonsInit {
    /// The network whose founder history is installed locally.
    pub network: [u8; 16],
    /// The independently retained Commons authority.
    pub authority: [u8; 32],
    /// The signed genesis shared by every replica.
    pub genesis: [u8; 32],
    /// Vault machines eligible to see the control descriptor and request membership.
    pub eligible: usize,
    /// Whether this call published the foundation.
    pub published: bool,
    /// Vault machines, by name, this call approved for Commons (`Manual`): with the
    /// foundation, or on a root an earlier aterm founded, in a publication of their own.
    pub approved: Vec<String>,
}

fn prepared(paths: &Paths) -> crate::Result<Option<Descriptor>> {
    let Some(bytes) = fsx::read_private(&paths.dir.join(PREPARED), envelope::MAX_ENVELOPE)? else {
        return Ok(None);
    };
    Descriptor::decode(bytes.as_slice()).map(Some)
}

fn refuse_unrelated_history(paths: &Paths) -> crate::Result<()> {
    let commons = paths.dir.join("commons");
    match fsx::kind(&commons)? {
        fsx::Kind::Missing => return Ok(()),
        fsx::Kind::Dir => {}
        fsx::Kind::Symlink | fsx::Kind::File | fsx::Kind::Other => {
            return Err(Error::new(
                "Commons foundation cannot replace the existing commons path",
            ));
        }
    }
    let entries = std::fs::read_dir(&commons)
        .map_err(|error| Error::new(format!("{}: {error}", commons.display())))?;
    for entry in entries {
        let entry = entry.map_err(|error| Error::new(format!("{}: {error}", commons.display())))?;
        if !matches!(
            entry.file_name().to_str(),
            Some("runtime.lock" | "device.age" | "device.claim")
        ) {
            return Err(Error::new(format!(
                "Commons foundation cannot replace existing local history: {}",
                entry.path().display()
            )));
        }
    }
    Ok(())
}

fn audience(admin: &Admin) -> Audience {
    let mut hosts = admin
        .doc
        .machines()
        .into_iter()
        .map(|machine| machine.name)
        .collect::<BTreeSet<_>>();
    hosts.insert(admin.config.host.clone());
    Audience::Hosts(hosts.into_iter().collect())
}

/// The machines `doc`'s control entry shares the root with that hold no admission
/// decision, which the admin founding Commons approves (`Manual`) in its own publication.
/// Members let in before decisions were recorded, or by an aterm that recorded none,
/// could otherwise never enroll (owner, 2026-10-27: "BY DEFAULT ON all machines running
/// aterm find each other"). A recorded decision is never replaced, and a machine outside
/// the entry's audience is never approved, since an approval would bring it in.
fn vouched(admin: &Admin, doc: &Document) -> Vec<text::Machine> {
    let Some(Audience::Hosts(hosts)) = doc.commons_audience() else {
        return Vec::new();
    };
    doc.machines()
        .into_iter()
        .filter(|machine| hosts.contains(&machine.name))
        .filter(|machine| {
            Recipient::parse(&machine.recipient)
                .is_ok_and(|key| admin.admissions.evidence(&key).is_none())
        })
        .collect()
}

/// Publish `doc` with this admin's approval of `machines`, naming them.
fn approve(
    paths: &Paths,
    admin: &Admin,
    doc: &Document,
    machines: Vec<text::Machine>,
) -> crate::Result<Vec<String>> {
    let recipients = machines
        .iter()
        .map(|machine| machine.recipient.clone())
        .collect::<Vec<_>>();
    let _ = admin_publish_approved(paths, admin, doc, &recipients)?;
    Ok(machines.into_iter().map(|machine| machine.name).collect())
}

fn prepare(paths: &Paths, admin: &Admin) -> crate::Result<Descriptor> {
    refuse_unrelated_history(paths)?;
    if let Some(descriptor) = prepared(paths)? {
        if descriptor.trust().authority != admin.commons_authority()? {
            return Err(Error::new(
                "retained Commons foundation belongs to another authority",
            ));
        }
        return Ok(descriptor);
    }
    let descriptor = Descriptor::from_genesis(Node::foundation(admin.commons_authority()?)?)?;
    let mut doc = admin.doc.clone();
    let eligible = audience(admin);
    doc.set_entry(&eligible, crate::commons::ENTRY, &descriptor.bytes())
        .map_err(|error| Error::new(error.to_string()))?;
    let descriptor = creation::enroll_founder(
        paths,
        &doc,
        &admin.config.host,
        &admin.commons_seed,
        descriptor,
    )?;
    aterm_journal::write_private_durable(&paths.dir, PREPARED, &descriptor.bytes())?;
    Ok(descriptor)
}

fn finish(paths: &Paths, descriptor: &Descriptor, published: bool) -> crate::Result<CommonsInit> {
    let _ = sync(paths, None)?;
    let bootstrap = crate::commons::bootstrap::Bootstrap::cached(paths)?
        .ok_or_else(|| Error::new("published Commons control is not visible after sync"))?;
    if bootstrap.trust() != descriptor.trust() {
        return Err(Error::new(
            "installed Commons history belongs to another foundation",
        ));
    }
    bootstrap.access()?.ok_or_else(|| {
        Error::new("Commons foundation is published but founder membership is not ready")
    })?;
    let eligible = match admin_open(paths)?.doc.commons_audience() {
        Some(Audience::Hosts(hosts)) => hosts.len(),
        _ => 0,
    };
    Ok(CommonsInit {
        network: descriptor.trust().network,
        authority: descriptor.trust().authority,
        genesis: descriptor.trust().genesis,
        eligible,
        published,
        approved: Vec::new(),
    })
}

/// Initialize Commons on an existing vault, or finish an earlier uncertain attempt.
/// Existing roots and local history are never replaced. All current vault machines
/// receive the control descriptor, and the same publication records this admin's approval
/// of each one with no admission decision (`vouched`). On a published root an earlier aterm
/// founded, which recorded no decisions, a call that finds the root ready records it for
/// each machine in the root's audience still without one; on a root this build founded it
/// approves nothing. Membership still requires the normal proofs.
///
/// # Errors
/// The caller is not an admin, the vault is unbound or has an opaque control entry,
/// unrelated local Commons history exists, the retained descriptor is for another
/// authority, or publication/bootstrap fails.
pub fn admin_commons_init(paths: &Paths) -> crate::Result<CommonsInit> {
    let admin = admin_open(paths)?;
    let retained = prepared(paths)?;
    if let Some(descriptor) = admin_keys::control_descriptor(&admin.doc)? {
        if retained
            .as_ref()
            .is_some_and(|old| old.trust() != descriptor.trust())
        {
            return Err(Error::new(
                "published Commons foundation differs from the retained attempt",
            ));
        }
        if descriptor.trust().authority != admin.commons_authority()? {
            return Err(Error::new(
                "published Commons foundation has no matching authority custody",
            ));
        }
        // Readiness first, so a run that fails has changed nothing.
        let done = finish(paths, &descriptor, false)?;
        // After this build's founding every machine it shares the root with holds a decision,
        // this admin included (`audience` names it). A root whose admin holds none was founded
        // by an earlier aterm, which recorded none; on any other root an undecided machine in
        // the audience came by an edit that recorded no decision, which never admits it.
        let admin = admin_open(paths)?;
        let own = machine_identity(paths)?.recipient();
        if admin.admissions.evidence(&own).is_some() {
            return Ok(done);
        }
        let vouched = vouched(&admin, &admin.doc);
        if vouched.is_empty() {
            return Ok(done);
        }
        let approved = approve(paths, &admin, &admin.doc, vouched)?;
        return Ok(CommonsInit { approved, ..done });
    }
    if admin.doc.commons_audience().is_some() {
        return Err(Error::new(
            "Commons control entry is opaque; refusing to replace it",
        ));
    }
    if !admin.commons_bound {
        return Err(Error::new(
            "this vault has no bound Commons authority for foundation",
        ));
    }
    let descriptor = prepare(paths, &admin)?;
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &audience(&admin),
        crate::commons::ENTRY,
        &descriptor.bytes(),
    )
    .map_err(|error| Error::new(error.to_string()))?;
    let approved = approve(paths, &admin, &doc, vouched(&admin, &doc))?;
    Ok(CommonsInit {
        approved,
        ..finish(paths, &descriptor, true)?
    })
}

#[cfg(test)]
mod tests;
