// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-primer` — the installer that makes coding agents aterm-aware, behind
//! `aterm agents` AND behind every session aterm opens.
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
//! So this crate manages a short, marked, SELF-GATING primer block in those files:
//! the block itself instructs the agent to detect aterm via
//! `$TERM_PROGRAM`/`$ATERM_CHILD` and to ignore the section in any other terminal —
//! installing it is harmless outside aterm, and inside aterm it is exactly the
//! 3-line pointer (`aterm help`) that unlocks the whole brief.
//!
//! ## Two callers, one installer
//!
//! * [`agents_report`] is the explicit, scriptable `aterm agents` surface
//!   (status / install / remove / primer).
//! * [`auto_prime`] is what the GUI runs when it opens a session: the same upsert
//!   over every DETECTED agent, fail-soft and idempotent. The primer is installed
//!   by aterm itself — "having aterm installed in aterm itself isn't something
//!   that happens later" (owner decision, docs/AGENT-EXPERIENCE-2026-08-26.md §3
//!   S1): an `aterm agents install` nobody ran was the whole of finding F1, every
//!   agent row `absent` on a machine that had run aterm for weeks.
//!
//! ## Contract
//!
//! * The block lives between [`MARK_BEGIN`]-shaped and [`MARK_END`] marker lines and
//!   is the ONLY thing this crate ever touches in a context file — user content
//!   outside the markers is preserved byte-for-byte, and `remove` deletes exactly
//!   the block.
//! * Idempotent: an install over a current block is a no-op; over an older/edited
//!   block it updates in place at the same position.
//! * Only DETECTED agents (their config dir exists — the signal the agent is
//!   actually in use) are touched unless the user names one; neither caller ever
//!   creates an agent's config dir.
//! * Fail-closed: a begin marker without its end marker is reported as corrupt and
//!   the file is left untouched — never a destructive guess.
//! * Per-agent ADDENDA: an agent whose runtime needs one extra paragraph (Codex's
//!   sandbox refuses the control socket) gets it inside ITS block only; the
//!   generic block stays three lines.
//!
//! Pure string transforms ([`upsert_block`] / [`remove_block`] / [`block_state`])
//! carry the logic so every edge is unit-testable; the two public entry points are
//! thin filesystem wrappers over the same per-file operations.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// Every marker line any past or future primer version begins with — the search
/// key for an installed block of ANY version (so an old block is found, reported
/// stale, and updated in place rather than duplicated).
const MARK_PREFIX: &str = "<!-- aterm primer";

/// The begin marker of the CURRENT primer version. Bump the version token when
/// [`PRIMER_BODY`] or an addendum changes meaningfully; every installer then
/// reports existing blocks as `stale` and rewrites them in place. v2 (2026-08-26):
/// the Codex sandbox addendum — a v1 block on a Codex machine is silent about the
/// one thing that makes every `aterm ctl` verb fail there. v3 (2026-08-27): the
/// first-moves sentence — `windows` and `ls`, and reading a peer's `status` before
/// typing into it. A v2 block sent an agent to `aterm help` and nowhere else; the
/// agent-experience report (docs/AGENT-EXPERIENCE-2026-08-26.md F2/F5) is what it
/// did instead: six calls to learn there were two windows, and three peers it
/// nearly typed into that were other agents. v4 (2026-08-27): the Codex
/// addendum's escalation sentence — v3 told Codex that `aterm ctl` verbs "are
/// read-only unless you use send/turn/key/spawn/close", an allow-list that was
/// already false (paste, feed, ctrl, mouse, signal, meta set, settings, tab and
/// resize all write) and would drift further with every verb added; v4 states
/// the rule by what a verb DOES instead.
const MARK_BEGIN: &str =
    "<!-- aterm primer v4 — managed by `aterm agents`; `aterm agents remove` uninstalls -->";

/// The end marker closing the managed block.
const MARK_END: &str = "<!-- /aterm primer -->";

/// The primer itself — the short brief an agent needs: how to DETECT aterm, where
/// the full manual lives (`aterm help`), its FIRST MOVES (`windows` and `ls`, and
/// reading a peer's `status` before typing into it), and why its own context env
/// vars were stripped. Self-gating: the last sentence tells the agent to ignore the
/// section in any other terminal, so the block is safe in a global context file
/// that loads everywhere. Deny-prefix names must stay in sync with
/// `aterm_types::domain::ENV_DENY_PREFIXES` (pinned by a test below). Depth lives
/// behind `aterm help`, never here: the body is pinned to a byte budget below.
const PRIMER_BODY: &str = "\
## aterm
If the environment has `TERM_PROGRAM=aterm` or `ATERM_CHILD=1`, this terminal is aterm — an
AI-native terminal whose sessions are introspectable and drivable: agents and humans can read
the live screen, send input, and await real transitions, concurrently. Run `aterm help` for the
agent operating brief and `aterm help introspection` for the `aterm ctl` control verbs (`aterm`
is already on PATH inside aterm sessions). First moves: `aterm ctl windows` and `aterm ctl ls`
list every window and session; read a peer's `status` (detail= names the program it is running)
before typing into it. aterm deliberately STRIPS `CLAUDE*`, `ANTHROPIC_*`, `COPILOT_*`,
`CODEX_*`, `CURSOR_*`, and `AI_*` env vars from the shells it spawns — `aterm help` explains
why. If neither variable is set, you are not inside aterm; ignore this section.";

/// Codex CLI's addendum (docs/AGENT-EXPERIENCE-2026-08-26.md §3 S8). Measured on
/// 2026-08-26: Codex's default macOS sandbox refuses AF_UNIX `connect()` outside
/// its writable roots, so an agent that follows the generic primer sees every
/// `aterm ctl` verb fail with EPERM and concludes the fleet is empty — the
/// allowance below is the fix, and only the primer can teach it in every cwd.
///
/// The FLAG is the only spelling this text recommends, because it is the only one
/// that worked when measured (2026-08-27, Codex CLI 0.149.1, a scratch
/// `CODEX_HOME`, `codex sandbox -- aterm ctl ls` against a live instance): the
/// binary carries `network.allow_unix_sockets`, `network.unix_sockets` and
/// `network.dangerously_allow_all_unix_sockets` as config keys, and none of them —
/// nor `[sandbox_workspace_write] writable_roots`/`network_access`, nor
/// `sandbox_mode = "danger-full-access"`, nor the `-c` override — changed what the
/// sandbox permitted (the config WAS read: invalid TOML errors out). A primer that
/// recommended a durable config line nobody had seen work would be finding F8 again.
const CODEX_ADDENDUM: &str = "\
Codex's macOS sandbox only allows AF_UNIX connect() under its writable roots, so every
`aterm ctl` verb fails with `Operation not permitted (os error 1)` until the aterm socket
directory is allowed: run with --allow-unix-socket \"$HOME/Library/Application Support/aterm\",
or request escalation for `aterm ctl` commands — prefer escalating read verbs one at a time
(ls, windows, sessions, status, text, blocks); anything that types, signals, spawns, closes
or changes settings is a write. The flag is the only spelling verified to work: on Codex CLI
0.149.1 the `[network] allow_unix_sockets` config key (and its `-c` override) did not take
effect under `codex sandbox`. A refused `aterm ctl ls` says so — exit 2 and a `hint:` naming
the flag — instead of claiming the fleet is empty.";

/// The one sentence every surface that could leave a user surprised by a
/// reinstalled primer must carry: `aterm agents status` (so the knob is
/// discoverable) and `aterm agents remove` (so a removal is never silently undone
/// by the next session without the user knowing how to make it stick).
pub const AUTO_PRIME_NOTE: &str = "\
aterm installs/updates this primer for every detected agent each time it opens a
session (set `agents_auto_prime = false` in ~/.config/aterm/aterm.toml to stop).";

/// The full managed block (markers + body + the agent's addendum, if any),
/// newline-terminated — what an install writes and `aterm agents primer [<agent>]`
/// prints for manual pasting. `None`, or a name the registry does not know, gives
/// the generic addendum-free block.
#[must_use]
pub fn primer_block(agent: Option<&str>) -> String {
    let addendum = agent
        .and_then(|n| AGENT_FILES.iter().find(|a| a.name == n))
        .and_then(|a| a.addendum);
    block_with(addendum)
}

