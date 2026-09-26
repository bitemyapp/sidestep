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
shell loads as soon as anything reads more than its name. An unloaded shell
is still a complete object: its metaclass's `isa` is NSObject's metaclass
from the start, as loading would set it. A class nobody has used also keeps
its name: `objc_allocateClassPair` refuses it.

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
`+allocWithZone:`, a flag its subclasses inherit. A class registered after
its superclass gained or lost an override inherits the flags as they are
when it is registered. Replacing one of these methods' implementations
(swizzling the root class's `-retain`, say) works every class's flags out
again from the methods in its superclass chain, so the replacement is
called, and putting the original back returns the fast paths to every class
without an override of its own. Objects come from Rust's global allocator, so a
program's `#[global_allocator]` serves them too. Freeing checks the
header's side-table bits once and, when none is set, goes straight to the
allocator.

**Side tables.** Weak references, associated objects and `@synchronized`
locks live in tables keyed by object address, each split into 64 shards on
cache lines of their own, with a one-multiply hash rather than SipHash.
Addresses lose their four alignment bits before they are multiplied, which
spreads objects allocated one after another over all the shards rather
than a sixth of them. Threads working on different objects don't wait for
each other. A weak load
locks only the shard of the object it read, checks the location still holds
it, and retains through a CAS that fails once deallocation has begun;
freeing an object zeroes its weak references under that same lock, so the
object can't be freed under a load. An object with one weak reference, the
usual case, needs no allocation for it. The atomic association policies
retain the value under the lock and autorelease it after, as Apple's do.
Heap blocks are objects too: a block in a side table is marked in its
`reserved` word, which the runtime owns in heap copies, and
`_Block_release` takes it out of the tables when it frees it.

**The autorelease handoff.** A method returning an object it doesn't own
ends with `objc_autoreleaseReturnValue`, and a caller keeping the object
starts with `objc_retainAutoreleasedReturnValue`, which objc2 emits on both
sides. The first parks the object in a thread-local slot instead of the
pool, with the address it returns to, and the second takes it back when it
is called on the same object from the instruction at that address: the
reference passes from callee to caller without touching the count or the
pool. Adjacency is the point, as on Apple's runtime, which checks return
addresses the same way. A claim matched on the object alone could take the
pool's reference from a caller that didn't keep the object and later got it
back from a method that doesn't autorelease (`-self`, a collection's
getter), freeing it under a pointer the pool should keep valid. The
handoff applies when the method jumps to `objc_autoreleaseReturnValue` as
its last act (a tail call, which optimized builds of simple getters make)
and the caller claims straight after the call; small assembly entry points
on aarch64 and x86_64 pass the return addresses along. Otherwise the object
is simply autoreleased. A parked object counts as the newest entry of the
current pool, and everything that could observe the difference (another
autorelease, a pool push, a pop and every release it makes, the thread
ending) first moves it into the pool, where autoreleasing would have put
it, so an object the caller doesn't keep lives exactly as long as an
autoreleased one.

## Message dispatch

`objc_msg_lookup(receiver, sel)` returns the implementation and the caller
calls it, the GNUstep convention objc2 already supports. No assembly
trampoline is involved, so dispatch is portable Rust. Small stubs for
aarch64 and x86_64 serve the rest: forwarding (below) and `objc_msgSend`
for code that calls it directly, which share one register-saving frame
(`trampoline.rs`), the autorelease handoff's entry points, and methods made
from blocks with `imp_implementationWithBlock`, whose stubs are written
into pages made executable only once written.

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
`+resolveClassMethod:`, then forwarding, and then `-doesNotRecognizeSelector:`,
which classes may override and NSObject's panics with the message Apple's
runtime raises, `-[Class selector]: unrecognized selector sent to instance
0x…`; the panic unwinds into the Rust caller.

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
selector (a lock-free range check: selectors live in a few chunks, each
twice the size of the one before). The trampoline is cached like a method,
so `-performSelector:withObject:` and `-methodForSelector:` find it as a
message does, but the target is asked for on every message. The
trampolines carry unwind tables, so a panic in
`-forwardingTargetForSelector:` or from an unrecognized selector unwinds
through them. `-forwardInvocation:` is not supported: it needs
`NSInvocation`.

`+initialize` is sent lazily before a class's first message, superclasses
first, with messages from inside `+initialize` on the same thread allowed
through.

## Foundation collections

Apple's collections are class clusters: `NSArray` is abstract and every
instance is some private subclass. Sidestep's are concrete classes under
the public names, and the mutable ones are subclasses of the immutable
ones, as the objc2 bindings require.

- **Storage.** An immutable `NSArray` holds a boxed slice of retained
  elements, `NSIndexSet` sorted ranges with their total, and `NSDictionary`
  and `NSSet` a hash table (`crates/sidestep-foundation/src/table.rs`):
  entries in a vector with their hashes, plus an open-addressed index of
  positions once there are more than four. Nothing in them changes after
  `init`, so any thread may read them. A mutable `NSArray` keeps its
  elements in a `Deque` (`deque.rs`), a vector with room at both ends, so
  adding and removing at either end costs constant time and apps can use
  it as a queue.
