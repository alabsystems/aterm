// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! How the bridge reaches the broker: its Unix socket, behind one erased
//! stream type.
//!
//! ```text
//! aterm-link serve --broker <socket>
//! ```
//!
//! The bus is one machine's. Reachability is scoped by the socket path's
//! permissions, and nothing crosses a wire: `aterm link broker` serves its
//! Unix socket only. The plaintext `--tcp` wire (§8.6's trusted-network-only
//! setting) went on 2026-10-16: no broker aterm ships served it, and only the
//! test suites reached it.
//!
//! ## No wire between machines
//!
//! The sealed record layer (`--tcp --key-file`, astream's `aead`) was compiled
//! out of every shipped binary and was removed on 2026-10-15 without having
//! shipped, and astream's `handshake` and `identity` transports were never
//! offered: a bus that spans machines needs per-machine identity and a guarded
//! broker, a later design that starts in astream. A machine reads another's
//! sessions through the fleet listener instead (`aterm fleet`, aterm-net's TLS).
//!
//! ## The erased stream and its closer
//!
//! The bridge must end a subscription from another thread — `recv` blocks with
//! no timeout, and a reconnect that left the old group subscription running
//! would deliver every record twice and could walk the commit backwards. So
//! astream's [`astream_broker::connect`] makes a [`astream_broker::Closer`] at
//! connect time from a duplicate of the underlying socket. What stays here is
//! policy: which transports a node may use, and the bound on every connect.

use std::io;
use std::time::{Duration, Instant};

use astream_broker::Record;

/// Any byte stream a broker connection can ride. `Send` is a supertrait because each
/// subscription is pumped by its own thread.
pub use astream_broker::AnyStream as Stream;

/// Ends a subscription from outside its reading thread, and bounds a connection's
/// reads and writes. Past a deadline the connection is out of step (a late reply may
/// still arrive), so astream's client refuses every later request on it
/// (`BrokenPipe`) and the caller reconnects. A subscription wants `None`, since
/// parking in `recv` is its normal state, until the broker's first idle mark arms a
/// liveness bound through this same closer (`Subscription::recv_live`).
///
/// A subscriber re-bounds with [`Closer::set_read_timeout`] directly, `?` and all,
/// even on a connection the broker may ALREADY have hung up (it answered and
/// closed, refused a subscription, or marked a tail and went away). macOS answers
/// `EINVAL` for a socket option on a socket shut down both ways, and astream takes
/// that as nothing to bound when a zero-wait poll of the socket sees both
/// directions ended: the reads after it return what the broker sent, then end of
/// stream. Any other refusal stays the caller's. The cause and its tests are
/// astream's; `a_subscription_whose_broker_already_hung_up_reads_its_mail_then_the_close`
/// holds the pinned astream to it for the re-bounds here.
pub use astream_broker::Closer;

/// One broker connection with its transport erased: one code path per verb, not one
/// per wire.
pub type Conn = astream_broker::AnyClient;

/// Attach a whole capability ring to `conn` under one `Hello` with astream's
/// [`attach_all`](astream_broker::Client::attach_all): two round trips however many
/// grants the ring holds, the `Attach`es pipelined. An empty ring sends nothing (an
/// unguarded broker with no cap files).
///
/// # Errors
///
/// The socket, or the broker's first refusal in ring order (a
/// [`astream_broker::BrokerRefusal`] inside). The grants ahead of the refused one stay
/// attached, so a caller that needs the whole ring drops the connection, as every
/// caller here does.
pub fn attach_ring(conn: &mut Conn, ring: &[crate::bridge::Cap]) -> io::Result<()> {
    conn.attach_all(
        ring.iter()
            .map(|cap| (cap.grant.as_str(), cap.tag.as_slice())),
    )
    .map(drop)
}

/// [`attach_ring`] over every capability in `cap_files`, in file order. The files are
/// read on every call, so a connection opened after a re-mint attaches the new ring.
///
/// # Errors
///
/// A cap file that cannot be read, or [`attach_ring`]'s.
pub fn attach_cap_files(conn: &mut Conn, cap_files: &[String]) -> io::Result<()> {
    let mut ring = Vec::new();
    for path in cap_files {
        ring.extend(crate::bridge::read_cap_file(path)?);
    }
    attach_ring(conn, &ring)
}

