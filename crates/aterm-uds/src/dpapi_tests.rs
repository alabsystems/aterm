// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Every blob here seals scratch bytes made for the test, never a key or anything of the
//! person's. No test makes another Windows account: another user's blob is stood in for by one
//! naming a master key this profile does not hold, which is what another user's blob, or this
//! user's after an administrator reset the password, names.

use super::{Unsealed, seal, seam, unseal};
use std::ops::Range;

const ENTROPY: &[u8] = b"aterm-uds dpapi test entropy";

/// A DPAPI blob's head (`dpapi.h`'s `DPAPI_BLOB`, as measured): `dwVersion` (4), the provider's
/// GUID (16), `dwMasterKeyVersion` (4), then the master key's GUID and `dwFlags`.
const VERSION: Range<usize> = 0..4;
const MASTER_KEY: Range<usize> = 24..40;
const FLAGS: Range<usize> = 40..44;
/// `CRYPTPROTECT_LOCAL_MACHINE`, which a blob in the user's scope never carries.
const CRYPTPROTECT_LOCAL_MACHINE: u32 = 0x4;

fn u32_at(blob: &[u8], at: Range<usize>) -> u32 {
    u32::from_le_bytes(blob[at].try_into().expect("four bytes"))
}

fn not_openable(blob: &[u8], entropy: &[u8], what: &str) -> i32 {
    match unseal(blob, entropy) {
        Err(Unsealed::NotOpenable(error)) => error.raw_os_error().unwrap_or_default(),
        Err(Unsealed::Failed(error)) => panic!("{what}: a failed call, not a refusal: {error:?}"),
        Ok(opened) => panic!("{what}: opened {} bytes", opened.len()),
    }
}

/// A sealed blob opens for this user with its entropy, to the bytes sealed, and holds no run of
/// them in the clear.
/// NEGATIVE CONTROL (2026-11-16): `seal` answering the bytes as given: `the blob shows eight bytes
/// of what it seals in the clear`.
#[test]
fn a_sealed_blob_opens_for_this_user_with_its_entropy_and_shows_none_of_its_bytes() {
    let plain = b"scratch bytes for the round trip, never a key";
    let blob = seal(plain, ENTROPY).expect("seal");
    assert!(
        !blob
            .windows(8)
            .any(|run| plain.windows(8).any(|p| p == run)),
        "the blob shows eight bytes of what it seals in the clear"
    );
    let opened = unseal(&blob, ENTROPY).expect("unseal");
    assert_eq!(opened.expose(), plain);
    let empty = seal(b"", ENTROPY).expect("seal nothing");
    assert!(unseal(&empty, ENTROPY).expect("unseal nothing").is_empty());
}

/// The blob is the user's scope: version 1, and its flags carry no `CRYPTPROTECT_LOCAL_MACHINE`,
/// which would let every account of the machine open it.
/// No mutant of this law runs: one would call DPAPI with `CRYPTPROTECT_LOCAL_MACHINE`, which
/// nothing here may. `dwFlags`' offset is the measured one.
#[test]
fn a_blob_is_sealed_in_the_users_scope_never_the_machines() {
    let blob = seal(b"scope", ENTROPY).expect("seal");
    assert_eq!(u32_at(&blob, VERSION), 1);
    let flags = u32_at(&blob, FLAGS);
    assert_eq!(flags & CRYPTPROTECT_LOCAL_MACHINE, 0, "flags {flags:#x}");
}

/// Other entropy, none included, opens nothing: the blob is refused as one this user cannot
/// open, not as a call that failed.
/// NEGATIVE CONTROLS (2026-11-16): `unseal` passing no entropy: `other entropy: opened
/// 23 bytes`; `Unsealed::of` answering `Failed` for every code: `other entropy: a failed call,
/// not a refusal`.
#[test]
fn other_entropy_cannot_open_a_blob() {
    let blob = seal(b"sealed with one entropy", ENTROPY).expect("seal");
    for other in [&b"another entropy"[..], b""] {
        let code = not_openable(&blob, other, "other entropy");
        println!("other entropy {other:?}: {code:#x}");
    }
    let plain = seal(b"sealed with none", b"").expect("seal with none");
    not_openable(&plain, ENTROPY, "entropy where none was sealed");
    assert_eq!(
        unseal(&plain, b"").expect("none opens none").expose(),
        b"sealed with none"
    );
}

