// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde_derive 1.0.228 src/internals/attr.rs and
// src/internals/name.rs, and modified by the aterm project in 2026: the
// attributes are read with aterm-macro-support instead of syn, and only the
// keys, positions and forms in docs/measured/third-party-2026-10-15/serde-attrs.tsv
// are implemented. Every other key is an error that says so.
// See ../LICENSE-MIT and ../LICENSE-APACHE for the upstream licenses.

//! This module handles parsing of `#[serde(...)]` attributes. The entrypoints
//! are `attr::Container::from_ast`, `attr::Variant::from_ast`, and
//! `attr::Field::from_ast`. Each returns an instance of the corresponding
//! struct. Note that none of them return a Result. Unrecognized, malformed, or
//! duplicated attributes result in an error but otherwise are ignored. The
//! user will see errors simultaneously for all bad attributes in the crate
//! rather than just the first.
//!
//! The set aterm writes (container: `default`, `deny_unknown_fields`,
//! `rename_all`, `tag`, `transparent`, `untagged`; variant: `rename`; field:
//! `alias`, `default`, `default = "..."`, `deserialize_with`, `flatten`,
//! `rename`, `skip`, `skip_serializing_if`) is closed: a key upstream serde
//! knows but this derive does not is refused by name, as is a known key in a
//! position or form outside the set.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Display};

use aterm_macro_support::lit::{StrLit, parse_expr_path, str_literal};
use aterm_macro_support::meta::{Meta, MetaKind, parse_list, value_str};
use aterm_macro_support::{
    AttrArgs, Attribute, Data, DeriveInput, Errors, Fields, GenericArg, Lifetime, PathArgs,
    ToTokens, Ty, TyPath,
};
use proc_macro::{Delimiter, Ident, Span, TokenStream, TokenTree};

pub use crate::case::RenameRule;
use crate::case::{RENAME_RULES, UPSTREAM_ONLY_RULES};

/// Where the attributes are used, so error messages can say.
#[derive(Copy, Clone)]
enum Position {
    Container,
    Variant,
    Field,
}

impl Position {
    fn word(self) -> &'static str {
        match self {
            Position::Container => "container",
            Position::Variant => "variant",
            Position::Field => "field",
        }
    }
}

/// Keys upstream serde 1.0.228 accepts in each position, so a key outside
/// aterm's set but inside serde's is refused by name rather than as unknown.
const UPSTREAM_CONTAINER: &[&str] = &[
    "rename",
    "rename_all",
    "rename_all_fields",
    "transparent",
    "deny_unknown_fields",
    "default",
    "bound",
    "untagged",
    "tag",
    "content",
    "from",
    "try_from",
    "into",
    "remote",
    "field_identifier",
    "variant_identifier",
    "crate",
    "expecting",
];
const UPSTREAM_VARIANT: &[&str] = &[
    "rename",
    "alias",
    "rename_all",
    "skip",
    "skip_deserializing",
    "skip_serializing",
    "other",
    "bound",
    "with",
    "serialize_with",
    "deserialize_with",
    "borrow",
    "untagged",
];
const UPSTREAM_FIELD: &[&str] = &[
    "rename",
    "alias",
    "default",
    "skip_serializing",
    "skip_deserializing",
    "skip",
    "skip_serializing_if",
    "serialize_with",
    "deserialize_with",
    "with",
    "bound",
    "borrow",
    "getter",
    "flatten",
];

/// The error for a key this derive does not implement in this position.
fn unsupported(cx: &Errors, position: Position, meta: &Meta) {
    let key = meta.key();
    let upstream = match position {
        Position::Container => UPSTREAM_CONTAINER,
        Position::Variant => UPSTREAM_VARIANT,
        Position::Field => UPSTREAM_FIELD,
    };
    if upstream.contains(&key.as_str()) {
        cx.push(meta.key_error(format!(
            "#[serde({key})] on a {} is not in aterm's serde attribute set; \
             aterm-serde-derive implements only the keys and forms \
             docs/measured/third-party-2026-10-15/serde-attrs.tsv lists",
            position.word()
        )));
    } else {
        cx.push(meta.key_error(format!(
            "unknown serde {} attribute `{key}`",
            position.word()
        )));
    }
}

