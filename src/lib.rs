use sti::alloc::{Alloc, GlobalAlloc};
use sti::arena::Arena;
use sti::boks::Box;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use core::ptr::NonNull;
use core::cell::UnsafeCell;
use core::mem::{ManuallyDrop, MaybeUninit};
use core::marker::PhantomData;


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
            let arena = Arena::new();
            arena.min_block_size.set(64*1024);
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

        // @temp
        assert_eq!(scope.state.running.load(Ordering::SeqCst), 0);

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

        // @temp!
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




/// tasks can borrow locals from parent.
/// ```rust
/// let foo = 42;
/// forkyou::scope(|scope| {
///     scope.spawn(|| foo);
/// });
/// ```
///
/// tasks cannot borrow locals from scope.
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
///     // can assign locals from parent.
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
/// ```compile_fail
/// let mut foo = &mut 42;
/// forkyou::scope(|scope| {
///     foo = scope.alloc().alloc_new(42);
/// });
/// ```
#[allow(dead_code)]
struct DocTests;

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use sti::sync::spin_lock::SpinLock;


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
}

