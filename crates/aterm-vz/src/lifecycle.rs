// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The steps of `VzLifecycle` (`aterm_models_misc::vz_lifecycle_model`) that no function of
//! this crate performs. The lifetime binds where the real code runs: the run files on
//! `helper::take`, the sessions, `keep`, the release rule, the boot, the hello and the stops
//! on the core's own functions, a command on `Session::connect`, the core's connect and
//! `agent::run`, the leftovers and the escalation on `record::remove_leftovers`,
//! `record::kill_helper` and the lab's escalation, and the disks on
//! [`crate::layers::Layers::prepare`] and [`crate::layers::PartialBake::seal`], each projected
//! by the conformance (`tests/conformance_lifecycle.rs`). Test and debug builds only.

/// The lab's bring-up lock, the world's steps, and the mutants of `VzLifecycle`.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "vz_lifecycle",
        action = "UpLocks",
        reason = "The lab's bring-up lock (run/bring-up.lock, aterm-lab's linux_vz_host::bring_up_lock over record::DiskLock): the lab's, not this crate's. The conformance takes and drops the same flock exactly where the model's up does, outside any step's window, since helper::take's own DiskLock is the Start's."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "vz_lifecycle",
        action = "UpRefused",
        reason = "A second up refused the bring-up lock (NOT RUN, naming it): the lab's. The conformance's second try_take answers None exactly where the model's up holds it."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "vz_lifecycle",
        action = "UpUnlocks",
        reason = "The up's end lets the bring-up lock go, and a prepared boot it never made with it: the lab's. The conformance drops its lock and its plan exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "vz_lifecycle",
        action = "Hang",
        reason = "A helper that stops answering (alive, its socket gone or silent): the world's. The conformance's stand-in, this test binary holding the run files as the helper does, never answers."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "vz_lifecycle",
        action = "Die",
        reason = "A helper that dies with its run files left (its VM process exits by itself, fact 44): the world's. The conformance SIGKILLs its stand-in."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "vz_lifecycle",
        action = "PidReused",
        reason = "A dead helper's pid taken by another process: the world's. The conformance's record names its live stand-in's pid with another unique id."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "vz_lifecycle",
        action = "SecondBoots",
        reason = "Buggy=1 negative control only: helper::take fails the live disk's flock while another helper holds it, before any framework sees the disk (fault NoFlock locks a file of its own)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "vz_lifecycle",
        action = "ClonePartial",
        reason = "Buggy=1 negative control only: Layers::prepare removes every partial file before it reads a directory and clones only a sealed baked disk (fault ClonePartial)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "vz_lifecycle",
        action = "AttachBaked",
        reason = "Buggy=1 negative control only: Layers::prepare hands out only the live disk or a bake's partial file, never the sealed baked disk (fault AttachBaked)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "vz_lifecycle",
        action = "PrepareUnderVm",
        reason = "Buggy=1 negative control only: Session::bring_up calls its prepare only while its session found no VM; the conformance plays a lab without that gate and sees the disk pulled"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "vz_lifecycle",
        action = "UpWithoutLock",
        reason = "Buggy=1 negative control only: an up prepares only under the bring-up lock; the conformance plays a second up without it and sees the first's prepared disk pulled"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "vz_lifecycle",
        action = "DropExit",
        reason = "Buggy=1 negative control only: agent::run returns at the command's Exit (fault DropExit reads past it)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "vz_lifecycle",
        action = "KeepRunning",
        reason = "Buggy=1 negative control only: the core's release rule stops the VM once the last session closes with keep unset (fault KeepRunning)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "vz_lifecycle",
        action = "HardUnasked",
        reason = "Buggy=1 negative control only: the core stops a VM only for a detector, a Stop or the release rule (fault HardOnClose stops it hard at a session's close)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "vz_lifecycle",
        action = "SignalReused",
        reason = "Buggy=1 negative control only: record::holder and record::kill_helper read the pid's unique id again and match it to the record's (fault PidOnly)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "vz_lifecycle",
        action = "ReadyUnchecked",
        reason = "Buggy=1 negative control only: a watch dial whose first frame is no keyed hello is refused, and the core ignores a Ready before the agent's hello (fault ReadyBeforeHello)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "vz_lifecycle",
        action = "ExecBooting",
        reason = "Buggy=1 negative control only: the core's connect_port opens a command connection only on a Ready VM (fault ConnectBooting opens one while it boots)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "vz_lifecycle",
        action = "SealHalfMade",
        reason = "Buggy=1 negative control only: a bake is sealed only once its VM was Ready and its helper reported it stopped with no detector asking (§4); PartialBake::seal seals what it is handed, and the choice is the lab's, so the conformance plays a lab that seals whatever stopped the bake's VM"
    )
)]
#[doc(hidden)]
pub fn vz_lifecycle_environment() {}
