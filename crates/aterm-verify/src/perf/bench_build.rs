// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The bench lanes' build support, shared by the MEASURED lanes `fabric`, `restore-e2e` and
//! `commons-quiet` and by the FLOOR lane `fabric-counts`, which builds and runs the fabric bench
//! as `fabric` does: the arm's cargo profile read back from cargo's own plan and config before
//! anything runs ([`prepare`]), the aterm the bench launches pinned by path and identity
//! ([`Built`], [`another_build`]), the bench run with its words in one environment variable
//! ([`run_bench`]), and the provenance an entry records (the load average, the CPUs, the next
//! free entry name). [`run_bench`] admits a run by what it reads ([`Entry`]): `fabric`'s and
//! `restore-e2e`'s timed runs on fresh measurement-entry evidence, `fabric-counts`' counted runs
//! at the load they find, so a change to it or to [`prepare`] changes that floor lane too.

use std::io::BufRead as _;
use std::path::{Path, PathBuf};

use aterm_uds::spawnfd::{Command, Stdio};

use super::arena::{self, Halt};
use super::workspace_root;
use crate::perf::driver::CargoDriver;

/// THE BENCHES' OWN TABLE of arms, mounted from the fabric bench: the cargo profile
/// each builds with, where its binaries land, and the opt-level it promises.
#[path = "../../../aterm/tests/fabric_bench/profile.rs"]
pub(crate) mod bench_profile;

pub(crate) mod flags;
pub(crate) mod units;

use bench_profile::Profile;

/// How many lines of a failed bench's output a lane quotes.
pub(crate) const TAIL: usize = 60;

/// The aterm a bench launches: cargo's own report of where it built it, its identity then, and
/// the build line an entry records.
pub(crate) struct Built {
    pub path: PathBuf,
    pub identity: String,
    pub lane_build: String,
}

/// Build `build_args`'s bench for `profile`'s arm, after reading back from cargo's config and
/// unit plan that the arm's build is what the arm says: the targo it runs with, the aterm it
/// built, and the load average just before the build.
///
/// # Errors
/// A build that is not the arm's, or one that fails.
pub(crate) fn prepare<'a>(
    driver: &'a CargoDriver,
    profile: Profile,
    build_args: &[&str],
) -> Result<(Targo<'a>, Built, String), Halt> {
    let targo = Targo::new(driver);
    let removed: Vec<String> = std::env::vars_os()
        .filter_map(|(key, _)| key.into_string().ok())
        .filter(|key| flags::is_env_source(key))
        .collect();
    let sources = config_rustflags(&targo)?;
    if let Some(why) = flags::refusal(&sources) {
        return Err(Halt::Fail(format!(
            "the `{}` arm's build is not what the arm says: {why}",
            profile.name()
        )));
    }
    let audit = audit(&targo, "bench", build_args)?;
    if let Some(why) = audit.refusal(units::Promise::of(profile)) {
        return Err(Halt::Fail(format!(
            "the `{}` arm's build is not what the arm says: {why}",
            profile.name()
        )));
    }
    let mut build = format!(
        "`{}` — {}. Cargo's plan, read before the run: {}; {}",
        profile.name(),
        profile.describe(),
        audit.summary(),
        flags::describe(&sources)
    );
    if !removed.is_empty() {
        build.push_str(&format!(" (the caller's {} removed)", removed.join(", ")));
    }
    build.push_str("; its identity record the constant one (no identity input reaches it)");
    eprintln!("  {build}");
    let load_before = load_average();
    let built = build_bench(&targo, build_args)?;
    if Profile::of_binary(&built) != Some(profile) {
        return Err(Halt::Fail(format!(
            "cargo built the `{}` arm's aterm at {}, outside the arm's `{}` directory",
            profile.name(),
            built.display(),
            profile.dir()
        )));
    }
    let identity = bench_profile::identity(&built)
        .map_err(|why| Halt::Fail(format!("the aterm cargo just built cannot be read: {why}")))?;
    Ok((
        targo,
        Built {
            path: built,
            identity,
            lane_build: build,
        },
        load_before,
    ))
}

