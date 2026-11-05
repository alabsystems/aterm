// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The joining Mac's side of the `ask` exchange: read a member's greeting, ask, run the phrase
//! a person typed, and say `done` to the other members it asked.

use super::desk::{
    Answer, CHALLENGE_LEN, DONE_DOMAIN, GREET_NONCE_LEN, ask_message, greet_message, prove_message,
    run_message,
};
use super::wire::{self, Line};
use super::{Bundle, bundle::MAX_BUNDLE};
use crate::commons::binding::Request;
use crate::commons::bootstrap::Device;
use crate::pake::{ChannelId, Joiner};
use crate::phrase::Phrase;
use crate::{Error, Result};
use ring::signature::Ed25519KeyPair;
use std::io::{Read, Write};

/// What a member says first: its writer key, the network it is a member of, its name, and
/// whether it can allow. Any host can send one, a copy of a member's included: a member's answer
/// to this Mac's ask signs it over a fresh nonce ([`ask_checked`], [`Signed::Verified`]), and
/// every key in it is in the run's `CI`, so a greeting with any key swapped fails the run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Greeting {
    /// The member's Commons writer key.
    pub key: [u8; 32],
    /// The network's identity.
    pub network: [u8; 16],
    /// The network's control authority.
    pub authority: [u8; 32],
    /// The network's genesis digest.
    pub genesis: [u8; 32],
    /// The member's machine name.
    pub name: String,
    /// Whether the member can allow.
    pub can_allow: bool,
    /// Whether the member shows an invite now (phrase first).
    pub invite: bool,
    /// The challenge a run on this connection signs, fresh for each connection.
    pub challenge: [u8; CHALLENGE_LEN],
}

/// The Mac that asks: its Commons device, its node key, and the name and model it asks under.
pub struct Joining<'a> {
    /// This Mac's Commons device.
    pub device: &'a Device,
    /// Capacity and crossed state read from this device's actual local custody.
    pub records_bound: crate::crossing::RecordsBound,
    /// This Mac's node key, in the run's `CI`.
    pub node: [u8; 32],
    /// The machine name it asks for.
    pub name: String,
    /// Its model, for the row.
    pub model: String,
}

impl Joining<'_> {
    /// This Mac's request in the network `greeting` names.
    ///
    /// # Errors
    /// The device cannot sign one.
    pub fn request(&self, greeting: &Greeting) -> Result<Request> {
        self.device.request(greeting.network, greeting.authority)
    }
}

/// Read a member's greeting.
///
/// # Errors
/// The stream failed or the line is not a member's greeting.
pub fn greeting<S: Read + ?Sized>(stream: &mut S) -> Result<Greeting> {
    let line = wire::read_line(stream)?;
    let parsed = Line::parse(&line);
    if parsed.verb == "NO" {
        wire::answer(&line)?;
    }
    if parsed.verb != "ALAB" || parsed.get("member")? != "1" {
        return Err(Error::new("that address is not an ALab member"));
    }
    Ok(Greeting {
        key: parsed.hex("key")?,
        network: parsed.hex("network")?,
        authority: parsed.hex("authority")?,
        genesis: parsed.hex("genesis")?,
        name: parsed.text("name")?,
        can_allow: parsed.get("allow")? == "1",
        invite: parsed.get("invite").is_ok_and(|invite| invite == "1"),
        challenge: parsed.hex("challenge")?,
    })
}

/// `message` signed by this Mac's Commons writer key, the key its request binds.
pub(super) fn sign(device: &Device, message: &[u8]) -> Result<ring::signature::Signature> {
    device.with_seed(|seed| {
        Ed25519KeyPair::from_seed_unchecked(seed)
            .map(|key| key.sign(message))
            .map_err(|_| Error::new("invalid Commons writer seed"))
    })
}

/// The signature [`ask`] sends with `request` (its bytes) to the member whose key is `member`,
/// under `node`, `name` and `model`: the seam other crates' tests put a row on a
/// [`super::desk::Desk`] with ([`super::desk::Asking`]).
///
/// # Errors
/// This Mac's Commons writer seed is invalid.
#[cfg(any(test, debug_assertions))]
pub fn ask_signature(
    device: &Device,
    member: &[u8; 32],
    node: &[u8; 32],
    name: &str,
    model: &str,
    request: &[u8],
) -> Result<[u8; 64]> {
    let signature = sign(device, &ask_message(member, node, name, model, request))?;
    signature
        .as_ref()
        .try_into()
        .map_err(|_| Error::new("an Ed25519 signature is 64 bytes"))
}

