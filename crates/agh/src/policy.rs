// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Routing (design §6.4–§6.5): tiers, access classes, requests, decisions and the Router.

use aterm_json::{Map, Value};

use crate::config::{Validated, on_floor};
use crate::error::{Error, Kind, Result, Sub, Tried};
use crate::identity::Profile;
use crate::slug::{RepoArg, Slug};

/// An owner's or a profile's tier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Tier {
    /// The development owners.
    Dev,
    /// The release owners: the public channel.
    Release,
    /// Everyone else.
    External,
}

impl Tier {
    /// `dev`, `release`, `external`.
    #[inline]
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Tier::Dev => "dev",
            Tier::Release => "release",
            Tier::External => "external",
        }
    }

    /// The tier `slug` names.
    #[inline]
    #[must_use]
    pub fn parse(text: &str) -> Option<Tier> {
        match text {
            "dev" => Some(Tier::Dev),
            "release" => Some(Tier::Release),
            "external" => Some(Tier::External),
            _ => None,
        }
    }
}

/// What a request may see and do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Access {
    /// A read.
    #[default]
    Read,
    /// A read as exactly the writer: drafts and compare-and-swap reads.
    WriterRead,
    /// A write.
    Write,
}

impl Access {
    /// `read`, `writer-read`, `write`.
    #[inline]
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Access::Read => "read",
            Access::WriterRead => "writer-read",
            Access::Write => "write",
        }
    }

    /// The access `slug` names.
    #[inline]
    #[must_use]
    pub fn parse(text: &str) -> Option<Access> {
        match text {
            "read" => Some(Access::Read),
            "writer-read" => Some(Access::WriterRead),
            "write" => Some(Access::Write),
            _ => None,
        }
    }
}

/// How the destination was named. The library default is `Default`, which no release-tier
/// write accepts. A `Literal` carries the `ExplicitTarget` it was made from, and a library
/// caller can carry one but never make one (design §10.2).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Named {
    /// `OWNER/NAME` typed on this command line.
    Literal(ExplicitTarget),
    /// `@name` from `[targets]`.
    Target(String),
    /// Neither: the library's default.
    #[default]
    Default,
}

/// A repository typed as `OWNER/NAME` on this command line. It has no public constructor, and
/// no public function returns one. In M1 only the CLI makes one, from argv text, in
/// `Invocation::repo` and `content` (crate-private) and in `whoami --for`, and it never leaves
/// the verb that made it. M2 adds `Router::grant`, for the `[[writers]]` callers, and a
/// private provenance; private fields keep that additive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExplicitTarget {
    slug: Slug,
}

impl ExplicitTarget {
    /// The text as `RepoArg::parse` reads it, which must be a literal `OWNER/NAME`.
    ///
    /// # Errors
    ///
    /// Usage: `RepoArg::parse`'s errors; an `@target` → `a literal destination is OWNER/NAME,
    /// not @{name}`.
    pub(crate) fn from_argv(arg: &std::ffi::OsStr) -> Result<ExplicitTarget> {
        match RepoArg::parse(&arg.to_string_lossy())? {
            RepoArg::Literal(slug) => Ok(ExplicitTarget { slug }),
            RepoArg::Target(name) => Err(Error::usage(format!(
                "a literal destination is OWNER/NAME, not @{name}"
            ))),
        }
    }

    /// The repository it names.
    #[inline]
    #[must_use]
    pub const fn slug(&self) -> &Slug {
        &self.slug
    }
}

/// What a request is about. `#[non_exhaustive]`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RouteTarget {
    /// A repository.
    Repo(Slug),
    /// An owner.
    Owner(String),
    /// No owner: a path outside any repository.
    Unscoped,
    /// The identity itself (`/user`).
    Identity,
}

/// One routing question. `#[non_exhaustive]`: callers outside agh start from `read`,
/// `writer_read` or `write` and set fields.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct RouteRequest {
    /// What it is about.
    pub target: RouteTarget,
    /// What it may see and do.
    pub access: Access,
    /// How the destination was named.
    pub named: Named,
    /// `--anonymous`.
    pub anonymous: bool,
    /// `--as`.
    pub as_profile: Option<String>,
}

