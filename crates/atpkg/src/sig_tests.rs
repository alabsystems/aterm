// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use aterm_update_core::roster::Machine;
use ring::signature::{Ed25519KeyPair, KeyPair};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const MASTER_SEED: [u8; 32] = [0xA1; 32];
const OTHER_MASTER_SEED: [u8; 32] = [0xA2; 32];
const M3_SEED: [u8; 32] = [0xB1; 32];
const M11_SEED: [u8; 32] = [0xB2; 32];
const THIEF_SEED: [u8; 32] = [0xC1; 32];

/// 2026-08-04T00:00:00Z — comfortably inside every fixture window below.
const NOW: i64 = 1_785_801_600;

fn keypair(seed: &[u8; 32]) -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(seed).expect("synthetic 32-byte seed")
}

fn pubkey_b64(kp: &Ed25519KeyPair) -> String {
    aterm_codec::base64::encode(kp.public_key().as_ref()).expect("32-byte key")
}

fn sign(seed: &[u8; 32], msg: &[u8]) -> Vec<u8> {
    keypair(seed).sign(msg).as_ref().to_vec()
}

/// A two-machine roster at `roster_seq = 3`: m3 and m11 both live.
fn roster() -> Roster {
    Roster {
        schema: 1,
        roster_seq: 3,
        valid_until: "2027-02-01T00:00:00Z".into(),
        machines: vec![
            Machine {
                id: "m3".into(),
                pubkey: pubkey_b64(&keypair(&M3_SEED)),
                added_at: "2026-08-04T00:00:00Z".into(),
                not_after: None,
            },
            Machine {
                id: "m11".into(),
                pubkey: pubkey_b64(&keypair(&M11_SEED)),
                added_at: "2026-08-04T00:00:00Z".into(),
                not_after: None,
            },
        ],
        revoked: vec![],
    }
}

/// The roster as it is published: emitted bytes + the paper master's detached
/// signature over exactly those bytes.
fn published(r: &Roster, master_seed: &[u8; 32]) -> (Vec<u8>, Vec<u8>) {
    let bytes = r.to_toml().expect("a valid roster emits").into_bytes();
    let sig = sign(master_seed, &bytes);
    (bytes, sig)
}

/// An anchor armed with the real paper master, at roster floor `floor`.
fn armed(floor: u64) -> Anchor {
    Anchor::of(vec![pubkey_b64(&keypair(&MASTER_SEED))], floor)
}

/// The published roster, taken through the real client path.
fn admit(anchor: &Anchor, r: &Roster) -> Result<TrustedRoster, Reject> {
    let (bytes, sig) = published(r, &MASTER_SEED);
    admit_roster(anchor, bytes, &sig, NOW)
}

/// An `index.toml` body naming `machine_id` / `roster_seq`, the attribution step 10 binds.
fn index_body(machine_id: &str, roster_seq: u64, index_build: u64) -> Vec<u8> {
    let mut s = String::from("schema = 2\nindex_build = ");
    s.push_str(&index_build.to_string());
    s.push_str("\nvalid_until = \"2027-02-01T00:00:00Z\"\nmachine_id = \"");
    s.push_str(machine_id);
    s.push_str("\"\nroster_seq = ");
    s.push_str(&roster_seq.to_string());
    s.push_str("\n[programs.ay]\nrepo = \"ay\"\n");
    s.into_bytes()
}

const PKG: &[u8] = b"schema = 2\nprogram = \"ay\"\nbuild_number = 18\n";

fn private_tmp_dir(label: &str) -> crate::testdir::Dir {
    crate::testdir::dir(&format!("atpkg-{label}-"))
}

