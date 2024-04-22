#![forbid(unsafe_op_in_unsafe_fn)]

use sti::alloc::{Alloc, GlobalAlloc};
use sti::boks::Box;
use sti::vec::Vec;
use core::ptr::NonNull;
use core::mem::ManuallyDrop;
use core::marker::PhantomData;

pub mod deque;
pub mod state_cache;
pub mod spliterator;

pub use state_cache::StateCache;
pub use spliterator::{Spliterator, IntoSpliterator};

mod runtime;
mod worker;
mod scope;

pub use runtime::Terminator;
pub use scope::scope;


// temp sti stuff:
type CovariantLifetime<'a>     = PhantomData<fn()       -> &'a ()>;
type ContravariantLifetime<'a> = PhantomData<fn(&'a ())>;
type InvariantLifetime<'a>     = PhantomData<fn(&'a ()) -> &'a ()>;
fn box_into_inner<T, A: Alloc>(this: Box<T, A>) -> T {
    let (ptr, alloc) = this.into_raw_parts();
    let ptr = ptr.cast::<ManuallyDrop<T>>();
    let mut this = unsafe { Box::from_raw_parts(ptr, alloc) };
    let result = unsafe { ManuallyDrop::take(&mut *this) };
    return result;
}


#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SendPtr<T>(pub *const T);

unsafe impl<T> Sync for SendPtr<T> {}
unsafe impl<T> Send for SendPtr<T> {}


#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SendPtrMut<T>(pub *mut T);

unsafe impl<T> Sync for SendPtrMut<T> {}
unsafe impl<T> Send for SendPtrMut<T> {}



use runtime::Runtime;



#[inline]
pub fn ncpu() -> usize {
    thread_local! {
        static NCPU: usize =
            if cfg!(not(miri)) {
                std::thread::available_parallelism()
                    .map_or(1, |n| n.get())
            }
            else { 4 };
    }

    NCPU.with(|ncpu| *ncpu)
}



pub struct Task {
    ptr: NonNull<u8>,
    call: fn(NonNull<u8>),
}

impl Task {
    #[inline]
    pub unsafe fn new(ptr: NonNull<u8>, call: fn(NonNull<u8>)) -> Self {
        Self { ptr, call }
    }

    #[inline]
    pub fn call(self) {
        (self.call)(self.ptr);
    }
}

unsafe impl Send for Task {}


/*
pub struct StackTask<R, F> {
    result: MaybeUninit<R>,
    f: F,
}

impl<R, F> StackTask<R, F> {
    pub unsafe fn new(f: F) -> Self  where F: FnOnce() -> R + Send {
        Self { result: MaybeUninit::uninit(), f }
    }
}
*/



pub fn spawn_untracked<F: FnOnce() + Send + 'static>(f: F) {
    let task = Task {
        ptr: Box::new(f).into_raw_parts().0.cast(),
        call: |ptr| {
            let f = unsafe { Box::from_raw_parts(ptr.cast(), GlobalAlloc) };
            // @temp
            let f: F = box_into_inner(f); //f.into_inner();
            // @panic.
            f();
        },
    };
    Runtime::submit_task(task);
}


pub fn for_each<T: Sync, F: Fn(&T) + Sync>(values: &[T], f: F) {
    let f = &f;
    scope(move |scope| {
        for value in values {
            scope.spawn(move || f(value));
        }
    });
}

pub fn for_each_mut<T: Send, F: Fn(&mut T) + Sync>(values: &mut [T], f: F) {
    let f = &f;
    scope(move |scope| {
        for value in values {
            scope.spawn(move || f(value));
        }
    });
}


pub fn map<T: Sync, U: Send, F: Fn(&T) -> U + Sync>(values: &[T], f: F) -> Vec<U> {
    map_in(GlobalAlloc, values, f)
}

pub fn map_in<A: Alloc, T: Sync, U: Send, F: Fn(&T) -> U + Sync>(alloc: A, values: &[T], f: F) -> Vec<U, A> {
    let mut result = Vec::with_cap_in(alloc, values.len());
    map_into(&mut result, values, f);
    return result;
}

