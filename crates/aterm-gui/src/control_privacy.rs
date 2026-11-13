// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm ctl privacy` and `await consent`
//! (`docs/DESIGN-macos-tcc-prompts-2026-08-30.md` §5.1, §5.3).

use std::time::{Duration, Instant};

use aterm_containment::consent::{
    Attribution, DebuggerMode, DebuggerProbe, FdaProbe, FdaState, FsConsent, ProbeLabel,
};
use aterm_window::event_loop::EventLoopProxy;

use crate::consent_state::{
    ChangeKind, ConsentPolicy, ConsentState, session_attribution, session_posture,
};
use crate::control::control_media::MainHopError;
use crate::control::{Scope, cross_session_authorized};
use crate::privacy_report::{PrivacyAppFacts, PrivacyForm, cmd_privacy_json};
use crate::signing_identity::signing_identity;
use crate::{App, Wake};

const AWAIT_CONSENT_DEFAULT_MS: u64 = 300_000;

const AWAIT_MAX_MS: u64 = 600_000;

const CONSENT_EXIT_POLL: Duration = crate::control::HANGUP_POLL;

const NO_SELECTOR: &str = "ERR privacy is instance-wide and takes no selector\n";

/// `privacy [--json]`: the macOS consent posture, instance-wide.
pub(crate) fn cmd_privacy(
    json: bool,
    rest: &str,
    scope: Scope,
    proxy: &EventLoopProxy<Wake>,
) -> String {
    let form = match parse_privacy_form(json, rest) {
        Ok(form) => form,
        Err(usage) => return usage,
    };
    privacy(
        form,
        || {
            crate::control::control_media::call_main(proxy, |tx| Wake::ReadPrivacy {
                scope,
                reply: tx,
            })
        },
        || {
            cfg!(target_os = "macos")
                .then(|| crate::sys_host::SYS.program_check_now())
                .flatten()
        },
    )
}

/// The report from the main hop's facts and `held`, this user's programs macOS's program check holds now and how many of them are stranded, read here on the control worker since it may pull a reading.
fn privacy(
    form: PrivacyForm,
    hop: impl FnOnce() -> Result<PrivacyAppFacts, MainHopError>,
    held: impl FnOnce() -> Option<crate::vitals_jobs::HeldCount>,
) -> String {
    let identity = signing_identity();
    let facts = match hop() {
        Ok(facts) => facts,
        Err(e) => return format!("ERR {e}\n"),
    };
    let snapshot = facts.with_program_check(held()).report(identity);
    match form {
        PrivacyForm::Json => cmd_privacy_json(&snapshot),
        PrivacyForm::Lines => {
            let lines = snapshot.lines();
            let mut out = format!("OK {}\n", lines.len());
            for line in lines {
                out.push_str(&line);
                out.push('\n');
            }
            out
        }
    }
}

fn parse_privacy_form(json: bool, rest: &str) -> Result<PrivacyForm, String> {
    let unknown: Vec<&str> = rest.split_whitespace().collect();
    if !unknown.is_empty() {
        return Err(format!(
            "ERR usage: privacy [--json] (got {:?})\n",
            unknown.join(" ")
        ));
    }
    Ok(if json {
        PrivacyForm::Json
    } else {
        PrivacyForm::Lines
    })
}

/// The refusal a selector earns on this verb.
pub(crate) const fn no_selector_error() -> &'static str {
    NO_SELECTOR
}

/// The tuple `await consent` arms on and latches a change against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConsentTuple {
    /// This session's Full Disk Access tuple: the bare form.
    Fda {
        fs_consent: FsConsent,
        fda: FdaState,
        attribution: Attribution,
    },
    /// The debugger rule's tuple.
    Debugger { mode: DebuggerMode },
}

impl std::fmt::Display for ConsentTuple {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fda {
                fs_consent,
                fda,
                attribution,
            } => write!(
                f,
                "fs_consent={} fda={} attribution={}",
                fs_consent.as_str(),
                fda.as_str(),
                attribution.as_str()
            ),
            Self::Debugger { mode } => write!(f, "debugger_mode={}", mode.as_str()),
        }
    }
}

/// One reading of what a consent wait watches.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ConsentObservation {
    /// No tuple yet: a probe still out, or a reading that observed nothing.
    Pending,
    Complete(ConsentTuple),
    /// The addressed session ended.
    Exited,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ConsentWaitDecision {
    Wait,
    Changed(ConsentTuple),
    Exited,
    TimedOut,
}

pub(crate) struct ConsentWait {
    baseline: Option<ConsentTuple>,
    /// When the request arrived; `elapsed_ms=` counts from it.
    armed: Instant,
    deadline: Instant,
}

