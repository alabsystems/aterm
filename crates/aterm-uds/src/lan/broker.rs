// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The broker's client: one connection per request, on every Unix (the public calls in
//! [`super`] route here on macOS; a test drives a broker here on any Unix).

use std::io;
use std::net::{IpAddr, SocketAddr, TcpStream, UdpSocket};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, RawFd};
use std::path::Path;
use std::time::{Duration, Instant};

use super::wire::{self, Advert, Event, Frame, Reader, Request, Target};
use super::{ConnectOptions, LanError, UdpOptions, Unavailable};
use crate::CtlStream;

/// How long the broker's socket may take to accept: a hang detector, as it answers at once.
const DIAL_HANG: Duration = Duration::from_secs(60);

/// How long the broker may take to say its hello past its check of this process
/// ([`crate::peer::CHECK_HANG`]), or to answer past a request's own bound.
const ANSWER_HANG: Duration = Duration::from_secs(60);

/// What a broker said of itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrokerHello {
    /// Its protocol version.
    pub protocol: u32,
    /// Its build.
    pub build: String,
    /// `Some(macOS's allowance, in words)` when no broker is installed and this process serves
    /// the broker's protocol to itself under that allowance ([`super::direct`]): the answer is
    /// this process's own server's, not the root broker's.
    pub direct: Option<String>,
}

/// A connection whose hellos were exchanged and whose request went.
struct Session {
    stream: CtlStream,
    reader: Reader,
    broker: BrokerHello,
}

/// Whether a dial's error means no broker is serving there.
fn not_installed(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::ENOENT | libc::ECONNREFUSED | libc::ENOTDIR)
    )
}

/// The uid a broker's socket must belong to: root's, or in a test binary (which serves its own
/// broker unprivileged, under its own scratch root) this process's.
fn broker_uid() -> u32 {
    if crate::test_binary::scratch_root().is_some() {
        crate::peer::our_uid()
    } else {
        0
    }
}

/// Dial the broker, exchange hellos and send `request` (none: a hello alone).
fn open(request: Option<&Request>) -> Result<Session, LanError> {
    let path = super::dialled_socket();
    open_at(request, &path, super::protocol(), Absent::Direct)
}

/// What a dial that finds no broker does.
#[derive(Clone, Copy)]
enum Absent {
    /// Takes this process's own server when macOS already allowed this code (`super::direct`):
    /// a LAN use.
    Direct,
    /// Reads [`Unavailable::NotInstalled`]: a question about the broker itself
    /// ([`super::Arrivals`]), which this process's own server cannot answer.
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(dead_code, reason = "broker arrivals are watched on macOS")
    )]
    NotInstalled,
}

fn open_at(
    request: Option<&Request>,
    path: &Path,
    ours: u32,
    absent: Absent,
) -> Result<Session, LanError> {
    open_started_at(request, path, ours, absent, Instant::now())
}

fn open_started_at(
    request: Option<&Request>,
    path: &Path,
    ours: u32,
    absent: Absent,
    started: Instant,
) -> Result<Session, LanError> {
    open_cancelled_at(request, path, ours, absent, started, None)
}

fn open_cancelled_at(
    request: Option<&Request>,
    path: &Path,
    ours: u32,
    absent: Absent,
    started: Instant,
    cancel: Option<BorrowedFd<'_>>,
) -> Result<Session, LanError> {
    let stream = match crate::connect::until_cancelled(path, started + DIAL_HANG, cancel) {
        Ok(stream) => stream,
        // No broker: this process's own server, when the caller makes a LAN use and macOS
        // already allowed this code (`super::direct`); a broker that answers, refuses or speaks
        // another protocol is the broker, and its answer stands.
        Err(error) if not_installed(&error) => {
            return match absent {
                Absent::Direct => {
                    crate::ready::check_cancelled(cancel)?;
                    match super::direct::open() {
                        Ok((stream, allowed)) => exchange_at(
                            stream,
                            request,
                            ours,
                            Some(allowed),
                            Instant::now(),
                            cancel,
                        ),
                        Err(_) => Err(Unavailable::NotInstalled.into()),
                    }
                }
                Absent::NotInstalled => Err(Unavailable::NotInstalled.into()),
            };
        }
        Err(error) => return Err(error.into()),
    };
    let wanted = broker_uid();
    match crate::peer::peer_uid(&stream) {
        Some(uid) if uid == wanted => {}
        Some(uid) => {
            return Err(Unavailable::Refused(format!(
                "{} is served by uid {uid}, not {wanted}: it is not the broker",
                path.display()
            ))
            .into());
        }
        None => {
            return Err(Unavailable::Unchecked(format!(
                "the kernel did not name the uid serving {}",
                path.display()
            ))
            .into());
        }
    }
    exchange_at(stream, request, ours, None, Instant::now(), cancel)
}

