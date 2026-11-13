// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

/// Formats unix milliseconds `ms` as an RFC 3339 rollout timestamp.
pub(crate) fn stamp(ms: i64) -> String {
    let whole = crate::harness::usage::rfc3339_utc(ms.div_euclid(1000));
    format!("{}.{:03}Z", &whole[..19], ms.rem_euclid(1000))
}

/// Builds a `session_meta` JSON line.
pub(crate) fn meta(ms: i64, thread: &str, session: &str, fork: Option<&str>) -> String {
    let fork = fork.map_or_else(String::new, |f| format!(",\"forked_from_id\":\"{f}\""));
    format!(
        "{{\"timestamp\":\"{at}\",\"type\":\"session_meta\",\"payload\":{{\"session_id\":\
         \"{session}\",\"id\":\"{thread}\"{fork},\"timestamp\":\"{at}\",\"cwd\":\
         \"/stand-in/project\",\"originator\":\"codex-tui\",\"cli_version\":\"0.163.0\",\
         \"source\":\"cli\",\"model_provider\":\"openai\",\"base_instructions\":{{\"text\":\
         \"You are Codex (stand-in)\"}}}}}}\n",
        at = stamp(ms),
    )
}

/// Builds a Codex 0.163.0 `session_meta` JSON line with `history_mode` and login IDs.
pub(crate) fn meta_as(
    ms: i64,
    thread: &str,
    session: &str,
    fork: Option<&str>,
    history: Option<&str>,
    creator: Option<(&str, &str)>,
) -> String {
    let mut extra = history.map_or_else(String::new, |h| format!(",\"history_mode\":\"{h}\""));
    if let Some((account, user)) = creator {
        extra.push_str(&format!(
            ",\"creator_account_id\":\"{account}\",\"creator_user_id\":\"{user}\""
        ));
    }
    meta(ms, thread, session, fork).replacen(
        ",\"timestamp\":",
        &format!("{extra},\"timestamp\":"),
        1,
    )
}

/// Builds a `turn_context` JSON line for `model` and optional `tier`.
pub(crate) fn context(ms: i64, model: &str, tier: Option<&str>) -> String {
    let tier = tier.map_or_else(String::new, |t| format!(",\"service_tier\":\"{t}\""));
    format!(
        "{{\"timestamp\":\"{}\",\"type\":\"turn_context\",\"payload\":{{\"turn_id\":\"t\",\
         \"cwd\":\"/stand-in/project\",\"approval_policy\":\"never\",\"sandbox_policy\":\
         {{\"type\":\"danger-full-access\"}},\"model\":\"{model}\",\"effort\":\"high\"{tier}}}}}\n",
        stamp(ms)
    )
}

/// Tuple `(input_tokens, cached_input_tokens, output_tokens)`.
pub(crate) type Counts = (u64, u64, u64);

fn usage((input, cached, output): Counts) -> String {
    format!(
        "{{\"input_tokens\":{input},\"cached_input_tokens\":{cached},\
         \"cache_write_input_tokens\":0,\"output_tokens\":{output},\
         \"reasoning_output_tokens\":{},\"total_tokens\":{}}}",
        output / 2,
        input + output
    )
}

/// Builds a `token_count` JSON line with running `total` and `last` turn counts.
pub(crate) fn count(ms: i64, total: Counts, last: Counts) -> String {
    count_with(
        ms,
        total,
        last,
        "{\"limit_id\":\"codex\",\"primary\":{\"used_percent\":12.0,\"window_minutes\":300,\
         \"resets_at\":1790000000},\"secondary\":null,\"plan_type\":\"pro\"}",
    )
}

/// Builds a `token_count` JSON line as [`count`] does, with `"rate_limits":null` (a login
/// Codex is told no limits for).
pub(crate) fn count_unlimited(ms: i64, total: Counts, last: Counts) -> String {
    count_with(ms, total, last, "null")
}

