// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ARENA lanes: real-window measurements of the one shipped `aterm`, and
//! head-to-heads against the pinned ghostty tip.

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::{Command, command};

use crate::perf::LaneRun;
use crate::perf::workspace_root;

pub(crate) mod cat;
pub(crate) mod compare;
pub(crate) mod inner;
pub(crate) mod live;
pub(crate) mod scroll;
pub(crate) mod start;
pub(crate) mod vte;

pub(crate) use live::Group;
pub(crate) use start::BackendRequest;

/// Why an arena lane stopped short: a fact about this box it cannot measure
/// past (a SKIP, with the fix), or a failure.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Halt {
    Skip { why: String, fix: String },
    Fail(String),
}

impl Halt {
    pub(crate) fn skip(why: impl Into<String>, fix: impl Into<String>) -> Halt {
        Halt::Skip {
            why: why.into(),
            fix: fix.into(),
        }
    }
}

impl From<String> for Halt {
    fn from(e: String) -> Halt {
        Halt::Fail(e)
    }
}

impl From<&str> for Halt {
    fn from(e: &str) -> Halt {
        Halt::Fail(e.to_string())
    }
}

/// An arena lane's outcome.
pub(crate) fn outcome(result: Result<String, Halt>) -> LaneRun {
    match result {
        Ok(at) => LaneRun::measured(at),
        Err(Halt::Skip { why, fix }) => LaneRun::skip(why, fix),
        Err(Halt::Fail(e)) => LaneRun::fail(e),
    }
}

/// A program this box lacks is a box fact; any other spawn error is a failure.
fn spawn_halt(what: &str, e: &std::io::Error, fix: &str) -> Halt {
    if e.kind() == std::io::ErrorKind::NotFound {
        Halt::skip(format!("{what} is not installed"), fix)
    } else {
        Halt::Fail(format!("could not run {what}: {e}"))
    }
}

/// The arena lanes' options, parsed and range-checked.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Args {
    pub binary: Option<PathBuf>,
    pub baseline: Option<PathBuf>,
    pub candidate: Option<PathBuf>,
    pub samples: Option<u32>,
    pub pairs: Option<u32>,
    pub backend: BackendRequest,
    pub settle_ms: Option<u64>,
    pub timeout_s: Option<u64>,
    pub cooldown_s: Option<f64>,
    pub json_out: Option<PathBuf>,
    pub allow_identical: bool,
    pub allow_legacy_metric: bool,
    pub warm: bool,
    pub dev_smoke: bool,
    pub dmg: Option<PathBuf>,
    pub trials: Option<u32>,
    pub fill_lines: Option<u64>,
    pub scrub_steps: Option<u32>,
    pub columns: Option<u32>,
    pub rows: Option<u32>,
    pub max_secs: Option<u32>,
}

fn positive<T: std::str::FromStr + PartialOrd + Default>(
    flag: &str,
    raw: &str,
) -> Result<T, String> {
    match raw.parse::<T>() {
        Ok(v) if v > T::default() => Ok(v),
        _ => Err(format!("`{flag}` needs a positive integer, got `{raw}`")),
    }
}

impl Args {
    /// Parse the options the arena lanes read.
    pub(crate) fn from_options(options: &BTreeMap<&'static str, String>) -> Result<Args, String> {
        let mut a = Args::default();
        for (flag, raw) in options {
            let path = || Some(PathBuf::from(raw));
            match *flag {
                "--binary" => a.binary = path(),
                "--baseline" => a.baseline = path(),
                "--candidate" => a.candidate = path(),
                "--json-out" => a.json_out = path(),
                "--dmg" => a.dmg = path(),
                "--samples" => a.samples = Some(positive(flag, raw)?),
                "--pairs" => a.pairs = Some(positive(flag, raw)?),
                "--timeout-s" => a.timeout_s = Some(positive(flag, raw)?),
                "--trials" => a.trials = Some(positive(flag, raw)?),
                "--fill-lines" => a.fill_lines = Some(positive(flag, raw)?),
                "--scrub-steps" => a.scrub_steps = Some(positive(flag, raw)?),
                "--columns" => a.columns = Some(positive(flag, raw)?),
                "--rows" => a.rows = Some(positive(flag, raw)?),
                "--max-secs" => a.max_secs = Some(positive(flag, raw)?),
                "--settle-ms" => {
                    a.settle_ms = Some(raw.parse().map_err(|_| {
                        format!("`--settle-ms` needs a non-negative integer, got `{raw}`")
                    })?);
                }
                "--cooldown-s" => {
                    let v: f64 = raw.parse().map_err(|_| {
                        format!("`--cooldown-s` needs a non-negative number, got `{raw}`")
                    })?;
                    if !(v.is_finite() && v >= 0.0) {
                        return Err(format!(
                            "`--cooldown-s` needs a non-negative number, got `{raw}`"
                        ));
                    }
                    a.cooldown_s = Some(v);
                }
                "--backend" => {
                    a.backend = match raw.as_str() {
                        "auto" => BackendRequest::Auto,
                        "cpu" => BackendRequest::Cpu,
                        "gpu" => BackendRequest::Gpu,
                        _ => return Err(format!("`--backend` is auto, cpu or gpu, got `{raw}`")),
                    };
                }
                "--allow-identical" => a.allow_identical = true,
                "--allow-legacy-metric" => a.allow_legacy_metric = true,
                "--warm" => a.warm = true,
                "--dev-smoke" => a.dev_smoke = true,
                "--no-windows" | "--admit" => {}
                other => return Err(format!("internal: `{other}` has no parser")),
            }
        }
        Ok(a)
    }
}

