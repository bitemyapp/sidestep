//! Calls laid out from type encodings, for `NSInvocation` and for
//! forwarding a message through `-forwardInvocation:`.
//!
//! A [`Signature`] parses a method's type encoding and works out once
//! where each argument and the return value travel under the C calling
//! convention: which bytes go in which integer or floating-point register,
//! which go on the stack, and which structs travel through memory by
//! address. Both directions use that plan with one block of registers,
//! [`Registers`], laid out as the forwarding trampoline saves them:
//!
//! - [`Signature::call`] fills a block and a stack area from argument
//!   values and calls an implementation through `sidestep_invoke`, a
//!   small assembly routine that loads the registers, copies the stack
//!   arguments below its frame, calls, and stores the return registers
//!   back into the block;
//! - a forwarded message's [`Frame`] is the block the trampoline saved
//!   plus the sender's stack arguments. [`Signature::read_arguments`]
//!   copies the arguments out of it, and [`Signature::write_return`]
//!   writes the return value into it, which the trampoline loads into the
//!   return registers on its way back to the sender (see `forward`).
//!
//! The conventions are Linux's: AAPCS64 on aarch64 (structs of up to four
//! floats or doubles in floating-point registers, other structs of up to
//! 16 bytes in integer registers, larger ones by address, and stack
//! arguments in 8-byte slots), and the System V ABI on x86_64 (structs of
//! up to 16 bytes classified by eightbyte, larger ones copied onto the
//! stack, and a struct returned in memory through a hidden first
//! argument). Integers narrower than a register are sign- or zero-extended
//! to all of it, which every convention accepts. `long double` is a
//! 16-byte floating-point value in a vector register on aarch64; on x86_64
//! it is passed on the stack but can't be returned, since it comes back on
//! the x87 stack.
//!
//! Encodings are read as Apple's `NSMethodSignature` reads them: offsets
//! after each type are ignored, qualifiers kept, `l` is 4 bytes (Clang
//! writes 64-bit `long` as `q`), and unions, bit-fields, `?`, complex and
//! atomic types can't be laid out.

use std::ffi::{CStr, CString};
use std::sync::atomic::{AtomicPtr, Ordering};

use objc2::runtime::AnyObject;

use crate::Imp;

/// The argument registers, laid out as the forwarding trampoline saves
/// them (`trampoline::saving_entry!`): x0-x8, then q0-q7. x8 holds the
/// address a struct is returned to.
#[cfg(target_arch = "aarch64")]
#[repr(C, align(16))]
pub struct Registers {
    gpr: [u64; 9],
    _pad: u64,
    fpr: [[u8; 16]; 8],
}

/// The argument registers, laid out as the forwarding trampoline saves
/// them: xmm0-xmm7, then rdi, rsi, rdx, rcx, r8, r9 and rax (the number of
/// vector registers a variadic call uses, and the integer return
/// register; rdx is the second).
#[cfg(target_arch = "x86_64")]
#[repr(C, align(16))]
pub struct Registers {
    fpr: [[u8; 16]; 8],
    gpr: [u64; 7],
    _pad: u64,
}

#[cfg(target_arch = "aarch64")]
const _: () = assert!(size_of::<Registers>() == 208 && std::mem::offset_of!(Registers, fpr) == 80);
#[cfg(target_arch = "x86_64")]
const _: () = assert!(size_of::<Registers>() == 192 && std::mem::offset_of!(Registers, gpr) == 128);

/// How many integer and floating-point registers carry arguments.
#[cfg(target_arch = "aarch64")]
const GPR_ARGS: u8 = 8;
#[cfg(target_arch = "x86_64")]
const GPR_ARGS: u8 = 6;
const FPR_ARGS: u8 = 8;

/// The registers a return value comes back in: on x86_64, rax and rdx.
#[cfg(target_arch = "aarch64")]
const GPR_RETURN: [u8; 2] = [0, 1];
#[cfg(target_arch = "x86_64")]
const GPR_RETURN: [u8; 2] = [6, 2];

/// The register holding the address a struct returned in memory goes to:
/// x8, or on x86_64 rdi, the hidden first argument.
#[cfg(target_arch = "aarch64")]
const INDIRECT_RESULT: u8 = 8;
#[cfg(target_arch = "x86_64")]
const INDIRECT_RESULT: u8 = 0;

#[cfg(target_arch = "x86_64")]
const RAX: usize = 6;

impl Registers {
    fn zeroed() -> Registers {
        // SAFETY: plain integers, for which zero is valid.
        unsafe { std::mem::zeroed() }
    }

    /// The integer registers, in the order above.
    pub(crate) fn integer_mut(&mut self) -> &mut [usize] {
        // SAFETY: `usize` is `u64` on the 64-bit targets Sidestep supports.
        unsafe { std::slice::from_raw_parts_mut(self.gpr.as_mut_ptr().cast::<usize>(), self.gpr.len()) }
    }
}

/// A forwarded message's registers and stack arguments.
pub struct Frame {
    regs: *mut Registers,
    stack: *const u8,
}

impl Frame {
    /// # Safety
    /// `regs` must be the registers a message was sent with, saved by the
    /// trampoline, and `stack` its stack arguments.
    pub(crate) unsafe fn new(regs: *mut Registers, stack: *const u8) -> Frame {
        Frame { regs, stack }
    }
}

/// Where one piece of a value travels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Loc {
    /// An integer register, by its index in [`Registers`].
    Gpr(u8),
    /// A floating-point register: the piece fills its low bytes.
    Fpr(u8),
    /// The stack, this many bytes into the arguments.
    Stack(u32),
}

/// How a narrow integer fills the rest of its register or stack slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ext {
    None,
    Sign,
    Zero,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Piece {
    loc: Loc,
    /// Where the piece starts in the value.
    offset: u32,
    len: u32,
    ext: Ext,
}

const NO_PIECE: Piece = Piece { loc: Loc::Gpr(0), offset: 0, len: 0, ext: Ext::None };

/// How a value travels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Passing {
    /// Nothing: `void`, or a struct with no fields.
    Nothing,
    /// In pieces, each in a register or on the stack.
    Pieces([Piece; 4], u8),
    /// A copy in memory, whose address travels in a register or stack slot.
    /// For a return value, the caller's memory, at the address in
    /// [`INDIRECT_RESULT`].
    Indirect(Loc),
    /// x86_64's `long double` return, on the x87 stack.
    X87,
}

