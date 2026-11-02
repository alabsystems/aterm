// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A member's desk (§3.3.1, §3.3.4): the requests it heard, the phrases it shows, and the
//! member's side of the `ask` exchange ([`serve`]).
//!
//! A request is heard on `ask` and shows as a row while its Mac asks: it lapses
//! [`STILL_ASKING`] after the last ask (a row whose phrase can still admit stays until the phrase
//! ends), goes when this Mac moves to another network ([`Desk::on_network`]), is ignored, or is
//! closed: by this member's run that let it in, by the new Mac's `done` after another member's
//! did, or by any admission this Mac reads (an `Admit` in the membership log, or the Commons
//! roster binding its key, [`Desk::refresh`]). A closed request that asks again shows again: the
//! new Mac lost the bundle it was handed. A row's handle is its request's digest, so a Mac that
//! asks again after its row lapsed shows under the same handle. Allow on a row shows a phrase bound to that request's `CI`
//! ([`Phrases::allow`]); invite shows one for whichever Mac uses it first. A run is CPace over
//! that phrase, then the possession challenge (step 8: `T_M` is checked by the new Mac before
//! it answers, and this member checks the answer before it signs anything), then the bundle
//! (step 9) under the run's bundle key. Runs are limited to [`RUNS_PER_MINUTE`] per source
//! address, counted when they start.
//!
//! On the pinned default ALab vault, a verified request from another host on this member's home
//! ALab LAN ([`Local::automatically_admits`]) is forwarded to the vault's join queue
//! ([`forward_automatic_requests`]), where the vault's own admission takes it; the vault's server
//! observes this member, not the asking Mac, so a request from any other network, however
//! private its address, is never forwarded and waits for a person's Allow
//! (`docs/DESIGN-automatic-lan-admission-2026-11-02.md`).

use super::wire::{self, Line};
use super::{Allowed, PhraseState};
use crate::client::{self, Paths};
use crate::commons::binding::{Pending, Request};
use crate::commons::bootstrap::{Bootstrap, Device};
use crate::commons::journal::Trust;
use crate::pake::{ChannelId, MemberSession, Nonce, PhraseId, Phrases, Share, Tag};
use crate::{Error, Result, SecretBytes};
use aterm_time::Instant;
use aterm_types::MutexExt;
use ring::signature::{ED25519, UnparsedPublicKey};
use std::io::{Read, Write};
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Duration;

/// How many requests a desk holds. Past it the oldest settled one (ignored, let in, or allowed
/// with a phrase that can no longer admit) goes, else an asking one of the sources that hold the
/// most asking rows ([`Desk::victim`] among them): a flood's, while it holds more rows from each
/// of its addresses than a Mac asking for itself holds from its own. A row whose phrase can still
/// admit never goes: at most [`crate::pake::MAX_LIVE`] rows hold one.
pub const MAX_HEARD: usize = 64;
/// The asking rows one source holds (an IPv4 address, an IPv6 /64): its next new request takes
/// the place of one of its own ([`Desk::victim`] among them), so one LAN host's flood never
/// pushes another Mac's row out.
pub const PER_SOURCE: usize = 4;
/// How long after its first ask a request that asks again counts as a waiting Mac's. A Mac
/// waiting to be let in asks each member again (its subscriber 1, 2, 4 and 8 minutes after its
/// first ask and then every 15, `aterm members join` every few seconds), and its row goes only
/// once no row that is not a waiting Mac's is left among those that may go ([`Desk::victim`]).
/// However often it asks, it shows one row.
pub const ASKED_AGAIN_AFTER: Duration = Duration::from_secs(30);
/// How long a row shows after its Mac's last ask: past twice the subscriber's longest pause
/// between asks (15 minutes), so one ask lost on the LAN keeps it, while a Mac that stopped
/// asking (or a host that asked its own requests again once) holds no row at all past it. A row
/// whose phrase can still admit stays until that phrase ends.
pub const STILL_ASKING: Duration = Duration::from_secs(32 * 60);
/// The CPace runs one source address may start a minute.
pub const RUNS_PER_MINUTE: usize = 3;
pub(super) const DONE_DOMAIN: &[u8] = b"aterm/alab/join-done/1\0";
pub(super) const RUN_DOMAIN: &[u8] = b"aterm/alab/join-run/1\0";
/// The bytes of the challenge a member's greeting carries, fresh on each connection.
pub(super) const CHALLENGE_LEN: usize = 16;

/// What the new Mac signs with its request's writer key to start a run: this member's key, the
/// challenge its greeting carried on this connection, the run's nonce, the node key and the
/// request. Only the Mac that holds the request's keys starts a run with the phrase a person
/// Allowed for it (a LAN host that copied the request from an `ask` cannot spend that phrase),
/// and a run line taken from one connection starts nothing on another.
pub(super) fn run_message(
    member: &[u8; 32],
    challenge: &[u8; CHALLENGE_LEN],
    nonce: &Nonce,
    node: &[u8; 32],
    request: &[u8],
) -> Vec<u8> {
    [RUN_DOMAIN, member, challenge, nonce, node, request].concat()
}

pub(super) const ASK_DOMAIN: &[u8] = b"aterm/alab/join-ask/1\0";

pub(super) const GREET_DOMAIN: &[u8] = b"aterm/alab/join-greet/1\0";
/// The bytes of the fresh nonce a new Mac's ask carries (`gnonce`), which the member's answer
/// signs.
pub(super) const GREET_NONCE_LEN: usize = 16;

/// What a member signs with its Commons writer key to answer an ask that carried `nonce`: the
/// nonce, every field of the greeting it sent on that connection, and its answer. A host that
/// copied the greeting cannot sign it, and a signature one connection got proves nothing on
/// another, since each ask brings a fresh nonce.
pub(super) fn greet_message(
    nonce: &[u8; GREET_NONCE_LEN],
    greeting: &super::joiner::Greeting,
    answer: Answer,
) -> Vec<u8> {
    let mut out = [
        GREET_DOMAIN,
        nonce,
        &greeting.key,
        &greeting.network,
        &greeting.authority,
        &greeting.genesis,
    ]
    .concat();
    // At most `wire::MAX_TEXT` bytes; its length keeps the boundary after it fixed.
    out.extend_from_slice(
        &u16::try_from(greeting.name.len())
            .unwrap_or(u16::MAX)
            .to_be_bytes(),
    );
    out.extend_from_slice(greeting.name.as_bytes());
    out.extend_from_slice(&[u8::from(greeting.can_allow), u8::from(greeting.invite)]);
    out.extend_from_slice(&greeting.challenge);
    out.extend_from_slice(answer.word().as_bytes());
    out
}

/// What the new Mac signs with its request's writer key to ask: this member's key, its node key,
/// the name and model it asks under, and the request. A host that copied the request cannot
/// show it under another node key (a phrase Allowed for that row would then admit nobody) or
/// another name (the one the Allowed carries).
pub(crate) fn ask_message(
    member: &[u8; 32],
    node: &[u8; 32],
    name: &str,
    model: &str,
    request: &[u8],
) -> Vec<u8> {
    let mut out = [ASK_DOMAIN, member, node].concat();
    for text in [name, model] {
        // Each is at most `wire`'s 64 bytes; the length keeps the boundary between them fixed.
        out.push(u8::try_from(text.len()).unwrap_or(u8::MAX));
        out.extend_from_slice(text.as_bytes());
    }
    out.extend_from_slice(request);
    out
}

pub(super) const PROVE_DOMAIN: &[u8] = b"aterm/alab/join-prove/1\0";
/// The record domain a Mac's fleet node key signs a proof under: the joiner signs with
/// `NodeIdentity::sign_record(PROVE_RECORD, &prove_message(..))`, which prefixes
/// [`PROVE_RECORD_PREFIX`].
pub const PROVE_RECORD: &str = "membership prove";
/// `aterm-fleet/v3 record ` (aterm-net's `RECORD_LABEL`), [`PROVE_RECORD`] and a NUL: what a
/// node key's record signature starts with, so a proof never meets a `Node` record's or a
/// channel's.
pub(super) const PROVE_RECORD_PREFIX: &[u8] = b"aterm-fleet/v3 record membership prove\0";

/// What a Mac that proves itself (`prove`) signs, with its request's writer key and with its
/// fleet node key: this member's key, the challenge its greeting carried on this connection,
/// the node key and the request. A proof taken from one connection proves nothing on another,
/// and a host that copied a request or a node key's public half signs neither.
#[must_use]
pub fn prove_message(
    member: &[u8; 32],
    challenge: &[u8; CHALLENGE_LEN],
    node: &[u8; 32],
    request: &[u8],
) -> Vec<u8> {
    [PROVE_DOMAIN, member, challenge, node, request].concat()
}

/// The source a request is counted under ([`PER_SOURCE`]): an IPv4 address (a mapped one
/// too), or an IPv6 address's /64, which one host can fill.
fn source_of(from: IpAddr) -> IpAddr {
    match from.to_canonical() {
        IpAddr::V6(v6) => {
            let mut octets = v6.octets();
            octets[8..].fill(0);
            IpAddr::from(octets)
        }
        v4 => v4,
    }
}

/// This Mac as a member: its Commons device, the network it belongs to, its name, and whether
/// it can allow: the membership log seats its device key (F15), or the current Commons roster
/// binds its writer key, which an admin's approver needs to check its `Allowed` (F10).
pub struct Local {
    device: Device,
    request: Request,
    trust: Trust,
    can_allow: bool,
    name: String,
    removed: client::RemovedNotes,
    admitted: std::collections::BTreeSet<crate::membership_log::MemberId>,
    /// The writer keys the current Commons roster binds: a Mac an approver enrolled from an
    /// `Allowed` (F10) is bound here once let in.
    bound: std::collections::BTreeSet<[u8; 32]>,
    /// The pinned default ALab vault served at a URL: requests from this member's home ALab LAN
    /// are forwarded to its join queue ([`Local::automatically_admits`]).
    automatic_lan: bool,
    /// The recipients the vault's signed swarm roster names (on the default ALab vault only).
    admitted_recipients: std::collections::BTreeSet<String>,
    /// This member's home ALab LAN when it read its place (`crate::home_lan::home_net`): only a
    /// host on it is forwarded.
    home: Option<crate::home_lan::HomeNet>,
    /// Where this Mac keeps its vault state ([`Local::of`]): a run takes that state
    /// ([`client::take_handover_hold`]) before it touches a phrase. `None` for a test's member
    /// ([`Local::with`]), whose runs hold nothing.
    paths: Option<Paths>,
}