impl RouteRequest {
    /// A read of `target`.
    #[inline]
    #[must_use]
    pub const fn read(target: RouteTarget) -> RouteRequest {
        RouteRequest {
            target,
            access: Access::Read,
            named: Named::Default,
            anonymous: false,
            as_profile: None,
        }
    }

    /// A WriterRead of `target`.
    #[inline]
    #[must_use]
    pub fn writer_read(target: RouteTarget) -> RouteRequest {
        RouteRequest {
            access: Access::WriterRead,
            ..RouteRequest::read(target)
        }
    }

    /// A write of `target`. M1 decides it (`whoami --for … --op write`); no M1 path sends one.
    #[inline]
    #[must_use]
    pub fn write(target: RouteTarget) -> RouteRequest {
        RouteRequest {
            access: Access::Write,
            ..RouteRequest::read(target)
        }
    }

    /// The same request with `anonymous`.
    #[inline]
    #[must_use]
    pub fn with_anonymous(mut self, anonymous: bool) -> RouteRequest {
        self.anonymous = anonymous;
        self
    }

    /// The same request with `as_profile`.
    #[inline]
    #[must_use]
    pub fn with_as(mut self, profile: Option<String>) -> RouteRequest {
        self.as_profile = profile;
        self
    }

    /// The same request with `named`.
    #[inline]
    #[must_use]
    pub fn with_named(mut self, named: Named) -> RouteRequest {
        self.named = named;
        self
    }

    /// The repository's owner, or the owner target's name.
    #[inline]
    #[must_use]
    pub fn owner(&self) -> Option<&str> {
        match &self.target {
            RouteTarget::Repo(slug) => Some(slug.owner()),
            RouteTarget::Owner(owner) => Some(owner),
            RouteTarget::Unscoped | RouteTarget::Identity => None,
        }
    }
}

/// One step of a read chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChainStep {
    /// A profile, by name.
    Profile(String),
    /// No credential.
    Anonymous,
}

impl ChainStep {
    /// The profile's name, or `anonymous`.
    #[inline]
    #[must_use]
    pub fn label(&self) -> &str {
        match self {
            ChainStep::Profile(name) => name,
            ChainStep::Anonymous => "anonymous",
        }
    }
}

/// Whether a profile can be used now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presence {
    /// Its credential loads.
    Present,
    /// It has no credential.
    Absent,
    /// Its credential is there and unusable: the `LoadError` slug.
    Unusable(&'static str),
}

impl Presence {
    /// `present`, `absent`, or the unusable slug.
    #[inline]
    #[must_use]
    pub const fn slug(&self) -> &'static str {
        match self {
            Presence::Present => "present",
            Presence::Absent => "absent",
            Presence::Unusable(slug) => slug,
        }
    }
}

/// Why a read went anonymous.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnonymousWhy {
    /// `--anonymous`.
    Asked,
    /// The chain's anonymous step.
    ChainFellThrough,
    /// There is no agh.toml.
    NoConfig,
}

impl AnonymousWhy {
    /// `asked`, `chain_fell_through`, `no_config`.
    #[inline]
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            AnonymousWhy::Asked => "asked",
            AnonymousWhy::ChainFellThrough => "chain_fell_through",
            AnonymousWhy::NoConfig => "no_config",
        }
    }
}

/// Why the Router refused, or (`MethodNotRead`) why the client's pipeline did before routing.
/// `#[non_exhaustive]`: M2 adds `NotGranted` (`policy.not_granted`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RefusalCode {
    /// The owner's writer has no credential.
    WriterAbsent,
    /// The owner's writer's credential is unusable.
    WriterUnusable,
    /// No step of the read chain is present, and it has no anonymous step.
    ChainExhausted,
    /// A step of the read chain is unusable, so the chain stops there.
    ChainStoppedUnusable,
    /// The owner has no writer.
    WriteNone,
    /// A release-tier write needs the repository typed as `OWNER/NAME`.
    ReleaseNeedsLiteral,
    /// A `@target` names a release-tier owner for a write.
    ReleaseTargetForWrite,
    /// `--as` names a profile outside the read chain.
    AsNotInChain,
    /// `--as` names a profile other than the writer.
    AsNotWriter,
    /// agh.toml breaks its rules.
    ConfigInvalid,
    /// A token would speak for another tier.
    TierMismatch,
    /// A method other than GET and HEAD.
    MethodNotRead,
    /// `--anonymous` on a WriterRead or a write.
    AnonymousWriterRead,
    /// No profile has that name.
    ProfileUnknown,
    /// An identity path needs `--as`.
    IdentityNeedsAs,
    /// An unscoped or identity path is read only.
    UnscopedNotRead,
}

