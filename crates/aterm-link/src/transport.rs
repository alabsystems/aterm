// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! How the bridge reaches the broker: its Unix socket, behind one erased stream type.

use std::io;
use std::time::{Duration, Instant};

use astream_broker::Record;

/// Any byte stream a broker connection can ride.
pub use astream_broker::AnyStream as Stream;

/// Ends a subscription from outside its reading thread, and bounds a connection's reads and writes.
pub use astream_broker::Closer;

/// One broker connection with its transport erased: one code path per verb, not one per wire.
pub type Conn = astream_broker::AnyClient;

/// Attach a whole capability ring to `conn` under one `Hello` with astream's [`attach_all`](astream_broker::Client::attach_all).
///
/// # Errors
/// The socket, or the broker's first refusal in ring order (a [`astream_broker::BrokerRefusal`] inside).
pub fn attach_ring(conn: &mut Conn, ring: &[crate::bridge::Cap]) -> io::Result<()> {
    conn.attach_all(
        ring.iter()
            .map(|cap| (cap.grant.as_str(), cap.tag.as_slice())),
    )
    .map(drop)
}

/// [`attach_ring`] over every capability in `cap_files`, in file order.
///
/// # Errors
/// A cap file that cannot be read, or [`attach_ring`]'s.
pub fn attach_cap_files(conn: &mut Conn, cap_files: &[String]) -> io::Result<()> {
    let mut ring = Vec::new();
    for path in cap_files {
        ring.extend(crate::bridge::read_cap_file(path)?);
    }
    attach_ring(conn, &ring)
}

/// Walk every row of a last-value face, paging on the resume cursor ([`astream_broker::Client::last_walk`]).
///
/// # Errors
/// A broker or transport failure, a failing `on_row`, or a walk that did not end within [`astream_broker::LAST_WALK_PAGES_MAX`] pages.
pub fn walk_last<F>(conn: &mut Conn, filter: &str, mut on_row: F) -> io::Result<()>
where
    F: FnMut(&Record) -> io::Result<()>,
{
    let (_, rest) = conn.last_walk(filter, "", u32::MAX, |row| {
        on_row(&row)?;
        Ok(astream_broker::Walk::Continue)
    })?;
    match rest {
        None => Ok(()),
        Some(_) => Err(io::Error::other(format!(
            "last-value walk of {filter} did not end within {} rows",
            u32::MAX
        ))),
    }
}

/// The floor a read was told.
#[must_use]
pub fn retained_past(e: &io::Error, from: u64) -> Option<u64> {
    astream_broker::broker_refusal(e)
        .and_then(astream_broker::BrokerRefusal::retained_from)
        .filter(|oldest| *oldest > from)
}

/// What a tail's gap notice ([`astream_broker::Event::Gap`]) says retention gave up, for a log line.
#[must_use]
pub fn gap_lost(gap: &astream_broker::Gap) -> String {
    match gap.records {
        Some(n) => format!("{n} record(s) in @{}..@{}", gap.from, gap.to),
        None => format!(
            "an uncounted number of records in @{}..@{}",
            gap.from, gap.to
        ),
    }
}

/// How many times [`fetch_kept`] follows a floor that rose under it.
pub const KEPT_TRIES: usize = 4;

/// A `Fetch` of `filter` from `from` that READS ON AT THE FLOOR.
///
/// # Errors
/// The broker's or the transport's, as `fetch`'s; a refusal past [`KEPT_TRIES`] floors.
pub fn fetch_kept<S: io::Read + io::Write>(
    conn: &mut astream_broker::Client<S>,
    from: u64,
    filter: &str,
    max: u32,
) -> io::Result<(astream_broker::Page, Option<u64>)> {
    let mut at = from;
    let mut tries = 0usize;
    loop {
        match conn.fetch(at, filter, max) {
            Ok(page) => return Ok((page, (at != from).then_some(at))),
            Err(e) => match retained_past(&e, at) {
                Some(floor) if tries < KEPT_TRIES => {
                    tries = tries.saturating_add(1);
                    at = floor;
                }
                _ => return Err(e),
            },
        }
    }
}