/// What an argument is, as far as `-retainArguments` cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueKind {
    /// `@`: retained.
    Object,
    /// `@?`: copied.
    Block,
    /// `*`: a C string, copied.
    CString,
    /// Anything else, kept as it is.
    Plain,
}

/// One argument or the return value.
pub struct Value {
    types: CString,
    size: usize,
    /// Where the value sits in an invocation's buffer of values.
    offset: usize,
    kind: ValueKind,
    passing: Passing,
}

impl Value {
    /// The value's type encoding, with its qualifiers and without an
    /// offset.
    pub fn types(&self) -> &CStr {
        &self.types
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn kind(&self) -> ValueKind {
        self.kind
    }

    /// Where the value sits in a buffer of [`Signature::values_size`]
    /// bytes.
    pub fn offset(&self) -> usize {
        self.offset
    }
}

/// A method's types, laid out for calls.
pub struct Signature {
    ret: Value,
    args: Box<[Value]>,
    /// Bytes of stack arguments, a multiple of 16.
    stack_size: usize,
    /// Bytes of copies of structs passed by address (aarch64).
    indirect_size: usize,
    values_size: usize,
    oneway: bool,
    /// How many floating-point registers carry arguments (x86_64's `al`).
    #[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
    fprs_used: u8,
}

/// Why an encoding can't be laid out, in the terms Apple's Foundation
/// reports it.
#[derive(Debug, PartialEq, Eq)]
pub struct Unsupported {
    /// The character that can't be laid out.
    pub spec: char,
    /// The encoding from that character on.
    pub at: String,
    /// The encoding from the start of the type it is in.
    pub within: String,
    /// It is a union, which Apple's Foundation reports separately.
    pub union: bool,
}

impl Signature {
    /// Parse a method's type encoding: the return type, then each
    /// argument's, receiver and selector included. `Ok(None)` for an
    /// empty encoding.
    pub fn parse(types: &[u8]) -> Result<Option<Signature>, Unsupported> {
        let mut parser = Parser { s: types, pos: 0, top: 0 };
        let mut parsed = Vec::new();
        while parser.pos < types.len() {
            parsed.push(parser.top_level()?);
        }
        let Some((ret_types, ret_ty)) = parsed.first().cloned() else { return Ok(None) };
        let oneway = ret_types.iter().take_while(|b| QUALIFIERS.contains(b)).any(|&b| b == b'V');
        let mut layout = Layout::default();
        let ret_passing = layout.return_value(&ret_ty);
        let mut args = Vec::with_capacity(parsed.len() - 1);
        let mut values_size = 0;
        for (arg_types, ty) in &parsed[1..] {
            let passing = layout.argument(ty);
            args.push(Value::new(arg_types, ty, passing, &mut values_size));
        }
        let ret = Value::new(&ret_types, &ret_ty, ret_passing, &mut values_size);
        Ok(Some(Signature {
            ret,
            args: args.into_boxed_slice(),
            stack_size: layout.stack.next_multiple_of(16),
            indirect_size: layout.indirect,
            values_size: values_size.next_multiple_of(16),
            oneway,
            fprs_used: layout.fprs,
        }))
    }

    /// Arguments, the receiver and selector included.
    pub fn arguments(&self) -> &[Value] {
        &self.args
    }

    pub fn return_value(&self) -> &Value {
        &self.ret
    }

    pub fn is_oneway(&self) -> bool {
        self.oneway
    }

    /// The bytes a call's arguments take: the argument registers and the
    /// stack arguments.
    pub fn frame_length(&self) -> usize {
        if self.args.is_empty() { 0 } else { size_of::<Registers>() + self.stack_size }
    }

    /// The size of a buffer holding every argument and the return value,
    /// each at its [`Value::offset`]. The buffer must be 16-byte aligned.
    pub fn values_size(&self) -> usize {
        self.values_size
    }

    /// Call `imp` with the arguments in `values` and store its return
    /// value there.
    ///
    /// # Safety
    /// `values` must be a 16-byte aligned buffer of
    /// [`values_size`](Self::values_size) bytes holding valid arguments,
    /// and `imp` a function taking and returning what this signature
    /// describes.
    pub unsafe fn call(&self, imp: Imp, values: *mut u8) {
        assert!(self.ret.passing != Passing::X87, "sidestep: can't call a method returning long double on x86_64");
        let mut regs = Registers::zeroed();
        // Stack arguments: most calls have few or none, so they go in a
        // buffer on this stack, of which only the part used is cleared.
        let mut small = std::mem::MaybeUninit::<[u128; 16]>::uninit();
        let mut large = Vec::new();
        let stack = if self.stack_size <= size_of_val(&small) {
            small.as_mut_ptr().cast::<u8>()
        } else {
            large.resize(self.stack_size / 16, 0u128);
            large.as_mut_ptr().cast::<u8>()
        };
        // SAFETY: the buffer has room for the stack arguments.
        unsafe { stack.write_bytes(0, self.stack_size) };
        let mut copies = vec![0u128; self.indirect_size.div_ceil(16)];
        let mut copy_at = 0;
        for arg in &self.args {
            // SAFETY: the caller's buffer holds each argument at its offset;
            // `stack` has room for every stack argument.
            unsafe {
                let src = values.add(arg.offset);
                match arg.passing {
                    Passing::Nothing | Passing::X87 => {}
                    Passing::Pieces(pieces, n) => {
                        for piece in &pieces[..n as usize] {
                            store(&mut regs, stack, piece, src.add(piece.offset as usize));
                        }
                    }
                    Passing::Indirect(loc) => {
                        // The callee may change its copy, so it gets one of
                        // its own.
                        let copy = copies.as_mut_ptr().cast::<u8>().add(copy_at);
                        copy.copy_from_nonoverlapping(src, arg.size);
                        copy_at += arg.size.next_multiple_of(16);
                        let address = (copy as usize).to_ne_bytes();
                        store(&mut regs, stack, &word(loc), address.as_ptr());
                    }
                }
            }
        }
        // SAFETY: the return value's place in the caller's buffer.
        let ret = unsafe { values.add(self.ret.offset) };
        if let Passing::Indirect(_) = self.ret.passing {
            regs.gpr[INDIRECT_RESULT as usize] = ret as u64;
        }
        #[cfg(target_arch = "x86_64")]
        {
            regs.gpr[RAX] = u64::from(self.fprs_used);
        }
        // SAFETY: the registers and stack hold the call's arguments as the
        // calling convention places them; the caller vouches for `imp`.
        unsafe { sidestep_invoke(&mut regs, stack, self.stack_size, imp) };
        if let Passing::Pieces(pieces, n) = self.ret.passing {
            for piece in &pieces[..n as usize] {
                // SAFETY: the return value's bytes in the caller's buffer.
                unsafe { load(&regs, std::ptr::null(), piece, ret.add(piece.offset as usize)) };
            }
        }
    }