impl RefusalCode {
    /// snake_case: `writer_absent`, `writer_unusable`, `chain_exhausted`, …, `tier_mismatch`,
    /// `method_not_read`, …
    #[inline]
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            RefusalCode::WriterAbsent => "writer_absent",
            RefusalCode::WriterUnusable => "writer_unusable",
            RefusalCode::ChainExhausted => "chain_exhausted",
            RefusalCode::ChainStoppedUnusable => "chain_stopped_unusable",
            RefusalCode::WriteNone => "write_none",
            RefusalCode::ReleaseNeedsLiteral => "release_needs_literal",
            RefusalCode::ReleaseTargetForWrite => "release_target_for_write",
            RefusalCode::AsNotInChain => "as_not_in_chain",
            RefusalCode::AsNotWriter => "as_not_writer",
            RefusalCode::ConfigInvalid => "config_invalid",
            RefusalCode::TierMismatch => "tier_mismatch",
            RefusalCode::MethodNotRead => "method_not_read",
            RefusalCode::AnonymousWriterRead => "anonymous_writer_read",
            RefusalCode::ProfileUnknown => "profile_unknown",
            RefusalCode::IdentityNeedsAs => "identity_needs_as",
            RefusalCode::UnscopedNotRead => "unscoped_not_read",
        }
    }

    /// The first four → Unauthenticated; the next eight → Refused; the last four → Usage.
    #[inline]
    #[must_use]
    pub const fn kind(self) -> Kind {
        match self {
            RefusalCode::WriterAbsent
            | RefusalCode::WriterUnusable
            | RefusalCode::ChainExhausted
            | RefusalCode::ChainStoppedUnusable => Kind::Unauthenticated,
            RefusalCode::WriteNone
            | RefusalCode::ReleaseNeedsLiteral
            | RefusalCode::ReleaseTargetForWrite
            | RefusalCode::AsNotInChain
            | RefusalCode::AsNotWriter
            | RefusalCode::ConfigInvalid
            | RefusalCode::TierMismatch
            | RefusalCode::MethodNotRead => Kind::Refused,
            RefusalCode::AnonymousWriterRead
            | RefusalCode::ProfileUnknown
            | RefusalCode::IdentityNeedsAs
            | RefusalCode::UnscopedNotRead => Kind::Usage,
        }
    }
}

/// A refusal, with the chain steps it judged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    /// Why, as a code.
    pub code: RefusalCode,
    /// Why, in one sentence.
    pub why: String,
    /// What to do next.
    pub next: String,
    /// Each step judged, with its presence.
    pub tried: Vec<(ChainStep, Presence)>,
}

impl Refusal {
    /// `Error::new(code.kind(), why)`, `Sub::Policy(code)`, `next`, `repo`, and each judged step
    /// as `Tried::Skipped { as_: step.label(), why: presence.slug() }`.
    #[must_use]
    pub fn into_error(self, repo: Option<&Slug>) -> Error {
        let tried = self
            .tried
            .iter()
            .map(|(step, presence)| Tried::Skipped {
                as_: step.label().to_string(),
                why: presence.slug().to_string(),
            })
            .collect();
        let error = Error::new(self.code.kind(), self.why)
            .with_sub(Sub::Policy(self.code))
            .with_next(self.next)
            .with_tried(tried);
        match repo {
            Some(slug) => error.with_repo(slug.to_string()),
            None => error,
        }
    }
}

