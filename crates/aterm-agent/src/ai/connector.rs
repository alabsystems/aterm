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
    /// Lowercase prefix for lines reporting the CLI's own failure.
    pub error_lead: Option<&'static str>,
    /// Lowercase phrases in a failed run's error output meaning the login has ended.
    pub auth_words: &'static [&'static str],
    /// Lowercase phrases in a failed run's error output meaning the usage limit is reached.
    pub limit_words: &'static [&'static str],
    /// Whether a lab pass has verified this row's commands against a real install.
    pub measured: bool,
    /// The account pool's data, for a CLI whose logins aterm rotates ([`Cli`]).
    pub accounts: Option<&'static Accounts>,
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

/// One AI CLI whose logins aterm pools and rotates (`docs/DESIGN-ai-accounts-multi-cli-2026-10-23.md`
/// §0): the connector row it names carries the pool's data ([`Accounts`]), and the one roster
/// parser, store, advice and ledger read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Cli {
    /// Claude Code ([`CLAUDE`]).
    Claude,
    /// Codex CLI and codex-alab, which share its login ([`CODEX`]).
    Codex,
    /// Grok Build ([`GROK`]).
    Grok,
    /// Kimi Code CLI ([`KIMI`]).
    Kimi,
}

impl Cli {
    /// Every pooled CLI, Claude Code first.
    pub const ALL: [Cli; 4] = [Cli::Claude, Cli::Codex, Cli::Grok, Cli::Kimi];

    /// The connector row it names.
    #[must_use]
    pub const fn connector(self) -> &'static Connector {
        match self {
            Cli::Claude => &CLAUDE,
            Cli::Codex => &CODEX,
            Cli::Grok => &GROK,
            Cli::Kimi => &KIMI,
        }
    }

    /// The pooled CLI a session of `program` runs, where it runs one.
    #[must_use]
    pub const fn of(program: aterm_phase::Program) -> Option<Cli> {
        use aterm_phase::Program;
        match program {
            Program::Claude => Some(Cli::Claude),
            Program::Codex => Some(Cli::Codex),
            Program::Grok => Some(Cli::Grok),
            Program::Kimi => Some(Cli::Kimi),
            Program::Agy | Program::Muse | Program::Gemini | Program::Generic => None,
        }
    }

    /// Its program word ([`Connector::id`]).
    #[must_use]
    pub const fn id(self) -> &'static str {
        self.connector().id
    }

    /// Its pool's data: every row a [`Cli`] names carries one, which the compiler checks
    /// ([`EVERY_CLI_POOLS`]).
    #[must_use]
    pub const fn accounts(self) -> &'static Accounts {
        match self.connector().accounts {
            Some(accounts) => accounts,
            None => panic!("a Cli names a connector row with no accounts block"),
        }
    }

    /// The CLI whose program word is `id`.
    #[must_use]
    pub fn parse(id: &str) -> Option<Cli> {
        Cli::ALL.into_iter().find(|cli| cli.id() == id)
    }

    /// The program words of [`Cli::ALL`], for messages that list what is accepted.
    #[must_use]
    pub fn ids() -> Vec<&'static str> {
        Cli::ALL.into_iter().map(Cli::id).collect()
    }
}

/// Every [`Cli`] row carries an accounts block: evaluated at compile time, so a row without one
/// fails the build rather than a call.
pub const EVERY_CLI_POOLS: () = {
    let mut i = 0;
    while i < Cli::ALL.len() {
        let _ = Cli::ALL[i].accounts();
        i += 1;
    }
};

