// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The compiled vendor table (design §1.1): one row per vendor-direct program. Everything
//! that decides where bytes may come from and who vouches for them is here, in source, so
//! no index publish can move it or stall it.

use serde::{Deserialize, Serialize};

use super::version::Version;
use crate::manifest::DigestAlg;

/// What authenticates a program's digest document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Anchor {
    /// `<v>/manifest.json` under Anthropic's compiled OpenPGP key.
    #[serde(rename = "anthropic-openpgp")]
    AnthropicOpenPgp,
    /// The release JSON on `releases.openai.com` agreeing with the GitHub release's
    /// `SHA256SUMS` — two independent hosts, no signature.
    #[serde(rename = "openai-two-host")]
    OpenAiTwoHost,
    /// agy's per-platform manifest on Google's Cloud Run updater (the one agy's own
    /// updater obeys, for this client's platform) agreeing on `{version, url, sha512}`
    /// with the per-version `manifest.json` beside the payload in Google Cloud Storage,
    /// the payload exactly at its compiled bucket path — two services, no signature.
    /// Residual: both hosts are Google's, so this is one vendor's word from two services;
    /// on darwin the Developer ID team pin binds the bytes to Google independently of TLS.
    #[serde(rename = "google-two-host")]
    GoogleTwoHost,
    /// muse's `muse-stable` channel on `api.meta.ai` naming the version, and the release
    /// manifest at the client-built lookaside URL carrying the payload's sha256 and size —
    /// TLS to Meta's hosts, no signature, and no second host: the manifest comes from the
    /// host that serves the bytes. Residual: TLS alone vouches for the digest, so the
    /// anchor is the Developer ID team pin every darwin Mach-O must pass, and no other
    /// platform is served.
    #[serde(rename = "meta-developer-id")]
    MetaDeveloperId,
}

impl Anchor {
    /// Whether the documents vouch for the payload digest before a byte is fetched. muse's
    /// come from the host that serves the bytes, so its digest is vouched for only once the
    /// Developer ID team pin passed the bytes it names.
    #[must_use]
    pub(crate) const fn vouches_before_download(self) -> bool {
        !matches!(self, Self::MetaDeveloperId)
    }

    /// The payload digest this anchor vouches for: Google publishes only a sha512.
    #[must_use]
    pub(crate) const fn digest_alg(self) -> DigestAlg {
        match self {
            Self::GoogleTwoHost => DigestAlg::Sha512,
            Self::AnthropicOpenPgp | Self::OpenAiTwoHost | Self::MetaDeveloperId => {
                DigestAlg::Sha256
            }
        }
    }
}

/// Where one kind of digest document is fetched, and where its answer may come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocPin {
    /// The document's URL starts with this (https, trailing `/`); a program's pins are
    /// disjoint, so a URL names exactly one.
    pub prefix: &'static str,
    /// Hosts a redirect may end on. Empty: the document must be answered at the URL
    /// requested, with no redirect at all.
    pub redirect_hosts: &'static [&'static str],
}

/// How a build this machine just landed is warmed ([`crate::warm`]): one headless run that
/// refreshes the vendor CLI's account-bound caches (the served model catalog, feature flags)
/// before the user's first launch, and never starts a model turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Warm {
    /// The arguments after the program's name.
    pub argv: &'static [&'static str],
    /// The AI variables ([`aterm_types::domain::is_ai_env_var`]) the program is handed
    /// although every other child has them stripped: not a session's state but the user's
    /// own setup of this program — where it keeps its config and caches (the files the warm
    /// refreshes) and which endpoint it talks to. Every other AI variable, another vendor's
    /// among them, is kept from it.
    pub keep: &'static [&'static str],
    /// `(NAME, VALUE)` pairs set on the program, over anything the user's setup holds.
    pub env: &'static [(&'static str, &'static str)],
    /// The variable whose non-empty value opts the user out of exactly the traffic the warm
    /// makes: set, the program is not warmed. A variable the program honours itself
    /// (`DISABLE_TELEMETRY`) is not named here: it reaches the program.
    pub opt_out: Option<&'static str>,
    /// Where the program keeps its state on this account: none of it present, the program
    /// has never run here, there is nothing to refresh, and it is not warmed. `None`: it is
    /// always warmed.
    pub state: Option<AccountState>,
}

