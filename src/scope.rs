use sti::arena::Arena;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use crate::{CovariantLifetime, InvariantLifetime};

use crate::Runtime;
use crate::runtime::{Latch, StackTask, StackTaskHandle};


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
                    running: AtomicUsize::new(1),
                    latch: Latch::new_on_worker(),
                },
                _s: Default::default(),
                _p: Default::default(),
            };

            let result = f(&scope);

            if scope.state.running.fetch_sub(1, Ordering::AcqRel) != 1 {
                let ok = scope.state.latch.wait();
                debug_assert!(ok);
            }

            if scope.state.panic.load(Ordering::Acquire) {
                todo!()
            }

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
    state: ScopeState<'s>,
    _s: InvariantLifetime<'s>,
    _p: CovariantLifetime<'p>,
}

pub const SCOPE_ARENA_INITIAL_SIZE: usize = 64*1024;

struct ScopeState<'s> {
    panic: AtomicBool,
    running: AtomicUsize,
    latch: Latch<'s>
}

impl<'s, 'p: 's> Scope<'s, 'p> {
    pub fn alloc(&self) -> &'s Arena {
        self.alloc
    }

    pub fn spawn<R, F: FnOnce() -> R + Send + 'p>(&self, f: F) -> ScopeTask<'s, R> {
        let state = unsafe { sti::erase!(&ScopeState, &self.state) };

        let task = self.alloc.alloc_new(StackTask::new_on_worker(move || {
            // @panic
            let result = f();

            let prev_running = state.running.fetch_sub(1, Ordering::AcqRel);
            debug_assert!(prev_running > 0);
            if prev_running == 1 {
                unsafe { Latch::set(&state.latch, true) };
            }

            return result;
        }));

        let prev_running = self.state.running.fetch_add(1, Ordering::AcqRel);
        assert!(prev_running < usize::MAX);

        Runtime::submit_task_on_worker(unsafe { task.create_task() });

        return ScopeTask { handle: task.handle() };
    }
}


pub struct ScopeTask<'s, R> {
    handle: StackTaskHandle<'s, 's, R>,
}

impl<'s, R> ScopeTask<'s, R> {
    #[inline]
    pub fn join(self) -> R {
        unsafe { self.handle.join().expect("todo") }
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

    #[test]
    fn scoped_nop() {
        let _t = crate::Terminator::new();

        crate::scope(|_| {
        });

        crate::scope(|scope| {
            scope.spawn(|| {});
        });

        crate::scope(|scope| {
            scope.spawn(|| {
                crate::scope(|_| {
                });
            });
        });

        crate::scope(|scope| {
            scope.spawn(|| {
                crate::scope(|scope| {
                    scope.spawn(|| {});
                });
            });
        });
    }

    #[test]
    fn scoped_read_bool() {
        let _t = crate::Terminator::new();

        crate::scope(|scope| {
            let mut i = 0;
            loop {
                let a = scope.spawn(move || i < 100);
                let b = scope.spawn(move || i <  99);
                let c = scope.spawn(move || i <  98);
                let d = scope.spawn(move || i <  97);
                let e = scope.spawn(move || i <  96);

                if a.join() & b.join() & c.join() & d.join() & e.join() {
                    i += 5;
                }
                else { break }
            }
        });
    }
}

