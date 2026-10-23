// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host callback trait ([`IdleHost`]) and its `idle_host_trait!` macro for taped and replayed supervision.

use super::*;

macro_rules! idle_host_trait {
    (
        $(#[$doc:meta])*
        pub trait $host:ident: Send + Sync + std::fmt::Debug {
            $($methods:tt)*
        }
    ) => {
        $crate::supervise::run::host_door::idle_host_trait! {
            @munch [$(#[$doc])* $host] [] $($methods)*
        }
    };
    (
        @munch $head:tt [$($done:tt)*]
        $(#[$m:meta])*
        fn $name:ident(&$this:ident $(, $arg:ident: $ty:ty)* $(,)?) $(-> $ret:ty)?;
        $($rest:tt)*
    ) => {
        $crate::supervise::run::host_door::idle_host_trait! {
            @munch $head [$($done)* [[$(#[$m])*] [] $name $this [$($arg: $ty),*] [$($ret)?] ;]]
            $($rest)*
        }
    };
    (
        @munch $head:tt [$($done:tt)*]
        $(#[$m:meta])*
        fn $name:ident(&$this:ident $(, $arg:ident: $ty:ty)* $(,)?) $(-> $ret:ty)? $body:block
        $($rest:tt)*
    ) => {
        $crate::supervise::run::host_door::idle_host_trait! {
            @munch $head [$($done)* [[$(#[$m])*] [] $name $this [$($arg: $ty),*] [$($ret)?] $body]]
            $($rest)*
        }
    };
    (
        @munch $head:tt [$($done:tt)*]
        $(#[$m:meta])*
        act fn $name:ident(&$this:ident $(, $arg:ident: $ty:ty)* $(,)?) $(-> $ret:ty)?;
        $($rest:tt)*
    ) => {
        $crate::supervise::run::host_door::idle_host_trait! {
            @munch $head [$($done)* [[$(#[$m])*] [act] $name $this [$($arg: $ty),*] [$($ret)?] ;]]
            $($rest)*
        }
    };
    (
        @munch $head:tt [$($done:tt)*]
        $(#[$m:meta])*
        act fn $name:ident(&$this:ident $(, $arg:ident: $ty:ty)* $(,)?) $(-> $ret:ty)? $body:block
        $($rest:tt)*
    ) => {
        $crate::supervise::run::host_door::idle_host_trait! {
            @munch $head [$($done)* [[$(#[$m])*] [act] $name $this [$($arg: $ty),*] [$($ret)?] $body]]
            $($rest)*
        }
    };
    (
        @munch [$(#[$doc:meta])* $host:ident]
        [$([[$(#[$m:meta])*] [$($act:ident)?] $name:ident $this:ident [$($arg:ident: $ty:ty),*] [$($ret:ty)?] $body:tt])*]
    ) => {
        $(#[$doc])*
        pub trait $host: Send + Sync + std::fmt::Debug {
            $(
                $(#[$m])*
                fn $name(&$this $(, $arg: $ty)*) $(-> $ret)? $body
            )*
        }

        impl $host for $crate::supervise::tape::host::TapedHost<dyn $host> {
            $(
                $crate::supervise::run::host_door::idle_host_trait! {
                    @taped $name [$($act)?] [$($arg: $ty),*] [$($ret)?]
                }
            )*
        }

        impl $host for $crate::supervise::tape::host::ReplayHost {
            $(
                $crate::supervise::run::host_door::idle_host_trait! {
                    @replay $name [$($arg: $ty),*] [$($ret)?]
                }
            )*
        }

        pub(crate) const HOST_ACTS: &[&str] = &[$($(
            {
                let _act = stringify!($act);
                concat!("host.", stringify!($name))
            },
        )?)*];

        #[cfg(test)]
        pub(crate) const HOST_METHODS: &[&str] = &[$(stringify!($name)),*];

        pub(crate) const HOST_DOORS: &[&str] = &[$(concat!("host.", stringify!($name))),*];
    };
    (@act act) => { true };
    (@act) => { false };
    (@taped $name:ident [$($act:ident)?] [$($arg:ident: $ty:ty),*] []) => {
        fn $name(&self $(, $arg: $ty)*) {
            let inner = self.inner();
            self.tell(
                concat!("host.", stringify!($name)),
                $crate::supervise::run::host_door::idle_host_trait!(@request $($arg),*),
                || inner.$name($($arg),*),
                $crate::supervise::run::host_door::idle_host_trait!(@act $($act)?),
            );
        }
    };
    (@taped $name:ident [$($act:ident)?] [$($arg:ident: $ty:ty),*] [$ret:ty]) => {
        fn $name(&self $(, $arg: $ty)*) -> $ret {
            let request = $crate::supervise::run::host_door::idle_host_trait!(@request $($arg),*);
            let inner = self.inner();
            self.ask(
                concat!("host.", stringify!($name)),
                request,
                || inner.$name($($arg),*),
                $crate::supervise::run::host_door::idle_host_trait!(@act $($act)?),
            )
        }
    };
    (@replay $name:ident [$($arg:ident: $ty:ty),*] []) => {
        fn $name(&self $(, $arg: $ty)*) {
            self.tell(
                concat!("host.", stringify!($name)),
                $crate::supervise::run::host_door::idle_host_trait!(@request $($arg),*),
            );
        }
    };
    (@replay $name:ident [$($arg:ident: $ty:ty),*] [$ret:ty]) => {
        fn $name(&self $(, $arg: $ty)*) -> $ret {
            self.ask(
                concat!("host.", stringify!($name)),
                $crate::supervise::run::host_door::idle_host_trait!(@request $($arg),*),
            )
        }
    };
    (@request $($arg:ident),*) => {
        $crate::supervise::tape::host::request(&[
            $((stringify!($arg), &$arg as &dyn $crate::supervise::tape::host::TapeArg)),*
        ])
    };
}

pub(crate) use idle_host_trait;

idle_host_trait! {
/// Host callbacks invoked by the supervision loop at idle points, walls, and turn boundaries.
pub trait IdleHost: Send + Sync + std::fmt::Debug {
    /// Returns whether the host wants the next idle point.
    fn wants(&self) -> bool;
    /// Returns the earliest instant when the host will next want an idle point.
    fn wants_at(&self) -> Option<Instant> {
        None
    }
    /// Executes the host's pending action at an authoritative idle point.
    act fn at_idle(&self) -> Option<HostStep>;
    /// Executes [`Self::at_idle`] with the loop's [`IdlePoint`] context.
    act fn at_idle_on(&self, point: IdlePoint) -> Option<HostStep> {
        let _ = point;
        self.at_idle()
    }
    /// Returns whether the host currently owns the session's turn ends (such as during an upgrade wind-down).
    fn owns_turn_end(&self) -> bool;
    /// Returns whether the host temporarily withholds automatic turn-end continuations.
    fn withholds_turn_end(&self) -> bool {
        false
    }
    /// Polls for a host action during a break in the agent's background work.
    act fn at_background(&self) -> Option<(String, crate::supervise::policy::turn_end::HostTurn)> {
        None
    }
    /// Notifies the host that a requested point was withheld by a guard and returns any action taken.
    fn at_withheld_point(&self, wake: bool) -> Option<String> {
        let _ = wake;
        None
    }
    /// Ends the agent and relaunches it on the same conversation for `why`.
    act fn restart(&self, why: &super::super::policy::turn_end::Restart) -> Option<String> {
        let _ = why;
        None
    }
    /// Returns whether [`Self::restart`] is supported and enabled for this session.
    fn can_restart(&self) -> bool {
        false
    }
    /// Returns whether `[harness] relaunch = false` is the sole reason [`Self::can_restart`] is false.
    fn relaunch_off(&self) -> bool {
        false
    }
    /// Returns whether an explicit `restart` action would be carried for the agent now.
    fn restart_offered(&self) -> Option<bool> {
        None
    }
    /// Notifies the host whether the loop is holding for a server-published input stall.
    fn stalled(&self, held: bool) {
        let _ = held;
    }
    /// Returns whether an agent terminated by the stall remedy will be relaunched on its conversation.
    fn relaunches_after_stall(&self) -> bool {
        false
    }
    /// Returns the agent's foreground process group leader PID as last observed by the host.
    fn agent_pid(&self) -> Option<u32> {
        None
    }
    /// Returns the agent process's kernel start timestamp (Unix seconds) as last observed by the host.
    fn agent_started(&self) -> Option<i64> {
        None
    }
    /// Returns the conversation session ID held by the agent process, if known.
    fn agent_conversation(&self) -> Option<String> {
        None
    }
    /// Returns whether a self-supervising agent (such as `codex-alab`) is at its own idle point.
    fn beside_idle(&self) -> bool {
        false
    }
    /// Returns whether the conversation has received no user or orchestrator task.
    fn taskless(&self) -> bool {
        false
    }
    /// Returns whether the conversation's transcript indicates its task has finished and landed.
    fn finished(&self) -> Option<bool> {
        None
    }
    /// Returns the [`crate::harness::upgrade::LastTurn`] keys for the conversation's final turn.
    fn last_turn(&self) -> Option<crate::harness::upgrade::LastTurn> {
        None
    }
    /// Returns uncommitted, unpushed, or unmerged git work in the session's repository.
    fn repo_work(&self) -> Option<crate::harness::repo_work::RepoWork> {
        None
    }
    /// Returns whether this supervision loop started when Autopilot switched out of `Off`.
    fn lifted_off(&self) -> bool {
        false
    }
    /// Returns the Unix timestamp when aterm observed the current agent launch.
    fn launch(&self) -> Option<i64> {
        None
    }
    /// Notifies the host that the session was observed busy since the last idle point.
    fn turn_ran(&self) {}
    /// Notifies the host of a decided idle turn end on a Claude Code session for the usage tap.
    fn usage_tap(&self) {}
    /// The usage tap's newest reading of `window` for the agent's own process, and when the tap wrote it ([`super::super::tap_clock`]): asked only at a Claude Code usage wall, to time its continuation. `None`: no reading this host stands behind. The default reads none.
    fn tap_window(
        &self,
        window: crate::supervise::tap_clock::UsageWindow,
    ) -> Option<crate::supervise::tap_clock::TapWindow> {
        let _ = window;
        None
    }
    /// Records an informational supervisor note for the session.
    fn inform(&self, text: &str) {
        let _ = text;
    }
    /// Notifies the host that a usage-limit episode opened (`true`) or closed (`false`).
    fn limited(&self, open: bool) {
        let _ = open;
    }
    /// Returns the Unix reset timestamp named by the session's standing usage limit in its transcript.
    fn limit_until(&self) -> Option<u64> {
        None
    }
    /// Returns the standing usage-limit row from the session's transcript, if any.
    fn standing_limit(&self) -> Option<crate::harness::upgrade::StandingLimit> {
        None
    }
    /// Returns the last 256 KiB of the active Claude Code transcript.
    fn transcript_tail(&self) -> Option<String> {
        None
    }
    /// Re-reads the active Claude Code transcript tail within a single wall-confirmation decision.
    fn transcript_tail_again(&self) -> Option<String> {
        self.transcript_tail()
    }
    /// Returns the path and tail contents of the active Claude Code transcript.
    fn transcript(&self) -> Option<(PathBuf, String)> {
        None
    }

    /// Returns the kernel launch timestamp (Unix seconds) of the process owning [`Self::transcript_tail`].
    fn launched_at(&self) -> Option<i64> {
        None
    }

    /// Publishes or clears the supervision loop's active [`WaitPlan`].
    fn waiting(&self, plan: Option<WaitPlan>) {
        let _ = plan;
    }

    /// Records a harness [`crate::harness::acts::Act`] as it is taken.
    fn act_taken(&self, act: crate::harness::acts::Act) {
        let _ = act;
    }

    /// Updates the session's [`crate::harness::acts::TaskFace`] state.
    fn task_face(&self, face: crate::harness::acts::TaskFace) {
        let _ = face;
    }

    /// Restates the session's [`crate::harness::acts::TaskFace`] from the journal at loop startup.
    fn task_face_noted(&self, face: crate::harness::acts::TaskFace, noted_ms: i64) {
        let _ = noted_ms;
        self.task_face(face);
    }

    /// Publishes the worker's standing `NEEDS YOU:` items and summary entry.
    fn needs_standing(&self, needs: &[String], entry: &str) {
        let _ = (needs, entry);
    }

    /// Publishes or clears the reason Autopilot Land is still holding.
    fn land_held(&self, held: Option<crate::harness::acts::LandHeld>) {
        let _ = held;
    }

    /// Returns the shell command a user can run to resume the agent's conversation.
    fn resume_command(&self) -> Option<String> {
        None
    }

    /// Returns the host's measured network reachability ([`Reach`]) to the agent's API.
    fn reach(&self) -> Reach {
        Reach::Unknown
    }

    /// Returns the host's memory reading for the agent's process group and the machine.
    fn memory(&self) -> Option<super::super::memory::AgentMemory> {
        None
    }

    /// Records a warning from the session's supervisor.
    fn warn(&self, text: &str) {
        self.inform(text);
    }

    /// Records which [`Guard`] withheld an idle point requested by the host.
    fn withheld(&self, guard: Guard) {
        let _ = guard;
    }

    /// Reports or clears a rating survey card that the supervision loop cannot dismiss.
    fn survey_stuck(&self, stuck: Option<(StuckSurvey, &str)>) {
        let _ = stuck;
    }

    /// Reports or clears a turn-end [`Exception`] that requires human intervention.
    fn exception(&self, standing: Option<&Exception>) {
        let _ = standing;
    }

    /// Reports or clears a second-rung stall canary observation ([`StallSeen`]).
    fn stall_unread(&self, stall: Option<StallSeen>) {
        let _ = stall;
    }

    /// Returns kernel process-tree evidence of background work running under the agent.
    fn background_jobs(&self) -> crate::supervise::background::BackgroundEvidence {
        crate::supervise::background::BackgroundEvidence::Unknown
    }

    /// Returns the machine's CPU/GPU/memory occupancy reading for this session.
    fn occupancy(&self) -> crate::supervise::occupancy::OccupancyReading {
        crate::supervise::occupancy::OccupancyReading::default()
    }

    /// Reports usage-credit and quota-warning notices read from the agent's screen.
    fn usage_seen(&self, seen: &UsageSeen) {
        let _ = seen;
    }

    /// Queries the host for multi-account rotation [`AccountAdvice`] at `point`.
    fn account(&self, point: AccountPoint) -> Option<AccountAdvice> {
        let _ = point;
        None
    }

    /// Notifies the host that the account this session rotated onto served a turn.
    fn account_served(&self) {}

    /// Notifies the host that the account this session rotated onto hit a wall or lost login.
    fn account_answered(&self, bad: AccountBad) {
        let _ = bad;
    }

    /// Returns a monotonic stamp that advances whenever host account advice may have changed.
    fn account_stamp(&self) -> u64 {
        0
    }

    /// Returns the renewable roster account label this session runs on, if any.
    fn login_label(&self) -> Option<String> {
        None
    }

    /// Returns whether this session's process holds a login directory superseded by a renewal.
    fn login_superseded(&self) -> bool {
        false
    }

    /// Returns the [`aterm_phase::LoginMethod`] configured for this session's login.
    fn login_method(&self) -> Option<aterm_phase::LoginMethod> {
        None
    }

    /// Returns shared sign-in latch state across tabs using the same login.
    fn login_shared(&self) -> Option<super::super::policy::login::LoginShared> {
        None
    }

    /// Returns the roster status word for this session's login.
    fn roster_word(&self) -> Option<super::super::policy::turn_end::RosterWord> {
        None
    }

    /// Claims the shared sign-in reservation before typing `/login`.
    act fn login_carry(&self) -> Result<(), String> {
        Ok(())
    }

    /// Releases this worker's sign-in reservation.
    fn login_abandon(&self) {}

    /// Notifies the host that this session's lost login has recovered.
    fn login_back(&self) {}

    /// Notifies the host that a usage or model limit ended without a sign-in.
    fn limit_over(&self) {}

    /// Records a vendor notice that the stored login expires in `days` days.
    fn login_notice(&self, days: u32) {
        let _ = days;
    }

    /// Claims the `/usage` probe slot for this session's account.
    act fn account_probe(&self) -> Result<(), ProbeNo> {
        Err(ProbeNo::NoAccounts)
    }

    /// Records the settled `/usage` panel sample or miss from a claimed probe.
    fn account_probed(&self, read: Result<&crate::harness::accounts::Sample, ProbeMiss>) {
        let _ = read;
    }

    /// Requests permission to take a limit-reset offer on this session's account.
    act fn reset_take(&self, ask: &ResetAsk) -> ResetWord {
        let _ = ask;
        ResetWord::No(ResetNo::NoHost)
    }

    /// Records the outcome of a claimed limit-reset dialog step.
    fn reset_told(&self, end: &ResetEnd) -> ResetOutcome {
        let _ = end;
        ResetOutcome::Unread
    }
}
}

#[cfg(test)]
#[path = "host_door_tests.rs"]
mod tests;
