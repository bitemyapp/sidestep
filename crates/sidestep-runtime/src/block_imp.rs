//! Methods implemented by blocks: `imp_implementationWithBlock`,
//! `imp_getBlock` and `imp_removeBlock`.
//!
//! A method is called as `imp(self, _cmd, args...)`, a block as
//! `invoke(block, args...)` where a method block's first argument is
//! `self`. So each block gets a stub of four instructions that moves
//! `self` into the second argument register, over `_cmd`, loads its block
//! into the first and jumps to the block's `invoke`, leaving every other
//! argument, stack argument and the return value as they are.
//!
//! Stubs are made in chunks at run time, since there is no telling how
//! many blocks a program turns into methods. A chunk is two regions of one
//! page each: the stubs, and after them one data slot per stub holding its
//! block, which each stub finds at its own address plus the region size.
//! The stubs are written into an anonymous file (`memfd_create`) that is
//! then mapped read-only and executable over the first region, so no
//! memory is ever writable and executable, and none becomes executable
//! after being writable: processes denied that (systemd's
//! `MemoryDenyWriteExecute=`, the kernel's `PR_SET_MDWE`) can still make
//! methods from blocks. Where there is no `memfd_create`, or the kernel
//! won't map its files executable (`vm.memfd_noexec`), the stubs are
//! written into the region, which is then made executable and read-only.
//! Slots of removed blocks are reused.
//!
//! On x86_64, a method returning a struct in memory takes the address to
//! write it to first, which moves `self` and `_cmd` along by one register.
//! As on Apple's runtime, a block whose flags say it returns that way gets
//! a stub that shuffles accordingly. That is `BLOCK_USE_STRET` together
//! with `BLOCK_HAS_SIGNATURE`: without a signature the bit means nothing
//! (it was once "has a descriptor"), and block2 sets it alone on every
//! global block.

use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, Ordering};

use crate::blocks::{_Block_copy, _Block_release};
use crate::object::Object;
use crate::util::lock;
use crate::{Bool, Imp, NO, YES};

type Id = *mut Object;

/// Each stub and each data slot takes 16 bytes.
const STRIDE: usize = 16;

/// A block's flags: the block returns a struct through memory, which only
/// counts with `BLOCK_HAS_SIGNATURE`.
#[cfg(target_arch = "x86_64")]
const BLOCK_USE_STRET: i32 = 1 << 29;
#[cfg(target_arch = "x86_64")]
const BLOCK_HAS_SIGNATURE: i32 = 1 << 30;

/// Which way a stub moves the arguments.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Plain,
    /// x86_64 only: the method returns a struct in memory.
    #[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
    Stret,
}

struct Chunk {
    /// Where the stubs start; the data slots start `region` bytes later.
    code: usize,
    kind: Kind,
}

struct Stubs {
    /// The page size, which is also each region's size.
    region: usize,
    chunks: Vec<Chunk>,
    /// Stubs with no block, by kind.
    free: Vec<(Kind, usize)>,
}

static STUBS: Mutex<Stubs> = Mutex::new(Stubs { region: 0, chunks: Vec::new(), free: Vec::new() });

