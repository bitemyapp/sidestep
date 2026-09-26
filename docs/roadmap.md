# Roadmap

Each milestone ends with conformance tests that pass on macOS and Linux.

## 0. Runtime (done)

Objective-C runtime with libobjc2's C ABI, blocks runtime, `NSObject`,
`NSString`, `NSThread`, and 20 conformance tests: classes and subclasses,
ivars and `Drop`, `super`, autorelease pools, weak references under thread
contention, protocols, introspection (including before a class's first
message), blocks, strings. Passing on macOS, and on Linux aarch64 and x86_64.

## 1. Foundation core

Done so far: constant strings from static memory, an immutable
`NSDictionary` (faster than Apple's on the same Mac, see `examples/dictbench`), `NSTimer` with blocks, a timer-only `NSRunLoop`, and
`NSNotification` for delegate callbacks.

- `NSMutableString`; `-description` on NSObject before Foundation's
  `NSString` has loaded.
- Collections: `NSArray`, `NSMutableArray`, `NSMutableDictionary`, `NSSet`.
- `NSNumber`, `NSValue`, `NSData`, `NSDate`, `NSError`, `NSURL`,
  `NSProcessInfo`.
- `NSNotificationCenter`, the rest of `NSRunLoop`, `NSAutoreleasePool`, and
  the rest of `NSThread`.
- Every static class findable by name, not only after first use.
- Message forwarding.

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
  (`interpretKeyEvents:`, `insertText:`, `doCommandBySelector:`), key window
  focus and application activation.
- Pointer: enter and leave, all buttons, click counts, horizontal and
  high-resolution scrolling, `NSCursor`'s standard cursors as cursor shapes
  with a themed fallback.
- Windows: several at once, client-side decorations where the compositor
  wants them (GNOME), resizing by the user, size limits, zoom, full screen,
  miniaturize, borderless child windows as popups, activation.
- HiDPI at integer and fractional scales.
- The general `NSPasteboard` as the Wayland clipboard, for strings.

Next:

- `keyCode`: Apple's virtual key codes come only from Apple's headers, so
  Sidestep reports XKB keycodes; a clean-room table (for example measured
  with a program on a Mac) needs a decision.
- Text input methods (zwp_text_input_v3, `NSTextInputClient`, marked text);
  target/action.
- `NSTrackingArea` and cursor rectangles, image cursors, drag and drop,
  `NSScreen`.
- `NSGraphicsContext`, strokes, curves, images, transforms.
- Scrollers.
- Text shaping and fallback with parley and fontique.
- `NSMenu`, which has no system-wide equivalent on Linux, so it becomes a
  menu bar inside the window.
- X11, after Wayland is solid.

## 3. Text

`NSAttributedString`, `NSTextField`, `NSTextView`, and the TextKit 1 subset
(`NSLayoutManager`, `NSTextStorage`, `NSTextTable`) that real apps lean on.

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
