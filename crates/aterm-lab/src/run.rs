// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Lab run-record persistence and tag resolution for `runs/<tag>/env.sh`.

use std::path::{Path, PathBuf};

use crate::LAB;
use crate::paths::{paths, valid_tag};
use crate::util::{atomic_write, shell_quote, shell_unquote};

/// Everything `env.sh` says of a lab.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Run {
    /// The lab's tag.
    pub tag: String,
    /// `runs/<tag>/`.
    pub run: PathBuf,
    /// `runs/<tag>/scratch/`.
    pub w: PathBuf,
    /// The private socket directory, `/tmp/at.XXXX`.
    pub sockdir: String,
    /// `<sockdir>/a.sock`.
    pub sock: String,
    /// The project directory the tab starts in, `<sockdir>/proj`.
    pub proj: String,
    /// The stub's API port.
    pub port: String,
    /// The black-hole proxy's port.
    pub pport: String,
    /// What `HTTP(S)_PROXY` names.
    pub proxy: String,
    /// The stub's pid.
    pub stub_pid: i32,
    /// The private instance's pid.
    pub instance_pid: i32,
    /// The egress witness's pid.
    pub sampler_pid: i32,
    /// The Claude Code binary the lab launches by path.
    pub claude_bin: String,
    /// Its version (`2.1.300`).
    pub claude_ver: String,
    /// The grid's columns at `up`.
    pub cols: String,
    /// The grid's rows at `up`.
    pub rows: String,
    /// `1` when the instance runs under the headless-restore seam.
    pub seam: String,
    /// The Codex binary the lab launches by path, when one is managed here.
    pub codex_bin: String,
    /// Its version, from its store package (`0.163.0`).
    pub codex_ver: String,
    /// The Gemini CLI the lab launches by path, when there is one.
    pub gemini_bin: String,
    /// Its version, from its package (`0.46.0`).
    pub gemini_ver: String,
    /// The Antigravity CLI (`agy`) the lab launches by path, when there is one.
    pub agy_bin: String,
    /// Its version, from its store build's record (`1.4.0`).
    pub agy_ver: String,
    /// The Muse Code (`muse`) the lab launches by path, when there is one.
    pub muse_bin: String,
    /// Its version, from its store build's record (`1.4.10-R6427.1`).
    pub muse_ver: String,
    /// The agent the tab last launched.
    pub agent: String,
    /// The login the tab's Codex was launched with: `lab`, `chatgpt` or `signed-out`.
    pub codex_mode: String,
    /// The aterm under test, resolved once at `up`.
    pub aterm: String,
    /// The jail's mark ([`crate::procs::jail_members`]).
    pub mark: String,
    /// `LAB_INSTANCE_ENV` as `up` took it (`K=V K=V`, checked by [`crate::lab::instance_env`]).
    pub instance_env: String,
    /// `1` when `up` loaded the lab's sleep clock into its instance and its Claude Code.
    pub clock: String,
    /// `1` when the lab's instance opens a window (`forever --window`, [`crate::lab::windowed`]).
    pub window: String,
    /// `1` when `up` laid the jail's reroute prefix (`LAB_REROUTE=1`) and the instance reroutes.
    pub reroute: String,
}

impl Run {
    /// A fresh record for `tag` (nothing started yet).
    #[must_use]
    pub fn new(tag: &str) -> Run {
        let run = paths().run_dir(tag);
        Run {
            tag: tag.to_string(),
            w: run.join("scratch"),
            run,
            seam: "0".into(),
            ..Run::default()
        }
    }

