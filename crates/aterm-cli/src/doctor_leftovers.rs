// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Doctor's `leftovers:` row (`docs/DESIGN-leftover-processes-2026-10-16.md` §6.4): what closed
//! tabs and dead verify runs left running, and the dangling build links the disk watch saw. It
//! asks the running instance's leftover view (`aterm ctl sys left scope=aterm --json`) within a
//! bound, and an instance that runs but does not answer leaves the row not measured; with none
//! running, it reads this boot's verify run records and this user's process table itself, which
//! names only a dead run's leftovers (no instance, no closed tab) and reads no link, and appends a
//! dead run's `dead_seen` line as every reader of the records does. It ends nothing.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long the running instance has to answer its leftover view.
pub(crate) const INSTANCE_BOUND: Duration = Duration::from_secs(2);

/// The request doctor sends the running instance: every session's rows, as many as one reply
/// shows.
pub(crate) const REQUEST: &str = "sys left scope=aterm top=64 --json";

/// What the row read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Leftovers {
    /// The running instance's view answered; else this boot's records and this user's table.
    pub(crate) from_instance: bool,
    /// Processes that outlived a closed tab or a dead verify run (`state=left`).
    pub(crate) rows: usize,
    /// More rows than the reply showed: `rows` is a floor.
    pub(crate) more: bool,
    /// The longest any of them has run, ms.
    pub(crate) oldest_ms: Option<u64>,
    /// One of them is a row the `leftovers` Entry names ([`aterm_vitals::left::eligible`]).
    pub(crate) eligible: bool,
    /// The dangling links the disk watch saw; `None` where nothing read them (no instance).
    pub(crate) links: Option<usize>,
}

/// The row's facts, or why it was not read.
pub(crate) type LeftoversFact = Result<Leftovers, String>;

/// The row's mark and words: ok with nothing left, a note while nothing is one the Entry names,
/// a warning once one is.
pub(crate) fn row(fact: &LeftoversFact) -> (super::Mark, String) {
    use super::Mark;
    let left = match fact {
        Ok(left) => left,
        Err(why) => return (Mark::Note, format!("not measured ({why})")),
    };
    let links = match left.links {
        Some(0) => "no dangling links".to_owned(),
        Some(1) => "1 dangling link".to_owned(),
        Some(n) => format!("{n} dangling links"),
        None => "links not read with no instance running".to_owned(),
    };
    if left.rows == 0 && !left.more && left.links.unwrap_or(0) == 0 {
        let none = if left.from_instance {
            "no process outlived a closed tab or a dead verify run"
        } else {
            "no process outlived a dead verify run"
        };
        return (Mark::Ok, format!("{none}; {links}"));
    }
    let floor = if left.more { "at least " } else { "" };
    let n = left.rows;
    let noun = if n == 1 { "process" } else { "processes" };
    let owner = if left.from_instance {
        "a closed tab or a dead verify run"
    } else {
        "a dead verify run"
    };
    let oldest = left
        .oldest_ms
        .map(|ms| {
            if ms < 3_600_000 {
                format!(", the oldest {}", aterm_uds::process::held_age(ms))
            } else {
                format!(", the oldest {} h", ms / 3_600_000)
            }
        })
        .unwrap_or_default();
    let mark = if left.eligible {
        Mark::Warn
    } else {
        Mark::Note
    };
    (
        mark,
        format!(
            "{floor}{n} {noun} outlived {owner}{oldest}; {links} (aterm ctl sys left \
             scope=aterm)"
        ),
    )
}

/// Whether a `proc` row, by its `state=` and `reason=` words, is one that outlived its owner:
/// `left` for its owner's going (`owner-gone`, a closed tab; `launcher-dead`, a dead verify run).
/// A row a live run or session still holds (`live`) and a daemon (`daemon`) are listed by the
/// view, never counted here, and neither is a held program, whose state is `program-check`.
fn outlived(state: &str, reason: &str) -> bool {
    use aterm_vitals::left::{State, Why};
    state == State::Left.word()
        && (reason == Why::OwnerGone.word() || reason == Why::LauncherDead.word())
}

