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

// The pending fixture encodes a restart plan, whose Unix image path only a Unix host observes
// ([`native_act::Image::at`]); the plan's wire refuses any other.
#[cfg(all(test, unix))]
#[path = "native_retirement_debt_tests.rs"]
mod retirement_debt_tests;

// The closed fixtures name a restart plan and a live Unix shell, as the pending fixture does.
#[cfg(all(test, unix))]
#[path = "native_closed_tests.rs"]
pub(super) mod closed_tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Recovery {
    Legacy,
    Admitted,
    Cancelled,
    Completed,
    Retired,
    Held,
    /// The host proved it never admitted the saved prepare, which is dropped: nothing ran.
    Unadmitted,
    /// The host proved no exec grant was or can be issued under the saved restart prepare
    /// (`state=lapsed`), which is dropped with its binding. Unlike [`Self::Unadmitted`] it
    /// claims nothing about the operation's other effects: it may have signalled the old agent
    /// or typed its launcher line, so its lane ends and a later pass plans afresh.
    Lapsed,
    /// The host proved the saved restart reached its close uncommitted (`state=closed`): no host
    /// epoch will commit, roll back or recover it, and an exec grant under it may have run. Its
    /// prepare and binding are dropped and the record keeps the closed prepare as a watch
    /// ([`St::native_closed`]): the lane adopts the relaunched agent it observes, or ends once
    /// nothing the restart may have started runs, and plans nothing afresh before
    /// ([`closed_watch`]).
    Closed,
}

pub(super) const WAIT_RETIRED_UNISSUED: &str = "wait:native-retired-unissued";
/// The wait of a lane whose saved restart closed ([`Recovery::Closed`]) while its watch holds.
pub(super) const WAIT_CLOSED: &str = "wait:native-closed";
/// The end of a lane whose closed restart's watch saw nothing it may have started run: the shell
/// it typed at is back with no child running an image the restart's grant may run, so nothing is
/// left to adopt.
pub(super) const FAILED_CLOSED: &str = "failed:native-closed";
/// The refusal of a fresh plan while a closed restart's watch is kept ([`St::native_closed`]): a
/// process its exec grant started may still run.
pub(super) const CLOSED_GRANTEE: &str = "native-closed-grantee";
/// The lane's wait on [`CLOSED_GRANTEE`], as `wait:` spells every refusal word.
pub(super) const WAIT_CLOSED_GRANTEE: &str = "wait:native-closed-grantee";
/// The wait of a lane whose saved prepare the host proved never admitted ([`Recovery::Unadmitted`]).
pub(super) const WAIT_UNADMITTED: &str = "wait:native-unadmitted";
/// The wait of a lane whose saved prepare the host proved can grant no exec ([`Recovery::Lapsed`]).
pub(super) const WAIT_LAPSED: &str = "wait:native-lapsed";
/// A positive safe-cancellation reply, translated by a constructor's refusal path.
pub(super) const WAIT_CANCELLED: &str = "wait:native-cancelled";
/// A retained operation closed with positive proof that its mutation did not happen.
pub(super) const WAIT_CANCELLED_BEFORE_MUTATION: &str = "wait:native-cancelled-before-mutation";
/// A retained operation safely closed before its signal step.
pub(super) const WAIT_ABORTED_BEFORE_SIGNAL: &str = "wait:native-aborted-before-signal";

/// The wait word of a [`begin`] that admitted nothing: its refusal, or the answer that dropped
/// its prepare. A cancellation is `begin`'s own refusal word.
pub(super) const fn refusal_word(admission: Result<Recovery, &'static str>) -> &'static str {
    match admission {
        Err(why) => why,
        Ok(Recovery::Unadmitted) => "native-unadmitted",
        Ok(Recovery::Lapsed) => "native-lapsed",
        Ok(Recovery::Cancelled) => "native-cancelled",
        Ok(Recovery::Closed) => "native-closed",
        Ok(
            Recovery::Legacy
            | Recovery::Admitted
            | Recovery::Completed
            | Recovery::Retired
            | Recovery::Held,
        ) => "native-unresolved-operation",
    }
}

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
        || st.native_closed.is_some()
        || super::native_record::was_coordinated(st)
}

