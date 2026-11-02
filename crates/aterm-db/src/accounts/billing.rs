// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Root-pinned billing configuration. Proposal references grant nothing until
//! captured through a current root account decision. Version one caps request
//! count only; it supplies neither provider pricing nor native launch permission.

use crate::{Error, Result};
use astream_wire::sync::Digest;
use aterm_json::{Value, json};

#[cfg(unix)]
mod policy;
#[cfg(unix)]
pub use policy::{Policy, Resource};
#[cfg(unix)]
mod observation;
#[cfg(unix)]
pub use observation::{Billing, Dispatch, ENTRY, Prepared};
#[cfg(unix)]
pub(crate) mod command;

/// Proposal metadata identifying exact billing resource terms. Its authority
/// comes from the enclosing current root account decision, never this parser.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reference {
    pub principal: Digest,
    pub policy: Digest,
}

impl Reference {
    pub fn parse(value: &Value) -> Result<Self> {
        let object = value
            .as_object()
            .filter(|object| object.len() == 2)
            .ok_or_else(|| Error::new("billing reference needs principal and policy"))?;
        let digest = |key| {
            let text = object
                .get(key)
                .and_then(Value::as_str)
                .ok_or_else(|| Error::new("billing reference identity is missing"))?;
            let value = crate::database::decode_hex::<32>(text)?;
            if value == [0; 32] {
                return Err(Error::new("billing reference identity is zero"));
            }
            Ok(value)
        };
        Ok(Self {
            principal: digest("principal")?,
            policy: digest("policy")?,
        })
    }

    pub fn value(self) -> Value {
        json!({
            "principal": aterm_codec::hex::encode_lower(&self.principal),
            "policy": aterm_codec::hex::encode_lower(&self.policy)
        })
    }
}
