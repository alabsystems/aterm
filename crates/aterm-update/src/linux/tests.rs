// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use ring::signature::{Ed25519KeyPair, KeyPair};

#[test]
fn required_linux_directory_flushes_reject_refusals_and_genuine_failures() {
    use aterm_durable::seam::FailingDirs;
    let fixture = Fixture::new();
    for errno in [libc::ENOTSUP, libc::EIO] {
        let _failure = FailingDirs::with(errno);
        assert!(sync_dir(&fixture.root).is_err());
        assert!(ordered_dir(&fixture.root).is_err());
    }
}

struct Fixture {
    root: PathBuf,
    context: Context,
    state: State,
    proof: Proof,
    /// The paper master the fixture's roster is signed by, the one anchor: an appcast
    /// verifies only through a roster this key signed.
    master: String,
}

fn machine() -> Ed25519KeyPair {
    key(7)
}

fn master() -> Ed25519KeyPair {
    key(11)
}

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
fn a_request_over_unreadable_settings_has_no_apply_authority() {
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
fn manual_staging_never_bypasses_authentication() {
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
    for (min_build, high_water, rejected_build, admitted) in
        [(11, 10, 0, true), (0, 11, 0, false), (0, 10, 11, false)]
    {
        (
            f.state.min_build,
            f.state.high_water,
            f.state.rejected_build,
        ) = (min_build, high_water, rejected_build);
        assert_eq!(
            reusable_stage(&f.context, &f.state, &manifest, native().unwrap()),
            admitted,
            "min_build {min_build}, high_water {high_water}, rejected {rejected_build}"
        );
    }
    (
        f.state.min_build,
        f.state.high_water,
        f.state.rejected_build,
    ) = (0, 10, 0);
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

fn fixture_parent() -> PathBuf {
    use std::os::unix::fs::DirBuilderExt;
    let beside_build = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent()?.parent().map(Path::to_path_buf));
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut refused = Vec::new();
    for base in beside_build.into_iter().chain(home) {
        let parent = base.join(".aterm-linux-update-tests");
        let made = fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&parent)
            .map_err(|e| e.to_string())
            .and_then(|()| ancestry::refusal(&parent));
        match made {
            Ok(()) => return parent,
            Err(why) => refused.push(format!("{}: {why}", parent.display())),
        }
    }
    panic!("no private fixture parent: {}", refused.join("; "));
}

