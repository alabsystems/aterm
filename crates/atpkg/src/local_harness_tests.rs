// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Text/byte fixtures only: no harness, native image or compiler is ever executed.

use super::*;
use crate::store::Layout;

const PROGRAM: &str = "claude-code-alab";
const COMMAND: &str = "claude-alab";
const SOURCE: &str = "0123456789012345678901234567890123456789";

struct World {
    _dir: crate::testdir::Dir,
    artifact: PathBuf,
    layout: Layout,
    receipt: aterm_json::Value,
}

impl World {
    fn new(label: &str) -> Self {
        let dir = crate::testdir::dir(label);
        let root = dir.canonicalize().unwrap();
        let artifact = root.join("artifact");
        std::fs::create_dir_all(artifact.join("bin")).unwrap();
        crate::platform::set_mode(&artifact, 0o700).unwrap();
        crate::platform::set_mode(&artifact.join("bin"), 0o700).unwrap();
        #[cfg(target_os = "macos")]
        let magic = b"\xcf\xfa\xed\xfe";
        #[cfg(not(target_os = "macos"))]
        let magic = b"\x7fELF";
        let mut bytes = magic.to_vec();
        bytes.extend_from_slice(b"mock bytes, never executed");
        let exe = artifact.join("bin").join(COMMAND);
        std::fs::write(&exe, &bytes).unwrap();
        crate::platform::set_mode(&exe, 0o700).unwrap();
        let receipt = aterm_json::json!({
            "schema":2, "origin":"local-build", "program":PROGRAM, "source":SOURCE,
            "build_number":42, "version":"0.1.0+g012345678", "source_policy":"clean-main",
            "command":COMMAND, "compiler":"a".repeat(40), "sysroot":"/mock/sysroot",
            "target":crate::cli::current_triple(), "policy":"unverified", "profile":"dev",
            "checkout":"/mock/checkout", "logs":"/mock/logs", "exposes":[COMMAND],
            "executables":[{"path":format!("bin/{COMMAND}"), "size":bytes.len(),
                "sha256":crate::tree::file_digest(&exe, crate::manifest::DigestAlg::Sha256).unwrap()}]
        });
        let world = Self {
            layout: Layout {
                prefix: root.join("pkg"),
            },
            _dir: dir,
            artifact,
            receipt,
        };
        world.write_receipt(&world.receipt);
        world
    }
    fn write_receipt(&self, receipt: &aterm_json::Value) {
        std::fs::write(
            self.artifact.join("local-build.json"),
            aterm_json::to_vec(receipt).unwrap(),
        )
        .unwrap();
    }
    fn authority(&self) -> LocalAuthority {
        LocalAuthority::new(PROGRAM, COMMAND, SOURCE, crate::cli::current_triple()).unwrap()
    }
    fn lock(&self) -> StoreLock {
        crate::lock::try_lock_store(&self.layout).unwrap()
    }
    fn build(&self) -> PathBuf {
        self.layout.build_dir(PROGRAM, 42)
    }
}

