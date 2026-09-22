// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The limit and capacity classifier and its recovery table (design §5.8;
//! `FailureRecovery` in §11 item 7).
//!
//! Three things live here, each a pure function of its arguments — no clock
//! is read (`now` is injected everywhere), no environment, no network, no
//! control socket, no process is spawned:
//!
//! 1. **The classifier** ([`classify`]): the evidence aterm read off its own
//!    grid, or the vendor wrote to a hook, a statusLine or its cache, read
//!    into one of the eight [`Class`]es of §5.8.2, with the two-source rule
//!    decided at the same time ([`Classification::unpaired`]).
//! 2. **The table** ([`ActionTable`], [`LimitsConfig`]): the ordered
//!    candidates per class, in the closed [`Action`] vocabulary of §5.8.4,
//!    with the per-class ALLOWED SETS enforced by [`ActionTable::validate`]
//!    so the §11 invariants (no `switch-*` for `network-offline`, `unknown`
//!    or `model-bucket-limit`; no `retry` for `unknown`; `spend-billing`
//!    pinned to `["escalate"]`; `auth` pinned inside `["relogin",
//!    "escalate"]`) hold by construction, not by default.
//! 3. **The engine** ([`step`], [`State`], [`Guards`]): one decision at a
//!    time — [`Decision::Act`], [`Decision::Refused`] or
//!    [`Decision::Wait`] — with never two automatic actions in flight, the
//!    dwell, the one shared switch budget, stale generations refused, and the
//!    T1/T2 timers of §5.8.4 per class.
//!
//! # What is measured and what is assumed
//!
//! The hook literals (the closed `StopFailure.error` list, the
//! `quota_auto_resume_*` notification family, the `PostModelSwitch` fields)
//! and the banner phrases are MEASURED in the claude 2.1.274 store binary
//! (design §5.8.1–§5.8.3, `grep -c -F` over its `strings` dump). Whether a
//! usage-limit turn fires `StopFailure` at all, the `Notification` payload
//! shape, and the `PostModelSwitch.source` values are UNVERIFIED (§5.8.9);
//! this module treats each as a value it may or may not receive, never as a
//! value it can rely on. Nothing here has been run against a live limit.
//!
//! # Reuse
//!
//! Screen evidence is read by `aterm_phase::phase::limit_notice` (re-exported
//! under [`crate::supervise::phase`]), and a banner's reset time by
//! [`crate::supervise::limit`]'s `parse_reset` / `reset_at`; see
//! [`Evidence::banner_from_screen`] and [`banner_reset_at`]. Neither reader is
//! re-implemented here. The window source vocabulary is
//! [`super::source::Source`].
//!
//! # The grid is the spine, and what the two-source rule still binds
//!
//! **Narrowed 2026-09-19 under design §0.2, and this module enforced the
//! pre-inversion rule until then.** aterm's own view — `status` plus the
//! parsed grid through the shipped readers — is RANK 1 and is the spine;
//! hooks, the statusLine and the cache are ENRICHMENT that raise confidence
//! and are never load-bearing. So a class named by the grid ALONE is a real
//! class: `let-vendor-retry`, `retry`, `wait`, `switch-model`, `relogin`,
//! `escalate` and every L1 display run on it, at the confidence that
//! evidence carries.
//!
//! What still needs two independent sources is exactly the four actions that
//! **spend money, spend an allowance, or move an account** —
//! [`Action::SwitchAccount`], [`Action::LowerPriority`],
//! [`Action::LimitReset`], [`Action::ExtraUsage`]
//! ([`Action::needs_two_sources`]) — where a wrong classification costs
//! something no later evidence can give back. The old rule bound EVERY
//! action above L1, which meant a session with zero hooks (a `--bare`
//! launch, one renamed vendor event, every adopted session) could never rise
//! above display-only and could never act at all.
//!
//! # Every tie breaks toward the safe answer
//!
//! An unrecognised `StopFailure.error` forces [`Class::Unknown`] regardless
//! of agreeing windows or banners (a hook rename fails closed). `rate_limit`
//! with no READABLE window is `unknown`, never `transient-capacity`. A dry
//! budget degrades a switch to its L1 half, and never makes an un-due
//! candidate due: the class's timing gate is asked first. `extra-usage` is
//! unreachable in this ABI whatever `allow_spend` says. When in doubt the
//! engine escalates or does nothing; it never approves. And what a row SAYS
//! is never what the harness types: a grid row is believed as evidence about
//! what was drawn and never obeyed as an instruction (§4.4).
//!
//! The class and the table cursor are ONE coupled value: every path that
//! changes [`State::class`] goes through the same private `adopt`, which puts
//! the cursor back at the new row's beginning. A class that moved under a
//! cursor that did not is how the fail-closed rename ended in silence rather
//! than in `escalate` — see [`State::observe`].
//!
//! STATUS (docs/README.md honesty ratchet): unit-tested; the bounded machine
//! carries a derived `ty_model!` in `aterm-spec`
//! (`harness_failure_recovery_model`, Tier-0 by the in-process interpreter and
//! by `ty` where installed) and a Tier-1 bind in
//! `aterm-agent/tests/conformance_harness.rs` that drives this engine and
//! checks each transition against it. This module is REACHED from the front
//! door as of 2026-09-22 — `aterm harness limits` prints [`classify`]'s
//! verdict, `harness recover` runs one class's table or one step through the
//! guarded path, and [`super::watch`] drives [`step`] on the loop — which
//! corrects the line that stood here saying nothing was wired into a verb.
//! What is NOT reached: no control-socket verb answers any of it (design
//! §5.7), and nothing here has ever run against a REAL limit — the ladder is
//! exercised against fixtures and a fake wire only (design §10.1, the P2
//! exit criterion).

use std::fmt;

use super::source::Source;
use super::usage;
use crate::supervise::limit::{parse_reset, reset_at};
use crate::supervise::phase::limit_notice;

// ---------------------------------------------------------------------------
// Classes and evidence
// ---------------------------------------------------------------------------

/// The eight failure classes of design §5.8.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Class {
    /// A 529 / 5xx the vendor is already retrying, or a `rate_limit` that is
    /// a throttle (every readable window below the threshold).
    TransientCapacity,
    /// The turn never reached the API.
    NetworkOffline,
    /// The account-wide five-hour window is exhausted.
    Session5hLimit,
    /// The account-wide seven-day window (or a per-model seven-day window
    /// the statusLine can read) is exhausted.
    Weekly7dLimit,
    /// The vendor-owned `seven_day_overage_included` ("Fable limit") window:
    /// observe-only, its dialog and its live substitution are the vendor's.
    ModelBucketLimit,
    /// Billing, spend caps, holds and verification: money, a human's call.
    SpendBilling,
    /// An expired or invalid login.
    Auth,
    /// Anything else, and every fail-closed outcome.
    Unknown,
}

impl Class {
    /// Every class, in table order.
    pub const ALL: [Class; 8] = [
        Class::TransientCapacity,
        Class::NetworkOffline,
        Class::Session5hLimit,
        Class::Weekly7dLimit,
        Class::ModelBucketLimit,
        Class::SpendBilling,
        Class::Auth,
        Class::Unknown,
    ];

    /// The config-file spelling (design §5.8.7).
    pub fn as_str(self) -> &'static str {
        match self {
            Class::TransientCapacity => "transient-capacity",
            Class::NetworkOffline => "network-offline",
            Class::Session5hLimit => "session-5h-limit",
            Class::Weekly7dLimit => "weekly-7d-limit",
            Class::ModelBucketLimit => "model-bucket-limit",
            Class::SpendBilling => "spend-billing",
            Class::Auth => "auth",
            Class::Unknown => "unknown",
        }
    }

    /// The class a config-file spelling names, if any.
    pub fn parse(name: &str) -> Option<Class> {
        Class::ALL.into_iter().find(|c| c.as_str() == name)
    }

    fn index(self) -> usize {
        match self {
            Class::TransientCapacity => 0,
            Class::NetworkOffline => 1,
            Class::Session5hLimit => 2,
            Class::Weekly7dLimit => 3,
            Class::ModelBucketLimit => 4,
            Class::SpendBilling => 5,
            Class::Auth => 6,
            Class::Unknown => 7,
        }
    }

    /// Classes whose table may never name a `switch-*` action (§11 item 7).
    pub fn switch_allowed(self) -> bool {
        !matches!(
            self,
            Class::NetworkOffline | Class::Unknown | Class::ModelBucketLimit
        )
    }
}

impl fmt::Display for Class {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The closed `StopFailure.error` value list, as the vendor's matcher
/// declares it (design §5.8.1, MEASURED). A value outside it forces
/// [`Class::Unknown`] for the whole classification.
pub const STOP_FAILURE_ERRORS: [&str; 13] = [
    "rate_limit",
    "overloaded",
    "authentication_failed",
    "oauth_org_not_allowed",
    "account_on_hold",
    "verification_required",
    "billing_error",
    "invalid_request",
    "model_not_found",
    "server_error",
    "max_output_tokens",
    "cloud_credential_error",
    "unknown",
];

/// A window a source of design §5.8.1 can report.
///
/// The Fable window `seven_day_overage_included` was deliberately ABSENT here
/// until 2026-09-22, on §5.8.2's ground that it is header-only and therefore
/// never readable, "so there is no way to hand one in". That is now MEASURED
/// false, and the correction is this variant: Claude Code 2.1.278 PAINTS the
/// bucket on its `/usage` panel as `Current week (Fable)` (captured on a real
/// screen at `100% used · Resets Sep 23 at 11:59am`), which
/// [`super::usage::usage_panel_windows`] reads and
/// [`Evidence::windows_from_screen`] hands in at [`Source::Grid`]. The
/// statusLine and the cache still cannot carry it, so the grid is its ONLY
/// source — the sharpest case for §5.8.1's inverted ranking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WindowKind {
    FiveHour,
    SevenDay,
    SevenDayOpus,
    SevenDaySonnet,
    SevenDayOverageIncluded,
    SpendLimit,
}