pub fn map_into<A: Alloc, T: Sync, U: Send, F: Fn(&T) -> U + Sync>(out: &mut Vec<U, A>, values: &[T], f: F) {
    out.reserve_extra(values.len());

    let f = &f;
    let dst = unsafe { SendPtrMut(out.as_mut_ptr().add(out.len())) };
    scope(move |scope| {
        let dst = dst;
        for (i, value) in values.iter().enumerate() {
            let dst = unsafe { SendPtrMut(dst.0.add(i)) };
            scope.spawn(move || unsafe {
                let dst = dst;
                dst.0.write(f(value));
            });
        }
    });

    unsafe { out.set_len(out.len() + values.len()) }
}




/// tasks can borrow locals from parent.
/// ```rust
/// let _t = forkyou::Terminator::new();
/// let foo = 42;
/// forkyou::scope(|scope| {
///     scope.spawn(|| foo);
/// });
/// ```
///
/// tasks cannot borrow locals from scope.
/// this is necessary because tasks may not complete before the closure returns.
/// ```compile_fail
/// forkyou::scope(|scope| {
///     let foo = 42;
///     scope.spawn(|| foo);
/// });
/// ```
///
/// scope can assign locals from parent.
/// ```rust
/// let _t = forkyou::Terminator::new();
/// let mut foo = None;
/// forkyou::scope(|scope| {
///     foo = Some(42);
/// });
/// ```
///
/// single task can assign locals from parent.
/// ```rust
/// let _t = forkyou::Terminator::new();
/// let mut foo = None;
/// forkyou::scope(|scope| {
///     scope.spawn(|| foo = Some(42));
/// });
/// ```
///
/// multiple tasks cannot mutably borrow locals from parent.
/// ```compile_fail
/// let mut foo = None;
/// forkyou::scope(|scope| {
///     scope.spawn(|| foo = Some(42));
///     scope.spawn(|| foo = Some(42));
/// });
/// ```
///
/// allocations cannot escape scope.
/// this is necessary for the use Arena::save/restore to be safe.
/// ```compile_fail
/// let mut foo = &mut 42;
/// forkyou::scope(|scope| {
///     foo = scope.alloc().alloc_new(42);
/// });
/// ```
///
/// tasks cannot access scope.
/// this is necessary for the use Arena::save/restore to be safe.
/// ```compile_fail
/// forkyou::scope(|scope| {
///     scope.spawn(|| {
///         scope.alloc().alloc_new(42);
///     });
/// });
/// ```
///
/// inner scope cannot access outer scope.
/// this is necessary for the use Arena::save/restore to be safe.
/// ```compile_fail
/// forkyou::scope(|scope| {
///     forkyou::scope(|_| {
///         scope.alloc().alloc_new(42);
///     });
/// });
/// ```
#[allow(dead_code)]
struct DocTests;

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use sti::vec::Vec;
    use core::sync::atomic::{Ordering, AtomicU32};


    #[test]
    fn untracked() {
        let _t = super::Terminator::new();

        let result = Arc::new(Mutex::new(None));

        super::spawn_untracked(sti::enclose!(result; move || {
            let mut lock = result.lock().unwrap();
            assert!(lock.is_none());
            *lock = Some(42);
        }));

        let result = loop {
            let lock = result.lock().unwrap();
            if let Some(result) = *lock {
                break result;
            }
        };
        assert_eq!(result, 42);
    }


    #[test]
    fn for_each() {
        let _t = crate::Terminator::new();

        let mut values = Vec::from_iter(0..100);

        let result = AtomicU32::new(0);
        super::for_each(&values, |v| {
            result.fetch_add(*v, Ordering::Relaxed);
        });
        assert_eq!(result.load(Ordering::Relaxed), 99*100/2);

        super::for_each_mut(&mut values, |v| *v += 1);

        let result = AtomicU32::new(0);
        super::for_each(&values, |v| {
            result.fetch_add(*v, Ordering::Relaxed);
        });
        assert_eq!(result.load(Ordering::Relaxed), 100*101/2);
    }

    #[test]
    fn map() {
        let _t = crate::Terminator::new();

        let values = Vec::from_iter(0..100);

        let squared = super::map(&values, |v| *v * *v);
        assert_eq!(squared.cap(), 100);
        assert_eq!(squared.len(), 100);

        for (i, v) in values.iter().copied().enumerate() {
            assert_eq!(squared[i], v*v)
        }
    }
}

