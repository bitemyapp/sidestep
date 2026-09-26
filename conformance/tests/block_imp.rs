//! Methods implemented by blocks, through `imp_implementationWithBlock`.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use block2::RcBlock;
use objc2::ffi;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, Imp, NSObject, Sel};
use objc2::{ClassType, msg_send, sel};

use sidestep as _;

fn new_class(name: &std::ffi::CStr) -> &'static AnyClass {
    ClassBuilder::new(name, NSObject::class()).expect("a new class name").register()
}

fn add(class: &AnyClass, sel: Sel, imp: Imp, types: &std::ffi::CStr) {
    let class = (class as *const AnyClass).cast_mut();
    assert!(unsafe { ffi::class_addMethod(class, sel, imp, types.as_ptr()) }.as_bool());
}

fn block_imp<B: ?Sized>(block: &RcBlock<B>) -> Imp {
    let block = RcBlock::as_ptr(block).cast::<AnyObject>();
    unsafe { ffi::imp_implementationWithBlock(block) }
}

/// Sends `sel` through the method's own implementation, which works in
/// debug builds too (where objc2 checks the method exists first, and it
/// does).
#[test]
fn block_methods() {
    let class = new_class(c"SidestepBlockImpTarget");
    let total = Arc::new(AtomicI64::new(0));
    let captured = total.clone();
    let add_block = RcBlock::new(move |this: *mut AnyObject, a: i64, b: i64| -> i64 {
        assert!(!this.is_null());
        captured.fetch_add(a + b, Ordering::SeqCst);
        a + b
    });
    let scale_block = RcBlock::new(|_this: *mut AnyObject, x: f64, y: f64| -> f64 { x * y });
    add(class, sel!(add:to:), block_imp(&add_block), c"q@:qq");
    add(class, sel!(scale:by:), block_imp(&scale_block), c"d@:dd");

    let obj: Retained<AnyObject> = unsafe { msg_send![class, new] };
    let sum: i64 = unsafe { msg_send![&*obj, add: 40i64, to: 2i64] };
    assert_eq!(sum, 42);
    let product: f64 = unsafe { msg_send![&*obj, scale: 1.5f64, by: 4.0f64] };
    assert_eq!(product, 6.0);
    assert_eq!(total.load(Ordering::SeqCst), 42);
}

#[test]
fn block_receives_the_receiver() {
    let class = new_class(c"SidestepBlockImpReceiver");
    let block = RcBlock::new(|this: *mut AnyObject| -> *mut AnyObject { this });
    add(class, sel!(me), block_imp(&block), c"@@:");
    let obj: Retained<AnyObject> = unsafe { msg_send![class, new] };
    let me: *mut AnyObject = unsafe { msg_send![&*obj, me] };
    assert_eq!(me, Retained::as_ptr(&obj).cast_mut());
}

#[test]
fn get_and_remove_blocks() {
    let witness = Arc::new(());
    let captured = witness.clone();
    let block = RcBlock::new(move |_this: *mut AnyObject| -> i32 {
        let _keep = &captured;
        7
    });
    let imp = block_imp(&block);
    // The implementation holds a copy of the block.
    drop(block);
    assert_eq!(Arc::strong_count(&witness), 2);
    assert!(!unsafe { ffi::imp_getBlock(imp) }.is_null());

    let class = new_class(c"SidestepBlockImpRemoved");
    add(class, sel!(seven), imp, c"i@:");
    let obj: Retained<AnyObject> = unsafe { msg_send![class, new] };
    let seven: i32 = unsafe { msg_send![&*obj, seven] };
    assert_eq!(seven, 7);

    assert!(unsafe { ffi::imp_removeBlock(imp) }.as_bool());
    assert_eq!(Arc::strong_count(&witness), 1);

    // An ordinary method's implementation has no block.
    let plain = NSObject::class().instance_method(sel!(hash)).unwrap().implementation();
    assert!(unsafe { ffi::imp_getBlock(plain) }.is_null());
    assert!(!unsafe { ffi::imp_removeBlock(plain) }.as_bool());
}

#[test]
fn many_block_methods() {
    // More than fit in one page of stubs, created and removed repeatedly.
    let class = new_class(c"SidestepBlockImpMany");
    let obj: Retained<AnyObject> = unsafe { msg_send![class, new] };
    let mut imps = Vec::new();
    for round in 0..3i64 {
        for i in 0..5000i64 {
            let block = RcBlock::new(move |_this: *mut AnyObject, x: i64| -> i64 { x + i + round });
            imps.push((i, block_imp(&block)));
        }
        for &(i, imp) in imps.iter().step_by(97) {
            let f: unsafe extern "C-unwind" fn(&AnyObject, Sel, i64) -> i64 = unsafe { std::mem::transmute(imp) };
            assert_eq!(unsafe { f(&obj, sel!(anything:), 1000) }, 1000 + i + round);
        }
        for (_, imp) in imps.drain(..) {
            assert!(unsafe { ffi::imp_removeBlock(imp) }.as_bool());
        }
    }
}

/// A struct too large for registers comes back through memory: in the
/// register aarch64 reserves for it, which the stub leaves alone.
/// (On x86_64 the address takes the first argument register, and the
/// block's flags would have to say so, which block2's blocks don't.)
#[test]
#[cfg(not(target_arch = "x86_64"))]
fn block_returning_a_large_struct() {
    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq)]
    struct Wide([i64; 5]);
    unsafe impl objc2::Encode for Wide {
        const ENCODING: objc2::Encoding = objc2::Encoding::Struct("SidestepBlockWide", &[<[i64; 5]>::ENCODING]);
    }
    let class = new_class(c"SidestepBlockImpWide");
    let block = RcBlock::new(|_this: *mut AnyObject, x: i64| -> Wide { Wide([x, x + 1, x + 2, x + 3, x + 4]) });
    add(class, sel!(wide:), block_imp(&block), c"{SidestepBlockWide=[5q]}@:q");
    let obj: Retained<AnyObject> = unsafe { msg_send![class, new] };
    let wide: Wide = unsafe { msg_send![&*obj, wide: 10i64] };
    assert_eq!(wide, Wide([10, 11, 12, 13, 14]));
}
