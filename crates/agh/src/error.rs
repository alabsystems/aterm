// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Kinds, sub-kinds, errors, the exit table and the two-line rendering (design §7, §8.5).

use aterm_json::{Map, Value};

use crate::policy::RefusalCode;
use crate::rate::RateSnapshot;
use crate::redact::Redactor;
use crate::wire::WireKind;

/// What went wrong: sixteen kinds, one exit code each.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    /// agh's own gate refused; nothing was sent (exit 1).
    Refused,
    /// Bad flags or arguments (exit 2).
    Usage,
    /// The request could not run: network, trust store, proxy, deadline or local I/O (exit 3).
    CouldNotRun,
    /// No routed credential, an unusable token file, or a 401 (exit 4).
    Unauthenticated,
    /// GitHub refused the permission: a 403 or 451 (exit 5).
    Forbidden,
    /// Confirmed absent: the parent is visible to this identity (exit 6).
    NotFound,
    /// A 404 where the parent cannot be seen (exit 7).
    NotVisible,
    /// A wait timed out (exit 8).
    Pending,
    /// The state disagrees with the request (exit 9).
    Conflict,
    /// GitHub or agh rejected the input (exit 10).
    Invalid,
    /// A primary or secondary rate limit (exit 11).
    RateLimited,
    /// GitHub answered 5xx after the retry budget (exit 12).
    Server,
    /// A write was sent and no answer was read (exit 13).
    OutcomeUnknown,
    /// The answer is not the whole collection (exit 14).
    Incomplete,
    /// The subject reports failure (exit 15).
    Failed,
    /// Interrupted by SIGINT (exit 130).
    Interrupted,
}

impl Kind {
    /// Every kind, in exit order.
    pub const ALL: [Kind; 16] = [
        Kind::Refused,
        Kind::Usage,
        Kind::CouldNotRun,
        Kind::Unauthenticated,
        Kind::Forbidden,
        Kind::NotFound,
        Kind::NotVisible,
        Kind::Pending,
        Kind::Conflict,
        Kind::Invalid,
        Kind::RateLimited,
        Kind::Server,
        Kind::OutcomeUnknown,
        Kind::Incomplete,
        Kind::Failed,
        Kind::Interrupted,
    ];

    /// 1 to 15 in declaration order; `Interrupted` is 130.
    #[must_use]
    pub const fn exit(self) -> u8 {
        match self {
            Kind::Refused => 1,
            Kind::Usage => 2,
            Kind::CouldNotRun => 3,
            Kind::Unauthenticated => 4,
            Kind::Forbidden => 5,
            Kind::NotFound => 6,
            Kind::NotVisible => 7,
            Kind::Pending => 8,
            Kind::Conflict => 9,
            Kind::Invalid => 10,
            Kind::RateLimited => 11,
            Kind::Server => 12,
            Kind::OutcomeUnknown => 13,
            Kind::Incomplete => 14,
            Kind::Failed => 15,
            Kind::Interrupted => 130,
        }
    }

