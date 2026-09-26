//! A vector with room at both ends: the element buffer of `NSMutableArray`.
//!
//! Foundation's mutable arrays add and remove at either end in constant
//! time, so apps use them as queues; a `Vec` moves every element to change
//! its front. A [`Deque`] keeps its elements side by side, so it derefs to a
//! slice and fast enumeration can hand out its buffer, with spare room
//! before and after them. Adding at the front uses the room before,
//! removing from the front leaves more of it, and a change in the middle
//! moves whichever side of it is shorter.
//!
//! When an end runs out of room, the elements move within the buffer if at
//! least half of it would stay free, or else into a buffer twice the size.
//! The end that ran out gets half of the spare room; the other end keeps
//! what it had, up to the other half, so an array that is only appended to
//! keeps all its spare room at the back, as a `Vec` does. Each move follows
//! at least as many cheap changes as it moves elements, so a change at
//! either end costs constant time on average.

use std::alloc::{Layout, alloc, dealloc, handle_alloc_error};
use std::marker::PhantomData;
use std::ops::{Deref, Range};
use std::ptr::{self, NonNull};

pub(crate) struct Deque<T> {
    /// A buffer of `cap` slots, of which `head..head + len` hold elements.
    ptr: NonNull<T>,
    cap: usize,
    head: usize,
    len: usize,
    owns: PhantomData<T>,
}

impl<T> Default for Deque<T> {
    fn default() -> Self {
        Deque::new()
    }
}

/// A buffer for `cap` elements, allocated as a `Vec` would.
fn allocate<T>(cap: usize) -> NonNull<T> {
    let layout = Layout::array::<T>(cap).expect("capacity overflow");
    // SAFETY: `cap` is non-zero and `T` is not zero-sized (see `Deque::new`),
    // so the layout has a non-zero size.
    let raw = unsafe { alloc(layout) };
    match NonNull::new(raw) {
        Some(p) => p.cast(),
        None => handle_alloc_error(layout),
    }
}

/// Move `count` elements from `from` to `to`, as `ptr::copy` does, without
/// calling `memmove` for none: changes at the ends move none.
///
/// # Safety
/// As for `ptr::copy`.
#[inline]
unsafe fn shift<T>(from: *const T, to: *mut T, count: usize) {
    if count > 0 {
        // SAFETY: guaranteed by the caller.
        unsafe { ptr::copy(from, to, count) };
    }
}

/// Free a buffer from `allocate`, or from a `Vec`, of `cap` elements.
///
/// # Safety
/// `ptr` must be such a buffer, holding no elements any more.
unsafe fn free<T>(ptr: NonNull<T>, cap: usize) {
    if cap > 0 {
        // SAFETY: allocated with this layout, which was valid then.
        unsafe { dealloc(ptr.as_ptr().cast(), Layout::array::<T>(cap).unwrap_unchecked()) };
    }
}

impl<T> Deque<T> {
    pub(crate) const fn new() -> Self {
        const { assert!(size_of::<T>() != 0, "no zero-sized elements") };
        Deque { ptr: NonNull::dangling(), cap: 0, head: 0, len: 0, owns: PhantomData }
    }

    /// The elements of `items`, keeping its buffer.
    pub(crate) fn from_vec(items: Vec<T>) -> Self {
        let _ = Deque::<T>::new();
        let mut items = std::mem::ManuallyDrop::new(items);
        let (len, cap) = (items.len(), items.capacity());
        // SAFETY: a Vec's pointer is never null (it dangles without capacity).
        let ptr = unsafe { NonNull::new_unchecked(items.as_mut_ptr()) };
        // The buffer was allocated as `allocate` does, so `free` frees it.
        Deque { ptr, cap, head: 0, len, owns: PhantomData }
    }

    #[inline]
    pub(crate) fn as_slice(&self) -> &[T] {
        // SAFETY: `head..head + len` hold elements.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr().add(self.head), self.len) }
    }

