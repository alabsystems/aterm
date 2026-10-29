// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::age::Identity;
use crate::commons::binding::Request;
use aterm_spec::interp::State;
use std::collections::{BTreeMap, VecDeque};

const RIGHT: &str = "maple river ocean";
const WRONG: &str = "maple river zebra";

fn hex(text: &str) -> Vec<u8> {
    aterm_codec::hex::decode_vec(text.as_bytes()).expect("hex")
}

fn hex32(text: &str) -> [u8; 32] {
    hex(text).try_into().expect("32 bytes")
}

fn phrase(text: &str) -> Phrase {
    Phrase::parse(text).expect("a listed phrase")
}

/// A valid binding for machine `n`, as a joiner's request carries it.
fn binding(n: u8) -> Binding {
    let identity = Identity::from_secret_bytes([n; 32]);
    Request::create(
        [1; 16],
        [2; 32],
        [n; 16],
        [3; 16],
        &identity,
        &[n ^ 0x5a; 32],
    )
    .expect("a request")
    .binding()
    .clone()
}

/// The `CI` of machine `n`'s join at one member.
fn ci(n: u8) -> ChannelId {
    ChannelId::new(&[7; 32], &binding(n), &[8; 32], &[9; 32])
}

/// A book holding `text` as one phrase, Allowed for `bound` (or an invite with none).
fn book_with(text: &str, bound: Option<ChannelId>, now: Instant) -> (Phrases, PhraseId) {
    let mut book = Phrases::new();
    let id = book
        .issue(phrase(text), bound, ALLOW_LIFETIME, now)
        .expect("issue");
    (book, id)
}

/// One whole run: J types `typed` and its own request makes `joiner_ci`; M runs phrase `id`
/// for the request it sees, which makes `member_ci`.
fn run(
    book: &mut Phrases,
    id: PhraseId,
    typed: &str,
    joiner_ci: ChannelId,
    member_ci: &ChannelId,
    now: Instant,
) -> Result<(MemberSession, JoinerSession)> {
    let (joiner, joiner_nonce) = Joiner::start(&phrase(typed), joiner_ci)?;
    let (member, member_nonce) = book.begin(id, member_ci, &joiner_nonce, now)?;
    let (joiner, joiner_share) = joiner.on_nonce(&member_nonce)?;
    let (member, member_share) = book.share(member, &joiner_share)?;
    let (joiner, joiner_tag) = joiner.on_share(&member_share)?;
    let (member, member_tag) = book.confirm(member, &joiner_tag, now)?;
    Ok((member, joiner.on_confirm(&member_tag)?))
}

/// The draft's `generator_string` as its Appendix A.2 writes it, in plain code, as the
/// reference the masked [`first_block`] must equal.
fn reference_generator_string(prs: &[u8], ci: &[u8], sid: &[u8]) -> Vec<u8> {
    let mut prs_part = Vec::new();
    prepend_len(&mut prs_part, prs);
    let mut dsi_part = Vec::new();
    prepend_len(&mut dsi_part, DSI);
    let len_zpad = SHA512_BLOCK
        .saturating_sub(1)
        .saturating_sub(prs_part.len())
        .saturating_sub(dsi_part.len());
    lv_cat(&[DSI, prs, &vec![0; len_zpad], ci, sid])
}

