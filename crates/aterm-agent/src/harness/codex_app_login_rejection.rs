// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Evidence from the actual, serialized authorization-code POST. Classification
//! of arbitrary bytes is not producer authority; the constructor stays private
//! to the native exchange module.

use super::{AuthLock, StagedWitness};

pub(in crate::harness) const MAX_REJECTION_BODY: usize = 4096;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OAuthError {
    error: String,
    #[serde(default)]
    error_description: String,
    #[serde(default)]
    error_uri: String,
}

pub(in crate::harness) fn is_definitive_code_rejection(status: u16, body: &[u8]) -> bool {
    if status != 400 || body.len() > MAX_REJECTION_BODY {
        return false;
    }
    let Ok(error) = aterm_json::from_slice::<OAuthError>(body) else {
        return false;
    };
    error.error == "invalid_grant"
        && error.error_description.len() <= 1024
        && !error.error_description.chars().any(char::is_control)
        && error.error_uri.len() <= 2048
        && !error.error_uri.chars().any(char::is_control)
}

/// Non-cloneable and not deserializable. Only the actual code exchange can
/// attach the live AuthLock and its opened stage to a received HTTP response.
pub(in crate::harness) struct CodeRejection<'a> {
    status: u16,
    body: &'a [u8],
    witness: StagedWitness,
    _auth: &'a AuthLock,
}
impl<'a> CodeRejection<'a> {
    pub(super) fn mint(
        status: u16,
        body: &'a [u8],
        witness: StagedWitness,
        auth: &'a AuthLock,
    ) -> Option<Self> {
        is_definitive_code_rejection(status, body).then_some(Self {
            status,
            body,
            witness,
            _auth: auth,
        })
    }
    pub(in crate::harness) fn consume<R>(
        self,
        retain: impl FnOnce(u16, &[u8], &StagedWitness) -> R,
    ) -> R {
        retain(self.status, self.body, &self.witness)
    }
}
