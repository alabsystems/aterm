// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The window's collection of what departed work leaves on this Mac: the release-line records no
//! reader needs ([`crate::release_progress::collect`]), and the journals of a session gone from
//! every window's list for [`STATE_KEEP_S`] in `<aterm state>/drive`: its ledger (`<stem>.jsonl`,
//! [`crate::supervise::approvals::ledger_under`]), its journal (`<stem>.journal.jsonl`,
//! [`crate::supervise::approvals::journal_beside`]), their sibling locks and its earlier-tabs
//! note.
//!
//! A session has left only where no window of this state root holds it: the sweep's own graph
//! says Gone ([`registry_life`]: it vouches for every tab this window knows live and holds no
//! entry for it), and so does the graph of every other window the state root records as running
//! ([`graph_records::live`], [`life_across`]). A record that cannot be read, or a recorded graph
//! that is no folder, makes every tab unknown, and a graph that cannot be read for a tab makes
//! that tab unknown. The pass that first sees a stem gone writes `<stem>.left`, holding that
//! wall second, this boot's id and the seconds this boot has run through sleep ([`Now`]); one
//! that sees it live, or cannot tell, removes the marker, so the wait starts again. The stem's
//! files go once the marker is [`STATE_KEEP_S`] old ([`kept_for`]: by the wall clock, and within
//! the boot that wrote it by the clock that counts through sleep as well), none of them was
//! written after it, and no session that is not gone may still read them ([`needed`]).
//!
//! Where this host names no process start, machine, boot id or clock that counts through sleep
//! (Windows), no window records itself and nothing in the drive folder is collected: a window
//! there could not tell another window's graph (an elevated one's) from none.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use aterm_types::control_request::Sid;

use super::{
    LiveTab, Opts, STATE_KEEP_S, St, TabLife, check_owned_plain_dir_existing, check_upgrade_tree,
    read_record_text, read_upgrade_dir, registry_graph, registry_life,
};
use crate::harness::graph_records;

/// The marker of a stem the sweep saw gone: `<stem>.left`, holding the [`Now`] it first did.
const LEFT: &str = ".left";

/// Every name a session's stem takes in the drive folder, a longer suffix before one it ends
/// with, so a journal is never read as a ledger.
const KINDS: [&str; 6] = [
    ".journal.jsonl.lock",
    ".jsonl.lock",
    ".journal.jsonl",
    ".jsonl",
    ".earlier-tabs",
    LEFT,
];

/// The stem's files whose write after its marker says the session came back.
const WRITTEN: [&str; 3] = [".jsonl", ".journal.jsonl", ".earlier-tabs"];

/// The most bytes of the upgrade ledger read for the tabs it pairs, the bound its own readers
/// read it under (`typed_through`).
const LEDGER_BYTES: u64 = 16 * 1024 * 1024;

/// How often one process collects: at most once an hour, whatever the sweeps between.
const COLLECT_EVERY_S: u64 = 3600;

/// When this process last collected ([`sweep`]'s gate).
pub(super) static COLLECTED_AT: AtomicU64 = AtomicU64::new(0);

/// The window's collection, from its sweep under the sweep lock (`sweep_with_lanes`) at wall
/// second `now`: nothing for a dry run, a background sweep or one that knows no live tabs (the
/// CLI's), and otherwise, at most once every [`COLLECT_EVERY_S`] by the gate `at` ([`due`]), the
/// release-line records of `opts.state` no reader needs, then, where this host reads a [`Now`],
/// one [`collect_across`] over the drive folder of `opts.aterm_state`, from this window's own
/// graph ([`registry_life`]).
pub(super) fn sweep(opts: &Opts, live_tabs: Option<&[LiveTab]>, now: u64, at: &AtomicU64) {
    let Some(live_tabs) = live_tabs.filter(|_| !opts.dry_run && !opts.background) else {
        return;
    };
    if !due(at, now) {
        return;
    }
    crate::release_progress::collect(&opts.state, now);
    let (Some(root), Some(readings)) = (opts.aterm_state.as_deref(), Now::read(now)) else {
        return;
    };
    let known_live: Vec<String> = live_tabs.iter().map(|tab| tab.sid.clone()).collect();
    let graph = registry_graph();
    collect_across(
        root,
        &registry_life(graph.as_deref(), &known_live),
        &known_live,
        &|| groups(&opts.state),
        &readings,
    );
}

