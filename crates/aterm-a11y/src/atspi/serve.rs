// Copyright 2022, 2024, 2026 The AccessKit Authors.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from accesskit_unix 0.22.0 (MIT OR Apache-2.0, used under
// Apache-2.0) src/atspi/interfaces/*.rs and src/util.rs, and rewritten by the
// aterm project in 2026 as one synchronous dispatcher over aterm-dbus messages
// in place of zbus's object server and `#[interface]` macros.

//! Answering an assistive technology's calls: the object table and the
//! dispatcher for every interface the publisher serves.
//!
//! The table holds what zbus's object server held for accesskit_unix: the
//! application root (`Accessible`, `Application`) and the `Cache` object once
//! the publisher is on the accessibility bus, and each node at its path with
//! the interfaces the semantics registered for it. Of those, the
//! seven accesskit_unix implemented are served: `Accessible`, `Action`,
//! `Component`, `Hyperlink` (a `Role::Link` node has it), `Selection`, `Text`
//! and `Value`. Every object also answers `org.freedesktop.DBus.Properties`
//! and, on any path, `org.freedesktop.DBus.Peer`. `Introspectable` is not
//! served: no AT-SPI client introspects an application.
//!
//! The answers are the old publisher's, method for method: the same members
//! (the root has no `GetAttributes`, as before), the same out-signatures, and
//! zbus's error names and texts (`Unknown method 'GetAttributes'`), so the
//! recorded tree walks of `crates/aterm-lab/linux-baseline/` read the same.

use std::collections::HashMap;

use aterm_dbus::{Message, Type, Value};

use super::semantics::{
    CoordType, Error as SemanticError, Granularity, Interface, InterfaceSet, NodeIdentity,
    PlatformNode, PlatformRoot, ScrollType,
};
use super::wire::{self, CACHE_PATH, CacheText, ObjectId, ROOT_PATH};

pub(crate) const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const PEER: &str = "org.freedesktop.DBus.Peer";

const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const ACTION: &str = "org.a11y.atspi.Action";
const APPLICATION: &str = "org.a11y.atspi.Application";
const CACHE: &str = "org.a11y.atspi.Cache";
const COMPONENT: &str = "org.a11y.atspi.Component";
const HYPERLINK: &str = "org.a11y.atspi.Hyperlink";
const SELECTION: &str = "org.a11y.atspi.Selection";
const TEXT: &str = "org.a11y.atspi.Text";
const VALUE: &str = "org.a11y.atspi.Value";

const UNKNOWN_OBJECT: &str = "org.freedesktop.DBus.Error.UnknownObject";
const UNKNOWN_INTERFACE: &str = "org.freedesktop.DBus.Error.UnknownInterface";
const UNKNOWN_METHOD: &str = "org.freedesktop.DBus.Error.UnknownMethod";
const UNKNOWN_PROPERTY: &str = "org.freedesktop.DBus.Error.UnknownProperty";
const INVALID_ARGS: &str = "org.freedesktop.DBus.Error.InvalidArgs";
const FAILED: &str = "org.freedesktop.DBus.Error.Failed";

/// An error answer: its D-Bus name and text.
#[derive(Debug, PartialEq)]
pub(crate) struct Fault {
    pub(crate) name: &'static str,
    pub(crate) text: String,
}

fn fault(name: &'static str, text: impl Into<String>) -> Fault {
    Fault {
        name,
        text: text.into(),
    }
}

/// accesskit_unix's mapping of a semantic error to the error an AT reads:
/// a node gone or without the interface is an unknown object, named by path.
fn semantic(path: &str, error: SemanticError) -> Fault {
    match error {
        SemanticError::Defunct | SemanticError::UnsupportedInterface => fault(UNKNOWN_OBJECT, path),
        SemanticError::TooManyChildren => fault(FAILED, "Too many children."),
        SemanticError::IndexOutOfRange => fault(FAILED, "Index is too big."),
        SemanticError::TooManyCharacters => fault(FAILED, "Too many characters."),
        SemanticError::UnsupportedTextGranularity => fault(FAILED, "Unsupported text granularity."),
    }
}

/// What one call produced: the reply's body or its error, and the signals
/// to send before it (a property `Set` announces the new value first, as
/// zbus does).
#[derive(Debug)]
pub(crate) struct Served {
    pub(crate) answer: Result<Vec<Value>, Fault>,
    pub(crate) signals: Vec<Message>,
}

