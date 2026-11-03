// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private built-in constructors. A native ticket contains one exact stored continuation;
//! neither a reconnect nor its semantic Upgrade/Relaunch duty grants general terminal input.

use super::contract::Scope;
use super::native_act::{
    self, Binding, Constructor, Decision, Image, ImagePhase, NativePlan, Prepare, Process, Reply,
    Request,
};
use super::upgrade_drive::{self, Client, Opts, St, Terminated, Typed};

#[cfg(all(test, unix))]
#[path = "native_restart_transport_tests.rs"]
mod transport_tests;

#[path = "native_restart_observation.rs"]
mod observation;
pub(super) use observation::observe_live;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Recovery {
    Legacy,
    Admitted,
    Cancelled,
    Completed,
    Retired,
}

pub(super) fn coordinated(st: &St) -> bool {
    st.native_coordination
        || st.native.is_some()
        || !st.native_prepare.is_empty()
        || st.native_completed_prepare.is_some()
        || st.native_observation.is_some()
}

pub(super) fn constructor_enabled(client: &mut Client, tab: &str) -> bool {
    matches!(native_act::exchange(client, tab, &Request::Constructors),
        Ok(Reply::Constructors(enabled)) if enabled.contains(&Constructor::PrivateRestart))
}

fn legacy_before_prepare(opts: &Opts, client: &mut Client, st: &St) -> Result<bool, &'static str> {
    if opts.token.is_none() {
        return if coordinated(st) {
            Err("native-coordinated-unavailable")
        } else {
            Ok(true)
        };
    }
    if constructor_enabled(client, &st.tab) {
        return Ok(false);
    }
    if coordinated(st) {
        Err("native-constructor-disabled")
    } else {
        Ok(true)
    }
}

fn terminal_reply(st: &mut St, reply: &Reply) -> Option<Recovery> {
    let outcome = match reply {
        Reply::Decision(Decision::ProvenNotCommittedRollbackSafe) => Recovery::Cancelled,
        Reply::Committed(_) | Reply::RestartCommitted(_) => Recovery::Completed,
        Reply::Decision(Decision::Retired(_)) => Recovery::Retired,
        _ => return None,
    };
    if outcome == Recovery::Completed {
        if let Some(Request::Prepare(prepare)) = Request::parse(&st.native_prepare) {
            st.native_completed_prepare = Some(prepare);
        }
        st.native_observation = match reply {
            Reply::RestartCommitted(observation) => Some((**observation).clone()),
            _ => None,
        };
    }
    st.native = None;
    st.native_prepare.clear();
    // Completion retains the phase of the command that actually ran. The
    // renewal caller and durable observation must distinguish a live rollback
    // vendor from a successful forward replacement after this capability closes.
    if outcome != Recovery::Completed {
        st.native_rollback = false;
    }
    if outcome == Recovery::Completed
        && !matches!(
            st.phase,
            super::upgrade::Phase::Relaunched { .. } | super::upgrade::Phase::Done
        )
    {
        // A completed native mutation is observed, never signalled/typed a second time.
        st.relaunched(upgrade_drive::now_s());
    }
    if outcome == Recovery::Retired {
        st.stop("native-cohort-retired", upgrade_drive::now_s());
    }
    Some(outcome)
}

fn recover_reply(st: &mut St, reply: Reply) -> Result<Recovery, &'static str> {
    if let Some(outcome) = terminal_reply(st, &reply) {
        return Ok(outcome);
    }
    match reply {
        Reply::Admitted(binding) => {
            st.native = Some(binding);
            Ok(Recovery::Admitted)
        }
        Reply::Pending(_) => Err("native-predecessors"),
        _ => Err("native-unknown"),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn begin(
    opts: &Opts,
    client: &mut Client,
    st: &mut St,
    session: &str,
    duty: Scope,
    target: &std::path::Path,
    command: Option<&super::relaunch::PlannedCommand>,
    rollback: Option<&super::relaunch::PlannedCommand>,
) -> Result<Recovery, &'static str> {
    begin_plan(
        opts, client, st, session, duty, target, command, rollback, None,
    )
}