- **Reading while other code runs.** A mutable collection's storage sits in
  a `Guarded` cell (`guarded.rs`): an `UnsafeCell` beside an atomic count of
  the readers that are sending messages while they hold it. Reads that send
  none (`-count`, `-objectAtIndex:`, a lookup of one of Sidestep's strings
  or numbers) write nothing, so threads can read a mutable collection at
  once, as Foundation allows. A change checks the count, so an `-isEqual:`
  or comparator that mutates the collection it was called from panics with
  Foundation's "was mutated while being enumerated" instead of freeing
  elements under the loop. Changes never run other code while they hold
  the storage: they retain what they add before and release what they
  remove after.
- **No messages on hot paths.** Methods check whether the receiver's class
  is exactly Sidestep's, one comparison, and then read the storage
  directly. Keys and elements that are exactly Sidestep's strings or
  numbers are hashed and compared without messages (strings cache their
  hashes); anything else gets `-hash` and `-isEqual:`.
- **Subclasses.** A subclass defined in an app is reached through its
  primitive methods (`-count`, `-objectAtIndex:`, `-objectForKey:`,
  `-keyEnumerator`, `-member:`, `-objectEnumerator`), and a mutable
  subclass is changed through its primitive mutators
  (`-insertObject:atIndex:`, `-setObject:forKey:`, `-addObject:` and the
  rest Foundation lists), as Foundation specifies. `NSIndexSet` is a
  concrete class in Foundation, so its subclasses keep their indexes in the
  storage they inherit.
- **Fast enumeration.** Arrays hand out their element buffer in one batch;
  dictionaries and sets copy batches into the caller's buffer.
  `mutationsPtr` points at the mutation count, which objc2's iterators
  check before reading each element. `NSEnumerator`s follow Foundation's
  looser rules: an array's walks it by position up to its count when the
  enumerator was made, so the array may change meanwhile; a dictionary's or
  set's retains 16 entries at a time and fails when the collection changed
  and it would otherwise hand out or skip entries.
- **Copy-on-write.** A copy of a mutable collection shares its storage
  behind a reference count; the mutable one copies the storage before its
  next change. So `[items copy]` costs the same for ten elements or ten
  thousand, as on Apple's Foundation.
- **Numbers.** `NSNumber` keeps the value in its widest type and reports
  Foundation's C types (`numberWithUnsignedChar:` is a `short`, and so
  on). Numbers compare by value across types and hash through their value
  as a `double`, so `1` and `1.0` are one dictionary key. Small integers
  are shared objects made on first use. Conversions C leaves undefined
  (a negative `double` to `unsignedLongLongValue`) follow what Apple's
  hardware does.
- **Failures.** Where Foundation raises (an index out of range, a nil
  element), Sidestep panics with Foundation's message, which unwinds into
  the Rust caller like an unrecognized selector.

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

## Foundation services: the run loop and what feeds it

Every thread has at most one run loop, made on first use. Its state is
split by who may touch it:

- **Shared** (an `Arc`): the owner's thread id, a wake cell, an inbox, the
  current mode and the set of common modes. The wake cell is an atomic
  state beside a mutex and condition variable: a sleeping loop is woken
  only if it is asleep, and a wake that arrives before the sleep isn't
  lost. The inbox is a `Mutex<Vec>` swapped out whole; it carries blocks,
  timers added from other threads, main-queue work and stop requests.
- **Owner-only** (a thread-local `RefCell`): timers in a `BTreeMap` keyed
  by due time and sequence, observers sorted by order, signalled sources,
  queued blocks and the stack of running modes. No borrow is held across a
  callout; work is copied out first, because any callout may run the loop
  again.

An `NSThread`'s loop is made by `-start`, on the starting thread, so work
handed to it right away waits for the thread to run it. A loop ends with
its thread: its inbox closes (work handed to it later is dropped, and a
caller waiting for such work is told the thread exited, as macOS raises)
and its state is dropped while the thread can still run code, since
releasing a timer's target may use a run loop again. An `NSRunLoop`
keeps its CFRunLoop object, so `-getCFRunLoop` answers on any thread.

A pass follows CFRunLoop's documented order: entry, timers, sources,
blocks, sleep until the next due timer or a wake, exit, with an
autorelease pool around each pass and observers told at each step. Timers
keep their phase after a stall and drop missed fires; their fire dates are
kept on the monotonic clock and converted to `NSDate` time only at the
edges. The idle main loop wakes for nothing.

Rust code in other crates uses `runloop::main()`/`current()`,
`add_source` (whose `SourceSignal::signal_and_wake` is `Send + Sync`),
`add_observer`, `run_mode`, `perform` and `stop`, and
`notification_center::post`, which does no work when nobody observes the
name. AppKit's event loop is built on them (see "Events and the run loop").

