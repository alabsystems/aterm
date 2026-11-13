// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The request store's intake on this Mac (ATERM-KI-1109-3, steps 2 and 3 of the requests
//! design): who asks a spawn ([`asker`]), the request a spawn opens or joins ([`prepare`],
//! [`begin`], [`hand`]), the later verbs that attach to it ([`owned_target`], [`attach_after`]),
//! a tab's leaving ([`left`]), a leased plain shell's end ([`lease_look`]), a person's Keep as Tab
//! ([`kept`]) and the sweep after a relaunch ([`sweep`]), all kept by [`aterm_requests::Store`] in
//! the state home's `requests/`; and the indexes the main thread reads, which only the writer
//! thread updates: which tab is whose ([`placed`]) and the newest requests ([`listed`]).
//!
//! Every process keeps one store, opened lazily, and reads what any other process (the one a
//! seamless update replaces, or a second instance on this state home) appended before it acts,
//! so a request outlives the process that made it. What a spawn made of its request (its tab,
//! or its refusal) and a tab's leaving both start on the main thread, in the order the tab lived,
//! and are written in that order by this module's one writer thread, `requests-journal`, which a
//! quit or an update's exec flushes within its budget ([`flush_within`]).

use crate::session_store::{ExitActor, Store};
use aterm_requests::{Brief, EndWhy, Form, From, Intake, Out, Stage, Verb, Via, Why, WhySrc};
use aterm_session::SessionId;
use aterm_types::control_request::{Head, Selector};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock, mpsc};
use std::time::Duration;

/// This process's store, by the folder it is of: one in a running aterm (the state home's), one
/// per test thread's folder in tests. A store is kept for the process's life, since it holds
/// the `req=` joins still pending ([`aterm_requests::Store::join`]).
static STORE: Mutex<BTreeMap<PathBuf, aterm_requests::Store>> = Mutex::new(BTreeMap::new());

#[cfg(test)]
thread_local! {
    static DIR: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// The store's folder: the state home's `requests/`; `Ok(None)` in a test that set none
/// ([`set_dir_for_test`]), which then makes no request.
pub(crate) fn dir() -> Result<Option<PathBuf>, &'static str> {
    #[cfg(test)]
    {
        Ok(DIR.with(|dir| dir.borrow().clone()))
    }
    #[cfg(not(test))]
    {
        aterm_types::dirs::state_dir()
            .map(|state| Some(state.join(aterm_requests::DIR)))
            .ok_or("this Mac has no state home")
    }
}

/// Keep this test thread's requests in `dir` (`None`: make none).
#[cfg(test)]
pub(crate) fn set_dir_for_test(dir: Option<PathBuf>) {
    DIR.with(|slot| *slot.borrow_mut() = dir);
}

/// Drop this test thread's store and index, as a process the update replaces loses them.
#[cfg(test)]
pub(crate) fn forget_for_test() {
    if let Ok(Some(dir)) = dir() {
        STORE.lock().unwrap_or_else(|p| p.into_inner()).remove(&dir);
        PLACED
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&dir);
        LISTED
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&dir);
    }
}

/// A request opened for `by` on node `node` (Mac `host`; over a fleet drive where `fleet`), whose
/// spawn of `made` landed tab `sid`, recorded and indexed: its id. Waits on the writer thread as a
/// hang detector ([`Receipt::named`]).
#[cfg(test)]
pub(crate) fn opened_for_test(
    fleet: bool,
    node: &str,
    host: &str,
    by: &str,
    made: Made,
    sid: &str,
) -> String {
    opened_why_for_test((fleet, node, host, by), made, sid, Ok("a test's request"))
}

/// [`opened_for_test`] for asker `(fleet, node, host, by)`, the request's why `why`: `Ok` the
/// requester's own line, `Err` a brief its fallback why is taken from (the content tier).
#[cfg(test)]
pub(crate) fn opened_why_for_test(
    (fleet, node, host, by): (bool, &str, &str, &str),
    made: Made,
    sid: &str,
    why: Result<&str, &str>,
) -> String {
    let asker = Asker {
        fleet,
        from: From {
            node: node.to_owned(),
            host: host.to_owned(),
            by: by.to_owned(),
            program: None,
            model: None,
            account: None,
        },
        goal: None,
        asked: None,
    };
    let (given, brief) = match why {
        Ok(given) => (Some(given), None),
        Err(brief) => (None, Some(brief.as_bytes())),
    };
    let prepared = prepare(Some(&asker), None, made, given, brief)
        .expect("prepared")
        .expect("a folder is set");
    let (pending, receipt) = hand(begin(Some(prepared)).expect("begun"), None);
    pending.expect("a request").settle(Some(sid));
    receipt
        .expect("a receipt")
        .named()
        .expect("the tab recorded")
}

/// A request-owned tab as the main thread reads it ([`placed`]): how its request came in, who
/// asked, and how it concluded.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Placed {
    /// Its request's id.
    pub(crate) id: String,
    /// When its request opened, Unix ms.
    pub(crate) opened_ms: u64,
    /// When its request concluded, Unix ms.
    pub(crate) ended_ms: Option<u64>,
    pub(crate) via: Via,
    /// The requester's node id (empty where it has none).
    pub(crate) node: String,
    /// The requester's Mac as a person reads it (empty where unknown).
    pub(crate) host: String,
    pub(crate) end: Option<(Out, EndWhy)>,
    /// Its why: the requester's own line, the fallback it recorded (read from the content
    /// tier), or [`aterm_requests::why::NONE_GIVEN`].
    pub(crate) why: Arc<str>,
    /// Where its why came from: a reader that serves no content shows a fallback's as `1`.
    pub(crate) why_src: WhySrc,
}