/// A dispatch step: `Ok(None)` when the member is not one the interface has.
type Dispatch = Result<Option<Vec<Value>>, Fault>;

/// A node at its path and the interfaces registered for it.
struct Registered {
    node: PlatformNode,
    interfaces: InterfaceSet,
}

/// The interfaces a node path can serve (accesskit_unix's seven).
fn served() -> InterfaceSet {
    InterfaceSet::new(
        Interface::Accessible
            | Interface::Action
            | Interface::Component
            | Interface::Hyperlink
            | Interface::Selection
            | Interface::Text
            | Interface::Value,
    )
}

/// Everything one connection serves. The session bus's table is empty (it
/// answers `Peer` and unknown objects); the accessibility bus's has the root,
/// the cache and the nodes.
pub(crate) struct Objects {
    bus_name: String,
    root: PlatformRoot,
    published: bool,
    nodes: HashMap<String, Registered>,
}

/// What a path names.
enum Target<'a> {
    Root,
    Cache,
    Node(&'a Registered),
    /// `/` or an ancestor of a served path: it exists, with the standard
    /// interfaces only (zbus's object tree).
    Bare,
}

impl Objects {
    /// An empty table for the connection named `bus_name`.
    pub(crate) fn new(bus_name: &str, root: PlatformRoot) -> Self {
        Self {
            bus_name: bus_name.to_owned(),
            root,
            published: false,
            nodes: HashMap::new(),
        }
    }

    /// Serve the application root and the cache.
    pub(crate) fn publish_root(&mut self) {
        self.published = true;
    }

    /// The connection's name, which every reference carries.
    pub(crate) fn bus_name(&self) -> &str {
        &self.bus_name
    }

    /// Add `interfaces` to `node`'s path.
    pub(crate) fn register(&mut self, node: PlatformNode, interfaces: InterfaceSet) {
        let path = ObjectId::of(&node).path();
        match self.nodes.get_mut(&path) {
            Some(entry) => interfaces.iter().for_each(|i| entry.interfaces.insert(i)),
            None => {
                self.nodes.insert(path, Registered { node, interfaces });
            }
        }
    }

    /// Take `interfaces` off `id`'s path; the path goes with its last one.
    pub(crate) fn unregister(&mut self, id: ObjectId, interfaces: InterfaceSet) {
        let path = id.path();
        if let Some(entry) = self.nodes.get_mut(&path) {
            let kept: InterfaceSet = entry
                .interfaces
                .iter()
                .filter(|i| !interfaces.contains(*i))
                .collect();
            if kept.bits() == 0 {
                self.nodes.remove(&path);
            } else {
                entry.interfaces = kept;
            }
        }
    }

    /// Drops a closed window's paths. accesskit_unix left them registered,
    /// answering each call with its defunct node's error, for as long as the
    /// bus lived; this forgets them, and a call there is an unknown object.
    pub(crate) fn forget_adapter(&mut self, adapter: usize) {
        self.nodes
            .retain(|_, entry| entry.node.adapter_id() != adapter);
    }

    fn target(&self, path: &str) -> Option<Target<'_>> {
        if self.published && path == ROOT_PATH {
            return Some(Target::Root);
        }
        if self.published && path == CACHE_PATH {
            return Some(Target::Cache);
        }
        if let Some(entry) = self.nodes.get(path) {
            return Some(Target::Node(entry));
        }
        let fixed = [ROOT_PATH, CACHE_PATH];
        let served = fixed
            .iter()
            .copied()
            .filter(|_| self.published)
            .chain(self.nodes.keys().map(String::as_str));
        let ancestor = path == "/"
            || served.into_iter().any(|p| {
                p.strip_prefix(path)
                    .is_some_and(|rest| rest.starts_with('/'))
            });
        ancestor.then_some(Target::Bare)
    }
}

impl Target<'_> {
    fn has(&self, interface: &str) -> bool {
        match self {
            Self::Root => interface == ACCESSIBLE || interface == APPLICATION,
            Self::Cache => interface == CACHE,
            Self::Node(entry) => entry
                .interfaces
                .iter()
                .filter(|i| served().contains(*i))
                .any(|i| wire::interface_name(i) == interface),
            Self::Bare => false,
        }
    }
}