impl Fixture {
    fn new() -> Self {
        let root = fixture_parent().join(aterm_uds::rand::hex_token::<16>().unwrap());
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
            rejected_unstarted: false,
            kept_unproven: None,
            unsaid_trial_ends: Vec::new(),
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
    let probe = |path: &Path, bound: Duration| {
        probe_candidate(path, &manifest, native().unwrap(), &f.context.dir, bound)
    };
    let write_script = |body: &str| {
        fs::write(&script, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    };
    write_script(&format!(
        "test \"$1 $2\" = 'update identity' || exit 9\nprintf '%s\\n' '{}'",
        aterm_json::to_string(&good).unwrap()
    ));
    probe(&script, aterm_spec::wait::MUST_HAPPEN).unwrap();
    let mut bad = good;
    bad.build_number += 1;
    let other_identity = format!("printf '%s\\n' '{}'", aterm_json::to_string(&bad).unwrap());
    for (body, says) in [
        ("exit 3", None),
        ("printf 'not json'", None),
        (other_identity.as_str(), None),
        (
            "echo \"$0: /lib/x86_64-linux-gnu/libc.so.6: version \\`GLIBC_2.99' not found \
             (required by $0)\" >&2\nexit 127",
            Some("GLIBC_2.99"),
        ),
        (
            "echo \"$0: /usr/lib/libstdc++.so.6: version \\`CXXABI_1.3.15' not found \
             (required by $0)\" >&2\nexit 127",
            Some("CXXABI_1.3.15"),
        ),
        (
            "echo 'unknown command: identity' >&2\nexit 2",
            Some("unknown command: identity"),
        ),
    ] {
        write_script(body);
        let refused = probe(&script, aterm_spec::wait::MUST_HAPPEN).unwrap_err();
        assert!(
            says.is_none_or(|says| refused.contains(says)),
            "{body}: {refused}"
        );
    }
    write_script("while :; do :; done");
    probe(&script, Duration::from_millis(50)).expect_err("a probe that never answers");
    probe(&f.context.dir.join("candidate"), Duration::from_secs(1))
        .expect_err("synthetic signed-header ELF cannot satisfy the real loader probe");
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
fn a_first_bootstrap_orders_its_candidate_and_keeps_its_records_durable() {
    use aterm_durable::{Flush, seam::Recording};
    const O: Flush = Flush::Ordered;
    const D: Flush = Flush::Durable;
    let f = Fixture::new();
    let download = f.root.join("download");
    fs::copy(f.context.dir.join("candidate"), &download).unwrap();
    fs::set_permissions(&download, fs::Permissions::from_mode(0o755)).unwrap();
    fs::remove_file(&f.context.target).unwrap();
    fs::remove_file(f.context.dir.join("state.toml")).unwrap();
    let pins = Pins {
        masters: &[&f.master],
    };
    let recording = Recording::start();
    install_locked(&f.context, &download, &f.proof, pins, |_, _, _| Ok(())).unwrap();
    assert_eq!(
        recording.take(),
        [D, D, D, D, O, O, D, D, D, D, D, D, D, D, O, D, D, D, D, D,]
    );
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
    f.apply().unwrap();
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
fn a_linux_apply_and_rollback_order_their_steps_and_keep_the_state_durable() {
    use aterm_durable::{Flush, seam::Recording};
    const O: Flush = Flush::Ordered;
    const D: Flush = Flush::Durable;
    let mut f = Fixture::new();
    let recording = Recording::start();
    f.apply().unwrap();
    assert_eq!(
        recording.take(),
        [D, D, D, D, O, O, O, D, D, O, D, D],
        "authorize's two state saves (the roster observed, the floor ratcheted), the \
         rollback copy and its entry, the candidate, Prepared saved, the replacing \
         rename, Installed saved"
    );
    rollback_locked(&f.context, &mut f.state).unwrap();
    assert_eq!(
        recording.take(),
        [O, D, D, O, D, D],
        "the restored copy, RollbackPrepared saved, its rename into place, the rollback saved"
    );
}

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

#[test]
fn a_build_that_pins_no_paper_master_verifies_no_update_channel() {
    let mut f = Fixture::new();
    authorize(&f.context, &mut f.state, &f.proof, Pins { masters: &[] })
        .expect_err("no anchor, no channel");
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
    roster.revoked.clear();
    f.proof.roster = roster.to_toml().unwrap().into_bytes();
    f.proof.roster_signature = master.sign(&f.proof.roster).as_ref().to_vec();
    assert!(authorize(&f.context, &mut f.state, &f.proof, pins).is_err());
}

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
    assert!(f.state.trial.as_ref().unwrap().healthy);
    assert_eq!(f.state.installed.build, 11);
}

fn trial_on_disk(f: &Fixture) -> Trial {
    f.context
        .read_state()
        .unwrap()
        .unwrap()
        .trial
        .expect("a trial")
}

fn reads(now: impl Into<StartClock>) -> impl FnOnce() -> Result<StartClock, String> {
    let now = now.into();
    move || Ok(now)
}

fn session_start(f: &Fixture, now: impl Into<StartClock>) -> bool {
    start_in(f, Lane::Session, reads(now))
}

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

fn session_confirm(f: &Fixture) -> Confirmed {
    let inode = fs::metadata(&f.context.target).unwrap();
    confirm_session_in(&f.context, 11, &"b".repeat(40), || {
        Ok((inode.dev(), inode.ino()))
    })
    .unwrap()
}

fn window_start(f: &Fixture, now: impl Into<StartClock>) -> bool {
    window_start_as(f, 11, 'b', now)
}

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

#[test]
fn a_session_lane_install_is_counted_and_confirmed_by_a_session() {
    let mut f = Fixture::new();
    f.apply().unwrap();
    awaiting_start(&f.state).expect("the install waits for a start");
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
}

#[test]
fn a_silent_session_still_up_buys_its_burst_one_more_window() {
    let t0 = minute(0);
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
    assert!(!session_defer(&f, &first, t0 + 10));
    assert!(session_defer(&f, &own, t0 + 11));
    assert_eq!(trial_on_disk(&f).start_clock(), StartClock::from(t0 + 11));
    assert!(!session_defer(&f, &own, t0 + 12), "once");
    assert!(!trial_on_disk(&f).healthy, "the move confirms nothing");
    assert!(
        session_start(&f, t0 + 21),
        "no rollback for one more window"
    );
    assert_eq!(f.context.read_state().unwrap().unwrap().installed.build, 11);
    assert!(!session_start(&f, t0 + 31));
    let state = f.context.read_state().unwrap().unwrap();
    assert_eq!((state.installed.build, state.rejected_build), (10, 11));

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

#[test]
fn a_silent_session_s_deferral_outlasts_a_held_update_lock() {
    let t0 = minute(0);
    let commit = "b".repeat(40);
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

    assert!(!defer_session_verdict_retrying(
        &own,
        || Err(LOCK_BUSY.to_string()),
        || Ok(StartClock::from(
            t0 + 1 + crate::linux_trial::START_SETTLES_SECS
        )),
        |_| panic!("no pause once the stamp has settled"),
    ));
    assert!(!defer_session_verdict_retrying(
        &own,
        || Err(LOCK_BUSY.to_string()),
        || Err("no clock".to_string()),
        |_| panic!("no pause without a clock"),
    ));
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

fn rollback_copies(f: &Fixture) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(&f.context.dir)
        .unwrap()
        .filter_map(|entry| entry.unwrap().file_name().into_string().ok())
        .filter(|name| name.starts_with("rollback-"))
        .collect();
    names.sort();
    names
}

fn spoil_kept_copy(f: &Fixture, identity: &Identity, way: &str) {
    let copy = f.context.backup(identity);
    match way {
        "gone" => fs::remove_file(copy).unwrap(),
        "changed" => fs::write(copy, elf(9, native().unwrap())).unwrap(),
        mode => {
            let mode = u32::from_str_radix(mode.trim_start_matches("0o"), 8).unwrap();
            fs::set_permissions(copy, fs::Permissions::from_mode(mode)).unwrap();
        }
    }
}

const SPOILED_COPIES: [&str; 6] = ["gone", "changed", "0o644", "0o775", "0o000", "0o111"];

#[test]
fn a_trial_no_rollback_can_be_taken_from_ends_and_the_copy_updates_again() {
    for way in std::iter::once("yanked").chain(SPOILED_COPIES) {
        let mut f = Fixture::new();
        f.state.installed.machine_id = Some("old-machine".into());
        let ten = f.state.installed.clone();
        f.apply().unwrap();
        if way == "yanked" {
            let mut state = f.context.read_state().unwrap().unwrap();
            state.min_build = 11;
            f.context.save(&state).unwrap();
        } else {
            spoil_kept_copy(&f, &ten, way);
        }
        for n in 0..MAX_TRIAL_STARTS {
            assert!(window_start(&f, minute(i64::from(n))), "{way}: start {n}");
        }
        assert!(!window_start(&f, minute(i64::from(MAX_TRIAL_STARTS))));
        let state = f.context.read_state().unwrap().unwrap();
        assert!(state.trial.is_none(), "{way}: the trial is over");
        assert_eq!(state.installed.build, 11, "{way}: the build stays");
        assert_eq!(state.rejected_build, 0, "{way}: and is not refused");
        assert_eq!(
            state.kept_unproven,
            Some(11),
            "{way}: the record keeps why it stays"
        );
        assert!(
            matches!(state.unsaid_trial_ends.as_slice(), [words]
                if words.contains("0.11.0") && words.contains("stays")),
            "{way}: and keeps the words a window is told: {:?}",
            state.unsaid_trial_ends
        );
        assert!(
            awaiting_start(&state).is_none(),
            "{way}: nothing waits on it"
        );
        assert!(
            !window_start(&f, minute(10)),
            "{way}: no start counts any more"
        );
        spoil_kept_copy(&f, &state.installed, "changed");
        stage_release(&mut f, 12, 3, 'c');
        f.apply()
            .unwrap_or_else(|error| panic!("{way}: a newer release installs: {error}"));
        assert_eq!(f.state.installed.build, 12);
        let eleven = trial_on_disk(&f).old;
        assert_eq!(eleven.build, 11);
        assert_eq!(
            hash_file(&f.context.backup(&eleven)).unwrap(),
            eleven.sha256,
            "{way}: the stale copy under 11's name is replaced"
        );
    }
}

#[test]
fn a_kept_copy_no_rollback_could_take_is_never_inherited() {
    for way in SPOILED_COPIES {
        let mut g = Fixture::new();
        let ten = g.state.installed.clone();
        g.apply().unwrap();
        assert!(session_start(&g, minute(0)));
        assert_eq!(session_confirm(&g), Confirmed::Yes);
        assert!(window_start(&g, minute(1)), "a window of 11 crashes");
        spoil_kept_copy(&g, &ten, way);
        stage_release(&mut g, 12, 3, 'c');
        g.apply()
            .unwrap_or_else(|error| panic!("{way}: a newer release installs: {error}"));
        assert_eq!(trial_on_disk(&g).old.build, 11, "{way}: not inherited");
    }
}

#[test]
fn a_rotated_signers_build_still_comes_back_from_a_crash_loop() {
    for manual in [false, true] {
        let mut f = Fixture::new();
        f.state.installed.machine_id = Some("m3".into());
        let old = f.state.installed.clone();
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

fn save_as_098(f: &Fixture) {
    let path = f.context.dir.join("state.toml");
    let text = String::from_utf8(read_small(&path).unwrap()).unwrap();
    let mut older: v098::State = aterm_toml::from_str(&text).unwrap();
    older.next_check_unix += 1800;
    write_atomic(&path, aterm_toml::to_string(&older).unwrap().as_bytes()).unwrap();
}

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

#[test]
fn a_headless_instance_confirms_the_session_lane_only() {
    assert_eq!(confirm_lane(true), Lane::Session);
    assert_eq!(confirm_lane(false), Lane::Window);
    let mut f = Fixture::new();
    f.apply().unwrap();
    assert!(session_start(&f, minute(0)));
    assert_eq!(session_confirm(&f), Confirmed::Yes);
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
}

#[test]
fn a_terminal_only_copy_moves_its_rollback_target_on() {
    let mut g = Fixture::new();
    g.apply().unwrap();
    assert!(session_start(&g, minute(0)));
    assert_eq!(session_confirm(&g), Confirmed::Yes);
    let eleven = trial_on_disk(&g).new;
    stage_release(&mut g, 12, 3, 'c');
    g.apply().unwrap();
    assert_eq!(trial_on_disk(&g).old.build, 11);
    assert_eq!(
        rollback_copies(&g),
        vec![format!("rollback-{}", eleven.sha256)]
    );
}

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

#[test]
fn an_older_builds_save_mid_burst_still_rolls_a_crash_loop_back() {
    let t0 = minute(0);
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

fn clock_at(unix: i64, boot: &str, secs: i64) -> StartClock {
    StartClock {
        unix,
        steady: Some(Steady {
            clock: format!("{boot} time:[4026531834]"),
            secs,
        }),
    }
}

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
        assert!(
            session_start(&f, clock_at(t0 + step, "boot-a", 500)),
            "step {step}: the burst's next start is a try"
        );
        assert_eq!(session_confirm(&f), Confirmed::Yes, "step {step}");
        let state = f.context.read_state().unwrap().unwrap();
        assert_eq!(state.installed.build, 11, "step {step}: {}", state.outcome);
        assert_eq!(state.rejected_build, 0, "step {step}");
    }
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

fn the_lock_is_held(f: &Fixture) -> bool {
    FileLock::acquire_within(&f.context.dir.join("lock"), Duration::ZERO)
        .is_err_and(|error| error.kind() == std::io::ErrorKind::TimedOut)
}

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
        assert!(
            start_in(&f, lane, monotonic(&f, &steady, t0)),
            "{lane:?}: the burst's fourth start is a try"
        );
        let state = f.context.read_state().unwrap().unwrap();
        assert_eq!(state.installed.build, 11, "{lane:?}: {}", state.outcome);
        assert_eq!(state.rejected_build, 0, "{lane:?}");

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
}

#[test]
fn the_steady_clock_names_its_boot_and_never_runs_back() {
    let f = Fixture::new();
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
    for damaged in [
        "00000000-0000-0000-0000-000000000000\n",
        "boot-a\n",
        "0f5a1c6e-3b0e-4c55-9d4e-6c1f2a7b9e1\n",
    ] {
        fs::write(&boot, damaged).unwrap();
        assert_eq!(steady_clock(&boot, &namespace), None, "{damaged:?}");
    }
    fs::write(&boot, "0F5A1C6E-3B0E-4C55-9D4E-6C1F2A7B9E10").unwrap();
    assert_eq!(
        steady_clock(&boot, &namespace).map(|s| s.clock),
        Some("0F5A1C6E-3B0E-4C55-9D4E-6C1F2A7B9E10 time:[4026531834]".to_string()),
        "CONTROL"
    );
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
}

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

#[test]
fn an_older_builds_save_in_a_burst_does_not_roll_a_healthy_build_back() {
    let t0 = minute(0);
    for past in [1, 2, 3] {
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

    let mut thiefs = f.proof.clone();
    (thiefs.roster, thiefs.roster_signature) =
        roster_with(2, &[("fixture", &machine()), ("thief", &thief)], &["thief"]);
    authorize(&f.context, &mut f.state, &thiefs, pins).expect_err("the thief is revoked");
    assert_eq!(f.state.min_build, 0);
    assert_eq!(f.context.read_state().unwrap().unwrap().min_build, 0);

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

fn with_head(f: &mut Fixture, build: &str, signed: bool) {
    let head = String::from_utf8(f.proof.appcast.clone())
        .unwrap()
        .replace("build_number = 11", build)
        .into_bytes();
    let signature = if signed {
        machine().sign(&head).as_ref().to_vec()
    } else {
        vec![0; 64]
    };
    f.proof.policy = Some((head, signature));
}

#[test]
fn every_refusal_of_the_chain_refuses_the_linux_release() {
    let anchor = public(&master());
    let pins = Pins {
        masters: &[&anchor],
    };
    type Row = (&'static str, fn(&mut Fixture), bool);
    let rows: [Row; 7] = [
        ("as made", |_| {}, true),
        (
            "a roster below the durable floor",
            |f| f.state.roster_floor = 2,
            false,
        ),
        (
            "attributed to another machine",
            |f| f.alter_manifest("machine_id = \"fixture\"", "machine_id = \"other\""),
            false,
        ),
        (
            "ahead of its roster",
            |f| f.alter_manifest("roster_seq = 1", "roster_seq = 2"),
            false,
        ),
        (
            "a head at its build",
            |f| with_head(f, "build_number = 11", true),
            true,
        ),
        (
            "a head older than it",
            |f| with_head(f, "build_number = 10", true),
            false,
        ),
        (
            "a forged head",
            |f| with_head(f, "build_number = 11", false),
            false,
        ),
    ];
    for (name, edit, authorized) in rows {
        let mut f = Fixture::new();
        edit(&mut f);
        assert_eq!(
            authorize(&f.context, &mut f.state, &f.proof, pins).is_ok(),
            authorized,
            "{name}"
        );
    }
}

#[test]
fn a_held_stage_is_proved_again_under_the_roster_the_head_s_check_admitted() {
    let anchor = public(&master());
    let pins = Pins {
        masters: &[&anchor],
    };
    let provider = settings_provider(false);
    let source = provider().unwrap().source;
    let (thief, joined) = (key(13), key(17));
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

    for exit in ["accepted", "stolen", "mistagged"] {
        let mut f = Fixture::new();
        stage(&mut f);
        let mut head = f.proof.clone();
        let tag = if exit == "stolen" {
            (head.roster, head.roster_signature) =
                roster_with(2, &[("fixture", &machine())], &["thief"]);
            head.appcast = String::from_utf8(head.appcast.clone())
                .unwrap()
                .replace("machine_id = \"fixture\"", "machine_id = \"thief\"")
                .replace("roster_seq = 1", "roster_seq = 2")
                .replace("build_number = 11", "build_number = 12")
                .into_bytes();
            head.signature = thief.sign(&head.appcast).as_ref().to_vec();
            "v0.12.0"
        } else {
            (head.roster, head.roster_signature) =
                roster_with(2, &[("fixture", &machine()), ("joined", &joined)], &[]);
            if exit == "accepted" {
                "v0.11.0"
            } else {
                "v0.99.0"
            }
        };
        let judged = authenticate_head(&f.context, &mut f.state, tag, &head, pins);
        assert_eq!(judged.is_ok(), exit == "accepted", "{exit}: {judged:?}");
        assert_eq!(
            f.state.roster_floor, 2,
            "{exit}: the head's roster was admitted"
        );
        f.proof = Proof::read(&f.context.dir).unwrap();
        assert_eq!(
            f.proof.roster, head.roster,
            "{exit}: the stage carries roster 2"
        );
        f.apply()
            .unwrap_or_else(|error| panic!("{exit}: the held stage applies: {error}"));
        assert_eq!(f.state.installed.build, 11);
    }

    for control in ["forged", "revoking"] {
        let mut n = Fixture::new();
        stage(&mut n);
        let before = Proof::read(&n.context.dir).unwrap().roster;
        let mut head = n.proof.clone();
        if control == "forged" {
            (head.roster, _) = roster_with(2, &[("fixture", &machine())], &[]);
        } else {
            (head.roster, head.roster_signature) =
                roster_with(2, &[("joined", &joined)], &["fixture"]);
        }
        let judged = authenticate_head(&n.context, &mut n.state, "v0.11.0", &head, pins);
        assert!(judged.is_err(), "{control}");
        let floor = if control == "forged" { 1 } else { 2 };
        assert_eq!(n.state.roster_floor, floor, "{control}");
        assert_eq!(
            Proof::read(&n.context.dir).unwrap().roster,
            before,
            "{control}"
        );
    }
}

fn hashes_in<T>(run: impl FnOnce() -> T) -> (T, u32) {
    let before = HASHED.with(std::cell::Cell::get);
    let value = run();
    (value, HASHED.with(std::cell::Cell::get) - before)
}

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
    fs::set_permissions(&replacement, fs::Permissions::from_mode(0o755)).unwrap();
    fs::rename(&replacement, &candidate).unwrap();
    let (offered, hashed) = hashes_in(|| linux_status(&f.context, &f.state).staged_build);
    assert_eq!((offered, hashed), (Some(11), 1), "replaced: hashed again");
    fs::remove_file(&candidate).unwrap();
    assert_eq!(linux_status(&f.context, &f.state).staged_build, None);
}

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
}

fn another_aterm_holds_the_lock(f: &Fixture) -> aterm_update_core::FileLock {
    aterm_update_core::FileLock::acquire(&f.context.dir.join("lock")).unwrap()
}

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
                vetoed.store(true, Ordering::SeqCst);
            }
            Ok(())
        },
    )
    .unwrap();
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
    f.apply().unwrap();
    let trial = trial_on_disk(&f);
    assert_eq!((trial.old.build, trial.new.build), (10, 12));
}

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

    f.state = state;
    f.apply().unwrap();
    let trial = trial_on_disk(&f);
    assert_eq!((trial.old.build, trial.new.build), (10, 12));
    let text = fs::read_to_string(f.context.dir.join("state.toml")).unwrap();
    assert!(!text.contains("displaced"), "{text}");
}

#[test]
fn a_due_pass_that_waits_on_the_trial_records_no_check() {
    use crate::linux_notice::Record;
    let mut f = Fixture::new();
    hold_stage(&mut f);
    let failed = "2026-09-28T10:30:00Z";
    f.state.outcome = "machine roster signature refused".into();
    f.state.failing_checks = 3;
    f.state.failing_kind = "linux-update".into();
    f.state.updated_at = failed.into();
    f.context.save(&f.state).unwrap();
    f.apply().unwrap();

    let due = check_recording_in(Ok(&f.context), 11, &settings_provider(false), false);
    assert_eq!(
        (due.1, due.2),
        (Record::Kept, true),
        "the pass is on record"
    );
    let state = f.context.read_state().unwrap().unwrap();
    assert!(state.last_attempt_unix > 0, "the pass was due and ran");
    assert_eq!(state.updated_at, failed, "no check is on record");
    assert_eq!(state.check_build, 0, "nor the build that made one");
}

#[test]
fn a_trial_start_made_while_a_check_holds_the_lock_counts_nothing() {
    let mut f = Fixture::new();
    f.apply().unwrap();
    let inode = fs::metadata(&f.context.target).unwrap();
    let actual = || Ok((inode.dev(), inode.ino()));
    let commit = "b".repeat(40);
    let busy = |result: Result<bool, String>| result.is_err_and(|error| error == LOCK_BUSY);
    let check = another_aterm_holds_the_lock(&f);
    assert!(
        busy(boot_in(&f.context, 11, &commit, actual, reads(minute(0)))),
        "a window's start"
    );
    assert!(
        busy(session_started_in(
            &f.context,
            11,
            &commit,
            actual,
            reads(minute(0))
        )),
        "a session's start"
    );
    assert!(
        confirm_session_in(&f.context, 11, &commit, actual).is_err_and(|error| error == LOCK_BUSY),
        "a session's confirmation"
    );
    assert_eq!(trial_on_disk(&f).starts, 0, "none of them counted");
    drop(check);
    assert!(session_start(&f, minute(1)), "with the lock free it counts");
    assert_eq!(trial_on_disk(&f).starts, 1);
}

#[test]
fn a_pass_that_never_reached_the_lock_does_not_say_it_took_it() {
    use crate::linux_notice::Record;
    let f = Fixture::new();
    let mut state = f.state.clone();
    state.check_owner = "alabsystems".into();
    state.check_repo = "aterm".into();
    state.next_check_unix = i64::MAX;
    f.context.save(&state).unwrap();
    let holder = another_aterm_holds_the_lock(&f);
    let busy = check_recording_in(Ok(&f.context), 10, &settings_provider(false), false);
    assert_eq!(
        (busy.1, busy.2),
        (Record::Busy, false),
        "{}",
        busy.0.outcome
    );
    let unreadable: crate::CheckSettingsProvider = std::sync::Arc::new(|| None);
    let stalled = check_recording_in(Ok(&f.context), 10, &unreadable, false);
    assert_eq!(
        (stalled.1, stalled.2),
        (Record::Lost, false),
        "a failure before the lock: {}",
        stalled.0.outcome
    );
    drop(holder);
    let freed = check_recording_in(Ok(&f.context), 10, &settings_provider(false), false);
    assert!(freed.2, "the pass took the lock: {}", freed.0.outcome);
}

fn leave_temp(f: &Fixture, stem: &str) -> PathBuf {
    let path = f.context.dir.join(format!(
        "{stem}-{}",
        aterm_uds::rand::hex_token::<16>().unwrap()
    ));
    fs::write(&path, vec![7u8; 1 << 20]).unwrap();
    path
}

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
}

