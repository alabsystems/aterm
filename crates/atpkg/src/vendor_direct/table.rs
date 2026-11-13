// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The compiled vendor table (design §1.1).

use serde::{Deserialize, Serialize};

use super::version::Version;
use crate::manifest::DigestAlg;

/// What authenticates a program's digest document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Anchor {
    /// `<v>/manifest.json` under Anthropic's compiled OpenPGP key.
    #[serde(rename = "anthropic-openpgp")]
    AnthropicOpenPgp,
    /// The release JSON on `releases.openai.com` agreeing with the GitHub release's `SHA256SUMS`.
    #[serde(rename = "openai-two-host")]
    OpenAiTwoHost,
    /// agy's Cloud Run updater manifest matching the per-version GCS `manifest.json` on `{version, url, sha512}`.
    #[serde(rename = "google-two-host")]
    GoogleTwoHost,
    /// muse's `muse-stable` channel on `api.meta.ai` paired with the lookaside release manifest's sha256 and size.
    #[serde(rename = "meta-developer-id")]
    MetaDeveloperId,
    /// kimi's CDN release manifest paired with the GitHub release manifest on `{version, tag, filename, sha256}`.
    #[serde(rename = "moonshot-two-host")]
    MoonshotTwoHost,
    /// What vouched for a grok build before [`Self::XaiNpmRegistry`]: the lane measured the
    /// bucket's raw bytes and the darwin team pin judged them. Kept only so the records those
    /// builds carry still read; no row names it.
    #[serde(rename = "xai-developer-id")]
    XaiDeveloperId,
    /// grok's `@xai-official/grok-<plat>` version document on npm's registry, signed by a
    /// compiled registry key over its tarball's sha512, for the version xAI's bucket pointer
    /// names; the binary decoded from the tarball must agree with the bucket's own copy, and on
    /// darwin its Developer ID team.
    #[serde(rename = "xai-npm-registry")]
    XaiNpmRegistry,
}

impl Anchor {
    /// Whether the documents vouch for the payload digest before a byte is fetched.
    #[must_use]
    pub(crate) const fn vouches_before_download(self) -> bool {
        !matches!(self, Self::MetaDeveloperId | Self::XaiDeveloperId)
    }

    /// Whether a record written under `recorded` reads as one of a row anchored by `self`: its
    /// own anchor, and the one it succeeded (a grok build xAI's Developer ID vouched for, staged
    /// before npm's registry did).
    #[must_use]
    pub(crate) const fn reads_record_of(self, recorded: Self) -> bool {
        matches!(
            (self, recorded),
            (Self::AnthropicOpenPgp, Self::AnthropicOpenPgp)
                | (Self::OpenAiTwoHost, Self::OpenAiTwoHost)
                | (Self::GoogleTwoHost, Self::GoogleTwoHost)
                | (Self::MetaDeveloperId, Self::MetaDeveloperId)
                | (Self::MoonshotTwoHost, Self::MoonshotTwoHost)
                | (Self::XaiDeveloperId, Self::XaiDeveloperId)
                | (
                    Self::XaiNpmRegistry,
                    Self::XaiNpmRegistry | Self::XaiDeveloperId
                )
        )
    }

    /// The payload digest this anchor vouches for.
    #[must_use]
    pub(crate) const fn digest_alg(self) -> DigestAlg {
        match self {
            Self::GoogleTwoHost | Self::XaiNpmRegistry => DigestAlg::Sha512,
            Self::AnthropicOpenPgp
            | Self::OpenAiTwoHost
            | Self::MetaDeveloperId
            | Self::MoonshotTwoHost
            | Self::XaiDeveloperId => DigestAlg::Sha256,
        }
    }
}

/// Where one kind of digest document is fetched, and where its answer may come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocPin {
    /// The document's URL starts with this (https, trailing `/`).
    pub prefix: &'static str,
    /// Hosts a redirect may end on.
    pub redirect_hosts: &'static [&'static str],
}

