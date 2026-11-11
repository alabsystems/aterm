// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The broker's wire: length-prefixed frames over its Unix socket, each field explicitly
//! length-prefixed and every length bounded before anything is allocated for it.
//!
//! A connection carries a hello each way ([`Frame::Hello`]: the protocol and the build), then the
//! client's one [`Request`], then the broker's answer: a [`Frame::Socket`] carrying the made
//! socket as its one passed descriptor, [`Frame::Addresses`], a stream of [`Frame::Event`]s until
//! either side closes, or a [`Frame::Error`]. A broker that refuses the client's code sends
//! [`Frame::Refused`] instead of its hello, and one that did not judge it [`Frame::Unchecked`]. A
//! frame is a big-endian `u32` body length (at most [`MAX_FRAME`]) and the body: a tag byte and
//! the frame's fields.

use std::io::{self, Write as _};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::os::fd::{AsFd as _, BorrowedFd, OwnedFd};
use std::time::Instant;

use crate::CtlStream;

/// The largest frame body either side accepts: a record's rdata is at most 65535 bytes, and every
/// frame fits beside it.
pub const MAX_FRAME: usize = 72 * 1024;

/// The longest name, service, domain or words field (a DNS name is at most 1009 escaped bytes).
pub const MAX_TEXT: usize = 4096;

/// The most addresses, groups or TXT strings a frame lists.
pub const MAX_ITEMS: usize = 1024;

/// The longest TXT string (one length byte on the wire).
pub const MAX_TXT_ITEM: usize = 255;

/// Where a connect goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A name, resolved by whoever dials it.
    Host(String),
    /// An address (its port is the request's).
    Addr(SocketAddr),
}

/// A Bonjour advert ([`Request::Register`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Advert {
    /// The instance name (empty: the computer's name).
    pub name: String,
    /// The service type (`_alab._tcp`).
    pub service: String,
    /// The domain (empty: the default ones).
    pub domain: String,
    /// The port the service listens on.
    pub port: u16,
    /// The TXT record's strings, each at most [`MAX_TXT_ITEM`] bytes.
    pub txt: Vec<Vec<u8>>,
    /// The interface (index) to advertise on, or every one.
    pub interface: Option<u32>,
    /// The host the service runs on, when it is not this machine: `dns-sd -P`'s proxy form, which
    /// also registers `host`'s [`Self::addresses`] as its `A`/`AAAA` records.
    pub host: Option<String>,
    /// The proxy host's addresses (empty unless [`Self::host`] is set).
    pub addresses: Vec<IpAddr>,
}

/// The client's one request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// A connected TCP socket.
    Connect {
        /// Where.
        target: Target,
        /// The destination port.
        port: u16,
        /// The whole connect's bound, resolution included.
        within_ms: u32,
        /// The interface (index) it must leave by.
        bound_if: Option<u32>,
        /// The local address it leaves from.
        source: Option<IpAddr>,
        /// Its hop limit.
        hop_limit: Option<u8>,
    },
    /// A bound UDP socket with its groups joined.
    Udp {
        /// Where it binds.
        bind: SocketAddr,
        /// `SO_REUSEADDR` and `SO_REUSEPORT`.
        reuse: bool,
        /// Multicast groups, each on an interface (index) or the kernel's choice.
        groups: Vec<(IpAddr, Option<u32>)>,
        /// The interface multicast leaves by.
        multicast_if: Option<u32>,
        /// The hop limit of what it sends.
        ttl: Option<u8>,
    },
    /// A local name's addresses.
    Resolve {
        /// The name.
        name: String,
        /// The resolution's bound.
        within_ms: u32,
    },
    /// A Bonjour browse.
    Browse {
        /// The service type.
        service: String,
        /// The domain (empty: the default ones).
        domain: String,
    },
    /// A Bonjour service resolve.
    ResolveService {
        /// The instance name.
        name: String,
        /// The service type.
        service: String,
        /// The domain.
        domain: String,
    },
    /// A DNS-SD record query.
    Query {
        /// The record's full name.
        name: String,
        /// The record type.
        rrtype: u16,
    },
    /// A Bonjour advert, for as long as the connection lasts.
    Register(Advert),
}

/// One service instance a browse heard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instance {
    /// The instance name.
    pub name: String,
    /// Its service type.
    pub service: String,
    /// Its domain.
    pub domain: String,
    /// The interface (index) it was heard on.
    pub interface: u32,
}

/// What a Bonjour request heard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// A browse heard an instance come.
    Added(Instance),
    /// A browse heard an instance go.
    Removed(Instance),
    /// A service resolve's answer.
    Resolved {
        /// The host the service runs on.
        host: String,
        /// Its port.
        port: u16,
        /// Its TXT record's strings.
        txt: Vec<Vec<u8>>,
        /// The interface (index) the answer came on.
        interface: u32,
    },
    /// A query's record, added or removed.
    Record {
        /// Its type.
        rrtype: u16,
        /// Its data.
        rdata: Vec<u8>,
        /// Its time to live, in seconds.
        ttl: u32,
        /// The interface (index) it came on.
        interface: u32,
        /// Added (or removed).
        added: bool,
    },
    /// An advert is registered (under this name, which a conflict may have changed).
    Registered {
        /// The registered instance name.
        name: String,
        /// Its service type.
        service: String,
        /// Its domain.
        domain: String,
    },
}

