// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn elf_interpreter_reads_only_bounded_program_headers() {
    let loader = b"@@HOMEBREW_PREFIX@@/lib/ld.so\0";
    let mut bytes = vec![0; 176];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    bytes[16..18].copy_from_slice(&2_u16.to_le_bytes());
    bytes[20..24].copy_from_slice(&1_u32.to_le_bytes());
    bytes[32..40].copy_from_slice(&64_u64.to_le_bytes());
    bytes[52..54].copy_from_slice(&64_u16.to_le_bytes());
    bytes[54..56].copy_from_slice(&56_u16.to_le_bytes());
    bytes[56..58].copy_from_slice(&2_u16.to_le_bytes());
    bytes[64..68].copy_from_slice(&1_u32.to_le_bytes());
    bytes[96..104].copy_from_slice(&176_u64.to_le_bytes());
    bytes[104..112].copy_from_slice(&176_u64.to_le_bytes());
    bytes[120..124].copy_from_slice(&3_u32.to_le_bytes());
    bytes[128..136].copy_from_slice(&176_u64.to_le_bytes());
    bytes[152..160].copy_from_slice(&(loader.len() as u64).to_le_bytes());
    bytes[160..168].copy_from_slice(&(loader.len() as u64).to_le_bytes());
    bytes.extend_from_slice(loader);
    assert_eq!(
        elf::interpreter(&bytes).unwrap().as_deref(),
        Some("@@HOMEBREW_PREFIX@@/lib/ld.so")
    );
    assert!(elf::interpreter(&bytes[..119]).is_err());
    *bytes.last_mut().unwrap() = b'x';
    assert!(elf::interpreter(&bytes).is_err());
    bytes[120..124].copy_from_slice(&1_u32.to_le_bytes());
    assert_eq!(elf::interpreter(&bytes).unwrap(), None);
    bytes[32..40].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(elf::interpreter(&bytes).is_err());
}

#[test]
fn machine_local_classification_covers_both_oses() {
    assert!(is_machine_local("/opt/homebrew/lib/libz.dylib"));
    assert!(is_machine_local("/Users//example/.rustup/x/lib"));
    assert!(is_machine_local("/home/builder/.rustup/toolchains/n/lib"));
    assert!(is_machine_local("/root/.cargo/registry/x"));
    assert!(is_machine_local(
        "/usr/local/rustup/toolchains/1.97.1-x86_64-unknown-linux-gnu/lib/libstd.so"
    ));
    assert!(is_machine_local(
        "/usr/local/Cellar/zstd/1.5.7/lib/libzstd.1.dylib"
    ));
    assert!(!is_machine_local("@rpath/libx.dylib"));
    assert!(!is_machine_local("$ORIGIN/../lib"));
    assert!(!is_machine_local("/usr/lib/libSystem.B.dylib"));
    assert!(!is_machine_local("libc.so.6"));
}

#[test]
fn origin_relative_is_platform_tokened() {
    let stage = Path::new("/s");
    let vendored = stage.join(VENDOR_REL);
    assert_eq!(
        origin_relative("@loader_path", &stage.join("bin"), &vendored),
        "@loader_path/../lib/atpkg-vendored"
    );
    assert_eq!(
        origin_relative("$ORIGIN", &stage.join("bin"), &vendored),
        "$ORIGIN/../lib/atpkg-vendored"
    );
    assert_eq!(
        origin_relative("@loader_path", &vendored, &vendored),
        "@loader_path"
    );
    assert_eq!(
        origin_relative("$ORIGIN", &stage.join("lib/rustlib/x/bin"), &vendored),
        "$ORIGIN/../../../atpkg-vendored"
    );
}

