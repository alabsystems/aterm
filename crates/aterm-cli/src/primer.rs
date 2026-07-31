// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm agents` — the primer installer that makes coding agents aterm-aware.
//!
//! ## Why this exists (the delivery problem)
//!
//! aterm already TELLS an agent everything it needs — `aterm help` inside a session
//! prints the full agent operating brief — but a coding agent (Claude Code, Codex
//! CLI, Gemini CLI, ...) never LOOKS unless something puts aterm into its context
//! first. Nothing on the screen can do that: an agent reads its own stdin and its
//! context files, never the terminal scrollback, so a banner injected into the PTY
//! reaches only the human. The one channel every major agent reliably loads in
//! EVERY project is its global context file (`~/.claude/CLAUDE.md`,
//! `~/.codex/AGENTS.md`, `~/.gemini/GEMINI.md`, ...).
//!
//! So `aterm agents` manages a short, marked, SELF-GATING primer block in those
//! files: the block itself instructs the agent to detect aterm via
//! `$TERM_PROGRAM`/`$ATERM_CHILD` and to ignore the section in any other terminal —
//! installing it is harmless outside aterm, and inside aterm it is exactly the
//! 3-line pointer (`aterm help`) that unlocks the whole brief.
//!
//! ## Contract
//!
//! * The block lives between [`MARK_BEGIN`]-shaped and [`MARK_END`] marker lines and
//!   is the ONLY thing this module ever touches — user content outside the markers
//!   is preserved byte-for-byte, and `remove` deletes exactly the block.
//! * Idempotent: `install` over a current block is a no-op; over an older/edited
//!   block it updates in place at the same position.
//! * A bare `install` touches only DETECTED agents (their config dir exists — the
//!   signal the agent is actually in use); naming an agent forces it.
//! * Fail-closed: a begin marker without its end marker is reported as corrupt and
//!   the file is left untouched — never a destructive guess.
//!
//! Pure string transforms ([`upsert_block`] / [`remove_block`] / [`block_state`])
//! carry the logic so every edge is unit-testable; [`agents_report`] is the thin
//! filesystem wrapper `parse_args` dispatches to.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Every marker line any past or future primer version begins with — the search
/// key for an installed block of ANY version (so an old block is found, reported
/// stale, and updated in place rather than duplicated).
const MARK_PREFIX: &str = "<!-- aterm primer";

/// The begin marker of the CURRENT primer version. Bump the version token when
/// [`PRIMER_BODY`] changes meaningfully; `install` then reports existing blocks
/// as `stale` and rewrites them in place.
const MARK_BEGIN: &str =
    "<!-- aterm primer v1 — managed by `aterm agents`; `aterm agents remove` uninstalls -->";

/// The end marker closing the managed block.
const MARK_END: &str = "<!-- /aterm primer -->";

/// The primer itself — the 3-line brief an agent needs: how to DETECT aterm, where
/// the full manual lives (`aterm help`), and why its own context env vars were
/// stripped. Self-gating: the last sentence tells the agent to ignore the section
/// in any other terminal, so the block is safe in a global context file that loads
/// everywhere. Deny-prefix names must stay in sync with
/// [`aterm_types::domain::ENV_DENY_PREFIXES`] (pinned by a test below).
const PRIMER_BODY: &str = "\
## aterm
If the environment has `TERM_PROGRAM=aterm` or `ATERM_CHILD=1`, this terminal is aterm — an
AI-native terminal whose sessions are introspectable and drivable: agents and humans can read
the live screen, send input, and await real transitions, concurrently. Run `aterm help` for the
agent operating brief and `aterm help introspection` for the `aterm ctl` control verbs (`aterm`
is already on PATH inside aterm sessions). aterm deliberately STRIPS `CLAUDE*`, `ANTHROPIC_*`,
`COPILOT_*`, `CODEX_*`, `CURSOR_*`, and `AI_*` env vars from the shells it spawns — `aterm help`
explains why. If neither variable is set, you are not inside aterm; ignore this section.";