Handing work to another thread's loop always goes through its inbox:
`performSelectorOnMainThread:` (queued for the loop's one perform
source, which runs every waiting request for the running mode in one go,
as macOS does; waiting callers on the target thread run inline),
`CFRunLoopPerformBlock` with `CFRunLoopWakeUp`, the main dispatch queue
and `NSOperationQueue.mainQueue`.

**libdispatch** is implemented in Rust behind its C ABI, so `dispatch2`
works unchanged (the linker finds an empty `libdispatch.a` that
`build.rs` writes). Dispatch objects are Objective-C objects; queue
attributes are immortal ones (serial or concurrent, active or initially
inactive). One global pool runs work; a monitor adds a worker when queued
work has waited 50 ms, up to a cap, and retires idle extras after 5 s.
Serial queues drain in batches of 64 on their target, concurrent queues
honour barriers. `dispatch_sync` runs the block on the calling thread: an
idle queue is taken on the spot, with no other thread involved, and a
busy one hands itself to the waiting caller when its turn comes. One
thread serves `dispatch_after` and timer sources; vnode sources watch
through inotify on one I/O thread. A client callout that unwinds aborts
the process, as with libdispatch.

**NSNotificationCenter** keeps one slab of registrations, each with a
sequence number, and lists each in one of four places: by name and then
object, by name only, by object only, or among the wildcards. A post
looks only at the lists its name and object can match, collects the
registrations, releases the lock, and calls them in registration order,
skipping any removed meanwhile; selector observers are weak.

**Locks.** `NSLock`, `NSRecursiveLock` and `NSCondition`'s lock are futex
words: an uncontended lock and unlock is one atomic operation each, with
no system call.

**Value and system classes.** `NSData` has one storage enum (owned,
borrowed with a deallocator, or growable). `NSURL` keeps its string, base
and RFC 3986 component ranges, with the resolved form and file-system
representation cached; the parser is Sidestep's own, with macOS's
departures from the RFC. File operations (`NSData`, `NSFileManager`,
`NSURL`) report the Cocoa error codes macOS reports for each operation,
with the POSIX error underneath. Search-path directories map to the XDG
base and user directories; trashing follows the freedesktop.org
specification. `NSBundle.mainBundle` is an `.app` layout, then
`<exe>/Resources`, then `<exe>/../share/<name>`, then the executable's
directory. `NSUserDefaults` keeps every domain in memory as property-list
values, shared by all instances, and a background thread writes changed
domains to `$XDG_CONFIG_HOME/<domain>/defaults.plist` (atomically, and
again at exit); a domain's values are shared and copied on write, so the
writer holds the store's lock only for its snapshot. Property lists are read with the `plist` crate and
written as XML by Sidestep, byte for byte as macOS writes them.
`NSJSONSerialization` has its own reader and writer. `NSDateFormatter`
is a TR35 pattern engine with English symbols and the en_US style
patterns; `NSTimeZone` uses jiff over the system's tz database.

**CoreFoundation** functions are toll-free: a `CFStringRef` is an
`NSString` and so on, so each CF function forwards to the Foundation
class behind it. `CFGetTypeID` goes by class name, so it needs no link
reference to classes that may not be built.

Parts that return arrays or numbers (`URLsForDirectory:inDomains:`,
`queryItems`, `-[NSURL port]`, `CFArray`, `CFNumber`, JSON arrays) build
with sidestep-foundation's `collections` feature, until Foundation's
collections land; the rest never needs them.

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
  layer, in layer points.
- **Display.** Once the render thread reports the last frame shown, the
  window places its scroll layers, calls `drawRect:` only for damaged areas,
  sends the operations and presents. A window the compositor isn't showing
  gets no frame callbacks, so it stops drawing.
- **Rendering.** The render thread owns the Wayland connection through
  smithay-client-toolkit. It rasterizes operations on the CPU into a cache
  per layer (tiny-skia for paths, swash for glyphs), only inside damaged
  rectangles. The window surface is presented from a few shared-memory
  buffers, each remembering what changed since it was last written, so a
  frame copies and damages only changed pixels. A Wayland protocol error
  ends the connection; the render thread then stops, and the main thread,
  seeing its channel close, exits with a message rather than running on
  without windows.
- **Scrolling.** A document layer is cut into 512-point-tall tiles, each on
  its own subsurface and cropped to the clip view with wp_viewporter.
  Scrolling moves tiles and changes crops. A tile is drawn and uploaded when
  it comes within a tile of the viewport or its content changes, and dropped
  when it is two tiles away.
- **Nested and modal loops.** Every loop is the main thread's run loop,
  run in a mode (see "Events and the run loop" below): the main loop in
  the default mode, `nextEventMatchingMask:untilDate:inMode:dequeue:` (a
  view following a drag) in the mode it is given, a modal loop in
  `NSModalPanelRunLoopMode`. Drags are typed from the buttons input says
  are held, so a loop that takes the mouse-up leaves nothing stuck.
  `runModalForWindow:` asks for the keyboard back (xdg-activation) if the
  compositor gives it to another of the program's windows.