/// Assemble a block from its parts. The addendum is its own paragraph (blank-line
/// separated) so a Markdown renderer keeps it distinct from the generic brief.
fn block_with(addendum: Option<&str>) -> String {
    match addendum {
        Some(extra) => format!("{MARK_BEGIN}\n{PRIMER_BODY}\n\n{extra}\n{MARK_END}\n"),
        None => format!("{MARK_BEGIN}\n{PRIMER_BODY}\n{MARK_END}\n"),
    }
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
/// control socket. Compiled in from the crate asset so the shipped binary is the
/// single source of truth — there is no separate file to forget to update.
const DRIVE_SKILL_BODY: &str = include_str!("../assets/drive-aterm-skill.md");

/// The `supervise-agent` skill: the SUPERVISION layer over `drive-aterm` — how an
/// agent runs the persistent sweep/classify/review/wait loop over a WORKER
/// session, reviewing each turn against ground truth (not the screen) with a
/// turn budget, a no-progress breaker, and human-escalation. Compiled in from the
/// repo asset, same single-source-of-truth rule as the drive skill.
const SUPERVISE_SKILL_BODY: &str = include_str!("../assets/supervise-agent-skill.md");

/// One managed skill file: `path` is the agent-relative location under `$HOME`,
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
        "claude" => &[
            SkillFile {
                path: ".claude/skills/drive-aterm/SKILL.md",
                body: DRIVE_SKILL_BODY,
            },
            SkillFile {
                path: ".claude/skills/supervise-agent/SKILL.md",
                body: SUPERVISE_SKILL_BODY,
            },
        ],
        _ => &[],
    }
}

/// The install state of one managed skill file.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum SkillState {
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
fn skill_state(content: &str, body: &str) -> SkillState {
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

/// What one skill-file install did — the shared per-file operation both callers
/// map onto their own vocabulary.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum SkillWrite {
    /// Already this build's content; nothing written.
    Current,
    /// The user's own file at our path; nothing written.
    Foreign,
    /// Written where no file existed.
    Installed,
    /// A stale (ours, older) file rewritten.
    Updated,
}

/// Write `bytes` to `path` so a reader sees the old file or the new one and
/// nothing in between: the bytes go to a sibling temp file
/// (`.<name>.aterm-tmp-<pid>`, same directory so the rename is one filesystem),
/// are fsync'd, and the temp file is renamed over the target. A plain
/// `fs::write` truncates first, so an agent loading its context file in the
/// window between truncate and write — or a crash there — read an empty or
/// half-written block; the primer's whole promise to that agent is the block,
/// so the write must be all-or-nothing. A failure anywhere leaves the target as
/// it was and removes the temp file. The target's existing mode survives the
/// swap of inodes (a user's `0600` context file must not come back `0644`).
///
/// Two things a context file is that a bare "temp file plus rename" gets wrong:
///
/// * It is often a SYMLINK into a dotfiles checkout. `rename(2)` replaces the
///   LINK, so the primed bytes would land in a fresh regular file at the link's
///   path while the file the agent actually loads — the link's target — kept the
///   old content forever. So the real target is resolved FIRST
///   ([`std::fs::canonicalize`]), which also puts the temp file beside the real
///   file and keeps the rename inside one directory. A path that does not
///   resolve is the fresh-install case: write where the caller asked.
/// * It is the user's WHOLE context file, not just our block. The temp file is
///   therefore born private ([`create_private_temp`]) and only widened to the
///   target's own mode once the bytes are down. Created at the process umask
///   instead — `0644` on a stock machine — every byte of it is world-readable
///   for the width of the write, and a file the primer creates stays that way.
fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let path = resolved.as_path();
    let name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a context file path needs a file name",
        )
    })?;
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(format!(".aterm-tmp-{}", std::process::id()));
    let tmp = dir.join(tmp_name);
    let written = (|| {
        let mut file = create_private_temp(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        // Widen only now, and only to what the file being replaced already
        // allowed: the temp is never wider than the target it becomes.
        if let Ok(meta) = std::fs::metadata(path)
            && meta.is_file()
        {
            file.set_permissions(meta.permissions())?;
        }
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        // Best effort: the error the caller sees is the write's, not the cleanup's.
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// The mode the temp file is born with on unix: the owner, nobody else. A
/// context file the primer CREATES keeps exactly this, because there is no
/// previous mode to restore.
#[cfg(unix)]
const TEMP_MODE: u32 = 0o600;

/// Create the sibling temp file [`write_atomically`] renames into place.
///
/// `create_new` so a file already sitting at the temp path is never opened:
/// `File::create` follows a symlink planted there and truncates whatever it
/// points at, which would write the user's context file wherever the link
/// aimed. The cost is that a temp file left behind by a KILLED process is not
/// reused: that write fails and the next pass — a different pid, so a different
/// temp name — succeeds. Priming is fail-soft and idempotent, so a refusal
/// costs one pass; truncating whatever sits at that path costs the file it
/// points at. On unix the file is private from its first instant — see
/// [`TEMP_MODE`].
fn create_private_temp(tmp: &Path) -> std::io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(TEMP_MODE);
    }
    opts.open(tmp)
}

/// Install or update one managed skill file. Never clobbers a `Foreign` file.
/// The write is atomic ([`write_atomically`]): a stale file is replaced whole or
/// not at all.
fn install_skill_file(path: &Path, body: &str) -> Result<SkillWrite, String> {
    let existing = std::fs::read_to_string(path).ok();
    let state = existing
        .as_deref()
        .map_or(SkillState::Absent, |c| skill_state(c, body));
    match state {
        SkillState::Current => Ok(SkillWrite::Current),
        SkillState::Foreign => Ok(SkillWrite::Foreign),
        SkillState::Absent | SkillState::Stale => {
            path.parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| write_atomically(path, body.as_bytes()))
                .map_err(|e| e.to_string())?;
            Ok(if state == SkillState::Stale {
                SkillWrite::Updated
            } else {
                SkillWrite::Installed
            })
        }
    }
}

/// One coding agent's global context file. `dir` doubles as the DETECTION signal:
/// its existence means the agent is installed/in use on this machine, so a bare
/// install may write there. Paths are `/`-separated segments under `$HOME`,
/// joined per-platform by [`home_join`].
struct AgentFile {
    /// The selector on the command line (`aterm agents install <name>`).
    name: &'static str,
    /// The product name for human-readable listings.
    product: &'static str,
    /// The agent's config dir under `$HOME` — existence ⇒ detected.
    dir: &'static str,
    /// The agent's ALWAYS-LOADED global context file under `$HOME`.
    file: &'static str,
    /// An agent-specific paragraph appended inside THIS agent's block only —
    /// for a runtime whose defaults defeat the generic brief. `None` for the
    /// generic three-line block.
    addendum: Option<&'static str>,
}

/// The registry of supported agents. Global-context-file conventions as of 2026:
/// each entry is the ONE file that agent loads in every project, which is what
/// makes the primer reach it regardless of cwd. Extend here to support another
/// agent — everything else (status/install/remove/usage/auto-prime) derives from
/// this table.
const AGENT_FILES: &[AgentFile] = &[
    AgentFile {
        name: "claude",
        product: "Claude Code",
        dir: ".claude",
        file: ".claude/CLAUDE.md",
        addendum: None,
    },
    AgentFile {
        name: "codex",
        product: "Codex CLI",
        dir: ".codex",
        file: ".codex/AGENTS.md",
        addendum: Some(CODEX_ADDENDUM),
    },
    AgentFile {
        name: "gemini",
        product: "Gemini CLI",
        dir: ".gemini",
        file: ".gemini/GEMINI.md",
        addendum: None,
    },
    AgentFile {
        name: "opencode",
        product: "OpenCode",
        dir: ".config/opencode",
        file: ".config/opencode/AGENTS.md",
        addendum: None,
    },
];

/// The block this agent's file should carry.
fn block_for(agent: &AgentFile) -> String {
    block_with(agent.addendum)
}

