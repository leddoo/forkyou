use sti::mem::Cell;
use std::sync::{Arc, Mutex, Condvar};

use crate::{Task, Runtime, XorShift32};
use crate::deque::Deque;



thread_local! {
    static WORKER: Cell<*const Worker> = Cell::new(core::ptr::null());
}

pub(crate) struct Worker {
    sleeper: Sleeper,
    deque: Deque<Task>,
    steal_rng: Cell<XorShift32>,
}

unsafe impl Sync for Worker {}

impl Worker {
    pub fn spawn() -> Arc<Worker> {
        let worker = Arc::new(Worker {
            sleeper: Sleeper::new(),
            deque: Deque::new(),
            steal_rng: Cell::new(XorShift32::new()),
        });

        worker.steal_rng.set(XorShift32::from_seed(
            sti::hash::fxhash::fxhash32(&(worker.as_ref() as *const _))));

        std::thread::spawn(sti::enclose!(worker; move || {
            WORKER.with(|ptr| {
                ptr.set(&*worker);
            });

            // @panic
            Self::main(Self::current());

            WORKER.with(|ptr| {
                ptr.set(core::ptr::null());
            });

            Runtime::worker_exit();
        }));

        return worker;
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
    fn find_task(&self, rt: &Runtime) -> Option<Task> {
        let task = unsafe { self.deque.pop() };
        if task.is_some() { return task }

        let mut rng = self.steal_rng.get();
        let task = rt.steal_task(&mut rng, self);
        self.steal_rng.set(rng);
        if task.is_some() { return task }

        return None;
    }

    pub fn work_while<F: FnMut() -> bool>(&self, mut f: F) {
        debug_assert!(core::ptr::eq(self, Worker::current()));

        let rt = Runtime::get();
        while f() {
            let Some(task) = self.find_task(rt) else {
                debug_assert!(false, "work while ran out of work");

                std::thread::sleep(std::time::Duration::from_millis(1));
                continue;
            };
            task.call();
        }
    }

    #[inline]
    pub fn push_task(&self, task: Task) {
        debug_assert!(core::ptr::eq(self, Worker::current()));
        unsafe { self.deque.push(task) }
    }

    #[inline]
    pub fn steal_from(&self) -> Result<Task, bool> {
        debug_assert!(!core::ptr::eq(self, Worker::current()));
        self.deque.steal()
    }

    #[inline]
    pub fn wake(&self) -> bool {
        self.sleeper.wake()
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


