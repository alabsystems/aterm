// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The broker's one act that is not LAN traffic: Spotlight's exclusion of the console
//! account's own new top-level home folders ([`aterm_uds::lan::wire::Request::ScopeHome`],
//! `docs/DESIGN-lan-broker-2026-11-05.md` §2). The request names nothing; the duty
//! `aterm lan serve` hands the broker ([`Duties`], `aterm_machines::init_spotlight::broker_duty`)
//! works out the edit from what root reads. Here the broker runs one such request at a time,
//! answers another at once as busy, and restarts `mds` at most once a [`SCOPE_PACE`]: a request
//! inside it waits out the rest and plans again on fresh reads. Adoption's exit waits for an apply
//! under way ([`exit_for_launchd`]), so no exit lands between Spotlight's edit and its receipt.

use std::io;
use std::sync::{Arc, Mutex, OnceLock, PoisonError, TryLockError};
use std::time::Duration;

use aterm_uds::CtlStream;
use aterm_uds::lan::wire;
use aterm_uds::lan::{SCOPE_PACE, ScopeAnswer, ScopeDuty};

/// How long adoption's exit waits for an apply under way before it exits anyway: a hang
/// detector over the client's own bound for the request.
#[cfg(target_os = "macos")]
const SETTLE_HANG: Duration = Duration::from_secs(240);

/// What `aterm lan serve` hands the broker to do beside the LAN ([`crate::serve`]).
#[derive(Clone, Default)]
pub struct Duties {
    /// Spotlight's exclusion of the console account's own new top-level home folders. None
    /// answers every request `ENOTSUP` ([`ScopeAnswer::Unserved`]): the direct path, which is
    /// no root process, and a test's broker serving the LAN alone.
    pub scope: Option<Arc<dyn ScopeDuty>>,
}

impl Duties {
    /// No duty beside the LAN.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }
}

/// The time of the broker's last apply that may have restarted `mds`, held under the one lock
/// every apply takes.
#[derive(Debug, Default)]
pub(crate) struct Paced {
    /// Unix seconds by the duty's clock as the last apply that may have restarted `mds` ended
    /// ([`restarts_pace`]).
    last: Option<u64>,
}

/// The Spotlight duty as one server holds it: the duty, and its one lock.
pub(crate) struct Scope {
    duty: Option<Arc<dyn ScopeDuty>>,
    pub(crate) held: Arc<Mutex<Paced>>,
}

impl Scope {
    /// What `duties` asks of this server.
    pub(crate) fn of(duties: Duties) -> Self {
        Self {
            duty: duties.scope,
            held: Arc::new(Mutex::new(Paced::default())),
        }
    }

    /// No Spotlight duty.
    pub(crate) fn none() -> Self {
        Self::of(Duties::none())
    }
}

