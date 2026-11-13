// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! HTTP and local-path vault transport.

use crate::envelope::MAX_ENVELOPE;
use crate::{Error, ErrorKind};
use aterm_http::{Client, ProxyMode, Trust, Uri};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[cfg(any(unix, test))]
mod records_body;

/// Where a vault is.
#[derive(Clone, PartialEq, Eq)]
pub enum Source {
    /// `http://` or `https://` URL, without its fragment.
    Url(String),
    /// A local path (absolute, or `~/…` expanded by the caller).
    Path(PathBuf),
}

impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Url(url) => f.debug_tuple("Url").field(&describe(url)).finish(),
            Self::Path(path) => f.debug_tuple("Path").field(path).finish(),
        }
    }
}

/// What a fetch got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetched {
    /// The bytes at the source.
    Body(Vec<u8>),
    /// A long-poll with no newer version: an HTTP 204, or a sent request held past its deadline.
    NoNewer(Option<u64>),
    /// Nothing there: a `404`, or no file at the path.
    Missing,
}

/// A completed HTTP 204 response, distinct from a held request's synthetic no-news result.
/// Its constructor stays at the actual response boundary; no URL or headers leave it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(any(unix, test))]
pub(crate) struct NoContentReply {
    request: [u8; 32],
    responded_at_ms: u64,
}

#[cfg(any(unix, test))]
impl NoContentReply {
    pub(crate) const fn responded_at_ms(self) -> u64 {
        self.responded_at_ms
    }

    pub(crate) fn matches_binding(self, binding: [u8; 32]) -> bool {
        self.request == binding
    }
}

/// The exact URL and caller-supplied request context, without retaining their text.
#[cfg(any(unix, test))]
pub(crate) fn request_binding(url: &str, after: Option<u64>, subscriber: Option<&str>) -> [u8; 32] {
    let mut digest = aterm_digest::Sha256::new();
    digest.update(b"aterm vault HTTP204 request/1\0");
    digest.update((url.len() as u64).to_be_bytes());
    digest.update(url.as_bytes());
    match after {
        Some(after) => {
            digest.update([1]);
            digest.update(after.to_be_bytes());
        }
        None => digest.update([0]),
    }
    match subscriber {
        Some(subscriber) => {
            digest.update([1]);
            digest.update((subscriber.len() as u64).to_be_bytes());
            digest.update(subscriber.as_bytes());
        }
        None => digest.update([0]),
    }
    digest.finalize()
}

#[derive(Debug)]
pub(crate) struct ObservedFetch {
    pub(crate) fetched: Fetched,
    #[cfg(any(unix, test))]
    pub(crate) http204: Option<NoContentReply>,
}

#[derive(Debug)]
pub(crate) struct ObservedAnswer {
    answer: Answer,
    #[cfg(any(unix, test))]
    http204: Option<NoContentReply>,
}

impl From<Answer> for ObservedAnswer {
    fn from(answer: Answer) -> Self {
        Self {
            answer,
            #[cfg(any(unix, test))]
            http204: None,
        }
    }
}

impl ObservedAnswer {
    pub(crate) fn fetched(self) -> crate::Result<ObservedFetch> {
        match self.answer {
            Answer::Fetched(fetched) => Ok(ObservedFetch {
                fetched,
                #[cfg(any(unix, test))]
                http204: self.http204,
            }),
            Answer::Foreign(why) => Err(Error::new(why)),
        }
    }
}

/// What [`fetch`] adds to `wait` for the request's own deadline.
#[cfg(not(test))]
pub const FETCH_MARGIN: Duration = Duration::from_secs(5);
/// A test's margin outlasts its one-minute hang detector: a wait meant to end at its own total
/// ([`fetch_within`], [`get_within`]) that waits the margin too overruns the detector, and one
/// that ends at its total never waits it.
#[cfg(test)]
pub const FETCH_MARGIN: Duration = aterm_spec::wait::MUST_HAPPEN.saturating_mul(2);

/// [`FETCH_MARGIN`], or a test's hang detector where longer: the bound a test holds on this
/// thread for the vault servers it started (`client::ServersAnswering`), which answer.
fn fetch_margin() -> Duration {
    #[cfg(unix)]
    {
        FETCH_MARGIN.max(crate::client::servers_within(FETCH_MARGIN))
    }
    #[cfg(not(unix))]
    {
        FETCH_MARGIN
    }
}
/// The DNS lookup, connect and TLS handshake of each of a look's tries ([`look_bounded`]):
/// a vault's name answers in milliseconds once macOS lets it, and mDNS says a `.local` name
/// nobody has is not there after about five seconds, which this outlasts so that answer is
/// heard as one.
pub const LOOK_CONNECT: Duration = Duration::from_secs(8);
/// A look's DNS lookup, connect and TLS handshake ([`get_within`]): a vault on the LAN answers
/// in milliseconds.
pub const PROBE_CONNECT: Duration = Duration::from_secs(1);
/// The longest `Retry-After` a [`post_join`] sits out before it asks once more: the server's
/// join change gap, which a key's changed request (its join password typed again) waits for.
/// The server's other `429`s, its caps on the queue, carry none.
const JOIN_RETRY_MOST: Duration = Duration::from_secs(10);
/// The deadline of a publish (up to 16 MiB on a LAN), of a records post and of the join
/// exchanges. Every exchange of one fan-out to many servers shares one such deadline
/// (`client::fan_out`).
pub(crate) const EXCHANGE: Duration = Duration::from_secs(120);
/// The deadline of a join verdict's one-line exchange ([`post_verdict`], [`list_verdicts`]):
/// a waiting subscriber reads it before each long-poll, and the approver posts it within its
/// pass, so neither waits out a [`EXCHANGE`] on a vault that does not answer.
#[cfg(not(test))]
const VERDICT_EXCHANGE: Duration = Duration::from_secs(10);
/// A unit test's is its one-minute hang detector: every vault it posts a verdict to or reads
/// verdicts from is one it started on loopback, which answers.
#[cfg(test)]
const VERDICT_EXCHANGE: Duration = aterm_spec::wait::MUST_HAPPEN;

/// [`VERDICT_EXCHANGE`], or a test's hang detector where longer: the bound a test holds on this
/// thread for the vault servers it started (`client::ServersAnswering`), which answer.
fn verdict_exchange() -> Duration {
    #[cfg(unix)]
    {
        VERDICT_EXCHANGE.max(crate::client::servers_within(VERDICT_EXCHANGE))
    }
    #[cfg(not(unix))]
    {
        VERDICT_EXCHANGE
    }
}
/// The largest answer read from a PUT or a join POST (a one-line reason).
const REASON_BODY: usize = 64 * 1024;
/// The largest join queue read (256 lines of under 256 bytes, with room to spare).
const JOINS_BODY: usize = 1 << 20;
/// The most characters of a server's reason quoted in an error.
const REASON_CHARS: usize = 200;
/// The most characters of this machine's own failure to reach a server quoted in an error (an
/// I/O error: the system's and aterm-http's words, which may end with a remedy, a Linux `.local`
/// lookup's), longer than a server's one-line reason. Any other transport error may quote what
/// the server sent (a malformed answer's bytes) and keeps [`REASON_CHARS`].
const ERROR_CHARS: usize = 512;

/// Parse what a person typed.
pub fn parse_source(
    text: &str,
    home: &std::path::Path,
) -> crate::Result<(Source, Option<[u8; 32]>)> {
    let text = text.trim();
    if text.is_empty() {
        return Err(Error::new(
            "no source given: type an http(s):// URL, an absolute path, or a path starting ~/",
        ));
    }
    let lower = text.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        let (url, fragment) = match text.split_once('#') {
            Some((url, fragment)) => (url, Some(fragment)),
            None => (text, None),
        };
        if Uri::parse(url).is_none() {
            return Err(Error::new(
                "that URL is not usable: it needs a host, and no user:password@, spaces or control characters",
            ));
        }
        let pin = fragment.map(parse_pin).transpose()?;
        return Ok((Source::Url(url.to_owned()), pin));
    }
    let (path, pin) = match text.rsplit_once('#') {
        Some((path, fragment)) if !path.is_empty() => match parse_pin(fragment) {
            Ok(pin) => (path, Some(pin)),
            Err(_) => (text, None),
        },
        _ => (text, None),
    };
    if let Some(rest) = path.strip_prefix("~/") {
        let rest = rest.trim_start_matches('/');
        if rest.is_empty() {
            return Err(Error::new(
                "~/ names your home directory, not a file: name the vault file under it",
            ));
        }
        return Ok((Source::Path(home.join(rest)), pin));
    }
    if path.starts_with('/') {
        return Ok((Source::Path(PathBuf::from(path)), pin));
    }
    let hint = if lower.contains("://") {
        "only http:// and https:// URLs are vault sources (for a file, type its path)"
    } else if text == "~" || text.starts_with('~') {
        "a home path starts with ~/ (another user's ~name is not expanded)"
    } else if text.contains(':') || text.contains(".local") {
        "a URL starts with http:// or https:// (for example http://alab-secrets.local:7484/v1/alab)"
    } else {
        "a path is absolute or starts with ~/; a URL starts with http:// or https://"
    };
    Err(Error::new(format!("not a vault source: {hint}")))
}

/// Decode a `#<signer>` pin: 32 bytes in standard or URL-safe base64, padded or not.
fn parse_pin(fragment: &str) -> crate::Result<[u8; 32]> {
    let refused = || {
        Error::new(
            "the #pin after the URL is not a 32-byte signing key in base64 (standard or URL-safe)",
        )
    };
    let urlsafe = fragment.contains(['-', '_']);
    if urlsafe && fragment.contains(['+', '/']) {
        return Err(refused());
    }
    let mut text: String = fragment
        .chars()
        .map(|c| match c {
            '-' => '+',
            '_' => '/',
            c => c,
        })
        .collect();
    if !text.contains('=') {
        while !text.len().is_multiple_of(4) {
            text.push('=');
        }
    }
    crate::b64::decode_padded(&text)
        .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok())
        .ok_or_else(refused)
}

/// The vault name a source implies: a URL's last path segment, a path's file stem.
pub fn vault_name(source: &Source) -> crate::Result<String> {
    match source {
        Source::Url(url) => {
            let uri =
                Uri::parse(base(url)).ok_or_else(|| Error::new("the vault URL is not usable"))?;
            let name = uri.path().rsplit('/').next().unwrap_or_default();
            if crate::envelope::valid_name(name) {
                Ok(name.to_owned())
            } else {
                Err(Error::new(
                    "the vault URL must end in the vault's name, …/v1/<name>, with <name> of lowercase letters, digits and -",
                ))
            }
        }
        Source::Path(path) => {
            let stem = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or_default();
            if crate::envelope::valid_name(stem) {
                Ok(stem.to_owned())
            } else {
                Err(Error::new(
                    "the vault file's name must be <name> or <name>.<ext>, with <name> of lowercase letters, digits and -",
                ))
            }
        }
    }
}

/// A URL without its fragment.
fn base(url: &str) -> &str {
    url.split_once('#').map_or(url, |(base, _)| base)
}

/// `url` with `pair` added to its query.
fn with_query(url: &str, pair: &str) -> String {
    let url = base(url);
    let joiner = if url.contains('?') { '&' } else { '?' };
    format!("{url}{joiner}{pair}")
}

/// `url`'s join endpoint: `/join` after its path, before any query.
fn join_url(url: &str) -> String {
    let url = base(url);
    match url.split_once('?') {
        Some((path, query)) => format!("{path}/join?{query}"),
        None => format!("{url}/join"),
    }
}

/// `<url>/join/verdict`, the admin Macs' verdicts on join requests ([`post_verdict`]).
fn verdict_url(url: &str) -> String {
    let url = base(url);
    match url.split_once('?') {
        Some((path, query)) => format!("{path}/join/verdict?{query}"),
        None => format!("{url}/join/verdict"),
    }
}

