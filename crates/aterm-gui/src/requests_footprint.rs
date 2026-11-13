// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A request's footprint on this Mac (ATERM-KI-1109-3, step 4 of the requests design): the
//! folders it claims and what its tabs' process trees spent. Nothing here deletes.
//!
//! Three ways a folder becomes a request's ([`aterm_requests::ClaimSrc`]):
//! - its requester names it, [`cmd_request_as`] (`request claim path=…`), on the control thread;
//! - it is born directly under `$HOME`, or under a folder one of its spawns opened in (a root),
//!   while one of its tabs' commands ran, or it is such a root born inside the request's life:
//!   the birth scan ([`scan`]), which the requests' writer thread runs on the minute's look with
//!   the command spans the main thread read of every tab ([`BlockSpan`]).
//!
//! The same look judges a person's later work in a claimed folder (`touched`): a command there
//! from a tab no open request owns, from a request tab a person kept, or one a person typed.
//!
//! What a folder holds (its size, whether a repo in it has work no remote has) and what a request
//! tab's tree spends while it runs are read on this module's own thread, `requests-footprint`,
//! never the writer's (a spawn's reply waits on that one) and never the main thread's: a size
//! walk of a 300 MB folder and a `git` read take seconds.

use crate::control::Scope;
use crate::session_store::{ExitActor, Store};
use aterm_requests::claim::{self, BIRTH_SKEW_MS, HOME_CHILD_MAX, SPAN_RING_MS, Span};
use aterm_requests::{ClaimAsk, ClaimRefusal, ClaimSrc, Claimed, Measured, Ran, Spent};
use aterm_requests::{Touch, TouchWhy};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, RwLock, mpsc};
use std::time::Duration;

const USAGE: &str = "ERR usage: request claim path=<abs|~/…> [req=<id>]\n";

/// Where a claim is judged against: this Mac's home and aterm's state home.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Places {
    pub(crate) home: Option<PathBuf>,
    pub(crate) state: Option<PathBuf>,
}

/// Why a folder is never a request's.
const HOME: &str = "is this Mac's home or a folder holding it";
const NO_HOME: &str = "cannot be judged: this Mac's home is unknown";
const STATE: &str = "lies in aterm's state home, or holds it";
/// Why a step of a walk found no folder.
const GUARDED: &str = "lies in a folder macOS guards";
const GONE: &str = "is no folder here";

impl Places {
    /// This host's.
    pub(crate) fn host() -> Self {
        Self {
            home: aterm_types::dirs::home_dir(),
            state: aterm_types::dirs::state_dir(),
        }
    }

    /// Why `path` (absolute, no `..`) can never be a request's, judged before any look at every
    /// spelling macOS resolves to one folder (case, Unicode form, the data volume's firmlinks:
    /// `/USERS/me` and `/System/Volumes/Data` hold the home as `/Users` does): it is the home or
    /// holds it (everything a person keeps, which no request may mark for a cleanup), it lies in
    /// a folder macOS guards, or it lies in aterm's state home or holds it. With no home known,
    /// nothing can be judged, so every folder is refused.
    fn refusal(&self, path: &Path) -> Option<&'static str> {
        use atpkg::protected::{at_or_under, under_protected_root};
        let Some(home) = self.home.as_deref() else {
            return Some(NO_HOME);
        };
        if at_or_under(path, home) {
            return Some(HOME);
        }
        if under_protected_root(home, path) {
            return Some(GUARDED);
        }
        if let Some(state) = self.state.as_deref()
            && (at_or_under(state, path) || at_or_under(path, state))
        {
            return Some(STATE);
        }
        None
    }
}