/// Whether a member's answer proved, on its own connection, the key its greeting names
/// ([`ask_checked`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signed {
    /// It signed this Mac's fresh nonce, its greeting and its answer with that key.
    Verified,
    /// It signed nothing: a member from before signed greetings, or a host posing as one.
    Unsigned,
}

/// [`ask_checked`], whose answer alone it returns.
///
/// # Errors
/// As [`ask_checked`].
pub fn ask<S: Read + Write + ?Sized>(
    stream: &mut S,
    greeting: &Greeting,
    joining: &Joining<'_>,
) -> Result<Answer> {
    ask_checked(stream, greeting, joining).map(|(answer, _)| answer)
}

/// Ask the member `greeting` came from to show this Mac's request, signed by this Mac's writer
/// key over its node key, name and model ([`ask_message`]), with a fresh nonce the member's
/// answer signs ([`greet_message`]): its answer, and whether it was signed by the key `greeting`
/// names.
///
/// # Errors
/// The stream failed, the member refused, or its answer carries a signature that does not
/// verify (a host answering with another connection's signature, or as a member it is not).
pub fn ask_checked<S: Read + Write + ?Sized>(
    stream: &mut S,
    greeting: &Greeting,
    joining: &Joining<'_>,
) -> Result<(Answer, Signed)> {
    let mut nonce = [0u8; GREET_NONCE_LEN];
    aterm_uds::rand::fill(&mut nonce)
        .map_err(|why| Error::new(format!("no randomness for an ask: {why}")))?;
    let request = joining.request(greeting)?;
    // What the line carries, after its escaping and cut, is what is signed.
    let name = wire::unescape(&wire::escape(&joining.name))?;
    let model = wire::unescape(&wire::escape(&joining.model))?;
    let signature = sign(
        joining.device,
        &ask_message(&greeting.key, &joining.node, &name, &model, request.bytes()),
    )?;
    wire::write_line(
        stream,
        &format!(
            "ask request={} node={} name={} model={} sig={} gnonce={}{}",
            wire::b64(request.bytes()),
            wire::hex(&joining.node),
            wire::escape(&name),
            wire::escape(&model),
            wire::hex(signature.as_ref()),
            wire::hex(&nonce),
            wire::lab_field(joining.device.lab())
        ),
    )?;
    let line = wire::read_line(stream)?;
    let answer = state(&line)?;
    let parsed = Line::parse(&line);
    if parsed.get("gsig").is_err() {
        return Ok((answer, Signed::Unsigned));
    }
    if !astream_aead::signature::verify(
        &greeting.key,
        &greet_message(&nonce, greeting, answer),
        &parsed.hex::<64>("gsig")?,
    ) {
        return Err(Error::new(
            "that answer is not signed by the member its greeting names: another host may be \
             answering as it",
        ));
    }
    Ok((answer, Signed::Verified))
}

fn state(line: &str) -> Result<Answer> {
    let parsed = wire::answer(line)?;
    Answer::of_word(parsed.get("state")?)
        .ok_or_else(|| Error::new("the member answered an unknown state"))
}

/// Run `phrase` with the member `greeting` came from: a run line signed by this Mac's writer key
/// over the greeting's challenge ([`run_message`]), CPace, then the possession challenge
/// (answered only after the member's tag checked), then the bundle, opened under the run's
/// key. The bundle's Allowed must name this Mac's request and be signed by that member; this
/// acknowledges every bundle that holds, where a join acknowledges only one its write takes
/// ([`run_noting_send`]'s `accepts`).
///
/// # Errors
/// The stream failed, the member refused, the phrase was wrong, or the bundle does not hold.
pub fn run<S: Read + Write + ?Sized>(
    stream: &mut S,
    greeting: &Greeting,
    joining: &Joining<'_>,
    phrase: &Phrase,
) -> Result<Bundle> {
    run_noting_send(
        stream,
        greeting,
        joining,
        phrase,
        &mut Sending::default(),
        &|_| Ok(()),
    )
}