/// Answer `call`, a method call to this connection.
pub(crate) fn serve(objects: &Objects, call: &Message) -> Served {
    let mut signals = Vec::new();
    let answer = dispatch(objects, call, &mut signals);
    Served { answer, signals }
}

fn dispatch(
    objects: &Objects,
    call: &Message,
    signals: &mut Vec<Message>,
) -> Result<Vec<Value>, Fault> {
    let path = call.path.as_ref().map_or("/", |p| p.as_str());
    let Some(interface) = call.interface.as_deref() else {
        return Err(fault(FAILED, "Missing interface"));
    };
    let member = call.member.as_deref().unwrap_or_default();
    let body = call.body.as_slice();
    if interface == PEER {
        return peer(member, body);
    }
    let Some(target) = objects.target(path) else {
        return Err(fault(UNKNOWN_OBJECT, format!("Unknown object '{path}'")));
    };
    let unknown_interface = || {
        fault(
            UNKNOWN_INTERFACE,
            format!("Unknown interface '{interface}'"),
        )
    };
    if interface == PROPERTIES {
        return properties(objects, &target, path, member, body, signals);
    }
    if !target.has(interface) {
        return Err(unknown_interface());
    }
    let found = match (&target, interface) {
        (Target::Root, ACCESSIBLE) => root_accessible(objects, member, body)?,
        (Target::Root, _) => None,
        (Target::Cache, _) => cache(objects, member, body)?,
        (Target::Node(entry), _) => {
            node_method(objects, &entry.node, path, interface, member, body)?
        }
        (Target::Bare, _) => return Err(unknown_interface()),
    };
    found.ok_or_else(|| fault(UNKNOWN_METHOD, format!("Unknown method '{member}'")))
}

// --- arguments ----------------------------------------------------------

/// Holds `body` to the in-signature `sig`, zbus's check before a method runs.
fn args(body: &[Value], sig: &str) -> Result<(), Fault> {
    let types: Vec<Type> = body.iter().map(Value::type_of).collect();
    let got = aterm_dbus::sig::signature_of(&types);
    if got == sig {
        Ok(())
    } else {
        Err(fault(
            INVALID_ARGS,
            format!("Invalid arguments: got signature '{got}', expected '{sig}'"),
        ))
    }
}

// After `args` the accessors cannot miss; their defaults are never read.
fn int(body: &[Value], at: usize) -> i32 {
    body.get(at).and_then(Value::as_i32).unwrap_or_default()
}

fn uint(body: &[Value], at: usize) -> u32 {
    body.get(at).and_then(Value::as_u32).unwrap_or_default()
}

fn string(body: &[Value], at: usize) -> &str {
    body.get(at).and_then(Value::as_str).unwrap_or_default()
}

fn boolean(body: &[Value], at: usize) -> bool {
    body.get(at).and_then(Value::as_bool).unwrap_or_default()
}

fn index(value: i32) -> Result<usize, Fault> {
    usize::try_from(value).map_err(|_| fault(INVALID_ARGS, "Index can't be negative."))
}

fn coord_type(value: u32) -> Result<CoordType, Fault> {
    match value {
        0 => Ok(CoordType::Screen),
        1 => Ok(CoordType::Window),
        2 => Ok(CoordType::Parent),
        _ => Err(fault(
            INVALID_ARGS,
            format!("Unknown coordinate type {value}"),
        )),
    }
}

fn granularity(value: u32) -> Result<Granularity, Fault> {
    match value {
        0 => Ok(Granularity::Char),
        1 => Ok(Granularity::Word),
        2 => Ok(Granularity::Sentence),
        3 => Ok(Granularity::Line),
        4 => Ok(Granularity::Paragraph),
        _ => Err(fault(INVALID_ARGS, format!("Unknown granularity {value}"))),
    }
}

fn scroll_type(value: u32) -> Result<ScrollType, Fault> {
    match value {
        0 => Ok(ScrollType::TopLeft),
        1 => Ok(ScrollType::BottomRight),
        2 => Ok(ScrollType::TopEdge),
        3 => Ok(ScrollType::BottomEdge),
        4 => Ok(ScrollType::LeftEdge),
        5 => Ok(ScrollType::RightEdge),
        6 => Ok(ScrollType::Anywhere),
        _ => Err(fault(INVALID_ARGS, format!("Unknown scroll type {value}"))),
    }
}

