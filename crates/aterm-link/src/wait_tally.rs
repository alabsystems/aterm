// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BROKER-WAIT TALLY: how long the bridge's one loop spends blocked on the
//! broker's replies, counted where it blocks — a development seam the fabric
//! bench's ping-pong arm reads (`crates/aterm/tests/fabric_bench/run/pingpong.rs`).
//!
//! WHY IT IS COUNTED IN HERE. The bridge is one thread. While it waits for a
//! group commit's answer or a publish's ack it does nothing else: a reply a
//! session posted the moment its message arrived usually sits in aterm's
//! outbox until the commit for that very delivery comes back. (Not always: the
//! roster round at the top of a loop's pass reads the outbox too,
//! `Bridge::roster_backstop`, before that pass commits, and a reply it finds
//! there is published ahead of the commit.) From outside, a bench sees only
//! the round trip; how much of it is the loop parked on the broker is a fact
//! only this process has. So the loop counts it, at the two places every wait
//! passes through:
//!
//! - every request that reads its own reply — a `Fetch`, a `Last` page, a
//!   `Commit`, the `Will`, a `Budget`, a blocking `Publish` — hands its answer
//!   to `Bridge::observe`, which notes [`Recorder::request`] from the moment
//!   the request was written to the moment its reply was read, and the group
//!   commit, one of them, is noted again apart ([`Recorder::commit`]). (A
//!   `Last` walk the bridge gives up on for running past its bound hands
//!   `observe` an error of its own making: one more request, and no wait.)
//! - every PIPELINED publish's ack is read in `Bridge::settle_one`, which
//!   notes [`Recorder::ack`] around the read alone: an ack that was already
//!   waiting in the socket costs what the read costs, not the time since its
//!   publish was sent, which the loop spent working.
//!
//! What is NOT a wait on the broker is not counted: writing a pipelined
//! publish, the verb lane's round trips to aterm (`deliver`, the outbox), the
//! state-dir writes, and the dial. The tally says how much of the loop's time
//! the broker held it; the rest of a round trip is the loop's own work, aterm's
//! and the scheduler's.
//!
//! HOW IT LEAVES THE PROCESS. With [`SEAM`] set in a build that reads seams,
//! the bridge opens `<state>/`[`FILE`] at its start and, at each idle moment —
//! its loop found nothing queued and has read the acks it owed
//! (`Bridge::settle_owed`) — when the tally has moved since the last write,
//! writes the tally there ([`Recorder::idle`]): one positioned write of one
//! line of constant length at offset 0 — no temp, no rename, no flush. The line
//! carries a checksum of itself ([`Tally::parse`]), so a reader that raced a
//! write reads a line that does not parse, never a count that is half old and
//! half new.
//!
//! The line also names this process's pid, and the inbox group cursor the loop
//! last committed: a reader waits for a tally that has committed past the
//! message it just saw (the loop's idle write after that commit), and refuses
//! a pair of tallies from two processes ([`Tally::since`]) — a relaunched
//! bridge starts again from zero.
//!
//! WHAT IT COSTS THE LOOP IT MEASURES. The loop the arm measures is NOT the
//! loop that runs without the seam: it is that loop plus this, small beside a
//! broker round trip (a Strict commit is an fsync, milliseconds), and not
//! nothing.
//!
//! - Per request that reads its own reply: one clock read and two additions
//!   ([`Recorder::request`]; the `Instant` at the write is read with or
//!   without the seam, for the link's own round-trip note). Per group commit,
//!   one clock read and two additions more ([`Recorder::commit`]).
//! - Per settle of the owed pipelined acks (`Bridge::settle_acks`, run before
//!   each request on the publisher connection that reads its own reply,
//!   `Bridge::conn_now`, at each idle moment, and before an inbox commit a
//!   claim rides on): one clock read before each look for an owed ack
//!   ([`Recorder::ack_starts`]) — including the last look, which finds none
//!   owed and reads nothing — and, per ack read, one clock read and two
//!   additions after it ([`Recorder::ack`]). So two clock reads per ack, and
//!   one more per settle, owed or not. (The settle's own `Instant`, for the
//!   link's round-trip note, is read with or without the seam.)
//! - Per idle moment: one comparison, and when the tally (cursor included)
//!   differs from the one last written, rendering the line, hashing it, and
//!   one `pwrite` ([`Recorder::idle`]). The loop judges the moment idle BEFORE
//!   it settles its owed acks, and that settle blocks on each ack still in
//!   flight (an fsync, on Strict), so an item queued during it is already
//!   waiting when the write runs, and waits for it — as does one that arrives
//!   while it runs. On the ping-pong arm's own path that is expected, not
//!   rare: the bridge publishes a message, finds nothing queued and blocks on
//!   the message's ack; on Strict the message turns visible at that same
//!   fsync, so its delivery — the one the arm times — can be queued before the
//!   settle returns, and then waits behind the write that ack just moved.
//!
//! A RELEASED BINARY HAS NO TALLY. `dev_seam!` is `None` there, the recorder is
//! off, and every note below is one branch on that.

