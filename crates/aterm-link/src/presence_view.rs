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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(node: &str, sid: &str) -> String {
        format!("/f/f/pub/{node}/{sid}/presence")
    }

    const LIVE: &[u8] = b"v=1 t=1 state=live";
    const EXITED: &[u8] = b"v=1 t=1 state=exited";

    fn nodes(view: &PresenceView, sid: &str) -> Option<Vec<String>> {
        view.snapshot(sid).map(|s| s.nodes.into_iter().collect())
    }

    fn answer(view: &PresenceView, sid: &str) -> Option<(Vec<String>, u64)> {
        view.snapshot(sid)
            .map(|s| (s.nodes.into_iter().collect(), s.exact_to))
    }

    #[test]
    fn the_view_answers_only_while_live() {
        let view = PresenceView::default();
        assert_eq!(view.snapshot("s-a"), None, "closed");
        let g = view.open();
        assert!(view.apply(g, "f", 7, &row("n-a", "s-a"), LIVE));
        assert_eq!(view.snapshot("s-a"), None, "seeding");
        assert!(view.mark(g, 7, 7), "a mark AT the highest record applied");
        assert_eq!(view.phase(), Phase::Seeding, "is not past it");
        assert!(view.mark(g, 8, 8));
        assert_eq!(view.phase(), Phase::Live);
        let n_a = vec!["n-a".to_string()];
        assert_eq!(answer(&view, "s-a"), Some((n_a.clone(), 8)));
        assert_eq!(answer(&view, "s-b"), Some((vec![], 8)), "a live miss");
        assert!(view.mark(g, 40, 40));
        assert_eq!(answer(&view, "s-a"), Some((n_a.clone(), 40)));
        assert!(view.apply(g, "f", 41, "/f/f/pub/n-z/node/presence", LIVE));
        assert_eq!(answer(&view, "s-a"), Some((n_a, 42)));
        assert!(view.mark(g, 30, 30), "a mark behind it");
        assert_eq!(
            view.snapshot("s-a").map(|s| s.exact_to),
            Some(42),
            "never back"
        );
        view.end(g);
        assert_eq!(view.phase(), Phase::Dead);
        assert_eq!(view.snapshot("s-a"), None, "dead");
    }

    #[test]
    fn a_row_never_loses_to_an_older_one() {
        let view = PresenceView::default();
        let g = view.open();
        for (off, node, body) in [(50, "n-a", EXITED), (30, "n-b", LIVE)] {
            assert!(view.apply(g, "f", off, &row(node, "s-x"), body));
        }
        for (off, node, body) in [(45, "n-a", LIVE), (50, "n-a", EXITED), (25, "n-b", EXITED)] {
            assert!(view.apply(g, "f", off, &row(node, "s-x"), body));
        }
        assert!(view.mark(g, 51, 51));
        assert_eq!(nodes(&view, "s-x"), Some(vec!["n-b".to_string()]));
        assert!(view.apply(g, "f", 60, &row("n-b", "s-x"), EXITED));
        assert!(view.apply(g, "f", 61, &row("n-c", "s-x"), LIVE));
        assert_eq!(nodes(&view, "s-x"), Some(vec!["n-c".to_string()]));
        assert!(view.apply(g, "f", 62, &row("n-d", "s-x"), LIVE));
        assert_eq!(
            nodes(&view, "s-x"),
            Some(vec!["n-c".to_string(), "n-d".to_string()])
        );
    }

    #[test]
    fn only_a_hosting_nodes_live_row_advertises() {
        let view = PresenceView::default();
        let g = view.open();
        let records: [(&str, &[u8]); 7] = [
            (
                "/f/f/pub/n-a/s-x/presence",
                b"v=1 t=1 state=live observer=1",
            ),
            ("/f/f/pub/n-b/s-x/presence", b"v=1 t=1"),
            ("/f/f/pub/n-c/node/presence", LIVE),
            ("/f/f/pub/p/h-human/presence", LIVE),
            ("/f/g/pub/n-d/s-x/presence", LIVE),
            ("/f/f/pub/n-e/s-X!/presence", LIVE),
            ("/f/f/pub/n-f/s-x/presence", LIVE),
        ];
        for (off, (subject, body)) in (1u64..).zip(records) {
            assert!(view.apply(g, "f", off, subject, body));
        }
        assert!(view.mark(g, 100, 100));
        assert_eq!(nodes(&view, "s-x"), Some(vec!["n-f".to_string()]));
    }

    #[test]
    fn a_stale_reader_cannot_touch_the_view() {
        let view = PresenceView::default();
        let old = view.open();
        let new = view.open();
        assert!(!view.apply(old, "f", 1, &row("n-a", "s-x"), LIVE));
        assert!(!view.mark(old, 2, 2));
        view.end(old);
        assert_eq!(
            view.phase(),
            Phase::Seeding,
            "the old reader's end is not this open's"
        );
        assert!(view.mark(new, 1, 1));
        assert_eq!(nodes(&view, "s-x"), Some(vec![]));
        view.close();
        assert!(!view.apply(new, "f", 3, &row("n-a", "s-x"), LIVE));
        assert_eq!(view.phase(), Phase::Closed);
    }

    /// **A CLOSE OR A REOPEN HANGS UP THE SOCKET THE VIEW HOLDS, AND A READER
    /// THAT DIALLED FOR AN OPEN THAT IS GONE IS NOT HELD.** The reader dials on
    /// its own thread, so its open can be closed or replaced before it hands
    /// its socket over: that hand-over is refused, and the reader hangs up
    /// itself.
    #[cfg(unix)]
    #[test]
    fn a_close_hangs_up_the_socket_the_view_holds() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("al-pv")
            .tempdir_in("/tmp")
            .expect("a scratch dir");
        let socket = dir.path().join("b.sock");
        let broker = crate::test_broker(dir.path().join("b.log")).expect("a broker");
        let _server = broker.serve(&socket).expect("serve");
        let dial = || {
            crate::transport::connect(&socket.to_string_lossy(), Duration::from_secs(60))
                .expect("dial")
        };
        let alive = |conn: &mut Conn| conn.fetch(0, "/f/>", 0).is_ok();
        let view = PresenceView::default();
        let gone = view.open();
        let g = view.open();
        let (mut late, late_closer) = dial();
        assert!(
            !view.hold(gone, Arc::new(late_closer)),
            "dialled for an open that is gone"
        );
        assert!(
            alive(&mut late),
            "the reader hangs that one up, not the view"
        );
        let (mut held, closer) = dial();
        assert!(view.hold(g, Arc::new(closer)));
        assert!(alive(&mut held));
        view.close();
        assert!(!alive(&mut held), "a close hangs up the held socket");
        assert!(alive(&mut late), "and nothing else");

        let g = view.open();
        let (mut held, closer) = dial();
        assert!(view.hold(g, Arc::new(closer)));
        view.open();
        assert!(!alive(&mut held), "a reopen hangs up the previous reader");
    }

    #[test]
    fn a_confirmation_routes_only_through_to_the_head_and_is_a_mark_either_way() {
        let rec = |off: u64, subject: &str| (off, subject.to_string(), LIVE.to_vec());
        let judge = |found: &[astream_broker::Record], next: u64| {
            confirmation("f", "s-x", 100, found, (next, 200))
        };
        let routes = |exact_to| Confirmation {
            routes: true,
            exact_to,
        };
        let walks = |exact_to| Confirmation {
            routes: false,
            exact_to,
        };
        assert_eq!(judge(&[], 200), routes(200));
        assert_eq!(
            judge(
                &[
                    rec(150, &row("n-a", "s-y")),
                    rec(160, "/f/f/pub/n-a/node/presence")
                ],
                200
            ),
            routes(150)
        );
        for subject in [row("n-b", "s-x"), "/f/f/pub/N!/s-x/presence".to_string()] {
            assert_eq!(judge(&[rec(170, &subject)], 200), walks(170), "{subject}");
        }
        assert_eq!(
            judge(
                &[rec(120, &row("n-a", "s-y")), rec(170, &row("n-b", "s-x"))],
                200
            ),
            walks(120)
        );
        assert_eq!(judge(&[], 180), walks(180), "cut short by the scan bound");
        assert_eq!(
            judge(&[rec(110, &row("n-a", "s-y"))], 111),
            walks(110),
            "cut short by max"
        );
        let other_fleet =
            confirmation("g", "s-x", 100, &[rec(150, &row("n-a", "s-x"))], (200, 200));
        assert_eq!(other_fleet, routes(150));

        let view = PresenceView::default();
        let g = view.open();
        view.advance(g, 500, 600);
        assert!(view.mark(g, 100, 120));
        assert_eq!(
            view.snapshot("s-x").map(|s| (s.exact_to, s.head)),
            Some((100, 120)),
            "a seeding view's first mark is the broker's, not a confirmation's"
        );
        view.advance(g, 200, 250);
        assert_eq!(
            view.snapshot("s-x").map(|s| (s.exact_to, s.head)),
            Some((200, 250))
        );
        view.advance(g, 150, 240);
        assert_eq!(
            view.snapshot("s-x").map(|s| (s.exact_to, s.head)),
            Some((200, 250)),
            "never back"
        );
        view.advance(g.wrapping_add(1), 900, 900);
        assert_eq!(view.snapshot("s-x").map(|s| s.exact_to), Some(200));
        let stale = view.snapshot("s-x").expect("live").generation;
        let g2 = view.open();
        assert!(view.mark(g2, 10, 10));
        view.advance(stale, 900, 900);
        assert_eq!(
            view.snapshot("s-x").map(|s| s.exact_to),
            Some(10),
            "a confirmation read from another open moves nothing"
        );
    }
}
