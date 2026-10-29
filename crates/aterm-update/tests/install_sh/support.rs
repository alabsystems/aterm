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

/// A shell command running `bash <args>` in a process group of its own, as a
/// terminal runs a foreground job, SIGINT and SIGQUIT at their defaults; it prints
/// how bash ended, `signal <n>` or `exit <n>`, which `$?` cannot tell apart. The
/// group's leader, bash's `$PPID`, ignores the signals sent to the group.
pub fn group_run(args: &str) -> String {
    format!(
        "/usr/bin/perl -e 'setpgrp(0, 0); $SIG{{INT}} = $SIG{{QUIT}} = \"IGNORE\"; my $p = fork // die; if (!$p) {{ $SIG{{INT}} = $SIG{{QUIT}} = \"DEFAULT\"; exec @ARGV or die }} \
         waitpid($p, 0); print $? & 127 ? \"signal \" . ($? & 127) : \"exit \" . ($? >> 8)' {} {args}",
        q(s(&bash()))
    )
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

/// Where every [`Scratch`] is made: the runtime temporary directory on macOS,
/// so fixture filesystem walks stay within the running test's sandbox.
pub fn scratch_root() -> &'static Path {
    static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        #[cfg(target_os = "macos")]
        let target = std::env::temp_dir();
        #[cfg(not(target_os = "macos"))]
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

/// A scratch directory, removed when it drops unless its test is failing (`Drop for Scratch`).
pub struct Scratch {
    dir: Option<aterm_tempfile::TempDir>,
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
        Self {
            dir: Some(dir),
            path,
        }
    }

    pub fn join(&self, rel: &str) -> PathBuf {
        self.path.join(rel)
    }

    /// The scratch's own folder.
    pub fn path(&self) -> &Path {
        &self.path
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

    /// An executable stand-in at `rel` whose body names no per-test path, for every program
    /// install.sh only runs (by its name, or by a path it takes as it is given): on macOS a
    /// symlink to the file of `body` this process's stand-ins share (`aterm_tempfile::stand_in`),
    /// outside the scratch, so only the first exec of `body` on the machine waits on the system's
    /// code assessment, and the scratch going removes no name of a file an exec may still be
    /// assessed by. A program install.sh judges by its type or by where it resolves is a
    /// [`Self::program`].
    pub fn stand_in(&self, rel: &str, body: &str) -> PathBuf {
        let file = self.join(rel);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        aterm_tempfile::stand_in(&file, body).expect("a stand-in");
        file
    }

    /// An executable `rel` holding `body`, a regular file of its own, mode 0555, laid by temp and
    /// rename: a program install.sh judges by its type (`[[ -L ]]` in `bin_aterm_verdict` and
    /// `retire_exposed_siblings`, `may_replace`'s "a link is no program", `find -type f` in
    /// `bundle_showed_it_ran`) or by where it resolves (`gate_store_targo`'s physical path, root's
    /// `may_run` over another account's toolchain), so it is the file a real install holds, never
    /// [`Self::stand_in`]'s link to a file outside the scratch. Each is a new file every run, so
    /// its first exec waits on the system's code assessment every run (p50 134 ms over 12 fresh
    /// scripts at a load of 40, against 8 ms through a symlink to an assessed one, measured
    /// 2026-10-28 on macOS 27.0; minutes while syspolicyd is backed up): every program install.sh
    /// only runs is a [`Self::stand_in`]. A test cut off while one is still in that wait keeps
    /// its scratch (`Drop for Scratch`).
    pub fn program(&self, rel: &str, body: &str) -> PathBuf {
        use std::io::Write as _;
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let file = self.join(rel);
        let parent = file.parent().expect("a program's folder");
        std::fs::create_dir_all(parent).expect("mkdir");
        let name = file
            .file_name()
            .expect("a program's name")
            .to_string_lossy();
        let tmp = parent.join(format!(".{name}.{}.program", std::process::id()));
        let mut made = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&tmp)
            .expect("a program's temp");
        made.write_all(body.as_bytes()).expect("write a program");
        made.set_permissions(std::fs::Permissions::from_mode(0o555))
            .expect("chmod a program");
        drop(made);
        std::fs::rename(&tmp, &file).expect("a program renamed into place");
        file
    }
}