/// The full managed block (markers + body), newline-terminated — what `install`
/// writes and `aterm agents primer` prints for manual pasting.
#[must_use]
pub(crate) fn primer_block() -> String {
    format!("{MARK_BEGIN}\n{PRIMER_BODY}\n{MARK_END}\n")
}

// ---------------------------------------------------------------------------
// Managed SKILL files
//
// The primer is a marked BLOCK inside a file the user also owns. A skill is the
// opposite shape: a WHOLE file that is entirely aterm's, in an agent-specific
// skills directory. So it gets its own three-state model, and one extra state
// the primer cannot have — `Foreign`, a file at our path with no marker, which
// is the user's and is never touched.
//
// Compiled in, never artifact-supplied — the same rule as atpkg's shell hooks.
// ---------------------------------------------------------------------------

/// Every marker line any past or future managed skill begins with — the search
/// key that identifies a file as OURS regardless of version.
const SKILL_MARK_PREFIX: &str = "<!-- aterm skill";

/// The `drive-aterm` skill: how one agent drives another aterm session over the
/// control socket. Compiled in from the repo asset so the shipped binary is the
/// single source of truth — there is no separate file to forget to update.
const DRIVE_SKILL_BODY: &str = include_str!("../assets/drive-aterm-skill.md");

/// One managed skill file: `dir_rel` is the agent-relative skills subdirectory,
/// `body` the compiled-in content.
struct SkillFile {
    /// Path under `$HOME`, `/`-separated (joined per-platform by [`home_join`]).
    path: &'static str,
    /// The compiled-in file content.
    body: &'static str,
}

/// The skills this build ships, per agent `name`. Only Claude Code defines a
/// skills convention (`~/.claude/skills/<name>/SKILL.md`) today; the other agents
/// in [`AGENT_FILES`] get the primer only. Extend here when they grow one.
fn skills_for(agent: &str) -> &'static [SkillFile] {
    match agent {
        "claude" => &[SkillFile {
            path: ".claude/skills/drive-aterm/SKILL.md",
            body: DRIVE_SKILL_BODY,
        }],
        _ => &[],
    }
}

/// The install state of one managed skill file.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum SkillState {
    /// No file at the path.
    Absent,
    /// Our file, byte-identical to this build's content.
    Current,
    /// Our file (marker present) but different — an older version or hand-edited.
    Stale,
    /// A file exists with NO aterm marker: the user's own skill of the same name.
    /// Never overwritten — removing the marker is the documented way to opt out.
    Foreign,
}

/// Classify existing `content` at a skill path against the compiled-in `body`.
/// Pure, so every edge is unit-testable without touching the filesystem.
#[must_use]
pub(crate) fn skill_state(content: &str, body: &str) -> SkillState {
    if content == body {
        return SkillState::Current;
    }
    if content
        .lines()
        .any(|l| l.trim_start().starts_with(SKILL_MARK_PREFIX))
    {
        SkillState::Stale
    } else {
        SkillState::Foreign
    }
}

/// One coding agent's global context file. `dir` doubles as the DETECTION signal:
/// its existence means the agent is installed/in use on this machine, so a bare
/// `aterm agents install` may write there. Paths are `/`-separated segments under
/// `$HOME`, joined per-platform by [`home_join`].
struct AgentFile {
    /// The selector on the command line (`aterm agents install <name>`).
    name: &'static str,
    /// The product name for human-readable listings.
    product: &'static str,
    /// The agent's config dir under `$HOME` — existence ⇒ detected.
    dir: &'static str,
    /// The agent's ALWAYS-LOADED global context file under `$HOME`.
    file: &'static str,
}

/// The registry of supported agents. Global-context-file conventions as of 2026:
/// each entry is the ONE file that agent loads in every project, which is what
/// makes the primer reach it regardless of cwd. Extend here to support another
/// agent — everything else (status/install/remove/usage) derives from this table.
const AGENT_FILES: &[AgentFile] = &[
    AgentFile {
        name: "claude",
        product: "Claude Code",
        dir: ".claude",
        file: ".claude/CLAUDE.md",
    },
    AgentFile {
        name: "codex",
        product: "Codex CLI",
        dir: ".codex",
        file: ".codex/AGENTS.md",
    },
    AgentFile {
        name: "gemini",
        product: "Gemini CLI",
        dir: ".gemini",
        file: ".gemini/GEMINI.md",
    },
    AgentFile {
        name: "opencode",
        product: "OpenCode",
        dir: ".config/opencode",
        file: ".config/opencode/AGENTS.md",
    },
];

