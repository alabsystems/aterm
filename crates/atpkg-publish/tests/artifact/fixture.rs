// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The artifact tests' fixtures: object headers written byte by byte (so the aarch64 cases
//! run on an x86_64 box, the Mach-O cases on Linux and the PE cases everywhere), trust
//! checkouts with bare origins, and a compiler the client's lineage checker admits.

use std::path::{Path, PathBuf};

pub use super::super::shared::scratch;

/// `ELF64` header bytes: `e_machine`, `EI_CLASS`, `EI_DATA`, padded to 64 bytes.
#[must_use]
pub fn elf(machine: u16, class: u8, data: u8) -> Vec<u8> {
    let mut b = b"\x7fELF".to_vec();
    b.extend([class, data, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    b.extend(2u16.to_le_bytes());
    b.extend(machine.to_le_bytes());
    b.extend([1, 0, 0, 0]);
    b.resize(64, 0);
    b
}

/// A thin 64-bit Mach-O header, little-endian on disk.
#[must_use]
pub fn macho(cputype: u32) -> Vec<u8> {
    let mut b = vec![0xcf, 0xfa, 0xed, 0xfe];
    b.extend(cputype.to_le_bytes());
    b.extend([0; 4]);
    b.extend(2u32.to_le_bytes());
    b.resize(32, 0);
    b
}

/// A fat Mach-O head (`FAT_MAGIC`, big-endian) with one 20-byte `fat_arch` per slice.
#[must_use]
pub fn fat(cputypes: &[u32]) -> Vec<u8> {
    let mut b = vec![0xca, 0xfe, 0xba, 0xbe];
    b.extend(u32::try_from(cputypes.len()).expect("few").to_be_bytes());
    for c in cputypes {
        b.extend(c.to_be_bytes());
        b.extend([0; 16]);
    }
    b
}

/// A PE image head: `MZ`, `e_lfanew` = 0x80, `PE\0\0` and the COFF `Machine`.
#[must_use]
pub fn pe(machine: u16) -> Vec<u8> {
    let mut b = b"MZ".to_vec();
    b.resize(0x3c, 0);
    b.extend(0x80u32.to_le_bytes());
    b.resize(0x80, 0);
    b.extend(b"PE\0\0");
    b.extend(machine.to_le_bytes());
    b.resize(0x80 + 24, 0);
    b
}

/// `CPU_TYPE_X86_64`.
pub const CPU_X86_64: u32 = 0x0100_0007;
/// `CPU_TYPE_ARM64`.
pub const CPU_ARM64: u32 = 0x0100_000c;
/// `EM_X86_64`.
pub const EM_X86_64: u16 = 62;
/// `EM_AARCH64`.
pub const EM_AARCH64: u16 = 183;
/// `IMAGE_FILE_MACHINE_AMD64`.
pub const PE_AMD64: u16 = 0x8664;
/// `IMAGE_FILE_MACHINE_ARM64`.
pub const PE_ARM64: u16 = 0xaa64;

/// Write `bytes` to `path` (parents made), mode 0755.
pub fn put_exe(path: &Path, bytes: &[u8]) -> PathBuf {
    put(path, bytes);
    set_mode(path, 0o755);
    path.to_path_buf()
}

/// Write `bytes` to `path` (parents made).
pub fn put(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
    std::fs::write(path, bytes).expect("write fixture");
}

/// chmod.
pub fn set_mode(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
}

/// `git -C <dir> <args…>` with a fixed identity; panics on failure.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = aterm_uds::spawnfd::command("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The seed ledger with no minted re-pin (the receipt switch reads dormant).
pub const DORMANT_LEDGER: &str = "[release]\ntag = \"fixture\"\n";
/// The seed ledger once the first minted re-pin landed (the switch is live).
pub const MINTED_LEDGER: &str =
    "[release]\ntag = \"fixture\"\n\n[release.mint]\nddc_seed_relation = \"pin-ancestor\"\n";
/// Where the switch reads the ledger.
pub const LEDGER: &str = "bootstrap/trust-stage0/seed-ledger.toml";

/// A trust checkout at `dir` whose origin (the bare `<dir>.git`) has `ledger` on main,
/// fetched — the receipt switch's input.
pub fn trust_repo(dir: &Path, ledger: &str) -> PathBuf {
    let bare = dir.with_extension("git");
    std::fs::create_dir_all(&bare).expect("mkdir bare");
    git(&bare, &["init", "-q", "--bare"]);
    std::fs::create_dir_all(dir).expect("mkdir checkout");
    git(dir, &["init", "-q", "."]);
    git(dir, &["remote", "add", "origin", &bare.to_string_lossy()]);
    put(&dir.join(LEDGER), ledger.as_bytes());
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "ledger"]);
    git(dir, &["push", "-q", "origin", "HEAD:refs/heads/main"]);
    git(dir, &["fetch", "-q", "origin"]);
    bare
}