#[test]
fn origin_relative_rpath_is_never_machine_local() {
    for stage in [
        Path::new("/Users//builder/stage"),
        Path::new("/opt/homebrew/var/stage"),
        Path::new("/home/builder/.cargo/checkout/stage"),
    ] {
        let vendored = stage.join(VENDOR_REL);
        for dir in [
            stage.join("bin"),
            stage.join("lib/rustlib/x/bin"),
            vendored.clone(),
        ] {
            for token in ["@loader_path", "$ORIGIN"] {
                let rel = origin_relative(token, &dir, &vendored);
                assert!(rel.starts_with(token));
                assert!(!is_machine_local(&rel), "{rel} must partition into `keep`");
            }
        }
    }
}

#[test]
fn shared_lib_name_detection() {
    assert!(is_shared_lib_basename("librustc_driver-abc.dylib"));
    assert!(is_shared_lib_basename("libstd.so"));
    assert!(is_shared_lib_basename("libLLVM.so.19.1"));
    assert!(!is_shared_lib_basename("trustc"));
    assert!(!is_shared_lib_basename("libfoo.rlib"));
}

#[test]
fn macho_magic_and_otool_parse() {
    assert!(macho::is_macho_magic(&[0xcf, 0xfa, 0xed, 0xfe]));
    assert!(macho::is_macho_magic(&[0xca, 0xfe, 0xba, 0xbe]));
    assert!(!macho::is_macho_magic(&[0x7f, b'E', b'L', b'F']));
    let text = "          cmd LC_ID_DYLIB\n         name /Users//b/deps/libchecker.dylib (offset 24)\n          cmd LC_LOAD_DYLIB\n         name @rpath/librustc_driver.dylib (offset 24)\n          cmd LC_RPATH\n         path /Users//b/.rustup/x/lib (offset 12)\n          cmd LC_RPATH\n         path @loader_path/../toolchain/lib (offset 12)\n";
    let r = macho::parse_otool(text);
    assert_eq!(r.id.as_deref(), Some("/Users//b/deps/libchecker.dylib"));
    assert_eq!(r.needed, vec!["@rpath/librustc_driver.dylib"]);
    assert_eq!(
        r.rpaths,
        vec!["/Users//b/.rustup/x/lib", "@loader_path/../toolchain/lib"]
    );
}

#[test]
fn elf_magic_and_patchelf_parse() {
    assert!(elf::is_elf_magic(&[0x7f, b'E', b'L', b'F']));
    assert!(!elf::is_elf_magic(&[0xcf, 0xfa, 0xed, 0xfe]));
    assert_eq!(
        elf::parse_needed("libc.so.6\nlibrustc_driver.so\n"),
        vec!["libc.so.6", "librustc_driver.so"]
    );
    assert_eq!(
        elf::parse_rpath("/home/b/.rustup/x/lib:$ORIGIN/../lib"),
        vec!["/home/b/.rustup/x/lib", "$ORIGIN/../lib"]
    );
    assert_eq!(
        elf::combined_rpath(
            "$ORIGIN/../lib/atpkg-vendored",
            &["/home/b/.rustup/x/lib".into(), "$ORIGIN/../lib".into()]
        ),
        "$ORIGIN/../lib/atpkg-vendored:$ORIGIN/../lib"
    );
}

/// A backend that behaves like `install_name_tool` and `codesign` where it matters here:
/// every rewrite replaces the file (a sibling renamed over, a new inode), and the
/// "signature" names the file it was written under, as an ad-hoc identifier does.
#[cfg(unix)]
struct ReplacingBackend {
    log: std::cell::RefCell<Vec<String>>,
}

#[cfg(unix)]
impl ReplacingBackend {
    fn rewrite(&self, path: &Path, what: &str) -> Result<(), String> {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        self.log
            .borrow_mut()
            .push(format!("{what} {}", name.unwrap_or_default()));
        let mut bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        bytes.extend_from_slice(format!("|{what}").as_bytes());
        let tmp = path.with_extension("fake-rewrite");
        std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    }
}

