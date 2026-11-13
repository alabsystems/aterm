// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

mod channel_roster {
    use super::super::channel_roster_pair;
    use agh_fake::{AssetSpec, FakeGithub, FakeOrigin, Fault, Match, ReleaseSpec, RepoSpec};
    use aterm_update_core::github_rest::Origins;
    use aterm_update_core::release_asset::Reader;
    use aterm_update_core::roster::{ROSTER_ASSET, ROSTER_SIG_ASSET};
    use std::time::Duration;

    const SLUG: &str = "fixture-dev/app";

    /// agh-fake with `SLUG`'s latest release `v2`, carrying the roster pair when `roster`.
    fn channel(roster: bool) -> FakeGithub {
        let fake = FakeGithub::start();
        fake.add_repo(SLUG, RepoSpec::public().push("fixture-dev-bot"));
        let old = fake.add_release(SLUG, ReleaseSpec::published("v1", "2026-10-01T00:00:00Z"));
        fake.add_asset(old, AssetSpec::uploaded(ROSTER_ASSET, b"old roster"));
        let id = fake.add_release(SLUG, ReleaseSpec::published("v2", "2026-10-02T00:00:00Z"));
        if roster {
            fake.add_asset(id, AssetSpec::uploaded(ROSTER_ASSET, b"roster"));
            fake.add_asset(id, AssetSpec::uploaded(ROSTER_SIG_ASSET, b"sig"));
        }
        fake
    }

    fn reader(fake: &FakeGithub, patience: Duration) -> Reader {
        let p = fake.ports();
        Reader::loopback(
            Origins::loopback(p.api, p.uploads, p.web, p.cdn),
            None,
            patience,
        )
    }

    fn gateway_timeout() -> Fault {
        Fault::Status {
            status: 504,
            body: String::new(),
            headers: Vec::new(),
        }
    }

    #[test]
    fn a_web_route_504_reads_the_latest_pair_through_the_api_route() {
        let fake = channel(true);
        fake.fault_always(Match::web("/fixture-dev/app/releases/"), gateway_timeout());
        let pair = channel_roster_pair(&reader(&fake, aterm_spec::wait::MUST_HAPPEN), SLUG);
        let expected = super::super::RosterDocument {
            bytes: b"roster".to_vec(),
            signature: b"sig".to_vec(),
        };
        assert_eq!(pair, Ok(Some(expected)));
        assert!(
            fake.seen().iter().any(|s| s.origin == FakeOrigin::Api),
            "the API route served it"
        );
    }

    /// NEGATIVE CONTROL: a latest release without a roster is absence, read on the web route
    /// alone; the API is never asked.
    #[test]
    fn a_latest_release_without_a_roster_is_absent_and_asks_the_api_nothing() {
        let fake = channel(false);
        let pair = channel_roster_pair(&reader(&fake, aterm_spec::wait::MUST_HAPPEN), SLUG);
        assert_eq!(
            pair,
            Ok(None),
            "the older release's roster is not the latest's"
        );
        assert!(fake.seen().iter().all(|s| s.origin != FakeOrigin::Api));
    }

    #[test]
    fn both_routes_failing_names_both_and_never_reads_as_absence() {
        let fake = channel(true);
        fake.fault_always(Match::web("/fixture-dev/app/releases/"), gateway_timeout());
        fake.fault_always(
            Match::api("/repos/fixture-dev/app/releases"),
            gateway_timeout(),
        );
        let error =
            channel_roster_pair(&reader(&fake, Duration::from_millis(300)), SLUG).unwrap_err();
        assert!(
            error.contains("web route") && error.contains("API route"),
            "{error}"
        );
    }
}

/// Standard padded Base64 — the shipped encoder (`aterm_codec::base64`).
fn b64(raw: &[u8]) -> String {
    aterm_codec::base64::encode(raw).expect("test key material is far below MAX_INPUT_LEN")
}
use ring::signature::{Ed25519KeyPair, KeyPair};

