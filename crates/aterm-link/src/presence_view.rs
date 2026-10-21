// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LIVE PRESENCE VIEW: which nodes advertise which session, kept current by
//! ONE subscription to `/f/<F>/pub/*/*/presence`, so a post to a bare
//! `@s-<sid>` this node does not host is routed with one small read instead of
//! a walk of the presence index.
//!
//! ## Why it exists
//!
//! §6.1 routes a bare `@s-<sid>` only when exactly one node advertises that sid
//! and the TOFU pin agrees. The bridge answered "who advertises it" with a
//! whole last-value walk of `/f/<F>/pub/*/<sid>/presence` plus a `Fetch` of the
//! stretch since the walk's mark ([`crate::bridge`]'s `last_face`) on EVERY post
//! to such an address, the second and the hundredth alike: two serial round
//! trips at least, and a `Last` whose only literal prefix is `/f/<F>/pub/`, so
//! the broker visits every index entry under it — every face of every node —
//! with its log lock held. All for an answer that changes only when somebody's
//! presence does. The view holds the answer instead; a node that never posts to
//! a bare remote sid never opens it and pays nothing.
//!
//! ## Exact up to an offset, and confirmed past it
//!
//! The view knows, besides its rows, the offset it is EXACT up to
//! (`ViewState::reached`): every presence record below it has been applied, in
//! order, because the tail delivers gaplessly. What it cannot know without
//! asking is what landed since — and that matters, because §6.1's guarantee is
//! a RESOLVE-TIME read: a second claimant whose row is on the log before a post
//! is asked makes that post `ERR ambiguous` (the two-node sealed test stages a
//! rogue doing exactly that and posts the moment its claim is acked). A view a
//! delivery behind would route that post to the pinned node instead.
//!
//! So the bridge asks the bus one bounded question per post
//! ([`PresenceView::snapshot`], then `Bridge::route_from_view`): has any presence
//! record landed at or after the offset the view is exact up to? One
//! `Fetch{from: reached, /f/<F>/pub/*/*/presence, max: CONFIRM_ROWS}` over the
//! WHOLE face, judged by [`confirmation`]: read through to the head with no record
//! of THIS sid among what it found, the view's rows for the sid ARE the bus's,
//! read after the post was asked, as linearizable as the walk. A record of the
//! sid, or a read cut short (the broker's scan bound, or `CONFIRM_ROWS` other
//! sessions' rows before the head): the walk, as before.
//!
//! ## What a confirmation costs, and why no stretch is scanned twice
//!
//! A `Fetch` scans the LOG, not an index: every record since `reached`, whatever
//! its subject, with the broker's log lock held. And a quiet presence face moves
//! `reached` only at the broker's idle marks, one per `IDLE_MARK` (10 s), so on a
//! log busy with other traffic the stretch since `reached` can be ten seconds of
//! the whole fleet's records. Two rules keep that from costing every post:
//!
//! * A CONFIRMATION IS ALSO A MARK. The question covers every session's presence,
//!   so an answer proves the view exact up to the first presence record it found,
//!   or to where it stopped when it found none, and the view moves `reached` there
//!   ([`PresenceView::advance`]): nothing below it was a presence record, so there
//!   was nothing to apply. The stretch one post scanned is where the next post's
//!   read STARTS, so posts in a row each scan what landed since the one before,
//!   never the same records again — and a read the scan bound cut short still
//!   moves the view on by what it scanned.
//! * A STRETCH KNOWN TO BE LONG IS NOT ASKED ABOUT. The view keeps the highest
//!   head the bus has told it (each mark's and each confirmation's), and the bridge
//!   knows the head one past its own last publish; when either is more than
//!   `CONFIRM_SCAN_MAX` records past `reached`, the post walks without sending the
//!   confirmation, because across a long stretch the walk's `Last` — which visits
//!   the presence index, not the log — is the cheaper read, and a confirmation
//!   that could not reach the head is a round trip for nothing. Both heads are
//!   ones already heard, never asked for, so they can be low: a confirmation can
//!   still scan the records that landed since either was heard, and that scan,
//!   too, becomes the next post's starting point.
//!
//! ## An accelerator, never an authority
//!
//! Even confirmed, the view answers only a ROUTE: exactly one live advertiser,
//! and a pin that is absent or names that node. Every answer that would RETIRE a
//! post — nobody advertises the sid (`unroutable`), two claimants or a pin
//! conflict (`ambiguous`) — and every question asked while the view is not
//! [`Phase::Live`] goes to the walk exactly as before, so no post is retired on
//! the view's word, and a view that misbehaves costs walks, never a wrong
//! verdict.
//!
//! ## Live, and how the view knows it
//!
//! [`Phase::Seeding`] while the reader walks the face's last values and replays
//! the tail from the walk's splice point; [`Phase::Live`] from the tail's first
//! `Mark` — the broker's word that the subscription has caught up, written the
//! moment it does. From then on the broker marks an idle tail after every
//! `IDLE_MARK` (each mark moves `reached` to the tail's cursor), and each mark
//! re-arms a read deadline of `IDLE_MARK_DEADLINE` on the socket (what astream's
//! `recv_live` does), so a link that dies without a FIN fails the read and the
//! view goes [`Phase::Dead`]. A view whose link died silently and is not yet
//! dead is still exact up to its `reached`: a presence record its reader never
//! delivered is one a confirmation finds, and a post to its sid walks.
//!
//! ## Dialled off the bridge's loop
//!
//! The view's connection is dialled by its own reader thread ([`spawn_reader`]),
//! never by the bridge's loop: a dial and its ring attach can each take the whole
//! ack deadline on a broker that accepts slowly or not at all, and the loop is
//! where a queued fleet halt is applied (§11.2: "a halt never queues"). The post
//! that asked for the view walks, and is published, while the reader dials; the
//! reader hands its socket's closer to the view ([`PresenceView::hold`]), so a
//! close or a reopen ends it from the loop, and one that finishes dialling after
//! its open was closed hangs up at once.
//!
//! ## Ordering: newest wins on the broker's offset
//!
//! One subject per `(node, sid)`, and one row kept per subject: a record
//! replaces what the view holds only when its offset is higher. A record that
//! withdraws the session (`state=` other than `live`, or an observer's row)
//! removes the row instead of storing it, so the view's memory is the sessions
//! LIVE across the fleet, not every presence subject the index ever held.
//!
//! The seed is where order needs care. `Client::last_walk`'s pages after the
//! first are read at later heads, so a seed row can sit at or above the splice
//! point, and the tail from that point repeats it — after the older records it
//! superseded. Dropping a withdrawal (rather than keeping a tombstone) is still
//! exact because the view answers nothing until it is live: the tail redelivers
//! EVERY record at or above the splice point in offset order, so by the first
//! `Mark` the last record applied for each subject is its newest, and a stale
//! row an older tail record put back has been removed again by the newer one.
//! The first mark must also lie past every record applied (`reached`): the tail
//! started after the walk ended, so its first caught-up head is past every head
//! a seed page was read at. A mark that is not (a broker that marks early) is
//! waited past rather than trusted.
//!
//! A presence row that MOVES a sid — its old node withdrawing, a new node
//! advertising — is two subjects, and the offsets settle each; two nodes both
//! live for one sid is §6.1's ambiguity, which the view never answers.

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
    /// Caught up and following the tail: answers, with the offset it is exact
    /// up to.
    Live,
    /// Its reader ended (a refusal, a transport error, a silent link): answers
    /// nothing until it is opened again.
    Dead,
}