/// The arena's cache directory reused across runs outside the checkout.
pub(crate) fn arena_dir() -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join("aterm-arena");
    if dir.starts_with(workspace_root()) {
        return Err(format!(
            "the arena cache {} is inside the checkout ($TMPDIR points into it); cargo would \
             claim the vtebench checkout as a workspace member. Point TMPDIR elsewhere.",
            dir.display()
        ));
    }
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    Ok(dir)
}

/// The longest unix-socket path the kernel takes (`sun_path` less its NUL).
pub(crate) const MAX_SUN_PATH: usize = 103;

/// Where a run's sockets live, whatever `$TMPDIR` is: macOS's per-user
/// `$TMPDIR` alone is 49 bytes before a run adds its HOME tree to it.
const SOCKET_BASE: &str = "/tmp";

/// `dir/name`, refused up front when it would not fit in `sun_path`.
pub(crate) fn socket_in(dir: &Path, name: &str) -> Result<PathBuf, String> {
    let path = dir.join(name);
    let len = path.as_os_str().len();
    if len > MAX_SUN_PATH {
        return Err(format!(
            "the socket path {} is {len} bytes; a unix socket takes at most {MAX_SUN_PATH}",
            path.display()
        ));
    }
    Ok(path)
}

/// A private directory for one run, removed on drop and on interrupt.
pub(crate) struct Scratch(PathBuf);

impl Scratch {
    /// Under `$TMPDIR`: a run's private HOME trees, logs and staged binaries.
    pub(crate) fn new(tag: &str) -> Result<Scratch, String> {
        live::make_dir(&std::env::temp_dir(), &format!("aterm-{tag}-")).map(Scratch)
    }

    /// Under `/tmp`, named `at.<pid>.<n>`: a run's unix sockets, short enough
    /// for every name a lane gives one ([`Scratch::sock`]).
    pub(crate) fn sockets() -> Result<Scratch, String> {
        live::make_dir(Path::new(SOCKET_BASE), "at.").map(Scratch)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }

    /// A socket path in this directory, checked against [`MAX_SUN_PATH`].
    pub(crate) fn sock(&self, name: &str) -> Result<PathBuf, String> {
        socket_in(&self.0, name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        live::remove_dir(&self.0);
    }
}

/// Owner-only permissions on a directory the run's state lives in.
pub(crate) fn private(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("could not chmod {}: {e}", dir.display()))
}

/// The lowercase hex SHA-256 of a file, streamed.
pub(crate) fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file =
        std::fs::File::open(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let mut hasher = aterm_digest::Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| format!("could not read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// A program's trimmed stdout, or `None` when it cannot run or fails.
pub(crate) fn stdout_of(cmd: &mut Command) -> Option<String> {
    let out = cmd.output().ok().filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The CPU, as `sysctl` names it (the architecture where it cannot).
pub(crate) fn chip() -> String {
    stdout_of(command(aterm_uds::os_path!("sysctl")).args(["-n", "machdep.cpu.brand_string"]))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| std::env::consts::ARCH.to_string())
}

/// `pmset -g therm`, its non-empty lines joined by ` | `, or `unavailable`.
pub(crate) fn thermal() -> String {
    let text = stdout_of(command("pmset").args(["-g", "therm"])).unwrap_or_default();
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        "unavailable".to_string()
    } else {
        lines.join(" | ")
    }
}

/// Where a run sits under rule zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Machine {
    pub chip: String,
    /// Off the M4 Max bench box: every number is labelled non-publishable.
    pub dev_smoke: bool,
}

