// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

struct Fixture {
    root: aterm_tempfile::TempDir,
    cargo_home: PathBuf,
    package: PathBuf,
    lock: String,
}

impl Fixture {
    fn new() -> Self {
        if let Err(none) = resolve_targo() {
            aterm_spec::host::could_not_run(format!(
                "locked metadata requires the Trust toolchain: {none}"
            ));
        }
        let root = aterm_tempfile::Builder::new()
            .prefix("aterm-locked-metadata-")
            .tempdir()
            .unwrap();
        let repo = root.path();
        let cargo_home = repo.join("cargo-home");
        let package = repo.join("vendor/offline-fixture-1.0.0");
        for path in [repo.join("src"), repo.join(".cargo"), package.join("src")] {
            fs::create_dir_all(path).unwrap();
        }
        fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"lock-gate-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[workspace]\n\n[dependencies]\noffline-fixture = \"=1.0.0\"\n",
        )
        .unwrap();
        fs::write(repo.join("src/lib.rs"), "").unwrap();
        fs::write(
            repo.join(".cargo/config.toml"),
            "[source.crates-io]\nreplace-with = \"fixture\"\n\n[source.fixture]\ndirectory = \"vendor\"\n",
        )
        .unwrap();
        fs::write(
            package.join("Cargo.toml"),
            "[package]\nname = \"offline-fixture\"\nversion = \"1.0.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        fs::write(package.join("src/lib.rs"), "").unwrap();
        let checksum = "0".repeat(64);
        fs::write(
            package.join(".cargo-checksum.json"),
            format!("{{\"files\":{{}},\"package\":\"{checksum}\"}}"),
        )
        .unwrap();
        let lock = format!(
            "version = 4\n\n[[package]]\nname = \"lock-gate-fixture\"\nversion = \"0.1.0\"\ndependencies = [\"offline-fixture\"]\n\n[[package]]\nname = \"offline-fixture\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"{checksum}\"\n"
        );
        fs::write(repo.join("Cargo.lock"), &lock).unwrap();
        Self {
            root,
            cargo_home,
            package,
            lock,
        }
    }

    fn check(&self) -> std::result::Result<(), LockedMetadataError> {
        locked_metadata_gate_with(self.root.path(), Some(&self.cargo_home))
    }

    fn assert_lock(&self, expected: &str) {
        assert_eq!(
            fs::read_to_string(self.root.path().join("Cargo.lock")).unwrap(),
            expected
        );
    }
}

#[test]
fn exact_offline_registry_resolution_passes_without_touching_the_lock() {
    let fixture = Fixture::new();
    fixture.check().unwrap();
    fixture.assert_lock(&fixture.lock);
}

#[test]
fn missing_offline_registry_dependency_does_not_prove_a_stale_lock() {
    let fixture = Fixture::new();
    fixture.check().unwrap();
    fs::rename(&fixture.package, fixture.root.path().join("unavailable")).unwrap();
    let error = fixture.check().unwrap_err();
    assert!(
        matches!(error, LockedMetadataError::Unavailable(_)),
        "{error}"
    );
    fixture.assert_lock(&fixture.lock);
}

#[test]
fn missing_and_stale_locks_both_keep_admission_closed() {
    let fixture = Fixture::new();
    let path = fixture.root.path().join("Cargo.lock");
    fs::remove_file(&path).unwrap();
    let missing = fixture.check().unwrap_err();
    assert!(matches!(missing, LockedMetadataError::Lockfile(_)));
    assert!(!path.exists());
    let stale = fixture.lock.replacen("0.1.0", "0.0.9", 1);
    fs::write(&path, &stale).unwrap();
    let error = fixture.check().unwrap_err();
    assert!(
        matches!(error, LockedMetadataError::Unavailable(_)),
        "{error}"
    );
    fixture.assert_lock(&stale);
}
