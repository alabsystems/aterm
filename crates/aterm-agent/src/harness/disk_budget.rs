// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE REBUILDABLE BUDGET: its sizing (`aterm ctl sys disk`'s `budget` record), and the pass
//! that holds the volume to it.
//!
//! - **Sizing.** Which looks may begin one ([`admitted`], [`due`]), the bytes it counts
//!   ([`rebuildable`], [`size`]), and the record of the last one every process of the account
//!   shares ([`Record`], [`claim`], [`keep`]). It takes no build's lock: each tree is sized by
//!   [`target::total_bytes_until`], never judged for removal.
//! - **The pass.** A look the budget admits ([`pass_admitted`]) on a volume a sizing measured
//!   over its cap ([`over`]) takes, at any `disk=` level, tier 1, 2 and 3 rows only, and only
//!   those [`ask`] makes eligible: a build directory no live session works in, started a build
//!   in or used in its recent runs ([`Live`]; an agent's `CARGO_TARGET_DIR` and a live Claude
//!   Code session's scratch build never). It pages its search across looks ([`Run`],
//!   [`BUDGET_PAGES`]) and ends each page by what was measured ([`end_of`]): met, stood down,
//!   unfinished (a row refused counts as one not reached), or unable to meet the target.
//! - **The one row.** A pass that cannot meet its target says so once for a volume and cap
//!   ([`UNMET_FILE`], [`unmet`]), and plans again in that episode only when the rebuildable
//!   bytes grow, the live sessions change, or [`REPLAN_AFTER_S`] has passed since its last pass,
//!   in which a hold may have lapsed ([`plans`]). The row is a record: nothing here raises,
//!   holds a build or tells one anything.
//! - **The entry-bound pass.** A look [`bound_admitted`] admits, at any `disk=` level and with
//!   the budget off too, plans one tier 3 directory over the entry bound by the same
//!   eligibility ([`bound_ask`], [`Admit::EntryBound`]); `disk.rs`'s `EntryBound` says which
//!   directories are over it.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

use aterm_json::{Map, Value};
use aterm_sysprobe::volumes::Table;

use super::{
    Applied, Budget, Cap, Hog, Level, MAX_TARGETS, Rebuildable, StopBy, Survey, TargetDir,
    collapse_nested_holders, discover, idle, serves_of, target,
};

/// The least time, in seconds, from the start of one sizing to the start of the next, across
/// every process of the account ([`due`], [`claim`]).
pub const EVERY_S: i64 = 3600;

/// The one wall-clock bound on a whole sizing: the search ([`discover::Budget::SHIPPED`]'s own
/// wall within it) and every size walk. A sizing that meets it stops and is a floor.
pub const WALL: Duration = Duration::from_secs(120);

/// The record of the last sizing, in the harness state directory ([`Record`]).
pub const FILE: &str = "disk-budget.json";

/// The most bytes of [`FILE`] read: one record of a few numbers.
const FILE_MAX: u64 = 4096;

/// Whether a look of the disk watch may begin a sizing: the volume reads `ok`, this process is
/// the account's own watch (`owner`), no agent's demand and no fill projection stands (those
/// looks belong to a pass), and the budget is not off. The hourly rule is [`due`]'s.
#[must_use]
pub fn admitted(level: Level, owner: bool, demand: bool, projected: bool, cap: Cap) -> bool {
    level == Level::Ok && owner && !demand && !projected && cap != Cap::Off
}

/// Whether a sizing is due at unix second `now`: none began (`began` is `None`), the last
/// began [`EVERY_S`] or more ago, or the clock reads before it (a clock set back does not hold
/// the budget unsized for as long as it was set back).
#[must_use]
pub fn due(began: Option<i64>, now: i64) -> bool {
    began.is_none_or(|at| now < at || now.saturating_sub(at) >= EVERY_S)
}

/// The rebuildable bytes the volume holds now: each registered build cache under `roots`'s
/// home, its shared dependency stores (`atpkg::build_cache::shared_units::roots`) and each build directory a search from `roots` finds (under `search`), those
/// `same_volume` places on the volume, sized by [`size`] before `deadline`, a registered cache
/// counted only where its registration is still current once it was sized (as the report's
/// holders are). The figure is a floor (`partial`) where the search was cut short, too.
#[must_use]
pub fn rebuildable(
    roots: &discover::Roots,
    search: discover::Budget,
    deadline: Instant,
    same_volume: &dyn Fn(&Path) -> bool,
) -> Rebuildable {
    let _exposed = discover::expose_placeholders().ok();
    let wall = deadline
        .saturating_duration_since(Instant::now())
        .min(search.wall);
    let found = discover::discover(roots, discover::Budget { wall, ..search });
    let home = roots.home.as_deref();
    let registered = home.map_or_else(Vec::new, atpkg::build_cache::registered::roots);
    let stores = home.map_or_else(Vec::new, atpkg::build_cache::shared_units::roots);
    let caches: Vec<PathBuf> = home.map_or_else(Vec::new, |home| {
        registered
            .iter()
            .filter(|cache| cache.is_current(home))
            .map(|cache| cache.root.clone())
            .chain(
                stores
                    .iter()
                    .filter(|store| std::fs::symlink_metadata(store).is_ok_and(|m| m.is_dir()))
                    .cloned(),
            )
            .collect()
    });
    let current = |root: &Path| {
        stores.iter().any(|store| store == root)
            || home.is_some_and(|home| {
                registered
                    .iter()
                    .find(|cache| cache.root == root)
                    .is_some_and(|cache| cache.is_current(home))
            })
    };
    let dirs: Vec<PathBuf> = found.found.into_iter().map(|f| f.path).collect();
    let mut sized = size(&caches, &dirs, same_volume, deadline, &current);
    sized.partial |= found.cut.is_some();
    sized
}

