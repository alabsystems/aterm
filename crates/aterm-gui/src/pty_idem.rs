// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **A6 — the idempotency key at the PTY seam** (design §6.5's last open row,
//! spelled out in §11.2's "The idempotency key at the PTY seam").
//!
//! §6.5's table has one hop still marked DESIGNED: `bus → PTY (term/in)`.
//! `feed-bin` is not idempotent, so a bridge that crashes between "wrote the
//! bytes" and "recorded that it wrote them" has two bad options. Replay, and the
//! keystroke is typed twice. Do not replay, and the keystroke is lost — which is
//! what the bridge does today, silently, by subscribing the drive face from the
//! HEAD on every attach. **A silent duplicate and a silent loss are both
//! failures**; the design asks for neither.
//!
//! This module is the missing half. A driver stamps an input verb with
//! `id=<epoch>:<producer>:<seq>`; the endpoint keeps a per-session, per-producer
//! HIGH-WATER mark and answers an already-consumed sequence `OK dup=1` **without
//! writing**. A crash inside the window is then no longer in doubt from the
//! outside: the retry's answer says which side of the window it fell on.
//!
//! ## The three fields, and why each one is load-bearing
//!
//! * `<epoch>` — the session's launch nonce (`LaunchNonce`, 32 lowercase hex),
//!   the same public anti-spoof value the Owner-only `sessions` roster carries as
//!   `nonce=` and §6.6 requires on a `term/in` body. A high-water mark is only
//!   meaningful inside one incarnation of one shell: a relaunched session is a
//!   FRESH mark, or an id minted for a dead shell would suppress a live
//!   keystroke. Rather than leave that to the fact that a fresh session gets a
//!   fresh [`SessionCtx`], the epoch is carried IN the key and checked, so a key
//!   from a dead session is REFUSED (`ERR epoch`) instead of quietly doing
//!   something. Refused is visible; either silent outcome is not.
//! * `<producer>` — the u64 the marks are keyed by, WITHIN the caller's
//!   authority namespace ([`Realm`]). It is the bridge's astream `producer_id` on
//!   the fabric path; anything stable per driver otherwise. The namespace is the
//!   important half and it is NOT the caller's to choose: the producer NUMBER is
//!   a string the caller types, so a number alone made every producer's whole
//!   sequence space claimable by anyone the dispatch had already authorized. See
//!   [`Realm`] for what is enforced and what deliberately is not.
//! * `<seq>` — the driver's own monotone sequence, exactly the
//!   `(producer_id, producer_seq)` shape the broker already dedups ingest with
//!   (§6.5's first row). A monotone sequence is what makes the mark O(1): at or
//!   below the mark is consumed, above it is new.
//!
//! ## What an attempt can end as, and why there are four answers
//!
//! | Outcome | Answer | Meaning |
//! |---|---|---|
//! | above the mark | the verb's own reply | fresh; it ran |
//! | at/below the mark, applied | `OK dup=1` | already typed; nothing written |
//! | at the mark, still running | `ERR busy idem=<seq>` | another connection holds it; transient |
//! | at the mark, outcome unknown | `ERR in-doubt seq=<seq>` | it may have typed; DO NOT replay |
//!
//! The fourth row is the point of the rung. An attempt whose reply was not `OK`
//! may still have reached the PTY — `cmd_turn` can type its text and then fail to
//! submit — so the mark is kept and the outcome is recorded as UNKNOWN. A retry
//! of that exact sequence is told so, in those words, and the session's
//! `timeline` carries an `in-doubt` row a human can read. The alternative — free
//! the mark and let the retry type again — is the silent duplicate.
//!
//! **The two carve-outs.** `ERR busy` (the drive lease) and `ERR denied`
//! (authority) release the mark instead of clouding it. For all four verbs both
//! are decided before the first byte: the dispatch fast-fail and op-scope gate,
//! `cmd_turn`'s read-authority check and its authoritative lease acquire (its
//! first act after option parsing), and `run_feed_bin_routed`'s edge check and
//! lease mirror, both of which precede its write. `ERR busy` is also the reply a
//! well-behaved driver retries most, so making it sticky would be a nuisance for
//! no safety. NOTHING ELSE is carved out — in particular NOT `ERR usage`,
//! because `cmd_turn_guarded` answers its own USAGE string when a submit press
//! fails, which happens AFTER the text has been typed (`control_session.rs`, the
//! `io.press` arm). A reply string is not evidence about the PTY.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use aterm_session::LaunchNonce;