/// How a build this machine just landed is warmed ([`crate::warm`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Warm {
    /// The arguments after the program's name.
    pub argv: &'static [&'static str],
    /// The AI variables ([`aterm_types::domain::is_ai_env_var`]) the program is handed although every other child has them stripped.
    pub keep: &'static [&'static str],
    /// `(NAME, VALUE)` pairs set on the program, over anything the user's setup holds.
    pub env: &'static [(&'static str, &'static str)],
    /// The variable whose non-empty value opts the user out of exactly the traffic the warm makes.
    pub opt_out: Option<&'static str>,
    /// Where the program keeps its state on this account.
    pub state: Option<AccountState>,
    /// How the program shows that it is signed in on this account: a warm runs only then.
    pub login: Login,
}

/// How a warmed program shows that it is signed in on this account, in its environment and
/// files alone ([`Warm::login`]); nothing reads it by running the program. A program that is
/// not signed in is not warmed: what it does when it finds no login (a sign-in page in the
/// desk's browser, a prompt) is its vendor's to change, and nobody asked for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Login {
    /// Variables the program is handed ([`Warm::keep`]) whose non-empty value signs a launch in.
    pub env: &'static [&'static str],
    /// The variable naming the program's home.
    pub home_var: &'static str,
    /// The home under `$HOME` where [`Self::home_var`] is unset.
    pub home_default: &'static str,
    /// A file in the home that holds a login while it has bytes.
    pub files: &'static [&'static str],
    /// A document of the program's that names the signed-in account.
    pub profile: Option<Profile>,
}

/// A JSON document that names a program's signed-in account ([`Login::profile`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Profile {
    /// Its name in a home [`Login::home_var`] names.
    pub in_home: &'static str,
    /// Its path under `$HOME` where that variable is unset.
    pub under_home: &'static str,
    /// The top-level key that holds an object only while signed in.
    pub key: &'static str,
}

/// Where a warmed program keeps its state on this account ([`Warm::state`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountState {
    /// The variable naming its state directory.
    pub dir_var: &'static str,
    /// Its state under HOME when [`Self::dir_var`] is unset.
    pub under_home: &'static [&'static str],
}

/// A program that opens its vendor's sign-in page in the desk's browser on every signed-out
/// launch (Grok Build: through LaunchServices, whatever its `PATH`): under aterm its twin signs
/// the machine in once through `aterm harness signin --cli <cli> --wait` before an interactive
/// launch, and runs the program only once that exits 0 ([`crate::platform`]'s sign-in block).
/// Signed in is read the way the program reads it, from its environment and files alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SigninGate {
    /// The word `aterm harness signin --cli` takes.
    pub cli: &'static str,
    /// Variables whose non-empty value signs a launch in ahead of any login file.
    pub credential_env: &'static [&'static str],
    /// The variable naming the login file the program then reads in place of its home's.
    pub login_file_env: &'static str,
    /// The variable naming the program's home.
    pub home_env: &'static str,
    /// The home under `$HOME` where [`Self::home_env`] is unset.
    pub home_default: &'static str,
    /// The login file in the home.
    pub login_file: &'static str,
    /// Options that answer and exit, or run one prompt and exit: a launch carrying one before
    /// a literal `--` is never gated.
    pub one_shot: &'static [&'static str],
    /// The subcommands that start neither the program's interactive screen nor a sign-in of
    /// its own (a local read or edit, help, version, and its own `login`/`logout`, which a
    /// person types to sign in or out): a launch whose first word is one is never gated, nor is
    /// one of [`VendorSpec::self_update_verbs`]. Every other subcommand is gated, an unknown one
    /// too (fail closed: [`VendorSpec::yolo_skip`] names subcommands that take no permission
    /// flag, among them ones that open the interactive screen, such as Grok Build's
    /// `dashboard`).
    pub local: &'static [&'static str],
}

