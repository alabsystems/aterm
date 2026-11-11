// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live presence view over the fleet stream.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io;
use std::sync::{Arc, Mutex, MutexGuard};
#[cfg(test)]
use std::time::{Duration, Instant};

use crate::body::Body;
use crate::subject::is_principal;
use crate::transport::{Closer, Conn};

/// The presence face of every session in `fleet`: `/f/<F>/pub/<node>/<sid>/presence`.
#[must_use]
pub fn presence_filter(fleet: &str) -> String {
    format!("/f/{fleet}/pub/*/*/presence")
}

/// Where the view is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Phase {
    /// Not open: never asked for, or closed with the attach it belonged to.
    #[default]
    Closed,
    /// Open, and not yet caught up: answers nothing.
    Seeding,
    /// Caught up and following the tail: answers, with the offset it is exact up to.
    Live,
    /// Its reader ended (a refusal, a transport error, a silent link).
    Dead,
}

/// One `(node, sid)` presence subject's newest live row: only its offset is kept.
type Rows = HashMap<String, BTreeMap<String, u64>>;

#[derive(Default)]
struct ViewState {
    /// Which open this content belongs to.
    generation: u64,
    phase: Phase,
    /// The offset the view is EXACT up to.
    reached: u64,
    /// The highest head the bus has told the view: each mark's, and each confirmation's.
    head: u64,
    /// `sid -> node -> offset` of each live advertisement.
    rows: Rows,
    /// The reader's socket, once it has dialled: what a close or a reopen ends it by.
    closer: Option<Arc<Closer>>,
}

/// What the bridge reads off a live view for one sid ([`PresenceView::snapshot`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// The nodes advertising the sid live.
    pub nodes: BTreeSet<String>,
    /// The offset that answer is exact up to: where a confirmation reads from.
    pub exact_to: u64,
    /// The highest head the bus has told the view.
    pub head: u64,
    /// The open it was read from, for [`PresenceView::advance`].
    pub generation: u64,
}

/// The view: shared between the bridge's loop, which asks it, and the reader thread that feeds it.
#[derive(Default)]
pub struct PresenceView {
    state: Mutex<ViewState>,
    /// Woken on every change, so a test waits on the view rather than on time.
    #[cfg(test)]
    changed: std::sync::Condvar,
}

impl PresenceView {
    fn lock(&self) -> MutexGuard<'_, ViewState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[cfg(test)]
    fn notify(&self) {
        self.changed.notify_all();
    }

    #[cfg(not(test))]
    const fn notify(&self) {}

    /// Clears held state, starts `phase` under a new generation, and closes the old socket.
    fn reset(&self, phase: Phase) -> u64 {
        let mut s = self.lock();
        let generation = s.generation.wrapping_add(1);
        let closer = s.closer.take();
        *s = ViewState {
            generation,
            phase,
            ..ViewState::default()
        };
        drop(s);
        if let Some(closer) = closer {
            closer.close();
        }
        self.notify();
        generation
    }

    /// Where the view is.
    #[must_use]
    pub fn phase(&self) -> Phase {
        self.lock().phase
    }

    /// Start a new open: everything held is forgotten.
    pub fn open(&self) -> u64 {
        self.reset(Phase::Seeding)
    }

    /// Close it: everything held is forgotten, the reader's socket is hung up.
    pub fn close(&self) {
        self.reset(Phase::Closed);
    }

    /// The reader of `generation` has dialled.
    pub fn hold(&self, generation: u64, closer: Arc<Closer>) -> bool {
        let mut s = self.lock();
        if s.generation != generation || !matches!(s.phase, Phase::Seeding | Phase::Live) {
            return false;
        }
        s.closer = Some(closer);
        true
    }

    /// Hang up the reader's socket and leave everything else as it is.
    #[cfg(test)]
    pub(crate) fn cut(&self) {
        let closer = self.lock().closer.clone();
        if let Some(closer) = closer {
            closer.close();
        }
    }

    /// The reader of `generation` ended.
    pub fn end(&self, generation: u64) {
        let mut s = self.lock();
        if s.generation == generation && s.phase != Phase::Closed {
            s.phase = Phase::Dead;
            s.rows.clear();
            s.closer = None;
        }
        drop(s);
        self.notify();
    }

