// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
#[cfg(unix)]
use aterm_toolchain::lasting::{CACHEDIR_TAG_SIGNATURE, temp_roots_from};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;

fn layout(label: &str) -> crate::testdir::ScratchLayout {
    crate::testdir::layout(&format!("atpkg-stub-{label}-"))
}

fn tool(name: &str) -> ToolName {
    ToolName::new(name).unwrap()
}

#[test]
fn an_agent_programs_pending_stub_stands_in_agents_too_and_passes_arguments() {
    let l = layout("agents-stub");
    lay_adoption_stubs(&l, &crate::config::PackagesConfig::default());
    let claude = tool("claude");
    let twin = l.agent_shim(&claude);
    assert!(
        is_pending_stub(&twin),
        "claude's stub stands first on PATH: {}",
        twin.display()
    );
    assert!(is_pending_stub(&l.shim(&claude)), "and in bin/ as before");
    assert!(
        !l.agent_shim(&tool("trust")).exists(),
        "only the agent programs get a twin"
    );
    let body = std::fs::read_to_string(&twin).unwrap();
    let forwards = if cfg!(windows) {
        "__pending \"claude\" %*"
    } else {
        "__pending 'claude' \"$@\""
    };
    assert!(
        body.contains(forwards),
        "the caller's arguments ride through: {body}"
    );
    let mut installed = BTreeMap::new();
    installed.insert(String::from("claude"), 1u64);
    reconcile(&l, &BTreeSet::new(), &installed, &BTreeMap::new());
    assert!(
        !twin.exists(),
        "an installed program's stub is swept from agents/"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_command_only_the_manifest_exposes_is_stubbed_naming_its_program() {
    let l = layout("exposed");
    let wanted: BTreeSet<String> = BTreeSet::from(["clean".to_string()]);
    let exposed = |names: &[&str]| {
        BTreeMap::from([(
            "clean".to_string(),
            names.iter().map(ToString::to_string).collect::<Vec<_>>(),
        )])
    };
    let lsp = l.shim(&tool("clean-lsp"));
    reconcile(
        &l,
        &wanted,
        &BTreeMap::new(),
        &exposed(&["clean", "clean-lsp"]),
    );
    assert!(is_pending_stub(&lsp), "laid with its program");
    assert!(is_pending_stub(&l.shim(&tool("clean"))));
    assert_eq!(
        program_of("clean-lsp"),
        "clean-lsp",
        "the table does not know it"
    );
    assert_eq!(program_of_in(&l, "clean-lsp"), "clean");
    assert_eq!(
        program_of_in(&l, "targo"),
        "trust",
        "the table still answers first"
    );
    reconcile(&l, &wanted, &BTreeMap::new(), &BTreeMap::new());
    assert!(
        is_pending_stub(&lsp),
        "a pass that read no manifest of clean keeps what the last one said"
    );
    reconcile(&l, &wanted, &BTreeMap::new(), &exposed(&["clean"]));
    assert!(
        !lsp.exists(),
        "a manifest that no longer exposes it sweeps it"
    );
    reconcile(
        &l,
        &wanted,
        &BTreeMap::new(),
        &exposed(&["clean", "clean-lsp"]),
    );
    let mut installed = BTreeMap::new();
    installed.insert("clean".to_string(), 1u64);
    reconcile(&l, &wanted, &installed, &BTreeMap::new());
    assert!(
        !lsp.exists(),
        "an installed program's command stub is swept"
    );
    reconcile(
        &l,
        &wanted,
        &BTreeMap::new(),
        &exposed(&["clean", "clean-lsp"]),
    );
    remove_stub(&l, "clean");
    assert!(!lsp.exists(), "remove_stub takes it with its program");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_program_name_that_would_break_the_comes_with_line_lays_no_command_stub() {
    let l = layout("comes-with-line");
    let program = "ay\nid";
    let wanted: BTreeSet<String> = BTreeSet::from([program.to_string()]);
    let exposed = BTreeMap::from([(program.to_string(), vec!["ay-qbf".to_string()])]);
    reconcile(&l, &wanted, &BTreeMap::new(), &exposed);
    assert!(
        !l.shim(&tool("ay-qbf")).exists(),
        "no stub carries the program's second line as a command"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_programs_commands_are_stubbed_while_it_is_wanted_and_absent() {
    let l = layout("commands");
    lay_adoption_stubs(&l, &crate::config::PackagesConfig::default());
    let commands = ["targo", "trustc", "tippy", "trustfmt", "trustdoc"];
    for command in commands {
        assert!(pending_stub_exists(&l, command), "{command} answers");
        assert_eq!(program_of(command), "trust");
        let body = std::fs::read_to_string(l.shim(&tool(command))).unwrap();
        let names = if cfg!(windows) {
            format!("__pending \"{command}\" %*")
        } else {
            format!("__pending '{command}' \"$@\"")
        };
        assert!(
            body.contains(&names),
            "the stub names what was typed: {body}"
        );
    }
    assert_eq!(program_of("ty"), "ty", "a program is its own");
    for upstream in [
        "cargo", "rustc", "clippy", "rustfmt", "rustdoc", "lean", "z3",
    ] {
        let stub = format!("{upstream}{}", crate::platform::SHIM_SUFFIX);
        assert!(
            std::fs::symlink_metadata(l.bin_dir().join(stub)).is_err(),
            "{upstream} is never stubbed in bin/"
        );
    }
    let trust: BTreeSet<String> = ["trust".to_string()].into_iter().collect();
    reconcile(&l, &trust, &BTreeMap::new(), &BTreeMap::new());
    assert!(commands.iter().all(|c| pending_stub_exists(&l, c)), "kept");
    reconcile(&l, &BTreeSet::new(), &BTreeMap::new(), &BTreeMap::new());
    assert!(
        commands.iter().all(|c| !l.shim(&tool(c)).exists()),
        "swept once trust leaves `wanted`"
    );
    reconcile(&l, &trust, &BTreeMap::new(), &BTreeMap::new());
    assert!(
        commands.iter().all(|c| pending_stub_exists(&l, c)),
        "laid again"
    );
    let installed: BTreeMap<String, u64> = [("trust".to_string(), 1u64)].into_iter().collect();
    reconcile(&l, &trust, &installed, &BTreeMap::new());
    assert!(
        commands.iter().all(|c| !l.shim(&tool(c)).exists()),
        "swept once trust is installed (its real shims stand there next)"
    );
    reconcile(&l, &trust, &BTreeMap::new(), &BTreeMap::new());
    remove_stub(&l, "trust");
    assert!(
        commands.iter().all(|c| !l.shim(&tool(c)).exists()),
        "an uninstall takes the commands with the program"
    );
}

#[test]
fn a_command_stub_never_takes_a_file_it_did_not_lay() {
    let l = layout("command-foreign");
    std::fs::create_dir_all(l.bin_dir()).unwrap();
    let mine = l.shim(&tool("targo"));
    std::fs::write(&mine, "#!/bin/sh\necho mine\n").unwrap();
    let trust: BTreeSet<String> = ["trust".to_string()].into_iter().collect();
    reconcile(&l, &trust, &BTreeMap::new(), &BTreeMap::new());
    assert_eq!(
        std::fs::read_to_string(&mine).unwrap(),
        "#!/bin/sh\necho mine\n"
    );
    assert!(pending_stub_exists(&l, "trustc"), "its siblings are laid");
    reconcile(&l, &BTreeSet::new(), &BTreeMap::new(), &BTreeMap::new());
    assert_eq!(
        std::fs::read_to_string(&mine).unwrap(),
        "#!/bin/sh\necho mine\n",
        "nor swept"
    );
}

#[test]
fn a_lost_or_cut_short_stub_is_laid_again() {
    let l = layout("relay");
    let cfg = crate::config::PackagesConfig::default();
    lay_adoption_stubs(&l, &cfg);
    let targo = l.shim(&tool("targo"));
    let trustc = l.shim(&tool("trustc"));
    let whole = std::fs::read_to_string(&trustc).unwrap();
    std::fs::remove_file(&targo).unwrap();
    std::fs::write(&trustc, format!("#!/bin/sh\n{STUB_MARKER}\n")).unwrap();
    lay_adoption_stubs(&l, &cfg);
    assert!(
        pending_stub_exists(&l, "targo"),
        "a missing stub is laid again"
    );
    assert_eq!(
        std::fs::read_to_string(&trustc).unwrap(),
        whole,
        "a cut-short stub is rewritten whole"
    );
}

#[test]
fn an_alias_is_never_a_stub() {
    let l = layout("alias-stub");
    let wanted: BTreeSet<String> = ["trust".to_string()].into_iter().collect();
    reconcile(&l, &wanted, &BTreeMap::new(), &BTreeMap::new());
    assert!(
        pending_stub_exists(&l, "trust"),
        "the plain name is stubbed"
    );
    assert!(
        std::fs::symlink_metadata(l.shim(&tool("alab-trust"))).is_err(),
        "no stub is laid under the alias name"
    );
    assert!(!pending_stub_exists(&l, "alab-trust"));
    write_pending_stub(&l, &tool("alab-trust")).unwrap();
    assert!(
        std::fs::symlink_metadata(l.shim(&tool("alab-trust"))).is_err(),
        "the writer is a no-op for an alias"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn stub_content_shape_and_shim_invisibility() {
    let body = stub_content_sh(
        &tool("trust"),
        None,
        Path::new("/Apps/aterm.app/Contents/MacOS/atpkg"),
    );
    assert!(body.contains(STUB_MARKER));
    assert_eq!(
        crate::platform::parse_sh_shim_target(&body),
        None,
        "a stub must never parse as a store shim"
    );
    let nasty = ToolName::new("a'b").expect("shim_allowed admits quotes");
    let body = stub_content_sh(&nasty, None, Path::new("/x'y/atpkg"));
    assert!(body.contains("__pending 'a'\\''b'"));
    assert!(body.contains("ATPKG='/x'\\''y/atpkg'"));
}

#[test]
fn cmd_stub_shape_marker_and_safety() {
    let body = stub_content_cmd(
        &tool("trust"),
        None,
        Path::new(r"C:\Program Files\aterm\atpkg.exe"),
    );
    assert!(
        body.starts_with(&crate::platform::cmd_frame()),
        "framed: {body}"
    );
    assert!(body.is_ascii(), "a .cmd stub is ASCII: {body}");
    assert_eq!(
        body.matches("__pending \"trust\" %*\r\n").count(),
        2,
        "both arms that reach atpkg carry the caller's arguments: {body}"
    );

    for hostile in ["a%b", "a\"b", "a&b", "a^b", "a|b", "a<b", "a!b"] {
        assert!(
            !cmd_stub_name_safe(hostile),
            "{hostile:?} has no inert batch embedding"
        );
    }
    for fine in ["trust", "ay", "clean-2", "a'b", "a b"] {
        assert!(cmd_stub_name_safe(fine), "{fine:?} is batch-inert quoted");
    }
}

/// The batch stub gives a private stand-in atpkg the caller's arguments,
/// checked by real cmd.exe when this test runs on Windows.
#[cfg(windows)]
#[test]
fn the_batch_stub_hands_atpkg_the_callers_arguments() {
    let l = layout("cmd-args");
    let record = l.prefix.join("argv.txt");
    let fake = l.prefix.join("fake-atpkg.cmd");
    std::fs::write(&fake, format!("@echo %*> \"{}\"\r\n", record.display())).unwrap();
    let stub = l.prefix.join("claude.cmd");
    std::fs::write(&stub, stub_content_cmd(&tool("claude"), None, &fake)).unwrap();
    let mut command = aterm_uds::spawnfd::command("cmd");
    command
        .args(["/d", "/c"])
        .arg(&stub)
        .args(["-p", "two words"]);
    let ran = aterm_uds::deadline::output_until(
        &mut command,
        std::time::Instant::now() + std::time::Duration::from_secs(60),
    )
    .expect("the private cmd fixture must finish within the hang detector");
    let got = std::fs::read_to_string(&record)
        .unwrap_or_else(|e| panic!("the stand-in never ran ({e}): {ran:?}"));
    assert_eq!(got.trim_end(), "__pending \"claude\" -p \"two words\"");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_pre_frame_cmd_stub_re_laid_while_executing_resumes_into_the_frame() {
    use crate::platform::{
        CMD_FRAME_HEAD, CMD_LEGACY_TARGET_BOUND_BYTES, CMD_PADDING_END_BYTES, cmd_frame,
        cmd_resumed_line, cmd_resumed_line_is_inert,
    };
    let frame = cmd_frame();
    let long_atpkg = std::path::PathBuf::from(format!(
        "C:\\{}\\atpkg.exe",
        "p".repeat(CMD_LEGACY_TARGET_BOUND_BYTES - "C:\\\\atpkg.exe".len())
    ));
    assert_eq!(
        long_atpkg.to_string_lossy().len(),
        CMD_LEGACY_TARGET_BOUND_BYTES
    );
    let cases = [
        (
            "a Program Files atpkg",
            stub_content_cmd(
                &tool("trust"),
                None,
                Path::new(r"C:\Program Files (x86)\aterm\app\atpkg.exe"),
            ),
        ),
        (
            "the longest atpkg path",
            stub_content_cmd(&tool("vendorx"), None, &long_atpkg),
        ),
    ];
    for (what, new) in &cases {
        assert!(new.starts_with(&frame), "{what}");
        let old = &new[frame.len()..];
        assert!(old.starts_with("@echo off\r\n"), "{what}");
        let old = old.as_bytes();
        let new = new.as_bytes();
        assert!(
            old.len() <= CMD_PADDING_END_BYTES,
            "{what}: {} bytes",
            old.len()
        );
        let ends: Vec<usize> = old
            .iter()
            .enumerate()
            .filter(|&(_, &b)| b == b'\n')
            .map(|(i, _)| i + 1)
            .collect();
        assert_eq!(ends.last().copied(), Some(old.len()), "{what}");
        assert_eq!(ends[0], CMD_STUB_FIRST_LINE.len(), "{what}");
        assert_eq!(
            ends[0],
            CMD_FRAME_HEAD.len() - 2,
            "{what}: on the head's CRLF"
        );
        assert_eq!(
            cmd_resumed_line(new, ends[0]).as_deref(),
            Some(""),
            "{what}: the first old line end reads the head's CRLF as empty"
        );
        assert!(
            !cmd_resumed_line_is_inert(cmd_resumed_line(new, ends[0] - 1).as_deref()),
            "{what}: one byte shorter would not be"
        );
        assert_eq!(
            cmd_resumed_line(new, 0).as_deref(),
            Some("@goto :main"),
            "{what}: the fresh run"
        );
        for offset in CMD_FRAME_HEAD.len() - 2..=old.len() {
            let line = cmd_resumed_line(new, offset);
            assert!(
                cmd_resumed_line_is_inert(line.as_deref()),
                "{what}: a resume at offset {offset} of {} reads {line:?}",
                old.len()
            );
        }
        for end in &ends {
            assert!(
                cmd_resumed_line_is_inert(cmd_resumed_line(new, *end).as_deref()),
                "{what}: old line end {end}"
            );
        }
    }
    let old = &cases[0].1[frame.len()..];
    let moved = stub_content_cmd(
        &tool("trust"),
        None,
        Path::new(&format!(
            r"C:\Program Files (x86)\aterm\app\{}\atpkg.exe",
            "m".repeat(29)
        )),
    );
    let bare = &moved[frame.len()..];
    assert_eq!(bare.len(), old.len() + 60);
    let line = cmd_resumed_line(bare.as_bytes(), old.len());
    assert!(
        !cmd_resumed_line_is_inert(line.as_deref()),
        "laid bare, the resume lands in the body: {line:?}"
    );
    assert_eq!(
        cmd_resumed_line(moved.as_bytes(), old.len()).as_deref(),
        Some(&":".repeat(78)[(old.len() - CMD_FRAME_HEAD.len()) % 80..]),
        "framed, the same resume reads a colon label"
    );
}

#[test]
fn rem_marker_recognition_round_trips() {
    let dir = crate::testdir::dir("atpkg-stub-rem-");
    let path = dir.join("trust.cmd");
    std::fs::write(
        &path,
        stub_content_cmd(&tool("trust"), None, Path::new(r"C:\x\atpkg.exe")),
    )
    .unwrap();
    assert!(is_pending_stub(&path), "batch stub recognized");
    let _ = std::fs::remove_file(&path);
}

/// A laid stub is recognized, the real shim lands over it via temp+rename with the name
/// always resolving, and afterwards the stub is gone.
#[cfg(unix)]
#[test]
fn install_over_a_stub_never_leaves_a_window() {
    let l = layout("exec-through");
    let t = tool("trust");
    write_pending_stub(&l, &t).unwrap();
    let shim = l.shim(&t);
    assert!(is_pending_stub(&shim));
    assert!(pending_stub_exists(&l, "trust"));
    assert_eq!(crate::platform::resolve_shim(&shim), None);
    let build_bin = l.prefix.join("store/trust/210/bin");
    std::fs::create_dir_all(&build_bin).unwrap();
    std::fs::write(build_bin.join("trust"), "#!/bin/sh\nexit 0\n").unwrap();
    crate::platform::install_shim(&build_bin, &t, &shim)
        .expect("install over a stub must not EEXIST");
    let target = crate::platform::resolve_shim(&shim).expect("real shim resolves");
    assert!(target.starts_with(&build_bin));
    assert!(!is_pending_stub(&shim));
    let strays: Vec<_> = std::fs::read_dir(l.bin_dir())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name() != "trust")
        .collect();
    assert!(
        strays.is_empty(),
        "temp+rename leaves nothing beside the shim: {strays:?}"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// The stub is the lowest-precedence occupant: it never overwrites a real shim,
/// a tombstone, or a hand-made file — and removal is recognition-gated the same
/// way.
#[cfg(unix)]
#[test]
fn stub_never_clobbers_and_removal_is_gated() {
    let l = layout("no-clobber");
    let t = tool("ty");
    let shim = l.shim(&t);
    l.ensure_dir(&l.bin_dir()).unwrap();
    std::fs::write(&shim, "#!/bin/sh\n# someone's own file\nexit 3\n").unwrap();
    write_pending_stub(&l, &t).unwrap();
    assert!(
        std::fs::read_to_string(&shim)
            .unwrap()
            .contains("someone's own file"),
        "a foreign file wins over the stub"
    );
    remove_stub(&l, "ty");
    assert!(
        shim.exists(),
        "removal only removes what the marker proves ours"
    );
    crate::activate::install_tombstone_shim(&l, &t).unwrap();
    write_pending_stub(&l, &t).unwrap();
    assert!(!is_pending_stub(&shim), "a tombstone outranks a stub");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// Adoption lays the whole roster, every agent program included, minus
/// removed/excluded/installed; `remove_all_stubs` clears exactly the stubs.
#[cfg(unix)]
#[test]
fn adoption_lays_the_roster_and_remove_all_clears_it() {
    let l = layout("adoption");
    std::fs::write(l.removed(), "ny\nkimi\n").unwrap();
    let cfg = crate::config::PackagesConfig {
        exclude: Some(vec![String::from("agy"), String::from("grok")]),
        ..Default::default()
    };
    lay_adoption_stubs(&l, &cfg);
    for kept_off in ["agy", "grok", "kimi"] {
        let t = tool(kept_off);
        assert!(
            !l.shim(&t).exists() && !l.agent_shim(&t).exists(),
            "{kept_off}: an excluded or removed program gets no stub, in bin/ or agents/"
        );
    }
    for (name, _) in DEFAULT_SET_STUB_NAMES {
        let expect = *name != "ny";
        assert_eq!(
            pending_stub_exists(&l, name),
            expect,
            "{name}: removed-on-purpose stays removed at adoption"
        );
    }
    let triple = crate::cli::current_triple();
    for name in AGENT_PROGRAMS {
        let row = crate::vendor_direct::spec(name).unwrap();
        let laid = row.serves(triple) && !["agy", "grok", "kimi"].contains(name);
        assert_eq!(
            pending_stub_exists(&l, name),
            laid,
            "{name}: laid at adoption wherever it can arrive"
        );
        assert_eq!(
            l.agent_shim(&tool(name)).exists(),
            laid,
            "{name}: its twin too"
        );
    }
    let all = layout("adoption-all");
    lay_adoption_stubs(&all, &crate::config::PackagesConfig::default());
    for name in ["grok", "kimi"] {
        let row = crate::vendor_direct::spec(name).unwrap();
        assert_eq!(
            pending_stub_exists(&all, name) && all.agent_shim(&tool(name)).exists(),
            row.serves(triple),
            "{name} is in the default set: adoption lays it and its twin where it is served"
        );
    }
    let _ = std::fs::remove_dir_all(&all.prefix);
    std::fs::write(l.bin_dir().join("mine"), "not a stub").unwrap();
    remove_all_stubs(&l);
    for name in compiled_names() {
        assert!(!pending_stub_exists(&l, name), "{name} swept with the rest");
    }
    assert!(l.bin_dir().join("mine").exists());
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A stub already on disk with the exact bytes the pass would render is never re-laid
/// (no new inode); a stub whose body changed (an older client's extra line, a moved
/// atpkg) is rewritten.
#[cfg(unix)]
#[test]
fn an_identical_stub_is_left_alone_and_a_changed_one_is_rewritten() {
    use std::os::unix::fs::MetadataExt as _;
    let l = layout("identical-skip");
    let t = tool("trust");
    let body = stub_content(&t, None, Embedder::this_process().embed(None).as_deref());
    let fresh = pending_stub_executable(&l, &t).unwrap();
    assert!(fresh.is_some(), "an absent name is ours to claim");
    write_pending_stub(&l, &t).unwrap();
    let shim = l.shim(&t);
    assert!(is_pending_stub(&shim));
    assert_eq!(
        std::fs::read(&shim).unwrap(),
        body.as_bytes(),
        "the writer lays exactly the bytes the renderer renders"
    );
    assert!(stub_is_current(&shim, &body), "byte-identical: current");
    let ino = std::fs::metadata(&shim).unwrap().ino();
    let wanted: BTreeSet<String> = ["trust".to_string()].into_iter().collect();
    reconcile(&l, &wanted, &BTreeMap::new(), &BTreeMap::new());
    lay_adoption_stubs(&l, &crate::config::PackagesConfig::default());
    assert!(pending_stub_exists(&l, "trust"), "the reconcile keeps it");
    assert!(
        pending_stub_executable(&l, &t).unwrap().is_none(),
        "byte-identical: nothing to lay, tag or no tag"
    );
    assert_eq!(
        std::fs::metadata(&shim).unwrap().ino(),
        ino,
        "an identical stub is never re-laid"
    );
    let older = body.replacen(
        STUB_MARKER,
        "# atpkg pending-program stub v1\n# atpkg extra: asks consent before installing\n\
         # atpkg requires: clt",
        1,
    );
    std::fs::write(&shim, &older).unwrap();
    assert!(
        is_pending_stub(&shim),
        "an older extra stub is still a stub"
    );
    assert!(
        !stub_is_current(&shim, &body),
        "a changed body is never current"
    );
    reconcile(&l, &wanted, &BTreeMap::new(), &BTreeMap::new());
    assert_eq!(
        std::fs::read(&shim).unwrap(),
        body.as_bytes(),
        "the plain body is what landed"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// The index reconcile: newly listed names gain stubs, de-listed ones lose
/// them, sensitive names are refused, and installed programs need no stub.
#[cfg(unix)]
#[test]
fn reconcile_tracks_the_signed_set_and_refuses_sensitive_names() {
    let l = layout("reconcile");
    lay_adoption_stubs(&l, &crate::config::PackagesConfig::default());
    assert!(pending_stub_exists(&l, "ay"));
    let wanted: BTreeSet<String> = [
        "brandnew".to_string(),
        "trust".to_string(),
        "sudo".to_string(),
    ]
    .into_iter()
    .collect();
    let installed: BTreeMap<String, u64> = BTreeMap::new();
    reconcile(&l, &wanted, &installed, &BTreeMap::new());
    assert!(
        pending_stub_exists(&l, "brandnew"),
        "newly listed name gains a stub"
    );
    assert!(pending_stub_exists(&l, "trust"));
    assert!(
        !pending_stub_exists(&l, "sudo"),
        "the sensitive-name refusal binds stubs"
    );
    assert!(
        !pending_stub_exists(&l, "ay"),
        "a de-listed name loses its stub"
    );
    let installed: BTreeMap<String, u64> = [("trust".to_string(), 210)].into_iter().collect();
    reconcile(&l, &wanted, &installed, &BTreeMap::new());
    assert!(!pending_stub_exists(&l, "trust"));
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// The fallback chain through a real `/bin/sh`: a live embedded atpkg wins; a dangling
/// one falls back to the first absolute-entry atpkg on PATH (never a relative or empty
/// entry's); with neither, the static message lands on stderr with exit 127.
#[cfg(unix)]
#[test]
fn fallback_chain_runs_in_a_real_shell() {
    let l = layout("fallback");
    let t = tool("trust");
    let fake = l.prefix.join("fake-atpkg");
    std::fs::write(&fake, "#!/bin/sh\necho \"co-located: $*\"\nexit 127\n").unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let run = |body: &str, path_env: &str| {
        let script = l.prefix.join("stub-under-test");
        std::fs::write(&script, body).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::process::Command::new(&script)
            .env("PATH", path_env)
            .output()
            .unwrap()
    };
    let out = run(&stub_content(&t, None, Some(&fake)), "/nonexistent");
    assert_eq!(out.status.code(), Some(127));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "co-located: __pending trust\n"
    );
    let path_dir = l.prefix.join("pathbin");
    std::fs::create_dir_all(&path_dir).unwrap();
    std::fs::write(
        path_dir.join("atpkg"),
        "#!/bin/sh\necho \"path: $*\"\nexit 127\n",
    )
    .unwrap();
    std::fs::set_permissions(
        path_dir.join("atpkg"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let out = run(
        &stub_content(&t, None, Some(Path::new("/gone/after/relocation/atpkg"))),
        path_dir.to_str().unwrap(),
    );
    assert_eq!(out.status.code(), Some(127));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "path: __pending trust\n"
    );
    let out = run(&stub_content(&t, None, None), path_dir.to_str().unwrap());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "path: __pending trust\n"
    );
    let out = run(
        &stub_content(&t, None, Some(Path::new("/gone/atpkg"))),
        "/nonexistent",
    );
    assert_eq!(out.status.code(), Some(127));
    assert!(String::from_utf8_lossy(&out.stderr).contains(STUB_UNREACHABLE_MSG));
    let script = l.prefix.join("stub-under-test");
    std::fs::write(&script, stub_content(&t, None, None)).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    for path_env in ["/nonexistent:.", "/nonexistent:"] {
        let out = std::process::Command::new(&script)
            .current_dir(&path_dir)
            .env("PATH", path_env)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "", "{path_env:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(STUB_UNREACHABLE_MSG),
            "{path_env:?}: {out:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// The classes the rule ranks, each from its own path: an app bundle, a plain binary, and
/// the four transient shapes — a temp dir, a cargo target dir, a translocated copy, a
/// mounted image.
#[cfg(unix)]
#[test]
fn durability_ranks_a_bundle_over_a_plain_binary_over_a_transient_copy() {
    let l = layout("durability");
    let root = std::fs::canonicalize(&l.prefix).unwrap();
    let target = root.join("checkout/target.noindex");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(
        target.join("CACHEDIR.TAG"),
        format!("{CACHEDIR_TAG_SIGNATURE}\n# This file is a cache directory tag.\n"),
    )
    .unwrap();
    let untagged = root.join("untagged");
    std::fs::create_dir_all(&untagged).unwrap();
    std::fs::write(untagged.join("CACHEDIR.TAG"), "not a tag\n").unwrap();
    let scratch = root.join("tmp");
    let scratch_roots = [scratch.clone()];
    for (path, want) in [
        (
            root.join("Applications/aterm.app/Contents/MacOS/atpkg"),
            Durability::Bundle,
        ),
        (
            root.join("Applications/aterm (dev).app/Contents/MacOS/atpkg"),
            Durability::Bundle,
        ),
        (root.join(".local/lib/aterm/bin/atpkg"), Durability::Plain),
        (untagged.join("debug/atpkg"), Durability::Plain),
        (root.join("aterm.app/MacOS/atpkg"), Durability::Plain),
        (
            root.join("aterm.app/Contents/Resources/atpkg"),
            Durability::Plain,
        ),
        (root.join("aterm.app/Other/MacOS/atpkg"), Durability::Plain),
        (root.join("aterm/Contents/MacOS/atpkg"), Durability::Plain),
        (target.join("debug/atpkg"), Durability::Transient),
        (
            target.join("release/bundle/aterm.app/Contents/MacOS/atpkg"),
            Durability::Transient,
        ),
        (scratch.join("run/bin/atpkg"), Durability::Transient),
        (
            PathBuf::from(
                "/private/var/folders/x/AppTranslocation/Y/d/aterm.app/Contents/MacOS/atpkg",
            ),
            Durability::Transient,
        ),
        (
            PathBuf::from("/Volumes/aterm/aterm.app/Contents/MacOS/atpkg"),
            Durability::Transient,
        ),
    ] {
        assert_eq!(
            durability(&path, &scratch_roots),
            want,
            "{}",
            path.display()
        );
    }
    let real = root.join("real-tmp");
    std::fs::create_dir_all(&real).unwrap();
    let link = root.join("link-tmp");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let through_a_link = temp_roots_from(link.clone());
    assert!(
        through_a_link.contains(&link) && through_a_link.contains(&real),
        "{through_a_link:?}"
    );
    assert_eq!(
        durability(&real.join("x/bin/atpkg"), &through_a_link),
        Durability::Transient
    );
    let at_the_root = temp_roots_from(PathBuf::from("/"));
    assert!(
        at_the_root.iter().all(|r| r.parent().is_some()),
        "{at_the_root:?}"
    );
    for scratch in ["/tmp/x/bin/atpkg", "/var/tmp/x/bin/atpkg"] {
        assert_eq!(
            durability(Path::new(scratch), &at_the_root),
            Durability::Transient,
            "{scratch}: {at_the_root:?}"
        );
    }
    assert_eq!(
        durability(
            Path::new("/Applications/aterm.app/Contents/MacOS/atpkg"),
            &at_the_root
        ),
        Durability::Bundle
    );
    let roots = temp_roots();
    assert_eq!(
        durability(&std::env::temp_dir().join("x/bin/atpkg"), &roots),
        Durability::Transient
    );
    assert_eq!(
        durability(
            Path::new("/Applications/aterm.app/Contents/MacOS/atpkg"),
            &roots
        ),
        Durability::Bundle
    );
    assert!(lasting(Path::new(
        "/Applications/aterm.app/Contents/MacOS/aterm"
    )));
    assert!(lasting(Path::new("/usr/local/lib/aterm/bin/aterm")));
    assert!(!lasting(&std::env::temp_dir().join("x/bin/aterm")));
    assert!(!lasting(&target.join("debug/aterm")));
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// [`Embedder::embed`], case by case: a missing atpkg is never named on either side; a
/// live one stays unless the offered one lasts at least as long; a gone one yields to
/// anything live.
#[cfg(unix)]
#[test]
fn the_embedder_names_no_missing_atpkg_and_never_trades_down() {
    let l = layout("embedder");
    let root = std::fs::canonicalize(&l.prefix).unwrap();
    let live = |rel: &str| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    };
    let installed = live("Applications/aterm.app/Contents/MacOS/atpkg");
    let dev = live("Applications/aterm (dev).app/Contents/MacOS/atpkg");
    let store = live("home/.local/lib/aterm/bin/atpkg");
    let scratch = live("tmp/run/bin/atpkg");
    let target = root.join("checkout/target");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(
        target.join("CACHEDIR.TAG"),
        format!("{CACHEDIR_TAG_SIGNATURE}\n"),
    )
    .unwrap();
    let target_build = live("checkout/target/debug/atpkg");
    let gone = root.join("gone/aterm.app/Contents/MacOS/atpkg");
    let not_executable = root.join("plain-file/atpkg");
    std::fs::create_dir_all(not_executable.parent().unwrap()).unwrap();
    std::fs::write(&not_executable, "not a program").unwrap();
    let scratch_roots = [root.join("tmp")];
    let embed = |offered: &Path, standing: Option<&Path>| {
        Embedder::offering(offered, &scratch_roots).embed(standing)
    };
    assert_eq!(embed(&gone, Some(&installed)), Some(installed.clone()));
    assert_eq!(embed(&gone, None), None);
    assert_eq!(embed(&gone, Some(&gone)), None);
    assert_eq!(embed(&not_executable, None), None);
    assert_eq!(
        embed(Path::new("atpkg"), None),
        None,
        "a relative path is never named"
    );
    assert_eq!(
        embed(&target_build, Some(&installed)),
        Some(installed.clone())
    );
    assert_eq!(embed(&scratch, Some(&installed)), Some(installed.clone()));
    assert_eq!(embed(&store, Some(&installed)), Some(installed.clone()));
    assert_eq!(embed(&scratch, Some(&store)), Some(store.clone()));
    assert_eq!(embed(&target_build, Some(&store)), Some(store.clone()));
    assert_eq!(
        embed(&installed, Some(&target_build)),
        Some(installed.clone())
    );
    assert_eq!(embed(&dev, Some(&installed)), Some(dev.clone()));
    assert_eq!(embed(&installed, Some(&dev)), Some(installed.clone()));
    assert_eq!(embed(&store, Some(&scratch)), Some(store.clone()));
    assert_eq!(embed(&installed, Some(&installed)), Some(installed.clone()));
    assert_eq!(embed(&installed, Some(&gone)), Some(installed.clone()));
    assert_eq!(
        embed(&target_build, Some(&gone)),
        Some(target_build.clone())
    );
    assert_eq!(embed(&installed, None), Some(installed.clone()));
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn the_named_atpkg_reads_back_as_written() {
    let t = tool("trust");
    for path in [
        "/Applications/aterm.app/Contents/MacOS/atpkg",
        "/Users//o'brien/Application Support/aterm/bin/atpkg",
        "/it's/''/x",
    ] {
        let body = stub_content_sh(&t, None, Path::new(path));
        assert_eq!(
            sh_named_atpkg(&body, "ATPKG"),
            Some(PathBuf::from(path)),
            "{body}"
        );
        assert_eq!(stub_named_atpkg(&body), Some(PathBuf::from(path)));
        let twin = crate::platform::sh_selfupdate_prelude(
            "claude",
            Path::new("/p"),
            Path::new(path),
            &["update"],
        );
        assert_eq!(
            sh_named_atpkg(&twin, "__atpkg"),
            Some(PathBuf::from(path)),
            "{twin}"
        );
        for strict in [false, true] {
            let reroute =
                crate::reroute::stub_body_sh("cargo", Path::new(path), Path::new("/r"), strict);
            assert_eq!(sh_named_atpkg(&reroute, "ATPKG"), Some(PathBuf::from(path)));
        }
    }
    let cmd = stub_content_cmd(&t, None, Path::new(r"C:\Program Files\aterm\atpkg.exe"));
    assert_eq!(
        stub_named_atpkg(&cmd),
        Some(PathBuf::from(r"C:\Program Files\aterm\atpkg.exe"))
    );
    let none = stub_content_sh(&t, None, named(None));
    assert!(none.contains("\nATPKG=''\n"), "{none}");
    assert_eq!(stub_named_atpkg(&none), None);
    let cmd_none = stub_content_cmd(&t, None, named(None));
    assert!(!cmd_none.contains("if exist"), "{cmd_none}");
    assert!(cmd_none.contains("where atpkg"), "{cmd_none}");
    assert_eq!(stub_named_atpkg(&cmd_none), None);
    assert_eq!(sh_named_atpkg("ATPKG=/unquoted/atpkg\n", "ATPKG"), None);
    assert_eq!(sh_named_atpkg("ATPKG='/open\n", "ATPKG"), None);
    assert_eq!(sh_named_atpkg("MY_ATPKG='/x'\n", "ATPKG"), None);
}

/// Make the three lays that run at every launch, session spawn and pass (the reroute
/// stubs, the pending stubs, the `agents/` twins) over `l` as a process running from
/// `exe` makes them ([`with_current_exe`]), with a stand-in placed at `exe` (and the
/// `atpkg` alias beside it when `alias`, as every bundle and install.sh's store carry)
/// if none is there yet.
#[cfg(unix)]
fn lay_as_run_from(exe: &Path, alias: bool, l: &Layout) {
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    if !exe.is_file() {
        std::fs::write(exe, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let atpkg = exe.with_file_name("atpkg");
    if alias && std::fs::symlink_metadata(&atpkg).is_err() {
        std::os::unix::fs::symlink(exe.file_name().unwrap(), &atpkg).unwrap();
    }
    with_current_exe(exe, || {
        assert_eq!(
            co_located_atpkg_path(),
            atpkg,
            "PRECONDITION: the lays offer the atpkg beside {}",
            exe.display()
        );
        crate::reroute::lay(l).expect("the reroute lay");
        lay_adoption_stubs(l, &crate::config::PackagesConfig::default());
        crate::activate::reconcile_agents(l);
    });
}

/// Every `(file, atpkg it names)` under `prefix`'s three laid directories: the value of
/// each file's `ATPKG=` line (a reroute or pending stub) or `__atpkg=` line (an agents
/// twin), as written.
#[cfg(unix)]
fn atpkg_named_under(prefix: &Path) -> Vec<(std::path::PathBuf, String)> {
    let l = Layout {
        prefix: prefix.to_path_buf(),
    };
    let mut named = Vec::new();
    for dir in [crate::reroute::dir(&l), l.bin_dir(), l.agents_dir()] {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(text) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            for line in text.lines() {
                let line = line.trim_start();
                if let Some(value) = line
                    .strip_prefix("ATPKG=")
                    .or_else(|| line.strip_prefix("__atpkg="))
                {
                    named.push((entry.path(), value.to_string()));
                }
            }
        }
    }
    named.sort();
    named
}

/// Rewrite every atpkg a laid file under `prefix` names to `atpkg`: the state a launch
/// of the installed app leaves behind.
#[cfg(unix)]
fn name_atpkg_everywhere(prefix: &Path, atpkg: &Path) {
    let quoted = sh_single_quote(&atpkg.to_string_lossy());
    for (file, _) in atpkg_named_under(prefix) {
        let text = std::fs::read_to_string(&file).unwrap();
        let mut out = String::new();
        for line in text.split_inclusive('\n') {
            let body = line.trim_start();
            let indent = &line[..line.len() - body.len()];
            let var = ["ATPKG=", "__atpkg="]
                .into_iter()
                .find(|var| body.starts_with(var));
            match var {
                Some(var) => {
                    out.push_str(indent);
                    out.push_str(var);
                    out.push_str(&quoted);
                    out.push('\n');
                }
                None => out.push_str(line),
            }
        }
        std::fs::write(&file, out).unwrap();
    }
}

/// The three real lays, made as a copy of aterm run from a scratch `bin/` with no
/// `atpkg` beside it makes them ([`lay_as_run_from`]), over a prefix whose every file
/// names the installed app's live atpkg: nothing is re-pointed at the scratch directory.
#[cfg(unix)]
#[test]
fn a_copy_run_from_a_scratch_dir_never_re_points_a_laid_file_at_a_missing_atpkg() {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("atpkg-lay-as-run-from-")
        .tempdir()
        .unwrap();
    let root = std::fs::canonicalize(scratch.path()).unwrap();
    let l = Layout {
        prefix: root.join("prefix"),
    };
    let installed = root.join("Applications/aterm.app/Contents/MacOS/atpkg");
    std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
    std::fs::write(&installed, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&installed, std::fs::Permissions::from_mode(0o755)).unwrap();
    let build = l.build_dir("claude", 2026092501);
    std::fs::create_dir_all(build.join("bin")).unwrap();
    std::fs::write(build.join("bin/claude"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        build.join("bin/claude"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    crate::activate::install_shims(
        &l,
        &build,
        &["claude".to_string()],
        crate::activate::Aliases::Off,
    )
    .unwrap();
    crate::store::mark_build_ready(&build).unwrap();
    crate::reroute::lay(&l).unwrap();
    lay_adoption_stubs(&l, &crate::config::PackagesConfig::default());
    crate::activate::reconcile_agents(&l);
    name_atpkg_everywhere(&l.prefix, &installed);
    let before = atpkg_named_under(&l.prefix);
    let quoted = sh_single_quote(&installed.to_string_lossy());
    assert!(
        before.iter().all(|(_, v)| *v == quoted),
        "the fixture names the installed atpkg everywhere: {before:#?}"
    );
    let reroute_dir = crate::reroute::dir(&l);
    for (what, file) in [
        ("a reroute stub", reroute_dir.join("cargo")),
        ("a pending stub", l.shim(&tool("ty"))),
        ("the agents twin", l.agent_shim(&tool("claude"))),
    ] {
        assert!(
            before.iter().any(|(f, _)| *f == file),
            "{what} names an atpkg: {before:#?}"
        );
    }

    let scratch = root.join("scratch/bin/aterm");
    lay_as_run_from(&scratch, false, &l);
    let after = atpkg_named_under(&l.prefix);
    let scratch_dir = scratch.parent().unwrap().to_string_lossy().into_owned();
    let leaked: Vec<_> = after
        .iter()
        .filter(|(_, v)| v.contains(&scratch_dir))
        .collect();
    assert!(
        leaked.is_empty(),
        "a copy run from a scratch dir re-pointed laid files at an atpkg that is not \
         there: {leaked:#?}"
    );
    assert_eq!(
        after, before,
        "every laid file still names the installed atpkg"
    );

    std::fs::remove_file(&installed).unwrap();
    let moved = root.join("Moved/aterm.app/Contents/MacOS/aterm");
    lay_as_run_from(&moved, true, &l);
    let moved_atpkg = sh_single_quote(&moved.with_file_name("atpkg").to_string_lossy());
    let relaid = atpkg_named_under(&l.prefix);
    assert_eq!(relaid.len(), before.len(), "{relaid:#?}");
    assert!(
        relaid.iter().all(|(_, v)| *v == moved_atpkg),
        "the moved app re-lays every file naming its own atpkg: {relaid:#?}"
    );
    let fresh = Layout {
        prefix: root.join("fresh"),
    };
    lay_as_run_from(&moved, true, &fresh);
    let laid = atpkg_named_under(&fresh.prefix);
    assert!(
        laid.iter()
            .any(|(f, _)| *f == crate::reroute::dir(&fresh).join("cargo")),
        "{laid:#?}"
    );
    assert!(
        laid.iter().all(|(_, v)| *v == moved_atpkg),
        "a fresh install names its own atpkg: {laid:#?}"
    );
}