/// The facts the running instance's `sys left --json` body says: its `proc` rows that outlived
/// their owner ([`outlived`]), whether the reply or the reading left some out (more rows than
/// shown, a sweep that hit its cap, or a reading that stopped at its bound of new candidates: a
/// partial reading proves no absence), the oldest of them, whether one is the Entry's, and its
/// links. `None` for a body that is not the leftover view's.
pub(crate) fn from_json(body: &str) -> Option<Leftovers> {
    use aterm_json::Value;
    let doc: Value = aterm_json::from_str(body).ok()?;
    let procs = doc.get("procs")?.as_array()?;
    fn word<'v>(p: &'v Value, key: &str) -> &'v str {
        p.get(key).and_then(Value::as_str).unwrap_or_default()
    }
    let left: Vec<&Value> = procs
        .iter()
        .filter(|p| outlived(word(p, "state"), word(p, "reason")))
        .collect();
    let reading = |key: &str| {
        doc.get("left")
            .and_then(|l| l.get(key))
            .and_then(Value::as_bool)
    };
    let whole = reading("complete") == Some(true) && reading("bounded") == Some(false);
    Some(Leftovers {
        from_instance: true,
        rows: left.len(),
        more: !whole
            || doc
                .get("more")
                .and_then(Value::as_u64)
                .is_some_and(|n| n > 0),
        oldest_ms: left
            .iter()
            .filter_map(|p| p.get("started_ms").and_then(Value::as_u64))
            .max(),
        eligible: left
            .iter()
            .any(|p| p.get("entry").and_then(Value::as_bool) == Some(true)),
        links: doc.get("links").and_then(Value::as_array).map(Vec::len),
    })
}

/// The facts of rows a reading judged with no tab known (this boot's records and this user's
/// table): its dead verify runs' leftovers alone ([`outlived`]), a floor where the reading was
/// partial, and no link.
pub(crate) fn from_rows(view: &aterm_vitals::left::LeftView) -> Leftovers {
    use aterm_vitals::left::{ENTRY_AFTER_MS, eligible};
    let left: Vec<&aterm_vitals::left::LeftRow> = view
        .rows
        .iter()
        .filter(|r| outlived(r.state.word(), r.why.word()))
        .collect();
    Leftovers {
        from_instance: false,
        rows: left.len(),
        more: view.bounded || !view.complete,
        oldest_ms: left.iter().filter_map(|r| r.started_ms).max(),
        eligible: left.iter().any(|r| eligible(r, view, ENTRY_AFTER_MS)),
        links: None,
    }
}

/// What asking the running instance found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Asked {
    /// No instance takes a connection: none runs.
    NoInstance,
    /// Its answer's body, the line after its `OK … view=left` header.
    View(String),
    /// An instance takes connections but did not answer its leftover view: why.
    NoView(String),
}