/// The install state of the managed block within one file's content.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum BlockState {
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

/// The block's install state within `content`, judged against `block` (the
/// agent's expected block). `Err` on a corrupt (unterminated) block, mirroring
/// [`find_block`].
fn block_state(content: &str, block: &str) -> Result<BlockState, String> {
    match find_block(content)? {
        None => Ok(BlockState::Absent),
        Some((start, end)) => {
            // Compare modulo the trailing newline: a block at EOF may lack one.
            if content[start..end].trim_end_matches('\n') == block.trim_end_matches('\n') {
                Ok(BlockState::Current)
            } else {
                Ok(BlockState::Stale)
            }
        }
    }
}

/// Insert `block` (append, blank-line separated) or replace a stale one in place.
/// `Ok(None)` when the content already carries exactly `block` — the idempotent
/// no-op. `Err` on a corrupt block, leaving the caller's file untouched.
fn upsert_block(content: &str, block: &str) -> Result<Option<String>, String> {
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
            out.push_str(block);
            Ok(Some(out))
        }
    }
}

/// Remove the managed block, collapsing the seam so a former
/// `content\n\n<block>` round-trips back to `content\n`. `Ok(None)` when no block
/// is present; `Err` on a corrupt block.
fn remove_block(content: &str) -> Result<Option<String>, String> {
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

/// What one primer-file upsert did — the shared per-file operation both callers
/// map onto their own vocabulary.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum PrimerWrite {
    /// The file already carried exactly this block; nothing written.
    Current,
    /// No context file existed; one was created holding only the block.
    Created,
    /// The file existed without a block; the block was appended.
    Appended,
    /// A stale block was rewritten in place.
    Replaced,
}

/// Upsert `block` into the context file at `path`, creating the file (and its
/// parent) when absent. Whole-file writes only: the transform runs on the full
/// content and the result lands through [`write_atomically`] — a temp file
/// renamed over the target — so a reader or a crash at any instant sees either
/// the old file or the new one, never a torn block. (A truncate-then-write
/// promised the same and could not keep it: the file was empty between the two.)
fn upsert_primer_file(path: &Path, block: &str) -> Result<PrimerWrite, String> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            path.parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| write_atomically(path, block.as_bytes()))
                .map_err(|e| e.to_string())?;
            return Ok(PrimerWrite::Created);
        }
        Err(e) => return Err(format!("unreadable: {e}")),
    };
    let had_block = find_block(&content)?.is_some();
    match upsert_block(&content, block)? {
        None => Ok(PrimerWrite::Current),
        Some(updated) => {
            write_atomically(path, updated.as_bytes()).map_err(|e| e.to_string())?;
            Ok(if had_block {
                PrimerWrite::Replaced
            } else {
                PrimerWrite::Appended
            })
        }
    }
}

/// The user's home directory, from the platform's canonical env var. `None` (an
/// unset/empty var) makes every caller fail with a clear message rather than
/// writing relative to an arbitrary cwd. Deliberately NOT
/// `aterm_types::dirs::home_dir`: that one accepts an empty `$HOME` and falls
/// back to `/etc/passwd`, either of which would defeat the fail-fast here.
#[must_use]
pub fn home_dir() -> Option<PathBuf> {
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

/// Whether the agent's config dir exists — the one detection signal.
fn detected(home: &Path, agent: &AgentFile) -> bool {
    home_join(home, agent.dir).is_dir()
}

/// One agent's on-disk situation, resolved read-only for `status` and
/// [`status_line`].
enum FileSituation {
    /// The config dir does not exist — the agent is not in use on this machine.
    NotDetected,
    /// Dir exists, context file does not.
    NoFile,
    /// File exists; the block's state within it (or a corrupt-block message).
    File(Result<BlockState, String>),
}

fn situation(home: &Path, agent: &AgentFile) -> FileSituation {
    if !detected(home, agent) {
        return FileSituation::NotDetected;
    }
    let path = home_join(home, agent.file);
    match std::fs::read_to_string(&path) {
        Ok(content) => FileSituation::File(block_state(&content, &block_for(agent))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => FileSituation::NoFile,
        Err(e) => FileSituation::File(Err(format!("unreadable: {e}"))),
    }
}

/// The read-only state of one skill file, in the status vocabulary.
fn skill_status(home: &Path, s: &SkillFile) -> SkillState {
    match std::fs::read_to_string(home_join(home, s.path)) {
        Err(_) => SkillState::Absent,
        Ok(c) => skill_state(&c, s.body),
    }
}

// ---------------------------------------------------------------------------
// auto-prime: the GUI's entry point
// ---------------------------------------------------------------------------

/// What [`auto_prime`] did for one detected agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Everything of ours was already current; nothing written.
    Unchanged,
    /// The primer block was absent (or the file was) and is now installed —
    /// this agent is primed for the first time.
    Installed,
    /// Something of ours was stale and rewritten in place (a v1 block, an older
    /// skill), or a skill was added beside an already-current block.
    Updated,
    /// The only thing left to do was a skill whose file is the user's own
    /// (no aterm marker); it was left alone. Reported every time, never logged
    /// as a change.
    SkippedForeign,
    /// An I/O failure or a corrupt (unterminated) block; the message names it.
    /// The file is left as it was.
    Error(String),
}

/// One detected agent's row in an [`AutoPrime`] result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentOutcome {
    /// The registry selector (`claude`, `codex`, ...).
    pub agent: &'static str,
    /// The product name (`Claude Code`, ...).
    pub product: &'static str,
    /// What happened.
    pub outcome: Outcome,
}

/// The result of one [`auto_prime`] pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoPrime {
    /// One row per DETECTED agent, in registry order; empty when no agent's
    /// config dir exists.
    pub agents: Vec<AgentOutcome>,
    /// The one-line human summary (`agent primer: claude installed, codex
    /// unchanged`), ready for a log line.
    pub summary: String,
}

impl AutoPrime {
    /// Whether anything was written — the condition for an `info` log line.
    #[must_use]
    pub fn changed(&self) -> bool {
        self.agents
            .iter()
            .any(|a| matches!(a.outcome, Outcome::Installed | Outcome::Updated))
    }

    /// The rows that failed, for `warn` lines.
    pub fn errors(&self) -> impl Iterator<Item = (&'static str, &str)> {
        self.agents.iter().filter_map(|a| match &a.outcome {
            Outcome::Error(msg) => Some((a.agent, msg.as_str())),
            _ => None,
        })
    }
}

/// Fold one agent's primer write and its skill writes into the agent's outcome.
/// `Installed` means the PRIMER was new (the agent is primed for the first
/// time); any other write is an update; a foreign skill only shows when nothing
/// else happened; an error wins outright.
fn fold_outcome(
    primer: Result<PrimerWrite, String>,
    skills: &[Result<SkillWrite, String>],
) -> Outcome {
    let primer = match primer {
        Ok(p) => p,
        Err(e) => return Outcome::Error(e),
    };
    if let Some(e) = skills.iter().find_map(|s| s.as_ref().err()) {
        return Outcome::Error(e.clone());
    }
    if matches!(primer, PrimerWrite::Created | PrimerWrite::Appended) {
        return Outcome::Installed;
    }
    let skill_written = skills
        .iter()
        .any(|s| matches!(s, Ok(SkillWrite::Installed | SkillWrite::Updated)));
    if primer == PrimerWrite::Replaced || skill_written {
        return Outcome::Updated;
    }
    if skills.iter().any(|s| matches!(s, Ok(SkillWrite::Foreign))) {
        return Outcome::SkippedForeign;
    }
    Outcome::Unchanged
}

/// The summary word for one outcome, as it reads in the one-line summary.
fn outcome_word(o: &Outcome) -> String {
    match o {
        Outcome::Unchanged => "unchanged".to_string(),
        Outcome::Installed => "installed".to_string(),
        Outcome::Updated => "updated".to_string(),
        Outcome::SkippedForeign => "unchanged (a skill file is yours, left alone)".to_string(),
        Outcome::Error(e) => format!("ERROR: {e}"),
    }
}