/// The account pool's data for one CLI (design §0): how a session is pointed at one of its
/// logins, where its roster and store live, and what its screens say at a wall.
#[derive(Debug)]
pub struct Accounts {
    /// How a session is pointed at a login.
    pub seam: Seam,
    /// The roster's file name, beside `aterm.toml`.
    pub roster_file: &'static str,
    /// The CLI's own default home under the user's home, which an overlay login's shared
    /// entries link to (`accounts_overlay::ensure`).
    pub default_home: &'static str,
    /// The store's directory name under the harness state.
    pub store_dir: &'static str,
    /// The vendor keys a roster row may carry beside the common ones.
    pub keys: RowKeys,
    /// The phrases its screens and exit text say at a wall.
    pub walls: Walls,
    /// Words in the sender of the vendor's sign-in mail, for the mailbox reader.
    pub mail_senders: &'static [&'static str],
    /// Whether a lab pass measured [`Accounts::seam`] (and its shared set) against a real
    /// install; until one has, moves between its logins are not advised.
    pub measured: bool,
    /// The build the lab measured the seam on, where it did.
    pub measured_on: Option<&'static str>,
    /// What the lab measured the CLI's walls on (the limit and login-ended rows a running
    /// session ends a turn on, or the records its limits are read from), where it did. Until it
    /// has, a session of the CLI is moved only where aterm launches it (its pin, or `next:`),
    /// never mid-session ([`crate::harness::accounts::advise_session`]).
    pub walls_measured_on: Option<&'static str>,
    /// How the harness signs one of its logins in by itself; `None` for Claude Code, whose
    /// renewal is its own (`claude auth login` through the argv0 opener).
    pub signin: Option<SignIn>,
}

/// How a session is pointed at one login of a pool (design §3). A default row (no `dir`)
/// exports nothing and unsets the variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seam {
    /// The variable names the login's directory; the CLI's home stays the default one.
    LoginDir {
        /// The variable.
        var: &'static str,
    },
    /// The variable names one file in the login's directory; the CLI's home stays shared.
    LoginFile {
        /// The variable.
        var: &'static str,
        /// The file's name in the login's directory.
        file: &'static str,
    },
    /// The variable names the login's directory as a whole home, whose shared entries link to
    /// the default home (`accounts_overlay::ensure`).
    Overlay {
        /// The variable.
        var: &'static str,
        /// The entries every overlay home links to the default home.
        shared: &'static [Shared],
    },
}

impl Seam {
    /// The variable it sets.
    #[must_use]
    pub const fn var(self) -> &'static str {
        match self {
            Seam::LoginDir { var } | Seam::LoginFile { var, .. } | Seam::Overlay { var, .. } => var,
        }
    }
}

/// One entry an overlay home links to the default home ([`Seam::Overlay`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shared {
    /// A directory: made in the default home where it is missing, so the link never dangles.
    Dir(&'static str),
    /// A file: linked whether or not it exists yet, since the CLI's create follows the link.
    File(&'static str),
    /// Every file at the default home's root whose name ends so (a database: `-wal` and `-shm`
    /// are made beside the file a link resolves to, so they are never linked themselves).
    Suffix(&'static str),
}

/// The vendor keys a roster row may carry (design §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKeys {
    /// None: the common keys alone.
    Plain,
    /// `region`, required on a row with an `email`: the two regions are separate account
    /// systems, so a sign-in on the wrong one can make a new account.
    Region,
    /// `flow`, the sign-in flow, defaulting to the one named.
    Flow(Flow),
}

/// A sign-in flow a row may ask for ([`RowKeys::Flow`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// The browser flow with a loopback callback.
    Browser,
    /// The device-code flow.
    Device,
}

impl Flow {
    /// The roster's word for it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Flow::Browser => "browser",
            Flow::Device => "device",
        }
    }

    /// The flow the roster's `word` names.
    #[must_use]
    pub fn parse(word: &str) -> Option<Flow> {
        match word {
            "browser" => Some(Flow::Browser),
            "device" => Some(Flow::Device),
            _ => None,
        }
    }
}

/// A Kimi account system ([`RowKeys::Region`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    /// kimi.ai.
    Global,
    /// kimi.com.
    MainlandCn,
}

impl Region {
    /// The roster's word for it, as `kimi login --region` takes it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Region::Global => "global",
            Region::MainlandCn => "mainland-cn",
        }
    }

    /// The region the roster's `word` names.
    #[must_use]
    pub fn parse(word: &str) -> Option<Region> {
        match word {
            "global" => Some(Region::Global),
            "mainland-cn" => Some(Region::MainlandCn),
            _ => None,
        }
    }

    /// The site its accounts live on.
    #[must_use]
    pub const fn site(self) -> &'static str {
        match self {
            Region::Global => "kimi.ai",
            Region::MainlandCn => "kimi.com",
        }
    }
}

