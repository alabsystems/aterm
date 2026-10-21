// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fleet handshake (`docs/DESIGN-sessions-as-subagents-2026-10-15.md` §5):
//! how one aterm proves to another, over TCP, that both hold the same fleet
//! secret, before a single control byte moves and with no token on the wire.
//!
//! It is the only way one aterm reaches another over the network: one secret
//! per fleet, one certificate in every aterm, and a proof each way. One secret
//! is one read trust domain — every member can read every other's survey at
//! the level that machine allows, and nothing more; nothing it admits is ever
//! the Owner.
//!
//! **Keys.** The fleet secret `K` is 20 to 32 bytes
//! ([`FleetSecret`](crate::fleet::FleetSecret); a join code carries 160 bits).
//! Each role proves with its own key, derived from `K` with HKDF-SHA256
//! (RFC 5869, no salt):
//!
//! | key  | HKDF info               | proves                                     |
//! |------|-------------------------|--------------------------------------------|
//! | `Ks` | `aterm-fleet/v1 server` | the listener's scope                       |
//! | `Kc` | `aterm-fleet/v1 client` | a dialer's op (`survey`, `book`, `drive`)  |
//! | `Kj` | `aterm-fleet/v1 join`   | a dialer's `join`, keyed apart from a read |
//!
//! **Wire.** TLS 1.3 only, ALPN [`ALPN`](crate::fleet::ALPN) (`atf/1`), no
//! resumption and no 0-RTT. `E` is the connection's 32-byte RFC 5705 exporter
//! under the label `EXPORTER-aterm-fleet-v1`, with no context. Then three
//! lines, each ASCII, ending in LF, at most 128 bytes:
//!
//! ```text
//! listener → dialer   <scope> SP hex(HMAC-SHA256(Ks, E ‖ scope)) LF    scope: facts | content
//! dialer → listener   <op> SP hex(HMAC-SHA256(Kc, E ‖ op)) LF          op: survey | book | drive | join (Kj)
//! listener → dialer   OK LF  |  NO SP proof LF  |  NO SP op LF  |  NO SP capacity LF
//! ```
//!
//! A word is `[a-z0-9-]{1,32}`, a tag 64 lowercase hex digits. `E` has a fixed
//! length, so `E ‖ word` is unambiguous. After `OK` the stream is the caller's:
//! the listener hands it to its control server, the dialer speaks its op.
//!
//! **Nodes** ([`NodeIdentity`], [`NodeProof`]). The secret says a peer is OF
//! the fleet, never WHICH machine of it: every member holds it. So each node
//! holds an Ed25519 key of its own too (`fleet_member::NODE_KEY_FILE`), which
//! the issuer's book binds to its node id at its first registration, and each
//! end proves it holds the private half by signing `E`. A dialer offers ALPN
//! [`ALPN_NODES`] (`atf/2`) before `atf/1`, a listener prefers it; under it
//! two lines more, each at most 256 bytes, each sent only to a peer that has
//! already proved the secret:
//!
//! ```text
//! dialer → listener   (after its op line)  node SP <node id> SP hex(K) SP hex(Sig(E)) LF  |  node SP - LF
//! listener → dialer   (after its OK)       node SP <node id> SP hex(K) SP hex(Sig(E)) LF  |  node SP - LF
//! ```
//!
//! `Sig(E)` is the node key's Ed25519 signature over its end's label
//! (`aterm-fleet/v2 node dialer` or `… listener`), a zero byte, `E` and the
//! node id: each end's proof is its own, of its own node, on this channel
//! alone. `node -` is an end that holds no node key (it has not joined since
//! keys came). A signature that does not verify is refused (`NO node`); a
//! valid one is a proof of POSSESSION, and which node it makes the peer is
//! the book's word ([`NodeProof`]). Under `atf/1` — an older aterm on either
//! end — no node is proved, and a reader whose book binds that peer a key
//! does not read it as that peer (`fleet_member`).
//!
//! **Why this order.** The listener proves first, and the dialer checks that
//! proof in constant time before it sends a byte: a dialer never hands its
//! proof to a peer that has not shown it holds the secret, so an echo has
//! nothing to send back. Each role has its own key, so the listener's tag
//! reflected at it never passes as a dialer's, nor a dialer's as a listener's.
//! Every tag covers `E`, which a man in the middle cannot carry across: it runs
//! a separate TLS session on each leg, each with its own exporter, so a tag it
//! relays from one leg fails on the other. And a tag names its word, so a proof
//! of one op or scope is no proof of another.
//!
//! **The certificate is not a secret** ([`CERT_DER`](crate::fleet::CERT_DER)):
//! every aterm carries it and its private key. TLS gives the channel its
//! encryption, from the ephemeral key exchange (which a public signing key does
//! not weaken), and an exporter bound to the handshake transcript;
//! authentication is the exporter-bound HMAC. The dialer still pins that one
//! certificate, so a listener that is not a fleet listener fails at once and
//! by name, and still verifies the TLS 1.3 handshake signature.
//!
//! **The listener** ([`serve`]) accepts only from loopback, private and
//! link-local addresses ([`is_fleet_source`]), holds at most a fixed number of
//! connections at once — a connection that has proved nothing gives its slot
//! up to a newcomer under the NET-1 rule, so idle sockets never lock a member
//! out (on an IPv4 listener; on IPv6 a LAN is one address, [`serve`] says
//! why) — gives each [`HANDSHAKE_WITHIN`] for TLS and both proofs on a
//! thread of its own, and hands each proved stream to its owner for at most
//! [`SERVED_WITHIN`]; clearing its flag cuts every connection it handed over.
//! A dialer proving an op the listener does not serve is answered `NO op`, and
//! one whose address already holds its share of proved connections
//! ([`Listening::per_source`]; a drive's apart, [`drive_per_source`]) `NO
//! capacity`: one member, which holds the secret every member holds, never
//! fills the slots the others read through.
//! A proved connection goes to its owner as a [`Served`], whose two ends
//! are its only ones, and a connection that ends on its own — its control
//! server's answer and hang-up, or a join's answer — frees its slot and share
//! before its peer hears the end: a member that has read the end of a served
//! connection can dial again at once. Whatever of an end waits on the peer
//! comes before that, and nothing after it waits on either end, so a
//! connection that no longer counts holds no thread or descriptor the peer
//! can keep; a relayed one counts until its control server has answered the
//! line it was serving, however the peer ended it. One the listener cuts (past its lifetime, or as it stops or
//! rekeys) hears the cut first: its share goes with the cut, its slot as its
//! end runs its course. A refused one hears its `NO <why>` before its slot is
//! freed: a refusal is not undone by dialling again at once.
//!
//! **The key file** ([`KEY_FILE`], [`read_key_file`]): a machine's fleet
//! secret is `<state>/fleet.key`, the secret in lowercase hex — 40 to 64
//! digits, 20 to 32 bytes — on one line (a final LF optional, nothing else).
//! It is a regular file with one link, owned by this user, mode 0600, directly
//! in the state directory, which is this user's, mode 0700; neither is a
//! symlink. Anything else is refused and never repaired: `aterm fleet
//! init|join` writes it, and every reader of it reads it here.

use std::fmt;
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use aterm_codec::hex;
use aterm_digest::HmacSha256;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair as _, UnparsedPublicKey};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use rustls::{ClientConfig, ClientConnection, ServerConfig, ServerConnection, StreamOwned};

use crate::slots::{
    ACCEPT_POLL, Claim, DISPLACED, HandshakeWatchdog, Slots, arm_unauthenticated, source_of,
    wait_listener_readable,
};
use crate::tls::{self, EXPORTER_LEN, PinnedServerVerifier, TlsTransport};

/// The ALPN protocol every fleet connection negotiates, or [`ALPN_NODES`]. A
/// peer that negotiates neither is no fleet peer ([`FleetError::Alpn`]).
pub const ALPN: &[u8] = b"atf/1";

/// The ALPN under which both ends prove their node as well as the secret
/// (the module doc's **Nodes**): offered first by every dialer and preferred
/// by every listener, so two aterms that know it speak it and one older end
/// is spoken to under [`ALPN`], proving no node.
pub const ALPN_NODES: &[u8] = b"atf/2";

/// The protocols a fleet end offers or serves, the preferred first.
const PROTOCOLS: [&[u8]; 2] = [ALPN_NODES, ALPN];

/// The fleet's RFC 5705 exporter label: every proof is taken over keying
/// material exported under it alone, apart from an older aterm's network
/// drive (`EXPORTER-aterm-net-capability-v1`), so no tag of one is ever a tag
/// of the other.
const EXPORTER_LABEL: &[u8] = b"EXPORTER-aterm-fleet-v1";

/// The fleet's one certificate: DER, Ed25519, self-signed, `CN=aterm-fleet`,
/// valid 2026-10-15 to 2126-10-16. Its fingerprint — the SHA-256 digest of
/// these DER bytes, which a dialer pins — is
/// `a1159e84212477a69a2e6ce663db7c6796d0456a6150446469e90f832f381926`; the
/// certificate itself is signed with Ed25519, not with SHA-256.
///
/// **NOT A SECRET** — every aterm carries it, and its private key
/// (`fleet_key.der`). Authentication is the exporter-bound HMAC, never this
/// certificate: a man in the middle holds a different exporter on each leg. A
/// dialer pins its fingerprint ([`client_config`]), so replacing it splits the
/// fleet between the aterms that carry the old one and the new. Minted once,
/// with OpenSSL 3.6:
///
/// ```text
/// openssl genpkey -algorithm ed25519 -outform DER -out fleet_key.der
/// openssl req -x509 -key fleet_key.der -keyform DER -days 36525 -subj /CN=aterm-fleet \
///   -addext basicConstraints=critical,CA:FALSE -addext keyUsage=critical,digitalSignature \
///   -addext extendedKeyUsage=serverAuth -addext subjectAltName=DNS:aterm-fleet \
///   -outform DER -out fleet_cert.der
/// ```
pub const CERT_DER: &[u8] = include_bytes!("fleet_cert.der");

