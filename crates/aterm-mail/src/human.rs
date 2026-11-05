// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! Unattended Proton ownership verification through its recovery-code API.
use crate::Secret;
use aterm_http::{Client, Method, ProxyMode, Trust};
use aterm_json::Value;
use std::time::Duration;

pub struct Challenge {
    pub token: Secret,
    pub methods: Vec<String>,
}
pub struct Verified {
    pub token: Secret,
    pub method: String,
}
pub struct Recovery {
    token: Secret,
    method: String,
    pub destination: String,
    pub purpose: Purpose,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    Login,
    External,
    VerifyAddress,
    Unknown,
}
impl Recovery {
    pub fn prepare(challenge: &Challenge) -> Result<Self, String> {
        let method = ["ownership-email", "ownership-sms"]
            .into_iter()
            .find(|method| challenge.methods.iter().any(|offered| offered == method))
            .ok_or("Proton offered no unattended recovery-code verification method")?;
        if challenge.token.expose().is_empty()
            || challenge.token.expose().len() > 8192
            || challenge.token.expose().chars().any(char::is_control)
        {
            return Err("Invalid verification challenge".into());
        }
        let mut recovery = Self {
            token: challenge.token.clone(),
            method: method.into(),
            destination: String::new(),
            purpose: Purpose::Unknown,
        };
        let details = request(Method::Get, &recovery.path())?;
        let (destination, purpose) = decode_details(&details)?;
        recovery.destination = destination;
        recovery.purpose = purpose;
        Ok(recovery)
    }
    pub fn is_email(&self) -> bool {
        self.method == "ownership-email"
    }
    pub fn request_code(&self) -> Result<(), String> {
        request(Method::Post, &self.path()).map(|_| ())
    }
    pub fn complete(&self, code: &Secret) -> Result<Verified, String> {
        let code = code.expose().trim();
        if code.is_empty() || code.len() > 128 || code.chars().any(char::is_control) {
            return Err("Invalid recovery-code format".into());
        }
        let response = request(
            Method::Post,
            &format!("{}/{}", self.path(), crate::oauth::encode(code)),
        )?;
        verified(&response, &self.method)
    }
    fn path(&self) -> String {
        format!(
            "/core/v4/verification/{}/{}",
            self.method,
            crate::oauth::encode(self.token.expose())
        )
    }
}
fn decode_details(details: &Value) -> Result<(String, Purpose), String> {
    let destination = details["ChallengeDestination"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 512 && !s.chars().any(char::is_control))
        .ok_or("Proton returned no recovery destination")?
        .into();
    let purpose = match details["ChallengeType"].as_str() {
        Some("login") => Purpose::Login,
        Some("external") => Purpose::External,
        Some("verify_address") => Purpose::VerifyAddress,
        _ => Purpose::Unknown,
    };
    Ok((destination, purpose))
}
fn verified(response: &Value, method: &str) -> Result<Verified, String> {
    let token = response["Token"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 8192 && !s.chars().any(char::is_control))
        .ok_or("Proton returned no valid verification proof")?;
    Ok(Verified {
        token: Secret::new(token.into()),
        method: method.into(),
    })
}
pub(crate) fn supported(method: &str) -> bool {
    matches!(method, "ownership-email" | "ownership-sms")
}
fn request(method: Method, path: &str) -> Result<Value, String> {
    let client = Client::new(
        Trust::PlatformVerifier,
        ProxyMode::Environment,
        Duration::from_secs(30),
    );
    let app_version = if cfg!(target_os = "macos") {
        "macos-bridge@3.27.1"
    } else if cfg!(target_os = "windows") {
        "windows-bridge@3.27.1"
    } else {
        "linux-bridge@3.27.1"
    };
    let response = client
        .request(method, &format!("https://mail.proton.me/api{path}"))
        .header("x-pm-appversion", app_version)
        .header(
            "User-Agent",
            concat!("aterm-mail/", env!("CARGO_PKG_VERSION")),
        )
        .header("Accept", "application/json")
        .limit(65536)
        .send(b"")
        .map_err(|_| "Could not reach Proton verification")?;
    let value: Value =
        aterm_json::from_slice(response.body()).map_err(|_| "Invalid verification response")?;
    if !response.is_success() || value["Code"].as_u64() != Some(1000) {
        return Err(format!(
            "Proton refused recovery verification (HTTP {}, code {})",
            response.status(),
            value["Code"].as_u64().unwrap_or(0)
        ));
    }
    Ok(value)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_recovery_methods_and_valid_proofs_are_accepted() {
        assert!(supported("ownership-email"));
        assert!(supported("ownership-sms"));
        assert!(!supported("captcha"));
        assert!(!supported("payment"));
        assert_eq!(
            verified(&aterm_json::json!({"Token":"proof"}), "ownership-email")
                .unwrap()
                .token
                .expose(),
            "proof"
        );
        assert!(verified(&aterm_json::json!({"Token":""}), "ownership-email").is_err());
        assert!(
            verified(
                &aterm_json::json!({"Token":"bad\r\nheader"}),
                "ownership-email"
            )
            .is_err()
        );
        let poisoned = Challenge {
            token: Secret::new("tok\r\ninjected".into()),
            methods: vec!["ownership-email".into()],
        };
        assert!(Recovery::prepare(&poisoned).is_err());
        assert!(
            decode_details(&aterm_json::json!({
                "ChallengeDestination": "user\x1b[2J@proton.example",
                "ChallengeType": "login"
            }))
            .is_err()
        );
        assert_eq!(
            decode_details(&aterm_json::json!({
                "ChallengeDestination": "u***@proton.example",
                "ChallengeType": "login"
            }))
            .unwrap(),
            ("u***@proton.example".into(), Purpose::Login)
        );
    }
}
