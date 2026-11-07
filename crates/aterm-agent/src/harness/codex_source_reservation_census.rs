// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Bounded, revision-bound census observations. Pages convey metadata only;
//! neither a cursor nor a revision can authorize source or destination work.

use super::*;

const PAGE_SIZE: usize = 16;

pub struct CensusCursor<'a> {
    pub revision: &'a str,
    pub after: &'a str,
}

pub struct CensusPage {
    pub revision: String,
    pub rows: Vec<CensusMember>,
    pub next: Option<String>,
}

fn condition(condition: CensusCondition) -> u8 {
    match condition {
        CensusCondition::Unresolved => 0,
        CensusCondition::Unknown => 1,
        CensusCondition::Manager => 2,
        CensusCondition::Reader => 3,
        CensusCondition::Quiescent => 4,
        CensusCondition::Retired => 5,
        CensusCondition::Disjoint => 6,
    }
}

fn revision(cell: &Cell, rows: &[CensusMember]) -> Result<String, Error> {
    let observations = rows
        .iter()
        .map(|row| {
            (
                &row.member,
                &row.registration,
                condition(row.condition),
                row.credential_generation,
                &row.digest,
                row.auth
                    .map(|auth| (auth.generation, auth.owner, auth.source, auth.login)),
            )
        })
        .collect::<Vec<_>>();
    let bytes = aterm_json::to_vec(&(
        "aterm/codex-reservation-census-page/v1",
        cell.target.caller(),
        cell.target.operation(),
        &cell.capability,
        &cell.issued,
        observations,
    ))
    .map_err(|_| Error::Unavailable)?;
    Ok(aterm_codec::hex::encode_lower(
        &aterm_digest::Sha256::digest(bytes),
    ))
}

fn page(
    revision: String,
    rows: Vec<CensusMember>,
    cursor: Option<CensusCursor<'_>>,
) -> Result<CensusPage, Error> {
    let start = match cursor {
        None => 0,
        Some(cursor) => {
            if !hex(cursor.revision, 64) || cursor.revision != revision || !hex(cursor.after, 32) {
                return Err(Error::Stale);
            }
            rows.iter()
                .position(|row| row.member == cursor.after)
                .ok_or(Error::Stale)?
                + 1
        }
    };
    let remaining = rows.len().saturating_sub(start);
    let rows = rows
        .into_iter()
        .skip(start)
        .take(PAGE_SIZE)
        .collect::<Vec<_>>();
    let next = (remaining > PAGE_SIZE)
        .then(|| rows.last().map(|row| row.member.clone()))
        .flatten();
    Ok(CensusPage {
        revision,
        rows,
        next,
    })
}

impl Reservation {
    /// Capture the entire bounded cohort and its observations under the same
    /// State/Cell locks before selecting a page. A changed Ready, registration,
    /// member, or quiescence report invalidates every old continuation cursor.
    pub fn census_page(
        &self,
        peer: &Peer,
        cursor: Option<super::CensusCursor<'_>>,
    ) -> Result<super::CensusPage, Error> {
        let state = self.state.upgrade().ok_or(Error::Stale)?;
        let state = lock(&state)?;
        let cell = checked_cell(&state, &self.cell, peer)?;
        let rows = cell
            .issued
            .iter()
            .map(|(member, operation)| row(&state, &cell, member, operation))
            .collect::<Result<Vec<_>, _>>()?;
        page(revision(&cell, &rows)?, rows, cursor)
    }
}
