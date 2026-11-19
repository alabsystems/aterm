// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Immutable direct Claude target selection for an already supervised native restart.
//!
//! A coordinated upgrade execs the physical build, never the twin atpkg lays for it, so every
//! block of that twin is either carried by the plan, frozen into the projection, or refused: the
//! rule of each block is [`projection_rule`], one arm per [`TwinBlock`], and the projection reads
//! the twin as exactly the blocks the producer's own renderers lay. A block the producer starts
//! laying is a new [`TwinBlock`] variant, which has no rule until one is written, and a twin
//! holding text no block accounts for is refused as [`NativeProjectionRefusal::ProducerUnruled`].

use std::path::PathBuf;

/// One block of a twin atpkg lays for an agent program, in the order the producer lays it
/// ([`super::twin_prelude_blocks`], [`crate::launch_tap::sh_prelude_blocks`],
/// [`crate::launch_tap::sh_token_block`] and the body every Unix shim renders through).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TwinBlock {
    /// The interpreter line and why the shim execs.
    Head,
    /// `claude update|upgrade|install`, answered by atpkg.
    SelfUpdate,
    /// `--no-harness` and `--no-alab-harness`, taken off and exported as `ATERM_NO_HARNESS=1`.
    NoHarness,
    /// The `--yolo` block: the launch's permission posture.
    Posture,
    /// The ALab harness hand-off, once a live build of it is installed.
    HarnessHandoff,
    /// aterm's usage tap, which also settles the launch folder's trust.
    UsageTap,
    /// Claude Code's renderer, fullscreen in a tab where a launch names none.
    Renderer,
    /// The managed copy's exported environment.
    Exports,
    /// The Trust build cache's block (`targo`'s and `targo-tippy`'s).
    BuildCache,
    /// A compatibility exec root's guarded route (`[ ! -h …`).
    Route,
    /// An admitted ALab primary's stable managed launch entry.
    Launch,
    /// aterm's token route: `aterm harness accounts token-run --auto` ahead of the build.
    TokenRoute,
    /// The plain `exec '<build>' "$@"`.
    Exec,
}

impl TwinBlock {
    /// Every block, in the order the producer lays them.
    pub const ALL: [Self; 13] = [
        Self::Head,
        Self::SelfUpdate,
        Self::NoHarness,
        Self::Posture,
        Self::HarnessHandoff,
        Self::UsageTap,
        Self::Renderer,
        Self::Exports,
        Self::BuildCache,
        Self::Route,
        Self::Launch,
        Self::TokenRoute,
        Self::Exec,
    ];
}

/// What a native restart does with one block of the twin it projects ([`projection_rule`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectionRule {
    /// The block holds nothing the launch runs: the projection reads past it.
    Inert,
    /// The block's own condition is checked here against the plan's argv or this prefix, and the
    /// projection refuses where the block would do anything but run the build as typed.
    Checked,
    /// The plan carries the block's work itself, before the line exists.
    Plan,
    /// No plan carries it, and a resume needs none: named so the gap stays visible.
    Dropped,
    /// Frozen into the projection ([`NativeClaudeRestart`]'s target or exports).
    Frozen,
    /// Projected as its as-typed arm, only under the caller's own token judgment ([`TokenBlock`]).
    Witnessed,
    /// Never projected: a twin carrying it is refused with this reason.
    Refused(NativeProjectionRefusal),
}

