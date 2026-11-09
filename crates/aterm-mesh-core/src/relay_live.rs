// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! P5b, the relay's liveness (step PF5b): the decisions by which a relay's push edge and its
//! flood bring every record of a current seat to every current seat, and Inventory repairs what
//! the edge let go (one-mesh §4.6, "No anti-entropy timer"; §11 B7b;
//! `docs/design/one-mesh-proofs/P5-relay.md`).
//!
//! A relay that starts again reads the cursor it kept for its neighbour ([`cursor`]): the same log
//! resumes, a later generation its lineage carries resumes after the newest rows, anything else
//! starts at the head. The start asks the exchange the cursor cannot carry ([`start_repair`]): none
//! for a kept cursor, a lineage exchange for a moved one, a new link's otherwise. A gap notice (a
//! retention cut below the neighbour's cursor) asks one too, unless `NO_INVENTORY_ON_GAP`
//! ([`requests`]); an asked exchange runs unless the pair's last one moved nothing and this Mac's
//! summary has not changed since ([`runs`]). A start that asked while this Mac's ingest had not yet
//! passed the last table record its log held asks once more when it does ([`asks_again`]): the
//! first exchange can run before this Mac's stores hold what its log held, and move nothing
//! (finding H2).
//!
//! Each decision is a scalar `contract!` kernel stating `ensures result == spec_<name>(..) by
//! <name>_is_exact` for a definition of `relay_live_island.rs`. That island (every Trust build)
//! proves, for any number of Macs on any overlay: an exchange this Mac remembers as quiet is one
//! whose neighbour holds what it holds (`quiet_safe`); with nothing let go, the edge and the flood
//! bring the record to every seat the origin reaches (`flood_without_let_go`); a record an edge owes
//! reaches the neighbour's stores, by the push or by Inventory after a cut, a lost log or a new link
//! (`the_edge_is_repaired`); and with the digest trigger, every reachable seat gets it
//! (`every_seat_gets_the_record`); each under its named fairness premises, with no premise on the
//! order of a start and the ingest, and runs by `rfl` show
//! each kernel decision and each premise is needed. `relay_live_binding_island.rs` proves each
//! compiled kernel equal to its spec and closes the theorems for the compiled kernels; the answer
//! sheet `relay_live::tests` recomputes holds the Rust to the island until TG12.

/// The cursor codes [`cursor`] returns; aterm-mesh's `relay::Cursor` takes them as its
/// discriminants. Gone: none, too old, or another log.
pub const GONE: u8 = 0;
/// The same log: resume.
pub const KEPT: u8 = 1;
/// A later generation of the same log, carried by its lineage.
pub const MOVED: u8 = 2;

/// The exchange codes [`start_repair`] returns. None: the cursor carries everything.
pub const ASK_NONE: u8 = 0;
/// A lineage exchange: the later generation may have lost the last records it held.
pub const ASK_LINEAGE: u8 = 1;
/// A new link's exchange: what lies before the head the relay starts at.
pub const ASK_NEW_LINK: u8 = 2;

contract! {
    /// The cursor a relay kept for its neighbour, from whether it has an entry, the entry is
    /// within `CURSOR_KEPT`, the neighbour's log is the one the entry names, and its lineage
    /// continues that one: [`KEPT`], [`MOVED`] or [`GONE`].
    pub fn cursor(has: bool, fresh: bool, same: bool, continues: bool) -> u8
    clauses [ensures result == spec_cursor(has, fresh, same, continues) by cursor_is_exact]
    { if has && fresh { if same { 1 } else if continues { 2 } else { 0 } } else { 0 } }
}
contract! {
    /// The exchange a relay starting at cursor code `cursor` asks: [`ASK_NONE`] for [`KEPT`],
    /// [`ASK_LINEAGE`] for [`MOVED`], [`ASK_NEW_LINK`] for [`GONE`] and any other code.
    pub fn start_repair(cursor: u8) -> u8
    clauses [ensures result == spec_start_repair(cursor) by start_repair_is_exact]
    { if cursor == 1 { 0 } else if cursor == 2 { 1 } else { 2 } }
}
contract! {
    /// A request for an exchange is queued, unless it is a gap notice under
    /// `NO_INVENTORY_ON_GAP`.
    pub fn requests(gap: bool, no_gap: bool) -> bool
    clauses [ensures result == spec_requests(gap, no_gap) by requests_is_exact]
    { !(gap && no_gap) }
}
contract! {
    /// A queued exchange runs, unless this Mac remembers the pair's last exchange as one that
    /// moved nothing under the summary it holds now.
    pub fn runs(remembers: bool, quiet_same: bool) -> bool
    clauses [ensures result == spec_runs(remembers, quiet_same) by runs_is_exact]
    { !(remembers && quiet_same) }
}
contract! {
    /// A relay start that `asked` an exchange asks it once more when this Mac's ingest passes
    /// the last table record its log held at the start, unless the ingest had `ingested` through
    /// it already.
    pub fn asks_again(asked: bool, ingested: bool) -> bool
    clauses [ensures result == spec_asks_again(asked, ingested) by asks_again_is_exact]
    { asked && !ingested }
}

#[cfg(test)]
mod tests;
