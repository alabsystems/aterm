// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! The `brew` reroute stub (design §1): one `/bin/sh` file in the
//! session reroute directory beside the cargo/rustc stubs, with its
//! own marker so `reroute::lay` treats it as foreign (never swept,
//! never rewritten). The escape walk runs in `sh`, so `brew --real`
//! works even when atpkg cannot.

use std::path::{Path, PathBuf};

use super::prefix::Prefix;
use crate::store::Layout;

/// First-arg escape flag: `brew --real …` runs real Homebrew.
pub const REAL_FLAG: &str = "--real";
/// Env escape: engaged (non-empty, not `0`) means run real Homebrew.
pub const REAL_ENV: &str = "ABREW_REAL";
/// Pin the real brew's path (ops/tests). Absolute + executable or ignored.
pub const BREW_PATH_ENV: &str = "ABREW_BREW_PATH";
/// Marker set on children of a rerouted run, so nothing loops.
pub const REROUTED_ENV: &str = "__ABREW_REROUTED";
/// The hidden verb the stub execs: `atpkg __abrew [args…]`.
pub const HIDDEN_VERB: &str = "__abrew";
/// The stub's own identity line (never the reroute table's marker).
const STUB_MARKER: &str = "# atpkg-abrew stub v1 — regenerable; brew --real runs real Homebrew";

/// The stub file for a layout.
#[must_use]
pub fn stub_path(layout: &Layout) -> PathBuf {
    crate::reroute::dir(layout).join("brew")
}

/// The stub body. `atpkg` is the absolute binary path to name;
/// `disabled_file` and `bundled_brew` are baked in at lay time.
#[must_use]
pub fn stub_body(atpkg: &Path, disabled_file: &Path, bundled_brew: &Path) -> String {
    format!(
        "#!/bin/sh\n{STUB_MARKER}\nATPKG='{atpkg}'\nDISABLED='{disabled}'\nBUNDLED='{bundled}'\n{S}",
        atpkg = sh_single(&atpkg.to_string_lossy()),
        disabled = sh_single(&disabled_file.to_string_lossy()),
        bundled = sh_single(&bundled_brew.to_string_lossy()),
        S = STUB_TAIL,
    )
}

/// Single-quote a string for /bin/sh (`'\''` for an embedded quote).
fn sh_single(s: &str) -> String {
    s.replace('\'', "'\\''")
}

const STUB_TAIL: &str = r#"real_brew() {
  if [ -n "${ABREW_BREW_PATH:-}" ] && [ -x "$ABREW_BREW_PATH" ]; then
    printf '%s' "$ABREW_BREW_PATH"; return 0
  fi
  if [ -x "$BUNDLED" ]; then printf '%s' "$BUNDLED"; return 0; fi
  _oldifs=$IFS; IFS=:
  for _d in $PATH; do
    IFS=$_oldifs
    [ -n "$_d" ] || _d=.
    [ -e "$_d/.atpkg-reroute-dir" ] && continue
    if [ -x "$_d/brew" ]; then printf '%s' "$_d/brew"; return 0; fi
    IFS=:
  done
  IFS=$_oldifs
  for _d in /opt/homebrew/bin /usr/local/bin "$HOME/.linuxbrew/bin"; do
    if [ -x "$_d/brew" ]; then printf '%s' "$_d/brew"; return 0; fi
  done
  return 1
}
run_real() {
  _b=$(real_brew) || { echo "abrew: no real Homebrew found (lay one with: abrew bundle)" >&2; exit 127; }
  __ABREW_REROUTED=1; export __ABREW_REROUTED
  exec "$_b" "$@"
}
case "${ABREW_REAL:-}" in ''|0) ;; *) run_real "$@" ;; esac
case "${__ABREW_REROUTED:-}" in ''|0) ;; *) run_real "$@" ;; esac
case "${__ATERM_REROUTE_PASSTHROUGH:-}" in ''|0) ;; *) run_real "$@" ;; esac
if [ "${1:-}" = "--real" ]; then shift; run_real "$@"; fi
if [ -e "$DISABLED" ]; then run_real "$@"; fi
if [ -x "$ATPKG" ]; then
  __ABREW_REROUTED=1; export __ABREW_REROUTED
  exec "$ATPKG" __abrew "$@"
