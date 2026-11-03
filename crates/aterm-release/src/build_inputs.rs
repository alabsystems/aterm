// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Content identity of the universal release's Rust and Apple toolchains and SDK.

#[cfg(target_os = "macos")]
use std::collections::BTreeMap;
#[cfg(any(test, target_os = "macos"))]
use std::collections::BTreeSet;
#[cfg(any(test, target_os = "macos"))]
use std::io::Read as _;
#[cfg(any(test, target_os = "macos"))]
use std::path::{Path, PathBuf};

#[cfg(any(test, target_os = "macos"))]
const SCHEMA: &[u8] = b"aterm-release-build-inputs-v2";

/// Fingerprint selected drivers, Rust and Apple toolchain bin/lib trees, and the macOS SDK.
/// Linux's native handoff has its own compiler provenance protocol.
/// # Errors
/// A compiler or SDK cannot be resolved or read, or this is not a macOS host.
pub fn current() -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        current_macos()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("universal release build input identity requires the macOS SDK; native Linux handoffs use their own provenance protocol".into())
    }
}

#[cfg(target_os = "macos")]
fn current_macos() -> Result<String, String> {
    use aterm_uds::spawnfd::command;

    let trust = crate::gates::trust_stage2_bin().map_err(|e| e.to_string())?;
    let trust_root = output_path(
        command(trust.join("trustc")).args(["--print", "sysroot"]),
        "Trust sysroot",
    )?;
    let home = std::env::var_os("HOME").ok_or("compiler identity requires HOME")?;
    let shim = crate::buildplan::release_rustup_shim_dir()?;
    let rustup_home = atpkg::seam::rustup_home_with(
        std::env::var_os("RUSTUP_HOME").as_deref(),
        Some(Path::new(&home)),
    )
    .filter(|path| path.is_absolute())
    .ok_or("compiler identity requires an absolute RUSTUP_HOME")?;
    let path = std::env::join_paths([
        shim.as_path(),
        Path::new("/usr/bin"),
        Path::new("/bin"),
        Path::new("/usr/sbin"),
        Path::new("/sbin"),
    ])
    .map_err(|e| format!("compiler identity PATH: {e}"))?;
    let stable_root = output_path(
        // STOCK EXCEPTION: the universal release's x86_64 slice uses upstream stable.
        command(shim.join("rustc"))
            .env_clear()
            .env("HOME", &home)
            .env("PATH", path)
            .env("RUSTUP_HOME", rustup_home)
            .env("RUSTUP_TOOLCHAIN", "stable")
            .args(["--print", "sysroot"])
            .current_dir("/"),
        "stable sysroot",
    )?;
    let trust_host = output_host(command(trust.join("trustc")).arg("-vV"), "Trust host")?;
    let stable_host = output_host(
        command(stable_root.join("bin/rustc")).arg("-vV"),
        "stable host",
    )?;
    let sdk = output_path(
        command("/usr/bin/xcrun")
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .args(["--sdk", "macosx", "--show-sdk-path"])
            .current_dir("/"),
        "macOS SDK",
    )?;
    let mut roots = vec![
        ("trust/selected-bin".to_string(), trust),
        ("trust/bin".to_string(), trust_root.join("bin")),
        ("stable/proxies".to_string(), shim),
        ("stable/bin".to_string(), stable_root.join("bin")),
        ("macos-sdk".to_string(), sdk),
    ];
    roots.extend(rust_library_roots("trust", &trust_root, &[&trust_host])?);
    roots.extend(rust_library_roots(
        "stable",
        &stable_root,
        &[&stable_host, "x86_64-apple-darwin"],
    )?);
    // Build scripts and Rust's linker reach these system dispatchers through
    // the scrubbed PATH; their selected toolchain alone does not identify them.
    for tool in [
        "xcrun",
        "cc",
        "c++",
        "clang",
        "ld",
        "as",
        "ar",
        "ranlib",
        "libtool",
        "dsymutil",
        "strip",
        "lipo",
        "ditto",
        "dwarfdump",
    ] {
        roots.push((
            format!("macos-driver/{tool}"),
            Path::new("/usr/bin").join(tool),
        ));
    }
    let mut apple_roots: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    for tool in ["clang", "ld", "dsymutil", "strip", "lipo"] {
        let executable = output_path(
            command("/usr/bin/xcrun")
                .env_clear()
                .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
                .args(["--sdk", "macosx", "--find", tool])
                .current_dir("/"),
            &format!("Apple {tool}"),
        )?
        .canonicalize()
        .map_err(|e| format!("resolve Apple {tool}: {e}"))?;
        let bin = executable
            .parent()
            .ok_or("Apple tool has no bin directory")?;
        let prefix = bin
            .parent()
            .ok_or("Apple tool has no toolchain directory")?;
        for (kind, path) in [("bin", bin.to_path_buf()), ("lib", prefix.join("lib"))] {
            let path = path
                .canonicalize()
                .map_err(|e| format!("resolve Apple {tool} {kind}: {e}"))?;
            apple_roots
                .entry(path)
                .or_default()
                .push(format!("{tool}/{kind}"));
        }
    }
    // The usual five tools share one tree. Hash that tree once, retaining the
    // selected-tool associations without incorporating an installation path.
    for (root, mut names) in apple_roots {
        names.sort();
        roots.push((format!("apple/{}", names.join("+")), root));
    }
    digest_roots(
        &roots
            .iter()
            .map(|(name, path)| (name.as_str(), path.as_path()))
            .collect::<Vec<_>>(),
    )
}