    /// Copy a forwarded message's arguments out of its frame into `values`.
    ///
    /// # Safety
    /// `values` must be a buffer as for [`call`](Self::call), and the
    /// message must have been sent with this signature.
    pub unsafe fn read_arguments(&self, frame: &Frame, values: *mut u8) {
        // SAFETY: the frame holds the registers the message was sent with.
        let regs = unsafe { &*frame.regs };
        for arg in &self.args {
            // SAFETY: as documented; stack arguments are where the
            // signature places them.
            unsafe {
                let dst = values.add(arg.offset);
                match arg.passing {
                    Passing::Nothing | Passing::X87 => {}
                    Passing::Pieces(pieces, n) => {
                        for piece in &pieces[..n as usize] {
                            load(regs, frame.stack, piece, dst.add(piece.offset as usize));
                        }
                    }
                    Passing::Indirect(loc) => {
                        let mut address = [0u8; 8];
                        load(regs, frame.stack, &word(loc), address.as_mut_ptr());
                        let src = usize::from_ne_bytes(address) as *const u8;
                        dst.copy_from_nonoverlapping(src, arg.size);
                    }
                }
            }
        }
    }

    /// Write the return value in `values` into a forwarded message's frame,
    /// where the trampoline returns it from.
    ///
    /// # Safety
    /// As for [`read_arguments`](Self::read_arguments).
    pub unsafe fn write_return(&self, frame: &mut Frame, values: *const u8) {
        assert!(self.ret.passing != Passing::X87, "sidestep: can't forward a method returning long double on x86_64");
        // SAFETY: the frame holds the registers the message was sent with.
        let regs = unsafe { &mut *frame.regs };
        // SAFETY: the return value's place in the caller's buffer.
        let src = unsafe { values.add(self.ret.offset) };
        match self.ret.passing {
            Passing::Nothing | Passing::X87 => {}
            Passing::Pieces(pieces, n) => {
                for piece in &pieces[..n as usize] {
                    // SAFETY: return values only go in registers.
                    unsafe { store(regs, std::ptr::null_mut(), piece, src.add(piece.offset as usize)) };
                }
            }
            Passing::Indirect(_) => {
                let dst = regs.gpr[INDIRECT_RESULT as usize] as *mut u8;
                // SAFETY: the sender passed the address of room for the
                // return value.
                unsafe { dst.copy_from_nonoverlapping(src, self.ret.size) };
                // The System V ABI returns that address in rax too.
                #[cfg(target_arch = "x86_64")]
                {
                    regs.gpr[RAX] = dst as u64;
                }
            }
        }
    }
}

impl Value {
    fn new(types: &[u8], ty: &Ty, passing: Passing, values_size: &mut usize) -> Value {
        let (size, align) = (ty.size(), ty.align());
        let offset = values_size.next_multiple_of(align.max(1));
        *values_size = offset + size;
        let body = &types[types.iter().take_while(|b| QUALIFIERS.contains(b)).count()..];
        let kind = match body {
            [b'@', b'?', ..] => ValueKind::Block,
            [b'@', ..] => ValueKind::Object,
            [b'*', ..] => ValueKind::CString,
            _ => ValueKind::Plain,
        };
        Value { types: CString::new(types).expect("encodings have no NULs"), size, offset, kind, passing }
    }
}

/// A pointer-sized piece at `loc`.
fn word(loc: Loc) -> Piece {
    Piece { loc, offset: 0, len: 8, ext: Ext::None }
}

/// Store the bytes of `piece` from `src` into its register or stack slot.
///
/// # Safety
/// `src` must hold `piece.len` bytes; a stack piece needs `stack` to have
/// room for it.
unsafe fn store(regs: &mut Registers, stack: *mut u8, piece: &Piece, src: *const u8) {
    let len = piece.len as usize;
    match piece.loc {
        Loc::Gpr(i) => {
            let mut word = [0u8; 8];
            // SAFETY: a register piece is at most 8 bytes.
            unsafe { word.as_mut_ptr().copy_from_nonoverlapping(src, len) };
            regs.gpr[i as usize] = extend(u64::from_ne_bytes(word), len, piece.ext);
        }
        Loc::Fpr(i) => {
            let mut reg = [0u8; 16];
            // SAFETY: a register piece is at most 16 bytes.
            unsafe { reg.as_mut_ptr().copy_from_nonoverlapping(src, len) };
            regs.fpr[i as usize] = reg;
        }
        Loc::Stack(at) => {
            // SAFETY: guaranteed by the caller.
            unsafe {
                let dst = stack.add(at as usize);
                if piece.ext == Ext::None {
                    dst.copy_from_nonoverlapping(src, len);
                } else {
                    let mut word = [0u8; 8];
                    word.as_mut_ptr().copy_from_nonoverlapping(src, len);
                    let word = extend(u64::from_ne_bytes(word), len, piece.ext).to_ne_bytes();
                    dst.copy_from_nonoverlapping(word.as_ptr(), 8);
                }
            }
        }
    }
}

/// Load the bytes of `piece` from its register or stack slot into `dst`.
///
/// # Safety
/// `dst` must have room for `piece.len` bytes; a stack piece needs `stack`
/// to hold it.
unsafe fn load(regs: &Registers, stack: *const u8, piece: &Piece, dst: *mut u8) {
    let len = piece.len as usize;
    // SAFETY: guaranteed by the caller; register pieces are at most the
    // register's size.
    unsafe {
        match piece.loc {
            Loc::Gpr(i) => dst.copy_from_nonoverlapping(regs.gpr[i as usize].to_ne_bytes().as_ptr(), len),
            Loc::Fpr(i) => dst.copy_from_nonoverlapping(regs.fpr[i as usize].as_ptr(), len),
            Loc::Stack(at) => dst.copy_from_nonoverlapping(stack.add(at as usize), len),
        }
    }
}

/// `word`'s low `len` bytes, sign- or zero-extended to 64 bits.
fn extend(word: u64, len: usize, ext: Ext) -> u64 {
    if len >= 8 || ext == Ext::None {
        return word;
    }
    let bits = 64 - 8 * len as u32;
    match ext {
        Ext::Sign => (((word << bits) as i64) >> bits) as u64,
        _ => (word << bits) >> bits,
    }
}

/// Type qualifiers kept on a type: const, in, inout, out, bycopy, byref,
/// oneway. Apple's Foundation can't lay out atomic (`A`) or complex (`j`)
/// types.
const QUALIFIERS: &[u8] = b"rnNoORV";

/// A C type, as far as calls care.
#[derive(Clone, Debug)]
enum Ty {
    Int {
        size: u8,
        signed: bool,
    },
    /// `float`, `double` or `long double`, by size.
    Float(u8),
    Pointer,
    Struct {
        fields: Vec<Ty>,
        size: usize,
        align: usize,
    },
    Array {
        elem: Box<Ty>,
        count: usize,
    },
    Void,
}

impl Ty {
    fn size(&self) -> usize {
        match self {
            Ty::Int { size, .. } | Ty::Float(size) => *size as usize,
            Ty::Pointer => 8,
            Ty::Struct { size, .. } => *size,
            Ty::Array { elem, count } => elem.size() * count,
            Ty::Void => 0,
        }
    }

