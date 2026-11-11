// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Mutual exporter-bound handshakes over TLS 1.3 for fleet TCP connections: the fleet's
//! (`atf/2`, `atf/1`), which proves one shared secret by HMAC and, under `atf/2`, each end's node
//! key; and the mesh's (`atm/1`, `docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.4), which proves no
//! shared secret: the listener proves its node key first, the dialer judges that key by its own
//! membership log before it proves its own, and the listener judges the dialer's the same way.

use std::fmt;
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
#[cfg(unix)]
use std::sync::Weak;
#[cfg(all(test, target_os = "macos"))]
use std::sync::atomic::AtomicBool;
#[cfg(test)]
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use aterm_codec::hex;
use aterm_digest::{hkdf_expand_32, hkdf_extract, hmac_sha256_parts};
use aterm_uds::stop::Stop;
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use rustls::{ClientConfig, ClientConnection, ServerConfig, ServerConnection, StreamOwned};

use crate::slots::{
    ACCEPT_POLL, Claim, DISPLACED, HandshakeWatchdog, Slots, arm_unauthenticated, source_of,
    wait_listener_readable,
};
use crate::tls::{self, EXPORTER_LEN, PinnedServerVerifier, TlsTransport};

#[cfg(unix)]
mod serve_wake;
#[cfg(unix)]
use serve_wake::{Signal, Wake};

/// Base ALPN protocol (`atf/1`) for fleet connections without node proofs.
pub const ALPN: &[u8] = b"atf/1";

/// Preferred ALPN protocol (`atf/2`) where both ends exchange Ed25519 node proofs.
pub const ALPN_NODES: &[u8] = b"atf/2";

/// Database repair with signed membership records before application access.
/// The fleet/node proofs are unchanged; both ends must select this protocol
/// before using its repair order. Ordinary fleet connections do not offer it.
pub const ALPN_DATABASE_REPAIR: &[u8] = b"atdb/2";

/// Database repair followed by an optional, connection-bound credential handoff.
/// Older database peers still select [`ALPN_DATABASE_REPAIR`]; ordinary fleet
/// endpoints offer none of the database protocols.
pub const ALPN_DATABASE_CREDENTIALS: &[u8] = b"atdb/3";

/// Database repair with fixed metadata, bounded authority evidence and causal frontiers, each
/// side sending the whole seat witness its bootstrap selected, as 0.153 released it. Newer
/// endpoints prefer [`ALPN_DATABASE_DELTA`] and keep this one, frame for frame, for 0.153 peers.
/// This remains a database-only protocol with exporter-bound node proofs.
pub const ALPN_DATABASE_FRONTIER: &[u8] = b"atdb/4";

/// Database repair over causal frontiers where each side states what it holds before any
/// authority evidence, so each pushes only the seat evidence the other lacks, and membership
/// rounds move bounded batches. Older database peers still select [`ALPN_DATABASE_FRONTIER`] or an
/// earlier protocol. This remains a database-only protocol with exporter-bound node proofs.
pub const ALPN_DATABASE_DELTA: &[u8] = b"atdb/5";

const PROTOCOLS: [&[u8]; 2] = [ALPN_NODES, ALPN];

/// The mesh's ALPN protocol (`atm/1`, `docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.4): the
/// listener proves its node key first, over [`MESH_EXPORTER_LABEL`]'s exporter and bound to its
/// role; the dialer proves its own only once its membership log seats a member under the
/// listener's key ([`Denied::seat`]), and the listener admits it only once its own log does. No
/// shared secret is proved. A listener with a node offers it before [`ALPN_NODES`], so a dialer that
/// offers it ([`NodeIdentity::dialing_mesh`]) speaks it, and every other dialer negotiates what it
/// did before.
pub const ALPN_MESH: &[u8] = b"atm/1";

/// What a dialer whose node dials the mesh, and a listener with a node, offer: [`ALPN_MESH`]
/// first, then the fleet's, so an older listener answers a mesh dialer on [`ALPN_NODES`], and a
/// dialer that does not offer the mesh negotiates exactly what it did before.
const PROTOCOLS_MESH: [&[u8]; 3] = [ALPN_MESH, ALPN_NODES, ALPN];

/// ALPN protocol (`atf-ask/1`) of a Mac asking to join ALab (`docs/DESIGN-fluid-swarm-2026-10-28.md`
/// F10): the one path a caller without the fleet secret reaches. It is not in [`PROTOCOLS`], so no
/// fleet dialer offers it, and only a listener given an ask handler ([`serve_offering_with_ask`])
/// negotiates it. No fleet op is ever dispatched on it: the stream goes to the ask handler alone.
pub const ALPN_ASK: &[u8] = b"atf-ask/1";

/// The protocols a listener that serves ask offers: the fleet's first, so a fleet dialer, which
/// never offers [`ALPN_ASK`], negotiates exactly what it did before.
const PROTOCOLS_ASKED: [&[u8]; 3] = [ALPN_NODES, ALPN, ALPN_ASK];

/// [`PROTOCOLS_MESH`] and [`ALPN_ASK`], for a listener with a node and an ask handler.
const PROTOCOLS_MESH_ASKED: [&[u8]; 4] = [ALPN_MESH, ALPN_NODES, ALPN, ALPN_ASK];

const EXPORTER_LABEL: &[u8] = b"EXPORTER-aterm-fleet-v1";

/// The TLS exporter label of an [`ALPN_MESH`] connection, so no fleet proof's exporter is ever a
/// mesh proof's.
const MESH_EXPORTER_LABEL: &[u8] = b"EXPORTER-aterm-mesh-v1";

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
    /// Prove this Mac to a member's vault desk over the fleet's own channel, which proved the
    /// fleet's key and this node key, to be let back into the vault with nobody's Allow
    /// (`aterm_secrets::client::readmit`, `aterm_agent::vault_join`).
    VaultJoin,
    /// Hold a persistent neighbour link ([`link`]) that carries the other mesh ops as streams
    /// and re-proves its peer (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.4, §11 B5).
    Neighbour,
    /// An astream stream of one of the lab's logs ([`link::BusLog`]), served by that log's broker.
    Bus,
    /// One digest exchange of one store with one neighbour.
    Inventory,
    /// Join asks, re-admission proofs and unbound `Node` records, from a member.
    Desk,
    /// atpkg's content-addressed toolset units (`atpkg::peers`), in place of port 7487.
    Units,
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
            Self::VaultJoin => "vault-join",
            Self::Neighbour => "neighbour",
            Self::Bus => "bus",
            Self::Inventory => "inventory",
            Self::Desk => "desk",
            Self::Units => "units",
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
            Self::VaultJoin,
            Self::Neighbour,
            Self::Bus,
            Self::Inventory,
            Self::Desk,
            Self::Units,
        ]
        .into_iter()
        .find(|op| op.word() == word)
    }

    /// Whether [`ALPN_MESH`] carries it: every op but the three bound to the fleet's secret or
    /// its own protocols. [`Self::Join`] registers with the issuer under the join key,
    /// [`Self::VaultJoin`] proves the vault's current fleet code (the desk takes it over the mesh's
    /// `Desk` op, `docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.4), and [`Self::Database`] keeps its
    /// own `atdb` protocols on the old listener. A dialer offers the mesh only for an op it
    /// carries, so neither end ever needs another try for one it does not.
    #[inline]
    #[must_use]
    pub const fn meshed(self) -> bool {
        !matches!(self, Self::Join | Self::VaultJoin | Self::Database)
    }

    /// Whether only [`ALPN_MESH`] carries it: the ops the mesh brought ([`Self::Neighbour`],
    /// [`Self::Bus`], [`Self::Inventory`], [`Self::Desk`], [`Self::Units`]), which no `atf/2`
    /// listener serves and no dialer offers on any other protocol.
    #[inline]
    #[must_use]
    pub const fn mesh_only(self) -> bool {
        matches!(
            self,
            Self::Neighbour | Self::Bus | Self::Inventory | Self::Desk | Self::Units
        )
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
    /// A secret no handshake proves: what a dial that offers [`ALPN_MESH`] alone passes, whose
    /// handshake proves node keys and never a fleet secret ([`FleetOp::mesh_only`]).
    pub(crate) const fn unproved() -> Self {
        Self {
            server: [0; 32],
            client: [0; 32],
            join: [0; 32],
        }
    }

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
    /// the curve: a key that is none verifies no signature, and nor does a point of small order
    /// ([`NodeKey::verifies`]).
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

    /// Whether `sig` is this key's Ed25519 signature of `message`, strictly
    /// ([`astream_aead::signature::verify`]): a small-order key or commitment, which signs every
    /// message under a lax check, verifies nothing, and so does a signature that is not
    /// [`SIG_LEN`] bytes.
    #[must_use]
    pub fn verifies(&self, message: &[u8], sig: &[u8]) -> bool {
        sig.try_into()
            .is_ok_and(|sig| astream_aead::signature::verify(&self.0, message, sig))
    }
}

impl fmt::Debug for NodeKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NodeKey({})", self.short())
    }
}

/// Every member of the fleet, rather than one named node, in `drive_from`.
pub const ALL_MEMBERS: &str = "*";

/// Whether `word` is a legacy node id or a key-derived mesh principal.
#[must_use]
pub fn is_node_id(word: &str) -> bool {
    hex::is_lower(word, 32)
        || word
            .strip_prefix("n-")
            .is_some_and(|id| hex::is_lower(id, 16))
}

