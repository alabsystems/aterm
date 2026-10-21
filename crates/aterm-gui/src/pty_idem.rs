// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The idempotency key at the PTY seam. Input verbs are not idempotent, so a driver
//! that crashed between "wrote the bytes" and "recorded that it wrote them" must
//! choose between a duplicate keystroke and a lost one. Instead it stamps the verb
//! with `id=<epoch>:<producer>:<seq>`; the endpoint keeps a per-session,
//! per-producer high-water mark and answers an already-consumed sequence
//! `OK dup=1` without writing, so the retry's answer says which side of the window
//! the crash fell on.
//!
//! * `<epoch>`: the session's launch nonce (`LaunchNonce`, 32 lowercase hex, the
//!   `sessions` roster's `nonce=`). A mark means something only inside one
//!   incarnation of one shell, so a key from another launch is refused
//!   (`ERR epoch`) rather than suppressing a live keystroke.
//! * `<producer>`: the u64 the marks are keyed by, within the caller's authority
//!   namespace ([`Realm`]), which the caller does not choose.
//! * `<seq>`: the driver's own monotone sequence, so the mark is O(1): at or below
//!   it is consumed, above it is new.
//!
//! | Outcome | Answer | Meaning |
//! |---|---|---|
//! | above the mark, producer idle | the verb's own reply | fresh; it ran |
//! | above the mark, an attempt in flight | `ERR busy idem=<running seq>` | one attempt per producer; nothing consumed |
//! | at/below the mark, applied | `OK dup=1` | already typed; nothing written |
//! | at the mark, still running | `ERR busy idem=<seq>` | another connection holds it; transient |
//! | at the mark, outcome unknown | `ERR in-doubt seq=<seq>` | it may have typed; do not replay |
//! | above the mark, guard missed | `OK skipped` | nothing written; the sequence is given back |
//!
//! A realm whose [`PRODUCER_CAP`] producers are all in flight answers `ERR busy
//! idem=` naming the caller's own sequence. A reply other than `OK` may still have
//! reached the PTY (`cmd_turn` can type its text and then fail to submit), so its
//! mark is kept with the outcome unknown: a retry is told `ERR in-doubt`, and the
//! session's `timeline` gets an `in-doubt` row.
//!
//! Three replies release the mark instead, because each provably wrote nothing:
//! `ERR busy` (the drive lease, or `ERR busy sink`: a sink that accepted zero
//! bytes), `ERR denied` (authority), and `OK skipped` (a guard that matched no row,
//! so the retry is re-evaluated against the live screen, not answered `dup=1`). For
//! every keyed verb the busy and denied decisions precede the first byte (the
//! dispatch fast-fail and op-scope gate, `turn`'s read-authority check and
//! `cmd_turn`'s lease acquire, `run_feed_bin_routed`'s edge check and lease mirror),
//! except `ERR busy sink`, decided at a write that moved nothing; a partial write
//! answers `ERR write failed` (`control_input::guarded_input_reply`). Nothing else
//! is carved out, not even `ERR usage`: `cmd_turn` answers its usage string
//! when a submit press fails after the text was typed. A reply string is not
//! evidence about the PTY.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use aterm_session::LaunchNonce;

use crate::SessionCtx;

/// The namespace a mark lives in: the authority half of the key, which the caller
/// does not type.
///
/// Naming a producer's next sequence suppresses a write, so marks keyed only by the
/// caller's `<producer>` would let any authorized caller (an in-session agent holds
/// Owner) settle another driver's producer at `u64::MAX` and silence it. The epoch
/// is no defence: it is public (`nonce=` on the roster, `$ATERM_LAUNCH_NONCE`).
///
/// Exactly two. `Scope::Bridge` (the socketpair end this instance handed its own
/// child) is the one authority no token opens, so it gets a namespace nothing else
/// can claim in; everything else shares [`Realm::Local`]. Nothing claims in
/// [`Realm::Bridge`] today (the bridge drives no PTY), but `Scope::Bridge` still
/// holds every op, the keyed verbs included, so the fence stays. Not one realm per
/// connection or edge token: an Owner caller can mint edge tokens, so the map would
/// grow without bound. A session holds at most `2 * PRODUCER_CAP` marks.
///
/// Only claiming is fenced: a `Local` caller naming a sequence the bridge consumed
/// is answered from the bridge's mark (every such answer writes nothing; pinned by
/// `control_tests.rs`'s `feed_idempotent_a_producer_is_namespaced_by_the_callers_authority`)
/// but never installs a mark there, and a `Bridge` claim never consults `Local`. So a
/// `Local` caller can probe the bridge's high-water for a guessed producer id and have
/// its own low sequences answered as duplicates; neither writes or silences the bridge.
///
/// Two `Local` drivers share a namespace, so one can burn the other's sequence:
/// within one authority class nothing tells them apart.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Realm {
    /// The fabric bridge's own connection.
    Bridge,
    /// Every other scope: Owner, and every edge token.
    Local,
}

