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
        created.out.contains(
            "an already-installed toolset keeps updating but gets no new default-set member"
        ),
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