pub(super) fn begin_crash(
    opts: &Opts,
    client: &mut Client,
    st: &mut St,
    session: &str,
    snap: &super::relaunch::Snapshot,
    command: Option<&super::relaunch::PlannedCommand>,
) -> Result<Recovery, &'static str> {
    if legacy_before_prepare(opts, client, st)? {
        return Ok(Recovery::Legacy);
    }
    let observed = if st.native_prepare.is_empty() {
        Some(observation::captured(opts, snap).ok_or("native-crash-observation")?)
    } else {
        None
    };
    let target = command.ok_or("native-command")?.target.clone();
    begin_plan(
        opts,
        client,
        st,
        session,
        Scope::Relaunch,
        &target,
        command,
        None,
        observed,
    )
}

/// A prior native mutation is positively terminal. Its later failed launch is a distinct
/// crash, subject to the caller's already-applied rollback/attempt/grace policy. Only the
/// immutable old slot can become this fresh operation's command; mutable current is unread.
pub(super) fn begin_completed_rollback(
    opts: &Opts,
    client: &mut Client,
    st: &mut St,
    session: &str,
) -> Result<Recovery, &'static str> {
    if legacy_before_prepare(opts, client, st)? {
        return Ok(Recovery::Legacy);
    }
    if st.source != upgrade_drive::SAME_PROGRAM
        || !matches!(st.phase, super::upgrade::Phase::Exiting { .. })
    {
        return Err("native-crash-observation");
    }
    let previous = st
        .native_completed_prepare
        .as_deref()
        .ok_or("native-completed-recipe")?;
    let NativePlan::Restart {
        shell,
        forward,
        rollback: Some(back),
        ..
    } = &previous.plan
    else {
        return Err("native-completed-rollback");
    };
    let observed = st
        .native_observation
        .clone()
        .ok_or("native-crash-observation")?;
    if observed.registration.host_epoch != previous.host_epoch
        || observed.registration.vendor.image != forward.image
        || observed.registration.shell != *shell
        || back.legacy_line().as_deref() != Some(st.line.as_str())
    {
        return Err("native-completed-recipe-changed");
    }
    let command = super::relaunch::PlannedCommand {
        target: std::path::PathBuf::from(&back.program),
        argv: back.argv.clone(),
        env: back.env.clone(),
        unset: back.unset.clone(),
        cwd: back.cwd.clone(),
        heal: back.heal.as_ref().map(std::path::PathBuf::from),
        dialect: back.dialect,
    };
    if command.capture().as_ref() != Some(back.as_ref()) {
        return Err("native-completed-target-changed");
    }
    st.pid = observed.registration.vendor.pid;
    st.shell = observed.registration.shell.pid;
    let target = command.target.clone();
    begin_plan(
        opts,
        client,
        st,
        session,
        Scope::Relaunch,
        &target,
        Some(&command),
        None,
        Some((observed.registration, observed.binding)),
    )
}