/// One [`pass`] over `<root>/drive` at `now`, each tab judged by `own` (this window's graph)
/// widened to the graphs of every running window recorded beside those journals
/// ([`graph_records::drive_folder`], [`graph_records::live`], [`life_across`]); the spent
/// records (of windows proven ended, or abandoned) are removed first.
pub(super) fn collect_across(
    root: &Path,
    own: &dyn Fn(&str) -> TabLife,
    known_live: &[String],
    groups: &dyn Fn() -> Option<Vec<BTreeSet<String>>>,
    now: &Now,
) -> Pass {
    let others = graph_records::live(&graph_records::drive_folder(root), now.wall);
    pass(root, &life_across(own, others), known_live, groups, now)
}

/// Whether a collection is due at `now` by the gate `at`: [`COLLECT_EVERY_S`] after the second
/// it holds. A gate that holds none yet (0), or a second after `now` (a clock that went back),
/// is set to `now` and nothing is due, so a window's first collection comes an hour after its
/// first sweep, once the tabs it restores have taken their places in the graph. Taking it moves
/// the gate, so of two sweeps at once one collects.
fn due(at: &AtomicU64, now: u64) -> bool {
    let last = at.load(Ordering::SeqCst);
    if last == 0 || last > now {
        let _ = at.compare_exchange(last, now, Ordering::SeqCst, Ordering::SeqCst);
        return false;
    }
    now - last >= COLLECT_EVERY_S
        && at
            .compare_exchange(last, now, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
}

/// `own`'s verdict on a tab (this window's graph, [`registry_life`]), widened to the graphs the
/// other running windows of its state root publish in (`others`, [`graph_records::live`]): a
/// tab `own` finds gone is live where any of them holds it, unknown where one of them cannot be
/// read, and gone only where none holds it. With no reading of the records (`None`), every tab
/// is unknown.
pub(super) fn life_across<'a>(
    own: impl Fn(&str) -> TabLife + 'a,
    others: Option<Vec<PathBuf>>,
) -> impl Fn(&str) -> TabLife + 'a {
    move |tab: &str| {
        let Some(others) = &others else {
            return TabLife::Unknown;
        };
        let seen = own(tab);
        if seen != TabLife::Gone {
            return seen;
        }
        let mut unread = false;
        for graph in others {
            match graph_records::holds(graph, tab) {
                Some(true) => return TabLife::Live,
                Some(false) => {}
                None => unread = true,
            }
        }
        if unread {
            TabLife::Unknown
        } else {
            TabLife::Gone
        }
    }
}

/// The readings a marker holds and a pass judges its age at: the wall clock's unix second, this
/// boot's id ([`aterm_uds::process::boot_id`]: macOS's `kern.bootsessionuuid`, Linux's
/// `boot_id`) and the whole seconds this boot has run, sleep included
/// ([`aterm_uds::process::continuous_ns`]: `mach_continuous_time`, `CLOCK_BOOTTIME`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Now {
    pub(super) wall: u64,
    pub(super) boot: String,
    pub(super) since_boot: u64,
}

impl Now {
    /// This host's readings at wall second `wall`: `None` where it names no boot id or has no
    /// clock that counts through sleep.
    fn read(wall: u64) -> Option<Self> {
        Some(Self {
            wall,
            boot: aterm_uds::process::boot_id()?,
            since_boot: aterm_uds::process::continuous_ns()? / 1_000_000_000,
        })
    }

    /// The marker's text: the wall second, then the boot id and its seconds, each line ended.
    fn text(&self) -> String {
        format!("{}\n{} {}\n", self.wall, self.boot, self.since_boot)
    }

