// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::native_ui::tests::button;
use crate::native_ui::{
    Control, GroupSpec, Insets, Layout, Length, SliderSpec, StyleRef, SwitchSpec, TextFieldSpec,
    TextSpec, TextViewportSpec, UiContent, UiNode, UiTree,
};

fn compiled_form(order: &[&str]) -> CompiledUi {
    let children = order
        .iter()
        .map(|key| button(key, &key.to_ascii_uppercase()))
        .collect();
    UiTree::new(
        UiNode::new(
            "app",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
        )
        .layout(
            Layout::column()
                .padding(Insets::all(4.0))
                .gap(2.0)
                .clipped(),
        )
        .children(children),
    )
    .compile(LogicalRect::new(0.0, 0.0, 240.0, 200.0))
    .unwrap()
}

fn node<'a>(projection: &'a NativeAccessibilityProjection, key: &str) -> &'a Node {
    let id = projection.id_for_key(&UiKey::new(key)).unwrap();
    &projection
        .update()
        .nodes
        .iter()
        .find(|(candidate, _)| *candidate == id)
        .unwrap()
        .1
}

fn semantic(
    key: &str,
    parent: Option<&str>,
    role: SemanticRole,
    value: SemanticValue,
) -> SemanticNode {
    SemanticNode {
        key: UiKey::new(key),
        parent: parent.map(UiKey::new),
        rect: LogicalRect::new(0.0, 0.0, 10.0, 10.0),
        role,
        label: key.to_string(),
        value,
        state: None,
        action: None,
        audit_id: None,
        description: None,
        offscreen: false,
        value_text: None,
    }
}

#[test]
fn stable_ids_derive_from_keys_not_order_or_filter_position() {
    let first = project_native_accessibility(&compiled_form(&["alpha", "beta"]), None).unwrap();
    let reordered = project_native_accessibility(&compiled_form(&["beta", "alpha"]), None).unwrap();
    let filtered = project_native_accessibility(&compiled_form(&["beta"]), None).unwrap();

    let beta = UiKey::new("beta");
    assert_eq!(first.id_for_key(&beta), reordered.id_for_key(&beta));
    assert_eq!(first.id_for_key(&beta), filtered.id_for_key(&beta));
    assert_eq!(first.id_for_key(&beta), Some(stable_node_id(&beta)));
    assert_ne!(
        first.id_for_key(&UiKey::new("alpha")),
        first.id_for_key(&beta)
    );
}

#[test]
fn text_field_accessibility_uses_the_same_preedit_value_and_selection_as_paint() {
    let mut input = crate::native_text_input::TextInputState::new("aβz".to_string());
    input.set_selection(1, 3);
    input.set_preedit("に".to_string(), Some(0..3));
    let input = input.projection();
    let field = Control::new(
        TextFieldSpec {
            label: "Search".to_string(),
            placeholder: None,
            secret: false,
            visual_value: None,
            input: Some(input.clone()),
            swatch: None,
            swatch_line: false,
            trailing_hint: None,
            description: None,
            end_button: false,
        },
        ActionId::new("search"),
    )
    .value(SemanticValue::Text(input.text.clone()))
    .state(ControlState {
        focused: true,
        ..ControlState::default()
    });
    let compiled = UiTree::new(UiNode::new("search", UiContent::TextField(field)))
        .compile(LogicalRect::new(0.0, 0.0, 240.0, 40.0))
        .unwrap();
    let projection = project_native_accessibility(&compiled, None).unwrap();
    let id = projection.id_for_key(&UiKey::new("search")).unwrap();
    let node = node(&projection, "search");
    assert_eq!(node.value(), Some("aにz"));
    assert_eq!(node.character_lengths(), &[1, 3, 1]);
    assert_eq!(
        node.text_selection(),
        Some(&TextSelection {
            anchor: TextPosition {
                node: id,
                character_index: 1,
            },
            focus: TextPosition {
                node: id,
                character_index: 2,
            },
        })
    );
}

#[test]
fn projection_consumes_compiled_semantics_once_and_preserves_hierarchy() {
    let compiled = compiled_form(&["alpha", "beta"]);
    let projection = project_native_accessibility(&compiled, None).unwrap();
    assert_eq!(projection.update().nodes.len(), compiled.semantics.len());
    assert_eq!(projection.routes().len(), compiled.semantics.len());

    let root_id = projection.id_for_key(&UiKey::new("app")).unwrap();
    let root = node(&projection, "app");
    assert_eq!(projection.update().tree.as_ref().unwrap().root, root_id);
    assert_eq!(root.role(), Role::Application);
    assert_eq!(
        root.children(),
        [
            projection.id_for_key(&UiKey::new("alpha")).unwrap(),
            projection.id_for_key(&UiKey::new("beta")).unwrap(),
        ]
    );
    assert_eq!(projection.update().focus, root_id);
}

