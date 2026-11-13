// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DOCTESTS' DOC DRIVER.

use std::ffi::{OsStr, OsString};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The real doc driver, an absolute path.
pub const DRIVER_ENV: &str = "ATERM_VERIFY_DOC_DRIVER";

/// The packages whose doctest pass is skipped, by package name, separated by [`SKIP_SEPARATOR`].
pub const SKIP_ENV: &str = "ATERM_VERIFY_DOC_SKIP";

/// The verifier executable rustdoc runs around each generated doctest binary.
pub const RUNTOOL_ENV: &str = "ATERM_VERIFY_DOCTEST_RUNTOOL";

/// The verifier scratch directory inherited by the generated-binary runtool.
pub const RUNTOOL_SCRATCH_ENV: &str = "ATERM_VERIFY_DOCTEST_SCRATCH";

/// The owner-private directory in which the runtool keeps a hung binary's stack sample.
pub const RUNTOOL_SAMPLES_ENV: &str = "ATERM_VERIFY_DOCTEST_SAMPLES";

/// The run-private first-red admission protocol shared by every rustdoc runtool process. The path
/// names its lock file; owner-private pending/red siblings hold the monotonic decision. Its
/// presence means this verify run stops at its first proven red; halt-off runs inherit no path.
pub const RUNTOOL_HALT_ENV: &str = "ATERM_VERIFY_DOCTEST_FIRST_RED";

/// The first argument rustdoc gives the verifier when it invokes it as a doctest runtool.
pub const RUNTOOL_FLAG: &str = "--aterm-verify-doctest-runtool";

/// Between two package names in [`SKIP_ENV`]: a package name never holds one.
pub const SKIP_SEPARATOR: char = ',';

/// [`SKIP_ENV`]'s value for `packages`.
#[must_use]
pub fn skip_list(packages: &[String]) -> String {
    packages.join(&SKIP_SEPARATOR.to_string())
}

/// Is this invocation a doctest pass of a package in `skip`? Cargo spells a doctest pass with a
/// `--test` argument of its own and names the package in `CARGO_PKG_NAME`; anything else, or
/// anything this cannot read, is no.
#[must_use]
pub fn skips(args: &[OsString], package: Option<&OsStr>, skip: Option<&OsStr>) -> bool {
    let (Some(package), Some(skip)) = (package, skip.and_then(OsStr::to_str)) else {
        return false;
    };
    args.iter().any(|a| a == "--test")
        && skip
            .split(SKIP_SEPARATOR)
            .any(|name| !name.is_empty() && OsStr::new(name) == package)
}

/// What a skipped pass prints in rustdoc's place.
#[must_use]
pub fn skip_note(package: &OsStr) -> String {
    format!(
        "aterm-verify: no doctest in {}'s docs (verify's scan), so rustdoc was not run on it",
        package.to_string_lossy()
    )
}

/// The stand-in: skip the pass ([`skips`]) or become the real driver.
#[must_use]
pub fn main(args: &[OsString]) -> i32 {
    let Some(driver) = std::env::var_os(DRIVER_ENV) else {
        eprintln!("aterm-verify: the doc-driver stand-in needs {DRIVER_ENV}");
        return 2;
    };
    let package = std::env::var_os("CARGO_PKG_NAME");
    if skips(
        args,
        package.as_deref(),
        std::env::var_os(SKIP_ENV).as_deref(),
    ) {
        println!("{}", skip_note(package.as_deref().unwrap_or_default()));
        return 0;
    }
    #[cfg(unix)]
    let mut command = aterm_uds::spawnfd::transparent(&driver).into_command();
    #[cfg(not(unix))]
    let mut command = aterm_uds::spawnfd::transparent(&driver);
    command.args(args).env("RUSTDOC", &driver);
    if args.iter().any(|arg| arg == "--test") {
        let Some(runtool) = std::env::var_os(RUNTOOL_ENV) else {
            eprintln!("aterm-verify: the doc-driver stand-in needs {RUNTOOL_ENV}");
            return 2;
        };
        command
            .arg("--test-runtool")
            .arg(runtool)
            .arg("--test-runtool-arg")
            .arg(RUNTOOL_FLAG);
    }
    command.env_remove(DRIVER_ENV).env_remove(SKIP_ENV);
    let err = become_driver(&mut command);
    eprintln!(
        "aterm-verify: could not run the doc driver {}: {err}",
        driver.to_string_lossy()
    );
    127
}

