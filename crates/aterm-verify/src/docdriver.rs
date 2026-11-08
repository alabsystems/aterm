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
mod tests {
    use super::*;

    fn argv(a: &[&str]) -> Vec<OsString> {
        a.iter().map(OsString::from).collect()
    }

    /// A doctest pass as cargo spells it: the package's crate as `--test`'s
    /// input, beside the `--test-*` options that are not the flag.
    fn pass() -> Vec<OsString> {
        argv(&[
            "--edition=2024",
            "--crate-name",
            "plain",
            "--test",
            "crates/plain/src/lib.rs",
            "--test-run-directory",
            "/r/crates/plain",
            "--test-args",
            "--quiet",
        ])
    }

    #[test]
    fn only_a_doctest_pass_of_a_named_package_is_skipped() {
        let skip = OsString::from(skip_list(&["other".into(), "plain".into()]));
        let s = Some(skip.as_os_str());
        assert!(skips(&pass(), Some(OsStr::new("plain")), s));
        assert!(!skips(&pass(), Some(OsStr::new("documented")), s));
        for name in ["plai", "plainer", "lain", ""] {
            assert!(!skips(&pass(), Some(OsStr::new(name)), s), "{name:?}");
        }
        let gappy = OsString::from(",,plain,");
        assert!(skips(&pass(), Some(OsStr::new("plain")), Some(&gappy)));
        assert!(!skips(&pass(), Some(OsStr::new("")), Some(&gappy)));
    }