#[test]
fn paint_only_responsive_status_copy_is_not_published_to_accessibility() {
    const COMPLETE: &str =
        "On. Typing notes are silent while the cursor trail is off; other sounds still play.";
    let visual = crate::native_ui::fit_native_status_label(COMPLETE, 96.0);
    assert_ne!(visual, COMPLETE, "fixture must exercise responsive copy");
    let compiled = UiTree::new(
        UiNode::new(
            "app",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
        )
        .layout(Layout::column())
        .children(vec![
            UiNode::new(
                "status",
                UiContent::Group(GroupSpec {
                    label: Some(COMPLETE.to_string()),
                    role: SemanticRole::Status,
                    style: StyleRef::Plain,
                }),
            )
            .layout(Layout::default().height(Length::Fixed(24.0)))
            .children(vec![
                UiNode::new(
                    "status/visual",
                    UiContent::Text(TextSpec {
                        text: visual.clone(),
                        role: SemanticRole::Status,
                        style: StyleRef::Plain,
                    }),
                )
                .paint_only(),
            ]),
        ]),
    )
    .compile(LogicalRect::new(0.0, 0.0, 96.0, 24.0))
    .unwrap();

    assert!(compiled.paint.iter().any(|node| {
        node.key == UiKey::new("status/visual")
            && matches!(&node.content, UiContent::Text(spec) if spec.text == visual)
    }));
    assert_eq!(compiled.semantics.len(), 2);
    assert!(compiled.semantic(&UiKey::new("status/visual")).is_none());

    let projection = project_native_accessibility(&compiled, None).unwrap();
    assert_eq!(projection.update().nodes.len(), 2);
    assert!(
        projection
            .id_for_key(&UiKey::new("status/visual"))
            .is_none()
    );
    let statuses = projection
        .update()
        .nodes
        .iter()
        .map(|(_, node)| node)
        .filter(|node| node.role() == Role::Status)
        .collect::<Vec<_>>();
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].live(), Some(Live::Polite));
    assert_eq!(statuses[0].label(), Some(COMPLETE));
    assert_eq!(statuses[0].value(), Some(COMPLETE));
    assert!(statuses[0].children().is_empty());
    assert!(
        projection
            .update()
            .nodes
            .iter()
            .all(|(_, node)| node.label() != Some(visual.as_str()))
    );
}

#[test]
fn labels_values_states_actions_focus_and_bounds_lower_without_side_tables() {
    let switch = Control::new(
        SwitchSpec {
            label: "Automatic updates".to_string(),
            description: None,
        },
        ActionId::new("updates/toggle"),
    )
    .value(SemanticValue::Bool(true))
    .state(ControlState {
        enabled: false,
        selected: true,
        invalid: true,
        busy: true,
        focused: false,
        ..ControlState::default()
    });
    let slider = Control::new(
        SliderSpec {
            description: None,
            track: None,
            label: "Font size".to_string(),
            step: 1.0,
            display_value: "14 px".to_string(),
        },
        ActionId::new("font/size"),
    )
    .value(SemanticValue::Number {
        value: 14.0,
        minimum: 6.0,
        maximum: 32.0,
    })
    .state(ControlState {
        focused: true,
        ..ControlState::default()
    });
    let field = Control::new(
        TextFieldSpec {
            label: "Family".to_string(),
            placeholder: None,
            secret: false,
            visual_value: None,
            input: None,
            swatch: None,
            swatch_line: false,
            trailing_hint: None,
            description: None,
            end_button: false,
        },
        ActionId::new("font/family"),
    )
    .value(SemanticValue::Text("Berkeley Mono".to_string()));
    let tree = UiTree::new(
        UiNode::new(
            "app",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
        )
        .layout(Layout::column().clipped())
        .children(vec![
            UiNode::new("updates", UiContent::Switch(switch))
                .layout(Layout::default().height(Length::Fixed(40.0))),
            UiNode::new("font", UiContent::Slider(slider))
                .layout(Layout::default().height(Length::Fixed(40.0))),
            UiNode::new("family", UiContent::TextField(field))
                .layout(Layout::default().height(Length::Fixed(40.0))),
            UiNode::new(
                "status",
                UiContent::Text(TextSpec {
                    text: "Update ready".to_string(),
                    role: SemanticRole::Status,
                    style: crate::native_ui::StyleRef::Plain,
                }),
            )
            .layout(Layout::default().height(Length::Fixed(24.0))),
        ]),
    );
    let compiled = tree
        .compile(LogicalRect::new(10.0, 20.0, 300.0, 144.0))
        .unwrap();
    let projection = project_native_accessibility(&compiled, None).unwrap();

    let updates = node(&projection, "updates");
    assert_eq!(updates.role(), Role::Switch);
    assert_eq!(updates.label(), Some("Automatic updates"));
    assert_eq!(updates.value(), Some("true"));
    assert_eq!(updates.toggled(), Some(Toggled::True));
    assert!(updates.is_disabled());
    assert_eq!(updates.is_selected(), Some(true));
    assert_eq!(updates.invalid(), Some(Invalid::True));
    assert!(updates.is_busy());
    assert!(!updates.supports_action(Action::Click));
    assert!(!updates.supports_action(Action::Focus));

    let font = node(&projection, "font");
    assert_eq!(font.numeric_value(), Some(14.0));
    assert_eq!(font.value(), Some("14 px"));
    assert_eq!(font.min_numeric_value(), Some(6.0));
    assert_eq!(font.max_numeric_value(), Some(32.0));
    assert!(font.supports_action(Action::Click));
    assert!(font.supports_action(Action::Focus));
    assert!(font.supports_action(Action::SetValue));
    assert!(!font.supports_action(Action::Increment));
    assert!(!font.supports_action(Action::Decrement));
    assert_eq!(
        projection.update().focus,
        projection.id_for_key(&UiKey::new("font")).unwrap()
    );
    assert_eq!(
        font.bounds(),
        Some(Rect {
            x0: 10.0,
            y0: 60.0,
            x1: 310.0,
            y1: 100.0,
        })
    );

    let family = node(&projection, "family");
    assert_eq!(family.value(), Some("Berkeley Mono"));
    assert!(family.supports_action(Action::SetValue));
    assert!(!family.supports_action(Action::ReplaceSelectedText));
    assert_eq!(node(&projection, "status").live(), Some(Live::Polite));
}

