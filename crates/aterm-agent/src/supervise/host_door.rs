// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HOST'S DOOR: [`IdleHost`], the window's part in the loop, declared
//! ONCE through [`idle_host_trait!`], which emits from that one method list
//! the trait itself and the two impls a harness tape needs of it — the TAPED
//! host a recording run's loop asks (`tape::host::TapedHost`: every call
//! forwarded to the window's host, its answer taped on the way back) and the
//! REPLAY host a replay's loop asks (`tape::host::ReplayHost`: every answer
//! the tape's, no host at all).
//!
//! WHY GENERATED, AND NOT WRITTEN BY HAND. The trait has some fifty methods,
//! all but three with a default body, and grows a method most weeks. A
//! wrapper written beside it would compile without a method added after it —
//! the default body would answer for the window's host — so the loop would
//! read that method's answer past the tape, and a replay would answer it
//! with the default instead of what the host said: a hole nobody sees until
//! a replay of a real incident goes wrong. Generated from the same list, a
//! method added to the trait is taped by construction, and a method whose
//! answer has no tape form (`tape::host::HostReply`) or whose argument is not
//! one a tape compares (`tape::host::TapeArg`) does not build. No call site
//! changes: the loop is handed the wrapper in place of the host
//! (`Fx::host_door`, at the loop's start), and asks it as it always asked
//! the host.
//!
//! What crosses, per method (the replay plan's §4.6): a method that RETURNS
//! a value is a round trip at `host.<name>` — its arguments' `Debug` text as
//! the request (`tape::host::request`), its answer as the reply, every
//! `Instant` in it written as an offset from the tape's origin; a method that
//! returns nothing is a NOTIFICATION, an output at `host.<name>` — its
//! arguments — that a replay compares and the window's host is still told.
//! The taped host forwards EVERY method to the window's host, the defaulted
//! ones too, so a default body that asks another method (`at_idle_on` asks
//! `at_idle`, `warn` tells `inform`) asks it of the window's host, below the
//! door, as it did before the door: the tape holds the one call the loop
//! made, and a replay, which answers that call, never makes the other.
//!
//! A method through which the host ACTS on the session — types into it,
//! ends or relaunches its agent, lets the loop take a reset or type a probe
//! (the replay plan's §5.1) — is marked `act` before its `fn` in the list
//! below, and nowhere else: the taped host closes the tape's frame after
//! it, as after a control act, so what the session was done is on disk
//! before what follows; and the macro writes the list of those doors
//! ([`HOST_ACTS`]) that a replay reads to call a divergence there a
//! DECISION. A method added as an act is classed where it is declared, not
//! in a list kept beside the trait that a new method could be missing from.
//!
//! What the window's host does inside a call — its upgrade's steps, a
//! relaunch, a look at a transcript or a roster — is below the door: frozen
//! on the tape as the answer it gave, never re-executed by a replay (the
//! plan's §11: a replay does not re-run the host's upgrade logic).

use super::*;

/// Declare THE HOST'S TRAIT and its two taped impls from one method list
/// ([module docs](self)).
///
/// The input is the trait as it would be written by hand — its docs, `pub
/// trait <Name>: Send + Sync + std::fmt::Debug`, and its methods, each `fn
/// <name>(&self, <arg>: <type>, …) [-> <type>]` with its docs and either
/// `;` or its default body — save that a method that ACTS on the session
/// is marked `act` before its `fn` ([module docs](self)). The macro emits
/// the trait (without the marks), then `impl <Name> for
/// tape::host::TapedHost<dyn <Name>>` and `impl <Name> for
/// tape::host::ReplayHost`, and `HOST_ACTS`, the marked methods' doors. A
/// test may declare a trait of its own with it (a planted method that the
/// taped host must forward), which is why the name is the input's.
///
/// It reads the methods one at a time (a "muncher"): a method with a default
/// body and one without differ only in what follows the signature, which one
/// pattern cannot say after a return type, so each is matched by its own
/// arm — and an act by two more — and collected, its mark as `[act]` or
/// `[]`; the last arm, with every method collected, writes the four items.
/// Each method's own `self` is carried through (`$this`), as macro hygiene
/// keeps the `self` of a default body from naming a `self` the macro wrote.
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
    // A method with no default body.
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
    // A method with a default body.
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
    // An act with no default body.
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
    // An act with a default body.
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
    // Every method collected: the trait, then its taped and replay impls, and
    // the acts' doors.
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

        /// The doors of the methods marked `act` ([module docs](self)), in
        /// the trait's order: what a replay calls a DECISION where it
        /// diverges (`tape::doors::is_host_act`). (`_act` only places each
        /// door under its method's mark: a method with none writes nothing.)
        pub(crate) const HOST_ACTS: &[&str] = &[$($(
            {
                let _act = stringify!($act);
                concat!("host.", stringify!($name))
            },
        )?)*];

        /// The trait's methods by name, in its order: what this crate's tests
        /// hold the door's names to.
        #[cfg(test)]
        pub(crate) const HOST_METHODS: &[&str] = &[$(stringify!($name)),*];
    };
    // Whether a method's mark makes it an act.
    (@act act) => { true };
    (@act) => { false };
    // The taped host: a notification is said, then told to the window's host.
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
    // The taped host: a query is asked of the window's host, its answer taped.
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
    // The replay host: a notification is compared with the recorded one.
    (@replay $name:ident [$($arg:ident: $ty:ty),*] []) => {
        fn $name(&self $(, $arg: $ty)*) {
            self.tell(
                concat!("host.", stringify!($name)),
                $crate::supervise::run::host_door::idle_host_trait!(@request $($arg),*),
            );
        }
    };
    // The replay host: a query is answered from the tape.
    (@replay $name:ident [$($arg:ident: $ty:ty),*] [$ret:ty]) => {
        fn $name(&self $(, $arg: $ty)*) -> $ret {
            self.ask(
                concat!("host.", stringify!($name)),
                $crate::supervise::run::host_door::idle_host_trait!(@request $($arg),*),
            )
        }
    };
    // A call's request: each argument by its name and its `Debug` text.
    (@request $($arg:ident),*) => {
        $crate::supervise::tape::host::request(&[
            $((stringify!($arg), &$arg as &dyn $crate::supervise::tape::host::TapeArg)),*
        ])
    };
}