/// Grok Build's sign-in gate: `GROK_AUTH`/`XAI_API_KEY`, else `GROK_AUTH_PATH`'s file, else
/// `${GROK_HOME:-~/.grok}/auth.json` (aterm's `harness::grok_signin::signed_in`, the same
/// order); its one-shot options are 1.0.70's `--help` and `--version` spellings and the
/// one-prompt options of its `PagerArgs` (`crate::yolo::word_options`). Its local subcommands
/// are those 1.0.70's `--help` describes as a local read or edit (`doctor` "without starting
/// Grok", the `~/.grok` disk use, the configuration, a transcript export, persisted usage, MCP
/// and memory configuration, leader processes, worktrees, a trace export that falls back to a
/// local file without credentials), help, version, and its own `login` and `logout`. Gated:
/// `dashboard` (the interactive screen, "Sign in to open the dashboard"), `sessions` (it
/// restores one), `agent`, `models`, `plugin`, `setup`, `share`, `wrap`, the workers and hosts,
/// and any word not named here.
pub const GROK_SIGNIN_GATE: SigninGate = SigninGate {
    cli: "grok",
    credential_env: &["GROK_AUTH", "XAI_API_KEY"],
    login_file_env: "GROK_AUTH_PATH",
    home_env: "GROK_HOME",
    home_default: ".grok",
    login_file: "auth.json",
    one_shot: &[
        "-h",
        "--help",
        "-v",
        "-V",
        "--version",
        "-p",
        "--single",
        "--print",
        "--prompt-file",
        "--prompt-json",
    ],
    local: &[
        "login",
        "logout",
        "help",
        "version",
        "v",
        "completions",
        "doctor",
        "du",
        "disk-usage",
        "inspect",
        "export",
        "usage",
        "mcp",
        "memory",
        "leader",
        "worktree",
        "trace",
    ],
};

/// One vendor-direct program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VendorSpec {
    /// The program's name ([`crate::stub::AGENT_PROGRAMS`] is these names, in table order).
    pub program: &'static str,
    /// Who publishes it, for user-visible lines (`Anthropic latest`).
    pub vendor: &'static str,
    /// The product's own name.
    pub product: &'static str,
    /// The authored what-it-is line the pending stub prints while the install is on its way ([`crate::stub::describe`]).
    pub description: &'static str,
    /// The vendor's `latest` pointer.
    pub head_url: &'static str,
    /// What authenticates the digest.
    pub anchor: Anchor,
    /// The Apple Developer ID team every darwin Mach-O must be signed by.
    pub apple_team: &'static str,
    /// The tools the build exposes.
    pub exposes: &'static [&'static str],
    /// `(name, target)`.
    pub aliases: &'static [(&'static str, &'static str)],
    /// Every first-hop fetch URL must start with one of these (https, trailing `/`).
    pub url_prefixes: &'static [&'static str],
    /// The one query a fetch URL may carry.
    pub url_query: &'static [&'static str],
    /// Per digest document, where it is fetched and where its answer may come from.
    pub digest_docs: &'static [DocPin],
    /// `NAME=VALUE` entries the shims export.
    pub shim_env: &'static [&'static str],
    /// How a build this machine just landed is warmed.
    pub warm: Option<Warm>,
    /// The vendor's self-update verbs the program's twin hands to aterm's package manager ([`crate::selfupdate`]), exactly as typed as `$1`.
    pub self_update_verbs: &'static [&'static str],
    /// The vendor flags `--yolo` on the managed name expands to ([`crate::yolo`]).
    pub yolo_flags: &'static [&'static str],
    /// The tokens that already decide this launch's permission posture, so the default yolo injection ([`crate::yolo`]) leaves the argv as typed.
    pub yolo_markers: &'static [&'static str],
    /// Subcommands that default flag injection follows rather than precedes (e.g. muse's `exec` and `resume`).
    pub yolo_after: &'static [&'static str],
    /// First words (subcommands) the default injection never lands on.
    pub yolo_skip: &'static [&'static str],
    /// Whether live upgrade moves running sessions onto newly landed builds ([`crate::activation_notice`]).
    pub live_upgrade: bool,
    /// Whether versions use the revision form `x.y.z-R<n>[.<s>]` rather than plain `x.y.z`.
    pub revision_versions: bool,
    /// The oldest version installed.
    pub floor: Version,
    /// The newest legacy ALab index build of the program this code vouches for, or [`NEVER_INDEX_PINNED`].
    pub legacy_ceiling: u64,
    /// Its twin's sign-in gate under aterm, for a program that opens a browser on every signed-out launch ([`SigninGate`]).
    pub signin_gate: Option<SigninGate>,
}