    fn align(&self) -> usize {
        match self {
            Ty::Int { size, .. } | Ty::Float(size) => *size as usize,
            Ty::Pointer => 8,
            Ty::Struct { align, .. } => *align,
            Ty::Array { elem, .. } => elem.align(),
            Ty::Void => 1,
        }
    }

    /// The scalars that make up the type, with their offsets, for
    /// classifying a small struct. `None` past `limit` of them.
    fn leaves(&self, offset: usize, out: &mut Vec<(usize, Leaf)>, limit: usize) -> Option<()> {
        match self {
            Ty::Int { size, .. } => out.push((offset, Leaf::Int(*size))),
            Ty::Pointer => out.push((offset, Leaf::Int(8))),
            Ty::Float(size) => out.push((offset, Leaf::Float(*size))),
            Ty::Void => {}
            Ty::Array { elem, count } => {
                for i in 0..*count {
                    elem.leaves(offset + i * elem.size(), out, limit)?;
                }
            }
            Ty::Struct { fields, .. } => {
                let mut at = 0usize;
                for field in fields {
                    at = at.next_multiple_of(field.align());
                    field.leaves(offset + at, out, limit)?;
                    at += field.size();
                }
            }
        }
        (out.len() <= limit).then_some(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Leaf {
    Int(u8),
    Float(u8),
}

struct Parser<'a> {
    s: &'a [u8],
    pos: usize,
    /// Where the current top-level type starts.
    top: usize,
}

impl Parser<'_> {
    /// One argument's (or the return value's) type, and its encoding
    /// without the offset after it.
    fn top_level(&mut self) -> Result<(Vec<u8>, Ty), Unsupported> {
        self.top = self.pos;
        let ty = self.ty()?;
        let types = self.s[self.top..self.pos].to_vec();
        // Skip the offset; GNU encodings may sign it.
        if matches!(self.peek(), Some(b'+' | b'-')) {
            self.pos += 1;
        }
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
        }
        Ok((types, ty))
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }

    fn unsupported(&self, at: usize) -> Unsupported {
        let text = |from: usize| String::from_utf8_lossy(&self.s[from..]).into_owned();
        let spec = self.s.get(at).map_or('\0', |&b| b as char);
        Unsupported { spec, at: text(at), within: text(self.top), union: spec == '(' }
    }

    fn ty(&mut self) -> Result<Ty, Unsupported> {
        while self.peek().is_some_and(|b| QUALIFIERS.contains(&b)) {
            self.pos += 1;
        }
        let start = self.pos;
        let Some(c) = self.peek() else { return Err(self.unsupported(start)) };
        self.pos += 1;
        let int = |size, signed| Ok(Ty::Int { size, signed });
        match c {
            b'c' => int(1, true),
            b'C' | b'B' => int(1, false),
            b's' => int(2, true),
            b'S' => int(2, false),
            b'i' | b'l' => int(4, true),
            b'I' | b'L' => int(4, false),
            b'q' => int(8, true),
            b'Q' => int(8, false),
            b't' => int(16, true),
            b'T' => int(16, false),
            b'f' => Ok(Ty::Float(4)),
            b'd' => Ok(Ty::Float(8)),
            b'D' => Ok(Ty::Float(16)),
            b'v' => Ok(Ty::Void),
            b'*' | b'#' | b':' => Ok(Ty::Pointer),
            b'@' => {
                match self.peek() {
                    // `@"NSString"`: an object of a named class.
                    Some(b'"') => {
                        let close = self.s[self.pos + 1..].iter().position(|&b| b == b'"');
                        self.pos = close.map_or(self.s.len(), |p| self.pos + p + 2);
                    }
                    // `@?`, a block, maybe with its signature in `<...>`.
                    Some(b'?') => {
                        self.pos += 1;
                        if self.peek() == Some(b'<') {
                            self.pos += crate::encoding::bracketed_len(&self.s[self.pos..]);
                        }
                    }
                    _ => {}
                }
                Ok(Ty::Pointer)
            }
            b'^' => {
                // Whatever it points to, even what can't be laid out.
                self.pos += crate::encoding::type_len(&self.s[self.pos..]);
                Ok(Ty::Pointer)
            }
            b'[' => {
                let digits = self.s[self.pos..].iter().take_while(|b| b.is_ascii_digit()).count();
                let count = std::str::from_utf8(&self.s[self.pos..self.pos + digits]).ok().and_then(|n| n.parse().ok());
                self.pos += digits;
                let elem = self.ty()?;
                let Some(count) = count.filter(|_| self.peek() == Some(b']')) else {
                    return Err(self.unsupported(start));
                };
                self.pos += 1;
                Ok(Ty::Array { elem: Box::new(elem), count })
            }
            b'{' => {
                let name = self.s[self.pos..].iter().position(|&b| b == b'=' || b == b'}');
                let Some(name) = name else { return Err(self.unsupported(start)) };
                self.pos += name;
                let mut fields = Vec::new();
                if self.peek() == Some(b'=') {
                    self.pos += 1;
                    while self.peek() != Some(b'}') {
                        if self.peek().is_none() {
                            return Err(self.unsupported(start));
                        }
                        // Field names, when present, are quoted.
                        if self.peek() == Some(b'"') {
                            let close = self.s[self.pos + 1..].iter().position(|&b| b == b'"');
                            let Some(close) = close else { return Err(self.unsupported(start)) };
                            self.pos += close + 2;
                        }
                        fields.push(self.ty()?);
                    }
                }
                self.pos += 1;
                let (mut size, mut align) = (0usize, 1usize);
                for field in &fields {
                    align = usize::max(align, field.align());
                    size = size.next_multiple_of(field.align()) + field.size();
                }
                Ok(Ty::Struct { fields, size: size.next_multiple_of(align), align })
            }
            _ => Err(self.unsupported(start)),
        }
    }
}

/// Assigns registers and stack slots to arguments in order.
#[derive(Default)]
struct Layout {
    gprs: u8,
    fprs: u8,
    /// Bytes of stack arguments so far.
    stack: usize,
    /// Bytes of copies of structs passed by address.
    indirect: usize,
}

impl Layout {
    /// A value on the stack, at the next slot aligned for it.
    fn on_stack(&mut self, size: usize, align: usize, ext: Ext) -> Passing {
        let at = self.stack.next_multiple_of(align.max(8));
        self.stack = at + size.next_multiple_of(8);
        pieces(&[Piece { loc: Loc::Stack(at as u32), offset: 0, len: size as u32, ext }])
    }
}

fn pieces(list: &[Piece]) -> Passing {
    let mut array = [NO_PIECE; 4];
    array[..list.len()].copy_from_slice(list);
    Passing::Pieces(array, list.len() as u8)
}

/// How a scalar integer extends.
fn ext_of(ty: &Ty) -> Ext {
    match *ty {
        Ty::Int { size, signed } if size < 8 => {
            if signed {
                Ext::Sign
            } else {
                Ext::Zero
            }
        }
        _ => Ext::None,
    }
}

/// Whether `ty` is made of one to four floating-point values of one type
/// with no padding (a homogeneous floating-point aggregate, or a lone
/// float), and if so how many and how big.
#[cfg(target_arch = "aarch64")]
fn hfa(ty: &Ty) -> Option<(usize, u8)> {
    let mut leaves = Vec::new();
    ty.leaves(0, &mut leaves, 4)?;
    let Some(&(_, Leaf::Float(size))) = leaves.first() else { return None };
    let uniform =
        leaves.iter().enumerate().all(|(i, &(at, leaf))| leaf == Leaf::Float(size) && at == i * size as usize);
    (uniform && ty.size() == leaves.len() * size as usize).then_some((leaves.len(), size))
}

#[cfg(target_arch = "aarch64")]
impl Layout {
    /// A pointer-sized value: the next integer register, or a stack slot.
    fn word(&mut self) -> Loc {
        if self.gprs < GPR_ARGS {
            self.gprs += 1;
            Loc::Gpr(self.gprs - 1)
        } else {
            let at = self.stack;
            self.stack += 8;
            Loc::Stack(at as u32)
        }
    }