fn notices_over<'a>(
    reporter: &mut crate::linux_notice::Reporter,
    f: &Fixture,
    passes: impl IntoIterator<Item = &'a crate::UpdateStatus>,
) -> Vec<(String, String)> {
    use std::sync::{Arc, Mutex};
    let said = Arc::new(Mutex::new(Vec::new()));
    let notify: crate::HealthNotify = Box::new({
        let said = Arc::clone(&said);
        move |title, body| said.lock().unwrap().push((title, body))
    });
    for status in passes {
        report_pass(
            reporter,
            Ok(&f.context),
            (status.clone(), crate::linux_notice::Record::Kept, true),
            None,
            Some(&notify),
        );
    }
    said.lock().unwrap().clone()
}

fn install_half(title: &str) -> bool {
    title == crate::INSTALL_CRASH_LOOP_TITLE || crate::health_failing_class(title) == Some("apply")
}

#[test]
fn a_crash_loop_rollback_the_loop_sees_is_said() {
    let mut f = Fixture::new();
    f.apply().unwrap();
    let waiting = status_in(Ok(&f.context), 10);
    let mut reporter = crate::linux_notice::Reporter::new("2026-09-28T10:00:00Z".into());
    assert!(notices_over(&mut reporter, &f, [&waiting]).is_empty());
    let t0 = minute(0);
    let rolled = (1..=60_i64).any(|start| !window_start(&f, t0 + start * 5));
    assert!(rolled, "PRECONDITION: the crash loop is rolled back");
    let rolled_back = status_in(Ok(&f.context), 10);
    assert!(
        rolled_back
            .linux
            .as_ref()
            .is_some_and(|linux| linux.refused_newer)
    );
    let said = notices_over(&mut reporter, &f, [&rolled_back]);
    assert!(
        said.iter()
            .any(|(title, body)| install_half(title) && body.contains("0.10.0 is back")),
        "the rollback was never said: {said:?}"
    );
}

