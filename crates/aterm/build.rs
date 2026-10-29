// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// Win32 resource embedding and build metadata emission.

#![allow(
    clippy::disallowed_types,
    reason = "never linked into the shipped binary: a build script"
)]

/// Ordinal of both the icon and the manifest; resources are keyed by (type, id).
const RESOURCE_ID: u16 = 1;

/// `RT_MANIFEST` as its raw type number (`24`) so `rc.exe` needs no `<winuser.h>`.
const RT_MANIFEST: u16 = 24;

/// `VS_VERSION_INFO` as its raw id (`1`) so `rc.exe` needs no `<winver.h>`.
const VS_VERSION_INFO_ID: u16 = 1;

fn main() {
    build_stamp::emit();
    let compiler_flavor = compiler::emit();
    chained_fixups();
    macos_release_linker(compiler_flavor);

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if msvc {
        println!("cargo:rustc-link-arg-bins=/STACK:{}", 8 * 1024 * 1024);
    }

    let manifest_dir = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo always sets CARGO_MANIFEST_DIR"),
    );
    let out_dir = std::path::PathBuf::from(
        std::env::var_os("OUT_DIR").expect("cargo always sets OUT_DIR for a build script"),
    );

    let crates_dir = manifest_dir.parent().unwrap_or(&manifest_dir).to_path_buf();
    let icon = crates_dir.join("aterm-gui/assets/aterm.ico");
    let app_manifest = manifest_dir.join("assets/aterm.manifest");

    println!("cargo:rerun-if-changed={}", icon.display());
    println!("cargo:rerun-if-changed={}", app_manifest.display());
    println!("cargo:rerun-if-env-changed=ATERM_REQUIRE_WIN_RESOURCES");

    // MSVC's linker writes the one manifest, the `asInvoker` one every executable gets
    // (.cargo/config.toml) merged with this file: a second `RT_MANIFEST` 1 from rc.exe would
    // collide with it. Another linker takes the file as a resource.
    if msvc {
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}",
            app_manifest.display()
        );
    }
    let rc_path = out_dir.join("aterm.rc");
    let rc_text = resource_script(&icon, (!msvc).then_some(app_manifest.as_path()));
    if let Err(e) = std::fs::write(&rc_path, rc_text) {
        report(&format!("could not write {}: {e}", rc_path.display()));
        return;
    }

    let outcome = aterm_winres::compile(&rc_path);
    if outcome.is_linked() {
        return;
    }
    report(&outcome.to_string());
}

/// Emit `-Wl,-fixup_chains` on `aarch64-apple-darwin` so dyld uses chained fixups.
fn chained_fixups() {
    let target = |key: &str| std::env::var(key).unwrap_or_default();
    if target("CARGO_CFG_TARGET_OS") == "macos" && target("CARGO_CFG_TARGET_ARCH") == "aarch64" {
        println!("cargo:rustc-link-arg-bins=-Wl,-fixup_chains");
    }
}

/// On macOS `[profile.release]` builds, strip LTO-internalized local symbols (`-x`) so
/// `dyld` does not fault megabytes of `__LINKEDIT` during `dlsym`, and order section atoms
/// via `src/macho_order.rs` (structural `(tier, crate, module)` grouping refined by
/// multi-permutation headless startup `CS_VALIDATED` page-fault intersection) so cold crates
/// and cold functions inside touched modules do not fragment startup/idle `__TEXT` pages.
fn macos_release_linker(compiler_flavor: &str) {
    let manifest_dir = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo always sets CARGO_MANIFEST_DIR"),
    );
    let macho_order_src = manifest_dir.join("src/macho_order.rs");
    println!("cargo:rerun-if-changed={}", macho_order_src.display());
    println!("cargo:rerun-if-env-changed=PROFILE");
    println!("cargo:rerun-if-env-changed=DEBUG");
    println!("cargo:rerun-if-env-changed=OPT_LEVEL");

    let env_or_empty = |key: &str| std::env::var(key).unwrap_or_default();
    let is_stripped_release = env_or_empty("CARGO_CFG_TARGET_OS") == "macos"
        && env_or_empty("PROFILE") == "release"
        && matches!(env_or_empty("DEBUG").as_str(), "" | "0" | "false")
        && std::env::var_os("CARGO_CFG_DEBUG_ASSERTIONS").is_none()
        && env_or_empty("OPT_LEVEL") == "3";
    if !is_stripped_release {
        return;
    }

    if !cfg!(target_os = "macos") {
        println!("cargo:rustc-link-arg-bins=-Wl,-x");
        return;
    }

    let Some(out_dir) = std::env::var_os("OUT_DIR").map(std::path::PathBuf::from) else {
        println!("cargo:rustc-link-arg-bins=-Wl,-x");
        return;
    };
    let Ok(order_mod_bytes) = std::fs::read(&macho_order_src) else {
        println!("cargo:rustc-link-arg-bins=-Wl,-x");
        return;
    };

    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let probe_dylib = if env_or_empty("CARGO_CFG_TARGET_ARCH") == std::env::consts::ARCH {
        build_macos_order_probe_dylib(&out_dir, &rustc, compiler_flavor).unwrap_or_default()
    } else {
        std::path::PathBuf::new()
    };

    let real_ld = run("/usr/bin/xcrun", &["-f", "ld"]).unwrap_or_else(|| "/usr/bin/ld".into());
    let header = format!(
        "// GENERATED by crates/aterm/build.rs -- do not edit.\n\
         #[allow(dead_code)]\n\
         #[path = {macho_order_src:?}]\n\
         mod macho_order;\n\
         const REAL_LD: &str = {real_ld:?};\n\
         const ORDER_DIR: &str = {out_dir:?};\n\
         const PROBE_DYLIB: &str = {probe_dylib:?};\n\n"
    );
    let wrapper_body = r##"
fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--aterm-self-check") {
        println!("aterm-order-ld ok");
        return std::process::ExitCode::SUCCESS;
    }
    let link_args = &args[1..];
    let expanded = macho_order::expand_linker_args(link_args);
    let has_order_file = expanded.iter().any(|a| a == "-order_file");
    let has_strip_locals = expanded.iter().any(|a| a == "-x");

    let mut temp_order: Option<std::path::PathBuf> = None;
    if !has_order_file {
        let inputs = macho_order::collect_input_paths(&expanded);
        let mut ordered = macho_order::build_order_list(&inputs);
        if should_run_probe(&expanded, &ordered)
            && let Some(refined) = run_permutation_sieve(&ordered, &expanded)
        {
            ordered = refined;
        }
        if !ordered.is_empty() {
            let path = std::path::Path::new(ORDER_DIR)
                .join(format!("aterm-{}.order", std::process::id()));
            if macho_order::write_order_file(&path, &ordered).is_ok() {
                temp_order = Some(path);
            }
        }
    }

    let mut cmd = std::process::Command::new(REAL_LD);
    cmd.args(link_args);
    if let Some(ref order_path) = temp_order {
        cmd.arg("-order_file").arg(order_path);
    }
    if !has_strip_locals {
        cmd.arg("-x");
    }

    let status = cmd.status();
    if let Some(order_path) = temp_order {
        let _ = std::fs::remove_file(order_path);
    }
    match status {
        Ok(s) if s.success() => std::process::ExitCode::SUCCESS,
        Ok(s) => std::process::ExitCode::from(
            s.code().and_then(|c| u8::try_from(c).ok()).unwrap_or(1),
        ),
        Err(e) => {
            eprintln!("aterm-order-ld: could not invoke {REAL_LD}: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn find_output_path(expanded: &[String]) -> Option<std::path::PathBuf> {
    let mut i = 0;
    let mut found = None;
    while i < expanded.len() {
        if expanded[i] == "-o"
            && let Some(val) = expanded.get(i + 1)
        {
            found = Some(std::path::PathBuf::from(val));
            i += 2;
            continue;
        }
        i += 1;
    }
    found
}

fn should_run_probe(expanded: &[String], ordered: &[String]) -> bool {
    if PROBE_DYLIB.is_empty()
        || !std::path::Path::new(PROBE_DYLIB).is_file()
        || std::env::var_os("ATERM_SKIP_ORDER_PROBE").is_some()
        || ordered.len() <= 5_000
        || !ordered.iter().any(|s| s == "_main")
    {
        return false;
    }
    let Some(out) = find_output_path(expanded) else {
        return false;
    };
    let Some(stem) = out.file_stem().and_then(|s| s.to_str()) else {
        return false;
    };
    (stem == "aterm" || stem.starts_with("aterm-")) && !stem.contains("bootstrap")
}

fn build_probe_ld_args(
    expanded: &[String],
    probe_bin: &std::path::Path,
    pad_obj: &std::path::Path,
    order_file: &std::path::Path,
) -> Vec<std::ffi::OsString> {
    let mut out = Vec::with_capacity(expanded.len() + 4);
    let mut i = 0;
    let mut replaced_o = false;
    while i < expanded.len() {
        match expanded[i].as_str() {
            "-o" => {
                out.push("-o".into());
                out.push(probe_bin.as_os_str().to_owned());
                replaced_o = true;
                i += 2;
                continue;
            }
            "-order_file" => {
                i += 2;
                continue;
            }
            "-x" | "-dead_strip" => {
                i += 1;
                continue;
            }
            _ => {}
        }
        out.push(expanded[i].clone().into());
        i += 1;
    }
    if !replaced_o {
        out.push("-o".into());
        out.push(probe_bin.as_os_str().to_owned());
    }
    out.push(pad_obj.as_os_str().to_owned());
    out.push("-order_file".into());
    out.push(order_file.as_os_str().to_owned());
    out
}

fn run_permutation_sieve(
    base_ordered: &[String],
    expanded: &[String],
) -> Option<Vec<String>> {
    const SEEDS: [u64; 5] = [
        0x9e37_79b9_7f4a_7c15,
        0xbf58_476d_1ce4_e5b9,
        0x94d0_49bb_1331_11eb,
        0xd6e8_feb8_6659_fd93,
        0xa076_1d64_78bd_642f,
    ];
    let pid = std::process::id();
    let order_dir = std::path::Path::new(ORDER_DIR);
    let pad_obj = order_dir.join(format!("aterm-pad-{pid}.o"));
    // 64 MiB `__text` spacers + 16 MiB `__const` spacers keep total `__TEXT,__text`
    // well below arm64's 128 MiB `BL` branch range while diluting hot-page density to ~0.30.
    let pad_names = macho_order::write_probe_spacer_object(&pad_obj, 16_384, 4_096, 4_096).ok()?;

    let probe_bins: Vec<Option<std::path::PathBuf>> = std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(SEEDS.len());
        for (k, &seed) in SEEDS.iter().enumerate() {
            let pad_ref = &pad_obj;
            let pad_names_ref = &pad_names;
            handles.push(scope.spawn(move || {
                let perm = macho_order::permute_order_list_with_spacers(
                    base_ordered,
                    pad_names_ref,
                    seed,
                );
                let order_k = order_dir.join(format!("aterm-probe-{pid}-{k}.order"));
                if macho_order::write_order_file(&order_k, &perm).is_err() {
                    return None;
                }
                let bin_k = order_dir.join(format!("aterm-probe-{pid}-{k}.bin"));
                let ld_args = build_probe_ld_args(expanded, &bin_k, pad_ref, &order_k);
                let ok = std::process::Command::new(REAL_LD)
                    .args(&ld_args)
                    .output()
                    .is_ok_and(|o| o.status.success())
                    && bin_k.is_file();
                let _ = std::fs::remove_file(&order_k);
                ok.then_some(bin_k)
            }));
        }
        handles.into_iter().map(|h| h.join().ok().flatten()).collect()
    });
    let _ = std::fs::remove_file(&pad_obj);

    let linked: Vec<(usize, std::path::PathBuf)> = probe_bins
        .into_iter()
        .enumerate()
        .filter_map(|(k, b)| b.map(|p| (k, p)))
        .collect();
    if linked.len() < 3 {
        return None;
    }

    // Separate inode for `ctl` requests so client CLI page faults never mark any
    // server probe binary's UBC `vm_object_t`.
    let ctl_bin = order_dir.join(format!("aterm-probe-ctl-{pid}.bin"));
    if std::fs::copy(&linked[0].1, &ctl_bin).is_err() {
        return None;
    }
    let _ = std::process::Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "-"])
        .arg(&ctl_bin)
        .output();
    let ctl_proved = std::process::Command::new(&ctl_bin)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success() && !o.stdout.is_empty());
    if !ctl_proved {
        return None;
    }

    let hit_sets: Vec<std::collections::HashSet<String>> = std::thread::scope(|scope| {
        let ctl_ref = &ctl_bin;
        let mut handles = Vec::with_capacity(linked.len());
        for (k, bin_k) in linked {
            handles.push(scope.spawn(move || run_one_probe(pid, k, &bin_k, ctl_ref)));
        }
        handles
            .into_iter()
            .filter_map(|h| h.join().ok().flatten())
            .filter(|set| set.len() > 100)
            .collect()
    });

    let _ = std::fs::remove_file(&ctl_bin);

    if hit_sets.len() < 3 {
        return None;
    }

    let refined = macho_order::refine_order_with_probe_hits(base_ordered, &hit_sets);
    if let Some(log_path) = std::env::var_os("ATERM_ORDER_PROBE_LOG") {
        let lens: Vec<usize> = hit_sets.iter().map(std::collections::HashSet::len).collect();
        let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for set in &hit_sets {
            for s in set {
                *counts.entry(s.as_str()).or_insert(0) += 1;
            }
        }
        let n = hit_sets.len();
        let b0 = counts.values().filter(|&&c| c == n).count();
        let b1 = counts.values().filter(|&&c| n >= 4 && c + 1 == n).count();
        let _ = std::fs::write(
            log_path,
            format!(
                "probes={n} total_syms={} hit_sets={lens:?} bucket0_all={b0} bucket1_near={b1}\n",
                base_ordered.len()
            ),
        );
    }
    Some(refined)
}