/// The bytes under `caches` and `dirs` that `same_volume` places on the volume, each tree
/// sized once ([`target::total_bytes_until`], on the tree's own device) before `deadline`:
/// the caches first, a path inside a tree already sized whole is not walked again, and a
/// tree nested in another is counted in the outer one ([`collapse_nested_holders`]). A cache
/// that `current` no longer holds once it was sized (its registration changed during the walk)
/// is not counted, and the figure is then a floor; so it is where a walk or the whole met
/// `deadline` or its entry bound.
#[must_use]
pub fn size(
    caches: &[PathBuf],
    dirs: &[PathBuf],
    same_volume: &dyn Fn(&Path) -> bool,
    deadline: Instant,
    current: &dyn Fn(&Path) -> bool,
) -> Rebuildable {
    let mut by: BTreeMap<PathBuf, Hog> = BTreeMap::new();
    let mut partial = false;
    for (at, path) in caches.iter().chain(dirs).enumerate() {
        if by.contains_key(path)
            || by
                .values()
                .any(|h| !h.bytes_partial && path.starts_with(&h.path))
            || !same_volume(path)
        {
            continue;
        }
        if Instant::now() >= deadline {
            partial = true;
            break;
        }
        let (bytes, bytes_partial) = target::total_bytes_until(path, deadline);
        if at < caches.len() && !current(path) {
            partial = true;
            continue;
        }
        by.insert(
            path.clone(),
            Hog {
                path: path.clone(),
                bytes,
                bytes_partial,
                why: String::new(),
                serves: None,
                built_s_ago: None,
            },
        );
    }
    let mut sized = Rebuildable { bytes: 0, partial };
    for root in collapse_nested_holders(by) {
        sized.bytes = sized.bytes.saturating_add(root.bytes);
        sized.partial |= root.bytes_partial;
    }
    sized
}

/// One sizing: the volume's figures at the instant the rebuildable bytes were counted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sizing {
    /// The volume sized, by its spelled key (`aterm_sysprobe::volumes::VolumeKey::spelled`).
    pub volume: String,
    /// Its size, bytes.
    pub total: u64,
    /// Its used bytes: its size less what this user may still write.
    pub used: u64,
    /// The rebuildable bytes counted, and whether that figure is a floor.
    pub sized: Rebuildable,
    /// Unix second the sizing ended.
    pub sized_unix: i64,
}

/// What [`FILE`] holds: when the last sizing began (the claim every process reads before it
/// begins one) and the last sizing that ended, which a process that starts prints with its
/// true age.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Record {
    /// Unix second the last sizing began.
    pub began_unix: Option<i64>,
    /// The last sizing that ended.
    pub last: Option<Sizing>,
}

impl Record {
    /// The record as [`FILE`] holds it.
    #[must_use]
    pub fn encode(&self) -> String {
        let mut o = Map::new();
        o.insert("schema".to_owned(), Value::from(1u64));
        if let Some(began) = self.began_unix {
            o.insert("began_unix".to_owned(), Value::from(began));
        }
        if let Some(s) = &self.last {
            let mut l = Map::new();
            l.insert("volume".to_owned(), Value::from(s.volume.clone()));
            l.insert("total".to_owned(), Value::from(s.total));
            l.insert("used".to_owned(), Value::from(s.used));
            l.insert("rebuildable".to_owned(), Value::from(s.sized.bytes));
            l.insert("partial".to_owned(), Value::from(s.sized.partial));
            l.insert("sized_unix".to_owned(), Value::from(s.sized_unix));
            o.insert("last".to_owned(), Value::Object(l));
        }
        aterm_json::to_string(&Value::Object(o)).unwrap_or_else(|_| "{}".to_owned())
    }

