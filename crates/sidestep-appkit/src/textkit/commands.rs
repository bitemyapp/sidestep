//! A text view's editing commands: the `NSStandardKeyBindingResponding`
//! methods the key bindings send, and the clipboard's `copy:`, `cut:`,
//! `paste:` and the rest, as AppKit's text view does them (measured on
//! macOS by `conformance/tests/text_view.rs`).
//!
//! Moves go by characters (grapheme clusters), words, visual lines (from
//! the layout manager), paragraphs, pages and the document; their
//! `…AndModifySelection` forms move the selection's moving end and keep
//! the other. Vertical moves keep a goal: the x they started at, so going
//! down through a short line comes back to the same column. Deletions of
//! lines and paragraphs go to the kill buffer, which `yank:` inserts;
//! edits go through the view's transaction (`edit`), so they ask the
//! delegate and can be undone. In a field editor, Return, Tab and Backtab
//! end editing, with the movement in the notification, rather than insert.
//!
//! The methods are a link-time category on `NSTextView`: a helper class
//! holds them for their encodings, and they treat the receiver as a text
//! view.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, Sel};
use objc2::{ClassType, DefinedClass, define_class, msg_send, sel};
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString, NSTextStorage};
use objc2_foundation::{NSArray, NSPoint, NSRange, NSString};

use super::edit::Kind;
use super::selection;
use super::text_view::{NSTextViewImpl, ns};

/// `NSTextMovement` values.
pub(crate) mod movement {
    pub const RETURN: isize = 0x10;
    pub const TAB: isize = 0x11;
    pub const BACKTAB: isize = 0x12;
}