/// The Router's answer. `#[non_exhaustive]`, so a match outside agh carries a `_` arm.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Decision {
    /// Send as this profile.
    Send {
        /// The profile.
        profile: String,
        /// Its tier.
        tier: Tier,
    },
    /// Send with no credential.
    Anonymous {
        /// Why.
        why: AnonymousWhy,
    },
    /// Send nothing.
    Refuse(Refusal),
}

/// `agh whoami --for`'s answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Explanation {
    /// The owner, when the request has one.
    pub owner: Option<String>,
    /// The owner's tier (External when there is no owner).
    pub tier: Tier,
    /// The access asked.
    pub access: Access,
    /// The read chain, each step with its presence.
    pub chain: Vec<(ChainStep, Presence)>,
    /// The owner's writer and its presence.
    pub writer: Option<(String, Presence)>,
    /// The decision.
    pub decision: Decision,
}

/// A presence into `item`, the one form a chain step and the writer share: `presence` as one
/// word (`present`, `absent` or `unusable`), and for an unusable profile `why`, its slug.
fn insert_presence(item: &mut Map, presence: Presence) {
    let word = match presence {
        Presence::Present => "present",
        Presence::Absent => "absent",
        Presence::Unusable(_) => "unusable",
    };
    item.insert("presence".into(), Value::from(word));
    if let Presence::Unusable(slug) = presence {
        item.insert("why".into(), Value::from(slug));
    }
}

impl Explanation {
    /// `{"chain":[{"presence","step"[,"why"]}],"decision":{"send":{"profile","tier"}} |
    /// {"anonymous":{"why"}} | {"refuse":{"code","next","why"}},"op","owner","tier",
    /// "writer":{"presence","profile"[,"why"]}|null}`. An anonymous step has no `presence`; an
    /// unusable step or writer adds `"why": <slug>`.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let chain = self
            .chain
            .iter()
            .map(|(step, presence)| {
                let mut item = Map::new();
                item.insert("step".into(), Value::from(step.label()));
                if let ChainStep::Profile(_) = step {
                    insert_presence(&mut item, *presence);
                }
                Value::Object(item)
            })
            .collect::<Vec<_>>();
        let mut inner = Map::new();
        let key = match &self.decision {
            Decision::Send { profile, tier } => {
                inner.insert("profile".into(), Value::from(profile.as_str()));
                inner.insert("tier".into(), Value::from(tier.slug()));
                "send"
            }
            Decision::Anonymous { why } => {
                inner.insert("why".into(), Value::from(why.slug()));
                "anonymous"
            }
            Decision::Refuse(refusal) => {
                inner.insert("code".into(), Value::from(refusal.code.slug()));
                inner.insert("next".into(), Value::from(refusal.next.as_str()));
                inner.insert("why".into(), Value::from(refusal.why.as_str()));
                "refuse"
            }
        };
        let mut decision = Map::new();
        decision.insert(key.into(), Value::Object(inner));
        let writer = self
            .writer
            .as_ref()
            .map_or(Value::Null, |(profile, presence)| {
                let mut item = Map::new();
                insert_presence(&mut item, *presence);
                item.insert("profile".into(), Value::from(profile.as_str()));
                Value::Object(item)
            });
        let mut object = Map::new();
        object.insert("chain".into(), Value::Array(chain));
        object.insert("decision".into(), Value::Object(decision));
        object.insert("op".into(), Value::from(self.access.slug()));
        object.insert(
            "owner".into(),
            self.owner.as_deref().map_or(Value::Null, Value::from),
        );
        object.insert("tier".into(), Value::from(self.tier.slug()));
        object.insert("writer".into(), writer);
        Value::Object(object)
    }
}

/// The routing rules over a validated agh.toml (or none).
#[derive(Clone, Debug, Default)]
pub struct Router {
    validated: Option<Validated>,
}

impl Router {
    /// No config: every read anonymous (`NoConfig`), every write refused.
    #[inline]
    #[must_use]
    pub const fn empty() -> Router {
        Router { validated: None }
    }

    /// The rules of `validated`.
    #[inline]
    #[must_use]
    pub const fn from_validated(validated: Validated) -> Router {
        Router {
            validated: Some(validated),
        }
    }

