// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

// amacs as the default EDITOR (docs/DESIGN-amacs-2026-10-19.md §8.3): each shell exports
// `EDITOR=$ATERM_DEFAULT_EDITOR` after the person's startup files, only when neither EDITOR nor
// VISUAL is set and the word resolves, and drops the hint.

/// The hint aterm's spawn hands an integrated shell (`ATERM_DEFAULT_EDITOR`).
#[cfg(unix)]
const EDITOR_HINT: (&str, &str) = ("ATERM_DEFAULT_EDITOR", "amacs");

/// What one shell exported once it was up: `EDITOR`, `VISUAL`, and the hint (`none` when gone).
#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
struct Exported {
    editor: String,
    visual: String,
    hint: String,
}

#[cfg(unix)]
impl Exported {
    fn of(editor: &str, visual: &str, hint: &str) -> Self {
        Self {
            editor: editor.to_string(),
            visual: visual.to_string(),
            hint: hint.to_string(),
        }
    }
}

/// One case of the rule: a startup-file line, whether `amacs` is on PATH, whether aterm handed
/// the hint, and what the shell must export.
#[cfg(unix)]
struct EditorCase {
    label: &'static str,
    /// `(name, value)` the person's startup file exports, if any.
    own: Option<(&'static str, &'static str)>,
    laid: bool,
    hinted: bool,
    want: (&'static str, &'static str),
}

#[cfg(unix)]
const EDITOR_CASES: &[EditorCase] = &[
    EditorCase {
        label: "nothing set: amacs",
        own: None,
        laid: true,
        hinted: true,
        want: ("amacs", ""),
    },
    EditorCase {
        label: "the person's EDITOR wins",
        own: Some(("EDITOR", "vi")),
        laid: true,
        hinted: true,
        want: ("vi", ""),
    },
    EditorCase {
        label: "the person's VISUAL wins",
        own: Some(("VISUAL", "nano")),
        laid: true,
        hinted: true,
        want: ("", "nano"),
    },
    EditorCase {
        label: "amacs not on PATH: nothing",
        own: None,
        laid: false,
        hinted: true,
        want: ("", ""),
    },
    EditorCase {
        label: "no hint (the opt-out): nothing",
        own: None,
        laid: true,
        hinted: false,
        want: ("", ""),
    },
];

/// Run `case` in a live `program` of `dialect` through the crate's own wrapper, as aterm starts
/// it: the startup file runs after the integration loaded, and the probe after the first prompt.
#[cfg(unix)]
fn editor_after_startup((dialect, program): (&str, &str), case: &EditorCase) -> Exported {
    let fx = LiveFixture::new();
    if case.laid {
        write_executable(&fx.foreign.join("amacs"), "#!/bin/sh\nexit 0\n");
    }
    let extra: &[(&str, &str)] = if case.hinted { &[EDITOR_HINT] } else { &[] };
    let posix_own = case
        .own
        .map(|(key, value)| format!("export {key}={value}\n"))
        .unwrap_or_default();
    let mut sh = match dialect {
        "zsh" => spawn_live_zsh(program, &fx, &posix_own, extra),
        "bash" => spawn_live_bash(program, &fx, &posix_own, extra),
        _ => {
            let conf_d = fx.home.join("config").join("fish").join("conf.d");
            std::fs::create_dir_all(&conf_d).expect("conf.d");
            if let Some((key, value)) = case.own {
                // Sorts after `aterm_shell_integration.fish`, so it runs after it.
                std::fs::write(
                    conf_d.join("zz-own.fish"),
                    format!("set -gx {key} {value}\n"),
                )
                .expect("the person's conf.d snippet");
            }
            spawn_live_fish(program, &fx, extra)
        }
    };
    let probe = if dialect == "fish" {
        "set -l e (/usr/bin/printenv EDITOR); set -l v (/usr/bin/printenv VISUAL); \
         set -l h (/usr/bin/printenv ATERM_DEFAULT_EDITOR; or echo none); \
         printf 'EDITOR_IS=%s\\nVISUAL_IS=%s\\nHINT_IS=%s\\n' \"$e\" \"$v\" \"$h\"; \
         echo MARK(math 40 + 2)"
    } else {
        "printf 'EDITOR_IS=%s\\nVISUAL_IS=%s\\nHINT_IS=%s\\n' \"$(/usr/bin/printenv EDITOR)\" \
         \"$(/usr/bin/printenv VISUAL)\" \"$(/usr/bin/printenv ATERM_DEFAULT_EDITOR || echo none)\"; \
         echo \"MARK$((40+2))\""
    };
    sh.send(probe);
    let at = sh.wait_for_after("MARK42", 0);
    let got = Exported {
        editor: sh.value_before("EDITOR_IS=", at),
        visual: sh.value_before("VISUAL_IS=", at),
        hint: sh.value_before("HINT_IS=", at),
    };
    sh.finish();
    got
}

