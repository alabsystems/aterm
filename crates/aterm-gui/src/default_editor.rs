// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! amacs, aterm's own editor, as the default `EDITOR` in aterm's tabs
//! (docs/DESIGN-amacs-2026-10-19.md §8.3–8.4, slice B3).
//!
//! aterm never sets `EDITOR` over a person's own. A tab's environment carries the hint
//! `ATERM_DEFAULT_EDITOR=amacs` ([`settle`]), and the tab's shell integration exports
//! `EDITOR=amacs` once the person's startup files have run, only when `EDITOR` and `VISUAL` are
//! both still empty and `amacs` resolves on `PATH`; it then drops the hint. A session that runs
//! no startup files before its program (a `-e` command, cmd.exe) gets `EDITOR` here, by the same
//! rule over the environment it inherits; an interactive shell aterm has no script for gets
//! none, since its startup files run after anything set here and a `VISUAL` set there could not
//! stop it ([`decides`], [`settle_session`]). `VISUAL` is never set: a program that reads
//! `VISUAL` first falls through to `EDITOR`, and a later `export EDITOR=…` in a shell still
//! decides. The hint is a deny row (`aterm_types::domain::ENV_DENY_VARS`), and an inherited
//! `EDITOR=amacs` an outer aterm's tab exported is dropped too
//! (`aterm_types::domain::is_handed_down_editor`), so an aterm started from a tab derives its
//! own under its own settings.

use crate::app_config::Config;
use aterm_core::shell_integration::{ShellType, reads_default_editor_hint};
use aterm_types::domain::{ENV_ATERM_CHILD, ENV_DEFAULT_EDITOR, is_handed_down_editor};
use std::ffi::OsString;

/// The word `EDITOR` gets: the `amacs` alias of aterm's own executable, bare, because a bundle
/// path holds spaces that `sh -c "$EDITOR file"` would split.
pub(crate) const EDITOR_WORD: &str = aterm_types::domain::DEFAULT_EDITOR_WORD;

/// `[amacs]` as the window holds it: the engine's one reader's settings and its notes.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub(crate) struct AmacsPolicy {
    pub(crate) config: amacs::config::AmacsConfig,
    pub(crate) notes: Vec<String>,
}

impl AmacsPolicy {
    /// What `text`, the whole `aterm.toml`, writes under `[amacs]`.
    pub(crate) fn read(text: &str) -> Self {
        let (config, notes) = amacs::config::AmacsConfig::from_aterm_toml(text);
        Self { config, notes }
    }

    /// The reader's notes as config notices (`config amacs.<key>: …`).
    pub(crate) fn notices(&self) -> Vec<String> {
        self.notes
            .iter()
            .map(|note| format!("config {note}"))
            .collect()
    }
}

/// Whether a tab gets the hint: `[amacs] default_editor` is on, `[packages] exclude` does not
/// name amacs, the session is not contained (a sandboxed shell may not reach the window that
/// would show the file), and `amacs` is a file in a directory of `path`, the `PATH` the tab is
/// handed.
pub(crate) fn wants_hint(
    on: bool,
    excluded: bool,
    contained: bool,
    path: Option<&std::ffi::OsStr>,
    is_file: impl Fn(&std::path::Path) -> bool,
) -> bool {
    if !on || excluded || contained {
        return false;
    }
    let Some(path) = path else {
        return false;
    };
    let name = format!("{EDITOR_WORD}{}", std::env::consts::EXE_SUFFIX);
    std::env::split_paths(path)
        .filter(|dir| dir.is_absolute())
        .any(|dir| is_file(&dir.join(&name)))
}

