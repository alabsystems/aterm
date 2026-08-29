// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DIFFERENTIAL ORACLE, over the configuration that actually ships.
//!
//! `crates/aterm-http/src/verifier.rs` replaced `rustls-platform-verifier` with
//! first-party OS calls. The retired crate is kept as a `[dev-dependencies]`
//! oracle, and this file drives BOTH implementations over the same chains, with
//! the same anchors, at the same pinned instant, for the same hostname, and
//! demands the SAME VERDICT — not "both are `Err`", the same verdict, down to
//! the error kind and, where the platform speaks for itself, down to the
//! `OSStatus` it reported.
//!
//! # Why this is an integration test, and what that buys
//!
//! `src/verifier/tests.rs` is the other half of the oracle and runs in the same
//! `cargo test -p aterm-http`. It is a UNIT test, so it can reach
//! `PlatformVerifier::new_with_extra_roots`, which is `#[cfg(test)]`. That lets
//! it hand both implementations a throwaway fixture CA and exercise the ACCEPT
//! path offline over seventeen locally minted chains.
//!
//! This file deliberately cannot do that. An integration test links the crate as
//! a CONSUMER does, so the only constructor it can name is
//! [`PlatformVerifier::new`] — the exact one `tls::client_config` calls, with no
//! anchor seam, no fixture CA, nothing but the operating system's own trust
//! store. That is the configuration that ships, and it is a strictly different
//! object under test:
//!
//! * the unit suite proves the two implementations agree about CHAIN MATH given
//!   an anchor;
//! * this suite proves they agree about what the MACHINE ACTUALLY TRUSTS — and
//!   that in the shipped build the only route to `Ok` is a real system anchor.
//!
//! The seam being unreachable from here is not a limitation to work around. It
//! is the property `verifier.rs`'s header claims, and this file is where a
//! reader can see the compiler enforcing it: if `new_with_extra_roots` were
//! callable by a consumer, it would be callable here.
//!
//! # THE THREE WAYS A SUITE LIKE THIS LIES
//!
//! **1. A reject-everything verifier passes a reject-only suite.** So there are
//! POSITIVE controls, and they are not decorative: [`REAL`] carries two captured
//! production chains from INDEPENDENT roots — github.com under Sectigo Public
//! Server Authentication Root E46 (ECC), and downloads.claude.ai under Google
//! Trust Services' GTS Root R1 (RSA) — and seven of the corpus's thirty-seven
//! cases must ACCEPT. Those counts are asserted, not merely written down, by
//! [`the_corpus_is_armed_in_both_directions`].
//! Both hosts are ones aterm really contacts (`aterm-update-core` fetches
//! releases from github.com; `crates/atpkg/src/vendor.rs:59` allow-lists
//! downloads.claude.ai). Replacing `verify_server_cert`'s body with
//! `Err(...)` turns this file red; that was not assumed, it was RUN, and the
//! ledger is under "Checked by mutation" below.
//!
//! [`the_positive_control_is_armed_and_not_inert`] is the guard on the guard: if
//! neither real chain validates any more — a root finally left the platform
//! store — it FAILS, loudly, naming the recapture procedure, instead of letting
//! the suite quietly degrade into a reject-only shell that a broken verifier
//! would sail through. It fails on the ORACLE's verdict, never on ours, so it
//! can never be misread as a defect in the reimplementation.
//!
//! **2. A reject case that fails for the wrong reason.** Two mechanisms:
//!
//! * Every reject case on a real chain is ARMED by a twin that differs in
//!   EXACTLY ONE variable and must ACCEPT — the same bytes at a `now` inside the
//!   window, the same chain asked for the name it carries, the same certificates
//!   in the order a server sends them. A rejection that survived because the DER
//!   never parsed, or because the verifier refuses everything, kills its twin.
//! * Nothing is asserted as merely `is_err()`. Each case records the exact
//!   `rustls::CertificateError` variant, and where the platform answered in its
//!   own words the case records the `OSStatus` NUMBER (`-67818`,
//!   `-67901`, ...). The number is appended by our own code
//!   (`verifier/apple.rs:537`), so it survives a macOS wording or localisation
//!   change; the PROSE is macOS's and is never compared against a literal here.
//! * `malformed.der` is the wrong-reason control and is asserted as
//!   `BadEncoding` SPECIFICALLY, by
//!   [`a_parse_failure_is_reported_as_one_and_never_as_a_trust_verdict`]. A DER
//!   that never parsed is not a trust verdict and must never be counted as one.
//!
//! **3. Time drift.** Every case names its own `now`; nothing here calls
//! `UnixTime::now()`, and [`nothing_in_this_suite_reads_the_wall_clock`] asserts
//! that the corpus's verdicts do not move when the machine's clock does. Both
//! implementations honour the `now` argument — `rustls-platform-verifier` 0.6.2
//! passes it to `SecTrustSetVerifyDate` unconditionally
//! (`src/verification/apple.rs`), and so do we — which is what makes the
//! captured leaves usable for ever despite expiring in 2026. **No assertion in
//! this file is wall-clock dependent.** The one thing pinning cannot protect is
//! a root leaving the trust store, which is why there are two of them from two
//! operators, and why failure #1's guard exists.
//!
//! # Checked by mutation, not by passing
//!
//! A green certificate suite is worth nothing until it has been shown to go RED.
//! Measured on m21 (Darwin 25.5.0), one defect at a time, `cargo test -p
//! aterm-http --test verifier_differential` re-run after each:
//!
//! | injected defect | THIS file (of 17) | `verifier::tests` (of 68) |
//! | --- | --- | --- |
//! | `verify_server_cert` returns `Err` unconditionally | **10 fail** | 6 fail |
//! | `verify_server_cert` returns `Ok` unconditionally | **12 fail** | 6 fail |
//! | `SecTrustEvaluateWithError`'s `bool` read inverted | **10 fail** | 5 fail |
//! | certificate ORDER reversed in the array | **10 fail** | 4 fail |
//! | `SecTrustSetVerifyDate` skipped (judge by the wall clock) | **5 fail** | 2 fail |
//! | hostname dropped: `SecPolicyCreateSSL(true, NULL)` | **5 fail** | 4 fail |
//! | `SecTrustSetNetworkFetchAllowed(true)` | **1 fails** | 0 fail |
//! | `SecTrustSetAnchorCertificatesOnly(true)` | 0 fail | **1 fails** |
//!
//! Row 1 is the one that matters most: it is the mutation a reject-only suite
//! cannot see, and it turns ten of the seventeen tests here red.
//!
//! The last two rows are why BOTH files exist and neither subsumes the other.
//! Only this one catches a verifier that lets macOS fetch a missing intermediate
//! over the network, because only a REAL chain carries an
//! `authorityInformationAccess` URI to fetch from. Only the unit suite catches
//! anchor REPLACE-vs-ADD semantics, because only it can supply an extra anchor.
//! Delete either file and a defect walks.
//!
//! # One observed instability, and where it is NOT
//!
//! While this file was being written, the UNIT suite's
//! `verifier::tests::extra_anchors_add_to_the_system_set_rather_than_replacing_it`
//! — a pre-existing test, not one added here — failed on THREE separate
//! occasions, each within minutes of a full mutation-testing run, and cleared
//! on its own each time. Always the same shape: over the captured github.com
//! chain, under a CUSTOM anchor set, the first-party verifier reported a
//! rejection where the reference accepted.
//!
//! **The cause was not established, and this comment does not pretend
//! otherwise.** What was ruled out, by measurement:
//!
//! * not test interaction or concurrency — it fails filtered down to that one
//!   test, and under `--test-threads=1`, just as reliably as in the full suite;
//! * not the anchor mutation's lingering effect — deliberately re-running that
//!   mutation and restoring, both filtered and across the whole suite, does not
//!   reproduce it;
//! * not a slow drift — while stuck it fails every run for minutes, then passes
//!   every run for hours.
//!
//! It also resists diagnosis in a way worth writing down: adding ANY
//! instrumentation to the failing test makes it pass. Four instrumented runs in
//! the middle of an episode where the uninstrumented test was failing six times
//! out of six all reported ACCEPT. That is why no `OSStatus` for it appears
//! anywhere in this repository — it was never observable.
//!
//! What IS established is where it is not. In every episode this file was run
//! against the same machine in the same minutes and was green every time,
//! including three runs taken CONCURRENTLY with the failing one. The two suites
//! differ in exactly one thing: the unit oracle attaches a custom anchor set
//! with `SecTrustSetAnchorCertificates`, and `PlatformVerifier::new` — the only
//! constructor a shipped build has, and the only one this file can reach — does
//! not. Every observation of the instability is on the `#[cfg(test)]` anchor
//! path; not one is on the path that ships.
//!
//! If it ever appears HERE, in the shipped configuration, that is a different
//! and much more serious thing and must not be waved off as the same flake. The
//! failure messages below name it so nobody has to rediscover this.
//!
//! # What this does NOT prove — read before quoting it
//!
//! * On macOS both implementations end up inside the SAME `SecTrust` engine.
//!   This is a PLUMBING differential: it pins policy construction, the verify
//!   date, the anchor set, certificate ORDER, the server-name string, how the
//!   result is read and how errors are translated. It does NOT independently
//!   validate macOS's trust decision.
//! * The Windows arm is not exercised by this file or by any other. Nothing has
//!   ever run it.
//! * Revocation (OCSP, CRL, CT) is untested on every arm. A verifier that
//!   silently skipped it would pass every case here.
//! * On a non-Apple host the `accept` column below still holds and is asserted;
//!   the `apple` detail column is not, because it was measured on macOS and
//!   predicting the other platforms' error vocabulary would be inventing
//!   evidence. See [`the_shipped_verifier_matches_its_recorded_expectations`].