/// One frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    /// The opening of each side: its protocol version and its build.
    Hello {
        /// [`super::PROTOCOL`].
        protocol: u32,
        /// The sender's build.
        build: String,
    },
    /// The broker refused the client's code (instead of its hello).
    Refused {
        /// Why.
        words: String,
    },
    /// The broker did not judge the client's code (instead of its hello): its check could not
    /// run or did not finish, or it was serving its most connections. A client of a wire that
    /// predates it reads the unknown tag as a failed exchange (`LanError::Io`): its watchers ask
    /// again, and a one-shot use fails as that act's own I/O error.
    Unchecked {
        /// Why.
        words: String,
    },
    /// The client's request.
    Request(Request),
    /// The made socket, passed beside this frame, and the address a connect reached.
    Socket {
        /// The peer a connect reached (none for a UDP socket).
        reached: Option<SocketAddr>,
    },
    /// A resolution's addresses.
    Addresses(Vec<IpAddr>),
    /// What a Bonjour request heard.
    Event(Event),
    /// The request failed: an `errno` (a socket request's) or a DNS-SD error code (a Bonjour
    /// request's), and the words.
    Error {
        /// The code.
        code: i32,
        /// The words.
        words: String,
    },
}

const HELLO: u8 = 1;
const REFUSED: u8 = 2;
const UNCHECKED: u8 = 3;
const CONNECT: u8 = 10;
const UDP: u8 = 11;
const RESOLVE: u8 = 12;
const BROWSE: u8 = 13;
const RESOLVE_SERVICE: u8 = 14;
const QUERY: u8 = 15;
const REGISTER: u8 = 16;
const SOCKET: u8 = 30;
const ADDRESSES: u8 = 31;
const EVENT: u8 = 32;
const ERROR: u8 = 33;

const ADDED: u8 = 1;
const REMOVED: u8 = 2;
const RESOLVED: u8 = 3;
const RECORD: u8 = 4;
const REGISTERED: u8 = 5;

const HOST: u8 = 1;
const ADDR: u8 = 2;

fn invalid(what: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.into())
}

trait WireBuffer {
    fn put(&mut self, bytes: &[u8]);
}

impl WireBuffer for Vec<u8> {
    fn put(&mut self, bytes: &[u8]) {
        self.extend_from_slice(bytes);
    }
}

impl WireBuffer for usize {
    fn put(&mut self, bytes: &[u8]) {
        *self = self.saturating_add(bytes.len());
    }
}

/// The same encoding walk counts bytes or writes them, with no separate size grammar.
struct Out<B>(B);

impl<B: WireBuffer> Out<B> {
    fn u8(&mut self, value: u8) {
        self.0.put(&[value]);
    }
    fn u16(&mut self, value: u16) {
        self.0.put(&value.to_be_bytes());
    }
    fn u32(&mut self, value: u32) {
        self.0.put(&value.to_be_bytes());
    }
    fn i32(&mut self, value: i32) {
        self.0.put(&value.to_be_bytes());
    }
    fn bool(&mut self, value: bool) {
        self.u8(u8::from(value));
    }
    fn bytes(&mut self, value: &[u8]) {
        self.u32(u32::try_from(value.len()).unwrap_or(u32::MAX));
        self.0.put(value);
    }
    fn text(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }
    fn count(&mut self, n: usize) {
        self.u32(u32::try_from(n).unwrap_or(u32::MAX));
    }
    fn ip(&mut self, value: IpAddr) {
        match value {
            IpAddr::V4(v4) => {
                self.u8(4);
                self.0.put(&v4.octets());
            }
            IpAddr::V6(v6) => {
                self.u8(6);
                self.0.put(&v6.octets());
            }
        }
    }
    fn sockaddr(&mut self, value: SocketAddr) {
        self.ip(value.ip());
        self.u16(value.port());
        if let SocketAddr::V6(v6) = value {
            self.u32(v6.flowinfo());
            self.u32(v6.scope_id());
        }
    }
    fn opt_u32(&mut self, value: Option<u32>) {
        self.bool(value.is_some());
        if let Some(value) = value {
            self.u32(value);
        }
    }
    fn opt_u8(&mut self, value: Option<u8>) {
        self.bool(value.is_some());
        if let Some(value) = value {
            self.u8(value);
        }
    }
    fn opt_ip(&mut self, value: Option<IpAddr>) {
        self.bool(value.is_some());
        if let Some(value) = value {
            self.ip(value);
        }
    }
    fn opt_text(&mut self, value: Option<&str>) {
        self.bool(value.is_some());
        if let Some(value) = value {
            self.text(value);
        }
    }
    fn txt(&mut self, value: &[Vec<u8>]) {
        self.count(value.len());
        for item in value {
            self.bytes(item);
        }
    }
    fn instance(&mut self, value: &Instance) {
        self.text(&value.name);
        self.text(&value.service);
        self.text(&value.domain);
        self.u32(value.interface);
    }
}

/// A frame's body being read: every read checks what is left first.
struct In<'a, const OWN: bool>(&'a [u8]);

