# The ABI contract

What Sidestep provides so the published objc2 crates work on Linux, and where
it knowingly differs. Taken from objc2 0.6.4, block2 0.6.2 and
objc2-foundation 0.3.2 with their `gnustep-2-1` features; other versions are
untested.

## Symbols

| Area | Functions |
|---|---|
| Dispatch | `objc_msg_lookup`, `objc_msg_lookup_super`, `class_getMethodImplementation`, `class_respondsToSelector` |
| Selectors | `sel_registerName`, `sel_getUid`, `sel_getName`, `sel_isEqual` |
| Classes | `objc_allocateClassPair`, `objc_registerClassPair`, `objc_disposeClassPair`, `objc_getClass`, `objc_lookUpClass`, `objc_getRequiredClass`, `objc_getMetaClass`, `objc_copyClassList`, `objc_getClassList`, `class_getName`, `class_getSuperclass`, `class_isMetaClass`, `class_getInstanceSize`, `class_getVersion`, `class_setVersion`, `class_getIvarLayout`, `class_setIvarLayout` |
| Methods | `class_addMethod`, `class_replaceMethod`, `class_getInstanceMethod`, `class_getClassMethod`, `class_copyMethodList`, `method_getName`, `method_getImplementation`, `method_getTypeEncoding`, `method_setImplementation`, `method_exchangeImplementations`, `method_getNumberOfArguments`, `method_copyReturnType`, `method_copyArgumentType`, `method_getReturnType`, `method_getArgumentType` |
| Ivars | `class_addIvar`, `class_getInstanceVariable`, `class_getClassVariable`, `class_copyIvarList`, `ivar_getName`, `ivar_getTypeEncoding`, `ivar_getOffset`, `object_getIvar`, `object_setIvar`, `object_getInstanceVariable`, `object_setInstanceVariable` |
| Objects | `class_createInstance`, `object_dispose`, `object_getClass`, `object_setClass`, `object_getClassName`, `object_getIndexedIvars` |
| Protocols | `objc_getProtocol`, `objc_copyProtocolList`, `objc_allocateProtocol`, `objc_registerProtocol`, `protocol_getName`, `protocol_isEqual`, `protocol_conformsToProtocol`, `protocol_addMethodDescription`, `protocol_addProtocol`, `protocol_addProperty`, `protocol_copyProtocolList`, `protocol_copyMethodDescriptionList`, `protocol_getMethodDescription`, `class_addProtocol`, `class_conformsToProtocol`, `class_copyProtocolList` |
| Reference counting | `objc_retain`, `objc_release`, `objc_autorelease`, `objc_autoreleaseReturnValue`, `objc_retainAutorelease`, `objc_retainAutoreleaseReturnValue`, `objc_retainAutoreleasedReturnValue`, `objc_retainBlock`, `objc_storeStrong`, `objc_autoreleasePoolPush`, `objc_autoreleasePoolPop` |
| Weak references | `objc_storeWeak`, `objc_initWeak`, `objc_destroyWeak`, `objc_loadWeak`, `objc_loadWeakRetained`, `objc_copyWeak`, `objc_moveWeak` |
| Other | `objc_setAssociatedObject`, `objc_getAssociatedObject`, `objc_removeAssociatedObjects`, `objc_sync_enter`, `objc_sync_exit` |
| Blocks | `_Block_copy`, `_Block_release`, `_Block_object_assign`, `_Block_object_dispose`, `_Block_has_signature`, `_Block_signature`, and the classes `_NSConcreteStackBlock`, `_NSConcreteMallocBlock`, `_NSConcreteGlobalBlock` |
| Class symbols | `._OBJC_CLASS_<Name>` and `._OBJC_METACLASS_<Name>` for `NSObject` (runtime) and each Foundation class |
| Foundation functions | `NSUnionRange`, `NSIntersectionRange`, `NSStringFromRange`, `NSRangeFromString`; the `NSGeometry` functions (`NSEqualRects`, `NSInsetRect`, `NSIntegralRectWithOptions`, `NSDivideRect`, `NSPointInRect`, `NSStringFromRect`, `NSRectFromString` and the rest objc2-foundation declares) |

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
  them anyway).
- The main thread is the one whose thread id equals the process id.

## Deliberate differences from libobjc2

- `class_addProtocol` returns NO only if the class itself already adopts the
  protocol, as on Apple's runtime. libobjc2 also returns NO when a superclass
  adopts it, and objc2's own GNUstep tests expect that. Sidestep follows Apple
  because app code is written against macOS behavior.
- Unrecognized selectors panic (unwinding into the caller) with Apple's
  exception message rather than raising an Objective-C exception.

## Known gaps

- `objc_getClass` finds a static framework class only after it has been used
  once; a class list per crate is planned.
- No message forwarding (`-forwardingTargetForSelector:`,
  `-forwardInvocation:`) yet; both need a small per-architecture trampoline.
- Not implemented: exceptions (`objc_exception_throw`, objc2's `exception`
  feature), `imp_implementationWithBlock`, properties (`class_addProperty`
  and `property_*`), `+load`.
- Method caches take a read lock per lookup. Fine for now; a lock-free cache
  comes later, measured against real workloads.

## Upstream issues

- **`NSStringEncoding` width.** On GNUstep, objc2's internal `NSString`
  helpers (`from_str`, `Display`, `to_str`) pass encodings as `i32`, while
  objc2-foundation's generated bindings pass `NSStringEncoding` as `usize`.
  objc2's debug-mode signature check only tolerates integer size differences on
  Apple targets, so on Linux one of the two paths fails it whatever the method
  is registered with. Sidestep registers `i32` to match the helpers (the path
  every string conversion takes) for the two methods the helpers send,
  `-initWithBytes:length:encoding:` and `-lengthOfBytesUsingEncoding:`, and
  `NSUInteger` for every other method taking an encoding; all of them read
  only the low 32 bits, which is correct for either caller. Direct calls such
  as `s.lengthOfBytesUsingEncoding(NSUTF8StringEncoding)` work in release
  builds but fail the debug check. To be raised with objc2.