use crate::SessionCtx;

/// The verbs that accept `id=<key>` — §11.2's set, exactly. Every one of them can
/// put bytes on the PTY and no other verb may claim a mark, so a mark can only
/// ever be consumed by something that CAN type.
///
/// It can still be consumed by something that DIDN'T: `send` with an empty tail
/// answers `OK` and types nothing, so a claim settles `Applied` with no bytes
/// moved. That is bounded to the caller's own [`Realm`] and no longer reaches the
/// bridge's marks, which is the property that matters; within one realm it stays
/// true and is stated rather than papered over.
pub(crate) const KEYED_VERBS: &[&str] = &["send", "key", "feed-bin", "turn"];

/// THE NAMESPACE A MARK LIVES IN — the authority half of the key, and the half
/// the caller does not get to type.
///
/// WHY THIS EXISTS. A mark is a claim on a sequence, and naming a producer's next
/// sequence is what suppresses the write. Keyed only by the caller's `<producer>`
/// STRING, every mark was claimable by every authorized caller: an in-session
/// agent (Owner, which every `aterm-ctl @self` holds) could run one
/// `send id=<epoch>:<the bridge's producer id>:18446744073709551615`, settle the
/// mark `Applied` with an empty tail, and from then on every `Bridge::feed` —
/// whose key is `{epoch}:{producer_id}:{off+1}` and therefore always lower — was
/// answered `OK dup=1` with NOTHING written, while the bridge published
/// `ev applied dup=1` on the fleet log. §6.6's structural "human always wins"
/// drive path would have been silently dead with §10's causal record asserting
/// the opposite. The epoch is no defence: it is public anti-spoof state, on the
/// Owner-readable roster as `nonce=` and in every session's
/// `$ATERM_LAUNCH_NONCE`.
///
/// EXACTLY TWO, AND THE SPLIT IS THE DOCTRINE. `Scope::Bridge` is a CONNECTION —
/// the socketpair end this instance handed its own child — so it is the one
/// authority no token opens, and it gets a namespace nothing else can CLAIM in.
/// Everything else shares [`Realm::Local`], which is where it already was.
///
/// ANSWERING IS NOT CLAIMING, and only claiming is fenced. A `Local` caller that
/// names a sequence the BRIDGE has already consumed is still answered from the
/// bridge's mark — `OK dup=1`, `ERR busy`, `ERR in-doubt` — because every one of
/// those answers writes NOTHING, and "a consumed sequence is answered without
/// writing, however it is asked" is the property `aterm-link`'s
/// `a_consumed_sequence_is_answered_without_writing_however_it_is_asked` names
/// and an operator debugging a stuck driver depends on. What a `Local` caller may
/// never do is INSTALL a high-water in the bridge's namespace, which is the act
/// that suppresses a write. The fallback is one-way: a `Bridge` claim never
/// consults `Local`, or a local driver could suppress the human's keystroke by
/// the same trick from the other side.
///
/// The costs of the one-way fallback, said out loud: a `Local` caller can PROBE
/// the bridge's high-water for a producer id it guesses (it already holds the
/// instance's god token and can read the fleet log), and a local driver that
/// picks the bridge's producer id can have its own low sequences answered as
/// duplicates. Neither writes anything, and neither can silence the bridge.
///
/// NOT ONE REALM PER CONNECTION OR PER EDGE TOKEN, and this is a bound, not an
/// oversight: an Owner caller can mint edge tokens, so a realm per token would
/// let it grow this map without limit, and [`PRODUCER_CAP`] is enforced PER REALM
/// precisely so a local driver churning producer ids cannot evict the bridge's
/// settled mark and turn its next replay into a second keystroke. Two realms,
/// each capped: at most `2 * PRODUCER_CAP` marks per session, and the bridge's
/// half is unreachable from any token.
///
/// WHAT IS THEREFORE NOT ENFORCED, said out loud: two `Local` drivers still share
/// a namespace, so one can still burn the other's sequence. Inside one authority
/// class there is nothing to tell them apart, and inventing a distinction here
/// would be inventing an authority the connection does not carry.
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
            crate::control::Scope::Bridge => Self::Bridge,
            crate::control::Scope::Owner | crate::control::Scope::Edge(_) => Self::Local,
        }
    }
}

