use crate::{Runtime, Worker, Spliterator, join_on_worker};


#[inline]
pub fn for_each<T, I, F>(iter: I, f: F)
where
    T: Send,
    I: Spliterator<Item = T> + Send,
    F: Fn(T) + Send + Sync
{
    let f = &f;
    Runtime::on_worker(move |worker, _| for_each_core(worker, iter, f))
}

fn for_each_core<T, I, F>(worker: &Worker, mut iter: I, f: &F)
where
    T: Send,
    I: Spliterator<Item = T> + Send,
    F: Fn(T) + Send + Sync
{
    if iter.len() >= 2 {
        let mid = iter.len() / 2;
        let (lhs, rhs) = iter.split(mid);
        join_on_worker(worker,
            move |worker, _| for_each_core(worker, lhs, f),
            move |worker, _| for_each_core(worker, rhs, f));
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
