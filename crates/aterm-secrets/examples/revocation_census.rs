// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Count the age recipients that two or more Macs' seats share, and what P1's revocation
//! (`aterm_mesh_core::revocation`) decides of them, from a copy of a membership-log `records`
//! file named on the command line. Read-only: no network, no identity, no vault, no write. It
//! prints counts only.
//!
//! A seat row is a founder's binding or an `Admit` whose signer is not its entrant and that no
//! quorum voided: what `bound_seats` can be at most (the authorization, cut and binding filters
//! the evaluator applies are not public). A recipient is shared when rows of two or more actors
//! bind it. Of those it reports how many the log's evaluation revokes, how many a current member
//! holds, and how many a current member holds while revoked: the co-holders P1's rule (ii) takes
//! out until they re-key.

use aterm_secrets::membership_log::{Body, Store};
use std::collections::{BTreeMap, BTreeSet};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

fn main() -> Res<()> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: revocation_census <records file>")?;
    let bytes = std::fs::read(&path)?;
    let store = Store::recover("alab", &bytes)?;
    let log = store.log().ok_or("no log")?;
    if log.waiting() != 0 {
        return Err("records wait for missing parents".into());
    }
    let membership = log.membership();
    let voided = membership.voided();
    let mut rows: BTreeMap<_, BTreeSet<_>> = BTreeMap::new();
    let mut row_count = 0_usize;
    let mut removes = 0_usize;
    for record in log.records() {
        match record.body() {
            Body::Genesis { founders, .. } => {
                for founder in founders {
                    if let Some(binding) = &founder.binding {
                        rows.entry(binding.recipient())
                            .or_default()
                            .insert(binding.actor());
                        row_count += 1;
                    }
                }
            }
            Body::Admit { entrant, .. }
                if !voided.contains(record.id()) && entrant.member() != *record.actor() =>
            {
                let binding = entrant.request.binding();
                rows.entry(binding.recipient())
                    .or_default()
                    .insert(binding.actor());
                row_count += 1;
            }
            Body::Remove { .. } if !voided.contains(record.id()) => removes += 1,
            _ => {}
        }
    }
    let shared: Vec<_> = rows
        .iter()
        .filter(|(_, actors)| actors.len() > 1)
        .map(|(recipient, _)| *recipient)
        .collect();
    let revoked = membership.removed_recipients();
    let held: BTreeSet<_> = membership
        .members()
        .values()
        .map(|m| m.binding.recipient())
        .collect();
    let count = |keep: &dyn Fn(&_) -> bool| shared.iter().filter(|r| keep(r)).count();
    println!("records {}", log.records().count());
    println!("seat rows {row_count}, recipients {}", rows.len());
    println!("removes (not voided) {removes}");
    println!("members {}", membership.members().len());
    println!("revoked recipients {}", revoked.len());
    println!(
        "members holding a revoked recipient {}",
        held.iter().filter(|r| revoked.contains(r)).count()
    );
    println!("shared recipients {}", shared.len());
    println!("shared and revoked {}", count(&|r| revoked.contains(r)));
    println!(
        "shared and held by a member {}",
        count(&|r| held.contains(r))
    );
    println!(
        "shared, held by a member, and revoked {}",
        count(&|r| held.contains(r) && revoked.contains(r))
    );
    Ok(())
}
