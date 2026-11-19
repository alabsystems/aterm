// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Content identity of the universal release's Rust and Apple toolchains and SDK, sealed once per
//! release run.
//!
//! The identity [`current_with_sdk`] returns is `atpkg::input_seal`'s identity of the release's input
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

use std::path::{Path, PathBuf};

use crate::trust_lock::CompilerInputs;

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

/// The system-selected SDK used both as a seal root and as compiler input DATA.
/// Never recover this selection from a caller's `SDKROOT` override.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AppleSdk {
    #[cfg(target_os = "macos")]
    root: PathBuf,
}

impl AppleSdk {
    pub(crate) fn capture() -> Result<Self, String> {
        #[cfg(target_os = "macos")]
        {
            Ok(Self {
                root: Self::selected()?,
            })
        }
        #[cfg(not(target_os = "macos"))]
        Ok(Self {})
    }

    #[cfg(target_os = "macos")]
    fn selected() -> Result<PathBuf, String> {
        let root = input_seal::macos_sdk()?
            .canonicalize()
            .map_err(|error| format!("canonical macOS SDK: {error}"))?;
        if !root.is_dir() {
            return Err("the selected macOS SDK is not a directory".into());
        }
        Ok(root)
    }

    /// Refuse a moved system selection instead of relabelling a command's captured inputs.
    pub(crate) fn verify(&self) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        self.verify_selected(Self::selected()?)?;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn verify_selected(&self, selected: PathBuf) -> Result<(), String> {
        if selected != self.root {
            return Err("the system macOS SDK selection changed after capture".into());
        }
        Ok(())
    }

    /// Apply after environment scrubbing, including to the command mirror used as evidence.
    pub(crate) fn apply(&self, command: &mut aterm_uds::spawnfd::Command) {
        if let Some((name, value)) = self.environment() {
            command.env(name, value);
        }
    }

    pub(crate) fn environment(&self) -> Option<(&'static str, &std::ffi::OsStr)> {
        #[cfg(target_os = "macos")]
        return Some(("SDKROOT", self.root.as_os_str()));
        #[cfg(not(target_os = "macos"))]
        None
    }
}

/// The build-input identity under already validated and leased candidate compiler roles.
/// Resolves neither role from this process's ambient toolchain selection.
pub(crate) fn current_for(compiler: &CompilerInputs) -> Result<String, String> {
    let _timing = crate::cut_timing::Timer::new("candidate inputs seal recheck");
    #[cfg(target_os = "macos")]
    {
        on_this_machine(Some(compiler), &AppleSdk::capture()?, false)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = compiler;
        Err("universal release build input identity requires the macOS SDK; native Linux handoffs use their own provenance protocol".into())
    }
}

/// The identity under the SDK captured before constructing the compiler command.
pub(crate) fn current_with_sdk(sdk: &AppleSdk) -> Result<String, String> {
    sdk.verify()?;
    #[cfg(target_os = "macos")]
    {
        on_this_machine(None, sdk, false)
    }
    #[cfg(not(target_os = "macos"))]
    Err("universal release build input identity requires the macOS SDK; native Linux handoffs use their own provenance protocol".into())
}

/// Exact candidate roles and the same SDK input DATA that its command receives.
pub(crate) fn current_for_with_sdk(
    compiler: &CompilerInputs,
    sdk: &AppleSdk,
) -> Result<String, String> {
    sdk.verify()?;
    #[cfg(target_os = "macos")]
    {
        on_this_machine(Some(compiler), sdk, false)
    }
    #[cfg(not(target_os = "macos"))]
    current_for(compiler)
}

/// A native instrument's Trust and SDK identity. It deliberately observes no stock compiler,
/// foreign standard library or stock sysroot, and uses a separate seal domain from a product.
pub(crate) fn current_native_with_sdk(
    compiler: Option<&CompilerInputs>,
    sdk: &AppleSdk,
) -> Result<String, String> {
    sdk.verify()?;
    #[cfg(target_os = "macos")]
    {
        static NATIVE: std::sync::Mutex<input_seal::Process> =
            std::sync::Mutex::new(input_seal::Process::unstarted(input_seal::Schema {
                walk: b"aterm-release-native-inputs-v1",
                seal: "aterm-release-native-inputs-seal-v1",
            }));
        let mut roots = || {
            let trust = match compiler {
                Some(compiler) => {
                    if !compiler.bin.is_absolute()
                        || compiler.bin.canonicalize().map_err(|e| e.to_string())? != compiler.bin
                    {
                        return Err(
                            "native compiler inputs require the canonical absolute Trust bin"
                                .into(),
                        );
                    }
                    compiler.bin.clone()
                }
                None => crate::gates::trust_stage2_bin().map_err(|e| e.to_string())?,
            };
            let sysroot = input_seal::trust_sysroot(&trust)?;
            let host = input_seal::trust_host(&trust)?;
            let mut roots = input_seal::trust_roots(trust, &sysroot, &host)?;
            roots.extend(input_seal::apple_roots(sdk.root.clone())?);
            Ok(roots)
        };
        NATIVE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .identity(
                &mut input_seal::Sources {
                    roots: &mut roots,
                    facts: &mut |roots| facts_under(compiler, roots),
                    open: &mut crate::durable::open_regular_file,
                    now: &mut input_seal::now_ns,
                    stopped: &|| crate::interrupt::caught().is_some(),
                    report: &mut |said| report(None, said),
                },
                false,
            )
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = compiler;
        Err("native SDK input identity requires macOS".into())
    }
}