/// Walk every row of a last-value face, paging on the resume cursor
/// ([`astream_broker::Client::last_walk`]).
///
/// The broker clamps `max` and bounds the index entries one request visits (§5.2),
/// so a short or empty page is not the end; only an empty `resume` is. `on_row` sees
/// each row as its page arrives, so a fleet-sized answer costs one page of memory;
/// an `on_row` that fails ends the walk.
///
/// # Errors
///
/// A broker or transport failure, a failing `on_row`, or a walk that did not end
/// within [`astream_broker::LAST_WALK_PAGES_MAX`] pages: a caller that cannot tell
/// "no rows" from "stopped looking" would report an empty fleet.
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
        // Only a u32::MAX-row answer ends here; still never a short roster.
        Some(_) => Err(io::Error::other(format!(
            "last-value walk of {filter} did not end within {} rows",
            u32::MAX
        ))),
    }
}

/// RECORDS ONE `Fetch` PAGE ASKS FOR ON A FACE OF ROWS: presence, `ev`, halts —
/// bodies a bridge writes from closed grammars and capped fields (a presence
/// field is at most [`crate::glance::FIELD_CAP`] bytes), a few hundred bytes a row.
///
/// The page is the CLIENT's bound, not the broker's: astream bounds a `Fetch` by
/// the records it SCANS (`FETCH_SCAN_MAX`, 65,536 — the same work whatever the
/// page), frames every record on its own (no reply frame grows with the page, so
/// none nears the 16 MiB frame cap), and leaves the page to the client, which
/// buffers one page whole. So what a page costs is this process's memory, and
/// for rows that is a few MiB at 4096 where the 256 it replaced paid sixteen
/// round trips for the same stretch. It is `LAST_PAGE_MAX`, the broker's own
/// clamp on a `Last` page: every last-value walk of these faces already takes
/// pages of this many rows.
pub const ROW_PAGE: u32 = astream_broker::broker::LAST_PAGE_MAX;

/// RECORDS ONE `Fetch` PAGE ASKS FOR ON A FACE THAT CARRIES POSTS — the `in`
/// lanes, the `say` topics, the whole fleet. A post's body may be the endpoint's
/// `BODY_MAX` ([`crate::bridge::FETCH_BODY_MAX`], 256 KiB), so a page of 256 is
/// up to 64 MiB buffered at once and a [`ROW_PAGE`] of them would be a GiB:
/// kept small on purpose, whatever it costs in round trips.
pub const POST_PAGE: u32 = 256;

/// Open one connection to the broker's socket at `endpoint` UNDER `deadline`,
/// answering it and the closer that can end it.
///
/// The deadline bounds every read AND every write — `SO_RCVTIMEO` and
/// `SO_SNDTIMEO` on the socket, through the closer, before the first frame.
/// So a broker that accepts and never answers, and one that stopped reading
/// part-way through a frame, both fail `TimedOut`/`WouldBlock` after
/// `deadline`, never at all. A timed-out request leaves the stream unframed:
/// drop the connection, do not retry on it. The dial itself is not bounded —
/// std has no bounded Unix connect, and it fails at once when nothing listens.
///
/// A SUBSCRIBER lifts only the READ deadline once its subscription is open
/// (`closer.set_read_timeout(None)`): parking in `recv` with nothing to deliver
/// is its normal state, and a subscription writes nothing after its request.
///
/// astream's [`astream_broker::connect`] does the work. It is the only connect
/// here, so no caller can forget the bound.
///
/// # Errors
///
/// The connect itself, `TimedOut` past the bound, the descriptor duplication
/// the closer needs, or a zero `deadline` (`InvalidInput`).
pub fn connect(endpoint: &str, deadline: Duration) -> io::Result<(Conn, Closer)> {
    let (conn, closer) =
        astream_broker::connect(&astream_broker::Transport::Unix, endpoint, Some(deadline))?;
    closer.set_read_timeout(Some(deadline))?;
    closer.set_write_timeout(Some(deadline))?;
    Ok((conn, closer))
}

/// The reconnect back-off floor and ceiling. §7: a bridge reconnects with jittered
/// back-off and opens its publisher connection before its drain connection, because
/// `MAX_CONNS` can refuse a herd and the publisher's `live inc+1` suppresses a fenced
/// will.
const RECONNECT_MIN: Duration = Duration::from_millis(100);
pub(crate) const RECONNECT_MAX: Duration = Duration::from_secs(5);

/// How long a link must have stayed up for its loss to restart the back-off from
/// [`RECONNECT_MIN`] and be redialled at once. A link lost sooner is a failed attach:
/// a subscription the broker refused (astream answers a `Subscribe` only when it
/// delivers, so a refusal is the reader's first `recv` error, after the attach
/// reported `up`), or a broker that accepts and drops. Equal to the ceiling, so a
/// link that keeps dying that way dials at most once per [`RECONNECT_MAX`].
const LINK_STABLE: Duration = RECONNECT_MAX;