/// `<url>/records`, the grow-only membership-record endpoint ([`fetch_records`], [`post_records`]).
fn records_url(url: &str) -> String {
    let url = base(url);
    match url.split_once('?') {
        Some((path, query)) => format!("{path}/records?{query}"),
        None => format!("{url}/records"),
    }
}

/// `<url>/records?have=<frontier>`, naming record digests in lowercase hex: as many of `have`, in
/// its order ([`crate::membership_log::Store::have`] lists the deepest head first, each head with
/// a few of its ancestors), as keep the URL within what `aterm-http` sends
/// ([`aterm_http::uri::MAX_URI_LEN`]). A server answers the records past the ids named that it
/// holds, a superset of those past every head, and the puller drops the ones it holds, so a
/// frontier forked past that room (a seated key's records at sequence one, each a head) still
/// pulls.
fn records_have_url(url: &str, have: &[crate::membership_log::RecordId]) -> String {
    let mut target = with_query(&records_url(url), "have=");
    let start = target.len();
    for id in have {
        let comma = usize::from(target.len() > start);
        if target.len() + comma + 2 * id.len() > aterm_http::uri::MAX_URI_LEN {
            break;
        }
        if comma == 1 {
            target.push(',');
        }
        target.push_str(&aterm_codec::hex::encode_lower(id));
    }
    target
}

/// How human-readable output names a URL: scheme, host and path.
pub fn describe(url: &str) -> String {
    match Uri::parse(base(url)) {
        Some(uri) => format!(
            "{}://{}{}",
            uri.scheme().as_str(),
            uri.host_header(),
            uri.path()
        ),
        None => "the vault URL".to_owned(),
    }
}

/// A server's reason: its first line, sanitized.
pub fn reason(body: &[u8]) -> String {
    one_line(body, REASON_CHARS)
}

/// The first line of `bytes`, at most `cap` characters, each byte outside printable ASCII a `?`.
fn one_line(bytes: &[u8], cap: usize) -> String {
    let line = bytes.split(|&b| b == b'\n').next().unwrap_or_default();
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    line.iter()
        .take(cap)
        .map(|&b| {
            if (0x20..0x7f).contains(&b) {
                char::from(b)
            } else {
                '?'
            }
        })
        .collect::<String>()
        .trim()
        .to_owned()
}

fn refused(what: &str, url: &str, status: u16, body: &[u8]) -> Error {
    let reason = reason(body);
    let reason = if reason.is_empty() {
        String::new()
    } else {
        format!(": {reason}")
    };
    Error::new(format!(
        "{what} {} answered {status}{reason}",
        describe(url)
    ))
}

fn client(timeout: Duration) -> Client {
    Client::new(Trust::PlatformVerifier, ProxyMode::Direct, timeout)
        .with_connector(Arc::new(LanConnector))
}

/// The connector every vault request dials through. A local name
/// ([`aterm_uds::lan::is_local_name`]: `alab-secrets.local`, a Mac's `<host>.local`, a single
/// label) is resolved and dialled by [`aterm_uds::lan::connect`], which on macOS has the root LAN
/// broker resolve it, so this process makes no `.local` lookup macOS's Local Network privacy
/// judges; without a broker the connect fails with the broker's words
/// ([`aterm_uds::lan::LanError::into_io`]), never a lookup here. Any other name, and every name
/// off Unix, goes to aterm-http's own connector, whose connect sends a local-network address
/// through the broker ([`aterm_uds::connect_tcp`]).
#[derive(Clone, Copy, Debug, Default)]
struct LanConnector;

impl aterm_http::Connect for LanConnector {
    fn connect(
        &self,
        host: &str,
        port: u16,
        deadline: aterm_http::Deadline,
    ) -> std::io::Result<std::net::TcpStream> {
        #[cfg(unix)]
        if aterm_uds::lan::is_local_name(host) {
            let within = deadline.remaining_or_timeout()?;
            let (stream, _) = aterm_uds::lan::connect(
                &aterm_uds::lan::Target::Host(host.to_owned()),
                port,
                within,
                aterm_uds::lan::ConnectOptions::default(),
            )
            .map_err(aterm_uds::lan::LanError::into_io)?;
            stream.set_nodelay(true)?;
            return Ok(stream);
        }
        aterm_http::TcpConnector.connect(host, port, deadline)
    }
}

/// [`LanConnector`], which also keeps the address of the server its last connect reached, so a
/// reader can say which server answered it ([`wait_joins_from`]).
#[derive(Debug, Default)]
struct PeerNoting(std::sync::Mutex<Option<SocketAddr>>);

impl PeerNoting {
    fn reached(&self) -> Option<IpAddr> {
        use aterm_types::MutexExt as _;
        self.0.lock_or_recover().map(|addr| addr.ip())
    }
}

impl aterm_http::Connect for PeerNoting {
    fn connect(
        &self,
        host: &str,
        port: u16,
        deadline: aterm_http::Deadline,
    ) -> std::io::Result<std::net::TcpStream> {
        use aterm_types::MutexExt as _;
        let stream = LanConnector.connect(host, port, deadline)?;
        *self.0.lock_or_recover() = stream.peer_addr().ok();
        Ok(stream)
    }
}

/// A failure to talk to the server; a request past its deadline is [`ErrorKind::TimedOut`].
fn transport_error(what: &str, url: &str, error: &aterm_http::Error) -> Error {
    let text = format!(
        "{what} {}: {}",
        describe(url),
        one_line(
            error.to_string().as_bytes(),
            match error {
                aterm_http::Error::Io(_) => ERROR_CHARS,
                _ => REASON_CHARS,
            }
        )
    );
    if matches!(error, aterm_http::Error::Io(io) if io.kind() == std::io::ErrorKind::TimedOut) {
        return Error::of_kind(ErrorKind::TimedOut, text);
    }
    Error::new(text)
}

/// The header a subscriber's GET sends so the server records when it was last heard (`age1…`).
pub const SUBSCRIBER_HEADER: &str = "X-Aterm-Subscriber";

/// Advisory purpose of a non-consuming server probe or sealed replica pull. This grants no
/// authority; ordinary unknown consumers remain anonymous unless they identify themselves.
pub const OBSERVER_HEADER: &str = "X-Aterm-Observer";

/// Header asking a `>= 0.146` server's `?after=<seq>` GET to answer `204` at once instead of
/// holding a long-poll when `held.seq <= seq` (`DESIGN-fluid-swarm` §4.1 F6).
pub const NO_WAIT_HEADER: &str = "X-Aterm-No-Wait";

/// Header on a `>= 0.146` server's `/records` GET reply (`"1"`) when the caller's `?have=` frontier
/// names a record the server's own log does not hold, so the caller posts its records back.
pub const RECORDS_NEED_HEADER: &str = "X-Aterm-Records-Need";

/// Header on every `/records` GET reply of a server since F23: 32 lowercase hex digits, 16 random
/// bytes the server draws when it starts. A restart forgets the records the server held waiting
/// on a parent (its file holds complete records only), so a member that hears another boot id
/// from a server that lacks its frontier posts it the whole store again
/// (`docs/DESIGN-fluid-swarm-2026-10-28.md` §4.1 F23, rule 8).
pub const RECORDS_BOOT_HEADER: &str = "X-Aterm-Records-Boot";

/// Header on every records GET and POST a build since the crossed fence sends, on each of a vault
/// server's pulls of its peers, and on every records reply such a server sends: what the sender
/// states of its records ([`crate::crossing::RecordsBound`]: its held bound, and whether its log
/// has crossed). A crossed node answers a records request without it `409` with no record, and
/// takes nothing from a reply without it. A value that is not exactly a bound's, or a header
/// sent twice, is none.
pub const RECORDS_BOUND_HEADER: &str = "X-Aterm-Records-Bound";

/// The query a crossed node's records POST carries, beside a crossed
/// [`RECORDS_BOUND_HEADER`]. Version 2 requires numeric capacity checks: older
/// servers, including the header-only fence accepting `crossed=1`, reject this
/// query before reading the body. No larger history is handed to those readers.
pub const CROSSED_POST_QUERY: &str = "crossed=2";

/// What a records reply said of its server's records ([`RECORDS_BOUND_HEADER`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReplyBound {
    /// No reply came: a long poll the server held past its deadline.
    Unheard,
    /// A reply that stated no bound: a server before the fence (or none at all, `0.145`'s `404`).
    Unstated,
    /// A reply that stated this.
    Stated(crate::crossing::RecordsBound),
}

impl ReplyBound {
    /// What a reply whose [`RECORDS_BOUND_HEADER`] values are `values` states.
    fn of<'a>(mut values: impl Iterator<Item = &'a str>) -> Self {
        match (values.next(), values.next()) {
            (Some(value), None) => {
                crate::crossing::RecordsBound::parse(value).map_or(Self::Unstated, Self::Stated)
            }
            _ => Self::Unstated,
        }
    }

    /// The bound the reply stated.
    #[cfg(unix)]
    pub(crate) const fn stated(self) -> Option<crate::crossing::RecordsBound> {
        match self {
            Self::Stated(bound) => Some(bound),
            Self::Unheard | Self::Unstated => None,
        }
    }

    /// Whether the reply stated that its server's log has crossed.
    #[cfg(unix)]
    pub(crate) const fn crossed(self) -> bool {
        matches!(self, Self::Stated(bound) if bound.crossed)
    }
}

fn check_records_reply(
    local: crate::crossing::RecordsBound,
    remote: ReplyBound,
    url: &str,
) -> crate::Result<()> {
    let incompatible = match remote {
        ReplyBound::Stated(peer) => !local.can_exchange(peer),
        ReplyBound::Unstated => local.crossed,
        ReplyBound::Unheard => false,
    };
    if incompatible {
        return Err(Error::of_kind(
            ErrorKind::Refused,
            format!(
                "{}: incompatible membership records capacity",
                describe(url)
            ),
        ));
    }
    Ok(())
}

/// Fetch bytes at `source` ([`FETCH_MARGIN`], [`Fetched::NoNewer`], [`Fetched::Missing`]).
pub fn fetch(source: &Source, after: Option<u64>, wait: Duration) -> crate::Result<Fetched> {
    fetch_as(source, after, wait, None)
}

/// [`fetch`], naming the subscriber's `age1…` recipient in [`SUBSCRIBER_HEADER`] when `Some`.
pub fn fetch_as(
    source: &Source,
    after: Option<u64>,
    wait: Duration,
    subscriber: Option<&str>,
) -> crate::Result<Fetched> {
    fetch_as_observed(source, after, wait, subscriber).map(|observed| observed.fetched)
}

pub(crate) fn fetch_as_observed(
    source: &Source,
    after: Option<u64>,
    wait: Duration,
    subscriber: Option<&str>,
) -> crate::Result<ObservedFetch> {
    match source {
        Source::Url(url) => {
            let client = client(wait.saturating_add(fetch_margin()));
            let sent = get_request(&client, url, after, None, subscriber, false, false)
                .limit(MAX_ENVELOPE)
                .send_phased(&[]);
            classify_get_response(
                sent,
                url,
                after,
                #[cfg(any(unix, test))]
                subscriber,
            )
            .map_err(|unheard| unheard.why)
            .and_then(ObservedAnswer::fetched)
        }
        Source::Path(path) => read_path(path).map(|bytes| ObservedFetch {
            fetched: bytes.map_or(Fetched::Missing, Fetched::Body),
            #[cfg(any(unix, test))]
            http204: None,
        }),
    }
}

/// [`fetch`] of the vault at `url` as one try of a bounded wait: the whole request, lookup to
/// last byte, within `total`, with no [`FETCH_MARGIN`], so a long-poll the server holds for its
/// own time ends at `total` too, failing [`ErrorKind::TimedOut`].
pub fn fetch_within(url: &str, after: Option<u64>, total: Duration) -> crate::Result<Fetched> {
    fetch_within_as(url, after, total, None)
}

