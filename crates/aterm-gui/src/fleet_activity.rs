// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fleet activity record's writers in the processes that serve this Mac's fleet port
//! (`aterm_types::fleet_activity`, `aterm_ctl::fleet_activity`).
//!
//! Each incoming request has one writer, so it has one row. The node job writes what it ends or
//! serves itself: a first line this Mac's fleet access refuses, a board it reads, and a survey a
//! session asked for. A window writes every line it judges: the lines the node job relayed to
//! it, and all of them where the window holds the fleet port itself. A row is handed to the
//! process's writer thread, so no answer waits for the file, and one that cannot be written is
//! dropped.

use aterm_types::fleet_access::DriveKind;
use aterm_types::fleet_activity::{Outcome, Row, Way};

/// A fleet member that reads this Mac over a survey connection, as its handshake proved it: a
/// survey has no requester line, so this is all that names who asked.
#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Reader {
    /// Its node id.
    pub(crate) node: String,
}

#[cfg(unix)]
impl Reader {
    /// The reader a connection's proof names; none where it proved no node.
    pub(crate) fn of(proof: Option<&aterm_net::fleet::NodeProof>) -> Option<Self> {
        proof.map(|proof| Self {
            node: proof.node.clone(),
        })
    }
}

/// The host a driver's label names (`<host> (node <head>)`), or `None` for a driver its book
/// names by its node alone (`node <head>`): `fleet_listen::driver_name`'s inverse.
fn driver_host(label: &str) -> Option<&str> {
    label.split_once(" (node ").map(|(host, _)| host)
}

/// The row of a driver's request of `kind` at this Mac, `by` the session its requester line
/// named, or `None` where it is no agent request.
pub(crate) fn driven(
    driver: &crate::fabric::Driver,
    kind: DriveKind,
    by: Option<&str>,
    outcome: Outcome,
) -> Option<Row> {
    Row::of(
        aterm_ctl::fleet_activity::now_ms(),
        Way::In,
        (&driver.node, driver_host(&driver.host)),
        kind,
        by,
        outcome,
    )
}

/// The row of a survey `reader` was answered, `by` the session its line named
/// (`aterm_types::fleet_activity::survey_asker`); `None` for a poll. A survey's handshake proves
/// a node and no name, and looking one up in the book would be a file read on the answer's way:
/// the row names the node alone, and whoever reads the record names it from the book
/// (`aterm_agent::fleet_activity::Names`).
#[cfg(unix)]
pub(crate) fn surveyed(reader: Option<&Reader>, by: Option<&str>) -> Option<Row> {
    let (reader, by) = (reader?, by?);
    Row::of(
        aterm_ctl::fleet_activity::now_ms(),
        Way::In,
        (&reader.node, None),
        DriveKind::Read,
        Some(by),
        Outcome::Admitted,
    )
}

/// The row of a request this window itself sent the Mac `node` (named `label`) over a drive it
/// dialled (direct fleet mail) for the session `by`, as the first line of the answer says that
/// Mac's lock decided it.
#[cfg(unix)]
pub(crate) fn sent(
    (node, label): (&str, &str),
    by: Option<&str>,
    request: &str,
    answer: &str,
) -> Option<Row> {
    Row::of(
        aterm_ctl::fleet_activity::now_ms(),
        Way::Out,
        (node, Some(label)),
        aterm_types::fleet_activity::kind_of_line(request)?,
        by,
        Outcome::of_reply(answer.trim_end_matches(['\r', '\n']))?,
    )
}

/// Hand `row` to the record of the state folder `state`: the node job's writer, which serves
/// the state folder it was started on.
#[cfg(unix)]
pub(crate) fn note_in(state: &std::path::Path, row: Option<Row>) {
    if let Some(row) = row {
        aterm_ctl::fleet_activity::note_later(state, vec![row]);
    }
}

#[cfg(test)]
thread_local! {
    /// The rows [`note`] was handed on this test thread, which writes none.
    static NOTED: std::cell::RefCell<Vec<Row>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The rows [`note`] was handed on this test thread since the last call.
#[cfg(all(test, unix))]
pub(crate) fn noted() -> Vec<Row> {
    NOTED.with(|noted| std::mem::take(&mut *noted.borrow_mut()))
}

/// Hand `row` to this Mac's record: a window's writer. A unit test's rows stay on its thread
/// ([`noted`]), as its fleet access level does.
pub(crate) fn note(row: Option<Row>) {
    let Some(row) = row else {
        return;
    };
    #[cfg(test)]
    NOTED.with(|noted| noted.borrow_mut().push(row));
    #[cfg(not(test))]
    if let Some(state) = aterm_types::dirs::state_dir() {
        aterm_ctl::fleet_activity::note_later(&state, vec![row]);
    }
}

#[cfg(test)]
#[path = "fleet_activity_tests.rs"]
mod tests;