fn run_one_probe(
    pid: u32,
    k: usize,
    bin_k: &std::path::Path,
    ctl_bin: &std::path::Path,
) -> Option<std::collections::HashSet<String>> {
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    let root = std::path::PathBuf::from(format!("/tmp/.ubd/atord-{pid}-{k}.noindex"));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let cfg_dir = root.join("cfg/aterm");
    let run_dir = root.join("run");
    for d in [&home, &cfg_dir, &run_dir] {
        if std::fs::create_dir_all(d).is_err() {
            let _ = std::fs::remove_dir_all(&root);
            return None;
        }
        let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700));
    }
    let shrc = home.join(".shrc");
    let _ = std::fs::write(home.join(".profile"), "PS1='$ '\n");
    let _ = std::fs::write(&shrc, "PS1='$ '\n");
    let _ = std::fs::write(
        cfg_dir.join("aterm.toml"),
        "bell_sound = false\ntemporal_recording = true\n\
         [update]\nenabled = false\nauto_apply = false\n\
         [packages]\nenabled = false\n\
         [machine]\nuniversal_control = \"leave\"\n",
    );

    let sock = root.join("s.sock");
    let pages_out = root.join("pages.txt");
    let ready_file = root.join("ready");

    let mut child = match std::process::Command::new(bin_k)
        .env_clear()
        .env("HOME", &home)
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("SHELL", "/bin/sh")
        .env("ENV", &shrc)
        .env("TERM", "xterm-256color")
        .env("XDG_CONFIG_HOME", root.join("cfg"))
        .env("XDG_RUNTIME_DIR", &run_dir)
        .env("HISTFILE", "/dev/null")
        .env("DYLD_INSERT_LIBRARIES", PROBE_DYLIB)
        .env("ATERM_ORDER_PROBE_OUT", &pages_out)
        .env("ATERM_ORDER_PROBE_READY", &ready_file)
        .current_dir(&home)
        .args(["--headless", "--no-reroute", "--control-sock"])
        .arg(&sock)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => {
            let _ = std::fs::remove_dir_all(&root);
            return None;
        }
    };

    let deadline = Instant::now() + Duration::from_secs(12);
    let mut connected = false;
    while Instant::now() < deadline {
        if let Ok(Some(_)) = child.try_wait() {
            break;
        }
        if std::os::unix::net::UnixStream::connect(&sock).is_ok() {
            connected = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    if connected {
        let run_ctl = |extra: &[&str]| {
            let _ = std::process::Command::new(ctl_bin)
                .env_clear()
                .env("HOME", &home)
                .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
                .arg("ctl")
                .arg("--sock")
                .arg(&sock)
                .args(extra)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        };
        run_ctl(&["await", "idle", "200", "timeout=10000"]);
        run_ctl(&["temporal", "status"]);
        let _ = std::fs::write(&ready_file, b"1");
    }

    let exit_deadline = Instant::now() + Duration::from_secs(5);
    let mut exited = false;
    while Instant::now() < exit_deadline {
        if let Ok(Some(_)) = child.try_wait() {
            exited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if !exited {
        let _ = child.kill();
        let _ = child.wait();
    }

    let page_text = std::fs::read_to_string(&pages_out).unwrap_or_default();
    let proved_exec = exited && page_text.ends_with("OK\n");
    let _ = std::fs::remove_dir_all(&root);

    if !proved_exec {
        // Leave `bin_k` on disk if killed before proving execution (macOS syspolicyd rule).
        return None;
    }

    let mut faulted_pages = std::collections::HashSet::new();
    for line in page_text.lines() {
        if let Some(hex) = line.strip_prefix("0x")
            && let Ok(addr) = u64::from_str_radix(hex, 16)
        {
            faulted_pages.insert(addr);
        }
    }

    let hits = std::fs::read(bin_k)
        .ok()
        .map(|bytes| macho_order::extract_executable_faulted_symbols(&bytes, &faulted_pages));
    let _ = std::fs::remove_file(bin_k);
    hits
}
"##;

    let wrapper_src = format!("{header}{wrapper_body}");
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in order_mod_bytes.iter().chain(wrapper_src.as_bytes()) {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }

    let ld_bin = out_dir.join(format!("aterm-order-ld-{hash:016x}"));
    let self_check_ok = |bin: &std::path::Path| -> bool {
        bin.is_file()
            && run(&bin.to_string_lossy(), &["--aterm-self-check"])
                .is_some_and(|out| out == "aterm-order-ld ok")
    };

    if self_check_ok(&ld_bin) {
        println!("cargo:rustc-link-arg-bins=--ld-path={}", ld_bin.display());
        return;
    }

    let src_path = out_dir.join(format!("macho_order_ld_{hash:016x}.rs"));
    if std::fs::write(&src_path, wrapper_src).is_err() {
        println!("cargo:rustc-link-arg-bins=-Wl,-x");
        return;
    }

    let mut cmd = std::process::Command::new(&rustc);
    if compiler_flavor == "t" {
        cmd.arg("-Ztrust-verify=off");
    }
    cmd.args(["--edition=2024", "-Copt-level=2", "-o"])
        .arg(&ld_bin)
        .arg(&src_path);

    if cmd.output().is_ok_and(|o| o.status.success()) && self_check_ok(&ld_bin) {
        println!("cargo:rustc-link-arg-bins=--ld-path={}", ld_bin.display());
    } else {
        println!("cargo:rustc-link-arg-bins=-Wl,-x");
    }
}

/// Builds the pure-Rust `DYLD_INSERT_LIBRARIES` probe `cdylib` used by `aterm-order-ld`
/// to query `VM_PAGE_QUERY_PAGE_CS_VALIDATED` (`0x100`) pages on `__TEXT` and `__DATA_CONST`
/// after headless startup reaches idle.
fn build_macos_order_probe_dylib(
    out_dir: &std::path::Path,
    rustc: &str,
    compiler_flavor: &str,
) -> Option<std::path::PathBuf> {
    let probe_src = r##"// GENERATED by crates/aterm/build.rs -- do not edit.
use std::ffi::{c_char, c_int, c_uint, c_void};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

const MH_MAGIC_64: u32 = 0xfeed_facf;
const MH_EXECUTE: u32 = 2;
const LC_SEGMENT_64: u32 = 0x19;
const VM_PAGE_QUERY_PAGE_CS_VALIDATED: c_int = 0x100;
const PAGE_BYTES: u64 = 16_384;

#[repr(C)]
struct MachHeader64 {
    magic: u32,
    cputype: i32,
    cpusubtype: i32,
    filetype: u32,
    ncmds: u32,
    sizeofcmds: u32,
    flags: u32,
    reserved: u32,
}

#[repr(C)]
struct LoadCommand {
    cmd: u32,
    cmdsize: u32,
}

#[repr(C)]
struct SegmentCommand64 {
    cmd: u32,
    cmdsize: u32,
    segname: [u8; 16],
    vmaddr: u64,
    vmsize: u64,
    fileoff: u64,
    filesize: u64,
    maxprot: i32,
    initprot: i32,
    nsects: u32,
    flags: u32,
}

unsafe extern "C" {
    fn _dyld_image_count() -> u32;
    fn _dyld_get_image_header(image_index: u32) -> *const MachHeader64;
    fn _dyld_get_image_vmaddr_slide(image_index: u32) -> isize;
    fn mach_task_self() -> c_uint;
    fn mach_vm_page_range_query(
        target_map: c_uint,
        address: u64,
        size: u64,
        dispositions: *mut c_int,
        dispositions_count: *mut u64,
    ) -> c_int;
    fn unsetenv(name: *const c_char) -> c_int;
    fn _exit(status: c_int) -> !;
}

#[used]
#[unsafe(link_section = "__DATA,__mod_init_func")]
static INIT: extern "C" fn() = init_probe;

extern "C" fn init_probe() {
    let out_os = std::env::var_os("ATERM_ORDER_PROBE_OUT");
    let ready_os = std::env::var_os("ATERM_ORDER_PROBE_READY");
    // Strip probe variables immediately before `main()` so child shells never inherit them.
    unsafe {
        let _ = unsetenv(c"DYLD_INSERT_LIBRARIES".as_ptr());
        let _ = unsetenv(c"ATERM_ORDER_PROBE_OUT".as_ptr());
        let _ = unsetenv(c"ATERM_ORDER_PROBE_READY".as_ptr());
    }
    let (Some(out_os), Some(ready_os)) = (out_os, ready_os) else {
        return;
    };
    let out_pb = PathBuf::from(out_os);
    let ready_pb = PathBuf::from(ready_os);
    let _ = std::thread::Builder::new()
        .name("at-ord-probe".into())
        .stack_size(256 * 1024)
        .spawn(move || {
            for _ in 0..1000 {
                if ready_pb.exists() {
                    dump_validated_pages(&out_pb);
                    unsafe { _exit(0) };
                }
                std::thread::sleep(Duration::from_millis(15));
            }
            let _ = &(0 as *const c_void);
        });
}

fn seg_name(bytes: &[u8; 16]) -> &[u8] {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    &bytes[..end]
}

fn dump_validated_pages(out_path: &Path) {
    let Ok(file) = std::fs::File::create(out_path) else {
        return;
    };
    let mut w = std::io::BufWriter::new(file);
    unsafe {
        let count = _dyld_image_count();
        for idx in 0..count {
            let hdr = _dyld_get_image_header(idx);
            if hdr.is_null() || (*hdr).magic != MH_MAGIC_64 || (*hdr).filetype != MH_EXECUTE {
                continue;
            }
            let slide = _dyld_get_image_vmaddr_slide(idx) as i64;
            let mut lc_ptr = (hdr as *const u8).add(std::mem::size_of::<MachHeader64>());
            for _ in 0..(*hdr).ncmds {
                let lc = &*(lc_ptr as *const LoadCommand);
                if lc.cmdsize < 8 {
                    break;
                }
                if lc.cmd == LC_SEGMENT_64
                    && (lc.cmdsize as usize) >= std::mem::size_of::<SegmentCommand64>()
                {
                    let seg = &*(lc_ptr as *const SegmentCommand64);
                    let name = seg_name(&seg.segname);
                    if (name == b"__TEXT" || name == b"__DATA_CONST") && seg.vmsize > 0 {
                        query_segment_pages(&mut w, seg.vmaddr, seg.vmsize, slide);
                    }
                }
                lc_ptr = lc_ptr.add(lc.cmdsize as usize);
            }
            break;
        }
    }
    let _ = w.write_all(b"OK\n");
    let _ = w.flush();
}

unsafe fn query_segment_pages(
    w: &mut impl Write,
    unslid_vmaddr: u64,
    vmsize: u64,
    slide: i64,
) {
    let total_pages = vmsize.div_ceil(PAGE_BYTES);
    let mut disp = [0 as c_int; 512];
    let mut page_idx = 0u64;
    while page_idx < total_pages {
        let batch = (total_pages - page_idx).min(512);
        let unslid_base = unslid_vmaddr + page_idx * PAGE_BYTES;
        let slid_base = (unslid_base as i64 + slide) as u64;
        let mut queried = batch;
        let kr = unsafe {
            mach_vm_page_range_query(
                mach_task_self(),
                slid_base,
                batch * PAGE_BYTES,
                disp.as_mut_ptr(),
                &mut queried,
            )
        };
        if kr == 0 {
            for k in 0..(queried as usize).min(disp.len()) {
                if (disp[k] & VM_PAGE_QUERY_PAGE_CS_VALIDATED) != 0 {
                    let _ = writeln!(w, "0x{:x}", unslid_base + (k as u64) * PAGE_BYTES);
                }
            }
        }
        page_idx += batch;
    }
}
"##;

    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in probe_src.as_bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }

    let dylib_path = out_dir.join(format!("libaterm_order_probe_{hash:016x}.dylib"));
    if dylib_path.is_file() {
        return Some(dylib_path);
    }

    let src_path = out_dir.join(format!("probe_cdylib_{hash:016x}.rs"));
    std::fs::write(&src_path, probe_src).ok()?;

    let mut cmd = std::process::Command::new(rustc);
    if compiler_flavor == "t" {
        cmd.arg("-Ztrust-verify=off");
    }
    cmd.args([
        "--edition=2024",
        "--crate-type=cdylib",
        "-Copt-level=2",
        "-o",
    ])
    .arg(&dylib_path)
    .arg(&src_path);

    (cmd.output().is_ok_and(|o| o.status.success()) && dylib_path.is_file()).then_some(dylib_path)
}

/// Print the "this exe will look unfinished" banner and honour `ATERM_REQUIRE_WIN_RESOURCES`.
fn report(why: &str) {
    for line in [
        "-----------------------------------------------------------------".to_string(),
        "aterm: the Windows executable is being built WITHOUT its resources".to_string(),
        format!("  cause: {why}"),
        "  visible result: the generic Windows exe icon in the title bar, in".to_string(),
        "    Alt-Tab and in Task View; a bare filename (no description, no".to_string(),
        "    version) in Task Manager, Explorer and Apps & Features; and off".to_string(),
        "    MSVC, classic grey 3D dialog buttons instead of Common-Controls v6.".to_string(),
        "  fix: install a resource compiler — the Windows SDK's rc.exe (it is".to_string(),
        "    on PATH inside a VS Developer prompt / vcvars64) or LLVM's".to_string(),
        "    llvm-rc — or point ATERM_RC at one — then rebuild.".to_string(),
        "  set ATERM_REQUIRE_WIN_RESOURCES=1 to make this a hard build error".to_string(),
        "    instead of a warning (release cuts and CI should).".to_string(),
        "-----------------------------------------------------------------".to_string(),
    ] {
        println!("cargo:warning={line}");
    }

    let required = std::env::var("ATERM_REQUIRE_WIN_RESOURCES")
        .map(|v| !matches!(v.as_str(), "" | "0" | "false" | "no"))
        .unwrap_or(false);
    if required {
        panic!("ATERM_REQUIRE_WIN_RESOURCES is set and the Windows resources did not link: {why}");
    }
}

/// Build the `.rc` script text from `CARGO_PKG_VERSION_*`.
fn resource_script(icon: &std::path::Path, app_manifest: Option<&std::path::Path>) -> String {
    let major = env_num("CARGO_PKG_VERSION_MAJOR");
    let minor = env_num("CARGO_PKG_VERSION_MINOR");
    let patch = env_num("CARGO_PKG_VERSION_PATCH");
    let display = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());

    let quad = format!("{major},{minor},{patch},0");

    const VS_FF_DEBUG: u32 = 0x1;
    const VS_FF_PRERELEASE: u32 = 0x2;
    let mut flags = 0u32;
    if std::env::var("PROFILE").as_deref() == Ok("debug") {
        flags |= VS_FF_DEBUG;
    }
    if !std::env::var("CARGO_PKG_VERSION_PRE")
        .unwrap_or_default()
        .is_empty()
    {
        flags |= VS_FF_PRERELEASE;
    }

    format!(
        r#"// GENERATED by crates/aterm/build.rs -- do not edit, do not check in.
// Regenerated on every build; see that file for why each entry is here.
// Kept strictly ASCII: an .rc without a UTF-16 BOM is decoded by rc.exe in the
// host ANSI codepage, so a stray non-ASCII byte anywhere -- even in a comment --
// decodes differently on a machine with a different locale.

{RESOURCE_ID} ICON "{icon}"
{manifest}

// The id is the literal 1, NOT the VS_VERSION_INFO macro -- see the const's doc
// comment in build.rs. Without <winver.h> the macro becomes a resource NAME and
// GetFileVersionInfo silently finds nothing.
{VS_VERSION_INFO_ID} VERSIONINFO
FILEVERSION     {quad}
PRODUCTVERSION  {quad}
FILEFLAGSMASK   0x3fL
FILEFLAGS       0x{flags:x}L
FILEOS          0x40004L        // VOS_NT_WINDOWS32
FILETYPE        0x1L            // VFT_APP
FILESUBTYPE     0x0L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        // 0409 = en-US, 04b0 = 1200 = Unicode. Must agree with the
        // Translation pair below or the shell finds no strings at all.
        BLOCK "040904b0"
        BEGIN
            VALUE "CompanyName",      "Andrew Yates"
            VALUE "FileDescription",  "aterm - the transparent terminal"
            VALUE "FileVersion",      "{display}"
            VALUE "InternalName",     "aterm"
            VALUE "LegalCopyright",   "Copyright (C) 2026 Andrew Yates. Licensed under Apache-2.0."
            // Every installed CLI alias (aterm-ctl.exe, atpkg.exe, ...) is a
            // HARDLINK onto the console image, so they all report aterm.exe
            // here, truthfully. The windowed image (aterm-windowed.exe, shipped
            // as aterm-gui.exe) reports it too: one .rc serves both bins, and a
            // per-bin resource would need a second rc compile + link-arg-bin
            // pair to change one cosmetic string Explorer does not show.
            VALUE "OriginalFilename", "aterm.exe"
            VALUE "ProductName",      "aterm"
            VALUE "ProductVersion",   "{display}"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x409, 1200
    END
END
"#,
        icon = rc_path_literal(icon),
        manifest = app_manifest.map_or_else(String::new, |m| format!(
            "{RESOURCE_ID} {RT_MANIFEST} \"{}\"",
            rc_path_literal(m)
        )),
    )
}

