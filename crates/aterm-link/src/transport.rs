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

/// THE FLOOR A READ WAS TOLD (astream's R4).
#[must_use]
pub fn retained_past(e: &io::Error, from: u64) -> Option<u64> {
    astream_broker::broker_refusal(e)
        .and_then(astream_broker::BrokerRefusal::retained_from)
        .filter(|oldest| *oldest > from)
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

/// The reconnect back-off floor and ceiling. §7: a bridge reconnects with jittered
const RECONNECT_MIN: Duration = Duration::from_millis(100);
pub(crate) const RECONNECT_MAX: Duration = Duration::from_secs(5);

/// How long a link must have stayed up for its loss to restart the back-off from
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
    pub(crate) fn attached(&mut self, now: Instant) {
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

/// `b` jittered into `[b/2, b]` by `r`: integer-only, and never longer than
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

/// THE MINT SECRET READER.
///
/// # Errors
/// A missing file or directory (`NotFound`, which a caller may answer with a hint).
pub fn read_mint_secret(path: &str) -> io::Result<Vec<u8>> {
    let path = std::path::Path::new(path);
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{}: a mint secret's path names a file", path.display()),
        ));
    };
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => std::path::Path::new("."),
    };
    astream_cap::capfile::read_secret_in(dir, name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A connect's deadline bounds both directions: a `hello` nobody answers and a
    #[test]
    fn a_connects_deadline_ends_a_hello_nobody_answers_and_a_publish_nobody_reads() {
        use std::os::unix::net::UnixListener;
        let dir = std::env::temp_dir().join(format!("atlink-deadline-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        let sock = dir.join("s.sock");
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).expect("bind");
        let path = sock.to_str().expect("utf8").to_string();
        let deadline = Duration::from_millis(50);
        for what in ["hello", "publish"] {
            let (mut conn, _closer) = connect(&path, deadline).expect("connect");
            let (peer, _) = listener.accept().expect("accept");
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let served = if what == "hello" {
                    conn.hello().map(|_| ())
                } else {
                    // 256 KiB: the bridge's post cap, far past a Unix
                    conn.publish(1, 1, "/f/t/x", &[0; 256 * 1024]).map(|_| ())
                };
                let _ = tx.send(served);
            });
            let err = rx
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .unwrap_or_else(|_| panic!("a {what} nobody serves outlived the deadline"))
                .expect_err("nobody serves it");
            assert!(
                matches!(
                    err.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ),
                "{what}: {err:?}"
            );
            drop(peer);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A broker that sent a subscription its mail and hung up BEFORE the subscriber
    #[test]
    fn a_subscription_whose_broker_already_hung_up_reads_its_mail_then_the_close() {
        use astream_broker::proto::{
            decode_request, encode_response, read_frame, write_frame, Request, Response,
        };
        use std::os::unix::net::UnixListener;
        let dir = aterm_tempfile::Builder::new()
            .prefix("atlink-hungup")
            .tempdir_in("/tmp")
            .expect("scratch");
        let sock = dir.path().join("s.sock");
        let listener = UnixListener::bind(&sock).expect("bind");
        let (conn, closer) =
            connect(sock.to_str().expect("utf8"), aterm_spec::wait::MUST_HAPPEN).expect("connect");
        let (mut broker, _) = listener.accept().expect("accept");
        let mark = Response::Mark {
            next: 0,
            head: 0,
            resume: String::new(),
            durable: None,
            oldest: None,
        };
        write_frame(&mut broker, &encode_response(&mark)).expect("the idle-marks answer");
        let mut sub = conn.subscribe(0, "/f/f/>").expect("subscribe");
        for want in ["IdleMarks", "Subscribe"] {
            let frame = read_frame(&mut broker).expect("read").expect("a request");
            let got = decode_request(&frame).expect("a request frame");
            assert!(
                matches!(
                    (want, &got),
                    ("IdleMarks", Request::IdleMarks) | ("Subscribe", Request::Subscribe { .. })
                ),
                "{want}: {got:?}"
            );
        }
        let mail = Response::Delivery {
            offset: 7,
            subject: "/f/f/in/p/h-x/n-peer/note".into(),
            body: b"v=1 t=1 text=mail".to_vec(),
        };
        write_frame(&mut broker, &encode_response(&mail)).expect("the mail");
        drop(broker);
        closer
            .set_read_timeout(Some(astream_broker::IDLE_MARK_DEADLINE))
            .expect("a hung-up socket has no read to bound");
        closer.set_read_timeout(None).expect("nor one to lift");
        match sub.recv_event().expect("the mail the broker sent") {
            Some(astream_broker::Event::Delivery {
                offset: 7, body, ..
            }) => {
                assert_eq!(body, b"v=1 t=1 text=mail");
            }
            other => panic!("the mail, not {other:?}"),
        }
        assert!(
            sub.recv_event().expect("then the close").is_none(),
            "the broker hung up"
        );
        assert_eq!(
            closer
                .set_read_timeout(Some(Duration::ZERO))
                .expect_err("a zero bound is still refused")
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }

    /// A READ BELOW A RETAINING BROKER'S FLOOR READS ON FROM IT:
    #[test]
    fn a_fetch_below_the_retained_floor_reads_on_from_it() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("atlink-kept")
            .tempdir_in("/tmp")
            .expect("scratch");
        let sock = dir.path().join("b.sock");
        let broker = crate::test_broker(dir.path().join("b.log")).expect("a broker");
        let _served = broker.serve(&sock).expect("serve the broker");
        let (mut conn, _closer) =
            connect(sock.to_str().expect("utf8"), aterm_spec::wait::MUST_HAPPEN).expect("connect");
        let ev = "/f/f/pub/n-a/s-a/ev";
        let offs: Vec<u64> = (1..=3u64)
            .map(|seq| conn.publish(91, seq, ev, b"v=2 x").expect("publish").0)
            .collect();
        broker.retain_before(offs[1]).expect("the cut");

        let refused = conn
            .fetch(offs[0], ev, 8)
            .expect_err("PRECONDITION: a plain read below the floor is refused");
        assert_eq!(retained_past(&refused, offs[0]), Some(offs[1]));
        assert_eq!(
            retained_past(&refused, offs[1]),
            None,
            "no floor past the read"
        );
        let ((rows, _), floor) = fetch_kept(&mut conn, offs[0], ev, 8).expect("reads on");
        assert_eq!(floor, Some(offs[1]), "the floor it read on from");
        let got: Vec<u64> = rows.iter().map(|r| r.0).collect();
        assert_eq!(got, offs[1..], "the kept records");
        let ((rows, _), floor) = fetch_kept(&mut conn, offs[1], ev, 8).expect("at the floor");
        assert_eq!(
            (rows.len(), floor),
            (2, None),
            "a read at the floor is no read on"
        );
    }

    /// A MINT SECRET IS READ BY ASTREAM'S ONE SECRET-FILE RULE, over its directory
    #[test]
    fn a_mint_secret_is_owner_only_long_enough_and_read_byte_for_byte() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let dir = scratch.path().join("root");
        std::fs::create_dir(&dir).expect("root");
        let mode = |path: &std::path::Path, mode| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod")
        };
        mode(&dir, 0o700);
        let path = dir.join("mint.secret");
        let p = path.to_str().expect("utf8");
        let once = |p: &str, e: &io::Error| {
            let said = describe(p, e);
            assert!(said.starts_with(p), "{said}");
            assert_eq!(said, e.to_string(), "the path is not prefixed twice");
        };
        let absent = read_mint_secret(p).expect_err("absent");
        assert_eq!(absent.kind(), io::ErrorKind::NotFound);
        once(p, &absent);
        let mut bytes = vec![0x5a; 31];
        bytes.push(b'\n');
        std::fs::write(&path, &bytes).expect("write");
        for m in [0o644, 0o640, 0o604] {
            mode(&path, m);
            let e = read_mint_secret(p).expect_err("readable by others");
            assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{m:o}");
            assert!(e.to_string().contains("chmod 600"), "{e}");
            once(p, &e);
        }
        for m in [0o600, 0o400] {
            mode(&path, m);
            assert_eq!(read_mint_secret(p).expect("owner-only, 32 bytes"), bytes);
        }
        mode(&path, 0o600);

        mode(&dir, 0o755);
        let e = read_mint_secret(p).expect_err("a directory open to others");
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        assert!(e.to_string().contains("chmod 700"), "{e}");
        once(p, &e);
        mode(&dir, 0o700);

        let linked = dir.join("linked.secret");
        std::os::unix::fs::symlink(&path, &linked).expect("symlink");
        let l = linked.to_str().expect("utf8");
        let e = read_mint_secret(l).expect_err("a symlink");
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        once(l, &e);
        let second = scratch.path().join("second.secret");
        std::fs::hard_link(&path, &second).expect("hard link");
        let e = read_mint_secret(p).expect_err("a second link");
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        assert!(e.to_string().contains("has 2 links"), "{e}");
        once(p, &e);
        std::fs::remove_file(&second).expect("unlink");
        assert_eq!(read_mint_secret(p).expect("one link again"), bytes);

        std::fs::write(&path, [0x5a; 31]).expect("write");
        let e = read_mint_secret(p).expect_err("short");
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        once(p, &e);
        assert_eq!(crate::enable::SECRET_LEN, 32);
        let os = io::Error::from_raw_os_error(2);
        assert_eq!(
            describe(p, &os),
            format!("{p}: {os}"),
            "an OS error is prefixed"
        );
        let bare = io::Error::from(io::ErrorKind::NotFound);
        assert_eq!(
            describe(p, &bare),
            bare.to_string(),
            "a non-OS error is kept as said"
        );
    }
    /// Equal jitter: every draw lies in `[b/2, b]`, both ends reachable.
    #[test]
    fn a_jittered_wait_lies_in_the_upper_half_of_the_back_off() {
        let b = RECONNECT_MAX;
        assert_eq!(jittered(b, 0), b / 2);
        let span = u64::try_from((b - b / 2).as_micros()).expect("fits");
        assert_eq!(jittered(b, span), b);
        for r in [1, 7, span + 1, u64::MAX / 3, u64::MAX] {
            let d = jittered(b, r);
            assert!(b / 2 <= d && d <= b, "{r}: {d:?}");
        }
    }

    /// A link lost right after it came up is a failed attach (it waits and doubles);
    #[test]
    fn only_a_link_that_stayed_up_resets_the_back_off() {
        let within = |d: Duration, b: Duration| b / 2 <= d && d <= b;
        let t0 = Instant::now();
        let mut b = Backoff::default();
        assert_eq!(b.lost(t0), None);
        b.attached(t0);
        let early = b
            .lost(t0 + Duration::from_millis(1))
            .expect("an early loss waits");
        assert!(within(early, RECONNECT_MIN), "{early:?}");
        let next = b.failed();
        assert!(within(next, RECONNECT_MIN * 2), "it doubled: {next:?}");
        b.attached(t0);
        assert_eq!(b.lost(t0 + LINK_STABLE), None);
        let floor = b.failed();
        assert!(within(floor, RECONNECT_MIN), "{floor:?}");
        let draws: Vec<Duration> = (0..64).map(|_| b.failed()).collect();
        assert!(
            draws[8..].iter().all(|d| within(*d, RECONNECT_MAX)),
            "{draws:?}"
        );
    }
}