fn exchange_at(
    stream: CtlStream,
    request: Option<&Request>,
    ours: u32,
    direct: Option<String>,
    started: Instant,
    cancel: Option<BorrowedFd<'_>>,
) -> Result<Session, LanError> {
    let mut bytes = wire::encode(&Frame::Hello {
        protocol: ours,
        build: super::hello_build().to_owned(),
    })?;
    if let Some(request) = request {
        bytes.extend(wire::encode(&Frame::Request(request.clone()))?);
    }
    // A broker that refuses this process says so and closes before it reads: the write may
    // then fail, and the refusal is still there to read.
    let deadline = started + crate::peer::CHECK_HANG + ANSWER_HANG;
    let wrote = match cancel {
        Some(_) => wire::write_cancelled(&stream, &bytes, deadline, cancel),
        None => io::Write::write_all(&mut &stream, &bytes),
    };
    let mut reader = Reader::new(1);
    // The broker checks this process's code before it says anything: its hello, or its
    // `Unchecked` for a check that did not finish, can come that check's whole bound late.
    let first = reader.frame_cancelled(&stream, Some(deadline), cancel);
    let first = match (first, wrote) {
        (Ok(Some(Frame::Refused { words })), _) => {
            return Err(Unavailable::Refused(words).into());
        }
        (Ok(Some(Frame::Unchecked { words })), _) => {
            return Err(Unavailable::Unchecked(words).into());
        }
        (_, Err(error)) => return Err(error.into()),
        (first, Ok(())) => first?,
    };
    let broker = match first {
        Some(Frame::Hello { protocol, build }) => BrokerHello {
            protocol,
            build,
            direct,
        },
        Some(other) => return Err(unexpected(&other).into()),
        None => {
            return Err(Unavailable::Unchecked(
                "the broker closed the connection before its hello".to_owned(),
            )
            .into());
        }
    };
    if broker.protocol != ours {
        return Err(Unavailable::Outdated {
            ours,
            theirs: broker.protocol,
        }
        .into());
    }
    Ok(Session {
        stream,
        reader,
        broker,
    })
}

fn unexpected(frame: &Frame) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("the LAN broker answered out of turn: {frame:?}"),
    )
}

/// The broker's [`Frame::Error`] as this process's error: an `errno`'s kind (a positive code;
/// `EHOSTDOWN`, which std leaves uncategorized, is `HostUnreachable`, so a dial through the broker
/// reads as [`super::reached_no_peer`] as a direct one does), or `Other` (a DNS-SD error code),
/// with the broker's words.
pub(super) fn failed(code: i32, words: &str) -> io::Error {
    let kind = if code == libc::EHOSTDOWN {
        io::ErrorKind::HostUnreachable
    } else if code > 0 {
        io::Error::from_raw_os_error(code).kind()
    } else {
        io::ErrorKind::Other
    };
    io::Error::new(kind, format!("the LAN broker: {words}"))
}

/// A `within` as the wire's milliseconds, at least 1 (the broker refuses 0) and at most
/// [`super::here::MAX_WITHIN`].
fn millis(within: Duration) -> u32 {
    u32::try_from(within.min(super::here::MAX_WITHIN).as_millis())
        .unwrap_or(u32::MAX)
        .max(1)
}

impl Session {
    /// The one answer to a socket or resolve request, by `deadline`.
    fn answer(&mut self, deadline: Instant) -> Result<Frame, LanError> {
        self.answer_cancelled(deadline, None)
    }