/// The phrases a CLI's screens and exit text say at a wall, lowercase and matched within a
/// line lowercased ([`Walls::limit_in`]).
#[derive(Debug)]
pub struct Walls {
    /// The usage limit is reached (a wall).
    pub limit: &'static [&'static str],
    /// The login has ended.
    pub ended: &'static [&'static str],
    /// A passing throttle, which is never a wall even where a limit phrase also matches.
    pub transient: &'static [&'static str],
}

impl Walls {
    /// Whether `line` says the usage limit is reached and no passing throttle.
    #[must_use]
    pub fn limit_in(&self, line: &str) -> bool {
        let line = line.to_lowercase();
        !self.transient.iter().any(|w| line.contains(w))
            && self.limit.iter().any(|w| line.contains(w))
    }

    /// Whether `line` says the login has ended.
    #[must_use]
    pub fn ended_in(&self, line: &str) -> bool {
        let line = line.to_lowercase();
        self.ended.iter().any(|w| line.contains(w))
    }
}

/// How the harness signs one login of a pool in by itself (design §6): the vendor's own sign-in,
/// run in a staging home, its page URL and code read off its output by a closed grammar, and the
/// page approved in a headless browser on the pinned hosts only.
#[derive(Debug)]
pub struct SignIn {
    /// One route per flow the pool offers, the CLI's default first; a row's `flow` picks one
    /// ([`SignIn::route`]).
    pub routes: &'static [Route],
    /// Variables the sign-in's child gets beside its home's.
    pub env: &'static [(&'static str, &'static str)],
    /// Whether the child's `PATH` is an empty private directory, so a CLI that opens its page
    /// with `open` or `xdg-open` from `PATH` opens nothing.
    pub no_opener: bool,
    /// The operating systems (`std::env::consts::OS`) on which the CLI opens its page in the
    /// default browser through the OS itself, whatever its `PATH` (Grok Build on macOS): there
    /// its sign-in runs only when a person asked for it, never in the background
    /// ([`SignIn::opens_browser_here`]).
    pub opens_browser_on: &'static [&'static str],
}

impl SignIn {
    /// Whether the CLI opens the default browser on this machine whatever its `PATH`
    /// ([`SignIn::opens_browser_on`]).
    #[must_use]
    pub fn opens_browser_here(&self) -> bool {
        self.opens_browser_on.contains(&std::env::consts::OS)
    }

    /// The route for `flow`, or the first one where the row names none or the CLI takes none.
    #[must_use]
    pub fn route(&self, flow: Option<Flow>) -> Option<&'static Route> {
        match flow {
            Some(flow) => self.routes.iter().find(|r| r.flow == flow),
            None => self.routes.first(),
        }
    }
}

/// One way into a CLI's sign-in ([`SignIn::routes`]).
#[derive(Debug)]
pub struct Route {
    /// The flow it is.
    pub flow: Flow,
    /// How it is run and read.
    pub drive: Drive,
    /// The hosts its pages may be on: the URL it names, and every page the headless browser
    /// presses on, must be `https` on one of them.
    pub hosts: Hosts,
}

/// How a route runs ([`Route::drive`]).
#[derive(Debug)]
pub enum Drive {
    /// The CLI's sign-in verb, its standard output and error read line by line.
    Lines {
        /// The arguments after the program.
        args: &'static [&'static str],
        /// The flag that names the row's region, followed by its word.
        region_flag: Option<&'static str>,
        /// What its lines say.
        grammar: Grammar,
        /// How the page hands the login back.
        completion: Completion,
    },
    /// Codex's app server over JSON-RPC (`harness::codex_app_login`): the login's page and its
    /// loopback callback named by `account/login/start`, the account by `account/read`.
    AppServer,
}

/// How a sign-in page hands the login back to the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completion {
    /// The page redirects to a callback the CLI listens on, on this machine.
    Loopback,
    /// The CLI polls the vendor until the page is approved: no callback.
    Device,
}

