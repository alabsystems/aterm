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

/// Database repair with signed membership records before application access.
/// The fleet/node proofs are unchanged; both ends must select this protocol
/// before using its repair order. Ordinary fleet connections do not offer it.
pub const ALPN_DATABASE_REPAIR: &[u8] = b"atdb/2";

const PROTOCOLS: [&[u8]; 2] = [ALPN_NODES, ALPN];

/// ALPN protocol (`atf-ask/1`) of a Mac asking to join ALab (`docs/DESIGN-fluid-swarm-2026-10-28.md`
/// F10): the one path a caller without the fleet secret reaches. It is not in [`PROTOCOLS`], so no
/// fleet dialer offers it, and only a listener given an ask handler ([`serve_offering_with_ask`])
/// negotiates it. No fleet op is ever dispatched on it: the stream goes to the ask handler alone.
pub const ALPN_ASK: &[u8] = b"atf-ask/1";

/// The protocols a listener that serves ask offers: the fleet's first, so a fleet dialer, which
/// never offers [`ALPN_ASK`], negotiates exactly what it did before.
const PROTOCOLS_ASKED: [&[u8]; 3] = [ALPN_NODES, ALPN, ALPN_ASK];

const EXPORTER_LABEL: &[u8] = b"EXPORTER-aterm-fleet-v1";

/// Shared self-signed Ed25519 DER certificate (`CN=aterm-fleet`) pinned by fleet dialers.
pub const CERT_DER: &[u8] = include_bytes!("fleet_cert.der");

const KEY_DER: &[u8] = include_bytes!("fleet_key.der");

const SERVER_NAME: &str = "aterm-fleet";

const MAX_LINE: usize = 128;

const MAX_WORD: usize = 32;

const TAG_LEN: usize = 32;

const MAX_NODE_LINE: usize = 256;

/// An Ed25519 signature's byte length.
pub const SIG_LEN: usize = 64;

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
    /// Push a member's C16 `survey facts` snapshot on the 3-minute heartbeat to the fleet issuer.
    Facts,
    /// Ask a steward for the newest sign-in code or link for a warm or leased email (`mail-code <email>`).
    MailCode,
    /// Read the cached swarm census computed every 5 minutes on the fleet issuer (`aterm fleet census`).
    Census,
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
            Self::Facts => "facts",
            Self::MailCode => "mail-code",
            Self::Census => "census",
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
            Self::Facts,
            Self::MailCode,
            Self::Census,
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
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeKey([u8; NodeKey::LEN]);

impl NodeKey {
    /// Public key byte length.
    pub const LEN: usize = 32;

    /// The key whose raw bytes are `bytes`, as a membership log binds it
    /// (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.2). Nothing checks that they are a point of
    /// the curve: a key that is none verifies no signature.
    #[inline]
    #[must_use]
    pub const fn from_bytes(bytes: [u8; Self::LEN]) -> Self {
        Self(bytes)
    }

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

    /// This node's signature of `body` as a record of `domain` ([`record_message`]): a book entry
    /// it vouches for, or the proof a membership log's `Node` record carries that this node key
    /// is the member's. No channel proof is such a message, so none can stand for another.
    #[must_use]
    pub fn sign_record(&self, domain: &str, body: &[u8]) -> [u8; SIG_LEN] {
        let signed = self.pair.sign(&record_message(domain, body));
        let mut sig = [0u8; SIG_LEN];
        sig.copy_from_slice(signed.as_ref());
        sig
    }
}

/// What starts every record a node key signs ([`NodeIdentity::sign_record`]). A channel proof
/// starts `aterm-fleet/v2 node`, so the two never meet.
pub const RECORD_LABEL: &[u8] = b"aterm-fleet/v3 record ";

/// The bytes a node key signs for a record of `domain`: [`RECORD_LABEL`], `domain`, a NUL, then
/// `body`.
#[must_use]
pub fn record_message(domain: &str, body: &[u8]) -> Vec<u8> {
    let mut message = RECORD_LABEL.to_vec();
    message.extend_from_slice(domain.as_bytes());
    message.push(0);
    message.extend_from_slice(body);
    message
}

/// Whether `sig` is `key`'s signature of `body` as a record of `domain` ([`record_message`]).
#[must_use]
pub fn verifies_record(key: &NodeKey, domain: &str, body: &[u8], sig: &[u8; SIG_LEN]) -> bool {
    UnparsedPublicKey::new(&ED25519, key.bytes())
        .verify(&record_message(domain, body), sig)
        .is_ok()
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
    /// Peer's node id is on the swarm roster's deny-list (`NO denied`).
    DeniedNode,
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
            Self::DeniedNode => Self::DeniedNode,
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
            Self::DeniedNode => f.write_str(
                "the listener refused this machine's node (deny-listed, or out of its \
                     membership log)",
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
    Arc::clone(CONFIG.get_or_init(|| listener_config(&PROTOCOLS)))
}

/// [`server_config`] that also offers [`ALPN_ASK`], for a listener with an ask handler.
fn server_config_asked() -> Arc<ServerConfig> {
    #[cfg(all(test, unix))]
    test_socket_headroom();
    static CONFIG: OnceLock<Arc<ServerConfig>> = OnceLock::new();
    Arc::clone(CONFIG.get_or_init(|| listener_config(&PROTOCOLS_ASKED)))
}

fn listener_config(protocols: &[&[u8]]) -> Arc<ServerConfig> {
    tls::init_crypto();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KEY_DER));
    let mut cfg = ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_no_client_auth()
        .with_single_cert(vec![CertificateDer::from(CERT_DER)], key)
        .expect("the embedded fleet certificate and key are one Ed25519 pair");
    cfg.alpn_protocols = protocols.iter().map(|alpn| alpn.to_vec()).collect();
    cfg.send_tls13_tickets = 0;
    cfg.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});
    cfg.max_early_data_size = 0;
    Arc::new(cfg)
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
    Arc::clone(CONFIG.get_or_init(|| dialer_config(&PROTOCOLS)))
}