    /// The record `text` holds; `None` where it is not one ([`Self::encode`]'s schema 1).
    #[must_use]
    pub fn decode(text: &str) -> Option<Self> {
        let v = aterm_json::from_str::<Value>(text).ok()?;
        if v.get("schema").and_then(Value::as_u64) != Some(1) {
            return None;
        }
        let began_unix = match v.get("began_unix") {
            None => None,
            Some(b) => Some(b.as_i64()?),
        };
        let last = match v.get("last") {
            None => None,
            Some(l) => Some(Sizing {
                volume: l.get("volume")?.as_str()?.to_owned(),
                total: l.get("total")?.as_u64()?,
                used: l.get("used")?.as_u64()?,
                sized: Rebuildable {
                    bytes: l.get("rebuildable")?.as_u64()?,
                    partial: l.get("partial")?.as_bool()?,
                },
                sized_unix: l.get("sized_unix")?.as_i64()?,
            }),
        };
        Some(Self { began_unix, last })
    }

    /// The record in `dir`'s [`FILE`]: empty where there is none or it is not one.
    #[must_use]
    pub fn load(dir: &Path) -> Self {
        read(dir).0
    }
}

/// [`FILE`]'s record and its text as read (`None`: no file, or one that could not be read).
fn read(dir: &Path) -> (Record, Option<String>) {
    match aterm_uds::atomic_file::read_regular_text(&dir.join(FILE), FILE_MAX) {
        Ok(text) => (Record::decode(&text).unwrap_or_default(), Some(text)),
        Err(_) => (Record::default(), None),
    }
}

/// What [`claim`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claim {
    /// This process begins the sizing: [`FILE`] now says it began at the claim's second. The
    /// record as it was before.
    Claimed(Record),
    /// A sizing began within [`EVERY_S`] (in this process or another): none is begun. The
    /// record as read.
    NotDue(Record),
}

/// Claim the next sizing at unix second `now` for every process of the account, as the daily
/// cache maintenance stamps its claim (`maintenance::claim`): read [`FILE`] in `dir`, and
/// where a sizing is [`due`] write it back saying one began `now`, the write refused where
/// the file changed since it was read (another process claimed meanwhile). The stamp goes
/// before the walk, so a sizing that fails or is cut short is not begun again within the hour.
///
/// # Errors
///
/// The clock is unread (`now` not above 0), `dir` could not be made, or the write failed or
/// was refused.
pub fn claim(dir: &Path, now: i64) -> io::Result<Claim> {
    if now <= 0 {
        return Err(io::Error::other("the budget's clock is unread"));
    }
    std::fs::create_dir_all(dir)?;
    let (record, text) = read(dir);
    if !due(record.began_unix, now) {
        return Ok(Claim::NotDue(record));
    }
    let claimed = Record {
        began_unix: Some(now),
        ..record.clone()
    };
    write(dir, &claimed, text.as_deref())?;
    Ok(Claim::Claimed(record))
}

/// Keep `sizing`, begun at unix second `began` ([`claim`]), as [`FILE`]'s last sizing in `dir`.
///
/// # Errors
///
/// The file no longer says this sizing's claim (another process claimed since), or the write
/// failed.
pub fn keep(dir: &Path, began: i64, sizing: Sizing) -> io::Result<()> {
    let (record, text) = read(dir);
    if record.began_unix != Some(began) {
        return Err(io::Error::other(
            "another sizing was claimed since this one began",
        ));
    }
    write(
        dir,
        &Record {
            began_unix: Some(began),
            last: Some(sizing),
        },
        text.as_deref(),
    )
}

/// Write `record` to `dir`'s [`FILE`] where the file still reads `was` (`None`: unread).
fn write(dir: &Path, record: &Record, was: Option<&str>) -> io::Result<()> {
    aterm_uds::atomic_file::write_checked(
        &dir.join(FILE),
        record.encode().as_bytes(),
        0o600,
        || {
            if read(dir).1.as_deref() == was {
                Ok(())
            } else {
                Err(io::Error::other(
                    "the budget's record changed while it was written",
                ))
            }
        },
    )
}

/// The most discovery pages one run of the budget pass takes, one a look ([`Run`]): a run that
/// has neither met its target nor run out of rows by its last page ends there, unable to meet
/// it on what its search reached ([`End::CannotMeet`], a floor).
pub const BUDGET_PAGES: u32 = 8;

/// Whether a look may run the budget's pass: this process is the account's own watch
/// (`owner`), no agent's demand and no fill projection stands (those looks belong to the
/// pressure pass), the budget is not off (`cap`), and removal is on (`auto_free_gib`; an
/// unreadable `[disk]` reads as 0). The volume's level is not read: the pass runs at any.
#[must_use]
pub fn pass_admitted(
    owner: bool,
    demand: bool,
    projected: bool,
    cap: Cap,
    auto_free_gib: u64,
) -> bool {
    bound_admitted(owner, demand, projected, auto_free_gib) && cap != Cap::Off
}

