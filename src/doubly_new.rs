use seize::{reclaim, AtomicPtr, Collector, Guard, Linked};
use std::{mem::ManuallyDrop, ptr};
use std::{
    mem::MaybeUninit,
    sync::atomic::{AtomicUsize, Ordering},
};

pub struct LinkedList<T> {
    head: AtomicPtr<Node<T>>,
    tail: AtomicPtr<Node<T>>,
    len: AtomicUsize,
    collector: Collector,
}

#[derive(Debug)]
pub struct Node<T> {
    inner: MaybeUninit<ManuallyDrop<T>>,
    next: AtomicPtr<Node<T>>,
    prev: AtomicPtr<Node<T>>,
}

impl<T> Node<T> {
    fn new(t: T) -> Self {
        Self {
            inner: MaybeUninit::new(ManuallyDrop::new(t)),
            next: AtomicPtr::new(ptr::null_mut()),
            prev: AtomicPtr::new(ptr::null_mut()),
        }
    }
}

impl<T> LinkedList<T> {
    pub fn new() -> Self {
        let list = Self {
            head: AtomicPtr::new(ptr::null_mut()),
            tail: AtomicPtr::new(ptr::null_mut()),
            collector: Collector::new(),
            len: AtomicUsize::new(0),
        };

        let sentinel = list.collector.link_boxed(Node {
            inner: MaybeUninit::uninit(),
            next: AtomicPtr::new(ptr::null_mut()),
            prev: AtomicPtr::new(ptr::null_mut()),
        });

        list.head.store(sentinel, Ordering::Relaxed);
        list.tail.store(sentinel, Ordering::Relaxed);

        list
    }

    pub fn len(&self) -> usize {
        self.len.load(Ordering::Acquire)
    }

    #[inline]
    fn push_back_internal(
        &self,
        onto: *mut Linked<Node<T>>,
        new: *mut Linked<Node<T>>,
        guard: &Guard,
    ) -> bool {
        let next = guard.protect(&unsafe { &*onto }.next, Ordering::Acquire);

        if !next.is_null() {
            let _ = self
                .tail
                .compare_exchange(onto, next, Ordering::Acquire, Ordering::Relaxed);

            false
        } else {
            let result = unsafe { &*onto }
                .next
                .compare_exchange(ptr::null_mut(), new, Ordering::Release, Ordering::Relaxed)
                .is_ok();

            if result {
                unsafe { &*new }.prev.store(onto, Ordering::Release);
                let _ = self
                    .tail
                    .compare_exchange(onto, new, Ordering::Release, Ordering::Relaxed);
            }
            result
        }
    }

    #[inline]
    fn push_front_internal(
        &self,
        onto: *mut Linked<Node<T>>,
        new: *mut Linked<Node<T>>,
        guard: &Guard,
    ) -> bool {
        let prev = guard.protect(&unsafe { &*onto }.prev, Ordering::Acquire);

        if !prev.is_null() {
            let _ = self
                .head
                .compare_exchange(onto, prev, Ordering::Acquire, Ordering::Relaxed);

            false
        } else {
            let result = unsafe { &*onto }
                .prev
                .compare_exchange(ptr::null_mut(), new, Ordering::Release, Ordering::Relaxed)
                .is_ok();

            if result {
                unsafe { &*new }.next.store(onto, Ordering::Release);
                let _ = self
                    .head
                    .compare_exchange(onto, new, Ordering::Release, Ordering::Relaxed);
            }
            result
        }
    }

    #[inline]
    fn pop_front_internal(&self, guard: &Guard) -> Result<Option<T>, ()> {
        let head = guard.protect(&self.head, Ordering::Acquire);
        let next = guard.protect(&unsafe { &*head }.next, Ordering::Acquire);

        if !next.is_null() {
            match self
                .head
                .compare_exchange(head, next, Ordering::Release, Ordering::Relaxed)
            {
                Ok(_) => {
                    let tail = guard.protect(&self.tail, Ordering::Release);
                    if head == tail {
                        let _ = self.tail.compare_exchange(
                            tail,
                            next,
                            Ordering::Release,
                            Ordering::Relaxed,
                        );
                    }
                    Ok(unsafe { self.consume_and_retire(next) })
                }
                Err(_) => Err(()),
            }
        } else {
            Ok(None)
        }
    }