impl Local {
    /// This Mac as a member of the network its applied vault names, `None` when it holds no
    /// Commons control (no vault, or not in it yet).
    ///
    /// # Errors
    /// Unreadable or corrupt local Commons state.
    pub fn of(paths: &Paths) -> Result<Option<Self>> {
        let Some(bootstrap) = Bootstrap::cached_until(
            paths,
            std::time::Instant::now() + std::time::Duration::from_secs(5),
        )?
        else {
            return Ok(None);
        };
        let Some(device) = Device::open(paths)? else {
            return Ok(None);
        };
        let access = bootstrap.access();
        let bound = match &access {
            Ok(Some(access)) => access
                .members()
                .iter()
                .map(|member| *member.binding().writer())
                .collect(),
            _ => std::collections::BTreeSet::new(),
        };
        let can_allow = matches!(access, Ok(Some(_))) || client::admits_by_log(paths);
        let admitted = client::log_admitted_actors(paths);
        let trust = bootstrap.trust();
        let request = device.request(trust.network, trust.authority)?;
        let config = client::config(paths)?;
        let automatic_lan = config.as_ref().is_some_and(|config| {
            client::alab_vault(config) && matches!(&config.source, crate::transport::Source::Url(_))
        });
        let name = config.map(|config| config.host).unwrap_or_default();
        // A removed Mac that asks again shows as removed, not new (F16): from the log this Mac
        // holds, read without the state lock (the records file is written whole).
        let removed = client::removed_notes(paths).unwrap_or_default();
        let admitted_recipients = if automatic_lan {
            client::swarm_roster(paths)?.map_or_else(Default::default, |roster| {
                roster
                    .members
                    .iter()
                    .filter(|member| !roster.is_member_denied(member))
                    .map(|member| member.recipient.clone())
                    .collect()
            })
        } else {
            Default::default()
        };
        // Read here, on the desk's lane or a connection thread, never a window's main thread.
        let home = if automatic_lan {
            crate::home_lan::home_net(paths)
        } else {
            None
        };
        Ok(Some(Self {
            device,
            request,
            trust,
            can_allow,
            name,
            removed,
            admitted,
            bound,
            automatic_lan,
            admitted_recipients,
            home,
            paths: Some(paths.clone()),
        }))
    }

    /// A member of `trust` with `device`, named `name`, that can allow or not: the seam a
    /// test builds a desk with under a scratch home.
    ///
    /// # Errors
    /// The device cannot sign its request.
    #[cfg(any(test, debug_assertions))]
    pub fn with(device: Device, trust: Trust, can_allow: bool, name: &str) -> Result<Self> {
        let request = device.request(trust.network, trust.authority)?;
        Ok(Self {
            device,
            request,
            trust,
            can_allow,
            name: name.to_owned(),
            removed: client::RemovedNotes::default(),
            admitted: std::collections::BTreeSet::new(),
            bound: std::collections::BTreeSet::new(),
            automatic_lan: false,
            admitted_recipients: Default::default(),
            home: None,
            paths: None,
        })
    }

    /// This member with `bound` as the writer keys its Commons roster binds: the seam a test
    /// lets a desk read an approver's enrollment with.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn with_bound(self, bound: std::collections::BTreeSet<[u8; 32]>) -> Self {
        Self { bound, ..self }
    }

    /// This member with `removed` as the Macs its log removed: the seam a test gives a desk
    /// built by [`Local::with`] its log's removals with.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn with_removed(self, removed: client::RemovedNotes) -> Self {
        Self { removed, ..self }
    }

    /// Supply authenticated LAN policy and admitted recipients in an isolated fixture, this member
    /// on the test home network ([`crate::home_lan::HomeNet::test`], `192.168.1.0/24`).
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn with_automatic_lan(self, admitted_recipients: impl IntoIterator<Item = String>) -> Self {
        Self {
            automatic_lan: true,
            admitted_recipients: admitted_recipients.into_iter().collect(),
            home: Some(crate::home_lan::HomeNet::test()),
            ..self
        }
    }

    /// This member on the home ALab LAN `home`, or on none: the seam a test moves it with.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn with_home(self, home: Option<crate::home_lan::HomeNet>) -> Self {
        Self { home, ..self }
    }

    /// Whether a request from `from` is covered by the default ALab LAN admission policy: this
    /// member serves the pinned default ALab vault, it was on its home ALab LAN when it last read
    /// its place, and `from` is another host on that network ([`crate::home_lan::HomeNet::holds`]).
    /// A private address alone is no home: a phone hotspot, a café, a VM bridge or Internet
    /// Sharing number their hosts privately too, and the vault's server observes the forwarding
    /// member, not the asking Mac, so the member judges the asking Mac's network here.
    #[must_use]
    pub fn automatically_admits(&self, from: IpAddr) -> bool {
        self.automatic_lan && self.home.is_some_and(|home| home.holds(from))
    }

    fn knows_recipient(&self, request: &Request) -> bool {
        self.automatic_lan
            && self
                .admitted_recipients
                .contains(&request.binding().recipient().to_bech32())
    }

    /// The writer key this member's greeting names and its `CI` binds.
    pub fn key(&self) -> &[u8; 32] {
        self.request.binding().writer()
    }

    /// Whether this Mac can allow: the log seats it, or the Commons roster binds it.
    pub const fn can_allow(&self) -> bool {
        self.can_allow
    }

    /// This Mac's machine name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The network this Mac is a member of.
    pub const fn trust(&self) -> Trust {
        self.trust
    }

    fn sign(&self, request: &Request, name: &str) -> Result<Allowed> {
        self.device
            .with_seed(|seed| Allowed::sign(request.binding(), name, seed))
    }
}

/// What a heard request is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowState {
    /// Waiting for admission through the automatic or manual path.
    Asking,
    /// Allowed here; its phrase is on this Mac's screen.
    Allowed(PhraseId),
    /// A person pressed Ignore here.
    Ignored,
    /// Let in, here or by another member.
    Closed,
}

/// One heard request, as the band shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// Its handle on this desk.
    pub id: u64,
    /// The machine name it asks for.
    pub name: String,
    /// The model it says it is.
    pub model: String,
    /// The address it asked from.
    pub from: IpAddr,
    /// Its join code, which the new Mac shows too.
    pub code: String,
    /// What it is doing.
    pub state: RowState,
    /// The membership log removed this Mac (by its recipient or its writer key): who did and
    /// when, so the band asks "Allow again?" instead of showing a new Mac (F16).
    pub removed: Option<client::RemovedNote>,
    /// A person pressed "Allow again": the run restores the Mac a plain removal took out.
    pub restore: bool,
    /// The default ALab policy forwards this LAN request without a person's approval.
    pub automatic: bool,
}

struct Heard {
    /// [`row_id`] of its request.
    id: u64,
    request: Request,
    node: [u8; 32],
    name: String,
    model: String,
    from: IpAddr,
    first: Instant,
    /// Whether it asked again [`ASKED_AGAIN_AFTER`] or more after `first`.
    again: bool,
    /// When it last asked.
    last: Instant,
    state: RowState,
    restore: bool,
    /// When the default ALab policy may forward it next ([`Desk::take_automatic_request`]).
    forward_after: Instant,
    /// The network this Mac was on when it last heard the request ([`Desk::on_network`]).
    network: Option<String>,
}

/// A row's handle: the first eight bytes of its request's SHA-256, so a Mac whose row lapsed
/// shows under the same handle when it asks again.
fn row_id(request: &Request) -> u64 {
    let digest = ring::digest::digest(&ring::digest::SHA256, request.bytes());
    let mut id = [0u8; 8];
    id.copy_from_slice(&digest.as_ref()[..8]);
    u64::from_be_bytes(id)
}

/// One `ask` line's fields, as [`Desk::hear`] checks them.
pub struct Asking<'a> {
    /// The request's bytes.
    pub wire: &'a [u8],
    /// The node key the asking Mac names.
    pub node: [u8; 32],
    /// The machine name it asks for.
    pub name: &'a str,
    /// The model it says it is.
    pub model: &'a str,
    /// Its writer key's signature over [`ask_message`] for this member.
    pub signature: &'a [u8; 64],
}

/// What `ask` answers the asking Mac.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    /// Waiting for a person (an Ignore reads the same).
    Asking,
    /// A person allowed it here: their phrase is on this member's screen.
    Allowed,
    /// It was let in.
    Closed,
}

impl Answer {
    pub(super) const fn word(self) -> &'static str {
        match self {
            Answer::Asking => "asking",
            Answer::Allowed => "allowed",
            Answer::Closed => "closed",
        }
    }

    pub(super) fn of_word(word: &str) -> Option<Self> {
        match word {
            "asking" => Some(Answer::Asking),
            "allowed" => Some(Answer::Allowed),
            "closed" => Some(Answer::Closed),
            _ => None,
        }
    }
}

/// One member's requests, phrases and run counts.
pub struct Desk {
    local: Local,
    heard: Vec<Heard>,
    phrases: Phrases,
    invites: Vec<PhraseId>,
    runs: Vec<(IpAddr, Instant)>,
    /// This Mac's network as [`Desk::on_network`] was last told it.
    network: Option<String>,
}

/// The fields of a `run` line, as [`Desk::begin`] checks them.
struct RunLine {
    wire: Vec<u8>,
    node: [u8; 32],
    name: String,
    nonce: Nonce,
    signature: [u8; 64],
}

/// The fields of a `prove` line, as [`Desk::begin_proof`] checks them.
struct ProveLine {
    wire: Vec<u8>,
    node: [u8; 32],
    signature: [u8; 64],
    node_signature: [u8; 64],
}

/// A run between [`Desk::begin`] and its end.
struct Running {
    phrase: PhraseId,
    request: Request,
    node: [u8; 32],
    name: String,
    reads_log: bool,
    restore: bool,
    /// The new Mac acknowledges its bundle (`ack=1`): the row closes only on that.
    acks: bool,
}

impl Desk {
    /// An empty desk for `local`.
    #[must_use]
    pub fn new(local: Local) -> Self {
        Self {
            local,
            heard: Vec::new(),
            phrases: Phrases::new(),
            invites: Vec::new(),
            runs: Vec::new(),
            network: None,
        }
    }

    /// The member this desk serves.
    pub const fn local(&self) -> &Local {
        &self.local
    }

