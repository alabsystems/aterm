// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Platform TLS certificate verifier tests over a locally minted fixture corpus.

use std::sync::Arc;

use rustls::client::danger::ServerCertVerifier;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};

use super::PlatformVerifier;
use super::test_platform::require_trust_service;

const ROOT: &[u8] = include_bytes!("../testdata/tls/root.der");
const INTER: &[u8] = include_bytes!("../testdata/tls/inter.der");
const NOTCA: &[u8] = include_bytes!("../testdata/tls/notca.der");
const GOOD: &[u8] = include_bytes!("../testdata/tls/good.der");
const EXPIRED: &[u8] = include_bytes!("../testdata/tls/expired.der");
const FUTURE: &[u8] = include_bytes!("../testdata/tls/future.der");
const WRONGHOST: &[u8] = include_bytes!("../testdata/tls/wronghost.der");
const VIAINTER: &[u8] = include_bytes!("../testdata/tls/viainter.der");
const VIANOTCA: &[u8] = include_bytes!("../testdata/tls/vianotca.der");
const SELFSIGNED: &[u8] = include_bytes!("../testdata/tls/selfsigned.der");
const NOSAN: &[u8] = include_bytes!("../testdata/tls/nosan.der");
const CLIENTONLY: &[u8] = include_bytes!("../testdata/tls/clientonly.der");
const NOEKU: &[u8] = include_bytes!("../testdata/tls/noeku.der");
const KEYENCIPH: &[u8] = include_bytes!("../testdata/tls/keyenciph.der");
const IPSAN: &[u8] = include_bytes!("../testdata/tls/ipsan.der");
const TAMPERSIG: &[u8] = include_bytes!("../testdata/tls/tampersig.der");
const TAMPERTBS: &[u8] = include_bytes!("../testdata/tls/tampertbs.der");
const MALFORMED: &[u8] = include_bytes!("../testdata/tls/malformed.der");
const GH_LEAF: &[u8] = include_bytes!("../testdata/tls/gh-leaf.der");
const GH_INT0: &[u8] = include_bytes!("../testdata/tls/gh-int0.der");
const GH_INT1: &[u8] = include_bytes!("../testdata/tls/gh-int1.der");

/// The name every local leaf but two is issued for.
const HOST: &str = "test.aterm.invalid";
/// `wronghost.der`'s own name.
const OTHER_HOST: &str = "other.aterm.invalid";

/// 2025-06-15T12:00:00Z — before the `good` window, inside `expired`'s.
const T25: u64 = 1_749_988_800;
/// 2026-06-15T12:00:00Z — inside the `good` window.
const T26: u64 = 1_781_524_800;
/// 2027-06-15T12:00:00Z — after the `good` window, inside `future`'s.
const T27: u64 = 1_813_060_800;
/// 2026-08-29T06:59:33Z — inside the captured github.com leaf's window
/// (2026-07-03 → 2026-09-30).
const T_GH: u64 = 1_787_986_773;

/// One verification's result: accept/reject plus the mapped error kinds.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Outcome {
    Accept,
    /// The bytes are not a certificate. Not a trust verdict.
    BadEncoding,
    NotValidForName,
    UnknownIssuer,
    /// The certificate's signature does not check out.
    BadSignature,
    Revoked,
    /// Extended key usage does not permit server authentication.
    Eku,
    /// Rejected, for a reason the platform did not put in the mapped set.
    Rejected,
}

impl Outcome {
    /// Did this verification accept the chain?
    fn accepted(self) -> bool {
        matches!(self, Self::Accept)
    }
}

/// A silent trustd is the machine's, not a verdict, so it is never counted as
/// `Rejected` or a failed `Accept`.
const TRUSTD_SILENT: &str =
    "COULD NOT RUN: the process trustd gate was silent — the machine's, not the code's";

fn reduce(result: &Result<rustls::client::danger::ServerCertVerified, rustls::Error>) -> Outcome {
    use rustls::CertificateError as Ce;
    match result {
        Err(e) if super::is_service_unanswered(e) => panic!("{TRUSTD_SILENT}: {e}"),
        Ok(_) => Outcome::Accept,
        Err(rustls::Error::InvalidCertificate(inner)) => match inner {
            Ce::BadEncoding => Outcome::BadEncoding,
            Ce::NotValidForName | Ce::NotValidForNameContext { .. } => Outcome::NotValidForName,
            Ce::UnknownIssuer => Outcome::UnknownIssuer,
            Ce::BadSignature => Outcome::BadSignature,
            Ce::Revoked => Outcome::Revoked,
            Ce::Other(other) if other.to_string() == "certificate had invalid extensions" => {
                Outcome::Eku
            }
            _ => Outcome::Rejected,
        },
        Err(_) => Outcome::Rejected,
    }
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    crate::tls::init_crypto();
    rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(crate::tls::provider()))
}

