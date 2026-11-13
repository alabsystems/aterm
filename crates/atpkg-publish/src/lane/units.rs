// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Service units that run a lane, rendered from typed fields.

use std::path::{Path, PathBuf};

use super::LaneKind;
use crate::ctx::HostChoice;

/// The Apple lane's launchd label.
pub const LAUNCHD_LABEL: &str = "systems.alab.aterm-auto-alab";

/// The retired claude/codex vendor lane's label.
pub const RETIRED_LABEL: &str = "systems.alab.aterm-auto-vendor";

/// The Linux lane's service unit.
pub const SYSTEMD_SERVICE: &str = "aterm-auto-atpkg.service";

/// The Linux lane's timer.
pub const SYSTEMD_TIMER: &str = "aterm-auto-atpkg.timer";

/// The explicit staged-only lane's filesystem event source.
pub const SYSTEMD_PATH: &str = "aterm-auto-atpkg.path";

/// The Apple lane's 6-hour start interval in seconds.
pub const START_INTERVAL_SECS: u64 = 21_600;

/// The tip lane's launchd label.
pub const TIP_LABEL: &str = "systems.alab.aterm-auto-tip";

/// The Apple ready dispatcher's launchd label.
pub const TIP_READY_LABEL: &str = "systems.alab.aterm-auto-tip-ready";

/// Every launchd job a lane install arms. One lane snapshot is live per machine, so arming one
/// lane's jobs retires the others'.
pub const APPLE_LABELS: &[&str] = &[LAUNCHD_LABEL, TIP_LABEL, TIP_READY_LABEL];

/// The tip lane's start interval in seconds: a pass freezes whatever main is then, so a short
/// interval bounds how stale a qualified build can be. launchd never starts a second pass while
/// one runs.
pub const TIP_INTERVAL_SECS: u64 = 900;

/// The ready dispatcher's fallback start interval in seconds: its event is a change of the
/// ready directory, and this retries a record a pass could not publish.
pub const TIP_READY_INTERVAL_SECS: u64 = 3600;

/// launchd's own capture of the job `label`, under the lanes' state root.
#[must_use]
pub fn launchd_log(state_dir: &Path, label: &str) -> PathBuf {
    state_dir.join(match label {
        TIP_LABEL => "tip.launchd.log",
        TIP_READY_LABEL => "tip-ready.launchd.log",
        _ => "alab.launchd.log",
    })
}

/// What a unit is rendered from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnitSpec {
    /// Which lane.
    pub kind: LaneKind,
    /// The snapshot binary.
    pub binary: PathBuf,
    /// Its sha256.
    pub sha256: String,
    /// `$HOME`.
    pub home: PathBuf,
    /// The lanes' state root (`Ctx::state_dir`).
    pub state_dir: PathBuf,
    /// `XDG_CONFIG_HOME` as the installer resolved it.
    pub xdg_config_home: PathBuf,
    /// The atpkg shim dir (`<prefix>/bin`), first on PATH.
    pub shim_bin: PathBuf,
    /// Which hosts the unit's runs build (`--host`, §16.1): `gh` until WP15.
    pub host: HostChoice,
    /// Consume staging notifications instead of building missing programs (Linux); also arm
    /// the ready dispatcher beside the tip lane (tip).
    pub staged_only: bool,
}

impl UnitSpec {
    /// The command the unit runs: the snapshot, then `--host {h}` when the choice is not `gh`
    /// (a global flag, so it comes before the verb), then `lane run`.
    #[must_use]
    pub fn argv(&self) -> Vec<String> {
        let host = (self.host != HostChoice::Gh)
            .then(|| ["--host".to_string(), self.host.as_str().to_string()])
            .into_iter()
            .flatten();
        std::iter::once(self.binary.display().to_string())
            .chain(host)
            .chain(self.run_words(self.staged_only && self.kind != LaneKind::Tip))
            .collect()
    }

    /// The command the tip lane's ready dispatcher runs: [`Self::argv`]'s, as the queued
    /// staged-only pass, with its own log.
    #[must_use]
    pub fn ready_argv(&self) -> Vec<String> {
        let host = (self.host != HostChoice::Gh)
            .then(|| ["--host".to_string(), self.host.as_str().to_string()])
            .into_iter()
            .flatten();
        std::iter::once(self.binary.display().to_string())
            .chain(host)
            .chain(self.run_words(true))
            .collect()
    }

    /// `lane run <kind> --sha256 … --log …`, and for a `queued` pass the staged-only words.
    fn run_words(&self, queued: bool) -> Vec<String> {
        let log = if queued && self.kind == LaneKind::Tip {
            self.state_dir.join("tip-ready.log")
        } else {
            super::state::LaneState::of(self.kind, &self.state_dir).log
        };
        let mut words = vec![
            "lane".into(),
            "run".into(),
            self.kind.as_str().into(),
            "--sha256".into(),
            self.sha256.clone(),
            "--log".into(),
            log.display().to_string(),
        ];
        if queued {
            words.extend(["--staged-only".into(), "--queued".into(), "--json".into()]);
        }
        words
    }

