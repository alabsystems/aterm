// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::collections::HashMap;
use std::sync::Mutex;

/// One hold: the slice's start offset in its batch, the token the
/// timeline held then, and the engine fence's current mark as the
/// slice's bytes were parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Hold {
    pub(crate) off: usize,
    pub(crate) bytes: Vec<u8>,
    pub(crate) token: Option<u64>,
    pub(crate) fence: Option<u64>,
}

/// The controls' mutants a watched session runs.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Knobs {
    /// Publish the holder image after the hold of its slice, not before.
    pub(crate) publish_after_hold: bool,
    /// The byte arm without its cut after an OSC 133 C.
    pub(crate) no_c_cut: bool,
    /// The parse stage drops the process watch's exits, so no stopped drawer's mark retires.
    #[cfg_attr(
        not(unix),
        expect(dead_code, reason = "only the unix gather loop reads it")
    )]
    pub(crate) no_retire: bool,
}

struct Watch {
    knobs: Knobs,
    holds: Vec<Hold>,
}

static WATCHED: Mutex<Option<HashMap<u64, Watch>>> = Mutex::new(None);

/// Record `session`'s holds from now on, running `knobs`.
#[cfg_attr(
    not(unix),
    expect(dead_code, reason = "only the unix reader tests watch a session")
)]
pub(crate) fn watch(session: u64, knobs: Knobs) {
    WATCHED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(
            session,
            Watch {
                knobs,
                holds: Vec::new(),
            },
        );
}

fn knobs(session: u64) -> Knobs {
    WATCHED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .and_then(|m| m.get(&session))
        .map(|w| w.knobs)
        .unwrap_or_default()
}

/// Whether `session` runs the no-C-cut mutant.
pub(crate) fn no_c_cut(session: u64) -> bool {
    knobs(session).no_c_cut
}

/// Whether `session` runs the no-retire mutant.
#[cfg(unix)]
pub(crate) fn no_retire(session: u64) -> bool {
    knobs(session).no_retire
}

/// Stop watching `session`; its holds.
#[cfg_attr(
    not(unix),
    expect(dead_code, reason = "only the unix reader tests watch a session")
)]
pub(crate) fn unwatch(session: u64) -> Vec<Hold> {
    WATCHED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_mut()
        .and_then(|m| m.remove(&session))
        .map(|w| w.holds)
        .unwrap_or_default()
}

/// Whether `session` runs the publish-after-hold mutant.
pub(crate) fn publish_after_hold(session: u64) -> bool {
    knobs(session).publish_after_hold
}

/// How many holds `session` has recorded so far: a test's `post` stamps
/// each wake with it, to order the wake against the holds.
#[cfg_attr(
    not(unix),
    expect(dead_code, reason = "only the unix reader tests watch a session")
)]
pub(crate) fn holds_so_far(session: u64) -> usize {
    WATCHED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .and_then(|m| m.get(&session))
        .map_or(0, |w| w.holds.len())
}

type AfterRead = Box<dyn FnOnce() + Send>;

static AFTER_READ: Mutex<Option<HashMap<u64, AfterRead>>> = Mutex::new(None);

/// Run `act` in `session`'s gather right after its next read.
#[cfg_attr(
    not(unix),
    expect(dead_code, reason = "only the unix reader tests watch a session")
)]
pub(crate) fn on_next_read(session: u64, act: impl FnOnce() + Send + 'static) {
    AFTER_READ
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(session, Box::new(act));
}

/// The gather read a batch's first bytes: run what a test armed.
#[cfg_attr(
    not(unix),
    expect(dead_code, reason = "only the unix gather reads a master")
)]
pub(crate) fn after_read(session: u64) {
    let act = AFTER_READ
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_mut()
        .and_then(|m| m.remove(&session));
    if let Some(act) = act {
        act();
    }
}

/// Inside a hold: record what `token` reads and the engine fence's mark,
/// for a watched session.
pub(crate) fn at_hold(
    session: u64,
    off: usize,
    bytes: &[u8],
    fence: Option<u64>,
    token: impl FnOnce() -> Option<u64>,
) {
    let mut watched = WATCHED.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(w) = watched.as_mut().and_then(|m| m.get_mut(&session)) {
        w.holds.push(Hold {
            off,
            bytes: bytes.to_vec(),
            token: token(),
            fence,
        });
    }
}
