// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The event loop's deadline healer: what `about_to_wait` actually arms when a
//! producer keeps re-arming the past. A field of `App`, written only by the
//! event-loop thread, so `metrics reset` cannot reach it: a reset never
//! re-logs a live spin and never hands a clamped owner a fresh window.
//! [`crate::metrics::note_deadline`] books what this decides.
//!
//! STALE-ARM HEAL (busy-rearm audit, item 3): the SAME owner arming a deadline
//! more than [`STALE_ARM_HEAL_FLOOR`] in the past on CONSECUTIVE turns is a
//! scheduler bug — an honest deadline can be a little late (the turn that
//! computed it did real work first), but a deadline that stays far in the past
//! across turns is a self-rearming `WaitUntil(past)` spin. [`DeadlineHealer::heal`]
//! clamps the second and later arms of such a streak to `now + floor`
//! (`stale_arm_heals` counts them), so the loop survives the whole class
//! VISIBLY (the wake_heals precedent: heal, count, log once per episode)
//! instead of burning a core at the event loop's floor. `streak` holds the
//! previous turn's over-floor-late owner (`DeadlineOwner::None` = no streak);
//! `episode_logged` makes the log once-per-episode, re-arming when a healthy
//! arm ends the streak.
//!
//! PER-OWNER PAST-ARM STREAK HEAL (follow-ups items 18/19): the detector the
//! 250 ms floor above is structurally blind to. That heal only *considers* arms
//! with `late > 250 ms`, so a producer arming `Instant::now()` every turn — the
//! exact pre-fix shape of the since-retired Smart Titles retry — spins at full
//! event-loop rate with `late ≈ 0` and takes the branch that RESETS its streak;
//! and its single streak slot is defeated outright by two owners alternating
//! stale arms. This detector is DECOUPLED from the floor and PER OWNER: each
//! owner keeps a 32-arm past/future window (`windows`, packed bits + fill
//! count), and when more than 90% of one owner's last 32 arms were already past
//! when armed (>= [`PAST_ARM_WINDOW_TRIGGER`] of [`PAST_ARM_WINDOW`]) that
//! owner's re-arms are clamped to `now + frame` — a spin degrades to display
//! cadence instead of the event loop's floor — and counted in a NAMED per-owner
//! heal ledger (`past_arm_streak_heals` beside `deadline_arms_by_owner`).
//! Occasional past arms never trigger: a busy turn's late arm is legitimate,
//! and the window tolerates 3 healthy arms in 32. When the coarser 250 ms heal
//! already moved the same arm further out, that stronger clamp is kept — this
//! one only ever raises a deadline that would otherwise re-arm the past.

use std::time::{Duration, Instant};

use crate::metrics::DeadlineOwner;

/// Detection threshold AND clamp distance for the stale-arm heal. At the
/// observation-gate scale (the status classifier's 250 ms `min_interval`) on
/// purpose: a healed spin degrades to <= 4 Hz, and no legitimate late arm —
/// a busy turn runs tens of milliseconds, not hundreds — ever trips it.
const STALE_ARM_HEAL_FLOOR_NS: u64 = 250_000_000;
const STALE_ARM_HEAL_FLOOR: Duration = Duration::from_nanos(STALE_ARM_HEAL_FLOOR_NS);

const PAST_ARM_WINDOW: u32 = 32;
/// `> 90%` of the 32-arm window: `ceil(0.9 * 32) + ...` — the smallest count
/// strictly greater than 28.8 past arms.
const PAST_ARM_WINDOW_TRIGGER: u32 = 29;
/// One 60 Hz display frame: the clamp cadence for a windowed-streak spin. The
/// producer's own work still runs (its deadline stays armed and near), but at
/// frame rate rather than event-loop rate.
const PAST_ARM_STREAK_CLAMP_NS: u64 = 16_666_667;
const PAST_ARM_STREAK_CLAMP: Duration = Duration::from_nanos(PAST_ARM_STREAK_CLAMP_NS);
const PAST_ARM_BITS_MASK: u64 = 0xFFFF_FFFF;

/// Advance one owner's packed past-arm window by one arm and decide whether
/// the streak detector fires. PURE on the packed word so the law is
/// unit-testable with synthetic histories: the trigger requires a FULL window
/// (32 arms seen) with >= [`PAST_ARM_WINDOW_TRIGGER`] of them past — never a
/// short, mostly-past warmup.
const fn past_arm_window_step(packed: u64, past: bool) -> (u64, bool) {
    let bits = (((packed & PAST_ARM_BITS_MASK) << 1) | past as u64) & PAST_ARM_BITS_MASK;
    let seen = {
        let next = (packed >> 32) + 1;
        if next > PAST_ARM_WINDOW as u64 {
            PAST_ARM_WINDOW as u64
        } else {
            next
        }
    };
    let trigger = seen == PAST_ARM_WINDOW as u64 && bits.count_ones() >= PAST_ARM_WINDOW_TRIGGER;
    ((seen << 32) | bits, trigger)
}