    #[inline]
    pub(crate) fn as_mut_slice(&mut self) -> &mut [T] {
        // SAFETY: `head..head + len` hold elements, borrowed mutably with self.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr().add(self.head), self.len) }
    }

    /// Slot `i` of the buffer, which may hold an element or not.
    #[inline]
    fn slot(&self, i: usize) -> *mut T {
        debug_assert!(i <= self.cap);
        // SAFETY: within the allocation, or one past its end.
        unsafe { self.ptr.as_ptr().add(i) }
    }

    pub(crate) fn push(&mut self, item: T) {
        self.reserve_back(1);
        // SAFETY: `reserve_back` left a free slot after the last element.
        unsafe { self.slot(self.head + self.len).write(item) };
        self.len += 1;
    }

    pub(crate) fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        // SAFETY: the slot held the last element, which is no longer counted.
        Some(unsafe { self.slot(self.head + self.len).read() })
    }

    /// Put `item` at `index`, moving the shorter side of it by one.
    pub(crate) fn insert(&mut self, index: usize, item: T) {
        assert!(index <= self.len, "insertion index out of bounds");
        if index < self.len - index {
            self.reserve_front(1);
            // SAFETY: the `index` elements before the new one move down into
            // the free slot `reserve_front` left before the first.
            unsafe { shift(self.slot(self.head), self.slot(self.head - 1), index) };
            self.head -= 1;
        } else {
            self.reserve_back(1);
            let at = self.head + index;
            // SAFETY: the elements from `index` on move up into the free slot
            // `reserve_back` left after the last.
            unsafe { shift(self.slot(at), self.slot(at + 1), self.len - index) };
        }
        // SAFETY: the slot at `index` is free now.
        unsafe { self.slot(self.head + index).write(item) };
        self.len += 1;
    }

    /// Take out the element at `index`, moving the shorter side by one.
    pub(crate) fn remove(&mut self, index: usize) -> T {
        assert!(index < self.len, "removal index out of bounds");
        let at = self.head + index;
        // SAFETY: the slot holds an element, which this takes; its slot is
        // filled or dropped from the range below.
        let item = unsafe { self.slot(at).read() };
        let after = self.len - 1 - index;
        if index < after {
            // SAFETY: the elements before it move up by one.
            unsafe { shift(self.slot(self.head), self.slot(self.head + 1), index) };
            self.head += 1;
        } else {
            // SAFETY: the elements after it move down by one.
            unsafe { shift(self.slot(at + 1), self.slot(at), after) };
        }
        self.len -= 1;
        item
    }

    /// Take out the elements in `range`, in order.
    pub(crate) fn drain(&mut self, range: Range<usize>) -> Vec<T> {
        let Range { start, end } = range;
        assert!(start <= end && end <= self.len, "drain range out of bounds");
        let n = end - start;
        let mut out = Vec::with_capacity(n);
        let after = self.len - end;
        // SAFETY: the `n` elements move into `out`, which has room for them,
        // and then the shorter side of the gap closes it. Nothing between
        // can panic, so no element is ever owned twice.
        unsafe {
            ptr::copy_nonoverlapping(self.slot(self.head + start), out.as_mut_ptr(), n);
            out.set_len(n);
            if start < after {
                ptr::copy(self.slot(self.head), self.slot(self.head + n), start);
                self.head += n;
            } else {
                ptr::copy(self.slot(self.head + end), self.slot(self.head + start), after);
            }
        }
        self.len -= n;
        out
    }

    /// Put `items` in at `index`, in order, moving the shorter side.
    pub(crate) fn insert_all(&mut self, index: usize, mut items: Vec<T>) {
        assert!(index <= self.len, "insertion index out of bounds");
        let n = items.len();
        if n == 0 {
            return;
        }
        if index < self.len - index {
            self.reserve_front(n);
            // SAFETY: the elements before `index` move down into the room
            // `reserve_front` left.
            unsafe { ptr::copy(self.slot(self.head), self.slot(self.head - n), index) };
            self.head -= n;
        } else {
            self.reserve_back(n);
            let at = self.head + index;
            // SAFETY: the elements from `index` on move up into the room
            // `reserve_back` left.
            unsafe { ptr::copy(self.slot(at), self.slot(at + n), self.len - index) };
        }
        // SAFETY: `n` free slots from `index`; the elements move out of
        // `items`, which then frees only its buffer.
        unsafe {
            ptr::copy_nonoverlapping(items.as_ptr(), self.slot(self.head + index), n);
            items.set_len(0);
        }
        self.len += n;
    }

    /// Put `items` in after the last element.
    pub(crate) fn extend(&mut self, items: Vec<T>) {
        self.insert_all(self.len, items);
    }

    /// Take out the elements whose positions `marked` flags, keeping the
    /// rest in order.
    pub(crate) fn remove_marked(&mut self, marked: &[bool]) -> Vec<T> {
        let mut removed = Vec::with_capacity(marked.iter().take(self.len).filter(|&&m| m).count());
        let mut kept = 0;
        for i in 0..self.len {
            let from = self.slot(self.head + i);
            if marked.get(i).copied().unwrap_or(false) {
                // SAFETY: the element moves out; `removed` has room, so the
                // push can't allocate or panic.
                removed.push(unsafe { from.read() });
            } else {
                if kept != i {
                    // SAFETY: the element moves down into a slot vacated
                    // before it.
                    unsafe { ptr::copy_nonoverlapping(from, self.slot(self.head + kept), 1) };
                }
                kept += 1;
            }
        }
        self.len = kept;
        removed
    }

    #[inline]
    fn reserve_back(&mut self, n: usize) {
        if self.cap - self.head - self.len < n {
            self.relocate(n, false);
        }
    }

    #[inline]
    fn reserve_front(&mut self, n: usize) {
        if self.head < n {
            self.relocate(n, true);
        }
    }

    /// Make room for `n` more elements at one end (see the module notes).
    #[cold]
    fn relocate(&mut self, n: usize, front: bool) {
        let needed = self.len.checked_add(n).expect("capacity overflow");
        let cap = if needed <= self.cap / 2 { self.cap } else { needed.max(self.cap.saturating_mul(2)).max(4) };
        let spare = cap - needed;
        let head = if front { n + spare / 2 } else { self.head.min(spare / 2) };
        if cap == self.cap {
            // SAFETY: the elements move within the buffer; `head + len + n`
            // is at most `cap` either way.
            unsafe { ptr::copy(self.slot(self.head), self.slot(head), self.len) };
        } else {
            let fresh = allocate::<T>(cap);
            // SAFETY: the elements move to the new buffer, which has room,
            // and the old one, now empty, is freed.
            unsafe {
                ptr::copy_nonoverlapping(self.slot(self.head), fresh.as_ptr().add(head), self.len);
                free(self.ptr, self.cap);
            }
            self.ptr = fresh;
            self.cap = cap;
        }
        self.head = head;
    }
}

