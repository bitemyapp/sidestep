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

**Categories.** A framework adds methods to a class it doesn't define
(Foundation's forwarding methods on `NSObject`, AppKit's drawing methods
on `NSString`) with `sidestep_runtime::category!`, which puts the class's
name and a function adding the methods in a sibling section,
`sidestep_categories`. `objc_registerClassPair` runs a class's categories
before it marks the class loaded, and other threads wait for loading, so
the methods are there before anything can message or inspect the class,
whichever class a program uses first. A category's method replaces one of
the class's own, as on Apple's runtime, without emptying other classes'
method caches, since nothing has used the class yet. Two categories adding
one selector to a class panic in debug builds, and debug builds check a
category's encodings against the methods they override, as objc2 does for
subclasses. Such a panic ends the program with its message: registration
is a C function that objc2 declares as never unwinding.

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
from blocks with `imp_implementationWithBlock`, whose stubs are written to
an anonymous file (`memfd_create`) mapped read-only and executable, so a
process denied writable-then-executable memory (systemd's
`MemoryDenyWriteExecute=`) can still make them.

`objc_msgSend` probes the method cache in assembly, as the lookup below
does, and on a hit jumps to the implementation with the argument
registers untouched; only a miss saves them and calls `objc_msg_lookup`.
Called directly it costs 0.94 ns (against Apple's 1.17 ns on the same
Mac). The assembly takes the cache's layout from `cache.rs`'s constants,
which compile-time assertions tie to the slot type, and a unit test
counts the messages that reach the slow path.

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
receiver on the way. Selectors a class doesn't implement resolve to a
trampoline of a few dozen instructions, one per architecture (aarch64 and
x86_64, in `forward.rs`), as Apple's resolve to `_objc_msgForward`: it saves
the argument registers and asks Rust what to do. If the receiver's class
overrides `-forwardingTargetForSelector:` (or
`+forwardingTargetForSelector:`) and it names another object, the
trampoline puts the target where the receiver was, restores the rest and
jumps to the target's implementation. Arguments, stack arguments and struct
returns pass through untouched; on x86_64, where a struct returned in
memory moves the receiver to the second register, the trampoline tells the
two cases apart by whether that register holds a selector (a lock-free
range check: selectors live in a few chunks, each twice the size of the one
before). The trampoline is cached like a method, so
`-performSelector:withObject:` and `-methodForSelector:` find it as a
message does, but the target is asked for on every message.

