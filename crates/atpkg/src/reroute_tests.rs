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

/// Replace whatever is at `at` with `bytes`, or with a symlink to `target`, by rename, so
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

/// The one fake program: it prints the label on the first line of the `.who` file beside
#[cfg(unix)]
const FAKE: &str = "#!/bin/sh\n{ IFS= read -r who; IFS= read -r code; } < \"$0.who\"\n\
                    echo \"$who: $*\"\nexit \"${code:-0}\"\n";

/// [`FAKE`] placed at `at` (a symlink, replacing whatever was there), labelled `who`
#[cfg(unix)]
fn place_fake(at: &Path, who: &str) -> PathBuf {
    std::fs::create_dir_all(at.parent().unwrap()).unwrap();
    let _ = std::fs::remove_file(at);
    std::fs::write(format!("{}.who", at.display()), format!("{who}\n")).unwrap();
    std::os::unix::fs::symlink(assessed("fake", FAKE), at).unwrap();
    at.to_path_buf()
}

/// `stub` run the way the kernel runs its `#!/bin/sh` line (see [`assessed`]). Only a stub
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
    cmd.arg(stub)
        .env_remove(PASSTHROUGH_ENV)
        .env_remove(ANNOUNCED_ENV)
        .env_remove(CARGO_CRATE_ENV)
        .env_remove(TARGO_CRATE_ENV);
    cmd
}