/// THE RULE of each block a twin may carry, one arm per [`TwinBlock`]:
/// - the head is inert;
/// - the self-update block answers only a self-update verb as the first argument, which a resume
///   is not ([`ProjectionRule::Checked`]);
/// - the `--no-harness` block rewrites only an argv that names either word ahead of `--`, which
///   is refused ([`ProjectionRule::Checked`]);
/// - the `--yolo` block runs as typed an argv that names its own posture, which the projection
///   requires of every argv ([`ProjectionRule::Checked`]);
/// - the harness hand-off runs the vendor where its own runtime arm finds no live build of the
///   harness that leads claude, and is refused where it finds one ([`ProjectionRule::Checked`]);
/// - the usage tap is the plan's own tap and folder trust, settled before the line is built
///   ([`ProjectionRule::Plan`]);
/// - the renderer block has no plan equivalent: a resume names a Claude home whose renderer
///   choice its own earlier starts already cached, which is the case the block exists to cover
///   ([`ProjectionRule::Dropped`]);
/// - the exports and the exec line are frozen ([`ProjectionRule::Frozen`]);
/// - the token route is its as-typed arm only where the caller's judgment of this exact launch
///   found its token off ([`ProjectionRule::Witnessed`]);
/// - the build cache's block belongs to no Claude Code twin, and a compatibility route and a
///   managed launch entry exec something other than the build: each is refused.
#[must_use]
pub const fn projection_rule(block: TwinBlock) -> ProjectionRule {
    match block {
        TwinBlock::Head => ProjectionRule::Inert,
        TwinBlock::SelfUpdate
        | TwinBlock::NoHarness
        | TwinBlock::Posture
        | TwinBlock::HarnessHandoff => ProjectionRule::Checked,
        TwinBlock::UsageTap => ProjectionRule::Plan,
        TwinBlock::Renderer => ProjectionRule::Dropped,
        TwinBlock::Exports | TwinBlock::Exec => ProjectionRule::Frozen,
        TwinBlock::TokenRoute => ProjectionRule::Witnessed,
        TwinBlock::BuildCache => ProjectionRule::Refused(NativeProjectionRefusal::ProducerForeign),
        TwinBlock::Route => ProjectionRule::Refused(NativeProjectionRefusal::Route),
        TwinBlock::Launch => ProjectionRule::Refused(NativeProjectionRefusal::Launch),
    }
}

/// The caller's own judgment of this exact launch's vault token, the one witness under which a
/// twin's token block ([`TwinBlock::TokenRoute`]) projects as its as-typed arm. The block runs
/// `aterm harness accounts token-run --auto`, which judges the same launch again at exec and,
/// judged off the token, execs the build as typed: exactly what the projection execs. The caller
/// builds it only from its own judgment having found the token off (the store login's, or as
/// typed) or from a line that never serves the token; one that found it in use has no witness.
/// Account custody and selection stay the caller's admission, which every purpose runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenBlock {
    /// The caller judged this launch off its account's vault token.
    JudgedOff,
}

/// Frozen launch data, not execution authority or an executable probe.
#[derive(Debug)]
pub struct NativeClaudeRestart {
    /// The actual physical vendor image, never its mutable wrapper.
    pub target: PathBuf,
    /// Exports from the exact recognized producer, before caller overrides.
    pub env: crate::shim_env::ShimEnv,
    /// Whether the twin carried the token block, projected as its as-typed arm under the
    /// caller's [`TokenBlock`].
    pub token_block: bool,
    /// The blocks the twin carried, each projected by its [`projection_rule`], in order.
    pub blocks: Vec<TwinBlock>,
}

/// Why a twin does not project: one variant per check, each with its own wait word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeProjectionRefusal {
    /// This platform has no native projection.
    Platform,
    /// The argv is not a resume naming its own posture, or names words a block would rewrite.
    Posture,
    /// The twin could not be read as bounded UTF-8.
    ProducerUnread,
    /// The twin exceeds the shim bound.
    ProducerOversize,
    /// No producer renders this exact twin.
    ProducerUnknown,
    /// The twin is atpkg's for another program or prefix, or carries a block no Claude Code
    /// twin carries.
    ProducerForeign,
    /// The twin holds text no block of the producer accounts for.
    ProducerUnruled,
    /// A compatibility exec root's route.
    Route,
    /// An admitted ALab primary's managed launch entry.
    Launch,
    /// The token block, with no judgment of the caller's that found the token off.
    TokenUnjudged,
    /// A live build of the harness that leads claude is installed (or its shim was not read), so
    /// the hand-off would run the harness, not the build.
    Harness,
    /// The build the twin names cannot be resolved or read.
    TargetUnresolved,
    /// The build is not an executable file named `claude`.
    TargetNotExecutable,
    /// The build is not a native image.
    TargetNotNative,
    /// The build lies outside Claude Code's store and its live checkout.
    OutsideStore,
    /// The prefix's dev-link record could not be read.
    CheckoutUnread,
}

impl NativeProjectionRefusal {
    /// Every refusal.
    pub const ALL: [Self; 16] = [
        Self::Platform,
        Self::Posture,
        Self::ProducerUnread,
        Self::ProducerOversize,
        Self::ProducerUnknown,
        Self::ProducerForeign,
        Self::ProducerUnruled,
        Self::Route,
        Self::Launch,
        Self::TokenUnjudged,
        Self::Harness,
        Self::TargetUnresolved,
        Self::TargetNotExecutable,
        Self::TargetNotNative,
        Self::OutsideStore,
        Self::CheckoutUnread,
    ];

