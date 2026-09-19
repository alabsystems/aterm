// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Session identities, phase 1 (separation): a session can carry its own AGENT
//! identity, set at spawn.
//!
//! ONE CONCEPT. `identity` is a spawn-time, immutable session property (like
//! `frozen_path`, unlike the `meta set` fields a driver rewrites). It names a
//! directory `<state>/identities/<name>/` — [`aterm_types::dirs::identities_dir`]
//! — and every agent aterm knows ([`aterm_primer::agent_homes`], the primer's
//! roster read through its `var` column) gets its home variable pointed at
//! its own conventional subdirectory there: `CLAUDE_CONFIG_DIR=<idir>/.claude`,
//! `CODEX_HOME=<idir>/.codex`. The subdirs carry the agents' OWN names so the
//! primer and the fabric hooks work unchanged: `ensure` runs
//! [`aterm_primer::auto_prime`] over the identity dir (detection-only, so the
//! 0700 subdirs aterm just made are what it detects) and carries the human's
//! `~/.claude/settings.json` aterm hook block into the identity's — a worker
//! under `identity=worker` has the aterm skills, the inbox hook and the manual
//! pointer, and its OWN login.
//!
//! aterm never opens a login. It knows nothing of what the agent keeps in the
//! dir beyond the directory names it made; `agents=<prog>:present|absent`
//! (the `identities` verb) is a `read_dir` name listing, bytes never opened.
//! The macOS keychain is the one login channel outside the dir: aterm never
//! reads, names or deletes a keychain item it did not create.
//!
//! THE SEAM. [`env`] rides `spawn_session`'s `env_add`, which
//! `aterm_pty::build_child_env` applies AFTER the deny pass — so the parent's
//! `CLAUDE_CONFIG_DIR` is dropped and the identity's survives, with no change
//! to the deny tables and no `is_ai_env_var` pin (an inherited key is REPLACED
//! by `env_add`'s value either way; the aterm-pty test
//! `build_child_env_replaces_the_parents_agent_home_with_the_identitys` pins
//! that per variable). An adopted shell (seamless update) gets nothing: it
//! keeps its env; only the label rides the handoff record.
//!
//! WHO CREATES. `spawn identity=<name>` is the ONLY create path
//! ([`ensure`] with `create = true`, from the wire verb — Owner-only). The
//! spawn seam itself and a cold restore of a leaf naming an identity pass
//! `create = false`: a missing identity there falls back to a default shell
//! with one stderr line ([`restorable`]), never a silently recreated one.
//! A downgrade to a build without the field keeps the shell's env and drops
//! the label — `sessions` says `identity=-` for a shell still logged in as
//! `worker` — the `frozen_path`-style degradation, documented and accepted.
//!
//! NAMES fold to lowercase at parse ([`parse_name`]): the default-case-
//! insensitive APFS would make `Worker` and `worker` one directory with two
//! labels, two `identities` rows and a `forget` that removes the other's tree.
//! Grammar after folding: `[a-z0-9][a-z0-9._-]{0,63}`; `-` alone is refused —
//! it is the wire's absent mark (`identity=-` opts an aimed spawn out of
//! inheriting) — and so is `forget`, the `identities` verb's sub-form word:
//! an identity so named could be created but never described or removed.

use std::io;
use std::path::{Path, PathBuf};

/// The longest name: a directory name, and one that fits an `ls` row.
pub(crate) const NAME_MAX: usize = 64;

/// The mark of aterm's own hook entry in a Claude settings file — the SAME
/// mark `aterm-link hook install` uses (`hook.rs`'s `OWN_MARK`): its command
/// runs `hook run`, under either spelling (`aterm-link hook run …`,
/// `aterm link hook run …`). The carry copies exactly the entries wearing it.
const HOOK_MARK: &str = " hook run ";

/// Parse (and FOLD) a spawn-time identity name. `Worker` and `worker` are one
/// identity; the folded spelling is the directory name. Refused: an empty
/// name, `-` (the absent mark), `forget` (the verb's sub-form word — the one
/// name `identities` could never describe or remove), a first character
/// outside `[a-z0-9]`, any character outside `[a-z0-9._-]`, and anything
/// past [`NAME_MAX`] bytes — so a name can never be `..`, a path, or a shell
/// word.
pub(crate) fn parse_name(raw: &str) -> Result<String, String> {
    let name = raw.to_ascii_lowercase();
    if name.is_empty() {
        return Err("identity: a name is required".to_string());
    }
    if name == "-" {
        return Err("identity: `-` is the absent mark, not a name".to_string());
    }
    if name == "forget" {
        return Err("identity: `forget` is the `identities` verb's word, not a name".to_string());
    }
    if name.len() > NAME_MAX {
        return Err(format!(
            "identity: a name is at most {NAME_MAX} characters, got {}",
            name.len()
        ));
    }
    let first = name.chars().next().unwrap_or('-');
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return Err(format!(
            "identity: a name starts with a letter or digit, got {name:?}"
        ));
    }
    if let Some(bad) = name
        .chars()
        .find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')))
    {
        return Err(format!(
            "identity: {bad:?} is not allowed in a name (letters, digits, `.`, `_`, `-`)"
        ));
    }
    Ok(name)
}

/// The directory a (parsed) name resolves to under this instance's state
/// root; `None` when no state root resolves at all.
#[cfg(test)]
pub(crate) fn dir_for(name: &str) -> Option<PathBuf> {
    aterm_types::dirs::identities_dir().map(|root| root.join(name))
}

/// The human's Claude settings file — the hook block a fresh identity carries
/// is read from here (never written to).
fn human_claude_settings() -> Option<PathBuf> {
    aterm_primer::home_dir().map(|home| home.join(".claude").join("settings.json"))
}