/// `lean f.lean` must reach `clean check f.lean` — the branded tool is
#[test]
fn a_verb_first_direct_row_routes_a_bare_source_file_through_its_verb() {
    let lean = row_for("lean").expect("lean is a rerouted name");
    let Policy::Direct { source_verb, .. } = lean.policy else {
        panic!("lean is a DIRECT row");
    };
    assert!(source_verb.is_some(), "lean declares a source verb");

    assert_eq!(
        direct_args(source_verb, &args(&["f.lean"])),
        args(&["check", "f.lean"])
    );
    assert_eq!(
        direct_args(source_verb, &args(&["a/b/Main.lean", "--json"])),
        args(&["check", "a/b/Main.lean", "--json"])
    );
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

/// The commands a signpost spells, one per line: each line's text before the padding that
fn spelled(text: &str) -> Vec<&str> {
    text.lines()
        .map(|line| line.trim().split("  ").next().unwrap_or(""))
        .collect()
}

/// Every signpost spells the caller's own command in a form the Trust tool ACCEPTS
#[test]
fn every_signpost_spells_a_command_the_trust_tool_accepts() {
    for (name, call, wanted, never) in [
        (
            "cargo",
            &["build", "--release"][..],
            &[
                "targo trust build --release",
                "targo --unverified build --release",
            ][..],
            &[][..],
        ),
        (
            "cargo",
            &["run", "-p", "x"],
            &["targo --unverified run -p x"],
            &["targo trust"],
        ),
        ("cargo", &[], &["targo"], &["targo trust"]),
        (
            "cargo",
            &["clippy", "--workspace", "--", "-D", "warnings"],
            &["targo tippy --workspace -- -D warnings"],
            &["targo trust"],
        ),
        (
            "cargo",
            &["fmt", "--check"],
            &["targo fmt --check"],
            &["targo trust"],
        ),
        (
            "cargo",
            &["metadata", "--format-version", "1"],
            &["targo metadata --format-version 1"],
            &["targo trust", "targo --unverified"],
        ),
        (
            "cargo",
            &["--version"],
            &["targo --version"],
            &["targo trust"],
        ),
        (
            "cargo",
            &["nextest", "run"],
            &["targo nextest run"],
            &["targo trust"],
        ),
        (
            "rustc",
            &["main.rs"],
            &["trustc main.rs", "trustc -Ztrust-verify=off main.rs"],
            &[],
        ),
        ("tlc", &["Spec.tla"], &["ty Spec.tla"], &[]),
    ] {
        let row = row_for(name).unwrap();
        let Policy::Signpost { lanes, .. } = row.policy else {
            panic!("{name} is a signpost row")
        };
        let text = signpost_message_in(row, lanes, &args(call), None, None, None);
        let commands = spelled(&text);
        for want in wanted {
            assert!(commands.contains(want), "{name} {call:?}: {want}\n{text}");
        }
        for not in never {
            assert!(
                !commands.iter().any(|c| c.starts_with(not)),
                "{name} {call:?}: {not}\n{text}"
            );
        }
    }
}

/// A second `lay` may be in flight: its `.<name>.stub-<pid>` temp and the
#[cfg(unix)]
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

/// The stub is invisible to every managed-shim sweep and known for ours by its marker
#[test]
fn stub_body_is_marked_and_never_parses_as_a_managed_shim() {
    let bodies = [
        stub_body_sh(
            "cargo",
            Path::new("/opt/aterm/atpkg"),
            Path::new("/p/reroute"),
            false,
        ),
        stub_body_sh(
            "cargo",
            Path::new("/opt/aterm/atpkg"),
            Path::new("/p/reroute"),
            true,
        ),
        agents_stub_body_sh("claude", Path::new("/p/agents")),
    ];
    for body in bodies {
        assert_eq!(body.lines().nth(1), Some(STUB_MARKER), "{body}");
        assert!(
            crate::platform::parse_sh_shim_target(&body).is_none(),
            "{body}"
        );
        assert!(body.len() < MAX_STUB_BYTES, "{}", body.len());
    }
}

/// A lay follows `[reroute] strict`: flipping it rewrites every row's stub, and
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

/// A second `lay` over a laid directory writes nothing: every stub and the marker keep
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

#[cfg(unix)]
#[test]
fn lay_claims_absent_and_own_files_never_foreign_ones_and_remove_all_sweeps_only_ours() {
    let l = layout("lay");
    let d = dir(&l);
    std::fs::create_dir_all(&d).unwrap();
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
    remove_all(&l);
    assert!(d.join("rustc").exists(), "foreign file swept");
    assert!(!d.join("cargo").exists());
    assert!(!is_reroute_dir(&d), "remove_all must take the marker file");
    assert!(d.exists(), "a non-empty directory (the foreign file) stays");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// The CPU time a pid has consumed, as `ps` spells it, or `None` when `ps` cannot answer.
#[cfg(unix)]
fn child_cpu_time(pid: u32) -> Option<String> {
    let out = std::process::Command::new("/bin/ps")
        .args(["-o", "time=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!t.is_empty()).then_some(t)
}

/// Run `cmd`, killing it after `secs`: an exec loop fails the test rather than hanging it.
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

/// With a second reroute directory on PATH (another prefix's, a symlink to ours, ours with
#[cfg(unix)]
#[test]
fn escape_walk_skips_every_reroute_directory_not_just_its_own() {
    let l = layout("second-spelling");
    let other = layout("second-spelling-other-prefix");
    lay(&l).unwrap();
    lay(&other).unwrap();
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

/// A copy of a stub is a stub: `cp <prefix>/reroute/* ~/bin/` copies every stub but not the
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
#[test]
fn a_recorded_decline_stops_lay() {
    let l = layout("declined");
    std::fs::write(l.declined(), b"# removed on purpose\n").unwrap();
    lay(&l).unwrap();
    assert!(!dir(&l).exists(), "a declined toolset must not get stubs");
    assert!(states(&l).iter().all(|(_, s)| *s == StubState::Missing));
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// rust-analyzer's lane-free `cargo metadata` / `locate-project` / `version` / `--version`
#[test]
fn lane_free_queries_run_targo_quietly() {
    for call in [
        &["metadata", "--format-version", "1"][..],
        &["locate-project", "--workspace"],
        &["version"],
        &["--version"],
    ] {
        for upstream in [Some(UP), None] {
            assert_eq!(
                plan_on("cargo", call, true, upstream, &["targo"]),
                Plan::Quiet {
                    copy: store("targo"),
                    args: args(call),
                },
                "{call:?} with upstream {upstream:?}"
            );
        }
    }
    assert_eq!(
        plan_on("cargo", &["+trust", "metadata"], true, Some(UP), &["targo"]),
        Plan::Quiet {
            copy: store("targo"),
            args: args(&["metadata"]),
        }
    );
    assert!(matches!(
        plan_on(
            "cargo",
            &["+1.97.1", "metadata"],
            true,
            Some(UP),
            &["targo"]
        ),
        Plan::Upstream { .. }
    ));
    assert!(matches!(
        plan_on("cargo", &["build"], true, Some(UP), &["targo"]),
        Plan::Upstream { say: Some(_), .. }
    ));
    assert!(matches!(
        plan_on("cargo", &["metadata"], true, None, &[]),
        Plan::Missing { .. }
    ));
    assert!(matches!(
        plan_on(
            "rustc",
            &["--version"],
            true,
            Some(UP),
            &["targo", "trustc"]
        ),
        Plan::Upstream { .. }
    ));
}

/// A managed copy at `/store/<tool>`, declaring no environment.
fn store(tool: &str) -> Managed {
    Managed {
        target: PathBuf::from(format!("/store/{tool}")),
        env: crate::shim_env::ShimEnv::NONE,
    }
}

/// [`plan`] for `name call…` on a machine whose PATH walk finds `upstream` and whose
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

/// [`plan_on`] under `[reroute] strict = true`, where the refusals stand.
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

/// What the test machine's rustup resolves `name` to: Trust for `+trust…`, stock for any
fn resolved(name: &str, toolchain: Option<&str>) -> Resolved {
    match toolchain {
        Some(tc) if rust_lane::is_trust_channel(tc) => trust_at(name, Picked::CommandLine),
        Some(tc) => stock_at(name, tc, Picked::CommandLine),
        None => stock_at(name, "1.97.1", Picked::Unsaid),
    }
}

/// [`plan`] for `name call…` with `upstream` at [`UP`], nothing branded installed, the
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

/// Nothing is announced that will not run: with nothing to run, one answer says so (never
#[test]
fn a_plan_with_nothing_to_run_says_only_that() {
    for (name, call, upstream, branded) in [
        ("tlc", &["Spec.tla"][..], None, &[][..]),
        ("cargo", &["build"], None, &[]),
        ("cargo", &["clippy"], None, &[]),
        ("cargo", &["+stable", "build"], None, &[]),
        ("rustfmt", &["x.rs"], None, &[]),
        ("lean", &["f.lean"], None, &[]),
        ("cargo", &["trust", "build"], Some(UP), &[]),
        ("z3", &["-in"], None, &["ay"]),
    ] {
        for announce in [true, false] {
            assert!(
                matches!(
                    plan_on(name, call, announce, upstream, branded),
                    Plan::Missing { .. }
                ),
                "{name} {call:?}, announce {announce}: nothing runs, and the setting does \
                 not silence that"
            );
        }
    }
}

/// A DIRECT row whose Trust tool is missing runs the upstream tool the caller named, with
#[test]
fn a_missing_trust_tool_runs_upstream_with_a_line_and_strict_refuses() {
    for (name, branded) in [
        ("clippy", "tippy"),
        ("rustfmt", "trustfmt"),
        ("rustdoc", "trustdoc"),
        ("lean", "clean"),
    ] {
        let call = ["f.lean", "--flag"];
        for (announce, said) in [(true, true), (false, false)] {
            let Plan::Upstream {
                say,
                target,
                args: argv,
            } = plan_on(name, &call, announce, Some(UP), &[])
            else {
                panic!("{name}: the default runs upstream");
            };
            assert_eq!(
                (say.is_some(), target, argv),
                (said, PathBuf::from(UP), args(&call)),
                "{name}: the setting silences the line, never the run"
            );
        }
        assert!(
            matches!(
                plan_strict(name, &call, Some(UP), &[]),
                Plan::Missing { .. }
            ),
            "{name}: strict refuses as before"
        );
        assert!(
            matches!(
                plan_strict(name, &call, Some(UP), &[branded]),
                Plan::Branded { .. }
            ),
            "{name}"
        );
    }
}

/// z3: announce, naming `ay`, then run upstream with the caller's own arguments; the
#[test]
fn z3_runs_upstream_naming_ay_and_strict_refuses() {
    for installed in [&["ay"][..], &[][..]] {
        let Plan::Upstream {
            say: Some(said),
            target,
            args: argv,
        } = plan_on("z3", &["-smt2", "q.smt2"], true, Some(UP), installed)
        else {
            panic!("ay installed {installed:?}: z3 announces and runs upstream");
        };
        assert_eq!(
            (target, argv),
            (PathBuf::from(UP), args(&["-smt2", "q.smt2"]))
        );
        assert_eq!(
            said.contains("`aterm pkg install ay`"),
            installed.is_empty(),
            "{said}"
        );
        let Plan::Missing { say } = plan_on("z3", &["-in"], true, None, installed) else {
            panic!("no z3 on PATH is 127's line: {installed:?}");
        };
        assert_eq!(
            say.contains("`aterm pkg install ay`"),
            installed.is_empty(),
            "{say}"
        );
        assert_eq!(
            plan_on("z3", &["-in"], false, Some(UP), installed),
            Plan::Upstream {
                say: None,
                target: PathBuf::from(UP),
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
    }
}

/// [`storeless_plan`] for `name call…`: upstream at `upstream`, the name resolving to
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

/// No store resolves (`atpkg __reroute` under `env -i`): the upstream copy runs after one
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
    assert!(matches!(
        storeless_on(
            "cargo",
            &["build"],
            true,
            true,
            Some(UP),
            &never,
            Some("/t")
        ),
        Plan::Refuse { .. }
    ));
}

/// No store is no way round the checkout: a `cargo`/`rustc` with no store is resolved and
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
    assert!(say.contains("Trust 7d5533b2"), "{say}");
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

/// A child of a command the reroute ran ([`ANNOUNCED_ENV`]) is never announced again and
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
    assert_eq!(
        child(
            "cargo",
            &["build"],
            &|_, _| trust_at("cargo", Picked::Environment),
            &in_checkout
        ),
        upstream(&["build"])
    );
    assert_eq!(
        child(
            "rustc",
            &["+1.97.1", "-V"],
            &|_, _| panic!("outside a checkout a child is not resolved"),
            &|| None,
        ),
        upstream(&["+1.97.1", "-V"])
    );
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
    let Plan::Upstream {
        say: Some(say),
        target,
        args: argv,
    } = lost(false, Some(PathBuf::from(UP)))("rustfmt", &["x.rs"])
    else {
        panic!("a lost shim with an upstream copy runs upstream");
    };
    assert_eq!((target, argv), (PathBuf::from(UP), args(&["x.rs"])));
    assert!(say.contains("`aterm pkg repair`"), "{say}");
    for (strict, upstream) in [(true, Some(PathBuf::from(UP))), (false, None)] {
        let Plan::Missing { say } = lost(strict, upstream)("rustfmt", &["x.rs"]) else {
            panic!("strict={strict}: nothing runs");
        };
        assert!(say.contains("`aterm pkg repair`"), "strict={strict}: {say}");
    }
    assert_eq!(
        lost(false, Some(PathBuf::from(UP)))("cargo", &["trust", "build"]),
        Plan::Missing {
            say: lost_shim_message("cargo", "targo"),
        }
    );
}

/// A PENDING STUB HOLDS NO CALL HOSTAGE (2026-10-15): while `trust` is still to come, a
#[test]
fn a_pending_branded_tool_runs_upstream_rather_than_wait_for_it() {
    let stub = PathBuf::from("/prefix/bin/trustfmt");
    let targo_stub = PathBuf::from("/prefix/bin/targo");
    let pending = |strict: bool, upstream: Option<PathBuf>| {
        let stub = stub.clone();
        let targo_stub = targo_stub.clone();
        move |row: &str, call: &[&str]| {
            let machine = Machine {
                passthrough: false,
                announced: false,
                announce: &|| true,
                strict: &|| strict,
                upstream: &|| upstream.clone(),
                branded: &|tool, program| {
                    assert_eq!(program, "trust", "{tool}");
                    Install::Pending(if tool == "targo" {
                        targo_stub.clone()
                    } else {
                        stub.clone()
                    })
                },
                pin: &|| None,
                resolve: &|_, _| panic!("nothing is resolved"),
                trust_checkout: &|| None,
            };
            plan(row_for(row).unwrap(), &args(call), &machine)
        }
    };
    assert_eq!(
        pending(false, Some(PathBuf::from(UP)))("rustfmt", &["x.rs"]),
        Plan::Upstream {
            say: Some(fallback_pending_message("rustfmt", "trustfmt", "trust")),
            target: PathBuf::from(UP),
            args: args(&["x.rs"]),
        }
    );
    for (strict, upstream) in [(true, Some(PathBuf::from(UP))), (false, None)] {
        assert_eq!(
            pending(strict, upstream)("rustfmt", &["x.rs"]),
            Plan::Shim {
                shim: stub.clone(),
                args: args(&["x.rs"]),
            },
            "strict={strict}: the stub is what is left to run"
        );
    }
    assert_eq!(
        pending(false, Some(PathBuf::from(UP)))("cargo", &["trust", "build"]),
        Plan::Shim {
            shim: targo_stub.clone(),
            args: args(&["trust", "build"]),
        },
        "an explicit lane has no upstream to fall back to"
    );
}

/// [`installed`] on a real store: a laid shim runs; with it lost, the live build says
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
        Install::Pending(l.shim(&targo))
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A tool is unserved here only when its program's recorded row says the index publishes
#[test]
fn a_tool_is_unserved_only_when_its_row_says_so_and_nothing_runs_it() {
    let l = layout("unserved");
    std::fs::create_dir_all(&l.prefix).unwrap();
    let write_row = |state: &str| {
        let mut programs = std::collections::BTreeMap::new();
        programs.insert(
            "trust".to_string(),
            crate::ProgramStatus {
                installed_build: None,
                state: state.to_string(),
                tree_root: String::new(),
            },
        );
        crate::status::write(
            &l,
            &crate::Status {
                schema: 1,
                programs,
                ..Default::default()
            },
        )
        .unwrap();
    };
    assert_eq!(
        unserved_here(&l, "targo", "trust"),
        None,
        "no row yet: the tool may still arrive"
    );
    let unavailable = crate::state::unavailable("aarch64-unknown-linux-gnu", "");
    write_row(&unavailable);
    assert_eq!(
        unserved_here(&l, "targo", "trust").as_deref(),
        Some(unavailable.as_str()),
        "nothing here and the row says no build for this target"
    );
    let targo = crate::store::ToolName::new("targo").unwrap();
    crate::stub::write_pending_stub(&l, &targo).unwrap();
    assert_eq!(
        unserved_here(&l, "targo", "trust").as_deref(),
        Some(unavailable.as_str()),
        "a pending stub waits on the same row"
    );
    write_row(&crate::state::held_failed(
        "x",
        "now: aterm pkg install trust",
    ));
    assert_eq!(
        unserved_here(&l, "targo", "trust"),
        None,
        "a held row is a fault on a served target, not an unserved one"
    );
    write_row(&unavailable);
    assert_eq!(
        unserved_here(&l, "tippy", "ay"),
        None,
        "another program's row says nothing of this one"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// What runs, and with which arguments: upstream gets the caller's own (a `+trust…`
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
    let (first, lanes) = say.split_once('\n').expect("a block");
    assert!(first.contains("Trust 7d5533b2"), "{say}");
    assert!(
        lanes.contains("targo trust build ") && !lanes.contains("+trust"),
        "{say}"
    );
    let Plan::Upstream { say: Some(say), .. } =
        plan_on("cargo", &["+trust", "clippy"], true, Some(UP), &[])
    else {
        panic!("`cargo +trust clippy` announces and runs upstream");
    };
    assert!(say.contains("targo tippy"), "{say}");
    let Plan::Branded {
        copy, args: argv, ..
    } = plan_on("clippy", &["x"], true, None, &["tippy"])
    else {
        panic!("an installed DIRECT tool runs");
    };
    assert_eq!((copy, argv), (store("tippy"), args(&["x"])));
    let Plan::Branded {
        copy, args: argv, ..
    } = plan_on(
        "cargo",
        &["+trust", "trust", "build"],
        true,
        Some(UP),
        &["targo"],
    )
    else {
        panic!("the explicit lane runs targo");
    };
    assert_eq!((copy, argv), (store("targo"), args(&["trust", "build"])));
}

/// `cargo +1.97.1 test -p x` in a session: the first line says what runs, the Trust
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
    assert!(text.contains("STOCK cargo 1.97.1"), "{text}");
    assert!(text.contains("targo trust test -p x"), "{text}");
    assert!(text.contains("targo --unverified test -p x"), "{text}");
    assert!(
        !text.contains("+1.97.1 test"),
        "the Trust spelling carries no directive: {text}"
    );
    let pinned = Pin {
        channel: "trust".into(),
        source: rust_lane::PinSource::File("/r/rust-toolchain.toml".into()),
    };
    assert_eq!(
        plan_pinned("cargo", &call, true, Some(UP), &[], Some(pinned)),
        plan_on("cargo", &call, true, Some(UP), &[])
    );
    assert_eq!(
        plan_on("cargo", &call, false, Some(UP), &[]),
        Plan::Upstream {
            say: None,
            target: PathBuf::from(UP),
            args: args(&call),
        }
    );
}

/// A DIRECT Rust row never hands a rustup directive to its branded tool: a stock one
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
    assert!(spelled(&text).contains(&"trustfmt --check x.rs"), "{text}");
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
    assert_eq!(
        plan_on("rustfmt", &call, true, None, &["trustfmt"]),
        Plan::Missing {
            say: not_on_path_message("rustfmt"),
        }
    );
    let Plan::Branded { args: argv, .. } = plan_on("lean", &["+x"], true, Some(UP), &["clean"])
    else {
        panic!("lean runs clean");
    };
    assert_eq!(argv, args(&["+x"]));
}

/// A directory that pins a STOCK channel says so under the lanes: a pin moves only
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
        text.contains("/r/rust-toolchain.toml"),
        "the pin is named: {text}"
    );
    let trust = Pin {
        channel: "trust".into(),
        source: rust_lane::PinSource::Env,
    };
    assert_eq!(
        plan_pinned("cargo", &["test"], true, Some(UP), &[], Some(trust)),
        plan_on("cargo", &["test"], true, Some(UP), &[])
    );
    let Plan::Upstream {
        say: Some(text), ..
    } = plan_pinned(
        "cargo",
        &["+trust", "test"],
        true,
        Some(UP),
        &[],
        Some(stock),
    )
    else {
        panic!("`cargo +trust test` runs upstream");
    };
    assert!(text.contains("targo trust test"), "{text}");
    assert!(!text.contains("rust-toolchain.toml"), "{text}");
    let pin = Pin::for_dir(None, Some("1.95.0")).unwrap();
    assert_eq!(
        (pin.channel.as_str(), &pin.source),
        ("1.95.0", &rust_lane::PinSource::Env)
    );
    assert_eq!(Pin::for_dir(None, Some("")), None);
}

/// THE NAMING OF 2026-09-29 (plan item MP-9), at the decision point: `cargo`/`rustc` are
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
    assert!(text.contains("Trust 7d5533b2"), "{text}");
    assert!(spelled(&text).contains(&"trustc -vV"), "{text}");
    assert!(
        !text.contains("upstream"),
        "nothing calls Trust upstream: {text}"
    );
    assert!(text.contains("/t/trust-7d5533b2/bin/rustc"), "{text}");

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
    assert!(text.contains("/usr/bin/up -vV exited 3"), "{text}");
}

/// Resolving costs a `rustup which` and maybe a `-vV`, so it is asked only where a line
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
    assert!(matches!(
        plan_resolving("cargo", &["trust", "build"], true, &never, Some("/t")),
        Plan::Missing { .. }
    ));
}

/// Inside a Trust source checkout only Trust runs by name: a `cargo`/`rustc` that resolves
#[test]
fn inside_a_trust_checkout_only_trust_runs_by_name() {
    let stock = |_: &Path, tc: Option<&str>| resolved("cargo", tc);
    let refused = plan_resolving("cargo", &["build"], true, &stock, Some("/t"));
    let Plan::Refuse { say } = &refused else {
        panic!("stock is refused inside the checkout: {refused:?}");
    };
    for named in ["STOCK cargo 1.97.1", "/t", "targo trust build"] {
        assert!(say.contains(named), "{named}: {say}");
    }
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
    assert!(say.contains("STOCK "), "{say}");
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

/// A program like [`FAKE`] that also prints the markers it was handed, `[<escape>/
#[cfg(unix)]
const FAKE_MARKED: &str = "#!/bin/sh\n{ IFS= read -r who; IFS= read -r code; } < \"$0.who\"\n\
                           echo \"$who: $* [${__ATERM_REROUTE_PASSTHROUGH:-}/${__ATERM_REROUTE_ANNOUNCED:-}]\"\n\
                           exit \"${code:-0}\"\n";

/// The stub's own decisions, in a real `/bin/sh`: the escape hatch execs the
#[cfg(unix)]
#[test]
fn escape_hatch_dispatch_and_fail_closed_run_in_a_real_shell() {
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
        for v in ["", "0"] {
            let out = run(&stub_body_sh("cargo", &fake, &d, strict), Some(v));
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                "atpkg: __reroute cargo build --x\n"
            );
            assert_eq!(out.status.code(), Some(3));
        }
        let out = run(&stub_body_sh("cargo", &fake, &d, strict), None);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "atpkg: __reroute cargo build --x\n"
        );
    }
    let stub = d.join("cargo");
    std::fs::write(&stub, stub_body_sh("cargo", &fake, &d, false)).unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = sh(&stub)
        .args(["build"])
        .env("PATH", &path_env)
        .env(CARGO_CRATE_ENV, "dep1")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "atpkg: __reroute cargo build\n"
    );
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
    let out = run_on(&gone, None, &d.display().to_string());
    assert_eq!(out.status.code(), Some(127), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).is_empty());
    let out = run(
        &stub_body_sh("cargo", Path::new("/gone/atpkg"), &d, true),
        None,
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stdout).is_empty(),
        "stdout must stay clean"
    );
    let none = stub_body_sh("cargo", crate::stub::named(None), &d, false);
    assert!(none.contains("\nATPKG=''\n"), "{none}");
    let out = run(&none, None);
    assert_eq!(
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned()
        ),
        (Some(0), "upstream: build --x [/1]\n".to_string()),
        "{out:?}"
    );
    let strict_none = stub_body_sh("cargo", crate::stub::named(None), &d, true);
    let out = run(&strict_none, None);
    assert_eq!(out.status.code(), Some(2));
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

