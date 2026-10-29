// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A member's desk (§3.3.1, §3.3.4): the requests it heard, the phrases it shows, and the
//! member's side of the `ask` exchange ([`serve`]).
//!
//! A request is heard on `ask` and shows as a row until it expires ([`REQUEST_LIFETIME`]),
//! is ignored, or is closed: by this member's run that let it in, or by the new Mac's `done`
//! after another member's did. A closed request that asks again shows again: the new Mac lost
//! the bundle it was handed. Allow on a row shows a phrase bound to that request's `CI`
//! ([`Phrases::allow`]); invite shows one for whichever Mac uses it first. A run is CPace over
//! that phrase, then the possession challenge (step 8: `T_M` is checked by the new Mac before
//! it answers, and this member checks the answer before it signs anything), then the bundle
//! (step 9) under the run's bundle key. Runs are limited to [`RUNS_PER_MINUTE`] per source
//! address, counted when they start.

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

/// How long a heard request shows (§3.3.4 "Limits on phrases").
pub const REQUEST_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);
/// How many requests a desk holds; past it the oldest settled one (ignored, let in, or allowed
/// with a phrase that can no longer admit), else the oldest still asking, goes. A row whose
/// phrase can still admit never goes: at most [`crate::pake::MAX_LIVE`] rows hold one.
pub const MAX_HEARD: usize = 64;
/// The asking rows one source holds (an IPv4 address, an IPv6 /64): its next new request takes
/// the place of its own oldest, so one LAN host's flood never pushes another Mac's row out.
pub const PER_SOURCE: usize = 4;
/// From this many asking rows on, the band shows them as one row.
pub const COLLAPSE_AT: usize = 10;
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
    admitted: std::collections::BTreeSet<crate::membership_log::MemberId>,
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
        let can_allow = matches!(bootstrap.access(), Ok(Some(_))) || client::admits_by_log(paths);
        let admitted = client::log_admitted_actors(paths);
        let trust = bootstrap.trust();
        let request = device.request(trust.network, trust.authority)?;
        let name = client::config(paths)?
            .map(|config| config.host)
            .unwrap_or_default();
        Ok(Some(Self {
            device,
            request,
            trust,
            can_allow,
            name,
            admitted,
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
            admitted: std::collections::BTreeSet::new(),
        })
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
    /// Waiting for a person on some member.
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
}

struct Heard {
    id: u64,
    request: Request,
    node: [u8; 32],
    name: String,
    model: String,
    from: IpAddr,
    first: Instant,
    state: RowState,
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
    const fn word(self) -> &'static str {
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
    next: u64,
}

/// The fields of a `run` line, as [`Desk::begin`] checks them.
struct RunLine {
    wire: Vec<u8>,
    node: [u8; 32],
    name: String,
    nonce: Nonce,
    signature: [u8; 64],
}