impl<T> Deref for Deque<T> {
    type Target = [T];

    #[inline]
    fn deref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T: Clone> Clone for Deque<T> {
    fn clone(&self) -> Self {
        Deque::from_vec(self.as_slice().to_vec())
    }
}

impl<T> Drop for Deque<T> {
    fn drop(&mut self) {
        /// Frees the buffer even if dropping an element panics.
        struct Buffer<T>(NonNull<T>, usize);
        impl<T> Drop for Buffer<T> {
            fn drop(&mut self) {
                // SAFETY: the deque's buffer, emptied by now.
                unsafe { free(self.0, self.1) };
            }
        }
        let _buffer = Buffer(self.ptr, self.cap);
        // SAFETY: the elements, dropped once each.
        unsafe { ptr::drop_in_place(self.as_mut_slice()) };
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::rc::Rc;

    use super::Deque;

    /// Random changes, checked against `VecDeque`, with elements that count
    /// their drops.
    #[test]
    fn matches_a_model() {
        let tracker = Rc::new(());
        let mut deque: Deque<(usize, Rc<()>)> = Deque::new();
        let mut model: VecDeque<usize> = VecDeque::new();
        let mut x: u64 = 0x9e37_79b9;
        let mut next = 0;
        for step in 0..20_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let len = model.len();
            let at = if len == 0 { 0 } else { (x >> 8) as usize % (len + 1) };
            let mut item = || {
                next += 1;
                (next, tracker.clone())
            };
            match x % 11 {
                0 | 1 => {
                    let it = item();
                    model.push_back(it.0);
                    deque.push(it);
                }
                2 => assert_eq!(deque.pop().map(|e| e.0), model.pop_back()),
                3 | 4 => {
                    let it = item();
                    model.insert(at, it.0);
                    deque.insert(at, it);
                }
                5 => {
                    let it = item();
                    model.push_front(it.0);
                    deque.insert(0, it);
                }
                6 if len > 0 => assert_eq!(deque.remove(0).0, model.pop_front().unwrap()),
                7 if len > 0 => {
                    let at = at.min(len - 1);
                    assert_eq!(deque.remove(at).0, model.remove(at).unwrap());
                }
                8 => {
                    let end = at + ((x >> 20) as usize % (len - at + 1));
                    let taken: Vec<usize> = deque.drain(at..end).into_iter().map(|e| e.0).collect();
                    let expected: Vec<usize> = model.drain(at..end).collect();
                    assert_eq!(taken, expected);
                }
                9 => {
                    let items: Vec<_> = (0..(x >> 30) as usize % 5).map(|_| item()).collect();
                    for (k, it) in items.iter().enumerate() {
                        model.insert(at + k, it.0);
                    }
                    deque.insert_all(at, items);
                }
                _ => {
                    let marked: Vec<bool> = (0..len).map(|i| (i as u64 ^ x).is_multiple_of(3)).collect();
                    let removed: Vec<usize> = deque.remove_marked(&marked).into_iter().map(|e| e.0).collect();
                    let expected: Vec<usize> =
                        model.iter().zip(&marked).filter(|(_, m)| **m).map(|(v, _)| *v).collect();
                    let mut i = 0;
                    model.retain(|_| {
                        i += 1;
                        !marked[i - 1]
                    });
                    assert_eq!(removed, expected);
                }
            }
            assert!(deque.iter().map(|e| e.0).eq(model.iter().copied()), "step {step}");
            assert_eq!(Rc::strong_count(&tracker), 1 + model.len(), "step {step}");
        }
        let copy = deque.clone();
        assert!(copy.iter().map(|e| e.0).eq(model.iter().copied()));
        drop((deque, copy));
        assert_eq!(Rc::strong_count(&tracker), 1);
    }

    /// Changes at either end stay cheap: the buffer never grows past a few
    /// times the elements, and elements move a bounded number of times.
    #[test]
    fn queues_and_stacks_stay_small() {
        let mut queue = Deque::new();
        for i in 0..100 {
            queue.push(i);
        }
        for i in 100..100_000 {
            queue.push(i);
            assert_eq!(queue.remove(0), i - 100);
        }
        assert!(queue.cap <= 512, "{}", queue.cap);
        let mut stack = Deque::from_vec((0..100).collect());
        for i in 0..100_000 {
            stack.insert(0, i);
            assert_eq!(stack.remove(0), i);
            stack.insert(1, i);
            assert_eq!(stack.remove(1), i);
        }
        assert!(stack.cap <= 512, "{}", stack.cap);
        let appended: Deque<usize> = {
            let mut d = Deque::new();
            for i in 0..1000 {
                d.push(i);
            }
            d
        };
        assert_eq!(appended.head, 0, "appending keeps no room at the front");
    }
}
