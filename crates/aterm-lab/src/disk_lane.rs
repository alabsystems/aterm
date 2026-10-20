// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `disk-lane`: the disk awareness live lane (design
//! `docs/DESIGN-disk-awareness-2026-10-17.md` §6.4) — a real aterm, never the
//! owner's, acting on a scratch volume.
//!
//! 1. A named skip where `hdiutil` is not there or this host has under
//!    [`HOST_FREE_MIN`] free: the lane must not be what fills the disk.
//! 2. An 8 GiB sparse APFS image, attached `-nobrowse` (no Finder window on a
//!    desktop a person uses) at a mount point outside `/Volumes` (discovery
//!    never looks under `/Volumes`); the instance's `HOME` is a folder on it.
//! 3. `[disk] warn_free_gib = 6`, `auto_free_gib = 5`, tier 3 on: `ok` from 6
//!    GiB, `low` 5-6, `short` 4-5, an emergency under 4.
//! 4. Fixtures, every file and folder dated [`IDLE_DAYS`] back but what a case
//!    writes today: `idle/` (its build directory idle, `doc/` and `criterion/`
//!    at its root), `recent-index/` (its git index written now), `edited/` (a
//!    working-tree file written now), `locked/` (a cargo lock this process
//!    holds), `linked/` (`~/.local/bin/tool` links into its `target/`), and the
//!    build cache of `gone/ws`, a workspace nested in a checkout that is gone.
//!    Then a ballast file fills the volume to about 4.5 GiB free.
//! 5. `status disk=` reads `short`; `await sys free>=5GiB` answers `OK sys`;
//!    the journal's removals run in tier order; the kept set is exactly what
//!    the witnesses keep: the four checkouts' `deps/` whole (tier 2 took their
//!    idle `incremental/` but the locked one's), `idle/`'s profile emptied and
//!    kept with its lock files, its `doc/` and `criterion/` untouched, the gone
//!    workspace's cache removed.
//! 6. Filled past the emergency, the journal shows the reserve released before
//!    anything else of that pass.
//! 7. The instance ends by its process group; the image is detached and
//!    removed with the world.
//!
//! Each wait in the lane is a one-minute hang detector. The lane is run by
//! hand, never by a default test run: reading inside an attached image can
//! raise macOS's removable-volume consent dialog where the terminal that runs
//! it lacks Full Disk Access (one did on 2026-10-17, atpkg's `stage_dmg`).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::instance::{
    BOOT, CONFIG_OFF, Instance, Launch, World, config_after, resolve_aterm, take_flag, take_value,
};
use crate::{Fail, LAB, Res, stop};

/// The lane runs only with this much free on this host.
pub const HOST_FREE_MIN: u64 = 24 << 30;

/// How far back the fixtures are dated.
const IDLE_DAYS: u64 = 8;

/// The image's size.
const IMAGE: &str = "8g";

/// What one fixture's build directory holds in each of its caches.
const CACHE_BYTES: u64 = 150 << 20;

/// A one-minute hang detector for what must happen.
const MUST: Duration = Duration::from_secs(60);

/// The lines of the instance's aterm.toml after its own tables.
const DISK: &[&str] = &[
    "[disk]",
    "warn_free_gib = 6",
    "auto_free_gib = 5",
    "idle_build_dirs = true",
];

/// The checkouts laid on the volume, by name.
const CHECKOUTS: &[&str] = &["idle", "recent-index", "edited", "locked", "linked"];

/// A mounted image, detached and removed when it drops.
struct Image {
    file: PathBuf,
    mnt: PathBuf,
}

impl Image {
    fn attach(dir: &Path) -> Result<Image, String> {
        let file = dir.join("lane.sparseimage");
        let mnt = dir.join("mnt");
        std::fs::create_dir_all(&mnt).map_err(|e| format!("cannot make {}: {e}", mnt.display()))?;
        run(
            "/usr/bin/hdiutil",
            &[
                "create",
                "-quiet",
                "-size",
                IMAGE,
                "-fs",
                "APFS",
                "-type",
                "SPARSE",
                "-volname",
                "atermdisklane",
            ],
            Some(&file.with_extension("")),
        )?;
        run(
            "/usr/bin/hdiutil",
            &[
                "attach",
                "-quiet",
                "-nobrowse",
                "-noautoopen",
                "-mountpoint",
                &mnt.to_string_lossy(),
            ],
            Some(&file),
        )?;
        Ok(Image { file, mnt })
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        let _ = run(
            "/usr/bin/hdiutil",
            &["detach", "-quiet", "-force", &self.mnt.to_string_lossy()],
            None,
        );
        let _ = std::fs::remove_file(&self.file);
    }
}

