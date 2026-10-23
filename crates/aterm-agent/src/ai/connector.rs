// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Compiled connector rows per AI CLI, keyed by [`atpkg::vendor_direct::VENDORS`] program name.

/// One AI CLI aterm can keep a connection to.
#[derive(Debug)]
pub struct Connector {
    /// The program word: `VENDORS`' `program`, the shim's name, the config table's key.
    pub id: &'static str,
    /// The product's name, as the vendor writes it.
    pub product: &'static str,
    /// Where the CLI keeps its state.
    pub home: Home,
    /// Files under the home whose existence indicates a stored login (`stat`ed, never opened).
    pub stores: &'static [&'static str],
    /// Environment variables the CLI reads ahead of its stored login.
    pub credential_env: &'static [&'static str],
    /// The sign-in flow.
    pub login: Login,
    /// The one-shot mode a review runs.
    pub review: Review,
    /// Lowercase prefix for lines reporting the CLI's own failure, or `None` to inspect trailing lines.
    pub error_lead: Option<&'static str>,
    /// Lowercase phrases in a failed run's error output meaning the login has ended.
    pub auth_words: &'static [&'static str],
    /// Lowercase phrases in a failed run's error output meaning the usage limit is reached.
    pub limit_words: &'static [&'static str],
    /// Whether a lab pass has verified this row's commands against a real install.
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
    /// Line prefixes and their meaning: `Some(method)` when signed in, `None` when signed out.
    pub lines: &'static [(&'static str, Option<&'static str>)],
}

/// The sign-in flow.
#[derive(Debug)]
pub enum Login {
    /// Claude Code login managed by accounts v2.
    Accounts,
    /// Run the CLI in this terminal and wait for it.
    Run {
        /// Arguments after the program.
        args: &'static [&'static str],
        /// Arguments over SSH, where no browser can open here (`None`: the same).
        remote: Option<&'static [&'static str]>,
        /// What the person is told before it starts.
        tell: &'static str,
        /// Whether the flow revokes the current login before starting.
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
    /// In `request.txt` in the run directory, read via a short prompt passed after this flag.
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
    pub const fn of(&self, level: super::permission::Level) -> &'static [&'static str] {
        use super::permission::Level;
        match level {
            Level::Yolo => self.yolo,
            Level::Edits => self.edits,
            Level::Ask => self.ask,
            Level::Plan => self.plan,
        }
    }
}

/// One-shot review configuration for a CLI.
#[derive(Debug)]
pub struct Review {
    /// Arguments after the program, before the answer's and the request's.
    pub args: &'static [&'static str],
    /// The flags for each permission level.
    pub levels: Levels,
    /// Whether permission-level flags precede `args`.
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
    // On macOS the login is a keychain item; profile `oauthAccount` and accounts v2 state report it instead.
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
        signs_out_first: true,
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

/// Antigravity CLI (Google).
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
    auth_words: &[
        "authentication required",
        "you are currently not signed in",
        "not signed in",
        "unauthenticated",
    ],
    limit_words: &["resource_exhausted", "quota exceeded", "rate limit"],
    measured: false,
};

/// Muse Code (Meta), served on macOS only.
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

/// All connector rows; [`connections`] yields them in `VENDORS` order.
pub const ROWS: &[&Connector] = &[&CLAUDE, &CODEX, &AGY, &MUSE];

/// Connector IDs in [`ROWS`] order.
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

/// Every program in `VENDORS` that has a connector row, in `VENDORS` order.
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
    /// Whether this connector uses accounts v2 login management.
    #[must_use]
    pub const fn is_claude(&self) -> bool {
        matches!(self.login, Login::Accounts)
    }
}

#[cfg(test)]
#[path = "connector_tests.rs"]
mod tests;