/// Where a warmed program keeps its state on this account ([`Warm::state`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountState {
    /// The variable naming its state directory; set and non-empty, that directory alone is
    /// its state.
    pub dir_var: &'static str,
    /// Its state under HOME when [`Self::dir_var`] is unset: any one present is enough.
    pub under_home: &'static [&'static str],
}

/// One vendor-direct program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VendorSpec {
    /// The program's name ([`crate::stub::AGENT_PROGRAMS`] is these names, in table order).
    pub program: &'static str,
    /// Who publishes it, for user-visible lines (`Anthropic latest`).
    pub vendor: &'static str,
    /// The product's own name.
    pub product: &'static str,
    /// The authored what-it-is line the pending stub prints while the install is on its way
    /// ([`crate::stub::describe`]): the vendor, the license, the rough size and the host the
    /// bytes come from — descriptively ("Anthropic Claude Code"), never as an ALab mark.
    pub description: &'static str,
    /// The vendor's `latest` pointer — a hint only; everything it names is re-verified.
    pub head_url: &'static str,
    /// What authenticates the digest.
    pub anchor: Anchor,
    /// The Apple Developer ID team every darwin Mach-O must be signed by.
    pub apple_team: &'static str,
    /// The tools the build exposes; never taken from the index.
    pub exposes: &'static [&'static str],
    /// `(name, target)`: an exposed `name` whose executable the vendor ships under another
    /// name, laid in the build as `bin/<name>`, a REGULAR-FILE clone of the vendor's own
    /// `bin/<target>`, before the Developer ID gate judges the tree
    /// ([`crate::install::StageHooks::aliases`]) — never a symlink, which the gate refuses
    /// (`crate::macho::find_macho`), and never a shim into another program's store, which
    /// would make this program's `current`, rollback and gc another's. Each `name` is one
    /// of [`Self::exposes`] and each `target` is not. Compiled only, never taken from the
    /// index. Empty: the build runs every exposed tool under its own name.
    pub aliases: &'static [(&'static str, &'static str)],
    /// Every first-hop fetch URL must start with one of these (https, trailing `/`).
    pub url_prefixes: &'static [&'static str],
    /// The one query a fetch URL may carry: these keys, in this order, each with a value,
    /// right after a prefix (`<prefix>?channel=…&version=…&file=…`;
    /// [`crate::vendor::vendor_direct_url_allowed`]). Empty: no URL carries a query.
    pub url_query: &'static [&'static str],
    /// Per digest document, where it is fetched and where its answer may come from.
    pub digest_docs: &'static [DocPin],
    /// `NAME=VALUE` entries the shims export; never taken from the index.
    pub shim_env: &'static [&'static str],
    /// How a build this machine just landed is warmed; `None`: it never is. Compiled only,
    /// never taken from the index.
    pub warm: Option<Warm>,
    /// The vendor's self-update verbs the program's twin hands to aterm's package manager
    /// ([`crate::selfupdate`]), exactly as typed as `$1`; each must pass
    /// [`crate::selfupdate::is_shell_safe_verb`] (it lands in a `case` pattern). Empty: the
    /// program has no self-updater and its twin carries no block. Where each was measured
    /// is the table in `docs/DESIGN-vendor-self-update-2026-09-19.md`.
    pub self_update_verbs: &'static [&'static str],
    /// The vendor flags `--yolo` on the managed name expands to ([`crate::yolo`]) — the
    /// program's own "every approval and sandbox off" spelling, measured from its
    /// `--help` on the version at this row's floor or later. Each must pass
    /// [`crate::yolo::is_shell_safe_flag`] (it is embedded quoted in the twin's block).
    /// EMPTY: the program's twin carries no `--yolo` block — an explicit decision per
    /// row, like [`Self::self_update_verbs`].
    pub yolo_flags: &'static [&'static str],
    /// The tokens that already decide this launch's permission posture, so the
    /// default yolo injection ([`crate::yolo`]) leaves the argv as typed: the
    /// row's own [`Self::yolo_flags`], its permission-mode and sandbox words, and
    /// their `--word=` spellings (an entry ending in `=`). Each must pass
    /// [`crate::yolo::is_shell_safe_marker`]. The renderer refuses a `--yolo`
    /// block with no markers: a default that cannot tell a decided launch from an
    /// undecided one would silently upgrade the harness's own resume lines.
    pub yolo_markers: &'static [&'static str],
    /// First words (subcommands) the default injection goes AFTER rather than in
    /// front of, because the program takes its safety switches only behind them
    /// (muse's `exec` and `resume`, measured 1.4.11, 2026-10-19); an explicit
    /// `--yolo` typed ahead of one expands there too. Each must pass
    /// [`crate::selfupdate::is_shell_safe_verb`].
    pub yolo_after: &'static [&'static str],
    /// First words (subcommands) the default injection never lands on: words
    /// that are not sessions and take no safety switches (muse's `skills`,
    /// `auth`, …: switches in front are `invalid TUI options`, measured
    /// 1.4.11, 2026-10-19), and words whose launch's posture the SESSION owns
    /// (a resume: the flags would override the choice the session kept). A
    /// word may also sit in [`Self::yolo_after`] (muse's `resume`): there an
    /// explicit `--yolo` still expands, behind the word. Each must pass
    /// [`crate::selfupdate::is_shell_safe_verb`].
    pub yolo_skip: &'static [&'static str],
    /// Whether aterm's live upgrade moves a running session of this program onto a build a
    /// pass lands, so a pass that moves the program's `agents/` twin wakes the window's host
    /// ([`crate::activation_notice`]). `false` for a program no harness supervises: no
    /// session waits on its landing.
    pub live_upgrade: bool,
    /// Whether the program's versions are spelled in the revision form `x.y.z-R<n>[.<s>]`
    /// (the same `x.y.z` rebuilt under another `R`) rather than plain `x.y.z`. A head
    /// spelled in the other form is refused, never ordered against this one.
    pub revision_versions: bool,
    /// The oldest version installed: a frozen or replayed CDN cannot hand a new machine an
    /// old build. It is the version the final legacy pin names, so it is at or above every
    /// legacy pin up to [`Self::legacy_ceiling`] (with no pin, the head when the row was
    /// written).
    pub floor: Version,
    /// The newest legacy ALab index build of the program this code vouches for, or
    /// [`NEVER_INDEX_PINNED`]. A legacy build from 1 up to it whose version is unread is
    /// replaced by any admissible head, with no index and no downgrade; any other may carry
    /// a version above the floor, so the lane keeps it until its version is read
    /// (`legacy_facts`) or a person asks.
    pub legacy_ceiling: u64,
}