/// Everything the runtool reads before it starts a generated doctest binary.
#[derive(Clone)]
struct Runtool {
    scratch: PathBuf,
    samples: Option<PathBuf>,
    spec: String,
    halt: Option<PathBuf>,
    hang_after: Duration,
}

impl Runtool {
    fn from_env() -> Result<Self, String> {
        let scratch = std::env::var_os(RUNTOOL_SCRATCH_ENV)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| format!("the doctest runtool needs {RUNTOOL_SCRATCH_ENV}"))?;
        let samples = std::env::var_os(RUNTOOL_SAMPLES_ENV)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let package = std::env::var("CARGO_PKG_NAME")
            .ok()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "unknown-package".to_owned());
        let halt = std::env::var_os(RUNTOOL_HALT_ENV)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        if let Some(path) = &halt
            && (!path.is_absolute() || path.parent() != Some(scratch.as_path()))
        {
            return Err(format!(
                "the doctest runtool's {RUNTOOL_HALT_ENV} must be an absolute file directly in \
                 {RUNTOOL_SCRATCH_ENV}"
            ));
        }
        Ok(Self {
            scratch,
            samples,
            spec: format!("-p {package} --doc"),
            halt,
            hang_after: crate::exec::TEST_BINARY_HANG,
        })
    }
}

const HALT_ADMISSION: u8 = b'A';
const HALT_SENTINEL: u8 = b'R';
/// An admission lock is held only across one process spawn. A minute is its hang detector, never
/// an expected latency budget.
const HALT_LOCK_HANG: Duration = Duration::from_secs(60);

/// This run's unique first-red admission file. [`crate::Ctx::scratch`] is itself made afresh for
/// one verify run, so the fixed leaf cannot alias another run or an older marker.
#[must_use]
pub(crate) fn halt_marker(scratch: &Path) -> PathBuf {
    scratch.join("doctest-first-red")
}

fn halt_pending(path: &Path) -> PathBuf {
    path.with_extension("pending")
}

fn halt_red(path: &Path) -> PathBuf {
    path.with_extension("red")
}

fn create_halt_file(path: &Path, state: u8) -> Result<(), String> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let mut file = options
        .open(path)
        .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
    file.write_all(&[state])
        .map_err(|error| format!("cannot initialize {}: {error}", path.display()))
}

/// Create the owner-only admission lock and its preallocated same-directory pending sentinel
/// before Cargo can start any rustdoc process. RED is later published by renaming the sentinel.
pub(crate) fn prepare_halt_marker(path: &Path) -> Result<(), String> {
    let pending = halt_pending(path);
    create_halt_file(path, HALT_ADMISSION)?;
    if let Err(error) = create_halt_file(&pending, HALT_SENTINEL) {
        let _ = std::fs::remove_file(path);
        return Err(error);
    }
    Ok(())
}

/// Remove every member of one first-red protocol after Cargo has reaped all runtools.
pub(crate) fn remove_halt_marker(path: &Path) -> Result<(), String> {
    let mut errors = Vec::new();
    for member in [path.to_path_buf(), halt_pending(path), halt_red(path)] {
        if let Err(error) = std::fs::remove_file(&member)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            errors.push(format!("{}: {error}", member.display()));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn open_halt_file(path: &Path, state: u8) -> Result<Option<std::fs::File>, String> {
    open_halt_file_with(path, state, false)
}

fn open_halt_file_with(
    path: &Path,
    state: u8,
    write: bool,
) -> Result<Option<std::fs::File>, String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(write);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "cannot open first-red protocol file {}: {error}",
                path.display()
            ));
        }
    };
    let metadata = file.metadata().map_err(|error| {
        format!(
            "cannot inspect first-red protocol file {}: {error}",
            path.display()
        )
    })?;
    if !metadata.is_file() || metadata.len() != 1 {
        return Err(format!(
            "first-red protocol file {} is not its one-byte regular file",
            path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        // SAFETY: `geteuid` reads the calling process's credential and has no precondition.
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(format!(
                "first-red protocol file {} is not owner-private",
                path.display()
            ));
        }
    }
    let mut actual = [0_u8; 1];
    file.read_exact(&mut actual).map_err(|error| {
        format!(
            "cannot read first-red protocol file {}: {error}",
            path.display()
        )
    })?;
    if actual[0] != state {
        return Err(format!(
            "first-red protocol file {} has invalid state byte {}",
            path.display(),
            actual[0]
        ));
    }
    Ok(Some(file))
}