thread_local! {
    /// The kill buffer the delete-to commands fill and `yank:` empties
    /// into the text: one for the application, as in AppKit.
    static KILL: RefCell<String> = const { RefCell::new(String::new()) };
    /// Where `setMark:` put the mark, by view.
    static MARKS: RefCell<Vec<(usize, usize)>> = const { RefCell::new(Vec::new()) };
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "_SidestepTextViewCommands"]
    struct Commands;

    impl Commands {
        // Characters.

        #[unsafe(method(moveForward:))]
        fn move_forward(&self, _s: Option<&AnyObject>) {
            by_char(tv(self), true, false);
        }

        #[unsafe(method(moveRight:))]
        fn move_right(&self, _s: Option<&AnyObject>) {
            by_char(tv(self), true, false);
        }

        #[unsafe(method(moveBackward:))]
        fn move_backward(&self, _s: Option<&AnyObject>) {
            by_char(tv(self), false, false);
        }

        #[unsafe(method(moveLeft:))]
        fn move_left(&self, _s: Option<&AnyObject>) {
            by_char(tv(self), false, false);
        }

        #[unsafe(method(moveForwardAndModifySelection:))]
        fn move_forward_extend(&self, _s: Option<&AnyObject>) {
            by_char(tv(self), true, true);
        }

        #[unsafe(method(moveRightAndModifySelection:))]
        fn move_right_extend(&self, _s: Option<&AnyObject>) {
            by_char(tv(self), true, true);
        }

        #[unsafe(method(moveBackwardAndModifySelection:))]
        fn move_backward_extend(&self, _s: Option<&AnyObject>) {
            by_char(tv(self), false, true);
        }

        #[unsafe(method(moveLeftAndModifySelection:))]
        fn move_left_extend(&self, _s: Option<&AnyObject>) {
            by_char(tv(self), false, true);
        }

        // Words.

        #[unsafe(method(moveWordForward:))]
        fn move_word_forward(&self, _s: Option<&AnyObject>) {
            by_word(tv(self), true, false);
        }

        #[unsafe(method(moveWordRight:))]
        fn move_word_right(&self, _s: Option<&AnyObject>) {
            by_word(tv(self), true, false);
        }

        #[unsafe(method(moveWordBackward:))]
        fn move_word_backward(&self, _s: Option<&AnyObject>) {
            by_word(tv(self), false, false);
        }

        #[unsafe(method(moveWordLeft:))]
        fn move_word_left(&self, _s: Option<&AnyObject>) {
            by_word(tv(self), false, false);
        }

        #[unsafe(method(moveWordForwardAndModifySelection:))]
        fn move_word_forward_extend(&self, _s: Option<&AnyObject>) {
            by_word(tv(self), true, true);
        }

        #[unsafe(method(moveWordRightAndModifySelection:))]
        fn move_word_right_extend(&self, _s: Option<&AnyObject>) {
            by_word(tv(self), true, true);
        }

        #[unsafe(method(moveWordBackwardAndModifySelection:))]
        fn move_word_backward_extend(&self, _s: Option<&AnyObject>) {
            by_word(tv(self), false, true);
        }

        #[unsafe(method(moveWordLeftAndModifySelection:))]
        fn move_word_left_extend(&self, _s: Option<&AnyObject>) {
            by_word(tv(self), false, true);
        }

        // Lines.

        #[unsafe(method(moveUp:))]
        fn move_up(&self, _s: Option<&AnyObject>) {
            vertical(tv(self), -1, false);
        }

        #[unsafe(method(moveDown:))]
        fn move_down(&self, _s: Option<&AnyObject>) {
            vertical(tv(self), 1, false);
        }

        #[unsafe(method(moveUpAndModifySelection:))]
        fn move_up_extend(&self, _s: Option<&AnyObject>) {
            vertical(tv(self), -1, true);
        }

        #[unsafe(method(moveDownAndModifySelection:))]
        fn move_down_extend(&self, _s: Option<&AnyObject>) {
            vertical(tv(self), 1, true);
        }

        #[unsafe(method(moveToBeginningOfLine:))]
        fn move_to_beginning_of_line(&self, _s: Option<&AnyObject>) {
            line_end(tv(self), false, false);
        }

        #[unsafe(method(moveToLeftEndOfLine:))]
        fn move_to_left_end_of_line(&self, _s: Option<&AnyObject>) {
            line_end(tv(self), false, false);
        }

        #[unsafe(method(moveToEndOfLine:))]
        fn move_to_end_of_line(&self, _s: Option<&AnyObject>) {
            line_end(tv(self), true, false);
        }

        #[unsafe(method(moveToRightEndOfLine:))]
        fn move_to_right_end_of_line(&self, _s: Option<&AnyObject>) {
            line_end(tv(self), true, false);
        }

        #[unsafe(method(moveToBeginningOfLineAndModifySelection:))]
        fn move_to_beginning_of_line_extend(&self, _s: Option<&AnyObject>) {
            line_end(tv(self), false, true);
        }

        #[unsafe(method(moveToLeftEndOfLineAndModifySelection:))]
        fn move_to_left_end_of_line_extend(&self, _s: Option<&AnyObject>) {
            line_end(tv(self), false, true);
        }

        #[unsafe(method(moveToEndOfLineAndModifySelection:))]
        fn move_to_end_of_line_extend(&self, _s: Option<&AnyObject>) {
            line_end(tv(self), true, true);
        }

        #[unsafe(method(moveToRightEndOfLineAndModifySelection:))]
        fn move_to_right_end_of_line_extend(&self, _s: Option<&AnyObject>) {
            line_end(tv(self), true, true);
        }

        // Paragraphs.

        #[unsafe(method(moveToBeginningOfParagraph:))]
        fn move_to_beginning_of_paragraph(&self, _s: Option<&AnyObject>) {
            paragraph_end(tv(self), false, false);
        }

        #[unsafe(method(moveToEndOfParagraph:))]
        fn move_to_end_of_paragraph(&self, _s: Option<&AnyObject>) {
            paragraph_end(tv(self), true, false);
        }

        #[unsafe(method(moveToBeginningOfParagraphAndModifySelection:))]
        fn move_to_beginning_of_paragraph_extend(&self, _s: Option<&AnyObject>) {
            paragraph_end(tv(self), false, true);
        }

        #[unsafe(method(moveToEndOfParagraphAndModifySelection:))]
        fn move_to_end_of_paragraph_extend(&self, _s: Option<&AnyObject>) {
            paragraph_end(tv(self), true, true);
        }

        #[unsafe(method(moveParagraphForwardAndModifySelection:))]
        fn move_paragraph_forward_extend(&self, _s: Option<&AnyObject>) {
            by_paragraph(tv(self), true);
        }

        #[unsafe(method(moveParagraphBackwardAndModifySelection:))]
        fn move_paragraph_backward_extend(&self, _s: Option<&AnyObject>) {
            by_paragraph(tv(self), false);
        }

        // The document and pages.

        #[unsafe(method(moveToBeginningOfDocument:))]
        fn move_to_beginning_of_document(&self, _s: Option<&AnyObject>) {
            tv(self).move_to(0, false, false, None);
        }

        #[unsafe(method(moveToEndOfDocument:))]
        fn move_to_end_of_document(&self, _s: Option<&AnyObject>) {
            let v = tv(self);
            v.move_to(v.text_length(), false, false, None);
        }

        #[unsafe(method(moveToBeginningOfDocumentAndModifySelection:))]
        fn move_to_beginning_of_document_extend(&self, _s: Option<&AnyObject>) {
            extend_edge(tv(self), 0, false, false);
        }

        #[unsafe(method(moveToEndOfDocumentAndModifySelection:))]
        fn move_to_end_of_document_extend(&self, _s: Option<&AnyObject>) {
            let v = tv(self);
            extend_edge(v, v.text_length(), true, false);
        }

        #[unsafe(method(pageDown:))]
        fn page_down(&self, _s: Option<&AnyObject>) {
            page(tv(self), 1, false);
        }

        #[unsafe(method(pageUp:))]
        fn page_up(&self, _s: Option<&AnyObject>) {
            page(tv(self), -1, false);
        }

        #[unsafe(method(pageDownAndModifySelection:))]
        fn page_down_extend(&self, _s: Option<&AnyObject>) {
            page(tv(self), 1, true);
        }

        #[unsafe(method(pageUpAndModifySelection:))]
        fn page_up_extend(&self, _s: Option<&AnyObject>) {
            page(tv(self), -1, true);
        }

        #[unsafe(method(scrollPageDown:))]
        fn scroll_page_down(&self, _s: Option<&AnyObject>) {
            scroll_by(tv(self), 1.0, true);
        }

        #[unsafe(method(scrollPageUp:))]
        fn scroll_page_up(&self, _s: Option<&AnyObject>) {
            scroll_by(tv(self), -1.0, true);
        }

        #[unsafe(method(scrollLineDown:))]
        fn scroll_line_down(&self, _s: Option<&AnyObject>) {
            scroll_by(tv(self), 1.0, false);
        }

        #[unsafe(method(scrollLineUp:))]
        fn scroll_line_up(&self, _s: Option<&AnyObject>) {
            scroll_by(tv(self), -1.0, false);
        }

        #[unsafe(method(scrollToBeginningOfDocument:))]
        fn scroll_to_beginning_of_document(&self, _s: Option<&AnyObject>) {
            tv(self).scroll_to(NSRange::new(0, 0));
        }

        #[unsafe(method(scrollToEndOfDocument:))]
        fn scroll_to_end_of_document(&self, _s: Option<&AnyObject>) {
            let v = tv(self);
            v.scroll_to(NSRange::new(v.text_length(), 0));
        }

        #[unsafe(method(centerSelectionInVisibleArea:))]
        fn center_selection_in_visible_area(&self, _s: Option<&AnyObject>) {
            let v = tv(self);
            v.scroll_to(v.selection());
        }

        // Selecting.

        #[unsafe(method(selectAll:))]
        fn select_all(&self, _s: Option<&AnyObject>) {
            let v = tv(self);
            if v.is_selectable_now() {
                v.as_text_view().setSelectedRange(NSRange::new(0, v.text_length()));
            }
        }

        /// The word around the selection; for an insertion point just
        /// after a word (not in one), that word, as AppKit selects it.
        #[unsafe(method(selectWord:))]
        fn select_word(&self, _s: Option<&AnyObject>) {
            let v = tv(self);
            let sel = v.selection();
            let before = (sel.length == 0 && sel.location > 0)
                .then(|| storage(v))
                .flatten()
                .filter(|ts| selection::word_holding(ts, sel.location).is_none())
                .and_then(|ts| selection::word_holding(&ts, sel.location - 1));
            let r = match before {
                Some(word) => ns(word),
                None => v.range_for_granularity(sel, objc2_app_kit::NSSelectionGranularity::SelectByWord),
            };
            v.as_text_view().setSelectedRange(r);
        }

        #[unsafe(method(selectParagraph:))]
        fn select_paragraph(&self, _s: Option<&AnyObject>) {
            let v = tv(self);
            let r = v.range_for_granularity(v.selection(), objc2_app_kit::NSSelectionGranularity::SelectByParagraph);
            v.as_text_view().setSelectedRange(r);
        }

        #[unsafe(method(selectLine:))]
        fn select_line(&self, _s: Option<&AnyObject>) {
            let v = tv(self);
            let sel = v.selection();
            let Some(lm) = v.geo() else { return };
            let first = lm.line_at(sel.location, false).map(|l| l.range());
            let last = lm.line_at((sel.location + sel.length).saturating_sub(usize::from(sel.length > 0)), false).map(|l| l.range());
            if let (Some(a), Some(b)) = (first, last) {
                v.as_text_view().setSelectedRange(ns(a.start..b.end));
            }
        }

        // Inserting.

        #[unsafe(method(insertNewline:))]
        fn insert_newline(&self, _s: Option<&AnyObject>) {
            let v = tv(self);
            if v.is_field_editor_now() {
                end_field_editing(v, movement::RETURN);
            } else {
                insert(v, "\n");
            }
        }

        #[unsafe(method(insertTab:))]
        fn insert_tab(&self, _s: Option<&AnyObject>) {
            let v = tv(self);
            if v.is_field_editor_now() {
                end_field_editing(v, movement::TAB);
            } else {
                insert(v, "\t");
            }
        }

        #[unsafe(method(insertBacktab:))]
        fn insert_backtab(&self, _s: Option<&AnyObject>) {
            let v = tv(self);
            if v.is_field_editor_now() {
                end_field_editing(v, movement::BACKTAB);
            }
        }

        #[unsafe(method(insertNewlineIgnoringFieldEditor:))]
        fn insert_newline_ignoring_field_editor(&self, _s: Option<&AnyObject>) {
            insert(tv(self), "\n");
        }

        #[unsafe(method(insertTabIgnoringFieldEditor:))]
        fn insert_tab_ignoring_field_editor(&self, _s: Option<&AnyObject>) {
            insert(tv(self), "\t");
        }

        /// A paragraph separator (U+2029), as AppKit's text view puts in.
        #[unsafe(method(insertParagraphSeparator:))]
        fn insert_paragraph_separator(&self, _s: Option<&AnyObject>) {
            let v = tv(self);
            if v.is_field_editor_now() {
                end_field_editing(v, movement::RETURN);
            } else {
                insert(v, "\u{2029}");
            }
        }

        #[unsafe(method(insertLineBreak:))]
        fn insert_line_break(&self, _s: Option<&AnyObject>) {
            insert(tv(self), "\u{2028}");
        }

        #[unsafe(method(insertContainerBreak:))]
        fn insert_container_break(&self, _s: Option<&AnyObject>) {
            insert(tv(self), "\u{c}");
        }

        #[unsafe(method(insertSingleQuoteIgnoringSubstitution:))]
        fn insert_single_quote(&self, _s: Option<&AnyObject>) {
            insert(tv(self), "'");
        }

        #[unsafe(method(insertDoubleQuoteIgnoringSubstitution:))]
        fn insert_double_quote(&self, _s: Option<&AnyObject>) {
            insert(tv(self), "\"");
        }

        #[unsafe(method(indent:))]
        fn indent(&self, _s: Option<&AnyObject>) {
            insert(tv(self), "\t");
        }

        /// Escape: nothing, in a text view or a field editor (a field's
        /// delegate sees it first, as a command).
        #[unsafe(method(cancelOperation:))]
        fn cancel_operation(&self, _s: Option<&AnyObject>) {}

        #[unsafe(method(complete:))]
        fn complete(&self, _s: Option<&AnyObject>) {}

        // Deleting.

        #[unsafe(method(deleteBackward:))]
        fn delete_backward(&self, _s: Option<&AnyObject>) {
            let v = tv(self);
            delete_to(v, selection::prev_deletable, Kind::Delete, false);
        }

        #[unsafe(method(deleteBackwardByDecomposingPreviousCharacter:))]
        fn delete_backward_decomposing(&self, _s: Option<&AnyObject>) {
            let v = tv(self);
            delete_to(v, unit_before, Kind::Delete, false);
        }

        #[unsafe(method(deleteForward:))]
        fn delete_forward(&self, _s: Option<&AnyObject>) {
            let v = tv(self);
            delete_to(v, selection::next_deletable, Kind::Other, false);
        }

        #[unsafe(method(deleteWordBackward:))]
        fn delete_word_backward(&self, _s: Option<&AnyObject>) {
            let v = tv(self);
            delete_to(v, selection::word_start_before, Kind::Other, false);
        }

        #[unsafe(method(deleteWordForward:))]
        fn delete_word_forward(&self, _s: Option<&AnyObject>) {
            let v = tv(self);
            delete_to(v, selection::word_end_after, Kind::Other, false);
        }

        #[unsafe(method(deleteToBeginningOfLine:))]
        fn delete_to_beginning_of_line(&self, _s: Option<&AnyObject>) {
            let v = tv(self);
            let target = line_bounds(v, v.selection().location).0;
            delete_to(v, |_, _| target, Kind::Other, true);
        }

        #[unsafe(method(deleteToEndOfLine:))]
        fn delete_to_end_of_line(&self, _s: Option<&AnyObject>) {
            let v = tv(self);
            let at = v.selection().location;
            let (_, end, full_end) = line_bounds(v, at);
            // At the line's end: the line break goes, as Control-K does.
            let target = if at == end { full_end } else { end };
            delete_to(v, |_, _| target, Kind::Other, true);
        }

        #[unsafe(method(deleteToBeginningOfParagraph:))]
        fn delete_to_beginning_of_paragraph(&self, _s: Option<&AnyObject>) {
            let v = tv(self);
            delete_to(v, |ts, i| selection::paragraph(ts, i).start, Kind::Other, true);
        }

        #[unsafe(method(deleteToEndOfParagraph:))]
        fn delete_to_end_of_paragraph(&self, _s: Option<&AnyObject>) {
            let v = tv(self);
            delete_to(
                v,
                |ts, i| {
                    let p = selection::paragraph(ts, i);
                    let end = selection::content_end(ts, p.clone());
                    if i == end { p.end } else { end }
                },
                Kind::Other,
                true,
            );
        }

        #[unsafe(method(yank:))]
        fn yank(&self, _s: Option<&AnyObject>) {
            let text = KILL.with(|k| k.borrow().clone());
            if !text.is_empty() {
                insert(tv(self), &text);
            }
        }

        // Changing the text.

        #[unsafe(method(transpose:))]
        fn transpose(&self, _s: Option<&AnyObject>) {
            transpose_chars(tv(self));
        }

        #[unsafe(method(uppercaseWord:))]
        fn uppercase_word(&self, _s: Option<&AnyObject>) {
            change_case(tv(self), |w| w.to_uppercase());
        }

        #[unsafe(method(lowercaseWord:))]
        fn lowercase_word(&self, _s: Option<&AnyObject>) {
            change_case(tv(self), |w| w.to_lowercase());
        }

        #[unsafe(method(capitalizeWord:))]
        fn capitalize_word(&self, _s: Option<&AnyObject>) {
            change_case(tv(self), capitalize);
        }

        // The mark.

        #[unsafe(method(setMark:))]
        fn set_mark(&self, _s: Option<&AnyObject>) {
            let v = tv(self);
            set_mark_at(v, v.selection().location);
        }

        #[unsafe(method(selectToMark:))]
        fn select_to_mark(&self, _s: Option<&AnyObject>) {
            let v = tv(self);
            if let Some(m) = mark_of(v) {
                let at = v.selection().location;
                v.as_text_view().setSelectedRange(ns(at.min(m)..at.max(m)));
            }
        }

        #[unsafe(method(deleteToMark:))]
        fn delete_to_mark(&self, _s: Option<&AnyObject>) {
            let v = tv(self);
            if let Some(m) = mark_of(v) {
                delete_to(v, |_, _| m, Kind::Other, true);
            }
        }

        #[unsafe(method(swapWithMark:))]
        fn swap_with_mark(&self, _s: Option<&AnyObject>) {
            let v = tv(self);
            if let Some(m) = mark_of(v) {
                let at = v.selection().location;
                set_mark_at(v, at);
                v.move_to(m, false, false, None);
            }
        }

        // Writing direction (kept as the paragraph style's).

        #[unsafe(method(makeBaseWritingDirectionNatural:))]
        fn make_base_natural(&self, _s: Option<&AnyObject>) {
            let v = tv(self);
            v.as_text_view().setBaseWritingDirection_range(objc2_app_kit::NSWritingDirection::Natural, v.selection());
        }

        #[unsafe(method(makeBaseWritingDirectionLeftToRight:))]
        fn make_base_ltr(&self, _s: Option<&AnyObject>) {
            let v = tv(self);
            v.as_text_view().setBaseWritingDirection_range(objc2_app_kit::NSWritingDirection::LeftToRight, v.selection());
        }

        #[unsafe(method(makeBaseWritingDirectionRightToLeft:))]
        fn make_base_rtl(&self, _s: Option<&AnyObject>) {
            let v = tv(self);
            v.as_text_view().setBaseWritingDirection_range(objc2_app_kit::NSWritingDirection::RightToLeft, v.selection());
        }

        // The clipboard.

        #[unsafe(method(copy:))]
        fn copy(&self, _s: Option<&AnyObject>) {
            let v = tv(self);
            if v.selection().length > 0 && !v.is_secure() {
                write_to_pasteboard(v, &NSPasteboard::generalPasteboard());
            }
        }

        #[unsafe(method(cut:))]
        fn cut(&self, _s: Option<&AnyObject>) {
            let v = tv(self);
            let sel = v.selection();
            if sel.length > 0 && v.is_editable_now() && !v.is_secure() {
                write_to_pasteboard(v, &NSPasteboard::generalPasteboard());
                v.edit_replace(sel, "", Kind::Cut);
            }
        }

        #[unsafe(method(paste:))]
        fn paste(&self, _s: Option<&AnyObject>) {
            read_selection(tv(self), &NSPasteboard::generalPasteboard(), Kind::Paste);
        }

        #[unsafe(method(pasteAsPlainText:))]
        fn paste_as_plain_text(&self, _s: Option<&AnyObject>) {
            read_as(tv(self), &NSPasteboard::generalPasteboard(), Kind::Paste, &[Flavor::Text]);
        }

        #[unsafe(method(pasteAsRichText:))]
        fn paste_as_rich_text(&self, _s: Option<&AnyObject>) {
            read_as(tv(self), &NSPasteboard::generalPasteboard(), Kind::Paste, &RICH_READABLE);
        }

        #[unsafe(method(delete:))]
        fn delete(&self, _s: Option<&AnyObject>) {
            let v = tv(self);
            let sel = v.selection();
            if sel.length > 0 {
                v.edit_replace(sel, "", Kind::Other);
            }
        }

        /// The pasteboard is cleared, then each type written by
        /// `writeSelectionToPasteboard:type:` (by message when a subclass
        /// overrides it), as AppKit's does. Whether any was written.
        #[unsafe(method(writeSelectionToPasteboard:types:))]
        fn write_selection_to_pasteboard_types(&self, pb: &NSPasteboard, types: &NSArray<NSString>) -> bool {
            write_types(tv(self), pb, types)
        }

        #[unsafe(method(writeSelectionToPasteboard:type:))]
        fn write_selection_to_pasteboard_type(&self, pb: &NSPasteboard, t: &NSString) -> bool {
            write_type(tv(self), pb, t)
        }

        #[unsafe(method(readSelectionFromPasteboard:))]
        fn read_selection_from_pasteboard(&self, pb: &NSPasteboard) -> bool {
            read_selection(tv(self), pb, Kind::Other)
        }

        #[unsafe(method(readSelectionFromPasteboard:type:))]
        fn read_selection_from_pasteboard_type(&self, pb: &NSPasteboard, t: &NSString) -> bool {
            Flavor::of(t).is_some_and(|f| read_as(tv(self), pb, Kind::Other, &[f]))
        }

        #[unsafe(method_id(writablePasteboardTypes))]
        fn writable_pasteboard_types(&self) -> Retained<NSArray<NSString>> {
            names(&writable(tv(self)))
        }

        #[unsafe(method_id(readablePasteboardTypes))]
        fn readable_pasteboard_types(&self) -> Retained<NSArray<NSString>> {
            names(readable(tv(self)))
        }

        #[unsafe(method(validateUserInterfaceItem:))]
        fn validate_user_interface_item(&self, item: &AnyObject) -> bool {
            // SAFETY: a validated item answers action.
            let action: Option<Sel> = unsafe { msg_send![item, action] };
            validate(tv(self), action)
        }

        #[unsafe(method(validateMenuItem:))]
        fn validate_menu_item(&self, item: &AnyObject) -> bool {
            // SAFETY: a menu item answers action.
            let action: Option<Sel> = unsafe { msg_send![item, action] };
            validate(tv(self), action)
        }
    }
);