/// Whether a look may run the entry-bound pass ([`bound_ask`]): this process is the account's
/// own watch (`owner`), no agent's demand and no fill projection stands (those looks belong to
/// the pressure pass), and removal is on (`auto_free_gib`; an unreadable `[disk]` reads as 0).
/// Neither the volume's level nor the budget's cap is read: it runs at any level, the budget off
/// too. The hourly rule is [`due`]'s.
#[must_use]
pub fn bound_admitted(owner: bool, demand: bool, projected: bool, auto_free_gib: u64) -> bool {
    owner && !demand && !projected && auto_free_gib > 0
}

/// A volume over its cap, as one sizing and a measure of free space now read it ([`over`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Over {
    /// The free bytes at which the volume's used bytes are back at the cap: its size less
    /// [`Budget::cap`]. Every row of the pass stops there.
    pub free_target: u64,
    /// The budget the sizing reads under the cap.
    pub budget: Budget,
}

/// Whether the volume `volume` (its spelled key) with `free` bytes free now is over its cap,
/// by `sizing`: `None` where the sizing is of another volume or counted no rebuildable byte
/// (nothing to take, or a volume it could not place), the budget is off, or free space is at
/// or above [`Over::free_target`]. The size is the sizing's.
#[must_use]
pub fn over(sizing: &Sizing, cap: Cap, volume: &str, free: u64) -> Option<Over> {
    if sizing.volume != volume || sizing.sized.bytes == 0 {
        return None;
    }
    let budget = cap.budget(sizing.total, sizing.used, sizing.sized.bytes)?;
    let free_target = sizing.total.saturating_sub(budget.cap);
    (free < free_target).then_some(Over {
        free_target,
        budget,
    })
}

/// THE LIVE SESSIONS a budget pass keeps the builds of, as one look read them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Live {
    /// The ids of the live sessions.
    pub sessions: BTreeSet<String>,
    /// The build-start record's lines of those sessions ([`atpkg::build_start`]).
    pub starts: Vec<atpkg::build_start::Start>,
    /// The build-start record holds bytes but no line could be read from it.
    pub starts_unknown: bool,
    /// Each live session's working directory.
    pub cwds: Vec<PathBuf>,
    /// Why the working directory of some live session is not known.
    pub cwd_unknown: Option<String>,
}

impl Live {
    /// The live `sessions`, their working directories `cwds` (`cwd_unknown`: why one is not
    /// known) and the lines of the build-start record ([`atpkg::build_start::record`]) those
    /// sessions wrote ([`Self::from_record`]).
    #[must_use]
    pub fn read(
        sessions: impl IntoIterator<Item = String>,
        cwds: Vec<PathBuf>,
        cwd_unknown: Option<String>,
    ) -> Self {
        Self::from_record(
            sessions,
            cwds,
            cwd_unknown,
            atpkg::build_start::record().as_deref(),
        )
    }

    /// [`Self::read`] over the build-start record at `record` (`None`: no state folder, so no
    /// record): the lines the live `sessions` wrote, each also as the scan names a build
    /// directory where that is spelled otherwise ([`resolved`]). A record that holds bytes and
    /// reads no line leaves the pass blind.
    #[must_use]
    pub fn from_record(
        sessions: impl IntoIterator<Item = String>,
        cwds: Vec<PathBuf>,
        cwd_unknown: Option<String>,
        record: Option<&Path>,
    ) -> Self {
        let sessions: BTreeSet<String> = sessions.into_iter().collect();
        let (mut starts, starts_unknown) = record.map_or((Vec::new(), false), |record| {
            let starts = atpkg::build_start::read(record);
            let unknown = starts.is_empty() && std::fs::metadata(record).is_ok_and(|m| m.len() > 0);
            (starts, unknown)
        });
        starts.retain(|start| sessions.contains(&start.session));
        let respelled: Vec<_> = starts.iter().filter_map(resolved).collect();
        starts.extend(respelled);
        Self {
            sessions,
            starts,
            starts_unknown,
            cwds,
            cwd_unknown,
        }
    }

    /// A digest of the live session ids and working directories, each set read sorted: what
    /// [`plans`] compares to say the live sessions changed.
    #[must_use]
    pub fn digest(&self) -> u64 {
        live_digest(
            self.sessions.iter().map(String::as_str),
            self.cwds.iter().map(PathBuf::as_path),
        )
    }

    /// Whether what keeps a live session's builds could not be read whole: no pass starts.
    #[must_use]
    pub fn blind(&self) -> bool {
        self.starts_unknown || self.cwd_unknown.is_some()
    }
}

/// `start` with its workspace and build folder resolved (links followed, `/tmp` read as
/// `/private/tmp`), as the scan names a build directory, where that spells either otherwise;
/// `None` where both read the same or neither resolves. A path that does not resolve is kept
/// as its build spelled it.
fn resolved(start: &atpkg::build_start::Start) -> Option<atpkg::build_start::Start> {
    let real = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let respelled = atpkg::build_start::Start {
        workspace: real(&start.workspace),
        build: start.build.as_deref().map(real),
        ..start.clone()
    };
    (respelled != *start).then_some(respelled)
}