const MASTER: [u8; 32] = [0x51; 32];
const M3: [u8; 32] = [0x52; 32];
const M11: [u8; 32] = [0x53; 32];

fn kp(seed: &[u8; 32]) -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(seed).unwrap()
}

fn pk(seed: &[u8; 32]) -> String {
    b64(kp(seed).public_key().as_ref())
}

/// A roster listing m3 and m11, with `revoked` under the caller's control.
fn roster(revoked: &[&str]) -> (Vec<u8>, Vec<u8>, String) {
    let r = Roster {
        schema: 1,
        roster_seq: 6,
        valid_until: "2027-02-01T00:00:00Z".into(),
        machines: vec![
            aterm_update_core::roster::Machine {
                id: "m3".into(),
                pubkey: pk(&M3),
                added_at: "2026-08-04T00:00:00Z".into(),
                not_after: None,
            },
            aterm_update_core::roster::Machine {
                id: "m11".into(),
                pubkey: pk(&M11),
                added_at: "2026-08-04T00:00:00Z".into(),
                not_after: None,
            },
        ],
        revoked: revoked.iter().map(|s| (*s).to_string()).collect(),
    };
    let bytes = r.to_toml().unwrap().into_bytes();
    let sig = kp(&MASTER).sign(&bytes).as_ref().to_vec();
    (bytes, sig, pk(&MASTER))
}

/// The fixture roster's `valid_until`, 2027-02-01T00:00:00Z, as unix seconds.
const FIXTURE_VALID_UNTIL: i64 = 1_801_440_000;

/// A roster still valid but within [`MIN_REMAINING_WINDOW_SECS`] of lapsing may not start a
/// cut: pins admitting at the horizon, not at `now`.
#[test]
fn a_roster_about_to_lapse_may_not_start_a_cut() {
    let one_second_left = FIXTURE_VALID_UNTIL - 1;
    let (bytes, sig, master) = roster(&[]);
    authorize_cut(&[&master], bytes, &sig, &pk(&M3), one_second_left).unwrap_err();
    let (bytes, sig, master) = roster(&[]);
    let verified = verify_roster(&[&master], bytes, &sig).unwrap();
    Roster::parse(&verified)
        .unwrap()
        .admit(0, one_second_left)
        .expect("the client's bound is still open — only the producer looks ahead");

    let just_short = FIXTURE_VALID_UNTIL - MIN_REMAINING_WINDOW_SECS;
    let (bytes, sig, master) = roster(&[]);
    assert!(authorize_cut(&[&master], bytes, &sig, &pk(&M3), just_short).is_err());
    let (bytes, sig, master) = roster(&[]);
    assert!(authorize_cut(&[&master], bytes, &sig, &pk(&M3), just_short - 1).is_ok());
}

/// Two master-signed documents at the same generation are a lineage fork, which the number
/// admits, so the bytes decide.
#[test]
fn an_equal_generation_with_a_different_document_is_a_lineage_fork() {
    let ours = b"roster_seq = 4\n[[machine]]\nid = \"m3\"\n".to_vec();
    let theirs = b"roster_seq = 4\n[[machine]]\nid = \"m19\"\n".to_vec();
    assert!(roster_lineage_agrees(&ours, Some(4), Some(&(4, ours.clone()))).is_ok());
    roster_lineage_agrees(&ours, Some(4), Some(&(4, theirs.clone()))).unwrap_err();
    assert!(roster_lineage_agrees(&ours, Some(4), Some(&(3, theirs.clone()))).is_ok());
    assert!(roster_lineage_agrees(&ours, Some(3), Some(&(4, theirs))).is_ok());
    assert!(roster_lineage_agrees(&ours, Some(4), None).is_ok());
    assert!(roster_lineage_agrees(&ours, None, Some(&(4, ours.clone()))).is_ok());
}

