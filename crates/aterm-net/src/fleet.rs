// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Mutual exporter-bound HMAC handshake over TLS 1.3 for fleet TCP connections.

use std::fmt;
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use aterm_codec::hex;
use aterm_digest::{hkdf_expand_32, hkdf_extract, hmac_sha256_parts};
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair as _, UnparsedPublicKey};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use rustls::{ClientConfig, ClientConnection, ServerConfig, ServerConnection, StreamOwned};

use crate::slots::{
    ACCEPT_POLL, Claim, DISPLACED, HandshakeWatchdog, Slots, arm_unauthenticated, source_of,
    wait_listener_readable,
};
use crate::tls::{self, EXPORTER_LEN, PinnedServerVerifier, TlsTransport};

/// Base ALPN protocol (`atf/1`) for fleet connections without node proofs.
pub const ALPN: &[u8] = b"atf/1";

/// Preferred ALPN protocol (`atf/2`) where both ends exchange Ed25519 node proofs.
pub const ALPN_NODES: &[u8] = b"atf/2";

const PROTOCOLS: [&[u8]; 2] = [ALPN_NODES, ALPN];

const EXPORTER_LABEL: &[u8] = b"EXPORTER-aterm-fleet-v1";

/// Shared self-signed Ed25519 DER certificate (`CN=aterm-fleet`) pinned by fleet dialers.
pub const CERT_DER: &[u8] = include_bytes!("fleet_cert.der");

const KEY_DER: &[u8] = include_bytes!("fleet_key.der");

const SERVER_NAME: &str = "aterm-fleet";

const MAX_LINE: usize = 128;

const MAX_WORD: usize = 32;

const TAG_LEN: usize = 32;

const MAX_NODE_LINE: usize = 256;

const SIG_LEN: usize = 64;

/// Visibility scope proved and served by a fleet listener (`[fleet] allow`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FleetScope {
    /// Session metadata only (program, state, task, model), without screen or transcript text.
    Facts,
    /// Metadata plus content (titles, cwd, prompts, and recent excerpts).
    Content,
}

impl FleetScope {
    /// Wire word for this scope (`"facts"` or `"content"`).
    #[inline]
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Facts => "facts",
            Self::Content => "content",
        }
    }

    /// Parse a wire word into a [`FleetScope`].
    #[inline]
    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        [Self::Facts, Self::Content]
            .into_iter()
            .find(|scope| scope.word() == word)
    }
}

/// Config value `[fleet] allow = "drive"` enabling [`FleetOp::Drive`] alongside
/// [`FleetScope::Content`].
pub const DRIVE_LEVEL: &str = "drive";

/// Maximum byte length of a [`FleetOp::Survey`] reply.
pub const REPLY_MAX: usize = 256 * 1024;

/// Operation requested and proved by a fleet dialer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FleetOp {
    /// Read the listener's session survey.
    Survey,
    /// Read the issuer's peer book.
    Book,
    /// Register with the fleet issuer (`aterm fleet join`), proved with the join key.
    Join,
    /// Drive sessions on a listener configured with `[fleet] allow = "drive"`.
    Drive,
    /// Replicate a separate application database; never grants terminal survey or drive.
    Database,
}

impl FleetOp {
    /// Wire word for this operation.
    #[inline]
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Survey => "survey",
            Self::Book => "book",
            Self::Join => "join",
            Self::Drive => "drive",
            Self::Database => "database",
        }
    }

    /// Parse a wire word into a [`FleetOp`].
    #[inline]
    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        [
            Self::Survey,
            Self::Book,
            Self::Join,
            Self::Drive,
            Self::Database,
        ]
        .into_iter()
        .find(|op| op.word() == word)
    }
}

/// Fleet secret expanded into its three HKDF-SHA256 role keys (`Ks`, `Kc`, `Kj`).
#[derive(Clone)]
pub struct FleetSecret {
    server: [u8; 32],
    client: [u8; 32],
    join: [u8; 32],
}

impl FleetSecret {
    /// Minimum accepted secret length in bytes (160 bits).
    pub const MIN_LEN: usize = 20;
    /// Maximum accepted secret length in bytes (256 bits).
    pub const MAX_LEN: usize = 32;

    /// Derive the three role keys from `secret` (`20..=32` bytes).
    /// # Errors
    /// [`SecretLength`] when `secret` is shorter than [`Self::MIN_LEN`] or longer than
    /// [`Self::MAX_LEN`].
    pub fn new(secret: &[u8]) -> Result<Self, SecretLength> {
        if !(Self::MIN_LEN..=Self::MAX_LEN).contains(&secret.len()) {
            return Err(SecretLength(secret.len()));
        }
        let prk = hkdf_extract(&[], secret);
        Ok(Self {
            server: hkdf_expand_32(&prk, b"aterm-fleet/v1 server"),
            client: hkdf_expand_32(&prk, b"aterm-fleet/v1 client"),
            join: hkdf_expand_32(&prk, b"aterm-fleet/v1 join"),
        })
    }

    #[inline]
    fn dialer_key(&self, word: &str) -> &[u8; 32] {
        if word == FleetOp::Join.word() {
            &self.join
        } else {
            &self.client
        }
    }
}

impl PartialEq for FleetSecret {
    fn eq(&self, other: &Self) -> bool {
        aterm_digest::ct_eq(&self.server, &other.server)
            & aterm_digest::ct_eq(&self.client, &other.client)
            & aterm_digest::ct_eq(&self.join, &other.join)
    }
}

impl Eq for FleetSecret {}

impl fmt::Debug for FleetSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FleetSecret(..)")
    }
}

impl FleetSecret {
    /// Which fleet this secret is.
    #[must_use]
    pub fn mark(&self) -> FleetMark {
        let mut h = aterm_digest::Sha256::new();
        h.update(b"aterm-fleet/v1 mark");
        h.update(self.server);
        let digest = h.finalize();
        let mut mark = [0; FleetMark::LEN];
        mark.copy_from_slice(&digest[..FleetMark::LEN]);
        FleetMark(mark)
    }
}

/// Which fleet a secret is: the first 8 bytes of SHA-256 over a label and `Ks`,
/// telling nothing of the secret or its keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FleetMark([u8; FleetMark::LEN]);

impl FleetMark {
    /// Mark byte length.
    pub const LEN: usize = 8;

    /// Encode as 16 lowercase hex digits.
    #[must_use]
    pub fn hex(self) -> String {
        hex::encode_lower(&self.0)
    }

    /// Parse from 16 lowercase hex digits.
    #[must_use]
    pub fn from_hex(word: &str) -> Option<Self> {
        if !hex::is_lower(word, 2 * Self::LEN) {
            return None;
        }
        crate::hex_decode(word)?.try_into().ok().map(Self)
    }
}

/// Error returned when a secret's byte length is outside `FleetSecret::MIN_LEN..=MAX_LEN`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SecretLength(pub usize);

impl fmt::Display for SecretLength {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "a fleet secret is {} to {} bytes, not {}",
            FleetSecret::MIN_LEN,
            FleetSecret::MAX_LEN,
            self.0
        )
    }
}

impl std::error::Error for SecretLength {}

/// A node's 32-byte Ed25519 public key.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeKey([u8; NodeKey::LEN]);

impl NodeKey {
    /// Public key byte length.
    pub const LEN: usize = 32;

    /// Raw 32-byte public key.
    #[inline]
    #[must_use]
    pub const fn bytes(&self) -> &[u8; Self::LEN] {
        &self.0
    }

    /// Encode as 64 lowercase hex digits.
    #[must_use]
    pub fn hex(&self) -> String {
        hex::encode_lower(&self.0)
    }

    /// Parse from 64 lowercase hex digits.
    #[must_use]
    pub fn from_hex(word: &str) -> Option<Self> {
        if !hex::is_lower(word, 2 * Self::LEN) {
            return None;
        }
        crate::hex_decode(word)?.try_into().ok().map(Self)
    }

    /// First 8 lowercase hex digits for compact display.
    #[must_use]
    pub fn short(&self) -> String {
        hex::encode_lower(&self.0[..4])
    }
}

impl fmt::Debug for NodeKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NodeKey({})", self.short())
    }
}

/// Every member of the fleet, rather than one named node, in `drive_from`.
pub const ALL_MEMBERS: &str = "*";

/// A `[fleet] drive_from` grant binding a node id to the [`NodeKey`] approved to drive this host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriveGrant {
    /// Granted 32-hex-digit node id, or [`ALL_MEMBERS`].
    pub node: String,
    /// Public key bound to `node` when the grant was recorded; zero for all members.
    pub key: NodeKey,
}

impl DriveGrant {
    /// Format as `<node id>:<key hex>`, or `*` for every member.
    #[must_use]
    pub fn word(&self) -> String {
        if self.all() {
            ALL_MEMBERS.to_owned()
        } else {
            format!("{}:{}", self.node, self.key.hex())
        }
    }

    /// Whether this grant admits every node the fleet's issuer currently binds.
    #[must_use]
    pub fn all(&self) -> bool {
        self.node == ALL_MEMBERS
    }

    /// Whether it admits `node` bound to `key`: it names that node with that key, or every member.
    #[must_use]
    pub fn admits(&self, node: &str, key: NodeKey) -> bool {
        self.all() || (self.node == node && self.key == key)
    }

    /// The grant among `grants` that admits `node` bound to `key` ([`Self::admits`]), the one
    /// naming it first: every check that reads grants asks this, so `*` and a named grant agree.
    #[must_use]
    pub fn admitting<'a>(grants: &'a [Self], node: &str, key: NodeKey) -> Option<&'a Self> {
        grants
            .iter()
            .find(|grant| !grant.all() && grant.admits(node, key))
            .or_else(|| grants.iter().find(|grant| grant.all()))
    }

    /// Parse `<node id>:<key hex>` or `*`, returning `None` if the grant is invalid.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        let word = word.trim();
        if word == ALL_MEMBERS {
            return Some(Self {
                node: ALL_MEMBERS.to_owned(),
                key: NodeKey([0; 32]),
            });
        }
        let (node, key) = word.split_once(':')?;
        let node = node.to_ascii_lowercase();
        if !hex::is_lower(&node, 32) {
            return None;
        }
        Some(Self {
            node,
            key: NodeKey::from_hex(&key.to_ascii_lowercase())?,
        })
    }

    /// Lowercase node id prefix (before `:`) of a `drive_from` entry.
    #[must_use]
    pub fn node_of(word: &str) -> String {
        let word = word.trim();
        word.split_once(':')
            .map_or(word, |(node, _)| node)
            .to_ascii_lowercase()
    }
}

/// Local node id and Ed25519 key pair used to sign [`ALPN_NODES`] channel proofs.
#[derive(Clone)]
pub struct NodeIdentity {
    node: String,
    key: NodeKey,
    pair: Arc<Ed25519KeyPair>,
}

impl NodeIdentity {
    /// Construct from a 32-lowercase-hex-digit `node` id and 32-byte Ed25519 `seed`.
    #[must_use]
    pub fn from_seed(node: &str, seed: &[u8]) -> Option<Self> {
        if !hex::is_lower(node, 32) {
            return None;
        }
        let pair = Ed25519KeyPair::from_seed_unchecked(seed).ok()?;
        let key = NodeKey(pair.public_key().as_ref().try_into().ok()?);
        Some(Self {
            node: node.to_owned(),
            key,
            pair: Arc::new(pair),
        })
    }

    /// This machine's 32-hex-digit node id.
    #[inline]
    #[must_use]
    pub fn node(&self) -> &str {
        &self.node
    }

    /// This machine's Ed25519 public key.
    #[inline]
    #[must_use]
    pub const fn key(&self) -> NodeKey {
        self.key
    }

    fn sign(&self, end: Signer, exporter: &[u8]) -> [u8; SIG_LEN] {
        let signed = self.pair.sign(&node_message(end, exporter, &self.node));
        let mut sig = [0u8; SIG_LEN];
        sig.copy_from_slice(signed.as_ref());
        sig
    }
}

impl fmt::Debug for NodeIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NodeIdentity({} {:?})", self.node, self.key)
    }
}

/// Verified possession proof of a peer's claimed `node` id and Ed25519 `key` on one connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeProof {
    /// Claimed 32-hex-digit node id.
    pub node: String,
    /// Public key whose private half signed the connection's exporter.
    pub key: NodeKey,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Signer {
    Dialer,
    Listener,
}

impl Signer {
    #[inline]
    const fn label(self) -> &'static [u8] {
        match self {
            Self::Dialer => b"aterm-fleet/v2 node dialer",
            Self::Listener => b"aterm-fleet/v2 node listener",
        }
    }
}

fn node_message(end: Signer, exporter: &[u8], node: &str) -> Vec<u8> {
    let mut message = end.label().to_vec();
    message.push(0);
    message.extend_from_slice(exporter);
    message.extend_from_slice(node.as_bytes());
    message
}

fn node_line(me: Option<&NodeIdentity>, end: Signer, exporter: &[u8]) -> String {
    match me {
        Some(me) => format!(
            "node {} {} {}\n",
            me.node,
            me.key.hex(),
            hex::encode_lower(&me.sign(end, exporter))
        ),
        None => "node -\n".to_owned(),
    }
}

/// Read `end`'s node line on this channel and check its signature: the proof, or `None` for
/// `node -`.
///
/// # Errors
/// [`FleetError::NodeProof`] for a signature that does not verify, [`FleetError::Malformed`] for a
/// line that is no node line, or the read's own error.
fn read_node_proof<S: Read>(
    stream: &mut S,
    end: Signer,
    exporter: &[u8],
) -> Result<Option<NodeProof>, FleetError> {
    let Some(line) = read_line_within(stream, MAX_NODE_LINE)? else {
        return Err(FleetError::Io(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the peer closed the connection before its node line",
        )));
    };
    if line == "node -" {
        return Ok(None);
    }
    let malformed = || FleetError::Malformed("a node line is not `node <id> <key> <signature>`");
    let mut words = line.strip_prefix("node ").ok_or_else(malformed)?.split(' ');
    let (Some(node), Some(key), Some(sig), None) =
        (words.next(), words.next(), words.next(), words.next())
    else {
        return Err(malformed());
    };
    let key = NodeKey::from_hex(key).ok_or_else(malformed)?;
    if !hex::is_lower(node, 32) || !hex::is_lower(sig, 2 * SIG_LEN) {
        return Err(malformed());
    }
    let sig = crate::hex_decode(sig).ok_or_else(malformed)?;
    UnparsedPublicKey::new(&ED25519, key.bytes())
        .verify(&node_message(end, exporter, node), &sig)
        .map_err(|_| FleetError::NodeProof)?;
    Ok(Some(NodeProof {
        node: node.to_owned(),
        key,
    }))
}

