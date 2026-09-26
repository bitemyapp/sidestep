# Text editing

TextKit 1 and the text view, as Sidestep implements them on Linux:
`NSTextStorage`, `NSLayoutManager`, `NSTextContainer`, `NSText` and
`NSTextView` with the field editor controls edit in, `NSUndoManager`, and
text blocks and tables. The code is in
`crates/sidestep-appkit/src/textkit/` (and `NSUndoManager` in
`crates/sidestep-foundation/src/undo.rs`); it stands on the line layout of
`text/lines.rs` described in [architecture.md](architecture.md#text).

What the classes do was measured on macOS, not read from Apple's headers
or documentation: each behavior below that a program can see is checked
by a conformance test that runs against AppKit first and Sidestep second
(`conformance/tests/text_storage.rs`, `text_layout.rs`, `text_blocks.rs`,
`undo_manager.rs`, `text_view.rs`).

## Text storage

**The text.** `storage::Storage` keeps a text as a tree of paragraphs: each
paragraph its UTF-8 text (separator included), its length in UTF-16 units
and in bytes, whether it is all ASCII, and its attribute runs over UTF-16
units. Paragraphs sit in chunks of at most 128 (a chunk's sums let an index
find its chunk in a binary search), and a paragraph longer than 4096 bytes
keeps UTF-16 checkpoints, so a UTF-16 index finds its byte in a few hundred
bytes at most. An edit inside one paragraph that makes no new paragraphs
changes it in place; others splice paragraphs. Every edit bumps a
generation. Property tests check the tree against a plain string and runs
over random edits, CR LF pairs and surrogates included.

**Attributes.** Each storage interns its attribute dictionaries
(`attrs::AttrTable`): runs name an id, the same dictionary (by address, then
by contents) gets the same id, and ids nobody uses are compacted away now
and then. Setting an attribute over a range goes run by run through the
table, not by building dictionaries per character.

**`NSTextStorage`** is Sidestep's own subclass of `NSMutableAttributedString`
over that storage; `-string` is a live `NSString` subclass reading the
storage, which takes a copy of the text if the storage goes before it.
What programs ask of that string as the user types, about the text near
an edit, is answered from the tree, costing what that text costs:
`paragraphRangeForRange:`, `lineRangeForRange:` and their
`get…Start:end:contentsEnd:forRange:` forms (Foundation's paragraphs end
where the tree's do, and a line never crosses one),
`rangeOfComposedCharacterSequenceAtIndex:`, `hasPrefix:` and `hasSuffix:`;
the rest of `NSString` reads the whole text, as for any subclass. A test
checks each of these against a plain string with the same text. A storage
made with text has its attributes fixed from the start, and the scripting
accessors `font` and `foregroundColor` read the first character's and set
one over all the text.
Edits between `beginEditing` and `endEditing` gather into one change (its
mask, the union of the edited ranges, the total change in length).
`processEditing` then runs as AppKit's does: the will-process notification,
the delegate's `textStorage:willProcessEditing:…`, fixing attributes (a font
where there is none, Helvetica 12 as on macOS, and each paragraph's style
made its first character's), the did-process notification and delegate
call, and each layout manager told of the edited range; `editedRange` is
`NSNotFound` afterwards. An edit made while the layout managers hear of a
change (a text view's delegate, told the selection moved, may make one) is
processed as a change of its own, at once. A subclass that keeps its own text (overriding the
primitives) works too: Sidestep's fast paths apply only to its own class,
and a layout manager keeps a copy of such a storage's text, read through
the primitives, edit by edit.

## Undo

`NSUndoManager` groups by event: the first registration (or
`beginUndoGrouping`, or `setActionName:`) in a turn of the run loop opens a
group, which a run-loop observer closes before the loop waits. `undo`
closes an open group of the first level first; undoing runs a group's
actions in reverse into a new group on the redo stack (without the
open/close notifications, as on macOS). Empty groups are kept, a new
registration clears the redo stack, `levelsOfUndo` drops the oldest groups,
and titles read "Undo" or "Undo Name". Actions are target–selector pairs,
blocks (`registerUndoWithTarget:handler:`) and invocations recorded through
the proxy `prepareWithInvocationTarget:` returns. Targets aren't retained
(an invocation's arguments are, its target isn't). `removeAllActions`
drops the open groups too, the turn's included, so nothing is left to
undo; while registration is disabled, grouping and naming do nothing
either. An action is dropped with no borrow of the manager held, so an
object it kept alive may, going, call `removeAllActionsWithTarget:`.

A responder's `undoManager` is its next responder's; a window's is the
delegate's `windowWillReturnUndoManager:` if that gives one, else its own,
made when first asked for. `undo:` and `redo:` on a window send them to
that manager.

## Layout

**`NSTextContainer`**: a size (as good as unbounded both ways by default),
line fragment padding 5, a maximum number of lines, a line break mode, and
whether it follows its text view's width and height.

**`NSLayoutManager`** keeps an entry per paragraph of its storage
(`layout_cache`): the paragraph's lines once laid out, else an estimate of
its height, in chunks whose heights are summed lazily, so a paragraph's
position is a binary search and an edit that changes one height costs one
chunk. An edit replaces the entries of the paragraphs it touched with
estimates, keeping the old height where one paragraph became one; nothing
else is laid out again, since other paragraphs' lines are relative to their
paragraph's top. A question about an index lays out the paragraph holding
it (with contiguous layout, AppKit's default, the paragraphs before it
first, so positions are exact; with `allowsNonContiguousLayout`, those keep
their estimates until laid out). Questions about a point lay out the
paragraphs there. What remains is laid out when the run loop is idle, three
milliseconds a turn, after the display pass, for a layout manager whose
text a view shows: that is the main thread's, as views are. A layout
manager no view shows lays out only on demand, on whatever thread uses it,
so a storage and layout manager can be made on the main thread and handed
to a worker. The cache remembers how far from the start everything is laid
out, so contiguous layout's "lay out all before this" costs nothing once
done.

What layout changes, the views showing it draw again: a paragraph laid
out again as tall as before alone, one whose extent changed from its top
down (and the view sizes to its text again). An edit of up to eight
paragraphs a window shows lays them out at once, so typing redraws its
line unless the lines below move; background layout draws again only when
what it laid out moved what shows, and sizes the view at most four times
a second until it finishes.

Glyphs are one per UTF-16 unit, so a glyph index is a character index (the
low half of a surrogate pair is a null glyph). A line fragment is as wide
as its container and as tall as its line plus the line spacing after it (the
paragraph spacing before the first line and after the last; the text's
last line has neither after it). Its used rect is the line's text with the
padding at each end, where alignment and indents put it, kept inside the
width the line may take (a clipped line, or spaces hanging past a wrap or
a right edge, reach no further), from the line's top to the fragment's
bottom less the paragraph spacing after; the container's used rect is
their union. Text that is empty or ends
in a paragraph separator ends in the extra line fragment, in the typing
attributes (or the last character's). Hit testing below the text gives the
last glyph; past a line's end, the line's last glyph.

Drawing records the laid-out lines through `string_drawing` for the lines
in the dirty rect. A secure field's layout manager lays its text out as a
bullet per character.

**Temporary attributes** (`temporary.rs`) are runs over character ranges
the layout manager keeps apart from the storage, set, added and removed
over ranges and read with their effective (or longest effective) ranges,
as AppKit's. An edit keeps the parts of runs before and after it (those
after moved along) and drops the part it replaced, so text typed into a
run has none. Only a background color is drawn, under the text.
`layoutManager:didCompleteLayoutForTextContainer:atEnd:` tells the
delegate when `ensureLayout…` or background layout laid text out.

## Text blocks and tables

A paragraph style's `textBlocks` put its paragraph in blocks
(`blocks.rs`), outermost first. Measured on macOS:

- A block's widths are per layer (padding, border, margin) and edge, and
  its content width a dimension; each absolute or a percentage of the
  width of the rect it is laid out in, down as well as across. Blocks are
  equal only to themselves, and so paragraph styles with blocks are equal
  only when they hold the same ones.
- A block is laid out in its enclosing rect: the container less its line
  fragment padding at each end, or the enclosing block's layout rect. Its
  layout rect starts inside its left layers and is as wide as its content
  width, never wider than fits (no content width is zero wide). Line
  fragments are the layout rect with the padding at each end.
- Consecutive paragraphs that share a block (the same object, enclosed the
  same way) are in it together. Its top layers come before its first
  paragraph (blocks starting together add up), and after its last the
  text goes on below the lowest bottom of the blocks ending there (margins
  don't collapse). Its bounds are its content, from its first paragraph's
  first line fragment to its last paragraph's last line (before that
  paragraph's trailing spacing), with its layers around it; the used rect
  takes them in.
- A table's cells are table blocks; the paragraphs of a row (consecutive
  cells of the same row of the same table) are laid out side by side. The
  table is laid out like a block (none set is as wide as fits) and its top
  layers come before its first row (nothing after its last, as on macOS).
  Its width splits into as many columns as the row's cells reach: a cell
  with a content width takes that much, the others share the rest evenly
  in whole points, for both layout algorithms. A row is as tall as its
  tallest cell, and every cell's bounds run its height. With
  `collapsesBorders` each cell keeps half of each border (rounded up to a
  half point) and the table moves half a point right and down.
- Drawing fills a block's bounds, margins included, with its background
  color, then its borders inside its margins: left and right the border
  box's height, top and bottom between them. Collapsed borders are drawn
  centered on the cell's edges, so neighbors' fall on one line; borders
  thinner than a point are drawn as shapes, antialiased, so hairlines show
  at any scale. Blocks are drawn under the selection.

The layout manager answers `layoutRectForTextBlock:…` and
`boundsRectForTextBlock:…` (glyph range or index and effective range), and
attributed strings `rangeOfTextBlock:atIndex:` and
`rangeOfTextTable:atIndex:`. Paragraphs in blocks read their neighbors'
blocks when laid out, and a table row's paragraphs are always laid out
together; an edit near blocks lays the paragraphs around it (and the rest of
their rows) out again. Paragraphs in no blocks pay nothing for this.

Not done: height dimensions, vertical alignment, row spans (a cell spanning
rows lays out in its first row, and later rows don't leave its columns),
`hidesEmptyCells`, and a table's own automatic column widths beyond cells'
content widths.

## The text view

`NSTextView` (and the `NSText` API it inherits) is a view over the three
objects, which it makes (`initWithFrame:`, growing down with its text) or
is given (`initWithFrame:textContainer:`, keeping its frame);
`scrollableTextView` and its kin put one in a scroll view as AppKit sets it
up (plain text, but for `scrollableDocumentContentTextView`; width
tracking; sizable both ways). In a clip view, a text view's minimum size
follows the clip view's, so it always fills what shows and grows down with
its text. Its `string` is the storage's live string. When a layout
manager takes another storage (`replaceTextStorage:`), its view shows and
edits that one.

**Edits.** A user's edit is one transaction in AppKit's order:
`shouldChangeTextInRange:replacementString:` (which begins editing, with
`textShouldBeginEditing:` and the did-begin notification, then asks the
delegate), the undo registration, the storage edit in the typing
attributes, the selection (the delegate's `willChangeSelection…` and the
did-change notification with the old ranges), `didChangeText` (the
did-change notification; a view that isn't first responder ends editing at
once), one size change, and scrolling the selection into view. A program's
`setString:` and `replaceCharactersInRange:withString:` don't post
`textDidChange` and leave the scroll position alone; `setString:` selects
the end. A program editing the storage itself moves the view's selection
as AppKit's does: along with the text after the edit, to the edit's end
where they overlap, into one range; either way, text an input method is
composing is no longer marked, and a run of typing ends.

**Undo** is registered in `shouldChangeTextInRange:replacementString:`, as
AppKit does, so a program's own `shouldChange…`, edit and `didChangeText`
is undoable too (unnamed). Typing and deleting backward coalesce into one
action while each edit ends where the run's text does (reaching back past
its start included), until the selection moves some other way,
`breakUndoCoalescing`, or the manager lets go of the action; a committed
composition joins the run it follows. Undoing selects the text that came
back, redoing puts the insertion point after it (an attribute change
selects its range both ways). Typing, deleting backward and the insertion
commands are named "Typing", `paste:` "Paste", `cut:` "Cut"; the other
deletions, `transpose:`, case changes and `readSelectionFromPasteboard:` go
unnamed, as on macOS. With registration disabled nothing is registered or
named.

**Selection.** Characters are ICU4X grapheme clusters and words its
word-like segments, found in a window of the storage around the caret, so a
command in a long document costs what the text near the caret costs.
`setSelectedRanges:` keeps ranges inside the text, in order, merged where
they overlap or touch, empty ones left out unless all are; the first is
the selection typing replaces. A selection change draws again only what
shows of the lines one selection covers and the other doesn't, and drawing
measures only the dirty rect's lines of a selection, so selecting all of a
long text costs what shows of it. Mouse
selection tracks drags by character, word (double click) or paragraph
(triple click), with the anchor kept, and extends with Shift. Links
(`NSLinkAttributeName`) go to the delegate's `textView:clickedOnLink:atIndex:`.
The caret blinks as GTK's does (0.6 s on, 0.6 s off, from each edit or
move, and solid after ten seconds without one) while the view is first
responder in the key window.

**Commands.** About eighty `NSStandardKeyBindingResponding` commands:
moves by character, word, line, paragraph, page and document, with their
selection-extending forms (to a line's, paragraph's or the text's end,
the selection's edge that way goes and the other stays), left and right
in the text's direction, up and down keeping a goal column; `select…`;
`insertNewline:`, `insertParagraphSeparator:` (U+2029), tabs and backtabs;
deleting by character, word, line and paragraph (to the kill ring, which
`yank:` puts back); a character deleted is one of Unicode's clusters before
15.1, as AppKit's is, so a conjunct of an Indic script, one character to
move over, is deleted a consonant at a time; `transpose:`, case changes
(of the word the insertion point follows), the mark; `copy:`, `cut:`,
`paste:` and `delete:` through the general pasteboard (as plain text);
`validateMenuItem:` and `validateUserInterfaceItem:`, which look only at
the pasteboard's types, reading nothing. A secure view won't copy or cut.
Like AppKit's, it has no `transposeWords:` or `changeCaseOfLetter:`.

**Input methods.** The view is an `NSTextInputClient`: marked text shown
underlined, `insertText:replacementRange:` (text put in elsewhere leaves
the selection where it was), `firstRectForCharacterRange:actualRange:` in
screen coordinates, `characterIndexForPoint:`. Keys go to the input context
first, then `interpretKeyEvents:`; `doCommandBySelector:` offers the command
to the delegate's `textView:doCommandBySelector:` first. A composition
registers one undo action when committed.

## The field editor

`-[NSWindow fieldEditor:forObject:]` asks the delegate's
`windowWillReturnFieldEditor:toObject:` first, else gives a secure field
the window's secure editor and anything else the shared one, made when
first asked for. A session puts the editor over the control's text rect in
a plain clipping view, set up as AppKit's (as measured): plain text, no
background, a container 40 000 points wide that doesn't track the view,
the control's text, font, color and alignment, the control as delegate.
Return, Tab and Backtab end editing with that `NSTextMovement` (Escape is
only offered to the delegate); after Return the field goes on editing with
its text selected, after Tab and Backtab the window selects the next or
previous key view. `-[NSWindow endEditingFor:]` makes the window first
responder. The controls' text fields reach it through
`textkit::field_editor`'s functions (see the module). A session whose
control has gone, or whose editor stopped being first responder without
being asked (its control left the window), is over: it is ended quietly
when next come across, touching the control only while it is alive, and
the editor forgets it was editing, so the next session begins afresh.

## Performance

`examples/textbench` (release; median of seven runs; Linux in a VM on an
M-series Mac, and AppKit on the same Mac): 11 MB of text in 200 000 lines
of 13-point monospaced text, in a scrollable text view not in a window.

| | Sidestep | AppKit |
|---|---:|---:|
| `setString:` | 25 ms | 1.7 ms |
| first screen laid out | 0.65 ms | 0.53 ms |
| all of it laid out | 2.8 s | 1.5 s |
| last screen, non-contiguous, fresh view | 0.33 ms | 0.34 ms |
| keystroke and its screen, contiguous (p50 / p99) | 0.048 / 0.071 ms | 0.15 / 9.6 ms |
| keystroke, non-contiguous (p50 / p99) | 0.048 / 0.069 ms | 0.86 / 0.99 ms |
| delete backward, contiguous (p50 / p99) | 0.050 / 0.074 ms | 0.12 / 0.22 ms |
| keystroke, then the string's length and paragraph (p50 / p99) | 0.048 / 0.071 ms | 0.13 / 0.26 ms |
| select all, with an input method's rect for it | 0.003 ms | 27 ms |
| keystroke in a 32 KB paragraph (p50 / p99) | 2.9 / 3.5 ms | 3.4 / 4.1 ms |

`setString:` builds the paragraph tree and fixes each paragraph's
attributes up front, where AppKit's storage defers that; full layout goes
at about 14 µs a line of the text engine's shaping. Neither blocks a
keystroke: sizing a text view never lays text out (it uses the estimates,
and background layout sizes it again as it goes), and nor does a selection
change. An edit lays its paragraph out again whole, so a keystroke costs
what its paragraph's layout costs (the last row). The view isn't in a
window, so drawing isn't measured.
