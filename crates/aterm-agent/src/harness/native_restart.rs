// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private built-in constructors. A native ticket contains one exact stored continuation;
//! neither a reconnect nor its semantic Upgrade/Relaunch duty grants general terminal input.

use super::contract::Scope;
use super::native_act::{
    self, AskedIntent, AskedUpgradeIntent, Binding, Constructor, Decision, Image, ImagePhase,
    NativePlan, Prepare, Process, Reply, Request,
};
use super::upgrade_drive::{self, Client, Opts, St, Terminated, Typed};

#[cfg(all(test, unix))]
#[path = "native_restart_transport_tests.rs"]
mod transport_tests;

#[cfg(all(test, unix))]
#[path = "native_restart_fixture.rs"]
pub(super) mod fixture;

#[path = "native_restart_observation.rs"]
mod observation;
pub(super) use observation::observe_live;

#[cfg(test)]
#[path = "native_retirement_debt_tests.rs"]
mod retirement_debt_tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Recovery {
    Legacy,
    Admitted,
    Cancelled,
    Completed,
    Retired,
    Held,
}

pub(super) const WAIT_RETIRED_UNISSUED: &str = "wait:native-retired-unissued";

pub(super) fn coordinated(st: &St) -> bool {
    st.native_coordination
        || st.native_rollback
        || st.native_retired_unissued
        || st.native_account_candidate.is_some()
        || st.native.is_some()
        || !st.native_prepare.is_empty()
        || st.native_completed_prepare.is_some()
        || st.native_noexec_prepare.is_some()
        || st.native_observation.is_some()
        || super::native_record::was_coordinated(st)
}

pub(super) fn constructor_enabled(client: &mut Client, tab: &str) -> bool {
    matches!(native_act::exchange(client, tab, &Request::Constructors),
        Ok(Reply::Constructors(enabled)) if enabled.contains(&Constructor::PrivateRestart))
}