    /// The wait word of a plan this refusal holds: `native-twin-<kind>`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Platform => "native-twin-platform",
            Self::Posture => "native-twin-posture",
            Self::ProducerUnread => "native-twin-producer-unread",
            Self::ProducerOversize => "native-twin-producer-oversize",
            Self::ProducerUnknown => "native-twin-producer-unknown",
            Self::ProducerForeign => "native-twin-producer-foreign",
            Self::ProducerUnruled => "native-twin-producer-unruled",
            Self::Route => "native-twin-route",
            Self::Launch => "native-twin-launch",
            Self::TokenUnjudged => "native-twin-token-unjudged",
            Self::Harness => "native-twin-harness",
            Self::TargetUnresolved => "native-twin-target-unresolved",
            Self::TargetNotExecutable => "native-twin-target-not-executable",
            Self::TargetNotNative => "native-twin-target-not-native",
            Self::OutsideStore => "native-twin-outside-store",
            Self::CheckoutUnread => "native-twin-checkout-unread",
        }
    }

    /// What the refusal means, in full.
    #[must_use]
    pub const fn why(self) -> &'static str {
        match self {
            Self::Platform => "native Claude restart projection is unavailable on this platform",
            Self::Posture => {
                "the relaunch's argv is not a resume naming its own posture, or names a word a \
                 block of the twin would rewrite"
            }
            Self::ProducerUnread => "the Claude Code twin cannot be read as bounded UTF-8",
            Self::ProducerOversize => "the Claude Code twin exceeds its bound",
            Self::ProducerUnknown => "no atpkg producer renders this Claude Code twin",
            Self::ProducerForeign => {
                "the Claude Code twin names another program or prefix, or carries a block no \
                 Claude Code twin carries"
            }
            Self::ProducerUnruled => {
                "the Claude Code twin holds text no block with a projection rule accounts for"
            }
            Self::Route => "the Claude Code twin routes through a compatibility exec root",
            Self::Launch => "the Claude Code twin runs through a managed launch entry",
            Self::TokenUnjudged => {
                "the Claude Code twin carries the token route and the plan judged no token off"
            }
            Self::Harness => {
                "a live build of the harness that leads claude is installed, so the twin would \
                 hand the session to the harness"
            }
            Self::TargetUnresolved => "the build the Claude Code twin names cannot be resolved",
            Self::TargetNotExecutable => {
                "the build the Claude Code twin names is not an executable named claude"
            }
            Self::TargetNotNative => "the build the Claude Code twin names is not a native image",
            Self::OutsideStore => {
                "the build the Claude Code twin names lies outside its store and live checkout"
            }
            Self::CheckoutUnread => "the prefix's Claude Code checkout binding cannot be read",
        }
    }
}

impl std::fmt::Display for NativeProjectionRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.word(), self.why())
    }
}

