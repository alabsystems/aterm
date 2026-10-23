// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stub's Gemini API.

use std::io;
use std::time::Duration;

use super::{Out, Stub, ToolTurn, Turn, analyse, bump, call_input, call_name, closed};
use super::{est_tokens, limit_params, rand_hex, script_calls, spec_headers, split_deltas};
use crate::pyjson::{COMPACT, J};
use crate::util::one_line;

/// The fixed fake key the lab gives Gemini CLI and agy: never a credential anywhere.
pub const LAB_GEMINI_KEY: &str = "lab-gemini-not-a-key";

/// agy echoes it back with the call's result, Gemini CLI stores it.
const SIGNATURE: &str = "bGFiLXN0dWItdGhvdWdodC1zaWduYXR1cmU=";

/// What a Gemini API request asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verb {
    /// `:streamGenerateContent`: SSE.
    Stream,
    /// `:generateContent`: one JSON answer.
    Generate,
    /// `:countTokens`.
    CountTokens,
}

/// The model and the verb of a `POST /v1beta/models/<model>:<verb>` (`/v1/` too).
#[must_use]
pub fn route(method: &str, path: &str) -> Option<(String, Verb)> {
    if method != "POST" {
        return None;
    }
    let rest = path
        .strip_prefix("/v1beta/models/")
        .or_else(|| path.strip_prefix("/v1/models/"))?;
    let (model, verb) = rest.rsplit_once(':')?;
    if model.is_empty() || model.contains('/') {
        return None;
    }
    let verb = match verb {
        "streamGenerateContent" => Verb::Stream,
        "generateContent" => Verb::Generate,
        "countTokens" => Verb::CountTokens,
        _ => return None,
    };
    Some((model.to_string(), verb))
}

/// The lab's label for the credentials a Gemini API request carried, never their values.
#[must_use]
pub fn credential_label(api_key: Option<&str>, authorization: Option<&str>) -> String {
    match (api_key, authorization) {
        (None, None) => "none".into(),
        (Some(one), None) | (None, Some(one)) => super::auth_label(Some(one)),
        (Some(key), Some(bearer)) => format!(
            "{}+{}",
            super::auth_label(Some(key)),
            super::auth_label(Some(bearer))
        ),
    }
}

/// What one Gemini API request is, read off its body.
#[derive(Clone, Debug)]
pub struct GInfo {
    /// How many contents it holds.
    pub contents: usize,
    /// How many tools it offers (each function declaration one).
    pub tools: usize,
    /// The system instruction's length in characters.
    pub sys_chars: usize,
    /// The functionResponse parts it carries back (a tool's results).
    pub results: usize,
    /// The last user text (agy's `<USER_REQUEST>` taken out of its wrapping).
    pub last_user: String,
    /// `gemini` (a turn), `gemini-json`, `gemini-aux` or `opaque`.
    pub kind: &'static str,
    /// A structured side request's schema.
    pub schema: Option<J>,
}