    /// `refused`, `usage`, `could_not_run`, `unauthenticated`, `forbidden`, `not_found`,
    /// `not_visible`, `pending`, `conflict`, `invalid`, `rate_limited`, `server`,
    /// `outcome_unknown`, `incomplete`, `failed`, `interrupted`.
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Kind::Refused => "refused",
            Kind::Usage => "usage",
            Kind::CouldNotRun => "could_not_run",
            Kind::Unauthenticated => "unauthenticated",
            Kind::Forbidden => "forbidden",
            Kind::NotFound => "not_found",
            Kind::NotVisible => "not_visible",
            Kind::Pending => "pending",
            Kind::Conflict => "conflict",
            Kind::Invalid => "invalid",
            Kind::RateLimited => "rate_limited",
            Kind::Server => "server",
            Kind::OutcomeUnknown => "outcome_unknown",
            Kind::Incomplete => "incomplete",
            Kind::Failed => "failed",
            Kind::Interrupted => "interrupted",
        }
    }

    /// The meaning column of the exit table.
    #[must_use]
    pub const fn meaning(self) -> &'static str {
        match self {
            Kind::Refused => "agh's own gate; nothing was sent",
            Kind::Usage => "bad flags or arguments",
            Kind::CouldNotRun => {
                "the request could not run: network, trust store, proxy, deadline or local I/O"
            }
            Kind::Unauthenticated => "no routed credential, an unusable token file, or a 401",
            Kind::Forbidden => "GitHub refused the permission (403 or 451)",
            Kind::NotFound => "confirmed absent: the parent is visible to this identity",
            Kind::NotVisible => "a 404 where the parent cannot be seen",
            Kind::Pending => "a wait timed out (the last state is on stdout)",
            Kind::Conflict => "the state disagrees with the request",
            Kind::Invalid => "GitHub or agh rejected the input",
            Kind::RateLimited => "a primary or secondary rate limit",
            Kind::Server => "GitHub answered 5xx after the retry budget",
            Kind::OutcomeUnknown => "a write was sent and no answer was read",
            Kind::Incomplete => "the answer is not the whole collection",
            Kind::Failed => "the subject reports failure",
            Kind::Interrupted => "interrupted (SIGINT)",
        }
    }

    /// The next-action column of the exit table.
    #[must_use]
    pub const fn next_action(self) -> &'static str {
        match self {
            Kind::Refused => "read why=; fix the config or run it as the person it needs",
            Kind::Usage => "agh help <verb>",
            Kind::CouldNotRun => "retry later",
            Kind::Unauthenticated => {
                "a person provisions or rotates the credential (agh auth status)"
            }
            Kind::Forbidden => "a person grants the permission named in sub=",
            Kind::NotFound => "the thing does not exist",
            Kind::NotVisible => {
                "never treat as absent; check tried= and the routing (agh whoami --for REPO)"
            }
            Kind::Pending => "wait longer or look",
            Kind::Conflict => "read sub=; never retry blindly",
            Kind::Invalid => "fix the input",
            Kind::RateLimited => "wait until reset= or retry_after=",
            Kind::Server => "retry later",
            Kind::OutcomeUnknown => {
                "re-run the same command: it discovers first; never re-POST by hand"
            }
            Kind::Incomplete => "--partial if a partial answer will do",
            Kind::Failed => "look at the subject",
            Kind::Interrupted => "-",
        }
    }

    /// True for CouldNotRun, Pending, RateLimited, Server and OutcomeUnknown (exits 3, 8, 11,
    /// 12, 13).
    #[must_use]
    pub const fn retriable(self) -> bool {
        matches!(
            self,
            Kind::CouldNotRun
                | Kind::Pending
                | Kind::RateLimited
                | Kind::Server
                | Kind::OutcomeUnknown
        )
    }

    /// The gh dialect's exit (M4): Unauthenticated → 4; Pending → 8 when `pending_is_8`, else
    /// 1; Interrupted → 2; every other kind → 1.
    #[must_use]
    pub const fn gh_exit(self, pending_is_8: bool) -> u8 {
        match self {
            Kind::Unauthenticated => 4,
            Kind::Pending if pending_is_8 => 8,
            Kind::Interrupted => 2,
            _ => 1,
        }
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.slug())
    }
}

/// One row of the exit table, as `agh help` and `agh schema exit` print it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExitRow {
    /// The exit code.
    pub exit: u8,
    /// The kind's slug (`ok` for exit 0).
    pub slug: &'static str,
    /// What the exit means.
    pub meaning: &'static str,
    /// What to do next.
    pub next: &'static str,
}

/// The exit table: `(0, "ok", "done", "continue")`, then every kind of `Kind::ALL`.
#[must_use]
pub fn exit_rows() -> Vec<ExitRow> {
    let mut rows = vec![ExitRow {
        exit: 0,
        slug: "ok",
        meaning: "done",
        next: "continue",
    }];
    rows.extend(Kind::ALL.iter().map(|kind| ExitRow {
        exit: kind.exit(),
        slug: kind.slug(),
        meaning: kind.meaning(),
        next: kind.next_action(),
    }));
    rows
}

/// Whether a write may have taken effect. Always `None` in M1.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SideEffect {
    /// Nothing was written.
    #[default]
    None,
    /// A write may or may not have taken effect.
    Unknown,
    /// A write took effect.
    Applied,
}

impl SideEffect {
    /// `none`, `unknown`, `applied`.
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            SideEffect::None => "none",
            SideEffect::Unknown => "unknown",
            SideEffect::Applied => "applied",
        }
    }
}

/// Which permission GitHub refused (a 403 or 451 that is not a rate limit), or a writer agh
/// will not trust to see drafts. `#[non_exhaustive]`, so M2 adds variants additively.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Forbid {
    /// "Resource not accessible by …", or an `X-Accepted-GitHub-Permissions` answer.
    Permission {
        /// The `X-Accepted-GitHub-Permissions` value, when GitHub sent one.
        accepted: Option<String>,
    },
    /// `X-GitHub-SSO: required; url=…`.
    Sso {
        /// The text after `url=`.
        url: Option<String>,
    },
    /// A WriterRead whose identity lacks `permissions.push`.
    NoPush,
    /// A WriterRead by a fine-grained (`github_pat_`) writer whose profile carries no
    /// `drafts_fp` equal to its token's fingerprint (design §6.4): refused before anything is
    /// sent, until a person vouches with `agh auth add NAME … --write --drafts`.
    DraftsUnproven,
    /// 451.
    Blocked,
    /// Any other 403.
    Other,
}

