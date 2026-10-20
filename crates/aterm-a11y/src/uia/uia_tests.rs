// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Windows-only controls exercise the actual COM objects and method dispatch.
//! They do not create an HWND, inspect the desktop or claim Narrator acceptance.

#![cfg(test)]

use super::*;

struct Activation;
impl ActivationHandler for Activation {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        None
    }
}
struct Actions(Arc<Mutex<Vec<model::ActionRequest>>>);
impl ActionHandler for Actions {
    fn do_action(&mut self, action: model::ActionRequest) {
        self.0.lock_or_recover().push(action);
    }
}

fn update(button: bool) -> TreeUpdate {
    let mut root = model::Node::new(Role::Terminal);
    root.set_read_only();
    root.set_label("terminal");
    let mut run = model::Node::new(Role::TextRun);
    run.set_value("A😀e\u{301}\n");
    run.set_character_lengths(vec![1, 4, 3, 1]);
    run.set_word_starts(vec![0, 2]);
    root.set_children(if button {
        vec![model::NodeId(2), model::NodeId(3)]
    } else {
        vec![model::NodeId(2)]
    });
    root.set_text_selection(model::TextSelection {
        anchor: model::TextPosition {
            node: model::NodeId(2),
            character_index: 1,
        },
        focus: model::TextPosition {
            node: model::NodeId(2),
            character_index: 1,
        },
    });
    let mut nodes = vec![(model::NodeId(1), root), (model::NodeId(2), run)];
    if button {
        let mut button = model::Node::new(Role::Button);
        button.set_label("Run");
        button.add_action(Action::Click);
        button.set_action_context(model::ActionContext::new(41, 7));
        nodes.push((model::NodeId(3), button));
    }
    TreeUpdate {
        nodes,
        tree: Some(model::Tree::new(model::NodeId(1))),
        tree_id: model::TreeId::ROOT,
        focus: model::NodeId(1),
    }
}

fn fixture() -> (Arc<Shared>, Arc<Mutex<Vec<model::ActionRequest>>>) {
    let actions = Arc::new(Mutex::new(Vec::new()));
    let mut publication = Publication::default();
    publication.update(update(true));
    let shared = Arc::new(Shared {
        hwnd: 0,
        state: Mutex::new(State {
            publication,
            origin: (0.0, 0.0),
        }),
        root: OnceLock::new(),
        active: AtomicBool::new(false),
        closed: AtomicBool::new(false),
        activation: Mutex::new(Box::new(Activation)),
        actions: Mutex::new(Box::new(Actions(actions.clone()))),
    });
    let _ = shared.root.set(ComObject::new(Provider {
        shared: Arc::downgrade(&shared),
        handle: None,
    }));
    (shared, actions)
}

#[test]
fn interface_queries_share_identity_and_owned_references_outlive_the_original() {
    let (shared, _) = fixture();
    let root = shared.root().unwrap();
    let identity: IUnknown = root.cast().unwrap();
    let fragment: IRawElementProviderFragment = root.cast().unwrap();
    let text: ITextProvider2 = root.cast().unwrap();
    assert_eq!(
        identity.as_raw(),
        fragment.cast::<IUnknown>().unwrap().as_raw()
    );
    assert_eq!(identity.as_raw(), text.cast::<IUnknown>().unwrap().as_raw());
    let held = root.clone();
    drop(root);
    drop(identity);
    drop(fragment);
    drop(text);
    assert!(unsafe { held.GetPropertyValue(UIA_NamePropertyId) }.is_ok());
    let weak = Arc::downgrade(&shared);
    drop(shared);
    assert!(
        weak.upgrade().is_none(),
        "provider references do not retain a closed window's model"
    );
    assert_eq!(
        unsafe { held.GetPropertyValue(UIA_NamePropertyId) }
            .err()
            .expect("a dropped publication is unavailable")
            .code(),
        ELEMENT_NOT_AVAILABLE
    );
}