sidestep_runtime::category!("NSTextView"(SidestepCommands), |category| {
    // SAFETY: the helper's methods treat their receiver as a text view.
    unsafe { category.add_methods_of(Commands::class()) };
});

/// The receiver of a command, as the text view it is.
fn tv(this: &Commands) -> &NSTextViewImpl {
    // SAFETY: the methods are installed on NSTextView, so the receiver is
    // one (of Sidestep's class, or a subclass of it).
    unsafe { &*(this as *const Commands).cast::<NSTextViewImpl>() }
}

fn storage(v: &NSTextViewImpl) -> Option<Retained<NSTextStorage>> {
    v.storage()
}

fn by_char(v: &NSTextViewImpl, forward: bool, extend: bool) {
    let Some(ts) = storage(v) else { return };
    let sel = v.selection();
    if !extend && sel.length > 0 {
        // A selection collapses to its end the move goes toward.
        let at = if forward { sel.location + sel.length } else { sel.location };
        v.move_to(at, false, false, None);
        return;
    }
    let from = if extend { v.moving_end() } else { sel.location };
    let to = if forward { selection::next_char(&ts, from) } else { selection::prev_char(&ts, from) };
    v.move_to(to, extend, false, None);
}

fn by_word(v: &NSTextViewImpl, forward: bool, extend: bool) {
    let Some(ts) = storage(v) else { return };
    let sel = v.selection();
    let from = if extend {
        v.moving_end()
    } else if forward {
        sel.location + sel.length
    } else {
        sel.location
    };
    let to = if forward { selection::word_end_after(&ts, from) } else { selection::word_start_before(&ts, from) };
    v.move_to(to, extend, false, None);
}

