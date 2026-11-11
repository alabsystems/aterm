// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/event.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use crate::event;

macro_rules! foreach_event {
    ($closure:expr) => {{
        #[allow(unused_mut)]
        let mut x = $closure;
        let did = event::DeviceId::dummy();

        #[allow(deprecated)]
        {
            use crate::event::Event::*;
            use crate::event::Ime::Enabled;
            use crate::event::WindowEvent::*;
            use crate::window::WindowId;

            let wid = WindowId::dummy();
            x(UserEvent(()));
            x(NewEvents(event::StartCause::Init));
            x(AboutToWait);
            x(LoopExiting);
            x(Suspended);
            x(Resumed);

            let with_window_event = |wev| x(WindowEvent { window_id: wid, event: wev });

            with_window_event(CloseRequested);
            with_window_event(Destroyed);
            with_window_event(Focused(true));
            with_window_event(Moved((17, -9).into()));
            with_window_event(Resized((321, 123).into()));
            with_window_event(DroppedFile("x.txt".into()));
            with_window_event(HoveredFile("x.txt".into()));
            with_window_event(HoveredFileCancelled);
            with_window_event(Ime(Enabled));
            with_window_event(CursorMoved { device_id: did, position: (0, 0).into() });
            with_window_event(ModifiersChanged(event::Modifiers::default()));
            with_window_event(CursorEntered { device_id: did });
            with_window_event(CursorLeft { device_id: did });
            with_window_event(MouseWheel {
                device_id: did,
                delta: event::MouseScrollDelta::LineDelta(0.0, 0.0),
                phase: event::TouchPhase::Started,
            });
            with_window_event(MouseInput {
                device_id: did,
                state: event::ElementState::Pressed,
                button: event::MouseButton::Other(0),
            });
            with_window_event(AxisMotion { device_id: did, axis: 0, value: 0.0 });
            with_window_event(Touch(event::Touch {
                device_id: did,
                phase: event::TouchPhase::Started,
                location: (0.0, 0.0).into(),
                id: 0,
                force: Some(event::Force::Normalized(0.0)),
            }));
            with_window_event(ThemeChanged(crate::window::Theme::Light));
            with_window_event(Occluded(true));
        }

        #[allow(deprecated)]
        {
            use event::DeviceEvent::*;

            let with_device_event =
                |dev_ev| x(event::Event::DeviceEvent { device_id: did, event: dev_ev });

            with_device_event(Added);
            with_device_event(Removed);
            with_device_event(MouseMotion { delta: (0.0, 0.0).into() });
            with_device_event(MouseWheel { delta: event::MouseScrollDelta::LineDelta(0.0, 0.0) });
            with_device_event(Motion { axis: 0, value: 0.0 });
            with_device_event(Button { button: 0, state: event::ElementState::Pressed });
        }
    }};
}

#[test]
fn test_map_nonuser_event() {
    foreach_event!(|event: event::Event<()>| {
        let is_user = matches!(event, event::Event::UserEvent(()));
        let expected = event.clone();
        let event2 = event.map_nonuser_event::<()>();
        if is_user {
            assert_eq!(event2, Err(event::Event::UserEvent(())));
        } else {
            assert_eq!(event2, Ok(expected), "variant and payload survive mapping");
        }
    })
}
