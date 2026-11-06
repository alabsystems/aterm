// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm name`: this machine's name, its NAME (`m8`, or `m0` until it is numbered) and nickname
//! (`docs/DESIGN-machine-names-2026-11-04.md` §5.7), made by the first read that finds none
//! ([`crate::machine_name::ensure`]). `aterm help name` is its page.

use std::ffi::OsString;
use std::process::ExitCode;

/// `aterm name --help`.
pub const USAGE: &str = "\
aterm name — this machine's name: its NAME (`m8`, or `m0` until it is numbered) and nickname

  aterm name [--json]
  aterm name nick <word> | --random  [--dry-run] [--json]

  `aterm name` prints `m8 Heron  class=seeded fleet=in node=<node id>`, or with `--json` one
  object of its fields; the first run to find no `machine-name.toml` makes it. `nick` sets the
  nickname to <word> (one argument: quote `Blue Heron`) or to a noun drawn again, as a chosen
  nickname, and prints the name as it then stands; `--dry-run` changes no nickname (a first
  run still makes the name, as any read does). `aterm help name` has the page. Unix only for
  now.
";

/// What `aterm name …` asks for, decided without running it so a test can read it.
#[derive(Debug, PartialEq, Eq)]
enum Mode {
    /// `-h` or `--help`: the usage, exit 0.
    Help,
    /// The name, as a line or (`--json`) one object.
    Show { json: bool },
    /// `nick`: set the nickname, then show the name as `Show` does.
    Nick { to: Nick, dry_run: bool, json: bool },
}

/// What `aterm name nick` sets the nickname to.
#[derive(Debug, PartialEq, Eq)]
enum Nick {
    /// This word.
    Word(aterm_types::machine_name::Nickname),
    /// `--random`: a noun drawn again.
    Random,
}

fn parse(argv: &[OsString]) -> Result<Mode, String> {
    let mut words = Vec::with_capacity(argv.len());
    for arg in argv {
        words.push(arg.to_str().ok_or("an argument is not UTF-8")?);
    }
    if words.first() == Some(&"nick") {
        return parse_nick(&words[1..]);
    }
    let mut json = false;
    for word in words {
        match word {
            "-h" | "--help" => return Ok(Mode::Help),
            "--json" if json => return Err("`--json` is given twice".to_owned()),
            "--json" => json = true,
            "nick" => return Err("`nick` comes first: `aterm name nick …`".to_owned()),
            arg => return Err(format!("`{arg}` is no `aterm name` argument")),
        }
    }
    Ok(Mode::Show { json })
}

fn parse_nick(words: &[&str]) -> Result<Mode, String> {
    use aterm_types::machine_name::Nickname;
    if words.iter().any(|word| matches!(*word, "-h" | "--help")) {
        return Ok(Mode::Help);
    }
    let (mut to, mut dry_run, mut json) = (None, false, false);
    for &word in words {
        let flag = match word {
            "--dry-run" => &mut dry_run,
            "--json" => &mut json,
            _ => {
                let next = if word == "--random" {
                    Nick::Random
                } else if word.starts_with('-') {
                    return Err(format!("`{word}` is no `aterm name nick` argument"));
                } else {
                    Nick::Word(
                        Nickname::parse(word)
                            .map_err(|why| format!("`{word}` is no nickname: {why}"))?,
                    )
                };
                if to.replace(next).is_some() {
                    return Err("`aterm name nick` takes one word or `--random`".to_owned());
                }
                continue;
            }
        };
        if std::mem::replace(flag, true) {
            return Err(format!("`{word}` is given twice"));
        }
    }
    let to = to.ok_or("`aterm name nick` takes a word or `--random`")?;
    Ok(Mode::Nick { to, dry_run, json })
}

/// `aterm name …`: `argv[1..]` in, exit code out (2 for a usage error or a platform that keeps no
/// machine name, 1 for a name that cannot be read, made or changed).
pub fn main_entry(argv: Vec<OsString>) -> ExitCode {
    match parse(&argv) {
        Ok(Mode::Help) => aterm_uds::stdout::answer_exit("aterm name", USAGE, 0),
        Ok(Mode::Show { json }) => show(json),
        Ok(Mode::Nick { to, dry_run, json }) => nick(&to, dry_run, json),
        Err(why) => {
            eprintln!("aterm name: {why}\n(`aterm name --help` says what it takes)");
            ExitCode::from(2)
        }
    }
}

#[cfg(unix)]
/// Who asks for a change from this command (§5.7): an agent when it runs in an aterm session whose
/// `status` names an agent program, or reads an agent's state there (`agent=`, which aterm sets
/// for one it told by its screen, such as Claude Code run by `node`); a person otherwise, and
/// where that status cannot be read. A command a person types through an agent (Claude Code's
/// `!`) is the agent's.
fn caller(status: Option<&str>) -> aterm_net::fleet_member::name_record::ByKind {
    use crate::supervise::screen::status_field;
    use aterm_net::fleet_member::name_record::ByKind;
    let Some(reply) = status else {
        return ByKind::Person;
    };
    let program = status_field(reply, "program").map(aterm_codec::pct::decode);
    if program.is_some_and(|program| aterm_phase::program_of(&program).is_some())
        || status_field(reply, "agent").is_some()
    {
        ByKind::Agent
    } else {
        ByKind::Person
    }
}