/// A sign-in's closed line grammar ([`Drive::Lines`]): each lead is matched within one line,
/// ANSI stripped; the value follows it on that line, or, when nothing that reads as one does,
/// is the next line that is not blank.
#[derive(Debug)]
pub struct Grammar {
    /// Leads of the line naming the page's URL.
    pub url: &'static [&'static str],
    /// Leads of the line naming the code the page shows or asks for.
    pub code: &'static [&'static str],
    /// Phrases of the line saying the sign-in finished.
    pub done: &'static [&'static str],
    /// The lead before the email a done line names, where the CLI names the account.
    pub signed_in_as: Option<&'static str>,
}

/// The hosts a route's pages may be on ([`Route::hosts`]).
#[derive(Debug)]
pub enum Hosts {
    /// These, for every row.
    Fixed(&'static [&'static str]),
    /// Per Kimi account system: a URL from the other region's hosts is refused.
    ByRegion {
        /// kimi.ai.
        global: &'static [&'static str],
        /// kimi.com.
        mainland_cn: &'static [&'static str],
    },
}

impl Hosts {
    /// The hosts for a row of `region`; `None` for a region-keyed route and a row naming none.
    #[must_use]
    pub const fn of(&self, region: Option<Region>) -> Option<&'static [&'static str]> {
        match (self, region) {
            (Hosts::Fixed(hosts), _) => Some(hosts),
            (Hosts::ByRegion { global, .. }, Some(Region::Global)) => Some(global),
            (Hosts::ByRegion { mainland_cn, .. }, Some(Region::MainlandCn)) => Some(mainland_cn),
            (Hosts::ByRegion { .. }, None) => None,
        }
    }
}

const CLAUDE_AUTH_WORDS: &[&str] = &[
    "failed to authenticate",
    "oauth session expired",
    "please run /login",
    "not logged in",
    "login expired",
    "oauth token revoked",
    "invalid api key",
];

const CLAUDE_LIMIT_WORDS: &[&str] = &[
    "hit your session limit",
    "hit your weekly limit",
    "hit your limit",
    "usage limit reached",
];

/// Claude Code's pool: the roster, store and seam accounts v2 has always used.
pub const CLAUDE_ACCOUNTS: Accounts = Accounts {
    seam: Seam::LoginDir {
        var: "CLAUDE_SECURESTORAGE_CONFIG_DIR",
    },
    roster_file: "accounts.toml",
    default_home: ".claude",
    store_dir: "accounts",
    keys: RowKeys::Plain,
    walls: Walls {
        limit: CLAUDE_LIMIT_WORDS,
        ended: CLAUDE_AUTH_WORDS,
        transient: &[],
    },
    mail_senders: &["anthropic"],
    measured: true,
    measured_on: None,
    walls_measured_on: Some("Claude Code's anchored wall rows"),
    signin: None,
};

/// The hosts every Codex sign-in page is on: OpenAI's own sign-in, consent and device pages.
const CODEX_HOSTS: &[&str] = &["auth.openai.com"];

/// Codex's sign-in: the app server's browser login by default, its loopback callback on this
/// machine (lab gate (d), codex-old 0.164.0: `account/login/start` answers a `loginId` and an
/// `authUrl` on auth.openai.com whose `redirect_uri` is `http://127.0.0.1:1455/auth/callback`,
/// and opens no browser), or `codex login --device-auth` where the row names `flow = "device"`
/// (recorded on codex-old 0.164.0: the link and the code each on the line after its lead, the
/// link always `https://auth.openai.com/codex/device`, so the page asks for the code).
pub const CODEX_SIGNIN: SignIn = SignIn {
    routes: &[
        Route {
            flow: Flow::Browser,
            drive: Drive::AppServer,
            hosts: Hosts::Fixed(CODEX_HOSTS),
        },
        Route {
            flow: Flow::Device,
            drive: Drive::Lines {
                args: &["login", "--device-auth"],
                region_flag: None,
                grammar: Grammar {
                    url: &["Open this link in your browser"],
                    code: &["Enter this one-time code"],
                    done: &["Successfully logged in"],
                    signed_in_as: None,
                },
                completion: Completion::Device,
            },
            hosts: Hosts::Fixed(CODEX_HOSTS),
        },
    ],
    env: &[],
    no_opener: true,
    opens_browser_on: &[],
};

