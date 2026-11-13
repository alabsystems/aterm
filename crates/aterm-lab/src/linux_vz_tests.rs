// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn stock() -> Stock {
    Stock {
        rustup: "/opt/rustup/bin/rustup".into(),
        name: "1.97.1".into(),
        rustc: "/r/toolchains/1.97.1-aarch64-apple-darwin/bin/rustc".into(),
        sysroot: "/r/toolchains/1.97.1-aarch64-apple-darwin".into(),
        host: "aarch64-apple-darwin".into(),
    }
}

fn recipe(inherited: &[&str]) -> Recipe {
    let inherited: Vec<String> = inherited.iter().map(|s| (*s).to_string()).collect();
    agent_recipe(
        &stock(),
        Path::new("/src/aterm"),
        Path::new("/src/aterm/target/.ubd/vz-agent.noindex"),
        &inherited,
    )
    .expect("a recipe")
}

#[test]
fn the_recipe_is_the_designs_stock_cross_build() {
    let r = recipe(&[]);
    assert_eq!(r.program, Path::new("/opt/rustup/bin/rustup"));
    assert_eq!(
        r.args,
        [
            "run",
            "1.97.1",
            "cargo",
            "build",
            "--locked",
            "-p",
            "aterm-vz-agent",
            "--bin",
            "aterm-vz-agent",
            "--release",
            "--target",
            "aarch64-unknown-linux-musl",
            "--manifest-path",
            "/src/aterm/Cargo.toml",
            "--config",
            "target.aarch64-unknown-linux-musl.linker=\"/r/toolchains/1.97.1-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/bin/rust-lld\"",
            "--config",
            "target.aarch64-unknown-linux-musl.rustflags=[\"-C\", \"linker-flavor=ld.lld\", \"-C\", \"link-self-contained=yes\"]",
        ]
    );
    assert_eq!(r.cwd, Path::new("/src/aterm"));
    // The cross triple, never the host's.
    assert!(
        !r.args
            .iter()
            .any(|a| a.contains("apple-darwin") && !a.contains("rust-lld"))
    );
}

#[test]
fn the_recipe_sets_its_compiler_and_target_dir_and_never_rustflags() {
    let r = recipe(&[]);
    let set: Vec<&str> = r.set.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(set, ["RUSTC", "CARGO_TARGET_DIR", "ATERM_STOCK_REASON"]);
    assert_eq!(
        r.set[0].1,
        OsString::from("/r/toolchains/1.97.1-aarch64-apple-darwin/bin/rustc")
    );
    assert_eq!(
        r.set[1].1,
        OsString::from("/src/aterm/target/.ubd/vz-agent.noindex")
    );
    assert!(r.set[2].1.to_string_lossy().starts_with("STOCK EXCEPTION"));
    for flags in [
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTDOCFLAGS",
        "CARGO_BUILD_RUSTFLAGS",
    ] {
        assert!(!set.contains(&flags), "{flags} is set");
        assert!(r.remove.iter().any(|v| v == flags), "{flags} is inherited");
    }
    for steer in [
        "RUSTC_WRAPPER",
        "RUSTUP_TOOLCHAIN",
        "CARGO_BUILD_TARGET",
        "CARGO",
    ] {
        assert!(r.remove.iter().any(|v| v == steer), "{steer} is inherited");
    }
}

#[test]
fn inherited_per_target_and_release_profile_variables_are_removed_and_no_others() {
    let r = recipe(&[
        "PATH",
        "HOME",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER",
        "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_RUSTFLAGS",
        "CARGO_PROFILE_RELEASE_LTO",
        "CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER",
        "RUSTFLAGS",
    ]);
    for gone in [
        "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER",
        "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_RUSTFLAGS",
        "CARGO_PROFILE_RELEASE_LTO",
    ] {
        assert!(r.remove.iter().any(|v| v == gone), "{gone}");
    }
    for kept in [
        "PATH",
        "HOME",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER",
    ] {
        assert!(!r.remove.iter().any(|v| v == kept), "{kept}");
    }
    assert_eq!(r.remove.iter().filter(|v| *v == "RUSTFLAGS").count(), 1);
}

#[test]
fn a_linker_path_is_quoted_as_toml_or_refused() {
    let mut s = stock();
    s.sysroot = r#"/odd "dir"\x"#.into();
    let r = agent_recipe(&s, Path::new("/src"), Path::new("/t"), &[]).expect("quoted");
    assert!(r.args.contains(
        &r#"target.aarch64-unknown-linux-musl.linker="/odd \"dir\"\\x/lib/rustlib/aarch64-apple-darwin/bin/rust-lld""#
            .to_string()
    ));
    s.sysroot = "/a\nb".into();
    assert!(agent_recipe(&s, Path::new("/src"), Path::new("/t"), &[]).is_err());
}