impl Forbid {
    /// `forbid.permission`, `forbid.sso`, `forbid.no_push`, `forbid.drafts_unproven`,
    /// `forbid.blocked`, `forbid.other`.
    #[must_use]
    pub const fn slug(&self) -> &'static str {
        match self {
            Forbid::Permission { .. } => "forbid.permission",
            Forbid::Sso { .. } => "forbid.sso",
            Forbid::NoPush => "forbid.no_push",
            Forbid::DraftsUnproven => "forbid.drafts_unproven",
            Forbid::Blocked => "forbid.blocked",
            Forbid::Other => "forbid.other",
        }
    }
}

/// Why a replayed run stopped (design §21.3). Seven of astream-effects' eight
/// `ReplayDivergence` variants map one to one; `Unreached` (a tape longer than the run, from
/// `ReplaySeam::finish`, astream ac17049) maps to `Unconsumed`; the rest are agh's own.
/// `#[non_exhaustive]`. `ReplayDivergence` itself is exhaustive, so `Replay::of` is an
/// exhaustive match with no `_` arm: a ninth astream variant fails agh's build, by design.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Replay {
    /// The run asked for more than the tape holds.
    Exhausted,
    /// Another kind of effect is next on the tape.
    KindMismatch,
    /// A file effect for another path (agh's doors never raise it).
    PathDiverged,
    /// Another door is next on the tape.
    DoorDiverged,
    /// The same door, another request: the run asked GitHub another question.
    RequestDiverged,
    /// stdout or the exit differs from the tape's.
    OutputDiverged,
    /// A keyed view holds no such exchange (agh's doors never raise it).
    Unrecorded,
    /// `--replay` with a verb whose command line is not the tape's (a usage error, exit 2).
    Argv,
    /// Records were left on the tape after the run's exit (astream's `Unreached`).
    Unconsumed,
    /// The run reached a reply the tape holds only by digest (a body over 1 MiB).
    BodyNotTaped,
    /// A streaming door, or a request with a body, under a tape (M1 tapes neither).
    StreamNotTaped,
    /// Not a tape in agh's subset of atape-text 1, or a header agh cannot replay.
    Format,
}

impl Replay {
    /// `exhausted`, `kind`, `path`, `door`, `request`, `output`, `unrecorded`, `argv`,
    /// `unconsumed`, `body_not_taped`, `stream_not_taped`, `format`.
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Replay::Exhausted => "exhausted",
            Replay::KindMismatch => "kind",
            Replay::PathDiverged => "path",
            Replay::DoorDiverged => "door",
            Replay::RequestDiverged => "request",
            Replay::OutputDiverged => "output",
            Replay::Unrecorded => "unrecorded",
            Replay::Argv => "argv",
            Replay::Unconsumed => "unconsumed",
            Replay::BodyNotTaped => "body_not_taped",
            Replay::StreamNotTaped => "stream_not_taped",
            Replay::Format => "format",
        }
    }

    /// The variant for astream's divergence: `Exhausted`, `KindMismatch`, `PathDiverged`,
    /// `DoorDiverged`, `RequestDiverged`, `OutputDiverged` and `Unrecorded` one to one,
    /// `Unreached{..}` → `Unconsumed`.
    #[must_use]
    pub fn of(divergence: &astream_effects::ReplayDivergence) -> Replay {
        use astream_effects::ReplayDivergence as D;
        match divergence {
            D::Exhausted { .. } => Replay::Exhausted,
            D::Unreached { .. } => Replay::Unconsumed,
            D::KindMismatch { .. } => Replay::KindMismatch,
            D::PathDiverged { .. } => Replay::PathDiverged,
            D::DoorDiverged { .. } => Replay::DoorDiverged,
            D::RequestDiverged { .. } => Replay::RequestDiverged,
            D::OutputDiverged { .. } => Replay::OutputDiverged,
            D::Unrecorded { .. } => Replay::Unrecorded,
        }
    }
}