#[test]
fn a_text_node_lowers_its_words_into_the_value_a_label_reads() {
    let words = |key: &str, text: &str, role: SemanticRole| {
        UiNode::new(
            key,
            UiContent::Text(TextSpec {
                text: text.to_string(),
                role,
                style: StyleRef::Plain,
            }),
        )
        .layout(Layout::default().height(Length::Fixed(20.0)))
    };
    let compiled = UiTree::new(
        UiNode::new(
            "app",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
        )
        .layout(Layout::column().clipped())
        .children(vec![
            words("title", "Appearance", SemanticRole::Heading),
            words("note", "Changes apply at once", SemanticRole::Text),
            UiNode::new(
                "brand",
                UiContent::Group(GroupSpec {
                    label: Some("aterm ALab".to_string()),
                    role: SemanticRole::Text,
                    style: StyleRef::Plain,
                }),
            )
            .layout(Layout::default().height(Length::Fixed(20.0))),
        ]),
    )
    .compile(LogicalRect::new(0.0, 0.0, 240.0, 80.0))
    .unwrap();
    let projection = project_native_accessibility(&compiled, None).unwrap();

    let note = node(&projection, "note");
    assert_eq!(note.role(), Role::Label);
    assert_eq!(note.value(), Some("Changes apply at once"));
    assert_eq!(note.label(), Some("Changes apply at once"), "UIA's Name");
    let brand = node(&projection, "brand");
    assert_eq!(brand.role(), Role::Label);
    assert_eq!(brand.value(), Some("aterm ALab"));
    let title = node(&projection, "title");
    assert_eq!(title.role(), Role::Heading);
    assert_eq!(title.label(), Some("Appearance"));
    assert_eq!(title.value(), None, "a heading names itself by its label");

    let explicit = CompiledUi {
        semantics: vec![
            semantic("app", None, SemanticRole::Application, SemanticValue::None),
            semantic(
                "version",
                Some("app"),
                SemanticRole::Text,
                SemanticValue::Text("1.2.3".to_string()),
            ),
            SemanticNode {
                label: String::new(),
                ..semantic(
                    "blank",
                    Some("app"),
                    SemanticRole::Text,
                    SemanticValue::None,
                )
            },
        ],
        ..CompiledUi::default()
    };
    let projection = project_native_accessibility(&explicit, None).unwrap();
    assert_eq!(node(&projection, "version").value(), Some("1.2.3"));
    assert_eq!(node(&projection, "version").label(), Some("version"));
    assert_eq!(node(&projection, "blank").value(), None);
}

#[test]
fn an_offscreen_row_lowers_off_screen_and_scrolls_into_view() {
    let row = |key: &str| {
        UiNode::new(
            key,
            UiContent::Button(
                Control::new(crate::native_ui::ButtonSpec::new(key), ActionId::new(key))
                    .role(SemanticRole::ListBoxOption),
            ),
        )
        .layout(Layout::default().height(Length::Fixed(32.0)))
    };
    let compiled = UiTree::new(
        UiNode::new(
            "list",
            UiContent::Group(GroupSpec {
                label: Some("Themes".to_string()),
                role: SemanticRole::ListBox,
                style: crate::native_ui::StyleRef::Plain,
            }),
        )
        .layout(Layout::column().clipped())
        .children(vec![row("shown"), row("below").offscreen()]),
    )
    .compile(LogicalRect::new(0.0, 0.0, 200.0, 32.0))
    .unwrap();
    let projection = project_native_accessibility(&compiled, None).unwrap();
    let below = node(&projection, "below");
    assert!(below.is_offscreen());
    assert!(below.supports_action(Action::ScrollIntoView));
    let shown = node(&projection, "shown");
    assert!(!shown.is_offscreen());
    assert!(!shown.supports_action(Action::ScrollIntoView));
    let below = projection.id_for_key(&UiKey::new("below")).unwrap();
    let (_, routes) = projection.into_update_and_routes();
    let published = PublishedNativeAccessibility::new(ViewId::from_stored(7), 3, routes);
    assert_eq!(
        route_accessibility_action(
            &published,
            &aterm_a11y::model::ActionRequest {
                context: Some(ActionContext::new(7, 3)),
                action: Action::ScrollIntoView,
                target_tree: TreeId::ROOT,
                target_node: below,
                data: None,
            },
        ),
        Ok(RoutedAccessibilityAction::ScrollIntoView {
            key: UiKey::new("below")
        })
    );
}

