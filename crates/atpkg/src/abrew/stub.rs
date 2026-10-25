// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! The `brew` reroute stub.

use std::path::{Path, PathBuf};

use super::prefix::Prefix;
use crate::store::Layout;

/// Retired escape flag, refused before a native command runs.
pub const REAL_FLAG: &str = "--real";
/// Retired escape environment; engaged values block ordinary commands, leaving `on` and `status` available.
pub const REAL_ENV: &str = "ABREW_REAL";
/// Native-only capability route: an older backend refuses this instead of entering its legacy brew handler.
pub const HIDDEN_VERB: &str = "__abrew_native";
const STUB_MARKER: &str = "# atpkg-abrew stub v2 — regenerable; native atpkg only";
const LEGACY_STUB_MARKER: &str =
    "# atpkg-abrew stub v1 — regenerable; brew --real runs real Homebrew";

/// The stub file for a layout.
#[must_use]
pub fn stub_path(layout: &Layout) -> PathBuf {
    crate::reroute::dir(layout).join("brew")
}

/// Format the native-only stub body for `atpkg`.
#[must_use]
pub fn stub_body(atpkg: &Path) -> String {
    format!(
        "#!/bin/sh\n{STUB_MARKER}\nATPKG='{atpkg}'\n{STUB_GUARDS}\
if [ -x \"$ATPKG\" ]; then\n  exec \"$ATPKG\" {HIDDEN_VERB} \"$@\"\nfi\n\
echo \"abrew: atpkg unreachable; run aterm pkg repair; real Homebrew is never a fallback\" >&2\nexit 127\n",
        atpkg = sh_single(&atpkg.to_string_lossy()),
    )
}

fn sh_single(s: &str) -> String {
    s.replace('\'', "'\\''")
}

const STUB_GUARDS: &str = r#"_ABREW_COMMAND=${1:-}
case "$_ABREW_COMMAND" in
  --verbose|-v|--debug|-d)
    case "${2:-}" in
      ''|-*) ;;
      *) _ABREW_COMMAND=$2 ;;
    esac
    ;;
esac
case "$_ABREW_COMMAND" in
  on|status) ;;
  *)
    case "${ABREW_REAL:-}" in
      ''|0) ;;
      *) echo "abrew: ABREW_REAL is unsupported; brew uses native atpkg, never real Homebrew" >&2; exit 2 ;;
    esac
    case "${__ATERM_REROUTE_PASSTHROUGH:-}" in
      ''|0) ;;
      *) echo "abrew: brew is unavailable in a --no-reroute session; real Homebrew is never a fallback" >&2; exit 2 ;;
    esac
    ;;
esac
if [ "${1:-}" = "--real" ]; then
  echo "abrew: --real is unsupported; brew uses native atpkg, never real Homebrew" >&2
  exit 2
fi
"#;

/// Whether the file at `path` is an abrew stub (by its marker line).
#[must_use]
pub fn is_abrew_stub(path: &Path) -> bool {
    std::fs::read(path).is_ok_and(|b| {
        b.len() <= 16 * 1024
            && String::from_utf8_lossy(&b)
                .lines()
                .take(3)
                .any(|l| matches!(l.trim(), STUB_MARKER | LEGACY_STUB_MARKER))
    })
}

/// Lay or refresh the stub for a layout (never over a foreign file).
pub fn lay(layout: &Layout) -> std::io::Result<()> {
    lay_with(layout, &crate::stub::Embedder::this_process())
}

/// [`lay`] with the caller's embedder — the reroute lay's own, so the
/// brew stub follows the same standing-aware rule as every reroute
/// stub: a copy run from a scratch dir keeps the standing stub's
/// (installed) atpkg; a moved app re-points it at its own.
pub(crate) fn lay_with(layout: &Layout, embedder: &crate::stub::Embedder) -> std::io::Result<()> {
    match plan(layout, embedder) {
        Plan::Nothing => Ok(()),
        Plan::Lay(path, body) => {
            layout.ensure_dir(&crate::reroute::dir(layout))?;
            crate::lay::write_regenerable(&crate::lay::Executable::new(&path, body.as_bytes()))
        }
    }
}