/// Resolve and hold the exact source's compiler roles before sealing their actual inputs.
/// Candidate release callers already holding a context use `current_for` directly.
pub fn for_commit(git: &dyn crate::ledger::GitRunner, commit: &str) -> Result<String, String> {
    current_for(&crate::trust_lock::inputs_at(git, commit)?)
}

/// Observe exact-source inputs without installing an unavailable compiler.
pub(crate) fn available_for_commit(
    git: &dyn crate::ledger::GitRunner,
    commit: &str,
) -> Result<String, String> {
    current_for(&crate::trust_lock::available_inputs_at(git, commit)?)
}

/// Whether the seal this process holds is `identity` and its roots, facts and every entry's
/// status are still the ones it hashed. Unlike [`current_with_sdk`] it opens no input file to hash it,
/// takes, adopts and keeps no seal, and finishes its walk after an interrupt was caught; it still
/// resolves the roots (running the compilers and `xcrun`) and reads the facts, the SDK's settings
/// files among them, as `current`'s recheck does. A cut told to stop asks this once its verifier
/// has ended, so the rows that verifier finished are recorded for the tree and seal they ran
/// under instead of being dropped with the interrupted walk. `false` is no verdict on the inputs:
/// it also answers a process that holds no seal, roots that cannot be resolved, and every host
/// but macOS.
#[must_use]
pub(crate) fn held(sdk: &AppleSdk, identity: &str) -> bool {
    held_under(None, sdk, identity)
}

/// Whether the held seal still stands under these exact candidate compiler roles.
#[must_use]
pub(crate) fn held_for(compiler: &CompilerInputs, sdk: &AppleSdk, identity: &str) -> bool {
    held_under(Some(compiler), sdk, identity)
}

fn held_under(compiler: Option<&CompilerInputs>, sdk: &AppleSdk, identity: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        if sdk.verify().is_err() {
            return false;
        }
        let mut roots = || roots_under(compiler, sdk);
        let mut facts = |roots: &[input_seal::Root]| facts_under(compiler, roots);
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
        let _ = (compiler, sdk, identity);
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

/// The kept seal for these candidate roles. Its receiver must still recheck every input.
#[must_use]
pub(crate) fn prefrozen_for(
    compiler: &CompilerInputs,
    state_dir: &std::path::Path,
    expected: &str,
) -> Option<PathBuf> {
    prefrozen_for_with_sdk(compiler, &AppleSdk::capture().ok()?, state_dir, expected)
}

/// The kept seal prepared under `state_dir` for these captured SDK roots. Its receiver
/// still rechecks every root, fact and status before adopting it.
#[must_use]
pub(crate) fn prefrozen_with_sdk(
    sdk: &AppleSdk,
    state_dir: &Path,
    expected: &str,
) -> Option<PathBuf> {
    prefrozen_under(None, sdk, state_dir, expected)
}

pub(crate) fn prefrozen_for_with_sdk(
    compiler: &CompilerInputs,
    sdk: &AppleSdk,
    state_dir: &Path,
    expected: &str,
) -> Option<PathBuf> {
    prefrozen_under(Some(compiler), sdk, state_dir, expected)
}

fn prefrozen_under(
    compiler: Option<&CompilerInputs>,
    sdk: &AppleSdk,
    state_dir: &std::path::Path,
    expected: &str,
) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        sdk.verify().ok()?;
        let roots = roots_under(compiler, sdk).ok()?;
        input_seal::matching_kept(SCHEMAS, &state_dir.join("inputs-seal"), expected, &roots)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (compiler, sdk, state_dir, expected);
        None
    }
}