pub(crate) use idle_host_trait;

idle_host_trait! {
/// A host's part at the loop's idle points ([`SuperviseOpts::idle_host`]): the
/// window's host takes the live upgrade's step, and a relaunched agent's owed
/// continuation, at an authoritative idle point inside the loop, which keeps
/// its back-off, typing budget, wall tracks, claim and badges across it.
pub trait IdleHost: Send + Sync + std::fmt::Debug {
    /// The host asks for the next idle point: read at each point the loop
    /// reaches and at each wake of its wait ([`WAIT_STEP`], the settle the
    /// upgrade's own gate asks for anyway) — nothing polls for it.
    fn wants(&self) -> bool;
    /// When the host will next want a point, if it already knows: a look it
    /// scheduled (the upgrade's short ladder after an unread look, 5 s then
    /// 15 s). The loop's wait ends there, so [`Self::wants`] is read at that
    /// instant and not at its next [`WAIT_STEP`] wake (else unread looks 5 s
    /// apart are taken 21 s apart). The default: never known ahead.
    fn wants_at(&self) -> Option<Instant> {
        None
    }
    /// Take the host's step. The loop is at an authoritative idle point
    /// with no box, no wall (as the point's decision judged it: one only the
    /// transcript named counts, one it refused does not), no limit episode
    /// and no act of its own in flight, and has typed nothing there — the host's step goes FIRST,
    /// before the turn-end policy acts on the point. The loop reads the
    /// screen afresh after it, and journals the step's word when there was
    /// one (`HOST <what> step=<word>`), so the session's journal carries the
    /// upgrade's and the relaunch's acts beside its own. Whether the step
    /// MOVED the session ([`HostStep::moved`]) says what becomes of the
    /// point: one it left standing is decided again once the host owns
    /// nothing ([`Session::host_held`]), and is offered the host again only
    /// after a step of the wait, however soon it wants a point
    /// ([`Session::host_passed`], a few follow-ups excepted:
    /// [`Session::host_follows`]); one it moved is gone.
    act fn at_idle(&self) -> Option<HostStep>;
    /// [`Self::at_idle`], told what the loop knows of the point
    /// ([`IdlePoint`]): the call the loop makes. A host that types a
    /// relaunched agent's continuation types none into a task done under
    /// Land ([`IdlePoint::done_under_land`]). The default takes the plain
    /// step: a host that types no continuation has nothing to read in it.
    act fn at_idle_on(&self, point: IdlePoint) -> Option<HostStep> {
        let _ = point;
        self.at_idle()
    }
    /// The host owns the session's turn ends for now — the live upgrade's
    /// wind-down, from the state of the upgrade itself, never from the
    /// screen: the turn-end policy types nothing ([`TurnEndReading::upgrading`]).
    fn owns_turn_end(&self) -> bool;
    /// The host WITHHOLDS the session's continuations for now — nothing
    /// else: a full disk ([`crate::harness::disk`]) on which every write of
    /// the agent fails, where a continuation only burns a turn. The turn-end
    /// policy types nothing at such a point and decides it again once the
    /// host withholds nothing ([`Session::host_held`]). Unlike
    /// [`Self::owns_turn_end`] it is no upgrade: no account move is held
    /// back for it, no relaunch is promised by it, and a wall keeps its own
    /// rule once the hold lifts. The default: never.
    fn withholds_turn_end(&self) -> bool {
        false
    }
    /// A natural break in the agent's own background work: its turn is over,
    /// the composer drawn, and only work it started runs (a workflow, a
    /// background agent, a shell, a Codex background terminal;
    /// [`aterm_phase::ScreenReader::background_wait`], or the session's
    /// transcript where the screen shows no row of it:
    /// [`Session::transcript_wait`]), the break has stood
    /// [`background_settle`], and there is no wall, limit episode, stall or
    /// untaken act (`TurnEndState::act_untaken`). Offered only while the host
    /// [`Self::wants`] a point. The host may type one line that ends nothing:
    /// the upgrade's notice (first, or a re-ask after `upgrade::REASK_S`) or
    /// the release line owed by a give-up, void or stop; or it may give up
    /// asking. It returns what it took (`Some`, journaled `HOST seq=<n>
    /// background <word>`) or `None`. Ending the agent is an idle point's alone.
    act fn at_background(&self) -> Option<String> {
        None
    }
    /// THE AGENT RESTS AT A POINT A GUARD WITHHELD FROM THE HOST: the host
    /// [`Self::wants`] a point, and the loop's own reading of the agent —
    /// one its reader vouches for, never the start-up read — is idle or at
    /// a usage limit's notice, where a guard keeps the host's step
    /// ([`Self::at_idle`]) from it: a limit episode short of its reset plus
    /// [`crate::harness::upgrade_drive::LIMIT_GRACE_S`], a wall, an act of
    /// the loop's in flight. Asked at the point (`wake` false), again at the
    /// first ask of its wait — after the point's own act is typed and its
    /// `DONE` or `LEFT` said, or where the host first wants a point partway
    /// through the wait, a look of its come due (`wake` false: F1-R5-A1) —
    /// and at each later wake of that wait (`wake` true), at most
    /// [`WAIT_STEP`] apart. The host settles there
    /// what it owes that TYPES NOTHING and ends nothing — a relaunched
    /// agent's carry-on the loop's act made moot, its one look at what did
    /// not come back (`crate::harness::relaunch::settle`) — which the guard
    /// may keep from it for hours, and for good where a limit names no reset
    /// (F1 of the live lab of 2026-10-16: a crash's relaunch under a usage
    /// limit stayed owed through the done check and `DONE`, holding the
    /// tab's automatic update). A settle that missed (another sweep's lock, a
    /// relaunch not adopted yet, a look still open) is settled again at the
    /// next ask: the loop's own points and wakes are the retries, and no
    /// timer of the host's is (F1-R4, the fourth verification of F1's fix).
    /// It returns what it settled (`Some`, journaled `HOST seq=<n> <line>`)
    /// or `None`. Whatever would type is still the next idle point's. The
    /// default settles nothing.
    fn at_withheld_point(&self, wake: bool) -> Option<String> {
        let _ = wake;
        None
    }
    /// End the agent and relaunch it on its own conversation, here, for `why`
    /// ([`super::super::policy::turn_end::TurnEndAction::Restart`]). Returns the step's
    /// word ([`crate::harness::relaunch::restart_here`]: `adopted`, `continued`
    /// when the host also typed its carry-on, `wait:…`, `refused:…`/`failed:…`),
    /// or `None` where this host makes no restart and the point is the
    /// person's. The default makes none.
    act fn restart(&self, why: &super::super::policy::turn_end::Restart) -> Option<String> {
        let _ = why;
        None
    }
    /// Whether [`Self::restart`] can be asked here at all — the agent one
    /// this host relaunches, under `[harness] relaunch` — so the turn-end
    /// policy asks for no restart that cannot be made
    /// ([`super::super::policy::turn_end::TurnEndReading::restartable`]). The
    /// default: none.
    fn can_restart(&self) -> bool {
        false
    }
    /// Whether the explicit `restart` would be CARRIED for the agent now: the
    /// host's one predicate for every word of a stall (aterm's
    /// `restart_act::offered`: a snapshot whose relaunch brings back what the
    /// act ends, [`crate::harness::relaunch::carries_restart`], the agent in
    /// front, and the host's hand the one on the session). `[harness]
    /// relaunch = false` does not limit it: it does not refuse a person's
    /// explicit restart (ruling 1, 2026-10-08). `Some(promised)` where it
    /// would: the frozen mail and the warning name that restart instead of
    /// `signal term` and [`Self::resume_command`] — as the server's own words
    /// do — and, only where `promised` (its exit relaunched on the agent's
    /// own conversation, [`crate::harness::relaunch::resumes_on_exit`]: never
    /// a Codex's), say "aterm relaunches it on its conversation". The
    /// default: `None`.
    fn restart_offered(&self) -> Option<bool> {
        None
    }
    /// The loop holds for a stall the server published (`true`,
    /// [`Session::hold_for_stall`]), or one it held lifted (`false`,
    /// [`Session::thaw`]). A NOTICE of what this loop saw, as it saw it — it
    /// lags the stall (a thaw is seen at the loop's next look) and a run that
    /// ends holding one never says it lifted — so it says nothing of how the
    /// agent ended: whether an exit was the stall's remedy's (U1) is the
    /// explicit restart's stamp, on disk before its signal, which aterm's
    /// host looks up instead. The default ignores it.
    fn stalled(&self, held: bool) {
        let _ = held;
    }
    /// Whether an agent the stall's remedy ends here is RELAUNCHED on its
    /// conversation by this host (U1: its relaunch on exit, under `[harness]
    /// relaunch`) — so the loop may take the remedy itself once the stall
    /// has stood past its bound ([`Session::stall_remedy`]). The default:
    /// no, and the remedy stays a person's.
    fn relaunches_after_stall(&self) -> bool {
        false
    }
    /// The agent's process as the host last read it — the tab's foreground
    /// group leader — which the stall's remedy names (`restart pid=<n>
    /// if-stalled`, refused unless that very process still leads the tab).
    /// The default: none, and the remedy names the tab's group.
    fn agent_pid(&self) -> Option<u32> {
        None
    }
    /// When the agent's process started, unix seconds, as the host last read
    /// it (its snapshot's kernel start) — read at each decision
    /// (`TurnEndState::agent_started`): a countdown Claude Code armed before
    /// it is a gone process's. The default: none, and the vendor's countdown
    /// is waited for as its notice says.
    fn agent_started(&self) -> Option<i64> {
        None
    }
    /// The conversation the agent's process holds, as the host last read it
    /// (its snapshot's session id; no read is made for it): with
    /// [`Self::agent_pid`], WHOM a limit only the transcript shows was last
    /// asked about ([`Session::read_standing_limit`]) — a process that named
    /// none yet, or another conversation since, is asked about again. The
    /// default: none.
    fn agent_conversation(&self) -> Option<String> {
        None
    }
    /// BESIDE A PROVEN SELF-CLAIM, THE AGENT'S OWN IDLE POINT (the codex-alab
    /// contract, §4; `beside.rs`): whether the agent's session record, read
    /// NOW while its identity is live, says it rests (`status: "idle"` with
    /// `draftEmpty: true`, `crate::harness::self_claimed::read_idle`). Asked
    /// at decision time, in place of any screen reader, before the memory
    /// ceiling's restart (never the stall's: a frozen TUI's record never
    /// rests, and the stall alone decides). The default — and anything
    /// unread, any other status, a draft, a dead identity — is `false`: wait.
    fn beside_idle(&self) -> bool {
        false
    }
    /// Nobody has asked the session anything, by the host's evidence: the
    /// conversation's record holds no prompt of a person's or orchestrator's
    /// (`crate::harness::upgrade::TaskScan`). Read at every turn end beside
    /// the screen's launch card ([`TurnEndReading::taskless`]); the turn-end
    /// policy types nothing into such a session. The default leaves it to the
    /// screen.
    fn taskless(&self) -> bool {
        false
    }
    /// WHETHER THE CONVERSATION'S OWN RECORD SAYS ITS TASK IS FINISHED, by the
    /// host's evidence (`crate::harness::upgrade::FinishedScan`): `Some(true)`
    /// where the worker ended it (`ATERM-TASK-DONE`, or a done reply to the
    /// done check) and no prompt of a person's or an orchestrator's has come
    /// since — Claude Code's own wakes (a background task's notification,
    /// its usage-limit continuation) and the harness's own turns being none;
    /// `Some(false)` where it says otherwise. Read at every point a turn ran
    /// and at a new loop's first point
    /// ([`TurnEndReading::finished_record`]); the turn-end policy types
    /// nothing into a finished task, whatever the screen's last words are.
    /// The default reads no record (`None`): the screen decides.
    fn finished(&self) -> Option<bool> {
        None
    }
    /// WHAT THE DONE CHECK'S CLOSED ANSWER SAYS BY THE RECORD
    /// (`crate::harness::upgrade::FinishedScan::check_answer`), as the read
    /// [`Self::finished`] just made left it: `Some(false)` where the turn the
    /// conversation ends on is the done check's, closed (`turn_duration`), and
    /// its last words are no done word of the worker's own — the record keeps
    /// inline code and fenced blocks, which the screen draws with no ticks and
    /// no fences, so a tool's quoted `Build completed successfully` is no done
    /// word there ([`TurnEndReading::check_record`], the review's RR4-TE-1).
    /// The default reads no record (`None`): the screen decides.
    fn check_answer(&self) -> Option<bool> {
        None
    }
    /// WHETHER THE FINISH THE RECORD SAYS ([`Self::finished`]) IS A DONE
    /// REPLY IN WORDS TO THE DONE CHECK, not the worker's `ATERM-TASK-DONE`
    /// (`crate::harness::upgrade::FinishedScan::finished_at_check`), as the
    /// read [`Self::finished`] just made left it
    /// ([`TurnEndReading::finished_at_check`]): the task ends the same, and
    /// the journal says the check's rule, never that the worker reported the
    /// token (F2 of the live lab of 2026-10-16). The default: `false`, a
    /// finish read as the token's.
    fn finished_at_check(&self) -> bool {
        false
    }
    /// THE TURN THE CONVERSATION ENDS ON, by its key
    /// (`crate::harness::upgrade::FinishedScan::last_turn_key`), as the read
    /// [`Self::finished`] just made left it — as far back as the finish is
    /// read, so Claude Code's idle compaction after a person's exchange (its
    /// summary and restored files, past [`Self::transcript_tail`]'s 256 KiB)
    /// hides the turn not: what a left exchange's ledger row is matched by
    /// at a later loop's points ([`TurnEndReading::left_record`]; the merge
    /// review's XR-1). The default reads none, and the loop keys the turn by
    /// [`Self::transcript_tail`].
    fn last_turn_key(&self) -> Option<String> {
        None
    }
    /// THE HOST STARTED THIS LOOP AS THE SESSION'S AUTOPILOT LEFT OFF: it
    /// stopped the session's last worker under Off, and the person drove the
    /// session since. The acts a previous loop typed, seeded from the
    /// ledger, still count toward the budget but carry no task — save for a
    /// turn already running as the loop attaches
    /// (`TurnEndState::start_lifted_off`). The first point it finds idle is
    /// backed off and acted on as any loop's first point is, never left to
    /// anyone by what it reads there: no read of its saw that turn run. Only
    /// an earlier loop's ledger row of a person's exchange it saw run, still
    /// the conversation's last turn, keeps that point theirs
    /// ([`TurnEndReading::left_record`]: a turn of the person's under Off is
    /// another turn, and ends the match). Read once, at the loop's start.
    /// The default: no — a loop started for any other reason (a new agent, a
    /// changed policy, a restart) carries what its ledger shows.
    fn lifted_off(&self) -> bool {
        false
    }
    /// A read since the last point saw the session busy: a turn ran (the
    /// agent's, a person's, an orchestrator's, or one the host typed). What the
    /// host counted of the points before starts over. The default ignores it.
    fn turn_ran(&self) {}
    /// A DECIDED IDLE TURN END of a Claude Code session (a decision's read,
    /// not the books' reread after an act): where the host reads the usage tap's
    /// file of the session's own process, records a fresh reading for its
    /// login and judges whether a tapped process's tap went silent
    /// (`crate::harness::tap`). It types nothing and decides nothing: a
    /// reading moves no session. The default reads nothing.
    fn usage_tap(&self) {}
    /// Information for a person from the session's supervisor, nothing asked
    /// and no badge: the host records it where a person looks (the window's
    /// Messages). Its first line is the state (a short title); further lines
    /// are detail. The default records nothing.
    fn inform(&self, text: &str) {
        let _ = text;
    }
    /// A limit episode opened (`true`, [`Session::open_episode`]) or closed
    /// (`false`). While one stands the loop offers the host no point, so the
    /// host holds its own clocks (the upgrade's re-ask and READY clocks), and
    /// the host is not read as owning turn ends
    /// ([`super::super::policy::turn_end::TurnEndReading::upgrading`]): the wall's
    /// own rule waits out the limit. The default ignores it.
    fn limited(&self, open: bool) {
        let _ = open;
    }
    /// The reset the agent's standing usage limit names in its transcript,
    /// unix seconds, past or to come
    /// (`crate::harness::upgrade::transcript_limit_reset`), or `None`. Asked
    /// where a limit's notice or episode would withhold a point and names no
    /// reset of its own ([`Session::limit_spent`]: a notice still shown
    /// [`crate::harness::upgrade_drive::LIMIT_GRACE_S`] past its reset is an
    /// idle point), and where a notice names a clock time but no day
    /// ([`Session::transcript_day`]). The default reads none.
    fn limit_until(&self) -> Option<u64> {
        None
    }
    /// THE LIMIT ROW THE AGENT'S CONVERSATION STANDS AT, by its own
    /// transcript (`crate::harness::upgrade::transcript_standing_limit`), or
    /// `None` — and `None` where the host MOVED the conversation off that row
    /// (a bucket's fallback model, the way back, another account, with no row
    /// of the model since: `crate::harness::relaunch::moved_off`), so a
    /// move's carry-on is never held behind the limit it left. Asked where
    /// the screen draws no wall and the loop tracks no limit: at the first
    /// decisions after the loop starts or the agent's process or
    /// conversation changes ([`Session::read_standing_limit`]), and where a
    /// host step is about to be taken ([`Session::host_steps_here`]). The
    /// host may read the agent again to answer: [`Self::agent_pid`] and
    /// [`Self::agent_conversation`] are read after it. The default reads none.
    fn standing_limit(&self) -> Option<crate::harness::upgrade::StandingLimit> {
        None
    }
    /// THE AGENT'S OWN TRANSCRIPT, its tail: the last 256 KiB of the
    /// conversation the Claude Code in the tab writes now, found from its
    /// process as the footer finds it (its own `CLAUDE_CONFIG_DIR` or
    /// `HOME`) — what confirms a wall read on its screen before the turn-end
    /// policy acts on it ([`wall_confirm`], H6-HARNESS-1). `None`: no
    /// transcript this host can read (no Claude Code, its process not
    /// registered yet, its conversation not written yet, two of its id and
    /// none in its project's own place, a file that cannot be read): then a
    /// wall read under a tool's call is not acted on, and any other wall as
    /// the screen reads it. The default reads none.
    fn transcript_tail(&self) -> Option<String> {
        None
    }
    /// THE SAME TRANSCRIPT READ AGAIN within one decision (wall_confirm's
    /// `LAG_STEP` re-reads, right after a [`Self::transcript_tail`] that
    /// answered): the conversation the agent read then names, WITHOUT reading
    /// the agent again — a process read is a control-socket round trip and
    /// four `ps` spawns, and nothing moves between the re-reads but the file
    /// (the cross-lane review's XT-4). The default reads it afresh.
    fn transcript_tail_again(&self) -> Option<String> {
        self.transcript_tail()
    }

    /// When the Claude Code whose transcript [`Self::transcript_tail`] last
    /// read was launched, unix seconds (its kernel start, as the host's
    /// record of the process keeps it). What tells a row THIS process wrote
    /// from one a dead process left (a `--resume`, or aterm's relaunch, kills
    /// the background work its rows count): only the former says the session
    /// waits on background work now ([`wall_confirm`]'s
    /// `waits_on_background`, BG-1). `None`: no record, and the transcript
    /// says nothing of that wait. The default records none.
    fn launched_at(&self) -> Option<i64> {
        None
    }

    /// The loop's next try, told to the host (`Some`) or gone (`None`): the
    /// deadline of the API wall's retry wait
    /// ([`Session::wait_for_next`]'s `turn_end_due`, a
    /// [`TurnEndAction::WaitUntil`] with `retry`), as a unix second,
    /// and why. Told when the deadline changes or the wall clock and the
    /// loop's (which stops while the machine sleeps) drift apart, never on
    /// every step, so a tab can show `→ 14:05 · 3m00s` without ticking.
    /// `None` when no retry wait stands (the try is being typed, the worker
    /// works, the point waits for another reason, or the loop ended).
    /// Reporting only. The default ignores it.
    fn waiting(&self, plan: Option<WaitPlan>) {
        let _ = plan;
    }

    /// AN ACT OF THE HARNESS'S, TOLD AS IT IS TAKEN
    /// (`docs/DESIGN-harness-autopilot-2026-10-15.md` §9.2.3): one call per
    /// ledger row that names an [`Act`](crate::harness::acts::Act), from the
    /// loop's single ledger writer, so the window can light its Autopilot
    /// chip for a moment. The ledger stays the record of every act; this is a
    /// view of one. Reporting only. The default ignores it, and so a CLI
    /// `watch` or `supervise`, which has no host, pays nothing.
    fn act_taken(&self, act: crate::harness::acts::Act) {
        let _ = act;
    }

    /// THE TASK'S FACE, TOLD AS IT MOVES (§9.4.2): open, landed, done, spent
    /// or theirs ([`TaskFace`](crate::harness::acts::TaskFace)), told once at
    /// the loop's start and then on each change, so the window's chip and
    /// `status task=` say how a landing ended. Reporting only: nothing the
    /// loop decides reads it back. The default ignores it.
    fn task_face(&self, face: crate::harness::acts::TaskFace) {
        let _ = face;
    }

    /// The line a person runs to resume the agent's own conversation after a
    /// restart (`claude --resume <id>` with its launch flags, a `--model` the
    /// conversation's record marks failed left off,
    /// [`crate::harness::upgrade_drive::resume_command_kept`]), or `None` when
    /// the host read none. Named where this host would not carry the explicit
    /// restart ([`Self::restart_offered`]); never `claude --continue`, which picks
    /// the directory's newest conversation, possibly a sibling tab's. The
    /// default: none.
    fn resume_command(&self) -> Option<String> {
        None
    }

    /// What this host measures of the agent's route to its API ([`Reach`]):
    /// `Down` only on a definite failure, `Up` only on a completed verified
    /// handshake, `since` the start of the measure's run (so only a new run is
    /// an edge). The loop asks only at a wall that never reached the API, was
    /// cut off, or refused a certificate or proxy ([`Session::reach_seen`]),
    /// at the point and each step of its wait: asking keeps the host's probe
    /// running, so it is never asked elsewhere. The default measures nothing
    /// ([`Reach::Unknown`], the time ladder).
    fn reach(&self) -> Reach {
        Reach::Unknown
    }

    /// What this host measures of the agent's memory
    /// ([`super::super::memory::AgentMemory`]): the agent's process-group footprint,
    /// the machine's memory, pressure and swap rate, and the largest other
    /// supervised agent, read at most every [`super::super::memory::SAMPLE_EVERY`],
    /// off the window's thread, spawning nothing. Judged against `[harness]
    /// memory_ceiling` ([`super::super::memory::judge`]): over the ceiling is a
    /// warning, said once an episode and on each verdict change
    /// ([`Self::warn`]); only a measure that
    /// [`super::super::memory::MemoryOver::acts`] (the machine in trouble, this agent
    /// the largest) is, at an idle point, the memory wall
    /// ([`super::super::policy::turn_end::TurnEndReading::wall`]). The default
    /// measures nothing.
    fn memory(&self) -> Option<super::super::memory::AgentMemory> {
        None
    }

    /// A warning for a person from the session's supervisor, nothing asked or
    /// held back: shown as a warning row in the window's Messages band
    /// ([`super::super::memory::MemoryOver::warning`]). The default: [`Self::inform`].
    fn warn(&self, text: &str) {
        self.inform(text);
    }

    /// A point the host asked for was withheld, and by which guard
    /// ([`Guard`]): said only while the host [`Self::wants`] a point and only
    /// when the guard changes, so a long turn is one call. A running
    /// foreground turn is `NoBackgroundWait`. Reporting only (the window's host
    /// records it on the upgrade's record). The default ignores it.
    fn withheld(&self, guard: Guard) {
        let _ = guard;
    }

    /// A RATING SURVEY THE LOOP CANNOT CLOSE, or its end (`None`): told once when
    /// it begins ([`StuckSurvey`], with the sentence the journal carries) and once
    /// when the card leaves. Owner rule: it is a fact the owner may
    /// act on (press `0` in that tab) but not a reason to mark the tab — the
    /// card blocks nothing the worker does — so the loop writes no attention for
    /// it. The host keeps a record and, once the card has stood an hour, one
    /// dismissable line of the exceptions digest. Told only while the loop is
    /// unattended (no manager watches it). The default ignores it: the journal's
    /// `WAITING`/`CLEARED` lines say it anyway.
    fn survey_stuck(&self, stuck: Option<(StuckSurvey, &str)>) {
        let _ = stuck;
    }

    /// A POINT ONLY A PERSON CAN ANSWER stands, or has ended: the turn-end
    /// policy's [`Exception`] — why the machine cannot answer it and the one
    /// action that does — told once per change, `Some` when it appears (or
    /// changes) and `None` when it is over (the worker works again, the
    /// condition left the screen, the loop ended). The same words are the
    /// journal's `EXCEPTION …` line. A host that keeps a digest of what needs
    /// a person's look files a `Later` one here, and a host that can
    /// interrupt the owner asks a `Now` one that no escalation of the loop's
    /// carries ([`Exception::escalated`]); nothing about the loop waits on
    /// the answer. The default keeps none: the journal line is the record.
    fn exception(&self, standing: Option<&Exception>) {
        let _ = standing;
    }

    /// THE STALL CANARY'S SECOND RUNG ([`super::super::wall_memory`]): a screen no
    /// reader names has stood, the agent's own title resting on the idle glyph,
    /// for `Some(minutes)` — the screen has read busy on the agent's own
    /// background work alone (told once when the stall passes 90 minutes), or
    /// it is a dialog with no composer, box or busy row whose words have not
    /// changed (30 minutes) — and `None` when it is over (the screen moved,
    /// the agent works, the loop ended). A fault of aterm's own reading, not a
    /// wait: the host keeps a record and an error line. The third rung (three
    /// hours; an hour for a dialog) is an [`Exception`] of
    /// `ExceptionKind::Stalled` or `ExceptionKind::Dialog` through
    /// [`Self::exception`]. Nothing is typed either way. The default ignores
    /// it: the journal's `STALLED` line says it anyway.
    fn stall_unread(&self, minutes: Option<u64>) {
        let _ = minutes;
    }

    /// WHAT THE AGENT'S OWN SCREEN SAYS OF ITS ACCOUNT'S USAGE ([`UsageSeen`]:
    /// usage credits carrying the session, the vendor's warning that a window
    /// is filling), told from every read the loop makes, busy reads included:
    /// once per change, and again every
    /// [`crate::harness::accounts::CREDITS_RETELL_S`] while it says something.
    /// FOR THE RECORD (accounts v2 §3.3): credits are work going on, never a
    /// wall, and nothing the loop decides reads this — the host keeps it for
    /// the account and its digest. The default ignores it.
    fn usage_seen(&self, seen: &UsageSeen) {
        let _ = seen;
    }

    /// WHAT THIS HOST ADVISES ABOUT THE SESSION'S ACCOUNT at `point`
    /// ([`AccountAdvice`]; `docs/DESIGN-claude-accounts-2026-10-01.md` §6):
    /// the roster's judgment — move to another of the owner's Claude Code
    /// accounts, or no account is usable before an instant — for the
    /// turn-end policy to decide on
    /// ([`super::super::policy::turn_end::TurnEndReading::account`]). `None` is
    /// "stay": no roster, a session the roster does not list, nothing due.
    ///
    /// ASKED ONCE A DECISION, after the point is folded into the policy's
    /// memory, and only where the answer can decide something
    /// ([`super::super::policy::turn_end::TurnEndState::account_point`]): the reading
    /// of one point is built several times while it is decided and carried
    /// out, and the answer is kept for all of them — a host may read its
    /// roster and state file here. A wait that runs out decides the point
    /// again and asks again: that is how a session waiting on every account
    /// learns one is usable. [`AccountPoint::Wall`] carries what the SCREEN
    /// reads; that the limit is this process's own is the host's to check
    /// before it rotates for it (from a limit that is not, a host advises
    /// only what it would at a plain turn end: following the account the
    /// others are on).
    ///
    /// A HOST WHOSE EVIDENCE LAGS THE SCREEN — the limit's row not in the
    /// transcript yet — LEARNS TO LOOK AGAIN BY BEING ASKED AGAIN: it
    /// answers [`AccountAdvice::Wait`] for an instant a moment away, the
    /// wall's wait is cut to it (never nearer than a minute), and the point
    /// decided then asks this once more. How many times is the host's to
    /// count; with no such answer the next ask is the wall's own next
    /// decision, which may be its reset.
    ///
    /// Never asked under `[harness] accounts = "off"`. The default advises
    /// nothing, and with it every decision is the one made before accounts
    /// existed.
    fn account(&self, point: AccountPoint) -> Option<AccountAdvice> {
        let _ = point;
        None
    }

    /// THE ACCOUNT THIS SESSION WAS MOVED TO SERVED A TURN: the first turn
    /// asked after a relaunch this host made for
    /// [`super::super::policy::turn_end::Restart::Account`] ran and ended with no
    /// wall ([`super::super::policy::turn_end::TurnEndState::account_answer`]) —
    /// or, a bad answer of the move told already, the worker worked past it
    /// with no wall (the limit lifted, the login back), where the move ends.
    /// Told once a move; the host records the account proven, and others may
    /// follow. Never told under `[harness] accounts = "off"`, like
    /// [`Self::account_answered`]. The default ignores it.
    fn account_served(&self) {}

    /// THE ACCOUNT THIS SESSION WAS MOVED TO ANSWERED WITH A WALL OR A LOST
    /// LOGIN instead ([`AccountBad`]): told once a move, BEFORE the same
    /// point asks [`Self::account`] — so a host that strikes the account
    /// here advises the next one, or the way back, in its very next answer.
    /// A usage limit is told only after a turn was asked under the new
    /// login, and only from the screen: the host checks it against its own
    /// evidence (the limit row this process wrote) before it strikes
    /// anything. The default ignores it.
    fn account_answered(&self, bad: AccountBad) {
        let _ = bad;
    }

    /// A STAMP THAT MOVES WHEN WHAT THIS HOST WOULD ADVISE MAY HAVE CHANGED
    /// with nothing of the session's own screen moving ([`Self::account`]):
    /// the owner's `aterm harness accounts use`, a check that found a login
    /// signed in, an account another session proved or struck. A session
    /// WAITING at a usage limit is decided again only when its screen moves
    /// or its wait runs out — which may be days off — so the loop reads this
    /// at the top of each step of a wait that stands on the host's advice
    /// ([`Session::account_edge`]) and decides the point again when it has
    /// moved since the session last read it — and a session whose move this
    /// host refused for good asks again at that news, where a refusal with
    /// none after it is not asked again
    /// ([`super::super::policy::turn_end::TurnEndState::account_news`]). The first
    /// value a loop reads is no news to it (the count is the host's, since
    /// it started). It must be
    /// cheap (one atomic read): it is asked every step, and at every
    /// decision. The default never moves, and with it no wait is decided a
    /// moment sooner than before accounts existed.
    fn account_stamp(&self) -> u64 {
        0
    }

    /// THE RENEWABLE LOGIN THE SESSION RUNS ON
    /// ([`super::super::policy::turn_end::TurnEndReading::login_label`]): the
    /// roster label of a row with an email that its process's login is —
    /// the row's renewed login, or one a renewal superseded. Asked at every
    /// reading of a point under `[harness] logins`; it must be cheap where
    /// no roster lists such a row. `None` (the default): every lost login is
    /// decided as it was before renewal existed.
    fn login_label(&self) -> Option<String> {
        None
    }

    /// WHETHER THE SESSION'S PROCESS HOLDS A LOGIN A RENEWAL REPLACED
    /// ([`super::super::policy::turn_end::TurnEndReading::login_superseded`]): its
    /// renewable row ([`Self::login_label`]) is bound to the renewed login,
    /// and the process is still on the one before it. Asked only at a lost
    /// login on the screen of a session on a renewable login: at every
    /// reading of such a point (a decision's, and each read for the loop's
    /// books after its act or restart), and, while a point decided on
    /// `false` stands, once each time [`Self::account_stamp`] moves (a
    /// renewal made then decides the point again). `false` (the default):
    /// the process holds its row's own login.
    fn login_superseded(&self) -> bool {
        false
    }

    /// CLAUDE CODE SAID THE STORED LOGIN ENDS IN `days` DAYS
    /// ([`super::super::policy::turn_end::TurnEndReading::login_notice`]): told
    /// once per value a loop reads, under `[harness] logins`. A host that
    /// keeps the logins' forecast records it for the login the process holds
    /// — only where that is the row's own renewed login. The default ignores
    /// it.
    fn login_notice(&self, days: u32) {
        let _ = days;
    }

    /// CLAIM THIS SESSION'S ACCOUNT'S ONE `/usage` PROBE
    /// (`docs/SPEC-claude-accounts-probe-2026-10-01.md`, P23, P27). `Ok`:
    /// the loop types `/usage` into the session NOW, reads the Settings
    /// panel it opens and closes it ([`Session::usage_probe`]) — and tells
    /// what it read, exactly once, through [`Self::account_probed`].
    ///
    /// ASKED LAST, after every gate of the loop's own has passed — the point
    /// is a plain idle turn end whose turn was read live, this host's advice
    /// for it was [`AccountAdvice::Probe`], the composer is empty with nobody
    /// at it, the server fences a write three ways, the agent draws on the
    /// alternate screen — so a claim is never wasted on a point the loop
    /// then declines. What the loop CANNOT know is the host's to check here,
    /// under its roster's lock: that `[harness] accounts` is `early` (the
    /// loop cannot tell it from `wall`), that the roster still says the read
    /// is due, and that no session of any account claimed one within the
    /// roster's spacing (`crate::harness::accounts::claim_probe`).
    ///
    /// THE DEFAULT CLAIMS NOTHING, and with it nothing is ever typed for a
    /// probe: no roster, a host that keeps none, `accounts = "off"` or
    /// `"wall"` — every byte the loop writes is the one it wrote before the
    /// probe existed.
    ///
    /// `Ok`: the claim is this session's, and the loop types. `Err`: why not
    /// ([`ProbeNo`]), whose ONE WORD — short, stable, no account's name — the
    /// loop puts on its line about the probe it did not run (`SKIPPED …
    /// gate=claim host=<word>`: [`Session::probe_shut`]), so a reader of the
    /// journal need not find the host's log to learn which refusal it was.
    /// An owned value, not the host's own `&'static str`: a harness tape
    /// holds the answer and a replay gives it back ([`ProbeNo::word`] read
    /// back), which a borrowed word of the host's could not be.
    act fn account_probe(&self) -> Result<(), ProbeNo> {
        Err(ProbeNo::NoAccounts)
    }

    /// WHAT THE CLAIMED PROBE READ ([`Self::account_probe`] answered `Ok`):
    /// `Ok`, the settled panel's windows as the roster's sample
    /// (`crate::harness::usage::panel_sample`: the loop placed the resets on
    /// its own clock and zone table; `by` is the recorder's to stamp), or
    /// `Err`, why nothing was read ([`ProbeMiss`]). Told ONCE a claim, and
    /// BEFORE the panel's Escape, so a close that drags on never costs the
    /// sample. The host records the sample only for the process the claim
    /// was made for; the loop names no account. The default ignores it: the
    /// journal's `PROBED` line says it anyway.
    fn account_probed(&self, read: Result<&crate::harness::accounts::Sample, ProbeMiss>) {
        let _ = read;
    }

    /// MAY THIS SESSION TAKE ITS ACCOUNT'S LIMIT RESET
    /// (`docs/SPEC-claude-accounts-resets-2026-10-02.md`, R7 to R10)? Asked
    /// LAST, after every gate of the loop's own and the person's hold,
    /// immediately before the first key on the usage-limit dialog's grant row
    /// — and once more before the confirmation's Enter, where anything but
    /// [`ResetWord::Take`] keeps the reset. `ask` is the row as read: its
    /// count, its day, and that day's first second on the loop's clock.
    ///
    /// What the loop CANNOT know is the host's to check here, under its
    /// accounts lock: that `[harness] limit_reset` is on (read live), that the
    /// process runs on the default login and no other, that the wall is this
    /// process's own and one a grant clears, that it qualifies (a wall more
    /// than a day long, or a grant whose last day could end within one), and
    /// that the ONE claim for this wall is this session's
    /// (`crate::harness::accounts::claim_reset`). [`ResetWord::Take`] carries
    /// what the confirmation is then held to (`ResetAsk::go`);
    /// [`ResetWord::Again`] is asked again in a moment; [`ResetWord::No`]
    /// names why not in one word ([`ResetNo::word`]), and the dialog is then
    /// answered as it was before resets existed.
    ///
    /// THE DEFAULT TAKES NOTHING, and with it no row is taken and no Enter
    /// of a reset is ever written: no host, a host that keeps no accounts,
    /// the key off. (The one key a loop may still write for a reset is the
    /// Escape of a step an earlier loop left open.)
    act fn reset_take(&self, ask: &ResetAsk) -> ResetWord {
        let _ = ask;
        ResetWord::No(ResetNo::NoHost)
    }

    /// THE STEP A TAKEN RESET OPENED HAS ENDED ([`Self::reset_take`] answered
    /// [`ResetWord::Take`], and the dialog is gone): `end` says whether the
    /// confirmation's Enter was written, why the loop kept it where it did,
    /// and the offer an exactly-read confirmation showed. The host reads the
    /// vendor's ONE answer line from the transcript — never the screen — and
    /// records it for the claim it holds: [`ResetOutcome::Pending`] while no
    /// line is written yet (asked again, the last time with `end.last`), else
    /// what the line says. Only `Limits reset · …` is a reset spent.
    ///
    /// The default reads nothing ([`ResetOutcome::Unread`]): the loop then
    /// owes the session one continuation, as after any reset that was kept.
    fn reset_told(&self, end: &ResetEnd) -> ResetOutcome {
        let _ = end;
        ResetOutcome::Unread
    }
}
}

#[cfg(test)]
#[path = "host_door_tests.rs"]
mod tests;