    /// `XDG_STATE_HOME`, so the run resolves the state root the installer used.
    #[must_use]
    pub fn xdg_state_home(&self) -> PathBuf {
        self.state_dir
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map_or_else(|| self.state_dir.clone(), Path::to_path_buf)
    }

    fn path(&self, tail: &[&str]) -> String {
        std::iter::once(self.shim_bin.display().to_string())
            .chain(tail.iter().map(|s| (*s).to_string()))
            .chain(std::iter::once(
                self.home.join(".local/bin").display().to_string(),
            ))
            .collect::<Vec<_>>()
            .join(":")
    }
}

/// One rendered unit file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unit {
    /// Its file name.
    pub name: String,
    /// Its text.
    pub text: String,
}

/// Every unit a lane installs.
#[must_use]
pub fn render(spec: &UnitSpec) -> Vec<Unit> {
    match spec.kind {
        LaneKind::Alab => vec![launchd(spec)],
        LaneKind::Tip => {
            let mut units = vec![launchd_tip(spec)];
            if spec.staged_only {
                units.push(launchd_tip_ready(spec));
            }
            units
        }
        LaneKind::Linux => {
            let mut units = vec![systemd_service(spec), systemd_timer()];
            if spec.staged_only {
                units.push(systemd_ready_path(spec));
            }
            units
        }
    }
}

/// A property-list value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plist {
    /// `<string>`.
    Str(String),
    /// `<integer>`.
    Int(u64),
    /// `<true/>` / `<false/>`.
    Bool(bool),
    /// `<array>`.
    Array(Vec<Plist>),
    /// `<dict>`, in order.
    Dict(Vec<(String, Plist)>),
}

fn xml(s: &str) -> std::borrow::Cow<'_, str> {
    if !s.contains(['&', '<', '>']) {
        return std::borrow::Cow::Borrowed(s);
    }
    std::borrow::Cow::Owned(
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;"),
    )
}

impl Plist {
    fn write(&self, depth: usize, out: &mut String) {
        use std::fmt::Write as _;
        out.extend(std::iter::repeat_n('\t', depth));
        match self {
            Self::Str(s) => {
                let _ = writeln!(out, "<string>{}</string>", xml(s));
            }
            Self::Int(n) => {
                let _ = writeln!(out, "<integer>{n}</integer>");
            }
            Self::Bool(b) => {
                let _ = writeln!(out, "<{b}/>");
            }
            Self::Array(items) => {
                out.push_str("<array>\n");
                for i in items {
                    i.write(depth + 1, out);
                }
                out.extend(std::iter::repeat_n('\t', depth));
                out.push_str("</array>\n");
            }
            Self::Dict(entries) => {
                out.push_str("<dict>\n");
                for (k, v) in entries {
                    out.extend(std::iter::repeat_n('\t', depth + 1));
                    let _ = writeln!(out, "<key>{}</key>", xml(k));
                    v.write(depth + 1, out);
                }
                out.extend(std::iter::repeat_n('\t', depth));
                out.push_str("</dict>\n");
            }
        }
    }
}

/// One launchd user agent of a lane snapshot.
struct Agent<'a> {
    label: &'a str,
    argv: Vec<String>,
    /// The verb that renders it, for the plist's own comment.
    rendered_by: &'a str,
    interval: u64,
    /// A directory whose change starts the job.
    watch: Option<PathBuf>,
    /// Run as a launchd `Background` process, which macOS throttles (CPU and I/O). Not for a
    /// job whose steps are judged against time bounds another repository committed.
    background: bool,
}

fn agent(spec: &UnitSpec, agent: &Agent<'_>) -> Unit {
    let s = |v: &str| Plist::Str(v.to_string());
    let launchd_log = launchd_log(&spec.state_dir, agent.label)
        .display()
        .to_string();
    let mut entries = vec![
        ("Label".into(), s(agent.label)),
        (
            "ProgramArguments".into(),
            Plist::Array(agent.argv.iter().map(|a| s(a)).collect()),
        ),
        (
            "WorkingDirectory".into(),
            s(&spec.home.display().to_string()),
        ),
        (
            "EnvironmentVariables".into(),
            Plist::Dict(vec![
                (
                    "PATH".into(),
                    s(&spec.path(&[
                        "/usr/bin",
                        "/bin",
                        "/usr/sbin",
                        "/sbin",
                        "/opt/homebrew/bin",
                        "/usr/local/bin",
                    ])),
                ),
                ("HOME".into(), s(&spec.home.display().to_string())),
                (
                    "XDG_STATE_HOME".into(),
                    s(&spec.xdg_state_home().display().to_string()),
                ),
                (
                    "XDG_CONFIG_HOME".into(),
                    s(&spec.xdg_config_home.display().to_string()),
                ),
            ]),
        ),
        ("StartInterval".into(), Plist::Int(agent.interval)),
    ];
    if let Some(watch) = &agent.watch {
        entries.push((
            "WatchPaths".into(),
            Plist::Array(vec![s(&watch.display().to_string())]),
        ));
    }
    entries.extend([
        ("RunAtLoad".into(), Plist::Bool(true)),
        ("Nice".into(), Plist::Int(10)),
    ]);
    if agent.background {
        entries.push(("ProcessType".into(), s("Background")));
    }
    entries.extend([
        ("StandardOutPath".into(), s(&launchd_log)),
        ("StandardErrorPath".into(), s(&launchd_log)),
    ]);
    let mut text = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <!-- Rendered by `atpkg-publish {}`: re-run it to change this agent. -->\n\
         <plist version=\"1.0\">\n",
        agent.rendered_by
    );
    Plist::Dict(entries).write(0, &mut text);
    text.push_str("</plist>\n");
    Unit {
        name: format!("{}.plist", agent.label),
        text,
    }
}