#[test]
fn draft_21_x25519_sha512_vectors() {
    // A.1.2, A.1.4 and A.3.5.
    let prepended = |data: &[u8]| {
        let mut out = Vec::new();
        prepend_len(&mut out, data);
        out
    };
    assert_eq!(prepended(b""), hex("00"));
    assert_eq!(prepended(b"1234"), hex("0431323334"));
    let range: Vec<u8> = (0..=127).collect();
    assert_eq!(
        prepended(&range[..127]),
        [[0x7f].as_slice(), &range[..127]].concat()
    );
    assert_eq!(
        prepended(&range),
        [[0x80, 0x01].as_slice(), range.as_slice()].concat()
    );
    assert_eq!(
        lv_cat(&[b"1234", b"5", b"", b"678"]),
        hex("043132333401350003363738")
    );
    assert_eq!(
        transcript_ir(b"123", b"PartyA", b"234", b"PartyB"),
        hex("03313233065061727479410332333406506172747942")
    );
    assert_eq!(
        transcript_ir(b"3456", b"PartyA", b"2345", b"PartyB"),
        hex("043334353606506172747941043233343506506172747942")
    );

    // B.1.1: the generator.
    let prs = b"Password";
    let ci = hex("0b415f696e69746961746f720b425f726573706f6e646572");
    let sid = hex("7e4b4791d6a8ef019b936c79fb7f2c57");
    let generator_string = hex(concat!(
        "0843506163653235350850617373776f72646d00000000000000000000000000",
        "0000000000000000000000000000000000000000000000000000000000000000",
        "0000000000000000000000000000000000000000000000000000000000000000",
        "0000000000000000000000000000000000000000000000000000000000000000",
        "180b415f696e69746961746f720b425f726573706f6e646572107e4b4791d6a8",
        "ef019b936c79fb7f2c57",
    ));
    assert_eq!(
        [
            first_block(prs, prs.len()).as_slice(),
            lv_cat(&[&ci, &sid]).as_slice()
        ]
        .concat(),
        generator_string
    );
    assert_eq!(reference_generator_string(prs, &ci, &sid), generator_string);
    let hash = digest::digest(&digest::SHA512, &generator_string);
    assert_eq!(
        hash.as_ref()[..32],
        hex("03998087bdb1a2617bbe25ef5a7c18cd4f84f902328701790958755ee4aed1d3")
    );
    let mut decoded = hex32("03998087bdb1a2617bbe25ef5a7c18cd4f84f902328701790958755ee4aed1d3");
    decoded[31] &= 0x7f;
    assert_eq!(
        decoded,
        hex32("03998087bdb1a2617bbe25ef5a7c18cd4f84f902328701790958755ee4aed153")
    );
    let g = calculate_generator(prs, prs.len(), &ci, &sid);
    assert_eq!(
        g,
        hex32("d04bf6d41f6a289632a2e929fa29bebd51092512a7829fdde7d314b62f05a73f")
    );

    // B.1.2-B.1.4: the shares and K.
    let ya = hex32("21b4f4bd9e64ed355c3eb676a28ebedaf6d8f17bdc365995b319097153044080");
    let yb = hex32("848b0779ff415f0af4ea14df9dd1d3c29ac41d836c7808896c4eba19c51ac40a");
    let big_ya = x25519::x25519(&ya, &g);
    let big_yb = x25519::x25519(&yb, &g);
    assert_eq!(
        big_ya,
        hex32("1d13c89278cdadd826f6d8d7f887701430f8380ddc17611cdd6dc989ce0c9f32")
    );
    assert_eq!(
        big_yb,
        hex32("248cccf6d5cdc3646f0ad593f9e6cef4e69d4945f8372e623512ecea32185623")
    );
    let k = hex32("5b067effbdc0b2a0e1d907b21ebb25cfedb96a852179a847c37e43ee71322c6b");
    assert_eq!(*agree(&ya, &big_yb).expect("not low order"), k);
    assert_eq!(*agree(&yb, &big_ya).expect("not low order"), k);

    // B.1.5: ISK, initiator-responder.
    let transcript = transcript_ir(&big_ya, b"ADa", &big_yb, b"ADb");
    assert_eq!(
        transcript,
        hex(concat!(
            "201d13c89278cdadd826f6d8d7f887701430f8380ddc17611cdd6dc989ce0c9f",
            "320341446120248cccf6d5cdc3646f0ad593f9e6cef4e69d4945f8372e623512",
            "ecea3218562303414462",
        ))
    );
    assert_eq!(
        [lv_cat(&[DSI_ISK, &sid, &k]), transcript.clone()].concat(),
        hex(concat!(
            "0c43506163653235355f49534b107e4b4791d6a8ef019b936c79fb7f2c57205b",
            "067effbdc0b2a0e1d907b21ebb25cfedb96a852179a847c37e43ee71322c6b20",
            "1d13c89278cdadd826f6d8d7f887701430f8380ddc17611cdd6dc989ce0c9f32",
            "0341446120248cccf6d5cdc3646f0ad593f9e6cef4e69d4945f8372e623512ec",
            "ea3218562303414462",
        ))
    );
    assert_eq!(
        isk(&sid, &k, &transcript).as_slice(),
        hex(concat!(
            "6e19b875f7a561d6b3ca3dbb9ef42ac55de3e717881018204b8922b4d5e53bb2",
            "aa82c300bea7b65d2b671da71922ddf6472301b79bc270adfa8bf413285f2263",
        ))
    );

    // B.1.10: scalar_mult_vfy at low-order and non-canonical u-coordinates.
    let s = hex32("af46e36bf0527c9d3b16154b82465edd62144c0ac1fc5a18506a2244ba449aff");
    let zero = "0000000000000000000000000000000000000000000000000000000000000000";
    for (u, q) in LOW_ORDER.iter().map(|u| (*u, zero)).chain([
        (
            "daffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            "d8e2c776bbacd510d09fd9278b7edcd25fc5ae9adfba3b6e040e8d3b71b21806",
        ),
        (
            "dbffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            "c85c655ebe8be44ba9c0ffde69f2fe10194458d137f09bbff725ce58803cdb38",
        ),
        (
            "d9ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            "db64dafa9b8fdd136914e61461935fe92aa372cb056314e1231bc4ec12417456",
        ),
        (
            "cdeb7a7c3b41b8ae1656e3faf19fc46ada098deb9c32b1fd866205165f49b880",
            "e062dcd5376d58297be2618c7498f55baa07d7e03184e8aada20bca28888bf7a",
        ),
        (
            "4c9c95bca3508c24b1d0b1559c83ef5b04445cc4581c8e86d8224eddd09f11d7",
            "993c6ad11c4c29da9a56f7691fd0ff8d732e49de6250b6c2e80003ff4629a175",
        ),
    ]) {
        assert_eq!(x25519::x25519(&s, &hex32(u)), hex32(q), "u = {u}");
    }
}

