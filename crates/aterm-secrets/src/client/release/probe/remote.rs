// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact owned draft objects. Server replies are evidence only after identity checks.

use super::{
    engine::{Outcome, Receipt},
    model::Journal,
};
use crate::{Error, Result};
use aterm_json::{Value, json};
use aterm_update_core::github_rest::Method;
use std::collections::BTreeMap;
use std::path::Path;

pub(super) struct Answer {
    pub status: u16,
    pub body: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ApiError {
    NotStarted,
    Uncertain,
}

impl From<ApiError> for Error {
    fn from(error: ApiError) -> Self {
        Self::new(match error {
            ApiError::NotStarted => "probe GitHub request did not start",
            ApiError::Uncertain => "probe GitHub request has no complete response",
        })
    }
}

/// The native port uses github_rest's pinned origins and anonymous CDN download path.
/// Errors never carry response bodies, credential values or subprocess diagnostics.
pub(super) trait Api {
    fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> std::result::Result<Answer, ApiError>;
    fn upload(&self, path: &str, file: &Path) -> std::result::Result<Answer, ApiError>;
    fn download(&self, path: &str, file: &Path, cap: u64) -> Result<()>;
}

pub(super) struct Remote<'a> {
    pub api: &'a dyn Api,
    pub journal: &'a Journal,
    pub index: usize,
    pub principal: &'a str,
}

#[derive(PartialEq)]
struct ReleaseObservation {
    tag: String,
    target: Option<TargetObservation>,
}

#[derive(PartialEq)]
struct TargetObservation {
    fields: Vec<Option<Value>>,
    author: Vec<Option<Value>>,
    assets: BTreeMap<u64, AssetObservation>,
}

#[derive(PartialEq)]
struct AssetObservation {
    fields: Vec<Option<Value>>,
    uploader: Vec<Option<Value>>,
}

fn fields(value: &Value, names: &[&str]) -> Vec<Option<Value>> {
    names.iter().map(|name| value.get(name).cloned()).collect()
}

fn actor_fields(value: Option<&Value>) -> Vec<Option<Value>> {
    value.map_or_else(
        || vec![None; 4],
        |value| fields(value, &["id", "node_id", "login", "type"]),
    )
}

fn release_observation(value: &Value, target: &str) -> Result<ReleaseObservation> {
    let tag = value
        .get("tag_name")
        .and_then(Value::as_str)
        .filter(|tag| !tag.is_empty())
        .ok_or_else(|| Error::new("probe release listing has no tag identity"))?;
    let observed = if tag == target {
        let mut assets = BTreeMap::new();
        for asset in value
            .get("assets")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::new("probe target release has no complete asset list"))?
        {
            let id = positive(asset, "id")?;
            let projection = AssetObservation {
                fields: fields(
                    asset,
                    &[
                        "id",
                        "node_id",
                        "name",
                        "label",
                        "state",
                        "size",
                        "digest",
                        "content_type",
                    ],
                ),
                uploader: actor_fields(asset.get("uploader")),
            };
            if assets.insert(id, projection).is_some() {
                return Err(Error::new("probe target release repeats an asset identity"));
            }
        }
        Some(TargetObservation {
            fields: fields(
                value,
                &[
                    "id",
                    "node_id",
                    "tag_name",
                    "name",
                    "body",
                    "target_commitish",
                    "draft",
                    "prerelease",
                    "published_at",
                ],
            ),
            author: actor_fields(value.get("author")),
            assets,
        })
    } else {
        None
    };
    Ok(ReleaseObservation {
        tag: tag.to_owned(),
        target: observed,
    })
}

fn listing_observation(rows: &[Value], target: &str) -> Result<BTreeMap<u64, ReleaseObservation>> {
    let mut observed = BTreeMap::new();
    for row in rows {
        let id = positive(row, "id")?;
        if observed
            .insert(id, release_observation(row, target)?)
            .is_some()
        {
            return Err(Error::new(
                "probe release listing repeats an object identity",
            ));
        }
    }
    Ok(observed)
}