impl Machine {
    pub(crate) fn label(&self) -> String {
        if self.dev_smoke {
            format!("{} — DEV-SMOKE, NON-PUBLISHABLE", self.chip)
        } else {
            self.chip.clone()
        }
    }
}

/// Rule zero §2's hardware (`docs/measured/arena/README.md`): publishable
/// head-to-heads run on the M4 Max only, as `sysctl` names it exactly.
pub(crate) fn rule_zero_hardware(chip: &str) -> bool {
    chip.trim() == "Apple M4 Max"
}

/// The bench-box check: publishable head-to-heads run on [`rule_zero_hardware`]
/// only, and anywhere else only when the caller says `--dev-smoke`.
pub(crate) fn bench_box(chip: &str, dev_smoke: bool) -> Result<Machine, Halt> {
    let bench_class = rule_zero_hardware(chip);
    if !bench_class && !dev_smoke {
        return Err(Halt::skip(
            format!("this is '{chip}', not the M4 Max bench machine"),
            "re-run with --dev-smoke to measure here, every number labelled non-publishable",
        ));
    }
    Ok(Machine {
        chip: chip.to_string(),
        dev_smoke: !bench_class,
    })
}

/// [`bench_box`] for this machine and the ledger its entry goes to, with the
/// thermal reading and the manual rule-zero reminders printed.
pub(crate) fn preflight(dev_smoke: bool) -> Result<(Machine, LedgerBook), Halt> {
    if !cfg!(target_os = "macos") {
        return Err(Halt::skip(
            "the head-to-head lanes open macOS windows",
            "run them on the bench Mac",
        ));
    }
    let machine = bench_box(&chip(), dev_smoke)?;
    let ledger = LedgerBook::open()?;
    if machine.dev_smoke {
        eprintln!(
            "== DEV-SMOKE RUN on '{}' — NON-PUBLISHABLE ==",
            machine.chip
        );
    }
    eprintln!("thermal: {}", thermal());
    eprintln!(
        "REMINDERS (rule zero §3): Do Not Disturb ON, no other apps, each window frontmost \
         and visible — do not touch the machine during trials."
    );
    Ok((machine, ledger))
}