/// Run `program args… [last]` to completion.
fn run(program: &str, args: &[&str], last: Option<&Path>) -> Result<(), String> {
    let mut cmd = aterm_uds::spawnfd::command(program);
    cmd.args(args);
    if let Some(l) = last {
        cmd.arg(l);
    }
    let out = cmd
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{program} {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Write `bytes` of a pattern to `path` (its folders made).
fn fill(path: &Path, bytes: u64) -> Result<(), String> {
    use std::io::Write as _;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut f = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let chunk = vec![0xa5u8; 1 << 20];
    let mut left = bytes;
    while left > 0 {
        let n = usize::try_from(left.min(1 << 20)).unwrap_or(1 << 20);
        f.write_all(&chunk[..n])
            .map_err(|e| format!("{}: {e}", path.display()))?;
        left -= n as u64;
    }
    // Ordered, not durable: the image is scratch and nothing here must outlive
    // a power loss; the lane reads the volume's free space next.
    aterm_durable::ordered(&f).map_err(|e| format!("{}: {e}", path.display()))
}

/// A cargo build directory at `dir`: a signed tag, `.rustc_info.json`, one
/// profile `debug/` with its three locks, `.fingerprint/`, `deps/`, `build/`
/// and `incremental/` of [`CACHE_BYTES`] each (deps and incremental).
fn lay_target(dir: &Path) -> Result<(), String> {
    let debug = dir.join("debug");
    for d in [
        ".fingerprint/lay-1",
        "build",
        "deps",
        "incremental/lay-1/s-1",
    ] {
        std::fs::create_dir_all(debug.join(d)).map_err(|e| format!("{}: {e}", debug.display()))?;
    }
    let write =
        |p: PathBuf, b: &[u8]| std::fs::write(&p, b).map_err(|e| format!("{}: {e}", p.display()));
    write(
        dir.join("CACHEDIR.TAG"),
        b"Signature: 8a477f597d28d172789f06886806bc55\n# a cargo target directory\n",
    )?;
    write(dir.join(".rustc_info.json"), b"{}")?;
    for lock in [".cargo-lock", ".cargo-build-lock", ".cargo-artifact-lock"] {
        write(debug.join(lock), b"")?;
    }
    write(
        debug.join(".fingerprint/lay-1/lib-lay"),
        b"0123456789abcdef",
    )?;
    fill(&debug.join("deps/liblay.rlib"), CACHE_BYTES)?;
    fill(
        &debug.join("incremental/lay-1/s-1/query-cache.bin"),
        CACHE_BYTES,
    )
}

/// A checkout at `dir`: its git files, a manifest, a source file and its
/// `target/`.
fn lay_checkout(dir: &Path) -> Result<(), String> {
    let write =
        |p: PathBuf, b: &[u8]| std::fs::write(&p, b).map_err(|e| format!("{}: {e}", p.display()));
    std::fs::create_dir_all(dir.join(".git/logs"))
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    std::fs::create_dir_all(dir.join("src")).map_err(|e| format!("{}: {e}", dir.display()))?;
    write(dir.join(".git/index"), b"DIRC")?;
    write(dir.join(".git/logs/HEAD"), b"0 1 a\n")?;
    write(dir.join("Cargo.toml"), b"[package]\nname = \"lay\"\n")?;
    write(dir.join("src/main.rs"), b"fn main() {}\n")?;
    lay_target(&dir.join("target"))
}

/// Date every file and folder under `root` (and `root`) `days` back, folders
/// after what they hold.
fn backdate(root: &Path, days: u64) -> Result<(), String> {
    let when = SystemTime::now() - Duration::from_secs(days * 86_400);
    let mut stack = vec![(root.to_path_buf(), false)];
    while let Some((p, visited)) = stack.pop() {
        let meta = std::fs::symlink_metadata(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() && !visited {
            stack.push((p.clone(), true));
            for e in std::fs::read_dir(&p).map_err(|e| format!("{}: {e}", p.display()))? {
                let e = e.map_err(|e| format!("{}: {e}", p.display()))?;
                stack.push((e.path(), false));
            }
            continue;
        }
        std::fs::File::open(&p)
            .and_then(|f| f.set_modified(when))
            .map_err(|e| format!("cannot date {}: {e}", p.display()))?;
    }
    Ok(())
}

/// Touch `path` now.
fn touch_now(path: &Path) -> Result<(), String> {
    std::fs::File::open(path)
        .and_then(|f| f.set_modified(SystemTime::now()))
        .map_err(|e| format!("cannot touch {}: {e}", path.display()))
}

/// Free bytes on the volume holding `path`.
fn free_on(path: &Path) -> Option<u64> {
    aterm_agent::harness::disk::free_bytes(path)
}

/// The lane's verdict: each check named, each a pass or why not.
#[derive(Default)]
struct Verdict {
    rows: Vec<(String, Result<(), String>)>,
}

impl Verdict {
    fn check(&mut self, name: &str, ok: bool, why: impl FnOnce() -> String) {
        let r = if ok { Ok(()) } else { Err(why()) };
        println!(
            "  {} {name}{}",
            if r.is_ok() { "PASS" } else { "FAIL" },
            r.as_ref()
                .err()
                .map_or_else(String::new, |w| format!(": {w}"))
        );
        self.rows.push((name.to_owned(), r));
    }

    fn passed(&self) -> bool {
        self.rows.iter().all(|(_, r)| r.is_ok())
    }
}

/// The value of `key=` in a status line.
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split_whitespace()
        .find_map(|w| w.strip_prefix(key).and_then(|v| v.strip_prefix('=')))
}

/// `disk-lane`: the lane (the module docs).
///
/// # Errors
/// Usage, or the world, the image or the instance could not be made.
pub fn cmd_disk_lane(args: &[String]) -> Res {
    stop::install();
    let mut a = args.to_vec();
    let aterm = take_value(&mut a, "--aterm")?;
    let keep = take_flag(&mut a, "--keep");
    if let Some(extra) = a.first() {
        return Err(Fail::usage(format!(
            "disk-lane: unexpected '{extra}' ({LAB} help)"
        )));
    }
    // 1. The named skips.
    if !Path::new("/usr/bin/hdiutil").exists() {
        println!("disk-lane: NOT RUN (skip: no hdiutil on this host)");
        return Ok(3);
    }
    match free_on(Path::new("/tmp")) {
        Some(f) if f >= HOST_FREE_MIN => {}
        free => {
            println!(
                "disk-lane: NOT RUN (skip: {} free here, the lane needs {} GiB so it cannot be what fills the disk)",
                free.map_or_else(|| "unknown".to_owned(), |f| format!("{} GiB", f >> 30)),
                HOST_FREE_MIN >> 30
            );
            return Ok(3);
        }
    }
    let aterm = resolve_aterm(aterm).map_err(|e| Fail::exit(3, e))?;
    // 2. The world, its HOME on the image.
    let disk: Vec<String> = DISK.iter().map(|l| (*l).to_owned()).collect();
    let mut world = World::new(&config_after(CONFIG_OFF, true, &disk))?;
    if keep {
        world.keep();
    }
    let image = Image::attach(&world.dir)?;
    world.rehome(&image.mnt.join("home"))?;
    let home = world.home.clone();
    println!(
        "disk-lane: image at {} (HOME {})",
        image.mnt.display(),
        home.display()
    );

    // 4. The fixtures.
    for c in CHECKOUTS {
        lay_checkout(&home.join(c))?;
    }
    std::fs::create_dir_all(home.join("idle/target/doc/lay")).map_err(|e| format!("doc: {e}"))?;
    std::fs::write(home.join("idle/target/doc/lay/index.html"), b"<html>")
        .map_err(|e| format!("doc: {e}"))?;
    fill(
        &home.join("idle/target/criterion/bench/base/estimates.json"),
        4096,
    )?;
    let mirror = home
        .join("Library/Caches/aterm/cargo-build.noindex")
        .join(home.strip_prefix("/").unwrap_or(&home))
        .join("gone/ws/.cargo-build");
    lay_target(&mirror)?;
    std::fs::create_dir_all(home.join(".local/bin")).map_err(|e| format!("bin: {e}"))?;
    std::os::unix::fs::symlink(
        home.join("linked/target/debug/tool"),
        home.join(".local/bin/tool"),
    )
    .map_err(|e| format!("link: {e}"))?;
    backdate(&home, IDLE_DAYS)?;
    // Written today: the git index of one checkout, a source file of another.
    touch_now(&home.join("recent-index/.git/index"))?;
    touch_now(&home.join("edited/src/main.rs"))?;
    // The reserve, laid as the watch lays it (it writes its own only far
    // above the floor, which an 8 GiB volume never is).
    fill(
        &home.join("Library/Caches/aterm/disk-reserve.noindex/reserve"),
        256 << 20,
    )?;
    // A build holding the locked checkout's lock: this process.
    let lock = std::fs::File::open(home.join("locked/target/debug/.cargo-lock"))
        .map_err(|e| format!("lock: {e}"))?;
    #[expect(
        clippy::disallowed_methods,
        reason = "the lane's build holds the lock for its life"
    )]
    let held = lock.try_lock();
    held.map_err(|e| format!("cannot take the lock: {e:?}"))?;
    // The ballast: to about 4.5 GiB free.
    let free = free_on(&home).ok_or("the image's free space could not be read")?;
    let target = (9 << 30) / 2;
    if free > target {
        fill(&home.join("ballast/one"), free - target)?;
    }
    println!(
        "disk-lane: fixtures laid; {} MiB free",
        free_on(&home).unwrap_or(0) >> 20
    );

    // The instance.
    let mut inst = Instance::start(
        &world,
        Launch {
            aterm,
            headless: true,
            args: vec![
                "--columns".into(),
                "110".into(),
                "--lines".into(),
                "32".into(),
            ],
            env: world.env(),
            cwd: home.clone(),
            sandbox: None,
        },
    )?;
    inst.await_socket(BOOT)?;
    if inst.ctl(&["ready"]).code != 0 {
        return Err(Fail::die("disk-lane: the instance is not ready"));
    }
    let mut v = Verdict::default();
    let verdict = lane(&inst, &home, &mirror, &mut v);
    inst.stop();
    drop(lock);
    drop(image);
    if let Err(why) = verdict {
        v.check("the lane ran to its end", false, || why);
    }
    if v.passed() {
        println!("disk-lane: PASS ({} checks)", v.rows.len());
        Ok(0)
    } else {
        println!("disk-lane: FAIL");
        Ok(1)
    }
}