#[cfg(unix)]
/// How long this command waits for its session's `status` (§5.7's `by`), seconds: a session
/// whose window does not answer costs the caller's kind, never the change.
const STATUS_WAIT_S: &str = "2";

#[cfg(unix)]
/// This session's `status` reply, where this command runs in an aterm session, within
/// [`STATUS_WAIT_S`].
fn session_status() -> Option<String> {
    std::env::var_os("ATERM_SESSION_ID")?;
    let exe = crate::own_exe().ok()?;
    crate::CtlClient::own_at(exe, None)
        .run(&["--timeout", STATUS_WAIT_S, "@self", "status"])
        .ok()
}

#[cfg(unix)]
fn show(json: bool) -> ExitCode {
    let Some(state) = aterm_types::dirs::state_dir() else {
        eprintln!("aterm name: no state directory: $HOME is not set");
        return ExitCode::from(1);
    };
    let roster = crate::machine_name::vault_roster_label;
    match unix::report(&state, json, roster, aterm_netplace::unix_ms_now()) {
        Ok((text, narrowed)) => {
            if let Some(was) = narrowed {
                eprintln!(
                    "aterm name: {} was open to other users ({was:o}); it is 0700 now",
                    state.display()
                );
            }
            aterm_uds::stdout::answer_exit("aterm name", &text, 0)
        }
        Err(why) => {
            eprintln!("aterm name: {why}");
            ExitCode::from(1)
        }
    }
}

#[cfg(not(unix))]
fn show(_json: bool) -> ExitCode {
    eprintln!("aterm name: this platform keeps no machine name yet");
    ExitCode::from(2)
}

#[cfg(unix)]
fn nick(to: &Nick, dry_run: bool, json: bool) -> ExitCode {
    use crate::machine_name::NickTo;
    let to = match to {
        Nick::Word(word) => NickTo::Word(word.clone()),
        Nick::Random => NickTo::Random,
    };
    let Some(state) = aterm_types::dirs::state_dir() else {
        eprintln!("aterm name: no state directory: $HOME is not set");
        return ExitCode::from(1);
    };
    let kind = caller(session_status().as_deref());
    let roster = crate::machine_name::vault_roster_label;
    match unix::renick(
        &state,
        &to,
        kind,
        json,
        dry_run,
        roster,
        aterm_netplace::unix_ms_now(),
    ) {
        Ok((text, notes)) => {
            for note in notes {
                eprintln!("aterm name: {}", note.text(&state));
            }
            aterm_uds::stdout::answer_exit("aterm name", &text, 0)
        }
        Err(why) => {
            eprintln!("aterm name: {why}");
            ExitCode::from(1)
        }
    }
}

#[cfg(not(unix))]
fn nick(_to: &Nick, _dry_run: bool, _json: bool) -> ExitCode {
    show(false)
}

#[cfg(unix)]
mod unix {
    use std::io;
    use std::path::Path;

    use aterm_net::fleet_member::read_node_identity;

    use crate::machine_name::file::{MachineName, class_word, migration_word, nick_class_word};
    use crate::machine_name::{Origin, ensure_in_view};

    /// The report of this machine's name in `state`, made first where no file is there
    /// ([`ensure_in_view`], which reads `roster` only then, and only when neither `fleet-name`
    /// nor Floor names the claim, so a read of a name already made opens no vault), with the
    /// mode `state` was narrowed from when it was open to others.
    ///
    /// # Errors
    /// `state` cannot be made private, the node key cannot be read, or the name cannot be read
    /// or made ([`ensure_in_view`]).
    pub(super) fn report(
        state: &Path,
        json: bool,
        roster: impl FnOnce() -> io::Result<Option<String>>,
        now_ms: u64,
    ) -> Result<(String, Option<u32>), String> {
        let narrowed =
            aterm_uds::private_file::ensure_private_dir(state).map_err(|e| e.to_string())?;
        let me = read_node_identity(state).map_err(|e| e.to_string())?;
        let (name, origin) = ensure_in_view(state, roster, now_ms).map_err(|e| e.to_string())?;
        let shown = Shown {
            name: &name,
            origin,
            fleet: aterm_net::fleet::read_key_file(state).is_ok(),
            node: me.as_ref().map(|me| me.node()),
        };
        Ok((if json { shown.json() } else { shown.line() }, narrowed))
    }

