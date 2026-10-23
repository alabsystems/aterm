// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHICH FIELDS A SET OF VALUES WRITES, read off their serde derives: the cross-version guard checks only the shapes its desks carry, so a field the producer can write but no desk does is a field the next consumer's parse is never held to.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use aterm_core::terminal::CheckpointMeta;

use crate::restore::RestoreManifest;
use crate::session_store::SessionHandoff;

use serde::ser::{
    Serialize, SerializeMap, SerializeSeq, SerializeStruct, SerializeStructVariant, SerializeTuple,
    SerializeTupleStruct, SerializeTupleVariant, Serializer,
};

/// The fields every value walked so far wrote, and the ones some value passed over, as `Struct.field` (`Enum::Variant.field` for a struct variant), and the variants of the enums they reach, as `Enum::Variant`: written when some value took it, passed over when none did (a walk names the externally tagged variants it wrote; [`Self::settle_variants`] the rest).
#[derive(Default)]
pub(crate) struct FieldCover {
    written: BTreeSet<String>,
    passed: BTreeSet<String>,
    /// Every value [`Self::read`] kept, as a document, and how to read one
    /// back as its own type ([`Self::settle_variants`]).
    docs: Vec<aterm_json::Value>,
    reread: Option<Reread>,
}

impl FieldCover {
    /// Record which of `value`'s fields it writes and which it passes over.
    pub(crate) fn walk<T: Serialize + ?Sized>(&mut self, value: &T) {
        value
            .serialize(Walk { cover: self })
            .expect("walking a value writes nothing that can fail");
    }

    /// [`Self::walk`] a value of a record's own type, and keep it for [`Self::settle_variants`].
    pub(crate) fn read<T: Serialize + serde::de::DeserializeOwned>(&mut self, value: &T) {
        self.walk(value);
        self.docs
            .push(aterm_json::to_value(value).expect("a carried value is a JSON document"));
        self.reread = Some(reread::<T>);
    }

    /// Name, as passed over, every variant of every enum the values [`Self::read`] reach that none of them took, and, as written, every one some value took that the walk could not name (an internally tagged enum's, whose tag is a string to a serializer).
    pub(crate) fn settle_variants(&mut self) {
        let Some(reread) = self.reread else {
            return;
        };
        for doc in &self.docs {
            if let Err(Refused(why)) = reread(Doc(doc.clone())) {
                panic!("a carried value does not read back from its own document: {why}");
            }
        }
        let mut places = BTreeMap::new();
        for (at, doc) in self.docs.iter().enumerate() {
            places_in(doc, at, &mut Vec::new(), "", &mut places);
        }
        let read_back = |place: &Place, put: &dyn Fn(&mut aterm_json::Value)| {
            let mut doc = self.docs[place.doc].clone();
            put(place.at(&mut doc));
            REFUSED.with(|refused| refused.borrow_mut().clear());
            let _ = reread(Doc(doc));
            REFUSED.with(std::cell::RefCell::take)
        };
        let mut enums: BTreeMap<&'static [&'static str], (&Place, BTreeSet<String>)> =
            BTreeMap::new();
        for place in places.values() {
            let refusals = read_back(place, &|value| place.unknown(value));
            for refusal in refusals {
                let Refusal::Variants(variants) = refusal else {
                    continue;
                };
                let (_, took) = enums.entry(variants).or_insert((place, BTreeSet::new()));
                took.extend(
                    place
                        .wrote
                        .iter()
                        .filter(|wrote| variants.contains(&wrote.as_str()))
                        .cloned(),
                );
            }
        }
        let walked = self.walked_enums();
        let mut settled = Vec::new();
        for (variants, (place, took)) in &enums {
            let name = walked_name(&walked, took)
                .or_else(|| {
                    let tagged = place.tag_holder()?;
                    read_back(&tagged, &|value| *value = aterm_json::Value::Bool(false))
                        .into_iter()
                        .find_map(|refusal| match refusal {
                            Refusal::Expected(expected) => expected
                                .strip_prefix("internally tagged enum ")
                                .map(str::to_string),
                            Refusal::Variants(_) => None,
                        })
                })
                .unwrap_or_else(|| format!("{{{}}}", variants.join("|")));
            for variant in *variants {
                settled.push((format!("{name}::{variant}"), took.contains(*variant)));
            }
        }
        for (entry, taken) in settled {
            if taken {
                self.written.insert(entry);
            } else {
                self.passed.insert(entry);
            }
        }
    }

    /// Every enum the walk named, with the externally tagged variants it
    /// wrote (`Enum::Variant`, not a struct variant's `Enum::Variant.field`).
    fn walked_enums(&self) -> BTreeMap<&str, BTreeSet<&str>> {
        let mut walked: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for entry in &self.written {
            if let Some((name, variant)) = entry.split_once("::")
                && !variant.contains('.')
            {
                walked.entry(name).or_default().insert(variant);
            }
        }
        walked
    }

    /// The fields some value passed over and no value wrote.
    pub(crate) fn never_written(&self) -> Vec<&str> {
        self.passed
            .difference(&self.written)
            .map(String::as_str)
            .collect()
    }

    fn note_variant(&mut self, name: &str, variant: &str) {
        self.written.insert(format!("{name}::{variant}"));
    }

    fn note(&mut self, owner: &str, key: &str, wrote: bool) {
        let field = format!("{owner}.{key}");
        if wrote {
            self.written.insert(field);
        } else {
            self.passed.insert(field);
        }
    }
}