impl<'a, const OWN: bool> In<'a, OWN> {
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        if n > self.0.len() {
            return Err(invalid("a broker frame ends inside a field"));
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }
    fn array<const N: usize>(&mut self) -> io::Result<[u8; N]> {
        let mut out = [0; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.array::<1>()?[0])
    }
    fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_be_bytes(self.array()?))
    }
    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_be_bytes(self.array()?))
    }
    fn i32(&mut self) -> io::Result<i32> {
        Ok(i32::from_be_bytes(self.array()?))
    }
    fn bool(&mut self) -> io::Result<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(invalid(format!("a broker flag reads {other}"))),
        }
    }
    fn borrowed_bytes(&mut self, max: usize) -> io::Result<&'a [u8]> {
        let len = usize::try_from(self.u32()?).unwrap_or(usize::MAX);
        if len > max {
            return Err(invalid(format!(
                "a broker field of {len} bytes is over its {max}-byte bound"
            )));
        }
        self.take(len)
    }
    fn bytes(&mut self, max: usize) -> io::Result<Vec<u8>> {
        let bytes = self.borrowed_bytes(max)?;
        Ok(if OWN { bytes.to_vec() } else { Vec::new() })
    }
    fn text(&mut self) -> io::Result<String> {
        let bytes = self.borrowed_bytes(MAX_TEXT)?;
        let value =
            std::str::from_utf8(bytes).map_err(|_| invalid("a broker text field is not UTF-8"))?;
        Ok(if OWN { value.to_owned() } else { String::new() })
    }
    /// A list's count, at most `max` and at most one item per byte left (each item is at least
    /// one byte), so the count alone allocates nothing.
    fn count(&mut self, max: usize) -> io::Result<usize> {
        let n = usize::try_from(self.u32()?).unwrap_or(usize::MAX);
        if n > max || n > self.0.len() {
            return Err(invalid(format!(
                "a broker list of {n} items is over its bound"
            )));
        }
        Ok(n)
    }
    fn ip(&mut self) -> io::Result<IpAddr> {
        match self.u8()? {
            4 => Ok(Ipv4Addr::from(self.array::<4>()?).into()),
            6 => Ok(Ipv6Addr::from(self.array::<16>()?).into()),
            other => Err(invalid(format!("a broker address of family {other}"))),
        }
    }
    fn sockaddr(&mut self) -> io::Result<SocketAddr> {
        Ok(match self.ip()? {
            IpAddr::V4(ip) => SocketAddrV4::new(ip, self.u16()?).into(),
            IpAddr::V6(ip) => {
                let port = self.u16()?;
                let flowinfo = self.u32()?;
                SocketAddrV6::new(ip, port, flowinfo, self.u32()?).into()
            }
        })
    }
    fn opt_u32(&mut self) -> io::Result<Option<u32>> {
        self.bool()?.then(|| self.u32()).transpose()
    }
    fn opt_u8(&mut self) -> io::Result<Option<u8>> {
        self.bool()?.then(|| self.u8()).transpose()
    }
    fn opt_ip(&mut self) -> io::Result<Option<IpAddr>> {
        self.bool()?.then(|| self.ip()).transpose()
    }
    fn opt_text(&mut self) -> io::Result<Option<String>> {
        self.bool()?.then(|| self.text()).transpose()
    }
    fn txt(&mut self) -> io::Result<Vec<Vec<u8>>> {
        let n = self.count(MAX_ITEMS)?;
        self.items(n, |at| at.bytes(MAX_TXT_ITEM))
    }
    fn ips(&mut self) -> io::Result<Vec<IpAddr>> {
        let n = self.count(MAX_ITEMS)?;
        self.items(n, Self::ip)
    }
    fn items<T>(
        &mut self,
        n: usize,
        mut read: impl FnMut(&mut Self) -> io::Result<T>,
    ) -> io::Result<Vec<T>> {
        let mut items = if OWN {
            Vec::with_capacity(n)
        } else {
            Vec::new()
        };
        for _ in 0..n {
            let item = read(self)?;
            if OWN {
                items.push(item);
            }
        }
        Ok(items)
    }
    fn instance(&mut self) -> io::Result<Instance> {
        Ok(Instance {
            name: self.text()?,
            service: self.text()?,
            domain: self.text()?,
            interface: self.u32()?,
        })
    }
    fn end(&self) -> io::Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(invalid("a broker frame has bytes past its last field"))
        }
    }
}

/// `frame` as its bytes on the wire, its length prefix first.
///
/// # Errors
/// A body over [`MAX_FRAME`], or a field over its bound (`InvalidInput`): nothing is sent that the
/// other side would refuse.
pub fn encode(frame: &Frame) -> io::Result<Vec<u8>> {
    let mut size = Out(0_usize);
    frame_body(&mut size, frame);
    let body = size.0;
    if body > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("a broker frame of {body} bytes is over the {MAX_FRAME}-byte bound"),
        ));
    }
    let mut out = Out(Vec::with_capacity(body + 4));
    out.u32(u32::try_from(body).unwrap_or(u32::MAX));
    frame_body(&mut out, frame);
    // The receiver's own checks, run before a byte goes: nothing is sent it would refuse.
    decode_by::<false>(&out.0[4..])
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    Ok(out.0)
}

fn frame_body<B: WireBuffer>(out: &mut Out<B>, frame: &Frame) {
    match frame {
        Frame::Hello { protocol, build } => {
            out.u8(HELLO);
            out.u32(*protocol);
            out.text(build);
        }
        Frame::Refused { words } => {
            out.u8(REFUSED);
            out.text(words);
        }
        Frame::Unchecked { words } => {
            out.u8(UNCHECKED);
            out.text(words);
        }
        Frame::Request(request) => request_body(out, request),
        Frame::Socket { reached } => {
            out.u8(SOCKET);
            out.bool(reached.is_some());
            if let Some(reached) = reached {
                out.sockaddr(*reached);
            }
        }
        Frame::Addresses(addrs) => {
            out.u8(ADDRESSES);
            out.count(addrs.len());
            for addr in addrs {
                out.ip(*addr);
            }
        }
        Frame::Event(event) => event_body(out, event),
        Frame::Error { code, words } => {
            out.u8(ERROR);
            out.i32(*code);
            out.text(words);
        }
    }
}

