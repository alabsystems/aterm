// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fabric endpoint bindings ([`FabricSession`], [`Registry`], [`Timeline`]).

use std::sync::Arc;

use aterm_session::SessionId;

pub(crate) use aterm_gui_fabric::*;

/// The one worker that reads each session's direct mailbox again after a write by another
/// process, a handoff peer's included ([`fleet_mail::watch`]): it hears each mailbox replaced
/// and waits with no deadline while every one is read. GUI readers only use the cached metadata,
/// and a parked `await inbox` is woken only by a refresh that changed it: nothing else rechecks.
/// Only the active owner's mailbox is read. An uncommitted successor waits for Commit; unknown
/// legacy mailboxes may still belong to older readers and are left untouched.
#[cfg(all(unix, not(test)))]
static MAIL_WATCH: std::sync::Mutex<Option<fleet_mail::watch::Door<crate::SessionCtx>>> =
    std::sync::Mutex::new(None);

/// Keep `ctx`'s direct mailbox read, starting the worker for the first session, or again where
/// it ended.
#[cfg(all(unix, not(test)))]
pub(crate) fn watch_direct_mail(ctx: &Arc<crate::SessionCtx>) {
    let mut door = MAIL_WATCH
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if door.as_ref().is_some_and(|door| door.watch(ctx)) {
        return;
    }
    match fleet_mail::watch::Door::spawn() {
        Ok(started) => {
            if started.watch(ctx) {
                *door = Some(started);
            }
        }
        Err(why) => aterm_log::info!(
            "fleet mail: no mailbox worker ({why}), so another process's mail waits for this session's next mailbox verb"
        ),
    }
}

/// Stop reading `sid`'s direct mailbox: its session closed.
#[cfg(all(unix, not(test)))]
pub(crate) fn forget_direct_mail(sid: &SessionId) {
    if let Some(door) = MAIL_WATCH
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
    {
        door.forget(sid);
    }
}

#[cfg(all(not(unix), not(test)))]
pub(crate) fn watch_direct_mail(_ctx: &Arc<crate::SessionCtx>) {}

#[cfg(all(not(unix), not(test)))]
pub(crate) fn forget_direct_mail(_sid: &SessionId) {}

impl FabricSession for crate::SessionCtx {
    fn self_id(&self) -> &SessionId {
        &self.self_id
    }

    fn fabric(&self) -> &SessionFabric {
        &self.fabric
    }

    fn record_fact(&self, kind: &'static str, fact: FabricFact<'_>) -> bool {
        self.timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .record_fact(kind, fact)
    }

    fn fabric_changed(&self) {
        crate::presence::post_fabric_changed(&self.self_id);
        self.watchers.ring_watchers();
    }

    /// A hold decides what the startup deadline may judge, so the host looks right after it moves,
    /// and a hold standing at that look is seen.
    fn hold_moved(&self) {
        crate::harness_host::ring();
    }

    fn holder_token(&self) -> Option<String> {
        let now = crate::metrics::now_us();
        self.turn_lease
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .and_then(|l| l.driving_token(now))
    }