Otherwise the message goes to `-forwardInvocation:`. The runtime can't
make an `NSInvocation`, so Foundation's category on `NSObject` installs a
handler that the trampoline calls with the saved registers and the
sender's stack arguments. It asks the receiver for
`-methodSignatureForSelector:` (nil means the selector is unrecognized),
makes an invocation whose arguments are read out of the registers as the
signature lays them out, sends `-forwardInvocation:`, and writes the
invocation's return value into the saved registers; the trampoline
restores them and returns to the sender. `NSInvocation` calls the other way
through the same layout: it loads a block of registers and a stack area and
calls through a small assembly routine. The layouts (`call.rs`) follow
Linux's calling conventions: on aarch64, structs of up to four floats or
doubles in floating-point registers, other structs of up to 16 bytes in
integer registers and larger ones by address; on x86_64, structs of up to
16 bytes classified by eightbyte, larger ones copied onto the stack, and a
struct returned in memory through a hidden first argument. A message
forwarded to `-forwardInvocation:` costs about 90 ns and an `-invoke` 17 ns
(Apple's: 390 ns and 72 ns). The trampolines carry unwind tables, so a
panic in `-forwardingTargetForSelector:`, `-forwardInvocation:` or from an
unrecognized selector unwinds through them.

`-methodSignatureForSelector:` also describes the instance methods that
the protocols a class adopts declare, implemented or not, so a forwarder
gets the optional protocol messages nobody implements (a multicast
delegate, say). An invocation told to `-retainArguments` keeps every
object, block copy and C string copy it has held, replaced ones too,
until it goes, as Foundation's does; a forwarded message's invocation
that holds its return value that way is autoreleased rather than freed,
so the sender gets the value alive.

`NSProxy` is a root class of its own, in the runtime beside `NSObject`,
with the same implementations of reference counting and identity. It
forwards everything else, as Apple's does, and its own
`-methodSignatureForSelector:` and `-forwardInvocation:` raise.
`-isKindOfClass:`, `-isMemberOfClass:`, `-respondsToSelector:` and
`-conformsToProtocol:` are methods of its own, as on Apple's, so objc2's
debug-build check that a receiver has a method passes, but their
implementation is a second trampoline that goes straight to
`-forwardInvocation:`, never to a forwarding target.

`+initialize` is sent lazily before a class's first message, superclasses
first, with messages from inside `+initialize` on the same thread allowed
through. `+load` goes to a framework class that implements it when its
static shell loads (Apple's runtime sends it to an image's classes before
`main`); classes made at run time, as `define_class!` makes them, get none
on either runtime. It waits until the shell's loader has returned, since
objc2 registers a `define_class!` type once only and a `+load` using its
class there would wait for itself; superclasses go first, one `+load` at
a time.

## Foundation collections

Apple's collections are class clusters: `NSArray` is abstract and every
instance is some private subclass. Sidestep's are concrete classes under
the public names, and the mutable ones are subclasses of the immutable
ones, as the objc2 bindings require.

- **Storage.** An immutable `NSArray` holds a boxed slice of retained
  elements, `NSIndexSet` sorted ranges with their total, and `NSDictionary`
  and `NSSet` a hash table (`crates/sidestep-foundation/src/table.rs`):
  entries in a vector with their hashes, plus an open-addressed index of
  positions (`hash_index.rs`, shared with the other hashed collections)
  once there are more than four. Nothing in them changes after
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
- **Descriptions.** Arrays and dictionaries nest only arrays and
  dictionaries in their descriptions and quote everything else; sets and
  ordered sets nest any collection that describes itself at a level
  (`-descriptionWithLocale:indent:`), quote strings, and show anything else
  as its description, unquoted, as Foundation's do (`describe.rs`).
- **Ordered sets.** `NSOrderedSet` keeps its members in a `Deque`, which
  is their order, with each member's hash beside it and, past eight
  members, an open-addressed index of positions by hash (`hash_index.rs`),
  so membership and `-indexOfObject:` cost a hash and usually one
  comparison, and fast enumeration hands out the members as an array's. A
  change at either end moves nothing. One in the middle moves the shorter
  side of the members, and the index renumbers that side, finding each
  member through its hash, or rewriting its slots in one pass when they
  are many; positions are stored offset from a base, so renumbering the
  front side moves the base instead. Changes of many members at once
  (`-insertObjects:atIndexes:`, `-replaceObjectsInRange:…`,
  `-moveObjectsAtIndexes:toIndex:`) move and renumber once. So a queue or a
  bulk change costs a fraction of Foundation's, but a single change deep
  in a large set costs more (Foundation's mutable `-indexOfObject:`
  searches instead of indexing). The mutable class follows the arrays'
  design (copy-on-write storage, counted readers, subclasses changed
  through their primitives). `-array` and `-set` answer proxies that read
  the set when asked, so they follow its changes as Foundation's do.
- **Sort descriptors.** Sorting by descriptors reads each element's values
  once, before comparing, rather than twice a comparison. Key paths go
  through `-valueForKey:` where objects answer it (dictionaries, looked up
  without a message when they are Sidestep's), else through the getter
  key-value coding would find, its number or `BOOL` wrapped in an
  `NSNumber`, else through `-valueForUndefinedKey:` where the object
  overrides it; Sidestep has no general key-value coding. How a step reads
  a class's objects is found once per sort, and the getters' selectors
  when the descriptor is made. `NSNull` sorts as nil wherever a key path
  meets it, as in Foundation, and neither reaches a comparator. A column
  of values that are all Sidestep's numbers, sorted by `compare:`, is
  compared without messages.
- **Weak collections.** `NSHashTable`, `NSMapTable` and `NSPointerArray`
  hold each member (or key, or value) as their pointer functions say:
  retained, weakly, or as a bare pointer, compared by `-isEqual:`, by
  address or as an integer (an object held as a bare pointer is still
  handed out as an object, by `-allObjects` and the rest). A weak member
  is a runtime weak reference in a box of its own, so its address
  survives the table moving its entries; when the object deallocates the
  runtime zeroes it and the entry is dead. Lookups, enumeration and
  descriptions pass over dead entries, and `-count` is exact, which costs
  a look at each entry (it reads the weak locations atomically, as the
  runtime writes them, without loading). The table sweeps dead entries
  out, releasing what they held, once per as many changes as it has
  entries. A lookup loads (retains) a weak member
  only when its hash matches, and releases what it loaded after letting
  the table go, since a release may run a `-dealloc` that changes the
  table. Enumerators of weak tables walk a snapshot that keeps the members
  alive, and still fail if the table changes. The write-barrier flags of
  `NSPointerFunctions` are only reported, as in Foundation: setting one
  never changes how a side holds its pointers.
- **Counted sets and caches.** `NSCountedSet` is a subclass of
  `NSMutableSet` whose table keeps each member's count, reached by the
  inherited methods through its primitives. Its set algebra counts, a
  plain set counting each member once: a union adds counts, a difference
  takes them away, an intersection keeps the smaller, and a plain set
  equals a counted set only if every count is one. `NSCache` is safe from
  any thread, its state behind a mutex: entries in a slab threaded on a
  least-recently-used list, evicted when the count or total cost passes
  its limit. Objects leaving are told to the delegate and released after
  the lock is let go, so a delegate may use the cache.

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

## AppKit: a main thread and a render thread

AppKit's contract is single-threaded: events, timers, the responder chain and
`drawRect:` run on the main thread. Sidestep keeps that contract and moves
everything that touches pixels or the display server to a render thread.

- **Drawing records.** Inside `drawRect:`, `-[NSColor setFill]`,
  `NSRectFill`, `-[NSBezierPath fill]`, `-[NSImage drawInRect:]` and
  `-[NSString drawAtPoint:withAttributes:]` append operations (fills,
  paths, strokes, images, runs of shaped glyphs) to a list, in the view's
  layer and clipped to the view and its ancestors (see
  [Drawing](#drawing)). The main thread never rasterizes a window.
- **Layers.** A window's own surface is one layer, and each clip view big
  enough to be worth it whose document overflows it adds one holding its
  document (see [Scroll views and layers](#scroll-views-and-layers)).
  `setNeedsDisplayInRect:` records damage per layer, in layer points.
- **Display.** Once the render thread reports the last frame shown, the
  window places its scroll layers, calls `drawRect:` only for damaged areas,
  sends the operations and presents; a pass that changed nothing presents
  nothing. A window the compositor isn't showing gets no frame callbacks,
  so it stops drawing.
- **Rendering.** The render thread owns the Wayland connection through
  smithay-client-toolkit. It rasterizes operations on the CPU into a cache
  per layer (tiny-skia for paths and images, swash for glyphs), only inside damaged
  rectangles. The window surface is presented from a few shared-memory
  buffers, each remembering what changed since it was last written, so a
  frame copies and damages only changed pixels. A Wayland protocol error
  ends the connection; the render thread then stops, and the main thread,
  seeing its channel close, exits with a message rather than running on
  without windows.
- **Scrolling.** A document layer is cut into tiles of device pixels, each
  on its own subsurface and cropped to the clip view with wp_viewporter.
  Scrolling moves tiles and changes crops; it uploads nothing until a tile
  comes into view. What is painted over a scroll view (overlay scrollers)
  goes on a transparent overlay above its tiles.
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
pixels, kept inside the buffer; a tile keeps a point's margin of pixels
around it, so at integer scales the crop is exactly the pixels that
belong there wherever the layer is scrolled, and at a fractional scale it
can be up to half a pixel from the exact one, which moves the tile by as
much rather than blurring it. A canvas
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
  enter another (the root, a decoration; scroll tiles take no input), so
  the render thread settles crossings
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
  act at once, input becomes `NSEvent`s in the application's queue, behind
  anything waiting there (posted events included). Only AppKit's loops
  take events from the queue, as on macOS: the main loop and modal loops
  send them through `-[NSApplication sendEvent:]` when their run returns,
  a nested `nextEventMatchingMask:…` takes the ones it asked for. A
  program running the loop itself (`runMode:beforeDate:` while it waits
  for something, even inside an event handler) leaves them queued, so no
  handler is entered from inside another. (Tracking areas' callouts are
  the exception: they are made as the move is handled.) The inbox is
  shared by every drain, so a loop nested inside an event handler carries
  on with the messages that came with the event it is handling (a mouse-up
  that arrived with its mouse-down reaches the drag loop the down
  started). Moves followed by another move of the same window are
  coalesced, and key repeats followed by a newer key message are dropped.
- **The display pass**, an observer before the loop sleeps and when a run
  ends, at order 2 000 000 (after programs' own before-waiting
  observers): each window displays if the render thread showed its last
  frame, tracking areas look again at views that moved, and pasteboard
  changes are offered. Registered in the common modes, it runs in modal,
  tracking and terminate-later loops too.

A loop asked to look for events in a mode outside the common modes (a
program's own tracking mode) gets the event source and the display pass
added to that mode the first time, as a CoreFoundation source can be in
several modes (`RunLoop::add_source_mode`); the program's common-mode
timers don't join it.

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
the other observers in registration order. A post costs one atomic load
while the center has no registrations at all; once it has any (a
delegate is enough), a window or application post takes the center's
lock and looks the name up before building anything, which only an
observed name does. Views, which post on every frame change, remember
whether anyone observes the frame and bounds names until the center's
registrations change (a generation count the center keeps), so an
unobserved frame change costs no lock. A
window's frame notifications follow macOS: a new size posts
`NSWindowDidResizeNotification` only, a move alone
`NSWindowDidMoveNotification`; the compositor's resizing state becomes
live resize notifications and `inLiveResize`, a new scale
`NSWindowDidChangeBackingPropertiesNotification` with the old scale in
its user info. `close` posts that the window will close, orders it out
and, for a window released when closed (the default), gives its own
reference to the autorelease pool, as AppKit does; closing it again does
nothing until it is shown again. Whether to quit after
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
for Escape, as Command-period does before anything else; other keys go on
up the chain (to a window controller). A responder at the end of a chain
hears `noResponderFor:`. An action sent to no target goes to the first
that has it among the key window's responder chain and delegate, then
the main window's when that isn't the key window (a panel is key over a
document), then the application and its delegate. The render thread tags the
press that gave its window the keyboard (compositors hand over the
keyboard before the click arrives, so the window is key by then); that
first click reaches a view only if it `acceptsFirstMouse:`. A left press
on a window that moves by its background, in a view that lets it
(`mouseDownCanMoveWindow`: views that aren't opaque), moves the window
through the compositor instead.

**Sheets and modal sessions** (`modal.rs`). Sheets are window-modal and
asynchronous: attached one at a time (others wait their turn), each ended
by `endSheet:returnCode:`, which calls its handler, posts that it ended,
clears the sheet's parent and attaches the next. While a sheet is
attached the parent refuses mouse input and the sheet is key in its place
(the compositor's keyboard stays on the parent's toplevel). On screen a
sheet is part of its parent: a desynchronized subsurface of the parent's
surface with its own canvas, buffers and frame callbacks, placed
top-centre under the title bar and kept there when the parent resizes,
above the parent's scroll tiles (`backend/tiles.rs` stacks tiles
directly above the window's surface, so below its sheets). Its position is the parent's state: a parent's resize
leaves it to the parent's own present at the new size, so a configure
never makes an extra commit, and a sheet made or resized commits the
parent only if the parent shows its current size. A sheet of a window
that isn't on screen shows as a window of its own, and is the parent's
sheet to the program all the same. A modal session is
the modal loop a pass at a time. `NSPanel` floats over the main window
(Wayland has no levels), hides when the application stops being active
(only while another window stays up, as a desktop without a dock couldn't
bring the program back otherwise), may work when modal, and may become
key only when a view needs it to.

**Tooltips** (`tooltip.rs`) are tracking areas of their views, as on
macOS, with one private owner. After the pointer rests for
`NSInitialToolTipDelay` (1000 ms unless the defaults say otherwise; read
once, and again after the defaults change), a borderless window opens as
an ungrabbed popup below the pointer, drawn with the text engine in the
tooltip font; leaving, a click or a key take it down. Moving starts the
wait over: a move only notes its time, and the one timer all waits share,
firing early, is set again for the rest. Each view keeps its own
tooltips' tags, so setting a tooltip costs what that view has; a view
going away takes its tooltips with it. Controls with a tooltip per part
use `tooltip::add_text_rect`.

**Controllers** (`controllers.rs`), without nibs, which Linux doesn't
have. A view controller loads its view when first asked (`loadView`, a
plain view unless overridden, then `viewDidLoad`) and sits in the
responder chain between the view and the view's superview, as on macOS:
the view's `controller` link (in `responder.rs`) names it, and setting
the view's next responder sets the controller's. A window controller
holding a window is its next responder and keeps it (not released when
closed); `setWindowController:` alone chains nothing. No link retains,
and each is cleared before what it names goes: a view going away clears
its subviews' links to it (as `-[NSView dealloc]` does), a controller
letting go of its view or window clears theirs, and a controller given a
view another has takes that one's place in the chain. A window takes the
size of its content view controller's view. Periodic events
(`startPeriodicEventsAfterDelay:withPeriod:`) come from a common-mode
timer that posts one only when none is waiting.

**Frame autosave.** A window with an autosave name saves its frame in the
user defaults whenever it moves or resizes, under macOS's key
(`NSWindow Frame <name>`) and in its form (the numbers, whole ones
without decimals); Sidestep writes the frame without a screen, since it
doesn't know where windows are. A live resize saves once, when it ends,
and a frame already saved (or just read) isn't written again. Taking a
saved frame asks the compositor for its size and keeps its origin as the
one `frame` reports. A name another live window holds can't be taken.

**Testing without a display.** `SIDESTEP_BACKEND=null` (or
`sidestep_appkit::testing::use_null_backend`) starts a render thread that
answers as a compositor would, at once and always the same way, draws
nothing, and writes down what it was asked; the `testing` module plays
the compositor's part for input, adding messages to the main thread's
inbox, and runs the loop until everything was answered and handled.
`crates/sidestep-appkit/tests/linux_events.rs` uses it.
`examples/eventbench` measures the machinery: on the same M-series Mac
(Sidestep under Linux in a VM), an unobserved view frame change costs
about 77 ns against about 910 ns in AppKit, sending a key through a
window to its first responder 14 ns against 170 ns, and an idle pass of
the run loop 174 ns against 650 ns. Taking a posted event back in a
tracking loop costs 48 ns against 33 µs, but that is the queue's fast
path (the event is there, so Sidestep runs no loop, while AppKit's call
runs its loop each time). The path each move of a drag takes, a render
thread message found by the event source in a run of the loop, made an
event and returned by `nextEventMatchingMask:` in the tracking mode, costs
about 310 ns (Linux only: it is measured through the null render
thread).

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
their own (sway). They're light or dark as the desktop prefers, as the
views are (see [Colors and appearance](#colors-and-appearance)).
`SIDESTEP_THEME=dark` or `light` (or a dark `GTK_THEME`) overrides the
desktop for both.

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
main thread keeps what it wrote and, once per turn of the event loop, has
the render thread offer a copy, so a copy that writes several types makes
one selection; the render thread serves other clients' reads through
non-blocking pipes from its event loop, with a marker type so it never
reads its own offer back. Reading can't be synchronous on Wayland, so once
a program has read the pasteboard, the render thread reads another
client's text ahead (up to 1 MiB) whenever it offers a selection, and
`stringForType:` answers from that at once. Text still on its way is
waited for until 200 ms after the selection came, and no longer, so a
client that never answers costs one wait; other types, and longer text,
are read when asked for, as long as data keeps coming and until 200 ms
pass without any (so a large image streams in whole, 6 MB in about 10
ms). Each type is read once per change, and a type that didn't come in
time is remembered as nothing, so a client that never answers costs 200
ms once, not on every menu validation. A counter both threads share plays
`changeCount`; an offer that repeats the last one (compositors offer the
selection again whenever a window gets the keyboard) isn't a change, and
the items and data read of the one before stay good. Setting the
selection takes an input event newer than the current selection's, as a
copy that follows a key press or click has. A promise kept (an owner or
data provider giving what it promised, for this program or another) is
no new selection: other clients reading the one offered are answered
from the main thread, and clipboard managers don't see a copy that
wasn't. Pasteboards made by name live in the process only;
`releaseGlobally` lets go of one's contents and name.

A pasteboard holds items (`NSPasteboardItem`), as on macOS: writing an
object makes an item of it, and the pasteboard's own `setString:forType:`
and the like write to its first item. Reads take the first item that has
the type, except text, which is every item's text a line each. Items are
live (one written to a pasteboard shows later writes, and once the
pasteboard is cleared it's empty and takes no more writes) and belong to
one pasteboard for good. `types` lists every item's types, each followed
by its old name (`NSStringPboardType` after `public.utf8-plain-text`), and
is made once per change. `NSFilenamesPboardType` and the old URL type are
property lists made from the items' URLs, and writing paths as
`NSFilenamesPboardType` writes file URLs. Property lists are kept as
copies made all the way down, as macOS keeps a snapshot. Types that
`declareTypes:owner:`, `addTypes:owner:` (which replaces what the types
held), an item's data provider or an object written with the promised
option promise are asked for when first read: on the reading thread when
the program reads them, and on the main thread, as a message from the
render thread, when another client does. The common types' conformances
are known (`public.html` is `public.text`, `public.file-url` is
`public.url`, `public.png` is `public.image`): `availableTypeFromArray:`
finds, for each type asked for in turn, that type or else the first type
on the pasteboard that is one of its kinds (an item finds nothing until
it's on a pasteboard, as on macOS), and
`canReadItemWithDataConformingToTypes:` uses them too.
`canReadObjectForClasses:options:`, which programs ask when validating
Paste and on every drag update, looks at the types only: nothing is read,
no owner is asked and no object is made. `readObjectsForClasses:options:`
hands a class of the program's its type as data, unless its
`readingOptionsForType:pasteboard:` asks for a string or a property list.

Types travel as MIME types (`pasteboard_types.rs` has the table): text as
`text/plain;charset=utf-8` and its aliases, `public.html` as `text/html`,
images and PDF as theirs, every item's URL in one `text/uri-list` (and
file URLs as `x-special/gnome-copied-files`, which file managers read). A
MIME type is a type of its own, so `image/webp` reads as `image/webp`, and
other types go as `application/x-sidestep-uti.<type>`, so Sidestep
programs exchange anything. Another client's URL list becomes an item per
URL, read (once per copy) the first time the program asks about the
pasteboard's items or types; a drag's comes with the drag. `NSData` is
found by name, so AppKit builds before Foundation has it; until then data
reads as nil.

### Drag and drop

Windows are drag destinations. The render thread turns wl_data_device's
enter, motion, leave and drop, and the offer's source actions, into
messages naming the drag; a drag that offers a URL list is told of once
the list is read (200 ms without data gives up), so the drag pasteboard
has the URLs from the start and the destination is chosen by what they
are. The main thread (`drag.rs`) finds the destination, the deepest view
under the pointer (or one of its superviews) that registered, with
`registerForDraggedTypes:`, a type the drag pasteboard has as
`availableTypeFromArray:` finds it (so a view that takes `public.url`
takes files, and one that takes `public.file-url` doesn't take a link),
else the window, and sends it `draggingEntered:`, `draggingUpdated:` and
`draggingExited:` as it changes. Its answer, masked with the source's
operations, goes back as the offer's accepted MIME type and actions,
passed on to the compositor only when it changes. The render thread
sends one position at a time and keeps the latest until the answer comes,
so a busy main thread sees where the pointer is, not where it was; while
the drag waits, it asks for an update every 50 ms if the destination
wants them (a view does unless its `wantsPeriodicDraggingUpdates` says
NO; a window doesn't unless its delegate says YES), asked once when it
becomes the destination. The main thread answers every message about a
drag exactly once, with the drag's name, so answers for a drag that has
gone are passed over, whatever the order. Destinations may run nested
event loops (a modal panel in `performDragOperation:`), so every step
works on its own drag: a drop ends its drag before the destination hears
of it, and a drag that comes meanwhile is a drag of its own. Crossing
between a window's surfaces (decorations are subsurfaces, and each brings a
new offer) is one drag to the destination. A drop sends
`prepareForDragOperation:`, `performDragOperation:`,
`concludeDragOperation:` and `draggingEnded:`, and is finished (the source
told the data was taken) only if the destination performed it.
smithay-client-toolkit destroys a dropped offer when the next drag
enters, so a drop still being handled then can't be finished, and its
source sees it cancelled. The dragging info's `draggingPasteboard` is the
drag pasteboard, which reads that drag's own offer through the
clipboard's transport, only when asked, until the drop is finished (then
there's nothing to read, and reads give nil), and
`enumerateDraggingItems…` hands out its items as `NSDraggingItem`s of
the classes asked for. Wayland's copy is Copy and its move Move and
Generic; Wayland has no link, so a source's operations never include
Link. NSWindow has every destination method and passes each to its
delegate. Views and windows get the methods from link-time categories
(`SidestepDragging`, the methods of a helper class each), which the
runtime attaches when `NSView` and `NSWindow` register. Drags from our
windows (sources) aren't there yet.

### Screens

`NSScreen`s are the compositor's outputs. The render thread publishes a
snapshot of them as they come, change and go, and the main thread makes
screens of it when a program asks, keeping one object per output. The
first screen is the output at (0, 0), and frames are placed around it with
y going up. A program that asks before showing a window starts the render
thread and waits, once, for the outputs (a round trip or two). Wayland
doesn't say where windows are, only which outputs they're on: a window's
`screen` is the one it entered last and is still on, else the main screen
(the key window's, else the first), and a window the compositor hasn't
configured yet (not yet shown) has its screen's `backingScaleFactor`, as
on macOS. wl_output's scale is a whole number;
an output's mode over its logical size (xdg-output) gives the fractional
one. What the outputs don't say is learned from windows, and kept while
the output keeps its scale and size: a window alone on an output gives it
its fractional scale for certain, and its configure bounds give the work
area that `visibleFrame` is (panels are taken to be at the top). A change
of outputs after the first snapshot calls the application delegate's
`applicationDidChangeScreenParameters:`, whether or not the program has
asked about screens yet, and a window moving to another output its
delegate's `windowDidChangeScreen:`; both are posted to the default
notification center too.
`backingAlignedRect:options:` rounds halfway to the nearest pixel up, as
macOS does (down on y in a flipped rectangle). The first
`NSScreen.screens` takes about 0.5 ms before any window (on macOS, about
36 ms).

### Drawing

Drawing goes through a graphics context (`context.rs`) with a real
graphics state, as a CGContext keeps: the transform (user space to layer
points, any affine), the clip, fill and stroke colors resolved to RGBA,
the compositing operation, antialiasing, image interpolation and the
shadow. `saveGraphicsState` and `restoreGraphicsState` push and pop it.
The current context is per thread, as in AppKit, and drawing reaches its
state through a thread local, with no message sends. A context records
either for the render thread (a window's display pass) or into a bitmap
(`+graphicsContextWithBitmapImageRep:`, `cacheDisplayInRect:toBitmapImageRep:`,
image drawing handlers, `lockFocus`), where each operation is rasterized
at once on the calling thread by the same rasterizer the render thread
uses. That is also how the conformance tests read pixels with no
compositor.

- **Views.** The display pass makes the window's context current and
  gives each `drawRect:` a fresh state: the view's transform, its visible
  part as the clip, black to draw with, and the view's effective
  appearance as the drawing appearance. Saves a view leaves unbalanced
  are dropped. A view with an `alphaValue` below 1 is drawn with its
  subviews into a group that is composited at that opacity; at 0 it isn't
  drawn. (Snapshots with `cacheDisplayInRect:` leave partial opacity out,
  as AppKit's do.)
- **Operations.** A rectangle filled under a transform that keeps it a
  rectangle, with no path in the clip, is clipped on the main thread and
  sent as a plain fill, the fast path. Everything else (paths, strokes,
  images, gradients, rotated rectangles) carries its transform,
  compositing operation, antialiasing, clip rectangle and, for `addClip`
  and `setClip`, the clip's paths, which the rasterizer turns into a mask
  only for the pixels the operation touches. `NSBezierPath` keeps a
  `kurbo` path (bounds, winding, flattening, arcs as AppKit splits them)
  and makes the tiny-skia path once, shared by every operation drawing
  it. Text under a path clip is drawn into a group the clip masks.
- **Rasterizing.** Canvases hold premultiplied RGBA, tiny-skia's format,
  which buffers show as they are where the compositor takes XBGR8888
  (wlroots, Mutter and KWin do; the formats are read once they've
  arrived, at the first buffer), and swizzled to XRGB8888 in the same
  copy otherwise. Each operation draws into a scratch copy of just the
  pixels it can reach, so a clip rectangle needs no mask; a clip path's
  mask is kept while the next operations ask for the same one. The 29
  compositing operations map onto tiny-skia's blend modes, except
  `PlusDarker`, done by hand; for the modes where a transparent source
  still changes the destination (`Copy`, `SourceIn`, …) a clip mask is
  applied by mixing afterwards, as tiny-skia's masks would clear what they
  leave out. Groups draw into a transparent layer the size of what their
  operations reach. A shadow is the operation's coverage blurred by three
  box blurs (a Gaussian of half the radius), moved by the offset in base
  coordinates (up is up, flipped or not) and drawn under it; the coverage
  is taken wherever it can reach the pixels being drawn, so a shadow
  doesn't change with how the damage was cut or crosses tiles. Gradients are
  tiny-skia shaders, two-point conical for the radial ones; where the
  options don't extend them, the filled shape is the band they cover.
- **Images.** An `NSImage` holds representations and draws the smallest
  bitmap whose pixels cover the destination in device pixels. Files are
  read with the pure-Rust codecs of the `image` crate (PNG, JPEG, GIF,
  WebP, BMP, TIFF, ICO): opening one reads only its header (size, alpha,
  EXIF orientation, density for the size in points), and the pixels are
  decoded on the render thread the first time the image is drawn in a
  window, or when a program asks for them. A drawn bitmap is an `Arc`
  snapshot of its pixels, made again only after something could have
  written them (`bitmapData`, `setColor:atX:y:`, drawing into it). Each
  thread that draws images keeps a cache: the snapshots themselves
  (shared, not copied) or decoded files, halvings made the first time an
  image is drawn below half size, and a few tinted copies for templates,
  dropped least recently used past 256 MiB (`SIDESTEP_IMAGE_CACHE_MB`)
  and when their representation goes away; the main loop tells the render
  thread which went away once a turn, after that turn's paints. A
  drawing handler's image is drawn into a bitmap as many pixels as the
  destination, kept per size and appearance. Bitmaps whose bytes wouldn't
  fit in memory, asked for or claimed by a file, are refused, as AppKit
  refuses them. `NSImage` isn't shared between threads, so neither are
  the names `setName:` registers: each thread has its own. An animated
  GIF's bitmap lists its frames from the file's blocks without decoding
  them (`NSImageFrameCount`, the durations, the loop count, which counts
  the first play as macOS does) and decodes a frame, composited as it
  shows, when it's set (`NSImageCurrentFrame`), which then becomes its
  pixels: a decoder kept on the bitmap brings the frames one at a time,
  and starts again from the first to go back, so only the frame showing
  and the decoder's canvas are held. Images go on
  pasteboards as TIFF and come off them from image data or an image
  file's URL (`NSPasteboardReading`/`Writing`).
- **Symbols.** `imageWithSystemSymbolName:` maps about a hundred common
  symbol names to Sidestep's own line drawings, template images drawn by
  a handler, sized and weighted by an `NSImageSymbolConfiguration`. No SF
  Symbols artwork is used; other names give nil.
- **Animation.** Nothing animates yet: `animator` is the view or window
  itself, so changes apply at once, and `NSAnimationContext` keeps its
  settings per group and runs completion handlers from the run loop once
  a group's duration has passed.

### Colors and appearance

`NSColor` is one immutable class: components in a color space, a system
color, a dynamic color (a provider block) or a pattern. A color is turned
into sRGB when it's set or converted, in the current drawing appearance:
the innermost `performAsCurrentDrawingAppearance:`, else the view being
drawn, else the application's. System colors come from Sidestep's own
palette (`palette.rs`), made for GNOME and KDE desktops rather than read
off macOS, in light, dark and high-contrast versions; the accent comes
from the desktop and the selection colors from the accent. There is no
color management: device, calibrated and generic RGB are sRGB, Display P3
converts through its matrix, and gray is RGB's luminance weighed in
linear light. Components are kept as given, beyond 0 to 1 too, and
clamped when drawn or converted into a space that isn't extended. A
system effect (`colorWithSystemEffect:`) on any color depends on the
appearance, so it's worked out each time the color is used, as macOS
works it out (measured): disabled fades the alpha (to 35% in a light
appearance, half in a dark one); pressed, deep-pressed and rollover add
a step to the color taken premultiplied, darkening in a light appearance
and lightening in a dark one, in whole 255ths.

A view's effective appearance is its own, its superview's, its window's
or the application's, which follows the desktop's. Views cache it; a
change of any of them (or a view moving) walks the views below, telling
each whose appearance changed with `viewDidChangeEffectiveAppearance` and
redrawing it.

The desktop's settings come from one thread, `sidestep-settings`, which
xdg-desktop-portal's Settings interface answers over the session bus (a
minimal D-Bus client, `desktop.rs`, rather than a D-Bus crate and its
async runtime): `color-scheme`, `accent-color` and `contrast` under
`org.freedesktop.appearance`, and GNOME's text scale, fonts, cursor size
and animation setting under `org.gnome.desktop.interface`. It starts with
the shared application, alongside the Wayland connection, and follows
changes, waking the main thread, which redraws every window once. A
window's first frame waits, the event loop running, until the desktop has
answered or 150 ms have passed, so a dark desktop doesn't flash a light
window; without a bus or a portal the answer is light at once.
`SIDESTEP_APPEARANCE=light` or `dark` (else `SIDESTEP_THEME`) and
`SIDESTEP_ACCENT=#rrggbb` override the desktop.


### Views, layout and containers

A view's subviews are a shared, reference-counted list: Sidestep's own
code takes a snapshot of it for the cost of a reference count, and a change
copies the list only while such a snapshot is still held, so iterating
while callbacks rearrange views is safe. `-subviews` gives programs a new
NSArray, as AppKit does. Moving a view sends AppKit's messages in AppKit's
order: `viewWillMoveToSuperview:`, the old superview's
`willRemoveSubview:`, `didAddSubview:`, `viewDidHide` when it joins a
hidden branch, `viewDidMoveToSuperview`, then `viewWillMoveToWindow:` and
`viewDidMoveToWindow` down the subtree. Every callback is program code
that may move views itself, so a move reads the view's place again after
each one: the view leaves whatever superview it has by then, and each view
joins the window its place puts it in once its own callback has run. A
window that is going away detaches its views without telling them.
`setBoundsSize:` is stored and reported by `bounds`, but drawing, hit
testing and conversion don't scale yet (so a scroll view's magnification is
kept, scales its clip view's bounds as AppKit's does, and changes nothing
drawn). The scrolling helpers (`scrollRectToVisible:`, `autoscroll:`) work
through the enclosing clip views.

**The layout pass.** Each view has a few flags: it needs layout, its
constraints need updating, and "some view below does" for each, raised
along its ancestors when set. Before a window draws, the pass walks only
flagged branches from the content view, once a round: `updateConstraints`
bottom up, the constraint solver, `layout` top down. A view's own request
made during its `layout` is dropped, as on macOS, so it is laid out once
a pass; requests for other views (a subview's `layout` flagging its
superview, views added during layout) make another round, at most sixteen,
so a pass costs at most sixteen walks of the flagged views. Then
`viewWillDraw` if something will be drawn, and a round more if that asked
for layout. A window where nothing is flagged pays one flag test a frame.
`layoutIfNeeded`, `layoutSubtreeIfNeeded` and `display` run the same pass
on demand, in windows that aren't shown too. Autoresizing happens as a
frame changes, not in the pass.

**Auto Layout** solves with kasuari, a Cassowary solver in Rust. One
engine serves each tree of views taking part: it is made on the first
layout that needs it, for the topmost view whose subtree has constraints
or stack views (the content view in a window), kept in that view, and
updated as constraints are activated, deactivated or changed, and as
views move: a subtree leaving takes its variables and the constraints
installed in it out, one joining puts its own in, so a move costs what the
moved subtree holds. Only subtrees flagged as having taken part are looked
at, so views that never used Auto Layout cost it nothing as they move.
Each view and layout guide has four variables: its alignment rectangle's x
and y (downward, from its superview's frame origin), width and height. An
anchor is a sum of these along the path to the two items' common ancestor,
so a view that moves doesn't rewrite anyone's constraints.

AppKit satisfies priorities strongest first: any number of constraints at
250 give way to one at 251. Cassowary weighs errors instead, so each
engine ranks the optional priorities it has seen and spreads the ranks'
strengths evenly over seven powers of ten: with k priorities in use, each
outweighs the next one down 10^(7/k) times, whatever their values. That is
AppKit's order for the handful of priorities a window uses, though not
for any number of them; kasuari loses optima (at random, as its tables are
hashed) once optional strengths reach 10^8 beside small ones, which caps
the span. A new priority reweighs the optional constraints once before the
next solve; priorities stay ranked once seen, so views coming and going
don't reweigh anything. A required constraint that can't be satisfied is
added at 999 instead, with a message, as AppKit breaks one.

Views that translate their autoresizing masks get constraints that share
their superview's size among margins and size as the mask does, so
autoresizing a view already gives the frame its constraints do and doesn't
make them again; views with an intrinsic size get hugging and compression
resistance. A window's content view holds its size at priority 500, as
AppKit's window does, so stronger constraints resize the window (the
engine asks `setContentSize:` once for each size the content needs); a
tree outside a window keeps its root's frame, or, when the root doesn't
translate its mask, lets its constraints size it. Frames come out with
their edges on the window's pixels (whole points outside a window).
`fittingSize` solves a separate engine that pulls the root's size to zero
at priority 50, and ambiguity is found by pulling a variable with an edit
constraint and seeing whether it moves. `NSStackView` makes its
constraints inside the engine rather than as `NSLayoutConstraint`s, so
they aren't in its `constraints`.

Views own what points at them, and clear it when they go: a constraint
installed on a view that is deallocated becomes inactive, a layout guide
loses its owning view, and anchors, which a view keeps one per attribute
(so the same anchor object comes back, as in AppKit), name their view
weakly. Each view also keeps the constraints naming it, so a view leaving
its superview finds the constraints that cross its subtree's edge in the
subtree itself; those go unless the view they are installed on still
holds the view where it goes, as in AppKit.

**Split views** place their panes by frames: resizing shares the change
among panes in proportion to their sizes (holding priorities are kept but
don't yet change that), the delegate constrains positions and collapsing, dividers are dragged from
the mouse events themselves (no nested loop), and `autosaveName` keeps
the frames in `NSUserDefaults` under AppKit's key and in its format.
**Tab views** inset their content by tab type as AppKit does and draw
their tabs straddling the bezel's edge, hit-tested where AppKit's are.
Items point back to their tab view weakly; one added to another tab view
leaves the first.

**Tables** are view-based. Their geometry follows the style, as measured
on macOS: the default (automatic) style is the inset one, which pads the
outer edges of the first and last columns by 6 points and insets the
columns by 10 and the rows by 5 above and below; the full-width style has
just the padding, the source list more room above the rows, the plain
style none of it. Row heights sit in a Fenwick tree (uniform heights are
plain arithmetic), so a row's rectangle and the rows in a rectangle take
logarithmic time whatever the row count, and inserting, removing or
moving rows keeps the others' heights, asking the delegate about new rows
only. `layout` gives views only to rows near the visible ones: those in the
visible rectangle, and in a shown window those within a tile of it (the
tiles its scroll view draws ahead), keeping rows two beyond the edge so
small scrolls back and forth make nothing. Rows that leave give their cell
views to pools by identifier, where `makeViewWithIdentifier:owner:` finds
them after `prepareForReuse`. The table follows its clip view through a
hook in the clip view's bounds change, not a notification. Rows carry the
selection with them as they come and go, and show it strongly while the
table is the first responder of the key window (the window tells it,
through a hook, when that changes). Columns point back to their table
weakly.

Scrolling a million-row table a page at a time and laying it out takes
8 µs (118 µs on macOS). Appending a row to 10,000 whose delegate sizes
them takes 34 µs (31 µs). Solving 1200 constraints in a row of 300 views
first takes 15 ms (22 ms), and changing one constant 0.9 ms (0.3 ms), as
kasuari has no way to change a constraint's constant in place and the
constraint is removed and added again. Resizing a window holding 200
autoresizing views, each with a view pinned inside by four constraints,
takes 0.4 ms (1.0 ms). A page of table cells with constraints, in a window
holding 1,200 other constraints, takes 5.1 ms (0.16 ms): each cell's
constraints leave and join the one solver, and kasuari's removal looks
through all of its rows.

### Scroll views and layers

`NSClipView`, `NSScrollView` (`scroll.rs`) and `NSScroller` (`scroller.rs`)
follow AppKit as `conformance/tests/appkit_scroll.rs` measures it.
`scrollToPoint:` moves a clip view's bounds where it is told and posts
the bounds notification; `setBoundsOrigin:` constrains first
(`constrainBoundsRect:`, which subclasses override to center a document
or keep it in pages), moves, and has the superview
`reflectScrolledClipView:`. The document can scroll past its edges by the
content insets, and a clip view that changes size constrains again. A
clip view follows its document's frame as AppKit's does by observing the
document's frame notification, through a hook rather than the
notification center: not while the document posts none (it catches up
when posting is turned back on), through `viewFrameChanged:` when a
subclass overrides it, constraining again, moving through the superview's
`scrollClipView:toPoint:` when it must, and reflecting. A document that
leaves its clip view (removed, moved elsewhere, or replaced by none) is no
longer its document, and the bounds origin stays where its corner was. A
scroll view's
`tile` lays its clip view and scrollers out from the border (1 point, 2 for
a groove), the scroller style (legacy scrollers take room, overlay ones
sit over the clip view, full length) and the content and scroller insets;
`reflectScrolledClipView:` gives the scrollers values and proportions over
the document and its insets, enables them while there's somewhere to go,
and shows or hides them when they hide automatically. A new scroll view
takes `+[NSScroller preferredScrollerStyle]`: overlay, unless
`SIDESTEP_SCROLLER_STYLE=legacy`. A wheel moves by lines of
`verticalLineScroll` (10 points; a detent is three lines), a touchpad by
points, along the predominant axis; an event along no axis the scroll view
can move goes to the next responder, so a scroll view inside another hands
it what it can't use. Each move posts `NSScrollViewDidLiveScrollNotification`
(before the scrollers follow), a gesture's start and end (after its
momentum) the will-start and did-end ones, and so does a press on a
scroller that drags its knob or pages, from the press to its release; a
wheel's moves aren't bracketed, as on macOS once a scroll view has
scrolled once. Magnifying scales the clip view's bounds about a point
that keeps its place (the middle, the pointer, or the one given), and
`magnifyToFitRect:` centers the rectangle; drawing doesn't follow the
scale yet. A scroller keeps its own value and proportion, so moving
redraws only the stretch its knob left and reached; its knob is dragged
and its slot paged from the mouse events (a held page repeats on a
timer), with no nested loop; overlay scrollers show while their view
scrolls, fade a second later on one timer every scroller shares, and widen
while the pointer is over them.

**Promotion** (`layers.rs`, main thread). At each display pass, a clip
view in the window gets a layer when nothing above it is hidden or faded,
its visible part is at least 64 device pixels each way, its document
overflows it, and the window has fewer than 24 layers. A clip view inside
a promoted one's document is nested in its layer; one without a layer
draws its document inline, and scrolling it redraws its visible part.
Gaining or losing a layer redraws the clip view's place in the layer it
sits in. `views::placement` stops at the nearest promoted clip view, so
invalidation lands in the right layer.

**Layer coordinates** are the clip view's bounds coordinates with y turned
down for an unflipped document: a flipped document grows downward from row
0, an unflipped one into negative rows, so a growing document never moves
pixels already drawn, and scrolling only moves the layer's origin in the
window (snapped to device pixels). A document inside a layer that grows or
shrinks from its origin redraws only what it gains when its
`layerContentsRedrawPolicy` is `OnSetNeedsDisplay` or `Never`, as a
layer-backed view's contents would stay; its clip view, drawn behind the
tiles, isn't redrawn. As on macOS, `OnSetNeedsDisplay` is the default
only for views without a `drawRect:` of their own: one that draws gets
`DuringViewResize` and is drawn whole again, unless it asks otherwise (a
log that only grows at its end should).

**Tiles** are 512 device pixels tall and as wide as the layer in steps of
64 pixels, up to 2048, keyed by column and row (`protocol::TileGrid`),
each with a margin of a point's pixels. A pass draws the tiles coming into
view and the damaged parts of those the render thread keeps (every tile a
damaged rectangle reaches, margins included) and presents; then, while it
has spent less than 4 ms, it draws one tile ahead in the direction the
layer moved (downward when it hasn't), after `prepareContentInRect:` to
the document, which the render thread rasterizes while the compositor
shows the frame. Tiles more
than two tiles from the viewport go, and the farthest from their viewports
while a window's tiles hold more than 96 MB. An opaque clip view
background makes the layer opaque: its tiles are cleared to that color and
shown without alpha. Otherwise tiles are cleared to nothing and shown with
alpha over what the window's surface drew there (the clip view's own
background, what's behind the scroll view).

**Overlays.** Views painted after a promoted clip view that reach its
viewport would be hidden by its tiles. The pass walks the views after the
clip view in paint order (in the layer it sits in), only through those
reaching the viewport or what the overlay has taken so far, and into views
that draw nothing of their own; those it takes are drawn into the layer's
overlay, a transparent surface above the layer's tiles and the layers
nested in it, and are left out of the layer below (a flag on the view), so
a translucent one isn't drawn twice. The overlay is the size of their
frames in the layer they are drawn in (the one this layer is nested in, or
the window's surface) and kept in that layer's points, so scrolling that
layer moves the overlay and redraws nothing; one far bigger than the
window is cut to what that layer shows. Damage to them goes to the overlay
only; the overlay is also drawn where the layer below it was damaged, and
whole when its rectangle or views change. Stacking is paint order: a
layer's tiles, the layers nested in it, its overlay.

A pass runs program code (`drawRect:`, `prepareContentInRect:`, colors),
which may order the window out, see its scale change, take clip views out
or display another window. The window's layers are out of it during the
pass, behind a stand-in that notes what happens to them, and the pass
applies that when it puts them back; a window asked to display during its
own pass does so at the next, and another window displays at once.

**On the render thread** (`backend/tiles.rs`) a layer is its placement
(`ToRender::PlaceLayer`, sent only when it changes: stacking, viewport,
origin, extent, grid, opacity, the layer it is nested in, overlay), its
tiles' canvases, and surface sets (surface, subsurface, viewport) while
tiles are on screen. Every set has an empty input region, so pointer input
lands on the window's surface in its own coordinates; sets taken off
screen go to a pool. Only the main thread decides which tiles exist: a
paint makes a tile only when it covers the whole tile with its margin,
which is how the main thread draws a tile it starts keeping, and a paint
reaching into the margin of a tile nobody keeps leaves it out. So a tile
keeps its canvas exactly as long as the main thread keeps the tile (a
dropped tile's canvas serves the next new one), and the memory cap counts
every canvas. A tile has buffers only while it is on screen: two, each
remembering the rectangles it misses (as the window's own buffers do), so
a change uploads only its pixels. An overlay is placed and cropped from
the origin and viewport of the layer it sits in, with a point's margin of
pixels like a tile's. A present commits a tile or overlay only when its
content or crop changed (positions belong to the window surface's commit),
restacks only when the surfaces on screen or their order changed, and asks
for a frame callback on the window's surface and on one other surface: the
first tile or overlay to commit, else the biggest tile on screen of a
layer nested in no other, committed for it. So a fling uploads nothing
until a tile comes into view, scrolling a scroll view of scroll views
uploads nothing to their overlays, appending a line to a log pinned to its
end uploads the line, and a caret blinking in a scroll view commits two
surfaces. `SIDESTEP_TRACE_FRAMES=1` prints, for each pass, the main
thread's time and the tiles it recorded, and for each present the bytes
uploaded to tiles, overlays and the window and the surfaces committed.

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
  otherwise give them plain glyphs. A tab goes to the first of the
  paragraph's tab stops beyond it, in the order they were set (by default
  twelve, 28 points apart): text after a left stop starts there, after a
  right stop ends there, after a centered one centers on it, and after a
  decimal stop has its decimal point centered on it (a number without one
  ends there). Past the last stop, stops follow it every
  `defaultTabInterval`, or a tab takes no room. Control characters take no
  room. A baseline offset adds its points, unrounded, above or below the
  line's rounded ascent and descent. U+2028 and NEL end lines within a
  paragraph (no paragraph spacing, as AppKit measures NEL), and a
  paragraph cut by them keeps one base direction. Spaces at the end of
  wrapped text hang past the width: parley hangs only the first and
  starts a line after it, so a text ending in more spaces than fit is
  broken again with room for them on the line before. parley shapes at
  most 4 KiB as one run, cut after whitespace: it counts a cluster's
  place in its run in 16 bits, and walks a run's glyphs from its start
  for each change of style in it.
- **Attributes.** Beyond fonts, colors, backgrounds, kerning, ligatures
  and baseline offsets: underlines and strikethroughs single, thick or
  double, solid or dotted and dashed, under whole runs or only under words,
  placed by the text's own font so that they run straight through
  fallback faces (emoji, other scripts); `NSStrokeWidth` (outlines alone
  when positive, outlines over the fill when negative, in
  `NSStrokeColor`); `NSObliqueness`; and `NSShadow` (any object whose
  `shadowOffset`, `shadowBlurRadius` and `shadowColor` have its
  signatures, checked before they are sent), drawn as the glyphs offset
  in the shadow's color underneath, without blur until the rasterizer has
  a blurred glyph op; a shadow whose color is nil draws nothing, as in
  AppKit. Strokes and slants are part of the face a glyph run names (the
  registry keeps synthesized bold, slant and stroke with the font file,
  slants in tenths of a degree and strokes in thousandths of the size, so
  a font has a bounded number of faces however an app varies them), so the
  render thread draws them with swash and the glyph-run op didn't
  change. `NSExpansion` isn't drawn: it
  scales advances, which line breaking would have to know about.
- **Lines for TextKit.** `text/lines.rs` lays text out as a layout
  manager needs it, on any thread: the input is the text, attributes and
  runs of them over UTF-16 units (as `NSString` counts), and the output,
  plain data behind `Arc`s, is lines with their UTF-16 ranges, baseline,
  ascent, descent, leading and widths, clusters in visual order with
  their positions and directions, and glyph runs to record as they are.
  A paragraph can be laid out from any of its lines and a few lines at a
  time; a long one is then shaped only in a window of its text as long as
  those lines need (the last line in the window, which might go on past
  it, is dropped), and only the attribute runs in the window are looked
  at. Its base direction is found once for the whole paragraph, and text
  laid out from inside a paragraph starts with an invisible mark standing
  for the last strong character before it, which the bidi algorithm
  resolves numbers, neutrals and brackets at its start by, so a paragraph
  laid out from one of its lines gets the lines it would have had. A
  `Frame` stacks a text's paragraphs and answers what a layout manager is
  asked: the character at a point (the spacing before a paragraph is its
  own, as in AppKit), the caret at an index (where directions meet, at
  the side running the paragraph's way, with the other edge as a
  secondary caret, as AppKit's layout manager does), the rectangles of a
  selection (split where directions mix) and the line fragment of an
  index. After an edit it lays out again from the line before the edit
  (or further back, past lines ending inside a word too long for a line
  or holding nothing strong) until a line starts where an old one did,
  after the same strong character, and keeps the rest; an edit that makes
  or joins paragraphs keeps the old lines on both sides the same way. In a
  paragraph that mixes directions and has brackets or explicit directions,
  which the bidi algorithm resolves across any distance, an edit that
  takes text out lays the paragraph out whole. Offsets in a line are
  relative to its paragraph and a cluster's to its line, and lines share
  their clusters and glyphs, so moving or copying them costs nothing per
  cluster. In a 78 KB paragraph 600 points wide, a key typed costs about
  0.08 ms (0.12 ms with an attribute run a word, 0.09 ms while a snapshot
  shares the frame), Return 1.8 ms, and laying out 20 lines from its
  middle 0.28 ms; a key typed in an ordinary paragraph costs about 25 µs,
  and recording a page of laid-out lines under a microsecond
  (`text/bench.rs`, Linux in a VM on an M-series Mac).
- **Caches.** Laid-out text is cached per thread by string, attributes and
  options (two generations of 2048 entries or 4 MB), so a view that
  redraws the same lines records them for about 0.1 µs a line; drawing
  borrows the cached layout rather than counting a reference. Glyph runs
  keep their glyphs in an `Arc`, so recording a cached line copies no
  glyphs, and layouts are `Send` and `Sync`. Paragraph styles own their
  tab stops, so nothing outlives the styles and layouts that use them.
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
  from a link-time category (`NSStringDrawing`, the methods of a helper
  class), which the runtime attaches when `NSString` registers. So a
  program may measure or draw a string as its very first AppKit call
  (`conformance/tests/text_first_call.rs`).

- **Text editing.** TextKit 1 and the text view live in
  `crates/sidestep-appkit/src/textkit/`, on the lines above: a text
  storage keeps its text as a tree of paragraphs with interned attribute
  runs; a layout manager keeps an entry per paragraph (its lines, or an
  estimate of its height, in chunks with lazily summed heights), lays out
  a paragraph at a time as questions need it and, for text a view shows,
  the rest when the main run loop is idle, places paragraphs in text
  blocks and table rows, and tells its views what to draw again (a
  paragraph, or from it down when it moved what follows); the text view
  runs AppKit's edit transactions, commands, input methods and undo over
  them. [text.md](text.md) has the details.

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

## Controls

`crates/sidestep-appkit/src/controls/` has the controls and
`crates/sidestep-appkit/src/theme/` draws them.

- **Cells are real.** As in AppKit, a control makes its cell from
  `+cellClass` (each control class registers its own), forwards its
  properties to it by message and draws and tracks through it, so a
  program's subclass of either, or a replaced cell, behaves as on macOS.
  A cell keeps its value as it was set (a string, an attributed string, a
  number of one of four kinds, or an object) and converts it when read: a
  number reads as `NSNumber` describes itself in the current locale (so
  Foundation decides "1,234.5"), and the cell keeps that string until the
  value changes; a plain value read as an attributed string carries the
  cell's font, color and paragraph settings. Only text cells take
  numbers. Cells copy (`copyWithZone:`), each class copying its own
  settings. Setters that change only a cell's look call `updateCellInside:`
  and keep the control's measured size; setters that can change its size
  call `updateCell:`; setting what's already there does neither.
  Everything a subclass may override is reached by message; the rest is
  plain Rust.
- **Tracking.** `-[NSControl mouseDown:]` and `-[NSCell
  trackMouse:inRect:ofView:untilMouseUp:]` run AppKit's loop in AppKit's
  order of calls, which `conformance/tests/control_events.rs` records on
  macOS, taking events from `nextEventMatchingMask:…` in the event-tracking
  run loop mode, so default-mode timers wait. What sends the action is the
  cell's `sendActionOn:` mask, which also says whether the cell is
  continuous (the periodic bit; a slider's drag bit), as on macOS. A
  periodic event is a wait that times out; a delay too long to be a time
  means no repeats. With no window on screen, a loop takes the events
  already posted and ends. Segmented controls, steppers, sliders and
  switches have loops of their own; macOS 26 tracks the real mouse for
  them, so their tests run on Linux only.
- **Geometry** is Apple's: every size and rectangle a program can read is
  measured by `conformance/tests/controls.rs` (every button bezel at every
  control size among them), and the constants are in `theme/metrics.rs`.
  Sizes that depend on text are formulas over the text's measured size,
  since the fonts differ. Controls cache their intrinsic size until their
  cell's size may have changed; a cell's generation, bumped by such
  changes, lets buttons keep their title's measurement and segmented
  controls their segments' widths.
- **Drawing.** `drawRect:` records, as every view's does; there is no
  widget op. `theme/parts.rs` has a painter per part (a bezel, a check
  box, a knob, a focus ring) that takes the rectangle AppKit's geometry
  gives and the part's state; `theme/paint.rs` turns them into fills and
  antialiased polygons (rounded rectangles with a radius per corner,
  ellipses, strokes as rings, arcs) in view coordinates, whichever way the
  view is flipped. The colors (`theme/palette.rs`) are Adwaita's, light or
  dark as `SIDESTEP_APPEARANCE` says, else as the decorations are told
  (`SIDESTEP_THEME`); following the desktop waits for `NSAppearance`.
  The painters take plain values and make no objects; the cells that call
  them draw from the measurements they keep, and a button makes its title
  into an `NSAttributedString` only for a subclass that overrides
  `drawTitle:withFrame:inView:`. `bench_controls_drawing`
  (`controls/bench.rs`) times a window's worth.
  `theme/golden/` holds reference images of every part, which a unit test
  compares (`SIDESTEP_BLESS=1` rewrites them).
- **Animation.** Progress indicators animate on the window's frame
  callbacks: each frame redraws the animating indeterminate indicators in
  that window, their phase taken from the clock (a determinate one shows
  its value, which the clock doesn't change). With no frames (the window
  hidden, covered or gone), nothing runs; there are no timers.
- **Focus.** The window draws the focus ring after the first responder's
  subtree, clipped to what its parent shows, and damages the ring's
  outset when focus moves. Full keyboard access is on, as on GNOME
  (`SIDESTEP_FULL_KEYBOARD_ACCESS=0` turns it off); a click never focuses a
  button-like control. Space presses the focused control, arrows move
  radio groups (buttons with an action; without one they're on their
  own) and segments, and step sliders and steppers through the action
  methods key bindings send (`moveUp:`, `pageDown:` …); Return presses the
  default button and Escape the button whose key equivalent it is. Key
  equivalents match Control, Option and Command exactly and ignore Shift,
  which the characters carry. Keys nothing takes go up the responder
  chain from the window.
- **Images in buttons** (`controls/button_layout.rs`) are laid out by a
  pure function of the bezel's family, the control size, the image's and
  title's sizes, the position, `imageScaling` and `imageHugsTitle`,
  measured on macOS for thousands of cases and checked against an oracle
  in `conformance/tests/control_images.rs` (which runs against AppKit on
  macOS). Rounded bezels round each edge to whole points; square bezels
  and borderless buttons don't; a disclosure button lays the image out in
  its 13-point square. A cell's alternate image shows with its
  alternate contents (on and showing state by contents, or highlighted
  and highlighting by them, not both). A template draws in the title's
  color on a bordered bezel; on a textured or toolbar one in the accent,
  shaded by state as measured (or the label's color when the button shows
  it's on by its bezel), and on a badge in the secondary label color; on
  a borderless one in the content tint (with the pressed system effect
  while pressed) or the label colors. A symbol configuration is applied
  to a symbol image once per image (`image_view::SymbolCache`, shared
  with image views), and the result kept until the image changes.
- **Image views** (`controls/image_view.rs`): `NSImageCell` places the
  image in its frame's drawing rect as it draws (its `imageRectForBounds:`
  is the bounds, as AppKit's is), rounding the origin to whole points;
  frames are the theme's (`parts::image_frame`). Templates take the
  view's `contentTintColor`, else the secondary label color from the
  system colors, white on an emphasized background. The view keeps the
  target and action (an image cell has none), registers the image drag
  types (whether editable or not), and while editable (enabled or not)
  takes drops of image files and image data from sources that allow a
  copy (the image is taken when the drop concludes), and, when it
  `allowsCutCopyPaste`, pastes, cuts and Delete, sending its action. An
  animated image steps through its frames on one-shot timers in the
  common modes while the view `animates`, in a window or not, as AppKit's
  does (`controls/image_animation.rs`); nothing runs between frames.
  `+imageViewWithImage:` is added by hand, as a category, so it can make
  an instance of the subclass it's sent to.
- **Text fields** display, size, truncate and draw placeholders; the
  editing entry points (`currentEditor`, `editWithFrame:…`,
  `selectWithFrame:…`, `selectText:`, `endEditing:`) are hooks in
  `text_field.rs` for the field editor to fill. The field already turns
  `textDidBeginEditing:` and the rest into one
  `NSControlTextDid…Notification` (the field editor, and at the end the
  text movement, in its user info) for its delegate and then the
  notification center, holding no borrow while they run.
- **Accessibility** is a store (`controls/a11y.rs`): views and cells keep
  what `setAccessibility…:` sets, nil included, and answer macOS's
  default for their class until then (as on macOS, a control's cell is
  the element: a button's cell has the button role and the title as its
  label; help set on a control is its cell's too, and a label set on a
  button leaves its cell's empty). Records are keyed by object and
  dropped with it, with fields that map onto AccessKit's node properties,
  for an adapter to read.

## Menus, alerts and panels

`crates/sidestep-appkit/src/` has the menu model (`menu.rs`), key
equivalents (`keyequiv.rs`), menus on screen (`menu_view.rs`,
`menu_tracking.rs`), the menu bar (`menubar.rs`, `backend/menubar.rs`),
`NSPopUpButton` (`popup_button.rs`), `NSAlert` (`alert.rs`), the file
panels and the workspace (`panels.rs`, `workspace.rs`, `portal.rs`), and
what menus add to the responder classes (`category.rs`). Each file's
module comment has the detail; `conformance/tests/menus.rs`,
`popup_button.rs`, `alert.rs` and `panels.rs` pin what macOS does,
`crates/sidestep-appkit/tests/menus.rs` drives menus with the testing
module's input on the null backend, and `tests/panels.rs` runs panels and
the workspace against stand-ins for the desktop's programs.

- **The model.** A menu retains its items and an item its submenu; the
  links back (an item's `menu`, a submenu's `supermenu`, targets and
  delegates) don't retain, and a menu clears them when it lets an item go
  or is freed. Each menu counts a generation up whenever something that
  shows changes; its layout is kept until the generation moves, and a menu
  on screen is laid out again at once. The bar is drawn again only when
  the main menu or one of its menus' titles changes. Add, remove and
  change notifications are built only when someone observes.
- **Validation and key equivalents.** `-[NSMenu update]` finds each
  item's target through `-[NSApplication targetForAction:to:from:]` and
  asks it (`validateMenuItem:`, then `validateUserInterfaceItem:`). The
  application and windows answer as AppKit's do (a window closes, zooms
  or miniaturizes from the menu only when its style allows, goes full
  screen when its collection behaviour says so, and has no toolbar or
  tabs; the application arranges windows only when one shows); views
  have no validators. `NSApp.menu` is the main menu.
  `performKeyEquivalent:` updates by message, then offers the key depth
  first; a disabled match takes the key and does nothing. In the
  key-equivalent phase of `sendEvent:` the application asks the key
  window's views first and then the main menu, which is offered keys with
  Command or Control and function keys.
  An item's key equivalent becomes a `Shortcut` when set, so matching
  sends no message per item. The matching rule is Linux's own: Command,
  Option and Control must equal the mask; Shift must agree, except that a
  shifted key equivalent (`W`, `!`) matches with Shift pressed, and a mask
  with Shift matches the unshifted key (`z` with Command-Shift is the `Z`
  that Command-Shift-Z types). Labels name the keys as the platform maps
  them: Command is Super, Option is Alt (Super+Q, Ctrl+Shift+Z).
- **Menus on screen** are borderless windows of their own (`menu_view`)
  that the render thread shows as grabbing xdg_popups of the window they
  belong to, placed by the positioner as `PopupPlacement` asks: at a point
  for pop-up and context menus (the chosen item over the point), beside
  the row of the item a submenu comes from, below a title of the bar. The
  compositor slides, flips and shrinks them to fit; Wayland doesn't say
  where windows are, so a menu positioned in screen coordinates opens at
  the pointer. Opening runs AppKit's loop, with its calls in AppKit's
  order: the delegate's fill calls, the begin-tracking notification,
  `menuWillOpen:`, the view's `willOpenMenu:withEvent:`, validation; then
  a loop in `NSEventTrackingRunLoopMode` until the menu closes (pointer,
  keyboard navigation, type select, submenus opening after 150 ms,
  click-and-click or press-drag-release, `cancelTracking`, the
  compositor's dismissal, the window it belongs to leaving the screen,
  which closes the popups first); then the highlights taken away, the
  delegates' `menuDidClose:`, the view's `didCloseMenu:withEvent:`, the
  end-tracking notification and the chosen item's action, after which the
  call returns. The loop keeps items, not rows, for what it does later, so
  a menu that changes while it shows (from a timer, or its delegate) keeps
  its highlight on the same item and shows again at its new size. Context
  menus come from views' `menuForEvent:` through the default
  `rightMouseDown:`, and `+popUpContextMenu:withEvent:forView:`.
- **The menu bar** is window chrome: GNOME and KDE programs put the menu
  bar in the window, and Wayland has nowhere else for it. Every titled
  window that isn't a panel or a sheet shows the main menu as a bar under
  the title bar (or at the top, when the compositor draws the
  decorations), drawn on the main thread as ops and rasterized on the
  render thread onto a subsurface. The window grows by the bar and its
  content keeps its size: to the frame arithmetic the bar is part of the
  title bar (`frameRectForContentRect:` includes it, `contentLayoutRect`
  leaves it out). A click on a title opens its menu; moving across titles
  and Left and Right switch menus; F10 opens the first. A program opts
  out with `+[NSMenu setMenuBarVisible:NO]`, and a user with
  `SIDESTEP_MENUBAR=hidden`; the bar also hides without a main menu and
  in full screen.
- **`NSPopUpButton`** keeps its menu and selection in its cell, as AppKit
  does, and pops the menu up through the same tracking loop: the selected
  item over the button for a pop-up, below it without the first item for
  a pull-down. Items added without an action get the cell's, which
  selects and sends the button's action.
- **`NSAlert`** lays its panel out as GNOME's message dialogs are (message
  and informative text centered, buttons along the bottom with the first
  on the right, stacked from three), with macOS's buttons, tags and key
  equivalents. `runModal` is the application's modal loop;
  `beginSheetModalForWindow:completionHandler:` a sheet.
- **Panels and the workspace** go to the desktop through
  xdg-desktop-portal: `FileChooser` for `NSSavePanel` and `NSOpenPanel`,
  `OpenURI` and `FileManager1` for `NSWorkspace`. D-Bus is spoken (with
  `desktop.rs`'s client) off the main thread, and no request waits on
  another: each file chooser has a thread and a connection of its own for
  as long as the user takes, URLs and files to show go to one thread
  whose calls end within seconds, and the programs it starts are waited
  for on threads of their own. Answers come back as tasks on the main run
  loop in the common modes. `runModal` is a modal loop for the panel
  (`NSApp.modalWindow` is the panel, other windows take no input, and
  they go on drawing) until the answer; `cancel:` gives the dialog up and
  answers Cancel. Without a portal, `zenity` or `kdialog` stand in for the
  file chooser and `xdg-open` for opening. `openURL:` answers at once (NO
  for a scheme nothing handles, or a file that isn't there), and
  `openURL:configuration:completionHandler:` calls its handler later, off
  the main thread, as AppKit does. `NSRunningApplication` knows only this
  program (Wayland shows it no others); `NSBeep` is silent.

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

`sidestep-appkit`'s `wayland_system` test checks the clipboard, drag and
drop and screens against other programs there (`wl-copy`, `wl-paste`, a
drag source of its own driving a virtual pointer, `swaymsg`, and `wtype`
for the keyboard focus the clipboard needs); under plain `cargo test` it
has no display and passes without checking:

```sh
scripts/linux-run scripts/headless-wayland cargo test -p sidestep-appkit --test wayland_system
```
