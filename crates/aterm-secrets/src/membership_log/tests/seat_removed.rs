// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one removal predicate ([`Membership::seat_removed`], P4's binding 4) over generated
//! memberships: it answers what its four verdicts, re-read plainly here, answer; each verdict
//! alone takes a seat out of some membership; and with its actor disjunct dropped (the negative
//! control) it disagrees with them, and lets in the superseded seat of a Mac rebuilt with its node
//! key kept (one-mesh A15). The kin verdict is re-read as `aterm-db-core`'s island spells it
//! (`kin` over the seats, `peer_check_island.rs` section 8, where `kin_any_size` proves the scan
//! for lists of any length), so this test is what holds [`Membership::kin_removed`] to it.

use super::super::eval::SEAT_WITHOUT_ACTOR;
use super::super::reach::tests::Rng;
use super::*;

/// How many memberships the property test generates; each is judged for every pooled binding.
const CASES: usize = 1_500;
/// The generator's seed: the same seed, the same memberships, in the test and its control.
const SEED: u64 = 0x5ea7_0b17_d4c0_0001;

/// The bindings the generator deals from: three recipients, three writer keys and two
/// incarnations, so a seat shares its actor (writer key and incarnation), its recipient or its
/// writer key with others.
fn pool() -> Vec<Binding> {
    let mut bindings = Vec::new();
    for recipient in 0..3u8 {
        for writer in 0..3u8 {
            for incarnation in 0..2u8 {
                bindings.push(binding(recipient, writer, incarnation));
            }
        }
    }
    bindings
}

/// The binding of recipient `recipient`, writer key `writer` and incarnation `incarnation`.
fn binding(recipient: u8, writer: u8, incarnation: u8) -> Binding {
    Request::create(
        [2; 16],
        [3; 32],
        [1 + recipient + 3 * writer + 9 * incarnation; 16],
        [1 + incarnation; 16],
        &age::Identity::from_secret_bytes([0x40 + recipient; 32]),
        &[0x60 + writer; 32],
    )
    .unwrap()
    .binding()
    .clone()
}

/// The node keys the generator binds: the zero key (which binds nothing), the pool's writer keys
/// (so a writer key can read removed) and three keys of no binding.
fn node_keys(bindings: &[Binding]) -> Vec<[u8; 32]> {
    let mut keys: BTreeSet<[u8; 32]> = bindings.iter().map(|b| *b.writer()).collect();
    keys.extend([[0; 32], [0x41; 32], [0x42; 32], [0x43; 32]]);
    keys.into_iter().collect()
}

/// What the predicate reads of a membership, as the generator deals it.
#[derive(Clone)]
struct Parts {
    members: BTreeMap<MemberId, Member>,
    removed: BTreeMap<MemberId, Removal>,
    removed_recipients: BTreeSet<Recipient>,
    unproved: Vec<Founder>,
    nodes: BTreeMap<[u8; 32], NodeStanding>,
}

impl Parts {
    fn membership(&self) -> Membership {
        let parts = self.clone();
        Membership::from_parts(
            parts.members,
            parts.removed,
            parts.removed_recipients,
            parts.unproved,
            parts.nodes,
        )
    }
}

fn member(binding: &Binding, node_key: Option<[u8; 32]>) -> Member {
    Member {
        binding: binding.clone(),
        node_key,
        name: String::new(),
        label: String::new(),
        admitted_by: None,
        entry: [0; 32],
    }
}

fn removal() -> Removal {
    Removal {
        by: BTreeSet::new(),
        quorum: false,
    }
}

