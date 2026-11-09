// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// A person's key (§11 C3c): its certificate, its seal, and the node service's vouching.

use super::*;

const NOW: u64 = 1_800_000_000_000;

/// What the tests' ask is sealed for: session `s-b`, from h-andrew.
fn address(sid: &str, kind: &str) -> String {
    post_address("alab", sid, "h-andrew", kind)
}

/// A change made to a sealed post after its sealing, and the field it changes.
type Change = (&'static str, fn(&mut Body));

/// A change made to a certificate after its signing, and what it shows.
type CertChange = (&'static str, fn(&mut Cert));

fn node(tag: u8) -> ([u8; 32], [u8; 32]) {
    let seed = [tag; 32];
    (seed, astream_aead::signature::public_key(&seed))
}

fn private_dir() -> aterm_tempfile::TempDir {
    let dir = crate::test_scratch_dir("pkey");
    aterm_uds::private_file::ensure_private_dir(dir.path()).expect("a private dir");
    dir
}

fn body(text: &str) -> Body {
    let mut body = Body::new(NOW);
    body.re = Some(7);
    body.dl = Some(NOW + 60_000);
    body.text = text.to_owned();
    body
}

/// The key `dir` holds, its certificate signed by `seed` at `at`, and a sealed ask.
fn sealed(dir: &Path, seed: &[u8; 32], at: u64) -> (PersonKey, Cert, Body) {
    assert!(vouch(dir, seed, at).is_ok());
    let key = PersonKey::read(dir).expect("reads").expect("made");
    let cert = read_cert(dir).expect("reads").expect("signed");
    let mut post = body("hello");
    key.seal(&cert, &address("s-b", "ask"), &mut post);
    (key, cert, post)
}

/// **A CERTIFICATE IS THE NODE KEY'S, AND IN FORCE FOR ITS LIFE**: one reads back as it was
/// written, names the node key that signed it, and checks under that key, given as itself or as
/// the node it names, from its signing to [`CERT_LIFE_MS`] later (and [`CERT_AHEAD_MS`] before, for
/// two Macs' clocks); under no other key or node, nor outside that, nor once any of it changed.
#[test]
fn a_certificate_is_the_node_keys_and_in_force_for_its_life() {
    let ((seed, key), (_, other)) = (node(1), node(2));
    let cert = Cert::issue(&seed, [9; 32], NOW);
    assert_eq!(cert.node, key);
    assert_eq!(Cert::decode(&cert.encode()), Some(cert));
    assert_eq!(Cert::decode(&cert.encode().to_uppercase()), None);
    assert_eq!(Cert::decode(&cert.encode()[2..]), None);
    let (mine, theirs) = (
        crate::federate::node_of(&key),
        crate::federate::node_of(&other),
    );
    for signer in [Signer::Key(&key), Signer::Node(&mine)] {
        assert_eq!(cert.check(signer, NOW), Ok(()));
        assert_eq!(cert.check(signer, NOW + CERT_LIFE_MS), Ok(()));
        assert_eq!(cert.check(signer, NOW - CERT_AHEAD_MS), Ok(()));
        assert_eq!(
            cert.check(signer, NOW + CERT_LIFE_MS + 1),
            Err(Refusal::Lapsed)
        );
        assert_eq!(
            cert.check(signer, NOW - CERT_AHEAD_MS - 1),
            Err(Refusal::Lapsed)
        );
    }
    for signer in [Signer::Key(&other), Signer::Node(&theirs)] {
        assert_eq!(cert.check(signer, NOW), Err(Refusal::NotVouched));
    }
    let changed: [CertChange; 3] = [
        ("its time is signed", |c| c.issued_ms += 1),
        ("its person key is signed", |c| c.person = [8; 32]),
        ("its node key made the signature", |c| {
            c.node = astream_aead::signature::public_key(&[2; 32]);
        }),
    ];
    for (what, change) in changed {
        let mut forged = cert;
        change(&mut forged);
        let signer = Signer::Key(&forged.node);
        assert_eq!(
            forged.check(signer, NOW),
            Err(Refusal::NotVouched),
            "{what}"
        );
    }
}

/// **A SEAL BINDS THE PERSON'S POST TO ITS SESSION AND ITS WORDS**: a post the person key sealed
/// under its certificate verifies under the node key that vouched, giving the person key. One
/// changed after its sealing (its text, its time, what it answers, its deadline) or bound for
/// another session or kind, sealed by another person key under that certificate, or carrying
/// another certificate of the same key, has a seal that does not verify; one under another Mac's
/// certificate is not vouched for; and one carrying no seal, relayed, or of a kind no person posts
/// is refused, each for its reason.
#[test]
fn a_seal_binds_the_persons_post_to_its_session_and_its_words() {
    let dir = private_dir();
    let ((seed, key), (other_seed, other)) = (node(1), node(2));
    let (person, cert, post) = sealed(dir.path(), &seed, NOW);
    let ask = address("s-b", "ask");
    let signer = Signer::Key(&key);
    assert_eq!(verify(signer, &ask, "ask", &post, NOW), Ok(person.public()));
    let node = crate::federate::node_of(&key);
    assert_eq!(
        verify(Signer::Node(&node), &ask, "ask", &post, NOW),
        Ok(person.public()),
        "as the Mac its lane names"
    );
    assert_eq!(
        verify(Signer::Key(&other), &ask, "ask", &post, NOW),
        Err(Refusal::NotVouched),
        "another Mac's node key vouched for no such key"
    );
    let changed: [Change; 4] = [
        ("text", |b| b.text.push('!')),
        ("t", |b| b.t += 1),
        ("re", |b| b.re = Some(8)),
        ("dl", |b| b.dl = None),
    ];
    for (what, change) in changed {
        let mut forged = post.clone();
        change(&mut forged);
        assert_eq!(
            verify(signer, &ask, "ask", &forged, NOW),
            Err(Refusal::BadSeal),
            "{what} changed after sealing"
        );
    }
    for (what, bound) in [
        ("another session", address("s-c", "ask")),
        ("another kind", address("s-b", "note")),
    ] {
        let kind = bound.rsplit('/').next().unwrap_or_default();
        assert_eq!(
            verify(signer, &bound, kind, &post, NOW),
            Err(Refusal::BadSeal),
            "sealed for {what}"
        );
    }
    // Another certificate of the same key, in force: the seal covers the one it was made under.
    let mut swapped = post.clone();
    let fresh = Cert::issue(&seed, person.public(), NOW + 1);
    swapped
        .unknown
        .insert(CERT_TOKEN.to_owned(), fresh.encode());
    assert_eq!(
        verify(signer, &ask, "ask", &swapped, NOW),
        Err(Refusal::BadSeal),
        "the seal covers its certificate"
    );
    let mut unsigned = post.clone();
    let mut altered = cert;
    altered.issued_ms -= 1;
    unsigned
        .unknown
        .insert(CERT_TOKEN.to_owned(), altered.encode());
    assert_eq!(
        verify(signer, &ask, "ask", &unsigned, NOW),
        Err(Refusal::NotVouched),
        "a certificate changed after its signing"
    );
    // Another Mac's person key, under this Mac's certificate: the seal is not the vouched key's.
    let elsewhere = private_dir();
    let (theirs, their_cert, their_post) = sealed(elsewhere.path(), &other_seed, NOW);
    let mut forged = body("hello");
    theirs.seal(&cert, &ask, &mut forged);
    assert_eq!(
        verify(signer, &ask, "ask", &forged, NOW),
        Err(Refusal::BadSeal)
    );
    // That Mac's own post, under its own certificate: no certificate of this Mac's.
    assert_eq!(their_cert.node, other);
    assert_eq!(
        verify(signer, &ask, "ask", &their_post, NOW),
        Err(Refusal::NotVouched)
    );
    let mut bare = post.clone();
    bare.unknown.remove(SEAL_TOKEN);
    assert_eq!(
        verify(signer, &ask, "ask", &bare, NOW),
        Err(Refusal::Unsealed)
    );
    let mut garbled = post.clone();
    garbled
        .unknown
        .insert(SEAL_TOKEN.to_owned(), "zz".to_owned());
    assert_eq!(
        verify(signer, &ask, "ask", &garbled, NOW),
        Err(Refusal::Malformed)
    );
    let mut relayed = post.clone();
    relayed.via = Some("n-00000000000000cc".to_owned());
    assert_eq!(
        verify(signer, &ask, "ask", &relayed, NOW),
        Err(Refusal::Relayed)
    );
    assert_eq!(verify(signer, &ask, "ack", &post, NOW), Err(Refusal::Kind));
    // Through the body's own text form, as the lane carries it.
    let (wire, _) = Body::decode(&post.encode());
    assert_eq!(verify(signer, &ask, "ask", &wire, NOW), Ok(person.public()));
}

/// **THE NODE SERVICE VOUCHES BY ITSELF, AND A LOST KEY IS REPLACED**: its first pass makes the
/// person key and signs its certificate, asking nothing; a later pass the same day keeps both;
/// a day on it signs again; a new node key signs at once; and a key lost or damaged is made anew,
/// with a certificate for the new key. A post the old key sealed carries its own certificate, so
/// it verifies until that certificate lapses ([`CERT_LIFE_MS`]), and no certificate of the new key
/// vouches for the old key's seal.
#[test]
fn the_node_service_vouches_by_itself_and_a_lost_key_is_replaced() {
    let dir = private_dir();
    let ((seed, key), (new_seed, new_key)) = (node(1), node(2));
    assert_eq!(
        vouch(dir.path(), &seed, NOW).unwrap(),
        Vouched::Signed { made: true }
    );
    let first = PersonKey::read(dir.path()).unwrap().unwrap().public();
    let cert = read_cert(dir.path()).unwrap().unwrap();
    assert_eq!(
        (cert.person, cert.check(Signer::Key(&key), NOW)),
        (first, Ok(()))
    );
    assert_eq!(
        vouch(dir.path(), &seed, NOW + CERT_RENEW_MS - 1).unwrap(),
        Vouched::Kept
    );
    assert_eq!(
        vouch(dir.path(), &seed, NOW + CERT_RENEW_MS).unwrap(),
        Vouched::Signed { made: false }
    );
    let later = NOW + CERT_RENEW_MS + 1;
    assert_eq!(
        vouch(dir.path(), &new_seed, later).unwrap(),
        Vouched::Signed { made: false },
        "a new node key"
    );
    let cert = read_cert(dir.path()).unwrap().unwrap();
    assert_eq!(cert.check(Signer::Key(&new_key), later), Ok(()));
    assert_eq!(
        cert.check(Signer::Key(&key), later),
        Err(Refusal::NotVouched)
    );
    assert_eq!(
        PersonKey::read(dir.path()).unwrap().unwrap().public(),
        first
    );

    let ask = address("s-b", "ask");
    let signer = Signer::Key(&new_key);
    for (case, damage) in [("lost", None), ("damaged", Some(b"not a key\n".as_slice()))] {
        let (_, old_cert, old_post) = sealed(dir.path(), &new_seed, later);
        match damage {
            None => std::fs::remove_file(dir.path().join(KEY_FILE)).unwrap(),
            Some(bytes) => {
                aterm_uds::private_file::write_private(dir.path(), KEY_FILE, bytes).unwrap();
            }
        }
        assert!(
            PersonKey::read(dir.path()).is_err() == damage.is_some(),
            "{case}"
        );
        assert_eq!(
            vouch(dir.path(), &new_seed, later).unwrap(),
            Vouched::Signed { made: true },
            "{case}: made anew, with no question"
        );
        let made = PersonKey::read(dir.path()).unwrap().unwrap().public();
        let cert = read_cert(dir.path()).unwrap().unwrap();
        assert_ne!(made, old_cert.person, "{case}");
        assert_eq!(
            cert.person, made,
            "{case}: its certificate names the new key"
        );
        assert_eq!(
            verify(signer, &ask, "ask", &old_post, later),
            Ok(old_cert.person),
            "{case}: an old post carries its own certificate"
        );
        assert_eq!(
            verify(signer, &ask, "ask", &old_post, later + CERT_LIFE_MS + 1),
            Err(Refusal::Lapsed),
            "{case}: until it lapses"
        );
        let mut resealed = old_post.clone();
        resealed
            .unknown
            .insert(CERT_TOKEN.to_owned(), cert.encode());
        assert_eq!(
            verify(signer, &ask, "ask", &resealed, later),
            Err(Refusal::BadSeal),
            "{case}: the old key's seal under the new certificate"
        );
    }
}

/// **THE KEY FILE IS PRIVATE**: the node service writes the person key 0600 and the certificate
/// beside it, and a key file others can read is refused, so a post is not sealed with it.
#[test]
fn the_key_file_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = private_dir();
    let (seed, _) = node(1);
    assert!(vouch(dir.path(), &seed, NOW).is_ok());
    for file in [KEY_FILE, CERT_FILE] {
        let mode = std::fs::metadata(dir.path().join(file))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "{file}");
    }
    std::fs::set_permissions(
        dir.path().join(KEY_FILE),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(PersonKey::read(dir.path()).is_err());
}