#[test]
fn the_producers_claude_package_and_distinct_command_stage_then_select_only_after_cas() {
    let w = World::new("local-harness-stage");
    let lock = w.lock();
    let vendor = w.layout.program_current("claude");
    std::fs::create_dir_all(vendor.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink("/untouched/vendor", &vendor).unwrap();
    let staged = stage(&lock, &w.authority(), &w.artifact).unwrap();
    assert_eq!(w.receipt["program"].as_str(), Some("claude-code-alab"));
    assert_eq!(w.receipt["command"].as_str(), Some("claude-alab"));
    assert!(staged.directory().join("bin/claude-alab").is_file());
    assert!(!staged.directory().join("bin/claude-code-alab").exists());
    assert_eq!(staged.directory(), w.build());
    assert!(crate::store::local_build_ready(&w.build()).unwrap());
    assert!(!w.layout.program_current(PROGRAM).exists());
    assert!(!w.layout.bin_dir().exists() && !w.layout.agents_dir().exists());
    select_current(&lock, &staged, ExpectedCurrent::Absent).unwrap();
    assert_eq!(
        std::fs::read_link(w.layout.program_current(PROGRAM)).unwrap(),
        w.build()
    );
    assert_eq!(
        std::fs::read_link(vendor).unwrap(),
        Path::new("/untouched/vendor")
    );
    assert!(!w.layout.bin_dir().exists() && !w.layout.agents_dir().exists());
}

#[test]
fn the_same_complete_local_receipt_reuses_its_first_publication_without_rewriting() {
    let w = World::new("local-harness-reuse");
    let lock = w.lock();
    stage(&lock, &w.authority(), &w.artifact).unwrap();
    let record = crate::store::sidecar_path(&w.build(), SUFFIX).unwrap();
    let before = (
        std::fs::metadata(&record).unwrap().ino(),
        std::fs::read(&record).unwrap(),
    );
    stage(&lock, &w.authority(), &w.artifact).unwrap();
    assert_eq!(
        (
            std::fs::metadata(&record).unwrap().ino(),
            std::fs::read(&record).unwrap()
        ),
        before
    );
    assert!(!w.layout.program_current(PROGRAM).exists());
}

#[test]
fn current_observation_is_positive_absence_then_checked_identity_without_reselection() {
    let w = World::new("local-harness-observe");
    let lock = w.lock();
    assert_eq!(
        observe_current(&lock, PROGRAM).unwrap(),
        CurrentLocal::Absent
    );
    let staged = stage(&lock, &w.authority(), &w.artifact).unwrap();
    assert_eq!(
        observe_current(&lock, PROGRAM).unwrap(),
        CurrentLocal::Absent
    );
    select_current(&lock, &staged, ExpectedCurrent::Absent).unwrap();
    let observed = observe_current(&lock, PROGRAM).unwrap();
    assert_eq!(observed, CurrentLocal::Selected(staged.identity()));
    assert_eq!(observed.expected(), ExpectedCurrent::Build(42));
    assert_eq!(staged.identity().program, PROGRAM);
    assert_eq!(staged.identity().command, COMMAND);
    assert_eq!(staged.identity().source, SOURCE);
    assert!(!w.layout.bin_dir().exists() && !w.layout.agents_dir().exists());
}

#[test]
fn observation_never_converts_foreign_incomplete_or_changed_current_to_absence() {
    for fault in [
        "foreign",
        "nonlink",
        "record",
        "ready",
        "bytes",
        "empty-executables",
        "command-mismatch",
    ] {
        let w = World::new("local-harness-observe-refuse");
        let lock = w.lock();
        let staged = stage(&lock, &w.authority(), &w.artifact).unwrap();
        select_current(&lock, &staged, ExpectedCurrent::Absent).unwrap();
        match fault {
            "foreign" | "nonlink" => {
                std::fs::remove_file(w.layout.program_current(PROGRAM)).unwrap();
                if fault == "foreign" {
                    std::os::unix::fs::symlink(
                        w.layout.build_dir("other-alab", 42),
                        w.layout.program_current(PROGRAM),
                    )
                    .unwrap();
                } else {
                    std::fs::write(w.layout.program_current(PROGRAM), b"42").unwrap();
                }
            }
            "record" => std::fs::write(
                crate::store::sidecar_path(&w.build(), SUFFIX).unwrap(),
                b"{}",
            )
            .unwrap(),
            "ready" => {
                std::fs::remove_file(crate::store::sidecar_path(&w.build(), ".ready").unwrap())
                    .unwrap()
            }
            "bytes" => {
                std::fs::write(w.build().join("bin").join(COMMAND), b"changed payload").unwrap()
            }
            "empty-executables" => {
                let mut record: Record = aterm_json::from_slice(
                    &std::fs::read(crate::store::sidecar_path(&w.build(), SUFFIX).unwrap())
                        .unwrap(),
                )
                .unwrap();
                record.receipt.executables.clear();
                std::fs::write(
                    crate::store::sidecar_path(&w.build(), SUFFIX).unwrap(),
                    aterm_json::to_vec(&record).unwrap(),
                )
                .unwrap();
            }
            "command-mismatch" => {
                let mut record: Record = aterm_json::from_slice(
                    &std::fs::read(crate::store::sidecar_path(&w.build(), SUFFIX).unwrap())
                        .unwrap(),
                )
                .unwrap();
                record.receipt.command = PROGRAM.to_string();
                record.receipt.exposes = vec![PROGRAM.to_string()];
                record.receipt.executables[0].path = format!("bin/{PROGRAM}");
                std::fs::write(
                    crate::store::sidecar_path(&w.build(), SUFFIX).unwrap(),
                    aterm_json::to_vec(&record).unwrap(),
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        assert!(observe_current(&lock, PROGRAM).is_err(), "{fault}");
    }
}

#[test]
fn producer_source_stamps_may_extend_past_nine_digits_without_weakening_authority() {
    for length in [9, 10, SOURCE.len()] {
        let mut w = World::new("local-harness-source-stamp");
        let version = format!("0.1.0+g{}", &SOURCE[..length]);
        w.receipt
            .as_object_mut()
            .unwrap()
            .insert("version".into(), aterm_json::json!(version));
        w.write_receipt(&w.receipt);
        let lock = w.lock();
        let staged = stage(&lock, &w.authority(), &w.artifact).unwrap();
        assert_eq!(staged.record.receipt.source, SOURCE);
        assert_eq!(staged.record.receipt.version, version);
        select_current(&lock, &staged, ExpectedCurrent::Absent).unwrap();
        assert_eq!(
            std::fs::read_link(w.layout.program_current(PROGRAM)).unwrap(),
            w.build()
        );
    }

    for version in [
        format!("0.1.0+g{}", &SOURCE[..8]),
        format!("0.1.0+g{SOURCE}0"),
        "0.1.0+g012345678f".into(),
    ] {
        let mut w = World::new("local-harness-invalid-source-stamp");
        w.receipt
            .as_object_mut()
            .unwrap()
            .insert("version".into(), aterm_json::json!(version));
        w.write_receipt(&w.receipt);
        let lock = w.lock();
        assert!(stage(&lock, &w.authority(), &w.artifact).is_err());
        assert!(!w.build().exists());
        assert!(std::fs::symlink_metadata(w.layout.program_current(PROGRAM)).is_err());
    }
}

#[test]
fn local_authority_and_receipt_shape_are_required_before_store_publication() {
    let w = World::new("local-harness-malformed");
    let lock = w.lock();
    for (field, value) in [
        ("schema", aterm_json::json!(3)),
        ("source", aterm_json::json!("short")),
        ("target", aterm_json::json!("another-target")),
        ("version", aterm_json::json!("0.1.0+gfffffffff")),
        ("command", aterm_json::json!("claude")),
        ("build_number", aterm_json::json!(0)),
        ("compiler", aterm_json::json!("unknown")),
        ("extra", aterm_json::json!(true)),
    ] {
        let mut receipt = w.receipt.clone();
        receipt
            .as_object_mut()
            .unwrap()
            .insert(field.to_owned(), value);
        w.write_receipt(&receipt);
        assert!(
            stage(&lock, &w.authority(), &w.artifact).is_err(),
            "{field}"
        );
        assert!(!w.build().exists());
    }
    w.write_receipt(&w.receipt);
    let mut duplicate = aterm_json::to_vec(&w.receipt).unwrap();
    duplicate.pop();
    duplicate.extend_from_slice(b",\"schema\":2}");
    std::fs::write(w.artifact.join("local-build.json"), duplicate).unwrap();
    assert!(stage(&lock, &w.authority(), &w.artifact).is_err());
    assert!(!w.build().exists());
    w.write_receipt(&w.receipt);
    let wrong = LocalAuthority::new(
        PROGRAM,
        COMMAND,
        &"f".repeat(40),
        crate::cli::current_triple(),
    )
    .unwrap();
    assert!(stage(&lock, &wrong, &w.artifact).is_err());
    let wrong_command =
        LocalAuthority::new(PROGRAM, PROGRAM, SOURCE, crate::cli::current_triple()).unwrap();
    assert!(stage(&lock, &wrong_command, &w.artifact).is_err());
    assert!(!w.build().exists());
    assert!(LocalAuthority::new("claude", COMMAND, SOURCE, crate::cli::current_triple()).is_err());
    assert!(
        LocalAuthority::new(PROGRAM, "../claude", SOURCE, crate::cli::current_triple()).is_err()
    );
    assert!(LocalAuthority::new(PROGRAM, COMMAND, SOURCE, "another-target").is_err());
    std::fs::write(
        w.artifact.join("local-build.json"),
        vec![b' '; RECEIPT_MAX + 1],
    )
    .unwrap();
    assert!(stage(&lock, &w.authority(), &w.artifact).is_err());
    assert!(!w.build().exists());
}

#[test]
fn changed_bytes_and_linked_inputs_never_become_ready() {
    let w = World::new("local-harness-links");
    let lock = w.lock();
    let binary = w.artifact.join("bin").join(COMMAND);
    let original = std::fs::read(&binary).unwrap();
    std::fs::write(&binary, b"different bytes").unwrap();
    assert!(stage(&lock, &w.authority(), &w.artifact).is_err());
    let outside = w.artifact.parent().unwrap().join("outside");
    std::fs::write(&outside, &original).unwrap();
    std::fs::remove_file(&binary).unwrap();
    std::os::unix::fs::symlink(&outside, &binary).unwrap();
    assert!(stage(&lock, &w.authority(), &w.artifact).is_err());
    assert_eq!(std::fs::read(&outside).unwrap(), original);
    assert!(!w.build().exists());
    std::fs::remove_file(&binary).unwrap();
    crate::platform::set_mode(&outside, 0o700).unwrap();
    std::fs::hard_link(&outside, &binary).unwrap();
    assert!(stage(&lock, &w.authority(), &w.artifact).is_err());
    assert_eq!(std::fs::read(&outside).unwrap(), original);
    assert!(!w.build().exists());
}

#[test]
fn old_builds_and_orphan_metadata_are_never_overwritten() {
    for orphan in [false, true] {
        let w = World::new("local-harness-collision");
        let lock = w.lock();
        std::fs::create_dir_all(w.build().parent().unwrap()).unwrap();
        let witness = if orphan {
            crate::store::sidecar_path(&w.build(), SUFFIX).unwrap()
        } else {
            std::fs::create_dir(w.build()).unwrap();
            w.build().join("old-image")
        };
        std::fs::write(&witness, b"first publication").unwrap();
        assert!(stage(&lock, &w.authority(), &w.artifact).is_err());
        assert_eq!(std::fs::read(witness).unwrap(), b"first publication");
        assert!(!w.layout.program_current(PROGRAM).exists());
    }
}

#[test]
fn current_compare_and_swap_refuses_stale_foreign_and_nonlink_entries() {
    let w = World::new("local-harness-cas");
    let lock = w.lock();
    let staged = stage(&lock, &w.authority(), &w.artifact).unwrap();
    let current = w.layout.program_current(PROGRAM);
    let old = w.layout.build_dir(PROGRAM, 41);
    std::os::unix::fs::symlink(&old, &current).unwrap();
    assert!(select_current(&lock, &staged, ExpectedCurrent::Build(40)).is_err());
    assert_eq!(std::fs::read_link(&current).unwrap(), old);
    std::fs::remove_file(&current).unwrap();
    let foreign = w.layout.build_dir("claude", 41);
    std::os::unix::fs::symlink(&foreign, &current).unwrap();
    assert!(select_current(&lock, &staged, ExpectedCurrent::Build(41)).is_err());
    assert_eq!(std::fs::read_link(&current).unwrap(), foreign);
    std::fs::remove_file(&current).unwrap();
    let descendant = old.join("bin").join(COMMAND);
    std::os::unix::fs::symlink(&descendant, &current).unwrap();
    assert!(select_current(&lock, &staged, ExpectedCurrent::Build(41)).is_err());
    assert_eq!(std::fs::read_link(&current).unwrap(), descendant);
    std::fs::remove_file(&current).unwrap();
    std::fs::write(&current, b"person's file").unwrap();
    assert!(select_current(&lock, &staged, ExpectedCurrent::Absent).is_err());
    assert_eq!(std::fs::read(&current).unwrap(), b"person's file");
    std::fs::remove_file(&current).unwrap();
    std::os::unix::fs::symlink(old, &current).unwrap();
    select_current(&lock, &staged, ExpectedCurrent::Build(41)).unwrap();
    assert_eq!(std::fs::read_link(&current).unwrap(), w.build());
}

#[test]
fn selecting_reproves_receipt_ready_bytes_and_store_lock() {
    for damaged in ["receipt", "ready", "binary"] {
        let w = World::new("local-harness-reproof");
        let lock = w.lock();
        let staged = stage(&lock, &w.authority(), &w.artifact).unwrap();
        let path = match damaged {
            "receipt" => crate::store::sidecar_path(&w.build(), SUFFIX).unwrap(),
            "ready" => crate::store::sidecar_path(&w.build(), ".ready").unwrap(),
            _ => w.build().join("bin").join(COMMAND),
        };
        std::fs::write(path, b"changed after stage").unwrap();
        assert!(select_current(&lock, &staged, ExpectedCurrent::Absent).is_err());
        assert!(!w.layout.program_current(PROGRAM).exists());
    }
    let w = World::new("local-harness-other-store");
    let other = World::new("local-harness-other-lock");
    let lock = w.lock();
    let staged = stage(&lock, &w.authority(), &w.artifact).unwrap();
    assert!(select_current(&other.lock(), &staged, ExpectedCurrent::Absent).is_err());
    assert!(!w.layout.program_current(PROGRAM).exists());
}

#[test]
fn a_metadata_collision_after_payload_publication_leaves_no_ready_or_current() {
    let w = World::new("local-harness-metadata-fault");
    let _lock = w.lock();
    let receipt = admit(&aterm_json::to_vec(&w.receipt).unwrap(), &w.authority()).unwrap();
    let blocked = crate::store::sidecar_path(&w.build(), crate::store::VERSION_SUFFIX).unwrap();
    let hook = |_: &Path| {
        std::fs::create_dir(&blocked).map_err(crate::StageError::Io)?;
        std::fs::write(blocked.join("keep"), b"not overwritten").map_err(crate::StageError::Io)
    };
    let records = |_: &str| {
        Ok(vec![
            (SUFFIX, b"not published as ready".to_vec()),
            (
                crate::store::VERSION_SUFFIX,
                b"version v1\n0.1.0\n".to_vec(),
            ),
            (
                crate::store::EXPOSES_SUFFIX,
                b"exposes v1\nclaude-alab\n".to_vec(),
            ),
        ])
    };
    let artifact: crate::Artifact = aterm_toml::from_str(&format!(
        "target={:?}\nkind=\"binary\"\npayload=\"raw-binary\"\nentry={COMMAND:?}\nsize={}\nsha256={:?}\n",
        receipt.target, receipt.executables[0].size,receipt.executables[0].sha256
    ))
    .unwrap();
    let input = std::fs::File::open(w.artifact.join(&receipt.executables[0].path)).unwrap();
    let hooks = crate::install::StageHooks {
        pre_swap: Some(&hook),
        sidecars: Some(&records),
        ..crate::install::StageHooks::NONE
    };
    let parent = fs::Parent::open(&_lock, PROGRAM, true).unwrap();
    assert!(
        crate::install::stage_local_binary(&parent, &artifact, input, &w.build(), &hooks).is_err()
    );
    assert_eq!(
        std::fs::read(blocked.join("keep")).unwrap(),
        b"not overwritten"
    );
    assert!(w.build().is_dir());
    assert!(
        !crate::store::sidecar_path(&w.build(), ".ready")
            .unwrap()
            .exists()
    );
    assert!(!w.layout.program_current(PROGRAM).exists());
}

#[test]
fn a_replaced_store_path_cannot_reuse_its_predecessors_writer_lock() {
    let w = World::new("local-harness-lock-replaced");
    let lock = w.lock();
    let old = w.layout.prefix.with_file_name("original-store");
    std::fs::rename(&w.layout.prefix, &old).unwrap();
    let replacement = crate::lock::try_lock_store(&w.layout).unwrap();
    assert!(stage(&lock, &w.authority(), &w.artifact).is_err());
    assert!(!w.build().exists());
    let staged = stage(&replacement, &w.authority(), &w.artifact).unwrap();
    select_current(&replacement, &staged, ExpectedCurrent::Absent).unwrap();
    assert!(!old.join("store").exists());
}

#[test]
fn a_late_payload_name_collision_preserves_the_first_publishers_bytes() {
    let w = World::new("local-harness-late-payload-collision");
    let _lock = w.lock();
    let hook = |_: &Path| {
        std::fs::create_dir(w.build()).map_err(crate::StageError::Io)?;
        std::fs::write(w.build().join("keep"), b"first publisher").map_err(crate::StageError::Io)
    };
    let records = |_: &str| {
        Ok(vec![
            (SUFFIX, Vec::new()),
            (crate::store::VERSION_SUFFIX, Vec::new()),
            (crate::store::EXPOSES_SUFFIX, Vec::new()),
        ])
    };
    let artifact: crate::Artifact = aterm_toml::from_str(&format!(
        "target={:?}\nkind=\"binary\"\npayload=\"raw-binary\"\nentry={COMMAND:?}\nsize={}\nsha256={:?}\n",
        crate::cli::current_triple(),
        w.receipt["executables"][0]["size"].as_u64().unwrap(),w.receipt["executables"][0]["sha256"].as_str().unwrap()
    ))
    .unwrap();
    let hooks = crate::install::StageHooks {
        pre_swap: Some(&hook),
        sidecars: Some(&records),
        ..crate::install::StageHooks::NONE
    };
    let input = std::fs::File::open(w.artifact.join("bin").join(COMMAND)).unwrap();
    let parent = fs::Parent::open(&_lock, PROGRAM, true).unwrap();
    assert!(
        crate::install::stage_local_binary(&parent, &artifact, input, &w.build(), &hooks).is_err()
    );
    assert_eq!(
        std::fs::read(w.build().join("keep")).unwrap(),
        b"first publisher"
    );
    assert!(
        !crate::store::sidecar_path(&w.build(), ".ready")
            .unwrap()
            .exists()
    );
}

#[test]
fn current_selection_refuses_dev_links_and_replaced_ancestor_directories() {
    let w = World::new("local-harness-selection-paths");
    let lock = w.lock();
    let staged = stage(&lock, &w.authority(), &w.artifact).unwrap();
    let marker = w.layout.link_marker(PROGRAM);
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    std::fs::write(&marker, b"person's dev checkout").unwrap();
    assert!(select_current(&lock, &staged, ExpectedCurrent::Absent).is_err());
    assert_eq!(std::fs::read(&marker).unwrap(), b"person's dev checkout");
    std::fs::remove_file(marker).unwrap();
    let program = w.build().parent().unwrap().to_path_buf();
    let moved = w.layout.prefix.join("original-program");
    std::fs::rename(&program, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &program).unwrap();
    assert!(select_current(&lock, &staged, ExpectedCurrent::Absent).is_err());
    assert!(!moved.join("current").exists());
}

struct Checkpoint;
impl Checkpoint {
    fn at(phase: &'static str, action: impl FnOnce() + 'static) -> Self {
        fs::CHECKPOINT.with(|slot| {
            assert!(slot.borrow().is_none());
            *slot.borrow_mut() = Some((phase, Box::new(action)));
        });
        Self
    }
    fn fired(&self) {
        fs::CHECKPOINT.with(|slot| {
            assert!(
                slot.borrow().is_none(),
                "actual mutation boundary was reached"
            )
        });
    }
}
impl Drop for Checkpoint {
    fn drop(&mut self) {
        fs::CHECKPOINT.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}

fn redirect(w: &World, phase: &'static str) -> (Checkpoint, PathBuf, PathBuf) {
    let program = w.build().parent().unwrap().to_path_buf();
    let retained = program.with_file_name("retained-program");
    let outside = w.artifact.parent().unwrap().join("outside-program");
    std::fs::create_dir(&outside).unwrap();
    crate::platform::set_mode(&outside, 0o700).unwrap();
    for name in ["current", "42.local-harness", "42.ready", "keep"] {
        std::fs::write(outside.join(name), format!("outside {name}")).unwrap();
    }
    let saved = retained.clone();
    let foreign = outside.clone();
    let checkpoint = Checkpoint::at(phase, move || {
        std::fs::rename(&program, &saved).unwrap();
        std::os::unix::fs::symlink(&foreign, &program).unwrap();
    });
    (checkpoint, retained, outside)
}
fn outside_unchanged(outside: &Path) {
    for name in ["current", "42.local-harness", "42.ready", "keep"] {
        assert_eq!(
            std::fs::read(outside.join(name)).unwrap(),
            format!("outside {name}").as_bytes()
        );
    }
    assert_eq!(
        std::fs::read_dir(outside).unwrap().count(),
        4,
        "no temp, incoming, metadata or current was written through the redirect"
    );
}

#[test]
fn late_parent_redirects_cannot_escape_local_staging_or_erase_its_orphans() {
    for phase in ["create", "publish", "publish-rename", "metadata"] {
        let w = World::new("local-harness-late-stage-parent");
        let lock = w.lock();
        let (checkpoint, retained, outside) = redirect(&w, phase);
        assert!(
            stage(&lock, &w.authority(), &w.artifact).is_err(),
            "{phase}"
        );
        checkpoint.fired();
        outside_unchanged(&outside);
        let scratch = retained.join(format!("42.incoming-{}", std::process::id()));
        match phase {
            "create" => assert!(!scratch.exists() && !retained.join("42").exists()),
            "publish" => assert!(
                scratch.join("bin").join(COMMAND).is_file() && !retained.join("42").exists()
            ),
            _ => assert!(
                retained.join("42/bin").join(COMMAND).is_file()
                    && !retained.join("42.ready").exists()
            ),
        }
    }
}

#[test]
fn late_parent_redirects_cannot_replace_an_outside_current_after_cas() {
    for phase in ["select", "select-rename"] {
        let w = World::new("local-harness-late-select-parent");
        let lock = w.lock();
        let staged = stage(&lock, &w.authority(), &w.artifact).unwrap();
        let (checkpoint, retained, outside) = redirect(&w, phase);
        assert!(select_current(&lock, &staged, ExpectedCurrent::Absent).is_err());
        checkpoint.fired();
        outside_unchanged(&outside);
        assert!(retained.join("42/bin").join(COMMAND).is_file());
        if phase == "select" {
            assert!(std::fs::symlink_metadata(retained.join("current")).is_err());
        } else {
            assert_eq!(
                std::fs::read_link(retained.join("current")).unwrap(),
                w.build()
            );
        }
    }
}

#[test]
fn exact_stage_inventory_refuses_empty_or_extra_root_and_bin_entries() {
    for damage in ["empty-root", "extra-root", "empty-bin", "extra-bin"] {
        let w = World::new("local-harness-exact-inventory");
        let lock = w.lock();
        let staged = stage(&lock, &w.authority(), &w.artifact).unwrap();
        let bin = staged.directory().join("bin");
        let record = crate::store::sidecar_path(staged.directory(), SUFFIX).unwrap();
        let metadata = std::fs::read(&record).unwrap();
        match damage {
            "extra-root" => std::fs::write(staged.directory().join("unexpected"), b"keep").unwrap(),
            "extra-bin" => {
                crate::platform::set_mode(&bin, 0o700).unwrap();
                std::fs::write(bin.join("unexpected"), b"keep").unwrap();
            }
            _ => {
                crate::platform::set_mode(&bin, 0o700).unwrap();
                std::fs::remove_file(bin.join(COMMAND)).unwrap();
                if damage == "empty-root" {
                    std::fs::remove_dir(&bin).unwrap();
                }
            }
        }
        assert!(
            select_current(&lock, &staged, ExpectedCurrent::Absent).is_err(),
            "{damage}"
        );
        assert!(std::fs::symlink_metadata(w.layout.program_current(PROGRAM)).is_err());
        assert_eq!(std::fs::read(record).unwrap(), metadata);
    }
}

#[test]
fn ordinary_read_and_search_modes_remain_compatible_with_local_custody() {
    let w = World::new("local-harness-nonwrite-modes");
    for path in [w.artifact.clone(), w.artifact.join("bin")] {
        crate::platform::set_mode(&path, 0o755).unwrap();
    }
    crate::platform::set_mode(&w.artifact.join("bin").join(COMMAND), 0o755).unwrap();
    crate::platform::set_mode(&w.artifact.join("local-build.json"), 0o644).unwrap();
    let lock = w.lock();
    let staged = stage(&lock, &w.authority(), &w.artifact).unwrap();
    select_current(&lock, &staged, ExpectedCurrent::Absent).unwrap();
    assert_eq!(
        std::fs::read_link(w.layout.program_current(PROGRAM)).unwrap(),
        w.build()
    );
}

#[cfg(target_vendor = "apple")]
#[path = "local_harness_acl_tests.rs"]
mod acl;
