use std::{
    cell::Cell,
    marker::PhantomPinned,
    pin::Pin,
    ptr::NonNull,
    task::{Poll, Waker},
};

use super::waker::AtomicWaker;

pub struct Waiter {
    prev: Cell<Option<NonNull<Self>>>,
    next: Cell<Option<NonNull<Self>>>,
    waiting: Cell<bool>,
    waker: AtomicWaker,
    _pin: PhantomPinned,
}

impl Waiter {
    pub const fn new() -> Self {
        Self {
            prev: Cell::new(None),
            next: Cell::new(None),
            waiting: Cell::new(false),
            waker: AtomicWaker::new(),
            _pin: PhantomPinned,
        }
    }

    #[inline]
    pub fn poll_ready(&self, waker: &Waker) -> Poll<()> {
        self.waker.poll_ready(waker)
    }

    #[inline]
    pub fn wake(&self) {
        self.waker.wake();
    }
}

pub struct WaitQueue {
    head: Option<NonNull<Waiter>>,
    tail: Option<NonNull<Waiter>>,
}

impl WaitQueue {
    pub const fn new() -> Self {
        Self {
            head: None,
            tail: None,
        }
    }

    pub unsafe fn push(&mut self, waiter: Pin<&Waiter>) {
        let waiter = NonNull::from(&*waiter);

        waiter.as_ref().waiting.set(true);
        waiter.as_ref().next.set(None);

        let tail = self.tail.replace(waiter);
        waiter.as_ref().prev.set(tail);

        match tail {
            Some(tail) => tail.as_ref().next.set(Some(waiter)),
            None => self.head = Some(waiter),
        }
    }

    pub unsafe fn pop(&mut self) -> Option<NonNull<Waiter>> {
        self.head.map(|head| {
            let waiter = Pin::new_unchecked(head.as_ref());
            assert!(self.try_remove(waiter));
            head
        })
    }

    pub unsafe fn try_remove(&mut self, waiter: Pin<&Waiter>) -> bool {
        let waiter = NonNull::from(&*waiter);

        if !waiter.as_ref().waiting.replace(false) {
            return false;
        }

        let prev = waiter.as_ref().prev.get();
        let next = waiter.as_ref().next.get();

        match prev {
            Some(prev) => {
                prev.as_ref().next.set(next);
                match next {
                    Some(next) => next.as_ref().prev.set(Some(prev)),
                    None => self.tail = Some(prev),
                }
            }
            None => {
                self.head = next;
                if self.head.is_none() {
                    self.tail = None;
                }
            }
        }

        true
    }

    pub unsafe fn drain(&mut self) -> Drain {
        let mut popped = 0;
        let mut nodes = self.head;

        while let Some(waiter) = self.head {
            self.head = waiter.as_ref().next.get();
            assert!(waiter.as_ref().waiting.replace(false));

            waiter.as_ref().next.set(nodes);
            nodes = Some(waiter);

            popped += 1;
        }

        self.head = None;
        self.tail = None;

        Drain { len: popped, nodes }
    }
}

pub struct Drain {
    len: usize,
    nodes: Option<NonNull<Waiter>>,
}

impl Drain {
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Iterator for Drain {
    type Item = NonNull<Waiter>;

    fn next(&mut self) -> Option<Self::Item> {
        self.nodes.map(|node| {
            self.nodes = unsafe { node.as_ref().next.get() };
            self.len -= 1;
            node
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.len, Some(self.len))
    }
}

impl ExactSizeIterator for Drain {}
