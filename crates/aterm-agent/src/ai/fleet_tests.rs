// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::ai::testing::FakeFetcher;

const URL: &str = "https://fleet.example/ai-fleet.toml";
const NOW: i64 = 1_800_000_000;

struct Signer {
    secret: Vec<u8>,
    public: String,
}

fn signer() -> Signer {
    let (secret, public) = keygen().expect("keygen");
    Signer { secret, public }
}

fn serve(net: &FakeFetcher, by: &Signer, text: &str) {
    net.serve(URL, text.as_bytes());
    let sig = sign(&by.secret, text.as_bytes()).expect("sign");
    net.serve(&format!("{URL}.sig"), sig.as_bytes());
}

#[test]
fn a_signature_verifies_only_its_own_bytes_under_its_own_key() {
    let a = signer();
    let doc = b"schema = 1\nseq = 1\n";
    let sig = sign(&a.secret, doc).expect("sign");
    assert_eq!(verify(doc, sig.as_bytes(), &a.public), Ok(()));
    let raw = sig_bytes(sig.as_bytes()).expect("64 bytes");
    assert_eq!(verify(doc, &raw, &a.public), Ok(()));
    assert!(verify(b"schema = 1\nseq = 2\n", sig.as_bytes(), &a.public).is_err());
    assert!(verify(doc, sig.as_bytes(), &signer().public).is_err());
    assert!(verify(doc, b"not a signature", &a.public).is_err());
    assert!(verify(doc, sig.as_bytes(), "short").is_err());
    assert_eq!(public_of(&a.secret).as_deref(), Ok(a.public.as_str()));
}

#[test]
fn seq_and_valid_until_are_read_from_the_top_level() {
    assert_eq!(meta("schema = 1\n"), Ok((None, None)));
    assert_eq!(
        meta("schema = 1\nseq = 4\nvalid_until = 1900000000\n"),
        Ok((Some(4), Some(1_900_000_000)))
    );
    assert_eq!(
        meta("schema = 1\nvalid_until = \"2030-01-01T00:00:00Z\"\n").map(|m| m.1),
        Ok(Some(1_893_456_000))
    );
    assert!(meta("seq = -1\n").is_err());
    assert!(meta("valid_until = \"soon\"\n").is_err());
}

#[test]
fn a_fetch_verifies_holds_and_reads_back() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let net = FakeFetcher::default();
    let a = signer();
    serve(&net, &a, "schema = 1\nseq = 2\npreferred = \"codex\"\n");
    assert_eq!(
        held(dir.path(), URL, &a.public, NOW),
        Err("not fetched yet".into())
    );
    assert_eq!(
        fetch(dir.path(), URL, &a.public, NOW, &net, false),
        Fetched::Fresh(2)
    );
    assert_eq!(net.asked(), [URL.to_string(), format!("{URL}.sig")]);
    let text = held(dir.path(), URL, &a.public, NOW).expect("held");
    assert!(text.contains("preferred = \"codex\""), "{text}");
    let h = held_state(dir.path()).expect("state");
    assert_eq!((h.seq, h.fetched_at, h.error), (Some(2), Some(NOW), None));
    assert!(held(dir.path(), URL, &signer().public, NOW).is_err());
}

#[test]
fn within_six_hours_nothing_is_asked_unless_forced() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let net = FakeFetcher::default();
    let a = signer();
    serve(&net, &a, "schema = 1\nseq = 1\n");
    assert_eq!(
        fetch(dir.path(), URL, &a.public, NOW, &net, false),
        Fetched::Fresh(1)
    );
    let later = NOW + FETCH_EVERY_S - 1;
    assert_eq!(
        fetch(dir.path(), URL, &a.public, later, &net, false),
        Fetched::NotDue
    );
    assert_eq!(net.asked().len(), 2);
    assert_eq!(
        fetch(dir.path(), URL, &a.public, later, &net, true),
        Fetched::Same(1)
    );
    assert_eq!(
        fetch(
            dir.path(),
            URL,
            &a.public,
            NOW + 2 * FETCH_EVERY_S,
            &net,
            false
        ),
        Fetched::Same(1)
    );
}

#[test]
fn a_bad_document_is_refused_and_the_held_one_stands() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let net = FakeFetcher::default();
    let a = signer();
    serve(&net, &a, "schema = 1\nseq = 5\nreview_count = 2\n");
    assert_eq!(
        fetch(dir.path(), URL, &a.public, NOW, &net, true),
        Fetched::Fresh(5)
    );
    for (text, by, why) in [
        ("schema = 1\nseq = 6\n", signer(), "does not verify"),
        (
            "schema = 1\nseq = 4\n",
            a_again(&a),
            "older than the one held",
        ),
        ("schema = 1\n", a_again(&a), "no `seq`"),
        (
            "schema = 1\nseq = 7\nvalid_until = 1700000000\n",
            a_again(&a),
            "expired",
        ),
        ("schema = 2\nseq = 8\n", a_again(&a), "schema 2"),
    ] {
        serve(&net, &by, text);
        match fetch(dir.path(), URL, &a.public, NOW, &net, true) {
            Fetched::Refused(got) => assert!(got.contains(why), "{text:?}: {got}"),
            other => panic!("{text:?}: {other:?}"),
        }
        let held = held(dir.path(), URL, &a.public, NOW).expect("held");
        assert!(held.contains("seq = 5"), "{text:?}: {held}");
        assert!(held_state(dir.path()).and_then(|h| h.error).is_some());
    }
    let empty = FakeFetcher::default();
    assert!(matches!(
        fetch(dir.path(), URL, &a.public, NOW, &empty, true),
        Fetched::Unreachable(_)
    ));
    assert!(held(dir.path(), URL, &a.public, NOW).is_ok());
}

fn a_again(a: &Signer) -> Signer {
    Signer {
        secret: a.secret.clone(),
        public: a.public.clone(),
    }
}

#[test]
fn a_tampered_or_expired_held_copy_is_not_used() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let net = FakeFetcher::default();
    let a = signer();
    serve(&net, &a, "schema = 1\nseq = 1\nvalid_until = 1800003600\n");
    assert_eq!(
        fetch(dir.path(), URL, &a.public, NOW, &net, true),
        Fetched::Fresh(1)
    );
    assert!(held(dir.path(), URL, &a.public, NOW).is_ok());
    let past = held(dir.path(), URL, &a.public, NOW + 7200).expect_err("expired");
    assert!(past.contains("expired"), "{past}");
    let doc = cache_dir(dir.path()).join(DOC_FILE);
    std::fs::write(&doc, "schema = 1\nseq = 1\npreferred = \"muse\"\n").expect("tamper");
    let got = held(dir.path(), URL, &a.public, NOW).expect_err("tampered");
    assert!(got.contains("does not verify"), "{got}");
}

#[test]
fn only_https_and_a_pinned_key_are_fetched() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let net = FakeFetcher::default();
    let a = signer();
    assert!(matches!(
        fetch(
            dir.path(),
            "http://fleet.example/f.toml",
            &a.public,
            NOW,
            &net,
            true
        ),
        Fetched::Refused(_)
    ));
    assert!(matches!(
        fetch(dir.path(), URL, "", NOW, &net, true),
        Fetched::Refused(_)
    ));
    assert!(net.asked().is_empty());
}
