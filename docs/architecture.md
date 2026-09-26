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

## Strings

Strings are stored as WTF-8: UTF-8 that can also hold the lone surrogates
UTF-16 allows, with a surrogate pair always joined into one 4-byte
character. An immutable string is one allocation holding a header (UTF-8
and UTF-16 lengths, an all-ASCII flag, a has-lone-surrogate flag, the cached
hash, an index) and the bytes with a trailing NUL, so `-UTF8String` and
`Display` are free. ASCII strings index by byte. Otherwise the index is a
cursor plus a crumb every 64 UTF-16 units, built lazily and published with
atomics, so sequential `characterAtIndex:` is about as fast as Apple's and a
random one walks at most 64 units. `NSMutableString` keeps the same form in
a buffer whose crumbs are cut back at each edit. `+[NSString alloc]` returns
a static placeholder whose initializers build the final object, as Apple's
class cluster does; subclasses get ordinary instances.

`NSString` is the one class registered by hand (`objc_allocateClassPair`,
methods copied from helper classes, then `objc_registerClassPair`), so
another thread can never see it with some methods missing.

Non-literal comparison and search fold both sides (NFD, then case, diacritic
and width folding as asked) and match on composed character sequence
boundaries: ICU4X graphemes, adjusted the way Foundation's sequences are.
The text is folded a piece at a time as a search or comparison walks it,
and what it has passed is dropped, so an early hit or an early difference
costs only what comes before it, and a walk over the whole text needs
little memory. ASCII runs skip the segmenter and the normalizer, and an
ASCII needle is found with memchr through stretches that hold none of the
handful of characters that fold to ASCII. A backwards search folds ever
longer stretches from the end. Edits that replace many matches build the
new text in one pass. Unicode data comes from ICU4X (normalization, case
mapping, collation, properties, word and sentence segmentation), regular
expressions from fancy-regex after translating ICU syntax (line
terminators, inline flags, POSIX classes, escapes). A search range acts as
ICU's region, and string searches keep their last compiled patterns per
thread.

An attributed string is its text (a live `NSMutableString` subclass whose
edits come back to the owner) and a vector of runs, each a range and an
attribute dictionary. Runs merge only when their dictionaries are the same
object, as Apple's do, and every run without attributes shares one empty
dictionary. Edits rebuild the touched runs and their neighbours in one
splice. Receivers that are not Sidestep's own are driven through the
primitive methods, so subclasses behave as on macOS; whether a subclass
keeps Sidestep's storage is asked of the method cache, without a lock, on
each call.

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
  layer, in layer pixels.
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
- **Scrolling.** A document layer is cut into 512-pixel-tall tiles, each on
  its own subsurface and cropped to the clip view with wp_viewporter.
  Scrolling moves tiles and changes crops. A tile is drawn and uploaded when
  it comes within a tile of the viewport or its content changes, and dropped
  when it is two tiles away.

This design keeps GPU wake-ups and uploads proportional to what changed,
which is what dominates power on a mostly idle desktop. A GPU rasterizer can
replace the CPU one behind the same operations later.

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