impl Realm {
    /// The namespace a connection's marks live in.
    pub(crate) fn of(scope: crate::control::Scope) -> Self {
        match scope {
            #[cfg(any(unix, test))]
            crate::control::Scope::Bridge => Self::Bridge,
            crate::control::Scope::Owner | crate::control::Scope::Edge(_) => Self::Local,
            // The fleet gate admits no keyed verb; named with the local scopes,
            // never the bridge's namespace.
            #[cfg(any(unix, test))]
            crate::control::Scope::Fleet { .. } => Self::Local,
        }
    }
}

/// How many producers one session remembers marks for, per [`Realm`]; headroom
/// over the handful of drivers a session sees. Past it the least-recently-used
/// settled producer in the same realm is evicted, and its next replay is applied
/// again: the one way this seam can still duplicate. A running producer is never
/// evicted (its guard would settle a mark that no longer exists), so 64 in-flight
/// producers in one realm answer `ERR busy idem=`. Per realm so a local driver
/// churning producer ids cannot evict the bridge's settled mark.
const PRODUCER_CAP: usize = 64;

/// The state of the sequence AT a producer's high-water mark.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tip {
    /// Claimed and still running on some connection.
    Running,
    /// It ran and answered `OK` — a replay is a duplicate.
    Applied,
    /// It ran and did NOT answer `OK`. It may or may not have reached the PTY,
    /// so a replay is refused rather than guessed at.
    Unknown,
}

/// One producer's mark.
#[derive(Clone, Copy, Debug)]
struct Mark {
    high_water: u64,
    tip: Tip,
    /// Monotone use stamp, for the LRU eviction at [`PRODUCER_CAP`].
    used: u64,
}

/// The per-session marks. A leaf lock: taken to claim and to settle, never held
/// across the verb it guards (which blocks on the event loop, the turn lease and
/// the PTY) and never held while recording a timeline row.
#[derive(Default)]
pub(crate) struct PtyIdem {
    marks: Mutex<HashMap<(Realm, u64), Mark>>,
    /// Feeds `Mark::used`. Only the order matters (it picks the LRU victim), so an
    /// atomic outside the marks lock keeps this a single-lock type.
    clock: AtomicU64,
}

/// A parsed, epoch-checked `id=` key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Key {
    producer: u64,
    seq: u64,
}

/// The usage line every malformed key answers. One string, so the grammar is
/// stated in exactly one place.
const USAGE: &str = "ERR usage: id=<epoch>:<producer>:<seq>\n";

/// The answer to a well-formed key minted for another launch of the session:
/// the head drivers key on, and where the live epoch is read.
pub(crate) const EPOCH_REFUSAL: &str =
    "ERR epoch: id= is from another launch; use this session's nonce= from sessions\n";

/// Parse `<epoch>:<producer>:<seq>` and check the epoch against the live
/// session's launch nonce. `Err` is the caller's whole reply: `ERR usage` for an
/// unparseable key, `ERR epoch` for a well-formed key minted for another launch.
pub(crate) fn parse_key(value: &str, live: LaunchNonce) -> Result<Key, String> {
    let mut parts = value.split(':');
    let (Some(epoch), Some(producer), Some(seq), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(USAGE.to_string());
    };
    let Some(epoch) = LaunchNonce::from_hex(epoch) else {
        return Err(USAGE.to_string());
    };
    let (Ok(producer), Ok(seq)) = (producer.parse::<u64>(), seq.parse::<u64>()) else {
        return Err(USAGE.to_string());
    };
    // Sequence 0 is the empty mark ("this producer has consumed nothing"), so a
    // driver may not name it — otherwise its first attempt would be
    // indistinguishable from no attempt at all.
    if seq == 0 {
        return Err(USAGE.to_string());
    }
    if !epoch.ct_eq(&live) {
        return Err(EPOCH_REFUSAL.to_string());
    }
    Ok(Key { producer, seq })
}