/// The visual line holding `index`: where it starts, where its characters
/// end, and where it ends with its separator.
fn line_bounds(v: &NSTextViewImpl, index: usize) -> (usize, usize, usize) {
    let Some(lm) = v.geo() else { return (index, index, index) };
    match lm.line_at(index, v.affinity() == objc2_app_kit::NSSelectionAffinity::Upstream) {
        Some(l) => {
            let r = l.range();
            let content = l.content_end();
            (r.start, content, r.end)
        }
        None => (index, index, index),
    }
}

fn line_end(v: &NSTextViewImpl, to_end: bool, extend: bool) {
    let sel = v.selection();
    let from = if to_end { sel.location + sel.length } else { sel.location };
    let (start, content, full) = line_bounds(v, from);
    // The end of a wrapped line (no separator) is its last index, with the
    // insertion point kept on that line.
    let (to, upstream) = if to_end { (content, content == full && full < v.text_length()) } else { (start, false) };
    if extend {
        extend_edge(v, to, to_end, upstream);
    } else {
        v.move_to(to, false, upstream, None);
    }
}

fn paragraph_end(v: &NSTextViewImpl, to_end: bool, extend: bool) {
    let Some(ts) = storage(v) else { return };
    let sel = v.selection();
    let from = if to_end { sel.location + sel.length } else { sel.location };
    let p = selection::paragraph(&ts, from);
    let to = if to_end { selection::content_end(&ts, p) } else { p.start };
    if extend {
        extend_edge(v, to, to_end, false);
    } else {
        v.move_to(to, false, false, None);
    }
}