/// [`CERT_DER`]'s private key, PKCS#8 DER. **NOT A SECRET**, for the same
/// reason: every aterm carries it.
const KEY_DER: &[u8] = include_bytes!("fleet_key.der");

/// The name a dialer sends (SNI): the certificate's. Identity is the proof,
/// never the name.
const SERVER_NAME: &str = "aterm-fleet";

/// The longest handshake line: the longest word, a space, a 64-digit tag and
/// the LF fit with room to spare.
const MAX_LINE: usize = 128;

/// The longest word (a scope, an op, a verdict's reason).
const MAX_WORD: usize = 32;

/// The tag length: HMAC-SHA256.
const TAG_LEN: usize = 32;

/// The longest node line (the module doc's **Nodes**): `node`, a 32-digit
/// node id, a 64-digit key and a 128-digit signature, spaces and the LF, with
/// room to spare.
const MAX_NODE_LINE: usize = 256;

/// An Ed25519 signature's length.
const SIG_LEN: usize = 64;

/// What a fleet listener serves a dialer whose proof verified: the level its
/// `[fleet] allow` names (design §3). The listener proves it, so the dialer
/// learns it from a peer it has authenticated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FleetScope {
    /// Facts only: the program, the agent's state, the task record, the model;
    /// no text from a screen or a transcript.
    Facts,
    /// Facts and content: titles, cwd, prompts, the agent's newest words.
    Content,
}

impl FleetScope {
    /// The scope's word on the wire.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Facts => "facts",
            Self::Content => "content",
        }
    }

    /// The scope a wire word names, if this aterm knows it.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        [Self::Facts, Self::Content]
            .into_iter()
            .find(|scope| scope.word() == word)
    }
}

/// `[fleet] allow = "drive"`: the level above content (the step-in,
/// `docs/DESIGN-sessions-as-subagents-2026-10-15.md` §5.1). Its reads are
/// [`FleetScope::Content`]'s, which is the scope the listener proves; it
/// also serves [`FleetOp::Drive`], to the nodes `[fleet] drive_from` names
/// alone. No scope on the wire: a level only this Mac's person sets.
pub const DRIVE_LEVEL: &str = "drive";

/// The most of a [`FleetOp::Survey`] reply a peer reads: the reader refuses
/// more (`fleet_member::survey_all`), so a listener's survey answers its
/// fleet within it, its least pressing sessions' excerpts cut first.
pub const REPLY_MAX: usize = 256 * 1024;

/// What a dialer asks of a fleet listener.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FleetOp {
    /// Read the listener's survey.
    Survey,
    /// Read the fleet's issuer's book of peers
    /// (`crate::fleet_member::serve_book`), so a member's read reaches every
    /// member, not only its issuer and the peers named on it by hand. Served
    /// at every level — the book is facts: node ids, host names, addresses,
    /// ports, versions — and proved with the client key, as a survey is. An
    /// older listener answers it `NO op` ([`FleetError::UnknownOp`]), and the
    /// member reads the peers it knows, as it did before.
    Book,
    /// Register with the fleet's issuer (`aterm fleet join`). Proved with the
    /// join key, so a read's proof is never a registration's.
    Join,
    /// Drive the listener's sessions: the step-in
    /// (`docs/DESIGN-sessions-as-subagents-2026-10-15.md` §5.1). Proved with
    /// the client key, as a read is; what makes it more than a read is the
    /// listener's: it serves the op only where its person allows driving
    /// (`[fleet] allow = "drive"`), and admits a connection only from a node
    /// it binds a key to and names on its drive list
    /// (`crate::fleet_member::proven_peer`). Elsewhere, and at an older
    /// listener, it is `NO op` ([`FleetError::UnknownOp`]).
    Drive,
}

impl FleetOp {
    /// The op's word on the wire.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Survey => "survey",
            Self::Book => "book",
            Self::Join => "join",
            Self::Drive => "drive",
        }
    }

    /// The op a wire word names, if this aterm knows it.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        [Self::Survey, Self::Book, Self::Join, Self::Drive]
            .into_iter()
            .find(|op| op.word() == word)
    }
}

/// A fleet secret, held as the three role keys derived from it (the module
/// doc's table). The secret itself is not kept, and no key is ever printed.
#[derive(Clone)]
pub struct FleetSecret {
    /// `Ks`: the listener's proof.
    server: [u8; 32],
    /// `Kc`: a dialer's proof of any op but `join`.
    client: [u8; 32],
    /// `Kj`: a dialer's proof of `join`.
    join: [u8; 32],
}

impl FleetSecret {
    /// The shortest secret accepted: 160 bits, what a join code carries.
    pub const MIN_LEN: usize = 20;
    /// The longest secret accepted: 32 bytes, the length of the HKDF-SHA256
    /// pseudorandom key every role key is expanded from, so a longer secret
    /// would hold no more than that.
    pub const MAX_LEN: usize = 32;

    /// Derive the role keys from `secret`.
    ///
    /// # Errors
    /// [`SecretLength`] when `secret` is shorter than [`Self::MIN_LEN`] or
    /// longer than [`Self::MAX_LEN`].
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

    /// The key a dialer proves the op `word` with: `join` has its own.
    fn dialer_key(&self, word: &str) -> &[u8; 32] {
        if word == FleetOp::Join.word() {
            &self.join
        } else {
            &self.client
        }
    }
}

impl PartialEq for FleetSecret {
    /// Whether both are one secret: every role key compared in constant time.
    fn eq(&self, other: &Self) -> bool {
        aterm_digest::ct_eq(&self.server, &other.server)
            & aterm_digest::ct_eq(&self.client, &other.client)
            & aterm_digest::ct_eq(&self.join, &other.join)
    }
}

impl Eq for FleetSecret {}

impl fmt::Debug for FleetSecret {
    /// Opaque on purpose: every field is a key.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FleetSecret(..)")
    }
}

/// A fleet secret of a length [`FleetSecret::new`] refuses. Carries the
/// length, never a byte.
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

/// A node's public key: Ed25519, 32 bytes — what the issuer's book binds a
/// node id to at its first registration (`fleet_member`), and what a peer
/// proves it holds the private half of ([`NodeProof`]). Not a secret; never
/// an identity by itself.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeKey([u8; NodeKey::LEN]);

impl NodeKey {
    /// A public key's length.
    pub const LEN: usize = 32;

    /// The key's bytes.
    #[must_use]
    pub const fn bytes(&self) -> &[u8; Self::LEN] {
        &self.0
    }

    /// The key in its one spelling: 64 lowercase hex digits.
    #[must_use]
    pub fn hex(&self) -> String {
        hex::encode_lower(&self.0)
    }

    /// The key `word` spells: 64 lowercase hex digits, nothing else.
    #[must_use]
    pub fn from_hex(word: &str) -> Option<Self> {
        if !hex::is_lower(word, 2 * Self::LEN) {
            return None;
        }
        crate::hex_decode(word)?.try_into().ok().map(Self)
    }

    /// Its first eight digits, as a row shows a key.
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

/// ONE DRIVE GRANT, as `[fleet] drive_from` holds it (the step-in,
/// `docs/DESIGN-sessions-as-subagents-2026-10-15.md` §5.1): a node, and the
/// key this machine's book bound it to when its person let it drive, spelt
/// `<node id>:<key hex>`. A node id is a label anyone may name beside a key
/// of their own ([`NodeProof`]), and a book's binding of it can change — a
/// `peers rm` on the issuer, and a join again under that id with another
/// key — so the grant holds the key the person approved: a Mac that comes
/// back under another key drives nothing until its person here grants it
/// again. It lives here, beside [`NodeKey`], and not in the Unix-only
/// `fleet_member`: every platform reads its `[fleet]` table, even one that
/// serves no fleet port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriveGrant {
    /// The node id.
    pub node: String,
    /// The key it was bound to when the grant was made.
    pub key: NodeKey,
}

impl DriveGrant {
    /// Its one spelling: `<node id>:<key hex>`.
    #[must_use]
    pub fn word(&self) -> String {
        format!("{}:{}", self.node, self.key.hex())
    }

    /// The grant `word` spells, or `None`: a bare node id names no key, so
    /// it grants nothing.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        let (node, key) = word.trim().split_once(':')?;
        let node = node.to_ascii_lowercase();
        if !hex::is_lower(&node, 32) {
            return None;
        }
        Some(Self {
            node,
            key: NodeKey::from_hex(&key.to_ascii_lowercase())?,
        })
    }

    /// The node a `drive_from` word names, granted or not (its text before
    /// any `:`, in lowercase): what a `peers rm` of that node takes out.
    #[must_use]
    pub fn node_of(word: &str) -> String {
        let word = word.trim();
        word.split_once(':')
            .map_or(word, |(node, _)| node)
            .to_ascii_lowercase()
    }
}

/// THIS machine's node: its node id (`fleet_member::NODE_FILE`) and its
/// Ed25519 key pair, from the seed `fleet_member::NODE_KEY_FILE` holds. The
/// private half never leaves it: it only signs an end's node proof on a
/// channel ([`client_handshake_as`], [`Listening::node`]).
#[derive(Clone)]
pub struct NodeIdentity {
    node: String,
    key: NodeKey,
    pair: Arc<Ed25519KeyPair>,
}

impl NodeIdentity {
    /// The node `node` (32 lowercase hex digits) with the key pair `seed`
    /// gives: `None` for a node id that is none, or a seed that is not 32
    /// bytes.
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

    /// Its node id.
    #[must_use]
    pub fn node(&self) -> &str {
        &self.node
    }

    /// Its public key.
    #[must_use]
    pub const fn key(&self) -> NodeKey {
        self.key
    }

    /// Its proof as `end` on the channel whose exporter is `exporter`.
    fn sign(&self, end: Signer, exporter: &[u8]) -> [u8; SIG_LEN] {
        let signed = self.pair.sign(&node_message(end, exporter, &self.node));
        let mut sig = [0u8; SIG_LEN];
        sig.copy_from_slice(signed.as_ref());
        sig
    }
}