/// Whether [`lay_with`] would write the stub now: what `aterm pkg repair`'s
/// `base.pkg.stubs` item reads beside the reroute stubs. Changes nothing.
pub(crate) fn lay_pending(layout: &Layout, embedder: &crate::stub::Embedder) -> bool {
    !matches!(plan(layout, embedder), Plan::Nothing)
}

enum Plan {
    Nothing,
    Lay(PathBuf, String),
}

fn plan(layout: &Layout, embedder: &crate::stub::Embedder) -> Plan {
    if cfg!(windows) {
        return Plan::Nothing;
    }
    if layout.declined().is_file() {
        return Plan::Nothing;
    }
    let path = stub_path(layout);
    let standing = is_abrew_stub(&path)
        .then(|| std::fs::read_to_string(&path).ok())
        .flatten()
        .and_then(|body| crate::stub::sh_named_atpkg(&body, "ATPKG"));
    let offered = embedder.embed(standing.as_deref());
    let atpkg = crate::stub::named(offered.as_deref());
    let body = stub_body(atpkg);
    match std::fs::symlink_metadata(&path) {
        Err(_) => Plan::Lay(path, body),
        Ok(_) if is_abrew_stub(&path) => {
            if std::fs::read(&path).is_ok_and(|have| have == body.as_bytes()) {
                Plan::Nothing
            } else {
                Plan::Lay(path, body)
            }
        }
        Ok(_) => Plan::Nothing, // foreign: never touched
    }
}

/// Doctor row and convergence class for the brew stub.
#[must_use]
pub fn doctor_row(layout: &Layout) -> (crate::doctor::Row, aterm_converge::Class) {
    let prefix = Prefix::resolve();
    doctor_row_with(
        layout,
        crate::config::load_abrew().enabled(),
        prefix.is_disabled(),
    )
}

