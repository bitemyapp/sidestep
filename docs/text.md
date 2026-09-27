# Text editing

TextKit 1, TextKit 2 and the text view, as Sidestep implements them on
Linux: `NSTextStorage`, `NSLayoutManager`, `NSTextContainer`, `NSText` and
`NSTextView` with the field editor controls edit in, `NSUndoManager`, text
blocks and tables, and TextKit 2's `NSTextLayoutManager`,
`NSTextContentStorage` and the rest over the same storage and line layout.
The code is in `crates/sidestep-appkit/src/textkit/` and `textkit2/` (and
`NSUndoManager` in `crates/sidestep-foundation/src/undo.rs`); it stands on
the line layout of `text/lines.rs` described in
[architecture.md](architecture.md#text).

What the classes do was measured on macOS, not read from Apple's headers
or documentation: each behavior below that a program can see is checked
by a conformance test that runs against AppKit first and Sidestep second
(`conformance/tests/text_storage.rs`, `text_fixing.rs`, `text_layout.rs`,
`text_blocks.rs`, `undo_manager.rs`, `text_view.rs`, `textkit2.rs`,
`textkit2_view.rs`).

## Text storage

**The text.** `storage::Storage` keeps a text as a tree of paragraphs: each
paragraph its UTF-8 text (separator included), its length in UTF-16 units
and in bytes, whether it is all ASCII, and its attribute runs over UTF-16
units. Paragraphs sit in chunks of at most 128 (a chunk's sums let an index
find its chunk in a binary search), and a paragraph longer than 4096 bytes
keeps UTF-16 checkpoints, so a UTF-16 index finds its byte in a few hundred
bytes at most. An edit inside one paragraph that makes no new paragraphs
changes it in place; others splice paragraphs. Every edit of the text bumps
a generation. Property tests check the tree against a plain string and
runs over random edits, CR LF pairs and surrogates included.

Long text taken in whole (16 KB or more: a text set at once, a large
paste) isn't cut into paragraphs as it comes. It is copied once into a
shared buffer and cut into raw chunks of whole paragraphs, a few kilobytes
each, whose sums (UTF-16 units, bytes, paragraphs) one pass over the bytes
counts, in byte lanes the compiler vectorizes; a chunk takes about as many
bytes as 64 paragraphs took in the one before, fewer where paragraphs get
short, so none holds more than 128. A raw chunk is cut into its paragraphs,
which slice the shared buffer until one is edited, when something first
reads them (finding an index, laying out); what needs no paragraphs reads
it as it is: the whole text, a chunk's text, its runs (which may cross its
paragraphs' ends), attributes set over it, effective ranges crossing it,
and layout estimates. Setting 11 MB of text costs the copy and the count;
the property tests run through raw chunks too, cut and uncut. When most of
such a text is deleted, so that the shared buffers would hold more than
twice the text left (and 16 KB more), what is left is copied into buffers
of its own, a chunk's to each, and the old ones go.

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
made with text has its attributes fixed from the start (lazily, when it is
long), and the scripting accessors `font` and `foregroundColor` read the
first character's and set one over all the text.
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

**Fixing lazily.** As on macOS, the storage fixes attributes lazily
(`fixesAttributesLazily`; a subclass with text of its own doesn't, unless
it says so). Measured there: an edit of 65 536 units or more, or any edit
while text is left to fix, isn't fixed as it is processed. The edit,
through the end of its last paragraph, joins the text left to fix (one
range covering it all, moved by later edits), the change the delegate
hears of covers its paragraphs whole, and its mask gets no attributes from
fixing. The text is fixed quietly (no notification, no delegate call, no
change to process) when something asks for its attributes
(`attributesAtIndex:effectiveRange:`, `attribute:atIndex:…`, the longest
effective ranges, and what goes through them, such as substrings) or the
layout manager lays it out, a stretch at a time: asked about an index
nearer the start of what is left than its end, from that start through
the index, 65 536 units at least; otherwise from the index's paragraph to
the end. So effective ranges end where the text fixed does, and reading
on from the start, or laying out down the text, fixes 64 KB at a time.
Fixing notes in each chunk how far from its start it has fixed, and any
change of the chunk clears the note, so a stretch fixed again reads only
what changed since: typing where text is left to fix, which puts what is
left back at the edit, fixes the edited chunk each keystroke, not 64 KB.
`ensureAttributesAreFixedInRange:` fixes the same way for its range; for a
subclass that says it fixes lazily it is the subclass's to call, and it
fixes through `fixAttributesInRange:`, whose edits are no change to
process. Fixing itself skips a chunk whose paragraphs all have one set of
attributes with a font, raw or not, and sets a font over a raw chunk
without cutting it, so text set with a font (as a text view's is) is
never cut to be fixed. A paragraph style set in the middle of a paragraph
is taken away as the edit is processed, and the change reported reaches
the paragraph's end, as on macOS. Where AppKit differs: its effective
ranges, once text has been fixed in several stretches, sometimes end where
a stretch did though the attributes either side are equal; Sidestep's
runs with equal attributes merge (an effective range may be shorter than
the longest, so both answer rightly).

**Longest effective ranges** walk the runs out from the index, each way,
until the attributes (or the one attribute's value) differ or the limit is
reached, so walking a text by them costs what the runs crossed cost. What
they find is clipped to the limit as AppKit clips it (measured): to (0, 0)
when it only touches the limit or misses it (the index needn't be in the
limit), and a limit past the text's end is no error, its end wrapping
rather than checked; Foundation's attributed strings clip the same way.

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
chunk. Text the storage hasn't cut into paragraphs is estimated a raw chunk
at a time, its paragraphs alike (as long as the chunk's average), and the
cache holds such a run as one chunk of a count and an entry, runs alike
joined: positions in it are multiples, and setting an entry there gives the
half-chunk of paragraphs around it entries of their own. Before laying
paragraphs out the manager has the storage fix their attributes where it
put that off, and a change widened to whole paragraphs for the delegate is
laid out again as edited (with what the delegate edited when told it was
processed). An edit replaces the entries of the paragraphs it touched with
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
`paste:`, `pasteAsPlainText:`, `pasteAsRichText:` and `delete:` through
the general pasteboard (rich text as [below](#rich-text));
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

## Rich text

Attributed strings are read and written as RTF, flat RTFD, HTML and
plain text by the methods AppKit adds to them
(`initWithData:options:documentAttributes:error:` and its URL, RTF, RTFD
and HTML forms, `dataFromRange:documentAttributes:error:`,
`RTFFromRange:documentAttributes:`, `RTFDFromRange:…`, and a mutable
string's `readFromData:…` and `readFromURL:…`). The code is in
`crates/sidestep-appkit/src/rich/`: each format reads into and writes
from plain data (a text with character runs and paragraphs), which
`convert` turns into an attributed string and back. Everything a program
can see was measured on macOS (`conformance/tests/rich_text.rs`).

**Reading.** The type is the one the options name, else what the data
starts with: flat RTFD, `{\rtf`, an HTML document (`<html`,
`<!DOCTYPE html`, `<head`), else plain text. A type Sidestep doesn't read
is error 65806; data that isn't the RTF it is said to be, 256; RTF that
ends before its last group closes, 259; a URL that isn't a file's, 262
(`NSFileReadUnsupportedSchemeError`). The document attributes are
AppKit's: for RTF the page (US Letter and RTF's margins unless the
document gives its own), the default tab interval (0 in Cocoa's RTF, half
an inch in others'), hyphenation, text scaling, the Cocoa RTF version
(80 when the RTF names none) and whatever the document's information
holds (title, author, keywords, …); for HTML the type (and a Cocoa HTML
writer's version); for plain text the encoding it was read in.

- **RTF** is read by a reader written from Microsoft's published
  specification and what AppKit writes: fonts by name (PostScript or
  family names, else Helvetica, Times or Courier by the family class) with
  `\b` and `\i` adding traits, Helvetica before any `\f` or for a font
  the table lacks (`\deff` is ignored and `\plain` goes back to `\f0`,
  as in AppKit); colors from Cocoa's extended color table
  in their spaces (sRGB, calibrated RGB, Display P3, gray, CMYK; a system
  color by its name), else from `\red\green\blue`; underlines and
  strikethroughs with their styles and colors, super- and subscripts,
  baseline offsets, kerning, shadows, strokes, obliqueness, expansion,
  ligatures; hyperlink fields as links; `\uN` with its stand-ins and
  surrogate pairs; Windows-1250 to 1257, Mac Roman and the DOS pages for
  `\'hh`, by the document's code page or the font's charset. A paragraph
  takes the paragraph formatting in effect at its end, and has a
  paragraph style only once some control word sets one; after `\pard` it
  is left-aligned (RTF has no natural alignment), left to right, with no
  tab stops. As on macOS, a right indent becomes a tail indent from the
  leading margin (the page's text width less the indent), `\cb1` in
  Cocoa's RTF is no background, `\line` is U+2028 and an optional hyphen
  is dropped; a parameter keeps the low 32 bits of its digits, and
  indents and line heights are worked out in floating point (so no
  number overflows); a mutable string reads RTF onto its end. Unlike
  AppKit, Word's highlights and character shading are backgrounds,
  `\cb0` is none, table cells and rows become tabs and paragraph ends,
  `\expnd` is in quarter points, and `\bin` data is skipped.
- **HTML** is read without a browser engine, by a tolerant tokenizer
  and a small CSS: inline styles, `<style>` sheets with type and class
  selectors, `<font>`. What it makes of markup follows what WebKit makes
  of it on macOS: Times 12, CSS pixels as points, `monospace` alone at
  13; bold, italic, underline and strikethrough elements; headings at 24,
  18, 14, 12, 10 and 9 points, bold, with their header level; links blue
  and underlined, with a URL when absolute (or made so by a base URL);
  each paragraph with a style (left to right, no tab stops, a 36-point
  default tab interval, its block's alignment and indents, and its bottom
  margin as the spacing after: a `<p>`'s 1em); list items with their
  markers between tabs, in the style of the item's first text, indented
  by depth alone, the blocks in an item (a `<p>` in an `<li>`, as Google
  Docs, GitHub and Confluence write them) lines of its paragraph, which
  takes their spacing, and nested lists, tables and preformatted text
  ending it; letters and Roman numerals as macOS counts them (an item's
  `value` ignored); `<br>` as U+2028 inside a paragraph, heading or list
  item and a paragraph end elsewhere (outside any block, a trailing one
  too); `<body>` not a block, so a fragment's last inline text ends
  without a newline; white space collapsed but in `<pre>`, and an
  `Apple-converted-space`'s no-break spaces read as spaces; opaque black
  as no color. Bytes are decoded by their byte order mark, the options'
  encoding, a `<meta>` charset, else Windows-1252. Unlike macOS, a
  block's margins add up with its ancestors' and its first line starts
  with the rest (macOS takes only the innermost `<p>`'s or
  `<blockquote>`'s margins and starts the first line at the page's margin
  unless `text-indent` is positive, so a quotation's first line hangs
  out).
- **Plain text** is UTF-8 unless the options say (a UTF-16 byte order
  mark is followed; bytes that aren't UTF-8 are Mac OS Roman), in the
  options' default attributes, else Helvetica 12.

**Writing.** `NSDocumentTypeDocumentAttribute` says the format (none, or
one Sidestep doesn't write, is error 66062).

- **RTF** is laid out as AppKit writes it: Windows-1252 with Cocoa's RTF
  version, a font table of PostScript names, a color table (auto and
  white first) with Cocoa's extended table beside it, the document's
  information and page, then a `\pard` wherever a paragraph's style
  changes (the twelve default tab stops included) and each run's
  formatting as it changes (the color written again after each `\pard`),
  links as fields, text beyond Windows-1252 as `\uN`. Text without a font
  is written in Helvetica 12.
- **RTFD** is flat RTFD with the RTF as its `TXT.rtf`, byte for byte as
  AppKit writes it (attachments aren't written yet).
- **HTML** is an HTML 4.01 document in UTF-8 shaped as AppKit's: a style
  sheet of paragraph and span classes (margins, indents, alignment, the
  paragraph's font; a run's font, color, background, decoration, baseline
  offset and kerning; Helvetica 12 for text without a font), `<b>`, `<i>`,
  `<sup>`, `<sub>` and `<a href>`, tabs and runs of spaces kept for
  browsers.
- **Plain text** is UTF-8 unless `NSCharacterEncodingDocumentAttribute`
  names another encoding (UTF-16 with its byte order mark,
  little-endian).

**Pasteboards.** An attributed string writes itself as RTF, HTML and plain
text (AppKit writes RTF and text; HTML is for the Linux programs that
read no RTF, browsers among them), with RTFD first when it has
attachments, and reads itself from flat RTFD, RTF, HTML or text, in that
order: text as a string, with no attributes, and HTML that names no
charset as UTF-8 where it is (Linux programs put it there so). A rich text
view writes its selection as RTF, HTML and text (a plain one as text; with
no selection, nothing), and reads RTF, RTFD, HTML and text in that order
(a plain one text first): rich text keeps its attributes in a rich view
and is text in a plain one or a field editor. `pasteAsPlainText:` reads
text alone. The lists are AppKit's old type names, as its text view gives
them (AppKit's writes only types named so: asked for `public.rtf` it
declares the type but writes nothing and answers NO, where Sidestep's
writes it). `writeSelectionToPasteboard:type:` writes one type beside
those already declared.

## TextKit 2

TextKit 2 is a layer over TextKit 1's pieces (`textkit2/`), not a second
layout engine: the content storage presents the text storage's paragraph
tree as elements, and the layout manager lays each element out through the
paragraph engine TextKit 1 uses (`text/lines.rs`), stacked as TextKit 2
stacks paragraphs. As for TextKit 1, what a program can see was measured
on macOS (`conformance/tests/textkit2.rs` and `textkit2_view.rs`).

**Locations and ranges.** A content storage's locations are
`NSCountableTextLocation`s, offsets in UTF-16 units that compare, hash and
describe themselves by number ("12"); moving one outside the document gives
nil. An `NSTextRange` holds two locations (nil if the end comes first; its
description is "2...5"), and keeps their offsets when both are countable,
so comparing, containing and intersecting cost no messages (locations of
other kinds go through `compare:`). A range contains its start and not its
end; an empty range contains nothing and intersects nothing, and is
contained in a range whose start it sits at but not one whose end it sits
at; ranges that only touch don't intersect; a union spans the gap between
ranges.

**Elements.** `NSTextContentStorage` (with a text storage of its own when
made, and its storage's `textStorageObserver`) hands out an
`NSTextParagraph` for each of the storage's paragraphs, separator included
in its range: the content range leaves the separator out ("\r\n" is two
units; U+2028 separates nothing), the separator range is empty for the last
paragraph without one. Elements are made when first asked for and kept, in a
chunked sequence of elements and gaps over the text, so the same paragraph
is the same object (with the same layout fragment) until an edit touches it:
an edit drops the elements of the paragraphs it touched (attributes changing
too: an element's text includes its attributes), and the ranges of those
after it move, brought up to date from a log of edits when asked for rather
than by visiting every element (every few hundred edits the kept ones are
brought up to date at once). The delegate's
`textContentStorage:textParagraphWithRange:` is asked once per paragraph,
with its range; a paragraph it returns (its own text, perhaps several
paragraphs of it) stands for the storage's, given the range and the content
storage, its separator measured from its own text; its range is the
paragraph's even when its text is shorter (the content and separator ranges
divide the paragraph's), and its fragment, once laid out, covers only its
own text (no fragment holds the rest).
`textContentManager:shouldEnumerateTextElement:options:` is asked of each
element enumerated, with the options, and filters what the block gets.
`textElementsForRange:` gives the elements, from the range's start, that
start inside the range, up to the first that doesn't (so a range starting
inside an element, or an empty one, finds none); an element made by
`textElementForAttributedString:` has no range or content manager yet. A
content storage subclass may hand out elements of its own (grouping
paragraphs, say) by overriding the enumeration; laying out the document's
last element asks a paragraph for its `paragraphSeparatorRange` (AppKit's
own measures neither range for an element its content storage didn't make,
so such subclasses answer both themselves). Enumerating forward from a
location starts with the element holding it and returns the end of the last
element given to the block (the one it stopped on included; the location
itself when there was none; nil in an empty document); in reverse, it starts
with the element holding the unit before the location, returns the start of
the last one given, and from nil gives nothing.
`performEditingTransactionUsingBlock:` only marks the transaction
(`hasEditingTransaction`): the storage processes each edit in it as it
comes.

**Layout fragments.** `NSTextLayoutManager` keeps an index over the
document (`seq.rs`): stretches known only by an estimate of their height,
and elements with their fragments, laid out or not, in chunks with lazily
summed lengths and heights. A fragment's place is the height of what comes
before it, estimates included: laying out only what is asked for (the
viewport, a range, a point) places it where the rest's estimates put it,
as TextKit 2 does on macOS (a far fragment laid out alone is placed by the
estimates above it). Estimates are TextKit 1's (a line of the paragraph's
font per container width of text, at half an em a unit, a raw stretch of
the storage at a time for text not yet cut into paragraphs), scaled by how
the fragments laid out so far compared with their estimates, so the
document's height settles as more of it is laid out. Near the start (the
first 16 K units) what comes before a fragment is laid out first, so short
texts are placed exactly. Stretches turn into elements when something
needs them: the layout manager asks the content manager for the elements
from an offset (its own content storage directly; anything else, a
subclass grouping paragraphs into elements included, through
`enumerateTextElementsFromLocation:options:usingBlock:`) and the delegate's
`textLayoutManager:textLayoutFragmentForLocation:inTextElement:` for each
fragment. Edits turn what they touched back into stretches (one element for
one keeps its height as the estimate); `invalidateLayoutForRange:` does the
same, keeping the fragments for their elements' return: an element the
content manager hands out again keeps its fragment, the same object, which
is what macOS shows (state 0, the frame kept until laid out again).

A fragment not laid out is in state 0 with a zero frame and no lines; laid
out, state 3. Laying one out reads its element's text (a content storage's
paragraph straight from the paragraph tree and attribute table, anything
else through its attributed string) and stacks its paragraphs as measured:
line spacing above every line but the document's first (so between lines,
and before a paragraph's first line with its spacing before), the spacing
after a paragraph below its last line except at the document's end, the
document's first element with nothing above it, and the document's last
element ending in an empty line when its text ends in a separator (spaced
as a paragraph of its own; the extra line fragment is inside the fragment,
and `EnsuresExtraLineFragment` only adds a fragment to an empty document).
The frame is as wide as the lines reach, from the leftmost line's start
(padding, indents and alignment move it) to the furthest one's end,
trailing spaces included. Then the layout manager asks the fragment its
`layoutFragmentFrame`, so a subclass's frame (taller, or of no height) is
what places the fragments below, as on macOS; usage bounds are the frames
laid out, down to the document's estimated end once a view sizing to it
asked (a text view does). Line fragments' character ranges are in their
element's text, their typographic bounds in the layout fragment's frame,
their glyph origin the baseline's height; `locationForCharacterAtIndex:`
is from the typographic origin, at the baseline;
`textLineFragmentForVerticalOffset:requiresExactMatch:` finds the line
whose box holds the offset (or, inexactly, the first whose bottom is below
it); `characterIndexForPoint:` is the character under the point (the first
left of the line, `NSNotFound` past its end). The rendering surface is the
frame with each line's box widened by its height across and a quarter of it
up and down. `textLayoutFragmentForLocation:` finds nothing at the
document's end.

**Where lines are** is where their fragment's `layoutFragmentFrame` puts
them (a subclass's own, moved or taller than its lines): carets, segments,
selection highlights and clicks go by it, as drawing does. A click in a
fragment's frame below its lines (a subclass's padding) finds the end of its
last line, as `NSTextSelectionNavigation` does on macOS. An empty document
has no fragments, but its extra line fragment is laid out (at the padding,
of no width and a line of the typing attributes' height) when a caret or
a range asks for it, or a viewport shows it (configured, with an empty
range), and the usage bounds are its frame then; a click in an empty
document makes no selection. Enumeration of fragments forward
from a location starts with the fragment holding it, in reverse with the one
before it (from nil, the end), and returns the end (the start, in reverse)
of the last one given; `EnsuresLayout` lays out each as it goes, and a
fragment laid out moves with the index as what is above it changes.
`enumerateTextSegmentsInRange:type:options:usingBlock:` gives a caret no
width and a segment per line the range meets, measured as follows. A
selection or highlight segment stops at the line's trailing edge (the
container's width less its padding), reaches it where the range goes on
past the line or takes in its separator, starts at the leading edge on
every line but the first, and starts down where the one before it ends.
`HeadSegmentExtended` starts every segment but the first at the leading
edge; `TailSegmentExtended` takes the segments the range goes on past to
the trailing edge, and the last one where the range reaches its line's end
(where the range stops short of the end of a paragraph's last line, an
empty segment at that line's end reaches from its text's end to the edge);
`MiddleFragmentsExcluded` keeps the first and last lines' segments (a
selection's last reaching up to the first's); `RangeNotRequired` passes no
range. The layout manager answers `NSTextSelectionDataSource`'s questions
from its layout. Rendering attributes are kept over ranges (set, added to,
removed), moved by edits before them and enumerated from a location (the
run there cut at it) or back from it; they aren't drawn yet.

**Selection navigation** moves as measured: up and down go a line by
character (from a selection's start), and like forward and back by
anything larger; a caret moves from where it is, a selection collapses to
its edge by character and moves from its edge otherwise; extending by
character moves a selection's end from its start, by anything larger its
end forward or its start back; a move to a line's end, and a larger
extension back, are upstream. Deletion ranges are the selection, or for a
caret the move's span.

**Drawing.** The default `drawAtPoint:inContext:` draws a fragment's lines
(backgrounds, then glyphs) with its frame's origin at the point, a line
fragment's with its typographic origin there, through
`textkit2::draw::with_context_state(cg, f)`: the one door from a
`CGContext` to the drawing state. A text view hands each fragment the point
zero and a context whose origin is moved to the fragment's frame origin, as
macOS does (`textkit2::draw::with_cg_context_at`). That context is the
current graphics context's `CGContext`, the same graphics state, so a
subclass's CoreGraphics calls and the default drawing land in the same
place; a fragment drawn into another `CGContext` (a program's bitmap
context) draws with that context made the current one for the call
(`coregraphics::context::drawing_into`).

**The viewport.** `NSTextViewportLayoutController` (none until the layout
manager has a container, as on macOS) lays out as measured: the delegate's
`textViewportLayoutControllerWillLayout:`, its viewport bounds, the
fragments the bounds meet laid out and handed to
`textViewportLayoutController:configureRenderingSurfaceForTextLayoutFragment:`
top to bottom, then `textViewportLayoutControllerDidLayout:`; the viewport
range is theirs together. Bounds of no height (no delegate, say) lay out
nothing and leave no range; an empty document's viewport holds its extra
line fragment. `adjustViewportByVerticalOffset:` moves the bounds and lays
out nothing; `relocateViewportToTextLocation:` moves them to where the
fragment holding the location is estimated to be, makes the range the empty
range there, and returns that height, laying nothing out and asking the
delegate nothing. The fragments a layout configured are kept by the
controller, and a text view draws those, whatever its subclass's delegate
methods do.

**Text views.** A text view is TextKit 2 when made with `initWithFrame:`
(and `init`, `new`, `scrollableTextView`, `fieldEditor`,
`initUsingTextLayoutManager:YES`, `textViewUsingTextLayoutManager:YES`),
unless its class overrides `drawRect:`; given a container, it is in the
container's mode whatever its class (a text layout manager's is TextKit 2).
The window's field editor is TextKit 2, the secure one TextKit 1. Asking a
TextKit 2 view for its `layoutManager` switches it to TextKit 1 for good: an
`NSLayoutManager` takes the same container and storage, and
`textLayoutManager` and `textContentStorage` are nil from then on (the text
layout manager keeps its container and content). A TextKit 2 view is its
viewport controller's delegate (a subclass's `viewportBoundsFor…` calling
`super` gets the view's): its viewport is what its clip view shows in
container coordinates, its whole width across, from the top of what shows
(not above its bounds) for the clip view's height (on past the view's
bottom); in a window and no clip view, its visible rect; in neither, as good
as unbounded, so laying the viewport out lays out all of the text, as on
macOS (20 000 lines in 0.4 s there). The viewport is laid out before the
view draws (`viewWillDraw`) when layout changed or the view scrolled; text
that showed before stays in place on screen when laying out what is above it
moves it (the view scrolls by as much). The fragments configured draw after
the view's `drawRect:` (as macOS draws them above it, in views of their
own), with the marked text and the caret above them, so a subclass drawing
in `drawRect:` draws under the text. The view sizes to the usage bounds (the
document as estimated), and its selection, caret, clicks, commands, input
method rects and scrolling go through the text layout manager's lines; the
text layout manager's `textSelections` follow the view's selection. Edits go
through the storage as in TextKit 1; the content storage hears of them and
tells the layout manager, which lays out again only the edited element (in
place when it keeps its height). A subclass overriding `textContainerOrigin`
is asked for the origin (in TextKit 1 too). `sizeToFit` lays out the start
of the text first (a short text whole, so its height is exact, and an empty
one's extra line); scrolling a range into view lays out the fragment at its
start, not the whole range. The switch to TextKit 1 posts
`NSTextViewWillSwitchToNSLayoutManagerNotification` and
`NSTextViewDidSwitchToNSLayoutManagerNotification` around it.

**Known differences from macOS**, measured, and kept on purpose or not yet
matched:

- A laid-out fragment below an edit moves to its place when enumerated;
  macOS keeps its old frame (overlapping the one above) until the viewport
  is laid out.
- Near the start (the first 16 K units) laying out a fragment lays out
  what comes before it; macOS lays out it and what follows it. Where each
  goes is the same.
- A fresh layout manager's `textSelections` is an empty array (nil on
  macOS, which objc2's non-null binding can't return).
- Elements the content storage's delegate filters out of enumeration are
  still laid out (macOS skips them in layout too).
- In a fragment a subclass moved, `characterIndexForInsertionAtPoint:`
  answers what selection navigation does; macOS answers the fragment's
  start for any point in it. macOS also places the fragments after a moved
  one from its moved origin (its `super` frame is moved too).
- Segments: macOS repeats the last segment of a range reaching the
  document's end, and with `MiddleFragmentsExcluded` and
  `TailSegmentExtended` treats a tail at the document's end by type.
- `sizeToFit` of a long text: macOS lays all of it out (0.7 s for 60 000
  lines); Sidestep lays out the first 16 K units and estimates the rest.
- `textLineFragmentForTextLocation:isUpstreamAffinity:` for the location
  just before its fragment: the fragment's first line on macOS, nil here
  (nil on both further before).
- Through a text view, macOS makes new fragments for the paragraphs after
  an edit; Sidestep keeps them (both do through the storage alone).
- After `relocateViewportToTextLocation:`, the fragment there is in state 0
  (1, estimated, on macOS).

## Performance

`examples/textbench` (release; median of seven runs; Linux in a VM on an
M-series Mac, and AppKit on the same Mac): 11 MB of text in 200 000 lines
of 13-point monospaced text, in a scrollable text view not in a window.

| | Sidestep | AppKit |
|---|---:|---:|
| `setString:`, 1 KB | 0.007 ms | 0.03 ms |
| `setString:`, 1 MB | 0.09 ms | 0.86 ms |
| `setString:`, 11 MB | 1.0–2.3 ms | 1.8–2.0 ms |
| `setString:` and the first screen laid out | 1.5 ms | 2.2 ms |
| `setString:`, then the attributes at the end | 0.9 ms | 1.7 ms |
| the storage's `replaceCharactersInRange:withString:`, all of it | 1.6 ms | 1.7 ms |
| the storage's `setAttributedString:`, two runs a line | 33–45 ms | 225 ms |
| first screen laid out | 0.64 ms | 0.54 ms |
| all of it laid out | 2.9 s | 1.5 s |
| last screen, non-contiguous, fresh view | 0.33 ms | 0.33 ms |
| keystroke and its screen, contiguous (p50 / p99) | 0.047 / 0.075 ms | 0.14 / 10 ms |
| keystroke, non-contiguous (p50 / p99) | 0.042 / 0.07 ms | 0.88 / 1.2 ms |
| delete backward, contiguous (p50 / p99) | 0.048 / 0.074 ms | 0.12 / 0.24 ms |
| keystroke, then the string's length and paragraph (p50 / p99) | 0.049 / 0.075 ms | 0.13 / 0.25 ms |
| select all, with an input method's rect for it | 0.005 ms | 27 ms |
| keystroke in a 32 KB paragraph (p50 / p99) | 3.1 / 4.0 ms | 3.4 / 4.3 ms |
| keystroke in highlighted text left to fix, non-contiguous (p50 / p99) | 0.045 / 0.066 ms | 9.2 / 11 ms |
| the same once all of it is fixed (p50 / p99) | 0.043 / 0.067 ms | 7.4 / 8.7 ms |
| 1 MB of rich text (four bold words a line) written as RTF | 141 ms | 127 ms |
| the same RTF read | 69 ms | 59 ms |
| the same written as HTML | 198 ms | 719 ms |
| the same HTML read | 123 ms | 3.7 s |

`setString:` copies the text once and counts its paragraphs and units;
cutting it into paragraphs and fixing its attributes wait until something
reads or lays out the text there, as AppKit's storage waits to fix (before
this, it built all 200 000 paragraphs and fixed them up front: 25 ms for
11 MB, 2 ms for 1 MB, and 380 ms for `setAttributedString:`, whose 400 000
runs, with a dictionary each, now go in as they are, looked up among the
last few by `isEqual:` before the storage's table). Typing costs what it
did, in text left to fix too (the highlighted rows: nine runs a line, set
through the storage in the view's font, before and after all of it is
fixed; fixing reads again only the chunk an edit changed). Full layout goes
at about 14 µs a line of the text engine's shaping. Neither blocks a
keystroke: sizing a text view never lays text out (it uses the estimates,
and background layout sizes it again as it goes), and nor does a selection
change. An edit lays its paragraph out again whole, so a keystroke costs
what its paragraph's layout costs (the 32 KB paragraph's row). The view
isn't in a window, so drawing isn't measured. Rich text's writers and
readers pass each paragraph and run once, so their time grows with the
text (the rich text rows: 18 182 paragraphs of nine runs, as copying from
and pasting into a rich text view writes and reads them).

`examples/textkit2bench` (release; median of seven runs, Linux in a VM on
an M-series Mac and AppKit on the same Mac): the same 11 MB in a TextKit 2
view as `scrollableTextView` makes it, 800 × 600 points, not in a window,
laying out its viewport. The last three rows make every fragment first
(enumerating them all, as scrolling through the text also does), then
free them all: a new font, a new text, and the view itself (dropped, and
its autorelease pool drained). AppKit's figures for them were taken with
the machine busy with other work, so they are rough.

| | Sidestep | AppKit |
|---|---:|---:|
| `setString:` | 1.1 ms | 2.4 ms |
| first viewport layout | 0.83 ms (41 fragments) | 1.8 ms (40 fragments) |
| scroll 40 points and lay out (p50 / p99) | 0.10 / 0.14 ms | 41 / 59 ms |
| jump to the middle and lay out | 0.99 ms | 99 ms |
| keystroke and lay out (p50 / p99) | 0.11 / 0.21 ms | 165 / 257 ms |
| `setFont:`, every fragment made | 121 ms | 203 ms |
| `setString:`, every fragment made | 83 ms | 550 ms |
| freeing the view, every fragment made | 20 ms | 30 s |

Only the viewport is laid out: the rest of the 200 000 paragraphs stay
estimates (the view is 3 000 015 points tall as estimated; 3 200 016 on
macOS), and nothing runs in the background. A keystroke lays out again the
edited paragraph and places the viewport's fragments. A layout manager's
fragments share one weak reference to it, as a content storage's elements
do. The runtime stores and destroys a weak reference in amortized constant
time however many an object has, but one each would still cost a weak
location apiece: with every fragment made, 21 to 22 MB more at peak, 2 to
2.7 times as long to free the view, and 10 to 30% more for the `setFont:`
and `setString:` rows (measured on Linux, in two sessions).