/// The sub-kind: what exactly, within the kind. `#[non_exhaustive]`, so M2 adds variants
/// additively; a match outside agh carries a `_` arm.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Sub {
    /// No sub-kind.
    #[default]
    None,
    /// Which permission GitHub refused.
    Forbid(Forbid),
    /// A primary rate limit.
    Primary,
    /// A secondary rate limit.
    Secondary,
    /// Refused before sending: the rate book says the budget is spent.
    PreRefused,
    /// The thing already exists.
    AlreadyExists,
    /// The repository moved (a 301).
    Moved {
        /// Where it moved to.
        to: String,
    },
    /// More than one release has the tag.
    AmbiguousTag {
        /// The releases' ids.
        ids: Vec<u64>,
    },
    /// The bytes do not match the digest.
    DigestMismatch {
        /// The digest the asset names.
        expected: String,
        /// The digest of the bytes read.
        got: String,
    },
    /// The bytes are not the size the asset names.
    SizeMismatch {
        /// The size the asset names.
        expected: u64,
        /// The bytes read.
        got: u64,
    },
    /// The asset is not uploaded.
    NotUploaded {
        /// The asset's state.
        state: String,
    },
    /// The listing changed while it was read.
    Torn {
        /// The page that showed the change.
        page: u32,
    },
    /// The listing stopped at its page cap.
    Capped {
        /// The pages read.
        pages: u32,
    },
    /// GitHub truncated the tree.
    TruncatedTree,
    /// The directory holds 1000 entries or more.
    DirectoryCap,
    /// The comparison hit GitHub's caps.
    CompareCap,
    /// The search hit GitHub's cap.
    SearchCap,
    /// What failed on the wire.
    Wire(WireKind),
    /// GitHub's answer broke the protocol.
    Protocol,
    /// A local failure.
    Local,
    /// Why the Router refused.
    Policy(RefusalCode),
    /// A replayed run diverged from its tape, or the tape is not one agh can replay.
    Replay(Replay),
}

impl Sub {
    /// `None` for `Sub::None`. Otherwise the snake_case variant name (`primary`, `secondary`,
    /// `pre_refused`, `already_exists`, `moved`, `ambiguous_tag`, `digest_mismatch`,
    /// `size_mismatch`, `not_uploaded`, `torn`, `capped`, `truncated_tree`, `directory_cap`,
    /// `compare_cap`, `search_cap`, `protocol`, `local`), `forbid.<…>` from `Forbid::slug`,
    /// `wire.<kind>` from `WireKind::slug`, `policy.<code>` from `RefusalCode::slug`, or
    /// `replay.<what>` from `Replay::slug`.
    #[must_use]
    pub fn slug(&self) -> Option<String> {
        let fixed = match self {
            Sub::None => return None,
            Sub::Forbid(forbid) => return Some(forbid.slug().to_string()),
            Sub::Wire(kind) => return Some(format!("wire.{}", kind.slug())),
            Sub::Policy(code) => return Some(format!("policy.{}", code.slug())),
            Sub::Replay(what) => return Some(format!("replay.{}", what.slug())),
            Sub::Primary => "primary",
            Sub::Secondary => "secondary",
            Sub::PreRefused => "pre_refused",
            Sub::AlreadyExists => "already_exists",
            Sub::Moved { .. } => "moved",
            Sub::AmbiguousTag { .. } => "ambiguous_tag",
            Sub::DigestMismatch { .. } => "digest_mismatch",
            Sub::SizeMismatch { .. } => "size_mismatch",
            Sub::NotUploaded { .. } => "not_uploaded",
            Sub::Torn { .. } => "torn",
            Sub::Capped { .. } => "capped",
            Sub::TruncatedTree => "truncated_tree",
            Sub::DirectoryCap => "directory_cap",
            Sub::CompareCap => "compare_cap",
            Sub::SearchCap => "search_cap",
            Sub::Protocol => "protocol",
            Sub::Local => "local",
        };
        Some(fixed.to_string())
    }
}

/// The HTTP exchange an error came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpRef {
    /// The status GitHub answered.
    pub status: u16,
    /// The request's method.
    pub method: &'static str,
    /// The request's path.
    pub path: String,
    /// `X-GitHub-Request-Id`, when GitHub sent one.
    pub request_id: Option<String>,
}

/// GitHub's error body, read leniently, and the permission headers of the same answer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GithubBody {
    /// `message`, at most 500 characters.
    pub message: Option<String>,
    /// `documentation_url`.
    pub documentation_url: Option<String>,
    /// `errors`.
    pub errors: Vec<FieldError>,
    /// `X-Accepted-GitHub-Permissions`: what a fine-grained token would need (`contents=read`).
    pub accepted_permissions: Option<String>,
    /// `X-OAuth-Scopes`: what a classic token holds.
    pub oauth_scopes: Option<String>,
    /// `X-Accepted-OAuth-Scopes`: what the route accepts (`admin:public_key`).
    pub accepted_oauth_scopes: Option<String>,
}