This design keeps GPU wake-ups and uploads proportional to what changed,
which is what dominates power on a mostly idle desktop. A GPU rasterizer can
replace the CPU one behind the same operations later.

### Scale

Operations stay in points. Each window renders at its output's scale: the
fractional one from wp_fractional_scale_v1 where the compositor offers it
(unless `SIDESTEP_NO_FRACTIONAL_SCALE` is set), else the integer one (the
surface's preferred buffer scale, or its outputs'). Until the compositor
says, a new window takes the outputs' scale when they agree, so on
integer-scaled outputs its first frame is drawn at the right one; a
fractional scale is known only once the window is mapped (wl_output rounds
it up), so there the first frame is drawn at the integer scale and the
window draws again. Every surface, tiles and decorations included, gets a
buffer of `points × scale` pixels (rounded as the fractional-scale protocol
asks) and a wp_viewporter destination of its size in points, so the
compositor maps buffer pixels to screen pixels one to one instead of
resampling. A tile cut by its scroll view shows a crop of whole buffer
pixels, kept inside the buffer; at a fractional scale that crop can be up
to half a pixel from the exact one, which moves the tile by as much rather
than blurring it. A canvas
has a `scale`; fills and damage cover the pixels whose centers they
contain, so neighboring fills share edges without gaps or overlaps at any
scale, and a pixel's value never depends on how damage was cut up. A scale
change drops every tile and redraws the window; `backingScaleFactor` and
the `convert…ToBacking:` methods report it.

### Input

The render thread binds each wl_seat itself (up to version 9, for
high-resolution wheels) and turns its events into messages for the main
thread, which makes `NSEvent`s and sends them through
`-[NSApplication sendEvent:]` to the window. Local event monitors
(`addLocalMonitorForEventsMatchingMask:handler:`) see each event there
first and may change or swallow it; global monitors never fire, as
Wayland shows a client no other program's input.

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
  `isARepeat`. `keyCode` is the macOS virtual key code, from a table
  measured on macOS through public API rather than taken from headers
  (`keycode_table.rs`; `conformance/tests/keycodes.rs` checks it against
  Apple's frameworks).
- **Routing.** Command-key presses are offered as key equivalents first
  (`performKeyEquivalent:` down the view tree, depth first); keys and
  modifier changes then go to the first responder, which is the window
  itself until a view takes over, and up the responder chain from there. A
  view outside the window can't become its first responder: the window
  takes over, as in AppKit. The window with Wayland's keyboard focus is the
  key window if it can become key (it has a title bar, or overrides
  `canBecomeKeyWindow`), gets `becomeKeyWindow` and tells its delegate, and
  becomes the main window too if it can; one that can't (a borderless
  window) leaves the key window as it was, and keys typed on it go to the
  key window, as on macOS. Whether any window of ours still has the keyboard
  is settled after each batch of input, so focus moving between two of them
  doesn't make the key window resign in between. The application is active
  while it has a key window. Without a keyboard, the window the compositor
  calls activated has the focus. A key held while the main thread is busy
  doesn't pile up: repeats followed by a newer key message for the same
  window are dropped. `interpretKeyEvents:` applies AppKit's standard key
  bindings: text goes to `insertText:`, editing keys become commands
  (`moveLeft:`, `deleteBackward:`, the Emacs-style Control keys, their
  Shift variants that extend the selection) sent with `doCommandBySelector:`
  (with no sender), which performs them where a responder up the chain has
  them, and other Command and Control keys become `noop:` or nothing. The
  table was read off AppKit on a Mac by feeding it the events Sidestep makes
  for every key of a US keyboard with every combination of modifiers;
  `conformance/tests/keybindings.tsv` keeps AppKit's answers, and the
  conformance test holds both platforms to them. Actions without a target (`sendAction:to:from:`) go to
  the first that has them of the key window's responder chain, the window's
  delegate, the application and its delegate; `tryToPerform:with:` walks the
  same way. Wayland can't hide windows, so `hide:` minimizes them and
  `unhide:` asks for activation.