/// [`fetch_within`], naming the subscriber's `age1…` recipient in [`SUBSCRIBER_HEADER`] when `Some`.
pub fn fetch_within_as(
    url: &str,
    after: Option<u64>,
    total: Duration,
    subscriber: Option<&str>,
) -> crate::Result<Fetched> {
    fetch_within_as_observed(url, after, total, subscriber).map(|observed| observed.fetched)
}

pub(crate) fn fetch_within_as_observed(
    url: &str,
    after: Option<u64>,
    total: Duration,
    subscriber: Option<&str>,
) -> crate::Result<ObservedFetch> {
    get_observed_via_with(&client(total), url, after, None, subscriber, false)
        .map_err(|unheard| unheard.why)?
        .fetched()
}

/// [`fetch_within`] for server-to-server pull (`DESIGN-fluid-swarm` §4.1 F6): sends `?after=<seq>`
/// with [`NO_WAIT_HEADER`] so `>= 0.146` peers answer `200` or `204` immediately while `0.145`
/// peers ignore the header and either answer `200` immediately (when newer) or time out into
/// [`Fetched::NoNewer`].
pub fn fetch_after_nowait(
    url: &str,
    after: Option<u64>,
    total: Duration,
) -> crate::Result<Fetched> {
    match get_via_with(
        &client(total),
        url,
        after,
        Some(PROBE_CONNECT.min(total)),
        None,
        true,
    )
    .map_err(|unheard| unheard.why)?
    {
        Answer::Fetched(fetched) => Ok(fetched),
        Answer::Foreign(why) => Err(Error::new(why)),
    }
}

/// A non-consuming server probe or sealed replica pull. Generic fetches deliberately do not
/// send this purpose marker, even when they ask not to wait.
#[cfg(any(unix, test))]
pub(crate) fn observe_after_nowait(
    url: &str,
    after: Option<u64>,
    total: Duration,
) -> crate::Result<Fetched> {
    match get_via_purpose(
        &client(total),
        url,
        after,
        Some(PROBE_CONNECT.min(total)),
        None,
        true,
        true,
    )
    .map(|observed| observed.answer)
    .map_err(|unheard| unheard.why)?
    {
        Answer::Fetched(fetched) => Ok(fetched),
        Answer::Foreign(why) => Err(Error::new(why)),
    }
}

/// What a read of at most some bytes of a vault server's answer heard
/// ([`observe_after_nowait_within`], [`get_within_as`], [`look_within`],
/// [`fetch_records_within`]).
#[cfg(any(unix, test))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Pulled<A = Fetched> {
    /// The answer, read whole: what [`observe_after_nowait`] returns for it, for [`look_within`]
    /// what [`get_within`] does, or for [`fetch_records_within`] what [`fetch_records_sync`]
    /// does.
    Fetched(A),
    /// An answer, of any status, whose body is longer than the limit given: no more of it than
    /// that was read (none, when the answer declares its length).
    Larger,
}

/// [`observe_after_nowait`], reading a body of at most `limit` bytes (and never more than
/// [`MAX_ENVELOPE`]): a longer one is [`Pulled::Larger`], where [`observe_after_nowait`] takes it
/// for an answer no vault server gives. A server pulling from all its peers at once reads each
/// one's answer so, under a small limit, and reads a longer one whole only under a slot of a few
/// (`crate::server`'s envelope pulls).
#[cfg(any(unix, test))]
pub(crate) fn observe_after_nowait_within(
    url: &str,
    after: Option<u64>,
    total: Duration,
    limit: usize,
) -> crate::Result<Pulled> {
    pulled(
        get_request(
            &client(total),
            url,
            after,
            Some(PROBE_CONNECT.min(total)),
            None,
            true,
            true,
        ),
        url,
        after,
        None,
        limit,
    )
}

/// `request`, a GET of the vault at `url` (asking after `after`, for `subscriber`), sent reading
/// a body of at most `limit` bytes and never more than [`MAX_ENVELOPE`]: a longer one is
/// [`Pulled::Larger`], and an answer no vault server gives is an error.
#[cfg(any(unix, test))]
fn pulled(
    request: aterm_http::RequestBuilder<'_>,
    url: &str,
    after: Option<u64>,
    subscriber: Option<&str>,
    limit: usize,
) -> crate::Result<Pulled> {
    match pulled_answer(request, url, after, subscriber, limit)? {
        Pulled::Fetched(Answer::Fetched(fetched)) => Ok(Pulled::Fetched(fetched)),
        Pulled::Fetched(Answer::Foreign(why)) => Err(Error::new(why)),
        Pulled::Larger => Ok(Pulled::Larger),
    }
}

/// [`pulled`], keeping an answer no vault server gives as the [`Answer::Foreign`] [`get`] makes of
/// it.
#[cfg(any(unix, test))]
fn pulled_answer(
    request: aterm_http::RequestBuilder<'_>,
    url: &str,
    after: Option<u64>,
    subscriber: Option<&str>,
    limit: usize,
) -> crate::Result<Pulled<Answer>> {
    let sent = request.limit(limit.min(MAX_ENVELOPE)).send_phased(&[]);
    if let Err(aterm_http::SendError {
        error: aterm_http::Error::TooLarge { .. },
        ..
    }) = &sent
    {
        return Ok(Pulled::Larger);
    }
    classify_get_response(sent, url, after, subscriber)
        .map(|observed| Pulled::Fetched(observed.answer))
        .map_err(|unheard| unheard.why)
}

/// [`fetch_within`] as a non-consuming probe ([`OBSERVER_HEADER`]): the server answers it as any
/// GET but keeps no heard record of it, so it never counts as a reader of the vault (F20).
#[cfg(unix)]
pub(crate) fn observe_within(url: &str, total: Duration) -> crate::Result<Fetched> {
    match get_via_purpose(&client(total), url, None, None, None, false, true)
        .map(|observed| observed.answer)
        .map_err(|unheard| unheard.why)?
    {
        Answer::Fetched(fetched) => Ok(fetched),
        Answer::Foreign(why) => Err(Error::new(why)),
    }
}

/// What one GET of a vault's URL heard ([`get`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// What [`fetch`] returns for it.
    Fetched(Fetched),
    /// An answer the vault server never gives a GET (e.g. a redirect).
    Foreign(String),
}

/// Execute one GET of the vault at `url` ([`fetch`], [`Answer::Foreign`]).
pub fn get(url: &str, after: Option<u64>, wait: Duration) -> crate::Result<Answer> {
    get_bounded(url, after, wait, None).map_err(|unheard| unheard.why)
}

/// Why a GET heard no answer ([`get_bounded`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unheard {
    /// What failed, as [`get`] says it.
    pub why: Error,
}

/// [`get`], with its DNS lookup, connect and TLS handshake bounded by `connect` within the
/// request's deadline.
pub fn get_bounded(
    url: &str,
    after: Option<u64>,
    wait: Duration,
    connect: Option<Duration>,
) -> Result<Answer, Unheard> {
    get_via(
        &client(wait.saturating_add(fetch_margin())),
        url,
        after,
        connect,
        None,
    )
}

/// [`get_bounded`] of the whole vault as a look: a server keeps no heard record of it
/// ([`get_within`]).
pub fn look_bounded(
    url: &str,
    wait: Duration,
    connect: Option<Duration>,
) -> Result<Answer, Unheard> {
    get_via_purpose(
        &client(wait.saturating_add(fetch_margin())),
        url,
        None,
        connect,
        None,
        false,
        true,
    )
    .map(|observed| observed.answer)
}

/// One GET of the vault at `url` as a look, never a long-poll: all of it within `total`, with
/// no [`FETCH_MARGIN`], its lookup and connect within [`PROBE_CONNECT`] (or `total`, if
/// shorter). A server keeps no heard record of a look ([`OBSERVER_HEADER`]): it is never a plain
/// read a `0.145` Mac may have made (F20). A join that takes the envelope a look read names itself
/// at its first sync.
pub fn get_within(url: &str, total: Duration) -> crate::Result<Answer> {
    get_via_purpose(
        &client(total),
        url,
        None,
        Some(PROBE_CONNECT.min(total)),
        None,
        false,
        true,
    )
    .map(|observed| observed.answer)
    .map_err(|unheard| unheard.why)
}

/// [`get_within`], reading a body of at most `limit` bytes (and never more than
/// [`MAX_ENVELOPE`]): a longer one is [`Pulled::Larger`], where [`get_within`] takes it for an
/// answer no vault server gives ([`Answer::Foreign`], saying [`past_any_envelope`] of one past
/// [`MAX_ENVELOPE`]). A join's probe of many servers at once reads each so, under a small limit
/// first (`crate::client`'s `probes_within`).
#[cfg(unix)]
pub(crate) fn look_within(
    url: &str,
    total: Duration,
    limit: usize,
) -> crate::Result<Pulled<Answer>> {
    pulled_answer(
        get_request(
            &client(total),
            url,
            None,
            Some(PROBE_CONNECT.min(total)),
            None,
            false,
            true,
        ),
        url,
        None,
        None,
        limit,
    )
}

/// What [`get_within`] says of `url`'s answer when its body is longer than [`MAX_ENVELOPE`]
/// ([`Answer::Foreign`]): an answer a read of at most that much ([`look_within`]) takes for
/// [`Pulled::Larger`].
#[cfg(unix)]
pub(crate) fn past_any_envelope(url: &str) -> String {
    transport_error(
        "fetching",
        url,
        &aterm_http::Error::TooLarge {
            limit: MAX_ENVELOPE,
        },
    )
    .0
}

/// One GET of the vault at `url` for an identified consumer (its `age1…` recipient in
/// [`SUBSCRIBER_HEADER`] when `Some`), never a look: all of it within `total`, its lookup and
/// connect within `connect` where given, reading a body of at most `limit` bytes (and never more
/// than [`MAX_ENVELOPE`]). A longer one is [`Pulled::Larger`], and an answer no vault server
/// gives is an error. A client reading many servers' envelopes at once reads each so, under a
/// small limit first (`crate::client`'s `fan_out::envelopes_within`).
#[cfg(unix)]
pub(crate) fn get_within_as(
    url: &str,
    total: Duration,
    connect: Option<Duration>,
    subscriber: Option<&str>,
    limit: usize,
) -> crate::Result<Pulled> {
    pulled(
        get_request(&client(total), url, None, connect, subscriber, false, false),
        url,
        None,
        subscriber,
        limit,
    )
}

/// [`get_bounded`] through `client` (its timeout is the request's deadline).
fn get_via(
    client: &Client,
    url: &str,
    after: Option<u64>,
    connect: Option<Duration>,
    subscriber: Option<&str>,
) -> Result<Answer, Unheard> {
    get_via_with(client, url, after, connect, subscriber, false)
}

fn get_via_with(
    client: &Client,
    url: &str,
    after: Option<u64>,
    connect: Option<Duration>,
    subscriber: Option<&str>,
    no_wait: bool,
) -> Result<Answer, Unheard> {
    get_observed_via_with(client, url, after, connect, subscriber, no_wait)
        .map(|observed| observed.answer)
}

fn get_observed_via_with(
    client: &Client,
    url: &str,
    after: Option<u64>,
    connect: Option<Duration>,
    subscriber: Option<&str>,
    no_wait: bool,
) -> Result<ObservedAnswer, Unheard> {
    get_via_purpose(client, url, after, connect, subscriber, no_wait, false)
}

fn get_via_purpose(
    client: &Client,
    url: &str,
    after: Option<u64>,
    connect: Option<Duration>,
    subscriber: Option<&str>,
    no_wait: bool,
    observer: bool,
) -> Result<ObservedAnswer, Unheard> {
    let sent = get_request(client, url, after, connect, subscriber, no_wait, observer)
        .limit(MAX_ENVELOPE)
        .send_phased(&[]);
    classify_get_response(
        sent,
        url,
        after,
        #[cfg(any(unix, test))]
        subscriber,
    )
}