/// Refuse a fresh plan while the record keeps a closed restart's watch ([`St::native_closed`]):
/// an exec grant of that restart may have started a process that still runs, which only
/// [`closed_watch`] read by a lane with a kernel may adopt or rule out.
const fn closed_refusal(st: &St) -> Result<(), &'static str> {
    if st.native_closed.is_some() {
        Err(CLOSED_GRANTEE)
    } else {
        Ok(())
    }
}

/// What a closed restart's watch reads now ([`closed_watch`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Watch {
    /// A live process registered on the conversation is the closed restart's relaunch: a child
    /// of its shell other than its old agent, while that shell no longer holds the tab, or, once
    /// that shell is gone, the conversation's one live registered holder. Adopting it takes the
    /// place of the commit the host never made.
    Adopt(Box<super::upgrade::SessionFile>),
    /// Nothing the closed restart may have started runs: its shell, still the process it named,
    /// has no live child that runs an image the restart's grant may run, its other children (a
    /// prompt helper, a job of the person's) being no grantee; or that shell is gone, its old
    /// agent too, and no live process holds the conversation; or the machine booted since.
    Clear,
    /// Anything else, a fact that did not read included: a process the restart's exec grant
    /// started may still run, so nothing plans afresh.
    Hold,
}

/// Read the watch of `closed`, a restart that reached its close uncommitted ([`Recovery::Closed`]):
/// `files` are the registered session files (`None`: unread), and `conversation` the session id
/// its relaunch registers (`None`: a fresh start's, whose agent registers another). It reads the
/// restart's own shell and old agent from its plan, never the record's current ones, so a record
/// moved on since judges the same processes. A later boot than the one the prepare names ended
/// them all. While the shell is the process it typed at, the grantee is its child that runs the
/// restart's launcher or a command its grant execs ([`grant_images`]), and no other child of it;
/// once it is gone by its kernel start it led its terminal's session, whose hangup ends a
/// foreground job that holds no conversation, so only the conversation's live registered holder
/// can be the grantee ([`orphaned`]). It grants nothing.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeClosedWatch",
        action = "Adopt",
        project = "aterm_agent::harness::native_restart::closed_tests::watch_model::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeClosedWatch",
        action = "Clear",
        project = "aterm_agent::harness::native_restart::closed_tests::watch_model::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeClosedWatch",
        action = "Launch",
        reason = "The line the restart typed starts its grantee as the shell's child, and only before the close: the host delivers no grant past the deadline"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeClosedWatch",
        action = "Register",
        reason = "The grantee writes its session file on the conversation; the watch reads the registered files"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeClosedWatch",
        action = "Exit",
        reason = "The grantee exits, and its shell takes its tab back; the kernel reads the shell's children and its tab"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeClosedWatch",
        action = "Spawn",
        reason = "The shell keeps another child that runs no image the restart's grant may run, a prompt helper or a job of the person's; the kernel reads the shell's children and each one's mapped image"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeClosedWatch",
        action = "SpawnAlike",
        reason = "The person starts, in the restart's shell, another child that runs an image the restart's grant may run; the kernel reads its mapped image, which the watch cannot tell from the grantee's"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeClosedWatch",
        action = "OtherExits",
        reason = "The shell's other child exits; the kernel reads the shell's children"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeClosedWatch",
        action = "ShellExits",
        reason = "The restart's shell, its terminal's session leader, exits or is reaped; the kernel reads its pid free or born at another time"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeClosedWatch",
        action = "Reboot",
        reason = "The machine boots again; the kernel reads another boot than the prepare's stamp"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeClosedWatch",
        action = "Close",
        reason = "The host answers closed for the restart past its close, which terminal_reply keeps as the record's watch (NativeRetirementDebt's Closed)"
    )
)]
pub(super) fn closed_watch(
    closed: &Prepare,
    k: &dyn upgrade_drive::Kernel,
    files: Option<&[super::upgrade::SessionFile]>,
    conversation: Option<&str>,
) -> Watch {
    let NativePlan::Restart {
        process: old,
        shell,
        launcher,
        forward,
        rollback,
        ..
    } = &closed.plan
    else {
        return Watch::Hold;
    };
    if closed
        .boot
        .as_ref()
        .is_some_and(|then| k.machine_boot().is_some_and(|now| now != *then))
    {
        return Watch::Clear;
    }
    match runs(shell, k) {
        Some(true) => {}
        Some(false) => return orphaned(old, k, files, conversation),
        None => return Watch::Hold,
    }
    // Unread files hide no relaunch from the children's read below, which they cannot clear.
    if let Some(relaunched) = files.into_iter().flatten().find(|sf| {
        conversation.is_none_or(|conversation| sf.session_id == conversation)
            && sf.pid != old.pid
            && super::relaunch::relaunch_by_parent(k.parent(sf.pid), shell.pid)
                == super::relaunch::Relaunch::Ours
    }) {
        return if k.shell_holds_tab(shell.pid) == Some(false) {
            Watch::Adopt(Box::new(relaunched.clone()))
        } else {
            Watch::Hold
        };
    }
    // The line the restart typed runs as the shell's child, and the host grants an exec only to
    // that child while it runs the restart's launcher: with that same shell alive none of its
    // children was reparented away, so the grantee, if it runs, is a child of the shell running
    // the launcher or a command the grant execs ([`grant_images`]). Past the close no grant is
    // delivered, so a child that runs none of those (a prompt helper, a job of the person's, a
    // fork of the shell that never reached the launcher) never becomes the agent: once every
    // child reads as one, the restart left nothing running, whoever holds the conversation now.
    // A child whose image does not read may be the grantee.
    let images = grant_images(launcher, forward, rollback.as_deref());
    match k.children(shell.pid) {
        Some(children)
            if children.iter().all(|&child| {
                k.image_file(child)
                    .is_some_and(|image| !images.contains(&image))
            }) =>
        {
            Watch::Clear
        }
        _ => Watch::Hold,
    }
}

