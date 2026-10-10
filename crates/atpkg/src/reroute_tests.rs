// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[cfg(unix)]
use crate::test_programs::{assessed, programs_dir};
use crate::upstream_identity::{Identity, Picked, Rustup, Vv};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;

fn layout(label: &str) -> crate::testdir::ScratchLayout {
    crate::testdir::layout(&format!("atpkg-reroute-{label}-"))
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// Replace whatever is at `at` with `bytes`, or with a symlink to `target`, by rename,
/// so the directory entry is the old one or the new one, never neither. That is NOT
/// safe under a concurrent run when the entry replaced is a SYMLINK: a call that reads
/// it while it is renamed over — `readlink`, or a path walk following it — fails
/// EINVAL ([`managed_target`] has the measurement), so a shared symlink is laid only
/// when it is wrong.
#[cfg(unix)]
fn lay_by_rename(at: &Path, bytes: Option<&str>, target: Option<&Path>) {
    static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let call = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = at.with_extension(format!("tmp-{}-{call}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    match (bytes, target) {
        (Some(bytes), _) => std::fs::write(&tmp, bytes).unwrap(),
        (None, Some(target)) => std::os::unix::fs::symlink(target, &tmp).unwrap(),
        (None, None) => unreachable!("bytes or a target"),
    }
    std::fs::rename(&tmp, at).unwrap();
}

/// The ONE fake program: it prints the label on the first line of the `.who` file
/// beside the name it was run by, then its arguments, and exits with the code on that
/// file's second line (0 when there is none).
#[cfg(unix)]
const FAKE: &str = "#!/bin/sh\n{ IFS= read -r who; IFS= read -r code; } < \"$0.who\"\n\
                    echo \"$who: $*\"\nexit \"${code:-0}\"\n";

/// [`FAKE`] placed at `at` (a symlink, replacing whatever was there), labelled `who`
/// (a second line is its exit code).
#[cfg(unix)]
fn place_fake(at: &Path, who: &str) -> PathBuf {
    std::fs::create_dir_all(at.parent().unwrap()).unwrap();
    let _ = std::fs::remove_file(at);
    std::fs::write(format!("{}.who", at.display()), format!("{who}\n")).unwrap();
    std::os::unix::fs::symlink(assessed("fake", FAKE), at).unwrap();
    at.to_path_buf()
}

/// `stub` run the way the kernel runs its `#!/bin/sh` line — see [`assessed`]. Only a
/// stub the kernel WOULD hand to `/bin/sh`: executable, first line `#!/bin/sh`. So the
/// one thing this changes is which process pays the first-exec assessment.
#[cfg(unix)]
fn sh(stub: &Path) -> std::process::Command {
    let mode = std::fs::metadata(stub).unwrap().permissions().mode();
    assert_ne!(mode & 0o111, 0, "{} is not executable", stub.display());
    assert!(
        std::fs::read(stub).unwrap().starts_with(b"#!/bin/sh\n"),
        "{} is not a #!/bin/sh script",
        stub.display()
    );
    let mut cmd = std::process::Command::new("/bin/sh");
    // No marker leaks in from the run's own environment (a `cargo test` the reroute ran
    // carries [`ANNOUNCED_ENV`]; a doctest, cargo's compile marks); a test that wants one
    // sets it after.
    cmd.arg(stub)
        .env_remove(PASSTHROUGH_ENV)
        .env_remove(ANNOUNCED_ENV)
        .env_remove(CARGO_CRATE_ENV)
        .env_remove(TARGO_CRATE_ENV);
    cmd
}

/// `lean f.lean` must reach `clean check f.lean` — the branded tool is
/// verb-first, so plain passthrough spelled `clean f.lean` and died on
/// "unrecognized subcommand". Every other shape stays byte-identical: a
/// flag, one of `clean`'s OWN subcommands (the reason this is keyed on the
/// extension and not on "not a flag"), an already-explicit verb, and every
/// DIRECT row that declares no `SourceVerb`.
#[test]
fn a_verb_first_direct_row_routes_a_bare_source_file_through_its_verb() {
    let lean = row_for("lean").expect("lean is a rerouted name");
    let Policy::Direct { source_verb, .. } = lean.policy else {
        panic!("lean is a DIRECT row");
    };
    let sv = source_verb.expect("lean declares a source verb");

    assert_eq!(
        direct_args(source_verb, &args(&["f.lean"])),
        args(&["check", "f.lean"])
    );
    assert_eq!(
        direct_args(source_verb, &args(&["a/b/Main.lean", "--json"])),
        args(&["check", "a/b/Main.lean", "--json"])
    );
    // Untouched: a flag, one of clean's own subcommands, an explicit verb.
    for passthrough in [
        vec!["--version"],
        vec!["repl"],
        vec!["check", "f.lean"],
        vec![],
    ] {
        assert_eq!(
            direct_args(source_verb, &args(&passthrough)),
            args(&passthrough),
            "{passthrough:?} must pass through unchanged"
        );
    }
    assert_eq!(sv.verb, "check");
    assert_eq!(sv.ext, ".lean");

    // A row with no source verb is pure identity.
    let clippy = row_for("clippy").expect("clippy is a rerouted name");
    let Policy::Direct {
        source_verb: none, ..
    } = clippy.policy
    else {
        panic!("clippy is a DIRECT row");
    };
    assert!(none.is_none());
    assert_eq!(direct_args(none, &args(&["x.rs"])), args(&["x.rs"]));
}

/// The table is the policy: no name twice, every branded target a name the
/// managed `bin/` may carry, and the three names the deny-list refuses STILL
/// refused — the reroute dir is not a loophole into `bin/`.
#[test]
fn table_is_unique_branded_targets_are_managed_names_and_sensitive_names_stay_sensitive() {
    let mut seen = std::collections::BTreeSet::new();
    for row in TABLE {
        assert!(seen.insert(row.upstream), "duplicate row {}", row.upstream);
        assert!(
            crate::store::ToolName::new(branded_of(row)).is_some(),
            "{} is not a name the managed bin/ may carry",
            branded_of(row)
        );
    }
    for sensitive in ["cargo", "rustc", "rustup"] {
        assert!(
            crate::store::ToolName::new(sensitive).is_none(),
            "{sensitive} left the deny-list"
        );
    }
    assert!(
        row_for("rustup").is_none(),
        "rustup is not rerouted: `rustup run trust` is the sanctioned spelling"
    );
}

#[test]
fn engaged_is_non_empty_and_not_zero() {
    assert!(!engaged(None));
    assert!(!engaged(Some("")));
    assert!(!engaged(Some("0")));
    assert!(engaged(Some("1")));
    assert!(engaged(Some("yes")));
}

#[test]
fn explicit_toolchain_is_only_a_leading_plus_argument() {
    assert_eq!(
        explicit_toolchain(&args(&["+stable", "build"])),
        Some("stable")
    );
    assert_eq!(explicit_toolchain(&args(&["build", "+stable"])), None);
    assert_eq!(explicit_toolchain(&args(&["+"])), None);
    assert_eq!(explicit_toolchain(&[]), None);
}

#[test]
fn signpost_fills_the_callers_arguments_into_both_lanes() {
    let row = row_for("cargo").unwrap();
    let Policy::Signpost { lanes, .. } = row.policy else {
        panic!()
    };
    let text = signpost_message_in(row, lanes, &args(&["build", "--release"]), None, None, None);
    assert!(
        text.starts_with(
            "aterm: running upstream 'cargo' (no proof claim); on Trust the tool is 'targo':\n"
        ),
        "{text}"
    );
    assert!(text.contains("targo trust build --release"), "{text}");
    assert!(
        text.contains("targo --unverified build --release"),
        "{text}"
    );
    assert!(text.contains("VERIFIED   — emits a proof claim"), "{text}");
    // Every announce-and-run signpost names the page that MEASURES the
    // answer, and says the default (owner, 2026-09-08).
    assert!(
        text.contains("`aterm help rust` shows which toolchain this directory gets"),
        "{text}"
    );
    assert!(text.contains("the default here is Trust"), "{text}");
    assert!(text.contains("UNVERIFIED — no proof claim"), "{text}");
    // ANNOUNCE, NOT REFUSE: the default says upstream is about to run (the
    // first line) and names the one setting that silences it — never an
    // environment variable.
    assert_eq!(text.lines().count(), 5, "{text}");
    assert!(
        text.ends_with(
            "([reroute] announce = false in aterm.toml silences this — Settings ▸ Packages.)"
        ),
        "{text}"
    );
    assert!(
        !text.contains("ATERM_"),
        "no environment knob is taught: {text}"
    );
    // No arguments: `targo` alone (which prints its usage), no trailing space —
    // not two lanes around no verb (`targo trust` alone is its own help page).
    let bare = signpost_message_in(row, lanes, &[], None, None, None);
    assert!(bare.contains("\n         targo\n"), "{bare}");
    assert!(!bare.contains("targo trust"), "{bare}");
}

#[test]
fn cargo_clippy_and_fmt_are_one_spelling_with_no_lane() {
    let row = row_for("cargo").unwrap();
    let Policy::Signpost { lanes, .. } = row.policy else {
        panic!()
    };
    let text = signpost_message_in(
        row,
        lanes,
        &args(&["clippy", "--workspace", "--", "-D", "warnings"]),
        None,
        None,
        None,
    );
    assert!(
        text.starts_with(
            "aterm: running upstream 'cargo clippy'; on Trust the tool is 'targo tippy':\n"
        ),
        "{text}"
    );
    assert!(
        text.contains("targo tippy --workspace -- -D warnings"),
        "{text}"
    );
    assert!(!text.contains("targo trust"), "{text}");
    let text = signpost_message_in(row, lanes, &args(&["fmt", "--check"]), None, None, None);
    assert!(text.contains("targo fmt --check"), "{text}");
}

#[test]
fn rustc_names_both_lanes_and_tlc_names_one_tool_without_claiming_equivalence() {
    let rustc = row_for("rustc").unwrap();
    let Policy::Signpost { lanes, .. } = rustc.policy else {
        panic!()
    };
    let text = signpost_message_in(rustc, lanes, &args(&["main.rs"]), None, None, None);
    assert!(text.contains("trustc main.rs"), "{text}");
    assert!(text.contains("trustc -Ztrust-verify=off main.rs"), "{text}");
    let tlc = row_for("tlc").unwrap();
    let Policy::Signpost { lanes, .. } = tlc.policy else {
        panic!()
    };
    let text = signpost_message_in(tlc, lanes, &args(&["Spec.tla"]), None, None, None);
    assert!(
        text.starts_with(
            "aterm: running upstream 'tlc' (no proof claim); on this toolchain the tool is 'ty'\n"
        ),
        "{text}"
    );
    assert!(
        text.contains("drop-in equivalence is not yet proven"),
        "{text}"
    );
    assert!(text.contains("\n         ty Spec.tla\n"), "{text}");
}

#[test]
fn direct_and_oracle_are_one_line_each_and_name_the_escape() {
    let no_copy = oracle_message("z3", None);
    for text in [
        direct_announcement("rustfmt", "trustfmt", true),
        oracle_message("z3", Some(Path::new("/opt/homebrew/bin/z3"))),
        no_copy.clone(),
        unreachable_message("cargo"),
    ] {
        assert_eq!(text.lines().count(), 1, "{text}");
        assert!(text.starts_with("aterm: "), "{text}");
        assert!(
            text == no_copy || text.contains("aterm --no-reroute"),
            "{text}"
        );
        assert!(
            !text.contains("ATERM_"),
            "no environment knob is taught: {text}"
        );
    }
    assert!(
        direct_announcement("rustfmt", "trustfmt", true).starts_with("aterm: running trustfmt, "),
        "a DIRECT line leads with what runs"
    );
    // Stock Rust ships no `clippy`: with no upstream copy on PATH, no escape offered.
    assert_eq!(
        direct_announcement("clippy", "tippy", false),
        "aterm: running tippy, Trust's 'clippy'."
    );
    assert_eq!(
        unreachable_message("cargo"),
        "aterm: 'cargo' did not run — aterm's package manager is missing (`aterm --no-reroute` restores upstream 'cargo')"
    );
    assert!(
        oracle_message("z3", Some(Path::new("/opt/homebrew/bin/z3")))
            .contains("run /opt/homebrew/bin/z3 to reach the real z3"),
        "the ORACLE refusal names the path that is the consent"
    );
    // With no upstream copy on PATH, an `aterm --no-reroute` session has none
    // either: the escape is not offered as the fix.
    assert!(!no_copy.contains("aterm --no-reroute"), "{no_copy}");
    assert!(no_copy.contains("no upstream 'z3' is on PATH"), "{no_copy}");
    // The 2026-09-29 lines — a warning before something runs, or why nothing could:
    // one line each, no environment knob, and never a refusal's words.
    for text in [
        oracle_notice("z3", "ay", None),
        oracle_notice("z3", "ay", Some("`aterm pkg install ay` installs it")),
        oracle_not_on_path_message("z3", "ay", None),
        fallback_not_installed_message("clippy", "tippy", "trust"),
        fallback_lost_shim_message("rustfmt", "trustfmt"),
        unreachable_fallback_message("cargo"),
        unreachable_not_on_path_message("cargo"),
        storeless_message("cargo", &[], None),
        storeless_message(
            "rustc",
            &args(&["+1.97.1"]),
            Some(&stock_at("rustc", "1.97.1", Picked::CommandLine)),
        ),
    ] {
        assert_eq!(text.lines().count(), 1, "{text}");
        assert!(text.starts_with("aterm: "), "{text}");
        assert!(!text.contains("ATERM_"), "{text}");
        assert!(!text.contains("refus"), "{text}");
    }
    assert!(
        oracle_notice("z3", "ay", None).contains("'ay'"),
        "names the preferred route"
    );
}

/// A second `lay` may be in flight: its `.<name>.stub-<pid>` temp and the
/// marker file are dotfiles, and dotfiles are never swept.
#[test]
fn the_sweep_never_touches_dotfiles() {
    let l = layout("dotfiles");
    lay(&l).unwrap();
    let d = dir(&l);
    std::fs::write(
        d.join(".cargo.stub-99999"),
        format!("#!/bin/sh\n{STUB_MARKER}\nexit 2\n"),
    )
    .unwrap();
    lay(&l).unwrap();
    assert!(
        d.join(".cargo.stub-99999").exists(),
        "a concurrent lay's temp was swept"
    );
    assert!(is_reroute_dir(&d), "the marker file was swept");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// The stub is invisible to every managed-shim sweep and carries its marker, in both
/// modes; only its last resort differs — upstream then 127 by default, exit 2 strict.
#[test]
fn stub_body_is_marked_and_never_parses_as_a_managed_shim() {
    for strict in [false, true] {
        let body = stub_body_sh(
            "cargo",
            Path::new("/opt/aterm/atpkg"),
            Path::new("/p/reroute"),
            strict,
        );
        assert_eq!(body.lines().nth(1), Some(STUB_MARKER));
        assert!(
            crate::platform::parse_sh_shim_target(&body).is_none(),
            "{body}"
        );
        assert!(!body.contains("exec '"), "{body}");
        assert!(
            body.contains("__reroute \"$__aterm_name\" \"$@\""),
            "{body}"
        );
        assert!(body.contains(PASSTHROUGH_ENV), "{body}");
        assert!(body.len() < MAX_STUB_BYTES, "{}", body.len());
        if strict {
            assert!(body.ends_with("exit 2\n"), "{body}");
            assert!(!body.contains("running upstream"), "{body}");
        } else {
            assert!(body.ends_with("exit 127\n"), "{body}");
            assert!(
                body.contains(&format!("{ANNOUNCED_ENV}=1; export {ANNOUNCED_ENV}")),
                "the fallback marks its child so cargo's rustc passes silently: {body}"
            );
            assert!(
                !body.contains(&format!("{PASSTHROUGH_ENV}=1")),
                "and never with the escape: {body}"
            );
        }
    }
    // The escape's walk is the same bytes in both modes: strict changes only the tail.
    let head = |strict| {
        let body = stub_body_sh("cargo", Path::new("/a"), Path::new("/p"), strict);
        body[..body.find("ATPKG=").unwrap()].to_string()
    };
    assert_eq!(head(false), head(true));
}

/// A lay follows `[reroute] strict`: flipping it rewrites every row's stub, and
/// flipping it back restores the bytes.
#[cfg(unix)]
#[test]
fn lay_carries_strict_into_every_row_stub() {
    let l = layout("strict-lay");
    let embedder = crate::stub::Embedder::offering(&l.prefix.join("atpkg"), &[]);
    let tails = |l: &Layout| -> Vec<bool> {
        TABLE
            .iter()
            .map(|row| {
                std::fs::read_to_string(dir(l).join(row.upstream))
                    .unwrap()
                    .ends_with("exit 2\n")
            })
            .collect()
    };
    lay_with(&l, &embedder, false).unwrap();
    assert_eq!(tails(&l), vec![false; TABLE.len()], "default: never refuse");
    lay_with(&l, &embedder, true).unwrap();
    assert_eq!(tails(&l), vec![true; TABLE.len()], "strict: refuse");
    lay_with(&l, &embedder, false).unwrap();
    assert_eq!(tails(&l), vec![false; TABLE.len()], "and back");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// The marker is written only when absent or different in content.
#[test]
fn the_marker_is_written_only_when_its_bytes_differ() {
    assert!(marker_needs_lay(None), "absent: laid");
    assert!(
        !marker_needs_lay(Some(marker_body().as_bytes())),
        "identical: left"
    );
    assert!(
        marker_needs_lay(Some(b"# something else\n")),
        "different: rewritten"
    );
}

/// A second `lay` over a laid directory writes NOTHING: every stub and the marker keep
/// their inodes, whatever tag they carry — the steady-state pass and every session
/// spawn converge.
#[cfg(unix)]
#[test]
fn a_second_lay_writes_nothing() {
    use std::os::unix::fs::MetadataExt as _;
    let l = layout("idempotent");
    lay(&l).unwrap();
    let d = dir(&l);
    let inodes = |d: &Path| -> Vec<(String, u64)> {
        let mut v: Vec<(String, u64)> = std::fs::read_dir(d)
            .unwrap()
            .flatten()
            .map(|e| {
                (
                    e.file_name().to_string_lossy().into_owned(),
                    e.metadata().unwrap().ino(),
                )
            })
            .collect();
        v.sort();
        v
    };
    let before = inodes(&d);
    assert!(
        before.iter().any(|(n, _)| n == DIR_MARKER_FILE),
        "{before:?}"
    );
    lay(&l).unwrap();
    assert_eq!(inodes(&d), before, "nothing was rewritten");
    // A stub whose body drifted is rewritten, and only it.
    let cargo = d.join("cargo");
    let body = std::fs::read_to_string(&cargo).unwrap();
    std::fs::write(&cargo, body.replace("ATPKG=", "ATPKG_OLD=")).unwrap();
    let drifted = inodes(&d);
    lay(&l).unwrap();
    let after = inodes(&d);
    for ((name, was), (_, now)) in drifted.iter().zip(&after) {
        assert_eq!(name == "cargo", was != now, "{name}");
    }
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn lay_claims_absent_and_own_files_never_foreign_ones_and_remove_all_sweeps_only_ours() {
    let l = layout("lay");
    let d = dir(&l);
    std::fs::create_dir_all(&d).unwrap();
    // A foreign occupant and a stale stub of ours for a name that left the table.
    std::fs::write(d.join("rustc"), "#!/bin/sh\necho someone else's\n").unwrap();
    std::fs::write(
        d.join("gone"),
        format!("#!/bin/sh\n{STUB_MARKER}\nexit 2\n"),
    )
    .unwrap();
    lay(&l).unwrap();
    for row in TABLE {
        let path = d.join(row.upstream);
        if row.upstream == "rustc" {
            assert!(!is_reroute_stub(&path), "foreign file was overwritten");
        } else {
            assert!(is_reroute_stub(&path), "{} not laid", row.upstream);
            #[cfg(unix)]
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o111,
                0o111
            );
        }
    }
    assert!(!d.join("gone").exists(), "stale stub survived reconcile");
    let states = states(&l);
    assert!(
        states
            .iter()
            .any(|(n, s)| *n == "rustc" && *s == StubState::Foreign)
    );
    assert!(
        states
            .iter()
            .any(|(n, s)| *n == "cargo" && *s == StubState::Laid)
    );
    assert!(is_reroute_dir(&d), "lay must leave the marker file");
    // Laying again is idempotent.
    lay(&l).unwrap();
    remove_all(&l);
    assert!(d.join("rustc").exists(), "foreign file swept");
    assert!(!d.join("cargo").exists());
    assert!(!is_reroute_dir(&d), "remove_all must take the marker file");
    assert!(d.exists(), "a non-empty directory (the foreign file) stays");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// The CPU time a pid has consumed, as `ps` spells it, or `None` when `ps`
/// cannot answer. Read ONLY to describe a timeout — never to decide one.
#[cfg(unix)]
fn child_cpu_time(pid: u32) -> Option<String> {
    let out = std::process::Command::new("/bin/ps")
        .args(["-o", "time=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!t.is_empty()).then_some(t)
}

/// Run `cmd`, killing it after `secs` — an exec loop must fail this test,
/// not hang it.
#[cfg(unix)]
fn output_within(mut cmd: std::process::Command, secs: u64) -> std::process::Output {
    use std::io::Read as _;
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            child
                .stdout
                .take()
                .unwrap()
                .read_to_end(&mut stdout)
                .unwrap();
            child
                .stderr
                .take()
                .unwrap()
                .read_to_end(&mut stderr)
                .unwrap();
            return std::process::Output {
                status,
                stdout,
                stderr,
            };
        }
        if std::time::Instant::now() > deadline {
            // A BOUND ON OUR PATIENCE IS NOT A FACT ABOUT THE STUB.
            //
            // This used to panic with "an exec loop" — a diagnosis nothing
            // here had measured. The bound is a minute at every call, and the two
            // things that exhaust it are NOT alike: an exec loop replaces
            // the image in the SAME pid over and over and burns CPU, while
            // a stub the test has just written can sit at ~0% CPU for
            // seconds while macOS `syspolicyd` assesses it under load (a
            // recorded hazard on this machine). Calling the second one an
            // exec loop sends the reader hunting a bug that is not there.
            //
            // So the pid is ASKED before it is killed. `ps -o time=` is the
            // CPU the process has actually consumed; it is the one cheap
            // measurement that separates spinning from blocked.
            let burned = child_cpu_time(child.id());
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "stub did not exit within {secs}s (cpu {}). Spinning CPU means an exec \
                 loop — the defect this test is about. Near-zero CPU means it is \
                 BLOCKED, not looping: on macOS the usual cause is Gatekeeper assessing \
                 a script this test wrote moments ago, which says nothing about the \
                 reroute. Re-run this test alone before believing the first reading.",
                burned.as_deref().unwrap_or("unknown")
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// The 2026-09-07 review's reproduction: with a SECOND reroute directory on
/// PATH — another prefix's, a symlink to ours, ours with a trailing slash —
/// the escape must still reach upstream, not exec the other stub forever.
#[cfg(unix)]
#[test]
fn escape_walk_skips_every_reroute_directory_not_just_its_own() {
    let l = layout("second-spelling");
    let other = layout("second-spelling-other-prefix");
    lay(&l).unwrap();
    lay(&other).unwrap();
    // The upstream copy lives OUTSIDE every manager prefix, as it does on a
    // real machine (~/.cargo/bin); a walk skips anything under a prefix.
    let upstream_dir = crate::testdir::dir("atpkg-reroute-upstream-");
    place_fake(&upstream_dir.join("cargo"), "upstream");
    let link = l.prefix.join("link-to-reroute");
    std::os::unix::fs::symlink(dir(&l), &link).unwrap();
    let mut trailing = dir(&l).into_os_string();
    trailing.push("/");
    for (label, spellings) in [
        ("second prefix", vec![dir(&other).into_os_string()]),
        ("symlink", vec![link.clone().into_os_string()]),
        ("trailing slash", vec![trailing.clone()]),
    ] {
        let mut entries = spellings.clone();
        entries.push(dir(&l).into_os_string());
        entries.push(upstream_dir.clone().into_os_string());
        let path_env = std::env::join_paths(entries).unwrap();
        let mut cmd = sh(&dir(&l).join("cargo"));
        cmd.args(["build"])
            .env("PATH", &path_env)
            .env(PASSTHROUGH_ENV, "1");
        // 60 s, not 5: the bound's only job is to tell "wedged" from
        // "slow", and an exec loop never finishes, so a longer wait costs
        // this test nothing when it is right and removes nearly all of the
        // window in which a loaded machine can be accused of one.
        let out = output_within(cmd, 60);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{label}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "upstream: build\n",
            "{label}"
        );
        // And the Rust side's walk agrees.
        assert_eq!(
            upstream_on_path(&l, "cargo", Some(&path_env)),
            Some(upstream_dir.join("cargo")),
            "{label}"
        );
    }
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&other.prefix);
    let _ = std::fs::remove_dir_all(&upstream_dir);
}

/// A TABLE stub under ANOTHER NAME in an ordinary PATH directory — `ln -s
/// <prefix>/reroute/cargo ~/bin/cargo`, or a hard link — has no
/// [`DIR_MARKER_FILE`] beside it, so the escape walk used to exec it, and it
/// exec'd itself again, forever. Only knowing it for a stub stops that — by `-ef
/// "$0"` until the 2026-09-24 review showed a COPY slips past an inode test
/// (`escape_walk_never_ping_pongs_between_a_copy_and_a_link`), by its content since
/// ([`sh_skip_a_stub`]). Entered by either spelling, the walk
/// reaches the upstream copy behind it, or — with none — exits 127 with its own
/// line. Every TABLE name.
#[cfg(unix)]
#[test]
fn escape_walk_never_execs_the_stub_under_another_name() {
    let l = layout("self-link");
    lay(&l).unwrap();
    // Both outside the manager prefix, as `~/bin` and `~/.cargo/bin` are, so the
    // Rust side's walk has to recognize the link for what it is too.
    let outside = |what: &str| crate::testdir::dir(&format!("atpkg-reroute-self-link-{what}-"));
    let user_bin = outside("user-bin");
    let upstream_dir = outside("upstream");
    for row in TABLE {
        let name = row.upstream;
        let stub = dir(&l).join(name);
        if name == "cargo" {
            std::fs::hard_link(&stub, user_bin.join(name)).unwrap();
        } else {
            std::os::unix::fs::symlink(&stub, user_bin.join(name)).unwrap();
        }
        fake(&upstream_dir, name, "upstream");
        let with_upstream = std::env::join_paths([&user_bin, &dir(&l), &upstream_dir]).unwrap();
        let without = std::env::join_paths([&user_bin, &dir(&l)]).unwrap();
        for entry in [user_bin.join(name), stub.clone()] {
            let run = |path: &std::ffi::OsStr| {
                let mut cmd = std::process::Command::new(&entry);
                cmd.args(["--version"])
                    .env("PATH", path)
                    .env(PASSTHROUGH_ENV, "1");
                // 60 s, as the walk test above: an exec loop never finishes.
                output_within(cmd, 60)
            };
            let out = run(&with_upstream);
            assert_eq!(
                (
                    out.status.code(),
                    String::from_utf8_lossy(&out.stdout).into_owned()
                ),
                (Some(0), "upstream: --version\n".to_string()),
                "{name} via {}: {}",
                entry.display(),
                String::from_utf8_lossy(&out.stderr)
            );
            let out = run(&without);
            assert_eq!(
                out.status.code(),
                Some(127),
                "{name} via {}",
                entry.display()
            );
            assert_eq!(
                String::from_utf8_lossy(&out.stderr),
                format!("aterm: upstream '{name}' is not on PATH\n"),
                "{name} via {}",
                entry.display()
            );
            assert!(out.stdout.is_empty(), "{name}");
        }
        // And the Rust side's walk agrees.
        assert_eq!(
            upstream_on_path(&l, name, Some(&with_upstream)),
            Some(upstream_dir.join(name)),
            "{name}"
        );
    }
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&user_bin);
    let _ = std::fs::remove_dir_all(&upstream_dir);
}

/// A COPY OF A STUB IS A STUB (review, 2026-09-24). `cp <prefix>/reroute/* ~/bin/`
/// copies every stub and — the glob skips a dotfile — no [`DIR_MARKER_FILE`], and a
/// copy has its own inode, so `-ef "$0"` never matched it: with `reroute : ~/a (a copy)
/// : ~/b (a link to the stub) : upstream` the stub exec'd the copy, the copy exec'd the
/// link (the stub again, under `$0 = ~/b/<name>`), which exec'd the copy, forever —
/// and two copies the same (measured: `cargo` spun 16 s until killed, and `claude`
/// outside aterm too). The walk has to know a stub the way [`upstream_on_path`] does,
/// by what it SAYS. Entered by the stub, the copy or the second entry, a TABLE name
/// under the escape and an agent program outside aterm reach the user's own copy
/// behind both — or, with none, exit 127 with their own line — and the Rust walk
/// agrees.
#[cfg(unix)]
#[test]
fn escape_walk_never_ping_pongs_between_a_copy_and_a_link() {
    let l = layout("copy-and-link");
    fake_twin(&l, "claude");
    lay(&l).unwrap();
    let outside = |what: &str| crate::testdir::dir(&format!("atpkg-reroute-copy-link-{what}-"));
    let upstream_dir = outside("upstream");
    for name in ["cargo", "claude"] {
        let stub = dir(&l).join(name);
        assert!(is_reroute_stub(&stub), "{name}");
        fake(&upstream_dir, name, "upstream");
        // The escape for a TABLE name; for an agent program, simply outside aterm
        // (`run_stub` clears every marker).
        let escape: &[(&str, &str)] = if name == "cargo" {
            &[(PASSTHROUGH_ENV, "1")]
        } else {
            &[]
        };
        let missing = if name == "cargo" {
            format!("aterm: upstream '{name}' is not on PATH")
        } else {
            agents_not_found_message(name)
        };
        for (label, second_is_a_link) in [("a copy then a link", true), ("two copies", false)] {
            let a = outside(&format!("{name}-a"));
            let b = outside(&format!("{name}-b"));
            std::fs::copy(&stub, a.join(name)).unwrap();
            if second_is_a_link {
                std::os::unix::fs::symlink(&stub, b.join(name)).unwrap();
            } else {
                std::fs::copy(&stub, b.join(name)).unwrap();
            }
            let with_upstream = std::env::join_paths([&dir(&l), &a, &b, &upstream_dir]).unwrap();
            let without = std::env::join_paths([&dir(&l), &a, &b]).unwrap();
            for entry in [stub.clone(), a.join(name), b.join(name)] {
                let out = run_stub(&entry, &with_upstream, escape);
                assert_eq!(
                    (
                        out.status.code(),
                        String::from_utf8_lossy(&out.stdout).into_owned()
                    ),
                    (Some(0), "upstream: --version x y\n".to_string()),
                    "{name}, {label}, via {}: {}",
                    entry.display(),
                    String::from_utf8_lossy(&out.stderr)
                );
                let out = run_stub(&entry, &without, escape);
                assert_eq!(
                    out.status.code(),
                    Some(127),
                    "{name}, {label}, via {}",
                    entry.display()
                );
                assert_eq!(
                    String::from_utf8_lossy(&out.stderr).trim_end(),
                    missing,
                    "{name}, {label}, via {}",
                    entry.display()
                );
                assert!(out.stdout.is_empty(), "{name}, {label}");
            }
            // The Rust side's walk has always known a copy by its content.
            assert_eq!(
                upstream_on_path(&l, name, Some(&with_upstream)),
                Some(upstream_dir.join(name)),
                "{name}, {label}"
            );
            let _ = std::fs::remove_dir_all(&a);
            let _ = std::fs::remove_dir_all(&b);
        }
    }
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&upstream_dir);
}

/// A recorded decline is durable: `lay` lays nothing, and doctor/which see
/// `Missing`, until the decline is lifted.
#[test]
fn a_recorded_decline_stops_lay() {
    let l = layout("declined");
    std::fs::write(l.declined(), b"# removed on purpose\n").unwrap();
    lay(&l).unwrap();
    assert!(!dir(&l).exists(), "a declined toolset must not get stubs");
    assert!(states(&l).iter().all(|(_, s)| *s == StubState::Missing));
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// `cargo trust check` named the lane already: one line, then `targo trust check`.
#[test]
fn an_explicit_lane_under_the_rust_name_is_run_not_refused() {
    assert!(
        EXPLICIT_LANE_VERBS.contains(&"trust") && EXPLICIT_LANE_VERBS.contains(&"--unverified")
    );
    // No escape: stock cargo has no `trust` subcommand, so upstream would not run it.
    assert_eq!(
        explicit_lane_note("cargo", "targo"),
        "aterm: running targo, Trust's 'cargo'."
    );
}

/// A managed copy at `/store/<tool>`, declaring no environment.
fn store(tool: &str) -> Managed {
    Managed {
        target: PathBuf::from(format!("/store/{tool}")),
        env: crate::shim_env::ShimEnv::NONE,
    }
}

/// [`plan`] for `name call…` on a machine whose PATH walk finds `upstream` and whose
/// store holds the `branded` names ([`store`]).
fn plan_on(
    name: &str,
    call: &[&str],
    announce: bool,
    upstream: Option<&str>,
    branded: &[&str],
) -> Plan {
    plan_pinned(name, call, announce, upstream, branded, None)
}

/// [`plan_on`] in a directory whose pinned channel is `pin` ([`Pin::for_dir`]).
fn plan_pinned(
    name: &str,
    call: &[&str],
    announce: bool,
    upstream: Option<&str>,
    branded: &[&str],
    pin: Option<Pin>,
) -> Plan {
    plan_with(name, call, announce, false, upstream, branded, pin)
}

/// [`plan_on`] under `[reroute] strict = true` — the refusals of before 2026-09-29.
fn plan_strict(name: &str, call: &[&str], upstream: Option<&str>, branded: &[&str]) -> Plan {
    plan_with(name, call, true, true, upstream, branded, None)
}

/// [`plan_pinned`] with `[reroute] strict` given.
fn plan_with(
    name: &str,
    call: &[&str],
    announce: bool,
    strict: bool,
    upstream: Option<&str>,
    branded: &[&str],
    pin: Option<Pin>,
) -> Plan {
    let machine = Machine {
        passthrough: false,
        announced: false,
        announce: &|| announce,
        strict: &|| strict,
        upstream: &|| upstream.map(PathBuf::from),
        branded: &|tool, _| {
            if branded.contains(&tool) {
                Install::Runs(store(tool))
            } else {
                Install::Absent
            }
        },
        pin: &|| pin.clone(),
        resolve: &|_, toolchain| resolved(name, toolchain),
        trust_checkout: &|| None,
    };
    plan(
        row_for(name).expect("a rerouted name"),
        &args(call),
        &machine,
    )
}

/// The Trust seal 7d5533b2's `name`, through rustup's `trust`, picked as `picked` says.
fn trust_at(name: &str, picked: Picked) -> Resolved {
    Resolved {
        real: PathBuf::from(format!("/t/trust-7d5533b2/bin/{name}")),
        rustup: Some(Rustup {
            toolchain: "trust".into(),
            picked,
        }),
        identity: Identity::Known(Vv {
            tool: name.into(),
            release: "1.99.0-dev".into(),
            commit: Some("7d5533b2bb9c9417bc09c27f1acd9d0b808f8f15".into()),
            host: Some("aarch64-apple-darwin".into()),
            trust: Some("0.1.0".into()),
            mark: None,
        }),
    }
}

/// A stock `name` through rustup's `<channel>-aarch64-apple-darwin`.
fn stock_at(name: &str, channel: &str, picked: Picked) -> Resolved {
    let toolchain = format!("{channel}-aarch64-apple-darwin");
    Resolved {
        real: PathBuf::from(format!("/r/toolchains/{toolchain}/bin/{name}")),
        rustup: Some(Rustup { toolchain, picked }),
        identity: Identity::Known(Vv {
            tool: name.into(),
            release: "1.97.1".into(),
            commit: Some("8bab26f4f68e0e26f0bb7960be334d5b520ea452".into()),
            host: Some("aarch64-apple-darwin".into()),
            trust: None,
            mark: None,
        }),
    }
}

/// What the test machine's rustup resolves `name` to: Trust for `+trust…`, stock for
/// any other `+<channel>`, and — for a bare name — stock 1.97.1 for a reason unsaid.
fn resolved(name: &str, toolchain: Option<&str>) -> Resolved {
    match toolchain {
        Some(tc) if rust_lane::is_trust_channel(tc) => trust_at(name, Picked::CommandLine),
        Some(tc) => stock_at(name, tc, Picked::CommandLine),
        None => stock_at(name, "1.97.1", Picked::Unsaid),
    }
}

/// [`plan`] for `name call…` with `upstream` at [`UP`], nothing branded installed, the
/// name resolving to `resolve`'s answer and the command running in the Trust source
/// checkout `checkout`.
fn plan_resolving(
    name: &str,
    call: &[&str],
    announce: bool,
    resolve: &dyn Fn(&Path, Option<&str>) -> Resolved,
    checkout: Option<&str>,
) -> Plan {
    let machine = Machine {
        passthrough: false,
        announced: false,
        announce: &|| announce,
        strict: &|| false,
        upstream: &|| Some(PathBuf::from(UP)),
        branded: &|_, _| Install::Absent,
        pin: &|| None,
        resolve,
        trust_checkout: &|| checkout.map(PathBuf::from),
    };
    plan(row_for(name).unwrap(), &args(call), &machine)
}

const UP: &str = "/usr/bin/up";

/// NOTHING IS ANNOUNCED THAT WILL NOT RUN: with nothing to run, one answer says so —
/// never a "running" line first — and the setting does not silence it.
#[test]
fn a_plan_with_nothing_to_run_says_only_that() {
    let tlc = plan_on("tlc", &["Spec.tla"], true, None, &[]);
    assert_eq!(
        tlc,
        Plan::Missing {
            say: "aterm: upstream 'tlc' is not on PATH; on this toolchain the tool is 'ty'\n       (drop-in equivalence is not yet proven):\n         ty Spec.tla".into(),
        }
    );
    assert_eq!(plan_on("tlc", &["Spec.tla"], false, None, &[]), tlc);
    let Plan::Missing { say } = plan_on("cargo", &["build"], true, None, &[]) else {
        panic!("no upstream cargo runs nothing");
    };
    assert!(
        say.starts_with("aterm: upstream 'cargo' is not on PATH; on Trust the tool is 'targo':\n"),
        "{say}"
    );
    assert!(say.ends_with("UNVERIFIED — no proof claim"), "{say}");
    assert!(
        !say.contains("running") && !say.contains(ANNOUNCE_SETTING),
        "{say}"
    );
    assert_eq!(
        plan_on("cargo", &["clippy"], true, None, &[]),
        Plan::Missing {
            say: "aterm: upstream 'cargo' is not on PATH; on Trust the tool is 'targo tippy':\n         targo tippy".into(),
        }
    );
    assert_eq!(
        plan_on("cargo", &["+stable", "build"], true, None, &[]),
        Plan::Missing {
            say: "aterm: upstream 'cargo' is not on PATH".into(),
        }
    );
    // A branded tool not installed and NO upstream copy either: one line naming the
    // program that installs it — nothing exists to run.
    assert_eq!(
        plan_on("rustfmt", &["x.rs"], true, None, &[]),
        Plan::Missing {
            say: "aterm: 'rustfmt' did not run — trustfmt is not installed; `aterm pkg install trust` installs it.".into(),
        }
    );
    assert_eq!(
        plan_on("lean", &["f.lean"], true, None, &[]),
        Plan::Missing {
            say: not_installed_message("lean", "clean", "clean"),
        }
    );
    // An explicit lane with targo missing: upstream cargo has no `trust` subcommand,
    // so there is nothing to fall back to, even with an upstream copy.
    assert_eq!(
        plan_on("cargo", &["trust", "build"], true, Some(UP), &[]),
        Plan::Missing {
            say: not_installed_message("cargo", "targo", "trust"),
        }
    );
    // z3 with no upstream copy: nothing to run, and the preferred tool named.
    assert_eq!(
        plan_on("z3", &["-in"], true, None, &["ay"]),
        Plan::Missing {
            say: "aterm: upstream 'z3' is not on PATH; on this toolchain the solver is 'ay'."
                .into(),
        }
    );
}

/// THE 2026-09-29 RULING, AT THE DECISION POINT: a DIRECT row whose Trust tool is
/// missing RUNS the upstream tool the caller named — the caller's own arguments, one
/// line first naming the fix, the line silenced by `[reroute] announce = false` — and
/// `[reroute] strict = true` restores the refusal byte for byte.
#[test]
fn a_missing_trust_tool_runs_upstream_with_a_line_and_strict_refuses() {
    let up = || PathBuf::from(UP);
    for (name, branded, program) in [
        ("clippy", "tippy", "trust"),
        ("rustfmt", "trustfmt", "trust"),
        ("rustdoc", "trustdoc", "trust"),
        ("lean", "clean", "clean"),
    ] {
        // `f.lean` is what a verb-first row would rewrite; upstream gets it untouched.
        let call = ["f.lean", "--flag"];
        assert_eq!(
            plan_on(name, &call, true, Some(UP), &[]),
            Plan::Upstream {
                say: Some(format!(
                    "aterm: running upstream '{name}' — {branded} is not installed (`aterm pkg install {program}` installs it)."
                )),
                target: up(),
                args: args(&call),
            },
            "{name}: the default runs upstream"
        );
        assert_eq!(
            plan_on(name, &call, false, Some(UP), &[]),
            Plan::Upstream {
                say: None,
                target: up(),
                args: args(&call),
            },
            "{name}: the setting silences the line, never the run"
        );
        assert_eq!(
            plan_strict(name, &call, Some(UP), &[]),
            Plan::Missing {
                say: not_installed_message(name, branded, program),
            },
            "{name}: strict refuses as before"
        );
        // Installed, the Trust tool runs in both modes: strict changes only refusals.
        assert!(
            matches!(
                plan_strict(name, &call, Some(UP), &[branded]),
                Plan::Branded { .. }
            ),
            "{name}"
        );
    }
    let text = fallback_not_installed_message("rustfmt", "trustfmt", "trust");
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(
        text.starts_with("aterm: running upstream 'rustfmt'"),
        "leads with what runs"
    );
}

/// z3 (2026-09-29): announce, naming `ay`, then run upstream — the caller's own
/// arguments; the setting silences the line; strict refuses, naming the path, as
/// before. A machine without `ay` is told the command that installs it, never
/// pointed at a tool it lacks.
#[test]
fn z3_runs_upstream_naming_ay_and_strict_refuses() {
    let up = || PathBuf::from(UP);
    for (installed, said) in [
        (
            &["ay"][..],
            "aterm: running upstream 'z3'; on this toolchain the solver is 'ay'.",
        ),
        (
            &[][..],
            "aterm: running upstream 'z3'; on this toolchain the solver is 'ay' \
             (`aterm pkg install ay` installs it).",
        ),
    ] {
        assert_eq!(
            plan_on("z3", &["-smt2", "q.smt2"], true, Some(UP), installed),
            Plan::Upstream {
                say: Some(said.into()),
                target: up(),
                args: args(&["-smt2", "q.smt2"]),
            },
            "ay installed: {installed:?}"
        );
        // With no upstream copy either, nothing runs, and the same fix is named.
        let Plan::Missing { say } = plan_on("z3", &["-in"], true, None, installed) else {
            panic!("no z3 on PATH is 127's line: {installed:?}");
        };
        assert_eq!(
            say.contains("`aterm pkg install ay` installs it"),
            installed.is_empty(),
            "{say}"
        );
        assert_eq!(
            plan_on("z3", &["-in"], false, Some(UP), installed),
            Plan::Upstream {
                say: None,
                target: up(),
                args: args(&["-in"]),
            }
        );
        assert_eq!(
            plan_strict("z3", &["-in"], Some(UP), installed),
            Plan::Refuse {
                say: oracle_message("z3", Some(Path::new(UP))),
            },
            "strict: ORACLE refuses, installed or not"
        );
        assert_eq!(
            plan_strict("z3", &["-in"], None, installed),
            Plan::Refuse {
                say: oracle_message("z3", None),
            }
        );
    }
}

/// `aterm pkg which` says what the stub does in each mode, from the same row.
#[test]
fn policy_summary_follows_strict() {
    let z3 = row_for("z3").unwrap();
    let default = policy_summary(z3, true, false);
    assert!(
        default.starts_with("announced, naming 'ay'") && default.contains("then run upstream"),
        "{default}"
    );
    let strict = policy_summary(z3, true, true);
    assert!(strict.starts_with("refused ('ay'"), "{strict}");
    // With no upstream copy on PATH either mode says so, as the stub's own line does
    // (`oracle_not_on_path_message`, `oracle_message`), and promises no run.
    for (strict, head) in [(false, "announced, naming 'ay'"), (true, "refused ('ay'")] {
        let none = policy_summary(z3, false, strict);
        assert!(
            none.starts_with(head) && none.ends_with("; no upstream 'z3' is on PATH"),
            "strict={strict}: {none}"
        );
        assert!(
            !none.contains("then run upstream") && !none.contains("reaches the real"),
            "strict={strict}: {none}"
        );
    }
    // `aterm pkg which cargo` says what the stub now does: names what the name resolves
    // to, and runs it — inside a Trust source checkout only when that is Trust. With no
    // upstream copy there is nothing to resolve, and the clause says so instead.
    for name in ["cargo", "rustc"] {
        let row = row_for(name).unwrap();
        let summary = policy_summary(row, true, false);
        assert!(
            summary.contains(&format!(
                "the line names what '{name}' resolves to (by its `-vV`: Trust, STOCK, or not known to be Trust), then run upstream — inside a Trust source checkout, only when that is Trust"
            )),
            "{summary}"
        );
        let none = policy_summary(row, false, false);
        assert!(
            none.contains(&format!("; no upstream '{name}' is on PATH"))
                && !none.contains("resolves to"),
            "{none}"
        );
    }
    assert!(
        !policy_summary(row_for("tlc").unwrap(), true, false).contains("resolves"),
        "tlc is not resolved"
    );
    let rustfmt = row_for("rustfmt").unwrap();
    assert!(
        policy_summary(rustfmt, true, false)
            .contains("(upstream 'rustfmt', with a line, while trustfmt is missing)")
    );
    for (found, strict) in [(true, true), (false, false), (false, true)] {
        assert!(
            !policy_summary(rustfmt, found, strict).contains("while trustfmt is missing"),
            "no fallback to promise: found={found} strict={strict}"
        );
    }
}

/// [`storeless_plan`] for `name call…`: upstream at `upstream`, the name resolving to
/// `resolve`'s answer, the command running in the Trust source checkout `checkout`.
fn storeless_on(
    name: &str,
    call: &[&str],
    strict: bool,
    announce: bool,
    upstream: Option<&str>,
    resolve: &dyn Fn(&Path, Option<&str>) -> Resolved,
    checkout: Option<&str>,
) -> Plan {
    let machine = Machine {
        passthrough: false,
        announced: false,
        announce: &|| announce,
        strict: &|| strict,
        upstream: &|| upstream.map(PathBuf::from),
        branded: &|_, _| panic!("no store: nothing branded is looked up"),
        pin: &|| panic!("no store: no pin is named"),
        resolve,
        trust_checkout: &|| checkout.map(PathBuf::from),
    };
    storeless_plan(row_for(name).unwrap(), &args(call), &machine)
}

/// No store resolves (`atpkg __reroute` under `env -i`): the upstream copy runs after
/// one line; strict refuses as before; with no copy, 127's line. A row that names no
/// compiler is never resolved.
#[test]
fn no_store_runs_upstream_and_strict_refuses() {
    let call = args(&["x.rs"]);
    let never = |_: &Path, _: Option<&str>| -> Resolved { panic!("rustfmt is never resolved") };
    assert_eq!(
        storeless_on("rustfmt", &["x.rs"], false, true, Some(UP), &never, None),
        Plan::Upstream {
            say: Some(storeless_message("rustfmt", &call, None)),
            target: PathBuf::from(UP),
            args: call.clone(),
        }
    );
    assert_eq!(
        storeless_message("rustfmt", &call, None),
        "aterm: running upstream 'rustfmt' — no package store resolves here, so its Trust tool was not looked up."
    );
    assert_eq!(
        storeless_on("rustfmt", &["x.rs"], false, false, Some(UP), &never, None),
        Plan::Upstream {
            say: None,
            target: PathBuf::from(UP),
            args: call.clone(),
        }
    );
    assert_eq!(
        storeless_on("cargo", &["build"], false, true, None, &never, None),
        Plan::Missing {
            say: not_on_path_message("cargo"),
        }
    );
    assert_eq!(
        storeless_on(
            "cargo",
            &["build"],
            true,
            true,
            Some(UP),
            &never,
            Some("/t")
        ),
        Plan::Refuse {
            say: "aterm: 'cargo' did not run (`aterm --no-reroute` restores upstream 'cargo')"
                .into(),
        }
    );
}

/// NO STORE IS NO WAY ROUND THE CHECKOUT (2026-10-01 review, reproduced: `env -i PATH=…
/// atpkg __reroute rustc main.rs` two levels inside a Trust checkout ran a stock rustc,
/// under "running upstream 'rustc'"). A `cargo`/`rustc` with no store is resolved and
/// judged as with one: refused there when not Trust, its line naming what runs
/// elsewhere — and resolved only where a line names it or a checkout judges it.
#[test]
fn with_no_store_a_compiler_is_named_and_judged_in_a_trust_checkout() {
    let stock = |name: &str| stock_at(name, "1.97.1", Picked::Unsaid);
    let refused = storeless_on(
        "rustc",
        &["main.rs"],
        false,
        false,
        Some(UP),
        &|_, _| stock("rustc"),
        Some("/t"),
    );
    assert_eq!(
        refused,
        Plan::Refuse {
            say: trust_checkout_refusal(
                row_for("rustc").unwrap(),
                &args(&["main.rs"]),
                None,
                &stock("rustc"),
                Path::new("/t"),
            ),
        },
        "refused, `announce = false` or not"
    );
    // Trust in the checkout runs, named.
    let Plan::Upstream { say: Some(say), .. } = storeless_on(
        "rustc",
        &["+trust", "main.rs"],
        false,
        true,
        Some(UP),
        &|_, tc| {
            trust_at(
                "rustc",
                if tc.is_some() {
                    Picked::CommandLine
                } else {
                    Picked::Default
                },
            )
        },
        Some("/t"),
    ) else {
        panic!("Trust runs in a Trust checkout");
    };
    assert_eq!(
        say,
        "aterm: running 'rustc +trust' -> Trust 7d5533b2, via rustup trust (named on the command line) — no package store resolves here, so its Trust tool was not looked up."
    );
    // Elsewhere stock runs, named STOCK — and, silenced, it is never resolved.
    let Plan::Upstream { say: Some(say), .. } = storeless_on(
        "cargo",
        &["build"],
        false,
        true,
        Some(UP),
        &|_, _| stock("cargo"),
        None,
    ) else {
        panic!("stock runs outside a checkout");
    };
    assert!(
        say.starts_with("aterm: running 'cargo' -> STOCK cargo 1.97.1 (8bab26f4f)"),
        "{say}"
    );
    assert!(say.contains("; not Trust — no package store"), "{say}");
    assert_eq!(
        storeless_on(
            "cargo",
            &["build"],
            false,
            false,
            Some(UP),
            &|_, _| panic!("nothing to name and no checkout: not resolved"),
            None,
        ),
        Plan::Upstream {
            say: None,
            target: PathBuf::from(UP),
            args: args(&["build"]),
        }
    );
}

/// A CHILD OF A COMMAND THE REROUTE RAN ([`ANNOUNCED_ENV`], 2026-10-01): never announced
/// again, and upstream as typed — a DIRECT row's too, whatever is installed — but a
/// `cargo`/`rustc` inside a Trust source checkout is judged as the same name typed
/// alone. Reproduced by the review before: a Trust `cargo demo` the reroute allowed
/// there handed its tree the ESCAPE, and the `rustc +1.97.1 -V` its subcommand spawned
/// ran stock with no line, where typed alone it was refused.
#[test]
fn a_child_of_a_command_the_reroute_ran_is_quiet_and_judged() {
    let child = |name: &str,
                 call: &[&str],
                 resolve: &dyn Fn(&Path, Option<&str>) -> Resolved,
                 checkout: &dyn Fn() -> Option<PathBuf>|
     -> Plan {
        let machine = Machine {
            passthrough: false,
            announced: true,
            announce: &|| true,
            strict: &|| false,
            upstream: &|| Some(PathBuf::from(UP)),
            branded: &|tool, _| Install::Runs(store(tool)),
            pin: &|| panic!("a child names no pin"),
            resolve,
            trust_checkout: checkout,
        };
        plan(row_for(name).unwrap(), &args(call), &machine)
    };
    let upstream = |call: &[&str]| Plan::Upstream {
        say: None,
        target: PathBuf::from(UP),
        args: args(call),
    };
    let in_checkout = || Some(PathBuf::from("/t"));
    // The reviewer's case: `rustc +1.97.1 -V` under a Trust cargo, inside the checkout.
    let stock = stock_at("rustc", "1.97.1", Picked::CommandLine);
    assert_eq!(
        child(
            "rustc",
            &["+1.97.1", "-V"],
            &|_, tc| resolved("rustc", tc),
            &in_checkout
        ),
        Plan::Refuse {
            say: trust_checkout_refusal(
                row_for("rustc").unwrap(),
                &args(&["-V"]),
                Some("1.97.1"),
                &stock,
                Path::new("/t"),
            ),
        }
    );
    // Trust there runs, said nothing.
    assert_eq!(
        child(
            "cargo",
            &["build"],
            &|_, _| trust_at("cargo", Picked::Environment),
            &in_checkout
        ),
        upstream(&["build"])
    );
    // Outside a checkout: upstream, said nothing, never resolved.
    assert_eq!(
        child(
            "rustc",
            &["+1.97.1", "-V"],
            &|_, _| panic!("outside a checkout a child is not resolved"),
            &|| None,
        ),
        upstream(&["+1.97.1", "-V"])
    );
    // A DIRECT row: upstream as typed, never the branded tool, judged nowhere — cargo's
    // `rustdoc` stays the rustdoc cargo spawned.
    for name in ["rustdoc", "rustfmt", "clippy", "lean"] {
        assert_eq!(
            child(
                name,
                &["x"],
                &|_, _| panic!("{name} is not resolved"),
                &|| panic!("{name} is judged in no checkout"),
            ),
            upstream(&["x"]),
            "{name}"
        );
    }
}

/// A branded tool that cannot run names the fix that works: a shim that forwards to no
/// build (a tombstone, a pending stub) is exec'd with nothing said first, so its own
/// line speaks; a lost shim names `repair`; only a tool nothing here holds names
/// `install`. The probe is asked about the program the table names.
#[test]
fn a_branded_tool_that_cannot_run_names_the_fix_that_works() {
    let shim = PathBuf::from("/prefix/bin/clean");
    let machine = Machine {
        passthrough: false,
        announced: false,
        announce: &|| true,
        strict: &|| false,
        upstream: &|| Some(PathBuf::from(UP)),
        branded: &|tool, program| {
            assert_eq!((tool, program), ("clean", "clean"));
            Install::Shim(shim.clone())
        },
        pin: &|| None,
        resolve: &|_, _| panic!("a DIRECT row resolves nothing"),
        trust_checkout: &|| panic!("a DIRECT row is judged in no checkout"),
    };
    assert_eq!(
        plan(row_for("lean").unwrap(), &args(&["f.lean"]), &machine),
        Plan::Shim {
            shim: shim.clone(),
            args: args(&["check", "f.lean"]),
        }
    );
    // A lost shim, an upstream copy on PATH: upstream runs, the line names `repair`;
    // strict refuses with the same fix, as before.
    let lost = |strict: bool, upstream: Option<PathBuf>| {
        move |row: &str, call: &[&str]| {
            let machine = Machine {
                passthrough: false,
                announced: false,
                announce: &|| true,
                strict: &|| strict,
                upstream: &|| upstream.clone(),
                branded: &|tool, program| {
                    assert_eq!(program, "trust", "{tool}");
                    Install::ShimLost
                },
                pin: &|| None,
                resolve: &|_, _| panic!("a DIRECT row resolves nothing"),
                trust_checkout: &|| None,
            };
            plan(row_for(row).unwrap(), &args(call), &machine)
        }
    };
    assert_eq!(
        lost(false, Some(PathBuf::from(UP)))("rustfmt", &["x.rs"]),
        Plan::Upstream {
            say: Some("aterm: running upstream 'rustfmt' — trustfmt's shim is missing (`aterm pkg repair` restores it).".into()),
            target: PathBuf::from(UP),
            args: args(&["x.rs"]),
        }
    );
    for (strict, upstream) in [(true, Some(PathBuf::from(UP))), (false, None)] {
        assert_eq!(
            lost(strict, upstream)("rustfmt", &["x.rs"]),
            Plan::Missing {
                say: "aterm: 'rustfmt' did not run — trustfmt's shim is missing; `aterm pkg repair` restores it.".into(),
            },
            "strict={strict}"
        );
    }
    assert_eq!(
        lost(false, Some(PathBuf::from(UP)))("cargo", &["trust", "build"]),
        Plan::Missing {
            say: lost_shim_message("cargo", "targo"),
        }
    );
}

/// [`installed`] on a real store: a laid shim runs; with it lost, the live build says
/// so; a tombstone and a pending stub are the shim to exec; nothing else is absent.
#[cfg(unix)]
#[test]
fn a_branded_tool_is_read_off_the_store_the_way_atpkg_run_reads_it() {
    let l = layout("installed");
    assert_eq!(installed(&l, "clean", "clean"), Install::Absent);
    let d = l.build_dir("clean", 7);
    std::fs::create_dir_all(d.join("bin")).unwrap();
    let exe = d.join("bin").join("clean");
    std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    crate::store::mark_build_ready(&d).unwrap();
    crate::activate::install_shims(
        &l,
        &d,
        &["clean".to_string()],
        crate::activate::Aliases::Alab,
    )
    .unwrap();
    crate::activate::activate_build(&l, &d).unwrap();
    assert!(
        matches!(installed(&l, "clean", "clean"), Install::Runs(_)),
        "a laid shim runs"
    );
    let clean = crate::store::ToolName::new("clean").unwrap();
    std::fs::remove_file(l.shim(&clean)).unwrap();
    assert_eq!(installed(&l, "clean", "clean"), Install::ShimLost);
    assert_eq!(
        installed(&l, "clean", "trust"),
        Install::Absent,
        "another program's build holds no `clean`"
    );
    crate::activate::install_tombstone_shim(&l, &clean).unwrap();
    assert_eq!(
        installed(&l, "clean", "clean"),
        Install::Shim(l.shim(&clean))
    );
    let targo = crate::store::ToolName::new("targo").unwrap();
    crate::stub::write_pending_stub(&l, &targo).unwrap();
    assert_eq!(
        installed(&l, "targo", "trust"),
        Install::Shim(l.shim(&targo))
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// What runs, and with which arguments: upstream gets the caller's own (a `+trust…`
/// included — only the rendered lanes drop it), the setting silences every upstream
/// line, and a DIRECT line names the escape only where an upstream copy exists.
#[test]
fn a_plan_runs_what_was_asked_with_the_callers_arguments() {
    let up = || PathBuf::from(UP);
    let Plan::Upstream {
        say: Some(say),
        target,
        args: argv,
    } = plan_on("cargo", &["+trust", "build"], true, Some(UP), &[])
    else {
        panic!("`cargo +trust build` announces and runs upstream");
    };
    assert_eq!((target, argv), (up(), args(&["+trust", "build"])));
    // What runs is what rustup's trust toolchain resolved to, as typed: the first line
    // names it by what it is and says no "no proof claim" of it; the lanes drop the
    // `+trust` targo rejects.
    let (first, lanes) = say.split_once('\n').expect("a block");
    assert_eq!(
        first,
        "aterm: running 'cargo +trust' -> Trust 7d5533b2, via rustup trust (named on the command line); Trust's own spelling is 'targo':"
    );
    assert!(
        lanes.contains("targo trust build ") && !lanes.contains("+trust"),
        "{say}"
    );
    let Plan::Upstream { say: Some(say), .. } =
        plan_on("cargo", &["+trust", "clippy"], true, Some(UP), &[])
    else {
        panic!("`cargo +trust clippy` announces and runs upstream");
    };
    assert!(
        say.starts_with(
            "aterm: running 'cargo +trust clippy' -> Trust 7d5533b2, via rustup trust (named on the command line); Trust's own spelling is 'targo tippy':\n"
        ),
        "{say}"
    );
    for call in [&["build"][..], &["+stable", "build"][..]] {
        assert_eq!(
            plan_on("cargo", call, false, Some(UP), &[]),
            Plan::Upstream {
                say: None,
                target: up(),
                args: args(call),
            },
            "{call:?}: the setting silences the line, never the run"
        );
    }
    assert_eq!(
        plan_on("cargo", &["+stable", "build"], true, Some(UP), &[]),
        Plan::Upstream {
            say: Some(stock_channel_signpost(
                row_for("cargo").unwrap(),
                "stable",
                &args(&["build"]),
                Some(&resolved("cargo", Some("stable"))),
            )),
            target: up(),
            args: args(&["+stable", "build"]),
        }
    );
    assert_eq!(
        plan_on("clippy", &["x"], true, None, &["tippy"]),
        Plan::Branded {
            say: "aterm: running tippy, Trust's 'clippy'.".into(),
            copy: store("tippy"),
            args: args(&["x"]),
        }
    );
    let Plan::Branded { say, .. } = plan_on("rustfmt", &[], false, Some(UP), &["trustfmt"]) else {
        panic!("an installed DIRECT tool runs");
    };
    assert_eq!(say, direct_announcement("rustfmt", "trustfmt", true));
    // An explicit lane: targo, without the `+trust` it rejects.
    assert_eq!(
        plan_on(
            "cargo",
            &["+trust", "trust", "build"],
            true,
            Some(UP),
            &["targo"]
        ),
        Plan::Branded {
            say: explicit_lane_note("cargo", "targo"),
            copy: store("targo"),
            args: args(&["trust", "build"]),
        }
    );
    // The escape engaged: upstream, silently, whatever the row.
    let machine = Machine {
        passthrough: true,
        announced: false,
        announce: &|| true,
        strict: &|| panic!("the escape reads no strictness"),
        upstream: &|| Some(up()),
        branded: &|_, _| Install::Absent,
        pin: &|| panic!("the escape reads no pin"),
        resolve: &|_, _| panic!("the escape resolves nothing"),
        trust_checkout: &|| panic!("the escape is judged in no checkout"),
    };
    assert_eq!(
        plan(row_for("rustfmt").unwrap(), &args(&["x.rs"]), &machine),
        Plan::Upstream {
            say: None,
            target: up(),
            args: args(&["x.rs"]),
        }
    );
}

/// THE INCIDENT'S COMMAND (2026-09-23): `cargo +1.97.1 test -p x` in a session.
/// Measured before on the installed aterm 0.91.0: ONE line naming only `targo trust
/// <verb>` — no arguments, and not the unverified lane the agents needed — and 33
/// agent logs carried it beside a stock run. Now the first line says what runs, the
/// Trust spelling of the very command follows in both lanes, and one line says a
/// stock channel moves only rustup's proxies. Upstream still runs as named (announce,
/// never prevent), and the setting silences the lines, never the run.
#[test]
fn a_stock_channel_signposts_the_trust_spelling_and_runs_upstream_as_named() {
    let call = ["+1.97.1", "test", "-p", "x"];
    let Plan::Upstream {
        say: Some(text),
        target,
        args: argv,
    } = plan_on("cargo", &call, true, Some(UP), &[])
    else {
        panic!("a stock channel runs upstream");
    };
    assert_eq!(
        (target, argv),
        (PathBuf::from(UP), args(&call)),
        "upstream runs exactly as named"
    );
    assert!(
        text.starts_with(
            "aterm: running 'cargo +1.97.1' -> STOCK cargo 1.97.1 (8bab26f4f), via rustup 1.97.1-aarch64-apple-darwin (named on the command line); not Trust (no proof claim); on Trust the tool is 'targo':\n"
        ),
        "{text}"
    );
    assert!(text.contains("targo trust test -p x"), "{text}");
    assert!(text.contains("targo --unverified test -p x"), "{text}");
    assert!(
        text.contains("moves only rustup's proxies, never targo.\n"),
        "{text}"
    );
    assert!(
        !text.contains("+1.97.1 test"),
        "the Trust spelling carries no directive: {text}"
    );
    assert!(text.contains("`aterm help rust`"), "{text}");
    assert!(
        text.ends_with(&format!(
            "({ANNOUNCE_SETTING} in aterm.toml silences this — Settings ▸ Packages.)"
        )),
        "{text}"
    );
    // The directive, not the directory's pin, decides what upstream is.
    let pinned = Pin {
        channel: "trust".into(),
        source: rust_lane::PinSource::File("/r/rust-toolchain.toml".into()),
    };
    assert_eq!(
        plan_pinned("cargo", &call, true, Some(UP), &[], Some(pinned)),
        plan_on("cargo", &call, true, Some(UP), &[])
    );
    // The setting silences the lines, never the run.
    assert_eq!(
        plan_on("cargo", &call, false, Some(UP), &[]),
        Plan::Upstream {
            say: None,
            target: PathBuf::from(UP),
            args: args(&call),
        }
    );
    // `cargo +stable clippy`: the linter's one spelling, not two lanes.
    let Plan::Upstream {
        say: Some(text), ..
    } = plan_on(
        "cargo",
        &["+stable", "clippy", "--all-targets"],
        true,
        Some(UP),
        &[],
    )
    else {
        panic!("`cargo +stable clippy` runs upstream");
    };
    assert!(
        text.starts_with(
            "aterm: running 'cargo +stable clippy' -> STOCK cargo 1.97.1 (8bab26f4f), via rustup stable-aarch64-apple-darwin (named on the command line); not Trust; on Trust the tool is 'targo tippy':\n         targo tippy --all-targets\n"
        ),
        "{text}"
    );
    assert!(!text.contains("targo trust"), "{text}");
    // `rustc +1.95.0 main.rs`: trustc's two lanes.
    let Plan::Upstream {
        say: Some(text),
        args: argv,
        ..
    } = plan_on("rustc", &["+1.95.0", "main.rs"], true, Some(UP), &[])
    else {
        panic!("`rustc +1.95.0` runs upstream");
    };
    assert!(text.contains("trustc main.rs"), "{text}");
    assert!(text.contains("trustc -Ztrust-verify=off main.rs"), "{text}");
    assert!(text.contains("never trustc.\n"), "{text}");
    assert_eq!(argv, args(&["+1.95.0", "main.rs"]));
    // A cargo extension targo lacks is never presented as THE Trust tool.
    let text = stock_channel_signpost(
        row_for("cargo").unwrap(),
        "1.97.1",
        &args(&["nextest", "run"]),
        None,
    );
    assert!(
        text.contains("targo is not known to have `nextest`"),
        "{text}"
    );
    assert!(
        text.contains("If it has, it is:\n         targo nextest run\n"),
        "{text}"
    );
    assert!(!text.contains("on Trust the tool is"), "{text}");
}

/// HOST-05's second half, measured before this change: `rustfmt +1.97.1 --check x.rs`
/// in a session ran trustfmt with `+1.97.1` as a FILE (`Error: file '+1.97.1' does not
/// exist`, exit 1) — and it still did on main's `plan` until this change. A DIRECT
/// Rust row never hands a rustup directive to its branded tool: a stock one is
/// signposted and runs upstream as named, `+trust…` is dropped and the branded tool
/// runs.
#[test]
fn a_direct_row_never_hands_a_rustup_directive_to_the_branded_tool() {
    let call = ["+1.97.1", "--check", "x.rs"];
    let Plan::Upstream {
        say: Some(text),
        args: argv,
        ..
    } = plan_on("rustfmt", &call, true, Some(UP), &["trustfmt"])
    else {
        panic!("a stock channel runs upstream rustfmt");
    };
    assert_eq!(argv, args(&call));
    assert!(
        text.starts_with(
            "aterm: running upstream 'rustfmt +1.97.1'; on Trust the tool is 'trustfmt':\n         trustfmt --check x.rs\n"
        ),
        "{text}"
    );
    assert!(text.contains("never trustfmt.\n"), "{text}");
    // `+trust` names Trust: the branded tool runs without the directive.
    assert_eq!(
        plan_on(
            "rustfmt",
            &["+trust", "x.rs"],
            true,
            Some(UP),
            &["trustfmt"]
        ),
        Plan::Branded {
            say: direct_announcement("rustfmt", "trustfmt", true),
            copy: store("trustfmt"),
            args: args(&["x.rs"]),
        }
    );
    for (name, branded) in [("clippy", "tippy"), ("rustdoc", "trustdoc")] {
        let Plan::Upstream { args: argv, .. } =
            plan_on(name, &["+1.97.1", "a.rs"], true, Some(UP), &[branded])
        else {
            panic!("{name} +1.97.1 runs upstream");
        };
        assert_eq!(argv, args(&["+1.97.1", "a.rs"]), "{name}");
        let Plan::Branded { args: argv, .. } =
            plan_on(name, &["+trust", "a.rs"], true, Some(UP), &[branded])
        else {
            panic!("{name} +trust runs {branded}");
        };
        assert_eq!(argv, args(&["a.rs"]), "{name}");
    }
    // No upstream copy: nothing runs, and the line says so — the directive is never
    // handed to the branded tool instead.
    assert_eq!(
        plan_on("rustfmt", &call, true, None, &["trustfmt"]),
        Plan::Missing {
            say: not_on_path_message("rustfmt"),
        }
    );
    // Not a Rust row: `+` means nothing to clean, and nothing is stripped.
    let Plan::Branded { args: argv, .. } = plan_on("lean", &["+x"], true, Some(UP), &["clean"])
    else {
        panic!("lean runs clean");
    };
    assert_eq!(argv, args(&["+x"]));
}

/// A directory that pins a STOCK channel says so under the lanes: a pin moves only
/// rustup's proxies, never targo. A Trust pin adds nothing, and a `+trust` directive
/// outranks the pin.
#[test]
fn a_pinned_channel_is_named_under_the_lanes() {
    let stock = Pin {
        channel: "1.97.1".into(),
        source: rust_lane::PinSource::File("/r/rust-toolchain.toml".into()),
    };
    let Plan::Upstream {
        say: Some(text),
        args: argv,
        ..
    } = plan_pinned("cargo", &["test"], true, Some(UP), &[], Some(stock.clone()))
    else {
        panic!("a pinned directory still runs upstream");
    };
    assert_eq!(argv, args(&["test"]));
    assert!(
        text.contains(
            "This directory pins stock \"1.97.1\", which moves only rustup's proxies, never targo:\n       the Trust spellings above run the Trust toolchain here too. (/r/rust-toolchain.toml)\n"
        ),
        "{text}"
    );
    let lanes_at = text.find("targo --unverified test").unwrap();
    assert!(
        text.find("pins stock").unwrap() > lanes_at,
        "under the lanes: {text}"
    );
    assert!(text.ends_with("Settings ▸ Packages.)"), "{text}");
    // `$RUSTUP_TOOLCHAIN` is the environment, not the directory, and is named so.
    let env = Pin {
        channel: "stable".into(),
        source: rust_lane::PinSource::Env,
    };
    let Plan::Upstream {
        say: Some(text), ..
    } = plan_pinned("cargo", &["test"], true, Some(UP), &[], Some(env))
    else {
        panic!("an environment pin still runs upstream");
    };
    assert!(
        text.contains(
            "\n       $RUSTUP_TOOLCHAIN names stock \"stable\", which moves only rustup's proxies, never targo:\n       the Trust spellings above run the Trust toolchain here too.\n"
        ),
        "{text}"
    );
    assert!(!text.contains("This directory"), "{text}");
    assert!(!text.contains("too. ($RUSTUP_TOOLCHAIN)"), "{text}");
    // A Trust pin is the default: nothing is added.
    let trust = Pin {
        channel: "trust".into(),
        source: rust_lane::PinSource::Env,
    };
    assert_eq!(
        plan_pinned("cargo", &["test"], true, Some(UP), &[], Some(trust)),
        plan_on("cargo", &["test"], true, Some(UP), &[])
    );
    // `cargo +trust test`: the directive, not the pin, decides what runs.
    let Plan::Upstream {
        say: Some(text), ..
    } = plan_pinned(
        "cargo",
        &["+trust", "test"],
        true,
        Some(UP),
        &[],
        Some(stock.clone()),
    )
    else {
        panic!("`cargo +trust test` runs upstream");
    };
    assert!(text.contains("targo trust test"), "{text}");
    assert!(!text.contains("pins stock"), "{text}");
    // Silenced by the setting like the rest of a signpost — but it runs.
    assert_eq!(
        plan_pinned(
            "cargo",
            &["test"],
            false,
            Some(UP),
            &[],
            Some(stock.clone())
        ),
        Plan::Upstream {
            say: None,
            target: PathBuf::from(UP),
            args: args(&["test"]),
        }
    );
    // With no upstream copy nothing runs, and no pin is named.
    let Plan::Missing { say } = plan_pinned("cargo", &["test"], true, None, &[], Some(stock))
    else {
        panic!("no upstream cargo runs nothing");
    };
    assert!(!say.contains("pins"), "{say}");
    // `$RUSTUP_TOOLCHAIN` outranks a file, as rustup ranks it; empty is unset.
    let pin = Pin::for_dir(None, Some("1.95.0")).unwrap();
    assert_eq!(
        (pin.channel.as_str(), &pin.source),
        ("1.95.0", &rust_lane::PinSource::Env)
    );
    assert_eq!(Pin::for_dir(None, Some("")), None);
    // A file up the tree is found from a subdirectory.
    let l = layout("pin");
    let sub = l.prefix.join("crate").join("src");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(
        l.prefix.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.97.1\"\n",
    )
    .unwrap();
    let pin = Pin::for_dir(Some(&sub), None).expect("the pin up the tree");
    assert_eq!(pin.channel, "1.97.1");
    assert!(
        matches!(&pin.source, rust_lane::PinSource::File(f) if f.ends_with("rust-toolchain.toml")),
        "{pin:?}"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// THE NAMING OF 2026-09-29 (plan item MP-9), at the decision point: `cargo`/`rustc` are
/// announced by what they RESOLVED to — Trust by its commit, with no "upstream" and no
/// "no proof claim" said of it; STOCK by its version, "not Trust"; anything unanswered as
/// not known to be Trust — and one line names the file. Measured before: a bare `rustc
/// -vV` that ran the Trust seal 7d5533b2 printed "running upstream 'rustc' (no proof
/// claim)", the words it printed over stock 1.97.1 in a pinned directory.
#[test]
fn cargo_and_rustc_are_named_by_what_they_resolve_to() {
    let trust = |_: &Path, _: Option<&str>| trust_at("rustc", Picked::Default);
    let Plan::Upstream {
        say: Some(text),
        target,
        args: argv,
    } = plan_resolving("rustc", &["-vV"], true, &trust, None)
    else {
        panic!("Trust runs");
    };
    assert_eq!(
        (target, argv),
        (PathBuf::from(UP), args(&["-vV"])),
        "what was asked"
    );
    assert!(
        text.starts_with(
            "aterm: running 'rustc' -> Trust 7d5533b2, via rustup trust (the rustup default); Trust's own spelling is 'trustc':\n         trustc -vV "
        ),
        "{text}"
    );
    assert!(
        !text.contains("upstream") && !text.contains("no proof claim)"),
        "nothing calls Trust upstream or says it carries no proof claim: {text}"
    );
    assert!(
        text.contains("\n       ('rustc' here is /t/trust-7d5533b2/bin/rustc.)\n"),
        "{text}"
    );
    assert!(text.ends_with("Settings ▸ Packages.)"), "{text}");

    let stock = |_: &Path, _: Option<&str>| {
        stock_at(
            "cargo",
            "1.97.1",
            Picked::File("/r/rust-toolchain.toml".into()),
        )
    };
    let Plan::Upstream {
        say: Some(text), ..
    } = plan_resolving("cargo", &["build"], true, &stock, None)
    else {
        panic!("outside a Trust source checkout, stock runs");
    };
    assert!(
        text.starts_with(
            "aterm: running 'cargo' -> STOCK cargo 1.97.1 (8bab26f4f), via rustup 1.97.1-aarch64-apple-darwin (pinned by /r/rust-toolchain.toml); not Trust (no proof claim); on Trust the tool is 'targo':\n"
        ),
        "{text}"
    );
    assert!(text.contains("targo --unverified build"), "{text}");
    assert!(
        text.contains("('cargo' here is /r/toolchains/1.97.1-aarch64-apple-darwin/bin/cargo.)"),
        "{text}"
    );

    let unknown = |_: &Path, _: Option<&str>| Resolved {
        real: PathBuf::from(UP),
        rustup: None,
        identity: Identity::Unknown {
            why: "/usr/bin/up -vV exited 3".into(),
        },
    };
    let Plan::Upstream {
        say: Some(text), ..
    } = plan_resolving("cargo", &["metadata"], true, &unknown, None)
    else {
        panic!("an unidentified cargo runs outside a checkout");
    };
    assert!(
        text.starts_with(
            "aterm: running 'cargo metadata' -> an unidentified cargo (/usr/bin/up -vV exited 3); not known to be Trust; on Trust the tool is 'targo metadata':\n"
        ),
        "{text}"
    );
    // The directory's stock pin is still named under the lanes, beside what resolved.
    let pin = Pin {
        channel: "1.97.1".into(),
        source: rust_lane::PinSource::File("/r/rust-toolchain.toml".into()),
    };
    let Plan::Upstream {
        say: Some(text), ..
    } = plan_pinned("cargo", &["test"], true, Some(UP), &[], Some(pin))
    else {
        panic!("a pinned directory runs");
    };
    assert!(text.contains("-> STOCK cargo 1.97.1"), "{text}");
    assert!(
        text.contains("This directory pins stock \"1.97.1\""),
        "{text}"
    );
}

/// Resolving costs a `rustup which` and maybe a `-vV`, so it is asked only where a line
/// will name it or a Trust source checkout must judge it: never under the escape, never
/// with the announcement silenced outside a checkout, never for a row that is not
/// `cargo`/`rustc` (`tlc`, a DIRECT row's `+<stock>`).
#[test]
fn a_resolution_is_asked_only_where_it_is_said_or_judged() {
    let never = |_: &Path, _: Option<&str>| -> Resolved { panic!("nothing to name or judge") };
    assert_eq!(
        plan_resolving("cargo", &["build"], false, &never, None),
        Plan::Upstream {
            say: None,
            target: PathBuf::from(UP),
            args: args(&["build"]),
        },
        "silenced, outside a checkout: nothing is resolved"
    );
    assert_eq!(
        plan_resolving("cargo", &["+1.97.1", "build"], false, &never, None),
        Plan::Upstream {
            say: None,
            target: PathBuf::from(UP),
            args: args(&["+1.97.1", "build"]),
        }
    );
    for (name, call) in [
        ("tlc", &["Spec.tla"][..]),
        ("rustfmt", &["+1.97.1", "x.rs"][..]),
    ] {
        assert!(
            matches!(
                plan_resolving(name, call, true, &never, Some("/t")),
                Plan::Upstream { say: Some(_), .. }
            ),
            "{name}: not a compiler name, never resolved nor judged"
        );
    }
    // The explicit lane runs targo: nothing upstream to resolve.
    assert!(matches!(
        plan_resolving("cargo", &["trust", "build"], true, &never, Some("/t")),
        Plan::Missing { .. }
    ));
}

/// INSIDE A TRUST SOURCE CHECKOUT only Trust runs by name (the owner's "NO STOCK RUST IN
/// THE TOOLCHAIN FOR TRUST"): a `cargo`/`rustc` that
/// resolves to STOCK — bare, pinned or `+<channel>` — or to anything not known to be
/// Trust is refused (exit 2) with what it resolved to, the checkout, the Trust spelling
/// and how to run it anyway; the setting that silences a signpost does not
/// silence a refusal. Trust there runs as anywhere.
#[test]
fn inside_a_trust_checkout_only_trust_runs_by_name() {
    let stock = |_: &Path, tc: Option<&str>| resolved("cargo", tc);
    let refused = plan_resolving("cargo", &["build"], true, &stock, Some("/t"));
    let Plan::Refuse { say } = &refused else {
        panic!("stock is refused inside the checkout: {refused:?}");
    };
    assert_eq!(
        say.as_str(),
        "aterm: 'cargo' did not run -> STOCK cargo 1.97.1 (8bab26f4f), via rustup 1.97.1-aarch64-apple-darwin; not Trust,\n       \
         and /t is a Trust source checkout (its src/stage0 says compiler_dist_channel=trust), where a compiler not known to be Trust would build part of Trust; on Trust the tool is 'targo':\n         \
         targo trust build          VERIFIED   — emits a proof claim\n         \
         targo --unverified build   UNVERIFIED — no proof claim\n       \
         ('cargo' here is /r/toolchains/1.97.1-aarch64-apple-darwin/bin/cargo; `aterm --no-reroute` runs it as named.)\n\
         \x20      `aterm help rust` shows which toolchain this directory gets; the default here is Trust."
    );
    assert_eq!(
        plan_resolving("cargo", &["build"], false, &stock, Some("/t")),
        refused,
        "a refusal is not an announcement: the setting does not silence it"
    );
    let Plan::Refuse { say } =
        plan_resolving("rustc", &["+1.97.1", "main.rs"], true, &stock, Some("/t"))
    else {
        panic!("`rustc +1.97.1` resolves stock: refused");
    };
    assert!(
        say.starts_with("aterm: 'rustc +1.97.1' did not run -> STOCK "),
        "{say}"
    );
    assert!(say.contains("trustc main.rs"), "{say}");
    let unknown = |_: &Path, _: Option<&str>| Resolved {
        real: PathBuf::from(UP),
        rustup: None,
        identity: Identity::Unknown {
            why: "`rustup which cargo`: error: toolchain 'x' is not installed".into(),
        },
    };
    assert!(
        matches!(
            plan_resolving("cargo", &["build"], true, &unknown, Some("/t")),
            Plan::Refuse { .. }
        ),
        "not known to be Trust is not Trust"
    );
    // Trust runs, announced as anywhere; `+trust` too.
    for call in [&["build"][..], &["+trust", "build"][..]] {
        let trust = |_: &Path, _: Option<&str>| trust_at("cargo", Picked::Default);
        let Plan::Upstream {
            say: Some(say),
            args: argv,
            ..
        } = plan_resolving("cargo", call, true, &trust, Some("/t"))
        else {
            panic!("{call:?}: Trust runs inside the checkout");
        };
        assert_eq!(argv, args(call));
        assert!(say.contains(" -> Trust 7d5533b2"), "{say}");
    }
    // The escape still restores upstream, unjudged: `aterm --no-reroute`.
    let machine = Machine {
        passthrough: true,
        announced: false,
        announce: &|| true,
        strict: &|| false,
        upstream: &|| Some(PathBuf::from(UP)),
        branded: &|_, _| Install::Absent,
        pin: &|| None,
        resolve: &|_, _| panic!("the escape resolves nothing"),
        trust_checkout: &|| panic!("the escape is judged in no checkout"),
    };
    assert_eq!(
        plan(
            row_for("cargo").unwrap(),
            &args(&["+1.97.1", "build"]),
            &machine
        ),
        Plan::Upstream {
            say: None,
            target: PathBuf::from(UP),
            args: args(&["+1.97.1", "build"]),
        }
    );
}

/// Every cargo verb is signposted with a command targo ACCEPTS (measured on store
/// build 9192): `cargo metadata` printed `targo trust metadata` and `targo
/// --unverified metadata` before this change — both refused by targo.
#[test]
fn every_cargo_verb_is_signposted_with_a_command_targo_accepts() {
    let cargo = row_for("cargo").unwrap();
    let Policy::Signpost { lanes, .. } = cargo.policy else {
        panic!()
    };
    let text = signpost_message_in(
        cargo,
        lanes,
        &args(&["metadata", "--format-version", "1"]),
        None,
        None,
        None,
    );
    assert!(
        text.starts_with(
            "aterm: running upstream 'cargo metadata'; on Trust the tool is 'targo metadata':\n         targo metadata --format-version 1\n"
        ),
        "{text}"
    );
    assert!(!text.contains("targo trust"), "{text}");
    assert!(!text.contains("targo --unverified"), "{text}");
    let text = signpost_message_in(cargo, lanes, &args(&["run", "-p", "x"]), None, None, None);
    assert!(
        text.starts_with(
            "aterm: running upstream 'cargo run' (no proof claim); on Trust the tool is 'targo':\n"
        ),
        "{text}"
    );
    assert!(text.contains("targo --unverified run -p x"), "{text}");
    assert!(text.contains("this verb has no verified lane"), "{text}");
    assert!(!text.contains("targo trust run"), "{text}");
    let text = signpost_message_in(cargo, lanes, &args(&["--version"]), None, None, None);
    assert!(
        text.starts_with(
            "aterm: running upstream 'cargo'; on Trust the tool is 'targo':\n         targo --version\n"
        ),
        "{text}"
    );
    // A verb targo is not known to have is never presented as THE Trust tool:
    // `targo nextest` answers "no such command" (measured 2026-09-24).
    let text = signpost_message_in(cargo, lanes, &args(&["nextest", "run"]), None, None, None);
    assert!(
        text.starts_with(
            "aterm: running upstream 'cargo nextest' (no proof claim); targo is not known to have `nextest`\n"
        ),
        "{text}"
    );
    assert!(
        text.contains("If it has, it is:\n         targo nextest run\n"),
        "{text}"
    );
    assert!(!text.contains("on Trust the tool is"), "{text}");
}

/// The spellings this module prints and the ones `aterm_types::rust_lane` renders
/// for `aterm pkg lane` and `aterm help rust` are ONE vocabulary: the lanes of the
/// policy table, the cargo verb reroutes and the DIRECT rows' branded tools cannot
/// drift from it.
#[test]
fn the_table_speaks_the_shared_rust_lane_vocabulary() {
    let as_lanes = |stock: &str, a: &[&str]| -> Vec<(String, String, String)> {
        rust_lane::trust_spelling(stock, &args(a))
            .unwrap()
            .into_iter()
            .map(|s| (s.label.to_string(), s.command, s.note.to_string()))
            .collect()
    };
    for (stock, table) in [("cargo", CARGO_LANES), ("rustc", RUSTC_LANES)] {
        let verb: &[&str] = if stock == "cargo" { &["build"] } else { &[] };
        let ours: Vec<(String, String, String)> = table
            .iter()
            .map(|l| {
                (
                    l.label.to_string(),
                    join_command(l.command, &verb.join(" ")),
                    l.note.to_string(),
                )
            })
            .collect();
        assert_eq!(ours, as_lanes(stock, verb), "{stock}");
    }
    for (verb, reroute) in CARGO_VERB_REROUTES {
        assert_eq!(as_lanes("cargo", &[verb])[0].1, *reroute, "cargo {verb}");
    }
    let mut rust_rows = 0;
    for row in TABLE.iter().filter(|r| is_rust_row(r)) {
        rust_rows += 1;
        if let Policy::Direct { branded, .. } = row.policy {
            assert_eq!(
                as_lanes(row.upstream, &[])[0].1,
                branded,
                "{}",
                row.upstream
            );
        }
    }
    assert_eq!(rust_rows, 5, "cargo, rustc, rustfmt, rustdoc, clippy");
}

/// A program like [`FAKE`] that also prints the markers it was handed, `[<escape>/
/// <announced>]` ([`PASSTHROUGH_ENV`], [`ANNOUNCED_ENV`]) — how a stub is seen to mark
/// its child.
#[cfg(unix)]
const FAKE_MARKED: &str = "#!/bin/sh\n{ IFS= read -r who; IFS= read -r code; } < \"$0.who\"\n\
                           echo \"$who: $* [${__ATERM_REROUTE_PASSTHROUGH:-}/${__ATERM_REROUTE_ANNOUNCED:-}]\"\n\
                           exit \"${code:-0}\"\n";

/// The stub's own decisions, in a real `/bin/sh`: the escape hatch execs the
/// first upstream copy past the reroute dir (no atpkg needed); otherwise the
/// embedded atpkg gets `__reroute <name> <args>`. With NOTHING reachable the default
/// stub runs upstream after one line, [`ANNOUNCED_ENV`] on the child (2026-09-29; the
/// escape's marker until 2026-10-01),
/// and 127 with none on PATH; a strict stub fails CLOSED with the escape named, exit 2.
/// And cargo's own compile (2026-10-02): a `rustc`/`rustdoc` carrying
/// [`CARGO_CRATE_ENV`] outside any source checkout runs upstream, stamped
/// [`ANNOUNCED_ENV`], before atpkg is asked — the `rustdoc` a `rustup run stable cargo test
/// --doc` spawned used to reach trustdoc (E0514) — and Trust's cargo's, carrying
/// [`TARGO_CRATE_ENV`] too, still reaches atpkg (trustdoc). Inside a checkout:
/// [`an_announced_child_asks_atpkg_only_where_a_checkout_may_be`].
#[cfg(unix)]
#[test]
fn escape_hatch_dispatch_and_fail_closed_run_in_a_real_shell() {
    assert!(FAKE_MARKED.contains(PASSTHROUGH_ENV) && FAKE_MARKED.contains(ANNOUNCED_ENV));
    let l = layout("shell");
    let d = dir(&l);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join(DIR_MARKER_FILE), format!("{STUB_MARKER}\n")).unwrap();
    let upstream_dir = l.prefix.join("upstream");
    std::fs::create_dir_all(&upstream_dir).unwrap();
    let upstream_cargo = upstream_dir.join("cargo");
    std::fs::write(format!("{}.who", upstream_cargo.display()), "upstream\n").unwrap();
    std::os::unix::fs::symlink(assessed("fake-marked", FAKE_MARKED), &upstream_cargo).unwrap();
    let fake = place_fake(&l.prefix.join("fake-atpkg"), "atpkg\n3");
    let path_env = format!("{}:{}", d.display(), upstream_dir.display());
    let run_on = |body: &str, no_reroute: Option<&str>, path: &str| {
        let stub = d.join("cargo");
        std::fs::write(&stub, body).unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut cmd = sh(&stub);
        cmd.args(["build", "--x"])
            .env("PATH", path)
            .env_remove(PASSTHROUGH_ENV);
        if let Some(v) = no_reroute {
            cmd.env(PASSTHROUGH_ENV, v);
        }
        cmd.output().unwrap()
    };
    let run = |body: &str, no_reroute: Option<&str>| run_on(body, no_reroute, &path_env);
    for strict in [false, true] {
        // Escape engaged: upstream runs, the stub's own dir skipped — in either mode.
        let out = run(&stub_body_sh("cargo", &fake, &d, strict), Some("1"));
        assert_eq!(
            out.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "upstream: build --x [1/]\n"
        );
        // Escape present but empty / zero: NOT engaged.
        for v in ["", "0"] {
            let out = run(&stub_body_sh("cargo", &fake, &d, strict), Some(v));
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                "atpkg: __reroute cargo build --x\n"
            );
            assert_eq!(out.status.code(), Some(3));
        }
        // No escape: the embedded atpkg decides.
        let out = run(&stub_body_sh("cargo", &fake, &d, strict), None);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "atpkg: __reroute cargo build --x\n"
        );
    }
    // CARGO'S OWN COMPILE: the variable cargo sets is taken by a compiler stub as the
    // announced marker is — upstream with no line, the marker stamped on the child — in
    // either mode, with atpkg there or gone, outside any source checkout. Unset, atpkg
    // decides (a person's `rustdoc` reaches trustdoc); on the cargo stub it is not cargo's
    // compile; and Trust's cargo's compile, marked twice, is atpkg's too (its `rustdoc` is
    // trustdoc).
    for name in CARGO_COMPILERS {
        let upstream = upstream_dir.join(name);
        std::fs::write(format!("{}.who", upstream.display()), "upstream\n").unwrap();
        std::os::unix::fs::symlink(assessed("fake-marked", FAKE_MARKED), &upstream).unwrap();
    }
    let compile = |name: &str, atpkg: &Path, strict: bool, marks: &[&str]| {
        let stub = d.join(name);
        std::fs::write(&stub, stub_body_sh(name, atpkg, &d, strict)).unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut cmd = sh(&stub);
        cmd.args(["--crate-name", "dep1"])
            .current_dir(&l.prefix)
            .env("PATH", &path_env);
        for mark in marks {
            cmd.env(mark, "dep1");
        }
        let out = cmd.output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    let said = |stdout: String| (stdout, String::new());
    for strict in [false, true] {
        for name in CARGO_COMPILERS {
            for atpkg in [fake.as_path(), Path::new("/gone/atpkg")] {
                assert_eq!(
                    compile(name, atpkg, strict, &[CARGO_CRATE_ENV]),
                    said("upstream: --crate-name dep1 [/1]\n".into()),
                    "{name}, strict {strict}, {}",
                    atpkg.display()
                );
            }
            for marks in [&[][..], &[CARGO_CRATE_ENV, TARGO_CRATE_ENV]] {
                assert_eq!(
                    compile(name, &fake, strict, marks),
                    said(format!("atpkg: __reroute {name} --crate-name dep1\n")),
                    "{name}, strict {strict}, {marks:?}"
                );
            }
        }
        assert_eq!(
            compile("cargo", &fake, strict, &[CARGO_CRATE_ENV]),
            said("atpkg: __reroute cargo --crate-name dep1\n".into())
        );
    }
    // Embedded path dangling and no atpkg on PATH — THE DEFAULT: upstream runs, after
    // one line, with the announced marker stamped on it — never the escape; its exit
    // code is upstream's.
    let gone = stub_body_sh("cargo", Path::new("/gone/atpkg"), &d, false);
    let out = run(&gone, None);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "upstream: build --x [/1]\n",
        "upstream ran, marked"
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stderr).trim_end(),
        unreachable_fallback_message("cargo")
    );
    // ... and with no upstream copy either: 127 and one line, stdout clean.
    let out = run_on(&gone, None, &d.display().to_string());
    assert_eq!(out.status.code(), Some(127), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr).trim_end(),
        unreachable_not_on_path_message("cargo")
    );
    assert!(String::from_utf8_lossy(&out.stdout).is_empty());
    // STRICT: fail closed, exit 2, escape named, upstream never run.
    let out = run(
        &stub_body_sh("cargo", Path::new("/gone/atpkg"), &d, true),
        None,
    );
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(err.trim_end(), unreachable_message("cargo"), "{err}");
    assert!(
        String::from_utf8_lossy(&out.stdout).is_empty(),
        "stdout must stay clean"
    );
    // NAMING NO ATPKG (the empty path, `crate::stub::named(None)`): `ATPKG=''`, which the
    // `[ -x ]` refuses — the first absolute-entry atpkg on PATH answers, and with none the same
    // answer as a dangling path, in each mode.
    let none = stub_body_sh("cargo", crate::stub::named(None), &d, false);
    assert!(none.contains("\nATPKG=''\n"), "{none}");
    let out = run(&none, None);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("running upstream 'cargo'"),
        "{out:?}"
    );
    let strict_none = stub_body_sh("cargo", crate::stub::named(None), &d, true);
    let out = run(&strict_none, None);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("did not run"),
        "{out:?}"
    );
    let path_atpkg = l.prefix.join("path-bin");
    place_fake(&path_atpkg.join("atpkg"), "path atpkg\n3");
    let stub = d.join("cargo");
    std::fs::write(&stub, &none).unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = sh(&stub)
        .args(["build"])
        .env(
            "PATH",
            format!(
                "{}:{}:{}",
                d.display(),
                path_atpkg.display(),
                upstream_dir.display()
            ),
        )
        .env_remove(PASSTHROUGH_ENV)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "path atpkg: __reroute cargo build\n",
        "no atpkg named: the one on PATH answers"
    );
    // ... but never one a RELATIVE or EMPTY entry names (2026-10-01 security review):
    // `.` and a stray `:` both mean the current directory, which here is a tree holding
    // its own `atpkg`. The shell's `command -v` ran it; the absolute-entry walk passes
    // it by and runs upstream, as with no atpkg at all.
    let cwd_atpkg = l.prefix.join("cloned");
    place_fake(&cwd_atpkg.join("atpkg"), "cwd atpkg\n3");
    for relative in [".", ""] {
        let out = sh(&stub)
            .args(["build"])
            .current_dir(&cwd_atpkg)
            .env(
                "PATH",
                format!("{}:{relative}:{}", d.display(), upstream_dir.display()),
            )
            .env_remove(PASSTHROUGH_ENV)
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "upstream: build [/1]\n",
            "PATH entry {relative:?}: {out:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A CHILD OF A COMMAND THE REROUTE RAN, in a real `/bin/sh` (2026-10-01): a
/// `cargo`/`rustc` stub under [`ANNOUNCED_ENV`] runs upstream with no atpkg and no line —
/// unless the directory may be inside a Rust source checkout, PHYSICALLY (a `$PWD`
/// through a symlink from outside hides nothing), where atpkg is asked. Every other stub
/// passes it through like the escape. With atpkg unreachable, a `cargo`/`rustc` inside a
/// Trust checkout does not run — in either marker state — while one in an upstream Rust
/// checkout runs after the fallback's line, as anywhere.
#[cfg(unix)]
#[test]
fn an_announced_child_asks_atpkg_only_where_a_checkout_may_be() {
    let l = layout("announced");
    let d = dir(&l);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join(DIR_MARKER_FILE), format!("{STUB_MARKER}\n")).unwrap();
    let upstream_dir = l.prefix.join("upstream");
    std::fs::create_dir_all(&upstream_dir).unwrap();
    for name in ["cargo", "rustfmt", "rustc", "rustdoc"] {
        let at = upstream_dir.join(name);
        std::fs::write(format!("{}.who", at.display()), "upstream\n").unwrap();
        std::os::unix::fs::symlink(assessed("fake-marked", FAKE_MARKED), &at).unwrap();
    }
    let fake = place_fake(&l.prefix.join("fake-atpkg"), "atpkg\n3");
    // An atpkg that prints the markers it was handed: what a stub tells it.
    let marked_atpkg = l.prefix.join("marked-atpkg");
    std::fs::write(format!("{}.who", marked_atpkg.display()), "atpkg\n3\n").unwrap();
    std::os::unix::fs::symlink(assessed("fake-marked", FAKE_MARKED), &marked_atpkg).unwrap();
    let path_env = format!("{}:{}", d.display(), upstream_dir.display());
    // Two levels inside a checkout whose `src/stage0` names `channel`.
    let checkout = |name: &str, channel: &str| {
        let root = l.prefix.join(name);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("x.py"), "").unwrap();
        std::fs::write(
            root.join("src/stage0"),
            format!("dist_server=x\ncompiler_dist_channel={channel}\n"),
        )
        .unwrap();
        let deep = root.join("compiler/rustc_driver");
        std::fs::create_dir_all(&deep).unwrap();
        deep
    };
    let trust = checkout("trust", "trust");
    let nightly = checkout("nightly", "nightly");
    let plain = l.prefix.join("plain/a/b");
    std::fs::create_dir_all(&plain).unwrap();
    // `outside/link` -> `trust/compiler`: logically outside every checkout.
    std::fs::create_dir_all(l.prefix.join("outside")).unwrap();
    std::os::unix::fs::symlink(
        l.prefix.join("trust/compiler"),
        l.prefix.join("outside/link"),
    )
    .unwrap();
    let logical = l.prefix.join("outside/link/rustc_driver");
    let run = |name: &str, atpkg: &Path, cwd: &Path, env: &[(&str, &str)]| {
        let stub = d.join(name);
        std::fs::write(&stub, stub_body_sh(name, atpkg, &d, false)).unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut cmd = sh(&stub);
        cmd.args(["build", "--x"])
            .current_dir(cwd)
            .env("PATH", &path_env);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).trim_end().to_string(),
        )
    };
    let child = [(ANNOUNCED_ENV, "1")];
    // Upstream ran, silent, with the marker it came with — never the escape.
    let upstream = (
        Some(0),
        "upstream: build --x [/1]\n".to_string(),
        String::new(),
    );
    let asked = |name: &str| {
        (
            Some(3),
            format!("atpkg: __reroute {name} build --x\n"),
            String::new(),
        )
    };
    // Outside every checkout: upstream, with no atpkg.
    assert_eq!(run("cargo", &fake, &plain, &child), upstream);
    // Inside one — Trust's, upstream's, or reached through a symlink from outside with
    // `$PWD` saying so — atpkg judges.
    assert_eq!(run("cargo", &fake, &trust, &child), asked("cargo"));
    assert_eq!(run("cargo", &fake, &nightly, &child), asked("cargo"));
    let pwd = logical.to_str().unwrap();
    assert_eq!(
        run(
            "cargo",
            &fake,
            &logical,
            &[(ANNOUNCED_ENV, "1"), ("PWD", pwd)]
        ),
        asked("cargo"),
        "the walk is physical"
    );
    // Empty or `0` is not engaged: the ordinary dispatch.
    for v in ["", "0"] {
        assert_eq!(
            run("cargo", &fake, &plain, &[(ANNOUNCED_ENV, v)]),
            asked("cargo")
        );
    }
    // A name that is never judged passes the marker through, even in a Trust checkout.
    assert_eq!(run("rustfmt", &fake, &trust, &child), upstream);
    // ATPKG UNREACHABLE: a Trust checkout refuses cargo/rustc, marked or not; an upstream
    // checkout and a plain directory run upstream after the fallback's line.
    let gone = Path::new("/gone/atpkg");
    for env in [&[][..], &child[..]] {
        assert_eq!(
            run("cargo", gone, &trust, env),
            (
                Some(2),
                String::new(),
                unreachable_in_checkout_message("cargo")
            ),
            "{env:?}"
        );
    }
    for cwd in [&nightly, &plain] {
        assert_eq!(
            run("cargo", gone, cwd, &[]),
            (
                Some(0),
                "upstream: build --x [/1]\n".to_string(),
                unreachable_fallback_message("cargo")
            ),
            "{}",
            cwd.display()
        );
    }
    assert_eq!(
        run("rustfmt", gone, &trust, &[]),
        (
            Some(0),
            "upstream: build --x [/1]\n".to_string(),
            unreachable_fallback_message("rustfmt")
        ),
        "only a compiler is judged"
    );
    // A STOCK CARGO'S OWN COMPILE ([`CARGO_CRATE_ENV`] without [`TARGO_CRATE_ENV`]) is a
    // child as an announced one is, whichever way that cargo started: outside a checkout
    // upstream, said nothing, the marker stamped on it; inside one a `rustc` goes to atpkg
    // marked, so it is judged and not announced — a cached `rustc -vV` in the cargo's
    // target dir is no way past the refusal. `rustdoc` is never judged. Trust's cargo's
    // compile, marked twice, is the ordinary dispatch.
    let compile = [(CARGO_CRATE_ENV, "dep1")];
    let trust_compile = [(CARGO_CRATE_ENV, "dep1"), (TARGO_CRATE_ENV, "dep1")];
    for name in ["rustc", "rustdoc"] {
        assert_eq!(run(name, &fake, &plain, &compile), upstream, "{name}");
    }
    assert_eq!(
        run("rustc", &marked_atpkg, &trust, &compile),
        (
            Some(3),
            "atpkg: __reroute rustc build --x [/1]\n".to_string(),
            String::new()
        ),
        "judged, and told it is a child"
    );
    assert_eq!(run("rustdoc", &fake, &trust, &compile), upstream);
    for cwd in [&plain, &trust] {
        assert_eq!(
            run("rustc", &fake, cwd, &trust_compile),
            asked("rustc"),
            "{}",
            cwd.display()
        );
    }
    // ... and with atpkg unreachable a Trust checkout refuses it; elsewhere it runs.
    assert_eq!(
        run("rustc", gone, &trust, &compile),
        (
            Some(2),
            String::new(),
            unreachable_in_checkout_message("rustc")
        )
    );
    assert_eq!(run("rustc", gone, &plain, &compile), upstream);
}