// --- org.freedesktop.DBus.Peer -------------------------------------------

fn peer(member: &str, body: &[Value]) -> Result<Vec<Value>, Fault> {
    args(body, "")?;
    match member {
        "Ping" => Ok(Vec::new()),
        "GetMachineId" => ["/etc/machine-id", "/var/lib/dbus/machine-id"]
            .iter()
            .find_map(|p| std::fs::read_to_string(p).ok())
            .map(|id| vec![id.trim().into()])
            .ok_or_else(|| fault(FAILED, "no machine id")),
        _ => Err(fault(UNKNOWN_METHOD, format!("Unknown method '{member}'"))),
    }
}

// --- org.freedesktop.DBus.Properties -------------------------------------

fn properties(
    objects: &Objects,
    target: &Target<'_>,
    path: &str,
    member: &str,
    body: &[Value],
    signals: &mut Vec<Message>,
) -> Result<Vec<Value>, Fault> {
    let unknown_interface = |interface: &str| {
        fault(
            UNKNOWN_INTERFACE,
            format!("Unknown interface '{interface}'"),
        )
    };
    match member {
        "Get" => {
            args(body, "ss")?;
            let (interface, name) = (string(body, 0), string(body, 1));
            if !target.has(interface) {
                return Err(unknown_interface(interface));
            }
            get(objects, target, path, interface, name)?
                .map(|v| vec![Value::variant(v)])
                .ok_or_else(|| fault(UNKNOWN_PROPERTY, format!("Unknown property '{name}'")))
        }
        "GetAll" => {
            args(body, "s")?;
            let interface = string(body, 0);
            if !target.has(interface) {
                return Err(unknown_interface(interface));
            }
            // A property whose getter fails is left out, as zbus leaves out
            // a fallible property that errs.
            let entries = property_names(interface)
                .iter()
                .filter_map(|name| {
                    let value = get(objects, target, path, interface, name).ok()??;
                    Some((Value::from(*name), Value::variant(value)))
                })
                .collect();
            let all = Value::dict(Type::Str, Type::Variant, entries)
                .map_err(|_| fault(FAILED, "property map"))?;
            Ok(vec![all])
        }
        "Set" => {
            args(body, "ssv")?;
            let (interface, name) = (string(body, 0), string(body, 1));
            if !target.has(interface) {
                return Err(unknown_interface(interface));
            }
            let value = body
                .get(2)
                .and_then(Value::as_variant)
                .unwrap_or(&Value::Bool(false));
            set(objects, target, path, interface, name, value)?;
            if let Some(Ok(now)) = get(objects, target, path, interface, name).transpose()
                && let Ok(signal) = Message::signal(path, PROPERTIES, "PropertiesChanged")
            {
                let changed = Value::dict(
                    Type::Str,
                    Type::Variant,
                    vec![(name.into(), Value::variant(now))],
                )
                .map_err(|_| fault(FAILED, "property map"))?;
                let invalidated = Value::strings(Vec::<String>::new());
                signals.push(signal.with_body(vec![interface.into(), changed, invalidated]));
            }
            Ok(Vec::new())
        }
        _ => Err(fault(UNKNOWN_METHOD, format!("Unknown method '{member}'"))),
    }
}

/// The readable properties of `interface`, in declaration order (GetAll's).
fn property_names(interface: &str) -> &'static [&'static str] {
    match interface {
        ACCESSIBLE => &[
            "Name",
            "Description",
            "Parent",
            "ChildCount",
            "Locale",
            "AccessibleId",
        ],
        APPLICATION => &["ToolkitName", "Version", "AtspiVersion", "Id"],
        ACTION => &["NActions"],
        HYPERLINK => &["NAnchors", "StartIndex", "EndIndex"],
        SELECTION => &["NSelectedChildren"],
        TEXT => &["CharacterCount", "CaretOffset"],
        VALUE => &[
            "MinimumValue",
            "MaximumValue",
            "MinimumIncrement",
            "CurrentValue",
        ],
        _ => &[],
    }
}

