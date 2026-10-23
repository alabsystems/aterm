// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Service units that run a lane, rendered from typed fields.

use std::path::{Path, PathBuf};

use super::LaneKind;

/// The Apple lane's launchd label.
pub const LAUNCHD_LABEL: &str = "systems.alab.aterm-auto-alab";

/// The retired claude/codex vendor lane's label.
pub const RETIRED_LABEL: &str = "systems.alab.aterm-auto-vendor";

/// The Linux lane's service unit.
pub const SYSTEMD_SERVICE: &str = "aterm-auto-atpkg.service";

/// The Linux lane's timer.
pub const SYSTEMD_TIMER: &str = "aterm-auto-atpkg.timer";

/// The Apple lane's 6-hour start interval in seconds.
pub const START_INTERVAL_SECS: u64 = 21_600;

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
}

impl UnitSpec {
    /// The command the unit runs.
    #[must_use]
    pub fn argv(&self) -> Vec<String> {
        vec![
            self.binary.display().to_string(),
            "lane".into(),
            "run".into(),
            self.kind.as_str().into(),
            "--sha256".into(),
            self.sha256.clone(),
            "--log".into(),
            super::state::LaneState::of(self.kind, &self.state_dir)
                .log
                .display()
                .to_string(),
        ]
    }

    /// `XDG_STATE_HOME`, so the run resolves the state root the installer used.
    #[must_use]
    pub fn xdg_state_home(&self) -> PathBuf {
        self.state_dir
            .parent()
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
        LaneKind::Linux => vec![systemd_service(spec), systemd_timer()],
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

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

impl Plist {
    fn write(&self, depth: usize, out: &mut String) {
        let pad = "\t".repeat(depth);
        match self {
            Self::Str(s) => out.push_str(&format!("{pad}<string>{}</string>\n", xml(s))),
            Self::Int(n) => out.push_str(&format!("{pad}<integer>{n}</integer>\n")),
            Self::Bool(b) => out.push_str(&format!("{pad}<{b}/>\n")),
            Self::Array(items) => {
                out.push_str(&format!("{pad}<array>\n"));
                for i in items {
                    i.write(depth + 1, out);
                }
                out.push_str(&format!("{pad}</array>\n"));
            }
            Self::Dict(entries) => {
                out.push_str(&format!("{pad}<dict>\n"));
                for (k, v) in entries {
                    out.push_str(&format!("{pad}\t<key>{}</key>\n", xml(k)));
                    v.write(depth + 1, out);
                }
                out.push_str(&format!("{pad}</dict>\n"));
            }
        }
    }
}

/// Render the Apple lane's launchd user agent.
#[must_use]
pub fn launchd(spec: &UnitSpec) -> Unit {
    let s = |v: &str| Plist::Str(v.to_string());
    let launchd_log = spec
        .state_dir
        .join("alab.launchd.log")
        .display()
        .to_string();
    let dict = Plist::Dict(vec![
        ("Label".into(), s(LAUNCHD_LABEL)),
        (
            "ProgramArguments".into(),
            Plist::Array(spec.argv().iter().map(|a| s(a)).collect()),
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
        ("StartInterval".into(), Plist::Int(START_INTERVAL_SECS)),
        ("RunAtLoad".into(), Plist::Bool(true)),
        ("Nice".into(), Plist::Int(10)),
        ("ProcessType".into(), s("Background")),
        ("StandardOutPath".into(), s(&launchd_log)),
        ("StandardErrorPath".into(), s(&launchd_log)),
    ]);
    let mut text = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <!-- Rendered by `atpkg-publish lane install alab`: re-run it to change this agent. -->\n\
         <plist version=\"1.0\">\n",
    );
    dict.write(0, &mut text);
    text.push_str("</plist>\n");
    Unit {
        name: format!("{LAUNCHD_LABEL}.plist"),
        text,
    }
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