#[test]
fn a_start_wait_past_its_hour_is_said_as_no_failure() {
    let mut f = Fixture::new();
    hold_stage(&mut f);
    f.apply().unwrap();
    let (waiting, _, _) = check_recording_in(Ok(&f.context), 10, &settings_provider(false), false);
    let mut reporter = crate::linux_notice::Reporter::new("2026-09-28T10:00:00Z".into());
    let said = notices_over(&mut reporter, &f, std::iter::repeat_n(&waiting, 24 * 60));
    assert!(
        said.iter().any(|(_, body)| body.contains("0.11.0")),
        "a day of waiting for a start is told: {said:?}"
    );
    assert!(
        said.iter()
            .all(|(title, _)| crate::health_failing_class(title).is_none()
                && title != crate::HEALTH_RESTATED_TITLE
                && title != crate::HEALTH_RECOVERED_TITLE),
        "and never as a failure of either half: {said:?}"
    );
}

#[test]
fn a_rollback_after_a_said_wait_is_said_as_the_install_failure() {
    let mut f = Fixture::new();
    f.apply().unwrap();
    let waiting = status_in(Ok(&f.context), 10);
    let mut reporter = crate::linux_notice::Reporter::new("2026-09-28T10:00:00Z".into());
    let wait = notices_over(&mut reporter, &f, std::iter::repeat_n(&waiting, 2 * 60));
    assert!(
        wait.iter().any(|(_, body)| body.contains("0.11.0")),
        "PRECONDITION: the wait was said: {wait:?}"
    );
    let t0 = minute(0);
    let rolled = (1..=60_i64).any(|start| !window_start(&f, t0 + start * 5));
    assert!(rolled, "PRECONDITION: the crash loop is rolled back");
    let rolled_back = status_in(Ok(&f.context), 10);
    let said = notices_over(&mut reporter, &f, [&rolled_back]);
    assert!(
        said.iter()
            .any(|(title, body)| install_half(title) && body.contains("is back")),
        "the rollback is the install half's failure, on the band: {said:?}"
    );
}