impl fmt::Debug for NodeIdentity {
    /// Its node and key; the key pair is opaque on purpose.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NodeIdentity({} {:?})", self.node, self.key)
    }
}

/// What a peer PROVED of its node on one connection: the node id it named
/// and the key it showed it holds the private half of, by a signature over
/// this channel's exporter (the module doc's **Nodes**). That it holds the
/// key is proved; that the key is that node's is the book's to say
/// (`fleet_member::Keyring::proven`): anyone may mint a key and name any
/// node id beside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeProof {
    /// The node id it named.
    pub node: String,
    /// The key it proved it holds.
    pub key: NodeKey,
}

/// Which end makes a node proof: each signs under its own label, so one
/// end's proof is never the other's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Signer {
    Dialer,
    Listener,
}

impl Signer {
    const fn label(self) -> &'static [u8] {
        match self {
            Self::Dialer => b"aterm-fleet/v2 node dialer",
            Self::Listener => b"aterm-fleet/v2 node listener",
        }
    }
}

/// What `end`'s node key signs on the channel whose exporter is `exporter`:
/// its label, a zero byte, the exporter and the node id. The label holds no
/// zero byte and the exporter and node id have fixed lengths, so no two
/// messages are one.
fn node_message(end: Signer, exporter: &[u8], node: &str) -> Vec<u8> {
    let mut message = end.label().to_vec();
    message.push(0);
    message.extend_from_slice(exporter);
    message.extend_from_slice(node.as_bytes());
    message
}

/// `end`'s node line on this channel: `me`'s proof, or `node -` for an end
/// that holds no node key.
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

/// Read `end`'s node line on this channel and check its signature: the
/// proof, or `None` for `node -`.
///
/// # Errors
/// [`FleetError::NodeProof`] for a signature that does not verify,
/// [`FleetError::Malformed`] for a line that is no node line, or the read's
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

/// Why a fleet handshake did not end in an authenticated stream. Each variant
/// says which end saw it; none carries a secret or a tag.
#[derive(Debug)]
pub enum FleetError {
    /// The dialer found the listener's proof wrong: the listener holds another
    /// fleet's secret, or a man in the middle runs this connection. The dialer
    /// sent nothing.
    ListenerWrongSecret,
    /// The listener found the dialer's proof wrong: the dialer holds another
    /// fleet's secret. The listener answered `NO proof`.
    DialerWrongSecret,
    /// The listener's view of a dialer that hung up on its proof without a
    /// byte: what an honest dialer of another fleet does
    /// ([`ListenerWrongSecret`](Self::ListenerWrongSecret) on its side), and
    /// any client that is not a fleet dialer.
    DialerHungUp,
    /// The dialer's view of a listener that answered its proof `NO proof`.
    Refused,
    /// The dialer proved itself, and its address already held as many proved
    /// connections as the listener serves one address at once
    /// ([`Listening::per_source`]): `NO capacity`, seen from either end.
    AddressBusy,
    /// A peer presented a tag of this end's own role: the listener's own proof
    /// sent back to it as a dialer's, or a "listener" proving with a dialer's
    /// key. The role keys refuse it; it is named apart so a log says what
    /// happened.
    Reflection,
    /// The listener does not serve the op the dialer proved: one end is an
    /// older aterm (a `join` at an aterm from before members registered), or
    /// the listener takes no such op.
    UnknownOp(String),
    /// The listener proved a scope the dialer does not know: the dialer is an
    /// older aterm.
    UnknownScope(String),
    /// A node proof did not verify (the module doc's **Nodes**): the peer's
    /// signature is not its key's over this channel — a broken or forged
    /// aterm, since an honest one signs what it sends. The listener answers
    /// `NO node`; the dialer's view of that answer is this too.
    NodeProof,
    /// The peer negotiated neither ALPN `atf/2` nor `atf/1`. On the dialer: a pre-fleet
    /// aterm listener (its own `[net]` certificate and no ALPN), or not an
    /// aterm — a caller says "pre-fleet aterm listener (update it)". On the
    /// listener: a dialer that is no fleet client.
    Alpn,
    /// A handshake line the protocol does not allow.
    Malformed(&'static str),
    /// The TLS handshake failed for a reason other than ALPN.
    Tls(io::Error),
    /// The handshake did not finish within its deadline.
    Timeout(Duration),
    /// The connection failed: a read or write error, or the peer closing
    /// mid-handshake.
    Io(io::Error),
}

impl FleetError {
    /// Whether a socket deadline, not the peer, ended a read or write.
    fn is_deadline(&self) -> bool {
        match self {
            Self::Tls(e) | Self::Io(e) => matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ),
            _ => false,
        }
    }
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

/// The listener's TLS configuration: [`CERT_DER`], TLS 1.3 only, ALPN
/// [`ALPN_NODES`] preferred, else [`ALPN`], no session tickets, no session
/// cache, no early data. Built once.
#[must_use]
pub fn server_config() -> Arc<ServerConfig> {
    static CONFIG: OnceLock<Arc<ServerConfig>> = OnceLock::new();
    Arc::clone(CONFIG.get_or_init(|| {
        tls::init_crypto();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KEY_DER));
        // TLS 1.3 only: its exporter is bound to the whole handshake
        // transcript, which a TLS 1.2 session without extended master secret
        // (RFC 7627) is not.
        let mut cfg = ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_no_client_auth()
            .with_single_cert(vec![CertificateDer::from(CERT_DER)], key)
            .expect("the embedded fleet certificate and key are one Ed25519 pair");
        cfg.alpn_protocols = PROTOCOLS.iter().map(|alpn| alpn.to_vec()).collect();
        // No resumption and no 0-RTT: every connection is a full handshake
        // with a fresh key exchange, and no byte is accepted before it ends.
        cfg.send_tls13_tickets = 0;
        cfg.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});
        cfg.max_early_data_size = 0;
        Arc::new(cfg)
    }))
}