/// Extend the selection to a line's, paragraph's or the document's end
/// (or start) as AppKit does: the selection's far edge that way goes
/// there, and the other edge stays, whichever end was moving.
fn extend_edge(v: &NSTextViewImpl, to: usize, to_end: bool, upstream: bool) {
    let sel = v.selection();
    let (start, end) = (sel.location, sel.location + sel.length);
    let (anchor, active) = if to_end { (start.min(to), to.max(start)) } else { (end.max(to), to.min(end)) };
    v.select_extended(anchor, active, upstream);
}

/// Extend by a paragraph: to the next paragraph's start, or back to this
/// one's (the one before's, at a start).
fn by_paragraph(v: &NSTextViewImpl, forward: bool) {
    let Some(ts) = storage(v) else { return };
    let from = v.moving_end();
    let p = selection::paragraph(&ts, from);
    let to = if forward {
        p.end
    } else if from > p.start {
        p.start
    } else {
        selection::paragraph(&ts, p.start.saturating_sub(1)).start
    };
    v.move_to(to, true, false, None);
}

/// Up or down a line, aiming for the goal x.
fn vertical(v: &NSTextViewImpl, dir: i32, extend: bool) {
    let Some(lm) = v.geo() else { return };
    let sel = v.selection();
    let from = if extend {
        v.moving_end()
    } else if dir > 0 {
        sel.location + sel.length
    } else {
        sel.location
    };
    let upstream = v.affinity() == objc2_app_kit::NSSelectionAffinity::Upstream;
    let caret = lm.caret_rect(from, upstream);
    let goal = v.goal_x().unwrap_or(caret.origin.x);
    let Some(line) = lm.line_at(from, upstream) else { return };
    let (top, bottom) = line.fragment_span();
    let len = v.text_length();
    let target_y = if dir > 0 { bottom + 0.5 } else { top - 0.5 };
    let (to, up) = if target_y < 0.0 {
        (0, false)
    } else if dir > 0 && target_y >= lm.height() {
        (len, false)
    } else {
        lm.insertion_index(NSPoint::new(goal, target_y))
    };
    v.move_to(to, extend, up, Some(goal));
}