impl Remote<'_> {
    fn root(&self) -> String {
        format!(
            "/repos/{}/aterm",
            self.journal.repositories[self.index].owner
        )
    }

    fn marker(&self) -> String {
        format!(
            "Owned aterm release capability probe.\nnonce={}\nmember={}\nwriter={}\nincarnation={}\nartifact={}\n",
            self.journal.nonce,
            self.journal.subject.member,
            self.journal.subject.writer,
            self.journal.subject.incarnation,
            self.journal.artifact.as_ref().unwrap().sha256
        )
    }

    fn name(&self) -> String {
        format!("aterm verification {}", self.journal.nonce)
    }
    fn asset_name(&self) -> String {
        format!("aterm-probe-{}.zip", self.journal.nonce)
    }

    fn release_matches(&self, value: &Value, expected: Option<u64>) -> Result<u64> {
        let id = positive(value, "id")?;
        let repo = &self.journal.repositories[self.index];
        if expected.is_some_and(|expected| id != expected)
            || value.get("draft").and_then(Value::as_bool) != Some(true)
            || value.get("prerelease").and_then(Value::as_bool) != Some(true)
            || value.get("tag_name").and_then(Value::as_str) != Some(self.journal.tag().as_str())
            || value.get("name").and_then(Value::as_str) != Some(self.name().as_str())
            || value.get("body").and_then(Value::as_str) != Some(self.marker().as_str())
            || value.get("target_commitish").and_then(Value::as_str) != repo.commit.as_deref()
            || value
                .get("author")
                .and_then(|v| v.get("login"))
                .and_then(Value::as_str)
                != Some(self.principal)
            || value.get("published_at").is_none_or(|v| !v.is_null())
        {
            return Err(Error::new("release probe found a foreign or changed draft"));
        }
        // A foreign attachment prevents deletion of the draft that contains it.
        let assets = value
            .get("assets")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::new("release probe draft has no complete asset list"))?;
        if assets.len() > 1 || assets.iter().any(|a| positive(a, "id").ok() != repo.asset) {
            return Err(Error::new("release probe draft contains an unowned asset"));
        }
        Ok(id)
    }

    fn asset_ownership(&self, value: &Value, expected: Option<u64>) -> Result<u64> {
        let id = positive(value, "id")?;
        if expected.is_some_and(|expected| id != expected)
            || value.get("name").and_then(Value::as_str) != Some(self.asset_name().as_str())
            || value.get("label").and_then(Value::as_str) != Some(self.journal.nonce.as_str())
            || !matches!(
                value.get("state").and_then(Value::as_str),
                Some("uploaded" | "starter")
            )
            || (value.get("state").and_then(Value::as_str) == Some("starter")
                && value.get("size").and_then(Value::as_u64) != Some(0))
            || value
                .get("uploader")
                .and_then(|v| v.get("login"))
                .and_then(Value::as_str)
                != Some(self.principal)
        {
            return Err(Error::new("release probe found a foreign or changed asset"));
        }
        Ok(id)
    }

    fn asset_matches(&self, value: &Value, expected: Option<u64>) -> Result<u64> {
        let id = self.asset_ownership(value, expected)?;
        let artifact = self.journal.artifact.as_ref().unwrap();
        if value.get("state").and_then(Value::as_str) != Some("uploaded")
            || value.get("size").and_then(Value::as_u64) != Some(artifact.bytes)
            || value
                .get("digest")
                .and_then(Value::as_str)
                .is_some_and(|d| d != format!("sha256:{}", artifact.sha256))
        {
            return Err(Error::new("release probe found a foreign or changed asset"));
        }
        Ok(id)
    }

    fn get(&self, path: &str) -> Result<Option<Value>> {
        let answer = self
            .api
            .request(Method::Get, path, None)
            .inspect_err(|error| {
                crate::log_line(&format!(
                    "aterm release probe: read stage=release-objects transport={error:?}"
                ));
            })?;
        match answer.status {
            200 => Ok(Some(answer.body)),
            404 => Ok(None),
            _ => {
                crate::log_line(&format!(
                    "aterm release probe: read stage=release-objects status={}",
                    answer.status
                ));
                Err(Error::new("release probe GitHub read was refused"))
            }
        }
    }

    fn listing(&self) -> Result<Vec<Value>> {
        let mut all = Vec::new();
        for page in 1..=10 {
            let path = format!("{}/releases?per_page=100&page={page}", self.root());
            let value = self
                .get(&path)?
                .ok_or_else(|| Error::new("probe release listing is unavailable"))?;
            let rows = value
                .as_array()
                .filter(|rows| rows.len() <= 100)
                .ok_or_else(|| Error::new("probe release listing is malformed"))?;
            all.extend(rows.iter().cloned());
            if rows.len() < 100 {
                return Ok(all);
            }
        }
        Err(Error::new(
            "probe release listing exceeds its complete-read bound",
        ))
    }

    fn objects_for_tag(&self) -> Result<Vec<Value>> {
        // The by-tag endpoint can omit drafts. Read the authenticated listing too,
        // and require two complete stable identity passes before relying on absence.
        // Unrelated download counters and timestamps can change during active use;
        // target ownership and artifact fields must remain unchanged.
        let published = self.get(&format!(
            "{}/releases/tags/{}",
            self.root(),
            self.journal.tag().replace('/', "%2F")
        ))?;
        if published.as_ref().is_some_and(|value| {
            value.get("tag_name").and_then(Value::as_str) != Some(self.journal.tag().as_str())
        }) {
            return Err(Error::new("probe tag lookup names another tag"));
        }
        let first = super::diagnosed("release-listing-first-read", self.listing())?;
        let observed = super::diagnosed(
            "release-listing-first",
            listing_observation(&first, &self.journal.tag()),
        )?;
        let second = super::diagnosed("release-listing-second-read", self.listing())?;
        if super::diagnosed(
            "release-listing-second",
            listing_observation(&second, &self.journal.tag()),
        )? != observed
        {
            super::diagnostic("release-listing-changed", &Error::new("changed"));
            return Err(Error::new("probe release listing changed while observed"));
        }
        if let Some(value) = &published {
            let id = positive(value, "id")?;
            let by_tag = release_observation(value, &self.journal.tag())?;
            if observed.get(&id).is_some_and(|listed| *listed != by_tag)
                || observed
                    .iter()
                    .any(|(listed_id, listed)| listed.target.is_some() && *listed_id != id)
            {
                super::diagnostic("release-by-tag-changed", &Error::new("changed"));
                return Err(Error::new("probe release changed across lookup sources"));
            }
        }
        let mut matching = BTreeMap::new();
        for value in first.into_iter().chain(published) {
            if value.get("tag_name").and_then(Value::as_str) == Some(self.journal.tag().as_str()) {
                let id = positive(&value, "id")?;
                matching.insert(id, value);
            }
        }
        Ok(matching.into_values().collect())
    }

    pub(super) fn require_no_release(&self) -> Result<()> {
        if !self.objects_for_tag()?.is_empty() {
            return Err(Error::new("probe tag still has a release or draft"));
        }
        Ok(())
    }

    fn release(&self) -> Result<Value> {
        let id = self.journal.repositories[self.index]
            .release
            .ok_or_else(|| Error::new("release probe has no draft receipt"))?;
        let value = self
            .get(&format!("{}/releases/{id}", self.root()))?
            .ok_or_else(|| Error::new("release probe draft disappeared before cleanup"))?;
        self.release_matches(&value, Some(id))?;
        Ok(value)
    }

    pub(super) fn create_draft(&self) -> Result<Outcome> {
        match self.objects_for_tag() {
            Ok(objects) if objects.is_empty() => {}
            Ok(_) => return Ok(Outcome::NotStarted("probe tag already has a release")),
            Err(error) => {
                super::diagnostic("draft-objects", &error);
                return Ok(Outcome::NotStarted("probe draft preflight is unavailable"));
            }
        }
        // The caller persisted Draft intent before reaching this POST. This method is
        // never called again by recovery; recovery observes the unique tag instead.
        let body = json!({"tag_name": self.journal.tag(),
            "target_commitish": self.journal.repositories[self.index].commit,
            "name": self.name(), "body": self.marker(), "draft":true,
            "prerelease":true, "make_latest":"false"});
        match self.api.request(
            Method::Post,
            &format!("{}/releases", self.root()),
            Some(&body),
        ) {
            Ok(answer) if answer.status == 201 => Ok(Outcome::Applied(Receipt::Draft(
                self.release_matches(&answer.body, None)?,
            ))),
            Err(ApiError::NotStarted) => Ok(Outcome::NotStarted("draft creation did not start")),
            Ok(_) | Err(ApiError::Uncertain) => {
                Ok(Outcome::Pending("draft creation needs observation"))
            }
        }
    }

    pub(super) fn observe_draft(&self) -> Result<Outcome> {
        let objects = self.objects_for_tag()?;
        let value = match objects.as_slice() {
            [] => return Ok(Outcome::Pending("draft creation remains unconfirmed")),
            [value] => value,
            _ => return Err(Error::new("probe tag has ambiguous releases")),
        };
        Ok(Outcome::Applied(Receipt::Draft(
            self.release_matches(value, None)?,
        )))
    }

    pub(super) fn upload(&self, file: &Path) -> Result<Outcome> {
        if self.release().is_err() {
            return Ok(Outcome::NotStarted("probe upload preflight is unavailable"));
        }
        let id = self.journal.repositories[self.index].release.unwrap();
        let path = format!(
            "{}/releases/{id}/assets?name={}&label={}",
            self.root(),
            self.asset_name(),
            self.journal.nonce
        );
        match self.api.upload(&path, file) {
            Ok(answer) if answer.status == 201 => Ok(Outcome::Applied(Receipt::Asset(
                self.asset_matches(&answer.body, None)?,
            ))),
            Err(ApiError::NotStarted) => Ok(Outcome::NotStarted("asset upload did not start")),
            Ok(_) | Err(ApiError::Uncertain) => {
                Ok(Outcome::Pending("asset upload needs observation"))
            }
        }
    }

    pub(super) fn observe_upload(&self) -> Result<Outcome> {
        let id = self.journal.repositories[self.index].release.unwrap();
        // The complete list is bounded; ambiguity is never resolved by deleting or retrying.
        let answer = self.api.request(
            Method::Get,
            &format!("{}/releases/{id}/assets?per_page=100", self.root()),
            None,
        )?;
        if answer.status != 200 {
            return Err(Error::new("release probe asset listing was refused"));
        }
        let rows = answer
            .body
            .as_array()
            .filter(|a| a.len() < 100)
            .ok_or_else(|| Error::new("release probe asset listing is incomplete"))?;
        if rows.is_empty() {
            return Ok(Outcome::Pending("asset upload remains unconfirmed"));
        }
        if rows.len() != 1 {
            return Err(Error::new("release probe found unowned attachments"));
        }
        let asset = self.asset_ownership(&rows[0], None)?;
        if rows[0].get("state").and_then(Value::as_str) == Some("uploaded") {
            self.asset_matches(&rows[0], None)?;
        }
        let mut observed = self.journal.clone();
        observed.repositories[self.index].asset = Some(asset);
        Remote {
            journal: &observed,
            ..*self
        }
        .release()?;
        Ok(Outcome::Applied(Receipt::Asset(asset)))
    }

    pub(super) fn readback(&self, destination: &Path) -> Result<Outcome> {
        self.release()?;
        let id = self.journal.repositories[self.index].asset.unwrap();
        let path = format!("{}/releases/assets/{id}", self.root());
        let value = self
            .get(&path)?
            .ok_or_else(|| Error::new("release probe asset disappeared"))?;
        self.asset_matches(&value, Some(id))?;
        self.api.download(
            &path,
            destination,
            self.journal.artifact.as_ref().unwrap().bytes,
        )?;
        let bytes = super::artifact::read(destination)?;
        let sha256 = aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&bytes));
        Ok(Outcome::Applied(Receipt::Readback {
            sha256,
            bytes: bytes.len() as u64,
        }))
    }

    pub(super) fn has_owned_asset(&self) -> Result<bool> {
        let Some(id) = self.journal.repositories[self.index].asset else {
            return Ok(false);
        };
        let path = format!("{}/releases/assets/{id}", self.root());
        let Some(value) = self.get(&path)? else {
            return Ok(false);
        };
        self.release()?;
        self.asset_ownership(&value, Some(id))?;
        if value.get("state").and_then(Value::as_str) == Some("starter") {
            return Ok(false);
        }
        self.asset_matches(&value, Some(id))?;
        Ok(true)
    }

    pub(super) fn delete_asset(&self, _observe: bool) -> Result<Outcome> {
        let id = self.journal.repositories[self.index].asset.unwrap();
        let path = format!("{}/releases/assets/{id}", self.root());
        let Some(value) = self.get(&path)? else {
            return Ok(Outcome::Applied(Receipt::AssetAbsent));
        };
        self.release()?;
        self.asset_ownership(&value, Some(id))?;
        // A current exact owned-object read permits another idempotent DELETE after
        // a lost reply. Creation/upload POSTs never use this recovery rule.
        match self.api.request(Method::Delete, &path, None) {
            Ok(answer) if matches!(answer.status, 204 | 404) => {
                if self.get(&path)?.is_none() {
                    Ok(Outcome::Applied(Receipt::AssetAbsent))
                } else {
                    Ok(Outcome::Pending("asset deletion needs observation"))
                }
            }
            Ok(_) | Err(_) => Ok(Outcome::Pending("asset deletion needs observation")),
        }
    }

    pub(super) fn delete_draft(&self, _observe: bool) -> Result<Outcome> {
        let id = self.journal.repositories[self.index].release.unwrap();
        let path = format!("{}/releases/{id}", self.root());
        let Some(value) = self.get(&path)? else {
            return Ok(Outcome::Applied(Receipt::DraftAbsent));
        };
        self.release_matches(&value, Some(id))?;
        if value["assets"]
            .as_array()
            .is_none_or(|assets| !assets.is_empty())
        {
            return Err(Error::new(
                "release probe refuses to delete a draft with attachments",
            ));
        }
        let assets = self
            .api
            .request(Method::Get, &format!("{path}/assets?per_page=100"), None)?;
        if assets.status != 200 || assets.body.as_array().is_none_or(|rows| !rows.is_empty()) {
            return Err(Error::new(
                "probe refuses to delete a draft with any attachment",
            ));
        }
        match self.api.request(Method::Delete, &path, None) {
            Ok(answer) if matches!(answer.status, 204 | 404) => {
                if self.get(&path)?.is_none() {
                    Ok(Outcome::Applied(Receipt::DraftAbsent))
                } else {
                    Ok(Outcome::Pending("draft deletion needs observation"))
                }
            }
            Ok(_) | Err(_) => Ok(Outcome::Pending("draft deletion needs observation")),
        }
    }
}

fn positive(value: &Value, field: &str) -> Result<u64> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .filter(|n| *n != 0)
        .ok_or_else(|| Error::new("release probe response lacks an object identity"))
}

#[cfg(test)]
mod tests;
