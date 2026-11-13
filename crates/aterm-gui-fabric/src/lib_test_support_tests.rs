// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

use aterm_session::SessionId;

use super::{FabricFact, FabricSession, Registry, SessionFabric, Timeline};

/// What every record rings, as aterm-gui's timeline rings its session's watchers.
#[derive(Default)]
pub(crate) struct Watchers {
    rung: Mutex<u64>,
    changed: Condvar,
}

impl Watchers {
    fn ring(&self) {
        *self.rung.lock().unwrap_or_else(|p| p.into_inner()) += 1;
        self.changed.notify_all();
    }
}

/// One watcher of a session's [`Watchers`].
pub(crate) struct Subscription {
    watchers: Arc<Watchers>,
    seen: std::cell::Cell<u64>,
}

impl Subscription {
    /// A watcher of `watchers`, rung from now on.
    pub(crate) fn single(watchers: &Arc<Watchers>) -> Self {
        let seen = *watchers.rung.lock().unwrap_or_else(|p| p.into_inner());
        Self {
            watchers: Arc::clone(watchers),
            seen: std::cell::Cell::new(seen),
        }
    }

    /// Whether the session rang, waiting at most `bound` for a ring.
    pub(crate) fn wait(&self, bound: Duration) -> bool {
        let until = Instant::now() + bound;
        let mut rung = self.watchers.rung.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if *rung > self.seen.get() {
                self.seen.set(*rung);
                return true;
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            rung = self
                .watchers
                .changed
                .wait_timeout(rung, left)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }
}

/// One timeline record: its kind and what its fact says ([`said`]).
pub(crate) struct Event {
    pub(crate) kind: &'static str,
    pub(crate) fact: String,
}

/// What a fact says: its words and offsets in the `k=v` order of the row aterm-gui's view
/// prints for it, so a test names the floor, the verdict or the hold a move stated.
pub(crate) fn said(fact: FabricFact<'_>) -> String {
    let or_dash = |n: Option<u64>| n.map_or_else(|| "-".to_string(), |n| n.to_string());
    match fact {
        FabricFact::FleetMailIn { id, kind, from } => {
            format!("f{id} from={from} kind={kind}")
        }
        FabricFact::FleetMailSeen { id } => format!("f{id} moved=1"),
        FabricFact::FleetMailOut { id, kind, to, idem } => {
            format!("f{id} to={} kind={kind} key={idem}", super::pct_encode(to))
        }
        FabricFact::MailIn {
            kind,
            from,
            off,
            inbox,
        } => format!("{inbox} from={from} kind={kind} off={off}"),
        FabricFact::MailSeen {
            inbox,
            off,
            floor,
            seen,
            verdict,
            kind,
            from,
            moved: _,
        } => {
            let tail = verdict.map_or_else(String::new, |v| {
                format!(" verdict={v} kind={kind} from={from}")
            });
            format!(
                "{inbox} off={} floor={} seen={seen}{tail}",
                or_dash(off),
                or_dash(floor)
            )
        }
        FabricFact::MailOut {
            kind,
            to,
            post,
            re,
            dl,
            via,
            idem,
        } => {
            let mut out = format!("{post} to={} kind={kind}", super::pct_encode(to));
            for (k, v) in [
                ("re", re.map(|n| n.to_string())),
                ("dl", dl.map(|n| n.to_string())),
                ("via", via.map(str::to_string)),
                ("key", idem.map(str::to_string)),
            ] {
                if let Some(v) = v {
                    out.push_str(&format!(" {k}={v}"));
                }
            }
            out
        }
        FabricFact::MailLanded { post, off, why } => {
            let why = why.map_or_else(String::new, |w| format!(" reason={}", w.word()));
            format!("{post} off={}{why}", or_dash(off))
        }
        FabricFact::Hold {
            held,
            bridge: _,
            origin,
            hold_origin,
            reason,
            carried,
        } => format!(
            "{} reason={} origin={origin} hold_origin={hold_origin}{}",
            u8::from(held),
            reason.unwrap_or("-"),
            if carried { " carried=1" } else { "" }
        ),
        FabricFact::Topic {
            add: true,
            name,
            since,
        } => format!("add {name} since={}", since.unwrap_or("-")),
        FabricFact::Topic { name, .. } => format!("drop {name}"),
        FabricFact::Advisory { origin, reason, .. } => {
            format!("1 reason={reason} origin={origin}")
        }
        FabricFact::Fetch { off } => format!("off={off}"),
        FabricFact::MailGap { from, to, records } => format!(
            "from={from} to={to} records={}",
            records.map_or_else(|| "-".to_string(), |n| n.to_string())
        ),
        FabricFact::TopicGap { from, to, topic } => {
            format!("from={from} to={to} topic={topic}")
        }
    }
}

/// A session's timeline.
#[derive(Default)]
pub(crate) struct TestTimeline {
    events: Vec<Event>,
    pub(crate) facts: Vec<String>,
    /// What [`Timeline::log_carried`] answers.
    pub(crate) log_carried: bool,
}

impl TestTimeline {
    /// Every record (the real timeline's `since(None)`).
    pub(crate) fn since(&self, after: Option<u64>) -> impl DoubleEndedIterator<Item = &Event> {
        assert!(after.is_none(), "the tests read the whole timeline");
        self.events.iter()
    }