/// A run between [`Desk::begin`] and its end.
struct Running {
    phrase: PhraseId,
    request: Request,
    node: [u8; 32],
    name: String,
    reads_log: bool,
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
            next: 1,
        }
    }

    /// The member this desk serves.
    pub const fn local(&self) -> &Local {
        &self.local
    }

    /// Replace the member (its roster binding, log admissions or name changed), keeping what it
    /// heard on the same network and closing rows whose Mac an `Admit` in the membership log has
    /// now let in (§3.3.1 step 6).
    pub fn refresh(&mut self, local: Local) {
        if self.local.trust != local.trust {
            *self = Self::new(local);
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
                }
            }
        }
        for heard in &mut self.heard {
            let actor = heard.request.binding().actor();
            if local.admitted.contains(&actor) && !self.local.admitted.contains(&actor) {
                if let RowState::Allowed(phrase) = heard.state {
                    self.phrases.cancel(phrase);
                }
                heard.state = RowState::Closed;
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

    fn expire(&mut self, now: Instant) {
        let phrases = &mut self.phrases;
        self.heard.retain(|heard| {
            let keep = now.saturating_duration_since(heard.first) < REQUEST_LIFETIME;
            if let (false, RowState::Allowed(phrase)) = (keep, heard.state) {
                phrases.cancel(phrase);
            }
            keep
        });
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

    /// Whether row `heard` is settled: ignored, let in, or allowed with a phrase that can no
    /// longer admit (spent, expired, or gone from the book).
    fn settled(&self, heard: &Heard, now: Instant) -> bool {
        match heard.state {
            RowState::Ignored | RowState::Closed => true,
            RowState::Asking => false,
            RowState::Allowed(phrase) => {
                !matches!(
                    self.phrases.state(phrase),
                    Some(PhraseState::Live | PhraseState::Running)
                ) || self.phrases.expires(phrase).is_none_or(|end| now >= end)
            }
        }
    }

    /// Drop row `at`, cancelling its phrase if one still shows.
    fn drop_row(&mut self, at: usize) {
        if let RowState::Allowed(phrase) = self.heard.remove(at).state {
            self.phrases.cancel(phrase);
        }
    }

    /// Make room for a new request from `from`: its source's own oldest asking row once it holds
    /// [`PER_SOURCE`], else, at [`MAX_HEARD`], the oldest settled row or the oldest asking one.
    fn make_room(&mut self, from: IpAddr, now: Instant) -> Result<()> {
        let source = source_of(from);
        let theirs: Vec<usize> = (0..self.heard.len())
            .filter(|&at| {
                self.heard[at].state == RowState::Asking && source_of(self.heard[at].from) == source
            })
            .collect();
        if theirs.len() >= PER_SOURCE {
            self.drop_row(theirs[0]);
            return Ok(());
        }
        if self.heard.len() < MAX_HEARD {
            return Ok(());
        }
        let settled = (0..self.heard.len()).find(|&at| self.settled(&self.heard[at], now));
        let asking = || {
            self.heard
                .iter()
                .position(|heard| heard.state == RowState::Asking)
        };
        let at = settled
            .or_else(asking)
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
        if model.is_empty() || !model.bytes().all(|b| b == b' ' || b.is_ascii_graphic()) {
            return Err(Error::new("that Mac names no model"));
        }
        let message = ask_message(self.local.key(), &node, name, model, request.bytes());
        UnparsedPublicKey::new(&ED25519, request.binding().writer())
            .verify(&message, signature)
            .map_err(|_| Error::new("that ask is not signed by the Mac whose request it names"))?;
        self.expire(now);
        if let Some(at) = self.find(&request) {
            // A Mac let in holds its bundle and asks no member again (`aterm members join` and
            // its subscriber stop at the Allowed it keeps), so a closed request that asks lost
            // its bundle between this member's send and its write: it asks anew, and an Allow
            // here can let it in again. A copied ask brings the row back at most; a run still
            // needs the request's keys.
            if self.heard[at].state == RowState::Closed {
                self.heard[at].state = RowState::Asking;
            }
            return Ok(self.answer(at, now));
        }
        self.make_room(from, now)?;
        let id = self.next;
        self.next += 1;
        self.heard.push(Heard {
            id,
            request,
            node,
            name: name.to_owned(),
            model: model.to_owned(),
            from,
            first: now,
            state: RowState::Asking,
        });
        Ok(Answer::Asking)
    }

    /// The rows the band shows, oldest first.
    #[must_use]
    pub fn rows(&self, now: Instant) -> Vec<Row> {
        self.heard
            .iter()
            .filter(|heard| now.saturating_duration_since(heard.first) < REQUEST_LIFETIME)
            .map(|heard| Row {
                id: heard.id,
                name: heard.name.clone(),
                model: heard.model.clone(),
                from: heard.from,
                code: client::join_code(&heard.request.binding().recipient()),
                state: heard.state,
            })
            .collect()
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
    /// This Mac cannot allow, the row is gone or closed, or four phrases are live already.
    pub fn allow(&mut self, id: u64, now: Instant) -> Result<PhraseId> {
        self.may_allow()?;
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
        self.row_mut(id)?.state = RowState::Allowed(phrase);
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
        self.row_mut(id)?.state = RowState::Ignored;
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
        if self.runs.iter().filter(|(at, _)| *at == from).count() >= RUNS_PER_MINUTE {
            return Err(Error::new(
                "too many join runs from that address; wait a minute",
            ));
        }
        self.runs.push((from, now));
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
        let (key, genesis) = (*self.local.key(), self.local.trust.genesis);
        let ci = ChannelId::new(&key, request.binding(), &node, &genesis);
        // The phrase a person Allowed for the request whose `CI` this run's is: a phrase typed
        // into another Mac starts no run here.
        let allowed = self.heard.iter().find_map(|heard| match heard.state {
            RowState::Allowed(phrase)
                if self.phrases.words(phrase, now).is_some()
                    && ChannelId::new(&key, heard.request.binding(), &heard.node, &genesis)
                        == ci =>
            {
                Some((phrase, allowed_name(&heard.name)))
            }
            _ => None,
        });
        let invited = || {
            self.invites
                .iter()
                .rev()
                .copied()
                .find(|phrase| self.phrases.words(*phrase, now).is_some())
                .map(|phrase| (phrase, name.to_owned()))
        };
        let (phrase, name) = allowed
            .or_else(invited)
            .ok_or_else(|| Error::new("no join phrase is shown on this Mac for that Mac"))?;
        let (run, nonce) = self.phrases.begin(phrase, &ci, nonce, now)?;
        Ok((
            Running {
                phrase,
                request,
                node,
                name,
                reads_log: false,
            },
            run,
            nonce,
        ))
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

/// The member's greeting line: its keys, its name, whether it can allow, whether it shows an
/// invite (phrase first) now, and this connection's `challenge`, which a run signs.
fn greeting(desk: &Desk, challenge: &[u8; CHALLENGE_LEN], now: Instant) -> String {
    let local = &desk.local;
    let invite = desk
        .invites
        .iter()
        .any(|phrase| desk.phrases.words(*phrase, now).is_some());
    format!(
        "ALAB member=1 key={} network={} authority={} genesis={} name={} allow={} invite={} \
         challenge={}",
        wire::hex(local.key()),
        wire::hex(&local.trust.network),
        wire::hex(&local.trust.authority),
        wire::hex(&local.trust.genesis),
        wire::escape(&local.name),
        u8::from(local.can_allow),
        u8::from(invite),
        wire::hex(challenge)
    )
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

/// Serve one `ask` connection from `from`: greet, then answer one `ask`, `done`, or a whole
/// run. `bundle` makes the bundle for a run ([`Handing`]); the run's phrase burns on any end
/// but a sent bundle. A run that sent its bundle returns the Allowed it signed, for the caller
/// to post to the vault's servers, and whether the bundle carried the log.
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
    let mut challenge = [0u8; CHALLENGE_LEN];
    if let Err(why) = aterm_uds::rand::fill(&mut challenge) {
        let error = Error::new(format!("no randomness for a join challenge: {why}"));
        let _ = wire::write_line(stream, &wire::refusal(&error.to_string()));
        return Err(error);
    }
    let hello = greeting(&desk.lock_or_recover(), &challenge, Instant::now());
    wire::write_line(stream, &hello)?;
    let line = wire::read_line(stream)?;
    let parsed = Line::parse(&line);
    let answered = match parsed.verb {
        "ask" => ask(desk, &parsed, from),
        "done" => done(desk, &parsed).map(|()| Answer::Closed),
        "run" => return run(desk, stream, &parsed, from, &challenge, bundle),
        _ => Err(Error::new(
            "the other Mac asked for something this Mac does not serve",
        )),
    };
    match answered {
        Ok(answer) => {
            wire::write_line(stream, &format!("OK state={}", answer.word()))?;
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

fn run<S: Read + Write + ?Sized>(
    desk: &Mutex<Desk>,
    stream: &mut S,
    first: &Line<'_>,
    from: IpAddr,
    challenge: &[u8; CHALLENGE_LEN],
    bundle: Hand<'_>,
) -> Result<Option<Let>> {
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
        desk.lock_or_recover()
            .begin(from, &line, challenge, Instant::now())
            .map(|(running, run, nonce)| {
                (
                    Running {
                        reads_log,
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
    let ended = steps(desk, stream, running, member_run, &nonce, bundle);
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
    bundle: Hand<'_>,
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
    })?;
    let sealed = session.seal_bundle(handout.bundle.as_slice())?;
    wire::write_line(stream, &format!("OK bundle={}", sealed.len()))?;
    stream.write_all(&sealed)?;
    stream.flush()?;
    desk.lock_or_recover().close(&running.request);
    Ok(Let {
        allowed,
        logged: handout.logged,
        note: handout.note,
    })
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