/// What [`run_noting_send`] notes of a run as it goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sending {
    /// The run line went to the member, which from there holds the phrase's try.
    pub sent: bool,
    /// The member answered busy and named how long the phrase has left (`left=`); `None` from
    /// a member that names none, or one that was not busy.
    pub left: Option<std::time::Duration>,
}

/// [`run`], setting `sending.sent` once the run line went to the member: from there the member holds
/// the phrase's try, which a phrase is good for one of. An error with `sent` still false came
/// before the member was asked anything of the phrase (this Mac could not build its request or
/// sign the run, the stream failed before the line went, or the member answered it busy,
/// [`crate::ErrorKind::MemberBusy`], before its run touched the phrase), so the same words go
/// again, while the phrase lives: a busy member names how long that is (`sending.left`).
///
///
/// The bundle is acknowledged only once `accepts` takes it (`client::accepts_bundle`: every
/// check this Mac's write of it runs, nothing written), so the member closes its row only on a
/// bundle this Mac will write; one it refuses leaves the row allowed, its phrase spent, and a
/// person's Allow again works at once. [`run`] accepts every bundle that opens and allows this
/// Mac.
///
/// # Errors
/// As [`run`], or `accepts` refused the bundle (why).
pub fn run_noting_send<S: Read + Write + ?Sized>(
    stream: &mut S,
    greeting: &Greeting,
    joining: &Joining<'_>,
    phrase: &Phrase,
    sending: &mut Sending,
    accepts: &dyn Fn(&Bundle) -> Result<()>,
) -> Result<Bundle> {
    let request = joining.request(greeting)?;
    let ci = ChannelId::new(
        &greeting.key,
        request.binding(),
        &joining.node,
        &greeting.genesis,
    );
    let (joiner, nonce) = Joiner::start(phrase, ci)?;
    let signature = sign(
        joining.device,
        &run_message(
            &greeting.key,
            &greeting.challenge,
            &nonce,
            &joining.node,
            request.bytes(),
        ),
    )?;
    wire::write_line(
        stream,
        &format!(
            "run request={} node={} name={} nonce={} sig={} reads=2 ack=1 records-bound={}{}",
            wire::b64(request.bytes()),
            wire::hex(&joining.node),
            wire::escape(&joining.name),
            wire::hex(&nonce),
            wire::hex(signature.as_ref()),
            wire::escape(&joining.records_bound.value()),
            wire::lab_field(joining.device.lab())
        ),
    )?;
    sending.sent = true;
    let line = wire::read_line(stream)?;
    let theirs = match wire::answer(&line) {
        // A member busy with a vault pass answers before its run touches the phrase: the same
        // words go to it again ([`crate::ErrorKind::MemberBusy`]) while the phrase lives.
        Err(error) if error.kind() == crate::ErrorKind::MemberBusy => {
            sending.sent = false;
            sending.left = wire::busy_left(&line);
            return Err(error);
        }
        answered => answered?.hex::<{ crate::pake::NONCE_LEN }>("nonce")?,
    };
    let (shared, ours) = joiner.on_nonce(&theirs)?;
    wire::write_line(stream, &format!("share y={}", wire::hex(&ours)))?;
    let line = wire::read_line(stream)?;
    let theirs = wire::answer(&line)?.hex::<32>("y")?;
    let (confirming, tag) = shared.on_share(&theirs)?;
    wire::write_line(stream, &format!("confirm t={}", wire::hex(&tag)))?;
    let line = wire::read_line(stream)?;
    let parsed = wire::answer(&line)?;
    let session = confirming.on_confirm(&parsed.hex::<32>("t")?)?;
    let challenge = parsed.b64("challenge")?;
    let response = joining.device.answer(&request, &challenge)?;
    wire::write_line(
        stream,
        &format!("possess response={}", wire::b64(&response)),
    )?;
    let line = wire::read_line(stream)?;
    let size: usize = Some(wire::answer(&line)?.get("bundle")?)
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|s| s.parse().ok())
        .filter(|size| *size <= MAX_BUNDLE + 64)
        .ok_or_else(|| Error::new("the member's bundle size is out of bounds"))?;
    let mut sealed = vec![0u8; size];
    stream.read_exact(&mut sealed)?;
    let (plain, ack) = session.open_bundle_acknowledged(&sealed)?;
    let bundle = Bundle::decode(plain.as_slice())?;
    if bundle.allowed.binding() != request.binding() || bundle.allowed.signer() != &greeting.key {
        return Err(Error::new(
            "the member's bundle allows another Mac or was signed by another member",
        ));
    }
    // A bundle this Mac's write would refuse goes unacknowledged: the member keeps its row
    // allowed (a member from before acknowledgements closed it at its write and reads none).
    accepts(&bundle)?;
    // This Mac holds a bundle it accepts: the member closes its row on this word (a failed write
    // here changes nothing this Mac holds).
    drop(wire::write_line(
        stream,
        &format!("ack t={}", wire::hex(&ack)),
    ));
    Ok(bundle)
}

