// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Content identity of the universal release's Rust and Apple toolchains and SDK, sealed once per
//! release run.
//!
//! The identity [`current`] returns is `atpkg::input_seal`'s identity of the release's input
//! roots (`machine_roots`) under the release's schemas ([`SCHEMA`], [`SEAL_SCHEMA`]): a SHA-256
//! over both schemas, the content digest of every input root (each file read in full) and this
//! machine's facts (its OS build, its model, the SDK's settings and the Trust build). Reading every
//! input's bytes takes seconds, and one release asks for the identity about two dozen times across
//! its processes. So a process's first call takes a seal (the content digest and, from the same
//! walk, a status digest of each root, [`aterm_toolchain::stat_digest`]) and every later call
//! rechecks it by status alone, reading no input file, and hashes again only when the roots, the
//! facts or a status differ. A take whose inputs changed too recently for their status to tell a
//! later write ([`aterm_toolchain::stat_digest::SETTLE_NS`]) is not held, so the next call hashes
//! again. A release arms persistence at its first step ([`arm`]): its seal is kept in the release
//! state, and every child it starts is handed the kept seal's path ([`SEAL_ENV`]), which the child
//! adopts once the same recheck holds.

#[cfg(any(test, target_os = "macos"))]
use std::path::Path;
use std::path::PathBuf;

#[cfg(any(test, target_os = "macos"))]
use atpkg::input_seal::{self, Report};

/// The content walk's schema, the first field of its digest and part of the identity. Any change
/// to what `atpkg::input_seal`'s walk reads into the content digest, or how it encodes it, ships
/// with a new schema: a seal kept by a cutter of the old walk otherwise still stands for the new
/// one, as nothing else in a kept seal names the walk's code.
/// `the_content_digest_of_a_fixed_tree_is_schema_v2s` holds the walk to it.
#[cfg(any(test, target_os = "macos"))]
const SCHEMA: &[u8] = b"aterm-release-build-inputs-v2";

/// The variable that hands a child its parent's kept seal: the path of the file. A release sets
/// it for each child it starts once its seal is kept ([`handed_path`]), and never passes on one
/// it inherited.
pub const SEAL_ENV: &str = "ATERM_RELEASE_INPUTS_SEAL";

/// The seal's own schema, part of the identity: a seal of another schema is no seal here.
#[cfg(any(test, target_os = "macos"))]
const SEAL_SCHEMA: &str = "aterm-release-inputs-seal-v2";

/// The release's schemas, as the seal binds them.
#[cfg(any(test, target_os = "macos"))]
const SCHEMAS: input_seal::Schema = input_seal::Schema {
    walk: SCHEMA,
    seal: SEAL_SCHEMA,
};

/// This machine's build-input identity: the selected drivers, the Rust and Apple toolchain
/// bin/lib trees and the macOS SDK by content, with the OS build, the model and the SDK's
/// settings. The first call in a process adopts a handed or kept seal whose recheck holds, or
/// takes one; each later call rechecks the seal by status and takes a new one only on a
/// difference. Each call says on stderr, and in the log [`arm`] named, what it did (`inputs:
/// hashed …` or `inputs: rechecked by stat …`, and one `release-event:` line). Linux's native
/// handoff has its own compiler provenance protocol.
/// # Errors
/// A compiler or SDK cannot be resolved or read, an input changed while it was hashed, or this
/// is not a macOS host.
pub fn current() -> Result<String, String> {
    let _timing = crate::cut_timing::Timer::new("inputs seal recheck");
    #[cfg(target_os = "macos")]
    {
        on_this_machine(false)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("universal release build input identity requires the macOS SDK; native Linux handoffs use their own provenance protocol".into())
    }
}

/// Whether the seal this process holds is `identity` and its roots, facts and every entry's
/// status are still the ones it hashed. Unlike [`current`] it opens no input file to hash it,
/// takes, adopts and keeps no seal, and finishes its walk after an interrupt was caught; it still
/// resolves the roots (running the compilers and `xcrun`) and reads the facts, the SDK's settings
/// files among them, as `current`'s recheck does. A cut told to stop asks this once its verifier
/// has ended, so the rows that verifier finished are recorded for the tree and seal they ran
/// under instead of being dropped with the interrupted walk. `false` is no verdict on the inputs:
/// it also answers a process that holds no seal, roots that cannot be resolved, and every host
/// but macOS.
#[must_use]
pub fn held(identity: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        let mut roots = machine_roots;
        let mut facts = input_seal::machine_facts;
        let mut open = crate::durable::open_regular_file;
        let mut now = input_seal::now_ns;
        // `holds` walks to its end whatever this says.
        let stopped = || crate::interrupt::caught().is_some();
        sealed().process.holds(
            &mut input_seal::Sources {
                roots: &mut roots,
                facts: &mut facts,
                open: &mut open,
                now: &mut now,
                stopped: &stopped,
                report: &mut |_| {},
            },
            identity,
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = identity;
        false
    }
}

