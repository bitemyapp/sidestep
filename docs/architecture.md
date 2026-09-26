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
  runs of shaped glyphs) to a list, already mapped to the view's layer and
  clipped to the view and its ancestors. The main thread never rasterizes.
- **Layers.** A window's own surface is one layer, and each `NSClipView` adds
  one holding its document. `setNeedsDisplayInRect:` records damage per
  layer, in layer pixels.
- **Display.** Once the render thread reports the last frame shown, the
  window places its scroll layers, calls `drawRect:` only for damaged areas,
  sends the operations and presents. A window the compositor isn't showing
  gets no frame callbacks, so it stops drawing.
- **Rendering.** The render thread owns the Wayland connection through
  smithay-client-toolkit. It rasterizes operations on the CPU into a cache
  per layer (tiny-skia for paths, swash for glyphs), only inside damaged
  rectangles. The window surface is presented from a few shared-memory
  buffers, each remembering what changed since it was last written, so a
  frame copies and damages only changed pixels.
- **Scrolling.** A document layer is cut into 512-pixel-tall tiles, each on
  its own subsurface and cropped to the clip view with wp_viewporter.
  Scrolling moves tiles and changes crops. A tile is drawn and uploaded when
  it comes within a tile of the viewport or its content changes, and dropped
  when it is two tiles away.

This design keeps GPU wake-ups and uploads proportional to what changed,
which is what dominates power on a mostly idle desktop. A GPU rasterizer can
replace the CPU one behind the same operations later.

## Text

AppKit measures text synchronously (`sizeWithAttributes:`,
`boundingRectWithSize:options:attributes:context:`), so text is shaped and
laid out on the thread that asks, and the render thread only ever sees
glyphs: a glyph-run op names a registered face, a size, and glyph ids with
positions in points. `crates/sidestep-appkit/src/text/` holds the stack.

- **Fonts.** fontique finds the system's fonts through fontconfig, loaded
  at run time with `dlopen` (every Linux desktop has `libfontconfig.so.1`;
  nothing is linked at build time). So the desktop's configuration decides
  what `system-ui`, `sans-serif` and `monospace` are and which families
  fill in for missing glyphs, as it does for GTK and Qt programs. The system
  font is `system-ui` (else `sans-serif`), the monospaced one `monospace`.
  Without fontconfig, the usual font directories are scanned and
  well-known families stand in. Opening the collection reads fontconfig's
  cache and takes some milliseconds (13 ms with 300 fonts), so it starts
  on a background thread as soon as `NSApplication` loads.
- **NSFont.** A font is a spec (family or system design, weight, italic,
  width, size) resolved to a face, whose metrics and names come from the
  font file through skrifa. `NSFontWeight` values map through the named
  weights to CSS weights. Like browsers, and unlike parley's default, a
  face is emboldened only when bold is asked of a family without one:
  Medium in a family with Regular and Bold is Regular. `fontWithName:`
  finds families, PostScript names (`DejaVuSans-Bold`) and full names, and
  maps Apple's own families (Menlo, SF Mono, Helvetica, Times, …) to the
  system designs, so programs written for macOS find a font. Descriptors
  take family, name, size, traits and feature settings (by Apple's feature
  registry numbers or OpenType tags) from attribute dictionaries.
- **Layout.** parley does bidi, line breaking (ICU4X, compiled in),
  shaping (harfrust) and fallback per script; `text/layout.rs` places the
  lines as AppKit's string drawing does, from measurements of Apple's (the
  conformance tests in `conformance/tests/text.rs` check them on both
  platforms): a line is as tall as its fonts' ascents and descents, each
  rounded to a whole point, plus their leading only with
  `usesFontLeading`; `lineHeightMultiple`, minimum and maximum apply next,
  extra height going above the text; `lineSpacing` goes between lines and
  paragraph spacing between paragraphs, never outside the text. Word
  wrapping breaks inside a word that doesn't fit a line alone; clipping and
  truncation don't wrap. Widths count trailing spaces, and count indents
  only for text that wraps. `drawInRect:` clips lines that don't fit rather
  than dropping them; `boundingRectWithSize:` drops them, and truncates the
  last one with `truncatesLastVisibleLine`. Emoji are shaped from the color
  emoji family first, as on macOS, where the text's own face might
  otherwise give them plain glyphs. Tabs go to the paragraph's tab stops
  (left, right, centered or decimal; by default twelve, 28 points apart),
  then every
  `defaultTabInterval`; control characters take no room.
- **Caches.** Laid-out text is cached per thread by string, attributes and
  options (two generations of 2048 entries or 4 MB), so a view that
  redraws the same lines records them for about 0.1 µs a line. Glyph runs
  keep their glyphs in an `Arc`, so recording a cached line copies no
  glyphs.
- **Parallel layout.** Drawing doesn't need the layout until the pass
  ends, so text drawn without having been measured is set aside, and the
  pass lays it all out at its end on a small pool of worker threads, the
  main thread taking a share, and splices the ops in where they were
  drawn. A new page of text lays out several times faster than one line
  after another; measuring (`sizeWithAttributes:`) stays synchronous.
- **Rasterizing.** The render thread rasterizes each glyph once per face,
  pixel size and quarter-pixel horizontal offset with swash (no hinting,
  as macOS draws), and composites coverage masks and premultiplied color
  images (COLR layers and CBDT or sbix bitmaps, such as Noto Color Emoji)
  from the cache. Baselines sit on whole pixels.

parley was chosen over cosmic-text, the other complete pure-Rust stack.
parley takes styles as ranges over the text, which is what an attributed
string's runs are; it lets each line have its own width and indent and
aligns and justifies lines itself, which paragraph styles need; and
fontique asks fontconfig for families, aliases and fallback, where
cosmic-text's fontdb scans directories and falls back through lists of its
own. cosmic-text is organized around editable buffers with one line height
per buffer, which fits a text editor better than AppKit's paragraph model.
Both shape with harfrust (HarfBuzz ported to Rust) and can rasterize with
swash.

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