fn require_cold_constructor(opts: &Opts, client: &mut Client, st: &St) -> Result<(), &'static str> {
    if opts.token.is_none() {
        return Err("native-cold-requires-owned-launcher");
    }
    if matches!(native_act::exchange(client,&st.tab,&Request::Constructors),Ok(Reply::Constructors(enabled)) if enabled.contains(&Constructor::ColdRestart))
    {
        Ok(())
    } else {
        Err("native-cold-constructor-disabled")
    }
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

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeRetirementDebt",
        action = "NotGranted",
        project = "aterm_agent::harness::native_restart::retirement_debt_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeRetirementDebt",
        action = "Exited",
        project = "aterm_agent::harness::native_restart::retirement_debt_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeRetirementDebt",
        action = "Completed",
        project = "aterm_agent::harness::native_restart::retirement_debt_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeRetirementDebt",
        action = "Replay",
        project = "aterm_agent::harness::native_restart::retirement_debt_tests::transition"
    )
)]
pub(super) fn terminal_reply(st: &mut St, reply: &Reply) -> Option<Recovery> {
    // A terminal no-grant hold cannot be revived by a later receipt or host epoch.
    if st.native_retired_unissued {
        return Some(Recovery::Held);
    }
    // Without a retained operation, replies carry no authority to change state.
    // Duplicate terminal replies can only acknowledge an existing matching kind.
    if st.native.is_none() && st.native_prepare.is_empty() {
        return match reply {
            Reply::Decision(Decision::ProvenNotCommittedRollbackSafe)
                if noexec_prepare(st).is_some() =>
            {
                Some(Recovery::Cancelled)
            }
            Reply::Committed(_) | Reply::RestartCommitted(_) if completed_inactive(st) => {
                Some(Recovery::Completed)
            }
            Reply::ColdRestartCommitted { .. }
                if completed_inactive(st)
                    && st.native_completed_prepare.as_ref().is_some_and(|prepare| {
                        matches!(prepare.plan, NativePlan::Restart { cold: Some(_), .. })
                    }) =>
            {
                Some(Recovery::Completed)
            }
            _ => None,
        };
    }
    if let Some(noexec) = &st.native_noexec_prepare {
        let active = retained_prepare(st).ok()?;
        if noexec.id == active.id {
            return None;
        }
    }
    if matches!(
        reply,
        Reply::Decision(Decision::Retired(native_act::Retirement::NotGranted))
    ) {
        st.native_retired_unissued = true;
        return Some(Recovery::Held);
    }
    let outcome = match reply {
        Reply::Decision(Decision::ProvenNotCommittedRollbackSafe) => Recovery::Cancelled,
        Reply::Committed(_) | Reply::RestartCommitted(_) | Reply::ColdRestartCommitted { .. } => {
            Recovery::Completed
        }
        Reply::Decision(Decision::Retired(_)) => Recovery::Retired,
        _ => return None,
    };
    let prepare = retained_prepare(st).ok()?;
    if matches!(reply, Reply::ColdRestartCommitted { .. })
        && !matches!(&prepare.plan, NativePlan::Restart { cold: Some(_), .. })
    {
        return None;
    }
    let noexec = if outcome == Recovery::Cancelled {
        if st
            .native_completed_prepare
            .as_ref()
            .is_some_and(|done| done.id == prepare.id)
        {
            return None;
        }
        Some(prepare.clone())
    } else {
        None
    };
    if outcome == Recovery::Completed {
        st.native_completed_prepare = Some(prepare);
        st.native_observation = match reply {
            Reply::RestartCommitted(observation) => Some((**observation).clone()),
            Reply::ColdRestartCommitted { observation, .. } => observation.as_deref().cloned(),
            _ => None,
        };
        if let Reply::ColdRestartCommitted { intent, .. } = reply {
            st.account_intent = Some((**intent).clone());
        }
    }
    st.native = None;
    st.native_prepare.clear();
    st.native_noexec_prepare = noexec;
    // Completion retains the phase of the command that actually ran. The
    // renewal caller and durable observation must distinguish a live rollback
    // vendor from a successful forward replacement after this capability closes.
    if outcome != Recovery::Completed {
        st.native_rollback = false;
    }
    if outcome == Recovery::Cancelled
        && matches!(st.phase, super::upgrade::Phase::Relaunched { .. })
    {
        st.stop("native-cancelled-before-mutation", upgrade_drive::now_s());
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

#[cfg(test)]
pub(super) fn terminal_for_test(st: &mut St, reply: &Reply) -> Option<Recovery> {
    terminal_reply(st, reply)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeRetirementDebt",
        action = "Unknown",
        project = "aterm_agent::harness::native_restart::retirement_debt_tests::transition"
    )
)]
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
    let asked = if duty == Scope::Upgrade && coordinated(st) {
        if st.native.is_some()
            || !st.native_prepare.is_empty()
            || st.native_rollback
            || st.native_retired_unissued
            || st.native_account_candidate.is_some()
        {
            return Err("native-unresolved-operation");
        }
        let previous = st
            .native_completed_prepare
            .as_deref()
            .ok_or("native-asked-predecessor-unproven")?;
        Some(AskedIntent::Upgrade(AskedUpgradeIntent {
            predecessor: previous.id.clone(),
        }))
    } else {
        None
    };
    begin_plan(
        opts,
        client,
        st,
        session,
        duty,
        target,
        command,
        rollback,
        None,
        None,
        asked.as_ref(),
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn begin_relaunch(
    opts: &Opts,
    client: &mut Client,
    st: &mut St,
    session: &str,
    duty: Scope,
    target: &std::path::Path,
    command: Option<&super::relaunch::PlannedCommand>,
    rollback: Option<&super::relaunch::PlannedCommand>,
    checked: Option<(
        &super::relaunch::CheckedSource,
        &dyn super::relaunch::RelaunchAuthorizer,
    )>,
) -> Result<Recovery, &'static str> {
    if !coordinated(st) {
        return begin(opts, client, st, session, duty, target, command, rollback);
    }
    if checked.is_none() {
        return Err("native-checked-source-unavailable");
    }
    if command.is_some_and(|command| {
        matches!(
            command.account_intent.as_ref(),
            Some(super::accounts_pool::ExecutionIntent::Token { .. })
        )
    }) {
        return Err("native-checked-token-route");
    }
    if st.native_completed_prepare.is_none() {
        return Err("native-asked-predecessor-unproven");
    }
    begin_plan(
        opts, client, st, session, duty, target, command, rollback, None, None, None, checked,
    )
}

