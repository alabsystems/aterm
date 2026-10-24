// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! HTTP and local-path vault transport.

use crate::Error;
use crate::envelope::MAX_ENVELOPE;
use aterm_http::{Client, ProxyMode, Trust, Uri};
use std::path::PathBuf;
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
/// The deadline of a publish (up to 16 MiB on a LAN) and of the join exchanges.
const EXCHANGE: Duration = Duration::from_secs(120);
/// The largest answer read from a PUT or a join POST (a one-line reason).
const REASON_BODY: usize = 64 * 1024;
/// The largest join queue read (256 lines of under 256 bytes, with room to spare).
const JOINS_BODY: usize = 1 << 20;
/// The most characters of a server's reason quoted in an error.
const REASON_CHARS: usize = 200;

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

/// A server's reason.
pub fn reason(body: &[u8]) -> String {
    let line = body.split(|&b| b == b'\n').next().unwrap_or_default();
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    line.iter()
        .take(REASON_CHARS)
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
}

/// A failure to talk to the server.
fn transport_error(what: &str, url: &str, error: &aterm_http::Error) -> Error {
    Error::new(format!(
        "{what} {}: {}",
        describe(url),
        reason(error.to_string().as_bytes())
    ))
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
    /// Local Network question nobody has answered yet, so a try after the answer can succeed
    /// (a slow network, a host that sleeps and a server that stalls look the same).
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
    let response = match request.send(&[]) {
        Ok(response) => response,
        Err(error) if foreign_reply(&error) => {
            return Ok(Answer::Foreign(transport_error("fetching", url, &error).0));
        }
        Err(error) => {
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

/// Whether `error` is [`Unheard::held`]: aterm-http's resolver gives up on a lookup with no
/// answer as `TimedOut` (a definite "no such name" is another kind), and a connect macOS
/// refuses an app it has not let onto the LAN fails `HostUnreachable`.
fn held(error: &aterm_http::Error) -> bool {
    matches!(
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
                Ok(())
            } else {
                Err(refused(
                    "publishing to",
                    url,
                    response.status(),
                    response.body(),
                ))
            }
        }
        Source::Path(path) => write_path(path, envelope),
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
/// a holder on its machine that no longer runs ([`PublishLock::take_clearing_dead`]); machines
/// that share the mount and a host name are not told apart. Any other lock a killed publisher
/// left refuses every publish until a person removes it.
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
            NotTaken::Held { lock, holder } => Error::new(format!(
                "{}: another publish to this vault holds it{}; if none is running, remove it and \
                 publish again",
                lock.display(),
                held_by(&holder)
            )),
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

/// Remove `lock` when it is a plain file whose holder line names this machine's host name and a
/// pid the kernel says no process has, and the path still holds the file judged (kept open
/// until the remove, so its inode cannot be reused meanwhile).
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
    let Some((host, pid)) = line.trim_end().rsplit_once(' ') else {
        return false;
    };
    let ours = !host.is_empty()
        && [
            aterm_uds::process::host_name(),
            aterm_uds::process::kernel_host_name(),
        ]
        .iter()
        .any(|name| name.as_deref() == Some(host));
    let gone = pid.parse::<i32>().is_ok_and(|pid| {
        aterm_uds::process::process_presence(pid) == aterm_uds::process::ProcessPresence::Absent
    });
    // A person may have put another lock there since it was read.
    let same = std::fs::symlink_metadata(lock)
        .is_ok_and(|now| (now.dev(), now.ino()) == (judged.dev(), judged.ino()));
    let removed = ours && gone && same && std::fs::remove_file(lock).is_ok();
    drop(file);
    removed
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
            return Err(Error::new(format!(
                "seq {} is not newer than seq {}, which {} holds",
                new.seq,
                held.seq,
                path.display()
            )));
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

/// `POST <url>/join` with one request line (`age1… <host> <code>`).
pub fn post_join(url: &str, line: &str) -> crate::Result<()> {
    let line = line.trim_end_matches('\n');
    parse_join_line(line)?;
    let target = join_url(url);
    let response = client(EXCHANGE)
        .post(&target)
        .header("Content-Type", "text/plain; charset=utf-8")
        .limit(REASON_BODY)
        .send(format!("{line}\n").as_bytes())
        .map_err(|e| transport_error("asking to join at", &target, &e))?;
    if response.is_success() {
        Ok(())
    } else {
        Err(refused(
            "asking to join at",
            &target,
            response.status(),
            response.body(),
        ))
    }
}

/// `GET <url>/join`: the pending request lines ([`parse_join_line`]).
pub fn list_joins(url: &str) -> crate::Result<Vec<String>> {
    let target = join_url(url);
    let response = client(EXCHANGE)
        .get(&target)
        .limit(JOINS_BODY)
        .send(&[])
        .map_err(|e| transport_error("listing join requests at", &target, &e))?;
    if response.status() != 200 {
        return Err(refused(
            "listing join requests at",
            &target,
            response.status(),
            response.body(),
        ));
    }
    let text = std::str::from_utf8(response.body())
        .map_err(|_| Error::new("the join queue is not UTF-8 text"))?;
    Ok(text
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
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
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}/v1/alab", listener.local_addr().expect("addr"));
        let server = std::thread::spawn(move || {
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
            head
        });
        (url, server)
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