impl WindowKind {
    /// Every kind, in key order.
    pub const ALL: [WindowKind; 6] = [
        WindowKind::FiveHour,
        WindowKind::SevenDay,
        WindowKind::SevenDayOpus,
        WindowKind::SevenDaySonnet,
        WindowKind::SevenDayOverageIncluded,
        WindowKind::SpendLimit,
    ];

    /// The statusLine / cache / panel key.
    pub fn as_str(self) -> &'static str {
        match self {
            WindowKind::FiveHour => "five_hour",
            WindowKind::SevenDay => "seven_day",
            WindowKind::SevenDayOpus => "seven_day_opus",
            WindowKind::SevenDaySonnet => "seven_day_sonnet",
            WindowKind::SevenDayOverageIncluded => "seven_day_overage_included",
            WindowKind::SpendLimit => "spend_limit",
        }
    }

    /// The kind a statusLine / cache / panel key names, if any.
    pub fn parse(key: &str) -> Option<WindowKind> {
        WindowKind::ALL.into_iter().find(|k| k.as_str() == key)
    }

    /// The limit class an exhausted window of this kind names.
    fn exhausted_class(self) -> Class {
        match self {
            WindowKind::FiveHour => Class::Session5hLimit,
            WindowKind::SevenDay | WindowKind::SevenDayOpus | WindowKind::SevenDaySonnet => {
                Class::Weekly7dLimit
            }
            // The vendor owns this bucket's dialog and its live substitution
            // (§5.8.2): the harness OBSERVES it and acts on nothing.
            WindowKind::SevenDayOverageIncluded => Class::ModelBucketLimit,
            WindowKind::SpendLimit => Class::SpendBilling,
        }
    }
}

/// One piece of evidence, from one of the sources of design §5.8.1. There is
/// no transcript-row variant on purpose: the transcript's error row is the
/// same vendor event as the hook and never a second source (§5.8.2).
#[derive(Debug, Clone, PartialEq)]
pub enum Evidence {
    /// The `StopFailure` hook: `error` is the matcher value, `details` the
    /// `error_details` string when the payload carried one.
    StopFailure {
        error: String,
        details: Option<String>,
    },
    /// The `Notification` hook's `notification_type`.
    Notification { kind: String },
    /// One rate-limit window from the statusLine or the cache.
    Window {
        which: WindowKind,
        /// Percent used as reported; `None` when the sample had no figure.
        used_pct: Option<f64>,
        /// Epoch seconds at which the window resets, when reported.
        resets_at: Option<i64>,
        source: Source,
        /// Seconds since the sample, when known.
        age_s: Option<u64>,
    },
    /// The `PostModelSwitch` hook: a switch the harness may not have made.
    PostModelSwitch {
        from: String,
        to: String,
        /// The hook's `source` field; its values are UNVERIFIED.
        source: Option<String>,
    },
    /// A screen banner as `limit_notice` hands it over, with its reset time
    /// already placed on the clock by [`banner_reset_at`].
    ///
    /// RANK 1 (design §5.8.1, inverted 2026-09-19): aterm drew this frame,
    /// so it survives `--bare`, a user statusLine, a hook rename and a
    /// program with no hooks at all. It names a class on its own and it
    /// completes a pair with any source that is not the same grid frame.
    /// What it never does is get OBEYED: its text is never typed back, and
    /// it is never a second source beside a [`Source::Grid`] window,
    /// because two readings of one frame are one source.
    Banner {
        text: String,
        resets_at: Option<i64>,
    },
}

impl Evidence {
    /// Read the screen for a limit notice through
    /// `aterm_phase::phase::limit_notice` and place its reset on the clock
    /// through [`banner_reset_at`]. `None` when the screen shows no notice.
    pub fn banner_from_screen(
        rows: &[String],
        now: i64,
        local_offset_s: i64,
        zone_offset: fn(&str) -> Option<i64>,
    ) -> Option<Evidence> {
        let (text, reset) = limit_notice(rows)?;
        let resets_at = reset.and_then(|r| banner_reset_at(&r, now, local_offset_s, zone_offset));
        Some(Evidence::Banner { text, resets_at })
    }

    /// The rate-limit windows the vendor PAINTED on its `/usage` panel, read
    /// off the same rows, through [`super::usage::usage_panel_windows`], with
    /// each painted reset placed on the clock by [`banner_reset_at`] — the
    /// same placer the banner uses, not a second one.
    ///
    /// This is the producer that was missing: before it, [`Source::Grid`]
    /// was admissible and ranked and nothing in the crate constructed one, so
    /// the spine could name a limit class and never supply the window figure
    /// that pairs with it. Each window is `age_s = 0` because a grid read is
    /// the instant it was taken.
    ///
    /// A panel key no [`WindowKind`] knows is DROPPED rather than guessed at:
    /// an unmeasured `Current week (<display>)` row is a figure with no
    /// vendor key, and a wrong key is worse than a missing one.
    #[must_use]
    pub fn windows_from_screen(
        rows: &[String],
        now: i64,
        local_offset_s: i64,
        zone_offset: fn(&str) -> Option<i64>,
    ) -> Vec<Evidence> {
        usage::usage_panel_windows(rows)
            .into_iter()
            .filter_map(|w| {
                Some(Evidence::Window {
                    which: WindowKind::parse(&w.name)?,
                    used_pct: Some(f64::from(w.used_pct)),
                    resets_at: w
                        .reset_text
                        .as_deref()
                        .and_then(|r| banner_reset_at(r, now, local_offset_s, zone_offset)),
                    source: Source::Grid,
                    age_s: Some(0),
                })
            })
            .collect()
    }
}

/// The reset time a banner names, as Unix seconds: the text after `resets ` /
/// `reset at ` (or the auto-continue notice's time) that `limit_notice`
/// handed over, read by `parse_reset` and placed by `reset_at`. `None` when
/// the text is not a reset the supervisor's clock knows.
pub fn banner_reset_at(
    reset: &str,
    now: i64,
    local_offset_s: i64,
    zone_offset: fn(&str) -> Option<i64>,
) -> Option<i64> {
    parse_reset(reset).map(|spec| reset_at(&spec, now, local_offset_s, zone_offset))
}

/// A window at or above this percent is exhausted for classification
/// (design §5.8.2: "any window with `source ≠ none` at ≥ 95 %").
pub const EXHAUSTED_PCT: f64 = 95.0;

/// The `error_details` / transcript literal that marks the Fable window.
const MODEL_BUCKET_MARKER: &str = "model_requires_usage_credits";

/// Connection-error markers, from the vendor's own copy and the runtime's
/// errno names (design §5.8.2 `network-offline`).
const NETWORK_MARKERS: [&str; 12] = [
    "network_error",
    "network error",
    "check your internet",
    "no internet",
    "temporary network issue",
    "no response from the api",
    "econnreset",
    "etimedout",
    "enotfound",
    "eai_again",
    "enetunreach",
    "dns",
];

/// The vendor's own "gave up after repeated 529s" banner.
const STORM_MARKER: &str = "repeated 529";

/// The vendor's first-byte timeout banner, the one case `network-offline`
/// may `retry` (design §5.8.4).
const NO_RESPONSE_MARKER: &str = "no response from the api after";

/// Overloaded `StopFailure`s that make a storm (design §5.8.4 T1).
pub const STORM_COUNT: u32 = 3;

/// How long a hit is kept in [`State::hits`], in seconds.
///
/// Two days, the same slack [`Ledger::spend`] uses, and far wider than either
/// reader's own window — the 600 s storm window and
/// `LimitsConfig::unknown_escalate_after.window_s` — so retention can never
/// take a hit a reader still counts. The vector was previously unbounded for
/// the life of a class, and both readers walk it linearly per event.
pub const HITS_RETAIN_S: i64 = 2 * 86_400;

/// What the classifier decided.
#[derive(Debug, Clone, PartialEq)]
pub struct Classification {
    pub class: Class,
    /// `true` when ONE independent source named this class.
    ///
    /// It is not "display only" and it has not been since the two-source
    /// rule was narrowed (design §5.8.2, 2026-09-19): every reversible
    /// action still runs on a single rank-1 source. What it withholds is
    /// exactly the four actions [`Action::needs_two_sources`] names, which
    /// spend money, an allowance or an account.
    pub unpaired: bool,
    /// One line per fact that decided the class, for the ledger row.
    pub reasons: Vec<String>,
    /// The reset the evidence names, when any: an exhausted readable window's
    /// (statusLine over cache), else the banner's.
    pub resets_at: Option<i64>,
    /// ≥ [`STORM_COUNT`] `overloaded` failures or the vendor's `Repeated 529`
    /// banner.
    pub storm: bool,
    /// The vendor printed `No response from the API after …`.
    pub no_response: bool,
}

fn contains_ci(haystack: &str, needle: &str) -> bool {
    haystack.to_ascii_lowercase().contains(needle)
}

/// The class a `StopFailure.error` value maps to, given what the readable
/// windows say. `rate_limit` is the one value the windows decide.
fn stop_failure_class(
    error: &str,
    details: Option<&str>,
    exhausted: Option<Class>,
    any_readable: bool,
) -> Class {
    match error {
        "overloaded" | "server_error" => Class::TransientCapacity,
        "rate_limit" => {
            if details.is_some_and(|d| contains_ci(d, MODEL_BUCKET_MARKER)) {
                Class::ModelBucketLimit
            } else if let Some(class) = exhausted {
                class
            } else if any_readable {
                Class::TransientCapacity
            } else {
                Class::Unknown
            }
        }
        "billing_error" | "account_on_hold" | "verification_required" | "oauth_org_not_allowed" => {
            Class::SpendBilling
        }
        "authentication_failed" | "cloud_credential_error" => Class::Auth,
        "unknown" if details.is_some_and(|d| NETWORK_MARKERS.iter().any(|m| contains_ci(d, m))) => {
            Class::NetworkOffline
        }
        _ => Class::Unknown,
    }
}