- **Input methods.** A view that implements `NSTextInputClient` has an
  `NSTextInputContext`, whose `handleEvent:` applies the key bindings with
  `insertText:replacementRange:` for text (leaving out the writing-direction
  commands and Control-/, as AppKit's does) and answers NO for keys that do
  nothing. While the key window's first
  responder is such a view, the render thread enables zwp_text_input_v3
  for the window and keeps the input method told where the caret is (the
  client's `firstRectForCharacterRange:actualRange:` for its selection,
  asked again after keys, clicks, the input method's changes and
  `invalidateCharacterCoordinates`). What the input method sends between
  two `done` events arrives as one message: committed text becomes
  `insertText:replacementRange:` and text being composed
  `setMarkedText:selectedRange:replacementRange:` (an empty string when
  composing ends), its selection turned from bytes into UTF-16 units. A
  pending compose sequence shows the same way: after a dead key the client
  has its accent as marked text, which the composed character replaces.
  Surrounding text isn't sent, so input methods have nothing to delete
  around the caret; `discardMarkedText` disables and re-enables the text
  input so the input method starts over. Clients are recognized by their
  methods, since the runtime knows no protocol it wasn't given.
- **Pointer.** A pointer frame may leave one of a window's surfaces and
  enter another (a tile, the root), so the render thread settles crossings
  at the end of each frame and tells the main thread when the pointer
  enters or leaves a window's content. It counts clicks (400 ms and 5
  points apart at most, GTK's defaults, which GNOME keeps) with the
  compositor's timestamps. Buttons map to AppKit's numbers: middle is 2,
  back 3 and forward 4. Touchpads scroll by precise point deltas; wheels by
  lines (3 a detent, fractions from high-resolution wheels), and Shift turns
  a wheel's scroll sideways in the event itself, as macOS does, so every
  view sees it so. `isDirectionInvertedFromDevice` says whether the
  compositor reverses the device's direction (natural scrolling, wl_seat 9's
  axis_relative_direction). A touchpad
  scroll is a gesture, `phase` began, changed and ended (at axis_stop, when
  the fingers lift), and since compositors don't coast, Sidestep does: a
  gesture that ends faster than 60 points a second is followed by
  `momentumPhase` events, one per frame the window shows (or every 50 ms
  when none come), each moving as far as the time since the last allows,
  the speed measured over the last 100 ms of the gesture and decaying by
  0.3% a millisecond, until it's slow, another scroll starts or a button is
  pressed. Touchpad pinches
  (zwp_pointer_gestures_v1) become magnify events, whose magnifications
  multiply to the pinch's scale, and rotate events once the fingers turn,
  sent to the view under the pointer.
- **Cursors.** A wp_cursor_shape_v1 shape where the compositor offers the
  protocol, else an image from the cursor theme (`XCURSOR_THEME`,
  `XCURSOR_SIZE`) at the window's scale. `NSCursor`'s standard cursors map
  to shapes; one that's set shows over every window's content (the
  decorations keep their resize cursors), `push` and `pop` stack them, and
  `setHiddenUntilMouseMoves:` hides the pointer until it moves.
- **Tracking areas and cursor rectangles.** A view keeps its
  `NSTrackingArea`s and cursor rectangles and its window a list of the
  views that have any (a flag on each view makes joining the list constant
  time), so each pointer event looks only at those. The
  window tells owners `mouseExited:`, then `mouseEntered:`,
  `cursorUpdate:` and `mouseMoved:`, honoring the active-when options,
  `InVisibleRect` and `EnabledDuringMouseDrag` (without it, what changed
  during a drag is told when the button comes up). Cursor rectangles set
  their cursor on the way in and the arrow on the way out; where areas or
  rectangles overlap, the deepest view wins. When views move, resize,
  scroll, hide or come and go, the views in the subtrees that changed (all
  of them once more than 16 subtrees did) get `updateTrackingAreas` and
  `resetCursorRects` before the window next looks, and the window looks
  again at the end of the turn so content moving under a still pointer
  counts; while the pointer is elsewhere that waits until it comes back.
  `invalidateCursorRectsForView:` resets that view's rectangles only.
  `examples/trackbench` measures the walk: for a window of 1,011 views that
  each replace a tracking area and add a cursor rectangle, resetting every
  rectangle takes 75 µs and moving the content view (every area and
  rectangle again) 165 µs on Linux, against 316 µs and 915 µs for AppKit
  on an M-series Mac; with 3,031 views, 253 µs and 564 µs against 916 µs
  and 4.6 ms.

[kbvm]: https://github.com/mahkoh/kbvm

### Events and the run loop

AppKit runs on Foundation's run loop (`event_loop.rs`); it adds no loop
of its own. When `NSApplication` is made (or the render thread started),
it adds `NSModalPanelRunLoopMode` and `NSEventTrackingRunLoopMode` to the
main loop's common modes and registers two things there:

- **The event source.** The render thread's sender to the main thread
  signals it and wakes the loop after every message; when the render
  thread stops, its sender going away wakes the loop to see the channel
  closed. Performing the source moves the messages into an inbox and
  handles them oldest first: configures, frames, focus and close requests
  act at once, input becomes `NSEvent`s sent through
  `-[NSApplication sendEvent:]`. While a nested loop looks for events, and
  while events wait from an earlier nested loop or `postEvent:atStart:`,
  new ones join the queue behind them, and the loop outside sends them in
  order when it gets back. The inbox is shared by every drain, so a loop
  nested inside an event handler carries on with the messages that came
  with the event it is handling (a mouse-up that arrived with its
  mouse-down reaches the drag loop the down started). Moves followed by
  another move of the same window are coalesced, and key repeats followed
  by a newer key message are dropped.
- **The display pass**, an observer before the loop sleeps and when a run
  ends, at order 2 000 000 (after programs' own before-waiting
  observers): each window displays if the render thread showed its last
  frame, tracking areas look again at views that moved, and pasteboard
  changes are offered. Registered in the common modes, it runs in modal,
  tracking and terminate-later loops too.