    /// Apply one presence record of `fleet` read by the reader of `generation`.
    pub fn apply(&self, generation: u64, fleet: &str, off: u64, subject: &str, raw: &[u8]) -> bool {
        let mut s = self.lock();
        if s.generation != generation || !matches!(s.phase, Phase::Seeding | Phase::Live) {
            return false;
        }
        s.reached = s.reached.max(off.saturating_add(1));
        if let Some((node, sid)) = session_presence(fleet, subject) {
            let routable = routable(raw);
            let have = s
                .rows
                .get(sid)
                .and_then(|by_node| by_node.get(node))
                .copied();
            if have.is_none_or(|at| at < off) {
                if routable {
                    s.rows
                        .entry(sid.to_string())
                        .or_default()
                        .insert(node.to_string(), off);
                } else if let Some(by_node) = s.rows.get_mut(sid) {
                    by_node.remove(node);
                    if by_node.is_empty() {
                        s.rows.remove(sid);
                    }
                }
            }
        }
        drop(s);
        self.notify();
        true
    }

    /// The reader of `generation` read the tail's `Mark` resuming at `next`.
    pub fn mark(&self, generation: u64, next: u64, head: u64) -> bool {
        let mut s = self.lock();
        if s.generation != generation || !matches!(s.phase, Phase::Seeding | Phase::Live) {
            return false;
        }
        if s.phase == Phase::Seeding && next >= s.reached {
            s.phase = Phase::Live;
        }
        s.reached = s.reached.max(next);
        s.head = s.head.max(head);
        drop(s);
        self.notify();
        true
    }

    /// A CONFIRMATION'S MARK.
    pub fn advance(&self, generation: u64, exact_to: u64, head: u64) {
        let mut s = self.lock();
        if s.generation != generation || s.phase != Phase::Live {
            return;
        }
        s.reached = s.reached.max(exact_to);
        s.head = s.head.max(head);
        drop(s);
        self.notify();
    }

    /// The nodes advertising `sid` live, the offset that answer is EXACT up to.
    #[must_use]
    pub fn snapshot(&self, sid: &str) -> Option<Snapshot> {
        let s = self.lock();
        if s.phase != Phase::Live {
            return None;
        }
        let nodes = s
            .rows
            .get(sid)
            .map(|by_node| by_node.keys().cloned().collect())
            .unwrap_or_default();
        Some(Snapshot {
            nodes,
            exact_to: s.reached,
            head: s.head,
            generation: s.generation,
        })
    }

