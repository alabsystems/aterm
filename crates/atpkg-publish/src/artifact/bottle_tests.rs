// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::sync::Mutex;

use super::*;
use crate::artifact::relocate::ObjectRefs;
use crate::artifact::tarwrite::RawFrames;

#[cfg(unix)]
#[test]
fn git_wrapper_relocates_without_path_utilities() {
    use std::os::unix::fs::PermissionsExt as _;

    let work = aterm_tempfile::tempdir().unwrap();
    let stage = work.path().join("original install");
    let core = stage.join("libexec/git-core");
    std::fs::create_dir_all(&core).unwrap();
    std::fs::create_dir_all(stage.join("share/git-core/templates")).unwrap();
    let backend = core.join("git");
    std::fs::write(
        &backend,
        b"#!/bin/sh\nprintf '%s\\n' \"$ATERM_GIT_ROOT\" \"$GIT_EXEC_PATH\" \"$GIT_TEMPLATE_DIR\" \"$#\" \"$@\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(install_git_wrapper("git", &stage).unwrap());
    let moved = work.path().join("moved git's install");
    std::fs::rename(&stage, &moved).unwrap();
    let moved = moved.canonicalize().unwrap();
    let poison = work.path().join("poison");
    std::fs::create_dir(&poison).unwrap();
    let marker = work.path().join("dirname-called");
    let dirname = poison.join("dirname");
    std::fs::write(
        &dirname,
        b"#!/bin/sh\nprintf called > \"$ATPKG_POISON_MARKER\"\nexit 97\n",
    )
    .unwrap();
    std::fs::set_permissions(&dirname, std::fs::Permissions::from_mode(0o755)).unwrap();
    for path in [std::ffi::OsStr::new(""), poison.as_os_str()] {
        let mut command = aterm_uds::spawnfd::command(moved.join("bin/git"));
        command
            .args(["argument with spaces", "--version"])
            .env("PATH", path)
            .env("ATPKG_POISON_MARKER", &marker)
            .env_remove("GIT_EXEC_PATH")
            .env_remove("GIT_TEMPLATE_DIR");
        let output =
            aterm_uds::deadline::output_within(&mut command, aterm_spec::wait::MUST_HAPPEN)
                .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            format!(
                "{}\n{}\n{}\n2\nargument with spaces\n--version\n",
                moved.display(),
                moved.join("libexec/git-core").display(),
                moved.join("share/git-core/templates").display()
            )
        );
        assert!(!marker.exists());
    }
}

#[test]
fn cli_accepts_one_archive_or_install_tree_and_refuses_ambiguous_inputs() {
    use crate::host::fake::FakeHost;
    use crate::testkit::Keys;
    use std::sync::Arc;

    let work = aterm_tempfile::tempdir().unwrap();
    let ctx = Keys::mint("m3", &[]).ctx(work.path(), Arc::new(FakeHost::new()));
    let base = ["lean4", "--version", "4.31.0", "--build", "1", "--unsigned"];
    let args = |tail: &[&str]| {
        base.iter()
            .chain(tail)
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
    };
    let archive = parse_args(&ctx, &args(&["--archive", "runtime.tar.zst"])).unwrap();
    assert!(matches!(archive.input, BottleInput::Archive(p) if p == Path::new("runtime.tar.zst")));
    let tree = parse_args(&ctx, &args(&["--install-tree", "owned runtime tree"])).unwrap();
    assert!(
        matches!(tree.input, BottleInput::InstallTree(p) if p == Path::new("owned runtime tree"))
    );
    for tail in [
        vec![],
        vec!["--archive", "a", "--install-tree", "b"],
        vec!["--install-tree", "a", "--archive", "b"],
        vec!["--archive", "a", "--archive", "b"],
        vec!["--install-tree", "a", "--install-tree", "b"],
    ] {
        assert!(parse_args(&ctx, &args(&tail)).is_err(), "{tail:?}");
    }
}

#[test]
fn text_rewriting_skips_proven_large_binaries_but_keeps_its_text_cap() {
    use std::io::Write as _;

    let work = aterm_tempfile::tempdir().unwrap();
    let binary = work.path().join("libLean.a");
    let mut file = std::fs::File::create(&binary).unwrap();
    file.write_all(b"!<arch>\n\0binary").unwrap();
    file.set_len(MAX_BOTTLE_TEXT_BYTES + 1).unwrap();
    drop(file);
    rewrite_bottle_text(work.path()).unwrap();
    assert_eq!(
        std::fs::metadata(&binary).unwrap().len(),
        MAX_BOTTLE_TEXT_BYTES + 1
    );
    let text = work.path().join("large-text");
    let mut file = std::fs::File::create(&text).unwrap();
    file.write_all(&vec![b'x'; 64 * 1024]).unwrap();
    file.set_len(MAX_BOTTLE_TEXT_BYTES + 1).unwrap();
    drop(file);
    let error = rewrite_bottle_text(work.path()).unwrap_err();
    assert!(error.to_string().contains("file exceeds"), "{error}");
}

#[test]
fn bottle_archives_recognize_zstd_without_changing_other_containers() {
    for (name, want) in [
        ("lean.tar.zst", Container::TarZst),
        ("LEAN.TAR.ZST", Container::TarZst),
        ("tool.tar.gz", Container::TarGz),
        ("tool.tgz", Container::TarGz),
        ("tool.tar", Container::Tar),
        ("tool.zip", Container::Zip),
    ] {
        assert_eq!(archive_container(Path::new(name)).unwrap(), want);
    }
    assert!(archive_container(Path::new("tool.zst")).is_err());
}

#[derive(Default)]
pub(super) struct FakeBackend {
    refs: Mutex<BTreeMap<PathBuf, ObjectRefs>>,
    interpreters: Mutex<BTreeMap<PathBuf, String>>,
}