/// Put the hint in a factory's baseline environment `env_add`, or take it out, for `config`
/// and a session `contained` or not: at launch and at every config reload, so a Settings
/// change reaches the next tab. The `PATH` judged is the one `env_add` hands a tab, else the
/// one this process has.
pub(crate) fn settle(env_add: &mut Vec<(String, String)>, config: &Config, contained: bool) {
    let excluded = config
        .packages
        .as_ref()
        .and_then(|p| p.exclude.as_ref())
        .is_some_and(|names| names.iter().any(|name| name.trim() == EDITOR_WORD));
    let inherited = std::env::var_os("PATH");
    let path = env_add
        .iter()
        .rev()
        .find(|(key, _)| key == "PATH")
        .map(|(_, value)| std::ffi::OsString::from(value))
        .or(inherited);
    let wanted = wants_hint(
        config.amacs.config.default_editor,
        excluded,
        contained,
        path.as_deref(),
        std::path::Path::is_file,
    );
    place_hint(env_add, wanted);
}

/// [`settle`]'s edit of `env_add`: exactly one hint when `wanted`, none otherwise.
pub(crate) fn place_hint(env_add: &mut Vec<(String, String)>, wanted: bool) {
    env_add.retain(|(key, _)| key != ENV_DEFAULT_EDITOR);
    if wanted {
        env_add.push((ENV_DEFAULT_EDITOR.to_string(), EDITOR_WORD.to_string()));
    }
}

/// Where a new session's default `EDITOR` is decided (docs/DESIGN-amacs-2026-10-19.md §8.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decides {
    /// In the shell's integration script, after the person's startup files: the hint stays.
    Script,
    /// Here, at spawn, over what the child inherits: nothing of the person's runs before the
    /// program (a `-e` command; cmd.exe, whose settings are the environment aterm hands on).
    Spawn,
    /// Nowhere: an interactive shell no script of aterm's runs in reads its startup files after
    /// anything set here, so a `VISUAL` set there could not stop it. The hint goes, and
    /// `EDITOR` is left as the session inherits it.
    Nowhere,
}

/// Where the default is decided for a session that `runs_command` (a non-empty `-e` argv) or
/// starts a shell whose integration aterm prepared (`integrated`, `None` when it has none:
/// an unknown shell, `--no-shell-integration`, a loader that could not be written).
pub(crate) fn decides(runs_command: bool, integrated: Option<ShellType>) -> Decides {
    if runs_command {
        return Decides::Spawn;
    }
    match integrated {
        Some(shell) if reads_default_editor_hint(shell) => Decides::Script,
        Some(ShellType::Cmd) => Decides::Spawn,
        _ => Decides::Nowhere,
    }
}

/// What the person set `key` (`EDITOR` or `VISUAL`) to in aterm's own environment, as `lookup`
/// reads it: an `EDITOR` an outer aterm's tab handed down is not theirs (the PTY spawn drops
/// it, [`is_handed_down_editor`]).
pub(crate) fn persons_own(
    key: &str,
    lookup: &dyn Fn(&str) -> Option<OsString>,
) -> Option<OsString> {
    let value = lookup(key)?;
    let handed_down =
        key == "EDITOR" && is_handed_down_editor(&value, lookup(ENV_ATERM_CHILD).as_deref());
    (!handed_down).then_some(value)
}

