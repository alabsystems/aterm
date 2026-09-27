// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance of the SHIPPING authorization gate to
//! `aterm_spec::derive::authorize_soundness_model` (`AuthorizeSoundness`).
//!
//! The model states the trust core's central predicate as a four-guard
//! conjunction — the presented token is in the table (`token`), its row's
//! destination is the resolved target (`dst`), its row's op is the op the verb
//! needs (`op`), and its row's launch nonce is the target's current one
//! (`nonce`) — and proves `PermitImpliesAllGuards`: a permit (`decided = 2`)
//! implies every guard held. Its `Buggy = 1` member WAIVES the `dst` conjunct,
//! the confused-deputy escalation where a token minted for one session
//! authorizes a different one.
//!
//! This test is what makes that a property of the code rather than of the
//! description. It drives the REAL `decide_edge` over all sixteen guard
//! combinations, and `EdgeTable::authorize` — the connect-time form of the same
//! predicate — over the eight its inputs reach. `authorize` takes no op: it
//! RETURNS the op the token carries, and its shipping callers compare it to
//! nothing — the control server's connect-time auth discards it (per-request
//! `decide_edge` re-derives it) and `whoami` only reports it. So for that gate
//! the `op` guard is not applicable and is projected as held. Each call is
//! projected onto the model: the guards are
//! whether each conjunct holds for the grant the presentation claims (a fresh
//! token stands in for a forgery of it), and `decided` is the real verdict.
//! Every real `Present`/`Authorize` step must be a transition the committed
//! model admits (interpreter always, `ty trace validate` wherever installed,
//! with the two tiers required to agree).
//!
//! NEGATIVE CONTROL. The same real transitions are replayed against the
//! `Buggy = 1` model, which must REJECT the wrong-destination presentation the
//! real gate denies and the mutant permits — so a pass is never vacuous. That
//! replay judges transitions only: a gate that permits the wrong destination
//! is then ADMITTED by the mutant, and the control fails. Delete
//! `e.dst == *dst` from `decide_edge` and both tests fail: the conformance on
//! the permit the committed model rejects, the control on the mutant it now
//! matches.

use std::collections::BTreeMap;

use aterm_session::{EdgeDecision, EdgeTable, EdgeToken, LaunchNonce, Op, SessionId, decide_edge};
use aterm_spec::derive::{Model, authorize_soundness_model};
use aterm_spec::verify::validate_transition_tiered;

type State = BTreeMap<&'static str, i64>;

const GUARDS: [&str; 4] = ["token", "dst", "op", "nonce"];

/// One presentation against a table holding exactly one grant, with each
/// conjunct of the claimed grant held or broken as `holds` says.
struct Presentation {
    table: EdgeTable,
    token: EdgeToken,
    dst: SessionId,
    op: Op,
    nonce: LaunchNonce,
}

fn presentation(holds: [bool; 4]) -> Presentation {
    let [token_known, dst_matches, op_matches, nonce_matches] = holds;
    let src = SessionId::new("s-parent");
    let target = SessionId::new("s-target");
    let nonce = LaunchNonce::from_bytes([5u8; 16]);
    let mut table = EdgeTable::new();
    // The one grant every presentation claims: write the target's input.
    let granted = table.grant(src, target.clone(), Op::WriteInput, nonce);
    Presentation {
        table,
        // A token the table never recorded is a forgery of the same claim.
        token: if token_known {
            granted
        } else {
            EdgeToken::generate()
        },
        dst: if dst_matches {
            target
        } else {
            SessionId::new("s-other")
        },
        op: if op_matches {
            Op::WriteInput
        } else {
            Op::ReadScreen
        },
        nonce: if nonce_matches {
            nonce
        } else {
            LaunchNonce::from_bytes([6u8; 16])
        },
    }
}

/// The model state after `Present` chose these guards (undecided).
fn presented(holds: [bool; 4]) -> State {
    let mut s: State = GUARDS
        .iter()
        .zip(holds)
        .map(|(g, h)| (*g, i64::from(h)))
        .collect();
    s.insert("decided", 0);
    s
}

/// The state after the real gate decided: `2` permit, `1` deny.
fn decided(holds: [bool; 4], permit: bool) -> State {
    let mut s = presented(holds);
    s.insert("decided", if permit { 2 } else { 1 });
    s
}