/// How a bench run is admitted: whether what it reads is time or counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    /// It times: fresh postcompile measurement-entry evidence ([`super::measurement_entry`]) is
    /// taken before it runs and must still qualify when it exits.
    Timed,
    /// It counts (`fabric-counts`): no admission, so it runs at the load it finds, which the lane
    /// reports beside its counts.
    Counted,
}

/// Build `build_args`'s bench without executing it, then execute Cargo's exact artifact with the
/// words of `knobs` (the variable and its words) and the separate OS-native output path in its
/// named variable (`output`), admitted as `entry` says: a timed run first obtains fresh
/// measurement-entry evidence. Echo stderr lines beginning with `progress`, and return stdout.
///
/// # Errors
/// A bench that does not start, or exits unsuccessfully (`what` names it, with both tails); a
/// timed run whose admission is refused or lapsed.
pub(crate) fn run_bench(
    targo: &Targo<'_>,
    build_args: &[&str],
    knobs: (&str, &str),
    output: (&str, &Path),
    progress: &str,
    what: &str,
    entry: Entry,
) -> Result<String, Halt> {
    let (env, words) = knobs;
    let mut bench_args = vec!["-q", "--no-run", "--message-format=json-render-diagnostics"];
    bench_args.extend_from_slice(build_args);
    eprintln!("  $ {}", targo.display("bench", &bench_args));
    let messages = targo.output("bench", &bench_args, "the measurement executable build")?;
    let name = build_args
        .windows(2)
        .find(|args| args[0] == "--bench")
        .map(|args| args[1])
        .ok_or_else(|| Halt::Fail("measurement build names no bench target".into()))?;
    let executable = built_executable(&messages, name, "bench")?;
    eprintln!(
        "  $ {env}=\"{words}\" {}={:?} {} --bench",
        output.0,
        output.1,
        executable.display()
    );
    let mut command = crate::exec::check_command(&executable);
    targo.environment(&mut command);
    command.current_dir(workspace_root()).arg("--bench");
    let admitted = match entry {
        Entry::Timed => Some(super::measurement_entry().map_err(Halt::Timing)?),
        Entry::Counted => None,
    };
    set_bench_environment(&mut command, env, words, output);
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not execute {}: {e}", executable.display()))?;
    let stdout = child.stdout.take().map(|mut out| {
        std::thread::spawn(move || {
            use std::io::Read as _;
            let mut text = String::new();
            let _ = (&mut out).take(16 * 1024 * 1024).read_to_string(&mut text);
            let _ = std::io::copy(&mut out, &mut std::io::sink());
            text
        })
    });
    let mut tail: Vec<String> = Vec::new();
    if let Some(err) = child.stderr.take() {
        use std::io::Read as _;
        const MAX_BENCH_STDERR_LINE: usize = 64 << 10;
        let mut reader = std::io::BufReader::new(err);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match (&mut reader)
                .take((MAX_BENCH_STDERR_LINE as u64) + 1)
                .read_until(b'\n', &mut buf)
            {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            if !buf.ends_with(b"\n") && buf.len() > MAX_BENCH_STDERR_LINE {
                while let Ok(chunk) = reader.fill_buf() {
                    if chunk.is_empty() {
                        break;
                    }
                    let nl = chunk.iter().position(|&b| b == b'\n');
                    let len = chunk.len();
                    if let Some(nl) = nl {
                        reader.consume(nl + 1);
                        break;
                    }
                    reader.consume(len);
                }
            }
            let line = String::from_utf8_lossy(&buf)
                .trim_end_matches(['\r', '\n'])
                .to_string();
            if line.starts_with(progress) {
                eprintln!("  {line}");
            }
            tail.push(line);
            if tail.len() > TAIL {
                tail.remove(0);
            }
        }
    }
    let status = child
        .wait()
        .map_err(|e| format!("could not wait for measurement executable: {e}"))?;
    let stdout = stdout.and_then(|h| h.join().ok()).unwrap_or_default();
    if let Some(admitted) = &admitted {
        admitted
            .timing_admission()
            .map_err(|why| Halt::Timing(why.to_string()))?;
    }
    if !status.success() {
        let out_tail: Vec<&str> = stdout.lines().rev().take(TAIL).collect();
        return Err(Halt::Fail(format!(
            "{what} exited {:?}\n--- stderr (tail) ---\n{}\n--- stdout (tail) ---\n{}",
            status.code(),
            tail.join("\n"),
            out_tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        )));
    }
    Ok(stdout)
}

