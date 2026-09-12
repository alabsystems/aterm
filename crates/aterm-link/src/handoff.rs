// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **`decide_control` — §6.6's handoff table, as one pure function.**
//!
//! The keyboard has one holder. On the bus that is the last-value
//! `/f/<F>/pub/<node>/<sid>/control` row, written only by the session's node;
//! on aterm it is the cooperative `lease`. This module is the policy that keeps
//! them equal, and it is deliberately pure: it reads no clock, opens no socket
//! and publishes nothing, so every row of §6.6's table is a unit test rather
//! than an end-to-end story.
//!
//! | Event | Rule |
//! |---|---|
//! | `claim` by an `h-*` principal carrying the session's `epoch=` | granted unconditionally; the previous holder gets `control lost` |
//! | `request` by an agent | granted iff `holder ∈ {none, expired}` and no halt; else a pending row the holder's next wake shows |
//! | `release` by the holder | holder = none |
//! | `grant <p>` by the holder or an `h-*` | holder = `<p>` |
//! | an unaccounted `status revision=` advance with no `term/in` in flight | `holder=human? evidence=unaccounted-change` — the conservative pause, labelled inferred |
//! | a local `lease acquire` by a socket driver | mirrored as `holder=owner-cli:<h>` |
//!
//! ## What "expired" means here, and why it is not a timer
//!
//! §6.6's `request` row grants when the holder is `none` **or `expired`**, and
//! names no expiry mechanism. Two readings were available and only one is safe.
//!
//! A TTL on the bus row would mean republishing that row while the holder
//! stands — a retained record every renewal period, per held session, forever.
//! That is a durable, unbounded, silent cost, and this project has a rule about
//! trading one hole for one of those. The mirror lease's TTL exists for exactly
//! this and costs nothing: it is a local verb call.
//!
//! So `expired` is **observed, not timed**: a holder is expired when the thing
//! that could still act as it is gone — its session has exited, or the
//! conservative pause has already parked the row at `human?`. A human's claim
//! never expires on its own; a human keeps the wheel until they release it or
//! another human takes it. The cost is stated: an agent that claims control and
//! then goes quiet without exiting holds the row until someone takes it back,
//! and the human's `claim` is the thing that takes it back — unconditionally,
//! which is the first row of the table.

/// The `holder=` value the conservative pause parks a session at (§6.6). NOT a
/// principal by §3.2's grammar, and deliberately so: `on_term_record` compares
/// the holder to a record's cap-forced `<src>` segment, and no `<src>` can ever
/// equal this, so a paused session applies nothing from the bus until somebody
/// claims it again. The pause is a refusal that reads as an explanation.
pub const PAUSED: &str = "human?";

/// What the bus and the local instance say about one session's keyboard.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    /// The `holder=` on the last-value `control` row, if any.
    pub holder: Option<String>,
    /// Whether that holder can still act: `false` once its session has exited.
    /// A holder that is not a session (a human, a service) is always live —
    /// there is nothing local to observe about it.
    pub holder_live: bool,
    /// Whether a fleet halt stands over this session (§5.3).
    pub halted: bool,
}

impl State {
    /// Whether the holder slot is takeable by an agent's `request`: nobody
    /// holds it, the holder's session is gone, or the conservative pause has
    /// already parked it.
    #[must_use]
    pub fn free_for_request(&self) -> bool {
        match &self.holder {
            None => true,
            Some(h) => h == PAUSED || !self.holder_live,
        }
    }
}

/// One thing that happened to a session's keyboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// An `h-*` principal claimed it (§6.6 row 1). The epoch was already
    /// checked by the caller — an epoch mismatch never reaches this function,
    /// because it is not a policy question.
    Claim { by: String },
    /// A non-human principal asked for it (row 2).
    Request { by: String },
    /// The holder gave it up (row 3).
    Release { by: String },
    /// The holder, or any human, handed it to `to` (row 3).
    Grant { by: String, to: String },
    /// A local `status revision=` advance the bridge did not cause (row 4).
    UnaccountedChange,
    /// A local socket driver took aterm's own cooperative lease (row 5).
    LocalLease { holder: String },
}