/// `request claim path=<abs> [req=<id>]` on the connection being served (through
/// [`crate::requests_review::cmd`]), `client` naming the owner-token connection's process
/// ([`crate::control::caller_actor_as`]) and `places` the home and state home it is judged by.
pub(crate) fn cmd_request_as(
    rest: &str,
    scope: Scope,
    store: &Store,
    client: impl FnOnce() -> crate::typed_by::Who,
    places: &Places,
) -> String {
    let (path, req) = match parse(rest) {
        Ok(parsed) => parsed,
        Err(refusal) => return refusal,
    };
    // Who asks: a drive's requester, or the tab the caller runs in. A person's shell is a tab
    // too; only a tab a request owns, or the requester's own `req=`, claims anything.
    let (node, by, tab) = match crate::control::caller_actor_as(scope, None, store, client) {
        ExitActor::Fleet { token, node } => (node, token, None),
        ExitActor::Sid(sid) => (crate::requests_host::own_node(), sid.clone(), Some(sid)),
        _ => return refused(&ClaimRefusal::NoRequest, req.as_deref()),
    };
    let (real, born_ms) = match judge(&path, places) {
        Ok(judged) => judged,
        Err(why) => {
            return format!(
                "ERR request claim path={}: {why}\n",
                aterm_codec::pct::encode(&path)
            );
        }
    };
    let dir = match crate::requests_host::dir() {
        Ok(Some(dir)) => dir,
        Ok(None) => return "ERR request claim: this Mac keeps no requests\n".to_owned(),
        Err(why) => return format!("ERR request store: {why}\n"),
    };
    let at = crate::requests_host::now_unix_ms();
    let held = held_spans(&dir);
    let ran: Vec<Ran<'_>> = held
        .iter()
        .map(|had| Ran {
            tab: &had.span.sid,
            span: had.span.span,
            whole: had.whole,
        })
        .collect();
    let ask = ClaimAsk {
        id: req.as_deref(),
        path: &real,
        node: &node,
        by: &by,
        tab: tab.as_deref(),
        born_ms,
        ran: &ran,
    };
    let done = crate::requests_host::with_store(&dir, |store| {
        let claimed = store.claim(at, ask);
        let life = match &claimed {
            Ok(Claimed::New(id) | Claimed::Already(id) | Claimed::Inside { id, .. }) => store
                .get(id)
                .map(|request| (request.opened_ms, request.ended_ms)),
            Err(_) => None,
        };
        crate::requests_host::note_claims(&dir, store);
        Ok((claimed, life))
    });
    let (claimed, life) = match done {
        Ok(done) => done,
        Err(error) => return format!("ERR request store: {error}\n"),
    };
    let pct = aterm_codec::pct::encode(&real);
    match claimed {
        Ok(Claimed::New(id)) => {
            let born = match (born_ms, life) {
                (None, _) | (_, None) => "unread",
                (Some(born), Some((opened, ended))) => {
                    let until = ended.unwrap_or(at).saturating_add(BIRTH_SKEW_MS);
                    if born >= opened.saturating_sub(BIRTH_SKEW_MS) && born <= until {
                        "in"
                    } else {
                        "before"
                    }
                }
            };
            aterm_log::info!("requests: {id} claims {pct} (src=named by={by} born={born})");
            measure(at, vec![real], places.home.clone(), measure_probe());
            format!("OK claimed req={id} path={pct} born={born}\n")
        }
        Ok(Claimed::Already(id)) => format!("OK already req={id} path={pct}\n"),
        Ok(Claimed::Inside { id, claim }) => format!(
            "OK inside req={id} path={pct} claim={}\n",
            aterm_codec::pct::encode(&claim)
        ),
        Err(refusal) => refused(&refusal, req.as_deref()),
    }
}

/// A refusal's reply.
fn refused(refusal: &ClaimRefusal, req: Option<&str>) -> String {
    match (refusal, req) {
        (ClaimRefusal::OtherTab(_), Some(id)) => format!("ERR request claim req={id}: {refusal}\n"),
        _ => format!("ERR request claim: {refusal}\n"),
    }
}

/// `claim path=<pct> [req=<id>]`: the path (decoded) and the request named.
fn parse(rest: &str) -> Result<(String, Option<String>), String> {
    let mut words = rest.split_whitespace();
    if words.next() != Some("claim") {
        return Err(USAGE.to_owned());
    }
    let (mut path, mut req) = (None, None);
    for word in words {
        if let Some(value) = word.strip_prefix("path=") {
            if value.is_empty() || path.replace(aterm_codec::pct::decode(value)).is_some() {
                return Err(USAGE.to_owned());
            }
        } else if let Some(value) = word.strip_prefix("req=") {
            let id = aterm_types::control_verbs::request_id(value).map_err(|why| {
                format!(
                    "ERR request claim req={}: {why}\n",
                    aterm_codec::tty::one_line(value, 40)
                )
            })?;
            if req.replace(id.to_owned()).is_some() {
                return Err(USAGE.to_owned());
            }
        } else {
            return Err(USAGE.to_owned());
        }
    }
    path.map(|path| (path, req)).ok_or_else(|| USAGE.to_owned())
}

/// The folder `path` names, as a claim records it, and its birth where the filesystem says:
/// absolute with no `..`, never `/` or a folder [`Places::refusal`] refuses (judged before any
/// look, each link on the way followed by hand, never by `canonicalize`); it must be a folder,
/// and the folder its links lead to is held to the same rules.
fn judge(path: &str, places: &Places) -> Result<(String, Option<u64>), String> {
    if path.starts_with('~') {
        return Err(
            "names `~`, which only a drive's line resolves: name an absolute path (a \
                    local shell expands `~`)"
                .to_owned(),
        );
    }
    let lexical = |path: &str| -> Result<String, &'static str> {
        let path = claim::normal(path)?;
        match places.refusal(Path::new(&path)) {
            Some(why) => Err(why),
            None => Ok(path),
        }
    };
    let named = lexical(path)?;
    let real = walk(places.home.as_deref(), Path::new(&named))?;
    let real = real.to_str().ok_or("is not UTF-8")?;
    let real = lexical(real)?;
    Ok((real.clone(), born_ms(Path::new(&real))))
}