/// The install state of the managed block within one file's content.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum BlockState {
    /// No marker present.
    Absent,
    /// The current block, byte-identical.
    Current,
    /// A marker is present but the block differs (older version, or hand-edited).
    Stale,
}

/// Locate the managed block: the byte range covering the begin-marker line through
/// the end-marker line (inclusive, with the end line's terminating newline when
/// present). `Ok(None)` when no begin marker exists (a stray end marker alone is
/// treated as user content). `Err` when a begin marker has no end marker after it —
/// the fail-closed corrupt case where no edit is safe.
fn find_block(content: &str) -> Result<Option<(usize, usize)>, String> {
    let mut offset = 0usize;
    let mut begin: Option<usize> = None;
    for line in content.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']).trim();
        match begin {
            None if trimmed.starts_with(MARK_PREFIX) && trimmed != MARK_END => {
                begin = Some(offset);
            }
            Some(start) if trimmed == MARK_END => {
                return Ok(Some((start, offset + line.len())));
            }
            _ => {}
        }
        offset += line.len();
    }
    match begin {
        Some(_) => Err(format!(
            "found `{MARK_PREFIX} ...` without a closing `{MARK_END}` — refusing to touch the file"
        )),
        None => Ok(None),
    }
}

/// The block's install state within `content`. `Err` on a corrupt (unterminated)
/// block, mirroring [`find_block`].
pub(crate) fn block_state(content: &str) -> Result<BlockState, String> {
    match find_block(content)? {
        None => Ok(BlockState::Absent),
        Some((start, end)) => {
            let current = primer_block();
            // Compare modulo the trailing newline: a block at EOF may lack one.
            if content[start..end].trim_end_matches('\n') == current.trim_end_matches('\n') {
                Ok(BlockState::Current)
            } else {
                Ok(BlockState::Stale)
            }
        }
    }
}

/// Insert the current block (append, blank-line separated) or replace a stale one
/// in place. `Ok(None)` when the content already carries the current block — the
/// idempotent no-op. `Err` on a corrupt block, leaving the caller's file untouched.
pub(crate) fn upsert_block(content: &str) -> Result<Option<String>, String> {
    let block = primer_block();
    match find_block(content)? {
        Some((start, end)) => {
            if content[start..end].trim_end_matches('\n') == block.trim_end_matches('\n') {
                Ok(None)
            } else {
                Ok(Some(format!(
                    "{}{}{}",
                    &content[..start],
                    block,
                    &content[end..]
                )))
            }
        }
        None => {
            // Append with exactly one separating blank line after existing content.
            let mut out = content.trim_end_matches('\n').to_string();
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(&block);
            Ok(Some(out))
        }
    }
}

/// Remove the managed block, collapsing the seam so a former
/// `content\n\n<block>` round-trips back to `content\n`. `Ok(None)` when no block
/// is present; `Err` on a corrupt block.
pub(crate) fn remove_block(content: &str) -> Result<Option<String>, String> {
    match find_block(content)? {
        None => Ok(None),
        Some((start, end)) => {
            let mut out = format!("{}{}", &content[..start], &content[end..]);
            if out.trim().is_empty() {
                out.clear();
            } else {
                // Collapse the separator blank line install added (or trailing
                // blanks a mid-file removal leaves) without touching interior text.
                let trimmed = out.trim_end_matches('\n');
                out.truncate(trimmed.len());
                out.push('\n');
            }
            Ok(Some(out))
        }
    }
}

/// The user's home directory, from the platform's canonical env var. `None` (an
/// unset/empty var) makes `aterm agents` fail with a clear message rather than
/// writing relative to an arbitrary cwd. Deliberately NOT
/// `aterm_types::dirs::home_dir`: that one accepts an empty `$HOME` and falls
/// back to `/etc/passwd`, either of which would defeat the fail-fast here.
pub(crate) fn home_dir() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Join a `/`-separated registry path onto `home` segment-by-segment, so the
/// registry stays readable while Windows gets native separators.
fn home_join(home: &Path, rel: &str) -> PathBuf {
    let mut p = home.to_path_buf();
    for seg in rel.split('/') {
        p.push(seg);
    }
    p
}