/// The healer's memory between event-loop turns. See the module note.
#[derive(Debug)]
pub(crate) struct DeadlineHealer {
    /// The previous turn's over-floor-late owner; `DeadlineOwner::None` is no
    /// streak.
    streak: DeadlineOwner,
    /// Whether this streak's episode has logged its line.
    episode_logged: bool,
    /// Per owner, by `index()`. Low 32 bits: the last-32-arms past bits
    /// (bit 0 = newest; 1 = armed already past). Bits 32..: how many arms the
    /// window has seen, saturating at 32.
    windows: [u64; DeadlineOwner::COUNT],
}

impl Default for DeadlineHealer {
    fn default() -> Self {
        Self {
            streak: DeadlineOwner::None,
            episode_logged: false,
            windows: [0; DeadlineOwner::COUNT],
        }
    }
}

/// What [`DeadlineHealer::heal`] decided for one requested deadline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Heal {
    /// The deadline to arm: the requested one, or a clamp.
    pub(crate) armed: Option<Instant>,
    /// The stale-arm floor clamped this arm (`stale_arm_heals`).
    pub(crate) floor: bool,
    /// The owner's windowed streak clamped this arm (`past_arm_streak_heals`).
    pub(crate) streak: bool,
}

impl DeadlineHealer {
    /// Decide what to arm for `owner`'s requested `deadline` (`None`: pure
    /// `Wait`). Normally the request, unchanged — clamped to
    /// `now + STALE_ARM_HEAL_FLOOR` only when the SAME owner arms a deadline
    /// more than the floor in the past on consecutive turns, and clamped to
    /// `now + PAST_ARM_STREAK_CLAMP` (one display frame) when > 90% of that
    /// owner's last [`PAST_ARM_WINDOW`] arms were already past regardless of
    /// HOW late (the stronger of the two clamps wins). A single late arm passes
    /// through untouched: a busy turn legitimately computes a deadline that is
    /// already behind it, and healing that would delay real work.
    #[must_use = "the RETURNED instant is the healed one — arming the argument re-arms the stale deadline this fn exists to clamp"]
    pub(crate) fn heal(
        &mut self,
        owner: DeadlineOwner,
        deadline: Option<Instant>,
        now: Instant,
    ) -> Heal {
        let mut heal = Heal {
            armed: deadline,
            floor: false,
            streak: false,
        };
        match deadline {
            // A pure-Wait turn breaks any streak: consecutiveness is the bug's
            // signature, and it just ended.
            None => self.end_streak(),
            Some(deadline) if deadline >= now => {
                self.end_streak();
                // A healthy arm feeds the owner's 32-arm window too: it dilutes a
                // past streak instead of resetting it, which is exactly what makes
                // this detector immune to the `late ≈ 0`/"a nanosecond in the
                // future" blip that clears the consecutive-turn latch above.
                let _ = self.step_window(owner, false);
            }
            Some(deadline) => {
                let late =
                    u64::try_from(now.duration_since(deadline).as_nanos()).unwrap_or(u64::MAX);
                if late > STALE_ARM_HEAL_FLOOR_NS && owner != DeadlineOwner::None {
                    if std::mem::replace(&mut self.streak, owner) == owner {
                        heal.armed = Some(now + STALE_ARM_HEAL_FLOOR);
                        heal.floor = true;
                        if !std::mem::replace(&mut self.episode_logged, true) {
                            aterm_log::warn!(
                                "healed a stale deadline arm: owner={} late {} ms on consecutive \
                                 turns — clamped to now+{} ms (scheduler bug; see stale_arm_heals)",
                                owner.as_str(),
                                late / 1_000_000,
                                STALE_ARM_HEAL_FLOOR_NS / 1_000_000
                            );
                        }
                    }
                } else if late <= STALE_ARM_HEAL_FLOOR_NS {
                    self.end_streak();
                }
                // The WINDOWED per-owner detector (items 18/19): fires when > 90%
                // of this owner's last 32 arms were already past, at ANY lateness
                // — the `late ≈ 0` spin the floor above cannot see, and the
                // alternating-owner spin its single streak slot cannot see. Clamp
                // this owner's re-arm to one display frame ahead unless a stronger
                // heal (the 250 ms floor above) already moved it further out.
                if self.step_window(owner, true) {
                    let clamp = now + PAST_ARM_STREAK_CLAMP;
                    if heal.armed.is_none_or(|current| current < clamp) {
                        heal.armed = Some(clamp);
                        heal.streak = true;
                    }
                }
            }
        }
        heal
    }

