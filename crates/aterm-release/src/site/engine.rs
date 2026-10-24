// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `/terminal` engine, built by `ship site` itself as Trust code (AGENTS.md: no shell
//! scripts), so the sync runs nothing the site repo carries but its `deploy.sh`
//! (docs/AUDIT-third-party-2026-10-15.md, the site-engine follow-up of wasm-platform WP.7).
//! In order:
//!
//! 1. the CPU module: `aterm-wasm` built in the clone of the public commit for wasm32,
//!    release profile, on the wasm32 pin `publish/config.sh` names (a STOCK EXCEPTION:
//!    Trust ships only its host std), `+simd128`, the builder's paths remapped out
//!    ([`World::build_module`]);
//! 2. its ES-module glue, from `verify glue`'s generator run in process
//!    ([`aterm_verify::wasm_glue::glue`]);
//! 3. the shipping `wasm-opt` pass over the bound module ([`World::optimise`]): a
//!    module it left unchanged is refused, so nothing ships pre-opt;
//! 4. refused when a remapped path survives in the module: a panic location must never
//!    show a visitor the builder's home;
//! 5. installed at `aterm_wasm.<sha12>.js` and `.wasm`, where `sha12` hashes the
//!    optimized WASM. Both names are immutable: existing non-link regular files must
//!    match exactly. A JS-only generator change under the same WASM hash refuses and
//!    needs a deliberate URL-contract change. The glue names the hashed module, and
//!    `boot.js` imports the atomically replaced stable `engine.js` pointer.
//!
//! The commit it was built from (`ENGINE_SOURCE`) is the sync's to write.

use std::io::Write;
use std::path::Path;

use aterm_verify::wasm_glue::{self, WASM_OPT_FLAGS};

use super::{World, say};

/// The CPU module's stem.
pub const STEM: &str = "aterm_wasm";

/// The CPU module's target.
pub const TARGET: &str = "wasm32-unknown-unknown";

/// The wasm32 target's rustflags with `+simd128` and path-prefix remaps.
#[must_use]
pub fn rustflags(clone: &Path, home: Option<&Path>) -> Vec<String> {
    let remap = |from: &Path, to: &str| format!("--remap-path-prefix={}={to}", from.display());
    let mut flags = vec!["-C".to_string(), "target-feature=+simd128".to_string()];
    flags.extend(home.map(|h| remap(&h.join(".cargo"), "/cargo")));
    flags.push(remap(clone, "/aterm"));
    flags.extend(home.map(|h| remap(h, "/build")));
    flags
}

fn usable_home(home: Option<&Path>) -> Option<&Path> {
    home.filter(|h| h.is_absolute() && h.parent().is_some())
}

/// Build the engine from `clone` on `pin` into `term`, binding and optimising in `work`
/// (a scratch directory of the sync's): the pair's hash.
///
/// # Errors
/// A build or `wasm-opt` that did not finish, a module the glue generator refuses, a
/// `wasm-opt` that changed nothing, a builder's path left in the module, a glue that
/// never names its module: all before `term` is written. Asset or pointer publication
/// can fail with the old pointer retained; new unreferenced assets may remain. A
/// retirement error occurs after the new pointer and complete assets are installed.
pub fn build(
    world: &mut dyn World,
    clone: &Path,
    pin: &str,
    home: Option<&Path>,
    work: &Path,
    term: &Path,
    out: &mut dyn Write,
) -> Result<String, String> {
    let home = usable_home(home);
    let resolved = clone.canonicalize().unwrap_or_else(|_| clone.to_path_buf());
    let module = world.build_module(&resolved, pin, &rustflags(&resolved, home), out)?;
    let mut files = wasm_glue::glue(&module, STEM)
        .map_err(|e| format!("verify glue's generator refused {STEM}.wasm: {e}"))?;
    let mut take = |name: String| {
        files
            .iter()
            .position(|(n, _)| *n == name)
            .map(|at| files.swap_remove(at).1)
            .ok_or_else(|| format!("verify glue's generator wrote no {name}"))
    };
    let js = String::from_utf8(take(format!("{STEM}.js"))?)
        .map_err(|_| format!("the glue {STEM}.js is not UTF-8"))?;
    let bound = take(format!("{STEM}_bg.wasm"))?;
    say(
        out,
        &format!(
            "engine: {STEM}.wasm bound by verify glue's generator (web); wasm-opt {}",
            WASM_OPT_FLAGS.join(" ")
        ),
    );
    std::fs::create_dir_all(work).map_err(|e| format!("cannot create {}: {e}", work.display()))?;
    let at = work.join(format!("{STEM}_bg.wasm"));
    std::fs::write(&at, &bound).map_err(|e| format!("cannot write {}: {e}", at.display()))?;
    world.optimise(&at, out)?;
    let wasm = std::fs::read(&at).map_err(|e| format!("cannot read {}: {e}", at.display()))?;
    if wasm == bound {
        return Err(format!(
            "wasm-opt left {} byte for byte as it was — the engine never ships pre-opt",
            at.display()
        ));
    }
    if let Some(path) = leaked(&wasm, home.into_iter().chain([resolved.as_path(), clone])) {
        return Err(format!(
            "refusing: {} survived the path remapping in the module — a visitor's panic \
             message would show it",
            path.display()
        ));
    }
    let hash =
        aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&wasm))[..12].to_string();
    let js = rename_module(&js, &format!("{STEM}.{hash}.wasm"))?;
    install(term, &hash, &js, &wasm)?;
    Ok(hash)
}