use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

/// The development seam that turns the tally on: any value, in a build that
/// reads seams (a debug build, or `--features dev-seams`). Inherited by the
/// bridge from the aterm that launches it (`aterm-gui`'s `fabric_launch.rs`).
pub const SEAM: &str = "ATERM_LINK_WAIT_TALLY";

/// The file in the bridge's state dir the tally is written to.
pub const FILE: &str = "wait-tally";

/// What the line starts with: the format's name and version.
const HEAD: &str = "wait-tally v=1";

/// The loop's waits on the broker since this process started, and where its
/// inbox group cursor stood at the write. Every time is in nanoseconds of
/// `Instant`, the monotonic clock: an ack already in the socket reads in well
/// under a microsecond, and microseconds would round thousands of them to zero.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    /// The bridge process the counts are of.
    pub pid: u32,
    /// Requests that read their own reply, and the time spent waiting for it.
    pub requests: u64,
    pub request_ns: u64,
    /// Of those, the inbox group's commits.
    pub commits: u64,
    pub commit_ns: u64,
    /// Pipelined publishes' acks read, and the time the reads blocked.
    pub acks: u64,
    pub ack_ns: u64,
    /// The inbox group cursor this process last committed (0 before any).
    pub committed: u64,
}

impl Tally {
    /// Every wait on the broker, requests and acks: the time the loop was held.
    #[must_use]
    pub fn blocked_ns(&self) -> u64 {
        self.request_ns.saturating_add(self.ack_ns)
    }

    /// The line [`Tally::parse`] reads, newline included. Every number is
    /// zero-padded to its type's widest, so every line is the same length and a
    /// write at offset 0 replaces the last one whole. Pure (unit-tested).
    #[must_use]
    pub fn render(&self) -> String {
        let body = format!(
            "{HEAD} pid={:010} requests={:020} request_ns={:020} commits={:020} \
             commit_ns={:020} acks={:020} ack_ns={:020} committed={:020}",
            self.pid,
            self.requests,
            self.request_ns,
            self.commits,
            self.commit_ns,
            self.acks,
            self.ack_ns,
            self.committed
        );
        let sum = aterm_hash::fnv1a_64(body.as_bytes());
        format!("{body} sum={sum:016x}\n")
    }

    /// Read a line [`Tally::render`] wrote. Pure (unit-tested).
    ///
    /// # Errors
    ///
    /// A line that is not one: another format or version, a field missing, out
    /// of order or not a number, or a checksum that does not match — which is
    /// what a read that raced the write sees.
    pub fn parse(text: &str) -> Result<Tally, String> {
        let line = text
            .strip_suffix('\n')
            .ok_or_else(|| format!("an unterminated tally line: {text:?}"))?;
        let (body, sum) = line
            .rsplit_once(" sum=")
            .ok_or_else(|| format!("a tally line with no checksum: {line:?}"))?;
        let want = format!("{:016x}", aterm_hash::fnv1a_64(body.as_bytes()));
        if sum != want {
            return Err(format!(
                "a tally line whose checksum is {sum}, not {want} (a read that raced the write, \
                 or not a tally): {line:?}"
            ));
        }
        let rest = body
            .strip_prefix(HEAD)
            .and_then(|rest| rest.strip_prefix(' '))
            .ok_or_else(|| format!("not a `{HEAD}` line: {line:?}"))?;
        let mut words = rest.split(' ');
        let mut field = |key: &str| -> Result<u64, String> {
            words
                .next()
                .and_then(|w| w.strip_prefix(key))
                .and_then(|w| w.strip_prefix('='))
                .and_then(|v| v.parse::<u64>().ok())
                .ok_or_else(|| {
                    format!("a tally line with no `{key}=<number>` in its place: {line:?}")
                })
        };
        let pid = u32::try_from(field("pid")?)
            .map_err(|_| format!("a tally line whose pid is not a pid: {line:?}"))?;
        let tally = Tally {
            pid,
            requests: field("requests")?,
            request_ns: field("request_ns")?,
            commits: field("commits")?,
            commit_ns: field("commit_ns")?,
            acks: field("acks")?,
            ack_ns: field("ack_ns")?,
            committed: field("committed")?,
        };
        if words.next().is_some() {
            return Err(format!("a tally line with words past its fields: {line:?}"));
        }
        Ok(tally)
    }