/// The file each release's desks record their fields in, beside the desks.
pub(crate) const FIELDS_FILE: &str = "fields.toml";

/// What a release's desks wrote, record by record: the manifest (`SessionHandoff`, its window, messages and held panes included), the layout sidecar (`RestoreManifest`), and the engine state of every carried screen (`CheckpointMeta`, the held panes' too).
#[derive(Default)]
pub(crate) struct DeskFields {
    manifest: FieldCover,
    layout: FieldCover,
    meta: FieldCover,
    control: FieldCover,
}

/// A release's `fields.toml`, read back.
#[derive(serde::Deserialize)]
pub(crate) struct Recorded {
    manifest: Lists,
    layout: Lists,
    meta: Lists,
    control: Lists,
}

/// One record's lists as a `fields.toml` holds them: what the hold reads back
/// is what its desks wrote (its `unwritten` list is for the reader).
#[derive(serde::Deserialize)]
struct Lists {
    written: Vec<String>,
}

impl DeskFields {
    /// Walk every desk under `release`: each directory holding a `parent.toml`.
    pub(crate) fn of_release(release: &Path) -> Self {
        let mut desks: Vec<PathBuf> = std::fs::read_dir(release)
            .unwrap_or_else(|e| panic!("read {}: {e}", release.display()))
            .flatten()
            .map(|entry| entry.path())
            .filter(|desk| desk.join("parent.toml").is_file())
            .collect();
        desks.sort();
        let mut fields = Self::default();
        for desk in &desks {
            fields.walk_desk(desk);
        }
        for cover in [
            &mut fields.manifest,
            &mut fields.layout,
            &mut fields.meta,
            &mut fields.control,
        ] {
            cover.settle_variants();
        }
        fields
    }

    fn walk_desk(&mut self, desk: &Path) {
        let entries =
            std::fs::read_dir(desk).unwrap_or_else(|e| panic!("read {}: {e}", desk.display()));
        for path in entries.flatten().map(|entry| entry.path()) {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            if !name.starts_with("seamless-") {
                continue;
            }
            let label = path.display();
            if name.ends_with(".ctl") {
                let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {label}: {e}"));
                let control: crate::handoff_carry::Wire =
                    aterm_json::from_slice(&bytes).unwrap_or_else(|e| panic!("{label}: {e}"));
                self.control.read(&control);
                continue;
            }
            if !name.ends_with(".toml") {
                continue;
            }
            let text =
                std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {label}: {e}"));
            if name.ends_with(".layout.toml") {
                let layout: RestoreManifest =
                    aterm_toml::from_str(&text).unwrap_or_else(|e| panic!("{label}: {e}"));
                self.layout.read(&layout);
                continue;
            }
            let (_, body) = text
                .split_once('\n')
                .unwrap_or_else(|| panic!("{label} opens with its nonce"));
            let manifest: SessionHandoff =
                aterm_toml::from_str(body).unwrap_or_else(|e| panic!("{label}: {e}"));
            self.manifest.read(&manifest);
            let screens = manifest
                .sessions
                .iter()
                .filter_map(|record| record.screen.as_ref())
                .chain(manifest.held.iter().map(|pane| &pane.screen));
            for screen in screens {
                let meta: CheckpointMeta = aterm_json::from_str(&screen.meta)
                    .unwrap_or_else(|e| panic!("{label}: a carried meta: {e}"));
                self.meta.read(&meta);
            }
        }
    }

