// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Routing tiers, access classes, requests, decisions, and `Router`.

use aterm_json::{Map, Value};

use crate::config::{ConfigOrigin, Validated, on_floor};
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

/// How the destination was named.
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

/// A repository typed as `OWNER/NAME` on this command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExplicitTarget {
    slug: Slug,
    provenance: Provenance,
    grant: Option<String>,
}

/// How an [`ExplicitTarget`] was made (design §10.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provenance {
    /// Typed literally on agh's own command line (`from_argv`).
    Argv,
    /// [`Router::grant`] made it from a `[[writers]]` row of the owner's agh.toml.
    Granted,
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
            RepoArg::Literal(slug) => Ok(ExplicitTarget {
                slug,
                provenance: Provenance::Argv,
                grant: None,
            }),
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

    /// How this target was made.
    #[must_use]
    pub fn provenance(&self) -> Provenance {
        self.provenance
    }

    /// The `[[writers]]` caller whose row made it, when [`Provenance::Granted`].
    #[must_use]
    pub fn granted_to(&self) -> Option<&str> {
        self.grant.as_deref()
    }
}

/// What a request is about.
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

/// One routing question.
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

    /// A write of `target`.
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
    pub const fn with_anonymous(mut self, anonymous: bool) -> RouteRequest {
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
    /// A release-tier write named by a library caller with no `[[writers]]` grant (L11).
    NotGranted,
}

impl RefusalCode {
    /// Returns the `snake_case` refusal code identifier.
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
            RefusalCode::NotGranted => "not_granted",
        }
    }

    /// The first four → Unauthenticated; the next eight and the last → Refused; the four between
    /// → Usage.
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
            | RefusalCode::MethodNotRead
            | RefusalCode::NotGranted => Kind::Refused,
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

/// The Router's answer.
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

/// A presence into `item`, the one form a chain step and the writer share.
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
    /// Formats the explanation as a canonical JSON object.
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

/// The routing rules over a validated agh.toml (or none), or over one profile
/// ([`Router::single_identity`]).
#[derive(Clone, Debug, Default)]
pub struct Router {
    validated: Option<Validated>,
    single: Option<Profile>,
}

impl Router {
    /// No config: every read anonymous (`NoConfig`), every write refused.
    #[inline]
    #[must_use]
    pub const fn empty() -> Router {
        Router {
            validated: None,
            single: None,
        }
    }

    /// The rules of `validated`.
    #[inline]
    #[must_use]
    pub const fn from_validated(validated: Validated) -> Router {
        Router {
            validated: Some(validated),
            single: None,
        }
    }

    /// Whether an agh.toml stands behind it.
    #[inline]
    #[must_use]
    pub const fn has_config(&self) -> bool {
        self.validated.is_some()
    }

    /// Resolves the effective tier for an owner (`Release`, configured row tier, or `External`).
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

    /// The owner's chain (`None`: the default chain).
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

    /// THE decision, with its two decide-time tier walls.
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

    /// The owner as the refusal texts name it: the owner, or `the default` for a path with none.
    fn owner_label(req: &RouteRequest) -> String {
        req.owner()
            .map_or_else(|| "the default".to_string(), str::to_string)
    }

    /// What `agh whoami --for` and `-R` name.
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

    /// A WriterRead or a Write.
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
                    "agh auth add {writer} --file PATH --tier {} --owner {owner} --write",
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

    /// Evaluates the owner's read chain, writer presence, and routing decision for `whoami --for`.
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

/// What a [`Permit`] lets a write reach.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PermitTarget {
    /// `/repos/{owner}/{repo}` and below, on the api and uploads origins.
    Repo(Slug),
    /// `/user` and below (machines' keys, under [`Router::single_identity`]).
    Identity,
}

/// A write permit (design §10.2). Only [`Router::permit`] mints one, and it is not `Clone`,
/// so one routing decision is one permit. `HttpTransport` sends a method other than GET or
/// HEAD only with a permit whose target covers the URL's path and whose profile is the
/// credential's (design §10.1).
#[derive(Debug, PartialEq, Eq)]
pub struct Permit {
    target: PermitTarget,
    profile: String,
    tier: Tier,
    provenance: Option<Provenance>,
    grant: Option<String>,
    literal: Option<ExplicitTarget>,
}

impl Permit {
    /// What the permit reaches.
    #[must_use]
    pub fn target(&self) -> &PermitTarget {
        &self.target
    }

    /// The profile the write speaks as.
    #[must_use]
    pub fn profile(&self) -> &str {
        &self.profile
    }

    /// That profile's tier.
    #[must_use]
    pub fn tier(&self) -> Tier {
        self.tier
    }

    /// How the destination was named: `None` for a write that needed no literal (a dev-tier
    /// owner's, or an identity write under `single_identity`).
    #[must_use]
    pub fn provenance(&self) -> Option<Provenance> {
        self.provenance
    }

    /// The `[[writers]]` caller, when a grant made the target: the journal's `grant`.
    #[must_use]
    pub fn grant(&self) -> Option<&str> {
        self.grant.as_deref()
    }

