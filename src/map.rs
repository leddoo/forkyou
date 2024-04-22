use sti::alloc::{Alloc, GlobalAlloc};
use sti::vec::Vec;
use core::mem::{MaybeUninit, ManuallyDrop};
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU8, Ordering};

use crate::{Runtime, Task, Spliterator};


pub fn map_core<T, U, I, F>(iter: I, result: &mut [MaybeUninit<U>], f: F)
where
    T: Send,
    U: Send,
    I: Spliterator<Item = T> + Send,
    F: Fn(T) -> U + Send + Sync
{
    Runtime::on_worker(move || {
        map_core_core(iter, result, &f)
    })
}

fn map_core_core<T, U, I, F>(mut iter: I, result: &mut [MaybeUninit<U>], f: &F)
where
    T: Send,
    U: Send,
    I: Spliterator<Item = T> + Send,
    F: Fn(T) -> U + Send + Sync
{
    assert_eq!(iter.len(), result.len());

    if iter.len() >= 2 {
        let mid = iter.len() / 2;
        let (lhs, rhs) = iter.split(mid);
        let (lhs_result, rhs_result) = result.split_at_mut(mid);


        const STATE_WAIT: u8 = 0;
        const STATE_DONE: u8 = 1;
        const STATE_PANIC: u8 = 2;

        struct FBox<'a, U, I, F> {
            iter: ManuallyDrop<I>,
            result: &'a mut [MaybeUninit<U>],
            f: &'a F,
            state: &'a AtomicU8,
        }

        let state = AtomicU8::new(STATE_WAIT);

        let mut fbox = FBox {
            iter: ManuallyDrop::new(lhs),
            result: lhs_result,
            f,
            state: &state,
        };

        let ptr = NonNull::from(&mut fbox).cast();

        let call = |ptr: NonNull<u8>| {
            let mut ptr = ptr.cast::<FBox<U, I, F>>();
            let fbox = unsafe { ptr.as_mut() };
            map_core_core(
                unsafe { ManuallyDrop::take(&mut fbox.iter) },
                &mut *fbox.result,
                fbox.f);
            fbox.state.store(STATE_DONE, Ordering::Release);
        };

        Runtime::submit_task_on_worker(unsafe {
            Task::new(ptr, call)
        });

        map_core_core(rhs, rhs_result, f);

        Runtime::work_while(|| state.load(Ordering::Acquire) == STATE_WAIT);

        let state = state.load(Ordering::Acquire);
        if state != STATE_DONE {
            todo!()
        }
    }
    else if iter.len() == 1 {
        result[0] = MaybeUninit::new(f(iter.next()));
    }
}


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