/// The root a spawn that opened in `cwd` gives its request: the folder it names, where a claim
/// of it could be made ([`judge`]): its links followed by hand, so a scan never lists through
/// one, and never the home, a folder holding it, a folder macOS guards or the state home.
pub(crate) fn root_of(cwd: &str, places: &Places) -> Option<String> {
    judge(cwd, places).ok().map(|(real, _)| real)
}

/// The folder `path` (absolute) names, each link on the way followed by hand: a step's path is
/// judged against the folders macOS guards under `home` before it is looked at, so no link leads
/// a look into one. The folder it ends at has no link in it.
fn walk(home: Option<&Path>, path: &Path) -> Result<PathBuf, &'static str> {
    fn split(path: &Path) -> (PathBuf, VecDeque<OsString>) {
        let mut root = PathBuf::new();
        let mut steps = VecDeque::new();
        for part in path.components() {
            match part {
                Component::Prefix(_) | Component::RootDir => root.push(part.as_os_str()),
                Component::CurDir => {}
                Component::ParentDir => steps.push_back("..".into()),
                Component::Normal(name) => steps.push_back(name.to_owned()),
            }
        }
        (root, steps)
    }
    let (mut at, mut left) = split(path);
    let mut links = 0_u32;
    while let Some(step) = left.pop_front() {
        if step == ".." {
            // `at` holds no link, so its parent is the folder `..` names.
            at.pop();
            continue;
        }
        let next = at.join(&step);
        if home.is_some_and(|home| atpkg::protected::under_protected_root(home, &next)) {
            return Err(GUARDED);
        }
        let meta = std::fs::symlink_metadata(&next).map_err(|_| GONE)?;
        if !meta.file_type().is_symlink() {
            at = next;
            continue;
        }
        links += 1;
        if links > 40 {
            return Err(GONE);
        }
        let target = std::fs::read_link(&next).map_err(|_| GONE)?;
        let (root, steps) = split(&target);
        if !root.as_os_str().is_empty() {
            at = root;
        }
        for step in steps.into_iter().rev() {
            left.push_front(step);
        }
    }
    if !std::fs::symlink_metadata(&at).is_ok_and(|meta| meta.is_dir()) {
        return Err(GONE);
    }
    Ok(at)
}

/// When the folder at `path` was born, Unix milliseconds, where the filesystem keeps it; never
/// following a link.
fn born_ms(path: &Path) -> Option<u64> {
    let born = std::fs::symlink_metadata(path).ok()?.created().ok()?;
    let since = born.duration_since(std::time::UNIX_EPOCH).ok()?;
    u64::try_from(since.as_millis()).ok()
}

/// One command's span in a tab, as the minute's look read it from the tab's terminal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BlockSpan {
    /// The tab.
    pub(crate) sid: String,
    /// Its block.
    pub(crate) blk: u64,
    /// The folder it ran in, as its shell said.
    pub(crate) cwd: Option<String>,
    /// When its input began, Unix ms.
    pub(crate) input_ms: Option<u64>,
    /// When it ran.
    pub(crate) span: Span,
}

/// One entry of a folder, as a birth scan's listing gives it: its own type, no link followed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Child {
    pub(crate) name: String,
    pub(crate) dir: bool,
    pub(crate) link: bool,
}

/// The filesystem reads a birth scan makes, injected so a test's folder stands in.
#[derive(Clone, Copy)]
pub(crate) struct FsProbe {
    /// The entries of a folder, at most [`HOME_CHILD_MAX`].
    pub(crate) list: fn(&Path) -> Vec<Child>,
    /// A folder's birth, Unix ms ([`born_ms`]).
    pub(crate) born: fn(&Path) -> Option<u64>,
}

impl FsProbe {
    /// This host's.
    pub(crate) const HOST: Self = Self {
        list: list_children,
        born: born_ms,
    };
}