#[allow(clippy::too_many_arguments)]
fn begin_plan(
    opts: &Opts,
    client: &mut Client,
    st: &mut St,
    session: &str,
    duty: Scope,
    target: &std::path::Path,
    command: Option<&super::relaunch::PlannedCommand>,
    rollback: Option<&super::relaunch::PlannedCommand>,
    observed: Option<(
        super::native_observe::Registration,
        super::native_observe::ObservationBinding,
    )>,
) -> Result<Recovery, &'static str> {
    // CLI/operator constructors preserve their independent legacy authority. Only the real
    // private host opts contain this token; holder/as=person words never select this branch.
    if legacy_before_prepare(opts, client, st)? {
        return Ok(Recovery::Legacy);
    }
    if st.native.is_some() || !st.native_prepare.is_empty() {
        return Err("native-unresolved-operation");
    }
    if st.native_prepare.is_empty() {
        let supported = client
            .help_counted("native-act")
            .is_ok_and(|(head, _)| crate::is_ok_reply(&head));
        if !supported {
            return Err("native-act-unavailable");
        }
        let status = upgrade_drive::status_line(client, &st.tab).ok_or("native-status")?;
        if super::relaunch::line_held_by_status(
            Some(&status),
            st.line_grace_s.unwrap_or(opts.human_grace_s),
            &upgrade_drive::hand_name(),
        )
        .is_some()
        {
            return Err("native-person-or-hand");
        }
        let expected_human_seq = human_seq(&status).ok_or("native-person-unread")?;
        let revision = status
            .split_ascii_whitespace()
            .find_map(|w| w.strip_prefix("supervisor_revision="))
            .and_then(|n| n.parse::<u64>().ok())
            .ok_or("native-revision")?;
        let process = match observed.as_ref() {
            Some((registration, _)) if registration.vendor.pid == st.pid => {
                registration.vendor.clone()
            }
            Some(_) => return Err("native-crash-observation"),
            None => Process::observe(st.pid).ok_or("native-process")?,
        };
        let shell = Process::observe(st.shell).ok_or("native-shell")?;
        if observed
            .as_ref()
            .is_some_and(|(registration, _)| registration.shell != shell)
        {
            return Err("native-shell-changed");
        }
        let target = Image::at(target).ok_or("native-target")?;
        let forward = command
            .and_then(super::relaunch::PlannedCommand::capture)
            .ok_or("native-command")?;
        if forward.image != target || forward.legacy_line().as_deref() != Some(st.line.as_str()) {
            return Err("native-command-changed");
        }
        let rollback = match rollback {
            None if st.back_line.is_empty() => None,
            Some(command) => {
                let command = command.capture().ok_or("native-rollback-command")?;
                if command.image != process.image
                    || command.legacy_line().as_deref() != Some(st.back_line.as_str())
                {
                    return Err("native-rollback-changed");
                }
                Some(command)
            }
            None => return Err("native-rollback-command"),
        };
        let launcher = std::env::current_exe()
            .ok()
            .and_then(|path| Image::at(&path))
            .ok_or("native-launcher")?;
        let socket = opts.sock.clone().ok_or("native-endpoint")?;
        let Reply::Info {
            epoch: host_epoch, ..
        } = native_act::exchange(client, &st.tab, &Request::Info)
            .map_err(|_| "native-host-epoch")?
        else {
            return Err("native-host-epoch");
        };
        let deadline_us = native_act::monotonic_us()
            .and_then(|now| now.checked_add(120_000_000))
            .ok_or("native-clock")?;
        let request = Request::prepare(Prepare {
            id: native_act::operation_id().map_err(|_| "native-entropy")?,
            host_epoch,
            holder: upgrade_drive::hand_name(),
            revision,
            expected_human_seq: Some(expected_human_seq),
            deadline_us,
            duty,
            plan: NativePlan::Restart {
                process,
                observation: observed.map(|(_, binding)| binding),
                shell,
                launcher,
                socket,
                forward: Box::new(forward),
                rollback: rollback.map(Box::new),
            },
        });
        st.native_prepare = request.encode().ok_or("native-plan")?;
        st.native_coordination = true;
        // Persist the original id/plan/deadline BEFORE the first prepare dispatch. A lost
        // prepare ACK can recover that same operation without inventing another mutation.
        upgrade_drive::save_checked(opts, session, st).map_err(|_| "native-unsaved")?;
    }
    let tab = st.tab.clone();
    let recovered = recover(client, &tab, st)?;
    if recovered == Recovery::Completed {
        // A local record failure cannot turn a positively completed operation back into an
        // unsent constructor. The saved original prepare still replays the same completion.
        let _saved = upgrade_drive::save_checked(opts, session, st);
        return Ok(recovered);
    }
    upgrade_drive::save_checked(opts, session, st).map_err(|_| "native-unsaved")?;
    if recovered == Recovery::Cancelled {
        Err("native-cancelled")
    } else {
        Ok(recovered)
    }
}

pub(super) fn recover(
    client: &mut Client,
    tab: &str,
    st: &mut St,
) -> Result<Recovery, &'static str> {
    let request = Request::parse(&st.native_prepare).ok_or("native-plan")?;
    let Request::Prepare(prepare) = &request else {
        return Err("native-plan");
    };
    let reply = native_act::exchange(client, tab, &request).map_err(|_| "native-unknown")?;
    if matches!(&reply, Reply::Admitted(binding) | Reply::Pending(binding) if binding.id != prepare.id)
    {
        return Err("native-unknown");
    }
    recover_reply(st, reply)
}

pub(super) fn abort(client: &mut Client, tab: &str, st: &mut St) -> bool {
    match recover(client, tab, st) {
        Ok(Recovery::Cancelled) => return true,
        Ok(Recovery::Completed | Recovery::Retired | Recovery::Legacy) | Err(_) => return false,
        Ok(Recovery::Admitted) => {}
    }
    let Some(binding) = st.native.as_ref() else {
        return false;
    };
    native_act::exchange(client, tab, &Request::Abort(binding.clone()))
        .ok()
        .and_then(|reply| terminal_reply(st, &reply))
        == Some(Recovery::Cancelled)
}