/// One GET of the vault at `url` through `client`, as [`get_via_purpose`] asks it, with its
/// body limit left for the caller to set.
fn get_request<'c>(
    client: &'c Client,
    url: &str,
    after: Option<u64>,
    connect: Option<Duration>,
    subscriber: Option<&str>,
    no_wait: bool,
    observer: bool,
) -> aterm_http::RequestBuilder<'c> {
    let target = match after {
        Some(seq) => with_query(url, &format!("after={seq}")),
        None => base(url).to_owned(),
    };
    let request = client.get(&target);
    let request = match subscriber {
        Some(subscriber) => request.header(SUBSCRIBER_HEADER, subscriber),
        None => request,
    };
    let request = if observer {
        request.header(OBSERVER_HEADER, "1")
    } else {
        request
    };
    let request = if no_wait {
        request.header(NO_WAIT_HEADER, "1")
    } else {
        request
    };
    match connect {
        Some(connect) => request.connect_within(connect),
        None => request,
    }
}

/// Shared response classifier: only its successful actual 204 branch can mint a reply token.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultHttp204",
        action = "Receive",
        project = "aterm_secrets::client::http204_observation::conformance_tests::http204_transition"
    )
)]
fn classify_get_response(
    response: Result<aterm_http::Response, aterm_http::SendError>,
    url: &str,
    after: Option<u64>,
    #[cfg(any(unix, test))] subscriber: Option<&str>,
) -> Result<ObservedAnswer, Unheard> {
    let response = match response {
        Ok(response) => response,
        // A long-poll the server still held at the request's deadline (one held longer than
        // the asker waits, by an older server): nothing newer was heard.
        Err(failed) if after.is_some() && held_past_deadline(&failed) => {
            return Ok(Answer::Fetched(Fetched::NoNewer(None)).into());
        }
        Err(aterm_http::SendError { error, .. }) if foreign_reply(&error) => {
            return Ok(Answer::Foreign(transport_error("fetching", url, &error).0).into());
        }
        Err(aterm_http::SendError { error, .. }) => {
            return Err(Unheard {
                why: transport_error("fetching", url, &error),
            });
        }
    };
    // Sample only a completed response, never a request start, synthetic timeout or later read.
    #[cfg(any(unix, test))]
    let http204 = if response.status() == 204 {
        aterm_time::unix_millis()
            .filter(|at| *at > 0 && *at <= i64::MAX as u64)
            .map(|responded_at_ms| NoContentReply {
                request: request_binding(url, after, subscriber),
                responded_at_ms,
            })
    } else {
        None
    };
    let answer = match response.status() {
        200 => Answer::Fetched(Fetched::Body(response.into_body())),
        204 => Answer::Fetched(Fetched::NoNewer(response.header("etag").and_then(etag_seq))),
        404 => Answer::Fetched(Fetched::Missing),
        status if foreign_status(status) => {
            Answer::Foreign(refused("fetching", url, status, response.body()).0)
        }
        status => {
            return Err(Unheard {
                why: refused("fetching", url, status, response.body()),
            });
        }
    };
    Ok(ObservedAnswer {
        answer,
        #[cfg(any(unix, test))]
        http204,
    })
}

/// Exercise the production classifier's failure branches without constructing reply tokens.
#[cfg(all(test, unix))]
pub(crate) fn test_classify_get_response(
    response: Result<aterm_http::Response, aterm_http::SendError>,
    url: &str,
    after: Option<u64>,
    subscriber: Option<&str>,
) -> Result<ObservedAnswer, Unheard> {
    classify_get_response(response, url, after, subscriber)
}

/// Whether `failed` is a request the server took and never answered by its deadline: the
/// whole request went out ([`aterm_http::Phase::Sent`]) and its time ran out.
fn held_past_deadline(failed: &aterm_http::SendError) -> bool {
    failed.phase == aterm_http::Phase::Sent
        && matches!(&failed.error, aterm_http::Error::Io(io) if io.kind() == std::io::ErrorKind::TimedOut)
}

/// The seq an `ETag` names: `"<seq>"`, as the server sends it; anything else names none.
fn etag_seq(value: &str) -> Option<u64> {
    let digits = value.trim().strip_prefix('"')?.strip_suffix('"')?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// A status the vault server never answers a GET with.
fn foreign_status(status: u16) -> bool {
    (300..400).contains(&status) || matches!(status, 401 | 403)
}

/// Whether `err` is an answer the vault server never gives ([`aterm_http::Error::Io`]).
const fn foreign_reply(error: &aterm_http::Error) -> bool {
    matches!(
        error,
        aterm_http::Error::Protocol(_) | aterm_http::Error::TooLarge { .. }
    )
}

#[cfg(unix)]
fn read_path(path: &std::path::Path) -> crate::Result<Option<Vec<u8>>> {
    crate::fsx::read_public(path, MAX_ENVELOPE)
}

#[cfg(not(unix))]
fn read_path(path: &std::path::Path) -> crate::Result<Option<Vec<u8>>> {
    Err(Error::new(format!(
        "{}: a vault at a path is read on Unix only",
        path.display()
    )))
}

/// Fetch membership records missing past causal frontier `have` (`GET <url>/records?have=…`,
/// `docs/DESIGN-fluid-swarm-2026-10-28.md` §4.1 F13), as a caller that keeps no membership log
/// of its own states its records ([`crate::crossing::RecordsBound::uncrossed`]): a test, or a
/// lab reading what a server holds. A member's and a server's pulls state their own.
///
/// With `wait = None`, sends [`NO_WAIT_HEADER`] so peers answer immediately; with `wait = Some(w)`,
/// long-polls up to `w` (`total` bounds the HTTP exchange). Answers `204` and `404` (including
/// `0.145` servers without `/records`) return an empty byte vector.
///
/// # Errors
/// Transport failure, oversized body, incompatible records capacity, or a
/// non-200/204/404 status from the server.
pub fn fetch_records(
    url: &str,
    have: &[crate::membership_log::RecordId],
    wait: Option<Duration>,
    total: Duration,
    subscriber: Option<&str>,
) -> crate::Result<Vec<u8>> {
    fetch_records_sync(
        url,
        have,
        wait,
        total,
        subscriber,
        crate::crossing::RecordsBound::uncrossed(),
    )
    .map(|answer| answer.bytes)
}

/// What a server answered a records GET ([`fetch_records_sync`]).
#[cfg_attr(
    not(unix),
    expect(
        dead_code,
        reason = "only the unix record transport reads `needs` and `boot`"
    )
)]
pub(crate) struct RecordsAnswer {
    /// The records it sent: empty for a `204`, a `404` or a long poll held past its deadline.
    pub(crate) bytes: Vec<u8>,
    /// Whether it lacks at least one record the GET's `have` named ([`RECORDS_NEED_HEADER`]).
    pub(crate) needs: bool,
    /// The boot id it sent ([`RECORDS_BOOT_HEADER`]): `None` from a server before F23, or one
    /// that sent no 32 lowercase hex digits there.
    pub(crate) boot: Option<String>,
    /// What it stated of its records ([`RECORDS_BOUND_HEADER`]).
    pub(crate) bound: ReplyBound,
}

impl RecordsAnswer {
    /// No answer: a long poll held past its deadline.
    const fn none() -> Self {
        Self::stating(ReplyBound::Unheard)
    }

    /// An answer that brings nothing and states `bound`.
    const fn stating(bound: ReplyBound) -> Self {
        Self {
            bytes: Vec::new(),
            needs: false,
            boot: None,
            bound,
        }
    }
}

/// A completely received records response whose valid frames already own their wire bytes.
#[cfg(unix)]
pub(crate) struct ReceivedRecordsAnswer {
    pub(crate) received: crate::membership_log::untaken::ReceivedFrames,
    pub(crate) needs: bool,
    pub(crate) boot: Option<String>,
    /// What the server stated of its records ([`RECORDS_BOUND_HEADER`]).
    pub(crate) bound: ReplyBound,
}

#[cfg(unix)]
impl ReceivedRecordsAnswer {
    /// No answer: a long poll held past its deadline.
    fn none() -> Self {
        Self {
            received: crate::membership_log::untaken::ReceivedFrames::empty(),
            needs: false,
            boot: None,
            bound: ReplyBound::Unheard,
        }
    }
}

/// One records GET ([`records_request`]): the ids it names, how long the server may hold it and
/// the exchange may take, the subscriber it reads for, what it states of the puller's own
/// records, and the most of the answer it reads.
#[derive(Clone, Copy)]
struct Pull<'a> {
    have: &'a [crate::membership_log::RecordId],
    wait: Option<Duration>,
    total: Duration,
    subscriber: Option<&'a str>,
    bound: crate::crossing::RecordsBound,
    limit: usize,
}

fn records_exchange(
    url: &str,
    pull: Pull<'_>,
) -> Result<aterm_http::Exchange, aterm_http::SendError> {
    let deadline = pull.wait.map_or(pull.total, |wait| {
        wait.saturating_add(fetch_margin()).max(pull.total)
    });
    let http = client(deadline);
    records_request(&http, url, pull).exchange(aterm_http::Body::Empty)
}

fn completed_records<T>(
    response: Result<T, aterm_http::SendError>,
    url: &str,
    wait: Option<Duration>,
) -> crate::Result<Option<T>> {
    match response {
        Ok(response) => Ok(Some(response)),
        Err(failed) if wait.is_some() && held_past_deadline(&failed) => Ok(None),
        Err(aterm_http::SendError { error, .. }) => {
            Err(transport_error("fetching records from", url, &error))
        }
    }
}

#[cfg(unix)]
enum RecordReception {
    Received(ReceivedRecordsAnswer),
    Other(aterm_http::Response),
    Refused(Error),
}

#[cfg(unix)]
impl RecordReception {
    fn read(
        exchange: aterm_http::Exchange,
        local: crate::crossing::RecordsBound,
        url: &str,
    ) -> Result<Self, aterm_http::SendError> {
        Self::read_with_known(exchange, local, url, |_| None)
    }

    fn read_with_known(
        exchange: aterm_http::Exchange,
        local: crate::crossing::RecordsBound,
        url: &str,
        known: impl FnMut(&crate::membership_log::RecordId) -> Option<crate::membership_log::Record>,
    ) -> Result<Self, aterm_http::SendError> {
        let bound = ReplyBound::of(exchange.header_all(RECORDS_BOUND_HEADER));
        if matches!(exchange.status(), 200 | 204 | 404)
            && let Err(error) = check_records_reply(local, bound, url)
        {
            return Ok(Self::Refused(error));
        }
        if exchange.status() != 200 {
            return exchange.collect().map(Self::Other);
        }
        let (needs, boot) = records_headers(
            exchange.header(RECORDS_NEED_HEADER),
            exchange.header(RECORDS_BOOT_HEADER),
        );
        let received = exchange.consume_body(|reader| {
            crate::membership_log::untaken::ReceivedFrames::read_from_with_known(reader, known)
        })?;
        Ok(Self::Received(ReceivedRecordsAnswer {
            received,
            needs,
            boot,
            bound,
        }))
    }

    fn into_answer(
        self,
        url: &str,
        local: crate::crossing::RecordsBound,
    ) -> crate::Result<ReceivedRecordsAnswer> {
        match self {
            Self::Received(answer) => Ok(answer),
            Self::Refused(error) => Err(error),
            Self::Other(response) => {
                records_answer(response, url, local).map(|answer| ReceivedRecordsAnswer {
                    received: crate::membership_log::untaken::ReceivedFrames::empty(),
                    needs: answer.needs,
                    boot: answer.boot,
                    bound: answer.bound,
                })
            }
        }
    }
}

/// [`fetch_records`] stating `bound` of the puller's records, also returning whether the server
/// signaled [`RECORDS_NEED_HEADER`] (`"1"`) because it lacks at least one record named in
/// `have`, the boot id it sent, and what it stated of its own records.
pub(crate) fn fetch_records_sync(
    url: &str,
    have: &[crate::membership_log::RecordId],
    wait: Option<Duration>,
    total: Duration,
    subscriber: Option<&str>,
    bound: crate::crossing::RecordsBound,
) -> crate::Result<RecordsAnswer> {
    let pull = Pull {
        have,
        wait,
        total,
        subscriber,
        bound,
        limit: crate::membership_log::MAX_RECORDS_STREAM,
    };
    let response = records_exchange(url, pull).and_then(aterm_http::Exchange::collect);
    match completed_records(response, url, wait)? {
        Some(response) => records_answer(response, url, bound),
        None => Ok(RecordsAnswer::none()),
    }
}