/// Failure during a fleet TLS or HMAC/node handshake.
#[derive(Debug)]
pub enum FleetError {
    /// Listener's proof did not verify under this fleet's secret; dialer sent nothing.
    ListenerWrongSecret,
    /// Dialer's proof did not verify under this fleet's secret (`NO proof`).
    DialerWrongSecret,
    /// Dialer closed before sending its proof.
    DialerHungUp,
    /// Listener answered `NO proof`.
    Refused,
    /// Source address already holds its share of proved connections (`NO capacity`).
    AddressBusy,
    /// Peer presented a proof signed with this end's own role key.
    Reflection,
    /// Listener does not serve the requested operation (`NO op`).
    UnknownOp(String),
    /// Listener proved a scope word this version does not recognise.
    UnknownScope(String),
    /// Peer's Ed25519 node signature failed verification (`NO node`).
    NodeProof,
    /// Peer negotiated neither `atf/2` nor `atf/1`.
    Alpn,
    /// Protocol line violated framing or syntax rules.
    Malformed(&'static str),
    /// TLS handshake failed for a reason other than ALPN.
    Tls(io::Error),
    /// Handshake exceeded its deadline.
    Timeout(Duration),
    /// Underlying socket I/O error or unexpected EOF.
    Io(io::Error),
}

impl FleetError {
    #[inline]
    fn is_deadline(&self) -> bool {
        match self {
            Self::Tls(e) | Self::Io(e) => matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ),
            _ => false,
        }
    }

    /// The same failure again, for each dial that shares one handshake (the Unix member's).
    #[cfg(unix)]
    pub(crate) fn copied(&self) -> Self {
        match self {
            Self::ListenerWrongSecret => Self::ListenerWrongSecret,
            Self::DialerWrongSecret => Self::DialerWrongSecret,
            Self::DialerHungUp => Self::DialerHungUp,
            Self::Refused => Self::Refused,
            Self::AddressBusy => Self::AddressBusy,
            Self::Reflection => Self::Reflection,
            Self::UnknownOp(op) => Self::UnknownOp(op.clone()),
            Self::UnknownScope(scope) => Self::UnknownScope(scope.clone()),
            Self::NodeProof => Self::NodeProof,
            Self::Alpn => Self::Alpn,
            Self::Malformed(why) => Self::Malformed(why),
            Self::Tls(e) => Self::Tls(copied_io(e)),
            Self::Timeout(within) => Self::Timeout(*within),
            Self::Io(e) => Self::Io(copied_io(e)),
        }
    }
}

/// `e` again: its OS error where it carries one, else its kind and text.
#[cfg(unix)]
pub(crate) fn copied_io(e: &io::Error) -> io::Error {
    e.raw_os_error().map_or_else(
        || io::Error::new(e.kind(), e.to_string()),
        io::Error::from_raw_os_error,
    )
}

impl fmt::Display for FleetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ListenerWrongSecret => f.write_str(
                "the listener did not prove it holds this fleet's secret (another fleet's \
                 machine, or a man in the middle); nothing was sent",
            ),
            Self::DialerWrongSecret => {
                f.write_str("the dialer's proof does not verify under this fleet's secret")
            }
            Self::DialerHungUp => f.write_str(
                "the dialer hung up on this listener's proof (another fleet's machine, or not \
                 a fleet client)",
            ),
            Self::Refused => f.write_str("the listener refused this machine's fleet proof"),
            Self::AddressBusy => f.write_str(
                "the listener already serves as many proved connections from this address as it \
                 serves one address at once; try again when one ends",
            ),
            Self::Reflection => f.write_str("the peer sent back a proof of this end's own role"),
            Self::UnknownOp(op) => write!(f, "the listener does not serve fleet op `{op}`"),
            Self::UnknownScope(scope) => write!(
                f,
                "the listener serves fleet level `{scope}`, which this aterm does not know \
                 (update it)"
            ),
            Self::NodeProof => f.write_str(
                "a node proof does not verify: the signature is not its key's over this channel",
            ),
            Self::Alpn => f.write_str(
                "the peer does not speak the fleet protocol (ALPN atf/2 or atf/1): a pre-fleet \
                 aterm, or not an aterm",
            ),
            Self::Malformed(why) => write!(f, "malformed fleet handshake: {why}"),
            Self::Tls(e) => write!(f, "TLS: {e}"),
            Self::Timeout(within) => write!(f, "no fleet handshake within {within:?}"),
            Self::Io(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for FleetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Tls(e) | Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

/// Listener TLS 1.3 [`ServerConfig`] using [`CERT_DER`], [`ALPN_NODES`]/[`ALPN`], and no resumption
/// or early data.
#[must_use]
pub fn server_config() -> Arc<ServerConfig> {
    #[cfg(all(test, unix))]
    test_socket_headroom();
    static CONFIG: OnceLock<Arc<ServerConfig>> = OnceLock::new();
    Arc::clone(CONFIG.get_or_init(|| {
        tls::init_crypto();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KEY_DER));
        let mut cfg = ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_no_client_auth()
            .with_single_cert(vec![CertificateDer::from(CERT_DER)], key)
            .expect("the embedded fleet certificate and key are one Ed25519 pair");
        cfg.alpn_protocols = PROTOCOLS.iter().map(|alpn| alpn.to_vec()).collect();
        cfg.send_tls13_tickets = 0;
        cfg.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});
        cfg.max_early_data_size = 0;
        Arc::new(cfg)
    }))
}

/// Parallel loopback fixtures deliberately hold hundreds of cloned sockets.
/// macOS shells commonly inherit a 256-descriptor soft limit; that is a fixture
/// resource shortage, not a reason for unrelated peers to fail authentication.
#[cfg(all(test, unix))]
fn test_socket_headroom() {
    static READY: std::sync::Once = std::sync::Once::new();
    READY.call_once(|| {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: limit is a live writable rlimit for this complete call.
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) },
            0
        );
        let wanted = limit.rlim_max.min(4096);
        if limit.rlim_cur < wanted {
            limit.rlim_cur = wanted;
            // SAFETY: initialized resource limit, bounded by the existing hard
            // limit. Only the test process's soft descriptor limit is raised.
            assert_eq!(
                unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const limit) },
                0
            );
        }
        assert!(
            limit.rlim_cur >= 1024,
            "parallel network fixtures need at least 1024 descriptors"
        );
    });
}

/// Dialer TLS 1.3 [`ClientConfig`] pinning [`CERT_DER`] and offering [`ALPN_NODES`] before
/// [`ALPN`].
#[must_use]
pub fn client_config() -> Arc<ClientConfig> {
    #[cfg(all(test, unix))]
    test_socket_headroom();
    static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    Arc::clone(CONFIG.get_or_init(|| {
        tls::init_crypto();
        let verifier = PinnedServerVerifier::new(tls::cert_fingerprint(CERT_DER));
        let mut cfg = ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_no_client_auth();
        cfg.alpn_protocols = PROTOCOLS.iter().map(|alpn| alpn.to_vec()).collect();
        cfg.resumption = rustls::client::Resumption::disabled();
        cfg.enable_early_data = false;
        Arc::new(cfg)
    }))
}

/// Listener handshake without local node identity (delegates to [`server_handshake_as`]).
/// # Errors
/// A [`FleetError`] naming what failed.
pub fn server_handshake(
    tcp: TcpStream,
    secret: &FleetSecret,
    scope: FleetScope,
    serves: &[FleetOp],
    within: Duration,
) -> Result<(TlsTransport<ServerConnection>, FleetOp), FleetError> {
    server_handshake_as(tcp, secret, None, scope, serves, within)
        .map(|accepted| (accepted.transport, accepted.op))
}

/// Listener result from [`server_handshake_as`].
pub struct Accepted {
    /// Authenticated TLS transport with socket deadlines cleared.
    pub transport: TlsTransport<ServerConnection>,
    /// Operation proved by the dialer.
    pub op: FleetOp,
    /// Node proof presented by the dialer under [`ALPN_NODES`], if any.
    pub peer: Option<NodeProof>,
}

/// Complete the listener TLS and fleet handshake on `tcp` within `within`, proving `scope` and
/// optional node `me`.
/// # Errors
/// As [`server_handshake`]; [`FleetError::NodeProof`] for a dialer's node proof that does not
/// verify.
pub fn server_handshake_as(
    tcp: TcpStream,
    secret: &FleetSecret,
    me: Option<&NodeIdentity>,
    scope: FleetScope,
    serves: &[FleetOp],
    within: Duration,
) -> Result<Accepted, FleetError> {
    let watchdog = arm(&tcp, within)?;
    server_handshake_armed(tcp, watchdog, within, (secret, me), scope, serves, |_| true)
}

fn server_handshake_armed(
    tcp: TcpStream,
    watchdog: HandshakeWatchdog,
    within: Duration,
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
    scope: FleetScope,
    serves: &[FleetOp],
    admit: impl FnOnce(FleetOp) -> bool,
) -> Result<Accepted, FleetError> {
    let (mut transport, (op, peer)) = bounded(tcp, watchdog, within, |tcp| {
        let (mut transport, nodes) = accept_tls(tcp, server_config())?;
        let exporter = transport.exporter().to_vec();
        let me = me.filter(|_| nodes);
        let proved = listener_proves(
            transport.stream(),
            &exporter,
            (secret, me, nodes),
            scope,
            serves,
            admit,
        )?;
        Ok((transport, proved))
    })?;
    release_deadlines(transport.stream().get_ref())?;
    Ok(Accepted {
        transport,
        op,
        peer,
    })
}

/// Dialer handshake without local node identity (delegates to [`client_handshake_as`]).
/// # Errors
/// A [`FleetError`] naming what failed. On
/// [`ListenerWrongSecret`](FleetError::ListenerWrongSecret),
/// [`Reflection`](FleetError::Reflection) and every earlier failure, this end sent no byte of
/// its own proof.
pub fn client_handshake(
    tcp: TcpStream,
    secret: &FleetSecret,
    op: FleetOp,
    within: Duration,
) -> Result<(TlsTransport<ClientConnection>, FleetScope), FleetError> {
    client_handshake_as(tcp, secret, None, op, within)
        .map(|dialed| (dialed.transport, dialed.scope))
}

/// Dialer result from [`client_handshake_as`].
pub struct Dialed {
    /// Authenticated TLS transport with socket deadlines cleared.
    pub transport: TlsTransport<ClientConnection>,
    /// Scope proved by the listener.
    pub scope: FleetScope,
    /// Node proof presented by the listener under [`ALPN_NODES`], if any.
    pub peer: Option<NodeProof>,
}

/// Complete the dialer TLS and fleet handshake on `tcp` within `within`, verifying the listener
/// before proving `op` and `me`.
/// # Errors
/// As [`client_handshake`]; [`FleetError::NodeProof`] for a listener's node proof that does not
/// verify, or a listener that refused this end's.
pub fn client_handshake_as(
    tcp: TcpStream,
    secret: &FleetSecret,
    me: Option<&NodeIdentity>,
    op: FleetOp,
    within: Duration,
) -> Result<Dialed, FleetError> {
    client_handshake_by(tcp, secret, me, op, within).map_err(|undialed| undialed.error)
}

/// A dialer's handshake that did not complete ([`client_handshake_by`]).
#[derive(Debug)]
pub struct Undialed {
    /// What failed.
    pub error: FleetError,
    /// The node the listener proved beside its refusal of this end's op or share, past both
    /// proofs of the secret (a listener from this build on, under [`ALPN_NODES`]).
    pub refused_by: Option<NodeProof>,
}

/// [`client_handshake_as`], with the node that refused this end, when the listener proved one.
/// # Errors
/// As [`client_handshake_as`].
pub fn client_handshake_by(
    tcp: TcpStream,
    secret: &FleetSecret,
    me: Option<&NodeIdentity>,
    op: FleetOp,
    within: Duration,
) -> Result<Dialed, Undialed> {
    let mut refused_by = None;
    let undialed = |error: FleetError, refused_by: Option<NodeProof>| Undialed {
        refused_by: refused_by
            .filter(|_| matches!(error, FleetError::UnknownOp(_) | FleetError::AddressBusy)),
        error,
    };
    let watchdog = arm(&tcp, within).map_err(|e| undialed(e, None))?;
    let (mut transport, (scope, peer)) = bounded(tcp, watchdog, within, |tcp| {
        let (mut transport, nodes) = connect_tls(tcp, client_config())?;
        let exporter = transport.exporter().to_vec();
        let me = me.filter(|_| nodes);
        let proved = dialer_proves(
            transport.stream(),
            &exporter,
            (secret, me, nodes),
            op,
            &mut refused_by,
        )?;
        Ok((transport, proved))
    })
    .map_err(|e| undialed(e, refused_by.take()))?;
    release_deadlines(transport.stream().get_ref()).map_err(|e| undialed(e, None))?;
    Ok(Dialed {
        transport,
        scope,
        peer,
    })
}

/// Wall-clock deadline (3 s) for TLS and both proofs on an accepted fleet connection.
pub const HANDSHAKE_WITHIN: Duration = Duration::from_secs(3);

/// Maximum lifetime (30 s) [`serve`] allows a proved connection before cutting it.
pub const SERVED_WITHIN: Duration = Duration::from_secs(30);

/// How often a running aterm's fleet listener reads its key again, so a join's new key is
/// served within it (`aterm-gui`'s `fleet_listen::RETRY`).
pub const KEY_READ_EVERY: Duration = Duration::from_secs(2);

/// Default maximum concurrent connections (handshaking or proved) on a fleet listener.
pub const MAX_SLOTS: usize = 16;

/// Maximum concurrent proved read/join connections allowed from one source (`max(1, slots / 4)`).
#[must_use]
pub const fn per_source(slots: usize) -> usize {
    if slots < 4 { 1 } else { slots / 4 }
}

/// Maximum concurrent proved [`FleetOp::Drive`] connections allowed from one source (`max(1, slots
/// / 2)`).
#[must_use]
pub const fn drive_per_source(slots: usize) -> usize {
    if slots < 2 { 1 } else { slots / 2 }
}

/// How long a connection that has proved nothing keeps its slot against a newcomer from an address
/// holding no more unproved connections than its own (the slots' grace for the fleet's deadline).
pub(crate) const PREAUTH_GRACE: Duration = Duration::from_secs(1);

