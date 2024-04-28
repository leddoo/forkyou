use sti::boks::Box;
use sti::vec::Vec;
use std::sync::{Arc, Mutex, Condvar};
use sti::mem::{Cell, UnsafeCell, MaybeUninit, ManuallyDrop};
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicPtr, Ordering};
use core::ptr::NonNull;
use core::marker::PhantomData;

use crate::{Task, XorShift32, AssertSend};
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
    sleeping_workers: AtomicU32,

    workers: Vec<Arc<Worker>>,

    injector_mutex: Mutex<()>,
    injector_deque: Deque<Task>,
}

impl Runtime {
    #[inline]
    fn get_or_init() -> &'static Runtime {
        let p = RUNTIME.load(Ordering::Acquire);
        if !p.is_null() {
            return unsafe { &*p };
        }

        return Runtime::get_slow_path();
    }

    #[inline]
    fn expect() -> &'static Runtime {
        let p = RUNTIME.load(Ordering::Acquire);
        if p.is_null() {
            panic!("runtime not init");
        }
        return unsafe { &*p };
    }

    #[cold]
    fn get_slow_path() -> &'static Runtime {
        // syncs with terminator drop.
        // again, this is mostly to please miri.
        // in a real program, this is only executed once.
        // @todo investigate whether this actually does anything.
        core::sync::atomic::fence(Ordering::SeqCst);

        if STATE.compare_exchange(
            UNINIT, INITING,
            Ordering::SeqCst, Ordering::SeqCst).is_ok()
        {
            let ptr = Runtime::init();
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
    fn init() -> NonNull<Runtime> {
        let ptr: NonNull<Runtime> = sti::alloc::alloc_ptr(&sti::alloc::GlobalAlloc).expect("oom");

        let num_workers = crate::ncpu() as u32;

        let workers = Vec::from_iter(
            (0..num_workers).map(|_| { Worker::spawn(ptr) }));

        let this = Self {
            has_terminator: AtomicBool::new(false),
            terminating: AtomicBool::new(false),
            termination_sleeper: Arc::new(Sleeper::new()),
            running_workers: AtomicU32::new(num_workers),
            sleeping_workers: AtomicU32::new(0),
            workers,
            injector_mutex: Mutex::new(()),
            injector_deque: Deque::new(),
        };
        unsafe { ptr.as_ptr().write(this) };

        return ptr;
    }


    pub fn submit_task(task: Task) {
        if let Some(worker) = Worker::try_current() {
            worker.submit_task(task);
        }
        else {
            Self::inject_task(task);
        }
    }

    #[cold]
    fn inject_task(task: Task) {
        let this = Runtime::get_or_init();

        unsafe {
            let guard = this.injector_mutex.lock();
            this.injector_deque.push(task);
            drop(guard);
        }

        // always wake a worker, so we don't have to worry about deadlocks.
        this.wake_one_worker();
    }

    fn wake_one_worker(&self) {
        for worker in self.workers.iter() {
            if worker.wake() {
                break;
            }
        }
    }


    #[inline]
    pub fn on_worker<R: Send, F: FnOnce(&Worker, bool) -> R + Send>(f: F) -> R {
        if let Some(worker) = Worker::try_current() {
            f(worker, false)
        }
        else {
            Runtime::on_worker_slow_path(f)
        }
    }

    #[cold]
    fn on_worker_slow_path<R: Send, F: FnOnce(&Worker, bool) -> R + Send>(f: F) -> R {
        thread_local! {
            static SLEEPER: Sleeper = Sleeper::new();
        }

        SLEEPER.with(|sleeper| {
            let task = StackTask::with_sleeper(sleeper, f);
            Runtime::inject_task(unsafe { task.create_task() });
            unsafe { task.handle().join().expect("todo") }
        })
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
    fn worker_sleep(&self, sleeper: &Sleeper) {
        debug_assert!(Worker::try_current().is_some());

        // need to read running first, in case worker wakes up and exits
        // before we inc sleeping. if we read sleeping first, we could see
        // more sleeping than running workers.
        let running = self.running_workers.load(Ordering::Acquire);
        let prev_sleeping = self.sleeping_workers.fetch_add(1, Ordering::AcqRel);
        debug_assert!(prev_sleeping < running);

        sleeper.sleep();

        let prev_sleeping = self.sleeping_workers.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(prev_sleeping > 0);
    }

    #[inline]
    fn worker_exit(this: *const Runtime) {
        debug_assert!(core::ptr::eq(this, Runtime::expect()));
        let this = unsafe { &*this };

        let n = this.running_workers.fetch_sub(1, Ordering::SeqCst);
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
            let sleeper = this.termination_sleeper.clone();
            sleeper.wake();
        }
    }
}



