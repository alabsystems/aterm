// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests for vi mode types: coordinates, motions, marks, and conversions.

use super::*;

#[test]
fn vi_point_default_is_origin() {
    let p = ViPoint::default();
    assert_eq!(p.line, 0);
    assert_eq!(p.col, 0);
}

#[test]
fn vi_motion_directions() {
    assert_eq!(ViMotion::Up.direction(), ViDirection::Left);
    assert_eq!(ViMotion::Down.direction(), ViDirection::Right);
    assert_eq!(ViMotion::Left.direction(), ViDirection::Left);
    assert_eq!(ViMotion::Right.direction(), ViDirection::Right);
    assert_eq!(ViMotion::First.direction(), ViDirection::Left);
    assert_eq!(ViMotion::Last.direction(), ViDirection::Right);
    assert_eq!(ViMotion::High.direction(), ViDirection::Left);
    assert_eq!(ViMotion::Middle.direction(), ViDirection::Right);
    assert_eq!(ViMotion::Low.direction(), ViDirection::Right);
    assert_eq!(ViMotion::SearchNext.direction(), ViDirection::Right);
    assert_eq!(ViMotion::SearchPrevious.direction(), ViDirection::Left);
    assert_eq!(ViMotion::GotoMark('a').direction(), ViDirection::Right);
}

#[test]
fn inline_search_kind_directions() {
    assert_eq!(InlineSearchKind::FindRight.direction(), ViDirection::Right);
    assert_eq!(InlineSearchKind::FindLeft.direction(), ViDirection::Left);
    assert_eq!(InlineSearchKind::TillRight.direction(), ViDirection::Right);
    assert_eq!(InlineSearchKind::TillLeft.direction(), ViDirection::Left);
}

#[test]
fn inline_search_kind_reverse() {
    assert_eq!(
        InlineSearchKind::FindRight.reversed(),
        InlineSearchKind::FindLeft
    );
    assert_eq!(
        InlineSearchKind::TillRight.reversed(),
        InlineSearchKind::TillLeft
    );
}

#[test]
fn inline_search_kind_is_till() {
    assert!(!InlineSearchKind::FindRight.is_till());
    assert!(InlineSearchKind::TillRight.is_till());
    assert!(InlineSearchKind::TillLeft.is_till());
}

#[test]
fn vi_marks_set_get_remove() {
    let mut marks = ViMarks::new();
    let p = ViPoint::new(5, 10);

    assert!(marks.set('a', p));
    assert_eq!(marks.get('a'), Some(p));
    assert!(marks.contains('a'));

    assert_eq!(marks.remove('a'), Some(p));
    assert!(!marks.contains('a'));
}

#[test]
fn vi_marks_rejects_invalid_chars() {
    let mut marks = ViMarks::new();
    assert!(!marks.set('A', ViPoint::default()));
    assert!(!marks.set('1', ViPoint::default()));
}

#[test]
fn vi_marks_accepts_special_chars() {
    let mut marks = ViMarks::new();
    let p = ViPoint::new(1, 0);
    assert!(marks.set('\'', p));
    assert!(marks.set('`', p));
}

#[test]
fn vi_motion_to_buffer_command_up() {
    assert_eq!(
        BufferCommand::from(ViMotion::Up),
        BufferCommand::PreviousLine
    );
}

#[test]
fn vi_motion_to_buffer_command_down() {
    assert_eq!(BufferCommand::from(ViMotion::Down), BufferCommand::NextLine);
}

#[test]
fn vi_motion_to_buffer_command_navigation() {
    assert_eq!(
        BufferCommand::from(ViMotion::Left),
        BufferCommand::BackwardChar
    );
    assert_eq!(
        BufferCommand::from(ViMotion::Right),
        BufferCommand::ForwardChar
    );
    assert_eq!(
        BufferCommand::from(ViMotion::First),
        BufferCommand::BeginningOfLine
    );
    assert_eq!(
        BufferCommand::from(ViMotion::Last),
        BufferCommand::EndOfLine
    );
    assert_eq!(
        BufferCommand::from(ViMotion::FirstOccupied),
        BufferCommand::FirstNonBlank
    );
}

#[test]
fn vi_motion_to_buffer_command_screen_position() {
    assert_eq!(
        BufferCommand::from(ViMotion::High),
        BufferCommand::ScreenTop
    );
    assert_eq!(
        BufferCommand::from(ViMotion::Middle),
        BufferCommand::ScreenMiddle
    );
    assert_eq!(
        BufferCommand::from(ViMotion::Low),
        BufferCommand::ScreenBottom
    );
}

#[test]
fn vi_motion_to_buffer_command_word_motions() {
    assert_eq!(
        BufferCommand::from(ViMotion::SemanticLeft),
        BufferCommand::BackwardWord
    );
    assert_eq!(
        BufferCommand::from(ViMotion::SemanticRight),
        BufferCommand::ForwardWord
    );
    assert_eq!(
        BufferCommand::from(ViMotion::SemanticLeftEnd),
        BufferCommand::BackwardWordEnd
    );
    assert_eq!(
        BufferCommand::from(ViMotion::SemanticRightEnd),
        BufferCommand::ForwardWordEnd
    );
    assert_eq!(
        BufferCommand::from(ViMotion::WordLeft),
        BufferCommand::BackwardWordBig
    );
    assert_eq!(
        BufferCommand::from(ViMotion::WordRight),
        BufferCommand::ForwardWordBig
    );
    assert_eq!(
        BufferCommand::from(ViMotion::WordLeftEnd),
        BufferCommand::BackwardWordEndBig
    );
    assert_eq!(
        BufferCommand::from(ViMotion::WordRightEnd),
        BufferCommand::ForwardWordEndBig
    );
}

#[test]
fn vi_motion_to_buffer_command_structural() {
    assert_eq!(
        BufferCommand::from(ViMotion::Bracket),
        BufferCommand::MatchBracket
    );
    assert_eq!(
        BufferCommand::from(ViMotion::ParagraphUp),
        BufferCommand::ParagraphUp
    );
    assert_eq!(
        BufferCommand::from(ViMotion::ParagraphDown),
        BufferCommand::ParagraphDown
    );
}

#[test]
fn vi_motion_to_buffer_command_search() {
    assert_eq!(
        BufferCommand::from(ViMotion::SearchNext),
        BufferCommand::SearchNext
    );
    assert_eq!(
        BufferCommand::from(ViMotion::SearchPrevious),
        BufferCommand::SearchPrevious
    );
}

#[test]
fn vi_motion_to_buffer_command_marks() {
    assert_eq!(
        BufferCommand::from(ViMotion::GotoMark('a')),
        BufferCommand::GotoMark('a')
    );
    assert_eq!(
        BufferCommand::from(ViMotion::GotoMarkLine('b')),
        BufferCommand::GotoMarkLine('b')
    );
}

#[test]
fn vi_marks_clear() {
    let mut marks = ViMarks::new();
    marks.set('a', ViPoint::new(1, 0));
    marks.set('b', ViPoint::new(2, 0));
    marks.clear();
    assert!(!marks.contains('a'));
    assert!(!marks.contains('b'));
}