/// The images a closed restart's grantee may run while it lives, by file identity: the restart's
/// `launcher` until its Execute grant, then the `forward` or `rollback` command that grant execs
/// in the same pid, the command's own image and the image it settles on
/// ([`native_act::RestartCommand::settled`]). A cold restart's resolved command agrees with its
/// forward seed on both ([`native_act::RestartCommand::cold_resolution_agrees`]), and a source
/// constructor's command with the plan's, so the plan names every image a grant may run.
fn grant_images(
    launcher: &Image,
    forward: &native_act::RestartCommand,
    rollback: Option<&native_act::RestartCommand>,
) -> Vec<super::agent_image::FileId> {
    let id = |image: &Image| super::agent_image::FileId {
        dev: image.device,
        ino: image.inode,
    };
    std::iter::once(id(launcher))
        .chain(
            std::iter::once(forward)
                .chain(rollback)
                .flat_map(|command| [id(&command.image), id(command.settled())]),
        )
        .collect()
}

/// Whether `process`'s pid still names it by its kernel start (`None`: not read): a free pid, a
/// zombie or a pid that names a process born at another time is its exit.
fn runs(process: &Process, k: &dyn upgrade_drive::Kernel) -> Option<bool> {
    if k.holds_pid(process.pid) == Some(false) {
        return Some(false);
    }
    let now = k.process_start(process.pid)?;
    let start = vendor_start(process);
    (!start.is_empty()).then(|| upgrade_drive::squash(&now) == upgrade_drive::squash(&start))
}

/// Whether the process a session file names is the one that wrote it (`None`: not read): alive
/// with the kernel start the file records.
fn registered_runs(
    sf: &super::upgrade::SessionFile,
    k: &dyn upgrade_drive::Kernel,
) -> Option<bool> {
    if k.holds_pid(sf.pid) == Some(false) {
        return Some(false);
    }
    let now = k.process_start(sf.pid)?;
    let start = upgrade_drive::squash(&sf.proc_start);
    (!start.is_empty()).then(|| upgrade_drive::squash(&now) == start)
}

