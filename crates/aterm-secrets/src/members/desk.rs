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
//! On the pinned default ALab vault, a Mac on this member's home ALab LAN
//! ([`Local::automatically_admits`]) is let in for being there: its `prove` on this desk reaches
//! the caller's decision marked [`Proving::lan`], which signs its `Admit` and hands the log bundle
//! on that one connection, with nobody's Allow (owner, 2026-11-02: "physical access to the LAN is
//! sufficient proof"). A proof from any other network, however private its address, is not
//! marked so, and a plain `ask`, from any network, shows as a row that waits for a person's Allow
//! (`docs/DESIGN-automatic-lan-admission-2026-11-02.md`). In a debug build a lab's own vault,
//! served at a URL and never the live one, can name its home LAN instead
//! (`members::lab_lan`): a proof from another host inside that network is then marked
//! [`Proving::lan`] on its desk the same way, which a release build never compiles.

use super::bundle::Reader;
use super::wire::{self, Line};
use super::{Allowed, PhraseState};
use crate::client::{self, Paths};
use crate::commons::Unavailable;
use crate::commons::binding::{Pending, Request};
use crate::commons::bootstrap::{Availability, Bootstrap, Device};
use crate::commons::journal::Trust;
use crate::pake::{ChannelId, MemberSession, Nonce, PhraseId, Phrases, Share, Tag};
use crate::{Error, Result, SecretBytes};
use aterm_time::Instant;
use aterm_types::MutexExt;
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
/// waiting to be let in asks each member again (its subscriber about half a minute, then 1, 2, 4
/// and 8 minutes after its first ask and then every 15, `aterm members join` every few seconds),
/// and its row goes only
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
/// Why a member on the live ALab genesis refuses a lab identity's ask, run or proof
/// (`docs/DESIGN-alab-one-mesh-2026-11-05.md` A12): it names no key, label or digest.
pub const LAB_REFUSED: &str = "this member is on the live ALab genesis, and a lab identity never \
                               joins it: a lab identity founds its own lab";
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
/// binds its writer key, which an admin's approver needs to check its `Allowed` (F10). A log seat
/// under the anchor it pinned makes a member ([`Local::of`]), whether or not Commons control is
/// this Mac's.
pub struct Local {
    device: Device,
    request: Request,
    trust: Trust,
    can_allow: bool,
    name: String,
    removed: client::RemovedNotes,
    /// The live seats by their fleet node keys ([`client::kept_seats`]): a Mac that asks with
    /// one of them under another vault key is that seat's Mac rebuilt, and its row says so.
    kept: std::collections::BTreeMap<[u8; 32], client::KeptSeat>,
    admitted: std::collections::BTreeSet<crate::membership_log::MemberId>,
    /// The writer keys the current Commons roster binds: a Mac an approver enrolled from an
    /// `Allowed` (F10) is bound here once let in. Empty where Commons control is not this Mac's.
    bound: std::collections::BTreeSet<[u8; 32]>,
    /// The pinned default ALab vault served at a URL, or (a debug build's seam) a lab vault that
    /// names its home LAN (`members::lab_lan::home`): a Mac on this member's home LAN that proves
    /// itself is let in ([`Local::automatically_admits`], [`Proving::lan`]), and the vault's
    /// signed swarm roster closes rows (`Local::knows_recipient`).
    automatic_lan: bool,
    /// The recipients the vault's signed swarm roster names (where `automatic_lan` holds only).
    admitted_recipients: std::collections::BTreeSet<String>,
    /// This member's home LAN when it read its place: the home ALab LAN on the default ALab vault
    /// (`crate::home_lan::home_net`), the network a lab vault names in a debug build
    /// (`members::lab_lan::home`). Only a host on it is let in by its proof.
    home: Option<crate::home_lan::HomeNet>,
    /// Where [`Self::home`] is read again from ([`Desk::home_from`]).
    home_from: HomeFrom,
    /// Its vault is the live ALab's by its genesis ([`client::on_live_genesis`]): a lab identity's
    /// ask and run are refused here ([`LAB_REFUSED`]), and its proof is by the caller's decision.
    live_genesis: bool,
    /// Where this Mac keeps its vault state ([`Local::of`]): a run takes that state
    /// ([`client::take_handover_hold`]) before it touches a phrase. `None` for a test's member
    /// ([`Local::with`]), whose runs hold nothing.
    paths: Option<Paths>,
}

/// Where a member's desk reads its home LAN again from ([`Desk::home_from`]), so its lane can
/// follow a network move without reading the vault where the vault does not decide it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HomeFrom {
    /// It has none: no Mac is let in by its proof alone.
    None,
    /// The pinned default ALab vault served at a URL: the home ALab LAN as this Mac reads its
    /// place now (`crate::home_lan::home_net`), which a network move changes and the vault does
    /// not ([`Desk::on_home`]).
    Place,
    /// A debug build's lab vault that names a home LAN (`super::lab_lan`): that file and this
    /// Mac's route together, so it is read again only with the vault ([`Local::of`]).
    Vault,
}