    /// Replace the member (its roster binding, log admissions or name changed), keeping what it
    /// heard on the same network and closing rows whose Mac is now let in: an `Admit` in the
    /// membership log (§3.3.1 step 6), the Commons roster binding its writer key (an approver
    /// enrolled it from a member's `Allowed`), or, on the default ALab vault, its signed swarm
    /// roster naming its recipient.
    pub fn refresh(&mut self, local: Local) {
        if self.local.trust != local.trust {
            let network = self.network.take();
            *self = Self::new(local);
            self.network = network;
            return;
        }
        if !local.can_allow || self.local.key() != local.key() {
            for phrase in self.invites.drain(..) {
                self.phrases.cancel(phrase);
            }
            for heard in &mut self.heard {
                if let RowState::Allowed(phrase) = heard.state {
                    self.phrases.cancel(phrase);
                    heard.state = RowState::Asking;
                    heard.restore = false;
                }
            }
        }
        for heard in &mut self.heard {
            let binding = heard.request.binding();
            let actor = binding.actor();
            let admitted = local.admitted.contains(&actor) && !self.local.admitted.contains(&actor);
            let bound = local.bound.contains(binding.writer())
                && !self.local.bound.contains(binding.writer());
            let rostered = local.knows_recipient(&heard.request)
                && local
                    .removed
                    .of_with_node(&binding.recipient(), binding.writer(), &heard.node)
                    .is_none();
            if admitted || bound || rostered {
                if let RowState::Allowed(phrase) = heard.state {
                    self.phrases.cancel(phrase);
                }
                heard.state = RowState::Closed;
                heard.restore = false;
                continue;
            }
            if let RowState::Allowed(phrase) = heard.state
                && let Some(note) =
                    local
                        .removed
                        .of_with_node(&binding.recipient(), binding.writer(), &heard.node)
                && (!heard.restore || note.quorum)
            {
                self.phrases.cancel(phrase);
                heard.state = RowState::Asking;
                heard.restore = false;
            }
        }
        self.local = local;
    }

    fn checked(&self, wire: &[u8]) -> Result<Request> {
        let request = Request::decode(wire)?;
        let binding = request.binding();
        if binding.network() != &self.local.trust.network
            || binding.authority() != &self.local.trust.authority
        {
            return Err(Error::new("that Mac asks to join another network"));
        }
        if binding.writer() == self.local.key() {
            return Err(Error::new("that request is this Mac's own"));
        }
        Ok(request)
    }

    /// Drop the rows that lapsed ([`Desk::lapsed`]).
    fn expire(&mut self, now: Instant) {
        let mut at = self.heard.len();
        while at > 0 {
            at -= 1;
            if self.lapsed(&self.heard[at], now) {
                self.drop_row(at);
            }
        }
        self.runs
            .retain(|(_, at)| now.saturating_duration_since(*at) < Duration::from_secs(60));
    }

    fn find(&self, request: &Request) -> Option<usize> {
        self.heard
            .iter()
            .position(|heard| heard.request.bytes() == request.bytes())
    }

    fn answer(&self, at: usize, now: Instant) -> Answer {
        match self.heard[at].state {
            RowState::Closed => Answer::Closed,
            RowState::Allowed(id) if self.phrases.words(id, now).is_some() => Answer::Allowed,
            _ => Answer::Asking,
        }
    }

    /// Whether row `heard` holds a phrase that can still admit: live or mid-run, unexpired.
    fn admits(&self, heard: &Heard, now: Instant) -> bool {
        match heard.state {
            RowState::Allowed(phrase) => {
                matches!(
                    self.phrases.state(phrase),
                    Some(PhraseState::Live | PhraseState::Running)
                ) && self.phrases.expires(phrase).is_some_and(|end| now < end)
            }
            _ => false,
        }
    }

    /// Whether row `heard` lapsed at `now`: its Mac last asked [`STILL_ASKING`] or more ago,
    /// and it holds no phrase that can still admit.
    fn lapsed(&self, heard: &Heard, now: Instant) -> bool {
        now.saturating_duration_since(heard.last) >= STILL_ASKING && !self.admits(heard, now)
    }

    /// Whether row `heard` is settled: ignored, let in, or allowed with a phrase that can no
    /// longer admit (spent, expired, or gone from the book).
    fn settled(&self, heard: &Heard, now: Instant) -> bool {
        match heard.state {
            RowState::Ignored | RowState::Closed => true,
            RowState::Asking => false,
            RowState::Allowed(_) => !self.admits(heard, now),
        }
    }

    /// This Mac's network is now `network` ([`crate::heard::network_key`]): every row heard on
    /// another one goes, its phrase cancelled, since a Mac that asked there is not here to be let
    /// in (a row heard before this desk was told any network goes too). `None`, no network to
    /// name for now, changes nothing. How many rows went.
    pub fn on_network(&mut self, network: Option<String>) -> usize {
        let Some(network) = network else {
            return 0;
        };
        if self.network.as_deref() == Some(network.as_str()) {
            return 0;
        }
        let mut gone = 0;
        let mut at = self.heard.len();
        while at > 0 {
            at -= 1;
            if self.heard[at].network.as_deref() != Some(network.as_str()) {
                self.drop_row(at);
                gone += 1;
            }
        }
        self.network = Some(network);
        gone
    }

    /// Drop row `at`, cancelling its phrase if one still shows.
    fn drop_row(&mut self, at: usize) {
        if let RowState::Allowed(phrase) = self.heard.remove(at).state {
            self.phrases.cancel(phrase);
        }
    }

    /// Whether `heard` is a waiting Mac's at `now`: it asked again [`ASKED_AGAIN_AFTER`] or more
    /// after its first ask, and last asked within [`STILL_ASKING`].
    fn waiting(heard: &Heard, now: Instant) -> bool {
        heard.again && now.saturating_duration_since(heard.last) < STILL_ASKING
    }

    /// Which of the asking rows `rows` (oldest first) goes: the oldest that is not a waiting
    /// Mac's ([`Desk::waiting`]), else the oldest. A Mac that just asked is then the last of the
    /// rows that have not asked again to go, and a Mac waiting to be let in keeps its row while
    /// another is left.
    fn victim(&self, rows: &[usize], now: Instant) -> Option<usize> {
        rows.iter()
            .find(|&&at| !Self::waiting(&self.heard[at], now))
            .or_else(|| rows.first())
            .copied()
    }

    /// The asking rows `asking` (oldest first) of the sources that hold the most of them. A host
    /// whose flood fills the desk from fewer addresses than it holds rows holds more than one
    /// from each, so a Mac asking for itself from its own address (one row) is never among them
    /// while it does.
    fn crowded(&self, asking: &[usize]) -> Vec<usize> {
        let mut held = std::collections::BTreeMap::<IpAddr, usize>::new();
        for &at in asking {
            *held.entry(source_of(self.heard[at].from)).or_default() += 1;
        }
        let most = held.values().copied().max();
        asking
            .iter()
            .copied()
            .filter(|&at| held.get(&source_of(self.heard[at].from)).copied() == most)
            .collect()
    }

    /// Make room for a new request from `from`: one of its source's own asking rows once it
    /// holds [`PER_SOURCE`], else, at [`MAX_HEARD`], the oldest settled row or an asking one of
    /// the most crowded sources ([`Desk::crowded`], [`Desk::victim`]).
    fn make_room(&mut self, from: IpAddr, now: Instant) -> Result<()> {
        let source = source_of(from);
        let asking: Vec<usize> = (0..self.heard.len())
            .filter(|&at| self.heard[at].state == RowState::Asking)
            .collect();
        let theirs: Vec<usize> = asking
            .iter()
            .copied()
            .filter(|&at| source_of(self.heard[at].from) == source)
            .collect();
        if theirs.len() >= PER_SOURCE {
            if let Some(at) = self.victim(&theirs, now) {
                self.drop_row(at);
            }
            return Ok(());
        }
        if self.heard.len() < MAX_HEARD {
            return Ok(());
        }
        let settled = (0..self.heard.len()).find(|&at| self.settled(&self.heard[at], now));
        let at = settled
            .or_else(|| self.victim(&self.crowded(&asking), now))
            .ok_or_else(|| Error::new("this Mac holds as many join requests as it can now"))?;
        self.drop_row(at);
        Ok(())
    }

    /// Hear `asking` from `from` and say what its request is doing. A request heard again keeps
    /// its row, which keeps the node key, name and model its signature named; a closed one asks
    /// anew.
    ///
    /// # Errors
    /// The request is not a valid self-signed one for this member's network, its name or model
    /// is no such thing, its signature is not its writer key's over them, or no row can go.
    pub fn hear(&mut self, asking: &Asking<'_>, from: IpAddr, now: Instant) -> Result<Answer> {
        let Asking {
            wire,
            node,
            name,
            model,
            signature,
        } = *asking;
        let request = self.checked(wire)?;
        if !crate::text::valid_machine_name(name) {
            return Err(Error::new("that Mac asks under no machine name"));
        }
        if model.is_empty()
            || model.len() > super::wire::MAX_TEXT
            || !model.bytes().all(|b| b == b' ' || b.is_ascii_graphic())
        {
            return Err(Error::new("that Mac names no model"));
        }
        let message = ask_message(self.local.key(), &node, name, model, request.bytes());
        UnparsedPublicKey::new(&ED25519, request.binding().writer())
            .verify(&message, signature)
            .map_err(|_| Error::new("that ask is not signed by the Mac whose request it names"))?;
        self.expire(now);
        if self.local.knows_recipient(&request)
            && self
                .local
                .removed
                .of_with_node(
                    &request.binding().recipient(),
                    request.binding().writer(),
                    &node,
                )
                .is_none()
        {
            self.close(&request);
            return Ok(Answer::Closed);
        }
        if let Some(at) = self.find(&request) {
            let heard = &mut self.heard[at];
            if now.saturating_duration_since(heard.first) >= ASKED_AGAIN_AFTER {
                heard.again = true;
            }
            heard.last = heard.last.max(now);
            heard.network.clone_from(&self.network);
            // A Mac let in holds its bundle and asks no member again (`aterm members join` and
            // its subscriber stop at the Allowed it keeps), so a closed request that asks lost
            // its bundle between this member's send and its write: it asks anew, and an Allow
            // here can let it in again. A copied ask brings the row back at most; a run still
            // needs the request's keys.
            if self.heard[at].state == RowState::Closed {
                self.heard[at].state = RowState::Asking;
                self.heard[at].restore = false;
            }
            return Ok(self.answer(at, now));
        }
        self.make_room(from, now)?;
        self.heard.push(Heard {
            id: row_id(&request),
            request,
            node,
            name: name.to_owned(),
            model: model.to_owned(),
            from,
            first: now,
            again: false,
            last: now,
            state: RowState::Asking,
            restore: false,
            network: self.network.clone(),
            forward_after: now,
        });
        Ok(Answer::Asking)
    }