impl FakeBackend {
    fn set(&self, path: &Path, refs: ObjectRefs) {
        self.refs
            .lock()
            .expect("lock")
            .insert(path.to_path_buf(), refs);
    }

    fn lookup(map: &BTreeMap<PathBuf, ObjectRefs>, path: &Path) -> ObjectRefs {
        let clone_refs = |r: &ObjectRefs| ObjectRefs {
            id: r.id.clone(),
            needed: r.needed.clone(),
            rpaths: r.rpaths.clone(),
        };
        if let Some(r) = map.get(path) {
            return clone_refs(r);
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && let Some((_, r)) = map
                .iter()
                .find(|(k, _)| k.file_name().and_then(|n| n.to_str()) == Some(name))
        {
            return clone_refs(r);
        }
        ObjectRefs::default()
    }

    fn entry_mut<'a>(
        map: &'a mut BTreeMap<PathBuf, ObjectRefs>,
        path: &Path,
    ) -> &'a mut ObjectRefs {
        if !map.contains_key(path) {
            let seed = Self::lookup(map, path);
            map.insert(path.to_path_buf(), seed);
        }
        map.get_mut(path).expect("inserted")
    }
}

impl Backend for FakeBackend {
    fn is_native_object(&self, path: &Path) -> bool {
        arch::read(path).is_some()
    }

    fn read_refs(&self, path: &Path) -> std::result::Result<ObjectRefs, String> {
        let map = self.refs.lock().expect("lock");
        Ok(Self::lookup(&map, path))
    }

    fn interpreter(&self, path: &Path) -> std::result::Result<Option<String>, String> {
        let map = self.interpreters.lock().expect("lock");
        Ok(map
            .get(path)
            .or_else(|| {
                map.iter()
                    .find_map(|(key, value)| (key.file_name() == path.file_name()).then_some(value))
            })
            .cloned())
    }

    fn set_interpreter(&self, path: &Path, loader: &str) -> std::result::Result<(), String> {
        self.interpreters
            .lock()
            .expect("lock")
            .insert(path.to_path_buf(), loader.into());
        Ok(())
    }

    fn set_id_portable(&self, path: &Path, basename: &str) -> std::result::Result<(), String> {
        let mut map = self.refs.lock().expect("lock");
        let entry = Self::entry_mut(&mut map, path);
        entry.id = Some(format!("@rpath/{basename}"));
        Ok(())
    }

    fn repoint_dep(
        &self,
        path: &Path,
        from: &str,
        basename: &str,
    ) -> std::result::Result<(), String> {
        let mut map = self.refs.lock().expect("lock");
        let entry = Self::entry_mut(&mut map, path);
        for dep in &mut entry.needed {
            if dep == from {
                *dep = format!("@rpath/{basename}");
            }
        }
        Ok(())
    }

    fn fix_rpaths(
        &self,
        path: &Path,
        rel_origin: &str,
        keep: &[String],
        drop: &[String],
    ) -> std::result::Result<(), String> {
        let mut map = self.refs.lock().expect("lock");
        let entry = Self::entry_mut(&mut map, path);
        entry.rpaths.retain(|r| !drop.contains(r));
        for k in keep {
            if !entry.rpaths.contains(k) {
                entry.rpaths.push(k.clone());
            }
        }
        if !entry.rpaths.iter().any(|r| r == rel_origin) {
            entry.rpaths.push(rel_origin.to_string());
        }
        Ok(())
    }

    fn resign(&self, _path: &Path, _sign_id: Option<&str>) -> std::result::Result<(), String> {
        Ok(())
    }

    fn origin_token(&self) -> &'static str {
        "@loader_path"
    }
}