    fn end_streak(&mut self) {
        self.streak = DeadlineOwner::None;
        self.episode_logged = false;
    }

    /// Push one arm into the owner's window and report whether the streak
    /// detector fires. `DeadlineOwner::None` keeps no window.
    fn step_window(&mut self, owner: DeadlineOwner, past: bool) -> bool {
        if owner == DeadlineOwner::None {
            return false;
        }
        let window = &mut self.windows[owner.index()];
        let (next, trigger) = past_arm_window_step(*window, past);
        *window = next;
        trigger
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DeadlineHealer, Heal, PAST_ARM_STREAK_CLAMP, PAST_ARM_WINDOW, STALE_ARM_HEAL_FLOOR,
        past_arm_window_step,
    };
    use crate::metrics::DeadlineOwner;
    use std::time::{Duration, Instant};

    /// A heal that changed nothing.
    fn passed(deadline: Option<Instant>) -> Heal {
        Heal {
            armed: deadline,
            floor: false,
            streak: false,
        }
    }

    /// HEAL-AT-THE-FOLD (busy-rearm audit, item 3). One late arm is legal — a
    /// busy turn computes its deadline after real work — but the SAME owner
    /// arming more than the floor in the past on CONSECUTIVE turns is a
    /// self-rearming `WaitUntil(past)` spin, and the fold must clamp it to
    /// `now + floor` and say so, so the loop survives the whole class visibly.
    #[test]
    fn consecutive_same_owner_stale_arms_heal_to_the_floor() {
        let mut healer = DeadlineHealer::default();
        let now = Instant::now();
        let future = now + Duration::from_millis(5);
        let stale = now - Duration::from_secs(2);
        let owner = DeadlineOwner::Blink;

        // First offence passes through unhealed: it may be honest lateness.
        assert_eq!(healer.heal(owner, Some(stale), now), passed(Some(stale)));

        // The second consecutive same-owner offence is the bug's signature:
        // clamped to the floor, and reported.
        assert_eq!(
            healer.heal(owner, Some(stale), now),
            Heal {
                armed: Some(now + STALE_ARM_HEAL_FLOOR),
                floor: true,
                streak: false,
            },
            "the armed deadline is healed to now + floor"
        );

        // A DIFFERENT owner does not inherit the streak.
        assert_eq!(
            healer.heal(DeadlineOwner::Rain, Some(stale), now),
            passed(Some(stale))
        );

        // A healthy arm ends the episode; the next late arm is a fresh first
        // offence and passes through again.
        assert_eq!(healer.heal(owner, Some(future), now), passed(Some(future)));
        assert_eq!(healer.heal(owner, Some(stale), now), passed(Some(stale)));

        // Lateness at or under the floor never trips the healer: that is the
        // ordinary busy-turn case, on any number of consecutive turns.
        let barely = now - Duration::from_millis(100);
        assert_eq!(healer.heal(owner, Some(barely), now), passed(Some(barely)));
        assert_eq!(healer.heal(owner, Some(barely), now), passed(Some(barely)));

        // A pure-Wait turn also breaks a streak.
        assert_eq!(healer.heal(owner, Some(stale), now), passed(Some(stale)));
        assert_eq!(healer.heal(owner, None, now), passed(None));
        assert_eq!(healer.heal(owner, Some(stale), now), passed(Some(stale)));
    }

    /// The PURE window law behind the items-18/19 detector: a trigger needs a
    /// FULL 32-arm window with more than 90% of it past (>= 29 of 32) — never
    /// a short mostly-past warmup — and a healthy arm DILUTES the window
    /// instead of resetting it (the property the consecutive-turn latch above
    /// lacks, and exactly how a `late ≈ 0`/nanosecond-in-the-future blip used
    /// to hide a spin).
    #[test]
    fn past_arm_window_step_requires_a_full_window_over_ninety_percent_past() {
        let run = |arms: &[bool]| {
            let mut packed = 0u64;
            let mut last = false;
            for &past in arms {
                let (next, trigger) = past_arm_window_step(packed, past);
                packed = next;
                last = trigger;
            }
            (packed, last)
        };
        // 31 consecutive past arms: window not yet full, never triggers.
        let mut warmup = vec![true; 31];
        assert!(!run(&warmup).1, "a not-yet-full window must not convict");
        // The 32nd closes an all-past window; the fill count saturates at 32.
        warmup.push(true);
        let (packed, trigger) = run(&warmup);
        assert!(trigger, "32/32 past is the streak signature");
        assert_eq!(packed >> 32, u64::from(PAST_ARM_WINDOW), "fill saturates");
        assert!(
            past_arm_window_step(packed, true).1,
            "the streak keeps triggering while it persists"
        );
        // Boundary: 29/32 past (> 90%) triggers; 28/32 does not.
        let mut edge = vec![false; 3];
        edge.extend(std::iter::repeat_n(true, 29));
        assert!(run(&edge).1, "29 of 32 past is over the 90% line");
        let mut under = vec![false; 4];
        under.extend(std::iter::repeat_n(true, 28));
        assert!(!run(&under).1, "28 of 32 past is under the 90% line");
        // Dilution, not reset: one healthy arm inside an otherwise-past
        // window leaves 31/32 past — still a conviction.
        let mut interleaved = vec![true; 16];
        interleaved.push(false);
        interleaved.extend(std::iter::repeat_n(true, 16));
        assert!(
            run(&interleaved).1,
            "a single healthy blip must not acquit a windowed spin"
        );
    }