/// B.1.10's u0-u5 and u7: the values a run MUST abort on.
const LOW_ORDER: [&str; 7] = [
    "0000000000000000000000000000000000000000000000000000000000000000",
    "0100000000000000000000000000000000000000000000000000000000000000",
    "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
    "e0eb7a7c3b41b8ae1656e3faf19fc46ada098deb9c32b1fd866205165f49b800",
    "5f9c95bca3508c24b1d0b1559c83ef5b04445cc4581c8e86d8224eddd09f1157",
    "edffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
    "eeffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
];

#[test]
fn the_masked_first_block_is_the_drafts_for_every_prs_length() {
    let (ci, sid) = (b"channel".as_slice(), b"session".as_slice());
    for len in 0..=PRS_MAX {
        let prs: Vec<u8> = (0..len).map(|i| b'a' + (i % 26) as u8).collect();
        let mut buffer = [0xffu8; PRS_MAX];
        buffer[..len].copy_from_slice(&prs);
        assert_eq!(
            first_block(&buffer, len).as_slice(),
            &reference_generator_string(&prs, ci, sid)[..SHA512_BLOCK],
            "PRS of {len} bytes"
        );
    }
    let canonical = phrase(RIGHT).canonical();
    assert_eq!(
        [
            first_block(&canonical.bytes, canonical.len).as_slice(),
            lv_cat(&[ci, sid]).as_slice()
        ]
        .concat(),
        reference_generator_string(RIGHT.as_bytes(), ci, sid)
    );
}

#[test]
fn shifting_the_phrase_ci_boundary_changes_the_generator() {
    let sid = lv_cat(&[&[1; NONCE_LEN], &[2; NONCE_LEN]]);
    // One letter moves from the end of PRS to the start of CI: the bare joins are equal.
    let (prs_a, ci_a) = (b"maple river ocean".as_slice(), b"Xdevice".as_slice());
    let (prs_b, ci_b) = (b"maple river ocea".as_slice(), b"nXdevice".as_slice());
    assert_eq!([prs_a, ci_a].concat(), [prs_b, ci_b].concat());
    assert_ne!(
        calculate_generator(prs_a, prs_a.len(), ci_a, &sid),
        calculate_generator(prs_b, prs_b.len(), ci_b, &sid)
    );
    // One byte moves from the end of CI to the start of sid.
    let prs = RIGHT.as_bytes();
    let (ci_c, sid_c) = (b"ALab join v1".as_slice(), b"\x10nonce".as_slice());
    let (ci_d, sid_d) = (b"ALab join v1\x10".as_slice(), b"nonce".as_slice());
    assert_eq!([ci_c, sid_c].concat(), [ci_d, sid_d].concat());
    assert_ne!(
        calculate_generator(prs, prs.len(), ci_c, sid_c),
        calculate_generator(prs, prs.len(), ci_d, sid_d)
    );
}