/// The claim one attempt holds while it runs. Settled by [`guarded`]; a drop
/// without a settle (an unwind out of the guarded verb) leaves the mark unknown,
/// the fail-visible direction: a mark that a panic frees is not a mark.
struct Claim<'a> {
    idem: &'a PtyIdem,
    realm: Realm,
    key: Key,
    /// The mark this claim displaced, restored on release. `None` when this
    /// producer had no mark at all, in which case a release removes the entry.
    prior: Option<Mark>,
    /// Set once the claim has been settled or released, so `Drop` knows there is
    /// nothing left to decide.
    done: bool,
}

impl Claim<'_> {
    /// This attempt answered `OK`: the bytes went out.
    fn applied(mut self) {
        self.idem.settle(self.realm, self.key, Tip::Applied);
        self.done = true;
    }

    /// This attempt was refused before any byte could move: give the sequence
    /// back so the driver's retry is a first attempt, not a duplicate.
    fn released(mut self) {
        self.idem.release(self.realm, self.key, self.prior);
        self.done = true;
    }

    /// This attempt failed in a way that says nothing about the PTY.
    fn unknown(mut self) {
        self.idem.settle(self.realm, self.key, Tip::Unknown);
        self.done = true;
    }
}

/// What [`PtyIdem::claim`] decided.
enum Claimed<'a> {
    /// Fresh: run the verb, then settle.
    Fresh(Claim<'a>),
    /// Answer this and write nothing.
    Answer(String),
}

impl PtyIdem {
    fn tick(&self) -> u64 {
        self.clock.fetch_add(1, Ordering::Relaxed)
    }

    /// Reserve `key`'s sequence in `realm`, or answer for it. `dup` is the
    /// duplicate reply in the calling verb's framing ([`dup_reply`]).
    fn claim(&self, realm: Realm, key: Key, dup: &str) -> Claimed<'_> {
        let used = self.tick();
        let slot = (realm, key.producer);
        let mut marks = self.marks.lock().unwrap_or_else(|p| p.into_inner());
        let own = marks.get(&slot).copied();
        if let Some(mark) = own
            && let Some(answer) = consumed_answer(mark, key, dup)
        {
            return Claimed::Answer(answer);
        }
        // Answer-only fallback into the bridge's namespace: every answer of
        // `consumed_answer` writes nothing, and installing below happens only in
        // the caller's own realm. One-way on purpose; see [`Realm`].
        if realm != Realm::Bridge
            && let Some(mark) = marks.get(&(Realm::Bridge, key.producer)).copied()
            && let Some(answer) = consumed_answer(mark, key, dup)
        {
            return Claimed::Answer(answer);
        }
        if let Some(mark) = own {
            // One in-flight attempt per producer. A displaced `Running` mark could
            // not settle its own outcome (`settle` writes only while
            // `high_water == key.seq`), and a later `release` of the displacing
            // claim would reinstate a `Running` tip no live claim stands behind:
            // answered `ERR busy` forever and never evicted. The refusal names the
            // running sequence and consumes nothing, so the retry is a first attempt.
            if mark.tip == Tip::Running {
                return Claimed::Answer(format!("ERR busy idem={}\n", mark.high_water));
            }
            marks.insert(
                slot,
                Mark {
                    high_water: key.seq,
                    tip: Tip::Running,
                    used,
                },
            );
            return Claimed::Fresh(Claim {
                idem: self,
                realm,
                key,
                prior: Some(mark),
                done: false,
            });
        }
        // Counted, and the victim chosen, within this realm ([`PRODUCER_CAP`]).
        if marks.keys().filter(|(r, _)| *r == realm).count() >= PRODUCER_CAP {
            // Evict the least-recently-used settled producer, never a running one
            // (its replay would type a second time).
            let victim = marks
                .iter()
                .filter(|((r, _), m)| *r == realm && m.tip != Tip::Running)
                .min_by_key(|(_, m)| m.used)
                .map(|(p, _)| *p);
            match victim {
                Some(p) => {
                    marks.remove(&p);
                }
                None => return Claimed::Answer(format!("ERR busy idem={}\n", key.seq)),
            }
        }
        marks.insert(
            slot,
            Mark {
                high_water: key.seq,
                tip: Tip::Running,
                used,
            },
        );
        Claimed::Fresh(Claim {
            idem: self,
            realm,
            key,
            prior: None,
            done: false,
        })
    }

    /// Move this claim's tip, only while the claim still owns the mark. A live claim
    /// always does (a `Running` mark is never displaced, evicted, or released by
    /// another claim); the check is a second lock on that invariant.
    fn settle(&self, realm: Realm, key: Key, tip: Tip) {
        let used = self.tick();
        let mut marks = self.marks.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(mark) = marks.get_mut(&(realm, key.producer))
            && mark.high_water == key.seq
            && mark.tip == Tip::Running
        {
            mark.tip = tip;
            mark.used = used;
        }
    }

    /// Give the sequence back, restoring whatever this claim displaced — again
    /// only while the claim still owns the mark.
    fn release(&self, realm: Realm, key: Key, prior: Option<Mark>) {
        let slot = (realm, key.producer);
        let mut marks = self.marks.lock().unwrap_or_else(|p| p.into_inner());
        let owned = marks
            .get(&slot)
            .is_some_and(|m| m.high_water == key.seq && m.tip == Tip::Running);
        if !owned {
            return;
        }
        match prior {
            // A restored mark is never `Running` (`claim` refuses to displace a
            // running attempt). Should one reach here, `Tip::Unknown` is the
            // fail-visible restoration: evictable, and a replay is answered
            // `ERR in-doubt` rather than a busy that never clears.
            Some(prior) => {
                debug_assert!(
                    prior.tip != Tip::Running,
                    "`claim` must not have displaced a running mark"
                );
                marks.insert(
                    slot,
                    Mark {
                        tip: if prior.tip == Tip::Running {
                            Tip::Unknown
                        } else {
                            prior.tip
                        },
                        ..prior
                    },
                );
            }
            None => {
                marks.remove(&slot);
            }
        }
    }
}