use std::sync::Arc;
use std::time::Duration;

use rustls::client::danger::{ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};

use aterm_http::verifier::PlatformVerifier;

// ---------------------------------------------------------------------------
// Fixtures. See `src/testdata/tls/README.md` and `mint.sh`.
// ---------------------------------------------------------------------------

macro_rules! fixture {
    ($name:literal) => {
        include_bytes!(concat!("../src/testdata/tls/", $name)) as &'static [u8]
    };
}

// Locally minted: every one chains to a throwaway CA that is in no machine's
// trust store anywhere, so the SHIPPED verifier must refuse all of them.
const ROOT: &[u8] = fixture!("root.der");
const INTER: &[u8] = fixture!("inter.der");
const NOTCA: &[u8] = fixture!("notca.der");
const GOOD: &[u8] = fixture!("good.der");
const EXPIRED: &[u8] = fixture!("expired.der");
const FUTURE: &[u8] = fixture!("future.der");
const WRONGHOST: &[u8] = fixture!("wronghost.der");
const VIAINTER: &[u8] = fixture!("viainter.der");
const VIANOTCA: &[u8] = fixture!("vianotca.der");
const SELFSIGNED: &[u8] = fixture!("selfsigned.der");
const NOSAN: &[u8] = fixture!("nosan.der");
const CLIENTONLY: &[u8] = fixture!("clientonly.der");
const NOEKU: &[u8] = fixture!("noeku.der");
const KEYENCIPH: &[u8] = fixture!("keyenciph.der");
const IPSAN: &[u8] = fixture!("ipsan.der");
const TAMPERSIG: &[u8] = fixture!("tampersig.der");
const TAMPERTBS: &[u8] = fixture!("tampertbs.der");
const MALFORMED: &[u8] = fixture!("malformed.der");

// Captured production chains — the ONLY inputs here a real system anchor
// validates, and therefore the only source of an ACCEPT in a shipped build.
const GH_LEAF: &[u8] = fixture!("gh-leaf.der");
const GH_INT0: &[u8] = fixture!("gh-int0.der");
const GH_INT1: &[u8] = fixture!("gh-int1.der");
const CL_LEAF: &[u8] = fixture!("cl-leaf.der");
const CL_INT0: &[u8] = fixture!("cl-int0.der");
const CL_INT1: &[u8] = fixture!("cl-int1.der");

const GH: &[&[u8]] = &[GH_LEAF, GH_INT0, GH_INT1];
const CL: &[&[u8]] = &[CL_LEAF, CL_INT0, CL_INT1];

/// The name the local leaves are issued for.
const HOST: &str = "test.aterm.invalid";
/// `wronghost.der`'s own name.
const OTHER_HOST: &str = "other.aterm.invalid";

// Pinned instants. NEVER `UnixTime::now()` — see failure mode 3 above.
/// 2025-06-15T12:00:00Z — inside `expired.der`'s window.
const T25: u64 = 1_749_988_800;
/// 2026-06-15T12:00:00Z — inside `good.der`'s window.
const T26: u64 = 1_781_524_800;
/// 2027-06-15T12:00:00Z — inside `future.der`'s window.
const T27: u64 = 1_813_060_800;
/// 2026-08-29T06:59:33Z — inside BOTH captured leaves' windows
/// (github.com 2026-07-03 → 2026-09-30, downloads.claude.ai
/// 2026-07-18 → 2026-10-16). Pinning here is what stops those fixtures rotting
/// when the leaves expire, which they will.
const T_REAL: u64 = 1_787_986_773;
/// 2010-01-01T00:00:00Z — long before either captured leaf existed.
const T_2010: u64 = 1_262_304_000;
/// 2040-03-19T00:00:00Z — long after both have expired, and after the roots'
/// own notAfter as well.
const T_2040: u64 = 2_216_160_000;

// ---------------------------------------------------------------------------
// Outcomes
// ---------------------------------------------------------------------------

/// One verification's result, reduced to a shape BOTH implementations produce.
///
/// [`Outcome::Platform`] keeps the platform's whole message so the differential
/// can compare two implementations' rendering of it exactly. The RECORDED
/// expectations never look at that text — only at the `OSStatus` our own code
/// appends to it (see [`Detail::Status`]).
#[derive(Debug, PartialEq, Eq, Clone)]
enum Outcome {
    Accept,
    /// The bytes are not a certificate. NOT a trust verdict.
    BadEncoding,
    NotValidForName,
    UnknownIssuer,
    BadSignature,
    Revoked,
    /// Extended key usage does not permit server authentication.
    Eku,
    /// Rejected by the platform's trust evaluation, in the platform's own words.
    Platform(String),
    /// Rejected by `rustls` for something outside the certificate itself.
    Other(String),
}

impl Outcome {
    fn accepted(&self) -> bool {
        matches!(self, Self::Accept)
    }

    /// The `OSStatus` our own code appended to a platform rejection
    /// (`verifier/apple.rs:537` formats `"{description}: {code}"`). `None` for
    /// every other outcome, and for a message that does not end in a number —
    /// which is itself worth failing on, since it would mean the code stopped
    /// being reported.
    fn status(&self) -> Option<i64> {
        let Self::Platform(text) = self else {
            return None;
        };
        text.rsplit_once(':')?.1.trim().parse().ok()
    }
}

fn reduce(result: &Result<ServerCertVerified, rustls::Error>) -> Outcome {
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
            // whose `Display` is this exact string — the incumbent's
            // `EkuError`, and `verifier::EkuError` written to match. Neither
            // type is nameable from here, so the text is the seam.
            Ce::Other(other) if other.to_string() == "certificate had invalid extensions" => {
                Outcome::Eku
            }
            Ce::Other(other) => Outcome::Platform(other.to_string()),
            other => Outcome::Other(format!("{other:?}")),
        },
        Err(other) => Outcome::Other(format!("{other:?}")),
    }
}

// ---------------------------------------------------------------------------
// Cases
// ---------------------------------------------------------------------------

/// What the Apple arm reports. MEASURED on m21 (Darwin 25.5.0) on 2026-08-29,
/// never predicted.
#[derive(Debug, Clone, Copy)]
enum Detail {
    Accept,
    /// One of the variants `rustls` names itself.
    Kind(Kind),
    /// The platform answered in its own words and reported this `OSStatus`.
    /// Only the NUMBER is asserted; see the module header.
    Status(i64),
}

/// The subset of [`Outcome`] that carries no platform text, so a case can name
/// its expectation without embedding a macOS message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    BadEncoding,
    NotValidForName,
    UnknownIssuer,
}

impl Detail {
    fn matches(self, got: &Outcome) -> bool {
        match self {
            Self::Accept => got.accepted(),
            Self::Kind(Kind::BadEncoding) => *got == Outcome::BadEncoding,
            Self::Kind(Kind::NotValidForName) => *got == Outcome::NotValidForName,
            Self::Kind(Kind::UnknownIssuer) => *got == Outcome::UnknownIssuer,
            Self::Status(code) => got.status() == Some(code),
        }
    }
}