/// A stub's template, with `region` (the distance from a stub to its data
/// slot) filled in.
fn template(kind: Kind, region: usize) -> [u8; STRIDE] {
    let mut stub = [0u8; STRIDE];
    #[cfg(target_arch = "aarch64")]
    {
        let _ = kind;
        // The literal load is the second instruction, 4 bytes in.
        let literal = u32::try_from((region - 4) / 4).expect("a page is within a literal load's reach");
        let words: [u32; 4] = [
            0xaa00_03e1,                  // mov x1, x0
            0x5800_0000 | (literal << 5), // ldr x0, <stub + region>
            0xf940_0810,                  // ldr x16, [x0, #16]
            0xd61f_0200,                  // br x16
        ];
        for (i, word) in words.iter().enumerate() {
            stub[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
    }
    #[cfg(target_arch = "x86_64")]
    {
        // The rip-relative load ends 10 bytes in.
        let disp = u32::try_from(region - 10).expect("a page is within a displacement's reach").to_le_bytes();
        let code: [u8; 13] = match kind {
            // mov rsi, rdi; mov rdi, [rip + disp]; jmp [rdi + 16]
            Kind::Plain => [0x48, 0x89, 0xfe, 0x48, 0x8b, 0x3d, disp[0], disp[1], disp[2], disp[3], 0xff, 0x67, 0x10],
            // mov rdx, rsi; mov rsi, [rip + disp]; jmp [rsi + 16]
            Kind::Stret => [0x48, 0x89, 0xf2, 0x48, 0x8b, 0x35, disp[0], disp[1], disp[2], disp[3], 0xff, 0x66, 0x10],
        };
        stub[..13].copy_from_slice(&code);
        stub[13..].fill(0xcc); // int3
    }
    stub
}

impl Stubs {
    /// A free stub of `kind`, making a new chunk if there is none.
    fn take(&mut self, kind: Kind) -> usize {
        if let Some(i) = self.free.iter().rposition(|&(k, _)| k == kind) {
            return self.free.swap_remove(i).1;
        }
        if self.region == 0 {
            // SAFETY: plain libc call.
            self.region = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap_or(4096).max(4096);
        }
        let region = self.region;
        // SAFETY: a fresh private anonymous mapping, never unmapped.
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                2 * region,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert!(base != libc::MAP_FAILED, "sidestep: out of memory for block implementations");
        let stub = template(kind, region);
        let mut stubs = vec![0u8; region];
        for chunk in stubs.as_chunks_mut::<STRIDE>().0 {
            *chunk = stub;
        }
        let code = base.cast::<u8>();
        // SAFETY: the first region of the mapping above, which nothing
        // uses yet.
        let mapped = unsafe { map_code(code, &stubs) };
        if let Err(error) = mapped {
            // SAFETY: as above; the stubs are written while the region is
            // writable, then it becomes executable and read-only.
            let protected = unsafe {
                code.copy_from_nonoverlapping(stubs.as_ptr(), region);
                libc::mprotect(base, region, libc::PROT_READ | libc::PROT_EXEC)
            };
            assert!(
                protected == 0,
                "sidestep: imp_implementationWithBlock needs executable memory, which this process may not make \
                 ({error}; {})",
                std::io::Error::last_os_error()
            );
        }
        // SAFETY: the stubs were just written, through the data cache or a
        // file.
        unsafe { sync_instruction_cache(code, region) };
        let code = code.addr();
        self.chunks.push(Chunk { code, kind });
        // Hand out the chunk's first stub; the rest go on the free list,
        // lowest last so they are handed out in order.
        self.free.extend((1..region / STRIDE).rev().map(|i| (kind, code + i * STRIDE)));
        code
    }

    /// The data slot of `imp`, if it is one of the stubs.
    fn slot(&self, imp: usize) -> Option<&'static AtomicPtr<c_void>> {
        let chunk = self.chunks.iter().find(|c| (c.code..c.code + self.region).contains(&imp))?;
        if !(imp - chunk.code).is_multiple_of(STRIDE) {
            return None;
        }
        // SAFETY: the stub's data slot, in the chunk's writable second
        // region, which is never unmapped; slots are only accessed
        // atomically.
        Some(unsafe { AtomicPtr::from_ptr((imp + self.region) as *mut *mut c_void) })
    }
}

/// Map `stubs` executable at `at`, over the region there, without any
/// memory ever being writable and executable, or becoming executable
/// after being writable: the code goes into an anonymous file
/// (`memfd_create`), which is then mapped for reading and executing only.
/// Processes denied writable-then-executable memory (systemd's
/// `MemoryDenyWriteExecute=`, the kernel's `PR_SET_MDWE`) may still map a
/// file executable, as they do shared libraries.
///
/// # Safety
/// `at` must start a mapping of the process's own of at least
/// `stubs.len()` bytes, a whole number of pages, that nothing uses.
unsafe fn map_code(at: *mut u8, stubs: &[u8]) -> Result<(), std::io::Error> {
    // Through `syscall`, so as not to need a newer C library than Rust's.
    // SAFETY: plain system call with a C string name.
    let create =
        |flags: libc::c_uint| unsafe { libc::syscall(libc::SYS_memfd_create, c"sidestep-block-imps".as_ptr(), flags) };
    // A kernel set to make anonymous files non-executable unless asked
    // (`vm.memfd_noexec`) needs MFD_EXEC, which kernels before 6.3 refuse.
    let mut fd = create(libc::MFD_CLOEXEC | libc::MFD_EXEC);
    if fd < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINVAL) {
        fd = create(libc::MFD_CLOEXEC);
    }
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let fd = fd as libc::c_int;
    let result = (|| {
        let mut written = 0;
        while written < stubs.len() {
            // SAFETY: writing from a live buffer to our own file.
            let n = unsafe {
                libc::pwrite(fd, stubs[written..].as_ptr().cast(), stubs.len() - written, written as libc::off_t)
            };
            if n <= 0 {
                return Err(std::io::Error::last_os_error());
            }
            written += n as usize;
        }
        // SAFETY: replaces the caller's region, as it allows, with the
        // file's contents, read-only and executable.
        let mapped = unsafe {
            libc::mmap(
                at.cast(),
                stubs.len(),
                libc::PROT_READ | libc::PROT_EXEC,
                libc::MAP_PRIVATE | libc::MAP_FIXED,
                fd,
                0,
            )
        };
        if mapped == libc::MAP_FAILED { Err(std::io::Error::last_os_error()) } else { Ok(()) }
    })();
    // The mapping keeps the file.
    // SAFETY: our own descriptor.
    unsafe { libc::close(fd) };
    result
}

