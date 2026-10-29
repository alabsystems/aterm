// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The argument gate and the settings the installer persists.

use crate::support::{Home, Lib, Scratch, install_sh, installer, q, read, s, symlink, tree};

fn gate(home: &Home, extra: &[&str]) -> crate::support::Run {
    let mut args = vec![
        "--dry-run",
        "--no-app",
        "--no-cli",
        "--no-toolchain",
        "--no-path",
    ];
    args.extend_from_slice(extra);
    installer(&install_sh(), home, &[], &args)
}

#[test]
fn retired_token_flags_are_unknown_arguments() {
    let scratch = Scratch::new();
    let home = scratch.home("gate");
    let control = gate(&home, &[]);
    assert_eq!(control.code(), 2, "{}", control.all());
    for retired in ["--token", "--no-token"] {
        let run = gate(&home, &[retired]);
        assert_eq!(run.code(), 2, "{retired}: {}", run.all());
        assert!(run.stderr.contains(retired), "{retired}: {}", run.stderr);
        assert_ne!(
            run.stderr, control.stderr,
            "{retired} must fail on the flag itself"
        );
    }
    assert_eq!(tree(&home.home), Vec::<String>::new());
}

/// `--no-open` is an argument the gate takes (it leaves the installed app closed), not an
/// unknown one.
#[test]
fn no_open_is_an_argument() {
    let scratch = Scratch::new();
    let home = scratch.home("no-open");
    let control = gate(&home, &[]);
    let run = gate(&home, &["--no-open"]);
    assert_eq!(
        (run.code(), run.stderr.as_str()),
        (control.code(), control.stderr.as_str()),
        "{}",
        run.all()
    );
    assert_eq!(tree(&home.home), Vec::<String>::new());
}

/// `sh install.sh` on macOS runs it in bash 3.2's POSIX mode, which takes
/// no process substitution: there the script parses and answers as bash
/// does.
#[test]
fn the_script_runs_as_sh_runs_it() {
    let scratch = Scratch::new();
    let home = scratch.home("posix");
    let control = gate(&home, &[]);
    let args = [
        "--dry-run",
        "--no-app",
        "--no-cli",
        "--no-toolchain",
        "--no-path",
    ];
    let posix = installer(&install_sh(), &home, &[("POSIXLY_CORRECT", "1")], &args);
    assert_eq!(
        (posix.code(), posix.stderr.as_str()),
        (control.code(), control.stderr.as_str()),
        "{}",
        posix.all()
    );
}

#[test]
fn the_version_flag_pins_a_three_component_release() {
    let scratch = Scratch::new();
    let home = scratch.home("pin");
    for args in [
        &["--version"][..],
        &["-v"],
        &["--version", "banana"],
        &["--version", "0.61"],
    ] {
        let run = installer(&install_sh(), &home, &[], args);
        assert_eq!(run.code(), 2, "{args:?}: {}", run.all());
        assert!(
            run.stderr.to_lowercase().contains("pins"),
            "{args:?}: the refusal says the flag pins a release: {}",
            run.stderr
        );
    }
    assert_eq!(tree(&home.home), Vec::<String>::new());
}

#[test]
fn only_one_opts_out_through_an_env_twin() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    let mut rows = Vec::new();
    for twin in ["ATERM_NO_TOOLCHAIN", "ATERM_NO_PATH"] {
        for (set, rc, on) in [
            (format!("unset {twin}"), 0, "1"),
            (format!("export {twin}="), 0, "1"),
            (format!("export {twin}=0"), 0, "1"),
            (format!("export {twin}=1"), 0, "0"),
            (format!("export {twin}=yes"), 2, ""),
        ] {
            let call = lib.call(format!("( {set}; env_twin_on {twin} )"), &[]);
            rows.push((set, rc, on, call));
        }
    }
    let recs = lib.run(&scratch);
    for (set, rc, on, i) in rows {
        assert_eq!((recs[i].rc, recs[i].out.as_str()), (rc, on), "{set}");
    }
}