fi
echo "abrew: atpkg unreachable; running real brew" >&2
run_real "$@"
"#;

/// Whether the file at `path` is an abrew stub (by its marker line).
#[must_use]
pub fn is_abrew_stub(path: &Path) -> bool {
    std::fs::read(path).is_ok_and(|b| {
        b.len() <= 16 * 1024
            && String::from_utf8_lossy(&b)
                .lines()
                .take(3)
                .any(|l| l.trim() == STUB_MARKER)
    })
}

/// Lay or refresh the stub for a layout. Never over a foreign file.
/// A disabled prefix lays nothing and removes an ours stub.
pub fn lay(layout: &Layout) -> std::io::Result<()> {
    lay_with(layout, &crate::stub::Embedder::this_process())
}

/// [`lay`] with the caller's embedder — the reroute lay's own, so the
/// brew stub follows the same standing-aware rule as every reroute
/// stub: a copy run from a scratch dir keeps the standing stub's
/// (installed) atpkg; a moved app re-points it at its own.
pub(crate) fn lay_with(layout: &Layout, embedder: &crate::stub::Embedder) -> std::io::Result<()> {
    if cfg!(windows) {
        return Ok(());
    }
    if layout.declined().is_file() {
        return Ok(());
    }
    // The prefix this layout serves (never a test's neighbour): the env
    // override when engaged, else `<layout prefix>/abrew` — the same root
    // `Prefix::resolve` computes from a configured layout in a real run.
    let prefix = match std::env::var_os(super::prefix::PREFIX_ENV) {
        Some(p) if PathBuf::from(&p).is_absolute() => Prefix::at(&PathBuf::from(p)),
        _ => Prefix::at(&layout.prefix.join("abrew")),
    };
    let path = stub_path(layout);
    if !crate::config::load_abrew().enabled() {
        if is_abrew_stub(&path) {
            let _ = std::fs::remove_file(&path);
        }
        return Ok(());
    }
    if prefix.is_disabled() {
        if is_abrew_stub(&path) {
            let _ = std::fs::remove_file(&path);
        }
        return Ok(());
    }
    let standing = is_abrew_stub(&path)
        .then(|| std::fs::read_to_string(&path).ok())
        .flatten()
        .and_then(|body| crate::stub::sh_named_atpkg(&body, "ATPKG"));
    let offered = embedder.embed(standing.as_deref());
    let atpkg = crate::stub::named(offered.as_deref());
    let body = stub_body(
        atpkg,
        &prefix.disabled_path(),
        &prefix.homebrew_bundle().join("bin/brew"),
    );
    match std::fs::symlink_metadata(&path) {
        Err(_) => {}
        Ok(_) if is_abrew_stub(&path) => {
            if std::fs::read(&path).is_ok_and(|have| have == body.as_bytes()) {
                return Ok(());
            }
        }
        Ok(_) => return Ok(()), // foreign: never touched
    }
    layout.ensure_dir(&crate::reroute::dir(layout))?;
    crate::lay::write_regenerable(&crate::lay::Executable::new(&path, body.as_bytes()))
}

/// The one doctor row for the brew stub: laid/off/foreign/missing,
/// plus where real brew resolves, and the class of what fixes it:
/// `relay` (`repair` lays the stub again), or `person` for a foreign
/// file at its path, which nothing touches. Doctor names; it never lays.
#[must_use]
pub fn doctor_row(layout: &Layout) -> (crate::doctor::Row, aterm_converge::Class) {
    let prefix = Prefix::resolve();
    let real =
        resolve_real_brew(&prefix).map_or_else(|| "none".to_string(), |p| p.display().to_string());
    doctor_row_with(
        layout,
        crate::config::load_abrew().enabled(),
        prefix.is_disabled(),
        &real,
    )
}