#[cfg(unix)]
impl Backend for ReplacingBackend {
    fn is_native_object(&self, path: &Path) -> bool {
        std::fs::read(path).is_ok_and(|b| b.starts_with(b"OBJ"))
    }
    fn read_refs(&self, _path: &Path) -> Result<ObjectRefs, String> {
        Ok(ObjectRefs {
            id: None,
            needed: Vec::new(),
            rpaths: vec!["/Users//builder/.rustup/lib".to_string()],
        })
    }
    fn set_id_portable(&self, _path: &Path, _basename: &str) -> Result<(), String> {
        Ok(())
    }
    fn repoint_dep(&self, _path: &Path, _from: &str, _basename: &str) -> Result<(), String> {
        Ok(())
    }
    fn fix_rpaths(
        &self,
        path: &Path,
        rel_origin: &str,
        _keep: &[String],
        _drop: &[String],
    ) -> Result<(), String> {
        self.rewrite(path, &format!("rpath={rel_origin}"))
    }
    fn resign(&self, path: &Path, _sign_id: Option<&str>) -> Result<(), String> {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        self.rewrite(path, &format!("sig={}", name.unwrap_or_default()))
    }
    fn origin_token(&self) -> &'static str {
        "@loader_path"
    }
}

/// A backend over fake objects (files starting `OBJ`) whose load commands are
/// scripted per path; a re-sign appends `|sig`, so a test can tell the donor's bytes
/// from what the signature pass added.
#[cfg(unix)]
struct ScriptedBackend {
    refs: BTreeMap<PathBuf, ObjectRefs>,
}

#[cfg(unix)]
impl Backend for ScriptedBackend {
    fn is_native_object(&self, path: &Path) -> bool {
        std::fs::read(path).is_ok_and(|b| b.starts_with(b"OBJ"))
    }
    fn read_refs(&self, path: &Path) -> Result<ObjectRefs, String> {
        Ok(self
            .refs
            .get(path)
            .map_or_else(ObjectRefs::default, |r| ObjectRefs {
                id: r.id.clone(),
                needed: r.needed.clone(),
                rpaths: r.rpaths.clone(),
            }))
    }
    fn set_id_portable(&self, _path: &Path, _basename: &str) -> Result<(), String> {
        Ok(())
    }
    fn repoint_dep(&self, _path: &Path, _from: &str, _basename: &str) -> Result<(), String> {
        Ok(())
    }
    fn fix_rpaths(&self, _: &Path, _: &str, _: &[String], _: &[String]) -> Result<(), String> {
        Ok(())
    }
    fn resign(&self, path: &Path, _sign_id: Option<&str>) -> Result<(), String> {
        let mut bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        bytes.extend_from_slice(b"|sig");
        std::fs::write(path, bytes).map_err(|e| e.to_string())
    }
    fn origin_token(&self) -> &'static str {
        "@loader_path"
    }
}

/// A scratch dir holding `stage/bin` (removed on drop), and its canonical path.
#[cfg(unix)]
fn scratch(label: &str) -> (aterm_tempfile::TempDir, PathBuf) {
    let dir = aterm_tempfile::Builder::new()
        .prefix(&format!("atpkg-reloc-{label}-"))
        .tempdir()
        .unwrap();
    std::fs::create_dir_all(dir.path().join("stage/bin")).unwrap();
    let path = std::fs::canonicalize(dir.path()).unwrap();
    (dir, path)
}