    pub fn pop_front(&self) -> Option<T> {
        let guard = self.collector.enter();
        loop {
            if let Ok(head) = self.pop_front_internal(&guard) {
                return head;
            }
        }
    }

    #[inline]
    pub fn push_back(&self, t: T) {
        let guard = self.collector.enter();
        let new = self.collector.link_boxed(Node::new(t));
        loop {
            let tail = guard.protect(&self.tail, Ordering::Acquire);
            if self.push_back_internal(tail, new, &guard) {
                self.len.fetch_add(1, Ordering::Release);
                break;
            }
        }
    }

    #[inline]
    pub fn push_front(&self, t: T) {
        let guard = self.collector.enter();
        let new = self.collector.link_boxed(Node::new(t));
        loop {
            let head = guard.protect(&self.head, Ordering::Acquire);
            if self.push_front_internal(head, new, &guard) {
                self.len.fetch_add(1, Ordering::Release);
                break;
            }
        }
    }

    #[inline]
    unsafe fn consume_and_retire(&self, ptr: *mut Linked<Node<T>>) -> Option<T> {
        let data = ptr::read(&(*ptr).inner);
        self.collector.retire(ptr, reclaim::boxed::<Node<T>>);
        self.len.fetch_sub(1, Ordering::Release);
        return Some(ManuallyDrop::into_inner(data.assume_init()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_back_new() {
        let list = LinkedList::new();
        list.push_back(1);
        list.push_back(2);
        list.push_back(3);
        let head = list.head.load(Ordering::Acquire);
        let head_next = unsafe {
            (&*list.head.load(Ordering::Acquire))
                .next
                .load(Ordering::Acquire)
        };

        let head_next_2 = unsafe { (*head_next).next.load(Ordering::Acquire) };
        let head_next_3 = unsafe { (*head_next_2).next.load(Ordering::Acquire) };

        assert_eq!(unsafe { (*head_next).prev.load(Ordering::Acquire) }, head);
        assert_eq!(
            unsafe { (*head_next_2).prev.load(Ordering::Acquire) },
            head_next
        );
        assert_eq!(
            unsafe { (*head_next_3).prev.load(Ordering::Acquire) },
            head_next_2
        );

        assert_eq!(
            unsafe { (*head_next).next.load(Ordering::Acquire) },
            head_next_2
        );
        assert_eq!(
            unsafe { (*head_next_2).next.load(Ordering::Acquire) },
            head_next_3
        );

        assert_eq!(list.len(), 3);
        assert_eq!(list.pop_front().unwrap(), 1);
        assert_eq!(list.pop_front().unwrap(), 2);
        assert_eq!(list.pop_front().unwrap(), 3);
        assert!(list.pop_front().is_none());
        assert_eq!(list.len(), 0);
    }

    #[test]
    fn push_front() {
        let list = LinkedList::new();
        list.push_front(1);
        list.push_front(2);
        list.push_front(3);

        let head = list.head.load(Ordering::Acquire);
        let head_next = unsafe {
            (&*list.head.load(Ordering::Acquire))
                .next
                .load(Ordering::Acquire)
        };

        let head_next_2 = unsafe { (*head_next).next.load(Ordering::Acquire) };
        let head_next_3 = unsafe { (*head_next_2).next.load(Ordering::Acquire) };

        assert_eq!(unsafe { (*head_next).prev.load(Ordering::Acquire) }, head);
        assert_eq!(
            unsafe { (*head_next_2).prev.load(Ordering::Acquire) },
            head_next
        );
        assert_eq!(
            unsafe { (*head_next_3).prev.load(Ordering::Acquire) },
            head_next_2
        );

        assert_eq!(
            unsafe { (*head_next).next.load(Ordering::Acquire) },
            head_next_2
        );
        assert_eq!(
            unsafe { (*head_next_2).next.load(Ordering::Acquire) },
            head_next_3
        );

        assert_eq!(list.len(), 3);
        assert_eq!(list.pop_front().unwrap(), 2);
        assert_eq!(list.pop_front().unwrap(), 1);
        assert_eq!(list.pop_front().unwrap(), 0);
        assert_eq!(list.len(), 0);
    }
}