    /// AAPCS64's rules for one argument, as Linux applies them (stack
    /// arguments take 8-byte slots).
    fn argument(&mut self, ty: &Ty) -> Passing {
        let (size, align) = (ty.size(), ty.align());
        if size == 0 {
            return Passing::Nothing;
        }
        if let Some((n, elem)) = hfa(ty) {
            if self.fprs as usize + n <= FPR_ARGS as usize {
                let list: Vec<Piece> = (0..n)
                    .map(|i| Piece {
                        loc: Loc::Fpr(self.fprs + i as u8),
                        offset: (i * elem as usize) as u32,
                        len: elem as u32,
                        ext: Ext::None,
                    })
                    .collect();
                self.fprs += n as u8;
                return pieces(&list);
            }
            self.fprs = FPR_ARGS;
            return self.on_stack(size, align, Ext::None);
        }
        if size > 16 {
            self.indirect += size.next_multiple_of(16);
            return Passing::Indirect(self.word());
        }
        let words = size.div_ceil(8) as u8;
        if align == 16 {
            self.gprs = self.gprs.next_multiple_of(2);
        }
        if self.gprs + words <= GPR_ARGS {
            let list: Vec<Piece> = (0..words)
                .map(|i| Piece {
                    loc: Loc::Gpr(self.gprs + i),
                    offset: 8 * i as u32,
                    len: (size - 8 * i as usize).min(8) as u32,
                    ext: ext_of(ty),
                })
                .collect();
            self.gprs += words;
            return pieces(&list);
        }
        self.gprs = GPR_ARGS;
        self.on_stack(size, align, ext_of(ty))
    }

    /// The return value: floating-point aggregates in v0-v3, up to 16
    /// other bytes in x0 and x1, anything larger through x8.
    fn return_value(&mut self, ty: &Ty) -> Passing {
        let size = ty.size();
        if size == 0 {
            return Passing::Nothing;
        }
        if let Some((n, elem)) = hfa(ty) {
            let list: Vec<Piece> = (0..n)
                .map(|i| Piece {
                    loc: Loc::Fpr(i as u8),
                    offset: (i * elem as usize) as u32,
                    len: elem as u32,
                    ext: Ext::None,
                })
                .collect();
            return pieces(&list);
        }
        if size > 16 {
            return Passing::Indirect(Loc::Gpr(INDIRECT_RESULT));
        }
        let list: Vec<Piece> = (0..size.div_ceil(8))
            .map(|i| Piece {
                loc: Loc::Gpr(GPR_RETURN[i]),
                offset: 8 * i as u32,
                len: (size - 8 * i).min(8) as u32,
                ext: ext_of(ty),
            })
            .collect();
        pieces(&list)
    }
}

/// The System V classes of an eightbyte.
#[cfg(target_arch = "x86_64")]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Integer,
    Sse,
}

/// The classes of the eightbytes of `ty`, or `None` if it goes in memory
/// (larger than 16 bytes, or holding a `long double`).
#[cfg(target_arch = "x86_64")]
fn classify(ty: &Ty) -> Option<Vec<Class>> {
    let size = ty.size();
    if size > 16 {
        return None;
    }
    let mut leaves = Vec::new();
    ty.leaves(0, &mut leaves, 16)?;
    let mut classes = vec![None; size.div_ceil(8)];
    for (at, leaf) in leaves {
        let (len, class) = match leaf {
            Leaf::Int(len) => (len as usize, Class::Integer),
            Leaf::Float(16) => return None,
            Leaf::Float(len) => (len as usize, Class::Sse),
        };
        for slot in &mut classes[at / 8..(at + len).div_ceil(8)] {
            *slot = Some(match (*slot, class) {
                (Some(Class::Integer), _) | (_, Class::Integer) => Class::Integer,
                _ => Class::Sse,
            });
        }
    }
    // An eightbyte of padding only is no register's business, but a
    // struct laid out by C's rules has none.
    Some(classes.into_iter().map(|c| c.unwrap_or(Class::Sse)).collect())
}

