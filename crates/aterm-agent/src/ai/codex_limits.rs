// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Codex's usage limits as its own records write them.

use std::path::Path;

use aterm_json::Value;

use crate::harness::accounts::Sample;
use crate::harness::codex_rollout::{
    Prefix, TurnReader, first_line, rollouts_by_write, stamp_ms, stream_lines, tail_after,
};
use crate::harness::profile::{AccountKey, Word};

/// How much of a rollout's end is read for its usage reading.
pub const TAIL_BYTES: u64 = 512 * 1024;

/// How old a usage reading may be and still be read.
pub const FRESH_S: i64 = 6 * 3600;

/// How far back from a rollout's end its readings are looked for ([`rollout_readings`]).
pub const CONTEXT_REACH: u64 = 64 * 1024 * 1024;

/// How many of the newest rollouts are read for a usage reading before the search gives up.
pub const NEWEST_READ: usize = 6;

/// The limit family the shipped decisions read ([`pick`]).
pub const CODEX_FAMILY: &str = "codex";

/// How far apart two resets of one window may be and still be one anchor ([`pick`], design §3.5).
pub const ANCHOR_SLACK_S: i64 = 600;

/// The slot of a family's `rate_limits` a window came in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Slot {
    /// `primary`.
    Primary,
    /// `secondary`.
    Secondary,
}

/// What a window is, by its length (design §3.3, F11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Bucket {
    /// 300 minutes, within 5%.
    FiveHour,
    /// 1440 minutes, within 5%.
    Day,
    /// 10080 minutes, within 5%.
    Week,
    /// Another length, in minutes.
    Minutes(u64),
    /// No length given (absent, or the `0` placeholder).
    Unplaced(Slot),
}

/// One usage window of a limit family.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Window {
    /// The slot it came in.
    pub slot: Slot,
    pub used_percent: f64,
    pub window_minutes: Option<u64>,
    /// Epoch seconds.
    pub resets_at: Option<i64>,
}

impl Window {
    /// Its kind ([`Bucket`]).
    #[must_use]
    pub fn bucket(&self) -> Bucket {
        let near = |minutes: u64, of: u64| minutes.abs_diff(of).saturating_mul(20) <= of;
        match self.window_minutes {
            Some(m) if m > 0 && near(m, 300) => Bucket::FiveHour,
            Some(m) if m > 0 && near(m, 1440) => Bucket::Day,
            Some(m) if m > 0 && near(m, 10080) => Bucket::Week,
            Some(m) if m > 0 => Bucket::Minutes(m),
            _ => Bucket::Unplaced(self.slot),
        }
    }
}

/// A family's usage credits (`credits`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credits {
    /// `has_credits`.
    pub has_credits: Option<bool>,
    /// `unlimited`.
    pub unlimited: Option<bool>,
    /// `balance`, in Codex's credits and never dollars (design Q12), as written (`"12"`); `None` where null.
    pub balance: Option<Word>,
}

/// One `token_count`'s `rate_limits` (design F10).
#[derive(Debug, Clone, PartialEq)]
pub struct CodexLimits {
    /// When the reading was written, epoch seconds.
    pub at_unix: i64,
    /// The same, in milliseconds ([`pick`]).
    pub at_ms: i64,
    /// `limit_id`: `codex`, another model's limit (`codex_bengalfox`), the metadata-only `premium`.
    pub family: Word,
    /// `limit_name`.
    pub name: Option<Word>,
    /// `primary`, then `secondary`, where written.
    pub windows: Vec<Window>,
    /// `credits`.
    pub credits: Option<Credits>,
    /// `plan_type` (`plus`, `pro`, `prolite`, …).
    pub plan_type: Option<Word>,
    /// `individual_limit`, a number or a number written as text, in Codex's credits (design §3.3).
    pub individual_limit: Option<f64>,
    /// `spend_control_reached`; null in every capture.
    pub spend_control_reached: Option<bool>,
    /// `rate_limit_reached_type` (`workspace_member_credits_depleted`, …).
    pub reached_type: Option<Word>,
    /// The account the rollout's thread was made under ([`opening_account`]).
    pub account: Option<AccountKey>,
}

/// A word of the vendor's file ([`Word`]).
fn word_of(v: Option<&Value>) -> Option<Word> {
    v.and_then(Value::as_str).and_then(Word::new)
}

fn window(slot: Slot, v: Option<&Value>) -> Option<Window> {
    let v = v?;
    Some(Window {
        slot,
        used_percent: v.get("used_percent")?.as_f64()?,
        window_minutes: v.get("window_minutes").and_then(Value::as_u64),
        resets_at: v.get("resets_at").and_then(Value::as_i64),
    })
}

/// One rollout line read as a `token_count`'s `rate_limits` ([`CodexLimits`]).
fn limits_line(line: &str) -> Option<CodexLimits> {
    if !line.contains("\"token_count\"") || !line.contains("\"rate_limits\"") {
        return None;
    }
    let v = crate::harness::transcript::codex_limits(line.trim()).ok()?;
    let payload = v.get("payload")?;
    if payload.get("type").and_then(Value::as_str) != Some("token_count") {
        return None;
    }
    let at_ms = v
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(stamp_ms)?;
    let l = payload.get("rate_limits").filter(|l| l.is_object())?;
    let credits = l.get("credits").filter(|c| c.is_object()).map(|c| Credits {
        has_credits: c.get("has_credits").and_then(Value::as_bool),
        unlimited: c.get("unlimited").and_then(Value::as_bool),
        balance: word_of(c.get("balance")),
    });
    let individual_limit = l.get("individual_limit").and_then(|v| {
        v.as_f64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
            .filter(|n| n.is_finite())
    });
    Some(CodexLimits {
        at_unix: at_ms.div_euclid(1000),
        at_ms,
        family: word_of(l.get("limit_id"))?,
        name: word_of(l.get("limit_name")),
        windows: [(Slot::Primary, "primary"), (Slot::Secondary, "secondary")]
            .into_iter()
            .filter_map(|(slot, key)| window(slot, l.get(key)))
            .collect(),
        credits,
        plan_type: word_of(l.get("plan_type")),
        individual_limit,
        spend_control_reached: l.get("spend_control_reached").and_then(Value::as_bool),
        reached_type: word_of(l.get("rate_limit_reached_type")),
        account: None,
    })
}

