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
    /// Credential variables the CLI reads in place of or beside its stored login;
    /// [`crate::harness::relaunch::ahead_env`] names those a pooled launch removes.
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
            Program::Agy | Program::Muse | Program::Generic => None,
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
    /// The vendor's own sign-in page, where `accounts browser` opens a row's browser profile for
    /// a person to sign in once.
    pub sign_in_page: SignInPage,
    /// The `localStorage` key under which the vendor's web app keeps its session on its sign-in
    /// page's origin, as its own script reads it; `None` where no lab pass read where it keeps
    /// one. `accounts browser` asks only whether the key is there, never its value.
    pub web_session_key: Option<&'static str>,
    /// Whether a lab pass measured [`Accounts::seam`] (and its shared set) against a real
    /// install; until one has, moves between its logins are not advised.
    pub measured: bool,
    /// The builds the lab measured the seam on, where it did.
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

/// The vendor's sign-in page ([`Accounts::sign_in_page`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignInPage {
    /// This page, for every row.
    Fixed(&'static str),
    /// Per Kimi account system ([`Region`]).
    ByRegion {
        /// kimi.ai.
        global: &'static str,
        /// kimi.com.
        mainland_cn: &'static str,
    },
}

impl SignInPage {
    /// The page for a row of `region`; `None` for a region-keyed page and a row naming none.
    #[must_use]
    pub const fn of(self, region: Option<Region>) -> Option<&'static str> {
        match (self, region) {
            (SignInPage::Fixed(page), _) => Some(page),
            (SignInPage::ByRegion { global, .. }, Some(Region::Global)) => Some(global),
            (SignInPage::ByRegion { mainland_cn, .. }, Some(Region::MainlandCn)) => {
                Some(mainland_cn)
            }
            (SignInPage::ByRegion { .. }, None) => None,
        }
    }
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
        /// The empty files every overlay home is given of its own where it has none: the CLI's
        /// "never ask" marker for a first-start offer whose answer is the default home's, since
        /// the overlay's sessions and setup are the default home's (kimi 2.3.1 offers to move
        /// the older kimi-cli's data into each home holding no `.skip-migration-from-kimi-cli`,
        /// however the default home answered).
        markers: &'static [&'static str],
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
    /// A file: linked whether or not it exists yet, since the CLI's create follows the link. A
    /// file the CLI rewrites by renaming a new copy over it is one only knowingly, since the
    /// rename replaces the link and the overlay's next launch is refused as drift: kimi 2.3.1's
    /// `workspaces.json` is not one, its `mcp.json` is (an MCP server added, changed or removed
    /// in an overlay replaces that link), so the person's MCP setup follows every login. A CLI
    /// that resolves the link before its rename keeps the link: both Codex 0.164.0 builds
    /// rename a new `config.toml` over the default home's file.
    File(&'static str),
    /// A file the person writes and the CLI only reads: linked once the default home has it,
    /// since the CLI warns of a link that leads nowhere (kimi 2.3.1 names a dangling
    /// `AGENTS.md` "not a readable regular file" at every start); while the default home has
    /// none, an overlay's link to it is removed.
    Present(&'static str),
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
    /// default browser through the OS itself, whatever its `PATH` (Grok Build and Codex on
    /// macOS): there its sign-in child runs under [`crate::harness::login_drive::NO_DESK`], and
    /// nothing runs where that sandbox is missing.
    pub no_desk: &'static [&'static str],
    /// Whether the headless approver may type the row's email into the vendor's sign-in page and
    /// read the row's mailbox for the code it mails: `false` where that one form signs a new
    /// account up as readily as it signs one in, and says which only once it is submitted
    /// (Kimi's, design §5 (f)), so the page waits for a person.
    pub types_email: bool,
}