/// How a joining Mac signs with its fleet node key: `NodeIdentity::sign_record(PROVE_RECORD,
/// message)` ([`super::desk::PROVE_RECORD`]).
pub type SignNode<'a> = &'a (dyn Fn(&[u8]) -> [u8; 64] + Sync);

/// Prove this Mac to the member that sent `greeting` (`prove`), with nobody's Allow: sign the
/// greeting's fresh challenge with the request's writer key and the fleet node key
/// (`sign_node`), answer the possession challenge, and open the bundle the member sealed to this
/// Mac's age identity. `sent` turns true before the possession answer's write: a failed write or
/// flush may still have delivered it, letting the member sign this Mac's records. The caller
/// tries no other member at once (two members removing one old seat at one cutoff would make a
/// quorum `Remove`).
///
/// # Errors
/// The member refused (why, from its `NO` line), the stream failed, or the bundle does not open,
/// allows another Mac or was signed by another member.
pub fn prove<S: Read + Write + ?Sized>(
    stream: &mut S,
    greeting: &Greeting,
    joining: &Joining<'_>,
    sign_node: SignNode<'_>,
    sent: &mut bool,
) -> Result<Bundle> {
    let request = joining.request(greeting)?;
    let message = prove_message(
        &greeting.key,
        &greeting.challenge,
        &joining.node,
        request.bytes(),
    );
    let signature = sign(joining.device, &message)?;
    wire::write_line(
        stream,
        &format!(
            "prove request={} node={} name={} sig={} nodesig={} reads=2 records-bound={}{}",
            wire::b64(request.bytes()),
            wire::hex(&joining.node),
            wire::escape(&joining.name),
            wire::hex(signature.as_ref()),
            wire::hex(&sign_node(&message)),
            wire::escape(&joining.records_bound.value()),
            wire::lab_field(joining.device.lab())
        ),
    )?;
    let line = wire::read_line(stream)?;
    let challenge = wire::answer(&line)?.b64("challenge")?;
    let response = joining.device.answer(&request, &challenge)?;
    let possession = format!("possess response={}", wire::b64(&response));
    *sent = true;
    wire::write_line(stream, &possession)?;
    let line = wire::read_line(stream)?;
    let size: usize = Some(wire::answer(&line)?.get("bundle")?)
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|s| s.parse().ok())
        .filter(|size| *size <= MAX_BUNDLE + 1024)
        .ok_or_else(|| Error::new("the member's bundle size is out of bounds"))?;
    let mut sealed = vec![0u8; size];
    stream.read_exact(&mut sealed)?;
    let plain = joining.device.open_sealed(&sealed)?;
    let bundle = Bundle::decode(plain.as_slice())?;
    if bundle.allowed.binding() != request.binding() || bundle.allowed.signer() != &greeting.key {
        return Err(Error::new(
            "the member's bundle allows another Mac or was signed by another member",
        ));
    }
    Ok(bundle)
}

/// Tell the member `greeting` came from that this Mac was let in, so its row closes.
///
/// # Errors
/// The stream failed or the member refused.
pub fn done<S: Read + Write + ?Sized>(
    stream: &mut S,
    greeting: &Greeting,
    joining: &Joining<'_>,
) -> Result<()> {
    let request = joining.request(greeting)?;
    let message = [DONE_DOMAIN, greeting.key.as_slice(), request.bytes()].concat();
    let signature = sign(joining.device, &message)?;
    wire::write_line(
        stream,
        &format!(
            "done request={} sig={}",
            wire::b64(request.bytes()),
            wire::hex(signature.as_ref())
        ),
    )?;
    state(&wire::read_line(stream)?).map(drop)
}