/// Every `token_count` reading in `tail`, every family, in the order written.
#[must_use]
pub fn rollout_rate_limits(tail: &str) -> Vec<CodexLimits> {
    tail.lines().filter_map(limits_line).collect()
}

/// The account a rollout's thread was made under (design §3.1, F14).
#[must_use]
pub fn opening_account(first_line: &str) -> Option<AccountKey> {
    let v = crate::harness::transcript::codex_meta(first_line.trim()).ok()?;
    if v.get("type").and_then(Value::as_str) != Some("session_meta") {
        return None;
    }
    let p = v.get("payload")?;
    AccountKey::codex(
        p.get("creator_account_id")?.as_str()?,
        p.get("creator_user_id")?.as_str()?,
    )
}

/// Where the rollout at `path`'s own lines begin ([`TurnReader`]).
fn copy_end(path: &Path, reach: u64) -> Option<u64> {
    let mut reader = TurnReader::default();
    let mut own = None;
    let read = stream_lines(path, reach, |start, text| {
        if let Some(text) = text {
            let _ = reader.line(text);
        }
        if reader.prefix == Prefix::Own {
            own = Some(start);
            return std::ops::ControlFlow::Break(());
        }
        std::ops::ControlFlow::Continue(())
    })?;
    own.or_else(|| read.complete.then_some(read.read))
}

/// The usage readings of the rollout at `path` ([`opening_account`]).
#[must_use]
pub fn rollout_readings(path: &Path, reach: u64) -> Option<Vec<CodexLimits>> {
    let own_from = copy_end(path, reach)?;
    let account = first_line(path).as_deref().and_then(opening_account);
    let tail = tail_after(path, own_from, TAIL_BYTES)?;
    let mut readings = rollout_rate_limits(&tail);
    for r in &mut readings {
        r.account = account;
    }
    Some(readings)
}

/// One window as [`pick`] read it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Picked {
    /// Its kind.
    pub bucket: Bucket,
    /// The largest share read within its anchor.
    pub used_percent: f64,
    /// The anchor: the latest reset among the readings within it (epoch seconds); `None` where none of them named one.
    pub resets_at: Option<i64>,
    /// When that share was read, the earliest of a tie (unix seconds).
    pub as_of: i64,
}

/// One family's windows as [`pick`] read them.
#[derive(Debug, Clone, PartialEq)]
pub struct Pick {
    /// The account whose readings these are: the one the newest reading with a window names (`None`: its rollout names none).
    pub account: Option<AccountKey>,
    /// That newest reading's stamp (unix seconds) ([`FRESH_S`]).
    pub newest: i64,
    /// Each kind with a window whose reset is ahead (or names none).
    pub windows: Vec<Picked>,
    /// Each kind whose every reading's reset has passed: void, read as empty, never as the share it last had (design §3.4).
    pub void: Vec<Bucket>,
}

/// The `family`'s windows at `now_unix` from `rollouts` ([`rollout_readings`]).
#[must_use]
pub fn pick(rollouts: &[Vec<CodexLimits>], family: &str, now_unix: i64) -> Option<Pick> {
    let has = |r: &&CodexLimits| r.family.as_str() == family && !r.windows.is_empty();
    let newest = rollouts
        .iter()
        .flatten()
        .filter(has)
        .max_by_key(|r| r.at_ms)?;
    let account = newest.account;
    type Seen<'a> = Vec<(usize, i64, &'a Window)>;
    let mut kinds: std::collections::BTreeMap<Bucket, Seen<'_>> = std::collections::BTreeMap::new();
    for (i, rollout) in rollouts.iter().enumerate() {
        for r in rollout.iter().filter(has).filter(|r| r.account == account) {
            for w in &r.windows {
                kinds.entry(w.bucket()).or_default().push((i, r.at_ms, w));
            }
        }
    }
    let mut windows = Vec::new();
    let mut void = Vec::new();
    for (bucket, all) in kinds {
        let live: Seen<'_> = all
            .into_iter()
            .filter(|(_, _, w)| w.resets_at.is_none_or(|r| r > now_unix))
            .collect();
        let anchor = live.iter().filter_map(|(_, _, w)| w.resets_at).max();
        let within = |w: &Window| match (anchor, w.resets_at) {
            (Some(a), Some(r)) => r >= a.saturating_sub(ANCHOR_SLACK_S),
            (Some(_), None) => false,
            (None, _) => true,
        };
        let held: Seen<'_> = live.into_iter().filter(|(_, _, w)| within(w)).collect();
        let drop = held
            .windows(2)
            .filter(|pair| {
                pair[0].0 == pair[1].0
                    && pair[0].1 < pair[1].1
                    && pair[1].2.used_percent < pair[0].2.used_percent
            })
            .map(|pair| pair[1].1)
            .max();
        let best = held
            .iter()
            .filter(|(_, at_ms, _)| drop.is_none_or(|d| *at_ms >= d))
            .min_by(|a, b| {
                b.2.used_percent
                    .total_cmp(&a.2.used_percent)
                    .then(a.1.cmp(&b.1))
            });
        match best {
            Some((_, at_ms, w)) => windows.push(Picked {
                bucket,
                used_percent: w.used_percent,
                resets_at: anchor,
                as_of: at_ms.div_euclid(1000),
            }),
            None => void.push(bucket),
        }
    }
    Some(Pick {
        account,
        newest: newest.at_unix,
        windows,
        void,
    })
}