    /// The readings a marker's `text` holds: `None` for any other text, a marker that holds the
    /// wall second alone included.
    fn parse(text: &str) -> Option<Self> {
        let (wall, rest) = text.strip_suffix('\n')?.split_once('\n')?;
        let (boot, since_boot) = rest.split_once(' ')?;
        if boot.is_empty() || !boot.bytes().all(|b| b.is_ascii_graphic()) {
            return None;
        }
        Some(Self {
            wall: crate::parse_uint(wall)?,
            boot: boot.to_owned(),
            since_boot: crate::parse_uint(since_boot)?,
        })
    }
}

/// Whether a marker written at `left` has stood [`STATE_KEEP_S`] at `now`: by the wall clock,
/// and, where `now` is in the boot that wrote it, by the clock that counts through sleep as well,
/// so a wall clock set forward within a boot never ages a marker. Across a boot the wall clock
/// alone decides. `None` where the marker cannot stand: a wall second after `now`'s, or a reading
/// of its own boot after this one's.
fn kept_for(left: &Now, now: &Now) -> Option<bool> {
    let wall = now.wall.checked_sub(left.wall)?;
    if left.boot != now.boot {
        return Some(wall >= STATE_KEEP_S);
    }
    let since_boot = now.since_boot.checked_sub(left.since_boot)?;
    Some(wall >= STATE_KEEP_S && since_boot >= STATE_KEEP_S)
}

/// What one [`pass`] did, by stem.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Pass {
    /// Stems seen gone whose marker this pass wrote: where there was none, or in place of one
    /// that could not stand (another format, a time after this pass's, or files written after
    /// it), whose wait starts again now.
    pub(super) marked: Vec<String>,
    /// Stems whose marker this pass removed: seen live or unknown.
    pub(super) unmarked: Vec<String>,
    /// Stems whose files this pass removed.
    pub(super) collected: Vec<String>,
}

/// One pass over `<root>/drive` at `now`, each session judged by `life`: a gone stem's marker
/// written where it has none, or written again where its marker cannot stand; a marker removed
/// where its stem is not gone; and a stem whose marker has stood [`STATE_KEEP_S`]
/// ([`kept_for`]) collected, unless [`needed`] names it from `known_live` and the tabs `groups`
/// pairs (both read only where a stem is due; where either cannot be read, nothing is
/// collected). A stem with any entry that is no regular file is left as it is. The marker and
/// the files' times are read again just before each stem's files go.
pub(super) fn pass(
    root: &Path,
    life: &dyn Fn(&str) -> TabLife,
    known_live: &[String],
    groups: &dyn Fn() -> Option<Vec<BTreeSet<String>>>,
    now: &Now,
) -> Pass {
    let mut out = Pass::default();
    let drive = root.join("drive");
    if !matches!(check_owned_plain_dir_existing(root), Ok(true))
        || !matches!(check_owned_plain_dir_existing(&drive), Ok(true))
    {
        return out;
    }
    let mut standing: BTreeSet<String> = known_live.iter().cloned().collect();
    let mut due = Vec::new();
    for (stem, plain) in stems(&drive) {
        let seen = life(&stem);
        if seen != TabLife::Gone {
            standing.insert(stem.clone());
        }
        if !plain {
            continue;
        }
        let marker = drive.join(format!("{stem}{LEFT}"));
        let left = left_at(&marker);
        if seen != TabLife::Gone {
            if left != Ok(None) && remove_regular(&marker) {
                out.unmarked.push(stem);
            }
            continue;
        }
        let stands = match &left {
            Ok(Some(left)) if !written_after(&drive, &stem, left.wall) => {
                kept_for(left, now).map(|kept| (left.clone(), kept))
            }
            _ => None,
        };
        match (left, stands) {
            (Ok(None), _) => {
                if mark(&marker, now).is_ok() {
                    out.marked.push(stem);
                }
            }
            (_, Some((left, true))) => due.push((stem, left)),
            (_, Some((_, false))) => {}
            (_, None) => {
                if remove_regular(&marker) && mark(&marker, now).is_ok() {
                    out.marked.push(stem);
                }
            }
        }
    }
    if due.is_empty() {
        return out;
    }
    let Some(groups) = groups() else {
        return out;
    };
    for group in &groups {
        standing.extend(
            group
                .iter()
                .filter(|tab| life(tab) != TabLife::Gone)
                .cloned(),
        );
    }
    let Some(needed) = needed(root, standing, &groups) else {
        return out;
    };
    for (stem, left) in due {
        let marker = drive.join(format!("{stem}{LEFT}"));
        if needed.contains(&stem)
            || left_at(&marker) != Ok(Some(left.clone()))
            || written_after(&drive, &stem, left.wall)
        {
            continue;
        }
        if collect_stem(&drive, &stem) {
            out.collected.push(stem);
        }
    }
    out
}