/// Whether `verb` accepts an `id=` idempotency key.
pub(crate) fn is_keyed_verb(verb: &str) -> bool {
    KEYED_VERBS.contains(&verb)
}

/// How many producers one session remembers marks for, PER [`Realm`].
///
/// A session realistically sees one driver, or a handful; 64 is headroom, not a
/// budget. Past it the LEAST-RECENTLY-USED settled producer IN THE SAME REALM is
/// evicted, and an evicted producer's next replay is applied again rather than
/// recognized — the at-least-once fallback. That is stated rather than hidden
/// because it is the one way this seam can still duplicate; it takes 64 distinct
/// drivers on ONE session to reach it. A producer whose attempt is still RUNNING
/// is never evicted (its guard would settle a mark that no longer exists), so a
/// session with 64 concurrent in-flight producers in one realm answers
/// `ERR busy idem=` instead.
///
/// PER REALM and not per session: eviction that crossed realms would let a local
/// driver churning 64 producer ids push out the BRIDGE's settled mark, which
/// turns the bridge's next replay into a second keystroke — the silent duplicate
/// this whole module exists to remove.
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

/// The per-session marks. A LEAF lock: taken to claim and taken to settle, never
/// held across the verb it guards (the verb blocks on the event loop, the turn
/// lease, and the PTY) and never held while recording a timeline row.
#[derive(Default)]
pub(crate) struct PtyIdem {
    marks: Mutex<HashMap<(Realm, u64), Mark>>,
    /// Feeds `Mark::used`. Only the ORDER matters (it picks the LRU victim), so
    /// an atomic counter outside the marks lock is enough and keeps this a
    /// single-lock type.
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

/// Parse `<epoch>:<producer>:<seq>` and check the epoch against the live
/// session's launch nonce.
///
/// `Err` is the caller's whole reply. An unparseable key is `ERR usage`; a
/// well-formed key minted for a DIFFERENT incarnation of this session is
/// `ERR epoch` — §6.6's `reason=epoch` refusal, applied to the socket seam the
/// bridge drives through rather than to the record it drives from.
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
        return Err("ERR epoch\n".to_string());
    }
    Ok(Key { producer, seq })
}

/// Take a LEADING `id=<key>` option off a keyed verb's argument tail.
///
/// OPTIONS LEAD, the rule `post`'s frame detector already runs on: the key is
/// recognized only as the FIRST token, so `send hello id=1` sends the eight
/// characters `hello id=1` exactly as it always did and only a first token
/// spelled `id=` changes meaning. A leading `--` ends option parsing and is
/// dropped, so a caller that really must `send` text beginning with `id=` writes
/// `send -- id=…`.
///
/// Returns `(the key value, the remaining tail)`. Only [`KEYED_VERBS`] are
/// scanned; every other verb's tail is returned untouched, so an `id=` token
/// elsewhere stays argument data.
pub(crate) fn take_key(verb: &str, rest: &str) -> (Option<String>, String) {
    if !is_keyed_verb(verb) {
        return (None, rest.to_string());
    }
    let trimmed = rest.trim_start();
    let (head, tail) = match trimmed.split_once(char::is_whitespace) {
        Some((h, t)) => (h, t.trim_start()),
        None => (trimmed, ""),
    };
    if head == "--" {
        return (None, tail.to_string());
    }
    match head.strip_prefix("id=") {
        Some(value) => (Some(value.to_string()), tail.to_string()),
        None => (None, rest.to_string()),
    }
}