    fn records(&self) -> [(&'static str, &FieldCover); 4] {
        [
            ("manifest", &self.manifest),
            ("layout", &self.layout),
            ("meta", &self.meta),
            ("control", &self.control),
        ]
    }

    /// The `fields.toml` recording these desks' fields.
    pub(crate) fn to_toml(&self) -> String {
        let mut out = String::from(
            "# The fields this release's desks wrote, and those no desk wrote (each one\n\
             # no handoff writes), record by record. Written by the generator\n\
             # (seamless_fixture_cover.rs); the next release's desks must write every\n\
             # field and variant listed as written. See ../README.md.\n",
        );
        for (record, cover) in self.records() {
            out.push_str(&format!("\n[{record}]\n"));
            for (key, fields) in [
                (
                    "written",
                    cover.written.iter().map(String::as_str).collect(),
                ),
                ("unwritten", cover.never_written()),
            ] {
                out.push_str(&format!("{key} = [\n"));
                for field in fields {
                    out.push_str(&format!("    \"{field}\",\n"));
                }
                out.push_str("]\n");
            }
        }
        out
    }

    /// What these desks leave uncovered, one `<record> <field> (<why>)` each:
    ///
    /// * every field or variant a desk here passes over and no desk writes, unless
    ///   `unhanded` names it, as `(record, field)`: one no handoff writes;
    /// * every field or variant `baseline`'s desks wrote that no desk here writes,
    ///   reached or not, unless `unhanded` names it (retired since);
    /// * every `unhanded` row of `manifest`, `layout`, `meta` or `control` that a desk
    ///   here writes, or that names no field a desk here passes over or `baseline`'s
    ///   desks wrote.
    pub(crate) fn uncovered(
        &self,
        baseline: Option<&Recorded>,
        unhanded: &[(&str, &str)],
    ) -> Vec<String> {
        let was = baseline.map(|baseline| {
            [
                &baseline.manifest,
                &baseline.layout,
                &baseline.meta,
                &baseline.control,
            ]
        });
        let mut uncovered = Vec::new();
        for (at, (record, cover)) in self.records().into_iter().enumerate() {
            let was_written: &[String] = was.map_or(&[][..], |was| was[at].written.as_slice());
            let listed = |field: &str| unhanded.contains(&(record, field));
            let never = cover.never_written();
            for field in &never {
                if !listed(field) {
                    uncovered.push(format!("{record} {field} (no desk writes it)"));
                }
            }
            for field in was_written {
                if !cover.written.contains(field) && !listed(field) {
                    uncovered.push(format!("{record} {field} (its desks wrote it)"));
                }
            }
            for &(_, field) in unhanded.iter().filter(|(owner, _)| *owner == record) {
                if cover.written.contains(field) {
                    uncovered.push(format!(
                        "{record} {field} (listed as no handoff's, but a desk writes it)"
                    ));
                } else if !never.contains(&field) && !was_written.iter().any(|was| was == field) {
                    uncovered.push(format!("{record} {field} (listed, but no desk reaches it)"));
                }
            }
        }
        uncovered
    }
}

/// The newest release under `root` that recorded its desks' fields (`vX.Y.Z/fields.toml`), by version, and what it recorded; never `this_run`, the directory the caller is writing (the hand-run fallback writes `v<release>/` under `root`), which would be its own baseline.
pub(crate) fn newest_recorded(root: &Path, this_run: &Path) -> Option<(String, Recorded)> {
    let this_run = std::fs::canonicalize(this_run).ok();
    let version = |name: &str| -> Option<Vec<u64>> {
        name.strip_prefix('v')?
            .split('.')
            .map(|part| part.parse().ok())
            .collect()
    };
    let (_, release, path) = std::fs::read_dir(root)
        .ok()?
        .flatten()
        .filter(|entry| this_run.is_none() || std::fs::canonicalize(entry.path()).ok() != this_run)
        .filter_map(|entry| {
            let release = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path().join(FIELDS_FILE);
            Some((version(&release)?, release, path)).filter(|(_, _, path)| path.is_file())
        })
        .max()?;
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let recorded =
        aterm_toml::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    Some((release, recorded))
}

/// How [`FieldCover::settle_variants`] reads a document back as its record's
/// type.
type Reread = fn(Doc) -> Result<(), Refused>;

fn reread<T: serde::de::DeserializeOwned>(doc: Doc) -> Result<(), Refused> {
    T::deserialize(doc).map(drop)
}

/// What [`FieldCover::settle_variants`] puts where a value wrote a string or a
/// key: a name no enum gives a variant.
const NO_VARIANT: &str = "\u{1}no variant";

thread_local! {
    /// Every refusal a read back made since it was last cleared, whether or
    /// not it reached the reader: an untagged or lenient field reads past one.
    static REFUSED: std::cell::RefCell<Vec<Refusal>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// A refusal [`FieldCover::settle_variants`] reads: an enum's variants, when
/// it refused [`NO_VARIANT`], and what a visitor expected, when it refused a
/// value of the wrong kind (an internally tagged enum says its own name).
enum Refusal {
    Variants(&'static [&'static str]),
    Expected(String),
}

/// The enum the walk named whose externally tagged variants it wrote are
/// exactly `took`, when one is.
fn walked_name(walked: &BTreeMap<&str, BTreeSet<&str>>, took: &BTreeSet<String>) -> Option<String> {
    let mut named = walked
        .iter()
        .filter(|(_, wrote)| wrote.iter().copied().eq(took.iter().map(String::as_str)))
        .map(|(name, _)| (*name).to_string());
    named
        .next()
        .filter(|_| named.next().is_none() && !took.is_empty())
}

/// A place a value wrote a string, or a table of one key, that
/// [`FieldCover::settle_variants`] reads back with [`NO_VARIANT`] there: the
/// first value that has it, the steps to it there, and what every value
/// wrote at it.
struct Place {
    doc: usize,
    steps: Vec<Step>,
    key: bool,
    wrote: BTreeSet<String>,
}

#[derive(Clone)]
enum Step {
    Key(String),
    At(usize),
}

impl Place {
    /// What `doc` holds at this place.
    fn at<'v>(&self, doc: &'v mut aterm_json::Value) -> &'v mut aterm_json::Value {
        let mut at = doc;
        for step in &self.steps {
            at = match (step, at) {
                (Step::Key(key), aterm_json::Value::Object(map)) => {
                    map.get_mut(key).expect("a step the walk took")
                }
                (Step::At(index), aterm_json::Value::Array(items)) => {
                    items.get_mut(*index).expect("a step the walk took")
                }
                _ => unreachable!("a step the walk took"),
            };
        }
        at
    }

    /// Put [`NO_VARIANT`] where this place's string or key was.
    fn unknown(&self, value: &mut aterm_json::Value) {
        match value {
            aterm_json::Value::Object(map) if self.key => {
                let only = map.pop_first().expect("a table of one key").1;
                map.insert(NO_VARIANT.to_string(), only);
            }
            aterm_json::Value::String(text) => *text = NO_VARIANT.to_string(),
            _ => unreachable!("a place holds a string or a table of one key"),
        }
    }

    /// The table this place's string is a field of: an internally tagged
    /// enum's, when the string is its tag.
    fn tag_holder(&self) -> Option<Place> {
        let (Step::Key(_), holder) = self.steps.split_last()? else {
            return None;
        };
        (!self.key).then(|| Place {
            doc: self.doc,
            steps: holder.to_vec(),
            key: false,
            wrote: BTreeSet::new(),
        })
    }
}

