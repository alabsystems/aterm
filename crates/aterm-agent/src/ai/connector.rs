// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CONNECTION LAYER'S ONE ROW PER AI CLI (docs/DESIGN-ai-connections-2026-10-16.md §5).
//!
//! A row is keyed by the program word of atpkg's [`atpkg::vendor_direct::VENDORS`], the
//! package roster every other layer derives from. Every fact is compiled (harness-plugins
//! owner rule R1): where the vendor keeps its home, which files show a login exists (their
//! existence and write time only, never their content), the sign-in flow, the one-shot
//! one-shot mode a review runs (and its flags for each permission level), and the wording a failed one-shot run uses for a lost login
//! or a reached limit.
//!
//! [`connections`] walks `VENDORS` and keeps the programs that have a row, so a package with
//! no row is not a connection in this build and nothing polices the pairing.
//!
//! A row whose commands have not run on a real install is `measured: false`: it never reads
//! FAIL in the doctor. It reviews like any other.

/// One AI CLI aterm can keep a connection to.
#[derive(Debug)]
pub struct Connector {
    /// The program word: `VENDORS`' `program`, the shim's name, the config table's key.
    pub id: &'static str,
    /// The product's name, as the vendor writes it.
    pub product: &'static str,
    /// Where the CLI keeps its state.
    pub home: Home,
    /// Files under the home whose existence shows a login was stored (stat()ed, never
    /// opened). Any one present is a login.
    pub stores: &'static [&'static str],
    /// Variables the CLI reads ahead of its stored login: a sign-in and a review run
    /// without them, and the doctor names any it finds set.
    pub credential_env: &'static [&'static str],
    /// The sign-in flow.
    pub login: Login,
    /// The one-shot mode a review runs.
    pub review: Review,
    /// How the CLI starts a line that reports its own failure (lowercase), where it marks
    /// one: only such lines are read for a lost login or a limit, so its talk of the change
    /// under review never decides a verdict. `None`: its last lines are read.
    pub error_lead: Option<&'static str>,
    /// Lowercase phrases in a failed run's error output meaning the login has ended.
    pub auth_words: &'static [&'static str],
    /// Lowercase phrases in a failed run's error output meaning the usage limit is reached.
    pub limit_words: &'static [&'static str],
    /// Whether a lab pass has run this row's commands on a real install.
    pub measured: bool,
}

/// Where a CLI keeps its state.
#[derive(Debug)]
pub struct Home {
    /// The variable that moves it, honoured when set and not empty.
    pub var: Option<&'static str>,
    /// The default, under `$HOME`.
    pub default: &'static str,
}

/// A status read after a sign-in, and its closed grammar.
#[derive(Debug)]
pub struct Status {
    /// Arguments after the program.
    pub args: &'static [&'static str],
    /// Line prefixes and what each means: `Some(method word)` signed in that way, `None`
    /// signed out. Anything after a prefix (an API key's masked fragment) is dropped where
    /// it is read; a line outside the set says nothing.
    pub lines: &'static [(&'static str, Option<&'static str>)],
}

/// The sign-in flow.
#[derive(Debug)]
pub enum Login {
    /// Claude Code: only accounts v2's renewal (accounts v2 §5), which owns the machine's
    /// Claude login, its staging home, its email hint and its one check after a sign-in.
    Accounts,
    /// Run the CLI in this terminal and wait for it.
    Run {
        /// Arguments after the program.
        args: &'static [&'static str],
        /// Arguments over SSH, where no browser can open here (`None`: the same).
        remote: Option<&'static [&'static str]>,
        /// What the person is told before it starts.
        tell: &'static str,
        /// Whether the flow signs out the current login before it starts (Codex revokes
        /// it), so it is run only when the CLI is already signed out.
        signs_out_first: bool,
        /// The status read run after the flow, on the login it just made.
        after: Option<Status>,
    },
}

/// How a review hands the request in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptVia {
    /// On stdin; `dash` appends `-` to say so.
    Stdin {
        /// Whether `-` follows the arguments.
        dash: bool,
    },
    /// In `request.txt` in the run directory, named by this flag.
    File(&'static str),
    /// In `request.txt` in the run directory, which a short request after this flag tells
    /// the CLI to read (a request in argv would be visible to `ps` and capped in size).
    Pointer(&'static str),
}

/// Where a review's answer comes out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerVia {
    /// Standard output.
    Stdout,
    /// `answer.txt` in the run directory, named by this flag.
    File(&'static str),
}

/// The flags that give a reviewer each permission level (`permission::Level`).
#[derive(Debug)]
pub struct Levels {
    /// No approvals and no sandbox.
    pub yolo: &'static [&'static str],
    /// Edits without asking.
    pub edits: &'static [&'static str],
    /// What would ask is refused.
    pub ask: &'static [&'static str],
    /// Read and plan only.
    pub plan: &'static [&'static str],
}