#[test]
fn no_toolchain_persists_its_exclusion_without_overwriting() {
    let scratch = Scratch::new();
    let rows: &[(&str, Option<&str>, &str, &str)] = &[
        (
            "create",
            None,
            "create",
            "[packages]\nauto_install = false\n",
        ),
        (
            "append",
            Some("[font]\nsize = 13"),
            "append",
            "[font]\nsize = 13\n\n[packages]\nauto_install = false\n",
        ),
        (
            "insert",
            Some("[packages]  # mine\nchannel = \"stable\"\n\n[font]\nsize = 13\n"),
            "insert",
            "[packages]  # mine\nauto_install = false\nchannel = \"stable\"\n\n[font]\nsize = 13\n",
        ),
        (
            "kept-true",
            Some("[packages]\nauto_install = true\n"),
            "keep auto_install = true",
            "[packages]\nauto_install = true\n",
        ),
        (
            "kept-retired",
            Some("[packages]\nseed_install = true\n"),
            "keep seed_install = true",
            "[packages]\nseed_install = true\n",
        ),
        (
            "kept-false",
            Some("[packages]\nauto_install=false\n"),
            "keep auto_install=false",
            "[packages]\nauto_install=false\n",
        ),
        (
            "inline",
            Some("packages = { auto_install = true }\n"),
            "keep [packages] is written in a form this installer does not edit",
            "packages = { auto_install = true }\n",
        ),
        (
            "dotted",
            Some("packages.channel = \"stable\"\n"),
            "keep [packages] is written in a form this installer does not edit",
            "packages.channel = \"stable\"\n",
        ),
        (
            "other-table",
            Some("[font]\nauto_install = true\n"),
            "append",
            "[font]\nauto_install = true\n\n[packages]\nauto_install = false\n",
        ),
    ];
    let mut lib = Lib::new();
    let mut calls = Vec::new();
    for (name, before, _, _) in rows {
        let cfg = scratch.join(&format!("cfg/{name}/aterm.toml"));
        if let Some(body) = before {
            crate::support::write(&cfg, body);
        }
        let plan = lib.call(format!("no_toolchain_plan {}", q(s(&cfg))), &[]);
        let persist = lib.call(format!("persist_no_toolchain {}", q(s(&cfg))), &[]);
        let again = lib.call(format!("no_toolchain_plan {}", q(s(&cfg))), &[]);
        calls.push((cfg, plan, persist, again));
    }
    let target = scratch.write(
        "cfg/link/dotfile.toml",
        "[packages]\nchannel = \"stable\"\n",
    );
    let link = scratch.join("cfg/link/aterm.toml");
    symlink(&target, &link);
    let linked = lib.call(format!("persist_no_toolchain {}", q(s(&link))), &[]);
    let recs = lib.run(&scratch);
    let created = &recs[calls[0].2];
    assert!(
        created.out.contains("aterm pkg install --default-set"),
        "{}",
        created.out
    );
    for ((name, _, plan, after), (cfg, p, w, again)) in rows.iter().zip(&calls) {
        assert_eq!(recs[*p].out, *plan, "{name}: the plan");
        assert_eq!(recs[*w].rc, 0, "{name}: {}", recs[*w].err);
        assert_eq!(read(cfg), *after, "{name}: the file");
        assert!(
            recs[*again].out.starts_with("keep "),
            "{name}: a second run changes nothing: {}",
            recs[*again].out
        );
    }
    assert_eq!(recs[linked].rc, 0);
    assert!(link.is_symlink(), "the link is kept");
    assert_eq!(
        read(&target),
        "[packages]\nauto_install = false\nchannel = \"stable\"\n"
    );
}

