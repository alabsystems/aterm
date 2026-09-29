// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use ring::signature::{Ed25519KeyPair, KeyPair};

struct Fixture {
    root: PathBuf,
    context: Context,
    state: State,
    proof: Proof,
    /// The paper master the fixture's roster is signed by — the one anchor (K1
    /// retired): an appcast verifies only through a roster this key signed.
    master: String,
}

/// The fixture's machine: the roster admits it as `fixture` at sequence 1, and it
/// signs the appcast.
fn machine() -> Ed25519KeyPair {
    key(7)
}

/// The fixture's paper master, which signs the roster.
fn master() -> Ed25519KeyPair {
    key(11)
}

/// A master-signed roster admitting [`machine`] as `fixture` at `seq`.
fn roster_proof(seq: u64) -> (Vec<u8>, Vec<u8>) {
    let roster = aterm_update_core::Roster {
        schema: 1,
        roster_seq: seq,
        valid_until: "2099-01-01T00:00:00Z".into(),
        machines: vec![aterm_update_core::Machine {
            id: "fixture".into(),
            pubkey: public(&machine()),
            added_at: String::new(),
            not_after: None,
        }],
        revoked: Vec::new(),
    };
    let bytes = roster.to_toml().unwrap().into_bytes();
    let signature = master().sign(&bytes).as_ref().to_vec();
    (bytes, signature)
}

fn key(seed: u8) -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap()
}

fn public(key: &Ed25519KeyPair) -> String {
    aterm_codec::base64::encode(key.public_key().as_ref()).unwrap()
}

fn settings_provider(auto_apply: bool) -> crate::CheckSettingsProvider {
    std::sync::Arc::new(move || {
        Some(crate::CheckSettings {
            source: Source {
                owner: "alabsystems".into(),
                repo: "aterm".into(),
            },
            auto_apply,
        })
    })
}

#[test]
fn auto_apply_requires_current_source_and_available_enabled_settings() {
    let source = settings_provider(true)().unwrap().source;
    for configured in [false, true] {
        let provider = settings_provider(configured);
        assert_eq!(automatic_apply_allowed(&source, &provider), configured);
        let changed_source = Source {
            owner: "different".into(),
            repo: source.repo.clone(),
        };
        assert!(!automatic_apply_allowed(&changed_source, &provider));
    }
    let unavailable: crate::CheckSettingsProvider = std::sync::Arc::new(|| None);
    assert!(!automatic_apply_allowed(&source, &unavailable));
}

#[test]
fn a_manual_request_cannot_gain_apply_authority_from_a_later_automatic_setting() {
    for requested in [false, true] {
        for current in [false, true] {
            let live = settings_provider(current);
            let sampled = request_apply_provider(&live, requested)().unwrap();
            assert_eq!(sampled.auto_apply, requested && current);
            assert_eq!(sampled.source, live().unwrap().source);
        }
    }
    let unavailable: crate::CheckSettingsProvider = std::sync::Arc::new(|| None);
    assert!(request_apply_provider(&unavailable, true)().is_none());
}

fn auto_apply_model() -> aterm_spec::derive::Model {
    aterm_spec::ty_model! {
        LinuxAutomaticReplacementPolicy {
            const Buggy = 0;
            var requested = 1;
            var allowed = 1;
            var prepared = 0;
            var replaced = 0;
            action RequestManual when (prepared == 0 && requested == 1) { requested = 0; }
            action Prepare when (prepared == 0) { prepared = 1; }
            action Veto when (prepared == 1 && replaced == 0 && allowed == 1) { allowed = 0; }
            action Replace when (prepared == 1 && requested == 1 && allowed == 1 && replaced == 0) { replaced = 1; }
            action UnsafeReplace when (Buggy == 1 && prepared == 1 && replaced == 0) { replaced = 1; }
            invariant CurrentPolicyRequired: replaced == 0 || (requested == 1 && allowed == 1);
        }
    }
}

#[test]
fn auto_apply_model_catches_a_missing_final_veto_and_matches_the_shipping_settings_predicate() {
    let model = auto_apply_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    for requested in [false, true] {
        for allowed in [false, true] {
            let live = settings_provider(allowed);
            let source = live().unwrap().source;
            let provider = request_apply_provider(&live, requested);
            let projection = std::collections::BTreeMap::from([
                ("requested", i64::from(requested)),
                ("allowed", i64::from(allowed)),
                ("prepared", 1),
                ("replaced", 0),
            ]);
            assert_eq!(
                model.action_enabled("Replace", &projection),
                automatic_apply_allowed(&source, &provider)
            );
        }
    }
}

#[test]
fn manual_policy_stages_exact_authenticated_identity_and_explicit_apply_still_works() {
    let mut f = Fixture::new();
    let old = hash_file(&f.context.target).unwrap();
    let provider = settings_provider(false);
    let source = provider().unwrap().source;
    f.proof.save(&f.context.dir).unwrap();
    finish_candidate(
        &f.context,
        &mut f.state,
        &f.proof,
        Pins {
            masters: &[&f.master],
        },
        &source,
        &provider,
        |_, _, _| Ok(()),
    )
    .unwrap();
    assert_eq!(hash_file(&f.context.target).unwrap(), old);
    assert_eq!(f.state.high_water, 10);
    assert!(f.state.trial.is_none());
    assert_eq!(f.state.staged.as_ref().unwrap().build, 11);
    let durable = f.context.read_state().unwrap().unwrap();
    let status = linux_status(&f.context, &durable);
    assert_eq!(status.installed_build, 10);
    assert_eq!(status.staged_build, Some(11));
    assert!(status.trial_phase.is_none());
    assert!(durable.outcome.contains("aterm update apply"));
    // Explicit apply is not routed through the automatic-policy provider.
    f.apply().unwrap();
    assert_eq!(f.state.installed.build, 11);
    assert!(f.state.staged.is_none());
    let status = linux_status(&f.context, &f.state);
    assert_eq!(status.staged_build, None);
    assert_eq!(status.trial_phase.as_deref(), Some("Installed"));
    assert!(!status.trial_healthy);
}