    fn answer_cancelled(
        &mut self,
        deadline: Instant,
        cancel: Option<BorrowedFd<'_>>,
    ) -> Result<Frame, LanError> {
        match self
            .reader
            .frame_cancelled(&self.stream, Some(deadline), cancel)?
        {
            Some(Frame::Error { code, words }) => Err(failed(code, &words).into()),
            Some(frame) => Ok(frame),
            None => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the LAN broker closed the connection without an answer",
            )
            .into()),
        }
    }

    /// The socket a [`Frame::Socket`] carried.
    fn socket(
        &mut self,
        deadline: Instant,
    ) -> Result<(std::os::fd::OwnedFd, Option<SocketAddr>), LanError> {
        self.socket_cancelled(deadline, None)
    }

    fn socket_cancelled(
        &mut self,
        deadline: Instant,
        cancel: Option<BorrowedFd<'_>>,
    ) -> Result<(std::os::fd::OwnedFd, Option<SocketAddr>), LanError> {
        match self.answer_cancelled(deadline, cancel)? {
            Frame::Socket { reached } => {
                crate::ready::check_cancelled(cancel)?;
                let fd = self.reader.take_fd().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "the LAN broker's socket answer carried no descriptor",
                    )
                })?;
                // The broker holds its copy until it reads this. Once the descriptor is here a
                // broker that went away has nothing left to hold, so a failed write loses nothing.
                let _ = match cancel {
                    Some(_) => wire::took_cancelled(&self.stream, deadline, cancel),
                    None => wire::took(&self.stream),
                };
                crate::ready::check_cancelled(cancel)?;
                Ok((fd, reached))
            }
            other => Err(unexpected(&other).into()),
        }
    }
}

/// [`super::connect`] through the broker: it dials `target` on `port` within `within` and passes
/// the connection here.
///
/// # Errors
/// [`LanError::Unavailable`], or the broker's refusal or the connect's error.
pub fn connect(
    target: &Target,
    port: u16,
    within: Duration,
    options: ConnectOptions,
) -> Result<(TcpStream, SocketAddr), LanError> {
    connect_cancelled(target, port, within, options, None)
}

/// [`connect`] interrupted by the caller's latched stop, closing its owned control socket.
/// The broker then observes that closure alongside its pending numeric connect.
/// # Errors
/// As [`connect`], or `Interrupted` when the stop is set.
pub fn connect_stopped(
    target: &Target,
    port: u16,
    within: Duration,
    options: ConnectOptions,
    stop: &crate::stop::Stop,
) -> Result<(TcpStream, SocketAddr), LanError> {
    connect_cancelled(target, port, within, options, Some(stop.bell()?))
}

fn connect_cancelled(
    target: &Target,
    port: u16,
    within: Duration,
    options: ConnectOptions,
    cancel: Option<BorrowedFd<'_>>,
) -> Result<(TcpStream, SocketAddr), LanError> {
    let request = Request::Connect {
        target: target.clone(),
        port,
        within_ms: millis(within),
        bound_if: options.bound_if,
        source: options.source,
        hop_limit: options.hop_limit,
    };
    let mut session = open_cancelled_at(
        Some(&request),
        &super::dialled_socket(),
        super::protocol(),
        Absent::Direct,
        Instant::now(),
        cancel,
    )?;
    let deadline = Instant::now() + within.min(super::here::MAX_WITHIN) + ANSWER_HANG;
    let (fd, reached) = session.socket_cancelled(deadline, cancel)?;
    let stream = TcpStream::from(fd);
    let reached = match reached {
        Some(reached) => reached,
        None => stream.peer_addr()?,
    };
    crate::ready::check_cancelled(cancel)?;
    Ok((stream, reached))
}

/// [`super::udp`] through the broker.
///
/// # Errors
/// [`LanError::Unavailable`], or the broker's refusal or the socket's error.
pub fn udp(options: UdpOptions) -> Result<UdpSocket, LanError> {
    super::here::check_udp(&options)?;
    let request = Request::Udp {
        bind: options.bind,
        reuse: options.reuse,
        groups: options.groups,
        multicast_if: options.multicast_if,
        ttl: options.ttl,
    };
    let mut session = open(Some(&request))?;
    let (fd, _) = session.socket(Instant::now() + ANSWER_HANG)?;
    Ok(UdpSocket::from(fd))
}

/// [`super::resolve`] through the broker.
///
/// # Errors
/// [`LanError::Unavailable`], or the broker's refusal or the resolution's error.
pub fn resolve(name: &str, within: Duration) -> Result<Vec<IpAddr>, LanError> {
    let request = Request::Resolve {
        name: name.to_owned(),
        within_ms: millis(within),
    };
    let mut session = open(Some(&request))?;
    let deadline = Instant::now() + within.min(super::here::MAX_WITHIN) + ANSWER_HANG;
    match session.answer(deadline)? {
        Frame::Addresses(addrs) => Ok(addrs),
        other => Err(unexpected(&other).into()),
    }
}

