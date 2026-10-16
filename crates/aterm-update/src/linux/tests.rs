// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use ring::signature::{Ed25519KeyPair, KeyPair};

struct Fixture {
    root: PathBuf,
    context: Context,
    state: State,
    proof: Proof,
    /// The paper master the fixture's roster is signed by, the one anchor: an appcast
    /// verifies only through a roster this key signed.
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
        aterm_spec::wait::MUST_HAPPEN,
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

/// Linux only: it runs a real native executable (a copy of `/bin/sh`) through the Linux
/// loader; macOS kills a copy of an Apple binary off the system volume.
#[cfg(target_os = "linux")]
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

/// Linux only, for the same reason: it runs the restored copy of `/bin/sh`.
#[cfg(target_os = "linux")]
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

/// A yank is the one signing fact a rollback respects: a build below the signed
/// minimum-build floor never comes back, and refusing lowers no floor. A revoked
/// signer is not one ([`a_rotated_signers_build_still_comes_back_from_a_crash_loop`]).
#[test]
fn rollback_respects_the_minimum_without_lowering_any_floor() {
    let mut f = Fixture::new();
    f.state.installed.machine_id = Some("old-machine".into());
    f.apply().unwrap();
    f.state.min_build = 11;
    let current = hash_file(&f.context.target).unwrap();
    assert!(rollback_locked(&f.context, &mut f.state).is_err());
    assert_eq!(hash_file(&f.context.target).unwrap(), current);
    assert_eq!(f.state.high_water, 11);
    assert_eq!(f.state.min_build, 11);
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

/// The paper master is the one anchor: a build that pins none has nothing to verify a
/// roster (and so an appcast) against, and refuses every update channel rather than
/// falling back to a bare channel key.
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

/// A start's clock as [`session_started_in`] and [`boot_in`] ask it, under the lock,
/// reading `now`.
fn reads(now: impl Into<StartClock>) -> impl FnOnce() -> Result<StartClock, String> {
    let now = now.into();
    move || Ok(now)
}

/// A session of the file at the install path: the new build's identity and inode.
fn session_start(f: &Fixture, now: impl Into<StartClock>) -> bool {
    start_in(f, Lane::Session, reads(now))
}

/// A start of the file at the install path in `lane`, through that lane's own entry
/// ([`session_started_in`], [`boot_in`]: the unlocked look, the lock, the count), with
/// its clock read when the entry asks.
fn start_in(f: &Fixture, lane: Lane, now: impl FnOnce() -> Result<StartClock, String>) -> bool {
    let inode = fs::metadata(&f.context.target).unwrap();
    let actual = || Ok((inode.dev(), inode.ino()));
    let commit = "b".repeat(40);
    match lane {
        Lane::Session => session_started_in(&f.context, 11, &commit, actual, now),
        Lane::Window => boot_in(&f.context, 11, &commit, actual, now),
    }
    .unwrap()
}

/// A session start as [`session_start`], answering also the stamp it left on the trial
/// when it was one of the budget's starts ([`session_started_stamping_in`]).
fn session_start_stamping(f: &Fixture, now: i64) -> (bool, Option<StartClock>) {
    let inode = fs::metadata(&f.context.target).unwrap();
    session_started_stamping_in(
        &f.context,
        11,
        &"b".repeat(40),
        || Ok((inode.dev(), inode.ino())),
        reads(now),
    )
    .unwrap()
}

/// That session, still up with its shell silent, deferring the verdict at `now`
/// ([`defer_session_verdict_in`]).
fn session_defer(f: &Fixture, own: &StartClock, now: i64) -> bool {
    let inode = fs::metadata(&f.context.target).unwrap();
    defer_session_verdict_in(
        &f.context,
        11,
        &"b".repeat(40),
        || Ok((inode.dev(), inode.ino())),
        own,
        reads(now),
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
fn window_start(f: &Fixture, now: impl Into<StartClock>) -> bool {
    window_start_as(f, 11, 'b', now)
}

/// A window of release `build` (whose commit is `commit` forty times) starting.
fn window_start_as(f: &Fixture, build: u64, commit: char, now: impl Into<StartClock>) -> bool {
    let inode = fs::metadata(&f.context.target).unwrap();
    let _lock = f.context.lock().unwrap();
    let mut state = f.context.read_state().unwrap().unwrap();
    boot_locked(
        &f.context,
        &mut state,
        build,
        &commit.to_string().repeat(40),
        (inode.dev(), inode.ino()),
        now,
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

/// A copy used only from the terminal finishes its update through the session lane: the
/// install is counted and confirmed at the first prompt, its words ask for no window, and
/// the copy updates again.
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

/// A session still running the old file (a tab opened before the install) is no start of
/// the new build's trial: it counts nothing and confirms nothing.
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
            reads(minute(0)),
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

/// A session does not vouch for a build whose window has not confirmed: two windows crash
/// before their first frame, and a session typed into a terminal confirms the session lane
/// only: the window starts count on from zero until the fourth that never confirms rolls
/// the install back.
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
    // A window that does confirm settles every lane.
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

/// A window still owes its verdict after a session confirmed: its starts count from zero,
/// and the start past the budget rolls the build back.
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

/// Six tabs restored at once are six starts before the first prompt: a start over the
/// budget waits while the last can still confirm, and a quiet gap delivers the verdict.
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

/// A burst of healthy but silent session starts is not rolled back by the next tab: the
/// budget's last start, still silent half a window on, moves the stamp once, so a fifth tab
/// at 21 s is young against the move; a build that never speaks is rolled back a window
/// later. Control: the same burst with no deferral rolls back at the fifth tab.
#[test]
fn a_silent_session_still_up_buys_its_burst_one_more_window() {
    let t0 = minute(0);
    // Four tabs within a second: the budget's three, the last at `t0 + 1`, and one past.
    let burst = |f: &Fixture| {
        let mut stamps = Vec::new();
        for (tab, at) in [t0, t0, t0 + 1, t0 + 1].into_iter().enumerate() {
            let (starts_trial, stamped) = session_start_stamping(f, at);
            assert!(
                starts_trial,
                "tab {tab} of the burst is a start of the trial"
            );
            stamps.push(stamped);
        }
        assert_eq!(trial_on_disk(f).starts, 4);
        assert_eq!(
            stamps[3], None,
            "the start past the budget leaves the stamp as it was"
        );
        stamps
    };

    // Control: no deferral, and the fifth tab at 21 s rolls the healthy build back.
    let mut silent = Fixture::new();
    silent.apply().unwrap();
    burst(&silent);
    assert!(!session_start(&silent, t0 + 21));
    let state = silent.context.read_state().unwrap().unwrap();
    assert_eq!(
        (state.installed.build, state.rejected_build),
        (10, 11),
        "PRECONDITION: without the deferral the burst is rolled back"
    );

    let mut f = Fixture::new();
    f.apply().unwrap();
    let stamps = burst(&f);
    let first = stamps[0].clone().expect("the first start stamped");
    let own = stamps[2].clone().expect("the budget's last start stamped");
    assert_eq!(own, StartClock::from(t0 + 1));
    assert!(
        !session_defer(&f, &own, t0 + 1),
        "no move to the moment of the stamp itself"
    );
    // A stamp an earlier start of the budget left is not the trial's any more.
    assert!(!session_defer(&f, &first, t0 + 10));
    // Half a window on, still up and silent: the stamp moves, once.
    assert!(session_defer(&f, &own, t0 + 11));
    assert_eq!(trial_on_disk(&f).start_clock(), StartClock::from(t0 + 11));
    assert!(!session_defer(&f, &own, t0 + 12), "once");
    assert!(!trial_on_disk(&f).healthy, "the move confirms nothing");
    // The fifth tab, before any prompt: young against the move.
    assert!(
        session_start(&f, t0 + 21),
        "no rollback for one more window"
    );
    assert_eq!(f.context.read_state().unwrap().unwrap().installed.build, 11);
    // A build that still never speaks is rolled back a window after the move.
    assert!(!session_start(&f, t0 + 31));
    let state = f.context.read_state().unwrap().unwrap();
    assert_eq!((state.installed.build, state.rejected_build), (10, 11));

    // A healthy one confirms at its first prompt, as any start does.
    let mut confirmed = Fixture::new();
    confirmed.apply().unwrap();
    let stamps = burst(&confirmed);
    assert!(session_defer(
        &confirmed,
        stamps[2].as_ref().expect("stamped"),
        t0 + 11
    ));
    assert!(session_start(&confirmed, t0 + 21));
    assert_eq!(session_confirm(&confirmed), Confirmed::Yes);
    assert!(trial_on_disk(&confirmed).healthy);

    // A move that lands late, the stamp already settled, defers nothing: the verdict is
    // due, and the next start delivers it.
    let mut late = Fixture::new();
    late.apply().unwrap();
    let stamps = burst(&late);
    let own = stamps[2].clone().expect("stamped");
    assert!(!session_defer(
        &late,
        &own,
        t0 + 1 + crate::linux_trial::START_SETTLES_SECS
    ));
    assert!(!session_start(&late, t0 + 21));
}

/// A silent session's deferral outlasts a held update lock: a first try that fails
/// [`LOCK_BUSY`] is retried after the pause and lands, and the fifth tab at 21 s is kept.
/// Controls: one try loses the move; the shell speaking ends the tries; a settled stamp
/// ends them with no pause; a refusal is never tried again.
#[test]
fn a_silent_session_s_deferral_outlasts_a_held_update_lock() {
    let t0 = minute(0);
    let commit = "b".repeat(40);
    // Four tabs within a second; the stamp the budget's last one left.
    let burst = |f: &Fixture| {
        let stamps = [t0, t0, t0 + 1, t0 + 1]
            .into_iter()
            .map(|at| session_start_stamping(f, at))
            .collect::<Vec<_>>();
        assert!(stamps.iter().all(|(starts_trial, _)| *starts_trial));
        stamps[2]
            .1
            .clone()
            .expect("the budget's last start stamped")
    };
    let try_at = |f: &Fixture, own: &StartClock, now: i64| {
        let inode = fs::metadata(&f.context.target).unwrap();
        defer_session_verdict_in(
            &f.context,
            11,
            &commit,
            || Ok((inode.dev(), inode.ino())),
            own,
            reads(now),
        )
    };

    // Control: one try. The held lock loses the move for good.
    let mut once = Fixture::new();
    once.apply().unwrap();
    let own = burst(&once);
    {
        let _sibling = once.context.lock().unwrap();
        assert_eq!(try_at(&once, &own, t0 + 11), Err(LOCK_BUSY.to_string()));
    }
    assert!(
        !session_start(&once, t0 + 21),
        "PRECONDITION: a move lost to the lock leaves the burst to be rolled back"
    );

    let mut f = Fixture::new();
    f.apply().unwrap();
    let own = burst(&f);
    let mut sibling = Some(f.context.lock().unwrap());
    let mut tries = Vec::new();
    let mut pauses = Vec::new();
    let moved = defer_session_verdict_retrying(
        &own,
        || {
            let answer = try_at(&f, &own, t0 + 11);
            tries.push(answer.clone());
            answer
        },
        || Ok(StartClock::from(t0 + 11)),
        |pause| {
            pauses.push(pause);
            sibling = None;
            true
        },
    );
    assert!(moved, "the try after the pause lands");
    assert_eq!(tries, [Err(LOCK_BUSY.to_string()), Ok(true)]);
    assert_eq!(pauses, [SESSION_DEFER_RETRY]);
    assert_eq!(trial_on_disk(&f).start_clock(), StartClock::from(t0 + 11));
    assert!(!trial_on_disk(&f).healthy, "the move confirms nothing");
    assert!(session_start(&f, t0 + 21), "the fifth tab is kept");

    // The shell spoke in the pause: its confirmation counts now, and no try follows.
    let mut spoke = Fixture::new();
    spoke.apply().unwrap();
    let own = burst(&spoke);
    let _sibling = spoke.context.lock().unwrap();
    let mut tries = 0;
    assert!(!defer_session_verdict_retrying(
        &own,
        || {
            tries += 1;
            try_at(&spoke, &own, t0 + 11)
        },
        || Ok(StartClock::from(t0 + 11)),
        |_| false,
    ));
    assert_eq!(tries, 1);

    // A stamp no longer young: the verdict is due, and nothing waits for the lock.
    assert!(!defer_session_verdict_retrying(
        &own,
        || Err(LOCK_BUSY.to_string()),
        || Ok(StartClock::from(
            t0 + 1 + crate::linux_trial::START_SETTLES_SECS
        )),
        |_| panic!("no pause once the stamp has settled"),
    ));
    // Nor a clock that cannot be read.
    assert!(!defer_session_verdict_retrying(
        &own,
        || Err(LOCK_BUSY.to_string()),
        || Err("no clock".to_string()),
        |_| panic!("no pause without a clock"),
    ));
    // A refusal is an answer.
    let mut tries = 0;
    assert!(!defer_session_verdict_retrying(
        &own,
        || {
            tries += 1;
            Ok(false)
        },
        || Ok(StartClock::from(t0 + 11)),
        |_| panic!("a refusal is not tried again"),
    ));
    assert_eq!(tries, 1);
}

/// A crash loop faster than the settle window (`aterm --headless` under `Restart=always`,
/// `RestartSec=5`) is still rolled back a window after the budget's last stamp.
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
/// reads as no start stamped and a window lane not yet proved.
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

/// A trial no rollback can ever be taken from (its target below the signed floor, or its
/// kept copy gone) ends at the start past the budget, keeping the build and saying why, and
/// a newer release installs over it.
#[test]
fn a_trial_no_rollback_can_be_taken_from_ends_and_the_copy_updates_again() {
    for copy_gone in [false, true] {
        let mut f = Fixture::new();
        f.state.installed.machine_id = Some("old-machine".into());
        f.apply().unwrap();
        let mut state = f.context.read_state().unwrap().unwrap();
        if copy_gone {
            let trial = state.trial.as_ref().unwrap();
            fs::remove_file(f.context.backup(&trial.old)).unwrap();
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

/// A machine rotation strands no crash-looping update: when a release revokes the signer of
/// the build it replaced and then fails its starts, that build still comes back,
/// automatically or by `aterm update rollback`. A yank is still respected
/// ([`rollback_respects_the_minimum_without_lowering_any_floor`]).
#[test]
fn a_rotated_signers_build_still_comes_back_from_a_crash_loop() {
    for manual in [false, true] {
        let mut f = Fixture::new();
        f.state.installed.machine_id = Some("m3".into());
        let old = f.state.installed.clone();
        // The release check that admits 11 records the roster's revocation of m3.
        f.state.revoked_machines.push("m3".into());
        f.context.save(&f.state).unwrap();
        f.apply().unwrap();
        assert_eq!(trial_on_disk(&f).old.machine_id.as_deref(), Some("m3"));
        if manual {
            let mut state = f.context.read_state().unwrap().unwrap();
            rollback_locked(&f.context, &mut state).expect("aterm update rollback");
        } else {
            for n in 0..MAX_TRIAL_STARTS {
                assert!(window_start(&f, minute(i64::from(n))), "start {n} is a try");
            }
            assert!(!window_start(&f, minute(i64::from(MAX_TRIAL_STARTS))));
        }
        let state = f.context.read_state().unwrap().unwrap();
        assert_eq!(
            hash_file(&f.context.target).unwrap(),
            old.sha256,
            "manual={manual}: {}",
            state.outcome
        );
        assert_eq!(state.installed.build, 10, "manual={manual}");
        assert_eq!(state.rejected_build, 11, "manual={manual}");
        assert!(state.trial.is_none(), "manual={manual}");
        assert!(
            state.revoked_machines.contains(&"m3".to_string()),
            "the revocation stands for every future acceptance"
        );
    }
}

/// One window that never drew (no display, killed at logout) does not let healthy sessions
/// roll the build back: the first session's prompt confirms the session lane and the count
/// starts again.
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

/// The shape of the record 0.98 reads and writes: no start stamp and no lane.
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

/// An older build's save mid-trial keeps the window's verdict: after 0.98's whole-record
/// save a crashing window's starts still roll the build back, and a working window proves
/// the lane again without nearing the budget.
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

/// A headless instance confirms the session lane only ([`confirm_lane`]), so a window still
/// owes its verdict and windows that crash still roll the build back.
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

/// A newer install over a build whose window never confirmed rolls back past it: 12's trial
/// keeps 11's target, 10. A copy used only from the terminal moves its target on, and either
/// install keeps only the one rollback copy its trial can restore.
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

/// Each update keeps one rollback copy: an install keeps only the copy its trial can
/// restore, and a rollback, which restores it, leaves none.
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

/// A window of a settled install leaves the recorded outcome alone, so a failing check's
/// reason stays; a window proving a trial a session settled records its lane only.
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

/// An inherited rollback target must still be one the copy may take: 12 would keep 11's
/// target, 10, but a yank floor above 10 rules it out, so 12 rolls back to 11. A revocation
/// of 10's signer rules nothing out.
#[test]
fn an_inherited_rollback_target_a_yank_ruled_out_is_not_inherited() {
    for yank in [true, false] {
        let mut f = Fixture::new();
        f.state.installed.machine_id = Some("old-machine".into());
        f.context.save(&f.state).unwrap();
        let ten = f.state.installed.clone();
        f.apply().unwrap();
        assert!(session_start(&f, minute(0)));
        assert_eq!(session_confirm(&f), Confirmed::Yes);
        assert!(window_start(&f, minute(1)), "a window of 11 crashes");
        let eleven = trial_on_disk(&f).new;
        stage_release(&mut f, 12, 3, 'c');
        if yank {
            let text = String::from_utf8(f.proof.appcast.clone()).unwrap();
            f.proof.appcast = format!("{text}min_build = 11\n").into_bytes();
            f.resign();
        } else {
            f.state.revoked_machines.push("old-machine".into());
        }
        f.apply().unwrap();
        let target = if yank { &eleven } else { &ten };
        assert_eq!(trial_on_disk(&f).old.build, target.build, "yank={yank}");
        assert_eq!(
            rollback_copies(&f),
            vec![format!("rollback-{}", target.sha256)],
            "yank={yank}"
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
        assert_eq!(state.installed.build, target.build, "yank={yank}");
        assert_eq!(hash_file(&f.context.target).unwrap(), target.sha256);
        assert_eq!(state.rejected_build, 12);
    }
}

/// An older build's save in the middle of a burst rolls no healthy build back, and a crash
/// loop is still rolled back.
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

/// `unix` on the wall clock, read with `secs` on boot `boot`'s steady clock.
fn clock_at(unix: i64, boot: &str, secs: i64) -> StartClock {
    StartClock {
        unix,
        steady: Some(Steady {
            clock: format!("{boot} time:[4026531834]"),
            secs,
        }),
    }
}

/// A clock step in the middle of a burst rolls no healthy build back: the age between two
/// starts is read on the steady clock both share. A crash loop still rolls back however the
/// wall clock moves, and across a boot the wall clock judges.
#[test]
fn a_clock_step_mid_burst_rolls_no_healthy_build_back() {
    let t0 = minute(0);
    let settles = crate::linux_trial::START_SETTLES_SECS;
    for step in [3_600, -3_600, -1] {
        let mut f = Fixture::new();
        f.apply().unwrap();
        for _ in 0..MAX_TRIAL_STARTS {
            assert!(session_start(&f, clock_at(t0, "boot-a", 500)));
        }
        // NTP steps the wall clock; the burst's next start, the same steady second.
        assert!(
            session_start(&f, clock_at(t0 + step, "boot-a", 500)),
            "step {step}: the burst's next start is a try"
        );
        assert_eq!(session_confirm(&f), Confirmed::Yes, "step {step}");
        let state = f.context.read_state().unwrap().unwrap();
        assert_eq!(state.installed.build, 11, "step {step}: {}", state.outcome);
        assert_eq!(state.rejected_build, 0, "step {step}");
    }
    // A crash loop is rolled back whatever the wall clock does meanwhile: a settle
    // window on the steady clock after the budget's last start.
    for step in [3_600, -3_600, 0] {
        let mut g = Fixture::new();
        g.apply().unwrap();
        for _ in 0..MAX_TRIAL_STARTS {
            assert!(session_start(&g, clock_at(t0, "boot-a", 500)));
        }
        assert!(
            session_start(&g, clock_at(t0 + step, "boot-a", 500 + settles - 1)),
            "step {step}: still young on the steady clock"
        );
        assert!(
            !session_start(&g, clock_at(t0 + step, "boot-a", 500 + settles)),
            "step {step}: settled on the steady clock"
        );
        let state = g.context.read_state().unwrap().unwrap();
        assert_eq!(state.installed.build, 10, "step {step}");
        assert_eq!(state.rejected_build, 11, "step {step}");
    }
    // Across a boot no steady clock spans both starts, so the wall clock judges, as it
    // does for a start that read no steady clock.
    for next in [
        clock_at(t0 + 3_600, "boot-b", 7),
        StartClock::from(t0 + 3_600),
    ] {
        let mut h = Fixture::new();
        h.apply().unwrap();
        for _ in 0..MAX_TRIAL_STARTS {
            assert!(session_start(&h, clock_at(t0, "boot-a", 500)));
        }
        assert!(!session_start(&h, next.clone()), "{next:?}");
        assert_eq!(
            h.context.read_state().unwrap().unwrap().rejected_build,
            11,
            "{next:?}"
        );
    }
}

/// Whether this copy's update lock is held right now, by anyone, this process's own
/// count included (each take opens its own descriptor of the lock file): a take that
/// may not wait is refused.
fn the_lock_is_held(f: &Fixture) -> bool {
    FileLock::acquire_within(&f.context.dir.join("lock"), Duration::ZERO)
        .is_err_and(|error| error.kind() == std::io::ErrorKind::TimedOut)
}

/// A burst's clock as one of its starts reads it: the boot's monotonic second, which
/// `steady` holds at the read, and the wall clock as far past `t0` as that is past 500.
/// Read only with the lock held, or the test fails.
fn monotonic<'a>(
    f: &'a Fixture,
    steady: &'a std::cell::Cell<i64>,
    t0: i64,
) -> impl FnOnce() -> Result<StartClock, String> + 'a {
    move || {
        assert!(
            the_lock_is_held(f),
            "a start reads its clock holding the lock"
        );
        let secs = steady.get();
        Ok(clock_at(t0 + secs - 500, "boot-a", secs))
    }
}

/// A start that waited on the lock rolls no healthy build back: in both lanes
/// ([`session_started_in`], [`boot_in`]) a start reads its clock holding the lock, so on one
/// boot's monotonic clock the age between two starts is never negative. Negative control: a
/// clock read before the wait, handed to the same locked count, rolls 11 back.
#[test]
fn a_start_that_waited_on_the_lock_rolls_no_healthy_build_back() {
    let t0 = minute(0);
    for lane in [Lane::Session, Lane::Window] {
        let mut f = Fixture::new();
        f.apply().unwrap();
        assert!(
            !the_lock_is_held(&f),
            "{lane:?}: the probe sees a free lock"
        );
        // The burst's fourth start begins at steady 500 and finds the trial pending;
        // the three before it take the lock first, a second later.
        let steady = std::cell::Cell::new(500);
        let began = clock_at(t0, "boot-a", 500);
        steady.set(501);
        for _ in 0..MAX_TRIAL_STARTS {
            assert!(start_in(&f, lane, monotonic(&f, &steady, t0)), "{lane:?}");
        }
        assert_eq!(
            trial_on_disk(&f).start_clock(),
            clock_at(t0 + 1, "boot-a", 501),
            "{lane:?}"
        );
        // Then the fourth holds the lock, and reads its clock.
        assert!(
            start_in(&f, lane, monotonic(&f, &steady, t0)),
            "{lane:?}: the burst's fourth start is a try"
        );
        let state = f.context.read_state().unwrap().unwrap();
        assert_eq!(state.installed.build, 11, "{lane:?}: {}", state.outcome);
        assert_eq!(state.rejected_build, 0, "{lane:?}");

        // Negative control: the reading it took before the wait, as it was counted.
        let mut g = Fixture::new();
        g.apply().unwrap();
        for _ in 0..MAX_TRIAL_STARTS {
            assert!(start_in(&g, lane, reads(clock_at(t0 + 1, "boot-a", 501))));
        }
        assert!(!start_in(&g, lane, reads(began)), "{lane:?}");
        let state = g.context.read_state().unwrap().unwrap();
        assert_eq!(
            (state.installed.build, state.rejected_build),
            (10, 11),
            "{lane:?}"
        );
    }
    // The shipping entries hand over the clock itself, to be read under the lock, not
    // a reading taken before it.
    let src: String = include_str!("../linux.rs").split_whitespace().collect();
    for entry in ["boot_in(", "session_started_stamping_in("] {
        assert!(
            src.contains(&format!(
                "{entry}context,current_build,current_commit,proc_self_inode,start_clock,)"
            )),
            "{entry} is handed `start_clock` unread"
        );
    }
    // And so is a silent session's deferral, which stamps as a start does, and its
    // retry, which asks the stamp's youth before each pause.
    assert!(
        src.contains(
            "defer_session_verdict_in(context,current_build,current_commit,proc_self_inode,&own,\
             start_clock,)"
        ),
        "the deferral is handed `start_clock` unread"
    );
    assert!(
        src.contains("},start_clock,pause,)"),
        "the deferral's retry is handed `start_clock` unread"
    );
    let early = src
        .match_indices("start_clock()")
        .filter(|(at, _)| !src[..*at].ends_with('.') && !src[..*at].ends_with("fn"))
        .count();
    assert_eq!(early, 0, "no start reads its clock before the lock");
}

/// The steady clock names its boot (boot id and time namespace) and never runs back; a
/// boot with no name to read gives no steady stamp. The stamp crosses the record, and an
/// older build's save drops it with the wall-clock one.
#[test]
fn the_steady_clock_names_its_boot_and_never_runs_back() {
    let mut f = Fixture::new();
    let boot = f.root.join("boot_id");
    let namespace = f.root.join("time-ns");
    fs::write(&boot, "0f5a1c6e-3b0e-4c55-9d4e-6c1f2a7b9e10\n").unwrap();
    std::os::unix::fs::symlink("time:[4026531834]", &namespace).unwrap();
    let first = steady_clock(&boot, &namespace).expect("a named boot reads a steady clock");
    let second = steady_clock(&boot, &namespace).unwrap();
    assert_eq!(
        first.clock,
        "0f5a1c6e-3b0e-4c55-9d4e-6c1f2a7b9e10 time:[4026531834]"
    );
    assert_eq!(first.clock, second.clock);
    assert!(second.secs >= first.secs, "{first:?} then {second:?}");
    assert_eq!(
        steady_clock(&boot, &f.root.join("no-namespace"))
            .unwrap()
            .clock,
        "0f5a1c6e-3b0e-4c55-9d4e-6c1f2a7b9e10 ",
        "a kernel with no time namespaces"
    );
    fs::write(&boot, "\n").unwrap();
    assert_eq!(steady_clock(&boot, &namespace), None, "an empty boot id");
    assert_eq!(
        steady_clock(&f.root.join("absent"), &namespace),
        None,
        "no boot id"
    );
    #[cfg(target_os = "linux")]
    assert!(
        steady_clock(Path::new(BOOT_ID), Path::new(TIME_NAMESPACE)).is_some(),
        "Linux names its boot"
    );

    f.apply().unwrap();
    let stamp = clock_at(minute(0), "boot-a", 500);
    assert!(session_start(&f, stamp.clone()));
    assert_eq!(
        trial_on_disk(&f).start_clock(),
        stamp,
        "the record keeps both"
    );
    save_as_098(&f);
    assert_eq!(
        trial_on_disk(&f).start_clock(),
        StartClock::default(),
        "an older build reads the record, and its save drops both stamps"
    );
}

/// An older build's save before every start still rolls a crash loop back: only the first
/// `LOST_STAMP_SPARES` starts past the budget are spared.
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

/// An older build's save late in a burst (after the fourth or fifth start) rolls no healthy
/// build back: the next start is spared.
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

/// A kept copy that is not its build ends the trial, as a missing copy does, and is never
/// inherited by a newer install; a stale copy under the installed build's name is replaced
/// rather than refusing every apply.
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

/// A kept copy the rollback would refuse to open (lost exec bit, group-writable,
/// unreadable) ends the trial too, and is never inherited.
#[test]
fn a_kept_copy_the_rollback_would_refuse_ends_the_trial_and_is_not_inherited() {
    use std::os::unix::fs::PermissionsExt;
    // 0o000 and 0o111: a copy we may not read fails the rollback's open with `EACCES`
    // at every try, a fact, never a retry.
    for mode in [0o644, 0o775, 0o000, 0o111] {
        let spoil = |f: &Fixture, identity: &Identity| {
            fs::set_permissions(f.context.backup(identity), fs::Permissions::from_mode(mode))
                .unwrap();
        };
        let mut f = Fixture::new();
        let ten = f.state.installed.clone();
        f.apply().unwrap();
        spoil(&f, &ten);
        for n in 0..MAX_TRIAL_STARTS {
            assert!(window_start(&f, minute(i64::from(n))));
        }
        assert!(!window_start(&f, minute(i64::from(MAX_TRIAL_STARTS))));
        let state = f.context.read_state().unwrap().unwrap();
        assert!(state.trial.is_none(), "mode {mode:o}: the trial is over");
        assert_eq!(state.installed.build, 11);
        assert!(awaiting_start(&state).is_none());
        assert!(
            state.outcome.contains("not safe to restore"),
            "mode {mode:o}: {}",
            state.outcome
        );

        let mut g = Fixture::new();
        g.apply().unwrap();
        assert!(session_start(&g, minute(0)));
        assert_eq!(session_confirm(&g), Confirmed::Yes);
        assert!(window_start(&g, minute(1)), "a window of 11 crashes");
        spoil(&g, &ten);
        stage_release(&mut g, 12, 3, 'c');
        g.apply()
            .unwrap_or_else(|error| panic!("mode {mode:o}: a newer release installs: {error}"));
        assert_eq!(
            trial_on_disk(&g).old.build,
            11,
            "mode {mode:o}: a copy the rollback would refuse is not inherited"
        );
    }
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

/// A stolen key's minimum-build floor goes with the key: the owner's next release, revoking
/// the thief, withdraws the thief's `min_build = 9_999_999_999`.
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

/// A held stage stays applicable after the roster moves: a check of a head with nothing to
/// stage admits roster 2, and the held stage is proved again under it.
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

    // Negative control: a roster that revokes the stage's signer proves nothing again.
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

/// A head refused after its roster was admitted still re-proves the held stage: the
/// refusal comes after the floor moved, and the reroot keeps the stage applicable.
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

    // Negative control: a roster the master did not sign is never admitted, moves no
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

/// How many files `run` hashed on this thread ([`HASHED`]).
fn hashes_in<T>(run: impl FnOnce() -> T) -> (T, u32) {
    let before = HASHED.with(std::cell::Cell::get);
    let value = run();
    (value, HASHED.with(std::cell::Cell::get) - before)
}

/// The fixture's release, staged and held for `aterm update apply`
/// (`[update] auto_apply = false`).
fn hold_stage(f: &mut Fixture) {
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
    assert_eq!(f.state.staged.as_ref().map(|stage| stage.build), Some(11));
}

/// A held download is hashed once, not at every pass of every process's loop: only a file
/// that is not the one last hashed ([`FileStamp`]) is hashed again. Negative controls: a
/// candidate changed in place (times set explicitly, since a same-tick rewrite moves no
/// part of the stamp) or replaced is hashed again and judged on its bytes.
#[test]
fn a_held_stage_is_hashed_once_not_at_every_pass() {
    let mut f = Fixture::new();
    hold_stage(&mut f);
    let (offered, hashed) = hashes_in(|| {
        (0..60)
            .map(|_| linux_status(&f.context, &f.state).staged_build)
            .collect::<Vec<_>>()
    });
    assert!(
        offered.iter().all(|build| *build == Some(11)),
        "{offered:?}"
    );
    assert!(
        hashed <= 1,
        "sixty passes hashed the held stage {hashed} times"
    );

    let candidate = f.context.dir.join("candidate");
    let bytes = fs::read(&candidate).unwrap();
    // A day before the stamped modification time: a stamp of its own, at any clock.
    let modified =
        fs::metadata(&candidate).unwrap().modified().unwrap() - Duration::from_secs(86_400);
    let set_modified = |at: std::time::SystemTime| {
        File::options()
            .write(true)
            .open(&candidate)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(at))
            .unwrap();
    };
    let mut changed = bytes.clone();
    changed[100] ^= 1;
    fs::write(&candidate, &changed).unwrap();
    set_modified(modified);
    let (offered, hashed) = hashes_in(|| linux_status(&f.context, &f.state).staged_build);
    assert_eq!(
        (offered, hashed),
        (None, 1),
        "changed in place, same size: hashed again"
    );
    // A new size, under the modification time just stamped: only the size is sure to
    // be new.
    let mut longer = bytes.clone();
    longer.push(0);
    fs::write(&candidate, &longer).unwrap();
    set_modified(modified);
    let (offered, hashed) = hashes_in(|| linux_status(&f.context, &f.state).staged_build);
    assert_eq!(
        (offered, hashed),
        (None, 1),
        "changed in place, new size: hashed again"
    );

    let replacement = f.context.dir.join("replacement");
    fs::write(&replacement, &bytes).unwrap();
    fs::rename(&replacement, &candidate).unwrap();
    let (offered, hashed) = hashes_in(|| linux_status(&f.context, &f.state).staged_build);
    assert_eq!((offered, hashed), (Some(11), 1), "replaced: hashed again");
    fs::remove_file(&candidate).unwrap();
    assert_eq!(linux_status(&f.context, &f.state).staged_build, None);
}

/// The start of a session or a window hashes nothing: whether a copy runs the background
/// check loop is asked of the small state record alone, not of `status()`.
#[test]
fn whether_the_check_loop_runs_is_asked_without_hashing() {
    let mut f = Fixture::new();
    hold_stage(&mut f);
    let (enrolled, hashed) = hashes_in(|| enrolled_in(&f.context));
    assert_eq!((enrolled, hashed), (true, 0));
    fs::remove_file(f.context.dir.join("state.toml")).unwrap();
    assert!(
        !enrolled_in(&f.context),
        "a copy with no record is not enrolled"
    );
    let src: String = include_str!("../linux.rs").split_whitespace().collect();
    let start = src
        .find("pub(crate)fnspawn_background_check_with_settings(")
        .expect("the background loop");
    let gate = &src[start..start + src[start..].find("std::thread::Builder").unwrap()];
    assert!(
        gate.contains("context().is_ok_and(enrolled_in)") && !gate.contains("status("),
        "the loop's gate reads the record alone: {gate}"
    );
}

/// The copy's update lock, held as another aterm holds it: its own descriptor of the
/// lock file, which this process's [`Context::lock`] then waits on and gives up.
fn another_aterm_holds_the_lock(f: &Fixture) -> aterm_update_core::FileLock {
    aterm_update_core::FileLock::acquire(&f.context.dir.join("lock")).unwrap()
}

/// A window of a settled install starts without the update lock or a hash. Negative
/// control: a trial whose window lane still owes its verdict is counted under the lock.
#[test]
fn a_window_of_a_settled_install_starts_without_the_lock_or_a_hash() {
    let commit = "b".repeat(40);
    let mut f = Fixture::new();
    f.apply().unwrap();
    assert!(window_start(&f, minute(0)));
    assert!(window_confirm(&f));
    let unasked = || -> Result<(u64, u64), String> { panic!("no inode is asked") };
    let (started, hashed) =
        hashes_in(|| boot_in(&f.context, 11, &commit, unasked, reads(minute(1))));
    assert_eq!((started, hashed), (Ok(false), 0));
    let held = another_aterm_holds_the_lock(&f);
    let (started, hashed) =
        hashes_in(|| boot_in(&f.context, 11, &commit, unasked, reads(minute(2))));
    assert_eq!((started, hashed), (Ok(false), 0), "the lock is not taken");
    drop(held);

    let mut g = Fixture::new();
    g.apply().unwrap();
    assert!(session_start(&g, minute(0)));
    assert_eq!(session_confirm(&g), Confirmed::Yes);
    let _held = another_aterm_holds_the_lock(&g);
    assert_eq!(
        boot_in(&g.context, 11, &commit, unasked, reads(minute(1))),
        Err(LOCK_BUSY.to_string()),
        "the window lane still owes its verdict"
    );
}

/// A vetoed automatic install keeps the trial it would have replaced: abandoning the
/// prepared intent puts the displaced trial back, so 11's crashed window start still counts
/// and the person's install of 12 still rolls back past 11.
#[test]
fn a_vetoed_automatic_install_keeps_the_trial_it_would_have_replaced() {
    let mut f = Fixture::new();
    let baseline = f.state.installed.clone();
    f.apply().unwrap();
    assert!(session_start(&f, minute(0)));
    assert_eq!(session_confirm(&f), Confirmed::Yes);
    assert!(window_start(&f, minute(1)), "a window of 11 crashes");
    stage_release(&mut f, 12, 3, 'c');
    let source = settings_provider(true)().unwrap().source;
    let vetoed = std::sync::Arc::new(AtomicBool::new(false));
    let provider: crate::CheckSettingsProvider = {
        let vetoed = std::sync::Arc::clone(&vetoed);
        let source = source.clone();
        std::sync::Arc::new(move || {
            (!vetoed.load(Ordering::SeqCst)).then(|| crate::CheckSettings {
                source: source.clone(),
                auto_apply: true,
            })
        })
    };
    let mut probes = 0;
    let held = finish_candidate(
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
                vetoed.store(true, Ordering::SeqCst);
            }
            Ok(())
        },
    )
    .unwrap();
    assert!(held.contains("`aterm update apply` installs it"), "{held}");
    assert_eq!(probes, 2, "the install was prepared");
    let trial = trial_on_disk(&f);
    assert_eq!((trial.old.build, trial.new.build), (10, 11));
    assert_eq!(trial.phase, Phase::Installed);
    assert!(trial.healthy && !trial.window_proven);
    assert_eq!(trial.starts, 1, "the window start still counts");
    assert_eq!(
        rollback_copies(&f),
        vec![format!("rollback-{}", baseline.sha256)]
    );
    assert_eq!(f.state.staged.as_ref().map(|stage| stage.build), Some(12));
    // The person's install of the held stage rolls back past 11, as it would have.
    f.apply().unwrap();
    let trial = trial_on_disk(&f);
    assert_eq!((trial.old.build, trial.new.build), (10, 12));
}

/// A replacement whose rename failed after its intent was written keeps the trial too: the
/// next locked look abandons the intent and puts the displaced trial back.
#[test]
fn an_abandoned_install_intent_puts_the_displaced_trial_back() {
    let mut f = Fixture::new();
    f.apply().unwrap();
    assert!(session_start(&f, minute(0)));
    assert_eq!(session_confirm(&f), Confirmed::Yes);
    assert!(window_start(&f, minute(1)));
    let before = trial_on_disk(&f);
    stage_release(&mut f, 12, 3, 'c');
    let pins = Pins {
        masters: &[&f.master],
    };
    let failed = apply_with_checkpoints(
        &f.context,
        &mut f.state,
        &f.proof,
        pins,
        |_, _, _| Ok(()),
        |at| {
            if at == Checkpoint::Prepared {
                Err("replace Linux executable: Read-only file system (os error 30)".into())
            } else {
                Ok(())
            }
        },
    );
    assert!(failed.is_err());
    assert_eq!(
        trial_on_disk(&f).phase,
        Phase::Prepared,
        "the intent is durable"
    );
    // A running 0.98 still reads the intent with its displaced trial (and abandons it).
    let text = fs::read_to_string(f.context.dir.join("state.toml")).unwrap();
    assert!(text.contains("displaced"), "{text}");
    let _: v098::State = aterm_toml::from_str(&text).expect("0.98 reads the record");
    let mut state = f.context.read_state().unwrap().unwrap();
    recover(&f.context, &mut state).unwrap();
    let trial = trial_on_disk(&f);
    assert_eq!(
        (trial.old.build, trial.new.build, trial.phase, trial.starts),
        (10, 11, Phase::Installed, 1)
    );
    assert_eq!(
        (trial.healthy, trial.window_proven),
        (before.healthy, before.window_proven)
    );

    // Negative control: the install that lands replaces the trial, displaced and all.
    f.state = state;
    f.apply().unwrap();
    let trial = trial_on_disk(&f);
    assert_eq!((trial.old.build, trial.new.build), (10, 12));
    let text = fs::read_to_string(f.context.dir.join("state.toml")).unwrap();
    assert!(!text.contains("displaced"), "{text}");
}

/// A hand install zeroes the failure count with no check run: the loop's next pass, reading
/// the copy's own record, announces no recovery.
#[test]
fn a_manual_install_after_failing_checks_announces_no_recovery() {
    use crate::linux_notice::{Announcer, Pass, Record};
    let pass = |state: &State| {
        (
            state.outcome.clone(),
            state.failing_checks,
            state.updated_at.clone(),
        )
    };
    let tick = |notice: &mut Announcer,
                (outcome, failing_checks, updated_at): &(String, u32, String)| {
        notice.tick(Pass {
            outcome,
            failing_checks: *failing_checks,
            updated_at,
            record: Record::Kept,
            took_lock: true,
        })
    };
    let mut f = Fixture::new();
    hold_stage(&mut f);
    let mut notice = Announcer::new("2026-09-28T10:00:00Z".into());
    f.state.outcome = "machine roster signature refused".into();
    f.state.failing_checks = 3;
    f.state.failing_kind = "linux-update".into();
    f.state.updated_at = "2026-09-28T10:30:00Z".into();
    f.context.save(&f.state).unwrap();
    assert!(tick(&mut notice, &pass(&f.state)).is_some(), "announced");
    f.apply().unwrap();
    let installed = f.context.read_state().unwrap().unwrap();
    assert_eq!(installed.failing_checks, 0, "the install zeroes the count");
    assert_eq!(tick(&mut notice, &pass(&installed)), None, "no check ran");
}

/// A due pass that waits on the trial is no check: it reads no channel, moves no
/// `updated_at` and clears no streak, so after a hand install the loop announces no
/// recovery. Negative control: a check recorded since the announcement answers it.
#[test]
fn a_due_pass_that_waits_on_the_trial_announces_no_recovery() {
    use crate::linux_notice::{Announcer, Pass, Record};
    let tick = |notice: &mut Announcer,
                (status, record, took_lock): &(crate::UpdateStatus, Record, bool)| {
        notice.tick(Pass {
            outcome: &status.outcome,
            failing_checks: status.failing_checks,
            updated_at: &status.updated_at,
            record: *record,
            took_lock: *took_lock,
        })
    };
    let mut f = Fixture::new();
    hold_stage(&mut f);
    let failed = "2026-09-28T10:30:00Z";
    f.state.outcome = "machine roster signature refused".into();
    f.state.failing_checks = 3;
    f.state.failing_kind = "linux-update".into();
    f.state.updated_at = failed.into();
    f.context.save(&f.state).unwrap();
    let mut notice = Announcer::new("2026-09-28T10:00:00Z".into());
    let announced = (status_in(Ok(&f.context), 10), Record::Kept, true);
    assert!(tick(&mut notice, &announced).is_some(), "announced");
    f.apply().unwrap();

    let due = check_recording_in(Ok(&f.context), 11, &settings_provider(false), false);
    assert_eq!(
        (due.1, due.2),
        (Record::Kept, true),
        "the pass is on record"
    );
    let state = f.context.read_state().unwrap().unwrap();
    assert!(state.last_attempt_unix > 0, "the pass was due and ran");
    assert!(
        state.outcome.contains("update checks wait until then"),
        "{}",
        state.outcome
    );
    assert_eq!(state.updated_at, failed, "no check is on record");
    assert_eq!(state.check_build, 0, "nor the build that made one");
    assert_eq!(tick(&mut notice, &due), None, "no check ran");

    let mut checked = state;
    checked.updated_at = "2026-09-28T15:00:00Z".into();
    checked.outcome = "Up to date".into();
    f.context.save(&checked).unwrap();
    assert_eq!(
        tick(
            &mut notice,
            &(status_in(Ok(&f.context), 11), Record::Kept, true)
        ),
        Some((
            crate::HEALTH_RECOVERED_TITLE.to_string(),
            "manifest".to_string()
        ))
    );
}

/// A pass that failed before reaching the lock (unreadable settings) does not answer the
/// lock's notice; the pass that takes the lock once it is free does. Driven through the
/// loop's own pass (`check_recording_in`).
#[test]
fn a_pass_that_never_reached_the_lock_does_not_say_it_came_free() {
    use crate::linux_notice::{Announcer, Pass, Record};
    let tick = |notice: &mut Announcer,
                (status, record, took_lock): &(crate::UpdateStatus, Record, bool)| {
        notice.tick(Pass {
            outcome: &status.outcome,
            failing_checks: status.failing_checks,
            updated_at: &status.updated_at,
            record: *record,
            took_lock: *took_lock,
        })
    };
    let mut f = Fixture::new();
    // No check is due, so the pass that takes the lock reads no channel.
    f.state.check_owner = "alabsystems".into();
    f.state.check_repo = "aterm".into();
    f.state.next_check_unix = i64::MAX;
    f.context.save(&f.state).unwrap();
    let mut notice = Announcer::new("2026-09-28T10:00:00Z".into());
    let holder = another_aterm_holds_the_lock(&f);
    let busy = check_recording_in(Ok(&f.context), 10, &settings_provider(false), false);
    assert_eq!(
        (busy.1, busy.2),
        (Record::Busy, false),
        "{}",
        busy.0.outcome
    );
    let said = (0..120)
        .filter_map(|_| tick(&mut notice, &busy))
        .collect::<Vec<_>>();
    assert_eq!(said.len(), 1, "PRECONDITION: the lock's notice is said");

    let unreadable: crate::CheckSettingsProvider = std::sync::Arc::new(|| None);
    let stalled = check_recording_in(Ok(&f.context), 10, &unreadable, false);
    assert_eq!(
        (stalled.1, stalled.2),
        (Record::Lost, false),
        "a failure before the lock: {}",
        stalled.0.outcome
    );
    assert_eq!(
        tick(&mut notice, &stalled),
        None,
        "no recovery while the lock is held"
    );

    drop(holder);
    let freed = check_recording_in(Ok(&f.context), 10, &settings_provider(false), false);
    assert!(freed.2, "the pass took the lock: {}", freed.0.outcome);
    assert_eq!(
        tick(&mut notice, &freed),
        Some((
            crate::HEALTH_RECOVERED_TITLE.to_string(),
            "manifest".to_string()
        )),
        "the lock came free"
    );
}

/// Leave a full-size temp named as `stem`'s writer names it, as a process killed in
/// the middle of writing it does.
fn leave_temp(f: &Fixture, stem: &str) -> PathBuf {
    let path = f.context.dir.join(format!(
        "{stem}-{}",
        aterm_uds::rand::hex_token::<16>().unwrap()
    ));
    fs::write(&path, vec![7u8; 1 << 20]).unwrap();
    path
}

/// What a killed writer left in the update folder is removed by the next install, check or
/// rollback: every writer of those names holds the update lock while its file exists.
/// Negative control: names no writer makes, and the stage itself, stay.
#[test]
fn what_a_killed_writer_left_in_the_update_folder_is_removed() {
    let mut f = Fixture::new();
    let stems = [
        "download",
        "restore",
        "probe-output",
        "probe-error",
        "record",
        "bootstrap",
    ];
    let left: Vec<_> = stems.iter().map(|stem| leave_temp(&f, stem)).collect();
    let kept = [
        f.context.dir.join("download-notes"),
        f.context.dir.join(format!("download-{}", "A".repeat(32))),
        f.context.dir.join(format!("downloaded-{}", "a".repeat(32))),
    ];
    for path in &kept {
        fs::write(path, b"not a temp").unwrap();
    }
    f.apply().unwrap();
    for path in &left {
        assert!(!path.exists(), "{} remains", path.display());
    }
    for path in &kept {
        assert!(path.exists(), "{} was removed", path.display());
    }
}

/// A rollback copy that failed is removed by its writer ([`fresh_written`]), and the
/// rollback sweeps what a killed one left.
#[test]
fn a_failed_rollback_copy_is_removed() {
    let mut f = Fixture::new();
    let failed = fresh_written(&f.context.dir, "restore", |file, _| {
        file.write_all(&[7u8; 4096]).map_err(|e| e.to_string())?;
        Err("File too large (os error 27)".into())
    });
    assert_eq!(failed, Err("File too large (os error 27)".to_string()));
    let restores = |f: &Fixture| {
        fs::read_dir(&f.context.dir)
            .unwrap()
            .filter_map(|entry| entry.unwrap().file_name().into_string().ok())
            .filter(|name| name.starts_with("restore-"))
            .count()
    };
    assert_eq!(restores(&f), 0, "the failed writer removed its own temp");

    f.apply().unwrap();
    let partial = leave_temp(&f, "restore");
    let mut start = 0;
    while window_start(&f, minute(start)) {
        start += 1;
    }
    assert_eq!(f.context.read_state().unwrap().unwrap().installed.build, 10);
    assert!(
        !partial.exists(),
        "the earlier start's partial copy remains"
    );
    assert_eq!(restores(&f), 0);
}

/// A candidate the dynamic loader refuses (linked against a newer glibc) says why: the
/// probe's first stderr line is the reason, and a loader's refusal is named as one.
#[test]
fn a_candidate_the_loader_refuses_says_why() {
    let f = Fixture::new();
    let manifest = Manifest::parse(std::str::from_utf8(&f.proof.appcast).unwrap()).unwrap();
    let script = f.root.join("identity-probe");
    let probe = |body: &str| {
        fs::write(&script, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        probe_candidate(
            &script,
            &manifest,
            native().unwrap(),
            &f.context.dir,
            aterm_spec::wait::MUST_HAPPEN,
        )
        .unwrap_err()
    };
    let refused = probe(
        "echo \"$0: /lib/x86_64-linux-gnu/libc.so.6: version \\`GLIBC_2.99' not found \
         (required by $0)\" >&2\nexit 127",
    );
    assert_eq!(
        refused,
        "aterm 0.11.0 can\u{2019}t run on this system: /lib/x86_64-linux-gnu/libc.so.6: \
         version `GLIBC_2.99' not found"
    );
    assert!(!refused.contains(&*script.to_string_lossy()), "{refused}");
    // A missing symbol version that is not glibc's is a loader refusal too.
    let refused = probe(
        "echo \"$0: /usr/lib/libstdc++.so.6: version \\`CXXABI_1.3.15' not found \
         (required by $0)\" >&2\nexit 127",
    );
    assert_eq!(
        refused,
        "aterm 0.11.0 can\u{2019}t run on this system: /usr/lib/libstdc++.so.6: \
         version `CXXABI_1.3.15' not found"
    );
    // Any other failure keeps its status, and says what it said.
    let refused = probe("echo 'unknown command: identity' >&2\nexit 2");
    assert_eq!(
        refused,
        "aterm 0.11.0 failed when checked here (exit status: 2): unknown command: identity"
    );
    let refused = probe("exit 3");
    assert_eq!(
        refused,
        "aterm 0.11.0 failed when checked here (exit status: 3)"
    );
    assert!(fs::read_dir(&f.context.dir).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("probe-")
    }));
}

/// A candidate whose identity probe failed is still the release's bytes: the next check of
/// that release probes it again instead of downloading it (about 100 MB). Negative control:
/// a candidate that is not the release's bytes is downloaded again.
#[test]
fn a_release_whose_probe_failed_is_not_downloaded_again() {
    let mut f = Fixture::new();
    let provider = settings_provider(true);
    let source = provider().unwrap().source;
    let pins = Pins {
        masters: &[&f.master],
    };
    let manifest = Manifest::parse(std::str::from_utf8(&f.proof.appcast).unwrap()).unwrap();
    let refused = finish_candidate(
        &f.context,
        &mut f.state,
        &f.proof,
        pins,
        &source,
        &provider,
        |_, manifest, _| {
            Err(format!(
                "aterm {} can\u{2019}t run on this system: version `GLIBC_2.99' not found",
                manifest.version
            ))
        },
    );
    assert!(refused.is_err());
    assert!(f.state.staged.is_none(), "nothing is staged");
    assert!(reusable_stage(
        &f.context,
        &f.state,
        &manifest,
        native().unwrap()
    ));
    // The system can run it now: the next check installs the bytes already here.
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
    assert_eq!(f.state.installed.build, 11);

    let g = Fixture::new();
    let mut bytes = fs::read(g.context.dir.join("candidate")).unwrap();
    bytes[100] ^= 1;
    fs::write(g.context.dir.join("candidate"), bytes).unwrap();
    assert!(!reusable_stage(
        &g.context,
        &g.state,
        &manifest,
        native().unwrap()
    ));
}