/// An unarmed anchor authorizes nothing, not even a roster genuinely signed by a real
/// paper master, and refuses before any crypto.
#[test]
fn an_unarmed_anchor_is_inert_and_authorizes_nothing() {
    let unarmed = Anchor::of(vec![], 0);
    assert!(
        !unarmed.is_armed(),
        "precondition: this anchor really does pin nothing"
    );
    let (bytes, sig) = published(&roster(), &MASTER_SEED);
    assert_eq!(
        admit_roster(&unarmed, bytes.clone(), &sig, NOW).err(),
        Some(Reject::Disabled),
        "an unpinned anchor must REFUSE, never wave through"
    );
    assert!(admit_roster(&armed(0), bytes, &sig, NOW).is_ok());
}

/// An index signed by a machine the roster names is accepted, attributed to that machine.
#[test]
fn an_index_signed_by_a_roster_named_machine_is_accepted() {
    let anchor = armed(0);
    let roster = admit(&anchor, &roster()).expect("master-signed, fresh, above the floor");
    assert_eq!(roster.seq(), 3);

    let raw = index_body("m3", 3, 41);
    let sig = sign(&M3_SEED, &raw);
    let (index, verified) = roster
        .authorize_index(raw.clone(), &sig)
        .expect("m3 is live and signed it");
    assert_eq!(index.index_build, 41, "Deref reaches the parsed index");
    assert_eq!(index.attribution().machine_id, "m3");
    assert_eq!(index.roster_seq(), 3);
    assert_eq!(
        verified.as_slice(),
        raw.as_slice(),
        "the verified bytes are the SAME bytes the signature covered"
    );
    let pkg_sig = sign(&M11_SEED, PKG);
    assert_eq!(
        index
            .verify_pkg(PKG.to_vec(), &pkg_sig)
            .expect("m11 is live too")
            .as_slice(),
        PKG
    );
}

/// An index signed by a machine not on the roster is refused, though the roster is
/// genuine and the signature valid.
#[test]
fn an_index_signed_by_a_machine_outside_the_roster_is_refused() {
    let anchor = armed(0);
    let roster = admit(&anchor, &roster()).unwrap();
    let raw = index_body("m3", 3, 41);
    let thief = sign(&THIEF_SEED, &raw);
    assert_eq!(
        roster.authorize_index(raw.clone(), &thief).err(),
        Some(Reject::Verify)
    );
    assert!(
        roster
            .authorize_index(raw, &sign(&M3_SEED, &index_body("m3", 3, 41)))
            .is_ok()
    );
}

/// A revoked machine is refused with its signature never checked: it leaves the candidate
/// set in `live()`, so the verdict is `Verify`, not `Revoked`.
#[test]
fn a_revoked_machine_is_refused_and_its_signature_is_never_verified() {
    let anchor = armed(0);
    let mut r = roster();
    r.roster_seq = 4;
    r.revoked = vec!["m11".into()];
    let revoking = admit(&anchor, &r).unwrap();

    let raw = index_body("m11", 4, 41);
    let sig = sign(&M11_SEED, &raw);
    assert_eq!(
        revoking.authorize_index(raw.clone(), &sig).err(),
        Some(Reject::Verify),
        "a revoked machine must not be in the candidate set at all"
    );
    assert_eq!(
        revoking
            .authorize_bytes(PKG.to_vec(), &sign(&M11_SEED, PKG))
            .err(),
        Some(Reject::Verify)
    );

    let mut same_but_live = roster();
    same_but_live.roster_seq = 4;
    let permissive = admit(&anchor, &same_but_live).unwrap();
    assert!(permissive.authorize_index(raw, &sig).is_ok());

    let m3_raw = index_body("m3", 4, 41);
    assert!(
        revoking
            .authorize_index(m3_raw.clone(), &sign(&M3_SEED, &m3_raw))
            .is_ok()
    );
}

/// An expired machine ages out of the candidate set on its own schedule, with no
/// revocation and no master touch.
#[test]
fn an_expired_machine_leaves_the_candidate_set() {
    let anchor = armed(0);
    let mut r = roster();
    r.machines[0].not_after = Some("2026-01-01T00:00:00Z".into());
    let roster = admit(&anchor, &r).unwrap();
    let raw = index_body("m3", 3, 41);
    assert_eq!(
        roster
            .authorize_index(raw, &sign(&M3_SEED, &index_body("m3", 3, 41)))
            .err(),
        Some(Reject::Verify)
    );
}