    fn pairs(&self) -> Vec<(&'static str, String)> {
        vec![
            ("TAG", self.tag.clone()),
            ("RUN", self.run.to_string_lossy().into_owned()),
            ("W", self.w.to_string_lossy().into_owned()),
            ("SOCKDIR", self.sockdir.clone()),
            ("SOCK", self.sock.clone()),
            ("PROJ", self.proj.clone()),
            ("PORT", self.port.clone()),
            ("PPORT", self.pport.clone()),
            ("PROXY", self.proxy.clone()),
            ("STUB_PID", self.stub_pid.to_string()),
            ("INSTANCE_PID", self.instance_pid.to_string()),
            ("SAMPLER_PID", self.sampler_pid.to_string()),
            ("CLAUDE_BIN", self.claude_bin.clone()),
            ("CLAUDE_VER", self.claude_ver.clone()),
            ("COLS", self.cols.clone()),
            ("ROWS", self.rows.clone()),
            ("SEAM", self.seam.clone()),
            ("CODEX_BIN", self.codex_bin.clone()),
            ("CODEX_VER", self.codex_ver.clone()),
            ("GEMINI_BIN", self.gemini_bin.clone()),
            ("GEMINI_VER", self.gemini_ver.clone()),
            ("AGY_BIN", self.agy_bin.clone()),
            ("AGY_VER", self.agy_ver.clone()),
            ("MUSE_BIN", self.muse_bin.clone()),
            ("MUSE_VER", self.muse_ver.clone()),
            (
                "AGENT",
                if self.agent.is_empty() {
                    "claude".into()
                } else {
                    self.agent.clone()
                },
            ),
            ("CODEX_MODE", self.codex_mode.clone()),
            ("ATERM", self.aterm.clone()),
            ("LAB_MARK", self.mark.clone()),
            ("INSTANCE_ENV", self.instance_env.clone()),
            ("CLOCK", self.clock.clone()),
            ("WINDOW", self.window.clone()),
            ("REROUTE", self.reroute.clone()),
        ]
    }

    /// The text of `env.sh`.
    #[must_use]
    pub fn render(&self) -> String {
        self.pairs()
            .into_iter()
            .map(|(k, v)| format!("{k}={}\n", shell_quote(&v)))
            .collect()
    }

    /// Parse `env.sh` text.
    #[must_use]
    pub fn parse(text: &str) -> Run {
        let mut r = Run::default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let v = shell_unquote(v);
            let pid = |v: &str| v.trim().parse().unwrap_or(0);
            match k {
                "TAG" => r.tag = v,
                "RUN" => r.run = PathBuf::from(v),
                "W" => r.w = PathBuf::from(v),
                "SOCKDIR" => r.sockdir = v,
                "SOCK" => r.sock = v,
                "PROJ" => r.proj = v,
                "PORT" => r.port = v,
                "PPORT" => r.pport = v,
                "PROXY" => r.proxy = v,
                "STUB_PID" => r.stub_pid = pid(&v),
                "INSTANCE_PID" => r.instance_pid = pid(&v),
                "SAMPLER_PID" => r.sampler_pid = pid(&v),
                "CLAUDE_BIN" => r.claude_bin = v,
                "CLAUDE_VER" => r.claude_ver = v,
                "COLS" => r.cols = v,
                "ROWS" => r.rows = v,
                "SEAM" => r.seam = v,
                "CODEX_BIN" => r.codex_bin = v,
                "CODEX_VER" => r.codex_ver = v,
                "GEMINI_BIN" => r.gemini_bin = v,
                "GEMINI_VER" => r.gemini_ver = v,
                "AGY_BIN" => r.agy_bin = v,
                "AGY_VER" => r.agy_ver = v,
                "MUSE_BIN" => r.muse_bin = v,
                "MUSE_VER" => r.muse_ver = v,
                "AGENT" => r.agent = v,
                "CODEX_MODE" => r.codex_mode = v,
                "ATERM" => r.aterm = v,
                "LAB_MARK" => r.mark = v,
                "INSTANCE_ENV" => r.instance_env = v,
                "CLOCK" => r.clock = v,
                "WINDOW" => r.window = v,
                "REROUTE" => r.reroute = v,
                _ => {}
            }
        }
        r
    }

    /// Write `env.sh` now, atomically.
    pub fn save(&self) {
        let _ = atomic_write(&self.run.join("env.sh"), self.render().as_bytes());
    }

    /// The lab `tag` as `env.sh` records it.
    /// # Errors
    /// No lab of that tag is up.
    pub fn load(tag: &str) -> Result<Run, String> {
        let file = paths().run_dir(tag).join("env.sh");
        let text = std::fs::read_to_string(&file).map_err(|_| {
            format!("no lab '{tag}' is up ({LAB} up {tag}; {LAB} ps lists the labs there are)")
        })?;
        Ok(Run::parse(&text))
    }

    /// The aterm this lab runs (its door, a restart), as `up` recorded it.
    /// # Errors
    /// An older lab's `env.sh` names none.
    pub fn need_aterm(&self) -> Result<std::path::PathBuf, String> {
        if self.aterm.is_empty() {
            return Err(format!(
                "lab '{}' records no aterm in its env.sh (an older aterm-lab brought it up): {LAB} down {0}, then up",
                self.tag
            ));
        }
        Ok(std::path::PathBuf::from(&self.aterm))
    }

    /// The agent the tab runs (`claude` unless `codex`, `gemini`, `agy` or `muse` launched one).
    #[must_use]
    pub fn agent(&self) -> &str {
        match self.agent.as_str() {
            "codex" => "codex",
            "gemini" => "gemini",
            "agy" => "agy",
            "muse" => "muse",
            _ => "claude",
        }
    }

    /// The prefix of this lab's fixtures.
    #[must_use]
    pub fn fixture_prefix(&self) -> String {
        let ver = match self.agent() {
            "codex" => &self.codex_ver,
            "gemini" => &self.gemini_ver,
            "agy" => &self.agy_ver,
            "muse" => &self.muse_ver,
            _ => &self.claude_ver,
        };
        format!(
            "{}-{}",
            self.agent(),
            if ver.is_empty() { "?" } else { ver }
        )
    }

    /// `scratch/home/.claude/settings.json`.
    #[must_use]
    pub fn settings(&self) -> PathBuf {
        self.w.join("home/.claude/settings.json")
    }

    /// The instance's state root, `scratch/state`.
    #[must_use]
    pub fn state(&self) -> PathBuf {
        self.w.join("state")
    }
}