/// The broker's hello alone: whether a LAN use would be served now, and by which broker. With
/// no broker installed it can be this process's own server ([`BrokerHello::direct`], `Some`).
///
/// # Errors
/// [`LanError::Unavailable`] (no broker and no direct path, another protocol, or not the root
/// broker), or the exchange's error.
pub fn hello() -> Result<BrokerHello, LanError> {
    open(None).map(|session| session.broker)
}

/// Read the installed root broker's protocol hello, never starting or using the
/// process-local direct server. Setup uses this before relinquishing root custody.
///
/// # Errors
/// No root broker answers, its uid or protocol differs, or its exchange fails.
#[cfg(target_os = "macos")]
pub fn installed_hello() -> Result<BrokerHello, LanError> {
    hello_at(&super::dialled_socket(), super::protocol())
}

/// The hello of the broker at `path` alone, never this process's own server: whether a broker
/// answers there ([`super::Arrivals`]).
#[cfg(target_os = "macos")]
pub(super) fn hello_at(path: &Path, protocol: u32) -> Result<BrokerHello, LanError> {
    open_at(None, path, protocol, Absent::NotInstalled).map(|session| session.broker)
}

/// What a Bonjour request hears, as it hears it: a blocking iterator, whose descriptor a poll
/// loop can wait on ([`AsFd`]), and [`Events::next_within`] for a bounded wait. Dropping it ends
/// the request.
#[derive(Debug)]
pub struct Events {
    stream: CtlStream,
    reader: Reader,
    ended: bool,
}

impl Events {
    /// The next event, waiting at most `within`: `Ok(None)` when none came in time.
    ///
    /// # Errors
    /// `UnexpectedEof` once the broker ended the request (and after an error), the request's
    /// [`Frame::Error`] (once), or the exchange's error.
    pub fn next_within(&mut self, within: Duration) -> Result<Option<Event>, LanError> {
        let deadline = Instant::now().checked_add(within);
        self.next_by(deadline.or(Some(Instant::now() + super::here::MAX_WITHIN)))
    }

    fn next_by(&mut self, deadline: Option<Instant>) -> Result<Option<Event>, LanError> {
        if self.ended {
            return Err(ended().into());
        }
        let frame = match self.reader.frame(&self.stream, deadline) {
            Err(error) if error.kind() == io::ErrorKind::TimedOut && deadline.is_some() => {
                return Ok(None);
            }
            other => other,
        };
        match frame {
            Ok(Some(Frame::Event(event))) => Ok(Some(event)),
            Ok(Some(Frame::Error { code, words })) => {
                self.ended = true;
                Err(failed(code, &words).into())
            }
            Ok(Some(other)) => {
                self.ended = true;
                Err(unexpected(&other).into())
            }
            Ok(None) => {
                self.ended = true;
                Err(ended().into())
            }
            Err(error) => {
                self.ended = true;
                Err(error.into())
            }
        }
    }

    /// Whether a frame already read waits here, so a poll of [`AsFd`] would not see it.
    #[must_use]
    pub fn buffered(&self) -> bool {
        self.reader.buffered()
    }
}

fn ended() -> io::Error {
    io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "the LAN broker ended the Bonjour request",
    )
}

impl Iterator for Events {
    type Item = Result<Event, LanError>;

    /// The next event, as long as it takes; `None` once the request ended.
    fn next(&mut self) -> Option<Self::Item> {
        if self.ended {
            return None;
        }
        match self.next_by(None) {
            Ok(Some(event)) => Some(Ok(event)),
            Ok(None) => None,
            Err(LanError::Io(error)) if error.kind() == io::ErrorKind::UnexpectedEof => None,
            Err(error) => Some(Err(error)),
        }
    }
}

impl AsFd for Events {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.stream.as_fd()
    }
}

impl AsRawFd for Events {
    fn as_raw_fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }
}

fn events(request: &Request) -> Result<Events, LanError> {
    let session = open(Some(request))?;
    Ok(Events {
        stream: session.stream,
        reader: session.reader,
        ended: false,
    })
}