/// One agent's on-disk situation, resolved read-only for `status` and reused as
/// the decision input for `install`/`remove`.
enum FileSituation {
    /// The config dir does not exist — the agent is not in use on this machine.
    NotDetected,
    /// Dir exists, context file does not.
    NoFile,
    /// File exists; the block's state within it (or a corrupt-block message).
    File(Result<BlockState, String>),
}

fn situation(home: &Path, agent: &AgentFile) -> FileSituation {
    if !home_join(home, agent.dir).is_dir() {
        return FileSituation::NotDetected;
    }
    let path = home_join(home, agent.file);
    match std::fs::read_to_string(&path) {
        Ok(content) => FileSituation::File(block_state(&content)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => FileSituation::NoFile,
        Err(e) => FileSituation::File(Err(format!("unreadable: {e}"))),
    }
}

/// The usage text for `aterm agents` (printed on an unknown subcommand/agent).
fn usage() -> String {
    let mut s = String::from(
        "usage: aterm agents [status | install [<agent>…] | remove [<agent>…] | primer]\n\
         \n\
         Manage the 3-line aterm primer in coding agents' global context files, so any\n\
         agent launched inside aterm knows what aterm is and to run `aterm help`.\n\
         \n\
           status    each agent's context file and whether the primer is installed (default)\n\
           install   install/update the primer for every detected agent (config dir exists);\n\
         \x20           name agents to force them (creates the file if needed)\n\
         \x20 remove    remove the primer block (everywhere, or from the named agents)\n\
         \x20 primer    print the block itself — paste it into any AGENTS.md/CLAUDE.md\n\
         \n\
         agents:\n",
    );
    for a in AGENT_FILES {
        let _ = writeln!(s, "  {:<9} {}  (~/{})", a.name, a.product, a.file);
    }
    s
}

/// Resolve the named agents (or, for an empty list, every registry entry) to
/// registry rows. `Err` is the usage error naming the unknown selector.
fn select<'a>(names: &[String]) -> Result<Vec<&'a AgentFile>, String> {
    if names.is_empty() {
        return Ok(AGENT_FILES.iter().collect());
    }
    names
        .iter()
        .map(|n| {
            AGENT_FILES
                .iter()
                .find(|a| a.name == n.as_str())
                .ok_or_else(|| format!("aterm agents: unknown agent '{n}'\n\n{}", usage()))
        })
        .collect()
}

/// The `aterm agents` command: `(report, exit_code)`. `home` is injected so the
/// whole surface is testable against a scratch directory; the real caller passes
/// [`home_dir`]. Exit codes: 0 success (status is always 0), 1 an install/remove
/// failure (corrupt block, I/O error), 2 usage.
pub(crate) fn agents_report(home: &Path, args: &[String]) -> (String, i32) {
    let sub = args.first().map(String::as_str).unwrap_or("status");
    let names = args.get(1..).unwrap_or(&[]);
    match sub {
        "primer" => (primer_block(), 0),
        "status" => {
            let mut out = String::new();
            for a in AGENT_FILES {
                let state = match situation(home, a) {
                    FileSituation::NotDetected => format!("not detected (no ~/{})", a.dir),
                    FileSituation::NoFile => "absent (no context file yet)".to_string(),
                    FileSituation::File(Ok(BlockState::Absent)) => "absent".to_string(),
                    FileSituation::File(Ok(BlockState::Current)) => "installed".to_string(),
                    FileSituation::File(Ok(BlockState::Stale)) => {
                        "stale (install updates it)".to_string()
                    }
                    FileSituation::File(Err(e)) => format!("ERROR: {e}"),
                };
                let _ = writeln!(out, "{:<9} ~/{:<28} {state}", a.name, a.file);
                // Skills are whole managed FILES, listed under their agent so the
                // status view stays one line per artifact.
                for s in skills_for(a.name) {
                    let path = home_join(home, s.path);
                    let st = match std::fs::read_to_string(&path) {
                        Err(_) => "absent".to_string(),
                        Ok(c) => match skill_state(&c, s.body) {
                            SkillState::Current => "installed".to_string(),
                            SkillState::Stale => "stale (install updates it)".to_string(),
                            SkillState::Foreign => "foreign — yours, left alone".to_string(),
                            SkillState::Absent => "absent".to_string(),
                        },
                    };
                    let _ = writeln!(out, "{:<9} ~/{:<28} {st}", "  skill", s.path);
                }
            }
            out.push_str(
                "\n`aterm agents install` installs/updates the primer AND the bundled skills\n\
                 for detected agents; `aterm agents primer` prints the block for manual pasting.\n",
            );
            (out, 0)
        }
        "install" => {
            let agents = match select(names) {
                Ok(a) => a,
                Err(msg) => return (msg, 2),
            };
            let forced = !names.is_empty();
            let mut out = String::new();
            let mut failed = false;
            for a in agents {
                let path = home_join(home, a.file);
                let sit = situation(home, a);
                // Captured BEFORE the match below consumes `sit` (its `File`
                // arm moves the inner Result, which is not Copy).
                let undetected = matches!(sit, FileSituation::NotDetected);
                let verdict = match sit {
                    FileSituation::NotDetected if !forced => {
                        format!("skipped — no ~/{} (not detected; name it to force)", a.dir)
                    }
                    FileSituation::NotDetected | FileSituation::NoFile => {
                        let write = path
                            .parent()
                            .map_or(Ok(()), std::fs::create_dir_all)
                            .and_then(|()| std::fs::write(&path, primer_block()));
                        match write {
                            Ok(()) => "installed (new file)".to_string(),
                            Err(e) => {
                                failed = true;
                                format!("ERROR: {e}")
                            }
                        }
                    }
                    FileSituation::File(state) => {
                        // Re-read + transform + write. `situation` proved readability
                        // just above; a race between the two reads only changes which
                        // content gets upserted, never what the transform preserves.
                        let done = state.and_then(|_| {
                            let content =
                                std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
                            match upsert_block(&content)? {
                                None => Ok("already installed"),
                                Some(updated) => {
                                    std::fs::write(&path, updated).map_err(|e| e.to_string())?;
                                    Ok("installed")
                                }
                            }
                        });
                        match done {
                            Ok(v) => v.to_string(),
                            Err(e) => {
                                failed = true;
                                format!("ERROR: {e}")
                            }
                        }
                    }
                };
                let _ = writeln!(out, "{:<9} ~/{:<28} {verdict}", a.name, a.file);

                // Bundled skills ride the SAME install: an agent that gets the
                // primer gets the skills too. A skipped (undetected, unforced)
                // agent skips its skills as well — never create `~/.claude` for
                // someone who does not use Claude Code.
                if undetected && !forced {
                    continue;
                }
                for s in skills_for(a.name) {
                    let sp = home_join(home, s.path);
                    let existing = std::fs::read_to_string(&sp).ok();
                    let state = existing
                        .as_deref()
                        .map_or(SkillState::Absent, |c| skill_state(c, s.body));
                    let verdict = match state {
                        SkillState::Current => "already installed".to_string(),
                        // The user's own file at our path: never clobbered.
                        SkillState::Foreign => {
                            "skipped — not an aterm-managed file (yours)".to_string()
                        }
                        SkillState::Absent | SkillState::Stale => {
                            let w = sp
                                .parent()
                                .map_or(Ok(()), std::fs::create_dir_all)
                                .and_then(|()| std::fs::write(&sp, s.body));
                            match w {
                                Ok(()) if state == SkillState::Stale => "updated".to_string(),
                                Ok(()) => "installed".to_string(),
                                Err(e) => {
                                    failed = true;
                                    format!("ERROR: {e}")
                                }
                            }
                        }
                    };
                    let _ = writeln!(out, "{:<9} ~/{:<28} {verdict}", "  skill", s.path);
                }
            }
            (out, i32::from(failed))
        }
        "remove" => {
            let agents = match select(names) {
                Ok(a) => a,
                Err(msg) => return (msg, 2),
            };
            let mut out = String::new();
            let mut failed = false;
            for a in agents {
                let path = home_join(home, a.file);
                let verdict = match std::fs::read_to_string(&path) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        "nothing to remove".to_string()
                    }
                    Err(e) => {
                        failed = true;
                        format!("ERROR: {e}")
                    }
                    Ok(content) => match remove_block(&content) {
                        Ok(None) => "nothing to remove".to_string(),
                        Ok(Some(rest)) => match std::fs::write(&path, rest) {
                            Ok(()) => "removed".to_string(),
                            Err(e) => {
                                failed = true;
                                format!("ERROR: {e}")
                            }
                        },
                        Err(e) => {
                            failed = true;
                            format!("ERROR: {e}")
                        }
                    },
                };
                let _ = writeln!(out, "{:<9} ~/{:<28} {verdict}", a.name, a.file);

                // Symmetric uninstall: delete only files we still recognise as
                // OURS. A `Foreign` file (marker removed) is the user's — and a
                // `Stale` one is ours from an older build, so it does go.
                for s in skills_for(a.name) {
                    let sp = home_join(home, s.path);
                    let verdict = match std::fs::read_to_string(&sp) {
                        Err(_) => "nothing to remove".to_string(),
                        Ok(c) => match skill_state(&c, s.body) {
                            SkillState::Foreign => {
                                "kept — not an aterm-managed file (yours)".to_string()
                            }
                            _ => match std::fs::remove_file(&sp) {
                                Ok(()) => "removed".to_string(),
                                Err(e) => {
                                    failed = true;
                                    format!("ERROR: {e}")
                                }
                            },
                        },
                    };
                    let _ = writeln!(out, "{:<9} ~/{:<28} {verdict}", "  skill", s.path);
                }
            }
            (out, i32::from(failed))
        }
        _ => (
            format!("aterm agents: unknown command '{sub}'\n\n{}", usage()),
            2,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primer_is_three_sentences_of_detection_manual_and_hygiene() {
        let block = primer_block();
        // Detection: both identity vars an agent can check.
        assert!(block.contains("TERM_PROGRAM=aterm") && block.contains("ATERM_CHILD"));
        // The pointer that unlocks the full brief.
        assert!(block.contains("`aterm help`"));
        // Self-gating: safe in a global file loaded in every terminal.
        assert!(block.contains("ignore this section"));
        // Marked + versioned, so installs are idempotent and updatable.
        assert!(block.starts_with(MARK_BEGIN) && block.trim_end().ends_with(MARK_END));
    }

    #[test]
    fn primer_names_every_env_deny_prefix() {
        // The hygiene sentence must stay in sync with the real sanitize list —
        // an agent reading the primer learns exactly which vars aterm strips.
        for prefix in aterm_types::domain::ENV_DENY_PREFIXES {
            let named = prefix.trim_end_matches('_').trim_end_matches('*');
            // `_DEVTOOL_` is an internal implementation marker, not an agent's
            // context prefix — the primer stays 3 lines by omitting it.
            if *prefix == "_DEVTOOL_" {
                continue;
            }
            assert!(
                PRIMER_BODY.contains(named),
                "primer must name deny prefix {prefix} (env_sanitize drift)"
            );
        }
    }

    #[test]
    fn upsert_appends_once_and_is_idempotent() {
        let v1 = upsert_block("").unwrap().unwrap();
        assert_eq!(v1, primer_block());
        assert_eq!(
            upsert_block(&v1).unwrap(),
            None,
            "second install is a no-op"
        );

        let with_user = upsert_block("# My rules\n\nBe nice.\n").unwrap().unwrap();
        assert!(with_user.starts_with("# My rules\n\nBe nice.\n\n<!-- aterm primer"));
        assert_eq!(upsert_block(&with_user).unwrap(), None);
        assert_eq!(block_state(&with_user).unwrap(), BlockState::Current);
    }

    #[test]
    fn stale_block_updates_in_place_preserving_surroundings() {
        // An older version: same markers shape, different version token + body.
        let old = "before\n\n<!-- aterm primer v0 — managed by `aterm agents` -->\nold body\n<!-- /aterm primer -->\n\nafter\n";
        assert_eq!(block_state(old).unwrap(), BlockState::Stale);
        let updated = upsert_block(old).unwrap().unwrap();
        assert!(updated.starts_with("before\n\n<!-- aterm primer v1"));
        assert!(updated.ends_with("<!-- /aterm primer -->\n\nafter\n"));
        assert!(!updated.contains("old body"));
        assert_eq!(block_state(&updated).unwrap(), BlockState::Current);
    }

    #[test]
    fn remove_round_trips_to_the_original() {
        let original = "# My rules\n\nBe nice.\n";
        let installed = upsert_block(original).unwrap().unwrap();
        let removed = remove_block(&installed).unwrap().unwrap();
        assert_eq!(removed, original);
        // A file that was only the block empties out entirely.
        assert_eq!(remove_block(&primer_block()).unwrap().unwrap(), "");
        // Nothing installed → nothing to remove.
        assert_eq!(remove_block(original).unwrap(), None);
    }

    #[test]
    fn unterminated_block_fails_closed() {
        let corrupt = "x\n<!-- aterm primer v1 -->\nno end marker\n";
        assert!(block_state(corrupt).is_err());
        assert!(upsert_block(corrupt).is_err());
        assert!(remove_block(corrupt).is_err());
        // A stray END marker alone is user content, not a block.
        let stray = format!("x\n{MARK_END}\ny\n");
        assert_eq!(block_state(&stray).unwrap(), BlockState::Absent);
    }

    #[test]
    fn install_touches_only_detected_agents_unless_forced() {
        let home = aterm_tempfile::tempdir().unwrap();
        // Detected: claude (dir exists, no file yet). Undetected: everyone else.
        std::fs::create_dir_all(home.path().join(".claude")).unwrap();
        let (out, code) = agents_report(home.path(), &["install".to_string()]);
        assert_eq!(code, 0, "skips are not failures:\n{out}");
        assert!(out.contains("claude") && out.contains("installed (new file)"));
        assert!(out.contains("not detected"));
        let claude = std::fs::read_to_string(home.path().join(".claude/CLAUDE.md")).unwrap();
        assert_eq!(claude, primer_block());
        assert!(
            !home.path().join(".codex").exists(),
            "a bare install must not create an undetected agent's dir"
        );

        // Forcing by name creates the file (and dir) for an undetected agent.
        let (out, code) = agents_report(home.path(), &["install".to_string(), "codex".to_string()]);
        assert_eq!(code, 0, "{out}");
        let codex = std::fs::read_to_string(home.path().join(".codex/AGENTS.md")).unwrap();
        assert_eq!(codex, primer_block());
    }

    #[test]
    fn install_preserves_user_content_and_remove_restores_it() {
        let home = aterm_tempfile::tempdir().unwrap();
        let dir = home.path().join(".gemini");
        std::fs::create_dir_all(&dir).unwrap();
        let user = "# Gemini rules\n\nAlways use uv.\n";
        std::fs::write(dir.join("GEMINI.md"), user).unwrap();

        let (out, code) =
            agents_report(home.path(), &["install".to_string(), "gemini".to_string()]);
        assert_eq!(code, 0, "{out}");
        let content = std::fs::read_to_string(dir.join("GEMINI.md")).unwrap();
        assert!(content.starts_with(user) && content.contains(MARK_END));

        let (out, code) = agents_report(home.path(), &["remove".to_string(), "gemini".to_string()]);
        assert_eq!(code, 0, "{out}");
        assert_eq!(
            std::fs::read_to_string(dir.join("GEMINI.md")).unwrap(),
            user
        );
    }

    #[test]
    fn status_reports_every_registry_agent_and_exits_zero() {
        let home = aterm_tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".claude")).unwrap();
        std::fs::write(home.path().join(".claude/CLAUDE.md"), primer_block()).unwrap();
        let (out, code) = agents_report(home.path(), &[]);
        assert_eq!(code, 0);
        for a in AGENT_FILES {
            assert!(out.contains(a.name), "status must list {}", a.name);
        }
        assert!(out.contains("installed"));
        assert!(out.contains("not detected"));
    }

    #[test]
    fn unknown_subcommand_and_unknown_agent_are_usage_errors() {
        let home = aterm_tempfile::tempdir().unwrap();
        let (_, code) = agents_report(home.path(), &["frobnicate".to_string()]);
        assert_eq!(code, 2);
        let (msg, code) =
            agents_report(home.path(), &["install".to_string(), "copilot".to_string()]);
        assert_eq!(code, 2);
        assert!(msg.contains("unknown agent"));
    }

    #[test]
    fn primer_subcommand_prints_the_block_verbatim() {
        let home = aterm_tempfile::tempdir().unwrap();
        let (out, code) = agents_report(home.path(), &["primer".to_string()]);
        assert_eq!(code, 0);
        assert_eq!(out, primer_block());
    }

    // ---- bundled skill files -------------------------------------------------

    /// The compiled-in skill must carry its marker, or every install would
    /// classify it `Foreign` and refuse to write. This is the one property that
    /// silently disables the whole feature if the asset is edited carelessly.
    #[test]
    fn bundled_skill_carries_its_managed_marker() {
        assert!(
            DRIVE_SKILL_BODY
                .lines()
                .any(|l| l.trim_start().starts_with(SKILL_MARK_PREFIX)),
            "the shipped skill lost its `{SKILL_MARK_PREFIX}` marker line"
        );
        assert_eq!(
            skill_state(DRIVE_SKILL_BODY, DRIVE_SKILL_BODY),
            SkillState::Current,
            "the shipped body must classify as Current against itself"
        );
    }

    /// The skill must be a valid Claude Code skill: YAML frontmatter with a
    /// `name:` and a `description:` (the fields the harness matches on).
    #[test]
    fn bundled_skill_has_usable_frontmatter() {
        let mut lines = DRIVE_SKILL_BODY.lines();
        assert_eq!(
            lines.next().map(str::trim),
            Some("---"),
            "must open with frontmatter"
        );
        let head: Vec<&str> = DRIVE_SKILL_BODY.lines().take(12).collect();
        assert!(
            head.iter().any(|l| l.starts_with("name:")),
            "frontmatter needs name:"
        );
        assert!(
            head.iter().any(|l| l.starts_with("description:")),
            "frontmatter needs description: (it is what triggers the skill)"
        );
    }

    /// The skill states must be distinguishable — especially `Foreign`, which is
    /// what stops aterm from clobbering a user's own same-named skill.
    #[test]
    fn skill_state_distinguishes_ours_from_the_users() {
        let body = "---\nname: x\n---\n<!-- aterm skill v1 -->\nbody\n";
        assert_eq!(skill_state(body, body), SkillState::Current);

        // Our marker, different content -> ours, outdated -> safe to overwrite.
        let older = "---\nname: x\n---\n<!-- aterm skill v0 -->\nold body\n";
        assert_eq!(skill_state(older, body), SkillState::Stale);

        // No marker at all -> the user's file. NEVER overwritten.
        let theirs = "---\nname: x\n---\nmy own notes\n";
        assert_eq!(skill_state(theirs, body), SkillState::Foreign);
    }

    /// Only Claude Code defines a skills convention today; the others must get
    /// none, so `install` never fabricates a skills dir for an agent that has no
    /// such concept.
    #[test]
    fn skills_are_registered_only_for_agents_that_have_them() {
        assert_eq!(skills_for("claude").len(), 1);
        for a in AGENT_FILES.iter().filter(|a| a.name != "claude") {
            assert!(
                skills_for(a.name).is_empty(),
                "{} has no skills convention; shipping one would create a bogus dir",
                a.name
            );
        }
        // Every registered skill must live UNDER its agent's own config dir.
        for a in AGENT_FILES {
            for s in skills_for(a.name) {
                assert!(
                    s.path.starts_with(a.dir),
                    "skill {} escapes {}'s config dir",
                    s.path,
                    a.name
                );
            }
        }
    }
}
