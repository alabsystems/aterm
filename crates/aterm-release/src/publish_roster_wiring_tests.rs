// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// NEGATIVE CONTROL: a real upload cannot lose either guard, and the owned
/// snapshot must release the journal lock before a remote check can start.
#[test]
fn upload_authority_is_owned_without_holding_the_journal_lock() {
    let mut cut = ctx(None);
    assert!(release_session(&cut).is_err());
    cut.lease = Some(ReleaseLeaseGuard {
        owner: "a".repeat(40),
    });
    assert!(release_session(&cut).is_err());
    cut.fence = Some(PublisherFenceGuard {
        owner: "a".repeat(40),
        token: "b".repeat(40),
    });
    let mutex = std::sync::Mutex::new(&mut cut);
    let snapshot = upload_release_session(&mutex).unwrap();
    assert!(
        mutex.try_lock().is_ok(),
        "remote checks must not retain the journal lock"
    );
    let (lease, fence) = snapshot.guards.unwrap();
    assert_eq!(lease.owner(), "a".repeat(40));
    assert_eq!(fence.token(), "b".repeat(40));
}

#[test]
fn a_recovery_verifies_under_the_release_key_and_signs_under_its_own() {
    let mut ctx = ctx(Some("m3"));
    ctx.signature_pubkey = Some("Kb-this-machine".to_string());
    assert_eq!(
        ctx.verification_pubkey(),
        Some("Kb-this-machine"),
        "an ordinary cut verifies under the key it signs with"
    );

    ctx.verify_pubkey = Some("Ka-dead-publisher".to_string());
    assert_eq!(
        ctx.verification_pubkey(),
        Some("Ka-dead-publisher"),
        "publish must prove the key that actually signed the artifacts"
    );
    assert_eq!(
        ctx.signature_pubkey.as_deref(),
        Some("Kb-this-machine"),
        "…while the local signing-configuration guards keep comparing this \
         machine's own key, or they would reject their own recovery"
    );
}

/// T-R8. EVERY CUT MODE WRITES THE RELEASE COMMIT'S ROLLED SECTION AS ITS NOTES, read from the
/// commit's objects: a real cut, a dry run and a draft over the same release commit write the
/// same `notes-<v>.md`.
/// NEGATIVE CONTROL: the working tree's own `[Unreleased]` holds another entry, which none of
/// them ships.
#[test]
fn every_cut_mode_writes_the_release_commits_notes() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let repo = dir.path().to_path_buf();
    crate::ledger::testing::init(
        &repo,
        &[
            ("user.name", "Test"),
            ("user.email", "test@example.invalid"),
            ("commit.gpgsign", "false"),
        ],
    );
    let git = |args: &[&str]| {
        let out = crate::ledger::testing::git()
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    fs::write(
        repo.join("CHANGELOG.md"),
        "## [Unreleased]\n\n## [0.5.0] - 2026-11-12\n\n### Fixed\n\n- **Shipped.** In C.\n",
    )
    .unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "release: v0.5.0"]);
    let commit = git(&["rev-parse", "HEAD"]);
    fs::write(
        repo.join("CHANGELOG.md"),
        "## [Unreleased]\n\n### Fixed\n\n- **Tree.** Not shipped.\n",
    )
    .unwrap();
    fs::create_dir(repo.join("dist")).unwrap();
    for kind in [CutKind::Real, CutKind::DryRun, CutKind::Draft] {
        let mut cut = ctx(None);
        cut.repo.clone_from(&repo);
        cut.tree.clone_from(&repo);
        cut.dist = repo.join("dist");
        cut.commit.clone_from(&commit);
        cut.kind = kind;
        write_release_notes(&cut).unwrap();
        assert_eq!(
            fs::read_to_string(cut.notes_path()).unwrap(),
            changelog::release_notes_document("0.5.0", "### Fixed\n\n- **Shipped.** In C."),
            "{kind:?}"
        );
        fs::remove_file(cut.notes_path()).unwrap();
    }
}

