// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! Create an encrypted Proton draft and submit one delivery attempt.
use super::{Proton, field};
use aterm_http::Method;
use aterm_json::{Value, json};

impl Proton {
    pub fn send(&mut self, draft: &crate::Draft) -> Result<(), String> {
        // Use the same bounded address and header validation as SMTP.
        let _validated = crate::smtp::encode(draft)?;
        let index = self
            .send_keys
            .iter()
            .find(|(email, _)| email.eq_ignore_ascii_case(&draft.from))
            .map(|(_, index)| *index)
            .ok_or("Sender address has no unlocked mail key")?;
        let sender = self
            .keys
            .get(index)
            .ok_or("Sender encryption key is unavailable")?
            .recipient();
        let body = crate::crypto::pgp::EncryptedBody::new(draft.text.as_bytes())?;
        let mut addresses = aterm_json::Map::new();
        let mut types = 0u64;
        let mut clear = false;
        let recipients: std::collections::BTreeSet<&str> = draft
            .to
            .iter()
            .chain(&draft.cc)
            .chain(&draft.bcc)
            .map(String::as_str)
            .collect();
        if recipients.len() > 100 {
            return Err("A message may have at most 100 recipients".into());
        }
        for address in recipients {
            let lookup = self.call(
                Method::Get,
                &format!("/core/v4/keys?Email={}", crate::oauth::encode(address)),
                None,
            )?;
            let recipient_type = lookup["RecipientType"]
                .as_u64()
                .ok_or("Proton did not classify a recipient")?;
            let public = lookup["Keys"]
                .as_array()
                .and_then(|keys| keys.first())
                .and_then(|key| key["PublicKey"].as_str())
                .filter(|key| !key.is_empty());
            if let Some(public) = public {
                let key = crate::crypto::pgp::public_keys(public)?
                    .into_iter()
                    .next()
                    .ok_or("Recipient encryption key is unavailable")?;
                let kind = if recipient_type == 1 { 1u64 } else { 8u64 };
                types |= kind;
                let packet = aterm_codec::base64::encode(&body.key_packet(&key)?)
                    .map_err(|_| "Could not encode recipient encryption key")?;
                addresses.insert(
                    address.to_owned(),
                    json!({"Type":kind,"Signature":0,"BodyKeyPacket":packet}),
                );
            } else if recipient_type == 2 {
                types |= 4;
                clear = true;
                addresses.insert(address.to_owned(), json!({"Type":4,"Signature":0}));
            } else {
                return Err("Internal recipient has no usable encryption key".into());
            }
        }
        let list = |values: &[String]| {
            values
                .iter()
                .map(|email| json!({"Address":email,"Name":""}))
                .collect::<Vec<_>>()
        };
        let template = json!({"Subject":draft.subject,"Sender":{"Address":draft.from,"Name":""},"ToList":list(&draft.to),"CCList":list(&draft.cc),"BCCList":list(&draft.bcc),"MIMEType":"text/plain","Body":body.armored(&sender)?,"Unread":0});
        let created = self.call(
            Method::Post,
            "/mail/v4/messages",
            Some(json!({"Message":template,"AttachmentKeyPackets":[],"Action":0})),
        )?;
        let id = field(&created["Message"], "ID")?;
        let encoded =
            aterm_codec::base64::encode(&body.data).map_err(|_| "Could not encode message")?;
        let mut package = json!({"Addresses":Value::Object(addresses),"MIMEType":"text/plain","Type":types,"Body":encoded});
        if clear && let Value::Object(fields) = &mut package {
            fields.insert(
                "BodyKey".into(),
                json!({"Key":body.key_base64()?,"Algorithm":"aes256"}),
            );
        }
        self.call(
            Method::Post,
            &format!("/mail/v4/messages/{}", crate::oauth::encode(id)),
            Some(json!({"Packages":[package]})),
        )
        .map_err(|error| {
            if self
                .last_failure()
                .is_some_and(|failure| failure.phase == Some(aterm_http::Phase::NotSent))
            {
                format!("Message was not submitted. {error}")
            } else {
                format!("Send was not confirmed. Check Sent and Drafts before retrying. {error}")
            }
        })?;
        Ok(())
    }
}