/// Answer the request on `stream` with its own frame, and write its one `log` line: the peer's
/// pid and uid and the answer's class ([`ScopeAnswer::class`]), never a path. The uid is the
/// kernel's ([`aterm_uds::peer::peer_uid`]); a peer it does not name is refused.
///
/// # Errors
/// The answer could not be sent.
pub(crate) fn serve(
    scope: &Scope,
    stream: &CtlStream,
    log: fn(std::fmt::Arguments<'_>),
) -> io::Result<()> {
    let pid = aterm_uds::fdpass::peer_pid(stream).unwrap_or(0);
    let uid = aterm_uds::peer::peer_uid(stream);
    let answer = match uid {
        Some(uid) => decide(scope, uid),
        None => ScopeAnswer::Refused("the kernel did not name this process's uid".to_owned()),
    };
    match uid {
        Some(uid) => log(format_args!("scope pid {pid} uid {uid} {}", answer.class())),
        None => log(format_args!(
            "scope pid {pid} uid unknown {}",
            answer.class()
        )),
    }
    wire::send(stream, &answer.frame())
}

/// The answer to `uid`'s request: [`ScopeAnswer::Unserved`] with no duty, [`ScopeAnswer::Busy`]
/// while another request holds the lock (a lock a panicked request left poisoned is taken all
/// the same), else, holding it, the duty's plan; a plan made inside [`SCOPE_PACE`] of the last
/// apply that may have restarted `mds` is made again after the wait; an empty plan is nothing
/// excluded and writes nothing; else the duty's apply, whose end starts the next pace where its
/// answer says it may have restarted `mds` ([`restarts_pace`]).
pub(crate) fn decide(scope: &Scope, uid: u32) -> ScopeAnswer {
    let Some(duty) = &scope.duty else {
        return ScopeAnswer::Unserved("this LAN broker has no Spotlight duty".to_owned());
    };
    let mut paced = match scope.held.try_lock() {
        Ok(paced) => paced,
        Err(TryLockError::WouldBlock) => return ScopeAnswer::Busy,
        Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
    };
    let mut plan = match duty.plan(uid) {
        Ok(plan) => plan,
        Err(answer) => return answer,
    };
    if plan.folders.is_empty() {
        return ScopeAnswer::Excluded(0);
    }
    if let Some(last) = paced.last {
        let since = duty.now().saturating_sub(last);
        let pace = SCOPE_PACE.as_secs();
        if since < pace {
            duty.sleep(Duration::from_secs(pace - since));
            plan = match duty.plan(uid) {
                Ok(plan) => plan,
                Err(answer) => return answer,
            };
            if plan.folders.is_empty() {
                return ScopeAnswer::Excluded(0);
            }
        }
    }
    let answer = duty.apply(&plan);
    if restarts_pace(&answer) {
        paced.last = Some(duty.now());
    }
    answer
}

/// Whether an apply that answered `answer` may have restarted `mds`, and so starts the pace
/// ([`ScopeDuty::apply`]'s contract): an exclusion of at least one folder, or a failure, which
/// may have come after the configuration's rename. A busy apply, one that left Spotlight as it
/// is and one that excluded nothing ended before the rename, so the next request waits nothing
/// for them.
fn restarts_pace(answer: &ScopeAnswer) -> bool {
    match answer {
        ScopeAnswer::Excluded(added) => *added > 0,
        ScopeAnswer::Failed(_) => true,
        ScopeAnswer::Busy
        | ScopeAnswer::Unserved(_)
        | ScopeAnswer::Refused(_)
        | ScopeAnswer::Unchanged(_) => false,
    }
}

/// The lock of the server whose apply adoption's exit waits for ([`held_at_exit`]).
static AT_EXIT: OnceLock<Arc<Mutex<Paced>>> = OnceLock::new();

/// Make adoption's exit wait for `scope`'s apply under way ([`exit_for_launchd`]): the serving
/// broker's, set once as it starts serving.
pub(crate) fn held_at_exit(scope: &Scope) {
    let _ = AT_EXIT.set(Arc::clone(&scope.held));
}

/// Exit 0 for launchd to start the broker's image again (an adopted update, a replaced image),
/// once no Spotlight apply is under way: the serving broker's lock is taken for good
/// ([`settle`]), waiting at most [`SETTLE_HANG`] for an apply that holds it, so a later request
/// answers busy and none starts its edit as the process ends.
#[cfg(target_os = "macos")]
pub(crate) fn exit_for_launchd() -> ! {
    if let Some(held) = AT_EXIT.get()
        && !settle(held, SETTLE_HANG)
    {
        crate::server::log(format_args!(
            "a Spotlight exclusion was still under way after {}s: exiting for launchd all the same",
            SETTLE_HANG.as_secs()
        ));
    }
    std::process::exit(0)
}

/// Take `held` and keep it until the process exits, waiting at most `hang` for an apply that
/// holds it: whether it was taken. A thread of its own takes it, so the wait has a bound, and
/// stays parked holding it.
pub(crate) fn settle(held: &Arc<Mutex<Paced>>, hang: Duration) -> bool {
    let (taken, heard) = std::sync::mpsc::sync_channel(1);
    let held = Arc::clone(held);
    let spawned = std::thread::Builder::new()
        .name("lan-scope-settle".into())
        .spawn(move || {
            let _kept = held.lock().unwrap_or_else(PoisonError::into_inner);
            let _ = taken.send(());
            loop {
                std::thread::park();
            }
        });
    spawned.is_ok() && heard.recv_timeout(hang).is_ok()
}

#[cfg(test)]
#[path = "scope_tests.rs"]
mod tests;