fn list_children(dir: &Path) -> Vec<Child> {
    // `read_dir` follows a link: a folder that is one is never listed.
    if !std::fs::symlink_metadata(dir).is_ok_and(|meta| meta.is_dir()) {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .take(HOME_CHILD_MAX)
        .filter_map(|entry| {
            let kind = entry.file_type().ok()?;
            Some(Child {
                name: entry.file_name().into_string().ok()?,
                dir: kind.is_dir(),
                link: kind.is_symlink(),
            })
        })
        .collect()
}

/// What the minute's look hands the requests' writer: the command spans read since its last
/// look (a running one again until it ends) and when a person last typed in each tab that has
/// one, with the reads to make.
pub(crate) struct Hand {
    pub(crate) at: u64,
    pub(crate) places: Places,
    pub(crate) fs: FsProbe,
    pub(crate) measure: MeasureProbe,
    pub(crate) spans: Vec<BlockSpan>,
    /// When a person last typed in each tab, Unix ms.
    pub(crate) typed: HashMap<String, u64>,
}

/// A span the requests' writer holds: whether its end was seen. A span still running at one look
/// and not handed at the next (its tab left, or the read passed its block) ended at some moment
/// between; it is cut at the later look, where it contests another's folder up to then, and it
/// never accounts for a folder of its own request's ([`claim::alone`], [`Ran::whole`]).
#[derive(Clone, Debug)]
struct Ringed {
    span: BlockSpan,
    whole: bool,
}

/// The command spans of the last [`SPAN_RING_MS`], per store folder, which only the requests'
/// writer thread reads and writes: a folder's birth is judged against every tab's.
static RING: Mutex<BTreeMap<PathBuf, Vec<Ringed>>> = Mutex::new(BTreeMap::new());

/// The spans the writer holds for the store of folder `dir`, each with its tab, as a named
/// claim's birth is judged by ([`aterm_requests::ClaimAsk::ran`]).
fn held_spans(dir: &Path) -> Vec<Ringed> {
    RING.lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(dir)
        .cloned()
        .unwrap_or_default()
}

/// The paths too long to record that a scan already named in the log.
static LONG: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

/// What a look read of the store before its scan, so the scan's reads of the filesystem run with
/// no lock held ([`plan`], [`scan`], [`record`]).
pub(crate) struct Plan {
    ring: Vec<Ringed>,
    /// The request that owns each tab of the ring, where one does.
    owner: HashMap<String, Option<String>>,
    /// The requests a newly handed span ran a command for, that ended within the ring's reach.
    fresh: Vec<aterm_requests::Request>,
    /// Every claimed folder.
    claimed: Vec<String>,
}

/// The writer thread's look at what `hand` holds, on the store of folder `dir`: [`plan`],
/// [`scan`] and [`record`] in one, the store held throughout (a test's; the writer releases its
/// lock for the scan). Returns each folder claimed and its request.
///
/// # Errors
/// The journal's.
#[cfg(test)]
pub(crate) fn look(
    dir: &Path,
    store: &mut aterm_requests::Store,
    hand: &Hand,
) -> std::io::Result<Vec<(String, String)>> {
    let plan = plan(dir, store, hand);
    let found = scan(&plan, hand);
    record(store, hand, &plan, &found)
}

/// A look's first step, under the store's lock: the handed spans into the ring of folder `dir`
/// (a running one replaced as it is handed again, one no longer handed cut), and what the scan
/// needs of the store.
pub(crate) fn plan(dir: &Path, store: &mut aterm_requests::Store, hand: &Hand) -> Plan {
    let ring = {
        let mut rings = RING.lock().unwrap_or_else(|p| p.into_inner());
        let ring = rings.entry(dir.to_path_buf()).or_default();
        let handed = |had: &BlockSpan| {
            hand.spans
                .iter()
                .any(|span| span.sid == had.sid && span.blk == had.blk)
        };
        for had in ring.iter_mut() {
            if had.span.span.to_ms.is_none() && !handed(&had.span) {
                had.span.span.to_ms = Some(hand.at);
                had.whole = false;
            }
        }
        for span in &hand.spans {
            let ringed = Ringed {
                span: span.clone(),
                whole: true,
            };
            match ring
                .iter_mut()
                .find(|had| had.span.sid == span.sid && had.span.blk == span.blk)
            {
                Some(had) => *had = ringed,
                None => ring.push(ringed),
            }
        }
        let floor = hand.at.saturating_sub(SPAN_RING_MS);
        ring.retain(|had| had.span.span.to_ms.is_none_or(|to| to >= floor));
        ring.clone()
    };
    let mut owner: HashMap<String, Option<String>> = HashMap::new();
    for had in &ring {
        if !owner.contains_key(&had.span.sid) {
            let id = store.owner(&had.span.sid).map(|(id, ..)| id);
            owner.insert(had.span.sid.clone(), id);
        }
    }
    let ids: BTreeSet<String> = hand
        .spans
        .iter()
        .filter_map(|span| owner.get(&span.sid).cloned().flatten())
        .collect();
    let reach = hand.at.saturating_sub(SPAN_RING_MS);
    let fresh = ids
        .iter()
        .filter_map(|id| store.get(id))
        .filter(|request| request.ended_ms.is_none_or(|ended| ended >= reach))
        .cloned()
        .collect();
    let claimed = store.claims().map(|(path, _)| path.to_owned()).collect();
    Plan {
        ring,
        owner,
        fresh,
        claimed,
    }
}

/// How a birth stands against a request's commands.
enum Birth {
    /// It is not the request's.
    Not,
    /// A command of the request's that runs still, for less than [`claim::ALONE_SPAN_MS`],
    /// held it: the look after it ends judges it.
    Later,
    /// It is the request's, made by a command of this tab's; [`claim::alone`] says how surely.
    Of { tab: String, alone: Option<bool> },
}

/// A look's second step, with no lock held: each fresh request has the direct children of
/// `$HOME` (listed once) and of its roots listed, and a folder born while one of its tabs'
/// commands ran is its (`src=born`), as is a root born inside its life (`src=cwd`), each with
/// [`claim::alone`]'s judgement; a dot-name under `$HOME`, a link, a file, a folder
/// [`Places::refusal`] refuses (never looked at) and a folder a claim covers are skipped. No home
/// known, nothing is listed.
pub(crate) fn scan(plan: &Plan, hand: &Hand) -> Vec<aterm_requests::Born> {
    let Some(home) = hand.places.home.clone() else {
        return Vec::new();
    };
    let covered =
        |path: &str| claim::inside(path, plan.claimed.iter().map(String::as_str)).is_some();
    let owner_of = |sid: &str| plan.owner.get(sid).cloned().flatten();
    let mut listed: HashMap<PathBuf, Vec<(String, u64)>> = HashMap::new();
    let mut found = Vec::new();
    for request in &plan.fresh {
        let id = &request.id;
        let (mine, others): (Vec<&Ringed>, Vec<&Ringed>) = plan
            .ring
            .iter()
            .partition(|had| owner_of(&had.span.sid).as_deref() == Some(id.as_str()));
        let mine: Vec<&BlockSpan> = mine
            .into_iter()
            .filter(|had| had.whole)
            .map(|had| &had.span)
            .collect();
        let mine_spans: Vec<Span> = mine.iter().map(|span| span.span).collect();
        let others: Vec<Span> = others.iter().map(|had| had.span.span).collect();
        let birth = |born: u64| -> Birth {
            if born < request.opened_ms.saturating_sub(BIRTH_SKEW_MS)
                || !claim::born_in(born, &mine_spans, BIRTH_SKEW_MS, request.ended_ms)
            {
                return Birth::Not;
            }
            let held: Vec<&&BlockSpan> = mine
                .iter()
                .filter(|span| span.span.holds(born, BIRTH_SKEW_MS))
                .collect();
            let Some(first) = held.first() else {
                return Birth::Not;
            };
            if held.iter().any(|span| {
                span.span.to_ms.is_none()
                    && hand.at.saturating_sub(span.span.from_ms) <= claim::ALONE_SPAN_MS
            }) {
                return Birth::Later;
            }
            Birth::Of {
                tab: first.sid.clone(),
                alone: claim::alone(born, &mine_spans, &others),
            }
        };
        let roots = std::iter::once((home.clone(), true)).chain(
            request
                .roots
                .iter()
                .map(|root| (PathBuf::from(root), false))
                .filter(|(root, _)| hand.places.refusal(root).is_none()),
        );
        for (root, is_home) in roots {
            if !is_home
                && let Some(text) = root.to_str()
                && !covered(text)
                && let Some(born) = (hand.fs.born)(&root)
                && let Birth::Of { tab, alone } = birth(born)
            {
                found.push(aterm_requests::Born {
                    id: id.clone(),
                    path: text.to_owned(),
                    src: ClaimSrc::Cwd,
                    tab: Some(tab),
                    born_ms: born,
                    alone,
                });
            }
            let children = listed
                .entry(root.clone())
                .or_insert_with(|| children(&root, is_home, hand));
            for (text, born) in children.iter() {
                if covered(text) {
                    continue;
                }
                if let Birth::Of { tab, alone } = birth(*born) {
                    found.push(aterm_requests::Born {
                        id: id.clone(),
                        path: text.clone(),
                        src: ClaimSrc::Born,
                        tab: Some(tab),
                        born_ms: *born,
                        alone,
                    });
                }
            }
        }
    }
    found
}

/// The folders directly in `root` a scan may claim, each in [`claim::normal`] form with its
/// birth: no link, no file, no dot-name under the home, none [`Places::refusal`] refuses (never
/// looked at), none too long to record.
fn children(root: &Path, is_home: bool, hand: &Hand) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    for child in (hand.fs.list)(root).into_iter().take(HOME_CHILD_MAX) {
        if !child.dir || child.link || (is_home && child.name.starts_with('.')) {
            continue;
        }
        let path = root.join(&child.name);
        if hand.places.refusal(&path).is_some() {
            continue;
        }
        let Some(text) = path.to_str() else {
            continue;
        };
        let text = match claim::normal(text) {
            Ok(text) => text,
            Err(why) => {
                if LONG
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(path.clone())
                {
                    aterm_log::info!(
                        "requests: {} not claimed: it {why}",
                        aterm_codec::tty::one_line(text, 120)
                    );
                }
                continue;
            }
        };
        if let Some(born) = (hand.fs.born)(&path) {
            out.push((text, born));
        }
    }
    out
}