/// Resolve `name` to its identity directory. With `create`, provision it —
/// `<identities>/<name>/` and each agent subdir, 0700, owned by us (a foreign
/// owner is REFUSED, fail-closed: [`aterm_types::fs_restricted::ensure_private_dir`]),
/// prime it and carry the hooks — idempotently: a second call over the same
/// name is the same directory, verified again, and nothing rewritten that is
/// current. Without `create`, an identity is what the `identities` verb calls
/// one ([`identity_dir`]: a REAL directory, never a symlink — measured
/// 2026-09-17 (review): `is_dir` followed a symlink the verb refused, so a
/// restore re-injected env pointing outside the identities tree at a name
/// `identities` could neither list nor forget); anything else is `NotFound`
/// and nothing is touched. The name is parsed (and folded) here, so every
/// caller gets the grammar for free.
pub(crate) fn ensure(name: &str, create: bool) -> io::Result<PathBuf> {
    let root = aterm_types::dirs::identities_dir().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "no state root resolves (HOME and ATERM_STATE_HOME both unset)",
        )
    })?;
    ensure_in(&root, name, create, human_claude_settings().as_deref())
}

/// [`ensure`] with its roots injected: `root` is the identities dir,
/// `human_settings` the Claude settings file whose aterm hook block is
/// carried (`None` = carry nothing).
pub(crate) fn ensure_in(
    root: &Path,
    name: &str,
    create: bool,
    human_settings: Option<&Path>,
) -> io::Result<PathBuf> {
    let name = parse_name(name).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    if !create {
        return identity_dir(root, &name).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, format!("no such identity {name}"))
        });
    }
    let dir = root.join(&name);
    aterm_types::fs_restricted::ensure_private_dir(root)?;
    aterm_types::fs_restricted::ensure_private_dir(&dir)?;
    for row in aterm_primer::agent_homes() {
        aterm_types::fs_restricted::ensure_private_dir(&dir.join(row.sub))?;
    }
    // The primer over the identity dir: the subdirs above are what it detects,
    // so exactly the agents of the table get their context file and skills —
    // fail-soft per agent, exactly as it is for `$HOME`. The IDENTITY entry
    // point, not `auto_prime`: that one follows the human's `$XDG_CONFIG_HOME`
    // for the `.config/` row, and measured 2026-09-17 (review) wrote aterm's
    // OpenCode files into the HUMAN's tree while provisioning an identity.
    let pass = aterm_primer::auto_prime_identity(&dir);
    for (agent, err) in pass.errors() {
        aterm_log::warn!("identity {name}: primer: {agent}: {err}");
    }
    if let Some(human) = human_settings {
        match carry_claude_hooks(human, &dir.join(".claude").join("settings.json")) {
            Ok(HookCarry::Written) => {
                aterm_log::info!(
                    "identity {name}: carried the aterm hooks into its .claude/settings.json"
                );
            }
            Ok(HookCarry::NoBlock | HookCarry::Unchanged) => {}
            Err(e) => aterm_log::warn!("identity {name}: hooks not carried: {e}"),
        }
    }
    Ok(dir)
}

/// The env additions for a session under the identity at `dir`: one pair per
/// table row, `<VAR>=<dir>/<sub>`. Applied by `build_child_env` after the deny
/// pass, so these are the ONLY agent-home variables the child sees.
pub(crate) fn env(dir: &Path) -> Vec<(String, String)> {
    aterm_primer::agent_homes()
        .map(|row| (row.var.to_string(), dir.join(row.sub).display().to_string()))
        .collect()
}

/// The RESTORE rule: a cold restore of a leaf naming `name` re-injects the
/// identity's env only when the identity still exists — `create = false`. A
/// forgotten (or foreign-rooted) identity falls back to a default shell, with
/// one stderr line saying so; restore never creates an identity.
pub(crate) fn restorable(name: &str) -> Option<String> {
    match ensure(name, false) {
        Ok(_) => Some(name.to_string()),
        Err(e) => {
            crate::logging::stderr_line!(
                "aterm-gui: session restore: identity {name}: {e}; the restored tab starts a \
                 default shell"
            );
            None
        }
    }
}

/// [`restorable`] without the stderr line: the identity a restore leaf naming
/// `name` would re-inject, or `None` when it no longer exists. For the SECOND
/// read of the same leaf on a cold restore — the graft's check that the
/// window's bootstrap shell wears what the pane names
/// (`App::restore_terminal_leaf`), after `main_entry` already read it (and
/// said so) to spawn that bootstrap.
pub(crate) fn existing(name: &str) -> Option<String> {
    ensure(name, false).ok().map(|_| name.to_string())
}

/// What [`carry_claude_hooks`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HookCarry {
    /// The human's settings carry no aterm hook entry (or no file): nothing to carry.
    NoBlock,
    /// The identity's settings already carry exactly these entries.
    Unchanged,
    /// The identity's settings were (re)written with the entries.
    Written,
}

