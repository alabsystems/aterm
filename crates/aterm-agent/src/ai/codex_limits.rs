// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Codex's usage limits as its own records write them.

use std::path::Path;

use aterm_json::Value;

use crate::harness::accounts::Sample;
use crate::harness::codex_rollout::{
    Conversation, META_BYTES, Prefix, Streamed, TurnReader, first_line_into, first_line_of,
    first_line_read, rollouts_by_write, stamp_ms, stream_open_ends_ref, tail_after_file,
};
use crate::harness::profile::{AccountKey, Word};

/// How much of a rollout's end is read for its usage reading.
pub const TAIL_BYTES: u64 = 512 * 1024;

/// How old a usage reading may be and still be read.
pub const FRESH_S: i64 = 6 * 3600;

/// How far into a rollout the end of a copied prefix is looked for, from its first byte
/// ([`rollout_readings`]). Its readings are looked for [`TAIL_BYTES`] back from its end.
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

impl Bucket {
    /// How long a window of this kind can run from a reading of it, in seconds: its length, and a
    /// week ([`LOGIN_REACH_S`]) where it names none or a longer one.
    fn length_s(self) -> i64 {
        let minutes = match self {
            Bucket::FiveHour => 300,
            Bucket::Day => 1440,
            Bucket::Week => 10_080,
            Bucket::Minutes(minutes) => minutes,
            Bucket::Unplaced(_) => return LOGIN_REACH_S,
        };
        i64::try_from(minutes.saturating_mul(60)).map_or(LOGIN_REACH_S, |s| s.min(LOGIN_REACH_S))
    }
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

impl CodexLimits {
    /// Whether the server refused use: `rate_limit_reached_type` names a limit. `unknown` names
    /// none, as the endpoint's reading of the same field says
    /// ([`crate::harness::codex_app_login::usage_from_backend`]).
    fn refused(&self) -> bool {
        self.reached_type
            .as_ref()
            .is_some_and(|reached| reached.as_str() != "unknown")
    }

    /// Whether it says anything of `family`: a window of it, or a refusal.
    fn speaks_of(&self, family: &str) -> bool {
        self.family.as_str() == family && (!self.windows.is_empty() || self.refused())
    }

