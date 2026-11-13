// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::Cell;

struct Fake {
    now: Cell<Instant>,
    events: Vec<&'static str>,
    stall: bool,
    stall_service: bool,
    no_pin: bool,
    late_admin: bool,
    authority: Value,
}

impl Fake {
    fn new() -> Self {
        Self {
            now: Cell::new(Instant::now()),
            events: Vec::new(),
            stall: false,
            stall_service: false,
            no_pin: false,
            late_admin: false,
            authority: json!({"schema":1,"kind":"vault-admin-capability", "vault":"team",
                "signer":"pin", "vault_admin":{"state":"known","value":true}}),
        }
    }
}
impl Source for Fake {
    fn now(&self) -> Instant {
        self.now.get()
    }
    fn local(&mut self, until: Instant) -> Value {
        self.events.push("local");
        if self.stall {
            self.now.set(until + BUDGET);
        }
        if self.no_pin {
            return json!({"vault":{"state":"unknown"}});
        }
        json!({"vault":{"state":"known","value":{"vault":"team","configured_signer":"pin"}}})
    }
    fn pending(&mut self, _: Instant) -> Value {
        self.events.push("pending");
        json!({"state":"known","value":[{"code":"123456","recipient":"age1pending","host":"new-mac"}]})
    }
    fn versions(&mut self, _: Instant) -> Value {
        self.events.push("versions");
        json!({})
    }
    fn services(&mut self, until: Instant) -> Value {
        self.events.push("services");
        if self.stall_service {
            self.now.set(until);
        }
        json!({})
    }
    fn candidates(&mut self) -> Result<Vec<String>, String> {
        self.events.push("candidates");
        Ok((0..MAX_PEERS + 1).map(|n| format!("node{n}")).collect())
    }
    fn authority(&mut self, node: &str, _: Instant) -> Result<Value, String> {
        self.events.push("authority");
        let mut reply = self.authority.clone();
        if self.late_admin {
            reply.as_object_mut().unwrap().insert(
                "vault_admin".into(),
                json!({"state":"known","value":node == "node4"}),
            );
        }
        Ok(reply)
    }
}

#[test]
fn one_total_budget_discards_late_answers_and_stops_further_effects() {
    let mut source = Fake::new();
    source.stall = true;
    let report = capture(&mut source);
    assert_eq!(source.events, ["local"]);
    assert_eq!(report["local"]["state"].as_str(), Some("unknown"));
    assert_eq!(report["pending"]["state"].as_str(), Some("unknown"));
    assert!(report["local"].get("value").is_none());
}

#[test]
fn authorities_need_current_transport_and_matching_independent_pins() {
    let mut source = Fake::new();
    let report = capture(&mut source);
    assert_eq!(report["authorities"].as_array().unwrap().len(), MAX_PEERS);
    assert_eq!(
        report["authority_discovery"]["candidates_truncated"].as_bool(),
        Some(true)
    );
    assert_eq!(
        report["authorities"][0]["capability"]["value"].as_bool(),
        Some(true)
    );
    for (field, changed) in [
        ("vault", json!("other")),
        ("signer", json!("self-asserted")),
        ("schema", json!(0)),
    ] {
        let mut reply = source.authority.clone();
        reply.as_object_mut().unwrap().insert(field.into(), changed);
        let result = validate_authority(reply, Some("team"), Some("pin"));
        assert_eq!(result["state"].as_str(), Some("unknown"));
        assert!(result.get("value").is_none());
    }
    let no_pin = validate_authority(source.authority.clone(), Some("team"), None);
    assert_eq!(no_pin["state"].as_str(), Some("unknown"));
    let old = validate_authority(json!({"error":"unknown verb"}), Some("team"), Some("pin"));
    assert_eq!(old["state"].as_str(), Some("unknown"));
    source
        .authority
        .as_object_mut()
        .unwrap()
        .insert("vault_admin".into(), json!({"state":"known","value":false}));
    assert_eq!(
        validate_authority(source.authority.clone(), Some("team"), Some("pin"))["value"].as_bool(),
        Some(false)
    );
    source.authority.as_object_mut().unwrap().insert(
        "vault_admin".into(),
        json!({"state":"unknown","reason":"busy"}),
    );
    let busy = validate_authority(source.authority, Some("team"), Some("pin"));
    assert_eq!(busy["state"].as_str(), Some("unknown"));
    assert!(busy.get("value").is_none());
}

#[test]
fn slow_service_observation_does_not_erase_vault_pins_or_admin_discovery() {
    let mut source = Fake::new();
    source.stall_service = true;
    let report = capture(&mut source);
    assert_eq!(report["services"]["state"].as_str(), Some("unknown"));
    assert_eq!(
        report["local"]["vault"]["value"]["configured_signer"].as_str(),
        Some("pin")
    );
    assert_eq!(
        report["authorities"][0]["capability"]["value"].as_bool(),
        Some(true)
    );
}

#[test]
fn discovery_continues_past_four_fast_non_admins_and_binds_repair_to_pending_identity() {
    let mut source = Fake::new();
    source.late_admin = true;
    let report = capture(&mut source);
    let routes = report["admission_routes"].as_array().unwrap();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0]["authority"].as_str(), Some("node4"));
    assert_eq!(routes[0]["recipient"].as_str(), Some("age1pending"));
    assert_eq!(routes[0]["approval_argv"][6].as_str(), Some("123456"));
    assert_eq!(routes[0]["executed"].as_bool(), Some(false));
    assert!(
        routes
            .iter()
            .all(|r| r["authority"].as_str() != Some("@local"))
    );
}

#[test]
fn missing_scope_skips_every_authority_dial() {
    let mut source = Fake::new();
    source.no_pin = true;
    let report = capture(&mut source);
    assert!(!source.events.contains(&"authority"));
    assert!(!source.events.contains(&"candidates"));
    assert_eq!(report["local_authority"]["state"].as_str(), Some("unknown"));
    assert!(report["admission_routes"].as_array().unwrap().is_empty());
}

#[test]
fn installed_metadata_is_a_version_not_an_executable_path() {
    assert_eq!(
        plist_version("<key>CFBundleShortVersionString</key><string>0.150.0+g123</string>"),
        Some("0.150.0+g123")
    );
    assert!(
        plist_version("<key>CFBundleShortVersionString</key><string>/tmp/executable</string>")
            .is_none()
    );
    assert!(plist_version("bplist00").is_none());
}

/// NEGATIVE CONTROL: `admission_routes` and `capture` without `valid_cli_token` emit
/// `"--help"` in `approval_argv` and interpolate whitespace/flags into `inspect_command`.
#[test]
fn untrusted_candidate_pending_and_control_tokens_are_rejected() {
    let cap = json!({"state":"known","value":true});
    let pending = json!({
        "state":"known",
        "value":[
            {"code":"--help","recipient":"age1pending","host":"h"},
            {"code":"123 456","recipient":"age1pending","host":"h"},
            {"code":"123456","recipient":"-bad","host":"h"},
            {"code":"123456","recipient":"age1valid","host":"h"}
        ]
    });
    let routes = admission_routes(&cap, Some("node1"), &pending);
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0]["recipient"].as_str(), Some("age1valid"));

    assert!(valid_control_token("abc123"));
    assert!(!valid_control_token(""));
    assert!(!valid_control_token("abc\nsend injected"));
    assert!(!valid_control_token("abc\rdef"));
    assert!(!valid_cli_token("--host"));
    assert!(!valid_cli_token("bad host"));
}