/// Every place in `value` (under `path`, its list positions erased) that
/// holds a string, or a table of one key, with what it holds.
fn places_in(
    value: &aterm_json::Value,
    doc: usize,
    steps: &mut Vec<Step>,
    path: &str,
    places: &mut BTreeMap<String, Place>,
) {
    let mut note = |place: String, key: bool, wrote: &str, steps: &[Step]| {
        places
            .entry(place)
            .or_insert_with(|| Place {
                doc,
                steps: steps.to_vec(),
                key,
                wrote: BTreeSet::new(),
            })
            .wrote
            .insert(wrote.to_string());
    };
    match value {
        aterm_json::Value::String(text) => note(format!("{path}="), false, text, steps),
        aterm_json::Value::Array(items) => {
            let path = format!("{path}[]");
            for (index, item) in items.iter().enumerate() {
                steps.push(Step::At(index));
                places_in(item, doc, steps, &path, places);
                steps.pop();
            }
        }
        aterm_json::Value::Object(map) => {
            if let Some((only, _)) = map.first_key_value().filter(|_| map.len() == 1) {
                note(format!("{path}:"), true, only, steps);
            }
            for (key, item) in map {
                steps.push(Step::Key(key.clone()));
                places_in(item, doc, steps, &format!("{path}.{key}"), places);
                steps.pop();
            }
        }
        aterm_json::Value::Null | aterm_json::Value::Bool(_) | aterm_json::Value::Number(_) => {}
    }
}

/// A refused read back: why, for the read that must not fail.
#[derive(Debug)]
struct Refused(String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

impl serde::de::Error for Refused {
    fn custom<T: std::fmt::Display>(why: T) -> Self {
        Self(why.to_string())
    }

    fn unknown_variant(variant: &str, expected: &'static [&'static str]) -> Self {
        if variant == NO_VARIANT {
            REFUSED.with(|refused| refused.borrow_mut().push(Refusal::Variants(expected)));
        }
        Self(format!("unknown variant `{variant}`"))
    }

    fn invalid_type(
        unexpected: serde::de::Unexpected<'_>,
        expected: &dyn serde::de::Expected,
    ) -> Self {
        let expected = expected.to_string();
        let why = format!("invalid type: {unexpected}, expected {expected}");
        REFUSED.with(|refused| refused.borrow_mut().push(Refusal::Expected(expected)));
        Self(why)
    }
}

/// A document read back as a record's type, with [`Refused`] as the error
/// every derive inside it builds its refusals with.
struct Doc(aterm_json::Value);

/// What a document holds, as a refusal names it.
fn unexpected(value: &aterm_json::Value) -> serde::de::Unexpected<'_> {
    use serde::de::Unexpected;
    match value {
        aterm_json::Value::Null => Unexpected::Unit,
        aterm_json::Value::Bool(value) => Unexpected::Bool(*value),
        aterm_json::Value::Number(_) => Unexpected::Other("a number"),
        aterm_json::Value::String(text) => Unexpected::Str(text),
        aterm_json::Value::Array(_) => Unexpected::Seq,
        aterm_json::Value::Object(_) => Unexpected::Map,
    }
}

/// A table's key, read as the type its map is keyed by (a number is a string
/// in JSON).
struct Key(String);

/// A list's or a table's entries, read one by one.
struct Entries<I>(I, Option<aterm_json::Value>);