/// A blob naming a master key this profile does not hold (as one another user sealed, or this
/// user's after an administrator reset the password), one altered since, and bytes too short to
/// be a blob are each refused as a blob this user cannot open.
/// NEGATIVE CONTROL (2026-11-16): `Unsealed::of` answering `Failed` for every code: `another
/// master key: a failed call, not a refusal`.
#[test]
fn a_blob_altered_or_naming_a_master_key_this_profile_lacks_is_not_openable() {
    let blob = seal(b"scratch bytes to alter", ENTROPY).expect("seal");
    let mut lost = blob.clone();
    lost[MASTER_KEY.start] ^= 0xff;
    let mut tail = blob.clone();
    *tail.last_mut().expect("a blob") ^= 1;
    let mut body = blob.clone();
    let middle = body.len() / 2 + 8;
    body[middle] ^= 0x40;
    for (what, bytes) in [
        ("another master key", &lost[..]),
        ("an altered signature", &tail[..]),
        ("an altered body", &body[..]),
        ("eight bytes", &blob[..8]),
    ] {
        let code = not_openable(bytes, ENTROPY, what);
        println!("{what}: {code:#x}");
    }
}

/// Puts this thread's DPAPI back as it drops ([`seam::answer_then_fail`]).
struct Answering;

impl Drop for Answering {
    fn drop(&mut self) {
        seam::answer_then_fail(None);
    }
}

/// The error `unseal` of `blob` gave as a failed call, panicking on any other answer.
fn failed(blob: &[u8], what: &str) -> String {
    match unseal(blob, ENTROPY) {
        Err(Unsealed::Failed(error)) => error.to_string(),
        Err(Unsealed::NotOpenable(error)) => panic!("{what}: not openable: {error:?}"),
        Ok(opened) => panic!("{what}: opened {} bytes", opened.len()),
    }
}

/// A refusal is a fact about the blob only where DPAPI seals and opens a probe in this session.
/// With this thread's DPAPI failing as it does in a logon that holds no password (an OpenSSH
/// key's, a scheduled task's that stores none: `NTE_BAD_KEY_STATE` for every call), a blob that
/// opens and an altered one are each a failed call, never one this user cannot open; so is the
/// altered blob's real refusal where the probe's seal, or its open, fails. Once DPAPI works again
/// the first opens and the second is refused as one this user cannot open.
/// NEGATIVE CONTROL (2026-11-16): `Unsealed::confirmed` leaving a refusal as it is: `a blob that
/// opens, in a session where DPAPI opens nothing: not openable`.
#[test]
fn a_refusal_is_the_blobs_only_where_dpapi_seals_and_opens_in_this_session() {
    let plain = b"scratch bytes that open";
    let blob = seal(plain, ENTROPY).expect("seal");
    let mut altered = blob.clone();
    altered[MASTER_KEY.start] ^= 0xff;
    let _answering = Answering;
    seam::answer_then_fail(Some(0));
    let why = failed(
        &blob,
        "a blob that opens, in a session where DPAPI opens nothing",
    );
    assert!(why.contains("logon session"), "{why}");
    failed(
        &altered,
        "an altered blob, in a session where DPAPI opens nothing",
    );
    for (calls, what) in [
        (1, "an altered blob, the probe's seal failing"),
        (2, "an altered blob, the probe's open failing"),
    ] {
        seam::answer_then_fail(Some(calls));
        failed(&altered, what);
    }
    seam::answer_then_fail(None);
    assert_eq!(unseal(&blob, ENTROPY).expect("opens again").expose(), plain);
    not_openable(&altered, ENTROPY, "an altered blob, DPAPI working");
}
