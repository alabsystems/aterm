// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::RefCell;
use std::sync::{Mutex, OnceLock};

type Observer = Box<dyn Fn(&aterm_log::Record<'_>)>;
thread_local! {
    static OBSERVER: RefCell<Option<Observer>> = const { RefCell::new(None) };
}

/// One captured record: its level, its target and its rendered message.
#[derive(Clone, Debug)]
pub(crate) struct Captured {
    pub(crate) level: aterm_log::Level,
    pub(crate) target: String,
    pub(crate) message: String,
}

struct Capture;

fn captured() -> &'static Mutex<Vec<Captured>> {
    static CAPTURED: OnceLock<Mutex<Vec<Captured>>> = OnceLock::new();
    CAPTURED.get_or_init(|| Mutex::new(Vec::new()))
}

impl aterm_log::Log for Capture {
    fn enabled(&self, _metadata: &aterm_log::Metadata) -> bool {
        true
    }
    fn log(&self, record: &aterm_log::Record<'_>) {
        OBSERVER.with(|observer| {
            if let Some(observe) = observer.borrow().as_ref() {
                observe(record);
            }
        });
        captured()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(Captured {
                level: record.level(),
                target: record.target().to_string(),
                message: format!("{}", record.args()),
            });
    }
    fn flush(&self) {}
}

/// Install the capture.
pub(crate) fn install() {
    static LOGGER: Capture = Capture;
    let _ = aterm_log::set_logger(&LOGGER);
    aterm_log::set_max_level(aterm_log::LevelFilter::Info);
}

/// Observe this thread's actual logger calls during `run`, restoring any outer observer even if it unwinds.
pub(crate) fn observe<R>(
    observer: impl Fn(&aterm_log::Record<'_>) + 'static,
    run: impl FnOnce() -> R,
) -> R {
    struct Restore(Option<Observer>);
    impl Drop for Restore {
        fn drop(&mut self) {
            OBSERVER.with(|observer| observer.replace(self.0.take()));
        }
    }
    install();
    let _restore = Restore(OBSERVER.with(|slot| slot.replace(Some(Box::new(observer)))));
    run()
}

/// Every record this thread logs while `run` runs, by its rendered message ([`observe`]).
pub(crate) fn logged_while(run: impl FnOnce()) -> Vec<String> {
    let seen: std::rc::Rc<RefCell<Vec<String>>> = std::rc::Rc::default();
    let into = std::rc::Rc::clone(&seen);
    observe(
        move |record| into.borrow_mut().push(format!("{}", record.args())),
        run,
    );
    seen.take()
}

/// Every record captured so far whose message contains `mark`, oldest first.
pub(crate) fn containing(mark: &str) -> Vec<Captured> {
    captured()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .filter(|record| record.message.contains(mark))
        .cloned()
        .collect()
}