#[test]
fn retained_child_does_not_act_after_removal_or_reinsertion() {
    let (shared, actions) = fixture();
    let button = shared.provider(NodeId(model::NodeId(3))).unwrap();
    let original_id = shared
        .state
        .lock_or_recover()
        .publication
        .tree()
        .unwrap()
        .node_by_id(NodeId(model::NodeId(3)))
        .map(runtime_id)
        .unwrap();
    let invoke: IInvokeProvider = button.cast().unwrap();
    unsafe {
        invoke.Invoke().unwrap();
    }
    assert_eq!(actions.lock_or_recover().len(), 1);
    assert_eq!(actions.lock_or_recover()[0].target_node, model::NodeId(3));
    assert_eq!(
        actions.lock_or_recover()[0].context,
        Some(model::ActionContext::new(41, 7))
    );
    shared
        .state
        .lock_or_recover()
        .publication
        .update(update(false));
    assert_eq!(
        unsafe { invoke.Invoke() }.unwrap_err().code(),
        ELEMENT_NOT_AVAILABLE
    );
    shared
        .state
        .lock_or_recover()
        .publication
        .update(update(true));
    let reinserted_id = shared
        .state
        .lock_or_recover()
        .publication
        .tree()
        .unwrap()
        .node_by_id(NodeId(model::NodeId(3)))
        .map(runtime_id)
        .unwrap();
    assert_ne!(
        original_id, reinserted_id,
        "same id and context still name a new insertion"
    );
    assert_eq!(
        unsafe { invoke.Invoke() }.unwrap_err().code(),
        ELEMENT_NOT_AVAILABLE
    );
    let fresh: IInvokeProvider = shared
        .provider(NodeId(model::NodeId(3)))
        .unwrap()
        .cast()
        .unwrap();
    unsafe {
        fresh.Invoke().unwrap();
    }
    assert_eq!(actions.lock_or_recover().len(), 2);
    let before = shared
        .state
        .lock_or_recover()
        .publication
        .tree()
        .and_then(|tree| tree.node_by_id(NodeId(model::NodeId(3))))
        .map(runtime_id)
        .unwrap();
    let mut replacement = update(true);
    replacement
        .nodes
        .iter_mut()
        .find(|(id, _)| *id == model::NodeId(3))
        .unwrap()
        .1
        .set_action_context(model::ActionContext::new(41, 8));
    shared
        .state
        .lock_or_recover()
        .publication
        .update(replacement);
    assert_eq!(
        unsafe { fresh.Invoke() }.unwrap_err().code(),
        ELEMENT_NOT_AVAILABLE
    );
    let after = shared
        .state
        .lock_or_recover()
        .publication
        .tree()
        .and_then(|tree| tree.node_by_id(NodeId(model::NodeId(3))))
        .map(runtime_id)
        .unwrap();
    assert_ne!(
        before, after,
        "UIA runtime identity carries the exact lifecycle too"
    );
    let current: IInvokeProvider = shared
        .provider(NodeId(model::NodeId(3)))
        .unwrap()
        .cast()
        .unwrap();
    unsafe {
        current.Invoke().unwrap();
    }
    assert_eq!(
        actions.lock_or_recover()[2].context,
        Some(model::ActionContext::new(41, 8))
    );
    assert_eq!(
        actions.lock_or_recover()[0].context,
        Some(model::ActionContext::new(41, 7)),
        "queued receipt retains original context"
    );
}