/// [`doctor_row`] over what it reads from the config and the abrew prefix: whether abrew
/// is `enabled`, whether `abrew off` `disabled` it, and where `real` brew resolves.
fn doctor_row_with(
    layout: &Layout,
    enabled: bool,
    disabled: bool,
    real: &str,
) -> (crate::doctor::Row, aterm_converge::Class) {
    use crate::doctor::{Level, Row};
    use aterm_converge::Class;
    let path = stub_path(layout);
    let row = |level, text| (Row { level, text }, Class::Relay);
    if !enabled {
        return row(
            Level::Note,
            format!("abrew off by [abrew] enabled=false; real brew: {real}"),
        );
    }
    if disabled {
        return row(
            Level::Note,
            format!("abrew off (`abrew on` re-enables); real brew: {real}"),
        );
    }
    if is_abrew_stub(&path) {
        // A stub naming no live atpkg degrades silently to real brew
        // on every invocation (found live: a dev binary with no
        // co-located atpkg laid ATPKG=''). Name it, don't say ok.
        let live = std::fs::read_to_string(&path).is_ok_and(|body| stub_names_live_atpkg(&body));
        if live {
            row(
                Level::Ok,
                format!("reroute stub brew laid (abrew); real brew: {real}"),
            )
        } else {
            row(
                Level::Warn,
                format!(
                    "reroute stub brew names no live atpkg, so brew runs real Homebrew; `aterm pkg repair` re-lays it; real brew: {real}"
                ),
            )
        }
    } else if path.exists() {
        (
            Row {
                level: Level::Warn,
                text: format!(
                    "{} is not ours (foreign file; never touched)",
                    path.display()
                ),
            },
            Class::Person,
        )
    } else {
        row(
            Level::Warn,
            format!(
                "reroute stub brew missing; the next aterm session lays it — now: aterm pkg repair; real brew: {real}"
            ),
        )
    }
}

/// Whether a stub body names an atpkg that is actually there and
/// executable — the same `[ -x ]` the body itself asks before its
/// hand-over, so doctor and the stub agree.
#[must_use]
pub fn stub_names_live_atpkg(body: &str) -> bool {
    crate::stub::sh_named_atpkg(body, "ATPKG").is_some_and(|p| crate::stub::is_live_atpkg(&p))
}

/// Remove an ours stub (`abrew off`, uninstall).
pub fn remove(layout: &Layout) {
    let path = stub_path(layout);
    if is_abrew_stub(&path) {
        let _ = std::fs::remove_file(path);
    }
}