fn request_body<B: WireBuffer>(out: &mut Out<B>, request: &Request) {
    match request {
        Request::Connect {
            target,
            port,
            within_ms,
            bound_if,
            source,
            hop_limit,
        } => {
            out.u8(CONNECT);
            match target {
                Target::Host(name) => {
                    out.u8(HOST);
                    out.text(name);
                }
                Target::Addr(addr) => {
                    out.u8(ADDR);
                    out.sockaddr(*addr);
                }
            }
            out.u16(*port);
            out.u32(*within_ms);
            out.opt_u32(*bound_if);
            out.opt_ip(*source);
            out.opt_u8(*hop_limit);
        }
        Request::Udp {
            bind,
            reuse,
            groups,
            multicast_if,
            ttl,
        } => {
            out.u8(UDP);
            out.sockaddr(*bind);
            out.bool(*reuse);
            out.count(groups.len());
            for (group, interface) in groups {
                out.ip(*group);
                out.opt_u32(*interface);
            }
            out.opt_u32(*multicast_if);
            out.opt_u8(*ttl);
        }
        Request::Resolve { name, within_ms } => {
            out.u8(RESOLVE);
            out.text(name);
            out.u32(*within_ms);
        }
        Request::Browse { service, domain } => {
            out.u8(BROWSE);
            out.text(service);
            out.text(domain);
        }
        Request::ResolveService {
            name,
            service,
            domain,
        } => {
            out.u8(RESOLVE_SERVICE);
            out.text(name);
            out.text(service);
            out.text(domain);
        }
        Request::Query { name, rrtype } => {
            out.u8(QUERY);
            out.text(name);
            out.u16(*rrtype);
        }
        Request::Register(advert) => {
            out.u8(REGISTER);
            out.text(&advert.name);
            out.text(&advert.service);
            out.text(&advert.domain);
            out.u16(advert.port);
            out.txt(&advert.txt);
            out.opt_u32(advert.interface);
            out.opt_text(advert.host.as_deref());
            out.count(advert.addresses.len());
            for addr in &advert.addresses {
                out.ip(*addr);
            }
        }
    }
}

fn event_body<B: WireBuffer>(out: &mut Out<B>, event: &Event) {
    out.u8(EVENT);
    match event {
        Event::Added(instance) => {
            out.u8(ADDED);
            out.instance(instance);
        }
        Event::Removed(instance) => {
            out.u8(REMOVED);
            out.instance(instance);
        }
        Event::Resolved {
            host,
            port,
            txt,
            interface,
        } => {
            out.u8(RESOLVED);
            out.text(host);
            out.u16(*port);
            out.txt(txt);
            out.u32(*interface);
        }
        Event::Record {
            rrtype,
            rdata,
            ttl,
            interface,
            added,
        } => {
            out.u8(RECORD);
            out.u16(*rrtype);
            out.bytes(rdata);
            out.u32(*ttl);
            out.u32(*interface);
            out.bool(*added);
        }
        Event::Registered {
            name,
            service,
            domain,
        } => {
            out.u8(REGISTERED);
            out.text(name);
            out.text(service);
            out.text(domain);
        }
    }
}

/// The frame `body` (a frame without its length prefix) holds.
///
/// # Errors
/// `InvalidData`: an unknown tag, a field past the body's end or over its bound, text that is not
/// UTF-8, or bytes past the last field.
pub fn decode(body: &[u8]) -> io::Result<Frame> {
    decode_by::<true>(body)
}

/// The same wire walk validates outgoing bytes without owning fields, or
/// materializes an incoming frame. Both modes read and check every field.
/// With `OWN = false`, only success/error is meaningful; owned fields are empty.
fn decode_by<const OWN: bool>(body: &[u8]) -> io::Result<Frame> {
    let mut at = In::<OWN>(body);
    let frame = match at.u8()? {
        HELLO => Frame::Hello {
            protocol: at.u32()?,
            build: at.text()?,
        },
        REFUSED => Frame::Refused { words: at.text()? },
        UNCHECKED => Frame::Unchecked { words: at.text()? },
        CONNECT => {
            let target = match at.u8()? {
                HOST => Target::Host(at.text()?),
                ADDR => Target::Addr(at.sockaddr()?),
                other => return Err(invalid(format!("a broker connect target of kind {other}"))),
            };
            Frame::Request(Request::Connect {
                target,
                port: at.u16()?,
                within_ms: at.u32()?,
                bound_if: at.opt_u32()?,
                source: at.opt_ip()?,
                hop_limit: at.opt_u8()?,
            })
        }
        UDP => {
            let bind = at.sockaddr()?;
            let reuse = at.bool()?;
            let n = at.count(MAX_ITEMS)?;
            let groups = at.items(n, |at| Ok((at.ip()?, at.opt_u32()?)))?;
            Frame::Request(Request::Udp {
                bind,
                reuse,
                groups,
                multicast_if: at.opt_u32()?,
                ttl: at.opt_u8()?,
            })
        }
        RESOLVE => Frame::Request(Request::Resolve {
            name: at.text()?,
            within_ms: at.u32()?,
        }),
        BROWSE => Frame::Request(Request::Browse {
            service: at.text()?,
            domain: at.text()?,
        }),
        RESOLVE_SERVICE => Frame::Request(Request::ResolveService {
            name: at.text()?,
            service: at.text()?,
            domain: at.text()?,
        }),
        QUERY => Frame::Request(Request::Query {
            name: at.text()?,
            rrtype: at.u16()?,
        }),
        REGISTER => Frame::Request(Request::Register(Advert {
            name: at.text()?,
            service: at.text()?,
            domain: at.text()?,
            port: at.u16()?,
            txt: at.txt()?,
            interface: at.opt_u32()?,
            host: at.opt_text()?,
            addresses: at.ips()?,
        })),
        SOCKET => Frame::Socket {
            reached: at.bool()?.then(|| at.sockaddr()).transpose()?,
        },
        ADDRESSES => Frame::Addresses(at.ips()?),
        EVENT => Frame::Event(match at.u8()? {
            ADDED => Event::Added(at.instance()?),
            REMOVED => Event::Removed(at.instance()?),
            RESOLVED => Event::Resolved {
                host: at.text()?,
                port: at.u16()?,
                txt: at.txt()?,
                interface: at.u32()?,
            },
            RECORD => Event::Record {
                rrtype: at.u16()?,
                rdata: at.bytes(usize::from(u16::MAX))?,
                ttl: at.u32()?,
                interface: at.u32()?,
                added: at.bool()?,
            },
            REGISTERED => Event::Registered {
                name: at.text()?,
                service: at.text()?,
                domain: at.text()?,
            },
            other => return Err(invalid(format!("a broker event of kind {other}"))),
        }),
        ERROR => Frame::Error {
            code: at.i32()?,
            words: at.text()?,
        },
        other => return Err(invalid(format!("a broker frame of kind {other}"))),
    };
    at.end()?;
    Ok(frame)
}