/// Every request-owned tab this process indexed, by tab sid, per store folder (as [`STORE`]):
/// the writer thread updates it after each write and the main thread only reads it, so the main
/// thread never waits on the journal's flush. A concluded request's tabs stay in it, since a
/// lease's end leaves its pane standing.
static PLACED: RwLock<BTreeMap<PathBuf, HashMap<String, Arc<Placed>>>> =
    RwLock::new(BTreeMap::new());

/// The request that owns tab `sid`, or, for a shell a cold restore stood in for a tab
/// (`lineage`, the first session of its chain of restores), that tab's: from the index alone.
pub(crate) fn placed(sid: &str, lineage: Option<&str>) -> Option<Arc<Placed>> {
    let dir = dir().ok().flatten()?;
    let placed = PLACED.read().unwrap_or_else(|p| p.into_inner());
    let index = placed.get(&dir)?;
    index
        .get(sid)
        .or_else(|| lineage.and_then(|lineage| index.get(lineage)))
        .cloned()
}

/// Whether a request this process indexed is open, or concluded at or after `since_ms` (Unix
/// ms): a look then reads its tabs' commands, the last ones of a request just concluded too.
pub(crate) fn any_live(since_ms: u64) -> bool {
    let Some(dir) = dir().ok().flatten() else {
        return false;
    };
    let placed = PLACED.read().unwrap_or_else(|p| p.into_inner());
    placed.get(&dir).is_some_and(|index| {
        index
            .values()
            .any(|placed| placed.ended_ms.is_none_or(|ended| ended >= since_ms))
    })
}

/// Request `id`'s why and where it came from: its given line, the fallback the content tier
/// holds, or [`aterm_requests::why::NONE_GIVEN`]. Only the writer thread and an owner's read ask,
/// since it may open the content tier.
pub(crate) fn why_of(store: &mut aterm_requests::Store, id: &str) -> Option<(Arc<str>, WhySrc)> {
    let (given, src) = store
        .get(id)
        .map(|request| (request.why.clone(), request.open.why_src))?;
    let text = match src {
        WhySrc::Given => given,
        WhySrc::None => None,
        WhySrc::Brief | WhySrc::Goal | WhySrc::Asked => store
            .said(id)
            .and_then(|said| said.why)
            .map(|(text, _)| text),
    };
    let text = text.unwrap_or_else(|| aterm_requests::why::NONE_GIVEN.to_owned());
    Some((Arc::from(text), src))
}

/// Index requests `ids` of `store`, the store of folder `dir`: their tabs in [`PLACED`], and the
/// newest requests again in [`LISTED`].
fn republish<'a>(
    dir: &Path,
    store: &mut aterm_requests::Store,
    ids: impl IntoIterator<Item = &'a str>,
) {
    let mut entries = Vec::new();
    for id in ids {
        let Some((why, why_src)) = why_of(store, id) else {
            continue;
        };
        let Some(request) = store.get(id) else {
            continue;
        };
        let entry = Arc::new(Placed {
            id: request.id.clone(),
            opened_ms: request.opened_ms,
            ended_ms: request.ended_ms,
            via: request.open.via,
            node: request.open.node.clone(),
            host: request.open.host.clone(),
            end: request.end,
            why,
            why_src,
        });
        let tabs: Vec<String> = request.tabs.iter().map(|tab| tab.sid.clone()).collect();
        entries.push((entry, tabs));
    }
    {
        let mut placed = PLACED.write().unwrap_or_else(|p| p.into_inner());
        let index = placed.entry(dir.to_path_buf()).or_default();
        for (entry, tabs) in entries {
            for tab in tabs {
                index.insert(tab, Arc::clone(&entry));
            }
        }
    }
    relist(dir, store);
}

/// The most requests [`LISTED`] holds: the newest.
pub(crate) const LIST_MAX: usize = 200;

/// One request as the Workers page's Requests section (step 5c) reads it ([`listed`]).
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ListedRow {
    pub(crate) id: String,
    pub(crate) opened_ms: u64,
    pub(crate) stage: Stage,
    /// How it came out, once concluded.
    pub(crate) out: Option<Out>,
    pub(crate) via: Via,
    /// The requester's node id (empty where it has none) and Mac (empty where unknown).
    pub(crate) node: String,
    pub(crate) host: String,
    /// The requester as its records name it (`fleet:<head>:<sid|->`, or a local tab's sid).
    pub(crate) by: String,
    pub(crate) program: Option<String>,
    pub(crate) tabs: usize,
    pub(crate) open_tabs: usize,
    /// Its claimed folders' measured bytes, and whether every one was measured whole; `None`
    /// where none was measured within [`crate::requests_footprint::MEASURE_TTL_MS`].
    pub(crate) claim_bytes: Option<(u64, bool)>,
    pub(crate) why: Arc<str>,
    pub(crate) why_src: WhySrc,
    pub(crate) kept: bool,
}

/// The newest requests of a store, newest first ([`LISTED`]).
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Listed {
    /// At most [`LIST_MAX`], newest first.
    pub(crate) rows: Vec<Arc<ListedRow>>,
    /// How many requests of the whole store are concluded and not reviewed.
    pub(crate) unreviewed: usize,
}

