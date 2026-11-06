// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Value-free views of the names in the applied secrets registry. This module receives
//! names only; it never opens credential files or decrypts their contents.

use crate::need::catalog::{GhProfile, NOTARY_ENTRY, SITE_DEPLOYER_KEY_ENTRY, Want};

fn description(name: &str) -> String {
    match name {
        "gh_access_token" => Want::Gh(GhProfile::Dev).title(),
        "gh_access_token_alabsystems" => Want::Gh(GhProfile::Release).title(),
        "github-repositories.json" => {
            "Private development account and repository inventory for automatic setup".into()
        }
        NOTARY_ENTRY => Want::Notary.title(),
        SITE_DEPLOYER_KEY_ENTRY => Want::SiteDeployerKey.title(),
        "~/.aterm/release-credentials.toml" | "~/.aterm/release-ring/release-credentials.toml" => {
            "Release signing profile; use with --release-credentials".into()
        }
        "~/.aterm/machine.key" | "~/.aterm/release-ring/machine.key" => {
            "Machine release signing key".into()
        }
        "~/.aterm/machine.toml" | "~/.aterm/release-ring/machine.toml" => {
            "Machine release identity".into()
        }
        "ai-accounts.csv" => {
            "AI account sheet (the lab importer's input; read directly only on a Mac the lab does \
             not name)"
                .into()
        }
        "ai-mailbox.toml" => "AI account mailbox configuration".into(),
        "~/.config/gh/hosts.yml" => "GitHub CLI authentication".into(),
        "~/.config/git/credentials" => "Git HTTPS credentials".into(),
        "~/.config/git/config" => "Git configuration and credential routing".into(),
        _ if name.ends_with(".cer") && name.starts_with("~/.aterm/apple/") => {
            "Apple Developer ID certificate".into()
        }
        _ if name.ends_with(".key") && name.starts_with("~/.aterm/apple/") => {
            "Apple Developer ID private key".into()
        }
        _ if name.starts_with("~/.aterm/roster/") => {
            "Release machine roster or its signature".into()
        }
        _ => Want::Entry(name.to_owned()).title(),
    }
}

fn pointer(name: &str) -> Option<String> {
    // Reuse the materializer's eligibility rules, including reserved entries and login
    // files it deliberately refuses. The synthetic home is never opened.
    crate::materialize::target_path(std::path::Path::new("~"), name)
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
}

pub(super) fn listing(names: &[String], describe: bool, json: bool) -> crate::Result<String> {
    if json {
        let entries: Vec<_> = names
            .iter()
            .map(|name| {
                aterm_json::json!({
                    "name": name,
                    "description": description(name),
                    "pointer": pointer(name),
                })
            })
            .collect();
        return aterm_json::to_string(&aterm_json::json!({"entries": entries}))
            .map(|mut text| {
                text.push('\n');
                text
            })
            .map_err(|error| crate::Error::new(error.to_string()));
    }
    let mut out = String::new();
    for name in names {
        if describe {
            out.push_str(&format!(
                "{}\t{}\t{}\n",
                aterm_codec::tty::one_line(name, 512),
                pointer(name).map_or_else(
                    || "registry only".into(),
                    |p| aterm_codec::tty::one_line(&p, 512)
                ),
                aterm_codec::tty::one_line(&description(name), 512)
            ));
        } else {
            out.push_str(name);
            out.push('\n');
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_listing_does_not_require_a_readable_credential_identity() {
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = crate::client::Paths::for_home(home.path());
        crate::fsx::ensure_private_dir(&paths.dir).unwrap();
        crate::fsx::write_atomic(&paths.dir.join("identity"), b"not an age identity", 0o600)
            .unwrap();
        crate::fsx::write_atomic(
            &paths.dir.join("state.toml"),
            format!(
                "own = ['{SITE_DEPLOYER_KEY_ENTRY}', 'release-notary']\n[manifest]\nrelease-notary = 'metadata-digest'\n"
            ).as_bytes(),
            0o600,
        )
        .unwrap();
        assert!(crate::client::status(&paths).is_err());
        let names = crate::client::registered_names(&paths).unwrap();
        assert_eq!(names, ["release-notary", SITE_DEPLOYER_KEY_ENTRY]);
        let text = listing(&names, true, false).unwrap();
        assert!(text.contains("~/.secrets/release-notary"));
        assert!(!text.contains("metadata-digest"));
        assert!(!text.contains("not an age identity"));
        assert!(text.contains(SITE_DEPLOYER_KEY_ENTRY));
    }

    #[test]
    fn metadata_preserves_custom_destinations_and_json_escaping() {
        let names = vec![
            "~/.aterm/release-credentials.toml".into(),
            "quoted\"name".into(),
        ];
        let encoded = listing(&names, false, true).unwrap();
        let parsed: aterm_json::Value = aterm_json::from_str(&encoded).unwrap();
        assert_eq!(
            parsed["entries"][0]["pointer"].as_str(),
            Some("~/.aterm/release-credentials.toml")
        );
        assert_eq!(parsed["entries"][1]["name"].as_str(), Some("quoted\"name"));
        assert!(parsed["entries"][1]["pointer"].is_null());
        assert_eq!(
            listing(&names[..1], false, false).unwrap(),
            "~/.aterm/release-credentials.toml\n"
        );
    }
}