/// A page: the visible height, less a line.
fn page(v: &NSTextViewImpl, dir: i32, extend: bool) {
    let Some(lm) = v.geo() else { return };
    let visible = v.as_view().visibleRect();
    let step = (visible.size.height - 16.0).max(16.0);
    let sel = v.selection();
    let from = if extend { v.moving_end() } else { sel.location };
    let caret = lm.caret_rect(from, false);
    let goal = v.goal_x().unwrap_or(caret.origin.x);
    let y = caret.origin.y + f64::from(dir) * step;
    let (to, up) = if y < 0.0 {
        (0, false)
    } else if y >= lm.height() {
        (v.text_length(), false)
    } else {
        lm.insertion_index(NSPoint::new(goal, y))
    };
    v.move_to(to, extend, up, Some(goal));
}

/// Scroll without moving the selection: a page or a line.
fn scroll_by(v: &NSTextViewImpl, dir: f64, page: bool) {
    let visible = v.as_view().visibleRect();
    let step = if page { (visible.size.height - 16.0).max(16.0) } else { 16.0 };
    let y = if dir > 0.0 { visible.origin.y + visible.size.height + step - 1.0 } else { visible.origin.y - step };
    let target =
        objc2_foundation::NSRect::new(NSPoint::new(visible.origin.x, y), objc2_foundation::NSSize::new(1.0, 1.0));
    super::text_view::scroll_rect_to_visible(v.as_view(), target);
}

fn insert(v: &NSTextViewImpl, text: &str) {
    if !v.is_editable_now() {
        return;
    }
    let sel = v.selection();
    // Text an input method is composing goes, replaced.
    let range = v.marked_range().unwrap_or(sel);
    v.ivars().marked.set(None);
    super::input_client::forget_composition(v);
    v.edit_replace(range, text, Kind::Typing);
}

/// Delete the selection, or from the insertion point to `target(index)`.
/// `kill` puts what goes into the kill buffer.
fn delete_to(v: &NSTextViewImpl, target: impl FnOnce(&NSTextStorage, usize) -> usize, kind: Kind, kill: bool) {
    if !v.is_editable_now() {
        return;
    }
    let Some(ts) = storage(v) else { return };
    let sel = v.selection();
    let range = if sel.length > 0 {
        sel
    } else {
        let to = target(&ts, sel.location);
        ns(sel.location.min(to)..sel.location.max(to))
    };
    if range.length == 0 {
        return;
    }
    if kill {
        let text = selection::text(&ts, range.location..range.location + range.length);
        KILL.with(|k| *k.borrow_mut() = text);
    }
    v.edit_replace(range, "", kind);
}

/// The index of the UTF-16 unit before `i`, a whole character's start.
fn unit_before(ts: &NSTextStorage, i: usize) -> usize {
    let prev = selection::prev_char(ts, i);
    // Decomposing: the last code point of the character goes alone.
    let text = selection::text(ts, prev..i);
    match text.chars().last() {
        Some(c) if text.chars().count() > 1 => i - c.len_utf16(),
        _ => prev,
    }
}

/// Swap the characters around the insertion point (the two before it at a
/// line's end), and move after them.
fn transpose_chars(v: &NSTextViewImpl) {
    let Some(ts) = storage(v) else { return };
    let sel = v.selection();
    if sel.length > 0 || !v.is_editable_now() {
        return;
    }
    let len = selection::len(&ts);
    let at = sel.location;
    let para = selection::paragraph(&ts, at);
    let end = selection::content_end(&ts, para.clone());
    let (a, mid, b) = if at >= end && at > para.start {
        let mid = selection::prev_char(&ts, at);
        (selection::prev_char(&ts, mid), mid, at)
    } else if at > para.start && at < len {
        (selection::prev_char(&ts, at), at, selection::next_char(&ts, at))
    } else {
        return;
    };
    if a >= mid || mid >= b {
        return;
    }
    let first = selection::text(&ts, a..mid);
    let second = selection::text(&ts, mid..b);
    v.edit_replace(ns(a..b), &(second + &first), Kind::Other);
}