/// A look's last step, under the store's lock: the folders [`scan`] found recorded as their
/// requests' claims ([`aterm_requests::Store::born`], which judges them against the claims as
/// they stand now), then each span the writer holds that ran in a claimed folder after that
/// folder's birth is a person's touch where its tab is no open request's, a kept one, or a person
/// typed it. Returns each folder claimed and its request.
///
/// # Errors
/// The journal's.
pub(crate) fn record(
    store: &mut aterm_requests::Store,
    hand: &Hand,
    plan: &Plan,
    found: &[aterm_requests::Born],
) -> std::io::Result<Vec<(String, String)>> {
    let new = store.born(hand.at, found)?;
    for (path, id) in &new {
        aterm_log::info!(
            "requests: {id} claims {} (a folder born while its command ran)",
            aterm_codec::pct::encode(path)
        );
    }
    let hits = touches(store, hand, &plan.ring);
    store.touched(hand.at, &hits)?;
    Ok(new)
}

/// The held spans that are a person's work in a claimed folder ([`record`]): each that began after
/// the folder's birth (its claim's, where its birth is unread), so work in the minute before the
/// look that claimed it counts, and work while no request lived (the look reads spans while any
/// claim a person has not touched stands, [`crate::requests_host::watching`]).
fn touches(store: &mut aterm_requests::Store, hand: &Hand, ring: &[Ringed]) -> Vec<Touch> {
    let claims: Vec<(String, String)> = store
        .claims()
        .map(|(path, id)| (path.to_owned(), id.to_owned()))
        .collect();
    if claims.is_empty() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    for span in ring.iter().map(|had| &had.span) {
        let Some(cwd) = span.cwd.as_deref().and_then(|cwd| claim::normal(cwd).ok()) else {
            continue;
        };
        let Some(path) = claim::inside(&cwd, claims.iter().map(|(path, _)| path.as_str())) else {
            continue;
        };
        let Some((path, id)) = claims.iter().find(|(had, _)| had == path) else {
            continue;
        };
        let after = store
            .get(id)
            .and_then(|request| request.claim(path))
            .is_some_and(|held| span.span.from_ms > held.claim.born_ms.unwrap_or(held.at_ms));
        if !after {
            continue;
        }
        let why = match store.owner(&span.sid) {
            None => Some(TouchWhy::PersonTab),
            Some((owner, ..)) => store.get(&owner).and_then(|request| {
                let tab = request.tabs.iter().find(|tab| tab.sid == span.sid)?;
                let typed = hand.typed.get(&span.sid).is_some_and(|typed| {
                    *typed >= span.input_ms.unwrap_or(span.span.from_ms)
                        && *typed <= span.span.from_ms.saturating_add(1000)
                });
                if tab.kept {
                    Some(TouchWhy::Kept)
                } else if request
                    .ended_ms
                    .is_some_and(|ended| span.span.from_ms > ended)
                {
                    Some(TouchWhy::PersonTab)
                } else if typed {
                    Some(TouchWhy::Typed)
                } else {
                    None
                }
            }),
        };
        if let Some(why) = why {
            hits.push(Touch {
                id: id.clone(),
                path: path.clone(),
                tab: span.sid.clone(),
                blk: span.blk,
                why,
            });
        }
    }
    hits
}