/// Keep this process's seal under `state_dir` (the release's state) from its next call on, and
/// look for a kept one there: the release's first step arms it, under its toolchain lease. A
/// dry run arms nothing, so it keeps no seal and hands none on. Each later call's report also
/// goes to `log` where one is named: the cutter names its transcript, as its stderr reaches no
/// log, while a child's stderr is its step's log.
pub fn arm(state_dir: &std::path::Path, log: Option<&std::path::Path>) {
    #[cfg(target_os = "macos")]
    {
        let mut held = sealed();
        held.process.arm(state_dir.join("inputs-seal"));
        held.log = log.map(Path::to_path_buf);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (state_dir, log);
}

/// Arm the real release state and offer one authenticated pre-freeze seal to this process's
/// first identity call. The call still rechecks every root, fact and status before adoption;
/// a changed offer is ignored and the current inputs are hashed instead.
pub(crate) fn arm_prefrozen(
    state_dir: &std::path::Path,
    log: Option<&std::path::Path>,
    handed: Option<&std::path::Path>,
) {
    #[cfg(target_os = "macos")]
    {
        let mut held = sealed();
        held.process.arm(state_dir.join("inputs-seal"));
        // An earlier cheap gate may have taken an unpersisted process-local seal. The exact
        // pre-freeze path must exercise the handed record's own recheck, not merely observe that
        // an independently held identity happens to match it.
        held.process.forget();
        held.started = true;
        held.process.begin(handed.map(Path::to_path_buf), None);
        held.log = log.map(Path::to_path_buf);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (state_dir, log, handed);
}

/// The file that holds this process's seal once it is kept, which a child is handed as
/// [`SEAL_ENV`]; `None` before then, when nothing armed or handed persistence, and off macOS.
#[must_use]
pub fn handed_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        sealed().process.saved().map(Path::to_path_buf)
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// The kept seal prepared under `state_dir` when it authenticates `expected` for this
/// machine's resolved input roots. The caller may hand this file to another process; that
/// process still rechecks every root, fact and status before it adopts the seal.
#[must_use]
pub(crate) fn prefrozen_seal(state_dir: &std::path::Path, expected: &str) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let roots = machine_roots().ok()?;
        input_seal::matching_kept(SCHEMAS, &state_dir.join("inputs-seal"), expected, &roots)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (state_dir, expected);
        None
    }
}

/// `inputs [--recheck]`, a measurement: arm the release state's seal, then take a new one, or,
/// with `recheck`, adopt or recheck the kept one (hashing only on a difference); print the
/// identity on stdout.
/// # Errors
/// What [`current`] refuses, and no HOME.
pub fn measure(recheck: bool) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .ok_or("HOME is not set")?;
        arm(&crate::release_steps::release_state(Path::new(&home)), None);
        println!("{}", on_this_machine(!recheck)?);
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = recheck;
        current().map(drop)
    }
}

/// The release's seal in this process: atpkg's protocol, with where its reports also go and
/// whether the handoff in the environment was read.
#[cfg(target_os = "macos")]
struct Held {
    process: input_seal::Process,
    /// The log each call's report also goes to ([`arm`]'s).
    log: Option<PathBuf>,
    /// Whether the environment has been read.
    started: bool,
}

#[cfg(target_os = "macos")]
impl Held {
    /// Read the environment, once: the handed seal ([`SEAL_ENV`]), and the release state's
    /// directory, where another process of the release may keep one.
    fn start(&mut self) {
        if std::mem::replace(&mut self.started, true) {
            return;
        }
        let handed = std::env::var_os(SEAL_ENV)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .filter(|path| path.is_absolute());
        let finds = std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(|home| crate::release_steps::release_state(Path::new(&home)).join("inputs-seal"));
        self.process.begin(handed, finds);
    }
}

#[cfg(target_os = "macos")]
static INPUTS_SEAL: std::sync::Mutex<Held> = std::sync::Mutex::new(Held {
    process: input_seal::Process::unstarted(SCHEMAS),
    log: None,
    started: false,
});

#[cfg(target_os = "macos")]
fn sealed() -> std::sync::MutexGuard<'static, Held> {
    INPUTS_SEAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// This process's identity from this machine's roots and facts; `fresh` takes a new seal
/// whatever is held or kept.
#[cfg(target_os = "macos")]
fn on_this_machine(fresh: bool) -> Result<String, String> {
    let mut roots = machine_roots;
    let mut facts = input_seal::machine_facts;
    let mut open = crate::durable::open_regular_file;
    let mut now = input_seal::now_ns;
    let mut held = sealed();
    held.start();
    let log = held.log.clone();
    let mut report = |said: Report<'_>| report(log.as_deref(), said);
    held.process.identity(
        &mut input_seal::Sources {
            roots: &mut roots,
            facts: &mut facts,
            open: &mut open,
            now: &mut now,
            stopped: &|| crate::interrupt::caught().is_some(),
            report: &mut report,
        },
        fresh,
    )
}

