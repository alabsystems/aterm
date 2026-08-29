// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The differential oracle: `rustls-platform-verifier` as the reference.
//!
//! The retired crate is kept as a `[dev-dependencies]` oracle (see this crate's
//! manifest) and every chain below is driven through BOTH implementations, with
//! the same anchors and the same pinned instant, and their verdicts compared.
//! This is the same discipline
//! `crates/aterm-gui/src/net_connections/keychain.rs:472` uses at the same OS
//! boundary, and the same skip rule: if the ORACLE fails first, the test skips
//! rather than reporting a defect in the reimplementation.
//!
//! # What this proves, and — read this part — what it does not
//!
//! On macOS both implementations end up inside the SAME `SecTrust` engine. So
//! this is a PLUMBING differential, not an independent PKI oracle. It pins the
//! things a reimplementation actually gets wrong — policy construction, the
//! verify date, the anchor set, certificate ORDER in the array, the server-name
//! string, the shape of the evaluation result, error translation — and in
//! particular it catches the failure mode that matters most here: misreading
//! `SecTrustEvaluateWithError`'s `bool` and turning a rejection into an
//! acceptance. It does NOT independently validate macOS's trust decision, and
//! nobody should describe it as proving the certificates are "correctly"
//! verified.
//!
//! Coverage, stated plainly:
//!
//! * **Apple arm** — fully exercised here, accept and reject, offline.
//! * **Unix arm** — its CHAIN MATH is exercised here
//!   ([`the_webpki_chain_math_matches_its_recorded_expectations`]) because
//!   `WebPkiServerVerifier` is portable and runs natively on this machine. Its
//!   `/etc/ssl/certs` DISCOVERY half is NOT exercised anywhere: that needs a
//!   Linux host.
//! * **Windows arm** — NOT exercised at all, by anything, on any machine that
//!   has run this suite.
//!
//! # Two places the platforms genuinely disagree
//!
//! A corpus asserted as "both arms must reach the same verdict" would be wrong
//! on two inputs, so each case records BOTH expectations and the assertion picks
//! by platform:
//!
//! * a leaf with **no `extendedKeyUsage` at all** — RFC 5280 reads an absent EKU
//!   as unconstrained and webpki follows it; Apple requires an explicit
//!   `id-kp-serverAuth`;
//! * a leaf whose `keyUsage` lacks `digitalSignature`.
//!
//! Both are properties of the PLATFORMS, not of this code:
//! `rustls-platform-verifier` has them today too, which is exactly why the
//! mine-vs-oracle assertion still holds on every case — it compares two
//! implementations on ONE platform, while the recorded expectations describe
//! what each platform does.
//!
//! # This suite was checked by MUTATION, not by passing
//!
//! A green certificate-verification suite proves nothing on its own; the only
//! question worth answering is whether it goes RED when the verifier is wrong.
//! Six defects were injected into `verifier::apple` one at a time and the suite
//! re-run. Every one was caught, and the table records which test caught it and
//! how many of the eleven failed:
//!
//! | injected defect | result |
//! | --- | --- |
//! | accept unconditionally, ignoring `SecTrustEvaluateWithError` | 5 tests fail |
//! | read the `bool` inverted — the `OSStatus`-habit trap | 4 tests fail |
//! | pass a NULL hostname to `SecPolicyCreateSSL` | 4 tests fail |
//! | skip `SecTrustSetVerifyDate` and judge against the wall clock | 2 tests fail |
//! | reverse the certificate order in the array | 3 tests fail |
//! | `SecTrustSetAnchorCertificatesOnly(true)` instead of `false` | 1 test fails |
//!
//! The last row is the reason
//! [`extra_anchors_add_to_the_system_set_rather_than_replacing_it`] exists. That
//! mutation was invisible to every LOCAL fixture — none of them chains to a
//! public root, so add-semantics and replace-semantics agree across the whole
//! offline corpus — and it was caught only after a case was added that runs the
//! captured real chain through the extra-roots verifier. Re-run the mutations
//! before believing any future change to this file.
//!
//! # Time is pinned, and that is not optional
//!
//! Every case names its own `now`. Nothing here calls `UnixTime::now()`. Apple
//! enforces a ~398-day ceiling on TLS leaf validity
//! (`errSecCertificateValidityPeriodTooLong`), so the corpus CANNOT dodge expiry
//! by minting a century-long certificate the way `crates/aterm-net/src/testdata`
//! does — every leaf here lives about a year, and a test that read the wall
//! clock would start failing inside one. A red test with a date on it is a test
//! someone eventually deletes.