/// The aterm.toml files an exclusion goes to: the one a shell's aterm reads, then `~/.config`'s,
/// which the app opened from Finder reads (launchd's environment has no `XDG_CONFIG_HOME`),
/// when `XDG_CONFIG_HOME` names another folder. The control: unset, empty or `~/.config`
/// itself, there is one; and another spelling of `~/.config` (a trailing slash, `//`, `/.`,
/// the folder a `~/.config` link points at), there or not yet, is `~/.config`, named once
/// (round 12's review: two names for one file).
#[test]
fn every_config_an_aterm_reads_is_named() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    let home = lib.call("echo \"$HOME\"", &[]);
    let one: &[&str] = &["~/.config/aterm/aterm.toml"];
    let rows: &[(&str, &[&str])] = &[
        ("unset XDG_CONFIG_HOME", one),
        ("XDG_CONFIG_HOME=''", one),
        ("XDG_CONFIG_HOME=\"$HOME/.config\"", one),
        ("XDG_CONFIG_HOME=\"$HOME/.config/\"", one),
        ("XDG_CONFIG_HOME=\"$HOME//.config\"", one),
        ("XDG_CONFIG_HOME=\"$HOME/.config/.\"", one),
        (
            "XDG_CONFIG_HOME=/x",
            &["/x/aterm/aterm.toml", "~/.config/aterm/aterm.toml"],
        ),
        (
            "mkdir \"$HOME/real\" && ln -s \"$HOME/real\" \"$HOME/.config\" && \
             XDG_CONFIG_HOME=\"$HOME/real\"",
            one,
        ),
        ("XDG_CONFIG_HOME=\"$HOME/.config/\"", one),
        ("XDG_CONFIG_HOME=\"$HOME/real/\"", one),
        (
            "mkdir \"$HOME/dots\" && XDG_CONFIG_HOME=\"$HOME/dots\"",
            &["~/dots/aterm/aterm.toml", "~/.config/aterm/aterm.toml"],
        ),
    ];
    let calls: Vec<usize> = rows
        .iter()
        .map(|(set, _)| lib.call(format!("( {set}; aterm_config_paths )"), &[]))
        .collect();
    let recs = lib.run(&scratch);
    let home = recs[home].out.clone();
    for ((set, want), i) in rows.iter().zip(calls) {
        let want: Vec<String> = want.iter().map(|w| w.replacen('~', &home, 1)).collect();
        assert_eq!(
            recs[i].out.lines().collect::<Vec<_>>(),
            want,
            "{set}: {}",
            recs[i].err
        );
    }
}

/// --no-toolchain under an `XDG_CONFIG_HOME` elsewhere writes its exclusion where the app's
/// first launch reads it too (round 12: only XDG_CONFIG_HOME's file had it, so the app opened
/// from Finder installed the toolset that run excluded). The control: with no
/// `XDG_CONFIG_HOME`, `~/.config`'s alone, and no other folder is made.
#[test]
fn no_toolchain_reaches_the_config_the_app_reads() {
    let scratch = Scratch::new();
    for xdg in [true, false] {
        let fresh = scratch.home(if xdg { "xdg" } else { "plain" });
        let dots = fresh.home.join("dots");
        let mut extra = vec![("ATERM_NO_PATH", "0")];
        if xdg {
            extra.push(("XDG_CONFIG_HOME", s(&dots)));
        }
        let app_reads = fresh.home.join(".config/aterm/aterm.toml");
        let plan = installer(
            &install_sh(),
            &fresh,
            &extra,
            &["--dry-run", "--no-app", "--no-cli"],
        )
        .all();
        let named: Vec<&str> = plan
            .lines()
            .filter(|l| l.starts_with("  config:   creates "))
            .collect();
        let mut want = vec![format!("  config:   creates {}", s(&app_reads))];
        if xdg {
            want.insert(
                0,
                format!("  config:   creates {}", s(&dots.join("aterm/aterm.toml"))),
            );
        }
        assert_eq!(
            named.len(),
            want.len(),
            "xdg {xdg}: the plan names each file: {plan}"
        );
        for (line, want) in named.iter().zip(&want) {
            assert!(line.starts_with(want.as_str()), "{line:?} names {want:?}");
        }
        assert!(
            tree(&fresh.home).is_empty(),
            "the dry run made nothing: {plan}"
        );
        let run = installer(&install_sh(), &fresh, &extra, &["--no-app", "--no-cli"]);
        assert_eq!(
            read(&app_reads),
            "[packages]\nauto_install = false\n",
            "xdg {xdg}: {}",
            run.all()
        );
        assert_eq!(
            read(&dots.join("aterm/aterm.toml")),
            if xdg {
                "[packages]\nauto_install = false\n"
            } else {
                ""
            },
            "xdg {xdg}: {}",
            run.all()
        );
        assert_eq!(dots.exists(), xdg, "xdg {xdg}: {}", run.all());
    }
}