/// The hosts every Grok Build sign-in page is on: xAI's issuer and its accounts app, which
/// serves the consent page.
const GROK_HOSTS: &[&str] = &["auth.x.ai", "accounts.x.ai"];

/// Grok Build's sign-in, its lines as 1.0.67's source prints them (not recorded: on macOS its
/// login opens the default browser through the OS, which nothing suppresses): the device flow
/// by default, its complete URL (the code filled in) and the code to confirm each on the line
/// after its lead, and `✓ Signed in as <email>` once approved; `flow = "browser"` takes its
/// loopback flow.
pub const GROK_SIGNIN: SignIn = SignIn {
    routes: &[
        Route {
            flow: Flow::Device,
            drive: Drive::Lines {
                args: &["login", "--device-auth"],
                region_flag: None,
                grammar: Grammar {
                    url: &["To sign in, open this URL in your browser:"],
                    code: &[
                        "Confirm this code in your browser:",
                        "Then enter this code:",
                    ],
                    done: &["Signed in"],
                    signed_in_as: Some("Signed in as "),
                },
                completion: Completion::Device,
            },
            hosts: Hosts::Fixed(GROK_HOSTS),
        },
        Route {
            flow: Flow::Browser,
            drive: Drive::Lines {
                args: &["login", "--oauth"],
                region_flag: None,
                grammar: Grammar {
                    url: &["Open this URL to sign in:"],
                    code: &[],
                    done: &["Signed in"],
                    signed_in_as: Some("Signed in as "),
                },
                completion: Completion::Loopback,
            },
            hosts: Hosts::Fixed(GROK_HOSTS),
        },
    ],
    env: &[],
    no_opener: true,
    opens_browser_on: &["macos"],
};

/// Kimi Code's sign-in, its device flow, recorded on 2.3.1 (lab gate (c): with no `open` on
/// `PATH` it opens nothing; it prints the URL, on www.kimi.ai for `--region global`, and the
/// code on standard error, and `Logged in to <provider>.` once approved, on the line of its
/// `Waiting for authorization to complete…`, which ends with no newline). Its telemetry and its
/// self-update stay off; it is never handed an identity slug or custom headers.
pub const KIMI_SIGNIN: SignIn = SignIn {
    routes: &[Route {
        flow: Flow::Device,
        drive: Drive::Lines {
            args: &["login"],
            region_flag: Some("--region"),
            grammar: Grammar {
                url: &["Opening browser for Kimi device login:"],
                code: &["enter code:"],
                done: &["Logged in to "],
                signed_in_as: None,
            },
            completion: Completion::Device,
        },
        hosts: Hosts::ByRegion {
            global: &["auth.kimi.ai", "www.kimi.ai"],
            mainland_cn: &["auth.kimi.com", "www.kimi.com"],
        },
    }],
    env: &[
        ("KIMI_DISABLE_TELEMETRY", "1"),
        ("KIMI_CODE_NO_AUTO_UPDATE", "1"),
    ],
    no_opener: true,
    opens_browser_on: &[],
};

const CODEX_AUTH_WORDS: &[&str] = &[
    "could not be refreshed",
    "sign in again",
    "unexpected status 401",
    "not logged in",
    "refresh token was already used",
    "was revoked",
    "has expired",
];

const CODEX_LIMIT_WORDS: &[&str] = &[
    "hit your usage limit",
    "reached your usage limit",
    "usage limit reached",
    "out of credits",
    "spend cap",
];