`-[NSApplication run]` calls `finishLaunching`, posts that launching
finished (as macOS does, at the first look for events after
`finishLaunching`), then runs the default mode until `stop:`; a modal loop
runs `NSModalPanelRunLoopMode` until the session is decided; each returns
to its caller after every batch of input to send what waits. Whatever
ends a loop (`stop:`, `stopModal…`, `replyToApplicationShouldTerminate:`)
also stops the run loop's innermost run, so a timer's callout ends it
without waiting for input. So timers and sources fire in exactly the
modes they were added to: a tracking loop fires tracking-mode and
common-mode timers, not default-mode ones. AppKit's own deadlines
(touchpad momentum when no frames come, the pause before deciding the
application lost focus) share one common-mode timer, moved only when a
deadline comes sooner; an idle program sleeps with nothing armed and
allocates nothing per turn. `+sharedApplication` is added to the class by
hand (a `define_class!` class method can't see its receiver), so sent to
a subclass it makes an instance of the subclass, whose `sendEvent:` then
sees every event.

**Notifications and delegates** (`notifications.rs`). Every window,
application and view notification name is exported with macOS's value.
As on macOS, a delegate hears through the default notification center:
`setDelegate:` asks the delegate once which notification methods it has
and registers it for those, with the window or application as the
object, so telling the delegate is a post and the delegate hears among
the other observers in registration order. Posting costs a load or two
when nobody observes; views remember whether anyone observes the frame
and bounds names until the center's registrations change (a generation
count the center keeps), so an unobserved frame change costs no lock. A
window's frame notifications follow macOS: a new size posts
`NSWindowDidResizeNotification` only, a move alone
`NSWindowDidMoveNotification`; the compositor's resizing state becomes
live resize notifications and `inLiveResize`, a new scale
`NSWindowDidChangeBackingPropertiesNotification` with the old scale in
its user info. `close` posts that the window will close, orders it out
and, for a window released when closed (the default), gives its own
reference to the autorelease pool, as AppKit does. Whether to quit after
the last window closed is asked after the event being handled, not
inside `close`. `terminate:` asks `applicationShouldTerminate:`; a later
answer runs the loop in `NSModalPanelRunLoopMode` until
`replyToApplicationShouldTerminate:`.

