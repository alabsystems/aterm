// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE STATUS SURFACE's engine: the one ambient lane for aterm's own
//! activity (docs/design/STATUS-SURFACE.md — owner-directed 2026-08-24).
//!
//! Producers report; this feed decides presentation. Nothing here is a
//! question, nothing floats, nothing takes focus: the renderers ask this
//! model for a [`FilamentState`] and a [`LaneLine`] each frame and paint them
//! inside chrome the window already owns. The model is pure over injected
//! clocks — every method takes `now: Instant`, exactly like the effects
//! engines, so tests drive it deterministically and a resumed clock cannot
//! fling an animation.
//!
//! One feed per APP (not per window): aterm's own machinery — the toolset
//! install, the updater, drive dials — is app-scoped, and every window's
//! band shows the same truth.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Which machinery an activity belongs to. The wire tokens (`as_str`) are the
/// `appstatus` verb's `kind=` values and are additive — clients must treat an
/// unknown token as unknown, never as an error (the `status` verb's law).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivityKind {
    /// The atpkg batteries-included toolset (first-launch install, repairs).
    Toolset,
    /// The self-updater (check / download / stage / activate — and deferrals).
    Update,
    /// An aterm-net network drive dialing or syncing.
    Drive,
    /// Release/roster machinery surfaced to the operator.
    Release,
    /// Self-healing and health counters worth a quiet mention.
    Health,
}

impl ActivityKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Toolset => "toolset",
            Self::Update => "update",
            Self::Drive => "drive",
            Self::Release => "release",
            Self::Health => "health",
        }
    }
}

/// How an activity ended. Failure carries the reason the lane shows and the
/// ledger keeps; it is one line of text, not a dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActivityOutcome {
    Success,
    Failure(String),
}

/// A live activity's determinate progress, when its producer knows one.
/// `den == 0` never happens through the API (`progress` ignores it) — a
/// zero-denominator fraction is how progress bars lie.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub num: u64,
    pub den: u64,
}

/// Opaque handle a producer holds; also the `id=` the `appstatus` verb
/// prints. Monotonic per feed, never reused, so a ledger row and a live row
/// can never alias.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ActivityId(u64);

impl ActivityId {
    #[must_use]
    pub fn raw(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug)]
struct Activity {
    id: ActivityId,
    kind: ActivityKind,
    /// The producer's standing title ("Installing ALab toolchain").
    title: String,
    /// The producer's latest one-line detail, when it has one; the lane shows
    /// this over the title.
    message: Option<String>,
    progress: Option<Progress>,
    began: Instant,
    /// Set at finish; a finished activity LINGERS in the live list for
    /// [`StatusFeed::LANE_LINGER`] so its completion is readable, then moves
    /// wholly to the ledger.
    finished: Option<(Instant, ActivityOutcome)>,
}

/// One completed activity, as the ledger keeps it.
#[derive(Clone, Debug)]
pub struct LedgerRow {
    pub id: ActivityId,
    pub kind: ActivityKind,
    pub title: String,
    pub outcome: ActivityOutcome,
    pub began: Instant,
    pub finished: Instant,
}

/// What the filament paints this frame. Renderers translate this to pixels;
/// Serious Mode substitutes its static pulse for `Indeterminate` at the
/// paint site (motion policy is a render concern, not a model one).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FilamentState {
    /// No live activity: paint nothing, reserve nothing.
    Hidden,
    /// Weighted fill in 0.0..=1.0.
    Determinate(f32),
    /// At least one live activity has no honest fraction: the comet sweep.
    /// `phase` is 0.0..1.0 across [`StatusFeed::SWEEP_PERIOD`].
    Indeterminate { phase: f32 },
    /// Every activity just succeeded: hold the full bar briefly, then fade.
    /// `q` runs 1.0 (fully shown) down to 0.0 (gone).
    CompletionFade { q: f32 },
    /// An activity just failed: one blink in the error tone, then the lane
    /// carries the message. `q` runs 1.0 down to 0.0 across the blink.
    FailureBlink { q: f32 },
}

/// What the message lane shows this frame: one line, its alpha, and the
/// activity it belongs to (for click routing and tooltips).
#[derive(Clone, Debug, PartialEq)]
pub struct LaneLine {
    pub id: ActivityId,
    pub kind: ActivityKind,
    /// Already middle-truncated to the renderer's stated budget.
    pub text: String,
    /// The full untruncated text, for the hover tooltip.
    pub full_text: String,
    /// 0.0..=1.0 fade envelope.
    pub alpha: f32,
}