/// The answer `mark` gives for `key`, or `None` when `key` is above the mark and
/// the decision is the caller's ([`PtyIdem::claim`] still refuses one while the
/// producer has an attempt in flight). Every answer is a refusal to write.
fn consumed_answer(mark: Mark, key: Key, dup: &str) -> Option<String> {
    if key.seq < mark.high_water {
        // The producer has moved past this sequence, so it was consumed. Only the
        // tip's outcome is remembered: a driver that advanced is not the one asking
        // to resolve an old in-doubt.
        return Some(dup.to_string());
    }
    if key.seq == mark.high_water {
        return Some(match mark.tip {
            Tip::Running => format!("ERR busy idem={}\n", key.seq),
            Tip::Applied => dup.to_string(),
            Tip::Unknown => format!("ERR in-doubt seq={}\n", key.seq),
        });
    }
    None
}

impl Drop for Claim<'_> {
    /// An unwind out of the guarded verb settles `Tip::Unknown`.
    fn drop(&mut self) {
        if !self.done {
            self.idem.settle(self.realm, self.key, Tip::Unknown);
        }
    }
}

/// Whether `reply` is a refusal that cannot have followed a write. Two prefixes,
/// deliberately: widening it is how a silent duplicate gets in.
fn refused_before_any_write(reply: &str) -> bool {
    // `ERR busy` (the drive lease, or a sink that accepted zero bytes) and
    // `ERR denied` (authority) are decided before any byte moves; see the module doc.
    reply.starts_with("ERR busy") || reply.starts_with("ERR denied")
}

/// `OK skipped`: a guarded attempt (`key if=<re>`, `send if=<re>`) whose guard
/// matched no visible row, or its fenced twins `reason=changed` (`if-gen=`,
/// `if-fp=`: the screen moved) and `reason=person` (`if-human=`: a person keyed
/// since the read). An `OK` that wrote nothing, decided under the terminal lock,
/// so the sequence is given back and the retry is re-evaluated against the live
/// screen. Checked before the plain `OK` arm, which would settle it.
fn skipped_by_guard(reply: &str) -> bool {
    reply.starts_with("OK skipped")
}

/// The `OK` a duplicate is answered with, in the framing the verb declares. A
/// `Lines` client (`turn`) reads a bare `OK dup=1`'s `dup=1` as a row count and
/// `aterm-ctl` answers `malformed response header`, so `OK <n>` framings get
/// `OK 0 dup=1`: a zero-row reply whose tail carries the marker, which
/// `stream_count` parses. The dup reply is runtime state `framing_of` cannot
/// express, so it is framed here.
fn dup_reply(verb: &str) -> String {
    use aterm_types::control_verbs::Framing;
    match aterm_types::control_verbs::framing_of(verb, verb) {
        // `OK <n>` header framings: a zero-length body, the marker in the tail. No
        // keyed verb is `Bytes` or `Push` today; the arm frames one that becomes so.
        Framing::Lines | Framing::Bytes | Framing::Push => "OK 0 dup=1\n".to_string(),
        Framing::Status => "OK dup=1\n".to_string(),
    }
}