/// Each stem of the drive folder `drive` that names a session ([`Sid::parse`]: never `self`, a
/// tape or a witness), with whether every entry of it is a regular file.
fn stems(drive: &Path) -> BTreeMap<String, bool> {
    let mut stems = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(drive) else {
        return stems;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(stem) = name
            .to_str()
            .and_then(|name| KINDS.iter().find_map(|kind| name.strip_suffix(kind)))
            .filter(|stem| Sid::parse(stem).is_some())
        else {
            continue;
        };
        let plain = entry.file_type().is_ok_and(|kind| kind.is_file());
        *stems.entry(stem.to_owned()).or_insert(true) &= plain;
    }
    stems
}

/// Write the marker at `path`, holding `now` ([`Now::text`]): a new file of mode 0600, never one
/// over what is there (a link included). A marker cut short does not parse ([`left_at`]), and
/// the next pass writes it again.
///
/// # Errors
/// Something is there already, or the create or the write failed.
fn mark(path: &Path, now: &Now) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        open.mode(0o600)
            .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS);
    }
    open.open(path)?.write_all(now.text().as_bytes())
}

/// The readings the marker at `path` holds: `Ok(None)` where there is none, `Err(())` where it
/// is no small regular file holding a marker's text ([`Now::parse`]).
fn left_at(path: &Path) -> Result<Option<Now>, ()> {
    match crate::harness::accounts::read_bounded(path, 256) {
        Ok(None) => Ok(None),
        Ok(Some(text)) => Now::parse(&text).map(Some).ok_or(()),
        Err(_) => Err(()),
    }
}

/// Whether any of `stem`'s files that a returning session writes ([`WRITTEN`]) was last
/// written after unix second `left`, or holds a time that cannot be read.
fn written_after(drive: &Path, stem: &str, left: u64) -> bool {
    WRITTEN.iter().any(|kind| {
        match std::fs::symlink_metadata(drive.join(format!("{stem}{kind}"))) {
            Ok(meta) => meta
                .modified()
                .ok()
                .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
                .is_none_or(|at| at.as_secs() > left),
            Err(e) => e.kind() != std::io::ErrorKind::NotFound,
        }
    })
}

/// The stems a session that is not gone may still read, from the stems in `standing`: each tab
/// a needed stem's earlier-tabs note names (as `tabs_through` follows them), and every tab of a
/// group (the tabs that read one another's journals, [`groups`]) that holds a needed one, to a
/// fixpoint. The groups pair tabs both ways, so this names at least every stem a reader reaches.
/// `None` where a needed stem's note is there but does not read whole
/// ([`super::noted_earlier_tabs_checked`]): the stems it names cannot be told.
fn needed(
    root: &Path,
    standing: BTreeSet<String>,
    groups: &[BTreeSet<String>],
) -> Option<BTreeSet<String>> {
    let mut needed = standing;
    let mut unread: Vec<String> = needed.iter().cloned().collect();
    loop {
        while let Some(tab) = unread.pop() {
            for earlier in super::noted_earlier_tabs_checked(root, &tab).ok()? {
                if needed.insert(earlier.clone()) {
                    unread.push(earlier);
                }
            }
        }
        for group in groups {
            if group.iter().any(|tab| needed.contains(tab)) {
                for tab in group {
                    if needed.insert(tab.clone()) {
                        unread.push(tab.clone());
                    }
                }
            }
        }
        if unread.is_empty() {
            return Some(needed);
        }
    }
}