/// The app-level feed. All methods are called on the GUI thread; producers on
/// other threads reach it through the host's existing event proxy.
#[derive(Debug, Default)]
pub struct StatusFeed {
    next_id: u64,
    live: Vec<Activity>,
    ledger: VecDeque<LedgerRow>,
    /// The sweep/rotation clock zero: first live activity's arrival. Cleared
    /// when the surface goes idle so a later activity restarts phases at 0.
    epoch: Option<Instant>,
    /// Completion-fade bookkeeping: set when the LAST live activity finishes
    /// successfully, cleared when consumed or a new activity begins.
    all_done_at: Option<Instant>,
    /// Failure-blink bookkeeping: the most recent failure instant.
    failed_at: Option<Instant>,
}

impl StatusFeed {
    /// Ledger capacity — matches the design doc and the trail ring's shape.
    pub const LEDGER_CAP: usize = 32;
    /// How long a finished activity's line lingers before fading.
    pub const LANE_LINGER: Duration = Duration::from_secs(4);
    /// Lane fade-in / fade-out envelopes.
    pub const LANE_FADE_IN: Duration = Duration::from_millis(150);
    pub const LANE_FADE_OUT: Duration = Duration::from_millis(250);
    /// Rotation cadence between concurrent live activities.
    pub const LANE_ROTATE: Duration = Duration::from_secs(5);
    /// The indeterminate comet's sweep period.
    pub const SWEEP_PERIOD: Duration = Duration::from_secs(4);
    /// Completion hold + fade envelope.
    pub const COMPLETE_HOLD: Duration = Duration::from_millis(400);
    pub const COMPLETE_FADE: Duration = Duration::from_millis(250);
    /// The failure blink's full envelope.
    pub const FAILURE_BLINK: Duration = Duration::from_millis(600);

    /// Begin a new activity. The returned id is the producer's handle and the
    /// ledger's identity.
    pub fn begin(&mut self, now: Instant, kind: ActivityKind, title: impl Into<String>) -> ActivityId {
        self.next_id += 1;
        let id = ActivityId(self.next_id);
        if self.live.is_empty() {
            self.epoch = Some(now);
        }
        self.all_done_at = None;
        self.live.push(Activity {
            id,
            kind,
            title: title.into(),
            message: None,
            progress: None,
            began: now,
            finished: None,
        });
        id
    }

    /// Report determinate progress. A zero denominator is ignored (a bar must
    /// never divide by a lie); progress on a finished/unknown id is ignored.
    pub fn progress(&mut self, id: ActivityId, num: u64, den: u64) {
        if den == 0 {
            return;
        }
        if let Some(activity) = self.live_mut(id) {
            activity.progress = Some(Progress { num: num.min(den), den });
        }
    }

    /// Replace the activity's one-line detail.
    pub fn message(&mut self, id: ActivityId, text: impl Into<String>) {
        if let Some(activity) = self.live_mut(id) {
            activity.message = Some(text.into());
        }
    }

    /// Finish an activity. It lingers in the lane for [`Self::LANE_LINGER`],
    /// then moves wholly to the ledger on the next [`Self::settle`].
    pub fn finish(&mut self, id: ActivityId, now: Instant, outcome: ActivityOutcome) {
        let Some(activity) = self.live_mut(id) else {
            return;
        };
        if activity.finished.is_some() {
            return;
        }
        let failed = matches!(outcome, ActivityOutcome::Failure(_));
        activity.finished = Some((now, outcome));
        if failed {
            self.failed_at = Some(now);
        } else if self.live.iter().all(|a| a.finished.is_some()) {
            self.all_done_at = Some(now);
        }
    }

    /// Housekeeping, called once per painted frame BEFORE reading the states:
    /// moves lingered-out finished activities to the ledger and clears the
    /// epoch when the surface goes fully idle.
    pub fn settle(&mut self, now: Instant) {
        let linger = Self::LANE_LINGER + Self::LANE_FADE_OUT;
        let mut moved = Vec::new();
        self.live.retain(|activity| match activity.finished {
            Some((at, _)) if now.saturating_duration_since(at) >= linger => {
                moved.push(activity.clone());
                false
            }
            _ => true,
        });
        for activity in moved {
            let (finished, outcome) = activity.finished.expect("retained only finished");
            if self.ledger.len() == Self::LEDGER_CAP {
                self.ledger.pop_front();
            }
            self.ledger.push_back(LedgerRow {
                id: activity.id,
                kind: activity.kind,
                title: activity.title,
                outcome,
                began: activity.began,
                finished,
            });
        }
        if self.live.is_empty() {
            self.epoch = None;
        }
    }