/// Render the Apple lane's launchd user agent.
#[must_use]
pub fn launchd(spec: &UnitSpec) -> Unit {
    agent(
        spec,
        &Agent {
            label: LAUNCHD_LABEL,
            argv: spec.argv(),
            rendered_by: "lane install alab",
            interval: START_INTERVAL_SECS,
            watch: None,
            background: true,
        },
    )
}

/// Render the tip lane's launchd user agent: a pass every [`TIP_INTERVAL_SECS`]. It is not a
/// `Background` process: a pass's steps are held to the time bounds each product repository
/// committed, and macOS's throttling of that class would turn a sound build into an overrun.
#[must_use]
pub fn launchd_tip(spec: &UnitSpec) -> Unit {
    agent(
        spec,
        &Agent {
            label: TIP_LABEL,
            argv: spec.argv(),
            rendered_by: "lane install tip",
            interval: TIP_INTERVAL_SECS,
            watch: None,
            background: false,
        },
    )
}

/// Render the Apple ready dispatcher's launchd user agent: started when the ready directory
/// changes (a record enqueued or retired), and every [`TIP_READY_INTERVAL_SECS`] besides. Each
/// start is the queued staged-only pass, which is idle on an empty queue and still checks its
/// installed hash, the fresh signed head and every record.
#[must_use]
pub fn launchd_tip_ready(spec: &UnitSpec) -> Unit {
    agent(
        spec,
        &Agent {
            label: TIP_READY_LABEL,
            argv: spec.ready_argv(),
            rendered_by: "lane install tip --staged-only",
            interval: TIP_READY_INTERVAL_SECS,
            watch: Some(super::state::LaneState::of(LaneKind::Tip, &spec.state_dir).ready_dir()),
            background: true,
        },
    )
}

fn exec_word(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('$', "$$")
    )
}

fn env_word(k: &str, v: &str) -> String {
    format!(
        "\"{k}={}\"",
        v.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
    )
}

/// Render the Linux lane's systemd service unit.
#[must_use]
pub fn systemd_service(spec: &UnitSpec) -> Unit {
    let exec = spec
        .argv()
        .iter()
        .map(|w| exec_word(w))
        .collect::<Vec<_>>()
        .join(" ");
    let text = format!(
        "# Rendered by `atpkg-publish lane install linux`: re-run it to change this unit.\n\
         [Unit]\n\
         Description=atpkg Linux lane (the atpkg-publish snapshot, lane run linux)\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         ExecStart={exec}\n\
         WorkingDirectory=~\n\
         Environment={} {} {}\n\
         TimeoutStartSec=4h\n\
         Nice=10\n",
        env_word("PATH", &spec.path(&["/usr/local/bin", "/usr/bin", "/bin"])),
        env_word(
            "XDG_STATE_HOME",
            &spec.xdg_state_home().display().to_string()
        ),
        env_word(
            "XDG_CONFIG_HOME",
            &spec.xdg_config_home.display().to_string()
        ),
    );
    Unit {
        name: SYSTEMD_SERVICE.into(),
        text,
    }
}

/// The Linux lane's hourly timer.
#[must_use]
pub fn systemd_timer() -> Unit {
    Unit {
        name: SYSTEMD_TIMER.into(),
        text: "# Rendered by `atpkg-publish lane install linux`: re-run it to change this unit.\n\
               [Unit]\n\
               Description=hourly trigger for the atpkg Linux lane\n\
               \n\
               [Timer]\n\
               OnCalendar=hourly\n\
               RandomizedDelaySec=15min\n\
               Persistent=true\n\
               \n\
               [Install]\n\
               WantedBy=timers.target\n"
            .into(),
    }
}

/// Wake the existing service when canonical staging changes its notification
/// directory. The service still checks its installed hash and the fresh cohort.
#[must_use]
pub fn systemd_ready_path(spec: &UnitSpec) -> Unit {
    let directory = super::state::LaneState::of(LaneKind::Linux, &spec.state_dir).ready_dir();
    // PathChanged takes the entire literal RHS, not an ExecStart-style quoted word.
    let path = directory.display().to_string().replace('%', "%%");
    Unit {
        name: SYSTEMD_PATH.into(),
        text: format!(
            "# Rendered by `atpkg-publish lane install linux --staged-only`.\n\
             [Unit]\nDescription=publish qualified staged atpkg cohorts\n\n\
             [Path]\nPathChanged={path}\nUnit={SYSTEMD_SERVICE}\n\n\
             [Install]\nWantedBy=default.target\n"
        ),
    }
}