/// `targo` as a lane runs it: in the workspace's build directory, with the caller's rustflags
/// sources and identity inputs removed, so every run builds the constant identity record and no
/// commit between the runs of one set reruns the `aterm` build script.
pub(crate) struct Targo<'a> {
    driver: &'a CargoDriver,
    build_dir: Option<String>,
}

impl<'a> Targo<'a> {
    /// Keep a fixture's intermediates separate from the executable being measured.
    #[cfg(target_os = "macos")]
    pub(crate) fn with_build_dir(mut self, path: &Path) -> Result<Self, String> {
        let path = path.to_str().ok_or("fixture build path is not UTF-8")?;
        self.build_dir = Some(format!(
            "build.build-dir={}",
            aterm_toml::Value::String(path.to_owned())
        ));
        Ok(self)
    }

    pub(crate) fn new(driver: &'a CargoDriver) -> Self {
        Targo {
            driver,
            build_dir: crate::perf::driver::workspace_build_dir().map(str::to_string),
        }
    }

    fn lead(&self, verb: &str) -> Vec<String> {
        let mut lead = Vec::new();
        if let Some(value) = &self.build_dir {
            lead.extend(["--config".to_string(), value.clone()]);
        }
        lead.extend(
            CargoDriver::lane_args(verb)
                .iter()
                .map(|s| (*s).to_string()),
        );
        lead.push(verb.to_string());
        lead
    }

    pub(crate) fn command(&self, verb: &str) -> Command {
        let mut cmd = crate::exec::check_command(&self.driver.program);
        cmd.args(self.lead(verb)).current_dir(workspace_root());
        self.environment(&mut cmd);
        cmd
    }

    pub(crate) fn environment(&self, cmd: &mut Command) {
        for (key, _) in std::env::vars_os() {
            if key.to_str().is_some_and(flags::is_env_source) {
                cmd.env_remove(key);
            }
        }
        for key in aterm_stamp::input::ALL {
            cmd.env_remove(key);
        }
    }

    pub(crate) fn display(&self, verb: &str, args: &[&str]) -> String {
        let mut parts = vec![self.driver.program.display().to_string()];
        parts.extend(self.lead(verb));
        parts.extend(args.iter().map(|s| (*s).to_string()));
        parts.join(" ")
    }

