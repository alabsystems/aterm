// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm fleet [--json]` (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.12, §4.16, §11 B9): the
//! lab's status line and one row per seat, read from what this Mac holds. It spawns nothing and
//! dials no other Mac.
//!
//! **Where each column comes from.** The seats are this Mac's membership log's current seats
//! (their label, the host name the log gives, the node id of the node key they bound), and any
//! seat the node's view holds that the log does not, under no label. Up or
//! silent, each seat's build and whether its tables are in step come from presence, read from
//! the node service's view (`aterm_mesh::view`): the node judges who is up from the rows that
//! advanced while it ran, so a reader of its own would read every seat silent for a period. Where
//! a seat has no presence row, its build is the one its own signed entry in this Mac's fleet book
//! names (before wave B, §4.12; never a poll). The log's fill is this Mac's own records against
//! the bound.
//!
//! **Needs update** names a member whose build is below what the lab needs now: below the record
//! fence ([`RECORD_FENCE`]) once this Mac's log has crossed past what such builds hold; or, while
//! this Mac's node serves presence, a member its book heard on the old path lately whose node
//! publishes no presence row, a build below the mesh protocol.
//!
//! **No Commons machine uuid column yet:** nothing on this Mac maps a node id to its registry
//! row without reading Commons' tables; `aterm db computers` is the registry's join (§4.12).

use aterm_json::{Value, json};
use aterm_mesh::presence::{Behind, Build, Standing as Presence};
use aterm_mesh::view::{Unserved, View};
use aterm_net::fleet_member::{LIVE_WITHIN_MS, Peer};
use aterm_netplace::words::{self, InStep, Seats};
use aterm_secrets::membership_log::{Log, MAX_HELD_RECORDS};
use std::fmt::Write as _;
use std::path::Path;
use std::process::ExitCode;

/// The first release whose membership reader holds more than `LEGACY_RECORDS`: 0.146 to 0.153
/// hold at most 4096 records, so once this Mac's log crossed past them a member that runs one
/// cannot hold the log and needs an update.
pub const RECORD_FENCE: (u64, u64) = (0, 154);

/// One seat as `aterm fleet` shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeatRow {
    /// Its log label (`m<N>`); `-` for a seat the view holds and this Mac's log does not.
    pub label: String,
    /// Its host name as the membership log gives it.
    pub name: String,
    /// Its node id (`n-<16 hex>`), where it bound a node key.
    pub node: Option<String>,
    /// Whether it is this Mac.
    pub me: bool,
    /// Up or silent, as this Mac's presence reads it; `None` where no presence was read.
    pub presence: Option<Presence>,
    /// Its build: its presence row's, else its signed book entry's.
    pub build: Option<String>,
    /// Whether it needs an update.
    pub needs_update: bool,
    /// How far its tables are from this Mac's; `None` where no presence was read.
    pub behind: Option<Behind>,
}

/// The whole `aterm fleet` answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fleet {
    /// The status line's facts.
    pub seats: Seats,
    /// One row per seat, in label order, this Mac's among them.
    pub rows: Vec<SeatRow>,
    /// Why no presence was read, where none was.
    pub unserved: Option<String>,
}

/// A current seat of this Mac's membership log, as `aterm fleet` lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogSeat {
    /// Its log label (`m<N>`).
    pub label: String,
    /// Its host name as the log gives it.
    pub name: String,
    /// The node key it bound, where it bound one.
    pub node_key: Option<[u8; 32]>,
}

/// The current seats of `log`: each seat the log removed neither by its node nor by its binding.
#[must_use]
pub fn log_seats(log: &Log) -> Vec<LogSeat> {
    let membership = log.membership();
    membership
        .members()
        .values()
        .filter(|seat| {
            !membership.member_node_removed(seat)
                && !log.recipient_is_removed(&seat.binding.recipient())
        })
        .map(|seat| LogSeat {
            label: seat.label.clone(),
            name: seat.name.clone(),
            node_key: seat.node_key,
        })
        .collect()
}