/// The ordinary client records pull, stating `bound` of the puller's records and staging frames
/// while the response is read.
#[cfg(unix)]
pub(crate) fn fetch_record_frames_sync(
    url: &str,
    have: &[crate::membership_log::RecordId],
    wait: Option<Duration>,
    total: Duration,
    subscriber: Option<&str>,
    bound: crate::crossing::RecordsBound,
) -> crate::Result<ReceivedRecordsAnswer> {
    fetch_record_frames_with_known(url, have, wait, total, subscriber, bound, |_| None)
}

/// A records pull that can reuse exact immutable owners already held in memory.
#[cfg(unix)]
pub(crate) fn fetch_record_frames_with_known(
    url: &str,
    have: &[crate::membership_log::RecordId],
    wait: Option<Duration>,
    total: Duration,
    subscriber: Option<&str>,
    bound: crate::crossing::RecordsBound,
    known: impl FnMut(&crate::membership_log::RecordId) -> Option<crate::membership_log::Record>,
) -> crate::Result<ReceivedRecordsAnswer> {
    let pull = Pull {
        have,
        wait,
        total,
        subscriber,
        bound,
        limit: crate::membership_log::MAX_RECORDS_STREAM,
    };
    let response = records_exchange(url, pull)
        .and_then(|exchange| RecordReception::read_with_known(exchange, bound, url, known));
    match completed_records(response, url, wait)? {
        Some(response) => response.into_answer(url, bound),
        None => Ok(ReceivedRecordsAnswer::none()),
    }
}

/// [`fetch_record_frames_sync`] of a server that does not wait ([`NO_WAIT_HEADER`]), reading a body of at
/// most `limit` bytes (and never more than [`crate::membership_log::MAX_RECORDS_STREAM`]): a longer
/// one is [`Pulled::Larger`] (past [`crate::membership_log::MAX_RECORDS_STREAM`], where
/// [`fetch_records_sync`] fails as [`past_any_records_stream`] says). A records pass reads its
/// peers so, all at once under a small limit first (`crate::client`'s `fan_out::RECORDS`).
#[cfg(unix)]
pub(crate) fn fetch_records_within(
    url: &str,
    have: &[crate::membership_log::RecordId],
    total: Duration,
    subscriber: Option<&str>,
    bound: crate::crossing::RecordsBound,
    limit: usize,
) -> crate::Result<Pulled<ReceivedRecordsAnswer>> {
    let pull = Pull {
        have,
        wait: None,
        total,
        subscriber,
        bound,
        limit,
    };
    match records_exchange(url, pull)
        .and_then(|exchange| RecordReception::read(exchange, bound, url))
    {
        Ok(response) => response.into_answer(url, bound).map(Pulled::Fetched),
        Err(aterm_http::SendError {
            error: aterm_http::Error::TooLarge { .. },
            ..
        }) => Ok(Pulled::Larger),
        Err(aterm_http::SendError { error, .. }) => {
            Err(transport_error("fetching records from", url, &error))
        }
    }
}

/// How [`fetch_records_sync`] fails at `url` when the records it answers are longer than
/// [`crate::membership_log::MAX_RECORDS_STREAM`], which [`fetch_records_within`] takes for
/// [`Pulled::Larger`].
#[cfg(unix)]
pub(crate) fn past_any_records_stream(url: &str) -> Error {
    transport_error(
        "fetching records from",
        url,
        &aterm_http::Error::TooLarge {
            limit: crate::membership_log::MAX_RECORDS_STREAM,
        },
    )
}

/// A records GET of `url` naming `pull.have` (`GET <url>/records?have=…`) on `http`, stating
/// `pull.bound` ([`RECORDS_BOUND_HEADER`]), for `pull.subscriber` where given, reading a body of
/// at most `pull.limit` bytes (never more than [`crate::membership_log::MAX_RECORDS_STREAM`]).
/// With no `pull.wait`, it asks the server not to ([`NO_WAIT_HEADER`]) and dials within
/// [`PROBE_CONNECT`] (or `pull.total`, where shorter).
fn records_request<'a>(
    http: &'a Client,
    url: &str,
    pull: Pull<'_>,
) -> aterm_http::RequestBuilder<'a> {
    let request = http
        .get(&records_have_url(url, pull.have))
        .header(RECORDS_BOUND_HEADER, &pull.bound.value())
        .limit(pull.limit.min(crate::membership_log::MAX_RECORDS_STREAM));
    let request = match pull.subscriber {
        Some(sub) => request.header(SUBSCRIBER_HEADER, sub),
        None => request,
    };
    if pull.wait.is_none() {
        request
            .header(NO_WAIT_HEADER, "1")
            .connect_within(PROBE_CONNECT.min(pull.total))
    } else {
        request
    }
}

/// What `response`, `url`'s to a records GET ([`records_request`]), says.
fn records_answer(
    response: aterm_http::Response,
    url: &str,
    local: crate::crossing::RecordsBound,
) -> crate::Result<RecordsAnswer> {
    let (needs, boot) = records_headers(
        response.header(RECORDS_NEED_HEADER),
        response.header(RECORDS_BOOT_HEADER),
    );
    let bound = ReplyBound::of(response.header_all(RECORDS_BOUND_HEADER));
    if matches!(response.status(), 200 | 204 | 404) {
        check_records_reply(local, bound, url)?;
    }
    match response.status() {
        200 => Ok(RecordsAnswer {
            bytes: response.into_body(),
            needs,
            boot,
            bound,
        }),
        204 => Ok(RecordsAnswer {
            bytes: Vec::new(),
            needs,
            boot,
            bound,
        }),
        404 => Ok(RecordsAnswer::stating(bound)),
        status => Err(refused(
            "fetching records from",
            url,
            status,
            response.body(),
        )),
    }
}

fn records_headers(needs: Option<&str>, boot: Option<&str>) -> (bool, Option<String>) {
    let boot = boot
        .filter(|id| id.len() == 32 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
        .map(str::to_owned);
    (needs == Some("1"), boot)
}

/// What `GET <url>/heard` names a subscriber by: lowercase hex of SHA-256 over a domain tag and
/// its recipient's 32 bytes. A Mac that holds a recipient can tell whether it was heard; one that
/// holds none learns no key from the list.
#[must_use]
pub fn heard_tag(recipient: &crate::age::Recipient) -> String {
    let mut bytes = Vec::with_capacity(HEARD_TAG_DOMAIN.len() + 32);
    bytes.extend_from_slice(HEARD_TAG_DOMAIN);
    bytes.extend_from_slice(&recipient.0);
    aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&bytes))
}

const HEARD_TAG_DOMAIN: &[u8] = b"aterm-secrets heard v1\0";
pub(crate) const HEARD_COVERAGE_SECS: u64 = 14 * 24 * 60 * 60;
/// The most subscriber last-heard records a server keeps per vault (`<name>.heard`), so the most
/// rows a heard history carries ([`fetch_heard`]). Here rather than in the Unix-only server, as
/// `fetch_heard` is built on every platform.
pub const MAX_HEARD: usize = 512;

/// A complete server history, with timestamps in that server's clock domain.
#[derive(Debug, PartialEq, Eq)]
pub struct Heard {
    /// The server's clock when it answered. Compare ages against this clock, not another Mac's.
    pub as_of: u64,
    /// Each subscriber's hashed tag and latest observed timestamp.
    pub subscribers: std::collections::BTreeMap<String, u64>,
    /// Latest legacy-style envelope read with no subscriber identity or observer purpose.
    pub anonymous: Option<u64>,
}

/// The subscribers the server at `url` heard for its vault within its heard window
/// (`GET <url>/heard`): each one's [`heard_tag`] and last time heard, in unix seconds. `Ok(None)`
/// where the server answers `404`, as one older than the route does, so a caller reads that as
/// unknown. A versioned coverage header must attest the full legacy window; prototype empty
/// replies and malformed answers are not evidence that no subscriber was heard.
///
/// # Errors
/// Transport fails, the server refuses with another status, or its coverage header, row count,
/// UTF-8 or a nonempty row is invalid.
pub fn fetch_heard(url: &str) -> crate::Result<Option<Heard>> {
    let base_url = base(url);
    let target = match base_url.split_once('?') {
        Some((path, query)) => format!("{path}/heard?{query}"),
        None => format!("{base_url}/heard"),
    };
    let response = client(EXCHANGE)
        .get(&target)
        .limit(256 * 1024)
        .connect_within(PROBE_CONNECT)
        .send_phased(&[])
        .map_err(|failed| transport_error("reading who was heard from", url, &failed.error))?;
    match response.status() {
        200 => {}
        404 => return Ok(None),
        status => {
            return Err(refused(
                "reading who was heard from",
                url,
                status,
                response.body(),
            ));
        }
    }
    let mut heard = std::collections::BTreeMap::new();
    let text = std::str::from_utf8(response.body())
        .map_err(|_| Error::new("the server's heard history is not UTF-8"))?;
    let (head, rows) = text
        .split_once('\n')
        .ok_or_else(|| Error::new("the server's heard history has no coverage header"))?;
    let header: Vec<_> = head.split_ascii_whitespace().collect();
    let ["aterm-heard-v2", since, as_of, count, anonymous] = header.as_slice() else {
        return Err(Error::new(
            "the server's heard history has an unknown coverage header",
        ));
    };
    let since = since
        .parse::<u64>()
        .map_err(|_| Error::new("invalid heard coverage start"))?;
    let as_of = as_of
        .parse::<u64>()
        .map_err(|_| Error::new("invalid heard coverage time"))?;
    let count = count
        .parse::<usize>()
        .map_err(|_| Error::new("invalid heard coverage count"))?;
    let anonymous = if *anonymous == "-" {
        None
    } else {
        Some(
            anonymous
                .parse::<u64>()
                .map_err(|_| Error::new("invalid anonymous heard time"))?,
        )
    };
    if since == 0 || as_of.saturating_sub(since) < HEARD_COVERAGE_SECS || count > MAX_HEARD {
        return Err(Error::new(
            "the server's heard history does not cover the legacy window",
        ));
    }
    let mut seen = 0;
    for line in rows.lines().filter(|line| !line.trim().is_empty()) {
        let mut words = line.split_ascii_whitespace();
        let (Some(tag), Some(at), None) = (words.next(), words.next(), words.next()) else {
            return Err(Error::new("the server's heard history has a malformed row"));
        };
        let Ok(at) = at.parse::<u64>() else {
            return Err(Error::new("the server's heard history has an invalid time"));
        };
        if tag.len() != 64 || !tag.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::new("the server's heard history has an invalid tag"));
        }
        let last = heard.entry(tag.to_ascii_lowercase()).or_insert(0);
        *last = (*last).max(at);
        seen += 1;
    }
    if seen != count {
        return Err(Error::new("the server's heard history is incomplete"));
    }
    Ok(Some(Heard {
        as_of,
        subscribers: heard,
        anonymous,
    }))
}

/// Post a self-delimiting stream of signed membership records (`POST <url>/records`), as a caller
/// that keeps no membership log of its own states its records
/// ([`crate::crossing::RecordsBound::uncrossed`]): a test, or a lab feeding a server. A member's
/// posts state its own ([`post_records_answered_until`]).
///
/// # Errors
/// `records` exceeds [`crate::membership_log::MAX_RECORDS_STREAM`], transport fails, or the server
/// refuses the records (`400`, `403`, `507`, …).
pub fn post_records(url: &str, records: &[u8]) -> crate::Result<()> {
    post_records_answered(url, records)?
}

