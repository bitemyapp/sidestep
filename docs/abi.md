# The ABI contract

What Sidestep provides so the published objc2 crates work on Linux, and where
it knowingly differs. Taken from objc2 0.6.4, block2 0.6.2 and
objc2-foundation 0.3.2 with their `gnustep-2-1` features; other versions are
untested.

## Symbols

| Area | Functions |
|---|---|
| Dispatch | `objc_msg_lookup`, `objc_msg_lookup_super`, `objc_msgSend`, `class_getMethodImplementation`, `class_respondsToSelector`; on x86_64 also `objc_msgSend_stret`, `objc_msgSend_fpret`, `class_getMethodImplementation_stret` |
| Selectors | `sel_registerName`, `sel_getUid`, `sel_getName`, `sel_isEqual` |
| Classes | `objc_allocateClassPair`, `objc_registerClassPair`, `objc_disposeClassPair`, `objc_getClass`, `objc_lookUpClass`, `objc_getRequiredClass`, `objc_getMetaClass`, `objc_copyClassList`, `objc_getClassList`, `class_getName`, `class_getSuperclass`, `class_isMetaClass`, `class_getInstanceSize`, `class_getVersion`, `class_setVersion`, `class_getIvarLayout`, `class_setIvarLayout` |
| Methods | `class_addMethod`, `class_replaceMethod`, `class_getInstanceMethod`, `class_getClassMethod`, `class_copyMethodList`, `method_getName`, `method_getImplementation`, `method_getTypeEncoding`, `method_setImplementation`, `method_exchangeImplementations`, `method_getNumberOfArguments`, `method_copyReturnType`, `method_copyArgumentType`, `method_getReturnType`, `method_getArgumentType` |
| Ivars | `class_addIvar`, `class_getInstanceVariable`, `class_getClassVariable`, `class_copyIvarList`, `ivar_getName`, `ivar_getTypeEncoding`, `ivar_getOffset`, `object_getIvar`, `object_setIvar`, `object_getInstanceVariable`, `object_setInstanceVariable` |
| Objects | `class_createInstance`, `object_dispose`, `object_getClass`, `object_setClass`, `object_getClassName`, `object_getIndexedIvars` |
| Protocols | `objc_getProtocol`, `objc_copyProtocolList`, `objc_allocateProtocol`, `objc_registerProtocol`, `protocol_getName`, `protocol_isEqual`, `protocol_conformsToProtocol`, `protocol_addMethodDescription`, `protocol_addProtocol`, `protocol_copyProtocolList`, `protocol_copyMethodDescriptionList`, `protocol_getMethodDescription`, `class_addProtocol`, `class_conformsToProtocol`, `class_copyProtocolList` |
| Properties | `class_addProperty`, `class_replaceProperty`, `class_getProperty`, `class_copyPropertyList`, `property_getName`, `property_getAttributes`, `property_copyAttributeList`, `property_copyAttributeValue`, `protocol_addProperty`, `protocol_copyPropertyList`, `protocol_getProperty` |
| Reference counting | `objc_retain`, `objc_release`, `objc_autorelease`, `objc_autoreleaseReturnValue`, `objc_retainAutorelease`, `objc_retainAutoreleaseReturnValue`, `objc_retainAutoreleasedReturnValue`, `objc_retainBlock`, `objc_storeStrong`, `objc_autoreleasePoolPush`, `objc_autoreleasePoolPop` |
| Weak references | `objc_storeWeak`, `objc_initWeak`, `objc_destroyWeak`, `objc_loadWeak`, `objc_loadWeakRetained`, `objc_copyWeak`, `objc_moveWeak` |
| Other | `objc_setAssociatedObject`, `objc_getAssociatedObject`, `objc_removeAssociatedObjects` (every association policy), `objc_sync_enter`, `objc_sync_exit`, `objc_enumerationMutation`, `objc_setEnumerationMutationHandler`, `objc_exception_throw`, `imp_implementationWithBlock`, `imp_getBlock`, `imp_removeBlock` |
| Blocks | `_Block_copy`, `_Block_release`, `_Block_object_assign`, `_Block_object_dispose`, `_Block_has_signature`, `_Block_signature`, and the classes `_NSConcreteStackBlock`, `_NSConcreteMallocBlock`, `_NSConcreteGlobalBlock` |
| Class symbols | `._OBJC_CLASS_<Name>` and `._OBJC_METACLASS_<Name>` for `NSObject` (runtime) and each Foundation and AppKit class, each also listed in the `sidestep_classes` linker section |

The blocks runtime follows Clang's published Block Implementation
Specification.

## Conventions

- `BOOL` is `unsigned char`, as on GNUstep outside Windows and as objc2 encodes
  it there.