#[test]
fn a_rollback_a_person_asked_for_is_said_as_no_install_failure() {
    let mut f = Fixture::new();
    f.apply().unwrap();
    assert!(window_start(&f, minute(0)), "the new build starts");
    assert!(window_confirm(&f), "and its window confirms it");
    let healthy = status_in(Ok(&f.context), 10);
    let mut reporter = crate::linux_notice::Reporter::new("2026-09-28T10:00:00Z".into());
    assert!(notices_over(&mut reporter, &f, [&healthy]).is_empty());
    let mut state = f.context.read_state().unwrap().unwrap();
    rollback_locked(&f.context, &mut state).expect("aterm update rollback");
    let rolled = status_in(Ok(&f.context), 10);
    assert!(
        rolled
            .linux
            .as_ref()
            .is_some_and(|linux| linux.refused_newer),
        "PRECONDITION: the copy refuses the build it rolled back"
    );
    let said = notices_over(&mut reporter, &f, [&rolled]);
    assert!(
        !said.iter().any(|(title, _)| install_half(title)),
        "a person's own rollback is no install failure: {said:?}"
    );
}

#[test]
fn a_wait_over_a_start_that_never_finished_does_not_say_none_began() {
    let mut f = Fixture::new();
    f.apply().unwrap();
    assert!(window_start(&f, minute(0)), "a start is counted");
    let waiting = status_in(Ok(&f.context), 10);
    assert_eq!(
        waiting
            .linux
            .as_ref()
            .map(|linux| (linux.trial_starts, linux.trial_healthy)),
        Some((1, false)),
        "PRECONDITION: one start, unfinished"
    );
    let mut reporter = crate::linux_notice::Reporter::new("2026-09-28T10:00:00Z".into());
    let said = notices_over(&mut reporter, &f, std::iter::repeat_n(&waiting, 2 * 60));
    let wait: Vec<_> = said
        .iter()
        .filter(|(_, body)| body.contains("0.11.0"))
        .collect();
    assert!(
        !wait.is_empty(),
        "PRECONDITION: the wait was said: {said:?}"
    );
    assert!(
        wait.iter()
            .all(|(_, body)| !body.contains("not started yet")),
        "a counted start is not 'not started yet': {wait:?}"
    );
}