struct Case {
    what: &'static str,
    /// End-entity FIRST, then intermediates in the order a server sends them.
    chain: &'static [&'static [u8]],
    server: &'static str,
    now: u64,
    /// Does the SHIPPED verifier accept? PORTABLE — asserted on every platform,
    /// because "is this chain rooted in the machine's trust store" has the same
    /// answer everywhere the roots are the usual public ones.
    accept: bool,
    /// Apple-only detail; see [`Detail`].
    apple: Detail,
}

impl Case {
    fn run(&self, verifier: &dyn ServerCertVerifier) -> Outcome {
        let chain: Vec<CertificateDer<'static>> = self
            .chain
            .iter()
            .copied()
            .map(CertificateDer::from)
            .collect();
        let Some((end_entity, intermediates)) = chain.split_first() else {
            panic!("case {} has an empty chain", self.what);
        };
        let name = ServerName::try_from(self.server)
            .unwrap_or_else(|_| panic!("case {} has an unusable server name", self.what));
        reduce(&verifier.verify_server_cert(
            end_entity,
            intermediates,
            &name,
            &[],
            UnixTime::since_unix_epoch(Duration::from_secs(self.now)),
        ))
    }
}

/// THE ARMED HALF: two captured production chains, from two independent roots.
///
/// Every rejection here is paired with an acceptance that differs in exactly one
/// variable, so no rejection can be passing for the wrong reason without its
/// twin noticing. These are also the ONLY cases in this file that can accept at
/// all — which is what makes a reject-everything verifier fail.
const REAL: &[Case] = &[
    // --- github.com, under Sectigo Public Server Authentication Root E46 -----
    Case {
        what: "github.com's real chain, for the name it carries",
        chain: GH,
        server: "github.com",
        now: T_REAL,
        accept: true,
        apple: Detail::Accept,
    },
    Case {
        what: "the same chain, for its second subjectAltName",
        chain: GH,
        server: "www.github.com",
        now: T_REAL,
        accept: true,
        apple: Detail::Accept,
    },
    Case {
        what: "the same chain, for a name it does not carry",
        chain: GH,
        server: "example.invalid",
        now: T_REAL,
        accept: false,
        apple: Detail::Kind(Kind::NotValidForName),
    },
    Case {
        what: "the same chain, for the OTHER captured host's name",
        chain: GH,
        server: "downloads.claude.ai",
        now: T_REAL,
        accept: false,
        apple: Detail::Kind(Kind::NotValidForName),
    },
    Case {
        what: "the same chain, sixteen years before its leaf existed",
        chain: GH,
        server: "github.com",
        now: T_2010,
        // errSecCertificateExpired. macOS reports the SAME code for
        // not-yet-valid as for expired, which is why this file records the
        // number rather than pretending to distinguish the two.
        accept: false,
        apple: Detail::Status(-67818),
    },
    Case {
        what: "the same chain, fourteen years after its leaf expired",
        chain: GH,
        server: "github.com",
        now: T_2040,
        accept: false,
        apple: Detail::Status(-67818),
    },
    Case {
        what: "the same certificates, in reverse order",
        chain: &[GH_INT1, GH_INT0, GH_LEAF],
        server: "github.com",
        now: T_REAL,
        // errSecCertificateValidityPeriodTooLong: a ROOT read as an end-entity
        // fails the ~398-day TLS leaf ceiling. The point is that order is load
        // bearing, not which code says so.
        accept: false,
        apple: Detail::Status(-67901),
    },
    Case {
        what: "the chain's own root offered as the end-entity",
        chain: &[GH_INT1],
        server: "github.com",
        now: T_REAL,
        accept: false,
        apple: Detail::Status(-67901),
    },
    Case {
        what: "github.com's leaf with only the intermediate it truly needs",
        chain: &[GH_LEAF, GH_INT0],
        server: "github.com",
        now: T_REAL,
        accept: true,
        apple: Detail::Accept,
    },
    // --- downloads.claude.ai, under Google Trust Services' GTS Root R1 -------
    // A SECOND positive control under a root from a different operator with a
    // different key type (RSA, not ECC). One root leaving the platform trust
    // store cannot disarm this suite on its own.
    Case {
        what: "downloads.claude.ai's real chain, for the name it carries",
        chain: CL,
        server: "downloads.claude.ai",
        now: T_REAL,
        accept: true,
        apple: Detail::Accept,
    },
    Case {
        what: "the same chain, for its second subjectAltName",
        chain: CL,
        server: "downloads.claude.com",
        now: T_REAL,
        accept: true,
        apple: Detail::Accept,
    },
    Case {
        what: "the same chain, for a name it does not carry",
        chain: CL,
        server: "example.invalid",
        now: T_REAL,
        accept: false,
        apple: Detail::Kind(Kind::NotValidForName),
    },
    Case {
        what: "the same chain, for the OTHER captured host's name",
        chain: CL,
        server: "github.com",
        now: T_REAL,
        accept: false,
        apple: Detail::Kind(Kind::NotValidForName),
    },
    Case {
        what: "the same chain, sixteen years before its leaf existed",
        chain: CL,
        server: "downloads.claude.ai",
        now: T_2010,
        accept: false,
        apple: Detail::Status(-67818),
    },
    Case {
        what: "the same chain, fourteen years after its leaf expired",
        chain: CL,
        server: "downloads.claude.ai",
        now: T_2040,
        accept: false,
        apple: Detail::Status(-67818),
    },
    Case {
        what: "the same certificates, in reverse order",
        chain: &[CL_INT1, CL_INT0, CL_LEAF],
        server: "downloads.claude.ai",
        now: T_REAL,
        accept: false,
        apple: Detail::Status(-67901),
    },
    Case {
        what: "the chain's own root offered as the end-entity",
        chain: &[CL_INT1],
        server: "downloads.claude.ai",
        now: T_REAL,
        accept: false,
        apple: Detail::Status(-67901),
    },
    Case {
        what: "downloads.claude.ai's leaf with only the intermediate it truly needs",
        chain: &[CL_LEAF, CL_INT0],
        server: "downloads.claude.ai",
        now: T_REAL,
        accept: true,
        apple: Detail::Accept,
    },
    // --- the two chains crossed ---------------------------------------------
    // A certificate from the OTHER real chain, offered as an extra
    // intermediate, must neither help nor hurt: the chain the leaf really needs
    // is still present, so this must still ACCEPT. It is the control that says
    // an unrelated certificate in the array is ignored rather than confusing
    // the builder.
    Case {
        what: "github.com's chain with an unrelated real leaf spliced in",
        chain: &[GH_LEAF, CL_LEAF, GH_INT0, GH_INT1],
        server: "github.com",
        now: T_REAL,
        accept: true,
        apple: Detail::Accept,
    },
];