/// A child of a command the reroute ran, in a real `/bin/sh`: a `cargo`/`rustc` stub under
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
    let marked_atpkg = l.prefix.join("marked-atpkg");
    std::fs::write(format!("{}.who", marked_atpkg.display()), "atpkg\n3\n").unwrap();
    std::os::unix::fs::symlink(assessed("fake-marked", FAKE_MARKED), &marked_atpkg).unwrap();
    let path_env = format!("{}:{}", d.display(), upstream_dir.display());
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
    assert_eq!(run("cargo", &fake, &plain, &child), upstream);
    assert_eq!(run("cargo", &fake, &trust, &child), asked("cargo"));
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
    assert_eq!(run("rustfmt", &fake, &trust, &child), upstream);
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

/// Which atpkg a lay names, through the real [`lay_with`]: a `target/` build with a live
#[cfg(unix)]
#[test]
fn a_target_build_never_takes_the_stubs_from_the_installed_app() {
    let l = layout("which-atpkg");
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
    lay_with(&l, &as_process(&installed), false).unwrap();
    assert_eq!(named(&l), all(&installed));
    lay_with(&l, &as_process(&target_build), false).unwrap();
    assert_eq!(named(&l), all(&installed), "a target/ build took the stubs");
    lay_with(&l, &as_process(&root.join("scratch/bin/atpkg")), false).unwrap();
    assert_eq!(named(&l), all(&installed), "a missing atpkg was named");
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
    std::fs::remove_file(&moved).unwrap();
    lay_with(&l, &as_process(&root.join("scratch/bin/atpkg")), false).unwrap();
    assert_eq!(named(&l), vec![None; TABLE.len()]);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A fake program at `dir/name` that prints `<who>: <args>`, never a real vendor binary (a
#[cfg(unix)]
fn fake(dir: &Path, name: &str, who: &str) -> PathBuf {
    place_fake(&dir.join(name), who)
}

/// An agents twin shaped like the real one: it execs a (fake) managed target by
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

/// The managed target a [`fake_twin`] execs: `managed-<name>` beside the programs, a symlink
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

/// `name` run through `stub` on `path`, with every marker and the escape cleared from the
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

/// A long-lived session shell whose PATH is `reroute, ~/.local/bin, /opt/homebrew/bin,
#[cfg(unix)]
#[test]
fn an_agents_stub_runs_the_twin_inside_aterm_and_the_users_own_copy_outside() {
    crate::testdir::each_side_by_side(crate::stub::AGENT_PROGRAMS, |_, name| {
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
        for marker in crate::hooks::AGENTS_MARKERS {
            for path in [&stale, &leaked] {
                let out = run_stub(&stub, path, &[(marker, "1")]);
                assert_eq!(stdout(&out), managed, "{name} under {marker}");
            }
        }
        let out = run_stub(&stub, &stale, &[("ATERM_CHILD", "")]);
        assert_eq!(stdout(&out), own, "{name}: an empty marker");
        for path in [&stale, &leaked] {
            assert_eq!(stdout(&run_stub(&stub, path, &[])), own, "{name} outside");
        }
        let out = run_stub(&stub, &stale, &[("TERM_PROGRAM", "aterm")]);
        assert_eq!(stdout(&out), own, "{name}: TERM_PROGRAM decides nothing");
        let out = run_stub(
            &stub,
            &leaked,
            &[("ATERM_CHILD", "1"), (PASSTHROUGH_ENV, "1")],
        );
        assert_eq!(stdout(&out), managed, "{name}: the escape inside aterm");
        let out = run_stub(&stub, &leaked, &[(PASSTHROUGH_ENV, "1")]);
        assert_eq!(stdout(&out), own, "{name}: the escape outside aterm");
        std::fs::remove_file(l.agents_dir().join(name)).unwrap();
        let out = run_stub(&stub, &stale, &[("ATERM_CHILD", "1")]);
        assert_eq!(stdout(&out), own, "{name}: no twin");
        let bin_only = std::env::join_paths([d.clone(), l.bin_dir()]).unwrap();
        let out = run_stub(&stub, &bin_only, &[]);
        assert_eq!(
            stdout(&out),
            format!("bin {name}: --version x y\n"),
            "{name}"
        );
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
    });
}

/// No recursion: a second reroute directory on PATH (another prefix's, holding its own
#[cfg(unix)]
#[test]
fn an_agents_stub_never_runs_another_stub_or_itself() {
    let l = layout("agents-no-recursion");
    let other = layout("agents-no-recursion-other");
    fake_twin(&l, "claude");
    fake_twin(&other, "claude");
    lay(&l).unwrap();
    lay(&other).unwrap();
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
    assert!(!d.join("codex").exists(), "codex has no twin");
    assert_eq!(agents_routes(&l), vec!["claude"]);
    let ino = std::fs::metadata(d.join("claude")).unwrap().ino();
    lay(&l).unwrap();
    assert_eq!(std::fs::metadata(d.join("claude")).unwrap().ino(), ino);
    std::fs::write(d.join("codex"), "#!/bin/sh\necho mine\n").unwrap();
    fake_twin(&l, "codex");
    lay(&l).unwrap();
    assert_eq!(
        std::fs::read_to_string(d.join("codex")).unwrap(),
        "#!/bin/sh\necho mine\n"
    );
    assert_eq!(stub_state(&l, "codex"), StubState::Foreign);
    std::fs::remove_file(l.agents_dir().join("claude")).unwrap();
    lay(&l).unwrap();
    assert!(!d.join("claude").exists(), "the stub followed the twin out");
    assert!(d.join("cargo").exists() && d.join("codex").exists());
    fake_twin(&l, "claude");
    lay(&l).unwrap();
    remove_all(&l);
    assert!(!d.join("claude").exists());
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// The Rust restatement the doctor and `which` answer with agrees with the stub:
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
    assert!(!env(&[]).in_aterm);
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

/// An upstream cargo gets the build cache only where its release reads it
#[test]
fn an_upstream_cargo_gets_the_build_cache_only_where_its_release_reads_it() {
    use crate::build_cache::Tool;
    let typed = args(&["build"]);
    let decided = |tool: Tool| args(&["--config", &format!("{tool:?}"), "build"]);
    let on = |identity: Identity, decide: &Decide<'_>| {
        let machine = Machine {
            passthrough: false,
            announced: false,
            announce: &|| true,
            strict: &|| false,
            upstream: &|| Some(PathBuf::from(UP)),
            branded: &|_, _| Install::Absent,
            pin: &|| None,
            resolve: &|_, _| Resolved {
                real: PathBuf::from(UP),
                rustup: None,
                identity: identity.clone(),
            },
            trust_checkout: &|| None,
        };
        upstream_cargo_args(Path::new(UP), typed.clone(), &machine, decide)
    };
    let release = |r: &str| match stock_at("cargo", "x", Picked::Unsaid).identity {
        Identity::Known(vv) => Identity::Known(Vv {
            release: r.into(),
            ..vv
        }),
        other @ Identity::Unknown { .. } => other,
    };
    let always = |_: &[String], tool: Tool| Some(decided(tool));
    let trust = || trust_at("cargo", Picked::Unsaid).identity;
    assert_eq!(on(release("1.97.1"), &always), decided(Tool::Cargo));
    assert_eq!(on(trust(), &always), decided(Tool::Targo));
    let verified_lane = |_: &[String], tool: Tool| (tool == Tool::Cargo).then(|| decided(tool));
    assert_eq!(on(trust(), &verified_lane), typed);
    assert_eq!(on(release("1.85.0"), &always), typed);
    let unknown = Identity::Unknown {
        why: "/usr/bin/up -vV exited 3".into(),
    };
    assert_eq!(on(unknown, &always), typed);
    assert_eq!(on(release("1.97.1"), &|_, _| None), typed);
}