/// A roster signed by a different master is refused under the pinned one, and the
/// chain never reaches a parse.
#[test]
fn a_roster_signed_by_the_wrong_master_is_refused() {
    let (bytes, sig) = published(&roster(), &OTHER_MASTER_SEED);
    assert_eq!(
        admit_roster(&armed(0), bytes, &sig, NOW).err(),
        Some(Reject::Verify)
    );
}

/// A one-byte edit anywhere in the roster is refused: no machine is added without the
/// paper master.
#[test]
fn a_tampered_roster_is_refused_byte_by_byte() {
    let (bytes, sig) = published(&roster(), &MASTER_SEED);
    for i in 0..bytes.len() {
        let mut bad = bytes.clone();
        bad[i] ^= 0x01;
        assert_eq!(
            admit_roster(&armed(0), bad, &sig, NOW).err(),
            Some(Reject::Verify),
            "a flip at roster byte {i} must reject"
        );
    }
    assert!(admit_roster(&armed(0), bytes, &sig, NOW).is_ok());
}

/// A rolled-back roster is refused once a newer generation is durably seen, though its
/// master signature still verifies.
#[test]
fn a_rolled_back_roster_is_refused_by_the_durable_floor() {
    let old = roster();
    assert_eq!(
        admit(&armed(4), &old).err(),
        Some(Reject::Rollback),
        "a client that has seen seq 4 must never accept seq 3 again"
    );
    assert!(admit(&armed(3), &old).is_ok());
}

/// A lapsed roster is refused, and an unreadable `valid_until` counts as lapsed, not
/// absent.
#[test]
fn a_lapsed_or_unreadable_roster_window_is_refused() {
    let mut r = roster();
    r.valid_until = "2026-01-01T00:00:00Z".into();
    assert_eq!(admit(&armed(0), &r).err(), Some(Reject::Stale));
    r.valid_until = "not a date".into();
    assert_eq!(
        admit(&armed(0), &r).err(),
        Some(Reject::Stale),
        "an unreadable deadline must not become an unbounded one"
    );
}

/// A roster from a format this build does not understand is refused rather than
/// misread, and the refusal is post-verify (so it is not a crypto oracle).
#[test]
fn a_newer_roster_schema_is_refused_not_misread() {
    let mut r = roster();
    r.schema = 99;
    assert_eq!(admit(&armed(0), &r).err(), Some(Reject::Schema));
}

/// An anchor list of nothing but garbage is `BadKey`, never `Verify`: a broken keyset
/// must not be reported as "this roster's signature was bad", and must certainly not
/// be reported as success.
#[test]
fn a_keyset_of_pure_garbage_is_bad_key_not_a_bad_signature() {
    let (bytes, sig) = published(&roster(), &MASTER_SEED);
    let garbage = Anchor::of(vec![String::new(), "!!not base64!!".into()], 0);
    assert!(
        garbage.is_armed(),
        "precondition: a non-empty list of unusable members still reads as ARMED"
    );
    assert_eq!(
        admit_roster(&garbage, bytes, &sig, NOW).err(),
        Some(Reject::BadKey)
    );
}

/// A one-byte edit anywhere in the index is refused, over the exact raw bytes with no
/// normalization: a trailing newline is a different document.
#[test]
fn a_tampered_index_is_refused_byte_by_byte() {
    let roster = admit(&armed(0), &roster()).unwrap();
    let raw = index_body("m3", 3, 41);
    let sig = sign(&M3_SEED, &raw);
    for i in 0..raw.len() {
        let mut bad = raw.clone();
        bad[i] ^= 0x01;
        assert_eq!(
            roster.authorize_index(bad, &sig).err(),
            Some(Reject::Verify),
            "a flip at index byte {i} must reject"
        );
    }
    let mut trailing = raw.clone();
    trailing.push(b'\n');
    assert_eq!(
        roster.authorize_index(trailing, &sig).err(),
        Some(Reject::Verify),
        "no newline normalization on the signed path"
    );
    assert!(roster.authorize_index(raw, &sig).is_ok());
}

