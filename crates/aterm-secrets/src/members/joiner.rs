// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The joining Mac's side of the `ask` exchange: read a member's greeting, ask, run the phrase
//! a person typed, and say `done` to the other members it asked.

use super::desk::{Answer, CHALLENGE_LEN, DONE_DOMAIN, ask_message, run_message};
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
/// whether it can allow. Nothing in it is trusted until a run confirms: every key in it is in
/// the run's `CI`, so a greeting with any key swapped fails the run.
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

/// Ask the member `greeting` came from to show this Mac's request, signed by this Mac's writer
/// key over its node key, name and model ([`ask_message`]).
///
/// # Errors
/// The stream failed or the member refused.
pub fn ask<S: Read + Write + ?Sized>(
    stream: &mut S,
    greeting: &Greeting,
    joining: &Joining<'_>,
) -> Result<Answer> {
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
            "ask request={} node={} name={} model={} sig={}",
            wire::b64(request.bytes()),
            wire::hex(&joining.node),
            wire::escape(&name),
            wire::escape(&model),
            wire::hex(signature.as_ref())
        ),
    )?;
    state(&wire::read_line(stream)?)
}

fn state(line: &str) -> Result<Answer> {
    let parsed = wire::answer(line)?;
    Answer::of_word(parsed.get("state")?)
        .ok_or_else(|| Error::new("the member answered an unknown state"))
}

/// Run `phrase` with the member `greeting` came from: a run line signed by this Mac's writer key
/// over the greeting's challenge ([`run_message`]), CPace, then the possession challenge
/// (answered only after the member's tag checked), then the bundle, opened under the run's
/// key. The bundle's Allowed must name this Mac's request and be signed by that member.
///
/// # Errors
/// The stream failed, the member refused, the phrase was wrong, or the bundle does not hold.
pub fn run<S: Read + Write + ?Sized>(
    stream: &mut S,
    greeting: &Greeting,
    joining: &Joining<'_>,
    phrase: &Phrase,
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
            "run request={} node={} name={} nonce={} sig={} reads=2",
            wire::b64(request.bytes()),
            wire::hex(&joining.node),
            wire::escape(&joining.name),
            wire::hex(&nonce),
            wire::hex(signature.as_ref())
        ),
    )?;
    let line = wire::read_line(stream)?;
    let theirs = wire::answer(&line)?.hex::<{ crate::pake::NONCE_LEN }>("nonce")?;
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
    let size: usize = wire::answer(&line)?
        .get("bundle")?
        .parse()
        .ok()
        .filter(|size| *size <= MAX_BUNDLE + 64)
        .ok_or_else(|| Error::new("the member's bundle size is out of bounds"))?;
    let mut sealed = vec![0u8; size];
    stream.read_exact(&mut sealed)?;
    let plain = session.open_bundle(&sealed)?;
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