/// Push a commit to `bare`'s main from a scratch clone that writes `ledger` (or a
/// README when `None`), leaving every other checkout of it behind.
pub fn push_from_elsewhere(bare: &Path, scratch: &Path, ledger: Option<&str>) {
    let pusher = scratch.join("pusher");
    git(
        scratch,
        &[
            "clone",
            "-q",
            "-b",
            "main",
            &bare.to_string_lossy(),
            &pusher.to_string_lossy(),
        ],
    );
    match ledger {
        Some(l) => put(&pusher.join(LEDGER), l.as_bytes()),
        None => put(&pusher.join("README"), b"unrelated\n"),
    }
    git(&pusher, &["add", "-A"]);
    git(&pusher, &["commit", "-q", "-m", "pushed"]);
    git(&pusher, &["push", "-q", "origin", "HEAD:refs/heads/main"]);
}

/// What a client does with a pack's pair: fetch and verify the channel head, then verify
/// and parse the manifest under the roster that authorized it.
///
/// # Panics
///
/// When the head does not verify; the client's verdict on the pair is returned.
pub fn client_verifies(
    ctx: &atpkg_publish::ctx::Ctx,
    pkg: &Path,
    sig: &Path,
) -> atpkg_publish::error::Result<atpkg::PkgManifest> {
    let dir = aterm_tempfile::Builder::new()
        .prefix("atpkg-client-head-")
        .tempdir()
        .expect("scratch");
    let (_, head) = atpkg_publish::quad::fetch_head(ctx, &ctx.index_slug(), dir.path())
        .expect("the channel head verifies");
    head.verify_pkg(
        &std::fs::read(pkg).expect("pkg"),
        &std::fs::read(sig).expect("sig"),
    )
}

/// The commit every fixture compiler reports.
pub const COMMIT: &str = "1234567890abcdef1234567890abcdef12345678";

/// `trustc -vV` of a fixture Trust compiler for `host`.
#[must_use]
pub fn version_text(commit: &str, host: &str) -> String {
    format!(
        "rustc 1.99.0-dev ({short} 2026-09-30) (trustc 0.1.0)\nbinary: trustc\ncommit-hash: \
         {commit}\ncommit-date: 2026-09-30\nhost: {host}\nrelease: 1.99.0-dev\ntrust: 0.1.0\n",
        short = &commit[..9]
    )
}

/// A toolchain the client's content checker admits when its `-vV` probe answers
/// [`version_text`]: native-object `bin/trustc` and `bin/targo` (headers only) and one
/// `lib/librustc_driver-*.so`. Returns `<root>/bin`.
pub fn toolchain(root: &Path) -> PathBuf {
    let head = elf(EM_X86_64, 2, 1);
    let mut trustc = head.clone();
    trustc.extend(b"fixture trustc");
    put_exe(&root.join("bin/trustc"), &trustc);
    let mut targo = head.clone();
    targo.extend(b"fixture targo");
    put_exe(&root.join("bin/targo"), &targo);
    let mut driver = head;
    driver.extend(b"fixture driver");
    put(
        &root.join("lib/librustc_driver-0123456789abcdef.so"),
        &driver,
    );
    root.join("bin")
}

use std::collections::BTreeMap;
use std::sync::Mutex;

use atpkg_publish::artifact::tools::{Ran, Run, RunError, Tools};

/// What a fake answers for one run it does not otherwise know.
pub type Answer = Box<dyn Fn(&Run) -> Option<Ran> + Send + Sync>;

/// The pack's external programs, faked: `-vV` answers from a table (by canonical path,
/// else a default), every other run from [`FakeTools::answer`] (else exit 0, silent),
/// `lipo -thin` writes a thin Mach-O of the asked arch, and every call is logged.
pub struct FakeTools {
    versions: Mutex<BTreeMap<PathBuf, String>>,
    /// The `-vV` of a compiler the table does not name.
    pub default_version: Option<String>,
    /// Every other run.
    pub answer: Answer,
    /// Whether two Mach-Os are one program unsigned.
    pub same_unsigned: bool,
    /// The relocator's refusal, when it should refuse.
    pub relocate_refusal: Option<String>,
    log: Mutex<Vec<String>>,
    runs: Mutex<Vec<Run>>,
}

