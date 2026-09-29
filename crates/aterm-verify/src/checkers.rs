// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE SPEC CHECKERS THE TESTS RUN, NAMED ONCE PER RUN.
//!
//! WHY (2026-09-26). The test stage runs `aterm-spec`'s escalation tier, and
//! that tier executes three Trust programs of its own — `ty`, `trust-ir` and
//! `ay` — which every test process finds for itself through
//! `aterm_spec::verify`'s discovery. The run's identity named the compiler
//! only (`targo`, `trustc`, `trustdoc`, `tippy`:
//! [`crate::identity::ToolchainIdentity`]) and the receipt named no toolchain
//! at all. So when two contract runs went red on 2026-09-24 because a June
//! `ty` in a `$HOME/trust` build tree shadowed the store's (`verify-9733.log`:
//! `NativeConfigTransaction` — the interpreter walked 1712 states, `ty`
//! reported 1662), nothing the gate wrote said which checker had answered;
//! only a failing test's panic did. Discovery has since lost those tiers. What
//! was still missing is the record.
//!
//! WHAT THIS DOES. Before any stage runs, the gate finds each checker the way
//! the tests will — THE SAME ORDER as `aterm_spec::verify::find_trust_bin`:
//! the atpkg store's shim `<default prefix>/bin/<tool>` (a symlink, or the
//! `sh` exec stub atpkg lays; a tombstone and a pending-program stub are
//! absent), then the first `<tool>` on the PATH the children get — and asks
//! each one found `--version`, bounded at [`VERSION_DEADLINE`]. The ladder's
//! `verify: checkers …` line names them ([`Checkers::header_line`]), the
//! receipt records them ([`crate::receipt::Receipt::checkers`]), and the
//! tripwire resolves them again before every stage that builds or drives
//! something ([`Checkers::moved`]): a checker that moves mid-run — an `aterm
//! pkg update` re-pointing a shim, a binary rewritten in place — makes the run
//! COULD NOT RUN exactly as a compiler that moves does, because some of its
//! tests would have run under one `ty` and some under another.
//!
//! A MIRROR, NOT A DEPENDENCY. This crate has no dependencies by charter (its
//! `Cargo.toml`), so the order and the shim rule are copied from
//! `crates/aterm-spec/src/verify.rs` (`find_trust_bin`, `resolve_store_shim`,
//! `parse_exec_stub_target`, `is_pending_stub`), and each site names the
//! other: if either changes, change both. The prefix is atpkg's platform
//! DEFAULT, as `aterm-spec` reads it — not the configured prefix
//! [`crate::toolchain::atpkg_prefix`] honours — because the record is of what
//! the tests execute.
//!
//! Re-resolving is `stat`s and one small read per shim; no version is asked
//! again. What is compared is the FILE a shim forwards to, never the shim, so
//! atpkg laying an identical shim again is no move.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::identity::FileStamp;

/// The three programs, in the order the ladder names them.
pub const NAMES: [&str; 3] = ["ty", "trust-ir", "ay"];

/// How long one `--version` may take: `aterm-spec`'s own bound. A checker that
/// does not answer in time is recorded without a version, never waited for.
/// Every run asks under it, the gate's own fixtures too: a checker they have
/// just written is run once before the run, so its first exec — which waits
/// on macOS's assessment of the new file — is paid outside this bound
/// (`tests/common`'s `run_once`, the review of 2026-09-28).
pub const VERSION_DEADLINE: Duration = Duration::from_secs(5);

/// The size bound on a shim this will read — atpkg's `MAX_SHIM_BYTES`, as
/// `aterm-spec` mirrors it.
const MAX_SHIM_BYTES: u64 = 64 * 1024;

/// How much of a PATH candidate is read for the pending-stub marker.
const PENDING_STUB_HEAD: u64 = 256;

/// The self-identifying line atpkg writes into every pending-program stub.
const PENDING_STUB_MARKER: &[u8] = b"atpkg pending-program stub";

/// Which discovery tier answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// The atpkg store, through its `<prefix>/bin/<tool>` shim.
    Store,
    /// `<tool>` on the children's PATH.
    Path,
}

impl Origin {
    /// How a `checkers` line names the tier ([`Checkers::summary`]).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Store => "atpkg store",
            Self::Path => "PATH",
        }
    }
}