    /// The earliest-due verified request from this member's home ALab LAN. Only this request
    /// enters retry backoff: other machines keep their place and get a fresh submission budget
    /// on the next call. None whose Mac stopped asking ([`STILL_ASKING`]), was let go, or asked
    /// from another network qualifies. This grants no access; the vault's server observes the
    /// forwarding member, and its approver applies its signed policy.
    fn take_automatic_request(&mut self, now: Instant) -> Option<crate::transport::JoinRequest> {
        self.expire(now);
        let heard = self
            .heard
            .iter_mut()
            .filter(|heard| {
                Self::wants_automatic(&self.local, heard, now) && heard.forward_after <= now
            })
            .min_by_key(|heard| heard.forward_after)?;
        let binding = heard.request.binding();
        heard.forward_after = now + Duration::from_secs(30);
        Some(crate::transport::JoinRequest {
            recipient: binding.recipient().to_bech32(),
            host: heard.name.clone(),
            code: client::join_code(&binding.recipient()),
            time: None,
            lan: false,
            home: None,
            allowed: None,
            proof: None,
        })
    }

    /// When the next eligible LAN request can be forwarded, including the desk's retry
    /// backoff. A time at or before `now` is ready; none means no live request needs a post.
    /// This does not consume requests, so a worker can wait without another Mac asking again.
    #[must_use]
    pub fn next_automatic_request(&self, now: Instant) -> Option<Instant> {
        self.heard
            .iter()
            .filter(|heard| Self::wants_automatic(&self.local, heard, now))
            .map(|heard| heard.forward_after)
            .min()
    }

    fn wants_automatic(local: &Local, heard: &Heard, now: Instant) -> bool {
        let binding = heard.request.binding();
        local.automatically_admits(heard.from)
            && matches!(heard.state, RowState::Asking | RowState::Allowed(_))
            && now.saturating_duration_since(heard.last) < STILL_ASKING
            && local
                .removed
                .of_with_node(&binding.recipient(), binding.writer(), &heard.node)
                .is_none()
    }

    /// The rows the band shows, oldest first: none that lapsed ([`Desk::lapsed`]).
    #[must_use]
    pub fn rows(&self, now: Instant) -> Vec<Row> {
        self.heard
            .iter()
            .filter(|heard| !self.lapsed(heard, now))
            .map(|heard| Row {
                id: heard.id,
                name: heard.name.clone(),
                model: heard.model.clone(),
                from: heard.from,
                code: client::join_code(&heard.request.binding().recipient()),
                state: heard.state,
                removed: self.removed_note(heard).cloned(),
                restore: heard.restore,
                automatic: self.local.automatically_admits(heard.from)
                    && self.removed_note(heard).is_none(),
            })
            .collect()
    }

    /// Why this Mac's log keeps out the Mac `heard` binds, where it does.
    fn removed_note(&self, heard: &Heard) -> Option<&client::RemovedNote> {
        let binding = heard.request.binding();
        self.local
            .removed
            .of_with_node(&binding.recipient(), binding.writer(), &heard.node)
    }

    fn row_mut(&mut self, id: u64) -> Result<&mut Heard> {
        self.heard
            .iter_mut()
            .find(|heard| heard.id == id)
            .ok_or_else(|| Error::new("no such request on this Mac"))
    }

    fn may_allow(&self) -> Result<()> {
        if self.local.can_allow {
            Ok(())
        } else {
            Err(Error::new(
                "this Mac cannot allow yet: it has no Commons binding",
            ))
        }
    }

    /// Allow row `id`: a phrase bound to its request, for this Mac's own sheet.
    ///
    /// # Errors
    /// This Mac cannot allow, the row is gone or closed, the log removed its Mac (that row asks
    /// "Allow again?", [`Desk::allow_again`]), or four phrases are live already.
    pub fn allow(&mut self, id: u64, now: Instant) -> Result<PhraseId> {
        self.allow_as(id, now, false)
    }

    /// "Allow again" row `id`, whose Mac a member's plain removal took out (F16): a phrase as
    /// [`Desk::allow`] gives, and the run then restores the Mac with an `Admit` under its old
    /// label. A Mac a quorum `Remove` took out is not restored by one person.
    ///
    /// # Errors
    /// As [`Desk::allow`], or the log did not remove the row's Mac, or a quorum did.
    pub fn allow_again(&mut self, id: u64, now: Instant) -> Result<PhraseId> {
        self.allow_as(id, now, true)
    }

    fn allow_as(&mut self, id: u64, now: Instant, restore: bool) -> Result<PhraseId> {
        self.may_allow()?;
        {
            let heard = self
                .heard
                .iter()
                .find(|heard| heard.id == id)
                .ok_or_else(|| Error::new("no such request on this Mac"))?;
            match (self.removed_note(heard), restore) {
                (Some(note), false) => {
                    let title = note.title();
                    return Err(Error::new(format!(
                        "{}; \"Allow again\" restores it",
                        title.strip_suffix(". Allow again?").unwrap_or(&title)
                    )));
                }
                (Some(note), true) if note.quorum => {
                    return Err(Error::new(format!(
                        "a quorum of members removed {}, and one person's Allow does not bring \
                         it back",
                        note.label
                    )));
                }
                (None, true) => {
                    return Err(Error::new(
                        "the vault did not remove that Mac: Allow it as a new one",
                    ));
                }
                _ => {}
            }
        }
        let key = *self.local.key();
        let genesis = self.local.trust.genesis;
        let (ci, prev) = {
            let heard = self
                .heard
                .iter()
                .find(|heard| heard.id == id)
                .ok_or_else(|| Error::new("no such request on this Mac"))?;
            if heard.state == RowState::Closed {
                return Err(Error::new("that Mac was already let in"));
            }
            let prev = match heard.state {
                RowState::Allowed(prev) => Some(prev),
                _ => None,
            };
            (
                ChannelId::new(&key, heard.request.binding(), &heard.node, &genesis),
                prev,
            )
        };
        if let Some(prev) = prev {
            self.phrases.cancel(prev);
        }
        let phrase = self.phrases.allow(ci, now)?;
        let heard = self.row_mut(id)?;
        heard.state = RowState::Allowed(phrase);
        heard.restore = restore;
        Ok(phrase)
    }

    /// A phrase for whichever Mac uses it first (phrase first).
    ///
    /// # Errors
    /// This Mac cannot allow, or four phrases are live already.
    pub fn invite(&mut self, now: Instant) -> Result<PhraseId> {
        self.may_allow()?;
        let phrase = self.phrases.invite(now)?;
        self.invites.push(phrase);
        Ok(phrase)
    }

    /// Ignore row `id`: it stays heard, so asking again shows nothing new.
    ///
    /// # Errors
    /// The row is gone.
    pub fn ignore(&mut self, id: u64) -> Result<()> {
        let prev = match self.row_mut(id)?.state {
            RowState::Closed => return Ok(()),
            RowState::Allowed(prev) => Some(prev),
            RowState::Asking | RowState::Ignored => None,
        };
        if let Some(prev) = prev {
            self.phrases.cancel(prev);
        }
        let heard = self.row_mut(id)?;
        heard.state = RowState::Ignored;
        heard.restore = false;
        Ok(())
    }

    /// Withdraw phrase `phrase`, which no person saw because its sheet did not open: it is
    /// cancelled at once, so no run can use it and it gives up its place, and a row Allowed with
    /// it asks again.
    pub fn withdraw(&mut self, phrase: PhraseId) {
        self.phrases.cancel(phrase);
        self.invites.retain(|&id| id != phrase);
        for heard in &mut self.heard {
            if heard.state == RowState::Allowed(phrase) {
                heard.state = RowState::Asking;
                heard.restore = false;
            }
        }
    }

    /// The live phrase's words, for this Mac's own sheet and nothing else.
    #[must_use]
    pub fn words(&self, phrase: PhraseId, now: Instant) -> Option<[&'static str; 3]> {
        self.phrases.words(phrase, now)
    }

    /// What phrase `phrase` is doing.
    #[must_use]
    pub fn phrase_state(&self, phrase: PhraseId) -> Option<PhraseState> {
        self.phrases.state(phrase)
    }

    /// When phrase `phrase` stops admitting.
    #[must_use]
    pub fn expires(&self, phrase: PhraseId) -> Option<Instant> {
        self.phrases.expires(phrase)
    }

    fn close(&mut self, request: &Request) {
        if let Some(at) = self.find(request) {
            if let RowState::Allowed(prev) = self.heard[at].state {
                self.phrases.cancel(prev);
            }
            self.heard[at].state = RowState::Closed;
        }
    }

    /// Start the run `line` from `from` asks for: the phrase Allowed for its request if one is
    /// live, else the newest live invite. The run must be signed by the request's writer key
    /// over this connection's `challenge` ([`run_message`]) before any phrase is touched, so a
    /// Mac that only heard the request spends nothing.
    fn begin(
        &mut self,
        from: IpAddr,
        line: &RunLine,
        challenge: &[u8; CHALLENGE_LEN],
        now: Instant,
    ) -> Result<(Running, crate::pake::MemberRun, Nonce)> {
        let RunLine {
            wire,
            node,
            name,
            nonce,
            signature,
        } = line;
        let (node, name) = (*node, name.as_str());
        self.expire(now);
        self.may_allow()?;
        let source = source_of(from);
        if self.runs.iter().filter(|(at, _)| *at == source).count() >= RUNS_PER_MINUTE {
            return Err(Error::new(
                "too many join runs from that address; wait a minute",
            ));
        }
        self.runs.push((source, now));
        let request = self.checked(wire)?;
        let message = run_message(self.local.key(), challenge, nonce, &node, request.bytes());
        UnparsedPublicKey::new(&ED25519, request.binding().writer())
            .verify(&message, signature)
            .map_err(|_| Error::new("that run is not signed by the Mac whose request it names"))?;
        if self
            .find(&request)
            .is_some_and(|at| self.heard[at].state == RowState::Closed)
        {
            return Err(Error::new("that Mac was already let in"));
        }
        let ci = self.channel(&request, &node);
        let (phrase, name, restore) = self
            .phrase_for(&ci, name, now)
            .ok_or_else(|| Error::new("no join phrase is shown on this Mac for that Mac"))?;
        if let Some(note) = self.local.removed.of_with_node(
            &request.binding().recipient(),
            request.binding().writer(),
            &node,
        ) {
            if note.quorum {
                return Err(Error::new(format!(
                    "a quorum of members removed {}, and one person's Allow does not bring it back",
                    note.label
                )));
            }
            if !restore {
                let title = note.title();
                return Err(Error::new(format!(
                    "{}; \"Allow again\" restores it",
                    title.strip_suffix(". Allow again?").unwrap_or(&title)
                )));
            }
        }
        let (run, nonce) = self.phrases.begin(phrase, &ci, nonce, now)?;
        Ok((
            Running {
                phrase,
                request,
                node,
                name,
                reads_log: false,
                restore,
                acks: false,
            },
            run,
            nonce,
        ))
    }

    /// The run channel's `CI` for the Mac whose request is `request`, from fleet node `node`.
    fn channel(&self, request: &Request, node: &[u8; 32]) -> ChannelId {
        ChannelId::new(
            self.local.key(),
            request.binding(),
            node,
            &self.local.trust.genesis,
        )
    }