#[test]
fn list_box_and_its_options_lower_to_their_roles() {
    let option = |key: &str, selected: bool| {
        UiNode::new(
            key,
            UiContent::Button(
                Control::new(crate::native_ui::ButtonSpec::new(key), ActionId::new(key))
                    .role(SemanticRole::ListBoxOption)
                    .state(ControlState {
                        selected,
                        ..ControlState::default()
                    }),
            ),
        )
        .layout(Layout::default().height(Length::Fixed(32.0)))
    };
    let tree = UiTree::new(
        UiNode::new(
            "app",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
        )
        .layout(Layout::column().clipped())
        .children(vec![
            UiNode::new(
                "themes",
                UiContent::Group(GroupSpec {
                    label: Some("Themes".to_string()),
                    role: SemanticRole::ListBox,
                    style: crate::native_ui::StyleRef::Plain,
                }),
            )
            .layout(Layout::column().height(Length::Fixed(64.0)))
            .children(vec![option("night", true), option("day", false)]),
            UiNode::new(
                "plain",
                UiContent::Button(Control::new(
                    crate::native_ui::ButtonSpec::new("plain"),
                    ActionId::new("plain"),
                )),
            )
            .layout(Layout::default().height(Length::Fixed(32.0))),
        ]),
    );
    let compiled = tree
        .compile(LogicalRect::new(0.0, 0.0, 200.0, 140.0))
        .unwrap();
    let projection = project_native_accessibility(&compiled, None).unwrap();
    let list = node(&projection, "themes");
    assert_eq!(list.role(), Role::ListBox);
    assert!(list.supports_action(Action::ScrollDown));
    let night = node(&projection, "night");
    assert_eq!(night.role(), Role::ListBoxOption);
    assert_eq!(night.is_selected(), Some(true));
    assert!(night.supports_action(Action::Click));
    assert!(night.supports_action(Action::Focus));
    let day = node(&projection, "day");
    assert_eq!(day.role(), Role::ListBoxOption);
    assert_eq!(day.is_selected(), Some(false), "selectable, not selected");
    assert_eq!(node(&projection, "plain").role(), Role::Button);
    assert_eq!(node(&projection, "plain").is_selected(), None);
}

#[test]
fn explicit_host_focus_wins_over_stale_per_control_flags() {
    let mut compiled = compiled_form(&["alpha", "beta"]);
    for semantic in &mut compiled.semantics {
        if semantic.role == SemanticRole::Button {
            semantic.state.as_mut().unwrap().focused = true;
        }
    }
    let beta = UiKey::new("beta");
    let projection = project_native_accessibility(&compiled, Some(&beta)).unwrap();
    assert_eq!(projection.update().focus, stable_node_id(&beta));
    assert_eq!(
        project_native_accessibility(&compiled, None),
        Err(AccessibilityProjectionError::MultipleFocusedNodes)
    );
}

#[test]
fn clipped_bounds_and_omitted_subtrees_are_used_verbatim() {
    let tree = UiTree::new(
        UiNode::new("root", UiContent::Group(GroupSpec::new("root")))
            .layout(Layout::column().clipped())
            .children(vec![
                button("first", "First"),
                button("partial", "Partial"),
                button("offscreen", "Offscreen"),
            ]),
    );
    let compiled = tree
        .compile(LogicalRect::new(5.0, 7.0, 100.0, 50.0))
        .unwrap();
    assert_eq!(
        compiled.semantic(&UiKey::new("partial")).unwrap().rect,
        LogicalRect::new(5.0, 47.0, 100.0, 10.0)
    );
    assert!(compiled.semantic(&UiKey::new("offscreen")).is_none());

    let projection = project_native_accessibility(&compiled, None).unwrap();
    assert_eq!(projection.update().nodes.len(), 3);
    assert!(projection.id_for_key(&UiKey::new("offscreen")).is_none());
    assert_eq!(
        node(&projection, "partial").bounds(),
        Some(Rect {
            x0: 5.0,
            y0: 47.0,
            x1: 105.0,
            y1: 57.0,
        })
    );
}

#[test]
fn container_transform_maps_logical_bounds_to_window_physical_pixels() {
    let compiled = compiled_form(&["alpha"]);
    let transform = Affine::translate(aterm_a11y::model::Vec2::new(0.0, 48.0)) * Affine::scale(2.0);
    let projection = project_native_accessibility_in_container(&compiled, None, transform).unwrap();
    let root = node(&projection, "app");
    assert_eq!(root.transform(), Some(&transform));
    assert_eq!(node(&projection, "alpha").transform(), None);
    assert_eq!(
        project_native_accessibility_in_container(&compiled, None, Affine::scale(0.0)),
        Err(AccessibilityProjectionError::InvalidContainerTransform)
    );
    assert_eq!(
        project_native_accessibility_in_container(&compiled, None, Affine::scale(f64::NAN)),
        Err(AccessibilityProjectionError::InvalidContainerTransform)
    );
}