/// The claim one attempt holds while it runs. Settled by [`guarded`]; a drop
/// without a settle (an unwind out of the guarded verb) leaves the mark UNKNOWN,
/// which is the fail-visible direction — the same reasoning as A2's
/// `BridgeLostGuard`: a mark that a panic frees is not a mark.
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

    /// Reserve `key`'s sequence in `realm`, or answer for it.
    ///
    /// `dup` is the duplicate reply in the CALLING VERB's framing — see
    /// [`dup_reply`]; a `Lines`-framed verb cannot be answered a bare
    /// `OK dup=1`, because its client reads the header's second token as a row
    /// count.
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
        // ANSWER-ONLY FALLBACK into the bridge's namespace. Every branch of
        // `consumed_answer` writes nothing, so this can refuse a write and never
        // cause one; installing below happens only in the caller's OWN realm.
        // One-way on purpose — see [`Realm`].
        if realm != Realm::Bridge
            && let Some(mark) = marks.get(&(Realm::Bridge, key.producer)).copied()
            && let Some(answer) = consumed_answer(mark, key, dup)
        {
            return Claimed::Answer(answer);
        }
        if let Some(mark) = own {
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
        // The cap is counted, and the victim chosen, WITHIN this realm: see
        // [`PRODUCER_CAP`]. A local driver churning producer ids must not be able
        // to evict the bridge's settled mark.
        if marks.keys().filter(|(r, _)| *r == realm).count() >= PRODUCER_CAP {
            // Evict the least-recently-used SETTLED producer. A running one is
            // never evicted: its guard would settle a mark that is gone, and a
            // later replay of it would type a second time.
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

    /// Move this claim's tip, but ONLY while the claim still owns the mark. A
    /// second claim from the same producer at a HIGHER sequence supersedes this
    /// one, and must not be clobbered by a laggard settling underneath it.
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
                marks.insert(slot, prior);
            }
            None => {
                marks.remove(&slot);
            }
        }
    }
}