/// The error for an implemented key written in a form outside the set.
fn unsupported_form(cx: &Errors, position: Position, meta: &Meta, form: &str) {
    cx.push(meta.error(format!(
        "#[serde({})] on a {} must be written {form}: no other form is in aterm's \
         serde attribute set (docs/measured/third-party-2026-10-15/serde-attrs.tsv)",
        meta.key(),
        position.word()
    )));
}

/// A flag: `#[serde(key)]` with nothing after the key.
fn expect_flag(cx: &Errors, position: Position, meta: &Meta) -> bool {
    if let MetaKind::Flag = meta.kind {
        true
    } else {
        unsupported_form(
            cx,
            position,
            meta,
            &format!("bare, `#[serde({})]`", meta.key()),
        );
        false
    }
}

/// `#[serde(key = "...")]`: the string and its span.
fn get_lit_str(cx: &Errors, position: Position, meta: &Meta) -> Option<(String, Span)> {
    let key = meta.key();
    let value = match &meta.kind {
        MetaKind::Value { value, .. } => value,
        MetaKind::Flag | MetaKind::List(_) => {
            unsupported_form(cx, position, meta, &format!("`{key} = \"...\"`"));
            return None;
        }
    };
    match value_str(value) {
        Some((StrLit { value, suffix }, span)) => {
            if !suffix.is_empty() {
                cx.at(
                    span,
                    format!("unexpected suffix `{suffix}` on string literal"),
                );
            }
            Some((value, span))
        }
        None => {
            let start = value.first().map_or_else(Span::call_site, TokenTree::span);
            let end = value.last().map_or(start, TokenTree::span);
            cx.spanned(
                start,
                end,
                format!("expected serde {key} attribute to be a string: `{key} = \"...\"`"),
            );
            None
        }
    }
}

/// `#[serde(key = "path::to::f")]`: the path's tokens, spanned at the string.
fn parse_lit_into_expr_path(cx: &Errors, position: Position, meta: &Meta) -> Option<TokenStream> {
    let (string, span) = get_lit_str(cx, position, meta)?;
    match parse_expr_path(&string, span) {
        Some(path) => Some(path),
        None => {
            cx.at(span, format!("failed to parse path: {string:?}"));
            None
        }
    }
}

/// The `#[serde(...)]` lists among `attrs`, each parsed into its items.
fn serde_metas(cx: &Errors, attrs: &[Attribute]) -> Vec<Meta> {
    let mut out = Vec::new();
    for attr in attrs {
        if !attr.is("serde") {
            continue;
        }
        match &attr.args {
            AttrArgs::List(group) if group.delimiter() == Delimiter::Parenthesis => {
                match parse_list(group) {
                    Ok(items) => out.extend(items),
                    Err(err) => cx.push(err),
                }
            }
            _ => cx.spanned(
                attr.first_span(),
                attr.last_span(),
                "expected attribute arguments in parentheses: #[serde(...)]",
            ),
        }
    }
    out
}

pub(crate) struct Attr<'c, T> {
    cx: &'c Errors,
    name: &'static str,
    value: Option<T>,
}

impl<'c, T> Attr<'c, T> {
    fn none(cx: &'c Errors, name: &'static str) -> Self {
        Attr {
            cx,
            name,
            value: None,
        }
    }

    fn set(&mut self, meta: &Meta, value: T) {
        if self.value.is_some() {
            let msg = format!("duplicate serde attribute `{}`", self.name);
            self.cx.push(meta.key_error(msg));
        } else {
            self.value = Some(value);
        }
    }

    fn set_if_none(&mut self, value: T) {
        if self.value.is_none() {
            self.value = Some(value);
        }
    }

    pub(crate) fn get(self) -> Option<T> {
        self.value
    }
}

struct BoolAttr<'c>(Attr<'c, ()>);

impl<'c> BoolAttr<'c> {
    fn none(cx: &'c Errors, name: &'static str) -> Self {
        BoolAttr(Attr::none(cx, name))
    }

    fn set_true(&mut self, meta: &Meta) {
        self.0.set(meta, ());
    }

    fn get(&self) -> bool {
        self.0.value.is_some()
    }
}

fn unraw(ident: &Ident) -> Name {
    let text = ident.to_string();
    Name {
        value: text.strip_prefix("r#").unwrap_or(&text).to_owned(),
        span: ident.span(),
    }
}