/// One `(node, sid)` presence subject's newest LIVE row: only its offset is kept.
type Rows = HashMap<String, BTreeMap<String, u64>>;

#[derive(Default)]
struct ViewState {
    /// Which open this content belongs to. A reader from another open writes
    /// nothing and is told to stop.
    generation: u64,
    phase: Phase,
    /// The offset the view is EXACT up to: just past every record applied —
    /// seed rows and tail alike — or the tail's cursor at its last mark, or
    /// where a confirmation proved there was nothing to apply, whichever is
    /// highest. The first mark must reach it before the view is live (see the
    /// module header); once live, every presence record below it has been
    /// applied, which is what a confirmation reads on from.
    reached: u64,
    /// The highest head the bus has told the view: each mark's, and each
    /// confirmation's. A head HEARD, never asked for — what tells a long
    /// stretch since `reached` from a short one without a round trip.
    head: u64,
    /// `sid -> node -> offset` of each live advertisement.
    rows: Rows,
    /// The reader's socket, once it has dialled: what a close or a reopen ends
    /// it by.
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

/// The view: shared between the bridge's loop, which asks it, and the reader
/// thread that feeds it. One lock, held for a map update or a lookup.
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
    fn notify(&self) {}

    /// Forget everything held and start `phase` under a new generation,
    /// hanging up the old reader's socket if it had one. The socket is closed
    /// OUTSIDE the lock: closing is a `shutdown`, and the reader it wakes takes
    /// this lock to say it ended.
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