/// The one scalar both sides use where a test fixes it.
const SCALAR: [u8; 32] = [0x42; 32];

#[test]
fn neither_side_sends_y_before_sid_is_fixed() {
    let joiner_share = |joiner_nonce: Nonce, member_nonce: Nonce| {
        // J's first message is its nonce alone.
        let (joiner, sent) = Joiner::start_with(&phrase(RIGHT), ci(1), joiner_nonce);
        assert_eq!(sent, joiner_nonce);
        joiner
            .on_nonce_with(&member_nonce, Zeroizing::new(SCALAR))
            .1
    };
    let member_share = |joiner_nonce: Nonce, member_nonce: Nonce| {
        let (mut book, id) = book_with(RIGHT, None, Instant::now());
        let (run, sent) = book
            .begin_with(
                id,
                &ci(1),
                &joiner_nonce,
                member_nonce,
                Zeroizing::new(SCALAR),
                Instant::now(),
            )
            .expect("begin");
        // M's answer to J's nonce is its own nonce alone; its share waits for Y_J.
        assert_eq!(sent, member_nonce);
        book.share(run, &x25519::BASEPOINT).expect("share").1
    };
    let (a, b) = ([1u8; NONCE_LEN], [2u8; NONCE_LEN]);
    // Each share depends on both nonces, so neither could be made before both were known.
    let sides: [fn(Nonce, Nonce) -> Share; 2] = [joiner_share, member_share];
    for share in sides {
        assert_ne!(share(a, b), share(a, a), "the member's nonce");
        assert_ne!(share(a, b), share(b, b), "the joiner's nonce");
    }
    // And each is X25519 of the draft's generator over sid = lv_cat(nonce_J, nonce_M).
    let sid = lv_cat(&[&a, &b]);
    let g = calculate_generator(RIGHT.as_bytes(), RIGHT.len(), &ci(1).0, &sid);
    assert_eq!(joiner_share(a, b), x25519::x25519(&SCALAR, &g));
    assert_eq!(member_share(a, b), x25519::x25519(&SCALAR, &g));
}

#[test]
fn a_run_is_the_drafts_cpace_with_sid_from_both_nonces() {
    let now = Instant::now();
    let (joiner_nonce, member_nonce) = ([0x31u8; NONCE_LEN], [0x32u8; NONCE_LEN]);
    let (joiner_scalar, member_scalar) = ([0x41u8; 32], [0x42u8; 32]);
    let (mut book, id) = book_with(RIGHT, Some(ci(1)), now);
    let (joiner, _) = Joiner::start_with(&phrase(RIGHT), ci(1), joiner_nonce);
    let (member, _) = book
        .begin_with(
            id,
            &ci(1),
            &joiner_nonce,
            member_nonce,
            Zeroizing::new(member_scalar),
            now,
        )
        .expect("begin");
    let (joiner, y_j) = joiner.on_nonce_with(&member_nonce, Zeroizing::new(joiner_scalar));
    let (member, y_m) = book.share(member, &y_j).expect("share");
    let (joiner, t_j) = joiner.on_share(&y_m).expect("joiner share");
    let (member, t_m) = book.confirm(member, &t_j, now).expect("confirm");
    let joiner = joiner.on_confirm(&t_m).expect("member tag");

    // The same run from the draft's plain functions and ring's HKDF and HMAC.
    let mut sid = vec![16u8];
    sid.extend_from_slice(&joiner_nonce);
    sid.push(16);
    sid.extend_from_slice(&member_nonce);
    let hash = digest::digest(
        &digest::SHA512,
        &reference_generator_string(RIGHT.as_bytes(), &ci(1).0, &sid),
    );
    let mut u = [0u8; 32];
    u.copy_from_slice(&hash.as_ref()[..32]);
    u[31] &= 0x7f;
    let g = x25519::elligator2(&u);
    assert_eq!(y_j, x25519::x25519(&joiner_scalar, &g));
    assert_eq!(y_m, x25519::x25519(&member_scalar, &g));
    let k = x25519::x25519(&joiner_scalar, &y_m);
    let mut transcript = vec![32u8];
    transcript.extend_from_slice(&y_j);
    transcript.extend_from_slice(&[0, 32]);
    transcript.extend_from_slice(&y_m);
    transcript.push(0);
    let mut isk_input = vec![12u8];
    isk_input.extend_from_slice(b"CPace255_ISK");
    isk_input.push(34);
    isk_input.extend_from_slice(&sid);
    isk_input.push(32);
    isk_input.extend_from_slice(&k);
    isk_input.extend_from_slice(&transcript);
    let isk = digest::digest(&digest::SHA512, &isk_input);
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, &[]).extract(isk.as_ref());
    let tag = |label: &[u8]| {
        let key = hmac::Key::from(prk.expand(&[label], hmac::HMAC_SHA256).expect("32 bytes"));
        hmac::sign(&key, &transcript).as_ref().to_vec()
    };
    assert_eq!(t_j.as_slice(), tag(b"J"));
    assert_eq!(t_m.as_slice(), tag(b"M"));
    let sealed = member.seal_bundle(b"[everyone]").expect("seal");
    // The bundle key is HKDF(ISK, "bundle"): a key the test derives itself opens the bundle.
    let bundle_key = aead::LessSafeKey::new(aead::UnboundKey::from(
        prk.expand(&[b"bundle"], &aead::CHACHA20_POLY1305)
            .expect("32 bytes"),
    ));
    let mut copy = sealed.clone();
    let opened = bundle_key
        .open_in_place(
            aead::Nonce::assume_unique_for_key([0; 12]),
            aead::Aad::empty(),
            &mut copy,
        )
        .expect("the reference bundle key");
    assert_eq!(opened, b"[everyone]");
    assert_eq!(
        joiner.open_bundle(&sealed).expect("open").as_slice(),
        b"[everyone]"
    );
}