/// Install or update the primer (and the bundled skills) for every DETECTED
/// agent under `home` — what aterm itself runs when it opens a session.
///
/// * Detection only: an agent's config dir is never created, so a machine
///   without Codex never grows a `~/.codex`.
/// * Idempotent: a second call over the same tree reports every row
///   `Unchanged` (or `SkippedForeign`, which also writes nothing).
/// * Fail-soft: no panics; an I/O error or a corrupt block becomes that row's
///   [`Outcome::Error`] and the other agents still proceed. Every write is a
///   whole-file atomic replace ([`write_atomically`]), so nothing is ever left
///   half-edited — not even for the instant of the write.
#[must_use]
pub fn auto_prime(home: &Path) -> AutoPrime {
    let mut agents = Vec::new();
    for a in AGENT_FILES.iter().filter(|a| detected(home, a)) {
        let primer = upsert_primer_file(&home_join(home, a.file), &block_for(a));
        let skills: Vec<Result<SkillWrite, String>> = skills_for(a.name)
            .iter()
            .map(|s| install_skill_file(&home_join(home, s.path), s.body))
            .collect();
        agents.push(AgentOutcome {
            agent: a.name,
            product: a.product,
            outcome: fold_outcome(primer, &skills),
        });
    }
    let summary = if agents.is_empty() {
        let looked: Vec<String> = AGENT_FILES.iter().map(|a| format!("~/{}", a.dir)).collect();
        format!(
            "agent primer: no coding agents detected (looked for {})",
            looked.join(", ")
        )
    } else {
        let rows: Vec<String> = agents
            .iter()
            .map(|a| format!("{} {}", a.agent, outcome_word(&a.outcome)))
            .collect();
        format!("agent primer: {}", rows.join(", "))
    };
    AutoPrime { agents, summary }
}

/// One line for `aterm --diagnose`: every registry agent's primer state (and any
/// skill of a primed agent that is not current), e.g.
/// `claude installed, codex stale, gemini not detected, opencode not detected`.
#[must_use]
pub fn status_line(home: &Path) -> String {
    let rows: Vec<String> = AGENT_FILES
        .iter()
        .map(|a| {
            let state = match situation(home, a) {
                FileSituation::NotDetected => "not detected".to_string(),
                FileSituation::NoFile | FileSituation::File(Ok(BlockState::Absent)) => {
                    "absent".to_string()
                }
                FileSituation::File(Ok(BlockState::Current)) => "installed".to_string(),
                FileSituation::File(Ok(BlockState::Stale)) => "stale".to_string(),
                FileSituation::File(Err(e)) => format!("ERROR: {e}"),
            };
            let mut row = format!("{} {state}", a.name);
            if detected(home, a) {
                for s in skills_for(a.name) {
                    let word = match skill_status(home, s) {
                        SkillState::Current => continue,
                        SkillState::Absent => "absent",
                        SkillState::Stale => "stale",
                        SkillState::Foreign => "yours, left alone",
                    };
                    let name = s.path.rsplit('/').nth(1).unwrap_or(s.path);
                    let _ = write!(row, " (skill {name} {word})");
                }
            }
            row
        })
        .collect();
    rows.join(", ")
}

// ---------------------------------------------------------------------------
// `aterm agents`: the CLI's entry point
// ---------------------------------------------------------------------------