/// The watch once the restart's shell is gone by its kernel start ([`closed_watch`]), on the boot
/// the restart ran in. That shell led its terminal's session, and its exit hung the terminal up:
/// the grantee, its foreground job, ends with it unless it outlives the hangup, and then only a
/// grantee registered on the conversation is a second agent of the lane. Adopt the
/// conversation's one live registered holder; Clear once none holds it and the old agent is gone
/// by its start; Hold where a fact did not read or two processes hold it. A fresh start's watch
/// (`conversation` `None`) reads no conversation: its grantee, if it outlived its terminal,
/// runs on one of its own that no fresh plan of the lane resumes.
fn orphaned(
    old: &Process,
    k: &dyn upgrade_drive::Kernel,
    files: Option<&[super::upgrade::SessionFile]>,
    conversation: Option<&str>,
) -> Watch {
    if runs(old, k) != Some(false) {
        return Watch::Hold;
    }
    let Some(conversation) = conversation else {
        return Watch::Clear;
    };
    let Some(files) = files else {
        return Watch::Hold;
    };
    let mut holder = None;
    for sf in files.iter().filter(|sf| sf.session_id == conversation) {
        match registered_runs(sf, k) {
            Some(false) => {}
            Some(true) if holder.is_none() => holder = Some(sf),
            Some(true) | None => return Watch::Hold,
        }
    }
    holder.map_or(Watch::Clear, |sf| Watch::Adopt(Box::new(sf.clone())))
}

/// What a fresh plan is about to do while the record may keep a closed restart's watch
/// ([`settle_closed`]).
#[derive(Clone, Copy, Debug)]
pub(super) enum Fresh<'a> {
    /// A move signals the live agent `pid`, which holds `conversation`.
    Signal { pid: u32, conversation: &'a str },
    /// The exit lane starts the conversation's agent again (`conversation`: `None` where the
    /// closed restart started its agent afresh, on another conversation).
    Start { conversation: Option<&'a str> },
}

/// Settle the record's closed restart watch ([`St::native_closed`]) before a fresh plan: `Ok`
/// where none is kept or it is cleared now, the plan going on; else the refusal it takes instead.
/// The watch clears where nothing the closed restart may have started runs, or, before a signal,
/// where its relaunch is the very agent the move signals, observed and the move's own subject
/// rather than a second process. A relaunch that holds the conversation keeps it for the exit
/// lane, which would start another. `files` reads the registered session files only when a watch
/// is kept.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeClosedWatch",
        action = "Start",
        project = "aterm_agent::harness::native_restart::closed_tests::watch_model::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeClosedWatch",
        action = "SignalAdopted",
        project = "aterm_agent::harness::native_restart::closed_tests::watch_model::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeClosedWatch",
        action = "SignalOther",
        project = "aterm_agent::harness::native_restart::closed_tests::watch_model::project"
    )
)]
pub(super) fn settle_closed(
    st: &mut St,
    fresh: Fresh<'_>,
    files: impl FnOnce() -> Option<Vec<super::upgrade::SessionFile>>,
    k: &dyn upgrade_drive::Kernel,
) -> Result<(), &'static str> {
    let Some(closed) = st.native_closed.as_deref() else {
        return Ok(());
    };
    let (conversation, signalled) = match fresh {
        Fresh::Signal { pid, conversation } => (Some(conversation), Some(pid)),
        Fresh::Start { conversation } => (conversation, None),
    };
    match closed_watch(closed, k, files().as_deref(), conversation) {
        Watch::Clear => {}
        Watch::Adopt(relaunched) if Some(relaunched.pid) == signalled => {}
        Watch::Adopt(_) | Watch::Hold => return Err(CLOSED_GRANTEE),
    }
    st.native_closed = None;
    Ok(())
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
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeRetirementDebt",
        action = "Closed",
        project = "aterm_agent::harness::native_restart::retirement_debt_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "ColdRetirementRecovery",
        action = "PriorRetirement",
        project = "aterm_agent::harness::native_restart::retirement_debt_tests::cold_transition"
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
            // A repeated closed answer acknowledges the watch it already left, never another id.
            Reply::Closed(id)
                if st
                    .native_closed
                    .as_ref()
                    .is_some_and(|closed| closed.id == *id) =>
            {
                Some(Recovery::Closed)
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
    if let Reply::Closed(id) = reply {
        return closed(st, id);
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

/// Record that the retained restart `id` reached its close uncommitted ([`Recovery::Closed`]): its
/// prepare, binding and rollback go, and the prepare stays as the watch the lane reads
/// ([`closed_watch`]) before anything plans afresh. Nothing is recorded completed, no observation
/// is kept and no rollback stays possible. A cold restart's executed account is unknown, since
/// its host names it only in a committed reply: its forward attribution goes, and the account is
/// learned again from the agent the lane observes. Only the exact retained restart's id may; a
/// terminal record of that id keeps the record as it is.
fn closed(st: &mut St, id: &str) -> Option<Recovery> {
    let prepare = retained_prepare(st).ok()?;
    if prepare.id != id
        || !matches!(prepare.plan, NativePlan::Restart { .. })
        || [&st.native_noexec_prepare, &st.native_completed_prepare]
            .into_iter()
            .flatten()
            .any(|done| done.id == id)
    {
        return None;
    }
    if matches!(prepare.plan, NativePlan::Restart { cold: Some(_), .. }) {
        st.account_intent = None;
    }
    st.native = None;
    st.native_prepare.clear();
    st.native_rollback = false;
    st.native_closed = Some(prepare);
    Some(Recovery::Closed)
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
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeRetirementDebt",
        action = "LoseAdmission",
        reason = "The host's admission reply is lost before the harness reads its binding"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "ColdRetirementRecovery",
        action = "NoGrant",
        project = "aterm_agent::harness::native_restart::retirement_debt_tests::cold_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "ColdRetirementRecovery",
        action = "Unknown",
        project = "aterm_agent::harness::native_restart::retirement_debt_tests::cold_transition"
    )
)]
fn recover_reply(st: &mut St, reply: Reply) -> Result<Recovery, &'static str> {
    if cold_retirement_recheck(st) {
        let id = retained_prepare(st)?.id.clone();
        if matches!(
            &reply,
            Reply::Decision(Decision::Retired(native_act::Retirement::NotGranted))
        ) || matches!(&reply, Reply::Lapsed(found) if *found == id)
        {
            let before = st.clone();
            let outcome = lapsed(st, &id)?;
            st.native_retired_unissued = false;
            super::native_record::authorize_cold_retirement(&before, st);
            return Ok(outcome);
        }
    }
    if let Some(outcome) = terminal_reply(st, &reply) {
        return Ok(outcome);
    }
    match reply {
        Reply::Admitted(binding) => {
            st.native = Some(binding);
            Ok(Recovery::Admitted)
        }
        Reply::Pending(_) => Err("native-predecessors"),
        Reply::Unadmitted(id) => unadmitted(st, &id),
        Reply::Lapsed(id) => lapsed(st, &id),
        _ => Err("native-unknown"),
    }
}