/// The text parts of a content's `parts`, joined by newlines.
fn parts_text(content: &J) -> String {
    content
        .get("parts")
        .and_then(J::as_arr)
        .unwrap_or_default()
        .iter()
        .filter(|p| !p.get("thought").is_some_and(J::truthy))
        .filter_map(|p| p.get("text").and_then(J::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

/// How many `functionResponse` parts a content holds.
fn results_in(content: &J) -> usize {
    content
        .get("parts")
        .and_then(J::as_arr)
        .unwrap_or_default()
        .iter()
        .filter(|p| p.get("functionResponse").is_some())
        .count()
}

/// Whether a Gemini API request's conversation carries a function's result back.
#[must_use]
pub fn results_answered(body: &[u8]) -> bool {
    let Ok(j) = J::parse_bytes(body) else {
        return false;
    };
    j.get("contents")
        .and_then(J::as_arr)
        .unwrap_or_default()
        .iter()
        .any(|c| results_in(c) > 0)
}

/// The text between `<USER_REQUEST>` and `</USER_REQUEST>`, else the text itself.
#[must_use]
pub fn unwrap_request(text: &str) -> String {
    text.split_once("<USER_REQUEST>")
        .and_then(|(_, rest)| rest.split_once("</USER_REQUEST>"))
        .map_or(text, |(inner, _)| inner)
        .trim()
        .to_string()
}

/// The user texts of a Gemini API request, oldest first.
#[must_use]
pub fn gemini_users(body: &[u8]) -> Vec<String> {
    let Ok(j) = J::parse_bytes(body) else {
        return Vec::new();
    };
    j.get("contents")
        .and_then(J::as_arr)
        .unwrap_or_default()
        .iter()
        .filter(|c| c.get("role").and_then(J::as_str) == Some("user"))
        .map(parts_text)
        .filter(|t| !t.trim().is_empty())
        .map(|t| unwrap_request(&t))
        .collect()
}

/// [`GInfo`] of a request body (best effort, never fails).
#[must_use]
pub fn analyse_gemini(body: &[u8]) -> GInfo {
    let mut info = GInfo {
        contents: 0,
        tools: 0,
        sys_chars: 0,
        results: 0,
        last_user: String::new(),
        kind: "gemini",
        schema: None,
    };
    let Ok(j @ J::Obj(_)) = J::parse_bytes(body) else {
        info.kind = "opaque";
        return info;
    };
    let contents = j.get("contents").and_then(J::as_arr).unwrap_or_default();
    info.contents = contents.len();
    info.results = contents.iter().map(results_in).sum();
    info.tools = j
        .get("tools")
        .and_then(J::as_arr)
        .unwrap_or_default()
        .iter()
        .map(|t| {
            t.get("functionDeclarations")
                .and_then(J::as_arr)
                .map_or(1, <[J]>::len)
        })
        .sum();
    info.sys_chars = j
        .get("systemInstruction")
        .map_or(0, |s| parts_text(s).chars().count());
    info.last_user = contents
        .iter()
        .rev()
        .filter(|c| c.get("role").and_then(J::as_str) == Some("user"))
        .map(parts_text)
        .find(|t| !t.is_empty())
        .map(|t| unwrap_request(&t))
        .unwrap_or_default();
    let config = j.get("generationConfig");
    info.schema = config
        .and_then(|g| {
            g.get("responseJsonSchema")
                .or_else(|| g.get("responseSchema"))
        })
        .cloned();
    let json_mode = info.schema.is_some()
        || config
            .and_then(|g| g.get("responseMimeType"))
            .and_then(J::as_str)
            == Some("application/json");
    info.kind = if json_mode {
        "gemini-json"
    } else if info.tools == 0 {
        "gemini-aux"
    } else {
        "gemini"
    };
    info
}

/// What the `tool` mode answers a turn.
#[must_use]
pub fn tool_turn(body: &[u8], script: &J) -> Option<ToolTurn> {
    let j = J::parse_bytes(body).ok()?;
    let calls: Vec<&J> = script_calls(script)
        .into_iter()
        .filter(|c| !c.get("when").is_some_and(J::truthy))
        .collect();
    let mut step: Option<usize> = None;
    for c in j.get("contents").and_then(J::as_arr).unwrap_or_default() {
        let n = results_in(c);
        if n > 0 {
            step = step.map(|s| s + n);
        }
        if c.get("role").and_then(J::as_str) == Some("user") && parts_text(c).contains("toolstart")
        {
            step = Some(0);
        }
    }
    let step = step?;
    Some(match calls.get(step) {
        Some(c) => ToolTurn::Call(call_name(c), call_input(c), step),
        None => ToolTurn::Done,
    })
}

/// A HELD REPLY of the `tool` mode.
#[must_use]
pub fn held_reply(body: &[u8], script: &J) -> Option<(String, f64)> {
    let j = J::parse_bytes(body).ok()?;
    let first = j
        .get("contents")
        .and_then(J::as_arr)
        .unwrap_or_default()
        .iter()
        .find(|c| c.get("role").and_then(J::as_str) == Some("user"))
        .map(parts_text)?;
    let steps: Vec<&J> = match script {
        J::Arr(a) => a.iter().collect(),
        one => vec![one],
    };
    steps.into_iter().find_map(|step| {
        if step.get("name").is_some_and(J::truthy) {
            return None;
        }
        let when = step
            .get("when")
            .and_then(J::as_str)
            .filter(|w| !w.is_empty())?;
        let text = step.get("text").and_then(J::as_str)?;
        first.contains(when).then(|| {
            (
                text.to_string(),
                step.get("delay_ms")
                    .and_then(J::as_f64)
                    .unwrap_or(0.0)
                    .max(0.0),
            )
        })
    })
}

/// A value that fits `schema` (Gemini's `responseSchema` or a JSON schema).
#[must_use]
pub fn schema_fill(schema: &J, name: Option<&str>) -> J {
    match name {
        Some("complexity_score") => return J::Int(10),
        Some("complexity_reasoning") => return J::s("lab stub: a simple request"),
        Some("next_speaker") => return J::s("user"),
        Some("model_choice") => return J::s("flash"),
        _ => {}
    }
    if let Some(first) = schema
        .get("enum")
        .and_then(J::as_arr)
        .and_then(<[J]>::first)
    {
        return first.clone();
    }
    let ty = match schema.get("type") {
        Some(J::Str(t)) => t.to_ascii_lowercase(),
        Some(J::Arr(a)) => a
            .first()
            .and_then(J::as_str)
            .unwrap_or("object")
            .to_ascii_lowercase(),
        _ => "object".into(),
    };
    match ty.as_str() {
        "object" => {
            let Some(J::Obj(props)) = schema.get("properties") else {
                return J::Obj(Vec::new());
            };
            let required: Vec<&str> = schema
                .get("required")
                .and_then(J::as_arr)
                .map(|r| r.iter().filter_map(J::as_str).collect())
                .unwrap_or_else(|| props.iter().map(|(k, _)| k.as_str()).collect());
            J::Obj(
                props
                    .iter()
                    .filter(|(k, _)| required.contains(&k.as_str()))
                    .map(|(k, v)| (k.clone(), schema_fill(v, Some(k))))
                    .collect(),
            )
        }
        "array" => J::Arr(Vec::new()),
        "integer" | "number" => J::Int(0),
        "boolean" => J::Bool(false),
        _ => {
            let max = schema
                .get("maxLength")
                .and_then(J::as_i64)
                .and_then(|n| usize::try_from(n).ok())
                .filter(|n| *n > 0)
                .unwrap_or(8);
            J::s("lab stub".chars().take(max).collect::<String>())
        }
    }
}

/// Google's error body: `{"error": {"code", "message", "status", "details"}}`.
#[must_use]
pub fn google_error(code: u16, status: &str, message: &str, details: Vec<J>) -> J {
    let mut e = J::obj([
        ("code", J::Int(i64::from(code))),
        ("message", J::s(message)),
        ("status", J::s(status)),
    ]);
    if !details.is_empty() {
        e.set("details", J::Arr(details));
    }
    J::obj([("error", e)])
}

/// The 429 `RESOURCE_EXHAUSTED` of a spent per-day quota on `model`.
#[must_use]
pub fn quota_error(model: &str, retry_secs: i64) -> J {
    quota_error_for(
        model,
        "GenerateRequestsPerDayPerProjectPerModel-FreeTier",
        250,
        &format!("{}s", retry_secs.max(0)),
    )
}

/// [`quota_error`] for the quota `quota_id` of `limit` requests.
#[must_use]
pub fn quota_error_for(model: &str, quota_id: &str, limit: u32, retry_delay: &str) -> J {
    let metric = "generativelanguage.googleapis.com/generate_content_free_tier_requests";
    google_error(
        429,
        "RESOURCE_EXHAUSTED",
        &format!(
            "You exceeded your current quota, please check your plan and billing details. Quota exceeded for \
             metric: {metric}, limit: {limit}, model: {model}"
        ),
        vec![
            J::obj([
                ("@type", J::s("type.googleapis.com/google.rpc.QuotaFailure")),
                (
                    "violations",
                    J::Arr(vec![J::obj([
                        ("quotaMetric", J::s(metric)),
                        ("quotaId", J::s(quota_id)),
                        (
                            "quotaDimensions",
                            J::obj([("location", J::s("global")), ("model", J::s(model))]),
                        ),
                        ("quotaValue", J::s(limit.to_string())),
                    ])]),
                ),
            ]),
            J::obj([
                ("@type", J::s("type.googleapis.com/google.rpc.Help")),
                (
                    "links",
                    J::Arr(vec![J::obj([
                        ("description", J::s("Learn more about Gemini API quotas")),
                        (
                            "url",
                            J::s("https://ai.google.dev/gemini-api/docs/rate-limits"),
                        ),
                    ])]),
                ),
            ]),
            J::obj([
                ("@type", J::s("type.googleapis.com/google.rpc.RetryInfo")),
                ("retryDelay", J::s(retry_delay)),
            ]),
        ],
    )
}

/// The `RetryInfo` of the `rate-limit` mode's spent day quota, in seconds.
pub const DAY_QUOTA_RETRY: i64 = 3600;

/// The claim of `limit:CLAIM:RESET` whose Gemini API answer is a Google ACCOUNT's spent usage.
pub const ACCOUNT_CLAIM: &str = "code_assist";

/// The claim of `limit:CLAIM:RESET` that walls a Google ACCOUNT's PRO
/// bucket under Gemini CLI's Auto model: the turn answered as
/// [`ACCOUNT_CLAIM`]'s ([`exhausted_error`]), and while it stands each
/// structured side request routed to the Pro model ([`route_pro`]), so the
/// turn meets the wall on a Pro model with a flash model to fall back to and
/// Gemini CLI 0.46.0 draws the per-model bucket's dialog (`Switch to
/// <fallback>` over `Stop`). On a model a `--model` named, or on the flash
/// model, the failed model is its own fallback and the CLI draws the
/// account's `Keep trying` instead.
pub const BUCKET_CLAIM: &str = "code_assist_pro";

/// A structured side request's answer `filled` ([`schema_fill`]) routed to
/// the Pro model: Gemini CLI's numerical router reads `complexity_score`
/// against its threshold (at most 100), its classifier `model_choice`
/// (`pro`, the bundle's `PRO_MODEL`); any other answer is left as it is.
#[must_use]
pub fn route_pro(filled: J) -> J {
    match filled {
        J::Obj(props) => J::Obj(
            props
                .into_iter()
                .map(|(k, v)| match k.as_str() {
                    "complexity_score" => (k, J::Int(100)),
                    "model_choice" => (k, J::s("pro")),
                    _ => (k, v),
                })
                .collect(),
        ),
        other => other,
    }
}

/// Whether stub mode `mode` routes the side requests to the Pro model now
/// (`now_s`): a [`BUCKET_CLAIM`] wall not yet reset.
#[must_use]
pub fn routes_pro(mode: &str, now_s: i64) -> bool {
    limit_params(mode).is_some_and(|(claim, reset, _)| claim == BUCKET_CLAIM && now_s < reset)
}

/// The 429 `RESOURCE_EXHAUSTED` Google's Code Assist server answers once the ACCOUNT's usage limit on `model` is spent.
#[must_use]
pub fn exhausted_error(model: &str, retry_secs: i64, reset_s: i64) -> J {
    let secs = retry_secs.max(0);
    let span = format!("{}h{}m{}s", secs / 3600, secs % 3600 / 60, secs % 60);
    let span = span.strip_prefix("0h").unwrap_or(&span).to_string();
    google_error(
        429,
        "RESOURCE_EXHAUSTED",
        &format!(
            "You have exhausted your capacity on this model. Your quota will reset after {span}."
        ),
        vec![
            J::obj([
                ("@type", J::s("type.googleapis.com/google.rpc.ErrorInfo")),
                ("reason", J::s("QUOTA_EXHAUSTED")),
                ("domain", J::s("cloudcode-pa.googleapis.com")),
                (
                    "metadata",
                    J::obj([
                        ("uiMessage", J::s("true")),
                        ("model", J::s(model)),
                        ("quotaResetDelay", J::s(span.as_str())),
                        (
                            "quotaResetTimeStamp",
                            J::s(format!("{}Z", crate::util::utc_civil(reset_s))),
                        ),
                    ]),
                ),
            ]),
            J::obj([
                ("@type", J::s("type.googleapis.com/google.rpc.RetryInfo")),
                ("retryDelay", J::s(format!("{secs}s"))),
            ]),
        ],
    )
}

/// The wall a fixed mode puts on a turn, in Google's shape.
#[must_use]
pub fn mode_error(mode: &str, model: &str) -> Option<(u16, J)> {
    Some(match mode {
        "rate-limit" => (429, quota_error(model, DAY_QUOTA_RETRY)),
        "overloaded" => (
            503,
            google_error(
                503,
                "UNAVAILABLE",
                "The model is overloaded. Please try again later.",
                Vec::new(),
            ),
        ),
        "500" => (
            500,
            google_error(
                500,
                "INTERNAL",
                "An internal error has occurred. Please retry or report in \
                 https://developers.generativeai.google/guide/troubleshooting",
                Vec::new(),
            ),
        ),
        "401" => (
            401,
            google_error(
                401,
                "UNAUTHENTICATED",
                "Request had invalid authentication credentials. Expected OAuth 2 access token, login cookie \
                 or other valid authentication credential. See \
                 https://developers.google.com/identity/sign-in/web/devconsole-project.",
                Vec::new(),
            ),
        ),
        "bad-key" => (
            400,
            google_error(
                400,
                "INVALID_ARGUMENT",
                "API key not valid. Please pass a valid API key.",
                vec![
                    J::obj([
                        ("@type", J::s("type.googleapis.com/google.rpc.ErrorInfo")),
                        ("reason", J::s("API_KEY_INVALID")),
                        ("domain", J::s("googleapis.com")),
                        (
                            "metadata",
                            J::obj([("service", J::s("generativelanguage.googleapis.com"))]),
                        ),
                    ]),
                    J::obj([
                        (
                            "@type",
                            J::s("type.googleapis.com/google.rpc.LocalizedMessage"),
                        ),
                        ("locale", J::s("en-US")),
                        (
                            "message",
                            J::s("API key not valid. Please pass a valid API key."),
                        ),
                    ]),
                ],
            ),
        ),
        "too-long" => (
            400,
            google_error(
                400,
                "INVALID_ARGUMENT",
                "The input token count (1048577) exceeds the maximum number of tokens allowed (1048576).",
                Vec::new(),
            ),
        ),
        _ => return None,
    })
}

/// The Gemini API's other refusals the lab measured Gemini CLI and agy under.
#[must_use]
pub fn measured_refusals() -> Vec<(&'static str, &'static str, J)> {
    let model = "gemini-3.1-pro-preview";
    let spec =
        |status: u16, body: J| J::obj([("status", J::Int(i64::from(status))), ("json", body)]);
    let quota = |per: &str, limit: u32, retry: &str| {
        let id = format!("GenerateRequestsPer{per}PerProjectPerModel-FreeTier");
        quota_error_for(model, &id, limit, retry)
    };
    vec![
        (
            "perm403",
            "403 PERMISSION_DENIED, ErrorInfo reason SERVICE_DISABLED (the Generative Language API not enabled on the key's project)",
            spec(
                403,
                google_error(
                    403,
                    "PERMISSION_DENIED",
                    "Generative Language API has not been used in project 1234567890 before or it is disabled. \
                     Enable it by visiting https://console.developers.google.com/apis/api/\
                     generativelanguage.googleapis.com/overview?project=1234567890 then retry. If you enabled \
                     this API recently, wait a few minutes for the action to propagate to our systems and retry.",
                    vec![J::obj([
                        ("@type", J::s("type.googleapis.com/google.rpc.ErrorInfo")),
                        ("reason", J::s("SERVICE_DISABLED")),
                        ("domain", J::s("googleapis.com")),
                        (
                            "metadata",
                            J::obj([
                                ("consumer", J::s("projects/1234567890")),
                                ("service", J::s("generativelanguage.googleapis.com")),
                            ]),
                        ),
                    ])],
                ),
            ),
        ),
        (
            "denied403",
            "403 PERMISSION_DENIED, `The caller does not have permission`, no details",
            spec(
                403,
                google_error(
                    403,
                    "PERMISSION_DENIED",
                    "The caller does not have permission",
                    Vec::new(),
                ),
            ),
        ),
        (
            "notfound404",
            "404 NOT_FOUND, the turn's model not found for v1beta",
            spec(
                404,
                google_error(
                    404,
                    "NOT_FOUND",
                    &format!(
                        "models/{model} is not found for API version v1beta, or is not supported for \
                         generateContent. Call ListModels to see the list of available models and their \
                         supported methods."
                    ),
                    Vec::new(),
                ),
            ),
        ),
        (
            "location400",
            "400 FAILED_PRECONDITION, `User location is not supported for the API use.`",
            spec(
                400,
                google_error(
                    400,
                    "FAILED_PRECONDITION",
                    "User location is not supported for the API use.",
                    Vec::new(),
                ),
            ),
        ),
        (
            "timeout504",
            "504 DEADLINE_EXCEEDED on every attempt",
            spec(
                504,
                google_error(
                    504,
                    "DEADLINE_EXCEEDED",
                    "The request timed out. Please try again.",
                    Vec::new(),
                ),
            ),
        ),
        (
            "exhausted429",
            "429 RESOURCE_EXHAUSTED, `Resource has been exhausted (e.g. check quota).`, no details (no RetryInfo)",
            spec(
                429,
                google_error(
                    429,
                    "RESOURCE_EXHAUSTED",
                    "Resource has been exhausted (e.g. check quota).",
                    Vec::new(),
                ),
            ),
        ),
        (
            "minute429",
            "429 RESOURCE_EXHAUSTED, QuotaFailure quotaId GenerateRequestsPerMinutePerProjectPerModel-FreeTier, RetryInfo 47.218363629s",
            spec(429, quota("Minute", 5, "47.218363629s")),
        ),
        (
            "day429long",
            "429 RESOURCE_EXHAUSTED, QuotaFailure quotaId GenerateRequestsPerDayPerProjectPerModel-FreeTier, RetryInfo 41397.204181s",
            spec(429, quota("Day", 250, "41397.204181s")),
        ),
    ]
}

/// One chunk of an answer.
#[must_use]
pub fn chunk(parts: Vec<J>, model: &str, rid: &str, usage: Option<(i64, i64)>) -> J {
    let mut cand = J::obj([
        (
            "content",
            J::obj([("role", J::s("model")), ("parts", J::Arr(parts))]),
        ),
        ("index", J::Int(0)),
    ]);
    if usage.is_some() {
        cand.set("finishReason", J::s("STOP"));
    }
    let mut out = J::obj([("candidates", J::Arr(vec![cand]))]);
    if let Some((prompt, cands)) = usage {
        out.set(
            "usageMetadata",
            J::obj([
                ("promptTokenCount", J::Int(prompt)),
                ("candidatesTokenCount", J::Int(cands)),
                ("totalTokenCount", J::Int(prompt + cands)),
            ]),
        );
    }
    out.set("modelVersion", J::s(model));
    out.set("responseId", J::s(rid));
    out
}

/// A text part.
fn text(t: &str) -> J {
    J::obj([("text", J::s(t))])
}

/// The steps of a streamed answer: each its pause before it, and its parts.
type Steps = Vec<(f64, Vec<J>)>;

/// `text` as the deltas of [`split_deltas`], with no pause.
fn text_steps(t: &str) -> Steps {
    split_deltas(t)
        .into_iter()
        .map(|d| (0.0, vec![text(&d)]))
        .collect()
}

/// The `slow` turn: a thought a second for half of `secs`, then a `tick N` of text a second.
#[must_use]
pub fn slow_steps(secs: i64) -> Vec<(f64, Vec<J>)> {
    let n = secs.max(2);
    let think = n / 2;
    (1..=n)
        .map(|i| {
            let part = if i <= think {
                J::obj([
                    ("text", J::s(format!("**Lab stub thinking**\n\ntick {i}"))),
                    ("thought", J::Bool(true)),
                ])
            } else {
                text(&format!("tick {i} "))
            };
            (1.0, vec![part])
        })
        .collect()
}

impl Stub {
    /// A Gemini API request, `label` the credentials it carried.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub(super) fn gemini(
        &self,
        method: &str,
        path: &str,
        (model, verb): (&str, Verb),
        body: &[u8],
        mode: &str,
        label: &str,
        out: &mut Out<'_>,
    ) -> io::Result<()> {
        let blen = body.len();
        if verb == Verb::CountTokens {
            let n = est_tokens(blen);
            let st = out.json(200, &J::obj([("totalTokens", J::Int(n))]), &[])?;
            let s = format!("kind=gemini-count auth={label} tokens={n}");
            self.record(method, path, st, blen, mode, &s);
            return Ok(());
        }
        let info = analyse_gemini(body);
        let kind = info.kind;
        let n = bump(&mut self.st().by_kind, kind);
        let mut summary = format!(
            "kind={kind} auth={label} model={model} stream={} contents={} tools={} sys={}c results={} last_user=\"{}\"",
            verb == Verb::Stream,
            info.contents,
            info.tools,
            info.sys_chars,
            info.results,
            one_line(&info.last_user, 70)
        );
        self.dump(body, kind);
        let in_tok = est_tokens(blen);
        let t = (method, path, blen, mode, model, verb, in_tok);
        if kind != "gemini" {
            let answer = if kind == "gemini-json" {
                let filled = schema_fill(info.schema.as_ref().unwrap_or(&J::Obj(Vec::new())), None);
                if routes_pro(mode, crate::util::now_secs()) {
                    summary.push_str(" routed:pro");
                    route_pro(filled).dump(COMPACT)
                } else {
                    filled.dump(COMPACT)
                }
            } else {
                "Lab stub session".to_string()
            };
            return self.gemini_answer(t, &summary, text_steps(&answer), &[], out);
        }
        if mode == "tool" {
            let script = self
                .args
                .tool_file
                .as_ref()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .and_then(|t| J::parse(&t).ok());
            if let Some((reply, ms)) = script.as_ref().and_then(|s| held_reply(body, s)) {
                std::thread::sleep(Duration::from_secs_f64(ms / 1000.0));
                let s = format!("{summary} tool:held {ms:.0} ms");
                return self.gemini_answer(t, &s, text_steps(&reply), &[], out);
            }
            match script.and_then(|s| tool_turn(body, &s)) {
                Some(ToolTurn::Call(name, input, step)) => {
                    let call = J::obj([
                        (
                            "functionCall",
                            J::obj([("name", J::s(name.as_str())), ("args", input)]),
                        ),
                        ("thoughtSignature", J::s(SIGNATURE)),
                    ]);
                    let s = format!("{summary} tool:{name} step {step}");
                    return self.gemini_answer(t, &s, vec![(0.0, vec![call])], &[], out);
                }
                Some(ToolTurn::Done) => {
                    let s = format!("{summary} tool:done");
                    return self.gemini_answer(t, &s, text_steps("Tool call done."), &[], out);
                }
                Some(ToolTurn::Say(..) | ToolTurn::Calls(..)) | None => {}
            }
        }
        let mut mode = mode.to_string();
        if let Some(spec) = self.take_custom(&mode, Some(label)) {
            let spec = super::with_reply(spec, &gemini_users(body));
            if let Some((name, input)) = super::scripted_call(&spec, results_answered(body)) {
                let call = J::obj([
                    (
                        "functionCall",
                        J::obj([("name", J::s(name.as_str())), ("args", input)]),
                    ),
                    ("thoughtSignature", J::s(SIGNATURE)),
                ]);
                let s = format!("{summary} custom tool:{name}");
                let t = (method, path, blen, mode.as_str(), model, verb, in_tok);
                return self.gemini_answer(t, &s, vec![(0.0, vec![call])], &[], out);
            }
            let status = spec
                .get("status")
                .and_then(J::as_i64)
                .and_then(|s| u16::try_from(s).ok())
                .unwrap_or(200);
            if status == 200 && spec.get("stream").is_none_or(J::truthy) {
                if let Some(ms) = spec
                    .get("delay_ms")
                    .and_then(J::as_f64)
                    .filter(|v| *v > 0.0)
                {
                    std::thread::sleep(Duration::from_secs_f64(ms / 1000.0));
                }
                let reply = spec
                    .get("text")
                    .and_then(J::as_str)
                    .map_or_else(|| format!("Hi! (custom mode {mode})"), str::to_string);
                let pace = spec.get("pace_s").and_then(J::as_f64).unwrap_or(0.0);
                let steps = split_deltas(&reply)
                    .into_iter()
                    .map(|d| (pace, vec![text(&d)]))
                    .collect();
                let s = format!("{summary} custom");
                let t = (method, path, blen, mode.as_str(), model, verb, in_tok);
                return self.gemini_answer(t, &s, steps, &spec_headers(&spec), out);
            }
            let msg_info = analyse(b"");
            let ct = Turn {
                method,
                path,
                blen,
                mode: &mode,
                info: &msg_info,
            };
            return self.custom(&ct, &spec, &summary, out);
        }
        if self.custom_spec(&mode).is_some() {
            mode = "ok".into();
        }
        if mode.split(':').next() == Some("limit") {
            let Some((claim, reset, _plan)) = limit_params(&mode) else {
                let msg = format!("lab stub: mode {mode} is not limit:CLAIM:RESET");
                let st = out.json(
                    400,
                    &google_error(400, "INVALID_ARGUMENT", &msg, Vec::new()),
                    &[],
                )?;
                self.record(method, path, st, blen, &mode, &summary);
                return Ok(());
            };
            let now_s = crate::util::now_secs();
            if now_s < reset {
                let body = if claim == ACCOUNT_CLAIM || claim == BUCKET_CLAIM {
                    exhausted_error(model, reset - now_s, reset)
                } else {
                    quota_error(model, reset - now_s)
                };
                let st = out.json(429, &body, &[])?;
                let s = format!("{summary} limit:{claim} resets in {}s", reset - now_s);
                self.record(method, path, st, blen, &mode, &s);
                return Ok(());
            }
            summary.push_str(" limit lifted (reset passed)");
            mode = "ok".into();
        }
        if let Some((status, err)) = mode_error(&mode, model) {
            let st = out.json(status, &err, &[])?;
            self.record(method, path, st, blen, &mode, &summary);
            return Ok(());
        }
        let steps = if mode == "slow" {
            slow_steps(self.args.slow_seconds)
        } else {
            text_steps(&self.reply_text("main", &mode, n))
        };
        let t = (method, path, blen, mode.as_str(), model, verb, in_tok);
        self.gemini_answer(t, &summary, steps, &[], out)
    }

    /// last one finishing), or merged into one JSON answer for `generateContent`.
    #[allow(clippy::type_complexity)]
    fn gemini_answer(
        &self,
        (method, path, blen, mode, model, verb, in_tok): (&str, &str, usize, &str, &str, Verb, i64),
        summary: &str,
        steps: Steps,
        extra: &[(String, String)],
        out: &mut Out<'_>,
    ) -> io::Result<()> {
        let rid = format!("lab{}", rand_hex());
        let out_tok = i64::try_from(
            steps
                .iter()
                .flat_map(|(_, parts)| parts.iter())
                .filter_map(|p| p.get("text").and_then(J::as_str))
                .map(str::len)
                .sum::<usize>()
                / 4,
        )
        .unwrap_or(0)
        .max(1);
        if verb != Verb::Stream {
            let parts: Vec<J> = steps.into_iter().flat_map(|(_, p)| p).collect();
            let st = out.json(
                200,
                &chunk(parts, model, &rid, Some((in_tok, out_tok))),
                extra,
            )?;
            self.record(method, path, st, blen, mode, &format!("{summary} json"));
            return Ok(());
        }
        out.stream_head(extra)?;
        let last = steps.len().saturating_sub(1);
        let sent = (|| -> io::Result<()> {
            for (i, (pause, parts)) in steps.iter().enumerate() {
                if *pause > 0.0 {
                    std::thread::sleep(Duration::from_secs_f64(*pause));
                }
                let usage = (i == last).then_some((in_tok, out_tok));
                let c = chunk(parts.clone(), model, &rid, usage);
                out.chunk(format!("data: {}\r\n\r\n", c.dump(COMPACT)).as_bytes())?;
            }
            out.chunk(b"")
        })();
        let note = match sent {
            Err(e) if closed(&e) => " (client closed mid-stream)",
            Err(e) => return Err(e),
            Ok(()) => "",
        };
        self.record(
            method,
            path,
            200,
            blen,
            mode,
            &format!("{summary} sse={} chunks{note}", steps.len()),
        );
        Ok(())
    }

    /// `GET /v1beta/models[/<model>]`.
    pub(super) fn gemini_models(
        &self,
        method: &str,
        path: &str,
        mode: &str,
        label: &str,
        out: &mut Out<'_>,
    ) -> io::Result<()> {
        let card = |id: &str| {
            J::obj([
                ("name", J::s(format!("models/{id}"))),
                ("baseModelId", J::s(id)),
                ("displayName", J::s(id)),
                ("inputTokenLimit", J::Int(1_048_576)),
                ("outputTokenLimit", J::Int(65_536)),
                (
                    "supportedGenerationMethods",
                    J::Arr(vec![J::s("generateContent"), J::s("countTokens")]),
                ),
                ("thinking", J::Bool(true)),
            ])
        };
        let body = match path
            .strip_prefix("/v1beta/models/")
            .filter(|m| !m.is_empty())
        {
            Some(id) => card(id),
            None => J::obj([(
                "models",
                J::Arr(
                    ["gemini-3.5-flash", "gemini-3.1-pro-preview"]
                        .map(card)
                        .to_vec(),
                ),
            )]),
        };
        let st = out.json(200, &body, &[])?;
        self.record(
            method,
            path,
            st,
            0,
            mode,
            &format!("kind=gemini-models auth={label}"),
        );
        Ok(())
    }
}
