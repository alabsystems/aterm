// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::sync::Mutex;

/// Recording notifier for testing.
///
/// Records all notifications for later inspection.
pub struct RecordingNotifier {
    notifications: Mutex<Vec<(String, String)>>,
    badge: Mutex<Option<u32>>,
    bell_count: Mutex<u32>,
}

impl RecordingNotifier {
    fn new() -> Self {
        Self {
            notifications: Mutex::new(Vec::new()),
            badge: Mutex::new(None),
            bell_count: Mutex::new(0),
        }
    }

    fn notifications(&self) -> Vec<(String, String)> {
        self.notifications
            .lock()
            .expect("notifications mutex poisoned")
            .clone()
    }

    fn badge(&self) -> Option<u32> {
        *self.badge.lock().expect("badge mutex poisoned")
    }

    fn bell_count(&self) -> u32 {
        *self.bell_count.lock().expect("bell count mutex poisoned")
    }
}

impl Notifier for RecordingNotifier {
    fn notify(&self, title: &str, body: &str) {
        self.notifications
            .lock()
            .expect("notifications mutex poisoned")
            .push((title.to_string(), body.to_string()));
    }

    fn set_badge(&self, count: Option<u32>) {
        *self.badge.lock().expect("badge mutex poisoned") = count;
    }

    fn set_badge_format(&self, _format: &str) {
        // No-op: badge format recording removed (no tests read this value).
    }

    fn bell(&self) {
        *self.bell_count.lock().expect("bell count mutex poisoned") += 1;
    }
}

/// Recording opener for testing.
///
/// Records all open attempts for later inspection.
pub struct RecordingOpener {
    urls: Mutex<Vec<String>>,
    files: Mutex<Vec<String>>,
    revealed: Mutex<Vec<String>>,
}

impl RecordingOpener {
    fn new() -> Self {
        Self {
            urls: Mutex::new(Vec::new()),
            files: Mutex::new(Vec::new()),
            revealed: Mutex::new(Vec::new()),
        }
    }

    fn urls(&self) -> Vec<String> {
        self.urls.lock().expect("urls mutex poisoned").clone()
    }

    fn files(&self) -> Vec<String> {
        self.files.lock().expect("files mutex poisoned").clone()
    }

    fn revealed(&self) -> Vec<String> {
        self.revealed
            .lock()
            .expect("revealed mutex poisoned")
            .clone()
    }
}

impl Opener for RecordingOpener {
    fn open_url(&self, url: &str) -> Result<(), OpenError> {
        self.urls
            .lock()
            .expect("urls mutex poisoned")
            .push(url.to_string());
        Ok(())
    }

    fn open_file(&self, path: &Path) -> Result<(), OpenError> {
        self.files
            .lock()
            .expect("files mutex poisoned")
            .push(path.display().to_string());
        Ok(())
    }

    fn reveal_file(&self, path: &Path) -> Result<(), OpenError> {
        self.revealed
            .lock()
            .expect("revealed mutex poisoned")
            .push(path.display().to_string());
        Ok(())
    }
}

#[test]
fn test_recording_notifier() {
    let notifier = RecordingNotifier::new();
    notifier.notify("Title 1", "Body 1");
    notifier.notify("Title 2", "Body 2");
    notifier.set_badge(Some(5));
    notifier.bell();
    notifier.bell();

    let notifications = notifier.notifications();
    assert_eq!(notifications.len(), 2);
    assert_eq!(
        notifications[0],
        ("Title 1".to_string(), "Body 1".to_string())
    );
    assert_eq!(notifier.badge(), Some(5));
    assert_eq!(notifier.bell_count(), 2);
}

#[test]
fn test_recording_opener() {
    let opener = RecordingOpener::new();
    opener.open_url("https://example.com").unwrap();
    opener.open_file(Path::new("/tmp/test.txt")).unwrap();
    opener.reveal_file(Path::new("/tmp/dir")).unwrap();

    assert_eq!(opener.urls(), vec!["https://example.com"]);
    assert_eq!(opener.files(), vec!["/tmp/test.txt"]);
    assert_eq!(opener.revealed(), vec!["/tmp/dir"]);
}

#[test]
fn test_stub_shaper() {
    let shaper = StubTextShaper;
    let provider = StubFontProvider::new();
    let font = provider.load_font(&FontDescriptor::default()).unwrap();

    let run = TextRun {
        text: "abc".to_string(),
        start_column: 0,
        row: 0,
        config: None,
        cursor: None,
    };
    let glyphs = shaper.shape(&run, &font);

    assert_eq!(glyphs.len(), 3);
    assert_eq!(glyphs[0].glyph_id, 'a' as u32);
    assert_eq!(glyphs[1].glyph_id, 'b' as u32);
    assert_eq!(glyphs[2].glyph_id, 'c' as u32);
}

#[test]
fn test_clipboard_selection() {
    let clipboard = StubClipboard::new();

    // Write to main clipboard
    clipboard.write("main").unwrap();
    assert_eq!(clipboard.read(), Some("main".to_string()));
    assert_eq!(clipboard.read_selection(), None);

    // Write to selection
    clipboard.write_selection("selection").unwrap();
    assert_eq!(clipboard.read(), Some("main".to_string()));
    assert_eq!(clipboard.read_selection(), Some("selection".to_string()));
}
