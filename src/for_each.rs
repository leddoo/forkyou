use crate::Spliterator;


#[inline]
pub fn for_each<T, I, F>(iter: I, f: F)
where
    T: Send,
    I: Spliterator<Item = T> + Send,
    F: Fn(T) + Send + Sync
{
    let dst = unsafe {
        core::slice::from_raw_parts_mut(
            core::ptr::NonNull::dangling().as_ptr(),
            iter.len())
    };
    crate::map_core(iter, dst, f)
}


#[cfg(test)]
mod tests {
    use crate::{IntoSpliteratorRef, IntoSpliteratorRefMut};
    use core::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn for_each() {
        let _t = crate::Terminator::new();

        let mut values = Vec::from_iter(0..100);

        let result = AtomicU32::new(0);
        crate::for_each(values.spliter(), |v| {
            result.fetch_add(*v, Ordering::Relaxed);
        });
        assert_eq!(result.load(Ordering::Relaxed), 99*100/2);

        super::for_each(values.spliter_mut(), |v| *v += 1);

        let result = AtomicU32::new(0);
        crate::for_each(values.spliter(), |v| {
            result.fetch_add(*v, Ordering::Relaxed);
        });
        assert_eq!(result.load(Ordering::Relaxed), 100*101/2);
    }
}