/// THE ANCHOR HALF: every locally minted fixture, each evaluated at a `now`
/// INSIDE its own validity window and asked for the name it carries, so that the
/// ONE thing wrong with it is that nothing anchors it.
///
/// These are not eighteen independent reject reasons — they are one fact
/// (`PlatformVerifier::new` trusts no fixture CA) in eighteen shapes, and the
/// value is that no shape sneaks past: not a self-signed leaf, not a chain
/// through a `CA:FALSE` issuer, not an IP-SAN, not a tampered signature. The
/// per-shape reject REASONS are proved in `src/verifier/tests.rs`, where the
/// anchor can be supplied and every one of them has an accepting twin.
const LOCAL: &[Case] = &[
    Case {
        what: "the fixture positive control, unanchored",
        chain: &[GOOD],
        server: HOST,
        now: T26,
        accept: false,
        apple: Detail::Kind(Kind::UnknownIssuer),
    },
    Case {
        what: "the expired leaf INSIDE its own window, unanchored",
        chain: &[EXPIRED],
        server: HOST,
        now: T25,
        accept: false,
        apple: Detail::Kind(Kind::UnknownIssuer),
    },
    Case {
        what: "the not-yet-valid leaf INSIDE its own window, unanchored",
        chain: &[FUTURE],
        server: HOST,
        now: T27,
        accept: false,
        apple: Detail::Kind(Kind::UnknownIssuer),
    },
    Case {
        what: "the wrong-host leaf asked for its OWN name, unanchored",
        chain: &[WRONGHOST],
        server: OTHER_HOST,
        now: T26,
        accept: false,
        apple: Detail::Kind(Kind::UnknownIssuer),
    },
    Case {
        what: "a leaf with a common name but no subjectAltName",
        chain: &[NOSAN],
        server: HOST,
        now: T26,
        accept: false,
        apple: Detail::Kind(Kind::UnknownIssuer),
    },
    Case {
        what: "an IP-SAN leaf dialled by its own IP",
        chain: &[IPSAN],
        server: "127.0.0.1",
        now: T26,
        accept: false,
        apple: Detail::Kind(Kind::UnknownIssuer),
    },
    Case {
        what: "a leaf under the fixture intermediate, WITH the intermediate",
        chain: &[VIAINTER, INTER],
        server: HOST,
        now: T26,
        accept: false,
        apple: Detail::Kind(Kind::UnknownIssuer),
    },
    Case {
        what: "the same leaf with the intermediate omitted",
        chain: &[VIAINTER],
        server: HOST,
        now: T26,
        accept: false,
        apple: Detail::Kind(Kind::UnknownIssuer),
    },
    Case {
        what: "a leaf signed by a CA:FALSE certificate",
        chain: &[VIANOTCA, NOTCA],
        server: HOST,
        now: T26,
        // errSecInvalidBasicConstraints -- macOS names the offending issuer
        // BEFORE it gets as far as "no anchor", which is why this one is not
        // UnknownIssuer like its neighbours.
        accept: false,
        apple: Detail::Status(-67605),
    },
    Case {
        what: "a leaf that is its own issuer",
        chain: &[SELFSIGNED],
        server: HOST,
        now: T26,
        // errSecNotTrusted.
        accept: false,
        apple: Detail::Status(-67843),
    },
    Case {
        what: "the good leaf with one signature byte flipped",
        chain: &[TAMPERSIG],
        server: HOST,
        now: T26,
        accept: false,
        apple: Detail::Kind(Kind::UnknownIssuer),
    },
    Case {
        what: "the good leaf with a SAN byte rewritten inside the signed body",
        chain: &[TAMPERTBS],
        server: HOST,
        now: T26,
        accept: false,
        apple: Detail::Kind(Kind::UnknownIssuer),
    },
    Case {
        what: "a leaf whose extendedKeyUsage is clientAuth only",
        chain: &[CLIENTONLY],
        server: HOST,
        now: T26,
        accept: false,
        apple: Detail::Kind(Kind::UnknownIssuer),
    },
    Case {
        what: "a leaf with no extendedKeyUsage at all",
        chain: &[NOEKU],
        server: HOST,
        now: T26,
        accept: false,
        apple: Detail::Kind(Kind::UnknownIssuer),
    },
    Case {
        what: "a leaf whose keyUsage omits digitalSignature",
        chain: &[KEYENCIPH],
        server: HOST,
        now: T26,
        accept: false,
        apple: Detail::Kind(Kind::UnknownIssuer),
    },
    Case {
        what: "the fixture ROOT offered as an end-entity certificate",
        chain: &[ROOT],
        server: HOST,
        now: T26,
        // errSecCertificateValidityPeriodTooLong: a ten-year CA read as a TLS
        // leaf. Recorded because it is what macOS ACTUALLY says, not because
        // the reason matters -- what matters is that a CA cannot serve as a
        // leaf.
        accept: false,
        apple: Detail::Status(-67901),
    },
    Case {
        what: "the fixture INTERMEDIATE offered as an end-entity certificate",
        chain: &[INTER],
        server: HOST,
        now: T26,
        accept: false,
        apple: Detail::Status(-67901),
    },
    Case {
        what: "nine bytes that are not a certificate at all",
        chain: &[MALFORMED],
        server: HOST,
        now: T26,
        accept: false,
        apple: Detail::Kind(Kind::BadEncoding),
    },
];

fn corpus() -> impl Iterator<Item = &'static Case> {
    REAL.iter().chain(LOCAL.iter())
}

// ---------------------------------------------------------------------------
// Building both implementations
// ---------------------------------------------------------------------------

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    aterm_http::tls::init_crypto();
    rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::ring::default_provider()))
}

/// The SHIPPED verifier and the oracle, both over the machine's own trust store
/// and nothing else. `None` when the ORACLE could not be constructed, which is
/// the skip signal — the same discipline as
/// `crates/aterm-gui/src/net_connections/keychain.rs:490`: a failure to build
/// the REFERENCE is a fact about the machine and must never be reported as a
/// defect in the reimplementation.
fn pair() -> Option<(PlatformVerifier, rustls_platform_verifier::Verifier)> {
    let oracle = rustls_platform_verifier::Verifier::new(provider()).ok()?;
    let mine = PlatformVerifier::new(provider())
        .expect("the shipped verifier must construct wherever the oracle does");
    Some((mine, oracle))
}

// ---------------------------------------------------------------------------
// THE DIFFERENTIAL
// ---------------------------------------------------------------------------

#[test]
fn the_shipped_verifier_and_the_incumbent_reach_the_same_verdict_on_every_case() {
    // Not "both are Err". The SAME verdict: the same accept/reject, the same
    // `rustls` error variant, and for a platform rejection the same message the
    // platform produced -- compared between the two implementations, never
    // against a literal, so a macOS wording change moves both sides together.
    let Some((mine, oracle)) = pair() else {
        eprintln!("SKIP: the oracle verifier could not be constructed on this platform");
        return;
    };
    let mut disagreements = Vec::new();
    for case in corpus() {
        let got = case.run(&mine);
        let want = case.run(&oracle);
        if got != want {
            disagreements.push(format!(
                "{}\n      mine={got:?}\n    oracle={want:?}",
                case.what
            ));
        }
    }
    // A disagreement here is NEWS, not noise, and is never softened into "both
    // rejected". If the disagreement is that OURS rejected and the reference
    // accepted, `the_shipped_verifier_is_never_more_permissive_than_the_incumbent`
    // will still be green -- that pair of results means aterm is being STRICTER,
    // which is safe, and the header's 'One observed instability' section is the
    // first thing to read.
    assert!(
        disagreements.is_empty(),
        "{} of {} cases disagree:\n  {}",
        disagreements.len(),
        corpus().count(),
        disagreements.join("\n  ")
    );
}