/// Only a plain cold restart may revalidate a stored no-grant hold. Account successor debt,
/// private renewals, and terminal history retain their existing hold semantics.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "ColdRetirementRecovery",
        action = "Cold",
        reason = "The environment supplies the retained recipe's constructor before recovery"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "ColdRetirementRecovery",
        action = "Account",
        reason = "The environment supplies existing account successor debt before recovery"
    )
)]
pub(super) fn cold_retirement_recheck(st: &St) -> bool {
    if st.native_account_candidate.is_some() {
        return false;
    }
    let Ok(prepare) = retained_prepare(st) else {
        return false;
    };
    prepare.duty == Scope::Relaunch
        && prepare.asked.is_none()
        && prepare.constructor() == Constructor::ColdRestart
        && [&st.native_noexec_prepare, &st.native_completed_prepare]
            .into_iter()
            .flatten()
            .all(|done| done.id != prepare.id)
}

/// Drop the saved restart prepare, and its binding, under which the host proved no exec grant
/// was or can be issued. Only the exact retained prepare of a cold or typed asked restart may:
/// the host answers no other, a private restart's renewal included, and a terminal record of the
/// same id is a completion or a positive no-grant. A binding may go too, since the retained
/// prepare already names its operation ([`retained_prepare`]). [`terminal_reply`]'s hold for a
/// retired-unissued record answers first. Nothing is recorded completed, and nothing says the
/// operation's other effects did not happen: its lane ends instead of restoring what came before.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeRetirementDebt",
        action = "Lapsed",
        project = "aterm_agent::harness::native_restart::retirement_debt_tests::transition"
    )
)]
fn lapsed(st: &mut St, id: &str) -> Result<Recovery, &'static str> {
    let prepare = retained_prepare(st).map_err(|_| "native-unknown")?;
    if prepare.id != id
        || !matches!(prepare.plan, NativePlan::Restart { .. })
        || !matches!(
            prepare.constructor(),
            Constructor::ColdRestart | Constructor::AskedUpgrade | Constructor::AskedRelaunch
        )
        || (prepare.asked.is_some() && !prepare.asked_restart_shape())
        || [&st.native_noexec_prepare, &st.native_completed_prepare]
            .into_iter()
            .flatten()
            .any(|done| done.id == id)
    {
        return Err("native-unknown");
    }
    st.native = None;
    st.native_prepare.clear();
    st.native_rollback = false;
    Ok(Recovery::Lapsed)
}