impl SignIn {
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
    sign_in_page: SignInPage::Fixed("https://claude.ai/login"),
    web_session_key: None,
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
/// link always `https://auth.openai.com/codex/device`, so the page asks for the code). Its own
/// `codex login` (a person's browser sign-in, `accounts login`) opens the default browser
/// through LaunchServices whatever its `PATH` (codex 0.164.0-dev under an empty `PATH`: `starting
/// browser login flow`, then `webbrowser::os: failed to get default browser, falling back to
/// Safari` where the no-desk sandbox denied the lookup), so on macOS its child runs under
/// [`crate::harness::login_drive::NO_DESK`] (`no_desk`); the app server, a renewal's route,
/// answers `initialize`, `account/read` and `account/login/start` under it unchanged (0.164.0-dev,
/// a scratch `CODEX_HOME`: a `loginId` and an `authUrl` on auth.openai.com, its loopback
/// listening on 127.0.0.1:1455, nothing on its standard error).
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
    no_desk: &["macos"],
    types_email: true,
};

/// The hosts every Grok Build sign-in page is on: xAI's issuer and its accounts app, which
/// serves the consent page.
const GROK_HOSTS: &[&str] = &["auth.x.ai", "accounts.x.ai"];

/// Grok Build's sign-in, recorded on 1.0.70 under the no-desk sandbox (design §5 (g); 1.0.67's
/// source prints the same lines): the device flow by default, its complete URL (the code filled
/// in) and the code to confirm each on the line after its lead, and `✓ Signed in as <email>`
/// once approved; `flow = "browser"` takes its loopback flow. Its standard error runs: the URL's
/// lead, the URL, an `ERROR … falling back to Safari` line and "(Could not open browser
/// automatically …)" (the browser open the sandbox stopped), the code's lead, the code, then
/// `Waiting for authorization...`. The grammar is unchanged, since the ERROR line comes after
/// the URL value. On macOS it opens the default browser through the OS whatever its `PATH`, so
/// there its child runs under [`crate::harness::login_drive::NO_DESK`] (`no_desk`).
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
    no_desk: &["macos"],
    types_email: true,
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
    no_desk: &[],
    types_email: false,
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
/// Neither codex-old 0.164.0 nor codex-alab 0.164.0-dev replaces one of these links
/// (`docs/measured/codex-overlay-writes-2026-10-24.md`, `exec` and TUI turns, resumes,
/// `/rename`, a history trim, `mcp`, `features`, `archive`, `plugin` (codex-old) and a saved
/// approval rule in an overlay). `config.toml`, the one shared file either rewrites by renaming
/// a new copy over it, gets that copy at the default home's path, so its link stays. The rest
/// is created through its link (`.sqlite-maintenance.lock` by codex-old), appended or written
/// in place, or written inside a linked directory. `AGENTS.override.md` and `AGENTS.md`, the
/// global instructions both builds read from the Codex home (the override first), are linked
/// whether or not the default home has them: neither build warns of a link that leads nowhere,
/// and each reads the file through it once the default home has it (the record's §7, §11).
///
/// An overlay that runs a session before the default home has made its databases makes six of
/// its own, which `ensure` reports as drift once the default home makes them (the record's
/// §8). The pool reads the default home's usage before it links or reads an overlay
/// (`accounts_pool::record_usage`), a sign-in's staging home keeps no database of its own (one
/// the default home lacks is moved there), and `launch_env` and `session_move` pass over a
/// drifted row unless it is the pinned one. Left open: a launch under `accounts = "off"`
/// (straight to the pin, no usage read), a roster with no default row, and a person's own
/// `CODEX_HOME=<overlay>` run.
///
/// Each build records a thread's rollout under the path of the home that wrote it (the shared
/// `state_5.sqlite`), and an `exec resume <id>` from the default home fails (`no rollout
/// found`) once that path no longer resolves (the record's §12). So an overlay directory is
/// never removed or renamed while a session it wrote may be resumed or moved.
pub const CODEX_ACCOUNTS: Accounts = Accounts {
    seam: Seam::Overlay {
        var: "CODEX_HOME",
        shared: &[
            Shared::Dir("sessions"),
            Shared::Dir("archived_sessions"),
            Shared::File("session_index.jsonl"),
            Shared::File("history.jsonl"),
            Shared::File("config.toml"),
            Shared::File("AGENTS.override.md"),
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
        markers: &[],
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
    // Not yet opened by a lab pass.
    sign_in_page: SignInPage::Fixed("https://auth.openai.com/log-in"),
    web_session_key: None,
    measured: true,
    measured_on: Some("codex-old 0.164.0 and codex-alab 0.164.0-dev"),
    walls_measured_on: Some("codex rollouts' rate_limits"),
    signin: Some(CODEX_SIGNIN),
};

/// Grok Build's texts for a login of its own (the one `grok login` writes to the auth file a
/// row's `GROK_AUTH_PATH` names) that is missing, expired, rejected or could not be renewed, as
/// grok 1.0.70's binary spells them (a sweep of its strings; none was seen on a screen): ``Not
/// logged in. Run `grok login`.``, ``Token expired. Run `grok login` to re-authenticate.``,
/// ``Your auth token is invalid or expired. Run `grok login` to re-authenticate.``,
/// ``Authentication rejected by server. …``, ``Auth recovery exhausted; re-authentication
/// required. …`` and `Session expired, re-authentication required`, ``Session expired. Run
/// `grok login` to re-authenticate.``, ``Your session has expired. …``, `Authentication
/// required: your session has expired or your credentials were rejected. …`, ``Authentication
/// required. Run `grok login` to re-authenticate.``, `Your session expired and … could not
/// renew it in the background. …`, ``Authentication could not be refreshed. …``, ``Your team
/// sign-in was rejected. It may have expired or lack access. Run `grok login` to sign in
/// again.``, the not-signed-in texts (``Not signed in. Run `grok login` …``, `Not signed in. To
/// authenticate without a browser, …`, ``not signed in — run `grok login`, …``, `You are not
/// authenticated.`, `No cached auth token found`, ``Authentication failed. Run `grok login`, set
/// XAI_API_KEY, or add api_key to ~/.grok/config.toml.``, ``No cached credentials found. …``,
/// `Headless mode requires a grok.com session. …`, ``preferred_method=oidc but no session is
/// available. …`` and `Signed out: /feedback works, and /login signs in.`), and a compaction's
/// 401 (`… compaction failed — re-authenticate with /login and retry.`, `authentication problem
/// — re-authenticate using /login and retry.`). Each phrase stands in one of aterm-phase's
/// `grok.auth.*` texts, which its `vendor_drift` bench holds to the installed binary
/// (`connector_tests::every_grok_auth_word_is_one_of_the_builds_login_ended_texts`). The agent
/// proxy's status `Not authenticated.` says no login is stored too, but its two words are no
/// phrase: the build's own log lines end on them (`announcements refresh skipped: not
/// authenticated`), and so does much other prose. A review's failed run reads these words
/// ([`super::review::verdict_of`]: Grok Build's row is in [`ROWS`]); no running path asks a
/// pool's [`Walls::ended_in`] yet.
///
/// None is bare `token expired`, `re-authenticate`, ``run `grok login` `` or `sign in again`, which
/// the build also prints where no login of the row's ended: a passing failure (``Authentication
/// is temporarily unavailable. Run `grok login` if this persists.``, `Token refresh failed
/// transiently (network); …`, `Re-authenticated after 401; retrying request`), an MCP server's
/// own OAuth (`Access token expired or nearly expired, refreshing.`, `Invalid token type: Token
/// expired`), Grove's Git credentials (`token expired and refresh is off; …`), the sign-in flow's
/// own ending (`Device code expired. …`), a feature that wants a grok.com login an API-key
/// session lacks (``Billing data requires auth with grok.com. Run `grok login` …``) and the
/// `/login` command's menu row. Nor are the texts of a credential no row holds: a team's
/// deployment key (`Your GROK_DEPLOYMENT_KEY is invalid or expired. Please contact a team
/// admin.`, `The deployment key was rejected. …`), which `grok setup` installs managed
/// configuration with, an admin's API-key policy (``API-key auth is disabled by your
/// administrator. …``), a configuration that offers no sign-in (`No login method available`), a
/// model endpoint's own key (`Authentication failed: the model endpoint rejected this build's
/// credential. …`), and a 401 that outlasts a login just renewed (`Auth recovery succeeded but …
/// still rejected (401); …`). A renewal (`accounts renew`, a fresh `grok login`) changes none of
/// those. Whether a set `GROK_DEPLOYMENT_KEY` also signs a session in ahead of the row's login
/// (the build's status texts list `You are authenticated via deployment key.` beside `You are
/// logged in with …`) is unmeasured, so a pooled line leaves it set.
const GROK_AUTH_WORDS: &[&str] = &[
    "not logged in. run `grok login`",
    "token expired. run `grok login`",
    "auth token is invalid or expired",
    "authentication rejected by server",
    "re-authentication required",
    "session expired. run `grok login`",
    "your session has expired",
    "authentication required. run `grok login`",
    "could not renew it in the background",
    "authentication could not be refreshed",
    "your team sign-in was rejected",
    "not signed in. run `grok login`",
    "not signed in. to authenticate without a browser",
    "not signed in \u{2014} run `grok login`",
    "you are not authenticated",
    "no cached auth token found",
    "authentication failed. run `grok login`",
    "no cached credentials found",
    "headless mode requires a grok.com session",
    "no session is available. run `grok login`",
    "signed out: /feedback works, and /login signs in",
    "compaction failed \u{2014} re-authenticate with /login",
    "authentication problem \u{2014} re-authenticate using /login",
];

/// Grok Build's usage walls and credit-exhaustion words from its 1.0.67 and 1.0.70 binaries.
/// The team's API rate limit keeps its full wording with either apostrophe. The bare strings
/// `spending-limit` and `spending limit` sit beside the compaction failure, but their error
/// grammar is unmeasured: a failed review can quote them without exhausting its account. They
/// alone therefore justify no cooldown. The complete compaction failure still matches its
/// `out of credits` words, alongside the upsells, exhaustion messages and error codes below.
const GROK_LIMIT_WORDS: &[&str] = &[
    "hit the rate limit for your plan",
    "hit your team's api rate limit",
    "hit your team\u{2019}s api rate limit",
    "ask a team admin to purchase more credits",
    "free grok build usage limit",
    "hit your free usage limit",
    "hit your weekly limit",
    "hit the credit limit for your plan",
    "hit your spending cap",
    "resubmit the last prompt once you have usage again",
    "out of credits",
    "usage balance exhausted",
    "usage limit reached",
    "free-usage-exhausted",
    "insufficient_quota",
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
    sign_in_page: SignInPage::Fixed("https://accounts.x.ai/sign-in"),
    web_session_key: None,
    measured: true,
    measured_on: Some("grok 1.0.67"),
    walls_measured_on: None,
    signin: Some(GROK_SIGNIN),
};

/// Kimi Code's texts for an ended login. `provider managed:kimi-code has no credential configured`
/// is what kimi 2.3.1's `-p` fails with where the home's `config.toml` names the managed login and
/// its token is gone (`docs/measured/kimi-credentials-2026-10-24.md` §5): the managed provider is
/// the one `kimi login` writes, so its missing token is the row's login ended. The same sentence
/// for another provider (`provider __kimi_env__ …`, an environment model with no key, or a
/// hand-written provider with none) is no login of the row's.
const KIMI_AUTH_WORDS: &[&str] = &[
    "re-login required",
    "no token for \"kimi-code\"",
    "run /login",
    "use /login to sign in",
    "provider managed:kimi-code has no credential configured",
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
/// host), its sessions, their locks, input history, folder trust and the user's own setup linked
/// to the default home; `config.toml`, `credentials`, the region marker, `device_id`, `logs`,
/// `cache`, `bin`, `session_index.jsonl` and `workspaces.json` stay each login's. Measured on
/// kimi 2.3.1 (`docs/measured/kimi-resume-2026-10-24.md`, no account signed in, a local stand-in
/// model): an overlay lists, opens, replays and continues a session the default home wrote, and
/// the default home sees the overlay's turns and sessions; a turn in one home refuses a second
/// open in the other only once `locks` is shared; a folder trusted in one is trusted in the other
/// only once `workspace-trust` is. `workspaces.json` is not linked: kimi replaces it (a write to
/// a temporary file renamed over it) at its first write in an overlay, and resume never needed
/// it, since each session's folder is named under `sessions`. `mcp.json` is linked though kimi
/// writes it the same way, at an MCP change made in an overlay ([`Shared::File`]). Each overlay
/// holds its own `.skip-migration-from-kimi-cli`: where `~/.kimi` holds the older kimi-cli's
/// data, kimi otherwise offers to move it into the overlay before its first screen, which no
/// unattended start answers (the lab record's migration section).
pub const KIMI_ACCOUNTS: Accounts = Accounts {
    seam: Seam::Overlay {
        var: "KIMI_CODE_HOME",
        shared: &[
            Shared::Dir("sessions"),
            Shared::Dir("locks"),
            Shared::Dir("user-history"),
            Shared::Dir("workspace-trust"),
            Shared::Dir("skills"),
            Shared::Dir("agents"),
            Shared::Dir("plugins"),
            Shared::File("mcp.json"),
            Shared::File("tui.toml"),
            Shared::Present("AGENTS.md"),
        ],
        markers: &[".skip-migration-from-kimi-cli"],
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
    sign_in_page: SignInPage::ByRegion {
        global: "https://www.kimi.ai/",
        mainland_cn: "https://www.kimi.com/",
    },
    // Both sites' script bundles keep `access_token` and `refresh_token` in `localStorage`
    // (design §5 (f)).
    web_session_key: Some("access_token"),
    measured: true,
    measured_on: Some("kimi 2.3.1"),
    // No signed-in session reached a wall, so a running session is not moved yet.
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
    // Measured on kimi 2.3.1 with `-p` (`docs/measured/kimi-credentials-2026-10-24.md`; no
    // account signed in, two loopback stand-ins): `KIMI_MODEL_NAME` with `KIMI_MODEL_API_KEY`
    // replaces the home's default model and its provider with an environment model of their
    // own, and with no key the run fails (`provider __kimi_env__ has no credential
    // configured`); a key without the name is never read. An `Authorization` line in
    // `KIMI_CODE_CUSTOM_HEADERS` replaces the provider's own bearer on every chat request.
    // `KIMI_API_KEY` is no variable kimi reads from its environment: it is a key of a provider's
    // `env` table in `config.toml`, and with it exported a provider with no key of its own still
    // has none; `KIMI_BASE_URL` is only an environment model's base. `KIMI_CODE_BASE_URL`,
    // `KIMI_CODE_OAUTH_HOST` and `KIMI_OAUTH_HOST` move the managed login's endpoints and, by
    // the binary (`resolveKimiCodeRuntimeAuth`; no OAuth login was measured), the credential
    // slot its token is read from; they carry no credential and are not listed.
    credential_env: &[
        "KIMI_MODEL_NAME",
        "KIMI_MODEL_API_KEY",
        "KIMI_CODE_CUSTOM_HEADERS",
    ],
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

/// All connector rows; [`connections`] yields them in `VENDORS` order. Each is a program atpkg's
/// `VENDORS` serves; [`GROK`] is one outside its default set, so a machine that never asked for
/// it reads it as off ([`super::facts::Kept::OptIn`]), not as an install it owes. [`KIMI`] is
/// served too and is not a connection yet: until its review and sign-in are tested here as Grok
/// Build's are (`connector_tests`, `review_tests`, `login_tests`), only its account pool
/// ([`Cli`]) reads it.
pub const ROWS: &[&Connector] = &[&CLAUDE, &CODEX, &AGY, &MUSE, &GROK];

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