/// A folder's size in bytes and whether the walk saw all of it.
pub(crate) type Walked = (u64, bool);

/// The reads a measure of a claimed folder makes, injected so a test's table stands in.
#[derive(Clone, Copy)]
pub(crate) struct MeasureProbe {
    /// What a walk of it found, given the home; `None` where it could not be walked.
    pub(crate) walk: fn(&Path, Option<&Path>) -> Option<Walked>,
    /// Whether a repo in it holds work no remote has; `None` where one could not be read, or the
    /// search for them did not see the whole folder.
    pub(crate) repos: fn(&Path, Option<&Path>) -> Option<bool>,
}

impl MeasureProbe {
    /// This host's: a size walk of at most 30 s, and each repo found within four levels (at
    /// most 16) read by the harness's repo probe.
    pub(crate) const HOST: Self = Self {
        walk: walk_size,
        repos: repos_unpushed,
    };

    /// A probe that reads nothing: every folder reads unmeasured.
    #[cfg(any(test, feature = "bench-support"))]
    pub(crate) const INERT: Self = Self {
        walk: |_, _| None,
        repos: |_, _| None,
    };
}

#[cfg(test)]
thread_local! {
    static MEASURE: std::cell::Cell<MeasureProbe> = const { std::cell::Cell::new(MeasureProbe::INERT) };
}

/// The probe a claim on this thread measures with: the host's; a test's own.
pub(crate) fn measure_probe() -> MeasureProbe {
    #[cfg(test)]
    {
        MEASURE.with(std::cell::Cell::get)
    }
    #[cfg(not(test))]
    {
        MeasureProbe::HOST
    }
}

/// Measure this test thread's claims with `probe`.
#[cfg(test)]
pub(crate) fn set_measure_for_test(probe: MeasureProbe) {
    MEASURE.with(|slot| slot.set(probe));
}

/// How long a walk of one claimed folder may take.
const WALK_BUDGET: Duration = Duration::from_secs(30);

/// The size of `path`, by a walk the registered sink `diskwalk_size_full` makes, held to
/// [`WALK_BUDGET`]: only the requests-footprint thread calls it.
fn walk_size(path: &Path, home: Option<&Path>) -> Option<Walked> {
    let home = home?;
    let policy = aterm_diskwalk::Policy::new(
        aterm_diskwalk::Deny::for_home(home, &[], &[]),
        aterm_sysprobe::volumes::table(),
    );
    let full = aterm_diskwalk::diskwalk_size_full(
        &aterm_diskwalk::RootSpec::new(path),
        &policy,
        WALK_BUDGET,
        &mut std::time::Instant::now,
    )
    .ok()?;
    Some((full.sums.alloc_fair, full.complete))
}