#[test]
fn a_queued_action_keeps_its_owner_context_after_the_same_key_is_republished() {
    let compiled = compiled_form(&["alpha"]);
    let owner = |generation| AccessibilityOwner {
        view: ViewId::from_stored(41),
        generation,
        review_revision: 0,
    };
    let before = project_native_accessibility_for_view_in_container(
        &compiled,
        None,
        Affine::IDENTITY,
        owner(7),
    )
    .unwrap();
    let id = before.id_for_key(&UiKey::new("alpha")).unwrap();
    let request = aterm_a11y::model::ActionRequest {
        context: before
            .update
            .nodes
            .iter()
            .find(|(node, _)| *node == id)
            .unwrap()
            .1
            .action_context(),
        action: Action::Focus,
        target_tree: TreeId::ROOT,
        target_node: id,
        data: None,
    };
    let after = project_native_accessibility_for_view_in_container(
        &compiled,
        None,
        Affine::IDENTITY,
        owner(8),
    )
    .unwrap();
    assert_eq!(
        after.id_for_key(&UiKey::new("alpha")),
        Some(id),
        "semantic ids remain stable"
    );
    let (_, routes, virtual_text) = after.into_update_routes_and_virtual_text();
    let published =
        PublishedNativeAccessibility::composite(owner(8), vec![owner(8)], routes, virtual_text);
    assert_eq!(
        route_accessibility_action(&published, &request),
        Err(AccessibilityActionError::WrongOwnerContext)
    );
    let current = aterm_a11y::model::ActionRequest {
        context: Some(ActionContext::new(41, 8)),
        ..request
    };
    assert!(matches!(
        route_accessibility_action(&published, &current),
        Ok(RoutedAccessibilityAction::Focus { .. })
    ));
}

#[test]
fn queued_and_legacy_contexts_cannot_authorize_a_different_review() {
    let compiled = compiled_form(&["alpha"]);
    let owner = |review_revision| AccessibilityOwner {
        view: ViewId::from_stored(41),
        generation: 7,
        review_revision,
    };
    let projection = project_native_accessibility_for_view_in_container(
        &compiled,
        None,
        Affine::IDENTITY,
        owner(4),
    )
    .unwrap();
    let node = projection.id_for_key(&UiKey::new("alpha")).unwrap();
    let (_, routes, text) = projection.into_update_routes_and_virtual_text();
    let published = PublishedNativeAccessibility::composite(owner(4), vec![owner(4)], routes, text);
    let mut request = aterm_a11y::model::ActionRequest {
        context: Some(ActionContext::native_review(41, 7, 3)),
        action: Action::Focus,
        target_tree: TreeId::ROOT,
        target_node: node,
        data: None,
    };
    for context in [request.context, Some(ActionContext::new(41, 7))] {
        request.context = context;
        assert_eq!(
            route_accessibility_action(&published, &request),
            Err(AccessibilityActionError::WrongOwnerContext)
        );
    }
    request.context = Some(ActionContext::native_review(41, 7, 4));
    assert!(matches!(
        route_accessibility_action(&published, &request),
        Ok(RoutedAccessibilityAction::Focus { .. })
    ));
}

#[test]
fn composite_namespaces_identical_sibling_keys_and_retains_each_transform_and_owner() {
    let compiled = compiled_form(&["alpha"]);
    let first = AccessibilityOwner {
        view: ViewId::from_stored(41),
        generation: 7,
        review_revision: 0,
    };
    let second = AccessibilityOwner {
        view: ViewId::from_stored(42),
        generation: 9,
        review_revision: 0,
    };
    let first_transform =
        Affine::translate(aterm_a11y::model::Vec2::new(12.0, 64.0)) * Affine::scale(1.0);
    let second_transform =
        Affine::translate(aterm_a11y::model::Vec2::new(412.0, 64.0)) * Affine::scale(2.0);
    let first_projection =
        project_native_accessibility_for_view_in_container(&compiled, None, first_transform, first)
            .unwrap();
    let second_projection = project_native_accessibility_for_view_in_container(
        &compiled,
        None,
        second_transform,
        second,
    )
    .unwrap();
    let first_root = first_projection.update().tree.as_ref().unwrap().root;
    let second_root = second_projection.update().tree.as_ref().unwrap().root;
    let first_alpha = first_projection.id_for_key(&UiKey::new("alpha")).unwrap();
    let second_alpha = second_projection.id_for_key(&UiKey::new("alpha")).unwrap();
    assert_ne!(first_root, second_root);
    assert_ne!(first_alpha, second_alpha);

    let projection = compose_native_accessibility(
        vec![(first, first_projection), (second, second_projection)],
        Some(second.view),
        Rect {
            x0: 0.0,
            y0: 0.0,
            x1: 900.0,
            y1: 700.0,
        },
        "Settings \u{2014} aterm",
    )
    .unwrap();
    let window_root = projection.update().tree.as_ref().unwrap().root;
    let window = projection
        .update()
        .nodes
        .iter()
        .find(|(id, _)| *id == window_root)
        .unwrap();
    assert_eq!(window.1.role(), Role::Window);
    assert_eq!(window.1.label(), Some("Settings \u{2014} aterm"));
    assert_eq!(window.1.children(), [first_root, second_root]);
    assert_eq!(
        projection.update().nodes.len(),
        compiled.semantics.len() * 2 + 1
    );
    assert_eq!(
        projection
            .update()
            .nodes
            .iter()
            .find(|(id, _)| *id == first_root)
            .unwrap()
            .1
            .transform(),
        Some(&first_transform)
    );
    assert_eq!(
        projection
            .update()
            .nodes
            .iter()
            .find(|(id, _)| *id == second_root)
            .unwrap()
            .1
            .transform(),
        Some(&second_transform)
    );
    assert_eq!(
        projection.route_for_node(first_alpha).unwrap().owner,
        Some(first)
    );
    assert_eq!(
        projection.route_for_node(second_alpha).unwrap().owner,
        Some(second)
    );
    assert_eq!(
        projection.update().focus,
        stable_node_id_for_view(second.view, &UiKey::new("app"))
    );
}