/// The dialer [`ClientConfig`] of a Mac asking to join: it offers [`ALPN_ASK`] alone, so it never
/// negotiates a fleet protocol, and it holds no fleet secret to prove one with.
fn ask_client_config() -> Arc<ClientConfig> {
    #[cfg(all(test, unix))]
    test_socket_headroom();
    static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    Arc::clone(CONFIG.get_or_init(|| dialer_config(&[ALPN_ASK])))
}

fn dialer_config(protocols: &[&[u8]]) -> Arc<ClientConfig> {
    tls::init_crypto();
    let verifier = PinnedServerVerifier::new(tls::cert_fingerprint(CERT_DER));
    let mut cfg = ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    cfg.alpn_protocols = protocols.iter().map(|alpn| alpn.to_vec()).collect();
    cfg.resumption = rustls::client::Resumption::disabled();
    cfg.enable_early_data = false;
    Arc::new(cfg)
}

fn database_server_config() -> Arc<ServerConfig> {
    let base = server_config();
    static DATABASE_SERVER_TLS: OnceLock<Arc<ServerConfig>> = OnceLock::new();
    Arc::clone(DATABASE_SERVER_TLS.get_or_init(|| {
        let mut config = (*base).clone();
        config
            .alpn_protocols
            .insert(0, ALPN_DATABASE_REPAIR.to_vec());
        Arc::new(config)
    }))
}

fn database_client_config() -> Arc<ClientConfig> {
    let base = client_config();
    static DATABASE_CLIENT_TLS: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    Arc::clone(DATABASE_CLIENT_TLS.get_or_init(|| {
        let mut config = (*base).clone();
        config
            .alpn_protocols
            .insert(0, ALPN_DATABASE_REPAIR.to_vec());
        Arc::new(config)
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
    /// Node proof presented by the dialer under a node-proof protocol, if any.
    pub peer: Option<NodeProof>,
}

/// Whether `node` matches any entry in `denied_nodes` (ASCII case-insensitive, trimmed).
#[must_use]
pub fn is_denied_node(node: &str, denied_nodes: &[String]) -> bool {
    let node = node.trim();
    !node.is_empty()
        && denied_nodes
            .iter()
            .any(|denied| denied.trim().eq_ignore_ascii_case(node))
}

/// Whether `key`'s 64-hex Ed25519 encoding matches any entry in `denied_nodes`.
#[must_use]
pub fn is_denied_key(key: &NodeKey, denied_nodes: &[String]) -> bool {
    is_denied_node(&key.hex(), denied_nodes)
}

/// Whom a listener refuses before any op (`NO denied`): node ids on the swarm roster's deny-list,
/// and the node keys this Mac's membership log took out (`docs/DESIGN-fluid-swarm-2026-10-28.md`
/// §3.3.7, F17: "the node key in the removed Mac's `Admit` is refused by every listener at once").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Denied {
    /// Node ids, as [`is_denied_node`] matches them.
    pub nodes: Vec<String>,
    /// Node keys.
    pub keys: Vec<NodeKey>,
}

impl Denied {
    /// The roster's node ids alone.
    #[must_use]
    pub fn nodes(nodes: &[String]) -> Self {
        Self {
            nodes: nodes.to_vec(),
            keys: Vec::new(),
        }
    }

    /// Whether it refuses no one.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty() && self.keys.is_empty()
    }

    /// Whether it refuses a dialer that proved `peer`: while it names anyone, both a named node
    /// id or key and an unproved peer (`None`) are refused, so a removed machine cannot pass by
    /// stripping its node key.
    #[must_use]
    pub fn refuses(&self, peer: Option<&NodeProof>) -> bool {
        !self.is_empty()
            && peer.is_none_or(|proof| {
                is_denied_node(&proof.node, &self.nodes)
                    || is_denied_key(&proof.key, &self.nodes)
                    || self.keys.contains(&proof.key)
            })
    }
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
    server_handshake_with_denied(tcp, secret, me, scope, serves, &Denied::default(), within)
}

/// [`server_handshake_as`], refusing any dialer `denied` refuses ([`Denied::refuses`]) with
/// [`FleetError::DeniedNode`] (`NO denied`).
/// # Errors
/// As [`server_handshake_as`], plus [`FleetError::DeniedNode`].
pub fn server_handshake_with_denied(
    tcp: TcpStream,
    secret: &FleetSecret,
    me: Option<&NodeIdentity>,
    scope: FleetScope,
    serves: &[FleetOp],
    denied: &Denied,
    within: Duration,
) -> Result<Accepted, FleetError> {
    let watchdog = arm(&tcp, within)?;
    server_handshake_armed(
        tcp,
        watchdog,
        within,
        (secret, me),
        scope,
        serves,
        denied,
        |_| true,
        server_config(),
    )
}

/// Authenticate a database-only listener, preferring [`ALPN_DATABASE_REPAIR`]
/// and retaining the existing fleet protocols for older dialers. This serves
/// only [`FleetOp::Database`], with no terminal survey or drive capability.
///
/// # Errors
/// As [`server_handshake_as`].
pub fn server_handshake_database_as(
    tcp: TcpStream,
    secret: &FleetSecret,
    me: Option<&NodeIdentity>,
    within: Duration,
) -> Result<Accepted, FleetError> {
    let watchdog = arm(&tcp, within)?;
    server_handshake_armed(
        tcp,
        watchdog,
        within,
        (secret, me),
        FleetScope::Facts,
        &[FleetOp::Database],
        &Denied::default(),
        |_| true,
        database_server_config(),
    )
}

#[allow(clippy::too_many_arguments)]
fn server_handshake_armed(
    tcp: TcpStream,
    watchdog: HandshakeWatchdog,
    within: Duration,
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
    scope: FleetScope,
    serves: &[FleetOp],
    denied: &Denied,
    admit: impl FnOnce(FleetOp) -> bool,
    config: Arc<ServerConfig>,
) -> Result<Accepted, FleetError> {
    match server_handshake_or_ask(
        tcp,
        watchdog,
        within,
        (secret, me),
        (scope, serves, denied),
        admit,
        config,
        None,
    )? {
        Handshook::Proved(accepted) => Ok(*accepted),
        Handshook::Asked => Err(FleetError::Alpn),
    }
}

/// What an accepted connection became: a dialer that proved the fleet secret, or a caller with
/// none whose [`ALPN_ASK`] stream the ask handler served to its end.
enum Handshook {
    Proved(Box<Accepted>),
    Asked,
}

/// The listener's ask handler: it gets the [`ALPN_ASK`] stream of a caller that proved nothing,
/// and the address it dialled from.
pub type AskHandler = Arc<dyn Fn(&mut dyn Duplex, SocketAddr) -> io::Result<()> + Send + Sync>;

/// A byte stream both ways: what an ask handler reads its caller's requests from and answers on.
pub trait Duplex: Read + Write {}

impl<T: Read + Write + ?Sized> Duplex for T {}

/// [`server_handshake_armed`], serving an [`ALPN_ASK`] caller with `ask` when it is given: such a
/// caller is never asked for a proof, never reaches `admit`, and never gets a fleet op. Its whole
/// exchange runs inside the handshake's watchdog, so it holds its slot no longer than a fleet
/// handshake may, and a listener given no ask handler does not offer the protocol at all.
#[expect(
    clippy::too_many_arguments,
    reason = "handshake binds transport, watchdog, deadline, credentials, scope, admission, TLS config and optional ask handler"
)]
fn server_handshake_or_ask(
    tcp: TcpStream,
    watchdog: HandshakeWatchdog,
    within: Duration,
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
    (scope, serves, denied): (FleetScope, &[FleetOp], &Denied),
    admit: impl FnOnce(FleetOp) -> bool,
    config: Arc<ServerConfig>,
    ask: Option<(&AskHandler, SocketAddr)>,
) -> Result<Handshook, FleetError> {
    let (mut transport, proved) = bounded(tcp, watchdog, within, |tcp| {
        let (mut conn, exporter, spoken) = accept_tls_on(tcp, config)?;
        let nodes = match spoken {
            Spoken::Fleet { nodes } => nodes,
            Spoken::Ask => {
                let Some((ask, peer)) = ask else {
                    return Err(FleetError::Alpn);
                };
                let mut stream = rustls::Stream::new(&mut conn, tcp);
                ask(&mut stream, peer).map_err(FleetError::Io)?;
                stream.flush().map_err(FleetError::Io)?;
                return Ok((conn, exporter, None));
            }
        };
        let me = me.filter(|_| nodes);
        let proved = listener_proves(
            &mut rustls::Stream::new(&mut conn, tcp),
            &exporter,
            (secret, me, nodes),
            scope,
            serves,
            denied,
            admit,
        )?;
        Ok((conn, exporter, Some(proved)))
    })?;
    let Some((op, peer)) = proved else {
        end_asked(transport);
        return Ok(Handshook::Asked);
    };
    release_deadlines(transport.stream().get_ref())?;
    Ok(Handshook::Proved(Box::new(Accepted {
        transport,
        op,
        peer,
    })))
}