/// A recovered roster must be the one the published manifest names; freshness and
/// revocation are not re-judged.
#[test]
fn a_recovered_roster_is_bound_to_the_manifest_that_names_it() {
    let (bytes, sig, master) = roster(&[]);
    verify_published_roster(&[&master], bytes.clone(), &sig, "m3", Some(6))
        .expect("the roster this manifest names");
    let lapsed = Roster::parse(&verify_roster(&[&master], bytes.clone(), &sig).unwrap())
        .unwrap()
        .admit(0, 1_900_000_000);
    assert!(lapsed.is_err(), "precondition: these bytes are lapsed");
    verify_published_roster(&[&master], bytes.clone(), &sig, "m3", Some(6))
        .expect("recovery does not re-judge a window it cannot change");

    verify_published_roster(&[&master], bytes.clone(), &sig, "m3", Some(7)).unwrap_err();
    verify_published_roster(&[&master], bytes.clone(), &sig, "m3", None).unwrap_err();
    verify_published_roster(&[&master], bytes.clone(), &sig, "m3", Some(5))
        .expect("a newer roster asset over an older attribution is what a join produces");
    verify_published_roster(&[&master], bytes.clone(), &sig, "m99", Some(6)).unwrap_err();
    verify_published_roster(&[], bytes, &sig, "m3", Some(6)).unwrap_err();
}

#[test]
fn a_missing_identity_is_none_and_a_broken_one_is_an_error() {
    let scratch = crate::test_scratch_dir("machine");
    let dir = scratch.path();
    let path = dir.join("machine.toml");
    assert_eq!(MachineIdentity::read(&path).unwrap(), None);

    std::fs::write(&path, "id = \"m3\"\npubkey = \"abc\"\nminted_at = \"x\"\n").unwrap();
    let me = MachineIdentity::read(&path).unwrap().unwrap();
    assert_eq!(me.id, "m3");
    assert_eq!(me.pubkey, "abc");

    std::fs::write(&path, "id = \"\"\npubkey = \"abc\"\n").unwrap();
    assert!(
        MachineIdentity::read(&path).is_err(),
        "an empty id is not an identity"
    );

    std::fs::write(&path, "this is not toml = = =").unwrap();
    assert!(MachineIdentity::read(&path).is_err());
}

/// Release attribution reads `~/.aterm/release-ring/machine.toml` and ignores the host's own
/// `~/.aterm/machine.toml` (`id != m22` in 0.146).
/// NEGATIVE CONTROL: reading `.aterm/machine.toml` returns `"m99"` instead of `"m22"` (or
/// `Some("m99")` when the release ring is absent).
#[test]
fn release_ring_identity_path_ignores_host_machine_toml() {
    let scratch = crate::test_scratch_dir("ring-id");
    let home = scratch.path();
    std::fs::create_dir_all(home.join(".aterm")).unwrap();
    std::fs::write(
        home.join(".aterm/machine.toml"),
        "id = \"m99\"\npubkey = \"host-key\"\n",
    )
    .unwrap();

    let ring_record = identity_path_in(home);
    assert_eq!(
        declared_machine_id(None, Some(&ring_record)).unwrap(),
        None,
        "host machine.toml alone never supplies a release machine id"
    );

    std::fs::create_dir_all(ring_record.parent().unwrap()).unwrap();
    std::fs::write(&ring_record, "id = \"m22\"\npubkey = \"ring-key\"\n").unwrap();
    assert_eq!(
        declared_machine_id(None, Some(&ring_record)).unwrap(),
        Some("m22".to_string()),
        "release-ring/machine.toml wins over ~/.aterm/machine.toml"
    );
}

#[cfg(unix)]
#[test]
fn machine_identity_and_roster_refuse_symlinks() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let real = dir.path().join("real.toml");
    std::fs::write(&real, "id = \"m3\"\npubkey = \"abc\"\n").unwrap();
    let link = dir.path().join("machine.toml");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert!(MachineIdentity::read(&link).is_err());

    let roster = dir.path().join("aterm-machines.toml");
    let sig = super::RosterDocument::signature_path(&roster);
    std::fs::write(&sig, b"sig").unwrap();
    std::os::unix::fs::symlink(&real, &roster).unwrap();
    assert!(super::RosterDocument::read(&roster).is_err());
}