/// The longest `message` kept from GitHub's error body, in characters.
const MESSAGE_CAP: usize = 500;
/// The longest permission header value kept, in characters.
const HEADER_CAP: usize = 200;

/// `text` cut to at most `cap` characters.
fn cap_chars(text: &str, cap: usize) -> String {
    text.chars().take(cap).collect()
}

/// `text` cut to at most `cap` characters without cutting inside a run of non-whitespace: a
/// run the cap would split is dropped whole, back to the ASCII whitespace before it. The cut
/// runs before any redaction can, so a learned value with no known prefix that crossed the
/// cap would otherwise leave a part no rule matches.
fn cap_whole_runs(text: &str, cap: usize) -> String {
    let Some((end, next)) = text.char_indices().nth(cap) else {
        return text.to_string();
    };
    let kept = &text[..end];
    if next.is_ascii_whitespace() {
        return kept.to_string();
    }
    let back = kept.rfind(|c: char| c.is_ascii_whitespace()).unwrap_or(0);
    kept[..back].to_string()
}

/// A string field of a JSON object.
fn string_at(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

impl GithubBody {
    /// Never fails: a body that is not JSON gives the default. `message` is capped at 500
    /// characters, and a run of non-whitespace the cap would split is dropped whole (it may
    /// be a credential the redactor knows only whole). A string item of `errors` becomes
    /// `FieldError { message: Some(it), .. }`.
    #[must_use]
    pub fn parse(body: &[u8]) -> GithubBody {
        let Ok(value) = aterm_json::from_slice::<Value>(body) else {
            return GithubBody::default();
        };
        if !value.is_object() {
            return GithubBody::default();
        }
        let errors = value
            .get("errors")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|item| match item {
                        Value::String(text) => FieldError {
                            message: Some(text.clone()),
                            ..FieldError::default()
                        },
                        other => FieldError {
                            resource: string_at(other, "resource"),
                            field: string_at(other, "field"),
                            code: string_at(other, "code"),
                            message: string_at(other, "message"),
                        },
                    })
                    .collect()
            })
            .unwrap_or_default();
        GithubBody {
            message: string_at(&value, "message").map(|m| cap_whole_runs(&m, MESSAGE_CAP)),
            documentation_url: string_at(&value, "documentation_url"),
            errors,
            ..GithubBody::default()
        }
    }

    /// The three permission headers' first values (names ASCII case-insensitive), each capped
    /// at 200 characters; an absent header leaves its field `None`.
    #[must_use]
    pub fn with_headers(mut self, headers: &[(String, String)]) -> GithubBody {
        let first =
            |name: &str| crate::wire::header_in(headers, name).map(|v| cap_chars(v, HEADER_CAP));
        self.accepted_permissions = first("x-accepted-github-permissions");
        self.oauth_scopes = first("x-oauth-scopes");
        self.accepted_oauth_scopes = first("x-accepted-oauth-scopes");
        self
    }
}

/// One item of GitHub's `errors` array.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FieldError {
    /// `resource`.
    pub resource: Option<String>,
    /// `field`.
    pub field: Option<String>,
    /// `code`.
    pub code: Option<String>,
    /// `message`, or the item itself when it is a string.
    pub message: Option<String>,
}

/// Who a request spoke as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdentityRef {
    /// A profile from agh.toml.
    Profile {
        /// The profile's name.
        name: String,
        /// Its token's fingerprint.
        fingerprint: String,
    },
    /// No credential.
    Anonymous,
}

impl IdentityRef {
    /// The profile's name, or `anonymous`.
    #[must_use]
    pub fn label(&self) -> &str {
        match self {
            IdentityRef::Profile { name, .. } => name,
            IdentityRef::Anonymous => "anonymous",
        }
    }

    /// The profile's fingerprint; `None` when anonymous.
    #[must_use]
    pub fn fingerprint(&self) -> Option<&str> {
        match self {
            IdentityRef::Profile { fingerprint, .. } => Some(fingerprint),
            IdentityRef::Anonymous => None,
        }
    }
}

/// One step a refusal or a 404 judgement took.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tried {
    /// A request sent as `as_`, answered `status`.
    Sent {
        /// The identity's label.
        as_: String,
        /// The status it answered.
        status: u16,
    },
    /// The 404 probe of `path` (`repos/o/r`), answered `status`.
    Probe {
        /// The path probed.
        path: String,
        /// The status it answered.
        status: u16,
    },
    /// A chain step passed over, and why (`absent`, or an unusable slug).
    Skipped {
        /// The step's label.
        as_: String,
        /// Why it was passed over.
        why: String,
    },
}