/// `inputs [--recheck]`, a measurement: arm the release state's seal, then take a new one, or,
/// with `recheck`, adopt or recheck the kept one (hashing only on a difference); print the
/// identity on stdout.
/// # Errors
/// What [`current_with_sdk`] refuses, and no HOME.
pub fn measure(recheck: bool) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .ok_or("HOME is not set")?;
        arm(&crate::release_steps::release_state(Path::new(&home)), None);
        println!(
            "{}",
            on_this_machine(None, &AppleSdk::capture()?, !recheck)?
        );
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = recheck;
        current_with_sdk(&AppleSdk::capture()?).map(drop)
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
fn on_this_machine(
    compiler: Option<&CompilerInputs>,
    sdk: &AppleSdk,
    fresh: bool,
) -> Result<String, String> {
    let mut roots = || roots_under(compiler, sdk);
    let mut facts = |roots: &[input_seal::Root]| facts_under(compiler, roots);
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

#[cfg(target_os = "macos")]
fn roots_under(
    compiler: Option<&CompilerInputs>,
    sdk: &AppleSdk,
) -> Result<Vec<input_seal::Root>, String> {
    match compiler {
        Some(compiler) => candidate_roots_with(compiler, |trust, stock| {
            let captured = compiler.stock()?;
            if captured.toolchain() != stock {
                return Err("the captured stock role differs from the source compiler pin".into());
            }
            machine_roots_from(trust.to_path_buf(), captured, sdk)
        }),
        None => machine_roots(sdk),
    }
}

#[cfg(target_os = "macos")]
fn facts_under(compiler: Option<&CompilerInputs>, roots: &[input_seal::Root]) -> input_seal::Facts {
    let facts = input_seal::machine_facts(roots);
    match compiler {
        Some(compiler) => candidate_facts(compiler, facts),
        None => facts,
    }
}

/// A domain-separated, length-framed identity of the complete source declaration. This is
/// a seal fact, not a replacement for the caller's validation and retained compiler lease.
#[cfg(any(test, target_os = "macos"))]
fn compiler_role(compiler: &CompilerInputs) -> String {
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm-release-candidate-compiler-inputs/1\0");
    hash.update(compiler.pin.build.to_be_bytes());
    for field in [
        &compiler.pin.tree_root,
        &compiler.pin.compiler_commit,
        &compiler.pin.receipt,
        &compiler.pin.stock_toolchain,
    ] {
        hash.update((field.len() as u64).to_be_bytes());
        hash.update(field.as_bytes());
    }
    aterm_codec::hex::encode_lower(&hash.finalize())
}

#[cfg(any(test, target_os = "macos"))]
fn candidate_facts(compiler: &CompilerInputs, mut facts: input_seal::Facts) -> input_seal::Facts {
    let observed = match &facts.trust_build {
        Some(value) => format!("some {}:{value}", value.len()),
        None => "none".into(),
    };
    facts.trust_build = Some(format!(
        "candidate/1 {} {observed}",
        compiler_role(compiler)
    ));
    facts
}

#[cfg(any(test, target_os = "macos"))]
fn candidate_roots_with(
    compiler: &CompilerInputs,
    resolve: impl FnOnce(&Path, &str) -> Result<Vec<input_seal::Root>, String>,
) -> Result<Vec<input_seal::Root>, String> {
    if !compiler.bin.is_absolute()
        || compiler
            .bin
            .canonicalize()
            .map_err(|error| error.to_string())?
            != compiler.bin
    {
        return Err("candidate compiler inputs require the canonical absolute Trust bin".into());
    }
    let mut roots = resolve(&compiler.bin, &compiler.pin.stock_toolchain)?;
    let stock = roots
        .iter_mut()
        .find(|(label, _)| label == "stock/bin")
        .ok_or("candidate compiler inputs have no stock bin root")?;
    // Kept filenames are keyed by roots. Bind the declaration there as well as in facts,
    // so a pre-freeze lookup cannot offer another pin's file even with identical inputs.
    stock.0 = format!("candidate/{}/stock/bin", compiler_role(compiler));
    Ok(roots)
}

/// The resolved input roots of this machine's universal build, labelled: the selected Trust
/// build's (`atpkg::input_seal::trust_roots`), the checkout's declared stock proxies, `bin` and the
/// libraries of its host and x86_64 targets, and the Apple roots
/// (`atpkg::input_seal::apple_roots`). They resolve in the release's order: the Trust sysroot,
/// HOME, the rustup shim, RUSTUP_HOME and PATH, the declared stock sysroot, the Trust and then the
/// stock host, the SDK, the Trust and then the stock libraries, the Apple tools. So where
/// more than one fails, the first error is the one it was before the protocol moved to atpkg,
/// and a Trust that cannot name its sysroot fails before the declared stock proxy runs. Where
/// that stock release, or a std the seal hashes, is not in place, the failure names what puts it
/// there ([`stock_gap`]).
#[cfg(target_os = "macos")]
fn machine_roots(sdk: &AppleSdk) -> Result<Vec<input_seal::Root>, String> {
    let trust = crate::gates::trust_stage2_bin().map_err(|e| e.to_string())?;
    let home = std::env::var_os("HOME").ok_or("compiler identity requires HOME")?;
    let stock =
        crate::buildplan::Stock::under(&home, &crate::trust_lock::checkout_stock_toolchain()?)?;
    machine_roots_from(trust, &stock, sdk)
}

#[cfg(target_os = "macos")]
fn machine_roots_from(
    trust: PathBuf,
    stock: &crate::buildplan::Stock,
    sdk: &AppleSdk,
) -> Result<Vec<input_seal::Root>, String> {
    use aterm_uds::spawnfd::command;

    let trust_sysroot = input_seal::trust_sysroot(&trust)?;
    let stock_toolchain = stock.toolchain();
    let shim = stock.shim()?.to_path_buf();
    let stock_root = input_seal::output_path(
        stock.command("rustc")?.args(["--print", "sysroot"]),
        &format!("{stock_toolchain} sysroot"),
    )
    .map_err(|why| stock_gap(stock_toolchain, &why))?;
    let trust_host = input_seal::trust_host(&trust)?;
    let stock_host = input_seal::output_host(
        command(stock_root.join("bin/rustc")).arg("-vV"),
        &format!("{stock_toolchain} host"),
    )?;
    let mut roots = input_seal::trust_roots(trust, &trust_sysroot, &trust_host)?;
    roots.extend([
        ("stock/proxies".into(), shim),
        ("stock/bin".into(), stock_root.join("bin")),
    ]);
    roots.extend(stock_library_roots(
        stock_toolchain,
        &stock_root,
        &stock_host,
    )?);
    roots.extend(input_seal::apple_roots(sdk.root.clone())?);
    Ok(roots)
}

/// The libraries of the stock release `release`, whose sysroot is `stock_root`, for its host
/// `host` and the x86_64 slice (`atpkg::input_seal::rust_library_roots`): where either is
/// missing, as `rustup toolchain install` alone leaves the x86_64 one on an arm64 Mac, the
/// failure is a [`stock_gap`].
#[cfg(any(test, target_os = "macos"))]
fn stock_library_roots(
    release: &str,
    stock_root: &Path,
    host: &str,
) -> Result<Vec<input_seal::Root>, String> {
    input_seal::rust_library_roots("stock", stock_root, &[host, "x86_64-apple-darwin"])
        .map_err(|why| stock_gap(release, &why))
}

/// What separates a build-inputs seal failure from the remedy [`stock_gap`] gives it.
const STOCK_GAP: &str = " — the stock release the seal hashes is not in place with its stds: ";

/// The seal failure `why` where `release`, the stock release the seal hashes (for its generic
/// roots the one the trust.lock of the checkout this process runs in names, for a candidate's
/// the one its source's pin names), is not in place with the stds the seal hashes (rustup does
/// not resolve its sysroot, or its host's or the x86_64 slice's library is missing), with what
/// puts it there ([`crate::buildplan::stock_release_remedy`]: `verify provision rustup` here
/// where this checkout's lock names `release`, else in a checkout whose lock does, and the
/// release's own install line), which [`stock_gap_remedy`] reads back. Every reader of the seal
/// says it so (a freeze of R's cutter, step 1's seal, `ship inputs`, a pre-freeze), never
/// rustup's own install hint alone, which installs no x86_64 std.
#[cfg(any(test, target_os = "macos"))]
pub(crate) fn stock_gap(release: &str, why: &str) -> String {
    let checkout = crate::trust_lock::checkout_stock_toolchain().ok();
    format!(
        "{why}{STOCK_GAP}{}",
        crate::buildplan::stock_release_remedy(release, checkout.as_deref())
    )
}

/// The remedy a seal failure `why` carries where it is a [`stock_gap`]; `None` for any other.
pub(crate) fn stock_gap_remedy(why: &str) -> Option<&str> {
    why.split_once(STOCK_GAP).map(|(_, remedy)| remedy)
}

/// The content digest of labeled `roots` under [`SCHEMA`] (`atpkg::input_seal::content_digest`).
#[cfg(test)]
fn digest_roots(roots: &[(&str, &Path)]) -> Result<String, String> {
    input_seal::content_digest(SCHEMA, roots)
}

#[cfg(test)]
#[path = "build_inputs_tests.rs"]
mod tests;
