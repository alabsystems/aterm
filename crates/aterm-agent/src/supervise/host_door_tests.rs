// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// THE HOST'S DOOR, GENERATED (`host_door.rs`): a trait of the test's own,
// declared with the same macro, with a method the window's trait does not
// have — the PLANTED method — shows the taped host forwards it and tapes it,
// and the replay host answers it from the tape alone, with nothing written
// for it anywhere but its declaration; and a method marked `act` there is an
// act by that mark alone.

use std::sync::{Arc, Mutex};

use astream_effects::EffectRecord;

use crate::supervise::fx::{ClockSource, Fx};
use crate::supervise::tape::doors;
use crate::supervise::tape::header::VERBATIM;
use crate::supervise::tape::host::{ReplayHost, TapedHost};
use crate::supervise::tape::replay::ReplayCore;
use crate::supervise::tape::testing::{MemoryTape, TAPE_ID};

mod planted {
    crate::supervise::run::host_door::idle_host_trait! {
        /// A host's trait of the test's own.
        pub trait Planted: Send + Sync + std::fmt::Debug {
            /// A query with no default body.
            fn wants(&self) -> bool;
            /// THE PLANTED METHOD: a query with a default body, which the
            /// window's trait does not have and nobody wrote a forward for.
            fn planted(&self, n: u32, why: &str) -> Option<String> {
                let _ = (n, why);
                None
            }
            /// A default that asks another method of its own host.
            fn first(&self) -> bool {
                self.second()
            }
            /// The method [`Self::first`]'s default asks.
            fn second(&self) -> bool {
                false
            }
            /// A notification.
            fn told(&self, text: &str) {
                let _ = text;
            }
            /// AN ACT: a query the list marks `act`, and nothing else does.
            act fn acted(&self) -> bool {
                false
            }
        }
    }
}

use planted::Planted;

/// The host behind the taped one: it answers the planted method,
/// [`Planted::second`] and the act otherwise than their defaults, and keeps
/// every call.
#[derive(Debug, Default)]
struct Inner {
    calls: Mutex<Vec<String>>,
}

impl Inner {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("calls").clone()
    }

    fn call(&self, what: String) {
        self.calls.lock().expect("calls").push(what);
    }
}

impl Planted for Inner {
    fn wants(&self) -> bool {
        self.call("wants".to_string());
        true
    }

    fn planted(&self, n: u32, why: &str) -> Option<String> {
        self.call(format!("planted {n} {why}"));
        Some(format!("{why} {n}"))
    }

    fn second(&self) -> bool {
        self.call("second".to_string());
        true
    }

    fn told(&self, text: &str) {
        self.call(format!("told {text}"));
    }

    fn acted(&self) -> bool {
        self.call("acted".to_string());
        true
    }
}

/// A door's name on the tape.
fn door(name: &str) -> String {
    format!("{}{name}", doors::HOST)
}

