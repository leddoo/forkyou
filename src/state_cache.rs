use sti::vec::Vec;
use sti::sync::spin_lock::SpinLock;
use core::mem::ManuallyDrop;


pub struct StateCache<T: Send> {
    buffer: SpinLock<Vec<T>>,
}

impl<T: Send> StateCache<T> {
    #[inline]
    pub fn new() -> Self {
        Self::with_cap(crate::ncpu())
    }

    #[inline]
    pub fn with_cap(cap: usize) -> Self {
        Self { buffer: SpinLock::new(Vec::with_cap(cap)) }
    }


    #[inline]
    pub fn push(&self, value: T) {
        self.buffer.lock().push(value);
    }

    #[inline]
    pub fn pop(&self) -> Option<T> {
        self.buffer.lock().pop()
    }

    #[inline]
    pub fn get(&self, f: impl FnOnce() -> T) -> Cached<T> {
        let inner = self.pop().unwrap_or_else(f);
        Cached { cache: self, inner: ManuallyDrop::new(inner) }
    }
}


pub struct Cached<'a, T: Send> {
    cache: &'a StateCache<T>,
    inner: ManuallyDrop<T>,
}

impl<'a, T: Send> core::ops::Deref for Cached<'a, T> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &Self::Target { &self.inner }
}

impl<'a, T: Send> core::ops::DerefMut for Cached<'a, T> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target { &mut self.inner }
}

impl<'a, T: Send> core::ops::Drop for Cached<'a, T> {
    #[inline]
    fn drop(&mut self) {
        let inner = unsafe { ManuallyDrop::take(&mut self.inner) };
        self.cache.push(inner);
    }
}