    /// Start a new open: everything held is forgotten — the previous reader's
    /// socket hung up — the phase is [`Phase::Seeding`], and the returned
    /// generation is the one its reader must carry.
    pub fn open(&self) -> u64 {
        self.reset(Phase::Seeding)
    }

    /// Close it: everything held is forgotten, the reader's socket is hung up,
    /// and whatever reader is still running belongs to a generation that no
    /// longer exists.
    pub fn close(&self) {
        self.reset(Phase::Closed);
    }

    /// The reader of `generation` has dialled: keep its socket's closer, so a
    /// close or a reopen hangs it up. `false` when `generation` is no longer
    /// the current open — the reader hangs up itself and stops.
    pub fn hold(&self, generation: u64, closer: Arc<Closer>) -> bool {
        let mut s = self.lock();
        if s.generation != generation || !matches!(s.phase, Phase::Seeding | Phase::Live) {
            return false;
        }
        s.closer = Some(closer);
        true
    }

    /// Hang up the reader's socket and leave everything else as it is: the
    /// link dying under the view, which its reader must notice on its own.
    #[cfg(test)]
    pub(crate) fn cut(&self) {
        let closer = self.lock().closer.clone();
        if let Some(closer) = closer {
            closer.close();
        }
    }

    /// The reader of `generation` ended: the view is [`Phase::Dead`] if that is
    /// still the open it belongs to.
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

    /// Apply one presence record of `fleet` read by the reader of `generation`:
    /// newest wins per subject on offset, and a withdrawal removes the row.
    /// `false` when `generation` is not the current open — the reader stops.
    ///
    /// A subject that is not a session's presence face (a node's own row, a
    /// person's) or whose node or sid is not a principal moves `reached` and is
    /// otherwise ignored, exactly as the walk's reader ignores it.
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
            // NEWEST WINS. An equal offset is the same record again (the seed
            // row the tail repeats): nothing changes.
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

    /// The reader of `generation` read the tail's `Mark` resuming at `next`,
    /// with the broker's head at `head`: every record below `next` has been
    /// delivered, so the view is exact up to it, and LIVE once a mark reaches
    /// past every record it applied. `false` when `generation` is not the
    /// current open.
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

    /// A CONFIRMATION'S MARK: a read of the whole presence face, from at or
    /// below where the view of `generation` is exact, found no presence record
    /// below `exact_to` ([`confirmation`]), on a bus whose head was `head`.
    /// Nothing below `exact_to` was there to apply, so the view is exact up to
    /// it — the next confirmation reads on from there, not from the last idle
    /// mark. Only a LIVE view of that same open moves: a seeding view's first
    /// mark must still be the broker's (the module header), and a closed or
    /// reopened one's offsets describe another subscription.
    ///
    /// Racing the reader is harmless: a record it applies meanwhile sits at or
    /// past `exact_to`, since nothing below it was a presence record, and both
    /// only ever raise `reached`.
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

    /// The nodes advertising `sid` live, the offset that answer is EXACT up
    /// to, and the highest head the view has heard — `None` unless
    /// [`Phase::Live`]. `None` is "ask the bus", never "nobody"; and an answer
    /// is the bus's only once nothing about `sid` has landed at or after the
    /// offset (the module header's confirmation).
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

