use sti::arena::Arena;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use core::cell::UnsafeCell;
use core::ptr::NonNull;
use core::mem::MaybeUninit;
use crate::{CovariantLifetime, InvariantLifetime};

use crate::{Runtime, Task};


pub fn scope<'p, R: Send, F: for<'s> FnOnce(&Scope<'s, 'p>) -> R + Send>(f: F) -> R {
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

    Runtime::on_worker(|| {
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

            let running = &scope.state.running;
            Runtime::work_while(|| running.load(Ordering::SeqCst) != 0);

            // safety:
            // - allocations cannot escape `f`, so there are no external dangling refs.
            // - during the execution of inner scopes, outer scopes are inaccessible,
            //   so inner scopes cannot accidentally free outer allocations.
            // - tasks cannot access outer scopes, so they can't make allocations while
            //   inner scopes are running.
            unsafe { alloc.restore(save) };

            return result
        })
    })
}


pub struct Scope<'s, 'p: 's> {
    alloc: &'s Arena,
    state: ScopeState,
    _s: InvariantLifetime<'s>,
    _p: CovariantLifetime<'p>,
}

pub const SCOPE_ARENA_INITIAL_SIZE: usize = 64*1024;

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
                state: AtomicU8::new(RESULT_WAIT),
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
                debug_assert_eq!(result.state.load(Ordering::SeqCst), RESULT_WAIT);

                let f = unsafe { (&mut (*ptr).f as *mut F).read() };
                // @panic.
                let value = f();

                unsafe { result.value.get().write(MaybeUninit::new(value)) };
                result.state.store(RESULT_DONE, Ordering::SeqCst);

                let prev_running = scope.running.fetch_sub(1, Ordering::SeqCst);
                debug_assert!(prev_running > 0);
            },
        };

        let prev_running = self.state.running.fetch_add(1, Ordering::SeqCst);
        assert!(prev_running < usize::MAX);

        Runtime::submit_task_on_worker(task);

        return ScopeTask { result };
    }
}

const RESULT_WAIT: u8 = 0;
const RESULT_DONE: u8 = 1;
const RESULT_PANIC: u8 = 2;
const RESULT_TAKEN: u8 = 3;

struct ScopeTaskResult<R> {
    state: AtomicU8,
    value: UnsafeCell<MaybeUninit<R>>,
}

pub struct ScopeTask<'s, R> {
    result: &'s ScopeTaskResult<R>,
}

impl<'s, R> ScopeTask<'s, R> {
    pub fn join(self) -> R {
        let state = &self.result.state;
        Runtime::work_while(|| state.load(Ordering::SeqCst) == RESULT_WAIT);

        let state = state.load(Ordering::SeqCst);
        if state == RESULT_DONE {
            unsafe { self.result.value.get().read().assume_init() }
        }
        else {
            todo!()
        }
    }
}


#[cfg(test)]
mod tests {
    use sti::sync::spin_lock::SpinLock;

    #[test]
    fn scoped() {
        let _t = crate::Terminator::new();

        let the_result = 42;
        let result = crate::scope(|scope| {
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
        let _t = crate::Terminator::new();

        let result = crate::scope(|scope| {
            assert_eq!(scope.alloc().stats().total_allocated, crate::scope::SCOPE_ARENA_INITIAL_SIZE);

            let p1 = scope.alloc() as *const _ as usize;

            let n0 = scope.alloc().current_block_used();
            let x = scope.alloc().alloc_new(33);
            let n1 = scope.alloc().current_block_used();
            assert_eq!(n1 - n0, 4);

            let y = crate::scope(|scope| {
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
}