#[test]
fn the_shipped_verifier_matches_its_recorded_expectations() {
    // Independent of the oracle ON PURPOSE: if `rustls-platform-verifier` is
    // ever dropped from the dev-dependencies, this table is what still says what
    // each input must do. Every value in it was MEASURED, none predicted.
    //
    // The `accept` column is asserted on every platform. The `apple` column is
    // asserted only on Apple, because that is the only platform this corpus was
    // measured on -- writing down a guess at Windows's or webpki's error
    // vocabulary would be manufacturing evidence, which is the one thing a
    // certificate-verification suite must never do.
    let Some((mine, _oracle)) = pair() else {
        eprintln!("SKIP: no platform verifier on this target");
        return;
    };
    let mut wrong = Vec::new();
    for case in corpus() {
        let got = case.run(&mine);
        if got.accepted() != case.accept {
            wrong.push(format!(
                "{}: accept={} but expected accept={}  ({got:?})",
                case.what,
                got.accepted(),
                case.accept
            ));
            continue;
        }
        if cfg!(target_vendor = "apple") && !case.apple.matches(&got) {
            wrong.push(format!(
                "{}: got {got:?}, recorded {:?}",
                case.what, case.apple
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} recorded expectations are stale:\n  {}",
        wrong.len(),
        wrong.join("\n  ")
    );
}

#[test]
fn the_positive_control_is_armed_and_not_inert() {
    // THE GUARD ON THE GUARD, and the reason this file is an oracle rather than
    // a decoration.
    //
    // Everything else here can be satisfied by a verifier that returns `Err` for
    // every input -- UNLESS something in the corpus must ACCEPT. Two captured
    // production chains, from two independent roots, are what supply that. If
    // BOTH stop validating, the suite silently degenerates into a reject-only
    // shell that the worst possible bug would sail straight through.
    //
    // So this fails LOUDLY rather than skipping. It fails on the ORACLE's
    // verdict, never on ours, so it can never be confused with a defect in the
    // reimplementation: it says "this machine can no longer arm this suite",
    // which is a fact about the machine and a job for a human.
    let Some((mine, oracle)) = pair() else {
        eprintln!("SKIP: the oracle verifier could not be constructed on this platform");
        return;
    };
    let accepting: Vec<&str> = REAL
        .iter()
        .filter(|case| case.accept && case.run(&oracle).accepted())
        .map(|case| case.what)
        .collect();
    assert!(
        accepting.len() >= 2,
        "THE POSITIVE CONTROL IS INERT. The reference implementation accepted \
         {} of the {} cases this suite relies on to detect a reject-everything \
         verifier. Every other test in this file would still pass with \
         `verify_server_cert` replaced by `Err(..)`.\n\
         Most likely cause: a captured chain's root (Sectigo Public Server \
         Authentication Root E46, or Google Trust Services GTS Root R1) has left \
         this machine's trust store, or the platform has tightened policy \
         against a chain captured on 2026-08-29. Recapture procedure: \
         src/testdata/tls/README.md and mint.sh.",
        accepting.len(),
        REAL.iter().filter(|case| case.accept).count()
    );

    // Both independent roots must be live, not just one of them. This is what
    // makes the control survive a single CA leaving the store: it is reported
    // here, on the day it happens, rather than discovered when the second one
    // goes too.
    for (host, chain) in [("github.com", GH), ("downloads.claude.ai", CL)] {
        let case = Case {
            what: "root liveness",
            chain,
            server: host,
            now: T_REAL,
            accept: true,
            apple: Detail::Accept,
        };
        assert!(
            case.run(&oracle).accepted(),
            "the {host} chain no longer validates against this machine's trust store; the \
             positive control is down to one root. See src/testdata/tls/README.md."
        );
        // And the shipped verifier must agree. This is the assertion a
        // reject-everything implementation dies on.
        assert!(
            case.run(&mine).accepted(),
            "the system trust store validated the {host} chain for the reference \
             implementation but NOT for the shipped one. The two differ in that the shipped \
             verifier forbids network fetching during evaluation \
             (SecTrustSetNetworkFetchAllowed(false)) while the reference permits it, so a cold \
             platform trust cache is one candidate: check whether the chain validates with \
             `security verify-cert`. See this file's header, 'One observed instability'."
        );
    }
}

#[test]
fn the_corpus_is_armed_in_both_directions() {
    // THE ANTI-DEGENERACY COUNT, asserted so the prose in this file's header
    // cannot drift away from the tables below it. (It already did once: the
    // header claimed eight ACCEPT cases when there were seven. Hence this test.)
    //
    // A verifier that rejected EVERYTHING would pass every reject case; one that
    // accepted everything would pass every accept case. Both counts must stay
    // non-trivial, or one of those two degenerate implementations goes green.
    let accepts = corpus().filter(|case| case.accept).count();
    let rejects = corpus().filter(|case| !case.accept).count();
    assert_eq!(
        (accepts, rejects),
        (7, 30),
        "the corpus changed shape; update this file's header to match"
    );
    // Every ACCEPT lives on a captured production chain, because a real system
    // anchor is the ONLY thing that can accept anything in a shipped build. If
    // that ever stops holding, something has been handed trust anchors it should
    // not have.
    assert_eq!(
        REAL.iter().filter(|case| case.accept).count(),
        accepts,
        "an ACCEPT case appeared outside the captured real chains"
    );
    assert_eq!(
        LOCAL.iter().filter(|case| case.accept).count(),
        0,
        "a locally minted fixture is expected to be ACCEPTED by the shipped verifier"
    );
}

#[test]
fn a_real_chain_is_accepted_only_for_the_names_it_carries() {
    // ARMING for the name-mismatch cases: the rejections below are paired with
    // acceptances over the SAME bytes at the SAME instant, differing only in the
    // hostname asked for. A rejection that were happening because the DER never
    // parsed, or because the verifier refuses everything, would take the
    // acceptance with it.
    let Some((mine, oracle)) = pair() else {
        eprintln!("SKIP: no platform verifier on this target");
        return;
    };
    let at = |chain, server| Case {
        what: "name",
        chain,
        server,
        now: T_REAL,
        accept: true,
        apple: Detail::Accept,
    };
    for (chain, carried, foreign) in [
        (GH, ["github.com", "www.github.com"], "downloads.claude.ai"),
        (
            CL,
            ["downloads.claude.ai", "downloads.claude.com"],
            "github.com",
        ),
    ] {
        // Equality holds unconditionally -- a machine whose trust store cannot
        // validate these chains flips BOTH implementations identically, and that
        // is still worth asserting.
        for name in carried {
            assert_eq!(
                at(chain, name).run(&mine),
                at(chain, name).run(&oracle),
                "verdicts diverge for {name}"
            );
        }
        assert_eq!(
            at(chain, foreign).run(&mine),
            at(chain, foreign).run(&oracle),
            "verdicts diverge for {foreign}"
        );
        // The hard assertions are gated on the oracle agreeing the machine can
        // validate this chain at all; `the_positive_control_is_armed_and_not_inert`
        // is what fails loudly if it cannot.
        if !at(chain, carried[0]).run(&oracle).accepted() {
            eprintln!(
                "SKIP (hard assertion only): {} does not validate here",
                carried[0]
            );
            continue;
        }
        for name in carried {
            assert!(
                at(chain, name).run(&mine).accepted(),
                "a real chain was refused for {name}, a name it carries"
            );
        }
        let got = at(chain, foreign).run(&mine);
        assert_eq!(
            got,
            Outcome::NotValidForName,
            "a real chain was not refused with NotValidForName for {foreign}, a name it does \
             not carry"
        );
    }
}

#[test]
fn a_real_chain_is_accepted_only_inside_its_validity_window() {
    // ARMING for the temporal cases, and the proof that BOTH implementations
    // honour the `now` argument rather than reading the clock. The same bytes,
    // the same name, three instants: accept inside the window, reject sixteen
    // years early, reject fourteen years late.
    //
    // This is what lets the captured fixtures outlive their own notAfter. It is
    // also the only reason this file is not a test that starts failing in
    // October 2026 and gets deleted by whoever is on call.
    let Some((mine, oracle)) = pair() else {
        eprintln!("SKIP: no platform verifier on this target");
        return;
    };
    for (host, chain) in [("github.com", GH), ("downloads.claude.ai", CL)] {
        let at = |now| Case {
            what: "time",
            chain,
            server: host,
            now,
            accept: true,
            apple: Detail::Accept,
        };
        for now in [T_REAL, T_2010, T_2040] {
            assert_eq!(
                at(now).run(&mine),
                at(now).run(&oracle),
                "verdicts diverge for {host} at now={now}"
            );
        }
        if !at(T_REAL).run(&oracle).accepted() {
            eprintln!("SKIP (hard assertion only): {host} does not validate here");
            continue;
        }
        assert!(at(T_REAL).run(&mine).accepted());
        for now in [T_2010, T_2040] {
            let got = at(now).run(&mine);
            assert!(
                !got.accepted(),
                "{host} was accepted at now={now}, far outside its leaf's window: the verify \
                 date is not reaching the platform"
            );
            // Specifically errSecCertificateExpired, from the trust evaluation
            // -- not a parse failure and not "no anchor". macOS reports the
            // same code for not-yet-valid, which is why the assertion is on the
            // code and not on a claim to tell the two apart.
            #[cfg(target_vendor = "apple")]
            assert_eq!(
                got.status(),
                Some(-67818),
                "{host} at now={now} was refused, but not by the date check: {got:?}"
            );
        }
    }
}

#[test]
fn certificate_order_is_not_a_suggestion() {
    // ARMING for the ordering cases. `SecTrustCreateWithCertificates` takes the
    // end-entity FIRST; hand it the array backwards and the root is evaluated as
    // a leaf. A verifier that sorted, deduplicated or searched the array would
    // accept both orders -- and would then accept a chain where an attacker
    // controls which certificate is treated as the end-entity.
    let Some((mine, oracle)) = pair() else {
        eprintln!("SKIP: no platform verifier on this target");
        return;
    };
    for (host, forward, backward) in [
        ("github.com", GH, &[GH_INT1, GH_INT0, GH_LEAF] as &[&[u8]]),
        (
            "downloads.claude.ai",
            CL,
            &[CL_INT1, CL_INT0, CL_LEAF] as &[&[u8]],
        ),
    ] {
        let case = |chain| Case {
            what: "order",
            chain,
            server: host,
            now: T_REAL,
            accept: true,
            apple: Detail::Accept,
        };
        assert_eq!(
            case(backward).run(&mine),
            case(backward).run(&oracle),
            "reversed-order verdicts diverge for {host}"
        );
        if !case(forward).run(&oracle).accepted() {
            eprintln!("SKIP (hard assertion only): {host} does not validate here");
            continue;
        }
        assert!(
            case(forward).run(&mine).accepted(),
            "the forward order must accept, or the reversed case proves nothing"
        );
        assert!(
            !case(backward).run(&mine).accepted(),
            "{host}'s chain was accepted with the certificates reversed: the array's ORDER is \
             not being respected"
        );
    }
}

#[test]
fn no_locally_minted_chain_is_trusted_by_the_shipped_verifier() {
    // The seam assertion, in eighteen shapes. `PlatformVerifier::new` is exactly
    // what `tls::client_config` calls, and it must know nothing about the
    // fixture CA -- otherwise the test anchor would be a live trust override in
    // a shipped binary.
    //
    // The STRUCTURAL half of that guarantee is stronger than anything asserted
    // here and is enforced by the compiler: `new_with_extra_roots` is
    // `#[cfg(test)]`, which is precisely why this integration test cannot call
    // it. This is the behavioural half.
    //
    // Each fixture is evaluated at a `now` inside its own window and asked for
    // the name it carries, so a rejection cannot be blamed on expiry or on the
    // hostname -- the missing anchor is the only variable left. That those
    // chains are otherwise GOOD is proved in `src/verifier/tests.rs`, where the
    // anchor is supplied and they accept.
    let Some((mine, oracle)) = pair() else {
        eprintln!("SKIP: no platform verifier on this target");
        return;
    };
    for case in LOCAL {
        let got = case.run(&mine);
        assert!(
            !got.accepted(),
            "the SHIPPED verifier accepted a chain rooted in a throwaway fixture CA: {}",
            case.what
        );
        assert_eq!(
            got,
            case.run(&oracle),
            "unanchored verdicts diverge: {}",
            case.what
        );
    }
}

#[test]
fn a_parse_failure_is_reported_as_one_and_never_as_a_trust_verdict() {
    // THE WRONG-REASON CONTROL. Bytes that are not a certificate never reach
    // trust evaluation at all, so a `BadEncoding` proves nothing whatsoever
    // about trust. If it were reported as an ordinary rejection, a change that
    // broke DER handling would present as a suite full of healthy-looking reject
    // cases -- which is exactly failure mode 2.
    //
    // So it is asserted as `BadEncoding` SPECIFICALLY, on both sides, and it is
    // the only case in this file allowed to be one.
    let Some((mine, oracle)) = pair() else {
        eprintln!("SKIP: no platform verifier on this target");
        return;
    };
    for (what, bytes) in [
        ("nine bytes of garbage", MALFORMED.to_vec()),
        ("no bytes at all", Vec::new()),
        ("a truncated certificate", GOOD[..GOOD.len() / 2].to_vec()),
        (
            "a certificate missing its last byte",
            GOOD[..GOOD.len() - 1].to_vec(),
        ),
        ("a certificate with a byte in front of it", {
            let mut shifted = vec![0u8];
            shifted.extend_from_slice(GOOD);
            shifted
        }),
    ] {
        let end_entity = CertificateDer::from(bytes);
        let name = ServerName::try_from(HOST).expect("a usable server name");
        let at = UnixTime::since_unix_epoch(Duration::from_secs(T26));
        let got = reduce(&mine.verify_server_cert(&end_entity, &[], &name, &[], at));
        let want = reduce(&oracle.verify_server_cert(&end_entity, &[], &name, &[], at));
        assert_eq!(
            got,
            Outcome::BadEncoding,
            "{what} must be reported as a PARSE failure, not as a trust verdict"
        );
        assert_eq!(got, want, "{what}: parse-failure reporting diverges");
    }

    // ...and no case that DOES reach trust evaluation may report `BadEncoding`,
    // or the control above would be indistinguishable from the rest of the
    // suite.
    let Some((mine, _)) = pair() else { return };
    for case in corpus() {
        if case.chain == [MALFORMED] {
            continue;
        }
        assert_ne!(
            case.run(&mine),
            Outcome::BadEncoding,
            "{} was refused by the DER parser rather than by the trust evaluation, so it \
             proves nothing about trust",
            case.what
        );
    }
}

#[test]
fn trailing_bytes_after_a_certificate_are_a_platform_quirk_and_both_sides_share_it() {
    // A MEASURED SURPRISE, recorded rather than smoothed over.
    //
    // Appending arbitrary bytes to a DER certificate does NOT make it
    // unparseable on macOS: `SecCertificateCreateWithData` ignores what follows
    // the outermost SEQUENCE. Measured on m21 with 1, 2, 8 and 64 trailing
    // bytes, of both `0x00` and `0xAA`, against a fixture leaf and against a
    // captured production leaf: every one parsed as the certificate it wraps and
    // went on to ordinary trust evaluation.
    //
    //     good.der + 64 trailing 0xAA   mine=UnknownIssuer  oracle=UnknownIssuer
    //     gh-leaf.der + 16 trailing 0x00  parsed as github.com's leaf on both sides
    //
    // Truncation and a LEADING byte are both rejected as `BadEncoding`; only
    // trailing data is tolerated. This is macOS's parser, not aterm's code, and
    // the retired crate does exactly the same thing because it makes the same
    // call -- so it is not a change this reimplementation introduced. It is
    // recorded because it means "the peer's certificate bytes" are not canonical
    // on this platform, and anyone who ever compares certificate DER for
    // IDENTITY rather than parsing it first needs to know that.
    //
    // The portable half is asserted everywhere; the "it is not a parse failure"
    // half is Apple-specific, because `rustls-webpki` (the Unix arm) is strict
    // about trailing data and would say `BadEncoding`.
    let Some((mine, oracle)) = pair() else {
        eprintln!("SKIP: no platform verifier on this target");
        return;
    };
    let name = ServerName::try_from(HOST).expect("a usable server name");
    let at = UnixTime::since_unix_epoch(Duration::from_secs(T26));
    for filler in [0x00u8, 0xAA] {
        for count in [1usize, 8, 64] {
            let mut bytes = GOOD.to_vec();
            bytes.extend(std::iter::repeat_n(filler, count));
            let end_entity = CertificateDer::from(bytes);
            let got = reduce(&mine.verify_server_cert(&end_entity, &[], &name, &[], at));
            let want = reduce(&oracle.verify_server_cert(&end_entity, &[], &name, &[], at));
            assert_eq!(
                got, want,
                "the two implementations disagree about a certificate with {count} trailing \
                 {filler:#04x} bytes"
            );
            assert!(
                !got.accepted(),
                "a fixture chain with {count} trailing {filler:#04x} bytes was ACCEPTED"
            );
            #[cfg(target_vendor = "apple")]
            assert_eq!(
                got,
                Outcome::UnknownIssuer,
                "macOS's tolerance of {count} trailing {filler:#04x} bytes has changed; the \
                 certificate now fails somewhere other than trust evaluation"
            );
        }
    }
}

#[test]
fn the_shipped_verifier_is_never_more_permissive_than_the_incumbent() {
    // The security-relevant HALF of the differential, asserted separately so it
    // survives the one place the two implementations are allowed to differ (see
    // `a_missing_intermediate_is_not_repaired_by_a_network_fetch`).
    //
    // Equality is the demand everywhere else. This is the invariant that must
    // hold even where equality is deliberately broken: aterm may refuse what the
    // retired crate accepted, but it must NEVER accept what the retired crate
    // refused.
    let Some((mine, oracle)) = pair() else {
        eprintln!("SKIP: no platform verifier on this target");
        return;
    };
    let mut extra: Vec<&str> = Vec::new();
    let leaf_only: &[Case] = &[
        Case {
            what: "github.com's leaf with NO intermediate",
            chain: &[GH_LEAF],
            server: "github.com",
            now: T_REAL,
            accept: false,
            apple: Detail::Kind(Kind::UnknownIssuer),
        },
        Case {
            what: "downloads.claude.ai's leaf with NO intermediate",
            chain: &[CL_LEAF],
            server: "downloads.claude.ai",
            now: T_REAL,
            accept: false,
            apple: Detail::Kind(Kind::UnknownIssuer),
        },
        Case {
            what: "github.com's leaf with the WRONG chain's intermediates",
            chain: &[GH_LEAF, CL_INT0, CL_INT1],
            server: "github.com",
            now: T_REAL,
            accept: false,
            apple: Detail::Kind(Kind::UnknownIssuer),
        },
    ];
    for case in corpus().chain(leaf_only.iter()) {
        if case.run(&mine).accepted() && !case.run(&oracle).accepted() {
            extra.push(case.what);
        }
    }
    assert!(
        extra.is_empty(),
        "the shipped verifier ACCEPTED chains the reference implementation refused:\n  {}",
        extra.join("\n  ")
    );
}

#[test]
fn a_missing_intermediate_is_not_repaired_by_a_network_fetch() {
    // THE ONE MEASURED DIVERGENCE, and a deliberate one.
    //
    // `verifier/apple.rs` calls `SecTrustSetNetworkFetchAllowed(false)`;
    // `rustls-platform-verifier` 0.6.2 does not. Both captured leaves carry an
    // `authorityInformationAccess` CA-Issuers URI, so when the intermediate is
    // withheld macOS can go and DOWNLOAD it -- inside a TLS handshake that
    // already has a deadline. Measured on m21, five consecutive runs, identical
    // every time:
    //
    //     github.com leaf alone      mine=UnknownIssuer   oracle=ACCEPT
    //     claude.ai  leaf alone      mine=UnknownIssuer   oracle=ACCEPT
    //     gh leaf + claude's ints    mine=UnknownIssuer   oracle=ACCEPT
    //
    // The oracle is being MORE permissive, so this is the safe direction, and
    // `the_shipped_verifier_is_never_more_permissive_than_the_incumbent` covers
    // these same inputs from the security side.
    //
    // What is asserted here is only the direction, never `oracle == ACCEPT`: on
    // a host with no network and a cold `trustd` cache the oracle would refuse
    // too, and both would agree. A test that demanded the fetch SUCCEED would be
    // a test that fails when the machine is offline, which is the opposite of
    // what this suite is for.
    let Some((mine, oracle)) = pair() else {
        eprintln!("SKIP: no platform verifier on this target");
        return;
    };
    // The premise is checkable: these leaves must actually carry an AIA
    // extension, or "we refused to fetch" would be an empty claim.
    // 1.3.6.1.5.5.7.1.1, as encoded inside an X.509 extension's OID.
    const AIA: &[u8] = &[0x06, 0x08, 0x2B, 0x06, 0x01, 0x05, 0x05, 0x07, 0x01, 0x01];
    for (name, bytes) in [("gh-leaf", GH_LEAF), ("cl-leaf", CL_LEAF)] {
        assert!(
            bytes.windows(AIA.len()).any(|window| window == AIA),
            "{name}.der carries no authorityInformationAccess, so withholding its issuer does \
             not test network-fetch suppression at all"
        );
    }
    for (what, chain, host) in [
        ("github.com", &[GH_LEAF] as &[&[u8]], "github.com"),
        (
            "downloads.claude.ai",
            &[CL_LEAF] as &[&[u8]],
            "downloads.claude.ai",
        ),
    ] {
        let case = Case {
            what,
            chain,
            server: host,
            now: T_REAL,
            accept: false,
            apple: Detail::Kind(Kind::UnknownIssuer),
        };
        let got = case.run(&mine);
        assert!(
            !got.accepted(),
            "{what}: a chain missing its intermediate was ACCEPTED. Either the intermediate \
             was fetched over the network mid-verification, or it came from a platform cache \
             -- both mean SecTrustSetNetworkFetchAllowed(false) is not in effect: {got:?}"
        );
        if case.run(&oracle).accepted() {
            eprintln!(
                "note: the reference implementation ACCEPTED {what} without its intermediate \
                 (it permits the AIA fetch); the shipped verifier refused it"
            );
        }
    }
}

#[test]
fn nothing_in_this_suite_reads_the_wall_clock() {
    // FAILURE MODE 3, asserted rather than asserted-about. Every verdict in the
    // corpus is recomputed with `now` held fixed while the machine's clock is
    // irrelevant -- so the only way a result could move with real time is if an
    // implementation ignored the `now` argument and called
    // `SystemTime::now()` itself.
    //
    // Proving that directly is not possible without changing the clock, so what
    // is proved instead is the property that matters: the corpus's verdicts are
    // a pure function of (chain, name, now), and a `now` that moves changes them
    // in the way the certificates say it should. If either implementation
    // consulted the wall clock, the two instants below would not produce
    // different answers over identical bytes.
    let Some((mine, oracle)) = pair() else {
        eprintln!("SKIP: no platform verifier on this target");
        return;
    };
    let at = |now| Case {
        what: "clock",
        chain: GH,
        server: "github.com",
        now,
        accept: true,
        apple: Detail::Accept,
    };
    let inside = at(T_REAL);
    let outside = at(T_2010);
    if !inside.run(&oracle).accepted() {
        eprintln!("SKIP: the captured chain does not validate on this machine");
        return;
    }
    assert!(inside.run(&mine).accepted());
    assert!(!outside.run(&mine).accepted());
    assert!(!outside.run(&oracle).accepted());

    // Repeated evaluation is stable: the same inputs give the same answer, so
    // no verdict here is a function of how long the test took to reach it.
    for _ in 0..3 {
        assert!(inside.run(&mine).accepted());
        assert_eq!(outside.run(&mine), outside.run(&oracle));
    }

    // The captured leaves HAVE expired-by-now dates in the real world; the
    // whole corpus works anyway because `now` is pinned. If this file ever
    // starts failing on a date rather than on a change, that is a bug in the
    // file and not a reason to delete it.
    let wall = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    assert!(
        inside.run(&mine).accepted(),
        "the pinned-instant verdict changed; wall clock is {wall}"
    );
}

#[test]
fn every_committed_fixture_is_exercised_by_this_corpus() {
    // A fixture added to `src/testdata/tls/` and never driven through the oracle
    // is a certificate nobody tested. This walks the directory so the corpus
    // cannot quietly fall behind the fixture set.
    //
    // Proved non-vacuous: dropping a `zz-untested.der` into that directory turns
    // this red and names the file; removing it turns it green again.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/testdata/tls");
    let mut missing = Vec::new();
    let mut seen = 0usize;
    let entries = std::fs::read_dir(&dir).expect("the fixture directory must exist");
    for entry in entries {
        let path = entry.expect("a readable directory entry").path();
        if path.extension().is_none_or(|extension| extension != "der") {
            continue;
        }
        seen += 1;
        let bytes = std::fs::read(&path).expect("a readable fixture");
        if !corpus().any(|case| case.chain.contains(&bytes.as_slice())) {
            missing.push(
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned()),
            );
        }
    }
    assert!(
        missing.is_empty(),
        "these committed fixtures are never driven through the differential: {missing:?}"
    );
    // ...and the walk really did read the corpus, rather than passing because it
    // found an empty directory.
    let distinct = corpus()
        .flat_map(|case| case.chain.iter())
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    assert_eq!(
        seen, distinct,
        "the fixture directory holds {seen} certificates but the corpus names {distinct}"
    );
}

// ---------------------------------------------------------------------------
// FAIL-CLOSED PATHS the differential cannot reach
//
// A differential can only compare inputs both implementations accept as
// well-formed. These are the paths where the answer must be "refuse" before any
// comparison is possible -- and each one is a place where a plausible
// implementation would instead succeed with an empty or default trust set,
// which is the failure that matters: not refusing a good connection, but
// trusting one that nothing anchors.
// ---------------------------------------------------------------------------

#[test]
fn an_empty_root_store_is_refused_rather_than_silently_trusting_nothing() {
    use aterm_http::Trust;

    // A `Trust::Roots` bundle with nothing in it must be an ERROR, not a
    // verifier that rejects everything and not one that falls back to the
    // platform anchors. Both fallbacks are worse than failing: the first turns a
    // configuration mistake into a mysterious outage, the second turns an
    // operator's explicit pin into no pin at all.
    let error = aterm_http::tls::client_config(&Trust::Roots(Vec::new()))
        .expect_err("an empty CA bundle must not build a client configuration");
    assert!(
        error.contains("no usable certificates"),
        "the empty-bundle error must say what was wrong: {error}"
    );

    // The same shape from the other direction: a bundle whose every entry is
    // unusable is empty after decoding, and must fail the same way rather than
    // quietly yielding a store with fewer roots than the operator listed.
    let error = aterm_http::tls::client_config(&Trust::Roots(vec![b"not a certificate".to_vec()]))
        .expect_err("a bundle of garbage must not build a client configuration");
    assert!(error.contains("CA bundle"), "{error}");

    // And the platform arm has its own empty-store failure, one this test
    // cannot reach because it needs control of `$SSL_CERT_FILE`:
    // `verifier::unix::tests::an_empty_system_store_is_a_loud_failure_not_a_silent_verifier`.
    // On Apple the store is the Keychain and cannot be emptied by a test at all.
}

#[test]
fn an_unparseable_ca_bundle_is_refused_rather_than_silently_empty() {
    use aterm_http::pem;

    // THE STRICT PARSER, used for the operator's `Trust::Roots` override. Every
    // one of these must be an error rather than a shorter list of roots: a
    // bundle that silently loses its entries is a pin that silently stops
    // pinning.
    for (what, text) in [
        ("nothing at all", ""),
        ("prose", "this is not a certificate"),
        (
            "a header with no footer",
            "-----BEGIN CERTIFICATE-----\nQUJD\n",
        ),
        (
            "a footer with no header",
            "QUJD\n-----END CERTIFICATE-----\n",
        ),
        (
            "base64 that is not base64",
            "-----BEGIN CERTIFICATE-----\n!!!! not base64 !!!!\n-----END CERTIFICATE-----\n",
        ),
    ] {
        assert!(
            pem::decode_certificates(text).is_err(),
            "the strict PEM parser accepted {what}"
        );
    }

    // A key where a certificate should be. The PEM label is ASSEMBLED rather
    // than written out, because `tools/grep_guard.sh`'s B6 check greps the whole
    // tracked tree for private-key headers and a literal one here would trip it.
    // The guard is right and stays as it is; this is the documented way round
    // it (see `crates/atpkg-keys`, which does the same).
    let wrong_label = format!(
        "-----BEGIN {label}-----\nQUJD\n-----END {label}-----\n",
        label = "PRIVATE KEY"
    );
    assert!(
        pem::decode_certificates(&wrong_label).is_err(),
        "the strict PEM parser accepted a key where a certificate should be"
    );
    assert!(
        pem::decode_certificates_lossy(&wrong_label).is_empty(),
        "the tolerant PEM parser accepted a key where a certificate should be"
    );

    // A well-formed PEM whose payload is not a certificate parses as PEM but
    // must be refused by the trust builder -- the layer below is where DER is
    // judged, and it must not be skipped.
    let junk_pem = "-----BEGIN CERTIFICATE-----\nQUJDREVGRw==\n-----END CERTIFICATE-----\n";
    let ders = pem::decode_certificates(junk_pem).expect("well-formed PEM decodes");
    assert_eq!(
        ders.len(),
        1,
        "the PEM layer must hand on exactly what it saw"
    );
    let error = aterm_http::tls::client_config(&aterm_http::Trust::Roots(ders))
        .expect_err("PEM-shaped garbage must not become a trust anchor");
    assert!(error.contains("CA bundle"), "{error}");

    // The TOLERANT parser exists only for reading a Linux distribution's system
    // store, where a single unreadable file must not take the whole store with
    // it. It must still never invent a certificate: garbage in, nothing out.
    for text in [
        "",
        "prose",
        "-----BEGIN CERTIFICATE-----\n!!!\n-----END CERTIFICATE-----\n",
    ] {
        assert!(
            pem::decode_certificates_lossy(text).is_empty(),
            "the tolerant parser produced a certificate from {text:?}"
        );
    }

    // ...and an empty result from the tolerant parser must still fail closed
    // when it reaches the trust builder, rather than becoming an empty store.
    assert!(
        aterm_http::tls::client_config(&aterm_http::Trust::Roots(pem::decode_certificates_lossy(
            "prose"
        )))
        .is_err(),
        "a store the tolerant parser could not fill must not build a verifier"
    );

    // A real certificate mixed in with junk the tolerant parser skips must
    // still come through, or "tolerant" would just mean "broken".
    let mixed = format!(
        "garbage before\n-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\ngarbage after\n",
        base64(ROOT)
    );
    assert_eq!(
        pem::decode_certificates_lossy(&mixed),
        vec![ROOT.to_vec()],
        "the tolerant parser lost a valid certificate that was surrounded by junk"
    );
}

#[test]
fn a_hostname_that_is_not_a_valid_dns_name_never_reaches_the_verifier() {
    // The name is checked BEFORE any socket or any certificate. `Stream::start_tls`
    // (`src/stream.rs:177`) is the shipped entry point and its very first act is
    // `ServerName::try_from`, so a name that cannot be a DNS name or an IP
    // address cannot become a verification against an empty or absent hostname
    // -- which on macOS is the difference between name checking and no name
    // checking at all (`SecPolicyCreateSSL`'s hostname argument is NULLABLE and
    // a NULL switches the check OFF).
    //
    // Every one of these is REJECTED by `rustls` before the verifier exists.
    // MEASURED, not assumed: the list was run and each returned "invalid dns
    // name".
    for name in [
        "",
        ".",
        "*.github.com",
        "git hub.com",
        "github.com\u{0}",
        "-github.com",
        "github..com",
        "github.com:443",
        "[::1]",
        "münchen.de",
        // U+2024 ONE DOT LEADER, the homoglyph a label-confusion attack uses.
        "github\u{2024}com",
        "0x7f.0.0.1",
        "http://github.com",
        "github.com/",
        " github.com",
    ] {
        assert!(
            ServerName::try_from(name).is_err(),
            "rustls accepted {name:?} as a server name; it would reach the verifier as a \
             hostname string"
        );
    }

    // ...and the shipped TLS entry point turns that into a refusal rather than a
    // panic or a nameless verification. A real socket is used because
    // `start_tls` takes one -- but the name is rejected before a single byte
    // moves, so the listener never even accepts.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
    let address = listener.local_addr().expect("a bound address");
    let config = aterm_http::tls::client_config(&aterm_http::Trust::PlatformVerifier)
        .expect("the platform verifier builds");
    for name in ["", "git hub.com", "github\u{2024}com"] {
        let tcp = std::net::TcpStream::connect(address).expect("a loopback connection");
        let started = aterm_http::stream::Stream::start_tls(
            tcp,
            Arc::clone(&config),
            name,
            Arc::new(aterm_http::AlwaysAuthorized),
            aterm_http::Deadline::after(Duration::from_secs(5)),
        );
        // `Stream` is deliberately not `Debug` (it owns a live socket), so the
        // success arm is matched rather than unwrapped.
        let Err(error) = started else {
            panic!("start_tls began a handshake for the unusable server name {name:?}");
        };
        assert_eq!(
            error.kind(),
            std::io::ErrorKind::InvalidInput,
            "{name:?} was refused, but not as an invalid name: {error}"
        );
    }

    // A name rustls DOES accept but the certificate does not carry must be
    // rejected by the VERIFIER, which is the other half of the same guarantee:
    // the ones that get through name parsing still have to match.
    let Some((mine, oracle)) = pair() else {
        eprintln!("SKIP (verifier half only): no platform verifier on this target");
        return;
    };
    for name in ["localhost", "127.0.0.1", "::1", "example.invalid"] {
        let case = Case {
            what: "unmatched but parseable name",
            chain: GH,
            server: name,
            now: T_REAL,
            accept: false,
            apple: Detail::Kind(Kind::NotValidForName),
        };
        let got = case.run(&mine);
        assert!(
            !got.accepted(),
            "a real chain was accepted for {name:?}, which it does not carry"
        );
        assert_eq!(got, case.run(&oracle), "verdicts diverge for {name:?}");
        assert_eq!(
            got,
            Outcome::NotValidForName,
            "{name:?} was refused, but not by the NAME check: {got:?}"
        );
    }

    // Two names rustls accepts that DO match, recorded because they look wrong
    // and are not: DNS matching is case-insensitive, and a fully-qualified name
    // with a trailing root dot is the same name. Both implementations agree.
    for name in ["GITHUB.COM", "github.com."] {
        let case = Case {
            what: "an odd spelling of a name the chain carries",
            chain: GH,
            server: name,
            now: T_REAL,
            accept: true,
            apple: Detail::Accept,
        };
        assert_eq!(
            case.run(&mine),
            case.run(&oracle),
            "verdicts diverge for {name:?}"
        );
    }
}

/// Minimal base64 for building a PEM block in [`an_unparseable_ca_bundle_is_refused_rather_than_silently_empty`].
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let mut buffer = [0u8; 3];
        buffer[..chunk.len()].copy_from_slice(chunk);
        let packed =
            (u32::from(buffer[0]) << 16) | (u32::from(buffer[1]) << 8) | u32::from(buffer[2]);
        for index in 0..4 {
            if index <= chunk.len() {
                let shift = 18 - index * 6;
                out.push(char::from(ALPHABET[((packed >> shift) & 0x3F) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}