/// A `[fleet] drive_from` grant for the [`NodeKey`] approved to drive this host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriveGrant {
    /// Node id recorded as a display hint, or [`ALL_MEMBERS`].
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

    /// Whether it admits the verified `key`, or every member. The node id is a display hint.
    #[must_use]
    pub fn admits(&self, _node: &str, key: NodeKey) -> bool {
        self.all() || self.key == key
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
        if !is_node_id(&node) {
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

/// Local node id and Ed25519 key pair used to sign [`ALPN_NODES`] and [`ALPN_MESH`] channel
/// proofs, and, where it dials the mesh, the verdict it judges each listener by
/// ([`Self::dialing_mesh`]), and where it notes each peer the mesh proved ([`Self::noting`]).
#[derive(Clone)]
pub struct NodeIdentity {
    node: String,
    key: NodeKey,
    pair: Arc<Ed25519KeyPair>,
    /// This Mac's membership verdict, read fresh for each dial, where this node dials
    /// [`ALPN_MESH`] first; `None` where it dials [`ALPN_NODES`] alone.
    mesh: Option<DenyFn>,
    /// The noting call each [`ALPN_MESH`] handshake this node completes, dialing or listening,
    /// notes its peer into ([`heard::Heard`]); `None` where it notes none.
    #[cfg(unix)]
    heard: Option<heard::Heard>,
}

impl NodeIdentity {
    /// Construct from a legacy or mesh `node` id and 32-byte Ed25519 `seed`.
    #[must_use]
    pub fn from_seed(node: &str, seed: &[u8]) -> Option<Self> {
        if !is_node_id(node) {
            return None;
        }
        let pair = Ed25519KeyPair::from_seed_unchecked(seed).ok()?;
        let key = NodeKey(pair.public_key().as_ref().try_into().ok()?);
        Some(Self {
            node: node.to_owned(),
            key,
            pair: Arc::new(pair),
            mesh: None,
            #[cfg(unix)]
            heard: None,
        })
    }

    /// This node dialing [`ALPN_MESH`] first where `verdict` is given: each listener's key is
    /// judged by the verdict it returns, this Mac's membership log read fresh for that dial
    /// ([`Denied::under`]), and this end proves its own key only to a listener the log seats a
    /// member under ([`Denied::seat`]). With `None` it dials [`ALPN_NODES`] alone, as before the
    /// mesh. The dialer that configures it passes `aterm_types::mesh_switch::MESH.then(..)`, so the
    /// switch is read once, into this field, and a test sets the field either way.
    #[must_use]
    pub fn dialing_mesh(self, verdict: Option<DenyFn>) -> Self {
        Self {
            mesh: verdict,
            ..self
        }
    }

    /// Whether this node dials [`ALPN_MESH`] first ([`Self::dialing_mesh`]).
    #[inline]
    #[must_use]
    pub const fn dials_mesh(&self) -> bool {
        self.mesh.is_some()
    }

    /// This node noting into `heard` the peer of each [`ALPN_MESH`] handshake it completes, as
    /// the dialer ([`client_handshake_by`]) or as a listener's node ([`Listening::node`]): the
    /// noting call of the heard ledger (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §11 B6). With
    /// `None` it notes nothing, as a test's, a lab's or a process that keeps no state does.
    #[cfg(unix)]
    #[must_use]
    pub fn noting(self, heard: Option<heard::Heard>) -> Self {
        Self { heard, ..self }
    }

    /// The noting call this node notes its mesh peers into ([`Self::noting`]).
    #[cfg(unix)]
    #[must_use]
    pub const fn heard(&self) -> Option<&heard::Heard> {
        self.heard.as_ref()
    }

    /// This machine's legacy or mesh node id.
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

    /// The verdict this node judges an [`ALPN_MESH`] listener by, read now; `None` where it
    /// does not dial the mesh.
    fn verdict(&self) -> Option<Arc<Denied>> {
        self.mesh.as_ref().map(|verdict| verdict())
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
    key.verifies(&record_message(domain, body), sig)
}

impl fmt::Debug for NodeIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NodeIdentity({} {:?})", self.node, self.key)
    }
}

/// Verified possession proof of a peer's claimed `node` id and Ed25519 `key` on one connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeProof {
    /// Claimed legacy or mesh node id.
    pub node: String,
    /// Public key whose private half signed the connection's exporter.
    pub key: NodeKey,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Signer {
    Dialer,
    Listener,
    /// An [`ALPN_MESH`] dialer.
    MeshDialer,
    /// An [`ALPN_MESH`] listener.
    MeshListener,
    /// An [`ALPN_MESH`] dialer re-proving its key on a neighbour link ([`link`]).
    ReproveDialer,
    /// An [`ALPN_MESH`] listener re-proving its key on a neighbour link ([`link`]).
    ReproveListener,
}

impl Signer {
    #[inline]
    const fn label(self) -> &'static [u8] {
        match self {
            Self::Dialer => b"aterm-fleet/v2 node dialer",
            Self::Listener => b"aterm-fleet/v2 node listener",
            Self::MeshDialer => b"aterm-mesh/1 node dialer",
            Self::MeshListener => b"aterm-mesh/1 node listener",
            Self::ReproveDialer => b"aterm-mesh/1 reproof dialer",
            Self::ReproveListener => b"aterm-mesh/1 reproof listener",
        }
    }

    /// The other end's role on the same [`ALPN_MESH`] channel, whose proof sent back is a
    /// reflection ([`FleetError::Reflection`]); `None` under [`ALPN_NODES`], whose node proofs
    /// keep the refusal they always had.
    #[inline]
    const fn mirrored(self) -> Option<Self> {
        match self {
            Self::MeshDialer => Some(Self::MeshListener),
            Self::MeshListener => Some(Self::MeshDialer),
            Self::ReproveDialer => Some(Self::ReproveListener),
            Self::ReproveListener => Some(Self::ReproveDialer),
            Self::Dialer | Self::Listener => None,
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
/// [`FleetError::NodeProof`] for a signature that does not verify, [`FleetError::Reflection`] for
/// one that verifies only as the other end's role on an [`ALPN_MESH`] channel
/// ([`Signer::mirrored`]), [`FleetError::Malformed`] for a line that is no node line, or the read's
/// own error.
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
    if !is_node_id(node) || !hex::is_lower(sig, 2 * SIG_LEN) {
        return Err(malformed());
    }
    let sig = crate::hex_decode(sig).ok_or_else(malformed)?;
    let signs = |end: Signer| key.verifies(&node_message(end, exporter, node), &sig);
    if !signs(end) {
        return Err(if end.mirrored().is_some_and(signs) {
            FleetError::Reflection
        } else {
            FleetError::NodeProof
        });
    }
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
    /// The listener has not read its deny list, or a verdict from the membership log it holds,
    /// yet, so it refuses every peer until it has: a refusal to dial again later, never a verdict
    /// on this machine. A listener names it to
    /// itself; on the wire it answers `NO capacity` ([`Self::AddressBusy`]), which every dialer,
    /// one older than this build included, takes as "busy, dial again" and which disowns no one.
    /// A dialer from this build on also reads `NO unread` as this, so a later listener can name it
    /// on the wire once no older dialer is left.
    DenyListUnread,
    /// Peer presented a proof signed with this end's own role key.
    Reflection,
    /// Listener does not serve the requested operation (`NO op`).
    UnknownOp(String),
    /// Listener proved a scope word this version does not recognise.
    UnknownScope(String),
    /// Peer's Ed25519 node signature failed verification (`NO node`).
    NodeProof,
    /// The [`ALPN_MESH`] listener's membership log seats no member under this end's node key, nor
    /// took it out (`NO unknown`): such a key reaches only the desk, over [`ALPN_ASK`], which
    /// takes a seated member's `Node` record that binds it.
    UnknownKey,
    /// The [`ALPN_MESH`] listener proved a node key this Mac's membership log seats no member
    /// under, nor took out: this end proved nothing to it.
    ListenerUnknown,
    /// The [`ALPN_MESH`] listener proved a node key this Mac's membership log took out, or whose
    /// node or key the swarm roster denies: this end proved nothing to it.
    ListenerOut,
    /// This Mac has not read its deny list or membership log yet, so it judges no
    /// [`ALPN_MESH`] listener's key and proved nothing to it.
    VerdictUnread,
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

    /// Whether an [`ALPN_MESH`] dial that ended so goes again on [`ALPN_NODES`] at the same
    /// address: one end's log binds the other's key to no seat (a seat with no `Node` record yet,
    /// one whose record has not reached this Mac or the listener), or this Mac cannot judge the
    /// listener now. Its proof of the fleet's secret then admits it as before the mesh. A key out
    /// of either log, a roster deny, a forged or reflected proof, and every other refusal stand.
    #[must_use]
    pub const fn falls_back(&self) -> bool {
        matches!(
            self,
            Self::UnknownKey | Self::ListenerUnknown | Self::VerdictUnread
        )
    }

    /// The same failure again, for each dial that shares one handshake (the Unix member's).
    #[cfg(unix)]
    pub(crate) fn copied(&self) -> Self {
        match self {
            Self::UnknownKey => Self::UnknownKey,
            Self::ListenerUnknown => Self::ListenerUnknown,
            Self::ListenerOut => Self::ListenerOut,
            Self::VerdictUnread => Self::VerdictUnread,
            Self::ListenerWrongSecret => Self::ListenerWrongSecret,
            Self::DialerWrongSecret => Self::DialerWrongSecret,
            Self::DialerHungUp => Self::DialerHungUp,
            Self::Refused => Self::Refused,
            Self::AddressBusy => Self::AddressBusy,
            Self::DeniedNode => Self::DeniedNode,
            Self::DenyListUnread => Self::DenyListUnread,
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
            Self::DenyListUnread => f.write_str(
                "the listener has not read its deny list or membership log yet, so it admits no \
                 one; try again once it has",
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
            Self::UnknownKey => f.write_str(
                "the listener's membership log binds this machine's node key to no seat: it \
                 reaches only the listener's desk until a `Node` record binds it",
            ),
            Self::ListenerUnknown => f.write_str(
                "the listener proved a node key this machine's membership log binds to no seat; \
                 nothing was sent",
            ),
            Self::ListenerOut => f.write_str(
                "the listener proved a node key this machine's membership log took out (or the \
                 swarm roster denies); nothing was sent",
            ),
            Self::VerdictUnread => f.write_str(
                "this machine has not read its deny list or membership log yet, so it judges no \
                 mesh listener; nothing was sent",
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

/// [`server_config`] that offers [`ALPN_MESH`] first, for a listener with a node.
fn server_config_meshed() -> Arc<ServerConfig> {
    #[cfg(all(test, unix))]
    test_socket_headroom();
    static CONFIG: OnceLock<Arc<ServerConfig>> = OnceLock::new();
    Arc::clone(CONFIG.get_or_init(|| listener_config(&PROTOCOLS_MESH)))
}

/// [`server_config_meshed`] that also offers [`ALPN_ASK`], for a listener with a node and an ask
/// handler.
fn server_config_meshed_asked() -> Arc<ServerConfig> {
    #[cfg(all(test, unix))]
    test_socket_headroom();
    static CONFIG: OnceLock<Arc<ServerConfig>> = OnceLock::new();
    Arc::clone(CONFIG.get_or_init(|| listener_config(&PROTOCOLS_MESH_ASKED)))
}

/// What a listener serving `ask` (or not) with a node (or none) offers: [`ALPN_MESH`] only where
/// it holds a node to prove first.
fn served_config(asked: bool, node: bool) -> Arc<ServerConfig> {
    match (asked, node) {
        (true, true) => server_config_meshed_asked(),
        (true, false) => server_config_asked(),
        (false, true) => server_config_meshed(),
        (false, false) => server_config(),
    }
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

/// [`client_config`] offering [`ALPN_MESH`] first, for a node that dials the mesh
/// ([`NodeIdentity::dialing_mesh`]).
fn mesh_client_config() -> Arc<ClientConfig> {
    #[cfg(all(test, unix))]
    test_socket_headroom();
    static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    Arc::clone(CONFIG.get_or_init(|| dialer_config(&PROTOCOLS_MESH)))
}

/// The dialer's for an op only [`ALPN_MESH`] carries ([`FleetOp::mesh_only`]): no other protocol is
/// offered, so a listener that does not speak it refuses the TLS handshake, and no fleet secret is
/// ever proved.
fn mesh_only_client_config() -> Arc<ClientConfig> {
    static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    Arc::clone(CONFIG.get_or_init(|| dialer_config(&[ALPN_MESH])))
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
        config
            .alpn_protocols
            .insert(0, ALPN_DATABASE_CREDENTIALS.to_vec());
        config
            .alpn_protocols
            .insert(0, ALPN_DATABASE_FRONTIER.to_vec());
        config
            .alpn_protocols
            .insert(0, ALPN_DATABASE_DELTA.to_vec());
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
        config
            .alpn_protocols
            .insert(0, ALPN_DATABASE_CREDENTIALS.to_vec());
        config
            .alpn_protocols
            .insert(0, ALPN_DATABASE_FRONTIER.to_vec());
        config
            .alpn_protocols
            .insert(0, ALPN_DATABASE_DELTA.to_vec());
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
    /// The two ends spoke [`ALPN_MESH`]: the dialer's key is one the log seats a member under.
    pub mesh: bool,
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
/// While `unread`, the listener has never read its deny list, or its membership log exists but
/// gave no verdict yet, so it refuses every dialer that proved the fleet secret
/// ([`FleetError::DenyListUnread`], `NO capacity` on the wire) rather than admit one the list or
/// the log it could not read names.
///
/// It is also the standing [`ALPN_MESH`] judges a peer's key by, at either end ([`Self::seat`]):
/// `members` holds the keys the log seats a member under, so a key it names neither way is
/// unknown, and reaches only the desk.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Denied {
    /// Node ids, as [`is_denied_node`] matches them.
    pub nodes: Vec<String>,
    /// Node keys.
    pub keys: Vec<NodeKey>,
    /// No deny list or log verdict has been read yet: every proved dialer is refused, to dial
    /// again later.
    pub unread: bool,
    /// The node keys the membership log seats a current member under: the only keys
    /// [`ALPN_MESH`] admits, at either end. Empty with no log.
    pub members: Vec<NodeKey>,
}

/// Where a node key a peer proved stands for [`ALPN_MESH`], as one [`Denied`] verdict says it
/// ([`Denied::seat`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seat {
    /// The membership log seats a current member under it, and the roster denies neither its node
    /// nor its key.
    Member,
    /// The log took it out (its Mac removed, its member bound another key since, or two members
    /// claim it), or the roster denies its node or key.
    Out,
    /// No record the verdict was read from binds it: a seat with no `Node` record yet, one whose
    /// record has not arrived, or no member at all. It reaches only the desk.
    Unknown,
    /// No deny list or log verdict was read yet: nothing is judged.
    Unread,
}

impl Denied {
    /// What a listener refuses under the deny list `nodes` (`None` while none was read) and the
    /// membership log's verdict `log` (`None` on a Mac with no log): the list's nodes and each
    /// key the log took out, or every proved dialer while either is unread
    /// ([`crate::fleet_member::MemberKeys::unreadable`]); and the keys the log seats a member
    /// under, which [`ALPN_MESH`] admits ([`Self::seat`]). Read for each connection, so a key the
    /// log takes out is refused from the first handshake after its `Remove` arrives.
    #[cfg(unix)]
    #[must_use]
    pub fn under(nodes: Option<&[String]>, log: Option<&crate::fleet_member::MemberKeys>) -> Self {
        match nodes {
            Some(nodes) if !log.is_some_and(|log| log.unread().is_some()) => Self {
                nodes: nodes.to_vec(),
                keys: log
                    .map(crate::fleet_member::MemberKeys::out)
                    .unwrap_or_default(),
                unread: false,
                members: log
                    .map(crate::fleet_member::MemberKeys::members)
                    .unwrap_or_default(),
            },
            _ => Self::not_yet_read(),
        }
    }

    /// Where the key `proof` proved stands for [`ALPN_MESH`]: nothing is judged while unread;
    /// out where this verdict refuses it ([`Self::refuses`]: a roster-denied node or key, or a key
    /// the log took out); a member's where the log seats a member under it; else unknown.
    #[must_use]
    pub fn seat(&self, proof: &NodeProof) -> Seat {
        if self.unread {
            Seat::Unread
        } else if self.refuses(Some(proof)) {
            Seat::Out
        } else if self.members.contains(&proof.key) {
            Seat::Member
        } else {
            Seat::Unknown
        }
    }

    /// The roster's node ids alone.
    #[must_use]
    pub fn nodes(nodes: &[String]) -> Self {
        Self {
            nodes: nodes.to_vec(),
            ..Self::default()
        }
    }

    /// A listener's that has not read its deny list or a log verdict yet: it refuses every
    /// proved dialer.
    #[must_use]
    pub fn not_yet_read() -> Self {
        Self {
            unread: true,
            ..Self::default()
        }
    }

    /// Whether it refuses no one: it names no one, and a list was read.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.unread && self.nodes.is_empty() && self.keys.is_empty()
    }

    /// Whether it refuses a dialer that proved `peer`: every one while `unread`; while it names
    /// anyone, both a named node id or key and an unproved peer (`None`) are refused, so a removed
    /// machine cannot pass by stripping its node key.
    #[must_use]
    pub fn refuses(&self, peer: Option<&NodeProof>) -> bool {
        self.unread
            || (!self.is_empty()
                && peer.is_none_or(|proof| {
                    is_denied_node(&proof.node, &self.nodes)
                        || is_denied_key(&proof.key, &self.nodes)
                        || self.keys.contains(&proof.key)
                }))
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

/// Authenticate a database-only listener, preferring [`ALPN_DATABASE_DELTA`]
/// and retaining the earlier database and fleet protocols for older dialers. This serves
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

/// A released `atdb/2` listener for cross-version protocol tests.
///
/// # Errors
/// As [`server_handshake_database_as`].
#[cfg(any(test, debug_assertions))]
pub fn test_server_handshake_database_v2_as(
    tcp: TcpStream,
    secret: &FleetSecret,
    me: Option<&NodeIdentity>,
    within: Duration,
) -> Result<Accepted, FleetError> {
    test_server_handshake_database_version_as(tcp, secret, me, within, 2)
}

/// A released `atdb/3` listener for cross-version protocol tests.
///
/// # Errors
/// As [`server_handshake_database_as`].
#[cfg(any(test, debug_assertions))]
pub fn test_server_handshake_database_v3_as(
    tcp: TcpStream,
    secret: &FleetSecret,
    me: Option<&NodeIdentity>,
    within: Duration,
) -> Result<Accepted, FleetError> {
    test_server_handshake_database_version_as(tcp, secret, me, within, 3)
}

/// A released `atdb/4` listener (0.153's) for cross-version protocol tests.
///
/// # Errors
/// As [`server_handshake_database_as`].
#[cfg(any(test, debug_assertions))]
pub fn test_server_handshake_database_v4_as(
    tcp: TcpStream,
    secret: &FleetSecret,
    me: Option<&NodeIdentity>,
    within: Duration,
) -> Result<Accepted, FleetError> {
    test_server_handshake_database_version_as(tcp, secret, me, within, 4)
}

/// The `atdb` version `alpn` names, or `None` for a protocol that is not a database one.
#[cfg(any(test, debug_assertions))]
fn database_version(alpn: &[u8]) -> Option<u8> {
    [
        ALPN_DATABASE_REPAIR,
        ALPN_DATABASE_CREDENTIALS,
        ALPN_DATABASE_FRONTIER,
        ALPN_DATABASE_DELTA,
    ]
    .iter()
    .zip(2..)
    .find_map(|(known, version)| (*known == alpn).then_some(version))
}

/// Whether a build that speaks database protocols up to `atdb/<version>` offers `alpn`.
#[cfg(any(test, debug_assertions))]
fn offered_up_to(version: u8, alpn: &[u8]) -> bool {
    database_version(alpn).is_none_or(|own| own <= version)
}

#[cfg(any(test, debug_assertions))]
fn test_server_handshake_database_version_as(
    tcp: TcpStream,
    secret: &FleetSecret,
    me: Option<&NodeIdentity>,
    within: Duration,
    version: u8,
) -> Result<Accepted, FleetError> {
    let mut config = (*database_server_config()).clone();
    config
        .alpn_protocols
        .retain(|alpn| offered_up_to(version, alpn));
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
        Arc::new(config),
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
        (scope, serves, || denied),
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
/// `deny` is called at most once, and only for a dialer that proved the fleet secret (and its node,
/// under a node-proof protocol) ([`listener_proves`]), or its node under [`ALPN_MESH`]
/// ([`listener_meets`]): a socket that proves nothing (a peer that gave up, a wrong secret, an ask
/// caller) costs no deny-list lookup.
#[expect(
    clippy::too_many_arguments,
    reason = "handshake binds transport, watchdog, deadline, credentials, scope, admission, TLS config and optional ask handler"
)]
fn server_handshake_or_ask<D: std::ops::Deref<Target = Denied>>(
    tcp: TcpStream,
    watchdog: HandshakeWatchdog,
    within: Duration,
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
    (scope, serves, deny): (FleetScope, &[FleetOp], impl FnOnce() -> D),
    admit: impl FnOnce(FleetOp) -> bool,
    config: Arc<ServerConfig>,
    ask: Option<(&AskHandler, SocketAddr)>,
) -> Result<Handshook, FleetError> {
    let mut mesh = false;
    let (mut transport, proved) = bounded(tcp, watchdog, within, |tcp| {
        let (mut conn, exporter, spoken) = accept_tls_on(tcp, config)?;
        let nodes = match spoken {
            Spoken::Fleet { nodes } => nodes,
            Spoken::Mesh => {
                // Only a listener with a node offers the mesh ([`served_config`]).
                let me = me.ok_or(FleetError::Alpn)?;
                mesh = true;
                let proved = listener_meets(
                    &mut rustls::Stream::new(&mut conn, tcp),
                    &exporter,
                    me,
                    (scope, serves),
                    deny,
                    admit,
                )?;
                return Ok((conn, exporter, Some(proved)));
            }
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
            deny,
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
        mesh,
    })))
}

/// Close an answered ask connection: a TLS close, then the socket, waiting for neither.
fn end_asked(transport: TlsTransport<ServerConnection>) {
    crate::tls::end_without_waiting(transport, true);
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
    /// The two ends spoke [`ALPN_MESH`]: the listener's key is one this Mac's log seats a member
    /// under, and no fleet secret was proved.
    pub mesh: bool,
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

/// Authenticate a database dialer, preferring [`ALPN_DATABASE_DELTA`] and
/// negotiating an earlier database or fleet protocol with an older listener. TLS pins,
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

/// A released `atdb/2` dialer for cross-version protocol tests.
///
/// # Errors
/// As [`client_handshake_database_as`].
#[cfg(any(test, debug_assertions))]
pub fn test_client_handshake_database_v2_as(
    tcp: TcpStream,
    secret: &FleetSecret,
    me: Option<&NodeIdentity>,
    within: Duration,
) -> Result<Dialed, FleetError> {
    test_client_handshake_database_version_as(tcp, secret, me, within, 2)
}

/// A released `atdb/3` dialer for cross-version protocol tests.
///
/// # Errors
/// As [`client_handshake_database_as`].
#[cfg(any(test, debug_assertions))]
pub fn test_client_handshake_database_v3_as(
    tcp: TcpStream,
    secret: &FleetSecret,
    me: Option<&NodeIdentity>,
    within: Duration,
) -> Result<Dialed, FleetError> {
    test_client_handshake_database_version_as(tcp, secret, me, within, 3)
}

/// A released `atdb/4` dialer (0.153's) for cross-version protocol tests.
///
/// # Errors
/// As [`client_handshake_database_as`].
#[cfg(any(test, debug_assertions))]
pub fn test_client_handshake_database_v4_as(
    tcp: TcpStream,
    secret: &FleetSecret,
    me: Option<&NodeIdentity>,
    within: Duration,
) -> Result<Dialed, FleetError> {
    test_client_handshake_database_version_as(tcp, secret, me, within, 4)
}

#[cfg(any(test, debug_assertions))]
fn test_client_handshake_database_version_as(
    tcp: TcpStream,
    secret: &FleetSecret,
    me: Option<&NodeIdentity>,
    within: Duration,
    version: u8,
) -> Result<Dialed, FleetError> {
    let mut config = (*database_client_config()).clone();
    config
        .alpn_protocols
        .retain(|alpn| offered_up_to(version, alpn));
    client_handshake_by_on(tcp, secret, me, FleetOp::Database, within, Arc::new(config))
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
///
/// Where `me` dials the mesh ([`NodeIdentity::dialing_mesh`]) and the mesh carries `op`
/// ([`FleetOp::meshed`]), it offers [`ALPN_MESH`] first: a listener that speaks it proves its
/// node first, and this end proves its own only once its verdict seats a member under the
/// listener's key. An older listener answers on [`ALPN_NODES`], proved as before.
pub fn client_handshake_by(
    tcp: TcpStream,
    secret: &FleetSecret,
    me: Option<&NodeIdentity>,
    op: FleetOp,
    within: Duration,
) -> Result<Dialed, Undialed> {
    client_handshake_controlled(tcp, secret, me, op, within, None)
}

#[cfg(unix)]
pub(crate) fn client_handshake_by_stopped(
    tcp: TcpStream,
    secret: &FleetSecret,
    me: Option<&NodeIdentity>,
    op: FleetOp,
    within: Duration,
    control: &Arc<crate::tls::PreparationControl>,
) -> Result<Dialed, Undialed> {
    client_handshake_controlled(tcp, secret, me, op, within, Some(control))
}

fn client_handshake_controlled(
    tcp: TcpStream,
    secret: &FleetSecret,
    me: Option<&NodeIdentity>,
    op: FleetOp,
    within: Duration,
    control: Option<&Arc<crate::tls::PreparationControl>>,
) -> Result<Dialed, Undialed> {
    let dials_mesh = me.is_some_and(NodeIdentity::dials_mesh);
    let config = if op.mesh_only() {
        if !dials_mesh {
            // Only the mesh carries it, and a node that judges no mesh listener cannot dial it.
            return Err(Undialed {
                error: FleetError::Alpn,
                refused_by: None,
            });
        }
        mesh_only_client_config()
    } else if op.meshed() && dials_mesh {
        mesh_client_config()
    } else {
        client_config()
    };
    client_handshake_by_on_controlled(tcp, secret, me, op, within, config, control)
}

/// Dial `op`, an op only the mesh carries ([`FleetOp::mesh_only`]), as `me`: the listener proves
/// its node first, and `me` proves its own only once its verdict seats a member under the
/// listener's key. No fleet secret is proved.
/// # Errors
/// As [`client_handshake_by`]; [`FleetError::Alpn`] where `me` dials no mesh.
pub fn dial_mesh_op(
    tcp: TcpStream,
    me: &NodeIdentity,
    op: FleetOp,
    within: Duration,
) -> Result<Dialed, Undialed> {
    debug_assert!(
        op.mesh_only(),
        "`{}` is not an op only the mesh carries",
        op.word()
    );
    client_handshake_by(tcp, &FleetSecret::unproved(), Some(me), op, within)
}

fn client_handshake_by_on(
    tcp: TcpStream,
    secret: &FleetSecret,
    me: Option<&NodeIdentity>,
    op: FleetOp,
    within: Duration,
    config: Arc<ClientConfig>,
) -> Result<Dialed, Undialed> {
    client_handshake_by_on_controlled(tcp, secret, me, op, within, config, None)
}

fn client_handshake_by_on_controlled(
    tcp: TcpStream,
    secret: &FleetSecret,
    me: Option<&NodeIdentity>,
    op: FleetOp,
    within: Duration,
    config: Arc<ClientConfig>,
    control: Option<&Arc<crate::tls::PreparationControl>>,
) -> Result<Dialed, Undialed> {
    let mut refused_by = None;
    let undialed = |error: FleetError, refused_by: Option<NodeProof>| Undialed {
        refused_by: refused_by.filter(|_| {
            matches!(
                error,
                FleetError::UnknownOp(_)
                    | FleetError::AddressBusy
                    | FleetError::DeniedNode
                    | FleetError::DenyListUnread
            )
        }),
        error,
    };
    // When this dial began, on the clock of the call that notes a mesh peer it proves.
    #[cfg(unix)]
    let began_ms = me.and_then(NodeIdentity::heard).map(heard::Heard::now_ms);
    let watchdog = arm(&tcp, within).map_err(|e| undialed(e, None))?;
    let preparation = control
        .map(|control| control.register(&watchdog))
        .transpose()
        .map_err(|error| undialed(FleetError::Io(error), None))?;
    if let Some(control) = control {
        control
            .check()
            .map_err(|error| undialed(FleetError::Io(error), None))?;
    }
    let (mut transport, (scope, peer, mesh)) = bounded(tcp, watchdog, within, |tcp| {
        let (mut conn, exporter, spoken) = connect_tls_on(tcp, config)?;
        let nodes = match spoken {
            Spoken::Fleet { nodes } => nodes,
            Spoken::Mesh => {
                // Only a node that dials the mesh offers it ([`client_handshake_by`]).
                let me = me.filter(|me| me.dials_mesh()).ok_or(FleetError::Alpn)?;
                let (scope, peer) = dialer_meets(
                    &mut rustls::Stream::new(&mut conn, tcp),
                    &exporter,
                    me,
                    op,
                    &mut refused_by,
                )?;
                return Ok((conn, exporter, (scope, Some(peer), true)));
            }
            Spoken::Ask => return Err(FleetError::Alpn),
        };
        let me = me.filter(|_| nodes);
        let (scope, peer) = dialer_proves(
            &mut rustls::Stream::new(&mut conn, tcp),
            &exporter,
            (secret, me, nodes),
            op,
            &mut refused_by,
        )?;
        Ok((conn, exporter, (scope, peer, false)))
    })
    .map_err(|e| undialed(e, refused_by.take()))?;
    drop(preparation);
    if let Some(control) = control {
        control
            .check()
            .map_err(|error| undialed(FleetError::Io(error), None))?;
    }
    release_deadlines(transport.stream().get_ref()).map_err(|e| undialed(e, None))?;
    // A listener the mesh proved is a member's node-proven answer (§11 B6).
    #[cfg(unix)]
    if mesh
        && let (Some(heard), Some(began_ms), Some(proof)) =
            (me.and_then(NodeIdentity::heard), began_ms, peer.as_ref())
        && let (Ok(addr), Ok(local)) = (transport.tcp().peer_addr(), transport.tcp().local_addr())
    {
        heard.note(proof, (addr, local), began_ms);
    }
    Ok(Dialed {
        transport,
        scope,
        peer,
        mesh,
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
    ask_handshake_controlled(tcp, within, None)
}

#[cfg(unix)]
pub(crate) fn ask_handshake_stopped(
    tcp: TcpStream,
    within: Duration,
    control: &Arc<crate::tls::PreparationControl>,
) -> Result<TlsTransport<ClientConnection>, FleetError> {
    ask_handshake_controlled(tcp, within, Some(control))
}

fn ask_handshake_controlled(
    tcp: TcpStream,
    within: Duration,
    control: Option<&Arc<crate::tls::PreparationControl>>,
) -> Result<TlsTransport<ClientConnection>, FleetError> {
    let watchdog = arm(&tcp, within)?;
    let preparation = control
        .map(|control| control.register(&watchdog))
        .transpose()
        .map_err(FleetError::Io)?;
    if let Some(control) = control {
        control.check().map_err(FleetError::Io)?;
    }
    let (transport, ()) = bounded(tcp, watchdog, within, |tcp| {
        let (conn, exporter, spoken) = connect_tls_on(tcp, ask_client_config())?;
        if spoken != Spoken::Ask {
            return Err(FleetError::Alpn);
        }
        Ok((conn, exporter, ()))
    })?;
    drop(preparation);
    if let Some(control) = control {
        control.check().map_err(FleetError::Io)?;
    }
    release_deadlines(transport.tcp())?;
    Ok(transport)
}

/// Wall-clock deadline (3 s) for TLS and both proofs on an accepted fleet connection.
pub const HANDSHAKE_WITHIN: Duration = Duration::from_secs(3);

/// The dev seam that sets, in milliseconds, how long a listener gives each connection's handshake
/// (and aterm's fleet listener each read and write of an op it answers itself: join, book, facts,
/// mail code, census and vault join) and a dialer each address's, in place of
/// [`HANDSHAKE_WITHIN`]: a test world whose every peer is an aterm on
/// the same machine, and so must finish its handshake and exchange however loaded the machine is,
/// gives it its hang detector.
pub const HANDSHAKE_SEAM: &str = "ATERM_DEBUG_FLEET_HANDSHAKE_MS";

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
    /// Listener stopped due to an error before its generation was stopped.
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
    mesh: bool,
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

    /// Whether the connection spoke [`ALPN_MESH`]: its peer proved a key this Mac's membership
    /// log seats a member under ([`Self::peer_node`]), and proved no fleet secret.
    #[must_use]
    pub const fn meshed(&self) -> bool {
        self.mesh
    }

    /// Relay the connection to `local` via [`tls::relay_answers`], releasing the listener hold
    /// before EOF.
    /// # Errors
    /// As [`tls::relay_answers`], or if native locality evidence changed.
    pub fn relay_answers(self, local: aterm_uds::CtlStream) -> io::Result<()> {
        self.admission.recheck()?;
        self.release
            .tenure
            .relays_to(aterm_uds::kept_out(|| local.try_clone())?);
        let result = tls::relay_answers(self.transport, local, self.release);
        self.admission.recheck()?;
        result
    }

    /// Hold this [`FleetOp::Neighbour`] connection as a link ([`link::Link`]) run as `cfg` says,
    /// with `me` the listener's node. It gives back its handshake slot, since a link is no
    /// handshake, and no longer counts toward its source's share or the listener's lifetime: a
    /// [`link::Neighbours`] table bounds links, and the listener's stop still cuts it.
    /// # Errors
    /// [`io::ErrorKind::InvalidInput`] for a connection that is no [`ALPN_MESH`] one with a proven
    /// peer; changed native locality evidence; or where the link could not start.
    pub fn into_link(
        self,
        me: &NodeIdentity,
        cfg: &link::LinkConfig,
    ) -> io::Result<Arc<link::Link>> {
        let Some(peer) = self.peer.filter(|_| self.mesh) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a neighbour link is an atm/1 connection with a proven peer",
            ));
        };
        self.admission.recheck()?;
        let Release { _slot, tenure } = self.release;
        drop(_slot);
        link::Link::start(
            self.transport.into_stream(),
            me,
            (peer, false),
            cfg,
            Box::new((tenure, self.admission)),
        )
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
    fn validity_deadline(&self) -> Option<Instant> {
        match self {
            Self::Fleet(_) => None,
            #[cfg(unix)]
            Self::Link(_) => Some(Instant::now() + ACCEPT_POLL),
        }
    }
    #[cfg(unix)]
    fn withdrawal_bell(&self) -> io::Result<Option<std::os::fd::BorrowedFd<'_>>> {
        match self {
            Self::Fleet(_) => Ok(None),
            Self::Link(listener) => listener.withdrawal_bell().map(Some),
        }
    }
    #[cfg(unix)]
    fn changes_fd(&self) -> Option<std::os::fd::BorrowedFd<'_>> {
        match self {
            Self::Fleet(_) => None,
            Self::Link(listener) => listener.changes_fd(),
        }
    }
    #[cfg(unix)]
    fn drain_changes(&self) -> io::Result<bool> {
        match self {
            Self::Fleet(_) => Ok(false),
            Self::Link(listener) => listener.drain_changes(),
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

#[cfg(unix)]
fn wait_serving(
    listener: &ListenerSocket<'_>,
    stop: &Stop,
    wake: &Wake,
    deadline: Option<Instant>,
) -> io::Result<()> {
    loop {
        let Some(ready) = wait_listener_readable(
            Some(listener.tcp()),
            Some(stop.bell()?),
            Some(wake.reader()),
            listener.withdrawal_bell()?,
            listener.changes_fd(),
            deadline,
        )?
        else {
            return Ok(());
        };
        if ready.changes && listener.drain_changes()? {
            return Ok(());
        }
        if ready.listener
            || ready.stop
            || ready.live
            || ready.withdrawn
            || deadline.is_some_and(|at| Instant::now() >= at)
        {
            return Ok(());
        }
        // A route source can also hear replies to our own route queries. Drain those hints,
        // then retain the same deadline without running policy queries that generate more.
    }
}

/// Without native stop bells, retain the bounded serving fallback. No Live stream is made on
/// these targets, and the next loop turn checks cancellation and lifetime state again.
#[cfg(not(unix))]
fn wait_serving(
    listener: &ListenerSocket<'_>,
    _stop: &Stop,
    deadline: Option<Instant>,
) -> io::Result<()> {
    let fallback = Instant::now() + ACCEPT_POLL;
    let deadline = deadline.map_or(fallback, |at| at.min(fallback));
    wait_listener_readable(Some(listener.tcp()), Some(deadline)).map(|_| ())
}

/// Accept and authenticate fleet connections on `listener` until `stop` is set, enforcing
/// NET-1 pre-auth slot displacement, per-source shares, and connection lifetimes.
pub fn serve<H, E>(
    listener: &TcpListener,
    listening: &Listening,
    stop: &Stop,
    hand_over: H,
    on_event: E,
) where
    H: Fn(Served, FleetOp) -> io::Result<()> + Send + Sync + Clone + 'static,
    E: Fn(FleetEvent) + Send + Sync + Clone + 'static,
{
    let empty = Arc::new(Denied::default());
    let offering = (listening.scope, listening.serves);
    serve_inner(
        ListenerSocket::Fleet(listener),
        (
            listening,
            Arc::new(move || offering),
            Arc::new(move || Arc::clone(&empty)),
        ),
        None,
        stop,
        Locality::Fleet,
        HANDSHAKE_WITHIN,
        hand_over,
        on_event,
    );
}

/// [`serve`], each connection proving the level and serving the ops `offer` names as it is
/// handshaking, in place of `listening`'s. Its worker obtains the current policy once; a level
/// or op changed afterward does not cut a connection already proved under that policy.
/// Policy readers may run concurrently; each worker uses its own returned snapshot.
pub fn serve_offering<H, E>(
    listener: &TcpListener,
    listening: &Listening,
    offer: OfferFn,
    stop: &Stop,
    hand_over: H,
    on_event: E,
) where
    H: Fn(Served, FleetOp) -> io::Result<()> + Send + Sync + Clone + 'static,
    E: Fn(FleetEvent) + Send + Sync + Clone + 'static,
{
    let empty = Arc::new(Denied::default());
    serve_inner(
        ListenerSocket::Fleet(listener),
        (listening, offer, Arc::new(move || Arc::clone(&empty))),
        None,
        stop,
        Locality::Fleet,
        HANDSHAKE_WITHIN,
        hand_over,
        on_event,
    );
}

/// [`serve_offering`], also refusing any dialer `deny()` refuses ([`Denied::refuses`]) with
/// [`FleetError::DeniedNode`] (`NO denied`), and every one while its answer is unread
/// ([`Denied::unread`]) with [`FleetError::DenyListUnread`] (`NO capacity` on the wire, so every
/// dialer, an older one included, dials again later). `deny` is called once
/// per dialer that proved the fleet secret, past its proofs, and never for a connection that
/// proved nothing.
pub fn serve_offering_with_denied<H, E>(
    listener: &TcpListener,
    listening: &Listening,
    offer: OfferFn,
    deny: DenyFn,
    stop: &Stop,
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
        stop,
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
    offer: OfferFn,
    deny: DenyFn,
    ask: Option<AskHandler>,
    stop: &Stop,
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
        stop,
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
    (listening, offer, deny): (&Listening, OfferFn, DenyFn),
    ask: Option<AskHandler>,
    stop: &Stop,
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
        stop,
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
    stop: &Stop,
    handshake_within: Duration,
    hand_over: H,
    on_event: E,
) where
    H: Fn(Served, FleetOp) -> io::Result<()> + Send + Sync + Clone + 'static,
    E: Fn(FleetEvent) + Send + Sync + Clone + 'static,
{
    serve_with_handshake_on(
        listener,
        listening,
        stop,
        handshake_within,
        (hand_over, on_event),
        (
            Arc::new(Slots::new(listening.slots, PREAUTH_GRACE)),
            Arc::default(),
        ),
    );
}

/// [`serve_with_handshake`] seating its connections in `slots`, which the caller keeps to read
/// who holds them, and holding them in `live`, whose clock runs their lifetimes.
#[cfg(any(test, debug_assertions))]
pub(crate) fn serve_with_handshake_on<H, E>(
    listener: &TcpListener,
    listening: &Listening,
    stop: &Stop,
    handshake_within: Duration,
    (hand_over, on_event): (H, E),
    (slots, live): (Arc<Slots>, Arc<LiveConnections>),
) where
    H: Fn(Served, FleetOp) -> io::Result<()> + Send + Sync + Clone + 'static,
    E: Fn(FleetEvent) + Send + Sync + Clone + 'static,
{
    let empty = Arc::new(Denied::default());
    let offering = (listening.scope, listening.serves);
    serve_seated(
        ListenerSocket::Fleet(listener),
        (
            listening,
            Arc::new(move || offering),
            Arc::new(move || Arc::clone(&empty)),
        ),
        None,
        stop,
        Locality::Fleet,
        handshake_within,
        (hand_over, on_event),
        (slots, live),
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
    stop: &Stop,
    hand_over: H,
    on_event: E,
) where
    H: Fn(Served, FleetOp) -> io::Result<()> + Send + Sync + Clone + 'static,
    E: Fn(FleetEvent) + Send + Sync + Clone + 'static,
{
    serve_lan_within(
        listener,
        listening,
        stop,
        HANDSHAKE_WITHIN,
        hand_over,
        on_event,
    );
}

/// [`serve_lan`], each connection's TLS and both proofs given `handshake_within` in place of
/// [`HANDSHAKE_WITHIN`]: a test's peer, which serves in process and so must finish each
/// handshake, gives it its hang detector.
#[cfg(unix)]
pub fn serve_lan_within<H, E>(
    listener: crate::lan::Listener,
    listening: &Listening,
    stop: &Stop,
    handshake_within: Duration,
    hand_over: H,
    on_event: E,
) where
    H: Fn(Served, FleetOp) -> io::Result<()> + Send + Sync + Clone + 'static,
    E: Fn(FleetEvent) + Send + Sync + Clone + 'static,
{
    let locality = Locality::Link(listener.scope);
    let empty = Arc::new(Denied::default());
    let offering = (listening.scope, listening.serves);
    serve_inner(
        ListenerSocket::Link(listener),
        (
            listening,
            Arc::new(move || offering),
            Arc::new(move || Arc::clone(&empty)),
        ),
        None,
        stop,
        locality,
        handshake_within,
        hand_over,
        on_event,
    );
}

// Workers own policy readers so a slow lookup holds its connection's slot,
// rather than blocking every subsequent accept. The original watchdog stays
// armed during the lookup, and SlotTenure retains capacity through the connection.
// The deny list is looked up only once a dialer has proved the fleet secret
// (`listener_proves`), so a socket whose peer gave up, or that proves nothing,
// costs its worker no lookup.
type OfferFn = Arc<dyn Fn() -> (FleetScope, &'static [FleetOp]) + Send + Sync>;
/// A verdict read fresh for each connection: what a listener refuses ([`Denied::under`]), and
/// what a dialer that meets the mesh judges a listener by ([`NodeIdentity::dialing_mesh`]).
pub type DenyFn = Arc<dyn Fn() -> Arc<Denied> + Send + Sync>;

/// Keep the borrowed descriptor alive until its watchdog joins, including unwinding a
/// policy callback or dropping an unstarted worker. Drop prevents disjoint closure capture.
struct ArmedHandshake {
    watchdog: Option<HandshakeWatchdog>,
    tcp: Option<TcpStream>,
}

impl ArmedHandshake {
    fn new(tcp: TcpStream, watchdog: HandshakeWatchdog) -> Self {
        Self {
            watchdog: Some(watchdog),
            tcp: Some(tcp),
        }
    }

    fn tcp(&self) -> &TcpStream {
        self.tcp.as_ref().expect("armed handshake owns its socket")
    }

    fn finish<T>(mut self, finish: impl FnOnce(TcpStream, HandshakeWatchdog) -> T) -> T {
        let tcp = self.tcp.take().expect("armed handshake owns its socket");
        let watchdog = self
            .watchdog
            .take()
            .expect("armed handshake owns its watchdog");
        finish(tcp, watchdog)
    }
}

impl Drop for ArmedHandshake {
    fn drop(&mut self) {
        drop(self.watchdog.take());
    }
}

#[allow(clippy::too_many_arguments)]
fn serve_inner<H, E>(
    listener: ListenerSocket<'_>,
    (listening, offer, deny): (&Listening, OfferFn, DenyFn),
    ask: Option<AskHandler>,
    stop: &Stop,
    locality: Locality,
    handshake_within: Duration,
    hand_over: H,
    on_event: E,
) where
    H: Fn(Served, FleetOp) -> io::Result<()> + Send + Sync + Clone + 'static,
    E: Fn(FleetEvent) + Send + Sync + Clone + 'static,
{
    serve_seated(
        listener,
        (listening, offer, deny),
        ask,
        stop,
        locality,
        handshake_within,
        (hand_over, on_event),
        (
            Arc::new(Slots::new(listening.slots, PREAUTH_GRACE)),
            Arc::default(),
        ),
    );
}

/// [`serve_inner`] seating its connections in `slots` and holding them in `live`, whose clock
/// runs their lifetimes.
#[allow(clippy::too_many_arguments)]
fn serve_seated<H, E>(
    listener: ListenerSocket<'_>,
    (listening, offer, deny): (&Listening, OfferFn, DenyFn),
    ask: Option<AskHandler>,
    stop: &Stop,
    locality: Locality,
    handshake_within: Duration,
    (hand_over, on_event): (H, E),
    (slots, live): (Arc<Slots>, Arc<LiveConnections>),
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
    #[cfg(unix)]
    let wake = match Wake::new().and_then(|wake| {
        let _ = stop.bell()?;
        let _ = listener.withdrawal_bell()?;
        Ok(wake)
    }) {
        Ok(wake) => wake,
        Err(error) => {
            drop(listener);
            live.cut_all();
            on_event(FleetEvent::Stopped(format!(
                "could not prepare fleet serving wake: {error}"
            )));
            return;
        }
    };
    #[cfg(unix)]
    live.set_wake(&wake.signal);
    let mut next_id: u64 = 0;
    let mut stopped = None;
    loop {
        #[cfg(unix)]
        if let Err(error) = wake.drain() {
            stopped = Some(format!("fleet live wake failed: {error}"));
            break;
        }
        if stop.is_set() {
            break;
        }
        if let Err(error) = listener.recheck() {
            stopped = Some(format!("listener withdrawn: {error}"));
            break;
        }
        let deadline = live.cut_overdue(listening.lifetime);
        let deadline = match (deadline, listener.validity_deadline()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let (tcp, peer) = match aterm_uds::accept(
            listener.tcp(),
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        ) {
            Ok(accepted) => accepted,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                if wait_serving(
                    &listener,
                    stop,
                    #[cfg(unix)]
                    &wake,
                    deadline,
                )
                .is_err()
                {
                    if let Err(error) = listener.recheck() {
                        stopped = Some(format!("listener withdrawn: {error}"));
                        break;
                    }
                    // Keep the existing error retry deadline without repeatedly polling a
                    // fault-ready listener, and let cancellation interrupt the backoff.
                    stop.sleep(ACCEPT_POLL);
                }
                continue;
            }
            Err(_) => {
                stop.sleep(ACCEPT_POLL);
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
        if !slots.make_room(source, stop) {
            if stop.is_set() {
                break;
            }
            on_event(FleetEvent::Refused {
                peer,
                reason: AT_CAPACITY.to_owned(),
            });
            continue;
        }
        let armed = aterm_uds::kept_out(|| tcp.try_clone())
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
        let offer = Arc::clone(&offer);
        let deny = Arc::clone(&deny);
        let hand_over = hand_over.clone();
        let ask = ask.clone();
        let tell = on_event.clone();
        #[cfg(unix)]
        let witness = listener.witness();
        let armed_conn = (ArmedHandshake::new(tcp, watchdog), release);
        let spawned = std::thread::Builder::new()
            .name("aterm-fleet-conn".to_owned())
            .spawn(move || {
                // Capture the aggregate before destructuring: an unstarted worker drops its
                // armed socket before releasing the slot, in the tuple's field order.
                let connection = armed_conn;
                let (armed, release) = connection;
                // When this connection's handshake began, on the clock of the call that notes a
                // mesh peer it proves.
                #[cfg(unix)]
                let began_ms = node
                    .as_ref()
                    .and_then(NodeIdentity::heard)
                    .map(heard::Heard::now_ms);
                let handshake: Result<_, FleetError> = (|| {
                    let admission = Admission::inspect(
                        armed.tcp(),
                        locality,
                        #[cfg(unix)]
                        witness,
                    )
                    .map_err(FleetError::Io)?;
                    let (scope, serves) = offer();
                    let accepted = armed.finish(|tcp, watchdog| {
                        server_handshake_or_ask(
                            tcp,
                            watchdog,
                            handshake_within,
                            (&secret, node.as_ref()),
                            (scope, serves, || deny()),
                            |op| {
                                if op == FleetOp::Neighbour {
                                    return release.tenure.linked();
                                }
                                let drive = op == FleetOp::Drive;
                                let share = if drive {
                                    drive_per_source(most)
                                } else {
                                    per_source
                                };
                                release.tenure.proved(drive, share)
                            },
                            served_config(ask.is_some(), node.is_some()),
                            ask.as_ref().map(|ask| (ask, peer)),
                        )
                    })?;
                    admission.recheck().map_err(FleetError::Io)?;
                    Ok((accepted, admission, scope))
                })();
                let event = match handshake {
                    Ok((Handshook::Asked, _, _)) => {
                        drop(release);
                        FleetEvent::Closed { peer, error: None }
                    }
                    Ok((Handshook::Proved(accepted), admission, scope)) => {
                        let Accepted {
                            transport,
                            op,
                            peer: proof,
                            mesh,
                        } = *accepted;
                        // A dialer the mesh proved is a member's node-proven answer (§11 B6).
                        #[cfg(unix)]
                        if mesh
                            && let (Some(heard), Some(began_ms), Some(proof)) = (
                                node.as_ref().and_then(NodeIdentity::heard),
                                began_ms,
                                proof.as_ref(),
                            )
                            && let Ok(local) = transport.tcp().local_addr()
                        {
                            heard.note(proof, (peer, local), began_ms);
                        }
                        tell(FleetEvent::Admitted { peer, op });
                        let served = Served {
                            transport,
                            release,
                            peer: proof,
                            scope,
                            admission,
                            mesh,
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

/// The connections a listener holds from their accept on: [`LiveConnections::cut_overdue`] cuts
/// each one a proof stamped ([`LiveTenure::proved`]) once the lifetime has passed on its clock,
/// and [`LiveConnections::cut_all`] cuts every one.
pub(crate) struct LiveConnections {
    fleet_live: Mutex<Vec<Live>>,
    /// The clock a proof's stamp ([`LiveTenure::proved`]) and the lifetime's cut
    /// ([`LiveConnections::cut_overdue`]) both read.
    now: Box<dyn Fn() -> Instant + Send + Sync>,
    advance: Option<aterm_types::advance_clock::Clock>,
    #[cfg(unix)]
    wake: Mutex<Option<Weak<Signal>>>,
    #[cfg(test)]
    looks: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    native_deadlines: bool,
}

impl Default for LiveConnections {
    fn default() -> Self {
        let mut live = Self::on_clock(Instant::now);
        #[cfg(test)]
        {
            live.native_deadlines = true;
        }
        if let Some(path) = aterm_types::dev_seam!("ATERM_DEBUG_FLEET_CLOCK") {
            live.advance = Some(aterm_types::advance_clock::Clock::new(
                Some(path),
                "lifetime",
            ));
        }
        live
    }
}

struct Live {
    id: u64,
    source: IpAddr,
    line: TcpStream,
    /// The local end a relay of this connection answers from ([`LiveTenure::relays_to`]).
    local: Option<aterm_uds::CtlStream>,
    proved: Option<Instant>,
    /// A proved neighbour link has no lifetime; its neighbour table bounds it.
    linked: bool,
    drive: bool,
}

impl Live {
    /// Hang up both ends: a relay holds its slot until its local end closes too, so a local server
    /// that never answers would otherwise outlive the cut.
    fn cut(&self) {
        aterm_uds::hangup::hang_up(&self.line);
        if let Some(local) = &self.local {
            aterm_uds::hangup::hang_up(local);
        }
    }
}

impl LiveConnections {
    /// Connections whose proofs are stamped, and whose lifetimes are cut, on `now`.
    fn on_clock(now: impl Fn() -> Instant + Send + Sync + 'static) -> Self {
        Self {
            fleet_live: Mutex::default(),
            now: Box::new(now),
            advance: None,
            #[cfg(unix)]
            wake: Mutex::default(),
            #[cfg(test)]
            looks: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            native_deadlines: false,
        }
    }

    #[cfg(unix)]
    fn set_wake(&self, signal: &Arc<Signal>) {
        *self.wake.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::downgrade(signal));
    }

    fn changed(&self) {
        #[cfg(unix)]
        {
            let signal = self
                .wake
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
                .and_then(Weak::upgrade);
            if let Some(signal) = signal {
                signal.ring();
            }
        }
    }

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
            local: None,
            proved: None,
            linked: false,
            drive: false,
        });
        LiveTenure {
            live: Arc::clone(self),
            id,
        }
    }

    fn cut_overdue(&self, lifetime: Duration) -> Option<Instant> {
        #[cfg(test)]
        self.looks.fetch_add(1, Ordering::Relaxed);
        #[cfg(any(test, debug_assertions))]
        let lifetime = if aterm_types::negative_control::on("fleet-fixed-lifetime") {
            SERVED_WITHIN
        } else {
            lifetime
        };
        let reading = self.advance.as_ref().map(|clock| clock.read());
        let now = reading
            .as_ref()
            .map_or_else(|| (self.now)(), |reading| reading.now);
        let mut deadline = None;
        self.lock().retain(|live| {
            if live.linked {
                return true;
            }
            let overdue = live
                .proved
                .is_some_and(|at| now.saturating_duration_since(at) >= lifetime);
            if overdue {
                live.cut();
            } else if let Some(at) = live.proved.and_then(|at| at.checked_add(lifetime)) {
                deadline = Some(deadline.map_or(at, |held: Instant| held.min(at)));
            }
            !overdue
        });
        if let Some(reading) = reading {
            deadline = deadline.map(|at| reading.wait_until(at));
            reading.observed();
        }
        #[cfg(test)]
        if !self.native_deadlines {
            // Still::pass advances this injected clock and rings the listener. Its absolute
            // instants do not run on native wall time, so native time cannot spend its budget.
            deadline = None;
        }
        deadline
    }

    fn cut_all(&self) {
        for live in std::mem::take(&mut *self.lock()) {
            live.cut();
        }
    }
}

struct LiveTenure {
    live: Arc<LiveConnections>,
    id: u64,
}

impl LiveTenure {
    /// This connection is relayed to `local` from here on: a cut reaches it too. If a cut
    /// already removed the connection, close this late attachment immediately.
    fn relays_to(&self, local: aterm_uds::CtlStream) {
        if let Some(live) = self.live.lock().iter_mut().find(|live| live.id == self.id) {
            live.local = Some(local);
        } else {
            aterm_uds::hangup::hang_up(&local);
        }
    }

    /// This connection proved a [`FleetOp::Neighbour`] link ([`Served::into_link`]): from its
    /// proof on, no lifetime cuts it and no source's share counts it, since a
    /// [`link::Neighbours`] table bounds links. The listener's stop still cuts it. `false` where
    /// a cut already removed it.
    fn linked(&self) -> bool {
        let mut held = self.live.lock();
        let Some(live) = held.iter_mut().find(|live| live.id == self.id) else {
            return false;
        };
        live.linked = true;
        drop(held);
        self.live.changed();
        true
    }

    fn proved(&self, drive: bool, most: usize) -> bool {
        let now = self
            .live
            .advance
            .as_ref()
            .map_or_else(|| (self.live.now)(), |clock| clock.read().now);
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
        drop(held);
        self.live.changed();
        true
    }
}

impl Drop for LiveTenure {
    fn drop(&mut self) {
        self.live.lock().retain(|live| live.id != self.id);
        self.live.changed();
    }
}

fn arm(tcp: &TcpStream, within: Duration) -> Result<HandshakeWatchdog, FleetError> {
    if within.is_zero() {
        return Err(FleetError::Timeout(within));
    }
    tcp.set_nonblocking(false).map_err(FleetError::Io)?;
    // Fleet and database protocols split short frames across writes and then
    // wait for replies. Send those writes without waiting for an earlier ACK.
    tcp.set_nodelay(true).map_err(FleetError::Io)?;
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

/// The channel's exporter under the label of what its ends agreed to speak: a mesh proof's never
/// a fleet proof's.
fn export<C, S>(conn: &C, spoken: Spoken) -> Result<[u8; EXPORTER_LEN], FleetError>
where
    C: std::ops::Deref<Target = rustls::ConnectionCommon<S>>,
{
    let label = if spoken == Spoken::Mesh {
        MESH_EXPORTER_LABEL
    } else {
        EXPORTER_LABEL
    };
    conn.export_keying_material([0u8; EXPORTER_LEN], label, None)
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
    let exporter = export(&conn, spoken)?;
    Ok((conn, exporter, spoken))
}

/// What a connection's two ends agreed to speak.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Spoken {
    /// The fleet handshake, with node proofs under [`ALPN_NODES`] or [`ALPN_DATABASE_REPAIR`].
    Fleet { nodes: bool },
    /// The mesh handshake, under [`ALPN_MESH`]: node proofs alone, the listener's first.
    Mesh,
    /// A Mac asking to join, under [`ALPN_ASK`]: no secret, no proof, no op.
    Ask,
}

const fn spoken(alpn: Option<&[u8]>) -> Option<Spoken> {
    match alpn {
        Some(
            ALPN_NODES
            | ALPN_DATABASE_REPAIR
            | ALPN_DATABASE_CREDENTIALS
            | ALPN_DATABASE_FRONTIER
            | ALPN_DATABASE_DELTA,
        ) => Some(Spoken::Fleet { nodes: true }),
        Some(ALPN) => Some(Spoken::Fleet { nodes: false }),
        Some(ALPN_MESH) => Some(Spoken::Mesh),
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
    let exporter = export(&conn, spoken)?;
    Ok((conn, exporter, spoken))
}

/// The listener's half of the proofs. `deny` is looked up here, past both proofs of the secret and
/// any node proof the dialer gave, where its answer is first needed: never for a dialer that
/// proved nothing.
fn listener_proves<S: Read + Write, D: std::ops::Deref<Target = Denied>>(
    stream: &mut S,
    exporter: &[u8],
    (secret, me, nodes): (&FleetSecret, Option<&NodeIdentity>, bool),
    scope: FleetScope,
    serves: &[FleetOp],
    deny: impl FnOnce() -> D,
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
    let denied = deny();
    if denied.unread {
        // `NO capacity`, not `NO unread`: a dialer older than this build reads any other `NO
        // <word>` as a refused proof ("another fleet's secret") and disowns this node.
        return refuse_as(stream, "capacity", FleetError::DenyListUnread, node);
    }
    if denied.refuses(peer.as_ref()) {
        return refuse_as(stream, "denied", FleetError::DeniedNode, node);
    }
    let Some(op) = FleetOp::from_word(word).filter(|op| !op.mesh_only() && serves.contains(op))
    else {
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
        Some("NO unread") => Some(FleetError::DenyListUnread),
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

/// The listener's half of [`ALPN_MESH`]. It proves its node `me` first, over this channel's
/// exporter and bound to the listener's role, and the level it serves; then reads the dialer's op
/// and node proof, and judges the key that proof proved by `deny`'s verdict ([`Denied::seat`]),
/// looked up only once that proof verified: a dialer the roster or the log refuses is answered
/// `NO denied`, one whose key the log binds to no seat `NO unknown` (it reaches only the desk, on
/// [`ALPN_ASK`]), and an unread verdict `NO capacity`, as under [`ALPN_NODES`]. A proof that does
/// not verify, or verifies only as the listener's own role (a reflection), is answered `NO node`.
fn listener_meets<S: Read + Write, D: std::ops::Deref<Target = Denied>>(
    stream: &mut S,
    exporter: &[u8],
    me: &NodeIdentity,
    (scope, serves): (FleetScope, &[FleetOp]),
    deny: impl FnOnce() -> D,
    admit: impl FnOnce(FleetOp) -> bool,
) -> Result<(FleetOp, Option<NodeProof>), FleetError> {
    send(
        stream,
        &format!(
            "{}{}\n",
            node_line(Some(me), Signer::MeshListener, exporter),
            scope.word()
        ),
    )?;
    let Some(word) = read_line(stream)? else {
        return Err(FleetError::DialerHungUp);
    };
    if !is_word(&word) {
        return refuse(
            stream,
            "op",
            FleetError::Malformed("the dialer's op is not a word"),
        );
    }
    let peer = match read_node_proof(stream, Signer::MeshDialer, exporter) {
        Ok(Some(peer)) => peer,
        Ok(None) => {
            return refuse(
                stream,
                "node",
                FleetError::Malformed("a mesh dialer proves a node"),
            );
        }
        Err(
            error @ (FleetError::NodeProof | FleetError::Reflection | FleetError::Malformed(_)),
        ) => {
            return refuse(stream, "node", error);
        }
        Err(error) => return Err(error),
    };
    match deny().seat(&peer) {
        Seat::Member => {}
        // `NO capacity`, as under [`ALPN_NODES`]: dial again once the verdict is read.
        Seat::Unread => return refuse(stream, "capacity", FleetError::DenyListUnread),
        Seat::Out => return refuse(stream, "denied", FleetError::DeniedNode),
        Seat::Unknown => return refuse(stream, "unknown", FleetError::UnknownKey),
    }
    let Some(op) = FleetOp::from_word(&word).filter(|op| op.meshed() && serves.contains(op)) else {
        return refuse(stream, "op", FleetError::UnknownOp(word));
    };
    if !admit(op) {
        return refuse(stream, "capacity", FleetError::AddressBusy);
    }
    send(stream, "OK\n")?;
    Ok((op, Some(peer)))
}

/// The dialer's half of [`ALPN_MESH`]. It reads the listener's node proof and checks it: a
/// signature that does not verify is [`FleetError::NodeProof`], and one that verifies only as the
/// dialer's own role [`FleetError::Reflection`]. Then it judges the key that proof proved by `me`'s
/// verdict, read now ([`NodeIdentity::dialing_mesh`]), and proves `op` and its own node only to a
/// listener that verdict seats a member under: to any other, this end sends nothing at all, so a
/// client never names itself to an unproven server. The listener's node, proved and judged, is
/// `refused_by` when it refuses this end.
fn dialer_meets<S: Read + Write>(
    stream: &mut S,
    exporter: &[u8],
    me: &NodeIdentity,
    op: FleetOp,
    refused_by: &mut Option<NodeProof>,
) -> Result<(FleetScope, NodeProof), FleetError> {
    let Some(listener) = read_node_proof(stream, Signer::MeshListener, exporter)? else {
        return Err(FleetError::Malformed(
            "a mesh listener proves its node first",
        ));
    };
    match me.verdict().map(|verdict| verdict.seat(&listener)) {
        Some(Seat::Member) => {}
        Some(Seat::Out) => return Err(FleetError::ListenerOut),
        Some(Seat::Unknown) | None => return Err(FleetError::ListenerUnknown),
        Some(Seat::Unread) => return Err(FleetError::VerdictUnread),
    }
    let Some(word) = read_line(stream)? else {
        return Err(FleetError::Io(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the listener closed the connection before naming its level",
        )));
    };
    let Some(scope) = FleetScope::from_word(&word) else {
        return Err(FleetError::UnknownScope(word));
    };
    send(
        stream,
        &format!(
            "{}\n{}",
            op.word(),
            node_line(Some(me), Signer::MeshDialer, exporter)
        ),
    )?;
    let refusal = match read_line(stream)?.as_deref() {
        Some("OK") => return Ok((scope, listener)),
        Some("NO op") => FleetError::UnknownOp(op.word().to_owned()),
        Some("NO capacity") => FleetError::AddressBusy,
        Some("NO denied") => FleetError::DeniedNode,
        Some("NO unread") => FleetError::DenyListUnread,
        Some("NO unknown") => FleetError::UnknownKey,
        Some("NO node") => FleetError::NodeProof,
        Some(verdict) if verdict.strip_prefix("NO ").is_some_and(is_word) => FleetError::Refused,
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
    *refused_by = Some(listener);
    Err(refusal)
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

#[cfg(all(test, unix))]
#[path = "fleet_mesh_tests.rs"]
mod mesh_tests;

#[cfg(unix)]
pub mod heard;
pub mod link;

#[cfg(test)]
#[path = "fleet_tests.rs"]
mod tests;
