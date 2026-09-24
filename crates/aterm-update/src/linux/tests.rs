// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use ring::signature::{Ed25519KeyPair, KeyPair};

struct Fixture {
    root: PathBuf,
    context: Context,
    state: State,
    proof: Proof,
    channel: String,
}

fn key(seed: u8) -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap()
}

fn public(key: &Ed25519KeyPair) -> String {
    aterm_codec::base64::encode(key.public_key().as_ref()).unwrap()
}

fn catalog(tag: &str, native: bool) -> aterm_update_core::release_catalog::ReleaseCandidate {
    aterm_update_core::release_catalog::ReleaseCandidate {
        tag: tag.into(),
        current_appcast: true,
        linux_aarch64: native,
        linux_x86_64: native,
    }
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
            masters: &[],
            channels: &[&f.channel],
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
            masters: &[],
            channels: &[&automatic.channel],
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
                masters: &[],
                channels: &[&f.channel],
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
                masters: &[],
                channels: &[&f.channel]
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
            masters: &[],
            channels: &[&f.channel],
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
            masters: &[],
            channels: &[&f.channel],
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
        masters: &[],
        channels: &[&f.channel],
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
            "schema = 1\nversion = \"0.11.0\"\nbuild_number = 11\ncommit = \"{}\"\ndmg = \"aterm-0.11.0.dmg\"\nsha256 = \"{}\"\nlinux_{arch} = \"aterm-0.11.0-linux-{arch}\"\nlinux_{arch}_sha256 = \"{digest}\"\nlinux_{arch}_size = 128\n",
            "b".repeat(40),
            "0".repeat(64)
        );
        let proof = Proof {
            policy: None,
            appcast: text.into_bytes(),
            signature: Vec::new(),
            roster: Vec::new(),
            roster_signature: Vec::new(),
        };
        let mut this = Self {
            root,
            context,
            state,
            proof,
            channel: public(&key(7)),
        };
        this.resign();
        this
    }

    fn resign(&mut self) {
        self.proof.signature = key(7).sign(&self.proof.appcast).as_ref().to_vec();
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
                masters: &[],
                channels: &[&self.channel],
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
        masters: &[],
        channels: &[&f.channel],
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
        masters: &[],
        channels: &[&f.channel],
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
        masters: &[],
        channels: &[&f.channel],
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
        masters: &[],
        channels: &[&f.channel],
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
    rollback_locked(&f.context, &mut f.state).unwrap();
    assert_eq!(hash_file(&f.context.target).unwrap(), old_hash);
    assert_eq!(
        f.state.high_water, 11,
        "repair is not permission to replay the failed build"
    );
    assert_eq!(f.state.rejected_build, 11);
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
            "key" => f.channel = public(&key(9)),
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
                masters: &[],
                channels: &[&f.channel],
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

#[test]
fn admitted_roster_revocation_is_durable_even_when_it_refuses_the_appcast() {
    let mut f = Fixture::new();
    f.alter_manifest(
        "schema = 1",
        "schema = 1\nmachine_id = \"fixture\"\nroster_seq = 2",
    );
    let master = key(11);
    let anchor = public(&master);
    let mut roster = aterm_update_core::Roster {
        schema: 1,
        roster_seq: 2,
        valid_until: "2099-01-01T00:00:00Z".into(),
        machines: vec![aterm_update_core::Machine {
            id: "fixture".into(),
            pubkey: f.channel.clone(),
            added_at: String::new(),
            not_after: None,
        }],
        revoked: Vec::new(),
    };
    f.proof.roster = roster.to_toml().unwrap().into_bytes();
    f.proof.roster_signature = master.sign(&f.proof.roster).as_ref().to_vec();
    let pins = Pins {
        masters: &[&anchor],
        channels: &[],
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

#[test]
fn actual_boot_and_health_transitions_bind_build_commit_and_process_inode() {
    let mut f = Fixture::new();
    let old = File::open(&f.context.target).unwrap().metadata().unwrap();
    f.apply().unwrap();
    boot_locked(
        &f.context,
        &mut f.state,
        10,
        &"a".repeat(40),
        (old.dev(), old.ino()),
    )
    .unwrap();
    assert_eq!(
        f.state.trial.as_ref().unwrap().starts,
        0,
        "old live process has no new-build trial"
    );
    let actual = File::open(&f.context.target).unwrap().metadata().unwrap();
    boot_locked(
        &f.context,
        &mut f.state,
        11,
        &"b".repeat(40),
        (actual.dev(), actual.ino()),
    )
    .unwrap();
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
    for _ in 0..5 {
        boot_locked(
            &f.context,
            &mut f.state,
            11,
            &"b".repeat(40),
            (actual.dev(), actual.ino()),
        )
        .unwrap();
    }
    assert!(f.state.trial.as_ref().unwrap().healthy);
    assert_eq!(f.state.installed.build, 11);

    let mut failing = Fixture::new();
    let baseline = failing.state.installed.sha256.clone();
    failing.apply().unwrap();
    let failed_inode = fs::metadata(&failing.context.target).unwrap();
    for _ in 0..MAX_TRIAL_STARTS {
        boot_locked(
            &failing.context,
            &mut failing.state,
            11,
            &"b".repeat(40),
            (failed_inode.dev(), failed_inode.ino()),
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
    )
    .unwrap();
    assert_eq!(failing.state.installed.build, 10);
    assert_eq!(hash_file(&failing.context.target).unwrap(), baseline);
    assert_eq!(failing.state.high_water, 11);
    assert_eq!(failing.state.rejected_build, 11);
}

#[test]
fn newest_mac_only_release_does_not_hide_an_older_authenticated_native_artifact() {
    let mut f = Fixture::new();
    let candidate = f.proof.clone();
    let head_text = String::from_utf8(candidate.appcast.clone())
        .unwrap()
        .lines()
        .filter(|line| !line.starts_with("linux_"))
        .collect::<Vec<_>>()
        .join("\n")
        .replace("0.11.0", "0.12.0")
        .replace("build_number = 11", "build_number = 12");
    let mut head = candidate.clone();
    head.appcast = head_text.into_bytes();
    head.signature = key(7).sign(&head.appcast).as_ref().to_vec();
    let pins = Pins {
        masters: &[],
        channels: &[&f.channel],
    };
    let policy = authenticate_tag(&f.context, &mut f.state, "v0.12.0", &head, pins).unwrap();
    assert!(policy.linux_artifact(native().unwrap()).unwrap().is_none());
    let (tag, elected, _) = older_native_candidate(
        &f.context,
        &mut f.state,
        "v0.12.0",
        &head,
        &[
            catalog("v0.12.0", false),
            catalog("v0.11.5", false),
            catalog("v0.11.0", true),
        ],
        pins,
        |candidate_location, _| {
            assert_eq!(candidate_location.tag, "v0.11.0");
            Ok(candidate.clone())
        },
    )
    .unwrap();
    assert_eq!(tag, "v0.11.0");
    assert_eq!(elected.build_number, 11);
    let bad = Proof {
        signature: vec![0; 64],
        ..candidate
    };
    assert!(
        older_native_candidate(
            &f.context,
            &mut f.state,
            "v0.12.0",
            &head,
            &[catalog("v0.11.0", true)],
            pins,
            |_, _| Ok(bad.clone())
        )
        .is_err()
    );
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
                masters: &[],
                channels: &[&f.channel]
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
    assert!(
        older_native_candidate(
            &f.context,
            &mut f.state,
            "v0.12.0",
            &f.proof,
            &[catalog("v0.11.0", false)],
            Pins {
                masters: &[],
                channels: &[&f.channel]
            },
            |_, _| panic!("Mac-only inventory must not authenticate retired unrelated history")
        )
        .err()
        .unwrap()
        .contains("no authenticated")
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

#[test]
fn archived_native_manifest_is_elected_under_current_policy_and_normalized_for_storage() {
    let mut f = Fixture::new();
    let mut archived = catalog("v0.11.0", true);
    archived.current_appcast = false;
    assert_eq!(archived.appcast_name(), "aterm-appcast-v0.11.0.toml");
    let old_proof = f.proof.clone();
    let head_text = String::from_utf8(old_proof.appcast.clone())
        .unwrap()
        .replace("0.11.0", "0.12.0")
        .replace("build_number = 11", "build_number = 12");
    let head_bytes = head_text.into_bytes();
    let head = Proof {
        appcast: head_bytes.clone(),
        signature: key(7).sign(&head_bytes).as_ref().to_vec(),
        ..old_proof.clone()
    };
    let (_, manifest, proof) = older_native_candidate(
        &f.context,
        &mut f.state,
        "v0.12.0",
        &head,
        &[archived],
        Pins {
            masters: &[],
            channels: &[&f.channel],
        },
        |location, policy| {
            assert!(!location.current_appcast);
            assert_eq!(location.appcast_name(), "aterm-appcast-v0.11.0.toml");
            Ok(Proof {
                policy: Some((policy.appcast.clone(), policy.signature.clone())),
                ..old_proof.clone()
            })
        },
    )
    .unwrap();
    assert_eq!(manifest.build_number, 11);
    proof.save(&f.context.dir).unwrap();
    assert_eq!(
        read_bounded(&f.context.dir.join(APPCAST), APPCAST_LIMIT).unwrap(),
        old_proof.appcast
    );
    assert_eq!(
        read_bounded(&f.context.dir.join(POLICY), APPCAST_LIMIT).unwrap(),
        head.appcast
    );
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
            masters: &[],
            channels: &[&f.channel],
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