    /// Block until `ready` holds for the view, or `within` passes.
    #[cfg(test)]
    pub(crate) fn wait_for(&self, within: Duration, ready: impl Fn(Phase, u64) -> bool) -> bool {
        let deadline = Instant::now() + within;
        let mut s = self.lock();
        loop {
            if ready(s.phase, s.reached) {
                return true;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            s = self
                .changed
                .wait_timeout(s, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }
}

/// What one confirmation read proved, for the post it was asked for ([`confirmation`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Confirmation {
    /// The view's answer for the sid is the bus's at the read's head.
    pub routes: bool,
    /// The offset the view is now known to be exact up to.
    pub exact_to: u64,
}

/// Judge a confirmation.
#[must_use]
pub fn confirmation(
    fleet: &str,
    sid: &str,
    from: u64,
    found: &[astream_broker::Record],
    (next, head): (u64, u64),
) -> Confirmation {
    let exact_to = found.first().map_or(next, |(off, _, _)| *off).max(from);
    let routes = next >= head
        && !found
            .iter()
            .any(|(_, subject, _)| names_sid(fleet, subject, sid));
    Confirmation { routes, exact_to }
}

/// Whether `subject` is a presence face of `sid` in `fleet` under any node.
fn names_sid(fleet: &str, subject: &str, sid: &str) -> bool {
    let segs: Vec<&str> = subject.split('/').collect();
    matches!(
        segs.as_slice(),
        ["", "f", f, "pub", _, s, "presence"] if *f == fleet && *s == sid
    )
}

/// `(node, sid)` of a SESSION's presence face in `fleet`.
fn session_presence<'a>(fleet: &str, subject: &'a str) -> Option<(&'a str, &'a str)> {
    let segs: Vec<&str> = subject.split('/').collect();
    match segs.as_slice() {
        ["", "f", f, "pub", node, sid, "presence"]
            if *f == fleet && is_principal(node) && is_principal(sid) && sid.starts_with("s-") =>
        {
            Some((node, sid))
        }
        _ => None,
    }
}

/// Whether a presence row makes its node a routing candidate for its sid.
fn routable(raw: &[u8]) -> bool {
    let (body, _) = Body::decode(raw);
    body.unknown.get("observer").is_none_or(|v| v != "1")
        && body.unknown.get("state").is_some_and(|s| s == "live")
}

/// Why a reader ended: its dial, or the follow after it.
enum Ended {
    Dial(io::Error),
    Follow(io::Error),
}

/// Feed `view` (open `generation`) on a thread of its own: DIAL with `dial`.
/// # Errors
/// The thread could not be spawned; `dial` is dropped with the closure, never called.
pub fn spawn_reader<D>(
    dial: D,
    fleet: &str,
    view: Arc<PresenceView>,
    generation: u64,
) -> io::Result<()>
where
    D: FnOnce() -> io::Result<(Conn, Closer)> + Send + 'static,
{
    let fleet = fleet.to_string();
    std::thread::Builder::new()
        .name("aterm-link-presence".to_string())
        .spawn(move || {
            let ended = match dial() {
                Err(e) => Err(Ended::Dial(e)),
                Ok((conn, closer)) => {
                    let closer = Arc::new(closer);
                    let r = if view.hold(generation, Arc::clone(&closer)) {
                        follow(conn, &closer, &fleet, &view, generation).map_err(Ended::Follow)
                    } else {
                        Ok(())
                    };
                    closer.close();
                    r
                }
            };
            let current = view.lock().generation == generation;
            if let (Err(why), true) = (ended, current) {
                match why {
                    Ended::Dial(e) => eprintln!(
                        "aterm-link: the presence view could not be opened ({e}); a bare \
                         @s-<sid> post reads the roster from the bus"
                    ),
                    Ended::Follow(e) => eprintln!(
                        "aterm-link: the presence view ended ({e}); a bare @s-<sid> post reads \
                         the roster from the bus until it is open again"
                    ),
                }
            }
            view.end(generation);
        })
        .map(drop)
}

/// [`spawn_reader`]'s body, once dialled.
fn follow(
    mut conn: Conn,
    closer: &Closer,
    fleet: &str,
    view: &PresenceView,
    generation: u64,
) -> io::Result<()> {
    let filter = presence_filter(fleet);
    let mut stale = false;
    let ((next, _), rest) = conn.last_walk(&filter, "", u32::MAX, |(off, subject, raw)| {
        if view.apply(generation, fleet, off, &subject, &raw) {
            Ok(astream_broker::Walk::Continue)
        } else {
            stale = true;
            Ok(astream_broker::Walk::Stop)
        }
    })?;
    if stale {
        return Ok(());
    }
    if rest.is_some() {
        return Err(io::Error::other(format!(
            "the last-value face of {filter} did not end within {} rows",
            u32::MAX
        )));
    }
    let mut sub = conn.subscribe(next, &filter)?;
    closer.set_read_timeout(None)?;
    loop {
        match sub.recv_live_event(closer, astream_broker::IDLE_MARK_DEADLINE)? {
            None => return Ok(()),
            Some(astream_broker::Event::Mark { next, head, .. }) => {
                if !view.mark(generation, next, head) {
                    return Ok(());
                }
            }
            Some(astream_broker::Event::Delivery {
                offset,
                subject,
                body,
            }) => {
                if !view.apply(generation, fleet, offset, &subject, &body) {
                    return Ok(());
                }
            }
            Some(astream_broker::Event::Gap(gap)) => {
                return Err(io::Error::other(format!(
                    "the broker's retention gave up {} below its tail, so it is exact no more",
                    crate::transport::gap_lost(&gap)
                )));
            }
        }
    }
}

#[cfg(test)]
#[path = "presence_view_tests.rs"]
mod tests;