/// Send `frame` on `stream`.
///
/// # Errors
/// [`encode`]'s, or the write's.
pub fn send(stream: &CtlStream, frame: &Frame) -> io::Result<()> {
    let bytes = encode(frame)?;
    (&*stream).write_all(&bytes)
}

/// The byte a client writes once it has taken in a passed descriptor ([`took`]): the broker
/// holds its own copy, and its end of the connection, until it reads it ([`pass`]).
pub const TOOK: u8 = 0x06;

/// Send `frame` on `stream` with `fd` passed beside its first byte.
///
/// # Errors
/// [`encode`]'s, or the `sendmsg(2)`'s or the write's.
pub(crate) fn send_with_fd(
    stream: &CtlStream,
    frame: &Frame,
    fd: BorrowedFd<'_>,
) -> io::Result<()> {
    let bytes = encode(frame)?;
    let sent = loop {
        match crate::fdpass::send_with_fds(stream, &bytes, &[fd]) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            other => break other?,
        }
    };
    (&*stream).write_all(bytes.get(sent..).unwrap_or_default())
}

/// Pass `fd` beside `frame` on `stream`, then hold it (and `stream`) until the peer says it took
/// the descriptor in ([`TOOK`]) or hangs up, by `deadline`. A descriptor still in flight when
/// its sender closes its copy and its end of the connection can arrive with its receive side
/// flushed on XNU: a passed TCP stream then reads end-of-file before the peer's bytes, and a
/// passed UDP socket a 0-byte datagram with no source.
///
/// # Errors
/// [`send_with_fd`]'s; `TimedOut` past `deadline`; `InvalidData` for any byte but [`TOOK`];
/// `UnexpectedEof` for a peer that hung up without it; or the read's error.
pub fn pass(
    stream: &CtlStream,
    frame: &Frame,
    fd: BorrowedFd<'_>,
    deadline: Instant,
) -> io::Result<()> {
    send_with_fd(stream, frame, fd)?;
    let mut byte = [0u8; 1];
    let n = loop {
        crate::ready::until(stream.as_fd(), crate::ready::Interest::Read, deadline)?;
        match crate::ready::with_nonblocking(stream.as_fd(), || {
            io::Read::read(&mut &*stream, &mut byte)
        }) {
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) => {}
            other => break other?,
        }
    };
    match (n, byte[0]) {
        (0, _) => Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the client hung up before it took the passed socket in",
        )),
        (_, TOOK) => Ok(()),
        (_, other) => Err(invalid(format!(
            "the client answered a passed socket with byte {other:#04x}"
        ))),
    }
}

/// Tell the broker this side took its passed descriptor in ([`pass`]).
///
/// # Errors
/// The write's.
pub fn took(stream: &CtlStream) -> io::Result<()> {
    (&*stream).write_all(&[TOOK])
}

/// Send a bounded request header or ownership acknowledgement without blocking a stop.
pub(crate) fn write_cancelled(
    stream: &CtlStream,
    mut bytes: &[u8],
    deadline: Instant,
    cancel: Option<BorrowedFd<'_>>,
) -> io::Result<()> {
    while !bytes.is_empty() {
        crate::ready::until_cancelled(
            stream.as_fd(),
            crate::ready::Interest::Write,
            deadline,
            cancel,
        )?;
        let wrote = crate::ready::with_nonblocking(stream.as_fd(), || (&*stream).write(bytes));
        match wrote {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => bytes = &bytes[n..],
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) => {}
            Err(error) => return Err(error),
        }
    }
    crate::ready::check_cancelled(cancel)
}

pub(crate) fn took_cancelled(
    stream: &CtlStream,
    deadline: Instant,
    cancel: Option<BorrowedFd<'_>>,
) -> io::Result<()> {
    write_cancelled(stream, &[TOOK], deadline, cancel)
}

/// The descriptors one receive took in: what [`crate::kept_out`] marks.
struct Took(crate::fdpass::Received);

impl crate::spawn_lock::Descriptors for Took {
    fn mark(&self) -> io::Result<()> {
        self.0.fds.iter().try_for_each(crate::keep_out_of_children)
    }
}