/// Compiler runtime libraries and the target libraries this build can use.
/// rustup's manifests and unrelated installed targets are installation state,
/// not inputs to either slice of the universal application.
#[cfg(any(test, target_os = "macos"))]
fn rust_library_roots(
    label: &str,
    root: &Path,
    targets: &[&str],
) -> Result<Vec<(String, PathBuf)>, String> {
    if targets.is_empty() {
        return Err(format!("{label} has no selected Rust target"));
    }
    let lib = root.join("lib");
    let mut roots = Vec::new();
    for entry in std::fs::read_dir(&lib).map_err(|e| format!("read {}: {e}", lib.display()))? {
        let entry = entry.map_err(|e| format!("read {}: {e}", lib.display()))?;
        let name = entry.file_name();
        let name = name.to_str().ok_or("Rust library name is not UTF-8")?;
        if name != "rustlib" {
            roots.push((format!("{label}/lib/{name}"), entry.path()));
        }
    }
    for target in targets.iter().copied().collect::<BTreeSet<_>>() {
        if !valid_rust_target(target) {
            return Err(format!("invalid {label} Rust target {target:?}"));
        }
        let path = lib.join("rustlib").join(target);
        if !path.is_dir() {
            return Err(format!(
                "selected {label} Rust target is missing: {}",
                path.display()
            ));
        }
        roots.push((format!("{label}/rustlib/{target}"), path));
    }
    roots.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(roots)
}