pub(super) fn authorize_relaunch(
    st: &St,
    source: &super::relaunch::CheckedSource,
    authorizer: &dyn super::relaunch::RelaunchAuthorizer,
) -> Result<Box<dyn Send>, &'static str> {
    let request = retained_prepare(st)?;
    let checked = source
        .checked(&request)
        .ok_or("native-checked-source-changed")?;
    authorizer.authorize(&checked)
}

pub(super) enum RelaunchObservation {
    Terminal(Recovery),
    Signalled,
    Pending,
}

/// Observe exact saved work before asking for any fresh source authorization.
pub(super) fn observe_relaunch(
    opts: &Opts,
    session: &str,
    client: &mut Client,
    tab: &str,
    st: &mut St,
) -> RelaunchObservation {
    if retained_prepare(st).is_err() {
        return RelaunchObservation::Pending;
    }
    if st.native.is_none() {
        // Without a scoped producer permit this cannot admit a missing AskedRelaunch.
        // An accepted lost-ACK operation instead returns its existing exact binding.
        match recover(opts, session, client, tab, st) {
            Ok(Recovery::Admitted) => {}
            Ok(outcome) => return RelaunchObservation::Terminal(outcome),
            Err(_) => return RelaunchObservation::Pending,
        }
    }
    let Some(binding) = &st.native else {
        return RelaunchObservation::Pending;
    };
    let Ok(reply) = native_act::exchange(client, tab, &Request::Query(binding.clone())) else {
        return RelaunchObservation::Pending;
    };
    if let Some(outcome) = terminal_reply(st, &reply) {
        return RelaunchObservation::Terminal(outcome);
    }
    if reply == Reply::Signalled {
        RelaunchObservation::Signalled
    } else {
        RelaunchObservation::Pending
    }
}

pub(super) fn begin_crash(
    opts: &Opts,
    client: &mut Client,
    st: &mut St,
    session: &str,
    snap: &super::relaunch::Snapshot,
    command: Option<&super::relaunch::PlannedCommand>,
    cold: Option<&super::native_act::ColdIntent>,
) -> Result<Recovery, &'static str> {
    if st.native_retired_unissued {
        return Ok(Recovery::Held);
    }
    if cold.is_some() {
        require_cold_constructor(opts, client, st)?;
    } else if legacy_before_prepare(opts, client, st)? {
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
        cold,
        None,
        None,
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
    if st.native_retired_unissued {
        return Ok(Recovery::Held);
    }
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
    // The crashed vendor is the image the forward settled on: a token forward's is the store
    // build aterm's wrapper exec'd in the same pid, never the wrapper
    // ([`native_act::RestartCommand::settled`]).
    if observed.registration.host_epoch != previous.host_epoch
        || observed.registration.vendor.image != *forward.settled()
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
        account_intent: back.account_intent.as_deref().cloned(),
    };
    if command.capture().as_ref() != Some(back.as_ref()) {
        return Err("native-completed-target-changed");
    }
    // The rollback's vendor is another agent than the one `st.pid` named, bound by its pid and
    // kernel start: its exit is what the relaunch waits on, and `exited_at` spoke of the other.
    st.bind(
        observed.registration.vendor.pid,
        &vendor_start(&observed.registration.vendor),
    );
    st.exited_at = 0;
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
        None,
        None,
        None,
    )
}