/// A tool loading rustup's nightly `librustc_driver`, by absolute path or through an
/// rpath into rustup's directory, is refused before a byte is copied, and declaring that
/// directory a donor root does not launder it.
#[cfg(unix)]
#[test]
fn a_stock_toolchain_library_is_never_vendored() {
    let (_dir, d) = scratch("stock");
    let stage = d.join("stage");
    let tool = stage.join("bin/tool");
    std::fs::write(&tool, b"OBJ tool").unwrap();
    let absolute = "/Users//x/.rustup/toolchains/nightly-2025-12-03-aarch64-apple-darwin/lib/librustc_driver-3136bc78c25198f1.dylib";
    let backend = ScriptedBackend {
        refs: BTreeMap::from([(
            tool.clone(),
            ObjectRefs {
                needed: vec![absolute.to_string()],
                ..ObjectRefs::default()
            },
        )]),
    };
    let err = relocate_with(&stage, &backend, None, &[]).expect_err("stock by absolute path");
    assert!(
        err.contains("a stock toolchain library cannot be vendored"),
        "{err}"
    );
    assert!(
        !stage.join(VENDOR_REL).exists(),
        "nothing may be written to {VENDOR_REL}"
    );

    let system = d.join("usr/local/rustup/toolchains/1.97.1-x86_64-unknown-linux-gnu/lib");
    std::fs::create_dir_all(&system).unwrap();
    std::fs::write(system.join("libstd-0123456789abcdef.so"), b"OBJ stock std").unwrap();
    let backend = ScriptedBackend {
        refs: BTreeMap::from([(
            tool.clone(),
            ObjectRefs {
                needed: vec![
                    system
                        .join("libstd-0123456789abcdef.so")
                        .display()
                        .to_string(),
                ],
                ..ObjectRefs::default()
            },
        )]),
    };
    let err = relocate_with(&stage, &backend, None, std::slice::from_ref(&system))
        .expect_err("a system rustup's library, even as a declared donor");
    assert!(
        err.contains("a stock toolchain library cannot be vendored"),
        "{err}"
    );
    assert!(
        !stage.join(VENDOR_REL).exists(),
        "nothing may be written to {VENDOR_REL}"
    );

    let nightly = d.join("fake/.rustup/toolchains/nightly-2025-12-03-aarch64-apple-darwin/lib");
    std::fs::create_dir_all(&nightly).unwrap();
    std::fs::write(
        nightly.join("librustc_driver-3136bc78c25198f1.dylib"),
        b"OBJ stock driver",
    )
    .unwrap();
    let backend = ScriptedBackend {
        refs: BTreeMap::from([(
            tool.clone(),
            ObjectRefs {
                needed: vec!["@rpath/librustc_driver-3136bc78c25198f1.dylib".to_string()],
                rpaths: vec![nightly.display().to_string()],
                ..ObjectRefs::default()
            },
        )]),
    };
    let err = relocate_with(&stage, &backend, None, std::slice::from_ref(&nightly))
        .expect_err("stock through an rpath, even as a declared donor");
    assert!(
        err.contains("a stock toolchain library cannot be vendored"),
        "{err}"
    );
    assert!(
        !stage.join(VENDOR_REL).exists(),
        "nothing may be written to {VENDOR_REL}"
    );
}

/// The Trust shape: a library from inside a declared donor root is copied
/// byte-identical (the only later change is the re-sign), and the copy is recorded.
#[cfg(unix)]
#[test]
fn a_donor_root_library_is_vendored_byte_identical_and_recorded() {
    let (_dir, d) = scratch("donor");
    let stage = d.join("stage");
    let seal = d.join("seal");
    std::fs::create_dir_all(seal.join("lib")).unwrap();
    let donor = seal.join("lib/librustc_driver-0123456789abcdef.dylib");
    std::fs::write(&donor, b"OBJ trust driver").unwrap();
    let tool = stage.join("bin/tool");
    std::fs::write(&tool, b"OBJ tool").unwrap();
    let backend = ScriptedBackend {
        refs: BTreeMap::from([(
            tool.clone(),
            ObjectRefs {
                needed: vec![donor.display().to_string()],
                ..ObjectRefs::default()
            },
        )]),
    };
    let report =
        relocate_with(&stage, &backend, None, std::slice::from_ref(&seal)).expect("relocate");
    assert!(report.unresolved.is_empty(), "{:?}", report.unresolved);
    assert_eq!(
        report.vendored,
        vec!["librustc_driver-0123456789abcdef.dylib"]
    );
    let want = atpkg::tree::sha256_file(&donor).unwrap();
    assert_eq!(
        report.vendored_from,
        vec![Vendored {
            name: "librustc_driver-0123456789abcdef.dylib".to_string(),
            sha256: want.clone(),
            donor_rel: "lib/librustc_driver-0123456789abcdef.dylib".to_string(),
        }]
    );
    let copy = std::fs::read(
        stage
            .join(VENDOR_REL)
            .join("librustc_driver-0123456789abcdef.dylib"),
    )
    .unwrap();
    assert_eq!(
        copy, b"OBJ trust driver|sig",
        "byte-identical to the donor before the re-sign"
    );
    let record = std::fs::read_to_string(stage.join(VENDOR_REL).join(VENDORED_RECORD)).unwrap();
    assert!(
        record.contains(&format!("{want} librustc_driver-0123456789abcdef.dylib lib/librustc_driver-0123456789abcdef.dylib")),
        "{record}"
    );
}