/// Why the build directory `t` stays out of a budget pass, or `None` where it may go.
fn held(live: &Live, t: &TargetDir, table: Option<&Table>, now: i64) -> Option<String> {
    use discover::{Origin, Owned};
    if matches!(t.origin, Origin::AgentTargetDir) {
        return Some("an agent's CARGO_TARGET_DIR".to_owned());
    }
    if matches!(t.origin, Origin::ClaudeScratch { .. }) && matches!(t.owned, Owned::Live(_)) {
        return Some("a live Claude Code session's scratch build".to_owned());
    }
    // Reached through a link from a scratch build or a cache mirror, or found from both: the
    // folder it lies in names no checkout, and a live session's scratch may build into it.
    if matches!(t.origin, Origin::Linked { .. }) {
        return Some("reached through a link, so whose builds use it is not known".to_owned());
    }
    let checkout = serves_of(&t.origin, &t.path)
        .or_else(|| t.path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| t.path.clone());
    kept(live, &t.path, &checkout, table, now)
}

/// Why the build directory `dir` of `checkout` is a live session's, or `None`: a session works
/// in either, a live session's build started in either (its workspace or its build folder
/// nested with them, either way), or its runs used it within
/// [`atpkg::build_cache::stale::KEPT_FOR`] of `now` (unix seconds), as its
/// [`atpkg::build_cache::stale::USED`] record's time says. Any of it unread keeps it.
fn kept(
    live: &Live,
    dir: &Path,
    checkout: &Path,
    table: Option<&Table>,
    now: i64,
) -> Option<String> {
    if live.starts_unknown {
        return Some("the build-start record could not be read".to_owned());
    }
    let sessions = idle::Sessions {
        cwds: live.cwds.clone(),
        unknown: live.cwd_unknown.clone(),
    };
    if let Err(why) = sessions.clear(checkout, dir, table) {
        return Some(why);
    }
    let nested = |a: &Path, b: &Path| a.starts_with(b) || b.starts_with(a);
    if let Some(start) = live.starts.iter().find(|start| {
        live.sessions.contains(&start.session)
            && (start
                .build
                .as_deref()
                .is_some_and(|build| nested(build, dir))
                || nested(&start.workspace, checkout))
    }) {
        return Some(format!(
            "live session {} started a build of {} here",
            start.session,
            start.workspace.display()
        ));
    }
    used_within(dir, now)
}

/// Why `dir`'s runs keep it ([`atpkg::build_cache::stale::USED`] written within
/// [`atpkg::build_cache::stale::KEPT_FOR`] of `now`, or its time unread), or `None`.
fn used_within(dir: &Path, now: i64) -> Option<String> {
    let record = dir.join(atpkg::build_cache::stale::USED);
    let modified = match std::fs::symlink_metadata(&record) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
        Err(e) => {
            return Some(format!(
                "the record of its recent runs could not be read: {e}"
            ));
        }
        Ok(meta) => meta.modified(),
    };
    let Some(at) = modified
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_secs()).ok())
    else {
        return Some("the time its runs last used it could not be read".to_owned());
    };
    let keep = i64::try_from(atpkg::build_cache::stale::KEPT_FOR.as_secs()).unwrap_or(i64::MAX);
    let age = now.saturating_sub(at);
    (age < keep).then(|| {
        format!(
            "its runs used it {} ago, inside the {} their record keeps",
            target::human_age(age.max(0)),
            target::human_age(keep)
        )
    })
}

/// Whether a budget pass keeps the shared-cache build directory `leaf` of `workspace` (the
/// closure [`atpkg::build_cache::reclaim_over_budget`] asks): by the rules [`ask`] applies to
/// every build directory.
#[must_use]
pub fn keeps_leaf(
    live: &Live,
    leaf: &Path,
    workspace: &Path,
    table: Option<&Table>,
    now: i64,
) -> bool {
    live.blind() || kept(live, leaf, workspace, table, now).is_some()
}

/// Which pass an [`Ask`] plans for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admit {
    /// The budget's pass ([`ask`]): every row stops at `free_target` ([`Over::free_target`]).
    Budget {
        /// The free bytes every row of the pass stops at.
        free_target: u64,
    },
    /// The entry-bound pass ([`bound_ask`]): one idle build directory over the entry bound,
    /// whole, with no free-space target.
    EntryBound,
}

impl Default for Admit {
    fn default() -> Self {
        Admit::Budget { free_target: 0 }
    }
}

/// What a budget or entry-bound pass may take ([`ask`], [`bound_ask`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ask {
    /// Which pass plans by it, and where its rows stop.
    pub admit: Admit,
    /// The build directories no live session keeps: the only ones whose rows the pass plans.
    pub eligible: BTreeSet<PathBuf>,
    /// The build directories a live session keeps, each with why.
    pub held: Vec<(PathBuf, String)>,
}

