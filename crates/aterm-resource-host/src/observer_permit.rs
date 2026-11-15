// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Custodian-lifetime read-only authority explicitly delegated by an enrolled GUI to its
//! successor. Possession permits observations only; it never authorizes an execution request.

use aterm_uds::secret::Secret;
use std::sync::{Arc, Mutex};

static PERMIT: Mutex<Option<Arc<Secret>>> = Mutex::new(None);

pub(crate) fn issue() -> std::io::Result<Arc<Secret>> {
    let mut permit = PERMIT.lock().unwrap_or_else(|p| p.into_inner());
    if permit.is_none() {
        *permit = Some(Arc::new(Secret::from_vec(
            aterm_uds::rand::hex_token::<32>()?.into_bytes(),
        )));
    }
    Ok(permit.as_ref().unwrap().clone())
}

pub(crate) fn validate(offered: &[u8]) -> std::io::Result<()> {
    let permit = PERMIT.lock().unwrap_or_else(|p| p.into_inner());
    let valid = permit.as_ref().is_some_and(|permit| {
        offered.len() == 64
            && permit.len() == offered.len()
            && offered
                .iter()
                .zip(permit.expose())
                .fold(0_u8, |different, (a, b)| different | (a ^ b))
                == 0
    });
    if valid {
        Ok(())
    } else {
        Err(std::io::ErrorKind::PermissionDenied.into())
    }
}

#[cfg(test)]
#[path = "observer_permit_tests.rs"]
mod tests;
