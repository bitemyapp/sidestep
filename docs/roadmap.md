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
- `-forwardInvocation:`, once `NSInvocation` exists (the runtime forwards
  through `-forwardingTargetForSelector:` already).

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
- The general `NSPasteboard` as the Wayland clipboard, for strings (other
  types wait for `NSData` and `NSArray`), bounded so a client that never
  answers can't stall the main thread more than once, with the old type
  names.

Next:

- Input methods: surrounding text (and so deleting around the caret),
  content types from the client.
- Image cursors, drag and drop, `NSScreen`. (`-[NSView trackingAreas]`
  answers once Foundation has `NSArray`.)
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
paragraph styles, kerning, underlines, strikethroughs, baseline offsets,
ligatures and tab stops (`NSTextTab`); all attribute name constants. See
[architecture.md](architecture.md#text).

- `NSAttributedString` drawing and measuring, as an adapter over the
  layout's attribute runs, once Foundation has the class.
- String drawing before any AppKit class has loaded: `NSString` gets its
  drawing methods when `NSColor`, `NSFont`, `NSParagraphStyle` or a window
  first loads, so `sizeWithAttributes:nil` as a program's very first AppKit
  call finds no method. Foundation's `NSString` loader needs a hook that
  AppKit can register (see the text workstream's notes).
- Reading tab stops back (`tabStops`, an `NSArray`); descriptors'
  `fontAttributes` with numbers (`NSNumber`); `NSStringDrawingContext`.
- The desktop's own interface font where fontconfig's `system-ui` doesn't
  name it (GNOME keeps it in GSettings); dictionary line breaking for Thai,
  Lao, Khmer and Myanmar (parley's `complex-scripts`, several megabytes of
  data).
- Vertical text, hyphenation, `allowsDefaultTighteningForTruncation`,
  stroke and shadow attributes.
- `NSTextField`, `NSTextView`, and the TextKit 1 subset (`NSLayoutManager`,
  `NSTextStorage`, `NSTextTable`) that real apps lean on.

## 4. Controls and services

Scroll, split, popup and segmented views; pasteboard and drag and drop; open
panels through xdg-desktop-portal; appearance (dark mode); accessibility
through AccessKit.

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
