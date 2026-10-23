// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! GitHub's REST objects.

use aterm_json::Value;

use crate::error::{Error, Result};

/// Read a model from JSON, keeping the raw value.
pub trait FromJson: Sized {
    /// The model `value` holds.
    /// # Errors
    /// CouldNotRun, `Sub::Protocol`, why `GitHub's answer does not fit {type}: {e}`.
    fn from_json(value: &Value) -> Result<Self>;
}

/// `T::from_json`.
/// # Errors
/// As `FromJson::from_json`.
pub fn from_json<T: FromJson>(value: &Value) -> Result<T> {
    T::from_json(value)
}

fn decode<T: serde::de::DeserializeOwned>(value: &Value, name: &str) -> Result<T> {
    aterm_json::from_value(value.clone())
        .map_err(|e| Error::protocol(format!("GitHub's answer does not fit {name}: {e}")))
}

macro_rules! from_json_raw {
    ($($model:ident),* $(,)?) => {$(
        impl FromJson for $model {
            fn from_json(value: &Value) -> Result<Self> {
                let mut model: $model = decode(value, stringify!($model))?;
                model.raw = value.clone();
                Ok(model)
            }
        }
    )*};
}

/// A repository's or a user's owner.
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub struct OwnerObject {
    /// The login.
    pub login: String,
    /// The numeric id.
    pub id: u64,
    /// `User` or `Organization`.
    #[serde(rename = "type", default)]
    pub kind: String,
    /// The answer as GitHub sent it.
    #[serde(skip)]
    pub raw: Value,
}

/// A repository's permissions for the identity that read it.
#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize)]
pub struct Permissions {
    /// `admin`.
    #[serde(default)]
    pub admin: bool,
    /// `maintain`.
    #[serde(default)]
    pub maintain: bool,
    /// `push`.
    #[serde(default)]
    pub push: bool,
    /// `triage`.
    #[serde(default)]
    pub triage: bool,
    /// `pull`.
    #[serde(default)]
    pub pull: bool,
    /// The block as GitHub sent it.
    #[serde(skip)]
    pub raw: Value,
}

/// A repository.
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub struct RepoObject {
    /// The numeric id.
    pub id: u64,
    /// `OWNER/NAME`.
    pub full_name: String,
    /// `NAME`.
    pub name: String,
    /// The owner.
    pub owner: OwnerObject,
    /// Whether it is private.
    #[serde(default)]
    pub private: bool,
    /// Whether it is a fork.
    #[serde(default)]
    pub fork: bool,
    /// `public`, `private` or `internal`.
    #[serde(default)]
    pub visibility: Option<String>,
    /// The default branch.
    #[serde(default)]
    pub default_branch: Option<String>,
    /// The last push (`None` when nothing was ever pushed).
    #[serde(default)]
    pub pushed_at: Option<String>,
    /// The description.
    #[serde(default)]
    pub description: Option<String>,
    /// The identity's permissions, when GitHub says.
    #[serde(default)]
    pub permissions: Option<Permissions>,
    /// The answer as GitHub sent it.
    #[serde(skip)]
    pub raw: Value,
}

/// A release.
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub struct ReleaseObject {
    /// The numeric id.
    pub id: u64,
    /// The tag.
    pub tag_name: String,
    /// The title.
    #[serde(default)]
    pub name: Option<String>,
    /// Whether it is a draft.
    #[serde(default)]
    pub draft: bool,
    /// Whether it is a prerelease.
    #[serde(default)]
    pub prerelease: bool,
    /// Whether it is immutable.
    #[serde(default)]
    pub immutable: bool,
    /// The commitish the tag is made from.
    #[serde(default)]
    pub target_commitish: String,
    /// When it was made.
    #[serde(default)]
    pub created_at: String,
    /// When it was published.
    #[serde(default)]
    pub published_at: Option<String>,
    /// Its page.
    #[serde(default)]
    pub html_url: String,
    /// The notes.
    #[serde(default)]
    pub body: Option<String>,
    /// The assets, each with its raw.
    #[serde(default)]
    pub assets: Vec<AssetObject>,
    /// The answer as GitHub sent it.
    #[serde(skip)]
    pub raw: Value,
}

/// A release asset.
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub struct AssetObject {
    /// The numeric id.
    pub id: u64,
    /// The file name.
    pub name: String,
    /// The size in bytes.
    #[serde(default)]
    pub size: u64,
    /// `sha256:<hex>`, or null for assets before June 2025.
    #[serde(default)]
    pub digest: Option<String>,
    /// `uploaded`, `open`, `starter`, ….
    #[serde(default)]
    pub state: String,
    /// The media type.
    #[serde(default)]
    pub content_type: String,
    /// When it last changed.
    #[serde(default)]
    pub updated_at: String,
    /// The anonymous download URL.
    #[serde(default)]
    pub browser_download_url: String,
    /// Downloads so far.
    #[serde(default)]
    pub download_count: u64,
    /// The answer as GitHub sent it.
    #[serde(skip)]
    pub raw: Value,
}