/// An externally tagged variant written as a table of one key.
struct Variant(String, aterm_json::Value);

impl<'de> serde::de::IntoDeserializer<'de, Refused> for Doc {
    type Deserializer = Self;
    fn into_deserializer(self) -> Self {
        self
    }
}

impl<'de> serde::de::Deserializer<'de> for Doc {
    type Error = Refused;

    fn deserialize_any<V: serde::de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, Refused> {
        use aterm_json::Value;
        match self.0 {
            Value::Null => visitor.visit_unit(),
            Value::Bool(value) => visitor.visit_bool(value),
            Value::Number(number) => match (number.as_u64(), number.as_i64(), number.as_f64()) {
                (Some(value), _, _) => visitor.visit_u64(value),
                (None, Some(value), _) => visitor.visit_i64(value),
                (None, None, value) => visitor.visit_f64(value.unwrap_or_default()),
            },
            Value::String(text) => visitor.visit_string(text),
            Value::Array(items) => visitor.visit_seq(Entries(items.into_iter(), None)),
            Value::Object(map) => visitor.visit_map(Entries(map.into_iter(), None)),
        }
    }

    fn deserialize_option<V: serde::de::Visitor<'de>>(
        self,
        visitor: V,
    ) -> Result<V::Value, Refused> {
        match self.0 {
            aterm_json::Value::Null => visitor.visit_none(),
            value => visitor.visit_some(Doc(value)),
        }
    }

    fn deserialize_newtype_struct<V: serde::de::Visitor<'de>>(
        self,
        _: &'static str,
        visitor: V,
    ) -> Result<V::Value, Refused> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_enum<V: serde::de::Visitor<'de>>(
        self,
        _: &'static str,
        _: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Refused> {
        match self.0 {
            aterm_json::Value::String(text) => {
                visitor.visit_enum(serde::de::value::StrDeserializer::<Refused>::new(&text))
            }
            aterm_json::Value::Object(mut map) if map.len() == 1 => {
                let (variant, value) = map.pop_first().expect("a table of one key");
                visitor.visit_enum(Variant(variant, value))
            }
            other => Err(serde::de::Error::invalid_type(unexpected(&other), &visitor)),
        }
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf unit unit_struct seq tuple tuple_struct map struct
        identifier ignored_any
    }
}

impl<'de, I: Iterator<Item = aterm_json::Value>> serde::de::SeqAccess<'de> for Entries<I> {
    type Error = Refused;
    fn next_element_seed<T: serde::de::DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Refused> {
        self.0
            .next()
            .map(|item| seed.deserialize(Doc(item)))
            .transpose()
    }
}

impl<'de, I: Iterator<Item = (String, aterm_json::Value)>> serde::de::MapAccess<'de>
    for Entries<I>
{
    type Error = Refused;
    fn next_key_seed<K: serde::de::DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Refused> {
        let Some((key, value)) = self.0.next() else {
            return Ok(None);
        };
        self.1 = Some(value);
        seed.deserialize(Key(key)).map(Some)
    }

    fn next_value_seed<V: serde::de::DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> Result<V::Value, Refused> {
        let value = self.1.take().expect("a value follows its key");
        seed.deserialize(Doc(value))
    }
}

impl<'de> serde::de::EnumAccess<'de> for Variant {
    type Error = Refused;
    type Variant = Doc;
    fn variant_seed<V: serde::de::DeserializeSeed<'de>>(
        self,
        seed: V,
    ) -> Result<(V::Value, Doc), Refused> {
        let variant = seed.deserialize(Key(self.0))?;
        Ok((variant, Doc(self.1)))
    }
}

impl<'de> serde::de::VariantAccess<'de> for Doc {
    type Error = Refused;
    fn unit_variant(self) -> Result<(), Refused> {
        Ok(())
    }
    fn newtype_variant_seed<T: serde::de::DeserializeSeed<'de>>(
        self,
        seed: T,
    ) -> Result<T::Value, Refused> {
        seed.deserialize(self)
    }
    fn tuple_variant<V: serde::de::Visitor<'de>>(
        self,
        _: usize,
        visitor: V,
    ) -> Result<V::Value, Refused> {
        serde::de::Deserializer::deserialize_any(self, visitor)
    }
    fn struct_variant<V: serde::de::Visitor<'de>>(
        self,
        _: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Refused> {
        serde::de::Deserializer::deserialize_any(self, visitor)
    }
}

/// A key read as a number when its map is keyed by one.
macro_rules! numbered_keys {
    ($($method:ident: $as:ty => $visit:ident),* $(,)?) => {$(
        fn $method<V: serde::de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, Refused> {
            match self.0.parse::<$as>() {
                Ok(number) => visitor.$visit(number),
                Err(_) => visitor.visit_string(self.0),
            }
        }
    )*};
}

impl<'de> serde::de::Deserializer<'de> for Key {
    type Error = Refused;

