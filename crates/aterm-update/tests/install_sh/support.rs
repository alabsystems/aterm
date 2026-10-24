// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Driving tools/install.sh from a test.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::Command;

/// A wait for something that must happen: a hang detector, never a latency budget.
pub const HANG: Duration = Duration::from_secs(60);

const TOOLCHAIN: [&str; 5] = ["cargo", "rustc", "rustup", "targo", "alab-targo"];

/// The system dirs every run's PATH ends with: `/usr/bin:/bin`, unless one of them holds a
/// [`TOOLCHAIN`] program, when it is one folder under [`scratch_root`] linking every other
/// program of theirs.
pub fn system_path() -> &'static str {
    static PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PATH.get_or_init(|| {
        const DIRS: [&str; 2] = ["/usr/bin", "/bin"];
        let toolchain_in = |dir: &str| TOOLCHAIN.iter().any(|t| Path::new(dir).join(t).exists());
        if !DIRS.into_iter().any(toolchain_in) {
            return DIRS.join(":");
        }
        let changed: Vec<String> = DIRS
            .into_iter()
            .map(|dir| {
                let at = std::fs::metadata(dir).and_then(|meta| meta.modified());
                let since = at
                    .ok()
                    .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok());
                format!("{:x}", since.unwrap_or_default().as_nanos())
            })
            .collect();
        let view = scratch_root().join(format!("install-sh-system-{}", changed.join("-")));
        if !view.is_dir() {
            let part = scratch_root().join(format!("install-sh-system.{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&part);
            std::fs::create_dir_all(&part).expect("the system view");
            for dir in DIRS {
                for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                    let name = entry.file_name();
                    let at = part.join(&name);
                    if TOOLCHAIN.iter().any(|t| name == *t) || at.symlink_metadata().is_ok() {
                        continue;
                    }
                    std::os::unix::fs::symlink(entry.path(), at).expect("a system program's link");
                }
            }
            if std::fs::rename(&part, &view).is_err() {
                let _ = std::fs::remove_dir_all(&part);
            }
        }
        s(&view).to_string()
    })
}

/// The checkout this test binary was built from.
pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the checkout root")
}

/// The installer under test.
pub fn install_sh() -> PathBuf {
    root().join("tools/install.sh")
}

/// The names of install.sh's bootstrap verifier pin (crates/aterm-bootstrap-verify's `pin::KEYS`).
#[cfg_attr(
    not(target_os = "linux"),
    allow(
        dead_code,
        reason = "this suite empties the pin only in its Linux-only tests"
    )
)]
pub const VERIFIER_PIN: [&str; 4] = [
    "LINUX_VERIFIER_REPO",
    "LINUX_VERIFIER_TAG",
    "LINUX_VERIFIER_SHA256_X86_64",
    "LINUX_VERIFIER_SHA256_AARCH64",
];

/// A copy of install.sh at `rel` under `scratch` with its bootstrap verifier pin emptied.
#[cfg_attr(
    not(target_os = "linux"),
    allow(
        dead_code,
        reason = "this suite empties the pin only in its Linux-only tests"
    )
)]
pub fn unpinned_install_sh(scratch: &Scratch, rel: &str) -> PathBuf {
    let text = read(&install_sh());
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    for key in VERIFIER_PIN {
        let prefix = format!("{key}=");
        let at: Vec<usize> = (0..lines.len())
            .filter(|n| lines[*n].starts_with(&prefix))
            .collect();
        assert_eq!(at.len(), 1, "install.sh sets {key} on one line of its own");
        lines[at[0]] = format!("{key}=\"\"");
    }
    let mut unpinned = lines.join("\n");
    unpinned.push('\n');
    let file = scratch.write(rel, unpinned);
    chmod(&file, 0o755);
    file
}

/// The installer's floor: macOS's stock bash 3.2 where it exists, else PATH's bash.
pub fn bash() -> PathBuf {
    let stock = Path::new("/bin/bash");
    if stock.is_file() {
        return stock.to_path_buf();
    }
    which("bash").expect("a bash on PATH")
}

/// The first executable `name` on this process's PATH.
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// `s` single-quoted for bash.
pub fn q(s: &str) -> String {
    let mut quoted = String::with_capacity(s.len() + 2);
    quoted.push('\'');
    for c in s.chars() {
        if c == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(c);
        }
    }
    quoted.push('\'');
    quoted
}

