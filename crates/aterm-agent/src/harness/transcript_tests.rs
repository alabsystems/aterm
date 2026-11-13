// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn project(value: Value, shape: Shape) -> Value {
    match (value, shape) {
        (Value::Array(parts), Shape::Seq(element) | Shape::Parts(element)) => {
            Value::Array(parts.into_iter().map(|p| project(p, *element)).collect())
        }
        (_, Shape::Parts(_)) => Value::Null,
        (Value::Object(mut obj), Shape::Object(fields) | Shape::Record(fields)) => Value::Object(
            fields
                .iter()
                .filter_map(|&(name, shape)| {
                    obj.remove(name)
                        .map(|v| (name.to_owned(), project(v, shape)))
                })
                .collect(),
        ),
        (Value::Object(_) | Value::Array(_), _) | (_, Shape::Record(_)) => Value::Null,
        (scalar, _) => scalar,
    }
}

#[test]
fn projections_match_full_values_including_vendor_type_drift_and_duplicates() {
    for value in [
        "null",
        "true",
        "false",
        "0",
        "-1",
        "-0",
        "1.5",
        "1e3",
        "[]",
        "{}",
        r#""text\n\uD83D\uDE80""#,
        r#"{"model":"opus","content":[{"type":"tool_use","input":{"rows":[1,2]}}],"usage":{"input_tokens":42}}"#,
        r#"{"model":"opus","content":[{"type":"tool_result","content":[{"text":"not a prompt"}]},null,12,"scalar",["nested"],{"type":"text","text":"first"},{"type":"text","text":"second"}]}"#,
        r#"{"content":[{"type":"tool_use","id":"toolu_1","name":"Edit","input":{"file_path":"/p/notes.txt","old_string":"seed","new_string":"sprout","file_path":{"wrong":1}}},{"type":"tool_result","tool_use_id":"toolu_1","content":[{"type":"text","text":"done"}]},{"type":"tool_use","id":2,"name":["Bash"],"input":"sed -i"}]}"#,
    ] {
        for text in [
            value.to_owned(),
            format!(r#"{{"type":{value},"payload":{value}}}"#),
            format!(
                r#"{{"payload":{{"type":{value},"role":{value},"cwd":{value},"model":{value},"sandbox_policy":{{"type":{value}}}}}}}"#
            ),
            format!(
                r#"{{"type":"ignored","t\u0079pe":"turn_context","payload":null,"payload":{{"cwd":"/old","c\u0077d":{value},"sandbox_policy":{{"type":"read-only","type":{value}}}}}}}"#
            ),
            format!(
                r#"{{"message":{value},"effort":{value},"cwd":{value},"isApiErrorMessage":{value},"timestamp":{value}}}"#
            ),
            format!(r#"{{"message":{{"model":{value},"usage":{value},"content":{value}}}}}"#),
            format!(
                r#"{{"message":{{"usage":{{"iterations":[{value},{{"type":{value},"input_tokens":1}}],"speed":{value}}}}}}}"#
            ),
            format!(r#"{{"message":{{"usage":{{"iterations":{value}}}}}}}"#),
            format!(
                r#"{{"payload":{{"forked_from_id":{value},"service_tier":{value},"info":{value}}}}}"#
            ),
            format!(
                r#"{{"type":"response_item","payload":{{"type":"function_call","call_id":{value},"name":{value},"namespace":{value},"arguments":{value},"turn_id":{value},"cli_version":{value},"effort":{value},"duration_ms":{value},"error":{value}}}}}"#
            ),
            format!(
                r#"{{"payload":{{"type":"task_complete","error":{{"message":{value},"codex_error_info":{value},"codex_error_info":"usage_limit_exceeded"}},"last_agent_message":{value}}}}}"#
            ),
            format!(
                r#"{{"payload":{{"info":{{"total_token_usage":{value},"last_token_usage":{{"input_tokens":{value},"total_tokens":3,"total_tokens":{value}}},"rate_limits":{value}}}}}}}"#
            ),
            format!(
                r#"{{"payload":{{"type":{value},"id":{value},"thread_id":{value},"history_mode":{value},"creator_account_id":{value},"creator_user_id":"u","rate_limits":{{"limit_id":{value},"primary":{{"used_percent":{value},"resets_at":3,"resets_at":{value}}},"secondary":{value},"credits":{{"balance":{value}}},"individual_limit":{value},"normal_model_slug":{value}}}}}}}"#
            ),
            format!(
                r#"{{"type":"ignored","t\u0079pe":"assistant","message":null,"message":{{"model":"opus","usage":{{"input_tokens":3,"input_tokens":{value}}}}}}}"#
            ),
        ] {
            let full: Value = aterm_json::from_str(&text).expect("fixture");
            assert_eq!(usage(&text).expect("usage"), project(full.clone(), USAGE));
            assert_eq!(
                metadata(&text).expect("metadata"),
                project(full.clone(), METADATA)
            );
            assert_eq!(
                command(&text).expect("command"),
                project(full.clone(), COMMAND)
            );
            assert_eq!(
                conversation(&text).expect("conversation"),
                project(full.clone(), CONVERSATION)
            );
            assert_eq!(calls(&text).expect("calls"), project(full.clone(), CALLS));
            assert_eq!(steps(&text).expect("steps"), project(full.clone(), STEPS));
            assert_eq!(
                steps_bare(&text).expect("bare steps"),
                project(full.clone(), STEPS_BARE)
            );
            assert_eq!(
                exchange(&text).expect("exchange"),
                project(full.clone(), EXCHANGE)
            );
            assert_eq!(
                excerpt(&text).expect("excerpt"),
                project(full.clone(), EXCERPT)
            );
            assert_eq!(
                codex_spend(&text).expect("Codex spend"),
                project(full.clone(), CODEX_SPEND)
            );
            assert_eq!(
                codex_meta(&text).expect("Codex meta"),
                project(full.clone(), CODEX_META)
            );
            assert_eq!(
                codex_limits(&text).expect("Codex limits"),
                project(full.clone(), CODEX_LIMITS)
            );
            assert_eq!(
                codex_steps(&text).expect("Codex steps"),
                project(full, CODEX_STEPS)
            );
        }
    }
}

#[test]
fn discarded_fields_are_still_fully_validated() {
    for bad in [
        r#""\uD800""#,
        r#""\x""#,
        "[1,]",
        r#"{"a":1,}"#,
        "01",
        "1e9999",
        "true false",
    ] {
        for name in ["unknown", "message"] {
            let text = format!(r#"{{"type":"assistant","{name}":{bad}}}"#);
            assert!(usage(&text).is_err(), "{text}");
            assert!(metadata(&text).is_err(), "{text}");
            assert!(conversation(&text).is_err(), "{text}");
            assert!(excerpt(&text).is_err(), "{text}");
            assert!(exchange(&text).is_err(), "{text}");
            assert!(calls(&text).is_err(), "{text}");
            assert!(steps(&text).is_err(), "{text}");
            assert!(steps_bare(&text).is_err(), "{text}");
            assert!(codex_spend(&text).is_err(), "{text}");
            assert!(codex_meta(&text).is_err(), "{text}");
            assert!(codex_limits(&text).is_err(), "{text}");
            assert!(codex_steps(&text).is_err(), "{text}");
        }
    }
    let deep = format!("{}0{}", "[".repeat(130), "]".repeat(130));
    assert!(metadata(&format!(r#"{{"unknown":{deep}}}"#)).is_err());
}
