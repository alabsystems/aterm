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
    let requester = session.peer(thread.requester)?;
    match thread.origin {
        Some(origin) => {
            session.peer_writer(thread.requester)?;
            session.peer_writer(origin)
        }
        None => Ok(requester),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_secrets::{
        age::Identity,
        commons::{binding::Request, journal::Trust},
    };

    fn binding(tag: u8, identity: u8) -> Binding {
        Request::create(
            [2; 16],
            [3; 32],
            [tag; 16],
            [tag; 16],
            &Identity::from_secret_bytes([identity; 32]),
            &[tag; 32],
        )
        .unwrap()
        .binding()
        .clone()
    }

    #[test]
    fn delivery_requires_both_current_writers_and_binds_the_entire_request_and_recipient() {
        let original = binding(40, 50);
        let successor = binding(41, 51);
        let other = binding(42, 52);
        let scope = descriptor_for_test();
        let mut context = Context {
            policy: Policy::compiled(true),
            signer: None,
            network: [2; 16],
            members: [original.actor(), successor.actor(), other.actor()]
                .into_iter()
                .collect(),
            scope: Some(scope),
            writers: [original.clone(), successor.clone(), other.clone()]
                .into_iter()
                .map(|binding| (binding.actor(), binding))
                .collect(),
            now_ms: 1_000,
        };
        let nonce = [11; 16];
        let requester = successor.actor();
        let mut body = Map::new();
        body.insert("nonce".into(), Value::from(hex::encode_lower(&nonce)));
        body.insert(
            "want".into(),
            Value::from(Want::Mailbox("recovered".into()).key()),
        );
        body.insert("purpose".into(), Value::from(Purpose::ByHand.id()));
        body.insert(
            "origin".into(),
            Value::from(hex::encode_lower(&original.actor())),
        );
        let mut record = Record::new("request", &request_key(&requester, &nonce), body);
        let signature = crate::crypto::signed(
            &[40; 32],
            DOMAIN,
            &message(scope, requester, original.recipient(), &record).unwrap(),
        )
        .unwrap();
        record.body.insert(
            "origin_att".into(),
            Value::from(hex::encode_lower(&signature)),
        );
        let entry = Entry {
            actor: requester,
            sequence: 1,
            id: [9; 32],
            received_ms: 1_000,
            record,
        };
        let key = ids(&entry.record.key).unwrap();
        let project = |entry: Entry, context: &Context| Book::project(vec![entry], context);
        assert_eq!(
            project(entry.clone(), &context).threads[&key].origin,
            Some(original.actor())
        );

        for (field, value) in [
            ("want", Value::from(Want::Mailbox("another".into()).key())),
            ("purpose", Value::from(Purpose::HarnessSignIn.id())),
            ("refused", Value::from(7_u64)),
            ("supersedes", Value::from(hex::encode_lower(&[8; 16]))),
            ("origin", Value::from(hex::encode_lower(&other.actor()))),
            ("origin_att", Value::from(hex::encode_lower(&[0; 64]))),
        ] {
            let mut changed = entry.clone();
            changed.record.body.insert(field.into(), value);
            assert!(
                project(changed, &context).threads.is_empty(),
                "changed {field}"
            );
        }
        let mut changed = entry.clone();
        changed.actor = other.actor();
        changed.record.key = hex::encode_lower(&request_key(&changed.actor, &nonce));
        assert!(project(changed, &context).threads.is_empty());
        let mut changed = entry.clone();
        changed
            .record
            .body
            .insert("nonce".into(), Value::from(hex::encode_lower(&[12; 16])));
        changed.record.key = hex::encode_lower(&request_key(&requester, &[12; 16]));
        assert!(project(changed, &context).threads.is_empty());

        context.scope = Some([99; 32]);
        assert!(project(entry.clone(), &context).threads.is_empty());
        context.scope = Some(scope);
        for actor in [original.actor(), requester] {
            let saved = context.writers.remove(&actor).unwrap();
            assert!(project(entry.clone(), &context).threads.is_empty());
            context.writers.insert(actor, saved);
        }
        context.writers.insert(original.actor(), binding(40, 53));
        assert!(
            project(entry, &context).threads.is_empty(),
            "rebound age recipient"
        );
    }

    fn descriptor_for_test() -> Digest {
        Descriptor::application(
            Trust {
                network: [2; 16],
                authority: [3; 32],
                genesis: [4; 32],
            },
            "credentials",
            "requests",
            1,
        )
        .unwrap()
        .scope
        .digest()
    }
}
