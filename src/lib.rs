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
            alloc: unsafe { sti::erase!(&Arena, alloc) },
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
                let f = unsafe { (&mut (*ptr).f as *mut F).read() };
                let scope = unsafe { (*ptr).scope };
                let result = unsafe { &(*ptr).result };
                debug_assert_eq!(result.state.load(Ordering::SeqCst), SCOPE_TASK_RESULT_WAIT);

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



