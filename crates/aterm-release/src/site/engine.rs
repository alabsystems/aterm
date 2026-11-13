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
//! 3. the bound module's identity record rewritten ([`aterm_stamp::rewrite`]) with the
//!    clone's `[workspace.package]` version and the commit it was built from, so the engine
//!    reports that version; a module that holds no record, or more than one, is refused;
//! 4. the shipping `wasm-opt` pass over the stamped module ([`World::optimise`]): a
//!    module it left unchanged is refused, so nothing ships pre-opt, and so is one whose
//!    record text did not survive the pass;
//! 5. refused when a remapped path survives in the module: a panic location must never
//!    show a visitor the builder's home;
//! 6. installed at `aterm_wasm.<sha12>.js` and `.wasm`, where `sha12` hashes the
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

/// What an engine is built from: the clone of the public commit, and that commit.
#[derive(Clone, Copy, Debug)]
pub struct Source<'a> {
    /// The clone's root.
    pub clone: &'a Path,
    /// The commit it is a clone of, in full.
    pub commit: &'a str,
}

/// Build the engine from `source` on `pin` into `term`, binding, stamping and optimising in
/// `work` (a scratch directory of the sync's): the pair's hash.
///
/// # Errors
/// A build or `wasm-opt` that did not finish, a module the glue generator refuses, a clone
/// whose `Cargo.toml` names no `[workspace.package]` version, a module that holds no identity
/// record or more than one, or whose stamped record `wasm-opt` did not keep, a `wasm-opt` that
/// changed nothing, a builder's path left in the module, a glue that
/// never names its module: all before `term` is written. Asset or pointer publication
/// can fail with the old pointer retained; new unreferenced assets may remain. A
/// retirement error occurs after the new pointer and complete assets are installed.
pub fn build(
    world: &mut dyn World,
    source: Source<'_>,
    pin: &str,
    home: Option<&Path>,
    work: &Path,
    term: &Path,
    out: &mut dyn Write,
) -> Result<String, String> {
    let home = usable_home(home);
    let clone = source.clone;
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
    let mut bound = take(format!("{STEM}_bg.wasm"))?;
    let stamp = source_stamp(source)?;
    aterm_stamp::rewrite(&mut bound, &stamp)
        .map_err(|e| format!("cannot stamp {STEM}.wasm as {}: {e}", stamp.build_id()))?;
    let record = aterm_stamp::encode(&stamp)?;
    let record_text = record.split(|&b| b == 0).next().unwrap_or_default();
    say(
        out,
        &format!(
            "engine: {STEM}.wasm bound by verify glue's generator (web), stamped {}; wasm-opt {}",
            stamp.build_id(),
            WASM_OPT_FLAGS.join(" ")
        ),
    );
    std::fs::create_dir_all(work).map_err(|e| format!("cannot create {}: {e}", work.display()))?;
    let at = work.join(format!("{STEM}_bg.wasm"));
    crate::durable::write_fresh(&at, &bound)
        .map_err(|e| format!("cannot write {}: {e}", at.display()))?;
    world.optimise(&at, out)?;
    let wasm =
        crate::durable::read_file(&at).map_err(|e| format!("cannot read {}: {e}", at.display()))?;
    if wasm == bound {
        return Err(format!(
            "wasm-opt left {} byte for byte as it was — the engine never ships pre-opt",
            at.display()
        ));
    }
    if !wasm.windows(record_text.len()).any(|w| w == record_text) {
        return Err(format!(
            "wasm-opt did not keep {}'s identity record ({}), so the engine would report \
             another version",
            at.display(),
            stamp.build_id()
        ));
    }
    if let Some(path) = leaked(&wasm, home.into_iter().chain([resolved.as_path(), clone])) {
        return Err(format!(
            "refusing: {} survived the path remapping in the module — a visitor's panic \
             message would show it",
            path.display()
        ));
    }
    let hash = aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&wasm)[..6]);
    let js = rename_module(&js, &format!("{STEM}.{hash}.wasm"))?;
    install(term, &hash, &js, &wasm)?;
    Ok(hash)
}

/// The identity the engine built from `source` reports: its clone's `[workspace.package]`
/// version and its commit, clean (the clone is the commit's own tree).
///
/// # Errors
/// The clone's `Cargo.toml` cannot be read or names no `[workspace.package]` version.
fn source_stamp(source: Source<'_>) -> Result<aterm_stamp::Stamp, String> {
    let Source { clone, commit } = source;
    let manifest = clone.join("Cargo.toml");
    let toml = crate::durable::read_file(&manifest)
        .map_err(|e| format!("cannot read {}: {e}", manifest.display()))?;
    let version = crate::publish::workspace_version(&String::from_utf8_lossy(&toml))
        .map_err(|e| format!("{}: {e}", manifest.display()))?;
    // One record per engine build: its strings live as long as the sync.
    let text = |s: &str| -> &'static str { Box::leak(s.to_owned().into_boxed_str()) };
    Ok(aterm_stamp::Stamp {
        app_version: text(&version),
        git_commit: text(commit.get(..12).unwrap_or(commit)),
        git_commit_full: text(commit),
        ..aterm_stamp::Stamp::UNSTAMPED
    })
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
    let hashed = name
        .strip_prefix(STEM)
        .and_then(|rest| rest.strip_prefix('.'))
        .is_some_and(|rest| {
            rest.strip_suffix(".js")
                .or_else(|| rest.strip_suffix(".wasm"))
                .is_some_and(valid_hash)
        });
    !keep.contains(&name)
        && (hashed
            || name
                .strip_prefix(STEM)
                .is_some_and(|s| matches!(s, ".js" | "_bg.wasm")))
}

fn valid_hash(hash: &str) -> bool {
    aterm_codec::hex::is_lower(hash, 12)
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
            options
                .mode(0o644)
                .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS);
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
        let file_name = entry.file_name();
        if let Some(name) = file_name.to_str()
            && replaced(name, [&js_name, &wasm_name])
        {
            let path = entry.path();
            std::fs::remove_file(&path)
                .map_err(|e| format!("cannot remove {}: {e}", path.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