#[cfg(any(test, target_os = "macos"))]
fn valid_rust_target(target: &str) -> bool {
    !target.is_empty()
        && target.len() <= 128
        && target
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(target_os = "macos")]
fn output_host(command: &mut aterm_uds::spawnfd::Command, what: &str) -> Result<String, String> {
    let output = command
        .output()
        .map_err(|e| format!("resolve {what}: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "resolve {what}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let text = std::str::from_utf8(&output.stdout).map_err(|e| format!("resolve {what}: {e}"))?;
    host_from_text(text).map_err(|why| format!("{what}: {why}"))
}

#[cfg(any(test, target_os = "macos"))]
fn host_from_text(text: &str) -> Result<String, String> {
    let hosts: Vec<_> = text
        .lines()
        .filter_map(|line| line.strip_prefix("host: "))
        .collect();
    match hosts.as_slice() {
        [host] if valid_rust_target(host) => Ok((*host).to_owned()),
        _ => Err("compiler did not name one valid host target".into()),
    }
}

#[cfg(target_os = "macos")]
fn output_path(command: &mut aterm_uds::spawnfd::Command, what: &str) -> Result<PathBuf, String> {
    let output = command
        .output()
        .map_err(|e| format!("resolve {what}: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "resolve {what}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let text = std::str::from_utf8(&output.stdout).map_err(|e| format!("resolve {what}: {e}"))?;
    let path = PathBuf::from(text.trim());
    if !path.is_absolute() {
        return Err(format!("{what} did not name an absolute path"));
    }
    Ok(path)
}

/// The most entries one identity walks, across all its roots. Links that alias directories make
/// the walk visit some trees more than once (the SDK's frameworks: about 69,000 visits for its
/// 32,000 files on macOS 26.4), and links that lead between sibling directories can multiply
/// it without end; past this, the identity refuses rather than run on.
#[cfg(any(test, target_os = "macos"))]
const MAX_ENTRIES: usize = 2_000_000;

/// Hash labeled roots by their logical names and content, independent of absolute placement
/// (but for the target text an absolute link below contributes). Files, directories, modes and
/// resolved symlink contents contribute; a symlink beneath a root that dangles (the Command
/// Line Tools' `usr/bin/crashlog`), or that resolves to a directory the walk is inside (the
/// SDK's `Ruby.framework/…/Headers/ruby/ruby` is `.`), contributes its target text instead, as
/// it has no content of its own to add; timestamps do not. Root labels must be unique.
/// # Errors
/// There are no roots; a label is empty or repeated; a root is missing or dangles; a root or
/// descendant cannot be resolved (a link loop included), inspected or read, or is a special
/// file; nesting passes 256 levels or the walk passes [`MAX_ENTRIES`] entries; or a real
/// directory is reached again inside itself, which only a link above it (to the root's parent,
/// say) can do.
#[cfg(any(test, target_os = "macos"))]
pub(crate) fn digest_roots(roots: &[(&str, &Path)]) -> Result<String, String> {
    digest_roots_with(roots, &mut crate::durable::open_regular_file)
}

#[cfg(any(test, target_os = "macos"))]
fn digest_roots_with(
    roots: &[(&str, &Path)],
    open: &mut dyn FnMut(&Path) -> std::io::Result<std::fs::File>,
) -> Result<String, String> {
    if roots.is_empty() {
        return Err("build input identity has no roots".into());
    }
    let mut roots = roots.to_vec();
    roots.sort_by_key(|(label, _)| *label);
    let mut digest = aterm_digest::Sha256::new();
    field(&mut digest, SCHEMA);
    let mut previous = None;
    let mut visits = 0;
    for (label, root) in roots {
        if label.is_empty() || previous == Some(label) {
            return Err(format!(
                "build input root label is empty or duplicated: {label:?}"
            ));
        }
        previous = Some(label);
        field(&mut digest, label.as_bytes());
        // Resolve the root itself before walking, so a root alias has the same identity.
        let root = root
            .canonicalize()
            .map_err(|e| format!("resolve build input {}: {e}", root.display()))?;
        walk(
            &mut digest,
            &root,
            &[],
            &mut BTreeSet::new(),
            0,
            &mut visits,
            open,
        )?;
    }
    Ok(aterm_codec::hex::encode_lower(&digest.finalize()))
}

#[cfg(any(test, target_os = "macos"))]
fn field(digest: &mut aterm_digest::Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
}

#[cfg(any(test, target_os = "macos"))]
fn walk(
    digest: &mut aterm_digest::Sha256,
    path: &Path,
    relative: &[u8],
    ancestors: &mut BTreeSet<PathBuf>,
    depth: usize,
    visits: &mut usize,
    open: &mut dyn FnMut(&Path) -> std::io::Result<std::fs::File>,
) -> Result<(), String> {
    if depth > 256 {
        return Err(format!(
            "build input nesting exceeds 256 levels: {}",
            path.display()
        ));
    }
    *visits += 1;
    if *visits > MAX_ENTRIES {
        return Err(format!(
            "build input walk passes {MAX_ENTRIES} entries at {}: links multiply the tree",
            path.display()
        ));
    }
    let link = std::fs::symlink_metadata(path)
        .map_err(|e| format!("inspect build input {}: {e}", path.display()))?;
    field(digest, relative);
    field(
        digest,
        if link.file_type().is_symlink() {
            b"link"
        } else {
            b"entry"
        },
    );
    let resolved = match path.canonicalize() {
        Ok(resolved) => resolved,
        // A dangling link has no content: its target text is all of it, and that text is read
        // in full, so it is evidence rather than an absence (its target appearing changes the
        // identity). Apple's Command Line Tools ship one in usr/bin (`crashlog`, into an LLDB
        // framework they do not install). Any other failure to resolve still refuses.
        Err(err) if link.file_type().is_symlink() && err.kind() == std::io::ErrorKind::NotFound => {
            let target = std::fs::read_link(path)
                .map_err(|e| format!("read build input link {}: {e}", path.display()))?;
            field(digest, b"dangling");
            field(digest, target.as_os_str().as_encoded_bytes());
            return Ok(());
        }
        Err(e) => return Err(format!("resolve build input {}: {e}", path.display())),
    };
    let metadata = std::fs::metadata(&resolved)
        .map_err(|e| format!("inspect build input {}: {e}", resolved.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        field(
            digest,
            &(metadata.permissions().mode() & 0o7777).to_le_bytes(),
        );
    }
    #[cfg(not(unix))]
    field(digest, &[u8::from(metadata.permissions().readonly())]);
    if metadata.is_dir() {
        field(digest, b"directory");
        // A link that resolves to a directory the walk is inside adds only its target text:
        // that directory's content is this walk's already, and following it would never end.
        // A real directory reached again inside itself (after a link above it) still refuses.
        if ancestors.contains(&resolved) && link.file_type().is_symlink() {
            let target = std::fs::read_link(path)
                .map_err(|e| format!("read build input link {}: {e}", path.display()))?;
            field(digest, b"ancestor");
            field(digest, target.as_os_str().as_encoded_bytes());
            return Ok(());
        }
        if !ancestors.insert(resolved.clone()) {
            return Err(format!("build input directory cycle: {}", path.display()));
        }
        let mut entries = std::fs::read_dir(&resolved)
            .map_err(|e| format!("read build input {}: {e}", resolved.display()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("read build input {}: {e}", resolved.display()))?;
        entries.sort_by(|a, b| {
            a.file_name()
                .as_encoded_bytes()
                .cmp(b.file_name().as_encoded_bytes())
        });
        for entry in entries {
            let mut name = relative.to_vec();
            if !name.is_empty() {
                name.push(b'/');
            }
            name.extend_from_slice(entry.file_name().as_encoded_bytes());
            walk(
                digest,
                &entry.path(),
                &name,
                ancestors,
                depth + 1,
                visits,
                open,
            )?;
        }
        ancestors.remove(&resolved);
    } else if metadata.is_file() {
        field(digest, b"file");
        let mut file =
            open(&resolved).map_err(|e| format!("read build input {}: {e}", resolved.display()))?;
        let mut content = aterm_digest::Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|e| format!("read build input {}: {e}", resolved.display()))?;
            if read == 0 {
                break;
            }
            content.update(&buffer[..read]);
        }
        field(digest, &content.finalize());
    } else {
        return Err(format!("unsupported build input: {}", path.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_rust_libraries_ignore_other_targets_but_bind_every_used_library() {
        let root = aterm_tempfile::tempdir().unwrap();
        let selected = ["aarch64-apple-darwin", "x86_64-apple-darwin"];
        let used = [
            "lib/libLLVM.dylib",
            "lib/rustlib/aarch64-apple-darwin/lib/std.rlib",
            "lib/rustlib/aarch64-apple-darwin/codegen-backends/backend.dylib",
            "lib/rustlib/x86_64-apple-darwin/lib/std.rlib",
        ];
        let write = |name: &str, bytes: &[u8]| {
            let path = root.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        };
        for name in used {
            write(name, b"original");
        }
        let digest = || {
            let roots = rust_library_roots("compiler", root.path(), &selected).unwrap();
            digest_roots(
                &roots
                    .iter()
                    .map(|(name, path)| (name.as_str(), path.as_path()))
                    .collect::<Vec<_>>(),
            )
            .unwrap()
        };
        let original = digest();
        for name in [
            "lib/rustlib/aarch64-unknown-linux-gnu/lib/std.rlib",
            "lib/rustlib/components",
            "lib/rustlib/manifest-rust-std-aarch64-unknown-linux-gnu",
            "lib/rustlib/multirust-config.toml",
        ] {
            write(name, b"installed later");
            assert_eq!(digest(), original, "unrelated installation state: {name}");
        }
        for name in used {
            write(name, b"changed");
            assert_ne!(digest(), original, "selected input: {name}");
            write(name, b"original");
            assert_eq!(digest(), original);
        }
        assert_eq!(
            rust_library_roots("compiler", root.path(), &selected).unwrap(),
            rust_library_roots(
                "compiler",
                root.path(),
                &[selected[1], selected[0], selected[1]]
            )
            .unwrap()
        );
        for targets in [&[][..], &["missing"][..], &["../escape"][..]] {
            assert!(rust_library_roots("compiler", root.path(), targets).is_err());
        }
    }

    #[test]
    fn compiler_host_requires_one_valid_target() {
        assert_eq!(
            host_from_text("rustc 1.0\nhost: aarch64-apple-darwin\n").unwrap(),
            "aarch64-apple-darwin"
        );
        for text in ["", "host: ../escape", "host: ", "host: a\nhost: b"] {
            assert!(host_from_text(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn an_unreadable_input_never_yields_an_identity() {
        let root = aterm_tempfile::tempdir().unwrap();
        let file = root.path().join("compiler");
        std::fs::write(&file, "compiler").unwrap();
        assert!(digest_roots(&[("compiler", root.path())]).is_ok());
        let error = digest_roots_with(&[("compiler", root.path())], &mut |_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "denied",
            ))
        })
        .unwrap_err();
        assert!(error.contains("denied"), "{error}");
    }

    #[test]
    fn compiler_library_standard_library_and_sdk_bytes_all_change_identity() {
        let root = aterm_tempfile::tempdir().unwrap();
        let files = [
            "bin/trustc",
            "lib/compiler.dylib",
            "lib/rustlib/target/lib/std.rlib",
            "sdk/usr/include/header.h",
        ];
        for name in files {
            let path = root.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"original").unwrap();
        }
        let digest = || digest_roots(&[("inputs", root.path())]).unwrap();
        let original = digest();
        for name in files {
            std::fs::write(root.path().join(name), b"changed!").unwrap();
            assert_ne!(digest(), original, "{name}");
            std::fs::write(root.path().join(name), b"original").unwrap();
            assert_eq!(digest(), original);
        }
    }

    #[cfg(unix)]
    #[test]
    fn root_aliases_match_but_changed_link_targets_do_not() {
        use std::os::unix::fs::symlink;
        let root = aterm_tempfile::tempdir().unwrap();
        let payload = root.path().join("payload");
        std::fs::create_dir(&payload).unwrap();
        std::fs::write(payload.join("compiler"), b"one").unwrap();
        let alias = root.path().join("alias");
        symlink(&payload, &alias).unwrap();
        let original = digest_roots(&[("compiler", &payload)]).unwrap();
        assert_eq!(digest_roots(&[("compiler", &alias)]).unwrap(), original);
        let external = root.path().join("external");
        std::fs::write(&external, b"library").unwrap();
        symlink(&external, payload.join("lib")).unwrap();
        let with_link = digest_roots(&[("compiler", &payload)]).unwrap();
        std::fs::write(&external, b"replacement").unwrap();
        assert_ne!(digest_roots(&[("compiler", &payload)]).unwrap(), with_link);
    }

    /// A link back to a directory the walk is inside counts by its target text, once: the
    /// walk ends, the link's presence and its spelling change the identity, and the content
    /// it leads back to still does. CONTROL: the same tree without the link.
    #[cfg(unix)]
    #[test]
    fn a_link_back_to_an_enclosing_directory_counts_by_its_target_text() {
        use std::os::unix::fs::symlink;
        let root = aterm_tempfile::tempdir().unwrap();
        let headers = root.path().join("Headers/ruby");
        std::fs::create_dir_all(&headers).unwrap();
        std::fs::write(headers.join("ruby.h"), b"one").unwrap();
        let digest = || digest_roots(&[("sdk", root.path())]).unwrap();
        let without = digest();
        symlink(".", headers.join("ruby")).unwrap();
        let with_dot = digest();
        assert_ne!(with_dot, without, "the link is part of the identity");
        assert_eq!(digest(), with_dot);
        std::fs::remove_file(headers.join("ruby")).unwrap();
        symlink("..", headers.join("ruby")).unwrap();
        assert_ne!(digest(), with_dot, "so is its target text");
        std::fs::write(headers.join("ruby.h"), b"two").unwrap();
        let changed = digest();
        std::fs::write(headers.join("ruby.h"), b"one").unwrap();
        assert_ne!(changed, digest(), "and the content it leads back to");
    }

    #[test]
    fn missing_roots_and_ambiguous_labels_fail_closed() {
        let root = aterm_tempfile::tempdir().unwrap();
        assert!(digest_roots(&[("missing", &root.path().join("missing"))]).is_err());
        assert!(digest_roots(&[("same", root.path()), ("same", root.path())]).is_err());
        assert!(digest_roots(&[]).is_err());
    }

    /// A dangling link beneath a root counts by its target text: a new target text, or the
    /// target appearing, changes the identity. CONTROL: a dangling root still refuses.
    #[cfg(unix)]
    #[test]
    fn a_dangling_input_link_counts_by_its_target_text() {
        use std::os::unix::fs::symlink;
        let base = aterm_tempfile::tempdir().unwrap();
        let root = base.path().join("bin");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("compiler"), b"compiler").unwrap();
        let digest = || digest_roots(&[("tools", &root)]).unwrap();
        let without = digest();
        let link = root.join("crashlog");
        symlink("../missing", &link).unwrap();
        let dangling = digest();
        assert_ne!(dangling, without, "the link is part of the identity");
        assert_eq!(digest(), dangling);
        std::fs::remove_file(&link).unwrap();
        symlink("../elsewhere", &link).unwrap();
        let retargeted = digest();
        assert_ne!(
            retargeted, dangling,
            "the target text is part of the identity"
        );
        // The target lies outside the root, so only the link can carry its arrival.
        std::fs::write(base.path().join("elsewhere"), b"script").unwrap();
        assert_ne!(
            digest(),
            retargeted,
            "a target that appears changes the identity"
        );
        let dangling_root = base.path().join("root");
        symlink(base.path().join("absent"), &dangling_root).unwrap();
        assert!(digest_roots(&[("tools", &dangling_root)]).is_err());
    }

    /// A link above the root leads the walk back into the root as a real directory, which
    /// refuses, and a loop of links refuses as unresolvable. CONTROL: a link back to an
    /// enclosing directory counts (`a_link_back_to_an_enclosing_directory_counts_…`).
    #[cfg(unix)]
    #[test]
    fn a_walk_that_reenters_a_real_directory_or_loops_refuses() {
        use std::os::unix::fs::symlink;
        let base = aterm_tempfile::tempdir().unwrap();
        let root = base.path().join("sdk");
        std::fs::create_dir_all(root.join("lib")).unwrap();
        std::fs::write(root.join("lib/x.h"), b"x").unwrap();
        assert!(digest_roots(&[("sdk", &root)]).is_ok());
        symlink("../..", root.join("lib/up")).unwrap();
        let error = digest_roots(&[("sdk", &root)]).unwrap_err();
        assert!(error.contains("directory cycle"), "{error}");
        std::fs::remove_file(root.join("lib/up")).unwrap();
        symlink("b", root.join("a")).unwrap();
        symlink("a", root.join("b")).unwrap();
        let error = digest_roots(&[("sdk", &root)]).unwrap_err();
        assert!(error.contains("resolve build input"), "{error}");
    }
}