fn leaked<'p>(module: &[u8], paths: impl IntoIterator<Item = &'p Path>) -> Option<&'p Path> {
    paths.into_iter().find(|p| {
        let needle = p.as_os_str().as_encoded_bytes();
        !needle.is_empty() && module.windows(needle.len()).any(|w| w == needle)
    })
}

/// `js` with its module URL (`<STEM>_bg.wasm` between two quotes) renamed to `wasm`.
///
/// # Errors
/// The glue never names its module that way.
fn rename_module(js: &str, wasm: &str) -> Result<String, String> {
    let from = format!("{STEM}_bg.wasm");
    let quote = |c: Option<char>| matches!(c, Some('"' | '\''));
    let mut renamed = String::with_capacity(js.len());
    let (mut copied, mut free, mut found) = (0, 0, 0);
    for (at, _) in js.match_indices(&from) {
        let end = at + from.len();
        if at > free && quote(js[..at].chars().next_back()) && quote(js[end..].chars().next()) {
            renamed.push_str(&js[copied..at]);
            renamed.push_str(wasm);
            (copied, free, found) = (end, end + 1, found + 1);
        }
    }
    renamed.push_str(&js[copied..]);
    if found == 0 {
        return Err(format!(
            "the glue never names {from} as a string literal, so its module URL cannot be \
             pointed at {wasm}"
        ));
    }
    Ok(renamed)
}

/// What `engine.js` says for the pair `hash`: everything, and the default (`init`), from
/// the pair's glue.
#[must_use]
pub fn engine_js(hash: &str) -> String {
    format!(
        "export * from \"./{STEM}.{hash}.js\";\nexport {{ default }} from \"./{STEM}.{hash}.js\";\n"
    )
}

fn replaced(name: &str, keep: [&str; 2]) -> bool {
    let hashed = name.strip_prefix(&format!("{STEM}.")).is_some_and(|rest| {
        rest.strip_suffix(".js")
            .or_else(|| rest.strip_suffix(".wasm"))
            .is_some_and(valid_hash)
    });
    !keep.contains(&name)
        && (hashed || name == format!("{STEM}.js") || name == format!("{STEM}_bg.wasm"))
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 12
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

struct Stage {
    path: std::path::PathBuf,
    owned: bool,
}

impl Stage {
    fn write(term: &Path, name: &str, bytes: &[u8]) -> Result<Self, String> {
        let path = term.join(format!(".{name}.tmp.{}", std::process::id()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o644);
        }
        let mut file = options
            .open(&path)
            .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
        let stage = Self { path, owned: true };
        let written = file.write_all(bytes);
        #[cfg(unix)]
        let written = written.and_then(|()| {
            use std::os::unix::fs::PermissionsExt as _;
            file.set_permissions(std::fs::Permissions::from_mode(0o644))
        });
        drop(file);
        written.map_err(|e| format!("cannot write {}: {e}", stage.path.display()))?;
        Ok(stage)
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        if self.owned {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Existing immutable assets are opened without following their final link or blocking
/// on a FIFO. These are the same OS flags used by the store's owner-lock probe.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
fn open_asset(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    #[cfg(target_vendor = "apple")]
    const FLAGS: i32 = 0x0100 /* O_NOFOLLOW */ | 0x0004 /* O_NONBLOCK */;
    #[cfg(all(target_os = "linux", any(target_arch = "aarch64", target_arch = "arm")))]
    const FLAGS: i32 = 0o100_000 /* O_NOFOLLOW */ | 0o4000 /* O_NONBLOCK */;
    #[cfg(all(
        target_os = "linux",
        not(any(target_arch = "aarch64", target_arch = "arm"))
    ))]
    const FLAGS: i32 = 0o400_000 /* O_NOFOLLOW */ | 0o4000 /* O_NONBLOCK */;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FLAGS)
        .open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "engine asset is not a regular non-link file",
        ));
    }
    Ok(file)
}

