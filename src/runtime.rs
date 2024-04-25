use sti::boks::Box;
use sti::vec::Vec;
use std::sync::{Arc, Mutex, Condvar};
use sti::mem::{Cell, UnsafeCell, MaybeUninit, ManuallyDrop};
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicPtr, Ordering};
use core::ptr::NonNull;
use core::marker::PhantomData;

use crate::{Task, XorShift32};
use crate::deque::Deque;



static STATE: AtomicU8 = AtomicU8::new(UNINIT);

const UNINIT: u8 = 0;
const INITING: u8 = 1;
const INIT: u8 = 2;

static RUNTIME: AtomicPtr<Runtime> = AtomicPtr::new(core::ptr::null_mut());


pub(crate) struct Runtime {
    has_terminator: AtomicBool,
    terminating: AtomicBool,

    termination_sleeper: Arc<Sleeper>,
    running_workers: AtomicU32,

    workers: Vec<Arc<Worker>>,

    injector_mutex: Mutex<()>,
    injector_deque: Deque<Task>,
}

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
            let ptr = Box::new(Runtime::init()).into_raw_parts();
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

        let workers = Vec::from_iter(
            (0..num_workers).map(|_| { Worker::spawn() }));

        Self {
            has_terminator: AtomicBool::new(false),
            terminating: AtomicBool::new(false),
            termination_sleeper: Arc::new(Sleeper::new()),
            running_workers: AtomicU32::new(num_workers),
            workers,
            injector_mutex: Mutex::new(()),
            injector_deque: Deque::new(),
        }
    }


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

        for worker in this.workers.iter() {
            if worker.wake() {
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
            let task = StackTask::with_sleeper(sleeper, f);
            Runtime::inject_task(unsafe { task.create_task() });

            let ok = task.complete();
            if ok {
                unsafe { task.into_result() }
            }
            else {
                todo!()
            }
        })
    }


    #[inline]
    pub fn work_while<F: FnMut() -> bool>(f: F) {
        Worker::current().work_while(f);
    }

    fn steal_task(&self, rng: &mut XorShift32, exclude: *const Worker) -> Option<Task> {
        let mut retry = true;
        while retry {
            retry = false;

            let mut at = rng.next_n(self.workers.len() as u32) as usize;
            for _ in 0..self.workers.len() {
                let worker = &self.workers[at];
                if worker.as_ref() as *const _ != exclude {
                    match worker.steal_from() {
                        Ok(task) => return Some(task),
                        Err(empty) => retry |= !empty,
                    }
                }

                at += 1;
                if at == self.workers.len() { at = 0; }
            }
        }

        self.injector_deque.steal().ok()
    }

    #[inline]
    fn terminating(&self) -> bool {
        self.terminating.load(Ordering::Acquire)
    }

    #[inline]
    fn tasks_pending(&self) -> bool {
        self.injector_deque.len() > 0
    }

    #[inline]
    fn worker_exit() {
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



thread_local! {
    static WORKER: Cell<*const Worker> = Cell::new(core::ptr::null());
}

struct Worker {
    sleeper: Sleeper,
    deque: Deque<Task>,
    steal_rng: Cell<XorShift32>,
}

unsafe impl Sync for Worker {}

impl Worker {
    fn spawn() -> Arc<Worker> {
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
    fn try_current() -> Option<&'static Worker> {
        WORKER.with(|ptr| {
            let ptr = ptr.get();
            if ptr.is_null() {
                return None;
            }
            return Some(unsafe { &*ptr });
        })
    }

    #[inline]
    fn current() -> &'static Worker {
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

    fn work_while<F: FnMut() -> bool>(&self, mut f: F) {
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
    fn push_task(&self, task: Task) {
        debug_assert!(core::ptr::eq(self, Worker::current()));
        unsafe { self.deque.push(task) }
    }

    #[inline]
    fn steal_from(&self) -> Result<Task, bool> {
        debug_assert!(!core::ptr::eq(self, Worker::current()));
        self.deque.steal()
    }

    #[inline]
    fn wake(&self) -> bool {
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



struct Sleeper {
    sleeping: Mutex<bool>,
    condvar: Condvar,
}

impl Sleeper {
    #[inline]
    fn new() -> Self {
        Self {
            sleeping: Mutex::new(false),
            condvar: Condvar::new(),
        }
    }

    #[inline]
    fn prime(&self) {
        let mut sleeping = self.sleeping.lock().unwrap();
        debug_assert!(*sleeping == false);
        *sleeping = true;
    }

    #[inline]
    fn unprime(&self) {
        let mut sleeping = self.sleeping.lock().unwrap();
        *sleeping = false;
    }

    #[inline]
    fn sleep(&self) {
        let mut sleeping = self.sleeping.lock().unwrap();
        if *sleeping {
            sleeping = self.condvar.wait(sleeping).unwrap();
            debug_assert!(!*sleeping);
        }
    }

    #[inline]
    fn wake(&self) -> bool {
        let mut sleeping = self.sleeping.lock().unwrap();
        let was_sleeping = core::mem::replace(&mut *sleeping, false);
        drop(sleeping);

        if was_sleeping {
            self.condvar.notify_one();
        }
        return was_sleeping;
    }
}


const LATCH_WAIT:  u8 = 0;
const LATCH_SLEEP: u8 = 1;
const LATCH_READY: u8 = 2;
const LATCH_PANIC: u8 = 3;

struct Latch<'a> {
    state: AtomicU8,
    sleeper: &'a Sleeper,
}

impl<'a> Latch<'a> {
    #[inline]
    fn new(sleeper: &'a Sleeper) -> Self {
        Self { state: AtomicU8::new(LATCH_WAIT), sleeper }
    }

    #[inline]
    fn wait(&self) -> bool {
        let prev = self.state.swap(LATCH_SLEEP, Ordering::AcqRel);
        debug_assert!([LATCH_WAIT, LATCH_READY, LATCH_PANIC].contains(&prev));
        if prev != LATCH_WAIT {
            return prev == LATCH_READY;
        }

        self.wait_slow_path()
    }

    #[cold]
    fn wait_slow_path(&self) -> bool {
        let rt = Runtime::get();
        let worker = Worker::try_current();

        loop {
            if let Some(worker) = worker {
                while let Some(task) = worker.find_task(rt) {
                    task.call();

                    // exit asap to reduce latency.
                    if let Some(result) = check_completion(self) {
                        return result;
                    }
                }
            }


            // cf: comment in `Worker::main`
            // nb: we don't check for the termination signal because
            //  that would result in a busy wait loop if there is no work
            //  available. when eventually returning to the Worker main
            //  loop, we'll check for termination before going to sleep
            //  and exit instead.
            self.sleeper.prime();

            if let Some(result) = check_completion(self) {
                self.sleeper.unprime();
                return result;
            }

            if rt.tasks_pending() {
                self.sleeper.unprime();
                continue;
            }


            self.sleeper.sleep();

            if let Some(result) = check_completion(self) {
                return result;
            }
        }

        #[inline]
        fn check_completion(latch: &Latch) -> Option<bool> {
            let state = latch.state.load(Ordering::Acquire);
            debug_assert!([LATCH_SLEEP, LATCH_READY, LATCH_PANIC].contains(&state));
            if state != LATCH_SLEEP {
                return Some(state == LATCH_READY);
            }
            return None;
        }
    }

    #[inline]
    fn set(&self, ok: bool) {
        let s = if ok { LATCH_READY } else { LATCH_PANIC };

        let prev = self.state.swap(s, Ordering::AcqRel);
        debug_assert!([LATCH_WAIT, LATCH_SLEEP].contains(&prev));
        if prev == LATCH_SLEEP {
            self.sleeper.wake();
        }
    }
}


pub(crate) struct StackTask<'a, R, F: FnOnce() -> R + Send> {
    f:      UnsafeCell<ManuallyDrop<F>>,
    result: UnsafeCell<MaybeUninit<R>>,
    latch:  Latch<'a>,
}

impl<'a, R, F: FnOnce() -> R + Send> StackTask<'a, R, F> {
    #[inline]
    pub fn new_on_worker(f: F) -> Self {
        Self::with_sleeper(&Worker::current().sleeper, f)
    }

    #[inline]
    fn with_sleeper(sleeper: &'a Sleeper, f: F) -> Self {
        Self {
            f:      UnsafeCell::new(ManuallyDrop::new(f)),
            result: UnsafeCell::new(MaybeUninit::uninit()),
            latch:  Latch::new(sleeper),
        }
    }

    #[inline]
    pub unsafe fn create_task(&self) -> Task {
        Task {
            ptr: NonNull::from(self).cast(),
            call: |ptr| unsafe {
                let this = ptr.cast::<Self>().as_ref();
                let f = ManuallyDrop::take(&mut *this.f.get());

                // @panic
                let result = f();

                this.result.get().write(MaybeUninit::new(result));
                this.latch.set(true);
            },
        }
    }

    #[must_use]
    #[inline]
    pub fn complete(&self) -> bool {
        self.latch.wait()
    }

    #[inline]
    pub unsafe fn into_result(self) -> R {
        unsafe { self.result.into_inner().assume_init() }
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
        for worker in rt.workers.iter() {
            worker.wake();
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
            Box::from_raw_parts(ptr)
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