/// An agh error: one pointer wide, so that every `Result<T>` stays under clippy's
/// `result_large_err` threshold (128 bytes), which agh may not silence. Its fields are read
/// through `Deref` (`err.kind`, `err.why`) and set through the builders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(Box<ErrorData>);

impl std::ops::Deref for Error {
    type Target = ErrorData;

    fn deref(&self) -> &ErrorData {
        &self.0
    }
}

impl std::ops::DerefMut for Error {
    fn deref_mut(&mut self) -> &mut ErrorData {
        &mut self.0
    }
}

impl From<ErrorData> for Error {
    fn from(data: ErrorData) -> Error {
        Error(Box::new(data))
    }
}

/// Everything an error carries: the kind, the sub-kind and everything the two lines print.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ErrorData {
    /// What went wrong.
    pub kind: Kind,
    /// What exactly, within the kind.
    pub sub: Sub,
    /// Why, in one sentence.
    pub why: String,
    /// What to do next.
    pub next: String,
    /// Whether a write may have taken effect.
    pub side_effect: SideEffect,
    /// The HTTP exchange it came from.
    pub http: Option<HttpRef>,
    /// GitHub's error body.
    pub github: Option<GithubBody>,
    /// Who the request spoke as.
    pub identity: Option<IdentityRef>,
    /// The last rate-limit snapshot.
    pub rate: Option<RateSnapshot>,
    /// `Retry-After`, in seconds.
    pub retry_after: Option<u64>,
    /// When the rate limit resets, in Unix seconds.
    pub reset: Option<u64>,
    /// The steps the judgement took.
    pub tried: Vec<Tried>,
    /// The repository, `owner/name`.
    pub repo: Option<String>,
}

impl Error {
    /// `kind` with `why`; `next` is `kind.next_action()`; everything else empty.
    pub fn new(kind: Kind, why: impl Into<String>) -> Error {
        Error::from(ErrorData {
            kind,
            sub: Sub::None,
            why: why.into(),
            next: kind.next_action().to_string(),
            side_effect: SideEffect::None,
            http: None,
            github: None,
            identity: None,
            rate: None,
            retry_after: None,
            reset: None,
            tried: Vec::new(),
            repo: None,
        })
    }

    /// The data, unboxed.
    #[must_use]
    pub fn into_data(self) -> ErrorData {
        *self.0
    }

    /// A Refused error.
    pub fn refused(why: impl Into<String>) -> Error {
        Error::new(Kind::Refused, why)
    }

    /// A Usage error.
    pub fn usage(why: impl Into<String>) -> Error {
        Error::new(Kind::Usage, why)
    }

    /// A CouldNotRun error.
    pub fn could_not_run(why: impl Into<String>) -> Error {
        Error::new(Kind::CouldNotRun, why)
    }

    /// A CouldNotRun `Sub::Protocol` error: an answer outside what agh reads.
    pub(crate) fn protocol(why: impl Into<String>) -> Error {
        Error::could_not_run(why).with_sub(Sub::Protocol)
    }

    /// The same error (its exchange, identity, rate, `sub`, `next` and `tried` kept) recast as
    /// `kind` with `why`; the caller sets whatever else the new verdict changes.
    #[must_use]
    pub(crate) fn recast(mut self, kind: Kind, why: impl Into<String>) -> Error {
        self.kind = kind;
        self.why = why.into();
        self
    }

    /// The same error, its why replaced by `why()` when it is NotFound: how a resource names
    /// the thing a 404 left absent.
    #[must_use]
    pub(crate) fn why_if_not_found(mut self, why: impl FnOnce() -> String) -> Error {
        if self.kind == Kind::NotFound {
            self.why = why();
        }
        self
    }

    /// A replay's stop: `Sub::Replay(what)` with `why`; Usage for `Replay::Argv`, CouldNotRun
    /// (exit 3) for every other. `next` is `re-record the tape: agh --replay OLD --record NEW`
    /// for `RequestDiverged`, `OutputDiverged` and `Unconsumed`, and the kind's next otherwise.
    pub fn replay(what: Replay, why: impl Into<String>) -> Error {
        let kind = if what == Replay::Argv {
            Kind::Usage
        } else {
            Kind::CouldNotRun
        };
        let error = Error::new(kind, why).with_sub(Sub::Replay(what));
        match what {
            Replay::RequestDiverged | Replay::OutputDiverged | Replay::Unconsumed => {
                error.with_next("re-record the tape: agh --replay OLD --record NEW")
            }
            _ => error,
        }
    }

    /// The same error with `sub`.
    #[must_use]
    pub fn with_sub(mut self, sub: Sub) -> Error {
        self.sub = sub;
        self
    }