    /// The live phrase a run on channel `ci` takes, with the name and the restore it lets the
    /// Mac in under: the one a person Allowed for the request whose `CI` this is (a phrase typed
    /// into another Mac starts no run here), else the newest live invite, under the run's `name`.
    fn phrase_for(
        &self,
        ci: &ChannelId,
        name: &str,
        now: Instant,
    ) -> Option<(PhraseId, String, bool)> {
        let allowed = self.heard.iter().find_map(|heard| match heard.state {
            RowState::Allowed(phrase)
                if self.phrases.words(phrase, now).is_some()
                    && self.channel(&heard.request, &heard.node) == *ci =>
            {
                Some((phrase, allowed_name(&heard.name), heard.restore))
            }
            _ => None,
        });
        allowed.or_else(|| {
            self.invites
                .iter()
                .rev()
                .copied()
                .find(|phrase| self.phrases.words(*phrase, now).is_some())
                .map(|phrase| (phrase, name.to_owned(), false))
        })
    }

    /// How long the phrase the run line `first` would take ([`Desk::phrase_for`]) has left at
    /// `now`, for a busy answer (`left=`): none where no phrase would take it (the line names no
    /// request of this network, or no live phrase is shown for that Mac), so its Mac stops asking.
    fn phrase_left(&self, first: &Line<'_>, now: Instant) -> Duration {
        (|| {
            let request = self.checked(&first.b64("request").ok()?).ok()?;
            let ci = self.channel(&request, &first.hex::<32>("node").ok()?);
            let (phrase, _, _) = self.phrase_for(&ci, "", now)?;
            Some(self.phrases.expires(phrase)?.saturating_duration_since(now))
        })()
        .unwrap_or_default()
    }

    /// Check a `prove` line from `from` before anything else runs for it: within the runs one
    /// source may start a minute, a request of this network that is not this Mac's own, signed
    /// over this connection's `challenge` ([`prove_message`]) by its writer key and by the node
    /// key it names; where `fleet` names the node key the fleet's channel proved, that one.
    fn begin_proof(
        &mut self,
        from: IpAddr,
        line: &ProveLine,
        challenge: &[u8; CHALLENGE_LEN],
        fleet: Option<&[u8; 32]>,
        now: Instant,
    ) -> Result<Request> {
        self.expire(now);
        self.may_allow()?;
        let source = source_of(from);
        if self.runs.iter().filter(|(at, _)| *at == source).count() >= RUNS_PER_MINUTE {
            return Err(Error::new(
                "too many join runs from that address; wait a minute",
            ));
        }
        self.runs.push((source, now));
        let request = self.checked(&line.wire)?;
        if fleet.is_some_and(|proven| *proven != line.node) {
            return Err(Error::new(
                "that proof names another node key than the fleet's channel proved",
            ));
        }
        let message = prove_message(self.local.key(), challenge, &line.node, request.bytes());
        UnparsedPublicKey::new(&ED25519, request.binding().writer())
            .verify(&message, &line.signature)
            .map_err(|_| {
                Error::new("that proof is not signed by the Mac whose request it names")
            })?;
        UnparsedPublicKey::new(&ED25519, &line.node)
            .verify(
                &[PROVE_RECORD_PREFIX, &message].concat(),
                &line.node_signature,
            )
            .map_err(|_| Error::new("that proof is not signed by the node key it names"))?;
        Ok(request)
    }

    /// The new Mac's own word that it was let in (`done`), signed by its writer key over this
    /// member's key and its request: the row closes.
    fn done(&mut self, wire: &[u8], signature: &[u8; 64]) -> Result<()> {
        let request = self.checked(wire)?;
        let message = [DONE_DOMAIN, self.local.key(), request.bytes()].concat();
        UnparsedPublicKey::new(&ED25519, request.binding().writer())
            .verify(&message, signature)
            .map_err(|_| Error::new("that Mac's word that it was let in did not verify"))?;
        self.close(&request);
        Ok(())
    }
}

/// The member's greeting: its keys, its name (as the line carries it), whether it can allow,
/// whether it shows an invite (phrase first) now, and this connection's `challenge`, which a run
/// signs.
fn greeting(desk: &Desk, challenge: &[u8; CHALLENGE_LEN], now: Instant) -> super::joiner::Greeting {
    let local = &desk.local;
    super::joiner::Greeting {
        key: *local.key(),
        network: local.trust.network,
        authority: local.trust.authority,
        genesis: local.trust.genesis,
        name: wire::unescape(&wire::escape(&local.name)).unwrap_or_default(),
        can_allow: local.can_allow,
        invite: desk
            .invites
            .iter()
            .any(|phrase| desk.phrases.words(*phrase, now).is_some()),
        challenge: *challenge,
    }
}

/// The line that carries `greeting`.
fn greeting_line(greeting: &super::joiner::Greeting) -> String {
    format!(
        "ALAB member=1 key={} network={} authority={} genesis={} name={} allow={} invite={} \
         challenge={}",
        wire::hex(&greeting.key),
        wire::hex(&greeting.network),
        wire::hex(&greeting.authority),
        wire::hex(&greeting.genesis),
        wire::escape(&greeting.name),
        u8::from(greeting.can_allow),
        u8::from(greeting.invite),
        wire::hex(&greeting.challenge)
    )
}

/// The `OK` line answering an ask with `answer`: where the ask carried a fresh nonce (`gnonce`),
/// signed (`gsig`) with this member's Commons writer key over it, the greeting `greeted` this
/// connection carried and the answer ([`greet_message`]), so the new Mac knows the answer came
/// from the key the greeting names. An ask with no nonce (a Mac from before signed greetings)
/// gets the answer alone.
fn answer_line(
    desk: &Mutex<Desk>,
    line: &Line<'_>,
    greeted: &super::joiner::Greeting,
    answer: Answer,
) -> Result<String> {
    let word = answer.word();
    if line.get("gnonce").is_err() {
        return Ok(format!("OK state={word}"));
    }
    let message = greet_message(&line.hex::<GREET_NONCE_LEN>("gnonce")?, greeted, answer);
    let signature = super::joiner::sign(&desk.lock_or_recover().local.device, &message)?;
    Ok(format!(
        "OK state={word} gsig={}",
        wire::hex(signature.as_ref())
    ))
}

/// How a run's bundle reached the new Mac ([`Let::delivered`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Delivered {
    /// The new Mac acknowledged the bundle it opened (its run said `ack=1`): its row closed.
    Acknowledged,
    /// The new Mac asked for no acknowledgement (a joiner from before them): its row closed
    /// once the bundle was written, as before.
    Written,
    /// The new Mac asked to acknowledge the bundle and did not (its link dropped after the
    /// write, or a relay cut it): sent, not acknowledged. The row stays allowed, its phrase
    /// spent, so Allow again works at once. Why.
    Unacknowledged(String),
}

/// What a finished run hands its caller: the Allowed this member signed, for the servers, and
/// whether the bundle carried the membership log.
pub struct Let {
    /// The member's signed Allow.
    pub allowed: Allowed,
    /// The bundle carried the member's `Admit` and the log ([`super::bundle::Handed::Log`]):
    /// the caller posts this Mac's records to the vault's servers.
    pub logged: bool,
    /// Why a member that holds a log handed F10's files instead ([`Handout::note`]), for the
    /// caller's log.
    pub note: Option<String>,
    /// Whether the new Mac acknowledged its bundle. The caller posts the records and the
    /// Allowed however it went (the `Admit` is in this Mac's records already), and logs an
    /// unacknowledged bundle as sent, not as a Mac let in.
    pub delivered: Delivered,
}

/// What a run that proved the new Mac's keys gives its caller to build the bundle from.
pub struct Handing<'a> {
    /// The new Mac's self-signed request: its binding, proved by the possession challenge.
    pub request: &'a Request,
    /// Its fleet node key, which its request's `CI` bound.
    pub node: [u8; 32],
    /// The machine name it was allowed under.
    pub name: &'a str,
    /// The Allow this member signed for it.
    pub allowed: &'a Allowed,
    /// It reads `alab-join-bundle/2`, the membership log ([`super::bundle::Handed::Log`]): it
    /// said so when it started the run. A Mac that did not gets the `[everyone]` files.
    pub reads_log: bool,
    /// A person pressed "Allow again" for it: the log removed it, and a plain removal does not
    /// keep it out of this run (F16).
    pub restore: bool,
    /// This Mac's vault state, which the desk took before the run touched its phrase
    /// ([`client::take_handover_hold`]): [`client::hand_over`] builds the bundle under it and
    /// takes the state no second time. `None` where the desk holds nothing (a test's member).
    pub held: Option<&'a client::HandoverHold>,
}

/// What the caller hands back for a run: the encoded bundle, and whether it carries the log.
pub struct Handout {
    /// [`super::Bundle::encode`]'s text.
    pub bundle: SecretBytes,
    /// It is a [`super::bundle::Handed::Log`] bundle.
    pub logged: bool,
    /// Why it is F10's files where this member holds a membership log (its log path failed), for
    /// the caller's log; `None` for a log bundle and where no log applies.
    pub note: Option<String>,
}

