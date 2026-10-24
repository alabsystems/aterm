// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Stub certificate verifier for targets with no operating-system trust store.

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
        Err(rustls::Error::General(
            "no certificate verification is available on this target".to_owned(),
        ))
    }
}