/// [`super::browse`] through the broker.
///
/// # Errors
/// [`LanError::Unavailable`], or the exchange's error.
pub fn browse(service: &str, domain: &str) -> Result<Events, LanError> {
    events(&Request::Browse {
        service: service.to_owned(),
        domain: domain.to_owned(),
    })
}

/// [`super::resolve_service`] through the broker.
///
/// # Errors
/// As [`browse`].
pub fn resolve_service(name: &str, service: &str, domain: &str) -> Result<Events, LanError> {
    events(&Request::ResolveService {
        name: name.to_owned(),
        service: service.to_owned(),
        domain: domain.to_owned(),
    })
}

/// [`super::query`] through the broker.
///
/// # Errors
/// As [`browse`].
pub fn query(name: &str, rrtype: u16) -> Result<Events, LanError> {
    events(&Request::Query {
        name: name.to_owned(),
        rrtype,
    })
}

/// A Bonjour advert the broker holds for as long as this lives: dropping it closes the
/// connection, and the broker withdraws the advert.
#[derive(Debug)]
pub struct Registration {
    events: Events,
    name: String,
    service: String,
    domain: String,
}

impl Registration {
    /// The registered instance name (a conflict may have renamed it).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The registered service type.
    #[must_use]
    pub fn service(&self) -> &str {
        &self.service
    }

    /// The registered domain.
    #[must_use]
    pub fn domain(&self) -> &str {
        &self.domain
    }

    /// What the broker says of the advert later (a rename, an error), as [`Events`] do.
    pub fn events(&mut self) -> &mut Events {
        &mut self.events
    }
}