/// The kernel start (`upgrade_drive::kernel_start`'s spelling) of the native process `vendor`
/// names, what proves its exit once its pid is free or holds a process born at another time. On
/// Apple it is its recorded birth itself (whole microseconds of the start the kernel spells in
/// seconds), whatever the pid holds now; elsewhere it is read only while the pid still names that
/// process (its birth read the same before and after), and empty once it is gone, when only its
/// free pid proves the exit.
pub(super) fn vendor_start(vendor: &super::native_act::Process) -> String {
    #[cfg(target_vendor = "apple")]
    {
        super::footer::lstart_utc(vendor.birth / 1_000_000)
    }
    #[cfg(all(unix, not(target_vendor = "apple")))]
    {
        let born = || {
            i32::try_from(vendor.pid)
                .ok()
                .and_then(aterm_uds::process::read_birth)
                .is_some_and(|birth| birth.0 == vendor.birth)
        };
        if !born() {
            return String::new();
        }
        let start = upgrade_drive::kernel_start(vendor.pid);
        if !born() {
            return String::new();
        }
        start.unwrap_or_default()
    }
    #[cfg(not(unix))]
    {
        let _ = vendor;
        String::new()
    }
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
    cold: Option<&super::native_act::ColdIntent>,
    asked: Option<&AskedIntent>,
    checked: Option<(
        &super::relaunch::CheckedSource,
        &dyn super::relaunch::RelaunchAuthorizer,
    )>,
) -> Result<Recovery, &'static str> {
    if st.native_retired_unissued {
        return Ok(Recovery::Held);
    }
    // CLI/operator constructors preserve their independent legacy authority. Only the real
    // private host opts contain this token; holder/as=person words never select this branch.
    if asked.is_some() || checked.is_some() {
        let constructor = if checked.is_some() {
            Constructor::AskedRelaunch
        } else {
            Constructor::AskedUpgrade
        };
        let expected_duty = if checked.is_some() {
            Scope::Relaunch
        } else {
            Scope::Upgrade
        };
        if duty != expected_duty || cold.is_some() || observed.is_some() || opts.token.is_none() {
            return Err("native-asked-upgrade-unqualified");
        }
        if !matches!(native_act::exchange(client, &st.tab, &Request::Constructors),
            Ok(Reply::Constructors(enabled)) if enabled.contains(&constructor))
        {
            return Err("native-asked-constructor-disabled");
        }
    } else if cold.is_some() {
        require_cold_constructor(opts, client, st)?;
    } else if legacy_before_prepare(opts, client, st)? {
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
            .and_then(|command| {
                if asked.is_some() || checked.is_some() {
                    command.capture_native()
                } else {
                    command.capture()
                }
            })
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
        let mut prepare = Prepare {
            asked: asked.cloned(),
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
                cold: cold.cloned().map(Box::new),
                rollback: rollback.map(Box::new),
            },
        };
        if let Some((source, _)) = checked {
            if !source.agrees(&prepare) {
                return Err("native-checked-source-changed");
            }
            let predecessor = st
                .native_completed_prepare
                .as_ref()
                .ok_or("native-asked-predecessor-unproven")?
                .id
                .clone();
            prepare.asked = Some(AskedIntent::Relaunch(native_act::AskedRelaunchIntent {
                predecessor,
                ask: source.ask().to_owned(),
            }));
        }
        let request = Request::prepare(prepare);
        st.native_prepare = request.encode().ok_or("native-plan")?;
        st.native_noexec_prepare = None;
        st.native_coordination = true;
    }
    let _checked_authorization = if let Some((source, host)) = checked {
        // The producer can authorize only a recipe whose actual durability barrier succeeded.
        upgrade_drive::save_checked(opts, session, st).map_err(|_| "native-unsaved")?;
        Some(authorize_relaunch(st, source, host)?)
    } else {
        None
    };
    let tab = st.tab.clone();
    let recovered = recover(opts, session, client, &tab, st)?;
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

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeReplayPublication",
        action = "FailBarrier",
        reason = "The injected filesystem refuses the directory durability barrier"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeReplayPublication",
        action = "RepairBarrier",
        reason = "The injected filesystem resumes acknowledging directory durability"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeReplayPublication",
        action = "DryRun",
        reason = "The caller supplies options that forbid native dispatch"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeReplayPublication",
        action = "LiveRun",
        reason = "The caller supplies ordinary execution options"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeReplayPublication",
        action = "Hold",
        reason = "A positive retirement receipt has established the retained hold"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeReplayPublication",
        action = "Attempt",
        project = "aterm_agent::harness::native_restart::transport_tests::replay_model::transition"
    )
)]
pub(super) fn recover(
    opts: &Opts,
    session: &str,
    client: &mut Client,
    tab: &str,
    st: &mut St,
) -> Result<Recovery, &'static str> {
    if st.native_retired_unissued {
        return Ok(Recovery::Held);
    }
    if opts.dry_run {
        return Err("native-dry-run");
    }
    if noexec_prepare(st).is_some() && st.tab == tab {
        upgrade_drive::save_checked(opts, session, st).map_err(|_| "native-unsaved")?;
        return Ok(Recovery::Cancelled);
    }
    let prepare = retained_prepare(st)?;
    let expected_id = prepare.id.clone();
    let request = Request::Prepare(prepare);
    // Prepare replay can admit an operation whose earlier request never arrived.
    // Reconfirm persistence on every attempt, including after a lost directory
    // barrier acknowledgement; a visible or in-memory record is insufficient.
    upgrade_drive::save_checked(opts, session, st).map_err(|_| "native-unsaved")?;
    let reply = native_act::exchange_retained(client, tab, &request, &st.native_prepare).map_err(
        |error| {
            aterm_log::debug!("native prepare replay error kind={:?}", error.kind());
            "native-unknown"
        },
    )?;
    if matches!(&reply, Reply::Admitted(binding) | Reply::Pending(binding) if binding.id != expected_id)
    {
        return Err("native-unknown");
    }
    recover_reply(st, reply)
}

