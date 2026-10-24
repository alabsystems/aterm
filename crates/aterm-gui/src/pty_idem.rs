// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The idempotency key at the PTY seam.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use aterm_session::LaunchNonce;

use crate::SessionCtx;

/// The namespace a mark lives in: the authority half of the key, which the caller does not type.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Realm {
    /// The fabric bridge's own connection.
    Bridge,
    /// Every other scope: Owner, and every edge token.
    Local,
}

impl Realm {
    /// The namespace a connection's marks live in.
    pub(crate) const fn of(scope: crate::control::Scope) -> Self {
        match scope {
            #[cfg(any(unix, test))]
            crate::control::Scope::Bridge => Self::Bridge,
            crate::control::Scope::Owner | crate::control::Scope::Edge(_) => Self::Local,
            #[cfg(any(unix, test))]
            crate::control::Scope::Fleet { .. } => Self::Local,
        }
    }
}

/// How many producers one session remembers marks for, per [`Realm`]; headroom
/// over the handful of drivers a session sees.
const PRODUCER_CAP: usize = 64;

/// The state of the sequence at a producer's high-water mark.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tip {
    /// Claimed and still running on some connection.
    Running,
    /// It ran and answered `OK`.
    Applied,
    /// It ran and did not answer `OK`, so a replay is refused as in-doubt.
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

/// The per-session marks.
#[derive(Default)]
pub(crate) struct PtyIdem {
    marks: Mutex<HashMap<(Realm, u64), Mark>>,
    /// Monotone counter feeding `Mark::used` for LRU eviction.
    clock: AtomicU64,
}

/// A parsed, epoch-checked `id=` key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Key {
    producer: u64,
    seq: u64,
}

/// The usage line every malformed key answers.
const USAGE: &str = "ERR usage: id=<epoch>:<producer>:<seq>\n";

/// The answer to a well-formed key minted for another launch of the session: the head drivers key on, and where the live epoch is read.
pub(crate) const EPOCH_REFUSAL: &str =
    "ERR epoch: id= is from another launch; use this session's nonce= from sessions\n";

/// Parse `<epoch>:<producer>:<seq>` and check the epoch against the live session's launch nonce.
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
    if seq == 0 {
        return Err(USAGE.to_string());
    }
    if !epoch.ct_eq(&live) {
        return Err(EPOCH_REFUSAL.to_string());
    }
    Ok(Key { producer, seq })
}

/// The claim one attempt holds while it runs, settling to `Tip::Unknown` if dropped without settling.
struct Claim<'a> {
    idem: &'a PtyIdem,
    realm: Realm,
    key: Key,
    /// The mark this claim displaced, restored on release.
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

    /// Reserve `key`'s sequence in `realm`, or answer for it using `dup` on duplicates.
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
        if marks.keys().filter(|(r, _)| *r == realm).count() >= PRODUCER_CAP {
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

    /// Move this claim's tip while the claim still owns the mark.
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
/// producer has an attempt in flight).
fn consumed_answer(mark: Mark, key: Key, dup: &str) -> Option<String> {
    if key.seq < mark.high_water {
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

/// Whether `reply` is a refusal that cannot have followed a write: `ERR busy…` and `ERR denied`. Every producer answers these heads only while nothing of the attempt has reached the PTY; one that has written answers some other `ERR`, which leaves the sequence in doubt.
fn refused_before_any_write(reply: &str) -> bool {
    reply.starts_with("ERR busy") || reply.starts_with("ERR denied")
}

/// An attempt's own word that none of its bytes reached the PTY, for a verb that keeps an account of its writes (`turn`, through [`crate::control_session::TurnIo::witness`]); the reply alone cannot say it, since `ERR exited` or a skip answers both before and after a write.
#[derive(Default)]
pub(crate) struct Witness(std::cell::Cell<bool>);

impl Witness {
    /// The attempt answers having written nothing: its sequence goes back, whatever the reply.
    pub(crate) fn nothing_written(&self) {
        self.0.set(true);
    }
}

/// Whether `reply` is `OK skipped` from a non-matching guard.
fn skipped_by_guard(reply: &str) -> bool {
    reply.starts_with("OK skipped")
}

/// The `OK` a duplicate is answered with, in the framing the verb declares.
fn dup_reply(verb: &str) -> String {
    use aterm_types::control_verbs::Framing;
    match aterm_types::control_verbs::framing_of(verb, verb) {
        Framing::Lines | Framing::Bytes | Framing::Push => "OK 0 dup=1\n".to_string(),
        Framing::Status => "OK dup=1\n".to_string(),
    }
}

/// A keyed attempt that carries no payload (`send id=<k>` with an empty tail, `feed-bin 0 id=<k>`) would answer `OK` with nothing typed and settle its claim `Applied`, consuming a sequence no byte moved under.
pub(crate) fn empty_payload_refusal(key: Option<&str>, payload_is_empty: bool) -> Option<String> {
    (key.is_some() && payload_is_empty)
        .then(|| "ERR usage: id= needs a non-empty payload\n".to_string())
}

/// Run `attempt` under the session's mark for `key`, or answer for it.
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
    guarded_waking(ctx, scope, verb, key, &|| {}, |_| attempt())
}

/// `guarded`, calling `wake` (the control worker's, for this session's watchers) once the session log's readers are owed one: an `in-doubt` mark was written, after its timeline hold was released. `attempt` is handed the [`Witness`] it may tell that it wrote nothing.
pub(crate) fn guarded_waking<F>(
    ctx: &SessionCtx,
    scope: crate::control::Scope,
    verb: &str,
    key: Option<&str>,
    wake: &dyn Fn(),
    attempt: F,
) -> String
where
    F: FnOnce(&Witness) -> String,
{
    let witness = Witness::default();
    let Some(raw) = key else {
        return attempt(&witness);
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
    let reply = attempt(&witness);
    if witness.0.get() || skipped_by_guard(&reply) {
        claim.released();
    } else if reply.starts_with("OK") {
        claim.applied();
    } else if refused_before_any_write(&reply) {
        claim.released();
    } else {
        claim.unknown();
        if record_in_doubt(ctx, verb, key, &reply) {
            wake();
        }
    }
    reply
}

/// Append the `in-doubt` row: a per-session record, not a fabric event kind.
pub(crate) fn record_in_doubt(ctx: &SessionCtx, verb: &str, key: Key, reply: &str) -> bool {
    let why = reply
        .trim_end_matches(['\r', '\n'])
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join("-");
    let why = aterm_slog::BoundedText::new(aterm_slog::Key::Reply, &why)
        .map(|t| t.as_str().to_string())
        .unwrap_or_default();
    let mut tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    let at = tl.slog().now();
    tl.slog_mut()
        .observe_in_doubt(verb, key.producer, key.seq, &why, at);
    tl.record("in-doubt");
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
    /// unsettleable `Running` tip.
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
        match idem.claim(Realm::Local, b, DUP) {
            Claimed::Answer(reply) => assert_eq!(
                reply, "ERR busy idem=5\n",
                "the refusal names the sequence actually in flight, not the caller's"
            ),
            Claimed::Fresh(_) => {
                panic!("a running mark was displaced: A can no longer settle its own outcome")
            }
        }

        running.applied();
        match idem.claim(Realm::Local, a, DUP) {
            Claimed::Answer(reply) => {
                assert_eq!(reply, DUP, "the replay of an applied seq is a dup")
            }
            Claimed::Fresh(_) => panic!("an applied sequence must not run twice"),
        }

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