    /// The filament's state this frame.
    #[must_use]
    pub fn filament(&self, now: Instant) -> FilamentState {
        // A fresh failure blinks over everything else — it must never be
        // silent — then the lane carries the words.
        if let Some(failed) = self.failed_at {
            let age = now.saturating_duration_since(failed);
            if age < Self::FAILURE_BLINK {
                let q = 1.0 - age.as_secs_f32() / Self::FAILURE_BLINK.as_secs_f32();
                return FilamentState::FailureBlink { q };
            }
        }
        let unfinished: Vec<&Activity> =
            self.live.iter().filter(|a| a.finished.is_none()).collect();
        if unfinished.is_empty() {
            if let Some(done) = self.all_done_at {
                let age = now.saturating_duration_since(done);
                if age < Self::COMPLETE_HOLD {
                    return FilamentState::CompletionFade { q: 1.0 };
                }
                let fade = age.saturating_sub(Self::COMPLETE_HOLD);
                if fade < Self::COMPLETE_FADE {
                    let q = 1.0 - fade.as_secs_f32() / Self::COMPLETE_FADE.as_secs_f32();
                    return FilamentState::CompletionFade { q };
                }
            }
            return FilamentState::Hidden;
        }
        // Aggregation: weighted by declared cost (the denominator), and ANY
        // member without an honest fraction degrades the whole filament to
        // indeterminate — a bar that mixes real fractions with guesses is a
        // guess wearing a ruler.
        let mut num: u128 = 0;
        let mut den: u128 = 0;
        for activity in &unfinished {
            match activity.progress {
                Some(progress) => {
                    num += u128::from(progress.num);
                    den += u128::from(progress.den);
                }
                None => {
                    let phase = self.phase(now, Self::SWEEP_PERIOD);
                    return FilamentState::Indeterminate { phase };
                }
            }
        }
        if den == 0 {
            let phase = self.phase(now, Self::SWEEP_PERIOD);
            return FilamentState::Indeterminate { phase };
        }
        FilamentState::Determinate((num as f64 / den as f64) as f32)
    }

    /// The message lane's line this frame, if any: live activities rotate
    /// newest-first every [`Self::LANE_ROTATE`]; a finished activity's line
    /// lingers then fades. `budget` is the renderer's character budget.
    #[must_use]
    pub fn lane(&self, now: Instant, budget: usize) -> Option<LaneLine> {
        if self.live.is_empty() {
            return None;
        }
        // Newest first, exactly as the design reads.
        let mut candidates: Vec<&Activity> = self.live.iter().collect();
        candidates.sort_by(|a, b| b.began.cmp(&a.began));
        let slot = if candidates.len() > 1 {
            (self.elapsed_since_epoch(now).as_secs_f64()
                / Self::LANE_ROTATE.as_secs_f64()) as usize
                % candidates.len()
        } else {
            0
        };
        let activity = candidates[slot];
        let full_text = match (&activity.finished, &activity.message) {
            (Some((_, ActivityOutcome::Failure(reason))), _) => {
                format!("{} — {}", activity.title, reason)
            }
            (Some((_, ActivityOutcome::Success)), _) => activity.title.clone(),
            (None, Some(message)) => message.clone(),
            (None, None) => activity.title.clone(),
        };
        let alpha = self.lane_alpha(activity, now);
        if alpha <= 0.0 {
            return None;
        }
        Some(LaneLine {
            id: activity.id,
            kind: activity.kind,
            text: middle_truncate(&full_text, budget),
            full_text,
            alpha,
        })
    }

    /// Live activities (unfinished first, then lingering), for `appstatus`.
    #[must_use]
    pub fn live_rows(&self) -> &[Activity] {
        &self.live
    }

    /// The completed ledger, oldest first.
    #[must_use]
    pub fn ledger_rows(&self) -> impl Iterator<Item = &LedgerRow> {
        self.ledger.iter()
    }