/// Return `true` if `ip` (after canonicalising IPv4-mapped IPv6) is loopback, private, or
/// link-local.
#[must_use]
pub const fn is_fleet_source(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            let head = v6.segments()[0];
            v6.is_loopback() || head & 0xfe00 == 0xfc00 || head & 0xffc0 == 0xfe80
        }
    }
}

/// Refusal reason when a peer IP fails [`is_fleet_source`].
pub const NOT_A_FLEET_SOURCE: &str = "not a loopback, private or link-local address";

/// Refusal reason when all listener slots are occupied by non-displaceable connections.
pub const AT_CAPACITY: &str = "the fleet listener is at capacity";

/// Filename of the fleet secret in the state directory.
pub const KEY_FILE: &str = "fleet.key";

#[cfg(unix)]
const KEY_FILE_MAX: usize = 2 * FleetSecret::MAX_LEN + 1;

/// Error reading or validating [`KEY_FILE`].
#[derive(Debug)]
pub enum KeyFileError {
    /// Key file or state directory does not exist.
    Missing,
    /// File or directory permissions, owner, or hard-link count are not private.
    NotPrivate(&'static str),
    /// File or directory is a symlink.
    Symlink,
    /// File is not a regular file containing 40–64 lowercase hex digits.
    Malformed(&'static str),
    /// Underlying I/O error.
    Io(io::Error),
}

impl KeyFileError {
    /// Short status token (`"no-key"`, `"key-mode"`, `"key-link"`, `"key-format"`, or
    /// `"key-unreadable"`).
    #[must_use]
    pub const fn word(&self) -> &'static str {
        match self {
            Self::Missing => "no-key",
            Self::NotPrivate(_) => "key-mode",
            Self::Symlink => "key-link",
            Self::Malformed(_) => "key-format",
            Self::Io(_) => "key-unreadable",
        }
    }
}

impl fmt::Display for KeyFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => write!(f, "no {KEY_FILE}: this machine is in no fleet"),
            Self::NotPrivate(why) => write!(f, "{KEY_FILE} is not private: {why}"),
            Self::Symlink => write!(f, "{KEY_FILE} or its directory is a symlink"),
            Self::Malformed(why) => write!(f, "{KEY_FILE} holds no fleet secret: {why}"),
            Self::Io(e) => write!(f, "{KEY_FILE} could not be read: {e}"),
        }
    }
}

impl std::error::Error for KeyFileError {}

/// Parse a [`FleetSecret`] from 40–64 lowercase hex digits (with optional trailing LF).
/// # Errors
/// [`KeyFileError::Malformed`] for any other text.
pub fn parse_key_file(text: &[u8]) -> Result<FleetSecret, KeyFileError> {
    let digits = text.strip_suffix(b"\n").unwrap_or(text);
    let span = 2 * FleetSecret::MIN_LEN..=2 * FleetSecret::MAX_LEN;
    if !span.contains(&digits.len()) || !digits.len().is_multiple_of(2) {
        return Err(KeyFileError::Malformed(
            "not 40 to 64 hex digits on one line",
        ));
    }
    if !digits
        .iter()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
    {
        return Err(KeyFileError::Malformed("not lowercase hex"));
    }
    let bytes = std::str::from_utf8(digits)
        .ok()
        .and_then(crate::hex_decode)
        .ok_or(KeyFileError::Malformed("not lowercase hex"))?;
    FleetSecret::new(&bytes).map_err(|_| KeyFileError::Malformed("not 20 to 32 bytes"))
}

/// Read and validate [`KEY_FILE`] from `state_dir`, enforcing private ownership and permissions.
/// # Errors
/// A [`KeyFileError`] naming the first rule the file or directory breaks.
#[cfg(unix)]
pub fn read_key_file(state_dir: &std::path::Path) -> Result<FleetSecret, KeyFileError> {
    let text = crate::fleet_member::read_private(state_dir, KEY_FILE, KEY_FILE_MAX)?;
    parse_key_file(&text)
}

#[cfg(unix)]
impl From<crate::fleet_member::PrivateFileError> for KeyFileError {
    fn from(error: crate::fleet_member::PrivateFileError) -> Self {
        use crate::fleet_member::PrivateFileError as E;
        match error {
            E::Missing => Self::Missing,
            E::NotPrivate(why) => Self::NotPrivate(why),
            E::Symlink => Self::Symlink,
            E::Malformed("longer than such a file can be") => {
                Self::Malformed("longer than one secret")
            }
            E::Malformed(why) => Self::Malformed(why),
            E::Io(e) => Self::Io(e),
        }
    }
}

/// Audit event emitted by [`serve`].
#[derive(Debug)]
pub enum FleetEvent {
    /// A dialer proved `op` and was handed over.
    Admitted {
        /// Dialer socket address.
        peer: SocketAddr,
        /// Operation proved by the dialer.
        op: FleetOp,
    },
    /// An admitted connection finished (`error` is `None` on clean completion).
    Closed {
        /// Dialer socket address.
        peer: SocketAddr,
        /// Hand-over error string, if any.
        error: Option<String>,
    },
    /// Connection was rejected before hand-over.
    Refused {
        /// Dialer socket address.
        peer: SocketAddr,
        /// Refusal reason ([`NOT_A_FLEET_SOURCE`], [`AT_CAPACITY`], or [`FleetError`]).
        reason: String,
    },
    /// Listener stopped due to a fatal setup error before `running` was cleared.
    Stopped(String),
}

/// Configuration for [`serve`].
#[derive(Clone, Debug)]
pub struct Listening {
    /// Fleet secret.
    pub secret: FleetSecret,
    /// Scope proved and served by this listener.
    pub scope: FleetScope,
    /// Optional node identity proved under [`ALPN_NODES`].
    pub node: Option<NodeIdentity>,
    /// Allowed operations (`NO op` is returned for others).
    pub serves: &'static [FleetOp],
    /// Maximum total concurrent connections (handshaking or proved).
    pub slots: usize,
    /// Maximum concurrent proved read/join connections per source ([`per_source`]).
    pub per_source: usize,
    /// Maximum duration a proved connection is served before being cut ([`SERVED_WITHIN`]).
    pub lifetime: Duration,
}

struct Release {
    _slot: crate::slots::SlotTenure,
    tenure: LiveTenure,
}

/// Proved fleet connection handed to [`serve`]'s callback, releasing its slot and per-source share
/// before signaling EOF to the peer via [`Served::relay_answers`] or [`Served::answer`].
pub struct Served {
    release: Release,
    transport: TlsTransport<ServerConnection>,
    peer: Option<NodeProof>,
    scope: FleetScope,
    admission: Admission,
}

impl Served {
    /// Remote socket address of the peer.
    #[must_use]
    pub fn peer_addr(&self) -> Option<SocketAddr> {
        self.transport.tcp().peer_addr().ok()
    }

    /// Node possession proof presented by the peer on this connection, if any.
    #[must_use]
    pub const fn peer_node(&self) -> Option<&NodeProof> {
        self.peer.as_ref()
    }

    /// The fleet level this listener proved on the connection.
    #[must_use]
    pub const fn scope(&self) -> FleetScope {
        self.scope
    }

    /// Relay the connection to `local` via [`tls::relay_answers`], releasing the listener hold
    /// before EOF.
    /// # Errors
    /// As [`tls::relay_answers`], or if native locality evidence changed.
    pub fn relay_answers(self, local: aterm_uds::CtlStream) -> io::Result<()> {
        self.admission.recheck()?;
        let result = tls::relay_answers(self.transport, local, self.release);
        self.admission.recheck()?;
        result
    }

    /// Serve a direct response via `write` ([`tls::answer`]), flushing and releasing the listener
    /// hold before EOF.
    /// # Errors
    /// `write`'s own, the flush's, or changed native locality evidence.
    pub fn answer<T>(
        self,
        write: impl FnOnce(&mut tls::Answering<'_, ServerConnection>) -> io::Result<T>,
    ) -> io::Result<T> {
        self.admission.recheck()?;
        tls::answer(self.transport, self.release, |stream| {
            let result = write(stream);
            self.admission.recheck()?;
            result
        })
    }
}

#[derive(Clone, Copy)]
enum Locality {
    Fleet,
    #[cfg(unix)]
    Link(crate::lan::Scope),
}

#[derive(Default)]
struct Admission {
    #[cfg(unix)]
    proof: Option<crate::lan::Proof>,
}
impl Admission {
    fn inspect(
        tcp: TcpStream,
        locality: Locality,
        #[cfg(unix)] witness: Option<crate::lan::ListenerWitness>,
    ) -> io::Result<(TcpStream, Self)> {
        match locality {
            Locality::Fleet => Ok((tcp, Self::default())),
            #[cfg(unix)]
            Locality::Link(scope) => {
                let connection = match witness {
                    Some(witness) => crate::lan::Connection::admitted_by(tcp, scope, witness)?,
                    None => crate::lan::Connection::admit(tcp, scope)?,
                };
                let (tcp, proof) = connection.into_parts();
                Ok((tcp, Self { proof: Some(proof) }))
            }
        }
    }
    fn recheck(&self) -> io::Result<()> {
        #[cfg(unix)]
        if let Some(proof) = &self.proof {
            proof.recheck()?;
        }
        Ok(())
    }
}

enum ListenerSocket<'a> {
    Fleet(&'a TcpListener),
    #[cfg(unix)]
    Link(crate::lan::Listener),
}
impl ListenerSocket<'_> {
    fn tcp(&self) -> &TcpListener {
        match self {
            Self::Fleet(tcp) => tcp,
            #[cfg(unix)]
            Self::Link(listener) => &listener.tcp,
        }
    }
    fn recheck(&self) -> io::Result<()> {
        match self {
            Self::Fleet(_) => Ok(()),
            #[cfg(unix)]
            Self::Link(listener) => listener.recheck(),
        }
    }
    #[cfg(unix)]
    fn witness(&self) -> Option<crate::lan::ListenerWitness> {
        match self {
            Self::Fleet(_) => None,
            Self::Link(listener) => Some(listener.witness()),
        }
    }
}

/// Accept and authenticate fleet connections on `listener` until `running` is false, enforcing
/// NET-1 pre-auth slot displacement, per-source shares, and connection lifetimes.
pub fn serve<H, E>(
    listener: &TcpListener,
    listening: &Listening,
    running: &AtomicBool,
    hand_over: H,
    on_event: E,
) where
    H: Fn(Served, FleetOp) -> io::Result<()> + Send + Sync + Clone + 'static,
    E: Fn(FleetEvent) + Send + Sync + Clone + 'static,
{
    serve_inner(
        ListenerSocket::Fleet(listener),
        (listening, &|| (listening.scope, listening.serves)),
        running,
        Locality::Fleet,
        HANDSHAKE_WITHIN,
        hand_over,
        on_event,
    );
}