/// The class a screen banner's words name (design §5.8.2's literal column),
/// or `None` for a notice this module cannot place.
///
/// EVERY NEEDLE IS A PHRASE, not a word. Tightened 2026-09-22: the table held
/// `weekly`, `billing`, `on hold`, `verification` and `authentication` as
/// bare substrings, and a class minted here short-circuits
/// [`super::watch::classify_liveness`] — so one ordinary English word on the
/// screen turned the stall ladder off for the class's lifetime. A phrase is
/// the smallest change that keeps the design's literal column readable while
/// making an accidental match improbable; the row-placement fence in
/// [`super::observe`] is the other half, and neither alone is enough.
pub fn banner_class(text: &str) -> Option<Class> {
    let t = text.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| t.contains(n));
    if has(&[
        "weekly limit",
        "weekly usage limit",
        "weekly rate limit",
        "7-day limit",
        "seven-day limit",
    ]) {
        Some(Class::Weekly7dLimit)
    } else if has(&["fable limit", "opus limit", "sonnet limit", "usage credits"]) {
        // Opus/Sonnet limits are per-model buckets too; escalating is the
        // safe reading until a window says otherwise.
        Some(Class::ModelBucketLimit)
    } else if has(&["session limit", "usage limit", "5-hour", "limit resets", "limit will reset"])
        // The two phrases that say the vendor goes on by itself are the
        // supervisor's reader, not a second copy of the same literals.
        || crate::supervise::limit::resumes_by_itself(text)
    {
        Some(Class::Session5hLimit)
    } else if has(&[
        STORM_MARKER,
        "overloaded",
        "high load",
        "temporarily limiting",
    ]) {
        Some(Class::TransientCapacity)
    } else if has(&[
        "run /login",
        "token expired",
        "not logged in",
        "login expired",
        "invalid api key",
        "authentication failed",
        "authentication error",
        "authentication required",
    ]) {
        Some(Class::Auth)
    } else if NETWORK_MARKERS.iter().any(|m| t.contains(m)) {
        Some(Class::NetworkOffline)
    } else if has(&[
        "spend limit",
        "credit balance",
        "monthly spend",
        "shared budget",
        "billing issue",
        "billing problem",
        "update your billing",
        "account is on hold",
        "account on hold",
        "verification required",
    ]) {
        Some(Class::SpendBilling)
    } else {
        None
    }
}

/// A readable window, after the sample has been checked.
struct Readable {
    which: WindowKind,
    pct: f64,
    resets_at: Option<i64>,
    source: Source,
}

/// Classify one generation's evidence as of `now`. `None` when nothing in
/// the slice says anything about a limit (an empty slice, or a
/// `PostModelSwitch` alone, which reconciles a switch but names no failure).
///
/// The rules, in the order they are applied: the closed `StopFailure.error`
/// list (a value outside it → `unknown`, unpaired, whatever else agrees);
/// the readable windows (`source` is `statusline`, `cache` or `grid`, a
/// figure is present, and `resets_at` has not passed); the hook value, with
/// `rate_limit` decided by the windows; then the notification family, then
/// the banner, each only when nothing higher spoke.
///
/// The pair rule counts distinct supporting [`Source`] channels and needs two of
/// them, one of which must NAME the failure ([`Source::names_the_failure`]:
/// the grid, `StopFailure` or `Notification`). Before 2026-09-19 the namer
/// had to be a HOOK value and the grid could never count at all, so a
/// zero-hook session was permanently unpaired; the grid is rank 1 now
/// (§0.2), and it is what a `--bare` launch still has.
pub fn classify(evidence: &[Evidence], now: i64) -> Option<Classification> {
    if evidence.is_empty() {
        return None;
    }
    let mut reasons = Vec::new();

    // 1. The closed list: fail closed before anything else is read.
    for e in evidence {
        if let Evidence::StopFailure { error, .. } = e
            && !STOP_FAILURE_ERRORS.contains(&error.as_str())
        {
            return Some(Classification {
                class: Class::Unknown,
                unpaired: true,
                reasons: vec![format!(
                    "StopFailure error `{error}` is outside the closed list: unknown, fail closed"
                )],
                resets_at: None,
                storm: false,
                no_response: false,
            });
        }
    }

    // 2. The readable windows.
    let mut readable: Vec<Readable> = Vec::new();
    for e in evidence {
        if let Evidence::Window {
            which,
            used_pct,
            resets_at,
            source,
            age_s,
        } = e
        {
            let src = match source {
                // `grid` joined this list on 2026-09-19 (§5.8.2, amended):
                // the window the vendor PRINTED is still readable where the
                // vendor put it, so a renamed statusLine field alone no
                // longer forces `unknown`.
                Source::StatusLine | Source::Cache | Source::Grid => *source,
                // Every OTHER source is refused BY NAME rather than by a
                // wildcard: the three readable window sources are a closed
                // list, so a source that is not one of them — a transcript
                // fold, an aterm read that carries no figure, the absence —
                // is named in the reasons and dropped. A `_ =>` here would
                // silently admit whatever the vocabulary grows next.
                _ => {
                    reasons.push(format!(
                        "window {}: source={} is not readable",
                        which.as_str(),
                        source.as_str()
                    ));
                    continue;
                }
            };
            let Some(pct) = used_pct else {
                reasons.push(format!("window {}: no figure", which.as_str()));
                continue;
            };
            if resets_at.is_some_and(|r| r <= now) {
                reasons.push(format!(
                    "window {}: resets_at {} has passed, not read",
                    which.as_str(),
                    resets_at.unwrap_or(0)
                ));
                continue;
            }
            reasons.push(format!(
                "window {}={}% source={} age_s={}",
                which.as_str(),
                pct,
                src.as_str(),
                age_s.map_or("?".to_string(), |a| a.to_string())
            ));
            readable.push(Readable {
                which: *which,
                pct: *pct,
                resets_at: *resets_at,
                source: src,
            });
        }
    }
    let any_readable = !readable.is_empty();
    // Spend over weekly over five-hour: waiting on the shorter window cannot
    // help while the longer one is exhausted.
    let exhausted: Option<Class> = [
        Class::SpendBilling,
        Class::Weekly7dLimit,
        Class::Session5hLimit,
    ]
    .into_iter()
    .find(|class| {
        readable
            .iter()
            .any(|w| w.pct >= EXHAUSTED_PCT && w.which.exhausted_class() == *class)
    });
    if let Some(class) = exhausted {
        reasons.push(format!("a readable window is >= {EXHAUSTED_PCT}%: {class}"));
    } else if any_readable {
        reasons.push(format!("every readable window is below {EXHAUSTED_PCT}%"));
    } else {
        reasons.push("no readable window".to_string());
    }

    // 3. The hook value (the LAST StopFailure decides; the count is noted).
    let stops: Vec<(&str, Option<&str>)> = evidence
        .iter()
        .filter_map(|e| match e {
            Evidence::StopFailure { error, details } => Some((error.as_str(), details.as_deref())),
            _ => None,
        })
        .collect();
    let hook_class = stops.last().map(|(error, details)| {
        let class = stop_failure_class(error, *details, exhausted, any_readable);
        reasons.push(format!(
            "StopFailure error={error} ({} of them) -> {class}",
            stops.len()
        ));
        class
    });

    // 4. The notification family.
    let quota_notice = evidence.iter().any(
        |e| matches!(e, Evidence::Notification { kind } if kind.starts_with("quota_auto_resume")),
    );
    let notification_class = quota_notice.then(|| {
        let class = match exhausted {
            Some(c @ (Class::Session5hLimit | Class::Weekly7dLimit)) => c,
            _ => Class::Session5hLimit,
        };
        reasons.push(format!("Notification quota_auto_resume_* -> {class}"));
        class
    });

    // 5. The banner.
    let banner = evidence.iter().find_map(|e| match e {
        Evidence::Banner { text, resets_at } => Some((text.as_str(), *resets_at)),
        _ => None,
    });
    let banner_cls = banner.map(|(text, _)| {
        let class = banner_class(text).unwrap_or(Class::Unknown);
        // "rank 1" used to end this sentence, and it read two ways at once:
        // design §5.8.1's FIRST ROW (which is what is meant — the grid is the
        // top of the evidence order) and `Source::rank`'s 1-of-4, which was
        // the WEAKEST figure authority. The ordinal is spelled out now and
        // the accessor is called `figure_authority`.
        reasons.push(format!(
            "banner `{}` -> {class} (grid, §5.8.1's first row)",
            one_line(text)
        ));
        class
    });

    let has_switch = evidence
        .iter()
        .any(|e| matches!(e, Evidence::PostModelSwitch { .. }));
    if has_switch {
        reasons.push("PostModelSwitch present".to_string());
    }

    let class = hook_class
        .or(exhausted)
        .or(notification_class)
        .or(banner_cls)?;

    // 6. The pair rule, over CHANNELS (§5.8.2, narrowed 2026-09-19).
    //
    // A window supports the class per-window, so that the channel it lands
    // on can be told apart: a figure the vendor computed is
    // `Source::StatusLine`, one read back off the frame it painted is
    // `Source::Grid` and collapses into the banner's channel.
    let window_supports = |w: &Readable| -> bool {
        match class {
            Class::TransientCapacity => exhausted.is_none(),
            // `ModelBucketLimit` joined this arm on 2026-09-22, when the
            // Fable window stopped being unreadable: before the `/usage`
            // panel reader there was no window whose `exhausted_class` was
            // this one, so the arm would have been dead code. Now the
            // painted `seven_day_overage_included` row at 100 % corroborates
            // the class exactly as a `five_hour` row corroborates its own.
            Class::Session5hLimit
            | Class::Weekly7dLimit
            | Class::SpendBilling
            | Class::ModelBucketLimit => {
                w.pct >= EXHAUSTED_PCT && w.which.exhausted_class() == class
            }
            _ => false,
        }
    };
    let supporting_windows: Vec<&Readable> =
        readable.iter().filter(|w| window_supports(w)).collect();
    // The channel a supporting window lands on is the SOURCE's own answer
    // (`Source::channel`), not a second table here: a figure read back off
    // the frame the vendor painted collapses into the banner's channel, and
    // a figure the vendor computed is one channel whether it arrived on the
    // statusLine or through its on-disk cache.
    let mut channels: Vec<Source> = Vec::new();
    let on_channel = |c: Source| supporting_windows.iter().any(|w| w.source.channel() == c);
    if banner_cls == Some(class) || on_channel(Source::Grid) {
        channels.push(Source::Grid);
    }
    if hook_class == Some(class) {
        channels.push(Source::StopFailure);
    }
    if quota_notice && matches!(class, Class::Session5hLimit | Class::Weekly7dLimit) {
        channels.push(Source::Notification);
    }
    if has_switch
        && matches!(
            class,
            Class::TransientCapacity
                | Class::Session5hLimit
                | Class::Weekly7dLimit
                | Class::ModelBucketLimit
        )
    {
        channels.push(Source::PostModelSwitch);
    }
    if on_channel(Source::StatusLine) {
        channels.push(Source::StatusLine);
    }
    let pair = channels.len() >= 2 && channels.iter().any(|c| c.names_the_failure());
    let unpaired = !pair || class == Class::Unknown;
    let named = channels
        .iter()
        .map(|c| c.as_str())
        .collect::<Vec<_>>()
        .join(" + ");
    if pair {
        reasons.push(format!("independent pair: {named}"));
    } else {
        // NOT "display only": every reversible action runs on this. Only
        // the four that spend are withheld (`Action::needs_two_sources`).
        reasons.push(format!(
            "one source ({named}): switch-account, lower-priority, limit-reset and extra-usage are refused"
        ));
    }
    if class == Class::Unknown {
        reasons.push("unknown: escalate only".to_string());
    }

    // 7. The reset: the exhausted window's, statusLine over cache; else the
    // banner's.
    let mut windows_for_class: Vec<&Readable> = readable
        .iter()
        .filter(|w| w.which.exhausted_class() == class && w.resets_at.is_some())
        .collect();
    windows_for_class.sort_by_key(|w| match w.source {
        Source::StatusLine => 0,
        _ => 1,
    });
    let resets_at = windows_for_class
        .first()
        .and_then(|w| w.resets_at)
        .or_else(|| banner.and_then(|(_, r)| r));

    let overloaded = stops
        .iter()
        .filter(|(error, _)| *error == "overloaded")
        .count();
    let storm = overloaded >= STORM_COUNT as usize
        || banner.is_some_and(|(text, _)| contains_ci(text, STORM_MARKER));
    let no_response = banner.is_some_and(|(text, _)| contains_ci(text, NO_RESPONSE_MARKER))
        || stops
            .iter()
            .any(|(_, d)| d.is_some_and(|d| contains_ci(d, NO_RESPONSE_MARKER)));

    Some(Classification {
        class,
        unpaired,
        reasons,
        resets_at,
        storm,
        no_response,
    })
}