/// The usage text for `aterm agents` (printed on an unknown subcommand/agent).
fn usage() -> String {
    let mut s = String::from(
        "usage: aterm agents [status | install [<agent>…] | remove [<agent>…] | primer [<agent>]]\n\
         \n\
         Manage the 3-line aterm primer in coding agents' global context files, so any\n\
         agent launched inside aterm knows what aterm is and to run `aterm help`. aterm\n\
         also does this itself for every detected agent each time it opens a session.\n\
         \n\
           status    each agent's context file and whether the primer is installed (default)\n\
           install   install/update the primer for every detected agent (config dir exists);\n\
         \x20           name agents to force them (creates the file if needed)\n\
         \x20 remove    remove the primer block (everywhere, or from the named agents)\n\
         \x20 primer    print the block itself — paste it into any AGENTS.md/CLAUDE.md;\n\
         \x20           name an agent to include its agent-specific addendum (codex: sandbox)\n\
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
#[must_use]
pub fn agents_report(home: &Path, args: &[String]) -> (String, i32) {
    let sub = args.first().map(String::as_str).unwrap_or("status");
    let names = args.get(1..).unwrap_or(&[]);
    match sub {
        "primer" => {
            // At most one agent: the block is one agent's, not a concatenation.
            if names.len() > 1 {
                return (
                    format!(
                        "aterm agents: primer takes at most one agent\n\n{}",
                        usage()
                    ),
                    2,
                );
            }
            match select(names) {
                Ok(_) => (primer_block(names.first().map(String::as_str)), 0),
                Err(msg) => (msg, 2),
            }
        }
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
                    let st = match skill_status(home, s) {
                        SkillState::Current => "installed",
                        SkillState::Stale => "stale (install updates it)",
                        SkillState::Foreign => "foreign — yours, left alone",
                        SkillState::Absent => "absent",
                    };
                    let _ = writeln!(out, "{:<9} ~/{:<28} {st}", "  skill", s.path);
                }
            }
            out.push_str(
                "\n`aterm agents install` installs/updates the primer AND the bundled skills\n\
                 for detected agents; `aterm agents primer` prints the block for manual pasting.\n",
            );
            let _ = writeln!(out, "{AUTO_PRIME_NOTE}");
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
                let undetected = !detected(home, a);
                if undetected && !forced {
                    let _ = writeln!(
                        out,
                        "{:<9} ~/{:<28} skipped — no ~/{} (not detected; name it to force)",
                        a.name, a.file, a.dir
                    );
                    // A skipped (undetected, unforced) agent skips its skills as
                    // well — never create `~/.claude` for someone who does not
                    // use Claude Code.
                    continue;
                }
                let verdict = match upsert_primer_file(&home_join(home, a.file), &block_for(a)) {
                    Ok(PrimerWrite::Current) => "already installed".to_string(),
                    Ok(PrimerWrite::Created) => "installed (new file)".to_string(),
                    Ok(PrimerWrite::Appended | PrimerWrite::Replaced) => "installed".to_string(),
                    Err(e) => {
                        failed = true;
                        format!("ERROR: {e}")
                    }
                };
                let _ = writeln!(out, "{:<9} ~/{:<28} {verdict}", a.name, a.file);

                // Bundled skills ride the SAME install: an agent that gets the
                // primer gets the skills too.
                for s in skills_for(a.name) {
                    let verdict = match install_skill_file(&home_join(home, s.path), s.body) {
                        Ok(SkillWrite::Current) => "already installed".to_string(),
                        // The user's own file at our path: never clobbered.
                        Ok(SkillWrite::Foreign) => {
                            "skipped — not an aterm-managed file (yours)".to_string()
                        }
                        Ok(SkillWrite::Updated) => "updated".to_string(),
                        Ok(SkillWrite::Installed) => "installed".to_string(),
                        Err(e) => {
                            failed = true;
                            format!("ERROR: {e}")
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
                        Ok(Some(rest)) => match write_atomically(&path, rest.as_bytes()) {
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
            // A removal the next session would silently undo is a trap; the
            // knob that makes it stick rides on the same screen.
            let _ = writeln!(out, "\n{AUTO_PRIME_NOTE}");
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

    /// The exact v1 block a machine primed before 2026-08-26 carries — pinned as
    /// a literal (not built from today's constants) so the upgrade path is tested
    /// against what is really on disk out there.
    const V1_BLOCK: &str = "<!-- aterm primer v1 — managed by `aterm agents`; `aterm agents remove` uninstalls -->\n\
        ## aterm\n\
        If the environment has `TERM_PROGRAM=aterm` or `ATERM_CHILD=1`, this terminal is aterm.\n\
        <!-- /aterm primer -->\n";

    /// The exact v2 GENERIC block the 2026-08-26 auto-prime wrote — pinned as a
    /// literal so the v2 → v3 upgrade is tested against what is on disk, not
    /// against today's constants.
    const V2_BLOCK: &str = r"<!-- aterm primer v2 — managed by `aterm agents`; `aterm agents remove` uninstalls -->
## aterm
If the environment has `TERM_PROGRAM=aterm` or `ATERM_CHILD=1`, this terminal is aterm — an
AI-native terminal whose sessions are introspectable and drivable: agents and humans can read
the live screen, send input, and await real transitions, concurrently. Run `aterm help` for the
agent operating brief and `aterm help introspection` for the `aterm ctl` control verbs (`aterm`
is already on PATH inside aterm sessions). aterm deliberately STRIPS `CLAUDE*`, `ANTHROPIC_*`,
`COPILOT_*`, `CODEX_*`, `CURSOR_*`, and `AI_*` env vars from the shells it spawns — `aterm help`
explains why. If neither variable is set, you are not inside aterm; ignore this section.
<!-- /aterm primer -->
";

    fn generic() -> String {
        primer_block(None)
    }

    #[test]
    fn primer_is_detection_manual_first_moves_and_hygiene() {
        let block = generic();
        // Detection: both identity vars an agent can check.
        assert!(block.contains("TERM_PROGRAM=aterm") && block.contains("ATERM_CHILD"));
        // The pointer that unlocks the full brief.
        assert!(block.contains("`aterm help`"));
        // The first moves (F2) and the one rule before touching a peer (F5).
        assert!(block.contains("`aterm ctl windows`") && block.contains("`aterm ctl ls`"));
        assert!(block.contains("read a peer's `status`"));
        assert!(block.contains("detail="));
        assert!(block.contains("before typing into it"));
        // Self-gating: safe in a global file loaded in every terminal.
        assert!(block.contains("ignore this section"));
        // Marked + versioned, so installs are idempotent and updatable.
        assert!(block.starts_with(MARK_BEGIN) && block.trim_end().ends_with(MARK_END));
        assert!(
            MARK_BEGIN.contains(" v4 "),
            "the Codex escalation sentence bumped the version"
        );
    }

    /// The body is a POINTER, not the manual. A sentence that earns its place in
    /// every agent's context file in every project is rare; the budget makes the
    /// next author argue for one rather than drift the block into a page.
    #[test]
    fn primer_body_stays_within_its_byte_budget() {
        assert!(
            PRIMER_BODY.len() <= 1_000,
            "primer body is {} bytes — depth belongs behind `aterm help`",
            PRIMER_BODY.len()
        );
        assert!(
            PRIMER_BODY.lines().count() <= 12,
            "{}",
            PRIMER_BODY.lines().count()
        );
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

    /// Only Codex carries the sandbox paragraph; every other agent's block is the
    /// generic one, byte-identical to `primer_block(None)`.
    #[test]
    fn only_codex_carries_the_sandbox_addendum() {
        let codex = primer_block(Some("codex"));
        assert!(codex.starts_with(MARK_BEGIN) && codex.ends_with(&format!("{MARK_END}\n")));
        assert!(
            codex.contains(PRIMER_BODY),
            "the addendum ADDS to the brief"
        );
        assert!(codex.contains("--allow-unix-socket \"$HOME/Library/Application Support/aterm\""));
        assert!(codex.contains("Operation not permitted (os error 1)"));
        // The escalation advice is a rule about what a verb DOES, never an
        // allow-list of write verbs: v3's "read-only unless you use
        // send/turn/key/spawn/close" was false the day it shipped (paste, feed,
        // ctrl, mouse, signal, meta set, settings, tab, resize all write) and
        // every new verb would have widened the lie.
        assert!(codex.contains("prefer escalating read verbs one at a time"));
        assert!(codex.contains("(ls, windows, sessions, status, text, blocks)"));
        assert!(codex.contains(
            "anything that types, signals, spawns, closes\nor changes settings is a write"
        ));
        assert!(!codex.contains("read-only"), "no blanket read-only claim");
        assert!(
            !codex.contains("unless you use"),
            "no allow-list of write verbs"
        );
        // The verbs it names as reads carry no write class in the one verb table
        // (`sessions` is the owner-only roster, class `Owner`, and it writes
        // nothing; the two it names that the table lacks, ls and windows, are
        // aterm-ctl's own client-answered listings, which dial `sessions`) —
        // and the verbs v3's allow-list left out really are writes there.
        use aterm_types::control_verbs::{OpClass, spec};
        let writes = |op: OpClass| {
            matches!(
                op,
                OpClass::Write | OpClass::Signal | OpClass::ConfigWrite | OpClass::ClipboardWrite
            )
        };
        for verb in ["sessions", "status", "text", "blocks"] {
            let s = spec(verb).unwrap_or_else(|| panic!("{verb} is a table verb"));
            assert!(
                !writes(s.op),
                "{verb} must not be a write verb to be named as a read ({:?})",
                s.op
            );
        }
        for verb in ["ls", "windows"] {
            assert!(spec(verb).is_none());
        }
        for verb in [
            "paste", "feed", "signal", "settings", "tab", "resize", "spawn", "close",
        ] {
            let s = spec(verb).unwrap_or_else(|| panic!("{verb} is a table verb"));
            assert!(
                writes(s.op),
                "{verb} writes ({:?}) — the v3 allow-list missed it",
                s.op
            );
        }
        // The config spelling was measured and did NOT work; the block says so
        // rather than recommending a line nobody has seen take effect, and it
        // tells the agent what a refused `ls` now looks like (exit 2 + hint).
        assert!(codex.contains("`[network] allow_unix_sockets`"));
        assert!(codex.contains("did not take\neffect under `codex sandbox`"));
        assert!(codex.contains("exit 2"));
        for a in AGENT_FILES.iter().filter(|a| a.name != "codex") {
            assert_eq!(
                primer_block(Some(a.name)),
                generic(),
                "{} must get the generic block",
                a.name
            );
        }
        // An unknown selector never invents an addendum.
        assert_eq!(primer_block(Some("copilot")), generic());
        assert!(!generic().contains("sandbox"));
    }

    #[test]
    fn upsert_appends_once_and_is_idempotent() {
        let block = generic();
        let v1 = upsert_block("", &block).unwrap().unwrap();
        assert_eq!(v1, block);
        assert_eq!(
            upsert_block(&v1, &block).unwrap(),
            None,
            "second install is a no-op"
        );

        let with_user = upsert_block("# My rules\n\nBe nice.\n", &block)
            .unwrap()
            .unwrap();
        assert!(with_user.starts_with("# My rules\n\nBe nice.\n\n<!-- aterm primer"));
        assert_eq!(upsert_block(&with_user, &block).unwrap(), None);
        assert_eq!(
            block_state(&with_user, &block).unwrap(),
            BlockState::Current
        );
    }

    /// A machine primed by the v1 installer: the block is found by its prefix,
    /// reported stale, and rewritten IN PLACE (never duplicated, never moved),
    /// with the user's text on both sides intact.
    #[test]
    fn v1_block_is_stale_and_updates_in_place_preserving_surroundings() {
        let block = generic();
        let old = format!("before\n\n{V1_BLOCK}\nafter\n");
        assert_eq!(block_state(&old, &block).unwrap(), BlockState::Stale);
        let updated = upsert_block(&old, &block).unwrap().unwrap();
        assert!(updated.starts_with("before\n\n<!-- aterm primer v4"));
        assert!(updated.ends_with("<!-- /aterm primer -->\n\nafter\n"));
        assert_eq!(
            updated.matches(MARK_PREFIX).count(),
            1,
            "one begin marker: never a duplicate block"
        );
        assert_eq!(updated.matches(MARK_END).count(), 1, "one end marker");
        assert_eq!(block_state(&updated, &block).unwrap(), BlockState::Current);
        // The same content judged against ANOTHER agent's block is stale: the
        // Codex file must carry the Codex block, not the generic one.
        let codex = primer_block(Some("codex"));
        assert_eq!(block_state(&updated, &codex).unwrap(), BlockState::Stale);
    }

    /// A machine the 2026-08-26 auto-prime already primed (every such machine, a
    /// day later): the v2 block is found by its prefix, reported stale, rewritten
    /// in place with the user's text intact, and a Codex file gets the v3 Codex
    /// block — not the generic one.
    #[test]
    fn v2_block_is_stale_and_upgrades_in_place_for_every_agent() {
        let old = format!("mine\n\n{V2_BLOCK}\ntheirs\n");
        for a in AGENT_FILES {
            let block = block_for(a);
            assert_eq!(
                block_state(&old, &block).unwrap(),
                BlockState::Stale,
                "{}: a v2 block must read stale",
                a.name
            );
            let updated = upsert_block(&old, &block).unwrap().unwrap();
            assert!(
                updated.starts_with("mine\n\n<!-- aterm primer v4"),
                "{}",
                a.name
            );
            assert!(
                updated.ends_with("<!-- /aterm primer -->\n\ntheirs\n"),
                "{}",
                a.name
            );
            assert_eq!(updated.matches(MARK_PREFIX).count(), 1, "{}", a.name);
            assert_eq!(updated.matches(MARK_END).count(), 1, "{}", a.name);
            assert!(updated.contains("`aterm ctl windows`"), "{}", a.name);
            assert_eq!(
                block_state(&updated, &block).unwrap(),
                BlockState::Current,
                "{}",
                a.name
            );
        }
        // The v2 literal really is the v2 shape: same markers, no first-moves line.
        assert!(V2_BLOCK.starts_with("<!-- aterm primer v2 "));
        assert!(!V2_BLOCK.contains("First moves"));
    }

    /// The exact v3 GENERIC block the 2026-08-27 auto-prime wrote — pinned as a
    /// literal so the v3 → v4 upgrade is tested against what is on disk, not
    /// against today's constants. (The generic BODY did not change between v3
    /// and v4 — only the Codex addendum did — so the marker alone makes it stale.)
    const V3_BLOCK: &str = r"<!-- aterm primer v3 — managed by `aterm agents`; `aterm agents remove` uninstalls -->
## aterm
If the environment has `TERM_PROGRAM=aterm` or `ATERM_CHILD=1`, this terminal is aterm — an
AI-native terminal whose sessions are introspectable and drivable: agents and humans can read
the live screen, send input, and await real transitions, concurrently. Run `aterm help` for the
agent operating brief and `aterm help introspection` for the `aterm ctl` control verbs (`aterm`
is already on PATH inside aterm sessions). First moves: `aterm ctl windows` and `aterm ctl ls`
list every window and session; read a peer's `status` (detail= names the program it is running)
before typing into it. aterm deliberately STRIPS `CLAUDE*`, `ANTHROPIC_*`, `COPILOT_*`,
`CODEX_*`, `CURSOR_*`, and `AI_*` env vars from the shells it spawns — `aterm help` explains
why. If neither variable is set, you are not inside aterm; ignore this section.
<!-- /aterm primer -->
";

    /// A machine the 2026-08-27 auto-prime primed to v3: stale by its marker,
    /// rewritten in place for every agent, and the Codex file gets the corrected
    /// escalation sentence.
    #[test]
    fn v3_block_is_stale_and_upgrades_in_place_for_every_agent() {
        let old = format!("mine\n\n{V3_BLOCK}\ntheirs\n");
        assert!(V3_BLOCK.starts_with("<!-- aterm primer v3 "));
        assert!(V3_BLOCK.contains("First moves"));
        for a in AGENT_FILES {
            let block = block_for(a);
            assert_eq!(
                block_state(&old, &block).unwrap(),
                BlockState::Stale,
                "{}: a v3 block must read stale",
                a.name
            );
            let updated = upsert_block(&old, &block).unwrap().unwrap();
            assert!(
                updated.starts_with("mine\n\n<!-- aterm primer v4"),
                "{}",
                a.name
            );
            assert!(
                updated.ends_with("<!-- /aterm primer -->\n\ntheirs\n"),
                "{}",
                a.name
            );
            assert_eq!(updated.matches(MARK_PREFIX).count(), 1, "{}", a.name);
            assert_eq!(
                updated.contains("prefer escalating read verbs"),
                a.name == "codex",
                "{}",
                a.name
            );
        }
    }

    // ---- atomic writes ---------------------------------------------------

    /// The primer's promise is "the old file or the new one, never a torn block":
    /// a write that cannot complete — here the rename target is a DIRECTORY, so
    /// `rename(2)` refuses — leaves the original bytes exactly where they were and
    /// no temp file behind. Then the happy path: the swap keeps the file's mode.
    #[test]
    fn a_failed_atomic_write_leaves_the_original_intact() {
        let home = aterm_tempfile::tempdir().unwrap();
        let dir = home.path().join("d");
        std::fs::create_dir_all(&dir).unwrap();

        // A directory sits where the file should go; its contents are "the
        // original" a torn write would have damaged.
        let target = dir.join("CLAUDE.md");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("keep.txt"), "keep").unwrap();
        let err = write_atomically(&target, b"new").expect_err("a directory in the way fails");
        assert!(!err.to_string().is_empty());
        assert!(target.is_dir(), "the directory is still there");
        assert_eq!(
            std::fs::read_to_string(target.join("keep.txt")).unwrap(),
            "keep"
        );
        let litter: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("aterm-tmp"))
            .collect();
        assert!(litter.is_empty(), "no temp file left behind: {litter:?}");

        // The same failure through the two helpers: the skill installer reads
        // nothing (a directory is not a file), treats the path as absent, and
        // its write fails without touching what is there; the primer upsert
        // refuses at the read.
        let skill = dir.join("SKILL.md");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("keep.txt"), "keep").unwrap();
        assert!(install_skill_file(&skill, DRIVE_SKILL_BODY).is_err());
        assert_eq!(
            std::fs::read_to_string(skill.join("keep.txt")).unwrap(),
            "keep"
        );
        assert!(upsert_primer_file(&target, &generic()).is_err());
        assert_eq!(
            std::fs::read_to_string(target.join("keep.txt")).unwrap(),
            "keep"
        );

        // A temp-file collision (a directory at the temp path) fails the write
        // and the original file is byte-identical afterwards.
        let file = dir.join("AGENTS.md");
        std::fs::write(&file, "original\n").unwrap();
        let tmp = dir.join(format!(".AGENTS.md.aterm-tmp-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        assert!(write_atomically(&file, b"replaced\n").is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "original\n");
        std::fs::remove_dir(&tmp).unwrap();

        // Happy path: the file is replaced whole, and its mode survives the
        // inode swap (a private context file stays private).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        write_atomically(&file, b"replaced\n").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "replaced\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                0o600,
                "the target's mode survives the swap"
            );
        }
        assert!(
            !dir.join(format!(".AGENTS.md.aterm-tmp-{}", std::process::id()))
                .exists(),
            "the temp file was renamed away"
        );
    }

    /// A context file is very often a SYMLINK into a dotfiles checkout. Renaming
    /// the temp file over the link replaces the LINK with a regular file: the
    /// primer would look installed at `~/.claude/CLAUDE.md` while the file the
    /// agent actually loads — the link's target — never sees the block, and the
    /// user's dotfiles repo silently stops being the source of that file. So the
    /// write resolves the link first and lands on the real file.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_context_file_is_written_through_to_its_target() {
        let home = aterm_tempfile::tempdir().unwrap();
        let store = home.path().join("dotfiles");
        let cfg = home.path().join(".claude");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::create_dir_all(&cfg).unwrap();
        let real = store.join("claude-context.md");
        std::fs::write(&real, "# mine\n").unwrap();
        let link = cfg.join("CLAUDE.md");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        write_atomically(&link, b"primed\n").unwrap();
        assert!(
            std::fs::symlink_metadata(&link).unwrap().is_symlink(),
            "the link is still a link, not a regular file that shadows it"
        );
        assert_eq!(
            std::fs::read_to_string(&real).unwrap(),
            "primed\n",
            "the file the agent loads is the one that changed"
        );
        // The swap happens beside the REAL file — one directory, one rename —
        // and leaves nothing behind in either directory.
        for d in [&store, &cfg] {
            let litter: Vec<String> = std::fs::read_dir(d)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|n| n.contains("aterm-tmp"))
                .collect();
            assert!(litter.is_empty(), "no temp file left in {d:?}: {litter:?}");
        }

        // The same through the installer an `aterm agents install` runs: the
        // block reaches the real file, and the link survives.
        std::fs::write(&real, "# mine\n").unwrap();
        upsert_primer_file(&link, &generic()).unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
        let primed = std::fs::read_to_string(&real).unwrap();
        assert!(primed.starts_with("# mine\n"), "{primed}");
        assert!(primed.contains(MARK_PREFIX), "{primed}");
    }

    /// The temp file holds the user's WHOLE context file — every private note
    /// outside our markers included — for the width of the write. Created with
    /// `File::create` it takes the process umask, so on a stock machine that
    /// file, and any context file the primer creates, is `0644`: world-readable.
    /// It is born `0600` instead and only ever widened to the mode of the file
    /// it replaces, so it is never wider than the target it becomes.
    #[cfg(unix)]
    #[test]
    fn the_temp_file_is_never_wider_than_the_context_file_it_becomes() {
        use std::os::unix::fs::PermissionsExt as _;
        let home = aterm_tempfile::tempdir().unwrap();
        let dir = home.path().join("d");
        std::fs::create_dir_all(&dir).unwrap();

        // The temp file at the instant it exists, whatever the umask is.
        let tmp = dir.join(".CLAUDE.md.aterm-tmp-probe");
        drop(create_private_temp(&tmp).unwrap());
        let mode = std::fs::metadata(&tmp).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, TEMP_MODE, "the temp file is born private");
        assert_eq!(mode & 0o077, 0, "no group or other bits to widen from");
        // And a file already at that path is never opened — so a symlink planted
        // there is never followed and truncated with the context file's bytes.
        assert_eq!(
            create_private_temp(&tmp).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        std::fs::remove_file(&tmp).unwrap();

        // A context file the primer CREATES keeps that mode: the rename carries
        // the temp file's inode, so the umask never gets a say.
        let fresh = dir.join("CLAUDE.md");
        write_atomically(&fresh, b"primed\n").unwrap();
        assert_eq!(
            std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777,
            TEMP_MODE,
            "a new context file is private"
        );

        // Over an EXISTING file the target's own mode is restored, and only
        // after the bytes are down: the widening never precedes the write.
        let existing = dir.join("AGENTS.md");
        std::fs::write(&existing, "old\n").unwrap();
        std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_atomically(&existing, b"primed\n").unwrap();
        assert_eq!(
            std::fs::metadata(&existing).unwrap().permissions().mode() & 0o777,
            0o644,
            "the file's own mode comes back"
        );
        assert_eq!(std::fs::read_to_string(&existing).unwrap(), "primed\n");
    }

    #[test]
    fn remove_round_trips_to_the_original() {
        let block = generic();
        let original = "# My rules\n\nBe nice.\n";
        let installed = upsert_block(original, &block).unwrap().unwrap();
        let removed = remove_block(&installed).unwrap().unwrap();
        assert_eq!(removed, original);
        // A file that was only the block empties out entirely.
        assert_eq!(remove_block(&block).unwrap().unwrap(), "");
        // Nothing installed → nothing to remove.
        assert_eq!(remove_block(original).unwrap(), None);
    }

    #[test]
    fn unterminated_block_fails_closed() {
        let block = generic();
        let corrupt = "x\n<!-- aterm primer v1 -->\nno end marker\n";
        assert!(block_state(corrupt, &block).is_err());
        assert!(upsert_block(corrupt, &block).is_err());
        assert!(remove_block(corrupt).is_err());
        // A stray END marker alone is user content, not a block.
        let stray = format!("x\n{MARK_END}\ny\n");
        assert_eq!(block_state(&stray, &block).unwrap(), BlockState::Absent);
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
        assert_eq!(claude, generic());
        assert!(
            !home.path().join(".codex").exists(),
            "a bare install must not create an undetected agent's dir"
        );

        // Forcing by name creates the file (and dir) for an undetected agent —
        // with THAT agent's block (Codex gets its addendum).
        let (out, code) = agents_report(home.path(), &["install".to_string(), "codex".to_string()]);
        assert_eq!(code, 0, "{out}");
        let codex = std::fs::read_to_string(home.path().join(".codex/AGENTS.md")).unwrap();
        assert_eq!(codex, primer_block(Some("codex")));

        // A second install over both is a no-op.
        let (out, code) = agents_report(home.path(), &["install".to_string()]);
        assert_eq!(code, 0, "{out}");
        // Two primers (claude + codex) plus every bundled Claude skill —
        // counted from the registry, never typed, so a new skill does not make
        // this a false failure.
        assert_eq!(
            out.matches("already installed").count(),
            2 + skills_for("claude").len(),
            "primer x2 + every claude skill:\n{out}"
        );
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
        std::fs::write(home.path().join(".claude/CLAUDE.md"), generic()).unwrap();
        let (out, code) = agents_report(home.path(), &[]);
        assert_eq!(code, 0);
        for a in AGENT_FILES {
            assert!(out.contains(a.name), "status must list {}", a.name);
        }
        assert!(out.contains("installed"));
        assert!(out.contains("not detected"));
    }

    /// The knob that stops auto-prime must be on the two screens where a user
    /// could otherwise be surprised: status (discoverability) and remove (a
    /// removal the next session would silently undo).
    #[test]
    fn status_and_remove_name_the_auto_prime_knob() {
        let home = aterm_tempfile::tempdir().unwrap();
        let (status, _) = agents_report(home.path(), &[]);
        assert!(status.contains(AUTO_PRIME_NOTE), "status footer:\n{status}");
        assert!(status.contains("agents_auto_prime = false"));
        let (removed, code) = agents_report(home.path(), &["remove".to_string()]);
        assert_eq!(code, 0);
        assert!(
            removed.trim_end().ends_with(AUTO_PRIME_NOTE.trim_end()),
            "remove must END with the knob sentence:\n{removed}"
        );
        // `install` does not nag: the sentence belongs where it prevents a surprise.
        let (installed, _) = agents_report(home.path(), &["install".to_string()]);
        assert!(!installed.contains("agents_auto_prime"));
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
        assert_eq!(out, generic());
        // Naming an agent prints ITS block; an unknown name is a usage error, and
        // so is more than one (the block is one agent's, not a concatenation).
        let (out, code) = agents_report(home.path(), &["primer".to_string(), "codex".to_string()]);
        assert_eq!(code, 0);
        assert_eq!(out, primer_block(Some("codex")));
        let (_, code) = agents_report(home.path(), &["primer".to_string(), "copilot".to_string()]);
        assert_eq!(code, 2);
        let (_, code) = agents_report(
            home.path(),
            &[
                "primer".to_string(),
                "claude".to_string(),
                "codex".to_string(),
            ],
        );
        assert_eq!(code, 2);
    }

    // ---- auto-prime ---------------------------------------------------------

    /// The GUI's pass: detected agents get primed (skills included), undetected
    /// agents get no directory, and the second pass changes nothing.
    #[test]
    fn auto_prime_primes_detected_agents_only_and_is_idempotent() {
        let home = aterm_tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".claude")).unwrap();
        std::fs::create_dir_all(home.path().join(".codex")).unwrap();

        let first = auto_prime(home.path());
        assert_eq!(first.agents.len(), 2, "{}", first.summary);
        assert!(
            first.agents.iter().all(|a| a.outcome == Outcome::Installed),
            "{:?}",
            first.agents
        );
        assert!(first.changed());
        assert_eq!(first.errors().count(), 0);
        assert_eq!(
            first.summary,
            "agent primer: claude installed, codex installed"
        );

        assert_eq!(
            std::fs::read_to_string(home.path().join(".claude/CLAUDE.md")).unwrap(),
            generic()
        );
        assert_eq!(
            std::fs::read_to_string(home.path().join(".codex/AGENTS.md")).unwrap(),
            primer_block(Some("codex"))
        );
        assert_eq!(
            std::fs::read_to_string(home.path().join(".claude/skills/drive-aterm/SKILL.md"))
                .unwrap(),
            DRIVE_SKILL_BODY
        );
        assert!(!home.path().join(".gemini").exists());
        assert!(!home.path().join(".config").exists());

        let second = auto_prime(home.path());
        assert!(
            second
                .agents
                .iter()
                .all(|a| a.outcome == Outcome::Unchanged),
            "{:?}",
            second.agents
        );
        assert!(!second.changed());
        assert_eq!(
            second.summary,
            "agent primer: claude unchanged, codex unchanged"
        );
    }

    /// A machine that ran the v1 installer: the block is upgraded in place around
    /// the user's own text, and the pass reports `Updated`, not `Installed`.
    #[test]
    fn auto_prime_upgrades_a_v1_block_in_place() {
        let home = aterm_tempfile::tempdir().unwrap();
        let dir = home.path().join(".codex");
        std::fs::create_dir_all(&dir).unwrap();
        let user = "# Codex rules\n\nPrefer uv.\n";
        std::fs::write(dir.join("AGENTS.md"), format!("{user}\n{V1_BLOCK}")).unwrap();

        let pass = auto_prime(home.path());
        assert_eq!(pass.agents[0].outcome, Outcome::Updated, "{}", pass.summary);
        let content = std::fs::read_to_string(dir.join("AGENTS.md")).unwrap();
        assert!(content.starts_with(user));
        assert!(
            content.contains("--allow-unix-socket"),
            "the Codex addendum landed"
        );
        assert_eq!(content.matches(MARK_PREFIX).count(), 1, "one begin marker");
        assert_eq!(content.matches(MARK_END).count(), 1, "one end marker");
        assert_eq!(
            auto_prime(home.path()).agents[0].outcome,
            Outcome::Unchanged
        );
    }

    /// A user's own `drive-aterm` skill (no marker) is never overwritten; the
    /// pass says so every time and never counts it as a change.
    #[test]
    fn auto_prime_never_overwrites_a_foreign_skill() {
        let home = aterm_tempfile::tempdir().unwrap();
        let skill = home.path().join(".claude/skills/drive-aterm/SKILL.md");
        std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
        let theirs = "---\nname: drive-aterm\n---\nmy own notes\n";
        std::fs::write(&skill, theirs).unwrap();

        let first = auto_prime(home.path());
        // The primer was still new, so the agent counts as installed…
        assert_eq!(first.agents[0].outcome, Outcome::Installed);
        assert_eq!(std::fs::read_to_string(&skill).unwrap(), theirs);
        // …and from then on the only thing left is the foreign file: reported,
        // never written, never logged as a change.
        let second = auto_prime(home.path());
        assert_eq!(second.agents[0].outcome, Outcome::SkippedForeign);
        assert!(!second.changed());
        assert!(second.summary.contains("left alone"), "{}", second.summary);
        assert_eq!(std::fs::read_to_string(&skill).unwrap(), theirs);
    }

    /// A corrupt (unterminated) block is an `Error` row that leaves the file
    /// byte-for-byte alone, and the other agents still get primed.
    #[test]
    fn auto_prime_is_fail_soft_per_agent() {
        let home = aterm_tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".gemini")).unwrap();
        std::fs::create_dir_all(home.path().join(".config/opencode")).unwrap();
        let corrupt = "x\n<!-- aterm primer v1 -->\nno end marker\n";
        std::fs::write(home.path().join(".gemini/GEMINI.md"), corrupt).unwrap();

        let pass = auto_prime(home.path());
        assert_eq!(pass.agents.len(), 2);
        assert!(
            matches!(pass.agents[0].outcome, Outcome::Error(_)),
            "{:?}",
            pass.agents[0]
        );
        assert_eq!(pass.agents[1].outcome, Outcome::Installed);
        assert_eq!(pass.errors().count(), 1);
        assert!(pass.summary.contains("gemini ERROR:"), "{}", pass.summary);
        assert_eq!(
            std::fs::read_to_string(home.path().join(".gemini/GEMINI.md")).unwrap(),
            corrupt
        );
    }

    #[test]
    fn auto_prime_with_no_agents_says_where_it_looked() {
        let home = aterm_tempfile::tempdir().unwrap();
        let pass = auto_prime(home.path());
        assert!(pass.agents.is_empty());
        assert!(!pass.changed());
        assert!(pass.summary.contains("no coding agents detected"));
        for a in AGENT_FILES {
            assert!(pass.summary.contains(a.dir), "{}", pass.summary);
        }
    }

    /// The `--diagnose` line: one word per registry agent, a skill mentioned
    /// only when it is not current.
    #[test]
    fn status_line_names_every_agent_in_one_line() {
        let home = aterm_tempfile::tempdir().unwrap();
        assert_eq!(
            status_line(home.path()),
            "claude not detected, codex not detected, gemini not detected, opencode not detected"
        );
        std::fs::create_dir_all(home.path().join(".claude")).unwrap();
        std::fs::create_dir_all(home.path().join(".codex")).unwrap();
        std::fs::write(home.path().join(".codex/AGENTS.md"), V1_BLOCK).unwrap();
        // One `(skill <name> absent)` per registered Claude skill, in registry
        // order — DERIVED, so shipping another bundled skill updates the
        // expectation with the registry instead of failing this assertion.
        let claude_skills: String = skills_for("claude")
            .iter()
            .map(|s| {
                format!(
                    " (skill {} absent)",
                    s.path.rsplit('/').nth(1).unwrap_or(s.path)
                )
            })
            .collect();
        assert_eq!(
            status_line(home.path()),
            format!(
                "claude absent{claude_skills}, codex stale, gemini not detected, \
                 opencode not detected"
            )
        );
        let _ = auto_prime(home.path());
        assert_eq!(
            status_line(home.path()),
            "claude installed, codex installed, gemini not detected, opencode not detected"
        );
        assert!(!status_line(home.path()).contains('\n'));
    }

    // ---- bundled skill files -------------------------------------------------

    /// The compiled-in skill must carry its marker, or every install would
    /// classify it `Foreign` and refuse to write. This is the one property that
    /// silently disables the whole feature if the asset is edited carelessly.
    #[test]
    fn bundled_skill_carries_its_managed_marker() {
        // Every bundled skill (drive-aterm, supervise-agent, …) must carry the
        // marker, or its install would classify it `Foreign` and refuse to write.
        for s in skills_for("claude") {
            assert!(
                s.body
                    .lines()
                    .any(|l| l.trim_start().starts_with(SKILL_MARK_PREFIX)),
                "{} lost its `{SKILL_MARK_PREFIX}` marker line",
                s.path
            );
            assert_eq!(
                skill_state(s.body, s.body),
                SkillState::Current,
                "{} must classify as Current against itself",
                s.path
            );
        }
    }

    /// The skill must be a valid Claude Code skill: YAML frontmatter with a
    /// `name:` and a `description:` (the fields the harness matches on).
    #[test]
    fn bundled_skill_has_usable_frontmatter() {
        for s in skills_for("claude") {
            assert_eq!(
                s.body.lines().next().map(str::trim),
                Some("---"),
                "{} must open with frontmatter",
                s.path
            );
            let head: Vec<&str> = s.body.lines().take(12).collect();
            assert!(
                head.iter().any(|l| l.starts_with("name:")),
                "{} frontmatter needs name:",
                s.path
            );
            assert!(
                head.iter().any(|l| l.starts_with("description:")),
                "{} frontmatter needs description: (it is what triggers the skill)",
                s.path
            );
        }
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
    /// none, so an install never fabricates a skills dir for an agent that has no
    /// such concept.
    #[test]
    fn skills_are_registered_only_for_agents_that_have_them() {
        // Claude Code ships two bundled skills today: drive-aterm + supervise-agent.
        assert_eq!(skills_for("claude").len(), 2);
        assert!(
            skills_for("claude")
                .iter()
                .any(|s| s.path.ends_with("drive-aterm/SKILL.md"))
                && skills_for("claude")
                    .iter()
                    .any(|s| s.path.ends_with("supervise-agent/SKILL.md")),
            "both drive-aterm and supervise-agent must be registered"
        );
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