/// [`post_records`], telling a server that judged the records from one that did not: `Ok` holds
/// the server's verdict (`Ok(())` took the records, `Err` refused them: a record it does not
/// take, a store or a server at its bound, a vault it keeps for no one here), and `Err` is a
/// batch too large to send, a POST that heard no answer, or an answer that judged nothing (`408`,
/// `429`, or a `5xx` but `507`: busy reading other bodies, slow, failing to write), after which
/// the same POST may land.
///
/// # Errors
/// `records` exceeds [`crate::membership_log::MAX_RECORDS_STREAM`], transport fails, or the server
/// answers `408`, `429` or a `5xx` other than `507`.
pub(crate) fn post_records_answered(url: &str, records: &[u8]) -> crate::Result<crate::Result<()>> {
    post_records_answered_until(
        url,
        records,
        crate::crossing::RecordsBound::uncrossed(),
        None,
    )
}

/// [`post_records_answered`], stating `bound` of the poster's records ([`RECORDS_BOUND_HEADER`];
/// a crossed one posts with [`CROSSED_POST_QUERY`], which a server before the fence refuses
/// unread), the whole exchange ending by `deadline` where one is given (at most [`EXCHANGE`]
/// from now): a post that starts once `deadline` has passed dials nothing and answers
/// [`ErrorKind::TimedOut`], so the posts of one fan-out that share a deadline end by it however
/// many servers take a post and never answer.
///
/// # Errors
/// As [`post_records_answered`], and [`ErrorKind::TimedOut`] once `deadline` has passed.
pub(crate) fn post_records_answered_until(
    url: &str,
    records: &[u8],
    bound: crate::crossing::RecordsBound,
    deadline: Option<std::time::Instant>,
) -> crate::Result<crate::Result<()>> {
    post_records_body_answered_until(url, aterm_http::Body::Bytes(records), bound, deadline)
}

/// Post immutable selected records without collecting their wire stream, as
/// [`post_records_answered_until`] posts a stream.
#[cfg(unix)]
pub(crate) fn post_record_selection_answered_until(
    url: &str,
    records: &[crate::membership_log::Record],
    bound: crate::crossing::RecordsBound,
    deadline: Option<std::time::Instant>,
) -> crate::Result<crate::Result<()>> {
    let mut reader = records_body::RecordsReader::new(records)?;
    let len = reader.len() as u64;
    post_records_body_answered_until(
        url,
        aterm_http::Body::Reader {
            len,
            reader: &mut reader,
        },
        bound,
        deadline,
    )
}

fn post_records_body_answered_until(
    url: &str,
    body: aterm_http::Body<'_>,
    bound: crate::crossing::RecordsBound,
    deadline: Option<std::time::Instant>,
) -> crate::Result<crate::Result<()>> {
    let max = crate::membership_log::MAX_RECORDS_STREAM;
    let len = match &body {
        aterm_http::Body::Empty => 0,
        aterm_http::Body::Bytes(bytes) => bytes.len() as u64,
        aterm_http::Body::Reader { len, .. } => *len,
    };
    if len > max as u64 {
        return Err(Error::new(format!(
            "the record batch is larger than {max} bytes"
        )));
    }
    let within = match deadline {
        Some(deadline) => deadline
            .checked_duration_since(std::time::Instant::now())
            .filter(|left| !left.is_zero())
            .ok_or_else(|| {
                Error::of_kind(
                    ErrorKind::TimedOut,
                    format!(
                        "posting records to {}: the deadline passed before it began",
                        describe(url)
                    ),
                )
            })?
            .min(EXCHANGE),
        None => EXCHANGE,
    };
    let target = if bound.crossed {
        with_query(&records_url(url), CROSSED_POST_QUERY)
    } else {
        records_url(url)
    };
    let response = client(within)
        .post(&target)
        .header("Content-Type", "application/octet-stream")
        .header(RECORDS_BOUND_HEADER, &bound.value())
        .limit(REASON_BODY)
        .send_body(body)
        .map_err(|e| transport_error("posting records to", url, &e))?;
    if response.is_success() {
        check_records_reply(
            bound,
            ReplyBound::of(response.header_all(RECORDS_BOUND_HEADER)),
            url,
        )?;
        return Ok(Ok(()));
    }
    let status = response.status();
    let error = refused("posting records to", url, status, response.body());
    Ok(Err(match status {
        403 | 507 => Error::of_kind(ErrorKind::Refused, error.0),
        408 | 429 | 500..=599 => return Err(error),
        _ => error,
    }))
}

/// Publish an envelope (`PUT <url>`).
pub fn publish(source: &Source, envelope: &[u8]) -> crate::Result<()> {
    publish_until(source, envelope, None)
}

pub(crate) fn publish_until(
    source: &Source,
    envelope: &[u8],
    deadline: Option<std::time::Instant>,
) -> crate::Result<()> {
    let wait = match deadline {
        Some(deadline) => {
            let wait = deadline.saturating_duration_since(std::time::Instant::now());
            if wait.is_zero() {
                return Err(Error::new("vault publication deadline expired"));
            }
            wait
        }
        None => EXCHANGE,
    };
    if envelope.len() > MAX_ENVELOPE {
        return Err(Error::new(format!(
            "the envelope is larger than {MAX_ENVELOPE} bytes"
        )));
    }
    match source {
        Source::Url(url) => {
            let response = client(wait)
                .put(base(url))
                .header("Content-Type", "text/plain; charset=utf-8")
                .limit(REASON_BODY)
                .send(envelope)
                .map_err(|e| transport_error("publishing to", url, &e))?;
            if response.is_success() {
                return Ok(());
            }
            let error = refused("publishing to", url, response.status(), response.body());
            // The server answers 409, 403 and 507 only before it stores anything: 409 for
            // another publish ([`ErrorKind::NotStored`]), the others for this one
            // ([`ErrorKind::Refused`]).
            Err(match response.status() {
                409 => Error::of_kind(crate::ErrorKind::NotStored, error.0),
                403 | 507 => Error::of_kind(crate::ErrorKind::Refused, error.0),
                _ => error,
            })
        }
        Source::Path(path) => write_path(path, envelope),
    }
}

/// Publish retained version-one bytes without replacing a path that appeared
/// after the caller's absence check. The HTTP server enforces its signer and
/// version rules; its response still cannot prove global uniqueness. A failure
/// that cannot have stored `bytes` is [`ErrorKind::Refused`](crate::ErrorKind::Refused):
/// the server's 403 or 507, or a path that does not hold `bytes` afterwards.
#[cfg(unix)]
pub(crate) fn publish_initial(source: &Source, bytes: &[u8]) -> crate::Result<()> {
    if bytes.len() > MAX_ENVELOPE || crate::envelope::parse(bytes)?.seq != 1 {
        return Err(Error::new(
            "initial publication requires a bounded version-one envelope",
        ));
    }
    match source {
        Source::Url(_) => publish(source, bytes),
        Source::Path(path) => {
            let written = PublishLock::take(path).and_then(|_held| match read_path(path)? {
                None => crate::fsx::write_atomic(path, bytes, 0o644),
                Some(held) if held == bytes => Ok(()),
                Some(_) => Err(Error::new("the creation path changed before publication")),
            });
            written.map_err(|error| match read_path(path) {
                // Unread, the path may hold them: the failure stays unclassified.
                Ok(held) if held.as_deref() != Some(bytes) => {
                    Error::of_kind(crate::ErrorKind::Refused, error.0)
                }
                _ => error,
            })
        }
    }
}

/// [`publish`] over `over`, the version the source served that the caller judged it must not keep.
pub fn replace(source: &Source, envelope: &[u8], over: &[u8]) -> crate::Result<()> {
    match source {
        Source::Path(path) if envelope.len() <= MAX_ENVELOPE => replace_path(path, envelope, over),
        _ => publish(source, envelope),
    }
}

#[cfg(unix)]
fn replace_path(path: &std::path::Path, envelope: &[u8], over: &[u8]) -> crate::Result<()> {
    let _held = PublishLock::take(path)?;
    if read_path(path)?.as_deref() == Some(over) {
        return crate::fsx::write_atomic(path, envelope, 0o644);
    }
    write_path_held(path, envelope)
}

#[cfg(not(unix))]
fn replace_path(path: &std::path::Path, envelope: &[u8], _over: &[u8]) -> crate::Result<()> {
    write_path(path, envelope)
}

/// A path keeps the server's rule (its 403 and 409, [`write_path_held`]), checked and renamed
/// under [`PublishLock`].
#[cfg(unix)]
fn write_path(path: &std::path::Path, envelope: &[u8]) -> crate::Result<()> {
    let _held = PublishLock::take(path)?;
    write_path_held(path, envelope)
}

/// `<path>.lock`, held by one publisher from its check of the version a path holds to its
/// rename over it, so two admins publishing at once cannot both pass the check and one drop the
/// other's version (a revocation, say). It is an exclusive create, which a shared mount (SMB,
/// NFSv3 and later) also serializes; it holds `<host> <pid>` and goes when the publish ends. A
/// server takes it too, over the vault file in its store, and only a server clears one left by
/// a holder on its machine that no longer runs, or written before its machine's boot
/// ([`PublishLock::take_clearing_dead`]); machines that share the mount and a host name are not
/// told apart. Any other lock a killed publisher left refuses every publish until a person
/// removes it.
#[cfg(unix)]
pub(crate) struct PublishLock(std::path::PathBuf);

/// Why [`PublishLock::take_clearing_dead`] did not take the lock.
#[cfg(unix)]
pub(crate) enum NotTaken {
    /// A lock the server did not clear: a publish that runs, or a holder it cannot tell is
    /// gone. `holder` is the line it holds, for a person (empty when unread).
    Held { lock: PathBuf, holder: String },
    /// The lock could not be made.
    Failed(Error),
}

#[cfg(unix)]
impl PublishLock {
    pub(crate) fn take(path: &std::path::Path) -> crate::Result<Self> {
        Self::taken(path, false).map_err(|refused| match refused {
            NotTaken::Held { lock, holder } => Error::of_kind(
                crate::ErrorKind::NotStored,
                format!(
                    "{}: another publish to this vault holds it{}; if none is running, remove it \
                     and publish again",
                    lock.display(),
                    held_by(&holder)
                ),
            ),
            NotTaken::Failed(e) => e,
        })
    }

    /// [`take`](Self::take) for a server, which first clears a lock left by a holder on this
    /// machine that no longer runs. A server alone does, under its state lock, so no two
    /// clearers race to remove another's new lock, as long as one server serves the store.
    pub(crate) fn take_clearing_dead(path: &std::path::Path) -> Result<Self, NotTaken> {
        Self::taken(path, true)
    }

    fn taken(path: &std::path::Path, clear_dead: bool) -> Result<Self, NotTaken> {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut name = path.as_os_str().to_owned();
        name.push(".lock");
        let lock = PathBuf::from(name);
        let mut cleared = false;
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o644)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(&lock)
            {
                Ok(mut file) => {
                    let holder = aterm_uds::process::host_name().unwrap_or_default();
                    // Who holds it is for a person reading the file; the lock is the file.
                    let _ = writeln!(file, "{holder} {}", std::process::id());
                    return Ok(Self(lock));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if clear_dead && !cleared && remove_dead(&lock) {
                        cleared = true;
                        continue;
                    }
                    let holder = read_path(&lock)
                        .ok()
                        .flatten()
                        .map_or_else(String::new, |bytes| reason(&bytes));
                    return Err(NotTaken::Held { lock, holder });
                }
                // The lock sits beside the vault, so a missing folder fails here first: name the
                // folder the person typed, not a lock file they never asked for.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    let folder = path
                        .parent()
                        .filter(|folder| !folder.as_os_str().is_empty())
                        .unwrap_or_else(|| std::path::Path::new("."));
                    return Err(NotTaken::Failed(Error::new(format!(
                        "{}: no such folder to hold {}; create it, or check the path",
                        folder.display(),
                        path.display()
                    ))));
                }
                Err(e) => {
                    return Err(NotTaken::Failed(Error::new(format!(
                        "{}: {e}",
                        lock.display()
                    ))));
                }
            }
        }
    }
}

