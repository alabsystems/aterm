// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only Hosting-site and project-permission observations.

use super::{Body, Client, Failed, Key};

/// The permission used to update existing Firebase Hosting resources.
pub const UPDATE: &str = "firebasehosting.sites.update";

/// A successful point-in-time permission observation, not a deployment receipt.
#[derive(Debug)]
pub struct Access {
    /// The site whose canonical project was read from Hosting.
    pub site: String,
    /// The canonical project used for the IAM permission test.
    pub project: String,
}

fn unavailable(why: impl Into<String>) -> Failed {
    Failed::NotReleased {
        why: why.into(),
        version: None,
    }
}

/// Resolve the actual site's project, then ask IAM whether this key can update it.
/// Neither request writes a resource or changes policy.
///
/// # Errors
/// Authentication, site lookup or permission observation fails, a response is
/// malformed, or IAM does not return the requested update permission.
pub fn check(
    hosting: &Client,
    iam: &Client,
    key: &Key,
    site: &str,
    now: u64,
) -> Result<Access, Failed> {
    super::site_id(site).map_err(Failed::Credential)?;
    let bearer = hosting.token(key, now)?;
    let answer = hosting
        .call(
            "GET",
            &hosting.api(&format!("v1beta1/projects/-/sites/{site}")),
            Body::Empty,
            &bearer,
        )
        .map_err(|why| unavailable(format!("reading the Hosting site: {why}")))?;
    let doc = answer
        .json()
        .map_err(|why| unavailable(format!("reading the Hosting site: {why}")))?;
    let name = doc["name"]
        .as_str()
        .ok_or_else(|| unavailable("Hosting returned no site name"))?;
    let mut parts = name.split('/');
    let project = match (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) {
        (Some("projects"), Some(project), Some("sites"), Some(found), None)
            if found == site
                && project != "-"
                && !project.is_empty()
                && project.len() <= 128
                && project
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-') =>
        {
            project
        }
        _ => {
            return Err(unavailable(
                "Hosting returned an invalid or different site name",
            ));
        }
    };
    let request = aterm_json::json!({"permissions": [UPDATE]});
    let answer = iam
        .call(
            "POST",
            &iam.api(&format!("v3/projects/{project}:testIamPermissions")),
            Body::Json(&request),
            &bearer,
        )
        .map_err(|why| unavailable(format!("checking Hosting permission: {why}")))?;
    let doc = answer
        .json()
        .map_err(|why| unavailable(format!("checking Hosting permission: {why}")))?;
    let granted = match doc.get("permissions") {
        None => false,
        Some(aterm_json::Value::Array(permissions))
            if permissions.iter().all(|p| p.as_str().is_some()) =>
        {
            permissions.iter().any(|p| p.as_str() == Some(UPDATE))
        }
        _ => return Err(unavailable("IAM returned malformed permissions")),
    };
    if !granted {
        return Err(Failed::Credential(format!(
            "IAM did not grant {UPDATE} for the Hosting site's project"
        )));
    }
    Ok(Access {
        site: site.into(),
        project: project.into(),
    })
}
