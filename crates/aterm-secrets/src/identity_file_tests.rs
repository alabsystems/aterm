// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A vault folder in a scratch home and the identity file's path in it.
fn vault() -> (
    aterm_tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let home = aterm_tempfile::tempdir().expect("scratch home");
    let dir = home.path().join("secrets");
    let path = dir.join("identity");
    (home, dir, path)
}

/// The machine key is made once, with its mark, and read back as made: a second ask reads the
/// first key, and a later install keeps it.
#[test]
fn a_machine_key_is_made_once_with_its_mark_and_read_back() {
    let (_home, dir, path) = vault();
    let made = machine_identity(&dir, &path, Some(LabMark::TestBinary), 1).expect("made");
    let (read, mark) = read_marked(&path).expect("read").expect("there");
    assert_eq!(read.recipient(), made.recipient());
    assert_eq!(mark, Some(LabMark::TestBinary));
    let again = machine_identity(&dir, &path, None, 2).expect("read");
    assert_eq!(again.recipient(), made.recipient(), "the first key stands");
    let later = install(&path, Identity::from_secret_bytes([9; 32]), None, 3).expect("installed");
    assert_eq!(
        later.recipient(),
        made.recipient(),
        "a creator never replaces it"
    );
}

/// On Windows the identity file holds no key in the clear: its last line is the key's text
/// sealed by DPAPI, and the file is owner-only (its owner this user, a protected list of the user
/// and SYSTEM alone), so a copy taken to another user or PC opens nothing.
/// NEGATIVE CONTROL (2026-11-16): `stored_text` returning `plain_text` on Windows: the secret
/// key's text is in the file (`the key in the clear`).
#[cfg(windows)]
#[test]
fn a_windows_machine_key_is_sealed_in_an_owner_only_file() {
    let (_home, dir, path) = vault();
    let made = machine_identity(&dir, &path, None, 7).expect("made");
    let bytes = std::fs::read(&path).expect("the file");
    let text = String::from_utf8(bytes).expect("UTF-8");
    let secret = made.to_bech32();
    let secret = std::str::from_utf8(secret.as_slice()).expect("bech32");
    assert!(!text.contains(secret), "the key in the clear");
    assert!(
        !text.to_ascii_uppercase().contains("AGE-SECRET-KEY-"),
        "the key in the clear"
    );
    let last = text
        .lines()
        .rfind(|line| !line.starts_with('#'))
        .expect("a line");
    assert!(last.starts_with(&format!("{SEALED} ")), "{last}");
    assert!(
        text.contains(&made.recipient().to_bech32()),
        "its public key is named in a comment"
    );
    let file = std::fs::File::open(&path).expect("open");
    let user = aterm_uds::acl::Sid::token_user().expect("user");
    assert_eq!(
        aterm_uds::acl::owner_only(&file, &user).expect("judged"),
        Ok(())
    );
}

/// On Windows a key DPAPI can no longer open for this user (sealed under another entropy, or
/// its blob altered) reads as no identity, never as a corrupt file, and the next making sets it
/// aside, kept under a name of its own, and makes a new key in its place; a plain key, which no
/// Windows build writes, is refused, and so is a file that holds no key.
/// NEGATIVE CONTROLS (2026-11-16): `unsealed` answering a blob DPAPI cannot open as
/// `Unsealed::Failed` (an error): the other entropy's file is an error (`another entropy reads
/// as no identity`); `install` writing without setting the unsealable file aside: the making
/// fails (`made in its place`).
#[cfg(windows)]
#[test]
fn a_windows_key_dpapi_cannot_open_is_set_aside_for_a_new_one() {
    let (_home, dir, path) = vault();
    fsx::ensure_private_dir(&dir).expect("vault folder");
    let old = Identity::from_secret_bytes([3; 32]);
    let plain = plain_text(&old, None, 1);
    let other =
        aterm_uds::dpapi::seal(plain.as_slice(), b"another program's entropy").expect("sealed");
    let file = format!(
        "# a machine key sealed under another entropy\n{SEALED} {}\n",
        crate::b64::encode_padded(&other)
    );
    fsx::write_atomic(&path, file.as_bytes(), 0o600).expect("written");
    assert_eq!(
        read_marked(&path)
            .expect("another entropy reads as no identity")
            .map(|(i, _)| i.recipient()),
        None
    );
    let made = machine_identity(&dir, &path, None, 42).expect("made in its place");
    assert_ne!(made.recipient(), old.recipient());
    assert_eq!(
        read_marked(&path)
            .expect("read")
            .map(|(i, _)| i.recipient()),
        Some(made.recipient())
    );
    let kept = dir.join("identity.unsealable-42");
    assert_eq!(
        std::fs::read_to_string(&kept).expect("set aside, kept"),
        file,
        "the unsealable file is kept as it was"
    );

    // Its blob altered: still no identity, and a second set-aside at the same second takes
    // another name.
    let sealed = stored_text(&old, None, 1).expect("sealed");
    let text = sealed.as_str().expect("text");
    let line = text.lines().last().expect("sealed line");
    let mut blob =
        crate::b64::decode_padded(line.strip_prefix(&format!("{SEALED} ")).unwrap()).unwrap();
    let at = blob.len() / 2;
    blob[at] ^= 0x5a;
    let altered = format!("{SEALED} {}\n", crate::b64::encode_padded(&blob));
    fsx::write_atomic(&path, altered.as_bytes(), 0o600).expect("written");
    assert!(
        read_marked(&path)
            .expect("an altered blob reads as no identity")
            .is_none()
    );
    machine_identity(&dir, &path, None, 42).expect("made in its place");
    assert_eq!(
        std::fs::read_to_string(dir.join("identity.unsealable-42.1")).expect("kept"),
        altered
    );

    // A plain key and a file with no key are refused, and left as they are.
    for refused in [
        String::from_utf8(plain.as_slice().to_vec()).unwrap(),
        "# no key at all\n".to_owned(),
    ] {
        fsx::write_atomic(&path, refused.as_bytes(), 0o600).expect("written");
        assert!(read_marked(&path).is_err(), "{refused:?} is refused");
        assert!(machine_identity(&dir, &path, None, 43).is_err());
        assert_eq!(std::fs::read_to_string(&path).expect("left"), refused);
    }
}