#[test]
fn the_hosts_triple_is_read_from_rustc_verbose_version() {
    let text = "rustc 1.99.1 (aaaa 2026-10-15)\nbinary: rustc\nhost: aarch64-apple-darwin\nrelease: 1.99.1\n";
    assert_eq!(host_of(text).as_deref(), Some("aarch64-apple-darwin"));
    assert_eq!(host_of("rustc 1.99.1\n"), None);
    assert_eq!(host_of("host: \n"), None);
}

#[test]
fn a_missing_std_or_linker_is_named_by_its_path() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let s = Stock {
        sysroot: dir.path().to_path_buf(),
        ..stock()
    };
    let why = s.ready(AGENT_TRIPLE).expect_err("no std");
    assert!(why.contains("no aarch64-unknown-linux-musl std"), "{why}");
    std::fs::create_dir_all(s.std_dir(AGENT_TRIPLE)).expect("std dir");
    let why = s.ready(AGENT_TRIPLE).expect_err("no lld");
    assert!(why.contains("no rust-lld"), "{why}");
    let lld = s.rust_lld();
    std::fs::create_dir_all(lld.parent().expect("bin")).expect("bin dir");
    std::fs::write(&lld, b"").expect("lld");
    std::fs::set_permissions(&lld, std::fs::Permissions::from_mode(0o755)).expect("mode");
    assert_eq!(s.ready(AGENT_TRIPLE), Ok(()));
}

/// A minimal 64-bit little-endian ELF header of `kind` for `machine`, with one program
/// header of `segment`.
fn elf(kind: u16, machine: u16, segment: u32) -> Vec<u8> {
    let mut b = vec![0u8; 64 + 56];
    b[..4].copy_from_slice(b"\x7fELF");
    b[4] = 2;
    b[5] = 1;
    b[6] = 1;
    b[16..18].copy_from_slice(&kind.to_le_bytes());
    b[18..20].copy_from_slice(&machine.to_le_bytes());
    b[32..40].copy_from_slice(&64u64.to_le_bytes());
    b[54..56].copy_from_slice(&56u16.to_le_bytes());
    b[56..58].copy_from_slice(&1u16.to_le_bytes());
    b[64..68].copy_from_slice(&segment.to_le_bytes());
    b
}

#[test]
fn only_a_static_aarch64_elf_is_a_guest_agent() {
    // PT_LOAD (1) and PT_DYNAMIC (2: a static-pie's own relocations) need no loader.
    assert_eq!(not_a_static_guest_elf(&elf(2, 183, 1)), None);
    assert_eq!(not_a_static_guest_elf(&elf(3, 183, 2)), None);
    let why = |b: &[u8]| not_a_static_guest_elf(b).expect("refused");
    assert!(why(&elf(3, 183, 3)).contains("dynamically linked"));
    assert!(why(&elf(2, 62, 1)).contains("aarch64"));
    assert!(why(&elf(1, 183, 1)).contains("executable"));
    assert!(why(b"\xcf\xfa\xed\xfe\x0c\x00\x00\x01").contains("not an ELF"));
    let mut thirty_two = elf(2, 183, 1);
    thirty_two[4] = 1;
    assert!(why(&thirty_two).contains("64-bit"));
    assert!(why(&elf(2, 183, 1)[..40]).contains("truncated"));
    let mut far = elf(2, 183, 1);
    far[32..40].copy_from_slice(&4096u64.to_le_bytes());
    assert!(why(&far).contains("past the file"));
}

/// A malformed or truncated program-header table, or one whose later entry names a loader, is
/// refused before anything is placed.
#[test]
fn malformed_program_headers_are_refused_before_an_agent_is_placed() {
    let good = elf(2, 183, 1);
    assert_eq!(not_a_static_guest_elf(&good), None);
    assert!(not_a_static_guest_elf(&elf(2, 183, 3)).is_some());
    let with = |offset: u64, stride: u16, count: u16| {
        let mut bytes = good.clone();
        bytes[32..40].copy_from_slice(&offset.to_le_bytes());
        bytes[54..56].copy_from_slice(&stride.to_le_bytes());
        bytes[56..58].copy_from_slice(&count.to_le_bytes());
        bytes
    };
    let mut hidden_loader = good.clone();
    hidden_loader.extend_from_slice(&[0u8; 56]);
    hidden_loader[120..124].copy_from_slice(&3u32.to_le_bytes());
    hidden_loader[56..58].copy_from_slice(&2u16.to_le_bytes());
    assert!(not_a_static_guest_elf(&hidden_loader).is_some());
    let malformed = [
        with(u64::MAX, 56, 1),
        with(u64::MAX - 55, 56, 2),
        with(64, 0, 1),
        with(64, 4, 1),
        with(64, 56, 0),
        with(0, 56, 1),
        good[..58].to_vec(),
        good[..68].to_vec(),
        hidden_loader[..124].to_vec(),
    ];
    let scratch = aterm_tempfile::tempdir().expect("private malformed ELF fixtures");
    let built = scratch.path().join("built");
    let vz = scratch.path().join("vz.noindex");
    for bytes in malformed {
        assert!(not_a_static_guest_elf(&bytes).is_some());
        std::fs::write(&built, &bytes).expect("fixture");
        assert!(place(&built, &vz).is_err());
        assert!(!vz.exists(), "a refused file creates no agent store");
    }
}