#[test]
fn default_policy_applies_and_rechecks_live_policy_after_candidate_probe() {
    let mut automatic = Fixture::new();
    let provider = settings_provider(true);
    let source = provider().unwrap().source;
    finish_candidate(
        &automatic.context,
        &mut automatic.state,
        &automatic.proof,
        Pins {
            masters: &[&automatic.master],
        },
        &source,
        &provider,
        |_, _, _| Ok(()),
    )
    .unwrap();
    assert_eq!(automatic.state.installed.build, 11);
    assert!(automatic.state.staged.is_none());

    for change in ["manual", "source", "unavailable"] {
        let mut f = Fixture::new();
        let old = hash_file(&f.context.target).unwrap();
        let changed = std::sync::Arc::new(AtomicBool::new(false));
        let observed = std::sync::Arc::clone(&changed);
        let provider: crate::CheckSettingsProvider = std::sync::Arc::new(move || {
            let changed = observed.load(Ordering::SeqCst);
            if changed && change == "unavailable" {
                return None;
            }
            Some(crate::CheckSettings {
                source: Source {
                    owner: "alabsystems".into(),
                    repo: if changed && change == "source" {
                        "different"
                    } else {
                        "aterm"
                    }
                    .into(),
                },
                auto_apply: !(changed && change == "manual"),
            })
        });
        let mut probes = 0;
        finish_candidate(
            &f.context,
            &mut f.state,
            &f.proof,
            Pins {
                masters: &[&f.master],
            },
            &source,
            &provider,
            |_, _, _| {
                probes += 1;
                if probes == 2 {
                    changed.store(true, Ordering::SeqCst);
                }
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(probes, 2);
        assert_eq!(hash_file(&f.context.target).unwrap(), old, "{change}");
        assert!(
            f.state.trial.is_none(),
            "pre-rename intent is safely abandoned"
        );
        assert_eq!(f.state.staged.as_ref().unwrap().build, 11);
        assert_eq!(f.state.high_water, 10);
    }
}

#[test]
fn manual_staging_never_bypasses_authentication_or_the_apply_time_recheck() {
    let mut f = Fixture::new();
    let provider = settings_provider(false);
    let source = provider().unwrap().source;
    let mut bad = f.proof.clone();
    bad.signature[0] ^= 1;
    assert!(
        finish_candidate(
            &f.context,
            &mut f.state,
            &bad,
            Pins {
                masters: &[&f.master],
            },
            &source,
            &provider,
            |_, _, _| Ok(())
        )
        .is_err()
    );
    assert!(f.state.staged.is_none());
    finish_candidate(
        &f.context,
        &mut f.state,
        &f.proof,
        Pins {
            masters: &[&f.master],
        },
        &source,
        &provider,
        |_, _, _| Ok(()),
    )
    .unwrap();
    fs::write(f.context.dir.join("candidate"), b"tampered").unwrap();
    assert!(f.apply().is_err());
    assert_eq!(f.state.installed.build, 10);
    assert!(f.state.trial.is_none());
}

#[test]
fn pre_policy_enrollment_records_remain_readable_without_a_stage_field() {
    let f = Fixture::new();
    let text = aterm_toml::to_string(&f.state).unwrap();
    assert!(!text.contains("staged"));
    let decoded: State = aterm_toml::from_str(&text).unwrap();
    assert!(decoded.staged.is_none());
    assert_eq!(decoded.installed.sha256, f.state.installed.sha256);
}

#[test]
fn staged_status_and_reuse_obey_new_minimum_revocations_and_exact_current_bytes() {
    let mut f = Fixture::new();
    let provider = settings_provider(false);
    let source = provider().unwrap().source;
    finish_candidate(
        &f.context,
        &mut f.state,
        &f.proof,
        Pins {
            masters: &[&f.master],
        },
        &source,
        &provider,
        |_, _, _| Ok(()),
    )
    .unwrap();
    let manifest = Manifest::parse(std::str::from_utf8(&f.proof.appcast).unwrap()).unwrap();
    assert!(reusable_stage(
        &f.context,
        &f.state,
        &manifest,
        native().unwrap()
    ));
    f.state.min_build = 12;
    assert!(linux_status(&f.context, &f.state).staged_build.is_none());
    assert!(!reusable_stage(
        &f.context,
        &f.state,
        &manifest,
        native().unwrap()
    ));
    f.state.min_build = 0;
    f.state.staged.as_mut().unwrap().machine_id = Some("revoked".into());
    f.state.revoked_machines.push("revoked".into());
    assert!(linux_status(&f.context, &f.state).staged_build.is_none());
    assert!(!reusable_stage(
        &f.context,
        &f.state,
        &manifest,
        native().unwrap()
    ));
    f.state.revoked_machines.clear();
    fs::write(f.context.dir.join("candidate"), b"changed").unwrap();
    assert!(linux_status(&f.context, &f.state).staged_build.is_none());
    assert!(!reusable_stage(
        &f.context,
        &f.state,
        &manifest,
        native().unwrap()
    ));
}

#[test]
fn failed_stage_refresh_cannot_advertise_the_previous_candidate() {
    let mut f = Fixture::new();
    let provider = settings_provider(false);
    let source = provider().unwrap().source;
    let pins = Pins {
        masters: &[&f.master],
    };
    finish_candidate(
        &f.context,
        &mut f.state,
        &f.proof,
        pins,
        &source,
        &provider,
        |_, _, _| Ok(()),
    )
    .unwrap();
    assert_eq!(linux_status(&f.context, &f.state).staged_build, Some(11));
    let old = hash_file(&f.context.target).unwrap();
    // A refreshed proof/candidate must regain admission and compiled identity.
    // Even when the replacement probe fails, the old presentation is retired.
    assert!(
        finish_candidate(
            &f.context,
            &mut f.state,
            &f.proof,
            pins,
            &source,
            &provider,
            |_, _, _| Err("loader or compiled identity refused".into())
        )
        .is_err()
    );
    assert!(f.context.read_state().unwrap().unwrap().staged.is_none());
    assert!(linux_status(&f.context, &f.state).staged_build.is_none());
    assert_eq!(hash_file(&f.context.target).unwrap(), old);
    assert_eq!(f.state.high_water, 10);

    finish_candidate(
        &f.context,
        &mut f.state,
        &f.proof,
        pins,
        &source,
        &provider,
        |_, _, _| Ok(()),
    )
    .unwrap();
    fs::remove_file(f.context.dir.join("candidate")).unwrap();
    assert!(linux_status(&f.context, &f.state).staged_build.is_none());
}

fn elf(marker: u8, target: LinuxTarget) -> Vec<u8> {
    let mut bytes = vec![0; 128];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    bytes[16..18].copy_from_slice(&3u16.to_le_bytes());
    bytes[18..20].copy_from_slice(
        &(match target {
            LinuxTarget::Aarch64 => 183u16,
            LinuxTarget::X86_64 => 62,
        })
        .to_le_bytes(),
    );
    bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
    bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
    bytes[127] = marker;
    bytes
}

impl Fixture {
    fn new() -> Self {
        // Tests deliberately share the real ownership/ancestor checks: no /tmp
        // escape, and no injected production trust pin or network endpoint.
        use std::os::unix::fs::DirBuilderExt;
        let parent = PathBuf::from(std::env::var_os("HOME").expect("private fixture parent HOME"))
            .join(".aterm-linux-update-tests");
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&parent)
            .unwrap();
        let root = parent.join(aterm_uds::rand::hex_token::<16>().unwrap());
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let target = root.join("aterm");
        fs::write(&target, elf(1, native().unwrap())).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        let context = Context::at(target).unwrap();
        context.prepare_dir().unwrap();
        let installed = Identity {
            build: 10,
            sha256: hash_file(&context.target).unwrap(),
            version: "0.10.0".into(),
            commit: "a".repeat(40),
            machine_id: None,
        };
        let state = State {
            schema: 1,
            target: context.target.clone(),
            enabled: true,
            high_water: 10,
            min_build: 0,
            roster_floor: 0,
            revoked_machines: Vec::new(),
            min_build_sources: FloorSources::default(),
            installed,
            trial: None,
            staged: None,
            rejected_build: 0,
            outcome: String::new(),
            updated_at: String::new(),
            failing_checks: 0,
            failing_kind: String::new(),
            check_owner: String::new(),
            check_repo: String::new(),
            check_build: 0,
            last_attempt_unix: 0,
            next_check_unix: 0,
        };
        context.save(&state).unwrap();
        fs::write(context.dir.join("candidate"), elf(2, native().unwrap())).unwrap();
        fs::set_permissions(
            context.dir.join("candidate"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let digest = hash_file(&context.dir.join("candidate")).unwrap();
        let arch = native().unwrap().arch();
        let text = format!(
            "schema = 1\nmachine_id = \"fixture\"\nroster_seq = 1\nversion = \"0.11.0\"\nbuild_number = 11\ncommit = \"{}\"\ndmg = \"aterm-0.11.0.dmg\"\nsha256 = \"{}\"\nlinux_{arch} = \"aterm-0.11.0-linux-{arch}\"\nlinux_{arch}_sha256 = \"{digest}\"\nlinux_{arch}_size = 128\n",
            "b".repeat(40),
            "0".repeat(64)
        );
        let (roster, roster_signature) = roster_proof(1);
        let proof = Proof {
            policy: None,
            appcast: text.into_bytes(),
            signature: Vec::new(),
            roster,
            roster_signature,
        };
        let mut this = Self {
            root,
            context,
            state,
            proof,
            master: public(&master()),
        };
        this.resign();
        this
    }

    fn resign(&mut self) {
        self.proof.signature = machine().sign(&self.proof.appcast).as_ref().to_vec();
    }

    fn alter_manifest(&mut self, from: &str, to: &str) {
        self.proof.appcast = String::from_utf8(self.proof.appcast.clone())
            .unwrap()
            .replace(from, to)
            .into_bytes();
        self.resign();
    }

    fn apply(&mut self) -> Result<String, String> {
        apply_with_checkpoints(
            &self.context,
            &mut self.state,
            &self.proof,
            Pins {
                masters: &[&self.master],
            },
            |_, _, _| Ok(()),
            |_| Ok(()),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn authenticated_probe_is_bounded_and_rejects_loader_identity_and_exit_failures() {
    let f = Fixture::new();
    let manifest = Manifest::parse(std::str::from_utf8(&f.proof.appcast).unwrap()).unwrap();
    let script = f.root.join("identity-probe");
    let good = aterm_update_core::linux::BinaryIdentity {
        schema: 1,
        version: manifest.version.clone(),
        build_number: manifest.build_number,
        commit: manifest.commit.clone().unwrap(),
        target: native().unwrap().triple().into(),
        dirty: false,
    };
    let write_script = |body: &str| {
        fs::write(&script, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    };
    write_script(&format!(
        "test \"$1 $2\" = 'update identity' || exit 9\nprintf '%s\\n' '{}'",
        aterm_json::to_string(&good).unwrap()
    ));
    probe_candidate(
        &script,
        &manifest,
        native().unwrap(),
        &f.context.dir,
        Duration::from_secs(1),
    )
    .unwrap();
    for body in ["exit 3", "printf 'not json'", "while :; do :; done"] {
        write_script(body);
        assert!(
            probe_candidate(
                &script,
                &manifest,
                native().unwrap(),
                &f.context.dir,
                Duration::from_millis(50)
            )
            .is_err()
        );
    }
    let mut bad = good;
    bad.build_number += 1;
    write_script(&format!(
        "printf '%s\\n' '{}'",
        aterm_json::to_string(&bad).unwrap()
    ));
    assert!(
        probe_candidate(
            &script,
            &manifest,
            native().unwrap(),
            &f.context.dir,
            Duration::from_secs(1)
        )
        .is_err()
    );
    assert!(
        probe_candidate(
            &f.context.dir.join("candidate"),
            &manifest,
            native().unwrap(),
            &f.context.dir,
            Duration::from_secs(1)
        )
        .is_err(),
        "synthetic signed-header ELF cannot satisfy the real loader probe"
    );
    assert!(fs::read_dir(&f.context.dir).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("probe-")
    }));
}

#[test]
fn bootstrap_preserves_enrolled_bytes_and_state_on_probe_failure_then_uses_normal_trial() {
    let f = Fixture::new();
    let source = f.root.join("download");
    fs::copy(f.context.dir.join("candidate"), &source).unwrap();
    fs::set_permissions(&source, fs::Permissions::from_mode(0o755)).unwrap();
    let pins = Pins {
        masters: &[&f.master],
    };
    assert!(
        install_locked(&f.context, &source, &f.proof, pins, |_, _, _| Err(
            "loader refused".into()
        ))
        .is_err()
    );
    assert_eq!(
        hash_file(&f.context.target).unwrap(),
        f.state.installed.sha256
    );
    let state = f.context.read_state().unwrap().unwrap();
    assert_eq!(state.installed.build, 10);
    assert!(state.trial.is_none());
    install_locked(&f.context, &source, &f.proof, pins, |_, _, _| Ok(())).unwrap();
    let mut state = f.context.read_state().unwrap().unwrap();
    assert!(state.enabled);
    assert_eq!(state.installed.build, 11);
    assert_eq!(
        state.trial.as_ref().unwrap().old.sha256,
        f.state.installed.sha256
    );
    assert!(
        install_locked(&f.context, &source, &f.proof, pins, |_, _, _| Ok(())).is_err(),
        "bootstrap cannot bypass an unhealthy trial"
    );
    rollback_locked(&f.context, &mut state).unwrap();
    assert_eq!(
        hash_file(&f.context.target).unwrap(),
        f.state.installed.sha256
    );
}

#[test]
fn bootstrap_first_install_and_latest_policy_handoff_preserve_floors() {
    let f = Fixture::new();
    let source = f.root.join("download");
    fs::copy(f.context.dir.join("candidate"), &source).unwrap();
    fs::set_permissions(&source, fs::Permissions::from_mode(0o755)).unwrap();
    fs::remove_file(&f.context.target).unwrap();
    fs::remove_file(f.context.dir.join("state.toml")).unwrap();
    let pins = Pins {
        masters: &[&f.master],
    };
    let mut proof = f.proof.clone();
    let policy = format!(
        "{}\nmin_build = 12\n",
        String::from_utf8(proof.appcast.clone())
            .unwrap()
            .replace("build_number = 11", "build_number = 12")
            .replace("0.11.0", "0.12.0")
    )
    .into_bytes();
    proof.policy = Some((policy.clone(), key(7).sign(&policy).as_ref().to_vec()));
    assert!(install_locked(&f.context, &source, &proof, pins, |_, _, _| Ok(())).is_err());
    assert!(!f.context.target.exists());
    assert_eq!(f.context.read_state().unwrap().unwrap().min_build, 12);
    assert!(
        install_locked(&f.context, &source, &f.proof, pins, |_, _, _| Ok(())).is_err(),
        "omitting the admitted policy cannot lower its durable floor"
    );

    let fresh = Fixture::new();
    let download = fresh.root.join("download");
    fs::copy(fresh.context.dir.join("candidate"), &download).unwrap();
    fs::set_permissions(&download, fs::Permissions::from_mode(0o755)).unwrap();
    fs::remove_file(&fresh.context.target).unwrap();
    fs::remove_file(fresh.context.dir.join("state.toml")).unwrap();
    install_locked(&fresh.context, &download, &fresh.proof, pins, |_, _, _| {
        Ok(())
    })
    .unwrap();
    let state = fresh.context.read_state().unwrap().unwrap();
    assert!(state.enabled);
    assert_eq!(state.installed.build, 11);
    assert!(state.trial.is_none());
    assert_eq!(
        hash_file(&fresh.context.target).unwrap(),
        state.installed.sha256
    );
    install_locked(&fresh.context, &download, &fresh.proof, pins, |_, _, _| {
        Ok(())
    })
    .unwrap();
}

#[test]
fn bootstrap_and_self_update_share_the_same_process_lock() {
    let f = Fixture::new();
    let _lock = f.context.lock().unwrap();
    assert!(
        Context::destination(f.context.target.clone())
            .unwrap()
            .lock()
            .is_err()
    );
    assert_eq!(
        hash_file(&f.context.target).unwrap(),
        f.state.installed.sha256
    );
}

#[test]
fn interrupted_prepared_transaction_can_be_retried_by_the_bootstrap_entry() {
    let mut f = Fixture::new();
    let download = f.root.join("download");
    fs::copy(f.context.dir.join("candidate"), &download).unwrap();
    fs::set_permissions(&download, fs::Permissions::from_mode(0o755)).unwrap();
    let pins = Pins {
        masters: &[&f.master],
    };
    assert!(
        apply_with_checkpoints(
            &f.context,
            &mut f.state,
            &f.proof,
            pins,
            |_, _, _| Ok(()),
            |at| if at == Checkpoint::Prepared {
                Err("interrupted".into())
            } else {
                Ok(())
            }
        )
        .is_err()
    );
    assert_eq!(
        f.context
            .read_state()
            .unwrap()
            .unwrap()
            .trial
            .unwrap()
            .phase,
        Phase::Prepared
    );
    install_locked(&f.context, &download, &f.proof, pins, |_, _, _| Ok(())).unwrap();
    let state = f.context.read_state().unwrap().unwrap();
    assert_eq!(state.installed.build, 11);
    assert!(state.enabled);
}

#[test]
fn old_and_new_live_builds_share_one_source_cooldown_without_minute_polling() {
    let mut f = Fixture::new();
    let source = Source {
        owner: "alabsystems".into(),
        repo: "aterm".into(),
    };
    f.state.check_owner = source.owner.clone();
    f.state.check_repo = source.repo.clone();
    f.state.next_check_unix = 2800;
    for build in [10, 11, 10, 11] {
        f.state.check_build = build;
        assert!(!automatic_check_due(&f.state, &source, 1060));
        assert!(!automatic_check_due(&f.state, &source, 2799));
    }
    assert!(automatic_check_due(&f.state, &source, 2800));
    assert!(automatic_check_due(
        &f.state,
        &Source {
            repo: "another-channel".into(),
            ..source
        },
        1060
    ));
}

#[test]
fn bootstrap_executes_real_authenticated_native_inode_after_closing_all_writers() {
    let mut f = Fixture::new();
    let source = f.root.join("native-loader-fixture");
    // Real native ELF exercises the kernel loader (including ETXTBSY), while
    // the private probe seam runs a side-effect-free shell builtin. Identity
    // wire admission has its separate positive/negative subprocess test above.
    fs::copy("/bin/sh", &source).unwrap();
    fs::set_permissions(&source, fs::Permissions::from_mode(0o755)).unwrap();
    let old_digest = hash_file(&f.context.dir.join("candidate")).unwrap();
    f.alter_manifest(&old_digest, &hash_file(&source).unwrap());
    f.alter_manifest(
        "_size = 128",
        &format!("_size = {}", fs::metadata(&source).unwrap().len()),
    );
    let pins = Pins {
        masters: &[&f.master],
    };
    install_locked(&f.context, &source, &f.proof, pins, |path, _, _| {
        let status = std::process::Command::new(path)
            .args(["-c", "exit 0"])
            .env_clear()
            .status()
            .map_err(|error| error.to_string())?;
        if status.success() {
            Ok(())
        } else {
            Err(status.to_string())
        }
    })
    .unwrap();
    assert_eq!(
        hash_file(&f.context.target).unwrap(),
        hash_file(&source).unwrap()
    );
    let mut installed = f.context.read_state().unwrap().unwrap();
    rollback_locked(&f.context, &mut installed).unwrap();
    assert_eq!(
        hash_file(&f.context.target).unwrap(),
        f.state.installed.sha256
    );
}

#[test]
fn rollback_inode_is_executable_immediately_after_rename_before_receipt_commit() {
    let mut f = Fixture::new();
    fs::copy("/bin/sh", &f.context.target).unwrap();
    fs::set_permissions(&f.context.target, fs::Permissions::from_mode(0o755)).unwrap();
    f.state.installed.sha256 = hash_file(&f.context.target).unwrap();
    f.context.save(&f.state).unwrap();
    f.apply().unwrap();
    let mut ran = false;
    rollback_with_checkpoint(&f.context, &mut f.state, |path| {
        assert_eq!(
            f.context.read_state()?.unwrap().trial.unwrap().phase,
            Phase::RollbackPrepared
        );
        let status = std::process::Command::new(path)
            .args(["-c", "exit 0"])
            .env_clear()
            .status()
            .map_err(|e| e.to_string())?;
        assert!(status.success());
        ran = true;
        Ok(())
    })
    .unwrap();
    assert!(ran);
    assert_eq!(f.state.installed.build, 10);
}

#[test]
fn verified_native_update_replaces_disk_without_changing_open_old_inode_and_rolls_back() {
    let mut f = Fixture::new();
    let mut old_inode = File::open(&f.context.target).unwrap();
    let old_hash = f.state.installed.sha256.clone();
    assert!(f.apply().unwrap().contains("existing sessions continue"));
    assert_eq!(f.state.installed.build, 11);
    assert_ne!(hash_file(&f.context.target).unwrap(), old_hash);
    let mut bytes = Vec::new();
    old_inode.read_to_end(&mut bytes).unwrap();
    assert_eq!(
        bytes,
        elf(1, native().unwrap()),
        "running/open old inode is never rewritten"
    );
    assert_eq!(
        hash_file(&f.context.backup(&f.state.trial.as_ref().unwrap().old)).unwrap(),
        old_hash
    );
    assert!(!linux_status(&f.context, &f.state).refused_newer);
    rollback_locked(&f.context, &mut f.state).unwrap();
    assert_eq!(hash_file(&f.context.target).unwrap(), old_hash);
    assert_eq!(
        f.state.high_water, 11,
        "repair is not permission to replay the failed build"
    );
    assert_eq!(f.state.rejected_build, 11);
    assert!(
        linux_status(&f.context, &f.state).refused_newer,
        "`aterm update status` must not call the rolled-back copy up to date"
    );
}

#[test]
fn tamper_wrong_key_wrong_target_bad_size_and_replay_cannot_replace_installed_bytes() {
    for case in [
        "signature",
        "key",
        "bytes",
        "arch",
        "size",
        "replay",
        "missing-target",
    ] {
        let mut f = Fixture::new();
        let old = hash_file(&f.context.target).unwrap();
        match case {
            "signature" => f.proof.appcast[0] ^= 1,
            // Signed by a machine the master-signed roster does not admit.
            "key" => f.proof.signature = key(9).sign(&f.proof.appcast).as_ref().to_vec(),
            "bytes" => {
                fs::write(f.context.dir.join("candidate"), elf(3, native().unwrap())).unwrap()
            }
            "arch" => {
                let other = match native().unwrap() {
                    LinuxTarget::Aarch64 => LinuxTarget::X86_64,
                    LinuxTarget::X86_64 => LinuxTarget::Aarch64,
                };
                fs::write(f.context.dir.join("candidate"), elf(2, other)).unwrap();
                let old_digest = Manifest::parse(std::str::from_utf8(&f.proof.appcast).unwrap())
                    .unwrap()
                    .linux_artifact(native().unwrap())
                    .unwrap()
                    .unwrap()
                    .sha256
                    .to_owned();
                let digest = hash_file(&f.context.dir.join("candidate")).unwrap();
                f.alter_manifest(&old_digest, &digest);
            }
            "size" => f.alter_manifest("_size = 128", "_size = 129"),
            "replay" => f.alter_manifest("build_number = 11", "build_number = 10"),
            "missing-target" => {
                let text = std::str::from_utf8(&f.proof.appcast).unwrap();
                f.proof.appcast = text
                    .lines()
                    .filter(|line| !line.starts_with("linux_"))
                    .collect::<Vec<_>>()
                    .join("\n")
                    .into_bytes();
                f.resign();
            }
            _ => unreachable!(),
        }
        assert!(f.apply().is_err(), "{case}");
        assert_eq!(
            hash_file(&f.context.target).unwrap(),
            old,
            "{case} changed installed executable"
        );
        assert!(f.state.trial.is_none(), "{case} armed a trial");
    }
}

#[test]
fn state_symlinks_shared_directories_and_tampered_backups_are_refused() {
    let mut f = Fixture::new();
    fs::set_permissions(&f.root, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(
        Context::at(f.context.target.clone()).is_ok(),
        "private ancestor shields this directory"
    );
    fs::set_permissions(&f.root, fs::Permissions::from_mode(0o700)).unwrap();
    let state_file = f.context.dir.join("state.toml");
    fs::rename(&state_file, f.context.dir.join("saved-state")).unwrap();
    std::os::unix::fs::symlink("saved-state", &state_file).unwrap();
    assert!(f.context.read_state().is_err());
    fs::remove_file(&state_file).unwrap();
    fs::rename(f.context.dir.join("saved-state"), &state_file).unwrap();
    f.apply().unwrap();
    let current = hash_file(&f.context.target).unwrap();
    let backup = f.context.backup(&f.state.trial.as_ref().unwrap().old);
    fs::write(&backup, elf(9, native().unwrap())).unwrap();
    assert!(rollback_locked(&f.context, &mut f.state).is_err());
    assert_eq!(hash_file(&f.context.target).unwrap(), current);
}

#[test]
fn rollback_respects_minimum_and_revocation_without_lowering_any_floor() {
    for revoke in [false, true] {
        let mut f = Fixture::new();
        f.state.installed.machine_id = Some("old-machine".into());
        f.apply().unwrap();
        if revoke {
            f.state.revoked_machines.push("old-machine".into());
        } else {
            f.state.min_build = 11;
        }
        let current = hash_file(&f.context.target).unwrap();
        assert!(rollback_locked(&f.context, &mut f.state).is_err());
        assert_eq!(hash_file(&f.context.target).unwrap(), current);
        assert_eq!(f.state.high_water, 11);
    }
}

#[test]
fn interrupted_apply_recovers_both_sides_of_the_atomic_rename() {
    for stop in [
        Checkpoint::Backup,
        Checkpoint::Prepared,
        Checkpoint::Replaced,
        Checkpoint::Committed,
    ] {
        let mut f = Fixture::new();
        let old = f.state.installed.sha256.clone();
        let result = apply_with_checkpoints(
            &f.context,
            &mut f.state,
            &f.proof,
            Pins {
                masters: &[&f.master],
            },
            |_, _, _| Ok(()),
            |at| {
                if at == stop {
                    Err("simulated interruption".into())
                } else {
                    Ok(())
                }
            },
        );
        assert!(result.is_err());
        let mut durable = f.context.read_state().unwrap().unwrap();
        recover(&f.context, &mut durable).unwrap();
        if matches!(stop, Checkpoint::Backup | Checkpoint::Prepared) {
            assert_eq!(hash_file(&f.context.target).unwrap(), old);
            assert_eq!(durable.installed.build, 10);
        } else {
            assert_eq!(durable.installed.build, 11);
            assert_eq!(durable.high_water, 11);
            rollback_locked(&f.context, &mut durable).unwrap();
            assert_eq!(hash_file(&f.context.target).unwrap(), old);
        }
    }
}

/// K1 IS RETIRED: the paper master is the one anchor. A build that pins none has
/// nothing to verify a roster — and so an appcast — against, and refuses every
/// update channel rather than falling back to a bare channel key.
#[test]
fn a_build_that_pins_no_paper_master_verifies_no_update_channel() {
    let mut f = Fixture::new();
    let error = authorize(&f.context, &mut f.state, &f.proof, Pins { masters: &[] })
        .expect_err("no anchor, no channel");
    assert!(error.contains("pins no paper master"), "{error}");
    // Negative control: the very same proof verifies under the fixture's master.
    assert!(
        authorize(
            &f.context,
            &mut f.state,
            &f.proof,
            Pins {
                masters: &[&f.master]
            }
        )
        .is_ok()
    );
}

#[test]
fn admitted_roster_revocation_is_durable_even_when_it_refuses_the_appcast() {
    let mut f = Fixture::new();
    f.alter_manifest("roster_seq = 1", "roster_seq = 2");
    let master = master();
    let anchor = public(&master);
    let mut roster = aterm_update_core::Roster {
        schema: 1,
        roster_seq: 2,
        valid_until: "2099-01-01T00:00:00Z".into(),
        machines: vec![aterm_update_core::Machine {
            id: "fixture".into(),
            pubkey: public(&machine()),
            added_at: String::new(),
            not_after: None,
        }],
        revoked: Vec::new(),
    };
    f.proof.roster = roster.to_toml().unwrap().into_bytes();
    f.proof.roster_signature = master.sign(&f.proof.roster).as_ref().to_vec();
    let pins = Pins {
        masters: &[&anchor],
    };
    assert!(authorize(&f.context, &mut f.state, &f.proof, pins).is_ok());
    let old_proof = f.proof.clone();
    roster.roster_seq = 3;
    roster.revoked.push("fixture".into());
    f.proof.roster = roster.to_toml().unwrap().into_bytes();
    f.proof.roster_signature = master.sign(&f.proof.roster).as_ref().to_vec();
    assert!(authorize(&f.context, &mut f.state, &f.proof, pins).is_err());
    let durable = f.context.read_state().unwrap().unwrap();
    assert_eq!(durable.roster_floor, 3);
    assert!(durable.revoked_machines.contains(&"fixture".into()));
    assert!(authorize(&f.context, &mut f.state, &old_proof, pins).is_err());
    // Even a same-sequence equivocated master document cannot resurrect a name
    // this client has durably seen revoked.
    roster.revoked.clear();
    f.proof.roster = roster.to_toml().unwrap().into_bytes();
    f.proof.roster_signature = master.sign(&f.proof.roster).as_ref().to_vec();
    assert!(authorize(&f.context, &mut f.state, &f.proof, pins).is_err());
}

/// One start a minute: far enough apart that no start is still young when the next
/// one is counted ([`crate::linux_trial::START_SETTLES_SECS`]), so every verdict is the
/// plain budget's.
fn minute(n: i64) -> i64 {
    1_700_000_000 + n * 60
}

#[test]
fn actual_boot_and_health_transitions_bind_build_commit_and_process_inode() {
    let mut f = Fixture::new();
    let old = File::open(&f.context.target).unwrap().metadata().unwrap();
    f.apply().unwrap();
    assert!(
        !boot_locked(
            &f.context,
            &mut f.state,
            10,
            &"a".repeat(40),
            (old.dev(), old.ino()),
            minute(0),
        )
        .unwrap()
    );
    assert_eq!(
        f.state.trial.as_ref().unwrap().starts,
        0,
        "old live process has no new-build trial"
    );
    let actual = File::open(&f.context.target).unwrap().metadata().unwrap();
    assert!(
        boot_locked(
            &f.context,
            &mut f.state,
            11,
            &"b".repeat(40),
            (actual.dev(), actual.ino()),
            minute(1),
        )
        .unwrap()
    );
    assert_eq!(f.state.trial.as_ref().unwrap().starts, 1);
    assert!(
        !confirm_locked(
            &f.context,
            &mut f.state,
            11,
            &"b".repeat(40),
            (old.dev(), old.ino())
        )
        .unwrap()
    );
    let actual = File::open(&f.context.target).unwrap().metadata().unwrap();
    assert!(
        confirm_locked(
            &f.context,
            &mut f.state,
            11,
            &"b".repeat(40),
            (actual.dev(), actual.ino())
        )
        .unwrap()
    );
    for n in 0..5 {
        boot_locked(
            &f.context,
            &mut f.state,
            11,
            &"b".repeat(40),
            (actual.dev(), actual.ino()),
            minute(2 + n),
        )
        .unwrap();
    }
    assert!(f.state.trial.as_ref().unwrap().healthy);
    assert_eq!(f.state.installed.build, 11);

    let mut failing = Fixture::new();
    let baseline = failing.state.installed.sha256.clone();
    failing.apply().unwrap();
    let failed_inode = fs::metadata(&failing.context.target).unwrap();
    for n in 0..MAX_TRIAL_STARTS {
        boot_locked(
            &failing.context,
            &mut failing.state,
            11,
            &"b".repeat(40),
            (failed_inode.dev(), failed_inode.ino()),
            minute(i64::from(n)),
        )
        .unwrap();
    }
    assert_eq!(failing.state.installed.build, 11);
    boot_locked(
        &failing.context,
        &mut failing.state,
        999,
        &"c".repeat(40),
        (failed_inode.dev(), failed_inode.ino()),
        minute(i64::from(MAX_TRIAL_STARTS)),
    )
    .unwrap();
    assert_eq!(failing.state.installed.build, 10);
    assert_eq!(hash_file(&failing.context.target).unwrap(), baseline);
    assert_eq!(failing.state.high_water, 11);
    assert_eq!(failing.state.rejected_build, 11);
    assert!(
        failing
            .state
            .outcome
            .contains("didn\u{2019}t start properly")
            && !failing.state.outcome.contains("window"),
        "a copy used only from the terminal opens no window: {}",
        failing.state.outcome
    );
}

/// The trial as it stands on disk — what the next process reads.
fn trial_on_disk(f: &Fixture) -> Trial {
    f.context
        .read_state()
        .unwrap()
        .unwrap()
        .trial
        .expect("a trial")
}

/// A session of the file at the install path: the new build's identity and inode.
fn session_start(f: &Fixture, now_unix: i64) -> bool {
    let inode = fs::metadata(&f.context.target).unwrap();
    session_started_in(
        &f.context,
        11,
        &"b".repeat(40),
        || Ok((inode.dev(), inode.ino())),
        now_unix,
    )
    .unwrap()
}

/// That session proving healthy.
fn session_confirm(f: &Fixture) -> Confirmed {
    let inode = fs::metadata(&f.context.target).unwrap();
    confirm_session_in(&f.context, 11, &"b".repeat(40), || {
        Ok((inode.dev(), inode.ino()))
    })
    .unwrap()
}

/// A window of the file at the install path starting, and its first frame.
fn window_start(f: &Fixture, now_unix: i64) -> bool {
    window_start_as(f, 11, 'b', now_unix)
}

/// A window of release `build` (whose commit is `commit` forty times) starting.
fn window_start_as(f: &Fixture, build: u64, commit: char, now_unix: i64) -> bool {
    let inode = fs::metadata(&f.context.target).unwrap();
    let _lock = f.context.lock().unwrap();
    let mut state = f.context.read_state().unwrap().unwrap();
    boot_locked(
        &f.context,
        &mut state,
        build,
        &commit.to_string().repeat(40),
        (inode.dev(), inode.ino()),
        now_unix,
    )
    .unwrap()
}

fn window_confirm(f: &Fixture) -> bool {
    window_confirm_as(f, 11, 'b')
}

fn window_confirm_as(f: &Fixture, build: u64, commit: char) -> bool {
    let inode = fs::metadata(&f.context.target).unwrap();
    let _lock = f.context.lock().unwrap();
    let mut state = f.context.read_state().unwrap().unwrap();
    confirm_locked(
        &f.context,
        &mut state,
        build,
        &commit.to_string().repeat(40),
        (inode.dev(), inode.ino()),
    )
    .unwrap()
}

/// A COPY USED ONLY FROM THE TERMINAL FINISHES ITS UPDATE (2026-09-28). Its session
/// lane's check installed the update, and only a window counted or confirmed a start,
/// so nothing ever finished it: `check_locked` stopped at "launch an aterm window once"
/// on every later check, for good. Driven through the session lane's own entry
/// ([`session_started_in`], the body of [`session_started`]: the unlocked steady-state
/// read, the lock, the count) and its own confirmation ([`confirm_session_in`]), the
/// install is counted, confirmed at the first prompt, and the copy updates again. The
/// words it waits with ask for no window. A session after that counts nothing.
#[test]
fn a_session_lane_install_is_counted_and_confirmed_by_a_session() {
    let mut f = Fixture::new();
    assert!(
        f.apply()
            .unwrap()
            .contains("the next aterm you start finishes it")
    );
    let pending = awaiting_start(&f.state).expect("the install waits for a start");
    assert!(!pending.contains("window"), "{pending}");
    assert!(
        session_start(&f, minute(0)),
        "a session of the new file is a start of its trial"
    );
    assert_eq!(trial_on_disk(&f).starts, 1);
    assert_eq!(trial_on_disk(&f).last_start_unix, minute(0));
    assert_eq!(session_confirm(&f), Confirmed::Yes);
    let state = f.context.read_state().unwrap().unwrap();
    let trial = state.trial.as_ref().unwrap();
    assert!(trial.healthy);
    assert!(
        !trial.window_proven,
        "a session's prompt does not vouch for the window lane"
    );
    assert!(
        awaiting_start(&state).is_none(),
        "confirmed: checks and applies run again"
    );
    assert_eq!(state.installed.build, 11);
    assert!(
        !session_start(&f, minute(1)),
        "a confirmed trial counts no more sessions"
    );
}

/// A session still running the OLD file — a tab opened before the install — is no
/// start of the new build's trial: it counts nothing and confirms nothing, so only
/// the new file can vouch for itself.
#[test]
fn a_session_on_the_old_file_does_not_confirm() {
    let mut f = Fixture::new();
    let old = File::open(&f.context.target).unwrap().metadata().unwrap();
    f.apply().unwrap();
    assert!(
        !session_started_in(
            &f.context,
            10,
            &"a".repeat(40),
            || Ok((old.dev(), old.ino())),
            minute(0),
        )
        .unwrap()
    );
    // Even one that claims the new build's identity is refused on its inode.
    assert_eq!(
        confirm_session_in(&f.context, 11, &"b".repeat(40), || Ok((
            old.dev(),
            old.ino()
        )))
        .unwrap(),
        Confirmed::OtherFile
    );
    let state = f.context.read_state().unwrap().unwrap();
    let trial = state.trial.as_ref().unwrap();
    assert_eq!(trial.starts, 0);
    assert!(!trial.healthy);
    assert!(awaiting_start(&state).is_some());
}

/// A SESSION DOES NOT VOUCH FOR A BUILD WHOSE WINDOW HAS NOT CONFIRMED (the round-four
/// review). Two windows of the new build crash before their first frame; the person
/// opens a terminal and types `aterm` to find out why — the natural move, and the one
/// that used to confirm the trial for the whole file: its window starts then stopped
/// counting, and a build that cannot open a window stayed installed for good. Now the
/// session's prompt confirms the session lane only: its window starts go on counting,
/// from zero, and the fourth that never confirms rolls the install back.
///
/// FAILS WITHOUT THE FIX: the session's confirmation settled the whole file, and every
/// window start after it returned early; nothing rolled back.
#[test]
fn a_window_that_has_not_confirmed_still_decides_after_a_session_confirms() {
    let mut f = Fixture::new();
    f.apply().unwrap();
    assert!(window_start(&f, minute(0)));
    assert!(window_start(&f, minute(1)));
    assert!(session_start(&f, minute(2)), "the session is counted");
    assert_eq!(session_confirm(&f), Confirmed::Yes, "and is never refused");
    let trial = trial_on_disk(&f);
    assert!(trial.healthy && !trial.window_proven);
    assert_eq!(trial.starts, 0, "the window lane counts from clean");
    for n in 3..3 + i64::from(MAX_TRIAL_STARTS) {
        assert!(window_start(&f, minute(n)), "window start {n} is a try");
    }
    assert!(
        !window_start(&f, minute(3 + i64::from(MAX_TRIAL_STARTS))),
        "the window lane's own budget rolls back"
    );
    let state = f.context.read_state().unwrap().unwrap();
    assert_eq!(state.installed.build, 10);
    assert_eq!(state.rejected_build, 11);
    // A window that DOES confirm settles every lane.
    let mut g = Fixture::new();
    g.apply().unwrap();
    assert!(window_start(&g, minute(0)));
    assert!(window_confirm(&g));
    let trial = trial_on_disk(&g);
    assert!(trial.healthy && trial.window_proven);
    assert!(
        !window_start(&g, minute(1)),
        "a confirmed window counts no more"
    );
}

/// A WINDOW STILL OWES ITS VERDICT AFTER A SESSION CONFIRMED (the round-four review).
/// A session confirms first — checks and applies run again — and a window of the same
/// build crashes at every start later: its starts still count, from a clean count, and
/// the start past the budget rolls the build back. The clean count matters as much: six
/// tabs spent the shared budget before the first prompt, and the first window after
/// them is a first try, never a verdict.
///
/// FAILS WITHOUT THE FIX: once a session had confirmed, window starts counted nothing.
#[test]
fn a_window_start_after_a_session_confirmed_still_owes_its_verdict() {
    let mut f = Fixture::new();
    f.apply().unwrap();
    for _ in 0..6 {
        assert!(session_start(&f, minute(0)), "a burst of six tabs");
    }
    assert_eq!(session_confirm(&f), Confirmed::Yes);
    let trial = trial_on_disk(&f);
    assert!(trial.healthy && !trial.window_proven);
    assert_eq!(trial.starts, 0, "the window lane counts from clean");
    for n in 1..=MAX_TRIAL_STARTS {
        assert!(
            window_start(&f, minute(i64::from(n))),
            "window start {n} is a try, not a verdict"
        );
    }
    assert_eq!(f.context.read_state().unwrap().unwrap().installed.build, 11);
    assert!(!window_start(&f, minute(i64::from(MAX_TRIAL_STARTS) + 1)));
    let state = f.context.read_state().unwrap().unwrap();
    assert_eq!(
        state.installed.build, 10,
        "the window lane's verdict rolled it back"
    );
    assert_eq!(state.rejected_build, 11);
}

/// SIX TABS RESTORED AT ONCE, each running `aterm`, are six starts counted before the
/// first prompt confirms. By the budget alone the fourth rolled a healthy build back,
/// for good. A start over the budget now waits while the budget's last start can still
/// confirm; a build that never confirms is rolled back at the first start after a
/// quiet gap.
#[test]
fn a_burst_of_session_starts_never_rolls_back_the_build_its_first_prompt_confirms() {
    let mut f = Fixture::new();
    f.apply().unwrap();
    for tab in 0..6 {
        assert!(
            session_start(&f, minute(0)),
            "tab {tab} of the burst is a start of the trial"
        );
    }
    assert_eq!(
        f.context.read_state().unwrap().unwrap().installed.build,
        11,
        "no start of the burst rolled back"
    );
    assert_eq!(trial_on_disk(&f).starts, 6);
    assert_eq!(session_confirm(&f), Confirmed::Yes);
    assert!(trial_on_disk(&f).healthy);

    // The same burst from a build that crashes before its prompt: the first start
    // after the burst has settled delivers the verdict.
    let mut crashing = Fixture::new();
    crashing.apply().unwrap();
    for _ in 0..6 {
        session_start(&crashing, minute(0));
    }
    assert_eq!(
        crashing
            .context
            .read_state()
            .unwrap()
            .unwrap()
            .installed
            .build,
        11
    );
    assert!(!session_start(&crashing, minute(1)));
    let state = crashing.context.read_state().unwrap().unwrap();
    assert_eq!(state.installed.build, 10);
    assert_eq!(state.rejected_build, 11);
}

/// A CRASH LOOP FASTER THAN THE SETTLE WINDOW IS STILL ROLLED BACK (the round-four
/// review): `aterm --headless` under a systemd unit with `Restart=always` and
/// `RestartSec=5`, on a build that crashes at every start. Every start used to restamp,
/// so each found the one five seconds before it young and none rolled back — and while
/// the trial stood unconfirmed, checks and applies waited, so no fixed release could
/// install either. Now the starts past the budget share the budget's last stamp, and the
/// first one a whole window after it rolls back.
///
/// FAILS WITHOUT THE FIX: all sixty starts below are kept.
#[test]
fn a_crash_loop_restarting_every_five_seconds_is_rolled_back() {
    let mut f = Fixture::new();
    f.apply().unwrap();
    let t0 = minute(0);
    let mut rolled_at = None;
    for start in 1..=60_i64 {
        if !window_start(&f, t0 + start * 5) {
            rolled_at = Some(start);
            break;
        }
    }
    let rolled_at = rolled_at.expect("the crash loop is rolled back");
    let bound = i64::from(MAX_TRIAL_STARTS) + crate::linux_trial::START_SETTLES_SECS / 5;
    assert!(
        rolled_at <= bound,
        "rolled back at start {rolled_at}, bound {bound}"
    );
    let state = f.context.read_state().unwrap().unwrap();
    assert_eq!(state.installed.build, 10);
    assert_eq!(state.rejected_build, 11);
}

/// A state record an older build wrote has no `last_start_unix` or `window_proven`: it
/// reads as "no start stamped", which is the plain budget, and a window lane that has not
/// proved itself, which is the careful reading.
#[test]
fn a_trial_record_without_a_start_stamp_still_reads() {
    let mut f = Fixture::new();
    f.apply().unwrap();
    let path = f.context.dir.join("state.toml");
    let text = String::from_utf8(read_small(&path).unwrap()).unwrap();
    let older: String = text
        .lines()
        .filter(|line| !line.starts_with("last_start_unix") && !line.starts_with("window_proven"))
        .map(|line| format!("{line}\n"))
        .collect();
    assert_ne!(older, text, "the record carried the stamp");
    write_atomic(&path, older.as_bytes()).unwrap();
    let state = f.context.read_state().unwrap().unwrap();
    let trial = state.trial.as_ref().unwrap();
    assert_eq!(trial.last_start_unix, 0);
    assert!(!trial.window_proven, "the window lane still proves itself");
}

#[test]
fn source_only_pointer_can_reach_discovery_but_a_non_app_tag_cannot_authorize_an_appcast() {
    let mut f = Fixture::new();
    let tag = discovered_head(Err(aterm_update_core::pointer::PointerError::OtherTag {
        tag: "atpkg-index-7".into(),
    }))
    .unwrap();
    assert_eq!(tag, "atpkg-index-7");
    assert!(
        authenticate_tag(
            &f.context,
            &mut f.state,
            &tag,
            &f.proof,
            Pins {
                masters: &[&f.master],
            }
        )
        .is_err()
    );
    assert!(
        discovered_head(Err(aterm_update_core::pointer::PointerError::Refused {
            why: "foreign location"
        }))
        .is_err()
    );
}

#[test]
fn appcasts_have_the_full_five_megabyte_bound_not_the_small_state_record_bound() {
    let f = Fixture::new();
    let path = f.context.dir.join(APPCAST);
    let bytes = vec![b'x'; (RECORD_LIMIT + 1) as usize];
    write_atomic(&path, &bytes).unwrap();
    assert_eq!(
        read_bounded(&path, APPCAST_LIMIT).unwrap().len(),
        bytes.len()
    );
    assert!(read_small(&path).is_err());
}

/// Stage signed release `build` — an executable ending in `marker`, built from
/// `commit` forty times — as the fixture's candidate and proof, and take the record
/// as it stands on disk into the fixture's state.
fn stage_release(f: &mut Fixture, build: u64, marker: u8, commit: char) {
    let candidate = f.context.dir.join("candidate");
    fs::write(&candidate, elf(marker, native().unwrap())).unwrap();
    fs::set_permissions(&candidate, fs::Permissions::from_mode(0o644)).unwrap();
    let digest = hash_file(&candidate).unwrap();
    let arch = native().unwrap().arch();
    let version = format!("0.{build}.0");
    f.proof.appcast = format!(
        "schema = 1\nmachine_id = \"fixture\"\nroster_seq = 1\nversion = \"{version}\"\nbuild_number = {build}\ncommit = \"{}\"\ndmg = \"aterm-{version}.dmg\"\nsha256 = \"{}\"\nlinux_{arch} = \"aterm-{version}-linux-{arch}\"\nlinux_{arch}_sha256 = \"{digest}\"\nlinux_{arch}_size = 128\n",
        commit.to_string().repeat(40),
        "0".repeat(64)
    )
    .into_bytes();
    f.resign();
    f.state = f.context.read_state().unwrap().unwrap();
}

/// The rollback copies the update directory keeps, by name.
fn rollback_copies(f: &Fixture) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(&f.context.dir)
        .unwrap()
        .filter_map(|entry| entry.unwrap().file_name().into_string().ok())
        .filter(|name| name.starts_with("rollback-"))
        .collect();
    names.sort();
    names
}

/// A TRIAL NO ROLLBACK CAN EVER BE TAKEN FROM ENDS, AND THE COPY UPDATES AGAIN (round
/// six). A yank publishes its successor with a minimum-build floor above the yanked
/// build, so a copy on the yanked build that installs the successor records a trial
/// whose rollback target is below the floor (so is a first install's un-enrolled
/// baseline, build 0, under any floor). When that successor fails its starts, the
/// rollback is refused — and so was every later start, check, apply, reinstall and
/// `aterm update rollback`, all waiting on the trial, for good. The same holds for a
/// rollback target whose signer was revoked. Now the start past the budget ends the
/// trial, keeping the build and saying why, and a newer release installs over it.
///
/// FAILS WITHOUT THE FIX: the fourth start's rollback refusal is an error out of the
/// count (the `unwrap` below), the trial stays pending, and the release after it is
/// refused while it waits.
#[test]
fn a_trial_no_rollback_can_be_taken_from_ends_and_the_copy_updates_again() {
    for revoke in [false, true] {
        let mut f = Fixture::new();
        f.state.installed.machine_id = Some("old-machine".into());
        f.apply().unwrap();
        let mut state = f.context.read_state().unwrap().unwrap();
        if revoke {
            state.revoked_machines.push("old-machine".into());
        } else {
            state.min_build = 11;
        }
        f.context.save(&state).unwrap();
        for n in 0..MAX_TRIAL_STARTS {
            assert!(window_start(&f, minute(i64::from(n))), "start {n} is a try");
        }
        assert!(!window_start(&f, minute(i64::from(MAX_TRIAL_STARTS))));
        let state = f.context.read_state().unwrap().unwrap();
        assert!(state.trial.is_none(), "the trial is over");
        assert_eq!(state.installed.build, 11, "the build stays");
        assert_eq!(state.rejected_build, 0, "and is not refused");
        assert!(awaiting_start(&state).is_none(), "nothing waits on it");
        assert!(
            state.outcome.contains("can\u{2019}t come back"),
            "{}",
            state.outcome
        );
        assert!(!window_start(&f, minute(10)), "no start counts any more");
        stage_release(&mut f, 12, 3, 'c');
        f.apply().expect("a newer release installs over it");
        assert_eq!(f.state.installed.build, 12);
    }
}

/// ONE WINDOW THAT NEVER DREW DOES NOT LET HEALTHY SESSIONS ROLL THE BUILD BACK (round
/// six). `aterm` run with no terminal on a box with no display — or a window killed at
/// logout before its first frame — is one window start that never confirms. The person
/// then works in the terminal, as the update's own words tell them to. The first
/// session's prompt confirms the session lane and the count starts again, so the build
/// stays and checks go on; the window lane still proves itself at its next windows.
///
/// FAILS WITHOUT THE FIX: every session was refused (`WindowOwes`) while each was
/// counted, and the third one rolled back, and refused for good, a build no second
/// window ever tried.
#[test]
fn one_window_that_never_drew_does_not_let_sessions_roll_the_build_back() {
    let mut f = Fixture::new();
    f.apply().unwrap();
    assert!(window_start(&f, minute(0)));
    for m in 1..=3 {
        if session_start(&f, minute(m)) {
            assert_eq!(session_confirm(&f), Confirmed::Yes, "session {m}");
        }
    }
    let state = f.context.read_state().unwrap().unwrap();
    assert_eq!(state.installed.build, 11);
    assert_eq!(state.rejected_build, 0);
    assert!(awaiting_start(&state).is_none(), "checks and applies go on");
    let trial = state.trial.unwrap();
    assert!(trial.healthy && !trial.window_proven);
    assert_eq!(
        trial.starts, 0,
        "no session start spent the window lane's budget"
    );
}

/// The shape of the record 0.98 reads and writes (`git show
/// v0.98.0:crates/aterm-update/src/linux.rs`): no start stamp and no lane.
mod v098 {
    use super::{Identity, Phase};
    use serde::{Deserialize, Serialize};
    use std::path::PathBuf;

    #[derive(Serialize, Deserialize)]
    pub(super) struct Trial {
        old: Identity,
        new: Identity,
        phase: Phase,
        starts: u32,
        healthy: bool,
    }

    #[derive(Serialize, Deserialize)]
    pub(super) struct State {
        schema: u32,
        target: PathBuf,
        enabled: bool,
        high_water: u64,
        min_build: u64,
        roster_floor: u64,
        revoked_machines: Vec<String>,
        installed: Identity,
        trial: Option<Trial>,
        #[serde(default)]
        staged: Option<Identity>,
        rejected_build: u64,
        outcome: String,
        updated_at: String,
        failing_checks: u32,
        failing_kind: String,
        check_owner: String,
        check_repo: String,
        check_build: u64,
        last_attempt_unix: i64,
        pub(super) next_check_unix: i64,
    }
}

/// A running 0.98 taking the shared check slot: it reads the record into its own
/// shape and saves the whole of it back (v0.98 `check_with_settings`).
fn save_as_098(f: &Fixture) {
    let path = f.context.dir.join("state.toml");
    let text = String::from_utf8(read_small(&path).unwrap()).unwrap();
    let mut older: v098::State = aterm_toml::from_str(&text).unwrap();
    older.next_check_unix += 1800;
    write_atomic(&path, aterm_toml::to_string(&older).unwrap().as_bytes()).unwrap();
}

/// AN OLDER BUILD'S SAVE MID-TRIAL KEEPS THE WINDOW'S VERDICT (round six). On Linux the
/// process that installs an update is the old build, and it keeps running and checking
/// after it: every save it makes rewrites the whole record in its own shape, without
/// the fields it does not know. A session confirmed the new build and a window of it
/// crashes at every start; after 0.98's save its window starts still count and roll it
/// back. And a window that works proves the lane again after such a save without ever
/// nearing the budget.
///
/// FAILS WITHOUT THE FIX: round four's `window_owed` read "nothing owed" once 0.98 had
/// dropped it, so no window start counted and the build stayed for good.
#[test]
fn an_older_builds_save_mid_trial_keeps_the_window_verdict() {
    let mut f = Fixture::new();
    f.apply().unwrap();
    assert!(session_start(&f, minute(0)));
    assert_eq!(session_confirm(&f), Confirmed::Yes);
    save_as_098(&f);
    let trial = trial_on_disk(&f);
    assert!(trial.healthy && !trial.window_proven);
    for n in 1..=MAX_TRIAL_STARTS {
        assert!(window_start(&f, minute(i64::from(n))), "window start {n}");
    }
    assert!(!window_start(&f, minute(i64::from(MAX_TRIAL_STARTS) + 1)));
    let state = f.context.read_state().unwrap().unwrap();
    assert_eq!(state.installed.build, 10);
    assert_eq!(state.rejected_build, 11);

    let mut g = Fixture::new();
    g.apply().unwrap();
    assert!(window_start(&g, minute(0)));
    assert!(window_confirm(&g));
    for n in 1..=10 {
        save_as_098(&g);
        assert!(window_start(&g, minute(n)), "the lane proves itself again");
        assert!(window_confirm(&g));
    }
    let state = g.context.read_state().unwrap().unwrap();
    assert_eq!(
        state.installed.build, 11,
        "a working window is never rolled back"
    );
    assert!(state.trial.unwrap().window_proven);
}

/// A HEADLESS INSTANCE CONFIRMS THE SESSION LANE ONLY (round six). It opens no window,
/// so it starts through the session lane (`aterm_update::linux_session_started`, in
/// place of the window's boot count) and its confirmation at its bound socket confirms
/// that lane ([`confirm_lane`]). A trial a session confirmed still owes its window's
/// verdict after a headless run, and windows that crash still roll it back.
///
/// FAILS WITHOUT THE FIX: the headless instance was counted and confirmed as a window,
/// its bound socket proved the window lane, and no window start counted after it.
#[test]
fn a_headless_instance_confirms_the_session_lane_only() {
    assert_eq!(confirm_lane(true), Lane::Session);
    assert_eq!(confirm_lane(false), Lane::Window);
    let mut f = Fixture::new();
    f.apply().unwrap();
    assert!(session_start(&f, minute(0)));
    assert_eq!(session_confirm(&f), Confirmed::Yes);
    // The headless instance: its start, then its confirmation.
    assert!(
        !session_start(&f, minute(1)),
        "the session lane has proved itself"
    );
    let inode = fs::metadata(&f.context.target).unwrap();
    {
        let _lock = f.context.lock().unwrap();
        let mut state = f.context.read_state().unwrap().unwrap();
        assert_eq!(
            confirm_lane_locked(
                &f.context,
                &mut state,
                confirm_lane(true),
                11,
                &"b".repeat(40),
                (inode.dev(), inode.ino()),
            )
            .unwrap(),
            Confirmed::NotPending
        );
    }
    assert!(!trial_on_disk(&f).window_proven, "the window still owes");
    for n in 2..2 + i64::from(MAX_TRIAL_STARTS) {
        assert!(window_start(&f, minute(n)));
    }
    assert!(!window_start(&f, minute(2 + i64::from(MAX_TRIAL_STARTS))));
    assert_eq!(
        f.context.read_state().unwrap().unwrap().installed.build,
        10,
        "the windows that crash roll it back"
    );
}

/// A NEWER INSTALL OVER A BUILD WHOSE WINDOW NEVER CONFIRMED ROLLS BACK PAST IT (round
/// six). A session confirmed 11 and one of its windows crashed; 12 then installs over
/// it. 12's trial keeps 11's rollback target, 10, and so a window regression carried
/// into 12 rolls back to 10, not to 11, whose window crashed too. A copy used only from
/// the terminal moves its rollback target on as before, and after either install the
/// update directory keeps the one rollback copy the trial can restore.
///
/// FAILS WITHOUT THE FIX: 12's trial took 11 as its rollback target, 11's owed verdict
/// was dropped, and the rollback restored a build whose window crashes.
#[test]
fn a_newer_install_over_a_window_that_never_confirmed_rolls_back_past_it() {
    let mut f = Fixture::new();
    let baseline = f.state.installed.clone();
    f.apply().unwrap();
    assert!(session_start(&f, minute(0)));
    assert_eq!(session_confirm(&f), Confirmed::Yes);
    assert!(window_start(&f, minute(1)), "a window of 11 crashes");
    stage_release(&mut f, 12, 3, 'c');
    f.apply().unwrap();
    assert_eq!(trial_on_disk(&f).old.build, 10);
    assert_eq!(
        rollback_copies(&f),
        vec![format!("rollback-{}", baseline.sha256)]
    );
    for n in 2..2 + i64::from(MAX_TRIAL_STARTS) {
        assert!(window_start_as(&f, 12, 'c', minute(n)));
    }
    assert!(!window_start_as(
        &f,
        12,
        'c',
        minute(2 + i64::from(MAX_TRIAL_STARTS))
    ));
    let state = f.context.read_state().unwrap().unwrap();
    assert_eq!(
        state.installed.build, 10,
        "back to the last window that worked"
    );
    assert_eq!(hash_file(&f.context.target).unwrap(), baseline.sha256);
    assert_eq!(state.rejected_build, 12);
    assert!(rollback_copies(&f).is_empty(), "nothing left to restore");

    let mut g = Fixture::new();
    g.apply().unwrap();
    assert!(session_start(&g, minute(0)));
    assert_eq!(session_confirm(&g), Confirmed::Yes);
    let eleven = trial_on_disk(&g).new;
    stage_release(&mut g, 12, 3, 'c');
    g.apply().unwrap();
    assert_eq!(
        trial_on_disk(&g).old.build,
        11,
        "a copy used only from the terminal moves on"
    );
    assert_eq!(
        rollback_copies(&g),
        vec![format!("rollback-{}", eleven.sha256)]
    );
}

/// EACH UPDATE KEEPS ONE ROLLBACK COPY (round six). Every apply hard-links the
/// executable it replaces into the update directory, where it is the last name of that
/// file; nothing ever removed one, so every update of a copy's life left a whole
/// executable on disk. Now an install keeps only the copy its trial can restore, and a
/// rollback, which restores it, leaves none.
///
/// FAILS WITHOUT THE FIX: after two updates both copies are still there.
#[test]
fn each_update_keeps_only_the_rollback_copy_its_trial_can_restore() {
    let mut f = Fixture::new();
    f.apply().unwrap();
    assert!(window_start(&f, minute(0)));
    assert!(window_confirm(&f));
    let eleven = trial_on_disk(&f).new;
    stage_release(&mut f, 12, 3, 'c');
    f.apply().unwrap();
    assert_eq!(
        rollback_copies(&f),
        vec![format!("rollback-{}", eleven.sha256)]
    );
    assert!(window_start_as(&f, 12, 'c', minute(1)));
    assert!(window_confirm_as(&f, 12, 'c'));
    let twelve = trial_on_disk(&f).new;
    stage_release(&mut f, 13, 4, 'd');
    f.apply().unwrap();
    assert_eq!(
        rollback_copies(&f),
        vec![format!("rollback-{}", twelve.sha256)]
    );
    let mut state = f.context.read_state().unwrap().unwrap();
    rollback_locked(&f.context, &mut state).unwrap();
    assert_eq!(state.installed.build, 12);
    assert!(rollback_copies(&f).is_empty());
}

/// A WINDOW OF A SETTLED INSTALL LEAVES THE RECORDED OUTCOME ALONE (round six). An
/// install stays on record, confirmed, until the next one, and every window's first
/// frame confirmed it again: the outcome became "aterm X is installed" on every window
/// launch, over a failing check's reason, which then showed nowhere. A lane already
/// proved now answers that nothing was pending and writes nothing; a window proving a
/// trial a session already settled records its lane, not a new outcome.
///
/// FAILS WITHOUT THE FIX: the reason below is replaced by "aterm 0.11.0 is installed".
#[test]
fn a_window_of_a_settled_install_leaves_the_recorded_outcome_alone() {
    let reason = "machine roster signature refused";
    let fail_checks = |f: &Fixture| {
        let mut state = f.context.read_state().unwrap().unwrap();
        state.outcome = reason.into();
        state.failing_checks = 2;
        f.context.save(&state).unwrap();
    };
    let mut f = Fixture::new();
    f.apply().unwrap();
    assert!(window_start(&f, minute(0)));
    assert!(window_confirm(&f));
    fail_checks(&f);
    assert!(!window_start(&f, minute(1)));
    assert!(window_confirm(&f), "nothing is owed");
    assert_eq!(f.context.read_state().unwrap().unwrap().outcome, reason);

    let mut g = Fixture::new();
    g.apply().unwrap();
    assert!(session_start(&g, minute(0)));
    assert_eq!(session_confirm(&g), Confirmed::Yes);
    fail_checks(&g);
    assert!(window_start(&g, minute(1)));
    assert!(window_confirm(&g));
    let state = g.context.read_state().unwrap().unwrap();
    assert!(state.trial.unwrap().window_proven);
    assert_eq!(state.outcome, reason);
}

fn transaction_model() -> aterm_spec::derive::Model {
    aterm_spec::ty_model! {
        LinuxExecutableReplacement {
            const Buggy = 0;
            var verified = 0;
            var backup = 0;
            var prepared = 0;
            var replaced = 0;
            var committed = 0;
            action Verify when (verified == 0) { verified = 1; }
            action Backup when (verified == 1 && backup == 0) { backup = 1; }
            action Prepare when (backup == 1 && prepared == 0) { prepared = 1; }
            action Replace when (verified == 1 && backup == 1 && prepared == 1 && replaced == 0) { replaced = 1; }
            action Commit when (replaced == 1 && committed == 0) { committed = 1; }
            action UnsafeReplace when (Buggy == 1 && replaced == 0) { replaced = 1; }
            invariant AuthenticatedRecoverable: replaced == 0 || (verified == 1 && backup == 1 && prepared == 1);
        }
    }
}

#[test]
fn derived_transaction_model_proves_and_catches_missing_prepare_and_binds_shipping_checkpoints() {
    let model = transaction_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    let mut f = Fixture::new();
    let mut steps = Vec::new();
    apply_with_checkpoints(
        &f.context,
        &mut f.state,
        &f.proof,
        Pins {
            masters: &[&f.master],
        },
        |_, _, _| Ok(()),
        |at| {
            let state = f.context.read_state()?.ok_or("missing state")?;
            let trial = state.trial.as_ref();
            let old = trial.map_or(&state.installed, |trial| &trial.old);
            let backup = f.context.backup(old).exists();
            let replaced = hash_file(&f.context.target)? != old.sha256;
            let projection = std::collections::BTreeMap::from([
                ("verified", 1),
                ("backup", i64::from(backup)),
                ("prepared", i64::from(trial.is_some())),
                ("replaced", i64::from(replaced)),
                (
                    "committed",
                    i64::from(trial.is_some_and(|trial| trial.phase == Phase::Installed)),
                ),
            ]);
            let expected_action = match at {
                Checkpoint::Backup => "Prepare",
                Checkpoint::Prepared => "Replace",
                Checkpoint::Replaced => "Commit",
                Checkpoint::Committed => "Commit",
            };
            if at != Checkpoint::Committed {
                assert!(
                    model.action_enabled(expected_action, &projection),
                    "{at:?}: {projection:?}"
                );
            }
            assert!(!replaced || (backup && trial.is_some()));
            steps.push(at);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(
        steps,
        vec![
            Checkpoint::Backup,
            Checkpoint::Prepared,
            Checkpoint::Replaced,
            Checkpoint::Committed
        ]
    );
    let missing_prepare = std::collections::BTreeMap::from([
        ("verified", 1),
        ("backup", 1),
        ("prepared", 0),
        ("replaced", 0),
        ("committed", 0),
    ]);
    assert!(
        !model.action_enabled("Replace", &missing_prepare),
        "negative control must reject replace-before-receipt"
    );
}

/// AN INHERITED ROLLBACK TARGET MUST STILL BE ONE THE COPY MAY TAKE (round six). A
/// session confirmed 11 and one window of it crashed, so 12 would keep 11's rollback
/// target, 10. But 12 carries a yank floor above 10 (or 10's signer is revoked by the
/// time 12 installs): 10 can never come back. 12's trial rolls back to 11 instead,
/// which is permitted and proven for sessions — as it did before 12 kept its
/// predecessor's target at all.
///
/// FAILS WITHOUT THE FIX: 12's trial took 10, no copy of 11 was kept, and 12's
/// crashing windows ended the trial with 12 kept ("can't come back").
#[test]
fn an_inherited_rollback_target_a_yank_or_revocation_ruled_out_is_not_inherited() {
    for revoke in [false, true] {
        let mut f = Fixture::new();
        f.state.installed.machine_id = Some("old-machine".into());
        f.context.save(&f.state).unwrap();
        f.apply().unwrap();
        assert!(session_start(&f, minute(0)));
        assert_eq!(session_confirm(&f), Confirmed::Yes);
        assert!(window_start(&f, minute(1)), "a window of 11 crashes");
        let eleven = trial_on_disk(&f).new;
        stage_release(&mut f, 12, 3, 'c');
        if revoke {
            f.state.revoked_machines.push("old-machine".into());
        } else {
            let text = String::from_utf8(f.proof.appcast.clone()).unwrap();
            f.proof.appcast = format!("{text}min_build = 11\n").into_bytes();
            f.resign();
        }
        f.apply().unwrap();
        assert_eq!(trial_on_disk(&f).old.build, 11, "revoke={revoke}");
        assert_eq!(
            rollback_copies(&f),
            vec![format!("rollback-{}", eleven.sha256)]
        );
        for n in 2..2 + i64::from(MAX_TRIAL_STARTS) {
            assert!(window_start_as(&f, 12, 'c', minute(n)));
        }
        assert!(!window_start_as(
            &f,
            12,
            'c',
            minute(2 + i64::from(MAX_TRIAL_STARTS))
        ));
        let state = f.context.read_state().unwrap().unwrap();
        assert_eq!(state.installed.build, 11, "revoke={revoke}");
        assert_eq!(hash_file(&f.context.target).unwrap(), eleven.sha256);
        assert_eq!(state.rejected_build, 12);
    }
}

/// AN OLDER BUILD'S SAVE IN THE MIDDLE OF A BURST DOES NOT ROLL A HEALTHY BUILD BACK
/// (round six). A terminal restoring its tabs starts several sessions at once; a
/// running 0.98's save between the budget's last start and the next one drops the
/// start stamp, and "no stamp" read as the plain budget, so the next start of the
/// burst rolled back, and refused for good, a build none of whose starts had had the
/// time to print a prompt. That start now stamps and waits; the next start past the
/// budget decides as usual, so a crash loop is still rolled back.
///
/// FAILS WITHOUT THE FIX: the fourth start of the burst rolls 11 back.
#[test]
fn an_older_builds_save_mid_burst_does_not_roll_a_healthy_build_back() {
    let t0 = minute(0);
    let mut f = Fixture::new();
    f.apply().unwrap();
    for _ in 0..MAX_TRIAL_STARTS {
        assert!(session_start(&f, t0));
    }
    save_as_098(&f);
    assert_eq!(trial_on_disk(&f).last_start_unix, 0);
    assert!(session_start(&f, t0 + 1), "the burst's next start is a try");
    assert_eq!(trial_on_disk(&f).last_start_unix, t0 + 1);
    assert_eq!(session_confirm(&f), Confirmed::Yes);
    let state = f.context.read_state().unwrap().unwrap();
    assert_eq!(state.installed.build, 11);
    assert_eq!(state.rejected_build, 0);

    // A crash loop the save landed in is still rolled back, one settle window on.
    let mut g = Fixture::new();
    g.apply().unwrap();
    for _ in 0..MAX_TRIAL_STARTS {
        assert!(session_start(&g, t0));
    }
    save_as_098(&g);
    assert!(session_start(&g, t0 + 1));
    assert!(session_start(&g, t0 + 2), "still young");
    assert!(!session_start(
        &g,
        t0 + 1 + crate::linux_trial::START_SETTLES_SECS
    ));
    let state = g.context.read_state().unwrap().unwrap();
    assert_eq!(state.installed.build, 10);
    assert_eq!(state.rejected_build, 11);
}

/// AN OLDER BUILD'S SAVE BEFORE EVERY START STILL ROLLS A CRASH LOOP BACK. The 0.98
/// that installed the update keeps its check loop, and with a trial pending it saves
/// the whole record every half hour in its own shape, dropping the start stamp. A
/// window that crashes at every start, opened once an hour, finds the stamp gone at
/// each start past the budget. Only the first `LOST_STAMP_SPARES` of them are spared;
/// the next rolls back.
///
/// FAILS WITHOUT THE FIX: each start past the budget restamped and was spared, the
/// next save dropped the stamp again, and the build stayed for good (checked below
/// through twenty hourly starts) while its pending trial held every check and apply.
#[test]
fn an_older_builds_save_before_every_start_still_rolls_a_crash_loop_back() {
    let hour = |n: i64| minute(60 * n);
    let mut f = Fixture::new();
    f.apply().unwrap();
    for n in 0..i64::from(MAX_TRIAL_STARTS) {
        save_as_098(&f);
        assert!(window_start(&f, hour(n)), "window start {n}");
    }
    let mut rolled = None;
    for n in i64::from(MAX_TRIAL_STARTS)..20 {
        save_as_098(&f);
        if !window_start(&f, hour(n)) {
            rolled = Some(n);
            break;
        }
    }
    assert_eq!(
        rolled,
        Some(i64::from(
            MAX_TRIAL_STARTS + crate::linux_trial::LOST_STAMP_SPARES
        ))
    );
    let state = f.context.read_state().unwrap().unwrap();
    assert_eq!(state.installed.build, 10);
    assert_eq!(state.rejected_build, 11);
    assert!(awaiting_start(&state).is_none(), "checks and applies go on");
}

/// AN OLDER BUILD'S SAVE LATE IN A BURST ROLLS NO HEALTHY BUILD BACK. Six restored
/// tabs start inside one second; the budget's three stamp, the next are judged young
/// against the last. A 0.98 save landing after the fifth start (or the fourth) drops
/// the stamp, and the next start must be spared as a save after the budget's last
/// start spares it: its tabs have had no time to print a prompt.
///
/// FAILS WITHOUT THE FIX: only the first start past the budget was spared, so the
/// burst's sixth (or fifth) start read "no stamp" as the plain budget and rolled 11
/// back.
#[test]
fn an_older_builds_save_late_in_a_burst_does_not_roll_a_healthy_build_back() {
    let t0 = minute(0);
    for past in [2, 3] {
        let mut f = Fixture::new();
        f.apply().unwrap();
        for n in 1..=MAX_TRIAL_STARTS + 3 {
            if n == MAX_TRIAL_STARTS + past {
                save_as_098(&f);
                assert_eq!(trial_on_disk(&f).last_start_unix, 0);
            }
            assert!(session_start(&f, t0), "start {n}, save before {past} past");
        }
        assert_eq!(session_confirm(&f), Confirmed::Yes);
        let state = f.context.read_state().unwrap().unwrap();
        assert_eq!(state.installed.build, 11, "save before {past} past");
        assert_eq!(state.rejected_build, 0);
    }
}

/// A KEPT COPY THAT IS NOT ITS BUILD ENDS THE TRIAL, AND IS NEVER INHERITED (round
/// six). A rollback copy changed on disk can never be restored: the rollback failed
/// "could not be proved" at every start past the budget, while checks, applies and
/// reinstalls waited on the trial for good. Now it ends the trial as a missing copy
/// does, a newer install does not inherit it, and a stale copy under the installed
/// build's name is replaced rather than refusing every apply.
///
/// FAILS WITHOUT THE FIX: the fourth start is an error out of the count (the `unwrap`
/// in `window_start`), and the trial stays pending.
#[test]
fn a_kept_copy_that_is_not_its_build_ends_the_trial_and_is_not_inherited() {
    let corrupt = |f: &Fixture, identity: &Identity| {
        fs::write(f.context.backup(identity), elf(9, native().unwrap())).unwrap();
    };
    let mut f = Fixture::new();
    let ten = f.state.installed.clone();
    f.apply().unwrap();
    corrupt(&f, &ten);
    for n in 0..MAX_TRIAL_STARTS {
        assert!(window_start(&f, minute(i64::from(n))));
    }
    assert!(!window_start(&f, minute(i64::from(MAX_TRIAL_STARTS))));
    let state = f.context.read_state().unwrap().unwrap();
    assert!(state.trial.is_none(), "the trial is over");
    assert_eq!(state.installed.build, 11);
    assert_eq!(state.rejected_build, 0);
    assert!(awaiting_start(&state).is_none());
    assert!(
        state.outcome.contains("not that build"),
        "{}",
        state.outcome
    );
    // A stale copy under the installed build's own name does not refuse the apply.
    corrupt(&f, &state.installed);
    stage_release(&mut f, 12, 3, 'c');
    f.apply().expect("a newer release installs over it");
    let eleven = trial_on_disk(&f).old;
    assert_eq!(eleven.build, 11);
    assert_eq!(
        hash_file(&f.context.backup(&eleven)).unwrap(),
        eleven.sha256
    );

    let mut g = Fixture::new();
    g.apply().unwrap();
    assert!(session_start(&g, minute(0)));
    assert_eq!(session_confirm(&g), Confirmed::Yes);
    assert!(window_start(&g, minute(1)), "a window of 11 crashes");
    corrupt(&g, &ten);
    stage_release(&mut g, 12, 3, 'c');
    g.apply().unwrap();
    assert_eq!(
        trial_on_disk(&g).old.build,
        11,
        "a copy that is not 10 is not inherited"
    );
}

/// A master-signed roster at `seq` listing `machines` (id, key) and revoking `revoked`.
fn roster_with(
    seq: u64,
    machines: &[(&str, &Ed25519KeyPair)],
    revoked: &[&str],
) -> (Vec<u8>, Vec<u8>) {
    let roster = aterm_update_core::Roster {
        schema: 1,
        roster_seq: seq,
        valid_until: "2099-01-01T00:00:00Z".into(),
        machines: machines
            .iter()
            .map(|(id, key)| aterm_update_core::Machine {
                id: (*id).into(),
                pubkey: public(key),
                added_at: String::new(),
                not_after: None,
            })
            .collect(),
        revoked: revoked.iter().map(|id| (*id).to_string()).collect(),
    };
    let bytes = roster.to_toml().unwrap().into_bytes();
    let signature = master().sign(&bytes).as_ref().to_vec();
    (bytes, signature)
}

/// A STOLEN KEY'S MINIMUM-BUILD FLOOR GOES WITH THE KEY (round seven, H1 finding 2).
/// The thief's appcast, signed by a machine the roster still lists, carries
/// `min_build = 9_999_999_999`; the floor ratchets there. The owner revokes the
/// machine and ships the next genuine release: before the fix, `check_locked` refused
/// it — and every later one — as "below the signed minimum-build floor" forever.
#[test]
fn a_revoked_machines_minimum_build_floor_is_withdrawn() {
    let mut f = Fixture::new();
    let thief = key(13);
    let anchor = public(&master());
    let pins = Pins {
        masters: &[&anchor],
    };
    let genuine = f.proof.clone();
    (f.proof.roster, f.proof.roster_signature) =
        roster_with(1, &[("fixture", &machine()), ("thief", &thief)], &[]);
    f.proof.appcast = String::from_utf8(f.proof.appcast.clone())
        .unwrap()
        .replace("machine_id = \"fixture\"", "machine_id = \"thief\"")
        .replace(
            "build_number = 11",
            "build_number = 9999999999\nmin_build = 9999999999",
        )
        .into_bytes();
    f.proof.signature = thief.sign(&f.proof.appcast).as_ref().to_vec();
    authorize(&f.context, &mut f.state, &f.proof, pins).expect("the thief is still rostered");
    assert_eq!(f.state.min_build, 9_999_999_999);

    // The revocation, carried by the owner's next release.
    let mut next = genuine;
    (next.roster, next.roster_signature) = roster_with(2, &[("fixture", &machine())], &["thief"]);
    authorize(&f.context, &mut f.state, &next, pins).expect("the genuine release");
    assert_eq!(
        f.state.min_build, 0,
        "the thief's floor is withdrawn, so build 11 is not below it"
    );
    let durable = f.context.read_state().unwrap().unwrap();
    assert_eq!(durable.min_build, 0);
    assert!(durable.revoked_machines.contains(&"thief".to_string()));
}

/// A HELD STAGE STAYS APPLICABLE AFTER THE ROSTER MOVES (round seven, H1 finding 34).
/// Build 11 is staged and held (automatic apply off) under roster 1. A check of a head
/// with no Linux executable admits roster 2 — a machine joined — which ratchets the
/// floor. `aterm update apply` then re-authorized the saved roster-1 proof against a
/// floor of 2 and failed with "machine roster admission refused: Rollback". The check
/// now proves the held stage again under the roster it admitted.
#[test]
fn a_held_stage_is_proved_again_under_the_roster_a_later_check_admitted() {
    let mut f = Fixture::new();
    let anchor = public(&master());
    let pins = Pins {
        masters: &[&anchor],
    };
    let provider = settings_provider(false);
    let source = provider().unwrap().source;
    f.proof.save(&f.context.dir).unwrap();
    finish_candidate(
        &f.context,
        &mut f.state,
        &f.proof,
        pins,
        &source,
        &provider,
        |_, _, _| Ok(()),
    )
    .unwrap();
    assert_eq!(f.state.staged.as_ref().unwrap().build, 11);

    // The later head's check: roster 2 (a machine joined), no Linux artifact to stage.
    let joined = key(17);
    let mut head = f.proof.clone();
    (head.roster, head.roster_signature) =
        roster_with(2, &[("fixture", &machine()), ("joined", &joined)], &[]);
    authorize(&f.context, &mut f.state, &head, pins).unwrap();
    assert_eq!(f.state.roster_floor, 2);
    let stale = Proof::read(&f.context.dir).unwrap();
    let refused = apply_with_checkpoints(
        &f.context,
        &mut f.state.clone(),
        &stale,
        pins,
        |_, _, _| Ok(()),
        |_| Ok(()),
    );
    assert!(
        refused.as_ref().is_err_and(|e| e.contains("Rollback")),
        "the saved proof alone is refused by the ratcheted floor: {refused:?}"
    );

    reroot_held_stage_proof(&f.context, &mut f.state, &head, pins).unwrap();
    let proved = Proof::read(&f.context.dir).unwrap();
    assert_eq!(proved.roster, head.roster);
    f.proof = proved;
    f.apply().expect("the held stage applies");
    assert_eq!(f.state.installed.build, 11);

    // NEGATIVE CONTROL: a roster that REVOKES the stage's signer proves nothing again.
    let mut g = Fixture::new();
    g.proof.save(&g.context.dir).unwrap();
    finish_candidate(
        &g.context,
        &mut g.state,
        &g.proof,
        pins,
        &source,
        &provider,
        |_, _, _| Ok(()),
    )
    .unwrap();
    let mut revoking = g.proof.clone();
    (revoking.roster, revoking.roster_signature) =
        roster_with(2, &[("joined", &joined)], &["fixture"]);
    let before = Proof::read(&g.context.dir).unwrap().roster;
    reroot_held_stage_proof(&g.context, &mut g.state, &revoking, pins).unwrap();
    assert_eq!(Proof::read(&g.context.dir).unwrap().roster, before);
}

/// A HEAD REFUSED AFTER ITS ROSTER WAS ADMITTED STILL RE-PROVES THE HELD STAGE (round
/// seven, H1 finding 34, second exit). `authorize_one` saves the ratcheted
/// `roster_floor` before it judges the head's appcast, so a head refused there — a
/// stolen key's release carrying the owner's public roster 2 that revokes it, or a tag
/// that does not match its appcast — moved the floor and returned before the reroot:
/// `aterm update apply` then refused the held roster-1 stage as `Rollback`.
#[test]
fn a_refused_head_whose_roster_was_admitted_still_reproves_the_held_stage() {
    let anchor = public(&master());
    let pins = Pins {
        masters: &[&anchor],
    };
    let provider = settings_provider(false);
    let source = provider().unwrap().source;
    let thief = key(13);
    let stage = |f: &mut Fixture| {
        f.proof.save(&f.context.dir).unwrap();
        finish_candidate(
            &f.context,
            &mut f.state,
            &f.proof,
            pins,
            &source,
            &provider,
            |_, _, _| Ok(()),
        )
        .unwrap();
        assert_eq!(f.state.staged.as_ref().unwrap().build, 11);
    };

    // The stolen-key head: the thief's appcast, the owner's roster 2 revoking the thief.
    let mut f = Fixture::new();
    stage(&mut f);
    let mut head = f.proof.clone();
    (head.roster, head.roster_signature) = roster_with(2, &[("fixture", &machine())], &["thief"]);
    head.appcast = String::from_utf8(head.appcast.clone())
        .unwrap()
        .replace("machine_id = \"fixture\"", "machine_id = \"thief\"")
        .replace("roster_seq = 1", "roster_seq = 2")
        .replace("build_number = 11", "build_number = 12")
        .into_bytes();
    head.signature = thief.sign(&head.appcast).as_ref().to_vec();
    let refused = authenticate_head(&f.context, &mut f.state, "v0.12.0", &head, pins);
    assert!(refused.is_err(), "the thief's head is refused: {refused:?}");
    assert_eq!(f.state.roster_floor, 2, "the head's roster was admitted");
    let proved = Proof::read(&f.context.dir).unwrap();
    assert_eq!(
        proved.roster, head.roster,
        "the held stage carries roster 2"
    );
    f.proof = proved;
    f.apply().expect("the held stage applies");
    assert_eq!(f.state.installed.build, 11);

    // A genuine head whose tag does not match its appcast.
    let mut g = Fixture::new();
    stage(&mut g);
    let mut head = g.proof.clone();
    (head.roster, head.roster_signature) =
        roster_with(2, &[("fixture", &machine()), ("joined", &key(17))], &[]);
    let refused = authenticate_head(&g.context, &mut g.state, "v0.99.0", &head, pins);
    assert!(
        refused
            .as_ref()
            .is_err_and(|e| e.contains("does not match the elected release tag")),
        "the head's own refusal is the answer: {refused:?}"
    );
    assert_eq!(g.state.roster_floor, 2);
    g.proof = Proof::read(&g.context.dir).unwrap();
    assert_eq!(g.proof.roster, head.roster);
    g.apply().expect("the held stage applies");
    assert_eq!(g.state.installed.build, 11);

    // NEGATIVE CONTROL: a roster the master did not sign is never admitted, moves no
    // floor, and re-proves nothing.
    let mut n = Fixture::new();
    stage(&mut n);
    let before = Proof::read(&n.context.dir).unwrap().roster;
    let mut forged = n.proof.clone();
    (forged.roster, _) = roster_with(2, &[("fixture", &machine())], &[]);
    let refused = authenticate_head(&n.context, &mut n.state, "v0.11.0", &forged, pins);
    assert!(refused.is_err());
    assert_eq!(n.state.roster_floor, 1);
    assert_eq!(Proof::read(&n.context.dir).unwrap().roster, before);
}