    /// What the loop waited on between `earlier` and this tally: each count's
    /// difference, and this tally's cursor. Pure (unit-tested).
    ///
    /// # Errors
    ///
    /// Two processes' tallies (a relaunched bridge counts from zero again), or
    /// a count that went backwards.
    pub fn since(&self, earlier: &Tally) -> Result<Tally, String> {
        if self.pid != earlier.pid {
            return Err(format!(
                "the tallies are of two bridge processes, pid {} and then {}: the bridge was \
                 relaunched between them",
                earlier.pid, self.pid
            ));
        }
        let pairs = [
            ("requests", self.requests, earlier.requests),
            ("request_ns", self.request_ns, earlier.request_ns),
            ("commits", self.commits, earlier.commits),
            ("commit_ns", self.commit_ns, earlier.commit_ns),
            ("acks", self.acks, earlier.acks),
            ("ack_ns", self.ack_ns, earlier.ack_ns),
        ];
        let mut d = [0u64; 6];
        for (slot, (key, now, then)) in d.iter_mut().zip(pairs) {
            *slot = now
                .checked_sub(then)
                .ok_or_else(|| format!("the tally's `{key}` went backwards, {then} to {now}"))?;
        }
        Ok(Tally {
            pid: self.pid,
            requests: d[0],
            request_ns: d[1],
            commits: d[2],
            commit_ns: d[3],
            acks: d[4],
            ack_ns: d[5],
            committed: self.committed,
        })
    }
}