/// [`fetch_kept`] FOR A READER TOLD WHAT RETENTION GAVE UP (`Client::fetch_told`, which asks
/// for gap notices on `conn` first, once): below the filter's oldest it reads the records
/// retention still holds there too, as a tail does (the oldest is where the filter is
/// complete, astream's `retention::oldest`, and records some subject of it still holds can
/// lie below it), and reads on at the floor only where the filter holds none. The page's gap
/// notice is not kept: what it tells (every record of the filter the page skips below the
/// notice's `to` was given up) is what the read already is. Against a broker that does not
/// tell fetches (`Client::supports_told_fetch`) it reads as [`fetch_kept`] does.
///
/// # Errors
/// As [`fetch_kept`]'s, and the gap-notice ask's.
pub fn fetch_told_kept<S: io::Read + io::Write>(
    conn: &mut astream_broker::Client<S>,
    from: u64,
    filter: &str,
    max: u32,
) -> io::Result<(astream_broker::Page, Option<u64>)> {
    let mut at = from;
    let mut tries = 0usize;
    loop {
        match conn.fetch_told(at, filter, max) {
            Ok((page, _)) => return Ok((page, (at != from).then_some(at))),
            Err(e) => match retained_past(&e, at) {
                Some(floor) if tries < KEPT_TRIES => {
                    tries = tries.saturating_add(1);
                    at = floor;
                }
                _ => return Err(e),
            },
        }
    }
}

/// RECORDS ONE `Fetch` PAGE ASKS FOR ON A FACE OF ROWS: presence, `ev`, halts.
pub const ROW_PAGE: u32 = astream_broker::broker::LAST_PAGE_MAX;

/// RECORDS ONE `Fetch` PAGE ASKS FOR ON A FACE THAT CARRIES POSTS — the `in` lanes, the `say` topics, the whole fleet.
pub const POST_PAGE: u32 = 256;

/// Open one connection to the broker's socket at `endpoint` UNDER `deadline`, answering it and the closer that can end it.
///
/// # Errors
/// The connect itself, `TimedOut` past the bound, the descriptor duplication the closer needs, or a zero `deadline` (`InvalidInput`).
pub fn connect(endpoint: &str, deadline: Duration) -> io::Result<(Conn, Closer)> {
    let (conn, closer) =
        astream_broker::connect(&astream_broker::Transport::Unix, endpoint, Some(deadline))?;
    closer.set_read_timeout(Some(deadline))?;
    closer.set_write_timeout(Some(deadline))?;
    Ok((conn, closer))
}

/// The reconnect back-off floor and ceiling.
const RECONNECT_MIN: Duration = Duration::from_millis(100);
pub(crate) const RECONNECT_MAX: Duration = Duration::from_secs(5);

/// How long a link must have stayed up for its loss to restart the back-off from
/// [`RECONNECT_MIN`] and be redialled at once.
const LINK_STABLE: Duration = RECONNECT_MAX;

/// One broker client's reconnect back-off.
#[derive(Debug)]
pub(crate) struct Backoff {
    next: Duration,
    up_since: Option<Instant>,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            next: RECONNECT_MIN,
            up_since: None,
        }
    }
}

impl Backoff {
    /// A dial or attach failed: how long to wait before the next one, which waits twice as long, up to the ceiling.
    pub(crate) fn failed(&mut self) -> Duration {
        self.failed_with(random_u64())
    }

    /// [`Backoff::failed`] with the jitter drawn by the caller: `r`, or none at all (`None`, the whole wait).
    pub(crate) fn failed_with(&mut self, r: Option<u64>) -> Duration {
        let wait = r.map_or(self.next, |r| jittered(self.next, r));
        self.next = (self.next * 2).min(RECONNECT_MAX);
        wait
    }

    /// The link came up at `now`.
    pub(crate) const fn attached(&mut self, now: Instant) {
        self.up_since = Some(now);
    }

    /// The link that came up was lost at `now`.
    pub(crate) fn lost(&mut self, now: Instant) -> Option<Duration> {
        self.lost_with(now, random_u64())
    }

    /// [`Backoff::lost`] with the jitter drawn by the caller ([`Backoff::failed_with`]).
    pub(crate) fn lost_with(&mut self, now: Instant, r: Option<u64>) -> Option<Duration> {
        let up = self.up_since.take()?;
        if now.saturating_duration_since(up) >= LINK_STABLE {
            self.next = RECONNECT_MIN;
            None
        } else {
            Some(self.failed_with(r))
        }
    }
}