fn doctor_row_with(
    layout: &Layout,
    enabled: bool,
    disabled: bool,
) -> (crate::doctor::Row, aterm_converge::Class) {
    use crate::doctor::{Level, Row};
    use aterm_converge::Class;
    let path = stub_path(layout);
    let row = |level, text| (Row { level, text }, Class::Relay);
    if is_abrew_stub(&path)
        && std::fs::read_to_string(&path).is_ok_and(|body| {
            body.lines()
                .take(3)
                .any(|line| line.trim() == LEGACY_STUB_MARKER)
        })
    {
        return row(
            Level::Warn,
            "reroute stub brew needs native-only refresh; run aterm pkg repair".to_string(),
        );
    }
    if !enabled {
        return row(
            Level::Note,
            "abrew off by [abrew] enabled=false; brew refuses commands until re-enabled"
                .to_string(),
        );
    }
    if disabled {
        return row(
            Level::Note,
            "abrew off (`abrew on` re-enables); brew never falls back to Homebrew".to_string(),
        );
    }
    if is_abrew_stub(&path) {
        let body = std::fs::read_to_string(&path).unwrap_or_default();
        if !body.lines().take(3).any(|line| line.trim() == STUB_MARKER) {
            return row(
                Level::Warn,
                "reroute stub brew needs native-only refresh; run aterm pkg repair".to_string(),
            );
        }
        let live = stub_names_live_atpkg(&body);
        if live {
            row(
                Level::Ok,
                "reroute stub brew laid (native atpkg only)".to_string(),
            )
        } else {
            row(
                Level::Warn,
                "reroute stub brew names no live atpkg; brew refuses commands until aterm pkg repair re-lays it".to_string(),
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
            "reroute stub brew missing; the next aterm session lays it — now: aterm pkg repair"
                .to_string(),
        )
    }
}

/// Whether `body` names an executable `atpkg`.
#[must_use]
pub fn stub_names_live_atpkg(body: &str) -> bool {
    crate::stub::sh_named_atpkg(body, "ATPKG").is_some_and(|p| crate::stub::is_live_atpkg(&p))
}

/// Remove an owned stub during uninstall.
pub fn remove(layout: &Layout) {
    let path = stub_path(layout);
    if is_abrew_stub(&path) {
        let _ = std::fs::remove_file(path);
    }
}

/// Whether an env value counts as engaged.
#[must_use]
pub fn engaged(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.is_empty() && v != "0")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_foreign_file_at_the_stub_s_path_is_the_person_s() {
        use crate::doctor::Level;
        use aterm_converge::Class;
        let l = crate::testdir::layout("atpkg-abrew-row-");
        let (row, class) = doctor_row_with(&l, true, false);
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
        let (row, class) = doctor_row_with(&l, true, false);
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
        let (row, class) = doctor_row_with(&l, false, false);
        assert_eq!(
            (row.level, class),
            (Level::Note, Class::Relay),
            "{}",
            row.text
        );
    }

    #[test]
    fn legacy_brew_stub_needs_repair_even_when_native_routing_is_disabled() {
        use crate::doctor::Level;
        use aterm_converge::Class;
        let layout = crate::testdir::layout("atpkg-abrew-legacy-row-");
        let path = stub_path(&layout);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let old = format!("#!/bin/sh\n{LEGACY_STUB_MARKER}\nexec /old/atpkg __abrew \"$@\"\n");
        std::fs::write(&path, &old).unwrap();
        for (enabled, disabled) in [(true, false), (false, false), (true, true)] {
            let (row, class) = doctor_row_with(&layout, enabled, disabled);
            assert_eq!((row.level, class), (Level::Warn, Class::Relay));
            assert!(row.text.contains("native-only refresh"));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), old);
        }
    }

    #[cfg(unix)]
    #[test]
    fn stub_execs_only_native_abrew_and_refuses_real_brew_bypasses() {
        use std::os::unix::fs::PermissionsExt;
        let owned = aterm_tempfile::TempDir::new().unwrap();
        let tmp = owned.path().join("quoted ' paths");
        std::fs::create_dir_all(&tmp).unwrap();
        let fake_atpkg = tmp.join("atpkg ' backend");
        std::fs::write(
            &fake_atpkg,
            "#!/bin/sh\nprintf 'ATPKG\\n'\nprintf '<%s>\\n' \"$@\"\n",
        )
        .unwrap();
        let fake_brew = tmp.join("real-brew");
        std::fs::write(
            &fake_brew,
            "#!/bin/sh\nprintf 'called\\n' >> \"${BREW_CALLED:?}\"\nprintf 'REAL\\n'\n",
        )
        .unwrap();
        for f in [&fake_atpkg, &fake_brew] {
            std::fs::set_permissions(f, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let stub_file = tmp.join("brew");
        let disabled_file = tmp.join("disabled");
        let called = tmp.join("real-brew-called");
        let mut failures = Vec::new();
        for (label, args, env, disabled, missing, expected) in [
            (
                "native arguments",
                vec!["install", "a b", "'quoted'", "", "$(literal)"],
                None,
                false,
                false,
                Some("ATPKG\n<__abrew_native>\n<install>\n<a b>\n<'quoted'>\n<>\n<$(literal)>\n"),
            ),
            (
                "real flag",
                vec!["--real", "list"],
                None,
                false,
                false,
                None,
            ),
            (
                "real environment",
                vec!["list"],
                Some(("ABREW_REAL", "1")),
                false,
                false,
                None,
            ),
            (
                "no-reroute environment",
                vec!["list"],
                Some(("__ATERM_REROUTE_PASSTHROUGH", "1")),
                false,
                false,
                None,
            ),
            (
                "inherited child marker",
                vec!["list"],
                Some(("__ABREW_REROUTED", "1")),
                false,
                false,
                Some("ATPKG\n<__abrew_native>\n<list>\n"),
            ),
            (
                "disabled backend routing",
                vec!["list"],
                None,
                true,
                false,
                Some("ATPKG\n<__abrew_native>\n<list>\n"),
            ),
            (
                "disabled recovery",
                vec!["on"],
                None,
                true,
                false,
                Some("ATPKG\n<__abrew_native>\n<on>\n"),
            ),
            ("missing backend", vec!["list"], None, false, true, None),
            (
                "legacy env on recovery",
                vec!["on"],
                Some(("ABREW_REAL", "1")),
                false,
                false,
                Some("ATPKG\n<__abrew_native>\n<on>\n"),
            ),
            (
                "legacy env status recovery",
                vec!["status"],
                Some(("ABREW_REAL", "1")),
                false,
                false,
                Some("ATPKG\n<__abrew_native>\n<status>\n"),
            ),
            (
                "no-reroute on recovery",
                vec!["on"],
                Some(("__ATERM_REROUTE_PASSTHROUGH", "1")),
                false,
                false,
                Some("ATPKG\n<__abrew_native>\n<on>\n"),
            ),
            (
                "no-reroute status recovery",
                vec!["status"],
                Some(("__ATERM_REROUTE_PASSTHROUGH", "1")),
                false,
                false,
                Some("ATPKG\n<__abrew_native>\n<status>\n"),
            ),
        ] {
            let _ = std::fs::remove_file(&called);
            if disabled {
                std::fs::write(&disabled_file, "off\n").unwrap();
            } else {
                let _ = std::fs::remove_file(&disabled_file);
            }
            let missing_atpkg = tmp.join("missing-atpkg");
            let backend = if missing { &missing_atpkg } else { &fake_atpkg };
            std::fs::write(&stub_file, stub_body(backend)).unwrap();
            let mut cmd = aterm_uds::spawnfd::command("/bin/sh");
            cmd.arg(&stub_file).args(&args);
            cmd.env("ABREW_BREW_PATH", &fake_brew);
            cmd.env("BREW_CALLED", &called);
            cmd.env("PATH", &tmp);
            for name in [
                "ABREW_REAL",
                "__ABREW_REROUTED",
                "__ATERM_REROUTE_PASSTHROUGH",
            ] {
                cmd.env_remove(name);
            }
            if let Some((name, value)) = env {
                cmd.env(name, value);
            }
            let out = cmd.output().unwrap();
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let routed = match expected {
                Some(expected) => out.status.success() && stdout == expected,
                None => !out.status.success() && stdout.is_empty() && !stderr.is_empty(),
            };
            if !routed || called.exists() {
                failures.push((
                    label,
                    out.status.code(),
                    stdout.into_owned(),
                    stderr.into_owned(),
                    called.exists(),
                ));
            }
        }
        assert!(
            failures.is_empty(),
            "(case, exit, stdout, stderr, real brew called): {failures:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn prefixed_recovery_preserves_arguments_and_keeps_ordinary_commands_blocked() {
        use std::os::unix::fs::PermissionsExt;

        let temp = aterm_tempfile::TempDir::new().unwrap();
        let backend = temp.path().join("atpkg");
        std::fs::write(
            &backend,
            "#!/bin/sh\nprintf 'ATPKG\\n'\nprintf '<%s>\\n' \"$@\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = temp.path().join("brew");
        std::fs::write(&path, stub_body(&backend)).unwrap();
        let mut failures = Vec::new();
        for flag in ["--verbose", "-v", "--debug", "-d"] {
            for command in ["on", "status", "list"] {
                for env in ["ABREW_REAL", "__ATERM_REROUTE_PASSTHROUGH"] {
                    let mut child = aterm_uds::spawnfd::command("/bin/sh");
                    child.arg(&path).args([flag, command]);
                    for name in ["ABREW_REAL", "__ATERM_REROUTE_PASSTHROUGH"] {
                        child.env_remove(name);
                    }
                    let output = child.env(env, "1").output().unwrap();
                    let valid = if command == "list" {
                        !output.status.success()
                            && output.stdout.is_empty()
                            && !output.stderr.is_empty()
                    } else {
                        output.status.success()
                            && output.stdout
                                == format!("ATPKG\n<{HIDDEN_VERB}>\n<{flag}>\n<{command}>\n")
                                    .as_bytes()
                    };
                    if !valid {
                        failures.push(format!(
                            "{env}=1 {flag} {command}: status={:?}, stdout={:?}, stderr={:?}",
                            output.status,
                            String::from_utf8_lossy(&output.stdout),
                            String::from_utf8_lossy(&output.stderr)
                        ));
                    }
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
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
        let body = stub_body;
        assert!(stub_names_live_atpkg(&body(&fake)));
        assert!(!stub_names_live_atpkg(&body(Path::new(""))));
        assert!(!stub_names_live_atpkg(&body(&tmp.join("gone/atpkg"))));
        let dull = tmp.join("dull");
        std::fs::write(&dull, "not a program\n").unwrap();
        std::fs::set_permissions(&dull, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!stub_names_live_atpkg(&body(&dull)));
    }

    #[cfg(unix)]
    #[test]
    fn legacy_stub_refresh_does_not_call_an_old_backend_legacy_brew_route() {
        use std::os::unix::fs::PermissionsExt;

        let temp = aterm_tempfile::TempDir::new().unwrap();
        let old = temp.path().join("Legacy.app/Contents/MacOS/atpkg");
        let transient = temp.path().join("transient");
        let offered = transient.join("atpkg");
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::create_dir_all(&transient).unwrap();
        std::fs::write(
            &old,
            "#!/bin/sh\nif [ \"${1:-}\" = __abrew ]; then\n  printf 'legacy brew fallback\\n' >> \"${LEGACY_BREW_CALLED:?}\"\n  exit 0\nfi\nprintf 'old backend: unknown verb\\n' >&2\nexit 2\n",
        )
        .unwrap();
        std::fs::write(&offered, "#!/bin/sh\nprintf 'new backend\\n' >&2\nexit 1\n").unwrap();
        for backend in [&old, &offered] {
            std::fs::set_permissions(backend, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let embedder = crate::stub::Embedder::offering(&offered, &[transient]);
        assert_eq!(embedder.embed(Some(&old)), Some(old.clone()));

        let mut failures = Vec::new();
        for disabled in [false, true] {
            let layout = crate::testdir::layout("native-brew-old-backend-");
            let prefix = Prefix::at(&layout.prefix.join("abrew"));
            if disabled {
                std::fs::create_dir_all(&prefix.root).unwrap();
                std::fs::write(prefix.disabled_path(), "off\n").unwrap();
            }
            let path = stub_path(&layout);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                &path,
                format!(
                    "#!/bin/sh\n{LEGACY_STUB_MARKER}\nATPKG='{}'\n",
                    sh_single(&old.to_string_lossy())
                ),
            )
            .unwrap();
            assert!(lay_pending(&layout, &embedder));
            lay_with(&layout, &embedder).unwrap();
            let refreshed = std::fs::read_to_string(&path).unwrap();
            assert_eq!(
                crate::stub::sh_named_atpkg(&refreshed, "ATPKG"),
                Some(old.clone()),
                "repair preserves the more durable installed backend"
            );
            assert!(!lay_pending(&layout, &embedder));
            assert_eq!(prefix.is_disabled(), disabled);

            let called = layout.prefix.join("legacy-brew-called");
            let mut command = aterm_uds::spawnfd::command("/bin/sh");
            command
                .arg(&path)
                .args(["services", "list"])
                .env("LEGACY_BREW_CALLED", &called)
                .env("ABREW_PREFIX", &prefix.root);
            for name in ["ABREW_REAL", "__ATERM_REROUTE_PASSTHROUGH"] {
                command.env_remove(name);
            }
            let output = command.output().unwrap();
            if output.status.success()
                || called.exists()
                || !String::from_utf8_lossy(&output.stderr).contains("old backend: unknown verb")
            {
                failures.push(format!(
                    "disabled={disabled}: status={:?}, legacy_called={}, stdout={:?}, stderr={:?}",
                    output.status,
                    called.exists(),
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[cfg(unix)]
    #[test]
    fn legacy_stub_refresh_keeps_disabled_native_route() {
        use std::os::unix::fs::PermissionsExt;

        const CHILD: &str = "ATERM_NATIVE_BREW_OFF_CONFIG_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let config = aterm_tempfile::TempDir::new().unwrap();
            let config_dir = config.path().join("aterm");
            std::fs::create_dir_all(&config_dir).unwrap();
            std::fs::write(config_dir.join("aterm.toml"), "[abrew]\nenabled = false\n").unwrap();
            let output = aterm_uds::spawnfd::command(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "abrew::stub::tests::legacy_stub_refresh_keeps_disabled_native_route",
                    "--nocapture",
                ])
                .env(CHILD, config.path().join("child-completed"))
                .env("XDG_CONFIG_HOME", config.path())
                .env_remove("ABREW_PREFIX")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "disabled-config child failed: stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                std::fs::read(config.path().join("child-completed")).unwrap(),
                b"refreshed both stubs\n"
            );
            return;
        }
        assert!(
            !crate::config::load_abrew().enabled(),
            "the child really uses disabled configuration"
        );
        for disabled in [false, true] {
            let layout = crate::testdir::layout("native-brew-legacy-");
            let backend = layout.prefix.join("atpkg ' backend");
            std::fs::write(
                &backend,
                "#!/bin/sh\nprintf 'ATPKG\\n'\nprintf '<%s>\\n' \"$@\"\n",
            )
            .unwrap();
            std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o755)).unwrap();
            let embedder = crate::stub::Embedder::offering(&backend, &[]);
            let prefix = Prefix::at(&layout.prefix.join("abrew"));
            if disabled {
                std::fs::create_dir_all(&prefix.root).unwrap();
                std::fs::write(prefix.disabled_path(), "off\n").unwrap();
            }
            let path = stub_path(&layout);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let legacy = format!(
                "#!/bin/sh\n{LEGACY_STUB_MARKER}\nATPKG='{}'\nprintf 'LEGACY\\n'\n",
                sh_single(&backend.to_string_lossy())
            );
            std::fs::write(&path, &legacy).unwrap();
            assert!(is_abrew_stub(&path));
            assert!(lay_pending(&layout, &embedder));
            lay_with(&layout, &embedder).unwrap();
            let refreshed = std::fs::read_to_string(&path).unwrap();
            assert_ne!(
                refreshed, legacy,
                "the old fallback-capable stub is refreshed"
            );
            assert!(is_abrew_stub(&path));
            assert_eq!(
                crate::stub::sh_named_atpkg(&refreshed, "ATPKG"),
                Some(backend.clone())
            );
            assert!(!lay_pending(&layout, &embedder));
            assert_eq!(
                prefix.is_disabled(),
                disabled,
                "stub repair preserves the off sentinel"
            );
            let mut command = aterm_uds::spawnfd::command("/bin/sh");
            command.arg(&path).arg("status");
            for name in ["ABREW_REAL", "__ATERM_REROUTE_PASSTHROUGH"] {
                command.env_remove(name);
            }
            let output = command.output().unwrap();
            assert!(output.status.success());
            assert_eq!(output.stdout, b"ATPKG\n<__abrew_native>\n<status>\n");

            let foreign = "#!/bin/sh\nprintf 'foreign\\n'\n";
            std::fs::write(&path, foreign).unwrap();
            lay_with(&layout, &embedder).unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), foreign);
        }
        std::fs::write(std::env::var_os(CHILD).unwrap(), "refreshed both stubs\n").unwrap();
    }
}