    /// What `aterm name nick` says beside the name, on stderr.
    #[derive(Debug, PartialEq, Eq)]
    pub(super) enum Note {
        /// The state directory was open to others, in this mode, and is 0700 now.
        Narrowed(u32),
        /// The nickname, in display form, was held already as chosen.
        Already(String),
        /// Another machine in view goes by this nickname too.
        Shared(String),
        /// A dry run: no nickname changed.
        DryRun,
    }

    impl Note {
        pub(super) fn text(&self, state: &Path) -> String {
            match self {
                Self::Narrowed(was) => format!(
                    "{} was open to other users ({was:o}); it is 0700 now",
                    state.display()
                ),
                Self::Already(nick) => format!("the nickname is {nick} already"),
                Self::Shared(nick) => format!("{nick} is another machine's nickname too"),
                Self::DryRun => "a dry run: no nickname changed".to_owned(),
            }
        }
    }

    /// [`crate::machine_name::set_nick`] as `aterm name nick` runs it, then the report of the name
    /// as it stands (or would, after a dry run), with the [`Note`]s said beside it.
    ///
    /// # Errors
    /// As [`report`], and the change cannot be written.
    pub(super) fn renick(
        state: &Path,
        to: &crate::machine_name::NickTo,
        kind: aterm_net::fleet_member::name_record::ByKind,
        json: bool,
        dry_run: bool,
        roster: impl FnOnce() -> io::Result<Option<String>>,
        now_ms: u64,
    ) -> Result<(String, Vec<Note>), String> {
        let narrowed =
            aterm_uds::private_file::ensure_private_dir(state).map_err(|e| e.to_string())?;
        let set = crate::machine_name::set_nick(state, to, kind, roster, now_ms, dry_run)
            .map_err(|e| e.to_string())?;
        let me = read_node_identity(state).map_err(|e| e.to_string())?;
        let mut notes: Vec<Note> = narrowed.into_iter().map(Note::Narrowed).collect();
        let nick = set.name.nick.display();
        if !set.changed {
            notes.push(Note::Already(nick.clone()));
        }
        if set.shared {
            notes.push(Note::Shared(nick));
        }
        if dry_run {
            notes.push(Note::DryRun);
        }
        let shown = Shown {
            name: &set.name,
            origin: set.origin,
            fleet: aterm_net::fleet::read_key_file(state).is_ok(),
            node: me.as_ref().map(|me| me.node()),
        };
        Ok((if json { shown.json() } else { shown.line() }, notes))
    }

    /// What `aterm name` says of `name`.
    pub(super) struct Shown<'a> {
        pub(super) name: &'a MachineName,
        pub(super) origin: Origin,
        /// This machine holds a fleet key that reads ([`aterm_net::fleet::read_key_file`]).
        pub(super) fleet: bool,
        /// Its fleet node, once it has one.
        pub(super) node: Option<&'a str>,
    }

    impl Shown<'_> {
        /// `m8 Heron  class=seeded fleet=in node=<node>`, `migration=` before `node=` while one
        /// is not finished.
        pub(super) fn line(&self) -> String {
            let mut line = format!(
                "{}  class={} fleet={}",
                self.name.title(),
                class_word(self.name.class),
                if self.fleet { "in" } else { "out" }
            );
            if let Some(migration) = self.name.migration {
                line.push_str(" migration=");
                line.push_str(migration_word(migration));
            }
            if let Some(node) = self.node {
                line.push_str(" node=");
                line.push_str(node);
            }
            line.push('\n');
            line
        }

        /// One object of every field, keys in order.
        pub(super) fn json(&self) -> String {
            use aterm_json::Value;
            let name = self.name;
            let text = |word: &str| Value::String(word.to_owned());
            let mut object = aterm_json::Map::new();
            object.insert("class".to_owned(), text(class_word(name.class)));
            object.insert("fleet".to_owned(), Value::Bool(self.fleet));
            object.insert(
                "made".to_owned(),
                text(match self.origin {
                    Origin::Kept => "kept",
                    Origin::Floor => "floor",
                    Origin::Evidence => "evidence",
                    Origin::Drawn => "drawn",
                }),
            );
            object.insert(
                "migration".to_owned(),
                name.migration
                    .map_or(Value::Null, |migration| text(migration_word(migration))),
            );
            object.insert("name".to_owned(), text(&name.name()));
            object.insert("nick".to_owned(), text(name.nick.as_str()));
            object.insert(
                "nick_class".to_owned(),
                text(nick_class_word(name.nick_class)),
            );
            object.insert("node".to_owned(), self.node.map_or(Value::Null, text));
            object.insert(
                "number".to_owned(),
                name.num
                    .map_or(Value::Null, |num| Value::from(u64::from(num.get()))),
            );
            object.insert("rev".to_owned(), Value::from(name.rev));
            object.insert("since".to_owned(), Value::from(name.since));
            object.insert("title".to_owned(), text(&name.title()));
            let mut json = aterm_json::to_string(&Value::Object(object)).unwrap_or_default();
            json.push('\n');
            json
        }
    }
}

#[cfg(all(test, unix))]
mod tests;
