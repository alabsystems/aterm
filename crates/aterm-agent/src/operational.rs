// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Early release observations, independent of publication or credential application.

use aterm_json::{Value, json};
use std::time::{Duration, Instant};

const BUDGET: Duration = Duration::from_secs(20);
const PROBE: Duration = Duration::from_secs(3);
const MAX_PEERS: usize = 64;
const MAX_PENDING: usize = 32;

fn valid_cli_token(text: &str) -> bool {
    !text.is_empty()
        && !text.starts_with('-')
        && !text.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn unknown(reason: impl Into<String>) -> Value {
    json!({"state":"unknown", "reason":reason.into()})
}

/// The observation's clock and effects share absolute deadlines. A late result is
/// discarded, and no subsequent effect starts once the total deadline expires.
pub trait Source {
    fn now(&self) -> Instant;
    fn local(&mut self, until: Instant) -> Value;
    fn pending(&mut self, until: Instant) -> Value;
    fn versions(&mut self, until: Instant) -> Value;
    fn services(&mut self, until: Instant) -> Value;
    fn candidates(&mut self) -> Result<Vec<String>, String>;
    fn authority(&mut self, node: &str, until: Instant) -> Result<Value, String>;
}

fn probe(
    source: &mut dyn Source,
    end: Instant,
    call: impl FnOnce(&mut dyn Source, Instant) -> Value,
) -> Value {
    let now = source.now();
    if now >= end {
        return unknown("total observation deadline expired");
    }
    let until = end.min(now + PROBE);
    let value = call(source, until);
    if source.now() >= until {
        unknown("observation deadline expired")
    } else {
        value
    }
}

/// Collect using one monotonic budget from the release observation handler entry.
/// This does not include compilation of the release runner by `targo ship`.
pub fn capture(source: &mut dyn Source) -> Value {
    let start = source.now();
    let end = start + BUDGET;
    let local = probe(source, end, |s, until| s.local(until));
    let pending = probe(source, end, |s, until| s.pending(until));
    let versions = probe(source, end, |s, until| s.versions(until));
    let services = probe(source, end, |s, until| s.services(until));
    let vault = local["vault"]["value"]["vault"].as_str();
    let signer = local["vault"]["value"]["configured_signer"].as_str();
    let scoped = vault.is_some() && signer.is_some();
    let local_authority = if scoped {
        probe(source, end, |s, until| match s.authority("@local", until) {
            Ok(reply) => validate_authority(reply, vault, signer),
            Err(reason) => unknown(reason),
        })
    } else {
        unknown("no local vault and signer pin to scope authority observations")
    };
    let mut admission = admission_routes(&local_authority, None, &pending);
    let mut authorities = Vec::new();
    let discovery = if !scoped {
        unknown("no local vault and signer pin to scope authority observations")
    } else if source.now() >= end {
        unknown("total observation deadline expired")
    } else {
        match source.candidates() {
            Err(reason) => unknown(reason),
            Ok(_) if source.now() >= end => {
                unknown("candidate discovery exceeded the total deadline")
            }
            Ok(nodes) => {
                let candidate_count = nodes.len();
                let truncated = nodes.len() > MAX_PEERS;
                for node in nodes.into_iter().take(MAX_PEERS) {
                    if source.now() >= end {
                        break;
                    }
                    if !valid_cli_token(&node) {
                        authorities.push(json!({"node":node,
                            "capability":unknown("candidate host is not a single CLI token"),
                            "inspect_command":Value::Null}));
                        continue;
                    }
                    let report = probe(source, end, |s, until| match s.authority(&node, until) {
                        Ok(reply) => validate_authority(reply, vault, signer),
                        Err(reason) => unknown(reason),
                    });
                    if admission.is_empty() {
                        admission.extend(admission_routes(&report, Some(&node), &pending));
                    }
                    authorities.push(json!({"node":node,"capability":report,
                        "inspect_command":format!("aterm ctl --host {node} vault status")}));
                }
                json!({"state":"known", "candidates_truncated":truncated,
                    "candidate_count":candidate_count,"probed_count":authorities.len(),
                    "deadline_reached":source.now() >= end})
            }
        }
    };
    json!({"schema":1,"kind":"release-operational-status",
        "timing":{"origin":"release observation handler entry; runner compilation excluded",
            "budget_ms":BUDGET.as_millis() as u64,
            "elapsed_ms":source.now().saturating_duration_since(start).as_millis() as u64},
        "local":local,"pending":pending,"versions":versions,"services":services,
        "local_authority":local_authority,
        "authority_discovery":discovery,"authorities":authorities,
        "repair_routes":{
            "membership":"aterm secrets status --json",
            "pending":"aterm secrets requests --json",
            "services":"aterm secrets repair --json",
            "release_signing":"aterm secrets need ls --all"},
        "admission_routes":admission,
        "recipient_credentials_applied":"unobserved"})
}

fn admission_routes(capability: &Value, node: Option<&str>, pending: &Value) -> Vec<Value> {
    if capability["state"].as_str() != Some("known") || capability["value"].as_bool() != Some(true)
    {
        return Vec::new();
    }
    let mut prefix = vec!["aterm".to_owned(), "ctl".to_owned()];
    if let Some(node) = node {
        prefix.extend(["--host".into(), node.into()]);
    }
    prefix.extend(["vault".into(), "approve".into()]);
    let mut routes = Vec::new();
    if let Some(rows) = pending["value"].as_array() {
        for row in rows.iter().take(MAX_PENDING) {
            let (Some(code), Some(recipient)) = (row["code"].as_str(), row["recipient"].as_str())
            else {
                continue;
            };
            if !valid_cli_token(code) || !valid_cli_token(recipient) {
                continue;
            }
            let mut args = prefix.clone();
            args.extend([code.into(), "<fresh-32-lowercase-hex-nonce>".into()]);
            routes.push(
                json!({"authority":node.unwrap_or("@local"),"recipient":recipient,
                "requested_host":row["host"],"join_code":code,"approval_argv":args,
                "nonce":"generate a fresh 16-byte nonce before approval",
                "executed":false,"recipient_credentials_applied":"unobserved"}),
            );
        }
    }
    routes
}

fn validate_authority(reply: Value, vault: Option<&str>, signer: Option<&str>) -> Value {
    if vault.is_none() || signer.is_none() {
        return unknown("no local vault and signer pin to authenticate authority scope");
    }
    if reply["schema"].as_u64() != Some(1)
        || reply["kind"].as_str() != Some("vault-admin-capability")
    {
        return unknown("peer does not support vault capability observation");
    }
    if reply["vault"].as_str() != vault || reply["signer"].as_str() != signer {
        return unknown("authenticated peer reports another vault or signer");
    }
    match (
        reply["vault_admin"]["state"].as_str(),
        reply["vault_admin"]["value"].as_bool(),
    ) {
        (Some("known"), Some(value)) => json!({"state":"known","value":value}),
        _ => unknown("peer could not observe current administrative capability"),
    }
}

struct Live {
    paths: aterm_secrets::client::Paths,
}

impl Source for Live {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn local(&mut self, _: Instant) -> Value {
        aterm_secrets::operational::metadata(&self.paths)
    }
    fn pending(&mut self, until: Instant) -> Value {
        match aterm_secrets::client::inspect_requests_until(&self.paths, until) {
            Ok(inspection) => {
                let truncated = inspection.requests.len() > MAX_PENDING;
                let rows: Vec<_> = inspection
                    .requests
                    .into_iter()
                    .take(MAX_PENDING)
                    .map(|r| {
                        json!({
                            "recipient":r.recipient,"host":r.host,"code":r.code,"requested_at":r.at
                        })
                    })
                    .collect();
                json!({"state":"known","value":rows,
                    "membership_authenticated":inspection.known_members_filtered,
                    "queue":{"source":inspection.queue_source,
                        "server_address":inspection.queue_server.map(|ip| ip.to_string()),
                        "version":inspection.queue_version},
                    "truncated":truncated,"all_queues_observed":"unobserved"})
            }
            Err(error) => unknown(error.to_string()),
        }
    }
    fn versions(&mut self, until: Instant) -> Value {
        // Connect, authenticate and read under the same absolute deadline.
        let answer = local_version(until);
        let gui = match answer {
            Ok(line) if line.starts_with("OK version=") => {
                json!({"state":"known","value":line,"evidence":"current control reply"})
            }
            _ => unknown("local GUI did not report a version"),
        };
        json!({"gui_reported":gui,
            "installed_gui":installed_gui(),
            "service_reported":unknown("service protocol has no version observation"),
            "running_image_verified":false})
    }
    fn services(&mut self, until: Instant) -> Value {
        aterm_secrets::operational::services_until(&self.paths, until)
    }
    fn candidates(&mut self) -> Result<Vec<String>, String> {
        let steward = aterm_secrets::client::swarm_lease_holder(&self.paths)
            .ok()
            .flatten();
        crate::fleet_member::operational_candidates(steward.as_deref())
    }
    fn authority(&mut self, node: &str, until: Instant) -> Result<Value, String> {
        if node == "@local" {
            let mut client = local_client(until).map_err(|e| e.to_string())?;
            client.bound_by(until.saturating_duration_since(Instant::now()));
            let (header, body) = client
                .request_counted_with_limit("vault status", 16 * 1024)
                .map_err(|e| e.to_string())?;
            if !header.starts_with("OK ") {
                return Err("local service refused vault status".into());
            }
            return aterm_json::from_str(&body).map_err(|e| e.to_string());
        }
        let link = crate::drive_fleet(node, until.saturating_duration_since(Instant::now()))
            .map_err(|e| e.to_string())?;
        if Instant::now() >= until {
            return Err("authority connection deadline expired".into());
        }
        // The fleet Driven stream keeps the dial's absolute watchdog through
        // every exchange, including this bounded reply's body.
        let mut client = crate::RelayClient::new(link.stream);
        let (header, body) = client
            .request_counted_with_limit("vault status", 16 * 1024)
            .map_err(|e| e.to_string())?;
        if !header.starts_with("OK ") {
            return Err("peer refused vault status".into());
        }
        aterm_json::from_str(&body).map_err(|e| e.to_string())
    }
}

fn local_version(until: Instant) -> std::io::Result<String> {
    let mut client = local_client(until)?;
    client.bound_by(until.saturating_duration_since(Instant::now()));
    client.request_line("version")
}

fn valid_control_token(token: &str) -> bool {
    !token.is_empty() && !token.bytes().any(|b| matches!(b, b'\r' | b'\n' | 0))
}

fn local_client(until: Instant) -> std::io::Result<crate::RelayClient<aterm_uds::CtlStream>> {
    let path = aterm_ctl::resolve_sock_for(None)?;
    let path = aterm_uds::latest::resolve(&path);
    let stream = aterm_uds::connect::until(std::path::Path::new(&path), until)?;
    let token = aterm_ctl::read_token_beside(&path)?;
    if !valid_control_token(&token) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "control token is empty or contains line breaks",
        ));
    }
    let left = until.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "control authentication deadline expired",
        ));
    }
    crate::arm_ctl(&stream, left)?;
    let mut client = crate::RelayClient::new(stream).bounded(left);
    client.ready_for("version")?;
    client.write_line(&format!("AUTH {token}"))?;
    Ok(client)
}

