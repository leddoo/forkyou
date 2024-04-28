use sti::alloc::{Alloc, GlobalAlloc};
use sti::vec::Vec;
use core::mem::MaybeUninit;

use crate::{Runtime, Worker, Spliterator, join_on_worker};
use crate::join::Splitter;


#[inline]
pub fn map<T, U, I, F>(iter: I, f: F) -> Vec<U>
where
    T: Send,
    U: Send,
    I: Spliterator<Item = T> + Send,
    F: Fn(T) -> U + Send + Sync
{
    map_in(GlobalAlloc, iter, f)
}

#[inline]
pub fn map_in<A: Alloc, T, U, I, F>(alloc: A, iter: I, f: F) -> Vec<U, A>
where
    T: Send,
    U: Send,
    I: Spliterator<Item = T> + Send,
    F: Fn(T) -> U + Send + Sync
{
    let mut result = Vec::with_cap_in(alloc, iter.len());
    map_into(&mut result, iter, f);
    return result;
}

#[inline]
pub fn map_into<A: Alloc, T, U, I, F>(result: &mut Vec<U, A>, iter: I, f: F)
where
    T: Send,
    U: Send,
    I: Spliterator<Item = T> + Send,
    F: Fn(T) -> U + Send + Sync
{
    let len = iter.len();
    result.reserve_extra(len);

    // @todo: move into sti.
    let dst = unsafe {
        core::slice::from_raw_parts_mut(
            result.as_mut_ptr().add(result.len()).cast(),
            len)
    };
    map_core(iter, dst, f);

    unsafe { result.set_len(result.len() + len) }
}

#[inline]
pub fn map_core<T, U, I, F>(iter: I, result: &mut [MaybeUninit<U>], f: F)
where
    T: Send,
    U: Send,
    I: Spliterator<Item = T> + Send,
    F: Fn(T) -> U + Send + Sync
{
    let f = &f;
    Runtime::on_worker(move |worker, _| {
        let splitter = Splitter::new(worker.num_workers());
        return map_core_core(worker, false, splitter, iter, result, f);
    })
}

fn map_core_core<T, U, I, F>(
    worker: &Worker,
    stolen: bool,
    mut splitter: Splitter,
    mut iter: I,
    result: &mut [MaybeUninit<U>],
    f: &F)
where
    T: Send,
    U: Send,
    I: Spliterator<Item = T> + Send,
    F: Fn(T) -> U + Send + Sync
{
    assert_eq!(iter.len(), result.len());

    if splitter.try_split(iter.len(), stolen.then(|| worker.num_workers())) {
        let mid = iter.len() / 2;
        let (lhs, rhs) = iter.split(mid);
        let (lhs_result, rhs_result) = result.split_at_mut(mid);
        join_on_worker(worker,
            move |worker, stolen| map_core_core(worker, stolen, splitter, lhs, lhs_result, f),
            move |worker, stolen| map_core_core(worker, stolen, splitter, rhs, rhs_result, f));
    }
    else {
        for i in 0..iter.len() {
            result[i] = MaybeUninit::new(f(iter.next()));
        }
    }
}


#[cfg(test)]
mod tests {
    use crate::{Spliterator, IntoSpliteratorRef};

    #[test]
    fn map() {
        let _t = crate::Terminator::new();

        let values = Vec::from_iter(0..100);

        let squared = crate::map(values.spliter().copied(), |v| v * v);
        assert_eq!(squared.cap(), 100);
        assert_eq!(squared.len(), 100);

        for (i, v) in values.iter().copied().enumerate() {
            assert_eq!(squared[i], v*v)
        }
    }
}