/// The answer `mark` gives for `key`, or `None` when `key` is ABOVE the mark and
/// is therefore a fresh attempt. Every answer this returns is a refusal to write.
fn consumed_answer(mark: Mark, key: Key, dup: &str) -> Option<String> {
    if key.seq < mark.high_water {
        // The producer has moved past this sequence, so it was consumed. Its
        // individual outcome is no longer remembered — only the tip is — which is
        // honest: a driver that has advanced its sequence is not the one asking to
        // resolve an old in-doubt.
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
    /// An unwind out of the guarded verb settles UNKNOWN. Only a settle that ran
    /// can say more than that.
    fn drop(&mut self) {
        if !self.done {
            self.idem.settle(self.realm, self.key, Tip::Unknown);
        }
    }
}

/// Whether `reply` is a refusal that CANNOT have followed a write. See the
/// module header: this is two prefixes, deliberately, and widening it further is
/// how a silent duplicate gets in.
fn refused_before_any_write(reply: &str) -> bool {
    // `ERR busy` — the drive lease. `ERR denied` — authority. For all four keyed
    // verbs both are decided before the first byte: the dispatch fast-fail and
    // op-scope gate, `turn`'s own read-authority check at the top of its arm and
    // its authoritative lease acquire, and `run_feed_bin_routed`'s edge check and
    // lease mirror, which both precede its write. Neither can be reached again
    // once a verb has started typing.
    reply.starts_with("ERR busy") || reply.starts_with("ERR denied")
}

/// The `OK` a duplicate is answered with, in the FRAMING the verb declares.
///
/// A duplicate must be a well-formed reply for the verb it duplicates, not merely
/// a well-formed line. `turn` is `Framing::Lines` — `OK <n>` then n rows — so a
/// bare `OK dup=1` makes a Lines client read `dup=1` as a row count: `aterm-ctl`
/// answers `malformed response header: "OK dup=1"` and exits 1, which a driver
/// retrying after a crash cannot tell from a broken server. `OK 0 dup=1` says the
/// same thing inside the verb's own grammar — a zero-row listing whose tail
/// carries the marker, which `stream_count` already parses.
///
/// A1 added `framing_of`'s `inbox get`/`inbox seen`/`outbox sent` flips for
/// exactly this hazard ("a client that read `OK seen=42` as a row count hangs
/// waiting for 42 rows"); the dup reply is runtime state that `framing_of` cannot
/// express, so it is honoured here instead.
fn dup_reply(verb: &str) -> String {
    use aterm_types::control_verbs::Framing;
    match aterm_types::control_verbs::framing_of(verb, verb) {
        // `OK <n>` header framings: a zero-length body, with the marker in the
        // tail. (`Bytes` is `OK <nbytes>` + that many bytes, so zero bytes is the
        // same shape. No keyed verb is `Bytes` or `Push` today; the arm exists so
        // one that becomes so is framed rather than malformed.)
        Framing::Lines | Framing::Bytes | Framing::Push => "OK 0 dup=1\n".to_string(),
        Framing::Status => "OK dup=1\n".to_string(),
    }
}

/// Run `attempt` under the session's mark for `key`, or answer for it.
///
/// `key` is the RAW `id=` value (`None` when the caller passed none, in which
/// case `attempt` runs unguarded and the seam behaves exactly as it did before
/// this rung — the whole feature is opt-in per request).
///
/// `scope` picks the mark's [`Realm`]: the producer NUMBER is the caller's to
/// type, the NAMESPACE is not. `verb` picks the duplicate reply's framing.
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
    let Some(raw) = key else {
        return attempt();
    };
    let key = match parse_key(raw, ctx.nonce) {
        Ok(key) => key,
        Err(reply) => return reply,
    };
    let realm = Realm::of(scope);
    let claim = match ctx.fabric.idem().claim(realm, key, &dup_reply(verb)) {
        Claimed::Fresh(claim) => claim,
        Claimed::Answer(reply) => return reply,
    };
    let reply = attempt();
    if reply.starts_with("OK") {
        claim.applied();
    } else if refused_before_any_write(&reply) {
        claim.released();
    } else {
        claim.unknown();
        // VISIBLE, not silent. The reply already tells THIS caller; the timeline
        // row is what a human (or the next `timeline since=`) reads afterwards,
        // and it is why the loss the bridge takes today stops being invisible.
        // Recorded after the mark is settled, so the marks lock is never held
        // across the timeline lock.
        record_in_doubt(ctx, key, &reply);
    }
    reply
}

/// Append the `in-doubt` row. Deliberately NOT a fabric event kind: the five
/// `EVENT` names of §11.2 are pinned, and this is a per-session record of an
/// input whose fate is unknown, not a fabric message. It carries the producer,
/// the sequence and the refusal's FIRST token only — never the input bytes, and
/// never the reply's tail (a `turn` reply is a whole screen).
fn record_in_doubt(ctx: &SessionCtx, key: Key, reply: &str) {
    let why = reply
        .trim_end_matches(['\r', '\n'])
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join("-");
    ctx.timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .record(
            "in-doubt",
            format!(
                "producer={} seq={} reply={}",
                key.producer,
                key.seq,
                aterm_control::wire::pct_encode(&why)
            ),
        );
}

#[cfg(test)]
mod tests {

