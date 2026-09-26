//! The blocks runtime, written from Clang's published "Block Implementation
//! Specification". Heap blocks and `__block` variables keep a reference
//! count in the low bits of their flags word, which the specification leaves
//! to the runtime.

use std::ffi::{CStr, c_char, c_int, c_ulong, c_void};
use std::sync::atomic::{AtomicI32, Ordering};

use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, Sel};
use objc2::sel;

use crate::class::{BLOCK, Class};
use crate::nsobject::NSOBJECT_CLASS;
use crate::object::Object;
use crate::util::{c_free, c_malloc};

// Flags set by the compiler.
const BLOCK_HAS_COPY_DISPOSE: i32 = 1 << 25;
const BLOCK_IS_GLOBAL: i32 = 1 << 28;
const BLOCK_HAS_SIGNATURE: i32 = 1 << 30;
// Flags owned by the runtime.
const BLOCK_NEEDS_FREE: i32 = 1 << 24;
const REFCOUNT_MASK: i32 = 0xfffe;
const REFCOUNT_ONE: i32 = 2;
/// A `__block` variable whose helpers are followed by a layout string.
const BYREF_LAYOUT_EXTENDED: i32 = 1 << 28;

// `_Block_object_assign` / `_Block_object_dispose` field kinds.
const FIELD_IS_OBJECT: c_int = 3;
const FIELD_IS_BLOCK: c_int = 7;
const FIELD_IS_BYREF: c_int = 8;
const FIELD_IS_WEAK: c_int = 16;
const BYREF_CALLER: c_int = 128;

#[repr(C)]
struct BlockLayout {
    isa: *const Class,
    flags: AtomicI32,
    reserved: i32,
    invoke: *const c_void,
    descriptor: *const Descriptor,
}

#[repr(C)]
struct Descriptor {
    reserved: c_ulong,
    size: c_ulong,
    // Followed by copy and dispose helpers if BLOCK_HAS_COPY_DISPOSE, then
    // the signature if BLOCK_HAS_SIGNATURE.
}

type CopyHelper = unsafe extern "C" fn(*mut c_void, *const c_void);
type DisposeHelper = unsafe extern "C" fn(*const c_void);

impl Descriptor {
    /// The optional fields after `size`, as pointer-sized slots.
    unsafe fn slot(&self, index: usize) -> *const c_void {
        // SAFETY: the caller checks the flags that make the slot present.
        unsafe { *(self as *const Descriptor).add(1).cast::<*const c_void>().add(index) }
    }
}

fn latching_incr(flags: &AtomicI32) {
    let mut old = flags.load(Ordering::Relaxed);
    loop {
        if old & REFCOUNT_MASK == REFCOUNT_MASK {
            return;
        }
        match flags.compare_exchange_weak(old, old + REFCOUNT_ONE, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return,
            Err(actual) => old = actual,
        }
    }
}

/// Decrement; true when the count reaches zero and the caller must free.
fn latching_decr(flags: &AtomicI32) -> bool {
    let mut old = flags.load(Ordering::Relaxed);
    loop {
        let count = old & REFCOUNT_MASK;
        if count == REFCOUNT_MASK || count == 0 {
            return false;
        }
        match flags.compare_exchange_weak(old, old - REFCOUNT_ONE, Ordering::Release, Ordering::Relaxed) {
            Ok(_) => {
                std::sync::atomic::fence(Ordering::Acquire);
                return count == REFCOUNT_ONE;
            }
            Err(actual) => old = actual,
        }
    }
}