impl VendorSpec {
    /// Whether a digest document requested at `requested` may be read, having been answered from `effective` (curl's effective URL).
    #[must_use]
    pub(crate) fn digest_doc_admitted(&self, requested: &str, effective: &str) -> bool {
        let Some(pin) = self
            .digest_docs
            .iter()
            .find(|pin| requested.starts_with(pin.prefix))
        else {
            return false;
        };
        crate::vendor::https_host(requested).is_some()
            && (effective == requested
                || crate::vendor::https_host(effective)
                    .is_some_and(|h| pin.redirect_hosts.contains(&h)))
    }

    /// Whether the vendor publishes a build of this program that the lane can verify for `triple`, the client's own.
    #[must_use]
    pub(crate) fn serves(&self, triple: &str) -> bool {
        super::resolve::serves(self.anchor, triple)
    }

    /// Whether a complete vendor build of `self`'s may stand in for `other`'s download of the same signed bytes ([`super::lane`]'s landing): another row under the same head whose tree has no alias of its own folded in. Today that is codex's build lending its tree to codex-old.
    #[must_use]
    pub(crate) fn lends_tree_to(&self, other: &Self) -> bool {
        self.program != other.program && self.head_url == other.head_url && self.aliases.is_empty()
    }

    /// [`Self::shim_env`] admitted under [`crate::shim_env::ShimEnv::admit`]'s rule (pinned by a test, so the fallback never fires).
    #[must_use]
    pub fn shim_env(&self) -> crate::shim_env::ShimEnv {
        let raw: Vec<String> = self.shim_env.iter().map(|s| (*s).to_string()).collect();
        crate::shim_env::ShimEnv::admit(&raw).unwrap_or(crate::shim_env::ShimEnv::NONE)
    }
}

/// A compile-time version; an out-of-range literal fails the build.
const fn floor(major: u32, minor: u32, patch: u32) -> Version {
    match Version::new(major, minor, patch) {
        Some(v) => v,
        None => panic!("vendor floor out of range"),
    }
}

/// A compile-time revision-form version (`x.y.z-R<n>[.<s>]`); an out-of-range literal fails
/// the build.
const fn revision_floor(
    major: u32,
    minor: u32,
    patch: u32,
    revision: u32,
    sub: Option<u32>,
) -> Version {
    match Version::with_revision(major, minor, patch, revision, sub) {
        Some(v) => v,
        None => panic!("vendor floor out of range"),
    }
}

/// agy's per-platform manifests on its Cloud Run updater service, as agy's own updater spells them.
macro_rules! agy_manifests {
    () => {
        "https://antigravity-cli-auto-updater-974169037036.us-central1.run.app/manifests/"
    };
}

/// agy's head: the manifest for this client's own platform — the one agy's own updater
/// obeys while a rollout is staged — chosen by the arms `cli::current_triple` reports
/// under.
#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
const AGY_HEAD: &str = concat!(agy_manifests!(), "darwin_arm64.json");
#[cfg(all(target_arch = "x86_64", target_os = "macos"))]
const AGY_HEAD: &str = concat!(agy_manifests!(), "darwin_amd64.json");
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
const AGY_HEAD: &str = concat!(agy_manifests!(), "linux_amd64.json");
#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
const AGY_HEAD: &str = concat!(agy_manifests!(), "linux_arm64.json");
#[cfg(all(target_arch = "x86_64", target_os = "windows"))]
const AGY_HEAD: &str = concat!(agy_manifests!(), "windows_amd64.json");
#[cfg(all(target_arch = "aarch64", target_os = "windows"))]
const AGY_HEAD: &str = concat!(agy_manifests!(), "windows_arm64.json");
#[cfg(not(any(
    all(target_arch = "aarch64", target_os = "macos"),
    all(target_arch = "x86_64", target_os = "macos"),
    all(target_arch = "x86_64", target_os = "linux"),
    all(target_arch = "aarch64", target_os = "linux"),
    all(target_arch = "x86_64", target_os = "windows"),
    all(target_arch = "aarch64", target_os = "windows"),
)))]
const AGY_HEAD: &str = agy_manifests!();