    /// When the refusal it names ends: when the most used window still running at that reading
    /// ends. The reading fixes this and the clock never moves it, so a five-hour wall ends at its
    /// reset and does not become the week's. Where every window had ended by then, it is the most
    /// used one's end, which has passed. It names no end with no window at all, so that is a week
    /// from the reading ([`LOGIN_REACH_S`]).
    fn refusal_ends(&self) -> i64 {
        let share = |a: &&Window, b: &&Window| a.used_percent.total_cmp(&b.used_percent);
        self.windows
            .iter()
            .filter(|w| w.resets_at.is_none_or(|reset| reset > self.at_unix))
            .max_by(share)
            .or_else(|| self.windows.iter().max_by(share))
            .map_or(self.at_unix.saturating_add(LOGIN_REACH_S), |w| {
                w.resets_at
                    .unwrap_or_else(|| self.at_unix.saturating_add(w.bucket().length_s()))
            })
    }
}

/// A word of the vendor's file ([`Word`]).
fn word_of(v: Option<&Value>) -> Option<Word> {
    v.and_then(Value::as_str).and_then(Word::new)
}

fn window(slot: Slot, v: Option<&Value>) -> Option<Window> {
    let v = v?;
    Some(Window {
        slot,
        used_percent: v.get("used_percent")?.as_f64().filter(|n| n.is_finite())?,
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

/// A rollout could not be opened or read: a look may find otherwise with nothing about it changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReadFailed;

/// Where a legacy fork's own lines begin, past its copied prefix ([`copy_end_and_account`]): `own`,
/// or the end of a file read whole that holds none. `None` where the prefix runs past the reach,
/// which an append cannot change, since it leaves the bytes in reach as they are.
fn own_lines_from(
    own: Option<u64>,
    read: &crate::harness::codex_rollout::RolloutRead,
    from: u64,
) -> Result<Option<u64>, ReadFailed> {
    if own.is_none() && read.failed {
        return Err(ReadFailed);
    }
    Ok(own.or_else(|| read.complete.then_some(from + read.read)))
}

/// Where the rollout in `file`'s own lines begin ([`TurnReader`]), plus its opening account when
/// `with_account` is set. `Ok(None)` where a copied prefix runs past `reach`.
fn copy_end_and_account(
    file: &std::fs::File,
    reach: u64,
    with_account: bool,
    first: &mut Vec<u8>,
) -> Result<Option<(u64, Option<AccountKey>)>, ReadFailed> {
    if reach >= META_BYTES && first_line_into(file, first) {
        let body_len = first.len() - 1;
        if body_len <= crate::harness::usage::MAX_LINE_BYTES
            && let Ok(text) = std::str::from_utf8(&first[..body_len])
        {
            let account = if with_account {
                opening_account(text)
            } else {
                None
            };
            let line1_end = first.len() as u64;
            let mut reader = TurnReader::default();
            let _ = reader.line(text);
            if reader.prefix == Prefix::Own {
                return Ok(Some((line1_end, account)));
            }
            let mut own = None;
            let read = stream_open_ends_ref(
                file,
                line1_end,
                None,
                reach - line1_end,
                |start, _, line| {
                    if let Streamed::Line(Some(text)) = line {
                        let _ = reader.line(text);
                    }
                    if reader.prefix == Prefix::Own {
                        own = Some(start);
                        return std::ops::ControlFlow::Break(());
                    }
                    std::ops::ControlFlow::Continue(())
                },
            );
            let Some(own_from) = own_lines_from(own, &read, line1_end)? else {
                return Ok(None);
            };
            return Ok(Some((own_from, account)));
        }
    }
    let mut reader = TurnReader::default();
    let mut own = None;
    let mut line0_account = None;
    let mut saw_invalid_line0 = false;
    let read = stream_open_ends_ref(file, 0, None, reach, |start, _, line| {
        if let Streamed::Line(text) = line {
            if start == 0 && with_account {
                match text {
                    Some(t) => line0_account = opening_account(t),
                    None => saw_invalid_line0 = true,
                }
            }
            if let Some(text) = text {
                let _ = reader.line(text);
            }
        }
        if reader.prefix == Prefix::Own {
            own = Some(start);
            return std::ops::ControlFlow::Break(());
        }
        std::ops::ControlFlow::Continue(())
    });
    let Some(own_from) = own_lines_from(own, &read, 0)? else {
        return Ok(None);
    };
    if saw_invalid_line0 {
        line0_account = first_line_of(file).as_deref().and_then(opening_account);
    }
    Ok(Some((own_from, line0_account)))
}

/// One rollout's readings: `Ok(None)` where a copied prefix runs past the reach, so it has none to
/// give, and an append does not change that.
type RolloutReadings = Result<Option<Vec<CodexLimits>>, ReadFailed>;

/// The readings of the rollout at `path` ([`RolloutReadings`]).
fn rollout_readings_checked(
    path: &Path,
    reach: u64,
    with_account: bool,
    scratch: &mut Vec<u8>,
) -> RolloutReadings {
    let mut file = crate::harness::footer::open_regular(path).ok_or(ReadFailed)?;
    let Some((own_from, account)) = copy_end_and_account(&file, reach, with_account, scratch)?
    else {
        return Ok(None);
    };
    let tail = tail_after_file(&mut file, own_from, TAIL_BYTES).ok_or(ReadFailed)?;
    let mut readings = rollout_rate_limits(&tail);
    for r in &mut readings {
        r.account = account;
    }
    Ok(Some(readings))
}

fn rollout_readings_inner(
    path: &Path,
    reach: u64,
    with_account: bool,
    scratch: &mut Vec<u8>,
) -> Option<Vec<CodexLimits>> {
    rollout_readings_checked(path, reach, with_account, scratch)
        .ok()
        .flatten()
}

/// The usage readings of the rollout at `path` ([`opening_account`]).
#[must_use]
pub fn rollout_readings(path: &Path, reach: u64) -> Option<Vec<CodexLimits>> {
    let mut scratch = Vec::with_capacity(8 * 1024);
    rollout_readings_inner(path, reach, true, &mut scratch)
}

/// One window as [`pick`] read it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Picked {
    /// Its kind.
    pub bucket: Bucket,
    /// The largest share read within its anchor. With no anchor, the newest reading's: nothing says
    /// two readings that name no reset are of one window.
    pub used_percent: f64,
    /// The anchor: the latest reset among the readings within it (epoch seconds); `None` where none of them named one.
    pub resets_at: Option<i64>,
    /// When that share was read, the earliest of a tie (unix seconds).
    pub as_of: i64,
}

impl Picked {
    /// When it ends: its reset, or where none is named, its length from when it was read.
    fn ends(&self) -> i64 {
        self.resets_at
            .unwrap_or_else(|| self.as_of.saturating_add(self.bucket.length_s()))
    }
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
        let newest_of_kind = live.iter().map(|(_, at_ms, _)| *at_ms).max();
        // A reading that names no reset has no anchor to place it, so only the kind's newest speaks:
        // a later reading below full lifts a full one, and one that names no reset is not lost to
        // an older one that named a reset.
        let within = |at_ms: i64, w: &Window| match (anchor, w.resets_at) {
            (Some(a), Some(r)) => r >= a.saturating_sub(ANCHOR_SLACK_S),
            (_, None) => Some(at_ms) == newest_of_kind,
            (None, Some(_)) => true,
        };
        let held: Seen<'_> = live
            .into_iter()
            .filter(|(_, at_ms, w)| within(*at_ms, w))
            .collect();
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
    pick(&home_readings(home), CODEX_FAMILY, now_unix)
}

/// [`home_pick`] of the readings from `since` on: an older one no longer speaks for the home's row
/// ([`crate::harness::accounts::Account::room_since`]). A pick keeps a window's largest share across
/// rollouts and stamps a tie with its earliest reading, so what it picked cannot say this afterwards.
#[must_use]
pub fn home_pick_since(home: &Path, since: Option<i64>, now_unix: i64) -> Option<Pick> {
    pick(
        &readings_since(home_readings(home), since),
        CODEX_FAMILY,
        now_unix,
    )
}

/// `rollouts` without the readings from before `since`.
fn readings_since(
    mut rollouts: Vec<Vec<CodexLimits>>,
    since: Option<i64>,
) -> Vec<Vec<CodexLimits>> {
    if let Some(since) = since {
        for rollout in &mut rollouts {
            rollout.retain(|reading| reading.at_unix >= since);
        }
    }
    rollouts
}

/// The usage readings of the six (`NEWEST_READ`) rollouts of `home` written last
/// ([`rollout_readings`]).
fn home_readings(home: &Path) -> Vec<Vec<CodexLimits>> {
    let mut scratch = Vec::with_capacity(64 * 1024);
    rollouts_by_write(home)
        .iter()
        .take(NEWEST_READ)
        .filter_map(|(path, _)| rollout_readings_inner(path, CONTEXT_REACH, true, &mut scratch))
        .collect()
}

/// How far back a rollout can hold a window that still runs: the longest a window lasts, a week.
const LOGIN_REACH_S: i64 = 7 * 86_400;

/// How many of a home's rollouts written last a survey looks through for one login's readings
/// ([`login_readings`]). Each costs its first line ([`META_BYTES`] at most). One that login opened
/// costs that line again, one tail ([`TAIL_BYTES`]) and any copied prefix a legacy fork holds
/// ([`CONTEXT_REACH`] at most). A look ends once six hold a reading from `since` on and no older
/// rollout could hold a newer one. A rollout that holds none is paid for and not counted, and it need
/// not be short: it can hold only another family's readings, or only readings from before `since`.
/// One written lately whose readings are old is paid for too, since it does not end the look. So the
/// worst case reads all 256: 160 MiB a home where first lines fit one 64 KiB read and none is a legacy
/// fork, and at most about 17 GiB a home where each is read to the reach for a copied prefix. This many
/// rollouts written after a wall take that wall out of view. A node's metrics pass surveys every
/// minute, so a look whose rollouts have not changed reads none of them ([`LOGIN_LOOKS`]).
const LOGIN_SCAN: usize = 256;

/// A [`login_readings`]: what it was asked, the rollouts it had in reach, and what it found.
struct LoginLook {
    since: Option<i64>,
    listed: Vec<(std::path::PathBuf, std::time::SystemTime, u64)>,
    rollouts: Vec<Vec<CodexLimits>>,
    whole: bool,
}

/// The last look at each home for each login, where its answer is a function of the listed
/// rollouts' bytes and `since`. It stands until a listed size or write time or `since` changes: new
/// bytes under the same size and write time stay unread. A look that could not open or read a folder
/// or a rollout is not kept, since that could go otherwise with nothing listed having changed. One
/// that met a fork whose copied prefix runs past the reach is kept, since that fork's bytes fix what a
/// read of it finds, and it says it is not whole, since that fork's own lines went unread. It holds
/// six rollouts' readings and a listing of [`LOGIN_SCAN`] at most, for each of the homes and logins a
/// survey reads.
static LOGIN_LOOKS: std::sync::Mutex<
    std::collections::BTreeMap<(std::path::PathBuf, AccountKey), LoginLook>,
> = std::sync::Mutex::new(std::collections::BTreeMap::new());

/// The usage readings, from `since` on, of the six (`NEWEST_READ`) rollouts of `home` whose newest
/// reading of the `codex` family is newest, of those that opened under the login `key`
/// ([`opening_account`]). A rollout with no such reading says nothing of a wall, and a session that
/// starts on a wall writes no figure, so such a rollout is not counted among the six. Six newer
/// readings say more of the windows than an older one could. A write time says nothing of that, since
/// a rollout written again can add no reading, so the look goes on past six until no older rollout
/// could hold a newer reading: a reading is stamped before it is written. So, up to [`LOGIN_SCAN`]
/// rollouts, neither a login's own rollouts with no newer reading nor other sessions' in the same home
/// push its wall out of view. The look reaches back only as far as a rollout can still speak: one last
/// written before `since` holds no reading from `since` on, and one last written over
/// [`LOGIN_REACH_S`] ago holds no running window. A look that finds what the last one had in reach,
/// unchanged, reads none of it ([`LOGIN_LOOKS`]). The answer also says whether the look read everything
/// it needed; where it did not, a wall can be missing from what it found.
fn login_readings(
    home: &Path,
    key: AccountKey,
    since: Option<i64>,
    now_unix: i64,
) -> (Vec<Vec<CodexLimits>>, bool) {
    login_readings_with(
        home,
        key,
        since,
        now_unix,
        &rollout_opening,
        &|path, scratch| rollout_readings_checked(path, CONTEXT_REACH, true, scratch),
    )
}

/// The first line of the rollout at `path`, `None` where it has none within [`META_BYTES`].
fn rollout_opening(path: &Path) -> Result<Option<String>, ReadFailed> {
    let file = crate::harness::footer::open_regular(path).ok_or(ReadFailed)?;
    let mut line = Vec::new();
    let found = first_line_read(&file, &mut line).map_err(|_| ReadFailed)?;
    Ok(found.then(|| String::from_utf8_lossy(&line).into_owned()))
}

/// [`login_readings`], reading each rollout through `opening` and `readings`.
fn login_readings_with(
    home: &Path,
    key: AccountKey,
    since: Option<i64>,
    now_unix: i64,
    opening: &dyn Fn(&Path) -> Result<Option<String>, ReadFailed>,
    readings: &dyn Fn(&Path, &mut Vec<u8>) -> RolloutReadings,
) -> (Vec<Vec<CodexLimits>>, bool) {
    let mut scratch = Vec::with_capacity(64 * 1024);
    let week_ago = now_unix.saturating_sub(LOGIN_REACH_S);
    let reach = u64::try_from(since.map_or(week_ago, |since| since.max(week_ago))).unwrap_or(0);
    let (all_listed, walked_whole) = crate::harness::codex_rollout::rollouts_by_write_sized(home);
    let listed: Vec<_> = all_listed
        .into_iter()
        .take_while(|(_, written, _)| {
            written
                .duration_since(std::time::UNIX_EPOCH)
                .is_ok_and(|at| at.as_secs() >= reach)
        })
        .take(LOGIN_SCAN)
        .collect();
    let slot = (home.to_path_buf(), key);
    let looks = || LOGIN_LOOKS.lock().unwrap_or_else(|p| p.into_inner());
    if walked_whole
        && let Some(look) = looks().get(&slot)
        && look.since == since
        && look.listed == listed
    {
        return (look.rollouts.clone(), look.whole);
    }
    let mut keep = walked_whole;
    let mut whole = walked_whole;
    // Each rollout kept, in the order listed, with its newest reading's stamp in milliseconds.
    let mut kept: Vec<(i64, Vec<CodexLimits>)> = Vec::with_capacity(NEWEST_READ + 1);
    for (path, written, size) in &listed {
        if kept.len() == NEWEST_READ {
            let written_ms = written
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |at| i64::try_from(at.as_millis()).unwrap_or(i64::MAX));
            if kept.iter().all(|(newest, _)| written_ms <= *newest) {
                break;
            }
        }
        match opening(path) {
            Ok(line) if line.as_deref().and_then(opening_account) == Some(key) => {}
            // Its first line runs past the cap, so its login is not known and it may be this one.
            Ok(None) if *size > META_BYTES => {
                whole = false;
                continue;
            }
            Ok(_) => continue,
            Err(ReadFailed) => {
                keep = false;
                whole = false;
                continue;
            }
        }
        let rollout = match readings(path, &mut scratch) {
            Ok(Some(rollout)) => rollout,
            Ok(None) => {
                whole = false;
                continue;
            }
            Err(ReadFailed) => {
                keep = false;
                whole = false;
                continue;
            }
        };
        let rollout: Vec<CodexLimits> = readings_since(vec![rollout], since)
            .into_iter()
            .flatten()
            .collect();
        let newest = rollout
            .iter()
            .filter(|reading| reading.speaks_of(CODEX_FAMILY))
            .map(|reading| reading.at_ms)
            .max();
        let Some(newest) = newest else {
            continue;
        };
        kept.push((newest, rollout));
        if kept.len() > NEWEST_READ {
            // The oldest goes, and of a tie the one listed last, whose write came first.
            let oldest = kept
                .iter()
                .enumerate()
                .min_by_key(|(at, (newest, _))| (*newest, std::cmp::Reverse(*at)))
                .map(|(at, _)| at);
            if let Some(oldest) = oldest {
                kept.remove(oldest);
            }
        }
    }
    let rollouts: Vec<Vec<CodexLimits>> = kept.into_iter().map(|(_, rollout)| rollout).collect();
    if keep {
        looks().insert(
            slot,
            LoginLook {
                since,
                listed,
                rollouts: rollouts.clone(),
                whole,
            },
        );
    }
    (rollouts, whole)
}

/// A wall a login's rollouts show ([`login_look`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoginWall {
    /// When the reading it stands on was written (unix seconds): for a full window the earliest
    /// reading of its largest share, for a refusal the reading that named it, and of several walls
    /// the newest of those. Only a later reading can say anything of room.
    pub at: i64,
    /// When it ends (unix seconds), which may have passed: the reset a reading named, or where it
    /// named none, the most its window can run from that reading: its length, a week at most.
    pub until: i64,
}

/// What the rollouts of a login hold of it ([`login_look`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginSample {
    /// What is left of its five-hour window and its week ([`sample_of`]). `at` is its newest
    /// reading's stamp.
    pub sample: Sample,
    /// The wall its readings show, which no [`Sample`] can hold: a window of another length or none
    /// reads no figure, and a refusal can stand with every window below full.
    pub wall: Option<LoginWall>,
}