/// The frames of one connection as they arrive, and the descriptors passed beside them.
#[derive(Debug)]
pub struct Reader {
    buf: Vec<u8>,
    start: usize,
    fds: Vec<OwnedFd>,
    max_fds: usize,
}

impl Reader {
    /// A reader that takes at most `max_fds` descriptors in one message (a broker's reader takes
    /// none: a client that passes one is refused).
    #[must_use]
    pub const fn new(max_fds: usize) -> Self {
        Self {
            buf: Vec::new(),
            start: 0,
            fds: Vec::new(),
            max_fds,
        }
    }

    /// Whether bytes past the last frame are waiting here.
    #[must_use]
    pub fn buffered(&self) -> bool {
        self.start != self.buf.len()
    }

    /// The first descriptor passed so far, taken.
    pub fn take_fd(&mut self) -> Option<OwnedFd> {
        (!self.fds.is_empty()).then(|| self.fds.remove(0))
    }

    /// The next whole frame: `Ok(None)` when the peer closed between frames. Waits until
    /// `deadline` (`TimedOut` past it, whatever part of a frame came staying here), or with no
    /// deadline as long as it takes.
    ///
    /// # Errors
    /// `TimedOut`; `UnexpectedEof` inside a frame; `InvalidData`: a length over [`MAX_FRAME`]
    /// (refused before it is read), a body [`decode`] refuses, or more descriptors than this
    /// reader takes; or the receive's error.
    pub fn frame(
        &mut self,
        stream: &CtlStream,
        deadline: Option<Instant>,
    ) -> io::Result<Option<Frame>> {
        self.frame_cancelled(stream, deadline, None)
    }