/// The dialer's TLS configuration: TLS 1.3 only, ALPN [`ALPN_NODES`] offered
/// before [`ALPN`], exactly [`CERT_DER`] accepted (by its fingerprint) with
/// the handshake signature still verified, no resumption, no early data.
/// Built once.
#[must_use]
pub fn client_config() -> Arc<ClientConfig> {
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

/// Listener side: complete TLS on an accepted `tcp`, prove this machine holds
/// `secret` and serves `scope`, then verify the dialer's proof and answer it —
/// `OK` for an op in `serves`, `NO op` for any other — all within `within`, a
/// wall-clock deadline a peer that dribbles bytes cannot stretch. Returns the
/// authenticated transport, its socket deadlines cleared, and the op the
/// dialer proved. This end proves no node ([`server_handshake_as`] does).
///
/// # Errors
/// A [`FleetError`] naming what failed; the connection is then the caller's to
/// drop, and nothing past the handshake was read from it.
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

/// A listener's end of a proved connection ([`server_handshake_as`]).
pub struct Accepted {
    /// The authenticated transport, its socket deadlines cleared.
    pub transport: TlsTransport<ServerConnection>,
    /// The op the dialer proved.
    pub op: FleetOp,
    /// The node the dialer proved it holds the key of, when both ends speak
    /// [`ALPN_NODES`] and it holds one.
    pub peer: Option<NodeProof>,
}

/// [`server_handshake`] proving this machine's node `me` too, under
/// [`ALPN_NODES`] (the module doc's **Nodes**): the dialer's node proof is
/// checked before `OK` (`NO node` for one that does not verify), and `me`'s
/// is sent after it.
///
/// # Errors
/// As [`server_handshake`]; [`FleetError::NodeProof`] for a dialer's node
/// proof that does not verify.
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

/// [`server_handshake_as`] on a connection [`arm`] armed with `within`:
/// [`serve`]'s accept loop arms it, so the connection's slot holds the
/// watchdog's evictor from the moment it is taken. `admit` is asked, with the
/// op, once the dialer's proof verified, before `OK`: `false` answers `NO
/// capacity` ([`FleetError::AddressBusy`]).
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

/// Dialer side: complete TLS on a connected `tcp`, verify the listener's proof
/// before sending anything, then prove `op` and read the listener's answer —
/// all within `within`, as [`server_handshake`]. Returns the authenticated
/// transport, its socket deadlines cleared, and the scope the listener proved.
/// This end proves no node ([`client_handshake_as`] does).
///
/// # Errors
/// A [`FleetError`] naming what failed. On
/// [`ListenerWrongSecret`](FleetError::ListenerWrongSecret),
/// [`Reflection`](FleetError::Reflection) and every earlier failure, this end
/// sent no byte of its own proof.
pub fn client_handshake(
    tcp: TcpStream,
    secret: &FleetSecret,
    op: FleetOp,
    within: Duration,
) -> Result<(TlsTransport<ClientConnection>, FleetScope), FleetError> {
    client_handshake_as(tcp, secret, None, op, within)
        .map(|dialed| (dialed.transport, dialed.scope))
}

/// A dialer's end of a proved connection ([`client_handshake_as`]).
pub struct Dialed {
    /// The authenticated transport, its socket deadlines cleared.
    pub transport: TlsTransport<ClientConnection>,
    /// The scope the listener proved.
    pub scope: FleetScope,
    /// The node the listener proved it holds the key of, when both ends
    /// speak [`ALPN_NODES`] and it holds one.
    pub peer: Option<NodeProof>,
}

/// [`client_handshake`] proving this machine's node `me` too, under
/// [`ALPN_NODES`] (the module doc's **Nodes**): `me`'s node proof goes with
/// the op's, and the listener's is read and checked after its `OK`.
///
/// # Errors
/// As [`client_handshake`]; [`FleetError::NodeProof`] for a listener's node
/// proof that does not verify, or a listener that refused this end's.
pub fn client_handshake_as(
    tcp: TcpStream,
    secret: &FleetSecret,
    me: Option<&NodeIdentity>,
    op: FleetOp,
    within: Duration,
) -> Result<Dialed, FleetError> {
    let watchdog = arm(&tcp, within)?;
    let (mut transport, (scope, peer)) = bounded(tcp, watchdog, within, |tcp| {
        let (mut transport, nodes) = connect_tls(tcp, client_config())?;
        let exporter = transport.exporter().to_vec();
        let me = me.filter(|_| nodes);
        let proved = dialer_proves(transport.stream(), &exporter, (secret, me, nodes), op)?;
        Ok((transport, proved))
    })?;
    release_deadlines(transport.stream().get_ref())?;
    Ok(Dialed {
        transport,
        scope,
        peer,
    })
}

/// How long an accepted fleet connection has for TLS and both proofs: the
/// one deadline a fleet read gets per host (design §5), so a peer that dials
/// and sends nothing holds a slot no longer than a reader waits for it.
pub const HANDSHAKE_WITHIN: Duration = Duration::from_secs(3);

/// How long [`serve`] serves a proved connection before it cuts it: ten times
/// the whole read a fleet member makes of one host ([`HANDSHAKE_WITHIN`], its
/// one wall-clock deadline), so no reader is cut, while a member that slept or
/// fell off the network mid-read (no FIN, no RST ever arrives) or hung gives
/// its slot back. A survey is one request on one connection.
pub const SERVED_WITHIN: Duration = Duration::from_secs(30);

/// The most connections a fleet listener should hold at once, handshaking or
/// handed over ([`Listening::slots`]): a fleet of thirty reads a machine one
/// connection per reader, each for well under a second. Its owner may hold
/// fewer: aterm-gui takes the slots from the descriptor limit too, since each
/// connection costs it several descriptors.
pub const MAX_SLOTS: usize = 16;

/// The most PROVED connections one address ([`serve`]'s: an IPv4 address or
/// an IPv6 /64) holds at once on a listener of `slots` slots: a quarter of
/// them, at least one ([`Listening::per_source`]). A proved connection's slot
/// is never given up, and every member holds the one secret, so without a
/// share one member's connections — renewed at each [`SERVED_WITHIN`] cut —
/// would hold every slot and turn every other member away. A machine's own
/// readers (`survey --fleet` in several sessions at once) open one each, for
/// well under a second.
#[must_use]
pub const fn per_source(slots: usize) -> usize {
    if slots < 4 { 1 } else { slots / 4 }
}

/// The most proved [`FleetOp::Drive`] connections one address holds at once
/// on a listener of `slots` slots, counted apart from its reads' share
/// ([`per_source`]): half the slots, at least one. A drive connection is held
/// for as long as its request waits — an `await task` leg up to 20 s, a
/// `turn` up to 22 s — where a read takes well under a second, so a Mac that
/// runs two waits and a turn on another one at once is served, and its reads
/// of that Mac's survey keep their own share. Only the nodes a person named
/// are served past the handshake (the listener's owner refuses every other
/// one at once), and the other half of the slots stays for every other
/// address.
#[must_use]
pub const fn drive_per_source(slots: usize) -> usize {
    if slots < 2 { 1 } else { slots / 2 }
}

/// How long a connection that has proved nothing keeps its slot against a
/// newcomer from an address holding no more unproved connections than its own
/// ([`serve`]'s rule 2, the slots' grace for the fleet's deadline). A fleet is
/// a LAN: its round trips take milliseconds, and the slot is taken after the
/// TCP handshake, so the stack has measured the path and a lost segment costs
/// a retransmission timeout of a few hundred milliseconds, not the one-second
/// initial one. TLS and both proofs, two
/// round trips at the listener, fit in it with room for that; a third of
/// [`HANDSHAKE_WITHIN`], which idle sockets would otherwise be renewed against.
pub(crate) const PREAUTH_GRACE: Duration = Duration::from_secs(1);

/// Whether a fleet listener serves a connection from `ip`: loopback, a private
/// network (RFC 1918's three IPv4 blocks, IPv6 unique local `fc00::/7`) or
/// link-local (`169.254.0.0/16`, `fe80::/10`) — the LAN a fleet is, never the
/// internet past it. The IPv4 peer of a dual-stack listener
/// (`::ffff:a.b.c.d`) is judged as that IPv4 address.
///
/// It is a fence, not an identity: a router forwarding the port from outside
/// presents its own private address, and the proof is what admits a peer.
#[must_use]
pub fn is_fleet_source(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            let head = v6.segments()[0];
            v6.is_loopback() || head & 0xfe00 == 0xfc00 || head & 0xffc0 == 0xfe80
        }
    }
}

/// Why [`serve`] turned away a connection from outside [`is_fleet_source`].
pub const NOT_A_FLEET_SOURCE: &str = "not a loopback, private or link-local address";

/// Why [`serve`] turned away a connection while every slot was held.
pub const AT_CAPACITY: &str = "the fleet listener is at capacity";

/// The key file's name in the state directory (the module doc).
pub const KEY_FILE: &str = "fleet.key";

/// The longest key file: the longest secret's 64 digits and a final LF.
#[cfg(unix)]
const KEY_FILE_MAX: usize = 2 * FleetSecret::MAX_LEN + 1;

