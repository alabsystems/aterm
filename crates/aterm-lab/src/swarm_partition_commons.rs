// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `swarm partition`'s Commons plane: the replicated database over the lab vault, driven in this
//! process on each lab Mac's own vault custody (`<home>/.config/aterm/secrets`).
//!
//! A lab instance runs no Commons runtime (it returns under `ATERM_STATE_HOME`), and the
//! runtime's loopback transport is a test's alone, so this lab plays the runtime's part: it opens
//! each Mac's endpoint (`aterm_db::control::Endpoint`) and runs the repair exchange the runtime
//! would run between two Macs that reach each other, over a loopback `lan::Connection`. During
//! the cut it pairs Macs of one island only, as a LAN runtime finds only those; after the heal it
//! pairs across, then round a ring of every enrolled Mac, as the runtime's repair on every tick
//! would, until every Mac reads alike, a pass moves nothing with every exchange run, or one pass
//! per Mac has run. Everything else is the production
//! code: enrolment, the roster's signed history, the catalog and the table.
//!
//! Before the cut a2, b1 and b2 enrol with a1 (the vault's founder and Commons authority) and
//! a1 declares the lane's table. In the cut each island lets its newcomer in (a3 with a1, b3 with
//! b1, an admin under `--writers both`) and writes a row on its side. After the heal the lane
//! reads each Mac's control history (one head, or competing heads) and which rows it can read.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use aterm_db::catalog::{Descriptor, Directory};
use aterm_db::control::{Endpoint, Repair, Side};
use aterm_db::table::{Session, store::Store};
use aterm_net::lan::{Connection, Scope};
use aterm_secrets::client::Paths;
use aterm_secrets::commons::bootstrap::Bootstrap;
use aterm_secrets::commons::journal::{Journal, State};

use super::Island;

/// The lane's table.
const APPLICATION: &str = "aterm-lab-partition";
/// Its role.
const ROLE: &str = "f8";
/// The rounds of one exchange: each moves at most 16 control nodes and one shared table.
const ROUNDS: usize = 4;
/// How long a Mac's custody may be busy (its own subscriber writing) before a read gives up: the
/// hang detector, as the custody must come free.
const BUSY: Duration = super::HANG;
/// How many times an exchange a busy custody refused is run.
const BUSY_TRIES: usize = 3;

/// One Mac's Commons custody, as this lab opens it.
#[derive(Clone, Debug)]
pub struct Mac {
    /// Its name (`a1` …).
    pub name: String,
    /// Its vault paths.
    pub paths: Paths,
    /// The Commons config file its endpoint reads (absent: sharing on).
    pub config: PathBuf,
}

/// A row written during the run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wrote {
    /// The row's word.
    pub row: String,
    /// The writing Mac.
    pub by: String,
    /// Its island.
    pub island: Island,
    /// The error, where the write failed.
    pub error: Option<String>,
}

/// A newcomer's enrolment during the cut.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Enrolled {
    /// The newcomer.
    pub mac: String,
    /// Its island.
    pub island: Island,
    /// The Mac that let it in.
    pub by: String,
    /// The error, where it was not let in.
    pub error: Option<String>,
}

/// One Mac after the heal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct After {
    /// The Mac.
    pub mac: String,
    /// Its control history's state (`ready`, `conflict`, `pending`, …).
    pub state: String,
    /// The heads its history holds: one when ready, two or more when they compete.
    pub heads: Vec<String>,
    /// The rows it reads.
    pub rows: Vec<String>,
    /// Why it read none, where it could not.
    pub unreadable: Option<String>,
}

/// The Commons plane's evidence.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Evidence {
    /// Why the plane did not run, where it did not.
    pub skipped: Option<String>,
    /// The Macs enrolled before the cut.
    pub before: Vec<String>,
    /// The newcomers' enrolments during the cut.
    pub enrolled: Vec<Enrolled>,
    /// Every row written.
    pub wrote: Vec<Wrote>,
    /// The exchanges that failed, each `<a> <-> <b>: <why>`.
    pub failed_exchanges: Vec<String>,
    /// Each enrolled Mac after the heal.
    pub after: Vec<After>,
    /// Milliseconds the heal's repair took: its exchanges and the reads that judge each pass.
    pub heal_ms: u64,
}

/// One named check of the plane.
pub type Check = super::Check;