/// What [`compose`] reads: every fact is already on this Mac.
pub struct Facts<'a> {
    /// This Mac's membership log's current seats ([`log_seats`]), where its vault holds a log.
    pub seats: Option<&'a [LogSeat]>,
    /// The records this Mac holds, and whether its log has crossed the legacy bound.
    pub records: Option<(u64, bool)>,
    /// The node service's view, or why there is none.
    pub view: Result<&'a View, String>,
    /// This Mac's fleet book.
    pub book: &'a [Peer],
    /// Now, Unix ms.
    pub now_ms: u64,
}

/// `n-<16 hex>` of a node key: astream's identity principal, as presence names a seat.
#[must_use]
pub fn principal_of(key: &[u8; 32]) -> String {
    format!("n-{}", aterm_codec::hex::encode_lower(&key[..8]))
}

/// The version `key`'s own signed book entry names, and when the book last heard it.
fn booked<'a>(book: &'a [Peer], key: &[u8; 32]) -> Option<(&'a str, Option<u64>)> {
    book.iter()
        .filter(|peer| peer.key.is_some_and(|k| k.bytes() == key) && peer.signature_holds())
        .find_map(|peer| Some((peer.version.as_deref()?, peer.seen_ms)))
}

/// Whether `build` is below `floor`; a build that does not read as a release is not.
fn below(build: &str, floor: (u64, u64)) -> bool {
    Build::new(build)
        .and_then(|build| build.release())
        .is_some_and(|release| release < floor)
}

/// The rows and the status line of `facts` (§4.12). Pure: a test hands in every fact.
#[must_use]
pub fn compose(facts: &Facts<'_>) -> Fleet {
    let view = facts.view.as_ref().ok();
    let seat_of =
        |principal: &str| view.and_then(|v| v.seats.iter().find(|s| s.principal == principal));
    let crossed = facts.records.is_some_and(|(_, crossed)| crossed);
    let needs_update = |build: Option<&str>, has_row: bool, seen_ms: Option<u64>| {
        build.is_some_and(|b| crossed && below(b, RECORD_FENCE))
            || (view.is_some()
                && !has_row
                && seen_ms.is_some_and(|seen| facts.now_ms.saturating_sub(seen) < LIVE_WITHIN_MS))
    };
    let mut rows = Vec::new();
    if let Some(seats) = facts.seats {
        for seat in seats {
            let node = seat.node_key.as_ref().map(principal_of);
            let held = node.as_deref().and_then(seat_of);
            let row = held.and_then(|s| s.row.as_ref());
            let book = seat
                .node_key
                .as_ref()
                .and_then(|key| booked(facts.book, key));
            let build = row
                .map(|(_, row)| row.build.as_str().to_owned())
                .or_else(|| book.map(|(version, _)| version.to_owned()));
            rows.push(SeatRow {
                label: seat.label.clone(),
                name: seat.name.clone(),
                me: held.is_some_and(|s| s.me)
                    || view.is_some_and(|v| node.as_deref() == Some(v.me.as_str())),
                presence: view
                    .map(|_| held.map_or(Presence::Silent { last_seen_ms: None }, |s| s.standing)),
                needs_update: needs_update(
                    build.as_deref(),
                    row.is_some(),
                    book.and_then(|(_, seen)| seen),
                ),
                behind: view.map(|_| held.map_or(Behind::Unknown, |s| s.behind)),
                build,
                node,
            });
        }
    }
    if let Some(view) = view {
        // A seat the node holds as current that this Mac's log does not seat (a log read
        // behind the node's): listed, so no up seat goes unseen.
        for seat in &view.seats {
            if rows
                .iter()
                .any(|row| row.node.as_deref() == Some(seat.principal.as_str()))
            {
                continue;
            }
            let build = seat
                .row
                .as_ref()
                .map(|(_, row)| row.build.as_str().to_owned());
            rows.push(SeatRow {
                label: "-".to_owned(),
                name: "-".to_owned(),
                node: Some(seat.principal.clone()),
                me: seat.me,
                presence: Some(seat.standing),
                needs_update: needs_update(build.as_deref(), seat.row.is_some(), None),
                behind: Some(seat.behind),
                build,
            });
        }
    }
    rows.sort_by(|a, b| {
        label_order(&a.label)
            .cmp(&label_order(&b.label))
            .then_with(|| a.node.cmp(&b.node))
    });
    let in_step = view.map(|_| {
        let others: Vec<&SeatRow> = rows
            .iter()
            .filter(|row| !row.me && row.presence == Some(Presence::Up))
            .collect();
        InStep {
            with: others
                .iter()
                .filter(|row| row.behind == Some(Behind::InStep))
                .count(),
            of: others.len(),
            slowest_ms: others
                .iter()
                .filter_map(|row| match row.behind {
                    Some(Behind::Behind { at_least }) => {
                        Some(u64::try_from(at_least.as_millis()).unwrap_or(u64::MAX))
                    }
                    _ => None,
                })
                .max(),
            apart: others
                .iter()
                .filter(|row| row.behind == Some(Behind::Apart))
                .count(),
        }
    });
    let seats = Seats {
        // Every seat listed: the log's, and those only the view holds.
        members: rows.len(),
        up: view.map(|_| {
            rows.iter()
                .filter(|row| row.presence == Some(Presence::Up))
                .count()
        }),
        needs_update: rows
            .iter()
            .filter(|row| row.needs_update)
            .map(|row| {
                (
                    row.label.clone(),
                    row.build.clone().unwrap_or_else(|| "-".to_owned()),
                )
            })
            .collect(),
        in_step,
        log: facts
            .records
            .map(|(held, _)| (held, MAX_HELD_RECORDS as u64)),
    };
    Fleet {
        seats,
        rows,
        unserved: facts.view.as_ref().err().cloned(),
    }
}