impl ConsentWait {
    pub(crate) fn new(armed: Instant, timeout: Duration) -> Self {
        Self {
            baseline: None,
            armed,
            deadline: armed + timeout,
        }
    }

    fn expired_at(&self, now: Instant) -> bool {
        now >= self.deadline
    }

    /// Pending is not an observed change or an exit.
    pub(crate) fn observe_at(
        &mut self,
        observation: ConsentObservation,
        now: Instant,
    ) -> ConsentWaitDecision {
        if self.expired_at(now) {
            return ConsentWaitDecision::TimedOut;
        }
        match observation {
            ConsentObservation::Pending => ConsentWaitDecision::Wait,
            ConsentObservation::Exited => ConsentWaitDecision::Exited,
            ConsentObservation::Complete(tuple) => match self.baseline {
                Some(baseline) if baseline != tuple => ConsentWaitDecision::Changed(tuple),
                Some(_) => ConsentWaitDecision::Wait,
                None => {
                    self.baseline = Some(tuple);
                    ConsentWaitDecision::Wait
                }
            },
        }
    }
}

fn fda_observation(
    policy: &ConsentPolicy,
    observed: Attribution,
    probe: FdaProbe,
) -> ConsentObservation {
    if probe.label == ProbeLabel::Pending {
        return ConsentObservation::Pending;
    }
    let posture = session_posture(policy, observed, probe.state);
    ConsentObservation::Complete(ConsentTuple::Fda {
        fs_consent: posture.fs_consent,
        fda: probe.state,
        attribution: posture.attribution,
    })
}

const fn debugger_observation(probe: DebuggerProbe) -> ConsentObservation {
    match probe.mode {
        DebuggerMode::Unknown => ConsentObservation::Pending,
        mode => ConsentObservation::Complete(ConsentTuple::Debugger { mode }),
    }
}

/// Read one session's Full Disk Access tuple off the event loop.
pub(crate) fn observe_fda(
    consent: &ConsentState,
    observed: Attribution,
    now: Instant,
) -> (ConsentObservation, Option<Instant>) {
    let policy = consent.policy();
    let (probe, _age) = consent.fda_off_loop(policy.gate, policy.interval);
    let due = if policy.gate.permits() {
        consent.next_refresh_deadline(now, policy.interval)
    } else {
        None
    };
    (fda_observation(&policy, observed, probe), due)
}

/// Read the debugger rule's tuple off the event loop.
pub(crate) fn observe_debugger(
    consent: &ConsentState,
    now: Instant,
) -> (ConsentObservation, Option<Instant>) {
    let gate = consent.policy().gate;
    let row = consent.debugger(gate);
    let due = if gate.permits() {
        consent.next_debugger_refresh_deadline(now)
    } else {
        None
    };
    (debugger_observation(row.probe()), due)
}

struct WaitEnv<'a> {
    /// The clock the deadline and `elapsed_ms=` are read on.
    now: &'a dyn Fn() -> Instant,
    /// Whether the addressed session ended (the store's exit probe).
    exited: &'a dyn Fn() -> bool,
    /// Whether the caller closed its connection ([`crate::control::caller_hung_up`]).
    hung_up: &'a dyn Fn() -> bool,
    /// How long a requested park may block ([`crate::control::hangup_park`]).
    park: &'a dyn Fn(Duration) -> Duration,
}

fn run_consent_wait(
    mut wait: ConsentWait,
    watch: &[ChangeKind],
    consent: &ConsentState,
    mut observe: impl FnMut(Instant) -> (ConsentObservation, Option<Instant>),
    env: &WaitEnv<'_>,
) -> String {
    loop {
        let now = (env.now)();
        if wait.expired_at(now) {
            return "OK timeout\n".to_string();
        }
        let seen = consent.publication();
        let (observation, refresh_due) = if (env.exited)() {
            (ConsentObservation::Exited, None)
        } else {
            observe(now)
        };
        let now = (env.now)();
        match wait.observe_at(observation, now) {
            ConsentWaitDecision::Changed(tuple) => {
                return format!(
                    "OK consent {tuple} elapsed_ms={}\n",
                    now.saturating_duration_since(wait.armed).as_millis()
                );
            }
            ConsentWaitDecision::Exited => return "ERR exited\n".to_string(),
            ConsentWaitDecision::TimedOut => return "OK timeout\n".to_string(),
            ConsentWaitDecision::Wait => {}
        }
        if (env.hung_up)() {
            return crate::control::HUNG_UP_REPLY.to_string();
        }
        let until = refresh_due.map_or(wait.deadline, |due| due.min(wait.deadline));
        let park = until.saturating_duration_since(now).min(CONSENT_EXIT_POLL);
        consent.wait_for_change(seen, watch, (env.park)(park));
    }
}