/// Say a call's report `line` on stderr and, where [`arm`] named one, in `log`.
#[cfg(any(test, target_os = "macos"))]
fn say_to(log: Option<&Path>, line: &str) {
    use std::io::Write as _;
    eprintln!("{line}");
    if let Some(log) = log
        && let Ok(mut file) = crate::durable::open_append(log, true)
    {
        let _ = writeln!(file, "{line}");
    }
}

/// Say what a seal call did ([`Report`]) as the release's logs count it ([`say_to`]): a take as
/// `inputs: hashed N entries in T s`, a recheck or an adoption as `inputs: rechecked by stat N
/// entries in T ms`, each followed by one `release-event:` line of its mode, and each note as
/// one `inputs:` line.
#[cfg(any(test, target_os = "macos"))]
fn report(log: Option<&Path>, said: Report<'_>) {
    let (mode, entries, secs, line) = match said {
        Report::Hashed { entries, secs } => (
            "hashed",
            entries,
            secs,
            format!("inputs: hashed {entries} entries in {secs:.1} s"),
        ),
        Report::Rechecked { entries, secs } => (
            "rechecked",
            entries,
            secs,
            format!(
                "inputs: rechecked by stat {entries} entries in {:.0} ms",
                secs * 1000.0
            ),
        ),
        Report::Adopted {
            entries,
            secs,
            from,
        } => (
            "adopted",
            entries,
            secs,
            format!(
                "inputs: rechecked by stat {entries} entries in {:.0} ms (adopted {})",
                secs * 1000.0,
                from.display()
            ),
        ),
        Report::Changed(why) => return say_to(log, &format!("inputs: {why}; hashing them again")),
        Report::Unsettled => {
            return say_to(
                log,
                "inputs: an input's status changed too recently to tell a later write by it, so \
                 this seal is neither held nor kept and the next call hashes again",
            );
        }
        Report::NotKept(why) => {
            return say_to(
                log,
                &format!("inputs: the seal was not kept, so each child hashes for itself: {why}"),
            );
        }
    };
    say_to(log, &line);
    say_to(
        log,
        &format!(
            "release-event: {}",
            aterm_json::json!({
                "schema": 1, "event": "inputs", "mode": mode, "entries": entries, "secs": secs,
            })
        ),
    );
}

/// The resolved input roots of this machine's universal build, labelled: the selected Trust
/// build's (`atpkg::input_seal::trust_roots`), upstream stable's proxies, `bin` and the
/// libraries of its host and x86_64 targets, and the Apple roots
/// (`atpkg::input_seal::apple_roots`). They resolve in the release's order: the Trust sysroot,
/// HOME, the rustup shim, RUSTUP_HOME and PATH, the stable sysroot, the Trust and then the
/// stable host, the SDK, the Trust and then the stable libraries, the Apple tools. So where
/// more than one fails, the first error is the one it was before the protocol moved to atpkg,
/// and a Trust that cannot name its sysroot fails before rustup's `stable` proxy runs.
#[cfg(target_os = "macos")]
fn machine_roots() -> Result<Vec<input_seal::Root>, String> {
    use aterm_uds::spawnfd::command;

    let trust = crate::gates::trust_stage2_bin().map_err(|e| e.to_string())?;
    let trust_sysroot = input_seal::trust_sysroot(&trust)?;
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
    let stable_root = input_seal::output_path(
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
    let trust_host = input_seal::trust_host(&trust)?;
    let stable_host = input_seal::output_host(
        command(stable_root.join("bin/rustc")).arg("-vV"),
        "stable host",
    )?;
    let sdk = input_seal::macos_sdk()?;
    let mut roots = input_seal::trust_roots(trust, &trust_sysroot, &trust_host)?;
    roots.extend([
        ("stable/proxies".to_string(), shim),
        ("stable/bin".to_string(), stable_root.join("bin")),
    ]);
    roots.extend(input_seal::rust_library_roots(
        "stable",
        &stable_root,
        &[&stable_host, "x86_64-apple-darwin"],
    )?);
    roots.extend(input_seal::apple_roots(sdk)?);
    Ok(roots)
}

/// The content digest of labeled `roots` under [`SCHEMA`] (`atpkg::input_seal::content_digest`).
#[cfg(test)]
fn digest_roots(roots: &[(&str, &Path)]) -> Result<String, String> {
    input_seal::content_digest(SCHEMA, roots)
}

#[cfg(test)]
#[path = "build_inputs_tests.rs"]
mod tests;