    /// The registry's own row, as the real store writes its session's open.
    fn spawned(&mut self) {
        self.events.push(Event {
            kind: "spawned",
            fact: "state=alive".to_string(),
        });
    }
}

impl Timeline for TestTimeline {
    fn record_fact(&mut self, kind: &'static str, fact: FabricFact<'_>) -> bool {
        self.facts.push(format!("{kind}: {fact:?}"));
        self.events.push(Event {
            kind,
            fact: said(fact),
        });
        true
    }

    fn log_carried(&self) -> bool {
        self.log_carried
    }
}

/// A session: its id, its fabric endpoint, its timeline and the watchers every record rings.
pub(crate) struct SessionCtx {
    pub(crate) self_id: SessionId,
    pub(crate) fabric: Arc<SessionFabric>,
    pub(crate) timeline: Arc<Mutex<TestTimeline>>,
    pub(crate) watchers: Arc<Watchers>,
}

impl FabricSession for SessionCtx {
    fn self_id(&self) -> &SessionId {
        &self.self_id
    }
    fn fabric(&self) -> &SessionFabric {
        &self.fabric
    }
    fn record_fact(&self, kind: &'static str, fact: FabricFact<'_>) -> bool {
        let owed = self
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .record_fact(kind, fact);
        self.watchers.ring();
        owed
    }
    fn fabric_changed(&self) {}
    fn holder_token(&self) -> Option<String> {
        None
    }
}

/// What `test_handle` hands a test.
pub(crate) struct TestHandle {
    pub(crate) sid: SessionId,
    pub(crate) ctx: Arc<SessionCtx>,
    local_id: u64,
}

/// A fresh, unregistered session with process-local id `local_id`, on the test timing value
/// (the fence and notice waits at their test lengths).
pub(crate) fn test_handle(local_id: u64) -> TestHandle {
    aterm_gui_timing::select_fast();
    let sid = SessionId::generate();
    TestHandle {
        sid: sid.clone(),
        ctx: Arc::new(SessionCtx {
            self_id: sid,
            fabric: Arc::default(),
            timeline: Arc::default(),
            watchers: Arc::default(),
        }),
        local_id,
    }
}

/// The registry: live sessions by local id.
#[derive(Default)]
pub(crate) struct TestStore {
    by_local: BTreeMap<u64, Arc<SessionCtx>>,
    /// Every `link set-aside` count handed over ([`Registry::torn_state_set_aside`]), in order.
    pub(crate) set_aside: std::sync::Mutex<Vec<u64>>,
    /// Whether each of those came from a bridge that says when its quarantine is empty.
    pub(crate) told_when_empty: std::sync::Mutex<Vec<bool>>,
    /// Every `link quarantine` count handed over ([`Registry::torn_state_held`]), in order.
    pub(crate) held: std::sync::Mutex<Vec<u64>>,
}

impl TestStore {
    /// Register a session; a first registration records `spawned`, as the real store's does.
    pub(crate) fn register(&mut self, h: TestHandle) {
        if !self.by_local.values().any(|c| c.self_id == h.sid) {
            h.ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .spawned();
            h.ctx.watchers.ring();
        }
        self.by_local.insert(h.local_id, h.ctx);
    }

    /// Drop the session with process-local id `local`, as a closed tab's.
    pub(crate) fn deregister_local(&mut self, local: u64) {
        self.by_local.remove(&local);
    }
}

impl Registry for TestStore {
    fn sessions(&self) -> Vec<Arc<dyn FabricSession>> {
        self.by_local
            .values()
            .map(|c| Arc::clone(c) as Arc<dyn FabricSession>)
            .collect()
    }
    fn session(&self, sid: &SessionId) -> Option<Arc<dyn FabricSession>> {
        self.by_local
            .values()
            .find(|c| &c.self_id == sid)
            .map(|c| Arc::clone(c) as Arc<dyn FabricSession>)
    }
    fn roster_seq(&self) -> u64 {
        0
    }
    fn bridge_lost_advised(&self, _sessions: usize) {}
    fn torn_state_set_aside(&self, files: u64, told_when_empty: bool) {
        self.set_aside
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(files);
        self.told_when_empty
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(told_when_empty);
    }
    fn torn_state_held(&self, files: u64) {
        self.held
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(files);
    }
}

/// The tests' registry handle.
pub(crate) type Store = Arc<RwLock<TestStore>>;

/// A fresh, empty registry.
pub(crate) fn new_store() -> Store {
    Arc::new(RwLock::new(TestStore::default()))
}