enum HaltState {
    Open,
    Red,
}

fn halt_state(path: &Path) -> Result<HaltState, String> {
    let red = halt_red(path);
    if open_halt_file(&red, HALT_SENTINEL)?.is_some() {
        return Ok(HaltState::Red);
    }
    let pending = halt_pending(path);
    if open_halt_file(&pending, HALT_SENTINEL)?.is_some() {
        Ok(HaltState::Open)
    } else {
        Err(format!(
            "first-red protocol has neither a valid pending nor red sentinel beside {}",
            path.display()
        ))
    }
}

/// Lock and prove that no earlier runtool has decided a red. The returned descriptor stays
/// locked through the generated binary's actual spawn; [`crate::exec::run_test_admitted`] then
/// explicitly unlocks it. Any missing, malformed or unreadable state fails closed.
fn admit_halt_marker(path: &Path) -> Result<std::fs::File, String> {
    admit_halt_marker_with(path, || {})
}

/// [`admit_halt_marker`], exposing evidence of an actual contended lock to the concurrency
/// control. Production supplies an inert observer.
fn admit_halt_marker_with(path: &Path, held: impl FnMut()) -> Result<std::fs::File, String> {
    let file = open_halt_file(path, HALT_ADMISSION)?
        .ok_or_else(|| format!("first-red admission {} is missing", path.display()))?;
    aterm_uds::file_lock::lock_exclusive_until_with(
        &file,
        std::time::Instant::now() + HALT_LOCK_HANG,
        "another doctest runtool held first-red admission for one minute",
        held,
    )
    .map_err(|error| {
        format!(
            "cannot lock first-red admission {}: {error}",
            path.display()
        )
    })?;
    let state = match halt_state(path) {
        Ok(state) => state,
        Err(error) => {
            let _ = file.unlock();
            return Err(error);
        }
    };
    match state {
        HaltState::Open => Ok(file),
        HaltState::Red => {
            let _ = file.unlock();
            Err("stopped before starting: another doctest binary reached the first red".to_owned())
        }
    }
}

/// Monotonically close every later start without waiting for or writing through an admission
/// lock. The pending sentinel was precreated in the same directory, so its rename publishes RED
/// atomically without allocating a new file. A concurrent signal treats a valid RED as success.
fn signal_halt_marker(path: &Path) -> Result<(), String> {
    signal_halt_marker_with(path, |from, to| std::fs::rename(from, to))
}