/// What the bridge should do about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Move the holder. `lost` is the principal to send `control lost` to.
    Hold {
        holder: String,
        evidence: &'static str,
        lost: Option<String>,
    },
    /// Clear the holder.
    Free {
        evidence: &'static str,
        lost: Option<String>,
    },
    /// Record `pending=<by>` on the row so the holder's next wake shows it, and
    /// tell the holder. NOT a grant: an agent asking while a human drives waits.
    Pending { by: String },
    /// Say no, and say why. `reason` lands on the node's `ev` digest.
    Refuse { reason: &'static str },
    /// Nothing changed, so nothing is published. This is what keeps a repeated
    /// observation (the same lease seen on every sample, a second unaccounted
    /// change while already paused) from becoming a record stream.
    Nothing,
}

/// §6.6's table. Pure: no clock, no I/O, no ambient state.
#[must_use]
pub fn decide_control(state: &State, event: &Event) -> Decision {
    match event {
        // ROW 1 — a human's claim is granted UNCONDITIONALLY. Not "unless
        // halted": a halt is a stop on what the session may be driven to do,
        // and the human who can lift it is exactly the human claiming here.
        // Refusing this row under a halt would mean a fleet halt could lock a
        // human out of their own terminal.
        Event::Claim { by } => {
            if state.holder.as_deref() == Some(by.as_str()) {
                return Decision::Nothing;
            }
            Decision::Hold {
                holder: by.clone(),
                evidence: "claim",
                lost: state.holder.clone().filter(|h| h != PAUSED),
            }
        }
        // ROW 2 — an agent's request. A halt blocks it: an agent taking the
        // keyboard during a fleet halt is the one thing the halt exists to stop.
        Event::Request { by } => {
            if state.holder.as_deref() == Some(by.as_str()) {
                return Decision::Nothing;
            }
            if state.halted {
                return Decision::Refuse { reason: "hold" };
            }
            if state.free_for_request() {
                Decision::Hold {
                    holder: by.clone(),
                    evidence: "request",
                    lost: state.holder.clone().filter(|h| h != PAUSED),
                }
            } else {
                Decision::Pending { by: by.clone() }
            }
        }
        // ROW 3a — only the holder releases, and releasing what you do not hold
        // is a refusal rather than a no-op: a stale `release` from a principal
        // that lost the wheel a moment ago must not free the new holder.
        Event::Release { by } => {
            if state.holder.as_deref() == Some(by.as_str()) {
                Decision::Free {
                    evidence: "release",
                    lost: None,
                }
            } else {
                Decision::Refuse { reason: "holder" }
            }
        }
        // ROW 3b — a grant, by the holder or by any human. `to` must be a
        // principal the caller already validated.
        Event::Grant { by, to } => {
            let allowed = state.holder.as_deref() == Some(by.as_str()) || by.starts_with("h-");
            if !allowed {
                return Decision::Refuse { reason: "holder" };
            }
            if state.holder.as_deref() == Some(to.as_str()) {
                return Decision::Nothing;
            }
            Decision::Hold {
                holder: to.clone(),
                evidence: "grant",
                lost: state.holder.clone().filter(|h| h != PAUSED && h != to),
            }
        }
        // ROW 4 — the conservative pause. Something moved the session that the
        // bridge cannot attribute (`docs/RFC-operator-2026-08-15.md:297-301`:
        // repaint, resize, another client, non-echoed typing — screen state
        // detects the change and cannot attribute it). The remote holder loses
        // the wheel until it claims again. It fires only when somebody actually
        // holds the row and it is not already parked, so a quiet session and an
        // already-paused one publish nothing.
        Event::UnaccountedChange => match &state.holder {
            None => Decision::Nothing,
            Some(h) if h == PAUSED => Decision::Nothing,
            Some(h) => Decision::Hold {
                holder: PAUSED.to_string(),
                evidence: "unaccounted-change",
                lost: Some(h.clone()),
            },
        },
        // ROW 5 — a local socket driver took aterm's cooperative lease. Mirror
        // it so a fleet reader sees who is driving. It cannot displace a live
        // fabric holder, because aterm's own `lease acquire` would have been
        // refused `ERR lease held` while the bridge's mirror stands — so
        // observing one at all means the fabric row was already free.
        Event::LocalLease { holder } => {
            let mirrored = format!("owner-cli:{holder}");
            if state.holder.as_deref() == Some(mirrored.as_str()) {
                return Decision::Nothing;
            }
            match &state.holder {
                Some(h) if h != PAUSED && state.holder_live => {
                    Decision::Refuse { reason: "holder" }
                }
                _ => Decision::Hold {
                    holder: mirrored,
                    evidence: "lease",
                    lost: None,
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held_by(who: &str) -> State {
        State {
            holder: Some(who.to_string()),
            holder_live: true,
            halted: false,
        }
    }

    fn free() -> State {
        State {
            holder: None,
            holder_live: true,
            halted: false,
        }
    }

    /// ROW 1. A human's claim wins over anybody, including another human, and
    /// the displaced holder is named so it can be told it lost the wheel.
    #[test]
    fn a_humans_claim_is_granted_unconditionally() {
        for prior in [free(), held_by("s-worker"), held_by("h-other")] {
            let lost = prior.holder.clone();
            assert_eq!(
                decide_control(
                    &prior,
                    &Event::Claim {
                        by: "h-andrew".into()
                    }
                ),
                Decision::Hold {
                    holder: "h-andrew".into(),
                    evidence: "claim",
                    lost,
                }
            );
        }
        // A HALT DOES NOT BLOCK IT. The human who can lift the halt is the one
        // claiming; refusing here would lock a human out of their own terminal.
        let mut halted = held_by("s-worker");
        halted.halted = true;
        assert!(matches!(
            decide_control(
                &halted,
                &Event::Claim {
                    by: "h-andrew".into()
                }
            ),
            Decision::Hold { .. }
        ));
        // Re-claiming what you already hold publishes nothing.
        assert_eq!(
            decide_control(
                &held_by("h-andrew"),
                &Event::Claim {
                    by: "h-andrew".into()
                }
            ),
            Decision::Nothing
        );
    }

    /// ROW 2. An agent's request takes a free slot, waits behind a live holder,
    /// and is refused outright under a halt.
    #[test]
    fn an_agents_request_takes_a_free_slot_and_otherwise_waits() {
        assert_eq!(
            decide_control(&free(), &Event::Request { by: "s-a".into() }),
            Decision::Hold {
                holder: "s-a".into(),
                evidence: "request",
                lost: None,
            }
        );
        assert_eq!(
            decide_control(&held_by("h-andrew"), &Event::Request { by: "s-a".into() }),
            Decision::Pending { by: "s-a".into() }
        );
        // An EXPIRED holder — its session exited — is a free slot.
        let gone = State {
            holder: Some("s-dead".into()),
            holder_live: false,
            halted: false,
        };
        assert_eq!(
            decide_control(&gone, &Event::Request { by: "s-a".into() }),
            Decision::Hold {
                holder: "s-a".into(),
                evidence: "request",
                lost: Some("s-dead".into()),
            }
        );
        // A PAUSED row is takeable too, and nothing is "lost" — `human?` is not
        // a principal and there is no lane to tell.
        assert_eq!(
            decide_control(&held_by(PAUSED), &Event::Request { by: "s-a".into() }),
            Decision::Hold {
                holder: "s-a".into(),
                evidence: "request",
                lost: None,
            }
        );
        let mut halted = free();
        halted.halted = true;
        assert_eq!(
            decide_control(&halted, &Event::Request { by: "s-a".into() }),
            Decision::Refuse { reason: "hold" }
        );
    }

    /// ROW 3. Only the holder releases; a grant comes from the holder or from
    /// any human. A stale `release` from a principal that already lost the
    /// wheel must not free the NEW holder — that is the whole reason this is a
    /// refusal and not a no-op.
    #[test]
    fn release_and_grant_answer_to_the_holder_and_to_humans() {
        assert_eq!(
            decide_control(&held_by("s-a"), &Event::Release { by: "s-a".into() }),
            Decision::Free {
                evidence: "release",
                lost: None,
            }
        );
        assert_eq!(
            decide_control(&held_by("h-andrew"), &Event::Release { by: "s-a".into() }),
            Decision::Refuse { reason: "holder" }
        );
        assert_eq!(
            decide_control(
                &held_by("s-a"),
                &Event::Grant {
                    by: "s-a".into(),
                    to: "s-b".into()
                }
            ),
            Decision::Hold {
                holder: "s-b".into(),
                evidence: "grant",
                lost: Some("s-a".into()),
            }
        );
        // A human may grant even while somebody else holds it.
        assert_eq!(
            decide_control(
                &held_by("s-a"),
                &Event::Grant {
                    by: "h-andrew".into(),
                    to: "s-b".into()
                }
            ),
            Decision::Hold {
                holder: "s-b".into(),
                evidence: "grant",
                lost: Some("s-a".into()),
            }
        );
        // A THIRD PARTY MAY NOT. This is the row that would otherwise let any
        // lane-writer move the keyboard.
        assert_eq!(
            decide_control(
                &held_by("s-a"),
                &Event::Grant {
                    by: "s-c".into(),
                    to: "s-c".into()
                }
            ),
            Decision::Refuse { reason: "holder" }
        );
    }

    /// ROW 4. The conservative pause parks the row at `human?` — which no
    /// `<src>` segment can equal, so the session applies nothing from the bus
    /// until someone claims it again — and it fires ONCE, not on every sample.
    #[test]
    fn an_unaccounted_change_parks_the_row_once() {
        assert_eq!(
            decide_control(&held_by("s-a"), &Event::UnaccountedChange),
            Decision::Hold {
                holder: PAUSED.into(),
                evidence: "unaccounted-change",
                lost: Some("s-a".into()),
            }
        );
        assert_eq!(
            decide_control(&held_by(PAUSED), &Event::UnaccountedChange),
            Decision::Nothing
        );
        assert_eq!(
            decide_control(&free(), &Event::UnaccountedChange),
            Decision::Nothing
        );
        // `human?` IS NOT A PRINCIPAL, which is what makes the pause a refusal
        // rather than a handover to a name somebody could mint a cap for.
        assert!(!crate::subject::is_principal(PAUSED));
    }

    /// ROW 5. A local `lease acquire` is mirrored, once, and it never displaces
    /// a live fabric holder.
    #[test]
    fn a_local_lease_is_mirrored_and_never_displaces_a_live_holder() {
        assert_eq!(
            decide_control(
                &free(),
                &Event::LocalLease {
                    holder: "drv-1".into()
                }
            ),
            Decision::Hold {
                holder: "owner-cli:drv-1".into(),
                evidence: "lease",
                lost: None,
            }
        );
        assert_eq!(
            decide_control(
                &held_by("owner-cli:drv-1"),
                &Event::LocalLease {
                    holder: "drv-1".into()
                }
            ),
            Decision::Nothing,
            "the same lease seen on every sample must not be a record stream"
        );
        assert_eq!(
            decide_control(
                &held_by("h-andrew"),
                &Event::LocalLease {
                    holder: "drv-1".into()
                }
            ),
            Decision::Refuse { reason: "holder" }
        );
    }
}