/// WHICH ATPKG A LAY NAMES, through the real [`lay_with`] (D1, 2026-09-25): a
/// `target/` build with a live `atpkg` beside it never takes the stubs from the installed
/// app, nor does a copy with none beside it; the installed app takes them from the
/// `target/` build, and a moved app from its own gone path — the negative control that
/// keeps this from passing for a lay that re-points nothing.
#[cfg(unix)]
#[test]
fn a_target_build_never_takes_the_stubs_from_the_installed_app() {
    let l = layout("which-atpkg");
    // Canonical, as a lay spells an atpkg path.
    let root = std::fs::canonicalize(&l.prefix).unwrap();
    let live = |path: PathBuf| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    };
    let installed = live(root.join("Applications/aterm.app/Contents/MacOS/atpkg"));
    let checkout = root.join("checkout/target");
    std::fs::create_dir_all(&checkout).unwrap();
    std::fs::write(
        checkout.join("CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n# a cache directory tag created by cargo.\n",
    )
    .unwrap();
    let target_build = live(checkout.join("debug/atpkg"));
    // The fixture lives under the temp dir, which is itself transient: this test's
    // "machine" has no temp dir, so each class is only what its own path says.
    let as_process = |offered: &Path| crate::stub::Embedder::offering(offered, &[]);
    let named = |l: &Layout| -> Vec<Option<PathBuf>> {
        TABLE
            .iter()
            .map(|row| {
                let body = std::fs::read_to_string(dir(l).join(row.upstream)).unwrap();
                crate::stub::sh_named_atpkg(&body, "ATPKG")
            })
            .collect()
    };
    let all = |p: &Path| vec![Some(p.to_path_buf()); TABLE.len()];
    // The installed app laid them.
    lay_with(&l, &as_process(&installed), false).unwrap();
    assert_eq!(named(&l), all(&installed));
    // A target/ build, its atpkg live: nothing re-pointed.
    lay_with(&l, &as_process(&target_build), false).unwrap();
    assert_eq!(named(&l), all(&installed), "a target/ build took the stubs");
    // A copy with no atpkg beside it (D1): nothing re-pointed.
    lay_with(&l, &as_process(&root.join("scratch/bin/atpkg")), false).unwrap();
    assert_eq!(named(&l), all(&installed), "a missing atpkg was named");
    // NEGATIVE CONTROLS. With the stubs on the target/ build, the installed app takes
    // them back; and once the app has moved (the old path gone), the new path takes them.
    let fresh = Layout {
        prefix: l.prefix.join("fresh"),
    };
    lay_with(&fresh, &as_process(&target_build), false).unwrap();
    assert_eq!(named(&fresh), all(&target_build), "a vacancy is anyone's");
    lay_with(&fresh, &as_process(&installed), false).unwrap();
    assert_eq!(
        named(&fresh),
        all(&installed),
        "the installed app takes them back"
    );
    let moved = live(root.join("Moved/aterm.app/Contents/MacOS/atpkg"));
    std::fs::remove_file(&installed).unwrap();
    lay_with(&l, &as_process(&moved), false).unwrap();
    assert_eq!(named(&l), all(&moved), "the moved app re-lays every stub");
    // Nothing live on either side: the stubs name none, never the missing path.
    std::fs::remove_file(&moved).unwrap();
    lay_with(&l, &as_process(&root.join("scratch/bin/atpkg")), false).unwrap();
    assert_eq!(named(&l), vec![None; TABLE.len()]);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A fake program at `dir/name` that prints `<who>: <args>` — never a real vendor
/// binary (a quarantined download raises a Gatekeeper dialog; owner, 2026-09-23).
#[cfg(unix)]
fn fake(dir: &Path, name: &str, who: &str) -> PathBuf {
    place_fake(&dir.join(name), who)
}

/// An agents twin shaped like the real one: it execs a (fake) managed target by
/// absolute path. One twin program per agent, shared by every test ([`assessed`]).
#[cfg(unix)]
fn fake_twin(l: &Layout, name: &str) -> PathBuf {
    let target = managed_target(name);
    let twin = assessed(
        &format!("twin-{name}"),
        &format!("#!/bin/sh\nexec '{}' \"$@\"\n", target.display()),
    );
    let at = l.agents_dir().join(name);
    std::fs::create_dir_all(l.agents_dir()).unwrap();
    let _ = std::fs::remove_file(&at);
    std::os::unix::fs::symlink(twin, &at).unwrap();
    at
}

/// The managed target a [`fake_twin`] execs: `managed-<name>` beside the programs, a
/// symlink to [`FAKE`] labelled `managed <name>`. ONE path, shared by every test that
/// makes a twin for `name` — so it is laid only when it is not already exactly that,
/// under one lock: once in a fresh target dir, and never while a test runs through it.
///
/// Every `fake_twin` call used to re-lay it by rename, and on Darwin a call that READS a
/// symlink while `rename(2)` replaces it fails with EINVAL — `readlink`, and `open`,
/// `stat` and `execve` following it — while `lstat` of it, a follow of a symlink whose
/// REGULAR-file target is renamed over, and a renamed-over regular file never fail
/// (measured 2026-09-25, Darwin 25.6 / APFS: 1–2% of calls under a tight rename loop).
/// So a parallel test laying its twin failed this one's `/bin/sh`, started by the
/// kernel for the `#!/bin/sh` behind the link, as `/bin/sh: …/managed-claude: Invalid
/// argument` (exit 126) — or the twin's own `exec`, as `bad interpreter: Invalid
/// argument`. A rename is atomic for the directory entry, not for a walk through it.
#[cfg(unix)]
fn managed_target(name: &str) -> PathBuf {
    static LAYING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let target = programs_dir().join(format!("managed-{name}"));
    let who = PathBuf::from(format!("{}.who", target.display()));
    let label = format!("managed {name}\n");
    let fake = assessed("fake", FAKE);
    let _laying = LAYING.lock().unwrap_or_else(|e| e.into_inner());
    if std::fs::read(&who).ok().as_deref() != Some(label.as_bytes()) {
        lay_by_rename(&who, Some(&label), None);
    }
    if std::fs::read_link(&target).ok().as_deref() != Some(fake.as_path()) {
        lay_by_rename(&target, None, Some(&fake));
    }
    target
}

/// `name` run through `stub` on `path`, with every marker and the escape CLEARED from
/// the inherited environment (this suite may itself run inside aterm) and `env` set.
#[cfg(unix)]
fn run_stub(stub: &Path, path: &std::ffi::OsStr, env: &[(&str, &str)]) -> std::process::Output {
    let mut cmd = sh(stub);
    cmd.args(["--version", "x y"]).env("PATH", path);
    for marker in crate::hooks::AGENTS_MARKERS {
        cmd.env_remove(marker);
    }
    cmd.env_remove(PASSTHROUGH_ENV);
    for (k, v) in env {
        cmd.env(k, v);
    }
    output_within(cmd, 60)
}

/// THE OWNER'S CASE, 2026-09-23, IN A REAL `/bin/sh`: a session shell from 2026-09-10
/// whose PATH is `reroute, ~/.local/bin, /opt/homebrew/bin, pkg/bin` with NO
/// `pkg/agents` runs the managed copy the moment the stub decides — inside aterm, on
/// any one of the four markers. Outside aterm the same stub is a pure pass-through to
/// the user's own copy, even with `agents/` leaked onto PATH ahead of it (owner law,
/// 03513b5d7). `claude` and `codex` alike.
#[cfg(unix)]
#[test]
fn an_agents_stub_runs_the_twin_inside_aterm_and_the_users_own_copy_outside() {
    for name in crate::stub::AGENT_PROGRAMS {
        let l = layout(&format!("agents-stub-{name}"));
        fake_twin(&l, name);
        lay(&l).unwrap();
        let d = dir(&l);
        let stub = d.join(name);
        assert!(is_reroute_stub(&stub), "{name}: a twin earns a stub");
        let local = l.prefix.parent().unwrap().join(format!(
            "atpkg-reroute-agents-local-{name}-{}",
            std::process::id()
        ));
        let brew = local.with_file_name(format!(
            "atpkg-reroute-agents-brew-{name}-{}",
            std::process::id()
        ));
        fake(&local, name, &format!("local {name}"));
        fake(&brew, name, &format!("brew {name}"));
        fake(&l.bin_dir(), name, &format!("bin {name}"));
        let stale =
            std::env::join_paths([d.clone(), local.clone(), brew.clone(), l.bin_dir()]).unwrap();
        let leaked = std::env::join_paths([
            d.clone(),
            l.agents_dir(),
            local.clone(),
            brew.clone(),
            l.bin_dir(),
        ])
        .unwrap();
        let stdout = |out: &std::process::Output| {
            assert!(
                out.status.success(),
                "{name}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        let managed = format!("managed {name}: --version x y\n");
        let own = format!("local {name}: --version x y\n");
        // Inside aterm, on EACH marker alone: the twin, whatever PATH puts ahead.
        for marker in crate::hooks::AGENTS_MARKERS {
            for path in [&stale, &leaked] {
                let out = run_stub(&stub, path, &[(marker, "1")]);
                assert_eq!(stdout(&out), managed, "{name} under {marker}");
            }
        }
        // A marker set but EMPTY is not set (`-n` on the joined word).
        let out = run_stub(&stub, &stale, &[("ATERM_CHILD", "")]);
        assert_eq!(stdout(&out), own, "{name}: an empty marker");
        // Outside aterm: the user's own copy — never the managed one, even leaked.
        for path in [&stale, &leaked] {
            assert_eq!(stdout(&run_stub(&stub, path, &[])), own, "{name} outside");
        }
        // TERM_PROGRAM is not a marker (a shell rc can reset it).
        let out = run_stub(&stub, &stale, &[("TERM_PROGRAM", "aterm")]);
        assert_eq!(stdout(&out), own, "{name}: TERM_PROGRAM decides nothing");
        // The --no-reroute marker does not apply to these stubs (manager's ruling,
        // 2026-09-23): it is the upstream Rust names' switch, and a foreign copy under it
        // in an old shell but the managed one in a new shell is the bug this closes.
        // Inside aterm the twin, outside the user's own — whatever its value, on any PATH.
        for v in ["1", "", "0", "yes"] {
            for path in [&stale, &leaked] {
                let out = run_stub(&stub, path, &[("ATERM_CHILD", "1"), (PASSTHROUGH_ENV, v)]);
                assert_eq!(stdout(&out), managed, "{name}: escape {v:?} inside aterm");
                let out = run_stub(&stub, path, &[(PASSTHROUGH_ENV, v)]);
                assert_eq!(stdout(&out), own, "{name}: escape {v:?} outside aterm");
            }
        }
        // The twin gone: inside aterm too, a pass-through.
        std::fs::remove_file(l.agents_dir().join(name)).unwrap();
        let out = run_stub(&stub, &stale, &[("ATERM_CHILD", "1")]);
        assert_eq!(stdout(&out), own, "{name}: no twin");
        // Nothing of the user's on PATH: `bin/` answers, as it would with no stub.
        let bin_only = std::env::join_paths([d.clone(), l.bin_dir()]).unwrap();
        let out = run_stub(&stub, &bin_only, &[]);
        assert_eq!(
            stdout(&out),
            format!("bin {name}: --version x y\n"),
            "{name}"
        );
        // Nothing at all, and the twin gone: exit 127, one line naming the install,
        // stdout clean — inside aterm and out.
        let nothing = std::env::join_paths([d.clone()]).unwrap();
        for env in [&[("ATERM_CHILD", "1")][..], &[]] {
            let out = run_stub(&stub, &nothing, env);
            assert_eq!(out.status.code(), Some(127), "{name} {env:?}");
            assert!(out.stdout.is_empty(), "{name} {env:?}");
            let err = String::from_utf8_lossy(&out.stderr);
            assert_eq!(
                err.trim_end(),
                agents_not_installed_message(name),
                "{name} {env:?}: one line"
            );
        }
        let _ = std::fs::remove_dir_all(&local);
        let _ = std::fs::remove_dir_all(&brew);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }
}

/// NO RECURSION: a second reroute directory on PATH — another prefix's, holding its
/// own `claude` stub, a symlink to ours, ours with a trailing slash — `agents/` under
/// another spelling, and a symlink to the stub FILE in an ordinary directory are all
/// walked past, inside aterm and out, in bounded time.
#[cfg(unix)]
#[test]
fn an_agents_stub_never_runs_another_stub_or_itself() {
    let l = layout("agents-no-recursion");
    let other = layout("agents-no-recursion-other");
    fake_twin(&l, "claude");
    fake_twin(&other, "claude");
    lay(&l).unwrap();
    lay(&other).unwrap();
    // The other prefix's twin gone: its stub passes through, and must not come back.
    std::fs::remove_file(other.agents_dir().join("claude")).unwrap();
    let local = l.prefix.parent().unwrap().join(format!(
        "atpkg-reroute-agents-norec-local-{}",
        std::process::id()
    ));
    fake(&local, "claude", "local claude");
    let link = l.prefix.join("link-to-reroute");
    std::os::unix::fs::symlink(dir(&l), &link).unwrap();
    let agents_link = l.prefix.join("link-to-agents");
    std::os::unix::fs::symlink(l.agents_dir(), &agents_link).unwrap();
    let mut trailing = dir(&l).into_os_string();
    trailing.push("/");
    // The stub itself under another name, in an ordinary directory (`ln -s "$(command
    // -v claude)" ~/bin/claude`): no marker there, so only knowing it for a stub by its
    // content ([`sh_skip_a_stub`]) stops the loop.
    let user_bin = l.prefix.join("user-bin");
    std::fs::create_dir_all(&user_bin).unwrap();
    std::os::unix::fs::symlink(dir(&l).join("claude"), user_bin.join("claude")).unwrap();
    let path = std::env::join_paths([
        dir(&other).into_os_string(),
        link.clone().into_os_string(),
        trailing,
        agents_link.clone().into_os_string(),
        user_bin.clone().into_os_string(),
        local.clone().into_os_string(),
    ])
    .unwrap();
    let stub = dir(&l).join("claude");
    let out = run_stub(&stub, &path, &[]);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "local claude: --version x y\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = run_stub(&stub, &path, &[("ATERM_SESSION_ID", "s-1")]);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "managed claude: --version x y\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // …and the OTHER prefix's stub (its twin gone, so it passes through), entered
    // first, walks past every reroute spelling of ours too. (It skips its OWN
    // `agents/` only — our `agents/` is left off this PATH, the nested-prefix corner
    // the pass-through does not claim to cover.)
    let path = std::env::join_paths([
        dir(&other).into_os_string(),
        link.clone().into_os_string(),
        dir(&l).into_os_string(),
        local.clone().into_os_string(),
    ])
    .unwrap();
    let out = run_stub(&dir(&other).join("claude"), &path, &[("ATERM_CHILD", "1")]);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "local claude: --version x y\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&local);
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&other.prefix);
}

/// `lay` lays an agent program's stub exactly while its twin stands: none without one
/// (a stub would make `command -v claude` answer on a machine with no claude), never
/// over a foreign file, swept when the twin goes, and a converged directory is left
/// alone byte for byte.
#[cfg(unix)]
#[test]
fn lay_follows_the_agents_twin_and_never_claims_a_foreign_name() {
    use std::os::unix::fs::MetadataExt as _;
    let l = layout("agents-lay");
    lay(&l).unwrap();
    let d = dir(&l);
    for name in crate::stub::AGENT_PROGRAMS {
        assert!(!d.join(name).exists(), "{name}: no twin, no stub");
        assert_eq!(stub_state(&l, name), StubState::Missing);
    }
    fake_twin(&l, "claude");
    lay(&l).unwrap();
    assert!(is_reroute_stub(&d.join("claude")));
    assert_eq!(
        std::fs::read_to_string(d.join("claude")).unwrap(),
        agents_stub_body_sh("claude", &l.agents_dir())
    );
    assert!(!d.join("codex").exists(), "codex has no twin");
    assert_eq!(agents_routes(&l), vec!["claude"]);
    // Converged: a second lay rewrites nothing.
    let ino = std::fs::metadata(d.join("claude")).unwrap().ino();
    lay(&l).unwrap();
    assert_eq!(std::fs::metadata(d.join("claude")).unwrap().ino(), ino);
    // A foreign `reroute/codex` is never claimed, even once codex has a twin.
    std::fs::write(d.join("codex"), "#!/bin/sh\necho mine\n").unwrap();
    fake_twin(&l, "codex");
    lay(&l).unwrap();
    assert_eq!(
        std::fs::read_to_string(d.join("codex")).unwrap(),
        "#!/bin/sh\necho mine\n"
    );
    assert_eq!(stub_state(&l, "codex"), StubState::Foreign);
    // The twin gone: its stub is swept at the next lay; the TABLE's stay.
    std::fs::remove_file(l.agents_dir().join("claude")).unwrap();
    lay(&l).unwrap();
    assert!(!d.join("claude").exists(), "the stub followed the twin out");
    assert!(d.join("cargo").exists() && d.join("codex").exists());
    // `remove_all` takes an agents stub like any other of ours.
    fake_twin(&l, "claude");
    lay(&l).unwrap();
    remove_all(&l);
    assert!(!d.join("claude").exists());
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// The stub's shape: marked on line 2 and recognized as ours, no store target for the
/// shim parser, the marker word rendered from THE list the rc hook's gate is (so the
/// shell-start and exec-time decisions read the same markers), neither `TERM_PROGRAM`
/// nor the `--no-reroute` marker read, and no marker ever SET — the stub reads the markers,
/// never writes one.
#[test]
fn the_agents_stub_reads_the_hooks_markers_and_is_never_a_managed_shim() {
    let body = agents_stub_body_sh("claude", Path::new("/p/agents"));
    assert_eq!(body.lines().nth(1), Some(STUB_MARKER));
    assert!(
        crate::platform::parse_sh_shim_target(&body).is_none(),
        "{body}"
    );
    assert!(!body.contains("exec '"), "{body}");
    assert!(
        body.contains(&format!(
            "if [ -n \"{}\" ]",
            crate::hooks::agents_markers_sh()
        )),
        "{body}"
    );
    assert_eq!(
        crate::hooks::AGENTS_MARKERS,
        &["ATERM_AGENTS_DIR", "ATERM_CHILD", "ATERM_SESSION_ID"]
    );
    for marker in crate::hooks::AGENTS_MARKERS {
        assert!(
            body.contains(&format!("${{{marker}-}}")),
            "{marker}: {body}"
        );
        assert!(
            !body.contains(&format!("{marker}=")),
            "{marker} is never set: {body}"
        );
    }
    let posix = crate::hooks::hook_files(
        Path::new("/p/bin"),
        Path::new("/p/agents"),
        Path::new("/p/reroute"),
    )
    .into_iter()
    .find(|(n, _)| n.ends_with(".zsh"))
    .unwrap()
    .1;
    assert!(
        posix.contains(&format!(
            "case \"{}\" in",
            crate::hooks::agents_markers_sh()
        )),
        "the rc hook's gate is the same word: {posix}"
    );
    assert!(!body.contains("TERM_PROGRAM"), "{body}");
    assert!(
        body.contains("'/p/agents'") && body.contains("'claude'"),
        "{body}"
    );
    assert!(body.contains(DIR_MARKER_FILE), "{body}");
    assert!(
        !body.contains(PASSTHROUGH_ENV),
        "the markers alone decide; the Rust names' escape is never read: {body}"
    );
    assert!(body.ends_with("exit 127\n"), "{body}");
}

/// The Rust restatement the doctor and `which` answer with agrees with the stub:
/// markers by `-n` (an empty one is unset), nothing else read, and the PATH a
/// pass-through walks.
#[cfg(unix)]
#[test]
fn the_stubs_decision_restated_matches_the_stub() {
    let env = |pairs: &[(&str, &str)]| {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        StubEnv::read(move |k| {
            owned
                .iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| std::ffi::OsString::from(v))
        })
    };
    assert_eq!(env(&[]), StubEnv::default());
    assert!(!env(&[("ATERM_CHILD", "")]).in_aterm);
    assert!(!env(&[("TERM_PROGRAM", "aterm")]).in_aterm);
    for marker in crate::hooks::AGENTS_MARKERS {
        assert!(env(&[(marker, "x")]).in_aterm, "{marker}");
    }
    assert!(
        !env(&[(PASSTHROUGH_ENV, "1")]).in_aterm,
        "the escape is not a marker, and the stub reads nothing else"
    );

    let l = layout("agents-restated");
    fake_twin(&l, "claude");
    lay(&l).unwrap();
    let inside = StubEnv { in_aterm: true };
    assert!(agents_stub_takes_twin(&l, "claude", inside));
    assert!(!agents_stub_takes_twin(&l, "claude", StubEnv::default()));
    assert!(!agents_stub_takes_twin(&l, "codex", inside), "no twin");
    let local = l.prefix.join("local");
    fake(&local, "claude", "local");
    let stale = std::env::join_paths([dir(&l), local.clone(), l.bin_dir()]).unwrap();
    assert_eq!(
        agents_stub_first_on_path(&l, "claude", Some(&stale)),
        Some(dir(&l).join("claude"))
    );
    let other_first = std::env::join_paths([local.clone(), dir(&l)]).unwrap();
    assert_eq!(
        agents_stub_first_on_path(&l, "claude", Some(&other_first)),
        None
    );
    let leaked =
        std::env::join_paths([dir(&l), l.agents_dir(), local.clone(), l.bin_dir()]).unwrap();
    assert_eq!(
        pass_through_path(&l, Some(&leaked)),
        Some(std::env::join_paths([local.clone(), l.bin_dir()]).unwrap())
    );
    assert_eq!(
        without_reroute_dirs(Some(&leaked)),
        Some(std::env::join_paths([l.agents_dir(), local, l.bin_dir()]).unwrap())
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}