impl Levels {
    /// The flags for `level`.
    #[must_use]
    pub fn of(&self, level: super::permission::Level) -> &'static [&'static str] {
        use super::permission::Level;
        match level {
            Level::Yolo => self.yolo,
            Level::Edits => self.edits,
            Level::Ask => self.ask,
            Level::Plan => self.plan,
        }
    }
}

/// The one-shot mode a review runs: the CLI with every capability it has, in the repository,
/// at the asking agent's permission level.
#[derive(Debug)]
pub struct Review {
    /// Arguments after the program, before the answer's and the request's.
    pub args: &'static [&'static str],
    /// The flags for each permission level.
    pub levels: Levels,
    /// Whether the level's flags go before `args` (Muse Code's are global flags, before its
    /// `exec`); otherwise after.
    pub levels_first: bool,
    /// How the request goes in.
    pub prompt: PromptVia,
    /// Where the answer comes out.
    pub answer: AnswerVia,
}

/// Claude Code (Anthropic).
pub const CLAUDE: Connector = Connector {
    id: "claude",
    product: "Claude Code",
    home: Home {
        var: Some("CLAUDE_CONFIG_DIR"),
        default: ".claude",
    },
    // On macOS the login is a keychain item, which aterm never reads: the profile's
    // `oauthAccount` and accounts v2's bound login show it instead (`facts`).
    stores: &[".credentials.json"],
    credential_env: &[
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
    ],
    login: Login::Accounts,
    review: Review {
        args: &["-p", "--output-format", "text"],
        levels: Levels {
            yolo: &["--dangerously-skip-permissions"],
            edits: &[
                "--permission-mode",
                "acceptEdits",
                "--permission-prompts",
                "none",
            ],
            ask: &["--permission-prompts", "none"],
            plan: &["--permission-mode", "plan", "--permission-prompts", "none"],
        },
        levels_first: false,
        prompt: PromptVia::Stdin { dash: false },
        answer: AnswerVia::Stdout,
    },
    error_lead: None,
    // `-p`'s non-interactive wording (2.1.301 bundle), and the TUI's.
    auth_words: &[
        "failed to authenticate",
        "oauth session expired",
        "please run /login",
        "not logged in",
        "login expired",
        "oauth token revoked",
        "invalid api key",
    ],
    limit_words: &[
        "hit your session limit",
        "hit your weekly limit",
        "hit your limit",
        "usage limit reached",
    ],
    measured: true,
};

/// Codex CLI (OpenAI).
pub const CODEX: Connector = Connector {
    id: "codex",
    product: "Codex CLI",
    home: Home {
        var: Some("CODEX_HOME"),
        default: ".codex",
    },
    stores: &["auth.json"],
    credential_env: &["OPENAI_API_KEY", "CODEX_API_KEY", "CODEX_ACCESS_TOKEN"],
    login: Login::Run {
        args: &["login"],
        remote: None,
        tell: "Codex's own sign-in opens in your browser. It signs out (revokes) Codex's \
               current login before it starts: if you cancel, Codex stays signed out.",
        // 0.163.0 `cli/src/login.rs`: `clear_existing_auth_before_login` revokes first.
        signs_out_first: true,
        // MEASURED on 0.163.0: one line on stderr, read from local storage only.
        after: Some(Status {
            args: &["login", "status"],
            lines: &[
                ("Logged in using ChatGPT", Some("chatgpt")),
                ("Logged in using an API key", Some("api-key")),
                ("Logged in using access token", Some("access-token")),
                (
                    "Logged in using personal access token",
                    Some("personal-access-token"),
                ),
                ("Logged in using Amazon Bedrock", Some("bedrock")),
                (
                    "Logged in using workload identity",
                    Some("workload-identity"),
                ),
                ("Not logged in", None),
            ],
        }),
    },
    review: Review {
        args: &["exec", "--skip-git-repo-check", "--color", "never"],
        levels: Levels {
            yolo: &["--dangerously-bypass-approvals-and-sandbox"],
            edits: &["--sandbox", "workspace-write"],
            ask: &["--sandbox", "workspace-write"],
            plan: &["--sandbox", "read-only"],
        },
        levels_first: false,
        prompt: PromptVia::Stdin { dash: true },
        answer: AnswerVia::File("-o"),
    },
    error_lead: Some("error"),
    auth_words: &[
        "could not be refreshed",
        "sign in again",
        "unexpected status 401",
        "not logged in",
    ],
    limit_words: &[
        "hit your usage limit",
        "reached your usage limit",
        "usage limit reached",
    ],
    measured: true,
};