/// A banner quoted into one bounded reason string: the harness's shared
/// one-line fold, with an ellipsis where it had to cut.
///
/// The cut is taken AFTER the fold, not before it as [`super::one_line`]'s
/// own `max` would: a banner is quoted screen text, the fold collapses the
/// padding a screen carries, and cutting first would spend the 80 bytes on
/// spaces. That is the whole difference, and it is why the `...` lives here
/// rather than in the shared helper.
fn one_line(text: &str) -> String {
    const MAX: usize = 80;
    let s = super::one_line(text, usize::MAX);
    if s.len() <= MAX {
        return s;
    }
    format!("{}...", super::truncate_bytes(&s, MAX - 3))
}

// ---------------------------------------------------------------------------
// Actions and the table
// ---------------------------------------------------------------------------

/// The closed action vocabulary of design §5.8.4 (plus `relogin`, §5.8.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    /// Do nothing but display: the vendor is retrying.
    LetVendorRetry,
    /// Re-submit `retry_text` via a bounded `turn` (L3).
    Retry,
    /// Own the wait keyed on `resets_at` (arming is L1).
    Wait,
    /// §5.3's model switch (L3 as typed; §5.3 mode B is the host's to gate).
    SwitchModel,
    /// §5.6's account relaunch (L4).
    SwitchAccount,
    /// `turn … /low-priority` (L3, opt-in).
    LowerPriority,
    /// `turn … /limit-reset` then stop (L3, opt-in; the keypress is human).
    LimitReset,
    /// Never typed in this ABI.
    ExtraUsage,
    /// `turn … /login` then wait for the human (L3, `auth` only).
    Relogin,
    /// Attention plus a fabric `ask` (L1).
    Escalate,
}

impl Action {
    /// Every action, in vocabulary order.
    pub const ALL: [Action; 10] = [
        Action::LetVendorRetry,
        Action::Retry,
        Action::Wait,
        Action::SwitchModel,
        Action::SwitchAccount,
        Action::LowerPriority,
        Action::LimitReset,
        Action::ExtraUsage,
        Action::Relogin,
        Action::Escalate,
    ];

    /// The config-file spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Action::LetVendorRetry => "let-vendor-retry",
            Action::Retry => "retry",
            Action::Wait => "wait",
            Action::SwitchModel => "switch-model",
            Action::SwitchAccount => "switch-account",
            Action::LowerPriority => "lower-priority",
            Action::LimitReset => "limit-reset",
            Action::ExtraUsage => "extra-usage",
            Action::Relogin => "relogin",
            Action::Escalate => "escalate",
        }
    }

    /// The action a config-file spelling names, if any.
    pub fn parse(name: &str) -> Option<Action> {
        Action::ALL.into_iter().find(|a| a.as_str() == name)
    }

    /// The actuator level of design §5.8.6 (0–4). `extra-usage` is given L5,
    /// the level nothing may use.
    pub fn level(self) -> u8 {
        match self {
            Action::LetVendorRetry => 0,
            Action::Wait | Action::Escalate => 1,
            Action::Retry
            | Action::SwitchModel
            | Action::LowerPriority
            | Action::LimitReset
            | Action::Relogin => 3,
            Action::SwitchAccount => 4,
            Action::ExtraUsage => 5,
        }
    }

    /// `switch-model` or `switch-account`: the two that share the budget and
    /// the dwell.
    pub fn is_switch(self) -> bool {
        matches!(self, Action::SwitchModel | Action::SwitchAccount)
    }

    /// The four actions design §5.8.2's two-source rule binds, narrowed
    /// 2026-09-19: the ones that **spend money, spend an allowance, or move
    /// an account**, where a wrong classification costs something no later
    /// evidence can give back — `switch-account`, `lower-priority` (spends
    /// the weekly allowance), `limit-reset` (burns the once-a-week reset)
    /// and `extra-usage` (spends money).
    ///
    /// Every other action is reversible and runs on rank-1 evidence alone,
    /// at the confidence that evidence carries: `let-vendor-retry`, `retry`,
    /// `wait`, `switch-model`, `relogin` and `escalate`, plus every L1
    /// display. Requiring vendor corroboration for THOSE is what left a
    /// zero-hook session unable to act at all.
    pub fn needs_two_sources(self) -> bool {
        matches!(
            self,
            Action::SwitchAccount | Action::LowerPriority | Action::LimitReset | Action::ExtraUsage
        )
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a table is refused. Every variant names the class and, where one
/// exists, the offending member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableError {
    /// A name outside the vocabulary; the whole table is refused.
    UnknownAction { class: Class, name: String },
    /// A `switch-*` in a class whose §11 invariant forbids it.
    SwitchNotAllowed { class: Class, action: Action },
    /// `retry` for `unknown`.
    RetryNotAllowed { class: Class },
    /// A member outside a pinned class's set.
    Pinned { class: Class, action: Action },
    /// `relogin` anywhere but `auth`.
    ReloginOutsideAuth { class: Class },
    /// A table without `escalate`: nothing would ever tell a human.
    NoEscalate { class: Class },
    /// A repeated member.
    Duplicate { class: Class, action: Action },
}

impl fmt::Display for TableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TableError::UnknownAction { class, name } => {
                write!(f, "{class}: `{name}` is not an action")
            }
            TableError::SwitchNotAllowed { class, action } => {
                write!(
                    f,
                    "{class}: `{action}` is not allowed (no switch for this class)"
                )
            }
            TableError::RetryNotAllowed { class } => {
                write!(
                    f,
                    "{class}: `retry` is not allowed (a deterministic error would loop)"
                )
            }
            TableError::Pinned { class, action } => {
                write!(f, "{class}: pinned, `{action}` is not a member")
            }
            TableError::ReloginOutsideAuth { class } => {
                write!(f, "{class}: `relogin` belongs to `auth` only")
            }
            TableError::NoEscalate { class } => write!(f, "{class}: no `escalate`"),
            TableError::Duplicate { class, action } => {
                write!(f, "{class}: `{action}` listed twice")
            }
        }
    }
}

impl std::error::Error for TableError {}