/// Check the retained wire before a binding can consume or change its debt.
/// This binds the operation ID, not every field of a server-side receipt.
pub(super) fn retained_prepare(st: &St) -> Result<Box<Prepare>, &'static str> {
    let request = Request::parse(&st.native_prepare).ok_or("native-plan")?;
    if !request.matches_retained(&st.native_prepare) {
        return Err("native-plan");
    }
    let Request::Prepare(prepare) = request else {
        return Err("native-plan");
    };
    if st
        .native
        .as_ref()
        .is_some_and(|binding| binding.id != prepare.id)
    {
        return Err("native-plan");
    }
    Ok(prepare)
}

/// A positive terminal fact, never an inference from absent execution evidence.
/// Ordinary lifecycle changes may follow it; they do not reopen its capability.
pub(super) fn noexec_prepare(st: &St) -> Option<&Prepare> {
    let prepare = st.native_noexec_prepare.as_deref()?;
    Request::Prepare(Box::new(prepare.clone())).encode()?;
    (st.native_coordination
        && st.native.is_none()
        && st.native_prepare.is_empty()
        && !st.native_retired_unissued
        && st.native_account_candidate.is_none()
        && !st.native_rollback
        && st
            .native_completed_prepare
            .as_ref()
            .is_none_or(|done| done.id != prepare.id)
        && (st.native_completed_prepare.is_some()
            || (st.native_observation.is_none()
                && !matches!(st.phase, super::upgrade::Phase::Relaunched { .. }))))
    .then_some(prepare)
}

fn completed_inactive(st: &St) -> bool {
    st.native_completed_prepare
        .as_ref()
        .is_some_and(|prepare| Request::Prepare(prepare.clone()).encode().is_some())
        && st.native_noexec_prepare.is_none()
        && st.native.is_none()
        && st.native_prepare.is_empty()
        && !st.native_retired_unissued
        && st.native_account_candidate.is_none()
        && matches!(
            st.phase,
            super::upgrade::Phase::Relaunched { .. } | super::upgrade::Phase::Done
        )
}