/// The id bind, both directions: a signature cannot be relabelled as another machine, an
/// unattributed index is refused, and an index cannot claim a generation ahead of its
/// roster.
#[test]
fn the_index_must_name_the_machine_and_generation_that_actually_signed_it() {
    let roster = admit(&armed(0), &roster()).unwrap();

    let raw = index_body("m3", 3, 41);
    assert_eq!(
        roster
            .authorize_index(raw, &sign(&M11_SEED, &index_body("m3", 3, 41)))
            .err(),
        Some(Reject::NotAuthorized),
        "a genuine signature must not be wearable under another machine's name"
    );

    let mut s = String::from("schema = 2\nindex_build = 41\n");
    s.push_str("valid_until = \"2027-02-01T00:00:00Z\"\n[programs.ay]\nrepo = \"ay\"\n");
    let bare = s.into_bytes();
    assert_eq!(
        roster
            .authorize_index(bare.clone(), &sign(&M3_SEED, &bare))
            .err(),
        Some(Reject::Unattributed)
    );

    let older = index_body("m3", 2, 41);
    assert!(
        roster
            .authorize_index(older.clone(), &sign(&M3_SEED, &older))
            .is_ok(),
        "an index attributed under an older generation verifies under a newer roster"
    );
    let ahead = index_body("m3", 4, 41);
    assert_eq!(
        roster
            .authorize_index(ahead.clone(), &sign(&M3_SEED, &ahead))
            .err(),
        Some(Reject::SeqMismatch)
    );
}

/// A signature of the wrong length is refused before the crypto, and an index whose
/// bytes verify but whose TOML is malformed or from a newer schema is a post-verify
/// refusal, not an oracle, since it is decided from authenticated bytes.
#[test]
fn cheap_local_gates_and_post_verify_parse_failures_both_fail_closed() {
    let roster = admit(&armed(0), &roster()).unwrap();
    assert_eq!(
        roster
            .authorize_index(index_body("m3", 3, 41), &[0u8; 10])
            .err(),
        Some(Reject::BadSig)
    );
    let garbage = b"this is not toml {{{".to_vec();
    assert_eq!(
        roster
            .authorize_index(garbage.clone(), &sign(&M3_SEED, &garbage))
            .err(),
        Some(Reject::Malformed)
    );
    let newer = b"schema = 99\nindex_build = 1\nvalid_until = \"2027-01-01T00:00:00Z\"\n".to_vec();
    assert_eq!(
        roster
            .authorize_index(newer.clone(), &sign(&M3_SEED, &newer))
            .err(),
        Some(Reject::Schema)
    );
}

/// A genuine signature over bytes holding 0xFF verifies (a `from_utf8_lossy` first would
/// change the bytes), flipping that byte fails, and the parse refuses non-UTF-8 as
/// `Malformed` after the signature.
#[test]
fn raw_non_utf8_bytes_are_verified_without_lossy_conversion() {
    let roster = admit(&armed(0), &roster()).unwrap();
    let mut raw = index_body("m3", 3, 41);
    let bad_idx = raw.len();
    raw.push(0xFF);
    let sig = sign(&M3_SEED, &raw);
    assert_eq!(
        roster.authorize_index(raw.clone(), &sig).err(),
        Some(Reject::Malformed),
        "verification passed over the raw byte; the parser refused it afterwards"
    );
    let mut tampered = raw;
    tampered[bad_idx] ^= 0x01;
    assert_eq!(
        roster.authorize_index(tampered, &sig).err(),
        Some(Reject::Verify)
    );
}