    /// `send`'s help SPLITS the keyed verbs by framing, and the split is derived
    /// from the table — so the roster in the prose must agree with it.
    ///
    /// It did not. The sentence filed `feed-bin` under "`OK 0 dup=1` for a
    /// `Lines`/`Bytes` one", but `feed-bin` is declared `Status`
    /// (`control_verbs.rs`, the `v("feed-bin", Write, Status, …)` row), so
    /// `dup_reply` answers `OK dup=1` for it — as this crate's own test at
    /// `control.rs` already asserted. The code and its test agreed with each
    /// other and disagreed with the help.
    ///
    /// The prior catalog check asserted only that both PHRASES appear, which is
    /// why a verb on the wrong side of the split survived it. This derives each
    /// verb's side from `framing_of` and requires the prose to name it there.
    #[test]
    fn the_send_help_files_every_keyed_verb_on_the_side_its_framing_puts_it() {
        use aterm_types::control_verbs::{Framing, framing_of, spec};
        let detail = spec("send").expect("`send` is a catalog verb").detail;
        // The two PARENTHESISED rosters, not "everything before/after the split
        // point". A first draft split the detail on `` `OK 0 dup=1` `` and looked
        // for each verb in the prefix — and passed the plant, because `feed-bin`
        // is also named two sentences earlier ("`key`, `feed-bin` and `turn` take
        // the same key"). A roster check must read the roster.
        let roster = |after: &str| -> String {
            let tail = detail
                .split_once(after)
                .unwrap_or_else(|| panic!("`send`'s help states {after:?}"))
                .1;
            tail.split_once(')')
                .expect("the roster is parenthesised")
                .0
                .to_string()
        };
        let status = roster("`Status`-framed verb (");
        let lines = roster("`Lines`/`Bytes` one (");
        for verb in KEYED_VERBS {
            let quoted = format!("`{verb}`");
            let (side, other, name) = match framing_of(verb, verb) {
                Framing::Status => (&status, &lines, "Status"),
                _ => (&lines, &status, "Lines/Bytes"),
            };
            assert!(
                side.contains(&quoted),
                "`{verb}` is {name}-framed, so `send`'s help must list it in the \
                 {name} roster; that roster reads ({side})"
            );
            assert!(
                !other.contains(&quoted),
                "`{verb}` is {name}-framed but `send`'s help also lists it in the \
                 OTHER roster ({other}) — a reader cannot tell which reply to expect"
            );
        }
    }
    use super::*;

    fn nonce() -> LaunchNonce {
        LaunchNonce::generate()
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
        // A key minted for another incarnation is refused by NAME, not silently
        // applied and not silently suppressed.
        let dead = nonce();
        assert_eq!(
            parse_key(&format!("{}:7:1", dead.to_hex()), live).unwrap_err(),
            "ERR epoch\n"
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

    #[test]
    fn feed_idempotent_options_lead_so_a_body_id_is_body() {
        assert_eq!(
            take_key("send", "id=a:b:c hello there"),
            (Some("a:b:c".to_string()), "hello there".to_string())
        );
        // NOT the first token ⇒ argument data, byte-identical to before.
        assert_eq!(
            take_key("send", "hello id=a:b:c"),
            (None, "hello id=a:b:c".to_string())
        );
        // `--` is the escape hatch for text that really does start with `id=`.
        assert_eq!(
            take_key("send", "-- id=literal"),
            (None, "id=literal".to_string())
        );
        // An unkeyed verb is never scanned.
        assert_eq!(
            take_key("paste", "id=a:b:c hi"),
            (None, "id=a:b:c hi".to_string())
        );
        // A lone key with no tail is legal (`key id=… enter` has a tail; `send
        // id=…` types nothing, which is what `send` with no text already does).
        assert_eq!(
            take_key("turn", "id=a:b:c"),
            (Some("a:b:c".to_string()), String::new())
        );
    }
}