/// Drop the saved prepare the host proved it never admitted. Only a record that kept that exact
/// restart prepare and no binding may: a binding is the receipt of an admission, and a terminal
/// record of the same id is a completion. Nothing is recorded completed; the coordination and
/// any earlier completion stay for the lane that plans afresh.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeRetirementDebt",
        action = "Unadmitted",
        project = "aterm_agent::harness::native_restart::retirement_debt_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeRetirementDebt",
        action = "Ask",
        reason = "The retained incoming recipe selects typed asked data without granting admission or changing passive history"
    )
)]
fn unadmitted(st: &mut St, id: &str) -> Result<Recovery, &'static str> {
    let prepare = retained_prepare(st).map_err(|_| "native-unknown")?;
    if st.native.is_some()
        || prepare.id != id
        || !matches!(prepare.plan, NativePlan::Restart { .. })
        || (prepare.asked.is_some() && !prepare.asked_restart_shape())
        || [&st.native_noexec_prepare, &st.native_completed_prepare]
            .into_iter()
            .flatten()
            .any(|done| done.id == id)
    {
        return Err("native-unknown");
    }
    st.native_prepare.clear();
    st.native_rollback = false;
    Ok(Recovery::Unadmitted)
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
        if st.native_retired_unissued || st.native_account_candidate.is_some() {
            return Err("native-unresolved-operation");
        }
        if st.native.is_some() || !st.native_prepare.is_empty() {
            return replay_retained(opts, client, st, session);
        }
        if st.native_rollback {
            return Err("native-unresolved-operation");
        }
        closed_refusal(st)?;
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
    closed_refusal(st)?;
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
    // A kept operation is answered by its own replay, whatever this pass's plan would need: an
    // earlier host's restore plans no cold intent, and production enables no private restart,
    // so a constructor gate here would hold the record for good ([`replay_retained`]).
    if st.native.is_some() || !st.native_prepare.is_empty() {
        return replay_retained(opts, client, st, session);
    }
    closed_refusal(st)?;
    if cold.is_some() {
        require_cold_constructor(opts, client, st)?;
    } else if legacy_before_prepare(opts, client, st)? {
        return Ok(Recovery::Legacy);
    }
    let observed = Some(observation::captured(opts, snap).ok_or("native-crash-observation")?);
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
    closed_refusal(st)?;
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
    // The kept operation's replay comes before every gate of the plan this call would mint: its
    // answer does not depend on the constructor the new plan would need, and the host judges
    // the replay by its own policy.
    if st.native.is_some() || !st.native_prepare.is_empty() {
        return replay_retained(opts, client, st, session);
    }
    // No plan is minted over a process a closed restart's exec grant may have started.
    closed_refusal(st)?;
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
        Some((registration, _)) if registration.vendor.pid == st.pid => registration.vendor.clone(),
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
    } = native_act::exchange(client, &st.tab, &Request::Info).map_err(|_| "native-host-epoch")?
    else {
        return Err("native-host-epoch");
    };
    let rejection_domain = match native_act::exchange(client, &st.tab, &Request::RejectionDomain) {
        Ok(Reply::RejectionDomain(domain)) => Some(domain),
        Err(error)
            if error.kind() == std::io::ErrorKind::PermissionDenied
                && error.to_string().starts_with("ERR usage: native-act ") =>
        {
            None
        }
        _ => return Err("native-rejection-domain-unavailable"),
    };
    let deadline_us = native_act::monotonic_us()
        .and_then(|now| now.checked_add(120_000_000))
        .ok_or("native-clock")?;
    let mut prepare = Prepare {
        receipt_mode: None,
        rejection_domain,
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
        // What a later host epoch reads to answer `lapsed` for this prepare. The host admits
        // a stamp only when it is its own machine, boot and issuer custody, so a stamp read
        // wrongly here costs this admission, never a grant.
        boot: native_act::BootStamp::current(),
        custody: custody_stamp(&opts.home),
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

/// Replay the retained prepare or binding a new plan would otherwise replace, and save what the
/// answer changed. It runs before any gate of that new plan (a constructor, a cold intent, an
/// owned launcher): the answer is the kept operation's own, and the host judges the replay by
/// its own policy. No call that finds one mints a plan: its caller fixed what it judged before
/// this ran (an exit lane its fresh-start judgement), so a new plan waits for the next pass,
/// which sees the record as this leaves it.
///
/// The host's proof that it never admitted the prepare, or that no exec grant was or can be
/// issued under it, is returned, as is a terminal answer: each caller's arm for it saves its own
/// end, and a save lost here only replays the same answer. A cancellation is the refusal `begin`
/// gives its own; an admission, a pending or unknown answer and an unreadable record keep the
/// refusal, with a recovered binding saved.
fn replay_retained(
    opts: &Opts,
    client: &mut Client,
    st: &mut St,
    session: &str,
) -> Result<Recovery, &'static str> {
    let tab = st.tab.clone();
    replay_retained_at(opts, client, st, session, &tab)
}

