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
#[path = "codex_limits_tests.rs"]
mod tests;