    fn deserialize_any<V: serde::de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, Refused> {
        visitor.visit_string(self.0)
    }

    numbered_keys! {
        deserialize_u8: u64 => visit_u64,
        deserialize_u16: u64 => visit_u64,
        deserialize_u32: u64 => visit_u64,
        deserialize_u64: u64 => visit_u64,
        deserialize_i8: i64 => visit_i64,
        deserialize_i16: i64 => visit_i64,
        deserialize_i32: i64 => visit_i64,
        deserialize_i64: i64 => visit_i64,
    }

    fn deserialize_option<V: serde::de::Visitor<'de>>(
        self,
        visitor: V,
    ) -> Result<V::Value, Refused> {
        visitor.visit_some(self)
    }

    fn deserialize_newtype_struct<V: serde::de::Visitor<'de>>(
        self,
        _: &'static str,
        visitor: V,
    ) -> Result<V::Value, Refused> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_enum<V: serde::de::Visitor<'de>>(
        self,
        _: &'static str,
        _: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Refused> {
        visitor.visit_enum(serde::de::value::StrDeserializer::<Refused>::new(&self.0))
    }

    serde::forward_to_deserialize_any! {
        bool i128 u128 f32 f64 char str string bytes byte_buf unit unit_struct
        seq tuple tuple_struct map struct identifier ignored_any
    }
}

/// A walk writes nothing, so nothing fails; serde asks for an error type.
#[derive(Debug)]
struct Error;

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a field walk failed")
    }
}

impl std::error::Error for Error {}

impl serde::ser::Error for Error {
    fn custom<T: std::fmt::Display>(_: T) -> Self {
        Self
    }
}

/// The serializer: each value answers whether it wrote anything (`false` for
/// `None`, an empty list or an empty table).
struct Walk<'a> {
    cover: &'a mut FieldCover,
}

/// A list, table or struct being walked: who owns its fields, and whether it
/// has written an element or entry yet.
struct Compound<'a> {
    cover: &'a mut FieldCover,
    owner: String,
    wrote: bool,
}

impl<'a> Walk<'a> {
    fn compound(self, owner: String) -> Compound<'a> {
        Compound {
            cover: self.cover,
            owner,
            wrote: false,
        }
    }
}

impl Compound<'_> {
    fn element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        value.serialize(Walk {
            cover: &mut *self.cover,
        })?;
        self.wrote = true;
        Ok(())
    }

    fn field<T: Serialize + ?Sized>(&mut self, key: &str, value: &T) -> Result<(), Error> {
        let wrote = value.serialize(Walk {
            cover: &mut *self.cover,
        })?;
        self.cover.note(&self.owner, key, wrote);
        Ok(())
    }
}

impl<'a> Serializer for Walk<'a> {
    type Ok = bool;
    type Error = Error;
    type SerializeSeq = Compound<'a>;
    type SerializeTuple = Compound<'a>;
    type SerializeTupleStruct = Compound<'a>;
    type SerializeTupleVariant = Compound<'a>;
    type SerializeMap = Compound<'a>;
    type SerializeStruct = Compound<'a>;
    type SerializeStructVariant = Compound<'a>;

    fn serialize_bool(self, _: bool) -> Result<bool, Error> {
        Ok(true)
    }
    fn serialize_i8(self, _: i8) -> Result<bool, Error> {
        Ok(true)
    }
    fn serialize_i16(self, _: i16) -> Result<bool, Error> {
        Ok(true)
    }
    fn serialize_i32(self, _: i32) -> Result<bool, Error> {
        Ok(true)
    }
    fn serialize_i64(self, _: i64) -> Result<bool, Error> {
        Ok(true)
    }
    fn serialize_u8(self, _: u8) -> Result<bool, Error> {
        Ok(true)
    }
    fn serialize_u16(self, _: u16) -> Result<bool, Error> {
        Ok(true)
    }
    fn serialize_u32(self, _: u32) -> Result<bool, Error> {
        Ok(true)
    }
    fn serialize_u64(self, _: u64) -> Result<bool, Error> {
        Ok(true)
    }
    fn serialize_f32(self, _: f32) -> Result<bool, Error> {
        Ok(true)
    }
    fn serialize_f64(self, _: f64) -> Result<bool, Error> {
        Ok(true)
    }
    fn serialize_char(self, _: char) -> Result<bool, Error> {
        Ok(true)
    }
    fn serialize_str(self, _: &str) -> Result<bool, Error> {
        Ok(true)
    }
    fn serialize_bytes(self, bytes: &[u8]) -> Result<bool, Error> {
        Ok(!bytes.is_empty())
    }
    fn serialize_none(self) -> Result<bool, Error> {
        Ok(false)
    }
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<bool, Error> {
        value.serialize(self)?;
        Ok(true)
    }
    fn serialize_unit(self) -> Result<bool, Error> {
        Ok(true)
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<bool, Error> {
        Ok(true)
    }
    fn serialize_unit_variant(
        self,
        name: &'static str,
        _: u32,
        variant: &'static str,
    ) -> Result<bool, Error> {
        self.cover.note_variant(name, variant);
        Ok(true)
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        value: &T,
    ) -> Result<bool, Error> {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        name: &'static str,
        _: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<bool, Error> {
        self.cover.note_variant(name, variant);
        value.serialize(self)?;
        Ok(true)
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Compound<'a>, Error> {
        Ok(self.compound(String::new()))
    }
    fn serialize_tuple(self, _: usize) -> Result<Compound<'a>, Error> {
        Ok(self.compound(String::new()))
    }
    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Compound<'a>, Error> {
        Ok(self.compound(String::new()))
    }
    fn serialize_tuple_variant(
        self,
        name: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<Compound<'a>, Error> {
        self.cover.note_variant(name, variant);
        Ok(self.compound(String::new()))
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Compound<'a>, Error> {
        Ok(self.compound(String::new()))
    }
    fn serialize_struct(self, name: &'static str, _: usize) -> Result<Compound<'a>, Error> {
        Ok(self.compound(name.to_string()))
    }
    fn serialize_struct_variant(
        self,
        name: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<Compound<'a>, Error> {
        self.cover.note_variant(name, variant);
        Ok(self.compound(format!("{name}::{variant}")))
    }
}

impl SerializeSeq for Compound<'_> {
    type Ok = bool;
    type Error = Error;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.element(value)
    }
    fn end(self) -> Result<bool, Error> {
        Ok(self.wrote)
    }
}