impl FakeTools {
    /// Every compiler answers [`version_text`] for `host`; every other run exits 0.
    #[must_use]
    pub fn new(host: &str) -> Self {
        Self {
            versions: Mutex::new(BTreeMap::new()),
            default_version: Some(version_text(COMMIT, host)),
            answer: Box::new(|_| None),
            same_unsigned: false,
            relocate_refusal: None,
            log: Mutex::new(Vec::new()),
            runs: Mutex::new(Vec::new()),
        }
    }

    /// `compiler -vV` answers `text`.
    pub fn version(&self, compiler: &Path, text: &str) {
        let key = std::fs::canonicalize(compiler).unwrap_or_else(|_| compiler.to_path_buf());
        self.versions
            .lock()
            .expect("versions")
            .insert(key, text.to_string());
    }

    /// Every call so far, in order (`run <program> <args…>`, `strip <file>`, …).
    #[must_use]
    pub fn log(&self) -> Vec<String> {
        self.log.lock().expect("log").clone()
    }

    /// Every child run so far, as asked.
    #[must_use]
    pub fn runs(&self) -> Vec<Run> {
        self.runs.lock().expect("runs").clone()
    }

    /// The runs whose first argument is `verb`.
    #[must_use]
    pub fn runs_of(&self, verb: &str) -> Vec<Run> {
        self.runs()
            .into_iter()
            .filter(|r| r.argv().iter().any(|a| a == verb))
            .collect()
    }

    fn note(&self, line: String) {
        self.log.lock().expect("log").push(line);
    }
}