/// `XDG_CONFIG_HOME` spelled as `~/.config` another way is one config end to end: the plan
/// creates it once and the run writes it once (round 12's review: `~/.config/` had the plan
/// create `~/.config//aterm/aterm.toml` and `~/.config/aterm/aterm.toml`, then the run write the
/// first and find the second "already says" the key).
#[test]
fn another_spelling_of_dot_config_is_one_config() {
    let scratch = Scratch::new();
    let fresh = scratch.home("slash");
    let xdg = format!("{}/", s(&fresh.home.join(".config")));
    let extra = [("ATERM_NO_PATH", "0"), ("XDG_CONFIG_HOME", xdg.as_str())];
    let app_reads = fresh.home.join(".config/aterm/aterm.toml");
    let plan = installer(
        &install_sh(),
        &fresh,
        &extra,
        &["--dry-run", "--no-app", "--no-cli"],
    )
    .all();
    let named: Vec<&str> = plan
        .lines()
        .filter(|l| l.starts_with("  config:"))
        .collect();
    assert_eq!(named.len(), 1, "{plan}");
    assert!(
        named[0].starts_with(&format!("  config:   creates {} ", s(&app_reads))),
        "{plan}"
    );
    let run = installer(&install_sh(), &fresh, &extra, &["--no-app", "--no-cli"]).all();
    assert_eq!(
        read(&app_reads),
        "[packages]\nauto_install = false\n",
        "{run}"
    );
    assert_eq!(run.matches("--no-toolchain: wrote ").count(), 1, "{run}");
    assert!(!run.contains("already says"), "{run}");
}

/// What the plan says the installed aterm's PATH step does: with a toolset coming, a block with
/// its hook and the bin dir, wherever PATH stands (the control); with none coming
/// (--no-toolchain), a block for the bin dir when PATH lacks it, and no promised block when PATH
/// has it, where atpkg edits nothing unless an earlier toolset's hook is laid (round 12's
/// review: the line said before the download promised a block there, and the run made none).
#[test]
fn the_plan_promises_a_path_block_only_where_one_is_written() {
    let scratch = Scratch::new();
    let bin = "/h/.local/bin";
    let block = "appends at most ONE marker-fenced block to your shell profile";
    let rows: &[(&str, &str, String)] = &[
        (
            "1",
            "/usr/bin:/bin",
            format!("{block} — the toolset hook + {bin}"),
        ),
        (
            "1",
            "/usr/bin:/h/.local/bin:/bin",
            format!("{block} — the toolset hook + {bin}"),
        ),
        (
            "0",
            "/usr/bin:/bin",
            format!(
                "{block} — {bin}, and the toolset hook only if an ALab toolset is installed \
                 already"
            ),
        ),
        (
            "0",
            "/usr/bin:/h/.local/bin:/bin",
            format!(
                "edits your shell profile only for an ALab toolset installed earlier — {bin} is \
                 already on PATH"
            ),
        ),
    ];
    let mut lib = Lib::new();
    let calls: Vec<usize> = rows
        .iter()
        .map(|(toolchain, path, _)| {
            lib.call(
                format!(
                    "( DO_TOOLCHAIN={toolchain} BIN_DIR={bin} PATH={}; path_step_plan )",
                    q(path)
                ),
                &[],
            )
        })
        .collect();
    let recs = lib.run(&scratch);
    for ((toolchain, path, want), i) in rows.iter().zip(calls) {
        assert_eq!(recs[i].rc, 0, "{toolchain} {path}: {}", recs[i].err);
        assert_eq!(recs[i].out.trim_end(), want.as_str(), "{toolchain} {path}");
    }
}

#[test]
fn the_config_path_is_atpkgs() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    let unset = lib.call("XDG_CONFIG_HOME='' aterm_config_path", &[]);
    let set = lib.call("XDG_CONFIG_HOME=/x aterm_config_path", &["HOME"]);
    let recs = lib.run(&scratch);
    assert_eq!(
        recs[unset].out,
        format!("{}/.config/aterm/aterm.toml", recs[set].var("HOME"))
    );
    assert_eq!(recs[set].out, "/x/aterm/aterm.toml");
}