    /// Block until `ready` holds for the view, or `within` passes: a test's
    /// hang detector, never a latency budget.
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

/// What one confirmation read proved, for the post it was asked for
/// ([`confirmation`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Confirmation {
    /// The view's answer for the sid is the bus's at the read's head: the read
    /// reached the head and found no presence record of the sid.
    pub routes: bool,
    /// The offset the view is now known to be exact up to: the first presence
    /// record the read found, or where it stopped when it found none
    /// ([`PresenceView::advance`]).
    pub exact_to: u64,
}

/// Judge a confirmation: `found` is what `Fetch{from, presence_filter(fleet),
/// max}` answered, and `(next, head)` its mark.
///
/// THE READ MUST HAVE REACHED THE HEAD (`next >= head`): a scan cut short by
/// the broker's bound, or by `max` rows of other sessions, says nothing about
/// what lies past it. And it must have found NO RECORD OF THE SID — under any
/// node, live or withdrawn, a node segment the view would ignore included,
/// since the walk's filter (`/f/<F>/pub/*/<sid>/presence`) would read it. What
/// it found of OTHER sessions does not bear on this sid: the view's rows for
/// the sid are exact below `from`, and nothing of the sid sits between `from`
/// and the head.
///
/// Whatever it answered, nothing below the first record it found — or below
/// `next`, when it found none — was a presence record, so the view is exact up
/// to there.
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

/// Whether `subject` is a presence face of `sid` in `fleet` under any node:
/// what the walk's `/f/<F>/pub/*/<sid>/presence` matches.
fn names_sid(fleet: &str, subject: &str, sid: &str) -> bool {
    let segs: Vec<&str> = subject.split('/').collect();
    matches!(
        segs.as_slice(),
        ["", "f", f, "pub", _, s, "presence"] if *f == fleet && *s == sid
    )
}

/// `(node, sid)` of a SESSION's presence face in `fleet` —
/// `["", "f", "<F>", "pub", "<node>", "<sid>", "presence"]`, the shape the
/// walk's reader takes — or `None`. Only an `s-` sid can be asked about.
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

/// Whether a presence row makes its node a routing candidate for its sid: the
/// walk's rule, word for word. An OBSERVER's row (a hand-started `--sock`
/// bridge watching a session it does not host) is no claim to host it, and a
/// row whose `state=` is not `live` is the session withdrawn.
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

/// Feed `view` (open `generation`) on a thread of its own: DIAL with `dial` —
/// here, not on the caller's thread (the module header) — then walk the
/// presence face's last values and follow it from the walk's splice point until
/// the connection ends or the view moves on. The view is [`Phase::Dead`] when
/// the thread ends, and the socket is closed, so nothing stays subscribed that
/// nobody reads.
///
/// # Errors
///
/// The thread could not be spawned; `dial` is dropped with the closure, never
/// called.
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
                        // Closed or reopened while this dialled.
                        Ok(())
                    };
                    closer.close();
                    r
                }
            };
            // ONE LINE PER DEATH, and deaths are bounded by how often the
            // bridge opens the view again (`PRESENCE_VIEW_REOPEN`). Nothing for
            // a reader whose open is gone: it was asked to stop.
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