/// Whether `tag` names a lab that has a run directory.
#[must_use]
pub fn is_run(tag: &str) -> bool {
    valid_tag(tag) && paths().run_dir(tag).join("env.sh").is_file()
}

/// The tag a verb acts on: its first argument when that names a lab that is up.
/// # Errors
/// `$LAB_TAG` is not a valid tag.
pub fn take_tag(args: &mut Vec<String>) -> Result<String, String> {
    if let Some(first) = args.first()
        && is_run(first)
    {
        return Ok(args.remove(0));
    }
    let tag = default_tag();
    if valid_tag(&tag) {
        Ok(tag)
    } else {
        Err(format!("bad tag '{tag}'"))
    }
}

/// The tag a verb whose ONE positional argument is its tag acts on ([`take_tag`]).
/// # Errors
/// The argument, or `$LAB_TAG`, is not a valid tag.
pub fn take_only_tag(args: &mut Vec<String>) -> Result<String, String> {
    if args.first().is_some_and(|a| !a.starts_with('-')) {
        let tag = args.remove(0);
        return if valid_tag(&tag) {
            Ok(tag)
        } else {
            Err(format!("bad tag '{tag}'"))
        };
    }
    take_tag(args)
}

/// `$LAB_TAG`, else `lab`.
#[must_use]
pub fn default_tag() -> String {
    std::env::var("LAB_TAG")
        .ok()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "lab".into())
}