#[test]
fn a_rollback_seen_on_a_checks_pass_is_said_in_its_own_words() {
    let mut f = Fixture::new();
    f.apply().unwrap();
    let waiting = status_in(Ok(&f.context), 10);
    let mut reporter = crate::linux_notice::Reporter::new("2026-09-28T10:00:00Z".into());
    assert!(notices_over(&mut reporter, &f, [&waiting]).is_empty());
    let t0 = minute(0);
    let rolled = (1..=60_i64).any(|start| !window_start(&f, t0 + start * 5));
    assert!(rolled, "PRECONDITION: the crash loop is rolled back");
    let mut checked = f.context.read_state().unwrap().unwrap();
    checked.outcome = "Up to date \u{2014} the newest release is aterm 0.10.0".into();
    checked.updated_at = "2026-09-28T15:00:00Z".into();
    f.context.save(&checked).unwrap();
    let said = notices_over(&mut reporter, &f, [&status_in(Ok(&f.context), 10)]);
    let rollback: Vec<_> = said
        .iter()
        .filter(|(title, _)| install_half(title))
        .collect();
    assert_eq!(
        rollback.len(),
        1,
        "PRECONDITION: the rollback is said: {said:?}"
    );
    assert!(
        rollback
            .iter()
            .all(|(_, body)| body.contains("0.10.0 is back") && !body.contains("Up to date")),
        "the rollback says what happened, not what the check found: {rollback:?}"
    );
}