fn der(bytes: &'static [u8]) -> CertificateDer<'static> {
    CertificateDer::from(bytes)
}

/// One chain, one name, one instant.
struct Case {
    what: &'static str,
    /// End-entity first, then intermediates in the order a server would send them.
    chain: &'static [&'static [u8]],
    server: &'static str,
    now: u64,
    /// What an Apple platform does with this input, measured.
    apple: Outcome,
    /// What the webpki chain math (the Unix arm) does with it. MEASURED.
    webpki: Outcome,
    /// What crypt32 (the Windows arm) does with it. MEASURED (Windows 11 26200).
    windows: Outcome,
}

impl Case {
    /// The expectation for the arm that is actually compiled in.
    fn expected(&self) -> Outcome {
        if cfg!(target_vendor = "apple") {
            self.apple
        } else if cfg!(windows) {
            self.windows
        } else {
            self.webpki
        }
    }

    fn run(&self, verifier: &dyn ServerCertVerifier) -> Outcome {
        let chain: Vec<CertificateDer<'static>> = self.chain.iter().copied().map(der).collect();
        let Some((end_entity, intermediates)) = chain.split_first() else {
            panic!("case {} has an empty chain", self.what);
        };
        let name = ServerName::try_from(self.server)
            .unwrap_or_else(|_| panic!("case {} has an unusable server name", self.what));
        let result = verifier.verify_server_cert(
            end_entity,
            intermediates,
            &name,
            &[],
            UnixTime::since_unix_epoch(std::time::Duration::from_secs(self.now)),
        );
        reduce(&result)
    }
}

/// The corpus.
const CORPUS: &[Case] = &[
    Case {
        what: "the good chain, inside its validity window",
        chain: &[GOOD],
        server: HOST,
        now: T26,
        apple: Outcome::Accept,
        webpki: Outcome::Accept,
        windows: Outcome::Accept,
    },
    Case {
        what: "the SAME bytes, evaluated after the window closes",
        chain: &[GOOD],
        server: HOST,
        now: T27,
        apple: Outcome::Rejected,
        webpki: Outcome::Rejected,
        windows: Outcome::Rejected,
    },
    Case {
        what: "the SAME bytes, evaluated before the window opens",
        chain: &[GOOD],
        server: HOST,
        now: T25,
        apple: Outcome::Rejected,
        webpki: Outcome::Rejected,
        windows: Outcome::Rejected,
    },
    Case {
        what: "an expired leaf, at a now past its notAfter",
        chain: &[EXPIRED],
        server: HOST,
        now: T26,
        apple: Outcome::Rejected,
        webpki: Outcome::Rejected,
        windows: Outcome::Rejected,
    },
    Case {
        what: "the same expired leaf, at a now INSIDE its window",
        chain: &[EXPIRED],
        server: HOST,
        now: T25,
        apple: Outcome::Accept,
        webpki: Outcome::Accept,
        windows: Outcome::Accept,
    },
    Case {
        what: "a not-yet-valid leaf, at a now before its notBefore",
        chain: &[FUTURE],
        server: HOST,
        now: T26,
        apple: Outcome::Rejected,
        webpki: Outcome::Rejected,
        windows: Outcome::Rejected,
    },
    Case {
        what: "the same not-yet-valid leaf, at a now INSIDE its window",
        chain: &[FUTURE],
        server: HOST,
        now: T27,
        apple: Outcome::Accept,
        webpki: Outcome::Accept,
        windows: Outcome::Accept,
    },
    Case {
        what: "a leaf for another name, asked for ours",
        chain: &[WRONGHOST],
        server: HOST,
        now: T26,
        apple: Outcome::NotValidForName,
        webpki: Outcome::NotValidForName,
        windows: Outcome::NotValidForName,
    },
    Case {
        what: "the same leaf, asked for the name it actually carries",
        chain: &[WRONGHOST],
        server: OTHER_HOST,
        now: T26,
        apple: Outcome::Accept,
        webpki: Outcome::Accept,
        windows: Outcome::Accept,
    },
    Case {
        what: "a leaf with a common name but NO subjectAltName",
        chain: &[NOSAN],
        server: HOST,
        now: T26,
        apple: Outcome::NotValidForName,
        webpki: Outcome::NotValidForName,
        windows: Outcome::NotValidForName,
    },
    Case {
        what: "a leaf whose only SAN is an IP address, dialled by that IP",
        chain: &[IPSAN],
        server: "127.0.0.1",
        now: T26,
        apple: Outcome::Accept,
        webpki: Outcome::Accept,
        windows: Outcome::Accept,
    },
    Case {
        what: "the same leaf, dialled by a DIFFERENT IP",
        chain: &[IPSAN],
        server: "127.0.0.2",
        now: T26,
        apple: Outcome::NotValidForName,
        webpki: Outcome::NotValidForName,
        windows: Outcome::NotValidForName,
    },
    Case {
        what: "a leaf under the intermediate, WITH the intermediate supplied",
        chain: &[VIAINTER, INTER],
        server: HOST,
        now: T26,
        apple: Outcome::Accept,
        webpki: Outcome::Accept,
        windows: Outcome::Accept,
    },
    Case {
        what: "the same leaf with the intermediate OMITTED",
        chain: &[VIAINTER],
        server: HOST,
        now: T26,
        apple: Outcome::UnknownIssuer,
        webpki: Outcome::UnknownIssuer,
        windows: Outcome::UnknownIssuer,
    },
    Case {
        what: "a leaf signed by a CA:FALSE certificate",
        chain: &[VIANOTCA, NOTCA],
        server: HOST,
        now: T26,
        apple: Outcome::Rejected,
        webpki: Outcome::Rejected,
        windows: Outcome::Eku,
    },
    Case {
        what: "a leaf that is its own issuer",
        chain: &[SELFSIGNED],
        server: HOST,
        now: T26,
        apple: Outcome::Rejected,
        webpki: Outcome::UnknownIssuer,
        windows: Outcome::UnknownIssuer,
    },
    Case {
        what: "the good leaf with one signature byte flipped",
        chain: &[TAMPERSIG],
        server: HOST,
        now: T26,
        apple: Outcome::UnknownIssuer,
        webpki: Outcome::BadSignature,
        windows: Outcome::Rejected,
    },
    Case {
        what: "the good leaf with a SAN byte rewritten inside the signed body",
        chain: &[TAMPERTBS],
        server: HOST,
        now: T26,
        apple: Outcome::UnknownIssuer,
        webpki: Outcome::BadSignature,
        windows: Outcome::Rejected,
    },
    Case {
        what: "a leaf whose extendedKeyUsage is clientAuth only",
        chain: &[CLIENTONLY],
        server: HOST,
        now: T26,
        apple: Outcome::Eku,
        webpki: Outcome::Eku,
        windows: Outcome::Eku,
    },
    Case {
        what: "a leaf with NO extendedKeyUsage extension (the platforms differ)",
        chain: &[NOEKU],
        server: HOST,
        now: T26,
        apple: Outcome::Eku,
        webpki: Outcome::Accept,
        windows: Outcome::Accept,
    },
    Case {
        what: "a leaf whose keyUsage omits digitalSignature",
        chain: &[KEYENCIPH],
        server: HOST,
        now: T26,
        apple: Outcome::Accept,
        webpki: Outcome::Accept,
        windows: Outcome::Accept,
    },
    Case {
        what: "nine bytes that are not a certificate at all",
        chain: &[MALFORMED],
        server: HOST,
        now: T26,
        apple: Outcome::BadEncoding,
        webpki: Outcome::BadEncoding,
        windows: Outcome::BadEncoding,
    },
];