/// One entry of a directory listing.
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub struct ContentEntry {
    /// `file`, `dir`, `symlink` or `submodule`.
    #[serde(rename = "type", default)]
    pub kind: String,
    /// The entry's name.
    pub name: String,
    /// The entry's path.
    pub path: String,
    /// The size in bytes.
    #[serde(default)]
    pub size: u64,
    /// The blob sha.
    #[serde(default)]
    pub sha: String,
    /// The raw URL, for files.
    #[serde(default)]
    pub download_url: Option<String>,
    /// The entry as GitHub sent it.
    #[serde(skip)]
    pub raw: Value,
}

/// A git tree.
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub struct TreeObject {
    /// The tree's sha.
    pub sha: String,
    /// Whether GitHub cut the listing.
    #[serde(default)]
    pub truncated: bool,
    /// The entries.
    #[serde(default)]
    pub tree: Vec<TreeEntry>,
    /// The answer as GitHub sent it.
    #[serde(skip)]
    pub raw: Value,
}

/// One entry of a git tree.
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub struct TreeEntry {
    /// The path.
    pub path: String,
    /// The mode.
    #[serde(default)]
    pub mode: String,
    /// `blob`, `tree` or `commit`.
    #[serde(rename = "type", default)]
    pub kind: String,
    /// The object's sha.
    #[serde(default)]
    pub sha: String,
    /// The size, for blobs.
    #[serde(default)]
    pub size: Option<u64>,
    /// The entry as GitHub sent it.
    #[serde(skip)]
    pub raw: Value,
}

/// A git author or committer.
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub struct GitActor {
    /// The name.
    #[serde(default)]
    pub name: Option<String>,
    /// The address.
    #[serde(default)]
    pub email: Option<String>,
    /// The date.
    #[serde(default)]
    pub date: Option<String>,
    /// The actor as GitHub sent it.
    #[serde(skip)]
    pub raw: Value,
}

/// A commit's git half.
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub struct CommitDetail {
    /// The message.
    #[serde(default)]
    pub message: String,
    /// The author.
    #[serde(default)]
    pub author: Option<GitActor>,
    /// The committer.
    #[serde(default)]
    pub committer: Option<GitActor>,
    /// The detail as GitHub sent it.
    #[serde(skip)]
    pub raw: Value,
}

/// A login.
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub struct LoginObject {
    /// The login (empty when GitHub answered an empty `{}` object).
    #[serde(default)]
    pub login: String,
    /// The object as GitHub sent it.
    #[serde(skip)]
    pub raw: Value,
}

/// A commit.
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub struct CommitObject {
    /// The sha.
    pub sha: String,
    /// The git half.
    pub commit: CommitDetail,
    /// The GitHub account of the author, when GitHub knows it.
    #[serde(default)]
    pub author: Option<LoginObject>,
    /// Its page.
    #[serde(default)]
    pub html_url: String,
    /// The answer as GitHub sent it.
    #[serde(skip)]
    pub raw: Value,
}

/// One file of a comparison.
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub struct CompareFile {
    /// The path.
    pub filename: String,
    /// `added`, `modified`, `removed`, ….
    #[serde(default)]
    pub status: String,
    /// The entry as GitHub sent it.
    #[serde(skip)]
    pub raw: Value,
}

/// A sha.
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub struct ShaObject {
    /// The sha.
    pub sha: String,
    /// The object as GitHub sent it.
    #[serde(skip)]
    pub raw: Value,
}

/// A comparison of two refs.
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub struct CompareObject {
    /// `ahead`, `behind`, `diverged` or `identical`.
    #[serde(default)]
    pub status: String,
    /// Commits head has that base lacks.
    #[serde(default)]
    pub ahead_by: u64,
    /// Commits base has that head lacks.
    #[serde(default)]
    pub behind_by: u64,
    /// Every commit of the comparison, listed or not.
    #[serde(default)]
    pub total_commits: u64,
    /// The commits GitHub listed.
    #[serde(default)]
    pub commits: Vec<CommitObject>,
    /// The files GitHub listed.
    #[serde(default)]
    pub files: Vec<CompareFile>,
    /// The merge base.
    #[serde(default)]
    pub merge_base_commit: Option<ShaObject>,
    /// The answer as GitHub sent it.
    #[serde(skip)]
    pub raw: Value,
}

/// A branch.
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub struct BranchObject {
    /// The name.
    pub name: String,
    /// Its head commit.
    pub commit: ShaObject,
    /// Whether it is protected.
    #[serde(default)]
    pub protected: bool,
    /// The answer as GitHub sent it.
    #[serde(skip)]
    pub raw: Value,
}

