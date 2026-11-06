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
const DIAL_HANG: Duration = Duration::from_secs(10);

/// How long the broker may take to say its hello, or to answer past a request's own bound.
const ANSWER_HANG: Duration = Duration::from_secs(30);

/// What a broker said of itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrokerHello {
    /// Its protocol version.
    pub protocol: u32,
    /// Its build.
    pub build: String,
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
    open_at(request, &path, super::protocol())
}

fn open_at(request: Option<&Request>, path: &Path, ours: u32) -> Result<Session, LanError> {
    let stream = match crate::connect::until(path, Instant::now() + DIAL_HANG) {
        Ok(stream) => stream,
        Err(error) if not_installed(&error) => return Err(Unavailable::NotInstalled.into()),
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
            return Err(Unavailable::Refused(format!(
                "the kernel did not name the uid serving {}",
                path.display()
            ))
            .into());
        }
    }
    let mut bytes = wire::encode(&Frame::Hello {
        protocol: ours,
        build: super::hello_build().to_owned(),
    })?;
    if let Some(request) = request {
        bytes.extend(wire::encode(&Frame::Request(request.clone()))?);
    }
    // A broker that refuses this process says so and closes before it reads: the write may
    // then fail, and the refusal is still there to read.
    let wrote = io::Write::write_all(&mut &stream, &bytes);
    let mut reader = Reader::new(1);
    let first = reader.frame(&stream, Some(Instant::now() + ANSWER_HANG));
    let first = match (first, wrote) {
        (Ok(Some(Frame::Refused { words })), _) => {
            return Err(Unavailable::Refused(words).into());
        }
        (_, Err(error)) => return Err(error.into()),
        (first, Ok(())) => first?,
    };
    let broker = match first {
        Some(Frame::Hello { protocol, build }) => BrokerHello { protocol, build },
        Some(other) => return Err(unexpected(&other).into()),
        None => {
            return Err(Unavailable::Refused(
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

/// The broker's [`Frame::Error`] as this process's error: an `errno`'s kind (a positive code), or
/// `Other` (a DNS-SD error code), with the broker's words.
fn failed(code: i32, words: &str) -> io::Error {
    let kind = if code > 0 {
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
        match self.reader.frame(&self.stream, Some(deadline))? {
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
        match self.answer(deadline)? {
            Frame::Socket { reached } => {
                let fd = self.reader.take_fd().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "the LAN broker's socket answer carried no descriptor",
                    )
                })?;
                // The broker holds its copy until it reads this. Once the descriptor is here a
                // broker that went away has nothing left to hold, so a failed write loses nothing.
                let _ = wire::took(&self.stream);
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
    let request = Request::Connect {
        target: target.clone(),
        port,
        within_ms: millis(within),
        bound_if: options.bound_if,
        source: options.source,
        hop_limit: options.hop_limit,
    };
    let mut session = open(Some(&request))?;
    let deadline = Instant::now() + within.min(super::here::MAX_WITHIN) + ANSWER_HANG;
    let (fd, reached) = session.socket(deadline)?;
    let stream = TcpStream::from(fd);
    let reached = match reached {
        Some(reached) => reached,
        None => stream.peer_addr()?,
    };
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

/// The broker's hello alone: whether a broker serves, and which.
///
/// # Errors
/// [`LanError::Unavailable`] (no broker, another protocol, or not the root broker), or the
/// exchange's error.
pub fn hello() -> Result<BrokerHello, LanError> {
    open(None).map(|session| session.broker)
}

#[cfg(target_os = "macos")]
pub(super) fn hello_at(path: &Path, protocol: u32) -> Result<BrokerHello, LanError> {
    open_at(None, path, protocol).map(|session| session.broker)
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