- A `SEL` points to an interned `{ name, types }` pair; equal names give equal
  pointers. Typed selectors are not used.
- Objects start with `isa`; a 16-byte header precedes them. Class objects and
  blocks have no header.
- Messages to nil resolve to a function returning zero (objc2 short-circuits
  them anyway). `objc_msgSend` to nil zeroes the integer and floating-point
  return registers and writes nothing through a struct return address.
- `objc_autoreleaseReturnValue` and `objc_retainAutoreleasedReturnValue`
  hand an object over without the pool only when the claim is the
  instruction the first returns to, judged by return addresses as on
  Apple's runtime (on aarch64 the claim is 4 bytes on; on x86_64, 8 or 9,
  after the move of `rax` into `rdi`). Elsewhere the object is
  autoreleased and retained as usual.
- `sidestep_classes` is a section of pointers, one to each static class
  shell, bracketed by the linker's `__start_sidestep_classes` and
  `__stop_sidestep_classes`. `static_class!` writes the entries.
- The main thread is the one whose thread id equals the process id.

## Deliberate differences from libobjc2

- `class_addProtocol` returns NO only if the class itself already adopts the
  protocol, as on Apple's runtime. libobjc2 also returns NO when a superclass
  adopts it, and objc2's own GNUstep tests expect that. Sidestep follows Apple
  because app code is written against macOS behavior.
- Unrecognized selectors panic (unwinding into the caller) with Apple's
  exception message rather than raising an Objective-C exception, and so do
  `objc_exception_throw` (naming the exception's class) and
  `objc_enumerationMutation` without a handler (with Foundation's message).
  There is no Objective-C exception unwinding: nothing implements
  `objc_begin_catch`, `objc_end_catch` or `objc_exception_rethrow`, which
  only Clang-compiled `@try` blocks call.
- As on Apple's runtime and unlike libobjc2, the atomic association
  policies return the value retained and autoreleased, the nonatomic ones
  as it is, and `protocol_getProperty` finds no optional properties.
- Declared properties follow Apple's runtime: the attribute string leaves
  out attributes given a null value and quotes names longer than one
  character, the attribute list is read back from it,
  `class_copyPropertyList` lists the newest first, and
  `class_replaceProperty` changes a property in place.
- libobjc2's return-value handoff claims on the object alone; Sidestep, like
  Apple's runtime, also requires the claim to follow the return directly
  (see Conventions), which keeps an object the caller didn't keep in its
  pool.
- `imp_removeBlock` on an implementation already removed returns NO (Apple's
  runtime crashes).

## Known gaps

- Forwarding stops at `-forwardingTargetForSelector:`: `-forwardInvocation:`
  and `-methodSignatureForSelector:` need `NSInvocation` and
  `NSMethodSignature` from Foundation, and an invocation needs to call a
  method with arguments rebuilt from a signature.
- objc2's `exception` feature: its helper crate compiles Objective-C with
  Clang at build time, which Sidestep's builds don't have, and catching
  would need a GNUstep-style personality routine.
- `+load`.
- The forwarding trampoline, `imp_implementationWithBlock`,
  `objc_msgSend` and the return-value handoff exist for aarch64 and x86_64
  only. On x86_64, `imp_implementationWithBlock` takes a block as returning
  a struct in memory when its flags have both `BLOCK_USE_STRET` and
  `BLOCK_HAS_SIGNATURE`, as Apple's runtime does. block2 never marks a
  block that way, so on x86_64 (as on Apple's runtime) a block2 block
  returning a struct in memory can't be used as a method; its other blocks,
  global ones included, can.
- `imp_implementationWithBlock` makes pages executable, which a process
  denied that (systemd's `MemoryDenyWriteExecute=`, for one) can't do; it
  panics there.
- `objc_msgSend` saves the argument registers and calls `objc_msg_lookup`
  rather than probing the cache in assembly, so it costs a few nanoseconds
  more than objc2's own sends.

## Upstream issues

- **`NSStringEncoding` width.** On GNUstep, objc2's internal `NSString`
  helpers (`from_str`, `Display`, `to_str`) pass encodings as `i32`, while
  objc2-foundation's generated bindings pass `NSStringEncoding` as `usize`.
  objc2's debug-mode signature check only tolerates integer size differences on
  Apple targets, so on Linux one of the two paths fails it whatever the method
  is registered with. Sidestep registers `i32` to match the helpers (the path
  every string conversion takes) and reads only the low 32 bits, which is
  correct for either caller. Direct calls such as
  `s.lengthOfBytesUsingEncoding(NSUTF8StringEncoding)` work in release builds
  but fail the debug check. To be raised with objc2.
