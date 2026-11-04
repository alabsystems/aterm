// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The MEASURED lanes' build support, shared by `fabric`, `restore-e2e` and `commons-quiet`: the arm's cargo
//! profile read back from cargo's own plan and config before anything runs ([`prepare`]), the
//! aterm the bench launches pinned by path and identity ([`Built`], [`another_build`]), the bench
//! run with its words in one environment variable ([`run_bench`]), and the provenance an entry
//! records (the load average, the CPUs, the next free entry name).

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

/// The paths a build stamp is of, as aterm's build script asks git of them.
#[path = "../../../aterm/stamp_paths.rs"]
mod stamp_paths;

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
    let pins: Vec<String> = targo
        .git_facts
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    build.push_str(&format!(
        "; the build's git facts pinned for every run: {}",
        pins.join(" ")
    ));
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

/// Build `build_args`'s bench without executing it, obtain fresh measurement-entry evidence,
/// then execute Cargo's exact artifact with `words` in `env`. Echo stderr lines beginning
/// with `progress`, and return stdout.
///
/// # Errors
/// A bench that does not start, or exits unsuccessfully (`what` names it, with both tails).
pub(crate) fn run_bench(
    targo: &Targo<'_>,
    build_args: &[&str],
    env: &str,
    words: &str,
    progress: &str,
    what: &str,
) -> Result<String, Halt> {
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
        "  $ {env}=\"{words}\" {}",
        format_args!("{} --bench", executable.display())
    );
    let mut command = aterm_uds::spawnfd::command(&executable);
    targo.environment(&mut command);
    command.current_dir(workspace_root()).arg("--bench");
    let _entry = super::measurement_entry().map_err(Halt::Timing)?;
    let mut child = command
        .env(env, words)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not execute {}: {e}", executable.display()))?;
    let stdout = child.stdout.take().map(|out| {
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = std::io::Read::read_to_string(&mut std::io::BufReader::new(out), &mut text);
            text
        })
    });
    let mut tail: Vec<String> = Vec::new();
    if let Some(err) = child.stderr.take() {
        for line in std::io::BufReader::new(err).lines().map_while(Result::ok) {
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
    _entry
        .timing_admission()
        .map_err(|why| Halt::Timing(why.to_string()))?;
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
/// sources removed and the build's git facts pinned for every command it runs.
pub(crate) struct Targo<'a> {
    driver: &'a CargoDriver,
    build_dir: Option<String>,
    git_facts: Vec<(&'static str, String)>,
}

impl<'a> Targo<'a> {
    pub(crate) fn new(driver: &'a CargoDriver) -> Self {
        Targo {
            driver,
            build_dir: crate::perf::driver::workspace_build_dir().map(str::to_string),
            git_facts: git_facts(
                |key| std::env::var(key).ok(),
                || probe_git_facts(&workspace_root()),
            ),
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
        let mut cmd = aterm_uds::spawnfd::command(&self.driver.program);
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
        for (key, value) in &self.git_facts {
            cmd.env(key, value);
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

const GIT_FACTS: [&str; 3] = [
    "ATERM_BUILD_GIT_COMMIT",
    "ATERM_BUILD_GIT_COMMIT_FULL",
    "ATERM_BUILD_DEV_COMMITS",
];

fn git_facts(
    caller: impl Fn(&str) -> Option<String>,
    probe: impl FnOnce() -> [String; 3],
) -> Vec<(&'static str, String)> {
    let pinned: Vec<String> = GIT_FACTS
        .iter()
        .filter_map(|key| caller(key).filter(|v| !v.trim().is_empty()))
        .collect();
    let values = match <[String; 3]>::try_from(pinned) {
        Ok(all) => all,
        Err(_) => probe(),
    };
    GIT_FACTS.into_iter().zip(values).collect()
}

/// The three facts as aterm's build script reads them of the work tree `dir` is in. The dirty
/// test asks of the paths the stamp is of ([`stamp_paths::PATHSPEC`]), so the entries a lane
/// writes into the measured ledger between the runs of one set leave the stamp, and so the
/// next run's build, as they were.
fn probe_git_facts(dir: &Path) -> [String; 3] {
    let git = |args: &[&str]| {
        arena::stdout_of(
            aterm_uds::spawnfd::command(aterm_uds::os_path!("git"))
                .args(args)
                .current_dir(dir),
        )
        .filter(|s| !s.is_empty())
    };
    let short = git(&["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = git(&[&["status", "--porcelain"][..], &stamp_paths::PATHSPEC].concat()).is_some();
    let commit = if short != "unknown" && dirty {
        format!("{short}-dirty")
    } else {
        short
    };
    let full = git(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dev_commits = git(&["describe", "--tags", "--match", "v*.*.0", "--abbrev=0"])
        .and_then(|tag| git(&["rev-list", &format!("{}..HEAD", tag.trim()), "--count"]))
        .unwrap_or_else(|| "0".into());
    [commit, full, dev_commits]
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

/// Cargo's exact executable artifact, matched by both target name and target kind.
pub(crate) fn built_executable(messages: &str, name: &str, kind: &str) -> Result<PathBuf, String> {
    let mut selected: Option<PathBuf> = None;
    for line in messages.lines() {
        let Ok(wire) = aterm_json::from_str::<crate::compiler_message::UnitMessage>(line) else {
            continue;
        };
        let message = match wire {
            crate::compiler_message::UnitMessage::Unit(value) => value,
            crate::compiler_message::UnitMessage::Other => continue,
            crate::compiler_message::UnitMessage::Ambiguous(_) => {
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
        aterm_uds::spawnfd::command(aterm_uds::os_path!("sysctl")).args(["-n", "vm.loadavg"]),
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
        aterm_uds::spawnfd::command(aterm_uds::os_path!("sysctl"))
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
    fn the_build_git_facts_are_the_callers_three_or_one_probe() {
        let probe = || {
            [
                "0123456789ab-dirty".to_string(),
                "0123456789abcdef".to_string(),
                "7".to_string(),
            ]
        };
        let pinned = git_facts(
            |key| Some(format!("pinned {key}")),
            || panic!("probed beside the caller's three pins"),
        );
        assert_eq!(
            pinned.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
            GIT_FACTS
        );
        assert!(
            pinned
                .iter()
                .all(|(key, value)| *value == format!("pinned {key}")),
            "{pinned:?}"
        );
        let partials: [fn(&str) -> Option<String>; 3] = [
            |key: &str| (key != "ATERM_BUILD_DEV_COMMITS").then(|| "x".to_string()),
            |key: &str| {
                Some(if key == "ATERM_BUILD_GIT_COMMIT" {
                    " ".to_string()
                } else {
                    "x".to_string()
                })
            },
            |_: &str| None,
        ];
        for partial in partials {
            let facts = git_facts(partial, probe);
            assert_eq!(
                facts
                    .into_iter()
                    .map(|(_, value)| value)
                    .collect::<Vec<_>>(),
                probe(),
                "short of three pins, all three are probed"
            );
        }
    }

    /// D10b's set (2026-11-04-fabric-1001 to -1004): run 1's entry, written into the ledger and
    /// not yet committed, stamped run 2's build `-dirty`, so run 2 ran a rebuild. The lane's
    /// own output, a new entry and an edit to the ledger's README, leaves the three facts as
    /// they were, asked from the top or from a package's folder (where a build script runs);
    /// the negative controls, an edited source file, a new one and a file beside the ledger
    /// that only shares its name's start, each mark the stamp dirty.
    #[test]
    fn a_ledger_entry_leaves_the_stamp_and_a_source_change_dirties_it() {
        let scratch = aterm_tempfile::tempdir().expect("tempdir");
        let top = scratch.path();
        let package = top.join("crates/a");
        let git = |args: &[&str]| {
            let out = aterm_uds::spawnfd::command(aterm_uds::os_path!("git"))
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .current_dir(top)
                .output()
                .expect("git runs");
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        let write = |path: &str, text: &str| {
            let path = top.join(path);
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("dir");
            std::fs::write(path, text).expect("write");
        };
        write("crates/a/src/lib.rs", "pub fn a() {}\n");
        write("docs/measured/fabric/README.md", "# fabric\n");
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        git(&["commit", "-qm", "base"]);
        let clean = probe_git_facts(&package);
        assert!(
            clean[0].len() == 12 && clean[0].bytes().all(|b| b.is_ascii_hexdigit()),
            "a committed tree's stamp is its commit alone: {clean:?}"
        );

        write("docs/measured/fabric/2026-11-04-fabric-1001.json", "{}\n");
        write("docs/measured/fabric/2026-11-04-fabric-1001.md", "# 1001\n");
        write(
            "docs/measured/fabric/README.md",
            "# fabric\n\nD10b's set.\n",
        );
        assert_eq!(
            probe_git_facts(&package),
            clean,
            "the ledger's entries changed the stamp"
        );
        assert_eq!(probe_git_facts(top), clean, "asked from the top");

        let dirty = [
            format!("{}-dirty", clean[0]),
            clean[1].clone(),
            clean[2].clone(),
        ];
        let controls: [(&str, &str, bool); 3] = [
            ("crates/a/src/lib.rs", "pub fn a() -> u8 { 1 }\n", true),
            ("crates/a/src/new.rs", "pub fn b() {}\n", false),
            ("docs/measured.md", "beside the ledger\n", false),
        ];
        for (path, text, tracked) in controls {
            write(path, text);
            assert_eq!(probe_git_facts(&package), dirty, "{path} changed");
            if tracked {
                git(&["checkout", "-q", "--", path]);
            } else {
                std::fs::remove_file(top.join(path)).expect("remove");
            }
            assert_eq!(probe_git_facts(&package), clean, "{path} restored");
        }
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
