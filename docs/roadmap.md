# Roadmap

Each milestone ends with conformance tests that pass on macOS and Linux.

## 0. Runtime (done)

Objective-C runtime with libobjc2's C ABI, blocks runtime, `NSObject`,
`NSString`, `NSThread`, and 20 conformance tests: classes and subclasses,
ivars and `Drop`, `super`, autorelease pools, weak references under thread
contention, protocols, introspection (including before a class's first
message), blocks, strings. Passing on macOS, and on Linux aarch64 and x86_64.

Since then: every framework class findable by name before its first use,
message forwarding through `-forwardingTargetForSelector:`, the autorelease
return-value handoff, declared properties, methods implemented by blocks,
`objc_msgSend` for direct callers, every association policy, and side
tables (weak references, associated objects, `@synchronized`) sharded so
threads don't contend. Allocating and freeing an object costs about 4 ns
beyond `malloc` and `free`.

Then: categories linked into the program (`category!`), attached before a
class's first use; `NSMethodSignature`, `NSInvocation` and forwarding
through `-forwardInvocation:`, with calls laid out for the aarch64 and
x86_64 calling conventions; `NSProxy`; `objc_msgSend` probing the method
cache in assembly (0.94 ns, against Apple's 1.17); `+load` for framework
classes; methods made from blocks without writable-then-executable
memory.

- Categories' own `+load`; `long double` returns through `NSInvocation` on
  x86_64 (the x87 stack).

## 1. Foundation core

Done so far: constant strings from static memory, an immutable
`NSDictionary` (faster than Apple's on the same Mac, see
`examples/dictbench`), and the system services: `NSRunLoop` and
CFRunLoop with modes, timers, observers, sources and cross-thread
handoff; `NSTimer`, `NSDate`, `NSNotification` and
`NSNotificationCenter`; `NSThread` and the `performSelector…` family;
libdispatch (queues, groups, semaphores, `after`, timer, data and vnode
sources) and `NSOperationQueue` with dependencies; `NSData`, `NSURL`, `NSURLComponents`,
`NSError`, `NSFileManager` over XDG directories, `NSBundle`,
`NSProcessInfo`, `NSUUID`, the runtime lookup functions,
`NSUserDefaults`, property lists, `NSJSONSerialization`, the locks,
`NSDateFormatter` with `NSLocale` and `NSTimeZone`, and toll-free
CoreFoundation for strings, data, dates, errors, URLs, dictionaries and
preferences. `examples/servicebench` measures the run loop and the
notification center.

Collections and values: `NSArray`, `NSMutableArray`, `NSMutableDictionary`,
`NSSet`, `NSMutableSet`, `NSIndexSet`, `NSMutableIndexSet`, `NSEnumerator`
and fast enumeration with mutation detection, `NSNumber`, `NSValue` and
`NSNull`, with Foundation's descriptions, copy-on-write copies, and its
failures (out-of-range indexes, nil elements) as panics carrying its
messages. Mutable collections may be read from several threads at once, as
in Foundation; mutable arrays change at either end in constant time, so
they work as queues; app subclasses that implement only the primitive
methods get the rest. Faster than Apple's on the same Mac in nearly every
operation `examples/arraybench` and `dictbench` measure; creating a small
`NSNumber` is the exception (Apple's are tagged pointers).

Strings: `NSString` and `NSMutableString` with encodings, comparison,
search, case mapping, normalization, lines and enumeration, paths and
numbers; `NSAttributedString` and `NSMutableAttributedString`;
`NSCharacterSet`, `NSScanner`, `NSRegularExpression` and
`NSTextCheckingResult`; the `NSRange` and `NSGeometry` functions (see
`examples/strbench`).

- Strings, still to do: initializers and writers for files, URLs and
  `NSData`; encodings beyond ASCII, Latin-1, Windows-1252, Mac Roman and
  the UTF forms; locale tailoring for case mapping and collation, and
  `NSLocale` arguments generally; dictionary-based word breaks for CJK and
  Thai; in regular expressions, character names (`\N{…}`), `\G`, full case
  folding and exact hit-end flags; attributed string drawing (with the text
  engine).
- `-description` on NSObject before Foundation's `NSString` has loaded.
- Collections: `NSOrderedSet`, `NSCountedSet`, `NSHashTable`, `NSMapTable`,
  key-value coding on collections, `NSCoding`; class factory methods
  (`+array`, `+dictionary`) that return the receiving subclass.
- `NSAutoreleasePool`; `NSCalendar`, `NSDateComponents` and
  `NSNumberFormatter`; locale data beyond English.
- `NSStream`; `NSURLSession`; `dispatch_io`, `dispatch_data` and
  dispatch blocks (`dispatch_block_create`).
- Remove sidestep-foundation's `collections` feature gates (the feature is
  on by default now that the collections exist).

## 2. AppKit skeleton

The first slice runs `examples/appkit-slice` unchanged on Wayland: one
window, a flipped view drawing text with `drawRect:`, mouse clicks moving a
blinking caret, a spinner at 60 fps, and a 2000-line list in an
`NSScrollView`. It has `NSApplication` and its delegate, `NSWindow`,
`NSView` (frames, bounds, flipping, conversion, hit testing, autoresizing),
`NSClipView`, `NSScrollView`, mouse and scroll `NSEvent`s through the
responder chain, `NSColor`, `NSFont`, `NSBezierPath` fills and string
drawing. The design is in [architecture.md](architecture.md).

The platform layer since (`examples/appkit-input` shows it):

- Keyboard: keymaps compiled in pure Rust (kbvm), compose and dead keys,
  key repeat, key-down, key-up and flags-changed events with characters
  and modifier flags, key equivalents, AppKit's standard key bindings
  (`interpretKeyEvents:`, `insertText:`, `doCommandBySelector:`; a table of
  AppKit's answers for every key of a US keyboard with every combination of
  modifiers holds both platforms to the same bindings), key and main window
  focus and application activation, input methods (zwp_text_input_v3 into
  `NSTextInputContext` and `NSTextInputClient`: marked text, committed
  text, the caret rectangle), dead keys shown as marked text.
- Pointer: enter and leave, all buttons, click counts, horizontal and
  high-resolution scrolling (Shift turns a wheel sideways, natural
  scrolling is reported), `NSCursor`'s standard cursors as cursor shapes
  with a themed fallback, `NSTrackingArea`s and cursor rectangles,
  touchpad scroll phases and momentum paced by frames, pinch to magnify and
  rotate.
- Windows: several at once, client-side decorations where the compositor
  wants them (GNOME), light or dark as the desktop prefers, resizing by the
  user, size limits, zoom, full screen, miniaturize, occlusion, borderless
  child windows as popups, activation, window settings (background color,
  click-through, movable, title visibility, initial first responder),
  dragging a window by its content.
- HiDPI at integer and fractional scales.
- Nested event loops (`nextEventMatchingMask:…`, posted events), modal
  loops (`runModalForWindow:`), local event monitors; target/action through
  the responder chain.
- The general `NSPasteboard` as the Wayland clipboard, bounded so a client
  that never answers can't stall the main thread more than once a copy:
  `NSPasteboardItem`s, any type (text, HTML, images, URL lists as an item
  per file, types of Sidestep's own), the old type names,
  `declareTypes:owner:` and data providers asked on demand (by other
  programs too, without a new selection), `writeObjects:` and
  `readObjectsForClasses:options:` for items, strings, URLs and the
  program's own classes, type conformance (in `availableTypeFromArray:`
  too), `canReadObjectForClasses:options:` from the types alone.
  Pasteboards made by name stay in the process, so they don't pay for a
  pasteboard server as Apple's do: a copy and paste costs 0.1 µs against
  100 µs (`conformance/tests/pasteboard.rs` times them).
- Drag and drop into windows: `registerForDraggedTypes:` (files and links
  told apart), the destination messages (periodic updates too) on views
  and windows, safe against nested event loops, the dragging info, its
  dragging items and the drag pasteboard over wl_data_device.
- `NSScreen`: the outputs, their frames, work areas and scales, a window's
  screen, and the delegate calls when they change.

Next:

- Input methods: surrounding text (and so deleting around the caret),
  content types from the client.
- Image cursors; dragging from our windows (drag sources), and `NSImage`
  and `NSColor` on pasteboards.
  (`-[NSView trackingAreas]` answers once Foundation has `NSArray`.)
- `NSGraphicsContext`, strokes, curves, images, transforms.
- Scrollers.
- `NSMenu`, which has no system-wide equivalent on Linux, so it becomes a
  menu bar inside the window.
- X11, after Wayland is solid.

## 3. Text

Done so far: fonts from the system's fontconfig with fallback, shaping,
bidi, line breaking and color emoji (parley, fontique, swash); `NSFont`
with weights, names, metrics, text styles and descriptors
(`NSFontDescriptor` with symbolic traits, system designs and feature
settings); `NSParagraphStyle` and `NSMutableParagraphStyle`; string
drawing and measuring (`drawAtPoint:`, `drawInRect:`,
`drawWithRect:options:`, `sizeWithAttributes:`,
`boundingRectWithSize:options:`) with fonts, colors, backgrounds,
paragraph styles, kerning, underlines and strikethroughs (with dot and
dash patterns, and by word), baseline offsets, ligatures, strokes,
obliqueness, shadows (without blur) and tab stops (`NSTextTab`, read back
with `tabStops`, laid out in the order set as on macOS); all attribute
name constants. The line layout TextKit will stand on: lines, clusters
and carets with UTF-16 ranges and directions, a paragraph or a few lines
at a time on any thread, hit testing, caret and selection geometry, and
relayout of only what an edit touched. See
[architecture.md](architecture.md#text).

- `NSAttributedString` drawing and measuring: `string_drawing` turns
  attribute dictionaries over UTF-16 ranges into the layout's runs
  (`attribute_spans`, `lines::runs_of`); the category methods wait for
  Foundation's class.
- `NSExpansion` (advances scaled before line breaking); shadow blur (a
  blurred glyph op); tabs in right-to-left paragraphs, measured from the
  right; descriptors' `fontAttributes` with numbers (`NSNumber`);
  `NSStringDrawingContext`.
- Bidi: clusters give their direction, not the bidi level, for numbers in
  right-to-left text of a left-to-right paragraph (level 2, shown as 0:
  parley keeps levels to itself); explicit embeddings and isolates open
  where a paragraph is laid out from a line aren't carried into it;
  deleting in a paragraph that mixes directions and has brackets lays it
  out whole (tracking bracket pairs across the edit would keep more).
- Two spaces where a line wraps: parley hangs the first and starts the
  next line with the second, where AppKit hangs both.
- The desktop's own interface font where fontconfig's `system-ui` doesn't
  name it (GNOME keeps it in GSettings); dictionary line breaking for Thai,
  Lao, Khmer and Myanmar (parley's `complex-scripts`, several megabytes of
  data).
- Vertical text, hyphenation, `allowsDefaultTighteningForTruncation`.
- `NSTextField`, `NSTextView`, and the TextKit 1 subset (`NSLayoutManager`,
  `NSTextStorage`, `NSTextTable`) that real apps lean on.

## 4. Controls and services

Scroll, split, popup and segmented views; open panels through
xdg-desktop-portal; appearance (dark mode); accessibility through
AccessKit.

## 5. A real app

Omperor, the motivating application, building for Linux with no source
changes beyond `use sidestep as _;`.

## Swift

Research, after Rust works. Swift on Linux is normally built without
Objective-C interop. The questions are whether `-enable-objc-interop` can
target a runtime with this ABI, and what Clang-emitted class structures (the
libobjc2 v2 ABI's `__objc_load` path) the runtime would then need to accept.

## Standing work

- Raise the `NSStringEncoding` width mismatch with objc2 (see
  [abi.md](abi.md)).
- Keep the fallback declarations crate from [legal.md](legal.md) ready:
  prototype the Cargo mechanics early.
- CI on Linux x86_64 and aarch64 and on macOS.
