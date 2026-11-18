// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const CHILD: &str = "ATERM_RELEASE_COMPILER_TRANSPORT_TEST";
const COMMIT: &str = "29ebb403de7a102850d6cb10a62d5cc3baf723ed";

// A child restores the real transported environment before libtest creates its threads.
// The parent changes no process-global environment or compiler pin.
extern "C" fn restore_at_load() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    // SAFETY: this load-time initializer runs before main and before any other thread.
    unsafe { capture_exec_handoff_environment() }.unwrap();
}

#[used]
#[cfg_attr(
    target_vendor = "apple",
    unsafe(link_section = "__DATA,__mod_init_func,mod_init_funcs")
)]
#[cfg_attr(target_os = "linux", unsafe(link_section = ".init_array"))]
static RESTORE_AT_LOAD: extern "C" fn() = restore_at_load;

fn build(prefix: &Path, n: u64, marker: Option<&Path>) -> atpkg::trust_lock::TrustLock {
    use std::os::unix::fs::PermissionsExt as _;
    let root = atpkg::trust_lock::build_dir(prefix, n);
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::create_dir_all(root.join("lib/rustlib")).unwrap();
    let write = marker.map_or(String::new(), |marker| {
        format!(
            "printf ran > '{}'\n",
            marker.to_string_lossy().replace('\'', "'\\''"),
        )
    });
    for (tool, body) in [
        (
            "trustc",
            format!("{write}printf 'commit-hash: {COMMIT}\\n'\n"),
        ),
        ("targo", format!("printf '{n}\\n'\n")),
    ] {
        let path = root.join("bin").join(tool);
        std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    atpkg::store::mark_build_ready(&root).unwrap();
    assert!(atpkg::store::build_is_complete(&root));
    atpkg::trust_lock::TrustLock {
        build: n,
        tree_root: atpkg::tree::tree_root(&root).unwrap(),
        compiler_commit: COMMIT.into(),
        receipt: "SEED-MINT sha256:00".into(),
        stock_toolchain: "1.99.1".into(),
    }
}

#[test]
fn implicit_transport_can_select_b_while_a_real_user_override_still_refuses_it() {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::MetadataExt as _;
    const NAME: &str = "release_steps::compiler_handoff_tests::implicit_transport_can_select_b_while_a_real_user_override_still_refuses_it";
    if let Some(mode) = std::env::var_os(CHILD) {
        if mode == "probe" {
            let raw = std::env::var("ATERM_RELEASE_COMPILER_TEST_FD")
                .unwrap()
                .parse()
                .unwrap();
            let expected = (
                std::env::var("ATERM_RELEASE_COMPILER_TEST_DEV")
                    .unwrap()
                    .parse::<u64>()
                    .unwrap(),
                std::env::var("ATERM_RELEASE_COMPILER_TEST_INO")
                    .unwrap()
                    .parse::<u64>()
                    .unwrap(),
            );
            assert!(
                !aterm_uds::spawnfd::descriptor_metadata(raw)
                    .is_ok_and(|metadata| (metadata.dev(), metadata.ino()) == expected)
            );
            println!("compiler-fd-not-inherited");
            return;
        }
        let root = PathBuf::from(std::env::var_os("ATERM_RELEASE_COMPILER_TEST_ROOT").unwrap());
        let prefix = root.join("pkg");
        let packet = STARTUP_HANDOFF.lock().unwrap().take().unwrap();
        let compiler = packet.compiler.unwrap();
        let a = compiler.pin.lock();
        let a_bin = compiler.bin;
        let original = packet.lease_fd.unwrap();
        let raw = original.as_raw_fd();
        // SAFETY: the packet owns this actual live inherited description.
        assert_ne!(
            unsafe { libc::fcntl(raw, libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
        let metadata = aterm_uds::spawnfd::descriptor_metadata(raw).unwrap();
        let mut probe = command(std::env::current_exe().unwrap());
        probe
            .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
            .env(CHILD, "probe")
            .env("ATERM_RELEASE_COMPILER_TEST_FD", raw.to_string())
            .env(
                "ATERM_RELEASE_COMPILER_TEST_DEV",
                metadata.dev().to_string(),
            )
            .env(
                "ATERM_RELEASE_COMPILER_TEST_INO",
                metadata.ino().to_string(),
            );
        let probe = aterm_uds::deadline::output_while_process_progressing(
            &mut probe,
            std::time::Duration::from_secs(60),
            "startup compiler descriptor probe",
        )
        .unwrap();
        assert!(probe.status.success(), "{probe:?}");
        assert!(String::from_utf8_lossy(&probe.stdout).contains("compiler-fd-not-inherited"));
        let leaf = packet.lease_leaf.unwrap().into_string().unwrap();
        let selected_lease = captured_intake(original, |intake| {
            // SAFETY: capture owned the real original inherited description at load; this
            // fixture independently names its exact managed subject and holder leaf.
            unsafe { atpkg::lease::Lease::adopt_inherited(intake, &prefix, &a_bin, &leaf) }
                .map_err(|error| error.to_string())
        })
        .unwrap();
        let fresh = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(selected_lease.path())
            .unwrap();
        let unrelated: std::os::fd::OwnedFd = fresh.into();
        aterm_uds::keep_out_of_children(&unrelated).unwrap();
        assert!(
            captured_intake(unrelated, |intake| {
                // SAFETY: intentionally offer a different description of the same named file;
                // native intake must refuse it without unlocking the actual retained lease.
                unsafe { atpkg::lease::Lease::adopt_inherited(intake, &prefix, &a_bin, &leaf) }
                    .map_err(|error| error.to_string())
            })
            .is_err()
        );
        let marker = root.join("compiler-b-ran");
        let b = build(&prefix, 2, Some(&marker));
        crate::gates::pin_trust_stage2_bin(&a_bin).unwrap();
        let explicit = std::env::var_os("TRUST_STAGE2_BIN").map(PathBuf::from);
        assert_eq!(explicit.is_some(), mode == "user");
        let mut leases = Vec::new();
        let selected = crate::trust_lock::locked_prover(
            &prefix,
            &b,
            explicit.as_deref(),
            crate::trust_lock::Run::Cut,
            &mut |_, _| panic!("both exact builds are present"),
            &mut |bin| {
                let lease = atpkg::lease::take_for_dir(&prefix, bin, "compiler transport control")
                    .map_err(|error| error.to_string())?
                    .ok_or("no managed compiler subject")?;
                leases.push(lease);
                Ok(())
            },
        );
        if mode == "user" {
            assert!(selected.is_err(), "an actual A override must refuse B");
            assert!(!marker.exists());
        } else {
            assert_eq!(
                selected.unwrap().bin,
                atpkg::trust_lock::build_dir(&prefix, 2)
                    .join("bin")
                    .canonicalize()
                    .unwrap()
            );
            assert!(
                marker.exists(),
                "B executed only after taking its managed lease"
            );
            assert_eq!(leases.len(), 1);
        }
        assert_eq!(
            selected_lease.subject(),
            &atpkg::lease::Subject::build("trust", a.build).unwrap()
        );
        assert_eq!(
            crate::gates::trust_stage2_bin().unwrap(),
            a_bin,
            "selecting B must preserve this image's original A pin"
        );
        println!("compiler-transport-checked");
        return;
    }
    for mode in ["implicit", "user"] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path().canonicalize().unwrap();
        let prefix = root.join("pkg");
        let a = build(&prefix, 1, None);
        let a_bin = atpkg::trust_lock::build_dir(&prefix, a.build)
            .join("bin")
            .canonicalize()
            .unwrap();
        let lease = atpkg::lease::take_for_dir(&prefix, &a_bin, "compiler transport sender")
            .unwrap()
            .unwrap();
        let leaf = lease.path().file_name().unwrap().to_owned();
        let offer = CompilerHandoff {
            commit: "1".repeat(40),
            bin: a_bin.clone(),
            pin: crate::release::CompilerPin::of(&a),
            user_override: mode == "user",
        };
        let mut child = command(std::env::current_exe().unwrap());
        child
            .inherit_guards(vec![lease.try_clone_descriptor().unwrap()])
            .unwrap();
        let descriptor = child.guard_targets().next().unwrap();
        child
            .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
            .env(CHILD, mode)
            .env("ATERM_RELEASE_COMPILER_TEST_ROOT", &root)
            .env("TRUST_STAGE2_BIN", &a_bin)
            .env(COMPILER_HANDOFF_ENV, aterm_json::to_string(&offer).unwrap())
            .env(TOOLCHAIN_LEASE_FD_ENV, descriptor.to_string())
            .env(TOOLCHAIN_LEASE_LEAF_ENV, leaf);
        let output = aterm_uds::deadline::output_while_process_progressing(
            &mut child,
            std::time::Duration::from_secs(60),
            "compiler transport control",
        )
        .unwrap();
        assert!(output.status.success(), "{mode}: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("compiler-transport-checked"),
            "{mode}: {output:?}"
        );
    }
}