/// Which of `survey`'s build directories a budget pass stopping at `free_target` may take, by
/// `live` at unix second `now` (`table` places the sessions' working directories): an agent's
/// `CARGO_TARGET_DIR` and a live Claude Code session's scratch build never, nor one a live
/// session works in, started a build in or used in its recent runs, nor any while `live` is
/// [`Live::blind`].
#[must_use]
pub fn ask(survey: &Survey, live: &Live, free_target: u64, table: Option<&Table>, now: i64) -> Ask {
    let admit = Admit::Budget { free_target };
    eligibility(survey, live, admit, table, now, |_| true)
}

/// Which of `survey`'s build directories over the entry bound (`over`) the entry-bound pass may
/// take: the one eligibility [`ask`] holds the budget's pass to, by `live` at unix second `now`.
#[must_use]
pub fn bound_ask(
    survey: &Survey,
    over: &BTreeSet<PathBuf>,
    live: &Live,
    table: Option<&Table>,
    now: i64,
) -> Ask {
    let admit = Admit::EntryBound;
    eligibility(survey, live, admit, table, now, |t| over.contains(&t.path))
}

/// The build directories of `survey` that `admits` names, each eligible or held by `live`.
fn eligibility(
    survey: &Survey,
    live: &Live,
    admit: Admit,
    table: Option<&Table>,
    now: i64,
    admits: impl Fn(&TargetDir) -> bool,
) -> Ask {
    let mut ask = Ask {
        admit,
        ..Ask::default()
    };
    for t in survey
        .targets
        .iter()
        .take(MAX_TARGETS)
        .filter(|t| admits(t))
    {
        let why = if live.blind() {
            Some(
                live.cwd_unknown
                    .clone()
                    .unwrap_or_else(|| "the build-start record could not be read".to_owned()),
            )
        } else {
            held(live, t, table, now)
        };
        match why {
            Some(why) => ask.held.push((t.path.clone(), why)),
            None => {
                ask.eligible.insert(t.path.clone());
            }
        }
    }
    ask
}

/// A digest of live session ids and working directories, each set read sorted and once: what
/// [`plans`] compares to say the live sessions changed ([`Live::digest`]), and what an
/// emergency pass that could not meet its line waits on (`super::cadence::Unmet`).
#[must_use]
pub fn live_digest<'a>(
    sessions: impl IntoIterator<Item = &'a str>,
    cwds: impl IntoIterator<Item = &'a Path>,
) -> u64 {
    let sessions: BTreeSet<&str> = sessions.into_iter().collect();
    let mut cwds: Vec<&Path> = cwds.into_iter().collect();
    cwds.sort();
    cwds.dedup();
    let mut bytes = Vec::new();
    for id in sessions {
        bytes.extend_from_slice(id.as_bytes());
        bytes.push(0);
    }
    bytes.push(1);
    for cwd in cwds {
        bytes.extend_from_slice(cwd.as_os_str().as_encoded_bytes());
        bytes.push(0);
    }
    aterm_hash::fnv1a_64(&bytes)
}

/// The record that a budget pass could not meet its target, in the harness state directory
/// ([`Unmet`]): read by [`plans`], written by [`unmet`], removed by [`met`]. An emergency pass
/// keeps its own (`super::EMERGENCY_UNMET_FILE`, through the `*_in` forms).
pub const UNMET_FILE: &str = "disk-budget-unmet.json";

/// The least time, in seconds, from an episode's last pass to the next one its memo plans with
/// nothing else changed ([`plans`]): what kept a row from it may have lapsed since (its runs'
/// record past [`atpkg::build_cache::stale::KEPT_FOR`], its idle window, a build that held a
/// lock it was refused under), and every such hold is counted in days or less.
pub const REPLAN_AFTER_S: i64 = 86_400;

/// What one budget pass ran against, as the memo compares it ([`plans`], [`unmet`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inputs {
    /// The volume, by its spelled key.
    pub volume: String,
    /// The cap, bytes.
    pub cap: u64,
    /// The bytes aterm could not rebuild.
    pub kept: u64,
    /// The rebuildable bytes sized.
    pub rebuildable: u64,
    /// Whether what the pass reached, or the sizing, was a floor.
    pub partial: bool,
    /// The live sessions' digest ([`Live::digest`]).
    pub live: u64,
}

impl Inputs {
    /// A pass's inputs: `sizing`'s volume, `budget`'s figures, the live digest `live`, and
    /// whether what it reached was a floor (`partial`).
    #[must_use]
    pub fn of(sizing: &Sizing, budget: &Budget, live: u64, partial: bool) -> Self {
        Self {
            volume: sizing.volume.clone(),
            cap: budget.cap,
            kept: budget.kept,
            rebuildable: budget.rebuildable,
            partial,
            live,
        }
    }
}