    /// Whether an agh.toml stands behind it.
    #[inline]
    #[must_use]
    pub const fn has_config(&self) -> bool {
        self.validated.is_some()
    }

    /// Release for a floor owner (`RELEASE_FLOOR_OWNERS`, ASCII case-insensitively), whatever
    /// its row says; else the owner's row tier; else External.
    #[must_use]
    pub fn tier_of(&self, owner: &str) -> Tier {
        if on_floor(owner) {
            return Tier::Release;
        }
        self.validated
            .as_ref()
            .and_then(|v| v.owners.get(&owner.to_ascii_lowercase()))
            .map_or(Tier::External, |row| row.tier)
    }

    /// The owner's chain (`None`: the default chain); a floor owner with no row and every
    /// other owner use the default chain; no config → `[Anonymous]`.
    #[must_use]
    pub fn read_chain(&self, owner: Option<&str>) -> Vec<ChainStep> {
        let Some(validated) = &self.validated else {
            return vec![ChainStep::Anonymous];
        };
        owner
            .and_then(|o| validated.owners.get(&o.to_ascii_lowercase()))
            .map_or_else(|| validated.default_read.clone(), |row| row.read.clone())
    }

    /// The owner's writer, when its row names one.
    #[must_use]
    pub fn writer_of(&self, owner: &str) -> Option<&str> {
        self.validated
            .as_ref()?
            .owners
            .get(&owner.to_ascii_lowercase())?
            .write
            .as_deref()
    }

    /// The profile so named.
    #[inline]
    #[must_use]
    pub fn profile(&self, name: &str) -> Option<&Profile> {
        self.validated.as_ref()?.profiles.get(name)
    }

    /// Every profile, sorted by name.
    #[must_use]
    pub fn profiles(&self) -> Vec<&Profile> {
        self.validated
            .as_ref()
            .map(|v| v.profiles.values().collect())
            .unwrap_or_default()
    }

    /// The `[targets]` row so named.
    #[inline]
    #[must_use]
    pub fn target(&self, name: &str) -> Option<&Slug> {
        self.validated.as_ref()?.targets.get(name)
    }