#[cfg(unix)]
#[test]
fn explicit_donor_libraries_resolve_portable_and_transitive_rpaths() {
    let (_dir, d) = scratch("portable-donor");
    let stage = d.join("stage");
    let seal = d.join("seal");
    std::fs::create_dir_all(seal.join("lib")).unwrap();
    let driver = "librustc_driver-0123456789abcdef.dylib";
    let runtime = "libstd-0123456789abcdef.dylib";
    for name in [driver, runtime] {
        std::fs::write(seal.join("lib").join(name), format!("OBJ {name}")).unwrap();
    }
    let tool = stage.join("bin/tool");
    std::fs::write(&tool, b"OBJ tool").unwrap();
    let backend = ScriptedBackend {
        refs: BTreeMap::from([
            (
                tool,
                ObjectRefs {
                    needed: vec![format!("@rpath/{driver}")],
                    rpaths: vec!["@loader_path/../toolchain/lib".into()],
                    ..ObjectRefs::default()
                },
            ),
            (
                stage.join(VENDOR_REL).join(driver),
                ObjectRefs {
                    needed: vec![format!("@rpath/{runtime}")],
                    rpaths: vec!["@loader_path".into()],
                    ..ObjectRefs::default()
                },
            ),
        ]),
    };
    let report = relocate_with(&stage, &backend, None, std::slice::from_ref(&seal)).unwrap();
    assert!(report.unresolved.is_empty(), "{:?}", report.unresolved);
    assert_eq!(report.vendored.len(), 2);
    assert_eq!(report.vendored_from.len(), 2);
    for name in [driver, runtime] {
        let original = seal.join("lib").join(name);
        let record = report
            .vendored_from
            .iter()
            .find(|v| v.name == name)
            .unwrap();
        assert_eq!(record.sha256, atpkg::tree::sha256_file(&original).unwrap());
        assert_eq!(record.donor_rel, format!("lib/{name}"));
        let original_bytes = std::fs::read(original).unwrap();
        let copy = std::fs::read(stage.join(VENDOR_REL).join(name)).unwrap();
        assert!(copy.starts_with(&original_bytes));
        assert!(copy.ends_with(b"|sig"));
    }
}

#[cfg(unix)]
#[test]
fn missing_portable_dependencies_are_reported_without_ambient_search() {
    let (_dir, d) = scratch("missing-portable");
    let stage = d.join("stage");
    let seal = d.join("seal");
    std::fs::create_dir_all(seal.join("lib")).unwrap();
    std::fs::create_dir_all(d.join("ambient/lib")).unwrap();
    std::fs::write(d.join("ambient/lib/libmissing.dylib"), b"OBJ ambient").unwrap();
    let tool = stage.join("bin/tool");
    std::fs::write(&tool, b"OBJ tool").unwrap();
    let backend = ScriptedBackend {
        refs: BTreeMap::from([(
            tool,
            ObjectRefs {
                needed: vec!["@rpath/libmissing.dylib".into()],
                rpaths: vec!["@loader_path/../toolchain/lib".into()],
                ..ObjectRefs::default()
            },
        )]),
    };
    for donors in [vec![], vec![seal]] {
        let report = relocate_with(&stage, &backend, None, &donors).unwrap();
        assert_eq!(report.unresolved.len(), 1);
        assert!(report.unresolved[0].contains("@rpath/libmissing.dylib is absent"));
        assert!(report.vendored.is_empty());
        assert!(!stage.join(VENDOR_REL).exists());
    }
}

