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

/// Where a vault is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// `http://` or `https://` URL, without its fragment.
    Url(String),
    /// A local path (absolute, or `~/…` expanded by the caller).
    Path(PathBuf),
}

/// What a fetch got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetched {
    /// The bytes at the source.
    Body(Vec<u8>),
    /// A long-poll that ended with nothing newer (`204`).
    NoNewer(Option<u64>),
    /// Nothing there: a `404`, or no file at the path.
    Missing,
}

/// What [`fetch`] adds to `wait` for the request's own deadline.
pub const FETCH_MARGIN: Duration = Duration::from_secs(5);
/// The DNS lookup, connect and TLS handshake of each of a look's tries ([`get_bounded`]):
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
/// The deadline of a publish (up to 16 MiB on a LAN) and of the join exchanges.
const EXCHANGE: Duration = Duration::from_secs(120);
/// The deadline of a join verdict's one-line exchange ([`post_verdict`], [`list_verdicts`]):
/// a waiting subscriber reads it before each long-poll, and the approver posts it within its
/// pass, so neither waits out a [`EXCHANGE`] on a vault that does not answer.
const VERDICT_EXCHANGE: Duration = Duration::from_secs(10);
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

/// A server's reason: its first line, sanitized, without the words aterm reads as macOS's Local
/// Network refusal ([`crate::client::refused_by_local_network`]), which only this Mac's own
/// lookup says, so a server quoting them reads as no refusal.
pub fn reason(body: &[u8]) -> String {
    one_line(body, REASON_CHARS).replace(aterm_http::LOCAL_NETWORK_REFUSED, QUOTED_REFUSAL)
}

/// What stands in a server's reason for the words of macOS's Local Network refusal ([`reason`]):
/// shorter than they are, so the reason stays within its cap.
const QUOTED_REFUSAL: &str = "[a refusal the server quoted]";

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
        .with_connector(Arc::new(LanNoting))
}

/// aterm-http's own connector, which also keeps where a connect found no route to a LAN
/// address ([`local_network_gate`]) in the error it returns ([`NoRoute`]), for
/// [`transport_error`] to tell.
#[derive(Debug)]
struct LanNoting;

impl aterm_http::Connect for LanNoting {
    fn connect(
        &self,
        host: &str,
        port: u16,
        deadline: aterm_http::Deadline,
    ) -> std::io::Result<std::net::TcpStream> {
        let mut last = None;
        aterm_http::TcpConnector
            .connect_noting(host, port, deadline, &mut |addr, error| {
                last = local_network_gate(addr.ip(), error).then_some(addr);
            })
            .map_err(|error| match last {
                // The error is that connect's own, not a deadline spent after it.
                Some(addr) if local_network_gate(addr.ip(), &error) => {
                    std::io::Error::new(error.kind(), NoRoute { addr, error })
                }
                _ => error,
            })
    }
}

/// A connect that found no route to a LAN address: the error, and where it went.
#[derive(Debug)]
struct NoRoute {
    addr: SocketAddr,
    error: std::io::Error,
}

impl std::fmt::Display for NoRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (to {})", self.error, self.addr.ip())
    }
}

impl std::error::Error for NoRoute {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Whether a connect to `ip` that failed with `error` is what macOS's Local Network gate
/// answers aterm: no route (`EHOSTUNREACH`, `ENETUNREACH`) to a private or link-local
/// address (IPv4 RFC 1918 or 169.254/16, IPv6 unique local or link-local, either one mapped).
/// A LAN host that is down answers the same.
fn local_network_gate(ip: IpAddr, error: &std::io::Error) -> bool {
    use std::io::ErrorKind::{HostUnreachable, NetworkUnreachable};
    let lan = match ip.to_canonical() {
        IpAddr::V4(ip) => ip.is_private() || ip.is_link_local(),
        IpAddr::V6(ip) => ip.is_unique_local() || ip.is_unicast_link_local(),
    };
    lan && matches!(error.kind(), HostUnreachable | NetworkUnreachable)
}

/// A failure to talk to the server. No route to a LAN address is
/// [`ErrorKind::LocalNetworkPending`], and says where the gate is; a request past its deadline
/// is [`ErrorKind::TimedOut`].
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
    if no_route_to_lan(error) {
        return Error::of_kind(
            ErrorKind::LocalNetworkPending,
            format!(
                "{text}; macOS lets aterm reach the local network only once allowed (Privacy & Security > Local Network)"
            ),
        );
    }
    if matches!(error, aterm_http::Error::Io(io) if io.kind() == std::io::ErrorKind::TimedOut) {
        return Error::of_kind(ErrorKind::TimedOut, text);
    }
    Error::new(text)
}

/// Whether `error` is a connect that found no route to a LAN address ([`NoRoute`]).
fn no_route_to_lan(error: &aterm_http::Error) -> bool {
    matches!(
        error,
        aterm_http::Error::Io(io) if io.get_ref().is_some_and(|inner| inner.is::<NoRoute>())
    )
}

/// Fetch bytes at `source` ([`FETCH_MARGIN`], [`Fetched::NoNewer`], [`Fetched::Missing`]).
pub fn fetch(source: &Source, after: Option<u64>, wait: Duration) -> crate::Result<Fetched> {
    match source {
        Source::Url(url) => match get(url, after, wait)? {
            Answer::Fetched(fetched) => Ok(fetched),
            Answer::Foreign(why) => Err(Error::new(why)),
        },
        Source::Path(path) => Ok(read_path(path)?.map_or(Fetched::Missing, Fetched::Body)),
    }
}