/// One broker client's reconnect back-off: doubling from [`RECONNECT_MIN`] to
/// [`RECONNECT_MAX`], each wait drawn from `[b/2, b]` (equal jitter, so a refused herd
/// does not come back in step), and reset only by a link that stayed up
/// [`LINK_STABLE`], never by a dial that merely succeeded (a refused subscription
/// would then redial at round-trip speed). Pure state: the caller passes the clock
/// and does the waiting.
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
    /// A dial or attach failed: how long to wait before the next one, which waits
    /// twice as long, up to the ceiling. The jitter is a fresh CSPRNG draw.
    pub(crate) fn failed(&mut self) -> Duration {
        self.failed_with(random_u64())
    }

    /// [`Backoff::failed`] with the jitter drawn by the caller: `r`, or none at
    /// all (`None`, the whole wait). The bridge draws through its door
    /// ([`crate::fx::Fx::jitter`]), so a simulated world's back-off is its seed's.
    pub(crate) fn failed_with(&mut self, r: Option<u64>) -> Duration {
        let wait = r.map_or(self.next, |r| jittered(self.next, r));
        self.next = (self.next * 2).min(RECONNECT_MAX);
        wait
    }

    /// The link came up at `now`.
    pub(crate) fn attached(&mut self, now: Instant) {
        self.up_since = Some(now);
    }

    /// The link that came up was lost at `now`: `None` when it had stayed up
    /// [`LINK_STABLE`] (the back-off restarts at the floor and the redial goes now),
    /// else the wait a failed attach gets ([`Self::failed`]). `None` too when no link
    /// was up, so one loss is waited out once.
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
/// `b`.
fn jittered(b: Duration, r: u64) -> Duration {
    let half = b / 2;
    let span = u64::try_from((b - half).as_micros()).unwrap_or(u64::MAX);
    half + Duration::from_micros(r % span.saturating_add(1))
}

/// A jitter draw from the OS CSPRNG; `None` when it cannot be read, and the
/// caller then waits the whole `b` — never a shorter wait.
pub(crate) fn random_u64() -> Option<u64> {
    let mut raw = [0u8; 8];
    aterm_uds::rand::fill(&mut raw).ok()?;
    Some(u64::from_le_bytes(raw))
}

/// One error from [`check_private`] or [`read_mint_secret`] as text that
/// names `path` ONCE: their own refusals already lead with it, and an OS error
/// (a missing file, a denied `stat`) never carries one. A caller prefixes only
/// its flag (`--secret-file {…}`).
#[must_use]
pub fn describe(path: &str, e: &io::Error) -> String {
    if e.raw_os_error().is_some() {
        format!("{path}: {e}")
    } else {
        e.to_string()
    }
}

/// Refuse a SECRET file anyone but its owner can read: a mint secret or a
/// capability file. `mode & 0o077` must be zero — 0600 or
/// tighter. The refusal names the `chmod` rather than fixing it silently: a key
/// that sat readable may already have been read.
///
/// # Errors
///
/// The `stat`, or a mode with any group/other bit set.
pub fn check_private(path: &str) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)?.permissions().mode() & 0o777;
    if mode & 0o077 == 0 {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{path} is mode {mode:04o}, not owner-only; run `chmod 600 {path}` \
                 and treat its contents as exposed"
            ),
        ))
    }
}