**Responders and keys.** Keys go to the key window (none: dropped); a
Command key is a key equivalent first, the key window's views' and then
the main menu's once there is one. A key no responder takes reaches the
window's `keyDown:`, which offers it to its views as a key equivalent and
then moves through the key view loop for Tab and Shift-Tab (`keyloop.rs`,
whose links are weak both ways) or sends `cancelOperation:` up the chain
for Escape, as Command-period does before anything else. A responder at
the end of a chain hears `noResponderFor:`. The render thread tags the
press that gave its window the keyboard (compositors hand over the
keyboard before the click arrives, so the window is key by then); that
first click reaches a view only if it `acceptsFirstMouse:`. A left press
on a window that moves by its background, in a view that lets it
(`mouseDownCanMoveWindow`: views that aren't opaque), moves the window
through the compositor instead.

**Sheets and modal sessions** (`modal.rs`). Sheets are window-modal and
asynchronous: attached one at a time (others wait their turn), each ended
by `endSheet:returnCode:`, which calls its handler, posts that it ended
and attaches the next. While a sheet is attached the parent refuses mouse
input and the sheet is key in its place (the compositor's keyboard stays
on the parent's toplevel). On screen a sheet is part of its parent: a
desynchronized subsurface of the parent's surface with its own canvas,
buffers and frame callbacks, placed top-centre under the title bar and
kept there when the parent resizes, above the parent's scroll tiles
(`backend/sheet.rs` keeps that stacking in one place). A modal session is
the modal loop a pass at a time. `NSPanel` floats over the main window
(Wayland has no levels), hides when the application stops being active
(only while another window stays up, as a desktop without a dock couldn't
bring the program back otherwise), may work when modal, and may become
key only when a view needs it to.

**Tooltips** (`tooltip.rs`) are tracking areas of their views, as on
macOS, with one private owner. After the pointer rests for
`NSInitialToolTipDelay` (1000 ms unless the defaults say otherwise), a
borderless window opens as an ungrabbed popup below the pointer, drawn
with the text engine in the tooltip font; leaving, a click or a key take
it down. Controls with a tooltip per part use `tooltip::add_text_rect`.

**Controllers** (`controllers.rs`), without nibs, which Linux doesn't
have. A view controller loads its view when first asked (`loadView`, a
plain view unless overridden, then `viewDidLoad`) and sits in the
responder chain between the view and the view's superview, as on macOS:
setting the view's next responder sets the controller's. A window
controller is its window's next responder. Neither is retained by what
it's next to; one going away puts the links back. Periodic events
(`startPeriodicEventsAfterDelay:withPeriod:`) come from a common-mode
timer that posts one only when none is waiting.

**Frame autosave.** A window with an autosave name saves its frame in the
user defaults whenever it moves or resizes, under macOS's key
(`NSWindow Frame <name>`) and in its form (the numbers, whole ones
without decimals); Sidestep writes the frame without a screen, since it
doesn't know where windows are. Taking a saved frame asks the compositor
for its size and keeps its origin as the one `frame` reports.

**Testing without a display.** `SIDESTEP_BACKEND=null` (or
`sidestep_appkit::testing::use_null_backend`) starts a render thread that
answers as a compositor would, at once and always the same way, draws
nothing, and writes down what it was asked; the `testing` module plays
the compositor's part for input, adding messages to the main thread's
inbox, and runs the loop until everything was answered and handled.
`crates/sidestep-appkit/tests/linux_events.rs` uses it.
`examples/eventbench` measures the machinery: on the same M-series Mac
(Sidestep under Linux in a VM), an unobserved view frame change costs
about 75 ns against about 950 ns in AppKit, posting an event and taking
it back in a tracking loop 43 ns against 33 µs, and sending a key through
a window to its first responder 13 ns against 175 ns.

### Windows and decorations

A window's size, maximized and fullscreen states and keyboard focus belong
to the compositor: `zoom:`, `toggleFullScreen:`, `miniaturize:`,
`setContentSize:` and `makeKeyWindow` (through xdg-activation) ask, and the
window follows the compositor's configures, telling its delegate. As in
AppKit, `performClose:`, `performMiniaturize:` and `performZoom:` need the
close, miniaturize and resize buttons in the style mask (the compositor's
close request is the close button's click), `miniaturize:` does nothing to
a window that isn't on screen, and `close` tells the delegate
`windowWillClose:` even then. The application's `windows` and
`windowWithWindowNumber:` hold every window the program has, on screen or
not. Each showing of a window is a new window to the render thread, so
its messages about an earlier showing, still on their way, find nothing. A window
the compositor calls suspended (covered, minimized or on another
workspace, for compositors that say so) isn't visible to `occlusionState`,
and its delegate hears `windowDidChangeOcclusionState:`. Size limits go to
the compositor in window-geometry terms. Wayland doesn't say where windows
are, so `frame` keeps the origin a program gives it. A
borderless child window (`addChildWindow:ordered:`) becomes an xdg_popup
placed where its frame is relative to its parent's, which is how tooltips
and completion lists are made; `show_as_popup` opens one below an anchor
with an input grab, the start of menus. A titled child window stays a
toplevel with its parent as xdg parent, which compositors keep it above,
and a modal window gets the window its session came from.

Where the compositor leaves decorations to the client (GNOME's does; a
compositor without xdg-decoration, or one answering with client mode),
the render thread draws them: an Adwaita-like header bar with the title and
close, maximize and minimize buttons (as the style mask and the
compositor's capabilities allow), a shadow and a resize border. Each part
is a subsurface outside the window's own surface, so the content view's
coordinates don't change; the window geometry takes in the header but not
the shadow, and the parts are synchronized subsurfaces. After a resize or
a new scale they're drawn by the present that brings the content drawn
for it, so a resize shows atomically; changes the content doesn't follow
(focus, hover, the title, the style) show at once, and parts are drawn
again only when what they show changed. Dragging the header moves the window, double-clicking it
maximizes, the right button opens the compositor's window menu, and the
border resizes with the matching cursor; maximized, tiled and fullscreen
windows lose the border and rounded corners, and fullscreen ones the header
too. They're drawn with tiny-skia rather than by a crate such as
sctk-adwaita so they render at fractional scales like everything else, the
title is set with the same text drawing as the content (the main thread
sends it as operations), and nothing runs a subprocess to find a font.
`SIDESTEP_DECORATIONS=client` draws them on compositors that would draw
their own (sway). They're light or dark as the desktop prefers: a thread
of its own asks xdg-desktop-portal's Settings interface for
`org.freedesktop.appearance` `color-scheme` over the session bus (a
minimal D-Bus client, `desktop.rs`, rather than a D-Bus crate and its
async runtime) and redraws them when the desktop signals a change.
`SIDESTEP_THEME=dark` or `light` (or a dark `GTK_THEME`) overrides it.

A window keeps the settings programs give it (background color, level,
shadow, alpha, collection behavior and the rest) and reads them back. The
ones Wayland has a use for act: views are drawn over the background color,
`setIgnoresMouseEvents:` gives every surface an empty input region so
clicks reach what's behind, `setMovable:` stops the header from moving the
window, a hidden title visibility leaves the title out of the header, and
the initial first responder takes over the first time the window is shown.
`performWindowDragWithEvent:` asks the compositor to move the window with
the pointer, from the press being handled, for windows that draw a title
bar of their own; the compositor then has the pointer, so the press ends
there without a mouse-up. Levels, shadows, alpha, tabbing and resize
increments have no Wayland request behind them and are only kept.

### The clipboard

The general `NSPasteboard` is the Wayland selection. Writes never wait: the
main thread keeps what it wrote (each string encoded once) and, once per
turn of the event loop, has the render thread offer a copy, so a copy that
writes several types makes one selection; the render thread serves other
clients' reads through non-blocking pipes from its event loop, with a
marker type so it never reads its own offer back. Reading can't be
synchronous on Wayland, so once a program has read the pasteboard, the
render thread reads another client's text ahead (up to 1 MiB) whenever it
offers a selection, and `stringForType:` answers from that at once. Text
still on its way is waited for until 200 ms after the selection came, and
no longer, so a client that never answers costs one wait; other types, and
longer text, are read when asked for, waiting 200 ms at most. Reads that
don't finish are abandoned, their pipes closed. A counter both threads
share plays `changeCount`; an offer that repeats the last one (compositors
offer the selection again whenever a window gets the keyboard) isn't a
change. The old type names (`NSStringPboardType`, …) name the current
types. Pasteboards made by name live in the process only.

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
  on a background thread as soon as `NSApplication`, `NSColor`, `NSFont` or
  `NSParagraphStyle` loads.
- **NSFont.** A font is a spec (family or system design, weight, italic,
  width, size) resolved to a face, whose metrics and names come from the
  font file through skrifa. `NSFontWeight` values map through the named
  weights to CSS weights. Like browsers, and unlike parley's default, a
  face is emboldened only when bold is asked of a family without one:
  Medium in a family with Regular and Bold is Regular. `fontWithName:`
  finds families, PostScript names (`DejaVuSans-Bold`) and full names, and
  maps Apple's own families (Menlo, SF Mono, Helvetica, Times, …) to the
  system designs, so programs written for macOS find a font; a descriptor
  naming a font the system lacks makes none (`fontWithDescriptor:size:`
  gives nil), so fallback chains work. Descriptors take family, name,
  size, traits and feature settings (by Apple's feature registry numbers
  or OpenType tags) from attribute dictionaries. Size 0 means 13 points for
  the interface fonts (`systemFontOfSize:` and its kin) and 12 for the
  rest, as on macOS.
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
  truncation don't wrap. Widths count trailing spaces, count indents only
  for text that wraps, and are the full width for justified lines.
  `drawInRect:` clips lines that don't fit rather than dropping them;
  `boundingRectWithSize:` and `drawWithRect:` with a height drop the lines
  after the first that don't fit (the first is kept however short the
  height, and one line on a baseline ignores the height), and with
  `truncatesLastVisibleLine` end the last line kept in an ellipsis whenever
  any text follows it. Emoji are shaped from the color
  emoji family first, as on macOS, where the text's own face might
  otherwise give them plain glyphs. Tabs go to the paragraph's tab stops
  (left, right, centered or decimal; by default twelve, 28 points apart),
  then every
  `defaultTabInterval`; control characters take no room.
- **Caches.** Laid-out text is cached per thread by string, attributes and
  options (two generations of 2048 entries or 4 MB), so a view that
  redraws the same lines records them for about 0.1 µs a line. Glyph runs
  keep their glyphs in an `Arc`, so recording a cached line copies no
  glyphs. Paragraph styles own their tab stops, so nothing outlives the
  styles and layouts that use them.
- **Parallel layout.** Drawing doesn't need the layout until the pass
  ends, so text drawn without having been measured is set aside, and the
  pass lays it all out at its end on a small pool of worker threads, the
  main thread taking a share, and splices the ops in where they were
  drawn. Text drawn several times in a pass (a label down a table) is laid
  out once. A new page of text lays out several times faster than one line
  after another; measuring (`sizeWithAttributes:`) stays synchronous.
- **Rasterizing.** The render thread rasterizes each glyph once per face,
  pixel size and quarter-pixel horizontal offset with swash (no hinting,
  as macOS draws), and composites coverage masks and premultiplied color
  images (COLR layers and CBDT or sbix bitmaps, such as Noto Color Emoji)
  from the cache. Baselines sit on whole pixels, and so do glyphs of faces
  made only of bitmaps, which are rasterized once per size. swash parses a
  face once and builds one scaler per run. The cache keeps two
  generations (24 MB in all, masks counted a byte a pixel), so the images
  a frame uses survive a turnover.
- **String drawing methods.** `NSString` gets `drawAtPoint:` and the rest
  as a category would give them: a helper class's methods, copied over by
  the loader of the helper's static shell (`_SidestepStringDrawing`),
  which the `NSColor`, `NSFont` and `NSParagraphStyle` loaders and the
  display pass message. The copying runs under the runtime's class-loading
  lock and takes no lock of its own, so loaders on two threads can't wait
  on each other.

parley was chosen over cosmic-text, the other complete pure-Rust stack.
parley takes styles as ranges over the text, which is what an attributed
string's runs are; it lets each line have its own width and indent and
aligns and justifies lines itself, which paragraph styles need; and
fontique asks fontconfig for families, aliases and fallback, where
cosmic-text's fontdb scans directories and falls back through lists of its
own. cosmic-text takes line heights as explicit metrics (per buffer, or
per span) rather than from each font's ascent and descent as AppKit does,
and has no per-line indents or paragraph spacing, which paragraph styles
need; its editable buffers fit a text editor better than string drawing.
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
