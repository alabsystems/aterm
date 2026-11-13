// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The desk door: whether a control client may take the person's screen or keyboard focus.
//!
//! Owner ruling 2026-11-09: nothing takes the person's screen or focus unless they asked in that
//! moment, and a program in a tab they are not looking at (an agent's tool call, a script left
//! running) did not ask for them. A control verb whose effect lands on the desk (it brings a
//! window or another tab forward, opens a page or a panel over what is shown, or brings System
//! Settings up) asks here first, on its own lane, before it posts its act ([`admit`]):
//!
//! * a client whose process runs in a tab ([`Standing::InFront`], [`Standing::Behind`]) is
//!   admitted only while that tab is the one a focused aterm window shows, the rule the password
//!   sheet of `aterm ctl sudo` holds (`App::present_askpass_sheet`): what comes up then comes up
//!   over the tab that asked for it, in front of the person reading it;
//! * a client in no tab of this aterm ([`Standing::Outside`]: the lab's driver, a person's
//!   script in another terminal, a launch agent) is admitted: aterm cannot place it, and the
//!   ruling is about what its own tabs' programs do;
//! * aterm's own lanes ([`Asker::Own`]: the in-process harness, the fabric bridge, another
//!   Mac's drive) keep the rules of their own (a drive's worker opens `raise=false`).
//!
//! A refusal is one line the client reads ([`refusal`]) and one in aterm.log; nothing is queued
//! and nothing asks again by itself. `spawn` is not refused: its tab opens behind what the
//! window shows, as `raise=false` asks for (`control_media::cmd_spawn`).

use crate::App;
use crate::Wake;
use crate::control::Scope;
use crate::control::control_media::call_main;
use aterm_window::event_loop::EventLoopProxy;

/// What a control verb would do on the desk, in the words its refusal says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Effect {
    /// `raise <sid>`: bring a window and a tab forward.
    Raise,
    /// `spawn` with no aim, or `raise=true`: bring the new tab and its window forward.
    SpawnInFront,
    /// `open …`: bring a page, a menu or a panel up over what the window shows.
    Open,
    /// `permissions walk now`, `devtools hand-over`: bring System Settings and aterm's helper
    /// panel up.
    SystemSettings,
}

impl Effect {
    const fn words(self) -> &'static str {
        match self {
            Self::Raise => "`raise` brings a window and its tab forward",
            Self::SpawnInFront => "a tab spawned in front takes the window",
            Self::Open => "`open` brings a page up over what the window shows",
            Self::SystemSettings => "this brings System Settings up",
        }
    }
}

/// Who sent the verb, read on the control lane that serves it ([`Asker::serving`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Asker {
    /// One of aterm's own lanes: the in-process harness, the fabric bridge, another Mac's drive.
    Own,
    /// A client that presented this instance's token: its connection's peer pid (`None` where
    /// it cannot be read).
    Client(Option<i32>),
}

impl Asker {
    /// The asker of the connection this control thread serves, under `scope`.
    pub(crate) fn serving(scope: Scope) -> Self {
        let own_lane = match scope {
            Scope::Owner => {
                crate::fabric::serving_driver().is_some()
                    || crate::control::serving_principal() == crate::control::Principal::Harness
            }
            Scope::Edge(_) => false,
            #[cfg(any(unix, test))]
            Scope::Bridge | Scope::Fleet { .. } => true,
        };
        if own_lane {
            Self::Own
        } else {
            Self::Client(crate::control::serving_peer_pid().and_then(|pid| i32::try_from(pid).ok()))
        }
    }
}

/// Where an asker stands to the person at this Mac.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Standing {
    /// Its process runs in no tab of this aterm.
    Outside,
    /// Its tab's session is what a focused aterm window shows.
    InFront(u64),
    /// Its tab's session is behind another tab, in a window without keyboard focus, or aterm is
    /// behind another app.
    Behind(u64),
}

/// The standing of a client whose process runs in `session`'s tab (`None`: in none), where
/// `in_front` says whether a focused window shows that session. Pure.
#[must_use]
pub(crate) fn standing(session: Option<u64>, in_front: impl FnOnce(u64) -> bool) -> Standing {
    match session {
        None => Standing::Outside,
        Some(session) if in_front(session) => Standing::InFront(session),
        Some(session) => Standing::Behind(session),
    }
}

/// The line a refused client reads: what the verb would have done, and the two ways on.
#[must_use]
pub(crate) fn refusal(effect: Effect) -> String {
    format!(
        "{} behind: {}, and the tab that asked is not the one in front of the person; ask from \
         the tab they are looking at, or leave it to them\n",
        aterm_types::macos_setup::DESK_REFUSED,
        effect.words()
    )
}

/// Ask the door for the verb this control thread serves under `scope`: `Ok` when its effect may
/// land on the desk, else the refusal line. An asker of aterm's own asks nothing; a main thread
/// that cannot answer admits, and the verb's own hop says why it could not run.
///
/// # Errors
///
/// The asker's tab is not the one in front ([`refusal`]).
pub(crate) fn admit(
    proxy: &EventLoopProxy<Wake>,
    scope: Scope,
    effect: Effect,
) -> Result<(), String> {
    let asker = Asker::serving(scope);
    if asker == Asker::Own {
        return Ok(());
    }
    call_main(proxy, |reply| Wake::DeskAsked {
        asker,
        effect,
        reply,
    })
    .unwrap_or(Ok(()))
}

impl App {
    /// Where `asker` stands now ([`standing`]): its process's tab by the walk up its parents
    /// that attributes a password request (`consent_observer::session_of_pid`), and that tab in
    /// front when a window with keyboard focus shows its session.
    pub(crate) fn desk_standing(&self, asker: Asker) -> Standing {
        self.desk_standing_with(
            asker,
            &crate::session_status::program::parent_of,
            &crate::consent_observer::live_pgid_of,
        )
    }

    /// [`Self::desk_standing`] with the process table injected.
    pub(crate) fn desk_standing_with(
        &self,
        asker: Asker,
        parent_of: &dyn Fn(i32) -> Option<i32>,
        pgid_of: &dyn Fn(i32) -> Option<i32>,
    ) -> Standing {
        let Asker::Client(Some(pid)) = asker else {
            return Standing::Outside;
        };
        let sessions: Vec<(u64, i32)> = self.pool.iter().map(|s| (s.id, s.pid)).collect();
        let session = crate::consent_observer::session_of_pid(pid, &sessions, parent_of, pgid_of);
        standing(session, |session| {
            self.windows.iter().any(|(wid, ws)| {
                crate::os_window_focused(ws) && self.active_tab_shows_session(*wid, session)
            })
        })
    }

    /// The door's answer to `asker` for `effect` ([`admit`]): refused, and logged, only for a
    /// client whose tab is behind.
    pub(crate) fn desk_admits(&self, asker: Asker, effect: Effect) -> Result<(), String> {
        self.desk_judges(self.desk_standing(asker), effect)
    }

    /// [`Self::desk_admits`] over a standing already read.
    pub(crate) fn desk_judges(&self, standing: Standing, effect: Effect) -> Result<(), String> {
        match standing {
            Standing::Outside | Standing::InFront(_) => Ok(()),
            Standing::Behind(session) => {
                aterm_log::info!(
                    "desk: refused a control client in session {session}, a tab not in front: \
                     {}",
                    effect.words()
                );
                Err(refusal(effect))
            }
        }
    }
}

#[cfg(test)]
#[path = "desk_door_tests.rs"]
mod tests;