/// The final legacy pins this code vouches for ([`VendorSpec::legacy_ceiling`]), from live index 46.
pub(crate) const LEGACY_CEILING: u64 = 2_026_092_401;

/// The [`VendorSpec::legacy_ceiling`] of a program no ALab index ever pinned.
pub const NEVER_INDEX_PINNED: u64 = 0;

/// OpenAI's Codex CLI: the row of `codex`, and of `codex-old` beside it.
const OPENAI_CODEX: VendorSpec = VendorSpec {
    program: "codex",
    vendor: "OpenAI",
    product: "Codex CLI",
    description: "OpenAI Codex CLI — Apache-2.0, ~110 MB (~290 MB on disk), downloaded \
                  from github.com/openai/codex",
    head_url: "https://releases.openai.com/codex/channels/latest",
    anchor: Anchor::OpenAiTwoHost,
    apple_team: "2DC432GLL2",
    exposes: &["codex"],
    aliases: &[],
    url_prefixes: &[
        "https://releases.openai.com/codex/",
        "https://github.com/openai/codex/releases/download/",
    ],
    url_query: &[],
    digest_docs: &[
        DocPin {
            prefix: "https://releases.openai.com/codex/",
            redirect_hosts: &[],
        },
        DocPin {
            prefix: "https://github.com/openai/codex/releases/download/",
            redirect_hosts: &[
                "release-assets.githubusercontent.com",
                "objects.githubusercontent.com",
            ],
        },
    ],
    shim_env: &[],
    warm: Some(Warm {
        argv: &["debug", "models"],
        keep: &["CODEX_HOME"],
        env: &[],
        opt_out: None,
        state: Some(AccountState {
            dir_var: "CODEX_HOME",
            under_home: &[".codex"],
        }),
        // Codex keeps its login in `auth.json` unless its config moves it to the OS keyring,
        // which no file shows: such an account is not warmed.
        login: Login {
            env: &[],
            home_var: "CODEX_HOME",
            home_default: ".codex",
            files: &["auth.json"],
            profile: None,
        },
    }),
    self_update_verbs: &["update"],
    yolo_flags: &["--dangerously-bypass-approvals-and-sandbox"],
    yolo_markers: &[
        "--dangerously-bypass-approvals-and-sandbox",
        "--sandbox",
        "--sandbox=",
        "-s",
        "--ask-for-approval",
        "--ask-for-approval=",
        "-a",
        "--approve-for-me",
        "--not-so-yolo",
    ],
    yolo_after: &[],
    yolo_skip: &[],
    live_upgrade: true,
    revision_versions: false,
    floor: floor(0, 156, 1),
    legacy_ceiling: LEGACY_CEILING,
    signin_gate: None,
};

