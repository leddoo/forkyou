use sti::cell::Cell;
use std::sync::{Arc, Mutex, Condvar};

use crate::{Task, Runtime};
use crate::deque::Deque;



thread_local! {
    static WORKER: Cell<*mut Worker> = Cell::new(core::ptr::null_mut());
}

pub(crate) struct Worker {
    sleeper: Arc<Sleeper>,
    deque: Deque<Task>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        Runtime::get().worker_exit();
    }
}

impl Worker {
    pub fn spawn() -> Arc<Sleeper> {
        let sleeper = Arc::new(Sleeper::new());

        std::thread::spawn(sti::enclose!(sleeper; || {
            let mut dont_touch_this = Worker {
                sleeper,
                deque: Deque::new(),
            };
            WORKER.with(|ptr| {
                ptr.set(&mut dont_touch_this);
            });

            Self::main(Self::current());
        }));

        return sleeper;
    }

    #[inline]
    pub fn try_current() -> Option<&'static Worker> {
        WORKER.with(|ptr| {
            let ptr = ptr.get();
            if ptr.is_null() {
                return None;
            }
            return Some(unsafe { &*ptr });
        })
    }

    #[inline]
    pub fn current() -> &'static Worker {
        Self::try_current().expect("not on a worker")
    }

    #[inline]
    pub unsafe fn push_task(&self, task: Task) {
        unsafe { self.deque.push(task) }
    }

    fn main(&self) {
        let rt = Runtime::get();
        loop {
            while let Some(task) = self.find_task(rt) {
                task.call();
            }

            // initiate sleeping.
            self.sleeper.prime();

            // we may only sleep if we know we will be woken up again.
            // since the sleeper is now primed, any `wake` calls from the runtime
            // after this point will wake us up (or prevent us from going to sleep).
            // however it is possible that the runtime tried to wake us before the
            // sleeper was primed (ie, it assumed we were awake) and now waits for us
            // to terminate or complete tasks.
            // thus, before actually going to sleep, we must check again for termination
            // or pending tasks.

            if rt.terminating() {
                break;
            }

            if rt.tasks_pending() {
                self.sleeper.unprime();
                continue;
            }

            self.sleeper.sleep();
        }
    }

    fn find_task(&self, rt: &Runtime) -> Option<Task> {
        if let Some(task) = unsafe { self.deque.pop() } {
            return Some(task);
        }

        if let Some(task) = rt.steal_task() {
            return Some(task);
        }

        return None;
    }
}



// @todo: move into separate module?
pub(crate) struct Sleeper {
    sleeping: Mutex<bool>,
    condvar: Condvar,
}

impl Sleeper {
    #[inline]
    pub fn new() -> Self {
        Self {
            sleeping: Mutex::new(false),
            condvar: Condvar::new(),
        }
    }

    #[inline]
    pub fn prime(&self) {
        let mut sleeping = self.sleeping.lock().unwrap();
        *sleeping = true;
    }

    #[inline]
    pub fn unprime(&self) {
        let mut sleeping = self.sleeping.lock().unwrap();
        *sleeping = false;
    }

    #[inline]
    pub fn sleep(&self) {
        let mut sleeping = self.sleeping.lock().unwrap();
        if *sleeping {
            sleeping = self.condvar.wait(sleeping).unwrap();
            debug_assert!(!*sleeping);
        }
    }

    #[inline]
    pub fn wake(&self) -> bool {
        let mut sleeping = self.sleeping.lock().unwrap();
        let was_sleeping = core::mem::replace(&mut *sleeping, false);
        drop(sleeping);

        if was_sleeping {
            self.condvar.notify_one();
        }
        return was_sleeping;
    }
}