    /// THE decision (design §6.4; the plan's §4.6.13 algorithm), with its two decide-time tier
    /// walls: whatever `Validated` the router holds, a release-tier profile serves only a
    /// release-tier owner, and an owner is written only by a profile of its own tier.
    /// `presence` judges a profile by name; each step it judges is in a refusal's `tried`.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AghRouteWall",
            action = "Decide",
            project = "agh::policy::project_decision"
        )
    )]
    #[must_use]
    pub fn decide(&self, req: &RouteRequest, presence: &dyn Fn(&str) -> Presence) -> Decision {
        match req.access {
            Access::Read => self.decide_read(req, presence),
            Access::WriterRead | Access::Write => self.decide_writer(req, presence),
        }
    }

    /// The owner as the refusal texts name it: the owner, or `the default` for a path with
    /// none.
    fn owner_label(req: &RouteRequest) -> String {
        req.owner()
            .map_or_else(|| "the default".to_string(), str::to_string)
    }

    /// What `agh whoami --for` and `-R` name: the repository, else `{owner}/NAME` (both take
    /// only OWNER/NAME), else `REPO`.
    fn repo_label(req: &RouteRequest) -> String {
        match &req.target {
            RouteTarget::Repo(slug) => slug.to_string(),
            RouteTarget::Owner(owner) => format!("{owner}/NAME"),
            RouteTarget::Unscoped | RouteTarget::Identity => "REPO".to_string(),
        }
    }

    /// The tier a request's owner is held at: External for a path with none.
    fn target_tier(&self, req: &RouteRequest) -> Tier {
        req.owner()
            .map_or(Tier::External, |owner| self.tier_of(owner))
    }

    /// A Read: `--anonymous`, an identity path, or the owner's chain walked in order.
    fn decide_read(&self, req: &RouteRequest, presence: &dyn Fn(&str) -> Presence) -> Decision {
        if req.anonymous {
            return Decision::Anonymous {
                why: AnonymousWhy::Asked,
            };
        }
        let owner = Self::owner_label(req);
        let chain = if req.target == RouteTarget::Identity {
            let Some(name) = &req.as_profile else {
                return refuse(
                    RefusalCode::IdentityNeedsAs,
                    "this path speaks as one identity: name it with --as".to_string(),
                    "--as PROFILE".to_string(),
                    Vec::new(),
                );
            };
            if self.profile(name).is_none() {
                return unknown_profile(name);
            }
            vec![ChainStep::Profile(name.clone())]
        } else {
            let chain = self.read_chain(req.owner());
            let chain = match &req.as_profile {
                None => chain,
                Some(name) if self.profile(name).is_none() => return unknown_profile(name),
                Some(name) => {
                    let step = ChainStep::Profile(name.clone());
                    if !chain.contains(&step) {
                        return refuse(
                            RefusalCode::AsNotInChain,
                            format!("--as {name} is not in {owner}'s read chain"),
                            format!("agh whoami --for {}", Self::repo_label(req)),
                            Vec::new(),
                        );
                    }
                    vec![step]
                }
            };
            // The tier wall (L6 at decide time): a release-tier profile in a chain that serves
            // no release-tier owner is refused before any presence is judged, never skipped.
            let tier = self.target_tier(req);
            let home = req.target != RouteTarget::Unscoped && tier == Tier::Release;
            for step in &chain {
                let ChainStep::Profile(name) = step else {
                    continue;
                };
                match self.profile(name) {
                    None => return unknown_profile(name),
                    Some(profile) if profile.tier == Tier::Release && !home => {
                        return tier_mismatch(name, profile.tier, &owner, tier);
                    }
                    Some(_) => {}
                }
            }
            chain
        };
        let mut tried = Vec::new();
        for step in chain {
            let ChainStep::Profile(name) = &step else {
                let why = if self.has_config() {
                    AnonymousWhy::ChainFellThrough
                } else {
                    AnonymousWhy::NoConfig
                };
                return Decision::Anonymous { why };
            };
            let judged = presence(name);
            tried.push((step.clone(), judged));
            match judged {
                Presence::Present => {
                    let Some(profile) = self.profile(name) else {
                        return unknown_profile(name);
                    };
                    return Decision::Send {
                        profile: name.clone(),
                        tier: profile.tier,
                    };
                }
                Presence::Absent => {}
                Presence::Unusable(slug) => {
                    return refuse(
                        RefusalCode::ChainStoppedUnusable,
                        format!(
                            "{name} in {owner}'s read chain is unusable: {slug}; the chain stops there"
                        ),
                        "agh auth status".to_string(),
                        tried,
                    );
                }
            }
        }
        refuse(
            RefusalCode::ChainExhausted,
            format!(
                "no profile in {owner}'s read chain is present, and the chain has no anonymous step"
            ),
            "agh auth status".to_string(),
            tried,
        )
    }

    /// A WriterRead or a Write: exactly the owner's writer, of the owner's own tier, and for a
    /// release-tier Write a destination typed literally.
    fn decide_writer(&self, req: &RouteRequest, presence: &dyn Fn(&str) -> Presence) -> Decision {
        if req.anonymous {
            return refuse(
                RefusalCode::AnonymousWriterRead,
                "--anonymous cannot read drafts or write".to_string(),
                "drop --anonymous".to_string(),
                Vec::new(),
            );
        }
        let Some(owner) = req.owner() else {
            return refuse(
                RefusalCode::UnscopedNotRead,
                "an unscoped or identity path is read only".to_string(),
                "use a repository path".to_string(),
                Vec::new(),
            );
        };
        let Some(writer) = self.writer_of(owner) else {
            let has_row = self
                .validated
                .as_ref()
                .is_some_and(|v| v.owners.contains_key(&owner.to_ascii_lowercase()));
            let why = if has_row {
                format!("{owner} has no write profile")
            } else {
                format!("{owner} has no row, so it is never written to")
            };
            return refuse(
                RefusalCode::WriteNone,
                why,
                "add write = \"PROFILE\" to the owner's row".to_string(),
                Vec::new(),
            );
        };
        if let Some(name) = &req.as_profile
            && name != writer
        {
            return refuse(
                RefusalCode::AsNotWriter,
                format!("--as {name} is not {owner}'s writer ({writer})"),
                format!("drop --as, or name {writer}"),
                Vec::new(),
            );
        }
        let tier = self.tier_of(owner);
        if req.access == Access::Write && tier == Tier::Release {
            let literal = match (&req.named, &req.target) {
                (Named::Literal(typed), RouteTarget::Repo(slug)) => typed.slug() == slug,
                _ => false,
            };
            if let Named::Target(name) = &req.named {
                let slug = self
                    .target(name)
                    .map(Slug::to_string)
                    .unwrap_or_else(|| Self::repo_label(req));
                return refuse(
                    RefusalCode::ReleaseTargetForWrite,
                    format!(
                        "@{name} names release-tier {owner}, and a write there needs the repository typed as owner/name"
                    ),
                    format!("-R {slug}"),
                    Vec::new(),
                );
            }
            if !literal {
                return refuse(
                    RefusalCode::ReleaseNeedsLiteral,
                    format!(
                        "a write to release-tier {owner} needs the repository typed as owner/name on this command line"
                    ),
                    format!("-R {owner}/NAME"),
                    Vec::new(),
                );
            }
        }
        // The tier wall (L2 and L6 at decide time).
        let Some(profile) = self.profile(writer) else {
            return refuse(
                RefusalCode::TierMismatch,
                format!("{owner}'s writer {writer} is no profile"),
                crate::config::POLICY_CHECK.to_string(),
                Vec::new(),
            );
        };
        if profile.tier != tier {
            return tier_mismatch(writer, profile.tier, owner, tier);
        }
        let judged = presence(writer);
        let tried = vec![(ChainStep::Profile(writer.to_string()), judged)];
        match judged {
            Presence::Present => Decision::Send {
                profile: writer.to_string(),
                tier: profile.tier,
            },
            Presence::Absent => refuse(
                RefusalCode::WriterAbsent,
                format!("the writer for {owner} ({writer}) is absent"),
                format!(
                    "a person runs: agh auth add {writer} --file PATH --tier {} --owner {owner} --write",
                    profile.tier.slug()
                ),
                tried,
            ),
            Presence::Unusable(slug) => refuse(
                RefusalCode::WriterUnusable,
                format!("the writer for {owner} ({writer}) is unusable: {slug}"),
                "agh auth status".to_string(),
                tried,
            ),
        }
    }

    /// The owner's whole read chain, each step with its presence (the anonymous step Present),
    /// the owner's writer with its presence, for every access, and `decide`'s decision for the
    /// request: `whoami --for` shows the chain and the writer beside the answer.
    #[must_use]
    pub fn explain(&self, req: &RouteRequest, presence: &dyn Fn(&str) -> Presence) -> Explanation {
        let owner = req.owner();
        let chain = self
            .read_chain(owner)
            .into_iter()
            .map(|step| {
                let judged = match &step {
                    ChainStep::Profile(name) => presence(name),
                    ChainStep::Anonymous => Presence::Present,
                };
                (step, judged)
            })
            .collect();
        let writer = owner
            .and_then(|o| self.writer_of(o))
            .map(|w| (w.to_string(), presence(w)));
        let decision = self.decide(req, presence);
        Explanation {
            owner: owner.map(str::to_string),
            tier: self.target_tier(req),
            access: req.access,
            chain,
            writer,
            decision,
        }
    }
}

