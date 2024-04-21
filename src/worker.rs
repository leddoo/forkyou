use sti::cell::Cell;
use std::sync::{Arc, Mutex, Condvar};

use crate::Task;
use crate::deque::Deque;



thread_local! {
    static WORKER: Cell<*mut Worker> = Cell::new(core::ptr::null_mut());
}

pub(crate) struct Worker {
    sleeper: Arc<Sleeper>,
    deque: Deque<Task>,
}

pub(crate) struct Sleeper {
    mutex: Mutex<bool>,
    condvar: Condvar,
}

impl Worker {
    pub fn spawn() -> Arc<Sleeper> {
        let sleeper = Arc::new(Sleeper {
            mutex: Mutex::new(false),
            condvar: Condvar::new(),
        });

        std::thread::spawn(sti::enclose!(sleeper; || {
            let mut dont_touch_this = Worker {
                sleeper,
                deque: Deque::new(),
            };
            WORKER.with(|ptr| {
                ptr.set(&mut dont_touch_this);
            });

            Self::main();
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

    fn main() {
        loop {
            println!("working {:p}", Self::current());
        }
    }
}