/// Close an answered ask connection: a TLS close, then the socket, waiting for neither.
fn end_asked(mut transport: TlsTransport<ServerConnection>) {
    let stream = transport.stream();
    stream.conn.send_close_notify();
    let _ = stream.conn.complete_io(&mut stream.sock);
    let _ = stream.sock.shutdown(std::net::Shutdown::Both);
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
    /// Node proof presented by the listener under a node-proof protocol, if any.
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

/// Authenticate a database dialer, preferring [`ALPN_DATABASE_REPAIR`] and
/// negotiating an existing fleet protocol with an older listener. TLS pins,
/// secret proofs and exporter-bound node proofs are unchanged.
///
/// # Errors
/// As [`client_handshake_as`].
pub fn client_handshake_database_as(
    tcp: TcpStream,
    secret: &FleetSecret,
    me: Option<&NodeIdentity>,
    within: Duration,
) -> Result<Dialed, FleetError> {
    client_handshake_by_on(
        tcp,
        secret,
        me,
        FleetOp::Database,
        within,
        database_client_config(),
    )
    .map_err(|undialed| undialed.error)
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
    client_handshake_by_on(tcp, secret, me, op, within, client_config())
}

fn client_handshake_by_on(
    tcp: TcpStream,
    secret: &FleetSecret,
    me: Option<&NodeIdentity>,
    op: FleetOp,
    within: Duration,
    config: Arc<ClientConfig>,
) -> Result<Dialed, Undialed> {
    let mut refused_by = None;
    let undialed = |error: FleetError, refused_by: Option<NodeProof>| Undialed {
        refused_by: refused_by.filter(|_| {
            matches!(
                error,
                FleetError::UnknownOp(_) | FleetError::AddressBusy | FleetError::DeniedNode
            )
        }),
        error,
    };
    let watchdog = arm(&tcp, within).map_err(|e| undialed(e, None))?;
    let (mut transport, (scope, peer)) = bounded(tcp, watchdog, within, |tcp| {
        let (mut conn, exporter, spoken) = connect_tls_on(tcp, config)?;
        let Spoken::Fleet { nodes } = spoken else {
            return Err(FleetError::Alpn);
        };
        let me = me.filter(|_| nodes);
        let proved = dialer_proves(
            &mut rustls::Stream::new(&mut conn, tcp),
            &exporter,
            (secret, me, nodes),
            op,
            &mut refused_by,
        )?;
        Ok((conn, exporter, proved))
    })
    .map_err(|e| undialed(e, refused_by.take()))?;
    release_deadlines(transport.stream().get_ref()).map_err(|e| undialed(e, None))?;
    Ok(Dialed {
        transport,
        scope,
        peer,
    })
}

/// Open an [`ALPN_ASK`] connection on `tcp` within `within`: TLS alone, pinned to [`CERT_DER`],
/// with no fleet secret and no proof, so the listener hands it to its ask handler and nothing else.
/// The socket's deadlines are cleared; the caller bounds each exchange.
/// # Errors
/// [`FleetError::Alpn`] when the listener serves no ask (a listener without an ask handler, or one
/// older than this build), or a TLS, I/O or deadline failure.
pub fn ask_handshake(
    tcp: TcpStream,
    within: Duration,
) -> Result<TlsTransport<ClientConnection>, FleetError> {
    let watchdog = arm(&tcp, within)?;
    let (transport, ()) = bounded(tcp, watchdog, within, |tcp| {
        let (conn, exporter, spoken) = connect_tls_on(tcp, ask_client_config())?;
        if spoken != Spoken::Ask {
            return Err(FleetError::Alpn);
        }
        Ok((conn, exporter, ()))
    })?;
    release_deadlines(transport.tcp())?;
    Ok(transport)
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
        tcp: &TcpStream,
        locality: Locality,
        #[cfg(unix)] witness: Option<crate::lan::ListenerWitness>,
    ) -> io::Result<Self> {
        match locality {
            Locality::Fleet => {
                let _ = tcp;
                Ok(Self::default())
            }
            #[cfg(unix)]
            Locality::Link(scope) => {
                let proof = crate::lan::Connection::admit_proof(tcp, scope, witness)?;
                Ok(Self { proof: Some(proof) })
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
    let empty = Arc::new(Denied::default());
    serve_inner(
        ListenerSocket::Fleet(listener),
        (listening, &|| (listening.scope, listening.serves), &|| {
            Arc::clone(&empty)
        }),
        None,
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
    let empty = Arc::new(Denied::default());
    serve_inner(
        ListenerSocket::Fleet(listener),
        (listening, offer, &|| Arc::clone(&empty)),
        None,
        running,
        Locality::Fleet,
        HANDSHAKE_WITHIN,
        hand_over,
        on_event,
    );
}

/// [`serve_offering`], also refusing any dialer `deny()` refuses ([`Denied::refuses`]) with
/// [`FleetError::DeniedNode`] (`NO denied`).
pub fn serve_offering_with_denied<H, E>(
    listener: &TcpListener,
    listening: &Listening,
    offer: &dyn Fn() -> (FleetScope, &'static [FleetOp]),
    deny: &dyn Fn() -> Arc<Denied>,
    running: &AtomicBool,
    hand_over: H,
    on_event: E,
) where
    H: Fn(Served, FleetOp) -> io::Result<()> + Send + Sync + Clone + 'static,
    E: Fn(FleetEvent) + Send + Sync + Clone + 'static,
{
    serve_inner(
        ListenerSocket::Fleet(listener),
        (listening, offer, deny),
        None,
        running,
        Locality::Fleet,
        HANDSHAKE_WITHIN,
        hand_over,
        on_event,
    );
}

/// [`serve_offering_with_denied`], also serving a caller with no fleet secret that speaks
/// [`ALPN_ASK`] when `ask` is given: its stream goes to `ask` alone, inside the handshake's deadline, and never to
/// `hand_over`, so such a caller reaches no fleet op (`docs/DESIGN-fluid-swarm-2026-10-28.md` F10).
#[allow(clippy::too_many_arguments)]
pub fn serve_offering_with_ask<H, E>(
    listener: &TcpListener,
    listening: &Listening,
    offer: &dyn Fn() -> (FleetScope, &'static [FleetOp]),
    deny: &dyn Fn() -> Arc<Denied>,
    ask: Option<AskHandler>,
    running: &AtomicBool,
    hand_over: H,
    on_event: E,
) where
    H: Fn(Served, FleetOp) -> io::Result<()> + Send + Sync + Clone + 'static,
    E: Fn(FleetEvent) + Send + Sync + Clone + 'static,
{
    serve_offering_with_ask_within(
        listener,
        (listening, offer, deny),
        ask,
        running,
        HANDSHAKE_WITHIN,
        hand_over,
        on_event,
    );
}

/// [`serve_offering_with_ask`], each connection's TLS and both proofs given `handshake_within`
/// in place of [`HANDSHAKE_WITHIN`]: a test world whose every dialer is an aterm on the same
/// machine, and so must finish each handshake however loaded it is, gives it its hang detector.
pub fn serve_offering_with_ask_within<H, E>(
    listener: &TcpListener,
    (listening, offer, deny): (&Listening, OfferFn<'_>, DenyFn<'_>),
    ask: Option<AskHandler>,
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
        (listening, offer, deny),
        ask,
        running,
        Locality::Fleet,
        handshake_within,
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
    let empty = Arc::new(Denied::default());
    serve_inner(
        ListenerSocket::Fleet(listener),
        (listening, &|| (listening.scope, listening.serves), &|| {
            Arc::clone(&empty)
        }),
        None,
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
    let empty = Arc::new(Denied::default());
    serve_inner(
        ListenerSocket::Link(listener),
        (listening, &|| (listening.scope, listening.serves), &|| {
            Arc::clone(&empty)
        }),
        None,
        running,
        locality,
        HANDSHAKE_WITHIN,
        hand_over,
        on_event,
    );
}

type OfferFn<'a> = &'a dyn Fn() -> (FleetScope, &'static [FleetOp]);
type DenyFn<'a> = &'a dyn Fn() -> Arc<Denied>;

#[allow(clippy::too_many_arguments)]
fn serve_inner<H, E>(
    listener: ListenerSocket<'_>,
    (listening, offer, deny): (&Listening, OfferFn<'_>, DenyFn<'_>),
    ask: Option<AskHandler>,
    running: &AtomicBool,
    locality: Locality,
    handshake_within: Duration,
    hand_over: H,
    on_event: E,
) where
    H: Fn(Served, FleetOp) -> io::Result<()> + Send + Sync + Clone + 'static,
    E: Fn(FleetEvent) + Send + Sync + Clone + 'static,
{
    let _ = aterm_uds::keep_out_of_children(listener.tcp());
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
        let armed = tcp.try_clone().map_err(FleetError::Io).and_then(|line| {
            let _ = aterm_uds::keep_out_of_children(&line);
            Ok((line, arm(&tcp, handshake_within)?))
        });
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
        let denied = deny();
        let hand_over = hand_over.clone();
        let ask = ask.clone();
        let tell = on_event.clone();
        #[cfg(unix)]
        let witness = listener.witness();
        let mut armed_conn = (Some(watchdog), Some(tcp), release);
        let spawned = std::thread::Builder::new()
            .name("aterm-fleet-conn".to_owned())
            .spawn(move || {
                let Some(tcp) = armed_conn.1.take() else {
                    return;
                };
                let Some(watchdog) = armed_conn.0.take() else {
                    return;
                };
                let release = armed_conn.2;
                let handshake: Result<_, FleetError> = (|| {
                    let admission = match Admission::inspect(
                        &tcp,
                        locality,
                        #[cfg(unix)]
                        witness,
                    ) {
                        Ok(admission) => admission,
                        Err(error) => {
                            // The native refusal still owns the armed phase.
                            // Retire its thread/socket before freeing the slot.
                            let _ = watchdog.finish();
                            return Err(FleetError::Io(error));
                        }
                    };
                    let accepted = server_handshake_or_ask(
                        tcp,
                        watchdog,
                        handshake_within,
                        (&secret, node.as_ref()),
                        (scope, serves, &denied),
                        |op| {
                            let drive = op == FleetOp::Drive;
                            let share = if drive {
                                drive_per_source(most)
                            } else {
                                per_source
                            };
                            release.tenure.proved(drive, share)
                        },
                        if ask.is_some() {
                            server_config_asked()
                        } else {
                            server_config()
                        },
                        ask.as_ref().map(|ask| (ask, peer)),
                    )?;
                    admission.recheck().map_err(FleetError::Io)?;
                    Ok((accepted, admission))
                })();
                let event = match handshake {
                    Ok((Handshook::Asked, _)) => {
                        drop(release);
                        FleetEvent::Closed { peer, error: None }
                    }
                    Ok((Handshook::Proved(accepted), admission)) => {
                        let Accepted {
                            transport,
                            op,
                            peer: proof,
                        } = *accepted;
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

fn bounded<C, S, T>(
    mut tcp: TcpStream,
    watchdog: HandshakeWatchdog,
    within: Duration,
    handshake: impl FnOnce(&mut TcpStream) -> Result<(C, [u8; EXPORTER_LEN], T), FleetError>,
) -> Result<(TlsTransport<C>, T), FleetError>
where
    C: std::ops::DerefMut + std::ops::Deref<Target = rustls::ConnectionCommon<S>>,
    S: rustls::SideData,
{
    let result = handshake(&mut tcp);
    match (watchdog.finish(), result) {
        (Claim::Authed, Err(e)) if e.is_deadline() => Err(FleetError::Timeout(within)),
        (Claim::Authed, Ok((conn, exporter, out))) => Ok((
            TlsTransport::from_parts(StreamOwned::new(conn, tcp), exporter),
            out,
        )),
        (Claim::Authed, Err(e)) => Err(e),
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

#[cfg(test)]
fn accept_tls(
    mut tcp: TcpStream,
    config: Arc<ServerConfig>,
) -> Result<(TlsTransport<ServerConnection>, bool), FleetError> {
    let (conn, exporter, spoken) = accept_tls_on(&mut tcp, config)?;
    Ok((
        TlsTransport::from_parts(StreamOwned::new(conn, tcp), exporter),
        spoken == Spoken::Fleet { nodes: true },
    ))
}

fn accept_tls_on(
    tcp: &mut TcpStream,
    config: Arc<ServerConfig>,
) -> Result<(ServerConnection, [u8; EXPORTER_LEN], Spoken), FleetError> {
    let mut conn =
        ServerConnection::new(config).map_err(|e| FleetError::Tls(io::Error::other(e)))?;
    while conn.is_handshaking() {
        if let Err(e) = conn.complete_io(tcp) {
            return Err(
                if matches!(rustls_error(&e), Some(rustls::Error::NoApplicationProtocol)) {
                    FleetError::Alpn
                } else {
                    FleetError::Tls(tls::listen_handshake_error(e))
                },
            );
        }
    }
    let Some(spoken) = spoken(conn.alpn_protocol()) else {
        return Err(FleetError::Alpn);
    };
    let exporter = export(&conn)?;
    Ok((conn, exporter, spoken))
}

/// What a connection's two ends agreed to speak.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Spoken {
    /// The fleet handshake, with node proofs under [`ALPN_NODES`] or [`ALPN_DATABASE_REPAIR`].
    Fleet { nodes: bool },
    /// A Mac asking to join, under [`ALPN_ASK`]: no secret, no proof, no op.
    Ask,
}

const fn spoken(alpn: Option<&[u8]>) -> Option<Spoken> {
    match alpn {
        Some(ALPN_NODES | ALPN_DATABASE_REPAIR) => Some(Spoken::Fleet { nodes: true }),
        Some(ALPN) => Some(Spoken::Fleet { nodes: false }),
        Some(ALPN_ASK) => Some(Spoken::Ask),
        _ => None,
    }
}

#[cfg(test)]
fn connect_tls(
    mut tcp: TcpStream,
    config: Arc<ClientConfig>,
) -> Result<(TlsTransport<ClientConnection>, bool), FleetError> {
    let (conn, exporter, spoken) = connect_tls_on(&mut tcp, config)?;
    Ok((
        TlsTransport::from_parts(StreamOwned::new(conn, tcp), exporter),
        spoken == Spoken::Fleet { nodes: true },
    ))
}

fn connect_tls_on(
    tcp: &mut TcpStream,
    config: Arc<ClientConfig>,
) -> Result<(ClientConnection, [u8; EXPORTER_LEN], Spoken), FleetError> {
    let name = ServerName::try_from(SERVER_NAME).expect("\"aterm-fleet\" is a valid DNS name");
    let mut conn =
        ClientConnection::new(config, name).map_err(|e| FleetError::Tls(io::Error::other(e)))?;
    while conn.is_handshaking() {
        if let Err(e) = conn.complete_io(tcp) {
            let pre_fleet = match rustls_error(&e) {
                Some(rustls::Error::AlertReceived(
                    rustls::AlertDescription::NoApplicationProtocol,
                )) => true,
                Some(rustls::Error::InvalidCertificate(_)) => {
                    spoken(conn.alpn_protocol()).is_none()
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
    let Some(spoken) = spoken(conn.alpn_protocol()) else {
        return Err(FleetError::Alpn);
    };
    let exporter = export(&conn)?;
    Ok((conn, exporter, spoken))
}

fn listener_proves<S: Read + Write>(
    stream: &mut S,
    exporter: &[u8],
    (secret, me, nodes): (&FleetSecret, Option<&NodeIdentity>, bool),
    scope: FleetScope,
    serves: &[FleetOp],
    denied: &Denied,
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
    };
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
    if denied.refuses(peer.as_ref()) {
        return refuse_as(stream, "denied", FleetError::DeniedNode, node);
    }
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
        Some("NO denied") => Some(FleetError::DeniedNode),
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
    fn database_repair_negotiates_its_order_and_falls_back_with_legacy_node_peers() {
        for (new_listener, new_dialer, expected) in [
            (true, true, ALPN_DATABASE_REPAIR),
            (true, false, ALPN_NODES),
            (false, true, ALPN_NODES),
            (false, false, ALPN_NODES),
        ] {
            let (listened, dialed) = over_loopback(
                move |tcp| {
                    let me = node(1);
                    let mut accepted = if new_listener {
                        server_handshake_database_as(tcp, &secret(7), Some(&me), HANG)
                    } else {
                        server_handshake_as(
                            tcp,
                            &secret(7),
                            Some(&me),
                            FleetScope::Facts,
                            &[FleetOp::Database],
                            HANG,
                        )
                    }
                    .unwrap();
                    assert_eq!(accepted.op, FleetOp::Database);
                    assert_eq!(accepted.peer.unwrap().key, node(2).key());
                    let exporter = accepted.transport.exporter().to_vec();
                    let stream = accepted.transport.stream();
                    stream.get_ref().set_read_timeout(Some(HANG)).unwrap();
                    let alpn = stream.conn.alpn_protocol().unwrap().to_vec();
                    let mut request = [0; 5];
                    stream.read_exact(&mut request).unwrap();
                    assert_eq!(&request, b"ping\n");
                    stream.write_all(b"pong\n").unwrap();
                    stream.flush().unwrap();
                    (alpn, exporter)
                },
                move |tcp| {
                    let me = node(2);
                    let mut dialed = if new_dialer {
                        client_handshake_database_as(tcp, &secret(7), Some(&me), HANG)
                    } else {
                        client_handshake_as(tcp, &secret(7), Some(&me), FleetOp::Database, HANG)
                    }
                    .unwrap();
                    assert_eq!(dialed.scope, FleetScope::Facts);
                    assert_eq!(dialed.peer.unwrap().key, node(1).key());
                    let exporter = dialed.transport.exporter().to_vec();
                    let stream = dialed.transport.stream();
                    stream.get_ref().set_read_timeout(Some(HANG)).unwrap();
                    let alpn = stream.conn.alpn_protocol().unwrap().to_vec();
                    stream.write_all(b"ping\n").unwrap();
                    stream.flush().unwrap();
                    let mut answer = [0; 5];
                    stream.read_exact(&mut answer).unwrap();
                    assert_eq!(&answer, b"pong\n");
                    (alpn, exporter)
                },
            );
            assert_eq!(listened.0, expected);
            assert_eq!(dialed.0, expected);
            assert_eq!(listened.1, dialed.1);
        }
    }

    #[test]
    fn a_database_listener_does_not_grant_terminal_drive() {
        let (listened, dialed) = over_loopback(
            |tcp| server_handshake_database_as(tcp, &secret(7), Some(&node(1)), HANG).err(),
            |tcp| client_handshake_as(tcp, &secret(7), Some(&node(2)), FleetOp::Drive, HANG).err(),
        );
        assert!(matches!(listened, Some(FleetError::UnknownOp(op)) if op == "drive"));
        assert!(matches!(dialed, Some(FleetError::UnknownOp(op)) if op == "drive"));
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
                    &Denied::default(),
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
                    &Denied::default(),
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
                    &Denied::default(),
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
                    let _ = server_handshake_armed(
                        tcp,
                        watchdog,
                        HANG,
                        me,
                        scope,
                        READ_ONLY,
                        &Denied::default(),
                        |_| !full,
                        server_config(),
                    );
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

    #[cfg(target_os = "macos")]
    #[test]
    fn a_concurrent_socket_descriptor_scan_during_handshake_never_drains_the_proved_stream() {
        use std::os::fd::AsRawFd as _;
        const PROC_PIDFDSOCKETINFO: libc::c_int = 3;
        const SOCKET_FDINFO_BYTES: libc::c_int = 792;
        unsafe extern "C" {
            fn proc_pidfdinfo(
                pid: libc::c_int,
                fd: libc::c_int,
                flavor: libc::c_int,
                buffer: *mut libc::c_void,
                buffersize: libc::c_int,
            ) -> libc::c_int;
        }

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_fd = listener.as_raw_fd();
        let addr = listener.local_addr().unwrap();
        let _served = Serving::start(listener, listening(8, HANG));

        let stop = Arc::new(AtomicBool::new(false));
        let pid = std::process::id().cast_signed();
        let scanners: Vec<_> = (0..2)
            .map(|_| {
                let stop = Arc::clone(&stop);
                std::thread::spawn(move || {
                    let mut buf = [0u8; 1024];
                    while !stop.load(Ordering::Relaxed) {
                        for fd in base_fd..base_fd.saturating_add(16) {
                            // SAFETY: `buf` holds at least `SOCKET_FDINFO_BYTES` writable bytes.
                            unsafe {
                                let _ = proc_pidfdinfo(
                                    pid,
                                    fd,
                                    PROC_PIDFDSOCKETINFO,
                                    buf.as_mut_ptr().cast(),
                                    SOCKET_FDINFO_BYTES,
                                );
                            }
                        }
                    }
                })
            })
            .collect();

        for round in 0..40 {
            let tcp = TcpStream::connect_timeout(&addr, HANG).unwrap();
            let (mut conn, _) = client_handshake(tcp, &secret(7), FleetOp::Survey, HANG)
                .unwrap_or_else(|e| panic!("round {round} handshake: {e}"));
            assert!(
                echoes(&mut conn),
                "round {round}: the proved stream must not be drained by a concurrent socket scan"
            );
        }

        // NEGATIVE CONTROL: duplicating a live socket (`try_clone`) and closing the duplicate
        // while `PROC_PIDFDSOCKETINFO` inspects it drains the shared socket (`SS_DRAINING`), so a
        // blocking read on the still-open original descriptor fails at once with `EBADF`.
        let mut control_drained = false;
        let started = std::time::Instant::now();
        while !control_drained && started.elapsed() < HANG {
            let pair_listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let pair_addr = pair_listener.local_addr().unwrap();
            let reader = TcpStream::connect_timeout(&pair_addr, HANG).unwrap();
            let (_writer, _) = pair_listener.accept().unwrap();
            reader
                .set_read_timeout(Some(Duration::from_millis(2)))
                .unwrap();
            for _ in 0..8 {
                let dup = reader.try_clone().unwrap();
                drop(dup);
            }
            let mut byte = [0u8; 1];
            if (&reader)
                .read(&mut byte)
                .is_err_and(|e| e.raw_os_error() == Some(libc::EBADF))
            {
                control_drained = true;
            }
        }

        stop.store(true, Ordering::Relaxed);
        for scanner in scanners {
            scanner.join().unwrap();
        }
        assert!(
            control_drained,
            "CONTROL: closing a try_clone duplicate under PROC_PIDFDSOCKETINFO drains the socket with EBADF"
        );
    }

    #[cfg(unix)]
    #[test]
    fn admission_refusal_keeps_accepted_fd_open_until_watchdog_joins() {
        use std::os::fd::AsRawFd as _;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let dialer = TcpStream::connect_timeout(&addr, HANG).unwrap();
        let (accepted, _) = listener.accept().unwrap();
        let accepted_fd = accepted.as_raw_fd();
        let watchdog = arm(&accepted, HANG).unwrap();
        let evictor = watchdog.evictor();

        let refused = Admission::inspect(&accepted, Locality::Link(crate::lan::Scope::Lan), None);
        assert!(
            refused.is_err(),
            "loopback socket must be refused under Scope::Lan"
        );

        let mut bystander_client = TcpStream::connect_timeout(&addr, HANG).unwrap();
        let (mut bystander_server, _) = listener.accept().unwrap();
        bystander_server.set_read_timeout(Some(HANG)).unwrap();
        assert_ne!(
            bystander_client.as_raw_fd(),
            accepted_fd,
            "accepted fd must remain open while watchdog is still armed"
        );
        assert_ne!(
            bystander_server.as_raw_fd(),
            accepted_fd,
            "accepted fd must remain open while watchdog is still armed"
        );

        assert!(evictor.evict());
        assert_eq!(watchdog.finish(), Claim::Evicted);
        drop((accepted, dialer));

        bystander_client.write_all(b"ok").unwrap();
        let mut buf = [0u8; 2];
        bystander_server.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"ok");

        // NEGATIVE CONTROL: if `accepted` is closed before `watchdog.finish()` and its descriptor
        // number (`victim_fd`) is reused by a bystander socket, `evictor.evict()` shuts down the
        // bystander socket. `dup2` replaces `victim_fd` atomically while `accepted` still owns it
        // so parallel test threads cannot claim `victim_fd` in between.
        let dialer = TcpStream::connect_timeout(&addr, HANG).unwrap();
        let (accepted, _) = listener.accept().unwrap();
        let victim_fd = accepted.as_raw_fd();
        let watchdog = arm(&accepted, HANG).unwrap();
        let evictor = watchdog.evictor();
        let mut bystander_client = TcpStream::connect_timeout(&addr, HANG).unwrap();
        let (bystander_server_orig, _) = listener.accept().unwrap();
        bystander_server_orig.set_read_timeout(Some(HANG)).unwrap();
        // SAFETY: both `bystander_server_orig` and `accepted` are open descriptors owned by this
        // test; `dup2` atomically closes `accepted`'s descriptor (`victim_fd`) and duplicates
        // `bystander_server_orig` onto `victim_fd` with no unallocated gap for sibling threads.
        let duped = unsafe { libc::dup2(bystander_server_orig.as_raw_fd(), victim_fd) };
        assert_eq!(duped, victim_fd, "dup2 onto victim_fd must succeed");
        std::mem::forget(accepted);
        drop(bystander_server_orig);
        // SAFETY: `victim_fd` now uniquely owns the duplicated `bystander_server` socket.
        let mut bystander_server: TcpStream =
            unsafe { std::os::fd::FromRawFd::from_raw_fd(victim_fd) };
        let _ = aterm_uds::keep_out_of_children(&bystander_server);
        assert!(evictor.evict());
        assert_eq!(watchdog.finish(), Claim::Evicted);
        drop(dialer);
        let cut = bystander_client.write_all(b"ok").is_err()
            || bystander_server.read_exact(&mut buf).is_err();
        assert!(
            cut,
            "CONTROL: closing accepted before watchdog.finish() shuts down the reused bystander fd"
        );
    }

    /// A listener serving ask on its own: every op it offers, an ask handler that records the one
    /// line it reads and answers `OK asked`, and a hand-over that records each op it is given.
    struct Asked {
        addr: SocketAddr,
        ops: Arc<Mutex<Vec<FleetOp>>>,
        lines: Arc<Mutex<Vec<String>>>,
        running: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Asked {
        fn start(serves_ask: bool) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let ops = Arc::new(Mutex::new(Vec::new()));
            let lines = Arc::new(Mutex::new(Vec::new()));
            let running = Arc::new(AtomicBool::new(true));
            let ask: AskHandler = Arc::new({
                let lines = Arc::clone(&lines);
                move |mut stream: &mut dyn Duplex, _peer| {
                    let line = read_line(&mut stream)
                        .map_err(|error| io::Error::other(error.to_string()))?
                        .unwrap_or_default();
                    lines.lock().unwrap().push(line);
                    stream.write_all(b"OK asked\n")
                }
            });
            let thread = std::thread::spawn({
                let (ops, running) = (Arc::clone(&ops), Arc::clone(&running));
                move || {
                    let listening = Listening {
                        secret: secret(7),
                        scope: FleetScope::Content,
                        node: None,
                        serves: &[
                            FleetOp::Survey,
                            FleetOp::Book,
                            FleetOp::Join,
                            FleetOp::Drive,
                            FleetOp::Facts,
                            FleetOp::Census,
                        ],
                        slots: 8,
                        per_source: 8,
                        lifetime: HANG,
                    };
                    let empty = Arc::new(Denied::default());
                    serve_inner(
                        ListenerSocket::Fleet(&listener),
                        (&listening, &|| (listening.scope, listening.serves), &|| {
                            Arc::clone(&empty)
                        }),
                        serves_ask.then_some(ask),
                        &running,
                        Locality::Fleet,
                        HANG,
                        move |served: Served, op| {
                            ops.lock().unwrap().push(op);
                            served.answer(|stream| stream.write_all(b"OK served\n"))
                        },
                        |_| {},
                    );
                }
            });
            Self {
                addr,
                ops,
                lines,
                running,
                thread: Some(thread),
            }
        }

        fn dial(&self) -> TcpStream {
            let tcp = TcpStream::connect_timeout(&self.addr, HANG).unwrap();
            tcp.set_read_timeout(Some(HANG)).unwrap();
            tcp
        }
    }

    impl Drop for Asked {
        fn drop(&mut self) {
            self.running.store(false, Ordering::Release);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// F10 (`docs/DESIGN-fluid-swarm-2026-10-28.md`): a caller without the fleet secret reaches the
    /// ask handler and nothing else. Whatever it writes on [`ALPN_ASK`] (a survey, a drive, a join,
    /// the fleet's own proof line) goes to the ask handler, and no op is handed over; on the fleet
    /// protocol a wrong secret is refused before any op; and a listener with no ask handler does
    /// not speak ask at all.
    #[test]
    fn a_caller_without_the_fleet_secret_reaches_only_ask() {
        let listening = Asked::start(true);
        let tries = [
            "survey".to_owned(),
            "version".to_owned(),
            format!("drive {}", "0".repeat(64)),
            format!("join {}", "0".repeat(64)),
            format!("survey {}", "f".repeat(64)),
        ];
        for line in &tries {
            let mut asked = ask_handshake(listening.dial(), HANG).expect("an ask is served");
            asked
                .stream()
                .write_all(format!("{line}\n").as_bytes())
                .unwrap();
            asked.stream().flush().unwrap();
            let reply = read_line(asked.stream()).unwrap();
            assert_eq!(reply.as_deref(), Some("OK asked"), "{line}");
        }
        assert_eq!(*listening.lines.lock().unwrap(), tries);
        assert!(
            listening.ops.lock().unwrap().is_empty(),
            "an ask reached a fleet op: {:?}",
            listening.ops.lock().unwrap()
        );
        // CONTROL: the fleet secret reaches the hand-over, so the empty record above means something.
        let (mut proved, _) = client_handshake(listening.dial(), &secret(7), FleetOp::Survey, HANG)
            .expect("the secret proves survey");
        assert_eq!(
            read_line(proved.stream()).unwrap().as_deref(),
            Some("OK served")
        );
        assert_eq!(*listening.ops.lock().unwrap(), [FleetOp::Survey]);
        // A listener given no ask handler never negotiates ask.
        let closed = Asked::start(false);
        let refused = ask_handshake(closed.dial(), HANG);
        assert!(
            matches!(refused, Err(FleetError::Alpn)),
            "{:?}",
            refused.err()
        );
        assert!(closed.lines.lock().unwrap().is_empty() && closed.ops.lock().unwrap().is_empty());
    }

    /// A survey or a drive from a caller that does not hold the fleet secret is refused in the
    /// handshake on a listener that also serves ask: the ask path is no way around the proof.
    #[test]
    fn a_survey_or_drive_without_the_fleet_secret_is_refused() {
        let listening = Asked::start(true);
        for op in [FleetOp::Survey, FleetOp::Drive] {
            let wrong = client_handshake(listening.dial(), &secret(9), op, HANG);
            assert!(
                matches!(wrong, Err(FleetError::ListenerWrongSecret)),
                "{op:?}: {:?}",
                wrong.err()
            );
        }
        // A dialer with no secret that ignores the listener's proof and presents its own guess.
        for word in ["survey", "drive"] {
            let (mut raw, _) = connect_tls(listening.dial(), client_config()).expect("tls");
            read_line(raw.stream())
                .unwrap()
                .expect("the listener's proof");
            raw.stream()
                .write_all(format!("{word} {}\n", "0".repeat(64)).as_bytes())
                .unwrap();
            raw.stream().flush().unwrap();
            let reply = read_line(raw.stream()).ok().flatten().unwrap_or_default();
            assert_eq!(reply, "NO proof", "{word}");
        }
        assert!(
            listening.ops.lock().unwrap().is_empty(),
            "an op ran without the secret: {:?}",
            listening.ops.lock().unwrap()
        );
        // CONTROL: the fleet secret reaches each op on the same listener.
        for op in [FleetOp::Survey, FleetOp::Drive] {
            let (mut proved, _) = client_handshake(listening.dial(), &secret(7), op, HANG)
                .expect("the secret proves the op");
            assert_eq!(
                read_line(proved.stream()).unwrap().as_deref(),
                Some("OK served")
            );
        }
        assert_eq!(
            *listening.ops.lock().unwrap(),
            [FleetOp::Survey, FleetOp::Drive]
        );
    }
}