    /// Whether anything at all is live (renderers skip every per-frame cost
    /// on the common idle path with one Vec-len read — the rain/ink-pop law).
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.live.is_empty()
    }

    fn live_mut(&mut self, id: ActivityId) -> Option<&mut Activity> {
        self.live
            .iter_mut()
            .find(|activity| activity.id == id && activity.finished.is_none())
    }

    fn elapsed_since_epoch(&self, now: Instant) -> Duration {
        self.epoch
            .map(|epoch| now.saturating_duration_since(epoch))
            .unwrap_or_default()
    }

    fn phase(&self, now: Instant, period: Duration) -> f32 {
        let elapsed = self.elapsed_since_epoch(now).as_secs_f64();
        ((elapsed / period.as_secs_f64()).fract()) as f32
    }

    fn lane_alpha(&self, activity: &Activity, now: Instant) -> f32 {
        let since_begin = now.saturating_duration_since(activity.began);
        let fade_in = (since_begin.as_secs_f32()
            / Self::LANE_FADE_IN.as_secs_f32())
        .clamp(0.0, 1.0);
        match activity.finished {
            None => fade_in,
            Some((at, _)) => {
                let since_finish = now.saturating_duration_since(at);
                if since_finish <= Self::LANE_LINGER {
                    fade_in
                } else {
                    let fade = since_finish - Self::LANE_LINGER;
                    (1.0 - fade.as_secs_f32() / Self::LANE_FADE_OUT.as_secs_f32())
                        .clamp(0.0, 1.0)
                        .min(fade_in)
                }
            }
        }
    }
}

/// One live activity's public view (the `appstatus` verb's row source).
impl Activity {
    #[must_use]
    pub fn id(&self) -> ActivityId {
        self.id
    }
    #[must_use]
    pub fn kind(&self) -> ActivityKind {
        self.kind
    }
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }
    #[must_use]
    pub fn progress_pair(&self) -> Option<(u64, u64)> {
        self.progress.map(|p| (p.num, p.den))
    }
    #[must_use]
    pub fn began(&self) -> Instant {
        self.began
    }
    #[must_use]
    pub fn outcome(&self) -> Option<&ActivityOutcome> {
        self.finished.as_ref().map(|(_, outcome)| outcome)
    }
}