/// Every case of §8.3's rule in zsh, each bash and fish.
/// NEGATIVE CONTROL: the person's EDITOR, the person's VISUAL, no `amacs` on PATH, and no hint
/// each keep `EDITOR=amacs` out; with every condition met it is exported.
#[cfg(unix)]
#[test]
fn each_shell_exports_amacs_only_where_the_person_set_no_editor() {
    let mut shells = ShellLanes::default();
    for lane in shells.lanes(&["zsh", "bash", "fish"]) {
        for case in EDITOR_CASES {
            let (editor, visual) = case.want;
            assert_eq!(
                editor_after_startup(lane, case),
                Exported::of(editor, visual, "none"),
                "{} ({}): {}",
                lane.0,
                lane.1,
                case.label
            );
        }
    }
}

/// The same rule in PowerShell, which aterm dot-sources after the profiles: the profile's line
/// runs first here.
/// NEGATIVE CONTROL: as in the POSIX shells.
#[cfg(unix)]
#[test]
fn pwsh_sets_amacs_only_where_the_person_set_no_editor() {
    let pwsh = need_pwsh();
    let script = format!(
        "{}/src/scripts/aterm_shell_integration.ps1",
        env!("CARGO_MANIFEST_DIR")
    );
    for case in EDITOR_CASES {
        let bin = aterm_tempfile::tempdir().expect("a bin dir");
        if case.laid {
            write_executable(&bin.path().join("amacs"), "#!/bin/sh\nexit 0\n");
        }
        let own = case
            .own
            .map(|(key, value)| format!("$env:{key} = '{value}'; "))
            .unwrap_or_default();
        let mut cmd = shell_command(pwsh);
        cmd.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!(
                "{own}. $env:ATERM_TEST_SCRIPT; \
                 [Console]::Write('EDITOR_IS=' + $env:EDITOR + '|VISUAL_IS=' + $env:VISUAL + \
                 '|HINT_IS=' + $(if (Test-Path Env:ATERM_DEFAULT_EDITOR) {{ 'kept' }} else {{ 'none' }}) + '|')"
            ),
        ])
        .env("ATERM_TEST_SCRIPT", &script)
        .env("PATH", format!("{}:/usr/bin:/bin", bin.path().display()))
        .env("POWERSHELL_TELEMETRY_OPTOUT", "1")
        .env("POWERSHELL_UPDATECHECK", "Off");
        if case.hinted {
            cmd.env(EDITOR_HINT.0, EDITOR_HINT.1);
        }
        let out = cmd
            .output()
            .unwrap_or_else(|error| panic!("spawn {pwsh}: {error}"));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{}: {stdout:?}; stderr: {:?}",
            case.label,
            String::from_utf8_lossy(&out.stderr)
        );
        let (editor, visual) = case.want;
        assert!(
            stdout.contains(&format!(
                "EDITOR_IS={editor}|VISUAL_IS={visual}|HINT_IS=none|"
            )),
            "pwsh: {}: {stdout:?}",
            case.label
        );
    }
}