#[cfg(unix)]
#[test]
fn explicit_donor_discovery_cannot_follow_a_library_outside_its_root() {
    let (_dir, d) = scratch("portable-escape");
    let stage = d.join("stage");
    let seal = d.join("seal");
    std::fs::create_dir_all(seal.join("lib")).unwrap();
    let outside = d.join("outside.dylib");
    std::fs::write(&outside, b"OBJ outside").unwrap();
    std::os::unix::fs::symlink(&outside, seal.join("lib/libescape.dylib")).unwrap();
    let tool = stage.join("bin/tool");
    std::fs::write(&tool, b"OBJ tool").unwrap();
    let backend = ScriptedBackend {
        refs: BTreeMap::from([(
            tool,
            ObjectRefs {
                needed: vec!["@rpath/libescape.dylib".into()],
                ..ObjectRefs::default()
            },
        )]),
    };
    let report = relocate_with(&stage, &backend, None, std::slice::from_ref(&seal)).unwrap();
    assert_eq!(report.unresolved.len(), 1);
    assert!(report.unresolved[0].contains("is not inside a declared donor root"));
    assert!(report.vendored.is_empty());
    assert!(!stage.join(VENDOR_REL).exists());
}

/// A library that exists outside every declared donor root is left unresolved and not
/// one byte is copied.
#[cfg(unix)]
#[test]
fn a_library_outside_every_donor_root_is_never_vendored() {
    let (_dir, d) = scratch("undeclared");
    let stage = d.join("stage");
    let seal = d.join("seal");
    std::fs::create_dir_all(seal.join("lib")).unwrap();
    let outside = d.join("elsewhere/libfoo.dylib");
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    std::fs::write(&outside, b"OBJ builder-local foo").unwrap();
    let vendored = stage.join(VENDOR_REL);
    let undeclared = |report: &Report| {
        report.unresolved.len() == 1
            && report.unresolved[0].starts_with("libfoo.dylib: ")
            && report.unresolved[0].contains("is not inside a declared donor root")
    };

    let backend = ScriptedBackend {
        refs: BTreeMap::new(),
    };
    let mut have = BTreeSet::new();
    let mut report = Report::default();
    let copied = vendor_file(
        &outside,
        &VendorTo {
            dir: &vendored,
            backend: &backend,
            donor_roots: std::slice::from_ref(&seal),
        },
        "libfoo.dylib",
        &mut have,
        &mut report,
    )
    .expect("an undeclared library is unresolved, never an error");
    assert!(!copied, "a library outside every donor root was vendored");
    assert!(undeclared(&report), "{:?}", report.unresolved);
    assert!(report.vendored.is_empty() && report.vendored_from.is_empty());
    assert!(have.is_empty());
    assert!(!vendored.exists(), "nothing may be written to {VENDOR_REL}");

    std::os::unix::fs::symlink(&outside, seal.join("lib/libfoo.dylib")).unwrap();
    let tool = stage.join("bin/tool");
    std::fs::write(&tool, b"OBJ tool").unwrap();
    let backend = ScriptedBackend {
        refs: BTreeMap::from([(
            tool,
            ObjectRefs {
                needed: vec!["@rpath/libfoo.dylib".to_string()],
                rpaths: vec![seal.join("lib").display().to_string()],
                ..ObjectRefs::default()
            },
        )]),
    };
    let report =
        relocate_with(&stage, &backend, None, std::slice::from_ref(&seal)).expect("relocate");
    assert!(report.vendored.is_empty(), "{:?}", report.vendored);
    assert!(undeclared(&report), "{:?}", report.unresolved);
    assert!(report.require_self_contained().is_err());
    assert!(
        !vendored.join("libfoo.dylib").exists(),
        "nothing may be written to {VENDOR_REL}"
    );
}