#[test]
fn virtual_text_is_one_node_with_bounded_addressable_ranges() {
    let visible = crate::native_editor::EditorViewportProjection {
        first_line: 7,
        total_lines: 20,
        lines: vec![crate::native_editor::EditorViewportLine {
            number: 7,
            source: 100..105,
            column_start: 0,
            text: "aé\tz".to_string(),
            selections: vec![crate::native_editor::EditorSelectionSpan {
                bytes: 1..4,
                continues: false,
                primary: true,
            }],
            carets: vec![(4, true)],
            syntax: Vec::new(),
            diagnostics: Vec::new(),
        }],
    };
    let tree = UiTree::new(
        UiNode::new(
            "editor",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
        )
        .children(vec![UiNode::new(
            "editor/document",
            UiContent::TextViewport(TextViewportSpec {
                label: "README.md editor".to_string(),
                document_key: "document:18446744073709551615".to_string(),
                selectable: true,
                projection: Some(visible),
                grid: None,
                grid_reading: None,
                concealed: false,
                preedit: String::new(),
                preedit_caret: None,
                status: Some("1 config error · Save blocked".to_string()),
                semantic_status: Some(
                    "Config error · Ln 7, Col 3 · complete diagnostic message".to_string(),
                ),
                minibuffer: None,
                minibuffer_caret: None,
                cursor_label: None,
                dirty: false,
                saving: false,
                focused: false,
                focus_visible: false,
                action: None,
            }),
        )]),
    );
    let compiled = tree
        .compile(LogicalRect::new(0.0, 0.0, 800.0, 600.0))
        .unwrap();
    let projection = project_native_accessibility(&compiled, None).unwrap();
    assert_eq!(projection.update().nodes.len(), 4);
    assert_eq!(projection.virtual_text().len(), 1);
    let target = &projection.virtual_text()[0];
    let document = node(&projection, "editor/document");
    assert_eq!(document.role(), Role::MultilineTextInput);
    assert_eq!(document.label(), Some("README.md editor"));
    assert_eq!(document.value(), None, "provider identity is not announced");
    assert!(document.supports_action(Action::SetTextSelection));
    assert!(document.supports_action(Action::ReplaceSelectedText));
    assert!(document.supports_action(Action::ScrollDown));
    assert_eq!(target.visible_range, 100..105);
    assert_eq!(target.lines.len(), 1);
    let line = &target.lines[0];
    let cell_w =
        crate::native_ui::text_viewport_geometry(LogicalRect::new(0.0, 0.0, 800.0, 600.0), 1)
            .cell_w;
    let expected_positions = [0.0, cell_w, 2.0 * cell_w, 4.0 * cell_w];
    let expected_widths = [cell_w, cell_w, 2.0 * cell_w, cell_w];
    assert_eq!(line.source, 100..105);
    assert_eq!(line.character_source_offsets, [100, 101, 103, 104, 105]);
    assert_eq!(line.character_positions, expected_positions);
    assert_eq!(line.character_widths, expected_widths);
    assert_eq!(
        target.primary_selection,
        Some(VirtualTextSelection {
            anchor_byte: 101,
            focus_byte: 104,
        })
    );
    assert_eq!(
        document.text_selection(),
        Some(&TextSelection {
            anchor: TextPosition {
                node: line.node,
                character_index: 1,
            },
            focus: TextPosition {
                node: line.node,
                character_index: 3,
            },
        })
    );
    let text_run = projection
        .update()
        .nodes
        .iter()
        .find(|(node, _)| *node == line.node)
        .map(|(_, node)| node)
        .unwrap();
    assert_eq!(text_run.role(), Role::TextRun);
    assert_eq!(text_run.value(), Some("aé\tz"));
    assert_eq!(text_run.character_lengths(), [1, 2, 1, 1]);
    assert_eq!(
        text_run.character_positions(),
        Some(&expected_positions[..])
    );
    assert_eq!(text_run.character_widths(), Some(&expected_widths[..]));
    assert!(!text_run.is_read_only());
    let status_id = stable_node_id(&UiKey::new("editor/document/status"));
    let status = projection
        .update()
        .nodes
        .iter()
        .find(|(id, _)| *id == status_id)
        .map(|(_, node)| node)
        .expect("editor status live region");
    assert_eq!(status.role(), Role::Status);
    assert_eq!(status.live(), Some(Live::Polite));
    assert_eq!(
        status.value(),
        Some("Config error · Ln 7, Col 3 · complete diagnostic message")
    );
    assert_eq!(
        status.label(),
        Some("Config error · Ln 7, Col 3 · complete diagnostic message")
    );
    assert_eq!(status.description(), Some("Editor status"));

    let published = PublishedNativeAccessibility::with_virtual_text(
        ViewId::from_stored(7),
        1,
        projection.routes().to_vec(),
        projection.virtual_text().to_vec(),
    );
    let replacement = aterm_a11y::model::ActionRequest {
        context: Some(ActionContext::new(7, 1)),
        action: Action::ReplaceSelectedText,
        target_tree: TreeId::ROOT,
        target_node: target.node,
        data: Some(aterm_a11y::model::ActionData::Value("replacement".into())),
    };
    assert_eq!(
        route_accessibility_action(&published, &replacement),
        Ok(RoutedAccessibilityAction::ReplaceSelectedText {
            key: UiKey::new("editor/document"),
            text: "replacement".to_string(),
        })
    );
    let selection = aterm_a11y::model::ActionRequest {
        context: Some(ActionContext::new(7, 1)),
        action: Action::SetTextSelection,
        target_tree: TreeId::ROOT,
        target_node: target.node,
        data: Some(aterm_a11y::model::ActionData::SetTextSelection(
            TextSelection {
                anchor: TextPosition {
                    node: line.node,
                    character_index: 0,
                },
                focus: TextPosition {
                    node: line.node,
                    character_index: 2,
                },
            },
        )),
    };
    assert_eq!(
        route_accessibility_action(&published, &selection),
        Ok(RoutedAccessibilityAction::SetTextSelection {
            key: UiKey::new("editor/document"),
            anchor_byte: 100,
            focus_byte: 103,
        })
    );

    let request = projection
        .request_virtual_text(target.node, 9_000_000..9_000_200)
        .unwrap();
    assert_eq!(request.document_key, target.document_key);
    assert_eq!(request.range, 9_000_000..9_000_200);
    assert_eq!(
        projection.request_virtual_text(target.node, 0..MAX_VIRTUAL_TEXT_RANGE.saturating_add(1)),
        Err(VirtualTextError::RangeTooLarge)
    );
    let reversed = Range { start: 9, end: 2 };
    assert_eq!(
        projection.request_virtual_text(target.node, reversed),
        Err(VirtualTextError::ReversedRange)
    );
    assert_eq!(
        projection.request_virtual_text(NodeId(42), 0..1),
        Err(VirtualTextError::UnknownNode)
    );
}