/// One property's value; `Ok(None)` when `interface` has no `name`.
fn get(
    objects: &Objects,
    target: &Target<'_>,
    path: &str,
    interface: &str,
    name: &str,
) -> Result<Option<Value>, Fault> {
    let bus = objects.bus_name();
    let err = |e| semantic(path, e);
    let root = &objects.root;
    let value = match (target, interface, name) {
        (Target::Root, ACCESSIBLE, "Name") => root.name().map_err(err)?.into(),
        (Target::Root, ACCESSIBLE, "Description") => root.description().map_err(err)?.into(),
        (Target::Root, ACCESSIBLE, "Parent") => wire::null_ref(),
        (Target::Root, ACCESSIBLE, "ChildCount") => root.child_count().map_err(err)?.into(),
        (Target::Root | Target::Node(_), ACCESSIBLE, "Locale") => "".into(),
        (Target::Root, ACCESSIBLE, "AccessibleId") => "".into(),
        (Target::Root, APPLICATION, "ToolkitName") => root.toolkit_name().map_err(err)?.into(),
        (Target::Root, APPLICATION, "Version") => root.toolkit_version().map_err(err)?.into(),
        (Target::Root, APPLICATION, "AtspiVersion") => "2.1".into(),
        (Target::Root, APPLICATION, "Id") => root.id().map_err(err)?.into(),
        (Target::Node(entry), _, _) => {
            let node = &entry.node;
            match (interface, name) {
                (ACCESSIBLE, "Name") => node.name().map_err(err)?.into(),
                (ACCESSIBLE, "Description") => node.description().map_err(err)?.into(),
                (ACCESSIBLE, "Parent") => wire::object_ref(
                    bus,
                    ObjectId::in_adapter(node.adapter_id(), node.parent().map_err(err)?),
                ),
                (ACCESSIBLE, "ChildCount") => node.child_count().map_err(err)?.into(),
                (ACCESSIBLE, "AccessibleId") => node.accessible_id().map_err(err)?.into(),
                (ACTION, "NActions") => node.n_actions().map_err(err)?.into(),
                (HYPERLINK, "NAnchors") => node.n_anchors().map_err(err)?.into(),
                (HYPERLINK, "StartIndex") => node.hyperlink_start_index().map_err(err)?.into(),
                (HYPERLINK, "EndIndex") => node.hyperlink_end_index().map_err(err)?.into(),
                (SELECTION, "NSelectedChildren") => node.n_selected_children().map_err(err)?.into(),
                (TEXT, "CharacterCount") => node.character_count().map_err(err)?.into(),
                (TEXT, "CaretOffset") => node.caret_offset().map_err(err)?.into(),
                (VALUE, "MinimumValue") => node.minimum_value().map_err(err)?.into(),
                (VALUE, "MaximumValue") => node.maximum_value().map_err(err)?.into(),
                (VALUE, "MinimumIncrement") => node.minimum_increment().map_err(err)?.into(),
                (VALUE, "CurrentValue") => node.current_value().map_err(err)?.into(),
                _ => return Ok(None),
            }
        }
        _ => return Ok(None),
    };
    Ok(Some(value))
}

/// The two writable properties: the application's `Id` (the registry's) and
/// a value's `CurrentValue` (a SetValue action).
fn set(
    objects: &Objects,
    target: &Target<'_>,
    path: &str,
    interface: &str,
    name: &str,
    value: &Value,
) -> Result<(), Fault> {
    let wrong_type = || {
        fault(
            INVALID_ARGS,
            format!("Incorrect type for property '{name}'"),
        )
    };
    match (target, interface, name) {
        (Target::Root, APPLICATION, "Id") => {
            let id = value.as_i32().ok_or_else(wrong_type)?;
            // `set_id` takes the root mutably; a root is a weak handle, so a
            // copy writes the same application context.
            objects
                .root
                .clone()
                .set_id(id)
                .map_err(|e| semantic(path, e))
        }
        (Target::Node(entry), VALUE, "CurrentValue") => {
            let v = value.as_f64().ok_or_else(wrong_type)?;
            entry
                .node
                .set_current_value(v)
                .map_err(|e| semantic(path, e))
        }
        _ => Err(fault(
            UNKNOWN_PROPERTY,
            format!("Unknown property '{name}'"),
        )),
    }
}

// --- the application root -------------------------------------------------