pub(super) fn signal(client: &mut Client, tab: &str, binding: &Binding) -> Terminated {
    match native_act::exchange(client, tab, &Request::Signal(binding.clone())) {
        Ok(Reply::Signalled) => Terminated::Sent,
        // A typed context never reaches the legacy raw-kill fallback.
        Ok(_) | Err(_) => Terminated::Refused("native-unknown".into()),
    }
}

pub(super) fn resume(
    client: &mut Client,
    tab: &str,
    binding: &Binding,
    phase: ImagePhase,
    generation: &str,
    human_seq: u64,
) -> Result<(), Typed> {
    match native_act::exchange(
        client,
        tab,
        &Request::Resume(binding.clone(), phase, generation.to_string(), human_seq),
    ) {
        Ok(Reply::Resumed(done)) if done == phase => Ok(()),
        Ok(Reply::Unwritten) => Err(Typed::Changed),
        // An unanswered continuation may already be in the queue. The exact same op is
        // observed/recovered; it is never retyped through ordinary send/key/turn.
        Ok(_) | Err(_) => Err(Typed::Unanswered),
    }
}

pub(super) fn complete(
    opts: &Opts,
    session: &str,
    client: &mut Client,
    tab: &str,
    st: &mut St,
) -> bool {
    let Some(binding) = st.native.clone() else {
        return st.native_prepare.is_empty();
    };
    if let Ok(reply) = native_act::exchange(client, tab, &Request::Query(binding.clone())) {
        if let Some(outcome) = terminal_reply(st, &reply) {
            if outcome == Recovery::Cancelled {
                st.stop("native-cancelled-before-mutation", upgrade_drive::now_s());
            }
            return outcome == Recovery::Completed;
        }
        match reply {
            Reply::Resumed(ImagePhase::Rollback) => {
                // The exact host operation can have accepted Rollback before
                // its Resume ACK was lost. Observe its phase; never re-signal.
                if !matches!(Request::parse(&st.native_prepare), Some(Request::Prepare(prepare))
                    if matches!(prepare.plan, NativePlan::Restart { rollback: Some(_), .. }))
                {
                    return false;
                }
                st.native_rollback = true;
            }
            Reply::Resumed(ImagePhase::Forward) if st.native_rollback => return false,
            _ => {}
        }
    }
    let phase = phase(st);
    // A stack or ACK can disappear after the host commits. Persist the exact
    // observed phase first, so a terminal Query without phase words cannot
    // relabel that already completed Rollback as Forward on the next sweep.
    if upgrade_drive::save_checked(opts, session, st).is_err() {
        return false;
    }
    // Precommit may be Unknown because an earlier identical Commit was received but its
    // kernel observation failed. Replaying the same sealed phase/version only completes it;
    // the server never admits another native mutation through this request.
    let _proof = native_act::exchange(client, tab, &Request::Precommit(binding.clone(), phase));
    native_act::exchange(client, tab, &Request::Commit(binding, phase, st.to.clone()))
        .ok()
        .and_then(|reply| terminal_reply(st, &reply))
        == Some(Recovery::Completed)
}

pub(super) fn resume_observed(client: &mut Client, tab: &str, st: &St) -> bool {
    st.native.as_ref().is_some_and(|binding| {
        match native_act::exchange(client, tab, &Request::Query(binding.clone())) {
            Ok(Reply::Resumed(done)) => done == phase(st),
            Ok(Reply::Committed(_) | Reply::RestartCommitted(_)) => true,
            _ => false,
        }
    })
}

pub(super) fn phase(st: &St) -> ImagePhase {
    if st.native_rollback {
        ImagePhase::Rollback
    } else {
        ImagePhase::Forward
    }
}

pub(super) fn human_seq(status: &str) -> Option<u64> {
    let mut values = status
        .split_ascii_whitespace()
        .filter_map(|word| word.strip_prefix("human_seq="));
    let text = values.next()?;
    let parsed = text.parse::<u64>().ok()?;
    (values.next().is_none() && parsed.to_string() == text).then_some(parsed)
}

pub(super) fn retire(client: &mut Client, tab: &str, st: &mut St) -> bool {
    let Some(binding) = st.native.clone() else {
        return st.native_prepare.is_empty();
    };
    native_act::exchange(client, tab, &Request::Reconcile(binding))
        .ok()
        .and_then(|reply| terminal_reply(st, &reply))
        .is_some_and(|outcome| matches!(outcome, Recovery::Retired | Recovery::Cancelled))
}

