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

**Finding classes by name.** `static_class!` also puts a pointer to each
shell in a linker section, `sidestep_classes`. The linker concatenates the
section from every object it links and defines `__start_sidestep_classes`
and `__stop_sidestep_classes` around it, so the runtime reads the list of
every framework class without a constructor. The name registry starts out
holding all of them, loaded or not, and `objc_getClass`, `objc_lookUpClass`
and `objc_getRequiredClass` load a shell when they find it;
`objc_copyClassList` and `objc_getClassList` list shells as they are, and a
shell loads as soon as anything reads more than its name. A class nobody has
used also keeps its name: `objc_allocateClassPair` refuses it.

A class the program never names still ends up in the list: rustc links into
every executable and shared library an object that refers to every symbol an
upstream crate exports under a fixed name, and each shell is exported as
`._OBJC_CLASS_<Name>`. Linking a framework crate at all, which
`use sidestep as _;` does, therefore links all of its shells and their
entries. `objc_getClass("NSTimer")`, which `NSClassFromString` rests on,
works in a program that never mentions `NSTimer`.

## Objects

Every object is preceded by a 16-byte header holding an atomic word: the retain
count above five flag bits (deallocating, weakly referenced, has associated
objects, immortal, locked with `@synchronized`). Retain is a single
`fetch_add`; release is a CAS loop that sets the deallocating bit at zero and
sends `-dealloc`. Before either, `objc_retain` and `objc_release` ask one
question in two relaxed loads, the object's class and its flags: is this an
ordinary counted object whose class doesn't override `retain` or `release`?
Class objects and blocks have no header; their classes' flags say so, and
static shells carry those flags from the start, so this works even before a
class has loaded.

Allocation is short too. A class works out its instances' allocation (header,
alignment, size rounded up to whole words) when it is registered, so
`class_createInstance` is a `malloc` of a known size, a header, the `isa`,
and word stores zeroing the instance variables; `calloc` would zero the
header and `isa` for nothing, and `memset` costs more than a few stores.
`+new` allocates directly unless the class overrides `+alloc` or
`+allocWithZone:`, a flag its subclasses inherit. Replacing one of these
methods' implementations later (swizzling the root class's `-retain`, say)
sets the same flags on the class that owns it and every class below, so
the replacement is called. Objects come from Rust's global allocator, so a
program's `#[global_allocator]` serves them too. Freeing checks the
header's side-table bits once and, when none is set, goes straight to the
allocator.

**Side tables.** Weak references, associated objects and `@synchronized`
locks live in tables keyed by object address, each split into 64 shards on
cache lines of their own, with a one-multiply hash rather than SipHash.
Threads working on different objects don't wait for each other. A weak load
locks only the shard of the object it read, checks the location still holds
it, and retains through a CAS that fails once deallocation has begun;
freeing an object zeroes its weak references under that same lock, so the
object can't be freed under a load. An object with one weak reference, the
usual case, needs no allocation for it. The atomic association policies
retain the value under the lock and autorelease it after, as Apple's do.

**The autorelease handoff.** A method returning an object it doesn't own
ends with `objc_autoreleaseReturnValue`, and a caller keeping the object
starts with `objc_retainAutoreleasedReturnValue`, which objc2 emits on both
sides. The first parks the object in a thread-local slot instead of the
pool, and the second, given the same object, takes it back: the reference
passes from callee to caller without touching the count or the pool. The
parked object counts as the newest entry of the current pool, and
everything that could observe the difference (another autorelease, a pool
push or pop, the thread ending) first moves it into the pool, where
autoreleasing would have put it, so an object the caller doesn't keep lives
exactly as long as before.

## Message dispatch

`objc_msg_lookup(receiver, sel)` returns the implementation and the caller
calls it, the GNUstep convention objc2 already supports. No assembly
trampoline is involved, so dispatch is portable Rust. Small stubs for
aarch64 and x86_64 serve the rest: forwarding (below), `objc_msgSend` for
code that calls it directly, and methods made from blocks with
`imp_implementationWithBlock`, whose stubs are written into pages made
executable only once written.

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
evenly over a table's slots, the runtime's own selectors (`init`, `dealloc`
and so on) first. Tables grow at half full, and a table still sparse also
grows rather than let a new selector share a first slot with another, so
nearly every selector is found in the first slot it probes. On an M-series
Mac, a message costs about 1.1 ns on Sidestep (under Linux in a VM) against
1.25 ns on Apple's runtime; `examples/msgbench` measures it. With link-time
optimization (`lto = "fat"` in the release profile) the lookup inlines into
every call site, which `objc_msgSend` never can: 0.9 ns.

Unknown selectors go through `+resolveInstanceMethod:` /
`+resolveClassMethod:`, then forwarding, and then panic with the message
Apple's runtime raises, `-[Class selector]: unrecognized selector sent to
instance 0x…`; the panic unwinds into the Rust caller.

**Forwarding.** Since the caller calls the implementation with the receiver
it already has, sending a message on to another object means changing the
receiver on the way. When a class overrides `-forwardingTargetForSelector:`
(or `+forwardingTargetForSelector:`), selectors it doesn't implement resolve
to a trampoline of a few dozen instructions, one per architecture (aarch64
and x86_64, in `forward.rs`): it saves the argument registers, asks the
receiver for its target, puts the target where the receiver was, restores
the rest and jumps to the target's implementation. Arguments, stack
arguments and struct returns pass through untouched; on x86_64, where a
struct returned in memory moves the receiver to the second register, the
trampoline tells the two cases apart by whether that register holds a
selector. The trampoline is cached like a method, but the target is asked
for on every message. The trampolines carry unwind tables, so a panic in
`-forwardingTargetForSelector:` or from an unrecognized selector unwinds
through them. `-forwardInvocation:` is not supported: it needs
`NSInvocation`.

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