pub(super) fn thin_macho_bytes(cputype: u32) -> Vec<u8> {
    let mut bytes = vec![0u8; 32];
    bytes[0..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
    bytes[4..8].copy_from_slice(&cputype.to_le_bytes());
    bytes
}

fn write_ustar_header(name: &str, size: u64, mode: u32, typeflag: u8, linkname: &str) -> [u8; 512] {
    let mut h = [0u8; 512];
    h[..name.len()].copy_from_slice(name.as_bytes());
    let mode_s = format!("{mode:07o}\0");
    h[100..108].copy_from_slice(mode_s.as_bytes());
    h[108..116].copy_from_slice(b"0000000\0");
    h[116..124].copy_from_slice(b"0000000\0");
    let size_s = format!("{size:011o}\0");
    h[124..136].copy_from_slice(size_s.as_bytes());
    h[136..148].copy_from_slice(b"00000000000\0");
    h[148..156].copy_from_slice(b"        ");
    h[156] = typeflag;
    if !linkname.is_empty() {
        h[157..157 + linkname.len()].copy_from_slice(linkname.as_bytes());
    }
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
    let sum_s = format!("{sum:06o}\0 ");
    h[148..156].copy_from_slice(sum_s.as_bytes());
    h
}

pub(super) enum TarEntry<'a> {
    Dir(&'a str),
    File(&'a str, &'a [u8], u32),
    Symlink(&'a str, &'a str),
}

pub(super) fn build_tar(path: &Path, entries: &[TarEntry<'_>]) {
    let mut out = Vec::new();
    for e in entries {
        match *e {
            TarEntry::Dir(name) => {
                out.extend_from_slice(&write_ustar_header(name, 0, 0o755, b'5', ""));
            }
            TarEntry::File(name, data, mode) => {
                out.extend_from_slice(&write_ustar_header(name, data.len() as u64, mode, b'0', ""));
                out.extend_from_slice(data);
                let rem = data.len() % 512;
                if rem != 0 {
                    out.resize(out.len() + (512 - rem), 0);
                }
            }
            TarEntry::Symlink(name, target) => {
                out.extend_from_slice(&write_ustar_header(name, 0, 0o777, b'2', target));
            }
        }
    }
    out.resize(out.len() + 1024, 0);
    std::fs::write(path, out).expect("write tar");
}

#[test]
fn bottle_row_admits_git_on_darwin_and_gnu_linux_and_refuses_other_shapes() {
    assert!(bottle_row("gh", AARCH64_APPLE_DARWIN).is_ok());
    assert!(bottle_row("git", X86_64_APPLE_DARWIN).is_ok());
    assert!(bottle_row("git", X86_64_UNKNOWN_LINUX_GNU).is_ok());
    assert!(bottle_row("git", AARCH64_UNKNOWN_LINUX_GNU).is_ok());
    assert!(bottle_row("gitleaks", AARCH64_APPLE_DARWIN).is_ok());
    assert!(bottle_row("gitleaks", X86_64_APPLE_DARWIN).is_ok());
    assert!(bottle_row("gitleaks", X86_64_UNKNOWN_LINUX_GNU).is_ok());
    assert!(bottle_row("gitleaks", AARCH64_UNKNOWN_LINUX_GNU).is_ok());
    assert!(bottle_row("cpython", AARCH64_APPLE_DARWIN).is_ok());
    assert!(bottle_row("cpython", X86_64_APPLE_DARWIN).is_ok());
    assert!(bottle_row("openssl-signing", AARCH64_APPLE_DARWIN).is_ok());
    assert!(bottle_row("openssl-signing", X86_64_APPLE_DARWIN).is_ok());
    for target in ["x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"] {
        assert!(bottle_row("gitleaks", Triple::named(target)).is_err());
        assert!(bottle_row("cpython", Triple::named(target)).is_err());
        assert!(bottle_row("openssl-signing", Triple::named(target)).is_err());
    }
    assert!(bottle_row("cpython", AARCH64_UNKNOWN_LINUX_GNU).is_err());
    assert!(bottle_row("cpython", X86_64_UNKNOWN_LINUX_GNU).is_err());
    assert!(bottle_row("openssl-signing", AARCH64_UNKNOWN_LINUX_GNU).is_err());
    assert!(bottle_row("openssl-signing", X86_64_UNKNOWN_LINUX_GNU).is_err());
    assert!(bottle_row("gh", X86_64_UNKNOWN_LINUX_GNU).is_err());
    assert!(bottle_row("git", Triple::named("x86_64-pc-windows-msvc")).is_err());
    assert!(bottle_row("aterm", AARCH64_APPLE_DARWIN).is_err());
    assert!(bottle_row("no-such-program", AARCH64_APPLE_DARWIN).is_err());
}

#[cfg(unix)]
#[test]
fn gnu_git_archives_merge_stage_elf_payloads_and_refuse_mislabeled_objects() {
    use std::sync::Arc;

    use crate::host::fake::FakeHost;
    use crate::testkit::Keys;

    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let keys = Keys::mint("m3", &[]);
    let host = Arc::new(FakeHost::new());
    let ctx = keys.ctx(tmp.path(), host.clone());
    keys.publish_quad(
        &host,
        &ctx.index_slug(),
        41,
        &keys.index_toml(41, &[("ay", "ay")], &[("ay", 9)]),
    );
    let mut previous = None;
    for (triple, machine) in [
        (X86_64_UNKNOWN_LINUX_GNU, 62_u16),
        (AARCH64_UNKNOWN_LINUX_GNU, 183),
    ] {
        let mut elf = vec![0; 64];
        elf[..6].copy_from_slice(b"\x7fELF\x02\x01");
        elf[6] = 1;
        elf[16..18].copy_from_slice(&2_u16.to_le_bytes());
        elf[18..20].copy_from_slice(&machine.to_le_bytes());
        elf[20..24].copy_from_slice(&1_u32.to_le_bytes());
        elf[52..54].copy_from_slice(&64_u16.to_le_bytes());
        elf[54..56].copy_from_slice(&56_u16.to_le_bytes());
        let archive = tmp.path().join(format!("git-{triple}.tar"));
        build_tar(
            &archive,
            &[
                TarEntry::Dir("git/2.56.0/bin/"),
                TarEntry::Dir("git/2.56.0/libexec/git-core/"),
                TarEntry::File("git/2.56.0/libexec/git-core/git", &elf, 0o755),
            ],
        );
        let donor = tmp.path().join(format!("pcre2-{triple}.tar"));
        build_tar(
            &donor,
            &[
                TarEntry::Dir("pcre2/10.44/lib/"),
                TarEntry::File("pcre2/10.44/lib/libpcre2-8.so.0", &elf, 0o755),
                TarEntry::File("pcre2/10.44/COPYING", b"PCRE2 license\n", 0o644),
                TarEntry::File(
                    "ca-certificates/2026/share/ca-certificates/cacert.pem",
                    b"-----BEGIN CERTIFICATE-----\nfixture\n-----END CERTIFICATE-----\n",
                    0o644,
                ),
            ],
        );
        let mut request = BottleRequest {
            program: "git".into(),
            input: BottleInput::Archive(archive),
            version: "2.56.0".into(),
            build: 1,
            triple,
            strip_components: 2,
            dep_bottles: vec![donor],
            donor_roots: Vec::new(),
            merge_from: previous.clone(),
            key: None,
            out: tmp.path().join("out"),
            signing: Signing::MachineKey,
            upload: false,
        };
        let backend = FakeBackend::default();
        backend.set(
            Path::new("git"),
            ObjectRefs {
                needed: vec!["libpcre2-8.so.0".into()],
                ..ObjectRefs::default()
            },
        );
        backend
            .interpreters
            .lock()
            .expect("lock")
            .insert(PathBuf::from("git"), "@@HOMEBREW_PREFIX@@/lib/ld.so".into());
        let result = pack_bottle_with(&ctx, &request, &backend, &RawFrames::WHOLE)
            .expect("GNU Git install archive packs");
        assert!(
            backend
                .interpreters
                .lock()
                .expect("lock")
                .iter()
                .any(|(path, loader)| path.ends_with("libexec/git-core/git")
                    && Some(loader.as_str()) == system_loader(triple))
        );
        assert!(result.signature.as_ref().is_some_and(|p| p.is_file()));
        let parsed: atpkg::PkgManifest = aterm_toml::from_str(
            &std::fs::read_to_string(&result.manifest).expect("read manifest"),
        )
        .expect("client parses manifest");
        let row = parsed
            .artifacts
            .iter()
            .find(|row| row.target == triple.as_str())
            .expect("GNU row retained");
        assert_eq!(
            row.kind, "elf-binary",
            "old clients must refuse the selected GNU target before staging"
        );
        let stage = tmp.path().join(format!("store with spaces/{triple}"));
        atpkg::verify_for_publication(
            row,
            &result.asset,
            &stage,
            &atpkg::install::StageHooks::NONE,
        )
        .expect("client verifies and stages GNU Git");
        assert!(
            !stage.with_file_name(format!("{triple}.ready")).exists(),
            "publication inspection must not advertise a foreign package as installed"
        );
        assert_eq!(
            std::fs::read(stage.join("libexec/git-core/git")).expect("ELF payload"),
            elf
        );
        assert!(stage.join("lib/atpkg-vendored/libpcre2-8.so.0").is_file());
        assert_eq!(
            std::fs::read(stage.join("share/licenses/upstream/0/pcre2/10.44/COPYING"))
                .expect("donor license"),
            b"PCRE2 license\n"
        );
        assert!(
            std::fs::read_to_string(stage.join("bin/git"))
                .expect("wrapper")
                .contains("GIT_EXEC_PATH=")
        );
        // The actual retained-archive route keeps the payload bytes and still performs
        // client hash/tree/ELF admission. Native execution is injected here because these
        // deliberately minimal ELF fixtures are never executable programs.
        let reuse_request = reuse::Request {
            manifest: result.manifest.clone(),
            triple,
            merge_from: result.manifest.clone(),
            out: tmp.path().join(format!("reuse-{triple}")),
        };
        let admitted = reuse::prepare_with(&reuse_request, |_, tree, _| {
            assert!(tree.join("libexec/git-core/git").is_file());
            Ok(())
        })
        .expect("unchanged retained archive is admitted");
        assert_eq!(
            std::fs::read(&admitted.asset).unwrap(),
            std::fs::read(&result.asset).unwrap()
        );
        assert_eq!(
            std::fs::read(&admitted.manifest).unwrap(),
            std::fs::read(&result.manifest).unwrap()
        );
        assert!(admitted.signature.is_none());
        let mut refused = reuse_request.clone();
        refused.out = tmp.path().join(format!("qualification-refused-{triple}"));
        assert!(
            reuse::prepare_with(&refused, |_, _, _| Err(Error::refused(
                "capability refused"
            )))
            .is_err()
        );
        let corrupt = tmp.path().join(format!("corrupt-retained-{triple}"));
        std::fs::create_dir(&corrupt).unwrap();
        std::fs::copy(
            &result.manifest,
            corrupt.join(result.manifest.file_name().unwrap()),
        )
        .unwrap();
        let mut damaged = std::fs::read(&result.asset).unwrap();
        damaged[0] ^= 1;
        std::fs::write(corrupt.join(result.asset.file_name().unwrap()), damaged).unwrap();
        refused.manifest = corrupt.join(result.manifest.file_name().unwrap());
        refused.out = tmp.path().join(format!("corrupt-output-{triple}"));
        assert!(
            reuse::prepare_with(&refused, |_, _, _| panic!(
                "corrupt bytes reach no qualification"
            ))
            .is_err()
        );
        for (label, broken) in [
            (
                "path",
                std::fs::read_to_string(&result.manifest).unwrap().replace(
                    &format!("asset = \"{}\"", row.asset),
                    "asset = \"../outside.tar.zst\"",
                ),
            ),
            (
                "tree",
                std::fs::read_to_string(&result.manifest).unwrap().replace(
                    &format!("tree_root = \"{}\"", row.tree_root),
                    "tree_root = \"\"",
                ),
            ),
        ] {
            let bad = tmp.path().join(format!("bad-{label}-{triple}.toml"));
            std::fs::write(&bad, broken).unwrap();
            refused.manifest = bad;
            refused.out = tmp.path().join(format!("bad-{label}-output-{triple}"));
            assert!(
                reuse::prepare_with(&refused, |_, _, _| panic!(
                    "invalid metadata reaches no qualification"
                ))
                .is_err()
            );
            assert!(
                !refused.out.exists(),
                "invalid metadata creates no archive output"
            );
        }
        let preserved = tmp.path().join(format!("git-{triple}.pkg.toml"));
        std::fs::copy(&result.manifest, &preserved).expect("preserve manifest for merge");
        previous = Some(preserved);
        request.merge_from = None;
        request.triple = if triple == X86_64_UNKNOWN_LINUX_GNU {
            AARCH64_UNKNOWN_LINUX_GNU
        } else {
            AARCH64_APPLE_DARWIN
        };
        let error = pack_bottle_with(&ctx, &request, &FakeBackend::default(), &RawFrames::WHOLE)
            .expect_err("wrong CPU or object format cannot masquerade as another target");
        assert!(error.to_string().contains("MISMATCH"), "{error}");
    }
    let parsed: atpkg::PkgManifest = aterm_toml::from_str(
        &std::fs::read_to_string(previous.expect("merged manifest")).expect("read final"),
    )
    .expect("parse merged");
    assert_eq!(parsed.artifacts.len(), 2);
}

#[cfg(unix)]
#[test]
fn materialize_symlinks_turns_in_root_links_into_hard_links_and_refuses_escape() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let stage = dir.path().join("stage");
    std::fs::create_dir_all(stage.join("bin")).expect("mkdir");
    std::fs::create_dir_all(stage.join("libexec/git-core")).expect("mkdir");
    std::fs::write(stage.join("bin/git"), b"binary-bytes").expect("write");
    std::os::unix::fs::symlink("../../bin/git", stage.join("libexec/git-core/git"))
        .expect("symlink");

    materialize_symlinks(&stage).expect("in-root symlink materializes");
    let m1 = std::fs::symlink_metadata(stage.join("bin/git")).expect("stat bin/git");
    let m2 = std::fs::symlink_metadata(stage.join("libexec/git-core/git"))
        .expect("stat libexec/git-core/git");
    assert!(!m2.file_type().is_symlink());
    assert_eq!(
        inode_key(&m1),
        inode_key(&m2),
        "must share one hard-link inode"
    );

    // Negative control: an escaping symlink fails closed.
    let bad = dir.path().join("bad");
    std::fs::create_dir_all(&bad).expect("mkdir");
    std::fs::write(dir.path().join("outside"), b"secret").expect("write");
    std::os::unix::fs::symlink("../outside", bad.join("leak")).expect("symlink");
    assert!(materialize_symlinks(&bad).is_err());
}

#[cfg(unix)]
#[test]
fn pack_bottle_packs_git_and_gh_merges_dual_darwin_and_refuses_missing_dep() {
    use std::sync::Arc;

    use crate::host::fake::FakeHost;
    use crate::testkit::Keys;

    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let keys = Keys::mint("m3", &[]);
    let host = Arc::new(FakeHost::new());
    let ctx = keys.ctx(tmp.path(), host.clone());
    keys.publish_quad(
        &host,
        &ctx.index_slug(),
        41,
        &keys.index_toml(41, &[("ay", "ay")], &[("ay", 9)]),
    );

    let arm64 = thin_macho_bytes(0x0100_000c);
    let x64 = thin_macho_bytes(0x0100_0007);

    let dep_tar = tmp.path().join("pcre2.bottle.tar");
    build_tar(
        &dep_tar,
        &[
            TarEntry::Dir("pcre2/10.44/lib/"),
            TarEntry::File("pcre2/10.44/lib/libpcre2-8.0.dylib", &arm64, 0o755),
        ],
    );

    let git_tar = tmp.path().join("git.arm64.bottle.tar");
    build_tar(
        &git_tar,
        &[
            TarEntry::Dir("git/2.51.0/bin/"),
            TarEntry::Dir("git/2.51.0/libexec/git-core/"),
            TarEntry::Dir("git/2.51.0/share/git-core/templates/"),
            TarEntry::File("git/2.51.0/bin/git", &arm64, 0o755),
            TarEntry::Symlink("git/2.51.0/libexec/git-core/git", "../../bin/git"),
            TarEntry::File(
                "git/2.51.0/libexec/git-core/git-credential-osxkeychain",
                &arm64,
                0o755,
            ),
            TarEntry::File(
                "git/2.51.0/libexec/git-core/git-difftool",
                b"#!@@HOMEBREW_PERL@@\nprint \"ok\\n\";\n",
                0o755,
            ),
            TarEntry::File(
                "git/2.51.0/share/git-core/templates/description",
                b"Unnamed repository\n",
                0o644,
            ),
        ],
    );

    let backend = FakeBackend::default();
    backend.set(
        Path::new("bin/git"),
        ObjectRefs {
            id: None,
            needed: vec![
                "@@HOMEBREW_PREFIX@@/opt/pcre2/lib/libpcre2-8.0.dylib".to_string(),
                "/usr/lib/libSystem.B.dylib".to_string(),
            ],
            rpaths: vec!["@@HOMEBREW_PREFIX@@/lib".to_string()],
        },
    );
    backend.set(
        Path::new("libpcre2-8.0.dylib"),
        ObjectRefs {
            id: Some("@@HOMEBREW_PREFIX@@/opt/pcre2/lib/libpcre2-8.0.dylib".to_string()),
            needed: vec!["/usr/lib/libSystem.B.dylib".to_string()],
            rpaths: Vec::new(),
        },
    );

    // Negative control: omitting `--dep-bottle` refuses fail-closed on unresolved bottle dep.
    let out_dir = tmp.path().join("out");
    let missing_dep_req = BottleRequest {
        program: "git".to_string(),
        input: BottleInput::Archive(git_tar.clone()),
        version: "2.51.0".to_string(),
        build: 1,
        triple: AARCH64_APPLE_DARWIN,
        strip_components: 2,
        dep_bottles: Vec::new(),
        donor_roots: Vec::new(),
        merge_from: None,
        key: None,
        out: out_dir.clone(),
        signing: Signing::Unsigned,
        upload: false,
    };
    assert!(
        pack_bottle_with(&ctx, &missing_dep_req, &backend, &RawFrames::WHOLE).is_err(),
        "missing @@HOMEBREW_PREFIX@@ dep must fail closed"
    );

    // Positive: with `--dep-bottle`, `git` vendors `libpcre2-8.0.dylib`, writes the `bin/git`
    // wrapper without clobbering `libexec/git-core/git`, and stages cleanly via atpkg.
    let mut arm_req = missing_dep_req;
    arm_req.dep_bottles = vec![dep_tar];
    let arm_out = pack_bottle_with(&ctx, &arm_req, &backend, &RawFrames::WHOLE)
        .expect("arm64 git bottle packs");
    assert!(arm_out.asset.is_file());
    assert!(arm_out.manifest.is_file());

    // Merge an `x86_64-apple-darwin` row into the same `git@1` manifest.
    let git_x64_tar = tmp.path().join("git.x64.bottle.tar");
    build_tar(
        &git_x64_tar,
        &[
            TarEntry::Dir("git/2.51.0/bin/"),
            TarEntry::Dir("git/2.51.0/libexec/git-core/"),
            TarEntry::File("git/2.51.0/libexec/git-core/git", &x64, 0o755),
        ],
    );
    let x64_backend = FakeBackend::default();
    let merged_copy = tmp.path().join("git-1-base.pkg.toml");
    std::fs::copy(&arm_out.manifest, &merged_copy).expect("copy base manifest");
    let x64_req = BottleRequest {
        program: "git".to_string(),
        input: BottleInput::Archive(git_x64_tar),
        version: "2.51.0".to_string(),
        build: 1,
        triple: X86_64_APPLE_DARWIN,
        strip_components: 2,
        dep_bottles: Vec::new(),
        donor_roots: Vec::new(),
        merge_from: Some(merged_copy),
        key: None,
        out: out_dir,
        signing: Signing::MachineKey,
        upload: false,
    };
    let merged = pack_bottle_with(&ctx, &x64_req, &x64_backend, &RawFrames::WHOLE)
        .expect("x64 git bottle merges and signs");
    assert!(merged.signature.as_ref().is_some_and(|p| p.is_file()));
    let text = std::fs::read_to_string(&merged.manifest).expect("read merged");
    let parsed: atpkg::PkgManifest = aterm_toml::from_str(&text).expect("parse merged");
    assert_eq!(parsed.artifacts.len(), 2);
    assert!(parsed.artifacts.iter().all(|a| a.provenance.is_none()));

    // Verify staged arm64 tree keeps Mach-O `libexec/git-core/git` and wrapper `bin/git`.
    let staged = tmp.path().join("staged-git");
    let arm_row = parsed
        .artifacts
        .iter()
        .find(|a| a.target == "aarch64-apple-darwin")
        .expect("arm64 row");
    atpkg::verify_for_publication(
        arm_row,
        &arm_out.asset,
        &staged,
        &atpkg::install::StageHooks::NONE,
    )
    .expect("publication verification");
    let wrapper = std::fs::read_to_string(staged.join("bin/git")).expect("read wrapper");
    assert!(wrapper.starts_with("#!/bin/sh\n"));
    assert!(wrapper.contains("GIT_EXEC_PATH="));
    assert_eq!(
        std::fs::read(staged.join("libexec/git-core/git")).expect("read core git"),
        arm64,
        "wrapper write must not clobber hard-linked libexec/git-core/git binary"
    );
    assert!(
        !staged
            .join("libexec/git-core/git-credential-osxkeychain")
            .exists(),
        "a bottle never ships the keychain helper"
    );
    let perl_script =
        std::fs::read_to_string(staged.join("libexec/git-core/git-difftool")).expect("read script");
    assert!(perl_script.starts_with("#!/usr/bin/perl\n"));
    assert!(
        staged
            .join("lib/atpkg-vendored/libpcre2-8.0.dylib")
            .is_file()
    );
}

#[cfg(unix)]
fn staged_git_wrapper() -> (aterm_tempfile::TempDir, PathBuf) {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = aterm_tempfile::tempdir().expect("tempdir");
    let original = temp.path().join("original stage");
    let core = original.join("libexec/git-core/git");
    std::fs::create_dir_all(core.parent().unwrap()).expect("core directory");
    std::fs::create_dir_all(original.join("share/git-core/templates")).expect("templates");
    std::fs::write(
        &core,
        b"#!/bin/sh\nprintf '%s\\n' \"$ATERM_GIT_ROOT\" \"$GIT_EXEC_PATH\" \
          \"$GIT_TEMPLATE_DIR\" \"$@\"\nexit 37\n",
    )
    .expect("payload");
    std::fs::set_permissions(&core, std::fs::Permissions::from_mode(0o755))
        .expect("executable payload");
    assert!(install_git_wrapper("git", &original).expect("install real producer wrapper"));
    let relocated = temp.path().join("relocated stage with spaces");
    std::fs::rename(&original, &relocated).expect("relocate staged wrapper and payload");
    let relocated = relocated.canonicalize().expect("physical stage root");
    (temp, relocated)
}

#[cfg(unix)]
fn assert_staged_git_wrapper_runs(stage: &Path, path: &std::ffi::OsStr) {
    for (program, script, directory) in [
        (stage.join("bin/git"), None, stage.to_path_buf()),
        (PathBuf::from("./bin/git"), None, stage.to_path_buf()),
        (PathBuf::from("/bin/sh"), Some("git"), stage.join("bin")),
    ] {
        for overrides in [false, true] {
            let mut command = aterm_uds::spawnfd::command(&program);
            command.current_dir(&directory);
            if let Some(script) = script {
                command.arg(script);
            }
            command
                .env_clear()
                .env("PATH", path)
                .args(["argument with spaces", "--literal"]);
            let exec = if overrides {
                command.env("GIT_EXEC_PATH", "caller exec directory");
                "caller exec directory".to_owned()
            } else {
                stage.join("libexec/git-core").display().to_string()
            };
            let templates = if overrides {
                command.env("GIT_TEMPLATE_DIR", "caller templates directory");
                "caller templates directory".to_owned()
            } else {
                stage.join("share/git-core/templates").display().to_string()
            };
            let output =
                aterm_uds::deadline::output_within(&mut command, aterm_spec::wait::MUST_HAPPEN)
                    .expect("staged wrapper starts");
            assert_eq!(
                output.status.code(),
                Some(37),
                "{program:?} {script:?} from {directory:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                output.stderr.is_empty(),
                "{program:?} {script:?} from {directory:?}: {output:?}"
            );
            assert_eq!(
                output.stdout,
                format!(
                    "{}\n{exec}\n{templates}\nargument with spaces\n--literal\n",
                    stage.display()
                )
                .as_bytes()
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn git_wrapper_works_without_a_caller_tool_path() {
    let (_temp, stage) = staged_git_wrapper();
    assert_staged_git_wrapper_runs(&stage, std::ffi::OsStr::new(""));
}

#[cfg(unix)]
#[test]
fn git_wrapper_works_with_poisoned_path_tools() {
    use std::os::unix::fs::PermissionsExt as _;

    let (temp, stage) = staged_git_wrapper();
    let poisoned = temp.path().join("poisoned caller tools");
    std::fs::create_dir(&poisoned).expect("poison directory");
    for name in ["dirname", "pwd"] {
        let tool = poisoned.join(name);
        std::fs::write(
            &tool,
            b"#!/bin/sh\nprintf 'caller tool ran\\n' >&2\nexit 89\n",
        )
        .expect("poison tool");
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755))
            .expect("executable poison");
    }
    assert_staged_git_wrapper_runs(&stage, poisoned.as_os_str());
}

#[cfg(unix)]
#[test]
fn git_wrapper_runs_before_relocation_and_in_stage_libs_receive_rpath() {
    use std::sync::Arc;

    use crate::host::fake::FakeHost;
    use crate::testkit::Keys;

    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let keys = Keys::mint("m3", &[]);
    let host = Arc::new(FakeHost::new());
    let ctx = keys.ctx(tmp.path(), host);

    let arm64 = thin_macho_bytes(0x0100_000c);
    let dep_tar = tmp.path().join("gettext.bottle.tar");
    build_tar(
        &dep_tar,
        &[
            TarEntry::Dir("gettext/0.22.5/lib/"),
            TarEntry::File("gettext/0.22.5/lib/libintl.8.dylib", &arm64, 0o755),
        ],
    );

    // Archive has `bin/git`, `libexec/git-core/` (without `libexec/git-core/git`), and an
    // in-stage shared library `lib/libgit_helper.dylib`.
    let git_tar = tmp.path().join("git.bin_only.bottle.tar");
    build_tar(
        &git_tar,
        &[
            TarEntry::Dir("git/2.51.0/bin/"),
            TarEntry::Dir("git/2.51.0/lib/"),
            TarEntry::Dir("git/2.51.0/libexec/git-core/"),
            TarEntry::File("git/2.51.0/bin/git", &arm64, 0o755),
            TarEntry::File("git/2.51.0/lib/libgit_helper.dylib", &arm64, 0o755),
        ],
    );

    let backend = FakeBackend::default();
    backend.set(
        Path::new("git"),
        ObjectRefs {
            id: None,
            needed: vec![
                "@@HOMEBREW_PREFIX@@/opt/gettext/lib/libintl.8.dylib".to_string(),
                "@@HOMEBREW_CELLAR@@/git/2.51.0/lib/libgit_helper.dylib".to_string(),
            ],
            rpaths: Vec::new(),
        },
    );
    backend.set(
        Path::new("libgit_helper.dylib"),
        ObjectRefs {
            id: Some("@@HOMEBREW_CELLAR@@/git/2.51.0/lib/libgit_helper.dylib".to_string()),
            needed: vec!["/usr/lib/libSystem.B.dylib".to_string()],
            rpaths: Vec::new(),
        },
    );
    backend.set(
        Path::new("libintl.8.dylib"),
        ObjectRefs {
            id: Some("@@HOMEBREW_PREFIX@@/opt/gettext/lib/libintl.8.dylib".to_string()),
            needed: vec!["/usr/lib/libSystem.B.dylib".to_string()],
            rpaths: Vec::new(),
        },
    );

    let req = BottleRequest {
        program: "git".to_string(),
        input: BottleInput::Archive(git_tar),
        version: "2.51.0".to_string(),
        build: 1,
        triple: AARCH64_APPLE_DARWIN,
        strip_components: 2,
        dep_bottles: vec![dep_tar],
        donor_roots: Vec::new(),
        merge_from: None,
        key: None,
        out: tmp.path().join("out"),
        signing: Signing::Unsigned,
        upload: false,
    };
    let _out = pack_bottle_with(&ctx, &req, &backend, &RawFrames::WHOLE)
        .expect("git bottle with bin/git + in-stage lib packs");

    // Verify `libexec/git-core/git` was relocated at depth 2 (`../../lib/atpkg-vendored` and
    // `../../lib`), not depth 1 (`../lib/atpkg-vendored`).
    let map = backend.refs.lock().expect("lock");
    let core_git_refs = map
        .iter()
        .find_map(|(k, v)| k.ends_with("libexec/git-core/git").then_some(v))
        .expect("libexec/git-core/git refs recorded");
    assert_eq!(
        core_git_refs.needed,
        vec![
            "@rpath/libintl.8.dylib".to_string(),
            "@rpath/libgit_helper.dylib".to_string(),
        ]
    );
    assert!(
        core_git_refs
            .rpaths
            .contains(&"@loader_path/../../lib/atpkg-vendored".to_string()),
        "libexec/git-core/git must have depth-2 vendored rpath: {:?}",
        core_git_refs.rpaths
    );
    assert!(
        core_git_refs
            .rpaths
            .contains(&"@loader_path/../../lib".to_string()),
        "libexec/git-core/git must have depth-2 in-stage lib/ rpath: {:?}",
        core_git_refs.rpaths
    );
    assert!(
        !core_git_refs
            .rpaths
            .contains(&"@loader_path/../lib/atpkg-vendored".to_string()),
        "bin/git must be wrapped before relocation so depth-1 rpath is not written to core git: {:?}",
        core_git_refs.rpaths
    );
    drop(map);

    // Negative control: `check_stage_tree` refuses a Mach-O object that still carries an
    // unrewritten `@@HOMEBREW_*@@` load command.
    let bad_stage = tmp.path().join("bad-stage");
    std::fs::create_dir_all(bad_stage.join("bin")).expect("mkdir");
    let bad_gh = bad_stage.join("bin/gh");
    std::fs::write(&bad_gh, &arm64).expect("write");
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&bad_gh, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let bad_backend = FakeBackend::default();
    bad_backend.set(
        &bad_gh,
        ObjectRefs {
            id: None,
            needed: Vec::new(),
            rpaths: vec!["@@HOMEBREW_PREFIX@@/lib".to_string()],
        },
    );
    assert!(
        check_stage_tree(
            "gh",
            &bad_stage,
            AARCH64_APPLE_DARWIN,
            &["gh"],
            &bad_backend
        )
        .is_err(),
        "leftover @@HOMEBREW_*@@ load command on native object must fail closed"
    );

    let primary = bad_stage.join("bin/primary");
    let alias = bad_stage.join("bin/alias");
    std::fs::write(&primary, b"v2").expect("write primary");
    std::fs::write(&alias, b"v1").expect("write alias");
    relink_group(&primary, std::slice::from_ref(&alias)).expect("relink distinct inode");
    assert_eq!(std::fs::read(&alias).expect("read alias"), b"v2");
    relink_group(&primary, std::slice::from_ref(&alias)).expect("relink same inode no-op");
    assert_eq!(std::fs::read(&alias).expect("read alias again"), b"v2");
}

#[cfg(unix)]
#[test]
fn relinking_never_consumes_a_hidden_primary_as_scratch() {
    super::super::relink_tests::preserves_payload(
        super::super::relink_tests::Collision::Primary,
        |_, primary, aliases| relink_group(primary, aliases).map_err(|error| error.to_string()),
    );
}

#[cfg(unix)]
#[test]
fn relinking_preserves_an_unrelated_occupied_legacy_name() {
    super::super::relink_tests::preserves_payload(
        super::super::relink_tests::Collision::Payload,
        |_, primary, aliases| relink_group(primary, aliases).map_err(|error| error.to_string()),
    );
}

/// The wrapper `install_git_wrapper` wrote before it located itself with builtins alone, byte for
/// byte: it ran `dirname` through the caller's PATH. Kept as the negative control for the test
/// below.
#[cfg(unix)]
const DIRNAME_GIT_WRAPPER: &str = "#!/bin/sh\n\
__atpkg_bin_dir=$(CDPATH='' cd -- \"$(dirname -- \"$0\")\" && pwd -P) || exit 1\n\
__atpkg_root=$(dirname -- \"$__atpkg_bin_dir\")\n\
export ATERM_GIT_ROOT=\"$__atpkg_root\"\n\
export GIT_EXEC_PATH=\"${GIT_EXEC_PATH:-$__atpkg_root/libexec/git-core}\"\n\
if [ -d \"$__atpkg_root/share/git-core/templates\" ]; then\n\
  export GIT_TEMPLATE_DIR=\"${GIT_TEMPLATE_DIR:-$__atpkg_root/share/git-core/templates}\"\n\
fi\n\
exec \"$__atpkg_root/libexec/git-core/git\" \"$@\"\n";

#[cfg(unix)]
#[test]
fn git_wrapper_locates_itself_under_an_empty_or_unset_path() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = aterm_tempfile::tempdir().expect("tempdir");
    // A space in the root, and a symlinked directory in front of `bin/`, as a store link has.
    let root = dir.path().join("git store").join("2026110303");
    let core = root.join("libexec/git-core");
    std::fs::create_dir_all(&core).expect("mkdir");
    let stand_in = core.join("git");
    std::fs::write(
        &stand_in,
        "#!/bin/sh\nprintf 'exec=%s root=%s args=%s\\n' \"${GIT_EXEC_PATH:-}\" \"${ATERM_GIT_ROOT:-}\" \"$*\"\n",
    )
    .expect("write stand-in");
    std::fs::set_permissions(&stand_in, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    assert!(install_git_wrapper("git", &root).expect("wrapper"));
    let link = dir.path().join("linked bin");
    std::os::unix::fs::symlink(root.join("bin"), &link).expect("symlink");

    let real = std::fs::canonicalize(&root).expect("canonical root");
    let expected = |args: &str| {
        format!(
            "exec={}/libexec/git-core root={} args={args}\n",
            real.display(),
            real.display()
        )
    };
    // How each run is started: by the wrapper's path, through a symlinked bin/, by `sh bin/git`
    // or `sh ./bin/git` from the root so `$0` is relative, or by `sh git` in bin/ so `$0` holds
    // no slash; each with PATH empty and with PATH unset.
    const LAUNCHES: [&str; 5] = ["absolute", "symlinked bin", "bin/git", "./bin/git", "git"];
    let run = |launch: &str, unset: bool| {
        let mut command = match launch {
            "absolute" => aterm_uds::spawnfd::command(root.join("bin/git")),
            "symlinked bin" => aterm_uds::spawnfd::command(link.join("git")),
            relative => {
                let mut c = aterm_uds::spawnfd::command("/bin/sh");
                let cwd = if relative.contains('/') {
                    root.clone()
                } else {
                    root.join("bin")
                };
                c.arg(relative).current_dir(cwd);
                c
            }
        };
        command
            .args(["status", "two words"])
            .env_remove("GIT_EXEC_PATH");
        if unset {
            command.env_remove("PATH");
        } else {
            command.env("PATH", "");
        }
        aterm_spec::wait::past_text_busy(|| {
            aterm_uds::deadline::output_within(&mut command, aterm_spec::wait::MUST_HAPPEN).map_err(
                |e| match e {
                    aterm_uds::deadline::RunError::Spawn(e) => e,
                    other => std::io::Error::other(other),
                },
            )
        })
        .expect("run wrapper")
    };
    for name in LAUNCHES {
        for unset in [false, true] {
            let out = run(name, unset);
            assert!(
                out.status.success(),
                "{name} unset={unset}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                expected("status two words"),
                "{name} unset={unset}"
            );
        }
    }

    // Negative control: the dirname wrapper, run each of the same ways, cannot find itself under
    // an empty PATH.
    let wrapper = root.join("bin/git");
    std::fs::remove_file(&wrapper).expect("rm wrapper");
    std::fs::write(&wrapper, DIRNAME_GIT_WRAPPER).expect("write old wrapper");
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    for name in LAUNCHES {
        let out = run(name, false);
        assert!(
            !out.status.success() && out.stdout.is_empty(),
            "{name}: the dirname wrapper must fail under an empty PATH: {out:?}"
        );
    }
}