/// A name as it appears in the data, with the span it came from (the
/// identifier, or the string literal that renamed it).
#[derive(Clone)]
pub struct Name {
    pub value: String,
    pub span: Span,
}

impl ToTokens for Name {
    fn to_tokens(&self, out: &mut Vec<TokenTree>) {
        out.push(TokenTree::Literal(str_literal(&self.value, self.span)));
    }
}

impl Ord for Name {
    fn cmp(&self, other: &Self) -> Ordering {
        Ord::cmp(&self.value, &other.value)
    }
}

impl PartialOrd for Name {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(Ord::cmp(self, other))
    }
}

impl Eq for Name {}

impl PartialEq for Name {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl Display for Name {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        Display::fmt(&self.value, formatter)
    }
}

pub struct MultiName {
    pub(crate) serialize: Name,
    pub(crate) serialize_renamed: bool,
    pub(crate) deserialize: Name,
    pub(crate) deserialize_renamed: bool,
    pub(crate) deserialize_aliases: BTreeSet<Name>,
}

impl MultiName {
    fn from_attrs(
        source_name: Name,
        ser_name: Attr<Name>,
        de_name: Attr<Name>,
        de_aliases: Vec<Name>,
    ) -> Self {
        let alias_set: BTreeSet<Name> = de_aliases.into_iter().collect();
        let ser_name = ser_name.get();
        let ser_renamed = ser_name.is_some();
        let de_name = de_name.get();
        let de_renamed = de_name.is_some();
        MultiName {
            serialize: ser_name.unwrap_or_else(|| source_name.clone()),
            serialize_renamed: ser_renamed,
            deserialize: de_name.unwrap_or(source_name),
            deserialize_renamed: de_renamed,
            deserialize_aliases: alias_set,
        }
    }

    /// Return the container name for the container when serializing.
    pub fn serialize_name(&self) -> &Name {
        &self.serialize
    }

    /// Return the container name for the container when deserializing.
    pub fn deserialize_name(&self) -> &Name {
        &self.deserialize
    }

    fn deserialize_aliases(&self) -> &BTreeSet<Name> {
        &self.deserialize_aliases
    }
}

/// Represents struct or enum attribute information.
pub struct Container {
    name: MultiName,
    transparent: bool,
    deny_unknown_fields: bool,
    default: Default,
    rename_all_rule: RenameRule,
    tag: TagType,
    is_packed: bool,
}

/// Styles of representing an enum.
pub enum TagType {
    /// The default.
    ///
    /// ```json
    /// {"variant1": {"key1": "value1", "key2": "value2"}}
    /// ```
    External,

    /// `#[serde(tag = "type")]`
    ///
    /// ```json
    /// {"type": "variant1", "key1": "value1", "key2": "value2"}
    /// ```
    Internal { tag: String },

    /// `#[serde(untagged)]`
    ///
    /// ```json
    /// {"key1": "value1", "key2": "value2"}
    /// ```
    None,
}