/// One checker as found: the file a test process would execute.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// A store shim's RESOLVED target, or the PATH entry.
    pub path: PathBuf,
    pub origin: Origin,
    /// The file's stamp at capture (`None` if it could not be read).
    pub stamp: Option<FileStamp>,
}

/// One checker: where it was found, if anywhere, and what it said it was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checker {
    pub name: &'static str,
    pub found: Option<Found>,
    /// The first line `--version` printed, when it answered in time.
    pub version: Option<String>,
}

/// The run's checkers, and what they were resolved against so the tripwire
/// can resolve them again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkers {
    /// `<default atpkg prefix>/bin`, where the shims live.
    pub store_bin: PathBuf,
    /// The PATH the children get.
    pub path_env: OsString,
    /// [`NAMES`], in order.
    pub each: Vec<Checker>,
}

impl Checkers {
    /// Resolve the three under `home` and `path_env`, and ask each one found
    /// for its version within [`VERSION_DEADLINE`].
    #[must_use]
    pub fn capture(home: &Path, path_env: &OsStr) -> Self {
        Self::capture_within(home, path_env, VERSION_DEADLINE)
    }

    /// [`Self::capture`], each `--version` bounded at `deadline` instead:
    /// [`VERSION_DEADLINE`] for every run, a shorter one where a test pins
    /// the bound itself.
    #[must_use]
    fn capture_within(home: &Path, path_env: &OsStr, deadline: Duration) -> Self {
        let store_bin = store_bin_dir(home);
        let each = NAMES
            .iter()
            .map(|&name| {
                let found = locate(name, &store_bin, path_env);
                let version = found
                    .as_ref()
                    .and_then(|f| version_of(&f.path, path_env, deadline));
                Checker {
                    name,
                    found,
                    version,
                }
            })
            .collect();
        Self {
            store_bin,
            path_env: path_env.to_os_string(),
            each,
        }
    }

    /// `ty = <path> (atpkg store, ty 0.15.0); trust-ir = …; ay = absent` — the
    /// ladder line's body and the receipt's `checkers` value. One line: a
    /// newline in a path is written as a space.
    #[must_use]
    pub fn summary(&self) -> String {
        self.each
            .iter()
            .map(|c| match &c.found {
                None => format!("{} = absent", c.name),
                Some(f) => format!(
                    "{} = {} ({}, {})",
                    c.name,
                    f.path.display(),
                    f.origin.label(),
                    c.version.as_deref().unwrap_or("no --version answer")
                ),
            })
            .collect::<Vec<_>>()
            .join("; ")
            .replace(['\n', '\r'], " ")
    }

    /// The `verify: checkers …` header line.
    #[must_use]
    pub fn header_line(&self) -> String {
        format!(
            "verify: checkers {} — as the spec tests find them: the atpkg store's shim, then PATH\n",
            self.summary()
        )
    }

    /// What moved since capture, in words: each checker is resolved again
    /// (never asked its version again) and compared by the file it resolves
    /// to and that file's stamp. `None` when nothing moved.
    #[must_use]
    pub fn moved(&self) -> Option<String> {
        let moved: Vec<String> = self
            .each
            .iter()
            .filter_map(|c| {
                let now = locate(c.name, &self.store_bin, &self.path_env);
                match (&c.found, &now) {
                    (None, None) => None,
                    (Some(then), Some(now)) if then.path == now.path && then.stamp == now.stamp => {
                        None
                    }
                    (None, Some(now)) => Some(format!(
                        "checker {} appeared at {}",
                        c.name,
                        now.path.display()
                    )),
                    (Some(then), None) => Some(format!(
                        "checker {} ({}) vanished",
                        c.name,
                        then.path.display()
                    )),
                    (Some(then), Some(now)) if then.path != now.path => Some(format!(
                        "checker {} moved: {} -> {}",
                        c.name,
                        then.path.display(),
                        now.path.display()
                    )),
                    (Some(then), Some(_)) => Some(format!(
                        "checker {} ({}) was rewritten or replaced",
                        c.name,
                        then.path.display()
                    )),
                }
            })
            .collect();
        (!moved.is_empty()).then(|| moved.join("; "))
    }
}

/// `<default atpkg prefix>/bin` under `home` — where `aterm-spec` looks for
/// the shims (its `atpkg_store_bin_dir`, Unix half).
#[must_use]
pub fn store_bin_dir(home: &Path) -> PathBuf {
    crate::toolchain::default_atpkg_prefix(home).join("bin")
}