unsafe fn layout<'a>(block: *const c_void) -> &'a BlockLayout {
    // SAFETY: the caller passes a block.
    unsafe { &*block.cast::<BlockLayout>() }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn _Block_copy(block: *const c_void) -> *mut c_void {
    if block.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the caller passes a block.
    let b = unsafe { layout(block) };
    let flags = b.flags.load(Ordering::Relaxed);
    if flags & BLOCK_NEEDS_FREE != 0 {
        latching_incr(&b.flags);
        return block.cast_mut();
    }
    if flags & BLOCK_IS_GLOBAL != 0 || std::ptr::eq(b.isa, &_NSConcreteGlobalBlock) {
        return block.cast_mut();
    }
    // A stack block: move a copy to the heap.
    // SAFETY: the descriptor records the block's size, and the copy helper
    // (if any) copies captured state from the original.
    unsafe {
        let descriptor = &*b.descriptor;
        let size = descriptor.size as usize;
        let copy = c_malloc(size);
        copy.cast::<u8>().copy_from_nonoverlapping(block.cast::<u8>(), size);
        let c = &mut *copy.cast::<BlockLayout>();
        c.isa = &_NSConcreteMallocBlock;
        c.flags.store((flags & !REFCOUNT_MASK) | BLOCK_NEEDS_FREE | REFCOUNT_ONE, Ordering::Relaxed);
        if flags & BLOCK_HAS_COPY_DISPOSE != 0 {
            let helper: CopyHelper = std::mem::transmute(descriptor.slot(0));
            helper(copy, block);
        }
        copy
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn _Block_release(block: *const c_void) {
    if block.is_null() {
        return;
    }
    // SAFETY: the caller passes a block it owns a reference to.
    let b = unsafe { layout(block) };
    let flags = b.flags.load(Ordering::Relaxed);
    if flags & BLOCK_NEEDS_FREE == 0 || !latching_decr(&b.flags) {
        return;
    }
    // SAFETY: the last reference is gone.
    unsafe {
        if flags & BLOCK_HAS_COPY_DISPOSE != 0 {
            let helper: DisposeHelper = std::mem::transmute((*b.descriptor).slot(1));
            helper(block);
        }
        c_free(block.cast_mut());
    }
}

/// `-retain` / `objc_retain` on a block: counts heap blocks, leaves stack and
/// global blocks alone (only `copy` moves a stack block).
pub(crate) unsafe fn retain(block: *mut Object) -> *mut Object {
    // SAFETY: the caller passes a block.
    let b = unsafe { layout(block.cast()) };
    if b.flags.load(Ordering::Relaxed) & BLOCK_NEEDS_FREE != 0 {
        latching_incr(&b.flags);
    }
    block
}

pub(crate) unsafe fn release(block: *mut Object) {
    // SAFETY: forwarded contract.
    unsafe { _Block_release(block.cast()) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Block_has_signature(block: *const c_void) -> bool {
    // SAFETY: the caller passes a block.
    !block.is_null() && unsafe { layout(block) }.flags.load(Ordering::Relaxed) & BLOCK_HAS_SIGNATURE != 0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Block_signature(block: *const c_void) -> *const c_char {
    // SAFETY: forwarded contract.
    if !unsafe { _Block_has_signature(block) } {
        return std::ptr::null();
    }
    // SAFETY: the signature follows the optional helpers.
    unsafe {
        let b = layout(block);
        let helpers = b.flags.load(Ordering::Relaxed) & BLOCK_HAS_COPY_DISPOSE != 0;
        (*b.descriptor).slot(if helpers { 2 } else { 0 }).cast()
    }
}

// `__block` variables.

#[repr(C)]
struct Byref {
    isa: *const c_void,
    forwarding: *mut Byref,
    flags: AtomicI32,
    size: u32,
}

#[repr(C)]
struct ByrefHelpers {
    keep: unsafe extern "C" fn(*mut Byref, *mut Byref),
    destroy: unsafe extern "C" fn(*mut Byref),
}

unsafe fn byref_copy(src: *mut Byref) -> *mut Byref {
    // SAFETY: the caller passes a `__block` variable.
    unsafe {
        let current = (*src).forwarding;
        if (*current).flags.load(Ordering::Relaxed) & BLOCK_NEEDS_FREE != 0 {
            latching_incr(&(*current).flags);
            return current;
        }
        // Still on the stack: move it to the heap. It starts with two
        // references, one released when the stack variable goes out of
        // scope and one for the block copying it.
        let flags = (*src).flags.load(Ordering::Relaxed);
        let size = (*src).size as usize;
        let copy = c_malloc(size).cast::<Byref>();
        copy.write(Byref {
            isa: std::ptr::null(),
            forwarding: copy,
            flags: AtomicI32::new((flags & !REFCOUNT_MASK) | BLOCK_NEEDS_FREE | (2 * REFCOUNT_ONE)),
            size: size as u32,
        });
        (*src).forwarding = copy;
        if flags & BLOCK_HAS_COPY_DISPOSE != 0 {
            let src_helpers = src.add(1).cast::<ByrefHelpers>();
            let copy_helpers = copy.add(1).cast::<ByrefHelpers>();
            copy_helpers.write(src_helpers.read());
            if flags & BYREF_LAYOUT_EXTENDED != 0 {
                let layout = src_helpers.add(1).cast::<*const c_void>();
                copy_helpers.add(1).cast::<*const c_void>().write(layout.read());
            }
            ((*src_helpers).keep)(copy, src);
        } else {
            let header = size_of::<Byref>();
            copy.cast::<u8>().add(header).copy_from_nonoverlapping(src.cast::<u8>().add(header), size - header);
        }
        copy
    }
}

unsafe fn byref_release(byref: *mut Byref) {
    // SAFETY: the caller passes a `__block` variable.
    unsafe {
        let byref = (*byref).forwarding;
        let flags = (*byref).flags.load(Ordering::Relaxed);
        if flags & BLOCK_NEEDS_FREE == 0 || !latching_decr(&(*byref).flags) {
            return;
        }
        if flags & BLOCK_HAS_COPY_DISPOSE != 0 {
            ((*byref.add(1).cast::<ByrefHelpers>()).destroy)(byref);
        }
        c_free(byref.cast());
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn _Block_object_assign(dest: *mut c_void, object: *const c_void, flags: c_int) {
    let dest = dest.cast::<*const c_void>();
    // SAFETY: the compiler-generated copy helper passes a field and the
    // value to store in it.
    unsafe {
        if flags & BYREF_CALLER != 0 {
            *dest = object;
            return;
        }
        *dest = match flags & (FIELD_IS_BLOCK | FIELD_IS_BYREF | FIELD_IS_WEAK) {
            FIELD_IS_OBJECT => crate::arc::objc_retain(object.cast_mut().cast()).cast_const().cast(),
            FIELD_IS_BLOCK => _Block_copy(object),
            f if f & FIELD_IS_BYREF != 0 => byref_copy(object.cast_mut().cast()).cast_const().cast(),
            _ => object,
        };
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn _Block_object_dispose(object: *const c_void, flags: c_int) {
    if flags & BYREF_CALLER != 0 {
        return;
    }
    // SAFETY: the compiler-generated dispose helper passes a field it owns.
    unsafe {
        match flags & (FIELD_IS_BLOCK | FIELD_IS_BYREF | FIELD_IS_WEAK) {
            FIELD_IS_OBJECT => crate::arc::objc_release(object.cast_mut().cast()),
            FIELD_IS_BLOCK => _Block_release(object),
            f if f & FIELD_IS_BYREF != 0 => byref_release(object.cast_mut().cast()),
            _ => {}
        }
    }
}

// The block classes. Their instances are blocks, not header-prefixed
// objects, which the BLOCK flag tells the ARC functions before the classes
// are even loaded.

#[unsafe(no_mangle)]
pub static _NSConcreteStackBlock: Class =
    Class::shell_with_flags(&STACK_BLOCK_META, "__NSStackBlock__\0", load_stack, BLOCK);
static STACK_BLOCK_META: Class = Class::meta_shell(&_NSConcreteStackBlock, "__NSStackBlock__\0");

#[unsafe(no_mangle)]
pub static _NSConcreteMallocBlock: Class =
    Class::shell_with_flags(&MALLOC_BLOCK_META, "__NSMallocBlock__\0", load_malloc, BLOCK);
static MALLOC_BLOCK_META: Class = Class::meta_shell(&_NSConcreteMallocBlock, "__NSMallocBlock__\0");

#[unsafe(no_mangle)]
pub static _NSConcreteGlobalBlock: Class =
    Class::shell_with_flags(&GLOBAL_BLOCK_META, "__NSGlobalBlock__\0", load_global, BLOCK);
static GLOBAL_BLOCK_META: Class = Class::meta_shell(&_NSConcreteGlobalBlock, "__NSGlobalBlock__\0");

crate::__linked_class!(_NSConcreteStackBlock);
crate::__linked_class!(_NSConcreteMallocBlock);
crate::__linked_class!(_NSConcreteGlobalBlock);

type Id = *mut AnyObject;

unsafe extern "C-unwind" fn block_copy(this: Id, _: Sel) -> Id {
    // SAFETY: the receiver is a block.
    unsafe { _Block_copy(this.cast()).cast() }
}

unsafe extern "C-unwind" fn block_retain(this: Id, _: Sel) -> Id {
    // SAFETY: the receiver is a block.
    unsafe { retain(this.cast()).cast() }
}

unsafe extern "C-unwind" fn block_release(this: Id, _: Sel) {
    // SAFETY: the receiver is a block the caller owns a reference to.
    unsafe { release(this.cast()) }
}

unsafe extern "C-unwind" fn block_autorelease(this: Id, _: Sel) -> Id {
    // SAFETY: the receiver is a block.
    if unsafe { layout(this.cast()) }.flags.load(Ordering::Relaxed) & BLOCK_NEEDS_FREE != 0 {
        crate::arc::pool_add(this.cast());
    }
    this
}

fn define_block_class(name: &CStr) {
    // SAFETY: NSObject's shell is a class.
    let nsobject: &AnyClass = unsafe { &*(&NSOBJECT_CLASS as *const Class).cast() };
    let mut builder = ClassBuilder::new(name, nsobject).expect("sidestep: block classes are defined once");
    // SAFETY: signatures match the selectors' conventions.
    unsafe {
        builder.add_method(sel!(copy), block_copy as unsafe extern "C-unwind" fn(_, _) -> _);
        builder.add_method(sel!(retain), block_retain as unsafe extern "C-unwind" fn(_, _) -> _);
        builder.add_method(sel!(release), block_release as unsafe extern "C-unwind" fn(_, _));
        builder.add_method(sel!(autorelease), block_autorelease as unsafe extern "C-unwind" fn(_, _) -> _);
    }
    builder.register();
}

fn load_stack() {
    define_block_class(c"__NSStackBlock__");
}

fn load_malloc() {
    define_block_class(c"__NSMallocBlock__");
}

fn load_global() {
    define_block_class(c"__NSGlobalBlock__");
}