/// Whether every Mac read after the heal reads alike: the same control history, heads, rows and
/// reason it could read none. Where they do, the heal's repair has nothing left to carry.
pub fn alike(reads: &[After]) -> bool {
    reads.windows(2).all(|w| {
        let (x, y) = (&w[0], &w[1]);
        x.state == y.state && x.heads == y.heads && x.rows == y.rows && x.unreadable == y.unreadable
    })
}

/// Whether the heal's repair stops after a ring pass that turned the reads `before` into
/// `after` with `failed` exchanges failing: every Mac reads [`alike`], or the pass moved no read
/// and every exchange in it ran. A failed exchange is run again on the next pass, as the
/// runtime's next tick would.
pub fn settled(before: &[After], after: &[After], failed: usize) -> bool {
    alike(after) || (after == before && failed == 0)
}

/// The rows a Mac must read after the heal: every row a write succeeded for.
fn written(ev: &Evidence) -> Vec<String> {
    ev.wrote
        .iter()
        .filter(|w| w.error.is_none())
        .map(|w| w.row.clone())
        .collect()
}

/// Judge the Commons plane.
#[must_use]
pub fn judge(ev: &Evidence) -> Vec<Check> {
    if let Some(why) = &ev.skipped {
        return vec![Check {
            name: "commons: the plane ran",
            pass: false,
            detail: why.clone(),
        }];
    }
    let mut out = Vec::new();
    let mut idle = Vec::new();
    for island in [Island::A, Island::B] {
        let rows: Vec<&Wrote> = ev
            .wrote
            .iter()
            .filter(|w| w.island == island && w.row.ends_with("-in-cut"))
            .collect();
        if rows.is_empty() {
            idle.push(format!("island {} wrote no row", island.word()));
        }
        for w in rows.iter().filter(|w| w.error.is_some()) {
            idle.push(format!(
                "{} could not write {}: {}",
                w.by,
                w.row,
                w.error.as_deref().unwrap_or_default()
            ));
        }
        let joins: Vec<&Enrolled> = ev.enrolled.iter().filter(|e| e.island == island).collect();
        if joins.is_empty() {
            idle.push(format!("island {} let no newcomer in", island.word()));
        }
        for e in joins.iter().filter(|e| e.error.is_some()) {
            idle.push(format!(
                "{} could not let {} in: {}",
                e.by,
                e.mac,
                e.error.as_deref().unwrap_or_default()
            ));
        }
    }
    out.push(Check {
        name: "commons: each island enrolled and wrote during the cut",
        pass: idle.is_empty(),
        detail: if idle.is_empty() {
            format!(
                "{}; rows {}",
                ev.enrolled
                    .iter()
                    .map(|e| format!("{} let {} in", e.by, e.mac))
                    .collect::<Vec<_>>()
                    .join(", "),
                written(ev).join(", ")
            )
        } else {
            idle.join("; ")
        },
    });

    let heads: std::collections::BTreeSet<&Vec<String>> =
        ev.after.iter().map(|a| &a.heads).collect();
    let split: Vec<String> = ev
        .after
        .iter()
        .filter(|a| a.state != "ready" || a.heads.len() != 1)
        .map(|a| format!("{} {} ({} heads)", a.mac, a.state, a.heads.len()))
        .collect();
    out.push(Check {
        name: "commons: one control history on every Mac after the heal",
        pass: !ev.after.is_empty() && split.is_empty() && heads.len() == 1,
        detail: if ev.after.is_empty() {
            "no Mac was read after the heal".to_owned()
        } else if split.is_empty() && heads.len() == 1 {
            format!(
                "{} Macs ready on one head, {:.1}s of repair after the heal",
                ev.after.len(),
                super::secs(ev.heal_ms)
            )
        } else if split.is_empty() {
            format!("{} different heads, each ready", heads.len())
        } else {
            split.join(", ")
        },
    });

    let want = written(ev);
    let mut lost = Vec::new();
    for a in &ev.after {
        if let Some(why) = &a.unreadable {
            lost.push(format!("{} reads no row: {why}", a.mac));
            continue;
        }
        let missing: Vec<&String> = want.iter().filter(|r| !a.rows.contains(r)).collect();
        if !missing.is_empty() {
            lost.push(format!(
                "{} lacks {}",
                a.mac,
                missing
                    .iter()
                    .map(|r| r.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    if ev.after.is_empty() {
        lost.push("no Mac was read after the heal".to_owned());
    }
    out.push(Check {
        name: "commons: every row written on either side is on every Mac after the heal",
        pass: lost.is_empty(),
        detail: if lost.is_empty() {
            format!("{} rows on {} Macs", want.len(), ev.after.len())
        } else {
            lost.join("; ")
        },
    });
    out
}

/// The report's Commons section.
#[must_use]
pub fn report(ev: &Evidence) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    if let Some(why) = &ev.skipped {
        let _ = writeln!(out, "- not run: {why}");
        return out;
    }
    let _ = writeln!(
        out,
        "- enrolled with a1 before the cut: {}",
        ev.before.join(", ")
    );
    for e in &ev.enrolled {
        let _ = writeln!(
            out,
            "- in the cut, island {}: {} lets {} in: {}",
            e.island.word(),
            e.by,
            e.mac,
            e.error.as_deref().unwrap_or("enrolled")
        );
    }
    for w in &ev.wrote {
        let _ = writeln!(
            out,
            "- island {}: {} writes `{}`: {}",
            w.island.word(),
            w.by,
            w.row,
            w.error.as_deref().unwrap_or("written")
        );
    }
    for f in &ev.failed_exchanges {
        let _ = writeln!(out, "- exchange failed: {f}");
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "| Mac | control history | heads | rows it reads |");
    let _ = writeln!(out, "|---|---|---|---|");
    for a in &ev.after {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} |",
            a.mac,
            a.state,
            a.heads.len(),
            a.unreadable
                .as_ref()
                .map_or_else(|| a.rows.join(", "), |why| format!("none ({why})"))
        );
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Driving it.
// ---------------------------------------------------------------------------------------------

fn bootstrap(mac: &Mac) -> Result<Bootstrap, String> {
    Bootstrap::required_until(&mac.paths, Instant::now() + BUSY)
        .map_err(|e| format!("{}: its Commons bootstrap: {e}", mac.name))
}

/// `mac`'s enrolled endpoint, or `None` while it is pending.
fn endpoint(mac: &Mac) -> Result<Option<Endpoint>, String> {
    let b = bootstrap(mac)?;
    match Endpoint::from_bootstrap(&b).map_err(|e| format!("{}: {e}", mac.name))? {
        Some(e) => e
            .with_commons(mac.paths.clone(), mac.config.clone())
            .map(Some)
            .map_err(|e| format!("{}: {e}", mac.name)),
        None => Ok(None),
    }
}

fn is_busy_error(e: &str) -> bool {
    e.contains("(vault-busy)")
        || e.contains("vault state is changing")
        || e.contains("changed while acquiring")
        || e.contains("(custody-busy)")
        || e.contains("(journal-busy)")
        || e.contains("stale membership")
        || e.contains("no longer current")
}

fn retry_busy<T>(mut f: impl FnMut() -> Result<T, String>) -> Result<T, String> {
    let mut got = f();
    for _ in 1..BUSY_TRIES {
        match &got {
            Err(e) if is_busy_error(e) => {
                std::thread::sleep(Duration::from_millis(50));
                got = f();
            }
            _ => break,
        }
    }
    got
}

/// Run `rounds` exchanges: `initiator` dials `responder` on loopback, each round a fresh
/// connection, one `Repair` per side kept across them.
fn exchange(initiator: &Endpoint, responder: &Endpoint, rounds: usize) -> Result<(), String> {
    let mut mine = Repair::default();
    let mut theirs = Repair::default();
    for _ in 0..rounds {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
        let addr = listener.local_addr().map_err(|e| e.to_string())?;
        let (served, dialed) = std::thread::scope(|s| {
            let server = s.spawn(|| {
                let (stream, _) = listener.accept().map_err(|e| e.to_string())?;
                let conn = Connection::admit(stream, Scope::Loopback).map_err(|e| e.to_string())?;
                responder
                    .exchange(conn, Side::Responder, &mut theirs)
                    .map_err(|e| e.to_string())
            });
            let dialed = Connection::connect(addr, Scope::Loopback, super::HANG)
                .map_err(|e| e.to_string())
                .and_then(|conn| {
                    initiator
                        .exchange(conn, Side::Initiator, &mut mine)
                        .map_err(|e| e.to_string())
                });
            if dialed.is_err() {
                // Unblock the responder's accept, which no dial will reach.
                let _ = std::net::TcpStream::connect(addr);
            }
            let served = server
                .join()
                .unwrap_or_else(|_| Err("the responder panicked".to_owned()));
            (served, dialed)
        });
        match (served, dialed) {
            (Err(s), Err(d)) if is_busy_error(&d) && !is_busy_error(&s) => {
                return Err(format!("the initiator: {d}"));
            }
            (Err(s), _) => return Err(format!("the responder: {s}")),
            (Ok(_), Err(d)) => return Err(format!("the initiator: {d}")),
            (Ok(_), Ok(_)) => {}
        }
        let app_err = match (mine.application_error(), theirs.application_error()) {
            (Some(m), Some(t)) if is_busy_error(t) && !is_busy_error(m) => Some(t),
            (Some(m), _) => Some(m),
            (None, Some(t)) => Some(t),
            (None, None) => None,
        };
        if let Some(e) = app_err {
            return Err(format!("the table round: {e}"));
        }
    }
    Ok(())
}

/// Enrol `newcomer` with `authority`, an admin's enrolled endpoint.
fn enrol(newcomer: &Mac, authority: &Mac) -> Result<(), String> {
    retry_busy(|| {
        if endpoint(newcomer)?.is_some() {
            return Ok(());
        }
        let b = bootstrap(newcomer)?;
        let pending = Endpoint::pending_from_bootstrap(&b, newcomer.paths.clone())
            .map_err(|e| format!("{}: pending: {e}", newcomer.name))?;
        let owner = endpoint(authority)?
            .ok_or_else(|| format!("{} is not enrolled itself", authority.name))?;
        exchange(&pending, &owner, 1)?;
        endpoint(newcomer)?
            .map(|_| ())
            .ok_or_else(|| format!("{} is still pending after the exchange", newcomer.name))
    })
}

/// Write `row` into the lane's table on `mac`.
fn write_row(mac: &Mac, row: &str) -> Result<(), String> {
    retry_busy(|| {
        let b = bootstrap(mac)?;
        let session = Session::open(&b).map_err(|e| format!("its session: {e}"))?;
        let directory = Directory::new(&mac.paths, b.trust());
        let descriptor =
            Descriptor::application(b.trust(), APPLICATION, ROLE, 1).map_err(|e| e.to_string())?;
        directory
            .publish(&session, &descriptor)
            .map_err(|e| format!("its declaration: {e}"))?;
        let path = directory
            .path(descriptor.scope)
            .map_err(|e| e.to_string())?;
        let mut store = Store::open(&session, &path, descriptor.scope)
            .map_err(|e| format!("its table: {e}"))?;
        let payload = format!("{{\"row\":\"{row}\"}}");
        store
            .commit_catalog(&session, payload.as_bytes())
            .map(|_| ())
            .map_err(|e| format!("its commit: {e}"))
    })
}

/// The rows `mac` reads, or why it reads none.
fn read_rows(mac: &Mac) -> Result<Vec<String>, String> {
    retry_busy(|| {
        let b = bootstrap(mac)?;
        let directory = Directory::new(&mac.paths, b.trust());
        let descriptor =
            Descriptor::application(b.trust(), APPLICATION, ROLE, 1).map_err(|e| e.to_string())?;
        let path = directory
            .path(descriptor.scope)
            .map_err(|e| e.to_string())?;
        if !path.try_exists().unwrap_or(false) {
            return Ok(Vec::new());
        }
        let session = Session::observe(&b).map_err(|e| e.to_string())?;
        let store = Store::open(&session, &path, descriptor.scope).map_err(|e| e.to_string())?;
        let mut rows: Vec<String> = store
            .catalog(&session)
            .map_err(|e| e.to_string())?
            .iter()
            .filter_map(|r| {
                let text = std::str::from_utf8(r.payload()).ok()?;
                let value = aterm_json::from_str::<aterm_json::Value>(text).ok()?;
                value.get("row")?.as_str().map(str::to_owned)
            })
            .collect();
        rows.sort();
        rows.dedup();
        Ok(rows)
    })
}

/// `mac`'s control history: its state's word and its heads.
fn history(mac: &Mac) -> (String, Vec<String>) {
    let b = match bootstrap(mac) {
        Ok(b) => b,
        Err(e) => return (format!("unread ({e})"), Vec::new()),
    };
    if b.is_log_backed().unwrap_or(false)
        && let Ok(journal) = Journal::read_existing(b.journal_dir(), b.trust())
        && let Ok(Some(access)) = b.access_in(&journal)
    {
        return (
            "ready".to_owned(),
            vec![aterm_codec::hex::encode_lower(access.head())],
        );
    }
    let read = Journal::read_existing(b.journal_dir(), b.trust()).map_err(|e| e.to_string());
    match read.map(|j| j.state()) {
        Ok(State::Ready { head, .. }) => (
            "ready".to_owned(),
            vec![aterm_codec::hex::encode_lower(&head)],
        ),
        Ok(State::Conflict { heads }) => (
            "conflict".to_owned(),
            heads
                .iter()
                .map(|h| aterm_codec::hex::encode_lower(h))
                .collect(),
        ),
        Ok(State::Empty) => ("empty".to_owned(), Vec::new()),
        Ok(State::Uncertain) => ("uncertain".to_owned(), Vec::new()),
        Ok(State::Pending) => ("pending".to_owned(), Vec::new()),
        Ok(State::Invalid { nodes }) => (format!("invalid ({} nodes)", nodes.len()), Vec::new()),
        Err(e) => (format!("unread ({e})"), Vec::new()),
    }
}

/// The plane's driver, kept across the lane's timeline.
pub struct Plane {
    /// The Macs, by name.
    pub macs: Vec<Mac>,
    /// What it saw.
    pub ev: Evidence,
}

impl Plane {
    fn mac(&self, name: &str) -> Option<&Mac> {
        self.macs.iter().find(|m| m.name == name)
    }

    /// Exchange `a` with `b`, noting a failure. An exchange refused because a custody was busy
    /// (its Mac's own subscriber writing) is run again, as the runtime's next tick would, up to
    /// [`BUSY_TRIES`] times.
    fn pair(&mut self, a: &str, b: &str) {
        let got = retry_busy(|| match (self.mac(a), self.mac(b)) {
            (Some(x), Some(y)) => match (endpoint(x), endpoint(y)) {
                (Ok(Some(x)), Ok(Some(y))) => exchange(&x, &y, ROUNDS),
                (Ok(None), _) => Err(format!("{a} is not enrolled")),
                (_, Ok(None)) => Err(format!("{b} is not enrolled")),
                (Err(e), _) | (_, Err(e)) => Err(e),
            },
            _ => Err("no such Mac".to_owned()),
        });
        if let Err(e) = got {
            self.ev.failed_exchanges.push(format!("{a} <-> {b}: {e}"));
        }
    }

    /// Before the cut: enrol a2, b1 and b2 with a1, seed Genesis v2 so the plane runs under
    /// log-backed Commons control (`F18`), declare the table on a1 with a row, and share it with
    /// every Mac.
    pub fn before(&mut self) {
        let Some(a1) = self.mac("a1").cloned() else {
            self.ev.skipped = Some("no a1".to_owned());
            return;
        };
        match endpoint(&a1) {
            Ok(Some(_)) => self.ev.before.push("a1".to_owned()),
            Ok(None) => {
                self.ev.skipped =
                    Some("a1, the vault's founder, holds no Commons membership".to_owned());
                return;
            }
            Err(e) => {
                self.ev.skipped = Some(e);
                return;
            }
        }
        for name in ["a2", "b1", "b2"] {
            let Some(mac) = self.mac(name).cloned() else {
                continue;
            };
            match enrol(&mac, &a1) {
                Ok(()) => self.ev.before.push(name.to_owned()),
                Err(e) => {
                    self.ev.skipped = Some(format!(
                        "{name} could not enrol with a1 before the cut: {e}"
                    ));
                    return;
                }
            }
        }
        let genesis_out = a1.paths.dir.join("f8.genesis");
        let _ = std::fs::remove_file(&genesis_out);
        if let Err(e) = aterm_secrets::client::seed_vault_genesis(&a1.paths, &genesis_out) {
            self.ev.skipped = Some(format!("a1 could not seed Genesis v2 before the cut: {e}"));
            return;
        }
        if let Ok(store) = aterm_secrets::client::load_records(&a1.paths) {
            let wire = store.encode();
            for name in ["a2", "b1", "b2"] {
                if let Some(mac) = self.mac(name) {
                    let _ = aterm_secrets::client::save_records(&mac.paths, &wire);
                }
            }
            if let Some(b1) = self.mac("b1")
                && let Ok(Some(cfg)) = aterm_secrets::client::config(&b1.paths)
                && let aterm_secrets::transport::Source::Url(url) = &cfg.source
            {
                let _ = aterm_secrets::transport::post_records(url, &wire);
            }
        }
        self.write("a1", Island::A, "before-cut");
        if let Some(e) = self.ev.wrote.last().and_then(|w| w.error.clone()) {
            self.ev.skipped = Some(format!("a1 could not write before-cut: {e}"));
            return;
        }
        for name in ["a2", "b1", "b2"] {
            self.pair(name, "a1");
        }
    }

    fn write(&mut self, by: &str, island: Island, row: &str) {
        let error = match self.mac(by) {
            Some(mac) => write_row(mac, row).err(),
            None => Some("no such Mac".to_owned()),
        };
        self.ev.wrote.push(Wrote {
            row: row.to_owned(),
            by: by.to_owned(),
            island,
            error,
        });
    }

    /// In the cut: island A lets `a3` in with a1 and island B `b3` with b1 (each only where the
    /// vault let it in), each writes a row, and each island's Macs exchange among themselves.
    pub fn in_cut(&mut self, vault_in: &[&str]) {
        if self.ev.skipped.is_some() {
            return;
        }
        for (newcomer, by, island) in [("a3", "a1", Island::A), ("b3", "b1", Island::B)] {
            let error = if !vault_in.contains(&newcomer) {
                Some("not a member of the vault".to_owned())
            } else {
                match (self.mac(newcomer), self.mac(by)) {
                    (Some(n), Some(b)) => enrol(n, b).err(),
                    _ => Some("no such Mac".to_owned()),
                }
            };
            self.ev.enrolled.push(Enrolled {
                mac: newcomer.to_owned(),
                island,
                by: by.to_owned(),
                error,
            });
        }
        self.write("a1", Island::A, "a-in-cut");
        self.write("b1", Island::B, "b-in-cut");
        for (a, b) in [("a2", "a1"), ("a3", "a1"), ("b2", "b1"), ("b3", "b1")] {
            if self
                .ev
                .enrolled
                .iter()
                .any(|e| e.mac == a && e.error.is_some())
            {
                continue;
            }
            self.pair(a, b);
        }
    }

    /// After the heal: exchange across, then round a ring of every enrolled Mac, as the runtime
    /// repairs on every tick, until the repair is [`settled`] or one pass per Mac has run
    /// (enough to carry a row round the ring). The last reads are the plane's `after`.
    pub fn after_heal(&mut self) {
        if self.ev.skipped.is_some() {
            return;
        }
        let names: Vec<String> = self
            .macs
            .iter()
            .map(|m| m.name.clone())
            .filter(|m| {
                self.ev.before.contains(m)
                    || self
                        .ev
                        .enrolled
                        .iter()
                        .any(|e| &e.mac == m && e.error.is_none())
            })
            .collect();
        let t = Instant::now();
        for (a, b) in [
            ("b1", "a1"),
            ("a1", "b1"),
            ("a2", "a1"),
            ("b2", "b1"),
            ("a3", "b1"),
            ("b3", "a1"),
        ] {
            if names.iter().any(|m| m == a) && names.iter().any(|m| m == b) {
                self.pair(a, b);
            }
        }
        let mut reads = self.read(&names);
        for _ in 0..names.len() {
            if names.len() < 2 || alike(&reads) {
                break;
            }
            let failed = self.ev.failed_exchanges.len();
            for (i, a) in names.iter().enumerate() {
                self.pair(a, &names[(i + 1) % names.len()]);
            }
            let next = self.read(&names);
            let stop = settled(&reads, &next, self.ev.failed_exchanges.len() - failed);
            reads = next;
            if stop {
                break;
            }
        }
        self.ev.heal_ms = u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.ev.after = reads;
    }

    /// Read each of `names`: its control history and the rows it reads.
    fn read(&self, names: &[String]) -> Vec<After> {
        names
            .iter()
            .filter_map(|name| {
                let mac = self.mac(name)?;
                let (state, heads) = history(mac);
                let (rows, unreadable) = match read_rows(mac) {
                    Ok(rows) => (rows, None),
                    Err(e) => (Vec::new(), Some(e)),
                };
                Some(After {
                    mac: name.clone(),
                    state,
                    heads,
                    rows,
                    unreadable,
                })
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "swarm_partition_commons_tests.rs"]
mod tests;