    /// The same error with `next`.
    #[must_use]
    pub fn with_next(mut self, next: impl Into<String>) -> Error {
        self.next = next.into();
        self
    }

    /// The same error with `repo`.
    #[must_use]
    pub fn with_repo(mut self, repo: impl Into<String>) -> Error {
        self.repo = Some(repo.into());
        self
    }

    /// The same error with `side_effect`.
    #[must_use]
    pub fn with_side_effect(mut self, side_effect: SideEffect) -> Error {
        self.side_effect = side_effect;
        self
    }

    /// The same error with `http`.
    #[must_use]
    pub fn with_http(mut self, http: HttpRef) -> Error {
        self.http = Some(http);
        self
    }

    /// The same error with `github`.
    #[must_use]
    pub fn with_github(mut self, github: GithubBody) -> Error {
        self.github = Some(github);
        self
    }

    /// The same error with `identity`.
    #[must_use]
    pub fn with_identity(mut self, identity: IdentityRef) -> Error {
        self.identity = Some(identity);
        self
    }

    /// The same error with `rate`.
    #[must_use]
    pub fn with_rate(mut self, rate: RateSnapshot) -> Error {
        self.rate = Some(rate);
        self
    }

    /// The same error with `retry_after` seconds.
    #[must_use]
    pub fn with_retry_after(mut self, secs: u64) -> Error {
        self.retry_after = Some(secs);
        self
    }

    /// The same error with `reset`, in Unix seconds.
    #[must_use]
    pub fn with_reset(mut self, unix: u64) -> Error {
        self.reset = Some(unix);
        self
    }

    /// The same error with `tried`.
    #[must_use]
    pub fn with_tried(mut self, tried: Vec<Tried>) -> Error {
        self.tried = tried;
        self
    }

    /// `self.kind.exit()`.
    #[must_use]
    pub fn exit(&self) -> u8 {
        self.kind.exit()
    }

    /// `self.kind.retriable()`.
    #[must_use]
    pub fn retriable(&self) -> bool {
        self.kind.retriable()
    }