/// Middle-ellipsis truncation to `budget` characters (chars, not bytes — the
/// lane is text, and a split UTF-8 boundary is a paint bug wearing a panic).
/// A budget too small for the ellipsis degrades to a hard prefix.
#[must_use]
pub fn middle_truncate(text: &str, budget: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= budget {
        return text.to_string();
    }
    if budget <= 1 {
        return chars.iter().take(budget).collect();
    }
    let keep = budget - 1;
    let head = keep.div_ceil(2);
    let tail = keep - head;
    let mut out: String = chars[..head].iter().collect();
    out.push('…');
    out.extend(chars[chars.len() - tail..].iter());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }
    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn idle_feed_hides_everything_and_costs_one_len_read() {
        let feed = StatusFeed::default();
        assert!(feed.is_idle());
        assert_eq!(feed.filament(t0()), FilamentState::Hidden);
        assert!(feed.lane(t0(), 60).is_none());
    }

    #[test]
    fn determinate_aggregation_is_cost_weighted_and_capped() {
        let base = t0();
        let mut feed = StatusFeed::default();
        let a = feed.begin(base, ActivityKind::Toolset, "install");
        let b = feed.begin(base, ActivityKind::Update, "download");
        feed.progress(a, 900, 1000);
        feed.progress(b, 0, 9000);
        match feed.filament(at(base, 200)) {
            FilamentState::Determinate(f) => {
                assert!((f - 0.09).abs() < 1e-6, "900/10000 weighted, got {f}");
            }
            other => panic!("expected determinate, got {other:?}"),
        }
        // num can never exceed den through the API.
        feed.progress(a, 5000, 1000);
        match feed.filament(at(base, 200)) {
            FilamentState::Determinate(f) => assert!(f <= 1.0),
            other => panic!("expected determinate, got {other:?}"),
        }
    }

    #[test]
    fn one_indeterminate_member_poisons_the_whole_filament() {
        let base = t0();
        let mut feed = StatusFeed::default();
        let a = feed.begin(base, ActivityKind::Toolset, "install");
        feed.progress(a, 1, 2);
        feed.begin(base, ActivityKind::Drive, "dialing");
        assert!(matches!(
            feed.filament(at(base, 100)),
            FilamentState::Indeterminate { .. }
        ));
    }

    #[test]
    fn zero_denominator_progress_is_refused() {
        let base = t0();
        let mut feed = StatusFeed::default();
        let a = feed.begin(base, ActivityKind::Update, "check");
        feed.progress(a, 5, 0);
        assert!(matches!(
            feed.filament(at(base, 100)),
            FilamentState::Indeterminate { .. }
        ));
    }

    #[test]
    fn success_holds_then_fades_then_hides_and_ledgers() {
        let base = t0();
        let mut feed = StatusFeed::default();
        let a = feed.begin(base, ActivityKind::Toolset, "install");
        feed.progress(a, 1, 1);
        feed.finish(a, at(base, 1000), ActivityOutcome::Success);
        assert!(matches!(
            feed.filament(at(base, 1100)),
            FilamentState::CompletionFade { q } if q == 1.0
        ));
        assert!(matches!(
            feed.filament(at(base, 1500)),
            FilamentState::CompletionFade { q } if q < 1.0
        ));
        assert_eq!(feed.filament(at(base, 2000)), FilamentState::Hidden);
        // The lane lingers, then the activity moves wholly to the ledger.
        assert!(feed.lane(at(base, 3000), 60).is_some());
        feed.settle(at(base, 6000));
        assert!(feed.is_idle());
        assert_eq!(feed.ledger_rows().count(), 1);
    }

    #[test]
    fn failure_blinks_once_and_the_lane_carries_the_reason() {
        let base = t0();
        let mut feed = StatusFeed::default();
        let a = feed.begin(base, ActivityKind::Update, "Update check");
        feed.finish(
            a,
            at(base, 500),
            ActivityOutcome::Failure("rate limited, retrying at :52".into()),
        );
        assert!(matches!(
            feed.filament(at(base, 600)),
            FilamentState::FailureBlink { .. }
        ));
        let lane = feed.lane(at(base, 1500), 120).expect("failure line lingers");
        assert!(lane.full_text.contains("rate limited"));
        assert!(lane.full_text.contains("Update check"));
    }

    #[test]
    fn concurrent_activities_rotate_newest_first() {
        let base = t0();
        let mut feed = StatusFeed::default();
        feed.begin(base, ActivityKind::Toolset, "older");
        feed.begin(at(base, 100), ActivityKind::Drive, "newer");
        let first = feed.lane(at(base, 200), 60).expect("line");
        assert_eq!(first.full_text, "newer");
        let second = feed
            .lane(at(base, 5200), 60)
            .expect("rotated line");
        assert_eq!(second.full_text, "older");
        let third = feed.lane(at(base, 10_200), 60).expect("wrapped line");
        assert_eq!(third.full_text, "newer");
    }

    #[test]
    fn lane_prefers_message_over_title_and_truncates_middle() {
        let base = t0();
        let mut feed = StatusFeed::default();
        let a = feed.begin(base, ActivityKind::Toolset, "Installing ALab toolchain");
        feed.message(a, "3/10 programs, 412 MB of 1.1 GB");
        let lane = feed.lane(at(base, 200), 16).expect("line");
        assert_eq!(lane.full_text, "3/10 programs, 412 MB of 1.1 GB");
        assert_eq!(lane.text.chars().count(), 16);
        assert!(lane.text.contains('…'));
        assert!(lane.text.starts_with("3/10"));
        assert!(lane.text.ends_with("GB"));
    }

    #[test]
    fn ledger_is_bounded_and_drops_oldest() {
        let base = t0();
        let mut feed = StatusFeed::default();
        for i in 0..40u64 {
            let id = feed.begin(at(base, i), ActivityKind::Health, format!("a{i}"));
            feed.finish(id, at(base, i), ActivityOutcome::Success);
            feed.settle(at(base, i + 60_000));
        }
        assert_eq!(feed.ledger_rows().count(), StatusFeed::LEDGER_CAP);
        assert_eq!(
            feed.ledger_rows().next().expect("oldest").title,
            "a8",
            "oldest rows dropped first"
        );
    }

    #[test]
    fn middle_truncate_respects_char_boundaries_and_tiny_budgets() {
        assert_eq!(middle_truncate("short", 60), "short");
        assert_eq!(middle_truncate("abcdef", 5), "ab…ef");
        assert_eq!(middle_truncate("日本語のテキスト", 5), "日本…スト");
        assert_eq!(middle_truncate("abcdef", 1), "a");
        assert_eq!(middle_truncate("abcdef", 0), "");
    }

    #[test]
    fn a_new_activity_cancels_the_completion_fade() {
        let base = t0();
        let mut feed = StatusFeed::default();
        let a = feed.begin(base, ActivityKind::Toolset, "one");
        feed.finish(a, at(base, 100), ActivityOutcome::Success);
        feed.begin(at(base, 200), ActivityKind::Drive, "two");
        assert!(
            matches!(
                feed.filament(at(base, 300)),
                FilamentState::Indeterminate { .. }
            ),
            "a live activity outranks the finished one's fade"
        );
    }
}
