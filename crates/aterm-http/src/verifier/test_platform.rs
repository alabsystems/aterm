// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test helper probing whether the macOS `trustd` trust service responds to code and certificate checks.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use rustls::client::danger::ServerCertVerifier;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};

use super::{PlatformVerifier, is_service_unanswered};

/// Ends the calling test with `COULD NOT RUN` when macOS `trustd` does not answer a code check or certificate evaluation.
#[track_caller]
pub(crate) fn require_trust_service() {
    aterm_uds::deadline::require_trust_service();
    if shipped_evaluation_unanswered() {
        aterm_spec::host::could_not_run(
            "macOS's trust service (trustd) did not answer the shipped certificate \
             evaluation, so no TLS check ran here; rebooting the Mac clears a wedged trustd",
        );
    }
}

fn shipped_evaluation_unanswered() -> bool {
    static SILENT: OnceLock<bool> = OnceLock::new();
    *SILENT.get_or_init(|| {
        let provider = Arc::new(crate::tls::provider());
        let Ok(verifier) = PlatformVerifier::new(provider) else {
            return false;
        };
        let leaf = CertificateDer::from(&include_bytes!("../testdata/tls/good.der")[..]);
        let name = ServerName::try_from("test.aterm.invalid").expect("a usable server name");
        let at = UnixTime::since_unix_epoch(Duration::from_secs(1_781_524_800));
        match verifier.verify_server_cert(&leaf, &[], &name, &[], at) {
            Err(error) => is_service_unanswered(&error),
            Ok(_) => false,
        }
    })
}