impl Container {
    /// Extract out the `#[serde(...)]` attributes from an item.
    pub fn from_ast(cx: &Errors, item: &DeriveInput) -> Self {
        let mut transparent = BoolAttr::none(cx, "transparent");
        let mut deny_unknown_fields = BoolAttr::none(cx, "deny_unknown_fields");
        let mut default = Attr::none(cx, "default");
        let mut rename_all_rule = Attr::none(cx, "rename_all");
        let mut untagged = BoolAttr::none(cx, "untagged");
        let mut internal_tag = Attr::none(cx, "tag");
        let position = Position::Container;

        for meta in serde_metas(cx, &item.attrs) {
            if meta.is("transparent") {
                // #[serde(transparent)]
                if expect_flag(cx, position, &meta) {
                    transparent.set_true(&meta);
                }
            } else if meta.is("deny_unknown_fields") {
                // #[serde(deny_unknown_fields)]
                if expect_flag(cx, position, &meta) {
                    deny_unknown_fields.set_true(&meta);
                }
            } else if meta.is("default") {
                // #[serde(default)]; `default = "..."` on a container is not in
                // aterm's set.
                if expect_flag(cx, position, &meta) {
                    match &item.data {
                        Data::Struct(Fields::Named(_) | Fields::Unnamed(_)) => {
                            default.set(&meta, Default::Default);
                        }
                        Data::Struct(Fields::Unit) => {
                            let msg =
                                "#[serde(default)] can only be used on structs that have fields";
                            cx.push(meta.error(msg));
                        }
                        Data::Enum(_) | Data::Union(_) => {
                            let msg = "#[serde(default)] can only be used on structs";
                            cx.push(meta.error(msg));
                        }
                    }
                }
            } else if meta.is("rename_all") {
                // #[serde(rename_all = "foo")]
                if let Some((value, span)) = get_lit_str(cx, position, &meta) {
                    match RenameRule::from_str(&value) {
                        Some(rule) => rename_all_rule.set(&meta, rule),
                        None if UPSTREAM_ONLY_RULES.contains(&value.as_str()) => cx.at(
                            span,
                            format!(
                                "rename rule `rename_all = {value:?}` is not in aterm's serde \
                                 attribute set; expected one of {}",
                                rule_list()
                            ),
                        ),
                        None => cx.at(
                            span,
                            format!(
                                "unknown rename rule `rename_all = {value:?}`, expected one of {}",
                                rule_list()
                            ),
                        ),
                    }
                }
            } else if meta.is("untagged") {
                // #[serde(untagged)]
                if expect_flag(cx, position, &meta) {
                    match item.data {
                        Data::Enum(_) => untagged.set_true(&meta),
                        Data::Struct(_) | Data::Union(_) => {
                            let msg = "#[serde(untagged)] can only be used on enums";
                            cx.push(meta.error(msg));
                        }
                    }
                }
            } else if meta.is("tag") {
                // #[serde(tag = "type")]
                if let Some((value, _)) = get_lit_str(cx, position, &meta) {
                    match &item.data {
                        Data::Enum(_) | Data::Struct(Fields::Named(_)) => {
                            internal_tag.set(&meta, value);
                        }
                        Data::Struct(Fields::Unnamed(_) | Fields::Unit) | Data::Union(_) => {
                            let msg = "#[serde(tag = \"...\")] can only be used on enums and structs with named fields";
                            cx.push(meta.error(msg));
                        }
                    }
                }
            } else {
                unsupported(cx, position, &meta);
            }
        }

        let mut is_packed = false;
        for attr in &item.attrs {
            if attr.is("repr")
                && let AttrArgs::List(group) = &attr.args
            {
                for token in group.stream() {
                    if let TokenTree::Ident(ident) = token {
                        is_packed |= ident.to_string() == "packed";
                    }
                }
            }
        }

        let source_name = unraw(&item.ident);
        Container {
            name: MultiName::from_attrs(
                source_name,
                Attr::none(cx, "rename"),
                Attr::none(cx, "rename"),
                Vec::new(),
            ),
            transparent: transparent.get(),
            deny_unknown_fields: deny_unknown_fields.get(),
            default: default.get().unwrap_or(Default::None),
            rename_all_rule: rename_all_rule.get().unwrap_or(RenameRule::None),
            tag: decide_tag(cx, item, &untagged, internal_tag),
            is_packed,
        }
    }

    pub fn name(&self) -> &MultiName {
        &self.name
    }

    pub fn rename_all_rule(&self) -> RenameRule {
        self.rename_all_rule
    }

    pub fn transparent(&self) -> bool {
        self.transparent
    }

    pub fn deny_unknown_fields(&self) -> bool {
        self.deny_unknown_fields
    }

    pub fn default(&self) -> &Default {
        &self.default
    }

    pub fn tag(&self) -> &TagType {
        &self.tag
    }

    pub fn is_packed(&self) -> bool {
        self.is_packed
    }
}