/// [`serve`], each connection proving the level and serving the ops `offer` names as it is
/// accepted, in place of `listening`'s: a level or an op the listener's owner changes applies to
/// the next connection, and none in flight is cut by it.
pub fn serve_offering<H, E>(
    listener: &TcpListener,
    listening: &Listening,
    offer: &dyn Fn() -> (FleetScope, &'static [FleetOp]),
    running: &AtomicBool,
    hand_over: H,
    on_event: E,
) where
    H: Fn(Served, FleetOp) -> io::Result<()> + Send + Sync + Clone + 'static,
    E: Fn(FleetEvent) + Send + Sync + Clone + 'static,
{
    serve_inner(
        ListenerSocket::Fleet(listener),
        (listening, offer),
        running,
        Locality::Fleet,
        HANDSHAKE_WITHIN,
        hand_over,
        on_event,
    );
}

/// [`serve`], each connection's TLS and both proofs given `handshake_within` in place of
/// [`HANDSHAKE_WITHIN`]: a test's peer, which serves in process and so must finish each
/// handshake, gives it its hang detector.
#[cfg(any(test, debug_assertions))]
pub fn serve_with_handshake<H, E>(
    listener: &TcpListener,
    listening: &Listening,
    running: &AtomicBool,
    handshake_within: Duration,
    hand_over: H,
    on_event: E,
) where
    H: Fn(Served, FleetOp) -> io::Result<()> + Send + Sync + Clone + 'static,
    E: Fn(FleetEvent) + Send + Sync + Clone + 'static,
{
    serve_inner(
        ListenerSocket::Fleet(listener),
        (listening, &|| (listening.scope, listening.serves)),
        running,
        Locality::Fleet,
        handshake_within,
        hand_over,
        on_event,
    );
}

/// Own and serve a prepared listener with native admission before TLS, after
/// authentication, and around each response. A changed attachment stops service,
/// closes all live connections, and drops the listening socket before returning.
/// This retains the fleet listener's bounded slots and lifetimes.
#[cfg(unix)]
pub fn serve_lan<H, E>(
    listener: crate::lan::Listener,
    listening: &Listening,
    running: &AtomicBool,
    hand_over: H,
    on_event: E,
) where
    H: Fn(Served, FleetOp) -> io::Result<()> + Send + Sync + Clone + 'static,
    E: Fn(FleetEvent) + Send + Sync + Clone + 'static,
{
    let locality = Locality::Link(listener.scope);
    serve_inner(
        ListenerSocket::Link(listener),
        (listening, &|| (listening.scope, listening.serves)),
        running,
        locality,
        HANDSHAKE_WITHIN,
        hand_over,
        on_event,
    );
}

fn serve_inner<H, E>(
    listener: ListenerSocket<'_>,
    (listening, offer): (&Listening, &dyn Fn() -> (FleetScope, &'static [FleetOp])),
    running: &AtomicBool,
    locality: Locality,
    handshake_within: Duration,
    hand_over: H,
    on_event: E,
) where
    H: Fn(Served, FleetOp) -> io::Result<()> + Send + Sync + Clone + 'static,
    E: Fn(FleetEvent) + Send + Sync + Clone + 'static,
{
    if let Err(error) = listener.tcp().set_nonblocking(true) {
        drop(listener);
        on_event(FleetEvent::Stopped(format!(
            "could not make the fleet listener non-blocking: {error}"
        )));
        return;
    }
    let slots = Arc::new(Slots::new(listening.slots, PREAUTH_GRACE));
    let live = Arc::new(LiveConnections::default());
    let mut next_id: u64 = 0;
    let mut stopped = None;
    while running.load(Ordering::Acquire) {
        if let Err(error) = listener.recheck() {
            stopped = Some(format!("listener withdrawn: {error}"));
            break;
        }
        live.cut_overdue(listening.lifetime);
        let (tcp, peer) = match listener.tcp().accept() {
            Ok(accepted) => {
                let _ = aterm_uds::keep_out_of_children(&accepted.0);
                accepted
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                // A readiness error backs off like an accept error below, so neither busy-loops.
                if wait_listener_readable(listener.tcp(), ACCEPT_POLL).is_err()
                    && running.load(Ordering::Acquire)
                {
                    std::thread::sleep(ACCEPT_POLL);
                }
                continue;
            }
            Err(_) => {
                std::thread::sleep(ACCEPT_POLL);
                continue;
            }
        };
        if let Err(error) = listener.recheck() {
            stopped = Some(format!("listener withdrawn: {error}"));
            break;
        }
        if matches!(locality, Locality::Fleet) && !is_fleet_source(peer.ip()) {
            on_event(FleetEvent::Refused {
                peer,
                reason: NOT_A_FLEET_SOURCE.to_owned(),
            });
            continue;
        }
        let source = source_of(peer.ip());
        if !slots.make_room(source) {
            on_event(FleetEvent::Refused {
                peer,
                reason: AT_CAPACITY.to_owned(),
            });
            continue;
        }
        let armed = tcp
            .try_clone()
            .map_err(FleetError::Io)
            .and_then(|line| Ok((line, arm(&tcp, handshake_within)?)));
        let (line, watchdog) = match armed {
            Ok(armed) => armed,
            Err(e) => {
                on_event(FleetEvent::Refused {
                    peer,
                    reason: e.to_string(),
                });
                continue;
            }
        };
        next_id = next_id.wrapping_add(1);
        let release = Release {
            _slot: slots.take(next_id, source, watchdog.evictor()),
            tenure: live.hold(next_id, source, line),
        };
        let Listening {
            secret,
            node,
            per_source,
            slots: most,
            ..
        } = listening.clone();
        let (scope, serves) = offer();
        let hand_over = hand_over.clone();
        let tell = on_event.clone();
        #[cfg(unix)]
        let witness = listener.witness();
        let spawned = std::thread::Builder::new()
            .name("aterm-fleet-conn".to_owned())
            .spawn(move || {
                let release = release;
                let handshake: Result<_, FleetError> = (|| {
                    let (tcp, admission) = match Admission::inspect(
                        tcp,
                        locality,
                        #[cfg(unix)]
                        witness,
                    ) {
                        Ok(admitted) => admitted,
                        Err(error) => {
                            // The native refusal still owns the armed phase.
                            // Retire its thread/socket before freeing the slot.
                            let _ = watchdog.finish();
                            return Err(FleetError::Io(error));
                        }
                    };
                    let accepted = server_handshake_armed(
                        tcp,
                        watchdog,
                        handshake_within,
                        (&secret, node.as_ref()),
                        scope,
                        serves,
                        |op| {
                            let drive = op == FleetOp::Drive;
                            let share = if drive {
                                drive_per_source(most)
                            } else {
                                per_source
                            };
                            release.tenure.proved(drive, share)
                        },
                    )?;
                    admission.recheck().map_err(FleetError::Io)?;
                    Ok((accepted, admission))
                })();
                let event = match handshake {
                    Ok((
                        Accepted {
                            transport,
                            op,
                            peer: proof,
                        },
                        admission,
                    )) => {
                        tell(FleetEvent::Admitted { peer, op });
                        let served = Served {
                            transport,
                            release,
                            peer: proof,
                            scope,
                            admission,
                        };
                        FleetEvent::Closed {
                            peer,
                            error: hand_over(served, op).err().map(|e| e.to_string()),
                        }
                    }
                    Err(e) => {
                        drop(release);
                        FleetEvent::Refused {
                            peer,
                            reason: e.to_string(),
                        }
                    }
                };
                tell(event);
            });
        if let Err(e) = spawned {
            on_event(FleetEvent::Refused {
                peer,
                reason: format!("its thread could not start: {e}"),
            });
        }
    }
    drop(listener);
    live.cut_all();
    if let Some(reason) = stopped {
        on_event(FleetEvent::Stopped(reason));
    }
}

#[derive(Default)]
struct LiveConnections {
    fleet_live: Mutex<Vec<Live>>,
}

struct Live {
    id: u64,
    source: IpAddr,
    line: TcpStream,
    proved: Option<Instant>,
    drive: bool,
}

impl LiveConnections {
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Live>> {
        self.fleet_live
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn hold(self: &Arc<Self>, id: u64, source: IpAddr, line: TcpStream) -> LiveTenure {
        self.lock().push(Live {
            id,
            source,
            line,
            proved: None,
            drive: false,
        });
        LiveTenure {
            live: Arc::clone(self),
            id,
        }
    }

    fn cut_overdue(&self, lifetime: Duration) {
        let now = Instant::now();
        self.lock().retain(|live| {
            let overdue = live
                .proved
                .is_some_and(|at| now.saturating_duration_since(at) >= lifetime);
            if overdue {
                aterm_uds::hangup::hang_up(&live.line);
            }
            !overdue
        });
    }

    fn cut_all(&self) {
        for live in std::mem::take(&mut *self.lock()) {
            aterm_uds::hangup::hang_up(&live.line);
        }
    }
}

struct LiveTenure {
    live: Arc<LiveConnections>,
    id: u64,
}

impl LiveTenure {
    fn proved(&self, drive: bool, most: usize) -> bool {
        let now = Instant::now();
        let mut held = self.live.lock();
        let Some(source) = held
            .iter()
            .find(|live| live.id == self.id)
            .map(|live| live.source)
        else {
            return false;
        };
        let proved = held
            .iter()
            .filter(|live| {
                live.id != self.id
                    && live.source == source
                    && live.proved.is_some()
                    && live.drive == drive
            })
            .count();
        if proved >= most {
            return false;
        }
        if let Some(live) = held.iter_mut().find(|live| live.id == self.id) {
            live.proved = Some(now);
            live.drive = drive;
        }
        true
    }
}

impl Drop for LiveTenure {
    fn drop(&mut self) {
        self.live.lock().retain(|live| live.id != self.id);
    }
}

fn arm(tcp: &TcpStream, within: Duration) -> Result<HandshakeWatchdog, FleetError> {
    if within.is_zero() {
        return Err(FleetError::Timeout(within));
    }
    tcp.set_nonblocking(false).map_err(FleetError::Io)?;
    arm_unauthenticated(tcp, within).map_err(FleetError::Io)
}

fn bounded<T>(
    tcp: TcpStream,
    watchdog: HandshakeWatchdog,
    within: Duration,
    handshake: impl FnOnce(TcpStream) -> Result<T, FleetError>,
) -> Result<T, FleetError> {
    let result = handshake(tcp);
    match (watchdog.finish(), result) {
        (Claim::Authed, Err(e)) if e.is_deadline() => Err(FleetError::Timeout(within)),
        (Claim::Authed, result) => result,
        (Claim::Evicted, _) => Err(FleetError::Io(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            DISPLACED,
        ))),
        (Claim::Deadline, _) => Err(FleetError::Timeout(within)),
    }
}

fn release_deadlines(tcp: &TcpStream) -> Result<(), FleetError> {
    tcp.set_read_timeout(None).map_err(FleetError::Io)?;
    tcp.set_write_timeout(None).map_err(FleetError::Io)
}

fn rustls_error(e: &io::Error) -> Option<&rustls::Error> {
    e.get_ref()
        .and_then(|inner| inner.downcast_ref::<rustls::Error>())
}

fn export<C, S>(conn: &C) -> Result<[u8; EXPORTER_LEN], FleetError>
where
    C: std::ops::Deref<Target = rustls::ConnectionCommon<S>>,
{
    conn.export_keying_material([0u8; EXPORTER_LEN], EXPORTER_LABEL, None)
        .map_err(|e| FleetError::Tls(io::Error::other(e)))
}

fn accept_tls(
    tcp: TcpStream,
    config: Arc<ServerConfig>,
) -> Result<(TlsTransport<ServerConnection>, bool), FleetError> {
    let mut conn =
        ServerConnection::new(config).map_err(|e| FleetError::Tls(io::Error::other(e)))?;
    let mut tcp = tcp;
    while conn.is_handshaking() {
        if let Err(e) = conn.complete_io(&mut tcp) {
            return Err(
                if matches!(rustls_error(&e), Some(rustls::Error::NoApplicationProtocol)) {
                    FleetError::Alpn
                } else {
                    FleetError::Tls(tls::listen_handshake_error(e))
                },
            );
        }
    }
    let Some(nodes) = speaks_nodes(conn.alpn_protocol()) else {
        return Err(FleetError::Alpn);
    };
    let exporter = export(&conn)?;
    Ok((
        TlsTransport::from_parts(StreamOwned::new(conn, tcp), exporter),
        nodes,
    ))
}

const fn speaks_nodes(alpn: Option<&[u8]>) -> Option<bool> {
    match alpn {
        Some(ALPN_NODES) => Some(true),
        Some(ALPN) => Some(false),
        _ => None,
    }
}

fn connect_tls(
    tcp: TcpStream,
    config: Arc<ClientConfig>,
) -> Result<(TlsTransport<ClientConnection>, bool), FleetError> {
    let name = ServerName::try_from(SERVER_NAME).expect("\"aterm-fleet\" is a valid DNS name");
    let mut conn =
        ClientConnection::new(config, name).map_err(|e| FleetError::Tls(io::Error::other(e)))?;
    let mut tcp = tcp;
    while conn.is_handshaking() {
        if let Err(e) = conn.complete_io(&mut tcp) {
            let pre_fleet = match rustls_error(&e) {
                Some(rustls::Error::AlertReceived(
                    rustls::AlertDescription::NoApplicationProtocol,
                )) => true,
                Some(rustls::Error::InvalidCertificate(_)) => {
                    speaks_nodes(conn.alpn_protocol()).is_none()
                }
                _ => false,
            };
            return Err(if pre_fleet {
                FleetError::Alpn
            } else {
                FleetError::Tls(tls::dial_handshake_error(e))
            });
        }
    }
    let Some(nodes) = speaks_nodes(conn.alpn_protocol()) else {
        return Err(FleetError::Alpn);
    };
    let exporter = export(&conn)?;
    Ok((
        TlsTransport::from_parts(StreamOwned::new(conn, tcp), exporter),
        nodes,
    ))
}

fn listener_proves<S: Read + Write>(
    stream: &mut S,
    exporter: &[u8],
    (secret, me, nodes): (&FleetSecret, Option<&NodeIdentity>, bool),
    scope: FleetScope,
    serves: &[FleetOp],
    admit: impl FnOnce(FleetOp) -> bool,
) -> Result<(FleetOp, Option<NodeProof>), FleetError> {
    let word = scope.word();
    write_proof(stream, word, &tag(&secret.server, exporter, word))?;
    let Some(line) = read_line(stream)? else {
        return Err(FleetError::DialerHungUp);
    };
    let Some((word, presented)) = parse_proof(&line) else {
        return refuse(
            stream,
            "proof",
            FleetError::Malformed("the dialer's proof is not `<op> <64 hex digits>`"),
        );
    };
    if !verifies(secret.dialer_key(word), exporter, word, &presented) {
        let reflected = verifies(&secret.server, exporter, word, &presented);
        return refuse(
            stream,
            "proof",
            if reflected {
                FleetError::Reflection
            } else {
                FleetError::DialerWrongSecret
            },
        );
    }
    let peer = if nodes {
        match read_node_proof(stream, Signer::Dialer, exporter) {
            Ok(peer) => peer,
            Err(error @ (FleetError::NodeProof | FleetError::Malformed(_))) => {
                return refuse(stream, "node", error);
            }
            Err(error) => return Err(error),
        }
    } else {
        None
    };
    // Past both proofs of the secret, a refusal names this node as `OK` does, so the dialer knows
    // which node refused it.
    let node = nodes.then(|| node_line(me, Signer::Listener, exporter));
    let Some(op) = FleetOp::from_word(word).filter(|op| serves.contains(op)) else {
        return refuse_as(stream, "op", FleetError::UnknownOp(word.to_owned()), node);
    };
    if !admit(op) {
        return refuse_as(stream, "capacity", FleetError::AddressBusy, node);
    }
    send(stream, &format!("OK\n{}", node.unwrap_or_default()))?;
    Ok((op, peer))
}

fn refuse<S: Write, T>(stream: &mut S, why: &str, error: FleetError) -> Result<T, FleetError> {
    refuse_as(stream, why, error, None)
}

/// [`refuse`], with this end's node line after the refusal where it is one.
fn refuse_as<S: Write, T>(
    stream: &mut S,
    why: &str,
    error: FleetError,
    node: Option<String>,
) -> Result<T, FleetError> {
    let _ = stream
        .write_all(format!("NO {why}\n{}", node.unwrap_or_default()).as_bytes())
        .and_then(|()| stream.flush());
    Err(error)
}

fn dialer_proves<S: Read + Write>(
    stream: &mut S,
    exporter: &[u8],
    (secret, me, nodes): (&FleetSecret, Option<&NodeIdentity>, bool),
    op: FleetOp,
    refused_by: &mut Option<NodeProof>,
) -> Result<(FleetScope, Option<NodeProof>), FleetError> {
    let Some(line) = read_line(stream)? else {
        return Err(FleetError::Io(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the listener closed the connection before proving itself",
        )));
    };
    let Some((word, presented)) = parse_proof(&line) else {
        return Err(FleetError::Malformed(
            "the listener's proof is not `<scope> <64 hex digits>`",
        ));
    };
    if !verifies(&secret.server, exporter, word, &presented) {
        let reflected = verifies(&secret.client, exporter, word, &presented)
            || verifies(&secret.join, exporter, word, &presented);
        return Err(if reflected {
            FleetError::Reflection
        } else {
            FleetError::ListenerWrongSecret
        });
    }
    let Some(scope) = FleetScope::from_word(word) else {
        return Err(FleetError::UnknownScope(word.to_owned()));
    };
    let word = op.word();
    let mut proof = proof_line(word, &tag(secret.dialer_key(word), exporter, word));
    if nodes {
        proof.push_str(&node_line(me, Signer::Dialer, exporter));
    }
    send(stream, &proof)?;
    let refusal = match read_line(stream)?.as_deref() {
        Some("OK") => None,
        Some("NO op") => Some(FleetError::UnknownOp(word.to_owned())),
        Some("NO capacity") => Some(FleetError::AddressBusy),
        Some("NO node") => return Err(FleetError::NodeProof),
        Some(verdict) if verdict.strip_prefix("NO ").is_some_and(is_word) => {
            return Err(FleetError::Refused);
        }
        Some(_) => {
            return Err(FleetError::Malformed(
                "the listener's answer is neither `OK` nor `NO <why>`",
            ));
        }
        None => {
            return Err(FleetError::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the listener hung up without answering this machine's proof",
            )));
        }
    };
    if let Some(refusal) = refusal {
        // A listener from this build on proves its node after the refusal; an older one hangs up.
        if nodes {
            *refused_by = read_node_proof(stream, Signer::Listener, exporter)
                .ok()
                .flatten();
        }
        return Err(refusal);
    }
    let peer = if nodes {
        read_node_proof(stream, Signer::Listener, exporter)?
    } else {
        None
    };
    Ok((scope, peer))
}