/// The root's `Accessible`: accesskit_unix's `RootAccessibleInterface`, which
/// has no `GetAttributes`, `GetRoleName` or `GetLocalizedRoleName`.
fn root_accessible(objects: &Objects, member: &str, body: &[Value]) -> Dispatch {
    let bus = objects.bus_name();
    let root = &objects.root;
    let err = |e| semantic(ROOT_PATH, e);
    let out = match member {
        "GetChildAtIndex" => {
            args(body, "i")?;
            let at = index(int(body, 0))?;
            let child = root.child_id_at_index(at).map_err(err)?;
            vec![wire::optional_ref(
                bus,
                child.map(|(adapter, node)| ObjectId::Node { adapter, node }),
            )]
        }
        "GetChildren" => {
            args(body, "")?;
            let ids: Vec<ObjectId> = root
                .map_child_ids(|(adapter, node)| ObjectId::Node { adapter, node })
                .map_err(err)?;
            vec![wire::refs(bus, ids)]
        }
        "GetIndexInParent" => {
            args(body, "")?;
            vec![root.index_in_parent().into()]
        }
        "GetRelationSet" => {
            args(body, "")?;
            vec![wire::relation_set(bus, Vec::new())]
        }
        "GetRole" => {
            args(body, "")?;
            vec![(root.role() as u32).into()]
        }
        "GetState" => {
            args(body, "")?;
            vec![wire::states(root.state())]
        }
        "GetApplication" => {
            args(body, "")?;
            vec![wire::object_ref(bus, ObjectId::Root)]
        }
        "GetInterfaces" => {
            args(body, "")?;
            vec![wire::interfaces(root.interfaces())]
        }
        _ => return Ok(None),
    };
    Ok(Some(out))
}

/// `org.a11y.atspi.Cache.GetItems`: the root's item, then every node's.
fn cache(objects: &Objects, member: &str, body: &[Value]) -> Dispatch {
    if member != "GetItems" {
        return Ok(None);
    }
    args(body, "")?;
    let bus = objects.bus_name();
    let root = &objects.root;
    let err = |e| semantic(ROOT_PATH, e);
    let descendants: Vec<Value> = root
        .map_descendant_cache_nodes(|node| Value::Struct(wire::cache_item(bus, node)))
        .map_err(err)?;
    let application = wire::application_cache_item(
        bus,
        CacheText {
            index: root.index_in_parent(),
            children: root.child_count().map_err(err)?,
            interfaces: root.interfaces(),
            short_name: root.name().map_err(err)?,
            role: root.role() as u32,
            name: root.description().map_err(err)?,
            states: root.state(),
        },
    );
    let items = std::iter::once(Value::Struct(application))
        .chain(descendants)
        .collect();
    let all = Value::array(wire::cache_item_type(), items)
        .map_err(|_| fault(FAILED, "cache item of another type"))?;
    Ok(Some(vec![all]))
}

// --- a node ------------------------------------------------------------------