#[test]
fn a_crash_loop_no_rollback_could_answer_is_said() {
    for wait_said in [false, true] {
        let mut f = Fixture::new();
        f.apply().unwrap();
        let mut state = f.context.read_state().unwrap().unwrap();
        state.min_build = 11;
        f.context.save(&state).unwrap();
        let waiting = status_in(Ok(&f.context), 10);
        let mut reporter = crate::linux_notice::Reporter::new("2026-09-28T10:00:00Z".into());
        let passes = if wait_said { 2 * 60 } else { 1 };
        let wait = notices_over(&mut reporter, &f, std::iter::repeat_n(&waiting, passes));
        assert_eq!(
            wait.iter().any(|(_, body)| body.contains("0.11.0")),
            wait_said,
            "PRECONDITION: the wait was said as planned: {wait:?}"
        );
        for n in 0..MAX_TRIAL_STARTS {
            assert!(window_start(&f, minute(i64::from(n))), "start {n} is a try");
        }
        assert!(!window_start(&f, minute(i64::from(MAX_TRIAL_STARTS))));
        let kept = f.context.read_state().unwrap().unwrap();
        assert!(
            kept.trial.is_none() && kept.installed.build == 11 && kept.rejected_build == 0,
            "PRECONDITION: the trial ended and the build stays"
        );
        let said = notices_over(&mut reporter, &f, [&status_in(Ok(&f.context), 10)]);
        assert!(
            said.iter().any(|(title, body)| install_half(title)
                && body.contains("0.11.0")
                && body.contains("stays")),
            "the build that failed its trial and stays is told (wait said: {wait_said}): \
             {said:?}"
        );
    }
}