/// A keyed attempt that carries no payload (`send id=<k>` with an empty tail,
/// `feed-bin 0 id=<k>`) would answer `OK` with nothing typed and settle its claim
/// `Applied`, consuming a sequence no byte moved under. Refuse it as a usage error
/// before [`guarded`], so a retry with the real payload is still a first attempt.
/// An unkeyed empty write still answers `OK`.
pub(crate) fn empty_payload_refusal(key: Option<&str>, payload_is_empty: bool) -> Option<String> {
    (key.is_some() && payload_is_empty)
        .then(|| "ERR usage: id= needs a non-empty payload\n".to_string())
}

/// Run `attempt` under the session's mark for `key`, or answer for it.
///
/// `key` is the raw `id=` value; `None` runs `attempt` unguarded (the key is
/// opt-in per request). `scope` picks the mark's [`Realm`]; `verb` picks the
/// duplicate reply's framing.
#[cfg(test)]
pub(crate) fn guarded<F>(
    ctx: &SessionCtx,
    scope: crate::control::Scope,
    verb: &str,
    key: Option<&str>,
    attempt: F,
) -> String
where
    F: FnOnce() -> String,
{
    guarded_waking(ctx, scope, verb, key, &|| {}, attempt)
}

/// [`guarded`], calling `wake` (the control worker's, for this session's
/// watchers) once the session log's readers are owed one: an `in-doubt`
/// mark was written, after its timeline hold was released.
pub(crate) fn guarded_waking<F>(
    ctx: &SessionCtx,
    scope: crate::control::Scope,
    verb: &str,
    key: Option<&str>,
    wake: &dyn Fn(),
    attempt: F,
) -> String
where
    F: FnOnce() -> String,
{
    let Some(raw) = key else {
        return attempt();
    };
    let key = match parse_key(raw, ctx.nonce) {
        Ok(key) => key,
        Err(reply) => return reply,
    };
    let realm = Realm::of(scope);
    let claim = match ctx.idem.claim(realm, key, &dup_reply(verb)) {
        Claimed::Fresh(claim) => claim,
        Claimed::Answer(reply) => return reply,
    };
    let reply = attempt();
    if skipped_by_guard(&reply) {
        claim.released();
    } else if reply.starts_with("OK") {
        claim.applied();
    } else if refused_before_any_write(&reply) {
        claim.released();
    } else {
        claim.unknown();
        // Visible, not silent: the reply tells this caller; the timeline row is
        // what a human (or the next `timeline since=`) reads afterwards. Recorded
        // after the mark is settled, so the marks lock is never held across the
        // timeline lock.
        if record_in_doubt(ctx, verb, key, &reply) {
            wake();
        }
    }
    reply
}

/// Append the `in-doubt` row: a per-session record, not a fabric event kind. It
/// carries the producer, the sequence and the reply's first two tokens joined with
/// `-` (`ERR-write` for `ERR write failed`), never the input bytes or the reply's
/// tail (a `turn` reply is a whole screen). The session log's `in-doubt` mark
/// goes in the same hold with the row's every field, named by its verb too,
/// the reply's words at the content tier (phase 6, 6F-7); the answer is
/// whether its readers are owed a wake.
pub(crate) fn record_in_doubt(ctx: &SessionCtx, verb: &str, key: Key, reply: &str) -> bool {
    let why = reply
        .trim_end_matches(['\r', '\n'])
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join("-");
    // ONE BOUND FOR THE ROW AND THE MARK (the review of 6F-7): the words as
    // the log's `reply=` holds them, hidden characters out and cut at its
    // cap, so the row never shows what the log does not hold.
    let why = aterm_slog::BoundedText::new(aterm_slog::Key::Reply, &why)
        .map(|t| t.as_str().to_string())
        .unwrap_or_default();
    let mut tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    tl.record(
        "in-doubt",
        format!(
            "producer={} seq={} reply={}",
            key.producer,
            key.seq,
            aterm_control::wire::pct_encode(&why)
        ),
    );
    let at = tl.slog().now();
    tl.slog_mut()
        .observe_in_doubt(verb, key.producer, key.seq, &why, at);
    tl.slog_take_wake()
}