/// THE MINT SECRET READER — the one every verb that mints or guards with a
/// secret goes through: `aterm link broker --secret-file`, `aterm link mint`,
/// `aterm fabric mint-for` and `aterm fabric on` (its secret step and its
/// mint). The file must be [`check_private`] (0600 or tighter) and hold at least
/// [`crate::enable::SECRET_LEN`] bytes — HMAC takes a key of any length, so a
/// short or empty one would mint capabilities that look well-formed and seal
/// nothing — and its bytes are the secret EXACTLY as read, a trailing newline
/// included: the broker that verifies and the verb that mints must agree byte
/// for byte (`asb`'s `load_secret` reads it the same way).
///
/// # Errors
///
/// A missing or unreadable file (`NotFound` when it is not there, which a
/// caller may answer with a hint), a mode with any group/other bit set
/// (`PermissionDenied`, naming the `chmod`), or a short secret (`InvalidData`).
pub fn read_mint_secret(path: &str) -> io::Result<Vec<u8>> {
    check_private(path)?;
    let secret = std::fs::read(path)?;
    if secret.len() < crate::enable::SECRET_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{path} holds {} bytes; a mint secret is at least {} (a short key mints \
                 capabilities that seal nothing)",
                secret.len(),
                crate::enable::SECRET_LEN
            ),
        ));
    }
    Ok(secret)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A connect's deadline bounds both directions: a `hello` nobody answers and a
    /// post bigger than the socket's buffers to a broker that stopped reading both
    /// fail at it. Each runs on its own thread behind the one-minute hang detector, so
    /// a missing deadline fails the test rather than hanging it.
    #[test]
    fn a_connects_deadline_ends_a_hello_nobody_answers_and_a_publish_nobody_reads() {
        use std::os::unix::net::UnixListener;
        let dir = std::env::temp_dir().join(format!("atlink-deadline-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        let sock = dir.join("s.sock");
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).expect("bind");
        let path = sock.to_str().expect("utf8").to_string();
        let deadline = Duration::from_millis(300);
        for what in ["hello", "publish"] {
            let (mut conn, _closer) = connect(&path, deadline).expect("connect");
            let (peer, _) = listener.accept().expect("accept");
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let served = if what == "hello" {
                    conn.hello().map(|_| ())
                } else {
                    // 256 KiB: the bridge's post cap, far past a Unix
                    // socket's buffers.
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
    /// re-bounds its reads (macOS: `EINVAL`, the socket is shut down both ways) is no
    /// error: the bound is set or lifted, and the subscription still reads the mail,
    /// then the close. The broker side runs on this thread and is closed before the
    /// re-bound, so the interleaving is the one a race only sometimes produced. astream's
    /// `Closer` takes that refusal as nothing to bound, so this holds the pinned astream
    /// to it for every re-bound here (the inbox, `fabric tail`, the presence view).
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
        };
        // The answer to the subscription's `IdleMarks`, ahead of it in the socket.
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

    /// 0600 or tighter passes; any group/other bit is refused.
    #[test]
    fn a_secret_file_other_users_can_read_is_refused_naming_the_chmod() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("atlink-priv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        let path = dir.join("k");
        let p = path.to_str().expect("utf8");
        std::fs::write(&path, [0x5a; 32]).expect("write");
        for (mode, ok) in [(0o600, true), (0o400, true), (0o640, false), (0o604, false)] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod");
            assert_eq!(check_private(p).is_ok(), ok, "{mode:o}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A mint secret is owner-only and at least 32 bytes, or refused with the reason,
    /// and is read byte for byte, a trailing newline included.
    #[test]
    fn a_mint_secret_is_owner_only_long_enough_and_read_byte_for_byte() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("atlink-mint-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        let path = dir.join("mint.secret");
        let p = path.to_str().expect("utf8");
        let chmod = |mode| {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod")
        };
        assert_eq!(
            read_mint_secret(p).expect_err("absent").kind(),
            io::ErrorKind::NotFound
        );
        let mut bytes = vec![0x5a; 31];
        bytes.push(b'\n');
        std::fs::write(&path, &bytes).expect("write");
        chmod(0o644);
        let e = read_mint_secret(p).expect_err("0644");
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        chmod(0o600);
        assert_eq!(read_mint_secret(p).expect("0600, 32 bytes"), bytes);
        std::fs::write(&path, [0x5a; 31]).expect("write");
        let e = read_mint_secret(p).expect_err("short");
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        let _ = std::fs::remove_dir_all(&dir);
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
    /// only one that stayed up restarts the back-off at the floor and redials at once.
    #[test]
    fn only_a_link_that_stayed_up_resets_the_back_off() {
        let within = |d: Duration, b: Duration| b / 2 <= d && d <= b;
        let t0 = Instant::now();
        let mut b = Backoff::default();
        // Nothing was up: nothing to wait out twice.
        assert_eq!(b.lost(t0), None);
        b.attached(t0);
        let early = b
            .lost(t0 + Duration::from_millis(1))
            .expect("an early loss waits");
        assert!(within(early, RECONNECT_MIN), "{early:?}");
        let next = b.failed();
        assert!(within(next, RECONNECT_MIN * 2), "it doubled: {next:?}");
        // Up long enough: the redial goes now, from the floor again.
        b.attached(t0);
        assert_eq!(b.lost(t0 + LINK_STABLE), None);
        let floor = b.failed();
        assert!(within(floor, RECONNECT_MIN), "{floor:?}");
        // The ceiling holds, including when entropy is unavailable and each
        // failure waits the full delay.
        let draws: Vec<Duration> = (0..64).map(|_| b.failed()).collect();
        assert!(
            draws[8..].iter().all(|d| within(*d, RECONNECT_MAX)),
            "{draws:?}"
        );
    }
}