/// A test's folder can carry an ACL that denies its own removal (`group:everyone deny delete`),
/// and then the TempDir could not remove the scratch: it stayed in [`scratch_root`], and a
/// removal of the folder that holds it stopped at it. So every ACL under the scratch goes first,
/// on a failed test's unwind too ([`strip_acls`]), from what the scratch holds and from nothing
/// a link in it leads to.
///
/// A scratch dropped while its test is failing (its thread unwinding) is kept, and its path said
/// on stderr. A run cut off (a hang detector, or install.sh's own bound) may have left macOS still
/// checking a program's first start in it, and macOS 27.0.1's syspolicyd crashes when the file it
/// is checking at a first exec is removed, stranding every later exec until a restart
/// (install.sh's `showed_it_ran`): a program that has not shown it ran stays where it is.
impl Drop for Scratch {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        strip_acls(&self.path);
        if std::thread::panicking()
            && let Some(dir) = self.dir.take()
        {
            let kept = dir.keep();
            eprintln!(
                "install_sh: kept the failed test's scratch {}: a program in it may not have shown it ran",
                kept.display()
            );
        }
    }
}

/// Take every ACL off `root` and what is under it, and off nothing a link in it leads to: one
/// `chmod -N` over every entry that is not a symlink, folders before what they hold (`root` is
/// the scratch's physical path, no link). `chmod -R -N` would follow a link (with `-P` too), as
/// `chmod -N` does a link it is named, and strip the ACL of a file outside the scratch, such as
/// a stand-in's shared file; `-R` refuses `-h`, and `-N` takes no `--` (it is a file to it). A
/// folder that cannot be listed loses its own ACL before it is listed again.
#[cfg(target_os = "macos")]
fn strip_acls(root: &Path) {
    let chmod = |paths: &[PathBuf]| {
        for chunk in paths.chunks(256) {
            let _ = aterm_uds::spawnfd::command("/bin/chmod")
                .arg("-N")
                .args(chunk)
                .stdin(aterm_uds::spawnfd::Stdio::null())
                .stdout(aterm_uds::spawnfd::Stdio::null())
                .stderr(aterm_uds::spawnfd::Stdio::null())
                .status();
        }
    };
    let mut entries = Vec::new();
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        let listed = std::fs::read_dir(&folder).or_else(|_| {
            chmod(std::slice::from_ref(&folder));
            std::fs::read_dir(&folder)
        });
        entries.push(folder);
        for entry in listed.into_iter().flatten().flatten() {
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => folders.push(entry.path()),
                Ok(kind) if !kind.is_symlink() => entries.push(entry.path()),
                _ => {}
            }
        }
    }
    chmod(&entries);
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
    Row(String, Vec<String>),
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
        self.record(Item::Call(
            code.into(),
            vars.iter().map(|v| (*v).to_string()).collect(),
        ))
    }

    /// Shell code recorded as a [`Self::call`] is, run in a background subshell of the batch:
    /// it starts from the batch's state at this point, and the batch sees none of what it
    /// sets. Rows next to one another run side by side, [`rows_at_once`] at a time, and the
    /// next step or call starts once they have all ended. Rows beside one another share the
    /// batch's TMPDIR, HOME, globals and files, and nothing keeps them apart: a row must
    /// write nothing another row beside it reads or writes. As for any background job of a
    /// script, its stdin is `/dev/null` and it ignores SIGINT and SIGQUIT.
    pub fn row(&mut self, code: impl Into<String>, vars: &[&str]) -> usize {
        self.record(Item::Row(
            code.into(),
            vars.iter().map(|v| (*v).to_string()).collect(),
        ))
    }

    /// Queues `item`, a call or a row, and returns its place among the batch's records.
    fn record(&mut self, item: Item) -> usize {
        self.items.push(item);
        self.items
            .iter()
            .filter(|i| matches!(i, Item::Call(..) | Item::Row(..)))
            .count()
            - 1
    }

    /// Run the batch with HOME and TMPDIR under `scratch`.
    pub fn run(&self, scratch: &Scratch) -> Vec<Rec> {
        let dir = scratch.mkdir(&format!("lib.{}", BATCH.fetch_add(1, Ordering::Relaxed)));
        let home = scratch.mkdir("home");
        let tmp = scratch.mkdir("tmp");
        let mut driver = String::from(DRIVER_PRELUDE);
        driver.push_str(&format!("__width={}\n", rows_at_once()));
        let mut calls = Vec::new();
        for item in &self.items {
            match item {
                Item::Step(code) => {
                    driver.push_str("__drain\n");
                    driver.push_str(code);
                    driver.push_str("\n__tick\n");
                }
                Item::Call(code, vars) => {
                    driver.push_str("__drain\n__rc=0\n{\n");
                    driver.push_str(code);
                    driver.push_str(
                        "\n} >\"$__d/o\" 2>\"$__d/e\" || __rc=$?\n\
                         __rec \"$__d/records\" \"$__d/o\" \"$__d/e\" \"$__rc\"",
                    );
                    for var in vars {
                        driver.push(' ');
                        driver.push_str(var);
                    }
                    driver.push('\n');
                    calls.push(code.clone());
                }
                Item::Row(code, vars) => {
                    let n = calls.len();
                    driver.push_str("__hold\n{\n__rc=0\n{\n");
                    driver.push_str(code);
                    driver.push_str(&format!(
                        "\n}} >\"$__d/o.{n}\" 2>\"$__d/e.{n}\" || __rc=$?\n\
                         __rec \"$__d/r.{n}\" \"$__d/o.{n}\" \"$__d/e.{n}\" \"$__rc\""
                    ));
                    for var in vars {
                        driver.push(' ');
                        driver.push_str(var);
                    }
                    driver.push_str(&format!(
                        "\n__tick\n}} &\n__pids+=(\"$!\")\n__rows+=({n})\n"
                    ));
                    calls.push(code.clone());
                }
            }
        }
        driver.push_str("__drain\n");
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
            Err(aterm_uds::deadline::RunError::TimedOut) => {
                // A row's record waits for the rows queued before it, but each row that
                // returned left a record file of its own, and the batch waits for rows in
                // the order they were queued and marks `d.<n>` each that ended before its
                // record: the first with neither never returned.
                let left = |n: &usize| {
                    !dir.join(format!("r.{n}")).exists() && !dir.join(format!("d.{n}")).exists()
                };
                panic!(
                    "no call returned for {HANG:?}, so the batch was killed after {} of {} calls; \
                     the one that never returned (or a step just before it): {:?}",
                    calls.len() - (recs.len()..calls.len()).filter(left).count(),
                    calls.len(),
                    (recs.len()..calls.len())
                        .find(left)
                        .and_then(|n| calls.get(n))
                )
            }
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
# Identity in this private library driver is an explicit injected observation;
# production installer_uid reads Bash's readonly EUID instead of this fake id.
installer_uid() { id -u; }
__d="$2"
__tick() { printf . >>"$__d/progress"; }
__tick
# <records> <stdout> <stderr> <status> <var>...: one call's record, appended to <records>.
__rec() {
	local __to="$1" __of="$2" __ef="$3" __rc="$4" __v __o="" __e=""
	shift 4
	IFS= read -r -d '' __o <"$__of" || true
	IFS= read -r -d '' __e <"$__ef" || true
	{
		printf 'rc\037%s\036out\037%s\036err\037%s\036' "$__rc" "$__o" "$__e"
		for __v in "$@"; do printf 'var:%s\037%s\036' "$__v" "${!__v-}"; done
		printf '\035'
	} >>"$__to"
}
# The rows running now: each one's pid, and its place among the records.
__pids=()
__rows=()
# <n>: once row <n> has ended, mark it d.<n> if it ended before its record, so a hang
# detector that ends the batch later names a row still running, not this one.
__ended() { [ -e "$__d/r.$1" ] || : >"$__d/d.$1"; }
# Before a row starts: wait for the oldest row while __width of them run.
__hold() {
	local __k
	if [ "${#__pids[@]}" -ge "$__width" ]; then
		__k=$((${#__pids[@]} - __width))
		builtin wait "${__pids[$__k]}" || :
		__ended "${__rows[$__k]}"
	fi
}
# Before a step or a call, and at the end: wait for every row, then append their records
# in the order they were queued. A row that ended before its record ends the batch, as a
# call that did would, once it is marked d.<n>, its stderr is said, and the rows queued
# after it have ended (a hang among them is the hang detector's to end).
__drain() {
	local __i=0 __n
	while [ "$__i" -lt "${#__rows[@]}" ]; do
		__n="${__rows[$__i]}"
		builtin wait "${__pids[$__i]}" || :
		if [ ! -e "$__d/r.$__n" ]; then
			command cat "$__d/e.$__n" >&2 || :
			while [ "$__i" -lt "${#__rows[@]}" ]; do
				builtin wait "${__pids[$__i]}" || :
				__ended "${__rows[$__i]}"
				__i=$((__i + 1))
			done
			exit 1
		fi
		command cat "$__d/r.$__n" >>"$__d/records"
		__i=$((__i + 1))
	done
	__pids=()
	__rows=()
}
"#;

/// How many rows ([`Lib::row`]) a batch runs at once: four, or one per core on a machine with
/// fewer.
fn rows_at_once() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get().min(4))
}

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