/// What a look at a login's rollouts found ([`login_look`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginLooked {
    /// What they hold of it, where a reading is fresh or a wall still stands.
    pub sample: Option<LoginSample>,
    /// The look read everything it needed. Where it did not, a wall can be missing from `sample`, so
    /// neither its absence nor a reading in it says the row has room.
    pub whole: bool,
}

/// The wall `pick` and the `newest` reading of a login show: a window full, whatever its kind and
/// whether or not it named a reset, and a refusal. That is wider than
/// [`crate::harness::codex_app_login::usage_of`], whose full windows are only the five-hour and the
/// week. A refusal is read from the newest reading alone, so a later reading that names none lifts
/// it. A pick keeps a full window's reading until its reset where one is named, and until a later
/// reading of its kind where none is.
fn login_wall(pick: Option<&Pick>, newest: &CodexLimits) -> Option<LoginWall> {
    let full = pick
        .into_iter()
        .flat_map(|pick| &pick.windows)
        .filter(|window| window.used_percent >= 100.0)
        .map(|window| LoginWall {
            at: window.as_of,
            until: window.ends(),
        });
    let refused = newest.refused().then(|| LoginWall {
        at: newest.at_unix,
        until: newest.refusal_ends(),
    });
    full.chain(refused).reduce(|a, b| LoginWall {
        at: a.at.max(b.at),
        until: a.until.max(b.until),
    })
}

/// What the rollouts of `homes` hold of the login `key` at `now_unix`: what is left of its windows,
/// and the wall its readings show ([`login_wall`]), where one of its readings is no older than
/// [`FRESH_S`] or that wall still stands, since a session stopped at a wall writes no later reading.
/// Only a rollout opened under that login counts ([`login_readings`]), and one that names no login
/// is left out rather than guessed at, and so is a reading from before `since`, once the row is
/// known to have had room ([`crate::harness::accounts::Account::room_since`]).
///
/// A wall that names no reset lifts as a named one does at a launch's read that shows room, which
/// moves `since`. It also lifts at a later reading that is not full, and it ends once its window
/// can have run out, a week from its reading at most.
///
/// A ROLLOUT NAMES ONLY THE LOGIN THAT OPENED IT: one resumed under another login keeps its
/// opening line, and nothing says which login wrote a reading ([`conversation_newest`]). So this
/// speaks for `key` only in a home no other login writes to, which the caller answers for as things
/// stand now ([`crate::harness::cli::survey_accounts`]). A login removed from that home within the
/// week can have written under this one's opening line: its wall reads as this one's, which keeps a
/// row from being picked until that wall ends or a launch reads it, and its later reading with room
/// lifts a wall of this one only rollouts knew, which a launch's read of the row then shows again.
/// Reads files alone.
#[must_use]
pub fn login_look(
    homes: &[&Path],
    key: AccountKey,
    since: Option<i64>,
    now_unix: i64,
) -> LoginLooked {
    let mut whole = true;
    let rollouts: Vec<Vec<CodexLimits>> = homes
        .iter()
        .flat_map(|home| {
            let (rollouts, read) = login_readings(home, key, since, now_unix);
            whole &= read;
            rollouts
        })
        .collect();
    LoginLooked {
        sample: login_sample_of(&rollouts, now_unix),
        whole,
    }
}

/// [`login_look`]'s sample of what it found in `rollouts`.
fn login_sample_of(rollouts: &[Vec<CodexLimits>], now_unix: i64) -> Option<LoginSample> {
    let newest = rollouts
        .iter()
        .flatten()
        .filter(|r| r.speaks_of(CODEX_FAMILY))
        .max_by_key(|r| r.at_ms)?;
    let pick = pick(rollouts, CODEX_FAMILY, now_unix);
    let wall = login_wall(pick.as_ref(), newest);
    let stands = wall.is_some_and(|wall| wall.until > now_unix);
    (stands || now_unix.saturating_sub(newest.at_unix) <= FRESH_S).then(|| LoginSample {
        sample: Sample {
            at: newest.at_unix,
            ..pick.as_ref().map(sample_of).unwrap_or_default()
        },
        wall,
    })
}

/// A [`Pick`] as the account pool's usage [`Sample`]: its five-hour window is the session, its
/// week the week, each share taken up to the next whole percent (so a share is never read as
/// further from its wall than it is) and each reset and read time as picked. Another window, an
/// unplaced one and a void one ([`Pick::void`]) give no figure, which [`LoginSample::wall`]
/// makes up for where such a window is full. `at` is the newest reading's stamp; `by` is left for
/// [`crate::harness::accounts::note_sample`] to stamp.
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
    let conv = crate::harness::codex_rollout::conversation_rollouts_for_sample(home, thread);
    let mut scratch = Vec::with_capacity(64 * 1024);
    let rollouts: Vec<Vec<CodexLimits>> = conversation_reads(&conv)
        .filter_map(|path| {
            let mut readings = rollout_readings_inner(path, CONTEXT_REACH, false, &mut scratch)?;
            readings.retain(|r| r.at_unix >= started_unix);
            (!readings.is_empty()).then_some(readings)
        })
        .collect();
    let pick = pick(&rollouts, CODEX_FAMILY, now_unix)?;
    (now_unix.saturating_sub(pick.newest) <= FRESH_S).then(|| sample_of(&pick))
}

/// The rollouts of `conv` a usage reading is looked for in: its root rollouts, then the six
/// (`NEWEST_READ`) of its subagent rollouts written last.
fn conversation_reads(conv: &Conversation) -> impl Iterator<Item = &std::path::PathBuf> {
    let root_count = conv.files.len().saturating_sub(conv.subagents);
    let roots = &conv.files[..root_count];
    let recent_subs = &conv.files[root_count + conv.subagents.saturating_sub(NEWEST_READ)..];
    roots.iter().chain(recent_subs.iter())
}