/// Carry the aterm hook entries of the human's Claude `settings.json` into the
/// identity's — the entries whose `command` carries [`HOOK_MARK`], grouped as
/// they are in the source, and NOTHING else of the human's file (their
/// permissions, model, env, other hooks stay theirs). In the identity's file
/// the same rule `aterm-link hook install --merge` applies: entries wearing
/// the mark are replaced, every other key and hook is kept, a group emptied
/// by the replacement goes. Written whole, atomically, 0600 — a settings file
/// can carry `env` keys. The hook commands were self-tested when the human
/// installed them; at run time each resolves its session from the identity
/// session's own `ATERM_SESSION_ID`, so nothing here needs a live socket.
pub(crate) fn carry_claude_hooks(human: &Path, ours: &Path) -> io::Result<HookCarry> {
    use aterm_json::{Map, Value};
    let text = match std::fs::read_to_string(human) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(HookCarry::NoBlock),
        Err(e) => return Err(e),
    };
    let doc: Value = aterm_json::from_str(&text).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: not JSON: {e}", human.display()),
        )
    })?;
    let carried = marked_hook_groups(&doc);
    if carried.is_empty() {
        return Ok(HookCarry::NoBlock);
    }

    let previous = match std::fs::read_to_string(ours) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    let mut existing: Value = match &previous {
        Some(text) => aterm_json::from_str(text).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: not JSON: {e}", ours.display()),
            )
        })?,
        None => Value::Object(Map::new()),
    };
    replace_marked_hook_groups(&mut existing, carried).map_err(|why| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: {why}", ours.display()),
        )
    })?;
    let rendered = format!(
        "{}\n",
        aterm_json::to_string(&existing)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?
    );
    if previous.as_deref() == Some(rendered.as_str()) {
        return Ok(HookCarry::Unchanged);
    }
    write_private_atomically(ours, rendered.as_bytes())?;
    Ok(HookCarry::Written)
}

/// The aterm hook groups of a Claude settings document, per event: each
/// source group narrowed to its marked entries (matcher kept), groups and
/// events with none dropped.
fn marked_hook_groups(doc: &aterm_json::Value) -> aterm_json::Map {
    use aterm_json::{Map, Value};
    let mut carried: Map = Map::new();
    for (event, groups) in doc
        .get("hooks")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let Some(groups) = groups.as_array() else {
            continue;
        };
        let mut kept = Vec::new();
        for group in groups {
            let Some(entries) = group.get("hooks").and_then(Value::as_array) else {
                continue;
            };
            let marked: Vec<Value> = entries
                .iter()
                .filter(|e| carries_mark(e))
                .cloned()
                .collect();
            if marked.is_empty() {
                continue;
            }
            let mut group = group.clone();
            if let Some(obj) = group.as_object_mut() {
                obj.insert("hooks".to_string(), Value::Array(marked));
            }
            kept.push(group);
        }
        if !kept.is_empty() {
            carried.insert(event.clone(), Value::Array(kept));
        }
    }
    carried
}

/// `aterm-link hook install --merge`'s rule over the identity's document:
/// entries wearing the mark go (a group emptied by that goes with them, an
/// event emptied too), every other key and hook stays, then `carried` is
/// appended per event. `Err` names a document shape no merge is safe into.
fn replace_marked_hook_groups(
    existing: &mut aterm_json::Value,
    carried: aterm_json::Map,
) -> Result<(), String> {
    use aterm_json::{Map, Value};
    let obj = existing
        .as_object_mut()
        .ok_or_else(|| "not a JSON object".to_string())?;
    let events = obj
        .entry("hooks".to_string())
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| "`hooks` is not a JSON object".to_string())?;
    for groups in events.values_mut() {
        let Some(groups) = groups.as_array_mut() else {
            continue;
        };
        for group in groups.iter_mut() {
            if let Some(entries) = group
                .as_object_mut()
                .and_then(|g| g.get_mut("hooks"))
                .and_then(Value::as_array_mut)
            {
                entries.retain(|e| !carries_mark(e));
            }
        }
        groups.retain(|g| {
            g.get("hooks")
                .and_then(Value::as_array)
                .is_none_or(|entries| !entries.is_empty())
        });
    }
    events.retain(|_, groups| groups.as_array().is_none_or(|g| !g.is_empty()));
    for (event, groups) in carried {
        let Value::Array(groups) = groups else {
            continue;
        };
        let slot = events
            .entry(event)
            .or_insert_with(|| Value::Array(Vec::new()));
        if !slot.is_array() {
            *slot = Value::Array(Vec::new());
        }
        if let Some(slot) = slot.as_array_mut() {
            slot.extend(groups);
        }
    }
    Ok(())
}

/// Whether one hook entry is aterm's own (its `command` runs `hook run`).
fn carries_mark(entry: &aterm_json::Value) -> bool {
    entry
        .get("command")
        .and_then(aterm_json::Value::as_str)
        .is_some_and(|c| c.contains(HOOK_MARK))
}

/// Whole-file replace: a 0600 temp file beside the target, renamed over it —
/// nothing is ever half-written, not even for the instant of the write.
fn write_private_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write as _;
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no parent directory"))?;
    let tmp = dir.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("settings.json"),
        std::process::id()
    ));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let mut file = opts.open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// The `identities` verb's usage line — the whole grammar in one row.
const IDENTITIES_USAGE: &str = "ERR usage: identities [<name>|forget <name> [confirm=<name>]]\n";