use std::sync::Arc;

use rustls::client::danger::ServerCertVerifier;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};

use super::PlatformVerifier;

// --- fixtures --------------------------------------------------------------
// See `src/testdata/tls/README.md` for what each one is and `mint.sh` for how
// it was made. All of them are public test data.

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

/// Every locally minted fixture, for the hermeticity check.
const LOCAL_FIXTURES: &[(&str, &[u8])] = &[
    ("root", ROOT),
    ("inter", INTER),
    ("notca", NOTCA),
    ("good", GOOD),
    ("expired", EXPIRED),
    ("future", FUTURE),
    ("wronghost", WRONGHOST),
    ("viainter", VIAINTER),
    ("vianotca", VIANOTCA),
    ("selfsigned", SELFSIGNED),
    ("nosan", NOSAN),
    ("clientonly", CLIENTONLY),
    ("noeku", NOEKU),
    ("keyenciph", KEYENCIPH),
    ("ipsan", IPSAN),
    ("tampersig", TAMPERSIG),
    ("tampertbs", TAMPERTBS),
];

/// The name every local leaf but two is issued for.
const HOST: &str = "test.aterm.invalid";
/// `wronghost.der`'s own name.
const OTHER_HOST: &str = "other.aterm.invalid";

// Pinned instants. NEVER the wall clock — see the module header.
/// 2025-06-15T12:00:00Z — before the `good` window, inside `expired`'s.
const T25: u64 = 1_749_988_800;
/// 2026-06-15T12:00:00Z — inside the `good` window.
const T26: u64 = 1_781_524_800;
/// 2027-06-15T12:00:00Z — after the `good` window, inside `future`'s.
const T27: u64 = 1_813_060_800;
/// 2026-08-29T06:59:33Z — inside the captured github.com leaf's window
/// (2026-07-03 → 2026-09-30). Pinning is what stops that fixture rotting.
const T_GH: u64 = 1_787_986_773;

// --- outcomes --------------------------------------------------------------

/// One verification's result, reduced to a shape BOTH implementations produce.
///
/// Accept/reject plus the four error kinds the incumbent actually maps. It
/// deliberately stops there: every other platform failure arrives as an opaque
/// `Other(..)` carrying the OS's own words, and asserting on those strings would
/// be asserting on a macOS release note.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Outcome {
    Accept,
    /// The bytes are not a certificate. NOT a trust verdict.
    BadEncoding,
    NotValidForName,
    UnknownIssuer,
    /// The certificate's signature does not check out. webpki says so
    /// specifically; Apple folds it into "no chain could be built".
    BadSignature,
    Revoked,
    /// Extended key usage does not permit server authentication.
    Eku,
    /// Rejected, for a reason the platform did not put in the mapped set.
    Rejected,
}

impl Outcome {
    /// Did this verification ACCEPT the chain? The only question that matters
    /// for the security property; everything else is diagnosis.
    fn accepted(self) -> bool {
        matches!(self, Self::Accept)
    }
}

