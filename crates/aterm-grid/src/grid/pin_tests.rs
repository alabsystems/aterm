// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests for grid pin and generation tracking.

use crate::{GenerationTracker, Pin, PinnedRange};

#[test]
fn pin_creation() {
    let pin = Pin::new(0, 10, 5, 0);
    assert_eq!(pin.page_id(), 0);
    assert_eq!(pin.row_offset(), 10);
    assert_eq!(pin.col(), 5);
    assert_eq!(pin.generation(), 0);
}

#[test]
fn pin_from_absolute() {
    let pin = Pin::from_absolute(1000, 42, 5);
    assert_eq!(pin.absolute_row(), 1000);
    assert_eq!(pin.col(), 42);
    assert_eq!(pin.generation(), 5);
}

#[test]
fn pin_with_modifications() {
    let pin = Pin::new(0, 10, 5, 0);
    let pin2 = pin.with_col(20);
    assert_eq!(pin2.col(), 20);
    assert_eq!(pin2.row_offset(), 10);

    let pin3 = pin.with_row_offset(50);
    assert_eq!(pin3.row_offset(), 50);
    assert_eq!(pin3.col(), 5);
}

#[test]
fn generation_tracker_new() {
    let tracker = GenerationTracker::new();
    assert_eq!(tracker.current_generation(), 0);
    assert_eq!(tracker.page_generation(0), 0);
    assert_eq!(tracker.page_generation(100), 0);
}

#[test]
fn generation_tracker_evict() {
    let mut tracker = GenerationTracker::new();
    tracker.ensure_capacity(3);

    // Evict page 1
    tracker.evict_page(1);
    assert_eq!(tracker.page_generation(0), 0);
    assert_eq!(tracker.page_generation(1), 1);
    assert_eq!(tracker.page_generation(2), 0);
    assert_eq!(tracker.current_generation(), 1);

    // Evict page 1 again
    tracker.evict_page(1);
    assert_eq!(tracker.page_generation(1), 2);
    assert_eq!(tracker.current_generation(), 2);
}

#[test]
fn pin_validity() {
    let mut tracker = GenerationTracker::new();
    tracker.ensure_capacity(2);

    // Create a pin at current generation
    let pin = Pin::new(0, 10, 5, tracker.page_generation(0));
    assert!(tracker.is_valid(&pin));
    assert!(tracker.is_potentially_valid(&pin));

    // Evict the page
    tracker.evict_page(0);
    assert!(!tracker.is_valid(&pin));

    // Create new pin at new generation
    let pin2 = Pin::new(0, 10, 5, tracker.page_generation(0));
    assert!(tracker.is_valid(&pin2));
}

#[test]
fn evict_pages_from() {
    let mut tracker = GenerationTracker::new();
    tracker.ensure_capacity(5);

    // Create pins on different pages
    let pin0 = Pin::new(0, 0, 0, 0);
    let pin2 = Pin::new(2, 0, 0, 0);
    let pin4 = Pin::new(4, 0, 0, 0);

    // All should be valid initially
    assert!(tracker.is_valid(&pin0));
    assert!(tracker.is_valid(&pin2));
    assert!(tracker.is_valid(&pin4));

    // Evict pages 2 and above
    tracker.evict_pages_from(2);

    // Page 0 and 1 should still be valid, 2+ should be invalid
    assert!(tracker.is_valid(&pin0));
    assert!(!tracker.is_valid(&pin2));
    assert!(!tracker.is_valid(&pin4));
}

#[test]
fn pinned_range() {
    let start = Pin::from_absolute(100, 10, 0);
    let end = Pin::from_absolute(200, 20, 0);
    let range = PinnedRange::new(start, end);

    assert_eq!(range.start.absolute_row(), 100);
    assert_eq!(range.end.absolute_row(), 200);
}

#[test]
fn pinned_range_normalized() {
    // End before start
    let start = Pin::from_absolute(200, 10, 0);
    let end = Pin::from_absolute(100, 20, 0);
    let range = PinnedRange::new(start, end).normalized();

    assert_eq!(range.start.absolute_row(), 100);
    assert_eq!(range.end.absolute_row(), 200);

    // Same row, end col before start col
    let start = Pin::from_absolute(100, 50, 0);
    let end = Pin::from_absolute(100, 10, 0);
    let range = PinnedRange::new(start, end).normalized();

    assert_eq!(range.start.col(), 10);
    assert_eq!(range.end.col(), 50);
}

#[test]
fn pinned_range_validity() {
    let mut tracker = GenerationTracker::new();
    tracker.ensure_capacity(3);

    let start = Pin::new(0, 0, 0, 0);
    let end = Pin::new(2, 0, 0, 0);
    let range = PinnedRange::new(start, end);

    assert!(range.is_valid(&tracker));

    // Evict page 2
    tracker.evict_page(2);
    assert!(!range.is_valid(&tracker));
}
