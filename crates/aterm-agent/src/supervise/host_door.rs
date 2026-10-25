// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host callback trait ([`IdleHost`]) and `idle_host_trait!` macro for taped/replayed supervision.

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
    fn wants(&self) -> bool;
    fn wants_at(&self) -> Option<Instant> {
        None
    }
    act fn at_idle(&self) -> Option<HostStep>;
    act fn at_idle_on(&self, point: IdlePoint) -> Option<HostStep> {
        let _ = point;
        self.at_idle()
    }
    fn owns_turn_end(&self) -> bool;
    fn withholds_turn_end(&self) -> bool {
        false
    }
    act fn at_background(&self) -> Option<(String, crate::supervise::policy::turn_end::HostTurn)> {
        None
    }
    fn at_withheld_point(&self, wake: bool) -> Option<String> {
        let _ = wake;
        None
    }
    act fn restart(&self, why: &super::super::policy::turn_end::Restart) -> Option<String> {
        let _ = why;
        None
    }
    fn can_restart(&self) -> bool {
        false
    }
    fn relaunch_off(&self) -> bool {
        false
    }
    fn restart_offered(&self) -> Option<bool> {
        None
    }
    fn stalled(&self, held: bool) {
        let _ = held;
    }
    fn relaunches_after_stall(&self) -> bool {
        false
    }
    fn agent_pid(&self) -> Option<u32> {
        None
    }
    fn agent_started(&self) -> Option<i64> {
        None
    }
    fn relaunch_owed(&self) -> bool {
        false
    }
    fn agent_conversation(&self) -> Option<String> {
        None
    }
    fn beside_idle(&self) -> bool {
        false
    }
    fn taskless(&self) -> bool {
        false
    }
    fn finished(&self) -> Option<bool> {
        None
    }
    fn last_turn(&self) -> Option<crate::harness::upgrade::LastTurn> {
        None
    }
    /// What the session's repository holds that no remote main has
    /// ([`crate::harness::repo_work::probe`]), or `None` while the probe still
    /// reads: a host that reads on its own thread waits a bounded time for the
    /// answer ([`crate::harness::repo_work::Probing`]), so the loop, asking
    /// again, renews its claim and sees a stop between asks however long the
    /// reads take.
    fn repo_work(&self) -> Option<crate::harness::repo_work::RepoRead> {
        Some(crate::harness::repo_work::RepoRead::Clean)
    }
    fn lifted_off(&self) -> bool {
        false
    }
    fn launch(&self) -> Option<i64> {
        None
    }
    fn turn_ran(&self) {}
    /// The loop decided a turn break (an idle or limited point), told once at the decision,
    /// before the host is asked whether it [`Self::wants`] a step there; the wait's later looks at
    /// the same point are not told. `theirs` is whose the session is for as long as they keep it,
    /// as [`IdlePoint::theirs`] says. A host that keeps a finding of the session as of its last
    /// turn break (the `disk-free@v1` seat rule's) makes it here, so the finding is never older
    /// than the turn break the session rests at, whether or not the host owes a step there.
    fn turn_break(&self, theirs: Option<Guard>) {
        let _ = theirs;
    }
    fn usage_tap(&self) {}
    /// The agent is mid-turn (a busy read): its usage tap's newest reading may be recorded,
    /// as at a decided idle turn end ([`Self::usage_tap`]) but never judged silent, so what
    /// the accounts store holds keeps up with a turn that runs for hours. The host bounds
    /// how often it reads. A host whose finding of the session at its last turn break holds only
    /// while the session rests there ([`Self::turn_break`]) drops it here.
    fn busy_tap(&self) {}
    fn tap_window(
        &self,
        window: crate::supervise::tap_clock::UsageWindow,
    ) -> Option<crate::supervise::tap_clock::TapWindow> {
        let _ = window;
        None
    }
    /// The account the session's Claude Code profile is signed in to now
    /// ([`crate::harness::profile::account_of`]), for a stored login alone.
    fn login_account(&self) -> Option<crate::harness::profile::AccountKey> {
        None
    }
    fn inform(&self, text: &str) {
        let _ = text;
    }
    fn limited(&self, open: bool) {
        let _ = open;
    }
    fn limit_until(&self) -> Option<u64> {
        None
    }
    fn standing_limit(&self) -> Option<crate::harness::upgrade::StandingLimit> {
        None
    }
    fn transcript_tail(&self) -> Option<String> {
        None
    }
    fn transcript_tail_again(&self) -> Option<String> {
        self.transcript_tail()
    }
    fn transcript(&self) -> Option<(PathBuf, String)> {
        None
    }

    fn launched_at(&self) -> Option<i64> {
        None
    }

    fn waiting(&self, plan: Option<WaitPlan>) {
        let _ = plan;
    }

    fn act_taken(&self, act: crate::harness::acts::Act) {
        let _ = act;
    }

    fn task_face(&self, face: crate::harness::acts::TaskFace) {
        let _ = face;
    }

    fn task_face_noted(&self, face: crate::harness::acts::TaskFace, noted_ms: i64) {
        let _ = noted_ms;
        self.task_face(face);
    }

    fn needs_standing(&self, needs: &[String], entry: &str) {
        let _ = (needs, entry);
    }

    fn land_held(&self, held: Option<crate::harness::acts::LandHeld>) {
        let _ = held;
    }

    fn resume_command(&self) -> Option<String> {
        None
    }

    fn reach(&self) -> Reach {
        Reach::Unknown
    }

    fn memory(&self) -> Option<super::super::memory::AgentMemory> {
        None
    }

    fn warn(&self, text: &str) {
        self.inform(text);
    }

    fn withheld(&self, guard: Guard) {
        let _ = guard;
    }

    fn survey_stuck(&self, stuck: Option<(StuckSurvey, &str)>) {
        let _ = stuck;
    }

    fn exception(&self, standing: Option<&Exception>) {
        let _ = standing;
    }

    fn stall_unread(&self, stall: Option<StallSeen>) {
        let _ = stall;
    }

    fn background_jobs(&self) -> crate::supervise::background::BackgroundEvidence {
        crate::supervise::background::BackgroundEvidence::Unknown
    }

    fn occupancy(&self) -> crate::supervise::occupancy::OccupancyReading {
        crate::supervise::occupancy::OccupancyReading::default()
    }

    fn facts(&self) -> Option<crate::supervise::facts::FactsReading> {
        None
    }

    fn break_taken(&self) {}

    fn occupancy_said(&self, said: &crate::supervise::occupancy::OccupancySaid) {
        let _ = said;
    }

    fn upgrade_due(&self) -> bool {
        false
    }

    act fn occupancy_pace(
        &self,
        rule: crate::supervise::occupancy::RuleId,
        hold_since_ms: i64,
    ) -> crate::supervise::occupancy::PaceGrant {
        let _ = (rule, hold_since_ms);
        crate::supervise::occupancy::PaceGrant::Local
    }

    fn wakes(&self) -> crate::supervise::occupancy::WakesSeen {
        crate::supervise::occupancy::WakesSeen::default()
    }

    act fn wake_claim(
        &self,
        ord: u16,
        episode: u64,
        door: crate::supervise::occupancy::WakeDoor,
    ) -> crate::supervise::occupancy::WakeClaim {
        let _ = (ord, episode, door);
        crate::supervise::occupancy::WakeClaim::Gone
    }

    act fn wake_release(&self, ord: u16, episode: u64) {
        let _ = (ord, episode);
    }

    fn wake_told(&self, ord: u16, episode: u64, door: crate::supervise::occupancy::WakeDoor) {
        let _ = (ord, episode, door);
    }

    act fn task_fact(
        &self,
        word: Option<crate::supervise::occupancy::FactWord>,
    ) -> crate::supervise::occupancy::NoteOutcome {
        let _ = word;
        crate::supervise::occupancy::NoteOutcome::NoTask
    }

    fn usage_seen(&self, seen: &UsageSeen) {
        let _ = seen;
    }

    fn account(&self, point: AccountPoint) -> Option<AccountAdvice> {
        let _ = point;
        None
    }

    fn account_served(&self) {}

    fn account_answered(&self, bad: AccountBad) {
        let _ = bad;
    }

    fn account_stamp(&self) -> u64 {
        0
    }

    fn login_label(&self) -> Option<String> {
        None
    }

    fn login_superseded(&self) -> bool {
        false
    }

    fn login_method(&self) -> Option<aterm_phase::LoginMethod> {
        None
    }

    fn login_shared(&self) -> Option<super::super::policy::login::LoginShared> {
        None
    }

    fn roster_word(&self) -> Option<super::super::policy::turn_end::RosterWord> {
        None
    }

    act fn login_carry(&self) -> Result<(), String> {
        Ok(())
    }

    fn login_abandon(&self) {}

    fn login_back(&self) {}

    fn limit_over(&self) {}

    fn login_notice(&self, days: u32) {
        let _ = days;
    }

    act fn account_probe(&self) -> Result<(), ProbeNo> {
        Err(ProbeNo::NoAccounts)
    }

    fn account_probed(&self, read: Result<&crate::harness::accounts::Sample, ProbeMiss>) {
        let _ = read;
    }

    act fn reset_take(&self, ask: &ResetAsk) -> ResetWord {
        let _ = ask;
        ResetWord::No(ResetNo::NoHost)
    }

    fn reset_told(&self, end: &ResetEnd) -> ResetOutcome {
        let _ = end;
        ResetOutcome::Unread
    }
}
}

#[cfg(test)]
#[path = "host_door_tests.rs"]
mod tests;
