// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Targets with no operating-system trust store (wasm32, Android, anything that
//! is neither Windows nor a non-Android Unix): construction fails.
//!
//! aterm builds for none of them; this arm keeps the `#[cfg]` set total. It
//! refuses rather than fall back to a bundled root set (as
//! `rustls-platform-verifier`'s wasm32 arm does), and in the constructor, so a
//! caller finds out when it builds its `ClientConfig`, not mid-handshake.

use std::sync::Arc;

use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};

/// A verifier that is never constructed: `new` always fails.
#[derive(Debug)]
pub(super) struct Verifier;

impl Verifier {
    pub(super) fn new(
        _extra_roots: Vec<CertificateDer<'static>>,
        _provider: Arc<CryptoProvider>,
    ) -> Result<Self, rustls::Error> {
        Err(rustls::Error::General(
            "this target has no operating-system certificate trust store, and aterm does not \
             bundle a root set"
                .to_owned(),
        ))
    }

    pub(super) fn verify(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _server_text: &str,
        _ocsp_response: Option<&[u8]>,
        _now: UnixTime,
    ) -> Result<(), rustls::Error> {
        // Unreachable (`new` never yields a value), but fails closed regardless.
        Err(rustls::Error::General(
            "no certificate verification is available on this target".to_owned(),
        ))
    }
}
