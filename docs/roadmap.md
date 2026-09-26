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
`NSDictionary` (faster than Apple's on the same Mac, see
`examples/dictbench`), and the system services: `NSRunLoop` and
CFRunLoop with modes, timers, observers, sources and cross-thread
handoff; `NSTimer`, `NSDate`, `NSNotification` and
`NSNotificationCenter`; `NSThread` and the `performSelector…` family;
libdispatch (queues, groups, semaphores, `after`, timer, data and vnode
sources) and `NSOperationQueue`; `NSData`, `NSURL`, `NSURLComponents`,
`NSError`, `NSFileManager` over XDG directories, `NSBundle`,
`NSProcessInfo`, `NSUUID`, the runtime lookup functions,
`NSUserDefaults`, property lists, `NSJSONSerialization`, the locks,
`NSDateFormatter` with `NSLocale` and `NSTimeZone`, and toll-free
CoreFoundation for strings, data, dates, errors, URLs, dictionaries and
preferences. `examples/servicebench` measures the run loop and the
notification center.

- `NSMutableString`; `-description` on NSObject before Foundation's
  `NSString` has loaded.
- Collections: `NSArray`, `NSMutableArray`, `NSMutableDictionary`, `NSSet`,
  `NSNumber`, `NSValue`; then turn on sidestep-foundation's `collections`
  feature, which lights up the services' array- and number-returning parts.
- `NSAutoreleasePool`; `NSCalendar`, `NSDateComponents` and
  `NSNumberFormatter`; locale data beyond English.
- `NSOperation` dependencies; `NSStream`; `NSURLSession`.
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

Next:

- Keyboard input and text input methods; target/action.
- `NSGraphicsContext`, strokes, curves, images, transforms.
- Scrollers, resizing by the user, several windows, HiDPI.
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