/// Steps 5 and 6 (the module docs) against the running `inst`.
fn lane(inst: &Instance, home: &Path, mirror: &Path, v: &mut Verdict) -> Result<(), String> {
    let ls = inst.ctl(&["ls"]);
    // `ls` rows: `<window> <tab> <sid> …`; the session id is the `s-` word.
    let sid = ls
        .stdout
        .split_whitespace()
        .find(|w| w.starts_with("s-"))
        .map(str::to_owned)
        .ok_or_else(|| format!("no session in `ls`: {}", ls.stdout))?;
    let at = format!("@{sid}");
    // The shell names its working directory (OSC 7), as an integrated shell does.
    let osc = "printf '\\033]7;file://localhost%s\\007' \"$PWD\"\n".to_string();
    let _ = inst.ctl_stdin(&[&at, "send", "--stdin"], osc.as_bytes());
    // The board maps the session at the watch's next look: a demand-free wait
    // for the volume's own level (one minute at most) needs no session row.
    let deadline = Instant::now() + MUST;
    let mut status = String::new();
    while Instant::now() < deadline {
        status = inst.ctl(&[&at, "status"]).stdout;
        if field(&status, "disk").is_some_and(|d| d != "-") {
            break;
        }
        stop::sleep(Duration::from_millis(250)).map_err(|_| "interrupted".to_owned())?;
    }
    v.check(
        "status disk= reads short",
        field(&status, "disk") == Some("short"),
        || {
            format!(
                "disk={:?} (status: {})",
                field(&status, "disk"),
                status.trim()
            )
        },
    );
    let vol = home.to_string_lossy().into_owned();
    let vol_arg = format!("vol={}", aterm_control_pct(&vol));
    let reply = inst.ctl(&[&at, "await", "sys", "free>=5GiB", &vol_arg, "timeout=60000"]);
    println!(
        "  await sys free>=5GiB -> exit {}: {}{}",
        reply.code,
        reply.stdout.trim(),
        reply.stderr.trim()
    );
    v.check(
        "await sys free>=5GiB answers OK sys (exit 0)",
        reply.code == 0 && reply.stdout.starts_with("OK sys "),
        || {
            format!(
                "exit {}: {}{}",
                reply.code,
                reply.stdout.trim(),
                reply.stderr.trim()
            )
        },
    );
    let after = inst.ctl(&[&at, "status"]).stdout;
    v.check(
        "status after reads low or ok",
        matches!(field(&after, "disk"), Some("low" | "ok")),
        || format!("disk={:?}", field(&after, "disk")),
    );
    // The journal.
    let ledger = home
        .join("Library/Application Support/aterm/harness")
        .join(aterm_agent::harness::cli::HARNESS)
        .join("disk.jsonl");
    let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
    let removed: Vec<(u64, String)> = rows
        .lines()
        .filter_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
        .filter(|r| r["kind"].as_str() == Some("removed"))
        .map(|r| {
            (
                r["tier"].as_u64().unwrap_or(0),
                r["path"].as_str().unwrap_or("").to_owned(),
            )
        })
        .collect();
    let tiers: Vec<u64> = removed.iter().map(|(t, _)| *t).collect();
    for (t, p) in &removed {
        println!("  removed tier={t} {p}");
    }
    v.check(
        "the journal's removals run in tier order",
        !tiers.is_empty() && tiers.windows(2).all(|w| w[0] <= w[1]),
        || format!("{removed:?}"),
    );
    v.check(
        "tier 1 took the removed workspace's build cache whole",
        !mirror.exists()
            && removed
                .iter()
                .any(|(t, p)| *t == 1 && p.ends_with("gone/ws/.cargo-build")),
        || format!("exists={} {removed:?}", mirror.exists()),
    );
    v.check(
        "tier 3 emptied idle/'s profile and kept it with its lock files",
        removed
            .iter()
            .any(|(t, p)| *t == 3 && p.ends_with("idle/target"))
            && home.join("idle/target/debug/.cargo-lock").exists()
            && !home.join("idle/target/debug/deps").exists(),
        || format!("{removed:?}"),
    );
    v.check(
        "doc/ and criterion/ at idle/target's root untouched",
        home.join("idle/target/doc/lay/index.html").exists()
            && home
                .join("idle/target/criterion/bench/base/estimates.json")
                .exists(),
        String::new,
    );
    for c in ["recent-index", "edited", "locked", "linked"] {
        v.check(
            &format!("{c}/ keeps its build (deps/ whole)"),
            home.join(c).join("target/debug/deps/liblay.rlib").exists(),
            String::new,
        );
    }
    v.check(
        "the locked checkout keeps its incremental/ (a build holds its lock)",
        home.join("locked/target/debug/incremental/lay-1/s-1/query-cache.bin")
            .exists(),
        String::new,
    );
    // 6. Past the emergency: the reserve goes first.
    let free = free_on(home).ok_or("free space could not be read")?;
    let target = 3 << 30;
    if free > target {
        fill(&home.join("ballast/two"), free - target)?;
    }
    let lines_before = rows.lines().count();
    let past = inst.ctl(&[&at, "await", "sys", "free>=4GiB", &vol_arg, "timeout=60000"]);
    println!(
        "  past the emergency, await sys free>=4GiB -> exit {}: {}{}",
        past.code,
        past.stdout.trim(),
        past.stderr.trim()
    );
    let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
    let fresh: Vec<aterm_json::Value> = rows
        .lines()
        .skip(lines_before)
        .filter_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
        .collect();
    let first_reserve = fresh.iter().position(|r| {
        r["kind"].as_str() == Some("reserve") && r["action"].as_str() == Some("released")
    });
    let first_removal = fresh
        .iter()
        .position(|r| matches!(r["kind"].as_str(), Some("removing" | "removed")));
    v.check(
        "at the emergency the reserve is released before anything else of the pass",
        first_reserve.is_some() && first_removal.is_none_or(|r| first_reserve < Some(r)),
        || format!("reserve at {first_reserve:?}, first removal at {first_removal:?}"),
    );
    Ok(())
}

/// A path as one wire token (`%XX` for a space or a `%`).
fn aterm_control_pct(s: &str) -> String {
    s.replace('%', "%25").replace(' ', "%20")
}