/// The `codex` family's pick at `now_unix` over the six (`NEWEST_READ`) rollouts of `home`
/// written last ([`rollout_readings`], [`pick`]).
#[must_use]
pub fn home_pick(home: &Path, now_unix: i64) -> Option<Pick> {
    let rollouts: Vec<Vec<CodexLimits>> = rollouts_by_write(home)
        .iter()
        .take(NEWEST_READ)
        .filter_map(|(path, _)| rollout_readings(path, CONTEXT_REACH))
        .collect();
    pick(&rollouts, CODEX_FAMILY, now_unix)
}

/// A [`Pick`] as the account pool's usage [`Sample`]: its five-hour window is the session, its
/// week the week, each share taken up to the next whole percent (so a share is never read as
/// further from its wall than it is) and each reset and read time as picked. Another window, an
/// unplaced one and a void one ([`Pick::void`]) give no figure. `at` is the newest reading's
/// stamp; `by` is left for [`crate::harness::accounts::note_sample`] to stamp.
#[must_use]
pub fn sample_of(pick: &Pick) -> Sample {
    // Clamped to 0..=100 first, so the cast neither truncates nor wraps.
    let whole = |share: f64| share.ceil().clamp(0.0, 100.0) as u32;
    let of = |bucket: Bucket| pick.windows.iter().find(|w| w.bucket == bucket);
    let session = of(Bucket::FiveHour);
    let week = of(Bucket::Week);
    Sample {
        session_pct: session.map(|w| whole(w.used_percent)),
        session_reset: session.and_then(|w| w.resets_at),
        week_pct: week.map(|w| whole(w.used_percent)),
        week_reset: week.and_then(|w| w.resets_at),
        session_read_at: session.map(|w| w.as_of),
        week_read_at: week.map(|w| w.as_of),
        at: pick.newest,
        by: String::new(),
    }
}

/// The usage sample of the Codex home `home` at `now_unix` ([`home_pick`], [`sample_of`]), with
/// the account its readings name, where a reading is no older than [`FRESH_S`].
#[must_use]
pub fn home_sample(home: &Path, now_unix: i64) -> Option<(Option<AccountKey>, Sample)> {
    let pick = home_pick(home, now_unix)?;
    (now_unix.saturating_sub(pick.newest) <= FRESH_S).then(|| (pick.account, sample_of(&pick)))
}