fn count_with(ms: i64, total: Counts, last: Counts, rate_limits: &str) -> String {
    format!(
        "{{\"timestamp\":\"{}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\",\
         \"info\":{{\"total_token_usage\":{},\"last_token_usage\":{},\
         \"model_context_window\":272000}},\"rate_limits\":{rate_limits}}}}}\n",
        stamp(ms),
        usage(total),
        usage(last)
    )
}

/// Builds a rate-limits-only `token_count` JSON line (`info: null`).
pub(crate) fn limits_only(ms: i64) -> String {
    format!(
        "{{\"timestamp\":\"{}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\",\
         \"info\":null,\"rate_limits\":{{\"limit_id\":\"codex\",\"primary\":null}}}}}}\n",
        stamp(ms)
    )
}

/// Rate-limit slot `(used_percent, window_minutes, resets_at)`.
pub(crate) type RateSlot = (f64, u64, i64);

/// Builds a rate-limits-only `token_count` JSON line for `family` with two slots.
pub(crate) fn rate(
    ms: i64,
    family: &str,
    primary: Option<RateSlot>,
    secondary: Option<RateSlot>,
) -> String {
    let slot = |s: Option<RateSlot>| {
        s.map_or_else(
            || "null".to_string(),
            |(used, minutes, resets)| {
                format!(
                    "{{\"used_percent\":{used:.1},\"window_minutes\":{minutes},\
                     \"resets_at\":{resets}}}"
                )
            },
        )
    };
    format!(
        "{{\"timestamp\":\"{}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\",\
         \"info\":null,\"rate_limits\":{{\"limit_id\":\"{family}\",\"limit_name\":null,\
         \"primary\":{},\"secondary\":{},\"credits\":{{\"has_credits\":true,\
         \"unlimited\":false,\"balance\":\"12\"}},\"individual_limit\":null,\
         \"spend_control_reached\":null,\"plan_type\":null,\"rate_limit_reached_type\":null}}}}}}\n",
        stamp(ms),
        slot(primary),
        slot(secondary),
    )
}

/// `line` (a [`rate`]) with each slot given the reset `0` naming none.
pub(crate) fn no_reset(line: &str) -> String {
    line.replace("\"resets_at\":0}", "\"resets_at\":null}")
}

/// `line` (a [`rate`]) with the server's refusal in it.
pub(crate) fn refused(line: &str) -> String {
    line.replace(
        "\"rate_limit_reached_type\":null",
        "\"rate_limit_reached_type\":\"workspace_member_usage_limit_reached\"",
    )
}

/// Builds a model message rollout JSON line.
pub(crate) fn message(ms: i64) -> String {
    format!(
        "{{\"timestamp\":\"{}\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\
         \"role\":\"assistant\",\"content\":[{{\"type\":\"output_text\",\"text\":\"done\"}}]}}}}\n",
        stamp(ms)
    )
}

/// Builds a `thread_settings_applied` JSON line for `thread`.
pub(crate) fn settings(ms: i64, thread: &str) -> String {
    format!(
        "{{\"timestamp\":\"{}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\
         \"thread_settings_applied\",\"thread_id\":\"{thread}\",\"thread_settings\":\
         {{\"model\":\"gpt-6.1-sol\",\"model_provider_id\":\"openai\",\
         \"approval_policy\":\"on-request\"}}}}}}\n",
        stamp(ms)
    )
}

/// Restamps `lines` in a 1-ms burst starting at `ms` to simulate a fork's copied prefix.
pub(crate) fn copied(lines: &str, ms: i64) -> String {
    lines
        .lines()
        .enumerate()
        .map(|(i, line)| {
            let at = line
                .find("\"timestamp\":\"")
                .map(|s| s + "\"timestamp\":\"".len())
                .expect("a stamped line");
            let i = i64::try_from(i).expect("few lines");
            format!("{}{}{}\n", &line[..at], stamp(ms + i), &line[at + 24..])
        })
        .collect()
}