fn ctx(machine_id: Option<&str>) -> CutCtx {
    CutCtx {
        credentials: None,
        apple: sign::AppleTier::Inactive,
        repo: PathBuf::from("/nonexistent/repo"),
        tree: PathBuf::from("/nonexistent/repo"),
        dist: PathBuf::from("/nonexistent/repo/dist"),
        journal_path: PathBuf::from("/nonexistent/repo/dist/cut-state.toml"),
        slug: "owner/repo".to_string(),
        version: "0.5.0".to_string(),
        tag: "v0.5.0".to_string(),
        build: 500,
        commit: "a".repeat(40),
        min_build: None,
        artifact: Some("aa".repeat(32)),
        macho_sha256: Some("bb".repeat(32)),
        linux: None,
        manifest_signed: true,
        signature_required: true,
        signature_pubkey: None,
        verify_pubkey: None,
        signature_machine_id: machine_id.map(str::to_string),
        attribution: None,
        roster: None,
        release_id: None,
        release_intent: false,
        upload_intents: Vec::new(),
        kind: CutKind::Real,
        lease: None,
        fence: None,
        journal: None,
        adopted: false,
    }
}

#[cfg(unix)]
struct CurrentRosterCut {
    home: aterm_tempfile::TempDir,
    cut: CutCtx,
    master_key: Vec<u8>,
    master: String,
    signer: String,
    canonical: PathBuf,
}