/// The most repos a measure reads in one folder, and how deep it looks for them.
const REPOS_MAX: usize = 16;
const REPO_DEPTH: usize = 4;
/// The most entries a measure lists looking for repos.
const REPO_LIST_MAX: usize = 4096;

/// Whether a repo in `path` holds work no remote has: `Some(false)` only where the search saw the
/// whole folder ([`repos_in`]) and the repo probe finished every read of each repo it found
/// (`Checked`, or `Found` with nothing to name). Its `Clean` covers a probe that could not run
/// (no `git`, a network volume, no home) and a repo with no commit yet, whose files may be
/// anyone's work; `Gone` is a folder that changed under the read. Each is unread.
fn repos_unpushed(path: &Path, home: Option<&Path>) -> Option<bool> {
    use aterm_agent::harness::repo_work::{PROBE_LIMIT, RepoRead, probe};
    let home = home?;
    let mut unpushed = false;
    for repo in repos_in(path, home)? {
        match probe(&repo, Some(home), PROBE_LIMIT) {
            RepoRead::Checked(_) => {}
            RepoRead::Found(work) => unpushed |= work.found(),
            RepoRead::Unread | RepoRead::Clean | RepoRead::Gone(_) => return None,
        }
    }
    Some(unpushed)
}

/// The repos at or under `path`, within [`REPO_DEPTH`] levels: a folder holding `.git` is one,
/// and is not looked into further. No link is followed. `None` where the search could not see
/// the whole folder: more than [`REPOS_MAX`] repos, more than [`REPO_LIST_MAX`] entries, a folder
/// it could not list whole, a folder macOS guards (never listed), or a folder past
/// [`REPO_DEPTH`] that holds a folder.
fn repos_in(path: &Path, home: &Path) -> Option<Vec<PathBuf>> {
    let mut repos = Vec::new();
    let mut listed = 0_usize;
    let mut next = VecDeque::from([(path.to_path_buf(), 0_usize)]);
    while let Some((dir, depth)) = next.pop_front() {
        if atpkg::protected::under_protected_root(home, &dir) {
            return None;
        }
        if std::fs::symlink_metadata(dir.join(".git")).is_ok() {
            if repos.len() >= REPOS_MAX {
                return None;
            }
            repos.push(dir);
            continue;
        }
        let children = read_children(&dir, REPO_LIST_MAX.saturating_sub(listed))?;
        listed += children.len();
        for child in children {
            if child.dir && !child.link {
                if depth >= REPO_DEPTH {
                    return None;
                }
                next.push_back((dir.join(child.name), depth + 1));
            }
        }
    }
    Some(repos)
}

/// Every entry of folder `dir`, each with its own type: `None` where it is a link or no folder,
/// it could not be listed, an entry could not be read or is not UTF-8, or it holds more than
/// `cap`.
fn read_children(dir: &Path, cap: usize) -> Option<Vec<Child>> {
    if !std::fs::symlink_metadata(dir).ok()?.is_dir() {
        return None;
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        if out.len() >= cap {
            return None;
        }
        let kind = entry.file_type().ok()?;
        out.push(Child {
            name: entry.file_name().into_string().ok()?,
            dir: kind.is_dir(),
            link: kind.is_symlink(),
        });
    }
    Some(out)
}

/// How long a measure stands before a reader measures again.
pub(crate) const MEASURE_TTL_MS: u64 = 10 * 60_000;

/// What each claimed folder measured, by path: never journaled.
static MEASURED: RwLock<BTreeMap<String, Measured>> = RwLock::new(BTreeMap::new());

/// What `path` measured, where it was measured within [`MEASURE_TTL_MS`] of `now_ms`.
pub(crate) fn measured(path: &str, now_ms: u64) -> Option<Measured> {
    MEASURED
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .get(path)
        .copied()
        .filter(|measured| measured.at_ms.saturating_add(MEASURE_TTL_MS) >= now_ms)
}

/// What a request tab's process tree spent while it ran, as the samples saw it: each figure the
/// largest any sample read, and disk written summed over every process a sample saw, each at
/// the most it was seen to have written.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct LiveSpend {
    cpu_ns: Option<u64>,
    peak: Option<u64>,
    /// By pid: the last count seen, and what earlier processes of that pid wrote (a count that
    /// fell is a new process).
    written: HashMap<u32, (u64, u64)>,
}