fn reduce(result: &Result<rustls::client::danger::ServerCertVerified, rustls::Error>) -> Outcome {
    use rustls::CertificateError as Ce;
    match result {
        Ok(_) => Outcome::Accept,
        Err(rustls::Error::InvalidCertificate(inner)) => match inner {
            Ce::BadEncoding => Outcome::BadEncoding,
            Ce::NotValidForName | Ce::NotValidForNameContext { .. } => Outcome::NotValidForName,
            Ce::UnknownIssuer => Outcome::UnknownIssuer,
            Ce::BadSignature => Outcome::BadSignature,
            Ce::Revoked => Outcome::Revoked,
            // Both implementations render "the EKU is wrong" as an `Other`
            // carrying an error whose `Display` is this exact string — the
            // incumbent's `EkuError`, and `super::EkuError` written to match.
            // Neither type is nameable from here, so the text is the seam.
            Ce::Other(other) if other.to_string() == "certificate had invalid extensions" => {
                Outcome::Eku
            }
            _ => Outcome::Rejected,
        },
        Err(_) => Outcome::Rejected,
    }
}

// --- driving both implementations -----------------------------------------

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    crate::tls::init_crypto();
    rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::ring::default_provider()))
}

fn der(bytes: &'static [u8]) -> CertificateDer<'static> {
    CertificateDer::from(bytes)
}

/// One chain, one name, one instant.
struct Case {
    what: &'static str,
    /// End-entity FIRST, then intermediates in the order a server would send them.
    chain: &'static [&'static [u8]],
    server: &'static str,
    now: u64,
    /// What an Apple platform does with this input. MEASURED.
    apple: Outcome,
    /// What the webpki chain math does with it. MEASURED, natively, by
    /// [`the_webpki_chain_math_matches_its_recorded_expectations`] — but note
    /// that on a real Linux box the ROOTS would come from `/etc/ssl/certs`,
    /// which nothing here can exercise.
    webpki: Outcome,
}

