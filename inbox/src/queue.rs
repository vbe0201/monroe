use std::{
    alloc::{alloc, dealloc, handle_alloc_error, Layout},
    cell::{Cell, UnsafeCell},
    marker::PhantomData,
    mem::{align_of, size_of, MaybeUninit},
    slice,
    sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering},
};

use crate::{cache::CachePadded, spin::Backoff};

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

        let ptr = unsafe { alloc(layout) };
        if ptr.is_null() {
            handle_alloc_error(layout);
        }

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

        debug_assert_eq!(result.1, Self::status_offset(capacity));

        result
    }

    #[inline]
    fn status_offset(capacity: usize) -> usize {
        align_up(size_of::<T>() * capacity, align_of::<AtomicBool>())
    }

    #[inline]
    unsafe fn slice<U>(&self, offset: usize) -> &[U] {
        slice::from_raw_parts(self.ptr.add(offset).cast(), self.capacity)
    }

    #[inline]
    fn values(&self) -> &[Slot<T>] {
        unsafe { self.slice(0) }
    }

    #[inline]
    fn statuses(&self) -> &[AtomicBool] {
        unsafe { self.slice(Self::status_offset(self.capacity)) }
    }
}

impl<T> Drop for Slots<T> {
    fn drop(&mut self) {
        let (layout, status_offset) = Self::layout(self.capacity);

        unsafe {
            let values =
                slice::from_raw_parts_mut(self.ptr as *mut u8 as *mut Slot<T>, self.capacity);
            let statuses = slice::from_raw_parts_mut(
                self.ptr.add(status_offset) as *mut u8 as *mut AtomicBool,
                self.capacity,
            );

            for (slot, status) in values.iter_mut().zip(statuses.iter_mut()) {
                if *status.get_mut() {
                    slot.get_mut().assume_init_drop();
                }
            }

            dealloc(self.ptr as *mut u8, layout);
        }
    }
}

struct Producer {
    tail: AtomicUsize,
    sema: AtomicIsize,
}

struct Consumer<T> {
    head: Cell<usize>,
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
            capacity < isize::MAX as usize,
            "capacity must not exceed isize::MAX"
        );

        Self {
            producer: CachePadded(Producer {
                tail: AtomicUsize::new(0),
                sema: AtomicIsize::new(capacity as isize),
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
    pub fn can_push(&self) -> bool {
        self.producer.sema.load(Ordering::Relaxed) > 0
    }

    pub fn try_push(&self, value: T) -> Result<(), T> {
        let mut backoff = Backoff::default();

        loop {
            if self.producer.sema.fetch_sub(1, Ordering::Acquire) > 0 {
                break;
            }

            backoff.spin();

            if self.producer.sema.fetch_add(1, Ordering::Relaxed) < 0 {
                return Err(value);
            }

            backoff.spin();
        }

        let tail = self.producer.tail.fetch_add(1, Ordering::Relaxed);
        let slot = tail & (self.consumer.slots.capacity - 1);

        unsafe {
            let slots = &self.consumer.slots;

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
        let head = self.consumer.head.get();
        let slot = head & (self.consumer.slots.capacity - 1);

        self.consumer
            .slots
            .statuses()
            .get_unchecked(slot)
            .load(Ordering::Relaxed)
    }

    pub unsafe fn try_pop(&self) -> Option<T> {
        let head = self.consumer.head.get();
        let slot = head & (self.consumer.slots.capacity - 1);

        let status = self.consumer.slots.statuses().get_unchecked(slot);
        if !status.load(Ordering::Acquire) {
            return None;
        }

        let value = self.consumer.slots.values().get_unchecked(slot);
        let value = value.get().read().assume_init();
        status.store(false, Ordering::Release);

        self.producer.sema.fetch_add(1, Ordering::Release);
        self.consumer.head.set(head.wrapping_add(1));
        Some(value)
    }
}