#[cfg(test)]
mod tests {
    /// Only a keyed attempt with nothing to type is refused; an unkeyed empty
    /// write and a keyed non-empty one both pass through to the claim.
    #[test]
    fn only_a_keyed_empty_payload_is_refused_before_the_claim() {
        assert_eq!(
            empty_payload_refusal(Some("1:2:3"), true).as_deref(),
            Some("ERR usage: id= needs a non-empty payload\n")
        );
        assert_eq!(empty_payload_refusal(Some("1:2:3"), false), None);
        assert_eq!(empty_payload_refusal(None, true), None);
        assert_eq!(empty_payload_refusal(None, false), None);
    }

    use super::*;

    fn nonce() -> LaunchNonce {
        LaunchNonce::generate()
    }

    /// A producer with an attempt in flight is refused, not displaced, so the running
    /// attempt's outcome survives and a later release cannot reinstate an
    /// unsettleable `Running` tip. Two connections sharing a producer number is
    /// ordinary: a driver that runs `aterm-ctl` per request opens one per request.
    #[test]
    fn a_running_producer_is_refused_rather_than_displaced() {
        const DUP: &str = "OK dup=1\n";
        let idem = PtyIdem::default();
        let a = Key {
            producer: 7,
            seq: 5,
        };
        let b = Key {
            producer: 7,
            seq: 9,
        };

        let running = match idem.claim(Realm::Local, a, DUP) {
            Claimed::Fresh(claim) => claim,
            Claimed::Answer(reply) => panic!("a fresh sequence must claim: {reply}"),
        };
        // B arrives at a higher sequence while A is still inside the seam.
        match idem.claim(Realm::Local, b, DUP) {
            Claimed::Answer(reply) => assert_eq!(
                reply, "ERR busy idem=5\n",
                "the refusal names the sequence actually in flight, not the caller's"
            ),
            Claimed::Fresh(_) => {
                panic!("a running mark was displaced: A can no longer settle its own outcome")
            }
        }

        // A's outcome survives.
        running.applied();
        match idem.claim(Realm::Local, a, DUP) {
            Claimed::Answer(reply) => {
                assert_eq!(reply, DUP, "the replay of an applied seq is a dup")
            }
            Claimed::Fresh(_) => panic!("an applied sequence must not run twice"),
        }

        // And the producer is not wedged: B proceeds now, and a refusal before any
        // write hands the sequence back rather than leaving an unsettleable tip.
        let b_claim = match idem.claim(Realm::Local, b, DUP) {
            Claimed::Fresh(claim) => claim,
            Claimed::Answer(reply) => panic!("B must proceed once A has settled: {reply}"),
        };
        b_claim.released();
        match idem.claim(Realm::Local, b, DUP) {
            Claimed::Fresh(claim) => claim.applied(),
            Claimed::Answer(reply) => {
                panic!("a released sequence is a FIRST attempt, not a duplicate: {reply}")
            }
        }
        // The restored mark is A's settled one, so A still answers a duplicate.
        match idem.claim(Realm::Local, a, DUP) {
            Claimed::Answer(reply) => assert_eq!(reply, DUP),
            Claimed::Fresh(_) => panic!("A's sequence is consumed"),
        }
    }

    #[test]
    fn feed_idempotent_key_grammar_is_epoch_producer_seq() {
        let live = nonce();
        let hex = live.to_hex();
        assert_eq!(
            parse_key(&format!("{hex}:7:1"), live),
            Ok(Key {
                producer: 7,
                seq: 1
            })
        );
        // A key minted for another incarnation is refused by name, not silently
        // applied and not silently suppressed.
        let dead = nonce();
        assert_eq!(
            parse_key(&format!("{}:7:1", dead.to_hex()), live).unwrap_err(),
            EPOCH_REFUSAL
        );
        for bad in [
            String::from("nope"),
            format!("{hex}:7"),
            format!("{hex}:7:1:2"),
            format!("{hex}:x:1"),
            format!("{hex}:7:0"),
            format!("{hex}:7:-1"),
            format!("{}:7:1", &hex[..30]),
        ] {
            assert_eq!(
                parse_key(&bad, live).unwrap_err(),
                USAGE,
                "must not parse: {bad}"
            );
        }
    }
}
