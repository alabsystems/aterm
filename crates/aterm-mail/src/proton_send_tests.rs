// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

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
    let (mut proton, calls) = mailbox(vec![Ok(json!({"Code":1000,"Message":{"ID":"new-draft"}}))]);
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