/// The pins every arena run names, from `crates/aterm-verify/perf/arena/manifest.toml`.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub(crate) struct Pins {
    pub ghostty: GhosttyPins,
    pub harness: HarnessPins,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub(crate) struct GhosttyPins {
    pub tip: GhosttyTip,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub(crate) struct GhosttyTip {
    pub artifact: String,
    pub artifact_sha256: String,
    /// What the tip build self-identifies as (`CFBundleShortVersionString`).
    pub bundle_version: String,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub(crate) struct HarnessPins {
    pub vtebench: RepoPin,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub(crate) struct RepoPin {
    pub repo: String,
    pub commit: String,
}

pub(crate) fn manifest_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("perf/arena/manifest.toml")
}

pub(crate) fn pins() -> Result<Pins, String> {
    let path = manifest_path();
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    aterm_toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// The pinned ghostty tip, sha-verified and installed under the arena cache.
#[derive(Debug, Clone)]
pub(crate) struct Ghostty {
    pub app: PathBuf,
    pub commit: String,
    pub sha256: String,
}

pub(crate) fn ghostty_tip(pins: &Pins, dmg: Option<&Path>) -> Result<Ghostty, Halt> {
    let tip = &pins.ghostty.tip;
    let arena = arena_dir()?;
    let dmg = match dmg {
        Some(d) => d.to_path_buf(),
        None => {
            let d = arena.join(format!("Ghostty-tip-{}.dmg", tip.bundle_version));
            if !d.is_file() {
                eprintln!("downloading {}", tip.artifact);
                let part = d.with_extension("part");
                let out = command(aterm_uds::os_path!("curl"))
                    .args(["-sSfL", "-o"])
                    .arg(&part)
                    .arg(&tip.artifact)
                    .output()
                    .map_err(|e| spawn_halt("curl", &e, "install curl, or pass --dmg PATH"))?;
                if !out.status.success() {
                    return Err(Halt::skip(
                        format!(
                            "the pinned ghostty dmg could not be downloaded ({}): {}",
                            out.status,
                            String::from_utf8_lossy(&out.stderr).trim()
                        ),
                        "check the network, or pass --dmg PATH",
                    ));
                }
                if sha256_file(&part)? != tip.artifact_sha256 {
                    let _ = std::fs::remove_file(&part);
                    return Err(Halt::Fail(format!(
                        "{} served a dmg that does not hash to the pinned sha256, so it is not \
                         the pinned build, and nothing was cached; {} names it by its \
                         per-commit artifact, never the rolling `tip` release, which moves.",
                        tip.artifact,
                        manifest_path().display()
                    )));
                }
                std::fs::rename(&part, &d)
                    .map_err(|e| format!("could not place {}: {e}", d.display()))?;
            }
            d
        }
    };
    if sha256_file(&dmg)? != tip.artifact_sha256 {
        return Err(Halt::Fail(format!(
            "{} does not hash to the pinned sha256, so it is not the pinned build{}; {} names \
             it by its per-commit artifact, never the rolling `tip` release, which moves.",
            dmg.display(),
            if dmg.starts_with(&arena) {
                " (a download an earlier run cached: remove it, and the next run downloads the \
                 pinned artifact)"
            } else {
                ""
            },
            manifest_path().display()
        )));
    }
    let app = arena.join(format!("Ghostty-tip-{}.app", tip.bundle_version));
    if !app.is_dir() {
        let part = app.with_extension("part");
        let _ = std::fs::remove_dir_all(&part);
        let mount = Scratch::new("ghostty-dmg")?;
        run_ok(
            command(aterm_uds::os_path!("hdiutil"))
                .args(["attach", "-nobrowse", "-quiet", "-readonly", "-mountpoint"])
                .arg(mount.path())
                .arg(&dmg),
            "hdiutil attach",
        )?;
        let copied = run_ok(
            command(aterm_uds::os_path!("cp"))
                .arg("-R")
                .arg(mount.path().join("Ghostty.app"))
                .arg(&part),
            "cp",
        );
        let _ = command(aterm_uds::os_path!("hdiutil"))
            .args(["detach", "-quiet"])
            .arg(mount.path())
            .status();
        copied?;
        std::fs::rename(&part, &app)
            .map_err(|e| format!("could not place {}: {e}", app.display()))?;
    }
    let plist = app.join("Contents/Info.plist");
    let version = stdout_of(
        command(aterm_uds::os_path!("defaults"))
            .arg("read")
            .arg(&plist)
            .arg("CFBundleShortVersionString"),
    )
    .unwrap_or_default();
    if version != tip.bundle_version {
        return Err(Halt::Fail(format!(
            "the installed ghostty self-identifies as '{version}', the pin is '{}'",
            tip.bundle_version
        )));
    }
    eprintln!("ghostty tip verified: {version}");
    Ok(Ghostty {
        app,
        commit: tip.bundle_version.clone(),
        sha256: tip.artifact_sha256.clone(),
    })
}

/// Run `cmd` and require success.
pub(crate) fn run_ok(cmd: &mut Command, what: &str) -> Result<(), String> {
    let out = cmd
        .output()
        .map_err(|e| format!("could not run {what}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{what} exited {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim_end()
        ))
    }
}

/// The one shipped executable under test, release-built, with provenance.
#[derive(Debug, Clone)]
pub(crate) struct Aterm {
    pub bin: PathBuf,
    pub sha256: String,
    /// `HEAD`'s short sha, `-dirty` when the tree has changes.
    pub git: String,
}

impl Aterm {
    pub(crate) fn provenance(&self, how: &str) -> String {
        format!("{} sha256={} ({how})", self.git, self.sha256)
    }
}

/// `targo --unverified build --release -p aterm`, then the binary's identity.
pub(crate) fn build_aterm() -> Result<Aterm, String> {
    let driver = crate::perf::driver::cargo_driver()?;
    let args = ["--release", "-q", "-p", "aterm"];
    eprintln!("$ {}", driver.display_workspace("build", &args));
    let status = driver
        .workspace_command("build")
        .args(args)
        .current_dir(workspace_root())
        .status()
        .map_err(|e| format!("could not run targo: {e}"))?;
    if !status.success() {
        return Err(format!(
            "the aterm release build exited {:?}",
            status.code()
        ));
    }
    let bin = release_dir()?.join("aterm");
    Ok(Aterm {
        sha256: sha256_file(&bin)?,
        git: git_provenance(&workspace_root()),
        bin,
    })
}

/// The checkout at `dir`: `HEAD`'s short sha, `-dirty` when its tracked files
/// have changes, `unknown` when git cannot say.
pub(crate) fn git_provenance(dir: &Path) -> String {
    let sha = stdout_of(
        command(aterm_uds::os_path!("git"))
            .args(["rev-parse", "--short=12", "HEAD"])
            .current_dir(dir),
    )
    .unwrap_or_else(|| "unknown".into());
    let clean = command(aterm_uds::os_path!("git"))
        .args(["diff", "--quiet"])
        .current_dir(dir)
        .status()
        .is_ok_and(|s| s.success());
    if clean { sha } else { format!("{sha}-dirty") }
}

/// Where the release build lands.
fn release_dir() -> Result<PathBuf, String> {
    Ok(target_dir()?.join("release"))
}

/// The target directory ([`crate::perf::driver::target_dir`]).
pub(crate) fn target_dir() -> Result<PathBuf, String> {
    crate::perf::driver::target_dir()
}

/// Strip every inherited `ATERM_*` override from a terminal under test: a
/// caller's terminal-specific settings must not change the run.
pub(crate) fn without_aterm_env(cmd: &mut Command) -> &mut Command {
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("ATERM_") && name != aterm_uds::process::VERIFY_RUN_VAR {
            cmd.env_remove(key);
        }
    }
    cmd
}

/// Wait for a launched instance's control socket: `Ok` once the path exists,
/// an error if the leader exits first or `timeout` passes.
pub(crate) fn await_socket(
    group: &mut Group,
    sock: &Path,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = group.exited() {
            return Err(format!(
                "aterm exited before creating its socket ({status})"
            ));
        }
        if sock.exists() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "control socket {} did not appear within {timeout:?}",
                sock.display()
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// `<aterm> ctl --sock <sock> …`: the instance's own executable as its client.
pub(crate) fn ctl(bin: &Path, sock: &Path) -> Command {
    let mut cmd = command(bin);
    cmd.arg("ctl").arg("--sock").arg(sock);
    cmd
}

/// How long a `ctl` client may run past its verb's own `timeout=`: the hang
/// detector for a client that wedges, never a latency budget (`start`'s own).
const CTL_HUNG: Duration = Duration::from_secs(60);

/// The verb's own `timeout=<ms>`, which the instance bounds its answer by; none
/// when it names none.
fn verb_timeout(verb: &[&str]) -> Duration {
    verb.iter()
        .find_map(|a| a.strip_prefix("timeout=")?.parse::<u64>().ok())
        .map_or(Duration::ZERO, Duration::from_millis)
}

/// Run one `ctl` verb and return its stdout, or the failure.
pub(crate) fn ctl_out(bin: &Path, sock: &Path, verb: &[&str]) -> Result<String, String> {
    let deadline = Instant::now() + verb_timeout(verb) + CTL_HUNG;
    let out = aterm_uds::deadline::output_until(ctl(bin, sock).args(verb), deadline)
        .map_err(|e| format!("ctl {}: {e}", verb.join(" ")))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "ctl {} exited {:?}: {}",
            verb.join(" "),
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim_end()
        ))
    }
}