/// The published lines are the rows the editor shows whole (126 pt of body over 20 pt rows:
/// six), never the row the footer cuts nor the projection's 2048.
#[test]
fn virtual_text_publication_is_bounded_to_rows_intersecting_the_viewport() {
    let lines = (0..2_048)
        .map(|number| crate::native_editor::EditorViewportLine {
            number,
            source: number * 2..number * 2 + 1,
            column_start: 0,
            text: "x".to_string(),
            selections: Vec::new(),
            carets: Vec::new(),
            syntax: Vec::new(),
            diagnostics: Vec::new(),
        })
        .collect();
    let tree = UiTree::new(
        UiNode::new(
            "editor",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
        )
        .children(vec![UiNode::new(
            "editor/document",
            UiContent::TextViewport(TextViewportSpec {
                label: "large.txt".to_string(),
                document_key: "document:1@7".to_string(),
                selectable: true,
                projection: Some(crate::native_editor::EditorViewportProjection {
                    first_line: 0,
                    total_lines: 10_000_000,
                    lines,
                }),
                grid: None,
                grid_reading: None,
                concealed: false,
                preedit: String::new(),
                preedit_caret: None,
                status: None,
                semantic_status: None,
                minibuffer: None,
                minibuffer_caret: None,
                cursor_label: None,
                dirty: false,
                saving: false,
                focused: false,
                focus_visible: false,
                action: None,
            }),
        )]),
    );
    let mut compiled = tree
        .compile(LogicalRect::new(0.0, 0.0, 800.0, 200.0))
        .unwrap();
    let projection = project_native_accessibility(&compiled, None).unwrap();
    let target = &projection.virtual_text()[0];

    assert_eq!(target.lines.len(), 6);
    assert_eq!(target.lines.last().unwrap().source, 10..11);
    assert_eq!(projection.update().nodes.len(), 2 + target.lines.len());

    let paint = compiled
        .paint
        .iter_mut()
        .find(|paint| paint.key == UiKey::new("editor/document"))
        .unwrap();
    let UiContent::TextViewport(spec) = &mut paint.content else {
        panic!("text viewport paint node");
    };
    spec.projection.as_mut().unwrap().lines[0].source.end += 1;
    assert!(matches!(
        project_native_accessibility(&compiled, None),
        Err(AccessibilityProjectionError::InvalidVirtualText(_))
    ));
}

