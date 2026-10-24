// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The front door's routes that run before it forks a window or a session: the argv0 aliases
//! and the first words it answers and exits on. `crates/aterm`'s `main` dispatches by them, and
//! doctor's census (`aterm_ctl::census`) reads another process's route by the same ones.

use crate::{DIAG_COMMANDS, Verb};

/// How an invocation under an argv0 alias name is served. On Windows the Start-Menu shortcut and
/// the jump list launch the `aterm-gui.exe` alias, so the windowing verbs and the routing policy
/// work under it too.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum AliasRoute {
    /// `aterm-ctl` — the control client.
    Ctl,
    /// `atpkg` — the package manager.
    Pkg,
    /// `abrew` / `atpkg-abrew` — aterm's Homebrew (docs/DESIGN-abrew-2026-10-19.md):
    /// an alias because the reroute stub and user shells name one executable.
    Abrew,
    /// `agh` — the GitHub client (docs/DESIGN-agh-2026-10-19.md): an alias so agents and
    /// scripts call it by its own name.
    Agh,
    /// `aterm-fleet` — this machine's fleet membership.
    Fleet,
    /// `aterm-drive` — the agent drive CLI.
    Drive,
    /// `aterm-open-url`, the `BROWSER` a Claude Code login renewal runs
    /// (`aterm harness accounts renew`): it opens the URL in the account's own browser
    /// profile.
    OpenUrl,
    /// `aterm-link`, the fabric bridge: an alias as well as a verb because `[fabric] command`
    /// is split on whitespace and exec'd, so it must name something executable.
    Link,
    /// `aterm-gui <new-tab|new-window|split-pane> …`: a windowing verb typed at, or committed
    /// into the jump list by, an alias copy; routed as `aterm <verb>` is, never handed to the
    /// window's flag parser.
    AliasWindowVerb,
    /// `aterm-gui …`: the window, with the plain-launch routing policy.
    AliasWindow,
    /// `aterm-askpass …`, the `SUDO_ASKPASS` helper (`docs/DESIGN-sudo-askpass-2026-10-15.md`):
    /// an argv0 alias because `sudo` execs it as `[path, prompt]` with no room for a verb, and,
    /// having no tty, it must run before the session/window fork.
    Askpass,
    /// `aterm-machines-askpass <prompt>`, the `SSH_ASKPASS` program `aterm machines enroll`
    /// points each Mac's `ssh` login at, through a link it makes in its own scratch
    /// directory: it answers with the password `enroll` feeds `ssh`'s standard input.
    MachinesAskpass,
    /// `amacs …`, aterm's editor (`aterm edit`, docs/DESIGN-amacs-2026-10-19.md §8.1): an alias
    /// because `$EDITOR` names one executable, which `git` and the rest run with the file and no
    /// room for a verb.
    Amacs,
    /// The front door itself: `aterm`, `aterm-cli` (forced to a session), a renamed copy, and the
    /// window alias running the hidden open probe (`__tcc-open`).
    FrontDoor,
}

/// The route of an invocation whose argv[0] file name is `argv0` and whose argv[1] is `first`.
#[must_use]
pub fn alias_route(argv0: &str, first: &str) -> AliasRoute {
    match argv0 {
        "aterm-gui" | "aterm-windowed" if first == aterm_containment::consent::OPEN_PROBE_VERB => {
            AliasRoute::FrontDoor
        }
        "aterm-ctl" => AliasRoute::Ctl,
        "atpkg" => AliasRoute::Pkg,
        "abrew" | "atpkg-abrew" => AliasRoute::Abrew,
        "agh" => AliasRoute::Agh,
        "aterm-fleet" => AliasRoute::Fleet,
        "aterm-drive" => AliasRoute::Drive,
        "aterm-askpass" => AliasRoute::Askpass,
        "aterm-machines-askpass" => AliasRoute::MachinesAskpass,
        "aterm-link" => AliasRoute::Link,
        "aterm-open-url" => AliasRoute::OpenUrl,
        "amacs" => AliasRoute::Amacs,
        "aterm-gui" | "aterm-windowed" => match Verb::from_operand(first) {
            Some(verb) if verb.is_windowing() => AliasRoute::AliasWindowVerb,
            _ => AliasRoute::AliasWindow,
        },
        _ => AliasRoute::FrontDoor,
    }
}

/// Whether the front door answers argv[1] `first` with [`crate::parse_args`] and exits before it
/// routes: `help`, `-h`, `--help`, `-V`, `--version` or a diagnostic ([`DIAG_COMMANDS`]).
#[must_use]
pub fn mode_free(first: &str) -> bool {
    matches!(first, "help" | "-h" | "--help" | "-V" | "--version")
        || DIAG_COMMANDS.iter().any(|(name, _)| *name == first)
}

/// Whether argv[1] `first` asks the front door for a completion script: `--completions` (the
/// shell in argv[2]) or `--completions=<shell>`.
#[must_use]
pub fn asks_completions(first: &str) -> bool {
    first == "--completions" || first.starts_with("--completions=")
}
