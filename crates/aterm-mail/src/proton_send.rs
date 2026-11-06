// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! Prepare encrypted Proton messages and submit exactly one delivery attempt.
use super::{Proton, field};
use aterm_http::Method;
use aterm_json::{Value, json};

pub struct PreparedMessage {
    payload: Value,
}

impl Proton {
    fn encrypted_body(
        &self,
        draft: &crate::Draft,
    ) -> Result<
        (
            crate::crypto::pgp::EncryptedBody,
            crate::crypto::pgp::Recipient,
        ),
        String,
    > {
        // Use the same bounded address and header validation as SMTP.
        crate::smtp::validate_draft(draft)?;
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
        Ok((body, sender))
    }

    fn draft_template(draft: &crate::Draft, body: String) -> Value {
        let list = |values: &[String]| {
            values
                .iter()
                .map(|email| json!({"Address":email,"Name":""}))
                .collect::<Vec<_>>()
        };
        json!({"Subject":draft.subject,"Sender":{"Address":draft.from,"Name":""},"ToList":list(&draft.to),"CCList":list(&draft.cc),"BCCList":list(&draft.bcc),"MIMEType":"text/plain","Body":body,"Flags":0,"Attachments":[]})
    }

    /// Save encrypted content without delivering it.
    pub fn save_draft(&mut self, draft: &crate::Draft) -> Result<String, String> {
        let (body, sender) = self.encrypted_body(draft)?;
        let template = Self::draft_template(draft, body.armored(&sender)?);
        let response = self.call(
            Method::Post,
            "/mail/v4/messages",
            Some(json!({"Message":template,"AttachmentKeyPackets":{}})),
        )?;
        self.cache.clear();
        Ok(field(&response["Message"], "ID")?.into())
    }

    /// Delete only a message the provider still identifies as a draft.
    pub fn discard_draft(&mut self, id: &str) -> Result<(), String> {
        let response = self.call(
            Method::Get,
            &format!("/mail/v4/messages/{}", crate::oauth::encode(id)),
            None,
        )?;
        if !response["Message"]["LabelIDs"]
            .as_array()
            .is_some_and(|labels| labels.iter().any(|label| label.as_str() == Some("1")))
        {
            return Err("Refusing to discard a message that is not a draft".into());
        }
        self.call(
            Method::Put,
            "/mail/v4/messages/delete",
            Some(json!({"IDs":[id],"CurrentLabelID":"1"})),
        )?;
        self.cache.clear();
        Ok(())
    }

    /// Perform recipient lookup and encryption without delivering a message.
    pub fn prepare_send(&mut self, draft: &crate::Draft) -> Result<PreparedMessage, String> {
        let (body, sender) = self.encrypted_body(draft)?;
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
        let template = Self::draft_template(draft, body.armored(&sender)?);
        let encoded =
            aterm_codec::base64::encode(&body.data).map_err(|_| "Could not encode message")?;
        let mut package = json!({"Addresses":Value::Object(addresses),"MIMEType":"text/plain","Type":types,"Body":encoded});
        if clear && let Value::Object(fields) = &mut package {
            fields.insert(
                "BodyKey".into(),
                json!({"Key":body.key_base64()?,"Algorithm":"aes256"}),
            );
        }
        Ok(PreparedMessage {
            payload: json!({"Message":template,"Packages":[package],"AttachmentKeys":{},"AutoSaveContacts":0}),
        })
    }

    /// A prepared message is consumed even if its delivery outcome is uncertain.
    pub fn send_prepared(&mut self, prepared: PreparedMessage) -> Result<(), String> {
        self.call(
            Method::Post,
            "/mail/v4/messages/send/direct",
            Some(prepared.payload),
        )
        .map_err(|error| {
            format!("Delivery was not confirmed; check Sent before retrying. {error}")
        })?;
        self.cache.clear();
        Ok(())
    }

    pub fn send(&mut self, draft: &crate::Draft) -> Result<(), String> {
        let prepared = self.prepare_send(draft)?;
        self.send_prepared(prepared)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proton::{ApiError, ApiResponse, ApiTransport};
    use std::sync::{Arc, Mutex};

    type Calls = Arc<Mutex<Vec<(Method, String, Option<Value>)>>>;
    struct MockApi {
        calls: Calls,
        responses: std::collections::VecDeque<Result<Value, String>>,
    }
    impl ApiTransport for MockApi {
        fn request(
            &mut self,
            method: Method,
            path: &str,
            body: Option<Value>,
        ) -> Result<ApiResponse, ApiError> {
            self.calls.lock().unwrap().push((method, path.into(), body));
            let body = self.responses.pop_front().expect("unexpected retry")?;
            Ok(ApiResponse {
                status: 200,
                retry_after: None,
                body: aterm_json::to_vec(&body).unwrap(),
            })
        }
        fn renew(&mut self) -> Result<(), ApiError> {
            panic!("a send must not silently renew and repeat")
        }
    }

    fn mailbox(responses: Vec<Result<Value, String>>) -> (Proton, Calls) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut proton = Proton::with_transport(Box::new(MockApi {
            calls: calls.clone(),
            responses: responses.into(),
        }));
        proton.keys =
            crate::crypto::pgp::unlock(include_str!("../tests/fixtures/proton-key-v4.asc"), b"")
                .unwrap();
        proton.send_keys.push(("sender@example.test".into(), 0));
        (proton, calls)
    }

    fn draft() -> crate::Draft {
        crate::Draft {
            from: "sender@example.test".into(),
            to: vec!["receiver@example.test".into()],
            subject: "A subject".into(),
            text: "A private body".into(),
            ..Default::default()
        }
    }

    #[test]
    fn direct_send_encrypts_saved_body_and_never_replays_uncertain_delivery() {
        let (mut proton, calls) = mailbox(vec![
            Ok(json!({"Code":1000,"RecipientType":2,"Keys":[]})),
            Err("connection lost".into()),
        ]);
        let error = proton.send(&draft()).unwrap_err();
        assert!(error.contains("not confirmed"));
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            (calls[1].0, calls[1].1.as_str()),
            (Method::Post, "/mail/v4/messages/send/direct")
        );
        let payload = calls[1].2.as_ref().unwrap();
        let decrypted =
            crate::crypto::pgp::decrypt(payload["Message"]["Body"].as_str().unwrap(), &proton.keys)
                .unwrap();
        assert_eq!(decrypted.as_slice(), b"A private body");
        assert_eq!(
            payload["Packages"][0]["Addresses"]["receiver@example.test"]["Type"].as_u64(),
            Some(4)
        );
        assert!(payload["Packages"][0]["BodyKey"]["Key"].as_str().is_some());
    }

    #[test]
    fn new_draft_does_not_claim_to_be_a_reply() {
        let (mut proton, calls) =
            mailbox(vec![Ok(json!({"Code":1000,"Message":{"ID":"new-draft"}}))]);
        assert_eq!(proton.save_draft(&draft()).unwrap(), "new-draft");
        let calls = calls.lock().unwrap();
        let payload = calls[0].2.as_ref().unwrap();
        assert!(payload["Action"].is_null());
        assert!(
            payload["AttachmentKeyPackets"]
                .as_object()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn discard_refuses_a_delivered_message() {
        let (mut proton, calls) = mailbox(vec![Ok(
            json!({"Code":1000,"Message":{"ID":"sent","LabelIDs":["2"]}}),
        )]);
        assert!(proton.discard_draft("sent").is_err());
        assert_eq!(calls.lock().unwrap().len(), 1);
    }
}