/// [`replay_retained`] addressed to `tab`, the tab its host holds now, for a caller that
/// replays before it rewrites the record it hands in (the exit lane's, whose tab may have
/// been reopened under another id).
pub(super) fn replay_retained_at(
    opts: &Opts,
    client: &mut Client,
    st: &mut St,
    session: &str,
    tab: &str,
) -> Result<Recovery, &'static str> {
    let recovered = recover(opts, session, client, tab, st);
    let saved = upgrade_drive::save_checked(opts, session, st);
    if matches!(recovered, Ok(Recovery::Lapsed | Recovery::Closed)) && saved.is_err() {
        return Err("native-unsaved");
    }
    match recovered {
        Ok(
            outcome @ (Recovery::Lapsed
            | Recovery::Unadmitted
            | Recovery::Completed
            | Recovery::Retired
            | Recovery::Held
            | Recovery::Closed),
        ) => Ok(outcome),
        Ok(Recovery::Cancelled) => Err("native-cancelled"),
        Ok(Recovery::Admitted | Recovery::Legacy) | Err(_) => Err("native-unresolved-operation"),
    }
}

/// End a lane whose kept operation lapsed ([`Recovery::Lapsed`]) as its record stands: no exec
/// grant was or can be issued under it, but it may have signalled the agent or typed its
/// launcher line, so the record is never restored as unsent; a later pass plans afresh from what
/// it then observes.
pub(super) fn end_lapsed(
    opts: &Opts,
    r: &upgrade_drive::Report,
    st: &mut St,
    session: &str,
) -> upgrade_drive::Report {
    st.stop("native-lapsed", upgrade_drive::now_s());
    upgrade_drive::save(opts, session, st);
    upgrade_drive::said(r.clone(), WAIT_LAPSED)
}