impl Local {
    /// This Mac as a member of the network its applied vault names. Where it holds Commons
    /// control, that control's descriptor names the network and its roster the writer keys it
    /// binds. Membership is the membership log (owner, 2026-11-02), not Commons control: where the
    /// current signed Commons epochs leave this Mac out of control, or no control entry is
    /// addressed to it, while its vault is otherwise applied and admits it, it is a member still
    /// wherever its log seats its Commons device under the trust anchor it pinned
    /// ([`client::seated_member`]). Such a member binds no writer key, allows by its log seat, and
    /// lets a Mac in through the log alone. `None` with no vault, or a vault not yet applied, not
    /// admitting this Mac or still busy at the read's deadline; and, where Commons control is not
    /// its own, for a Mac its log removed or does not seat, or one that never pinned an anchor
    /// ([`client::member_by_log`] tells those from a busy vault). Where Commons control is not its
    /// own, reading creates no custody, founds no network and claims no control. It reads its home
    /// LAN, which lets a proving Mac in, on the default ALab vault served at a URL and, in a debug
    /// build, on a lab vault that names one (`members::lab_lan::home`); on any other vault it has
    /// none.
    ///
    /// # Errors
    /// Unreadable or corrupt local Commons state, a pinned anchor that names another network than
    /// the membership log, or a vault still busy at the read's deadline while a seat is read.
    pub fn of(paths: &Paths) -> Result<Option<Self>> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let (device, trust, bound, can_allow) = match Bootstrap::observe_until(paths, deadline)? {
            Availability::Available(bootstrap) => {
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
                (device, bootstrap.trust(), bound, can_allow)
            }
            // Only Commons control is not this Mac's: its vault is applied and admits it. What
            // only control gives degrades (no roster binds a writer key here); the log seat is
            // what lets it allow, and what lets a new Mac in.
            Availability::Unavailable(
                Unavailable::ControlExcluded | Unavailable::ControlNotVisible,
            ) => {
                let Some((device, trust)) = client::seated_member(paths, Some(deadline))? else {
                    return Ok(None);
                };
                (device, trust, std::collections::BTreeSet::new(), true)
            }
            Availability::Unavailable(_) => return Ok(None),
        };
        let admitted = client::log_admitted_actors(paths);
        let request = device.request(trust.network, trust.authority)?;
        let config = client::config(paths)?;
        let live_genesis = client::on_live_genesis(paths);
        let alab_url = config.as_ref().is_some_and(|config| {
            client::alab_vault(config) && matches!(&config.source, crate::transport::Source::Url(_))
        });
        // A debug build's lab vault names its own home LAN (`super::lab_lan`); a release build
        // compiles no such seam, and its desk admits by the live vault's home alone.
        #[cfg(any(test, debug_assertions))]
        let lab_home = super::lab_lan::home(paths, config.as_ref(), live_genesis);
        #[cfg(not(any(test, debug_assertions)))]
        let lab_home: Option<crate::home_lan::HomeNet> = None;
        let automatic_lan = alab_url || lab_home.is_some();
        let name = config.map(|config| config.host).unwrap_or_default();
        // A removed Mac that asks again shows as removed, not new (F16): from the log this Mac
        // holds, read without the state lock (the records file is written whole).
        let removed = client::removed_notes(paths).unwrap_or_default();
        let kept = client::kept_seats(paths).unwrap_or_default();
        let admitted_recipients = if automatic_lan {
            client::swarm_roster_until(paths, deadline)?.map_or_else(Default::default, |roster| {
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
        let home = if alab_url {
            crate::home_lan::home_net()
        } else {
            lab_home
        };
        // A lab vault's home is its file and this Mac's route together (`super::lab_lan::home`),
        // read again only with the vault.
        #[cfg(any(test, debug_assertions))]
        let lab = std::fs::symlink_metadata(paths.dir.join(super::lab_lan::FILE)).is_ok();
        #[cfg(not(any(test, debug_assertions)))]
        let lab = false;
        let home_from = if alab_url {
            HomeFrom::Place
        } else if lab {
            HomeFrom::Vault
        } else {
            HomeFrom::None
        };
        Ok(Some(Self {
            device,
            request,
            trust,
            can_allow,
            name,
            removed,
            kept,
            admitted,
            bound,
            automatic_lan,
            admitted_recipients,
            home,
            home_from,
            live_genesis,
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
            kept: std::collections::BTreeMap::new(),
            admitted: std::collections::BTreeSet::new(),
            bound: std::collections::BTreeSet::new(),
            automatic_lan: false,
            admitted_recipients: Default::default(),
            home: None,
            home_from: HomeFrom::None,
            live_genesis: false,
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

    /// This member with `kept` as its log's live seats by node key ([`client::kept_seats`]): the
    /// seam a test gives a desk built by [`Local::with`] its log's seats with.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn with_kept(self, kept: std::collections::BTreeMap<[u8; 32], client::KeptSeat>) -> Self {
        Self { kept, ..self }
    }

    /// This member with `admitted` as the seats its membership log admits ([`Local::of`]'s
    /// `client::log_admitted_actors`): the seam a test gives a desk built by [`Local::with`]
    /// its log's seats with.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn with_admitted(
        self,
        admitted: impl IntoIterator<Item = crate::membership_log::MemberId>,
    ) -> Self {
        Self {
            admitted: admitted.into_iter().collect(),
            ..self
        }
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
            home_from: HomeFrom::Place,
            ..self
        }
    }

    /// This member on the live ALab genesis (`true`) or another (`false`): the seam a test puts a
    /// scratch member on the genesis this build compiles with ([`Local::on_live_genesis`]).
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn with_live_genesis(self, live_genesis: bool) -> Self {
        Self {
            live_genesis,
            ..self
        }
    }

    /// Whether this member's vault is the live ALab's by its genesis, where a lab identity's ask
    /// and run are refused ([`LAB_REFUSED`]).
    #[must_use]
    pub const fn on_live_genesis(&self) -> bool {
        self.live_genesis
    }

    /// This member on the home ALab LAN `home`, or on none: the seam a test moves it with.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn with_home(self, home: Option<crate::home_lan::HomeNet>) -> Self {
        Self { home, ..self }
    }

    /// Whether a proof from `from` is from the home ALab LAN ([`Proving::lan`]): this member
    /// serves the pinned default ALab vault and was on its home ALab LAN when it last read its
    /// place, or (a debug build's seam) its lab vault names a home LAN that held this Mac's route
    /// then (`members::lab_lan::home`), and `from` is another host on that network
    /// ([`crate::home_lan::HomeNet::holds`]).
    /// A private address alone is no home: a phone hotspot, a café, a VM bridge or Internet
    /// Sharing number their hosts privately too, and a proof from any other network is not
    /// marked so.
    #[must_use]
    pub fn automatically_admits(&self, from: IpAddr) -> bool {
        self.automatic_lan && self.home.is_some_and(|home| home.holds(from))
    }

    /// Whether the vault's signed swarm roster names `request`'s recipient as a member here, so
    /// its ask closes at once. Where this member's log seats anyone, the log must seat the
    /// asking Mac too: a seatless recipient the roster still names (one an approver let in
    /// through the envelope alone) asks for a seat, and its row stays for a person's Allow
    /// (one-mesh S1).
    fn knows_recipient(&self, request: &Request) -> bool {
        self.automatic_lan
            && self
                .admitted_recipients
                .contains(&request.binding().recipient().to_bech32())
            && (self.admitted.is_empty() || self.admitted.contains(&request.binding().actor()))
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
    /// Waiting for a person's Allow, or another admission this Mac reads.
    Asking,
    /// Allowed here; its phrase is on this Mac's screen.
    Allowed(PhraseId),
    /// A person pressed Ignore here.
    Ignored,
    /// Let in, here or by another member.
    Closed,
}

/// One heard request, as a member's window reads it for its join entries.
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
    /// when, so a member reads it as a removed Mac, not a new one (F16).
    pub removed: Option<client::RemovedNote>,
    /// A person pressed "Allow again": the run restores the Mac a plain removal took out.
    pub restore: bool,
    /// A live seat holds the node key this Mac asks with, under another vault key: the Mac is
    /// that seat's, rebuilt with its node key kept, and a person's Allow moves the seat to it
    /// (one-mesh A15). [`client::KeptSeat::title`] is the card's words: when the old key last
    /// signed, and that it reads no newer secret once Allowed. `None` for a removed Mac's row.
    pub moves: Option<client::KeptSeat>,
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
    /// The node keys requests proved on this desk ([`Desk::begin_proof`]: a signature by the node
    /// key over this member's fresh challenge and the request), by [`row_id`], with when: a
    /// person's Allow moves a seat to a Mac by its node key only where the Mac proved it here
    /// ([`Desk::node_proven`]). At most [`MAX_HEARD`]; each lapses [`STILL_ASKING`] after.
    proven: Vec<(u64, [u8; 32], Instant)>,
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
    /// The line carries the lab mark ([`wire::marked`]).
    lab: bool,
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
    reader: Reader,
    restore: bool,
    /// The Mac proved its node key on this desk ([`Desk::node_proven`]).
    node_proven: bool,
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
            proven: Vec::new(),
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

    /// Where this member's home LAN ([`Local::automatically_admits`]) is read again from.
    #[must_use]
    pub const fn home_from(&self) -> HomeFrom {
        self.local.home_from
    }

    /// This member's home ALab LAN as read again now (`crate::home_lan::home_net`), the rest of
    /// what [`Local::of`] read kept: a network move, or a day that made a LAN home, changes only
    /// this, so the desk takes it without reading the vault again. Only a member whose home is
    /// its place's ([`HomeFrom::Place`]) takes one; any other keeps what [`Local::of`] read.
    pub fn on_home(&mut self, home: Option<crate::home_lan::HomeNet>) {
        if self.local.home_from == HomeFrom::Place {
            self.local.home = home;
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
        self.hear_marked(asking, false, from, now)
    }

    /// [`Desk::hear`] of an ask that carries the lab mark (`lab`): on the live ALab genesis it is
    /// refused ([`LAB_REFUSED`]) once its signature checks, and shows no row.
    ///
    /// # Errors
    /// As [`Desk::hear`], or `lab` on this member's live genesis.
    pub fn hear_marked(
        &mut self,
        asking: &Asking<'_>,
        lab: bool,
        from: IpAddr,
        now: Instant,
    ) -> Result<Answer> {
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
        if !astream_aead::signature::verify(request.binding().writer(), &message, signature) {
            return Err(Error::new(
                "that ask is not signed by the Mac whose request it names",
            ));
        }
        if lab && self.local.live_genesis {
            return Err(Error::new(LAB_REFUSED));
        }
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
        });
        Ok(Answer::Asking)
    }

    /// The heard requests' rows, oldest first: none that lapsed ([`Desk::lapsed`]).
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
                moves: self.kept_seat(heard, now).cloned(),
            })
            .collect()
    }

    /// The live seat whose node key `heard` asks with under another vault key, where one does,
    /// the Mac proved that node key on this desk ([`Desk::node_proven`]) and the log did not
    /// remove it. An ask names its node key unproven: a Mac that only names a seat's key is a new
    /// Mac here.
    fn kept_seat(&self, heard: &Heard, now: Instant) -> Option<&client::KeptSeat> {
        if self.removed_note(heard).is_some()
            || heard.node == [0; 32]
            || !self.node_proven(&heard.request, &heard.node, now)
        {
            return None;
        }
        self.local
            .kept
            .get(&heard.node)
            .filter(|kept| kept.seat != heard.request.binding().actor())
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
            lab,
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
        if !astream_aead::signature::verify(request.binding().writer(), &message, signature) {
            return Err(Error::new(
                "that run is not signed by the Mac whose request it names",
            ));
        }
        // Before any phrase is touched: a lab identity's run spends none here.
        if *lab && self.local.live_genesis {
            return Err(Error::new(LAB_REFUSED));
        }
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
        let node_proven = self.node_proven(&request, &node, now);
        Ok((
            Running {
                phrase,
                request,
                node,
                name,
                reader: Reader::Files,
                restore,
                node_proven,
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
        if !astream_aead::signature::verify(request.binding().writer(), &message, &line.signature) {
            return Err(Error::new(
                "that proof is not signed by the Mac whose request it names",
            ));
        }
        if !astream_aead::signature::verify(
            &line.node,
            &[PROVE_RECORD_PREFIX, &message].concat(),
            &line.node_signature,
        ) {
            return Err(Error::new(
                "that proof is not signed by the node key it names",
            ));
        }
        self.proven.retain(|(id, _, _)| *id != row_id(&request));
        if self.proven.len() >= MAX_HEARD {
            self.proven.remove(0);
        }
        self.proven.push((row_id(&request), line.node, now));
        Ok(request)
    }

    /// Whether the Mac whose request is `request` proved the node key `node` on this desk within
    /// [`STILL_ASKING`] of `now` ([`Desk::begin_proof`]). A Mac rebuilt with its node key kept
    /// proves it first (`join::prove_first`), and is refused there while its old seat signed
    /// within the day; its plain ask that follows shows the seat it takes back only so.
    fn node_proven(&self, request: &Request, node: &[u8; 32], now: Instant) -> bool {
        let id = row_id(request);
        self.proven.iter().any(|(proved, key, at)| {
            *proved == id && key == node && now.saturating_duration_since(*at) < STILL_ASKING
        })
    }

    /// The seam a test proves a request's node key on this desk with, as [`Desk::begin_proof`]
    /// records it.
    #[cfg(any(test, debug_assertions))]
    pub fn with_proven_node(&mut self, request: &Request, node: [u8; 32], now: Instant) {
        self.proven.push((row_id(request), node, now));
    }

    /// The new Mac's own word that it was let in (`done`), signed by its writer key over this
    /// member's key and its request: the row closes.
    fn done(&mut self, wire: &[u8], signature: &[u8; 64]) -> Result<()> {
        let request = self.checked(wire)?;
        let message = [DONE_DOMAIN, self.local.key(), request.bytes()].concat();
        if !astream_aead::signature::verify(request.binding().writer(), &message, signature) {
            return Err(Error::new(
                "that Mac's word that it was let in did not verify",
            ));
        }
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
    /// A diagnostic supplied by the bundle producer ([`Handout::note`]), for the caller's log.
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
    /// The payload and capacity capability stated when the device started its run.
    pub reader: Reader,
    /// A person pressed "Allow again" for it: the log removed it, and a plain removal does not
    /// keep it out of this run (F16).
    pub restore: bool,
    /// It proved `node` on this desk by a signature of the node key ([`Desk::begin_proof`]): only
    /// then does an Allow move the seat that holds that key to it (one-mesh A15).
    pub node_proven: bool,
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
    /// An optional diagnostic from the bundle producer, for the caller's log. A failed log
    /// handover returns an error instead of a files payload.
    pub note: Option<String>,
}

/// How a desk's caller builds a run's bundle ([`serve`]).
pub type Hand<'a> = &'a dyn Fn(&Handing<'_>) -> Result<Handout>;

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
    /// The joining device's stated log capacity, or its explicit legacy capability.
    pub reader: Reader,
    /// The fleet's channel proved the fleet's current key and this node key, and the caller's
    /// drive admission took it ([`Proof::fleet`]).
    pub fleet: bool,
    /// The connection came from another host on this member's home ALab LAN
    /// ([`Local::automatically_admits`]): physical access to that LAN is proof enough to let a
    /// new Mac in (owner ruling 2026-11-02).
    pub lan: bool,
    /// Its line carried the lab mark ([`wire::marked`]): a member on the live ALab genesis refuses
    /// it (`client::refuses_before_possession`, `client::prove_at`).
    pub lab: bool,
    /// Set by [`Proof::refuses`] once it read the vault's newest records on this connection, so
    /// [`Proof::decide`] after the possession challenge reads them no second time: the whole
    /// exchange runs inside the fleet listener's handshake watchdog
    /// (`aterm_net::fleet::HANDSHAKE_WITHIN`).
    pub fresh: &'a std::cell::Cell<bool>,
}

/// What a fleet channel (`vault-join`) proved on one connection ([`Proof::fleet`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fleet {
    /// The node key the fleet's channel proved: the proof must name it.
    pub node: [u8; 32],
    /// Whether the channel also proved the vault's current fleet code and this Mac's drive
    /// admission took the node ([`Proving::fleet`]).
    pub current: bool,
}

/// A caller's bounded public membership evidence, read before possession with no desk lock.
pub type ProofAuthority<'a> = &'a (dyn Fn(&Proving<'_>) -> Result<Vec<u8>> + Sync);

/// How a desk's caller serves `prove` on one connection ([`serve_with`]).
#[derive(Clone, Copy)]
pub struct Proof<'a> {
    /// Decides and hands the bundle; called with no lock of the desk held.
    pub decide: Prove<'a>,
    /// Why this member refuses the Mac whatever it proves next, asked before the possession
    /// challenge (`client::refuses_before_possession`); called with no lock of the desk held.
    pub refuses: Refuses<'a>,
    /// Public/sealed acknowledged membership records, bounded by the joining reader. An
    /// automatic default join verifies these before answering possession. No desk lock is held.
    pub authority: Option<ProofAuthority<'a>>,
    /// What the fleet's channel proved on this connection, where it came over the fleet
    /// (`vault-join`): the proof must name its node key, and [`Proving::fleet`] holds whether it
    /// also proved the vault's current fleet code.
    pub fleet: Option<Fleet>,
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
    serve_with_request_policy(desk, stream, from, bundle, proof, &|| Ok(()))
}

/// [`serve_with`], checking the caller's current policy before a non-proof request.
/// Proofs retain their own preflight and decision checks; the request policy grants no
/// admission. The policy runs with no desk lock held and precedes any ask, done or run work.
///
/// # Errors
/// As [`serve_with`], or the caller refuses this non-proof request.
pub fn serve_with_request_policy<S: Read + Write + ?Sized>(
    desk: &Mutex<Desk>,
    stream: &mut S,
    from: IpAddr,
    bundle: Hand<'_>,
    proof: Option<Proof<'_>>,
    request_policy: &dyn Fn() -> Result<()>,
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
    if parsed.verb != "prove"
        && let Err(error) = request_policy()
    {
        let _ = wire::write_line(stream, &wire::refusal(&error.to_string()));
        return Err(error);
    }
    let answered = match parsed.verb {
        "ask" => {
            ask(desk, &parsed, from).and_then(|answer| answer_line(desk, &parsed, &greeted, answer))
        }
        "done" => done(desk, &parsed).map(|()| format!("OK state={}", Answer::Closed.word())),
        "node" => node(desk, &parsed).map(|taken| format!("OK state={}", taken.word())),
        "run" => return run(desk, stream, &parsed, from, &challenge, bundle),
        "prove" if let Some(proof) = proof => {
            return prove(desk, stream, &parsed, &line, from, &greeted, proof).map(|()| None);
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
    let lab = wire::marked(line)?;
    desk.lock_or_recover()
        .hear_marked(&asking, lab, from, Instant::now())
}

fn done(desk: &Mutex<Desk>, line: &Line<'_>) -> Result<()> {
    let wire = line.b64("request")?;
    let signature = line.hex::<64>("sig")?;
    desk.lock_or_recover().done(&wire, &signature)
}

/// What a member's desk did with a seat's `Node` record ([`offer_node`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeTaken {
    /// Its log now holds the record and binds the node key to the seat that signed it. Its
    /// admission grants from the history it acknowledged, so the key passes there once a vault
    /// pass acknowledges the record, as every record it takes.
    Bound,
    /// Its log already bound the node key to that seat.
    Already,
    /// It keeps the record, which waits on a parent its log does not hold yet: the key is bound
    /// once the log takes that parent.
    Waiting,
    /// Its log took the record, and a later `Node` record of the same seat it already held (one
    /// that waited on this one) replaces the key: the seat's current key is that record's.
    Replaced,
}

impl NodeTaken {
    /// The word a desk answers it with (`OK state=<word>`).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            NodeTaken::Bound => "bound",
            NodeTaken::Already => "already",
            NodeTaken::Waiting => "waiting",
            NodeTaken::Replaced => "replaced",
        }
    }

    fn of_word(word: &str) -> Option<Self> {
        [
            NodeTaken::Bound,
            NodeTaken::Already,
            NodeTaken::Waiting,
            NodeTaken::Replaced,
        ]
        .into_iter()
        .find(|taken| taken.word() == word)
    }
}

/// The verb a Mac sends a seat's `Node` record under ([`offer_node`]).
const NODE_VERB: &str = "node record=";

// The largest `Node` record a log takes fits a desk's line in base64.
const _: () = assert!(
    NODE_VERB.len() + crate::membership_log::Kind::Node.most_bytes().div_ceil(3) * 4
        < wire::MAX_LINE
);

/// Why a member that keeps no log refuses a `Node` record.
const NODE_NO_LOG: &str = "this Mac keeps no membership log to bind a node key in";

/// Why a member whose vault state a pass holds refuses a `Node` record for now.
const NODE_BUSY: &str =
    "a vault pass on this Mac holds its state for now; send the Node record again in a moment";

/// Take a seated member's signed `Node` record from a Mac whose node key no record binds yet
/// (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.4: the four seats with no `Node` record), so
/// that Mac can bind its key and join `atm/1`. The record verifies by itself ([`Record::decode`]
/// checks the seat's signature and the node key's own proof over this network and signer, so no
/// Mac binds a key it does not hold). This log must hold its network, seat its signer, and bind
/// its key to no one: a key bound to another seat, to a Mac now out or retired by a rotation is
/// refused, and one already bound to the signer is [`NodeTaken::Already`]. A seat's key no record
/// binds, a rotation's new key included, is taken into this Mac's store under its vault state,
/// held for at most [`client::HANDOVER_LOCK_WAIT`] as a run holds it; the log's own evaluation
/// then judges the record as it judges any other.
///
/// [`Record::decode`]: crate::membership_log::Record::decode
fn node(desk: &Mutex<Desk>, line: &Line<'_>) -> Result<NodeTaken> {
    use crate::membership_log::{Body, NodeStanding, Record};
    let record = Record::decode(&line.b64("record")?)?;
    let Body::Node { node_key, .. } = record.body() else {
        return Err(Error::new("that membership record is not a Node record"));
    };
    let paths = desk
        .lock_or_recover()
        .local
        .paths
        .clone()
        .ok_or_else(|| Error::new(NODE_NO_LOG))?;
    let Some(_held) = client::take_handover_hold(&paths)? else {
        return Err(Error::of_kind(crate::ErrorKind::MemberBusy, NODE_BUSY));
    };
    let store = client::load_records(&paths)?;
    let log = store.log().ok_or_else(|| Error::new(NODE_NO_LOG))?;
    if record.network() != log.network() {
        return Err(Error::new(
            "that Node record is of another network than this Mac's log",
        ));
    }
    let membership = log.membership();
    if !membership.contains(record.actor()) {
        return Err(Error::new(
            "that Node record's signer holds no seat in this Mac's log",
        ));
    }
    match membership.node(node_key) {
        NodeStanding::Unknown => {}
        NodeStanding::Member(actor) if actor == *record.actor() => return Ok(NodeTaken::Already),
        NodeStanding::Member(_) | NodeStanding::Contested => {
            return Err(Error::new(
                "this Mac's log binds that node key to another seat",
            ));
        }
        NodeStanding::Removed(_) => {
            return Err(Error::new(
                "this Mac's log binds that node key to a Mac now out",
            ));
        }
        NodeStanding::Retired(_) => {
            return Err(Error::new(
                "this Mac's log holds that node key as one its seat replaced",
            ));
        }
    }
    let encoded = crate::membership_log::store::encode_records([&record]);
    client::save_records_unlocked(&paths, &encoded)?;
    let store = client::load_records(&paths)?;
    if !store.holds(record.id()) {
        return Ok(NodeTaken::Waiting);
    }
    match store.log().map(|log| log.membership().node(node_key)) {
        Some(NodeStanding::Member(actor)) if actor == *record.actor() => Ok(NodeTaken::Bound),
        Some(NodeStanding::Retired(actor)) if actor == *record.actor() => Ok(NodeTaken::Replaced),
        _ => Err(Error::new(
            "this Mac's log took that Node record but does not bind its key to its seat",
        )),
    }
}

/// Offer a member's desk `record`, this Mac's seat's signed `Node` record ([`node_record`]), on
/// `stream` (an `atf-ask/1` connection, which a Mac whose key no log binds may still open): the
/// desk's greeting, the record, and its answer ([`NodeTaken`]).
///
/// # Errors
/// The stream fails, the other end is not a member, or its desk refuses the record (of kind
/// [`crate::ErrorKind::MemberBusy`] while a vault pass there holds its state).
pub fn offer_node<S: Read + Write + ?Sized>(stream: &mut S, record: &[u8]) -> Result<NodeTaken> {
    super::joiner::greeting(stream)?;
    wire::write_line(stream, &format!("{NODE_VERB}{}", wire::b64(record)))?;
    let line = wire::read_line(stream)?;
    let answer = wire::answer(&line)?;
    NodeTaken::of_word(answer.get("state")?)
        .ok_or_else(|| Error::new("the member answered an unknown state for a Node record"))
}

/// The record in this Mac's log that binds `node_key` to this Mac's seat, as [`offer_node`]
/// sends it: `None` where the log holds none (no log, another seat's key, or a key this Mac's
/// seat has not bound; `client::publish_local_node_key` authors one).
///
/// # Errors
/// This Mac's store cannot be read.
pub fn node_record(paths: &Paths, node_key: &[u8; 32]) -> Result<Option<Vec<u8>>> {
    use crate::membership_log::{Body, NodeStanding};
    let store = client::load_records(paths)?;
    let Some(log) = store.log() else {
        return Ok(None);
    };
    let NodeStanding::Member(actor) = log.membership().node(node_key) else {
        return Ok(None);
    };
    Ok(log
        .records()
        .filter(|record| {
            record.actor() == &actor
                && matches!(record.body(), Body::Node { node_key: bound, .. } if bound == node_key)
        })
        .last()
        .map(|record| record.bytes().to_vec()))
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
            lab: wire::marked(first)?,
        };
        let reader = reader(first)?;
        if reader.reads_log()
            && let Some(paths) = &paths
        {
            let store = client::load_records(paths)?;
            let source = client::records_bound(paths, &store)?;
            if !reader.can_take(source) {
                return Err(Error::new(reader.refusal(source)));
            }
        }
        // `ack=1`: the new Mac acknowledges the bundle it opened ([`Delivered::Acknowledged`]).
        let acks = first.get("ack").is_ok_and(|ack| ack == "1");
        desk.lock_or_recover()
            .begin(from, &line, challenge, Instant::now())
            .map(|(running, run, nonce)| {
                (
                    Running {
                        reader,
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
        reader: running.reader,
        restore: running.restore,
        node_proven: running.node_proven,
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
    proof_line: &str,
    from: IpAddr,
    greeted: &super::joiner::Greeting,
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
        let (request, trust, lan) = {
            let mut held = desk.lock_or_recover();
            let request = held.begin_proof(
                from,
                &asked,
                &greeted.challenge,
                proof.fleet.as_ref().map(|fleet| &fleet.node),
                Instant::now(),
            )?;
            (
                request,
                held.local.trust,
                held.local.automatically_admits(from),
            )
        };
        let reader = reader(first)?;
        if !reader.reads_log() {
            return Err(Error::new("a proof join requires a membership log reader"));
        }
        let fresh = std::cell::Cell::new(false);
        let proving = Proving {
            request: &request,
            node: asked.node,
            name: &name,
            reader,
            fleet: proof.fleet.is_some_and(|fleet| fleet.current),
            lan,
            lab: wire::marked(first)?,
            fresh: &fresh,
        };
        // Before any possession challenge: a Mac this member refuses whatever it proves next is told
        // so while it has sent nothing a member could sign on, so the next member and its plain ask
        // may follow. The desk's lock is not held: this reads the vault's state.
        if let Some(why) = (proof.refuses)(&proving) {
            return Err(Error::new(why));
        }
        if first.get("authority").is_ok() {
            let nonce = first.hex::<32>("authority")?;
            let records = proof
                .authority
                .ok_or_else(|| Error::new("this member serves no proof authority witness"))?(
                &proving,
            )?;
            if records.len() > crate::membership_log::MAX_RECORDS_STREAM {
                return Err(Error::new("the member's authority exceeds its bound"));
            }
            let message = super::authority::message(&nonce, greeted, proof_line, &records);
            let signature = {
                let held = desk.lock_or_recover();
                if held.local.key() != &greeted.key {
                    return Err(Error::new("the member's identity changed during proof"));
                }
                super::joiner::sign(&held.local.device, &message)?
            };
            wire::write_line(
                stream,
                &format!(
                    "OK authority={} authoritysig={}",
                    records.len(),
                    wire::hex(signature.as_ref()),
                ),
            )?;
            stream.write_all(&records)?;
            stream.flush()?;
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

fn reader(line: &Line<'_>) -> Result<Reader> {
    if !matches!(line.verb, "run" | "prove") {
        return Err(Error::new("invalid join capability line"));
    }
    let log = line.get("reads").is_ok_and(|reads| reads == "2");
    if line.get("records-bound").is_ok() {
        if !log {
            return Err(Error::new(
                "a records bound requires a membership log reader",
            ));
        }
        let bound = crate::crossing::RecordsBound::parse(&line.text("records-bound")?)
            .ok_or_else(|| Error::new("invalid join records bound"))?;
        Ok(Reader::BoundedLog(bound))
    } else if log {
        Ok(Reader::LegacyLog)
    } else {
        Ok(Reader::Files)
    }
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
mod lan_tests {
    use super::*;

    #[test]
    fn reader_capability_distinguishes_files_legacy_and_explicit_bounds() {
        use crate::crossing::RecordsBound;
        assert_eq!(reader(&Line::parse("run")).unwrap(), Reader::Files);
        assert_eq!(
            reader(&Line::parse("run reads=2")).unwrap(),
            Reader::LegacyLog
        );
        let bound = RecordsBound::of(8192, true);
        let line = format!(
            "prove reads=2 records-bound={}",
            wire::escape(&bound.value())
        );
        assert_eq!(
            reader(&Line::parse(&line)).unwrap(),
            Reader::BoundedLog(bound)
        );
        assert!(!Reader::LegacyLog.can_take(RecordsBound::of(4096, true)));
        assert!(Reader::LegacyLog.can_take(RecordsBound::of(8192, false)));
        assert!(!Reader::BoundedLog(RecordsBound::of(4096, false)).can_take(bound));
        for bad in [
            "run records-bound=1%204096%20uncrossed",
            "run reads=2 records-bound=1%200%20uncrossed",
            "run reads=2 records-bound=1%2004096%20uncrossed",
            "run reads=2 records-bound=1%204096%20maybe",
            "run reads=2 records-bound=1%204096%20crossed records-bound=1%204096%20crossed",
        ] {
            assert!(reader(&Line::parse(bad)).is_err(), "{bad}");
        }
    }

    /// A member's refusal of a history its reader cannot take, and a new Mac's of a member's
    /// bundle, name the Mac whose aterm is short.
    ///
    /// NEGATIVE CONTROL (in the test): histories both sides take carry no refusal.
    #[test]
    fn a_capacity_refusal_names_the_mac_whose_aterm_is_short() {
        use super::super::bundle::source_refusal;
        use crate::crossing::RecordsBound;
        let (legacy, larger) = (RecordsBound::of(4096, false), RecordsBound::of(8192, false));
        let (crossed, larger_crossed) =
            (RecordsBound::of(4096, true), RecordsBound::of(8192, true));
        for (reader, source, update) in [
            (Reader::Files, legacy, "on the new Mac"),
            (Reader::LegacyLog, crossed, "on the new Mac"),
            (Reader::BoundedLog(legacy), larger_crossed, "on the new Mac"),
            (
                Reader::BoundedLog(larger_crossed),
                legacy,
                "on the allowing Mac",
            ),
        ] {
            assert!(!reader.can_take(source), "{reader:?} {source:?}");
            let why = reader.refusal(source);
            assert!(why.contains(&format!("update aterm {update}")), "{why}");
        }
        assert!(
            Reader::LegacyLog
                .refusal(crossed)
                .contains("an older aterm")
        );
        for (local, source, update) in [
            (legacy, Some(larger_crossed), "on this Mac"),
            (larger_crossed, Some(legacy), "on the allowing Mac"),
            (crossed, None, "on the allowing Mac"),
        ] {
            let why = source_refusal(local, source).expect("refused");
            assert!(why.contains(&format!("update aterm {update}")), "{why}");
        }
        // Control: histories both sides hold.
        assert!(Reader::LegacyLog.can_take(larger));
        assert!(Reader::BoundedLog(larger_crossed).can_take(larger_crossed));
        assert_eq!(source_refusal(larger, Some(legacy)), None);
        assert_eq!(source_refusal(legacy, None), None);
        assert_eq!(source_refusal(larger_crossed, Some(larger_crossed)), None);
    }

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

    /// Only a proof from another host on this member's home ALab LAN is from the home ALab LAN
    /// (D1, [`Proving::lan`]): one from another private network the member also reaches (a
    /// second interface's `10.0.0.0/8`, `172.16.0.0/12`, a VM bridge, a phone hotspot's
    /// `172.20.10.0/28`), from this member's own address there, or from a public or IPv6 address
    /// is not; a member on no home ALab LAN marks none, even from an address its old home held;
    /// and a personal vault's member marks none wherever it is.
    ///
    /// NEGATIVE CONTROLS: `Local::automatically_admits` taking every private source while the
    /// member has a home (an IPv4 or IPv4-mapped private address, in place of `home.holds`): the
    /// VM bridge's `192.168.64.3` reads as home. Taking any source while the member has none
    /// (`self.home.is_none_or(…)`): "no home: 192.168.64.3" fails.
    #[test]
    fn only_a_host_on_the_members_home_lan_is_from_the_home_lan() {
        let root = aterm_tempfile::tempdir().unwrap();
        let member = || Local::with(device(root.path(), "member"), TRUST, true, "member").unwrap();
        let ip = |text: &str| -> IpAddr { text.parse().unwrap() };
        let home = member().with_automatic_lan([]);
        assert!(home.automatically_admits(ip("192.168.1.45")));
        assert!(home.automatically_admits(ip("::ffff:192.168.1.46")));
        let elsewhere = [
            "192.168.64.3",
            "172.20.10.5",
            "10.0.0.9",
            "172.20.0.5",
            "192.168.2.45",
            "192.168.1.20",
            "93.184.216.34",
            "fe80::1",
        ];
        for from in elsewhere {
            assert!(!home.automatically_admits(ip(from)), "{from}");
        }
        let nowhere = member().with_automatic_lan([]).with_home(None);
        let personal = member().with_home(Some(crate::home_lan::HomeNet::test()));
        for from in elsewhere.into_iter().chain(["192.168.1.45"]) {
            assert!(!nowhere.automatically_admits(ip(from)), "no home: {from}");
            assert!(!personal.automatically_admits(ip(from)), "personal: {from}");
        }
    }

    /// A plain ask, even from this member's home ALab LAN on the pinned ALab vault, is a row that
    /// waits for a person's Allow: nothing lets a Mac in by its asking alone.
    #[test]
    fn a_plain_ask_from_the_home_lan_waits_for_a_person() {
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
        let rows = desk.rows(now);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].state, RowState::Asking);
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
    }
}