fn node_method(
    objects: &Objects,
    node: &PlatformNode,
    path: &str,
    interface: &str,
    member: &str,
    body: &[Value],
) -> Dispatch {
    let bus = objects.bus_name();
    let adapter = node.adapter_id();
    let id = |n: NodeIdentity| ObjectId::Node { adapter, node: n };
    let err = |e| semantic(path, e);
    let out = match (interface, member) {
        (ACCESSIBLE, "GetChildAtIndex") => {
            args(body, "i")?;
            let child = node.child_at_index(index(int(body, 0))?).map_err(err)?;
            vec![wire::optional_ref(bus, child.map(id))]
        }
        (ACCESSIBLE, "GetChildren") => {
            args(body, "")?;
            let ids: Vec<ObjectId> = node.map_children(id).map_err(err)?;
            vec![wire::refs(bus, ids)]
        }
        (ACCESSIBLE, "GetIndexInParent") => {
            args(body, "")?;
            vec![node.index_in_parent().map_err(err)?.into()]
        }
        (ACCESSIBLE, "GetRelationSet") => {
            args(body, "")?;
            let mut set: Vec<_> = node.relation_set(id).map_err(err)?.into_iter().collect();
            set.sort_by_key(|(kind, _)| *kind as u32);
            vec![wire::relation_set(bus, set)]
        }
        (ACCESSIBLE, "GetRole") => {
            args(body, "")?;
            vec![(node.role().map_err(err)? as u32).into()]
        }
        (ACCESSIBLE, "GetLocalizedRoleName") => {
            args(body, "")?;
            vec![node.localized_role_name().map_err(err)?.into()]
        }
        (ACCESSIBLE, "GetState") => {
            args(body, "")?;
            vec![wire::states(node.state())]
        }
        (ACCESSIBLE, "GetAttributes") => {
            args(body, "")?;
            vec![wire::string_map(node.attributes().map_err(err)?)]
        }
        (ACCESSIBLE, "GetApplication") => {
            args(body, "")?;
            vec![wire::object_ref(bus, ObjectId::Root)]
        }
        (ACCESSIBLE, "GetInterfaces") => {
            args(body, "")?;
            vec![wire::interfaces(node.interfaces().map_err(err)?)]
        }
        (ACTION, "GetDescription" | "GetKeyBinding") => {
            args(body, "i")?;
            vec!["".into()]
        }
        (ACTION, "GetName" | "GetLocalizedName") => {
            args(body, "i")?;
            vec![node.action_name(int(body, 0)).map_err(err)?.into()]
        }
        (ACTION, "GetActions") => {
            args(body, "")?;
            vec![wire::actions(node.actions().map_err(err)?)]
        }
        (ACTION, "DoAction") => {
            args(body, "i")?;
            vec![node.do_action(int(body, 0)).map_err(err)?.into()]
        }
        (COMPONENT, "Contains") => {
            args(body, "iiu")?;
            let coords = coord_type(uint(body, 2))?;
            vec![
                node.contains(int(body, 0), int(body, 1), coords)
                    .map_err(err)?
                    .into(),
            ]
        }
        (COMPONENT, "GetAccessibleAtPoint") => {
            args(body, "iiu")?;
            let coords = coord_type(uint(body, 2))?;
            let hit = node
                .accessible_at_point(int(body, 0), int(body, 1), coords)
                .map_err(err)?;
            vec![wire::optional_ref(bus, hit.map(id))]
        }
        (COMPONENT, "GetExtents") => {
            args(body, "u")?;
            let coords = coord_type(uint(body, 0))?;
            vec![wire::rect(node.extents(coords).map_err(err)?)]
        }
        (COMPONENT, "GetLayer") => {
            args(body, "")?;
            vec![(node.layer().map_err(err)? as u32).into()]
        }
        (COMPONENT, "GrabFocus") => {
            args(body, "")?;
            vec![node.grab_focus().map_err(err)?.into()]
        }
        (COMPONENT, "ScrollTo") => {
            args(body, "u")?;
            let kind = scroll_type(uint(body, 0))?;
            vec![node.scroll_to(kind).map_err(err)?.into()]
        }
        (COMPONENT, "ScrollToPoint") => {
            args(body, "uii")?;
            let coords = coord_type(uint(body, 0))?;
            vec![
                node.scroll_to_point(coords, int(body, 1), int(body, 2))
                    .map_err(err)?
                    .into(),
            ]
        }
        (HYPERLINK, "GetObject") => {
            args(body, "i")?;
            let object = node.hyperlink_object(int(body, 0)).map_err(err)?;
            vec![wire::optional_ref(bus, object.map(id))]
        }
        (HYPERLINK, "GetURI") => {
            args(body, "i")?;
            vec![node.uri(int(body, 0)).map_err(err)?.into()]
        }
        (HYPERLINK, "IsValid") => {
            args(body, "")?;
            vec![node.hyperlink_is_valid().map_err(err)?.into()]
        }
        (SELECTION, "GetSelectedChild") => {
            args(body, "i")?;
            let child = node.selected_child(index(int(body, 0))?).map_err(err)?;
            vec![wire::optional_ref(bus, child.map(id))]
        }
        (SELECTION, "SelectChild") => {
            args(body, "i")?;
            vec![node.select_child(index(int(body, 0))?).map_err(err)?.into()]
        }
        (SELECTION, "DeselectSelectedChild") => {
            args(body, "i")?;
            let at = index(int(body, 0))?;
            vec![node.deselect_selected_child(at).map_err(err)?.into()]
        }
        (SELECTION, "IsChildSelected") => {
            args(body, "i")?;
            vec![
                node.is_child_selected(index(int(body, 0))?)
                    .map_err(err)?
                    .into(),
            ]
        }
        (SELECTION, "SelectAll") => {
            args(body, "")?;
            vec![node.select_all().map_err(err)?.into()]
        }
        (SELECTION, "ClearSelection") => {
            args(body, "")?;
            vec![node.clear_selection().map_err(err)?.into()]
        }
        (SELECTION, "DeselectChild") => {
            args(body, "i")?;
            vec![
                node.deselect_child(index(int(body, 0))?)
                    .map_err(err)?
                    .into(),
            ]
        }
        (TEXT, _) => return text(node, path, member, body),
        _ => return Ok(None),
    };
    Ok(Some(out))
}