#[test]
fn published_routes_validate_tree_node_action_and_typed_values() {
    let switch = Control::new(
        SwitchSpec {
            label: "Enabled".to_string(),
            description: None,
        },
        ActionId::new("set/enabled"),
    )
    .value(SemanticValue::Bool(false));
    let slider = Control::new(
        SliderSpec {
            description: None,
            track: None,
            label: "Size".to_string(),
            step: 1.0,
            display_value: "12".to_string(),
        },
        ActionId::new("set/size"),
    )
    .value(SemanticValue::Number {
        value: 12.0,
        minimum: 6.0,
        maximum: 32.0,
    });
    let tree = UiTree::new(
        UiNode::new("app", UiContent::Group(GroupSpec::new("app")))
            .layout(Layout::column())
            .children(vec![
                UiNode::new("enabled", UiContent::Switch(switch))
                    .layout(Layout::default().height(Length::Fixed(40.0))),
                UiNode::new("size", UiContent::Slider(slider))
                    .layout(Layout::default().height(Length::Fixed(40.0))),
            ]),
    );
    let compiled = tree
        .compile(LogicalRect::new(0.0, 0.0, 200.0, 80.0))
        .unwrap();
    let projection = project_native_accessibility(&compiled, None).unwrap();
    let enabled = projection.id_for_key(&UiKey::new("enabled")).unwrap();
    let size = projection.id_for_key(&UiKey::new("size")).unwrap();
    let (_, routes) = projection.into_update_and_routes();
    let published = PublishedNativeAccessibility::new(ViewId::from_stored(7), 3, routes);

    let click = aterm_a11y::model::ActionRequest {
        context: Some(ActionContext::new(7, 3)),
        action: Action::Click,
        target_tree: TreeId::ROOT,
        target_node: enabled,
        data: None,
    };
    assert_eq!(
        route_accessibility_action(&published, &click),
        Ok(RoutedAccessibilityAction::Activate {
            key: UiKey::new("enabled"),
            action: ActionId::new("set/enabled"),
            value: None,
        })
    );

    let set_size = aterm_a11y::model::ActionRequest {
        context: Some(ActionContext::new(7, 3)),
        action: Action::SetValue,
        target_tree: TreeId::ROOT,
        target_node: size,
        data: Some(aterm_a11y::model::ActionData::NumericValue(18.0)),
    };
    assert_eq!(
        route_accessibility_action(&published, &set_size),
        Ok(RoutedAccessibilityAction::Activate {
            key: UiKey::new("size"),
            action: ActionId::new("set/size"),
            value: Some(crate::native_app::SemanticInput::Number(18.0)),
        })
    );

    let wrong_kind = aterm_a11y::model::ActionRequest {
        data: Some(aterm_a11y::model::ActionData::Value("18".into())),
        ..set_size.clone()
    };
    assert_eq!(
        route_accessibility_action(&published, &wrong_kind),
        Err(AccessibilityActionError::WrongValueKind)
    );
    let unsupported = aterm_a11y::model::ActionRequest {
        action: Action::Increment,
        ..set_size
    };
    assert_eq!(
        route_accessibility_action(&published, &unsupported),
        Err(AccessibilityActionError::UnsupportedAction)
    );
}

#[test]
fn malformed_semantics_and_hash_collisions_fail_closed() {
    assert!(matches!(
        project_native_accessibility(&CompiledUi::default(), None),
        Err(AccessibilityProjectionError::EmptyTree)
    ));

    let compiled = compiled_form(&["alpha", "beta"]);
    assert!(matches!(
        project_with_ids(&compiled, None, |_| NodeId(7)),
        Err(AccessibilityProjectionError::IdCollision { .. })
    ));

    let missing_parent = CompiledUi {
        semantics: vec![semantic(
            "child",
            Some("absent"),
            SemanticRole::Text,
            SemanticValue::None,
        )],
        ..CompiledUi::default()
    };
    assert_eq!(
        project_native_accessibility(&missing_parent, None),
        Err(AccessibilityProjectionError::MissingParent {
            node: UiKey::new("child"),
            parent: UiKey::new("absent"),
        })
    );

    let two_roots = CompiledUi {
        semantics: vec![
            semantic("one", None, SemanticRole::Group, SemanticValue::None),
            semantic("two", None, SemanticRole::Group, SemanticValue::None),
        ],
        ..CompiledUi::default()
    };
    assert_eq!(
        project_native_accessibility(&two_roots, None),
        Err(AccessibilityProjectionError::MultipleRoots)
    );
}

#[test]
fn stale_focus_falls_back_to_root_and_invalid_numbers_fail_closed() {
    let compiled = compiled_form(&["alpha"]);
    let projection = project_native_accessibility(&compiled, Some(&UiKey::new("gone"))).unwrap();
    assert_eq!(
        projection.update().focus,
        projection.id_for_key(&UiKey::new("app")).unwrap()
    );

    let invalid = CompiledUi {
        semantics: vec![semantic(
            "slider",
            None,
            SemanticRole::Slider,
            SemanticValue::Number {
                value: f64::NAN,
                minimum: 0.0,
                maximum: 1.0,
            },
        )],
        ..CompiledUi::default()
    };
    assert_eq!(
        project_native_accessibility(&invalid, None),
        Err(AccessibilityProjectionError::InvalidNumber(UiKey::new(
            "slider"
        )))
    );
}