/// A refusal with its code, texts and judged steps.
fn refuse(
    code: RefusalCode,
    why: String,
    next: String,
    tried: Vec<(ChainStep, Presence)>,
) -> Decision {
    Decision::Refuse(Refusal {
        code,
        why,
        next,
        tried,
    })
}

/// `ProfileUnknown` for `name`.
fn unknown_profile(name: &str) -> Decision {
    refuse(
        RefusalCode::ProfileUnknown,
        format!("no profile named {name}"),
        "agh auth status".to_string(),
        Vec::new(),
    )
}

/// `TierMismatch`: `profile`, of `ptier`, would speak for `owner`, of `tier`.
fn tier_mismatch(profile: &str, ptier: Tier, owner: &str, tier: Tier) -> Decision {
    refuse(
        RefusalCode::TierMismatch,
        format!(
            "{profile} is {}, and {owner} is {}: a token speaks only for its own tier",
            ptier.slug(),
            tier.slug()
        ),
        crate::config::POLICY_CHECK.to_string(),
        Vec::new(),
    )
}

/// The model's view of a decision, `(phase, used)`: Send at release tier → (1, 2); any other
/// Send → (1, 1); Anonymous → (2, 0); Refuse → (3, 0).
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn project_decision(decision: &Decision) -> (i64, i64) {
    match decision {
        Decision::Send {
            tier: Tier::Release,
            ..
        } => (1, 2),
        Decision::Send { .. } => (1, 1),
        Decision::Anonymous { .. } => (2, 0),
        Decision::Refuse(_) => (3, 0),
    }
}