/// [`UNMET_FILE`]'s record: the `cannot-meet` row written for a volume and cap, and what the
/// passes of that episode have seen since.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unmet {
    /// The volume, by its spelled key.
    pub volume: String,
    /// The cap, bytes.
    pub cap: u64,
    /// The kept bytes the row said.
    pub kept: u64,
    /// The rebuildable bytes the row said.
    pub rebuildable: u64,
    /// Whether the row's figures were a floor.
    pub partial: bool,
    /// Unix second the row was written.
    pub written_unix: i64,
    /// The rebuildable bytes the last pass of the episode ran against.
    pub seen_rebuildable: u64,
    /// The live digest the last pass of the episode ran against.
    pub seen_live: u64,
    /// Unix second the last pass of the episode ended unable to meet the target.
    pub seen_unix: i64,
}

impl Unmet {
    /// The record as [`UNMET_FILE`] holds it.
    #[must_use]
    pub fn encode(&self) -> String {
        let mut o = Map::new();
        o.insert("schema".to_owned(), Value::from(1u64));
        o.insert("volume".to_owned(), Value::from(self.volume.clone()));
        o.insert("cap".to_owned(), Value::from(self.cap));
        o.insert("kept".to_owned(), Value::from(self.kept));
        o.insert("rebuildable".to_owned(), Value::from(self.rebuildable));
        o.insert("partial".to_owned(), Value::from(self.partial));
        o.insert("written_unix".to_owned(), Value::from(self.written_unix));
        o.insert(
            "seen_rebuildable".to_owned(),
            Value::from(self.seen_rebuildable),
        );
        o.insert("seen_live".to_owned(), Value::from(self.seen_live));
        o.insert("seen_unix".to_owned(), Value::from(self.seen_unix));
        aterm_json::to_string(&Value::Object(o)).unwrap_or_else(|_| "{}".to_owned())
    }

    /// The record `text` holds; `None` where it is not one ([`Self::encode`]'s schema 1).
    #[must_use]
    pub fn decode(text: &str) -> Option<Self> {
        let v = aterm_json::from_str::<Value>(text).ok()?;
        if v.get("schema").and_then(Value::as_u64) != Some(1) {
            return None;
        }
        Some(Self {
            volume: v.get("volume")?.as_str()?.to_owned(),
            cap: v.get("cap")?.as_u64()?,
            kept: v.get("kept")?.as_u64()?,
            rebuildable: v.get("rebuildable")?.as_u64()?,
            partial: v.get("partial")?.as_bool()?,
            written_unix: v.get("written_unix")?.as_i64()?,
            seen_rebuildable: v.get("seen_rebuildable")?.as_u64()?,
            seen_live: v.get("seen_live")?.as_u64()?,
            seen_unix: v.get("seen_unix")?.as_i64()?,
        })
    }
}

/// The record in `dir`'s [`UNMET_FILE`]; `None` where there is none, or one that is not a
/// record (a corrupt one then costs one more row, after which it is whole again).
#[must_use]
pub fn load(dir: &Path) -> Option<Unmet> {
    load_in(dir, UNMET_FILE)
}

/// [`load`] of the memo `file` in `dir`.
#[must_use]
pub fn load_in(dir: &Path, file: &str) -> Option<Unmet> {
    aterm_uds::atomic_file::read_regular_text(&dir.join(file), FILE_MAX)
        .ok()
        .and_then(|text| Unmet::decode(&text))
}

/// Whether a pass is planned against `inputs` at unix second `now` by the memo (`memo`,
/// [`load`]): where none says the target cannot be met, it says so of another volume or cap, or
/// since the episode's last pass the rebuildable bytes grew, the live sessions changed, or
/// [`REPLAN_AFTER_S`] passed, in which a row a hold kept from it may have become one it may take.
#[must_use]
pub fn plans(memo: Option<&Unmet>, inputs: &Inputs, now: i64) -> bool {
    memo.is_none_or(|memo| {
        memo.volume != inputs.volume
            || memo.cap != inputs.cap
            || inputs.rebuildable > memo.seen_rebuildable
            || inputs.live != memo.seen_live
            || now.saturating_sub(memo.seen_unix) >= REPLAN_AFTER_S
    })
}

/// Record in `dir` that a pass against `inputs` could not meet its target, at unix second
/// `now`: `true` where the `cannot-meet` row is to be written (no memo said so of this volume
/// and cap, and one now does), `false` where one already did (what this pass saw, and when, is
/// kept in it).
/// The memo is written before the row is, so a state directory that cannot be written writes
/// no row.
///
/// # Errors
///
/// `dir` could not be made, or the memo could not be written.
pub fn unmet(dir: &Path, inputs: &Inputs, now: i64) -> io::Result<bool> {
    unmet_in(dir, UNMET_FILE, inputs, now)
}