/// ` (<holder>)`, or nothing for a lock whose holder line was not read.
#[cfg(unix)]
pub(crate) fn held_by(holder: &str) -> String {
    if holder.is_empty() {
        String::new()
    } else {
        format!(" ({holder})")
    }
}

/// Remove `lock` when it is a plain file whose holder is gone, and the path still holds the file
/// judged (kept open until the remove, so its inode cannot be reused meanwhile). A holder is
/// gone when its line names this machine's host name and a pid the kernel says no process has,
/// or the file was written before this boot ([`before_this_boot`]), which no running process
/// can have done: a power loss mid-publish leaves such a lock, naming a pid this boot may have
/// handed to another process, or with a holder line that never reached the disk.
#[cfg(unix)]
fn remove_dead(lock: &std::path::Path) -> bool {
    use std::io::Read as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    // Never waits: a FIFO there would hold the server's state lock until a writer came.
    let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(lock)
    else {
        return false;
    };
    let Ok(judged) = file.metadata() else {
        return false;
    };
    let mut line = String::with_capacity(
        usize::try_from(judged.len().min(256))
            .unwrap_or(0)
            .saturating_add(1),
    );
    if !judged.is_file() || (&file).take(256).read_to_string(&mut line).is_err() {
        return false;
    }
    let before_boot = judged.modified().is_ok_and(before_this_boot);
    let dead = match line.trim_end().rsplit_once(' ') {
        Some((host, pid)) => {
            let ours = !host.is_empty()
                && [
                    aterm_uds::process::host_name(),
                    aterm_uds::process::kernel_host_name(),
                ]
                .iter()
                .any(|name| name.as_deref() == Some(host));
            let gone = !pid.is_empty()
                && pid.bytes().all(|b| b.is_ascii_digit())
                && pid.parse::<i32>().is_ok_and(|pid| {
                    pid > 0
                        && aterm_uds::process::process_presence(pid)
                            == aterm_uds::process::ProcessPresence::Absent
                });
            ours && (gone || before_boot)
        }
        None => line.trim().is_empty() && before_boot,
    };
    // A person may have put another lock there since it was read.
    let same = std::fs::symlink_metadata(lock)
        .is_ok_and(|now| (now.dev(), now.ino()) == (judged.dev(), judged.ino()));
    let removed = dead && same && std::fs::remove_file(lock).is_ok();
    drop(file);
    removed
}

/// How far before this boot's start a file must have been written to be judged older than the
/// boot ([`before_this_boot`]): room for a clock set forward after the boot (a network time
/// fix), which makes the start computed now later than the clock read at the boot.
#[cfg(unix)]
const BOOT_SLACK: Duration = Duration::from_secs(10 * 60);

/// Whether `written` (a file's modification time) is before this boot began, by more than
/// [`BOOT_SLACK`]. `false` where the time since boot cannot be read.
#[cfg(unix)]
fn before_this_boot(written: std::time::SystemTime) -> bool {
    let booted = aterm_uds::process::continuous_ms()
        .and_then(|up| std::time::SystemTime::now().checked_sub(Duration::from_millis(up)));
    booted.is_some_and(|booted| {
        written
            .checked_add(BOOT_SLACK)
            .is_some_and(|by| by < booted)
    })
}

#[cfg(unix)]
impl Drop for PublishLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(all(unix, any(test, debug_assertions)))]
thread_local! {
    /// The negative control of a path's compare-and-set (one-mesh P3, [`write_path_held`]): while
    /// set, the path judges a version of the vault it holds by its sequence and parents alone, as
    /// before it asked P3's kernels, so a version whose signer the held key never handed off to,
    /// or one that drops a link of the held handoff chain, takes the path over where a server
    /// refuses it. A test sets it for its thread; a lab instance turns it on for its whole process
    /// with `path-sequence-only` in `ATERM_TEST_NEGATIVE_CONTROL`
    /// ([`aterm_types::negative_control::on`]).
    pub(crate) static PATH_SEQUENCE_ONLY: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// Whether [`PATH_SEQUENCE_ONLY`] is on: set on this thread, or named for this process
/// (`path-sequence-only`); never in a release build, which has neither switch.
#[cfg(unix)]
fn path_sequence_only() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        PATH_SEQUENCE_ONLY.with(std::cell::Cell::get)
            || aterm_types::negative_control::on("path-sequence-only")
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        false
    }
}

/// [`write_path`] with its [`PublishLock`] held. A version of the vault the path holds is judged
/// as a server judges a PUT (server.rs `takes_over`), by P3's kernels
/// ([`Takeover::of`](aterm_mesh_core::no_lost_edit::Takeover::of)): the pin the held version's
/// key and chain give, the chain it must carry on, its sequence and its parents. A refusal of the
/// pin or the chain is [`ErrorKind::Refused`](crate::ErrorKind::Refused), as a server's 403 is:
/// nothing was stored, and the version can never take the path over. One by sequence or parents is
/// [`ErrorKind::NotStored`](crate::ErrorKind::NotStored), as a server's 409 is: another version
/// holds the path. A path that holds no envelope, or another vault's, is written.
#[cfg(unix)]
fn write_path_held(path: &std::path::Path, envelope: &[u8]) -> crate::Result<()> {
    use aterm_mesh_core::no_lost_edit::{Takeover, TakeoverFacts};
    if let (Ok(new), Some(held_bytes)) = (crate::envelope::parse(envelope), read_path(path)?)
        && let Ok(held) = crate::envelope::parse(&held_bytes)
        && held.name == new.name
    {
        let followed: Vec<[u8; 32]> = held.handoff.iter().map(|(key, _)| *key).collect();
        let pin = new.check_pin_with(
            &held.signer,
            &followed,
            &crate::envelope::Retired::compiled(),
        );
        let extends = new.check_extends(&held.signer, &held.handoff);
        let keys_unread = path_sequence_only();
        let facts = TakeoverFacts {
            pin_ok: pin.is_ok() || keys_unread,
            replaced: matches!(pin, Ok(crate::envelope::Pin::Replaced(_))),
            not_newer: new.seq <= held.seq,
            same_version: new.seq == held.seq && held_bytes == envelope,
            chain_ok: extends.is_ok() || keys_unread,
            names_parents: !new.parents.is_empty(),
            // A path's parent-CAS starts at the first version with parents: every later one
            // it took names the version before it.
            held_parented: !held.parents.is_empty(),
            among_parents: new.parents.contains(&held.digest()),
        };
        let refused = |error: Option<Error>| {
            Error::of_kind(
                crate::ErrorKind::Refused,
                format!(
                    "{}: {}",
                    path.display(),
                    error.map_or_else(String::new, |e| e.0)
                ),
            )
        };
        match Takeover::of(facts) {
            Takeover::Takes => {}
            Takeover::Holds => return Ok(()),
            Takeover::PinRefused => return Err(refused(pin.err())),
            Takeover::BreaksChain => return Err(refused(extends.err())),
            Takeover::NotNewer => {
                return Err(Error::of_kind(
                    crate::ErrorKind::NotStored,
                    format!(
                        "seq {} is not newer than seq {}, which {} holds",
                        new.seq,
                        held.seq,
                        path.display()
                    ),
                ));
            }
            Takeover::OffParents => {
                return Err(Error::of_kind(
                    crate::ErrorKind::NotStored,
                    format!(
                        "head at seq {} in {} is not among the envelope's parents",
                        held.seq,
                        path.display()
                    ),
                ));
            }
        }
    }
    crate::fsx::write_atomic(path, envelope, 0o644)
}

#[cfg(not(unix))]
fn write_path(path: &std::path::Path, _envelope: &[u8]) -> crate::Result<()> {
    Err(Error::new(format!(
        "{}: a vault at a path is written on Unix only",
        path.display()
    )))
}

/// `POST <url>/join` with one request line (`age1… <host> <code>`). A `429` with a
/// `Retry-After` of at most [`JOIN_RETRY_MOST`] (the server's gap after a key changed its
/// request, as a join password typed again does) is sat out, and the line sent once more.
pub fn post_join(url: &str, line: &str) -> crate::Result<()> {
    post_join_pausing(url, line, &std::thread::sleep)
}

/// [`post_join`], sitting out a `Retry-After` with `pause`.
pub(crate) fn post_join_pausing(
    url: &str,
    line: &str,
    pause: &dyn Fn(Duration),
) -> crate::Result<()> {
    post_join_bounded(url, line, pause, None)
}

/// Post a join request and honor at most one short Retry-After, sharing `deadline` across both.
#[cfg(any(unix, test))]
pub(crate) fn post_join_until(
    url: &str,
    line: &str,
    deadline: std::time::Instant,
) -> crate::Result<()> {
    post_join_bounded(url, line, &std::thread::sleep, Some(deadline))
}

fn post_join_bounded(
    url: &str,
    line: &str,
    pause: &dyn Fn(Duration),
    deadline: Option<std::time::Instant>,
) -> crate::Result<()> {
    let remaining = || match deadline {
        Some(deadline) => deadline
            .checked_duration_since(std::time::Instant::now())
            .filter(|left| !left.is_zero())
            .ok_or_else(|| Error::of_kind(ErrorKind::TimedOut, "join request deadline elapsed")),
        None => Ok(EXCHANGE),
    };
    let line = line.trim_end_matches('\n');
    parse_join_line(line)?;
    let target = join_url(url);
    let mut retried = false;
    loop {
        let response = client(remaining()?.min(EXCHANGE))
            .post(&target)
            .header("Content-Type", "text/plain; charset=utf-8")
            .limit(REASON_BODY)
            .send(format!("{line}\n").as_bytes())
            .map_err(|e| transport_error("asking to join at", &target, &e))?;
        if response.is_success() {
            return Ok(());
        }
        match retry_after(&response) {
            Some(wait) if response.status() == 429 && !retried && wait <= JOIN_RETRY_MOST => {
                if deadline.is_some() && wait >= remaining()? {
                    return Err(Error::of_kind(
                        ErrorKind::TimedOut,
                        "join retry would outlast the request deadline",
                    ));
                }
                retried = true;
                pause(wait);
            }
            _ => {
                return Err(refused(
                    "asking to join at",
                    &target,
                    response.status(),
                    response.body(),
                ));
            }
        }
    }
}

/// A response's `Retry-After` in its delay-seconds form (an HTTP date is not read).
fn retry_after(response: &aterm_http::Response) -> Option<Duration> {
    let value = response.header("Retry-After")?.trim();
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value.parse().ok().map(Duration::from_secs)
}

/// `GET <url>/join`: the pending request lines ([`parse_join_line`]).
pub fn list_joins(url: &str) -> crate::Result<Vec<String>> {
    list_joins_from(url).map(|(lines, _)| lines)
}

/// [`list_joins`], and the address of the server that answered (`None`: not known).
pub fn list_joins_from(url: &str) -> crate::Result<(Vec<String>, Option<IpAddr>)> {
    match wait_joins_from(url, None, EXCHANGE)? {
        (Joins::Listed(lines, _), from) => Ok((lines, from)),
        (Joins::Unchanged, from) => Ok((Vec::new(), from)),
    }
}

/// What one read of a vault's join queue heard ([`wait_joins`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Joins {
    /// The pending request lines, and the queue's version where the server names one (its
    /// `ETag`): a server that names none holds no join long-poll.
    Listed(Vec<String>, Option<u64>),
    /// The queue is still at the version asked past: the server's hold ran out, or it held the
    /// request past `wait`.
    Unchanged,
}

/// `GET <url>/join`, or with `after`, `GET <url>/join?after=<n>`: a long-poll the server holds
/// until a request is queued or changed past version `n` or the vault takes a new version, or
/// its hold runs out, the whole request within `wait` and [`FETCH_MARGIN`].
pub fn wait_joins(url: &str, after: Option<u64>, wait: Duration) -> crate::Result<Joins> {
    wait_joins_from(url, after, wait).map(|(joins, _)| joins)
}