/// The `codex` family's newest reading with a window in the rollouts of `conv` that
/// [`thread_sample`] reads too ([`conversation_reads`]), picked alone at `now_unix` ([`pick`]),
/// whatever its age: its windows as Codex last wrote them, a window whose reset has passed
/// void.
///
/// THE NEWEST READING ALONE, never the largest share of several: the rollout of a conversation
/// resumed on another login keeps the turns the earlier login read, and nothing in a rollout
/// says which login wrote a reading, so a share of several could be the earlier login's. A
/// fork's copied prefix is no reading of its own ([`rollout_readings`]).
#[must_use]
pub fn conversation_newest(conv: &Conversation, now_unix: i64) -> Option<Pick> {
    let mut scratch = Vec::with_capacity(64 * 1024);
    let newest = conversation_reads(conv)
        .filter_map(|path| rollout_readings_inner(path, CONTEXT_REACH, false, &mut scratch))
        .flatten()
        .filter(|r| r.family.as_str() == CODEX_FAMILY && !r.windows.is_empty())
        .max_by_key(|r| r.at_ms)?;
    pick(&[vec![newest]], CODEX_FAMILY, now_unix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::codex_rollout::PAGINATED;
    use crate::harness::codex_rollout::fixtures::{
        context, message, meta, meta_as, no_reset, rate, refused,
    };
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

    /// NOTHING SAYS TWO READINGS THAT NAME NO RESET ARE OF ONE WINDOW, so only a kind's newest
    /// speaks: a later one below full lifts a full one, and one that names no reset is not lost to
    /// an older one that named a reset, whose reset it stands until. A reading that did name a reset
    /// keeps the anchor's law: a later rollout's lower share leaves the largest where it was.
    /// NEGATIVE CONTROL: with no anchor the largest share stood, so a full reading stranded the row
    /// for a week, and a full reading was dropped beside any reading that named a reset.
    #[test]
    fn a_window_that_names_no_reset_is_read_from_its_kinds_newest_reading() {
        let now = T0 / 1000;
        let reset = now + 3_600;
        let five = |ms: i64, used: f64, resets: Option<i64>| {
            let mut read = reading(ms, key(LOGIN_A), &[(Slot::Primary, used, 300, 0)]);
            read.windows[0].resets_at = resets;
            read
        };
        let of = |rollouts: &[CodexLimits]| {
            let p = pick(&apart(rollouts), CODEX_FAMILY, now).expect("a pick");
            (p.windows[0].used_percent, p.windows[0].resets_at)
        };
        let (older, newer) = (T0 - 60_000, T0 - 10_000);
        assert_eq!(
            of(&[five(older, 100.0, None), five(newer, 20.0, None)]),
            (20.0, None),
            "a later reading below full lifts it"
        );
        assert_eq!(
            of(&[five(older, 20.0, None), five(newer, 100.0, None)]),
            (100.0, None)
        );
        assert_eq!(
            of(&[five(older, 30.0, Some(reset)), five(newer, 100.0, None)]),
            (100.0, Some(reset)),
            "it stands until the reset a read named"
        );
        assert_eq!(
            of(&[five(older, 100.0, None), five(newer, 20.0, Some(reset))]),
            (20.0, Some(reset)),
            "a later reading below full lifts it, whatever it names"
        );
        assert_eq!(
            of(&[five(older, 100.0, Some(reset)), five(newer, 20.0, None)]),
            (100.0, Some(reset)),
            "CONTROL: a full window that named its reset stands across rollouts"
        );
        assert_eq!(
            of(&[
                five(older, 100.0, None),
                five(newer - 1_000, 20.0, None),
                five(newer, 40.0, Some(reset))
            ]),
            (40.0, Some(reset)),
            "a full reading that named none is out of view once a later reading speaks"
        );
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
            &format!(
                "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n[[account]]\n\
                 label = \"work\"\nclass = \"dedicated\"\ndir = \"{}\"\n\
                 flow = \"device\"\n",
                crate::harness::host_path::abs("/logins/codex/work")
            ),
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
                yields: None,
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

    /// A LOGIN'S FIGURES COME ONLY FROM ROLLOUTS THAT OPENED UNDER IT: another login's newer reading
    /// in the same home never speaks for it, a rollout that names no login is left out rather
    /// than guessed at, and a reading older than [`FRESH_S`] gives none unless it reads a window
    /// full with its reset ahead: a session stopped at a wall writes nothing later, so age says
    /// nothing of that wall, until its window ends.
    #[test]
    fn a_logins_sample_is_read_only_from_its_own_fresh_rollouts() {
        let home = aterm_tempfile::Builder::new()
            .prefix("codex-login-sample-")
            .tempdir()
            .expect("temp dir");
        let day = home.path().join("sessions/2026/10/16");
        std::fs::create_dir_all(&day).expect("mkdir");
        let now_ms = T0;
        let now = now_ms / 1000;
        let week = now + 300_000;
        let write = |name: &str, thread: &str, login, at_ms: i64, used: f64| {
            let body = [
                meta_as(at_ms - 1_000, thread, thread, None, Some(PAGINATED), login),
                rate(at_ms, "codex", None, Some((used, 10_080, week))),
            ]
            .concat();
            std::fs::write(day.join(format!("rollout-{name}-{thread}.jsonl")), body)
                .expect("write rollout");
        };
        write(
            "2026-10-16T08-00-00",
            ROOT,
            Some(LOGIN_A),
            now_ms - 60_000,
            100.0,
        );
        write(
            "2026-10-16T09-00-00",
            SUB,
            Some(LOGIN_B),
            now_ms - 5_000,
            7.0,
        );
        write("2026-10-16T10-00-00", OTHER, None, now_ms - 1_000, 3.0);

        let homes = [home.path()];
        let of = |login, at| login_sample(&homes, key(login).expect("a key"), None, at);
        assert_eq!(of(LOGIN_A, now).and_then(|s| s.sample.week_pct), Some(100));
        assert_eq!(of(LOGIN_B, now).and_then(|s| s.sample.week_pct), Some(7));
        assert_eq!(
            of(("acct-lab-3", "user-lab"), now),
            None,
            "no rollout opened under it"
        );
        assert_eq!(
            of(LOGIN_B, now + FRESH_S + 61),
            None,
            "its reading is too old to read"
        );
        assert_eq!(
            of(LOGIN_A, now + FRESH_S + 61).and_then(|s| s.sample.week_pct),
            Some(100),
            "a wall outlasts the age of the reading that showed it"
        );
        assert_eq!(
            of(LOGIN_A, week + 1),
            None,
            "CONTROL: its window ended, so nothing is left of it"
        );
    }

    /// One rollout of the login `LOGIN_A` in a day folder under `day`, holding `lines`, written at
    /// `written`; `n` tells it from the others.
    fn lay_rollout(day: &Path, n: usize, written: i64, lines: &[String]) {
        let thread = format!("01900000-0000-7000-8000-{n:012}");
        let body = format!(
            "{}{}",
            meta_as(
                T0 - 7_200_000,
                &thread,
                &thread,
                None,
                Some(PAGINATED),
                Some(LOGIN_A)
            ),
            lines.concat()
        );
        let path = day.join(format!("rollout-2026-10-16T08-00-00-{thread}.jsonl"));
        std::fs::write(&path, body).expect("write rollout");
        let at = std::time::UNIX_EPOCH
            + std::time::Duration::from_secs(u64::try_from(written).expect("after the epoch"));
        std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("open")
            .set_modified(at)
            .expect("set its write time");
    }

    /// A home, and the day folder its rollouts go in.
    fn rollout_home() -> (aterm_tempfile::TempDir, std::path::PathBuf) {
        let home = aterm_tempfile::Builder::new()
            .prefix("codex-login-wall-")
            .tempdir()
            .expect("temp dir");
        let day = home.path().join("sessions/2026/10/16");
        std::fs::create_dir_all(&day).expect("mkdir");
        (home, day)
    }

    /// A WALL IS A FULL WINDOW OF ANY KIND, named reset or none, or a refusal with every window below
    /// full. That is wider than the endpoint's reading ([`crate::harness::codex_app_login::usage_of`]),
    /// whose full windows are only the five-hour and the week. Each outlasts the age of its reading,
    /// and one that names no reset ends when its window can have run out, or a week on where nothing
    /// says how long that is. NEGATIVE CONTROL: only a five-hour or weekly window with its reset ahead
    /// was read, and `rate_limit_reached_type` by nothing, so each of the others read `usable`.
    #[test]
    fn a_logins_wall_is_any_full_window_or_a_refusal() {
        let now = T0 / 1000;
        let at_ms = T0 - 60_000;
        let at = at_ms / 1000;
        let ahead = now + 3 * 86_400;
        let read = |lines: &[String], at_now: i64| {
            let (home, day) = rollout_home();
            lay_rollout(&day, 0, now, lines);
            login_sample(&[home.path()], key(LOGIN_A).expect("a key"), None, at_now)
        };
        let wall = |lines: &[String]| read(lines, now).and_then(|s| s.wall);
        for minutes in [300, 1440, 10_080, 90, 0] {
            let named = rate(at_ms, "codex", Some((100.0, minutes, ahead)), None);
            assert_eq!(
                wall(std::slice::from_ref(&named)),
                Some(LoginWall { at, until: ahead }),
                "{minutes} minutes, its reset named"
            );
            assert_eq!(
                read(std::slice::from_ref(&named), now + FRESH_S + 61).and_then(|s| s.wall),
                Some(LoginWall { at, until: ahead }),
                "{minutes} minutes: a wall outlasts the age of its reading"
            );
        }
        for (minutes, length) in [
            (300, 5 * 3600),
            (1440, 86_400),
            (10_080, 7 * 86_400),
            (90, 90 * 60),
            (0, LOGIN_REACH_S),
            (60 * 24 * 30, LOGIN_REACH_S),
        ] {
            let until = at + length;
            let unnamed = no_reset(&rate(at_ms, "codex", Some((100.0, minutes, 0)), None));
            assert_eq!(
                wall(std::slice::from_ref(&unnamed)),
                Some(LoginWall { at, until }),
                "{minutes} minutes, no reset named"
            );
        }
        let refusal = |slot| vec![refused(&rate(at_ms, "codex", None, slot))];
        assert_eq!(
            wall(&refusal(Some((30.0, 10_080, ahead)))),
            Some(LoginWall { at, until: ahead }),
            "refused with its week at 30%"
        );
        assert_eq!(
            wall(&refusal(Some((30.0, 10_080, now - 1)))),
            Some(LoginWall { at, until: now - 1 }),
            "refused, its window run out since"
        );
        assert_eq!(
            wall(&[refused(&rate(
                at_ms,
                "codex",
                Some((60.0, 300, now + 60)),
                Some((30.0, 10_080, ahead))
            ))]),
            Some(LoginWall {
                at,
                until: now + 60
            }),
            "refused: its most used window"
        );
        assert_eq!(
            wall(&refusal(None)),
            Some(LoginWall {
                at,
                until: at + LOGIN_REACH_S
            }),
            "refused with no window: a week on"
        );
        assert_eq!(
            wall(&[rate(at_ms, "codex", None, Some((99.0, 10_080, ahead)))]),
            None,
            "CONTROL: room, and no refusal"
        );
        assert_eq!(
            wall(&[
                rate(at_ms, "codex", None, Some((99.0, 10_080, ahead))).replace(
                    "\"rate_limit_reached_type\":null",
                    "\"rate_limit_reached_type\":\"unknown\""
                )
            ]),
            None,
            "CONTROL: the endpoint reads `unknown` as no refusal too"
        );
        assert_eq!(
            wall(&[refused(&rate(
                at_ms,
                "codex_bengalfox",
                None,
                Some((30.0, 10_080, ahead))
            ))]),
            None,
            "CONTROL: another family's refusal"
        );
    }

    /// A REFUSAL'S END IS FIXED BY ITS READING, so the clock does not re-judge which window runs: a
    /// five-hour wall ends at its reset, and the week's end does not take over. NEGATIVE CONTROL: the
    /// window that "still runs" was judged at the clock, so at the first reset the wall moved to the
    /// week's reset, and a row that could run read `limited` for days with no later reading to lift it.
    #[test]
    fn a_refusals_end_does_not_move_when_its_first_window_resets() {
        let now = T0 / 1000;
        let at_ms = T0 - 60_000;
        let at = at_ms / 1000;
        let first = now + 60;
        let week = now + 4 * 86_400;
        let read = |line: &String, at_now: i64| {
            let (home, day) = rollout_home();
            lay_rollout(&day, 0, now, std::slice::from_ref(line));
            login_sample(&[home.path()], key(LOGIN_A).expect("a key"), None, at_now)
        };
        let wall = |line: &String, at_now: i64| read(line, at_now).and_then(|s| s.wall);
        let until = |until: i64| Some(LoginWall { at, until });
        let windows = |five: (f64, i64), weekly: (f64, i64)| {
            refused(&rate(
                at_ms,
                "codex",
                Some((five.0, 300, five.1)),
                Some((weekly.0, 10_080, weekly.1)),
            ))
        };
        for used in [100.0, 60.0] {
            let line = windows((used, first), (30.0, week));
            assert_eq!(
                wall(&line, now),
                until(first),
                "CONTROL: {used}%, before its reset"
            );
            assert_eq!(
                wall(&line, first + 1),
                until(first),
                "{used}%, after its reset: the week's end does not take over"
            );
            assert_eq!(
                read(&line, at + FRESH_S + 1),
                None,
                "{used}%: it has ended and its reading is too old to say anything"
            );
        }
        let week_most_used = windows((30.0, first), (60.0, week));
        for at_now in [now, first + 1] {
            assert_eq!(
                wall(&week_most_used, at_now),
                until(week),
                "CONTROL: the week was its most used window, so the first reset does not end it"
            );
        }
        assert_eq!(
            wall(&windows((100.0, at - 5), (30.0, week)), now),
            until(week),
            "a window already over at the reading is not the one it names"
        );
        assert_eq!(
            wall(&windows((30.0, at - 5), (60.0, at - 9)), now),
            until(at - 9),
            "every window over at the reading: the most used one's end, which has passed"
        );
    }

    /// A WALL THAT NAMES NO RESET CANNOT STRAND A ROW. A later reading that is not full lifts it, a
    /// later one with no refusal lifts a refusal, a launch's read that shows room moves `since`
    /// past it, and it ends by itself when its window can have run out. Then its reading stays
    /// beside it as the reason no read shows room, until that reading is too old to say anything.
    #[test]
    fn a_wall_that_names_no_reset_lifts() {
        let now = T0 / 1000;
        let ahead = now + 3 * 86_400;
        let at_ms = T0 - 60_000;
        let at = at_ms / 1000;
        let later = T0 - 10_000;
        let full = no_reset(&rate(at_ms, "codex", Some((100.0, 300, 0)), None));
        let read = |lines: &[String], since: Option<i64>, at_now: i64| {
            let (home, day) = rollout_home();
            lay_rollout(&day, 0, now, lines);
            login_sample(&[home.path()], key(LOGIN_A).expect("a key"), since, at_now)
        };
        let walled = |lines: &[String]| read(lines, None, now).and_then(|s| s.wall);
        assert!(
            walled(std::slice::from_ref(&full)).is_some(),
            "CONTROL: it stands"
        );
        assert_eq!(
            walled(&[
                full.clone(),
                no_reset(&rate(later, "codex", Some((20.0, 300, 0)), None))
            ]),
            None,
            "a later reading that is not full"
        );
        let refusal = refused(&rate(at_ms, "codex", None, Some((30.0, 10_080, ahead))));
        assert!(
            walled(std::slice::from_ref(&refusal)).is_some(),
            "CONTROL: it stands"
        );
        assert_eq!(
            walled(&[
                refusal.clone(),
                rate(later, "codex", None, Some((30.0, 10_080, ahead)))
            ]),
            None,
            "a later reading with no refusal"
        );
        for lines in [full.clone(), refusal] {
            assert_eq!(
                read(&[lines], Some(at + 1), now),
                None,
                "a launch's read that showed room"
            );
        }
        let ended = at + 5 * 3600 + 1;
        let late = read(std::slice::from_ref(&full), None, ended).expect("a read still fresh");
        assert_eq!(
            (late.sample.session(ended), late.wall),
            (
                None,
                Some(LoginWall {
                    at,
                    until: at + 5 * 3600
                })
            ),
            "its window can have run out, and nothing later shows room"
        );
        assert_eq!(
            read(&[full], None, at + FRESH_S + 1),
            None,
            "then its reading is too old to say anything"
        );
    }

    /// A login's own rollouts are found before the six are counted: however many newer rollouts other
    /// logins and subagents wrote in that home, its wall stays in view, and a rollout written too
    /// long ago to hold a running window is not read.
    #[test]
    fn newer_rollouts_of_others_never_push_a_logins_wall_out_of_view() {
        let home = aterm_tempfile::Builder::new()
            .prefix("codex-login-cap-")
            .tempdir()
            .expect("temp dir");
        let day = home.path().join("sessions/2026/10/16");
        std::fs::create_dir_all(&day).expect("mkdir");
        let now_ms = T0;
        let now = now_ms / 1000;
        let week = now + 300_000;
        let set_written = |path: &std::path::Path, at: i64| {
            let at = std::time::UNIX_EPOCH
                + std::time::Duration::from_secs(u64::try_from(at).expect("after the epoch"));
            std::fs::File::options()
                .write(true)
                .open(path)
                .expect("open")
                .set_modified(at)
                .expect("set its write time");
        };
        let write = |name: String, thread: &str, login, at_ms: i64, used: f64| {
            let body = [
                meta_as(at_ms - 1_000, thread, thread, None, Some(PAGINATED), login),
                rate(at_ms, "codex", None, Some((used, 10_080, week))),
            ]
            .concat();
            let path = day.join(format!("rollout-{name}-{thread}.jsonl"));
            std::fs::write(&path, body).expect("write rollout");
            path
        };
        let own = write(
            "2026-10-16T08-00-00".to_string(),
            ROOT,
            Some(LOGIN_A),
            now_ms - 3_600_000,
            100.0,
        );
        set_written(&own, now - 3_600);
        for i in 0..NEWEST_READ * 2 {
            let login = (i % 2 == 0).then_some(LOGIN_B);
            let thread = format!("01900000-0000-7000-8000-0000000000{i:02}");
            let newer = write(
                format!("2026-10-16T09-00-{i:02}"),
                &thread,
                login,
                now_ms - 60_000,
                7.0,
            );
            set_written(&newer, now - 60);
        }
        let homes = [home.path()];
        let a = key(LOGIN_A).expect("a key");
        assert_eq!(
            login_sample(&homes, a, None, now).and_then(|s| s.sample.week_pct),
            Some(100)
        );
        set_written(&own, now - LOGIN_REACH_S - 1);
        assert_eq!(
            login_sample(&homes, a, None, now),
            None,
            "written too long ago"
        );
    }

    /// A ROLLOUT WITH NO READING SAYS NOTHING OF A WALL, so a login's own such rollouts do not count
    /// among the six: a session that starts on a wall writes no figure, and launches that read
    /// nothing first make them one after another. Six that hold newer readings do push it out, since they
    /// say more of its windows than it can, and so a reading is counted only while it still speaks
    /// (`since`). NEGATIVE CONTROL: the six were counted before any was read, so six such launches
    /// hid the wall and the row read `usable` or `unseen`.
    #[test]
    fn a_logins_own_rollouts_with_no_reading_never_push_its_wall_out_of_view() {
        let (home, day) = rollout_home();
        let now = T0 / 1000;
        let week = now + 300_000;
        let homes = [home.path()];
        let a = key(LOGIN_A).expect("a key");
        let at_ms = T0 - 3_600_000;
        let wall = rate(at_ms, "codex", None, Some((100.0, 10_080, week)));
        lay_rollout(&day, 0, now - 3_600, std::slice::from_ref(&wall));
        for n in 1..=NEWEST_READ * 2 {
            let says_nothing = [
                context(T0 - 60_000, "gpt-6.1-sol", None),
                rate(T0 - 60_000, "codex", None, None),
                rate(T0 - 60_000, "premium", None, None),
                rate(T0 - 60_000, "codex_bengalfox", Some((3.0, 300, week)), None),
            ];
            lay_rollout(&day, n, now - 60, &says_nothing);
        }
        let read = |since| login_sample(&homes, a, since, now);
        assert_eq!(
            read(None).and_then(|s| s.sample.week_pct),
            Some(100),
            "its wall stays in view"
        );
        let (home, day) = rollout_home();
        let homes = [home.path()];
        lay_rollout(&day, 0, now - 3_600, std::slice::from_ref(&wall));
        for n in 1..=NEWEST_READ {
            let since_then = rate(T0 - 60_000, "codex", None, Some((7.0, 10_080, week)));
            lay_rollout(&day, n, now - 60, std::slice::from_ref(&since_then));
        }
        assert_eq!(
            login_sample(&homes, a, None, now).and_then(|s| s.sample.week_pct),
            Some(7),
            "six that hold a reading say more than it can"
        );
        let (home, day) = rollout_home();
        let homes = [home.path()];
        let after_since = T0 - 300_000;
        lay_rollout(
            &day,
            0,
            now - 300,
            &[rate(
                after_since,
                "codex",
                None,
                Some((100.0, 10_080, week)),
            )],
        );
        for n in 1..=NEWEST_READ {
            lay_rollout(&day, n, now - 60, std::slice::from_ref(&wall));
        }
        assert_eq!(
            login_sample(&homes, a, Some(now - 600), now).and_then(|s| s.wall),
            Some(LoginWall {
                at: after_since / 1000,
                until: week
            }),
            "readings from before `since` no longer speak, so they are not counted"
        );
    }

    /// SIX ROLLOUTS WRITTEN LATELY DO NOT HIDE A NEWER READING: a session resumed since can write no
    /// reading, so what it was written last says nothing of how new its readings are. A wall read an
    /// hour ago stays in view behind six written a minute ago that hold readings from two hours ago.
    /// NEGATIVE CONTROL: the look took the six written last and the row lost its wall. CONTROL: six
    /// whose readings are newer than anything an older rollout can hold end the look, so a rollout
    /// past them is not read, which its impossible stamp would show. CONTROL: of more than six the
    /// newest six are read, whatever order they were written in.
    #[test]
    fn six_rollouts_written_lately_do_not_hide_a_newer_reading() {
        let now = T0 / 1000;
        let week = now + 300_000;
        let a = key(LOGIN_A).expect("a key");
        let reading = |ago_s: i64, pct: f64| {
            rate(T0 - ago_s * 1000, "codex", None, Some((pct, 10_080, week)))
        };

        let (home, day) = rollout_home();
        let homes = [home.path()];
        lay_rollout(&day, 0, now - 3_600, &[reading(3_600, 100.0)]);
        for n in 1..=NEWEST_READ {
            lay_rollout(&day, n, now - 60, &[reading(7_200, 7.0)]);
        }
        assert_eq!(
            login_sample(&homes, a, None, now).and_then(|s| s.wall),
            Some(LoginWall {
                at: now - 3_600,
                until: week
            }),
            "the reading an hour old is among the newest six"
        );

        let (home, day) = rollout_home();
        let homes = [home.path()];
        lay_rollout(&day, 0, now - 3_600, &[reading(10, 100.0)]);
        for n in 1..=NEWEST_READ {
            lay_rollout(&day, n, now - 60, &[reading(60, 7.0)]);
        }
        assert_eq!(
            login_sample(&homes, a, None, now).and_then(|s| s.wall),
            None,
            "CONTROL: no older rollout can hold a newer reading, so this one is not read"
        );

        let (home, day) = rollout_home();
        let homes = [home.path()];
        for n in 0..NEWEST_READ {
            let ago = i64::try_from(n).expect("small") + 100;
            lay_rollout(&day, n, now - 60, &[reading(ago, 7.0)]);
        }
        // Written after the oldest of the six read, so the look goes on to them.
        lay_rollout(&day, NEWEST_READ, now - 90, &[reading(95, 100.0)]);
        lay_rollout(&day, NEWEST_READ + 1, now - 95, &[reading(99, 100.0)]);
        let wall = login_sample(&homes, a, None, now).and_then(|s| s.wall);
        assert_eq!(
            wall.map(|wall| wall.at),
            Some(now - 99),
            "CONTROL: both are newer than the oldest of the six, so both displace one"
        );
    }

    /// [`login_look`]'s sample alone.
    fn login_sample(
        homes: &[&Path],
        key: AccountKey,
        since: Option<i64>,
        now_unix: i64,
    ) -> Option<LoginSample> {
        login_look(homes, key, since, now_unix).sample
    }

    /// A legacy fork opening as `thread` under `LOGIN_A`, whose copied burst runs past [`META_BYTES`].
    fn fork_past_meta_bytes(thread: &str) -> String {
        let mut body = meta_as(
            T0,
            thread,
            thread,
            Some(ROOT),
            Some("legacy"),
            Some(LOGIN_A),
        );
        body.push_str(&meta(T0, ROOT, ROOT, None));
        let line = message(T0 + 1);
        while u64::try_from(body.len()).expect("small") <= META_BYTES + 1024 {
            body.push_str(&line);
        }
        body
    }

    /// A READ THAT FAILED IS TOLD FROM A FILE THAT HOLDS NOTHING TO READ: a directory opens and
    /// cannot be read, so it stands in for an I/O error on a rollout. NEGATIVE CONTROL: each read
    /// the same, so a failure was remembered or a fork past the reach never was.
    #[cfg(unix)]
    #[test]
    fn a_read_that_failed_is_told_from_a_file_with_nothing_to_read() {
        use crate::harness::codex_rollout::stream_open_ends_ref;

        let dir = aterm_tempfile::Builder::new()
            .prefix("codex-read-failed-")
            .tempdir()
            .expect("temp dir");
        let folder = std::fs::File::open(dir.path()).expect("a folder opens");
        let mut scratch = Vec::new();
        assert!(first_line_read(&folder, &mut scratch).is_err());
        assert!(
            stream_open_ends_ref(&folder, 0, None, 1024, |_, _, _| {
                std::ops::ControlFlow::Continue(())
            })
            .failed
        );
        assert_eq!(
            copy_end_and_account(&folder, 1024, true, &mut scratch),
            Err(ReadFailed)
        );

        let short = dir.path().join("short");
        std::fs::write(&short, "no newline").expect("write");
        let short = std::fs::File::open(short).expect("open");
        assert_eq!(first_line_read(&short, &mut scratch).ok(), Some(false));

        let fork = dir.path().join("fork");
        std::fs::write(&fork, fork_past_meta_bytes(SUB)).expect("write");
        assert_eq!(
            rollout_readings_checked(&fork, META_BYTES, true, &mut scratch),
            Ok(None),
            "a copy past the reach"
        );
        let own = dir.path().join("own");
        let filler = fork_past_meta_bytes(SUB);
        let (_, filler) = filler.split_once('\n').expect("its first line");
        let opening = meta_as(T0, ROOT, ROOT, None, Some(PAGINATED), Some(LOGIN_A));
        std::fs::write(&own, format!("{opening}{filler}")).expect("write");
        assert!(
            matches!(
                rollout_readings_checked(&own, META_BYTES, true, &mut scratch),
                Ok(Some(_))
            ),
            "CONTROL: as long a rollout that is no fork"
        );
    }

    /// A LOOK THAT FAILED TO READ A ROLLOUT SAYS SO AND IS NOT KEPT, at its first line or at its
    /// readings. One that met a fork whose copy runs past the reach is kept and is not whole, since
    /// that fork's own lines went unread and can hold a wall. NEGATIVE CONTROL: the wall stayed
    /// hidden until another write, a fork's 64 MiB was read every minute with nothing listed having
    /// changed, or a row read `usable` past the wall in that fork.
    #[test]
    fn a_look_says_whether_it_read_everything_and_keeps_only_one_that_did() {
        let now = T0 / 1000;
        let week = now + 300_000;
        let a = key(LOGIN_A).expect("a key");
        let wall = rate(T0 - 60_000, "codex", None, Some((100.0, 10_080, week)));
        let product = |path: &Path, scratch: &mut Vec<u8>| {
            rollout_readings_checked(path, CONTEXT_REACH, true, scratch)
        };
        let shows_wall = |homes: &[&Path]| {
            let look = login_look(homes, a, None, now);
            (look.sample.and_then(|s| s.wall).is_some(), look.whole)
        };

        let (home, day) = rollout_home();
        lay_rollout(&day, 0, now - 60, std::slice::from_ref(&wall));
        let (found, whole) =
            login_readings_with(home.path(), a, None, now, &|_| Err(ReadFailed), &product);
        assert_eq!((found.len(), whole), (0, false), "its first line failed");
        assert_eq!(
            shows_wall(&[home.path()]),
            (true, true),
            "nothing listed changed, and the first line reads now"
        );

        let (home, day) = rollout_home();
        lay_rollout(&day, 0, now - 60, std::slice::from_ref(&wall));
        let (found, whole) =
            login_readings_with(home.path(), a, None, now, &rollout_opening, &|_, _| {
                Err(ReadFailed)
            });
        assert_eq!((found.len(), whole), (0, false), "its readings failed");
        assert_eq!(shows_wall(&[home.path()]), (true, true));

        let (home, day) = rollout_home();
        let thread = "01900000-0000-7000-8000-000000000000";
        std::fs::write(
            day.join(format!("rollout-2026-10-16T08-00-00-{thread}.jsonl")),
            fork_past_meta_bytes(thread),
        )
        .expect("write the fork");
        let reads = std::cell::Cell::new(0);
        let past_the_reach = |path: &Path, scratch: &mut Vec<u8>| {
            reads.set(reads.get() + 1);
            rollout_readings_checked(path, META_BYTES, true, scratch)
        };
        for _ in 0..2 {
            let (found, whole) =
                login_readings_with(home.path(), a, None, now, &rollout_opening, &past_the_reach);
            assert_eq!(
                (found.len(), whole),
                (0, false),
                "its own lines went unread"
            );
        }
        assert_eq!(reads.get(), 1, "the second look read nothing");
    }

    /// A ROLLOUT WHOSE FIRST LINE RUNS PAST THE CAP MAY BE THIS LOGIN'S, so a look that skipped it
    /// did not read everything. NEGATIVE CONTROL: it counted as another login's, and a row read
    /// `usable` past the wall in it. CONTROL: an empty file and a first line still being written
    /// name no login and are nothing to read, so they cost no row its `usable`.
    #[test]
    fn a_first_line_past_the_cap_is_a_look_that_is_not_whole() {
        let now = T0 / 1000;
        let a = key(LOGIN_A).expect("a key");
        let name = |n: usize| {
            format!("rollout-2026-10-16T08-00-0{n}-01900000-0000-7000-8000-00000000000{n}.jsonl")
        };
        let (home, day) = rollout_home();
        std::fs::write(day.join(name(1)), "").expect("write");
        std::fs::write(day.join(name(2)), "{\"type\":\"session_meta\"").expect("write");
        assert!(login_look(&[home.path()], a, None, now).whole, "CONTROL");
        let long = usize::try_from(META_BYTES).expect("small") + 1;
        let line = format!("{}\n", " ".repeat(long));
        std::fs::write(day.join(name(3)), line).expect("write");
        assert!(!login_look(&[home.path()], a, None, now).whole);
    }

    /// A LINKED ROLLOUT IS AS ITS TARGET IS. The walk lists a link to a file on purpose and a read
    /// follows it, so a look judged by the link's own size and time kept its answer past a write to
    /// the target. NEGATIVE CONTROL: the look answered 40% with the target at its wall. A target that
    /// could not be examined leaves the look not whole, and a dangling link is nothing to read.
    #[cfg(unix)]
    #[test]
    fn a_linked_rollout_is_as_its_target_is() {
        let now = T0 / 1000;
        let week = now + 300_000;
        let a = key(LOGIN_A).expect("a key");
        let (home, day) = rollout_home();
        let far = aterm_tempfile::Builder::new()
            .prefix("codex-login-target-")
            .tempdir()
            .expect("temp dir");
        let target = far.path().join("target.jsonl");
        let opening = meta_as(
            T0 - 7_200_000,
            ROOT,
            ROOT,
            None,
            Some(PAGINATED),
            Some(LOGIN_A),
        );
        let reading =
            |ago_ms: i64, pct: f64| rate(T0 - ago_ms, "codex", None, Some((pct, 10_080, week)));
        std::fs::write(&target, format!("{opening}{}", reading(120_000, 40.0))).expect("write");
        let link = format!("rollout-2026-10-16T08-00-00-{ROOT}.jsonl");
        std::os::unix::fs::symlink(&target, day.join(&link)).expect("link");
        let homes = [home.path()];
        let first = login_look(&homes, a, None, now);
        assert_eq!(
            first.sample.as_ref().and_then(|s| s.wall),
            None,
            "CONTROL: it reads through the link"
        );
        assert!(first.whole);
        let mut appended = std::fs::read(&target).expect("read");
        appended.extend_from_slice(reading(60_000, 100.0).as_bytes());
        std::fs::write(&target, appended).expect("write");
        assert!(
            login_look(&homes, a, None, now)
                .sample
                .and_then(|s| s.wall)
                .is_some(),
            "the write to its target shows"
        );

        let (home, day) = rollout_home();
        std::os::unix::fs::symlink(day.join("gone"), day.join(&link)).expect("link");
        assert!(login_look(&[home.path()], a, None, now).whole, "dangling");

        let (home, day) = rollout_home();
        let looped = day.join(&link);
        std::os::unix::fs::symlink(&looped, &looped).expect("link");
        assert!(
            !login_look(&[home.path()], a, None, now).whole,
            "a target that could not be examined"
        );
    }

    /// A FOLDER THAT COULD NOT BE READ IS A LOOK THAT DID NOT READ EVERYTHING, whose listing can
    /// come back as it was. NEGATIVE CONTROL: the look called itself whole with a day's rollouts
    /// out of its listing, so a row read `usable` past the wall they hold.
    #[cfg(unix)]
    #[test]
    fn a_folder_that_could_not_be_read_is_a_look_that_is_not_whole() {
        use std::os::unix::fs::PermissionsExt as _;

        let (home, day) = rollout_home();
        let now = T0 / 1000;
        let week = now + 300_000;
        let a = key(LOGIN_A).expect("a key");
        let wall = rate(T0 - 60_000, "codex", None, Some((100.0, 10_080, week)));
        lay_rollout(&day, 0, now - 60, std::slice::from_ref(&wall));
        let set_mode = |mode: u32| {
            std::fs::set_permissions(&day, std::fs::Permissions::from_mode(mode)).expect("chmod");
        };
        set_mode(0o000);
        if std::fs::read_dir(&day).is_ok() {
            // A user whom a mode does not refuse (root) cannot stage this.
            set_mode(0o755);
            return;
        }
        let homes = [home.path()];
        let look = login_look(&homes, a, None, now);
        set_mode(0o755);
        assert_eq!((look.sample, look.whole), (None, false));
        assert!(
            login_look(&homes, a, None, now).whole,
            "CONTROL: it reads again"
        );
    }

    /// A LOOK THAT COULD NOT OPEN A ROLLOUT IS NOT REMEMBERED: nothing listed changes when that
    /// rollout can be read again, so a remembered look would hide its wall until another write, up
    /// to a week. NEGATIVE CONTROL: the second look answered from the first and said no wall.
    #[cfg(unix)]
    #[test]
    fn a_look_that_could_not_open_a_rollout_is_not_remembered() {
        use std::os::unix::fs::PermissionsExt as _;

        let (home, day) = rollout_home();
        let now = T0 / 1000;
        let week = now + 300_000;
        let homes = [home.path()];
        let a = key(LOGIN_A).expect("a key");
        let wall = rate(T0 - 60_000, "codex", None, Some((100.0, 10_080, week)));
        lay_rollout(&day, 0, now - 60, std::slice::from_ref(&wall));
        let path = std::fs::read_dir(&day)
            .expect("its day")
            .next()
            .expect("one rollout")
            .expect("an entry")
            .path();
        let set_mode = |mode: u32| {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod");
        };
        set_mode(0o000);
        if crate::harness::footer::open_regular(&path).is_some() {
            // A user whom a mode does not refuse (root) cannot stage this.
            set_mode(0o600);
            return;
        }
        let look = login_look(&homes, a, None, now);
        assert_eq!(
            (look.sample, look.whole),
            (None, false),
            "CONTROL: it cannot be read, and says so"
        );
        set_mode(0o600);
        assert_eq!(
            login_sample(&homes, a, None, now).and_then(|s| s.wall),
            Some(LoginWall {
                at: now - 60,
                until: week
            }),
            "nothing listed changed, and it is read now"
        );
    }

    /// A LOOK AT ROLLOUTS THAT HAVE NOT CHANGED READS NONE OF THEM, since a node's metrics pass
    /// surveys every minute with nobody asking and a look can reach [`LOGIN_SCAN`] of them: new bytes
    /// under the size and write time it last saw stay unread. NEGATIVE CONTROL: with no memory of
    /// the last look, the second reads 60. CONTROL: another `since`, a later write, or another
    /// rollout in reach is a look of its own.
    #[test]
    fn a_logins_look_reads_nothing_while_its_rollouts_stand_still() {
        let (home, day) = rollout_home();
        let now = T0 / 1000;
        let week = now + 300_000;
        let homes = [home.path()];
        let a = key(LOGIN_A).expect("a key");
        let reading = |pct: f64| rate(T0 - 60_000, "codex", None, Some((pct, 10_080, week)));
        let week_of = |since: Option<i64>| {
            login_sample(&homes, a, since, now).and_then(|s| s.sample.week_pct)
        };

        lay_rollout(&day, 0, now - 60, &[reading(40.0)]);
        assert_eq!(week_of(None), Some(40));
        lay_rollout(&day, 0, now - 60, &[reading(60.0)]);
        assert_eq!(
            week_of(None),
            Some(40),
            "unchanged by its size and write time"
        );
        assert_eq!(
            week_of(Some(now - 600)),
            Some(60),
            "CONTROL: another `since` looks again"
        );
        lay_rollout(&day, 0, now - 60, &[reading(70.0)]);
        lay_rollout(&day, 0, now - 59, &[reading(70.0)]);
        assert_eq!(
            week_of(Some(now - 600)),
            Some(70),
            "CONTROL: a later write looks again"
        );
        lay_rollout(&day, 0, now - 59, &[reading(80.0)]);
        lay_rollout(&day, 1, now - 300, &[reading(10.0)]);
        assert_eq!(
            week_of(Some(now - 600)),
            Some(80),
            "CONTROL: another rollout in reach looks again"
        );
    }

    /// A ROLLOUT LAST WRITTEN BEFORE `since` IS NOT READ: a reading is stamped before it is written,
    /// so it holds none from `since` on, and a healthy row pays for no older tail. The first rollout
    /// here holds a stamp no real one can, a reading newer than its last write, so that reading it
    /// would show. NEGATIVE CONTROL: the look reached back a week whatever `since` said, so up to
    /// [`LOGIN_SCAN`] older rollouts each paid a first line and a tail, and none could be counted.
    #[test]
    fn a_logins_look_reaches_back_only_to_since() {
        let (home, day) = rollout_home();
        let now = T0 / 1000;
        let since = now - 600;
        let week = now + 300_000;
        let homes = [home.path()];
        let a = key(LOGIN_A).expect("a key");
        let stamped = rate(since * 1000, "codex", None, Some((100.0, 10_080, week)));
        let shown = Some(LoginWall {
            at: since,
            until: week,
        });
        let wall = |from: Option<i64>| login_sample(&homes, a, from, now).and_then(|s| s.wall);
        lay_rollout(&day, 0, since - 1, std::slice::from_ref(&stamped));
        assert_eq!(
            login_sample(&homes, a, Some(since), now),
            None,
            "last written the second before `since`: its tail is not read"
        );
        assert_eq!(wall(None), shown, "CONTROL: with no `since`, it is read");
        assert_eq!(
            wall(Some(since - 1)),
            shown,
            "CONTROL: last written in the very second of `since`, it is read"
        );
        lay_rollout(
            &day,
            0,
            now - LOGIN_REACH_S - 1,
            std::slice::from_ref(&stamped),
        );
        assert_eq!(
            login_sample(&homes, a, Some(now - 2 * LOGIN_REACH_S), now),
            None,
            "a `since` older than a week does not widen the week's reach"
        );

        let (home, day) = rollout_home();
        let homes = [home.path()];
        let at_ms = T0 - 60_000;
        let older = rate(
            (since - 120) * 1000,
            "codex",
            None,
            Some((40.0, 10_080, week)),
        );
        for n in 1..=NEWEST_READ * 2 {
            lay_rollout(&day, n, since - 60, std::slice::from_ref(&older));
        }
        let full = rate(at_ms, "codex", None, Some((100.0, 10_080, week)));
        lay_rollout(&day, 0, now - 60, std::slice::from_ref(&full));
        assert_eq!(
            login_sample(&homes, a, Some(since), now).and_then(|s| s.wall),
            Some(LoginWall {
                at: at_ms / 1000,
                until: week
            }),
            "CONTROL: the wall from `since` on is still found"
        );
    }

    /// A SURVEY NEVER READS WITHOUT LIMIT: it looks through [`LOGIN_SCAN`] rollouts written last, of
    /// any login, and a wall behind that many is out of view.
    #[test]
    fn a_login_scan_stops_at_its_bound() {
        let (home, day) = rollout_home();
        let now = T0 / 1000;
        let week = now + 300_000;
        let homes = [home.path()];
        let a = key(LOGIN_A).expect("a key");
        let wall = rate(T0 - 3_600_000, "codex", None, Some((100.0, 10_080, week)));
        lay_rollout(&day, 0, now - 3_600, std::slice::from_ref(&wall));
        for n in 1..LOGIN_SCAN {
            lay_rollout(&day, n, now - 60, &[]);
        }
        assert_eq!(
            login_sample(&homes, a, None, now).and_then(|s| s.sample.week_pct),
            Some(100),
            "the last rollout it looks through"
        );
        lay_rollout(&day, LOGIN_SCAN, now - 60, &[]);
        assert_eq!(
            login_sample(&homes, a, None, now),
            None,
            "the first it does not"
        );
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

        let escaped_sub = "01900000-0000-7000-8000-000000000009";
        let escaped_root = format!("\\u0030{}", &ROOT[1..]);
        std::fs::write(
            day.join(format!("rollout-2026-10-16T09-30-00-{escaped_sub}.jsonl")),
            format!(
                "{{\"timestamp\":\"2026-08-01T17:36:00.000Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"{escaped_sub}\",\"session_id\":\"{escaped_root}\",\"history_mode\":\"paginated\"}}}}\n{}",
                rate(now_ms - 20_000, "codex", None, Some((42.0, 10_080, week)))
            ),
        )
        .expect("write escaped sub");
        let sample =
            thread_sample(home.path(), ROOT, started_unix, now_unix).expect("escaped sub sample");
        assert_eq!(sample.week_pct, Some(42));

        let inf_line = format!(
            "{{\"timestamp\":\"2026-08-01T17:36:39.000Z\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\",\"rate_limits\":{{\"limit_id\":\"codex\",\"primary\":{{\"used_percent\":1e400,\"window_minutes\":300,\"resets_at\":{week}}}}}}}}}"
        );
        assert_eq!(
            limits_line(&inf_line),
            None,
            "overflowed JSON float 1e400 must be rejected"
        );
        let str_inf_line = format!(
            "{{\"timestamp\":\"2026-08-01T17:36:39.000Z\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\",\"rate_limits\":{{\"limit_id\":\"codex\",\"individual_limit\":\"Infinity\",\"primary\":{{\"used_percent\":25.0,\"window_minutes\":300,\"resets_at\":{week}}}}}}}}}"
        );
        let parsed = limits_line(&str_inf_line).expect("token_count with string limit parses");
        assert_eq!(parsed.individual_limit, None);
    }
}