    /// ITEMS 18/19 (2026-08-24 wake follow-ups): the WINDOWED per-owner
    /// detector heals the two flavours the 250 ms floor above is structurally
    /// blind to — the `late ≈ 0` spin (every arm barely past, each one taking
    /// the branch that RESETS the consecutive-turn latch) and two owners
    /// alternating stale arms (defeating the single streak slot). The clamp
    /// is one display frame, so a healed spin degrades to cadence.
    #[test]
    fn windowed_past_arm_streaks_heal_at_any_lateness_and_per_owner() {
        let mut healer = DeadlineHealer::default();
        let now = Instant::now();
        let future = now + Duration::from_millis(5);
        let clamped = Heal {
            armed: Some(now + PAST_ARM_STREAK_CLAMP),
            floor: false,
            streak: true,
        };
        // 1 ms late: UNDER the 250 ms floor, so the consecutive-turn heal
        // above never fires — this is the pre-fix `arms Instant::now() every
        // turn` shape, previously invisible.
        let barely = now - Duration::from_millis(1);

        // (1) The `late ≈ 0` spin: 31 barely-late arms pass through (an
        // honest busy stretch must not be delayed), the arm that completes an
        // all-past window is clamped to one frame ahead, and the heal
        // SUSTAINS while the spin does.
        let video = DeadlineOwner::Video;
        for _ in 0..31 {
            assert_eq!(
                healer.heal(video, Some(barely), now),
                passed(Some(barely)),
                "a not-yet-full window never heals"
            );
        }
        assert_eq!(
            healer.heal(video, Some(barely), now),
            clamped,
            "arm 32 of an all-past window is clamped to now + one frame"
        );
        assert_eq!(
            healer.heal(video, Some(barely), now),
            clamped,
            "the clamp sustains at frame cadence while the spin persists"
        );

        // (2) 28/32 past stays under the > 90% trigger: occasional lateness —
        // even a lot of it — is not the streak signature.
        let word = DeadlineOwner::WordDecorations;
        for _ in 0..4 {
            assert_eq!(healer.heal(word, Some(future), now), passed(Some(future)));
        }
        for _ in 0..28 {
            assert_eq!(
                healer.heal(word, Some(barely), now),
                passed(Some(barely)),
                "28 of 32 past must pass through unhealed"
            );
        }

        // (3) Two owners ALTERNATING deeply stale arms: the consecutive-turn
        // latch never sees the same owner twice in a row and stays silent,
        // but each owner's own window convicts it.
        let (left, right) = (DeadlineOwner::CursorEffect, DeadlineOwner::Predictor);
        let stale = now - Duration::from_secs(2);
        for _ in 0..31 {
            assert_eq!(
                healer.heal(left, Some(stale), now),
                passed(Some(stale)),
                "the single-slot consecutive heal is defeated by alternation"
            );
            assert_eq!(healer.heal(right, Some(stale), now), passed(Some(stale)));
        }
        assert_eq!(healer.heal(left, Some(stale), now), clamped);
        assert_eq!(healer.heal(right, Some(stale), now), clamped);

        // (4) When the coarser 250 ms heal already moved a consecutive
        // same-owner arm further out, the stronger clamp is KEPT and the
        // windowed heal does not claim that arm.
        assert_eq!(
            healer.heal(left, Some(stale), now),
            clamped,
            "first consecutive offence: the floor heal is not armed yet"
        );
        assert_eq!(
            healer.heal(left, Some(stale), now),
            Heal {
                armed: Some(now + STALE_ARM_HEAL_FLOOR),
                floor: true,
                streak: false,
            },
            "the stronger (floor) clamp wins over the frame clamp, and only it is reported"
        );
    }
}