impl LiveSpend {
    fn sample(&mut self, read: &[(u32, aterm_sysprobe::jobs::Counters)]) {
        if read.is_empty() {
            return;
        }
        let cpu = read.iter().fold(0_u64, |cpu, (_, c)| {
            cpu.saturating_add(c.cpu_ns.saturating_add(c.child_cpu_ns.unwrap_or(0)))
        });
        self.cpu_ns = Some(self.cpu_ns.unwrap_or(0).max(cpu));
        for (pid, c) in read {
            self.peak = match (self.peak, c.peak_footprint) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            };
            if let Some(now) = c.disk_written {
                let (last, banked) = self.written.entry(*pid).or_default();
                if now < *last {
                    *banked = banked.saturating_add(*last);
                }
                *last = now;
            }
        }
    }

    fn spent(&self) -> Spent {
        const MIB: u64 = 1024 * 1024;
        let written = (!self.written.is_empty()).then(|| {
            self.written.values().fold(0_u64, |sum, (last, banked)| {
                sum.saturating_add(*last).saturating_add(*banked)
            })
        });
        Spent {
            cpu_s: self.cpu_ns.map(|ns| ns / 1_000_000_000),
            peak_mb: self.peak.map(|bytes| bytes / MIB),
            written_mb: written.map(|bytes| bytes / MIB),
        }
    }
}

/// Each live request tab's spend so far, by sid.
static SPEND: Mutex<BTreeMap<String, LiveSpend>> = Mutex::new(BTreeMap::new());

/// What the samples saw request tab `sid` spend, where any read it.
pub(crate) fn spend_of(sid: &str) -> Option<Spent> {
    SPEND
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(sid)
        .map(LiveSpend::spent)
}

/// Tab `sid` left: its samples are done with.
pub(crate) fn forget_spend(sid: &str) {
    SPEND.lock().unwrap_or_else(|p| p.into_inner()).remove(sid);
}

enum Job {
    Sample {
        tabs: Vec<(String, i32)>,
        probe: crate::tab_marks::TreeProbe,
    },
    Measure {
        at: u64,
        paths: Vec<String>,
        home: Option<PathBuf>,
        probe: MeasureProbe,
    },
    #[cfg(test)]
    Flush(mpsc::Sender<()>),
}

static FOOTPRINT: Mutex<Option<mpsc::Sender<Job>>> = Mutex::new(None);

fn post(job: Job) {
    let mut thread = FOOTPRINT.lock().unwrap_or_else(|p| p.into_inner());
    let job = match thread.as_ref() {
        Some(tx) => match tx.send(job) {
            Ok(()) => return,
            Err(mpsc::SendError(job)) => job,
        },
        None => job,
    };
    let (tx, rx) = mpsc::channel::<Job>();
    let started = std::thread::Builder::new()
        .name("requests-footprint".to_owned())
        .spawn(move || {
            for job in rx {
                run(job);
            }
        });
    match started {
        Ok(_) => {
            let _ = tx.send(job);
            *thread = Some(tx);
        }
        Err(error) => aterm_log::warn!("requests: no footprint thread: {error}"),
    }
}

fn run(job: Job) {
    match job {
        Job::Sample { tabs, probe } => {
            let read: Vec<(String, Vec<(u32, aterm_sysprobe::jobs::Counters)>)> = tabs
                .into_iter()
                .map(|(sid, shell)| (sid, crate::tab_marks::tree_counters(shell, &probe)))
                .collect();
            let mut spend = SPEND.lock().unwrap_or_else(|p| p.into_inner());
            for (sid, read) in read {
                spend.entry(sid).or_default().sample(&read);
            }
        }
        Job::Measure {
            at,
            paths,
            home,
            probe,
        } => {
            for path in paths {
                let at_path = Path::new(&path);
                let walked = (probe.walk)(at_path, home.as_deref());
                let unpushed = (probe.repos)(at_path, home.as_deref());
                let measured = Measured {
                    at_ms: at,
                    bytes: walked.map(|(bytes, _)| bytes),
                    complete: walked.is_some_and(|(_, complete)| complete),
                    unpushed,
                };
                MEASURED
                    .write()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(path, measured);
            }
        }
        #[cfg(test)]
        Job::Flush(done) => {
            let _ = done.send(());
        }
    }
}

/// Sample the trees of request tabs `tabs` (sid and shell pid) on the footprint thread.
pub(crate) fn sample(tabs: Vec<(String, i32)>, probe: crate::tab_marks::TreeProbe) {
    if !tabs.is_empty() {
        post(Job::Sample { tabs, probe });
    }
}

/// Measure the claimed folders `paths` at `at` (Unix ms) on the footprint thread.
pub(crate) fn measure(at: u64, paths: Vec<String>, home: Option<PathBuf>, probe: MeasureProbe) {
    if !paths.is_empty() {
        post(Job::Measure {
            at,
            paths,
            home,
            probe,
        });
    }
}

/// Wait at most `budget` for every job handed to the footprint thread: a test's.
#[cfg(test)]
pub(crate) fn flush_within(budget: Duration) -> bool {
    if FOOTPRINT
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .is_none()
    {
        return true;
    }
    let (tx, rx) = mpsc::channel();
    post(Job::Flush(tx));
    rx.recv_timeout(budget).is_ok()
}

#[cfg(test)]
#[path = "requests_footprint_tests.rs"]
mod tests;