fn rule_list() -> String {
    RENAME_RULES
        .iter()
        .map(|(name, _)| format!("{name:?}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn decide_tag(
    cx: &Errors,
    item: &DeriveInput,
    untagged: &BoolAttr,
    internal_tag: Attr<String>,
) -> TagType {
    match (untagged.get(), internal_tag.get()) {
        (false, None) => TagType::External,
        (true, None) => TagType::None,
        (false, Some(tag)) => {
            // Check that there are no tuple variants.
            if let Data::Enum(variants) = &item.data {
                for variant in variants {
                    match &variant.fields {
                        Fields::Named(_) | Fields::Unit => {}
                        Fields::Unnamed(fields) => {
                            if fields.len() != 1 {
                                let msg =
                                    "#[serde(tag = \"...\")] cannot be used with tuple variants";
                                cx.spanned(variant.first_span, variant.last_span, msg);
                                break;
                            }
                        }
                    }
                }
            }
            TagType::Internal { tag }
        }
        (true, Some(_)) => {
            let msg = "enum cannot be both untagged and internally tagged";
            cx.spanned(item.first_span, item.last_span, msg);
            TagType::External // doesn't matter, will error
        }
    }
}

/// Represents variant attribute information
pub struct Variant {
    name: MultiName,
}

impl Variant {
    pub fn from_ast(cx: &Errors, variant: &aterm_macro_support::Variant) -> Self {
        let mut ser_name = Attr::none(cx, "rename");
        let mut de_name = Attr::none(cx, "rename");
        let mut de_aliases = Vec::new();
        let position = Position::Variant;

        for meta in serde_metas(cx, &variant.attrs) {
            if meta.is("rename") {
                // #[serde(rename = "foo")]
                if let Some((value, span)) = get_lit_str(cx, position, &meta) {
                    let name = Name { value, span };
                    ser_name.set(&meta, name.clone());
                    de_name.set_if_none(name.clone());
                    de_aliases.push(name);
                }
            } else {
                unsupported(cx, position, &meta);
            }
        }

        Variant {
            name: MultiName::from_attrs(unraw(&variant.ident), ser_name, de_name, de_aliases),
        }
    }

    pub fn name(&self) -> &MultiName {
        &self.name
    }

    pub fn aliases(&self) -> &BTreeSet<Name> {
        self.name.deserialize_aliases()
    }

    pub fn rename_by_rules(&mut self, rule: RenameRule) {
        if !self.name.serialize_renamed {
            self.name.serialize.value = rule.apply_to_variant(&self.name.serialize.value);
        }
        if !self.name.deserialize_renamed {
            self.name.deserialize.value = rule.apply_to_variant(&self.name.deserialize.value);
        }
        self.name
            .deserialize_aliases
            .insert(self.name.deserialize.clone());
    }
}

/// Represents field attribute information
pub struct Field {
    name: MultiName,
    skip_serializing: bool,
    skip_deserializing: bool,
    skip_serializing_if: Option<TokenStream>,
    default: Default,
    deserialize_with: Option<TokenStream>,
    borrowed_lifetimes: BTreeMap<String, Lifetime>,
    flatten: bool,
    transparent: bool,
}

/// Represents the default to use for a field when deserializing.
pub enum Default {
    /// Field must always be specified because it does not have a default.
    None,
    /// The default is given by `std::default::Default::default()`.
    Default,
    /// The default is given by this function.
    Path(TokenStream),
}

impl Default {
    pub fn is_none(&self) -> bool {
        match self {
            Default::None => true,
            Default::Default | Default::Path(_) => false,
        }
    }
}

impl Field {
    /// Extract out the `#[serde(...)]` attributes from a struct field.
    pub fn from_ast(
        cx: &Errors,
        index: usize,
        field: &aterm_macro_support::Field,
        ty: &Ty,
        container_default: &Default,
    ) -> Self {
        let mut ser_name = Attr::none(cx, "rename");
        let mut de_name = Attr::none(cx, "rename");
        let mut de_aliases = Vec::new();
        let mut skip = BoolAttr::none(cx, "skip");
        let mut skip_serializing_if = Attr::none(cx, "skip_serializing_if");
        let mut default = Attr::none(cx, "default");
        let mut deserialize_with = Attr::none(cx, "deserialize_with");
        let mut flatten = BoolAttr::none(cx, "flatten");
        let position = Position::Field;

        let ident = match &field.ident {
            Some(ident) => unraw(ident),
            None => Name {
                value: index.to_string(),
                span: Span::call_site(),
            },
        };

        for meta in serde_metas(cx, &field.attrs) {
            if meta.is("rename") {
                // #[serde(rename = "foo")]
                if let Some((value, span)) = get_lit_str(cx, position, &meta) {
                    let name = Name { value, span };
                    ser_name.set(&meta, name.clone());
                    de_name.set_if_none(name.clone());
                    de_aliases.push(name);
                }
            } else if meta.is("alias") {
                // #[serde(alias = "foo")]
                if let Some((value, span)) = get_lit_str(cx, position, &meta) {
                    de_aliases.push(Name { value, span });
                }
            } else if meta.is("default") {
                match meta.kind {
                    // #[serde(default = "...")]
                    MetaKind::Value { .. } => {
                        if let Some(path) = parse_lit_into_expr_path(cx, position, &meta) {
                            default.set(&meta, Default::Path(path));
                        }
                    }
                    // #[serde(default)]
                    MetaKind::Flag => default.set(&meta, Default::Default),
                    MetaKind::List(_) => unsupported_form(
                        cx,
                        position,
                        &meta,
                        "bare, `#[serde(default)]`, or `default = \"...\"`",
                    ),
                }
            } else if meta.is("skip") {
                // #[serde(skip)]
                if expect_flag(cx, position, &meta) {
                    skip.set_true(&meta);
                }
            } else if meta.is("skip_serializing_if") {
                // #[serde(skip_serializing_if = "...")]
                if let Some(path) = parse_lit_into_expr_path(cx, position, &meta) {
                    skip_serializing_if.set(&meta, path);
                }
            } else if meta.is("deserialize_with") {
                // #[serde(deserialize_with = "...")]
                if let Some(path) = parse_lit_into_expr_path(cx, position, &meta) {
                    deserialize_with.set(&meta, path);
                }
            } else if meta.is("flatten") {
                // #[serde(flatten)]
                if expect_flag(cx, position, &meta) {
                    flatten.set_true(&meta);
                }
            } else {
                unsupported(cx, position, &meta);
            }
        }

        let skipped = skip.get();

        // Is skip_deserializing, initialize the field to Default::default() unless a
        // different default is specified by `#[serde(default = "...")]` on
        // ourselves or our container (e.g. the struct we are in).
        if let Default::None = *container_default
            && skipped
        {
            default.set_if_none(Default::Default);
        }

        let mut borrowed_lifetimes = BTreeMap::new();
        if is_implicitly_borrowed(ty) {
            // Types &str and &[u8] are always implicitly borrowed. No need for
            // a #[serde(borrow)].
            collect_lifetimes(ty, &mut borrowed_lifetimes);
        }

        Field {
            name: MultiName::from_attrs(ident, ser_name, de_name, de_aliases),
            skip_serializing: skipped,
            skip_deserializing: skipped,
            skip_serializing_if: skip_serializing_if.get(),
            default: default.get().unwrap_or(Default::None),
            deserialize_with: deserialize_with.get(),
            borrowed_lifetimes,
            flatten: flatten.get(),
            transparent: false,
        }
    }

    pub fn name(&self) -> &MultiName {
        &self.name
    }

    pub fn aliases(&self) -> &BTreeSet<Name> {
        self.name.deserialize_aliases()
    }

    pub fn rename_by_rules(&mut self, rule: RenameRule) {
        if !self.name.serialize_renamed {
            self.name.serialize.value = rule.apply_to_field(&self.name.serialize.value);
        }
        if !self.name.deserialize_renamed {
            self.name.deserialize.value = rule.apply_to_field(&self.name.deserialize.value);
        }
        self.name
            .deserialize_aliases
            .insert(self.name.deserialize.clone());
    }

    pub fn skip_serializing(&self) -> bool {
        self.skip_serializing
    }

    pub fn skip_deserializing(&self) -> bool {
        self.skip_deserializing
    }

    pub fn skip_serializing_if(&self) -> Option<&TokenStream> {
        self.skip_serializing_if.as_ref()
    }

    pub fn default(&self) -> &Default {
        &self.default
    }

    pub fn deserialize_with(&self) -> Option<&TokenStream> {
        self.deserialize_with.as_ref()
    }

    pub fn borrowed_lifetimes(&self) -> &BTreeMap<String, Lifetime> {
        &self.borrowed_lifetimes
    }

    pub fn flatten(&self) -> bool {
        self.flatten
    }

    pub fn transparent(&self) -> bool {
        self.transparent
    }

    pub fn mark_transparent(&mut self) {
        self.transparent = true;
    }
}

/// `ty` with its None-delimited groups (`$t:ty` fragments) looked through.
pub fn ungroup(mut ty: &Ty) -> &Ty {
    while let Ty::Group(inner) = ty {
        ty = inner;
    }
    ty
}

fn is_implicitly_borrowed(ty: &Ty) -> bool {
    is_implicitly_borrowed_reference(ty) || is_option(ty, is_implicitly_borrowed_reference)
}

fn is_implicitly_borrowed_reference(ty: &Ty) -> bool {
    is_reference(ty, is_str) || is_reference(ty, is_slice_u8)
}

fn is_option(ty: &Ty, elem: fn(&Ty) -> bool) -> bool {
    let Ty::Path(path) = ungroup(ty) else {
        return false;
    };
    let Some(seg) = path.segments.last() else {
        return false;
    };
    let PathArgs::Angle(args) = &seg.args else {
        return false;
    };
    seg.ident.to_string() == "Option"
        && args.len() == 1
        && match &args[0] {
            GenericArg::Type(arg) => elem(arg),
            _ => false,
        }
}

// Whether the type looks like it might be `&T` where elem="T". This can have
// false negatives and false positives (a `type str = ...` alias), as upstream's
// does.
fn is_reference(ty: &Ty, elem: fn(&Ty) -> bool) -> bool {
    match ungroup(ty) {
        Ty::Reference {
            mutable: false,
            elem: inner,
            ..
        } => elem(inner),
        _ => false,
    }
}

fn is_str(ty: &Ty) -> bool {
    is_primitive_type(ty, "str")
}

fn is_slice_u8(ty: &Ty) -> bool {
    match ungroup(ty) {
        Ty::Slice(elem) => is_primitive_type(elem, "u8"),
        _ => false,
    }
}

fn is_primitive_type(ty: &Ty, primitive: &str) -> bool {
    match ungroup(ty) {
        Ty::Path(path) => path.qself.is_none() && is_primitive_path(path, primitive),
        _ => false,
    }
}

fn is_primitive_path(path: &TyPath, primitive: &str) -> bool {
    !path.leading_colon
        && path.segments.len() == 1
        && path.segments[0].ident.to_string() == primitive
        && matches!(path.segments[0].args, PathArgs::None)
}

// All lifetimes that this type could borrow from a Deserializer.
//
// For example a type `S<'a, 'b>` could borrow `'a` and `'b`. On the other hand
// a type `for<'a> fn(&'a str)` could not borrow `'a` from the Deserializer.
fn collect_lifetimes(ty: &Ty, out: &mut BTreeMap<String, Lifetime>) {
    match ty {
        Ty::Slice(elem) | Ty::Array(elem) | Ty::Ptr(elem) | Ty::Paren(elem) | Ty::Group(elem) => {
            collect_lifetimes(elem, out);
        }
        Ty::Reference { lifetime, elem, .. } => {
            if let Some(lifetime) = lifetime {
                out.insert(lifetime.name(), lifetime.clone());
            }
            collect_lifetimes(elem, out);
        }
        Ty::Tuple(elems) => {
            for elem in elems {
                collect_lifetimes(elem, out);
            }
        }
        Ty::Path(path) => {
            if let Some(qself) = &path.qself {
                collect_lifetimes(qself, out);
            }
            for seg in &path.segments {
                if let PathArgs::Angle(args) = &seg.args {
                    for arg in args {
                        match arg {
                            GenericArg::Lifetime(lifetime) => {
                                out.insert(lifetime.name(), lifetime.clone());
                            }
                            GenericArg::Type(ty) | GenericArg::AssocType(_, ty) => {
                                collect_lifetimes(ty, out);
                            }
                            GenericArg::Other => {}
                        }
                    }
                }
            }
        }
        Ty::Macro(tokens) => collect_lifetimes_from_tokens(tokens.clone(), out),
        Ty::BareFn { .. }
        | Ty::Never
        | Ty::TraitObject(_)
        | Ty::ImplTrait(_)
        | Ty::Infer
        | Ty::Other => {}
    }
}

fn collect_lifetimes_from_tokens(tokens: TokenStream, out: &mut BTreeMap<String, Lifetime>) {
    let mut iter = tokens.into_iter();
    while let Some(tt) = iter.next() {
        match &tt {
            TokenTree::Punct(op)
                if op.as_char() == '\'' && op.spacing() == proc_macro::Spacing::Joint =>
            {
                if let Some(TokenTree::Ident(ident)) = iter.next() {
                    let lifetime = Lifetime {
                        apostrophe: op.span(),
                        ident,
                    };
                    out.insert(lifetime.name(), lifetime);
                }
            }
            TokenTree::Group(group) => collect_lifetimes_from_tokens(group.stream(), out),
            _ => {}
        }
    }
}