const fn tag(key: &[u8; 32], exporter: &[u8], word: &str) -> [u8; TAG_LEN] {
    hmac_sha256_parts(key, &[exporter, word.as_bytes()])
}

fn verifies(key: &[u8; 32], exporter: &[u8], word: &str, presented: &[u8]) -> bool {
    aterm_digest::ct_eq(&tag(key, exporter, word), presented)
}

fn is_word(word: &str) -> bool {
    (1..=MAX_WORD).contains(&word.len())
        && word
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn parse_proof(line: &str) -> Option<(&str, Vec<u8>)> {
    let (word, tag_hex) = line.split_once(' ')?;
    if !is_word(word)
        || tag_hex.len() != 2 * TAG_LEN
        || tag_hex.bytes().any(|b| b.is_ascii_uppercase())
    {
        return None;
    }
    Some((word, crate::hex_decode(tag_hex)?))
}

fn proof_line(word: &str, tag: &[u8; TAG_LEN]) -> String {
    format!("{word} {}\n", hex::encode_lower(tag))
}

fn write_proof<W: Write>(
    stream: &mut W,
    word: &str,
    tag: &[u8; TAG_LEN],
) -> Result<(), FleetError> {
    send(stream, &proof_line(word, tag))
}

fn send<W: Write>(stream: &mut W, text: &str) -> Result<(), FleetError> {
    stream
        .write_all(text.as_bytes())
        .and_then(|()| stream.flush())
        .map_err(FleetError::Io)
}

fn read_line<R: Read>(stream: &mut R) -> Result<Option<String>, FleetError> {
    read_line_within(stream, MAX_LINE)
}

fn read_line_within<R: Read>(stream: &mut R, max: usize) -> Result<Option<String>, FleetError> {
    let mut line = Vec::with_capacity(max);
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) if line.is_empty() => return Ok(None),
            Ok(0) => {
                return Err(FleetError::Io(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the peer closed the connection mid-line",
                )));
            }
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) if line.len() >= max => {
                return Err(FleetError::Malformed(if max == MAX_NODE_LINE {
                    "a node line longer than 256 bytes"
                } else {
                    "a line longer than 128 bytes"
                }));
            }
            Ok(_) => line.push(byte[0]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e)
                if line.is_empty()
                    && matches!(
                        e.kind(),
                        io::ErrorKind::UnexpectedEof
                            | io::ErrorKind::ConnectionReset
                            | io::ErrorKind::ConnectionAborted
                            | io::ErrorKind::BrokenPipe
                    ) =>
            {
                return Ok(None);
            }
            Err(e) => return Err(FleetError::Io(e)),
        }
    }
    match String::from_utf8(line) {
        Ok(line) if line.is_ascii() => Ok(Some(line)),
        _ => Err(FleetError::Malformed("a line that is not ASCII")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_uds::CtlStream;
    use std::net::TcpListener;

    const HANG: Duration = Duration::from_secs(60);

    const EVERY_OP: &[FleetOp] = &[
        FleetOp::Survey,
        FleetOp::Book,
        FleetOp::Join,
        FleetOp::Drive,
    ];

    fn secret(byte: u8) -> FleetSecret {
        FleetSecret::new(&[byte; FleetSecret::MIN_LEN]).unwrap()
    }

    fn hex32(s: &str) -> Vec<u8> {
        crate::hex_decode(s).unwrap()
    }

    fn over_loopback<A: Send + 'static, B>(
        listen: impl FnOnce(TcpStream) -> A + Send + 'static,
        dial: impl FnOnce(TcpStream) -> B,
    ) -> (A, B) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let listening = std::thread::spawn(move || listen(listener.accept().unwrap().0));
        let tcp = TcpStream::connect(addr).unwrap();
        tcp.set_read_timeout(Some(HANG)).unwrap();
        let dialed = dial(tcp);
        (listening.join().unwrap(), dialed)
    }

    fn raw_accept(tcp: TcpStream) -> TlsTransport<ServerConnection> {
        tcp.set_read_timeout(Some(HANG)).unwrap();
        accept_tls(tcp, server_config()).unwrap().0
    }

    fn raw_connect(tcp: TcpStream) -> TlsTransport<ClientConnection> {
        connect_tls(tcp, client_config()).unwrap().0
    }

    fn drain<R: Read>(stream: &mut R) -> Vec<u8> {
        let mut got = Vec::new();
        let _ = stream.read_to_end(&mut got);
        got
    }

    #[test]
    fn the_role_keys_and_a_tag_are_the_answers_recorded_before_the_hkdf_moved() {
        let keys = FleetSecret::new(&[0x0b; 22]).unwrap();
        let recorded = |hex: &str| <[u8; 32]>::try_from(hex32(hex)).unwrap();
        assert_eq!(
            keys.server,
            recorded("4372c139d6e12dd79a1067058c17bc0b80c23ec04408767788a0c3564a3a8105")
        );
        assert_eq!(
            keys.client,
            recorded("58c9589b9c8d82c98a1d00fd0e05a0bed3586ec685ce363c6c46f61623b822ef")
        );
        assert_eq!(
            keys.join,
            recorded("45abb3624f933b8d0104508a3cd486edf83a358c4beec2d326b0d73e8a21e066")
        );
        let survey = tag(&keys.client, &[0xa5; 32], "survey");
        assert_eq!(
            survey,
            recorded("f92e33cf27fc4091ec8577e69a3afae1985bfca7b81d9d98738e89272125c732")
        );

        let mut flipped = [0x0b; 22];
        flipped[21] ^= 0x01;
        let other = FleetSecret::new(&flipped).unwrap();
        assert_ne!(other.server, keys.server);
        assert_ne!(other.client, keys.client);
        assert_ne!(other.join, keys.join);
        assert_ne!(tag(&other.client, &[0xa5; 32], "survey"), survey);
    }

    #[test]
    fn a_secret_is_20_to_32_bytes_and_its_role_keys_differ() {
        assert_eq!(FleetSecret::new(&[1; 19]).unwrap_err(), SecretLength(19));
        assert_eq!(FleetSecret::new(&[1; 33]).unwrap_err(), SecretLength(33));
        let short = FleetSecret::new(&[1; 20]).unwrap();
        let long = FleetSecret::new(&[1; 32]).unwrap();
        assert_ne!(short.server, long.server);
        for keys in [&short, &long] {
            assert_ne!(keys.server, keys.client);
            assert_ne!(keys.server, keys.join);
            assert_ne!(keys.client, keys.join);
        }
        assert_eq!(format!("{short:?}"), "FleetSecret(..)");
    }

    #[test]
    fn a_handshake_proves_both_ends_and_hands_over_the_stream() {
        for (scope, op) in [
            (FleetScope::Facts, FleetOp::Survey),
            (FleetScope::Facts, FleetOp::Book),
            (FleetScope::Content, FleetOp::Join),
        ] {
            let (listened, dialed) = over_loopback(
                move |tcp| {
                    let (mut t, op) =
                        server_handshake(tcp, &secret(7), scope, EVERY_OP, HANG).unwrap();
                    let alpn = t.stream().conn.alpn_protocol().map(<[u8]>::to_vec);
                    t.stream().get_ref().set_read_timeout(Some(HANG)).unwrap();
                    let mut request = [0u8; 5];
                    t.stream().read_exact(&mut request).unwrap();
                    t.stream().write_all(b"pong\n").unwrap();
                    t.stream().flush().unwrap();
                    (op, request, t.exporter().to_vec(), alpn)
                },
                move |tcp| {
                    let (mut t, scope) = client_handshake(tcp, &secret(7), op, HANG).unwrap();
                    let alpn = t.stream().conn.alpn_protocol().map(<[u8]>::to_vec);
                    t.stream().write_all(b"ping\n").unwrap();
                    t.stream().flush().unwrap();
                    t.stream().get_ref().set_read_timeout(Some(HANG)).unwrap();
                    let mut answer = [0u8; 5];
                    t.stream().read_exact(&mut answer).unwrap();
                    (scope, answer, t.exporter().to_vec(), alpn)
                },
            );
            assert_eq!(
                listened.0, op,
                "the listener learns the op the dialer proved"
            );
            assert_eq!(
                dialed.0, scope,
                "the dialer learns the scope the listener proved"
            );
            assert_eq!(&listened.1, b"ping\n");
            assert_eq!(&dialed.1, b"pong\n");
            assert_eq!(listened.2, dialed.2, "one exporter on both ends");
            assert_eq!(listened.3.as_deref(), Some(ALPN_NODES));
            assert_eq!(dialed.3.as_deref(), Some(ALPN_NODES));
        }
    }

    fn node(byte: u8) -> NodeIdentity {
        NodeIdentity::from_seed(&format!("{byte:02x}").repeat(16), &[byte; 32]).unwrap()
    }

    #[test]
    fn each_end_proves_its_node_under_atf2_and_neither_under_atf1() {
        let proof = |me: &NodeIdentity| NodeProof {
            node: me.node().to_owned(),
            key: me.key(),
        };
        for (listener, dialer) in [
            (Some(node(1)), Some(node(2))),
            (Some(node(1)), None),
            (None, Some(node(2))),
        ] {
            let (want_heard, want_dialed) =
                (dialer.as_ref().map(proof), listener.as_ref().map(proof));
            let (listened, dialed) = over_loopback(
                move |tcp| {
                    server_handshake_as(
                        tcp,
                        &secret(7),
                        listener.as_ref(),
                        FleetScope::Facts,
                        EVERY_OP,
                        HANG,
                    )
                    .map(|accepted| accepted.peer)
                },
                move |tcp| {
                    client_handshake_as(tcp, &secret(7), dialer.as_ref(), FleetOp::Survey, HANG)
                        .map(|dialed| dialed.peer)
                },
            );
            assert_eq!(listened.unwrap(), want_heard);
            assert_eq!(dialed.unwrap(), want_dialed);
        }
        let (listened, dialed) = over_loopback(
            |tcp| {
                server_handshake_as(
                    tcp,
                    &secret(7),
                    Some(&node(1)),
                    FleetScope::Facts,
                    EVERY_OP,
                    HANG,
                )
                .map(|accepted| (accepted.op, accepted.peer))
            },
            |tcp| {
                let (mut t, nodes) = connect_tls(tcp, dialer_tls(&[ALPN])).unwrap();
                let exporter = t.exporter().to_vec();
                assert!(!nodes, "atf/1 is spoken");
                dialer_proves(
                    t.stream(),
                    &exporter,
                    (&secret(7), Some(&node(2)), nodes),
                    FleetOp::Book,
                    &mut None,
                )
            },
        );
        assert_eq!(listened.unwrap(), (FleetOp::Book, None));
        assert_eq!(dialed.unwrap(), (FleetScope::Facts, None));
        let older = listener_tls(CERT_DER, KEY_DER, &[ALPN]);
        let (listened, dialed) = over_loopback(
            move |tcp| {
                tcp.set_read_timeout(Some(HANG)).unwrap();
                let mut conn = ServerConnection::new(older).unwrap();
                let mut tcp = tcp;
                while conn.is_handshaking() {
                    conn.complete_io(&mut tcp).unwrap();
                }
                let exporter = export(&conn).unwrap();
                let mut stream = StreamOwned::new(conn, tcp);
                listener_proves(
                    &mut stream,
                    &exporter,
                    (&secret(7), None, false),
                    FleetScope::Content,
                    EVERY_OP,
                    |_| true,
                )
            },
            |tcp| {
                client_handshake_as(tcp, &secret(7), Some(&node(2)), FleetOp::Survey, HANG)
                    .map(|dialed| (dialed.scope, dialed.peer))
            },
        );
        assert_eq!(listened.unwrap(), (FleetOp::Survey, None));
        assert_eq!(dialed.unwrap(), (FleetScope::Content, None));
    }

    #[test]
    fn a_node_proof_proves_only_its_own_end_on_its_own_channel() {
        const E: &[u8] = &[0xa5; EXPORTER_LEN];
        const OTHER: &[u8] = &[0x5a; EXPORTER_LEN];
        let keys = secret(7);
        let me = node(3);
        let op_line = proof_line("survey", &tag(&keys.client, E, "survey"));
        let forged = |named: &NodeIdentity, signer: &NodeIdentity, end: Signer, exporter: &[u8]| {
            format!(
                "node {} {} {}\n",
                named.node(),
                named.key().hex(),
                hex::encode_lower(&signer.sign(end, exporter))
            )
        };

        let listener_hears = |line: String| {
            let (mut listener, mut dialer) = CtlStream::pair().unwrap();
            dialer.set_read_timeout(Some(HANG)).unwrap();
            let listening = std::thread::spawn(move || {
                listener.set_read_timeout(Some(HANG)).unwrap();
                listener_proves(
                    &mut listener,
                    E,
                    (&secret(7), Some(&node(9)), true),
                    FleetScope::Facts,
                    EVERY_OP,
                    |_| true,
                )
            });
            let _hello = read_line(&mut dialer).unwrap();
            dialer
                .write_all(format!("{op_line}{line}").as_bytes())
                .unwrap();
            let answer = read_line(&mut dialer).unwrap();
            (listening.join().unwrap(), answer)
        };
        let (heard, answer) = listener_hears(node_line(Some(&me), Signer::Dialer, E));
        assert_eq!(answer.as_deref(), Some("OK"));
        assert!(
            matches!(&heard, Ok((FleetOp::Survey, Some(proof))) if proof.node == me.node() && proof.key == me.key()),
            "{heard:?}"
        );
        let (heard, answer) = listener_hears("node -\n".to_owned());
        assert!(matches!(heard, Ok((FleetOp::Survey, None))), "{heard:?}");
        assert_eq!(answer.as_deref(), Some("OK"));
        for refused in [
            forged(&me, &me, Signer::Dialer, OTHER),
            forged(&me, &me, Signer::Listener, E),
            forged(&me, &node(4), Signer::Dialer, E),
        ] {
            let (heard, answer) = listener_hears(refused.clone());
            assert!(
                matches!(heard, Err(FleetError::NodeProof)),
                "{refused:?}: {heard:?}"
            );
            assert_eq!(answer.as_deref(), Some("NO node"));
        }
        for malformed in ["node\n", "node 1234 5678 9abc\n", "node - -\n"] {
            let (heard, answer) = listener_hears(malformed.to_owned());
            assert!(
                matches!(heard, Err(FleetError::Malformed(_))),
                "{malformed:?}: {heard:?}"
            );
            assert_eq!(answer.as_deref(), Some("NO node"));
        }

        let dialer_hears = |line: String| {
            let (mut dialer, mut listener) = CtlStream::pair().unwrap();
            listener.set_read_timeout(Some(HANG)).unwrap();
            let dialing = std::thread::spawn(move || {
                dialer.set_read_timeout(Some(HANG)).unwrap();
                dialer_proves(
                    &mut dialer,
                    E,
                    (&secret(7), Some(&node(9)), true),
                    FleetOp::Survey,
                    &mut None,
                )
            });
            let hello = proof_line("facts", &tag(&keys.server, E, "facts"));
            listener.write_all(hello.as_bytes()).unwrap();
            let sent = (
                read_line(&mut listener).unwrap(),
                read_line_within(&mut listener, MAX_NODE_LINE).unwrap(),
            );
            listener
                .write_all(format!("OK\n{line}").as_bytes())
                .unwrap();
            drop(listener);
            (dialing.join().unwrap(), sent)
        };
        let (heard, sent) = dialer_hears(node_line(Some(&me), Signer::Listener, E));
        assert!(
            matches!(&heard, Ok((FleetScope::Facts, Some(proof))) if proof.node == me.node()),
            "{heard:?}"
        );
        assert_eq!(sent.0.as_deref(), Some(op_line.trim_end()));
        assert_eq!(
            sent.1,
            Some(
                node_line(Some(&node(9)), Signer::Dialer, E)
                    .trim_end()
                    .to_owned()
            ),
            "the dialer's node goes with its op"
        );
        for refused in [
            forged(&me, &me, Signer::Listener, OTHER),
            forged(&me, &me, Signer::Dialer, E),
            forged(&me, &node(4), Signer::Listener, E),
        ] {
            let (heard, _) = dialer_hears(refused.clone());
            assert!(
                matches!(heard, Err(FleetError::NodeProof)),
                "{refused:?}: {heard:?}"
            );
        }
    }

    #[test]
    fn a_wrong_secret_is_named_by_the_end_that_sees_it() {
        let (listened, dialed) = over_loopback(
            |tcp| server_handshake(tcp, &secret(2), FleetScope::Facts, EVERY_OP, HANG).err(),
            |tcp| client_handshake(tcp, &secret(1), FleetOp::Survey, HANG).err(),
        );
        assert!(
            matches!(dialed, Some(FleetError::ListenerWrongSecret)),
            "{dialed:?}"
        );
        assert!(
            matches!(listened, Some(FleetError::DialerHungUp)),
            "{listened:?}"
        );

        let (listened, answer) = over_loopback(
            |tcp| server_handshake(tcp, &secret(2), FleetScope::Facts, EVERY_OP, HANG).err(),
            |tcp| {
                let mut t = raw_connect(tcp);
                let exporter = t.exporter().to_vec();
                let _hello = read_line(t.stream()).unwrap();
                let word = FleetOp::Survey.word();
                write_proof(t.stream(), word, &tag(&secret(1).client, &exporter, word)).unwrap();
                read_line(t.stream()).unwrap()
            },
        );
        assert!(
            matches!(listened, Some(FleetError::DialerWrongSecret)),
            "{listened:?}"
        );
        assert_eq!(answer.as_deref(), Some("NO proof"));
    }

    #[test]
    fn a_reflected_proof_is_refused() {
        let (listened, (reflected, exporter, answer)) = over_loopback(
            |tcp| server_handshake(tcp, &secret(7), FleetScope::Facts, EVERY_OP, HANG).err(),
            |tcp| {
                let mut t = raw_connect(tcp);
                let exporter = t.exporter().to_vec();
                let hello = read_line(t.stream()).unwrap().unwrap();
                t.stream()
                    .write_all(format!("{hello}\n").as_bytes())
                    .unwrap();
                t.stream().flush().unwrap();
                let (_, reflected) = parse_proof(&hello).unwrap();
                (reflected, exporter, read_line(t.stream()).unwrap())
            },
        );
        assert!(
            matches!(listened, Some(FleetError::Reflection)),
            "{listened:?}"
        );
        assert_eq!(answer.as_deref(), Some("NO proof"));
        assert!(verifies(&secret(7).server, &exporter, "facts", &reflected));
        assert!(!verifies(&secret(7).client, &exporter, "facts", &reflected));

        let (sent, dialed) = over_loopback(
            |tcp| {
                let mut t = raw_accept(tcp);
                let exporter = t.exporter().to_vec();
                let word = FleetScope::Facts.word();
                write_proof(t.stream(), word, &tag(&secret(7).client, &exporter, word)).unwrap();
                drain(t.stream())
            },
            |tcp| client_handshake(tcp, &secret(7), FleetOp::Survey, HANG).err(),
        );
        assert!(matches!(dialed, Some(FleetError::Reflection)), "{dialed:?}");
        assert!(
            sent.is_empty(),
            "the dialer sent {sent:?} to an unproved listener"
        );

        let within = Duration::from_millis(500);
        let (echoed, dialed) = over_loopback(
            |tcp| {
                tcp.set_read_timeout(Some(HANG)).unwrap();
                let Ok((mut t, _)) = accept_tls(tcp, server_config()) else {
                    return Vec::new();
                };
                let got = drain(t.stream());
                let _ = t.stream().write_all(&got);
                got
            },
            |tcp| client_handshake(tcp, &secret(7), FleetOp::Survey, within).err(),
        );
        assert!(
            matches!(dialed, Some(FleetError::Timeout(w)) if w == within),
            "{dialed:?}"
        );
        assert!(echoed.is_empty(), "the dialer sent {echoed:?} to an echo");
    }

    #[test]
    fn a_tag_proves_only_its_own_word_on_its_own_channel() {
        const E: &[u8] = &[0xa5; EXPORTER_LEN];
        const OTHER: &[u8] = &[0x5a; EXPORTER_LEN];
        let keys = secret(7);

        let listener_hears = |line: String| {
            let (mut listener, mut dialer) = CtlStream::pair().unwrap();
            dialer.set_read_timeout(Some(HANG)).unwrap();
            let listening = std::thread::spawn(move || {
                listener.set_read_timeout(Some(HANG)).unwrap();
                listener_proves(
                    &mut listener,
                    E,
                    (&secret(7), None, false),
                    FleetScope::Content,
                    EVERY_OP,
                    |_| true,
                )
                .map(|(op, _)| op)
            });
            let _hello = read_line(&mut dialer).unwrap();
            dialer.write_all(line.as_bytes()).unwrap();
            let answer = read_line(&mut dialer).unwrap();
            (listening.join().unwrap(), answer)
        };
        let proof = |key: &[u8; 32], exporter: &[u8], tagged: &str, sent: &str| {
            format!(
                "{sent} {}\n",
                hex::encode_lower(&tag(key, exporter, tagged))
            )
        };
        let (heard, answer) = listener_hears(proof(&keys.client, E, "survey", "survey"));
        assert!(matches!(heard, Ok(FleetOp::Survey)), "{heard:?}");
        assert_eq!(answer.as_deref(), Some("OK"));
        let (heard, answer) = listener_hears(proof(&keys.join, E, "join", "join"));
        assert!(matches!(heard, Ok(FleetOp::Join)), "{heard:?}");
        assert_eq!(answer.as_deref(), Some("OK"));
        let (heard, answer) = listener_hears(proof(&keys.client, E, "book", "book"));
        assert!(matches!(heard, Ok(FleetOp::Book)), "{heard:?}");
        assert_eq!(answer.as_deref(), Some("OK"));
        let (heard, answer) = listener_hears(proof(&keys.client, E, "drive", "drive"));
        assert!(matches!(heard, Ok(FleetOp::Drive)), "{heard:?}");
        assert_eq!(answer.as_deref(), Some("OK"));
        for refused in [
            proof(&keys.client, E, "survey", "drive"),
            proof(&keys.client, E, "drive", "survey"),
            proof(&keys.join, E, "drive", "drive"),
            proof(&keys.client, E, "survey", "join"),
            proof(&keys.join, E, "join", "survey"),
            proof(&keys.client, E, "survey", "book"),
            proof(&keys.client, E, "book", "survey"),
            proof(&keys.client, E, "join", "join"),
            proof(&keys.client, OTHER, "survey", "survey"),
        ] {
            let (heard, answer) = listener_hears(refused.clone());
            assert!(
                matches!(heard, Err(FleetError::DialerWrongSecret)),
                "{refused:?}: {heard:?}"
            );
            assert_eq!(answer.as_deref(), Some("NO proof"));
        }
        let (heard, answer) = listener_hears(proof(&keys.client, E, "steer", "steer"));
        assert!(
            matches!(&heard, Err(FleetError::UnknownOp(op)) if op == "steer"),
            "{heard:?}"
        );
        assert_eq!(answer.as_deref(), Some("NO op"));
        let (heard, answer) = listener_hears(format!("survey {}\n", "AB".repeat(32)));
        assert!(matches!(heard, Err(FleetError::Malformed(_))), "{heard:?}");
        assert_eq!(answer.as_deref(), Some("NO proof"));

        let dialer_hears = |hello: String, answer: &'static str| {
            let (mut dialer, mut listener) = CtlStream::pair().unwrap();
            listener.set_read_timeout(Some(HANG)).unwrap();
            let dialing = std::thread::spawn(move || {
                dialer.set_read_timeout(Some(HANG)).unwrap();
                dialer_proves(
                    &mut dialer,
                    E,
                    (&secret(7), None, false),
                    FleetOp::Survey,
                    &mut None,
                )
                .map(|(scope, _)| scope)
            });
            listener.write_all(hello.as_bytes()).unwrap();
            let sent = read_line(&mut listener).unwrap();
            if sent.is_some() {
                listener.write_all(answer.as_bytes()).unwrap();
            }
            drop(listener);
            (dialing.join().unwrap(), sent)
        };
        let (heard, sent) = dialer_hears(proof(&keys.server, E, "content", "content"), "OK\n");
        assert!(matches!(heard, Ok(FleetScope::Content)), "{heard:?}");
        assert_eq!(
            sent,
            Some(
                proof(&keys.client, E, "survey", "survey")
                    .trim_end()
                    .to_owned()
            )
        );
        for refused in [
            proof(&keys.server, E, "content", "facts"),
            proof(&keys.server, OTHER, "facts", "facts"),
        ] {
            let (heard, sent) = dialer_hears(refused.clone(), "OK\n");
            assert!(
                matches!(heard, Err(FleetError::ListenerWrongSecret)),
                "{refused:?}: {heard:?}"
            );
            assert_eq!(
                sent, None,
                "the dialer sent its proof to an unproved listener"
            );
        }
        let hello = proof(&keys.server, E, "facts", "facts");
        let (heard, _) = dialer_hears(hello.clone(), "NO proof\n");
        assert!(matches!(heard, Err(FleetError::Refused)), "{heard:?}");
        let (heard, _) = dialer_hears(hello.clone(), "NO op\n");
        assert!(
            matches!(&heard, Err(FleetError::UnknownOp(op)) if op == "survey"),
            "{heard:?}"
        );
        let (heard, _) = dialer_hears(hello, "MAYBE\n");
        assert!(matches!(heard, Err(FleetError::Malformed(_))), "{heard:?}");
        let (heard, sent) = dialer_hears(proof(&keys.server, E, "drive", "drive"), "OK\n");
        assert!(
            matches!(&heard, Err(FleetError::UnknownScope(s)) if s == "drive"),
            "{heard:?}"
        );
        assert_eq!(sent, None);
    }

    fn listener_tls(cert: &[u8], key: &[u8], alpn: &[&[u8]]) -> Arc<ServerConfig> {
        tls::init_crypto();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.to_vec()));
        let mut cfg = ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_no_client_auth()
            .with_single_cert(vec![CertificateDer::from(cert.to_vec())], key)
            .unwrap();
        cfg.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
        Arc::new(cfg)
    }

    fn dialer_tls(alpn: &[&[u8]]) -> Arc<ClientConfig> {
        let mut cfg = (*client_config()).clone();
        cfg.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
        Arc::new(cfg)
    }

    fn raw_tls<C, S>(mut conn: C, mut tcp: TcpStream)
    where
        C: std::ops::DerefMut<Target = rustls::ConnectionCommon<S>>,
        S: rustls::SideData,
    {
        tcp.set_read_timeout(Some(HANG)).unwrap();
        while conn.is_handshaking() {
            if conn.complete_io(&mut tcp).is_err() {
                return;
            }
        }
        let _ = drain(&mut StreamOwned::new(conn, tcp));
    }

    #[test]
    fn a_peer_without_a_fleet_protocol_is_named_so_from_either_end() {
        const NET_CERT: &[u8] = include_bytes!("testdata/cert.der");
        const NET_KEY: &[u8] = include_bytes!("testdata/key.pkcs8.der");
        let listeners = [
            listener_tls(NET_CERT, NET_KEY, &[]),
            listener_tls(CERT_DER, KEY_DER, &[]),
            listener_tls(CERT_DER, KEY_DER, &[b"atf/3"]),
        ];
        for (i, listener) in listeners.into_iter().enumerate() {
            let ((), dialed) = over_loopback(
                move |tcp| raw_tls(ServerConnection::new(listener).unwrap(), tcp),
                |tcp| client_handshake(tcp, &secret(7), FleetOp::Survey, HANG).err(),
            );
            assert!(
                matches!(dialed, Some(FleetError::Alpn)),
                "listener {i}: {dialed:?}"
            );
        }
        for offered in [&[][..], &[&b"h2"[..]][..]] {
            let dialer = dialer_tls(offered);
            let (listened, ()) = over_loopback(
                |tcp| server_handshake(tcp, &secret(7), FleetScope::Facts, EVERY_OP, HANG).err(),
                move |tcp| {
                    let name = ServerName::try_from(SERVER_NAME).unwrap();
                    raw_tls(ClientConnection::new(dialer, name).unwrap(), tcp);
                },
            );
            assert!(
                matches!(listened, Some(FleetError::Alpn)),
                "{offered:?}: {listened:?}"
            );
        }
        let other = listener_tls(NET_CERT, NET_KEY, &[ALPN]);
        let ((), dialed) = over_loopback(
            move |tcp| raw_tls(ServerConnection::new(other).unwrap(), tcp),
            |tcp| client_handshake(tcp, &secret(7), FleetOp::Survey, HANG).err(),
        );
        assert!(
            matches!(dialed, Some(FleetError::Tls(_))),
            "another certificate under atf/1 is a TLS failure: {dialed:?}"
        );
    }

    #[test]
    fn a_dialer_that_proves_nothing_in_time_is_timed_out() {
        let within = Duration::from_millis(500);
        let (listened, ()) = over_loopback(
            move |tcp| server_handshake(tcp, &secret(7), FleetScope::Facts, EVERY_OP, within).err(),
            |tcp| {
                if let Ok((mut t, _)) = connect_tls(tcp, client_config()) {
                    let _ = drain(t.stream());
                }
            },
        );
        assert!(
            matches!(listened, Some(FleetError::Timeout(w)) if w == within),
            "{listened:?}"
        );
    }

    #[test]
    fn an_op_the_listener_does_not_serve_is_answered_no_op() {
        const OLDER: &[FleetOp] = &[FleetOp::Survey, FleetOp::Join];
        const READ_ONLY: &[FleetOp] = &[FleetOp::Survey, FleetOp::Book, FleetOp::Join];
        for (serves, op) in [
            (&[FleetOp::Survey][..], FleetOp::Join),
            (OLDER, FleetOp::Book),
            (READ_ONLY, FleetOp::Drive),
        ] {
            let (listened, dialed) = over_loopback(
                move |tcp| server_handshake(tcp, &secret(7), FleetScope::Facts, serves, HANG).err(),
                move |tcp| client_handshake(tcp, &secret(7), op, HANG).err(),
            );
            assert!(
                matches!(&listened, Some(FleetError::UnknownOp(word)) if word == op.word()),
                "{listened:?}"
            );
            assert!(
                matches!(&dialed, Some(FleetError::UnknownOp(word)) if word == op.word()),
                "{dialed:?}"
            );
        }
    }

    #[test]
    fn a_refusal_proves_the_node_that_refused() {
        const READ_ONLY: &[FleetOp] = &[FleetOp::Survey, FleetOp::Book, FleetOp::Join];
        for (listener, full) in [(Some(node(1)), false), (Some(node(1)), true), (None, false)] {
            let want = listener.as_ref().map(|me| NodeProof {
                node: me.node().to_owned(),
                key: me.key(),
            });
            let ((), dialed) = over_loopback(
                move |tcp| {
                    let watchdog = arm(&tcp, HANG).unwrap();
                    let me = (&secret(7), listener.as_ref());
                    let scope = FleetScope::Facts;
                    let _ =
                        server_handshake_armed(tcp, watchdog, HANG, me, scope, READ_ONLY, |_| {
                            !full
                        });
                },
                |tcp| {
                    let op = if full {
                        FleetOp::Survey
                    } else {
                        FleetOp::Drive
                    };
                    client_handshake_by(tcp, &secret(7), Some(&node(2)), op, HANG).err()
                },
            );
            let Some(Undialed { error, refused_by }) = dialed else {
                panic!("the listener served it");
            };
            assert!(
                matches!(
                    (&error, full),
                    (FleetError::UnknownOp(_), false) | (FleetError::AddressBusy, true)
                ),
                "{error:?}"
            );
            assert_eq!(refused_by, want, "{error:?}");
        }
    }

    #[test]
    fn only_loopback_private_and_link_local_sources_are_served() {
        for served in [
            "127.0.0.1",
            "127.255.0.9",
            "10.0.0.1",
            "172.16.0.1",
            "172.31.255.254",
            "192.168.1.20",
            "169.254.10.10",
            "::1",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "febf::1",
            "::ffff:192.168.1.20",
            "::ffff:127.0.0.1",
        ] {
            assert!(
                is_fleet_source(served.parse().unwrap()),
                "{served} is served"
            );
        }
        for refused in [
            "8.8.8.8",
            "172.15.255.255",
            "172.32.0.1",
            "192.169.0.1",
            "100.64.0.1",
            "198.51.100.7",
            "224.0.0.251",
            "255.255.255.255",
            "0.0.0.0",
            "2001:db8::1",
            "2606:4700::1111",
            "fec0::1",
            "ff02::1",
            "::",
            "::ffff:8.8.8.8",
        ] {
            assert!(
                !is_fleet_source(refused.parse().unwrap()),
                "{refused} is refused"
            );
        }
    }

    #[test]
    fn a_key_file_is_lowercase_hex_on_one_line() {
        let secret = [0xa7_u8; 20];
        let hex = hex::encode_lower(&secret);
        for text in [hex.clone(), format!("{hex}\n"), "5c".repeat(32)] {
            assert!(parse_key_file(text.as_bytes()).is_ok(), "{text:?} reads");
        }
        assert_eq!(
            parse_key_file(format!("{hex}\n").as_bytes())
                .unwrap()
                .server,
            FleetSecret::new(&secret).unwrap().server
        );
        for text in [
            String::new(),
            "ab".repeat(19),
            "ab".repeat(33),
            format!("{hex}0"),
            hex.to_uppercase(),
            format!("{hex}\n\n"),
            format!(" {hex}"),
            format!("{hex}\r\n"),
            format!("{}zz", &hex[..38]),
        ] {
            assert!(
                matches!(
                    parse_key_file(text.as_bytes()),
                    Err(KeyFileError::Malformed(_))
                ),
                "{text:?} is refused"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_key_file_is_read_only_when_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |path: &std::path::Path, bits: u32| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits)).unwrap();
        };
        let root = std::env::temp_dir().join(format!("aterm-fleet-key-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let state = root.join("state");
        std::fs::create_dir_all(&state).unwrap();
        mode(&root, 0o700);
        mode(&state, 0o700);
        let word = |dir: &std::path::Path| read_key_file(dir).err().map(|e| e.word());

        assert_eq!(word(&state), Some("no-key"));
        assert_eq!(word(&root.join("absent")), Some("no-key"));
        mode(&state, 0o755);
        assert_eq!(word(&state), Some("no-key"));
        mode(&state, 0o700);
        let key = state.join(KEY_FILE);
        std::fs::write(&key, format!("{}\n", "3e".repeat(20))).unwrap();
        mode(&key, 0o600);
        assert_eq!(word(&state), None, "a private key file reads");

        mode(&key, 0o640);
        assert_eq!(word(&state), Some("key-mode"));
        mode(&key, 0o604);
        assert_eq!(word(&state), Some("key-mode"));
        mode(&key, 0o600);
        mode(&state, 0o750);
        assert_eq!(word(&state), Some("key-mode"));
        mode(&state, 0o700);

        let second = root.join("second-link");
        std::fs::hard_link(&key, &second).unwrap();
        assert_eq!(word(&state), Some("key-mode"));
        std::fs::remove_file(&second).unwrap();
        assert_eq!(word(&state), None);

        let real = root.join("real.key");
        std::fs::rename(&key, &real).unwrap();
        std::os::unix::fs::symlink(&real, &key).unwrap();
        assert_eq!(word(&state), Some("key-link"));
        std::fs::remove_file(&key).unwrap();
        std::fs::rename(&real, &key).unwrap();
        let linked = root.join("linked-state");
        std::os::unix::fs::symlink(&state, &linked).unwrap();
        assert_eq!(word(&linked), Some("key-link"));
        assert_eq!(word(&state), None);

        std::fs::remove_file(&key).unwrap();
        std::fs::create_dir(&key).unwrap();
        mode(&key, 0o700);
        assert_eq!(word(&state), Some("key-format"));
        std::fs::remove_dir(&key).unwrap();
        std::fs::write(&key, "ab".repeat(64)).unwrap();
        mode(&key, 0o600);
        assert_eq!(word(&state), Some("key-format"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_listener_hands_proved_streams_over_and_cuts_them_when_stopped() {
        use std::sync::mpsc;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let running = Arc::new(AtomicBool::new(true));
        let (events, heard) = mpsc::channel::<FleetEvent>();
        let events = Arc::new(Mutex::new(events));
        let listening = Listening {
            secret: secret(7),
            scope: FleetScope::Content,
            node: None,
            serves: &[FleetOp::Survey],
            slots: 1,
            per_source: 1,
            lifetime: HANG,
        };
        let serving = {
            let running = Arc::clone(&running);
            std::thread::spawn(move || {
                serve(
                    &listener,
                    &listening,
                    &running,
                    |served, _op| served.answer(echo_all),
                    move |event| {
                        let _ = events.lock().unwrap().send(event);
                    },
                );
            })
        };
        let next = |want: &str| loop {
            let event = heard
                .recv_timeout(HANG)
                .expect("an event within the hang detector");
            let name = format!("{event:?}");
            if name.starts_with(want) {
                return event;
            }
        };

        let tcp = TcpStream::connect(addr).unwrap();
        let (mut held, scope) = client_handshake(tcp, &secret(7), FleetOp::Survey, HANG).unwrap();
        assert_eq!(scope, FleetScope::Content);
        assert!(matches!(
            next("Admitted"),
            FleetEvent::Admitted {
                op: FleetOp::Survey,
                ..
            }
        ));
        held.stream()
            .get_ref()
            .set_read_timeout(Some(HANG))
            .unwrap();
        held.stream().write_all(b"ping\n").unwrap();
        held.stream().flush().unwrap();
        let mut echo = [0u8; 5];
        held.stream().read_exact(&mut echo).unwrap();
        assert_eq!(&echo, b"ping\n");

        let tcp = TcpStream::connect(addr).unwrap();
        tcp.set_read_timeout(Some(HANG)).unwrap();
        assert!(client_handshake(tcp, &secret(7), FleetOp::Survey, HANG).is_err());
        assert!(
            matches!(next("Refused"), FleetEvent::Refused { reason, .. } if reason == AT_CAPACITY)
        );

        running.store(false, Ordering::Release);
        serving.join().unwrap();
        let mut rest = Vec::new();
        let ended = held.stream().read_to_end(&mut rest);
        assert!(
            rest.is_empty()
                && !ended.is_err_and(|e| matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                )),
            "the handed-over stream is cut: {rest:?}"
        );
        assert!(matches!(next("Closed"), FleetEvent::Closed { .. }));
    }

    #[test]
    fn a_peer_that_read_its_end_dials_again_at_once() {
        for (slots, per_source) in [(1, 1), (2, 1)] {
            for end in [End::Relayed, End::Answered] {
                let served = redial(slots, per_source, end);
                assert_eq!(
                    served.as_deref().ok(),
                    Some("answered"),
                    "{end:?}: {slots} slots, a share of {per_source}: {served:?}"
                );
            }
            let held = redial(slots, per_source, End::Kept);
            let refused = match &held {
                Err(FleetError::AddressBusy) => per_source < slots,
                Err(_) => slots == 1,
                Ok(_) => false,
            };
            assert!(
                refused,
                "the control: {slots} slots, a share of {per_source}: {held:?}"
            );
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum End {
        Relayed,
        Answered,
        Kept,
    }

    fn redial(slots: usize, per_source: usize, end: End) -> Result<String, FleetError> {
        use std::sync::mpsc;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let running = Arc::new(AtomicBool::new(true));
        let (go, gate) = mpsc::channel::<()>();
        let gate = Arc::new(Mutex::new(gate));
        let listening = Listening {
            secret: secret(7),
            scope: FleetScope::Facts,
            node: None,
            serves: &[FleetOp::Survey],
            slots,
            per_source,
            lifetime: HANG,
        };
        let serving = {
            let running = Arc::clone(&running);
            std::thread::spawn(move || {
                serve(
                    &listener,
                    &listening,
                    &running,
                    move |served: Served, _op| {
                        let (answered, kept) = match end {
                            End::Answered => (
                                served.answer(|stream| {
                                    let mut asked = [0u8; 5];
                                    stream.read_exact(&mut asked)?;
                                    stream.write_all(b"answered\n")
                                }),
                                None,
                            ),
                            End::Relayed | End::Kept => {
                                let (local, mut answering) = CtlStream::pair()?;
                                let answerer = std::thread::spawn(move || {
                                    let mut asked = [0u8; 5];
                                    answering.read_exact(&mut asked)?;
                                    answering.write_all(b"answered\n")?;
                                    answering.flush()?;
                                    aterm_uds::hangup::hang_up(&answering);
                                    Ok::<(), io::Error>(())
                                });
                                let (relayed, kept) = match end {
                                    End::Kept => {
                                        let Served {
                                            transport, release, ..
                                        } = served;
                                        (tls::relay_answers(transport, local, ()), Some(release))
                                    }
                                    _ => (served.relay_answers(local), None),
                                };
                                let _ = answerer.join();
                                (relayed, kept)
                            }
                        };
                        let _ = gate.lock().unwrap().recv_timeout(HANG);
                        drop(kept);
                        answered
                    },
                    |_: FleetEvent| {},
                );
            })
        };
        let dial = || {
            let tcp = TcpStream::connect(addr).map_err(FleetError::Io)?;
            let (mut proved, _) = client_handshake(tcp, &secret(7), FleetOp::Survey, HANG)?;
            proved
                .stream()
                .get_ref()
                .set_read_timeout(Some(HANG))
                .map_err(FleetError::Io)?;
            Ok::<_, FleetError>(proved)
        };
        let ask = |proved: &mut TlsTransport<ClientConnection>| {
            proved.stream().write_all(b"ping\n")?;
            proved.stream().flush()?;
            let mut line = Vec::new();
            let mut byte = [0u8; 1];
            while proved.stream().read(&mut byte)? == 1 && byte[0] != b'\n' {
                line.push(byte[0]);
            }
            Ok::<_, io::Error>(String::from_utf8_lossy(&line).into_owned())
        };
        let mut first = dial().expect("the first dial is served");
        assert_eq!(ask(&mut first).expect("its answer"), "answered");
        let mut rest = Vec::new();
        let end = first.stream().read_to_end(&mut rest);
        assert!(
            end.is_ok() && rest.is_empty(),
            "the first connection ends cleanly: {end:?} {rest:?}"
        );
        let second = dial().and_then(|mut proved| ask(&mut proved).map_err(FleetError::Io));
        drop(go);
        running.store(false, Ordering::Release);
        serving.join().unwrap();
        second
    }

    #[test]
    fn the_cut_frees_the_slot_of_a_peer_that_sent_its_fin_and_reads_nothing() {
        use std::sync::mpsc;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let running = Arc::new(AtomicBool::new(true));
        let (events, heard) = mpsc::channel::<FleetEvent>();
        let events = Arc::new(Mutex::new(events));
        let listening = Listening {
            secret: secret(7),
            scope: FleetScope::Facts,
            node: None,
            serves: &[FleetOp::Survey],
            slots: 1,
            per_source: 1,
            lifetime: Duration::from_millis(200),
        };
        let serving = {
            let running = Arc::clone(&running);
            std::thread::spawn(move || {
                serve(
                    &listener,
                    &listening,
                    &running,
                    |served: Served, _op| {
                        let (local, answering) = CtlStream::pair()?;
                        let answerer = std::thread::spawn(move || {
                            let chunk = vec![b'a'; 64 * 1024];
                            while (&answering).write_all(&chunk).is_ok() {}
                        });
                        let relayed = served.relay_answers(local);
                        let _ = answerer.join();
                        relayed
                    },
                    move |event| {
                        let _ = events.lock().unwrap().send(event);
                    },
                );
            })
        };
        let next = |want: &str| loop {
            let event = heard
                .recv_timeout(HANG)
                .unwrap_or_else(|_| panic!("{want} within the hang detector"));
            if format!("{event:?}").starts_with(want) {
                return event;
            }
        };

        let tcp = TcpStream::connect(addr).unwrap();
        let (silent, _) = client_handshake(tcp, &secret(7), FleetOp::Survey, HANG).unwrap();
        assert!(matches!(next("Admitted"), FleetEvent::Admitted { .. }));
        silent.tcp().shutdown(std::net::Shutdown::Write).unwrap();
        assert!(matches!(next("Closed"), FleetEvent::Closed { .. }));
        let tcp = TcpStream::connect(addr).unwrap();
        let again = client_handshake(tcp, &secret(7), FleetOp::Survey, HANG);
        assert!(
            again.is_ok(),
            "the next dial is served while the silent member holds its connection: {:?}",
            again.err()
        );
        drop(again);
        running.store(false, Ordering::Release);
        serving.join().unwrap();
        drop(silent);
    }

    #[derive(Default)]
    struct Heard {
        refused: Mutex<Vec<String>>,
        at_capacity: std::sync::atomic::AtomicUsize,
        displaced: std::sync::atomic::AtomicUsize,
        refused_v6: Mutex<Vec<String>>,
    }

    struct Serving {
        heard: Arc<Heard>,
        running: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Serving {
        fn start(listener: TcpListener, listening: Listening) -> Self {
            let heard = Arc::new(Heard::default());
            let running = Arc::new(AtomicBool::new(true));
            let thread = std::thread::spawn({
                let (heard, running) = (Arc::clone(&heard), Arc::clone(&running));
                move || {
                    serve_with_handshake(
                        &listener,
                        &listening,
                        &running,
                        HANG,
                        echo,
                        move |event| {
                            if let FleetEvent::Refused { peer, reason } = event {
                                heard.refused.lock().unwrap().push(reason.clone());
                                if reason == AT_CAPACITY {
                                    heard.at_capacity.fetch_add(1, Ordering::SeqCst);
                                }
                                if reason == DISPLACED {
                                    heard.displaced.fetch_add(1, Ordering::SeqCst);
                                }
                                if peer.ip().to_canonical().is_ipv6() {
                                    heard.refused_v6.lock().unwrap().push(reason);
                                }
                            }
                        },
                    );
                }
            });
            Self {
                heard,
                running,
                thread: Some(thread),
            }
        }
    }

    impl Drop for Serving {
        fn drop(&mut self) {
            self.running.store(false, Ordering::Release);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn echo(served: Served, _op: FleetOp) -> io::Result<()> {
        served.answer(echo_all)
    }

    fn echo_all(stream: &mut tls::Answering<'_, ServerConnection>) -> io::Result<()> {
        let mut got = [0u8; 64];
        loop {
            let n = stream.read(&mut got)?;
            if n == 0 {
                return Ok(());
            }
            stream.write_all(&got[..n])?;
            stream.flush()?;
        }
    }

    fn echoes(conn: &mut TlsTransport<ClientConnection>) -> bool {
        let mut back = [0u8; 5];
        conn.stream().get_ref().set_read_timeout(Some(HANG)).is_ok()
            && conn.stream().write_all(b"ping\n").is_ok()
            && conn.stream().flush().is_ok()
            && conn.stream().read_exact(&mut back).is_ok()
            && &back == b"ping\n"
    }

    fn listening(slots: usize, lifetime: Duration) -> Listening {
        Listening {
            secret: secret(7),
            scope: FleetScope::Facts,
            node: None,
            serves: &[FleetOp::Survey],
            slots,
            per_source: slots,
            lifetime,
        }
    }

    #[test]
    fn idle_sockets_from_one_address_never_turn_away_a_member_from_another() {
        use crate::slots::tests::{IdleSockets, dual_stack_listener, until_turned_away};
        use std::net::{Ipv4Addr, Ipv6Addr};
        let test = "idle_sockets_from_one_address_never_turn_away_a_member_from_another";
        let (listener, port) = dual_stack_listener(test);
        let slots = 16;
        let served = Serving::start(listener, listening(slots, HANG));
        let attacker = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let member = SocketAddr::from((Ipv6Addr::LOCALHOST, port));

        let idle = IdleSockets::hold(attacker, slots);
        until_turned_away(&served.heard.at_capacity, attacker);

        let tcp = TcpStream::connect_timeout(&member, HANG).unwrap();
        let (mut admitted, _) = client_handshake(tcp, &secret(7), FleetOp::Survey, HANG)
            .unwrap_or_else(|e| {
                panic!(
                    "idle sockets from 127.0.0.1 turned the member from ::1 away: {e} (the \
                     listener said {:?}; {} turned away at capacity, {} displaced)",
                    served.heard.refused_v6.lock().unwrap(),
                    served.heard.at_capacity.load(Ordering::SeqCst),
                    served.heard.displaced.load(Ordering::SeqCst),
                )
            });
        assert!(echoes(&mut admitted), "the admitted member is served");
        assert!(
            served.heard.displaced.load(Ordering::SeqCst) >= 1,
            "the member got in by taking an idle socket's slot, and the listener said so"
        );
        drop(idle);
    }

    #[test]
    fn idle_sockets_never_lock_a_member_of_their_own_address_out() {
        use crate::slots::tests::{IdleSockets, until_turned_away};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let served = Serving::start(listener, listening(MAX_SLOTS, HANG));
        let dial = || {
            let tcp = TcpStream::connect_timeout(&addr, HANG).map_err(FleetError::Io)?;
            client_handshake(tcp, &secret(7), FleetOp::Survey, HANG)
        };

        let (mut control, _) = dial().expect("an unloaded listener admits");
        assert!(echoes(&mut control), "the control is served");
        drop(control);

        let idle = IdleSockets::hold(addr, MAX_SLOTS);
        until_turned_away(&served.heard.at_capacity, addr);
        let started = Instant::now();
        let (mut admitted, _) = loop {
            match dial() {
                Ok(admitted) => break admitted,
                Err(e) => {
                    assert!(
                        started.elapsed() < HANG,
                        "{MAX_SLOTS} idle sockets locked the member out for {HANG:?} (last \
                         refusal: {e}; {} turned away at capacity, {} displaced)",
                        served.heard.at_capacity.load(Ordering::SeqCst),
                        served.heard.displaced.load(Ordering::SeqCst),
                    );
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        };
        assert!(echoes(&mut admitted), "the admitted member is served");
        assert!(
            served.heard.displaced.load(Ordering::SeqCst) >= 1,
            "the member got in by taking an idle socket's slot, and the listener said so"
        );
        drop(idle);
    }

    #[test]
    fn a_slow_handshake_is_never_cut_for_a_heavier_address() {
        use crate::slots::tests::{IdleSockets, dual_stack_listener, until_turned_away};
        use std::net::{Ipv4Addr, Ipv6Addr};
        let test = "a_slow_handshake_is_never_cut_for_a_heavier_address";
        let (listener, port) = dual_stack_listener(test);
        let served = Serving::start(listener, listening(MAX_SLOTS, HANG));
        let attacker = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let member = SocketAddr::from((Ipv6Addr::LOCALHOST, port));

        let idle = IdleSockets::hold(attacker, MAX_SLOTS);
        until_turned_away(&served.heard.at_capacity, attacker);

        let late = PREAUTH_GRACE + Duration::from_millis(500);
        let tcp = TcpStream::connect_timeout(&member, HANG).unwrap();
        std::thread::sleep(late);
        let (mut admitted, _) = client_handshake(tcp, &secret(7), FleetOp::Survey, HANG)
            .unwrap_or_else(|e| {
                panic!(
                    "a handshake from ::1 sent {late:?} after its connect was cut while idle \
                     sockets from 127.0.0.1 held the other slots: {e} (the listener said {:?})",
                    served.heard.refused_v6.lock().unwrap(),
                )
            });
        assert!(echoes(&mut admitted), "the admitted member is served");
        assert!(
            served.heard.refused_v6.lock().unwrap().is_empty(),
            "nothing from ::1 was turned away"
        );
        drop(idle);
    }

    #[test]
    fn proved_members_still_fill_the_listener_and_are_never_displaced() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let served = Serving::start(listener, listening(MAX_SLOTS, HANG));
        let dial = || {
            let tcp = TcpStream::connect_timeout(&addr, HANG).map_err(FleetError::Io)?;
            client_handshake(tcp, &secret(7), FleetOp::Survey, HANG)
        };

        let mut members: Vec<_> = (0..MAX_SLOTS)
            .map(|i| {
                dial()
                    .unwrap_or_else(|e| {
                        panic!(
                            "member {i}: {e}; listener refused: {:?}",
                            served.heard.refused.lock().unwrap()
                        )
                    })
                    .0
            })
            .collect();
        std::thread::sleep(PREAUTH_GRACE + Duration::from_millis(500));

        assert!(
            dial().is_err(),
            "a newcomer is turned away while every slot holds a proved member"
        );
        assert!(served.heard.at_capacity.load(Ordering::SeqCst) >= 1);
        assert_eq!(served.heard.displaced.load(Ordering::SeqCst), 0);
        for (i, member) in members.iter_mut().enumerate() {
            assert!(echoes(member), "member {i} is still served");
        }
    }

    #[test]
    fn one_address_holds_at_most_its_share_of_proved_connections() {
        use crate::slots::tests::dual_stack_listener;
        use std::net::{Ipv4Addr, Ipv6Addr};
        let test = "one_address_holds_at_most_its_share_of_proved_connections";
        let (listener, port) = dual_stack_listener(test);
        let slots = 8;
        let served = Serving::start(
            listener,
            Listening {
                per_source: per_source(slots),
                ..listening(slots, HANG)
            },
        );
        assert_eq!(per_source(slots), 2);
        let dial = |to: SocketAddr| {
            let tcp = TcpStream::connect_timeout(&to, HANG).map_err(FleetError::Io)?;
            client_handshake(tcp, &secret(7), FleetOp::Survey, HANG)
        };
        let busy = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let other = SocketAddr::from((Ipv6Addr::LOCALHOST, port));

        let mut held: Vec<_> = (0..2)
            .map(|i| {
                dial(busy)
                    .unwrap_or_else(|e| panic!("connection {i}: {e}"))
                    .0
            })
            .collect();
        let third = dial(busy).err();
        assert!(
            matches!(third, Some(FleetError::AddressBusy)),
            "the address past its share is refused by name: {third:?}"
        );
        let (mut member, _) = dial(other).expect("another address still gets a slot");
        assert!(echoes(&mut member), "the other member is served");
        for (i, conn) in held.iter_mut().enumerate() {
            assert!(echoes(conn), "connection {i} is still served");
        }
        drop(held.pop());
        let started = Instant::now();
        while let Err(e) = dial(busy) {
            assert!(
                started.elapsed() < HANG,
                "the address was never admitted again: {e}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(served.heard.displaced.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_drive_has_its_own_share_beside_the_reads() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let slots = 8;
        let _served = Serving::start(
            listener,
            Listening {
                serves: &[FleetOp::Survey, FleetOp::Drive],
                per_source: per_source(slots),
                ..listening(slots, HANG)
            },
        );
        assert_eq!((per_source(slots), drive_per_source(slots)), (2, 4));
        let dial = |op| {
            let tcp = TcpStream::connect_timeout(&addr, HANG).map_err(FleetError::Io)?;
            client_handshake(tcp, &secret(7), op, HANG).map(|(conn, _)| conn)
        };
        let mut reads: Vec<_> = (0..2)
            .map(|i| dial(FleetOp::Survey).unwrap_or_else(|e| panic!("read {i}: {e}")))
            .collect();
        assert!(matches!(
            dial(FleetOp::Survey).err(),
            Some(FleetError::AddressBusy)
        ));
        let mut drives: Vec<_> = (0..4)
            .map(|i| dial(FleetOp::Drive).unwrap_or_else(|e| panic!("drive {i}: {e}")))
            .collect();
        let past = dial(FleetOp::Drive).err();
        assert!(
            matches!(past, Some(FleetError::AddressBusy)),
            "a drive past its own share is refused by name: {past:?}"
        );
        for conn in reads.iter_mut().chain(drives.iter_mut()) {
            assert!(echoes(conn), "every one held is still served");
        }
    }

    #[test]
    fn a_proved_connection_is_cut_at_its_lifetime() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let _served = Serving::start(listener, listening(1, Duration::from_millis(300)));
        let dial = || {
            let tcp = TcpStream::connect(addr).map_err(FleetError::Io)?;
            client_handshake(tcp, &secret(7), FleetOp::Survey, HANG)
        };

        let (mut silent, _) = dial().unwrap();
        silent
            .stream()
            .get_ref()
            .set_read_timeout(Some(HANG))
            .unwrap();
        let mut rest = Vec::new();
        let ended = silent.stream().read_to_end(&mut rest);
        assert!(
            rest.is_empty()
                && !ended.is_err_and(|e| matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                )),
            "the silent connection is cut: {rest:?}"
        );
        let started = std::time::Instant::now();
        while let Err(e) = dial() {
            assert!(
                started.elapsed() < HANG,
                "the cut connection never gave its slot back: {e}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