pub(super) fn read(value: &aterm_json::Value) -> Option<Option<Binding>> {
    match (value.get("native_id"), value.get("native_cap")) {
        (None, None) => Some(None),
        (Some(id), Some(cap)) => {
            let binding = Binding {
                id: id.as_str()?.to_string(),
                capability: cap.as_str()?.to_string(),
            };
            Request::Query(binding.clone()).encode()?;
            Some(Some(binding))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::native_act::ClaimOutcome;
    use super::super::upgrade::Phase;
    use super::*;

    fn pending() -> (St, super::super::upgrade_drive::Unsent) {
        let mut st = St {
            phase: Phase::Announced { at_s: 7, asks: 1 },
            ..St::default()
        };
        let back = st.signalled(4242, 400, "tab", "exact-forward".into(), 8);
        st.native_prepare = "original-id-and-plan deadline-us=123".into();
        st.native = Some(Binding {
            id: "ab".repeat(16),
            capability: "cd".repeat(32),
        });
        (st, back)
    }

    #[test]
    fn positive_safe_cancellation_restores_unsent_constructor_while_unknown_retains_original() {
        let (mut st, back) = pending();
        assert_eq!(
            recover_reply(
                &mut st,
                Reply::Decision(Decision::ProvenNotCommittedRollbackSafe)
            ),
            Ok(Recovery::Cancelled)
        );
        assert!(st.native_prepare.is_empty() && st.native.is_none());
        // This is the real constructor's unsent transition, not a fresh native admission.
        st.unsent(back);
        assert_eq!(st.phase, Phase::Announced { at_s: 7, asks: 1 });

        let (mut st, _) = pending();
        let original = st.clone();
        assert_eq!(
            recover_reply(&mut st, Reply::Decision(Decision::Unknown)),
            Err("native-unknown")
        );
        assert_eq!(
            st, original,
            "Unknown cannot erase the original id/deadline or restore an unsent mutator"
        );
    }

    #[test]
    fn terminal_completion_keeps_the_actual_phase_but_retirement_and_cancellation_clear_it() {
        for rollback in [false, true] {
            let (mut st, _) = pending();
            st.native_rollback = rollback;
            assert_eq!(
                terminal_reply(&mut st, &Reply::Committed(ClaimOutcome::Builtin)),
                Some(Recovery::Completed)
            );
            assert_eq!(st.native_rollback, rollback);
            assert!(st.native.is_none() && st.native_prepare.is_empty());
            assert_eq!(
                terminal_reply(&mut st, &Reply::Committed(ClaimOutcome::Builtin)),
                Some(Recovery::Completed)
            );
            assert_eq!(
                st.native_rollback, rollback,
                "lost completion ACK preserves the observed phase"
            );
        }
        for decision in [
            Decision::ProvenNotCommittedRollbackSafe,
            Decision::Retired(super::super::native_act::Retirement::Exited),
        ] {
            let (mut st, _) = pending();
            st.native_rollback = true;
            terminal_reply(&mut st, &Reply::Decision(decision));
            assert!(!st.native_rollback);
        }
    }

    #[test]
    fn completed_prepare_or_query_or_abort_receipt_moves_constructor_to_observation_only() {
        for claim in [
            ClaimOutcome::Builtin,
            ClaimOutcome::Lost,
            ClaimOutcome::Bound(9),
        ] {
            let (mut st, _) = pending();
            assert_eq!(
                recover_reply(&mut st, Reply::Committed(claim)),
                Ok(Recovery::Completed)
            );
            assert!(st.native_prepare.is_empty() && st.native.is_none());
            assert!(
                matches!(st.phase, Phase::Relaunched { .. }),
                "terminal completion never restores an unsent constructor or permits another signal"
            );
            let observed = st.clone();
            assert_eq!(
                terminal_reply(&mut st, &Reply::Committed(claim)),
                Some(Recovery::Completed)
            );
            assert_eq!(
                st, observed,
                "duplicate completion never resets the observed act time"
            );
        }
        let (mut st, _) = pending();
        assert_eq!(
            terminal_reply(
                &mut st,
                &Reply::Decision(Decision::ProvenNotCommittedRollbackSafe)
            ),
            Some(Recovery::Cancelled)
        );
        assert!(
            st.native_prepare.is_empty() && st.native.is_none(),
            "a lost Abort ACK replays its safe cancellation"
        );
    }
}
