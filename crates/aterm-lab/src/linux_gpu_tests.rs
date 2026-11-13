// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The SDK ships `libvulkan.1.dylib` as a link to the versioned dylib beside it: placing
/// copies the file it names, under the placed name, as a regular file.
#[test]
fn a_placed_link_inside_the_install_root_is_copied_as_the_file_it_names() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let lib = root.join("macOS/lib");
    std::fs::create_dir_all(&lib).unwrap();
    std::fs::write(lib.join("libvulkan.1.4.363.dylib"), b"loader").unwrap();
    std::os::unix::fs::symlink("libvulkan.1.4.363.dylib", lib.join("libvulkan.1.dylib")).unwrap();
    let kk = dir.path().join("kk");
    place_files(
        &root,
        &kk,
        &[("macOS/lib/libvulkan.1.dylib", "lib/libvulkan.1.dylib")],
    )
    .expect("a link inside the root is placed");
    let placed = kk.join("lib/libvulkan.1.dylib");
    assert!(
        std::fs::symlink_metadata(&placed)
            .unwrap()
            .file_type()
            .is_file(),
        "placed as a regular file"
    );
    assert_eq!(std::fs::read(&placed).unwrap(), b"loader");
}

/// A placed source whose link leaves the install root is refused, and nothing is placed.
#[test]
fn a_placed_link_escaping_the_install_root_is_refused() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let lib = root.join("macOS/lib");
    std::fs::create_dir_all(&lib).unwrap();
    let outside = dir.path().join("outside.dylib");
    std::fs::write(&outside, b"not the SDK's").unwrap();
    std::os::unix::fs::symlink(&outside, lib.join("libvulkan.1.dylib")).unwrap();
    let kk = dir.path().join("kk");
    let why = place_files(
        &root,
        &kk,
        &[("macOS/lib/libvulkan.1.dylib", "lib/libvulkan.1.dylib")],
    )
    .expect_err("a link out of the root");
    assert!(why.contains("outside"), "{why}");
    assert!(!kk.join("lib/libvulkan.1.dylib").exists());
}