/// [`fetch`] of the vault at `url` as one try of a bounded wait: the whole request, lookup to
/// last byte, within `total`, with no [`FETCH_MARGIN`], so a long-poll the server holds for its
/// own time ends at `total` too, failing [`ErrorKind::TimedOut`].
pub fn fetch_within(url: &str, after: Option<u64>, total: Duration) -> crate::Result<Fetched> {
    match get_via(&client(total), url, after, None).map_err(|unheard| unheard.why)? {
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
    /// A timeout (a lookup with no answer in time, or a connect or request past its deadline)
    /// or no route to the host: how macOS holds the lookups and LAN connects of an app whose
    /// Local Network question nobody has answered yet, so a try after the answer can succeed,
    /// and how it refuses them after a Don't Allow (a slow network, a host that sleeps and a
    /// server that stalls look the same).
    pub held: bool,
}

/// [`get`], with its DNS lookup, connect and TLS handshake bounded by `connect` within the
/// request's deadline, saying of a failure whether it is [`Unheard::held`].
pub fn get_bounded(
    url: &str,
    after: Option<u64>,
    wait: Duration,
    connect: Option<Duration>,
) -> Result<Answer, Unheard> {
    get_via(
        &client(wait.saturating_add(FETCH_MARGIN)),
        url,
        after,
        connect,
    )
}

/// One GET of the vault at `url` as a look, never a long-poll: all of it within `total`, with
/// no [`FETCH_MARGIN`], its lookup and connect within [`PROBE_CONNECT`] (or `total`, if
/// shorter).
pub fn get_within(url: &str, total: Duration) -> crate::Result<Answer> {
    get_via(&client(total), url, None, Some(PROBE_CONNECT.min(total)))
        .map_err(|unheard| unheard.why)
}

/// [`get_bounded`] through `client` (its timeout is the request's deadline).
fn get_via(
    client: &Client,
    url: &str,
    after: Option<u64>,
    connect: Option<Duration>,
) -> Result<Answer, Unheard> {
    let target = match after {
        Some(seq) => with_query(url, &format!("after={seq}")),
        None => base(url).to_owned(),
    };
    let request = client.get(&target).limit(MAX_ENVELOPE);
    let request = match connect {
        Some(connect) => request.connect_within(connect),
        None => request,
    };
    let response = match request.send_phased(&[]) {
        Ok(response) => response,
        // A long-poll the server still held at the request's deadline (one held longer than
        // the asker waits, by an older server): nothing newer was heard.
        Err(failed) if after.is_some() && held_past_deadline(&failed) => {
            return Ok(Answer::Fetched(Fetched::NoNewer(None)));
        }
        Err(aterm_http::SendError { error, .. }) if foreign_reply(&error) => {
            return Ok(Answer::Foreign(transport_error("fetching", url, &error).0));
        }
        Err(aterm_http::SendError { error, .. }) => {
            return Err(Unheard {
                why: transport_error("fetching", url, &error),
                held: held(&error),
            });
        }
    };
    let failed = |status| Unheard {
        why: refused("fetching", url, status, response.body()),
        held: false,
    };
    match response.status() {
        200 => Ok(Answer::Fetched(Fetched::Body(response.body().to_vec()))),
        204 => Ok(Answer::Fetched(Fetched::NoNewer(
            response.header("etag").and_then(etag_seq),
        ))),
        404 => Ok(Answer::Fetched(Fetched::Missing)),
        status if foreign_status(status) => Ok(Answer::Foreign(failed(status).why.0)),
        status => Err(failed(status)),
    }
}

/// Whether `failed` is a request the server took and never answered by its deadline: the
/// whole request went out ([`aterm_http::Phase::Sent`]) and its time ran out.
fn held_past_deadline(failed: &aterm_http::SendError) -> bool {
    failed.phase == aterm_http::Phase::Sent
        && matches!(&failed.error, aterm_http::Error::Io(io) if io.kind() == std::io::ErrorKind::TimedOut)
}

/// Whether `error` is [`Unheard::held`]: aterm-http's resolver gives up on a lookup with no
/// answer as `TimedOut` (a definite "no such name" is another kind), a connect macOS refuses an
/// app it has not let onto the LAN fails `HostUnreachable`, and so does a lookup of a `.local`
/// name mDNSResponder refuses that app, which aterm-http names so (`getaddrinfo` reports the
/// refusal as "no such name"); no route to a LAN address ([`no_route_to_lan`]) is held too.
fn held(error: &aterm_http::Error) -> bool {
    no_route_to_lan(error)
        || matches!(
            error,
            aterm_http::Error::Io(error) if matches!(
                error.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::HostUnreachable
            )
        )
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

/// Publish an envelope (`PUT <url>`).
pub fn publish(source: &Source, envelope: &[u8]) -> crate::Result<()> {
    if envelope.len() > MAX_ENVELOPE {
        return Err(Error::new(format!(
            "the envelope is larger than {MAX_ENVELOPE} bytes"
        )));
    }
    match source {
        Source::Url(url) => {
            let response = client(EXCHANGE)
                .put(base(url))
                .header("Content-Type", "text/plain; charset=utf-8")
                .limit(REASON_BODY)
                .send(envelope)
                .map_err(|e| transport_error("publishing to", url, &e))?;
            if response.is_success() {
                return Ok(());
            }
            let error = refused("publishing to", url, response.status(), response.body());
            // The server answers 409 only before it stores anything ([`ErrorKind::NotStored`]).
            Err(if response.status() == 409 {
                Error::of_kind(crate::ErrorKind::NotStored, error.0)
            } else {
                error
            })
        }
        Source::Path(path) => write_path(path, envelope),
    }
}

/// Publish retained version-one bytes without replacing a path that appeared
/// after the caller's absence check. The HTTP server enforces its signer and
/// version rules; its response still cannot prove global uniqueness.
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
            let _held = PublishLock::take(path)?;
            match read_path(path)? {
                None => crate::fsx::write_atomic(path, bytes, 0o644),
                Some(held) if held == bytes => Ok(()),
                Some(_) => Err(Error::new("the creation path changed before publication")),
            }
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

/// A path keeps the server's rule (its 409), checked and renamed under [`PublishLock`].
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
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(lock)
    else {
        return false;
    };
    let Ok(judged) = file.metadata() else {
        return false;
    };
    let mut line = String::new();
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
            let gone = pid.parse::<i32>().is_ok_and(|pid| {
                aterm_uds::process::process_presence(pid)
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

/// [`write_path`] with its [`PublishLock`] held.
#[cfg(unix)]
fn write_path_held(path: &std::path::Path, envelope: &[u8]) -> crate::Result<()> {
    if let (Ok(new), Some(held)) = (crate::envelope::parse(envelope), read_path(path)?) {
        if held == envelope {
            return Ok(());
        }
        if let Ok(held) = crate::envelope::parse(&held)
            && held.name == new.name
            && held.seq >= new.seq
        {
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
    let line = line.trim_end_matches('\n');
    parse_join_line(line)?;
    let target = join_url(url);
    let mut retried = false;
    loop {
        let response = client(EXCHANGE)
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
    match wait_joins(url, None, EXCHANGE)? {
        Joins::Listed(lines, _) => Ok(lines),
        Joins::Unchanged => Ok(Vec::new()),
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
    let target = match after {
        Some(after) => with_query(&join_url(url), &format!("after={after}")),
        None => join_url(url),
    };
    let response = match client(wait.saturating_add(FETCH_MARGIN))
        .get(&target)
        .limit(JOINS_BODY)
        .send_phased(&[])
    {
        Ok(response) => response,
        Err(failed) if after.is_some() && held_past_deadline(&failed) => {
            return Ok(Joins::Unchanged);
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
        204 if after.is_some() => return Ok(Joins::Unchanged),
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
    Ok(Joins::Listed(
        text.lines()
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect(),
        response.header("etag").and_then(etag_seq),
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
    let response = client(VERDICT_EXCHANGE)
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
    list_verdicts_within(url, VERDICT_EXCHANGE)
}

/// [`list_verdicts`], the whole exchange within `total` (at most [`VERDICT_EXCHANGE`]).
pub fn list_verdicts_within(url: &str, total: Duration) -> crate::Result<Vec<(String, String)>> {
    let target = verdict_url(url);
    let response = client(total.min(VERDICT_EXCHANGE))
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinRequest {
    /// The machine's `age1…` recipient, as sent.
    pub recipient: String,
    /// The machine name it asks for.
    pub host: String,
    /// Its join code, `xxxx-xxxx-xxxx-xxxx`: 16 lowercase base32 characters, 80 bits.
    pub code: String,
    /// Unix seconds when the server queued it (listings only).
    pub time: Option<u64>,
    /// The join-password proof it carried (`crate::join`), padded base64.
    pub proof: Option<String>,
}

impl JoinRequest {
    /// The request line a machine POSTs.
    pub fn line(&self) -> String {
        let mut line = format!("{} {} {}", self.recipient, self.host, self.code);
        if let Some(proof) = &self.proof {
            line.push_str(" proof=");
            line.push_str(proof);
        }
        line
    }

    /// The line a server lists: `age1… <host> <code> <time>`, then ` proof=<b64>`.
    pub fn listed(&self) -> String {
        let mut line = format!(
            "{} {} {} {}",
            self.recipient,
            self.host,
            self.code,
            self.time.unwrap_or(0)
        );
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
    Ok(JoinRequest {
        recipient: recipient.to_owned(),
        host: host.to_owned(),
        code: code.to_owned(),
        time,
        proof,
    })
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
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::path::Path;

    const HANG_DETECTOR: Duration = Duration::from_secs(60);

    fn home() -> &'static Path {
        Path::new("/Users//someone")
    }

    fn key() -> [u8; 32] {
        let mut key = [0u8; 32];
        for (index, byte) in key.iter_mut().enumerate() {
            // Bytes that make `+`, `/` and padding appear in the base64.
            *byte = (index as u8).wrapping_mul(37).wrapping_add(251);
        }
        key
    }

    fn recipient() -> String {
        test_recipient(0)
    }

    #[test]
    fn a_url_parses_with_and_without_a_pin_in_every_base64_spelling() {
        let (source, pin) =
            parse_source("  http://alab-secrets.local:7484/v1/alab \n", home()).expect("url");
        assert_eq!(
            source,
            Source::Url("http://alab-secrets.local:7484/v1/alab".to_owned())
        );
        assert_eq!(pin, None);
        let standard = crate::b64::encode_padded(&key());
        assert!(
            standard.contains(['+', '/']) && standard.ends_with('='),
            "{standard}"
        );
        let urlsafe = standard.replace('+', "-").replace('/', "_");
        for spelling in [
            standard.clone(),
            standard.trim_end_matches('=').to_owned(),
            urlsafe.clone(),
            urlsafe.trim_end_matches('=').to_owned(),
        ] {
            let (source, pin) =
                parse_source(&format!("https://vault.example/v1/alab#{spelling}"), home())
                    .expect("pinned url");
            assert_eq!(
                source,
                Source::Url("https://vault.example/v1/alab".to_owned())
            );
            assert_eq!(pin, Some(key()), "{spelling}");
        }
        let extra = format!("{standard}A");
        for bad in ["", "AAAA", "not base64!", extra.as_str()] {
            assert!(
                parse_source(&format!("http://h/v1/alab#{bad}"), home()).is_err(),
                "{bad:?}"
            );
        }
        let mixed = format!("-{}", &standard[1..]);
        if mixed.contains(['+', '/']) {
            assert!(parse_source(&format!("http://h/v1/alab#{mixed}"), home()).is_err());
        }
    }

    #[test]
    fn paths_parse_and_anything_else_is_refused_with_a_hint() {
        assert_eq!(
            parse_source("~/vaults/alab.vault", home())
                .expect("home path")
                .0,
            Source::Path(PathBuf::from("/Users//someone/vaults/alab.vault"))
        );
        assert_eq!(
            parse_source("/Volumes/x/alab.vault", home()).expect("absolute"),
            (Source::Path(PathBuf::from("/Volumes/x/alab.vault")), None)
        );
        for (text, hint) in [
            ("", "no source"),
            ("~", "~/"),
            ("~/", "home directory"),
            ("~other/x", "~/"),
            ("alab.vault", "absolute"),
            ("alab-secrets.local:7484/v1/alab", "http://"),
            ("ftp://host/v1/alab", "only http"),
            ("file:///tmp/alab.vault", "only http"),
            ("http://", "not usable"),
            ("http://user:hunter2@host/v1/alab", "not usable"),
        ] {
            let error = parse_source(text, home()).expect_err(text);
            assert!(error.0.contains(hint), "{text:?}: {error}");
            assert!(!error.0.contains("hunter2"), "{error}");
        }
    }

    #[test]
    fn the_vault_name_is_the_last_segment_or_the_stem() {
        let url = |text: &str| Source::Url(text.to_owned());
        assert_eq!(
            vault_name(&url("http://h:7484/v1/alab")).as_deref(),
            Ok("alab")
        );
        assert_eq!(
            vault_name(&url("http://h/v1/my-vault?x=1")).as_deref(),
            Ok("my-vault")
        );
        assert!(vault_name(&url("http://h/v1/alab/")).is_err());
        assert!(vault_name(&url("http://h/v1/Alab")).is_err());
        assert!(vault_name(&url("http://h")).is_err());
        let path = |text: &str| Source::Path(PathBuf::from(text));
        assert_eq!(vault_name(&path("/x/alab.vault")).as_deref(), Ok("alab"));
        assert_eq!(vault_name(&path("/x/alab")).as_deref(), Ok("alab"));
        assert!(vault_name(&path("/x/My Secrets.txt")).is_err());
    }

    #[test]
    fn query_and_join_urls_compose() {
        assert_eq!(
            with_query("http://h/v1/a", "after=3"),
            "http://h/v1/a?after=3"
        );
        assert_eq!(
            with_query("http://h/v1/a?k=v#f", "after=3"),
            "http://h/v1/a?k=v&after=3"
        );
        assert_eq!(join_url("http://h/v1/a"), "http://h/v1/a/join");
        assert_eq!(join_url("http://h/v1/a?k=v"), "http://h/v1/a/join?k=v");
        assert_eq!(
            describe("http://h:7484/v1/a?token=secret#pin"),
            "http://h:7484/v1/a"
        );
    }

    #[test]
    fn a_reason_is_one_short_printable_line() {
        assert_eq!(reason(b"seq 3 is not newer\nmore"), "seq 3 is not newer");
        assert_eq!(reason(b"bad \x1b[31mred\x07\r\n"), "bad ?[31mred?");
        assert_eq!(reason("caf\u{e9}".as_bytes()), "caf??");
        assert_eq!(reason(&[b'x'; 500]).len(), 200);
        assert_eq!(reason(b""), "");
        // A server quoting macOS's Local Network refusal is no refusal of this Mac's.
        let quoted = format!("oops: {}", aterm_http::LOCAL_NETWORK_REFUSED);
        assert_eq!(
            reason(quoted.as_bytes()),
            "oops: [a refusal the server quoted]"
        );
        // The client, which reads the refusal, builds on unix only.
        #[cfg(unix)]
        assert!(!crate::client::refused_by_local_network(&refused(
            "fetching",
            "http://alab-secrets.local:7484/v1/alab",
            500,
            quoted.as_bytes()
        )));
    }

    /// This machine's own failure to reach a server keeps a remedy a server's reason would have
    /// no room for (a Linux `.local` lookup's), within its own cap.
    #[test]
    fn a_failure_to_reach_the_server_keeps_its_remedy() {
        let hint = format!(
            "dns: alab-secrets.local: Name or service not known ({})",
            "x".repeat(250)
        );
        let error = aterm_http::Error::Io(std::io::Error::other(hint.clone()));
        let said = transport_error("fetching", "http://alab-secrets.local:7484/v1/alab", &error).0;
        assert!(said.ends_with(&format!("{})", "x".repeat(250))), "{said}");
        assert_eq!(one_line(&[b'x'; 900], ERROR_CHARS).len(), ERROR_CHARS);
        // What the server sent keeps a server's cap.
        let sent = aterm_http::Error::Protocol("y".repeat(900));
        let said = transport_error("fetching", "http://alab-secrets.local:7484/v1/alab", &sent).0;
        assert!(said.len() < 300, "{said}");
    }

    #[cfg(unix)]
    #[test]
    fn a_path_is_fetched_and_published_atomically_and_world_readably() {
        use std::os::unix::fs::PermissionsExt;
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let path = root.path().join("alab.vault");
        let source = Source::Path(path.clone());
        assert_eq!(
            fetch(&source, None, HANG_DETECTOR).expect("missing"),
            Fetched::Missing
        );
        publish(&source, b"alab-vault/1\n").expect("published");
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o644);
        assert_eq!(
            fetch(&source, Some(9), HANG_DETECTOR).expect("fetched"),
            Fetched::Body(b"alab-vault/1\n".to_vec())
        );
        let link = root.path().join("link.vault");
        std::os::unix::fs::symlink(&path, &link).expect("link");
        assert!(fetch(&Source::Path(link), None, HANG_DETECTOR).is_err());
    }

    /// Version `seq` of vault `alab`, one admin locker holding `body`, signed.
    #[cfg(unix)]
    fn version(seq: u64, body: &[u8]) -> Vec<u8> {
        use crate::envelope::{Locker, Unsigned, sign, test_armor};
        let unsigned = Unsigned {
            name: "alab".to_owned(),
            seq,
            time: 1_792_530_000 + seq,
            handoff: Vec::new(),
            lockers: vec![Locker {
                label: "admin".to_owned(),
                armored: test_armor(body),
            }],
        };
        sign(&unsigned, &[7u8; 32]).expect("signed")
    }

    #[cfg(unix)]
    #[test]
    fn a_path_takes_only_a_newer_version_of_its_vault() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let path = root.path().join("alab.vault");
        let source = Source::Path(path.clone());
        let held = |bytes: &[u8]| {
            assert_eq!(
                fetch(&source, None, HANG_DETECTOR).expect("fetched"),
                Fetched::Body(bytes.to_vec())
            );
        };
        let first = version(41, b"one admin's text");
        publish(&source, &first).expect("published");
        let error = publish(&source, &version(41, b"another admin's text")).expect_err("not newer");
        assert!(error.0.contains("not newer than seq 41"), "{error}");
        assert!(publish(&source, &version(40, b"older")).is_err());
        held(&first);
        publish(&source, &first).expect("the same bytes again change nothing");
        let next = version(42, b"on top");
        publish(&source, &next).expect("newer");
        held(&next);
        std::fs::write(&path, b"not an envelope\n").expect("write");
        publish(&source, &first).expect("replaced");
        held(&first);
    }

    /// A vault path whose folder is missing (mistyped, or a volume not mounted) is refused naming
    /// that folder, and nothing is written. Negative control: with the folder there, it publishes.
    #[cfg(unix)]
    #[test]
    fn a_publish_into_a_missing_folder_names_the_folder() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let folder = root.path().join("shared");
        let path = folder.join("team.vault");
        let source = Source::Path(path.clone());
        let error = publish(&source, &version(1, b"first")).expect_err("no folder");
        assert!(
            error
                .0
                .starts_with(&format!("{}: no such folder", folder.display()))
                && !error.0.contains(".lock"),
            "{error}"
        );
        assert!(!folder.exists());
        std::fs::create_dir(&folder).expect("folder");
        publish(&source, &version(1, b"first")).expect("published");
        assert!(path.exists());
    }

    /// While another publisher holds `<path>.lock`, a publish or replace to the path is refused,
    /// naming the lock and its holder and leaving both as they were; a publish leaves no lock of
    /// its own. Negative control: a publish that took no lock would land the held-lock seq 42.
    #[cfg(unix)]
    #[test]
    fn a_publish_to_a_path_another_publisher_holds_is_refused() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let path = root.path().join("alab.vault");
        let lock = root.path().join("alab.vault.lock");
        let source = Source::Path(path.clone());
        let first = version(41, b"one admin's text");
        publish(&source, &first).expect("published");
        assert!(!lock.exists(), "a publish leaves no lock behind");
        std::fs::write(&lock, b"m22.local 4242\n\x1b[2J").expect("another publisher");
        let error = publish(&source, &version(42, b"a revocation")).expect_err("held");
        assert!(
            error.0.contains("alab.vault.lock") && error.0.contains("(m22.local 4242)"),
            "{error}"
        );
        assert!(
            replace(&source, &version(42, b"a put-back"), &first).is_err(),
            "a replace takes the same lock"
        );
        assert_eq!(std::fs::read(&path).expect("vault"), first);
        assert_eq!(
            std::fs::read(&lock).expect("the holder's lock"),
            b"m22.local 4242\n\x1b[2J"
        );
        std::fs::remove_file(&lock).expect("released");
        assert!(publish(&source, &version(40, b"older")).is_err());
        assert!(!lock.exists(), "a refused publish leaves no lock behind");
        publish(&source, &version(42, b"on top")).expect("newer");
        assert!(!lock.exists());
    }

    /// A one-shot HTTP stub: answers `response` to the first request and hands back the
    /// request head it read.
    fn stub(response: Vec<u8>) -> (String, std::thread::JoinHandle<String>) {
        let (url, server) = stub_each(vec![response]);
        (
            url,
            std::thread::spawn(move || server.join().expect("stub").remove(0)),
        )
    }

    /// An HTTP stub that answers each of `responses` to one request in turn and hands back
    /// the request heads it read.
    fn stub_each(responses: Vec<Vec<u8>>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}/v1/alab", listener.local_addr().expect("addr"));
        let server = std::thread::spawn(move || {
            let mut heads = Vec::new();
            for response in responses {
                let (mut stream, _) = listener.accept().expect("accept");
                stream
                    .set_read_timeout(Some(HANG_DETECTOR))
                    .expect("timeout");
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if stream.read(&mut byte).expect("read") == 0 {
                        break;
                    }
                    head.push(byte[0]);
                }
                let head = String::from_utf8_lossy(&head).into_owned();
                let length = head
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                let mut body = vec![0u8; length];
                stream.read_exact(&mut body).expect("body");
                stream.write_all(&response).expect("write");
                heads.push(head);
            }
            heads
        });
        (url, server)
    }

    fn too_many(retry_after: Option<u64>) -> Vec<u8> {
        let header =
            retry_after.map_or_else(String::new, |secs| format!("Retry-After: {secs}\r\n"));
        format!("HTTP/1.1 429 Too Many Requests\r\n{header}Content-Length: 9\r\n\r\nslow down")
            .into_bytes()
    }

    #[test]
    fn a_join_sits_out_the_change_gap_once_and_no_other_refusal() {
        let line = format!("{} m22 abcd-ef23-ghij-k234", recipient());
        let no_content = b"HTTP/1.1 204 No Content\r\n\r\n".to_vec();
        let paused = std::cell::RefCell::new(Vec::new());
        let pause = |wait: Duration| paused.borrow_mut().push(wait);
        // The gap's 429 names the seconds left: sat out, and the line sent again.
        let (url, server) = stub_each(vec![too_many(Some(7)), no_content.clone()]);
        post_join_pausing(&url, &line, &pause).expect("taken after the wait");
        let heads = server.join().expect("stub");
        assert_eq!(heads.len(), 2);
        assert!(
            heads
                .iter()
                .all(|head| head.starts_with("POST /v1/alab/join "))
        );
        assert_eq!(*paused.borrow(), [Duration::from_secs(7)]);
        // Only once: a second 429 is the answer.
        paused.borrow_mut().clear();
        let (url, server) = stub_each(vec![too_many(Some(1)), too_many(Some(1))]);
        let error = post_join_pausing(&url, &line, &pause).expect_err("refused twice");
        assert!(error.0.contains("429"), "{error}");
        assert_eq!(server.join().expect("stub").len(), 2);
        assert_eq!(*paused.borrow(), [Duration::from_secs(1)]);
        // CONTROL: the queue's caps name no wait, and a wait past the gap is no gap's: each
        // refused at once, with no pause.
        paused.borrow_mut().clear();
        for refusal in [too_many(None), too_many(Some(30))] {
            let (url, server) = stub_each(vec![refusal]);
            let error = post_join_pausing(&url, &line, &pause).expect_err("refused");
            assert!(error.0.contains("429: slow down"), "{error}");
            assert_eq!(server.join().expect("stub").len(), 1);
        }
        assert!(paused.borrow().is_empty());
    }

    /// The Local Network gate is told by where a connect went and how it failed: no route to a
    /// LAN address, and nothing else.
    #[cfg(unix)]
    #[test]
    fn no_route_to_a_lan_address_is_the_local_network_gate() {
        let lan = [
            "10.0.0.5",
            "172.16.4.2",
            "172.31.255.254",
            "192.168.1.20",
            "169.254.10.1",
            "fe80::1",
            "fd12:3456::1",
            "::ffff:192.168.1.20",
        ];
        let elsewhere = [
            "8.8.8.8",
            "192.0.2.1",
            "172.32.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "::1",
            "2001:db8::1",
            "::ffff:8.8.8.8",
        ];
        let gated = [libc::EHOSTUNREACH, libc::ENETUNREACH];
        let other = [
            libc::ECONNREFUSED,
            libc::ETIMEDOUT,
            libc::ECONNRESET,
            libc::EADDRNOTAVAIL,
        ];
        for (addresses, on_lan) in [(&lan, true), (&elsewhere, false)] {
            for address in addresses {
                let ip: IpAddr = address.parse().expect(address);
                for (errnos, no_route) in [(&gated[..], true), (&other[..], false)] {
                    for &errno in errnos {
                        let error = std::io::Error::from_raw_os_error(errno);
                        assert_eq!(
                            local_network_gate(ip, &error),
                            on_lan && no_route,
                            "{address} errno {errno}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_connect_with_no_route_on_the_lan_is_told_as_pending_and_others_are_not() {
        let addr: SocketAddr = "192.168.1.20:7484".parse().expect("addr");
        let unreachable = || std::io::Error::from(std::io::ErrorKind::HostUnreachable);
        let gated = aterm_http::Error::Io(std::io::Error::new(
            std::io::ErrorKind::HostUnreachable,
            NoRoute {
                addr,
                error: unreachable(),
            },
        ));
        let url = "http://alab-secrets.local:7484/v1/alab?token=x";
        let error = transport_error("fetching", url, &gated);
        assert_eq!(error.kind(), ErrorKind::LocalNetworkPending);
        assert!(
            error
                .0
                .starts_with("fetching http://alab-secrets.local:7484/v1/alab: "),
            "{error}"
        );
        assert!(error.0.contains("192.168.1.20"), "{error}");
        assert!(error.0.contains("Local Network"), "{error}");
        assert!(!error.0.contains("token"), "{error}");
        // CONTROL: the same failure with no LAN address behind it is any other error.
        let plain = aterm_http::Error::Io(unreachable());
        assert_eq!(
            transport_error("fetching", url, &plain).kind(),
            ErrorKind::Other
        );
    }

    #[test]
    fn a_refusal_names_the_status_and_a_sanitized_reason() {
        let mut response = b"HTTP/1.1 409 Conflict\r\nContent-Length: 300\r\n\r\n".to_vec();
        let mut body = b"seq 2 is not newer than 5 \x1b[2J".to_vec();
        body.resize(300, b'z');
        response.extend_from_slice(&body);
        let (url, server) = stub(response);
        let error = publish(&Source::Url(url), b"alab-vault/1\n").expect_err("refused");
        let head = server.join().expect("stub");
        assert!(head.starts_with("PUT /v1/alab HTTP/1.1\r\n"), "{head}");
        assert!(error.0.contains("409"), "{error}");
        assert!(
            error.0.contains("seq 2 is not newer than 5 ?[2J"),
            "{error}"
        );
        assert!(!error.0.contains('\x1b'), "{error}");
        assert!(error.0.len() < 300, "{error}");
    }

    #[test]
    fn a_malformed_answer_is_quoted_as_one_short_printable_line() {
        let mut response = b"HTTP/1.1 2x \x1b[2J".to_vec();
        response.resize(2000, b'z');
        response.extend_from_slice(b"\r\nsecond: line\r\n\r\n");
        let (url, server) = stub(response);
        let error = fetch(&Source::Url(url), None, HANG_DETECTOR).expect_err("malformed");
        server.join().expect("stub");
        assert!(error.0.starts_with("fetching http://127.0.0.1:"), "{error}");
        assert!(error.0.len() < 300, "{error}");
        assert!(
            error.0.bytes().all(|b| (0x20..0x7f).contains(&b)),
            "{error}"
        );
        assert!(!error.0.contains("second"), "{error}");
    }

    /// A long-poll's 204 says the seq the server holds by its `ETag`, which a machine holding a
    /// newer version reads; a 204 without one, or with one that names no seq, says none.
    #[test]
    fn a_long_poll_asks_after_the_seq_and_reads_no_newer() {
        let (url, server) = stub(b"HTTP/1.1 204 No Content\r\nETag: \"40\"\r\n\r\n".to_vec());
        let fetched = fetch(&Source::Url(url), Some(41), HANG_DETECTOR).expect("fetched");
        assert_eq!(fetched, Fetched::NoNewer(Some(40)));
        let head = server.join().expect("stub");
        assert!(
            head.starts_with("GET /v1/alab?after=41 HTTP/1.1\r\n"),
            "{head}"
        );
        assert!(
            !head.to_ascii_lowercase().contains("content-length"),
            "{head}"
        );
        for etag in ["", "ETag: 40\r\n", "ETag: \"\"\r\n", "ETag: \"+40\"\r\n"] {
            let (url, server) = stub(format!("HTTP/1.1 204 No Content\r\n{etag}\r\n").into_bytes());
            let fetched = fetch(&Source::Url(url), Some(41), HANG_DETECTOR).expect("fetched");
            assert_eq!(fetched, Fetched::NoNewer(None), "{etag:?}");
            server.join().expect("stub");
        }
    }

    /// A lookup that got no answer and a host with no route are held: how macOS holds an app
    /// it has not let onto the LAN. A refusal, a definite "no such name", no network at all, a
    /// malformed reply and a request that cannot be made are not.
    #[test]
    fn only_a_lookup_with_no_answer_and_no_route_are_held() {
        use std::io::ErrorKind;
        let io = |kind, text: &str| aterm_http::Error::Io(std::io::Error::new(kind, text));
        assert!(held(&io(
            ErrorKind::TimedOut,
            "dns: no answer within 8000ms"
        )));
        assert!(held(&io(ErrorKind::HostUnreachable, "No route to host")));
        for kind in [
            ErrorKind::ConnectionRefused,
            ErrorKind::NetworkUnreachable,
            ErrorKind::NotFound,
            ErrorKind::Other,
            ErrorKind::PermissionDenied,
        ] {
            let error = io(
                kind,
                "dns: alab-secrets.local: nodename nor servname provided",
            );
            assert!(!held(&error), "{kind:?}");
        }
        assert!(!held(&aterm_http::Error::Protocol("2x".into())));
        assert!(!held(&aterm_http::Error::Invalid("no host".into())));
        assert!(!held(&aterm_http::Error::TooLarge { limit: 1 }));
    }

    /// Over real sockets: a port nobody listens on and a server's `503` are not held, and a
    /// server that takes the request and never answers is, once the request's deadline
    /// passes (here a short one, not [`get_bounded`]'s `wait` plus [`FETCH_MARGIN`]).
    #[test]
    fn a_get_says_which_failures_are_held() {
        let closed = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}/v1/alab", closed.local_addr().expect("addr"));
        drop(closed);
        let refused =
            get_bounded(&url, None, HANG_DETECTOR, Some(LOOK_CONNECT)).expect_err("no listener");
        assert!(!refused.held, "{refused:?}");
        assert!(
            refused.why.0.starts_with(&format!("fetching {url}")),
            "{refused:?}"
        );

        let (url, server) =
            stub(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n".to_vec());
        let busy = get_bounded(&url, None, HANG_DETECTOR, Some(LOOK_CONNECT)).expect_err("503");
        server.join().expect("stub");
        assert!(!busy.held && busy.why.0.contains("503"), "{busy:?}");

        let silent = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}/v1/alab", silent.local_addr().expect("addr"));
        let unanswered = get_via(
            &client(Duration::from_millis(250)),
            &url,
            None,
            Some(LOOK_CONNECT),
        )
        .expect_err("silent");
        assert!(unanswered.held, "{unanswered:?}");
        drop(silent);
    }

    /// A connector that answers every connect as a lookup macOS holds would, and keeps the
    /// time its deadline left it.
    #[derive(Debug, Default)]
    struct HeldLookup {
        left: std::sync::Mutex<Vec<Duration>>,
    }

    impl aterm_http::Connect for HeldLookup {
        fn connect(
            &self,
            _host: &str,
            _port: u16,
            deadline: aterm_http::Deadline,
        ) -> std::io::Result<std::net::TcpStream> {
            let left = deadline.remaining().unwrap_or_default();
            self.left.lock().expect("lock").push(left);
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("dns: no answer within {}ms", left.as_millis()),
            ))
        }
    }

    /// A look's try gives its DNS lookup and connect [`LOOK_CONNECT`] of a much longer
    /// request deadline, and a lookup that gets no answer in it is held. Negative control:
    /// with no connect bound the lookup has the whole deadline.
    #[test]
    fn a_looks_lookup_and_connect_get_the_connect_bound_and_a_hung_one_is_held() {
        let held = std::sync::Arc::new(HeldLookup::default());
        let client =
            client(HANG_DETECTOR.saturating_add(FETCH_MARGIN)).with_connector(held.clone());
        let url = "http://alab-secrets.local:7484/v1/alab";
        let bounded = get_via(&client, url, None, Some(LOOK_CONNECT)).expect_err("held lookup");
        assert!(bounded.held, "{bounded:?}");
        assert!(
            bounded.why.0.starts_with(
                "fetching http://alab-secrets.local:7484/v1/alab: dns: no answer within"
            ),
            "{bounded:?}"
        );
        let unbounded = get_via(&client, url, None, None).expect_err("held lookup");
        assert!(unbounded.held, "{unbounded:?}");
        let left = held.left.lock().expect("lock").clone();
        assert_eq!(left.len(), 2);
        assert!(left[0] <= LOOK_CONNECT, "{left:?}");
        assert!(
            left[1] > LOOK_CONNECT,
            "the request's own deadline: {left:?}"
        );
    }

    /// A look ([`get_within`]) gives up on a server that takes its connect and never answers
    /// at its own total, with no [`FETCH_MARGIN`] after it. Negative control: a look on a
    /// fetch's deadline (the total plus the margin) is still waiting at the margin.
    #[test]
    fn a_look_gives_up_on_a_silent_server_at_its_total_with_no_margin() {
        let silent = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}/v1/alab", silent.local_addr().expect("addr"));
        let total = Duration::from_millis(200);
        let started = std::time::Instant::now();
        let error = get_within(&url, total).expect_err("a server that never answers");
        let took = started.elapsed();
        println!(
            "MEASURE a look at a silent server gave up after {} ms (total {} ms)",
            took.as_millis(),
            total.as_millis()
        );
        assert!(error.0.contains("deadline"), "{error:?}");
        assert!(
            took < FETCH_MARGIN,
            "the look waited the fetch margin past its total: {took:?}"
        );
        drop(silent);
    }

    /// A bounded wait's try ([`fetch_within`]) ends at its own total on a server that holds it,
    /// with no [`FETCH_MARGIN`] after it: a long-poll still held then heard nothing newer (an
    /// older server holds one longer than this asker waits). Negative control: a GET that is no
    /// long-poll (no `after`), which a server answers at once, failing so is a timeout.
    #[test]
    fn a_bounded_fetch_ends_at_its_total_as_no_news() {
        let silent = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}/v1/alab", silent.local_addr().expect("addr"));
        let total = Duration::from_millis(200);
        let started = std::time::Instant::now();
        let held = fetch_within(&url, Some(3), total).expect("a held long-poll is no news");
        let took = started.elapsed();
        assert_eq!(held, Fetched::NoNewer(None));
        assert!(
            took < FETCH_MARGIN,
            "the try waited the fetch margin past its total: {took:?}"
        );
        let error = fetch_within(&url, None, total).expect_err("a look that is never answered");
        assert_eq!(error.kind(), ErrorKind::TimedOut, "{error:?}");
        drop(silent);
    }

    /// `text` with its last character changed to another bech32 one: a broken checksum.
    fn flip_last(text: &str) -> String {
        let mut out = text.to_owned();
        let last = out.pop();
        out.push(if last == Some('q') { 'p' } else { 'q' });
        out
    }

    #[test]
    fn join_lines_are_validated() {
        let line = format!("{} m22 abcd-ef23-ghij-k234", recipient());
        let request = parse_join_line(&line).expect("valid");
        assert_eq!(request.host, "m22");
        assert_eq!(request.code, "abcd-ef23-ghij-k234");
        assert_eq!(request.time, None);
        assert_eq!(request.proof, None);
        assert_eq!(request.line(), line);
        let listed = parse_join_line(&format!("{line} 1792530000")).expect("listed");
        assert_eq!(listed.time, Some(1_792_530_000));
        assert_eq!(listed.listed(), format!("{line} 1792530000"));
        let proof = crate::b64::encode_padded(&[7u8; 32]);
        let proved = parse_join_line(&format!("{line} proof={proof}")).expect("proved");
        assert_eq!(proved.proof.as_deref(), Some(proof.as_str()));
        assert_eq!(proved.line(), format!("{line} proof={proof}"));
        let listed = parse_join_line(&format!("{line} 1792530000 proof={proof}")).expect("listed");
        assert_eq!(listed.time, Some(1_792_530_000));
        assert_eq!(listed.proof.as_deref(), Some(proof.as_str()));
        for bad in [
            format!("{line} proof="),
            format!("{line} proof={}", crate::b64::encode_padded(&[7u8; 31])),
            format!("{line} proof={proof} 1792530000"),
            format!("{line} proof={proof}x"),
            String::new(),
            line.replace(' ', "  "),
            format!("{line} x"),
            format!("{line} 1 2"),
            line.replace("m22", "M22"),
            line.replace("m22", "everyone"),
            line.replace("m22", "../x"),
            line.replace("abcd-ef23-ghij-k234", "abcd-ef21-ghij-k234"),
            line.replace("abcd-ef23-ghij-k234", "abcd-ef23"),
            line.replace("abcd-ef23-ghij-k234", "abcdef23ghijk234"),
            line.replace("abcd-ef23-ghij-k234", "abcd-ef23-ghij-k23-"),
            line.replace("age1", "age2"),
            format!("{}q m22 abcd-ef23-ghij-k234", recipient()),
            format!("{} m22 abcd-ef23-ghij-k234", flip_last(&recipient())),
        ] {
            assert!(parse_join_line(&bad).is_err(), "{bad:?}");
        }
        assert!(post_join("http://127.0.0.1:9/v1/alab", "nonsense").is_err());
    }
}