/// A hard-link group leaves relocation as one file: rewritten and signed once under its
/// first name, the other names re-linked onto the result.
#[cfg(unix)]
#[test]
fn a_hard_link_group_is_relocated_once_and_stays_one_file() {
    use std::os::unix::fs::MetadataExt;
    let scratch = aterm_tempfile::Builder::new()
        .prefix("atpkg-reloc-hardlink-")
        .tempdir()
        .unwrap();
    let stage = scratch.path().to_path_buf();
    std::fs::create_dir_all(stage.join("bin")).unwrap();
    std::fs::create_dir_all(stage.join("lib/rustlib/t/lib")).unwrap();
    std::fs::write(stage.join("bin/rustc"), b"OBJ compiler").unwrap();
    std::fs::hard_link(stage.join("bin/rustc"), stage.join("bin/trustc")).unwrap();
    std::fs::write(stage.join("lib/libdriver.dylib"), b"OBJ driver").unwrap();
    std::fs::hard_link(
        stage.join("lib/libdriver.dylib"),
        stage.join("lib/rustlib/t/lib/libdriver.dylib"),
    )
    .unwrap();
    std::fs::write(stage.join("bin/solo"), b"OBJ solo").unwrap();

    let backend = ReplacingBackend {
        log: std::cell::RefCell::default(),
    };
    let report = relocate_with(&stage, &backend, None, &[]).unwrap();
    assert!(report.unresolved.is_empty(), "{:?}", report.unresolved);
    assert_eq!(report.rewritten, 3, "one rewrite per object, not per name");

    let ino = |rel: &str| std::fs::metadata(stage.join(rel)).unwrap().ino();
    let text = |rel: &str| std::fs::read_to_string(stage.join(rel)).unwrap();
    assert_eq!(
        ino("bin/rustc"),
        ino("bin/trustc"),
        "bin/rustc must still be bin/trustc"
    );
    assert_eq!(
        text("bin/trustc"),
        "OBJ compiler|rpath=@loader_path/../lib/atpkg-vendored|sig=rustc"
    );
    assert_eq!(
        ino("lib/libdriver.dylib"),
        ino("lib/rustlib/t/lib/libdriver.dylib"),
        "the dylib must not ship twice"
    );
    assert_eq!(
        text("lib/rustlib/t/lib/libdriver.dylib"),
        "OBJ driver|rpath=@loader_path/atpkg-vendored\
         |rpath=@loader_path/../../../atpkg-vendored|sig=libdriver.dylib"
    );
    assert_eq!(
        text("bin/solo"),
        "OBJ solo|rpath=@loader_path/../lib/atpkg-vendored|sig=solo"
    );
    let log = backend.log.borrow();
    assert_eq!(
        log.iter().filter(|l| l.starts_with("sig=")).count(),
        3,
        "{log:?}"
    );
    assert!(
        !log.iter().any(|l| l.starts_with("sig=trustc")),
        "trustc must not be signed under its own name: {log:?}"
    );
    let leftovers: Vec<PathBuf> = walk_files(&stage)
        .unwrap()
        .into_iter()
        .filter(|p| p.to_string_lossy().contains("atpkg-relink"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "re-link scratch left behind: {leftovers:?}"
    );
    let _ = std::fs::remove_dir_all(&stage);
}

#[cfg(unix)]
#[test]
fn relinking_never_consumes_a_hidden_primary_as_scratch() {
    super::super::relink_tests::preserves_payload(
        super::super::relink_tests::Collision::Primary,
        relink_aliases,
    );
}

#[cfg(unix)]
#[test]
fn relinking_preserves_an_unrelated_occupied_legacy_name() {
    super::super::relink_tests::preserves_payload(
        super::super::relink_tests::Collision::Payload,
        relink_aliases,
    );
}