/// The issuer custody a new prepare names, read without a lock from the vault under this
/// harness's own home ([`super::native_receipts::custody_for_home`]); `None` when none is
/// established or it does not read, which leaves the prepare's absence from the journal
/// unprovable and changes nothing else.
fn custody_stamp(home: &std::path::Path) -> Option<String> {
    #[cfg(unix)]
    {
        super::native_receipts::custody_for_home(home)
            .ok()
            .flatten()
    }
    #[cfg(not(unix))]
    {
        let _ = home;
        None
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
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeReplayPublication",
        action = "Unadmit",
        reason = "The host's clock passes the deadline of a prepare it never admitted"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeReplayPublication",
        action = "Lapse",
        reason = "The host's issuer journal proves no exec grant was or can be issued under a cold or typed asked restart prepare"
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
    if st.native_retired_unissued && !cold_retirement_recheck(st) {
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
        || matches!(&reply, Reply::Unadmitted(id) | Reply::Lapsed(id) | Reply::Closed(id) if *id != expected_id)
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

/// End the retained operation before any signal of it: what ended it, or `None` when it stays.
/// The host's proof that it never admitted the prepare, that no exec grant was or can be issued
/// under it ([`Recovery::Lapsed`]), or a safe cancellation, answering the replay or an Abort.
/// A lapse proves nothing about the operation's other effects, so a caller ends its record as
/// it stands after one ([`end_lapsed`]) and restores it as unsent only after either other end.
pub(super) fn abort(
    opts: &Opts,
    session: &str,
    client: &mut Client,
    tab: &str,
    st: &mut St,
) -> Option<Recovery> {
    match recover(opts, session, client, tab, st) {
        Ok(outcome @ (Recovery::Cancelled | Recovery::Unadmitted | Recovery::Lapsed)) => {
            return Some(outcome);
        }
        // A closed restart ended after its signal, not before: its watch stays with the record.
        Ok(
            Recovery::Completed
            | Recovery::Retired
            | Recovery::Held
            | Recovery::Legacy
            | Recovery::Closed,
        )
        | Err(_) => return None,
        Ok(Recovery::Admitted) => {}
    }
    let binding = st.native.as_ref()?;
    native_act::exchange(client, tab, &Request::Abort(binding.clone()))
        .ok()
        .and_then(|reply| terminal_reply(st, &reply))
        .filter(|outcome| *outcome == Recovery::Cancelled)
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
    poll_completion(opts, session, client, tab, st).completed
}

/// One completion poll ([`poll_completion`]): whether the operation completed and, when it did
/// not, the phase whose launch its Query proved unissued.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Poll {
    pub(super) completed: bool,
    pub(super) unissued: Option<ImagePhase>,
}

/// [`complete`], keeping the phase a Query proved unissued for a caller that paces its Resumes
/// by it: that phase's line may be typed again.
pub(super) fn poll_completion(
    opts: &Opts,
    session: &str,
    client: &mut Client,
    tab: &str,
    st: &mut St,
) -> Poll {
    let mut unissued = None;
    let completed = complete_observing(opts, session, client, tab, st, &mut unissued);
    Poll {
        completed,
        unissued,
    }
}

fn complete_observing(
    opts: &Opts,
    session: &str,
    client: &mut Client,
    tab: &str,
    st: &mut St,
    unissued: &mut Option<ImagePhase>,
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
        if let Reply::Unissued(launch) = &reply {
            *unissued = Some(*launch);
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
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeReplayPublication",
        action = "Attempt",
        project = "aterm_agent::harness::native_restart::transport_tests::replay_model::bare_retirement_transition"
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
    if !st.native_prepare.is_empty() {
        // A lost Prepare reply can leave only the saved exact request. Replay
        // that request to recover its binding before reconciling; never mint a
        // replacement id or signal/resume a command merely to close it. A kept
        // binding replays it too: only that replay can hear that no exec grant
        // was or can be issued under it, which a later host epoch answers to
        // the prepare alone. An admission, or a replay that answers nothing,
        // reconciles the kept binding as before.
        match recover(opts, session, client, tab, st) {
            Ok(Recovery::Admitted) => {}
            // A closed restart is terminal too: its watch, not its binding, stays with the record.
            Ok(
                Recovery::Retired
                | Recovery::Cancelled
                | Recovery::Unadmitted
                | Recovery::Lapsed
                | Recovery::Closed,
            ) => return true,
            Err(_) if st.native.is_some() => {}
            Ok(Recovery::Completed | Recovery::Held | Recovery::Legacy) | Err(_) => return false,
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
        .is_some_and(|outcome| {
            matches!(
                outcome,
                Recovery::Retired | Recovery::Cancelled | Recovery::Closed
            )
        })
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

#[cfg(all(test, unix))]
#[path = "native_restart_tests.rs"]
mod tests;