#[test]
fn text_methods_navigate_model_characters_and_refuse_terminal_mutation() {
    let (shared, actions) = fixture();
    let text: ITextProvider2 = shared.root().unwrap().cast().unwrap();
    let range = unsafe { text.DocumentRange() }.unwrap();
    assert_eq!(
        unsafe { range.GetText(-1) }.unwrap().to_string(),
        "A😀e\u{301}\n"
    );
    assert_eq!(unsafe { range.GetText(2) }.unwrap().to_string(), "A");
    assert_eq!(unsafe { range.Move(TextUnit_Word, 0) }.unwrap(), 0);
    assert_eq!(
        unsafe { range.GetText(-1) }.unwrap().to_string(),
        "A😀e\u{301}\n",
        "zero movement must not normalize or shrink a nondegenerate range"
    );
    assert_eq!(
        unsafe { text.SupportedTextSelection() }.unwrap(),
        SupportedTextSelection_None
    );
    assert_eq!(unsafe { range.Select() }.unwrap_err().code(), NOT_SUPPORTED);
    assert!(actions.lock_or_recover().is_empty());
    let clone = unsafe { range.Clone() }.unwrap();
    unsafe {
        clone.ExpandToEnclosingUnit(TextUnit_Character).unwrap();
    }
    assert_eq!(unsafe { clone.GetText(-1) }.unwrap().to_string(), "A");
    assert_eq!(unsafe { clone.Move(TextUnit_Character, 1) }.unwrap(), 1);
    assert_eq!(unsafe { clone.GetText(-1) }.unwrap().to_string(), "😀");
    assert_eq!(unsafe { clone.Move(TextUnit_Character, 1) }.unwrap(), 1);
    assert_eq!(
        unsafe { clone.GetText(-1) }.unwrap().to_string(),
        "e\u{301}"
    );
    assert_eq!(
        unsafe { range.GetText(-1) }.unwrap().to_string(),
        "A😀e\u{301}\n",
        "Clone has independent endpoints"
    );
    let found = unsafe { range.FindText(&BSTR::from("😀"), false, false) }.unwrap();
    assert!(unsafe { found.Compare(&clone) }.is_ok());
    assert_eq!(unsafe { found.GetText(-1) }.unwrap().to_string(), "😀");
    let mut active = BOOL::from(false);
    let caret = unsafe { text.GetCaretRange(&mut active) }.unwrap();
    assert_eq!(unsafe { caret.GetText(-1) }.unwrap().to_string(), "");
    assert_eq!(
        unsafe {
            caret.CompareEndpoints(
                TextPatternRangeEndpoint_Start,
                &found,
                TextPatternRangeEndpoint_Start,
            )
        }
        .unwrap(),
        0
    );
    let mut editor = update(true);
    let root = &mut editor.nodes[0].1;
    root.set_role(Role::Document);
    root.set_action_context(model::ActionContext::new(41, 9));
    root.add_action(Action::SetTextSelection);
    shared.state.lock_or_recover().publication.update(editor);
    assert_eq!(
        unsafe { range.Select() }.unwrap_err().code(),
        ELEMENT_NOT_AVAILABLE,
        "a range pins even a document hosted at the fragment root"
    );
    assert_eq!(
        unsafe { range.GetEnclosingElement() }.unwrap_err().code(),
        ELEMENT_NOT_AVAILABLE
    );
    let pattern: ITextProvider2 = unsafe {
        shared
            .root()
            .unwrap()
            .GetPatternProvider(UIA_TextPattern2Id)
    }
    .unwrap()
    .cast()
    .unwrap();
    let fresh = unsafe { pattern.DocumentRange() }.unwrap();
    assert_eq!(
        unsafe { text.SupportedTextSelection() }.unwrap(),
        SupportedTextSelection_Single
    );
    unsafe {
        fresh.Select().unwrap();
    }
    assert_eq!(
        unsafe { fresh.AddToSelection() }.unwrap_err().code(),
        NOT_SUPPORTED
    );
    assert_eq!(
        actions.lock_or_recover().len(),
        1,
        "single-selection addition must not replace the selection"
    );
    let enclosing = unsafe { fresh.GetEnclosingElement() }.unwrap();
    let selected: ITextProvider2 = enclosing.cast().unwrap();
    assert_eq!(
        unsafe { selected.SupportedTextSelection() }.unwrap(),
        SupportedTextSelection_Single
    );
    let action = actions.lock_or_recover()[0].clone();
    assert_eq!(action.action, Action::SetTextSelection);
    assert_eq!(action.context, Some(model::ActionContext::new(41, 9)));
    let Some(ActionData::SetTextSelection(selection)) = action.data else {
        panic!("selection data missing");
    };
    assert_eq!(selection.anchor.character_index, 0);
    assert_eq!(selection.focus.character_index, 4);
    let mut replacement = update(true);
    replacement.nodes[0]
        .1
        .set_action_context(model::ActionContext::new(41, 10));
    replacement.nodes[0].1.add_action(Action::SetTextSelection);
    shared
        .state
        .lock_or_recover()
        .publication
        .update(replacement);
    assert_eq!(
        unsafe { pattern.DocumentRange() }.unwrap_err().code(),
        ELEMENT_NOT_AVAILABLE,
        "a held pattern cannot retarget a replacement document"
    );
    shared.closed.store(true, Ordering::Release);
    assert_eq!(
        unsafe { fresh.GetText(-1) }.unwrap_err().code(),
        ELEMENT_NOT_AVAILABLE
    );
}

#[test]
fn text_ranges_from_another_window_cannot_supply_endpoints() {
    let (a, _) = fixture();
    let (b, _) = fixture();
    let a: ITextProvider = a.root().unwrap().cast().unwrap();
    let b: ITextProvider = b.root().unwrap().cast().unwrap();
    let one = unsafe { a.DocumentRange() }.unwrap();
    let other = unsafe { b.DocumentRange() }.unwrap();
    assert_eq!(
        unsafe {
            one.MoveEndpointByRange(
                TextPatternRangeEndpoint_Start,
                &other,
                TextPatternRangeEndpoint_End,
            )
        }
        .unwrap_err()
        .code(),
        E_INVALIDARG
    );
}