/// Format a path for an `.rc` string literal with `/` separators.
fn rc_path_literal(p: &std::path::Path) -> String {
    p.display().to_string().replace('\\', "/")
}

/// A `CARGO_PKG_VERSION_*` component clamped to `u16`.
fn env_num(key: &str) -> u16 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .map(|v| v.min(u32::from(u16::MAX)) as u16)
        .unwrap_or(0)
}

/// The system's git, by the path `aterm_uds::host::TOOLS`'s `git` row names (a build script links
/// no workspace crate): never whichever git a `PATH` finds first. Off Unix the bare name, as
/// there.
const GIT: &str = if cfg!(unix) { "/usr/bin/git" } else { "git" };

/// `unix` seconds as RFC 3339 UTC (`2026-10-22T00:00:00Z`), as `date -u +%Y-%m-%dT%H:%M:%SZ`
/// prints it, computed here (the civil calendar of Hinnant's `civil_from_days`, as
/// `aterm_types::rfc3339` has it), so no `date` runs.
fn rfc3339_utc(unix: i64) -> String {
    let (days, secs) = (unix.div_euclid(86_400), unix.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

/// Run `cmd args…`, its trimmed stdout when it succeeded with some, else `None`.
fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(cmd).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

mod build_stamp {
    use super::{GIT, rfc3339_utc, run};
    use std::path::PathBuf;

    /// A build fact supplied by the caller instead of probed from git. Empty is
    /// treated as absent, so `FOO=` cannot half-pin a build.
    fn pinned(key: &str) -> Option<String> {
        println!("cargo:rerun-if-env-changed={key}");
        std::env::var(key).ok().filter(|v| !v.trim().is_empty())
    }

    /// Emits the git commit stamp and `rerun-if-changed` directives.
    pub fn emit() {
        let pinned_commit = pinned("ATERM_BUILD_GIT_COMMIT");
        let pinned_full_commit = pinned("ATERM_BUILD_GIT_COMMIT_FULL");
        let pinned_dev_commits = pinned("ATERM_BUILD_DEV_COMMITS");
        let git_is_pinned =
            pinned_commit.is_some() && pinned_full_commit.is_some() && pinned_dev_commits.is_some();

        let commit = match &pinned_commit {
            Some(commit) => commit.clone(),
            None => {
                let commit = run(GIT, &["rev-parse", "--short=12", "HEAD"])
                    .unwrap_or_else(|| "unknown".into());
                let dirty = run(GIT, &["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
                if commit != "unknown" && dirty {
                    format!("{commit}-dirty")
                } else {
                    commit
                }
            }
        };
        println!("cargo:rustc-env=ATERM_GIT_COMMIT={commit}");
        let full_commit = pinned_full_commit.unwrap_or_else(|| {
            run(GIT, &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into())
        });
        let dirty = commit.ends_with("-dirty");
        println!("cargo:rustc-env=ATERM_GIT_COMMIT_FULL={full_commit}");
        println!("cargo:rustc-env=ATERM_GIT_DIRTY={dirty}");

        let dev_commits = pinned_dev_commits.unwrap_or_else(|| {
            run(
                GIT,
                &["describe", "--tags", "--match", "v*.*.0", "--abbrev=0"],
            )
            .and_then(|tag| {
                run(
                    GIT,
                    &["rev-list", &format!("{}..HEAD", tag.trim()), "--count"],
                )
            })
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "0".into())
        });
        println!("cargo:rustc-env=ATERM_DEV_COMMITS={dev_commits}");

        let is_epoch = |s: &String| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        let build_number = std::env::var("SOURCE_DATE_EPOCH")
            .ok()
            .filter(is_epoch)
            .or_else(|| run(GIT, &["show", "-s", "--format=%ct", "HEAD"]).filter(is_epoch))
            .unwrap_or_else(|| "0".into());
        println!("cargo:rustc-env=ATERM_BUILD_NUMBER={build_number}");

        let build_time = match std::env::var("SOURCE_DATE_EPOCH") {
            Ok(epoch) if !epoch.is_empty() => epoch.parse::<i64>().ok().map(rfc3339_utc),
            _ => std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .and_then(|d| i64::try_from(d.as_secs()).ok())
                .map(rfc3339_utc),
        }
        .unwrap_or_else(|| "unknown".into());
        println!("cargo:rustc-env=ATERM_BUILD_TIME={build_time}");

        if !git_is_pinned {
            for path in git_watch_paths().into_iter().chain(tracked_files()) {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
        println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    }

    /// Resolve a `git rev-parse` path to an absolute [`PathBuf`].
    fn git_abs_path(args: &[&str]) -> Option<PathBuf> {
        let mut with_format = vec!["rev-parse", "--path-format=absolute"];
        with_format.extend_from_slice(args);
        let value = run(GIT, &with_format).or_else(|| {
            let mut plain = vec!["rev-parse"];
            plain.extend_from_slice(args);
            run(GIT, &plain)
        })?;
        let path = PathBuf::from(value);
        if path.is_absolute() {
            Some(path)
        } else {
            std::env::current_dir().ok().map(|cwd| cwd.join(path))
        }
    }

    /// Git files whose modification may invalidate the commit stamp.
    fn git_watch_paths() -> Vec<PathBuf> {
        let mut paths = Vec::new();
        let Some(git_dir) = run(GIT, &["rev-parse", "--absolute-git-dir"]).map(PathBuf::from)
        else {
            return paths;
        };
        let common_dir = git_abs_path(&["--git-common-dir"]).unwrap_or_else(|| git_dir.clone());

        paths.push(git_dir.join("HEAD"));

        if let Some(reference) = run(GIT, &["symbolic-ref", "--quiet", "HEAD"])
            && let (Some(ref_path), Some(refs_root)) = (
                git_abs_path(&["--git-path", &reference]),
                git_abs_path(&["--git-path", "refs"]),
            )
            && let Some(watch) = ref_path
                .ancestors()
                .take_while(|p| p.starts_with(&refs_root))
                .find(|p| p.exists())
        {
            paths.push(watch.to_path_buf());
        }

        paths.push(common_dir.join("packed-refs"));
        paths.push(common_dir.join("reftable"));

        paths.retain(|p| p.exists());
        paths.sort();
        paths.dedup();
        paths
    }

    /// Every file git tracks in this checkout and its submodules that exists as a file and has no newline in its name, absolute, so an edit to any of them reruns the script and no build after an edit keeps the clean stamp of the build before it.
    fn tracked_files() -> Vec<PathBuf> {
        let Some(top) = run(GIT, &["rev-parse", "--show-toplevel"]).map(PathBuf::from) else {
            return Vec::new();
        };
        let Ok(out) = std::process::Command::new(GIT)
            .arg("-C")
            .arg(&top)
            .args(["ls-files", "-z", "--recurse-submodules"])
            .output()
        else {
            return Vec::new();
        };
        if !out.status.success() {
            return Vec::new();
        }
        String::from_utf8_lossy(&out.stdout)
            .split('\0')
            .filter(|name| !name.is_empty() && !name.contains('\n'))
            .map(|name| top.join(name))
            .filter(|path| path.is_file())
            .collect()
    }
}

mod compiler {
    use super::run;

    include!("src/compiler_probe.rs");

    /// Emits compiler, profile, and verification build facts. Returns the detected flavor.
    pub fn emit() -> &'static str {
        let compiler_path = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
        let vv = run(&compiler_path, &["-vV"]).unwrap_or_default();
        let compiler = parse_rustc_vv(&vv);
        let toolchain = std::env::var("RUSTUP_TOOLCHAIN").ok();
        let flavor = detect_flavor(&vv, &compiler_path, toolchain.as_deref());
        println!(
            "cargo:rustc-env=ATERM_COMPILER_VERSION_LINE={}",
            compiler.version_line
        );
        println!("cargo:rustc-env=ATERM_COMPILER_COMMIT={}", compiler.commit);
        println!("cargo:rustc-env=ATERM_COMPILER_HOST={}", compiler.host);
        println!(
            "cargo:rustc-env=ATERM_COMPILER_TRUST_VERSION={}",
            compiler.trust_version
        );
        println!("cargo:rustc-env=ATERM_COMPILER_FLAVOR={flavor}");
        println!(
            "cargo:rustc-env=ATERM_BUILD_PROFILE={}",
            std::env::var("PROFILE").unwrap_or_else(|_| "unknown".into())
        );
        println!(
            "cargo:rustc-env=ATERM_TRUST_VERIFY={}",
            trust_verify_state()
        );
        println!("cargo:rerun-if-env-changed=CARGO_ENCODED_RUSTFLAGS");
        println!("cargo:rerun-if-env-changed=RUSTC");
        println!("cargo:rerun-if-env-changed=RUSTUP_TOOLCHAIN");
        flavor
    }

    /// `"on"` iff this compile really runs the Trust verification pipeline.
    fn trust_verify_state() -> &'static str {
        fn from_value(flag: &str) -> Option<&'static str> {
            if let Some(v) = flag.strip_prefix("trust-verify=") {
                return Some(if v.eq_ignore_ascii_case("off") {
                    "off"
                } else {
                    "on"
                });
            }
            if let Some(v) = flag.strip_prefix("no-trust-verify=") {
                return Some(if v.eq_ignore_ascii_case("yes") {
                    "off"
                } else {
                    "on"
                });
            }
            None
        }

        if let Some(encoded) = std::env::var_os("CARGO_ENCODED_RUSTFLAGS") {
            let encoded = encoded.to_string_lossy().into_owned();
            let flags: Vec<&str> = encoded
                .split('\u{1f}')
                .filter(|flag| !flag.is_empty())
                .collect();
            let mut expect_z_value = false;
            for flag in flags {
                if expect_z_value {
                    expect_z_value = false;
                    if let Some(state) = from_value(flag) {
                        return state;
                    }
                    continue;
                }
                if flag == "-Z" {
                    expect_z_value = true;
                    continue;
                }
                if let Some(rest) = flag.strip_prefix("-Z")
                    && let Some(state) = from_value(rest)
                {
                    return state;
                }
            }
        }

        if std::env::var_os("CARGO_CFG_TRUST_VERIFY").is_some() {
            "on"
        } else {
            "off"
        }
    }
}