/// Asks the running instance for [`REQUEST`] on a thread of its own, so a connect the kernel
/// holds never holds doctor: what it found, or that it did not answer within
/// [`INSTANCE_BOUND`].
fn ask_instance() -> Asked {
    let silent = || {
        Asked::NoView(format!(
            "the running instance did not answer its leftover view within {} s",
            INSTANCE_BOUND.as_secs()
        ))
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let asking = std::thread::Builder::new()
        .name("doctor-leftovers".to_owned())
        .spawn(move || {
            let _ = tx.send(ask_instance_now());
        });
    if asking.is_err() {
        return silent();
    }
    rx.recv_timeout(INSTANCE_BOUND).unwrap_or_else(|_| silent())
}

/// One `AUTH` and [`REQUEST`] to the front-door instance on one connection, as its owner.
fn ask_instance_now() -> Asked {
    use std::io::{BufRead as _, Read as _, Write as _};
    const MAX_HEADER_BYTES: u64 = 4096;
    const MAX_BODY_BYTES: u64 = 4 << 20;
    let Some(sock) = aterm_ctl::front_door_instance() else {
        return Asked::NoInstance;
    };
    let not = |what: &str| Asked::NoView(format!("the running instance {what}"));
    let Ok((stream, auth)) = aterm_ctl::connect_then_read_token(&sock) else {
        return not("did not take the request");
    };
    let Ok(auth) = auth else {
        return not("did not let doctor in");
    };
    let asked = stream
        .set_write_timeout(Some(INSTANCE_BOUND))
        .and_then(|()| stream.set_read_timeout(Some(INSTANCE_BOUND)))
        .and_then(|()| (&stream).write_all(format!("AUTH {auth}\n{REQUEST}\n").as_bytes()));
    if asked.is_err() {
        return not("did not take the request");
    }
    let mut reader = std::io::BufReader::new(&stream);
    let mut header = String::new();
    if (&mut reader)
        .take(MAX_HEADER_BYTES + 1)
        .read_line(&mut header)
        .is_err()
        || !header.ends_with('\n')
        || !header.starts_with("OK ")
        || !header.contains(" view=left")
    {
        return not("answered no leftover view");
    }
    let mut body = String::new();
    if (&mut reader)
        .take(MAX_BODY_BYTES + 1)
        .read_line(&mut body)
        .is_err()
        || body.len() as u64 > MAX_BODY_BYTES
    {
        return not("did not answer its leftover view");
    }
    Asked::View(body)
}

/// The row's facts from what asking the instance found: its view while one answers; with none
/// running, `table` (this boot's records and this user's table); an instance that runs but did
/// not answer its view leaves the row not measured, since its closed tabs and links are read
/// nowhere else.
pub(crate) fn fact_of(asked: Asked, table: impl FnOnce() -> LeftoversFact) -> LeftoversFact {
    match asked {
        Asked::NoInstance => table(),
        Asked::View(body) => from_json(&body)
            .ok_or_else(|| "the running instance's answer is not its leftover view".to_owned()),
        Asked::NoView(why) => Err(why),
    }
}

/// What a table read takes: the kernel's reads, one sweep of the process table and whether it
/// listed every process, the folder this boot's verify run records are in (`None`: this user's
/// rendezvous is unknown), this boot, this process and now.
pub(crate) struct TableReads<'a> {
    pub(crate) kernel: &'a dyn aterm_uds::left::LeftKernel,
    pub(crate) sweep: &'a [aterm_sysprobe::procs::ProcessReading],
    pub(crate) complete: bool,
    pub(crate) records: Option<&'a Path>,
    pub(crate) boot: Option<&'a str>,
    pub(crate) me: u32,
    pub(crate) now_ms: u64,
}