/// [`spawn_reader`]'s body, once dialled. `Ok` for an end the view asked for
/// (another generation took over) or the broker closing; `Err` for a refusal,
/// a transport failure or a silent link.
fn follow(
    mut conn: Conn,
    closer: &Closer,
    fleet: &str,
    view: &PresenceView,
    generation: u64,
) -> io::Result<()> {
    let filter = presence_filter(fleet);
    let mut stale = false;
    // THE SEED: the face's last values, page by page — memory is one page, not
    // the face. Rows are applied as they arrive; the splice point is the FIRST
    // page's `next` (`Client::last_walk`), the only offset a tail can start
    // from with no gap.
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
    // THE ACK DEADLINE COMES OFF once the tail is open: parking with nothing to
    // deliver is what a subscription does. It comes back as the LIVENESS bound
    // on the broker's first mark, re-armed on every one: astream's `recv_live`
    // rule, read event by event (`recv_live_event`) because a mark's `next`
    // is what makes the view exact up to it.
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

    /// `(nodes, exact up to)` of a live view's answer for `sid`.
    fn answer(view: &PresenceView, sid: &str) -> Option<(Vec<String>, u64)> {
        view.snapshot(sid)
            .map(|s| (s.nodes.into_iter().collect(), s.exact_to))
    }

    /// **NOTHING IS ANSWERED UNTIL THE TAIL HAS CAUGHT UP PAST THE SEED, AND
    /// EVERY ANSWER SAYS WHAT IT IS EXACT UP TO.** A seeding view, a closed one
    /// and a dead one answer `None` — ask the bus — never an empty set. A mark
    /// at the highest record applied is waited past; the first mark beyond it
    /// makes the view live. A record moves the exact-up-to offset past itself,
    /// a later mark to the tail's cursor, and neither moves it back.
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
        // AN IDLE MARK MOVES IT ON; a record elsewhere moves it past itself.
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

    /// **NEWEST WINS ON OFFSET, ACROSS THE SEED'S SEAM.** The seed reports
    /// `n-a` withdrawn at 50 (a later page, past the splice point at 40) and
    /// `n-b` live at 30. The tail from 40 then repeats the stretch: `n-a`'s
    /// OLDER live row at 45 puts it back for a moment, and its withdrawal at
    /// 50, repeated, takes it out again; `n-b`'s older withdrawal at 20 cannot
    /// arrive (the tail starts at 40), and an older live row for `n-b` at 25 —
    /// as a resubscribed tail could hand it — loses to the 30 it holds. Live at
    /// the first mark past 50: `s-x` is `n-b`'s alone.
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
        // A SID THAT MOVES: its old node withdraws, a new one advertises.
        assert!(view.apply(g, "f", 60, &row("n-b", "s-x"), EXITED));
        assert!(view.apply(g, "f", 61, &row("n-c", "s-x"), LIVE));
        assert_eq!(nodes(&view, "s-x"), Some(vec!["n-c".to_string()]));
        // TWO LIVE CLAIMANTS ARE BOTH ANSWERED: the ambiguity is the caller's
        // to refuse, never the view's to settle by recency.
        assert!(view.apply(g, "f", 62, &row("n-d", "s-x"), LIVE));
        assert_eq!(
            nodes(&view, "s-x"),
            Some(vec!["n-c".to_string(), "n-d".to_string()])
        );
    }

    /// **THE WALK'S RULE, ROW FOR ROW.** An observer's row, a row with no
    /// `state=`, a node's own presence row, a person's, another fleet's and a
    /// sid that is not a principal are no advertisement.
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

    /// **A READER FROM ANOTHER OPEN WRITES NOTHING, AND IS TOLD TO STOP.** A
    /// closed view's old reader, and a reopened view's previous one, apply
    /// nothing, mark nothing and kill nothing. An empty face is live at its
    /// first mark.
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
    /// itself. A socket held is ended by the loop's close or reopen — a request
    /// on it fails at once — and the view takes nothing else down.
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

    /// **A CONFIRMATION ROUTES ONLY THROUGH TO THE HEAD, AND IS A MARK EITHER
    /// WAY.** Read from 100 with the head at 200:
    ///
    /// * nothing found, through to the head: routes, exact to 200;
    /// * other sessions' rows only, through to the head: routes, and exact only
    ///   up to the FIRST of them — the view has not applied it;
    /// * a row of the sid under any node — withdrawn, or under a node segment
    ///   the view would ignore — does not route;
    /// * a read cut short (the scan bound, or `max` rows before the head) does
    ///   not route, and still says how far it got.
    ///
    /// The view takes a confirmation's mark only while live and of the same
    /// open, and never moves back; the highest head heard rides along.
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
        // Another fleet's row of the same sid is not this fleet's.
        let other_fleet =
            confirmation("g", "s-x", 100, &[rec(150, &row("n-a", "s-x"))], (200, 200));
        assert_eq!(other_fleet, routes(150));

        // THE VIEW TAKES IT only live, only from its own open, never back.
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
