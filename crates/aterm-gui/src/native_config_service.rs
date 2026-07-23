// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Serialized, versioned preference transaction core for native Settings.
//!
//! The reducer is pure and deterministic. A host worker reads/writes the file and feeds
//! whole snapshots here; this service performs OCC, per-key stale rebase, conditional
//! undo, and one `toml_edit` transform per accepted patch. The UI never writes config
//! bytes directly.

#![allow(
    dead_code,
    reason = "native Settings effect-host integration lands in stages"
)]

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

const UNDO_LIMIT: usize = 64;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ConfigSnapshot {
    pub(crate) revision: u64,
    pub(crate) text: Arc<str>,
    /// Exact immutable non-text assets admitted with `text` at `revision`.
    /// Cloning a snapshot clones this one outer Arc; Trail manifests and the
    /// custom Nyan sprite are never independently re-resolved by consumers.
    pub(crate) assets: Arc<crate::app_config::ConfigAssetCatalog>,
}

impl ConfigSnapshot {
    /// Semantic top-level values exactly as the transaction service compares
    /// them for optimistic concurrency.  Settings uses this projection for
    /// "Modified" and edit expectations: an effective default is presentation,
    /// not evidence that the key exists in `aterm.toml`.
    pub(crate) fn values(&self) -> Result<BTreeMap<String, String>, String> {
        parse_values(&self.text)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ExpectedValue {
    Any,
    Exact(Option<String>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConfigKeyEdit {
    pub(crate) key: String,
    pub(crate) expected: ExpectedValue,
    /// Raw semantic value. `None` removes the key and restores its default.
    pub(crate) value: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConfigPatchRequest {
    pub(crate) base_revision: u64,
    pub(crate) edits: Vec<ConfigKeyEdit>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct UndoToken(u64);

impl UndoToken {
    pub(crate) const fn get(self) -> u64 {
        self.0
    }

    pub(crate) const fn from_stored(raw: u64) -> Self {
        Self(raw)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ConfigPatchResult {
    Applied {
        snapshot: ConfigSnapshot,
        undo: UndoToken,
    },
    Unchanged {
        snapshot: ConfigSnapshot,
    },
    Conflict {
        snapshot: ConfigSnapshot,
        keys: Vec<String>,
    },
    Rejected {
        snapshot: ConfigSnapshot,
        message: String,
    },
}

#[derive(Clone, Debug)]
struct UndoRecord {
    token: UndoToken,
    before: Vec<(String, Option<String>)>,
    after: Vec<(String, Option<String>)>,
}

/// Single-writer reducer. It is intended to live behind the process-global Config
/// service queue; callers may clone only immutable snapshots.
pub(crate) struct VersionedConfigService {
    text: String,
    revision: u64,
    assets: Arc<crate::app_config::ConfigAssetCatalog>,
    key_revision: BTreeMap<String, u64>,
    next_undo: u64,
    undo: VecDeque<UndoRecord>,
}

impl VersionedConfigService {
    pub(crate) fn new(text: String) -> Result<Self, String> {
        parse_values(&text)?;
        let assets = resolve_assets(&text)?;
        Ok(Self {
            text,
            revision: 1,
            assets,
            key_revision: BTreeMap::new(),
            next_undo: 1,
            undo: VecDeque::new(),
        })
    }

    /// Seed the process service from the same user config path watched by the
    /// rest of the app. Missing config is the valid empty document; unreadable
    /// or malformed input is reported so startup can degrade without clobbering
    /// the user's file.
    pub(crate) fn load_current() -> Result<Self, String> {
        let text = match crate::app_config::config_path() {
            Some(path) => match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(error) => return Err(format!("{} unreadable ({error})", path.display())),
            },
            None => String::new(),
        };
        Self::new(text)
    }

    pub(crate) fn snapshot(&self) -> ConfigSnapshot {
        ConfigSnapshot {
            revision: self.revision,
            text: Arc::from(self.text.clone()),
            assets: Arc::clone(&self.assets),
        }
    }

    pub(crate) fn value(&self, key: &str) -> Result<Option<String>, String> {
        Ok(parse_values(&self.text)?.remove(key))
    }

    /// Apply one all-or-nothing patch. A stale patch is accepted only when every
    /// touched key is unchanged since its base revision and every supplied expectation
    /// still matches the current semantic TOML value.
    pub(crate) fn patch(&mut self, request: ConfigPatchRequest) -> ConfigPatchResult {
        if request.base_revision == 0 || request.base_revision > self.revision {
            return self.rejected("invalid base revision");
        }
        if request.edits.is_empty() {
            return ConfigPatchResult::Unchanged {
                snapshot: self.snapshot(),
            };
        }
        let mut keys = BTreeSet::new();
        if request
            .edits
            .iter()
            .any(|edit| edit.key.trim().is_empty() || !keys.insert(edit.key.clone()))
        {
            return self.rejected("empty or duplicate preference key");
        }

        let current = match parse_values(&self.text) {
            Ok(values) => values,
            Err(message) => return self.rejected(message),
        };
        let mut conflicts = Vec::new();
        for edit in &request.edits {
            let changed_after_base = self
                .key_revision
                .get(&edit.key)
                .is_some_and(|revision| *revision > request.base_revision);
            let expectation_failed = match &edit.expected {
                ExpectedValue::Any => false,
                ExpectedValue::Exact(expected) => current.get(&edit.key).cloned() != *expected,
            };
            if changed_after_base || expectation_failed {
                conflicts.push(edit.key.clone());
            }
        }
        if !conflicts.is_empty() {
            return ConfigPatchResult::Conflict {
                snapshot: self.snapshot(),
                keys: conflicts,
            };
        }

        let before = request
            .edits
            .iter()
            .map(|edit| (edit.key.clone(), current.get(&edit.key).cloned()))
            .collect::<Vec<_>>();
        let borrowed = request
            .edits
            .iter()
            .map(|edit| (edit.key.as_str(), edit.value.clone()))
            .collect::<Vec<_>>();
        let next = match crate::prefs::apply_prefs_edits(&self.text, &borrowed) {
            Ok(next) => next,
            Err(error) => return self.rejected(error.to_string()),
        };
        if next == self.text {
            return ConfigPatchResult::Unchanged {
                snapshot: self.snapshot(),
            };
        }

        // Resolve before advancing any service state. A catalog and its text are
        // admitted atomically; a parse failure cannot publish a mixed revision.
        let next_assets = match resolve_assets(&next) {
            Ok(catalog) => catalog,
            Err(message) => return self.rejected(message),
        };

        self.revision = self.revision.saturating_add(1);
        self.text = next;
        self.assets = next_assets;
        for key in &keys {
            self.key_revision.insert(key.clone(), self.revision);
        }
        let after_values = match parse_values(&self.text) {
            Ok(values) => values,
            Err(message) => return self.rejected(message),
        };
        let after = request
            .edits
            .iter()
            .map(|edit| (edit.key.clone(), after_values.get(&edit.key).cloned()))
            .collect();
        let token = UndoToken(self.next_undo.max(1));
        self.next_undo = self.next_undo.saturating_add(1);
        self.undo.push_back(UndoRecord {
            token,
            before,
            after,
        });
        while self.undo.len() > UNDO_LIMIT {
            self.undo.pop_front();
        }
        ConfigPatchResult::Applied {
            snapshot: self.snapshot(),
            undo: token,
        }
    }

    /// Reset a complete named set in one transform/revision. Consumers pass the
    /// editable schema keys; unrelated hand-authored configuration survives.
    pub(crate) fn reset_all(
        &mut self,
        base_revision: u64,
        keys: impl IntoIterator<Item = String>,
    ) -> ConfigPatchResult {
        self.patch(ConfigPatchRequest {
            base_revision,
            edits: keys
                .into_iter()
                .map(|key| ConfigKeyEdit {
                    key,
                    expected: ExpectedValue::Any,
                    value: None,
                })
                .collect(),
        })
    }

    /// Conditional undo: it succeeds only if every value written by the target patch
    /// is still current. Later unrelated changes are preserved.
    pub(crate) fn undo(&mut self, token: UndoToken) -> ConfigPatchResult {
        let Some(record) = self
            .undo
            .iter()
            .find(|record| record.token == token)
            .cloned()
        else {
            return self.rejected("undo token expired");
        };
        let edits = record
            .before
            .iter()
            .zip(&record.after)
            .map(|((key, before), (after_key, after))| {
                debug_assert_eq!(key, after_key);
                ConfigKeyEdit {
                    key: key.clone(),
                    expected: ExpectedValue::Exact(after.clone()),
                    value: before.clone(),
                }
            })
            .collect();
        self.patch(ConfigPatchRequest {
            base_revision: self.revision,
            edits,
        })
    }

    /// Accept a whole watcher/file snapshot. Changed keys are revision-stamped so a
    /// queued stale UI patch can rebase across unrelated external changes only.
    pub(crate) fn replace_external(&mut self, text: String) -> Result<ConfigSnapshot, String> {
        let before = parse_values(&self.text)?;
        let after = parse_values(&text)?;
        if text == self.text {
            return Ok(self.snapshot());
        }
        let assets = resolve_assets(&text)?;
        self.revision = self.revision.saturating_add(1);
        let keys = before
            .keys()
            .chain(after.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        for key in keys {
            if before.get(&key) != after.get(&key) {
                self.key_revision.insert(key, self.revision);
            }
        }
        self.text = text;
        self.assets = assets;
        Ok(self.snapshot())
    }

    fn rejected(&self, message: impl Into<String>) -> ConfigPatchResult {
        ConfigPatchResult::Rejected {
            snapshot: self.snapshot(),
            message: message.into(),
        }
    }
}

fn resolve_assets(text: &str) -> Result<Arc<crate::app_config::ConfigAssetCatalog>, String> {
    let config = toml::from_str::<crate::app_config::Config>(text)
        .map_err(|error| format!("aterm.toml is not a valid aterm config: {error}"))?;
    Ok(config.resolve_asset_catalog())
}

fn parse_values(text: &str) -> Result<BTreeMap<String, String>, String> {
    let document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| format!("existing aterm.toml is not valid TOML: {error}"))?;
    let mut values = BTreeMap::new();
    for (key, item) in document.iter() {
        // FULL-DEPTH dotted-key projection, mirroring the nested writer
        // (`prefs::apply_prefs_edits` walks a dotted path of ANY depth): every
        // item inside a table tree is ALSO exposed under its full dotted path,
        // so dotted editable keys at any depth (`matrix_rain.enabled`,
        // `sparkle_words.profanity.enabled`) get exact per-key semantics —
        // `is_explicit`, the Modified page, and OCC expected-value comparison
        // all address the leaf, never a table's opaque serialization. Each
        // intermediate table keeps its opaque whole-table entry too (its
        // consumers predate the dotted model, and an external edit anywhere in
        // the table must still stamp its revision).
        project_item(&mut values, key, item);
    }
    Ok(values)
}

/// Insert `item`'s semantic value at `path` and, when the item is table-like
/// (both `[a.b]` header tables and `b = { … }` inline tables), recurse into
/// every child under `path.child`. Arrays of tables stay one opaque entry,
/// matching the nested writer, which never addresses into them.
fn project_item(values: &mut BTreeMap<String, String>, path: &str, item: &toml_edit::Item) {
    if let Some(table) = item.as_table_like() {
        for (child, child_item) in table.iter() {
            project_item(values, &format!("{path}.{child}"), child_item);
        }
    }
    values.insert(path.to_string(), semantic_item(item));
}

fn semantic_item(item: &toml_edit::Item) -> String {
    match item {
        toml_edit::Item::Value(value) => {
            if let Some(value) = value.as_str() {
                value.to_owned()
            } else if let Some(value) = value.as_integer() {
                value.to_string()
            } else if let Some(value) = value.as_float() {
                value.to_string()
            } else if let Some(value) = value.as_bool() {
                value.to_string()
            } else {
                value.to_string().trim().to_owned()
            }
        }
        toml_edit::Item::Table(table) => table.to_string().trim().to_owned(),
        toml_edit::Item::ArrayOfTables(tables) => tables.to_string().trim().to_owned(),
        toml_edit::Item::None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(
        base_revision: u64,
        key: &str,
        expected: ExpectedValue,
        value: Option<&str>,
    ) -> ConfigPatchRequest {
        ConfigPatchRequest {
            base_revision,
            edits: vec![ConfigKeyEdit {
                key: key.into(),
                expected,
                value: value.map(str::to_owned),
            }],
        }
    }

    #[test]
    fn stale_patch_rebases_only_across_unchanged_keys() {
        let mut service = VersionedConfigService::new(
            "theme = \"Nord\"\nfont_px = 14.0\n# keep me\ncustom = 7\n".into(),
        )
        .unwrap();
        let base = service.snapshot().revision;
        assert!(matches!(
            service.patch(edit(base, "theme", ExpectedValue::Any, Some("Dracula"))),
            ConfigPatchResult::Applied { .. }
        ));
        assert!(matches!(
            service.patch(edit(base, "font_px", ExpectedValue::Any, Some("16"))),
            ConfigPatchResult::Applied { .. }
        ));
        assert!(matches!(
            service.patch(edit(base, "theme", ExpectedValue::Any, Some("Tokyo Night"))),
            ConfigPatchResult::Conflict { keys, .. } if keys == ["theme"]
        ));
        assert!(service.snapshot().text.contains("# keep me"));
        assert!(service.snapshot().text.contains("custom = 7"));
    }

    #[test]
    fn same_key_external_edit_blocks_stale_write() {
        let mut service = VersionedConfigService::new("theme = \"Nord\"\n".into()).unwrap();
        let base = service.snapshot().revision;
        service
            .replace_external("theme = \"Catppuccin Mocha\"\n".into())
            .unwrap();
        assert!(matches!(
            service.patch(edit(base, "theme", ExpectedValue::Any, Some("Dracula"))),
            ConfigPatchResult::Conflict { .. }
        ));
        assert_eq!(
            service.value("theme").unwrap().as_deref(),
            Some("Catppuccin Mocha")
        );
    }

    #[test]
    fn reset_all_is_one_revision_and_preserves_unknown_keys() {
        let mut service = VersionedConfigService::new(
            "theme = \"Nord\"\nfont_px = 15.0\ncustom = \"stay\"\n".into(),
        )
        .unwrap();
        let before = service.snapshot().revision;
        let ConfigPatchResult::Applied { snapshot, .. } =
            service.reset_all(before, ["theme".to_string(), "font_px".to_string()])
        else {
            panic!("reset should commit");
        };
        assert_eq!(snapshot.revision, before + 1);
        assert!(!snapshot.text.contains("theme ="));
        assert!(!snapshot.text.contains("font_px ="));
        assert!(snapshot.text.contains("custom = \"stay\""));
    }

    #[test]
    fn conditional_undo_preserves_unrelated_change_and_rejects_same_key_change() {
        let mut service =
            VersionedConfigService::new("theme = \"Nord\"\nfont_px = 14.0\n".into()).unwrap();
        let base = service.snapshot().revision;
        let ConfigPatchResult::Applied { undo, .. } =
            service.patch(edit(base, "theme", ExpectedValue::Any, Some("Dracula")))
        else {
            panic!("patch should commit");
        };
        let current = service.snapshot().revision;
        service.patch(edit(current, "font_px", ExpectedValue::Any, Some("18")));
        assert!(matches!(
            service.undo(undo),
            ConfigPatchResult::Applied { .. }
        ));
        assert_eq!(service.value("theme").unwrap().as_deref(), Some("Nord"));
        assert_eq!(service.value("font_px").unwrap().as_deref(), Some("18"));

        let base = service.snapshot().revision;
        let ConfigPatchResult::Applied { undo, .. } =
            service.patch(edit(base, "theme", ExpectedValue::Any, Some("Dracula")))
        else {
            panic!("patch should commit");
        };
        service
            .replace_external("theme = \"External\"\nfont_px = 18.0\n".into())
            .unwrap();
        assert!(matches!(
            service.undo(undo),
            ConfigPatchResult::Conflict { keys, .. } if keys == ["theme"]
        ));
        assert_eq!(service.value("theme").unwrap().as_deref(), Some("External"));
    }

    #[test]
    fn invalid_multi_key_patch_changes_nothing() {
        let mut service = VersionedConfigService::new("font_px = 14.0\n".into()).unwrap();
        let before = service.snapshot();
        let result = service.patch(ConfigPatchRequest {
            base_revision: before.revision,
            edits: vec![
                ConfigKeyEdit {
                    key: "font_px".into(),
                    expected: ExpectedValue::Any,
                    value: Some("16".into()),
                },
                ConfigKeyEdit {
                    key: "cursor_blink".into(),
                    expected: ExpectedValue::Any,
                    value: Some("definitely".into()),
                },
            ],
        });
        assert!(matches!(result, ConfigPatchResult::Rejected { .. }));
        assert_eq!(service.snapshot(), before);
    }

    #[test]
    fn rejected_sensitive_control_material_is_redacted_from_service_result() {
        for (key, pasted, expected) in [
            (
                crate::prefs::EDIT_TITLE_SUMMARY_TOKEN_FILE,
                "Bearer service-result-must-not-echo-this-secret",
                "expected a file path",
            ),
            (
                crate::prefs::EDIT_TITLE_SUMMARY_CA_FILE,
                concat!("-----BEGIN ", "PRIVATE KEY-----service-result-secret"),
                "expected a file path",
            ),
            (
                crate::prefs::EDIT_TITLE_SUMMARY_ENDPOINT,
                "https://models.example.test/chat?api_key=service-result-secret",
                "without a query or fragment",
            ),
        ] {
            let mut service = VersionedConfigService::new(String::new()).unwrap();
            let before = service.snapshot();
            let result = service.patch(ConfigPatchRequest {
                base_revision: before.revision,
                edits: vec![ConfigKeyEdit {
                    key: key.into(),
                    expected: ExpectedValue::Any,
                    value: Some(pasted.into()),
                }],
            });
            let ConfigPatchResult::Rejected { message, .. } = result else {
                panic!("pasted sensitive material should be rejected for {key}");
            };
            assert!(!message.contains(pasted), "material leaked: {message}");
            assert!(message.contains(expected), "{message}");
            assert_eq!(service.snapshot(), before);
        }
    }

    #[test]
    fn patch_undo_reset_and_external_publish_one_complete_asset_arc() {
        use std::sync::Arc;

        let path = std::env::temp_dir().join(format!(
            "aterm-config-service-nyan-{}.png",
            std::process::id()
        ));
        let rgba = [0x22, 0x66, 0xee, 0xff, 0xee, 0x66, 0x22, 0xff];
        let png = crate::app_introspect::encode_rgba8_png(&rgba, 2, 1).unwrap();
        std::fs::write(&path, png).unwrap();
        let source = path.to_string_lossy().into_owned();
        let mut service = VersionedConfigService::new(format!(
            "theme = \"Nord\"\ncursor_nyan_sprite = {source:?}\n"
        ))
        .unwrap();
        let initial = service.snapshot();
        assert!(matches!(
            initial.assets.nyan_sprite,
            crate::app_config::NyanSpriteAsset::Ready { .. }
        ));

        let ConfigPatchResult::Applied {
            snapshot: patched,
            undo,
        } = service.patch(edit(
            initial.revision,
            "theme",
            ExpectedValue::Exact(Some("Nord".to_string())),
            Some("Dracula"),
        ))
        else {
            panic!("patch must apply");
        };
        assert!(!Arc::ptr_eq(&initial.assets, &patched.assets));
        assert!(matches!(
            patched.assets.nyan_sprite,
            crate::app_config::NyanSpriteAsset::Ready { .. }
        ));

        let ConfigPatchResult::Applied {
            snapshot: undone, ..
        } = service.undo(undo)
        else {
            panic!("undo must apply");
        };
        assert!(!Arc::ptr_eq(&patched.assets, &undone.assets));
        assert_eq!(service.value("theme").unwrap().as_deref(), Some("Nord"));

        let ConfigPatchResult::Applied {
            snapshot: reset, ..
        } = service.reset_all(undone.revision, ["cursor_nyan_sprite".to_string()])
        else {
            panic!("reset must apply");
        };
        assert!(!Arc::ptr_eq(&undone.assets, &reset.assets));
        assert!(matches!(
            reset.assets.nyan_sprite,
            crate::app_config::NyanSpriteAsset::BuiltIn
        ));

        let missing = path.with_extension("missing.png");
        let external = service
            .replace_external(format!(
                "theme = \"External\"\ncursor_nyan_sprite = {:?}\n",
                missing.to_string_lossy()
            ))
            .expect("a bad optional sprite does not reject unrelated config");
        assert!(matches!(
            external.assets.nyan_sprite,
            crate::app_config::NyanSpriteAsset::Invalid { .. }
        ));
        assert_eq!(service.value("theme").unwrap().as_deref(), Some("External"));
        let _ = std::fs::remove_file(path);
    }

    /// FULL-DEPTH dotted projection: every item in a table tree is exposed
    /// under its full dotted path with exact per-key semantics (the
    /// `is_explicit` / OCC face for `matrix_rain.enabled`), while each table's
    /// legacy whole-table opaque entry survives for its pre-dotted consumers.
    /// An absent child is simply absent — an effective default is
    /// presentation, not evidence.
    #[test]
    fn parse_values_projects_dotted_keys() {
        let values =
            parse_values("font_px = 13.0\n[matrix_rain]\nenabled = true\nfps = 24\n").unwrap();
        assert_eq!(
            values.get("matrix_rain.enabled").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            values.get("matrix_rain.fps").map(String::as_str),
            Some("24")
        );
        assert!(
            values.contains_key("matrix_rain"),
            "the whole-table opaque entry survives"
        );
        assert_eq!(values.get("font_px").map(String::as_str), Some("13"));
        let none = parse_values("font_px = 13.0\n").unwrap();
        assert!(
            !none.keys().any(|k| k.starts_with("matrix_rain")),
            "absent table projects nothing (not explicit)"
        );
    }

    /// The dotted key is a first-class OCC citizen: a patch expecting the OLD
    /// child value conflicts once the file moved underneath it, exactly like a
    /// flat key — and the winning write + blank-revert both go through the one
    /// `apply_prefs_edits` transform (an emptied table is RETAINED: its header
    /// and comments survive, and empty parses to the same defaults).
    #[test]
    fn dotted_key_occ_conflict_write_and_blank_revert() {
        let mut service =
            VersionedConfigService::new("[matrix_rain]\nenabled = false\n".into()).unwrap();
        let base = service.snapshot().revision;

        // An external edit flips the child; a stale exact-expectation patch
        // must CONFLICT on the dotted key, not silently clobber.
        service
            .replace_external("[matrix_rain]\nenabled = true\n".into())
            .unwrap();
        let stale = service.patch(edit(
            base,
            "matrix_rain.enabled",
            ExpectedValue::Exact(Some("false".into())),
            Some("false"),
        ));
        let ConfigPatchResult::Conflict { keys, .. } = stale else {
            panic!("stale dotted expectation must conflict, got {stale:?}");
        };
        assert_eq!(keys, vec!["matrix_rain.enabled".to_string()]);

        // A current-revision write lands typed (bool, not string).
        let rev = service.snapshot().revision;
        let applied = service.patch(edit(
            rev,
            "matrix_rain.enabled",
            ExpectedValue::Exact(Some("true".into())),
            Some("false"),
        ));
        let ConfigPatchResult::Applied { snapshot, .. } = applied else {
            panic!("current-revision dotted write applies, got {applied:?}");
        };
        assert!(snapshot.text.contains("enabled = false"));
        assert_eq!(
            service.value("matrix_rain.enabled").unwrap().as_deref(),
            Some("false")
        );

        // Blank = revert: the child (the table's last key) goes; the emptied
        // [matrix_rain] header is RETAINED (it may carry user comments, and
        // empty parses to the same defaults).
        let rev = service.snapshot().revision;
        let reverted = service.patch(edit(
            rev,
            "matrix_rain.enabled",
            ExpectedValue::Exact(Some("false".into())),
            None,
        ));
        let ConfigPatchResult::Applied { snapshot, .. } = reverted else {
            panic!("blank revert applies, got {reverted:?}");
        };
        assert!(
            !snapshot.text.contains("enabled"),
            "the leaf is removed: {:?}",
            snapshot.text
        );
        assert!(
            snapshot.text.contains("[matrix_rain]"),
            "an emptied table is RETAINED (the nested writer's contract — its \
             header may carry user comments, and empty parses to the defaults): {:?}",
            snapshot.text
        );
        assert_eq!(service.value("matrix_rain.enabled").unwrap(), None);
    }

    /// DEPTH-2 dotted keys are projected and transacted exactly like depth-1:
    /// a hand-set `sparkle_words.profanity.enabled` gets its own per-key entry
    /// (the `is_explicit` face), a stale exact expectation CONFLICTS while the
    /// current one APPLIES, and blank-revert removes only the leaf — the
    /// emptied tables at BOTH depths are retained.
    #[test]
    fn depth_two_dotted_key_projection_occ_and_blank_revert() {
        let values = parse_values(
            "[sparkle_words]\nenabled = true\n\n[sparkle_words.profanity]\nenabled = false\n",
        )
        .unwrap();
        assert_eq!(
            values
                .get("sparkle_words.profanity.enabled")
                .map(String::as_str),
            Some("false"),
            "a hand-set depth-2 leaf projects as an exact per-key entry"
        );
        assert_eq!(
            values.get("sparkle_words.enabled").map(String::as_str),
            Some("true")
        );
        assert!(
            values.contains_key("sparkle_words") && values.contains_key("sparkle_words.profanity"),
            "every intermediate table keeps its opaque whole-table entry"
        );
        let none = parse_values("font_px = 13.0\n").unwrap();
        assert!(
            !none.keys().any(|k| k.starts_with("sparkle_words")),
            "an absent tree projects nothing (not explicit)"
        );

        let mut service = VersionedConfigService::new(
            "[sparkle_words]\n\n[sparkle_words.profanity]\nenabled = false\n".into(),
        )
        .unwrap();
        let base = service.snapshot().revision;

        // An external edit flips the depth-2 child; a stale exact-expectation
        // patch must CONFLICT on the full dotted path, not silently clobber.
        service
            .replace_external(
                "[sparkle_words]\n\n[sparkle_words.profanity]\nenabled = true\n".into(),
            )
            .unwrap();
        let stale = service.patch(edit(
            base,
            "sparkle_words.profanity.enabled",
            ExpectedValue::Exact(Some("false".into())),
            Some("false"),
        ));
        let ConfigPatchResult::Conflict { keys, .. } = stale else {
            panic!("stale depth-2 expectation must conflict, got {stale:?}");
        };
        assert_eq!(keys, vec!["sparkle_words.profanity.enabled".to_string()]);

        // The current expectation applies, typed (bool, not string).
        let rev = service.snapshot().revision;
        let applied = service.patch(edit(
            rev,
            "sparkle_words.profanity.enabled",
            ExpectedValue::Exact(Some("true".into())),
            Some("false"),
        ));
        let ConfigPatchResult::Applied { snapshot, .. } = applied else {
            panic!("current-revision depth-2 write applies, got {applied:?}");
        };
        assert!(snapshot.text.contains("enabled = false"));
        assert_eq!(
            service
                .value("sparkle_words.profanity.enabled")
                .unwrap()
                .as_deref(),
            Some("false")
        );

        // Blank = revert: only the leaf goes; the emptied tables at BOTH
        // depths are RETAINED (headers/comments survive, and empty parses to
        // the same defaults).
        let rev = service.snapshot().revision;
        let reverted = service.patch(edit(
            rev,
            "sparkle_words.profanity.enabled",
            ExpectedValue::Exact(Some("false".into())),
            None,
        ));
        let ConfigPatchResult::Applied { snapshot, .. } = reverted else {
            panic!("blank revert applies, got {reverted:?}");
        };
        assert!(
            !snapshot.text.contains("enabled"),
            "the leaf is removed: {:?}",
            snapshot.text
        );
        assert!(
            snapshot.text.contains("[sparkle_words]")
                && snapshot.text.contains("[sparkle_words.profanity]"),
            "emptied tables are RETAINED at both depths: {:?}",
            snapshot.text
        );
        assert_eq!(
            service.value("sparkle_words.profanity.enabled").unwrap(),
            None
        );
    }
}
