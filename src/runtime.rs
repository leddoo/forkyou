use sti::vec::Vec;
use std::sync::{Arc, Mutex};
use core::mem::{MaybeUninit, ManuallyDrop};
use core::sync::atomic::{AtomicU8, Ordering};
use core::ptr::NonNull;

use crate::Task;
use crate::deque::Deque;
use crate::worker::{Worker, Sleeper};


pub(crate) struct Runtime {
    sleepers: Vec<Arc<Sleeper>>,

    injector_mutex: Mutex<()>,
    injector_deque: Deque<Task>,
}

impl Runtime {
    pub fn submit_task(task: Task) {
        if let Some(worker) = Worker::try_current() {
            unsafe { worker.push_task(task) }
        }
        else {
            Self::inject_task(task);
        }
    }

    pub fn submit_task_on_worker(task: Task) {
        let worker = Worker::current();
        unsafe { worker.push_task(task) }
    }

    #[cold]
    pub fn inject_task(task: Task) {
        let this = Self::get();

        unsafe {
            let guard = this.injector_mutex.lock();
            this.injector_deque.push(task);
            drop(guard);
        }

        for sleeper in this.sleepers.iter() {
            if sleeper.wake() {
                break;
            }
        }
    }

    pub fn on_worker<R: Send, F: FnOnce() -> R + Send>(f: F) -> R {
        if Worker::try_current().is_some() {
            f()
        }
        else {
            Self::on_worker_slow_path(f)
        }
    }

    #[cold]
    pub fn on_worker_slow_path<R: Send, F: FnOnce() -> R + Send>(f: F) -> R {
        thread_local! {
            static SLEEPER: Sleeper = Sleeper::new();
        }

        SLEEPER.with(|sleeper| {
            struct FBox<'a, R, F> {
                f: ManuallyDrop<F>,
                sleeper: &'a Sleeper,
                result: MaybeUninit<R>,
            }

            let mut fbox = FBox {
                f: ManuallyDrop::new(f),
                sleeper,
                result: MaybeUninit::uninit(),
            };

            let ptr = NonNull::from(&mut fbox).cast();
            let call = |ptr: NonNull<u8>| {
                let ptr = ptr.cast::<FBox<R, F>>().as_ptr();
                let fbox = unsafe { &mut *ptr };

                // @panic.
                let f = unsafe { ManuallyDrop::take(&mut fbox.f) };

                let result = f();
                fbox.result = MaybeUninit::new(result);

                fbox.sleeper.wake();
            };

            sleeper.prime();

            let task = unsafe { Task::new(ptr, call) };
            Runtime::inject_task(task);

            sleeper.sleep();

            return unsafe { fbox.result.assume_init() };
        })
    }

    #[inline]
    pub fn steal_task(&self) -> Option<Task> {
        self.injector_deque.steal().ok()
    }
}


static mut RUNTIME: MaybeUninit<Runtime> = MaybeUninit::uninit();

static STATE: AtomicU8 = AtomicU8::new(UNINIT);

const UNINIT: u8 = 0;
const INITING: u8 = 1;
const INIT: u8 = 2;

impl Runtime {
    #[inline]
    pub fn get() -> &'static Self {
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