#[test]
fn the_agent_is_placed_under_its_hash_read_only_and_kept_when_there() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let built = scratch.path().join("built");
    let bytes = elf(3, 183, 1);
    std::fs::write(&built, &bytes).expect("built");
    let vz = scratch.path().join("vz.noindex");

    let first = place(&built, &vz).expect("placed");
    assert!(first.fresh);
    assert_eq!(first.sha256, sha256_hex(&bytes));
    assert_eq!(first.sha256.len(), 64);
    assert_eq!(
        first.path,
        vz.join("agent").join(&first.sha256).join("aterm-vz-agent")
    );
    assert_eq!(first.bytes, bytes.len() as u64);
    assert_eq!(std::fs::read(&first.path).expect("read"), bytes);
    let mode = std::fs::metadata(&first.path)
        .expect("meta")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o555);
    let names: Vec<String> = std::fs::read_dir(vz.join("agent"))
        .expect("agents")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        std::slice::from_ref(&first.sha256),
        "no partial left"
    );

    let again = place(&built, &vz).expect("again");
    assert!(!again.fresh);
    assert_eq!(again.path, first.path);

    // A second agent goes beside the first: a running guest's agent is never overwritten.
    let other = elf(2, 183, 1);
    std::fs::write(&built, &other).expect("rebuilt");
    let second = place(&built, &vz).expect("second");
    assert!(second.fresh);
    assert_ne!(second.sha256, first.sha256);
    assert_eq!(std::fs::read(&first.path).expect("first kept"), bytes);
}

#[test]
fn a_hash_directory_that_does_not_hold_its_agent_is_replaced() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let built = scratch.path().join("built");
    let bytes = elf(3, 183, 1);
    std::fs::write(&built, &bytes).expect("built");
    let vz = scratch.path().join("vz.noindex");
    let dir = vz.join("agent").join(sha256_hex(&bytes));
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(dir.join("aterm-vz-agent"), b"cut short").expect("stale");

    let placed = place(&built, &vz).expect("placed");
    assert!(placed.fresh);
    assert_eq!(std::fs::read(&placed.path).expect("read"), bytes);
}

/// The same agent placed by another build before the look or before the rename is kept, and
/// this one's partial goes.
#[test]
fn a_copy_another_build_placed_meanwhile_is_kept() {
    use std::os::unix::fs::MetadataExt as _;
    for race in [Race::BeforeLook, Race::BeforeRename] {
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let built = scratch.path().join("built");
        let bytes = elf(3, 183, 1);
        std::fs::write(&built, &bytes).expect("built");
        let vz = scratch.path().join("vz.noindex");
        let mut theirs = None;
        let placed = place_racing(&built, &vz, &mut |at, dir| {
            if at == race {
                std::fs::create_dir(dir).expect("their directory");
                let agent = dir.join(AGENT);
                std::fs::write(&agent, &bytes).expect("their copy");
                theirs = Some(std::fs::metadata(&agent).expect("meta").ino());
            }
        })
        .unwrap_or_else(|why| panic!("{race:?}: {why}"));
        assert!(!placed.fresh, "{race:?}");
        let kept = std::fs::metadata(&placed.path).expect("kept").ino();
        assert_eq!(Some(kept), theirs, "{race:?}: their copy, not this one's");
        let names: Vec<String> = std::fs::read_dir(vz.join("agent"))
            .expect("agents")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            std::slice::from_ref(&placed.sha256),
            "{race:?}: no partial left"
        );
    }
}

/// Negative control: a directory without this agent that appears before the rename fails it.
#[test]
fn a_rename_lost_to_a_directory_without_the_agent_fails() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let built = scratch.path().join("built");
    std::fs::write(&built, elf(3, 183, 1)).expect("built");
    let vz = scratch.path().join("vz.noindex");
    let why = place_racing(&built, &vz, &mut |at, dir| {
        if at == Race::BeforeRename {
            std::fs::create_dir(dir).expect("a directory");
            std::fs::write(dir.join(AGENT), b"another agent").expect("another");
        }
    })
    .expect_err("not this agent");
    assert!(why.contains("could not place"), "{why}");
}

#[test]
fn a_build_that_is_not_a_guest_agent_is_never_placed() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let built = scratch.path().join("built");
    std::fs::write(&built, elf(3, 183, 3)).expect("built");
    let vz = scratch.path().join("vz.noindex");
    let why = place(&built, &vz).expect_err("dynamic");
    assert!(why.contains("dynamically linked"), "{why}");
    assert!(!vz.exists());
    let why = place(&scratch.path().join("absent"), &vz).expect_err("absent");
    assert!(why.contains("could not read"), "{why}");
}