/// The agent programs, each fetched from its own vendor.
pub const VENDORS: &[VendorSpec] = &[
    VendorSpec {
        program: "claude",
        vendor: "Anthropic",
        product: "Claude Code",
        description: "Anthropic Claude Code — proprietary, ~200 MB, downloaded from \
                      downloads.claude.ai",
        head_url: "https://downloads.claude.ai/claude-code-releases/latest",
        anchor: Anchor::AnthropicOpenPgp,
        apple_team: "Q6L2SF6YDW",
        exposes: &["claude"],
        aliases: &[],
        url_prefixes: &["https://downloads.claude.ai/claude-code-releases/"],
        url_query: &[],
        digest_docs: &[DocPin {
            prefix: "https://downloads.claude.ai/claude-code-releases/",
            redirect_hosts: &[],
        }],
        shim_env: &[
            "DISABLE_AUTOUPDATER=1",
            "CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1",
        ],
        warm: Some(Warm {
            argv: &[
                "-p",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
                "--no-session-persistence",
                "--strict-mcp-config",
                "--settings",
                r#"{"disableAllHooks":true}"#,
            ],
            keep: &[
                "CLAUDE_CONFIG_DIR",
                "ANTHROPIC_BASE_URL",
                "CLAUDE_CODE_USE_BEDROCK",
                "CLAUDE_CODE_USE_VERTEX",
            ],
            env: &[("ENABLE_CLAUDEAI_MCP_SERVERS", "false")],
            opt_out: Some("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"),
            state: Some(AccountState {
                dir_var: "CLAUDE_CONFIG_DIR",
                under_home: &[".claude", ".claude.json"],
            }),
            // Claude Code names its signed-in account in `.claude.json`'s `oauthAccount`
            // (its token is in the login keychain on a Mac, in `.credentials.json` elsewhere);
            // an API key in the environment is stripped before the warm, so it signs nothing in.
            login: Login {
                env: &["CLAUDE_CODE_USE_BEDROCK", "CLAUDE_CODE_USE_VERTEX"],
                home_var: "CLAUDE_CONFIG_DIR",
                home_default: ".claude",
                files: &[".credentials.json"],
                profile: Some(Profile {
                    in_home: ".claude.json",
                    under_home: ".claude.json",
                    key: "oauthAccount",
                }),
            },
        }),
        self_update_verbs: &["update", "upgrade", "install"],
        yolo_flags: &["--dangerously-skip-permissions"],
        yolo_markers: &[
            "--dangerously-skip-permissions",
            "--allow-dangerously-skip-permissions",
            "--permission-mode",
            "--permission-mode=",
            "--permission-prompts",
            "--permission-prompts=",
            "--restricted",
            "--allowedTools",
            "--allowedTools=",
            "--allowed-tools",
            "--allowed-tools=",
            "--disallowedTools",
            "--disallowedTools=",
            "--disallowed-tools",
            "--disallowed-tools=",
            "--tools",
            "--tools=",
            "--settings",
            "--settings=",
            "--setting-sources",
            "--setting-sources=",
            "--resume",
            "--resume=",
            "-r",
            "--continue",
            "-c",
        ],
        yolo_after: &[],
        yolo_skip: &[],
        live_upgrade: true,
        revision_versions: false,
        floor: floor(2, 1, 281),
        legacy_ceiling: LEGACY_CEILING,
        signin_gate: None,
    },
    OPENAI_CODEX,
    VendorSpec {
        program: "agy",
        vendor: "Google",
        product: "Antigravity CLI",
        description: "Google Antigravity CLI — proprietary, ~55 MB (~185 MB on disk), \
                      downloaded from storage.googleapis.com",
        head_url: AGY_HEAD,
        anchor: Anchor::GoogleTwoHost,
        apple_team: "EQHXZ8M8AV",
        exposes: &["agy"],
        aliases: &[],
        url_prefixes: &[
            agy_manifests!(),
            "https://storage.googleapis.com/antigravity-public/antigravity-cli/",
        ],
        url_query: &[],
        digest_docs: &[
            DocPin {
                prefix: agy_manifests!(),
                redirect_hosts: &[],
            },
            DocPin {
                prefix: "https://storage.googleapis.com/antigravity-public/antigravity-cli/",
                redirect_hosts: &[],
            },
        ],
        shim_env: &["AGY_CLI_DISABLE_AUTO_UPDATE=true"],
        warm: None,
        self_update_verbs: &["update", "install"],
        yolo_flags: &["--dangerously-skip-permissions"],
        yolo_markers: &[
            "--dangerously-skip-permissions",
            "--mode",
            "--mode=",
            "--sandbox",
        ],
        yolo_after: &[],
        yolo_skip: &[],
        live_upgrade: true,
        revision_versions: false,
        floor: floor(1, 3, 6),
        legacy_ceiling: NEVER_INDEX_PINNED,
        signin_gate: None,
    },
    VendorSpec {
        program: "muse",
        vendor: "Meta",
        product: "Muse Code",
        description: "Meta Muse Code — proprietary, ~370 MB, downloaded from \
                      lookaside.facebook.com",
        head_url: "https://api.meta.ai/muse-code/channels/muse-stable",
        anchor: Anchor::MetaDeveloperId,
        apple_team: "V9WTTPBFK9",
        exposes: &["muse"],
        aliases: &[],
        url_prefixes: &[
            "https://api.meta.ai/muse-code/channels/",
            "https://lookaside.facebook.com/lookaside/muse/download/",
        ],
        url_query: &["channel", "version", "file"],
        digest_docs: &[
            DocPin {
                prefix: "https://api.meta.ai/muse-code/channels/",
                redirect_hosts: &[],
            },
            DocPin {
                prefix: "https://lookaside.facebook.com/lookaside/muse/download/",
                redirect_hosts: &[],
            },
        ],
        shim_env: &[],
        warm: None,
        self_update_verbs: &[],
        yolo_flags: &[
            "--disable-approval",
            "--disable-sandbox",
            "--trust-workspace",
            "--disable-request-user-input",
        ],
        yolo_markers: &[
            "--disable-approval",
            "--disable-sandbox",
            "--trust-workspace",
            "--disable-request-user-input",
            "--approval-mode",
            "--approval-mode=",
            "--permission-profile",
            "--permission-profile=",
            "--approval-judge",
            "--approval-judge=",
            "--sandbox-network",
            "--sandbox-network=",
            "--disable-write",
            "--disable-shell",
            "--config",
            "--config=",
            "-c",
        ],
        yolo_after: &["exec", "resume"],
        yolo_skip: &[
            "resume",
            "config",
            "export",
            "trace",
            "model-profile",
            "skills",
            "voice",
            "plugins",
            "sandbox",
            "schema",
            "serve",
            "session-message",
            "mcp",
            "auth",
            "login",
            "logout",
            "usage",
            "init",
        ],
        live_upgrade: true,
        revision_versions: true,
        floor: revision_floor(1, 4, 9, 6297, Some(1)),
        legacy_ceiling: NEVER_INDEX_PINNED,
        signin_gate: None,
    },
    VendorSpec {
        program: "codex-old",
        description: "OpenAI Codex CLI, OpenAI's own build beside codex — Apache-2.0, ~110 MB \
                      (~290 MB on disk), downloaded from github.com/openai/codex",
        exposes: &["codex-old"],
        aliases: &[("codex-old", "codex")],
        warm: None,
        live_upgrade: false,
        floor: floor(0, 163, 0),
        legacy_ceiling: NEVER_INDEX_PINNED,
        ..OPENAI_CODEX
    },
    VendorSpec {
        program: "grok",
        vendor: "xAI",
        product: "Grok Build",
        description: "xAI Grok Build — Apache-2.0, ~103 MB (~180 MB on disk), downloaded \
                      from registry.npmjs.org and checked against \
                      storage.googleapis.com/grok-build-public-artifacts",
        head_url: "https://storage.googleapis.com/grok-build-public-artifacts/cli/stable",
        anchor: Anchor::XaiNpmRegistry,
        apple_team: "5Y6N3AJ54S",
        exposes: &["grok"],
        aliases: &[],
        url_prefixes: &[
            "https://storage.googleapis.com/grok-build-public-artifacts/cli/",
            "https://registry.npmjs.org/@xai-official/grok-darwin-arm64/",
            "https://registry.npmjs.org/@xai-official/grok-darwin-x64/",
            "https://registry.npmjs.org/@xai-official/grok-linux-x64/",
            "https://registry.npmjs.org/@xai-official/grok-linux-arm64/",
        ],
        url_query: &[],
        digest_docs: &[
            DocPin {
                prefix: "https://storage.googleapis.com/grok-build-public-artifacts/cli/",
                redirect_hosts: &[],
            },
            DocPin {
                prefix: "https://registry.npmjs.org/@xai-official/grok-darwin-arm64/",
                redirect_hosts: &[],
            },
            DocPin {
                prefix: "https://registry.npmjs.org/@xai-official/grok-darwin-x64/",
                redirect_hosts: &[],
            },
            DocPin {
                prefix: "https://registry.npmjs.org/@xai-official/grok-linux-x64/",
                redirect_hosts: &[],
            },
            DocPin {
                prefix: "https://registry.npmjs.org/@xai-official/grok-linux-arm64/",
                redirect_hosts: &[],
            },
        ],
        shim_env: &["GROK_DISABLE_AUTOUPDATER=1"],
        warm: None,
        self_update_verbs: &["update"],
        yolo_flags: &["--always-approve"],
        yolo_markers: &[
            "--always-approve",
            "--dangerously-skip-permissions",
            "--permission-mode",
            "--permission-mode=",
            "--sandbox",
            "--sandbox=",
            "--allow",
            "--allow=",
            "--allowedTools",
            "--allowedTools=",
            "--deny",
            "--deny=",
            "--disallowedTools",
            "--disallowedTools=",
            "--disallowed-tools",
            "--disallowed-tools=",
        ],
        yolo_after: &["agent"],
        // The 1.0.45 source's `Command` words, and 1.0.67's `worker`, `cursor-worker`,
        // `clone`, `ssh` and `host-server` (its strings name each), and clap's `help`.
        yolo_skip: &[
            "inspect",
            "doctor",
            "leader",
            "login",
            "logout",
            "mcp",
            "plugin",
            "memory",
            "models",
            "sessions",
            "usage",
            "setup",
            "share",
            "wrap",
            "export",
            "trace",
            "version",
            "v",
            "completions",
            "worktree",
            "du",
            "disk-usage",
            "workspace",
            "dashboard",
            "worker",
            "cursor-worker",
            "clone",
            "ssh",
            "host-server",
            "help",
        ],
        live_upgrade: false,
        revision_versions: false,
        floor: floor(1, 0, 67),
        legacy_ceiling: NEVER_INDEX_PINNED,
        signin_gate: Some(GROK_SIGNIN_GATE),
    },
    VendorSpec {
        program: "kimi",
        vendor: "Moonshot AI",
        product: "Kimi Code",
        description: "Moonshot AI Kimi Code — MIT, ~190 MB, downloaded from cdn.kimi.com",
        head_url: "https://cdn.kimi.com/kimi-code/latest",
        anchor: Anchor::MoonshotTwoHost,
        apple_team: "2J9472RW75",
        exposes: &["kimi"],
        aliases: &[],
        url_prefixes: &[
            "https://cdn.kimi.com/kimi-code/",
            "https://github.com/MoonshotAI/kimi-code/releases/download/",
        ],
        url_query: &[],
        digest_docs: &[
            DocPin {
                prefix: "https://cdn.kimi.com/kimi-code/",
                redirect_hosts: &[],
            },
            DocPin {
                prefix: "https://github.com/MoonshotAI/kimi-code/releases/download/",
                redirect_hosts: &[
                    "release-assets.githubusercontent.com",
                    "objects.githubusercontent.com",
                ],
            },
        ],
        shim_env: &["KIMI_CODE_NO_AUTO_UPDATE=1"],
        warm: None,
        self_update_verbs: &["upgrade", "update"],
        yolo_flags: &["--auto"],
        yolo_markers: &[
            "--auto",
            "-y",
            "--yes",
            "--auto-approve",
            "--plan",
            "-p",
            "--prompt",
            "--prompt=",
            "-S",
            "--session",
            "--session=",
            "-r",
            "--resume",
            "--resume=",
            "-c",
            "--continue",
            "-C",
        ],
        yolo_after: &[],
        yolo_skip: &[
            "export",
            "fork",
            "provider",
            "session",
            "acp",
            "web",
            "login",
            "doctor",
            "vis",
            "install-desktop",
            "app",
            "migrate",
        ],
        live_upgrade: false,
        revision_versions: false,
        floor: floor(2, 3, 1),
        legacy_ceiling: NEVER_INDEX_PINNED,
        signin_gate: None,
    },
];

/// The row for `program`, or `None` for a program the index manages.
#[must_use]
pub fn spec(program: &str) -> Option<&'static VendorSpec> {
    VENDORS.iter().find(|s| s.program == program)
}

/// Whether `program` is updated vendor-direct rather than through the ALab index.
#[must_use]
pub fn is_vendor(program: &str) -> bool {
    spec(program).is_some()
}

/// Whether this platform has a build of `program` (muse is macOS only), by the rule the install and the doctor read.
#[must_use]
pub fn served_here(program: &str) -> bool {
    spec(program).is_some_and(|s| s.serves(crate::cli::current_triple()))
}

#[cfg(test)]
#[path = "table_tests.rs"]
mod tests;