#[test]
fn the_right_phrase_admits_once_and_only_the_joiner_opens_the_bundle() {
    let now = Instant::now();
    let fresh = || {
        let (mut book, id) = book_with(RIGHT, Some(ci(1)), now);
        run(&mut book, id, RIGHT, ci(1), &ci(1), now).expect("a run")
    };
    let (mut book, id) = book_with(RIGHT, Some(ci(1)), now);
    assert_eq!(book.words(id, now), Some(["maple", "river", "ocean"]));
    let (member, joiner) = run(&mut book, id, "MAPL rive-ocea", ci(1), &ci(1), now).expect("run");
    assert_eq!(book.state(id), Some(PhraseState::Admitted));
    assert_eq!(book.words(id, now), None);
    let sealed = member.seal_bundle(b"gh_access_token").expect("seal");
    assert!(!sealed.windows(15).any(|w| w == b"gh_access_token"));
    // Another run's key opens nothing, nor does this run's key open a changed bundle.
    assert!(fresh().1.open_bundle(&sealed).is_err(), "another run's key");
    let (other_member, other_joiner) = fresh();
    let mut tampered = other_member.seal_bundle(b"gh_access_token").expect("seal");
    tampered[3] ^= 1;
    assert!(
        other_joiner.open_bundle(&tampered).is_err(),
        "a changed bundle"
    );
    assert_eq!(
        joiner.open_bundle(&sealed).expect("open").as_slice(),
        b"gh_access_token"
    );
    // One-time: the admitted phrase starts no second run, even with the right phrase.
    assert!(run(&mut book, id, RIGHT, ci(1), &ci(1), now).is_err());
    assert_eq!(book.state(id), Some(PhraseState::Admitted));
}

#[test]
fn a_wrong_phrase_fails_confirmation() {
    let now = Instant::now();
    let (mut book, id) = book_with(RIGHT, Some(ci(1)), now);
    let (joiner, joiner_nonce) = Joiner::start(&phrase(WRONG), ci(1)).expect("start");
    let (member, member_nonce) = book.begin(id, &ci(1), &joiner_nonce, now).expect("begin");
    let (joiner, joiner_share) = joiner.on_nonce(&member_nonce).expect("nonce");
    let (member, member_share) = book.share(member, &joiner_share).expect("share");
    let (joiner, joiner_tag) = joiner.on_share(&member_share).expect("joiner share");
    let error = book
        .confirm(member, &joiner_tag, now)
        .err()
        .expect("a wrong phrase is refused");
    assert!(error.to_string().contains("wrong"), "{error}");
    assert_eq!(book.state(id), Some(PhraseState::Burned));
    // J, for its part, accepts no tag M could send without the phrase.
    assert!(joiner.on_confirm(&[0; TAG_LEN]).is_err());
}