    fn output(&self, verb: &str, args: &[&str], what: &str) -> Result<String, Halt> {
        let out = self
            .command(verb)
            .args(args)
            .output()
            .map_err(|e| format!("could not run targo for {what}: {e}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let tail: Vec<&str> = err.lines().rev().take(TAIL).collect();
            return Err(Halt::Fail(format!(
                "`{}` exited {:?}\n{}",
                self.display(verb, args),
                out.status.code(),
                tail.into_iter().rev().collect::<Vec<_>>().join("\n")
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

pub(crate) fn audit(
    targo: &Targo<'_>,
    verb: &str,
    build_args: &[&str],
) -> Result<units::Audit, Halt> {
    let mut plan_args = build_args.to_vec();
    plan_args.extend(["--unit-graph", "-Z", "unstable-options"]);
    let graph = targo.output(verb, &plan_args, "the build plan")?;
    units::Audit::of(&graph)
        .map_err(|why| Halt::Fail(format!("the build plan does not read: {why}")))
}

pub(crate) fn config_rustflags(targo: &Targo<'_>) -> Result<Vec<flags::Source>, Halt> {
    let table = |name: &str| -> Result<Option<String>, Halt> {
        let args = ["-Z", "unstable-options", "get", "--format", "json", name];
        let out = targo
            .command("config")
            .args(args)
            .output()
            .map_err(|e| format!("could not run targo config get: {e}"))?;
        let err = String::from_utf8_lossy(&out.stderr);
        if out.status.success() {
            Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned()))
        } else if err.contains("is not set") {
            Ok(None)
        } else {
            Err(Halt::Fail(format!(
                "`{}` exited {:?}\n{err}",
                targo.display("config", &args),
                out.status.code()
            )))
        }
    };
    flags::config_sources(table("build")?.as_deref(), table("target")?.as_deref())
        .map_err(|why| Halt::Fail(format!("cargo's config does not read: {why}")))
}

fn build_bench(targo: &Targo<'_>, build_args: &[&str]) -> Result<PathBuf, Halt> {
    let mut args = vec!["-q"];
    args.extend_from_slice(build_args);
    args.extend(["--no-run", "--message-format", "json-render-diagnostics"]);
    eprintln!("  $ {}", targo.display("bench", &args));
    let messages = targo.output("bench", &args, "the bench's build")?;
    built_binary(&messages, "aterm").map_err(|why| Halt::Fail(format!("the bench's build: {why}")))
}

fn built_binary(messages: &str, name: &str) -> Result<PathBuf, String> {
    built_executable(messages, name, "bin")
}

fn set_bench_environment(command: &mut Command, env: &str, words: &str, output: (&str, &Path)) {
    command.env(env, words).env(output.0, output.1);
}

/// Cargo's exact executable artifact, matched by both target name and target kind.
pub(crate) fn built_executable(messages: &str, name: &str, kind: &str) -> Result<PathBuf, String> {
    built_executable_checked(messages, name, kind, |_| Ok(()))
}

/// A normal debug executable whose compiler profile enables the checkout's lab identity.
/// Test harnesses and missing or disabled debug assertions cannot supply that fixture.
pub(crate) fn built_debug_app(messages: &str, name: &str) -> Result<PathBuf, String> {
    built_executable_checked(messages, name, "bin", |message| {
        let profile = message.get("profile");
        if profile
            .and_then(|p| p.get("debug_assertions"))
            .and_then(aterm_json::Value::as_bool)
            != Some(true)
            || profile
                .and_then(|p| p.get("test"))
                .and_then(aterm_json::Value::as_bool)
                != Some(false)
        {
            return Err(format!(
                "`{name}` lab fixture requires a normal executable with debug assertions enabled"
            ));
        }
        Ok(())
    })
}

fn built_executable_checked(
    messages: &str,
    name: &str,
    kind: &str,
    accept: impl Fn(&aterm_json::Value) -> Result<(), String>,
) -> Result<PathBuf, String> {
    let mut selected: Option<PathBuf> = None;
    for line in messages.lines() {
        let Ok(wire) = aterm_json::from_str::<crate::compiler_message::UnitMessage>(line) else {
            continue;
        };
        let message = match wire {
            crate::compiler_message::UnitMessage::Unit(value) => value,
            crate::compiler_message::UnitMessage::Other => continue,
            crate::compiler_message::UnitMessage::Ambiguous { .. } => {
                return Err("compiler reported ambiguous message fields".into());
            }
        };
        if message.get("reason").and_then(aterm_json::Value::as_str) != Some("compiler-artifact") {
            continue;
        }
        let Some(path) = message
            .get("executable")
            .and_then(aterm_json::Value::as_str)
        else {
            continue;
        };
        let kinds = crate::compiler_message::artifact_target_kinds(&message)?;
        let target = message.get("target");
        let named = target
            .and_then(|t| t.get("name"))
            .and_then(aterm_json::Value::as_str)
            == Some(name);
        let bin = kinds.contains(&kind);
        if named && bin {
            accept(&message)?;
            if selected
                .as_ref()
                .is_some_and(|prior| prior.as_os_str() != std::ffi::OsStr::new(path))
            {
                return Err(format!(
                    "compiler reported conflicting executables for `{name}`"
                ));
            }
            selected.get_or_insert_with(|| PathBuf::from(path));
        }
    }
    selected.ok_or_else(|| format!("cargo reported no `{name}` binary"))
}

/// Why a bench's report is not of `built` (`identity`): the aterm it says it launched, and that
/// file's identity at its start and end; `None` when they are the build's.
pub(crate) fn another_build(
    report: &aterm_json::Value,
    built: &Path,
    identity: &str,
) -> Option<String> {
    let ran = report
        .get("aterm_binary")
        .and_then(aterm_json::Value::as_str)
        .unwrap_or("(none reported)");
    if Path::new(ran) != built {
        return Some(format!(
            "the bench launched {ran}, not the {} cargo just built",
            built.display()
        ));
    }
    let seen = report.get("aterm_binary_identity");
    for moment in ["start", "end"] {
        let at = seen
            .and_then(|s| s.get(moment))
            .and_then(aterm_json::Value::as_str)
            .unwrap_or("(none reported)");
        if at != identity {
            return Some(format!(
                "{ran} at the bench's {moment} was {at}, not the build's {identity}: another build \
                 wrote that path"
            ));
        }
    }
    None
}

/// The next free entry in `dir` for `date`: `<date>-<stem>.md` and `.json`, then `-2`, `-3`, ….
pub(crate) fn ledger_paths(dir: &Path, date: &str, stem: &str) -> (PathBuf, PathBuf) {
    let mut n = 1;
    loop {
        let name = if n == 1 {
            format!("{date}-{stem}")
        } else {
            format!("{date}-{stem}-{n}")
        };
        let md = dir.join(format!("{name}.md"));
        let json = dir.join(format!("{name}.json"));
        if !md.exists() && !json.exists() {
            return (md, json);
        }
        n += 1;
    }
}

/// `vm.loadavg`'s three figures, or `unavailable`.
pub(crate) fn load_average() -> String {
    arena::stdout_of(
        crate::exec::check_command(aterm_uds::os_path!("sysctl")).args(["-n", "vm.loadavg"]),
    )
    .map(|s| {
        s.trim_matches(|c| c == '{' || c == '}' || c == ' ')
            .to_string()
    })
    .filter(|s| !s.is_empty())
    .unwrap_or_else(|| "unavailable".to_string())
}

pub(crate) fn cpus() -> String {
    let logical = std::thread::available_parallelism()
        .map_or_else(|_| "?".to_string(), |n| n.get().to_string());
    match arena::stdout_of(
        crate::exec::check_command(aterm_uds::os_path!("sysctl"))
            .args(["-n", "hw.perflevel0.physicalcpu"]),
    )
    .filter(|s| !s.is_empty())
    {
        Some(performance) => format!("{logical} logical, {performance} performance cores"),
        None => format!("{logical} logical"),
    }
}

/// A ledger entry's head: what it measured, on which machine and tree, by which command and
/// build, under what load.
pub(crate) struct Provenance<'a> {
    /// The entry's title, before its date (`Fabric: pingpong`).
    pub heading: &'a str,
    /// The README that keeps the ledger's protocol.
    pub protocol: &'a str,
    pub date: &'a str,
    pub machine: &'a str,
    pub macos: &'a str,
    pub aterm: &'a str,
    pub astream: &'a str,
    pub spec: &'a str,
    pub profile: &'a str,
    pub elapsed_s: f64,
    pub command: &'a str,
    pub build: &'a str,
    pub thermal: &'a str,
    pub cpus: &'a str,
    pub load: &'a str,
}

impl Provenance<'_> {
    /// The head as the entry's Markdown opens.
    pub(crate) fn render(&self) -> String {
        format!(
            "<!-- SPDX-License-Identifier: Apache-2.0 -->\n\
             <!-- Copyright 2026 Andrew Yates -->\n\
             \n\
             # {heading} — {date}\n\
             \n\
             Protocol: `{protocol}`. MEASURED, not judged. Append-only: a superseding run gets a \
             new file.\n\
             \n\
             - **Machine:** {machine}\n\
             - **macOS:** {macos}\n\
             - **aterm:** {aterm} ({profile})\n\
             - **astream:** vendor/astream at {astream}\n\
             - **Run:** `{command}` ({spec}), {elapsed:.1} s\n\
             - **Build:** {build}\n\
             - **Thermal:** {thermal}\n\
             - **CPUs:** {cpus}\n\
             - **Load average (1/5/15 min):** {load}\n",
            heading = self.heading,
            protocol = self.protocol,
            date = self.date,
            machine = self.machine,
            macos = self.macos,
            aterm = self.aterm,
            profile = self.profile,
            astream = self.astream,
            command = self.command,
            spec = self.spec,
            elapsed = self.elapsed_s,
            build = self.build,
            thermal = self.thermal,
            cpus = self.cpus,
            load = self.load,
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lab_app_requires_positive_debug_profile_evidence_and_no_test_harness() {
        let artifact = |profile: &str| {
            format!(
                "{{\"reason\":\"compiler-artifact\",\"target\":{{\"name\":\"aterm\",\"kind\":[\"bin\"]}},\"executable\":\"/fixture/aterm\",\"profile\":{profile}}}"
            )
        };
        let valid = artifact(r#"{"debug_assertions":true,"test":false}"#);
        assert_eq!(
            built_debug_app(&valid, "aterm").unwrap(),
            PathBuf::from("/fixture/aterm")
        );
        for profile in [
            r#"{"debug_assertions":false,"test":false}"#,
            r#"{"debug_assertions":true,"test":true}"#,
            r#"{"debug_assertions":true}"#,
            r#"{"test":false}"#,
            r#"{"debug_assertions":"true","test":false}"#,
            r#"{"debug_assertions":false,"debug_assertions":true,"test":false}"#,
        ] {
            assert!(
                built_debug_app(&artifact(profile), "aterm").is_err(),
                "{profile}"
            );
        }
        let release = artifact(r#"{"debug_assertions":false,"test":false}"#);
        assert!(built_executable(&release, "aterm", "bin").is_ok());
    }

    #[test]
    fn the_built_aterm_is_the_binary_cargo_reports() {
        let messages = [
            r#"{"reason":"compiler-artifact","target":{"kind":["lib"],"name":"aterm"},"executable":null,"fresh":true}"#,
            r#"{"reason":"compiler-artifact","target":{"kind":["bin"],"name":"aterm"},"filenames":["/w/target/opt-test/aterm"],"executable":"/w/target/opt-test/aterm","fresh":false}"#,
            r#"{"reason":"compiler-artifact","target":{"kind":["bench"],"name":"fabric_bench"},"executable":"/c/.cargo-build/opt-test/deps/fabric_bench-7b2c","fresh":false}"#,
            r#"{"reason":"build-finished","success":true}"#,
            "not json: a stray line",
        ]
        .join("\n");
        assert_eq!(
            built_binary(&messages, "aterm"),
            Ok(PathBuf::from("/w/target/opt-test/aterm"))
        );
        assert!(built_binary(&messages, "aterm-ctl").is_err());
        let nameless = r#"{"reason":"compiler-artifact","target":{"kind":["bin"],"name":"aterm"},"executable":null}"#;
        assert!(built_binary(nameless, "aterm").is_err());
        assert_eq!(
            built_executable(&messages, "fabric_bench", "bench"),
            Ok(PathBuf::from(
                "/c/.cargo-build/opt-test/deps/fabric_bench-7b2c"
            ))
        );
        assert!(built_executable(&messages, "fabric_bench", "example").is_err());
        let examples = r#"{"reason":"compiler-artifact","target":{"kind":["example"],"name":"probe"},"executable":"/cache/probe","fresh":true}"#;
        assert_eq!(
            built_executable(examples, "probe", "example"),
            Ok(PathBuf::from("/cache/probe"))
        );
        assert!(built_executable(examples, "probe", "lib").is_err());
        let library_tests=[
            r#"{"reason":"compiler-artifact","target":{"kind":["lib"],"name":"aterm_db"},"profile":{"test":false},"executable":null}"#,
            r#"{"reason":"compiler-artifact","target":{"kind":["lib"],"name":"aterm_db"},"profile":{"test":true},"executable":"/cache/deps/aterm_db-test"}"#,
        ].join("\n");
        assert_eq!(
            built_executable(&library_tests, "aterm_db", "lib"),
            Ok(PathBuf::from("/cache/deps/aterm_db-test"))
        );
    }

    #[test]
    fn bench_output_environment_keeps_os_paths() {
        let keeps = |path: &Path| {
            let mut command = crate::exec::check_command("not-executed");
            set_bench_environment(&mut command, "KNOBS", "samples=3", ("OUTPUT", path));
            let vars: Vec<_> = command.get_envs().collect();
            assert!(vars.contains(&(
                std::ffi::OsStr::new("KNOBS"),
                Some(std::ffi::OsStr::new("samples=3"))
            )));
            assert!(vars.contains(&(std::ffi::OsStr::new("OUTPUT"), Some(path.as_os_str()))));
        };
        keeps(Path::new("/scratch/with spaces"));
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt as _;
            keeps(&PathBuf::from(std::ffi::OsString::from_vec(
                b"/scratch/non-utf8-\xff".to_vec(),
            )));
        }
    }

    #[test]
    fn conflicting_artifacts_do_not_select_a_measurement_executable() {
        let artifact = |name: &str, kind: &str, path: &str| {
            format!(
                r#"{{"reason":"compiler-artifact","target":{{"kind":["{kind}"],"name":"{name}"}},"executable":"{path}"}}"#
            )
        };
        let first = artifact("x", "bin", "/first");
        let second = artifact("x", "bin", "/second");
        for positive in [
            first.clone(),
            format!("{first}\n{first}"),
            format!(
                "{}\n{first}\n{}",
                artifact("other", "bin", "/other"),
                artifact("x", "example", "/example")
            ),
        ] {
            assert_eq!(
                built_executable(&positive, "x", "bin").unwrap(),
                PathBuf::from("/first")
            );
        }
        for conflicting in [format!("{first}\n{second}"), format!("{second}\n{first}")] {
            assert!(built_executable(&conflicting, "x", "bin").is_err());
        }
    }

    #[test]
    fn mixed_type_target_kinds_do_not_select_measurement_executables() {
        let valid = r#"{"reason":"compiler-artifact","target":{"kind":["lib"],"name":"x"},"profile":{"test":true},"executable":"/built/x"}"#;
        for kinds in [r#"["lib"]"#, r#"["rlib","lib"]"#] {
            let positive = valid.replace(r#"["lib"]"#, kinds);
            assert_eq!(
                built_executable(&positive, "x", "lib").unwrap(),
                PathBuf::from("/built/x")
            );
        }
        for kinds in [r#"["lib",7]"#, r#"[null,"lib"]"#, r#"["lib",{}]"#] {
            let malformed = valid.replace(r#"["lib"]"#, kinds);
            for wire in [
                malformed.clone(),
                format!("{valid}\n{malformed}"),
                format!("{malformed}\n{valid}"),
            ] {
                assert!(built_executable(&wire, "x", "lib").is_err(), "{kinds}");
            }
        }
    }

    #[test]
    fn ambiguous_compiler_artifacts_do_not_select_measurement_executables() {
        let valid = r#"{"reason":"compiler-artifact","target":{"kind":["lib"],"name":"x"},"profile":{"test":true},"executable":"/built/x"}"#;
        assert_eq!(
            built_executable(valid, "x", "lib").unwrap(),
            PathBuf::from("/built/x")
        );
        for message in include_str!("../testdata/ambiguous-compiler-artifacts.jsonl").lines() {
            for wire in [format!("{valid}\n{message}"), format!("{message}\n{valid}")] {
                assert!(built_executable(&wire, "x", "lib").is_err(), "{message}");
            }
        }
    }

    #[test]
    fn another_build_detects_changed_bytes_with_identical_file_metadata() {
        let world = aterm_tempfile::tempdir().unwrap();
        let path = world.path().join("data-only-not-executed");
        std::fs::write(&path, b"original").unwrap();
        assert!(bench_profile::identity(&world.path().join("missing")).is_err());
        assert!(bench_profile::identity(world.path()).is_err());
        let before = std::fs::metadata(&path).unwrap();
        let identity = bench_profile::identity(&path).unwrap();
        assert_eq!(bench_profile::identity(&path).unwrap(), identity);
        std::fs::write(&path, b"replaced").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(before.modified().unwrap()))
            .unwrap();
        let after = std::fs::metadata(&path).unwrap();
        assert_eq!(before.len(), after.len());
        assert_eq!(before.modified().unwrap(), after.modified().unwrap());
        let changed = bench_profile::identity(&path).unwrap();
        let report = aterm_json::json!({
            "aterm_binary": path.to_str().unwrap(),
            "aterm_binary_identity": { "start": changed, "end": changed },
        });
        assert!(another_build(&report, &path, &identity).is_some());
    }

    #[test]
    fn a_report_of_another_path_or_another_file_there_is_another_build() {
        let built = Path::new("/w/target/opt-test/aterm");
        let id = "87127144 bytes, modified 1792221563.000000001";
        let report = |path: &str, start: &str, end: &str| {
            aterm_json::json!({
                "aterm_binary": path,
                "aterm_binary_identity": { "start": start, "end": end },
            })
        };
        let path = built.to_str().expect("utf-8");
        assert_eq!(another_build(&report(path, id, id), built, id), None);
        let why =
            another_build(&report("/w/target/debug/aterm", id, id), built, id).expect("refused");
        assert!(why.contains("/w/target/debug/aterm"), "{why}");
        let other = "84000000 bytes, modified 1792221999.000000001";
        for (start, end, moment) in [(other, id, "start"), (id, other, "end")] {
            let why = another_build(&report(path, start, end), built, id).expect("refused");
            assert!(why.contains(moment) && why.contains(other), "{why}");
        }
        let bare = aterm_json::json!({ "aterm_binary": path });
        assert!(another_build(&bare, built, id).is_some());
    }

    #[test]
    fn a_second_run_on_one_day_gets_its_own_entry() {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let (md, json) = ledger_paths(dir.path(), "2026-10-16", "fabric");
        assert!(md.ends_with("2026-10-16-fabric.md"), "{}", md.display());
        assert!(json.ends_with("2026-10-16-fabric.json"));
        std::fs::write(&md, "x").expect("write");
        let (md2, json2) = ledger_paths(dir.path(), "2026-10-16", "fabric");
        assert!(md2.ends_with("2026-10-16-fabric-2.md"), "{}", md2.display());
        assert!(json2.ends_with("2026-10-16-fabric-2.json"));
    }

    #[test]
    fn the_entry_head_carries_every_provenance_field() {
        let p = Provenance {
            heading: "Fabric: post -> deliver, burst",
            protocol: "docs/measured/fabric/README.md",
            date: "2026-10-16",
            machine: "Apple M5 Max",
            macos: "27.0",
            aterm: "abc123",
            astream: "6aab5d26a405",
            spec: "samples=200",
            profile: "dev",
            elapsed_s: 81.0,
            command: "targo --unverified verify perf fabric",
            build: "`dev` — 96 units on the measured path: opt-level 0 ×87",
            thermal: "No thermal warning level has been recorded",
            cpus: "18 logical, 6 performance cores",
            load: "5.62 4.40 4.08 before the build, 6.00 5.00 4.00 after the run",
        };
        let head = p.render();
        for (field, value) in [
            ("heading", p.heading),
            ("protocol", p.protocol),
            ("date", p.date),
            ("machine", p.machine),
            ("macos", p.macos),
            ("aterm", p.aterm),
            ("astream", p.astream),
            ("spec", p.spec),
            ("profile", p.profile),
            ("elapsed", "81.0"),
            ("command", p.command),
            ("build", p.build),
            ("thermal", p.thermal),
            ("cpus", p.cpus),
            ("load", p.load),
        ] {
            assert!(head.contains(value), "the head lost the {field}:\n{head}");
        }
    }
}