/// One membership from `rng`. Seats and removals are keyed by actor, and each of the pool's six
/// actors has three bindings, each drawn once: an actor is seated with chance 1 - (2/3)^3 (about
/// four of the six, so about a quarter of the pool; the last binding drawn holds the seat, most
/// with a node key), and out with chance 1 - (4/5)^3 (about half the actors). A sixth of its
/// recipients are listed as removed, up to two founders wait, and each node key is a removed
/// Mac's, a member's, a retired member's, contested, or unknown.
fn generate(rng: &mut Rng, bindings: &[Binding], keys: &[[u8; 32]]) -> Parts {
    let mut members = BTreeMap::new();
    for b in bindings {
        if rng.below(3) == 0 {
            let node_key = (rng.below(4) != 0).then(|| keys[rng.below(keys.len())]);
            members.insert(b.actor(), member(b, node_key));
        }
    }
    let mut removed = BTreeMap::new();
    for b in bindings {
        if rng.below(5) == 0 {
            removed.insert(b.actor(), removal());
        }
    }
    let recipients: Vec<Recipient> = bindings
        .iter()
        .map(Binding::recipient)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let removed_recipients = recipients
        .iter()
        .filter(|_| rng.below(6) == 0)
        .copied()
        .collect();
    let mut unproved = Vec::new();
    for _ in 0..rng.below(3) {
        unproved.push(Founder {
            recipient: recipients[rng.below(recipients.len())],
            binding: None,
            node_key: (rng.below(3) != 0).then(|| keys[rng.below(keys.len())]),
            name: String::new(),
            label: String::new(),
        });
    }
    let mut nodes = BTreeMap::new();
    for key in keys {
        let actor = bindings[rng.below(bindings.len())].actor();
        let standing = match rng.below(5) {
            0 => NodeStanding::Removed(actor),
            1 => NodeStanding::Member(actor),
            2 => NodeStanding::Retired(actor),
            3 => NodeStanding::Contested,
            _ => continue,
        };
        nodes.insert(*key, standing);
    }
    Parts {
        members,
        removed,
        removed_recipients,
        unproved,
        nodes,
    }
}

/// The island's `kinSeat`: the seat of `actor` shares `binding`'s actor or recipient, and its
/// node key (the zero key when it has none) is not the zero key and a removed Mac's.
fn kin_seat(parts: &Parts, binding: &Binding, actor: &MemberId, seat: &Member) -> bool {
    let node = seat.node_key.unwrap_or([0; 32]);
    (*actor == binding.actor() || seat.binding.recipient() == binding.recipient())
        && (node != [0; 32] && matches!(parts.nodes.get(&node), Some(NodeStanding::Removed(_))))
}

/// The island's `kin`, by its recursion: no seats are no verdict, and `s :: rest` is `s`'s
/// `kinSeat` or `kin` of the rest.
fn kin(parts: &Parts, binding: &Binding, seats: &[(&MemberId, &Member)]) -> bool {
    match seats {
        [] => false,
        [(actor, seat), rest @ ..] => {
            kin_seat(parts, binding, actor, seat) || kin(parts, binding, rest)
        }
    }
}

/// The four verdicts on `binding`, read plainly from `parts`: its actor is out; its recipient is
/// listed as removed, or held, and every holder holds a node key (a member, or its writer key) a
/// removed Mac bound; its writer key is a removed Mac's; a member that shares its actor or
/// recipient holds such a node key ([`kin`]). The zero key binds nothing.
fn verdicts(parts: &Parts, binding: &Binding) -> [bool; 4] {
    let removed_key = |key: &[u8; 32]| {
        *key != [0; 32] && matches!(parts.nodes.get(key), Some(NodeStanding::Removed(_)))
    };
    let removed_writer =
        |writer: &[u8; 32]| matches!(parts.nodes.get(writer), Some(NodeStanding::Removed(_)));
    let recipient = binding.recipient();
    let mut holders = 0;
    let mut every_holder_out = true;
    for m in parts.members.values() {
        if m.binding.recipient() == recipient {
            holders += 1;
            if !(m.node_key.is_some_and(|k| removed_key(&k)) || removed_writer(m.binding.writer()))
            {
                every_holder_out = false;
            }
        }
    }
    for f in &parts.unproved {
        if f.recipient == recipient {
            holders += 1;
            if !f.node_key.is_some_and(|k| removed_key(&k)) {
                every_holder_out = false;
            }
        }
    }
    let seats: Vec<(&MemberId, &Member)> = parts.members.iter().collect();
    [
        parts.removed.contains_key(&binding.actor()),
        parts.removed_recipients.contains(&recipient) || (holders > 0 && every_holder_out),
        removed_writer(binding.writer()),
        kin(parts, binding, &seats),
    ]
}