    pub(crate) fn frame_cancelled(
        &mut self,
        stream: &CtlStream,
        deadline: Option<Instant>,
        cancel: Option<BorrowedFd<'_>>,
    ) -> io::Result<Option<Frame>> {
        loop {
            if let Some(frame) = self.complete()? {
                return Ok(Some(frame));
            }
            match deadline {
                Some(deadline) => {
                    crate::ready::until_cancelled(
                        stream.as_fd(),
                        crate::ready::Interest::Read,
                        deadline,
                        cancel,
                    )?;
                }
                None => crate::ready::one(stream.as_fd())?,
            }
            let mut chunk = [0u8; 16 * 1024];
            let max = self.max_fds;
            let took = match crate::kept_out(|| {
                crate::ready::with_nonblocking(stream.as_fd(), || {
                    crate::fdpass::recv_with_fds(stream, &mut chunk, max)
                })
                .map(Took)
            }) {
                Ok(took) => took.0,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                    ) =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            };
            self.fds.extend(took.fds);
            if took.bytes == 0 {
                return if !self.buffered() {
                    Ok(None)
                } else {
                    Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "the broker connection closed inside a frame",
                    ))
                };
            }
            self.append(&chunk[..took.bytes]);
        }
    }

    /// Compact an incomplete tail only when more bytes arrive. Complete frames
    /// from the same receive advance an offset instead of moving that tail each time.
    fn append(&mut self, bytes: &[u8]) {
        if self.start != 0 {
            self.buf.copy_within(self.start.., 0);
            self.buf.truncate(self.buf.len() - self.start);
            self.start = 0;
        }
        self.buf.extend_from_slice(bytes);
    }

    /// The frame at the head of the buffer, if it is all here.
    fn complete(&mut self) -> io::Result<Option<Frame>> {
        let waiting = &self.buf[self.start..];
        let Some(prefix) = waiting.first_chunk::<4>() else {
            return Ok(None);
        };
        let len = usize::try_from(u32::from_be_bytes(*prefix)).unwrap_or(usize::MAX);
        if len > MAX_FRAME {
            return Err(invalid(format!(
                "a broker frame of {len} bytes is over the {MAX_FRAME}-byte bound"
            )));
        }
        let Some(body) = waiting.get(4..4 + len) else {
            return Ok(None);
        };
        let frame = decode(body)?;
        self.start += 4 + len;
        if self.start == self.buf.len() {
            self.buf.clear();
            self.start = 0;
        }
        Ok(Some(frame))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round(frame: &Frame) {
        let bytes = encode(frame).unwrap();
        assert_eq!(
            bytes.capacity(),
            bytes.len(),
            "encoded output retains only its wire size"
        );
        assert_eq!(&decode(&bytes[4..]).unwrap(), frame);
        let len = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
        assert_eq!(len, bytes.len() - 4);
        let body = &bytes[4..];
        let parity = |body: &[u8]| {
            let owned = decode(body)
                .map(|_| ())
                .map_err(|error| (error.kind(), error.to_string()));
            let checked = decode_by::<false>(body)
                .map(|_| ())
                .map_err(|error| (error.kind(), error.to_string()));
            assert_eq!(checked, owned, "ownership must not change wire validation");
        };
        for end in 0..=body.len() {
            parity(&body[..end]);
        }
        for index in 0..body.len() {
            for replacement in [0, 0x80, 0xff] {
                let mut changed = body.to_vec();
                changed[index] = replacement;
                parity(&changed);
            }
        }
    }

    #[test]
    fn every_frame_survives_the_wire() {
        let v6: SocketAddr = "[fe80::1%7]:7484".parse().unwrap();
        for frame in [
            Frame::Hello {
                protocol: 1,
                build: "0.155.0+gabc".into(),
            },
            Frame::Refused {
                words: "not aterm".into(),
            },
            Frame::Unchecked {
                words: "trustd did not answer".into(),
            },
            Frame::Request(Request::Connect {
                target: Target::Host("m11.local".into()),
                port: 22,
                within_ms: 5000,
                bound_if: Some(4),
                source: Some("192.168.1.2".parse().unwrap()),
                hop_limit: Some(1),
            }),
            Frame::Request(Request::Connect {
                target: Target::Addr(v6),
                port: 7484,
                within_ms: 1,
                bound_if: None,
                source: None,
                hop_limit: None,
            }),
            Frame::Request(Request::Udp {
                bind: "0.0.0.0:5353".parse().unwrap(),
                reuse: true,
                groups: vec![
                    ("224.0.0.251".parse().unwrap(), Some(4)),
                    ("ff02::fb".parse().unwrap(), None),
                ],
                multicast_if: Some(4),
                ttl: Some(255),
            }),
            Frame::Request(Request::Resolve {
                name: "m11.local".into(),
                within_ms: 3000,
            }),
            Frame::Request(Request::Browse {
                service: "_alab._tcp".into(),
                domain: String::new(),
            }),
            Frame::Request(Request::ResolveService {
                name: "m11".into(),
                service: "_alab._tcp".into(),
                domain: "local.".into(),
            }),
            Frame::Request(Request::Query {
                name: "m11.local.".into(),
                rrtype: 28,
            }),
            Frame::Request(Request::Register(Advert {
                name: "vault".into(),
                service: "_alab-vault._tcp".into(),
                domain: "local.".into(),
                port: 7100,
                txt: vec![b"v=1".to_vec(), Vec::new()],
                interface: Some(0),
                host: Some("vault-host.local.".into()),
                addresses: vec!["192.168.1.9".parse().unwrap(), "fe80::9".parse().unwrap()],
            })),
            Frame::Socket { reached: Some(v6) },
            Frame::Socket { reached: None },
            Frame::Addresses(vec!["10.0.0.1".parse().unwrap()]),
            Frame::Event(Event::Added(Instance {
                name: "m11".into(),
                service: "_alab._tcp.".into(),
                domain: "local.".into(),
                interface: 4,
            })),
            Frame::Event(Event::Removed(Instance {
                name: "m11".into(),
                service: "_alab._tcp.".into(),
                domain: "local.".into(),
                interface: 4,
            })),
            Frame::Event(Event::Resolved {
                host: "m11.local.".into(),
                port: 7484,
                txt: vec![b"a=b".to_vec()],
                interface: 4,
            }),
            Frame::Event(Event::Record {
                rrtype: 1,
                rdata: vec![192, 168, 1, 9],
                ttl: 120,
                interface: 4,
                added: true,
            }),
            Frame::Event(Event::Registered {
                name: "vault (2)".into(),
                service: "_alab-vault._tcp.".into(),
                domain: "local.".into(),
            }),
            Frame::Error {
                code: libc::EACCES,
                words: "not local network".into(),
            },
        ] {
            round(&frame);
        }
    }

    #[test]
    fn every_length_is_bounded_before_it_is_believed() {
        // A text field claiming 4 GiB, in a five-byte body: refused, nothing allocated for it.
        let mut body = vec![REFUSED];
        body.extend_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(
            decode(&body).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        // A list claiming more items than bytes are left.
        let mut body = vec![ADDRESSES];
        body.extend_from_slice(&1000u32.to_be_bytes());
        assert_eq!(
            decode(&body).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        // A TXT string over 255 bytes, from either side.
        let advert = Advert {
            txt: vec![vec![b'x'; 256]],
            ..Advert::default()
        };
        assert_eq!(
            encode(&Frame::Request(Request::Register(advert)))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        // Bytes past the last field, an unknown tag, an empty body.
        let mut extra = encode(&Frame::Refused { words: "x".into() }).unwrap();
        extra.push(0);
        assert!(decode(&extra[4..]).is_err());
        assert!(decode(&[200]).is_err());
        assert!(decode(&[]).is_err());
        // A whole frame over the bound is refused before it is sent.
        let huge = Frame::Error {
            code: 0,
            words: "x".repeat(MAX_TEXT + 1),
        };
        assert!(encode(&huge).is_err());
    }

    #[test]
    fn a_reader_refuses_an_oversized_length_before_reading_its_body() {
        let (ours, theirs) = CtlStream::pair().unwrap();
        let claimed = u32::try_from(MAX_FRAME + 1).unwrap();
        (&theirs).write_all(&claimed.to_be_bytes()).unwrap();
        let mut reader = Reader::new(0);
        let deadline = Instant::now() + std::time::Duration::from_secs(60);
        let refused = reader.frame(&ours, Some(deadline)).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::InvalidData, "{refused}");
        assert!(refused.to_string().contains("bound"), "{refused}");
    }

    #[test]
    fn coalesced_frames_survive_every_partial_tail_and_refuse_a_bad_next_length() {
        let frames = [
            Frame::Socket { reached: None },
            Frame::Hello {
                protocol: 7,
                build: "split".into(),
            },
            Frame::Refused {
                words: "end".into(),
            },
        ];
        let wire: Vec<_> = frames
            .iter()
            .flat_map(|frame| encode(frame).unwrap())
            .collect();
        for split in 0..=wire.len() {
            let mut reader = Reader::new(0);
            let mut found = Vec::new();
            reader.append(&wire[..split]);
            while let Some(frame) = reader.complete().unwrap() {
                found.push(frame);
            }
            reader.append(&wire[split..]);
            while let Some(frame) = reader.complete().unwrap() {
                found.push(frame);
            }
            assert_eq!(found, frames, "split at {split}");
            assert!(!reader.buffered());
        }
        let mut reader = Reader::new(0);
        reader.append(&encode(&frames[0]).unwrap());
        reader.append(&u32::try_from(MAX_FRAME + 1).unwrap().to_be_bytes());
        assert_eq!(reader.complete().unwrap(), Some(frames[0].clone()));
        assert_eq!(
            reader.complete().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn a_consumed_frame_keeps_its_partial_successor_through_timeout_or_eof() {
        let first = Frame::Socket { reached: None };
        let second = Frame::Hello {
            protocol: 7,
            build: "tail".into(),
        };
        let encoded = encode(&second).unwrap();
        for finish in [false, true] {
            let (ours, mut theirs) = CtlStream::pair().unwrap();
            let mut prefix = encode(&first).unwrap();
            prefix.extend_from_slice(&encoded[..2]);
            theirs.write_all(&prefix).unwrap();
            let mut reader = Reader::new(0);
            let deadline = Some(Instant::now() + std::time::Duration::from_secs(60));
            assert_eq!(reader.frame(&ours, deadline).unwrap(), Some(first.clone()));
            if finish {
                assert_eq!(
                    reader
                        .frame(&ours, Some(Instant::now()))
                        .unwrap_err()
                        .kind(),
                    io::ErrorKind::TimedOut
                );
                theirs.write_all(&encoded[2..]).unwrap();
                drop(theirs);
                assert_eq!(reader.frame(&ours, deadline).unwrap(), Some(second.clone()));
                assert!(!reader.buffered());
                assert_eq!(reader.frame(&ours, deadline).unwrap(), None);
            } else {
                drop(theirs);
                assert_eq!(
                    reader.frame(&ours, deadline).unwrap_err().kind(),
                    io::ErrorKind::UnexpectedEof
                );
                assert!(reader.buffered());
            }
        }
    }

    #[test]
    fn a_large_successor_survives_compaction_and_multiple_receive_quanta() {
        let frames = [
            Frame::Socket { reached: None },
            Frame::Event(Event::Record {
                rrtype: 16,
                rdata: vec![0x5a; 65_535],
                ttl: 60,
                interface: 7,
                added: true,
            }),
        ];
        let wire: Vec<_> = frames
            .iter()
            .flat_map(|frame| encode(frame).unwrap())
            .collect();
        for quantum in [3, 16 * 1024, 65_536] {
            let mut reader = Reader::new(0);
            let mut found = Vec::new();
            for bytes in wire.chunks(quantum) {
                reader.append(bytes);
                while let Some(frame) = reader.complete().unwrap() {
                    found.push(frame);
                }
            }
            assert_eq!(found, frames, "receive quantum {quantum}");
            assert!(!reader.buffered());
        }
    }

    #[test]
    fn a_reader_joins_split_frames_takes_a_passed_descriptor_and_ends_cleanly() {
        let (ours, theirs) = CtlStream::pair().unwrap();
        let hello = encode(&Frame::Hello {
            protocol: 7,
            build: "b".into(),
        })
        .unwrap();
        (&theirs).write_all(&hello[..3]).unwrap();
        (&theirs).write_all(&hello[3..]).unwrap();
        let (kept, _other) = CtlStream::pair().unwrap();
        send_with_fd(&theirs, &Frame::Socket { reached: None }, kept.as_fd()).unwrap();
        drop(theirs);
        let mut reader = Reader::new(1);
        let deadline = Some(Instant::now() + std::time::Duration::from_secs(60));
        assert_eq!(
            reader.frame(&ours, deadline).unwrap(),
            Some(Frame::Hello {
                protocol: 7,
                build: "b".into()
            })
        );
        assert_eq!(
            reader.frame(&ours, deadline).unwrap(),
            Some(Frame::Socket { reached: None })
        );
        assert!(reader.take_fd().is_some());
        assert_eq!(reader.frame(&ours, deadline).unwrap(), None);
    }

    #[test]
    fn a_pass_holds_the_descriptor_until_the_peer_took_it_in() {
        let deadline = Instant::now() + std::time::Duration::from_secs(60);
        let (ours, theirs) = CtlStream::pair().unwrap();
        let (kept, _other) = CtlStream::pair().unwrap();
        let passing = std::thread::spawn(move || {
            pass(
                &theirs,
                &Frame::Socket { reached: None },
                kept.as_fd(),
                deadline,
            )
        });
        let mut reader = Reader::new(1);
        assert_eq!(
            reader.frame(&ours, Some(deadline)).unwrap(),
            Some(Frame::Socket { reached: None })
        );
        assert!(reader.take_fd().is_some());
        assert!(!passing.is_finished(), "the pass waits for the peer's word");
        took(&ours).unwrap();
        passing.join().unwrap().unwrap();

        let (ours, theirs) = CtlStream::pair().unwrap();
        let (kept, _other) = CtlStream::pair().unwrap();
        let passing = std::thread::spawn(move || {
            pass(
                &theirs,
                &Frame::Socket { reached: None },
                kept.as_fd(),
                deadline,
            )
        });
        let mut reader = Reader::new(1);
        reader.frame(&ours, Some(deadline)).unwrap();
        drop(reader);
        drop(ours);
        assert_eq!(
            passing.join().unwrap().unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn a_reader_that_takes_no_descriptor_refuses_one() {
        let (ours, theirs) = CtlStream::pair().unwrap();
        let (kept, _other) = CtlStream::pair().unwrap();
        send_with_fd(&theirs, &Frame::Socket { reached: None }, kept.as_fd()).unwrap();
        let mut reader = Reader::new(0);
        let deadline = Some(Instant::now() + std::time::Duration::from_secs(60));
        assert_eq!(
            reader.frame(&ours, deadline).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