    /// The first stderr line: `agh: {kind} exit={exit} verb={verb}`, then `repo=`, `as=`,
    /// `http="{status} {method} {path}"`, `req=`, `sub=`, `reset=` and `retry_after=` when
    /// present, `side_effect=`, `accepted="…"`, `scopes="…"` and `accepted_scopes="…"` when
    /// present, and `why="…" next="…"`. No trailing newline.
    ///
    /// Each value passes `redactor.apply` raw, before it is quoted, so a learned value that
    /// holds `"` or `\` is still found, and a quoted value keeps its closing quote. `req=`
    /// keeps only the request id's `A-Z a-z 0-9 :`, so a header GitHub sent cannot add a field.
    #[must_use]
    pub fn line(&self, verb: &str, redactor: &Redactor) -> String {
        let clean = |text: &str| redactor.apply(text);
        let quoted = |text: &str| quote(&redactor.apply(text));
        let verb = if verb.is_empty() {
            "-".to_string()
        } else {
            clean(verb)
        };
        let mut parts = vec![
            format!("agh: {}", self.kind.slug()),
            format!("exit={}", self.exit()),
            format!("verb={verb}"),
        ];
        if let Some(repo) = &self.repo {
            parts.push(format!("repo={}", clean(repo)));
        }
        if let Some(identity) = &self.identity {
            parts.push(format!("as={}", clean(identity.label())));
        }
        if let Some(http) = &self.http {
            parts.push(format!(
                "http={}",
                quoted(&format!("{} {} {}", http.status, http.method, http.path))
            ));
            if let Some(id) = &http.request_id {
                let id: String = clean(id)
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric() || *c == ':')
                    .collect();
                parts.push(format!("req={id}"));
            }
        }
        if let Some(sub) = self.sub.slug() {
            parts.push(format!("sub={}", clean(&sub)));
        }
        if let Some(reset) = self.reset {
            parts.push(format!("reset={reset}"));
        }
        if let Some(secs) = self.retry_after {
            parts.push(format!("retry_after={secs}"));
        }
        parts.push(format!("side_effect={}", self.side_effect.slug()));
        if let Some(github) = &self.github {
            if let Some(accepted) = &github.accepted_permissions {
                parts.push(format!("accepted={}", quoted(accepted)));
            }
            if let Some(scopes) = &github.oauth_scopes {
                parts.push(format!("scopes={}", quoted(scopes)));
            }
            if let Some(scopes) = &github.accepted_oauth_scopes {
                parts.push(format!("accepted_scopes={}", quoted(scopes)));
            }
        }
        parts.push(format!("why={}", quoted(&self.why)));
        parts.push(format!("next={}", quoted(&self.next)));
        parts.join(" ")
    }

    /// The second stderr line's object, `{"error": {...}}` with its 19 keys (design §8.5),
    /// every string through `redactor.apply`.
    #[must_use]
    pub fn to_json(&self, redactor: &Redactor) -> Value {
        let text = |s: &str| Value::String(redactor.apply(s));
        let opt_text = |s: Option<&str>| s.map_or(Value::Null, text);
        let github = self.github.as_ref();
        let errors = github
            .map(|g| {
                g.errors
                    .iter()
                    .map(|e| {
                        let mut item = Map::new();
                        item.insert("code".into(), opt_text(e.code.as_deref()));
                        item.insert("field".into(), opt_text(e.field.as_deref()));
                        item.insert("message".into(), opt_text(e.message.as_deref()));
                        item.insert("resource".into(), opt_text(e.resource.as_deref()));
                        Value::Object(item)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let tried = self
            .tried
            .iter()
            .map(|step| {
                let mut item = Map::new();
                match step {
                    Tried::Sent { as_, status } => {
                        item.insert("as".into(), text(as_));
                        item.insert("status".into(), Value::from(*status));
                    }
                    Tried::Probe { path, status } => {
                        item.insert("probe".into(), text(path));
                        item.insert("status".into(), Value::from(*status));
                    }
                    Tried::Skipped { as_, why } => {
                        item.insert("as".into(), text(as_));
                        item.insert("why".into(), text(why));
                    }
                }
                Value::Object(item)
            })
            .collect::<Vec<_>>();
        let mut object = Map::new();
        object.insert(
            "as".into(),
            opt_text(self.identity.as_ref().map(IdentityRef::label)),
        );
        object.insert(
            "documentation_url".into(),
            opt_text(github.and_then(|g| g.documentation_url.as_deref())),
        );
        object.insert("errors".into(), Value::Array(errors));
        object.insert("exit".into(), Value::from(self.exit()));
        object.insert("kind".into(), Value::from(self.kind.slug()));
        object.insert(
            "message".into(),
            opt_text(github.and_then(|g| g.message.as_deref())),
        );
        object.insert("next".into(), text(&self.next));
        // The resource is the raw `x-ratelimit-resource` header, so it is redacted too.
        object.insert(
            "rate".into(),
            self.rate.as_ref().map_or(Value::Null, |rate| {
                let mut value = rate.to_json();
                if let Value::Object(fields) = &mut value {
                    fields.insert("resource".into(), text(&rate.resource));
                }
                value
            }),
        );
        object.insert("repo".into(), opt_text(self.repo.as_deref()));
        object.insert(
            "request_id".into(),
            opt_text(self.http.as_ref().and_then(|h| h.request_id.as_deref())),
        );
        object.insert("retriable".into(), Value::Bool(self.retriable()));
        object.insert(
            "retry_after".into(),
            self.retry_after.map_or(Value::Null, Value::from),
        );
        object.insert("reset".into(), self.reset.map_or(Value::Null, Value::from));
        object.insert("side_effect".into(), Value::from(self.side_effect.slug()));
        object.insert(
            "status".into(),
            self.http
                .as_ref()
                .map_or(Value::Null, |h| Value::from(h.status)),
        );
        object.insert(
            "sub".into(),
            self.sub.slug().map_or(Value::Null, |s| text(&s)),
        );
        object.insert("tried".into(), Value::Array(tried));
        object.insert("v".into(), Value::from(1u8));
        object.insert("why".into(), text(&self.why));
        let mut outer = Map::new();
        outer.insert("error".into(), Value::Object(object));
        Value::Object(outer)
    }
}

/// `text` in double quotes with the line's escapes: `\` → `\\`, `"` → `\"`, newline → `\n`,
/// carriage return → `\r`, tab → `\t`, any other byte below 0x20 → `\xHH`.
pub(crate) fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len().saturating_add(2));
    out.push('"');
    push_escaped(&mut out, text);
    out.push('"');
    out
}

/// The line's escapes of `text`, appended to `out`, with no quotes.
pub(crate) fn push_escaped(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => match u8::try_from(c) {
                Ok(b) if b < 0x20 => {
                    out.push_str("\\x");
                    aterm_codec::hex::push_lower(out, &[b]);
                }
                _ => out.push(c),
            },
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&Redactor::shapes(&format!("{}: {}", self.kind, self.why)))
    }
}

impl std::error::Error for Error {}

/// agh's result.
pub type Result<T, E = Error> = std::result::Result<T, E>;