/// Make the instruction cache see code just written at `code`.
///
/// # Safety
/// `code` must point to `len` readable bytes.
#[cfg(target_arch = "aarch64")]
unsafe fn sync_instruction_cache(code: *const u8, len: usize) {
    let ctr: u64;
    // SAFETY: reading the cache type register, which Linux allows.
    unsafe { std::arch::asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack, preserves_flags)) };
    let dline = 4usize << ((ctr >> 16) & 0xf);
    let iline = 4usize << (ctr & 0xf);
    let (start, end) = (code.addr(), code.addr() + len);
    // SAFETY: cache maintenance by address on memory this process maps.
    unsafe {
        for line in (start & !(dline - 1)..end).step_by(dline) {
            std::arch::asm!("dc cvau, {}", in(reg) line, options(nostack, preserves_flags));
        }
        std::arch::asm!("dsb ish", options(nostack, preserves_flags));
        for line in (start & !(iline - 1)..end).step_by(iline) {
            std::arch::asm!("ic ivau, {}", in(reg) line, options(nostack, preserves_flags));
        }
        std::arch::asm!("dsb ish", "isb", options(nostack, preserves_flags));
    }
}

/// x86 keeps instruction fetch coherent with stores.
#[cfg(not(target_arch = "aarch64"))]
unsafe fn sync_instruction_cache(_code: *const u8, _len: usize) {}

/// An implementation that calls `block` with the receiver and the
/// message's arguments. The implementation keeps a copy of the block until
/// `imp_removeBlock`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn imp_implementationWithBlock(block: Id) -> Option<Imp> {
    if block.is_null() {
        return None;
    }
    // SAFETY: the caller passes a block.
    let block = unsafe { _Block_copy(block.cast()) };
    let kind = stub_kind(block);
    let mut stubs = lock(&STUBS);
    let imp = stubs.take(kind);
    stubs.slot(imp).expect("a stub just taken").store(block, Ordering::Release);
    // SAFETY: the stub is code, called with a method's arguments.
    Some(unsafe { std::mem::transmute::<usize, Imp>(imp) })
}

#[cfg(target_arch = "x86_64")]
fn stub_kind(block: *mut c_void) -> Kind {
    // SAFETY: a block's flags follow its isa.
    let flags = unsafe { block.cast::<i32>().add(2).read() };
    let stret = BLOCK_USE_STRET | BLOCK_HAS_SIGNATURE;
    if flags & stret == stret { Kind::Stret } else { Kind::Plain }
}

#[cfg(not(target_arch = "x86_64"))]
fn stub_kind(_block: *mut c_void) -> Kind {
    Kind::Plain
}

/// The block behind an implementation from `imp_implementationWithBlock`,
/// or nil.
#[unsafe(no_mangle)]
pub extern "C" fn imp_getBlock(imp: Option<Imp>) -> Id {
    let Some(imp) = imp else { return std::ptr::null_mut() };
    let stubs = lock(&STUBS);
    stubs.slot(imp as usize).map_or(std::ptr::null_mut(), |slot| slot.load(Ordering::Acquire).cast())
}

/// Releases the block behind an implementation from
/// `imp_implementationWithBlock` and frees the implementation for reuse.
/// Nothing may call it afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn imp_removeBlock(imp: Option<Imp>) -> Bool {
    let Some(imp) = imp else { return NO };
    let block = {
        let mut stubs = lock(&STUBS);
        let Some(slot) = stubs.slot(imp as usize) else { return NO };
        let block = slot.swap(std::ptr::null_mut(), Ordering::AcqRel);
        if block.is_null() {
            return NO;
        }
        let kind =
            stubs.chunks.iter().find(|c| (c.code..c.code + stubs.region).contains(&(imp as usize))).map(|c| c.kind);
        stubs.free.push((kind.expect("the stub's chunk"), imp as usize));
        block
    };
    // SAFETY: the implementation owned this copy of the block.
    unsafe { _Block_release(block) };
    YES
}