/// The socket an in-terminal half ([`inner`]) reports on: the runner waits on
/// the report itself — an event — with a hang ceiling, never on a file poll.
pub(crate) struct Inbox {
    path: PathBuf,
    rx: mpsc::Receiver<String>,
    _dir: Scratch,
}

impl Inbox {
    pub(crate) fn bind() -> Result<Inbox, String> {
        let dir = Scratch::sockets()?;
        let path = dir.sock("report.sock")?;
        let listener = UnixListener::bind(&path)
            .map_err(|e| format!("could not listen on {}: {e}", path.display()))?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut text = String::new();
                let _ = stream.read_to_string(&mut text);
                let _ = tx.send(text);
            }
        });
        Ok(Inbox {
            path,
            rx,
            _dir: dir,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// The one report, or an error when nothing arrives within `ceiling`.
    pub(crate) fn wait(&self, ceiling: Duration) -> Result<String, String> {
        self.rx
            .recv_timeout(ceiling)
            .map(|t| t.trim().to_string())
            .map_err(|_| format!("no report on {} within {ceiling:?}", self.path.display()))
    }
}

impl Drop for Inbox {
    fn drop(&mut self) {
        let _ = UnixStream::connect(&self.path);
    }
}

/// Send one report line to the runner's [`Inbox`].
pub(crate) fn report(sock: &Path, line: &str) -> Result<(), String> {
    let mut stream = UnixStream::connect(sock)
        .map_err(|e| format!("could not reach the runner at {}: {e}", sock.display()))?;
    stream
        .write_all(line.as_bytes())
        .map_err(|e| format!("could not report: {e}"))
}

/// The cooldown between competitors (rule zero §2) — a protocol interval.
pub(crate) const COOLDOWN: Duration = Duration::from_secs(5);

/// The fields a ledger entry fills into `docs/measured/arena/TEMPLATE.md`.
pub(crate) struct Ledger<'a> {
    pub date: &'a str,
    pub dim: &'a str,
    pub machine: &'a str,
    pub macos: &'a str,
    pub aterm: &'a str,
    pub competitors: &'a str,
}

