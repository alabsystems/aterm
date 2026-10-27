// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The MEASURED lanes' bench build, shared by `fabric` and `restore-e2e`: the arm's cargo
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

/// THE BENCHES' OWN TABLE of arms, mounted from the fabric bench (std-only): the cargo profile
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
    let audit = audit(&targo, build_args)?;
    if let Some(why) = audit.refusal(profile.every_crate_at()) {
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

/// Run `build_args`'s bench (`targo bench -q …`) with `words` in `env`, echoing its stderr lines
/// that start with `progress`: its stdout.
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
    let mut bench_args = vec!["-q"];
    bench_args.extend_from_slice(build_args);
    eprintln!(
        "  $ {env}=\"{words}\" {}",
        targo.display("bench", &bench_args)
    );
    let mut child = targo
        .command("bench")
        .args(bench_args)
        .env(env, words)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run targo: {e}"))?;
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
        .map_err(|e| format!("could not wait for targo: {e}"))?;
    let stdout = stdout.and_then(|h| h.join().ok()).unwrap_or_default();
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
    fn new(driver: &'a CargoDriver) -> Self {
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

    fn command(&self, verb: &str) -> Command {
        let mut cmd = aterm_uds::spawnfd::command(&self.driver.program);
        cmd.args(self.lead(verb)).current_dir(workspace_root());
        for (key, _) in std::env::vars_os() {
            if key.to_str().is_some_and(flags::is_env_source) {
                cmd.env_remove(key);
            }
        }
        for (key, value) in &self.git_facts {
            cmd.env(key, value);
        }
        cmd
    }

    fn display(&self, verb: &str, args: &[&str]) -> String {
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
    let dirty = git(&["status", "--porcelain"]).is_some();
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

fn audit(targo: &Targo<'_>, build_args: &[&str]) -> Result<units::Audit, Halt> {
    let mut plan_args = build_args.to_vec();
    plan_args.extend(["--unit-graph", "-Z", "unstable-options"]);
    let graph = targo.output("bench", &plan_args, "the build plan")?;
    units::Audit::of(&graph)
        .map_err(|why| Halt::Fail(format!("the build plan does not read: {why}")))
}

fn config_rustflags(targo: &Targo<'_>) -> Result<Vec<flags::Source>, Halt> {
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
    for line in messages.lines() {
        let Ok(message) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        if message.get("reason").and_then(aterm_json::Value::as_str) != Some("compiler-artifact") {
            continue;
        }
        let target = message.get("target");
        let named = target
            .and_then(|t| t.get("name"))
            .and_then(aterm_json::Value::as_str)
            == Some(name);
        let bin = target
            .and_then(|t| t.get("kind"))
            .and_then(aterm_json::Value::as_array)
            .is_some_and(|kinds| kinds.iter().any(|k| k.as_str() == Some("bin")));
        if named && bin {
            return message
                .get("executable")
                .and_then(aterm_json::Value::as_str)
                .map(PathBuf::from)
                .ok_or_else(|| format!("cargo built the `{name}` binary and named no executable"));
        }
    }
    Err(format!("cargo reported no `{name}` binary"))
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