#[cfg(windows)]
fn open_asset(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "engine asset is not a regular non-link file",
        ));
    }
    Ok(file)
}

#[cfg(not(any(target_vendor = "apple", target_os = "linux", windows)))]
fn open_asset(_path: &Path) -> std::io::Result<std::fs::File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "no non-link engine asset admission on this platform",
    ))
}

fn existing_asset(path: &Path, bytes: &[u8]) -> Result<bool, String> {
    use std::io::Read as _;
    let mut file = match open_asset(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => {
            return Err(format!(
                "cannot admit immutable asset {}: {e}",
                path.display()
            ));
        }
    };
    let different = || {
        format!(
            "refusing different bytes at immutable asset {}",
            path.display()
        )
    };
    if file
        .metadata()
        .map_err(|e| format!("cannot inspect {}: {e}", path.display()))?
        .len()
        != bytes.len() as u64
    {
        return Err(different());
    }
    let mut buffer = [0; 8192];
    for chunk in bytes.chunks(buffer.len()) {
        file.read_exact(&mut buffer[..chunk.len()])
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if &buffer[..chunk.len()] != chunk {
            return Err(different());
        }
    }
    let mut extra = [0];
    if file
        .read(&mut extra)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?
        != 0
    {
        return Err(different());
    }
    Ok(true)
}

fn publish_asset(term: &Path, name: &str, bytes: &[u8]) -> Result<(), String> {
    let path = term.join(name);
    if existing_asset(&path, bytes)? {
        return Ok(());
    }
    let stage = Stage::write(term, name, bytes)?;
    match std::fs::hard_link(&stage.path, &path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if existing_asset(&path, bytes)? {
                Ok(())
            } else {
                Err(format!(
                    "immutable asset {} disappeared during publication",
                    path.display()
                ))
            }
        }
        Err(e) => Err(format!("cannot publish {}: {e}", path.display())),
    }
}