/// Codex's pool: a whole `CODEX_HOME` per account, since Codex has no login-only variable,
/// with everything but the login linked to `~/.codex` (lab gate (a), codex-old 0.164.0: its
/// databases keep their `-wal` beside the linked file, a rollout lands in the linked
/// `sessions`, and `exec resume <id>` in an overlay finds a rollout the default home wrote).
pub const CODEX_ACCOUNTS: Accounts = Accounts {
    seam: Seam::Overlay {
        var: "CODEX_HOME",
        shared: &[
            Shared::Dir("sessions"),
            Shared::Dir("archived_sessions"),
            Shared::File("session_index.jsonl"),
            Shared::File("history.jsonl"),
            Shared::File("config.toml"),
            Shared::File("AGENTS.md"),
            Shared::Dir("prompts"),
            Shared::Dir("rules"),
            Shared::Dir("skills"),
            Shared::Dir("plugins"),
            Shared::Dir("shell_snapshots"),
            Shared::Dir("thread-writer-locks"),
            Shared::File("installation_id"),
            Shared::File(".sqlite-maintenance.lock"),
            Shared::Suffix(".sqlite"),
        ],
    },
    roster_file: "accounts-codex.toml",
    default_home: ".codex",
    store_dir: "accounts-codex",
    keys: RowKeys::Flow(Flow::Browser),
    walls: Walls {
        limit: CODEX_LIMIT_WORDS,
        ended: CODEX_AUTH_WORDS,
        transient: &[],
    },
    mail_senders: &["openai", "chatgpt"],
    measured: true,
    measured_on: Some("codex-old 0.164.0"),
    walls_measured_on: Some("codex rollouts' rate_limits"),
    signin: Some(CODEX_SIGNIN),
};

const GROK_AUTH_WORDS: &[&str] = &[
    "not logged in. run `grok login`",
    "you are not authenticated",
    "no cached credentials found",
    "token expired",
    "your session has expired",
    "authentication could not be refreshed",
];

const GROK_LIMIT_WORDS: &[&str] = &[
    "hit the rate limit for your plan",
    "free grok build usage limit",
];

/// Grok Build's pool: `GROK_AUTH_PATH` names the account's `auth.json` and `GROK_HOME`
/// (sessions, `agent_id`, config) stays shared (lab gate (b), grok 1.0.67: with both set it reads
/// the auth file `GROK_AUTH_PATH` names, and `$GROK_HOME/auth.json` without it).
pub const GROK_ACCOUNTS: Accounts = Accounts {
    seam: Seam::LoginFile {
        var: "GROK_AUTH_PATH",
        file: "auth.json",
    },
    roster_file: "accounts-grok.toml",
    default_home: ".grok",
    store_dir: "accounts-grok",
    keys: RowKeys::Flow(Flow::Device),
    walls: Walls {
        limit: GROK_LIMIT_WORDS,
        ended: GROK_AUTH_WORDS,
        transient: &[],
    },
    mail_senders: &["x.ai", "xai", "grok"],
    measured: true,
    measured_on: Some("grok 1.0.67"),
    walls_measured_on: None,
    signin: Some(GROK_SIGNIN),
};

const KIMI_AUTH_WORDS: &[&str] = &[
    "re-login required",
    "no token for \"kimi-code\"",
    "run /login",
    "use /login to sign in",
];

const KIMI_LIMIT_WORDS: &[&str] = &[
    "exceeded your current quota",
    "exceeded your current token quota",
    "exceeded_current_quota_error",
    "check your account balance",
    "insufficient balance",
    "recharge your account",
    "please recharge",
    "in arrears",
];

/// Kimi Code's pool: a whole `KIMI_CODE_HOME` per account (one home holds one login per OAuth
/// host), its sessions, input history and the user's own setup linked to the default home. Its
/// home root was read in the lab (kimi 2.3.1 makes `device_id`, `cache`, `workspaces.json`,
/// `sessions` and `logs`; `config.toml`, `credentials` and the region marker are the login's),
/// but resume across two homes needs a signed-in account, so it is not measured yet.
pub const KIMI_ACCOUNTS: Accounts = Accounts {
    seam: Seam::Overlay {
        var: "KIMI_CODE_HOME",
        shared: &[
            Shared::Dir("sessions"),
            Shared::Dir("user-history"),
            Shared::File("workspaces.json"),
            Shared::Dir("skills"),
            Shared::Dir("agents"),
            Shared::Dir("plugins"),
            Shared::File("mcp.json"),
            Shared::File("tui.toml"),
            Shared::File("AGENTS.md"),
        ],
    },
    roster_file: "accounts-kimi.toml",
    // `.kimi-code` is the home name kimi 2.3.1's binary spells; the lab ran it under a scratch
    // `KIMI_CODE_HOME` only.
    default_home: ".kimi-code",
    store_dir: "accounts-kimi",
    keys: RowKeys::Region,
    walls: Walls {
        limit: KIMI_LIMIT_WORDS,
        ended: KIMI_AUTH_WORDS,
        transient: &["concurrent request limit"],
    },
    mail_senders: &["kimi", "moonshot"],
    measured: false,
    measured_on: None,
    walls_measured_on: None,
    signin: Some(KIMI_SIGNIN),
};