/// The template with its `{{…}}` fields filled.
pub(crate) fn fill_template(template: &str, l: &Ledger<'_>) -> String {
    template
        .replace("{{DATE}}", l.date)
        .replace("{{DIM}}", l.dim)
        .replace("{{MACHINE}}", l.machine)
        .replace("{{MACOS}}", l.macos)
        .replace("{{ATERM}}", l.aterm)
        .replace("{{COMPETITORS}}", l.competitors)
}

/// The append-only arena ledger, `docs/measured/arena/`.
pub(crate) struct LedgerBook {
    dir: PathBuf,
    template: String,
}

impl LedgerBook {
    pub(crate) fn open() -> Result<LedgerBook, Halt> {
        let dir = workspace_root().join("docs/measured/arena");
        let path = dir.join("TEMPLATE.md");
        match std::fs::read_to_string(&path) {
            Ok(template) => Ok(LedgerBook { dir, template }),
            Err(e) => Err(Halt::skip(
                format!("no arena ledger template at {} ({e})", path.display()),
                "run the lane in the development checkout, which carries docs/measured/arena",
            )),
        }
    }

    /// Write a new entry `<date>-<slug>.md` (append-only: a second run on one
    /// day gets its own file), returning its path.
    pub(crate) fn write(
        &self,
        slug: &str,
        ledger: &Ledger<'_>,
        body: &str,
    ) -> Result<PathBuf, String> {
        let mut text = fill_template(&self.template, ledger);
        text.push_str(body);
        let mut path = self.dir.join(format!("{}-{slug}.md", ledger.date));
        let mut n = 2;
        while path.exists() {
            path = self.dir.join(format!("{}-{slug}-{n}.md", ledger.date));
            n += 1;
        }
        std::fs::write(&path, text)
            .map_err(|e| format!("could not write {}: {e}", path.display()))?;
        eprintln!(
            "ledger entry written: {} — fill in the display/thermal/config fields before \
             committing.",
            path.display()
        );
        Ok(path)
    }
}