/// What `await consent`'s one main-thread hop brings back ([`App::arm_consent_wait`]).
#[derive(Debug)]
pub(crate) struct ConsentArm {
    /// The instance's consent state, observed from the control worker from then on.
    consent: ConsentState,
    /// The target's adoption record; `None` when it is not in the pool.
    adoption: Option<Attribution>,
}

impl App {
    /// `await consent`'s one main-thread hop (`Wake::ArmConsentWait`).
    pub(crate) fn arm_consent_wait(&self, session: u64) -> ConsentArm {
        ConsentArm {
            consent: self.consent.clone(),
            adoption: self.pool.get(session).map(session_attribution),
        }
    }
}

fn await_consent(
    args: ConsentArgs,
    authorized: bool,
    arm: impl FnOnce() -> Result<ConsentArm, MainHopError>,
    env: &WaitEnv<'_>,
) -> String {
    let wait = ConsentWait::new((env.now)(), Duration::from_millis(args.timeout_ms));
    if !authorized {
        return "ERR exited\n".to_string();
    }
    let arm = match arm() {
        Ok(arm) => arm,
        Err(e) => return format!("ERR {e}\n"),
    };
    let Some(adoption) = arm.adoption else {
        return "ERR exited\n".to_string();
    };
    let consent = &arm.consent;
    match args.kind {
        ConsentKind::Fda => run_consent_wait(
            wait,
            &[ChangeKind::Fda, ChangeKind::Policy],
            consent,
            |now| observe_fda(consent, adoption, now),
            env,
        ),
        ConsentKind::Debugger => run_consent_wait(
            wait,
            &[ChangeKind::Debugger, ChangeKind::Policy],
            consent,
            |now| observe_debugger(consent, now),
            env,
        ),
    }
}

/// `await consent [kind=debugger] [timeout=<ms>]` — wait for an observed tuple to change.
pub(crate) fn cmd_await_consent(
    proxy: &EventLoopProxy<Wake>,
    scope: Scope,
    session: u64,
    ctx: &crate::SessionCtx,
    exited: &dyn Fn() -> bool,
    rest: &str,
) -> String {
    let args = match parse_consent_args(rest) {
        Ok(args) => args,
        Err(usage) => return usage,
    };
    let authorized =
        args.kind == ConsentKind::Debugger || cross_session_authorized(scope, "privacy", ctx);
    await_consent(
        args,
        authorized,
        || {
            crate::control::control_media::call_main(proxy, |tx| Wake::ArmConsentWait {
                session,
                reply: tx,
            })
        },
        &WaitEnv {
            now: &Instant::now,
            exited,
            hung_up: &crate::control::caller_hung_up,
            park: &crate::control::hangup_park,
        },
    )
}

/// Which tuple `await consent` waits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConsentKind {
    /// This session's Full Disk Access tuple — the bare form.
    Fda,
    /// The debugger rule's tuple — `kind=debugger`.
    Debugger,
}

/// `await consent`'s parsed arguments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ConsentArgs {
    kind: ConsentKind,
    timeout_ms: u64,
}

fn parse_consent_args(rest: &str) -> Result<ConsentArgs, String> {
    const USAGE: &str = "ERR usage: await consent [kind=debugger] [timeout=<ms>]\n";
    let toks: Vec<&str> = rest.split_whitespace().collect();
    let mut args = ConsentArgs {
        kind: ConsentKind::Fda,
        timeout_ms: AWAIT_CONSENT_DEFAULT_MS,
    };
    let mut i = 0;
    if toks.first() == Some(&"consent") {
        i = 1;
    }
    while i < toks.len() {
        if toks[i] == "kind=debugger" {
            args.kind = ConsentKind::Debugger;
            i += 1;
        } else if let Some(v) = toks[i].strip_prefix("timeout=") {
            args.timeout_ms = v
                .bytes()
                .all(|b| b.is_ascii_digit())
                .then(|| v.parse().ok())
                .flatten()
                .ok_or_else(|| USAGE.to_string())?;
            i += 1;
        } else if toks[i] == "timeout" && i + 1 < toks.len() {
            let v = toks[i + 1];
            args.timeout_ms = v
                .bytes()
                .all(|b| b.is_ascii_digit())
                .then(|| v.parse().ok())
                .flatten()
                .ok_or_else(|| USAGE.to_string())?;
            i += 2;
        } else {
            return Err(USAGE.to_string());
        }
    }
    args.timeout_ms = args.timeout_ms.min(AWAIT_MAX_MS);
    Ok(args)
}

#[cfg(test)]
#[path = "control_privacy_tests.rs"]
mod tests;