/// The most distinct errors one [`Outage`] says; past them its failures say nothing more
/// until it attaches.
pub(crate) const OUTAGE_SAID_MAX: usize = 8;

/// A LONG-LIVED FOLLOWER'S BROKER OUTAGE, SAID ONCE (fix unit E5, E2E-15): what a reader that
/// redials on its [`Backoff`] says on stderr, held per reader thread. The first failure of an
/// outage says the error and that it retries, every [`RECONNECT_MAX`] at most, until it
/// attaches; a failure with an error this outage already said (the same kind and OS error),
/// however the errors alternate, says nothing, and each other error one more line, up to
/// [`OUTAGE_SAID_MAX`] of them; the attach after a failure says how many tries failed and
/// over how long.
#[derive(Debug, Default)]
pub(crate) struct Outage {
    /// When the outage's first failure was, while one is under way.
    since: Option<Instant>,
    /// The failures since then.
    tries: u64,
    /// The errors this outage's lines said, each its kind and OS error, oldest first.
    said: Vec<(io::ErrorKind, Option<i32>)>,
}

impl Outage {
    /// A dial or a subscription failed at `now` with `e`: the line to say (after the reader's
    /// own `<who> `), or `None` when it would repeat the last.
    pub(crate) fn failed(&mut self, now: Instant, e: &io::Error) -> Option<String> {
        self.tries = self.tries.saturating_add(1);
        let first = self.since.is_none();
        if first {
            self.since = Some(now);
        }
        let what = (e.kind(), e.raw_os_error());
        if self.said.contains(&what) || self.said.len() >= OUTAGE_SAID_MAX {
            return None;
        }
        self.said.push(what);
        Some(if first {
            format!(
                "could not subscribe: {e}; it retries, every {} s at most, until it attaches, \
                 and says when it does",
                RECONNECT_MAX.as_secs()
            )
        } else {
            format!("could not subscribe: {e} (still retrying)")
        })
    }

    /// The subscription is live again at `now`: the line saying so after an outage, which ends.
    pub(crate) fn attached(&mut self, now: Instant) -> Option<String> {
        let since = self.since.take()?;
        let tries = std::mem::take(&mut self.tries);
        self.said.clear();
        Some(format!(
            "subscribed again after {tries} failed tries over {} s",
            now.saturating_duration_since(since).as_secs()
        ))
    }
}

/// `b` jittered into `[b/2, b]` by `r`: integer-only, and never longer than
/// `b`.
fn jittered(b: Duration, r: u64) -> Duration {
    let half = b / 2;
    let span = u64::try_from((b - half).as_micros()).unwrap_or(u64::MAX);
    half + Duration::from_micros(r % span.saturating_add(1))
}

/// A jitter draw from the OS CSPRNG; `None` when it cannot be read, and the caller then waits the whole `b` — never a shorter wait.
pub(crate) fn random_u64() -> Option<u64> {
    let mut raw = [0u8; 8];
    aterm_uds::rand::fill(&mut raw).ok()?;
    Some(u64::from_le_bytes(raw))
}

/// Format an error from [`read_mint_secret`] as text naming `path` once.
#[must_use]
pub fn describe(path: &str, e: &io::Error) -> String {
    if e.raw_os_error().is_some() {
        format!("{path}: {e}")
    } else {
        e.to_string()
    }
}

/// THE MINT SECRET READER: astream's [`astream_cap::capfile::read_secret_at`], every refusal
/// naming `path` exactly as given (a bare name too, never `./<name>`).
///
/// # Errors
/// A missing file or directory (`NotFound`, which a caller may answer with a hint).
pub fn read_mint_secret(path: &str) -> io::Result<Vec<u8>> {
    astream_cap::capfile::read_secret_at(path)
}

/// [`read_mint_secret`] as it reads once the secret's directory is 0700
/// ([`astream_cap::capfile::read_secret_at_narrowed`]): `fabric on --dry-run`'s reader, since the
/// real run narrows the root before it reads the secret.
///
/// # Errors
/// [`read_mint_secret`]'s, but for its directory's group or other mode bits.
pub fn read_mint_secret_narrowed(path: &str) -> io::Result<Vec<u8>> {
    astream_cap::capfile::read_secret_at_narrowed(path)
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;