#[cfg(target_arch = "x86_64")]
impl Layout {
    /// The System V rules for one argument: small values by eightbyte into
    /// registers when all of them fit, everything else on the stack.
    fn argument(&mut self, ty: &Ty) -> Passing {
        let (size, align) = (ty.size(), ty.align());
        if size == 0 {
            return Passing::Nothing;
        }
        let Some(classes) = classify(ty) else { return self.on_stack(size, align, Ext::None) };
        let ints = classes.iter().filter(|&&c| c == Class::Integer).count() as u8;
        let sses = classes.len() as u8 - ints;
        if self.gprs + ints > GPR_ARGS || self.fprs + sses > FPR_ARGS {
            return self.on_stack(size, align, ext_of(ty));
        }
        let list: Vec<Piece> = classes
            .iter()
            .enumerate()
            .map(|(i, class)| {
                let loc = match class {
                    Class::Integer => {
                        self.gprs += 1;
                        Loc::Gpr(self.gprs - 1)
                    }
                    Class::Sse => {
                        self.fprs += 1;
                        Loc::Fpr(self.fprs - 1)
                    }
                };
                Piece { loc, offset: 8 * i as u32, len: (size - 8 * i).min(8) as u32, ext: ext_of(ty) }
            })
            .collect();
        pieces(&list)
    }

    /// The return value: by eightbyte into rax and rdx or xmm0 and xmm1,
    /// or in memory at an address the caller passes as a hidden first
    /// argument.
    fn return_value(&mut self, ty: &Ty) -> Passing {
        let size = ty.size();
        if size == 0 {
            return Passing::Nothing;
        }
        if matches!(ty, Ty::Float(16)) {
            return Passing::X87;
        }
        let Some(classes) = classify(ty) else {
            self.gprs = 1;
            return Passing::Indirect(Loc::Gpr(INDIRECT_RESULT));
        };
        let (mut ints, mut sses) = (0, 0);
        let list: Vec<Piece> = classes
            .iter()
            .enumerate()
            .map(|(i, class)| {
                let loc = match class {
                    Class::Integer => {
                        ints += 1;
                        Loc::Gpr(GPR_RETURN[ints - 1])
                    }
                    Class::Sse => {
                        sses += 1;
                        Loc::Fpr(sses - 1)
                    }
                };
                Piece { loc, offset: 8 * i as u32, len: (size - 8 * i).min(8) as u32, ext: ext_of(ty) }
            })
            .collect();
        pieces(&list)
    }
}

/// What finishes forwarding a message that `-forwardingTargetForSelector:`
/// didn't send elsewhere: Foundation's, which asks the receiver for a
/// method signature and sends it `-forwardInvocation:` (see
/// [`set_forward_handler`]).
pub type ForwardHandler = unsafe fn(receiver: &AnyObject, sel: objc2::runtime::Sel, frame: &mut Frame);

static FORWARD_HANDLER: AtomicPtr<()> = AtomicPtr::new(std::ptr::null_mut());

/// Install the function that forwards messages through
/// `-forwardInvocation:`. Until one is installed, a message nothing
/// implements or forwards to another object is unrecognized. Foundation
/// installs its own when `NSObject` loads, which the runtime makes sure of
/// before it forwards a message.
pub fn set_forward_handler(handler: ForwardHandler) {
    FORWARD_HANDLER.store(handler as *mut (), Ordering::Release);
}

/// `[receiver doesNotRecognizeSelector:sel]`, which raises (panics), for
/// a forwarded message nobody takes.
pub fn does_not_recognize(receiver: &AnyObject, sel: objc2::runtime::Sel) -> ! {
    let receiver = (receiver as *const AnyObject).cast_mut().cast::<crate::object::Object>();
    // SAFETY: a live receiver; objc2's `Sel` is the runtime's selector
    // pointer.
    unsafe {
        crate::message::does_not_recognize(
            receiver,
            std::mem::transmute::<objc2::runtime::Sel, crate::selector::Sel>(sel),
        )
    }
}

/// The forward handler, if one is installed. Foundation installs its own
/// from its category on `NSObject`, so a program that hasn't used
/// `NSObject` yet (only `NSProxy`, say) has `NSObject` loaded first.
pub(crate) fn forward_handler() -> Option<ForwardHandler> {
    let mut handler = FORWARD_HANDLER.load(Ordering::Acquire);
    if handler.is_null() {
        crate::class::ensure_loaded(&crate::nsobject::NSOBJECT_CLASS);
        handler = FORWARD_HANDLER.load(Ordering::Acquire);
    }
    // SAFETY: only `ForwardHandler`s are stored.
    (!handler.is_null()).then(|| unsafe { std::mem::transmute::<*mut (), ForwardHandler>(handler) })
}

unsafe extern "C-unwind" {
    /// Loads `regs` into the argument registers, copies `stack_len` bytes
    /// of stack arguments from `stack` below its frame, calls `imp`, and
    /// stores the return registers back into `regs`.
    fn sidestep_invoke(regs: *mut Registers, stack: *const u8, stack_len: usize, imp: Imp);
}

// The frame: x29/x30 and x19/x20, with x29 as the frame's base so the
// stack arguments can take any room below it. x19 keeps the block across
// the call.
#[cfg(target_arch = "aarch64")]
std::arch::global_asm!(
    ".text",
    ".p2align 2",
    ".globl sidestep_invoke",
    ".hidden sidestep_invoke",
    ".type sidestep_invoke, %function",
    "sidestep_invoke:",
    ".cfi_startproc",
    "hint #34",
    "stp x29, x30, [sp, #-32]!",
    ".cfi_def_cfa_offset 32",
    ".cfi_offset x29, -32",
    ".cfi_offset x30, -24",
    "mov x29, sp",
    ".cfi_def_cfa_register x29",
    "stp x19, x20, [sp, #16]",
    ".cfi_offset x19, -16",
    ".cfi_offset x20, -8",
    "mov x19, x0",
    "mov x16, x3",
    "sub sp, sp, x2",
    "mov x9, #0",
    "2:",
    "cmp x9, x2",
    "b.hs 3f",
    "ldr x10, [x1, x9]",
    "str x10, [sp, x9]",
    "add x9, x9, #8",
    "b 2b",
    "3:",
    "ldp q0, q1, [x19, #80]",
    "ldp q2, q3, [x19, #112]",
    "ldp q4, q5, [x19, #144]",
    "ldp q6, q7, [x19, #176]",
    "ldr x8, [x19, #64]",
    "ldp x6, x7, [x19, #48]",
    "ldp x4, x5, [x19, #32]",
    "ldp x2, x3, [x19, #16]",
    "ldp x0, x1, [x19, #0]",
    "blr x16",
    "stp x0, x1, [x19, #0]",
    "stp q0, q1, [x19, #80]",
    "stp q2, q3, [x19, #112]",
    "mov sp, x29",
    "ldp x19, x20, [sp, #16]",
    "ldp x29, x30, [sp], #32",
    ".cfi_def_cfa sp, 0",
    ".cfi_restore x29",
    ".cfi_restore x30",
    ".cfi_restore x19",
    ".cfi_restore x20",
    "ret",
    ".cfi_endproc",
    ".size sidestep_invoke, . - sidestep_invoke",
);

