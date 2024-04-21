use sti::alloc::{Alloc, GlobalAlloc};
use sti::arena::Arena;
use sti::boks::Box;
use sti::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use core::ptr::NonNull;
use core::cell::UnsafeCell;
use core::mem::{ManuallyDrop, MaybeUninit};
use core::marker::PhantomData;

pub mod deque;
pub mod state_cache;


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




#[inline]
pub fn ncpu() -> usize {
    thread_local! {
        static NCPU: usize =
            std::thread::available_parallelism()
                .map_or(1, |n| n.get())
    }

    NCPU.with(|ncpu| *ncpu)
}



pub struct Task {
    pub ptr: NonNull<u8>,
    pub call: fn(NonNull<u8>),
}

pub unsafe fn spawn_raw(task: Task) {
    // @temp
    (task.call)(task.ptr)
}



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
    unsafe { spawn_raw(task) }
}



pub fn scope<'p, R, F: for<'s> FnOnce(&Scope<'s, 'p>) -> R + Send>(f: F) -> R {
    thread_local! {
        static SCOPE_ARENA: Arena = {
            let mut arena = Arena::new();
            arena.min_block_size.set(SCOPE_ARENA_INITIAL_SIZE);
            // make sure root scope doesn't save "arena has no blocks",
            // and then frees everything on restore.
            arena.alloc_new(1);
            arena.reset();
            arena
        };
    };

    SCOPE_ARENA.with(|alloc| {
        let save = alloc.save();

        let scope = Scope {
            alloc,
            state: ScopeState {
                panic: AtomicBool::new(false),
                running: AtomicUsize::new(0),
            },
            _s: Default::default(),
            _p: Default::default(),
        };

        let result = f(&scope);

        // @temp: wait for tasks to complete.
        assert_eq!(scope.state.running.load(Ordering::SeqCst), 0);

        // safety:
        // - allocations cannot escape `f`, so there are no external dangling refs.
        // - during the execution of inner scopes, outer scopes are inaccessible,
        //   so inner scopes cannot accidentally free outer allocations.
        // - tasks cannot access outer scopes, so they can't make allocations while
        //   inner scopes are running.
        unsafe { alloc.restore(save) };

        return result
    })
}


pub struct Scope<'s, 'p: 's> {
    alloc: &'s Arena,
    state: ScopeState,
    _s: InvariantLifetime<'s>,
    _p: CovariantLifetime<'p>,
}

// @todo: this is probably too small.
const SCOPE_ARENA_INITIAL_SIZE: usize = 64*1024;

struct ScopeState {
    panic: AtomicBool,
    running: AtomicUsize,
}

impl<'s, 'p: 's> Scope<'s, 'p> {
    pub fn alloc(&self) -> &'s Arena {
        self.alloc
    }

    pub fn spawn<R, F: FnOnce() -> R + Send + 'p>(&self, f: F) -> ScopeTask<'s, R> {
        struct FBox<'a, R, F> {
            f: F,
            scope:  &'a ScopeState,
            result: ScopeTaskResult<R>,
        }

        let ptr: *mut FBox<R, F> = self.alloc.alloc_new(FBox {
            f,
            scope: &self.state,
            result: ScopeTaskResult {
                state: AtomicU8::new(SCOPE_TASK_RESULT_WAIT),
                value: UnsafeCell::new(MaybeUninit::uninit()),
            },
        });

        let result = unsafe { &(*ptr).result };

        let task = Task {
            ptr: unsafe { NonNull::new_unchecked(ptr).cast() },
            call: |ptr| {
                let ptr = ptr.cast::<FBox<R, F>>().as_ptr();
                let scope = unsafe { (*ptr).scope };
                let result = unsafe { &(*ptr).result };
                debug_assert_eq!(result.state.load(Ordering::SeqCst), SCOPE_TASK_RESULT_WAIT);

                let f = unsafe { (&mut (*ptr).f as *mut F).read() };
                // @panic.
                let value = f();

                unsafe { result.value.get().write(MaybeUninit::new(value)) };
                result.state.store(SCOPE_TASK_RESULT_DONE, Ordering::SeqCst);

                let prev_running = scope.running.fetch_sub(1, Ordering::SeqCst);
                debug_assert!(prev_running > 0);
            },
        };

        let prev_running = self.state.running.fetch_add(1, Ordering::SeqCst);
        assert!(prev_running < usize::MAX);

        unsafe { spawn_raw(task) };

        // @temp: wait for task to complete in join.
        assert_eq!(result.state.load(Ordering::SeqCst), SCOPE_TASK_RESULT_DONE);
        let temp = unsafe { result.value.get().read().assume_init() };

        return ScopeTask { result, temp };
    }
}

const SCOPE_TASK_RESULT_WAIT: u8 = 0;
const SCOPE_TASK_RESULT_DONE: u8 = 1;
const SCOPE_TASK_RESULT_PANIC: u8 = 2;

struct ScopeTaskResult<R> {
    state: AtomicU8,
    value: UnsafeCell<MaybeUninit<R>>,
}

pub struct ScopeTask<'s, R> {
    result: &'s ScopeTaskResult<R>,
    temp: R,
}

impl<'s, R> ScopeTask<'s, R> {
    pub fn join(self) -> R {
        self.temp
    }
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
/// let mut foo = None;
/// forkyou::scope(|scope| {
///     foo = Some(42);
/// });
/// ```
///
/// single task can assign locals from parent.
/// ```rust
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
    use sti::sync::spin_lock::SpinLock;
    use sti::vec::Vec;
    use core::sync::atomic::{Ordering, AtomicU32};


    #[test]
    fn untracked() {
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
    fn scoped() {
        let the_result = 42;
        let result = super::scope(|scope| {
            let result = &*scope.alloc().alloc_new(SpinLock::new(None));

            scope.spawn(|| {
                *result.lock() = Some(the_result);
            }).join();

            scope.spawn(|| {
                return result.lock().unwrap();
            }).join()
        });
        assert_eq!(result, 42);
    }

    #[test]
    fn nested_scopes() {
        let result = super::scope(|scope| {
            assert_eq!(scope.alloc().stats().total_allocated, super::SCOPE_ARENA_INITIAL_SIZE);

            let p1 = scope.alloc() as *const _ as usize;

            let n0 = scope.alloc().current_block_used();
            let x = scope.alloc().alloc_new(33);
            let n1 = scope.alloc().current_block_used();
            assert_eq!(n1 - n0, 4);

            let y = super::scope(|scope| {
                let p2 = scope.alloc() as *const _ as usize;
                assert_eq!(p2, p1);

                let n2 = scope.alloc().current_block_used();
                let x = scope.alloc().alloc_new(36);
                let n3 = scope.alloc().current_block_used();
                assert_eq!(n3 - n2, 4);

                return *x;
            });

            let n4 = scope.alloc().current_block_used();
            assert_eq!(n4, n1);

            return *x + y;
        });
        assert_eq!(result, 69);
    }

    #[test]
    fn for_each() {
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
        let values = Vec::from_iter(0..100);

        let squared = super::map(&values, |v| *v * *v);
        assert_eq!(squared.cap(), 100);
        assert_eq!(squared.len(), 100);

        for (i, v) in values.iter().copied().enumerate() {
            assert_eq!(squared[i], v*v)
        }
    }
}