/// `identities [<name>|forget <name> [confirm=<name>]]` -> the ON-DISK roster
/// of agent identities: what `spawn identity=` created under
/// [`aterm_types::dirs::identities_dir`], which live sessions carry each, and
/// which agents have files there. Owner-only like `sessions` (whose
/// `identity=` column it explains) and Lines-framed from the table — a listing
/// reply is `OK <n>` + exactly n lines (the client streams exactly that many)
/// or one `ERR` line — except `forget`, whose replies are all ONE status line
/// (`control_verbs::framing_of` flips that sub-form to `Status`, as it does
/// `inbox seen`): the live try of 2026-09-17 measured aterm-ctl refusing
/// `OK removed=… left=keychain` as a malformed row count under Lines.
///
/// | form | reply |
/// |---|---|
/// | `identities` | `OK <n>` then `<name> dir=<pct> sessions=<n> agents=claude:present,codex:absent` per identity, by name |
/// | `identities <name>` | `OK <1+agents>`: that row, then `agent=<prog> var=<VAR> home=<pct> files=present\|absent` per agent |
/// | `identities forget <name>` | `ERR confirm: identities forget <name> confirm=<name> removes <pct>` — nothing touched |
/// | `… confirm=<name>`, a live session carries it | `ERR identity in use sessions=<sid>,…` — nothing touched |
/// | `… confirm=<name>` | `OK removed=<pct> left=keychain` |
/// | unknown name | `ERR no such identity <name>` |
/// | anything else | `ERR usage: identities [<name>\|forget <name> [confirm=<name>]]` |
///
/// `present` is a `read_dir` NAME listing of the agent's subdirectory — at
/// least one entry — and nothing is ever opened: aterm knows no login, it
/// knows a directory it made is not empty (so a primed identity reads
/// `present` for every agent of the table; `absent` is a missing or emptied
/// subdirectory). `forget` removes the tree, and the credentials the agents
/// keep as files go with it; a login an agent keeps in the macOS keychain does
/// NOT — `left=keychain` says so every time, and the help says: sign out in
/// the agent first (`/logout`); aterm never reads, names or deletes a keychain
/// item it did not create. The live count is the registry's: a shell adopted
/// from a build without the label carries none and is not counted —
/// `sessions=0` beside a shell still logged in as that identity is the
/// documented degradation, and the help says to close it first. The in-use
/// check and the removal are two steps, not one: a `spawn identity=` racing
/// the removal recreates the directory, which is the create path doing its
/// job, never a session without its env. Names are parsed (folded) here, so
/// `Worker` is `worker`; a name the grammar refuses is usage — it cannot
/// exist, and it is never joined to a path. Only REAL directories are
/// identities: a symlink or a file in the root is neither listed, described
/// nor removed.
pub(crate) fn cmd_identities(store: &crate::session_store::Store, rest: &str) -> String {
    // The live users, from the registry snapshot: `(identity, sid)` in local-id
    // order. Cloned out under a short read guard, never held across the
    // directory walk.
    let live: Vec<(String, String)> = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        g.snapshot()
            .into_iter()
            .filter_map(|h| {
                h.identity
                    .as_deref()
                    .map(|name| (name.to_string(), h.sid.as_str().to_string()))
            })
            .collect()
    };
    let Some(root) = aterm_types::dirs::identities_dir() else {
        return "ERR identities: no state root resolves (HOME and ATERM_STATE_HOME both unset)\n"
            .to_string();
    };
    identities_reply(&root, &live, rest)
}

/// The pure half of [`cmd_identities`]: `root` is the identities dir, `live`
/// the registry's `(identity, sid)` pairs in listing order.
pub(crate) fn identities_reply(root: &Path, live: &[(String, String)], rest: &str) -> String {
    let toks: Vec<&str> = rest.split_whitespace().collect();
    match toks.as_slice() {
        [] => list_reply(root, live),
        ["forget", name] => forget_reply(root, live, name, None),
        ["forget", name, confirm] => match confirm.strip_prefix("confirm=") {
            Some(word) => forget_reply(root, live, name, Some(word)),
            None => IDENTITIES_USAGE.to_string(),
        },
        [name] if *name != "forget" && !name.contains('=') => one_reply(root, live, name),
        _ => IDENTITIES_USAGE.to_string(),
    }
}

/// The directory `name` (already folded) names under `root`, when it IS an
/// identity: a real directory, never a symlink — the verb lists, describes and
/// removes only directories aterm made.
fn identity_dir(root: &Path, name: &str) -> Option<PathBuf> {
    let dir = root.join(name);
    std::fs::symlink_metadata(&dir)
        .ok()
        .filter(std::fs::Metadata::is_dir)
        .map(|_| dir)
}