fn install(term: &Path, hash: &str, js: &str, wasm: &[u8]) -> Result<(), String> {
    if !valid_hash(hash) {
        return Err("an engine asset hash must be twelve lowercase hexadecimal digits".to_string());
    }
    let js_name = format!("{STEM}.{hash}.js");
    let wasm_name = format!("{STEM}.{hash}.wasm");
    std::fs::create_dir_all(term).map_err(|e| format!("cannot create {}: {e}", term.display()))?;
    existing_asset(&term.join(&js_name), js.as_bytes())?;
    existing_asset(&term.join(&wasm_name), wasm)?;
    publish_asset(term, &js_name, js.as_bytes())?;
    publish_asset(term, &wasm_name, wasm)?;
    let mut pointer = Stage::write(term, "engine.js", engine_js(hash).as_bytes())?;
    std::fs::rename(&pointer.path, term.join("engine.js"))
        .map_err(|e| format!("cannot replace engine.js in {}: {e}", term.display()))?;
    pointer.owned = false;
    let entries =
        std::fs::read_dir(term).map_err(|e| format!("cannot read {}: {e}", term.display()))?;
    for entry in entries {
        let entry =
            entry.map_err(|e| format!("cannot read an entry in {}: {e}", term.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if replaced(&name, [&js_name, &wasm_name]) {
            std::fs::remove_file(entry.path())
                .map_err(|e| format!("cannot remove {}: {e}", entry.path().display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flags_are_the_site_scripts_in_its_order() {
        let flags = rustflags(Path::new("/tmp/c lone"), Some(Path::new("/Users//b")));
        assert_eq!(
            flags,
            [
                "-C",
                "target-feature=+simd128",
                "--remap-path-prefix=/Users//b/.cargo=/cargo",
                "--remap-path-prefix=/tmp/c lone=/aterm",
                "--remap-path-prefix=/Users//b=/build",
            ]
        );
        assert_eq!(
            rustflags(Path::new("/c"), None)[2..],
            ["--remap-path-prefix=/c=/aterm"]
        );
        assert_eq!(usable_home(Some(Path::new("/"))), None);
        assert_eq!(usable_home(Some(Path::new("relative"))), None);
    }

    #[test]
    fn a_path_anywhere_in_the_module_is_found() {
        let home = Path::new("/Users//b");
        let clone = Path::new("/var/x/clone");
        let mut module = b"\0asm\x01\0\0\0panicked at /aterm/src/lib.rs".to_vec();
        assert_eq!(leaked(&module, [home, clone]), None);
        module.extend_from_slice(b"/var/x/clone/crates/aterm-wasm/src/lib.rs");
        assert_eq!(leaked(&module, [home, clone]), Some(clone));
        assert_eq!(leaked(&module, [Path::new("")]), None);
    }

    #[test]
    fn the_module_url_is_renamed_in_either_quote_and_a_glue_without_one_is_refused() {
        let js = "a = new URL(\"aterm_wasm_bg.wasm\", import.meta.url);\nb('aterm_wasm_bg.wasm');\n\
                  c(\"aterm_wasm_bg.wasm');\n// aterm_wasm_bg.wasm stays in prose\n";
        assert_eq!(
            rename_module(js, "aterm_wasm.0123456789ab.wasm").unwrap(),
            "a = new URL(\"aterm_wasm.0123456789ab.wasm\", import.meta.url);\n\
             b('aterm_wasm.0123456789ab.wasm');\nc(\"aterm_wasm.0123456789ab.wasm');\n\
             // aterm_wasm_bg.wasm stays in prose\n"
        );
        assert_eq!(
            rename_module("'aterm_wasm_bg.wasm'aterm_wasm_bg.wasm'", "x.wasm").unwrap(),
            "'x.wasm'aterm_wasm_bg.wasm'"
        );
        let why = rename_module("load(`aterm_wasm_bg.wasm`)", "x.wasm").unwrap_err();
        assert!(why.contains("never names aterm_wasm_bg.wasm"), "{why}");
    }

    #[test]
    fn the_pair_replaces_every_other_engine_file_and_nothing_else() {
        let keep = ["aterm_wasm.0123456789ab.js", "aterm_wasm.0123456789ab.wasm"];
        for name in [
            "aterm_wasm.5e1b202b6f21.js",
            "aterm_wasm.5e1b202b6f21.wasm",
            "aterm_wasm.js",
            "aterm_wasm_bg.wasm",
        ] {
            assert!(replaced(name, keep), "{name}");
        }
        for name in keep.into_iter().chain([
            "engine.js",
            "ENGINE_SOURCE",
            "boot.js",
            "canary.dd580a62d497.island.wasm",
            "aterm_wasm.d.ts",
            "aterm_wasm.wasm",
        ]) {
            assert!(!replaced(name, keep), "{name}");
        }
        assert_eq!(
            engine_js("0123456789ab"),
            "export * from \"./aterm_wasm.0123456789ab.js\";\n\
             export { default } from \"./aterm_wasm.0123456789ab.js\";\n"
        );
    }

    #[test]
    fn installing_an_engine_preserves_files_outside_its_asset_schema() {
        let private = aterm_tempfile::Builder::new()
            .prefix("site-engine-schema-")
            .tempdir()
            .unwrap();
        let term = private.path();
        install(term, "111111111111", "old glue", b"old module").unwrap();
        for legacy in ["aterm_wasm.js", "aterm_wasm_bg.wasm"] {
            std::fs::write(term.join(legacy), b"legacy").unwrap();
        }
        let unowned = [
            "aterm_wasm.notes.js",
            "aterm_wasm.0123456789AB.js",
            "aterm_wasm.0123456789a.js",
            "aterm_wasm.0123456789abc.wasm",
            "aterm_wasm.0123456789az.js",
            "aterm_wasm.0123456789ab.js.map",
        ];
        for name in unowned {
            std::fs::write(term.join(name), b"private bytes").unwrap();
        }
        install(term, "222222222222", "new glue", b"new module").unwrap();
        for name in unowned {
            assert_eq!(
                std::fs::read(term.join(name)).ok(),
                Some(b"private bytes".to_vec()),
                "preserve {name}"
            );
        }
        for old in [
            "aterm_wasm.111111111111.js",
            "aterm_wasm.111111111111.wasm",
            "aterm_wasm.js",
            "aterm_wasm_bg.wasm",
        ] {
            assert!(!term.join(old).exists(), "retire {old}");
        }
        assert_eq!(
            std::fs::read(term.join("aterm_wasm.222222222222.js")).unwrap(),
            b"new glue"
        );
        assert_eq!(
            std::fs::read_to_string(term.join("engine.js")).unwrap(),
            engine_js("222222222222")
        );
    }

    #[test]
    #[cfg(unix)]
    fn replacing_the_engine_pointer_does_not_write_through_a_link() {
        let private = aterm_tempfile::Builder::new()
            .prefix("site-engine-pointer-")
            .tempdir()
            .unwrap();
        let term = private.path().join("term");
        install(&term, "111111111111", "old glue", b"old module").unwrap();
        let note = private.path().join("private-note");
        std::fs::write(&note, b"private bytes").unwrap();
        std::fs::remove_file(term.join("engine.js")).unwrap();
        std::os::unix::fs::symlink(&note, term.join("engine.js")).unwrap();
        let installed = install(&term, "222222222222", "new glue", b"new module");
        assert_eq!(
            std::fs::read(&note).unwrap(),
            b"private bytes",
            "the pointer's link target stays untouched"
        );
        installed.unwrap();
        assert!(
            std::fs::symlink_metadata(term.join("engine.js"))
                .unwrap()
                .file_type()
                .is_file()
        );
        assert_eq!(
            std::fs::read_to_string(term.join("engine.js")).unwrap(),
            engine_js("222222222222")
        );
    }

    #[test]
    fn a_pointer_stage_collision_keeps_the_old_engine_and_the_collision() {
        let private = aterm_tempfile::Builder::new()
            .prefix("site-engine-stage-")
            .tempdir()
            .unwrap();
        let term = private.path();
        install(term, "111111111111", "old glue", b"old module").unwrap();
        let before = std::fs::read(term.join("engine.js")).unwrap();
        let stage = term.join(format!(".engine.js.tmp.{}", std::process::id()));
        std::fs::write(&stage, b"private stage bytes").unwrap();
        let installed = install(term, "222222222222", "new glue", b"new module");
        assert_eq!(
            std::fs::read(&stage).ok(),
            Some(b"private stage bytes".to_vec()),
            "a collision stays untouched"
        );
        assert_eq!(
            std::fs::read(term.join("engine.js")).unwrap(),
            before,
            "a failed pointer stage leaves the old engine"
        );
        assert!(installed.is_err());
        assert_eq!(
            std::fs::read(term.join("aterm_wasm.111111111111.js")).unwrap(),
            b"old glue"
        );
        assert_eq!(
            std::fs::read(term.join("aterm_wasm.111111111111.wasm")).unwrap(),
            b"old module"
        );
    }

    #[test]
    fn an_existing_immutable_url_cannot_acquire_different_glue() {
        let private = aterm_tempfile::Builder::new()
            .prefix("site-engine-immutable-")
            .tempdir()
            .unwrap();
        let term = private.path();
        install(term, "111111111111", "old glue", b"same module").unwrap();
        install(term, "111111111111", "old glue", b"same module").unwrap();
        let before = std::fs::read(term.join("engine.js")).unwrap();
        let installed = install(term, "111111111111", "different glue", b"same module");
        assert_eq!(
            std::fs::read(term.join("aterm_wasm.111111111111.js")).unwrap(),
            b"old glue",
            "an immutable URL keeps its original bytes"
        );
        assert_eq!(
            std::fs::read(term.join("aterm_wasm.111111111111.wasm")).unwrap(),
            b"same module"
        );
        assert_eq!(std::fs::read(term.join("engine.js")).unwrap(), before);
        assert!(installed.is_err());
    }

    #[test]
    #[cfg(unix)]
    fn an_existing_immutable_asset_link_is_refused_without_writing_its_target() {
        let private = aterm_tempfile::Builder::new()
            .prefix("site-engine-asset-link-")
            .tempdir()
            .unwrap();
        let term = private.path().join("term");
        install(&term, "111111111111", "old glue", b"old module").unwrap();
        let before = std::fs::read(term.join("engine.js")).unwrap();
        let note = private.path().join("private-note");
        std::fs::write(&note, b"private bytes").unwrap();
        std::os::unix::fs::symlink(&note, term.join("aterm_wasm.222222222222.js")).unwrap();
        let installed = install(&term, "222222222222", "new glue", b"new module");
        assert_eq!(
            std::fs::read(&note).unwrap(),
            b"private bytes",
            "an asset link target stays untouched"
        );
        assert_eq!(std::fs::read(term.join("engine.js")).unwrap(), before);
        assert!(installed.is_err());
    }
}