/// Every store folder's newest requests ([`Listed`]): the writer thread rebuilds it after each
/// write and the main thread only reads it, as [`PLACED`].
static LISTED: RwLock<BTreeMap<PathBuf, Arc<Listed>>> = RwLock::new(BTreeMap::new());

/// This process's store's newest requests, as the writer last listed them: from memory alone,
/// so the main thread may ask.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the Workers page's Requests section (requests step 5c) reads it"
    )
)]
pub(crate) fn listed() -> Option<Arc<Listed>> {
    let dir = dir().ok().flatten()?;
    LISTED
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .get(&dir)
        .cloned()
}

/// What claims `request` holds measured: the sum of each claim's last measure within its
/// lifetime, and whether each was measured whole; `None` where none was.
pub(crate) fn claim_bytes(request: &aterm_requests::Request, now_ms: u64) -> Option<(u64, bool)> {
    let (mut any, mut total, mut whole) = (false, 0_u64, true);
    for held in &request.claims {
        match crate::requests_footprint::measured(&held.claim.path, now_ms)
            .and_then(|measured| measured.bytes.map(|bytes| (bytes, measured.complete)))
        {
            Some((bytes, complete)) => {
                any = true;
                total = total.saturating_add(bytes);
                whole &= complete;
            }
            None => whole = false,
        }
    }
    any.then_some((total, whole))
}

/// Rebuild [`LISTED`] for folder `dir` from `store`: its newest [`LIST_MAX`] requests.
fn relist(dir: &Path, store: &mut aterm_requests::Store) {
    let now = now_unix_ms();
    let mut newest: Vec<(u64, String)> = store
        .requests()
        .map(|request| (request.opened_ms, request.id.clone()))
        .collect();
    newest.sort_by(|a, b| b.cmp(a));
    let unreviewed = store
        .requests()
        .filter(|request| request.stage() == Stage::Concluded)
        .count();
    let mut rows = Vec::new();
    for (_, id) in newest.into_iter().take(LIST_MAX) {
        let Some((why, why_src)) = why_of(store, &id) else {
            continue;
        };
        let Some(request) = store.get(&id) else {
            continue;
        };
        rows.push(Arc::new(ListedRow {
            id,
            opened_ms: request.opened_ms,
            stage: request.stage(),
            out: request.end.map(|(out, _)| out),
            via: request.open.via,
            node: request.open.node.clone(),
            host: request.open.host.clone(),
            by: request.open.by.clone(),
            program: request.open.program.clone(),
            tabs: request.tabs.len(),
            open_tabs: request.tabs.iter().filter(|tab| tab.is_open()).count(),
            claim_bytes: claim_bytes(request, now),
            why,
            why_src,
            kept: request.kept.is_some(),
        }));
    }
    LISTED
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .insert(dir.to_path_buf(), Arc::new(Listed { rows, unreviewed }));
}

/// `act` on the store of folder `dir`, opening it where this process holds none.
pub(crate) fn with_store<R>(
    dir: &Path,
    act: impl FnOnce(&mut aterm_requests::Store) -> std::io::Result<R>,
) -> std::io::Result<R> {
    let mut stores = STORE.lock().unwrap_or_else(|p| p.into_inner());
    let store = match stores.entry(dir.to_path_buf()) {
        std::collections::btree_map::Entry::Occupied(held) => held.into_mut(),
        std::collections::btree_map::Entry::Vacant(slot) => {
            slot.insert(aterm_requests::Store::open(dir)?)
        }
    };
    act(store)
}

/// Unix milliseconds now.
pub(crate) fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

/// Who asks for a spawn, where it is someone a request is made for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Asker {
    /// Asked over another Mac's fleet drive.
    fleet: bool,
    from: From,
    goal: Option<String>,
    asked: Option<String>,
}

/// [`asker_as`] for the connection being served.
pub(crate) fn asker(scope: crate::control::Scope, store: &Store) -> Option<Asker> {
    asker_as(scope, store, || {
        crate::typed_by::Who::client(crate::control::serving_peer_pid())
    })
}

/// Who asks on this connection where a request is made for it: another Mac's drive (its proven
/// node and host, and the requester its first line named: `fleet:<head>:<sid|->`), or a tab of
/// this Mac where an agent runs, found as [`crate::control::caller_actor_as`]
/// finds it (`client` naming the connection's process). `None` for anyone else: a person's
/// shell, an `aterm ctl` in no tab, aterm's harness (whose acts are intake of their own, step 7)
/// and the bridge.
pub(crate) fn asker_as(
    scope: crate::control::Scope,
    store: &Store,
    client: impl FnOnce() -> crate::typed_by::Who,
) -> Option<Asker> {
    if let Some(driver) = crate::fabric::serving_driver() {
        let requester = crate::fabric::serving_requester().unwrap_or_default();
        let ExitActor::Fleet { token: by, node } =
            ExitActor::fleet_drive(&driver.node, requester.sid())
        else {
            return None;
        };
        return Some(Asker {
            fleet: true,
            from: From {
                node,
                host: driver.host,
                by,
                program: requester.program().map(str::to_owned),
                model: requester.model().map(str::to_owned),
                account: requester.account().map(str::to_owned),
            },
            goal: None,
            asked: None,
        });
    }
    let ExitActor::Sid(sid) = crate::control::caller_actor_as(scope, None, store, client) else {
        return None;
    };
    let asking = {
        let sessions = store.read().unwrap_or_else(|p| p.into_inner());
        let handle = sessions.by_sid(&SessionId::new(&sid))?;
        crate::survey::asking_of(handle)?
    };
    let requester = aterm_types::control_verbs::Requester::of(
        Some(&sid),
        asking.program.as_deref(),
        asking.model.as_deref(),
        asking.account.as_deref(),
    );
    Some(Asker {
        fleet: false,
        from: From {
            node: own_node(),
            host: own_host(),
            by: sid,
            program: requester.program().map(str::to_owned),
            model: requester.model().map(str::to_owned),
            account: requester.account().map(str::to_owned),
        },
        goal: asking.goal,
        asked: asking.asked,
    })
}