thread_local! {
    static WORKER: Cell<*const Worker> = Cell::new(core::ptr::null());
}

pub(crate) struct Worker {
    runtime: NonNull<Runtime>,
    sleeper: Sleeper,
    deque: Deque<Task>,
    steal_rng: Cell<XorShift32>,
}

unsafe impl Sync for Worker {}

impl Worker {
    #[inline]
    pub fn runtime(&self) -> &Runtime {
        unsafe { self.runtime.as_ref() }
    }

    #[inline]
    pub fn num_workers(&self) -> usize {
        self.runtime().workers.len()
    }

    pub fn submit_task(&self, task: Task) {
        let had_more = self.push_task(task);
        if had_more {
            let rt = self.runtime();

            // this can't deadlock cause we're on a worker.
            // we'll get em next time.
            if rt.sleeping_workers.load(Ordering::Acquire) > 0 {
                rt.wake_one_worker();
            }
        }
    }


    fn spawn(runtime: NonNull<Runtime>) -> Arc<Worker> {
        let worker = Arc::new(Worker {
            runtime,
            sleeper: Sleeper::new(),
            deque: Deque::new(),
            steal_rng: Cell::new(XorShift32::new()),
        });

        worker.steal_rng.set(XorShift32::from_seed(
            sti::hash::fxhash::fxhash32(&(worker.as_ref() as *const _))));

        let send_worker = unsafe { AssertSend::new(worker.clone()) };
        std::thread::spawn(move || {
            let worker = send_worker.into_inner();

            // block until runtime is init.
            // this is critical!!
            let rt = Runtime::get_or_init();
            assert!(core::ptr::eq(worker.runtime.as_ptr(), rt));

            WORKER.with(|ptr| {
                ptr.set(&*worker);
            });

            // @panic
            Self::main(Self::current());

            WORKER.with(|ptr| {
                ptr.set(core::ptr::null());
            });

            Runtime::worker_exit(worker.runtime());
        });

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
    fn find_task(&self) -> Option<Task> {
        let task = unsafe { self.deque.pop() };
        if task.is_some() { return task }

        let mut rng = self.steal_rng.get();
        let task = self.runtime().steal_task(&mut rng, self);
        self.steal_rng.set(rng);
        if task.is_some() { return task }

        return None;
    }

    #[inline]
    fn push_task(&self, task: Task) -> bool {
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
        let rt = self.runtime();
        loop {
            while let Some(task) = self.find_task() {
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

            rt.worker_sleep(&self.sleeper);
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
        unsafe { Self::wake_unprotected(self) }
    }

    #[inline]
    unsafe fn wake_unprotected(this: *const Self) -> bool {
        let this = unsafe { &*this };
        let mut sleeping = this.sleeping.lock().unwrap();
        let was_sleeping = core::mem::replace(&mut *sleeping, false);
        if was_sleeping {
            this.condvar.notify_one();
        }
        return was_sleeping;
    }
}


const LATCH_WAIT:  u8 = 0;
const LATCH_SLEEP: u8 = 1;
const LATCH_READY: u8 = 2;
const LATCH_PANIC: u8 = 3;

pub(crate) struct Latch<'a> {
    state: AtomicU8,
    sleeper: &'a Sleeper,
}

impl<'a> Latch<'a> {
    #[inline]
    pub fn new_on_worker(worker: &'a Worker) -> Self {
        Self::with_sleeper(&worker.sleeper)
    }

    #[inline]
    fn with_sleeper(sleeper: &'a Sleeper) -> Self {
        Self { state: AtomicU8::new(LATCH_WAIT), sleeper }
    }

    #[inline]
    pub fn wait(&self) -> bool {
        if let Some(result) = self.check_completion_awake() {
            return result;
        }

        self.wait_slow_path()
    }

    #[cold]
    fn wait_slow_path(&self) -> bool {
        let rt = Runtime::expect();
        let worker = Worker::try_current();

        loop {
            if let Some(worker) = worker {
                while let Some(task) = worker.find_task() {
                    task.call();

                    // exit asap to reduce latency.
                    if let Some(result) = self.check_completion_awake() {
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

            if rt.tasks_pending() {
                self.sleeper.unprime();
                continue;
            }

            if let Some(result) = self.check_completion_for_sleep() {
                self.sleeper.unprime();
                return result;
            }


            if worker.is_some() {
                rt.worker_sleep(self.sleeper);
            }
            else {
                self.sleeper.sleep();
            }

            if let Some(result) = self.check_completion_awoken() {
                return result;
            }
        }
    }

    #[inline]
    fn check_completion_awake(&self) -> Option<bool> {
        let state = self.state.load(Ordering::Acquire);
        debug_assert!([LATCH_WAIT, LATCH_READY, LATCH_PANIC].contains(&state));
        if state != LATCH_WAIT {
            return Some(state == LATCH_READY);
        }
        return None;
    }

    #[inline]
    fn check_completion_awoken(&self) -> Option<bool> {
        let state = self.state.swap(LATCH_WAIT, Ordering::AcqRel);
        debug_assert!([LATCH_SLEEP, LATCH_READY, LATCH_PANIC].contains(&state));
        if state != LATCH_SLEEP {
            return Some(state == LATCH_READY);
        }
        return None;
    }

    #[inline]
    fn check_completion_for_sleep(&self) -> Option<bool> {
        let state = self.state.swap(LATCH_SLEEP, Ordering::AcqRel);
        debug_assert!([LATCH_WAIT, LATCH_READY, LATCH_PANIC].contains(&state));
        if state != LATCH_WAIT {
            return Some(state == LATCH_READY);
        }
        return None;
    }


    #[inline]
    pub unsafe fn set(this: *const Self, ok: bool) {
        let s = if ok { LATCH_READY } else { LATCH_PANIC };

        let this = unsafe { &*this };
        let sleeper = this.sleeper;

        let prev = this.state.swap(s, Ordering::AcqRel);
        debug_assert!([LATCH_WAIT, LATCH_SLEEP].contains(&prev));

        // `this` may now be dangling (if the thread wasn't sleeping or was just
        // woken up by someone else and has already deallocated a stack task
        // containing this latch),
        // don't access it!

        if prev == LATCH_SLEEP {
            // note: the sleeper must still be valid.
            unsafe { Sleeper::wake_unprotected(sleeper) };
        }
    }
}


pub(crate) struct StackTask<'a, R, F: FnOnce(&Worker, bool) -> R + Send> {
    f: UnsafeCell<ManuallyDrop<F>>,
    result: StackTaskResult<'a, R>,
}

struct StackTaskResult<'a, R> {
    latch: Latch<'a>,
    value: UnsafeCell<MaybeUninit<R>>,
}

pub(crate) struct StackTaskHandle<'me, 'a, R> {
    result: &'me StackTaskResult<'a, R>,
}

impl<'a, R, F: FnOnce(&Worker, bool) -> R + Send> StackTask<'a, R, F> {
    #[inline]
    pub fn new_on_worker(worker: &'a Worker, f: F) -> Self {
        Self::with_sleeper(&worker.sleeper, f)
    }

    #[inline]
    fn with_sleeper(sleeper: &'a Sleeper, f: F) -> Self {
        Self {
            f: UnsafeCell::new(ManuallyDrop::new(f)),
            result: StackTaskResult {
                latch: Latch::with_sleeper(sleeper),
                value: UnsafeCell::new(MaybeUninit::uninit()),
            },
        }
    }

    #[inline]
    pub unsafe fn create_task(&self) -> Task {
        Task {
            ptr: NonNull::from(self).cast(),
            call: |ptr| unsafe {
                let this = ptr.cast::<Self>().as_ref();

                // note: this load synchronizes with the store in `Latch::set`
                // at the end of the function. well kinda.
                // this happens when a stack task is created at the same address
                // quickly after another a stack task there completed there.
                // this makes miri shut up about data races on the retag of `f`
                // below. not sure this is actually necessary.
                let s = this.result.latch.state.load(Ordering::Acquire);
                debug_assert!([LATCH_WAIT, LATCH_SLEEP].contains(&s));

                let f = ManuallyDrop::take(&mut *this.f.get());

                let worker = Worker::current();
                let stolen = !core::ptr::eq(this.result.latch.sleeper, &worker.sleeper);

                // @panic
                let result = f(worker, stolen);

                this.result.value.get().write(MaybeUninit::new(result));
                Latch::set(&this.result.latch, true);
            },
        }
    }

    #[inline]
    pub fn handle(&self) -> StackTaskHandle<'_, 'a, R> {
        StackTaskHandle { result: &self.result }
    }
}

impl<'me, 'a, R> StackTaskHandle<'me, 'a, R> {
    #[inline]
    pub unsafe fn join(&self) -> Option<R> {
        let ok = self.result.latch.wait();
        ok.then(|| unsafe { self.result.value.get().read().assume_init() })
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

        let rt = Runtime::get_or_init();
        if rt.has_terminator.swap(true, Ordering::SeqCst) {
            panic!("multiple terminators");
        }

        Terminator { _unsend: PhantomData }
    }
}

impl Drop for Terminator {
    fn drop(&mut self) {
        let rt = Runtime::expect();
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



