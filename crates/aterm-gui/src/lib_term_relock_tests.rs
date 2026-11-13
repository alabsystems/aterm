// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_relock_on_the_holding_thread_panics_naming_the_hold() {
    let term = Arc::new(Mutex::new(Terminal::new(2, 10)));
    let (tx, rx) = std::sync::mpsc::channel();
    let t = Arc::clone(&term);
    std::thread::spawn(move || {
        let relock = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            (term_lock(&t).content_seq(), term_lock(&t).content_seq())
        }));
        let message = relock.err().and_then(|e| e.downcast::<String>().ok());
        let _ = tx.send(message.map(|m| *m));
    });
    let message = rx
        .recv_timeout(Duration::from_secs(60))
        .expect("the relock neither returned nor panicked")
        .expect("the relock panics");
    let site = |after: &str, before: &str| {
        let rest = message
            .split_once(after)
            .map(|(_, r)| r)
            .unwrap_or_default();
        let at = rest.split_once(before).map(|(a, _)| a).unwrap_or_default();
        at.rsplit_once(':').map(|(line, _)| line.to_string())
    };
    let relock = site("term_lock at ", " relocks");
    let hold = site("held since ", ": the lock");
    assert!(
        relock.as_deref().is_some_and(|l| l.starts_with(file!())) && relock == hold,
        "names the hold at the relock's own line: {message}"
    );
    let other = Mutex::new(Terminal::new(2, 10));
    let next = Mutex::new(Terminal::new(2, 10));
    let _a = term_lock(&other);
    let _b = term_lock(&next);
}