/// How a desk's caller builds a run's bundle ([`serve`]).
pub type Hand<'a> = &'a dyn Fn(&Handing<'_>) -> Result<Handout>;

/// Forward one due verified LAN ask to the default vault's queues, including verified mirrors.
/// Other requests stay due for a separate call and budget. Returns how many requests were
/// submitted: zero or one. The receiver does not grant membership or read an approval phrase.
/// # Errors
/// The configured vault cannot be read, or a bounded queue submission fails.
pub fn forward_automatic_requests(desk: &Mutex<Desk>, paths: &client::Paths) -> Result<usize> {
    forward_automatic_requests_if(desk, paths, &|| true)
}

/// Forward one automatic request while its owning service still considers this desk current.
/// The check runs before taking the request and before posting, without the desk lock. A post
/// already in flight can finish; cancellation prevents starting a new one. The request's
/// eligibility, including freshness, is checked again before its post.
///
/// # Errors
/// The configured vault cannot be read, or a bounded queue submission fails.
pub fn forward_automatic_requests_if(
    desk: &Mutex<Desk>,
    paths: &client::Paths,
    current: &dyn Fn() -> bool,
) -> Result<usize> {
    if !current() {
        return Ok(0);
    }
    let Some(request) = desk
        .lock_or_recover()
        .take_automatic_request(Instant::now())
    else {
        return Ok(0);
    };
    let Some(config) = client::config(paths)?.filter(client::alab_vault) else {
        return Ok(0);
    };
    if !matches!(&config.source, crate::transport::Source::Url(_)) {
        return Ok(0);
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    forward_selected(desk, request, current, &Instant::now, |request| {
        client::join_routing::post(paths, &config, &request.line(), Some(deadline))
    })
}

/// Forward one due request with a supplied clock and post operation: an isolated worker test's
/// transport, sharing the production desk's queue, eligibility checks and backoff.
///
/// # Errors
/// A supplied post failed.
#[cfg(any(test, debug_assertions))]
pub fn forward_automatic_with(
    desk: &Mutex<Desk>,
    now: Instant,
    post: impl FnMut(&crate::transport::JoinRequest) -> Result<()>,
) -> Result<usize> {
    let Some(request) = desk.lock_or_recover().take_automatic_request(now) else {
        return Ok(0);
    };
    forward_selected(desk, request, &|| true, &|| now, post)
}

fn forward_selected(
    desk: &Mutex<Desk>,
    request: crate::transport::JoinRequest,
    current: &dyn Fn() -> bool,
    now: &dyn Fn() -> Instant,
    mut post: impl FnMut(&crate::transport::JoinRequest) -> Result<()>,
) -> Result<usize> {
    if !current() {
        return Ok(0);
    }
    let still_allowed = {
        let held = desk.lock_or_recover();
        let now = now();
        held.heard.iter().any(|heard| {
            Desk::wants_automatic(&held.local, heard, now)
                && heard.name == request.host
                && heard.request.binding().recipient().to_bech32() == request.recipient
        })
    };
    if !still_allowed {
        return Ok(0);
    }
    post(&request)?;
    Ok(1)
}

/// Serve one `ask` connection from `from`: greet, then answer one `ask`, `done`, or a whole
/// run. `bundle` makes the bundle for a run ([`Handing`]); the run's phrase burns on any end
/// but a sent bundle, once the run touched it: a member whose vault pass holds its state answers
/// a run busy before that ([`wire::busy_refusal`]), and the phrase stays live. A run that sent
/// its bundle returns the Allowed it signed, for the caller to post to the vault's servers, and
/// whether the bundle carried the log.
///
/// # Errors
/// The stream failed, or the other Mac sent something this exchange does not take. Refusals
/// are sent as a `NO` line first.
pub fn serve<S: Read + Write + ?Sized>(
    desk: &Mutex<Desk>,
    stream: &mut S,
    from: IpAddr,
    bundle: Hand<'_>,
) -> Result<Option<Let>> {
    serve_with(desk, stream, from, bundle, None)
}

/// What a desk's caller decides of a Mac that proved itself (`prove`): the log bundle it hands
/// (`client::admit_by_proof`), or why not, which the Mac reads.
pub type Prove<'a> = &'a dyn Fn(&Proving<'_>) -> Result<SecretBytes>;

/// Why a desk's caller refuses a Mac before its possession proof, whatever it proves next
/// ([`Proof::refuses`]); `None` where the exchange goes on.
pub type Refuses<'a> = &'a dyn Fn(&Proving<'_>) -> Option<String>;

/// A Mac that proved itself on one connection ([`Prove`]).
pub struct Proving<'a> {
    /// Its self-signed request: the possession challenge proved its age identity and writer key.
    pub request: &'a Request,
    /// Its fleet node key, which signed this connection's fresh challenge.
    pub node: [u8; 32],
    /// The machine name it asks under.
    pub name: &'a str,
    /// The fleet's channel proved the fleet's current key and this node key, and the caller's
    /// drive admission took it ([`Proof::fleet`]).
    pub fleet: bool,
}

/// How a desk's caller serves `prove` on one connection ([`serve_with`]).
#[derive(Clone, Copy)]
pub struct Proof<'a> {
    /// Decides and hands the bundle; called with no lock of the desk held.
    pub decide: Prove<'a>,
    /// Why this member refuses the Mac whatever it proves next, asked before the possession
    /// challenge (`client::refuses_before_possession`); called with no lock of the desk held.
    pub refuses: Refuses<'a>,
    /// The node key the fleet's channel proved on this connection, where it came over the fleet
    /// (`vault-join`): the proof must name it, and [`Proving::fleet`] is set.
    pub fleet: Option<[u8; 32]>,
}

/// [`serve`], also taking a `prove` line where `proof` is given: the Mac signs this
/// connection's challenge with its writer key and its fleet node key, proves its age identity by
/// the possession challenge, and `proof.decide` hands the log bundle with nobody's Allow, sealed
/// to that age identity. A `prove` with no `proof` is refused.
///
/// # Errors
/// As [`serve`].
pub fn serve_with<S: Read + Write + ?Sized>(
    desk: &Mutex<Desk>,
    stream: &mut S,
    from: IpAddr,
    bundle: Hand<'_>,
    proof: Option<Proof<'_>>,
) -> Result<Option<Let>> {
    let mut challenge = [0u8; CHALLENGE_LEN];
    if let Err(why) = aterm_uds::rand::fill(&mut challenge) {
        let error = Error::new(format!("no randomness for a join challenge: {why}"));
        let _ = wire::write_line(stream, &wire::refusal(&error.to_string()));
        return Err(error);
    }
    let greeted = greeting(&desk.lock_or_recover(), &challenge, Instant::now());
    wire::write_line(stream, &greeting_line(&greeted))?;
    let line = wire::read_line(stream)?;
    let parsed = Line::parse(&line);
    let answered = match parsed.verb {
        "ask" => {
            ask(desk, &parsed, from).and_then(|answer| answer_line(desk, &parsed, &greeted, answer))
        }
        "done" => done(desk, &parsed).map(|()| format!("OK state={}", Answer::Closed.word())),
        "run" => return run(desk, stream, &parsed, from, &challenge, bundle),
        "prove" if let Some(proof) = proof => {
            return prove(desk, stream, &parsed, from, &challenge, proof).map(|()| None);
        }
        _ => Err(Error::new(
            "the other Mac asked for something this Mac does not serve",
        )),
    };
    match answered {
        Ok(answer) => {
            wire::write_line(stream, &answer)?;
            Ok(None)
        }
        Err(error) => {
            let _ = wire::write_line(stream, &wire::refusal(&error.to_string()));
            Err(error)
        }
    }
}

fn ask(desk: &Mutex<Desk>, line: &Line<'_>, from: IpAddr) -> Result<Answer> {
    let wire = line.b64("request")?;
    let name = line.text("name")?;
    let model = line.text("model")?;
    let asking = Asking {
        wire: &wire,
        node: line.hex::<32>("node")?,
        name: &name,
        model: &model,
        signature: &line.hex::<64>("sig")?,
    };
    desk.lock_or_recover().hear(&asking, from, Instant::now())
}

fn done(desk: &Mutex<Desk>, line: &Line<'_>) -> Result<()> {
    let wire = line.b64("request")?;
    let signature = line.hex::<64>("sig")?;
    desk.lock_or_recover().done(&wire, &signature)
}

/// The machine name an Allowed carries for a request asking under `asked`.
fn allowed_name(asked: &str) -> String {
    let lower = asked.to_ascii_lowercase();
    if crate::text::valid_machine_name(&lower) {
        lower
    } else {
        "machine".to_owned()
    }
}

/// What a member busy with a vault pass answers a run, in a [`wire::busy_refusal`]: its phrase
/// was not touched, so the same words work once the pass ends (the joiner tries again by itself).
pub(super) const BUSY: &str =
    "a vault pass on this Mac holds its state for now; the same phrase works again in a moment";

fn run<S: Read + Write + ?Sized>(
    desk: &Mutex<Desk>,
    stream: &mut S,
    first: &Line<'_>,
    from: IpAddr,
    challenge: &[u8; CHALLENGE_LEN],
    bundle: Hand<'_>,
) -> Result<Option<Let>> {
    // This Mac's vault state, taken before the run touches its phrase (and outside the desk's
    // lock): a vault pass that holds it past `HANDOVER_LOCK_WAIT` gets the run a busy answer that
    // leaves the phrase live and the row allowed, and once taken no pass takes it before the
    // bundle is built.
    let paths = desk.lock_or_recover().local.paths.clone();
    let held = match paths.as_ref().map(client::take_handover_hold).transpose() {
        Ok(Some(None)) => {
            // How long the phrase this run would take has left, so the new Mac asks again only
            // while it can live.
            let left = desk.lock_or_recover().phrase_left(first, Instant::now());
            let _ = wire::write_line(stream, &wire::busy_refusal(BUSY, left));
            return Err(Error::of_kind(crate::ErrorKind::MemberBusy, BUSY));
        }
        Ok(held) => held.flatten(),
        Err(error) => {
            let _ = wire::write_line(stream, &wire::refusal(&error.to_string()));
            return Err(error);
        }
    };
    let started = (|| {
        let line = RunLine {
            wire: first.b64("request")?,
            node: first.hex::<32>("node")?,
            name: allowed_name(&first.text("name")?),
            nonce: first.hex::<{ crate::pake::NONCE_LEN }>("nonce")?,
            signature: first.hex::<64>("sig")?,
        };
        // `reads=2`: the new Mac reads F15's log bundle (`alab-join-bundle/2`).
        let reads_log = first.get("reads").is_ok_and(|reads| reads == "2");
        // `ack=1`: the new Mac acknowledges the bundle it opened ([`Delivered::Acknowledged`]).
        let acks = first.get("ack").is_ok_and(|ack| ack == "1");
        desk.lock_or_recover()
            .begin(from, &line, challenge, Instant::now())
            .map(|(running, run, nonce)| {
                (
                    Running {
                        reads_log,
                        acks,
                        ..running
                    },
                    run,
                    nonce,
                )
            })
    })();
    let (running, member_run, nonce) = match started {
        Ok(started) => started,
        Err(error) => {
            let _ = wire::write_line(stream, &wire::refusal(&error.to_string()));
            return Err(error);
        }
    };
    let phrase = running.phrase;
    let ended = steps(desk, stream, running, member_run, &nonce, (bundle, held));
    if let Err(error) = &ended {
        desk.lock_or_recover().phrases.burn(phrase);
        let _ = wire::write_line(stream, &wire::refusal(&error.to_string()));
    }
    ended.map(Some)
}

fn steps<S: Read + Write + ?Sized>(
    desk: &Mutex<Desk>,
    stream: &mut S,
    running: Running,
    member_run: crate::pake::MemberRun,
    nonce: &Nonce,
    (bundle, held): (Hand<'_>, Option<client::HandoverHold>),
) -> Result<Let> {
    wire::write_line(stream, &format!("OK nonce={}", wire::hex(nonce)))?;
    let line = wire::read_line(stream)?;
    let share = expect(&line, "share")?.hex::<32>("y")?;
    let (confirming, ours): (_, Share) =
        desk.lock_or_recover().phrases.share(member_run, &share)?;
    wire::write_line(stream, &format!("OK y={}", wire::hex(&ours)))?;
    let line = wire::read_line(stream)?;
    let tag: Tag = expect(&line, "confirm")?.hex::<32>("t")?;
    let (session, ours): (MemberSession, Tag) =
        desk.lock_or_recover()
            .phrases
            .confirm(confirming, &tag, Instant::now())?;
    // Step 8: the possession challenge, under the network this member pins.
    let trust = desk.lock_or_recover().local.trust;
    let (mut pending, challenge) =
        Pending::issue(&running.request, trust.network, trust.authority)?;
    wire::write_line(
        stream,
        &format!(
            "OK t={} challenge={}",
            wire::hex(&ours),
            wire::b64(&challenge)
        ),
    )?;
    let line = wire::read_line(stream)?;
    let response = expect(&line, "possess")?.b64("response")?;
    let possession = pending.verify(&response)?;
    if possession.binding() != running.request.binding() {
        return Err(Error::new("the possession proof names another Mac"));
    }
    let allowed = desk
        .lock_or_recover()
        .local
        .sign(&running.request, &running.name)?;
    let handout = bundle(&Handing {
        request: &running.request,
        node: running.node,
        name: &running.name,
        allowed: &allowed,
        reads_log: running.reads_log,
        restore: running.restore,
        held: held.as_ref(),
    })?;
    // The bundle is built: a vault pass may take the state while it travels.
    drop(held);
    let (sealed, acknowledging) = if running.acks {
        let (sealed, acknowledging) =
            session.seal_bundle_acknowledged(handout.bundle.as_slice())?;
        (sealed, Some(acknowledging))
    } else {
        (session.seal_bundle(handout.bundle.as_slice())?, None)
    };
    wire::write_line(stream, &format!("OK bundle={}", sealed.len()))?;
    stream.write_all(&sealed)?;
    stream.flush()?;
    // A bundle written into a relay's or a socket's buffer has not reached the new Mac: where it
    // acknowledges, the row closes only on its acknowledgement, and stays allowed (its phrase
    // spent) otherwise, so a person's Allow again works at once.
    let delivered = match acknowledging {
        None => Delivered::Written,
        Some(acknowledging) => match acknowledged(stream, &acknowledging) {
            Ok(()) => Delivered::Acknowledged,
            Err(error) => Delivered::Unacknowledged(error.to_string()),
        },
    };
    if !matches!(delivered, Delivered::Unacknowledged(_)) {
        desk.lock_or_recover().close(&running.request);
    }
    Ok(Let {
        allowed,
        logged: handout.logged,
        note: handout.note,
        delivered,
    })
}

/// Read the new Mac's acknowledgement of its bundle (`ack t=…`) and check it.
fn acknowledged<S: Read + ?Sized>(
    stream: &mut S,
    acknowledging: &crate::pake::Acknowledging,
) -> Result<()> {
    let line = wire::read_line(stream)?;
    acknowledging.check(&expect(&line, "ack")?.hex::<{ crate::pake::TAG_LEN }>("t")?)
}

fn prove<S: Read + Write + ?Sized>(
    desk: &Mutex<Desk>,
    stream: &mut S,
    first: &Line<'_>,
    from: IpAddr,
    challenge: &[u8; CHALLENGE_LEN],
    proof: Proof<'_>,
) -> Result<()> {
    let ended = (|| {
        let asked = ProveLine {
            wire: first.b64("request")?,
            node: first.hex::<32>("node")?,
            signature: first.hex::<64>("sig")?,
            node_signature: first.hex::<64>("nodesig")?,
        };
        let name = allowed_name(&first.text("name")?);
        let (request, trust) = {
            let mut held = desk.lock_or_recover();
            let request = held.begin_proof(
                from,
                &asked,
                challenge,
                proof.fleet.as_ref(),
                Instant::now(),
            )?;
            (request, held.local.trust)
        };
        let proving = Proving {
            request: &request,
            node: asked.node,
            name: &name,
            fleet: proof.fleet.is_some(),
        };
        // Before any possession challenge: a Mac this member refuses whatever it proves next is told
        // so while it has sent nothing a member could sign on, so the next member and its plain ask
        // may follow. The desk's lock is not held: this reads the vault's state.
        if let Some(why) = (proof.refuses)(&proving) {
            return Err(Error::new(why));
        }
        // The possession challenge, under the network this member pins.
        let (mut pending, possess) = Pending::issue(&request, trust.network, trust.authority)?;
        wire::write_line(stream, &format!("OK challenge={}", wire::b64(&possess)))?;
        let line = wire::read_line(stream)?;
        let response = expect(&line, "possess")?.b64("response")?;
        if pending.verify(&response)?.binding() != request.binding() {
            return Err(Error::new("the possession proof names another Mac"));
        }
        // The desk's lock is not held here: the decision reads and writes the vault's state.
        let bundle = (proof.decide)(&proving)?;
        let sealed = crate::age::encrypt(&[request.binding().recipient()], bundle.as_slice())?;
        wire::write_line(stream, &format!("OK bundle={}", sealed.len()))?;
        stream.write_all(&sealed)?;
        stream.flush()?;
        desk.lock_or_recover().close(&request);
        Ok(())
    })();
    if let Err(error) = &ended {
        let _ = wire::write_line(stream, &wire::refusal(&error.to_string()));
    }
    ended
}

fn expect<'a>(line: &'a str, verb: &str) -> Result<Line<'a>> {
    let parsed = Line::parse(line);
    if parsed.verb == verb {
        Ok(parsed)
    } else {
        Err(Error::new(format!(
            "the other Mac sent {} where this Mac waited for {verb}",
            parsed.verb
        )))
    }
}

#[cfg(test)]
mod automatic_tests {
    use super::*;

    const TRUST: Trust = Trust {
        network: [1; 16],
        authority: [2; 32],
        genesis: [3; 32],
    };

    fn device(root: &std::path::Path, name: &str) -> Device {
        let paths = client::Paths::for_home(&root.join(name));
        client::machine_identity(&paths).unwrap();
        Device::open(&paths).unwrap().unwrap()
    }

    fn ask(desk: &mut Desk, device: &Device, name: &str, from: IpAddr, now: Instant) -> Request {
        let request = device.request(TRUST.network, TRUST.authority).unwrap();
        let node = [7; 32];
        let signature = super::super::joiner::ask_signature(
            device,
            desk.local.key(),
            &node,
            name,
            "Mac",
            request.bytes(),
        )
        .unwrap();
        desk.hear(
            &Asking {
                wire: request.bytes(),
                node,
                name,
                model: "Mac",
                signature: &signature,
            },
            from,
            now,
        )
        .unwrap();
        request
    }

    /// Forward through the real client configuration and independent server queues. A member's
    /// desk must use the same verified mirror route as a joining Mac, whether its configured
    /// queue accepts the request or has stopped before the next Mac asks.
    #[test]
    fn a_members_automatic_requests_reach_the_verified_mirror_of_its_configured_queue() {
        use crate::transport::{self, Fetched, Source};

        fn server(root: &std::path::Path) -> crate::server::Running {
            crate::server::start_at_home(
                std::net::TcpListener::bind("127.0.0.1:0").unwrap(),
                &crate::server::Options {
                    listen: "127.0.0.1:0".parse().unwrap(),
                    store: root.to_owned(),
                    advertise: None,
                    pins: Vec::new(),
                    hold: Duration::from_millis(200),
                    keep_awake: false,
                },
                Duration::ZERO,
            )
            .unwrap()
        }

        struct RestorePin(Option<crate::envelope::Retired>);
        impl Drop for RestorePin {
            fn drop(&mut self) {
                crate::envelope::TEST_RETIRED.with(|cell| *cell.borrow_mut() = self.0.take());
            }
        }

        let root = aterm_tempfile::tempdir().unwrap();
        let mut primary = Some(server(&root.path().join("primary")));
        let mirror = server(&root.path().join("mirror"));
        let primary_url = format!("http://{}/v1/team", primary.as_ref().unwrap().addr);
        let mirror_url = format!("http://{}/v1/team", mirror.addr);
        let paths = Paths::for_home(&root.path().join("admin"));
        client::create(&paths, &primary_url, None, Some("member")).unwrap();
        let admin = client::admin_open(&paths).unwrap();
        let mut document = admin.doc.clone();
        let policy = crate::swarm_roster::SwarmPolicy {
            servers: vec![mirror_url.clone()],
            ..Default::default()
        };
        document
            .set_entry(
                &crate::text::Audience::Everyone,
                crate::SWARM_POLICY_ENTRY,
                policy.to_toml().as_bytes(),
            )
            .unwrap();
        client::admin_publish(&paths, &admin, &document).unwrap();
        let Fetched::Body(envelope) =
            transport::fetch(&admin.config.source, None, Duration::ZERO).unwrap()
        else {
            panic!("the configured server must hold the signed vault");
        };
        transport::publish(&Source::Url(mirror_url.clone()), &envelope).unwrap();
        // Only this test thread treats the scratch authority as the compiled ALab pin.
        let _restore = RestorePin(crate::envelope::TEST_RETIRED.with(|cell| {
            cell.replace(Some(crate::envelope::Retired {
                keys: Vec::new(),
                current: admin.config.signer,
            }))
        }));
        assert!(client::alab_vault(&admin.config));

        for (name, stop_primary) in [("first", false), ("second", true)] {
            if stop_primary {
                primary.take().unwrap().stop();
            }
            let local = Local::with(device(root.path(), "member"), TRUST, true, "member")
                .unwrap()
                .with_automatic_lan([]);
            let desk = Mutex::new(Desk::new(local));
            let request = ask(
                &mut desk.lock_or_recover(),
                &device(root.path(), name),
                name,
                "192.168.1.45".parse().unwrap(),
                Instant::now(),
            );
            assert_eq!(forward_automatic_requests(&desk, &paths).unwrap(), 1);
            let recipient = request.binding().recipient().to_bech32();
            assert!(
                transport::list_joins(&mirror_url)
                    .unwrap()
                    .iter()
                    .any(|line| line.starts_with(&recipient)),
                "{name} must reach the mirror while its configured server is {}",
                if stop_primary { "offline" } else { "online" }
            );
        }
        mirror.stop();
    }

    #[test]
    fn automatic_lan_requests_reach_the_vault_queue_without_a_manual_allow() {
        let root = aterm_tempfile::tempdir().unwrap();
        let local = Local::with(device(root.path(), "member"), TRUST, true, "member")
            .unwrap()
            .with_automatic_lan([]);
        let mut desk = Desk::new(local);
        let now = Instant::now();
        let joining = device(root.path(), "new");
        let request = ask(
            &mut desk,
            &joining,
            "m110",
            "192.168.1.45".parse().unwrap(),
            now,
        );
        assert!(
            desk.hear(
                &Asking {
                    wire: request.bytes(),
                    node: [7; 32],
                    name: "forged",
                    model: "Mac",
                    signature: &[0; 64],
                },
                "192.168.1.46".parse().unwrap(),
                now
            )
            .is_err()
        );
        ask(
            &mut desk,
            &device(root.path(), "public"),
            "public",
            "8.8.8.8".parse().unwrap(),
            now,
        );
        let desk = Mutex::new(desk);
        assert_eq!(
            desk.lock_or_recover().next_automatic_request(now),
            Some(now)
        );
        let mut posted = Vec::new();
        assert_eq!(
            forward_automatic_with(&desk, now, |request| {
                posted.push(request.clone());
                Ok(())
            })
            .unwrap(),
            1
        );
        assert_eq!(
            posted[0].recipient,
            request.binding().recipient().to_bech32()
        );
        assert_eq!(posted[0].host, "m110");
        assert!(posted[0].allowed.is_none() && posted[0].proof.is_none());
        assert_eq!(
            desk.lock_or_recover().next_automatic_request(now),
            Some(now + Duration::from_secs(30))
        );
        assert_eq!(
            forward_automatic_with(&desk, now, |_| panic!("already queued")).unwrap(),
            0
        );
        assert_eq!(
            forward_automatic_with(&desk, now + Duration::from_secs(30), |_| Ok(())).unwrap(),
            1
        );
    }

    #[test]
    fn automatic_dispatch_rechecks_freshness_after_selection() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut desk = Desk::new(
            Local::with(device(root.path(), "member"), TRUST, true, "member")
                .unwrap()
                .with_automatic_lan([]),
        );
        let asked = Instant::now();
        ask(
            &mut desk,
            &device(root.path(), "new"),
            "new",
            "192.168.1.45".parse().unwrap(),
            asked,
        );
        let selected = asked + STILL_ASKING - Duration::from_secs(1);
        let request = desk.take_automatic_request(selected).unwrap();
        assert_eq!(
            forward_selected(
                &Mutex::new(desk),
                request,
                &|| true,
                &|| asked + STILL_ASKING,
                |_| panic!("expired request was sent"),
            )
            .unwrap(),
            0
        );
    }

    #[test]
    fn a_stopped_owner_cancels_a_selected_automatic_request() {
        let root = aterm_tempfile::tempdir().unwrap();
        let local = Local::with(device(root.path(), "member"), TRUST, true, "member")
            .unwrap()
            .with_automatic_lan([]);
        let mut desk = Desk::new(local);
        let now = Instant::now();
        for name in ["one", "two"] {
            ask(
                &mut desk,
                &device(root.path(), name),
                name,
                "192.168.1.45".parse().unwrap(),
                now,
            );
        }
        let request = desk.take_automatic_request(now).unwrap();
        let desk = Mutex::new(desk);
        assert_eq!(
            forward_selected(&desk, request, &|| false, &|| now, |_| panic!(
                "stopped owner posted a request"
            ),)
            .unwrap(),
            0
        );
        assert_eq!(
            desk.lock_or_recover().next_automatic_request(now),
            Some(now)
        );
    }

    /// Only an ask from another host on this member's home ALab LAN is forwarded (D1): one from
    /// another private network the member also reaches (a second interface, a VM bridge,
    /// Internet Sharing, a phone hotspot's `172.20.10.0/28`) is not, though the vault's server
    /// would observe it under the member's own home address, and a member that is on no home
    /// ALab LAN forwards nothing, even from an address its old home held.
    ///
    /// NEGATIVE CONTROL: `Local::automatically_admits` taking every private source, as before the
    /// home check (`self.automatic_lan && matches!(from, IpAddr::V4(ip) if ip.is_private())`),
    /// forwards all three asks (left `3`, right `1`).
    #[test]
    fn only_an_ask_from_the_members_home_lan_is_forwarded() {
        let root = aterm_tempfile::tempdir().unwrap();
        let local = || {
            Local::with(device(root.path(), "member"), TRUST, true, "member")
                .unwrap()
                .with_automatic_lan([])
        };
        let mut desk = Desk::new(local());
        let now = Instant::now();
        for (name, from) in [
            ("home", "192.168.1.45"),
            ("bridge", "192.168.64.3"),
            ("hotspot", "172.20.10.5"),
        ] {
            ask(
                &mut desk,
                &device(root.path(), name),
                name,
                from.parse().unwrap(),
                now,
            );
        }
        let desk = Mutex::new(desk);
        let mut posted = Vec::new();
        assert_eq!(
            forward_automatic_with(&desk, now, |request| {
                posted.push(request.host.clone());
                Ok(())
            })
            .unwrap(),
            1
        );
        assert_eq!(posted, ["home"]);
        let rows = desk.lock_or_recover().rows(now);
        assert_eq!(
            rows.iter()
                .map(|row| (row.name.as_str(), row.automatic))
                .collect::<Vec<_>>(),
            [("home", true), ("bridge", false), ("hotspot", false)]
        );

        desk.lock_or_recover().refresh(local().with_home(None));
        let later = now + Duration::from_secs(31);
        assert_eq!(
            forward_automatic_with(&desk, later, |request| panic!(
                "{} forwarded from no home",
                request.host
            ))
            .unwrap(),
            0
        );
    }

    #[test]
    fn confirmed_admission_closes_existing_and_repeated_approval_rows() {
        let root = aterm_tempfile::tempdir().unwrap();
        let local = || Local::with(device(root.path(), "member"), TRUST, true, "member").unwrap();
        let mut desk = Desk::new(local().with_automatic_lan([]));
        let now = Instant::now();
        let joining = device(root.path(), "new");
        let request = ask(
            &mut desk,
            &joining,
            "m110",
            "192.168.1.45".parse().unwrap(),
            now,
        );
        desk.refresh(local().with_automatic_lan([request.binding().recipient().to_bech32()]));
        assert_eq!(desk.rows(now)[0].state, RowState::Closed);
        ask(
            &mut desk,
            &joining,
            "m110",
            "192.168.1.45".parse().unwrap(),
            now,
        );
        assert_eq!(desk.rows(now)[0].state, RowState::Closed);
        assert!(desk.take_automatic_request(now).is_none());
    }

    #[test]
    fn automatic_queue_preserves_ignores_and_removals() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut desk = Desk::new(
            Local::with(device(root.path(), "member"), TRUST, true, "member")
                .unwrap()
                .with_automatic_lan([]),
        );
        let now = Instant::now();
        let removed = ask(
            &mut desk,
            &device(root.path(), "removed"),
            "removed",
            "192.168.1.45".parse().unwrap(),
            now,
        );
        let removed_recipient = removed.binding().recipient().to_bech32();
        desk.local.removed.by_recipient.insert(
            removed_recipient,
            client::RemovedNote {
                label: "m9".into(),
                by: vec!["m1".into()],
                on_ms: 1,
                quorum: false,
            },
        );
        ask(
            &mut desk,
            &device(root.path(), "ignored"),
            "ignored",
            "192.168.1.46".parse().unwrap(),
            now,
        );
        desk.ignore(desk.rows(now)[1].id).unwrap();
        assert!(desk.take_automatic_request(now).is_none());
    }

    #[test]
    fn a_personal_vault_keeps_its_existing_manual_admission_policy() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut desk =
            Desk::new(Local::with(device(root.path(), "member"), TRUST, true, "member").unwrap());
        let now = Instant::now();
        ask(
            &mut desk,
            &device(root.path(), "new"),
            "new",
            "192.168.1.45".parse().unwrap(),
            now,
        );
        assert!(desk.take_automatic_request(now).is_none());
    }

    #[test]
    fn an_exhausted_submission_leaves_other_machines_ready_for_a_fresh_budget() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut desk = Desk::new(
            Local::with(device(root.path(), "member"), TRUST, true, "member")
                .unwrap()
                .with_automatic_lan([]),
        );
        let now = Instant::now();
        for name in ["slow", "waiting"] {
            ask(
                &mut desk,
                &device(root.path(), name),
                name,
                "192.168.1.45".parse().unwrap(),
                now,
            );
        }
        let desk = Mutex::new(desk);
        let mut accepted = Vec::new();
        for _ in 0..2 {
            // Each forwarding call gets a fresh transport budget. The slow request spends
            // all of its call's budget; no later request in that call can succeed.
            let mut spent = false;
            let _ = forward_automatic_with(&desk, now, |request| {
                if std::mem::replace(&mut spent, true) || request.host == "slow" {
                    return Err(Error::of_kind(crate::ErrorKind::TimedOut, "budget spent"));
                }
                accepted.push(request.host.clone());
                Ok(())
            });
        }
        assert_eq!(accepted, ["waiting"]);
        assert_eq!(
            desk.lock_or_recover().next_automatic_request(now),
            Some(now + Duration::from_secs(30))
        );
    }

    #[test]
    fn one_refused_request_does_not_hold_back_the_other_waiting_machines() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut desk = Desk::new(
            Local::with(device(root.path(), "member"), TRUST, true, "member")
                .unwrap()
                .with_automatic_lan([]),
        );
        let now = Instant::now();
        for name in ["first", "second"] {
            ask(
                &mut desk,
                &device(root.path(), name),
                name,
                "192.168.1.45".parse().unwrap(),
                now,
            );
        }
        let desk = Mutex::new(desk);
        let mut attempted = Vec::new();
        assert!(
            forward_automatic_with(&desk, now, |request| {
                attempted.push(request.host.clone());
                if request.host == "first" {
                    Err(Error::new("refused"))
                } else {
                    Ok(())
                }
            })
            .is_err()
        );
        assert_eq!(attempted, ["first"]);
        assert_eq!(
            desk.lock_or_recover().next_automatic_request(now),
            Some(now)
        );
        // Even after the first request's retry is due, the unattempted machine is older.
        assert_eq!(
            forward_automatic_with(&desk, now + Duration::from_secs(30), |request| {
                attempted.push(request.host.clone());
                Ok(())
            })
            .unwrap(),
            1
        );
        assert_eq!(attempted, ["first", "second"]);
    }

    #[test]
    fn an_ignore_before_dispatch_cancels_a_selected_request() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut desk = Desk::new(
            Local::with(device(root.path(), "member"), TRUST, true, "member")
                .unwrap()
                .with_automatic_lan([]),
        );
        let now = Instant::now();
        ask(
            &mut desk,
            &device(root.path(), "new"),
            "new",
            "192.168.1.45".parse().unwrap(),
            now,
        );
        let request = desk.take_automatic_request(now).unwrap();
        desk.ignore(desk.rows(now)[0].id).unwrap();
        assert_eq!(
            forward_selected(&Mutex::new(desk), request, &|| true, &|| now, |_| panic!(
                "ignored request was sent"
            ))
            .unwrap(),
            0
        );
    }
}