/// `m<N>` labels in their number's order, then the rest by text.
fn label_order(label: &str) -> (u8, u64, &str) {
    match label.strip_prefix('m').and_then(|n| n.parse::<u64>().ok()) {
        Some(n) => (0, n, label),
        None => (1, 0, label),
    }
}

/// A seat's up column at `at_ms` (the view's time): `up`, `silent 3 h`, `silent`, or `-` where no
/// presence was read.
fn up_words(presence: Option<Presence>, at_ms: Option<u64>) -> String {
    match presence {
        None => "-".to_owned(),
        Some(Presence::Up) => "up".to_owned(),
        Some(Presence::Silent {
            last_seen_ms: Some(seen),
        }) => match at_ms {
            Some(now) => format!("silent {}", words::age_words(now.saturating_sub(seen))),
            None => "silent".to_owned(),
        },
        Some(Presence::Silent { last_seen_ms: None }) => "silent, never seen up".to_owned(),
    }
}

/// A seat's tables column.
fn step_words(behind: Option<Behind>, me: bool) -> String {
    if me {
        return "-".to_owned();
    }
    match behind {
        None | Some(Behind::Unknown) => "-".to_owned(),
        Some(Behind::InStep) => "in step".to_owned(),
        Some(Behind::Behind { at_least }) => format!(
            "{} behind",
            words::age_words(u64::try_from(at_least.as_millis()).unwrap_or(u64::MAX))
        ),
        Some(Behind::Apart) => "apart".to_owned(),
    }
}