fn signal_halt_marker_with(
    path: &Path,
    rename: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> Result<(), String> {
    signal_halt_marker_after(path, || {}, rename)
}

fn signal_halt_marker_after(
    path: &Path,
    before_pending_open: impl FnOnce(),
    rename: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> Result<(), String> {
    if matches!(halt_state(path), Ok(HaltState::Red)) {
        return Ok(());
    }
    before_pending_open();
    let pending = halt_pending(path);
    let red = halt_red(path);
    match open_halt_file(&pending, HALT_SENTINEL) {
        Ok(Some(_)) => {}
        Ok(None) if matches!(halt_state(path), Ok(HaltState::Red)) => return Ok(()),
        Ok(None) => {
            return Err(format!(
                "cannot publish first-red sentinel: {} is missing",
                pending.display()
            ));
        }
        Err(_) if matches!(halt_state(path), Ok(HaltState::Red)) => return Ok(()),
        Err(error) => return Err(error),
    }
    // Opening for poison is only a fallback aid. A read-only sentinel can still be renamed by
    // its owner through the writable private directory, so its open must never prevent publish.
    let pending_file = open_halt_file_with(&pending, HALT_SENTINEL, true).and_then(|file| {
        file.ok_or_else(|| {
            format!(
                "cannot open pending first-red sentinel {} for fail-close: it disappeared",
                pending.display()
            )
        })
    });
    match rename(&pending, &red) {
        Ok(()) => Ok(()),
        Err(_) if matches!(halt_state(path), Ok(HaltState::Red)) => Ok(()),
        Err(rename_error) => {
            let publish = format!(
                "cannot publish first-red sentinel {} -> {}: {rename_error}",
                pending.display(),
                red.display()
            );
            match pending_file.and_then(|file| {
                file.set_len(0)
                    .map(|()| file)
                    .map_err(|error| format!("cannot poison {}: {error}", pending.display()))
            }) {
                Ok(_pending_file) => Err(format!(
                    "{publish}; poisoned the pending sentinel so the protocol remains closed"
                )),
                Err(poison_error) => match std::fs::remove_file(&pending) {
                    Ok(()) => Err(format!(
                        "{publish}; {poison_error}; removed the pending sentinel so the protocol \
                         remains closed"
                    )),
                    Err(remove_error) if remove_error.kind() == std::io::ErrorKind::NotFound => {
                        Err(format!(
                            "{publish}; {poison_error}; the pending sentinel disappeared, so the \
                             protocol remains closed"
                        ))
                    }
                    Err(remove_error) => Err(format!(
                        "{publish}; could not fail-close the pending sentinel: {poison_error}; \
                         remove failed: {remove_error}"
                    )),
                },
            }
        }
    }
}

/// Why an admitted live peer must end now. A missing, unreadable or malformed protocol fails
/// closed; valid pending is the only state in which the peer may continue.
fn external_halt(path: &Path) -> Option<String> {
    match halt_state(path) {
        Ok(HaltState::Open) => None,
        Ok(HaltState::Red) => {
            Some("another doctest binary reached the first red; this peer was stopped".to_owned())
        }
        Err(error) => Some(format!("{error}; this peer was stopped")),
    }
}

/// Run one rustdoc-generated test executable under the same output/descendant-CPU progress
/// detector as Cargo's recorded test binaries. Rustdoc puts the executable first, followed by
/// its libtest arguments. The JSON flags make every running doctest name available before a
/// sample, and [`crate::libtest_json::render`] restores the ordinary human transcript for Cargo.
fn runtool_on(
    args: &[OsString],
    settings: &Runtool,
    host: &dyn crate::exec::Host,
) -> Result<crate::exec::Run, String> {
    runtool_on_with_admission(args, settings, host, admit_halt_marker)
}

fn runtool_on_with_admission(
    args: &[OsString],
    settings: &Runtool,
    host: &dyn crate::exec::Host,
    admit: impl FnOnce(&Path) -> Result<std::fs::File, String>,
) -> Result<crate::exec::Run, String> {
    let (binary, test_args) = args
        .split_first()
        .ok_or_else(|| "the doctest runtool was given no test executable".to_owned())?;
    let cwd = std::env::current_dir().map_err(|error| {
        format!("the doctest runtool cannot read its working directory: {error}")
    })?;
    let path = std::env::var_os("PATH").unwrap_or_default();
    let remove_env = [
        DRIVER_ENV,
        SKIP_ENV,
        RUNTOOL_ENV,
        RUNTOOL_SCRATCH_ENV,
        RUNTOOL_SAMPLES_ENV,
        RUNTOOL_HALT_ENV,
    ];
    let mut command = crate::exec::Cmd::new(binary).args(test_args.iter().cloned());
    command.args.extend(
        ["-Z", "unstable-options", "--format", "json"]
            .into_iter()
            .map(OsString::from),
    );
    command.test_cases = Some(crate::exec::CaseWatch {
        spec: settings.spec.clone(),
        binary: Path::new(binary).display().to_string(),
        json: true,
        stop_at_red: settings.halt.is_some(),
        hang_after: settings.hang_after,
    });
    let env = crate::exec::ExecEnv {
        events: None,
        cwd: &cwd,
        path: &path,
        scratch: &settings.scratch,
        child_ceiling: None,
        remove_env: &remove_env,
        add_env: &[],
        stage_groups: None,
        host,
        hang_samples: settings.samples.as_deref(),
    };
    let admission = match settings.halt.as_deref() {
        Some(path) => Some(admit(path)?),
        None => None,
    };
    let signal_error = std::sync::Mutex::new(None);
    let close_admission = || {
        if let Some(path) = settings.halt.as_deref()
            && let Err(error) = signal_halt_marker(path)
        {
            eprintln!("aterm-verify: first-red publication failed: {error}");
            let mut first_error = signal_error
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if first_error.is_none() {
                *first_error = Some(error);
            }
        }
    };
    let external_stop = || {
        admission
            .as_ref()
            .and_then(|_| settings.halt.as_deref().and_then(external_halt))
    };
    let mut run = match admission.as_ref() {
        Some(admission) => crate::exec::run_test_admitted(
            &command,
            env,
            &close_admission,
            admission,
            &external_stop,
        ),
        None => crate::exec::run_test(&command, env, &close_admission),
    };
    // A crash or non-libtest nonzero exit has no decisive JSON event, so exec has no earlier
    // point at which to close admission. It is still a tree red. Do this before rustdoc can hear
    // the exit and schedule another generated binary; the transition is idempotent for a JSON
    // red or HUNG whose callback already closed it. Environment failures decide no red.
    if !run.ok && run.environment_failure().is_none() {
        close_admission();
    }
    if let Some(error) = signal_error
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
    {
        use std::fmt::Write as _;
        let _ = writeln!(run.output, "aterm-verify: first-red signal failed: {error}");
    }
    Ok(run)
}

fn runtool_output(run: &crate::exec::Run) -> String {
    use std::fmt::Write as _;

    let mut output = crate::libtest_json::render(&run.output, run.stopped_at.as_ref());
    if let Some(hung) = &run.hung {
        let _ = writeln!(
            output,
            "\naterm-verify: {}\n\ntest result: FAILED. the doctest binary was hung before \
             libtest completed.\n",
            hung.detail()
        );
    }
    output
}

/// Entry point for rustdoc's `--test-runtool` invocation.
#[must_use]
pub fn runtool_main(args: &[OsString]) -> i32 {
    let settings = match Runtool::from_env() {
        Ok(settings) => settings,
        Err(error) => {
            eprintln!("aterm-verify: {error}");
            return 2;
        }
    };
    let run = match runtool_on(args, &settings, &crate::exec::LIVE_HOST) {
        Ok(run) => run,
        Err(error) => {
            eprintln!("aterm-verify: {error}");
            return 2;
        }
    };
    let output = runtool_output(&run);
    let mut stdout = std::io::stdout().lock();
    if let Err(error) = stdout
        .write_all(output.as_bytes())
        .and_then(|()| stdout.flush())
    {
        eprintln!("aterm-verify: could not write the doctest result: {error}");
        return 2;
    }
    if run.ok { 0 } else { run.code.unwrap_or(1) }
}

/// Replace this process with the driver: cargo's descriptors (its jobserver
/// among them) are rustdoc's, and an exec keeps the whole table. Returns the
/// error that prevented it.
#[cfg(unix)]
fn become_driver(command: &mut aterm_uds::spawnfd::Command) -> std::io::Error {
    command.exec()
}

/// Off unix there is no exec: run the driver and leave with its exit code.
#[cfg(not(unix))]
fn become_driver(command: &mut aterm_uds::spawnfd::Command) -> std::io::Error {
    match command.status() {
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(e) => e,
    }
}

#[cfg(test)]
#[path = "docdriver_tests.rs"]
mod tests;