/// Whether curl's `argv` follows redirects.
pub fn follows_redirects(argv: &[&str]) -> bool {
    argv.iter().any(|arg| match arg.strip_prefix("--") {
        Some(long) => long.starts_with("location"),
        None => arg
            .strip_prefix('-')
            .is_some_and(|cluster| cluster.contains('L')),
    })
}

/// A path as a `&str`.
pub fn s(path: &Path) -> &str {
    path.to_str().expect("scratch paths are UTF-8")
}

/// Where every [`Scratch`] is made.
pub fn scratch_root() -> &'static Path {
    static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        let target = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
        #[cfg(target_os = "linux")]
        if let Err(why) = aterm_update::linux::install_path::refusal(&target) {
            use std::os::unix::fs::MetadataExt as _;
            let private = |dir: &Path| {
                std::fs::symlink_metadata(dir).is_ok_and(|m| {
                    // SAFETY: getuid has no preconditions and cannot fail.
                    m.is_dir() && m.uid() == unsafe { libc::getuid() } && m.mode() & 0o077 == 0
                })
            };
            return std::env::var_os("XDG_RUNTIME_DIR")
                .map(PathBuf::from)
                .filter(|dir| dir.is_absolute() && private(dir))
                .unwrap_or_else(|| {
                    panic!(
                        "the Linux verifier refuses every store under the target dir ({why}), \
                         and no private $XDG_RUNTIME_DIR stands in for it"
                    )
                });
        }
        target
    })
}

/// A scratch directory.
pub struct Scratch {
    _dir: aterm_tempfile::TempDir,
    path: PathBuf,
}

impl Scratch {
    pub fn new() -> Self {
        let dir = aterm_tempfile::Builder::new()
            .prefix("install-sh.")
            .tempdir_in(scratch_root())
            .expect("a scratch dir");
        let path = dir
            .path()
            .canonicalize()
            .expect("the scratch dir's physical path");
        Self { _dir: dir, path }
    }

    pub fn join(&self, rel: &str) -> PathBuf {
        self.path.join(rel)
    }

    /// `rel`, created with its parents.
    pub fn mkdir(&self, rel: &str) -> PathBuf {
        let dir = self.join(rel);
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    /// `rel` holding `body`, its parents created.
    pub fn write(&self, rel: &str, body: impl AsRef<[u8]>) -> PathBuf {
        let file = self.join(rel);
        write(&file, body);
        file
    }

    /// An executable `rel` holding `body`, written fresh (its own inode).
    pub fn exe(&self, rel: &str, body: &str) -> PathBuf {
        let file = self.write(rel, body);
        chmod(&file, 0o755);
        file
    }

    /// An executable stand-in at `rel` whose body names no per-test path.
    pub fn stand_in(&self, rel: &str, body: &str) -> PathBuf {
        let file = self.join(rel);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        aterm_tempfile::stand_in(&file, body).expect("a stand-in");
        file
    }
}

/// `path` holding `body`, its parents created.
pub fn write(path: &Path, body: impl AsRef<[u8]>) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(path, body).expect("write");
}