/// Resolve the configured Claude twin for a typed resume whose posture is already explicit, as
/// the build it execs, the exports it freezes and the blocks it carried ([`projection_rule`]).
/// `token` is the caller's own judgment of this launch's vault token ([`TokenBlock`]), without
/// which a twin carrying the token block does not project. The caller still owns authentication,
/// permissions, the token judgment, account admission and execution custody.
///
/// # Errors
/// The [`NativeProjectionRefusal`] of the first check that refuses: the platform, the posture, the
/// producer (unread, oversize, unknown, foreign, unruled), a route or launch entry, an unjudged
/// token block, a live harness, or a build that is unresolved, not executable, not native or
/// outside Claude Code's store and live checkout.
pub fn native_claude_restart(
    #[cfg(unix)] layout: &crate::store::Layout,
    #[cfg(not(unix))] _: &crate::store::Layout,
    #[cfg(unix)] final_argv: &[String],
    #[cfg(not(unix))] _: &[String],
    #[cfg(unix)] token: Option<TokenBlock>,
    #[cfg(not(unix))] _: Option<TokenBlock>,
) -> Result<NativeClaudeRestart, NativeProjectionRefusal> {
    #[cfg(not(unix))]
    {
        Err(NativeProjectionRefusal::Platform)
    }
    #[cfg(unix)]
    {
        use NativeProjectionRefusal as No;
        use std::io::Read as _;
        use std::os::unix::fs::PermissionsExt as _;
        if !final_argv
            .iter()
            .take_while(|arg| arg.as_str() != "--")
            .any(|arg| arg == "--resume" || arg.starts_with("--resume="))
            || !matches!(
                crate::yolo::decide("claude", final_argv),
                crate::yolo::Launch::AsTyped
            )
        {
            return Err(No::Posture);
        }
        let tool = crate::store::ToolName::new("claude").ok_or(No::ProducerUnread)?;
        let wrapper = layout.agent_shim(&tool);
        let (file, metadata) =
            crate::metadata_io::open_regular_with_meta(&wrapper).map_err(|_| No::ProducerUnread)?;
        if metadata.len() > super::MAX_SHIM_BYTES as u64 {
            return Err(No::ProducerOversize);
        }
        let mut content = String::new();
        file.take((super::MAX_SHIM_BYTES + 1) as u64)
            .read_to_string(&mut content)
            .map_err(|_| No::ProducerUnread)?;
        let producer =
            super::shim_producer::recognize(&wrapper, &content).ok_or(No::ProducerUnknown)?;
        if producer.kind == super::shim_producer::Kind::Factory
            && (producer.program != "claude" || producer.prefix != layout.prefix)
        {
            return Err(No::ProducerForeign);
        }
        let blocks = blocks_of(&producer, &content).ok_or(No::ProducerUnruled)?;
        for block in &blocks {
            match projection_rule(*block) {
                ProjectionRule::Inert
                | ProjectionRule::Plan
                | ProjectionRule::Dropped
                | ProjectionRule::Frozen => {}
                ProjectionRule::Checked => checked(*block, layout, final_argv)?,
                ProjectionRule::Witnessed => {
                    let Some(TokenBlock::JudgedOff) = token else {
                        return Err(No::TokenUnjudged);
                    };
                }
                ProjectionRule::Refused(why) => return Err(why),
            }
        }
        let target = std::fs::canonicalize(&producer.target).map_err(|_| No::TargetUnresolved)?;
        let metadata = std::fs::metadata(&target).map_err(|_| No::TargetUnresolved)?;
        if !metadata.is_file()
            || metadata.permissions().mode() & 0o111 == 0
            || target.file_name() != Some(std::ffi::OsStr::new("claude"))
        {
            return Err(No::TargetNotExecutable);
        }
        let (head, _) =
            crate::metadata_io::read_head_with_meta(&target, 8).map_err(|_| No::TargetNotNative)?;
        if !crate::macho::is_native_executable(&head) {
            return Err(No::TargetNotNative);
        }
        let prefix = std::fs::canonicalize(&layout.prefix).map_err(|_| No::OutsideStore)?;
        let store = prefix.join("store/claude");
        let in_store = crate::shim_env::store_build_of(&target)
            .is_some_and(|build| build.parent() == Some(store.as_path()));
        let linked = crate::linkmode::live_linked_checkout(layout, "claude")
            .map_err(|_| No::CheckoutUnread)?;
        let in_checkout = match linked {
            Some(checkout) => target.starts_with(checkout),
            None => false,
        };
        if !in_store && !in_checkout {
            return Err(No::OutsideStore);
        }
        Ok(NativeClaudeRestart {
            target,
            env: producer.env,
            token_block: blocks.contains(&TwinBlock::TokenRoute),
            blocks,
        })
    }
}

/// The condition of a [`ProjectionRule::Checked`] block, against the final argv and this prefix.
#[cfg(unix)]
fn checked(
    block: TwinBlock,
    layout: &crate::store::Layout,
    argv: &[String],
) -> Result<(), NativeProjectionRefusal> {
    let holds = match block {
        TwinBlock::SelfUpdate => argv
            .first()
            .is_none_or(|first| !crate::selfupdate::verbs_of("claude").contains(&first.as_str())),
        TwinBlock::NoHarness => crate::yolo::strip_no_harness("claude", argv).is_none(),
        TwinBlock::Posture => matches!(
            crate::yolo::decide("claude", argv),
            crate::yolo::Launch::AsTyped
        ),
        TwinBlock::HarnessHandoff => {
            return handoff_runs_vendor(layout, crate::harness::twin_harness(layout, "claude"));
        }
        _ => true,
    };
    if holds {
        Ok(())
    } else {
        Err(NativeProjectionRefusal::Posture)
    }
}