#[test]
fn a_swapped_binding_fails_confirmation() {
    let now = Instant::now();
    // The person Allowed the request M saw, whose binding a man in the middle swapped for its own.
    let swapped = ci(2);
    let (mut book, id) = book_with(RIGHT, Some(swapped.clone()), now);
    let error = run(&mut book, id, RIGHT, ci(1), &swapped, now)
        .err()
        .expect("the swap is caught");
    assert!(error.to_string().contains("wrong"), "{error}");
    assert_eq!(book.state(id), Some(PhraseState::Burned));
    // Only the binding differs between the two CIs.
    let nonce = lv_cat(&[&[1; NONCE_LEN], &[2; NONCE_LEN]]);
    assert_ne!(
        calculate_generator(RIGHT.as_bytes(), RIGHT.len(), &ci(1).0, &nonce),
        calculate_generator(RIGHT.as_bytes(), RIGHT.len(), &swapped.0, &nonce)
    );
    // So with every other part of CI: M's device key, J's node key and the genesis digest.
    for (part, swapped) in [
        (
            "M's device key",
            ChannelId::new(&[6; 32], &binding(1), &[8; 32], &[9; 32]),
        ),
        (
            "J's node key",
            ChannelId::new(&[7; 32], &binding(1), &[5; 32], &[9; 32]),
        ),
        (
            "the genesis",
            ChannelId::new(&[7; 32], &binding(1), &[8; 32], &[4; 32]),
        ),
    ] {
        let (mut book, id) = book_with(RIGHT, Some(swapped.clone()), now);
        let error = run(&mut book, id, RIGHT, ci(1), &swapped, now)
            .err()
            .unwrap_or_else(|| panic!("a swapped {part} is caught"));
        assert!(error.to_string().contains("wrong"), "{part}: {error}");
        assert_eq!(book.state(id), Some(PhraseState::Burned), "{part}");
    }
}

#[test]
fn a_low_order_peer_value_aborts() {
    let now = Instant::now();
    let values = LOW_ORDER.iter().flat_map(|u| {
        let u = hex32(u);
        let mut top = u;
        top[31] |= 0x80;
        [u, top]
    });
    for value in values {
        // J aborts on a low-order Y_M.
        let (joiner, _) = Joiner::start(&phrase(RIGHT), ci(1)).expect("start");
        let (joiner, _) = joiner.on_nonce(&[2; NONCE_LEN]).expect("nonce");
        let error = joiner.on_share(&value).err().expect("J aborts");
        assert!(error.to_string().contains("low-order"), "{error}");
        // M aborts on a low-order Y_J, and the phrase burns.
        let (mut book, id) = book_with(RIGHT, None, now);
        let (run, _) = book.begin(id, &ci(1), &[1; NONCE_LEN], now).expect("begin");
        let error = book.share(run, &value).err().expect("M aborts");
        assert!(error.to_string().contains("low-order"), "{error}");
        assert_eq!(book.state(id), Some(PhraseState::Burned));
        assert!(book.begin(id, &ci(1), &[1; NONCE_LEN], now).is_err());
    }
}

#[test]
fn a_failed_attempt_burns_the_phrase() {
    let now = Instant::now();
    let (mut book, id) = book_with(RIGHT, None, now);
    assert!(run(&mut book, id, WRONG, ci(1), &ci(1), now).is_err());
    assert_eq!(book.state(id), Some(PhraseState::Burned));
    assert_eq!(book.words(id, now), None);
    // The right phrase, from the same Mac or another, admits nothing after the failure.
    assert!(run(&mut book, id, RIGHT, ci(1), &ci(1), now).is_err());
    assert!(run(&mut book, id, RIGHT, ci(3), &ci(3), now).is_err());
    assert_eq!(book.state(id), Some(PhraseState::Burned));
    // A run that ends any other way burns it too.
    let (mut book, id) = book_with(RIGHT, None, now);
    let (_dropped, _) = book.begin(id, &ci(1), &[1; NONCE_LEN], now).expect("begin");
    assert_eq!(book.state(id), Some(PhraseState::Running));
    assert!(
        book.begin(id, &ci(1), &[1; NONCE_LEN], now).is_err(),
        "one run"
    );
    book.burn(id);
    assert_eq!(book.state(id), Some(PhraseState::Burned));
}