/// Resolve the real Homebrew (design §1 order): `ABREW_BREW_PATH`,
/// bundled, PATH walk past reroute dirs, well-known prefixes.
#[must_use]
pub fn resolve_real_brew(prefix: &Prefix) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(BREW_PATH_ENV) {
        let p = PathBuf::from(p);
        if p.is_absolute() && is_executable(&p) {
            return Some(p);
        }
        eprintln!("abrew: ignoring {BREW_PATH_ENV} (must be an absolute executable path)");
    }
    if let Some(bundled) = super::bundle::bundled_brew(prefix) {
        return Some(bundled);
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            if dir.join(crate::reroute::DIR_MARKER_FILE).exists() {
                continue;
            }
            let cand = dir.join("brew");
            if is_executable(&cand) && !is_abrew_stub(&cand) {
                return Some(cand);
            }
        }
    }
    for cand in [
        PathBuf::from("/opt/homebrew/bin/brew"),
        PathBuf::from("/usr/local/bin/brew"),
    ] {
        if is_executable(&cand) {
            return Some(cand);
        }
    }
    aterm_types::dirs::home_dir()
        .map(|h| h.join(".linuxbrew/bin/brew"))
        .filter(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

/// Whether an env value counts as engaged (the crate's one rule).
#[must_use]
pub fn engaged(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.is_empty() && v != "0")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Doctor's row for the stub's path, and the class of what fixes it: a missing stub is
    /// `repair`'s to lay again (`relay`); a foreign file there is the person's, never
    /// touched (`person`).
    #[test]
    fn a_foreign_file_at_the_stub_s_path_is_the_person_s() {
        use crate::doctor::Level;
        use aterm_converge::Class;
        let l = crate::testdir::layout("atpkg-abrew-row-");
        let (row, class) = doctor_row_with(&l, true, false, "none");
        assert_eq!(
            (row.level, class),
            (Level::Warn, Class::Relay),
            "{}",
            row.text
        );
        assert!(row.text.contains("missing"), "{}", row.text);
        std::fs::create_dir_all(stub_path(&l).parent().unwrap()).unwrap();
        std::fs::write(
            stub_path(&l),
            "#!/bin/sh\nexec /opt/homebrew/bin/brew \"$@\"\n",
        )
        .unwrap();
        let (row, class) = doctor_row_with(&l, true, false, "none");
        assert_eq!(
            (row.level, class),
            (Level::Warn, Class::Person),
            "{}",
            row.text
        );
        assert!(
            row.text.contains("foreign file; never touched"),
            "{}",
            row.text
        );
        let (row, class) = doctor_row_with(&l, false, false, "none");
        assert_eq!(
            (row.level, class),
            (Level::Note, Class::Relay),
            "{}",
            row.text
        );
    }

    #[cfg(unix)]
    #[test]
    fn stub_execs_abrew_and_real_escape() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = std::env::temp_dir().join(format!("abrew-stub-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        // Fake atpkg: prints its argv. Fake real brew: prints REAL.
        let fake_atpkg = tmp.join("atpkg");
        std::fs::write(&fake_atpkg, "#!/bin/sh\necho ATPKG \"$@\"\n").unwrap();
        let fake_brew = tmp.join("real-brew");
        std::fs::write(&fake_brew, "#!/bin/sh\necho REAL \"$@\"\n").unwrap();
        for f in [&fake_atpkg, &fake_brew] {
            std::fs::set_permissions(f, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let stub_file = tmp.join("brew");
        let body = stub_body(
            &fake_atpkg,
            &tmp.join("disabled"),
            &tmp.join("none/bin/brew"),
        );
        std::fs::write(&stub_file, body).unwrap();
        std::fs::set_permissions(&stub_file, std::fs::Permissions::from_mode(0o755)).unwrap();

        let run = |args: &[&str], real_env: bool| -> String {
            let stub_s = stub_file.to_string_lossy().into_owned();
            let mut cmd = aterm_uds::spawnfd::command("/bin/sh");
            cmd.arg(stub_s.as_str()).args(args);
            cmd.env("ABREW_BREW_PATH", fake_brew.as_os_str());
            if real_env {
                cmd.env("ABREW_REAL", "1");
            } else {
                cmd.env_remove("ABREW_REAL");
            }
            cmd.env_remove("__ABREW_REROUTED");
            cmd.env_remove("__ATERM_REROUTE_PASSTHROUGH");
            let out = cmd.output().unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        assert_eq!(run(&["list"], false), "ATPKG __abrew list");
        assert_eq!(run(&["--real", "list"], false), "REAL list");
        assert_eq!(run(&["list"], true), "REAL list");
        // Disabled file flips to real with no flag at all.
        std::fs::write(tmp.join("disabled"), "off\n").unwrap();
        assert_eq!(run(&["info", "wget"], false), "REAL info wget");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(unix)]
    #[test]
    fn stub_names_live_atpkg_rejects_empty_and_missing() {
        use std::os::unix::fs::PermissionsExt;
        let owned = aterm_tempfile::TempDir::new().expect("private stub fixture");
        let tmp = owned.path();
        let fake = tmp.join("atpkg");
        std::fs::write(&fake, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let body =
            |atpkg: &Path| stub_body(atpkg, &tmp.join("disabled"), &tmp.join("none/bin/brew"));
        assert!(stub_names_live_atpkg(&body(&fake)));
        // The ATPKG='' lay a dev binary without a co-located atpkg
        // produces: doctor must warn, never say ok.
        assert!(!stub_names_live_atpkg(&body(Path::new(""))));
        assert!(!stub_names_live_atpkg(&body(&tmp.join("gone/atpkg"))));
        // A non-executable file is not live either.
        let dull = tmp.join("dull");
        std::fs::write(&dull, "not a program\n").unwrap();
        std::fs::set_permissions(&dull, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!stub_names_live_atpkg(&body(&dull)));
    }

    #[test]
    fn stub_body_carries_escapes_and_marker() {
        let body = stub_body(
            Path::new("/opt/aterm/atpkg"),
            Path::new("/p/disabled"),
            Path::new("/p/homebrew/bin/brew"),
        );
        assert!(body.contains(STUB_MARKER));
        assert!(body.contains("__abrew"));
        assert!(body.contains("--real"));
        assert!(body.contains("ABREW_REAL"));
        assert!(body.contains(".atpkg-reroute-dir"));
    }
}