/// The ordered candidates per class (`[cap.limits.actions]`, design §5.8.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionTable {
    rows: [Vec<Action>; 8],
}

impl Default for ActionTable {
    /// The table of design §5.8.7, verbatim.
    fn default() -> Self {
        ActionTable {
            rows: [
                vec![
                    Action::LetVendorRetry,
                    Action::SwitchModel,
                    Action::Wait,
                    Action::Retry,
                    Action::Escalate,
                ],
                vec![
                    Action::LetVendorRetry,
                    Action::Wait,
                    Action::Retry,
                    Action::Escalate,
                ],
                vec![
                    Action::SwitchAccount,
                    Action::SwitchModel,
                    Action::Wait,
                    Action::Retry,
                    Action::Escalate,
                ],
                vec![
                    Action::SwitchAccount,
                    Action::SwitchModel,
                    Action::Wait,
                    Action::Escalate,
                ],
                vec![Action::LetVendorRetry, Action::Escalate],
                vec![Action::Escalate],
                vec![Action::Relogin, Action::Escalate],
                vec![Action::Escalate],
            ],
        }
    }
}

impl ActionTable {
    /// The candidates for one class, in order.
    pub fn get(&self, class: Class) -> &[Action] {
        &self.rows[class.index()]
    }

    /// Replace one class's row, refusing it (and leaving the table unchanged)
    /// when the row breaks the class's allowed set. Names are checked first
    /// so an unknown name refuses the whole row.
    pub fn set_names(&mut self, class: Class, names: &[&str]) -> Result<(), TableError> {
        let mut row = Vec::with_capacity(names.len());
        for name in names {
            match Action::parse(name) {
                Some(a) => row.push(a),
                None => {
                    return Err(TableError::UnknownAction {
                        class,
                        name: (*name).to_string(),
                    });
                }
            }
        }
        self.set(class, row)
    }

    /// Replace one class's row, refusing it when it breaks the allowed set.
    pub fn set(&mut self, class: Class, row: Vec<Action>) -> Result<(), TableError> {
        validate_row(class, &row)?;
        self.rows[class.index()] = row;
        Ok(())
    }

    /// Every row against its class's allowed set (design §5.8.7, §11 item 7).
    pub fn validate(&self) -> Result<(), TableError> {
        for class in Class::ALL {
            validate_row(class, self.get(class))?;
        }
        Ok(())
    }
}

/// One row against one class. Rules: no repeats; `escalate` present; no
/// `switch-*` for `network-offline` / `unknown` / `model-bucket-limit`; no
/// `retry` for `unknown`; `spend-billing` is exactly `["escalate"]`; `auth`
/// is inside `["relogin", "escalate"]`; `relogin` nowhere else.
fn validate_row(class: Class, row: &[Action]) -> Result<(), TableError> {
    for (i, action) in row.iter().enumerate() {
        if row[..i].contains(action) {
            return Err(TableError::Duplicate {
                class,
                action: *action,
            });
        }
    }
    if !row.contains(&Action::Escalate) {
        return Err(TableError::NoEscalate { class });
    }
    for action in row {
        match class {
            Class::SpendBilling if *action != Action::Escalate => {
                return Err(TableError::Pinned {
                    class,
                    action: *action,
                });
            }
            Class::Auth if !matches!(action, Action::Relogin | Action::Escalate) => {
                return Err(TableError::Pinned {
                    class,
                    action: *action,
                });
            }
            _ => {}
        }
        if action.is_switch() && !class.switch_allowed() {
            return Err(TableError::SwitchNotAllowed {
                class,
                action: *action,
            });
        }
        if *action == Action::Retry && class == Class::Unknown {
            return Err(TableError::RetryNotAllowed { class });
        }
        if *action == Action::Relogin && class != Class::Auth {
            return Err(TableError::ReloginOutsideAuth { class });
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// `n` events per rolling window (`"4/6h"`, `"3/10m"`, `"3/24h"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub count: u32,
    pub window_s: u64,
}

impl Budget {
    /// `"<count>/<n><unit>"` with unit `s`, `m`, `h` or `d`. `None` for
    /// anything else, including a zero count or window.
    pub fn parse(text: &str) -> Option<Budget> {
        let (count, window) = text.trim().split_once('/')?;
        let count: u32 = count.trim().parse().ok()?;
        let window = window.trim();
        let (digits, unit) = window.split_at(window.len().checked_sub(1)?);
        let n: u64 = digits.parse().ok()?;
        let mult = match unit {
            "s" => 1,
            "m" => 60,
            "h" => 3600,
            "d" => 86_400,
            _ => return None,
        };
        let window_s = n.checked_mul(mult)?;
        (count > 0 && window_s > 0).then_some(Budget { count, window_s })
    }

    /// The config spelling, normalised to the largest exact unit **up to
    /// hours**. `d` parses but is never written, so the two day-long budgets
    /// the design spells — `relogin_budget = "3/24h"` (§5.8.10) and any
    /// `"n/24h"` an owner writes — round-trip through a `config get`/`set`
    /// pair unchanged instead of coming back re-spelled as `"n/1d"`.
    pub fn as_string(self) -> String {
        let (n, unit) = if self.window_s.is_multiple_of(3600) {
            (self.window_s / 3600, "h")
        } else if self.window_s.is_multiple_of(60) {
            (self.window_s / 60, "m")
        } else {
            (self.window_s, "s")
        };
        format!("{}/{}{}", self.count, n, unit)
    }
}

/// What `retry` types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RetryText {
    /// The fixed word `continue`.
    #[default]
    Continue,
    /// The ledger's last `UserPromptSubmit` row under the §5.8.6 rules
    /// (opt-in); the replay itself is the host's.
    LastPrompt,
}

impl RetryText {
    /// The config-file spelling (design §5.8.7's `retry_text`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            RetryText::Continue => "continue",
            RetryText::LastPrompt => "last-prompt",
        }
    }

    /// Read one `retry_text =` value. `None` for anything outside the two
    /// the design names — a third spelling is a refusal, never a silent fall
    /// back to `continue`.
    #[must_use]
    pub fn parse(token: &str) -> Option<RetryText> {
        match token {
            "continue" => Some(RetryText::Continue),
            "last-prompt" => Some(RetryText::LastPrompt),
            _ => None,
        }
    }
}

/// `[cap.limits]` (design §5.8.7 and §5.8.10); `Default` is the shipped
/// table.
#[derive(Debug, Clone, PartialEq)]
pub struct LimitsConfig {
    pub enabled: bool,
    /// The maximum level this capability may use; 4 is needed for
    /// `switch-account`.
    pub level: u8,
    /// Automatic switches, model and account together.
    pub budget: Budget,
    /// Turn-boundary wait before a switch (the host's bound; not read here).
    pub settle_s: u64,
    pub transient_t1_s: u64,
    pub transient_t2_s: u64,
    pub network_t1_s: u64,
    pub network_t2_s: u64,
    pub min_dwell_s: u64,
    pub switch_back: bool,
    /// **TARGET — no decision reads this.** §5.8.5's hysteresis: the
    /// primary's window must be below it before the harness switches back.
    /// Nothing switches back today (`watch::Watcher::switch_back_due` has no
    /// production caller), so nothing consults it, and it was REMOVED from
    /// the `config.toml` key registry on 2026-09-22 — a knob that is written,
    /// validated and canonically re-emitted while changing no behaviour is
    /// exactly what a closed registry exists to prevent. It returns to `KEYS`
    /// in the commit that makes switch-back act.
    pub switch_back_headroom_pct: u8,
    /// Beyond this, escalate instead of waiting (the vendor's own cut-off).
    pub max_wait_h: u64,
    pub unknown_escalate_after: Budget,
    pub retry_text: RetryText,
    pub allow_low_priority: bool,
    pub allow_limit_reset: bool,
    /// Reserved: `extra-usage` is unreachable in ABI 1 regardless.
    pub allow_spend: bool,
    /// The harness owns the wait/retry (writes `autoContinueAtUsageLimit=false`
    /// at spawn — the host's act); `false` = the vendor owns it.
    pub own_resume: bool,
    pub relogin: bool,
    pub relogin_wait_s: u64,
    pub relogin_budget: Budget,
    /// **TARGET — no decision reads this.** §5.8.10 step 6's relaunch into
    /// the SAME config dir. It is off the `KEYS` registry for the reason
    /// [`LimitsConfig::switch_back_headroom_pct`] gives, and doubly so: the
    /// relaunch it would gate has no admitted control line at all
    /// (`watch::Act::Relaunch`).
    pub relogin_fallback_relaunch: bool,
    pub actions: ActionTable,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        LimitsConfig {
            enabled: true,
            level: 3,
            budget: Budget {
                count: 4,
                window_s: 6 * 3600,
            },
            settle_s: 30,
            transient_t1_s: 300,
            transient_t2_s: 900,
            network_t1_s: 300,
            network_t2_s: 1800,
            min_dwell_s: 600,
            switch_back: true,
            switch_back_headroom_pct: 80,
            max_wait_h: 24,
            unknown_escalate_after: Budget {
                count: 3,
                window_s: 600,
            },
            retry_text: RetryText::Continue,
            allow_low_priority: false,
            allow_limit_reset: false,
            allow_spend: false,
            own_resume: true,
            relogin: true,
            relogin_wait_s: 900,
            relogin_budget: Budget {
                count: 3,
                window_s: 24 * 3600,
            },
            relogin_fallback_relaunch: false,
            actions: ActionTable::default(),
        }
    }
}

