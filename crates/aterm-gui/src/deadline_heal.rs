// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The event loop's deadline healer: what `about_to_wait` actually arms when a producer keeps re-arming the past.

use std::time::{Duration, Instant};

use crate::metrics::DeadlineOwner;

const STALE_ARM_HEAL_FLOOR_NS: u64 = 250_000_000;
const STALE_ARM_HEAL_FLOOR: Duration = Duration::from_nanos(STALE_ARM_HEAL_FLOOR_NS);

const PAST_ARM_WINDOW: u32 = 32;
const PAST_ARM_WINDOW_TRIGGER: u32 = 29;
const PAST_ARM_STREAK_CLAMP_NS: u64 = 16_666_667;
const PAST_ARM_STREAK_CLAMP: Duration = Duration::from_nanos(PAST_ARM_STREAK_CLAMP_NS);
const PAST_ARM_BITS_MASK: u64 = 0xFFFF_FFFF;

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

/// The healer's memory between event-loop turns.
#[derive(Debug)]
pub(crate) struct DeadlineHealer {
    /// The previous turn's over-floor-late owner; `DeadlineOwner::None` is no
    /// streak.
    streak: DeadlineOwner,
    /// Whether this streak's episode has logged its line.
    episode_logged: bool,
    /// Per owner, by `index()`. Low 32 bits: the last-32-arms past bits
    /// (bit 0 = newest; 1 = armed already past).
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

/// What `about_to_wait`'s fold kept: the soonest deadline's owner, and each
/// owner's own soonest deadline, which a clamp must not push out.
#[derive(Debug)]
pub(crate) struct DeadlineFold {
    owner: DeadlineOwner,
    by_owner: [Option<Instant>; DeadlineOwner::COUNT],
}

impl Default for DeadlineFold {
    fn default() -> Self {
        Self {
            owner: DeadlineOwner::None,
            by_owner: [None; DeadlineOwner::COUNT],
        }
    }
}

impl DeadlineFold {
    /// Fold `owner`'s `candidate` into the turn's soonest `deadline`.
    pub(crate) fn fold(
        &mut self,
        deadline: &mut Option<Instant>,
        candidate: Instant,
        owner: DeadlineOwner,
    ) {
        let own = &mut self.by_owner[owner.index()];
        if own.is_none_or(|current| candidate < current) {
            *own = Some(candidate);
        }
        if deadline.is_none_or(|current| candidate < current) {
            *deadline = Some(candidate);
            self.owner = owner;
        }
    }

    /// The soonest deadline's owner.
    pub(crate) const fn owner(&self) -> DeadlineOwner {
        self.owner
    }

    /// What to arm for `heal`. A clamp stands in for the soonest owner's past
    /// arm only: another owner's deadline still ahead of `now` and due before the
    /// clamp (a window's next pet or content frame) keeps its slot.
    pub(crate) fn armed(&self, heal: Heal, now: Instant) -> Option<Instant> {
        if !(heal.floor || heal.streak) {
            return heal.armed;
        }
        let ahead = self
            .by_owner
            .iter()
            .enumerate()
            .filter(|&(index, _)| index != self.owner.index())
            .filter_map(|(_, deadline)| deadline.filter(|deadline| *deadline > now))
            .min();
        match (heal.armed, ahead) {
            (Some(clamp), Some(ahead)) => Some(clamp.min(ahead)),
            (armed, _) => armed,
        }
    }
}

impl DeadlineHealer {
    /// Decide what to arm for `owner`'s requested `deadline` (`None`: pure `Wait`).
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
            None => self.end_streak(),
            Some(deadline) if deadline >= now => {
                self.end_streak();
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

    const fn end_streak(&mut self) {
        self.streak = DeadlineOwner::None;
        self.episode_logged = false;
    }

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
        DeadlineFold, DeadlineHealer, Heal, PAST_ARM_STREAK_CLAMP, PAST_ARM_WINDOW,
        STALE_ARM_HEAL_FLOOR, past_arm_window_step,
    };
    use crate::metrics::DeadlineOwner;
    use std::time::{Duration, Instant};

    fn passed(deadline: Option<Instant>) -> Heal {
        Heal {
            armed: deadline,
            floor: false,
            streak: false,
        }
    }