/// The tags of every lab that has a run directory.
#[must_use]
pub fn all_runs(state: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(state.join("runs")) else {
        return Vec::new();
    };
    let mut tags: Vec<String> = rd
        .flatten()
        .filter(|e| e.path().join("env.sh").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    tags.sort();
    tags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_sh_round_trips_every_field() {
        let r = Run {
            tag: "t1".into(),
            run: "/s/runs/t1".into(),
            w: "/s/runs/t1/scratch".into(),
            sockdir: "/tmp/at.AbCd".into(),
            sock: "/tmp/at.AbCd/a.sock".into(),
            proj: "/tmp/at.AbCd/proj".into(),
            port: "50130".into(),
            pport: "50131".into(),
            proxy: "http://127.0.0.1:50131".into(),
            stub_pid: 11,
            instance_pid: 12,
            sampler_pid: 13,
            claude_bin: "/Users//o/Library/Application Support/aterm/pkg/store/claude/1/bin/claude"
                .into(),
            claude_ver: "2.1.300".into(),
            cols: "144".into(),
            rows: "50".into(),
            seam: "0".into(),
            codex_bin: "/Users//o/Library/Application Support/aterm/pkg/store/codex/1/bin/codex"
                .into(),
            codex_ver: "0.163.0".into(),
            gemini_bin: "/opt/homebrew/Cellar/gemini-cli/0.46.0/libexec/lib/node_modules/@google/gemini-cli/bundle/gemini.js"
                .into(),
            gemini_ver: "0.46.0".into(),
            agy_bin: "/Users//o/Library/Application Support/aterm/pkg/store/agy/1000001000004000000/bin/agy"
                .into(),
            agy_ver: "1.4.0".into(),
            muse_bin: "/Users//o/Library/Application Support/aterm/pkg/store/muse/2001004010006427002/bin/muse"
                .into(),
            muse_ver: "1.4.10-R6427.1".into(),
            agent: "codex".into(),
            codex_mode: "chatgpt".into(),
            aterm: "/Users//o/Developer/aterm/target/debug/aterm".into(),
            mark: "lab.jail.t1.0123456789abcdef".into(),
            instance_env: "TMPDIR=/tmp ATERM_DEBUG_PASTE_FILE=/s/runs/t1/paste.txt".into(),
            clock: "1".into(),
            window: "1".into(),
            reroute: "1".into(),
        };
        assert_eq!(Run::parse(&r.render()), r);
        assert_eq!(r.fixture_prefix(), "codex-0.163.0");
        for (agent, prefix) in [
            ("gemini", "gemini-0.46.0"),
            ("agy", "agy-1.4.0"),
            ("muse", "muse-1.4.10-R6427.1"),
            ("claude", "claude-2.1.300"),
            ("", "claude-2.1.300"),
            ("bogus", "claude-2.1.300"),
        ] {
            let r = Run {
                agent: agent.into(),
                ..r.clone()
            };
            assert_eq!(r.fixture_prefix(), prefix, "{agent}");
        }
        let unread = Run {
            agent: "muse".into(),
            muse_ver: String::new(),
            ..r
        };
        assert_eq!(unread.fixture_prefix(), "muse-?");
    }

    #[test]
    fn a_verb_whose_one_argument_is_its_tag_takes_a_lab_that_is_down() {
        let gone = "ki1016-gone-lab";
        assert!(!is_run(gone));
        let mut a = vec![gone.to_string(), "--keep-dumps".to_string()];
        assert_eq!(take_only_tag(&mut a), Ok(gone.to_string()));
        assert_eq!(a, vec!["--keep-dumps".to_string()]);
        let mut flags = vec!["--keep-dumps".to_string()];
        assert_eq!(take_only_tag(&mut flags), take_tag(&mut Vec::new()));
        assert_eq!(flags, vec!["--keep-dumps".to_string()]);
        assert!(take_only_tag(&mut vec!["no/tag".to_string()]).is_err());
    }

    #[test]
    fn reads_the_env_sh_printf_q_wrote() {
        let r = Run::parse(
            "TAG=base1\nCLAUDE_BIN=/Users//o/Library/Application\\ Support/aterm/pkg/store/claude/1/bin/claude\nSTUB_PID=54527\n",
        );
        assert_eq!(r.tag, "base1");
        assert_eq!(
            r.claude_bin,
            "/Users//o/Library/Application Support/aterm/pkg/store/claude/1/bin/claude"
        );
        assert_eq!(r.stub_pid, 54527);
    }
}