/// Why this machine's [`KEY_FILE`] gives no fleet secret. None carries a byte
/// of the file.
#[derive(Debug)]
pub enum KeyFileError {
    /// There is no key file (or no state directory): this machine is in no
    /// fleet.
    Missing,
    /// The file or its directory is another user's, or another user could
    /// reach it (a group or other mode bit, a second link to the file).
    NotPrivate(&'static str),
    /// The file or its directory is a symbolic link.
    Symlink,
    /// The file holds no secret in the one spelling, or is no regular file.
    Malformed(&'static str),
    /// The file could not be read.
    Io(io::Error),
}

impl KeyFileError {
    /// The one word a status line says it with.
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

/// The fleet secret a key file's bytes spell (the module doc): 40 to 64
/// lowercase hex digits, a final LF optional, nothing else.
///
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

/// This machine's fleet secret, read from [`KEY_FILE`] in `state_dir` under
/// the module doc's rules: the directory and the file are this user's and
/// nobody else's, neither is a symlink, and the file is one link to a regular
/// file holding the secret in its one spelling. Read without following a
/// link and without waiting on a FIFO.
///
/// # Errors
/// A [`KeyFileError`] naming the first rule the file or directory breaks.
#[cfg(unix)]
pub fn read_key_file(state_dir: &std::path::Path) -> Result<FleetSecret, KeyFileError> {
    let text = crate::fleet_member::read_private(state_dir, KEY_FILE, KEY_FILE_MAX)?;
    parse_key_file(&text)
}

#[cfg(unix)]
impl From<crate::fleet_member::PrivateFileError> for KeyFileError {
    /// The private-file rule the key file broke, as the key file's own error.
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

/// What [`serve`] saw, for its owner's audit trail. Every connection event
/// names the peer's address; none carries a secret or a tag.
#[derive(Debug)]
pub enum FleetEvent {
    /// A dialer proved `op`: its stream is handed over now.
    Admitted {
        /// The dialer's address.
        peer: SocketAddr,
        /// The op it proved.
        op: FleetOp,
    },
    /// An admitted connection ended: `error` is `None` for an ordinary close.
    Closed {
        /// The dialer's address.
        peer: SocketAddr,
        /// Why the hand-over failed, when it did.
        error: Option<String>,
    },
    /// A connection was turned away before anything was handed over.
    Refused {
        /// The dialer's address.
        peer: SocketAddr,
        /// Why: [`NOT_A_FLEET_SOURCE`], [`AT_CAPACITY`], or the handshake's
        /// [`FleetError`].
        reason: String,
    },
    /// The listener could not serve and stopped before its flag was cleared.
    Stopped(String),
}

/// What one fleet listener serves ([`serve`]).
#[derive(Clone, Debug)]
pub struct Listening {
    /// The fleet's secret.
    pub secret: FleetSecret,
    /// The level this listener proves and serves (`[fleet] allow`).
    pub scope: FleetScope,
    /// This machine's node, which the listener proves to every dialer that
    /// speaks [`ALPN_NODES`]; `None` proves none (`node -`), and a reader
    /// whose book binds this machine a key then reads it as no one.
    pub node: Option<NodeIdentity>,
    /// The ops it serves; a dialer proving another is answered `NO op`.
    pub serves: &'static [FleetOp],
    /// The most connections in flight at once, handshaking or handed over.
    pub slots: usize,
    /// The most proved connections one address holds at once ([`per_source`]
    /// of the slots): a dialer past it is answered `NO capacity`.
    pub per_source: usize,
    /// The longest a proved connection is served before it is cut
    /// ([`SERVED_WITHIN`]).
    pub lifetime: Duration,
}

/// A served connection's hold on its listener ([`Served`]): its slot
/// ([`Listening::slots`]) and its address's share of proved connections
/// ([`Listening::per_source`]). Dropping it frees both.
struct Release {
    // Dropped in this order: the slot a newcomer's accept waits for first.
    _slot: crate::slots::SlotTenure,
    tenure: LiveTenure,
}

/// A proved connection as [`serve`] hands it over: its stream, and its hold
/// on the listener — its slot ([`Listening::slots`]) and its address's share
/// of proved connections ([`Listening::per_source`]).
///
/// It ends one of two ways, and each frees the hold BEFORE the peer hears
/// the end, once nothing more of the connection waits on the peer:
/// [`Served::relay_answers`] relays it to a local end that answers (a control
/// server), [`Served::answer`] answers it here (a join). A peer that has read
/// its connection's end can then dial again at once from the same address
/// and be served, never answered `NO capacity` by its own connection still
/// tearing down; no hand-over can keep the hold through the end. One dropped
/// without either frees the hold as it drops, before its stream closes.
///
/// Nothing after the release waits on either end: a relay hangs its local
/// end up first, and the peer is sent a `close_notify` only as far as the
/// socket takes it at once, then the FIN (each a shutdown of each half on its
/// own, [`aterm_uds::hangup::hang_up`], which wakes a thread writing to a
/// peer that sent its FIN and reads nothing; a both-ways one would not, on
/// Darwin). However the peer ends a relayed connection — its FIN, a reset, a
/// record that does not decrypt — its local end hears an EOF after the line
/// it is serving and the hold is kept until it has answered that line or
/// hung up, so the control server's thread serving it counts too. So a
/// connection whose hold is gone keeps no thread or descriptor on the peer's
/// account or its local end's, and one member cannot fill the slots with
/// teardowns: the slots bound the threads and descriptors in flight, and the
/// share what one address holds of them.
///
/// A connection the listener cuts — past [`Listening::lifetime`], or as it
/// stops or its key changes — hears the cut first: its share goes with the cut,
/// its slot as its end runs its course (a join's at once, a relay's after at
/// most the line its local end was serving), so the slots still bound the
/// threads and descriptors in flight. A connection the handshake refuses never
/// reaches a hand-over: it hears its `NO <why>` first and is freed as its
/// thread ends — a wrong secret or op is refused again however soon it dials,
/// and a peer past its share waits for one of its served connections to end,
/// which frees the share first.
pub struct Served {
    // Dropped first: a `Served` dropped through neither end frees its hold
    // before its stream closes, so the peer hears that end after the release
    // too, and the close that follows never blocks.
    release: Release,
    transport: TlsTransport<ServerConnection>,
    peer: Option<NodeProof>,
}

impl Served {
    /// The address the peer dialled from.
    #[must_use]
    pub fn peer_addr(&self) -> Option<SocketAddr> {
        self.transport.tcp().peer_addr().ok()
    }

    /// The node the peer proved it holds the key of on this connection, when
    /// it proved one: possession, not yet identity — which node that makes
    /// it is this machine's book's to say
    /// (`fleet_member::proven_peer`, the one an authorization asks).
    #[must_use]
    pub const fn peer_node(&self) -> Option<&NodeProof> {
        self.peer.as_ref()
    }

    /// Relay the connection to `local`, an end that answers and then hangs up
    /// (a control server): [`tls::relay_answers`], whose guard is this
    /// connection's hold, freed after every answer byte and before the peer
    /// hears the end.
    ///
    /// # Errors
    /// As [`tls::relay_answers`].
    pub fn relay_answers(self, local: aterm_uds::CtlStream) -> io::Result<()> {
        tls::relay_answers(self.transport, local, self.release)
    }

    /// Answer the connection here: `write` reads the request and writes the
    /// answer on the stream (set its read timeout first,
    /// [`tls::Answering::set_read_timeout`]: nothing here waits on its own
    /// clock), then the answer is flushed, the hold freed, and only then does
    /// the peer hear the end ([`tls::answer`], whose guard is this
    /// connection's hold). The stream `write` gets reads and writes and
    /// cannot end the connection, so no answer can put its end on the wire
    /// while the hold is held.
    ///
    /// # Errors
    /// `write`'s own, or the flush's.
    pub fn answer<T>(
        self,
        write: impl FnOnce(&mut tls::Answering<'_, ServerConnection>) -> io::Result<T>,
    ) -> io::Result<T> {
        tls::answer(self.transport, self.release, write)
    }
}

/// Serve a bound `listener` until `running` is cleared: each connection from
/// an [`is_fleet_source`] address that gets a slot runs [`server_handshake`]
/// within [`HANDSHAKE_WITHIN`] on a thread of its own, and a proved stream
/// goes to `hand_over` on that thread as a [`Served`], which holds the slot
/// and the share until it ends (freeing them before the peer hears the end)
/// or the hand-over returns; the listener cuts it `lifetime` after the proof.
/// Every refusal and every admitted connection's start and end go to
/// `on_event`, a connection's last event only once its slot is free, so the
/// owner's logging never holds a slot a newcomer waits for; no failure of a
/// connection stops the listener.
///
/// **Who gets a slot when every slot is held**: the NET-1 rule (r13,
/// 2026-10-10) of the listener's slots, with [`Listening::slots`] and its
/// `PREAUTH_GRACE` (1 s). A connection that has proved nothing gives its slot
/// up to a newcomer — at once to one from an address holding at least two
/// fewer unproved connections than the most crowded address, and after
/// `PREAUTH_GRACE` to one from an address no heavier than its own — so a LAN
/// host that holds every slot with idle sockets, renewing each one as its
/// deadline cuts it, never turns a member from another address away. A proved
/// connection's slot is never taken (the watchdog's claim decides it once), so
/// members still fill the listener: past that it refuses. **One address holds
/// at most [`Listening::per_source`] proved connections**, and apart from
/// them at most [`drive_per_source`] of the slots' proved drives, which wait
/// as long as their request does: a dialer whose proof verifies past its
/// op's share is answered `NO capacity` before `OK`, and its slot freed —
/// every member holds the secret, so one of them cannot keep the others out
/// by holding proved connections, renewed at each cut, and a Mac's waits on
/// another never take its reads' share. (On IPv6 the share is the LAN's /64,
/// below.)
///
/// **On IPv6 a LAN is one address.** An address is an IPv4 address or an IPv6
/// /64, and every IPv6 host on one segment shares a /64 (`fe80::/64` for
/// link-local, the LAN's SLAAC or ULA prefix otherwise): an IPv6 attacker and
/// every IPv6 member there are one address, so rule 1 never separates them and
/// a member is protected only by rule 2 — after the grace, and behind the
/// accept backlog, where each connection refused at capacity holds the accept
/// loop for up to the accept poll (200 ms), long enough for an attacker with
/// many queued connections to push a member past its 3 s read deadline. A
/// fleet listens on an IPv4 address (`[fleet] listen = "<IPv4>:<port>"`).
///
/// **A proved connection lives at most `lifetime`** from its proof: the
/// accept loop cuts it then (both halves shut down), so a member that slept
/// or vanished mid-read, which sends no FIN and no RST, or hung, gives its
/// slot back.
///
/// The listener is put in non-blocking mode and its wait for a connection is
/// bounded by the accept poll (200 ms), so clearing `running` is seen
/// promptly, and so is an overdue connection. When `running` is cleared,
/// every connection still open is cut before this returns: a listener told to
/// stop serves no one, a stream already handed over included.
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
    if let Err(error) = listener.set_nonblocking(true) {
        on_event(FleetEvent::Stopped(format!(
            "could not make the fleet listener non-blocking: {error}"
        )));
        return;
    }
    let slots = Arc::new(Slots::new(listening.slots, PREAUTH_GRACE));
    let live = Arc::new(LiveConnections::default());
    let mut next_id: u64 = 0;
    while running.load(Ordering::Acquire) {
        live.cut_overdue(listening.lifetime);
        let (tcp, peer) = match listener.accept() {
            Ok(accepted) => accepted,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                // A readiness error backs off like an accept error below, so
                // neither becomes a busy loop.
                if wait_listener_readable(listener, ACCEPT_POLL).is_err()
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
        if !is_fleet_source(peer.ip()) {
            on_event(FleetEvent::Refused {
                peer,
                reason: NOT_A_FLEET_SOURCE.to_owned(),
            });
            continue; // dropping `tcp` closes it
        }
        // A free slot, or one a connection that has proved nothing gives up
        // (the rule above); otherwise refuse rather than spawn unbounded work.
        let source = source_of(peer.ip());
        if !slots.make_room(source) {
            on_event(FleetEvent::Refused {
                peer,
                reason: AT_CAPACITY.to_owned(),
            });
            continue;
        }
        // Armed here, not on the connection's thread, so the slot holds the
        // connection's evictor from the moment it is taken.
        let armed = tcp
            .try_clone()
            .map_err(FleetError::Io)
            .and_then(|line| Ok((line, arm(&tcp, HANDSHAKE_WITHIN)?)));
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
            scope,
            node,
            serves,
            per_source,
            slots: most,
            ..
        } = listening.clone();
        let hand_over = hand_over.clone();
        let tell = on_event.clone();
        let spawned = std::thread::Builder::new()
            .name("aterm-fleet-conn".to_owned())
            .spawn(move || {
                // It frees the connection's slot and share as this thread
                // ends, however it ends, if the hand-over has not first.
                let release = release;
                let event = match server_handshake_armed(
                    tcp,
                    watchdog,
                    HANDSHAKE_WITHIN,
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
                ) {
                    Ok(Accepted {
                        transport,
                        op,
                        peer: proof,
                    }) => {
                        tell(FleetEvent::Admitted { peer, op });
                        let served = Served {
                            transport,
                            release,
                            peer: proof,
                        };
                        FleetEvent::Closed {
                            peer,
                            error: hand_over(served, op).err().map(|e| e.to_string()),
                        }
                    }
                    Err(e) => {
                        // Free first: a displaced connection's slot is what a
                        // newcomer waits for (`Slots::make_room`, the accept
                        // poll), and the owner's logging must never sit in
                        // that wait.
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
    live.cut_all();
}

/// [`serve`]'s connections in flight, each by a clone of its socket, which is
/// how the listener cuts one: when it is told to stop, and when a proved one
/// outlives its lifetime. The slots ([`Slots`]) bound how many there are.
#[derive(Default)]
struct LiveConnections {
    fleet_live: Mutex<Vec<Live>>,
}

/// One connection [`LiveConnections`] can cut.
struct Live {
    id: u64,
    /// The address it is counted against (`slots::source_of`).
    source: IpAddr,
    line: TcpStream,
    /// When it proved itself, and its lifetime began.
    proved: Option<Instant>,
    /// Whether it proved a drive, counted against its address's drive share
    /// ([`drive_per_source`]) rather than its reads' ([`per_source`]).
    drive: bool,
}

impl LiveConnections {
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Live>> {
        self.fleet_live
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Hold connection `id`'s `line`, from `source`, until the returned
    /// tenure drops.
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

    /// Cut every proved connection served `lifetime` or longer, both halves,
    /// each on its own ([`aterm_uds::hangup::hang_up`]): a both-ways
    /// `shutdown` of a peer that sent its FIN shuts nothing on Darwin, and
    /// would leave its relay writing to a peer that reads nothing, its slot
    /// held, once its share was gone with the cut.
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

    /// Shut every connection still held down, both halves, each on its own
    /// (as [`Self::cut_overdue`] does).
    fn cut_all(&self) {
        for live in std::mem::take(&mut *self.lock()) {
            aterm_uds::hangup::hang_up(&live.line);
        }
    }
}

/// Lets go of one connection's line as its thread ends, however it ends.
struct LiveTenure {
    live: Arc<LiveConnections>,
    id: u64,
}

impl LiveTenure {
    /// The connection proved itself — a drive where `drive`, else a read:
    /// `true`, and its lifetime starts now, unless its address already holds
    /// `most` proved connections of that kind — counted and marked under one
    /// lock, so two proofs from one address at once never both pass the
    /// share.
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

/// Arm a handshake's deadline on `tcp`: per-syscall timeouts, which bound an
/// idle peer, and the [`HandshakeWatchdog`], which force-closes
/// the socket at `within` against one that dribbles
/// ([`arm_unauthenticated`]).
///
/// The socket is put in blocking mode first: one accepted from a
/// non-blocking listener ([`serve`]'s) inherits that mode on macOS and the
/// BSDs, and a non-blocking read never waits for its timeout.
fn arm(tcp: &TcpStream, within: Duration) -> Result<HandshakeWatchdog, FleetError> {
    if within.is_zero() {
        return Err(FleetError::Timeout(within));
    }
    tcp.set_nonblocking(false).map_err(FleetError::Io)?;
    arm_unauthenticated(tcp, within).map_err(FleetError::Io)
}

/// Run `handshake` on `tcp` under the deadline `watchdog` was armed with
/// (`within`, [`arm`]). Past it the result is [`FleetError::Timeout`],
/// whatever error the force-close surfaced as; a connection whose slot
/// [`serve`] gave a newer one is an [`FleetError::Io`] that says so.
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

/// Lift the handshake's socket deadlines: what follows is the caller's.
fn release_deadlines(tcp: &TcpStream) -> Result<(), FleetError> {
    tcp.set_read_timeout(None).map_err(FleetError::Io)?;
    tcp.set_write_timeout(None).map_err(FleetError::Io)
}

/// The rustls error an `io::Error` from `complete_io` carries, if any.
fn rustls_error(e: &io::Error) -> Option<&rustls::Error> {
    e.get_ref()
        .and_then(|inner| inner.downcast_ref::<rustls::Error>())
}

/// The fleet exporter of a connection whose handshake is complete.
fn export<C, S>(conn: &C) -> Result<[u8; EXPORTER_LEN], FleetError>
where
    C: std::ops::Deref<Target = rustls::ConnectionCommon<S>>,
{
    conn.export_keying_material([0u8; EXPORTER_LEN], EXPORTER_LABEL, None)
        .map_err(|e| FleetError::Tls(io::Error::other(e)))
}

/// The listener's TLS handshake under `config`, which must name
/// [`PROTOCOLS`]: the transport, and whether it speaks [`ALPN_NODES`]. A
/// dialer that offers other protocols (rustls refuses it) or none (rustls
/// lets it through, so this checks) is [`FleetError::Alpn`].
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

/// Whether a connection that negotiated `alpn` speaks [`ALPN_NODES`]; `None`
/// when it negotiated no fleet protocol.
fn speaks_nodes(alpn: Option<&[u8]>) -> Option<bool> {
    match alpn {
        Some(ALPN_NODES) => Some(true),
        Some(ALPN) => Some(false),
        _ => None,
    }
}

/// The dialer's TLS handshake under `config`, which must name
/// [`PROTOCOLS`]: the transport, and whether it speaks [`ALPN_NODES`].
///
/// A listener that negotiates no fleet protocol is [`FleetError::Alpn`]: one
/// that serves other protocols refuses the handshake with an alert; one that
/// serves none completes it without ALPN, which this checks; and a pre-fleet
/// aterm listener — no ALPN and its own `[net]` certificate — fails the
/// certificate pin first, so a certificate refusal from a listener that chose
/// no fleet protocol is the same diagnosis. A listener that chose one and
/// presents another certificate is a TLS failure, said as the pin mismatch.
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

/// The listener's half on a channel whose exporter is `exporter`: prove
/// `scope` first, then verify the dialer's proof and answer it, `NO op` for an
/// op not in `serves`. Under [`ALPN_NODES`] (`nodes`) the dialer's node line
/// is read and checked before the answer (`NO node`), and `me`'s goes after
/// `OK`. Returns the op the dialer proved, and the node it proved.
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
        // Diagnosis only: the role keys have already refused it.
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
    // The dialer holds the secret: only now is its node read, and this
    // machine's said.
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
    let Some(op) = FleetOp::from_word(word).filter(|op| serves.contains(op)) else {
        return refuse(stream, "op", FleetError::UnknownOp(word.to_owned()));
    };
    if !admit(op) {
        return refuse(stream, "capacity", FleetError::AddressBusy);
    }
    let mut verdict = String::from("OK\n");
    if nodes {
        verdict.push_str(&node_line(me, Signer::Listener, exporter));
    }
    send(stream, &verdict)?;
    Ok((op, peer))
}

/// Answer the dialer `NO <why>` (best effort: it may be gone) and fail with
/// `error`.
fn refuse<S: Write, T>(stream: &mut S, why: &str, error: FleetError) -> Result<T, FleetError> {
    let _ = stream
        .write_all(format!("NO {why}\n").as_bytes())
        .and_then(|()| stream.flush());
    Err(error)
}

/// The dialer's half on a channel whose exporter is `exporter`: verify the
/// listener's proof, and only then prove `op` and read the answer. Under
/// [`ALPN_NODES`] (`nodes`) `me`'s node line goes with the op's, and the
/// listener's is read and checked after its `OK`. Returns the scope the
/// listener proved, and the node it proved.
fn dialer_proves<S: Read + Write>(
    stream: &mut S,
    exporter: &[u8],
    (secret, me, nodes): (&FleetSecret, Option<&NodeIdentity>, bool),
    op: FleetOp,
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
        // Diagnosis only: the role keys have already refused it.
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
    // The listener has proved itself: only now does this end send a byte.
    let word = op.word();
    let mut proof = proof_line(word, &tag(secret.dialer_key(word), exporter, word));
    if nodes {
        proof.push_str(&node_line(me, Signer::Dialer, exporter));
    }
    send(stream, &proof)?;
    match read_line(stream)?.as_deref() {
        Some("OK") => {}
        Some("NO op") => return Err(FleetError::UnknownOp(word.to_owned())),
        Some("NO capacity") => return Err(FleetError::AddressBusy),
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
    }
    let peer = if nodes {
        read_node_proof(stream, Signer::Listener, exporter)?
    } else {
        None
    };
    Ok((scope, peer))
}

/// HKDF-Extract (RFC 5869 §2.2): `PRK = HMAC-SHA256(salt, IKM)`. An empty
/// salt is RFC 5869's string of 32 zeros: HMAC zero-pads both to one block.
fn hkdf_extract(salt: &[u8], ikm: &[u8]) -> [u8; 32] {
    mac(salt, &[ikm])
}

/// HKDF-Expand (RFC 5869 §2.3) for 32 bytes, its first block alone:
/// `T(1) = HMAC-SHA256(PRK, info ‖ 0x01)`.
fn hkdf_expand_32(prk: &[u8; 32], info: &[u8]) -> [u8; 32] {
    mac(prk, &[info, &[1]])
}

/// HMAC-SHA256 under `key` of the concatenation of `parts`.
fn mac(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC-SHA256 accepts a key of any length");
    for part in parts {
        mac.update(part);
    }
    mac.finalize()
}

/// The tag a role's `key` makes for `word` on the channel whose exporter is
/// `exporter`.
fn tag(key: &[u8; 32], exporter: &[u8], word: &str) -> [u8; TAG_LEN] {
    mac(key, &[exporter, word.as_bytes()])
}

/// Whether `presented` is `key`'s tag for `word` on this channel, compared in
/// constant time.
fn verifies(key: &[u8; 32], exporter: &[u8], word: &str, presented: &[u8]) -> bool {
    aterm_digest::ct_eq(&tag(key, exporter, word), presented)
}

/// A wire word: `[a-z0-9-]{1,32}`.
fn is_word(word: &str) -> bool {
    (1..=MAX_WORD).contains(&word.len())
        && word
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Split a proof line, `<word> SP <64 lowercase hex digits>`, into its word
/// and tag.
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

/// One proof line, its LF included.
fn proof_line(word: &str, tag: &[u8; TAG_LEN]) -> String {
    format!("{word} {}\n", hex::encode_lower(tag))
}

/// Send one proof line.
fn write_proof<W: Write>(
    stream: &mut W,
    word: &str,
    tag: &[u8; TAG_LEN],
) -> Result<(), FleetError> {
    send(stream, &proof_line(word, tag))
}

/// Send `text`, one or more whole lines, in one flush.
fn send<W: Write>(stream: &mut W, text: &str) -> Result<(), FleetError> {
    stream
        .write_all(text.as_bytes())
        .and_then(|()| stream.flush())
        .map_err(FleetError::Io)
}

/// Read one handshake line (its LF consumed, not returned), at most
/// [`MAX_LINE`] bytes, a byte at a time so nothing past it is taken from the
/// caller's stream. `Ok(None)`: the peer closed before a byte of it.
fn read_line<R: Read>(stream: &mut R) -> Result<Option<String>, FleetError> {
    read_line_within(stream, MAX_LINE)
}

/// [`read_line`] of at most `max` bytes.
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
            // A peer gone without a TLS close_notify (or with a reset) before
            // a byte of the line is the same hang-up as a clean close.
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

    /// A one-minute hang detector on every handshake and every socket read
    /// here — never a latency budget: a loopback handshake takes milliseconds.
    const HANG: Duration = Duration::from_secs(60);

    /// Every op this aterm knows: a listener that serves them all.
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

    /// Run `listen` on a loopback connection's accepted end in a thread and
    /// `dial` on its connected end here; both results.
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

    /// The fleet listener's TLS alone, for a test peer that then misbehaves.
    fn raw_accept(tcp: TcpStream) -> TlsTransport<ServerConnection> {
        tcp.set_read_timeout(Some(HANG)).unwrap();
        accept_tls(tcp, server_config()).unwrap().0
    }

    /// The fleet dialer's TLS alone, for a test peer that then misbehaves.
    fn raw_connect(tcp: TcpStream) -> TlsTransport<ClientConnection> {
        connect_tls(tcp, client_config()).unwrap().0
    }

    /// Every byte a peer sends until it closes (or a read fails).
    fn drain<R: Read>(stream: &mut R) -> Vec<u8> {
        let mut got = Vec::new();
        let _ = stream.read_to_end(&mut got);
        got
    }

    /// RFC 5869 appendix A, test cases 1 and 3 (SHA-256): the extract step,
    /// and the expand step's first block, which is all a role key takes.
    #[test]
    fn hkdf_matches_rfc_5869() {
        let ikm = [0x0b; 22];
        let salt: Vec<u8> = (0x00..=0x0c).collect();
        let info: Vec<u8> = (0xf0..=0xf9).collect();
        let prk = hkdf_extract(&salt, &ikm);
        assert_eq!(
            prk.to_vec(),
            hex32("077709362c2e32df0ddc3f0dc47bba6390b6c73bb50f9c3122ec844ad7c2b3e5")
        );
        assert_eq!(
            hkdf_expand_32(&prk, &info).to_vec(),
            hex32("3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf")
        );
        let prk = hkdf_extract(&[], &ikm);
        assert_eq!(
            prk.to_vec(),
            hex32("19ef24a32c717b167f33a91d6f648bdf96596776afdb6377ac434c1c293ccb04")
        );
        assert_eq!(
            hkdf_expand_32(&prk, &[]).to_vec(),
            hex32("8da4e775a563c18f715f802a063c5a31b8a11f5c5ee1879ec3454e5f3c738d2d")
        );
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
                    // The stream is the caller's now: one request, one answer.
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

    /// A node with a seed of `byte`s and the node id `byte` repeated.
    fn node(byte: u8) -> NodeIdentity {
        NodeIdentity::from_seed(&format!("{byte:02x}").repeat(16), &[byte; 32]).unwrap()
    }

    /// EACH END PROVES ITS NODE under atf/2: the listener learns the node the
    /// dialer holds the key of and the dialer the listener's, and an end that
    /// holds no node key says so (`node -`), proving none. An older aterm on
    /// either end speaks atf/1, where no node is proved and the handshake is
    /// as it was.
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
        // An older dialer (it offers atf/1 alone) at a listener that holds a
        // node: proved as before, and neither end names a node.
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
                )
            },
        );
        assert_eq!(listened.unwrap(), (FleetOp::Book, None));
        assert_eq!(dialed.unwrap(), (FleetScope::Facts, None));
        // An older listener (it serves atf/1 alone): the same.
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

    /// A NODE PROOF IS ITS END'S, ON ITS CHANNEL: a signature over another
    /// channel's exporter, under another key than the one the line names, or
    /// the other end's proof sent back is refused — the listener answers
    /// `NO node` and hands nothing over, the dialer reads nothing past it —
    /// and a node line that is no node line is malformed. Over a socket pair
    /// with stand-in exporters, so each case is one scripted line.
    #[test]
    fn a_node_proof_proves_only_its_own_end_on_its_own_channel() {
        const E: &[u8] = &[0xa5; EXPORTER_LEN];
        const OTHER: &[u8] = &[0x5a; EXPORTER_LEN];
        let keys = secret(7);
        let me = node(3);
        let op_line = proof_line("survey", &tag(&keys.client, E, "survey"));
        // A node line naming `named`'s node and key, signed by `signer` as
        // `end` over `exporter`.
        let forged = |named: &NodeIdentity, signer: &NodeIdentity, end: Signer, exporter: &[u8]| {
            format!(
                "node {} {} {}\n",
                named.node(),
                named.key().hex(),
                hex::encode_lower(&signer.sign(end, exporter))
            )
        };

        // The listener's side.
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
            // Another connection's proof, replayed on this one.
            forged(&me, &me, Signer::Dialer, OTHER),
            // The listener's proof sent back as the dialer's.
            forged(&me, &me, Signer::Listener, E),
            // A key the line names, signed by another.
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

        // The dialer's side: the scripted listener's hello, then its OK and
        // node line.
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

    /// An honest dialer of another fleet checks the listener's proof and sends
    /// nothing; a dialer that skips that check (a hostile or broken one) has
    /// its own proof refused by the listener.
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
        // `DialerHungUp` is a close before a byte of the dialer's line: the
        // honest dialer sent nothing.
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

    /// THE NEGATIVE CONTROL. A dialer that holds no secret sends the listener
    /// its own proof back — a genuine tag for this very channel, refused only
    /// because the roles' keys differ. A "listener" that proves with a
    /// dialer's key is refused the same way, and an echo gets nothing at all.
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
        // The control: what came back is the listener's own valid proof on
        // this channel. One key for both roles would have taken it.
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

        // An echo listener: the dialer waits for a proof that never comes,
        // so there is nothing to echo. The deadline here is the outcome under
        // test, not a budget: this handshake can only end by it.
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

    /// A tag names its word and its channel: a proof for one op, scope or
    /// connection is no proof for another. Over a socket pair with stand-in
    /// exporters, so each case is one scripted line.
    #[test]
    fn a_tag_proves_only_its_own_word_on_its_own_channel() {
        const E: &[u8] = &[0xa5; EXPORTER_LEN];
        const OTHER: &[u8] = &[0x5a; EXPORTER_LEN];
        let keys = secret(7);

        // The listener's side: the scripted dialer's line, the listener's
        // error and answer.
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
            // A read's proof sent as a drive, and a drive's as a read: one
            // key, but a tag names its word.
            proof(&keys.client, E, "survey", "drive"),
            proof(&keys.client, E, "drive", "survey"),
            // A drive proved with the join key.
            proof(&keys.join, E, "drive", "drive"),
            // A survey's proof sent as a join, and a join's as a survey.
            proof(&keys.client, E, "survey", "join"),
            proof(&keys.join, E, "join", "survey"),
            // A survey's proof sent as a book read, and a book read's as a
            // survey: one key, but a tag names its word.
            proof(&keys.client, E, "survey", "book"),
            proof(&keys.client, E, "book", "survey"),
            // A join proved with the read key: the op has its own.
            proof(&keys.client, E, "join", "join"),
            // A proof from another connection, replayed on this one.
            proof(&keys.client, OTHER, "survey", "survey"),
        ] {
            let (heard, answer) = listener_hears(refused.clone());
            assert!(
                matches!(heard, Err(FleetError::DialerWrongSecret)),
                "{refused:?}: {heard:?}"
            );
            assert_eq!(answer.as_deref(), Some("NO proof"));
        }
        // An op this aterm does not know, proved: told apart from a bad proof.
        let (heard, answer) = listener_hears(proof(&keys.client, E, "steer", "steer"));
        assert!(
            matches!(&heard, Err(FleetError::UnknownOp(op)) if op == "steer"),
            "{heard:?}"
        );
        assert_eq!(answer.as_deref(), Some("NO op"));
        let (heard, answer) = listener_hears(format!("survey {}\n", "AB".repeat(32)));
        assert!(matches!(heard, Err(FleetError::Malformed(_))), "{heard:?}");
        assert_eq!(answer.as_deref(), Some("NO proof"));

        // The dialer's side: the scripted listener's hello and answer, the
        // dialer's verdict and what it sent.
        let dialer_hears = |hello: String, answer: &'static str| {
            let (mut dialer, mut listener) = CtlStream::pair().unwrap();
            listener.set_read_timeout(Some(HANG)).unwrap();
            let dialing = std::thread::spawn(move || {
                dialer.set_read_timeout(Some(HANG)).unwrap();
                dialer_proves(&mut dialer, E, (&secret(7), None, false), FleetOp::Survey)
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
            // The listener's proof of content claimed as facts, and one from
            // another connection.
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
        // A proved listener's answers: each verdict by its own name.
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

    /// A listener TLS configuration for a test peer: `cert`/`key`, TLS 1.3,
    /// offering `alpn`.
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

    /// A dialer TLS configuration for a test peer: the fleet's pin, offering
    /// `alpn`.
    fn dialer_tls(alpn: &[&[u8]]) -> Arc<ClientConfig> {
        let mut cfg = (*client_config()).clone();
        cfg.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
        Arc::new(cfg)
    }

    /// A peer's side of a TLS handshake under `conn`, then whatever it sends
    /// until it closes; the result is not the point.
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

    /// A peer that negotiates neither `atf/2` nor `atf/1` is named so, from
    /// either end: a pre-fleet aterm listener (an older aterm's `[net]` drive:
    /// its own certificate, no ALPN), a fleet-certified listener with no ALPN
    /// or another one, and dialers offering none or another.
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
        // The contrast: a listener that does choose atf/1 but presents
        // another certificate fails the pin, said as the mismatch.
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

    /// A dialer that completes TLS and then sends nothing gets no slot past the
    /// deadline: the listener's handshake ends as a timeout. The listener's
    /// deadline is the one outcome under test: the dialer sends no proof
    /// whether its TLS finished inside the short deadline or was cut by it,
    /// and reads until the close.
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

    /// A proved op the listener does not serve is answered `NO op`, which the
    /// dialer names as such; the listener hands nothing over. An older
    /// listener — one that serves `survey` and `join`, from before members
    /// read the issuer's book — answers a `book` so, and a listener whose
    /// person allows no driving (it serves the reads and the join, never
    /// `drive`) answers a `drive` so.
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

    /// The fence: loopback, the private blocks and link-local are served,
    /// judged through a dual-stack listener's IPv4-mapped form too; public,
    /// shared (CGNAT), documentation, multicast and unspecified addresses are
    /// not.
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

    /// The key file's one spelling: 40 to 64 lowercase hex digits, a final LF
    /// optional. The secret it spells is the one [`FleetSecret::new`] takes.
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

    /// The key file is read only when it and its directory are this user's
    /// alone: a group or other bit on either, a second link, a symlink at
    /// either, or no file at all is refused by its own word.
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
        // No key in a directory a launch left open to others is no key, not
        // a privacy wall: the mode is judged where there is a secret to keep.
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

        // A symlink in the file's place, and a state directory that is one.
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

        // Not a regular file, and a file too long to be one secret.
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

    /// THE LISTENER END TO END on a loopback port: a proved stream is handed
    /// over and served, a dialer past the last slot is turned away before any
    /// TLS byte, and clearing the flag ends the loop and cuts the stream it
    /// handed over. `serve` makes its listener non-blocking, which an accepted
    /// socket inherits on macOS: the handshake proves it is put back.
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
                    // An echo of one line, then of everything until the end.
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

        // The one slot is held: the next dialer is closed on before TLS.
        let tcp = TcpStream::connect(addr).unwrap();
        tcp.set_read_timeout(Some(HANG)).unwrap();
        assert!(client_handshake(tcp, &secret(7), FleetOp::Survey, HANG).is_err());
        assert!(
            matches!(next("Refused"), FleetEvent::Refused { reason, .. } if reason == AT_CAPACITY)
        );

        running.store(false, Ordering::Release);
        serving.join().unwrap();
        // Cut, not idle: the read ends at once, never by its hang detector.
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

    /// A PEER THAT HAS READ ITS CONNECTION'S END DIALS AGAIN AT ONCE, from the
    /// same address, and is served — through each of a [`Served`]'s two
    /// ends, the ones the aterm listener's hand-over takes: relayed to a local
    /// end that answers ([`Served::relay_answers`], every op but `join`) and
    /// answered here ([`Served::answer`], a `join`). Each frees the hold
    /// before the peer hears the end. Each hand-over then holds its thread
    /// until the test lets it go, so the first connection is still being torn
    /// down — deterministically, no sleep — when the peer dials again: on a
    /// listener of one slot (the slot is free) and on one of two slots with a
    /// share of one (the share is free).
    ///
    /// NEGATIVE CONTROL: a relay that keeps the hold until the hand-over
    /// returns — the order before 2026-10-16, which the live lane caught as a
    /// flake — has the second dial turned away: at capacity, before TLS, with
    /// one slot; `NO capacity` with a share of one. This holds the slot and
    /// share semantics; the order inside each end (the guard gone before any
    /// byte of the end is on the wire) is pinned by
    /// `tls::tests::an_answering_relay_says_nothing_of_the_end_while_its_guard_is_held`
    /// and `tls::tests::an_answer_says_nothing_of_the_end_while_its_guard_is_held`.
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

    /// How [`redial`]'s hand-over ends each connection.
    #[derive(Clone, Copy, Debug)]
    enum End {
        /// [`Served::relay_answers`] to a local end that answers.
        Relayed,
        /// [`Served::answer`] here.
        Answered,
        /// THE CONTROL: relayed with the hold kept until the hand-over
        /// returns.
        Kept,
    }

    /// [`a_peer_that_read_its_end_dials_again_at_once`]'s run: a listener of
    /// `slots` slots and a share of `per_source`, whose hand-over ends each
    /// connection as `end` says; one dial asked and read to its end; then a
    /// second dial at once, and what it was answered.
    fn redial(slots: usize, per_source: usize, end: End) -> Result<String, FleetError> {
        use std::sync::mpsc;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let running = Arc::new(AtomicBool::new(true));
        // Every hand-over waits here, its answer given, until the sender goes.
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
                        // One line asked, answered: here, or by a local end
                        // that then hangs up, as a control server ends a
                        // fleet connection.
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
        // Its end, read whole: a clean close_notify, never the hang detector.
        let mut rest = Vec::new();
        let end = first.stream().read_to_end(&mut rest);
        assert!(
            end.is_ok() && rest.is_empty(),
            "the first connection ends cleanly: {end:?} {rest:?}"
        );
        // At once, from the same address, while the first connection's thread
        // still holds on.
        let second = dial().and_then(|mut proved| ask(&mut proved).map_err(FleetError::Io));
        drop(go);
        running.store(false, Ordering::Release);
        serving.join().unwrap();
        second
    }

    /// THE LISTENER'S CUT FREES THE SLOT OF A MEMBER THAT SENT ITS FIN AND
    /// READS NOTHING. The member proves itself on a listener of one slot,
    /// sends its FIN at once and never reads, and the control server's
    /// stand-in answers without end, so the relay is left writing an answer
    /// nobody takes. The listener cuts the connection at its lifetime: the
    /// relay returns, its connection's slot comes back, and the next dial is
    /// served.
    ///
    /// NEGATIVE CONTROL (by mutation, run 2026-10-16): with the cut a
    /// both-ways `shutdown` — which Darwin refuses once the peer's FIN is in,
    /// shutting nothing — the cut drops the connection's share and leaves its
    /// relay blocked writing and its slot held for as long as the member
    /// keeps the connection open: the relay's end is the hang detector's to
    /// report, and a member could fill every slot so. Every wait here is the
    /// hang detector, never a latency budget; the lifetime is the cut's
    /// configuration, short so the cut comes soon.
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
                    // Relayed to a local end that answers without end and
                    // never reads, until its writes fail.
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
        // Its FIN, and never a read: the connection stays open on its side.
        silent.tcp().shutdown(std::net::Shutdown::Write).unwrap();
        // The cut ends the relay, and its slot comes back.
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

    /// What a [`serve`] under [`Serving`] said, counted as it says it.
    #[derive(Default)]
    struct Heard {
        /// Newcomers turned away with every slot held.
        at_capacity: std::sync::atomic::AtomicUsize,
        /// Connections that had proved nothing, whose slot went to a newcomer.
        displaced: std::sync::atomic::AtomicUsize,
        /// Why each connection from ::1 was turned away, in order.
        refused_v6: Mutex<Vec<String>>,
    }

    /// [`serve`] on its own thread, each proved stream an echo until its end;
    /// stopped, and joined, when dropped.
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
                    serve(&listener, &listening, &running, echo, move |event| {
                        if let FleetEvent::Refused { peer, reason } = event {
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
                    });
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

    /// A hand-over that echoes everything until the end.
    fn echo(served: Served, _op: FleetOp) -> io::Result<()> {
        served.answer(echo_all)
    }

    /// Echo everything `stream` reads until its end.
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

    /// One line through a proved stream's echo.
    fn echoes(conn: &mut TlsTransport<ClientConnection>) -> bool {
        let mut back = [0u8; 5];
        conn.stream().get_ref().set_read_timeout(Some(HANG)).is_ok()
            && conn.stream().write_all(b"ping\n").is_ok()
            && conn.stream().flush().is_ok()
            && conn.stream().read_exact(&mut back).is_ok()
            && &back == b"ping\n"
    }

    /// A listener that serves `slots` connections at once, each for `lifetime`,
    /// with no share of them kept from one address: the slots' rule alone.
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

    /// NET-1 across addresses at the fleet port: a LAN host that holds every
    /// slot with idle sockets — no TLS byte, each reconnected the moment the
    /// listener closes it, no secret needed — never turns a member from
    /// another address away. Rule 1 lets the member in on its FIRST dial, the
    /// attack milliseconds old; refusing the newest connection turned it away
    /// for as long as the sockets kept coming. A dual-stack listener sees
    /// 127.0.0.1 and ::1 as two addresses.
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

    /// NET-1 at one address (rule 2): idle sockets from the member's own
    /// address hold every slot, each reconnected the moment the listener
    /// closes it, and the member is still admitted, within the hang detector,
    /// by taking the slot of one that has gone `PREAUTH_GRACE` without proving
    /// anything, which the listener says. Every peer here is 127.0.0.1; a
    /// member from another address is the test above.
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

        // Control: with no attacker the member is admitted and served.
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

    /// Rule 2's address bound: a member from another address whose handshake
    /// outlasts `PREAUTH_GRACE` — a lossy path, here a ClientHello sent
    /// `PREAUTH_GRACE` + 500 ms after its connect, still inside
    /// [`HANDSHAKE_WITHIN`] — is never cut for the idle sockets' own
    /// reconnects, which may take only a slot of their own heavier address.
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

    /// The control for the rule: PROVED connections still fill the listener —
    /// [`MAX_SLOTS`] members hold every slot, a newcomer is turned away at
    /// capacity even once every member is older than `PREAUTH_GRACE`, and no
    /// member is displaced to make room for it.
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
            .map(|i| dial().unwrap_or_else(|e| panic!("member {i}: {e}")).0)
            .collect();
        // Older than the grace an unproved connection gets, so the age rule
        // would take a member's slot if a proved one were displaceable.
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

    /// ONE MEMBER NEVER FILLS THE LISTENER: an address that holds its share of
    /// proved connections ([`Listening::per_source`]) is answered `NO
    /// capacity` on the next proof — which the dialer reads as
    /// [`FleetError::AddressBusy`] — while a member from another address is
    /// still admitted to a free slot; and once one of the first address's
    /// connections ends, that address is admitted again. A dual-stack listener
    /// sees 127.0.0.1 and ::1 as two addresses.
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
        // One ends: its address is admitted again once its slot is free.
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

    /// A DRIVE HAS ITS OWN SHARE: an address that holds its reads' share
    /// ([`per_source`]) is still admitted a drive, and its drives — held as
    /// long as their request waits — are counted apart from its reads, up to
    /// [`drive_per_source`]; past that a drive is answered `NO capacity`
    /// (the NEGATIVE CONTROL), as a read past its own share is.
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

    /// A proved connection is served for its lifetime and then cut, so a
    /// member that went silent — asleep, off the network, or hung — gives its
    /// slot back: the silent dialer's read ends by the cut, never by its hang
    /// detector, and the one slot then serves the next member. The short
    /// lifetime is the outcome under test; it starts at the proof, so it
    /// never races a handshake.
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
        // Its thread lets the slot go as the cut ends its hand-over, which the
        // dialer's read does not wait for: dial until the slot is free.
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