/// now >= valid_until ⇒ Stale; now < valid_until ⇒ Ok.
#[test]
fn freshness_gate_rejects_at_or_after_valid_until() {
    assert_eq!(check_freshness(100, 200), Ok(()));
    assert_eq!(check_freshness(199, 200), Ok(()));
    assert_eq!(check_freshness(200, 200), Err(Reject::Stale));
    assert_eq!(check_freshness(201, 200), Err(Reject::Stale));
}

/// index_build below the recorded Floor ⇒ Rollback; a higher build advances the
/// durable floor (and the file is 0644, readable).
#[test]
fn high_water_floor_blocks_rollback_and_advances() {
    let dir = private_tmp_dir("floor");
    let path = dir.join("index_build.floor");
    let floor = Floor::new(path.clone());
    assert_eq!(floor.check_and_record(41), Ok(()));
    assert_eq!(floor.check_and_record(40), Err(Reject::Rollback));
    assert_eq!(floor.check_and_record(41), Ok(()));
    assert_eq!(floor.check_and_record(50), Ok(()));
    assert_eq!(floor.check_and_record(45), Err(Reject::Rollback));
    assert_eq!(
        Floor::new(path.clone()).check_and_record(49),
        Err(Reject::Rollback)
    );
    #[cfg(unix)]
    {
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o644, "floor file must be readable (0644)");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// What a test runs inside the floor's locked window: between the read and the
/// compare → write of an advance, or before a re-base's write.
type FloorAct = Box<dyn FnOnce(&Path)>;

thread_local! {
    static FLOOR_WINDOW: std::cell::RefCell<Option<FloorAct>> =
        const { std::cell::RefCell::new(None) };
}

/// The floor's locked window: run the act this thread armed, if any.
pub(super) fn floor_window(path: &Path) {
    if let Some(act) = FLOOR_WINDOW.with(|window| window.borrow_mut().take()) {
        act(path);
    }
}

/// THE RATCHET IS READ AND ADVANCED UNDER ITS LOCK (CWE-362): inside the read →
/// compare → write window, a second opener of `<floor>.lock` cannot take it.
#[cfg(unix)]
#[test]
fn the_floor_is_read_and_advanced_under_its_lock() {
    let dir = private_tmp_dir("floor-lock");
    let floor = Floor::new(dir.join("index_build.floor"));
    let advance = || assert_eq!(floor.check_and_record(7), Ok(()));
    let rebase = || floor.rebase(3);
    for (what, touch) in [
        ("an advance", &advance as &dyn Fn()),
        ("a re-base", &rebase),
    ] {
        let lock = dir.join("index_build.floor.lock");
        let held = std::rc::Rc::new(std::cell::Cell::new(None));
        let probe = std::rc::Rc::clone(&held);
        FLOOR_WINDOW.with(|window| {
            *window.borrow_mut() = Some(Box::new(move |_: &Path| {
                let file = std::fs::File::open(&lock).unwrap();
                #[expect(
                    clippy::disallowed_methods,
                    reason = "a probe: released below on the path that takes it"
                )]
                let took = file.try_lock();
                probe.set(Some(matches!(took, Err(std::fs::TryLockError::WouldBlock))));
                if took.is_ok() {
                    let _ = file.unlock();
                }
            }));
        });
        touch();
        assert_eq!(
            held.get(),
            Some(true),
            "{what} ran its window with the lock held"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A ratchet touch that advances nothing does not rewrite the file (on Apple targets a
/// rewrite costs two full drive flushes).
#[cfg(unix)]
#[test]
fn an_unchanged_floor_is_not_rewritten() {
    use std::os::unix::fs::MetadataExt as _;
    let dir = private_tmp_dir("floor-noop");
    let path = dir.join("roster.floor");
    let floor = Floor::new(path.clone());
    assert_eq!(floor.check_and_record(4), Ok(()));

    let witness = dir.join("witness");
    std::fs::hard_link(&path, &witness).unwrap();
    let ino = |p: &Path| std::fs::metadata(p).unwrap().ino();
    assert_eq!(
        ino(&path),
        ino(&witness),
        "precondition: the witness is the same file"
    );

    let recording = aterm_durable::seam::Recording::start();
    assert_eq!(floor.check_and_record(4), Ok(()));
    assert_eq!(recording.take(), [], "nothing to persist, nothing flushed");
    assert_eq!(
        ino(&path),
        ino(&witness),
        "an unchanged floor must not be re-published (temp + 2 fsyncs + rename)"
    );
    assert_eq!(
        floor.current(),
        4,
        "and the value is of course still recorded"
    );
    assert_eq!(floor.check_and_record_classified(4), (Ok(()), None));
    assert_eq!(ino(&path), ino(&witness));

    assert_eq!(floor.check_and_record(5), Ok(()));
    assert_eq!(recording.take(), [aterm_durable::Flush::Durable; 2]);
    assert_ne!(ino(&path), ino(&witness), "an advance must reach the disk");
    assert_eq!(Floor::new(path.clone()).current(), 5);

    for legacy in [0o600, 0o666] {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(legacy)).unwrap();
        assert_eq!(floor.check_and_record(5), Ok(()));
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644,
            "a {legacy:o} floor is re-published 0644 even at an unchanged value"
        );
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    let published = floor.is_published();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(!published, "a mode-000 floor is not published");

    std::fs::write(&path, b"not-a-floor").unwrap();
    assert_eq!(floor.read_floor_classified(), (0, true));
    assert_eq!(floor.check_and_record(0), Ok(()));
    assert_eq!(
        floor.read_floor_classified(),
        (0, false),
        "corruption is HEALED, never skipped over as 'nothing to persist'"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The build floor binds within its generation and a strictly newer one re-bases it, so
/// one machine publishing `index_build = u64::MAX` cannot put the store beyond the index
/// that revokes it.
#[test]
fn the_build_floor_binds_within_its_generation_and_is_rebased_by_a_newer_one() {
    let floor = BuildFloor {
        index_build: 50,
        roster_seq: 7,
    };
    assert!(floor.admits(7, 50), "equal to the floor passes");
    assert!(floor.admits(7, 51));
    assert!(
        !floor.admits(7, 49),
        "below the floor, same generation ⇒ refused"
    );
    assert!(
        floor.admits(8, 1),
        "a newer master-signed generation is not bound by a floor an older one set"
    );
    assert!(
        !floor.admits(6, 49),
        "an OLDER generation must not waive the floor"
    );
    assert!(!floor.admits(0, 49), "nor an absent/unknown one");
    assert!(BuildFloor::none().admits(0, 0));
    assert!(BuildFloor::none().admits(1, u64::MAX));
}

/// The poisoned-floor scenario, end to end over the durable file: a machine drives the
/// floor to the u64 ceiling, and `rebase` — and only `rebase` — brings it back.
#[test]
fn rebase_is_the_only_way_a_floor_ever_comes_down() {
    let dir = private_tmp_dir("floor-rebase");
    let path = dir.join("index_build.floor");
    let floor = Floor::new(path.clone());
    assert_eq!(floor.check_and_record(u64::MAX), Ok(()));
    assert_eq!(floor.current(), u64::MAX);
    assert_eq!(floor.check_and_record(101), Err(Reject::Rollback));
    assert_eq!(floor.current(), u64::MAX, "and the refusal changed nothing");
    floor.rebase(101);
    assert_eq!(Floor::new(path).current(), 101);
    assert_eq!(floor.check_and_record(100), Err(Reject::Rollback));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A floor file that exists but will not read as a floor is detected as corrupt and still
/// reads as `0`: refusing would let a same-uid scribbler wedge the store, and silent
/// first contact would reopen a rollback window unannounced.
#[test]
fn a_corrupt_floor_file_is_detected_and_still_reads_as_first_contact() {
    let dir = private_tmp_dir("floor-corrupt");
    let path = dir.join("roster.floor");
    let floor = Floor::new(path.clone());
    assert_eq!(floor.read_floor_classified(), (0, false));
    assert_eq!(floor.check_and_record(7), Ok(()));
    assert_eq!(floor.read_floor_classified(), (7, false));
    std::fs::write(&path, b" 7\n").unwrap();
    assert_eq!(floor.read_floor_classified(), (7, false));
    let oversized = vec![b'9'; MAX_FLOOR_BYTES + 1];
    for garbage in [
        b"garbage-not-a-number".as_slice(),
        b"00\n".as_slice(),
        b"07\n".as_slice(),
        &[0xFF, 0xFE, 0x00],
        &oversized,
    ] {
        std::fs::write(&path, garbage).unwrap();
        let (value, corrupt) = floor.read_floor_classified();
        assert_eq!(
            value, 0,
            "corrupt must read 0 — a refusal would be a local DoS"
        );
        assert!(
            corrupt,
            "corruption must be DETECTED, never silent first contact"
        );
    }
    assert_eq!(floor.check_and_record(1), Ok(()));
    assert_eq!(floor.read_floor_classified(), (1, false));
    let _ = std::fs::remove_dir_all(&dir);
}

/// An advance that cannot persist keeps the accept but reports the lost advance: the next
/// run re-reads the old floor and accepts the same generation again.
#[cfg(unix)]
#[test]
fn a_failed_floor_persist_keeps_the_accept_and_reports_the_lost_advance() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = private_tmp_dir("floor-persist");
    let path = dir.join("roster.floor");
    let floor = Floor::new(path.clone());
    assert_eq!(floor.check_and_record(7), Ok(()));

    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let (decision, persist_failure) = floor.check_and_record_classified(8);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        decision,
        Ok(()),
        "a persist failure never rejects a passed check"
    );
    assert!(
        persist_failure.is_some(),
        "the lost advance must be reported, not discarded"
    );

    assert_eq!(Floor::new(path.clone()).current(), 7);
    assert_eq!(floor.check_and_record(7), Ok(()));

    let (decision, persist_failure) = floor.check_and_record_classified(8);
    assert_eq!(decision, Ok(()));
    assert_eq!(persist_failure, None);
    assert_eq!(Floor::new(path).current(), 8);

    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
    let (decision, persist_failure) = floor.check_and_record_classified(9);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(decision, Ok(()));
    assert!(
        persist_failure.is_some(),
        "a shared parent refuses the write"
    );
    assert_eq!(floor.current(), 8);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A persist that fails after the temp exists removes the temp: nothing else sweeps
/// `<floor>.<pid>.tmp`, so a store that keeps refusing writes would collect one per pid.
#[test]
fn a_failed_floor_persist_removes_its_temp_file() {
    let dir = private_tmp_dir("floor-temp-litter");
    let path = dir.join("index_build.floor");
    std::fs::create_dir(&path).unwrap();
    let floor = Floor::new(path.clone());
    let (decision, persist_failure) = floor.check_and_record_classified(9);
    assert_eq!(
        decision,
        Ok(()),
        "a persist failure never rejects a passed check"
    );
    assert!(
        persist_failure.is_some(),
        "precondition: this fixture must make the persist fail"
    );

    let litter: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    assert!(
        litter.is_empty(),
        "a failed persist left its temp behind: {litter:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn fifo_and_symlink_floor_return_without_blocking() {
    use std::os::unix::ffi::OsStrExt as _;

    let dir = private_tmp_dir("floor-special");
    let path = dir.join("index_build.floor");
    let path_c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: `path_c` is a live NUL-terminated path in our private fixture.
    assert_eq!(unsafe { libc::mkfifo(path_c.as_ptr(), 0o600) }, 0);
    assert_eq!(Floor::new(path.clone()).current(), 0);
    std::fs::remove_file(&path).unwrap();
    let target = dir.join("foreign-floor");
    std::fs::write(&target, "99\n").unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert_eq!(Floor::new(path).current(), 0, "floor links are refused");
    let _ = std::fs::remove_dir_all(&dir);
}