/// The usage sample written by a running Codex session in `thread`'s conversation under `home`
/// at or after `started_unix` ([`crate::harness::codex_rollout::conversation_rollouts`], [`pick`],
/// [`sample_of`]), where a reading is no older than [`FRESH_S`].
///
/// Filtering to `thread`'s own conversation files and `at_unix >= started_unix` keeps concurrent
/// sessions in a shared `$CODEX_HOME` (`CODEX_AUTH_PATH`) and pre-resume turns on another login
/// from attributing their usage to the current process; `account` is cleared on the post-start
/// readings so a resumed rollout whose opening `session_meta` named an earlier login still yields
/// the current process's windows.
#[must_use]
pub fn thread_sample(
    home: &Path,
    thread: &str,
    started_unix: i64,
    now_unix: i64,
) -> Option<Sample> {
    if !crate::harness::codex_rollout::is_thread_id(thread) {
        return None;
    }
    let conv = crate::harness::codex_rollout::conversation_rollouts(home, thread);
    let rollouts: Vec<Vec<CodexLimits>> = conv
        .files
        .iter()
        .filter_map(|path| {
            let mut readings = rollout_readings(path, CONTEXT_REACH)?;
            readings.retain(|r| r.at_unix >= started_unix);
            for r in &mut readings {
                r.account = None;
            }
            (!readings.is_empty()).then_some(readings)
        })
        .collect();
    let pick = pick(&rollouts, CODEX_FAMILY, now_unix)?;
    (now_unix.saturating_sub(pick.newest) <= FRESH_S).then(|| sample_of(&pick))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::codex_rollout::PAGINATED;
    use crate::harness::codex_rollout::fixtures::{context, message, meta, meta_as, rate};
    use crate::harness::codex_rollout::tail_of;
    use std::time::SystemTime;

    const ROOT: &str = "01900000-0000-7000-8000-000000000001";
    const SUB: &str = "01900000-0000-7000-8000-000000000002";
    const OTHER: &str = "01900000-0000-7000-8000-000000000003";
    const T0: i64 = 1_790_000_000_000;

    fn token_count(at: &str, limit_id: &str, used: f64, resets_at: i64) -> String {
        format!(
            "{{\"timestamp\":\"{at}\",\"ordinal\":1,\"type\":\"event_msg\",\"payload\":{{\"type\":\
             \"token_count\",\"info\":null,\"rate_limits\":{{\"limit_id\":\"{limit_id}\",\
             \"limit_name\":null,\"primary\":{{\"used_percent\":{used},\"window_minutes\":10080,\
             \"resets_at\":{resets_at}}},\"secondary\":null,\"credits\":{{\"has_credits\":false,\
             \"unlimited\":false,\"balance\":\"0\"}},\"plan_type\":\"pro\",\
             \"rate_limit_reached_type\":null}}}}}}"
        )
    }

    const PREMIUM: &str = "{\"timestamp\":\"2026-09-27T20:45:24.900Z\",\"type\":\"event_msg\",\
        \"payload\":{\"type\":\"token_count\",\"info\":null,\"rate_limits\":{\"limit_id\":\
        \"premium\",\"primary\":null,\"secondary\":null}}}";

    fn unix(s: &str) -> i64 {
        aterm_types::rfc3339::parse_utc_fractional(s).expect("a stamp")
    }

    #[test]
    fn the_incidents_readings_pick_the_largest_share_of_the_window() {
        let first = [
            token_count("2026-09-27T20:30:00.000Z", "codex", 97.0, 1_791_082_750),
            token_count("2026-09-27T20:37:17.251Z", "codex", 99.0, 1_791_082_750),
            token_count(
                "2026-09-27T20:38:00.000Z",
                "codex_bengalfox",
                3.0,
                1_791_082_999,
            ),
            PREMIUM.to_string(),
        ]
        .join("\n");
        let readings = rollout_rate_limits(&first);
        assert_eq!(readings.len(), 4, "every family is a reading");
        let one = [readings.clone()];
        let now = unix("2026-09-27T20:39:47Z");
        let p = pick(&one, CODEX_FAMILY, now).expect("a pick");
        assert_eq!(p.newest, unix("2026-09-27T20:37:17.251Z"));
        assert_eq!(
            p.windows,
            vec![Picked {
                bucket: Bucket::Week,
                used_percent: 99.0,
                resets_at: Some(1_791_082_750),
                as_of: unix("2026-09-27T20:37:17Z"),
            }]
        );
        let other = pick(&one, "codex_bengalfox", now).expect("its pick");
        assert_eq!(other.windows[0].used_percent, 3.0);
        assert_eq!(pick(&one, "premium", now), None, "no window");
        let second = token_count("2026-09-28T03:29:30.000Z", "codex", 1.0, 1_791_169_817);
        let at_press = unix("2026-09-28T03:30:24Z");
        let p2 = pick(&[rollout_rate_limits(&second)], CODEX_FAMILY, at_press).expect("a pick");
        assert_eq!(p2.windows[0].used_percent, 1.0);
        let mut just_reset = readings[1].clone();
        just_reset.at_ms = 1_791_082_700_000;
        just_reset.at_unix = 1_791_082_700;
        let p = pick(&[vec![just_reset]], CODEX_FAMILY, 1_791_082_760).expect("a pick");
        assert_eq!(
            (p.windows.len(), p.void.as_slice()),
            (0, &[Bucket::Week][..])
        );
        assert_eq!(
            pick(&[rollout_rate_limits(PREMIUM)], CODEX_FAMILY, now),
            None
        );
        assert_eq!(rollout_rate_limits("not json\n{\"cut"), Vec::new());
    }

    const LOGIN_A: (&str, &str) = ("acct-lab-1", "user-lab");

    const LOGIN_B: (&str, &str) = ("acct-lab-2", "user-lab");

    fn key(login: (&str, &str)) -> Option<AccountKey> {
        AccountKey::codex(login.0, login.1)
    }

    fn reading(
        ms: i64,
        account: Option<AccountKey>,
        windows: &[(Slot, f64, u64, i64)],
    ) -> CodexLimits {
        CodexLimits {
            at_unix: ms.div_euclid(1000),
            at_ms: ms,
            family: Word::new(CODEX_FAMILY).expect("a word"),
            name: None,
            windows: windows
                .iter()
                .map(|&(slot, used_percent, minutes, resets_at)| Window {
                    slot,
                    used_percent,
                    window_minutes: Some(minutes),
                    resets_at: Some(resets_at),
                })
                .collect(),
            credits: None,
            plan_type: None,
            individual_limit: None,
            spend_control_reached: None,
            reached_type: None,
            account,
        }
    }

    fn apart(readings: &[CodexLimits]) -> Vec<Vec<CodexLimits>> {
        readings.iter().map(|r| vec![r.clone()]).collect()
    }

    #[test]
    fn every_family_is_read_with_what_codex_writes_beside_it() {
        let line = "{\"timestamp\":\"2026-10-16T22:20:05.645Z\",\"ordinal\":14,\"type\":\"event_msg\",\
            \"payload\":{\"type\":\"token_count\",\"info\":null,\"rate_limits\":{\"limit_id\":\"codex\",\
            \"limit_name\":\"Codex\",\"primary\":{\"used_percent\":30.0,\"window_minutes\":10080,\
            \"resets_at\":1792192805},\"secondary\":{\"used_percent\":10.0,\"window_minutes\":0,\
            \"resets_at\":null},\"credits\":{\"has_credits\":true,\"unlimited\":false,\"balance\":\"12\"},\
            \"individual_limit\":\"2500\",\"spend_control_reached\":true,\"plan_type\":\"prolite\",\
            \"rate_limit_reached_type\":\"workspace_member_usage_limit_reached\",\
            \"normal_model_slug\":\"gpt-6.1-sol\"}}}";
        let read = rollout_rate_limits(line);
        let [r] = read.as_slice() else {
            panic!("one reading: {read:?}");
        };
        assert_eq!(r.at_ms, unix("2026-10-16T22:20:05Z") * 1000 + 645);
        assert_eq!(r.at_unix, unix("2026-10-16T22:20:05Z"));
        assert_eq!(r.family.as_str(), "codex");
        assert_eq!(r.name.as_ref().map(Word::as_str), Some("Codex"));
        assert_eq!(
            r.windows,
            vec![
                Window {
                    slot: Slot::Primary,
                    used_percent: 30.0,
                    window_minutes: Some(10080),
                    resets_at: Some(1_792_192_805),
                },
                Window {
                    slot: Slot::Secondary,
                    used_percent: 10.0,
                    window_minutes: Some(0),
                    resets_at: None,
                },
            ]
        );
        assert_eq!(r.windows[0].bucket(), Bucket::Week, "a slot is no kind");
        assert_eq!(r.windows[1].bucket(), Bucket::Unplaced(Slot::Secondary));
        assert_eq!(
            r.credits,
            Some(Credits {
                has_credits: Some(true),
                unlimited: Some(false),
                balance: Word::new("12"),
            })
        );
        assert_eq!(r.individual_limit, Some(2500.0));
        assert_eq!(r.spend_control_reached, Some(true));
        assert_eq!(r.plan_type.as_ref().map(Word::as_str), Some("prolite"));
        assert_eq!(
            r.reached_type.as_ref().map(Word::as_str),
            Some("workspace_member_usage_limit_reached")
        );
        assert_eq!(r.account, None, "text alone names no account");
        let other = rate(T0, "codex_bengalfox", Some((3.0, 300, 1_792_200_000)), None);
        let premium = rate(T0 + 1, "premium", None, None);
        let both = rollout_rate_limits(&format!("{other}{premium}"));
        assert_eq!(
            both.iter().map(|r| r.family.as_str()).collect::<Vec<_>>(),
            ["codex_bengalfox", "premium"]
        );
        assert!(both[1].windows.is_empty(), "a metadata-only family");
        let kind = |minutes: Option<u64>| {
            Window {
                slot: Slot::Primary,
                used_percent: 1.0,
                window_minutes: minutes,
                resets_at: None,
            }
            .bucket()
        };
        assert_eq!(kind(Some(285)), Bucket::FiveHour);
        assert_eq!(kind(Some(315)), Bucket::FiveHour);
        assert_eq!(kind(Some(284)), Bucket::Minutes(284));
        assert_eq!(kind(Some(316)), Bucket::Minutes(316));
        assert_eq!(kind(Some(1440)), Bucket::Day);
        assert_eq!(kind(Some(10_584)), Bucket::Week);
        assert_eq!(kind(Some(10_585)), Bucket::Minutes(10_585));
        assert_eq!(kind(None), Bucket::Unplaced(Slot::Primary));
        assert_eq!(
            rollout_rate_limits(
                &line.replace("\"limit_id\":\"codex\"", "\"limit_id\":\"co\\u001bdex\"")
            ),
            Vec::new()
        );
        assert_eq!(
            rollout_rate_limits(&line.replace("\"limit_id\":\"codex\"", "\"limit_id\":null")),
            Vec::new()
        );
        let drift = rollout_rate_limits(
            &line
                .replace("\"balance\":\"12\"", "\"balance\":12")
                .replace(
                    "\"individual_limit\":\"2500\"",
                    "\"individual_limit\":{\"cap\":2500}",
                ),
        );
        assert_eq!(
            drift[0].credits.as_ref().and_then(|c| c.balance.clone()),
            None
        );
        assert_eq!(drift[0].individual_limit, None);
        assert_eq!(drift[0].windows.len(), 2, "the rest still reads");
    }

    #[test]
    fn a_rollouts_readings_are_keyed_by_its_first_lines_login() {
        let first = meta_as(T0, SUB, ROOT, Some(ROOT), Some(PAGINATED), Some(LOGIN_A));
        assert_eq!(opening_account(&first), key(LOGIN_A));
        assert!(
            key(LOGIN_A).is_some_and(|k| k.to_string().starts_with("x-")),
            "Codex's key"
        );
        assert_ne!(key(LOGIN_A), key(LOGIN_B));
        let dir = aterm_tempfile::Builder::new()
            .prefix("codex-usage-key-")
            .tempdir()
            .expect("temp dir");
        let path = dir.path().join("rollout.jsonl");
        let reset = T0 / 1000 + 3600;
        std::fs::write(
            &path,
            [
                first.clone(),
                meta_as(T0, ROOT, ROOT, None, Some(PAGINATED), Some(LOGIN_B)),
                context(T0 + 9, "gpt-6.1-sol", None),
                rate(T0 + 24, "codex", Some((30.0, 300, reset)), None),
            ]
            .concat(),
        )
        .expect("write");
        let read = rollout_readings(&path, CONTEXT_REACH).expect("read");
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].account, key(LOGIN_A));
        let api_key = meta_as(T0, ROOT, ROOT, None, Some(PAGINATED), None);
        assert_eq!(opening_account(&api_key), None);
        let half = first.replace(",\"creator_user_id\":\"user-lab\"", "");
        assert_eq!(opening_account(&half), None);
        assert_eq!(opening_account(&context(T0, "gpt-6.1-sol", None)), None);
        std::fs::write(
            &path,
            [
                api_key,
                rate(T0 + 24, "codex", Some((30.0, 300, reset)), None),
            ]
            .concat(),
        )
        .expect("write");
        let read = rollout_readings(&path, CONTEXT_REACH).expect("read");
        assert_eq!((read.len(), read[0].account), (1, None));
    }

    #[test]
    fn the_pick_voids_passed_resets_and_keeps_the_largest_share_of_the_latest_anchor() {
        let now = T0 / 1000;
        let a = key(LOGIN_A);
        let week = |ms: i64, used: f64, resets: i64| {
            reading(ms, a, &[(Slot::Secondary, used, 10_080, resets)])
        };
        let newest_by_stamp = |rs: &[CodexLimits]| {
            rs.iter()
                .max_by_key(|r| r.at_ms)
                .map(|r| r.windows[0].used_percent)
        };
        let passed = week(T0 - 7_200_000, 95.0, now - 10);
        let live = week(T0 - 3_600_000, 10.0, now + 432_000);
        let p = pick(&apart(&[passed.clone(), live]), CODEX_FAMILY, now).expect("a pick");
        assert_eq!(p.windows[0].used_percent, 10.0);
        assert_eq!(p.void, Vec::<Bucket>::new());
        let p = pick(&apart(&[passed]), CODEX_FAMILY, now).expect("a pick");
        assert_eq!(
            (p.windows.len(), p.void.as_slice()),
            (0, &[Bucket::Week][..])
        );
        let real = week(T0 - 1_800_000, 95.0, now + 432_000);
        let copy = week(T0 - 60_000, 60.0, now + 432_100);
        let rs = [real, copy];
        assert_eq!(newest_by_stamp(&rs), Some(60.0), "the rule replaced");
        let p = pick(&apart(&rs), CODEX_FAMILY, now).expect("a pick");
        assert_eq!(p.windows[0].used_percent, 95.0);
        assert_eq!(p.windows[0].resets_at, Some(now + 432_100), "the anchor");
        assert_eq!(p.newest, (T0 - 60_000) / 1000);
        let before = |moved: i64| {
            [
                week(T0 - 1_800_000, 99.0, now + 432_000),
                week(T0 - 60_000, 50.0, now + 432_000 + moved),
            ]
        };
        let same = pick(&apart(&before(ANCHOR_SLACK_S)), CODEX_FAMILY, now).expect("a pick");
        assert_eq!(same.windows[0].used_percent, 99.0, "one anchor");
        let later = pick(&apart(&before(ANCHOR_SLACK_S + 1)), CODEX_FAMILY, now).expect("a pick");
        assert_eq!(
            later.windows[0].used_percent, 50.0,
            "an early reset's later anchor"
        );
        let tie = [
            week(T0 - 25_200_000, 40.0, now + 432_000),
            week(T0 - 3_600_000, 40.0, now + 432_000),
        ];
        let p = pick(&[tie.to_vec()], CODEX_FAMILY, now).expect("a pick");
        assert_eq!(p.windows[0].as_of, (T0 - 25_200_000) / 1000);
        assert_eq!(p.newest, (T0 - 3_600_000) / 1000);
        let stale = pick(&apart(&tie[..1]), CODEX_FAMILY, now).expect("a pick");
        assert!(now.saturating_sub(stale.newest) > FRESH_S);
    }

    #[test]
    fn the_pick_reads_the_newest_login_only() {
        let now = T0 / 1000;
        let signed_out = reading(
            T0 - 3_600_000,
            key(LOGIN_A),
            &[(Slot::Secondary, 97.0, 10_080, now + 500_000)],
        );
        let signed_in = reading(
            T0 - 600_000,
            key(LOGIN_B),
            &[(Slot::Secondary, 10.0, 10_080, now + 100_000)],
        );
        let p = pick(
            &apart(&[signed_out.clone(), signed_in.clone()]),
            CODEX_FAMILY,
            now,
        )
        .expect("a pick");
        assert_eq!(p.account, key(LOGIN_B));
        assert_eq!(p.windows[0].used_percent, 10.0);
        let bare = reading(T0 - 60_000, key(LOGIN_A), &[]);
        let p = pick(
            &apart(&[signed_out.clone(), signed_in.clone(), bare]),
            CODEX_FAMILY,
            now,
        )
        .expect("a pick");
        assert_eq!((p.account, p.newest), (key(LOGIN_B), (T0 - 600_000) / 1000));
        let unkeyed = |mut r: CodexLimits| {
            r.account = None;
            r
        };
        let p = pick(
            &apart(&[unkeyed(signed_out), unkeyed(signed_in)]),
            CODEX_FAMILY,
            now,
        )
        .expect("a pick");
        assert_eq!(p.windows[0].used_percent, 97.0);
    }

    #[test]
    fn a_drop_within_one_rollout_opens_the_window_anew() {
        let now = T0 / 1000;
        let five = |ms: i64, used: f64, resets: i64| {
            reading(ms, key(LOGIN_A), &[(Slot::Primary, used, 300, resets)])
        };
        let before = five(T0 - 42_000, 95.0, now + 3021);
        let after = five(T0, 20.0, now + 3063);
        let p = pick(&[vec![before.clone(), after.clone()]], CODEX_FAMILY, now).expect("a pick");
        assert_eq!(p.windows[0].used_percent, 20.0);
        let since = five(T0 + 60_000, 25.0, now + 3100);
        let p = pick(
            &[vec![before.clone(), after.clone()], vec![since]],
            CODEX_FAMILY,
            now + 60,
        )
        .expect("a pick");
        assert_eq!(p.windows[0].used_percent, 25.0);
        let p = pick(&apart(&[before, after]), CODEX_FAMILY, now).expect("a pick");
        assert_eq!(p.windows[0].used_percent, 95.0);
    }

    #[test]
    fn a_legacy_forks_copied_readings_are_none_of_its_own() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("codex-usage-copy-")
            .tempdir()
            .expect("temp dir");
        let path = dir.path().join("rollout.jsonl");
        let now = T0 / 1000 + 30;
        let reset = now + 3600;
        let copy = [
            meta_as(T0, SUB, SUB, Some(ROOT), Some("legacy"), Some(LOGIN_A)),
            meta(T0, ROOT, ROOT, None),
            context(T0, "gpt-6-luna", None),
            rate(T0, "codex", Some((99.0, 300, reset)), None),
            message(T0 + 1),
        ]
        .concat();
        let own = [
            context(T0 + 21_000, "gpt-6.1-sol", None),
            rate(T0 + 21_010, "codex", Some((40.0, 300, reset + 30)), None),
        ]
        .concat();
        std::fs::write(&path, format!("{copy}{own}")).expect("write");
        let read = rollout_readings(&path, CONTEXT_REACH).expect("read");
        assert_eq!(read.len(), 1, "{read:?}");
        assert_eq!(read[0].windows[0].used_percent, 40.0);
        assert_eq!(read[0].account, key(LOGIN_A));
        assert_eq!(
            pick(&[read], CODEX_FAMILY, now).map(|p| p.windows[0].used_percent),
            Some(40.0)
        );
        assert_eq!(
            rollout_readings(&path, 64),
            None,
            "a copy past the reach is unread"
        );
        let whole = rollout_rate_limits(&tail_of(&path, TAIL_BYTES).expect("tail"));
        assert_eq!(whole.len(), 2, "the copy's 99% is in the text");
        std::fs::write(&path, &copy).expect("write");
        assert_eq!(rollout_readings(&path, CONTEXT_REACH), Some(Vec::new()));
        let whole = rollout_rate_limits(&tail_of(&path, TAIL_BYTES).expect("tail"));
        let p = pick(&[whole], CODEX_FAMILY, now).expect("a pick");
        assert_eq!((p.windows[0].used_percent, p.newest), (99.0, T0 / 1000));
    }

    #[test]
    fn a_home_pick_trusts_neither_a_copy_nor_another_login() {
        let home = aterm_tempfile::Builder::new()
            .prefix("codex-usage-pick-")
            .tempdir()
            .expect("temp dir");
        let day = home.path().join("sessions/2026/10/16");
        std::fs::create_dir_all(&day).expect("mkdir");
        let now = SystemTime::now();
        let now_ms = i64::try_from(
            now.duration_since(SystemTime::UNIX_EPOCH)
                .expect("clock")
                .as_millis(),
        )
        .expect("fits");
        let now_unix = now_ms / 1000;
        let week = now_unix + 300_000;
        let write = |name: &str, body: String, age_s: u64| {
            let path = day.join(name);
            std::fs::write(&path, body).expect("write");
            std::fs::File::options()
                .write(true)
                .open(&path)
                .and_then(|f| f.set_modified(now - std::time::Duration::from_secs(age_s)))
                .expect("mtime");
        };
        let real = [
            meta_as(
                now_ms - 7_200_000,
                ROOT,
                ROOT,
                None,
                Some(PAGINATED),
                Some(LOGIN_A),
            ),
            rate(
                now_ms - 1_800_000,
                "codex",
                None,
                Some((95.0, 10_080, week)),
            ),
        ]
        .concat();
        let fork = [
            meta_as(
                now_ms - 60_000,
                SUB,
                SUB,
                Some(ROOT),
                Some("legacy"),
                Some(LOGIN_A),
            ),
            rate(now_ms - 60_000, "codex", None, Some((60.0, 10_080, week))),
        ]
        .concat();
        write(
            &format!("rollout-2026-10-16T08-00-00-{ROOT}.jsonl"),
            real.clone(),
            1800,
        );
        write(
            &format!("rollout-2026-10-16T09-00-00-{SUB}.jsonl"),
            fork.clone(),
            60,
        );
        let p = home_pick(home.path(), now_unix).expect("a pick");
        assert_eq!(
            (p.account, p.windows[0].used_percent, p.windows[0].resets_at),
            (key(LOGIN_A), 95.0, Some(week))
        );
        let newest = [&real, &fork]
            .iter()
            .flat_map(|t| rollout_rate_limits(t))
            .max_by_key(|r| r.at_ms)
            .map(|r| r.windows[0].used_percent);
        assert_eq!(newest, Some(60.0));
        let other = [
            meta_as(
                now_ms - 30_000,
                OTHER,
                OTHER,
                None,
                Some(PAGINATED),
                Some(LOGIN_B),
            ),
            rate(
                now_ms - 20_000,
                "codex",
                None,
                Some((10.0, 10_080, week - 200_000)),
            ),
        ]
        .concat();
        write(
            &format!("rollout-2026-10-16T10-00-00-{OTHER}.jsonl"),
            other.clone(),
            20,
        );
        let p = home_pick(home.path(), now_unix).expect("a pick");
        assert_eq!((p.account, p.windows[0].used_percent), (key(LOGIN_B), 10.0));
        let unnamed = |t: &str, login: (&str, &str)| {
            t.replace(
                &format!(
                    ",\"creator_account_id\":\"{}\",\"creator_user_id\":\"{}\"",
                    login.0, login.1
                ),
                "",
            )
        };
        write(
            &format!("rollout-2026-10-16T08-00-00-{ROOT}.jsonl"),
            unnamed(&real, LOGIN_A),
            1800,
        );
        write(
            &format!("rollout-2026-10-16T09-00-00-{SUB}.jsonl"),
            unnamed(&fork, LOGIN_A),
            60,
        );
        write(
            &format!("rollout-2026-10-16T10-00-00-{OTHER}.jsonl"),
            unnamed(&other, LOGIN_B),
            20,
        );
        let p = home_pick(home.path(), now_unix).expect("a pick");
        assert_eq!((p.account, p.windows[0].used_percent), (None, 95.0));
    }

    /// A rollout whose primary (five-hour) window is `used` percent spent, resetting two hours
    /// after `now`, and whose week is 40% spent.
    fn five_hour_reading(now: i64, used: f64) -> String {
        let at = crate::harness::codex_rollout::fixtures::stamp((now - 60) * 1000);
        format!(
            "{{\"timestamp\":\"{at}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\
             \"token_count\",\"info\":null,\"rate_limits\":{{\"limit_id\":\"codex\",\
             \"primary\":{{\"used_percent\":{used},\"window_minutes\":300,\"resets_at\":{}}},\
             \"secondary\":{{\"used_percent\":40.0,\"window_minutes\":10080,\"resets_at\":{}}},\
             \"plan_type\":\"pro\"}}}}}}",
            now + 2 * 3600,
            now + 3 * 86_400
        )
    }

    #[test]
    fn a_codex_window_near_its_wall_moves_a_two_row_pool_to_its_fresh_row() {
        use crate::ai::connector::Cli;
        use crate::harness::accounts::{
            Advice, Me, Mode, Point, Roster, State, StayWhy, advise_for, note_sample,
        };
        let now = unix("2026-10-23T12:00:00Z");
        let roster = Roster::parse_for(
            Cli::Codex,
            "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n[[account]]\n\
             label = \"work\"\nclass = \"dedicated\"\ndir = \"/logins/codex/work\"\n\
             flow = \"device\"\n",
        )
        .expect("a roster");
        let enrolled = roster.rows.iter().map(|r| r.label.clone()).collect();
        let me = Me {
            account: Some("main"),
            left: None,
            default_home: true,
            stored_login: true,
        };
        let ask = |used: f64| {
            let readings = rollout_rate_limits(&five_hour_reading(now, used));
            let sample = sample_of(&pick(&[readings], CODEX_FAMILY, now).expect("a pick"));
            assert_eq!(sample.session_pct, Some(used as u32));
            assert_eq!(sample.session_reset, Some(now + 2 * 3600));
            assert_eq!(sample.week_pct, Some(40));
            let mut state = State {
                machine: "0a0b".into(),
                active: Some("main".into()),
                proven: true,
                ..State::default()
            };
            assert_eq!(note_sample(&mut state, "main", sample, now).len(), 1);
            advise_for(
                Cli::Codex,
                &roster,
                &state,
                &enrolled,
                &me,
                &Point::TurnEnd,
                Mode::Early,
                now,
            )
        };
        assert_eq!(
            ask(95.0),
            Advice::Move {
                to: "work".into(),
                ahead: true,
                rotates: true,
                takes_canary: false,
            }
        );
        assert_eq!(ask(50.0), Advice::Stay(StayWhy::NothingDue));
    }

    #[test]
    fn a_share_is_read_up_to_the_next_whole_percent_and_a_stale_home_gives_no_sample() {
        let now = unix("2026-10-23T12:00:00Z");
        let readings = rollout_rate_limits(&five_hour_reading(now, 89.2));
        let p = pick(&[readings], CODEX_FAMILY, now).expect("a pick");
        let sample = sample_of(&p);
        assert_eq!((sample.session_pct, sample.week_pct), (Some(90), Some(40)));
        assert_eq!(sample.at, now - 60);
        let void = Pick {
            windows: Vec::new(),
            void: vec![Bucket::FiveHour, Bucket::Week],
            ..p
        };
        let empty = sample_of(&void);
        assert_eq!((empty.session_pct, empty.week_pct), (None, None));
        let home = aterm_tempfile::Builder::new()
            .prefix("aterm-codex-sample-")
            .tempdir()
            .expect("tmp");
        assert_eq!(home_sample(home.path(), now), None, "no rollout, no sample");
    }

    #[test]
    fn thread_sample_isolates_the_sessions_post_start_readings_in_shared_home() {
        let home = aterm_tempfile::Builder::new()
            .prefix("codex-thread-sample-")
            .tempdir()
            .expect("temp dir");
        let day = home.path().join("sessions/2026/10/16");
        std::fs::create_dir_all(&day).expect("mkdir");
        let now_ms = T0;
        let now_unix = now_ms / 1000;
        let started_unix = now_unix - 120;
        let week = now_unix + 300_000;
        // Root thread created earlier under LOGIN_A (99% pre-start), then resumed at started_unix
        // under LOGIN_B (22% post-start), plus a subagent spawned post-start under LOGIN_B (35%),
        // while a concurrent OTHER thread in the same shared CODEX_HOME wrote a newer 5% reading.
        let root_body = [
            meta_as(
                now_ms - 7_200_000,
                ROOT,
                ROOT,
                None,
                Some(PAGINATED),
                Some(LOGIN_A),
            ),
            rate(
                now_ms - 3_600_000,
                "codex",
                None,
                Some((99.0, 10_080, week)),
            ),
            rate(now_ms - 60_000, "codex", None, Some((22.0, 10_080, week))),
        ]
        .concat();
        let sub_body = [
            meta_as(
                now_ms - 50_000,
                SUB,
                ROOT,
                None,
                Some(PAGINATED),
                Some(LOGIN_B),
            ),
            rate(now_ms - 40_000, "codex", None, Some((35.0, 10_080, week))),
        ]
        .concat();
        let other_body = [
            meta_as(
                now_ms - 10_000,
                OTHER,
                OTHER,
                None,
                Some(PAGINATED),
                Some(LOGIN_B),
            ),
            rate(now_ms - 5_000, "codex", None, Some((5.0, 10_080, week))),
        ]
        .concat();
        std::fs::write(
            day.join(format!("rollout-2026-10-16T08-00-00-{ROOT}.jsonl")),
            root_body,
        )
        .expect("write root");
        std::fs::write(
            day.join(format!("rollout-2026-10-16T09-00-00-{SUB}.jsonl")),
            sub_body,
        )
        .expect("write sub");
        std::fs::write(
            day.join(format!("rollout-2026-10-16T10-00-00-{OTHER}.jsonl")),
            other_body,
        )
        .expect("write other");

        let sample =
            thread_sample(home.path(), ROOT, started_unix, now_unix).expect("thread sample");
        assert_eq!(sample.week_pct, Some(35));
        assert_eq!(
            thread_sample(home.path(), ROOT, now_unix - 10, now_unix),
            None,
            "readings older than process start are excluded"
        );
        assert_eq!(
            thread_sample(home.path(), "not-a-thread-id", started_unix, now_unix),
            None
        );
    }
}