fn installed_gui() -> Value {
    #[cfg(target_os = "macos")]
    {
        let path = std::path::Path::new("/Applications/aterm.app/Contents/Info.plist");
        let result = (|| -> Option<Value> {
            let text = aterm_uds::atomic_file::read_regular_text(path, 64 * 1024).ok()?;
            let version = plist_version(&text)?;
            Some(json!({"state":"known", "value":version, "evidence":path,
                "running_image_verified":false}))
        })();
        result.unwrap_or_else(|| unknown("installed bundle XML version unavailable"))
    }
    #[cfg(not(target_os = "macos"))]
    unknown("no installed bundle metadata on this platform")
}

#[cfg(any(target_os = "macos", test))]
fn plist_version(text: &str) -> Option<&str> {
    let tail = text
        .split_once("<key>CFBundleShortVersionString</key>")?
        .1
        .trim_start();
    let value = tail
        .strip_prefix("<string>")?
        .split_once("</string>")?
        .0
        .trim();
    (!value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-')))
    .then_some(value)
}

/// Observe this account without provisioning, changing grants, or applying credentials.
#[must_use]
pub fn observe() -> Value {
    match aterm_secrets::client::Paths::live() {
        Ok(paths) => capture(&mut Live { paths }),
        Err(error) => unknown(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
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
            validate_authority(source.authority.clone(), Some("team"), Some("pin"))["value"]
                .as_bool(),
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
}