#[cfg(unix)]
impl CurrentRosterCut {
    fn new() -> Self {
        let home = aterm_tempfile::tempdir().unwrap();
        let (master_key, master) = atpkg_keys::generate().unwrap();
        let (key, signer) = atpkg_keys::generate().unwrap();
        let canonical = home.path().join(atpkg_keys::provision::RELEASE_ROSTER_REL);
        let profile = home.path().join("credentials.toml");
        let encoded = aterm_codec::base64::encode(&key).unwrap();
        fs::write(
            &profile,
            format!(
                "signing_key = \"{encoded}\"\nmachine_roster = \"{}\"\n",
                canonical.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&profile, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let mut cut = ctx(Some("m3"));
        cut.credentials =
            Some(sign::ReleaseCredentials::load(&profile, Some(home.path())).unwrap());
        cut.dist = home.path().join("unchanged");
        cut.journal_path = cut.dist.join("cut-state.toml");
        cut.manifest_signed = false;
        let mut fixture = Self {
            home,
            cut,
            master_key,
            master,
            signer,
            canonical,
        };
        fixture.advance(3, false);
        let observed = fixture.observe(RosterDuty::Sign);
        fixture.cut.signature_required = observed.policy.required;
        fixture.cut.signature_pubkey = observed.policy.pubkey.clone();
        fixture.cut.signature_machine_id = observed.machine_id();
        fixture.cut.attribution = observed.attribution;
        fixture.cut.roster = observed.roster;
        fixture
    }

    fn advance(&self, generation: u64, revoke_other: bool) {
        let body = roster::Roster {
            schema: 1,
            roster_seq: generation,
            valid_until: "9999-12-31T00:00:00Z".into(),
            machines: vec![
                roster::Machine {
                    id: "m3".into(),
                    pubkey: self.signer.clone(),
                    added_at: String::new(),
                    not_after: None,
                },
                roster::Machine {
                    id: "m11".into(),
                    pubkey: aterm_codec::base64::encode(&[0x42; 32]).unwrap(),
                    added_at: String::new(),
                    not_after: None,
                },
            ],
            revoked: if revoke_other {
                vec!["m11".into()]
            } else {
                Vec::new()
            },
        }
        .to_toml()
        .unwrap();
        let signature = atpkg_keys::sign(&self.master_key, body.as_bytes()).unwrap();
        crate::provision::publish_proven_pair(&self.canonical, body.as_bytes(), &signature)
            .unwrap();
    }

    fn observe(&self, duty: RosterDuty) -> SigningVerdict {
        signing_verdict(
            self.cut.credentials.as_ref(),
            &SigningAnchors {
                home: Some(self.home.path()),
                master_pubkeys: &[&self.master],
                identity_path: None,
                now_unix: 1_793_000_000,
                duty,
            },
        )
        .unwrap()
    }

    fn stage(&self, observed: &SigningVerdict, duty: RosterDuty) -> Result<PathBuf> {
        let inputs = manifest_out::ManifestInputs {
            version: &self.cut.version,
            build_number: self.cut.build,
            commit: &self.cut.commit,
            dmg_name: "aterm-0.5.0.dmg",
            dmg_sha256: &"a".repeat(64),
            zip_name: "aterm-0.5.0-mac.zip",
            zip_sha256: &"b".repeat(64),
            repo_slug: "owner/repo",
            min_os: "11.0",
            team_id: "",
            pub_date: "2026-10-26T00:00:00Z",
            min_build: None,
            changelog: "fixture",
        };
        stage_ctx_signing_inputs(&self.cut, &inputs, |cut| {
            revalidate_observed_signature_policy(cut, observed, duty)
        })
    }
}

#[test]
#[cfg(unix)]
fn a_roster_change_after_staging_refuses_signature_but_a_signed_cut_can_finish() {
    let fixture = CurrentRosterCut::new();
    let captured = fixture.observe(RosterDuty::Sign);
    let manifest = fixture.stage(&captured, RosterDuty::Sign).unwrap();
    fixture.advance(4, true);
    let changed = fixture.observe(RosterDuty::Sign);
    assert!(
        sign_ctx_manifest(&fixture.cut, &manifest, |cut| {
            revalidate_observed_signature_policy(cut, &changed, RosterDuty::Sign)
        })
        .is_err()
    );
    assert!(!manifest.with_extension("toml.sig").exists());

    // A separate cut really signs before gaining the completed-receipt semantics.
    let mut signed = CurrentRosterCut::new();
    let current = signed.observe(RosterDuty::Sign);
    let manifest = signed.stage(&current, RosterDuty::Sign).unwrap();
    let signature = sign_ctx_manifest(&signed.cut, &manifest, |cut| {
        revalidate_observed_signature_policy(cut, &current, RosterDuty::Sign)
    })
    .unwrap();
    verify_detached_manifest_signature(
        signed.cut.signature_pubkey.as_deref().unwrap(),
        &fs::read(&manifest).unwrap(),
        &fs::read(&signature).unwrap(),
    )
    .unwrap();
    signed.cut.manifest_signed = true;
    signed.cut.journal = Some(Journal {
        format: JOURNAL_FORMAT,
        version: signed.cut.version.clone(),
        build_number: signed.cut.build,
        commit: signed.cut.commit.clone(),
        min_build: None,
        artifact: signed.cut.artifact.clone(),
        macho_sha256: signed.cut.macho_sha256.clone(),
        linux: None,
        manifest_signed: true,
        signature_required: signed.cut.signature_required,
        signature_pubkey: signed.cut.signature_pubkey.clone(),
        verify_pubkey: signed.cut.verify_pubkey.clone(),
        signature_machine_id: signed.cut.signature_machine_id.clone(),
        release_id: None,
        release_intent: false,
        upload_intents: Vec::new(),
        delete_intent: None,
        done: vec!["lock".into()],
    });
    signed.cut.mark("sign").unwrap();
    signed.advance(4, true);
    let duty = roster_duty(signed.cut.is_done("sign"));
    assert_eq!(duty, RosterDuty::Finish);
    let finish = signed.observe(duty);
    assert!(finish.attribution.is_none());
    assert!(finish.roster.is_none());
    revalidate_observed_signature_policy(&signed.cut, &finish, duty).unwrap();
}

#[test]
#[cfg(unix)]
fn a_fresh_roster_for_the_same_signer_refuses_the_captured_context_before_staging() {
    let mut fixture = CurrentRosterCut::new();
    let unchanged = fixture.observe(RosterDuty::Sign);
    let manifest = fixture.stage(&unchanged, RosterDuty::Sign).unwrap();
    assert_eq!(
        Manifest::parse(&fs::read_to_string(&manifest).unwrap())
            .unwrap()
            .roster_seq,
        Some(3)
    );
    assert_eq!(
        machines::RosterDocument::read(&fixture.cut.dist.join(roster::ROSTER_ASSET)).unwrap(),
        fixture.cut.roster.clone().unwrap()
    );
    fixture.cut.dist = fixture.home.path().join("changed");
    fixture.advance(4, true);
    let fresh = fixture.observe(RosterDuty::Sign);
    assert_eq!(fresh.policy.required, fixture.cut.signature_required);
    assert_eq!(fresh.policy.pubkey, fixture.cut.signature_pubkey);
    assert_eq!(fresh.machine_id(), fixture.cut.signature_machine_id);
    assert_eq!(fresh.attribution.as_ref().unwrap().roster_seq, 4);
    assert_eq!(fixture.cut.attribution.as_ref().unwrap().roster_seq, 3);
    let result = fixture.stage(&fresh, RosterDuty::Sign);
    if let Ok(path) = result.as_ref() {
        assert_eq!(
            Manifest::parse(&fs::read_to_string(path).unwrap())
                .unwrap()
                .roster_seq,
            Some(3)
        );
        assert_eq!(
            machines::RosterDocument::read(&fixture.cut.dist.join(roster::ROSTER_ASSET)).unwrap(),
            fixture.cut.roster.clone().unwrap()
        );
    }
    assert!(
        result.is_err(),
        "fresh authority must not admit a different captured roster"
    );
    assert!(!fixture.cut.manifest_path().exists());
    assert!(!fixture.cut.dist.join(roster::ROSTER_ASSET).exists());
    assert!(
        !fixture
            .cut
            .manifest_path()
            .with_extension("toml.sig")
            .exists()
    );
}

fn names(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .filter_map(|p| p.file_name()?.to_str().map(str::to_string))
        .collect()
}

#[test]
fn the_lineage_check_reads_the_roster_this_cut_ships_not_a_dist_leftover() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let mut cut = ctx(Some("m3"));
    cut.dist = tmp.path().to_path_buf();
    fs::write(
        cut.dist.join(roster::ROSTER_ASSET),
        b"an earlier generation",
    )
    .unwrap();
    cut.roster = Some(machines::RosterDocument {
        bytes: b"the profile's generation".to_vec(),
        signature: Vec::new(),
    });
    assert_eq!(
        shipped_roster(&cut).as_deref(),
        Some(&b"the profile's generation"[..])
    );
    cut.roster = None;
    assert_eq!(
        shipped_roster(&cut).as_deref(),
        Some(&b"an earlier generation"[..]),
        "a recovered cut ships the pair it rebuilt in dist/"
    );
    cut.signature_machine_id = None;
    assert_eq!(
        shipped_roster(&cut),
        None,
        "an unrostered cut ships no roster"
    );
}

#[test]
fn an_attributed_cut_carries_its_roster_through_every_asset_set() {
    let rostered = ctx(Some("m3"));
    assert!(rostered.attaches_roster());
    for (label, set) in [
        ("channel upload", rostered.channel_asset_paths()),
        (
            "channel acceptance",
            rostered
                .channel_asset_names()
                .iter()
                .map(PathBuf::from)
                .collect(),
        ),
    ] {
        let set = names(&set);
        assert!(
            set.contains(&roster::ROSTER_ASSET.to_string()),
            "the {label} set must carry the roster: {set:?}"
        );
        assert!(
            set.contains(&roster::ROSTER_SIG_ASSET.to_string()),
            "the {label} set must carry the roster signature: {set:?}"
        );
        assert!(set.contains(&"aterm-appcast.toml".to_string()), "{set:?}");
    }

    let plain = ctx(None);
    assert!(!plain.attaches_roster());
    let set = names(&plain.channel_asset_paths());
    assert!(
        !set.iter().any(|n| n.starts_with("aterm-machines")),
        "an unattributed cut must publish no roster: {set:?}"
    );
}

#[test]
fn the_pin_expectation_is_the_committed_master_whoever_signs() {
    let master = aterm_update_core::pins::PAPER_MASTER_PUBKEYS
        .first()
        .copied()
        .expect("precondition: this tree pins a paper master");
    let one = "QkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkI=";
    let two = "Q0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0M=";
    let mut signing_one = ctx(None);
    signing_one.signature_pubkey = Some(one.to_string());
    let mut signing_two = ctx(None);
    signing_two.signature_pubkey = Some(two.to_string());
    let expected = signing_one.expected_embedded_pin().unwrap();
    assert_eq!(
        expected,
        signing_two.expected_embedded_pin().unwrap(),
        "the binary embeds the COMMITTED anchor, so the cutting machine cannot move \
         what the build and the self-check expect of it"
    );
    assert_eq!(expected, Some(update_key_fingerprint(master).unwrap()));
    assert_eq!(
        expected_embedded_update_pin(&[]).unwrap(),
        None,
        "a fork with no master has no anchor to prove"
    );
    assert_ne!(
        expected,
        Some(update_key_fingerprint(one).unwrap()),
        "deriving from the signer is the trap; the two must be distinguishable"
    );
}

#[test]
fn the_ratchet_reads_both_generations_out_of_manifest_bytes() {
    let scratch = crate::test_scratch_dir("ratchet");
    let dir = scratch.path();

    let mut manifest = manifest_out::build(&manifest_out::ManifestInputs {
        version: "0.5.0",
        build_number: 500,
        commit: &"a".repeat(40),
        dmg_name: "aterm-0.5.0.dmg",
        dmg_sha256: &"ab".repeat(32),
        zip_name: "aterm-0.5.0-mac.zip",
        zip_sha256: &"cd".repeat(32),
        repo_slug: "owner/repo",
        min_os: "11.0",
        team_id: "",
        pub_date: "2026-08-11T00:00:00Z",
        min_build: None,
        changelog: "### Added\n- a thing\n",
    });
    let unattributed = manifest.to_toml().unwrap();
    machines::attribute(
        &mut manifest,
        &roster::Attribution {
            machine_id: "m3".to_string(),
            pubkey_b64: "k".to_string(),
            roster_seq: 7,
        },
    );
    let attributed = manifest.to_toml().unwrap();

    let head = |text: &str| verify::Published {
        release_id: Some(1),
        release: None,
        tag: "v0.5.0".to_string(),
        build: 500,
        version: "0.5.0".to_string(),
        asset: manifest_out::MANIFEST_ASSET.to_string(),
        min_build: None,
        text: text.to_string(),
    };
    assert_eq!(published_roster_seq(None).unwrap(), None);
    assert_eq!(
        published_roster_seq(Some(&head(&unattributed))).unwrap(),
        None,
        "an unrostered head imposes no floor — the fork state"
    );
    assert_eq!(
        published_roster_seq(Some(&head(&attributed))).unwrap(),
        Some(7)
    );

    let mut fresh = ctx(Some("m3"));
    fresh.attribution = Some(roster::Attribution {
        machine_id: "m3".to_string(),
        pubkey_b64: "k".to_string(),
        roster_seq: 7,
    });
    assert_eq!(cut_roster_seq(&fresh).unwrap(), Some(7));

    let mut resumed = ctx(Some("m3"));
    resumed.dist = dir.to_path_buf();
    fs::write(dir.join(manifest_out::MANIFEST_ASSET), &attributed).unwrap();
    assert_eq!(
        cut_roster_seq(&resumed).unwrap(),
        Some(7),
        "a resume past build carries no attribution and must read its own manifest"
    );
    assert_eq!(cut_roster_seq(&ctx(None)).unwrap(), None);
    let mut lost = ctx(Some("m3"));
    lost.dist = dir.join("gone");
    assert!(cut_roster_seq(&lost).is_err());
}

#[test]
fn recovery_rebuilds_exactly_the_roster_assets_publish_will_demand() {
    let mut manifest = manifest_out::build(&manifest_out::ManifestInputs {
        version: "0.5.0",
        build_number: 500,
        commit: &"a".repeat(40),
        dmg_name: "aterm-0.5.0.dmg",
        dmg_sha256: &"ab".repeat(32),
        zip_name: "aterm-0.5.0-mac.zip",
        zip_sha256: &"cd".repeat(32),
        repo_slug: "owner/repo",
        min_os: "11.0",
        team_id: "",
        pub_date: "2026-08-11T00:00:00Z",
        min_build: None,
        changelog: "### Added\n- a thing\n",
    });
    assert!(recovered_roster_asset_names(&manifest).is_empty());
    assert!(
        !names(&ctx(manifest.machine_id.as_deref()).channel_asset_paths())
            .iter()
            .any(|name| name.starts_with("aterm-machines"))
    );

    machines::attribute(
        &mut manifest,
        &roster::Attribution {
            machine_id: "m3".to_string(),
            pubkey_b64: "k".to_string(),
            roster_seq: 4,
        },
    );
    let rebuilt: Vec<String> = recovered_roster_asset_names(&manifest)
        .into_iter()
        .map(str::to_string)
        .collect();
    let demanded = names(&ctx(manifest.machine_id.as_deref()).channel_asset_paths());
    for name in &rebuilt {
        assert!(demanded.contains(name), "{name} rebuilt but not demanded");
    }
    for name in demanded.iter().filter(|n| n.starts_with("aterm-machines")) {
        assert!(rebuilt.contains(name), "{name} demanded but never rebuilt");
    }
    assert_eq!(rebuilt.len(), 2, "{rebuilt:?}");
}

#[test]
fn recovery_refuses_to_overwrite_a_newer_local_roster() {
    let scratch = crate::test_scratch_dir("downgrad");
    let dir = scratch.path();

    assert!(refuse_roster_downgrade(dir, 1).is_ok());

    let bytes = aterm_codec::base64::decode_strict(ROSTER_SEQ2.concat().as_bytes()).unwrap();
    let sig = aterm_codec::base64::decode_strict(ROSTER_SEQ2_SIG.as_bytes()).unwrap();
    std::fs::write(dir.join(roster::ROSTER_ASSET), &bytes).unwrap();
    std::fs::write(dir.join(roster::ROSTER_SIG_ASSET), &sig).unwrap();

    let err = refuse_roster_downgrade(dir, 1).unwrap_err();
    assert!(err.0.contains("NEWER"), "{}", err.0);
    assert!(err.0.contains(roster::ROSTER_ASSET), "{}", err.0);

    assert!(refuse_roster_downgrade(dir, 2).is_ok());
    assert!(refuse_roster_downgrade(dir, 3).is_ok());

    std::fs::write(dir.join(roster::ROSTER_SIG_ASSET), [0u8; 64]).unwrap();
    assert!(refuse_roster_downgrade(dir, 1).is_ok());
}

#[test]
fn an_unattributed_release_keeps_the_policy_key() {
    let manifest = b"schema = 1\nversion = \"0.20.0\"\nbuild_number = 1786405661\n\
sha256 = \"aa\"\ndmg = \"aterm-0.20.0.dmg\"\n";
    let resolved = published_manifest_signature_pubkey(
        "unused/slug",
        0,
        manifest,
        Some("cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8="),
    )
    .expect("an unattributed manifest resolves without any download");
    assert_eq!(
        resolved.as_deref(),
        Some("cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8=")
    );
}

#[test]
fn prove_roster_on_disk_enforces_liveness_non_revocation_pubkey_binding_and_unrostered_cleanliness()
{
    use ring::signature::{Ed25519KeyPair, KeyPair};

    let scratch = crate::test_scratch_dir("prove");
    let dir = scratch.path();

    let master_kp = Ed25519KeyPair::from_seed_unchecked(&[0x51; 32]).unwrap();
    let master_pk = aterm_codec::base64::encode(master_kp.public_key().as_ref()).unwrap();
    let m3_kp = Ed25519KeyPair::from_seed_unchecked(&[0x52; 32]).unwrap();
    let m3_pk = aterm_codec::base64::encode(m3_kp.public_key().as_ref()).unwrap();
    let other_kp = Ed25519KeyPair::from_seed_unchecked(&[0x53; 32]).unwrap();
    let other_pk = aterm_codec::base64::encode(other_kp.public_key().as_ref()).unwrap();

    let write_roster = |revoked: &[&str], pubkey: &str| {
        let doc = roster::Roster {
            schema: 1,
            roster_seq: 6,
            valid_until: "2027-02-01T00:00:00Z".into(),
            machines: vec![roster::Machine {
                id: "m3".into(),
                pubkey: pubkey.to_string(),
                added_at: "2026-08-04T00:00:00Z".into(),
                not_after: None,
            }],
            revoked: revoked.iter().map(|id| (*id).to_string()).collect(),
        };
        let bytes = doc.to_toml().unwrap().into_bytes();
        let sig = master_kp.sign(&bytes).as_ref().to_vec();
        std::fs::write(dir.join(roster::ROSTER_ASSET), &bytes).unwrap();
        std::fs::write(dir.join(roster::ROSTER_SIG_ASSET), &sig).unwrap();
    };

    let base_manifest = || {
        manifest_out::build(&manifest_out::ManifestInputs {
            version: "0.5.0",
            build_number: 500,
            commit: &"a".repeat(40),
            dmg_name: "aterm-0.5.0.dmg",
            dmg_sha256: &"ab".repeat(32),
            zip_name: "aterm-0.5.0-mac.zip",
            zip_sha256: &"cd".repeat(32),
            repo_slug: "owner/repo",
            min_os: "11.0",
            team_id: "",
            pub_date: "2026-08-11T00:00:00Z",
            min_build: None,
            changelog: "### Added\n- a thing\n",
        })
    };

    let mut attributed = base_manifest();
    machines::attribute(
        &mut attributed,
        &roster::Attribution {
            machine_id: "m3".to_string(),
            pubkey_b64: m3_pk.clone(),
            roster_seq: 5,
        },
    );

    let mut rostered_ctx = ctx(Some("m3"));
    rostered_ctx.dist = dir.to_path_buf();
    rostered_ctx.signature_pubkey = Some(m3_pk.clone());
    let now_ok = 1_790_000_000;
    let masters = [master_pk.as_str()];

    write_roster(&[], &m3_pk);
    prove_roster_on_disk_at(&rostered_ctx, &attributed, &masters, now_ok)
        .expect("valid staged roster matching verification pubkey passes");

    write_roster(&["m3"], &m3_pk);
    let revoked_err = prove_roster_on_disk_at(&rostered_ctx, &attributed, &masters, now_ok)
        .expect_err("staged roster revoking the cut's machine must be refused");
    assert!(
        revoked_err.0.contains("does not admit machine"),
        "{}",
        revoked_err.0
    );

    write_roster(&[], &other_pk);
    let key_err = prove_roster_on_disk_at(&rostered_ctx, &attributed, &masters, now_ok)
        .expect_err("staged roster binding machine to a different pubkey must be refused");
    assert!(key_err.0.contains("binds machine"), "{}", key_err.0);

    write_roster(&[], &m3_pk);
    let lapsed_err = prove_roster_on_disk_at(&rostered_ctx, &attributed, &masters, 1_900_000_000)
        .expect_err("lapsed staged roster must be refused");
    assert!(
        lapsed_err.0.contains("not a fresh roster"),
        "{}",
        lapsed_err.0
    );

    let unrostered_ctx = ctx(None);
    prove_roster_on_disk_at(&unrostered_ctx, &base_manifest(), &masters, now_ok)
        .expect("unattributed manifest on unrostered cut passes");
    let unrostered_err = prove_roster_on_disk_at(&unrostered_ctx, &attributed, &masters, now_ok)
        .expect_err("attributed manifest on unrostered cut must be refused");
    assert!(
        unrostered_err.0.contains("attaches no machine roster"),
        "{}",
        unrostered_err.0
    );
}

const ROSTER_SEQ2: &[&str] = &[
    "c2NoZW1hID0gMQpyb3N0ZXJfc2VxID0gMgp2YWxpZF91bnRpbCA9ICI5OTk5LTEyLTMxVDAwOjAwOjAwWiIKcmV2",
    "b2tlZCA9IFtdCgpbW21hY2hpbmVdXQppZCA9ICJpbmN1bWJlbnQtaGVhZCIKcHVia2V5ID0gImN3NWdJR1lRelg2",
    "eHJoVFhqWFU5bllmTFdlb0lraVoxeVVYN2Qxd21kejg9IgphZGRlZF9hdCA9ICIyMDI2LTA4LTE1VDIxOjU1OjA0",
    "WiIKCltbbWFjaGluZV1dCmlkID0gIm0zIgpwdWJrZXkgPSAiWU9IdzBPb2VmUTc5TmRFOHFzUUZvYklNUjdRWENo",
    "cHJlWUJpMk9mNzRVbz0iCmFkZGVkX2F0ID0gIjIwMjYtMDgtMTVUMjE6NTU6MDRaIgo=",
];
const ROSTER_SEQ2_SIG: &str =
    "vNOvNYPssbUN3F/SmnoPDk6za2BAaewu9Vopl5YU7EDd+KUM0Y84eUryvFE9OWUywT/yggXE92SYQ2Qz7k56DA==";