fn all_presentations() -> Vec<[bool; 4]> {
    (0u8..16)
        .map(|bits| std::array::from_fn(|i| bits & (1 << i) != 0))
        .collect()
}

/// A real verdict source.
struct Gate {
    /// Named for the failure message.
    name: &'static str,
    /// Whether the gate decides on an op at all (see the module doc); where it
    /// does not, only presentations whose `op` guard holds are projected.
    decides_op: bool,
    permits: fn(&Presentation) -> bool,
}

impl Gate {
    fn presentations(&self) -> Vec<[bool; 4]> {
        all_presentations()
            .into_iter()
            .filter(|[_, _, op, _]| self.decides_op || *op)
            .collect()
    }
}

/// The two shipping forms of the predicate.
const GATES: [Gate; 2] = [
    Gate {
        name: "decide_edge",
        decides_op: true,
        permits: |p| {
            decide_edge(&p.table, &p.token, &p.dst, p.op, &p.nonce) == EdgeDecision::Permit
        },
    },
    Gate {
        name: "EdgeTable::authorize",
        decides_op: false,
        permits: |p| p.table.authorize(&p.token, &p.dst, &p.nonce).is_some(),
    },
];

/// Whether `model` admits the real two-step trace Init -Present-> presented
/// -Authorize-> decided for every presentation through `gate`, naming the first
/// it rejects; on success, every decided state. Transitions only: the invariant
/// is the conforming test's to check, so a mutant replay parts from the real
/// gate on a TRANSITION or not at all.
fn replay(model: &Model, gate: &Gate) -> Result<Vec<State>, String> {
    let label = format!("{}({})", model.name, gate.name);
    let init = model.init_state();
    let mut decided_states = Vec::new();
    for holds in gate.presentations() {
        let permit = (gate.permits)(&presentation(holds));
        let before = presented(holds);
        let after = decided(holds, permit);
        let (present_ok, why) =
            validate_transition_tiered(model, &[], &init, &before, Some("Present"), &label);
        if !present_ok {
            return Err(format!("{holds:?}: Present rejected — {why}"));
        }
        let (decide_ok, why) =
            validate_transition_tiered(model, &[], &before, &after, Some("Authorize"), &label);
        if !decide_ok {
            return Err(format!(
                "{holds:?}: {} {} but the model does not admit it — {why}",
                gate.name,
                if permit { "PERMITTED" } else { "denied" }
            ));
        }
        decided_states.push(after);
    }
    Ok(decided_states)
}

#[test]
fn the_real_authorization_gate_conforms_to_authorize_soundness() {
    let model = authorize_soundness_model();
    for gate in &GATES {
        let decided_states = replay(&model, gate).unwrap_or_else(|why| {
            panic!(
                "{} does not conform to AuthorizeSoundness: {why}",
                gate.name
            )
        });
        for after in &decided_states {
            assert!(
                model.check_invariant("PermitImpliesAllGuards", after),
                "{}: PermitImpliesAllGuards refuted by {after:?}",
                gate.name
            );
        }
        // Exactly one permit: the presentation where every guard holds. A gate
        // that denies everything would also satisfy the invariant, so the
        // permit is pinned as well.
        let permits: Vec<[bool; 4]> = gate
            .presentations()
            .into_iter()
            .filter(|h| (gate.permits)(&presentation(*h)))
            .collect();
        assert_eq!(
            permits,
            vec![[true; 4]],
            "{}: only the exact grant may permit",
            gate.name
        );
    }
}

#[test]
fn the_dropped_destination_mutant_is_told_apart_from_the_real_gate() {
    let buggy = aterm_spec::interp::with_buggy(&authorize_soundness_model(), 1);
    for gate in &GATES {
        let rejected = replay(&buggy, gate).expect_err(
            "the Buggy=1 model (dst conjunct waived) admitted every real verdict, so \
             the real gate permits the wrong destination the way a confused deputy does",
        );
        assert!(
            rejected.starts_with("[true, false, true, true]: ") && rejected.contains(" denied "),
            "{}: the mutant must part from the real gate on its DENIAL of the \
             wrong-destination presentation first: {rejected}",
            gate.name
        );
    }
}