/// Change the case of the selected words, or of the word the insertion
/// point follows (the one it is in, at the text's start), and select what
/// changed, as AppKit does. After a space that is the space, which has no
/// case.
fn change_case(v: &NSTextViewImpl, f: impl Fn(&str) -> String) {
    let Some(ts) = storage(v) else { return };
    if !v.is_editable_now() {
        return;
    }
    let sel = v.selection();
    let range = if sel.length == 0 && sel.location > 0 {
        ns(selection::word_at(&ts, sel.location - 1))
    } else {
        v.range_for_granularity(sel, objc2_app_kit::NSSelectionGranularity::SelectByWord)
    };
    v.as_text_view().setSelectedRange(range);
    let text = selection::text(&ts, range.location..range.location + range.length);
    let new = f(&text);
    if new == text {
        return;
    }
    if v.edit_replace(range, &new, Kind::Other) {
        let added = new.encode_utf16().count();
        v.as_text_view().setSelectedRange(NSRange::new(range.location, added));
    }
}

/// Upper case each word's first letter, lower case the rest.
fn capitalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut start = true;
    for c in text.chars() {
        if c.is_alphanumeric() {
            if start {
                out.extend(c.to_uppercase());
            } else {
                out.extend(c.to_lowercase());
            }
            start = false;
        } else {
            out.push(c);
            start = true;
        }
    }
    out
}

fn end_field_editing(v: &NSTextViewImpl, movement: isize) {
    super::field_editor::end_with_movement(v, movement);
}

fn view_key(v: &NSTextViewImpl) -> usize {
    v as *const NSTextViewImpl as usize
}

fn set_mark_at(v: &NSTextViewImpl, at: usize) {
    let key = view_key(v);
    MARKS.with(|m| {
        let mut m = m.borrow_mut();
        m.retain(|&(k, _)| k != key);
        m.push((key, at));
    });
}

fn mark_of(v: &NSTextViewImpl) -> Option<usize> {
    let key = view_key(v);
    MARKS.with(|m| m.borrow().iter().find(|&&(k, _)| k == key).map(|&(_, at)| at)).map(|at| at.min(v.text_length()))
}

/// Write the selection to `pb` as `copy:` and `cut:` do: its
/// `writablePasteboardTypes` through `writeSelectionToPasteboard:types:`,
/// both by message when a subclass overrides them.
fn write_to_pasteboard(v: &NSTextViewImpl, pb: &NSPasteboard) -> bool {
    let view: &AnyObject = v.as_text_view();
    if super::text_view::WRITE_TYPES.overridden(view, sel!(writeSelectionToPasteboard:types:)) {
        // SAFETY: writablePasteboardTypes returns an array of types; the
        // writer takes a pasteboard and the types and returns BOOL.
        unsafe {
            let types: Retained<NSArray<NSString>> = msg_send![view, writablePasteboardTypes];
            msg_send![view, writeSelectionToPasteboard: pb, types: &*types]
        }
    } else {
        write_selection(v, pb)
    }
}

/// `writeSelectionToPasteboard:types:`: the pasteboard is cleared, then
/// each type written by `writeSelectionToPasteboard:type:` (by message
/// when a subclass overrides it), as AppKit's does. Whether any was.
fn write_types(v: &NSTextViewImpl, pb: &NSPasteboard, types: &NSArray<NSString>) -> bool {
    if v.selection().length == 0 || v.is_secure() {
        return false;
    }
    pb.clearContents();
    let view: &AnyObject = v.as_text_view();
    let through = super::text_view::WRITE_TYPE.overridden(view, sel!(writeSelectionToPasteboard:type:));
    let mut wrote = false;
    for t in types.iter() {
        wrote |= if through {
            // SAFETY: the method takes a pasteboard and a type and returns
            // BOOL.
            unsafe { msg_send![view, writeSelectionToPasteboard: pb, type: &*t] }
        } else {
            write_type(v, pb, &t)
        };
    }
    wrote
}

/// What a text view puts on or takes from a pasteboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flavor {
    Rtf,
    Rtfd,
    Html,
    Text,
}

/// What a rich text view reads, best first; a plain one reads the text
/// first, and rich text as text.
const RICH_READABLE: [Flavor; 4] = [Flavor::Rtf, Flavor::Rtfd, Flavor::Html, Flavor::Text];
const PLAIN_READABLE: [Flavor; 4] = [Flavor::Text, Flavor::Rtf, Flavor::Rtfd, Flavor::Html];

impl Flavor {
    /// The flavor a pasteboard type is, by its name or its old name.
    fn of(t: &NSString) -> Option<Flavor> {
        use crate::pasteboard_types::{HTML, RTF, RTFD, STRING, from_ns};
        Some(match from_ns(t).as_str() {
            RTF => Flavor::Rtf,
            RTFD => Flavor::Rtfd,
            HTML => Flavor::Html,
            STRING => Flavor::Text,
            _ => return None,
        })
    }

    /// The type, by the old name AppKit's text view lists it by (the
    /// pasteboard takes it as the new one).
    #[allow(deprecated)] // The old names, which AppKit's text view still lists.
    fn name(self) -> &'static NSString {
        use objc2_app_kit::{NSHTMLPboardType, NSRTFDPboardType, NSRTFPboardType, NSStringPboardType};
        // SAFETY: constant strings AppKit exports.
        unsafe {
            match self {
                Flavor::Rtf => NSRTFPboardType,
                Flavor::Rtfd => NSRTFDPboardType,
                Flavor::Html => NSHTMLPboardType,
                Flavor::Text => NSStringPboardType,
            }
        }
    }

    fn uti(self) -> &'static str {
        use crate::pasteboard_types::{HTML, RTF, RTFD, STRING};
        match self {
            Flavor::Rtf => RTF,
            Flavor::Rtfd => RTFD,
            Flavor::Html => HTML,
            Flavor::Text => STRING,
        }
    }
}