/// [`super::register`] through the broker: the advert once mDNSResponder registered it.
///
/// # Errors
/// [`LanError::Unavailable`], the registration's error, or the exchange's.
pub fn register(advert: &Advert) -> Result<Registration, LanError> {
    let mut events = events(&Request::Register(advert.clone()))?;
    match events.next_by(Some(Instant::now() + ANSWER_HANG))? {
        Some(Event::Registered {
            name,
            service,
            domain,
        }) => Ok(Registration {
            events,
            name,
            service,
            domain,
        }),
        Some(other) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("the LAN broker answered a registration with {other:?}"),
        )
        .into()),
        None => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("the LAN broker did not register the advert within {ANSWER_HANG:?}"),
        )
        .into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello(protocol: u32) -> Vec<u8> {
        wire::encode(&Frame::Hello {
            protocol,
            build: "kernel-hello".into(),
        })
        .unwrap()
    }

    #[test]
    fn valid_local_broker_dial_survives_scheduling_delay() {
        let dir = aterm_tempfile::Builder::new().tempdir_in("/tmp").unwrap();
        let path = dir.path().join("b.sock");
        let ours = super::super::protocol();
        let listener = crate::kept_out(|| crate::CtlListener::bind(&path)).unwrap();
        let worker = std::thread::spawn(move || {
            let (stream, _) = crate::accept(
                &listener,
                crate::AcceptWait::Forever,
                crate::AcceptedMode::Blocking,
            )
            .unwrap();
            if io::Write::write_all(&mut &stream, &hello(ours)).is_err() {
                return false;
            }
            matches!(Reader::new(0).frame(&stream, Some(Instant::now() + Duration::from_secs(60))),
                Ok(Some(Frame::Hello { protocol, .. })) if protocol == ours)
        });
        // A refused dial fails here with its own error, the worker left in its accept.
        let started = Instant::now() - Duration::from_secs(11);
        let session = open_started_at(None, &path, ours, Absent::NotInstalled, started).unwrap();
        assert_eq!(session.broker.protocol, ours);
        assert!(worker.join().unwrap());
    }

    #[test]
    fn an_expired_local_broker_dial_is_refused_before_it_dials() {
        // No broker here: a dial that looked would read it as not installed.
        let dir = aterm_tempfile::Builder::new().tempdir_in("/tmp").unwrap();
        let path = dir.path().join("b.sock");
        let started = Instant::now() - Duration::from_secs(61);
        assert!(matches!(
            open_started_at(None, &path, super::super::protocol(), Absent::NotInstalled, started),
            Err(LanError::Io(error)) if error.kind() == io::ErrorKind::TimedOut
        ));
    }

    #[test]
    fn valid_local_broker_hello_survives_its_check_and_scheduling_delay() {
        let (client, server) = crate::kept_out(CtlStream::pair).unwrap();
        let ours = super::super::protocol();
        io::Write::write_all(&mut &server, &hello(ours)).unwrap();
        let started = Instant::now() - Duration::from_secs(91);
        let result = exchange_at(client, None, ours, None, started, None);
        assert_eq!(result.unwrap().broker.protocol, ours);

        // A real peer has received the header; cancellation must end the owned connection
        // while either the broker's hello or its socket answer is still outstanding.
        for waiting_for_socket in [false, true] {
            let (client, server) = crate::kept_out(CtlStream::pair).unwrap();
            let stop = crate::stop::Stop::new();
            let request = Request::Connect {
                target: Target::Addr("127.0.0.1:1".parse().unwrap()),
                port: 1,
                within_ms: 20_000,
                bound_if: None,
                source: None,
                hop_limit: None,
            };
            if waiting_for_socket {
                wire::send(
                    &server,
                    &Frame::Hello {
                        protocol: ours,
                        build: "held-answer".into(),
                    },
                )
                .unwrap();
            }
            std::thread::scope(|scope| {
                let caller_stop = &stop;
                let (entered, observed) = std::sync::mpsc::sync_channel(1);
                let worker = scope.spawn(move || -> Result<(), LanError> {
                    let cancel = Some(caller_stop.bell()?);
                    let mut session = exchange_at(
                        client,
                        Some(&request),
                        ours,
                        None,
                        Instant::now() - Duration::from_secs(60),
                        cancel,
                    )?;
                    entered.send(()).unwrap();
                    let _ = session
                        .socket_cancelled(Instant::now() + Duration::from_secs(60), cancel)?;
                    Ok(())
                });
                let mut reader = Reader::new(0);
                let deadline = Some(Instant::now() + Duration::from_secs(60));
                assert!(matches!(
                    reader.frame(&server, deadline).unwrap(),
                    Some(Frame::Hello { .. })
                ));
                assert!(matches!(
                    reader.frame(&server, deadline).unwrap(),
                    Some(Frame::Request(Request::Connect { .. }))
                ));
                if waiting_for_socket {
                    observed.recv_timeout(Duration::from_secs(60)).unwrap();
                }
                assert!(stop.set());
                assert!(
                    matches!(worker.join().unwrap(), Err(LanError::Io(error)) if error.kind() == io::ErrorKind::Interrupted)
                );
                assert!(
                    reader.frame(&server, deadline).unwrap().is_none(),
                    "cancellation closed the owned control socket"
                );
            });
        }
    }

    #[test]
    fn a_queued_broker_verdict_wins_when_the_client_hello_cannot_be_written() {
        for refused in [false, true] {
            let (client, server) = crate::kept_out(CtlStream::pair).unwrap();
            let words = "the broker's verdict".to_owned();
            let frame = if refused {
                Frame::Refused {
                    words: words.clone(),
                }
            } else {
                Frame::Unchecked {
                    words: words.clone(),
                }
            };
            io::Write::write_all(&mut &server, &wire::encode(&frame).unwrap()).unwrap();
            server.shutdown(std::net::Shutdown::Write).unwrap();
            client.shutdown(std::net::Shutdown::Write).unwrap();
            assert!(io::Write::write_all(&mut &client, b"unwritable hello").is_err());
            // The check took its whole minute. Its queued answer wins over the closed
            // write half's error; deleting that precedence is the negative control.
            let started = Instant::now() - Duration::from_secs(61);
            let result = exchange_at(client, None, super::super::protocol(), None, started, None);
            let expected = if refused {
                Unavailable::Refused(words)
            } else {
                Unavailable::Unchecked(words)
            };
            assert!(matches!(result, Err(LanError::Unavailable(said)) if said == expected));
        }
    }

    #[test]
    fn an_expired_local_broker_hello_is_still_refused() {
        let (client, server) = crate::kept_out(CtlStream::pair).unwrap();
        let ours = super::super::protocol();
        io::Write::write_all(&mut &server, &hello(ours)).unwrap();
        assert!(matches!(
            exchange_at(client, None, ours, None, Instant::now() - Duration::from_secs(121), None),
            Err(LanError::Io(error)) if error.kind() == io::ErrorKind::TimedOut
        ));
    }
}