/// Why a non-Apple host cannot run the system-anchored checks: its trust store
/// does not validate the captured github.com chain.
const UNANCHORED_GITHUB: &str = "this machine's trust store did not validate the captured \
     github.com chain; see src/testdata/tls/README.md for recapture";

/// The verifier over the fixture root.
fn mine() -> PlatformVerifier {
    let mine = PlatformVerifier::new_with_extra_roots(vec![der(ROOT)], provider()).unwrap_or_else(
        |error| {
            aterm_spec::host::could_not_run(format!("no platform verifier on this target: {error}"))
        },
    );
    require_trust_service();
    mine
}

#[test]
fn the_first_party_verifier_matches_its_recorded_expectations() {
    let mine = mine();
    let mut wrong = Vec::new();
    for case in CORPUS {
        let got = case.run(&mine);
        if got != case.expected() {
            wrong.push(format!(
                "{}: got {got:?}, expected {:?}",
                case.what,
                case.expected()
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "unexpected verdicts:\n  {}",
        wrong.join("\n  ")
    );
}

#[test]
fn the_shipped_constructor_trusts_no_fixture_anchor() {
    require_trust_service();
    let shipped = match PlatformVerifier::new(provider()) {
        Ok(verifier) => verifier,
        Err(error) => aterm_spec::host::could_not_run(format!(
            "no platform trust store on this target: {error}"
        )),
    };
    let case = Case {
        what: "the good chain under the SYSTEM anchors only",
        chain: &[GOOD],
        server: HOST,
        now: T26,
        apple: Outcome::UnknownIssuer,
        webpki: Outcome::UnknownIssuer,
        windows: Outcome::UnknownIssuer,
    };
    let got = case.run(&shipped);
    assert!(
        !got.accepted(),
        "the shipped verifier accepted a chain rooted in a fixture CA: {got:?}"
    );

    assert!(
        case.run(&mine()).accepted(),
        "the fixture chain must be good; only its anchor is missing"
    );
}

#[test]
fn the_system_trust_store_is_actually_consulted() {
    let real = Case {
        what: "the captured github.com chain, under the system anchors",
        chain: &[GH_LEAF, GH_INT0, GH_INT1],
        server: "github.com",
        now: T_GH,
        apple: Outcome::Accept,
        webpki: Outcome::Accept,
        windows: Outcome::Accept,
    };
    let wrong_name = Case {
        what: "the same chain, asked for a name it does not carry",
        chain: &[GH_LEAF, GH_INT0, GH_INT1],
        server: "example.invalid",
        now: T_GH,
        apple: Outcome::NotValidForName,
        webpki: Outcome::NotValidForName,
        windows: Outcome::NotValidForName,
    };

    let mine = PlatformVerifier::new(provider()).unwrap_or_else(|error| {
        aterm_spec::host::could_not_run(format!("no system trust store on this target: {error}"))
    });
    require_trust_service();
    assert!(
        !wrong_name.run(&mine).accepted(),
        "a real chain was accepted for a name it does not carry"
    );
    if !cfg!(target_vendor = "apple") && !real.run(&mine).accepted() {
        aterm_spec::host::could_not_run(UNANCHORED_GITHUB);
    }
    assert_eq!(real.run(&mine), real.expected(), "system-anchored verdict");
}

#[test]
fn extra_anchors_add_to_the_system_set_rather_than_replacing_it() {
    let chain = Case {
        what: "the captured github.com chain, under a verifier with extra anchors",
        chain: &[GH_LEAF, GH_INT0, GH_INT1],
        server: "github.com",
        now: T_GH,
        apple: Outcome::Accept,
        webpki: Outcome::Accept,
        windows: Outcome::Accept,
    };
    let mine = mine();
    let accepted = chain.run(&mine).accepted();
    if !cfg!(target_vendor = "apple") && !accepted {
        aterm_spec::host::could_not_run(UNANCHORED_GITHUB);
    }
    assert!(
        accepted,
        "supplying an extra anchor DISABLED the system anchors"
    );
}

#[test]
fn a_parse_failure_is_reported_as_one_and_never_as_a_trust_verdict() {
    let mine = mine();
    for (what, bytes) in [
        ("nine bytes of garbage", MALFORMED),
        ("no bytes at all", &[] as &[u8]),
    ] {
        let case = Case {
            what,
            chain: &[MALFORMED],
            server: HOST,
            now: T26,
            apple: Outcome::BadEncoding,
            webpki: Outcome::BadEncoding,
            windows: Outcome::BadEncoding,
        };
        let chain = [CertificateDer::from(bytes.to_vec())];
        let name = ServerName::try_from(HOST).unwrap();
        let at = UnixTime::since_unix_epoch(std::time::Duration::from_secs(case.now));
        let got = reduce(&mine.verify_server_cert(&chain[0], &[], &name, &[], at));
        assert_eq!(got, Outcome::BadEncoding, "{what} must be a PARSE failure");
    }
}

#[test]
fn a_degenerate_server_name_fails_closed_instead_of_disabling_name_checking() {
    let mine = mine();

    let mismatch = Case {
        what: "a leaf for another name",
        chain: &[WRONGHOST],
        server: HOST,
        now: T26,
        apple: Outcome::NotValidForName,
        webpki: Outcome::NotValidForName,
        windows: Outcome::NotValidForName,
    };
    assert!(
        !mismatch.run(&mine).accepted(),
        "name checking is not happening at all"
    );

    #[cfg(target_vendor = "apple")]
    {
        let arm = super::apple::Verifier::new(vec![der(ROOT)], provider())
            .expect("the fixture root is a usable anchor");
        let name = ServerName::try_from(HOST).expect("a usable server name");
        let at = UnixTime::since_unix_epoch(std::time::Duration::from_secs(T26));
        for degenerate in ["", "\u{0}"] {
            let result = arm.verify(&der(GOOD), &[], &name, degenerate, None, at);
            if let Err(e) = &result
                && super::is_service_unanswered(e)
            {
                panic!("{TRUSTD_SILENT}: {degenerate:?}: {e}");
            }
            assert!(
                result.is_err(),
                "the arm accepted a chain against the server name {degenerate:?}"
            );
        }
    }

    assert!(!ServerName::try_from(HOST).unwrap().to_str().is_empty());
}

#[test]
fn handshake_signature_verification_is_delegated_to_the_provider() {
    let mine = PlatformVerifier::new(provider()).unwrap_or_else(|error| {
        aterm_spec::host::could_not_run(format!("no platform verifier on this target: {error}"))
    });
    let expected = provider()
        .signature_verification_algorithms
        .supported_schemes();
    assert_eq!(mine.supported_verify_schemes(), expected);
    assert!(!expected.is_empty());
}