impl LimitsConfig {
    /// The table's allowed sets, plus the timers' order (`t1 <= t2`) and a
    /// level inside 0–4.
    pub fn validate(&self) -> Result<(), ConfigError> {
        self.actions.validate().map_err(ConfigError::Table)?;
        if self.level > 4 {
            return Err(ConfigError::Level(self.level));
        }
        if self.transient_t1_s > self.transient_t2_s {
            return Err(ConfigError::Timers("transient"));
        }
        if self.network_t1_s > self.network_t2_s {
            return Err(ConfigError::Timers("network"));
        }
        Ok(())
    }
}

/// Why a config is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    Table(TableError),
    /// A level above 4 (L5 is never reachable).
    Level(u8),
    /// `t1 > t2` for the named class.
    Timers(&'static str),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Table(e) => write!(f, "actions: {e}"),
            ConfigError::Level(l) => write!(f, "level {l} is above 4"),
            ConfigError::Timers(class) => write!(f, "{class}: t1 > t2"),
        }
    }
}

impl std::error::Error for ConfigError {}

// ---------------------------------------------------------------------------
// The engine
// ---------------------------------------------------------------------------

/// Seconds a human keystroke pauses the timers (design §5.8.4, as §5.4).
pub const HUMAN_PAUSE_S: i64 = 600;
/// The jitter added to `resets_at` before a `retry` (design §5.8.4, fixed:
/// there is no randomness in a pure function).
pub const RESET_JITTER_S: i64 = 60;
/// How long `transient-capacity`'s `wait` waits (design §5.8.4 T2 row).
pub const TRANSIENT_WAIT_S: i64 = 600;

/// The rolling ledger behind a [`Budget`]: the times the budgeted thing
/// happened. Carried across generations by the host.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Ledger {
    spent: Vec<i64>,
}

impl Ledger {
    /// Events inside the window as of `now`. The window is CLOSED at its far
    /// edge: an event exactly `window_s` old still counts, so a budget is
    /// released one second late rather than one second early — the tie broken
    /// toward refusing.
    pub fn used(&self, budget: Budget, now: i64) -> u32 {
        let floor = now.saturating_sub(i64::try_from(budget.window_s).unwrap_or(i64::MAX));
        u32::try_from(self.spent.iter().filter(|t| **t >= floor).count()).unwrap_or(u32::MAX)
    }

    /// Events left inside the window as of `now`.
    pub fn left(&self, budget: Budget, now: i64) -> u32 {
        budget.count.saturating_sub(self.used(budget, now))
    }

    /// Record one event; entries older than a day past the largest window
    /// this module uses are dropped so the vector stays bounded.
    pub fn spend(&mut self, now: i64) {
        self.spent.push(now);
        self.spent.retain(|t| now - *t <= 2 * 86_400);
    }

    /// The times recorded, oldest first.
    pub fn entries(&self) -> &[i64] {
        &self.spent
    }
}

/// The one automatic action awaiting its verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InFlight {
    pub action: Action,
    /// The table index the action came from.
    pub step: usize,
    /// The class whose row that index is an index INTO. A verdict advances
    /// the cursor only while the state is still on this class; a class that
    /// changed while the action was out reindexes a different row, and the
    /// new row is walked from its beginning instead.
    pub class: Class,
    /// The ledger id of the journal row written before it.
    pub id: u64,
    pub started: i64,
}

/// A terminal-for-generation state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terminal {
    /// `escalate` ran; a human owns it now.
    Escalated,
    /// The class cleared (a turn succeeded, the vendor resumed, the human
    /// completed the vendor's flow).
    Settled,
    /// The generation changed under the state; everything pending was
    /// dropped.
    Generation,
}

/// What the host carries from one generation's state into the next.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Carry {
    pub last_switch_at: Option<i64>,
    /// The shared `switch-model` + `switch-account` ledger.
    pub switches: Ledger,
    /// The per-account `relogin` ledger.
    pub relogins: Ledger,
}

/// The engine's state for one `(sid, generation)`.
#[derive(Debug, Clone, PartialEq)]
pub struct State {
    pub class: Class,
    /// The first classified event of this generation (wall clock, injected).
    pub since: i64,
    /// The next candidate's index into the class's row.
    pub step: usize,
    pub in_flight: Option<InFlight>,
    pub last_switch_at: Option<i64>,
    /// The shared switch ledger against [`LimitsConfig::budget`].
    pub budget: Ledger,
    pub generation: u64,
    /// One independent source named this class: the four spending actions
    /// are withheld and every reversible one is not
    /// ([`Classification::unpaired`]).
    pub unpaired: bool,
    pub resets_at: Option<i64>,
    pub storm: bool,
    pub no_response: bool,
    /// Set when `wait` armed; `retry` and the switch-back key off it.
    pub wait_until: Option<i64>,
    /// The vendor armed or fired its own resume: the pending `retry` is
    /// cancelled for this generation.
    pub retry_cancelled: bool,
    /// When `relogin` was typed (at most once per generation).
    pub relogin_at: Option<i64>,
    pub relogins: Ledger,
    /// The times `unknown` (or, for a storm count, `transient-capacity`) was
    /// classified in this generation, RETAINED: entries older than
    /// [`HITS_RETAIN_S`] are dropped on every push, exactly as
    /// [`Ledger::spend`] does twelve lines above. It used to be pushed to and
    /// never trimmed except on a class CHANGE, so a session parked on one
    /// class accumulated one `i64` per failure for its whole life, and both
    /// readers walk it linearly on every event.
    pub hits: Vec<i64>,
    /// The FIRST hit of the current class in this generation, kept apart from
    /// [`Self::hits`] so retention can never move it.
    ///
    /// `unknown`'s escalation deadline is anchored here. Anchoring it on
    /// `hits.iter().min()` — the surviving hits — pushed the deadline forward
    /// by a whole window every time one aged out, so a lone `unknown` waited
    /// for ever and no human was ever told; with retention added, that reader
    /// would have re-broken on its own.
    pub hits_anchor: Option<i64>,
    /// A human keystroke pauses the timers until this time.
    pub paused_until: Option<i64>,
    pub terminal: Option<Terminal>,
}

impl State {
    /// A fresh state for a classification made at `now` in `generation`,
    /// with what the host carried from before.
    pub fn new(c: &Classification, now: i64, generation: u64, carry: Carry) -> State {
        State {
            class: c.class,
            since: now,
            step: 0,
            in_flight: None,
            last_switch_at: carry.last_switch_at,
            budget: carry.switches,
            generation,
            unpaired: c.unpaired,
            resets_at: c.resets_at,
            storm: c.storm,
            no_response: c.no_response,
            wait_until: None,
            retry_cancelled: false,
            relogin_at: None,
            relogins: carry.relogins,
            hits: vec![now],
            hits_anchor: Some(now),
            paused_until: None,
            terminal: None,
        }
    }

    /// What to carry into the next generation.
    pub fn carry(&self) -> Carry {
        Carry {
            last_switch_at: self.last_switch_at,
            switches: self.budget.clone(),
            relogins: self.relogins.clone(),
        }
    }

    /// Move the state onto `class` and put the table cursor back at that
    /// row's beginning.
    ///
    /// The cursor is an index into ONE class's row, so it is meaningless the
    /// moment the class changes: `unknown`'s row is one entry long, and a
    /// cursor of 1 inherited from a five-hour ladder walked it past its end
    /// and answered `refused:exhausted` for ever — the fail-closed rename
    /// ending in silence instead of in the `escalate` its row carries.
    /// `since` moves too, so the new class's T1/T2 timers count from when
    /// this class was first seen rather than from the old one's first event.
    /// `in_flight` is deliberately kept: the action is already out, its
    /// verdict still owes the budget its spend, and [`Refusal::InFlight`]
    /// holds until it lands.
    fn adopt(&mut self, class: Class, now: i64) {
        if self.class == class {
            return;
        }
        self.class = class;
        self.step = 0;
        self.since = now;
        self.wait_until = None;
        self.hits.clear();
        self.hits_anchor = None;
    }

    /// Fold a later event into the state. A re-classification of the same
    /// generation is folded through [`Event::Reclassified`].
    pub fn observe(&mut self, event: &Event, now: i64) {
        match event {
            Event::Evidence(Evidence::StopFailure { error, details }) => {
                if error == "overloaded" {
                    self.hits_overloaded(now);
                }
                if !STOP_FAILURE_ERRORS.contains(&error.as_str()) {
                    self.adopt(Class::Unknown, now);
                    self.unpaired = true;
                }
                if self.class == Class::Unknown {
                    self.hit(now);
                }
                if details
                    .as_deref()
                    .is_some_and(|d| contains_ci(d, NO_RESPONSE_MARKER))
                {
                    self.no_response = true;
                }
            }
            Event::Evidence(Evidence::Notification { kind }) => match kind.as_str() {
                "quota_auto_resume_armed" | "quota_auto_resume_offer_armed" => {
                    self.retry_cancelled = true;
                }
                "quota_auto_resume_fired" => {
                    self.retry_cancelled = true;
                    self.terminal.get_or_insert(Terminal::Settled);
                }
                _ => {}
            },
            Event::Evidence(Evidence::Banner { text, resets_at }) => {
                if contains_ci(text, STORM_MARKER) {
                    self.storm = true;
                }
                if contains_ci(text, NO_RESPONSE_MARKER) {
                    self.no_response = true;
                }
                if self.resets_at.is_none() {
                    self.resets_at = *resets_at;
                }
            }
            Event::Evidence(Evidence::Window { .. } | Evidence::PostModelSwitch { .. }) => {}
            Event::Reclassified(c) => {
                if c.class == self.class {
                    self.unpaired = self.unpaired && c.unpaired;
                    self.storm |= c.storm;
                    self.no_response |= c.no_response;
                    if c.resets_at.is_some() {
                        self.resets_at = c.resets_at;
                    }
                } else {
                    // The generation's latest evidence names a DIFFERENT
                    // class. The state adopts it, exactly as a fresh state
                    // built from this classification would read it, so the
                    // decision that stands is the new row's. Dropping the
                    // reclassification instead left the old row deciding: a
                    // five-hour state told `authentication_failed` went on
                    // to relaunch an account. Nothing is actuated here — the
                    // new row is walked from its beginning, and an `auth` or
                    // `spend-billing` row can only escalate or re-login.
                    self.adopt(c.class, now);
                    self.unpaired = c.unpaired;
                    self.storm = c.storm;
                    self.no_response = c.no_response;
                    self.resets_at = c.resets_at;
                }
                if self.class == Class::Unknown {
                    self.hit(now);
                }
            }
            Event::HumanKeystroke => {
                self.paused_until = Some(now.saturating_add(HUMAN_PAUSE_S));
            }
            Event::GenerationChanged(g) => {
                self.in_flight = None;
                self.generation = *g;
                self.terminal = Some(Terminal::Generation);
            }
            Event::Cleared => {
                self.terminal.get_or_insert(Terminal::Settled);
            }
        }
    }

