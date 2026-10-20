// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TEST ONLY: what a test that needs the platform verifier to ANSWER uses to
//! find out whether it will.
//!
//! Declared by `super::tests` as `verifier::test_platform`; its parent module
//! has `PlatformVerifier` and `is_service_unanswered` in scope, which is all it
//! names.
//!
//! [`require_trust_service`] is the workspace's one probe and one COULD NOT RUN
//! (`aterm_uds::deadline`, a debug build's: a code check, through
//! the same gate the shipped verifier evaluates under) PLUS, when that one
//! answers, the exact call these tests depend on — one shipped certificate
//! evaluation — because the two `trustd` round trips can diverge
//! (`SecTrustEvaluateWithError` has been seen to hang while `security
//! verify-cert` still answered). Each is asked once per process; a silent
//! service costs the first ask its deadline, and every shipped evaluation
//! after it is refused at once.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use rustls::client::danger::ServerCertVerifier;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};

use super::{PlatformVerifier, is_service_unanswered};

/// End the calling test COULD NOT RUN when macOS's `trustd` does not answer a
/// code check OR the shipped certificate evaluation.
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

/// Whether the shipped verifier's evaluation of one chain went unanswered —
/// asked once per process. Any chain that reaches the evaluation will do; the
/// verdict is not the question, only whether there is one.
fn shipped_evaluation_unanswered() -> bool {
    static SILENT: OnceLock<bool> = OnceLock::new();
    *SILENT.get_or_init(|| {
        let provider = Arc::new(crate::tls::provider());
        let Ok(verifier) = PlatformVerifier::new(provider) else {
            // No platform verifier at all: the callers' own skips say so.
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