/// A report's header line, then its JSON: `exception` and the faulting thread's frames,
/// each naming its image, as macOS 27's `.ips` reports do.
fn ips(signal: &str, frames: &[(&str, usize)], images: &[&str]) -> String {
    let frames: Vec<String> = frames
        .iter()
        .map(|(s, i)| format!(r#"{{"imageIndex":{i},"symbol":"{s}"}}"#))
        .collect();
    let images: Vec<String> = images
        .iter()
        .map(|n| format!(r#"{{"name":"{n}"}}"#))
        .collect();
    format!(
        "{{\"app_name\":\"krunkit\",\"bug_type\":\"309\"}}\n{{\"exception\":{{\"type\":\
         \"EXC_CRASH\",\"signal\":\"{signal}\"}},\"faultingThread\":1,\"threads\":[{{\
         \"frames\":[]}},{{\"name\":\"gpu worker\",\"frames\":[{}]}}],\"usedImages\":[{}]}}",
        frames.join(","),
        images.join(",")
    )
}

/// The eight reports' stack: abort in libmalloc's double-free trap under
/// `venus_context_destroy`, from libkrun's GPU worker.
fn venus_abort() -> String {
    ips(
        "SIGABRT",
        &[
            ("__pthread_kill", 0),
            ("abort", 1),
            (
                "___BUG_IN_CLIENT_OF_LIBMALLOC_POINTER_BEING_FREED_WAS_NOT_ALLOCATED",
                2,
            ),
            ("venus_context_destroy", 3),
            ("util_hash_table_remove", 3),
            ("_RNvMNtCs9OaWgLio5qr_17krun_rutabaga_gfx13rutabaga_core", 4),
        ],
        &[
            "libsystem_kernel.dylib",
            "libsystem_c.dylib",
            "libsystem_malloc.dylib",
            "libvirglrenderer.1.dylib",
            "libkrun.1.19.7.dylib",
        ],
    )
}

#[test]
fn the_venus_double_free_is_named_as_the_known_abort() {
    let cause = crash_cause(&venus_abort()).expect("a crash report");
    assert!(
        cause.starts_with("virglrenderer's double free in venus_context_destroy (SIGABRT)"),
        "{cause}"
    );
}

#[test]
fn another_crash_is_named_by_its_first_frame_outside_the_system() {
    let other = ips(
        "SIGSEGV",
        &[("__pthread_kill", 0), ("vkr_ring_thread", 1)],
        &["libsystem_kernel.dylib", "libvirglrenderer.1.dylib"],
    );
    assert_eq!(
        crash_cause(&other).as_deref(),
        Some("SIGSEGV in vkr_ring_thread (libvirglrenderer.1.dylib)")
    );
    assert_eq!(
        crash_cause("{\"app_name\":\"krunkit\"}"),
        None,
        "a header alone"
    );
    assert_eq!(crash_cause("not a report\nat all"), None);
}

#[test]
fn only_a_krunkit_report_written_since_the_command_began_is_its_abort() {
    let dir = aterm_tempfile::tempdir().expect("a scratch dir");
    let old = dir.path().join("krunkit-2026-10-24-175530.ips");
    std::fs::write(&old, venus_abort()).expect("old report");
    let since = SystemTime::now();
    std::fs::File::options()
        .write(true)
        .open(&old)
        .and_then(|f| f.set_modified(since - Duration::from_secs(3600)))
        .expect("an hour old");
    std::fs::write(
        dir.path().join("Finder-2026-10-29-120000.ips"),
        venus_abort(),
    )
    .expect("another process's report");
    let lost = krunkit_lost(dir.path(), since, Duration::ZERO);
    assert!(lost.contains("no krunkit crash report"), "{lost}");
    let new = dir.path().join("krunkit-2026-10-29-120001.ips");
    std::fs::write(&new, venus_abort()).expect("new report");
    let lost = krunkit_lost(dir.path(), since, Duration::ZERO);
    assert!(
        lost.starts_with("krunkit aborted: virglrenderer's double free"),
        "{lost}"
    );
    assert!(lost.contains(&new.display().to_string()), "{lost}");
}

/// A missing krunkit is named with where it was looked for and no install command.
#[test]
fn a_missing_krunkit_is_named_without_an_install_command() {
    let why = no_krunkit();
    let (looked, rest) = why.split_once(": ").expect("where it looked, then why");
    assert_eq!(
        looked,
        "no krunkit at /opt/homebrew/bin/krunkit or /usr/local/bin/krunkit or on PATH"
    );
    assert!(rest.contains("aterm provides none"), "{why}");
    assert!(
        !rest.contains("install") && !rest.contains("brew"),
        "no remedy installs anything: {why}"
    );
}

#[test]
fn the_gpu_instance_runs_aarch64_alone_and_refuses_x86_64_by_name() {
    assert_eq!(
        lane_arches(vec![], false, "proof").ok(),
        Some(Arch::ALL.to_vec())
    );
    assert_eq!(
        lane_arches(vec![Arch::X86_64], false, "proof").ok(),
        Some(vec![Arch::X86_64])
    );
    assert_eq!(
        lane_arches(vec![], true, "proof").ok(),
        Some(vec![Arch::Aarch64])
    );
    let refused = lane_arches(vec![Arch::Aarch64, Arch::X86_64], true, "baseline")
        .expect_err("x86_64 on krunkit");
    let why = refused.msg.unwrap_or_default();
    assert_eq!(refused.code, 2, "a usage error");
    assert!(why.contains("no Rosetta"), "{why}");
}

#[test]
fn the_instance_is_krunkits_with_virtio_fs_and_no_rosetta() {
    let args = create_args();
    assert!(args.contains(&format!("--name={VM}")));
    assert!(args.contains(&"--vm-type=krunkit".to_string()));
    assert!(args.contains(&"--mount-type=virtiofs".to_string()));
    assert!(!args.iter().any(|a| a.contains("rosetta")));
    assert_eq!(
        args.last().map(String::as_str),
        Some(crate::linux::TEMPLATE)
    );
}

#[test]
fn krunkit_loads_kosmickrisp_through_the_shim() {
    let env = host_env(
        Path::new("/c/kosmickrisp-1.4.363.0"),
        Path::new("/opt/homebrew/bin/krunkit"),
        Some("/usr/bin:/bin"),
    );
    assert_eq!(
        env,
        vec![
            (
                "DYLD_LIBRARY_PATH".to_string(),
                "/c/kosmickrisp-1.4.363.0/shim".to_string()
            ),
            (
                "VK_DRIVER_FILES".to_string(),
                "/c/kosmickrisp-1.4.363.0/share/vulkan/icd.d/libkosmickrisp_icd.json".to_string()
            ),
            (
                "PATH".to_string(),
                "/opt/homebrew/bin:/usr/bin:/bin".to_string()
            ),
        ]
    );
}

#[test]
fn the_probes_judge_the_gpu_by_name() {
    // As measured on 2026-10-24 in the GPU instance.
    let vk = "Devices:\n========\nGPU0:\n\tapiVersion = 1.3.0\n\tdeviceName = Virtio-GPU Venus (Apple M4 Pro)\n";
    let gl = "name of display: :99\nOpenGL vendor string: Mesa\nOpenGL renderer string: zink Vulkan 1.3(Virtio-GPU Venus (Apple M4 Pro) (MESA_VENUS))\n";
    assert_eq!(vulkan_device(vk), Some("Virtio-GPU Venus (Apple M4 Pro)"));
    assert!(judge(vulkan_device(vk), gl_renderer(gl)).is_ok());
    let lavapipe = "\tdeviceName = llvmpipe (LLVM 20.1.2, 128 bits)\n";
    assert!(judge(vulkan_device(lavapipe), gl_renderer(gl)).is_err());
    let llvmpipe = "OpenGL renderer string: llvmpipe (LLVM 20.1.2, 128 bits)\n";
    let err = judge(vulkan_device(vk), gl_renderer(llvmpipe)).expect_err("llvmpipe");
    assert!(err.contains("llvmpipe"), "{err}");
    assert!(judge(None, gl_renderer(gl)).is_err());
}

#[test]
fn the_guest_steps_hold_every_download_to_its_digest() {
    let kernel = kernel_script(&KERNEL);
    for (deb, sha) in KERNEL.debs {
        assert!(kernel.contains(deb) && kernel.contains(sha), "{deb}");
    }
    assert!(kernel.contains("purge -y -q flash-kernel"));
    assert!(kernel.contains("--no-triggers") && kernel.contains(TRIGGER_FIX));
    let venus = venus_script(&VENUS);
    for pinned in [VENUS.mesa_sha256, VENUS.meson_sha256, VENUS.protocol.1] {
        assert!(venus.contains(pinned), "{pinned}");
    }
    assert!(venus.contains("--wrap-mode=nodownload"));
    assert!(venus.contains("subprojects/venus-protocol-1.1.3"));
    let labels: Vec<&str> = gpu_steps().iter().map(|s| s.label).collect();
    assert_eq!(labels, ["venus build packages", "mainline kernel", "venus"]);
}