/// block2's blocks don't mark a struct return, so the conformance tests
/// can't reach the x86_64 stub for methods returning structs in memory;
/// these build blocks by hand, with a signature (which makes
/// `BLOCK_USE_STRET` count) and without (block2's global blocks).
#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use std::ffi::{c_char, c_ulong, c_void};

    use super::{BLOCK_HAS_SIGNATURE, BLOCK_USE_STRET, imp_getBlock, imp_implementationWithBlock, imp_removeBlock};
    use crate::class::Class;
    use crate::object::Object;

    #[repr(C)]
    #[derive(Debug, PartialEq)]
    struct Wide([i64; 5]);

    /// A descriptor with a signature and no copy or dispose helpers.
    #[repr(C)]
    struct Descriptor {
        reserved: c_ulong,
        size: c_ulong,
        signature: *const c_char,
    }

    // SAFETY: immutable, and the signature is a static string.
    unsafe impl Sync for Descriptor {}

    #[repr(C)]
    struct Block {
        isa: *const Class,
        flags: i32,
        reserved: i32,
        invoke: *const c_void,
        descriptor: *const Descriptor,
        base: i64,
    }

    const BLOCK_IS_GLOBAL: i32 = 1 << 28;

    static SIGNED: Descriptor =
        Descriptor { reserved: 0, size: size_of::<Block>() as c_ulong, signature: c"{?=[5q]}@:q".as_ptr() };
    static UNSIGNED: Descriptor =
        Descriptor { reserved: 0, size: size_of::<Block>() as c_ulong, signature: std::ptr::null() };

    fn block(flags: i32, invoke: *const c_void, descriptor: &'static Descriptor) -> Block {
        Block {
            isa: &crate::blocks::_NSConcreteGlobalBlock,
            flags: BLOCK_IS_GLOBAL | flags,
            reserved: 0,
            invoke,
            descriptor,
            base: 7,
        }
    }

    extern "C" fn wide(block: *const Block, this: *mut Object, x: i64) -> Wide {
        // SAFETY: called by the stub with the block it was made for.
        let base = unsafe { (*block).base };
        Wide([base, x, this as i64, 0, -1])
    }

    #[test]
    fn struct_returning_block_method() {
        let invoke: extern "C" fn(*const Block, *mut Object, i64) -> Wide = wide;
        let block = block(BLOCK_USE_STRET | BLOCK_HAS_SIGNATURE, invoke as *const c_void, &SIGNED);
        let block_ptr = (&raw const block).cast_mut().cast::<Object>();
        // SAFETY: a global block, which copying leaves where it is.
        let imp = unsafe { imp_implementationWithBlock(block_ptr) }.unwrap();
        assert_eq!(imp_getBlock(Some(imp)), block_ptr);
        // SAFETY: the stub takes a method's arguments and returns what the
        // block returns.
        let method: extern "C" fn(*mut Object, *const c_void, i64) -> Wide = unsafe { std::mem::transmute(imp) };
        let receiver = 0x1000 as *mut Object;
        assert_eq!(method(receiver, std::ptr::null(), 42), Wide([7, 42, 0x1000, 0, -1]));
        // SAFETY: nothing calls the implementation any more.
        assert_eq!(unsafe { imp_removeBlock(Some(imp)) }, crate::YES);
    }

    extern "C" fn plain(block: *const Block, this: *mut Object, x: i64) -> i64 {
        // SAFETY: called by the stub with the block it was made for.
        let base = unsafe { (*block).base };
        (this as i64) * 10_000 + base * 1000 + x
    }

    /// `BLOCK_USE_STRET` without a signature, as block2 sets on every
    /// global block, is not a struct return.
    #[test]
    fn stret_flag_without_signature_is_plain() {
        let invoke: extern "C" fn(*const Block, *mut Object, i64) -> i64 = plain;
        let block = block(BLOCK_USE_STRET, invoke as *const c_void, &UNSIGNED);
        let block_ptr = (&raw const block).cast_mut().cast::<Object>();
        // SAFETY: a global block, which copying leaves where it is.
        let imp = unsafe { imp_implementationWithBlock(block_ptr) }.unwrap();
        // SAFETY: the stub takes a method's arguments.
        let method: extern "C" fn(*mut Object, *const c_void, i64) -> i64 = unsafe { std::mem::transmute(imp) };
        assert_eq!(method(0x2000 as *mut Object, std::ptr::null(), 42), 0x2000 * 10_000 + 7042);
        // SAFETY: nothing calls the implementation any more.
        assert_eq!(unsafe { imp_removeBlock(Some(imp)) }, crate::YES);
    }
}