fn names(flavors: &[Flavor]) -> Retained<NSArray<NSString>> {
    let names: Vec<&NSString> = flavors.iter().map(|f| f.name()).collect();
    NSArray::from_slice(&names)
}

/// What the selection is written as: nothing when there is none (as
/// AppKit's list is empty then), rich text and text from a rich view (RTF
/// and text, as AppKit writes, and HTML, for the Linux programs that read
/// no RTF), text from a plain one.
fn writable(v: &NSTextViewImpl) -> Vec<Flavor> {
    if v.selection().length == 0 || v.is_secure() {
        return Vec::new();
    }
    if v.as_text_view().isRichText() { vec![Flavor::Rtf, Flavor::Html, Flavor::Text] } else { vec![Flavor::Text] }
}

fn can_write(v: &NSTextViewImpl, f: Flavor) -> bool {
    f == Flavor::Text || v.as_text_view().isRichText()
}

fn readable(v: &NSTextViewImpl) -> &'static [Flavor] {
    if v.as_text_view().isRichText() { &RICH_READABLE } else { &PLAIN_READABLE }
}

/// Write the selected text to `pb` as the view writes it.
fn write_selection(v: &NSTextViewImpl, pb: &NSPasteboard) -> bool {
    let flavors = writable(v);
    !flavors.is_empty() && write_flavors(v, pb, &flavors, true)
}

/// Write the selection to `pb` as type `t`, beside what it holds, if the
/// view writes that type.
fn write_type(v: &NSTextViewImpl, pb: &NSPasteboard, t: &NSString) -> bool {
    Flavor::of(t).is_some_and(|f| can_write(v, f) && write_flavors(v, pb, &[f], false))
}

/// Write the selected text to `pb` as `flavors`, clearing it first if
/// `clear`.
fn write_flavors(v: &NSTextViewImpl, pb: &NSPasteboard, flavors: &[Flavor], clear: bool) -> bool {
    let Some(ts) = storage(v) else { return false };
    let sel = v.selection();
    if sel.length == 0 || v.is_secure() {
        return false;
    }
    if clear {
        pb.clearContents();
    }
    let mut wrote = false;
    for &f in flavors {
        wrote |= match f {
            Flavor::Text => {
                let text = NSString::from_str(&selection::text(&ts, sel.location..sel.location + sel.length));
                // SAFETY: the constant is a string AppKit exports.
                pb.setString_forType(&text, unsafe { NSPasteboardTypeString })
            }
            _ => crate::rich::to_pasteboard(&ts, sel, f.uti())
                .and_then(|value| value.downcast::<objc2_foundation::NSData>().ok())
                .is_some_and(|data| pb.setData_forType(Some(&data), &NSString::from_str(f.uti()))),
        };
    }
    wrote
}

/// Replace the selection with `pb`'s text, an edit of `kind` (a paste:
/// command's is named for undo; `readSelectionFromPasteboard:`'s isn't, as
/// on macOS), in the first of the view's readable types the pasteboard has.
fn read_selection(v: &NSTextViewImpl, pb: &NSPasteboard, kind: Kind) -> bool {
    read_as(v, pb, kind, readable(v))
}

/// Replace the selection with `pb`'s contents in the first of `flavors` it
/// has: rich text as it is in a rich text view, as text in a plain one.
fn read_as(v: &NSTextViewImpl, pb: &NSPasteboard, kind: Kind, flavors: &[Flavor]) -> bool {
    if !v.is_editable_now() {
        return false;
    }
    let Some(found) = pb.availableTypeFromArray(&names(flavors)) else { return false };
    let Some(flavor) = Flavor::of(&found) else { return false };
    let rich = match flavor {
        Flavor::Text => None,
        _ => {
            let Some(data) = pb.dataForType(&NSString::from_str(flavor.uti())) else { return false };
            let Some(rich) = crate::rich::from_pasteboard(&data, &NSString::from_str(flavor.uti())) else {
                return false;
            };
            Some(rich)
        }
    };
    let text = match &rich {
        Some(r) => r.string(),
        // SAFETY: the constant is a string AppKit exports.
        None => match pb.stringForType(unsafe { NSPasteboardTypeString }) {
            Some(text) => text,
            None => return false,
        },
    };
    let range = v.marked_range().unwrap_or(v.selection());
    v.ivars().marked.set(None);
    super::input_client::forget_composition(v);
    if let Some(rich) = rich.filter(|_| v.as_text_view().isRichText() && !v.is_field_editor_now()) {
        return v.edit_replace_attributed(range, &rich, kind);
    }
    let text = if v.is_field_editor_now() {
        // A field holds one line: line breaks become spaces.
        NSString::from_str(&text.to_string().replace(['\n', '\r', '\u{2029}', '\u{2028}'], " "))
    } else {
        text
    };
    v.edit_replace_ns(range, &text, kind)
}

/// Whether a menu item or control for `action` should be enabled.
fn validate(v: &NSTextViewImpl, action: Option<Sel>) -> bool {
    let Some(action) = action else { return true };
    let has_selection = v.selection().length > 0;
    if action == sel!(copy:) {
        has_selection && !v.is_secure()
    } else if action == sel!(cut:) {
        has_selection && v.is_editable_now() && !v.is_secure()
    } else if action == sel!(delete:) {
        has_selection && v.is_editable_now()
    } else if action == sel!(paste:) || action == sel!(pasteAsPlainText:) || action == sel!(pasteAsRichText:) {
        // Only the types are looked at: nothing is read (another program's
        // text may still be on its way).
        let types = names(readable(v));
        v.is_editable_now() && NSPasteboard::generalPasteboard().availableTypeFromArray(&types).is_some()
    } else if action == sel!(selectAll:) {
        v.is_selectable_now()
    } else if action == sel!(undo:) {
        v.text_undo_manager().is_some_and(|u| u.canUndo())
    } else if action == sel!(redo:) {
        v.text_undo_manager().is_some_and(|u| u.canRedo())
    } else {
        true
    }
}