    /// Push one hit, keeping the anchor and bounding the vector.
    fn hit(&mut self, now: i64) {
        self.hits.push(now);
        self.hits
            .retain(|t| now.saturating_sub(*t) <= HITS_RETAIN_S);
        self.hits_anchor = Some(self.hits_anchor.map_or(now, |a| a.min(now)));
    }

    fn hits_overloaded(&mut self, now: i64) {
        // The storm counter rides `hits` only for `unknown`; for transient the
        // count is kept in `storm` once reached, so a third overloaded
        // failure after classification also makes a storm.
        if self.class == Class::TransientCapacity {
            self.hit(now);
            let recent = self.hits.iter().filter(|t| now - **t <= 600).count();
            if recent >= STORM_COUNT as usize {
                self.storm = true;
            }
        }
    }

    /// Record what [`step`] decided, with the ledger id of its journal row.
    /// An L3/L4 action becomes the one in flight; an L0/L1 action completes
    /// at once and advances the table; `escalate` is terminal.
    pub fn commit(&mut self, decided: &Step, id: u64, now: i64) {
        match &decided.decision {
            Decision::Act { action, level, .. } => {
                if *level >= 3 {
                    self.in_flight = Some(InFlight {
                        action: *action,
                        step: decided.step,
                        class: self.class,
                        id,
                        started: now,
                    });
                } else {
                    if *action == Action::Wait {
                        self.wait_until = decided.wait_until;
                    }
                    if *action == Action::Escalate {
                        self.terminal.get_or_insert(Terminal::Escalated);
                    }
                    self.step = decided.step + 1;
                }
            }
            Decision::Refused { .. } | Decision::Wait { .. } => {}
        }
    }

    /// The in-flight action's verdict. Executed switches start the dwell and
    /// spend the budget; an executed `relogin` starts its wait and spends its
    /// own budget; every verdict advances past the action.
    ///
    /// The advance is the CLASS's advance: if the class changed while the
    /// action was out, `f.step` indexes a row the state has left and the new
    /// row keeps the beginning [`State::adopt`] gave it. The budget is spent
    /// either way — the switch really did run.
    pub fn verdict(&mut self, verdict: Verdict, now: i64) {
        let Some(f) = self.in_flight.take() else {
            return;
        };
        if matches!(verdict, Verdict::Executed | Verdict::Settled) {
            if f.action.is_switch() {
                self.last_switch_at = Some(now);
                self.budget.spend(now);
            }
            if f.action == Action::Relogin {
                self.relogin_at = Some(now);
                self.relogins.spend(now);
            }
        }
        if f.class == self.class {
            self.step = f.step + 1;
        }
    }
}

/// A later event folded into a [`State`].
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Evidence(Evidence),
    /// The same generation classified again (more evidence arrived).
    Reclassified(Classification),
    /// `await momentum` saw a human key.
    HumanKeystroke,
    GenerationChanged(u64),
    /// A turn succeeded again, or the vendor's own flow completed.
    Cleared,
}

/// The verdict row's outcome for the action in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Executed,
    Settled,
    Refused,
    Timeout,
}

/// The host's facts the engine gates on. Every one is server-side (§4.3);
/// none is derived from the caller's claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Guards {
    /// `[accounts] enabled`.
    pub accounts_enabled: bool,
    /// The level in force (0–4).
    pub level: u8,
    pub allow_low_priority: bool,
    pub allow_limit_reset: bool,
    /// Read for the reason string only: `extra-usage` is unreachable
    /// regardless.
    pub allow_spend: bool,
    pub own_resume: bool,
    /// `hold=1`: every actuating verb answers `ERR halted`.
    pub hold: bool,
    /// A live turn lease: not a boundary.
    pub busy: bool,
    /// The caller's sid is the target's.
    pub caller_is_target: bool,
    /// `self=1` was given.
    pub self_allowed: bool,
}

impl Guards {
    /// The guards a config implies with nothing held, busy or self-targeted.
    pub fn from_config(cfg: &LimitsConfig, accounts_enabled: bool) -> Guards {
        Guards {
            accounts_enabled,
            level: cfg.level,
            allow_low_priority: cfg.allow_low_priority,
            allow_limit_reset: cfg.allow_limit_reset,
            allow_spend: cfg.allow_spend,
            own_resume: cfg.own_resume,
            hold: false,
            busy: false,
            caller_is_target: false,
            self_allowed: false,
        }
    }
}

/// Why a whole step was refused (the ledger spellings of §5.8.6, plus two
/// this engine adds: `refused:in-flight` and `refused:terminal`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The request's generation is not the state's.
    StaleGeneration,
    /// `[cap.limits] enabled = false`.
    Disabled,
    Hold,
    /// A live turn; the host re-awaits the boundary once, then times out.
    Busy,
    /// Caller sid == target sid without `self=1`.
    SelfTarget,
    /// One automatic action already awaits its verdict; queued, not acted.
    InFlight,
    /// The generation is terminal.
    Terminal(Terminal),
    /// Every candidate was skipped or done; nothing left but to observe.
    Exhausted,
}

impl Refusal {
    /// The ledger verdict spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Refusal::StaleGeneration => "refused:generation",
            Refusal::Disabled => "refused:disabled",
            Refusal::Hold => "refused:hold",
            Refusal::Busy => "refused:busy",
            Refusal::SelfTarget => "refused:self",
            Refusal::InFlight => "refused:in-flight",
            Refusal::Terminal(_) => "refused:terminal",
            Refusal::Exhausted => "refused:exhausted",
        }
    }
}

/// What the engine decided for this step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Run `action` at `level`. A `level` below the action's own is a
    /// degrade (`degraded:budget`): do the L1 half only.
    Act {
        action: Action,
        level: u8,
        reason: String,
    },
    Refused {
        why: Refusal,
    },
    /// Nothing until `until`.
    Wait {
        until: i64,
    },
}

/// A candidate the step walked past, journaled `refused:disabled`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub action: Action,
    pub step: usize,
    pub why: String,
}

/// One step's outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub decision: Decision,
    /// Candidates skipped on the way, in order.
    pub skipped: Vec<Skipped>,
    /// The table index the decision is about.
    pub step: usize,
    /// For an `Act { Wait }`: when the wait ends.
    pub wait_until: Option<i64>,
}

enum Gate {
    Ready,
    Wait(i64),
    Skip(String),
    Degrade(String),
}