/// Settle the hint in a new session's `env_add` where [`decides`] says. [`Decides::Spawn`]
/// turns it into `EDITOR=amacs` when neither `EDITOR` nor `VISUAL` is set, non-empty, in what
/// the child gets: `env_add` itself, else the person's own in `lookup` (this process's
/// environment, [`persons_own`]); [`Decides::Nowhere`] drops it; [`Decides::Script`] leaves it
/// for the shell. Only a script reads the hint, so it leaves `env_add` in the other two.
pub(crate) fn settle_session(
    env_add: &mut Vec<(String, String)>,
    decides: Decides,
    lookup: &dyn Fn(&str) -> Option<OsString>,
) {
    if decides == Decides::Script {
        return;
    }
    let Some(at) = env_add
        .iter()
        .position(|(key, _)| key == ENV_DEFAULT_EDITOR)
    else {
        return;
    };
    let (_, word) = env_add.remove(at);
    env_add.retain(|(key, _)| key != ENV_DEFAULT_EDITOR);
    if decides == Decides::Nowhere {
        return;
    }
    let unset = |key: &str| {
        env_add
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| OsString::from(v))
            .or_else(|| persons_own(key, lookup))
            .is_none_or(|v| v.is_empty())
    };
    if unset("EDITOR") && unset("VISUAL") {
        env_add.push(("EDITOR".to_string(), word));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BIN: &str = "/Applications/aterm.app/Contents/MacOS";

    fn path_of(dirs: &[&str]) -> std::ffi::OsString {
        std::env::join_paths(dirs).expect("a PATH")
    }

    /// `amacs` under the bundle dir, and nowhere else.
    fn laid(path: &std::path::Path) -> bool {
        path == std::path::Path::new(BIN)
            .join(format!("{EDITOR_WORD}{}", std::env::consts::EXE_SUFFIX))
    }

    /// The hint is wanted only when every condition of §8.3 holds.
    /// NEGATIVE CONTROL: each condition broken alone (the switch off, `exclude = ["amacs"]`, a
    /// contained session, no `amacs` on the PATH, no PATH, a relative entry naming the bundle
    /// dir's twin) wants none.
    #[test]
    fn the_hint_is_wanted_only_when_every_condition_holds() {
        let path = path_of(&["/usr/bin", BIN, "/bin"]);
        assert!(wants_hint(true, false, false, Some(&path), laid));
        assert!(!wants_hint(false, false, false, Some(&path), laid));
        assert!(!wants_hint(true, true, false, Some(&path), laid));
        assert!(!wants_hint(true, false, true, Some(&path), laid));
        let without = path_of(&["/usr/bin", "/bin"]);
        assert!(!wants_hint(true, false, false, Some(&without), laid));
        assert!(!wants_hint(true, false, false, None, laid));
        let relative = path_of(&["Applications/aterm.app/Contents/MacOS"]);
        assert!(!wants_hint(true, false, false, Some(&relative), |p| {
            p.ends_with(format!("Contents/MacOS/{EDITOR_WORD}"))
        }));
    }

    /// [`settle`] reads the opt-out and the exclusion from the config, and leaves one hint or
    /// none however often it runs.
    /// NEGATIVE CONTROL: `[amacs] default_editor = false`, and `[packages] exclude = ["amacs"]`,
    /// each take a placed hint out.
    #[test]
    fn settle_follows_the_config_and_never_doubles_the_hint() {
        let dir = aterm_tempfile::tempdir().expect("a scratch bin dir");
        let amacs = dir
            .path()
            .join(format!("{EDITOR_WORD}{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&amacs, "").expect("an amacs file");
        let path = std::env::join_paths([dir.path()]).expect("a PATH");
        let mut env = vec![(
            "PATH".to_string(),
            path.to_str().expect("UTF-8").to_string(),
        )];
        let hints = |env: &[(String, String)]| {
            env.iter()
                .filter(|(k, v)| k == ENV_DEFAULT_EDITOR && v == EDITOR_WORD)
                .count()
        };
        let on = Config::parse("").expect("an empty config");
        settle(&mut env, &on, false);
        settle(&mut env, &on, false);
        assert_eq!(hints(&env), 1, "{env:?}");
        settle(&mut env, &on, true);
        assert_eq!(hints(&env), 0, "a contained session gets none: {env:?}");
        settle(&mut env, &on, false);
        let off = Config::parse("[amacs]\ndefault_editor = false\n").expect("the opt-out");
        settle(&mut env, &off, false);
        assert_eq!(hints(&env), 0, "the opt-out takes it out: {env:?}");
        settle(&mut env, &on, false);
        let excluded = Config::parse("[packages]\nexclude = [\"amacs\"]\n").expect("exclude");
        settle(&mut env, &excluded, false);
        assert_eq!(hints(&env), 0, "the exclusion takes it out: {env:?}");
        assert!(env.iter().any(|(k, _)| k == "PATH"), "PATH stays: {env:?}");
    }

    fn hinted() -> Vec<(String, String)> {
        vec![
            ("TERM".to_string(), "xterm-256color".to_string()),
            (ENV_DEFAULT_EDITOR.to_string(), EDITOR_WORD.to_string()),
        ]
    }

    fn editor_of(env: &[(String, String)]) -> Vec<String> {
        env.iter()
            .filter(|(k, _)| k == "EDITOR")
            .map(|(_, v)| v.clone())
            .collect()
    }

    /// Only the four shells whose script reads the hint keep it; a `-e` command and cmd.exe
    /// decide at spawn; an interactive shell with no script decides nowhere.
    /// NEGATIVE CONTROL: an unknown shell, a shell with integration off (`None`) and wsl.exe,
    /// whose Linux side never sees the hint, are not handed to a script or set at spawn.
    #[test]
    fn the_default_is_decided_after_the_startup_files_or_not_at_all() {
        for shell in [
            ShellType::Zsh,
            ShellType::Bash,
            ShellType::Fish,
            ShellType::PowerShell,
        ] {
            assert_eq!(decides(false, Some(shell)), Decides::Script, "{shell:?}");
            assert_eq!(decides(true, Some(shell)), Decides::Spawn, "{shell:?} -e");
        }
        assert_eq!(decides(true, None), Decides::Spawn);
        assert_eq!(decides(false, Some(ShellType::Cmd)), Decides::Spawn);
        assert_eq!(decides(false, None), Decides::Nowhere);
        assert_eq!(decides(false, Some(ShellType::Unknown)), Decides::Nowhere);
        assert_eq!(decides(false, Some(ShellType::Wsl)), Decides::Nowhere);
    }

    /// A session that runs no startup files gets `EDITOR=amacs` in place of the hint, unless
    /// the person's own `EDITOR` or `VISUAL` reaches it; an interactive shell no script runs in
    /// gets the hint taken away and nothing set; a script's shell keeps the hint.
    /// NEGATIVE CONTROL: an inherited `EDITOR=vi`, an inherited `VISUAL=nano`, and an `EDITOR`
    /// in `env_add` each keep `EDITOR=amacs` out; with no hint nothing is added.
    #[test]
    fn a_session_gets_editor_only_where_the_person_set_none_before_it_runs() {
        let none = |_: &str| None;
        let mut env = hinted();
        settle_session(&mut env, Decides::Spawn, &none);
        assert_eq!(editor_of(&env), [EDITOR_WORD]);
        assert!(env.iter().all(|(k, _)| k != ENV_DEFAULT_EDITOR), "{env:?}");

        let empty = |_: &str| Some(OsString::new());
        let mut env = hinted();
        settle_session(&mut env, Decides::Spawn, &empty);
        assert_eq!(editor_of(&env), [EDITOR_WORD], "an empty EDITOR is unset");

        for (key, value) in [("EDITOR", "vi"), ("VISUAL", "nano")] {
            let own = move |k: &str| (k == key).then(|| OsString::from(value));
            let mut env = hinted();
            settle_session(&mut env, Decides::Spawn, &own);
            assert!(editor_of(&env).is_empty(), "{key}={value} wins: {env:?}");
            assert!(env.iter().all(|(k, _)| k != ENV_DEFAULT_EDITOR), "{env:?}");
        }
        let mut env = hinted();
        env.push(("EDITOR".to_string(), "emacs".to_string()));
        settle_session(&mut env, Decides::Spawn, &none);
        assert_eq!(editor_of(&env), ["emacs"]);

        let mut env = vec![("TERM".to_string(), "xterm-256color".to_string())];
        settle_session(&mut env, Decides::Spawn, &none);
        assert!(editor_of(&env).is_empty(), "no hint, no EDITOR: {env:?}");

        let mut env = hinted();
        settle_session(&mut env, Decides::Nowhere, &none);
        assert!(editor_of(&env).is_empty(), "no script, no default: {env:?}");
        assert!(env.iter().all(|(k, _)| k != ENV_DEFAULT_EDITOR), "{env:?}");

        let mut env = hinted();
        settle_session(&mut env, Decides::Script, &none);
        assert_eq!(env, hinted(), "the script reads it");
    }

    /// A person's `EDITOR` or `VISUAL` that is not UTF-8 is still theirs.
    /// NEGATIVE CONTROL: the same lookup with the value removed sets `EDITOR=amacs`.
    #[cfg(unix)]
    #[test]
    fn a_non_utf8_editor_or_visual_is_the_persons() {
        use std::os::unix::ffi::OsStringExt as _;
        for key in ["EDITOR", "VISUAL"] {
            let own = move |k: &str| (k == key).then(|| OsString::from_vec(b"/opt/\xe9d".to_vec()));
            let mut env = hinted();
            settle_session(&mut env, Decides::Spawn, &own);
            assert!(editor_of(&env).is_empty(), "{key} is set: {env:?}");
        }
        let mut env = hinted();
        settle_session(&mut env, Decides::Spawn, &|_| None);
        assert_eq!(editor_of(&env), [EDITOR_WORD]);
    }

    /// An `EDITOR=amacs` an outer aterm's tab handed down is not the person's: this hop decides
    /// again, so its opt-out holds.
    /// NEGATIVE CONTROL: the same `EDITOR=amacs` with no `ATERM_CHILD` (the person's own,
    /// from outside any aterm) keeps it out of `env_add`, and `VISUAL` is never discounted.
    #[test]
    fn an_editor_an_outer_aterm_tab_handed_down_is_decided_again() {
        let from_tab = |k: &str| match k {
            "EDITOR" => Some(OsString::from("amacs")),
            "ATERM_CHILD" => Some(OsString::from("1")),
            _ => None,
        };
        assert_eq!(persons_own("EDITOR", &from_tab), None);
        assert_eq!(persons_own("ATERM_CHILD", &from_tab), Some("1".into()));
        let mut env = hinted();
        settle_session(&mut env, Decides::Spawn, &from_tab);
        assert_eq!(editor_of(&env), [EDITOR_WORD], "this hop's own default");

        let outside = |k: &str| (k == "EDITOR").then(|| OsString::from("amacs"));
        assert_eq!(persons_own("EDITOR", &outside), Some("amacs".into()));
        let mut env = hinted();
        settle_session(&mut env, Decides::Spawn, &outside);
        assert!(
            editor_of(&env).is_empty(),
            "the person's own, inherited: {env:?}"
        );

        let visual = |k: &str| match k {
            "VISUAL" => Some(OsString::from("amacs")),
            "ATERM_CHILD" => Some(OsString::from("1")),
            _ => None,
        };
        assert_eq!(persons_own("VISUAL", &visual), Some("amacs".into()));
    }

    /// A config reload settles the hint on the factory every later tab is spawned from: the
    /// opt-out takes it out and retires the spares (their fingerprint moves), and the default
    /// puts it back.
    /// NEGATIVE CONTROL: the opt-out generation leaves no hint where one was.
    #[test]
    fn a_config_reload_settles_the_hint_on_the_spawn_factory() {
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let bin = scratch.path().join("bin");
        std::fs::create_dir(&bin).expect("a bin dir");
        std::fs::write(
            bin.join(format!("{EDITOR_WORD}{}", std::env::consts::EXE_SUFFIX)),
            "",
        )
        .expect("an amacs file");
        let mut app = crate::App::headless_for_test();
        app.session_factory.sandbox_wrap = None;
        app.session_factory.env_add = vec![
            ("PATH".to_string(), bin.to_str().expect("UTF-8").to_string()),
            (ENV_DEFAULT_EDITOR.to_string(), EDITOR_WORD.to_string()),
        ];
        let hints = |app: &crate::App| {
            app.session_factory
                .env_add
                .iter()
                .filter(|(k, _)| k == ENV_DEFAULT_EDITOR)
                .count()
        };
        let before = app.session_factory.spawn_fingerprint();
        apply(
            &mut app,
            scratch.path(),
            "[amacs]\ndefault_editor = false\n",
        );
        assert_eq!(hints(&app), 0, "{:?}", app.session_factory.env_add);
        assert_ne!(app.session_factory.spawn_fingerprint(), before);
        apply(&mut app, scratch.path(), "");
        assert_eq!(hints(&app), 1, "{:?}", app.session_factory.env_add);
    }

    /// Apply `text` as `aterm.toml` through the window's admission path, parsed as the config
    /// watcher parses it.
    fn apply(app: &mut crate::App, dir: &std::path::Path, text: &str) {
        let path = dir.join("aterm.toml");
        std::fs::write(&path, text).expect("write config");
        let observation =
            crate::native_config_service::VersionedConfigService::observe_path(&path, false)
                .expect("observe");
        let config = Config::parse(text).expect("valid config");
        let assets = config.resolve_asset_catalog();
        let path_feed_fps = config.path_feed_fingerprints();
        let sparkle = config.prepare_sparkle_runtime();
        app.apply_prepared_config_generation(
            crate::native_font_catalog::PreparedConfigGeneration {
                observation,
                config,
                values: std::collections::BTreeMap::new(),
                disk_faults: std::sync::Arc::default(),
                assets,
                path_feed_fps,
                sparkle,
                fonts: None,
                warnings: Vec::new(),
                font_refusals: Vec::new(),
                reason: crate::app_colorscheme::ThemeChange::ConfigReload,
            },
        );
    }

    /// A `-e` command spawned through `spawn_session` gets `EDITOR=amacs` for the hint and not
    /// the hint itself, and one whose `VISUAL` is set gets neither. `EDITOR` and `VISUAL` are set
    /// (empty, or the person's) in `env_add` so this process's own environment cannot decide.
    /// NEGATIVE CONTROL: `VISUAL=nano` in what the command gets keeps `EDITOR` empty.
    #[cfg(unix)]
    #[test]
    fn a_command_spawned_through_the_factory_gets_the_default_editor() {
        struct Wakes(crate::spawn::WakePost);
        impl crate::spawn::WakeTarget for Wakes {
            fn wake_post(&self) -> crate::spawn::WakePost {
                std::sync::Arc::clone(&self.0)
            }
        }
        let app = crate::App::headless_for_test();
        for (id, visual, shown) in [
            (961, "", "E=[amacs] V=[] H=[]"),
            (962, "nano", "E=[] V=[nano] H=[]"),
        ] {
            let (posted, exits) = std::sync::mpsc::channel();
            let target = Wakes(std::sync::Arc::new(move |wake| {
                if let crate::Wake::Exit { session, .. } = wake {
                    let _ = posted.send(session);
                }
                true
            }));
            let mut factory = app.session_factory.clone();
            factory.integrate = false;
            factory.byte_arm = false;
            factory.env_add = vec![
                ("EDITOR".to_string(), String::new()),
                ("VISUAL".to_string(), visual.to_string()),
                (ENV_DEFAULT_EDITOR.to_string(), EDITOR_WORD.to_string()),
            ];
            factory.exec_command = Some(
                [
                    "/bin/sh",
                    "-c",
                    "printf 'E=[%s] V=[%s] H=[%s]\\n' \"$EDITOR\" \"$VISUAL\" \"$ATERM_DEFAULT_EDITOR\"",
                ]
                .into_iter()
                .map(Into::into)
                .collect(),
            );
            let session = crate::spawn::spawn_session(
                id,
                crate::WindowId(0),
                24,
                120,
                None,
                &factory,
                &target,
                None,
                None,
                None,
                None,
            )
            .expect("the command starts");
            assert_eq!(
                exits.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
                Ok(id),
                "the command ends"
            );
            let seen = aterm_spec::wait::within(
                aterm_spec::wait::MUST_HAPPEN,
                std::time::Duration::ZERO,
                || {
                    let terminal = crate::term_lock(&session.term);
                    let rows: String = (0..24).filter_map(|row| terminal.row_text(row)).collect();
                    rows.contains(shown).then_some(())
                },
            );
            let terminal = crate::term_lock(&session.term);
            let rows: Vec<String> = (0..24).filter_map(|row| terminal.row_text(row)).collect();
            drop(terminal);
            assert!(seen.is_some(), "want {shown:?}: {rows:?}");
            let _ = session.reap_child();
        }
    }

    /// The Settings row writes the key the engine's parser reads, as a TOML boolean, seeds from
    /// it, and sits with the shell rows, taking effect in new tabs.
    /// NEGATIVE CONTROL: the row's seed on a config with no `[amacs]` is `true`, and writing
    /// `true` over the opt-out reads back on.
    #[test]
    fn the_settings_row_writes_the_key_the_parser_reads() {
        use crate::prefs::{self, EDIT_AMACS_DEFAULT_EDITOR as KEY};
        let seed = |cfg: &Config| {
            prefs::editable_fields(cfg)
                .into_iter()
                .find(|f| f.key == KEY)
                .and_then(|f| f.seed)
        };
        assert_eq!(
            seed(&Config::parse("").expect("empty")).as_deref(),
            Some("true")
        );
        let off = prefs::apply_prefs_edits("", &[(KEY, Some("false".to_string()))])
            .expect("the row writes");
        assert!(off.lines().any(|l| l.trim() == "[amacs]"), "{off}");
        assert!(
            off.lines().any(|l| l.trim() == "default_editor = false"),
            "a TOML boolean, never a string: {off}"
        );
        let saved = Config::parse(&off).expect("parses");
        assert!(!saved.amacs.config.default_editor, "{off}");
        assert_eq!(seed(&saved).as_deref(), Some("false"));
        let on = prefs::apply_prefs_edits(&off, &[(KEY, Some("true".to_string()))])
            .expect("the row writes");
        assert!(
            Config::parse(&on)
                .expect("parses")
                .amacs
                .config
                .default_editor
        );

        assert_eq!(prefs::edit_kind(KEY), prefs::EditKind::Bool);
        assert_eq!(prefs::section_of(KEY), prefs::section_of(prefs::EDIT_SHELL));
        assert_eq!(prefs::group_of(KEY), prefs::group_of(prefs::EDIT_SHELL));
        assert_eq!(
            prefs::application_timing(KEY),
            prefs::application_timing(prefs::EDIT_SHELL)
        );
    }

    /// `[amacs]` reaches the window through `Config::parse` and the stand-in a broken file
    /// gets, and its notes are config notices.
    #[test]
    fn the_window_reads_amacs_through_the_one_parser() {
        let off = Config::parse("[amacs]\ndefault_editor = false\n").expect("parses");
        assert!(!off.amacs.config.default_editor);
        assert!(off.amacs.notices().is_empty());
        let stand_in = Config::stand_in("[amacs]\ndefault_editor = false\n[x\n");
        assert!(
            stand_in.amacs.config.default_editor,
            "not TOML: the default"
        );
        let bad = Config::parse("[amacs]\ndefault_editor = \"off\"\n").expect("parses");
        assert!(bad.amacs.config.default_editor);
        assert_eq!(
            bad.amacs.notices(),
            [
                "config amacs.default_editor: wants true or false, not a TOML string; amacs \
              stays the default editor"
            ]
        );
    }
}