/// A method added to a host's trait is TAPED BY CONSTRUCTION: the taped host
/// forwards the planted method to the host behind it — its override, not the
/// default — and tapes it at `host.planted`, its arguments by name; a default
/// that asks another method asks it of the host behind the door, the tape
/// holding the one call the loop made; a notification is said and told; and
/// the act closes the tape's frame after it, as no other call does. The
/// replay host then answers every call from the tape alone — the host
/// behind's overrides, which no default gives — and a call asked otherwise
/// than the recorded one is the replay's divergence.
#[test]
fn a_method_the_trait_grows_is_taped_by_construction() {
    let tape = MemoryTape::over(&Fx::over(ClockSource::Scripted(Arc::default())));
    let inner = Arc::new(Inner::default());
    let host = TapedHost::new(Arc::clone(&inner) as Arc<dyn Planted>, tape.fx.clone());
    let frames = || tape.sink.taken().writes.len();
    let opened = frames();
    assert!(Planted::wants(&host));
    assert_eq!(
        Planted::planted(&host, 7, "seven"),
        Some("seven 7".to_string())
    );
    assert!(Planted::first(&host), "the host's own second answered");
    Planted::told(&host, "hello");
    assert_eq!(frames(), opened, "no call but an act closes the frame");
    assert!(Planted::acted(&host));
    assert_eq!(frames(), opened + 1, "the act closed the frame after it");
    assert_eq!(
        inner.calls(),
        ["wants", "planted 7 seven", "second", "told hello", "acted"],
        "every call forwarded, the default's own ask below the door"
    );
    let read = tape.finish("done");
    let records: Vec<EffectRecord> = read
        .items
        .iter()
        .filter_map(|i| i.record().cloned())
        .collect();
    let crossed: Vec<(String, String, String)> = records
        .iter()
        .map(|r| match r {
            EffectRecord::Exchange {
                door,
                request,
                reply,
            } => (
                door.clone(),
                String::from_utf8_lossy(request).into_owned(),
                String::from_utf8_lossy(reply).into_owned(),
            ),
            EffectRecord::Emit { door, bytes } => (
                door.clone(),
                String::from_utf8_lossy(bytes).into_owned(),
                "(told)".to_string(),
            ),
            other => panic!("only the host's door is crossed: {other:?}"),
        })
        .collect();
    assert_eq!(
        crossed,
        [
            (door("wants"), String::new(), "1".to_string()),
            (
                door("planted"),
                "n=7\nwhy=\"seven\"".to_string(),
                "\"seven 7\"".to_string()
            ),
            (door("first"), String::new(), "1".to_string()),
            (
                door("told"),
                "text=\"hello\"".to_string(),
                "(told)".to_string()
            ),
            (door("acted"), String::new(), "1".to_string()),
        ],
        "one record per call the loop made, none for `second`"
    );

    // The replay host holds no host (its one field is the replay's door), so
    // what it answers can only be the tape's: the overrides above, where a
    // default would answer `None` and `false`.
    let core = ReplayCore::start(records.clone(), VERBATIM, VERBATIM, TAPE_ID, None);
    let replay = ReplayHost::new(Fx::replay(Arc::clone(&core)));
    assert!(Planted::wants(&replay));
    assert_eq!(
        Planted::planted(&replay, 7, "seven"),
        Some("seven 7".to_string())
    );
    assert!(Planted::first(&replay));
    Planted::told(&replay, "hello");
    assert!(Planted::acted(&replay));
    assert_eq!(core.left(), 0, "every call answered from the tape");

    // NEGATIVE CONTROL: the planted method asked otherwise diverges there.
    let core = ReplayCore::start(records, VERBATIM, VERBATIM, TAPE_ID, None);
    let replay = ReplayHost::new(Fx::replay(Arc::clone(&core)));
    assert!(Planted::wants(&replay));
    let asked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Planted::planted(&replay, 8, "seven")
    }));
    assert!(asked.is_err(), "the replay halts at the divergence");
    let d = core.diverged().expect("latched");
    assert_eq!(d.door.as_deref(), Some(door("planted").as_str()), "{d:?}");
}

/// AN ACT IS MARKED WHERE IT IS DECLARED: the macro lists the marked method's
/// door, and no other, among the acts (`HOST_ACTS`), its methods in their
/// order beside them; and the replay's question — is this door an act? — is
/// read off the window's own trait's list (`doors::is_host_act`), so a method
/// marked there is classed with no list kept beside the trait.
#[test]
fn an_act_is_marked_where_it_is_declared() {
    assert_eq!(planted::HOST_ACTS, ["host.acted"]);
    assert_eq!(
        planted::HOST_METHODS,
        ["wants", "planted", "first", "second", "told", "acted"]
    );
    for act in super::HOST_ACTS {
        assert!(doors::is_host_act(act), "{act}");
        let name = act.strip_prefix(doors::HOST).expect("a host door");
        assert!(super::HOST_METHODS.contains(&name), "{act}");
    }
    assert!(!super::HOST_ACTS.is_empty(), "the window's host has acts");
    assert!(!doors::is_host_act("host.wants"), "a query that is no act");
}