#[test]
fn an_expired_phrase_admits_nothing() {
    let now = Instant::now();
    let later = now + ALLOW_LIFETIME;
    let (mut book, id) = book_with(RIGHT, None, now);
    assert_eq!(book.words(id, later), None);
    assert!(run(&mut book, id, RIGHT, ci(1), &ci(1), later).is_err());
    assert_eq!(book.state(id), Some(PhraseState::Live));
    // A run that began in time but confirms late burns the phrase.
    let (joiner, joiner_nonce) = Joiner::start(&phrase(RIGHT), ci(1)).expect("start");
    let (member, member_nonce) = book.begin(id, &ci(1), &joiner_nonce, now).expect("begin");
    let (joiner, joiner_share) = joiner.on_nonce(&member_nonce).expect("nonce");
    let (member, member_share) = book.share(member, &joiner_share).expect("share");
    let (_, joiner_tag) = joiner.on_share(&member_share).expect("joiner share");
    let error = book
        .confirm(member, &joiner_tag, later)
        .err()
        .expect("late");
    assert!(error.to_string().contains("expired"), "{error}");
    assert_eq!(book.state(id), Some(PhraseState::Burned));
}

#[test]
fn a_phrase_allowed_for_one_request_starts_no_run_for_another() {
    let now = Instant::now();
    let (mut book, id) = book_with(RIGHT, Some(ci(1)), now);
    let refused = run(&mut book, id, RIGHT, ci(2), &ci(2), now)
        .err()
        .expect("refused");
    assert!(refused.to_string().contains("another Mac"), "{refused}");
    assert_eq!(book.state(id), Some(PhraseState::Live), "no guess was made");
    run(&mut book, id, RIGHT, ci(1), &ci(1), now).expect("the Allowed request");
}

#[test]
fn a_member_shows_at_most_four_live_phrases() {
    let now = Instant::now();
    let mut book = Phrases::new();
    let ids: Vec<PhraseId> = (0..MAX_LIVE)
        .map(|_| book.invite(now).expect("invite"))
        .collect();
    assert!(book.invite(now).is_err());
    assert!(book.allow(ci(1), now).is_err());
    for id in &ids {
        assert!(book.words(*id, now).is_some());
    }
    // A running phrase keeps its place, and its run survives new phrases; a spent one frees it.
    let (running, _) = book
        .begin(ids[0], &ci(1), &[1; NONCE_LEN], now)
        .expect("begin");
    assert!(book.invite(now).is_err());
    assert_eq!(book.state(ids[0]), Some(PhraseState::Running));
    assert!(book.share(running, &x25519::BASEPOINT).is_ok());
    book.burn(ids[0]);
    let fresh = book.allow(ci(1), now).expect("a freed place");
    assert_eq!(book.expires(fresh), Some(now + ALLOW_LIFETIME));
    assert_eq!(book.expires(ids[1]), Some(now + INVITE_LIFETIME));
    assert!(book.invite(now).is_err());
    // An admitted phrase frees its place too.
    let typed = book.words(ids[1], now).expect("live").join(" ");
    run(&mut book, ids[1], &typed, ci(1), &ci(1), now).expect("the invite admits");
    book.invite(now).expect("a place an admission freed");
    // Expired phrases free theirs.
    for _ in 0..MAX_LIVE {
        book.invite(now + INVITE_LIFETIME).expect("after expiry");
    }
}

/// The real book, driven by `JoinPhrase`'s actions, and read back as the model's variables.
struct Harness {
    book: Phrases,
    id: Option<PhraseId>,
    run: Option<MemberRun>,
    member_nonce: Nonce,
    now: Instant,
    guesses: i64,
    admits: i64,
    late_admits: i64,
}

const JOINER_NONCE: Nonce = [0x11; NONCE_LEN];

impl Harness {
    fn new() -> Self {
        Self {
            book: Phrases::new(),
            id: None,
            run: None,
            member_nonce: [0; NONCE_LEN],
            now: Instant::now(),
            guesses: 0,
            admits: 0,
            late_admits: 0,
        }
    }

    fn replay(path: &[&str]) -> Self {
        let mut harness = Self::new();
        for action in path {
            harness.apply(action);
        }
        harness
    }