/// Puts this thread's DPAPI back as it drops (`aterm_uds::dpapi::seam`).
#[cfg(windows)]
struct Answering;

#[cfg(windows)]
impl Drop for Answering {
    fn drop(&mut self) {
        aterm_uds::dpapi::seam::answer_then_fail(None);
    }
}

/// The names in the vault folder `dir`, the making lock's left out, sorted.
#[cfg(windows)]
fn names_in(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("the vault folder")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name != MAKING_LOCK)
        .collect();
    names.sort();
    names
}

/// No DPAPI failure, at any call of a making, leaves this machine without the key it held. With
/// this thread's DPAPI failing from its first call, as in a logon that holds no password (an
/// OpenSSH key's, a scheduled task's that stores none), a key that opens is an error to read and
/// to make, never no identity, and stays as it was, alone in its folder, opening as before once
/// DPAPI works. And for every count of calls DPAPI answers before it fails, a making over a key
/// it cannot open either finds DPAPI working, sets that file aside whole and puts a new key in
/// its place, or fails having changed nothing: the file as it was, nothing beside it.
/// NEGATIVE CONTROLS (2026-11-16): `Unsealed::confirmed` leaving a refusal as it is (no probe):
/// `a key that opens, DPAPI failing: Ok(None)`; `install` setting the file aside before it seals
/// the new key (the order before this fix, probe kept): `6 calls answered: failed having set
/// the file aside`.
#[cfg(windows)]
#[test]
fn no_dpapi_failure_leaves_the_machine_without_the_key_it_held() {
    use aterm_uds::dpapi::seam::answer_then_fail;
    let _answering = Answering;

    let (_home, dir, path) = vault();
    let held = machine_identity(&dir, &path, None, 5).expect("made");
    let before = std::fs::read(&path).expect("the file");
    answer_then_fail(Some(0));
    let read = read_marked(&path).map(|read| read.map(|(i, _)| i.recipient()));
    assert!(read.is_err(), "a key that opens, DPAPI failing: {read:?}");
    let made = machine_identity(&dir, &path, None, 6).map(|i| i.recipient());
    assert!(made.is_err(), "made while DPAPI fails: {made:?}");
    assert_eq!(std::fs::read(&path).expect("kept"), before);
    assert_eq!(names_in(&dir), ["identity"]);
    answer_then_fail(None);
    assert_eq!(
        read_marked(&path)
            .expect("read")
            .map(|(i, _)| i.recipient()),
        Some(held.recipient())
    );

    let old = Identity::from_secret_bytes([4; 32]);
    let other = aterm_uds::dpapi::seal(plain_text(&old, None, 1).as_slice(), b"another entropy")
        .expect("sealed");
    let unsealable = format!(
        "# a machine key sealed under another entropy\n{SEALED} {}\n",
        crate::b64::encode_padded(&other)
    );
    let mut placed = None;
    for calls in 0..12 {
        let (_home, dir, path) = vault();
        fsx::ensure_private_dir(&dir).expect("vault folder");
        fsx::write_atomic(&path, unsealable.as_bytes(), 0o600).expect("written");
        answer_then_fail(Some(calls));
        let made = machine_identity(&dir, &path, None, 42);
        answer_then_fail(None);
        match made {
            Ok(made) => {
                assert_eq!(
                    names_in(&dir),
                    ["identity", "identity.unsealable-42"],
                    "{calls} calls answered"
                );
                assert_eq!(
                    std::fs::read_to_string(dir.join("identity.unsealable-42")).expect("kept"),
                    unsealable,
                    "{calls} calls answered: set aside whole"
                );
                assert_eq!(
                    read_marked(&path)
                        .expect("read")
                        .map(|(i, _)| i.recipient()),
                    Some(made.recipient()),
                    "{calls} calls answered"
                );
                placed = placed.or(Some(calls));
            }
            Err(e) => {
                assert_eq!(
                    names_in(&dir),
                    ["identity"],
                    "{calls} calls answered: failed having set the file aside ({e})"
                );
                assert_eq!(
                    std::fs::read_to_string(&path).expect("left"),
                    unsealable,
                    "{calls} calls answered: failed having changed the file ({e})"
                );
                assert!(
                    placed.is_none(),
                    "{calls} calls answered: failed after {placed:?} made one"
                );
            }
        }
    }
    assert!(placed.is_some(), "no count of calls let the making through");
}

/// On Unix the identity file is the key's text as it always was: comment lines, the mark's line
/// for a lab identity, and the secret key on the last line, which any build reads.
#[cfg(unix)]
#[test]
fn a_unix_machine_key_is_its_plain_text() {
    let (_home, dir, path) = vault();
    let made = machine_identity(&dir, &path, Some(LabMark::DebugBuild), 11).expect("made");
    let text = std::fs::read_to_string(&path).expect("the file");
    let secret = made.to_bech32();
    assert!(
        text.lines().last() == Some(std::str::from_utf8(secret.as_slice()).unwrap()),
        "the last line is the secret key"
    );
    assert!(text.contains(&format!("{LAB_LINE}debug-build ")), "{text}");
    assert!(text.starts_with("# aterm secrets machine key: created 11 (unix)"));
}
