# Architecture

Sidestep lets one Rust codebase written against [objc2] build for macOS and for
Linux. On macOS nothing changes: objc2 talks to Apple's runtime and frameworks.
On Linux, Sidestep supplies what objc2 expects to find there, written in Rust.

```
your app                          unchanged source; one `use sidestep as _;`
objc2-app-kit, objc2-foundation   unmodified, from crates.io
objc2, block2                     unmodified; GNUstep ABI selected on Linux
───────── C ABI: objc_msg_lookup, objc_allocateClassPair, objc_retain, …,
          class symbols ._OBJC_CLASS_<Name>, _Block_copy, _NSConcreteStackBlock
sidestep-runtime                  the Objective-C runtime, in Rust
sidestep-foundation, -appkit      framework classes, in Rust, via define_class!
```

[objc2]: https://github.com/madsmtm/objc2

## How a Linux build is wired

- **Features.** The `sidestep` crate depends on objc2, block2 and
  objc2-foundation with their `gnustep-2-1` features, only on non-Apple
  targets. Cargo unifies features across the graph, so the app's own objc2
  dependencies switch to the GNUstep ABI without the app naming a feature.
- **Linking.** In that mode objc2 links `-lobjc`, `-lgnustep-base` and
  `-lgnustep-gui`. `sidestep-runtime`'s build script writes empty archives
  with those names and adds their directory to the search path, which Cargo
  passes on to every dependent. The symbols themselves come from Sidestep's
  rlibs, so a Linux binary is a single static Rust executable with no libobjc.
- **Referencing.** An rlib is only linked if the program mentions it, hence
  `use sidestep as _;`. On macOS `sidestep` is an empty crate, so that line
  needs no `cfg`.

## Classes are static shells

In GNUstep 2.x mode objc2 names every framework class it uses by linker symbol,
`._OBJC_CLASS_NSString`, rather than looking it up at run time. So each class
an app can touch must exist at link time, and a class Sidestep has not written
yet is a link error, not a crash.

`sidestep_runtime::static_class!` declares such a symbol: a class and a
metaclass *shell* plus a loader function. The first time the runtime needs the
class (a message to it, `+alloc`, introspection), it runs the loader under a
reentrant lock. The loader forces an objc2 `define_class!` type with the same
name, whose registration calls `objc_allocateClassPair`; the runtime recognizes
the pending shell by name and hands it over instead of allocating a new class.

So Foundation and AppKit are written with objc2 itself. Method type encodings
come from the same Rust types objc2 checks messages against, and in debug
builds objc2 verifies every message send against them.

The rule this imposes on framework code: reach other classes through their
objc2 types (`objc2_foundation::NSString::alloc()`), never by calling an
implementation type's `class()` directly. Doing so would define the class
before the runtime asks, leaving the shell empty; the runtime panics with an
explanation if that happens.

`NSObject` has no superclass, so `define_class!` cannot build it. The runtime
builds it with objc2's `ClassBuilder::root`, which gives the same encoding
guarantees.

## Objects

Every object is preceded by a 16-byte header holding an atomic word: the retain
count above four flag bits (deallocating, weakly referenced, has associated
objects, immortal). Retain is a single `fetch_add`; release is a CAS loop that
sets the deallocating bit at zero and sends `-dealloc`. Weak references live in
a side table keyed by object, cleared when the object is freed; loads retain
through a CAS that fails once deallocation has begun.

Class objects and blocks have no header. The ARC entry points tell them apart
by their class's flags, which static shells carry from the start, so this works
even before a class has loaded.

## Message dispatch

`objc_msg_lookup(receiver, sel)` returns the implementation and the caller
calls it, the GNUstep convention objc2 already supports. No assembly
trampoline is involved, so the runtime is portable Rust.

The lookup is built to cost less than Apple's `objc_msgSend`. Each class
keeps its method cache in one word: a pointer to a table of slots with the
table's index mask in the top 16 bits. A cached message is 14 straight-line
instructions: load the receiver's class, load the word, load one slot,
compare, return. There are no locks, no memory barriers and no "is this
class initialized?" check, because a class only gets a table once
`+initialize` has run. Three things make the unlocked, relaxed reads sound
(`crates/sidestep-runtime/src/cache.rs` has the full argument):

- Tables live in fresh anonymous mappings that are never unmapped or reused,
  so a reader can only see zeros or values the runtime stored.
- A slot holds the implementation and its selector XOR the implementation.
  A slot seen half-written never matches a selector, so one compare checks
  both.
- Tables are replaced, never edited in place. Adding a method to a class in
  use, or changing an implementation, empties every class's cache.

Selectors are allocated side by side, 16-byte aligned, so selectors spread
evenly over a table's slots, and tables grow at half full, so most
selectors are found in the first slot they probe. On an M-series Mac, a
message costs about 1.1 ns on Sidestep (under Linux in a VM) against 1.25 ns
on Apple's runtime; `examples/msgbench` measures it. With link-time
optimization (`lto = "fat"` in the release profile) the lookup inlines into
every call site, which `objc_msgSend` never can: 0.9 ns.

