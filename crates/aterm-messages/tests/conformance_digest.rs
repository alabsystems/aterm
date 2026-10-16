// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1: the shipping `MessageCenter` follows the derived `ExceptionsDigest`
//! model in lock step. Every real transition is one the model enables, and
//! after each the real center's `(open, glass)` equals the model's, so the
//! digest never wants a band row (`LaterNeverWantsARow`), a dismissal holds
//! inside its span (`DismissalHolds`), and the cap files rather than drops
//! (`NothingIsLost`). The negative control plants an exception posted as a
//! plain warning and shows the same projection catches it.

use aterm_messages::need::{Ask, Fingerprint, LaterAsk, Mover, Reason, Resurface, Span, Then, Why};
use aterm_messages::{
    Duration, Instant, Message, MessageCenter, MessageId, MessageLog, Outcome, PostOutcome,
    Severity, WallStamp, tags,
};
use aterm_spec::derive::exceptions_digest_model;

const DAY_MS: u64 = 86_400_000;

fn fact_key(n: usize) -> String {
    format!("f{n}")
}

fn later(n: usize) -> Message {
    Message::new(
        tags::HARNESS,
        Severity::Warn,
        format!("Claude in tab {n} is behind"),
    )
    .later(LaterAsk::person(
        Why::OnlyYou(Reason::new("its own background work holds the move")),
        Then::step("Stop that work in the tab when it suits you"),
        Fingerprint::new("harness.upgrade", &fact_key(n), "own-work"),
        Resurface::WAIT,
    ))
}

fn now_row(n: usize) -> Message {
    Message::new(
        tags::CONFIG,
        Severity::Warn,
        format!("aterm.toml has a bad key {n}"),
    )
    .key(&format!("config.k{n}"))
    .now(Ask::flash(
        Why::YourAction(Reason::new("you typed a key aterm does not know")),
        Then::press(Mover::open_config_editor(None), "Fix the key in aterm.toml"),
        Span::Minute,
    ))
}

struct Rng(u64);
impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

#[test]
fn the_real_center_follows_the_model_in_lock_step() {
    let model = exceptions_digest_model();
    let names = [
        "PostNow",
        "FoldNow",
        "PostLater",
        "PostLaterAtCap",
        "Suppress",
        "FileAll",
        "Resurface",
    ];
    let mut fired = std::collections::BTreeMap::<&str, u32>::new();
    for seed in 1..=30u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        #[expect(
            clippy::disallowed_methods,
            reason = "test code: the one instant a test mints and feeds in"
        )]
        let now = Instant::now();
        let mut c = MessageCenter::new(MessageLog::empty(), now);
        let mut st = model.init_state();
        let mut wall = 1_790_000_000_000_u64;
        let stamp = |w: u64| WallStamp { unix_ms: w };
        let mut rows: Vec<MessageId> = Vec::new();
        let (mut next_row, mut next_fact) = (0usize, 0usize);
        // facts whose quiet span is still running (filed since the last
        // Resurface) and facts whose span is over
        let mut recent: Vec<usize> = Vec::new();
        let mut filed_facts: Vec<usize> = Vec::new();
        for step in 0..400 {
            wall += 1_000;
            let enabled: Vec<&str> = names
                .iter()
                .copied()
                .filter(|a| model.action_enabled(a, &st))
                .collect();
            // Lean on posting exceptions so the run reaches the cap.
            let posting: Vec<&str> = enabled
                .iter()
                .copied()
                .filter(|a| a.starts_with("PostLater"))
                .collect();
            let action = if !posting.is_empty() && rng.below(3) != 0 {
                posting[rng.below(posting.len())]
            } else {
                enabled[rng.below(enabled.len())]
            };
            match action {
                "PostNow" => {
                    next_row += 1;
                    rows.push(c.post(now_row(next_row), stamp(wall), now).id);
                }
                "FoldNow" => {
                    let id = rows.remove(0);
                    assert!(c.resolve(id, Outcome::Ok, now), "seed {seed} step {step}");
                }
                "PostLater" => {
                    // a fresh fact, or (once its quiet span is over) one filed before
                    let n = if !filed_facts.is_empty() && rng.below(2) == 0 {
                        filed_facts.remove(0)
                    } else {
                        next_fact += 1;
                        next_fact
                    };
                    let p = c.post(later(n), stamp(wall), now);
                    assert_eq!(p.outcome, PostOutcome::New, "seed {seed} step {step}");
                }
                "PostLaterAtCap" => {
                    next_fact += 1;
                    let p = c.post(later(next_fact), stamp(wall), now);
                    assert_eq!(p.outcome, PostOutcome::New, "seed {seed} step {step}");
                }
                "Suppress" => {
                    let n = recent[rng.below(recent.len())];
                    let p = c.post(later(n), stamp(wall), now);
                    assert_eq!(
                        p.outcome,
                        PostOutcome::Suppressed,
                        "seed {seed} step {step}"
                    );
                }
                "FileAll" => {
                    let open = c.digest().len();
                    // the facts the digest holds are the quiet ones now
                    recent.extend(c.digest().entries.iter().filter_map(|e| {
                        e.key
                            .split('.')
                            .nth(2)?
                            .strip_prefix('f')?
                            .parse::<usize>()
                            .ok()
                    }));
                    assert_eq!(c.file_digest(stamp(wall), now), open);
                }
                "Resurface" => {
                    // time passes: 91 days clear every resurface span, the
                    // backed-off ones (at most 90 days) included
                    wall += 91 * DAY_MS;
                    filed_facts.append(&mut recent);
                }
                other => unreachable!("{other}"),
            }
            assert!(
                model.fire(action, &mut st),
                "seed {seed} step {step}: {action} {st:?}"
            );
            *fired.entry(action).or_default() += 1;
            assert_eq!(
                i64::try_from(c.digest().len()).unwrap(),
                st["open"],
                "seed {seed} step {step} after {action}: open"
            );
            assert_eq!(
                i64::from(c.wanted_rows()),
                st["glass"],
                "seed {seed} step {step} after {action}: the band's rows"
            );
            assert!(model.check_invariant("LaterNeverWantsARow", &st));
            assert!(model.check_invariant("DismissalHolds", &st));
            assert!(model.check_invariant("NothingIsLost", &st));
            assert!(model.check_invariant("OpenIsBounded", &st));
        }
    }
    for a in names {
        assert!(
            fired.get(a).copied().unwrap_or(0) > 0,
            "the lock-step run never fired {a}: {fired:?}"
        );
    }

    // Negative control: an exception posted as a plain warning moves the
    // band's rows without the model moving, and the same projection sees it.
    #[expect(
        clippy::disallowed_methods,
        reason = "test code: the one instant a test mints and feeds in"
    )]
    let now = Instant::now();
    let mut c = MessageCenter::new(MessageLog::empty(), now);
    let st = model.init_state();
    c.post(
        Message::new(tags::HARNESS, Severity::Warn, "Claude in tab 1 is behind"),
        WallStamp { unix_ms: 1 },
        now,
    );
    assert_ne!(
        i64::from(c.wanted_rows()),
        st["glass"],
        "a seated exception is seen"
    );
    let _ = Duration::ZERO;
}