impl Case {
    /// The expectation for the arm that is actually compiled in.
    fn expected(&self) -> Outcome {
        #[cfg(target_vendor = "apple")]
        {
            self.apple
        }
        #[cfg(not(target_vendor = "apple"))]
        {
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

/// THE CORPUS.
///
/// Every reject case is ARMED: it is paired with at least one case that differs
/// in exactly ONE variable and must ACCEPT. If a rejection were happening for
/// the wrong reason — a DER that never parsed, a verifier that refuses
/// everything — the twin fails and says so. The counts are asserted by
/// [`the_corpus_is_armed_in_both_directions`].
const CORPUS: &[Case] = &[
    // --- the positive control, and its two temporal twins ---
    Case {
        what: "the good chain, inside its validity window",
        chain: &[GOOD],
        server: HOST,
        now: T26,
        apple: Outcome::Accept,
        webpki: Outcome::Accept,
    },
    Case {
        what: "the SAME bytes, evaluated after the window closes",
        chain: &[GOOD],
        server: HOST,
        now: T27,
        apple: Outcome::Rejected,
        webpki: Outcome::Rejected,
    },
    Case {
        what: "the SAME bytes, evaluated before the window opens",
        chain: &[GOOD],
        server: HOST,
        now: T25,
        apple: Outcome::Rejected,
        webpki: Outcome::Rejected,
    },
    // --- expiry, from both sides ---
    Case {
        what: "an expired leaf, at a now past its notAfter",
        chain: &[EXPIRED],
        server: HOST,
        now: T26,
        apple: Outcome::Rejected,
        webpki: Outcome::Rejected,
    },
    Case {
        what: "the same expired leaf, at a now INSIDE its window",
        chain: &[EXPIRED],
        server: HOST,
        now: T25,
        apple: Outcome::Accept,
        webpki: Outcome::Accept,
    },
    Case {
        what: "a not-yet-valid leaf, at a now before its notBefore",
        chain: &[FUTURE],
        server: HOST,
        now: T26,
        apple: Outcome::Rejected,
        webpki: Outcome::Rejected,
    },
    Case {
        what: "the same not-yet-valid leaf, at a now INSIDE its window",
        chain: &[FUTURE],
        server: HOST,
        now: T27,
        apple: Outcome::Accept,
        webpki: Outcome::Accept,
    },
    // --- names ---
    Case {
        what: "a leaf for another name, asked for ours",
        chain: &[WRONGHOST],
        server: HOST,
        now: T26,
        apple: Outcome::NotValidForName,
        webpki: Outcome::NotValidForName,
    },
    Case {
        what: "the same leaf, asked for the name it actually carries",
        chain: &[WRONGHOST],
        server: OTHER_HOST,
        now: T26,
        apple: Outcome::Accept,
        webpki: Outcome::Accept,
    },
    Case {
        what: "a leaf with a common name but NO subjectAltName",
        chain: &[NOSAN],
        server: HOST,
        now: T26,
        apple: Outcome::NotValidForName,
        webpki: Outcome::NotValidForName,
    },
    Case {
        what: "a leaf whose only SAN is an IP address, dialled by that IP",
        chain: &[IPSAN],
        server: "127.0.0.1",
        now: T26,
        apple: Outcome::Accept,
        webpki: Outcome::Accept,
    },
    Case {
        what: "the same leaf, dialled by a DIFFERENT IP",
        chain: &[IPSAN],
        server: "127.0.0.2",
        now: T26,
        apple: Outcome::NotValidForName,
        webpki: Outcome::NotValidForName,
    },
    // --- chain construction ---
    Case {
        what: "a leaf under the intermediate, WITH the intermediate supplied",
        chain: &[VIAINTER, INTER],
        server: HOST,
        now: T26,
        apple: Outcome::Accept,
        webpki: Outcome::Accept,
    },
    Case {
        what: "the same leaf with the intermediate OMITTED",
        chain: &[VIAINTER],
        server: HOST,
        now: T26,
        apple: Outcome::UnknownIssuer,
        webpki: Outcome::UnknownIssuer,
    },
    Case {
        what: "a leaf signed by a CA:FALSE certificate",
        chain: &[VIANOTCA, NOTCA],
        server: HOST,
        now: T26,
        apple: Outcome::Rejected,
        webpki: Outcome::Rejected,
    },
    Case {
        what: "a leaf that is its own issuer",
        chain: &[SELFSIGNED],
        server: HOST,
        now: T26,
        apple: Outcome::Rejected,
        webpki: Outcome::UnknownIssuer,
    },
    // --- signature integrity: the trap for a verifier that never checks it ---
    Case {
        what: "the good leaf with one signature byte flipped",
        chain: &[TAMPERSIG],
        server: HOST,
        now: T26,
        apple: Outcome::UnknownIssuer,
        webpki: Outcome::BadSignature,
    },
    Case {
        what: "the good leaf with a SAN byte rewritten inside the signed body",
        chain: &[TAMPERTBS],
        server: HOST,
        now: T26,
        apple: Outcome::UnknownIssuer,
        webpki: Outcome::BadSignature,
    },
    // --- purpose ---
    Case {
        what: "a leaf whose extendedKeyUsage is clientAuth only",
        chain: &[CLIENTONLY],
        server: HOST,
        now: T26,
        apple: Outcome::Eku,
        webpki: Outcome::Eku,
    },
    Case {
        what: "a leaf with NO extendedKeyUsage extension (the platforms differ)",
        chain: &[NOEKU],
        server: HOST,
        now: T26,
        apple: Outcome::Eku,
        webpki: Outcome::Accept,
    },
    Case {
        what: "a leaf whose keyUsage omits digitalSignature",
        chain: &[KEYENCIPH],
        server: HOST,
        now: T26,
        apple: Outcome::Accept,
        webpki: Outcome::Accept,
    },
    // --- the wrong-reason control ---
    Case {
        what: "nine bytes that are not a certificate at all",
        chain: &[MALFORMED],
        server: HOST,
        now: T26,
        apple: Outcome::BadEncoding,
        webpki: Outcome::BadEncoding,
    },
];

// --- the tests -------------------------------------------------------------

/// Build both implementations over the fixture root. `None` when the ORACLE
/// could not be constructed, which is the skip signal.
fn pair() -> Option<(PlatformVerifier, rustls_platform_verifier::Verifier)> {
    let anchors = vec![der(ROOT)];
    let oracle =
        rustls_platform_verifier::Verifier::new_with_extra_roots(anchors.clone(), provider())
            .ok()?;
    let mine = PlatformVerifier::new_with_extra_roots(anchors, provider())
        .expect("the first-party verifier must construct wherever the oracle does");
    Some((mine, oracle))
}

#[test]
fn the_first_party_verifier_agrees_with_the_incumbent_on_every_chain() {
    let Some((mine, oracle)) = pair() else {
        eprintln!("SKIP: the oracle verifier could not be constructed on this platform");
        return;
    };
    let mut disagreements = Vec::new();
    for case in CORPUS {
        let got = case.run(&mine);
        let want = case.run(&oracle);
        if got != want {
            disagreements.push(format!("{}: mine={got:?} oracle={want:?}", case.what));
        }
    }
    assert!(
        disagreements.is_empty(),
        "the reimplementation and the incumbent disagree:\n  {}",
        disagreements.join("\n  ")
    );
}

#[test]
fn the_first_party_verifier_matches_its_recorded_expectations() {
    // Independent of the oracle ON PURPOSE. If `rustls-platform-verifier` is
    // ever dropped from the dev-dependencies, this table is what still says what
    // each input must do — and it was measured, not predicted.
    let Some((mine, _oracle)) = pair() else {
        eprintln!("SKIP: no platform verifier on this target");
        return;
    };
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
fn the_corpus_is_armed_in_both_directions() {
    // The anti-degeneracy check. A verifier that rejected EVERYTHING would pass
    // every reject case; a verifier that accepted everything would pass every
    // accept case. Both counts must stay non-trivial, so neither degenerate
    // implementation can be green.
    let accepts = CORPUS.iter().filter(|c| c.expected().accepted()).count();
    let rejects = CORPUS.len() - accepts;
    assert!(
        accepts >= 6,
        "only {accepts} ACCEPT cases: a reject-everything verifier would pass this suite"
    );
    assert!(
        rejects >= 12,
        "only {rejects} REJECT cases: an accept-everything verifier would pass this suite"
    );
}

#[test]
fn the_shipped_constructor_trusts_no_fixture_anchor() {
    // THE anchor-seam assertion. `PlatformVerifier::new` is what
    // `crate::tls::client_config` calls, and it must know nothing about the
    // fixture root — otherwise the test seam would be a live trust override.
    //
    // The stronger half of this guarantee is structural rather than behavioural:
    // `new_with_extra_roots` is `#[cfg(test)]`, so no non-test caller can name
    // it and the compiler enforces that. This is the behavioural half.
    let shipped = match PlatformVerifier::new(provider()) {
        Ok(verifier) => verifier,
        Err(_) => {
            eprintln!("SKIP: no platform trust store on this target");
            return;
        }
    };
    let case = Case {
        what: "the good chain under the SYSTEM anchors only",
        chain: &[GOOD],
        server: HOST,
        now: T26,
        apple: Outcome::UnknownIssuer,
        webpki: Outcome::UnknownIssuer,
    };
    let got = case.run(&shipped);
    assert!(
        !got.accepted(),
        "the shipped verifier accepted a chain rooted in a fixture CA: {got:?}"
    );

    // ...and the SAME chain accepts once the anchor is supplied, which proves
    // the rejection above is about the anchor set and not about the fixture
    // being broken in some other way.
    if let Some((mine, _)) = pair() {
        assert!(
            case.run(&mine).accepted(),
            "the fixture chain must be good; only its anchor is missing"
        );
    }
}

#[test]
fn the_system_trust_store_is_actually_consulted() {
    // The one control the local corpus cannot provide: a chain that a REAL
    // system anchor validates. Without it, a verifier that consulted no store at
    // all would still pass everything above.
    //
    // github.com because `aterm-update-core` fetches from github.com and
    // api.github.com. `now` is pinned inside the captured leaf's window, so this
    // does not rot when the leaf expires — it needs recapturing only when the
    // Sectigo/USERTrust ECC anchor leaves the platform store. See
    // `src/testdata/tls/README.md`.
    let real = Case {
        what: "the captured github.com chain, under the system anchors",
        chain: &[GH_LEAF, GH_INT0, GH_INT1],
        server: "github.com",
        now: T_GH,
        apple: Outcome::Accept,
        webpki: Outcome::Accept,
    };
    let wrong_name = Case {
        what: "the same chain, asked for a name it does not carry",
        chain: &[GH_LEAF, GH_INT0, GH_INT1],
        server: "example.invalid",
        now: T_GH,
        apple: Outcome::NotValidForName,
        webpki: Outcome::NotValidForName,
    };

    let (Ok(mine), Ok(oracle)) = (
        PlatformVerifier::new(provider()),
        rustls_platform_verifier::Verifier::new(provider()),
    ) else {
        eprintln!("SKIP: no system trust store on this target");
        return;
    };

    // Equality holds unconditionally: a machine with a broken or empty trust
    // store flips BOTH implementations the same way, and that is still a fact
    // worth asserting.
    assert_eq!(
        real.run(&mine),
        real.run(&oracle),
        "system-anchored verdicts diverge"
    );
    assert_eq!(wrong_name.run(&mine), wrong_name.run(&oracle));

    // The hard "must ACCEPT" is gated on the oracle having accepted, so a
    // machine whose trust store cannot validate this chain reports a skip rather
    // than a defect in the reimplementation. Same rule as
    // `net_connections/keychain.rs`.
    if real.run(&oracle).accepted() {
        assert!(
            real.run(&mine).accepted(),
            "the system trust store validated this chain for the oracle but not for us"
        );
        assert!(
            !wrong_name.run(&mine).accepted(),
            "a real chain was accepted for a name it does not carry"
        );
    } else {
        eprintln!(
            "SKIP (hard assertion only): this machine's trust store did not validate the \
             captured github.com chain; see src/testdata/tls/README.md for recapture"
        );
    }
}

#[test]
fn extra_anchors_add_to_the_system_set_rather_than_replacing_it() {
    // `SecTrustSetAnchorCertificates` DISABLES every other anchor unless
    // `SecTrustSetAnchorCertificatesOnly(false)` follows it. Getting that
    // backwards is invisible to every LOCAL fixture — none of them chains to a
    // public root, so add-semantics and replace-semantics give identical
    // verdicts across the whole corpus. (Confirmed by mutation: flipping that
    // one argument to `1` left every other test in this file green.)
    //
    // The captured real chain is what tells the two apart: under a verifier that
    // has been given the fixture root as an EXTRA anchor, github.com must still
    // validate through the SYSTEM anchors. If extras replaced the system set it
    // would not.
    let chain = Case {
        what: "the captured github.com chain, under a verifier with extra anchors",
        chain: &[GH_LEAF, GH_INT0, GH_INT1],
        server: "github.com",
        now: T_GH,
        apple: Outcome::Accept,
        webpki: Outcome::Accept,
    };
    let Some((mine, oracle)) = pair() else {
        eprintln!("SKIP: no platform verifier on this target");
        return;
    };
    // Equality unconditionally; the hard assertion only where the oracle agrees
    // the machine's trust store can validate this chain at all.
    assert_eq!(
        chain.run(&mine),
        chain.run(&oracle),
        "extra-anchor semantics diverge"
    );
    if chain.run(&oracle).accepted() {
        assert!(
            chain.run(&mine).accepted(),
            "supplying an extra anchor DISABLED the system anchors"
        );
    } else {
        eprintln!(
            "SKIP (hard assertion only): this machine's trust store did not validate the \
             captured github.com chain; see src/testdata/tls/README.md for recapture"
        );
    }
}

#[test]
fn a_parse_failure_is_reported_as_one_and_never_as_a_trust_verdict() {
    // If `BadEncoding` were reported as an ordinary rejection, a future change
    // that broke DER handling would look like a suite full of healthy reject
    // cases. It is called out separately for exactly that reason.
    let Some((mine, oracle)) = pair() else {
        eprintln!("SKIP: no platform verifier on this target");
        return;
    };
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
        };
        let chain = [CertificateDer::from(bytes.to_vec())];
        let name = ServerName::try_from(HOST).unwrap();
        let at = UnixTime::since_unix_epoch(std::time::Duration::from_secs(case.now));
        let got = reduce(&mine.verify_server_cert(&chain[0], &[], &name, &[], at));
        let want = reduce(&oracle.verify_server_cert(&chain[0], &[], &name, &[], at));
        assert_eq!(got, Outcome::BadEncoding, "{what} must be a PARSE failure");
        assert_eq!(got, want, "{what}: parse-failure reporting diverges");
    }
}

#[test]
fn a_degenerate_server_name_fails_closed_instead_of_disabling_name_checking() {
    // THE NULL-HOSTNAME BACKDOOR. `SecPolicyCreateSSL`'s hostname argument is
    // NULLABLE, and a NULL switches name verification OFF. MEASURED on this
    // machine with a standalone clang probe against Security.framework, all four
    // runs over the same `good.der` chain at the same pinned instant:
    //
    //     SecPolicyCreateSSL(true, NULL)                 -> ACCEPT
    //     SecPolicyCreateSSL(true, "")                   -> REJECT  -67602
    //     SecPolicyCreateSSL(true, "evil.test")          -> REJECT  -67602
    //     SecPolicyCreateSSL(true, "test.aterm.invalid") -> ACCEPT
    //
    // So the danger is the NULL POINTER specifically, not a short or odd string.
    // `verifier::apple` cannot produce that pointer: there is exactly one call to
    // `SecPolicyCreateSSL`, its hostname comes from a `CfOwned`, and a `cf_string`
    // failure is `?`-propagated rather than defaulted to null — the mistake this
    // guards against is `unwrap_or(ptr::null())`, which compiles and which every
    // other test in this file would still pass, because they all pass the RIGHT
    // name.
    //
    // What is asserted here is the adjacent value a refactor is most likely to
    // let through — the EMPTY name — plus the fact that name checking is on at
    // all. Neither may accept.
    let Some((mine, _oracle)) = pair() else {
        eprintln!("SKIP: no platform verifier on this target");
        return;
    };

    // Name checking is live: a certificate for another name is refused.
    let mismatch = Case {
        what: "a leaf for another name",
        chain: &[WRONGHOST],
        server: HOST,
        now: T26,
        apple: Outcome::NotValidForName,
        webpki: Outcome::NotValidForName,
    };
    assert!(
        !mismatch.run(&mine).accepted(),
        "name checking is not happening at all"
    );

    // `rustls` cannot currently build an empty `ServerName`, so the shell's
    // empty-name guard is defence in depth rather than a reachable path. The
    // platform arm is reachable directly, so the degenerate value is driven
    // through IT, and must be refused.
    #[cfg(target_vendor = "apple")]
    {
        let arm = super::apple::Verifier::new(vec![der(ROOT)], provider())
            .expect("the fixture root is a usable anchor");
        let name = ServerName::try_from(HOST).expect("a usable server name");
        let at = UnixTime::since_unix_epoch(std::time::Duration::from_secs(T26));
        for degenerate in ["", "\u{0}"] {
            let result = arm.verify(&der(GOOD), &[], &name, degenerate, None, at);
            assert!(
                result.is_err(),
                "the arm accepted a chain against the server name {degenerate:?}"
            );
        }
    }

    // And a valid name never renders empty, which is what the shell's guard
    // relies on being true for every name it does let through.
    assert!(!ServerName::try_from(HOST).unwrap().to_str().is_empty());
}

#[test]
fn every_local_fixture_is_hermetic() {
    // The corpus's reject cases only mean anything if the platform cannot repair
    // a broken chain by fetching something. No `authorityInformationAccess` and
    // no `crlDistributionPoints` anywhere means there is no URL to fetch, on any
    // platform, whatever the flags say.
    //
    // Encoded OIDs, as they appear inside an X.509 extension's OBJECT
    // IDENTIFIER: 1.3.6.1.5.5.7.1.1 (AIA) and 2.5.29.31 (CRL distribution
    // points).
    const AIA: &[u8] = &[0x06, 0x08, 0x2B, 0x06, 0x01, 0x05, 0x05, 0x07, 0x01, 0x01];
    const CRLDP: &[u8] = &[0x06, 0x03, 0x55, 0x1D, 0x1F];
    for (name, bytes) in LOCAL_FIXTURES {
        assert!(
            !bytes.windows(AIA.len()).any(|w| w == AIA),
            "{name}.der carries authorityInformationAccess; the platform could fetch its issuer \
             and turn a reject case into an accept"
        );
        assert!(
            !bytes.windows(CRLDP.len()).any(|w| w == CRLDP),
            "{name}.der carries crlDistributionPoints"
        );
    }
}

#[test]
fn the_webpki_chain_math_matches_its_recorded_expectations() {
    // The Unix arm's verifier is `rustls`'s own `WebPkiServerVerifier`, which is
    // portable — so its CHAIN MATH can be driven on this machine even though its
    // `/etc/ssl/certs` DISCOVERY half cannot. This is what backs the `webpki`
    // column of `CORPUS`; without it those values would be predictions.
    //
    // What it does NOT cover, and nothing here does: reading the system store,
    // the tolerant PEM path over a real distro bundle, or the empty-store
    // failure. Those need a Linux host.
    let mut store = rustls::RootCertStore::empty();
    store
        .add(der(ROOT))
        .expect("the fixture root is a usable anchor");
    let verifier =
        rustls::client::WebPkiServerVerifier::builder_with_provider(store.into(), provider())
            .build()
            .expect("a one-root store builds a verifier");

    let mut wrong = Vec::new();
    for case in CORPUS {
        // Match the Unix arm's own EKU normalisation so the two are comparable.
        let chain: Vec<CertificateDer<'static>> = case.chain.iter().copied().map(der).collect();
        let name = ServerName::try_from(case.server).expect("a usable server name");
        let result = verifier
            .verify_server_cert(
                &chain[0],
                &chain[1..],
                &name,
                &[],
                UnixTime::since_unix_epoch(std::time::Duration::from_secs(case.now)),
            )
            .map_err(|error| match &error {
                rustls::Error::InvalidCertificate(rustls::CertificateError::InvalidPurpose)
                | rustls::Error::InvalidCertificate(
                    rustls::CertificateError::InvalidPurposeContext { .. },
                ) => super::eku_rejected(),
                _ => error,
            });
        let got = reduce(&result);
        if got != case.webpki {
            wrong.push(format!(
                "{}: got {got:?}, recorded {:?}",
                case.what, case.webpki
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "the recorded webpki expectations are stale:\n  {}",
        wrong.join("\n  ")
    );
}

#[test]
fn handshake_signature_verification_is_delegated_to_the_provider() {
    // Not platform work: the three signature methods must expose exactly the
    // provider's algorithms, the way `crates/aterm-net/src/tls.rs:157` does.
    // A verifier that narrowed this set would break handshakes in a way no
    // certificate fixture can catch.
    let Some((mine, _oracle)) = pair() else {
        eprintln!("SKIP: no platform verifier on this target");
        return;
    };
    let expected = provider()
        .signature_verification_algorithms
        .supported_schemes();
    assert_eq!(mine.supported_verify_schemes(), expected);
    assert!(!expected.is_empty());
}