/// `d` in nanoseconds, saturating.
fn nanos(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

/// The bridge's side: counts while it is on, and writes at idle moments.
#[derive(Debug, Default)]
pub(crate) struct Recorder {
    /// The tally file, open for the life of the process; `None` is off.
    out: Option<std::fs::File>,
    tally: Tally,
    /// What the file holds: the last tally written.
    written: Option<Tally>,
}

impl Recorder {
    /// On when [`SEAM`] is set in a build that reads seams, writing to
    /// `<state>/`[`FILE`]; off otherwise, and off — said once, on stderr — when
    /// the file cannot be opened.
    pub(crate) fn from_env(state_root: &Path) -> Self {
        if aterm_types::dev_seam!(SEAM).is_none() {
            return Self::default();
        }
        let path = state_root.join(FILE);
        Self::at(&path).unwrap_or_else(|e| {
            eprintln!(
                "aterm-link: {SEAM} is set, but {} cannot be written ({e}); no wait tally",
                path.display()
            );
            Self::default()
        })
    }

    /// On, writing to a newly created private `path`.
    /// An occupied name, including an old tally or a link, is left untouched;
    /// the optional recorder stays off rather than adopting it.
    ///
    /// # Errors
    ///
    /// The exclusive open's, including an already occupied name.
    pub(crate) fn at(path: &Path) -> io::Result<Self> {
        use std::os::unix::fs::OpenOptionsExt as _;
        let out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        Ok(Self {
            out: Some(out),
            tally: Tally {
                pid: std::process::id(),
                ..Tally::default()
            },
            written: None,
        })
    }

    /// Whether anything is counted.
    pub(crate) fn on(&self) -> bool {
        self.out.is_some()
    }

    /// A request written at `started` has had its reply read now.
    pub(crate) fn request(&mut self, started: Instant) {
        if self.on() {
            self.tally.requests += 1;
            self.tally.request_ns = self
                .tally
                .request_ns
                .saturating_add(nanos(started.elapsed()));
        }
    }

    /// A group commit written at `started` has had its reply read now — a
    /// request [`Recorder::request`] counts as well, timed at the same reply.
    pub(crate) fn commit(&mut self, started: Instant) {
        if self.on() {
            self.tally.commits += 1;
            self.tally.commit_ns = self
                .tally
                .commit_ns
                .saturating_add(nanos(started.elapsed()));
        }
    }

    /// When an ack read is about to start: `None` while off, so a released
    /// bridge reads no clock for it.
    pub(crate) fn ack_starts(&self) -> Option<Instant> {
        self.on().then(Instant::now)
    }

    /// The ack read that [`Recorder::ack_starts`] began has returned.
    pub(crate) fn ack(&mut self, started: Option<Instant>) {
        if let Some(started) = started {
            self.tally.acks += 1;
            self.tally.ack_ns = self.tally.ack_ns.saturating_add(nanos(started.elapsed()));
        }
    }

    /// An idle moment — the loop found nothing queued and has since read the
    /// acks it owed, during which an item may have arrived — its inbox group
    /// cursor at `committed`: write the tally if it moved since the last
    /// write. A write that fails is said on stderr and turns the tally off, so
    /// a full disk costs one line rather than one per round.
    pub(crate) fn idle(&mut self, committed: u64) {
        use std::os::unix::fs::FileExt as _;
        let Some(out) = &self.out else {
            return;
        };
        self.tally.committed = committed;
        if self.written == Some(self.tally) {
            return;
        }
        match out.write_all_at(self.tally.render().as_bytes(), 0) {
            Ok(()) => self.written = Some(self.tally),
            Err(e) => {
                eprintln!("aterm-link: the wait tally could not be written ({e}); no wait tally");
                self.out = None;
            }
        }
    }

    /// The counts so far.
    #[cfg(test)]
    pub(crate) fn tally(&self) -> Tally {
        self.tally
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Tally {
        Tally {
            pid: 4242,
            requests: 7,
            request_ns: 31_000_000,
            commits: 3,
            commit_ns: 22_000_000,
            acks: 5,
            ack_ns: 18_000_000,
            committed: 119,
        }
    }

    #[test]
    fn a_tally_line_reads_back_as_written_and_every_line_is_one_length() {
        let t = sample();
        assert_eq!(Tally::parse(&t.render()), Ok(t));
        assert_eq!(t.blocked_ns(), 49_000_000);
        // ONE LENGTH, so a write at offset 0 always replaces the line whole:
        // the smallest tally and the largest render to the same width.
        let zero = Tally::default();
        let full = Tally {
            pid: u32::MAX,
            requests: u64::MAX,
            request_ns: u64::MAX,
            commits: u64::MAX,
            commit_ns: u64::MAX,
            acks: u64::MAX,
            ack_ns: u64::MAX,
            committed: u64::MAX,
        };
        assert_eq!(zero.render().len(), full.render().len());
        assert_eq!(Tally::parse(&full.render()), Ok(full));
        assert_eq!(full.blocked_ns(), u64::MAX, "saturates");
    }

    #[test]
    fn a_line_that_raced_its_write_is_refused_never_read_as_a_count() {
        let old = sample().render();
        let new = Tally {
            requests: 8,
            request_ns: 35_000_000,
            committed: 121,
            ..sample()
        }
        .render();
        assert_eq!(old.len(), new.len());
        // A read that took the new line's head and the old line's tail, at
        // every split point: a mix whose digits differ never parses.
        for cut in 1..old.len() {
            let torn = format!("{}{}", &new[..cut], &old[cut..]);
            if torn != old && torn != new {
                assert!(Tally::parse(&torn).is_err(), "cut {cut}: {torn:?}");
            }
        }
        // NEGATIVE CONTROL: the two whole lines parse.
        assert!(Tally::parse(&old).is_ok() && Tally::parse(&new).is_ok());
        for bad in [
            "",
            "wait-tally v=1\n",
            &old[..old.len() - 1],                            // unterminated
            &old.replace("wait-tally v=1", "wait-tally v=2"), // another version
            &old.replace(" sum=", " sum=0"),                  // a checksum that is not one
        ] {
            assert!(Tally::parse(bad).is_err(), "{bad:?}");
        }
        // A line with the right checksum over the wrong fields is still refused.
        let body = "wait-tally v=1 pid=1 requests=1";
        let short = format!(
            "{body} sum={:016x}\n",
            aterm_hash::fnv1a_64(body.as_bytes())
        );
        assert!(Tally::parse(&short).unwrap_err().contains("request_ns"));
    }

    #[test]
    fn the_difference_of_two_tallies_is_one_processs_and_never_negative() {
        let earlier = sample();
        let later = Tally {
            requests: 10,
            request_ns: 40_000_000,
            commits: 4,
            commit_ns: 29_000_000,
            acks: 9,
            ack_ns: 20_000_000,
            committed: 200,
            ..earlier
        };
        assert_eq!(
            later.since(&earlier),
            Ok(Tally {
                pid: 4242,
                requests: 3,
                request_ns: 9_000_000,
                commits: 1,
                commit_ns: 7_000_000,
                acks: 4,
                ack_ns: 2_000_000,
                committed: 200,
            })
        );
        let relaunched = Tally { pid: 4243, ..later };
        assert!(relaunched
            .since(&earlier)
            .unwrap_err()
            .contains("relaunched"));
        let backwards = Tally { acks: 1, ..later };
        assert!(backwards.since(&earlier).unwrap_err().contains("acks"));
    }

    #[test]
    fn an_off_recorder_counts_nothing_and_an_on_one_writes_only_what_moved() {
        // OFF: nothing counted, no clock read for an ack, nothing written.
        let mut off = Recorder::default();
        off.request(Instant::now());
        off.commit(Instant::now());
        let started = off.ack_starts();
        assert_eq!(started, None);
        off.ack(started);
        off.idle(9);
        assert_eq!(off.tally(), Tally::default());

        let dir = aterm_tempfile::tempdir().expect("scratch");
        let path = dir.path().join(FILE);
        let mut on = Recorder::at(&path).expect("open");
        assert!(on.on());
        let read = || std::fs::read_to_string(&path).expect("read the tally");
        assert_eq!(read(), "", "nothing until the first idle moment");
        let t = Instant::now();
        on.commit(t);
        on.request(t);
        on.request(Instant::now());
        let started = on.ack_starts();
        assert!(started.is_some());
        on.ack(started);
        on.idle(17);
        let written = Tally::parse(&read()).expect("a whole line");
        assert_eq!(
            (written.pid, written.requests, written.commits, written.acks),
            (std::process::id(), 2, 1, 1)
        );
        assert_eq!(written.committed, 17);
        assert!(written.commit_ns <= written.request_ns);

        // AN IDLE MOMENT WITH NOTHING MOVED WRITES NOTHING: the file is
        // overwritten from outside, and the next idle moment leaves it.
        std::fs::write(&path, "sentinel").expect("overwrite");
        on.idle(17);
        assert_eq!(read(), "sentinel");
        // NEGATIVE CONTROL: a moved cursor alone is a change, and is written.
        on.idle(18);
        let again = Tally::parse(&read()).expect("rewritten whole");
        assert_eq!(again.committed, 18);
        assert_eq!(again.since(&written).map(|d| d.requests), Ok(0));
    }

    /// Occupied diagnostic names never authorize truncating their bytes or a
    /// link target. Only the fresh file belongs to this recorder.
    #[cfg(unix)]
    #[test]
    fn a_wait_tally_never_truncates_an_occupied_name_or_its_link_target() {
        let dir = aterm_tempfile::tempdir().expect("exclusive fixture");
        let fresh = dir.path().join("fresh-tally");
        let mut recorder = Recorder::at(&fresh).expect("fresh recorder");
        recorder.idle(7);
        drop(recorder);
        assert_eq!(
            Tally::parse(&std::fs::read_to_string(&fresh).expect("fresh bytes"))
                .expect("fresh line")
                .committed,
            7
        );

        let occupied = dir.path().join("occupied");
        let unrelated = dir.path().join("unrelated");
        let linked = dir.path().join("linked-tally");
        std::fs::write(&occupied, b"previous diagnostic bytes").expect("owned occupied name");
        std::fs::write(&unrelated, b"unrelated owned sentinel").expect("owned sentinel");
        std::os::unix::fs::symlink(&unrelated, &linked).expect("owned link");
        let occupied_refused = Recorder::at(&occupied).is_err();
        let linked_refused = Recorder::at(&linked).is_err();
        assert!(
            occupied_refused
                && linked_refused
                && std::fs::read(&occupied).is_ok_and(|b| b == b"previous diagnostic bytes")
                && std::fs::read(&unrelated).is_ok_and(|b| b == b"unrelated owned sentinel")
                && std::fs::symlink_metadata(&linked).is_ok_and(|m| m.file_type().is_symlink()),
            "WAIT_TALLY_OCCUPIED_NAME_WAS_CLOBBERED"
        );
    }
}