/// Every identity under `root`, by name: the entries whose name IS its own
/// folded spelling and which are real directories. Anything else in the root
/// is not an identity and is not listed. A missing root is an empty roster.
fn list_names(root: &Path) -> io::Result<Vec<String>> {
    let rd = match std::fs::read_dir(root) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut names = Vec::new();
    for entry in rd {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if parse_name(&name).ok().as_deref() != Some(name.as_str()) {
            continue;
        }
        if entry.file_type()?.is_dir() {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

fn presence_word(present: bool) -> &'static str {
    if present { "present" } else { "absent" }
}

/// The agents' files under `dir`, per table row: `present` iff the agent's
/// subdirectory has at least one entry — a NAME listing; nothing is opened.
fn agent_presence(dir: &Path) -> Vec<(aterm_primer::AgentHome, bool)> {
    aterm_primer::agent_homes()
        .map(|row| {
            let present = dir_non_empty(&dir.join(row.sub));
            (row, present)
        })
        .collect()
}

/// At least one directory entry — the listing is read, no entry is opened.
fn dir_non_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .ok()
        .is_some_and(|mut rd| rd.next().is_some())
}

fn pct_path(path: &Path) -> String {
    aterm_control::wire::pct_encode(&path.display().to_string())
}

/// One roster line: `<name> dir=<pct> sessions=<n> agents=<prog>:<word>,…`.
fn identity_row(root: &Path, live: &[(String, String)], name: &str) -> String {
    let dir = root.join(name);
    let sessions = live.iter().filter(|(i, _)| i == name).count();
    let agents: Vec<String> = agent_presence(&dir)
        .into_iter()
        .map(|(row, present)| format!("{}:{}", row.agent, presence_word(present)))
        .collect();
    let agents = if agents.is_empty() {
        "-".to_string()
    } else {
        agents.join(",")
    };
    format!(
        "{name} dir={} sessions={sessions} agents={agents}",
        pct_path(&dir)
    )
}

fn list_reply(root: &Path, live: &[(String, String)]) -> String {
    let names = match list_names(root) {
        Ok(names) => names,
        Err(e) => return format!("ERR identities: {}: {e}\n", root.display()),
    };
    let mut out = format!("OK {}\n", names.len());
    for name in &names {
        out.push_str(&identity_row(root, live, name));
        out.push('\n');
    }
    out
}

fn one_reply(root: &Path, live: &[(String, String)], raw: &str) -> String {
    let Ok(name) = parse_name(raw) else {
        return IDENTITIES_USAGE.to_string();
    };
    let Some(dir) = identity_dir(root, &name) else {
        return format!("ERR no such identity {name}\n");
    };
    let agents = agent_presence(&dir);
    let mut out = format!("OK {}\n", 1 + agents.len());
    out.push_str(&identity_row(root, live, &name));
    out.push('\n');
    for (row, present) in agents {
        out.push_str(&format!(
            "agent={} var={} home={} files={}\n",
            row.agent,
            row.var,
            pct_path(&dir.join(row.sub)),
            presence_word(present)
        ));
    }
    out
}

fn forget_reply(
    root: &Path,
    live: &[(String, String)],
    raw: &str,
    confirm: Option<&str>,
) -> String {
    let Ok(name) = parse_name(raw) else {
        return IDENTITIES_USAGE.to_string();
    };
    let Some(dir) = identity_dir(root, &name) else {
        return format!("ERR no such identity {name}\n");
    };
    // The confirm word folds like the name: `confirm=Worker` confirms `worker`.
    if confirm.map(str::to_ascii_lowercase).as_deref() != Some(name.as_str()) {
        return format!(
            "ERR confirm: identities forget {name} confirm={name} removes {}\n",
            pct_path(&dir)
        );
    }
    let users: Vec<&str> = live
        .iter()
        .filter(|(i, _)| *i == name)
        .map(|(_, sid)| sid.as_str())
        .collect();
    if !users.is_empty() {
        return format!("ERR identity in use sessions={}\n", users.join(","));
    }
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => format!("OK removed={} left=keychain\n", pct_path(&dir)),
        Err(e) => format!("ERR identity {name}: {e}\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aterm-identity-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A human's settings file carrying the aterm hook block beside their own
    /// keys and a foreign hook — the shape `aterm-link hook install --merge`
    /// leaves behind.
    const HUMAN_SETTINGS: &str = r#"{
  "model": "opus",
  "env": {"SECRET_OF_THE_HUMAN": "never-copied"},
  "hooks": {
    "SessionStart": [
      {"hooks": [{"type": "command", "command": "/opt/aterm/aterm-link hook run session-start"}]}
    ],
    "Stop": [
      {"hooks": [{"type": "command", "command": "/opt/aterm/aterm-link hook run stop --wake-budget 3/4 --timeout 600", "timeout": 600}]},
      {"matcher": "", "hooks": [{"type": "command", "command": "/usr/local/bin/their-own-stop-hook"}]}
    ],
    "PreToolUse": [
      {"matcher": "Bash", "hooks": [{"type": "command", "command": "/usr/local/bin/their-lint"}]}
    ]
  }
}
"#;

    #[test]
    fn names_fold_to_lowercase_and_follow_the_grammar() {
        assert_eq!(parse_name("Worker").as_deref(), Ok("worker"));
        assert_eq!(parse_name("WORKER-2.a_b").as_deref(), Ok("worker-2.a_b"));
        assert_eq!(parse_name("9lives").as_deref(), Ok("9lives"));
        let max = "a".repeat(NAME_MAX);
        assert_eq!(parse_name(&max).as_deref(), Ok(max.as_str()));
        for bad in [
            "", "-", "-x", ".hidden", "_under", "a/b", "a b", "..", "a\u{e9}", "worker\n",
            " worker", "worker ", "ä",
        ] {
            assert!(parse_name(bad).is_err(), "{bad:?} must be refused");
        }
        // The verb's own word (review, 2026-09-17): `spawn identity=forget`
        // was creatable, and `identities forget` is usage — an identity no
        // verb could describe or remove. Folded first, so `FORGET` is it too.
        for word in ["forget", "FORGET", "Forget"] {
            assert!(parse_name(word).is_err(), "{word:?} must be refused");
        }
        assert!(parse_name("forget2").is_ok() && parse_name("forgetful").is_ok());
        assert!(parse_name(&"a".repeat(NAME_MAX + 1)).is_err());
        // The two spellings are ONE identity: same folded name, same dir.
        assert_eq!(parse_name("Worker"), parse_name("wOrKeR"));
    }

    /// `ensure(create = true)`: the dir and every agent subdir exist at 0700,
    /// the primer's files are in place for every measured agent, a second call
    /// is the same dir, two names are two dirs, and `create = false` finds
    /// what exists and refuses what does not — touching nothing.
    #[test]
    fn ensure_creates_0700_dirs_once_primes_them_and_never_creates_on_restore() {
        let root = scratch("ensure").join("identities");
        let dir = ensure_in(&root, "Worker", true, None).expect("created");
        assert_eq!(
            dir,
            root.join("worker"),
            "folded, under the identities root"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&root), 0o700);
            assert_eq!(mode(&dir), 0o700);
            for row in aterm_primer::agent_homes() {
                assert_eq!(mode(&dir.join(row.sub)), 0o700, "{}", row.agent);
            }
        }
        // The primer reached every measured agent: its context file and skills.
        assert!(dir.join(".claude/CLAUDE.md").is_file());
        assert!(dir.join(".claude/skills/aterm-fabric/SKILL.md").is_file());
        assert!(
            dir.join(".claude/skills/supervise-agent/SKILL.md")
                .is_file()
        );
        assert!(dir.join(".codex/AGENTS.md").is_file());
        assert!(dir.join(".codex/prompts/aterm-fabric.md").is_file());
        assert!(
            !dir.join(".claude/settings.json").exists(),
            "no human settings given: no settings file is invented"
        );
        // Once: the same name (any case) is the same dir, and nothing errs.
        let again = ensure_in(&root, "WORKER", true, None).expect("idempotent");
        assert_eq!(again, dir);
        assert_eq!(
            std::fs::read_dir(&root).unwrap().count(),
            1,
            "one directory for the one identity"
        );
        // Two names, two dirs.
        let other = ensure_in(&root, "reviewer", true, None).expect("second identity");
        assert_ne!(other, dir);
        assert!(other.join(".claude").is_dir() && other.join(".codex").is_dir());
        // The restore rule: existing is found; missing is NotFound and stays missing.
        assert_eq!(ensure_in(&root, "worker", false, None).unwrap(), dir);
        let missing = ensure_in(&root, "ghost", false, None).unwrap_err();
        assert_eq!(missing.kind(), io::ErrorKind::NotFound);
        assert!(!root.join("ghost").exists(), "create=false never creates");
        // A bad name is refused before anything is touched.
        assert_eq!(
            ensure_in(&root, "-", true, None).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    /// The env seam: one pair per table row, each pointing INTO the identity
    /// dir at the agent's own subdir — nothing else.
    #[test]
    fn env_points_every_table_var_into_the_identity_dir() {
        let dir = PathBuf::from("/state/identities/worker");
        let env = env(&dir);
        assert_eq!(
            env,
            vec![
                (
                    "CLAUDE_CONFIG_DIR".to_string(),
                    "/state/identities/worker/.claude".to_string()
                ),
                (
                    "CODEX_HOME".to_string(),
                    "/state/identities/worker/.codex".to_string()
                ),
            ]
        );
        assert_eq!(env.len(), aterm_primer::agent_homes().count());
    }

    /// THE HOOK MERGE. When the human's settings carry the aterm hook block, a
    /// fresh identity's `.claude/settings.json` carries exactly those entries —
    /// grouped as they were, the human's own hooks and every other key left
    /// behind. On an identity whose settings already exist, the aterm entries
    /// are replaced and everything else kept; a second pass is unchanged.
    #[test]
    fn the_hook_merge_carries_exactly_the_aterm_entries() {
        let root = scratch("hooks");
        let human = root.join("human-settings.json");
        std::fs::write(&human, HUMAN_SETTINGS).unwrap();
        let ours = root.join("identities/worker/.claude/settings.json");
        let dir = ensure_in(&root.join("identities"), "worker", true, Some(&human)).unwrap();
        assert_eq!(dir.join(".claude/settings.json"), ours);
        let text = std::fs::read_to_string(&ours).expect("written");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&ours).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let doc: aterm_json::Value = aterm_json::from_str(&text).unwrap();
        let obj = doc.as_object().unwrap();
        assert_eq!(
            obj.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["hooks"],
            "{text}"
        );
        assert!(!text.contains("SECRET_OF_THE_HUMAN") && !text.contains("opus"));
        assert!(!text.contains("their-own-stop-hook") && !text.contains("their-lint"));
        let hooks = doc.get("hooks").and_then(|h| h.as_object()).unwrap();
        assert_eq!(
            hooks.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["SessionStart", "Stop"]
        );
        assert_eq!(text.matches(HOOK_MARK).count(), 2, "{text}");
        assert!(
            text.contains("--wake-budget 3/4"),
            "the entry rides whole: {text}"
        );
        assert!(
            text.contains("\"timeout\":600") || text.contains("\"timeout\": 600"),
            "{text}"
        );

        // A second pass over the same human file: unchanged, byte for byte.
        assert_eq!(
            carry_claude_hooks(&human, &ours).unwrap(),
            HookCarry::Unchanged
        );
        assert_eq!(std::fs::read_to_string(&ours).unwrap(), text);

        // The identity's file grew a hook and a key of its own; the human's
        // aterm command moved. The carry replaces aterm's entries and keeps
        // the rest.
        let mut own: aterm_json::Value = aterm_json::from_str(&text).unwrap();
        let own_obj = own.as_object_mut().unwrap();
        own_obj.insert(
            "permissions".to_string(),
            aterm_json::from_str(r#"{"allow": ["Bash(ls:*)"]}"#).unwrap(),
        );
        own_obj
            .get_mut("hooks")
            .and_then(|h| h.as_object_mut())
            .unwrap()
            .insert(
                "UserPromptSubmit".to_string(),
                aterm_json::from_str(
                    r#"[{"hooks": [{"type": "command", "command": "/their/own/prompt-hook"}]}]"#,
                )
                .unwrap(),
            );
        std::fs::write(&ours, aterm_json::to_string(&own).unwrap()).unwrap();
        std::fs::write(
            &human,
            HUMAN_SETTINGS.replace("/opt/aterm/aterm-link", "/new/aterm-link"),
        )
        .unwrap();
        assert_eq!(
            carry_claude_hooks(&human, &ours).unwrap(),
            HookCarry::Written
        );
        let text = std::fs::read_to_string(&ours).unwrap();
        assert!(text.contains("/their/own/prompt-hook"), "kept: {text}");
        assert!(text.contains("Bash(ls:*)"), "kept: {text}");
        assert!(!text.contains("/opt/aterm/aterm-link"), "replaced: {text}");
        assert_eq!(text.matches("/new/aterm-link").count(), 2, "{text}");

        // No block in the human's file: nothing is written for a fresh identity.
        std::fs::write(&human, r#"{"model": "opus", "hooks": {"Stop": [{"hooks": [{"type": "command", "command": "/their/stop"}]}]}}"#).unwrap();
        let bare = ensure_in(&root.join("identities"), "bare", true, Some(&human)).unwrap();
        assert!(!bare.join(".claude/settings.json").exists());
        assert_eq!(
            carry_claude_hooks(&human, &bare.join(".claude/settings.json")).unwrap(),
            HookCarry::NoBlock
        );
        // No human file at all: the same.
        assert_eq!(
            carry_claude_hooks(
                &root.join("absent.json"),
                &bare.join(".claude/settings.json")
            )
            .unwrap(),
            HookCarry::NoBlock
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The public `ensure` resolves through `ATERM_STATE_HOME`, the knob a
    /// headless instance keeps its state under — the identities root is
    /// `<state>/identities`, and a name is a directory right below it.
    #[test]
    fn ensure_resolves_the_identities_root_through_the_state_home() {
        let state = scratch("state");
        aterm_log::env::scoped("ATERM_STATE_HOME", &state, || {
            assert_eq!(
                dir_for("worker"),
                Some(state.join("identities").join("worker"))
            );
            let dir = ensure("Test", true).expect("created under the state home");
            assert_eq!(dir, state.join("identities").join("test"));
            assert!(dir.join(".claude").is_dir() && dir.join(".codex").is_dir());
            assert_eq!(restorable("test").as_deref(), Some("test"));
            assert_eq!(
                restorable("ghost"),
                None,
                "a missing identity is not restorable"
            );
            assert!(
                !state.join("identities").join("ghost").exists(),
                "and restore created nothing"
            );
        });
        let _ = std::fs::remove_dir_all(&state);
    }

    /// The verb's LIST: every real directory in the root whose name is its own
    /// folded spelling, by name, with the live count from the registry pairs
    /// and `present`/`absent` per agent by a NON-EMPTY subdirectory. A file, a
    /// symlink and a mis-spelt directory in the root are not identities and
    /// are not listed.
    #[test]
    fn identities_lists_names_live_counts_and_presence_by_non_empty_subdir() {
        let root = scratch("verb-list");
        ensure_in(&root, "worker", true, None).unwrap();
        let bare = root.join("bare");
        std::fs::create_dir_all(bare.join(".claude")).unwrap();
        std::fs::create_dir_all(bare.join(".codex")).unwrap();
        std::fs::write(root.join("notes.txt"), b"not an identity").unwrap();
        std::fs::create_dir_all(root.join("Not Folded")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("worker"), root.join("link")).unwrap();
        let live = vec![
            ("worker".to_string(), "s-a".to_string()),
            ("worker".to_string(), "s-b".to_string()),
            ("other".to_string(), "s-c".to_string()),
        ];
        let out = identities_reply(&root, &live, "");
        assert_eq!(
            out,
            format!(
                "OK 2\nbare dir={} sessions=0 agents=claude:absent,codex:absent\n\
                 worker dir={} sessions=2 agents=claude:present,codex:present\n",
                pct_path(&bare),
                pct_path(&root.join("worker")),
            )
        );
        // A root that does not exist yet is an empty roster, not an error.
        assert_eq!(identities_reply(&root.join("none"), &[], ""), "OK 0\n");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// ONE identity: `OK <1+agents>` — the row, then one line per agent naming
    /// its variable and home. `present` is a name listing: the sentinel file
    /// is mode 0000, so any attempt to open it would have failed the test, and
    /// the name folds (`SEALED` is `sealed`). An unknown name is named back.
    #[test]
    fn identities_one_names_each_agents_var_and_home_and_never_opens_a_file() {
        let root = scratch("verb-one");
        let dir = root.join("sealed");
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        std::fs::create_dir_all(dir.join(".codex")).unwrap();
        let sentinel = dir.join(".claude").join(".credentials.json");
        std::fs::write(&sentinel, b"{\"secret\":\"never read\"}").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&sentinel, std::fs::Permissions::from_mode(0o000)).unwrap();
        }
        let want = format!(
            "OK 3\nsealed dir={} sessions=0 agents=claude:present,codex:absent\n\
             agent=claude var=CLAUDE_CONFIG_DIR home={} files=present\n\
             agent=codex var=CODEX_HOME home={} files=absent\n",
            pct_path(&dir),
            pct_path(&dir.join(".claude")),
            pct_path(&dir.join(".codex")),
        );
        assert_eq!(identities_reply(&root, &[], "sealed"), want);
        assert_eq!(
            identities_reply(&root, &[], "SEALED"),
            want,
            "the name folds"
        );
        assert_eq!(
            identities_reply(&root, &[], "ghost"),
            "ERR no such identity ghost\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&sentinel, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// FORGET: no confirm word (or the wrong one) is the confirm line and
    /// nothing is touched; a live user is `ERR identity in use` naming the
    /// sids and nothing is touched; then the tree goes and the reply says what
    /// aterm left behind — `left=keychain`, every time.
    #[test]
    fn identities_forget_needs_the_confirm_word_refuses_a_live_user_then_removes_the_tree() {
        let root = scratch("verb-forget");
        let dir = ensure_in(&root, "worker", true, None).unwrap();
        let confirm_line = format!(
            "ERR confirm: identities forget worker confirm=worker removes {}\n",
            pct_path(&dir)
        );
        assert_eq!(identities_reply(&root, &[], "forget worker"), confirm_line);
        assert_eq!(
            identities_reply(&root, &[], "forget worker confirm=other"),
            confirm_line
        );
        assert_eq!(
            identities_reply(&root, &[], "forget Worker"),
            confirm_line,
            "the name folds before the confirm line names it"
        );
        assert!(dir.is_dir(), "nothing removed without the word");

        let live = vec![
            ("worker".to_string(), "s-a".to_string()),
            ("other".to_string(), "s-b".to_string()),
            ("worker".to_string(), "s-c".to_string()),
        ];
        assert_eq!(
            identities_reply(&root, &live, "forget worker confirm=worker"),
            "ERR identity in use sessions=s-a,s-c\n"
        );
        assert!(dir.is_dir(), "nothing removed while a session carries it");

        assert_eq!(
            identities_reply(&root, &[], "forget worker confirm=Worker"),
            format!("OK removed={} left=keychain\n", pct_path(&dir)),
            "the confirm word folds like the name"
        );
        assert!(!dir.exists(), "the tree is gone");
        assert_eq!(
            identities_reply(&root, &[], "forget worker confirm=worker"),
            "ERR no such identity worker\n"
        );
        assert_eq!(identities_reply(&root, &[], ""), "OK 0\n");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Junk is usage, never a guess — and a name the grammar refuses is usage
    /// too, so `..` and a path are never joined to the root.
    #[test]
    fn identities_junk_and_ungrammatical_names_are_usage() {
        let root = scratch("verb-usage");
        for rest in [
            "forget",
            "x y",
            "confirm=x",
            "forget x y",
            "forget x confirm=x extra",
            "../x",
            "-",
            "forget ../x confirm=../x",
            "forget - confirm=-",
        ] {
            assert_eq!(
                identities_reply(&root, &[], rest),
                IDENTITIES_USAGE,
                "{rest:?}"
            );
        }
        assert!(
            std::fs::read_dir(&root).unwrap().next().is_none(),
            "usage touches nothing"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// REVIEW (separation lens, 2026-09-17): provisioning an identity must
    /// write under the identity dir and nowhere else. With the human's
    /// `$XDG_CONFIG_HOME` set and holding an `opencode` directory, `ensure`
    /// went through `auto_prime`, which follows that redirection for the
    /// `.config/` row: it detected the HUMAN's OpenCode and wrote aterm's
    /// `AGENTS.md` and command file into it. Measured red before the fix:
    /// `["command", "AGENTS.md"]` in the human's tree.
    #[test]
    fn ensure_never_writes_into_the_humans_xdg_config_home() {
        let scratch = scratch("xdg");
        let xdg = scratch.join("xdg");
        let humans_opencode = xdg.join("opencode");
        std::fs::create_dir_all(&humans_opencode).unwrap();
        let root = scratch.join("identities");
        aterm_log::env::scoped("XDG_CONFIG_HOME", &xdg, || {
            let dir = ensure_in(&root, "worker", true, None).expect("created");
            assert!(
                dir.join(".claude/CLAUDE.md").is_file(),
                "the identity IS primed"
            );
            let leaked: Vec<String> = std::fs::read_dir(&humans_opencode)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            assert!(
                leaked.is_empty(),
                "identity provisioning wrote into the human's $XDG_CONFIG_HOME/opencode: {leaked:?}"
            );
            assert!(
                !dir.join(".config").exists(),
                "and nothing of an agent without a var is invented under the identity"
            );
        });
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// REVIEW (separation lens, 2026-09-17): restore and the verb agree on
    /// what an identity is. `identities link` and `identities forget link`
    /// refuse a symlink in the root (`ERR no such identity`), but
    /// `ensure(create = false)` followed it — so a restore leaf naming `link`
    /// re-injected `CLAUDE_CONFIG_DIR=<root>/link/.claude`, pointing outside
    /// the identities tree, under a name the verb could neither list nor
    /// forget. Now both read the same [`identity_dir`]: a real directory.
    #[test]
    fn restore_and_the_verb_agree_on_what_an_identity_is() {
        let root = scratch("symlink").join("identities");
        std::fs::create_dir_all(&root).unwrap();
        let elsewhere = root.parent().unwrap().join("elsewhere");
        std::fs::create_dir_all(elsewhere.join(".claude")).unwrap();
        std::fs::create_dir_all(elsewhere.join(".codex")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&elsewhere, root.join("link")).unwrap();
        #[cfg(not(unix))]
        return;
        assert_eq!(
            identities_reply(&root, &[], "link"),
            "ERR no such identity link\n",
            "the verb: a symlink is not an identity"
        );
        assert_eq!(
            identities_reply(&root, &[], "forget link confirm=link"),
            "ERR no such identity link\n"
        );
        let restore = ensure_in(&root, "link", false, None);
        assert_eq!(
            restore.as_ref().map_err(io::Error::kind),
            Err(io::ErrorKind::NotFound),
            "restore: the same answer — got {restore:?}, whose env would have been {:?}",
            restore.as_ref().map(|d| env(d)).ok()
        );
        assert!(
            root.join("link").exists() && elsewhere.is_dir(),
            "nothing touched: the link and its target both stand"
        );
        // A file in the root is no identity to either, and a real directory
        // is one to both.
        std::fs::write(root.join("file"), b"").unwrap();
        assert_eq!(
            identities_reply(&root, &[], "file"),
            "ERR no such identity file\n"
        );
        assert_eq!(
            ensure_in(&root, "file", false, None).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        let real = ensure_in(&root, "real", true, None).unwrap();
        assert!(identities_reply(&root, &[], "real").starts_with("OK 3\nreal dir="));
        assert_eq!(ensure_in(&root, "real", false, None).unwrap(), real);
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }
}