Unknown selectors go through `+resolveInstanceMethod:` /
`+resolveClassMethod:` and then panic with the message Apple's runtime raises,
`-[Class selector]: unrecognized selector sent to instance 0x…`; the panic
unwinds into the Rust caller.

`+initialize` is sent lazily before a class's first message, superclasses
first, with messages from inside `+initialize` on the same thread allowed
through.

## AppKit: a main thread and a render thread

AppKit's contract is single-threaded: events, timers, the responder chain and
`drawRect:` run on the main thread. Sidestep keeps that contract and moves
everything that touches pixels or the display server to a render thread.

- **Drawing records.** Inside `drawRect:`, `-[NSColor setFill]`,
  `+[NSBezierPath fillRect:]`, `-[NSBezierPath fill]` and
  `-[NSString drawAtPoint:withAttributes:]` append operations (fills, paths,
  runs of text) to a list, already mapped to the view's layer and clipped to
  the view and its ancestors. The main thread never rasterizes.
- **Layers.** A window's own surface is one layer, and each `NSClipView` adds
  one holding its document. `setNeedsDisplayInRect:` records damage per
  layer, in layer points.
- **Display.** Once the render thread reports the last frame shown, the
  window places its scroll layers, calls `drawRect:` only for damaged areas,
  sends the operations and presents. A window the compositor isn't showing
  gets no frame callbacks, so it stops drawing.
- **Rendering.** The render thread owns the Wayland connection through
  smithay-client-toolkit. It rasterizes operations on the CPU into a cache
  per layer (tiny-skia for paths, fontdue for glyphs), only inside damaged
  rectangles. The window surface is presented from a few shared-memory
  buffers, each remembering what changed since it was last written, so a
  frame copies and damages only changed pixels.
- **Scrolling.** A document layer is cut into 512-point-tall tiles, each on
  its own subsurface and cropped to the clip view with wp_viewporter.
  Scrolling moves tiles and changes crops. A tile is drawn and uploaded when
  it comes within a tile of the viewport or its content changes, and dropped
  when it is two tiles away.

This design keeps GPU wake-ups and uploads proportional to what changed,
which is what dominates power on a mostly idle desktop. A GPU rasterizer can
replace the CPU one behind the same operations later.

### Scale