/// Exit `code` with `stdout` and `stderr`.
#[must_use]
pub fn ran(code: i32, stdout: &str, stderr: &str) -> Ran {
    Ran {
        code: Some(code),
        stdout: stdout.as_bytes().to_vec(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

impl Tools for FakeTools {
    fn run(&self, run: &Run) -> Result<Ran, RunError> {
        self.runs.lock().expect("runs").push(run.clone());
        self.note(format!(
            "run {} {}",
            run.program.display(),
            run.argv().join(" ")
        ));
        if run.argv() == ["-vV"] {
            let key = std::fs::canonicalize(&run.program).unwrap_or_else(|_| run.program.clone());
            let text = self
                .versions
                .lock()
                .expect("versions")
                .get(&key)
                .cloned()
                .or_else(|| self.default_version.clone());
            return Ok(match text {
                Some(t) => ran(0, &t, ""),
                None => ran(1, "", "not a compiler"),
            });
        }
        Ok((self.answer)(run).unwrap_or_else(|| ran(0, "", "")))
    }

    fn strip(&self, file: &Path, locals_and_debug: bool) -> Result<(), String> {
        self.note(format!(
            "strip{} {}",
            if locals_and_debug { " -x -S" } else { "" },
            file.display()
        ));
        Ok(())
    }

    fn resign(&self, file: &Path) -> Result<(), String> {
        self.note(format!("resign {}", file.display()));
        Ok(())
    }

    fn same_unsigned(&self, a: &Path, b: &Path) -> Result<bool, String> {
        self.note(format!("same_unsigned {} {}", a.display(), b.display()));
        Ok(self.same_unsigned)
    }

    fn thin(&self, file: &Path, arch: &str, out: &Path) -> Result<(), String> {
        self.note(format!("thin {} {arch}", file.display()));
        let cpu = if arch == "arm64" {
            CPU_ARM64
        } else {
            CPU_X86_64
        };
        std::fs::write(out, macho(cpu)).map_err(|e| e.to_string())
    }

    fn relocate(
        &self,
        stage: &Path,
        donors: &[PathBuf],
    ) -> Result<atpkg_publish::artifact::relocate::Report, String> {
        self.note(format!(
            "relocate {} donors={}",
            stage.display(),
            donors.len()
        ));
        match &self.relocate_refusal {
            Some(why) => Err(why.clone()),
            None => Ok(atpkg_publish::artifact::relocate::Report::default()),
        }
    }
}

/// Copy `src` to `dst` (made), symlinks copied as symlinks and every file copied on its
/// own (hard links not kept).
pub fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("mkdir copy");
    for e in std::fs::read_dir(src).expect("read").flatten() {
        let (from, to) = (e.path(), dst.join(e.file_name()));
        let ft = std::fs::symlink_metadata(&from).expect("lstat").file_type();
        if ft.is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(std::fs::read_link(&from).expect("readlink"), &to)
                .expect("symlink");
        } else if ft.is_dir() {
            copy_tree(&from, &to);
        } else {
            std::fs::copy(&from, &to).expect("copy");
        }
    }
}

/// The closure fixtures' compiler commit and target.
pub const SEAL_COMMIT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
/// The closure fixtures' target.
pub const SEAL_TRIPLE: &str = "aarch64-unknown-linux-gnu";
/// The closure fixtures' compiler driver.
pub const SEAL_DRIVER: &str = "librustc_driver-fixture.so";

/// A promoter-shaped seal and its sibling rustc-private closure, as trust's
/// `scripts/promote-toolchain.sh` lays them out: `(seal, private)`.
pub fn seal_fixture(root: &Path) -> (PathBuf, PathBuf) {
    let source = root.join("seal");
    let private = root.join("rustc-private-sysroots/exact-fixture");
    // Native heads for the target, so the client's checker admits the compiler and the
    // arch gate reads its bin/.
    let native = |body: &str| {
        let mut b = elf(EM_AARCH64, 2, 1);
        b.extend(body.as_bytes());
        b
    };
    put_exe(&source.join("bin/trustc"), &native("fixture trustc"));
    put_exe(&source.join("bin/targo"), &native("fixture targo"));
    let driver = native("exact compiler driver\n");
    put(&source.join("lib").join(SEAL_DRIVER), &driver);
    let target = source.join(format!("lib/rustlib/{SEAL_TRIPLE}/lib"));
    for name in ["std", "core"] {
        put(
            &target.join(format!("lib{name}-fixture.rlib")),
            format!("selected {name}\n").as_bytes(),
        );
    }
    let rust_src = source.join("lib/rustlib/src/rust");
    put(
        &rust_src.join("library/core/src/lib.rs"),
        b"// frozen core\n",
    );
    put(
        &rust_src.join("compiler/rustc_driver/src/lib.rs"),
        b"// frozen driver\n",
    );
    std::fs::create_dir_all(source.join("lib/rustlib/rustc-src")).expect("mkdir");
    #[cfg(unix)]
    std::os::unix::fs::symlink("../src/rust", source.join("lib/rustlib/rustc-src/rust"))
        .expect("alias");
    put(
        &private.join(".rustc-commit-hash"),
        format!("{SEAL_COMMIT}\n").as_bytes(),
    );
    put(&private.join(SEAL_TRIPLE).join(SEAL_DRIVER), &driver);
    put(
        &private
            .join(SEAL_TRIPLE)
            .join("librustc_middle-fixture.rmeta"),
        b"matching private metadata\n",
    );
    put(
        &private.join("host/libproc_macro-fixture.rmeta"),
        b"matching host metadata\n",
    );
    copy_tree(
        &target,
        &private.join(format!("lib/rustlib/{SEAL_TRIPLE}/lib")),
    );
    let fingerprint = atpkg::sha256_file(&source.join("lib").join(SEAL_DRIVER)).expect("hash");
    put(
        &source.join("PROVENANCE"),
        format!(
            "reported_commit={SEAL_COMMIT}\nprivate_sysroot=exact-fixture\n\
             artifact_driver_name={SEAL_DRIVER}\nartifact_driver_sha256={fingerprint}\n"
        )
        .as_bytes(),
    );
    (source, private)
}

/// The seal compiler's `-vV`, as the closure reads it.
pub fn seal_probe(_: &Path) -> Result<String, String> {
    Ok(format!("commit-hash: {SEAL_COMMIT}\nhost: {SEAL_TRIPLE}\n"))
}

/// The trust repo's pack-cut receipt for the toolchain at `root`, minted for `commit`:
/// a pin-ancestor double compile with byte-identical stage2 trees, re-pinned, comparing
/// its `bin/trustc` and its one driver — what the client's lineage checker admits.
pub fn seed_mint(root: &Path, commit: &str) {
    let driver = std::fs::read_dir(root.join("lib"))
        .expect("lib")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| n.starts_with("librustc_driver-"))
        .expect("a driver");
    let sha = |rel: &str| atpkg::sha256_file(&root.join(rel)).expect("hash");
    let tree = "5".repeat(64);
    put(
        &root.join("SEED-MINT"),
        format!(
            "# SEED-MINT (fixture)\nschema=trust-seed-mint-v1\nsource_commit={commit}\n\
             ddc_seed_relation=pin-ancestor\nstage2_tree_sha256_pinned_seed={tree}\n\
             stage2_tree_sha256_ddc_seed={tree}\nddc_verdict=identical\n\
             repin_commit=3333333333333333333333333333333333333333\n\
             repin_compiler_git_commit_hash={commit}\ncompiler_file={} bin/trustc\n\
             compiler_file={} lib/{driver}\n",
            sha("bin/trustc"),
            sha(&format!("lib/{driver}"))
        )
        .as_bytes(),
    );
}
