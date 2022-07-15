use std::{
    alloc::{alloc, dealloc, handle_alloc_error, Layout},
    cell::{Cell, UnsafeCell},
    marker::PhantomData,
    mem::{align_of, size_of, MaybeUninit},
    slice,
    sync::atomic::{AtomicBool, Ordering},
};

use crate::cache::CachePadded;

mod atomic;
use self::atomic::*;

#[inline(always)]
const fn align_up(value: usize, align: usize) -> usize {
    debug_assert!(align.is_power_of_two());
    (value + align - 1) & !(align - 1)
}

type Slot<T> = UnsafeCell<MaybeUninit<T>>;

// struct Slots<T> {
//     slots: [Slot<T>],
//     statuses: [AtomicBool],
// }
struct Slots<T> {
    ptr: *const u8,
    capacity: usize,
    _t: PhantomData<(T, AtomicBool)>,
}

impl<T> Slots<T> {
    fn alloc(capacity: usize) -> Self {
        let (layout, status_offset) = Self::layout(capacity);

        // Allocate memory for the type and handle errors.
        let ptr = unsafe { alloc(layout) };
        if ptr.is_null() {
            handle_alloc_error(layout);
        }

        // Initialize the `statuses` array which mark occupied slots.
        // Note that the slots themselves are `MaybeUninit`, so not
        // initializing them still has them in a valid-to-access state.
        unsafe {
            let statuses = ptr.add(status_offset).cast::<AtomicBool>();
            for i in 0..capacity {
                statuses.add(i).write(AtomicBool::new(false));
            }
        }

        Self {
            ptr: ptr as *const u8,
            capacity,
            _t: PhantomData,
        }
    }

    fn layout(capacity: usize) -> (Layout, usize) {
        let result = Layout::array::<Slot<T>>(capacity)
            .and_then(|layout| layout.extend(Layout::array::<AtomicBool>(capacity)?))
            .unwrap();

        // Sanity check that the offset we will be calculating for
        // accesses to the `statuses` array matches the real one
        // directly yielded from the type's layout.
        debug_assert_eq!(result.1, Self::status_offset(capacity));

        result
    }

    #[inline]
    const fn status_offset(capacity: usize) -> usize {
        align_up(size_of::<T>() * capacity, align_of::<AtomicBool>())
    }

    #[inline]
    unsafe fn slice<U>(&self, offset: usize) -> &[U] {
        slice::from_raw_parts(self.ptr.add(offset).cast(), self.capacity)
    }

    #[inline]
    fn values(&self) -> &[Slot<T>] {
        // SAFETY: No padding at front, so we start at 0.
        unsafe { self.slice(0) }
    }

    #[inline]
    fn statuses(&self) -> &[AtomicBool] {
        // SAFETY: `layout` asserts `status_offset`'s correctness.
        unsafe { self.slice(Self::status_offset(self.capacity)) }
    }
}

impl<T> Drop for Slots<T> {
    fn drop(&mut self) {
        let (layout, status_offset) = Self::layout(self.capacity);

        unsafe {
            // SAFETY: The slices represent valid data and do not alias
            // each other's memory. Thus, we can safely construct them.
            let values =
                slice::from_raw_parts_mut(self.ptr.as_mut().cast::<Slot<T>>(), self.capacity);
            let statuses = slice::from_raw_parts_mut(
                self.ptr.as_mut().add(status_offset).cast::<AtomicBool>(),
                self.capacity,
            );

            // Drop every currently filled slot.
            for (slot, status) in values.iter_mut().zip(statuses.iter_mut()) {
                if *status.get_mut() {
                    *status.get_mut() = false;
                    slot.get_mut().assume_init_drop();
                }
            }

            // Finally deallocate the actual memory for this type.
            dealloc(self.ptr.as_mut(), layout);
        }
    }
}

struct Producer {
    // The index into the Slots list at which the
    // next element to enqueue should be stored.
    tail: Counter,
    // Semaphore-like guard which atomically stores the
    // current number of elements inside the queue.
    // Permits are claimed on push and released on pop,
    // this protects the queue from overflowing.
    guard: AccessGuard,
}

struct Consumer<T> {
    // The index into the Slots list where the next
    // element should be read from, if available.
    head: Cell<usize>,
    // A fixed-capacity list of `T` value slots to
    // store enqueued data in.
    slots: Slots<T>,
}

pub struct Queue<T> {
    producer: CachePadded<Producer>,
    consumer: CachePadded<Consumer<T>>,
}

impl<T> Queue<T> {
    pub fn new(mut capacity: usize) -> Self {
        capacity = capacity.next_power_of_two();
        assert!(
            capacity < isize::MAX as _,
            "capacity must not exceed isize::MAX"
        );

        Self {
            producer: CachePadded(Producer {
                tail: Counter::new(),
                guard: AccessGuard::new(),
            }),
            consumer: CachePadded(Consumer {
                head: Cell::new(0),
                slots: Slots::alloc(capacity),
            }),
        }
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.consumer.slots.capacity
    }

    #[inline]
    fn slot_index(&self, value: usize) -> usize {
        // Given that capacity is a power of two by design,
        // this will strip unnecessary high bits and always
        // yield a valid slot index for us to access.
        value & (self.capacity() - 1)
    }

    pub fn can_push(&self) -> bool {
        self.producer.guard.may_claim(self.capacity())
    }

    pub fn try_push(&self, value: T) -> Result<(), T> {
        // Try to claim an access permit to the queue.
        // If we fail, we cannot safely modify it.
        if !self.producer.guard.try_claim(self.capacity()) {
            return Err(value);
        }

        // Compute the slot index where value should be stored.
        let slot = self.slot_index(self.producer.tail.fetch_inc());

        unsafe {
            let slots = &self.consumer.slots;

            // SAFETY: See `slot_index` for clarification why accesses
            // to this value will always be in bounds.
            slots
                .values()
                .get_unchecked(slot)
                .get()
                .write(MaybeUninit::new(value));
            slots
                .statuses()
                .get_unchecked(slot)
                .store(true, Ordering::SeqCst);
        }

        Ok(())
    }

    pub unsafe fn can_pop(&self) -> bool {
        // Compute the slot index where the next value should be read.
        let slot = self.slot_index(self.consumer.head.get());

        // SAFETY: See `slot_index` for clarification why accesses
        // to this value will always be in bounds.
        self.consumer
            .slots
            .statuses()
            .get_unchecked(slot)
            .load(Ordering::Relaxed)
    }

    pub unsafe fn try_pop(&self) -> Option<T> {
        // Compute the slot index where the next value should be read.
        // SAFETY: See `slot_index` for clarification why accesses
        // to this value will always be in bounds.
        let head = self.consumer.head.get();
        let slot = self.slot_index(head);

        // Check if the this slot is populated.
        let status = self.consumer.slots.statuses().get_unchecked(slot);
        if !status.load(Ordering::Acquire) {
            return None;
        }

        // At this point, we know that the slot is populated and have
        // exclusive access to the data structure. Take the value and
        // reset the slot back into an empty state.
        let value = self.consumer.slots.values().get_unchecked(slot);
        let value = value.get().read().assume_init();
        status.store(false, Ordering::Release);

        self.producer.guard.release();

        // Advance to the next slot to be read.
        // We don't need the guard anymore as only one reader is allowed.
        self.consumer.head.set(head.wrapping_add(1));

        Some(value)
    }
}