/// [`wait_joins`], and the address of the server that answered (`None`: not known): an approver
/// continues a queue's long-poll only from the server that answered it, and names that server in
/// `aterm secrets requests --json`.
pub fn wait_joins_from(
    url: &str,
    after: Option<u64>,
    wait: Duration,
) -> crate::Result<(Joins, Option<IpAddr>)> {
    wait_joins_from_within(url, after, wait.saturating_add(fetch_margin()))
}

/// Read a join queue within the remaining shared deadline, including connection and response.
/// A mirror's vault verification and its queue read share this same deadline.
#[cfg(any(unix, test))]
pub(crate) fn wait_joins_from_until(
    url: &str,
    after: Option<u64>,
    until: std::time::Instant,
) -> crate::Result<(Joins, Option<IpAddr>)> {
    let remaining = until
        .checked_duration_since(std::time::Instant::now())
        .filter(|left| !left.is_zero())
        .ok_or_else(|| Error::of_kind(ErrorKind::TimedOut, "join queue deadline elapsed"))?;
    wait_joins_from_within(url, after, remaining)
}

fn wait_joins_from_within(
    url: &str,
    after: Option<u64>,
    within: Duration,
) -> crate::Result<(Joins, Option<IpAddr>)> {
    let target = match after {
        Some(after) => with_query(&join_url(url), &format!("after={after}")),
        None => join_url(url),
    };
    let noting = Arc::new(PeerNoting::default());
    let response = match Client::new(Trust::PlatformVerifier, ProxyMode::Direct, within)
        .with_connector(Arc::clone(&noting) as Arc<dyn aterm_http::Connect>)
        .get(&target)
        .limit(JOINS_BODY)
        .send_phased(&[])
    {
        Ok(response) => response,
        Err(failed) if after.is_some() && held_past_deadline(&failed) => {
            return Ok((Joins::Unchanged, noting.reached()));
        }
        Err(failed) => {
            return Err(transport_error(
                "listing join requests at",
                &target,
                &failed.error,
            ));
        }
    };
    match response.status() {
        200 => {}
        204 if after.is_some() => return Ok((Joins::Unchanged, noting.reached())),
        status => {
            return Err(refused(
                "listing join requests at",
                &target,
                status,
                response.body(),
            ));
        }
    }
    let text = std::str::from_utf8(response.body())
        .map_err(|_| Error::new("the join queue is not UTF-8 text"))?;
    Ok((
        Joins::Listed(
            text.lines()
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect(),
            response.header("etag").and_then(etag_seq),
        ),
        noting.reached(),
    ))
}

/// `POST <url>/join/verdict` with one line, `age1… refused <tag>`: an admin Mac's word that the
/// request of that key whose proof `tag` names ([`crate::join::proof_tag`]) did not prove the
/// join password. The server keeps it beside the request, unauthenticated: a forged one can
/// only make the joining Mac ask its person for the password again.
pub fn post_verdict(url: &str, recipient: &str, tag: &str) -> crate::Result<()> {
    let line = format!("{recipient} refused {tag}");
    if parse_verdict_line(&line).is_none() {
        return Err(Error::new("a verdict is `age1… refused <16 hex digits>`"));
    }
    let target = verdict_url(url);
    let response = client(verdict_exchange())
        .post(&target)
        .header("Content-Type", "text/plain; charset=utf-8")
        .limit(REASON_BODY)
        .send(format!("{line}\n").as_bytes())
        .map_err(|e| transport_error("posting a join verdict at", &target, &e))?;
    if response.is_success() {
        return Ok(());
    }
    Err(refused(
        "posting a join verdict at",
        &target,
        response.status(),
        response.body(),
    ))
}

/// `GET <url>/join/verdict`: each `(age1…, tag)` an admin Mac refused ([`post_verdict`]). A
/// server that keeps no verdicts (an older one answers `404`) has none.
pub fn list_verdicts(url: &str) -> crate::Result<Vec<(String, String)>> {
    list_verdicts_within(url, verdict_exchange())
}

/// [`list_verdicts`], the whole exchange within `total` (at most [`VERDICT_EXCHANGE`], or the
/// hang detector a test holds on this thread).
pub fn list_verdicts_within(url: &str, total: Duration) -> crate::Result<Vec<(String, String)>> {
    let target = verdict_url(url);
    let response = client(total.min(verdict_exchange()))
        .get(&target)
        .limit(JOINS_BODY)
        .send(&[])
        .map_err(|e| transport_error("reading join verdicts at", &target, &e))?;
    match response.status() {
        200 => {}
        404 => return Ok(Vec::new()),
        status => {
            return Err(refused(
                "reading join verdicts at",
                &target,
                status,
                response.body(),
            ));
        }
    }
    let text = std::str::from_utf8(response.body())
        .map_err(|_| Error::new("the join verdicts are not UTF-8 text"))?;
    Ok(text.lines().filter_map(parse_verdict_line).collect())
}

/// A verdict line, `age1… refused <tag>` (`tag` 16 lowercase hex digits).
pub fn parse_verdict_line(line: &str) -> Option<(String, String)> {
    let mut words = line.split(' ');
    let (recipient, word, tag) = (words.next()?, words.next()?, words.next()?);
    let tag_ok = tag.len() == 16 && tag.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    (words.next().is_none() && recipient.starts_with("age1") && word == "refused" && tag_ok)
        .then(|| (recipient.to_owned(), tag.to_owned()))
}

/// One join request.
#[derive(Clone, PartialEq, Eq)]
pub struct JoinRequest {
    /// The machine's `age1…` recipient, as sent.
    pub recipient: String,
    /// The machine name it asks for.
    pub host: String,
    /// Its join code, `xxxx-xxxx-xxxx-xxxx`: 16 lowercase base32 characters, 80 bits.
    pub code: String,
    /// Unix seconds when the server queued it (listings only).
    pub time: Option<u64>,
    /// A member's signed Allow of it (`crate::members::Allowed`), padded base64.
    pub allowed: Option<String>,
    /// The join-password proof it carried (`crate::join`), padded base64.
    pub proof: Option<String>,
}

impl std::fmt::Debug for JoinRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JoinRequest")
            .field("recipient", &self.recipient)
            .field("host", &self.host)
            .field("code", &self.code)
            .field("time", &self.time)
            .field("member_allow_present", &self.allowed.is_some())
            .field("password_proof_present", &self.proof.is_some())
            .finish()
    }
}

impl JoinRequest {
    /// The request line a machine POSTs.
    pub fn line(&self) -> String {
        let mut line = format!("{} {} {}", self.recipient, self.host, self.code);
        if let Some(allowed) = &self.allowed {
            line.push_str(" allowed=");
            line.push_str(allowed);
        }
        if let Some(proof) = &self.proof {
            line.push_str(" proof=");
            line.push_str(proof);
        }
        line
    }

    /// The line a server lists: `age1… <host> <code> <time>`, then ` allowed=<b64>` and
    /// ` proof=<b64>`.
    pub fn listed(&self) -> String {
        let mut line = format!(
            "{} {} {} {}",
            self.recipient,
            self.host,
            self.code,
            self.time.unwrap_or(0)
        );
        if let Some(allowed) = &self.allowed {
            line.push_str(" allowed=");
            line.push_str(allowed);
        }
        if let Some(proof) = &self.proof {
            line.push_str(" proof=");
            line.push_str(proof);
        }
        line
    }
}

/// `age1` + 52 data characters (32 bytes) + 6 checksum characters.
const RECIPIENT_LEN: usize = 62;

/// Read one join request line.
pub fn parse_join_line(line: &str) -> crate::Result<JoinRequest> {
    let (line, proof) = match line.rsplit_once(" proof=") {
        Some((rest, proof)) => {
            if !proof_ok(proof) {
                return Err(Error::new(
                    "a join request's proof is not a 32-byte padded base64 tag",
                ));
            }
            (rest, Some(proof.to_owned()))
        }
        None => (line, None),
    };
    // An older parser reads ` allowed=` as a fifth field and skips the line: an older
    // approver never admits by it, and an older server refuses it.
    let (line, allowed) = match line.rsplit_once(" allowed=") {
        Some((rest, allowed)) => {
            if !allowed_ok(allowed) {
                return Err(Error::new(
                    "a join request's allowed is not a member's padded base64 Allow",
                ));
            }
            (rest, Some(allowed.to_owned()))
        }
        None => (line, None),
    };
    // An aterm 0.148 or 0.149 server lists ` home=<16 lowercase hex>` on a request it observed
    // on its home ALab LAN, and one before 0.148 ` lan=1` on one from a private address. Nothing
    // admits by either any longer (a Mac on the home ALab LAN is let in by a member when it
    // proves itself): each is read past, so a newer admin still sees and can approve the request.
    let (line, home) = match line.rsplit_once(" home=") {
        Some((rest, tag))
            if tag.len() == 16 && tag.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) =>
        {
            (rest, true)
        }
        Some(_) => return Err(Error::new("a join request's home LAN marker is invalid")),
        None => (line, false),
    };
    let (line, lan) = match line.rsplit_once(" lan=") {
        Some((rest, "1")) => (rest, true),
        Some(_) => return Err(Error::new("a join request's LAN marker is invalid")),
        None => (line, false),
    };
    let fields: Vec<&str> = line.split(' ').collect();
    let (recipient, host, code, time) = match fields.as_slice() {
        [recipient, host, code] => (*recipient, *host, *code, None),
        [recipient, host, code, time] => (*recipient, *host, *code, Some(*time)),
        _ => {
            return Err(Error::new(
                "a join request is one line: `age1… <host> <code>`",
            ));
        }
    };
    if !recipient_shape_ok(recipient) {
        return Err(Error::new("a join request's key is not an age1… recipient"));
    }
    if !machine_name_ok(host) {
        return Err(Error::new(
            "a join request's host is not a machine name ([a-z0-9][a-z0-9-]{0,62}, not machines, admins, everyone or join)",
        ));
    }
    if !valid_code(code) {
        return Err(Error::new(
            "a join request's code is not xxxx-xxxx-xxxx-xxxx in lowercase base32",
        ));
    }
    let time = match time {
        None => None,
        Some(text) => Some(
            text.parse::<u64>()
                .ok()
                .filter(|_| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
                .ok_or_else(|| Error::new("a join request's time is not unix seconds"))?,
        ),
    };
    if (lan || home) && time.is_none() {
        return Err(Error::new("only a server listing carries a LAN marker"));
    }
    Ok(JoinRequest {
        recipient: recipient.to_owned(),
        host: host.to_owned(),
        code: code.to_owned(),
        time,
        allowed,
        proof,
    })
}

/// The longest padded base64 an Allow takes (`crate::members::allowed::MAX_TEXT`: 4 + 144 +
/// 32 + 1 + 63 + 64 bytes); the signature is the approver's to check.
pub(crate) const MAX_ALLOWED_TEXT: usize = 412;

fn allowed_ok(allowed: &str) -> bool {
    allowed.len() <= MAX_ALLOWED_TEXT && crate::b64::decode_padded(allowed).is_some()
}

fn proof_ok(proof: &str) -> bool {
    proof.len() == 44 && crate::b64::decode_padded(proof).is_some_and(|tag| tag.len() == 32)
}

fn recipient_shape_ok(text: &str) -> bool {
    text.len() == RECIPIENT_LEN && crate::age::Recipient::parse(text).is_ok()
}

fn machine_name_ok(name: &str) -> bool {
    crate::text::valid_machine_name(name)
}

/// A join code's shape: four groups of four lowercase base32 letters (`[a-z2-7]`) joined by `-`.
pub fn valid_code(code: &str) -> bool {
    let bytes = code.as_bytes();
    bytes.len() == 19
        && bytes.iter().enumerate().all(|(index, &b)| {
            if index % 5 == 4 {
                b == b'-'
            } else {
                b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b)
            }
        })
}

/// A valid recipient for tests, distinct for distinct `n`.
#[cfg(test)]
pub(crate) fn test_recipient(n: u8) -> String {
    crate::age::Identity::from_secret_bytes([n.wrapping_add(1); 32])
        .recipient()
        .to_bech32()
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;