/// `org.a11y.atspi.Text`.
fn text(node: &PlatformNode, path: &str, member: &str, body: &[Value]) -> Dispatch {
    let err = |e| semantic(path, e);
    let with_range = |(map, start, end): (HashMap<&'static str, String>, i32, i32)| {
        vec![wire::string_map(map), start.into(), end.into()]
    };
    let out = match member {
        "GetStringAtOffset" => {
            args(body, "iu")?;
            let unit = granularity(uint(body, 1))?;
            let (s, start, end) = node.string_at_offset(int(body, 0), unit).map_err(err)?;
            vec![s.into(), start.into(), end.into()]
        }
        "GetText" => {
            args(body, "ii")?;
            vec![node.text(int(body, 0), int(body, 1)).map_err(err)?.into()]
        }
        "SetCaretOffset" => {
            args(body, "i")?;
            vec![node.set_caret_offset(int(body, 0)).map_err(err)?.into()]
        }
        "GetAttributeValue" => {
            args(body, "is")?;
            let v = node
                .text_attribute_value(int(body, 0), string(body, 1))
                .map_err(err)?;
            vec![v.into()]
        }
        "GetAttributes" => {
            args(body, "i")?;
            with_range(node.text_attributes(int(body, 0)).map_err(err)?)
        }
        "GetDefaultAttributes" => {
            args(body, "")?;
            vec![wire::string_map(
                node.default_text_attributes().map_err(err)?,
            )]
        }
        "GetCharacterExtents" => {
            args(body, "iu")?;
            let coords = coord_type(uint(body, 1))?;
            wire::rect_fields(node.character_extents(int(body, 0), coords).map_err(err)?)
        }
        "GetOffsetAtPoint" => {
            args(body, "iiu")?;
            let coords = coord_type(uint(body, 2))?;
            vec![
                node.offset_at_point(int(body, 0), int(body, 1), coords)
                    .map_err(err)?
                    .into(),
            ]
        }
        "GetNSelections" => {
            args(body, "")?;
            vec![node.n_selections().map_err(err)?.into()]
        }
        "GetSelection" => {
            args(body, "i")?;
            let (start, end) = node.selection(int(body, 0)).map_err(err)?;
            vec![start.into(), end.into()]
        }
        "AddSelection" => {
            args(body, "ii")?;
            vec![
                node.add_selection(int(body, 0), int(body, 1))
                    .map_err(err)?
                    .into(),
            ]
        }
        "RemoveSelection" => {
            args(body, "i")?;
            vec![node.remove_selection(int(body, 0)).map_err(err)?.into()]
        }
        "SetSelection" => {
            args(body, "iii")?;
            vec![
                node.set_selection(int(body, 0), int(body, 1), int(body, 2))
                    .map_err(err)?
                    .into(),
            ]
        }
        "GetRangeExtents" => {
            args(body, "iiu")?;
            let coords = coord_type(uint(body, 2))?;
            wire::rect_fields(
                node.range_extents(int(body, 0), int(body, 1), coords)
                    .map_err(err)?,
            )
        }
        "GetAttributeRun" => {
            args(body, "ib")?;
            with_range(
                node.text_attribute_run(int(body, 0), boolean(body, 1))
                    .map_err(err)?,
            )
        }
        "ScrollSubstringTo" => {
            args(body, "iiu")?;
            let kind = scroll_type(uint(body, 2))?;
            vec![
                node.scroll_substring_to(int(body, 0), int(body, 1), kind)
                    .map_err(err)?
                    .into(),
            ]
        }
        "ScrollSubstringToPoint" => {
            args(body, "iiuii")?;
            let coords = coord_type(uint(body, 2))?;
            vec![
                node.scroll_substring_to_point(
                    int(body, 0),
                    int(body, 1),
                    coords,
                    int(body, 3),
                    int(body, 4),
                )
                .map_err(err)?
                .into(),
            ]
        }
        _ => return Ok(None),
    };
    Ok(Some(out))
}