#[test]
fn absent_hwnd_and_other_object_ids_are_inert() {
    assert!(
        unsafe { Adapter::new(0, Activation, Actions(Arc::new(Mutex::new(Vec::new())))) }.is_none()
    );
    assert_eq!(handle_wm_getobject(0, 0, UiaRootObjectId as isize), None);
    assert_eq!(handle_wm_getobject(0, 0, -4), None);
}

#[test]
fn nullable_native_results_write_null_or_an_owned_interface_and_reject_null_slots() {
    use std::ffi::c_void;
    use windows::Win32::Foundation::E_POINTER;

    let (shared, _) = fixture();
    let root = shared.root().unwrap();
    let vtable = Interface::vtable(&root);
    let sentinel = std::ptr::dangling_mut::<c_void>();
    let mut out = sentinel;
    unsafe {
        assert_eq!(
            (vtable.GetPatternProvider)(root.as_raw(), UIA_ValuePatternId, &mut out),
            HRESULT(0)
        );
        assert!(
            out.is_null(),
            "S_OK without a pattern initializes the caller's slot"
        );
        assert_eq!(
            (vtable.GetPatternProvider)(root.as_raw(), UIA_TextPattern2Id, &mut out),
            HRESULT(0)
        );
        assert!(!out.is_null());
        let pattern = IUnknown::from_raw(out);
        assert!(
            pattern.cast::<ITextProvider2>().is_ok(),
            "returned reference is owned and queryable"
        );
        assert_eq!(
            (vtable.GetPatternProvider)(root.as_raw(), UIA_TextPattern2Id, std::ptr::null_mut()),
            E_POINTER
        );
    }
    let fragment: IRawElementProviderFragment = root.cast().unwrap();
    let fragment_vtable = Interface::vtable(&fragment);
    let mut runtime = std::ptr::dangling_mut::<SAFEARRAY>();
    unsafe {
        assert_eq!(
            (fragment_vtable.GetRuntimeId)(fragment.as_raw(), &mut runtime),
            HRESULT(0)
        );
        assert!(
            runtime.is_null(),
            "the HWND root delegates its runtime id to its host"
        );
        assert_eq!(
            (fragment_vtable.GetRuntimeId)(fragment.as_raw(), std::ptr::null_mut()),
            E_POINTER
        );
    }
    out = sentinel;
    unsafe {
        assert_eq!(
            (fragment_vtable.Navigate)(fragment.as_raw(), NavigateDirection_Parent, &mut out),
            HRESULT(0)
        );
        assert!(out.is_null(), "the fragment root has no parent");
        assert_eq!(
            (fragment_vtable.Navigate)(
                fragment.as_raw(),
                NavigateDirection_Parent,
                std::ptr::null_mut()
            ),
            E_POINTER
        );
    }
    let child = shared.provider(NodeId(model::NodeId(3))).unwrap();
    out = sentinel;
    unsafe {
        assert_eq!(
            (Interface::vtable(&child).HostRawElementProvider)(child.as_raw(), &mut out),
            HRESULT(0)
        );
        assert!(out.is_null(), "a fragment child has no host HWND provider");
    }
    let mut selectable = update(true);
    selectable.nodes[2].1.set_selected(false);
    shared
        .state
        .lock_or_recover()
        .publication
        .update(selectable);
    let item: ISelectionItemProvider = shared
        .provider(NodeId(model::NodeId(3)))
        .unwrap()
        .cast()
        .unwrap();
    out = sentinel;
    unsafe {
        assert_eq!(
            (Interface::vtable(&item).SelectionContainer)(item.as_raw(), &mut out),
            HRESULT(0)
        );
        assert!(
            out.is_null(),
            "a selected node without a selection container returns null"
        );
        assert_eq!(
            (Interface::vtable(&item).SelectionContainer)(item.as_raw(), std::ptr::null_mut()),
            E_POINTER
        );
    }
    let text: ITextProvider2 = root.cast().unwrap();
    let range = unsafe { text.DocumentRange() }.unwrap();
    let needle = BSTR::from("absent");
    out = sentinel;
    unsafe {
        assert_eq!(
            (Interface::vtable(&range).FindText)(
                range.as_raw(),
                needle.as_ptr() as *mut c_void,
                false.into(),
                false.into(),
                &mut out
            ),
            HRESULT(0)
        );
        assert!(out.is_null(), "an unmatched search returns S_OK/null");
        assert_eq!(
            (Interface::vtable(&range).FindText)(
                range.as_raw(),
                needle.as_ptr() as *mut c_void,
                false.into(),
                false.into(),
                std::ptr::null_mut()
            ),
            E_POINTER
        );
    }
}
