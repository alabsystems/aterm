// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Targets with no operating-system trust store: construction fails.
//!
//! wasm32, Android, and anything that is neither Windows nor a non-Android
//! Unix. aterm builds for none of them, so this arm never runs — but it is the
//! arm that keeps the `#[cfg]` set total, and what it does when it does not
//! exist matters.
//!
//! `rustls-platform-verifier`'s wasm32 arm loads
//! `webpki_root_certs::TLS_SERVER_ROOT_CERTS` — a root set BUNDLED into the
//! binary. That silently contradicts the trust model [`crate::tls`] documents
//! ("the OS trust store, never a root set compiled into the aterm binary"), and
//! it does so on a platform where the doc comment says otherwise. Since nothing
//! is built for wasm32, the divergence was on paper rather than in a shipped
//! artefact; it is closed here by refusing to produce a verifier at all, which
//! is the fail-closed answer and also drops `webpki-root-certs`
//! (2 packages / 3,357 lines) from that cell.
//!
//! Failing in the CONSTRUCTOR rather than in `verify` is deliberate: a caller on
//! such a target finds out when it builds its `ClientConfig`, not part-way
//! through a handshake.

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
        // Unreachable today, because `new` never yields a value. Written as a
        // rejection anyway so that a future change which DID make this type
        // constructible would still fail closed.
        Err(rustls::Error::General(
            "no certificate verification is available on this target".to_owned(),
        ))
    }
}