pub(super) fn abort(
    opts: &Opts,
    session: &str,
    client: &mut Client,
    tab: &str,
    st: &mut St,
) -> bool {
    match recover(opts, session, client, tab, st) {
        Ok(Recovery::Cancelled) => return true,
        Ok(Recovery::Completed | Recovery::Retired | Recovery::Held | Recovery::Legacy)
        | Err(_) => return false,
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

fn query_observation(
    client: &mut Client,
    tab: &str,
    st: &mut St,
) -> Result<Option<Reply>, &'static str> {
    if st.native_retired_unissued {
        return Ok(None);
    }
    let Some(binding) = st.native.clone() else {
        return Ok(None);
    };
    retained_prepare(st)?;
    let reply = native_act::exchange(client, tab, &Request::Query(binding))
        .map_err(|_| "native-recovery")?;
    // Even a probe for a different condition has received this irreversible
    // observation. Do not discard it and later consume the debt on a timeout.
    if matches!(
        reply,
        Reply::Decision(Decision::Retired(native_act::Retirement::NotGranted))
    ) {
        let _outcome = terminal_reply(st, &reply);
    }
    Ok(Some(reply))
}

pub(super) fn refresh_retirement_hold(
    client: &mut Client,
    tab: &str,
    st: &mut St,
) -> Result<bool, &'static str> {
    let _observation = query_observation(client, tab, st)?;
    Ok(st.native_retired_unissued)
}

pub(super) fn launch_unissued(
    client: &mut Client,
    tab: &str,
    st: &mut St,
) -> Result<bool, &'static str> {
    Ok(matches!(query_observation(client, tab, st)?,
        Some(Reply::Unissued(unissued)) if unissued == phase(st)))
}

pub(super) fn complete(
    opts: &Opts,
    session: &str,
    client: &mut Client,
    tab: &str,
    st: &mut St,
) -> bool {
    if st.native_retired_unissued {
        return false;
    }
    let Some(binding) = st.native.clone() else {
        return if coordinated(st) {
            completed_inactive(st)
        } else {
            st.native_prepare.is_empty()
        };
    };
    let Ok(prepare) = retained_prepare(st) else {
        return false;
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
                if !matches!(
                    prepare.plan,
                    NativePlan::Restart {
                        rollback: Some(_),
                        ..
                    }
                ) {
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
    if let Ok(reply) =
        native_act::exchange(client, tab, &Request::Precommit(binding.clone(), phase))
        && let Some(outcome) = terminal_reply(st, &reply)
    {
        if outcome == Recovery::Cancelled {
            st.stop("native-cancelled-before-mutation", upgrade_drive::now_s());
        }
        return outcome == Recovery::Completed;
    }
    native_act::exchange(client, tab, &Request::Commit(binding, phase, st.to.clone()))
        .ok()
        .and_then(|reply| terminal_reply(st, &reply))
        == Some(Recovery::Completed)
}

pub(super) fn resume_observed(
    client: &mut Client,
    tab: &str,
    st: &mut St,
) -> Result<bool, &'static str> {
    Ok(match query_observation(client, tab, st)? {
        Some(Reply::Resumed(done)) => done == phase(st),
        Some(reply) => terminal_reply(st, &reply) == Some(Recovery::Completed),
        _ => false,
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

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeReplayPublication",
        action = "Attempt",
        project = "aterm_agent::harness::native_restart::transport_tests::replay_model::retirement_transition"
    )
)]
pub(super) fn retire(
    opts: &Opts,
    session: &str,
    client: &mut Client,
    tab: &str,
    st: &mut St,
) -> bool {
    if st.native_retired_unissued {
        return false;
    }
    if st.native.is_none() && st.native_prepare.is_empty() {
        return true;
    }
    if opts.dry_run {
        return false;
    }
    if st.native.is_none() && !st.native_prepare.is_empty() {
        // A lost Prepare reply can leave only the saved exact request. Replay
        // that request to recover its binding before reconciling; never mint a
        // replacement id or signal/resume a command merely to close it.
        match recover(opts, session, client, tab, st) {
            Ok(Recovery::Admitted) => {}
            Ok(Recovery::Retired | Recovery::Cancelled) => return true,
            _ => return false,
        }
    }
    let Some(binding) = st.native.clone() else {
        return st.native_prepare.is_empty();
    };
    if retained_prepare(st).is_err() {
        return false;
    }
    // Reconcile can seal this operation. A loaded binding may be visible after
    // an unacknowledged rename, so it needs this writer's durability barrier too.
    if upgrade_drive::save_checked(opts, session, st).is_err() {
        return false;
    }
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
        let back = st.signalled(4242, "", 400, "tab", "exact-forward".into(), 8);
        st.native_prepare = super::retirement_debt_tests::pending().native_prepare;
        st.native_coordination = true;
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