/// Antigravity CLI (Google). Its sign-in, store and one-shot behaviour are unmeasured; its
/// help was read on 1.4.0 (2026-10-16): no login verb (it signs in on its first interactive
/// run, from the OS keyring or a browser), `--print` takes the request as an argument, and
/// `models` lists models (it reaches Google; atpkg's row: exit 1 when signed out).
pub const AGY: Connector = Connector {
    id: "agy",
    product: "Antigravity CLI",
    home: Home {
        var: None,
        default: ".gemini/antigravity-cli",
    },
    // The OS keyring holds its login, which aterm never reads.
    stores: &[],
    credential_env: &["GEMINI_API_KEY"],
    login: Login::Run {
        args: &[],
        remote: None,
        tell: "agy opens; sign in with Google in the browser it opens (over SSH it prints a \
               URL and waits for the code), then quit agy",
        signs_out_first: false,
        after: None,
    },
    review: Review {
        args: &["--output-format", "text"],
        levels: Levels {
            yolo: &["--dangerously-skip-permissions"],
            edits: &["--mode", "accept-edits"],
            ask: &[],
            plan: &["--mode", "plan"],
        },
        levels_first: false,
        prompt: PromptVia::Pointer("--print"),
        answer: AnswerVia::Stdout,
    },
    error_lead: None,
    // `You are currently not signed in.` is agy 1.4.0's signed-out wall (aterm-phase `agy`).
    auth_words: &[
        "authentication required",
        "you are currently not signed in",
        "not signed in",
        "unauthenticated",
    ],
    limit_words: &["resource_exhausted", "quota exceeded", "rate limit"],
    measured: false,
};

/// Muse Code (Meta); served on macOS only. Its help was read on 1.4.10-R6427.1 (2026-10-16):
/// `muse login` approves a code in the browser (so it works over SSH too), its login is
/// `~/.config/muse/.auth.json`, and `META_API_KEY` takes priority over it. Its safety flags
/// (`--yolo`, `--disable-approval`, `--disable-write`, `--disable-shell`) are global, before
/// `exec`. Its one-shot behaviour is unmeasured until someone signs in.
pub const MUSE: Connector = Connector {
    id: "muse",
    product: "Muse Code",
    home: Home {
        var: None,
        default: ".config/muse",
    },
    stores: &[".auth.json"],
    credential_env: &["META_API_KEY"],
    login: Login::Run {
        args: &["login"],
        remote: None,
        tell: "Muse Code's own sign-in: approve the code it shows in your browser",
        signs_out_first: false,
        after: None,
    },
    review: Review {
        args: &["exec"],
        levels: Levels {
            yolo: &["--yolo"],
            edits: &["--disable-approval"],
            ask: &[],
            plan: &["--disable-write", "--disable-shell"],
        },
        levels_first: true,
        prompt: PromptVia::File("--prompt-file"),
        answer: AnswerVia::Stdout,
    },
    error_lead: None,
    // `Log in with browser` heads Muse Code's signed-out menu (aterm-phase `muse`).
    auth_words: &[
        "log in with browser",
        "not logged in",
        "unauthorized",
        "authentication required",
        "muse login",
    ],
    limit_words: &["rate limit", "quota", "usage limit"],
    measured: false,
};

/// Every row, in no particular order: [`connections`] gives `VENDORS`' order.
pub const ROWS: &[&Connector] = &[&CLAUDE, &CODEX, &AGY, &MUSE];

/// Every row's id, in [`ROWS`]' order, for a list a constant must name (the window's
/// choice of a preferred CLI); [`ids`] is the connections' own, in `VENDORS`' order.
pub const IDS: [&str; ROWS.len()] = {
    let mut ids = [""; ROWS.len()];
    let mut i = 0;
    while i < ROWS.len() {
        ids[i] = ROWS[i].id;
        i += 1;
    }
    ids
};

/// The row for `id`, if this build has one.
#[must_use]
pub fn connector(id: &str) -> Option<&'static Connector> {
    ROWS.iter().copied().find(|c| c.id == id)
}

/// THE CONNECTIONS: every program in atpkg's `VENDORS` that has a row, in `VENDORS` order.
pub fn connections() -> impl Iterator<Item = &'static Connector> {
    atpkg::vendor_direct::VENDORS
        .iter()
        .filter_map(|v| connector(v.program))
}

/// The ids of [`connections`], for messages that list what is accepted.
#[must_use]
pub fn ids() -> Vec<&'static str> {
    connections().map(|c| c.id).collect()
}

impl Connector {
    /// Whether this is Claude Code, whose login accounts v2 owns.
    #[must_use]
    pub fn is_claude(&self) -> bool {
        matches!(self.login, Login::Accounts)
    }
}

#[cfg(test)]
#[path = "connector_tests.rs"]
mod tests;