// The frame: rbp, rbx and r12, with rbp as the frame's base so the stack
// arguments can take any room below it (a multiple of 16, which keeps the
// call aligned). rbx keeps the block across the call.
#[cfg(target_arch = "x86_64")]
std::arch::global_asm!(
    ".text",
    ".p2align 4",
    ".globl sidestep_invoke",
    ".hidden sidestep_invoke",
    ".type sidestep_invoke, @function",
    "sidestep_invoke:",
    ".cfi_startproc",
    "push rbp",
    ".cfi_def_cfa_offset 16",
    ".cfi_offset rbp, -16",
    "mov rbp, rsp",
    ".cfi_def_cfa_register rbp",
    "push rbx",
    "push r12",
    ".cfi_offset rbx, -24",
    ".cfi_offset r12, -32",
    "mov rbx, rdi",
    "mov r12, rcx",
    "sub rsp, rdx",
    "xor eax, eax",
    "2:",
    "cmp rax, rdx",
    "jae 3f",
    "mov r10, qword ptr [rsi + rax]",
    "mov qword ptr [rsp + rax], r10",
    "add rax, 8",
    "jmp 2b",
    "3:",
    "movdqu xmm0, xmmword ptr [rbx]",
    "movdqu xmm1, xmmword ptr [rbx + 16]",
    "movdqu xmm2, xmmword ptr [rbx + 32]",
    "movdqu xmm3, xmmword ptr [rbx + 48]",
    "movdqu xmm4, xmmword ptr [rbx + 64]",
    "movdqu xmm5, xmmword ptr [rbx + 80]",
    "movdqu xmm6, xmmword ptr [rbx + 96]",
    "movdqu xmm7, xmmword ptr [rbx + 112]",
    "mov rdi, qword ptr [rbx + 128]",
    "mov rsi, qword ptr [rbx + 136]",
    "mov rdx, qword ptr [rbx + 144]",
    "mov rcx, qword ptr [rbx + 152]",
    "mov r8, qword ptr [rbx + 160]",
    "mov r9, qword ptr [rbx + 168]",
    "mov rax, qword ptr [rbx + 176]",
    "call r12",
    "mov qword ptr [rbx + 176], rax",
    "mov qword ptr [rbx + 144], rdx",
    "movdqu xmmword ptr [rbx], xmm0",
    "movdqu xmmword ptr [rbx + 16], xmm1",
    "lea rsp, [rbp - 16]",
    "pop r12",
    "pop rbx",
    "pop rbp",
    ".cfi_def_cfa rsp, 8",
    ".cfi_restore rbp",
    ".cfi_restore rbx",
    ".cfi_restore r12",
    "ret",
    ".cfi_endproc",
    ".size sidestep_invoke, . - sidestep_invoke",
);

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(types: &str) -> Signature {
        Signature::parse(types.as_bytes()).unwrap().unwrap()
    }

    fn arg_types(sig: &Signature) -> Vec<String> {
        sig.arguments().iter().map(|a| a.types().to_str().unwrap().to_owned()).collect()
    }

    #[test]
    fn parses_like_foundation() {
        let s = sig("i24@0:8i16");
        assert_eq!(arg_types(&s), ["@", ":", "i"]);
        assert_eq!((s.return_value().types(), s.return_value().size()), (c"i", 4));
        let s = sig("Vv@:r*@\"NSString\"@?<v@?>[4i]^{Foo=i}^(U=ic){?=\"a\"i\"b\"d}Q16");
        assert_eq!(
            arg_types(&s),
            ["@", ":", "r*", "@\"NSString\"", "@?<v@?>", "[4i]", "^{Foo=i}", "^(U=ic)", "{?=\"a\"i\"b\"d}", "Q"]
        );
        assert!(s.is_oneway());
        let kinds: Vec<ValueKind> = s.arguments().iter().map(Value::kind).collect();
        assert_eq!(
            kinds[..5],
            [ValueKind::Object, ValueKind::Plain, ValueKind::CString, ValueKind::Object, ValueKind::Block]
        );
        assert_eq!(s.arguments()[5].size(), 16);
        assert_eq!(s.arguments()[8].size(), 16);
        assert!(Signature::parse(b"").unwrap().is_none());
    }

    #[test]
    fn refuses_what_foundation_refuses() {
        let err = Signature::parse(b"v@:b3").err().unwrap();
        assert_eq!(err, Unsupported { spec: 'b', at: "b3".into(), within: "b3".into(), union: false });
        let err = Signature::parse(b"x@:").err().unwrap();
        assert_eq!((err.spec, err.at.as_str(), err.within.as_str()), ('x', "x@:", "x@:"));
        let err = Signature::parse(b"v@:{S=i(U=id)}").err().unwrap();
        assert_eq!((err.spec, err.at.as_str(), err.within.as_str(), err.union), ('(', "(U=id)}", "{S=i(U=id)}", true));
        for bad in ["v@:?", "v@:jd", "Ad@:", "v@:[4", "v@:{S=i"] {
            assert!(Signature::parse(bad.as_bytes()).is_err(), "{bad}");
        }
    }

    #[test]
    fn extends_narrow_integers() {
        assert_eq!(extend(0xfd, 1, Ext::Sign), (-3i64) as u64);
        assert_eq!(extend(0xfffd, 2, Ext::Zero), 0xfffd);
        assert_eq!(extend(0xdead_0000_00fd, 1, Ext::Zero), 0xfd);
        assert_eq!(extend(u64::MAX, 8, Ext::Sign), u64::MAX);
    }

    fn passing(types: &str) -> Vec<Passing> {
        sig(types).arguments()[2..].iter().map(|a| a.passing).collect()
    }

    fn regs(list: &[(Loc, u32, u32)]) -> Passing {
        let list: Vec<Piece> =
            list.iter().map(|&(loc, offset, len)| Piece { loc, offset, len, ext: Ext::None }).collect();
        pieces(&list)
    }

    #[test]
    #[cfg(target_arch = "aarch64")]
    fn lays_out_aarch64() {
        use Loc::{Fpr, Gpr, Stack};
        // Floats and doubles each take a register; a struct of three
        // floats takes three.
        assert_eq!(
            passing("v@:f{F=fff}d"),
            [regs(&[(Fpr(0), 0, 4)]), regs(&[(Fpr(1), 0, 4), (Fpr(2), 4, 4), (Fpr(3), 8, 4)]), regs(&[(Fpr(4), 0, 8)])]
        );
        // A struct of doubles that doesn't fit uses up the registers.
        assert_eq!(
            passing("v@:dddddd{P=dd}{P=dd}d")[6..],
            [regs(&[(Fpr(6), 0, 8), (Fpr(7), 8, 8)]), regs(&[(Stack(0), 0, 16)]), regs(&[(Stack(16), 0, 8)])]
        );
        // Mixed and integer structs of up to 16 bytes take integer
        // registers; larger ones go by address.
        assert_eq!(passing("v@:{M=dq}{W=[5q]}"), [regs(&[(Gpr(2), 0, 8), (Gpr(3), 8, 8)]), Passing::Indirect(Gpr(4))]);
        // A struct that doesn't fit the registers left uses them up.
        assert_eq!(passing("v@:qqqqq{R=QQ}q")[5..], [regs(&[(Stack(0), 0, 16)]), regs(&[(Stack(16), 0, 8)])]);
        let s = sig("{W=[5q]}@:");
        assert_eq!(s.return_value().passing, Passing::Indirect(Gpr(8)));
        let s = sig("{R={P=dd}{S=dd}}@:");
        assert_eq!(s.return_value().passing, regs(&[(Fpr(0), 0, 8), (Fpr(1), 8, 8), (Fpr(2), 16, 8), (Fpr(3), 24, 8)]));
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn lays_out_x86_64() {
        use Loc::{Fpr, Gpr, Stack};
        // Two floats share an eightbyte; a float and an int make it an
        // integer eightbyte.
        assert_eq!(
            passing("v@:{F=fff}{G=fi}{M=dq}"),
            [regs(&[(Fpr(0), 0, 8), (Fpr(1), 8, 4)]), regs(&[(Gpr(2), 0, 8)]), regs(&[(Fpr(2), 0, 8), (Gpr(3), 8, 8)])]
        );
        // A struct that doesn't fit goes on the stack, leaving the register
        // for what follows.
        assert_eq!(passing("v@:qqq{R=QQ}q")[3..], [regs(&[(Stack(0), 0, 16)]), regs(&[(Gpr(5), 0, 8)])]);
        // Large structs are copied onto the stack.
        assert_eq!(passing("v@:{W=[5q]}"), [regs(&[(Stack(0), 0, 40)])]);
        // A struct returned in memory takes rdi, so the receiver is in rsi.
        let s = sig("{W=[5q]}@:q");
        assert_eq!(s.return_value().passing, Passing::Indirect(Gpr(0)));
        assert_eq!(s.arguments()[0].passing, regs(&[(Gpr(1), 0, 8)]));
        let s = sig("{M=dq}@:");
        assert_eq!(s.return_value().passing, regs(&[(Fpr(0), 0, 8), (Gpr(6), 8, 8)]));
    }

    #[repr(C)]
    #[derive(Debug, PartialEq, Clone, Copy)]
    struct Mixed {
        d: f64,
        c: i8,
    }

    #[repr(C)]
    #[derive(Debug, PartialEq, Clone, Copy)]
    struct Pair {
        a: i64,
        b: i64,
    }

    #[allow(clippy::too_many_arguments)]
    extern "C" fn many(a: i8, b: u16, c: f32, d: f64, e: Mixed, f: Pair, g: i64, h: i64, i: i64, j: f64) -> Mixed {
        let ints = a as f64 + b as f64 + f.a as f64 + f.b as f64 + (g + h + i) as f64;
        Mixed { d: ints + c as f64 + d + e.d + j, c: e.c.wrapping_mul(2) }
    }

    /// A call through the assembly routine, with values in every kind of
    /// place. The implementation is a plain function, so it is described
    /// without a receiver and selector.
    #[test]
    fn calls_what_it_lays_out() {
        let s = sig("{Mixed=dc}cSfd{Mixed=dc}{Pair=qq}qqqd");
        let mut values = vec![0u128; s.values_size() / 16];
        let buffer = values.as_mut_ptr().cast::<u8>();
        let put = |i: usize, bytes: &[u8]| {
            let arg = &s.arguments()[i];
            assert_eq!(arg.size(), bytes.len());
            // SAFETY: within the buffer.
            unsafe { buffer.add(arg.offset()).copy_from_nonoverlapping(bytes.as_ptr(), bytes.len()) };
        };
        let mixed = Mixed { d: 0.5, c: 21 };
        let pair = Pair { a: 10, b: 20 };
        put(0, &(-3i8).to_ne_bytes());
        put(1, &60000u16.to_ne_bytes());
        put(2, &1.5f32.to_ne_bytes());
        put(3, &2.25f64.to_ne_bytes());
        // SAFETY: the bytes of `repr(C)` values, padding left as it is.
        put(4, unsafe { std::slice::from_raw_parts((&raw const mixed).cast(), 16) });
        // SAFETY: as above.
        put(5, unsafe { std::slice::from_raw_parts((&raw const pair).cast(), 16) });
        put(6, &100i64.to_ne_bytes());
        put(7, &200i64.to_ne_bytes());
        put(8, &300i64.to_ne_bytes());
        put(9, &0.125f64.to_ne_bytes());
        let f: extern "C" fn(i8, u16, f32, f64, Mixed, Pair, i64, i64, i64, f64) -> Mixed = many;
        // SAFETY: the signature describes `many`.
        unsafe { s.call(std::mem::transmute::<*const (), Imp>(f as *const ()), buffer) };
        let ret = s.return_value();
        // SAFETY: the return value's bytes, written by the call.
        let got = unsafe { buffer.add(ret.offset()).cast::<Mixed>().read() };
        assert_eq!(got, many(-3, 60000, 1.5, 2.25, mixed, pair, 100, 200, 300, 0.125));
    }
}