/// `aterm fleet`'s text: the status line, why no presence was read where none was, then one row
/// per seat.
#[must_use]
pub fn render(fleet: &Fleet, at_ms: Option<u64>) -> String {
    let mut out = words::status_line(&fleet.seats);
    out.push('\n');
    if let Some(why) = &fleet.unserved {
        let _ = writeln!(out, "presence: {why}");
    }
    let table: Vec<[String; 6]> = fleet
        .rows
        .iter()
        .map(|row| {
            [
                row.label.clone(),
                if row.me {
                    format!("{} (this Mac)", row.name)
                } else {
                    row.name.clone()
                },
                row.node.clone().unwrap_or_else(|| "-".to_owned()),
                up_words(row.presence, at_ms),
                match (&row.build, row.needs_update) {
                    (Some(build), true) => format!("needs update ({build})"),
                    (None, true) => "needs update".to_owned(),
                    (Some(build), false) => build.clone(),
                    (None, false) => "-".to_owned(),
                },
                step_words(row.behind, row.me),
            ]
        })
        .collect();
    let head = ["LABEL", "NAME", "NODE", "UP", "BUILD", "TABLES"].map(str::to_owned);
    let mut widths = head.clone().map(|h| h.chars().count());
    for cells in &table {
        for (width, cell) in widths.iter_mut().zip(cells) {
            *width = (*width).max(cell.chars().count());
        }
    }
    for cells in std::iter::once(&head).chain(&table) {
        let mut line = String::new();
        for (i, (cell, width)) in cells.iter().zip(widths).enumerate() {
            if i + 1 == cells.len() {
                line.push_str(cell);
            } else {
                let _ = write!(line, "{cell:<width$}  ");
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// `aterm fleet --json`'s object.
#[must_use]
pub fn to_json(fleet: &Fleet) -> Value {
    let seats = &fleet.seats;
    let rows: Vec<Value> = fleet
        .rows
        .iter()
        .map(|row| {
            let (up, silent_since_ms) = match row.presence {
                None => (Value::Null, Value::Null),
                Some(Presence::Up) => (Value::from(true), Value::Null),
                Some(Presence::Silent { last_seen_ms }) => (
                    Value::from(false),
                    last_seen_ms.map_or(Value::Null, Value::from),
                ),
            };
            let (tables, behind_ms) = match row.behind {
                None => (Value::Null, Value::Null),
                Some(Behind::InStep) => (Value::from("in_step"), Value::Null),
                Some(Behind::Behind { at_least }) => (
                    Value::from("behind"),
                    Value::from(u64::try_from(at_least.as_millis()).unwrap_or(u64::MAX)),
                ),
                Some(Behind::Apart) => (Value::from("apart"), Value::Null),
                Some(Behind::Unknown) => (Value::from("unknown"), Value::Null),
            };
            json!({
                "label": row.label.as_str(),
                "name": row.name.as_str(),
                "node": row.node.as_deref().map_or(Value::Null, Value::from),
                "me": row.me,
                "up": up,
                "silent_since_ms": silent_since_ms,
                "build": row.build.as_deref().map_or(Value::Null, Value::from),
                "needs_update": row.needs_update,
                "tables": tables,
                "behind_ms": behind_ms,
            })
        })
        .collect();
    json!({
        "status": words::status_line(seats),
        "members": seats.members as u64,
        "up": seats.up.map_or(Value::Null, |up| Value::from(up as u64)),
        "needs_update": seats
            .needs_update
            .iter()
            .map(|(label, build)| json!({"label": label.as_str(), "build": build.as_str()}))
            .collect::<Vec<Value>>(),
        "in_step": seats.in_step.map_or(Value::Null, |step| json!({
            "with": step.with as u64,
            "of": step.of as u64,
            "slowest_ms": step.slowest_ms.map_or(Value::Null, Value::from),
            "apart": step.apart as u64,
        })),
        "log": seats.log.map_or(Value::Null, |(held, bound)| json!({"held": held, "bound": bound})),
        "presence": fleet.unserved.as_deref().map_or(Value::from("served"), Value::from),
        "seats": rows,
    })
}

/// The view of the node service whose state directory is `state`, or why there is none, in a
/// person's words.
pub fn ask_node(state: &Path) -> Result<View, String> {
    aterm_mesh::view::ask(
        &state.join(aterm_mesh::logs::MESH_DIR),
        aterm_mesh::view::WITHIN,
    )
    .map_err(|e| match e {
        Unserved::NoNode if !aterm_types::mesh_switch::MESH => {
            "this build runs no mesh, so no node serves presence".to_owned()
        }
        Unserved::NoNode => "no node service serves presence here (`aterm node status`)".to_owned(),
        Unserved::Failed(why) => format!("the node service's view did not read: {why}"),
    })
    .and_then(ready)
}

/// `view` where its node has read the lab's standings; before that it holds this Mac's seat
/// alone, which is no read of who is up.
///
/// # Errors
/// The node has not read the membership standings yet.
pub fn ready(view: View) -> Result<View, String> {
    if view.standings {
        Ok(view)
    } else {
        Err("the node has not read the lab's membership standings yet".to_owned())
    }
}

/// Read every fact on this Mac under the state directory `state` and the vault `paths`, and
/// compose them ([`compose`]), with the node's view a caller already asked ([`ask_node`]).
#[must_use]
pub fn compose_here(
    state: &Path,
    paths: Option<&aterm_secrets::client::Paths>,
    view: Result<&View, String>,
) -> Fleet {
    let book = aterm_net::fleet_member::read_peers(state).unwrap_or_default();
    let read =
        paths.and_then(|paths| aterm_secrets::client::load_acknowledged_and_ahead(paths).ok());
    let records = paths.and_then(|paths| {
        let store = aterm_secrets::client::load_records(paths).ok()?;
        let crossed =
            aterm_secrets::client::records_bound(paths, &store).is_ok_and(|bound| bound.crossed);
        Some((store.len() as u64, crossed))
    });
    let seats = read
        .as_ref()
        .and_then(|read| read.acknowledged.log())
        .map(log_seats);
    compose(&Facts {
        seats: seats.as_deref(),
        records,
        view,
        book: &book,
        now_ms: aterm_netplace::unix_ms_now(),
    })
}

/// Ask the node service under the state directory `state` for its view ([`ask_node`]) and
/// compose it with every other fact on this Mac ([`compose_here`]), and when the view was taken.
#[must_use]
pub fn read(state: &Path, paths: Option<&aterm_secrets::client::Paths>) -> (Fleet, Option<u64>) {
    let view = ask_node(state);
    let fleet = compose_here(state, paths, view.as_ref().map_err(Clone::clone));
    (fleet, view.ok().map(|view| view.at_ms))
}

/// `aterm status`'s fleet line (§4.16): this Mac's own row of [`read`] (its label, node id, up or
/// silent, build) and the seats in a few words; where the view holds no row of this Mac's, the
/// seats and why.
#[must_use]
pub fn own_line(fleet: &Fleet, at_ms: Option<u64>) -> String {
    let seats = words::seats_short(&fleet.seats);
    let Some(own) = fleet.rows.iter().find(|row| row.me) else {
        return match &fleet.unserved {
            Some(why) => format!("{seats}; presence not read: {why}"),
            None => format!("{seats}; the node holds no presence row of this Mac's yet"),
        };
    };
    let mut line = own.label.clone();
    if let Some(node) = &own.node {
        let _ = write!(line, " {node}");
    }
    let _ = write!(line, " {}", up_words(own.presence, at_ms));
    if let Some(build) = &own.build {
        let _ = write!(line, " at {build}");
    }
    if own.needs_update {
        line.push_str(", needs update");
    }
    let _ = write!(line, "; {seats}");
    line
}

/// `aterm fleet [--json]`: the view on stdout, exit 0; a usage error exits 2.
pub fn main_entry(args: &[std::ffi::OsString]) -> ExitCode {
    let json = match args {
        [] => false,
        [flag] if flag == "--json" => true,
        _ => {
            eprintln!("aterm fleet: no such mode; run `aterm fleet --help`");
            return ExitCode::from(2);
        }
    };
    let Some(state) = aterm_types::dirs::state_dir() else {
        eprintln!("aterm fleet: no state directory: $HOME is not set");
        return ExitCode::from(1);
    };
    let paths = aterm_secrets::client::Paths::live().ok();
    let (fleet, at_ms) = read(&state, paths.as_ref());
    let text = if json {
        let mut text = aterm_json::to_string(&to_json(&fleet)).unwrap_or_default();
        text.push('\n');
        text
    } else {
        render(&fleet, at_ms)
    };
    aterm_uds::stdout::answer_exit("aterm fleet", &text, 0)
}

#[cfg(test)]
#[path = "fleet_view_tests.rs"]
mod tests;