impl VendorSpec {
    /// Whether a digest document requested at `requested` may be read, having been
    /// answered from `effective` (curl's effective URL): `requested` is https under one of
    /// [`Self::digest_docs`], and `effective` is `requested` itself or an admissible https
    /// URL on one of that pin's redirect hosts.
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

    /// Whether the vendor publishes a build of this program that the lane can verify for
    /// `triple`, the client's own. A program that is not served is the canonical
    /// `unavailable on <target>` state: no head is asked for it, no stub promises it and
    /// no pass installs it.
    #[must_use]
    pub(crate) fn serves(&self, triple: &str) -> bool {
        super::resolve::serves(self.anchor, triple)
    }

    /// [`Self::shim_env`] admitted under [`crate::shim_env::ShimEnv::admit`]'s rule
    /// (pinned by a test, so the fallback never fires).
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

/// agy's per-platform manifests on its Cloud Run updater service, as agy's own updater
/// spells them: `<this>/<os>_<arch>[_musl].json`.
macro_rules! agy_manifests {
    () => {
        "https://antigravity-cli-auto-updater-974169037036.us-central1.run.app/manifests/"
    };
}

/// agy's head: the manifest for this client's own platform — the one agy's own updater
/// obeys while a rollout is staged — chosen by the arms `cli::current_triple` reports
/// under. A platform with none of them gets the bare prefix, which no fetch admits.
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

/// The final legacy pins this code vouches for ([`VendorSpec::legacy_ceiling`]), from live
/// index 46: claude 2026092401 (2.1.281) and codex 2026092301 (0.156.1), whose versions are
/// the floors. An index may pin either program above the ceiling. A legacy build above it
/// has its version read once from its signed manifest, under any verified index whether or
/// not that index still names the program, and kept in the stamp (`lane::legacy_facts`),
/// then is decided like any read build; while that read fails it is kept (`LegacyUnread`)
/// and the next pass reads again. Raise the ceiling and the floors together, never one
/// without the other.
pub(crate) const LEGACY_CEILING: u64 = 2_026_092_401;

/// The [`VendorSpec::legacy_ceiling`] of a program no ALab index ever pinned: no legacy
/// build of it is vouched for by its floor, so a non-vendor build in its store is kept,
/// unread, until a person asks.
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
        // The release JSON: releases.openai.com itself, never redirected — else both
        // halves of the two-host agreement could come from GitHub.
        DocPin {
            prefix: "https://releases.openai.com/codex/",
            redirect_hosts: &[],
        },
        // `SHA256SUMS`: github.com answers with a redirect to its release-asset
        // storage (measured for rust-v0.156.0).
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
        // Lists the served models headless and rewrites `~/.codex/models_cache.json`,
        // with no model turn (measured on 0.156.1). A debug subcommand: a release
        // that drops it fails the warm, silently.
        argv: &["debug", "models"],
        // The config home (an aterm identity session sets it after its own strip).
        keep: &["CODEX_HOME"],
        env: &[],
        opt_out: None,
        state: Some(AccountState {
            dir_var: "CODEX_HOME",
            under_home: &[".codex"],
        }),
    }),
    self_update_verbs: &["update"],
    // codex 0.163 parses `--yolo` itself (measured 2026-10-19); the expansion is
    // still spelled in full so the echo names the posture exactly.
    yolo_flags: &["--dangerously-bypass-approvals-and-sandbox"],
    // The bypass flag repeated is a clap refusal (exit 2, measured 0.163), so
    // its presence must suppress any second copy, by the flag or by default.
    // The sandbox/approval words (attached short spellings included, by
    // `yolo::marker_matches` — `-sread-only` is `-s read-only` to clap),
    // and the two config carriers: a `-c` override or a `--profile` can
    // spell the launch's whole posture (`sandbox_mode=`, measured in the
    // harness's own relaunch corpus), so either leaves the launch typed.
    yolo_markers: &[
        "--dangerously-bypass-approvals-and-sandbox",
        "--sandbox",
        "--sandbox=",
        "-s",
        "--ask-for-approval",
        "--ask-for-approval=",
        "-a",
        "--approve-for-me",
        "--config",
        "--config=",
        "-c",
        "--profile",
        "--profile=",
        "-p",
    ],
    yolo_after: &[],
    // A resumed or forked thread keeps its own settings; the default
    // never overrides them (an explicit `--yolo` still expands).
    yolo_skip: &["resume", "fork"],
    live_upgrade: true,
    revision_versions: false,
    floor: floor(0, 156, 1),
    legacy_ceiling: LEGACY_CEILING,
};