/// This Mac's node id, which a local request's id names; empty (`local`) where it has none,
/// which is every non-Unix build: fleet membership (`aterm_net::fleet_member`) is Unix-only.
pub(crate) fn own_node() -> String {
    #[cfg(any(test, not(unix)))]
    {
        String::new()
    }
    #[cfg(all(not(test), unix))]
    {
        aterm_types::dirs::state_dir()
            .and_then(|state| aterm_net::fleet_member::read_node_id(&state).ok().flatten())
            .unwrap_or_default()
    }
}

/// The node of actor `by`: a drive's proven node, else this Mac's.
fn node_of(by: &ExitActor) -> String {
    by.fleet_node().map_or_else(own_node, str::to_owned)
}

/// This Mac's name on its LAN.
pub(crate) fn own_host() -> String {
    #[cfg(test)]
    {
        "this-mac".to_owned()
    }
    #[cfg(not(test))]
    {
        aterm_uds::process::lan_host_name().unwrap_or_default()
    }
}

/// What a spawn will do about its request, decided before anything is written.
pub(crate) enum Prepared {
    /// Open a request.
    Open { dir: PathBuf, intake: Box<Intake> },
    /// Join request `id` (`req=`).
    Join {
        dir: PathBuf,
        id: String,
        node: String,
        by: String,
    },
}