fn secs(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// The class-specific timing of one candidate (design §5.8.4's T1/T2
/// columns), after the guards have passed. Returns the wait's end for
/// `wait` through the second slot.
fn timing(cfg: &LimitsConfig, state: &State, action: Action, now: i64) -> (Gate, Option<i64>) {
    let since = state.since;
    let at = |s: u64| since.saturating_add(secs(s));
    match state.class {
        Class::TransientCapacity => match action {
            Action::SwitchModel => {
                if now < at(cfg.transient_t1_s) {
                    (Gate::Wait(at(cfg.transient_t1_s)), None)
                } else if state.storm {
                    (Gate::Ready, None)
                } else if now < at(cfg.transient_t2_s) {
                    (Gate::Wait(at(cfg.transient_t2_s)), None)
                } else {
                    (Gate::Skip("no retry storm by T2".into()), None)
                }
            }
            Action::Wait => {
                if now < at(cfg.transient_t2_s) {
                    (Gate::Wait(at(cfg.transient_t2_s)), None)
                } else {
                    (Gate::Ready, Some(now.saturating_add(TRANSIENT_WAIT_S)))
                }
            }
            Action::Retry => retry_timing(state, now),
            Action::Escalate => {
                if now < at(cfg.transient_t2_s) {
                    (Gate::Wait(at(cfg.transient_t2_s)), None)
                } else {
                    (Gate::Ready, None)
                }
            }
            _ => (Gate::Ready, None),
        },
        Class::NetworkOffline => match action {
            Action::Wait => {
                if now < at(cfg.network_t1_s) {
                    (Gate::Wait(at(cfg.network_t1_s)), None)
                } else {
                    (Gate::Ready, Some(at(cfg.network_t2_s)))
                }
            }
            Action::Retry => {
                if !state.no_response {
                    (
                        Gate::Skip("retry only after `No response from the API after`".into()),
                        None,
                    )
                } else {
                    retry_timing(state, now)
                }
            }
            Action::Escalate => {
                if now < at(cfg.network_t2_s) {
                    (Gate::Wait(at(cfg.network_t2_s)), None)
                } else {
                    (Gate::Ready, None)
                }
            }
            _ => (Gate::Ready, None),
        },
        Class::Session5hLimit => {
            let max_wait = secs(cfg.max_wait_h).saturating_mul(3600);
            match action {
                Action::Wait => match state.resets_at {
                    None => (Gate::Skip("no resets_at to wait on".into()), None),
                    Some(r) if r.saturating_sub(now) > max_wait => (
                        Gate::Skip(format!("resets_at is beyond max_wait_h={}", cfg.max_wait_h)),
                        None,
                    ),
                    Some(r) => (Gate::Ready, Some(r.saturating_add(RESET_JITTER_S))),
                },
                Action::Retry => retry_timing(state, now),
                Action::Escalate => {
                    let dry = state.budget.left(cfg.budget, now) == 0;
                    match state.resets_at {
                        None => (Gate::Ready, None),
                        Some(r) if r.saturating_sub(now) > max_wait || dry => (Gate::Ready, None),
                        Some(_) => (
                            Gate::Skip("reset within max_wait_h and budget left: observing".into()),
                            None,
                        ),
                    }
                }
                _ => (Gate::Ready, None),
            }
        }
        Class::Weekly7dLimit => match action {
            Action::Wait => match state.resets_at {
                None => (Gate::Skip("no resets_at to wait on".into()), None),
                Some(r) => (Gate::Ready, Some(r.saturating_add(RESET_JITTER_S))),
            },
            Action::Retry => retry_timing(state, now),
            _ => (Gate::Ready, None),
        },
        Class::ModelBucketLimit | Class::SpendBilling => (Gate::Ready, None),
        Class::Auth => match action {
            Action::Escalate => match state.relogin_at {
                Some(t) if now < t.saturating_add(secs(cfg.relogin_wait_s)) => {
                    (Gate::Wait(t.saturating_add(secs(cfg.relogin_wait_s))), None)
                }
                _ => (Gate::Ready, None),
            },
            _ => (Gate::Ready, None),
        },
        Class::Unknown => match action {
            Action::Escalate => {
                let b = cfg.unknown_escalate_after;
                // The same closed far edge as `Ledger::used`.
                let floor = now.saturating_sub(secs(b.window_s));
                let recent = state.hits.iter().filter(|t| **t >= floor).count();
                if recent >= b.count as usize {
                    // The burst: `count` unplaceable failures inside one
                    // window (design §5.8.4's `unknown` row).
                    return (Gate::Ready, None);
                }
                // No burst. The deadline is anchored on the FIRST hit of
                // this class in this generation, never on the hits that
                // happen to survive the window: anchoring on the survivors
                // pushed the deadline forward by a whole window every time
                // one aged out, so a lone `unknown` waited for ever and no
                // human was ever told. Once that one window has closed the
                // failure nobody could place is escalated on its own.
                let anchor = state.hits_anchor.unwrap_or(state.since);
                let until = anchor.saturating_add(secs(b.window_s));
                if now >= until {
                    (Gate::Ready, None)
                } else {
                    (Gate::Wait(until), None)
                }
            }
            _ => (Gate::Ready, None),
        },
    }
}

fn retry_timing(state: &State, now: i64) -> (Gate, Option<i64>) {
    match state.wait_until {
        None => (Gate::Skip("no wait armed before retry".into()), None),
        Some(u) if now < u => (Gate::Wait(u), None),
        Some(_) => (Gate::Ready, None),
    }
}

/// The guards one candidate must pass before its timing is looked at.
fn guard(cfg: &LimitsConfig, state: &State, guards: &Guards, action: Action, now: i64) -> Gate {
    if action == Action::ExtraUsage {
        return Gate::Skip(format!(
            "extra-usage is unreachable in ABI 1 (allow_spend={})",
            guards.allow_spend
        ));
    }
    let level = action.level();
    if level > guards.level {
        return Gate::Skip(format!("level {level} > allowed {}", guards.level));
    }
    // The two-source rule, narrowed 2026-09-19 (§5.8.2, §0.2). It binds the
    // four actions that spend money, an allowance or an account — and
    // NOTHING else. It used to bind every action above L1, which is why a
    // session with no hooks could name a class from the grid and then never
    // act on it: the grid could not complete a pair, so `unpaired` stayed
    // true for ever and every L3 candidate was skipped.
    if state.unpaired && action.needs_two_sources() {
        return Gate::Skip(format!(
            "one source: `{action}` spends money, an allowance or an account and needs two (§5.8.2)"
        ));
    }
    match action {
        Action::SwitchAccount if !guards.accounts_enabled => {
            return Gate::Skip("[accounts] enabled = false".into());
        }
        Action::LowerPriority if !guards.allow_low_priority => {
            return Gate::Skip("allow_low_priority = false".into());
        }
        Action::LimitReset if !guards.allow_limit_reset => {
            return Gate::Skip("allow_limit_reset = false".into());
        }
        Action::Wait | Action::Retry if !guards.own_resume => {
            return Gate::Skip("own_resume = false: the vendor owns the wait".into());
        }
        Action::Retry if state.retry_cancelled => {
            return Gate::Skip("the vendor armed or fired its own resume: retry cancelled".into());
        }
        Action::Relogin if !cfg.relogin => {
            return Gate::Skip("relogin = false".into());
        }
        Action::Relogin if state.relogin_at.is_some() => {
            return Gate::Skip("relogin already typed this generation".into());
        }
        Action::Relogin if state.relogins.left(cfg.relogin_budget, now) == 0 => {
            return Gate::Skip(format!(
                "relogin budget {} is dry",
                cfg.relogin_budget.as_string()
            ));
        }
        _ => {}
    }
    if action.is_switch() {
        if let Some(last) = state.last_switch_at {
            let dwell_end = last.saturating_add(secs(cfg.min_dwell_s));
            if now < dwell_end {
                return Gate::Skip(format!(
                    "within min_dwell_s ({} s left): escalate, not switch",
                    dwell_end - now
                ));
            }
        }
        if state.budget.left(cfg.budget, now) == 0 {
            return Gate::Degrade(format!(
                "degraded:budget {} spent in {}",
                cfg.budget.count,
                cfg.budget.as_string()
            ));
        }
    }
    Gate::Ready
}

/// Decide the next thing for `state` as of `now`, for a request bound to
/// `generation`. Pure: nothing is recorded until [`State::commit`].
///
/// Order: generation, terminal, `enabled`, `hold`, one-in-flight, the human
/// pause; then the class's row from `state.step`, each candidate through its
/// guards (a failed guard skips it, journaled), its timing (a time gate
/// returns [`Decision::Wait`] without advancing), and for L3/L4 the boundary
/// (`busy`) and the self rule. An empty remainder is
/// [`Refusal::Exhausted`].
pub fn step(cfg: &LimitsConfig, state: &State, guards: &Guards, now: i64, generation: u64) -> Step {
    let refused = |why: Refusal| Step {
        decision: Decision::Refused { why },
        skipped: Vec::new(),
        step: state.step,
        wait_until: None,
    };
    if generation != state.generation {
        return refused(Refusal::StaleGeneration);
    }
    if let Some(t) = state.terminal {
        return refused(Refusal::Terminal(t));
    }
    if !cfg.enabled {
        return refused(Refusal::Disabled);
    }
    if guards.hold {
        return refused(Refusal::Hold);
    }
    if state.in_flight.is_some() {
        return refused(Refusal::InFlight);
    }
    if let Some(p) = state.paused_until
        && now < p
    {
        return Step {
            decision: Decision::Wait { until: p },
            skipped: Vec::new(),
            step: state.step,
            wait_until: None,
        };
    }

    let row = cfg.actions.get(state.class);
    let mut skipped = Vec::new();
    for (i, action) in row.iter().enumerate().skip(state.step) {
        let action = *action;
        let gate = match guard(cfg, state, guards, action, now) {
            Gate::Ready => timing(cfg, state, action, now),
            // A dry switch budget DEGRADES the candidate that is due; it
            // never makes an un-due one due. The class's timing gate is
            // asked first and a wait or a skip wins, so a dry budget can no
            // longer fire `switch-model` at T+2 s and journal
            // `degraded:budget` at a moment when no switch was a candidate
            // (design §5.8.4: the T1/T2 columns decide WHEN, the budget
            // decides WHETHER).
            Gate::Degrade(reason) => match timing(cfg, state, action, now) {
                (Gate::Ready, _) => (Gate::Degrade(reason), None),
                gated => gated,
            },
            other => (other, None),
        };
        match gate {
            (Gate::Skip(why), _) => {
                skipped.push(Skipped {
                    action,
                    step: i,
                    why,
                });
            }
            (Gate::Wait(until), _) => {
                return Step {
                    decision: Decision::Wait { until },
                    skipped,
                    step: i,
                    wait_until: None,
                };
            }
            (Gate::Degrade(reason), _) => {
                return Step {
                    decision: Decision::Act {
                        action,
                        level: 1,
                        reason,
                    },
                    skipped,
                    step: i,
                    wait_until: None,
                };
            }
            (Gate::Ready, wait_until) => {
                let level = action.level();
                if level >= 3 {
                    if guards.busy {
                        return Step {
                            decision: Decision::Refused { why: Refusal::Busy },
                            skipped,
                            step: i,
                            wait_until: None,
                        };
                    }
                    if guards.caller_is_target && !guards.self_allowed {
                        return Step {
                            decision: Decision::Refused {
                                why: Refusal::SelfTarget,
                            },
                            skipped,
                            step: i,
                            wait_until: None,
                        };
                    }
                }
                return Step {
                    decision: Decision::Act {
                        action,
                        level,
                        reason: format!("limits.actions.{}[{i}]", state.class),
                    },
                    skipped,
                    step: i,
                    wait_until,
                };
            }
        }
    }
    Step {
        decision: Decision::Refused {
            why: Refusal::Exhausted,
        },
        skipped,
        step: row.len(),
        wait_until: None,
    }
}

#[cfg(test)]
#[path = "limits_tests.rs"]
mod tests;