/// The harness hand-off's condition for `leader`, the registry row that leads claude as
/// `(harness, tool)`: the block runs the build as typed exactly where its own runtime arm finds
/// no live harness ([`super::probe_shim::harness_handoff`]), so a `bin/<tool>` that is a
/// tombstone, names a removed or unfinished build, or is not executable projects, and a live
/// harness, or one whose shim was not read, refuses.
#[cfg(unix)]
fn handoff_runs_vendor(
    layout: &crate::store::Layout,
    leader: Option<(String, String)>,
) -> Result<(), NativeProjectionRefusal> {
    let Some((harness, tool)) = leader else {
        return Ok(());
    };
    match super::probe_shim::harness_handoff(layout, &harness, &tool) {
        Some(super::probe_shim::Handoff::Vendor) => Ok(()),
        Some(super::probe_shim::Handoff::Link(_) | super::probe_shim::Handoff::Script { .. })
        | None => Err(NativeProjectionRefusal::Harness),
    }
}

/// The blocks `content`, recognized as `producer`'s, is made of, in order, each rendered by the
/// producer's own block renderers; `None` where any text is left that no block accounts for.
#[cfg(unix)]
fn blocks_of(producer: &super::shim_producer::Producer, content: &str) -> Option<Vec<TwinBlock>> {
    use super::shim_producer::Kind;
    let none = crate::shim_env::ShimEnv::NONE;
    // The frame every Unix shim renders through: its head, its exports, its build cache block
    // and its exec line, with no prelude, route or launch (each refused below where present).
    let frame = super::sh_shim_body(&producer.target, &producer.env, None, None, "");
    let bare = super::sh_shim_body(&producer.target, &none, None, None, "");
    let exec = format!("exec {} \"$@\"\n", super::sh_shim_quote(&producer.target));
    let after_head = |body: &str| body.strip_prefix(super::SH_SHIM_HEAD).map(str::to_owned);
    let (frame_tail, bare_tail) = (after_head(&frame)?, after_head(&bare)?);
    let exports = frame_tail.strip_suffix(bare_tail.as_str())?.to_string();
    let cache = bare_tail.strip_suffix(exec.as_str())?.to_string();
    // A route or a launch entry execs something other than the build: its rule refuses it before
    // any other block is read.
    if producer.route.is_some() {
        return Some(vec![TwinBlock::Route]);
    }
    if producer.launch.is_some() {
        return Some(vec![TwinBlock::Launch]);
    }
    let mut blocks = Vec::new();
    let mut rest = content;
    let mut take = |block: TwinBlock, text: &str, rest: &mut &str| -> Option<()> {
        if !text.is_empty() {
            *rest = rest.strip_prefix(text)?;
            blocks.push(block);
        }
        Some(())
    };
    if producer.kind == Kind::Direct {
        // The exact frame with no prelude, or the legacy two-line exec the recognizer admits.
        if content == frame {
            take(TwinBlock::Head, super::SH_SHIM_HEAD, &mut rest)?;
        } else {
            take(TwinBlock::Head, "#!/bin/sh\n", &mut rest)?;
        }
    } else {
        take(TwinBlock::Head, super::SH_SHIM_HEAD, &mut rest)?;
        let launcher = crate::native_defaults::Launcher::recorded(content);
        let verbs = crate::selfupdate::verbs_of(&producer.program);
        for (block, text) in super::twin_prelude_blocks(
            &producer.program,
            &producer.prefix,
            producer.atpkg.as_deref(),
            verbs,
            &crate::yolo::policy_of(&producer.program),
            launcher.as_ref(),
        )
        .into_iter()
        .chain(crate::launch_tap::sh_prelude_blocks(
            &producer.program,
            producer.atpkg.as_deref(),
        )) {
            take(block, &text, &mut rest)?;
        }
    }
    take(TwinBlock::Exports, &exports, &mut rest)?;
    take(TwinBlock::BuildCache, &cache, &mut rest)?;
    if producer.token_route {
        let token = crate::launch_tap::sh_token_block(
            &producer.program,
            &producer.target,
            producer.atpkg.as_deref(),
        );
        if token.is_empty() {
            return None;
        }
        take(TwinBlock::TokenRoute, &token, &mut rest)?;
    }
    take(TwinBlock::Exec, &exec, &mut rest)?;
    rest.is_empty().then_some(blocks)
}

#[cfg(all(test, unix))]
#[path = "native_claude_restart_tests.rs"]
mod tests;
