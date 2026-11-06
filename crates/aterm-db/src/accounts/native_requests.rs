// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native allocation requests are actor-authenticated hints on an ordinary
//! application table. They confer no preparation or execution authority.

use crate::{
    Error, Result,
    catalog::Descriptor,
    database::{Change, Command},
    documents::log::Log,
};
use astream_wire::sync::{Actor, Digest};
use aterm_digest::Sha256;
use aterm_json::json;
use aterm_secrets::{
    client::Paths,
    commons::{binding::Binding, bootstrap::Bootstrap},
};
use std::time::Instant;

const DOMAIN: &[u8] = b"aterm/native-allocation-request/1\0";
const MAX_PAYLOAD: usize = 2048;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Target {
    pub intent: bool,
    pub account: Digest,
    pub revision: Digest,
    pub provider: String,
    pub grant: [u8; 16],
}

impl Target {
    fn validate(&self) -> Result<()> {
        if self.account == [0; 32]
            || self.revision == [0; 32]
            || !matches!(self.provider.as_str(), "google" | "meta")
            || self.grant == [0; 16]
        {
            return Err(Error::new("invalid native allocation subject"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct Request {
    pub operation: Digest,
    pub target: Target,
    pub policy: Digest,
    pub requester: Binding,
}

pub struct View {
    pub local: Binding,
    pub members: Vec<Binding>,
    pub requests: Vec<Request>,
}

pub struct Requests {
    paths: Paths,
    deadline: Instant,
}

fn descriptor(bootstrap: &Bootstrap) -> Result<Descriptor> {
    Descriptor::application(bootstrap.trust(), "accounts", "native-requests", 1)
}

impl Requests {
    pub fn at_until(paths: Paths, deadline: Instant) -> Self {
        Self { paths, deadline }
    }

    /// Publish only this current writer's request. Exact repeated requests
    /// return their original operation instead of adding another queue item.
    pub fn request(&self, target: &Target, policy: Digest) -> Result<Digest> {
        target.validate()?;
        if policy == [0; 32] {
            return Err(Error::new("native request needs a current policy"));
        }
        crate::documents::application::with_store_until(
            &self.paths,
            true,
            descriptor,
            self.deadline,
            |session, store, scope| {
                session.writer()?;
                session.peer_writer(session.actor())?;
                let command = command(session.actor(), target, policy);
                let payload = command.encode()?;
                decode(&payload)?;
                let log = Log::read_with(session, store, scope, decode)?;
                log.writable(session.actor())?;
                if let Some(id) = log.retry(session.actor(), &command)? {
                    return Ok(id);
                }
                crate::documents::application::append(session, store, &payload)
                    .map(|receipt| receipt.id)
            },
        )
    }

    /// Read authenticated current requesters. The allocator still verifies the
    /// exact signed policy, configuration, input availability and local Off.
    pub fn view(&self) -> Result<View> {
        self.view_filtered(None)
    }

    /// Keep only hints for the currently observed execution subject and policy.
    /// The filter is not an authority check; the composing allocator owns that.
    pub fn view_for(&self, target: &Target, policy: Digest) -> Result<View> {
        target.validate()?;
        self.view_filtered(Some((target, policy)))
    }

    fn view_filtered(&self, filter: Option<(&Target, Digest)>) -> Result<View> {
        crate::documents::application::observe_store_until(
            &self.paths,
            descriptor,
            self.deadline,
            |session, store, scope| {
                let local = session.peer_writer(session.actor())?;
                let members = session.observed_account_targets()?;
                let log = Log::read_with(session, store, scope, decode)?;
                let mut requests = Vec::new();
                for (operation, decoded) in log.eligible() {
                    if Instant::now() >= self.deadline {
                        return Err(Error::new("native request observation expired"));
                    }
                    let (target, policy) = body(&decoded)?;
                    if filter
                        .is_some_and(|(expected, digest)| *expected != target || digest != policy)
                    {
                        continue;
                    }
                    if command(operation.header.actor, &target, policy) != decoded {
                        continue;
                    }
                    let Ok(requester) = session.peer_writer(operation.header.actor) else {
                        continue;
                    };
                    requests.push(Request {
                        operation: operation.header.id,
                        target,
                        policy,
                        requester,
                    });
                }
                Ok(View {
                    local,
                    members,
                    requests,
                })
            },
        )
    }
}

fn command(actor: Actor, target: &Target, policy: Digest) -> Command {
    let value = json!({"scope":if target.intent{"intent"}else{"preparation"},
        "account":aterm_codec::hex::encode_lower(&target.account),"revision":aterm_codec::hex::encode_lower(&target.revision),
        "provider":target.provider,"grant":aterm_codec::hex::encode_lower(&target.grant),"policy":aterm_codec::hex::encode_lower(&policy)});
    let mut hash = Sha256::new();
    hash.update(DOMAIN);
    hash.update(actor);
    hash.update(aterm_json::to_vec(&value).expect("fixed metadata request"));
    let key = hash.finalize();
    Command::Write {
        request: key[..16].try_into().unwrap(),
        changes: vec![Change {
            collection: "requests".into(),
            key: aterm_codec::hex::encode_lower(&key),
            value: Some(value),
        }],
    }
}

fn body(command: &Command) -> Result<(Target, Digest)> {
    let Command::Write { changes, .. } = command else {
        return Err(Error::new("native allocation request is not a write"));
    };
    let [change] = changes.as_slice() else {
        return Err(Error::new("native allocation request must have one row"));
    };
    let value = change
        .value
        .as_ref()
        .filter(|_| change.collection == "requests")
        .ok_or_else(|| Error::new("native allocation request is not a hint"))?;
    if value.as_object().is_none_or(|value| value.len() != 6) {
        return Err(Error::new("invalid native request fields"));
    }
    let digest = |name: &str| -> Result<Digest> {
        let text = value[name]
            .as_str()
            .ok_or_else(|| Error::new("invalid native request identifier"))?;
        let digest = crate::database::decode_hex::<32>(text)?;
        if digest == [0; 32] {
            return Err(Error::new("invalid native request identifier"));
        }
        Ok(digest)
    };
    let target = Target {
        intent: match value["scope"].as_str() {
            Some("intent") => true,
            Some("preparation") => false,
            _ => return Err(Error::new("invalid native request scope")),
        },
        account: digest("account")?,
        revision: digest("revision")?,
        provider: value["provider"].as_str().unwrap_or("").into(),
        grant: crate::database::decode_hex::<16>(
            value["grant"]
                .as_str()
                .ok_or_else(|| Error::new("native request has no grant"))?,
        )?,
    };
    target.validate()?;
    let policy = digest("policy")?;
    let key = crate::database::decode_hex::<32>(&change.key)?;
    if command.request() != key[..16] {
        return Err(Error::new("native request retry identifier differs"));
    }
    Ok((target, policy))
}

fn decode(bytes: &[u8]) -> Result<Command> {
    if bytes.len() > MAX_PAYLOAD {
        return Err(Error::new("native request exceeds its bound"));
    }
    let command = Command::decode(bytes)?;
    if command.encode()? != bytes {
        return Err(Error::new("native request is not canonical"));
    }
    body(&command)?;
    Ok(command)
}

#[cfg(test)]
mod tests;