impl SerializeTuple for Compound<'_> {
    type Ok = bool;
    type Error = Error;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.element(value)
    }
    fn end(self) -> Result<bool, Error> {
        Ok(true)
    }
}

impl SerializeTupleStruct for Compound<'_> {
    type Ok = bool;
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.element(value)
    }
    fn end(self) -> Result<bool, Error> {
        Ok(true)
    }
}

impl SerializeTupleVariant for Compound<'_> {
    type Ok = bool;
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.element(value)
    }
    fn end(self) -> Result<bool, Error> {
        Ok(true)
    }
}

impl SerializeMap for Compound<'_> {
    type Ok = bool;
    type Error = Error;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), Error> {
        self.element(key)
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.element(value)
    }
    fn end(self) -> Result<bool, Error> {
        Ok(self.wrote)
    }
}

impl SerializeStruct for Compound<'_> {
    type Ok = bool;
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        self.field(key, value)
    }
    fn skip_field(&mut self, key: &'static str) -> Result<(), Error> {
        self.cover.note(&self.owner, key, false);
        Ok(())
    }
    fn end(self) -> Result<bool, Error> {
        Ok(true)
    }
}

impl SerializeStructVariant for Compound<'_> {
    type Ok = bool;
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        self.field(key, value)
    }
    fn skip_field(&mut self, key: &'static str) -> Result<(), Error> {
        self.cover.note(&self.owner, key, false);
        Ok(())
    }
    fn end(self) -> Result<bool, Error> {
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::{DeskFields, FieldCover, Recorded};

    #[derive(serde::Serialize)]
    struct Inner {
        #[serde(skip_serializing_if = "Option::is_none")]
        hint: Option<u32>,
        always: u8,
    }

    #[derive(serde::Serialize)]
    struct Outer {
        #[serde(skip_serializing_if = "Vec::is_empty")]
        rows: Vec<Inner>,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        flag: bool,
        maybe: Option<String>,
        empty: Vec<u8>,
    }

    #[derive(serde::Serialize)]
    enum Shape {
        Line,
        Boxed(Inner),
    }

    #[derive(serde::Serialize)]
    struct Desk {
        shapes: Vec<Shape>,
    }

    /// A field or variant a baseline's desks wrote is held whether or not a
    /// desk still reaches its struct: a desk that stops carrying a shape stops
    /// writing every field and variant only it wrote.
    #[test]
    fn what_a_baseline_wrote_stays_written_reached_or_not() {
        let walked = |desk: &Desk| {
            let mut fields = DeskFields::default();
            fields.manifest.walk(desk);
            fields
        };
        let both = walked(&Desk {
            shapes: vec![
                Shape::Line,
                Shape::Boxed(Inner {
                    hint: Some(1),
                    always: 1,
                }),
            ],
        });
        let baseline: Recorded = aterm_toml::from_str(&both.to_toml()).expect("its own record");
        assert!(both.uncovered(Some(&baseline), &[]).is_empty());
        let line_only = walked(&Desk {
            shapes: vec![Shape::Line],
        });
        assert_eq!(
            line_only.uncovered(Some(&baseline), &[]),
            [
                "manifest Inner.always (its desks wrote it)",
                "manifest Inner.hint (its desks wrote it)",
                "manifest Shape::Boxed (its desks wrote it)",
            ]
        );
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    enum Mode {
        Plain,
        Fancy,
        Wild,
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    enum View {
        Text { mode: Mode },
        Picture { width: u32 },
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    enum Shade {
        Light,
        Dark,
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    struct Pane {
        view: View,
        #[serde(deserialize_with = "lenient_shade")]
        shade: Option<Shade>,
    }

    /// A shade, or none when the value is not one, like the engine state's
    /// lenient fields: a refusal inside never reaches the reader.
    fn lenient_shade<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Shade>, D::Error> {
        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Shade(Shade),
            Unreadable(serde::de::IgnoredAny),
        }
        Ok(match <Wire as serde::Deserialize>::deserialize(d)? {
            Wire::Shade(shade) => Some(shade),
            Wire::Unreadable(_) => None,
        })
    }

    /// Every variant of every enum a value reaches is named, taken or not:
    /// an externally tagged one's inside an internally tagged enum's buffered
    /// content, an internally tagged one's under its own name, and one a
    /// lenient field reads a refusal past.
    #[test]
    fn every_variant_of_an_enum_a_value_reaches_is_named_taken_or_not() {
        let mut cover = FieldCover::default();
        cover.read(&Pane {
            view: View::Text { mode: Mode::Plain },
            shade: Some(Shade::Light),
        });
        cover.settle_variants();
        assert_eq!(
            cover.never_written(),
            ["Mode::Fancy", "Mode::Wild", "Shade::Dark", "View::picture"]
        );
        assert!(cover.written.contains("View::text"));
        cover.read(&Pane {
            view: View::Picture { width: 3 },
            shade: Some(Shade::Dark),
        });
        cover.settle_variants();
        assert_eq!(cover.never_written(), ["Mode::Fancy", "Mode::Wild"]);
    }

    /// A field no desk writes is refused unless it is named as one no handoff
    /// writes, whatever the baseline, and a name that is no longer true (a
    /// desk writes the field, or nothing has it) is refused too.
    #[test]
    fn a_field_no_desk_writes_is_refused_unless_no_handoff_writes_it() {
        let walked = |hint: Option<u32>| {
            let mut fields = DeskFields::default();
            fields.manifest.walk(&Desk {
                shapes: vec![Shape::Boxed(Inner { hint, always: 1 })],
            });
            fields
        };
        let unwritten = walked(None);
        assert_eq!(
            unwritten.uncovered(None, &[]),
            ["manifest Inner.hint (no desk writes it)"]
        );
        let baseline: Recorded =
            aterm_toml::from_str(&unwritten.to_toml()).expect("its own record");
        assert_eq!(
            unwritten.uncovered(Some(&baseline), &[]),
            ["manifest Inner.hint (no desk writes it)"],
            "a baseline that left it unwritten does not excuse it"
        );
        assert!(
            unwritten
                .uncovered(Some(&baseline), &[("manifest", "Inner.hint")])
                .is_empty()
        );
        assert_eq!(
            walked(Some(7)).uncovered(Some(&baseline), &[("manifest", "Inner.hint")]),
            ["manifest Inner.hint (listed as no handoff's, but a desk writes it)"]
        );
        assert_eq!(
            unwritten.uncovered(
                Some(&baseline),
                &[("manifest", "Inner.hint"), ("manifest", "Inner.gone")]
            ),
            ["manifest Inner.gone (listed, but no desk reaches it)"]
        );
    }

    /// The hand-run fallback writes `v<release>/` under the fixtures' root:
    /// its own record is never its baseline, the release before it is.
    #[test]
    fn a_release_being_recorded_is_never_its_own_baseline() {
        let root = aterm_tempfile::tempdir().expect("a fixtures root");
        for release in ["v0.107.0", "v0.108.0"] {
            let dir = root.path().join(release);
            std::fs::create_dir(&dir).expect("a release directory");
            std::fs::write(
                dir.join(super::FIELDS_FILE),
                DeskFields::default().to_toml(),
            )
            .expect("its record");
        }
        let newest = |this_run: &std::path::Path| {
            super::newest_recorded(root.path(), this_run).map(|(release, _)| release)
        };
        assert_eq!(
            newest(&root.path().join("v0.108.0")).as_deref(),
            Some("v0.107.0")
        );
        assert_eq!(
            newest(&root.path().join("elsewhere")).as_deref(),
            Some("v0.108.0")
        );
    }

    #[test]
    fn a_field_no_value_writes_is_named_and_one_any_value_writes_is_not() {
        let mut cover = FieldCover::default();
        cover.walk(&Outer {
            rows: vec![Inner {
                hint: None,
                always: 1,
            }],
            flag: false,
            maybe: Some(String::new()),
            empty: Vec::new(),
        });
        assert_eq!(
            cover.never_written(),
            ["Inner.hint", "Outer.empty", "Outer.flag"]
        );
        cover.walk(&Outer {
            rows: vec![Inner {
                hint: Some(3),
                always: 2,
            }],
            flag: true,
            maybe: None,
            empty: vec![1],
        });
        assert!(
            cover.never_written().is_empty(),
            "{:?}",
            cover.never_written()
        );
    }
}
