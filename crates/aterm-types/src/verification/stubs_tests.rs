// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for verification-friendly stubs.

use super::*;
use std::time::Duration;

// Note: VerifyInstant tests share a global `AtomicU64` counter, so parallel
// tests interleave ticks. That is fine because these tests only assert
// monotonicity (t1 < t2 < t3), not specific tick values, and `fetch_add`
// hands each caller a distinct, increasing tick.

#[test]
fn verify_instant_monotonic() {
    let t1 = VerifyInstant::now();
    let t2 = VerifyInstant::now();
    let t3 = VerifyInstant::now();

    assert!(t1 < t2);
    assert!(t2 < t3);
}

#[test]
fn verify_instant_arithmetic() {
    let t = VerifyInstant { ticks: 100 };
    let later = t + Duration::from_millis(50);
    assert_eq!(later.ticks, 150);

    let earlier = t - Duration::from_millis(30);
    assert_eq!(earlier.ticks, 70);
}

#[test]
fn verify_instant_duration_since() {
    let t1 = VerifyInstant { ticks: 100 };
    let t2 = VerifyInstant { ticks: 150 };

    let duration = t2.duration_since(t1);
    assert_eq!(duration, Duration::from_millis(50));
}

#[test]
fn verify_set_basic() {
    let mut set = VerifySet::new();
    assert!(set.is_empty());

    set.insert(1);
    set.insert(2);
    set.insert(3);

    assert_eq!(set.len(), 3);
    assert!(set.contains(&2));
    assert!(!set.contains(&4));

    set.remove(&2);
    assert!(!set.contains(&2));
    assert_eq!(set.len(), 2);
}

#[test]
fn verify_set_subset() {
    let mut a = VerifySet::new();
    a.insert(1);
    a.insert(2);

    let mut b = VerifySet::new();
    b.insert(1);
    b.insert(2);
    b.insert(3);

    assert!(a.is_subset(&b));
    assert!(!b.is_subset(&a));
    assert!(b.is_superset(&a));
}

#[test]
fn verify_set_contains_ref() {
    let mut set = VerifySet::new();
    set.insert("ls".to_string());
    set.insert("cat".to_string());

    assert!(set.contains_ref("ls"));
    assert!(!set.contains_ref("rm"));
}

#[test]
fn verify_set_equality() {
    // Sets with same elements in different order should be equal
    let mut a = VerifySet::new();
    a.insert(1);
    a.insert(2);
    a.insert(3);

    let mut b = VerifySet::new();
    b.insert(3);
    b.insert(1);
    b.insert(2);

    assert_eq!(a, b, "Sets with same elements should be equal");

    // Different length = not equal
    let mut c = VerifySet::new();
    c.insert(1);
    c.insert(2);
    assert_ne!(a, c);

    // Different elements = not equal
    let mut d = VerifySet::new();
    d.insert(1);
    d.insert(2);
    d.insert(4);
    assert_ne!(a, d);
}

#[test]
fn verify_set_drain_preserves_remaining_order() {
    let mut set = VerifySet::new();
    set.insert(1);
    set.insert(2);
    set.insert(3);
    set.insert(4);

    let drained: Vec<_> = set.drain(..2).collect();

    assert_eq!(drained, vec![1, 2]);
    assert_eq!(set.into_iter().collect::<Vec<_>>(), vec![3, 4]);
}

#[test]
fn verify_map_basic() {
    let mut map = VerifyMap::new();
    assert!(map.is_empty());

    map.insert("a", 1);
    map.insert("b", 2);

    assert_eq!(map.len(), 2);
    assert_eq!(map.get(&"a"), Some(&1));
    assert_eq!(map.get(&"c"), None);

    map.remove(&"a");
    assert!(!map.contains_key(&"a"));
}

#[test]
fn verify_map_iteration() {
    let mut map = VerifyMap::new();
    map.insert("c", 3);
    map.insert("a", 1);
    map.insert("b", 2);

    // Test reference iteration (sorted by key due to BTreeMap)
    let pairs: Vec<_> = (&map).into_iter().collect();
    assert_eq!(pairs, vec![(&"a", &1), (&"b", &2), (&"c", &3)]);

    // Test owned iteration
    let owned_pairs: Vec<_> = map.into_iter().collect();
    assert_eq!(owned_pairs, vec![("a", 1), ("b", 2), ("c", 3)]);
}

#[test]
fn verify_map_advanced_ops() {
    let mut map = VerifyMap::new();
    map.insert("a", 1);
    map.insert("b", 2);
    map.insert("c", 3);

    // Test iter_mut - double all values
    for (_k, v) in map.iter_mut() {
        *v *= 2;
    }
    assert_eq!(map.get(&"a"), Some(&2));
    assert_eq!(map.get(&"b"), Some(&4));
    assert_eq!(map.get(&"c"), Some(&6));

    // Test &mut map IntoIterator
    for (_k, v) in &mut map {
        *v += 1;
    }
    assert_eq!(map.get(&"a"), Some(&3));

    // Test get_mut
    if let Some(v) = map.get_mut(&"b") {
        *v = 100;
    }
    assert_eq!(map.get(&"b"), Some(&100));

    // Test keys and values
    let keys: Vec<_> = map.keys().collect();
    assert_eq!(keys, vec![&"a", &"b", &"c"]);

    let values: Vec<_> = map.values().collect();
    assert_eq!(values, vec![&3, &100, &7]);

    // Test values_mut - triple all values
    for v in map.values_mut() {
        *v *= 3;
    }
    assert_eq!(map.get(&"a"), Some(&9));
    assert_eq!(map.get(&"b"), Some(&300));
    assert_eq!(map.get(&"c"), Some(&21));

    // Test retain - keep only values > 100
    map.retain(|_k, v| *v > 100);
    assert_eq!(map.len(), 1);
    assert!(map.contains_key(&"b"));
    assert!(!map.contains_key(&"a"));
    assert!(!map.contains_key(&"c"));

    // Test entry API
    map.entry("d").or_insert(50);
    assert_eq!(map.get(&"d"), Some(&50));
    *map.entry("d").or_insert(0) += 10;
    assert_eq!(map.get(&"d"), Some(&60));
}

#[test]
fn verify_deque_back() {
    let mut deque = VerifyDeque::new();
    assert_eq!(deque.back(), None);

    deque.push_back(1);
    assert_eq!(deque.back(), Some(&1));

    deque.push_back(2);
    assert_eq!(deque.back(), Some(&2));

    deque.push_back(3);
    assert_eq!(deque.back(), Some(&3));

    // pop_front doesn't change back
    deque.pop_front();
    assert_eq!(deque.back(), Some(&3));

    // drain until empty - back returns None
    deque.pop_front();
    deque.pop_front();
    assert!(deque.is_empty());
    assert_eq!(deque.back(), None);
}