    #[test]
    fn consecutive_same_owner_stale_arms_heal_to_the_floor() {
        let mut healer = DeadlineHealer::default();
        let now = Instant::now();
        let future = now + Duration::from_millis(5);
        let stale = now - Duration::from_secs(2);
        let owner = DeadlineOwner::Blink;

        assert_eq!(healer.heal(owner, Some(stale), now), passed(Some(stale)));

        assert_eq!(
            healer.heal(owner, Some(stale), now),
            Heal {
                armed: Some(now + STALE_ARM_HEAL_FLOOR),
                floor: true,
                streak: false,
            },
            "the armed deadline is healed to now + floor"
        );

        assert_eq!(
            healer.heal(DeadlineOwner::Rain, Some(stale), now),
            passed(Some(stale))
        );

        assert_eq!(healer.heal(owner, Some(future), now), passed(Some(future)));
        assert_eq!(healer.heal(owner, Some(stale), now), passed(Some(stale)));

        let barely = now - Duration::from_millis(100);
        assert_eq!(healer.heal(owner, Some(barely), now), passed(Some(barely)));
        assert_eq!(healer.heal(owner, Some(barely), now), passed(Some(barely)));

        assert_eq!(healer.heal(owner, Some(stale), now), passed(Some(stale)));
        assert_eq!(healer.heal(owner, None, now), passed(None));
        assert_eq!(healer.heal(owner, Some(stale), now), passed(Some(stale)));
    }

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
        let mut warmup = vec![true; 31];
        assert!(!run(&warmup).1, "a not-yet-full window must not convict");
        warmup.push(true);
        let (packed, trigger) = run(&warmup);
        assert!(trigger, "32/32 past is the streak signature");
        assert_eq!(packed >> 32, u64::from(PAST_ARM_WINDOW), "fill saturates");
        assert!(
            past_arm_window_step(packed, true).1,
            "the streak keeps triggering while it persists"
        );
        let mut edge = vec![false; 3];
        edge.extend(std::iter::repeat_n(true, 29));
        assert!(run(&edge).1, "29 of 32 past is over the 90% line");
        let mut under = vec![false; 4];
        under.extend(std::iter::repeat_n(true, 28));
        assert!(!run(&under).1, "28 of 32 past is under the 90% line");
        let mut interleaved = vec![true; 16];
        interleaved.push(false);
        interleaved.extend(std::iter::repeat_n(true, 16));
        assert!(
            run(&interleaved).1,
            "a single healthy blip must not acquit a windowed spin"
        );
    }

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
        let barely = now - Duration::from_millis(1);

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

    #[test]
    fn a_clamp_keeps_another_owners_deadline_due_before_it() {
        let now = Instant::now();
        let barely = now - Duration::from_millis(1);
        let stale = now - Duration::from_secs(2);
        let frame = now + Duration::from_millis(4);
        let fold = |past: Instant, pet: Instant| {
            let mut deadline = None;
            let mut fold = DeadlineFold::default();
            fold.fold(&mut deadline, pet, DeadlineOwner::CursorEffect);
            fold.fold(&mut deadline, past, DeadlineOwner::ConsentCard);
            (deadline, fold)
        };
        let arm = |healer: &mut DeadlineHealer,
                   (deadline, fold): (Option<Instant>, DeadlineFold)| {
            assert_eq!(
                fold.owner(),
                DeadlineOwner::ConsentCard,
                "the past arm is the soonest"
            );
            let heal = healer.heal(fold.owner(), deadline, now);
            (heal, fold.armed(heal, now))
        };

        let mut healer = DeadlineHealer::default();
        for _ in 0..31 {
            assert_eq!(arm(&mut healer, fold(barely, frame)).1, Some(barely));
        }
        let (heal, armed) = arm(&mut healer, fold(barely, frame));
        assert!(heal.streak, "arm 32 of an all-past window is a streak");
        assert_eq!(
            armed,
            Some(frame),
            "the pet's frame due before the clamp keeps its slot"
        );
        let late = now + Duration::from_millis(40);
        assert_eq!(
            arm(&mut healer, fold(barely, late)).1,
            Some(now + PAST_ARM_STREAK_CLAMP),
            "a frame due after the clamp waits for the clamp"
        );
        assert_eq!(
            arm(&mut healer, fold(barely, now - Duration::from_micros(500))).1,
            Some(now + PAST_ARM_STREAK_CLAMP),
            "another owner's past deadline is not kept: it would spin"
        );

        let mut healer = DeadlineHealer::default();
        assert_eq!(arm(&mut healer, fold(stale, frame)).1, Some(stale));
        let (heal, armed) = arm(&mut healer, fold(stale, frame));
        assert!(heal.floor, "a second consecutive stale arm is floored");
        assert_eq!(armed, Some(frame), "the floor keeps the pet's frame too");
    }
}