    /// Whether `path` (absolute, its query cut) is this permit's target or below it. Each
    /// segment is judged percent-decoded (`aterm_codec::pct::decode`), as GitHub reads it and
    /// as `api`'s routing already judges it (`cli/cmd_api.rs`, `fn target`), then compared
    /// ASCII case-insensitively: `/repos/{owner}/{repo}` exactly or followed by `/`, or
    /// `/user` exactly or followed by `/`. So `/repos/fixture%2Ddev/app` is covered by a
    /// permit for `fixture-dev/app`. These never match:
    /// - a dot segment, typed or encoded;
    /// - an empty segment;
    /// - a `%` not followed by two hex digits;
    /// - a segment that decodes to hold `/`.
    ///
    /// `pct::decode` is total: it passes a malformed escape through unchanged. So `covers`
    /// checks the raw segment for a `%` not followed by two hex digits itself, before it
    /// decodes, and answers `false` for one.
    #[must_use]
    pub fn covers(&self, path: &str) -> bool {
        // WP1 writes the body (plan §5, body 7); until then no path is covered, so the
        // transport guard refuses every write.
        let _ = path;
        false
    }

    /// The naming the permit's write was routed with: `Named::Literal` of its literal, or
    /// `Named::Default` when it had none. A git run done for the write (`Refs::delete_expect`)
    /// is named the same way, so the runner's release-tier rule sees the same literal.
    pub(crate) fn named(&self) -> Named {
        self.literal.clone().map_or(Named::Default, Named::Literal)
    }
}

impl Router {
    /// The one library constructor of an `ExplicitTarget`: `slug` must be listed under
    /// `caller`'s `[[writers]]` row (L11). Otherwise Refused `policy.not_granted`. `caller` is
    /// self-asserted inside the one binary: a grant limits which repositories the owner's config
    /// opens to unattended writes, not which code may write. The journal records the grant and
    /// the builder's caller side by side.
    ///
    /// # Errors
    ///
    /// - Refused `not_granted` (`Sub::Policy(RefusalCode::NotGranted)`), next
    ///   `a person runs: agh policy grant {caller} {slug}`, when no `[[writers]]` row of
    ///   `caller` lists `slug` (ASCII case-insensitive), or this router's `Validated` did not
    ///   come from a file (`ConfigOrigin::Text`), or it has none;
    /// - Usage when `caller` is not `[a-z][a-z0-9-]{0,31}`.
    pub fn grant(&self, caller: &str, slug: &Slug) -> Result<ExplicitTarget> {
        let _ = (caller, slug);
        Err(Error::not_yet("Router::grant", "WP1"))
    }

    /// A write permit for `req`, which must be a Write: refused unless `decide(req)` sends.
    /// It names the target, the profile and the tier, and `HttpTransport` checks it against
    /// the URL (§10.1). A release-tier target still needs its `Named::Literal`, from argv or
    /// `grant`. It takes a `RouteRequest` rather than an `ExplicitTarget` because every write
    /// carries one, and a library caller's dev-tier write has no `ExplicitTarget` (L11 refuses
    /// a dev-tier grant).
    ///
    /// # Errors
    ///
    /// - Usage `a permit is for a write; this request is {access}` when `req.access` is not
    ///   `Access::Write`;
    /// - the refusal `decide` gives (`into_error`);
    /// - Refused `unscoped_not_read` for an `Owner` or `Unscoped` target.
    pub fn permit(
        &self,
        req: &RouteRequest,
        presence: &dyn Fn(&str) -> Presence,
    ) -> Result<Permit> {
        let _ = (req, presence);
        Err(Error::not_yet("Router::permit", "WP1"))
    }

    /// [`Router::permit`] over a decision already made: `permit` is
    /// `self.permit_decided(req, self.decide(req, presence))`. The client calls it with the
    /// decision `Fx::route` gave, so a replayed tape's decision mints the permit the recorded
    /// one did. Crate-private: only a decision `decide` or the route door made reaches it.
    ///
    /// # Errors
    ///
    /// As [`Router::permit`].
    pub(crate) fn permit_decided(&self, req: &RouteRequest, decision: Decision) -> Result<Permit> {
        let _ = (req, decision);
        Err(Error::not_yet("Router::permit_decided", "WP1"))
    }

    /// A router over one profile (an inline source, §6.1). It routes Identity targets only;
    /// every Repo, Owner or Unscoped target is refused `UnscopedNotRead`.
    /// A Write on an Identity target (machines' `users().keys()`) is sent only under this
    /// router, with its one profile; a release-tier profile is refused `TierMismatch`, and
    /// `AghRouteWall`'s M2 `ident` variable carries the rule, with its mutant.
    #[must_use]
    pub fn single_identity(profile: Profile) -> Router {
        Router {
            validated: None,
            single: Some(profile),
        }
    }

    /// Whether `single_identity` built this router.
    #[must_use]
    pub fn is_single_identity(&self) -> bool {
        self.single.is_some()
    }

    /// The `[[writers]]` rows (empty without a file-made `Validated`): `policy check` and
    /// `atpkg-publish hosts check` print them.
    #[must_use]
    pub fn writers(&self) -> Vec<(String, Vec<Slug>)> {
        self.validated
            .as_ref()
            .filter(|v| v.origin() == ConfigOrigin::File)
            .map(|v| v.writers().to_vec())
            .unwrap_or_default()
    }
}

/// A refusal with its code, texts and judged steps.
const fn refuse(
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
pub const fn project_decision(decision: &Decision) -> (i64, i64) {
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

/// The host of the sixteen `spec_unmodeled` waivers of `AghRouteWall` .
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

#[cfg(all(test, unix))]
#[path = "policy_conformance_tests.rs"]
mod conformance_tests;
#[cfg(test)]
#[path = "policy_tests.rs"]
mod tests;