/// What a spawn's form is, as a request records it.
#[derive(Clone, Copy)]
pub(crate) enum Made {
    /// A plain shell.
    Shell,
    /// A worker of this agent word.
    Agent(&'static str),
    /// A program worker.
    Program,
    /// A connected pair.
    Connected,
}

/// The request of a spawn `asker` asks for: none where no one a request is made for asks
/// ([`asker`]) or this Mac keeps none (a test that set no folder); a join where the line named
/// `req=<id>`; else a new request, its why `why` or the documented fallback
/// ([`aterm_requests::why_of`]) and its brief `brief` (a worker's task). Pure: nothing is written
/// until [`begin`].
/// # Errors
/// The spawn's refusal: a `req=` from a caller that makes no request, or a Mac with no state home.
pub(crate) fn prepare(
    asker: Option<&Asker>,
    req: Option<&str>,
    made: Made,
    why: Option<&str>,
    brief: Option<&[u8]>,
) -> Result<Option<Prepared>, String> {
    let Some(asker) = asker else {
        return match req {
            Some(id) => Err(format!(
                "ERR spawn req={id}: this caller makes no request\n"
            )),
            None => Ok(None),
        };
    };
    let Some(dir) = dir().map_err(|why| format!("ERR request store: {why}\n"))? else {
        return Ok(None);
    };
    if let Some(id) = req {
        return Ok(Some(Prepared::Join {
            dir,
            id: id.to_owned(),
            node: asker.from.node.clone(),
            by: asker.from.by.clone(),
        }));
    }
    let via = match (asker.fleet, made) {
        (true, Made::Shell) => Via::FleetShell,
        (true, _) => Via::FleetWorker,
        (false, _) => Via::CtlSpawn,
    };
    let form = match made {
        Made::Shell => Form::Shell,
        Made::Agent(agent) => Form::Agent(agent.to_owned()),
        Made::Program => Form::Program,
        Made::Connected => Form::Connected,
    };
    let Why { text, src } = aterm_requests::why_of(
        via,
        why,
        brief,
        asker.goal.as_deref(),
        asker.asked.as_deref(),
    );
    Ok(Some(Prepared::Open {
        dir,
        intake: Box::new(Intake {
            via,
            form,
            from: asker.from.clone(),
            why: Why { text, src },
            brief: brief.map(Brief::of),
            goal: asker.goal.clone(),
            asked: asker.asked.clone(),
        }),
    }))
}

/// A request a spawn opened or joined, awaiting its tab.
pub(crate) struct Begun {
    dir: PathBuf,
    id: String,
    by: String,
    /// Named by `req=` ([`aterm_requests::Store::join`]), not opened by this spawn.
    joined: bool,
}

/// Write what `prepared` decided: the request's `open` (before the spawn reaches the main
/// thread, so nothing runs on another's behalf without one), or the check that `req=` names an
/// open request of the same requester, which holds it open until the spawn settles.
/// # Errors
/// The spawn's refusal: the store could not record the request, or the join is refused.
pub(crate) fn begin(prepared: Option<Prepared>) -> Result<Option<Begun>, String> {
    match prepared {
        None => Ok(None),
        Some(Prepared::Open { dir, intake }) => {
            let by = intake.from.by.clone();
            let id = with_store(&dir, |store| store.open_request(now_unix_ms(), *intake))
                .map_err(|error| format!("ERR request store: {error}\n"))?;
            Ok(Some(Begun {
                dir,
                id,
                by,
                joined: false,
            }))
        }
        Some(Prepared::Join { dir, id, node, by }) => {
            let joined = with_store(&dir, |store| Ok(store.join(&id, &node, &by)))
                .map_err(|error| format!("ERR request store: {error}\n"))?;
            joined.map_err(|why| format!("ERR spawn req={id}: {why}\n"))?;
            Ok(Some(Begun {
                dir,
                id,
                by,
                joined: true,
            }))
        }
    }
}

/// What the writer needs to settle a spawn's request.
#[derive(Debug)]
struct Settle {
    dir: PathBuf,
    id: String,
    by: String,
    joined: bool,
    /// The folder the spawn opens in, as a request's root ([`crate::requests_footprint::root_of`]).
    root: Option<String>,
    /// Whether the tab was recorded, for the control thread's reply ([`Receipt::named`]).
    recorded: mpsc::Sender<bool>,
}

/// A spawn's request, carried to the main thread with the spawn: the main thread settles it as
/// the spawn answers ([`Pending::settle`]), so its tab is queued for the writer before anything
/// can queue that tab's leaving, and a spawn the main thread acts on after its control thread
/// stopped waiting is still recorded. Dropped unsettled (the spawn never reached the main thread,
/// or was dropped there), its spawn counts as refused.
#[derive(Debug)]
pub(crate) struct Pending(Option<Settle>);

/// The control thread's half of a [`Pending`]: whether the tab was recorded.
pub(crate) struct Receipt {
    id: String,
    recorded: mpsc::Receiver<bool>,
}

/// Split `begun` into what goes to the main thread with the spawn and what its control thread
/// keeps for the reply; `cwd` the folder the spawn opens in, which its tab records as the
/// request's root where a claim of it could be made ([`crate::requests_footprint::root_of`],
/// [`aterm_requests::Store::tab`]).
pub(crate) fn hand(begun: Option<Begun>, cwd: Option<&str>) -> (Option<Pending>, Option<Receipt>) {
    let Some(Begun {
        dir,
        id,
        by,
        joined,
    }) = begun
    else {
        return (None, None);
    };
    let (recorded, rx) = mpsc::channel();
    let receipt = Receipt {
        id: id.clone(),
        recorded: rx,
    };
    let root = cwd.and_then(|cwd| {
        crate::requests_footprint::root_of(cwd, &crate::requests_footprint::Places::host())
    });
    let pending = Pending(Some(Settle {
        dir,
        id,
        by,
        joined,
        root,
        recorded,
    }));
    (Some(pending), Some(receipt))
}

impl Pending {
    /// The spawn answered: `landed` its tab's sid, which joins the request, or `None` for a
    /// refusal, which concludes a request left with no tab ([`aterm_requests::Store::refused`]).
    /// Queued for the writer thread from the main thread.
    pub(crate) fn settle(mut self, landed: Option<&str>) {
        if let Some(settle) = self.0.take() {
            post(Item::Settle {
                at: now_unix_ms(),
                settle,
                sid: landed.map(str::to_owned),
            });
        }
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if let Some(settle) = self.0.take() {
            post(Item::Settle {
                at: now_unix_ms(),
                settle,
                sid: None,
            });
        }
    }
}

/// How long a spawn's reply waits for the writer to record its tab ([`Receipt::named`]): one
/// ordered flush behind whatever leavings were queued first.
const RECORD_WAIT: Duration = Duration::from_secs(10);

impl Receipt {
    /// The request the reply names (`req=<id>`), once the writer recorded the spawn's tab in it,
    /// so a verb the requester sends next finds the tab its request's; `None` where the tab could
    /// not be recorded within [`RECORD_WAIT`] (the request then stays open, as a crash leaves
    /// one, or takes the tab later).
    pub(crate) fn named(self) -> Option<String> {
        matches!(self.recorded.recv_timeout(RECORD_WAIT), Ok(true)).then_some(self.id)
    }
}

fn write_settle(at: u64, settle: Settle, sid: Option<&str>) {
    let Settle {
        dir,
        id,
        by,
        joined,
        root,
        recorded,
    } = settle;
    let done = match sid {
        Some(sid) => with_store(&dir, |store| {
            store.tab(at, &id, sid, &by, joined, root.as_deref())?;
            republish(&dir, store, [id.as_str()]);
            Ok(())
        })
        .map_err(|error| aterm_log::warn!("requests: tab {sid} of {id} not recorded: {error}"))
        .is_ok(),
        None => {
            if let Err(error) = with_store(&dir, |store| {
                store.refused(at, &id, joined)?;
                republish(&dir, store, [id.as_str()]);
                Ok(())
            }) {
                aterm_log::warn!("requests: refused spawn of {id} not recorded: {error}");
            }
            false
        }
    };
    let _ = recorded.send(done);
}

/// A verb that may attach to a request, and the tab it names, read before the verb runs (a
/// `close` takes the tab away).
pub(crate) struct Owned {
    dir: PathBuf,
    tab: String,
    verb: Verb,
    /// The requester's node and actor token.
    node: String,
    requester: String,
}

/// The request-owned tab `head` drives, where its verb is one that attaches (`turn`, `send`,
/// `key`, `close`) and a request owns the tab it names. Cheap where no request owns it: the
/// store's index, after a check that no other process appended.
pub(crate) fn owned_target(head: &Head<'_>, store: &Store) -> Option<Owned> {
    let verb = Verb::of(head.verb)?;
    let selector = head.selector.as_ref()?;
    if matches!(selector, Selector::SelfTok | Selector::Invalid(_)) {
        return None;
    }
    let tab = {
        let sessions = store.read().unwrap_or_else(|p| p.into_inner());
        sessions.by_selector(selector)?.sid.as_str().to_owned()
    };
    let dir = dir().ok().flatten()?;
    if !dir.join(aterm_requests::JOURNAL).exists() {
        return None;
    }
    let (_, node, requester) = with_store(&dir, |store| Ok(store.owner(&tab))).ok()??;
    Some(Owned {
        dir,
        tab,
        verb,
        node,
        requester,
    })
}

/// After `owned`'s verb answered `reply`: where it took effect (input typed; a close done) and
/// `by` (who asked on this connection, read only now; its node a drive's proven one, else this
/// Mac's) is the request's requester, the verb attaches to the request.
pub(crate) fn attach_after(owned: Owned, reply: &str, by: &ExitActor) {
    let took = match owned.verb {
        Verb::Close => reply.starts_with("OK"),
        Verb::Turn | Verb::Send | Verb::Key => crate::control::typed_into(reply),
    };
    if !took || by.as_wire() != owned.requester {
        return;
    }
    let node = node_of(by);
    if node != owned.node {
        return;
    }
    let at = now_unix_ms();
    if let Err(error) = with_store(&owned.dir, |store| {
        store.attach(at, &owned.tab, owned.verb, &node, by.as_wire())
    }) {
        aterm_log::warn!(
            "requests: {} on {} not recorded: {error}",
            owned.verb.word(),
            owned.tab
        );
    }
}

/// How a tab left, as its close knows it.
pub(crate) struct TabLeft {
    pub(crate) sid: SessionId,
    pub(crate) reason: crate::session_store::ExitReason,
    pub(crate) code: Option<i32>,
    pub(crate) task: Option<aterm_requests::TaskEnd>,
    pub(crate) by: ExitActor,
    /// What its process tree spent, as the close's last read found it.
    pub(crate) spent: Option<aterm_requests::Spent>,
}

enum Item {
    /// A spawn's request settled: its tab `sid`, or `None` for a refusal.
    Settle {
        at: u64,
        settle: Settle,
        sid: Option<String>,
    },
    Left {
        dir: PathBuf,
        at: u64,
        left: TabLeft,
    },
    /// Index every request of the store ([`load`]).
    Load {
        dir: PathBuf,
    },
    /// A lease's look ([`lease_look`]).
    Look {
        dir: PathBuf,
        at: u64,
        look: aterm_requests::Look,
    },
    /// A person kept these tabs ([`kept`]).
    Keep {
        dir: PathBuf,
        at: u64,
        sids: Vec<String>,
        by: String,
    },
    /// The sweep after a relaunch ([`sweep`]).
    Sweep {
        dir: PathBuf,
        sweep: Sweep,
    },
    /// The minute's command spans ([`spans`]).
    Spans {
        dir: PathBuf,
        hand: crate::requests_footprint::Hand,
    },
    /// A person's act on a request was written on a control thread: list the store again
    /// ([`relisted`]).
    Relist {
        dir: PathBuf,
    },
    Flush(mpsc::Sender<()>),
}

static WRITER: Mutex<Option<mpsc::Sender<Item>>> = Mutex::new(None);

/// Hand `item` to the writer thread, starting it where it is not running.
fn post(item: Item) {
    let mut writer = WRITER.lock().unwrap_or_else(|p| p.into_inner());
    let item = match writer.as_ref() {
        Some(tx) => match tx.send(item) {
            Ok(()) => return,
            Err(mpsc::SendError(item)) => item,
        },
        None => item,
    };
    let (tx, rx) = mpsc::channel::<Item>();
    let started = std::thread::Builder::new()
        .name("requests-journal".to_owned())
        .spawn(move || {
            for item in rx {
                match item {
                    Item::Settle { at, settle, sid } => write_settle(at, settle, sid.as_deref()),
                    Item::Left { dir, at, left } => write_left(&dir, at, &left),
                    Item::Load { dir } => write_load(&dir),
                    Item::Look { dir, at, look } => write_look(&dir, at, &look),
                    Item::Keep { dir, at, sids, by } => write_keep(&dir, at, &sids, &by),
                    Item::Sweep { dir, sweep } => write_sweep(&dir, &sweep),
                    Item::Spans { dir, hand } => write_spans(&dir, &hand),
                    Item::Relist { dir } => {
                        if let Err(error) = with_store(&dir, |store| {
                            relist(&dir, store);
                            Ok(())
                        }) {
                            aterm_log::warn!("requests: not listed: {error}");
                        }
                    }
                    Item::Flush(done) => {
                        let _ = done.send(());
                    }
                }
            }
        });
    match started {
        Ok(_) => {
            let _ = tx.send(item);
            *writer = Some(tx);
        }
        Err(error) => aterm_log::warn!("requests: no writer thread: {error}"),
    }
}

fn write_left(dir: &Path, at: u64, left: &TabLeft) {
    if !dir.join(aterm_requests::JOURNAL).exists() {
        return;
    }
    let sid = left.sid.as_str();
    // The close's read, or what the samples saw while it ran where that is more: a process that
    // ended before the close is in the samples only.
    let sampled = crate::requests_footprint::spend_of(sid);
    crate::requests_footprint::forget_spend(sid);
    let spent = match (left.spent, sampled) {
        (Some(read), Some(sampled)) => Some(read.max(sampled)),
        (read, sampled) => read.or(sampled),
    };
    let record = aterm_requests::Left {
        reason: left.reason.as_str().to_owned(),
        code: left.code,
        task: left.task,
        by: left.by.as_wire().to_owned(),
        node: Some(node_of(&left.by)),
        spent,
    };
    if let Err(error) = with_store(dir, |store| {
        store.left(at, sid, &record)?;
        if let Some((id, ..)) = store.owner(sid) {
            republish(dir, store, [id.as_str()]);
        }
        Ok(())
    }) {
        aterm_log::warn!("requests: {sid} left, not recorded: {error}");
    }
}

fn write_load(dir: &Path) {
    if !dir.join(aterm_requests::JOURNAL).exists() {
        return;
    }
    if let Err(error) = with_store(dir, |store| {
        let ids: Vec<String> = store
            .placements()
            .map(|request| request.id.clone())
            .collect();
        republish(dir, store, ids.iter().map(String::as_str));
        note_claims(dir, store);
        Ok(())
    }) {
        aterm_log::warn!("requests: not read: {error}");
    }
}

/// Each request a write concluded, in the log, and indexed again.
fn note_ended(dir: &Path, store: &mut aterm_requests::Store, ended: &[aterm_requests::Ended]) {
    for end in ended {
        aterm_log::info!(
            "requests: {} concluded {} (why={}), its tabs left standing",
            end.id,
            end.out.word(),
            end.why.word()
        );
    }
    republish(dir, store, ended.iter().map(|end| end.id.as_str()));
}

fn write_look(dir: &Path, at: u64, look: &aterm_requests::Look) {
    if !dir.join(aterm_requests::JOURNAL).exists() {
        return;
    }
    match with_store(dir, |store| {
        let ended = store.lease(at, look)?;
        note_ended(dir, store, &ended);
        Ok(ended)
    }) {
        // Its requester's lease is over: its later drives into those tabs are a person's tab's.
        Ok(ended) => {
            for tab in ended.iter().flat_map(|end| &end.tabs) {
                crate::control::forget_driven_shell(&SessionId::new(tab));
            }
        }
        Err(error) => aterm_log::warn!("requests: lease look not recorded: {error}"),
    }
}

fn write_keep(dir: &Path, at: u64, sids: &[String], by: &str) {
    if !dir.join(aterm_requests::JOURNAL).exists() {
        return;
    }
    for sid in sids {
        if let Err(error) = with_store(dir, |store| {
            let ended = store.keep(at, sid, by)?;
            if let Some(end) = &ended {
                aterm_log::info!(
                    "requests: {} concluded {} (why=kept)",
                    end.id,
                    end.out.word()
                );
            }
            if let Some((id, ..)) = store.owner(sid) {
                republish(dir, store, [id.as_str()]);
            }
            Ok(())
        }) {
            aterm_log::warn!("requests: keep of {sid} not recorded: {error}");
        }
    }
}

fn write_sweep(dir: &Path, sweep: &Sweep) {
    if sweep.swept.load(Ordering::Acquire) || !(sweep.alone)() {
        return;
    }
    sweep.swept.store(true, Ordering::Release);
    if !dir.join(aterm_requests::JOURNAL).exists() {
        return;
    }
    if let Err(error) = with_store(dir, |store| {
        let ended = store.sweep(sweep.at, sweep.born_ms, &sweep.hosted)?;
        note_ended(dir, store, &ended);
        Ok(())
    }) {
        aterm_log::warn!("requests: relaunch sweep not recorded: {error}");
    }
}

/// The minute's footprint look ([`crate::requests_footprint::plan`]): the store is held to read
/// what the scan needs and again to record what it found, never across the scan's listing of
/// folders, so a control verb that takes the store (a drive's `turn`, a spawn, `request claim`)
/// never waits on a slow folder.
fn write_spans(dir: &Path, hand: &crate::requests_footprint::Hand) {
    use crate::requests_footprint::{plan, record, scan};
    if !dir.join(aterm_requests::JOURNAL).exists() {
        return;
    }
    let planned = match with_store(dir, |store| Ok(plan(dir, store, hand))) {
        Ok(planned) => planned,
        Err(error) => {
            aterm_log::warn!("requests: footprint look not read: {error}");
            return;
        }
    };
    let found = scan(&planned, hand);
    match with_store(dir, |store| {
        let new = record(store, hand, &planned, &found);
        note_claims(dir, store);
        new
    }) {
        Ok(new) => crate::requests_footprint::measure(
            hand.at,
            new.into_iter().map(|(path, _)| path).collect(),
            hand.places.home.clone(),
            hand.measure,
        ),
        Err(error) => aterm_log::warn!("requests: footprint look not recorded: {error}"),
    }
}

/// The store folders holding a claim no person has touched yet, whose spans the minute's look
/// reads whether or not a request lives ([`watching`]).
static WATCHED: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

/// Note whether `store`, the store of folder `dir`, holds a claim no person has touched: a later
/// cleanup could act on it, so a person's work in it must still be seen.
pub(crate) fn note_claims(dir: &Path, store: &mut aterm_requests::Store) {
    let claims: Vec<(String, String)> = store
        .claims()
        .map(|(path, id)| (path.to_owned(), id.to_owned()))
        .collect();
    let open = claims.iter().any(|(path, id)| {
        store
            .get(id)
            .is_some_and(|request| !request.touched.contains(path))
    });
    let mut watched = WATCHED.lock().unwrap_or_else(|p| p.into_inner());
    if open {
        watched.insert(dir.to_path_buf());
    } else {
        watched.remove(dir);
    }
}

/// Whether this process's store holds a claim no person has touched yet ([`note_claims`]), as
/// the writer last noted: from memory alone, so the main thread may ask.
pub(crate) fn watching() -> bool {
    let Some(dir) = dir().ok().flatten() else {
        return false;
    };
    WATCHED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .contains(&dir)
}

/// The minute's command spans and typing ([`crate::requests_footprint::Hand`]), judged off the
/// main thread for the folders requests made and a person's later work in them.
pub(crate) fn spans(hand: crate::requests_footprint::Hand) {
    if let Ok(Some(dir)) = dir() {
        post(Item::Spans { dir, hand });
    }
}

/// A person's act on a request of folder `dir` (a review, a keep, a reply) was written on a
/// control thread: the writer lists the store again ([`listed`]), so only it publishes the index.
pub(crate) fn relisted(dir: &Path) {
    post(Item::Relist {
        dir: dir.to_path_buf(),
    });
}

/// Read every request into the index [`placed`] reads, off the main thread: once, as the app
/// starts, so a seamless update's next process or a relaunch finds which tab is whose.
pub(crate) fn load() {
    if let Ok(Some(dir)) = dir() {
        post(Item::Load { dir });
    }
}

/// A lease's look at `at` (Unix ms) at the leased plain shells this process hosts
/// ([`aterm_requests::Store::lease`]), written off the main thread; the requester of a lease it
/// ends no longer owns its tabs ([`crate::control::forget_driven_shell`]).
pub(crate) fn lease_look(at: u64, look: aterm_requests::Look) {
    if let Ok(Some(dir)) = dir() {
        post(Item::Look { dir, at, look });
    }
}

/// A person (`by`) kept the tabs of sessions `sids` (Keep as Tab): each one a request owns
/// records it ([`aterm_requests::Store::keep`]), written off the main thread.
pub(crate) fn kept(sids: Vec<String>, by: &ExitActor) {
    if sids.is_empty() {
        return;
    }
    if let Ok(Some(dir)) = dir() {
        post(Item::Keep {
            dir,
            at: now_unix_ms(),
            sids,
            by: by.as_wire().to_owned(),
        });
    }
}

/// A relaunch sweep's ask ([`sweep`]).
pub(crate) struct Sweep {
    /// When, Unix ms.
    pub(crate) at: u64,
    /// When the asking process began, Unix ms: the requests opened before it are the sweep's.
    pub(crate) born_ms: u64,
    /// The sids of the tabs it hosts.
    pub(crate) hosted: BTreeSet<String>,
    /// Whether it is the only aterm on its state home ([`crate::app_requests::LeaseClock::alone`]),
    /// asked on the writer thread, since it reads the filesystem.
    pub(crate) alone: fn() -> bool,
    /// Set once a sweep ran, and then no later ask of the process sweeps.
    pub(crate) swept: Arc<AtomicBool>,
}

/// The relaunch sweep `sweep` asks for ([`aterm_requests::Store::sweep`]), off the main
/// thread, where the process is alone on its state home and has not swept; a process not alone
/// asks again on its next look.
pub(crate) fn sweep(sweep: Sweep) {
    if let Ok(Some(dir)) = dir() {
        post(Item::Sweep { dir, sweep });
    }
}

/// Tab `left` left this Mac: where a request owns it, its `left` record, and the request's `end`
/// where it was the last, written off the main thread.
pub(crate) fn left(left: TabLeft) {
    let Ok(Some(dir)) = dir() else {
        return;
    };
    post(Item::Left {
        dir,
        at: now_unix_ms(),
        left,
    });
}

/// How long a quit or an update's exec waits for the writer thread ([`flush_within`]): a record
/// takes one ordered flush, and only the tabs closed just before are queued.
pub(crate) const FLUSH_WAIT: Duration = Duration::from_secs(1);

/// Wait at most `budget` for every record handed to the writer thread to be written: a quit's or
/// an update's exec. `false` where the budget ran out first.
pub(crate) fn flush_within(budget: Duration) -> bool {
    if WRITER.lock().unwrap_or_else(|p| p.into_inner()).is_none() {
        return true;
    }
    let (tx, rx) = mpsc::channel();
    post(Item::Flush(tx));
    rx.recv_timeout(budget).is_ok()
}

/// A worker tab's task as its request records it at the close: its conclusion, or still working.
pub(crate) fn task_end(
    record: Option<&crate::task_record::TaskRecord>,
) -> Option<aterm_requests::TaskEnd> {
    record.map(|record| match record.conclusion() {
        Some(crate::task_record::Conclusion::Finished) => aterm_requests::TaskEnd::Finished,
        Some(crate::task_record::Conclusion::Failed) => aterm_requests::TaskEnd::Failed,
        None => aterm_requests::TaskEnd::Working,
    })
}

#[cfg(test)]
#[path = "requests_host_tests.rs"]
mod tests;