/// [`unmet`] in the memo `file` in `dir`.
///
/// # Errors
///
/// `dir` could not be made, or the memo could not be written.
pub fn unmet_in(dir: &Path, file: &str, inputs: &Inputs, now: i64) -> io::Result<bool> {
    let (memo, row) = match load_in(dir, file) {
        Some(memo) if memo.volume == inputs.volume && memo.cap == inputs.cap => (
            Unmet {
                seen_rebuildable: inputs.rebuildable,
                seen_live: inputs.live,
                seen_unix: now,
                ..memo
            },
            false,
        ),
        _ => (
            Unmet {
                volume: inputs.volume.clone(),
                cap: inputs.cap,
                kept: inputs.kept,
                rebuildable: inputs.rebuildable,
                partial: inputs.partial,
                written_unix: now,
                seen_rebuildable: inputs.rebuildable,
                seen_live: inputs.live,
                seen_unix: now,
            },
            true,
        ),
    };
    std::fs::create_dir_all(dir)?;
    aterm_uds::atomic_file::write_checked(
        &dir.join(file),
        memo.encode().as_bytes(),
        0o600,
        || Ok(()),
    )?;
    Ok(row)
}

/// A pass met its target in `dir`'s episode: the memo goes, and the next pass that cannot meet
/// one writes its row.
///
/// # Errors
///
/// The memo is there and could not be removed.
pub fn met(dir: &Path) -> io::Result<()> {
    met_in(dir, UNMET_FILE)
}

/// [`met`] of the memo `file` in `dir`.
///
/// # Errors
///
/// The memo is there and could not be removed.
pub fn met_in(dir: &Path, file: &str) -> io::Result<()> {
    match std::fs::remove_file(dir.join(file)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// How one page of a budget pass ended ([`end_of`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum End {
    /// Free space was measured at or above the target.
    Met,
    /// The pass stood down before it could tell (a stop that was no measure at the target, a
    /// measure that could not be read or was yielded to a pressure look): why.
    StoodDown(String),
    /// Short of the target with pages of its search left, or with a row refused (its removal
    /// failed, a build held its lock, the journal could not record it), which a later page may
    /// take.
    Unfinished,
    /// Short of the target with nothing it may take left: on what its whole search reached, or
    /// on what [`BUDGET_PAGES`] pages reached (`partial`, as where the sizing was a floor).
    CannotMeet { partial: bool },
}

/// How page `page` of a budget pass that stops at `free_target` ended, by what it did
/// (`applied`) and free space measured after it (`after`): only a measure ends the episode
/// ([`End::Met`]), and only a page that took every row it could reach, none refused, and is
/// still short says it cannot be met (`sized_partial`: the sizing was a floor); on its last page
/// it says so of what its pages reached (a floor).
#[must_use]
pub fn end_of(
    applied: &Applied,
    after: Option<u64>,
    free_target: u64,
    page: u32,
    sized_partial: bool,
) -> End {
    if applied.stops.iter().any(|s| s.by == StopBy::Measured)
        || after.is_some_and(|free| free >= free_target)
    {
        return End::Met;
    }
    if let Some(stop) = applied.stops.first() {
        return End::StoodDown(stop.describe());
    }
    if after.is_none() {
        return End::StoodDown(
            "free space could not be measured after the page, and nothing is said on a number \
             nobody read"
                .to_owned(),
        );
    }
    if applied.unfinished.is_none() && applied.denials.is_empty() {
        End::CannotMeet {
            partial: sized_partial,
        }
    } else if page >= BUDGET_PAGES {
        End::CannotMeet { partial: true }
    } else {
        End::Unfinished
    }
}

/// Where a budget run stands: the sizing it runs from, the pages it took, and whether it goes on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Run {
    /// The sizing (its `sized_unix`) the last run began from.
    pub from: Option<i64>,
    /// The pages it took.
    pub pages: u32,
    /// Whether its next look takes another page.
    pub open: bool,
}

impl Run {
    /// The page a look takes against the sizing that ended at `sized_unix`: the first of a new
    /// run where none began from that sizing, the next of an open run with pages left
    /// ([`BUDGET_PAGES`]), else none: one run a sizing.
    #[must_use]
    pub fn next(&self, sized_unix: i64) -> Option<u32> {
        if self.from != Some(sized_unix) {
            Some(1)
        } else if self.open && self.pages < BUDGET_PAGES {
            Some(self.pages.saturating_add(1))
        } else {
            None
        }
    }

    /// Page `page` of the run from the sizing that ended at `sized_unix` ran: it goes on only
    /// where it ended [`End::Unfinished`] (`unfinished`) with pages left.
    pub fn ran(&mut self, sized_unix: i64, page: u32, unfinished: bool) {
        self.from = Some(sized_unix);
        self.pages = page;
        self.open = unfinished && page < BUDGET_PAGES;
    }
}

#[cfg(test)]
#[path = "disk_budget_tests.rs"]
mod tests;