pub(crate) fn macos_version() -> String {
    stdout_of(command(aterm_uds::os_path!("sw_vers")).arg("-productVersion"))
        .unwrap_or_else(|| "unknown".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `ctl` call's deadline starts from its verb's own timeout: what
    /// `timeout=<ms>` says, and nothing for a verb without one or an unreadable
    /// value.
    #[test]
    fn a_ctl_call_is_bounded_by_its_verbs_timeout_and_the_hang_detector() {
        assert_eq!(
            verb_timeout(&["await", "idle", "1000", "timeout=60000"]),
            Duration::from_secs(60)
        );
        assert_eq!(verb_timeout(&["metrics", "--json"]), Duration::ZERO);
        assert_eq!(verb_timeout(&["send", "timeout=x"]), Duration::ZERO);
    }

    #[test]
    fn options_parse_into_typed_args_and_refuse_bad_values() {
        let mut o = BTreeMap::new();
        o.insert("--samples", "9".to_string());
        o.insert("--backend", "gpu".to_string());
        o.insert("--cooldown-s", "0.5".to_string());
        o.insert("--settle-ms", "0".to_string());
        o.insert("--warm", String::new());
        let a = Args::from_options(&o).expect("parses");
        assert_eq!(a.samples, Some(9));
        assert_eq!(a.backend, BackendRequest::Gpu);
        assert_eq!(a.cooldown_s, Some(0.5));
        assert_eq!(a.settle_ms, Some(0));
        assert!(a.warm);
        for (flag, bad) in [
            ("--samples", "0"),
            ("--samples", "x"),
            ("--backend", "metal"),
            ("--cooldown-s", "-1"),
            ("--settle-ms", "-5"),
        ] {
            let mut o = BTreeMap::new();
            o.insert(flag, bad.to_string());
            assert!(Args::from_options(&o).is_err(), "{flag} {bad}");
        }
    }

    #[test]
    fn the_bench_box_is_the_m4_max_or_a_labelled_dev_smoke() {
        assert!(!bench_box("Apple M4 Max", false).expect("bench").dev_smoke);
        for chip in ["Apple M5 Max", "Apple M4 Pro", "Virtual Apple M4 Max"] {
            match bench_box(chip, false) {
                Err(Halt::Skip { fix, .. }) => assert!(fix.contains("--dev-smoke"), "{fix}"),
                other => panic!("{chip}: expected a skip, got {other:?}"),
            }
            let smoke = bench_box(chip, true).expect("labelled");
            assert!(smoke.dev_smoke, "{chip}");
            assert!(smoke.label().ends_with("NON-PUBLISHABLE"), "{chip}");
        }
    }

    #[test]
    fn the_committed_pins_parse() {
        let p = pins().expect("manifest.toml parses into the typed pins");
        assert_eq!(p.ghostty.tip.artifact_sha256.len(), 64);
        assert_eq!(p.harness.vtebench.commit.len(), 40);
    }

    #[test]
    fn the_template_fills_every_field() {
        let l = Ledger {
            date: "2026-10-15",
            dim: "D1",
            machine: "m",
            macos: "27.0",
            aterm: "abc sha256=x",
            competitors: "ghostty",
        };
        let out = fill_template(
            "# {{DIM}} — {{DATE}}\n{{MACHINE}} {{MACOS}} {{ATERM}} {{COMPETITORS}}\n",
            &l,
        );
        assert_eq!(out, "# D1 — 2026-10-15\nm 27.0 abc sha256=x ghostty\n");
    }

    /// Every socket a lane makes fits `sun_path` whatever `$TMPDIR` is, and
    /// the layout it replaced — the socket inside the run's HOME tree under
    /// macOS's 49-byte per-user `$TMPDIR` — is refused before aterm sees it.
    #[test]
    fn a_socket_path_fits_sun_path_whatever_tmpdir_is() {
        let widest = Path::new(SOCKET_BASE).join(format!("at.{}.{}", u32::MAX, u32::MAX));
        let sock = socket_in(&widest, "trial-999-candidate.sock").expect("fits");
        assert!(sock.as_os_str().len() <= MAX_SUN_PATH, "{}", sock.display());
        let tmpdir = Path::new("/var/folders/rw/3088njqs1vsf5365hfl3yr8h0000gn/T/");
        assert_eq!(tmpdir.as_os_str().len(), 49);
        let deep = tmpdir
            .join("aterm-arena-compare-99999-3b9ac9ff")
            .join("trial-999-candidate");
        let e = socket_in(&deep, "control.sock").expect_err("past sun_path");
        assert!(e.contains("at most 103"), "{e}");
    }

    #[test]
    fn a_scratch_directory_is_private_and_gone_on_drop() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = Scratch::sockets().expect("socket dir");
        let path = dir.path().to_path_buf();
        let mode = std::fs::metadata(&path).expect("made").permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        drop(dir);
        assert!(!path.exists(), "{}", path.display());
    }

    #[test]
    fn sha256_streams_a_file() {
        let dir = Scratch::new("atv-sha").expect("scratch");
        let f = dir.path().join("f");
        std::fs::write(&f, b"abc").expect("write");
        assert_eq!(
            sha256_file(&f).expect("hash"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_report_reaches_the_inbox() {
        let inbox = Inbox::bind().expect("bind");
        report(inbox.path(), "drain 1.5\n").expect("report");
        assert_eq!(
            inbox.wait(Duration::from_secs(60)).expect("report"),
            "drain 1.5"
        );
        let silent = Inbox::bind().expect("bind");
        assert!(silent.wait(Duration::from_millis(1)).is_err());
    }
}