/// The agent programs, each fetched from its own vendor: the one roster of them
/// ([`crate::stub::AGENT_PROGRAMS`] is its names, in this order).
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
        // `<v>/manifest.json` and its `.sig`, answered in place.
        digest_docs: &[DocPin {
            prefix: "https://downloads.claude.ai/claude-code-releases/",
            redirect_hosts: &[],
        }],
        // Claude's own updater writes a copy under `~/.local` that this name never runs.
        shim_env: &[
            "DISABLE_AUTOUPDATER=1",
            "CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1",
        ],
        warm: Some(Warm {
            // Print mode reading stream-json from a stdin the warm never writes (measured on
            // 2.1.280): the bootstrap, feature and model-catalog fetches run, and no
            // `/v1/messages` call, transcript, hook or MCP server does; the warm closes stdin
            // and the CLI exits 0. Never `--bare` (it skips those fetches) and never
            // `--init-only` (it exits racing them).
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
            // The config home (an aterm identity session sets it after its own strip) and
            // the endpoint choice.
            keep: &[
                "CLAUDE_CONFIG_DIR",
                "ANTHROPIC_BASE_URL",
                "CLAUDE_CODE_USE_BEDROCK",
                "CLAUDE_CODE_USE_VERTEX",
            ],
            // The claude.ai MCP connectors stay unstarted: the warm wants the account's
            // caches, not its tools.
            env: &[("ENABLE_CLAUDEAI_MCP_SERVERS", "false")],
            opt_out: Some("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"),
            state: Some(AccountState {
                dir_var: "CLAUDE_CONFIG_DIR",
                under_home: &[".claude", ".claude.json"],
            }),
        }),
        self_update_verbs: &["update", "upgrade", "install"],
        // `--yolo` itself is an unknown option to claude (measured 2.1.304,
        // 2026-10-19).
        yolo_flags: &["--dangerously-skip-permissions"],
        // The mode words, the prompt handler, restricted mode, a tool
        // allow/deny list, a settings source (it can carry the mode), and a
        // resume — a session's own posture, which the harness restates on its
        // relaunch lines — each in every spelling the CLI takes.
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
        // Global options ride in front of every subcommand measured (mcp, doctor,
        // 2.1.304).
        yolo_after: &[],
        yolo_skip: &[],
        live_upgrade: true,
        revision_versions: false,
        floor: floor(2, 1, 281),
        legacy_ceiling: LEGACY_CEILING,
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
        // The Run manifest and the `manifest.json` beside the payload, each answered in
        // place — else both halves of the agreement could come from one host.
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
        // agy's background updater replaces its own executable unless this is exactly
        // `true` (`1` leaves it on; measured on 1.3.6).
        shim_env: &["AGY_CLI_DISABLE_AUTO_UPDATE=true"],
        // Nothing measured warms it signed out (`agy models` exits 1).
        warm: None,
        self_update_verbs: &["update", "install"],
        // `--yolo` is not a defined flag to agy (measured 1.4.2, 2026-10-19); its
        // sandbox is opt-in (`--sandbox`), off by default, so nothing else is owed.
        yolo_flags: &["--dangerously-skip-permissions"],
        // `--mode plan|accept-edits` and an explicit `--sandbox` are a decided
        // posture the default never overrides.
        yolo_markers: &[
            "--dangerously-skip-permissions",
            "--mode",
            "--mode=",
            "--sandbox",
        ],
        yolo_after: &[],
        yolo_skip: &[],
        // The harness's live upgrade moves a supervised agy onto a build a pass lands, in its
        // quiet flow (aterm-agent `upgrade_agy_live`, 2026-10-18): a landing that moves
        // `agents/agy` wakes the window's host for its agy sessions alone (its own marker).
        live_upgrade: true,
        revision_versions: false,
        floor: floor(1, 3, 6),
        legacy_ceiling: NEVER_INDEX_PINNED,
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
        // The channel and the release manifest, each answered in place.
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
        // The binary has no updater of its own (Meta's launcher script updates it, and is
        // never used here).
        shim_env: &[],
        // No login-free run refreshes anything.
        warm: None,
        // No updater, so no verb to hand over (`muse update` is read as a prompt).
        self_update_verbs: &[],
        // muse's `--help` defines its native `--yolo` (measured 1.4.11, 2026-10-19)
        // as exactly these four switches, each its own flag; spelled out so the echo
        // names the posture, as on every other row.
        yolo_flags: &[
            "--disable-approval",
            "--disable-sandbox",
            "--trust-workspace",
            "--disable-request-user-input",
        ],
        // The approval/sandbox words, its restriction switches, the network
        // mode and judge (the session's permission snapshot, `upgrade_muse`),
        // and a `-c` setting, which can spell any of them for the run.
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
        // Top-level switches AHEAD of a subcommand are `invalid TUI options`
        // (measured 1.4.11): `exec` and `resume` take them behind the word, the
        // rest take none at all.
        yolo_after: &["exec", "resume"],
        yolo_skip: &[
            // The session keeps its permission mode (measured, `upgrade_muse`):
            // the default never lands on a resume, an explicit `--yolo` does.
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
            "init",
        ],
        // The harness's live upgrade moves a supervised Muse Code onto a build a pass lands,
        // in its quiet flow (aterm-agent `upgrade_muse_live`, 2026-10-18): a landing that
        // moves `agents/muse` wakes the window's host.
        live_upgrade: true,
        revision_versions: true,
        floor: revision_floor(1, 4, 9, 6297, Some(1)),
        legacy_ceiling: NEVER_INDEX_PINNED,
    },
    // OPENAI'S OWN CODEX BESIDE `codex` (`codex` is codex-alab
    // where ALab builds it, upstream Codex is `codex-old`, and both are bundled). Its own
    // program — store, `current`, stamp, digest log and shims — so the source rule that
    // hands `codex` to the ALab index never reaches it ([`crate::alab`] has no row for
    // it): it is always the vendor's lane and the vendor's bytes. The build is OpenAI's
    // package as it ships, its entry cloned to `bin/codex-old` ([`VendorSpec::aliases`]),
    // so the gate judges the file the shim runs and the argv0 aterm reads names it.
    VendorSpec {
        program: "codex-old",
        description: "OpenAI Codex CLI, OpenAI's own build beside codex — Apache-2.0, ~110 MB \
                      (~290 MB on disk), downloaded from github.com/openai/codex",
        exposes: &["codex-old"],
        aliases: &[("codex-old", "codex")],
        // One warm per `CODEX_HOME`: it shares `~/.codex` with `codex`, whose row warms it.
        warm: None,
        // No harness supervises a vendor Codex (shown, never driven), so no session waits
        // on its landing.
        live_upgrade: false,
        // OpenAI's head when the row was written (`rust-v0.163.0`, read 2026-10-21); no
        // index ever pinned it.
        floor: floor(0, 163, 0),
        legacy_ceiling: NEVER_INDEX_PINNED,
        ..OPENAI_CODEX
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

/// Whether this platform has a build of `program` (muse is macOS only), by the rule the
/// install and the doctor read; for readers outside atpkg, such as the AI connections.
#[must_use]
pub fn served_here(program: &str) -> bool {
    spec(program).is_some_and(|s| s.serves(crate::cli::current_triple()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The warm never starts a turn: claude's is print mode over a stdin it never writes —
    /// never `--bare` or `--init-only`, which skip or race the fetches it exists for — with
    /// no transcript and no hooks; no warm argv carries a free operand, which print mode
    /// would send as a prompt.
    #[test]
    fn the_warm_argv_refreshes_caches_and_never_prompts() {
        let claude = spec("claude").unwrap().warm.unwrap().argv;
        for present in ["-p", "--no-session-persistence", "--strict-mcp-config"] {
            assert!(claude.contains(&present), "{present}");
        }
        assert!(claude.iter().any(|a| a.contains("disableAllHooks")));
        for absent in ["--bare", "--init-only"] {
            assert!(!claude.contains(&absent), "{absent}");
        }
        // Every operand is a flag or the value of the flag before it (codex's two words
        // are its subcommand, not a prompt).
        let takes_value = ["--input-format", "--output-format", "--settings"];
        for (i, arg) in claude.iter().enumerate() {
            assert!(
                arg.starts_with('-') || (i > 0 && takes_value.contains(&claude[i - 1])),
                "{arg:?} would be a prompt"
            );
        }
    }

    /// THE ALIASES ARE THE ROW'S OWN: each `(name, target)` names one of the row's exposed
    /// tools and a target it does not expose — a clone inside the program's own build, never
    /// a shim into another program's — both admissible tool names; and no tool is exposed by
    /// two rows, so every `bin/` name is one program's. `codex-old` is the one aliased row:
    /// OpenAI's lane pin for pin (one vendor's word, read under one set of rules), under its
    /// own name, with no warm of the `~/.codex` it shares, no live upgrade, a floor of its
    /// own, and no ALab row — so the source rule can never hand it to the index.
    ///
    /// NEGATIVE CONTROL: `codex` keeps its warm, its live upgrade and its ALab row.
    #[test]
    fn every_alias_is_a_clone_inside_its_own_rows_build() {
        let mut seen = std::collections::BTreeSet::new();
        for row in VENDORS {
            for tool in row.exposes {
                assert!(seen.insert(*tool), "{tool} exposed by two rows");
                assert!(crate::store::ToolName::new(tool).is_some(), "{tool}");
            }
            for (name, target) in row.aliases {
                assert!(row.exposes.contains(name), "{}: {name}", row.program);
                assert!(!row.exposes.contains(target), "{}: {target}", row.program);
                assert!(crate::store::ToolName::new(target).is_some(), "{target}");
            }
        }
        let aliased: Vec<&str> = VENDORS
            .iter()
            .filter(|row| !row.aliases.is_empty())
            .map(|row| row.program)
            .collect();
        assert_eq!(aliased, ["codex-old"]);
        let (old, codex) = (spec("codex-old").unwrap(), spec("codex").unwrap());
        assert_eq!(old.exposes, &["codex-old"]);
        assert_eq!(old.aliases, &[("codex-old", "codex")]);
        let lane = |s: &VendorSpec| {
            (
                (s.vendor, s.product, s.head_url, s.anchor, s.apple_team),
                (s.url_prefixes, s.url_query, s.digest_docs, s.shim_env),
                (s.self_update_verbs, s.yolo_flags, s.yolo_markers),
                (s.yolo_after, s.yolo_skip, s.revision_versions),
            )
        };
        assert_eq!(lane(old), lane(codex));
        assert!(old.warm.is_none() && !old.live_upgrade);
        assert_eq!(old.legacy_ceiling, NEVER_INDEX_PINNED);
        assert!(old.floor >= codex.floor);
        assert!(old.description.starts_with("OpenAI Codex CLI"));
        assert!(crate::alab::agent("codex-old").is_none());
        assert!(codex.warm.is_some() && codex.live_upgrade);
        assert!(crate::alab::agent("codex").is_some());
    }

    /// The `anchor` key of a `.vendor` record an earlier client wrote: each spelling is
    /// an on-disk contract, and a record whose anchor no longer parses is refused.
    #[test]
    fn anchors_spell_as_their_record_strings() {
        #[derive(Serialize, Deserialize)]
        struct Row {
            a: Anchor,
        }
        for (a, spelled) in [
            (Anchor::AnthropicOpenPgp, "anthropic-openpgp"),
            (Anchor::OpenAiTwoHost, "openai-two-host"),
            (Anchor::GoogleTwoHost, "google-two-host"),
            (Anchor::MetaDeveloperId, "meta-developer-id"),
        ] {
            let text = aterm_toml::to_string(&Row { a }).unwrap();
            assert_eq!(text.trim(), format!("a = \"{spelled}\""));
            assert_eq!(aterm_toml::from_str::<Row>(&text).unwrap().a, a);
        }
    }

    /// Each digest document keeps its own host pin: the codex release JSON is never read
    /// from GitHub's storage, and only the SUMS may arrive through a redirect.
    #[test]
    fn digest_documents_are_pinned_per_document() {
        let codex = spec("codex").unwrap();
        let json = codex.head_url;
        let sums = "https://github.com/openai/codex/releases/download/rust-v0.156.0/codex-package_SHA256SUMS";
        let storage =
            "https://release-assets.githubusercontent.com/github-production-release-asset/1";
        assert!(codex.digest_doc_admitted(json, json));
        for effective in [
            storage,
            "https://objects.githubusercontent.com/x",
            "https://releases.openai.com/codex/elsewhere",
            "https://github.com/openai/codex/releases/download/x",
        ] {
            assert!(!codex.digest_doc_admitted(json, effective), "{effective}");
        }
        assert!(codex.digest_doc_admitted(sums, storage));
        assert!(codex.digest_doc_admitted(sums, "https://objects.githubusercontent.com/x"));
        assert!(codex.digest_doc_admitted(sums, sums));
        for effective in [
            "https://releases.openai.com/codex/channels/latest",
            "https://evil.example/x",
            "http://release-assets.githubusercontent.com/x",
            "https://release-assets.githubusercontent.com:8443/x",
            "https://user@release-assets.githubusercontent.com/x",
        ] {
            assert!(!codex.digest_doc_admitted(sums, effective), "{effective}");
        }
        // A document outside every pin is never admitted, even answered in place.
        for requested in [
            "https://github.com/other/codex/releases/download/x",
            "https://downloads.claude.ai/claude-code-releases/2.1.280/manifest.json",
            "http://releases.openai.com/codex/channels/latest",
        ] {
            assert!(
                !codex.digest_doc_admitted(requested, requested),
                "{requested}"
            );
        }
        let claude = spec("claude").unwrap();
        let manifest = "https://downloads.claude.ai/claude-code-releases/2.1.280/manifest.json";
        assert!(claude.digest_doc_admitted(manifest, manifest));
        assert!(!claude.digest_doc_admitted(manifest, "https://downloads.claude.ai/other"));
        assert!(!claude.digest_doc_admitted(manifest, storage));
        // agy's two documents and muse's two are each answered in place, or not read —
        // never from the other document's host, which would make one host vouch twice.
        let agy = spec("agy").unwrap();
        let beside = "https://storage.googleapis.com/antigravity-public/antigravity-cli/1.3.6-1/darwin-arm/manifest.json";
        let muse = spec("muse").unwrap();
        let channel = muse.head_url;
        let release = "https://lookaside.facebook.com/lookaside/muse/download/?channel=muse&version=1.4.9-R6297.1&file=manifest.json";
        for (row, doc, other) in [
            (agy, agy.head_url, beside),
            (agy, beside, agy.head_url),
            (muse, channel, release),
            (muse, release, channel),
        ] {
            assert!(row.digest_doc_admitted(doc, doc), "{doc}");
            assert!(!row.digest_doc_admitted(doc, other), "{doc} from {other}");
        }
    }
}