/// Claude Code (Anthropic).
pub const CLAUDE: Connector = Connector {
    id: "claude",
    product: "Claude Code",
    home: Home {
        var: Some("CLAUDE_CONFIG_DIR"),
        default: ".claude",
    },
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
    auth_words: CLAUDE_AUTH_WORDS,
    limit_words: CLAUDE_LIMIT_WORDS,
    measured: true,
    accounts: Some(&CLAUDE_ACCOUNTS),
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
                ("API key configured", Some("api-key")),
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
    auth_words: CODEX_AUTH_WORDS,
    limit_words: CODEX_LIMIT_WORDS,
    measured: true,
    accounts: Some(&CODEX_ACCOUNTS),
};

/// Antigravity CLI (Google).
pub const AGY: Connector = Connector {
    id: "agy",
    product: "Antigravity CLI",
    home: Home {
        var: None,
        default: ".gemini/antigravity-cli",
    },
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
    accounts: None,
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
    accounts: None,
};

/// Grok Build (xAI). Its flags are read from 1.0.67's `--help`; no lab pass has run them.
pub const GROK: Connector = Connector {
    id: "grok",
    product: "Grok Build",
    home: Home {
        var: Some("GROK_HOME"),
        default: ".grok",
    },
    stores: &["auth.json"],
    credential_env: &["GROK_AUTH", "XAI_API_KEY"],
    login: Login::Run {
        args: &["login"],
        remote: Some(&["login", "--device-auth"]),
        tell: "Grok's own sign-in opens auth.x.ai in your browser (over SSH it prints a URL and \
               a code to confirm there)",
        signs_out_first: false,
        after: None,
    },
    review: Review {
        args: &["--output-format", "plain"],
        levels: Levels {
            yolo: &["--always-approve"],
            edits: &["--permission-mode", "acceptEdits"],
            ask: &["--permission-mode", "dontAsk"],
            plan: &["--permission-mode", "plan"],
        },
        levels_first: false,
        prompt: PromptVia::File("--prompt-file"),
        answer: AnswerVia::Stdout,
    },
    error_lead: None,
    auth_words: GROK_AUTH_WORDS,
    limit_words: GROK_LIMIT_WORDS,
    measured: false,
    accounts: Some(&GROK_ACCOUNTS),
};

/// Kimi Code CLI (Moonshot AI). Its flags are read from 2.3.1's `--help`; no lab pass has run
/// them.
pub const KIMI: Connector = Connector {
    id: "kimi",
    product: "Kimi Code CLI",
    home: Home {
        var: Some("KIMI_CODE_HOME"),
        default: ".kimi-code",
    },
    stores: &["credentials/kimi-code.json"],
    credential_env: &[],
    login: Login::Run {
        args: &["login"],
        remote: None,
        tell: "Kimi Code's own sign-in: approve the code it shows on Kimi's device page in your \
               browser",
        signs_out_first: false,
        after: None,
    },
    review: Review {
        args: &["--output-format", "text"],
        levels: Levels {
            yolo: &["--auto"],
            edits: &["--yolo"],
            ask: &[],
            plan: &["--plan"],
        },
        levels_first: false,
        prompt: PromptVia::Pointer("-p"),
        answer: AnswerVia::Stdout,
    },
    error_lead: None,
    auth_words: KIMI_AUTH_WORDS,
    limit_words: KIMI_LIMIT_WORDS,
    measured: false,
    accounts: Some(&KIMI_ACCOUNTS),
};

/// All connector rows; [`connections`] yields them in `VENDORS` order. [`GROK`] and [`KIMI`]
/// join once atpkg's `VENDORS` serves their programs: until then they are not connections,
/// and only their account pools ([`Cli`]) read them.
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