/// The host of the sixteen `spec_unmodeled` waivers of `AghRouteWall` (design §17.1): the
/// seven world actions, which choose the request and what the machine holds, and the nine
/// `Buggy = 1` mutants, which no shipping code performs. It does nothing.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghRouteWall",
        action = "NextTier",
        reason = "Bounded nondeterministic environment selection: which owner tier the next request names"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghRouteWall",
        action = "NextOp",
        reason = "Bounded nondeterministic environment selection: which access class it asks"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghRouteWall",
        action = "ToggleNamed",
        reason = "Bounded nondeterministic environment selection: whether the slug was typed literally"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghRouteWall",
        action = "ToggleDev",
        reason = "Bounded nondeterministic environment selection: whether the dev token file is present"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghRouteWall",
        action = "ToggleRel",
        reason = "Bounded nondeterministic environment selection: whether the release token file is present"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghRouteWall",
        action = "ToggleAmbient",
        reason = "Bounded nondeterministic environment selection: whether GH_TOKEN is set"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghRouteWall",
        action = "Reset",
        reason = "the router keeps no state between requests"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghRouteWall",
        action = "BuggyDecideFallback",
        reason = "Buggy=1 negative control only; the router has no such branch: TierWall is held by Decide"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghRouteWall",
        action = "BuggyDecideAlias",
        reason = "Buggy=1 negative control only; the router has no such branch: NamedDestination is held by Decide"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghRouteWall",
        action = "BuggyDecideReleaseAbroad",
        reason = "Buggy=1 negative control only; the router has no such branch: ReleaseTokenStaysHome is held by Decide"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghRouteWall",
        action = "BuggyDecideAmbient",
        reason = "Buggy=1 negative control only; the router has no such branch: NoAmbient is held by Decide"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghRouteWall",
        action = "BuggyAnonymousCarries",
        reason = "Buggy=1 negative control only; the router has no such branch: AnonymousCarriesNothing is held by Decide"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghRouteWall",
        action = "BuggyDecideAnonWrite",
        reason = "Buggy=1 negative control only; the router has no such branch: NoAnonymousWriterOrWrite is held by Decide"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghRouteWall",
        action = "BuggyRefuseButSend",
        reason = "Buggy=1 negative control only; the router has no such branch: RefusalSendsNothing is held by Decide"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghRouteWall",
        action = "BuggyHopKeepsAuth",
        reason = "Buggy=1 negative control only; the router has no such branch: HopDropsAuth is held by Hop"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghRouteWall",
        action = "BuggyHopOnWrite",
        reason = "Buggy=1 negative control only; the router has no such branch: NoWriteHop is held by Hop"
    )
)]
#[doc(hidden)]
pub fn route_wall_environment() {}

// The in-crate tests, in crate because only agh makes a `Named::Literal` (design §10.2)
// or a `Validated` that `validate` did not check (plan §4.6.12). The conformance writes token
// files, which are unix-only (plan §1, rule 12).
#[cfg(all(test, unix))]
#[path = "policy_conformance_tests.rs"]
mod conformance_tests;
#[cfg(test)]
#[path = "policy_tests.rs"]
mod tests;