/// This boot's verify run records and this user's rows of the sweep, judged as a reading that
/// knows no tab ([`aterm_uds::left::Reader`]), the first `dead_seen` observation recorded in a
/// record whose launcher this reading finds dead and that has none, as every reader records it
/// ([`aterm_uds::left::append_dead_seen`]). A dead run ties its members only by a bound its
/// record keeps, so a reading that recorded one is read again, over the records with the pairs
/// they now keep (never a proposed bound that was not recorded). Not measured where this
/// process's own lineage, this user's rows or the records do not read: a reading without them
/// proves no absence.
pub(crate) fn table_fact(t: &TableReads<'_>) -> LeftoversFact {
    let me = i32::try_from(t.me).map_err(|_| "this process's pid is out of range".to_owned())?;
    if t.kernel.lineage(me).is_none() {
        return Err("this process's lineage did not read, so no process could be judged".into());
    }
    let rows = aterm_uds::left::rows_of(t.sweep, t.me);
    if rows.is_empty() {
        return Err("this user's processes could not be listed".to_owned());
    }
    let dir = t
        .records
        .ok_or_else(|| "this user's rendezvous is unknown, so no run record was read".to_owned())?;
    let mut records = aterm_uds::runs::read_records(dir)
        .map_err(|why| format!("this boot's verify run records did not read ({why})"))?;
    let read = |records: &[(PathBuf, aterm_uds::runs::Record)]| {
        let known = aterm_uds::left::Known {
            self_pid: t.me,
            now_ms: t.now_ms,
            boot: t.boot,
            records,
            held: &[],
            waiting: &[],
            shells: &[],
        };
        aterm_uds::left::Reader::new().read(t.kernel, &rows, &known)
    };
    let mut reading = read(&records);
    let unrecorded = |reading: &aterm_uds::left::Reading| {
        reading
            .runs
            .iter()
            .filter(|r| r.record.dead_seen.is_none())
            .count()
    };
    let before = unrecorded(&reading);
    aterm_uds::left::append_dead_seen(t.kernel, t.me, &mut reading.runs, t.now_ms);
    if unrecorded(&reading) < before {
        for run in reading.runs.iter().filter(|r| r.record.dead_seen.is_some()) {
            if let Some((_, rec)) = records
                .iter_mut()
                .find(|(path, rec)| *path == run.path && rec.dead_seen.is_none())
            {
                rec.dead_seen = run.record.dead_seen;
                rec.dead_seen_ms = run.record.dead_seen_ms;
            }
        }
        reading = read(&records);
    }
    let spared = [t.me];
    let ctx = aterm_vitals::left::Ctx {
        launchd: reading.launchd,
        live: &[],
        closed: &[],
        spared: &spared,
        tellers: &[],
    };
    let view = aterm_vitals::left::LeftView {
        rows: reading
            .facts
            .iter()
            .filter_map(|f| aterm_vitals::left::judge(f, &ctx))
            .collect(),
        runs: reading
            .runs
            .iter()
            .map(|run| aterm_vitals::left::RunRow {
                id: run.record.id.map(|i| i.text()).unwrap_or_default(),
                dead: run.dead,
                dead_ms: run
                    .record
                    .dead_seen_ms
                    .filter(|_| run.dead)
                    .map(|ms| t.now_ms.saturating_sub(ms)),
                sid: run.record.sid.clone(),
                roots: run
                    .record
                    .roots
                    .iter()
                    .map(|r: &PathBuf| r.display().to_string())
                    .collect(),
                ..aterm_vitals::left::RunRow::default()
            })
            .collect(),
        complete: t.complete,
        bounded: reading.bounded,
        as_of_ms: t.now_ms,
        ..aterm_vitals::left::LeftView::default()
    };
    Ok(from_rows(&view))
}

/// [`table_fact`] over the kernel's readers, one sweep, and this user's rendezvous.
fn read_table() -> LeftoversFact {
    let sweep = aterm_sysprobe::procs::processes()
        .ok_or_else(|| "this user's processes could not be listed".to_owned())?;
    let records = aterm_uds::control_socket_dir().map(|dir| {
        aterm_uds::runs::records_dir(&dir, aterm_uds::process::pid_namespace().as_deref())
    });
    let boot = aterm_uds::process::boot_id();
    let kernel = aterm_uds::left::Live {
        writer: |pid| {
            aterm_uds::control_socket_dir().is_some_and(|dir| aterm_ctl::graph_writer(&dir, pid))
        },
    };
    table_fact(&TableReads {
        kernel: &kernel,
        sweep: &sweep.procs,
        complete: !sweep.truncated,
        records: records.as_deref(),
        boot: boot.as_deref(),
        me: std::process::id(),
        now_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
    })
}

/// The row's live read: the running instance's view, else, with none running, the table.
pub(crate) fn fact() -> LeftoversFact {
    fact_of(ask_instance(), read_table)
}

#[cfg(test)]
#[path = "doctor_leftovers_tests.rs"]
mod tests;