    #[test]
    fn whatever_the_stand_in_cannot_read_runs() {
        let skip = OsString::from("plain");
        let s = Some(skip.as_os_str());
        let plain = Some(OsStr::new("plain"));
        assert!(!skips(&argv(&["-vV"]), plain, s));
        assert!(!skips(&argv(&["@/tmp/cargo-args"]), plain, s));
        assert!(!skips(
            &argv(&["--test-args", "--quiet", "--test-run-directory", "/r"]),
            plain,
            s
        ));
        assert!(!skips(&pass(), None, s));
        assert!(!skips(&pass(), plain, None));
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt as _;
            let raw = OsStr::from_bytes(b"plain,\xff");
            assert!(!skips(&pass(), plain, Some(raw)));
        }
    }

    #[derive(Debug)]
    struct ProgressHost {
        busy_descendant: bool,
        reads: std::sync::atomic::AtomicU64,
    }

    impl ProgressHost {
        const fn blocked() -> Self {
            Self {
                busy_descendant: false,
                reads: std::sync::atomic::AtomicU64::new(0),
            }
        }

        const fn busy_descendant() -> Self {
            Self {
                busy_descendant: true,
                reads: std::sync::atomic::AtomicU64::new(0),
            }
        }
    }

    impl crate::exec::Host for ProgressHost {
        fn group_usage(&self, pgid: u32) -> Option<Vec<aterm_sysprobe::MemberUsage>> {
            Some(vec![aterm_sysprobe::MemberUsage {
                pid: pgid,
                start: 1,
                cpu_ns: self.reads.load(std::sync::atomic::Ordering::SeqCst),
                ..aterm_sysprobe::MemberUsage::default()
            }])
        }

        fn tree_usage(&self, root: u32) -> Option<Vec<crate::exec::TreeMember>> {
            let read = self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            let cpu = if self.busy_descendant {
                read
            } else {
                // Let the generated process start and publish its JSON case before the injected
                // clock becomes still. This is progress evidence, not a wall-time allowance.
                read.min(32)
            };
            Some(vec![
                crate::exec::TreeMember {
                    pid: root,
                    start: 1,
                    cpu_ns: 0,
                },
                crate::exec::TreeMember {
                    pid: root.saturating_add(1),
                    start: 2,
                    cpu_ns: cpu,
                },
            ])
        }

        fn usage_every(&self) -> Duration {
            Duration::ZERO
        }

        fn sample(
            &self,
            pid: u32,
            output_dir: &Path,
            _: &dyn Fn() -> bool,
        ) -> crate::exec::StackSample {
            let report = output_dir.join(format!("hung-{pid}.sample.txt"));
            match std::fs::write(&report, b"injected all-thread sample\n") {
                Ok(()) => crate::exec::StackSample {
                    report: Some(report),
                    error: None,
                },
                Err(error) => crate::exec::StackSample {
                    report: None,
                    error: Some(error.to_string()),
                },
            }
        }

        fn idle(&self) -> Option<Duration> {
            None
        }

        fn cores(&self) -> u32 {
            1
        }
    }

    #[cfg(unix)]
    fn fixture_settings(root: &Path, hang_after: Duration) -> Runtool {
        let halt = halt_marker(&root.join("scratch"));
        prepare_halt_marker(&halt).expect("first-red admission");
        Runtool {
            scratch: root.join("scratch"),
            samples: Some(root.join("hung-samples")),
            spec: "-p documented --doc".to_owned(),
            halt: Some(halt),
            hang_after,
        }
    }

    /// `create_new` transfers no ownership on collision. NEGATIVE CONTROL: deleting the pending
    /// path in `prepare_halt_marker`'s error cleanup destroys this pre-existing byte.
    #[test]
    fn a_pending_collision_survives_failed_protocol_preparation() {
        let root = aterm_tempfile::tempdir().expect("fixture root");
        let marker = halt_marker(root.path());
        let pending = halt_pending(&marker);
        create_halt_file(&pending, b'X').expect("pre-existing pending collision");

        let error = prepare_halt_marker(&marker).expect_err("pending create_new must collide");
        assert!(error.contains("cannot create"), "{error}");
        assert_eq!(
            std::fs::read(&pending).expect("the pre-existing pending survives"),
            b"X"
        );
        assert!(
            !marker.exists(),
            "only this invocation's admission is removed"
        );
    }

    /// A publication error must not restore OPEN. NEGATIVE CONTROL: leaving the valid pending
    /// sentinel intact makes both the live-peer probe and the next admission accept it.
    #[test]
    fn a_failed_red_rename_poison_closes_live_and_later_admission() {
        let root = aterm_tempfile::tempdir().expect("fixture root");
        let marker = halt_marker(root.path());
        prepare_halt_marker(&marker).expect("first-red protocol");

        let error = signal_halt_marker_with(&marker, |_, _| {
            Err(std::io::Error::other("injected rename refusal"))
        })
        .expect_err("the injected rename fails honestly");
        assert!(
            error.contains("injected rename refusal") && error.contains("poisoned"),
            "{error}"
        );
        let live = external_halt(&marker).expect("a live peer fails closed");
        assert!(live.contains("this peer was stopped"), "{live}");
        let later = admit_halt_marker(&marker).expect_err("a later start fails closed");
        assert!(later.contains("one-byte regular file"), "{later}");
    }

    /// Publishing RED is a directory operation, not a write through the pending sentinel.
    /// NEGATIVE CONTROL: requiring a writable pending handle makes this valid owner-private,
    /// read-only sentinel leave the protocol OPEN instead of atomically renaming it.
    #[cfg(unix)]
    #[test]
    fn a_read_only_pending_sentinel_still_publishes_red_by_rename() {
        use std::os::unix::fs::PermissionsExt;

        let root = aterm_tempfile::tempdir().expect("fixture root");
        let marker = halt_marker(root.path());
        prepare_halt_marker(&marker).expect("first-red protocol");
        let pending = halt_pending(&marker);
        std::fs::set_permissions(&pending, std::fs::Permissions::from_mode(0o400))
            .expect("make the validated sentinel read-only");

        signal_halt_marker(&marker).expect("rename publishes RED without writing the sentinel");
        assert!(matches!(
            halt_state(&marker).expect("published state"),
            HaltState::Red
        ));
        assert!(!pending.exists(), "the pending name moved atomically");
        let later = admit_halt_marker(&marker).expect_err("RED refuses every later start");
        assert!(later.contains("stopped before starting"), "{later}");
    }

    /// A publisher that loses the pending-to-RED rename race accepts the winner's valid RED.
    /// NEGATIVE CONTROL: returning the missing-pending error before re-reading RED makes this
    /// deterministic interleaving report a false publication failure.
    #[test]
    fn a_concurrent_red_publish_accepts_the_winner_before_pending_open() {
        let root = aterm_tempfile::tempdir().expect("fixture root");
        let marker = halt_marker(root.path());
        prepare_halt_marker(&marker).expect("first-red protocol");

        signal_halt_marker_after(
            &marker,
            || signal_halt_marker(&marker).expect("the competing publisher wins"),
            |from, to| std::fs::rename(from, to),
        )
        .expect("the losing publisher accepts the valid RED");
        assert!(matches!(
            halt_state(&marker).expect("published state"),
            HaltState::Red
        ));
    }

    /// The rustdoc runtool owns the generated executable, forces libtest JSON, and reports its
    /// unfinished doctest as hung when both evidence signals stay still. NEGATIVE CONTROL: the
    /// injected host lets startup CPU settle and then gives the generated process and its
    /// descendant stable identities and zero further CPU forever; without the runtool's
    /// [`crate::exec::CaseWatch`] this fixture never ends.
    #[cfg(unix)]
    #[test]
    fn a_blocked_generated_doctest_is_a_named_hung_binary() {
        let root = aterm_tempfile::tempdir().expect("fixture root");
        std::fs::create_dir(root.path().join("scratch")).expect("scratch");
        let binary = root.path().join("generated-doctest");
        crate::standin_body(
            &binary,
            "printf '%s\\n' \\
             '{\"type\":\"suite\",\"event\":\"started\",\"test_count\":1}' \\
             '{\"type\":\"test\",\"event\":\"started\",\"name\":\"docs::stuck\"}'\n\
             /usr/bin/mkfifo \"$0.pipe\"\n\
             exec /bin/cat \"$0.pipe\"",
        );
        let settings = fixture_settings(root.path(), Duration::ZERO);
        let run = runtool_on(
            &[binary.clone().into_os_string()],
            &settings,
            &ProgressHost::blocked(),
        )
        .expect("the runtool invocation");
        let hung = run.hung.clone().expect("the generated binary is hung");
        assert_eq!(hung.binary, binary.display().to_string());
        assert_eq!(hung.spec, "-p documented --doc");
        assert_eq!(hung.running, ["docs::stuck"]);
        assert_eq!(hung.window, Duration::ZERO);
        assert!(hung.sample.report.is_some(), "{hung:?}");
        let output = runtool_output(&run);
        assert!(
            output.contains("HUNG — test binary")
                && output.contains("still running: docs::stuck")
                && output.contains("test result: FAILED. the doctest binary was hung"),
            "{output}"
        );

        // RED is persistent, so the next admission refuses before exec. NEGATIVE CONTROL: this
        // stand-in's first instruction leaves a sentinel, so an accidentally open path is seen.
        let sentinel = root.path().join("queued-ran");
        let queued = root.path().join("queued-doctest");
        crate::standin_body(
            &queued,
            &format!(
                "printf ran > '{}'; printf '%s\\n' \\
                 '{{\"type\":\"suite\",\"event\":\"started\",\"test_count\":0}}' \\
                 '{{\"type\":\"suite\",\"event\":\"ok\",\"passed\":0,\"failed\":0,\"ignored\":0,\"measured\":0,\"filtered_out\":0,\"exec_time\":0.0}}'",
                sentinel.display()
            ),
        );
        let stopped = runtool_on(
            &[queued.into_os_string()],
            &settings,
            &ProgressHost::busy_descendant(),
        )
        .expect_err("the first-red admission stops a queued runtool");
        assert!(stopped.contains("stopped before starting"), "{stopped}");
        assert!(!sentinel.exists(), "the queued doctest must not exec");
    }

    /// RED never waits behind the admission lock. A start that has proved actual contention
    /// acquires the lock only after RED is visible and then refuses before exec. NEGATIVE
    /// CONTROL: serializing publication behind this holder misses the one-minute hang detector;
    /// checking the protocol before taking its lock lets the sentinel appear.
    #[cfg(unix)]
    #[test]
    fn first_red_overtakes_a_start_contending_on_admission() {
        use aterm_spec::wait::MUST_HAPPEN;

        let root = aterm_tempfile::tempdir().expect("fixture root");
        std::fs::create_dir(root.path().join("scratch")).expect("scratch");
        let settings = fixture_settings(root.path(), Duration::ZERO);
        let marker = settings.halt.clone().expect("armed runtool");
        let holder = open_halt_file(&marker, HALT_ADMISSION)
            .expect("valid admission")
            .expect("admission exists");
        aterm_uds::file_lock::lock_exclusive_until(
            &holder,
            std::time::Instant::now() + MUST_HAPPEN,
            "fixture could not hold admission",
        )
        .expect("hold admission");

        let sentinel = root.path().join("contended-ran");
        let binary = root.path().join("contended-doctest");
        crate::standin_body(
            &binary,
            &format!(
                "printf ran > '{}'; printf '%s\\n' \\
                 '{{\"type\":\"suite\",\"event\":\"started\",\"test_count\":0}}' \\
                 '{{\"type\":\"suite\",\"event\":\"ok\",\"passed\":0,\"failed\":0,\"ignored\":0,\"measured\":0,\"filtered_out\":0,\"exec_time\":0.0}}'",
                sentinel.display()
            ),
        );

        let (contended_tx, contended_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let queued_settings = settings.clone();
        let queued = std::thread::spawn(move || {
            let result = runtool_on_with_admission(
                &[binary.into_os_string()],
                &queued_settings,
                &ProgressHost::busy_descendant(),
                |path| {
                    admit_halt_marker_with(path, || {
                        let _ = contended_tx.send(());
                    })
                },
            );
            let _ = done_tx.send(result);
        });
        contended_rx
            .recv_timeout(MUST_HAPPEN)
            .expect("the queued runtool actually contended on admission");

        let (signal_tx, signal_rx) = std::sync::mpsc::channel();
        let signal_marker = marker.clone();
        let signaler = std::thread::spawn(move || {
            let _ = signal_tx.send(signal_halt_marker(&signal_marker));
        });
        let signaled = signal_rx.recv_timeout(MUST_HAPPEN);
        if signaled.is_err() {
            let _ = holder.unlock();
            let _ = signaler.join();
            let _ = done_rx.recv_timeout(MUST_HAPPEN);
            let _ = queued.join();
            panic!("RED waited behind the admission holder");
        }
        signaled
            .expect("checked above")
            .expect("atomic RED publication");
        signaler.join().expect("signaler");
        holder.unlock().expect("release admission holder");

        let stopped = done_rx
            .recv_timeout(MUST_HAPPEN)
            .expect("the contended runtool ended")
            .expect_err("RED refuses the contended start");
        queued.join().expect("queued runtool");
        assert!(stopped.contains("stopped before starting"), "{stopped}");
        assert!(!sentinel.exists(), "the contended doctest must not exec");
    }

    /// The OPEN read is the start-admission linearization point. A holder paused after that read
    /// may spawn after RED is published, but it must observe RED immediately and reap the child
    /// as NOT RUN. NEGATIVE CONTROL: removing the live probe leaves this fixture blocked forever.
    #[cfg(unix)]
    #[test]
    fn a_start_admitted_before_red_is_stopped_after_its_spawn() {
        use aterm_spec::wait::MUST_HAPPEN;

        let root = aterm_tempfile::tempdir().expect("fixture root");
        std::fs::create_dir(root.path().join("scratch")).expect("scratch");
        let settings = fixture_settings(root.path(), Duration::ZERO);
        let marker = settings.halt.clone().expect("armed runtool");
        let binary = root.path().join("admitted-doctest");
        crate::standin_body(
            &binary,
            "/usr/bin/mkfifo \"$0.pipe\"\nexec /bin/cat \"$0.pipe\"",
        );

        let (admitted_tx, admitted_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let peer_settings = settings.clone();
        let peer = std::thread::spawn(move || {
            let result = runtool_on_with_admission(
                &[binary.into_os_string()],
                &peer_settings,
                &ProgressHost::busy_descendant(),
                |path| {
                    let admission = admit_halt_marker(path)?;
                    admitted_tx
                        .send(())
                        .map_err(|error| format!("cannot publish fixture admission: {error}"))?;
                    release_rx.recv_timeout(MUST_HAPPEN).map_err(|error| {
                        format!("timed out holding fixture admission before spawn: {error}")
                    })?;
                    Ok(admission)
                },
            );
            let _ = done_tx.send(result);
        });
        admitted_rx
            .recv_timeout(MUST_HAPPEN)
            .expect("the peer passed its OPEN check");
        signal_halt_marker(&marker).expect("publish RED while the admitted peer is paused");
        release_tx.send(()).expect("release admitted peer to spawn");

        let stopped = done_rx
            .recv_timeout(MUST_HAPPEN)
            .expect("the admitted peer observed RED")
            .expect("the admitted peer returns its NOT RUN record");
        peer.join().expect("admitted peer");
        assert!(!stopped.ok && stopped.hung.is_none(), "{stopped:?}");
        assert!(
            stopped
                .spawn_error
                .as_deref()
                .is_some_and(|why| why.contains("another doctest binary reached the first red")),
            "{stopped:?}"
        );
        assert!(stopped.output.contains("NOT RUN"), "{}", stopped.output);
    }

    /// An admitted peer that remains healthy and consumes descendant CPU still obeys the shared
    /// first red. NEGATIVE CONTROL: without the live sentinel probe this busy fixture blocks on
    /// its pipe forever after its sibling becomes HUNG.
    #[cfg(unix)]
    #[test]
    fn a_live_busy_doctest_peer_stops_when_another_is_hung() {
        use aterm_spec::wait::{MUST_HAPPEN, until_within};

        let root = aterm_tempfile::tempdir().expect("fixture root");
        std::fs::create_dir(root.path().join("scratch")).expect("scratch");
        let settings = fixture_settings(root.path(), Duration::ZERO);
        let ready = root.path().join("busy-ready");
        let busy = root.path().join("busy-doctest");
        crate::standin_body(
            &busy,
            &format!(
                "/usr/bin/mkfifo \"$0.pipe\"\n\
                 printf '%s\\n' \\
                 '{{\"type\":\"suite\",\"event\":\"started\",\"test_count\":1}}' \\
                 '{{\"type\":\"test\",\"event\":\"started\",\"name\":\"docs::busy_peer\"}}'\n\
                 printf ready > '{}'\n\
                 exec /bin/cat \"$0.pipe\"",
                ready.display()
            ),
        );
        let (peer_tx, peer_rx) = std::sync::mpsc::channel();
        let peer_settings = settings.clone();
        let peer = std::thread::spawn(move || {
            let result = runtool_on(
                &[busy.into_os_string()],
                &peer_settings,
                &ProgressHost::busy_descendant(),
            );
            let _ = peer_tx.send(result);
        });
        until_within(MUST_HAPPEN, "the busy doctest's exec proof", || {
            ready.exists().then_some(())
        });

        let blocked = root.path().join("blocked-doctest");
        crate::standin_body(
            &blocked,
            "printf '%s\\n' \\
             '{\"type\":\"suite\",\"event\":\"started\",\"test_count\":1}' \\
             '{\"type\":\"test\",\"event\":\"started\",\"name\":\"docs::hung_sibling\"}'\n\
             /usr/bin/mkfifo \"$0.pipe\"\n\
             exec /bin/cat \"$0.pipe\"",
        );
        let hung_run = runtool_on(
            &[blocked.into_os_string()],
            &settings,
            &ProgressHost::blocked(),
        )
        .expect("the sibling runtool invocation");
        assert!(hung_run.hung.is_some(), "{hung_run:?}");

        let stopped = peer_rx
            .recv_timeout(MUST_HAPPEN)
            .expect("the live peer observed RED")
            .expect("the admitted peer returns its NOT RUN record");
        peer.join().expect("live peer");
        assert!(!stopped.ok && stopped.hung.is_none(), "{stopped:?}");
        assert!(
            stopped
                .spawn_error
                .as_deref()
                .is_some_and(|why| why.contains("another doctest binary reached the first red")),
            "{stopped:?}"
        );
        assert!(stopped.output.contains("NOT RUN"), "{}", stopped.output);
    }

    /// A malformed admission fails closed before exec, while a halt-off run has no admission and
    /// runs independently of a red file. These are the negative controls for I/O failure and for
    /// applying first-red behavior to an ordinary full-sweep verify run.
    #[cfg(unix)]
    #[test]
    fn doctest_first_red_fails_closed_and_halt_off_does_not_adopt_it() {
        let root = aterm_tempfile::tempdir().expect("fixture root");
        let scratch = root.path().join("scratch");
        std::fs::create_dir(&scratch).expect("scratch");
        let marker = halt_marker(&scratch);
        prepare_halt_marker(&marker).expect("first-red admission");
        std::fs::OpenOptions::new()
            .write(true)
            .open(&marker)
            .expect("open admission")
            .set_len(0)
            .expect("mutate admission");
        let sentinel = root.path().join("ran");
        let binary = root.path().join("generated-doctest");
        crate::standin_body(
            &binary,
            &format!(
                "printf ran > '{}'; printf '%s\\n' \\
                 '{{\"type\":\"suite\",\"event\":\"started\",\"test_count\":0}}' \\
                 '{{\"type\":\"suite\",\"event\":\"ok\",\"passed\":0,\"failed\":0,\"ignored\":0,\"measured\":0,\"filtered_out\":0,\"exec_time\":0.0}}'",
                sentinel.display()
            ),
        );
        let armed = Runtool {
            scratch: scratch.clone(),
            samples: None,
            spec: "-p documented --doc".to_owned(),
            halt: Some(marker),
            hang_after: Duration::ZERO,
        };
        let refused = runtool_on(
            &[binary.clone().into_os_string()],
            &armed,
            &ProgressHost::busy_descendant(),
        )
        .expect_err("malformed admission fails closed");
        assert!(refused.contains("one-byte regular file"), "{refused}");
        assert!(!sentinel.exists(), "malformed admission must prevent exec");

        let halt_off = Runtool {
            halt: None,
            ..armed
        };
        let run = runtool_on(
            &[binary.into_os_string()],
            &halt_off,
            &ProgressHost::busy_descendant(),
        )
        .expect("halt-off runtool");
        assert!(run.ok, "{run:?}");
        assert!(sentinel.exists(), "halt-off doctest runs");
    }

    /// A non-libtest crash is a decisive binary red even though it emits no failed JSON case.
    /// NEGATIVE CONTROL: without `runtool_on`'s post-run transition the admission stays OPEN and
    /// the final admission succeeds.
    #[cfg(unix)]
    #[test]
    fn a_non_json_doctest_red_closes_first_red_admission() {
        let root = aterm_tempfile::tempdir().expect("fixture root");
        std::fs::create_dir(root.path().join("scratch")).expect("scratch");
        let binary = root.path().join("generated-doctest");
        crate::standin_body(&binary, "exit 9");
        let settings = fixture_settings(root.path(), Duration::ZERO);
        let run = runtool_on(
            &[binary.into_os_string()],
            &settings,
            &ProgressHost::busy_descendant(),
        )
        .expect("the runtool invocation");
        assert!(!run.ok && run.code == Some(9), "{run:?}");
        assert!(run.environment_failure().is_none(), "{run:?}");
        let stopped = admit_halt_marker(settings.halt.as_deref().expect("armed"))
            .expect_err("the crash closes every later start");
        assert!(stopped.contains("stopped before starting"), "{stopped}");
    }

    /// A generated binary waiting while its descendant consumes CPU is making progress. The
    /// fixture itself sleeps, while the injected complete tree advances only the descendant's
    /// CPU generation; changing that generation to zero turns this into the hung control above.
    #[cfg(unix)]
    #[test]
    fn a_busy_doctest_descendant_prevents_a_hung_red() {
        let root = aterm_tempfile::tempdir().expect("fixture root");
        std::fs::create_dir(root.path().join("scratch")).expect("scratch");
        let binary = root.path().join("generated-doctest");
        crate::standin_body(
            &binary,
            "printf '%s\\n' \\
             '{\"type\":\"suite\",\"event\":\"started\",\"test_count\":1}' \\
             '{\"type\":\"test\",\"event\":\"started\",\"name\":\"docs::busy_child\"}'\n\
             /bin/sleep 0.05\n\
             printf '%s\\n' \\
             '{\"type\":\"test\",\"event\":\"ok\",\"name\":\"docs::busy_child\",\"exec_time\":0.05}' \\
             '{\"type\":\"suite\",\"event\":\"ok\",\"passed\":1,\"failed\":0,\"ignored\":0,\"measured\":0,\"filtered_out\":0,\"exec_time\":0.05}'",
        );
        let settings = fixture_settings(root.path(), Duration::ZERO);
        let run = runtool_on(
            &[binary.into_os_string()],
            &settings,
            &ProgressHost::busy_descendant(),
        )
        .expect("the runtool invocation");
        assert!(run.ok && run.hung.is_none(), "{run:?}");
        let rendered = crate::libtest_json::render(&run.output, run.stopped_at.as_ref());
        assert!(
            rendered.contains("test docs::busy_child ... ok")
                && rendered.contains("test result: ok."),
            "{rendered}"
        );
    }
}