/// Run `run` with the predicate's actor disjunct dropped, and restore it however `run` ends.
fn without_actor<T>(run: impl FnOnce() -> T) -> T {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            SEAT_WITHOUT_ACTOR.set(false);
        }
    }
    let _restore = Restore;
    SEAT_WITHOUT_ACTOR.set(true);
    run()
}

/// How many (membership, binding) pairs of the generated corpus `seat_removed` judges otherwise
/// than its four verdicts do.
fn disagreements(bindings: &[Binding], keys: &[[u8; 32]]) -> usize {
    let mut rng = Rng(SEED);
    let mut wrong = 0;
    for _ in 0..CASES {
        let parts = generate(&mut rng, bindings, keys);
        let membership = parts.membership();
        for b in bindings {
            if membership.seat_removed(b) != verdicts(&parts, b).contains(&true) {
                wrong += 1;
            }
        }
    }
    wrong
}

/// A Mac rebuilt with its node key kept, moved by a person's Allow (one-mesh A15): its old seat's
/// actor is out, and its recipient and node key are now the new seat's, a member in good standing.
/// The old seat's binding is the second value.
fn superseded_seat() -> (Membership, Binding) {
    let old = binding(0, 0, 0);
    let new = binding(0, 1, 1);
    let node_key = [0x41; 32];
    let membership = Membership::from_parts(
        BTreeMap::from([(new.actor(), member(&new, Some(node_key)))]),
        BTreeMap::from([(old.actor(), removal())]),
        BTreeSet::new(),
        Vec::new(),
        BTreeMap::from([(node_key, NodeStanding::Member(new.actor()))]),
    );
    (membership, old)
}

#[test]
fn the_removal_predicate_answers_its_four_verdicts_on_generated_memberships() {
    let bindings = pool();
    let keys = node_keys(&bindings);
    // The loop the negative control runs under the mutant finds nothing on the real predicate.
    assert_eq!(disagreements(&bindings, &keys), 0);
    let mut rng = Rng(SEED);
    let mut alone = [0usize; 4];
    let mut kept = 0usize;
    for case in 0..CASES {
        let parts = generate(&mut rng, &bindings, &keys);
        let membership = parts.membership();
        for b in &bindings {
            let verdicts = verdicts(&parts, b);
            assert_eq!(
                membership.seat_removed(b),
                verdicts.contains(&true),
                "case {case}: the predicate disagrees with its verdicts {verdicts:?}"
            );
            assert_eq!(
                membership.recipient_removed(&b.recipient()),
                verdicts[1],
                "case {case}: the recipient verdict"
            );
            assert_eq!(
                membership.kin_removed(b),
                verdicts[3],
                "case {case}: the kin verdict"
            );
            match verdicts.iter().filter(|&&v| v).count() {
                0 => kept += 1,
                1 => alone[verdicts.iter().position(|&v| v).unwrap()] += 1,
                _ => {}
            }
        }
    }
    // Non-vacuity: every verdict alone took some seat out, and some seat stayed in.
    assert!(
        alone.iter().all(|&n| n > 0) && kept > 0,
        "the corpus must exercise each verdict alone: {alone:?}, {kept} kept"
    );
}

/// NEGATIVE CONTROL: the predicate with its actor disjunct dropped disagrees with the four
/// verdicts on the generated corpus, and lets in the superseded seat the real predicate takes out
/// by its actor alone.
#[test]
fn without_its_actor_disjunct_the_predicate_lets_a_superseded_seat_in() {
    let bindings = pool();
    let keys = node_keys(&bindings);
    assert!(
        without_actor(|| disagreements(&bindings, &keys)) > 0,
        "the property test must see a predicate that lost its actor disjunct"
    );
    let (membership, old) = superseded_seat();
    assert!(
        membership.seat_removed(&old),
        "the superseded seat is out by its actor"
    );
    assert!(
        !membership.recipient_removed(&old.recipient())
            && !membership.kin_removed(&old)
            && !matches!(membership.node(old.writer()), NodeStanding::Removed(_)),
        "its actor is the one verdict that takes it out"
    );
    assert!(
        !without_actor(|| membership.seat_removed(&old)),
        "without the actor disjunct the superseded seat is let in"
    );
}