/// THE ORDER — the mirror of `aterm_spec::verify::find_trust_bin`: the store
/// shim when it resolves to a real file, else the first `name` on `path_env`
/// that is a file and not a pending-program stub.
#[must_use]
pub fn locate(name: &str, store_bin: &Path, path_env: &OsStr) -> Option<Found> {
    let exe = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let (path, origin) = if let Some(p) = resolve_store_shim(&store_bin.join(&exe)) {
        (p, Origin::Store)
    } else {
        let p = std::env::split_paths(path_env)
            .map(|dir| dir.join(&exe))
            .find(|cand| cand.is_file() && !is_pending_stub(cand))?;
        (p, Origin::Path)
    };
    let stamp = FileStamp::of(&path);
    Some(Found {
        path,
        origin,
        stamp,
    })
}

/// What a store shim forwards to — the mirror of
/// `aterm_spec::verify::resolve_store_shim`: a symlink, or a regular file of at
/// most [`MAX_SHIM_BYTES`] whose first `exec '<absolute path>' "$@"` line names
/// the target; the target must canonicalize to a regular file. A tombstone
/// (no `exec` line), a pending stub (`exec "$ATPKG"`), a directory or a
/// dangling link is `None`.
#[must_use]
pub fn resolve_store_shim(shim: &Path) -> Option<PathBuf> {
    let meta = std::fs::symlink_metadata(shim).ok()?;
    let target = if meta.file_type().is_symlink() {
        shim.to_path_buf()
    } else if meta.file_type().is_file() && meta.len() <= MAX_SHIM_BYTES {
        let target = exec_stub_target(&std::fs::read_to_string(shim).ok()?)?;
        if !target.is_absolute() {
            return None;
        }
        target
    } else {
        return None;
    };
    let resolved = std::fs::canonicalize(target).ok()?;
    resolved.is_file().then_some(resolved)
}

/// The first trimmed `exec '<path>' "$@"` line's path, `'\''` unquoted — the
/// mirror of `aterm_spec::verify::parse_exec_stub_target`.
fn exec_stub_target(content: &str) -> Option<PathBuf> {
    content.lines().map(str::trim).find_map(|line| {
        let rest = line.strip_prefix("exec '")?;
        let end = rest.rfind("' \"$@\"")?;
        Some(PathBuf::from(rest[..end].replace("'\\''", "'")))
    })
}

/// Does `cand` carry atpkg's pending-program marker in its head? The mirror of
/// `aterm_spec::verify::is_pending_stub`.
fn is_pending_stub(cand: &Path) -> bool {
    use std::io::Read as _;
    let Ok(file) = std::fs::File::open(cand) else {
        return false;
    };
    let mut head = Vec::new();
    if file.take(PENDING_STUB_HEAD).read_to_end(&mut head).is_err() {
        return false;
    }
    head.windows(PENDING_STUB_MARKER.len())
        .any(|w| w == PENDING_STUB_MARKER)
}