    fn expired(&self) -> bool {
        self.id
            .and_then(|id| self.book.expires(id))
            .is_some_and(|expires| self.now >= expires)
    }

    fn observe(&self) -> State {
        let phase = match self.id.and_then(|id| self.book.state(id)) {
            None => 0,
            Some(PhraseState::Live) => 1,
            Some(PhraseState::Running) => 2,
            Some(PhraseState::Admitted) => 3,
            Some(PhraseState::Burned) => 4,
        };
        State::from([
            ("phase", phase),
            ("expired", i64::from(self.expired())),
            ("guesses", self.guesses),
            ("admits", self.admits),
            ("late_admits", self.late_admits),
        ])
    }

    fn id(&self) -> PhraseId {
        self.id.expect("an issued phrase")
    }

    fn begin(&mut self) -> Result<()> {
        let (run, nonce) = self
            .book
            .begin(self.id(), &ci(1), &JOINER_NONCE, self.now)?;
        self.run = Some(run);
        self.member_nonce = nonce;
        Ok(())
    }

    fn apply(&mut self, action: &str) {
        match action {
            "Issue" => {
                let id = self
                    .book
                    .issue(phrase(RIGHT), Some(ci(1)), ALLOW_LIFETIME, self.now);
                self.id = Some(id.expect("issue"));
            }
            "Begin" => self.begin().expect("begin"),
            "ConfirmRight" | "ConfirmLate" => self.confirm(RIGHT),
            "ConfirmWrong" => self.confirm(WRONG),
            "Abort" => {
                let run = self.run.take().expect("a run");
                assert!(self.book.share(run, &[0; 32]).is_err());
            }
            "Expire" => self.now = self.book.expires(self.id()).expect("an expiry"),
            "Cancel" => self.book.cancel(self.id()),
            other => panic!("no such JoinPhrase action: {other}"),
        }
    }

    fn confirm(&mut self, typed: &str) {
        let (joiner, _) = Joiner::start_with(&phrase(typed), ci(1), JOINER_NONCE);
        let (joiner, joiner_share) = joiner.on_nonce(&self.member_nonce).expect("nonce");
        let run = self.run.take().expect("a run");
        let (member, member_share) = self.book.share(run, &joiner_share).expect("share");
        let (_, joiner_tag) = joiner.on_share(&member_share).expect("joiner share");
        let late = self.expired();
        self.guesses += 1;
        if self.book.confirm(member, &joiner_tag, self.now).is_ok() {
            self.admits += 1;
            self.late_admits += i64::from(late);
        }
    }
}

#[test]
fn join_phrase_model_conforms_and_catches_its_mutants() {
    let model = aterm_models_swarm::join_phrase_model();
    aterm_spec::interp::prove_and_catch(&model);
    let fired = aterm_spec::interp::fired_actions(&model);
    assert_eq!(fired.len(), 8, "fired actions: {fired:?}");

    // Every reachable state, with the shortest path of actions that reaches it.
    let mut paths: BTreeMap<State, Vec<&'static str>> = BTreeMap::new();
    let mut queue = VecDeque::from([model.init_state()]);
    paths.insert(model.init_state(), Vec::new());
    while let Some(state) = queue.pop_front() {
        for action in &model.actions {
            for next in model.successors(action.name, &state) {
                if !paths.contains_key(&next) {
                    let mut path = paths[&state].clone();
                    path.push(action.name);
                    paths.insert(next.clone(), path);
                    queue.push_back(next);
                }
            }
        }
    }
    assert_eq!(
        paths.len(),
        aterm_spec::interp::reachable_states(&model).len()
    );

    for (state, path) in &paths {
        assert_eq!(&Harness::replay(path).observe(), state, "after {path:?}");
        for action in &model.actions {
            if model.action_enabled(action.name, state) {
                let mut harness = Harness::replay(path);
                harness.apply(action.name);
                let observed = harness.observe();
                assert!(
                    model.successors(action.name, state).contains(&observed),
                    "{} after {path:?} gave {observed:?}",
                    action.name
                );
            }
        }
        // A run the model does not let start, the book refuses, and the refusal changes nothing.
        if state["phase"] > 0 && !model.action_enabled("Begin", state) {
            let mut harness = Harness::replay(path);
            assert!(harness.begin().is_err(), "Begin after {path:?}");
            assert_eq!(&harness.observe(), state);
        }
    }
}
