use crate::{Runtime, Spliterator, join_on_worker};


#[inline]
pub fn for_each<T, I, F>(iter: I, f: F)
where
    T: Send,
    I: Spliterator<Item = T> + Send,
    F: Fn(T) + Send + Sync
{
    let f = &f;
    Runtime::on_worker(move || for_each_core(iter, f))
}

#[inline]
fn for_each_core<T, I, F>(mut iter: I, f: &F)
where
    T: Send,
    I: Spliterator<Item = T> + Send,
    F: Fn(T) + Send + Sync
{
    if iter.len() >= 2 {
        let mid = iter.len() / 2;
        let (lhs, rhs) = iter.split(mid);
        join_on_worker(
            move || for_each_core(lhs, f),
            move || for_each_core(rhs, f));
    }
    else if iter.len() == 1 {
        f(iter.next());
    }
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
