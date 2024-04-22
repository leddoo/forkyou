use sti::alloc::GlobalAlloc;
use sti::boks::Box;
use sti::vec::Vec;
use std::sync::{Arc, Mutex};
use core::mem::{MaybeUninit, ManuallyDrop};
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicPtr, Ordering};
use core::ptr::NonNull;
use core::marker::PhantomData;

use crate::Task;
use crate::deque::Deque;
use crate::worker::{Worker, Sleeper};


pub(crate) struct Runtime {
    has_terminator: AtomicBool,
    terminating: AtomicBool,

    termination_sleeper: Arc<Sleeper>,
    running_workers: AtomicU32,

    sleepers: Vec<Arc<Sleeper>>,

    injector_mutex: Mutex<()>,
    injector_deque: Deque<Task>,
}

impl Runtime {
    pub fn submit_task(task: Task) {
        if let Some(worker) = Worker::try_current() {
            worker.push_task(task);
        }
        else {
            Self::inject_task(task);
        }
    }

    #[inline]
    pub fn submit_task_on_worker(task: Task) {
        let worker = Worker::current();
        worker.push_task(task);
    }

    #[cold]
    pub fn inject_task(task: Task) {
        let this = Runtime::get();

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

    #[inline]
    pub fn on_worker<R: Send, F: FnOnce() -> R + Send>(f: F) -> R {
        if Worker::try_current().is_some() {
            f()
        }
        else {
            Runtime::on_worker_slow_path(f)
        }
    }

    #[cold]
    fn on_worker_slow_path<R: Send, F: FnOnce() -> R + Send>(f: F) -> R {
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


    pub fn work_while<F: FnMut() -> bool>(mut f: F) {
        let rt = Runtime::get();
        let worker = Worker::current();
        while f() {
            let Some(task) = worker.find_task(rt) else {
                debug_assert!(false, "work while ran out of work");

                std::thread::sleep(std::time::Duration::from_millis(1));
                continue;
            };
            task.call();
        }
    }

    #[inline]
    pub fn steal_task(&self) -> Option<Task> {
        self.injector_deque.steal().ok()
    }

    #[inline]
    pub fn terminating(&self) -> bool {
        self.terminating.load(Ordering::Acquire)
    }

    #[inline]
    pub fn tasks_pending(&self) -> bool {
        self.injector_deque.len() > 0
    }

    #[inline]
    pub fn worker_exit() {
        let rt = Runtime::get();

        let n = rt.running_workers.fetch_sub(1, Ordering::SeqCst);
        assert!(n != 0);

        // not sure this is necessary.
        // without it, miri complains about `Runtime::get()`
        // racing with the `drop(rt)` in `Terminator::drop`.
        // it shouldn't be a problem cause clearly we need to get
        // the runtime, before we can signal the termination sleeper.
        // but since we're not on any hot path here, we'll do as miri says.
        core::sync::atomic::fence(Ordering::SeqCst);

        if n == 1 {
            // `rt` is dropped by the `wake` call,
            // so we can't keep any refs into it (`termination_sleeper`),
            // even if we don't use them anymore (strong protectors).
            let sleeper = rt.termination_sleeper.clone();
            sleeper.wake();
        }
    }
}


static RUNTIME: AtomicPtr<Runtime> = AtomicPtr::new(core::ptr::null_mut());

static STATE: AtomicU8 = AtomicU8::new(UNINIT);

const UNINIT: u8 = 0;
const INITING: u8 = 1;
const INIT: u8 = 2;

impl Runtime {
    #[inline]
    pub fn get() -> &'static Runtime {
        let p = RUNTIME.load(Ordering::Acquire);
        if !p.is_null() {
            return unsafe { &*p };
        }

        return Runtime::get_slow_path();
    }

    #[cold]
    fn get_slow_path() -> &'static Runtime {
        // syncs with terminator drop.
        // again, this is mostly to please miri.
        // in a real program, this is only executed once.
        core::sync::atomic::fence(Ordering::SeqCst);

        if STATE.compare_exchange(
            UNINIT, INITING,
            Ordering::SeqCst, Ordering::SeqCst).is_ok()
        {
            let ptr = Box::new(Runtime::init()).into_raw_parts().0;
            RUNTIME.store(ptr.as_ptr(), Ordering::Release);
            STATE.store(INIT, Ordering::Release);
        }
        else {
            while STATE.load(Ordering::Acquire) == INITING {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            assert_eq!(STATE.load(Ordering::Acquire), INIT);
        }

        return unsafe { &*RUNTIME.load(Ordering::Acquire) };
    }

    #[cold]
    fn init() -> Self {
        let num_workers = crate::ncpu() as u32;

        let sleepers = Vec::from_iter(
            (0..num_workers).map(|_| { Worker::spawn() }));

        Self {
            has_terminator: AtomicBool::new(false),
            terminating: AtomicBool::new(false),
            termination_sleeper: Arc::new(Sleeper::new()),
            running_workers: AtomicU32::new(num_workers),
            sleepers,
            injector_mutex: Mutex::new(()),
            injector_deque: Deque::new(),
        }
    }
}


pub struct Terminator {
    _unsend: PhantomData<*mut ()>,
}

impl Terminator {
    pub fn new() -> Self {
        if Worker::try_current().is_some() {
            panic!("terminator on worker");
        }

        let rt = Runtime::get();
        if rt.has_terminator.swap(true, Ordering::SeqCst) {
            panic!("multiple terminators");
        }

        Terminator { _unsend: PhantomData }
    }
}

impl Drop for Terminator {
    fn drop(&mut self) {
        let rt = Runtime::get();
        debug_assert!(rt.has_terminator.load(Ordering::SeqCst));

        rt.termination_sleeper.prime();
        rt.terminating.store(true, Ordering::Release);
        for sleeper in rt.sleepers.iter() {
            sleeper.wake();
        }
        rt.termination_sleeper.sleep();

        // refer to the comment in worker_exit.
        core::sync::atomic::fence(Ordering::SeqCst);
        let n = rt.running_workers.load(Ordering::SeqCst);
        assert_eq!(n, 0);


        STATE.store(INITING, Ordering::Release);
        let rt = unsafe {
            let ptr = RUNTIME.swap(core::ptr::null_mut(), Ordering::SeqCst);
            let ptr = NonNull::new(ptr).unwrap();
            Box::from_raw_parts(ptr, GlobalAlloc)
        };
        STATE.store(UNINIT, Ordering::Release);
        drop(rt);

        // refer to the comment in get_slow_path.
        core::sync::atomic::fence(Ordering::SeqCst);
    }
}


#[cfg(test)]
mod tests {
    use super::{Terminator, Ordering, STATE, INIT, UNINIT};

    #[test]
    fn graceful_shutdown() {
        {
            let _t = Terminator::new();
            assert_eq!(STATE.load(Ordering::SeqCst), INIT);
        }

        assert_eq!(STATE.load(Ordering::SeqCst), UNINIT);
    }
}



