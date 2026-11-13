// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A successor signs the request as its new table actor. The original current
//! Writer separately authorizes delivery to its own vault recipient. Neither
//! signature delegates table counters, account rights or arbitrary recipients.

use super::*;
use aterm_secrets::commons::binding::Binding;

const DOMAIN: &[u8] = b"aterm/credentials/logical-recipient/1\0";

fn message(
    scope: Digest,
    requester: Actor,
    recipient: aterm_secrets::age::Recipient,
    record: &Record,
) -> Result<Vec<u8>> {
    let canonical_body = if record.body.contains_key("origin_att") {
        let mut body = record.body.clone();
        body.remove("origin_att");
        canonical(&body)?
    } else {
        canonical(&record.body)?
    };
    let key = ids(&record.key)?;
    let mut bytes = Vec::with_capacity(32 + 48 + 32 + 16 + canonical_body.len());
    bytes.extend_from_slice(&scope);
    bytes.extend_from_slice(&requester);
    bytes.extend_from_slice(&recipient.0);
    bytes.extend_from_slice(&key);
    bytes.extend_from_slice(&canonical_body);
    Ok(bytes)
}

pub(super) struct Origin(Bootstrap);

impl Origin {
    pub(super) fn open(paths: &Paths) -> Result<Option<Self>> {
        if crate::generation::selected(paths)? == *paths {
            return Ok(None);
        }
        let bootstrap = Bootstrap::required(paths).map_err(crate::table::failure)?;
        Session::observe(&bootstrap)?.may_write()?;
        Ok(Some(Self(bootstrap)))
    }

    pub(super) fn bind(
        &self,
        session: &Session<'_>,
        scope: Scope,
        mut record: Record,
    ) -> Result<Record> {
        let origin = Session::observe(&self.0)?;
        origin.may_write()?;
        session.may_write()?;
        if self.0.trust() != scope.trust() || origin.actor() == session.actor() {
            return Err(Error::new(
                "credential origin has another writer or trust context",
            ));
        }
        record.body.insert(
            "origin".into(),
            Value::from(hex::encode_lower(&origin.actor())),
        );
        let recipient = origin.peer_writer(origin.actor())?.recipient();
        let body = message(scope.digest(), session.actor(), recipient, &record)?;
        let signature = self
            .0
            .device()
            .with_seed(|seed| crate::crypto::signed(seed, DOMAIN, &body))?;
        record.body.insert(
            "origin_att".into(),
            Value::from(hex::encode_lower(&signature)),
        );
        Ok(record)
    }
}

pub(super) fn origin(
    record: &Record,
    requester: Actor,
    context: &Context,
) -> Result<Option<Actor>> {
    let Some(value) = record.body.get("origin") else {
        return Ok(None);
    };
    let original = hex_field::<48>(value).ok_or_else(|| Error::new("invalid credential origin"))?;
    let binding = context
        .writers
        .get(&original)
        .filter(|binding| binding.actor() != requester)
        .ok_or_else(|| Error::new("credential origin is not a current Writer"))?;
    if !context.writers.contains_key(&requester) {
        return Err(Error::new("credential successor is not a current Writer"));
    }
    let scope = context
        .scope
        .ok_or_else(|| Error::new("credential delivery has no table context"))?;
    let signature = record
        .body
        .get("origin_att")
        .and_then(hex_field::<64>)
        .ok_or_else(|| Error::new("credential origin has no authorization"))?;
    crate::crypto::verify(
        binding.writer(),
        DOMAIN,
        &message(scope, requester, binding.recipient(), record)?,
        &signature,
    )?;
    Ok(Some(original))
}

pub(super) fn destination(session: &Session<'_>, thread: &Thread) -> Result<Binding> {
    match thread.origin {
        Some(origin) => {
            session.peer_writer(thread.requester)?;
            session.peer_writer(origin)
        }
        None => session.peer(thread.requester),
    }
}

#[cfg(test)]
#[path = "delivery_tests.rs"]
mod tests;