Operations stay in points. Each window renders at its output's scale: the
fractional one from wp_fractional_scale_v1 where the compositor offers it,
else the integer one (the surface's preferred buffer scale, or its
outputs'). Every surface, tiles and decorations included, gets a buffer of
`points × scale` pixels (rounded as the fractional-scale protocol asks) and
a wp_viewporter destination of its size in points, so the compositor maps
buffer pixels to screen pixels one to one instead of resampling. A canvas
has a `scale`; fills and damage cover the pixels whose centers they
contain, so neighboring fills share edges without gaps or overlaps at any
scale, and a pixel's value never depends on how damage was cut up. A scale
change drops every tile and redraws the window; `backingScaleFactor` and
the `convert…ToBacking:` methods report it.

### Input

The render thread binds each wl_seat itself (up to version 9, for
high-resolution wheels) and turns its events into messages for the main
thread, which makes `NSEvent`s and sends them through
`-[NSApplication sendEvent:]` to the window.

- **Keys.** Keymaps are compiled with [kbvm], a pure Rust XKB
  implementation, so neither building nor running needs libxkbcommon.
  `characters` apply every modifier (Control-A types U+0001, as on macOS)
  and the locale's compose sequences and dead keys;
  `charactersIgnoringModifiers` apply Shift only. Keys that type nothing
  get AppKit's function-key characters (`NSUpArrowFunctionKey`, …) and
  control characters (Return is `\r`, Backspace `NSDeleteCharacter`).
  Option is Alt and Command is the Super (logo) key. Modifier keys make
  flags-changed events rather than key events. Keys repeat on the render
  thread at the compositor's rate and delay, and arrive as key-downs with
  `isARepeat`. `keyCode` is the XKB keycode (evdev plus 8): Apple's virtual
  key codes come only from Apple's headers.
- **Routing.** Command-key presses are offered as key equivalents first
  (`performKeyEquivalent:` down the view tree, depth first); keys and
  modifier changes then go to the first responder, which is the window
  itself until a view takes over, and up the responder chain from there.
  The window with Wayland's keyboard focus is the key (and main) window,
  gets `becomeKeyWindow` and tells its delegate; the application is active
  while one of its windows is key. Without a keyboard, the window the
  compositor calls activated is key. `interpretKeyEvents:` applies AppKit's
  standard key bindings, learned by a conformance test on a Mac: text goes
  to `insertText:`, editing keys become commands (`moveLeft:`,
  `deleteBackward:`, the Emacs-style Control keys) sent with
  `doCommandBySelector:`, which performs them where a responder up the
  chain has them.
- **Pointer.** A pointer frame may leave one of a window's surfaces and
  enter another (a tile, the root), so the render thread settles crossings
  at the end of each frame and tells the main thread when the pointer
  enters or leaves a window's content. It counts clicks (400 ms and 5
  points apart at most, GTK's defaults, which GNOME keeps) with the
  compositor's timestamps. Buttons map to AppKit's numbers: middle is 2,
  back 3 and forward 4. Touchpads scroll by precise point deltas; wheels by
  lines (3 a detent, fractions from high-resolution wheels).
- **Cursors.** A wp_cursor_shape_v1 shape where the compositor offers the
  protocol, else an image from the cursor theme (`XCURSOR_THEME`,
  `XCURSOR_SIZE`) at the window's scale. `NSCursor`'s standard cursors map
  to shapes; one that's set shows over every window's content (the
  decorations keep their resize cursors), `push` and `pop` stack them, and
  `setHiddenUntilMouseMoves:` hides the pointer until it moves.

[kbvm]: https://github.com/mahkoh/kbvm

### Windows and decorations

A window's size, maximized and fullscreen states and keyboard focus belong
to the compositor: `zoom:`, `toggleFullScreen:`, `miniaturize:`,
`setContentSize:` and `makeKeyWindow` (through xdg-activation) ask, and the
window follows the compositor's configures, telling its delegate. Size
limits go to the compositor in window-geometry terms. Wayland doesn't say
where windows are, so `frame` keeps the origin a program gives it. A
borderless child window (`addChildWindow:ordered:`) becomes an xdg_popup
placed where its frame is relative to its parent's, which is how tooltips
and completion lists are made; `show_as_popup` opens one below an anchor
with an input grab, the start of menus.

Where the compositor leaves decorations to the client (GNOME's does; a
compositor without xdg-decoration, or one answering with client mode),
the render thread draws them: an Adwaita-like header bar with the title and
close, maximize and minimize buttons (as the style mask and the
compositor's capabilities allow), a shadow and a resize border. Each part
is a subsurface outside the window's own surface, so the content view's
coordinates don't change; the window geometry takes in the header but not
the shadow, and the parts are synchronized subsurfaces, so a resize shows
atomically. Dragging the header moves the window, double-clicking it
maximizes, the right button opens the compositor's window menu, and the
border resizes with the matching cursor; maximized, tiled and fullscreen
windows lose the border and rounded corners, and fullscreen ones the header
too. They're drawn with tiny-skia rather than by a crate such as
sctk-adwaita so they render at fractional scales like everything else, the
title is set with the same text drawing as the content (the main thread
sends it as operations), and nothing runs a subprocess to find a font.
`SIDESTEP_DECORATIONS=client` draws them on compositors that would draw
their own (sway), and `SIDESTEP_THEME=dark` (or a dark `GTK_THEME`) picks
the dark variant.

### The clipboard

The general `NSPasteboard` is the Wayland selection. Writes never wait: the
main thread keeps what it wrote and the render thread offers a copy,
serving other clients' reads a pipe-buffer at a time from its event loop,
with a marker type so it never reads its own offer back. Reading can't be
synchronous on Wayland, so the render thread reads another client's text
ahead, when the selection changes hands, and `stringForType:` answers from
that at once. For other types, or text still on its way, it waits for at
most 200 ms. A counter both threads share plays `changeCount`. Pasteboards
made by name live in the process only.

## Conformance

`conformance/` holds tests written only against objc2. They run on macOS
against Apple's runtime and on Linux against Sidestep's; a test that passes on
one and fails on the other is a Sidestep bug, or a test relying on something
unspecified (Apple's tagged-pointer strings made two equal strings the same
object, for example). These are ordinary programs against public APIs, which
is also the only kind of observation of Apple's behavior the project allows
(see [legal.md](legal.md) and CONTRIBUTING).

## Linux development loop

`scripts/linux-cargo` runs cargo in a Linux container with clippy and rustfmt
(`scripts/linux.Dockerfile`), keeping the registry and target directory in
named volumes:

```sh
scripts/linux-cargo test --workspace
scripts/linux-cargo run -p hello
```

On macOS, plain `cargo test --workspace` runs the same conformance tests
against Apple's runtime. `conformance/tests/appkit.rs` checks AppKit view
geometry the same way, without opening a window.

To see an AppKit program on Linux without a display, `scripts/headless-wayland`
runs it under a headless sway and can take a screenshot:

```sh
scripts/linux-cargo build -p appkit-slice
SCENARIO=scroll SHOT=/work/target/scroll.png scripts/linux-run scripts/headless-wayland /target/debug/appkit-slice
```
