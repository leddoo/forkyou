use sti::vec::Vec;
use sti::sync::spin_lock::SpinLock;


pub struct StateCache<T: Send> {
    buffer: SpinLock<Vec<T>>,
}

impl<T: Send> StateCache<T> {
    #[inline]
    pub fn new() -> Self {
        Self { buffer: SpinLock::new(Vec::new()) }
    }

    #[inline]
    pub fn with_cap(cap: usize) -> Self {
        Self { buffer: SpinLock::new(Vec::with_cap(cap)) }
    }

    #[inline]
    pub fn with_cap_ncpu() -> Self {
        Self::with_cap(crate::ncpu())
    }


    #[inline]
    pub fn push(&self, value: T) {
        self.buffer.lock().push(value);
    }

    #[inline]
    pub fn pop(&self) -> Option<T> {
        self.buffer.lock().pop()
    }
}