pub fn chmod(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

pub fn symlink(target: impl AsRef<Path>, link: &Path) {
    std::os::unix::fs::symlink(target, link).expect("symlink");
}

pub fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Every entry under `dir`, relative and sorted; empty when `dir` is absent.
pub fn tree(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let rel = path.strip_prefix(base).expect("under base");
            out.push(rel.display().to_string());
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                walk(base, &path, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// One finished child.
#[derive(Debug)]
pub struct Run {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Run {
    /// The exit code; a death by signal is a test failure.
    pub fn code(&self) -> i32 {
        self.status
            .unwrap_or_else(|| panic!("killed by a signal: {}", self.all()))
    }

    /// stdout then stderr, as a person at the terminal reads them.
    pub fn all(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

/// `cmd` run to completion under [`HANG`].
pub fn run(cmd: &mut Command) -> Run {
    let out = aterm_uds::deadline::output_until(cmd, Instant::now() + HANG)
        .unwrap_or_else(|e| panic!("{:?} {e}", cmd.get_program()));
    ran(&out)
}

fn ran(out: &std::process::Output) -> Run {
    Run {
        status: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// A scratch HOME and TMPDIR for whole-script runs.
pub struct Home {
    pub home: PathBuf,
    pub tmp: PathBuf,
}

impl Scratch {
    /// `name/home` and `name/tmp`, created.
    pub fn home(&self, name: &str) -> Home {
        Home {
            home: self.mkdir(&format!("{name}/home")),
            tmp: self.mkdir(&format!("{name}/tmp")),
        }
    }
}

/// The environment of a whole-script run.
pub fn hermetic(home: &Home, extra: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = vec![
        ("HOME".into(), s(&home.home).into()),
        ("TMPDIR".into(), s(&home.tmp).into()),
        ("SHELL".into(), "/bin/zsh".into()),
        ("PATH".into(), system_path().into()),
        (
            "ATERM_INSTALL_DIR".into(),
            s(&home.home.join("Applications")).into(),
        ),
        ("ATERM_NO_TOOLCHAIN".into(), "1".into()),
    ];
    for (key, value) in extra {
        env.retain(|(k, _)| k != key);
        env.push(((*key).into(), (*value).into()));
    }
    env
}

/// The whole installer, `bash <script> <args>`, in [`hermetic`]'s environment.
pub fn installer(script: &Path, home: &Home, extra: &[(&str, &str)], args: &[&str]) -> Run {
    let mut cmd = aterm_uds::spawnfd::command(bash());
    cmd.arg(script).args(args).env_clear();
    for (key, value) in hermetic(home, extra) {
        cmd.env(key, value);
    }
    run(&mut cmd)
}

/// A shell pipeline `code` run by `/bin/sh -c` in exactly `env`.
pub fn sh(code: &str, env: &[(&str, &str)]) -> Run {
    let mut cmd = aterm_uds::spawnfd::command("/bin/sh");
    cmd.arg("-c").arg(code).env_clear();
    for (key, value) in env {
        cmd.env(key, value);
    }
    run(&mut cmd)
}

enum Item {
    Step(String),
    Call(String, Vec<String>),
}

/// One recorded library call.
#[derive(Debug)]
pub struct Rec {
    pub code: String,
    pub rc: i32,
    pub out: String,
    pub err: String,
    vars: BTreeMap<String, String>,
}

impl Rec {
    /// A global the call named; a name it did not ask for is a test bug.
    pub fn var(&self, name: &str) -> &str {
        self.vars
            .get(name)
            .unwrap_or_else(|| panic!("{}: {name} was not recorded", self.code))
    }
}

/// A batch of library calls into install.sh, run in one bash.
pub struct Lib {
    script: PathBuf,
    items: Vec<Item>,
    env: Vec<(String, String)>,
}

static BATCH: AtomicU32 = AtomicU32::new(0);

impl Lib {
    pub fn new() -> Self {
        Self::of(&install_sh())
    }

    /// A batch over `script` (a copy of install.sh, for a negative control).
    pub fn of(script: &Path) -> Self {
        Self {
            script: script.to_path_buf(),
            items: Vec::new(),
            env: vec![("PATH".into(), system_path().into())],
        }
    }

    /// Set `key` in the batch's environment (PATH included).
    pub fn env(&mut self, key: &str, value: &str) -> &mut Self {
        self.env.retain(|(k, _)| k != key);
        self.env.push((key.into(), value.into()));
        self
    }

    /// Shell code run at this point, unrecorded; a failure ends the batch.
    pub fn step(&mut self, code: impl Into<String>) -> &mut Self {
        self.items.push(Item::Step(code.into()));
        self
    }

    /// Shell code run at this point in the batch's own shell (globals it sets persist).
    pub fn call(&mut self, code: impl Into<String>, vars: &[&str]) -> usize {
        self.items.push(Item::Call(
            code.into(),
            vars.iter().map(|v| (*v).to_string()).collect(),
        ));
        self.items
            .iter()
            .filter(|i| matches!(i, Item::Call(..)))
            .count()
            - 1
    }

    /// Run the batch with HOME and TMPDIR under `scratch`.
    pub fn run(&self, scratch: &Scratch) -> Vec<Rec> {
        let dir = scratch.mkdir(&format!("lib.{}", BATCH.fetch_add(1, Ordering::Relaxed)));
        let home = scratch.mkdir("home");
        let tmp = scratch.mkdir("tmp");
        let mut driver = String::from(DRIVER_PRELUDE);
        let mut calls = Vec::new();
        for item in &self.items {
            match item {
                Item::Step(code) => {
                    driver.push_str(code);
                    driver.push_str("\n__tick\n");
                }
                Item::Call(code, vars) => {
                    driver.push_str("__rc=0\n{\n");
                    driver.push_str(code);
                    driver.push_str("\n} >\"$__d/o\" 2>\"$__d/e\" || __rc=$?\n__rec \"$__rc\"");
                    for var in vars {
                        driver.push(' ');
                        driver.push_str(var);
                    }
                    driver.push('\n');
                    calls.push(code.clone());
                }
            }
        }
        let driver_path = dir.join("driver.sh");
        write(&driver_path, driver);
        let mut cmd = aterm_uds::spawnfd::command(bash());
        cmd.arg(&driver_path)
            .arg(&self.script)
            .arg(&dir)
            .env_clear()
            .env("HOME", &home)
            .env("TMPDIR", &tmp);
        for (key, value) in &self.env {
            cmd.env(key, value);
        }
        let (records, ticks) = (dir.join("records"), dir.join("progress"));
        let len = |file: &Path| std::fs::metadata(file).map_or(0, |m| m.len());
        let mut grown = || len(&records) + len(&ticks);
        let out = aterm_uds::deadline::output_while_progressing(&mut cmd, HANG, &mut grown);
        let raw = std::fs::read(&records).unwrap_or_default();
        let raw = String::from_utf8_lossy(&raw);
        let recs: Vec<Rec> = raw
            .split('\u{1d}')
            .filter(|r| !r.is_empty())
            .zip(&calls)
            .map(|(record, code)| parse_record(record, code))
            .collect();
        let done = match out {
            Ok(out) => ran(&out),
            Err(aterm_uds::deadline::RunError::TimedOut) => panic!(
                "no call returned for {HANG:?}, so the batch was killed after {} of {} calls; \
                 the one that never returned (or a step just before it): {:?}",
                recs.len(),
                calls.len(),
                calls.get(recs.len())
            ),
            Err(e) => panic!("{:?} {e}", bash()),
        };
        assert_eq!(
            recs.len(),
            calls.len(),
            "the batch stopped after {} of {} calls (exit {:?}): {}\nnext: {:?}",
            recs.len(),
            calls.len(),
            done.status,
            done.all(),
            calls.get(recs.len())
        );
        recs
    }
}

fn parse_record(record: &str, code: &str) -> Rec {
    let mut rec = Rec {
        code: code.to_string(),
        rc: -1,
        out: String::new(),
        err: String::new(),
        vars: BTreeMap::new(),
    };
    for field in record.split('\u{1e}').filter(|f| !f.is_empty()) {
        let (key, value) = field.split_once('\u{1f}').expect("a key and a value");
        let value = value.trim_end_matches('\n').to_string();
        match key {
            "rc" => rec.rc = value.parse().expect("a status"),
            "out" => rec.out = value,
            "err" => rec.err = value,
            var => {
                rec.vars
                    .insert(var.trim_start_matches("var:").to_string(), value);
            }
        }
    }
    rec
}

const DRIVER_PRELUDE: &str = r#"ATERM_INSTALL_LIBRARY_ONLY=1
. "$1"
__d="$2"
__tick() { printf . >>"$__d/progress"; }
__tick
__rec() {
	local __rc="$1" __v __o="" __e=""
	shift
	IFS= read -r -d '' __o <"$__d/o" || true
	IFS= read -r -d '' __e <"$__d/e" || true
	{
		printf 'rc\037%s\036out\037%s\036err\037%s\036' "$__rc" "$__o" "$__e"
		for __v in "$@"; do printf 'var:%s\037%s\036' "$__v" "${!__v-}"; done
		printf '\035'
	} >>"$__d/records"
}
"#;

/// A deterministic generator for the differential corpora (xorshift64*).
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    pub fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(n).expect("small")).expect("small")
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}