/// The first non-empty line `bin --version` printed (stdout, else stderr),
/// at most 120 characters, or `None` when it could not run, failed, or did
/// not answer within `deadline` ([`VERSION_DEADLINE`] in every run).
fn version_of(bin: &Path, path_env: &OsStr, deadline: Duration) -> Option<String> {
    let mut cmd = Command::new(bin);
    cmd.arg("--version").env("PATH", path_env);
    let out = crate::disk::output_within(cmd, deadline).ok()?;
    if !out.status.success() {
        return None;
    }
    let first = |bytes: &[u8]| {
        String::from_utf8_lossy(bytes)
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(|l| l.chars().take(120).collect::<String>())
    };
    first(&out.stdout).or_else(|| first(&out.stderr))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn script(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
        std::fs::write(path, format!("#!/bin/sh\n{body}\n")).expect("write");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    /// Run `path --version` once, unbounded, output discarded (`aterm-cli`'s
    /// `manual.rs` `run_once`): the FIRST exec of a file this process wrote
    /// waits on macOS's assessment of it (tens of seconds with the assessor
    /// busy) and later execs do not, so paying it here keeps it out of the
    /// [`VERSION_DEADLINE`] a capture asks the same file under.
    fn run_once(path: &Path) {
        let _ = std::process::Command::new(path)
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }

    /// The `sh` exec stub atpkg lays, forwarding to `target`.
    fn stub(target: &Path) -> String {
        format!(
            "#!/bin/sh\n# atpkg shim\nexec '{}' \"$@\"\n",
            target.display()
        )
    }

    /// Replace `path` by rename, so it is a new inode, as atpkg's writers do.
    fn relay(path: &Path, body: &str) {
        let tmp = path.with_extension("new");
        std::fs::write(&tmp, body).expect("write");
        std::fs::rename(&tmp, path).expect("rename");
    }

    /// THE ORDER, as the tests find them: a store shim that resolves beats a
    /// file on PATH; a tombstone shim is absent, so PATH answers; a pending
    /// stub on PATH is absent too; a symlink shim resolves to its target.
    #[cfg(unix)]
    #[test]
    fn the_store_shim_outranks_path_and_a_tombstone_or_pending_stub_is_no_checker() {
        let tmp = crate::mktemp_dir("atv-checkers-order").expect("mktemp");
        let home = tmp.join("home");
        let store_bin = store_bin_dir(&home);
        let on_path = tmp.join("path");
        let ty = home.join("store/ty/3007/bin/ty");
        script(&ty, "echo 'ty 0.15.0'");
        script(&on_path.join("ty"), "echo 'ty 0.0.1 (path)'");
        std::fs::create_dir_all(&store_bin).expect("mkdir");
        std::fs::write(store_bin.join("ty"), stub(&ty)).expect("stub");
        // A tombstone: a failing notice script with no exec line.
        std::fs::write(
            store_bin.join("trust-ir"),
            "#!/bin/sh\necho 'trust-ir was yanked' >&2\nexit 1\n",
        )
        .expect("tombstone");
        script(&on_path.join("trust-ir"), "echo 'trust-ir 0.14.0'");
        // ay: only a pending-program stub on PATH.
        script(
            &on_path.join("ay"),
            "# atpkg pending-program stub\nexec \"$ATPKG\" run ay \"$@\"",
        );
        let path_env = OsString::from(on_path.as_os_str());
        // Each stand-in the capture asks its version, run once first: the
        // capture bounds each at VERSION_DEADLINE, and a first exec slower
        // than that read `None` where this asserts the version (the review of
        // 2026-09-28, reproduced with a 6 s first exec).
        for asked in [&ty, &on_path.join("trust-ir")] {
            run_once(asked);
        }

        let c = Checkers::capture(&home, &path_env);
        let ty_found = c.each[0].found.as_ref().expect("ty");
        assert_eq!(ty_found.origin, Origin::Store);
        assert_eq!(ty_found.path, ty.canonicalize().expect("canonical"));
        assert_eq!(c.each[0].version.as_deref(), Some("ty 0.15.0"));
        let ir = c.each[1].found.as_ref().expect("trust-ir");
        assert_eq!(ir.origin, Origin::Path, "a tombstone shim is no checker");
        assert_eq!(ir.path, on_path.join("trust-ir"));
        assert_eq!(c.each[2].found, None, "a pending stub is no checker");
        let line = c.header_line();
        assert!(line.starts_with("verify: checkers ty = "), "{line}");
        assert!(line.contains("(atpkg store, ty 0.15.0)"), "{line}");
        assert!(line.contains("(PATH, trust-ir 0.14.0)"), "{line}");
        assert!(line.contains("; ay = absent"), "{line}");
        assert!(!c.summary().contains('\n'));

        // A symlink shim resolves to its target, like the stub.
        std::fs::remove_file(store_bin.join("ty")).expect("rm");
        std::os::unix::fs::symlink(&ty, store_bin.join("ty")).expect("symlink");
        let again = locate("ty", &store_bin, &path_env).expect("ty");
        assert_eq!(
            (again.origin, again.path),
            (Origin::Store, ty.canonicalize().expect("canonical"))
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A MOVE IS WHAT A TEST WOULD EXECUTE CHANGING. A shim re-pointed at
    /// another build, a checker rewritten in place, one that vanishes and one
    /// that appears are each named; atpkg laying the SAME shim again (a new
    /// inode, the same target) is not a move — the negative control that keeps
    /// a routine repair pass from tripping a run.
    #[cfg(unix)]
    #[test]
    fn a_repointed_shim_or_a_rewritten_checker_moves_and_an_identical_shim_does_not() {
        let tmp = crate::mktemp_dir("atv-checkers-moved").expect("mktemp");
        let home = tmp.join("home");
        let store_bin = store_bin_dir(&home);
        let (old, new) = (
            home.join("store/ty/3007/bin/ty"),
            home.join("store/ty/3008/bin/ty"),
        );
        script(&old, "echo 'ty 0.15.0'");
        script(&new, "echo 'ty 0.16.0'");
        let ay = home.join("store/ay/1/bin/ay");
        script(&ay, "echo 'ay 1'");
        std::fs::create_dir_all(&store_bin).expect("mkdir");
        std::fs::write(store_bin.join("ty"), stub(&old)).expect("stub");
        std::fs::write(store_bin.join("ay"), stub(&ay)).expect("stub");
        let path_env = OsString::new();
        let c = Checkers::capture(&home, &path_env);
        assert_eq!(c.moved(), None);

        relay(&store_bin.join("ty"), &stub(&old));
        assert_eq!(c.moved(), None, "an identical shim laid again is no move");

        relay(&store_bin.join("ty"), &stub(&new));
        let m = c.moved().expect("re-pointed");
        assert!(m.contains("checker ty moved: "), "{m}");
        assert!(m.contains("3007") && m.contains("3008"), "{m}");
        relay(&store_bin.join("ty"), &stub(&old));
        assert_eq!(c.moved(), None, "pointed back: the same file again");

        relay(&old, "#!/bin/sh\necho 'ty 0.15.0 rebuilt'\n");
        let m = c.moved().expect("rewritten");
        assert!(
            m.contains("checker ty (") && m.contains("rewritten or replaced"),
            "{m}"
        );

        std::fs::remove_file(&ay).expect("rm");
        let m = c.moved().expect("vanished");
        assert!(m.contains("checker ay (") && m.contains("vanished"), "{m}");

        let ir = home.join("store/trust-ir/1/bin/trust-ir");
        script(&ir, "echo 'trust-ir 1'");
        std::fs::write(store_bin.join("trust-ir"), stub(&ir)).expect("stub");
        let m = c.moved().expect("appeared");
        assert!(m.contains("checker trust-ir appeared at "), "{m}");
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A checker that answers nothing is recorded as found, without a version.
    #[cfg(unix)]
    #[test]
    fn a_checker_that_fails_its_version_query_is_named_without_one() {
        let tmp = crate::mktemp_dir("atv-checkers-version").expect("mktemp");
        let on_path = tmp.join("path");
        script(&on_path.join("ty"), "exit 1");
        let c = Checkers::capture(&tmp.join("home"), on_path.as_os_str());
        assert!(c.each[0].found.is_some());
        assert_eq!(c.each[0].version, None);
        assert!(
            c.summary().contains("(PATH, no --version answer)"),
            "{}",
            c.summary()
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// THE DEADLINE BOUNDS THE QUESTION. A checker slower than the deadline it
    /// is asked under is named without a version, never waited for; here the
    /// deadline is shorter than [`VERSION_DEADLINE`], so the test is quick.
    /// The stand-in sleeps whatever its exec costs, so a slow start only
    /// makes it later. (By its absolute path: the checker runs with the PATH
    /// it was found on, which holds nothing but itself.)
    #[cfg(unix)]
    #[test]
    fn a_checker_slower_than_its_deadline_is_named_without_a_version() {
        let tmp = crate::mktemp_dir("atv-checkers-deadline").expect("mktemp");
        let on_path = tmp.join("path");
        script(&on_path.join("ty"), "/bin/sleep 2; echo 'ty late'");
        let c = Checkers::capture_within(
            &tmp.join("home"),
            on_path.as_os_str(),
            Duration::from_millis(300),
        );
        assert!(c.each[0].found.is_some(), "{}", c.summary());
        assert_eq!(c.each[0].version, None, "{}", c.summary());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn the_exec_stub_target_is_the_first_exec_line_unquoted() {
        assert_eq!(
            exec_stub_target("#!/bin/sh\nexec '/s/it'\\''s/ty' \"$@\"\n"),
            Some(PathBuf::from("/s/it's/ty"))
        );
        assert_eq!(
            exec_stub_target("#!/bin/sh\nexec \"$ATPKG\" run ty\n"),
            None
        );
        assert_eq!(exec_stub_target("#!/bin/sh\nexit 1\n"), None);
    }
}