/// The tabs that read one another's journals through the upgrade state under `state`, one group
/// per session key: the tabs its record names (its tab, its earlier tabs and those it keeps an
/// end for, as `typed_through` reads them) and every tab a row of the upgrade ledger pairs with
/// it (`ledger_tabs`). `None` where a record or the ledger is there but cannot be read, or a
/// `*.json` there is no record this build reads and no other document ([`another_document`]).
pub(super) fn groups(state: &Path) -> Option<Vec<BTreeSet<String>>> {
    let tab = |tab: &String| Sid::parse(tab).is_some();
    let mut by_session: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let upgrade = match check_upgrade_tree(state, None) {
        Ok(Some(upgrade)) => upgrade,
        Ok(None) => return Some(Vec::new()),
        Err(_) => return None,
    };
    for entry in read_upgrade_dir(state).ok()? {
        let entry = entry.ok()?;
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let path = entry.path();
        let Some(session) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".json"))
        else {
            continue;
        };
        let Some(text) = read_record_text(&path).ok()? else {
            continue;
        };
        let Some(st) = St::from_json(&text) else {
            if another_document(session, &text) {
                continue;
            }
            return None;
        };
        by_session.entry(session.to_owned()).or_default().extend(
            std::iter::once(&st.tab)
                .chain(&st.tabs)
                .chain(st.tabs_until.iter().map(|(tab, _)| tab))
                .filter(|t| tab(t))
                .cloned(),
        );
    }
    let ledger =
        crate::harness::accounts::read_bounded(&upgrade.join("ledger.jsonl"), LEDGER_BYTES)
            .ok()?
            .unwrap_or_default();
    for row in ledger.lines() {
        let Ok(row) = aterm_json::from_str::<aterm_json::Value>(row) else {
            continue;
        };
        let (Some(t), Some(session)) = (row["tab"].as_str(), row["session"].as_str()) else {
            continue;
        };
        if session != "-" && Sid::parse(t).is_some() {
            by_session
                .entry(session.to_owned())
                .or_default()
                .insert(t.to_owned());
        }
    }
    Some(
        by_session
            .into_values()
            .filter(|group| !group.is_empty())
            .collect(),
    )
}

/// Whether `text`, read from `<session>.json` in the upgrade folder and no record this build
/// reads, is another document sharing the folder (a restart stamp, a retired writer's state
/// file): an object with no `phase` under a name no record takes, as the status reader tells
/// them apart. One under a record's own name is that record, from another build or damaged.
fn another_document(session: &str, text: &str) -> bool {
    let record_named = crate::harness::upgrade::is_session_id(session)
        || crate::harness::upgrade::Agent::keyed(session).is_some();
    !record_named
        && aterm_json::from_str::<aterm_json::Value>(text)
            .is_ok_and(|value| value.is_object() && value.get("phase").is_none())
}

/// Remove `stem`'s files from `drive`: its ledger and its journal, each with its lock
/// ([`aterm_uds::line_log::retire`]), then its earlier-tabs note, and last its marker. Whether
/// all of it went; one that stops part way keeps the marker, so a later pass finishes.
fn collect_stem(drive: &Path, stem: &str) -> bool {
    [".jsonl", ".journal.jsonl"]
        .iter()
        .all(|kind| aterm_uds::line_log::retire(&drive.join(format!("{stem}{kind}"))).is_ok())
        && remove_regular(&drive.join(format!("{stem}.earlier-tabs")))
        && remove_regular(&drive.join(format!("{stem}{LEFT}")))
}

/// Remove the regular file at `path`, never a link or anything else there: whether nothing is
/// there now.
fn remove_regular(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_file() => std::fs::remove_file(path).is_ok(),
        Ok(_) => false,
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}

#[cfg(all(
    test,
    any(target_vendor = "apple", target_os = "linux", target_os = "android")
))]
#[path = "drive_collect_tests.rs"]
mod tests;