#[test]
fn a_crash_loops_end_found_at_launch_is_said_once() {
    let mut f = Fixture::new();
    f.apply().unwrap();
    let t0 = minute(0);
    let rolled = (1..=60_i64).any(|start| !window_start(&f, t0 + start * 5));
    assert!(rolled, "PRECONDITION: the crash loop is rolled back");
    let mut launched = crate::linux_notice::Reporter::new("2026-09-28T10:00:00Z".into());
    let said = notices_over(&mut launched, &f, [&status_in(Ok(&f.context), 10)]);
    assert!(
        said.iter()
            .any(|(title, body)| install_half(title) && body.contains("0.10.0 is back")),
        "the rollback found at launch was never said: {said:?}"
    );
    let mut later = crate::linux_notice::Reporter::new("2026-09-28T11:00:00Z".into());
    let again = notices_over(&mut later, &f, [&status_in(Ok(&f.context), 10)]);
    assert!(
        !again.iter().any(|(title, _)| install_half(title)),
        "a later launch does not say it again: {again:?}"
    );
}

#[test]
fn a_crash_loops_end_waits_for_a_loop_with_a_window_to_tell() {
    let mut f = Fixture::new();
    f.apply().unwrap();
    let t0 = minute(0);
    assert!(
        (1..=60_i64).any(|start| !session_start(&f, t0 + start * 5)),
        "PRECONDITION: build 11's crash loop is rolled back at a session's start"
    );
    let mut session = crate::linux_notice::Reporter::new("2026-09-28T10:00:00Z".into());
    report_pass(
        &mut session,
        Ok(&f.context),
        (
            status_in(Ok(&f.context), 10),
            crate::linux_notice::Record::Kept,
            true,
        ),
        None,
        None,
    );
    assert_eq!(
        f.context
            .read_state()
            .unwrap()
            .unwrap()
            .unsaid_trial_ends
            .len(),
        1,
        "the session's windowless loop leaves the end on the record"
    );
    stage_release(&mut f, 12, 3, 'c');
    f.apply().unwrap();
    let t1 = minute(60);
    assert!(
        (1..=60_i64).any(|start| !window_start_as(&f, 12, 'c', t1 + start * 5)),
        "PRECONDITION: build 12's crash loop is rolled back too"
    );
    let mut window = crate::linux_notice::Reporter::new("2026-09-28T09:00:00Z".into());
    let said = notices_over(&mut window, &f, [&status_in(Ok(&f.context), 10)]);
    let ends: Vec<&str> = said
        .iter()
        .filter(|(title, _)| install_half(title))
        .map(|(_, body)| body.as_str())
        .collect();
    assert!(
        matches!(ends.as_slice(), [first, second]
            if first.contains("0.11.0") && second.contains("0.12.0")),
        "the window says each crash loop, oldest first: {said:?}"
    );
    let again = notices_over(&mut window, &f, [&status_in(Ok(&f.context), 10)]);
    assert!(
        !again.iter().any(|(title, _)| install_half(title)),
        "once: {again:?}"
    );

    let mut state = f.context.read_state().unwrap().unwrap();
    for n in 0..=UNSAID_TRIAL_ENDS_KEPT {
        keep_unsaid_trial_end(&mut state, format!("end {n}"));
    }
    assert_eq!(
        state.unsaid_trial_ends.first().map(String::as_str),
        Some("end 1"),
        "past the bound the oldest goes"
    );
    assert_eq!(state.unsaid_trial_ends.len(), UNSAID_TRIAL_ENDS_KEPT);
}

#[test]
fn a_second_crash_loop_is_said_as_its_own() {
    let mut f = Fixture::new();
    f.apply().unwrap();
    let mut reporter = crate::linux_notice::Reporter::new("2026-09-28T10:00:00Z".into());
    assert!(notices_over(&mut reporter, &f, [&status_in(Ok(&f.context), 10)]).is_empty());
    let t0 = minute(0);
    assert!(
        (1..=60_i64).any(|start| !window_start(&f, t0 + start * 5)),
        "PRECONDITION: build 11's crash loop is rolled back"
    );
    let first = notices_over(&mut reporter, &f, [&status_in(Ok(&f.context), 10)]);
    assert!(
        first
            .iter()
            .any(|(title, body)| install_half(title) && body.contains("0.11.0")),
        "the first crash loop names the build that failed: {first:?}"
    );
    stage_release(&mut f, 12, 3, 'c');
    f.apply().unwrap();
    assert!(notices_over(&mut reporter, &f, [&status_in(Ok(&f.context), 10)]).is_empty());
    let t1 = minute(60);
    assert!(
        (1..=60_i64).any(|start| !window_start_as(&f, 12, 'c', t1 + start * 5)),
        "PRECONDITION: build 12's crash loop is rolled back"
    );
    let second = notices_over(&mut reporter, &f, [&status_in(Ok(&f.context), 10)]);
    assert!(
        second
            .iter()
            .any(|(title, body)| install_half(title) && body.contains("0.12.0")),
        "the second crash loop is said as its own: {second:?}"
    );
}
