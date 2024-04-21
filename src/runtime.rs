use sti::vec::Vec;
use std::sync::{Arc, Mutex, Condvar};
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicU8, Ordering};

use crate::Task;
use crate::deque::Deque;
use crate::worker::{Worker, Sleeper};


pub(crate) struct Runtime {
    sleepers: Vec<Arc<Sleeper>>,

    injector_mutex: Mutex<()>,
    injector_deque: Deque<Task>,
}

impl Runtime {
    pub unsafe fn submit_task(task: Task) {
        if let Some(worker) = Worker::try_current() {
            unsafe { worker.push_task(task) }
        }
        else {
            unsafe { Self::inject_task(task) }
        }
    }

    #[cold]
    pub unsafe fn inject_task(task: Task) {
        let this = Self::get();

        unsafe {
            let guard = this.injector_mutex.lock();
            this.injector_deque.push(task);
            drop(guard);
        }

        todo!("wake worker if necessary")
    }

    pub fn on_worker<R: Send, F: FnOnce() -> R + Send>(f: F) -> R {
        if Worker::try_current().is_some() {
            return f();
        }

        // stack task.
        // maybe like a thread local sleeper?
        todo!()
    }
}


static mut RUNTIME: MaybeUninit<Runtime> = MaybeUninit::uninit();

static STATE: AtomicU8 = AtomicU8::new(UNINIT);

const UNINIT: u8 = 0;
const INITING: u8 = 1;
const INIT: u8 = 2;

impl Runtime {
    #[inline]
    fn get() -> &'static Self {
        let s = STATE.load(Ordering::Acquire);
        if s == INIT {
            return unsafe { &mut *RUNTIME.as_mut_ptr() };
        }

        return Self::get_slow_path();
    }

    fn get_slow_path() -> &'static Self {
        if STATE.compare_exchange(
            UNINIT, INITING,
            Ordering::SeqCst, Ordering::SeqCst).is_ok()
        {
            unsafe { RUNTIME = MaybeUninit::new(Self::init()) };
            STATE.store(INIT, Ordering::Release);
        }
        else {
            while STATE.load(Ordering::Acquire) != INIT {
                std::thread::yield_now();
            }
        }

        return unsafe { &mut *RUNTIME.as_mut_ptr() };
    }

    #[cold]
    fn init() -> Self {
        let sleepers = Vec::from_iter(
            (0..crate::ncpu()).map(|_| { Worker::spawn() }));

        Self {
            sleepers,
            injector_mutex: Mutex::new(()),
            injector_deque: Deque::new(),
        }
    }
}


#[cfg(test)]
mod tests {
    use super::Runtime;

    #[test]
    fn test() {
        Runtime::get();
    }
}