    fn fleet_mail_path(&self) -> Option<std::path::PathBuf> {
        #[cfg(unix)]
        {
            aterm_types::dirs::state_dir().map(|state| {
                state
                    .join("fleet-mail")
                    .join(format!("{}.toml", self.self_id.as_str()))
            })
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    fn fleet_mail_ready(&self) -> bool {
        !aterm_update::is_uncommitted_handoff_candidate()
    }

    /// [`peer_mail_in`] this Mac's state directory; with none, by direct fleet mail.
    #[cfg(unix)]
    fn peer_mail(&self, node: &str) -> fleet_mail::PeerMail {
        aterm_types::dirs::state_dir().map_or(fleet_mail::PeerMail::Fleet, |state| {
            peer_mail_in(&state, node)
        })
    }

    /// One drive of `node` as `aterm ctl --host <node>` dials it, bounded by its deadline, which
    /// writes `request` and reads the one line that answers it. The drive is this session's
    /// mail, so its requester line names this session: both Macs' fleet activity records then
    /// say which session sent it, as they do for a request `aterm ctl` carried.
    #[cfg(unix)]
    fn fleet_drive(&self, node: &str, request: &str) -> Result<String, String> {
        let by = self.self_id.as_str();
        let link = aterm_agent::drive_fleet(
            node,
            &aterm_types::control_verbs::Requester::of(Some(by), None, None, None),
            FLEET_MAIL_WITHIN,
        )
        .map_err(|r| r.why)?;
        let (peer, label) = (link.node.clone(), link.label.clone());
        let answer = ask_one_line(link.stream, &label, request)?;
        // Mail this window sent another Mac passes no `aterm ctl`: its row of the fleet activity
        // record is written here, from the answer's verdict and never from the mail.
        if let Some(peer) = peer {
            crate::fleet_activity::note(crate::fleet_activity::sent(
                (&peer, &label),
                Some(by),
                request,
                &answer,
            ));
        }
        Ok(answer)
    }
}

/// Write `request` as one line on a drive's `stream` and read the one line that answers it, at
/// most [`FLEET_MAIL_ANSWER_MAX`] bytes, which a peer that sends no newline cannot outrun.
/// `label` names the peer in what it says.
#[cfg(unix)]
fn ask_one_line(
    mut stream: impl std::io::Read + std::io::Write,
    label: &str,
    request: &str,
) -> Result<String, String> {
    use std::io::{BufRead as _, Read as _};
    stream
        .write_all(format!("{request}\n").as_bytes())
        .and_then(|()| stream.flush())
        .map_err(|e| format!("{label}: {e}"))?;
    let mut line = String::new();
    std::io::BufReader::new(stream)
        .take(FLEET_MAIL_ANSWER_MAX)
        .read_line(&mut line)
        .map_err(|e| format!("{label}: {e}"))?;
    if line.is_empty() {
        return Err(format!("{label} hung up before it answered"));
    }
    Ok(line)
}

/// How mail reaches `node` from the Mac whose state directory is `state` (§11 C6): by the mesh
/// route once that Mac heard `node`'s presence row (the node service's seen ledger,
/// `<state>/mesh/seen`, which outlives a restart), by direct fleet mail before. Its own node is
/// on the mesh.
#[cfg(unix)]
fn peer_mail_in(state: &std::path::Path, node: &str) -> fleet_mail::PeerMail {
    let own = aterm_net::fleet_member::read_node_id(state).ok().flatten();
    if own.as_deref() == Some(node)
        || aterm_mesh::presence::read_seen(&state.join(aterm_mesh::logs::MESH_DIR))
            .contains_key(node)
    {
        fleet_mail::PeerMail::Mesh
    } else {
        fleet_mail::PeerMail::Fleet
    }
}

/// A direct fleet mail drive's deadline, under the peer's 30 s cut as `aterm ctl --host`'s is.
#[cfg(unix)]
const FLEET_MAIL_WITHIN: std::time::Duration = std::time::Duration::from_secs(28);

/// The longest answer to a direct fleet mail post a drive reads.
#[cfg(unix)]
const FLEET_MAIL_ANSWER_MAX: u64 = 4096;

impl Registry for crate::session_store::SessionStore {
    fn sessions(&self) -> Vec<Arc<dyn FabricSession>> {
        let mut live: Vec<_> = self.live_handles().collect();
        live.sort_by_key(|h| h.local_id);
        live.into_iter()
            .map(|h| Arc::clone(&h.ctx) as Arc<dyn FabricSession>)
            .collect()
    }

    fn session(&self, sid: &SessionId) -> Option<Arc<dyn FabricSession>> {
        self.by_sid(sid)
            .map(|h| Arc::clone(&h.ctx) as Arc<dyn FabricSession>)
    }

    fn roster_seq(&self) -> u64 {
        crate::session_store::SessionStore::roster_seq(self)
    }

    fn bridge_lost_advised(&self, sessions: usize) {
        #[cfg(any(unix, test))]
        crate::message_inbox::queue_message(crate::message_reporters::fabric_lost_advisory(
            sessions,
        ));
        #[cfg(not(any(unix, test)))]
        let _ = sessions;
    }

    fn torn_state_set_aside(&self, files: u64, told_when_empty: bool) {
        #[cfg(any(unix, test))]
        crate::message_inbox::queue_exception(
            crate::message_reporters::torn_state_set_aside_entry(files, told_when_empty),
        );
        #[cfg(not(any(unix, test)))]
        let _ = (files, told_when_empty);
    }

    fn torn_state_held(&self, files: u64) {
        #[cfg(any(unix, test))]
        if files == 0 {
            crate::message_inbox::queue_exception_clear(crate::message_reporters::torn_state_key());
        }
        #[cfg(not(any(unix, test)))]
        let _ = files;
    }
}

impl Timeline for crate::session_timeline::SessionTimeline {
    /// THE ONE FABRIC HOOK ([`crate::session_timeline::SessionTimeline::slog_take_wake`]).
    fn record_fact(&mut self, kind: &'static str, fact: FabricFact<'_>) -> bool {
        let at = self.slog().now();
        self.slog_mut().observe_fabric(fact, at);
        crate::session_timeline::SessionTimeline::record(self, kind);
        self.slog_take_wake()
    }

    fn log_carried(&self) -> bool {
        self.slog().log_carried()
    }
}

/// [`aterm_gui_fabric::with_link_reset`].
#[cfg(test)]
pub(crate) fn with_link_reset<T>(f: impl FnOnce() -> T) -> T {
    #[cfg(unix)]
    also_reset_with_the_link(crate::fabric_launch::reset_for_tests);
    aterm_gui_fabric::with_link_reset(f)
}

/// THE NEGATIVE CONTROL OF A FENCED WAIT ([`SessionFabric::fence_waits`]).
#[cfg(all(test, unix))]
pub(crate) fn assert_parked_on_the_fence(
    fabric: &SessionFabric,
    before: u64,
    finished: impl Fn() -> bool,
    what: &str,
) {
    let parked = |n: u64| {
        aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            std::time::Duration::from_millis(1),
            || (fabric.fence_waits() > n).then_some(()),
        )
        .is_some()
    };
    assert!(
        parked(before),
        "{what}: the waiter never blocked on the fence"
    );
    assert!(!finished(), "{what}: blocked, so not answered");
    let blocked = fabric.fence_waits();
    fabric.wake_waiters_for_test();
    assert!(
        parked(blocked),
        "{what}: woken under the standing fence, it must block again"
    );
    assert!(!finished(), "{what}: still not answered");
}

#[cfg(test)]
#[path = "fabric_handoff_carry_tests.rs"]
mod handoff_carry;

#[cfg(all(test, unix))]
#[path = "fabric_bind_peer_mail_tests.rs"]
mod peer_mail_tests;