/// A tag.
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub struct TagObject {
    /// The name.
    pub name: String,
    /// Its commit.
    pub commit: ShaObject,
    /// The answer as GitHub sent it.
    #[serde(skip)]
    pub raw: Value,
}

/// A user or an organization.
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub struct UserObject {
    /// The login.
    pub login: String,
    /// The numeric id.
    pub id: u64,
    /// `User` or `Organization`.
    #[serde(rename = "type", default)]
    pub kind: String,
    /// The display name.
    #[serde(default)]
    pub name: Option<String>,
    /// The answer as GitHub sent it.
    #[serde(skip)]
    pub raw: Value,
}

from_json_raw!(
    OwnerObject,
    Permissions,
    RepoObject,
    AssetObject,
    ContentEntry,
    TreeObject,
    TreeEntry,
    GitActor,
    CommitDetail,
    LoginObject,
    CommitObject,
    CompareFile,
    ShaObject,
    CompareObject,
    BranchObject,
    TagObject,
    UserObject,
);

impl FromJson for ReleaseObject {
    fn from_json(value: &Value) -> Result<Self> {
        let mut release: ReleaseObject = decode(value, "ReleaseObject")?;
        release.raw = value.clone();
        if let Some(raws) = value.get("assets").and_then(Value::as_array) {
            for (asset, raw) in release.assets.iter_mut().zip(raws) {
                asset.raw = raw.clone();
            }
        }
        Ok(release)
    }
}

/// An asset's upload state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssetState {
    /// `uploaded`.
    Uploaded,
    /// `open`.
    Open,
    /// `starter`.
    Starter,
    /// Any other state.
    Other(String),
}

impl AssetObject {
    /// The state, as an `AssetState`.
    #[must_use]
    pub fn state(&self) -> AssetState {
        match self.state.as_str() {
            "uploaded" => AssetState::Uploaded,
            "open" => AssetState::Open,
            "starter" => AssetState::Starter,
            other => AssetState::Other(other.to_string()),
        }
    }

    /// `None` for a null digest (assets before June 2025).
    /// # Errors
    /// CouldNotRun, `Sub::Protocol`, why `asset {name}'s digest {digest} is not sha256:<64 hex>`.
    pub fn sha256(&self) -> Result<Option<Sha256Digest>> {
        let Some(digest) = &self.digest else {
            return Ok(None);
        };
        Sha256Digest::parse(digest).map(Some).ok_or_else(|| {
            Error::protocol(format!(
                "asset {}'s digest {digest} is not sha256:<64 hex>",
                self.name
            ))
        })
    }
}

/// A sha256 digest.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sha256Digest([u8; 32]);

impl Sha256Digest {
    /// `sha256:` and 64 hex digits (either case).
    #[must_use]
    pub fn parse(text: &str) -> Option<Sha256Digest> {
        let hex = text.strip_prefix("sha256:")?;
        if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let bytes = aterm_codec::hex::decode_vec(hex.as_bytes())?;
        let array: [u8; 32] = bytes.try_into().ok()?;
        Some(Sha256Digest(array))
    }

    /// These 32 bytes, already a SHA-256 digest (nothing is hashed).
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Sha256Digest {
        Sha256Digest(bytes)
    }

    /// 64 lowercase hex digits.
    #[must_use]
    pub fn hex(&self) -> String {
        aterm_codec::hex::encode_lower(&self.0)
    }

    /// The 32 bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Display for Sha256Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sha256:{}", self.hex())
    }
}

impl std::fmt::Debug for Sha256Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sha256:{}", self.hex())
    }
}

const COMPARE_FILES_CAP: usize = 300;

impl CommitObject {
    /// `commit.committer.date`.
    #[must_use]
    pub fn date(&self) -> Option<&str> {
        self.commit.committer.as_ref()?.date.as_deref()
    }

    /// The first line of `commit.message`.
    #[must_use]
    pub fn subject(&self) -> &str {
        self.commit.message.lines().next().unwrap_or("")
    }

    /// `author.login` (an empty one, from an `{}` author, is none), else `commit.author.name`.
    #[must_use]
    pub fn author_name(&self) -> Option<&str> {
        self.author
            .as_ref()
            .map(|a| a.login.as_str())
            .filter(|login| !login.is_empty())
            .or_else(|| self.commit.author.as_ref().and_then(|a| a.name.as_deref()))
    }
}

impl CompareObject {
    /// GitHub listed fewer commits than `total_commits` (its 250-commit cap).
    #[must_use]
    pub const fn capped_commits(&self) -> bool {
        (self.commits.len() as u64) < self.total_commits
    }

    /// GitHub listed 300 files, its cap.
    #[must_use]
    pub const fn capped_files(&self) -> bool {
        self.files.len() >= COMPARE_FILES_CAP
    }
}
