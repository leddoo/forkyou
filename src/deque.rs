use sti::boks::Box;
use sti::sync::rw_spin_lock::RwSpinLock;
use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicUsize, Ordering};



// impl of "Dynamic Circular Work-Stealing Deque" by Chase and Lev, SPAA 2005
pub struct Deque<T: Send> {
    begin: AtomicUsize,
    end: AtomicUsize,
    buffer: RwSpinLock<Buffer<T>>,
}

impl<T: Send> Deque<T> {
    pub fn new() -> Self {
        Self {
            begin: AtomicUsize::new(0),
            end: AtomicUsize::new(0),
            buffer: RwSpinLock::new(Buffer::new()),
        }
    }

    #[inline(always)]
    fn queue_len(begin: usize, end: usize) -> isize {
        (end as isize).wrapping_sub(begin as isize)
    }


    #[inline]
    pub fn len(&self) -> usize {
        let begin = self.begin.load(Ordering::Acquire);
        let end   = self.end.load(Ordering::Acquire);
        Self::queue_len(begin, end).max(0) as usize
    }

    pub unsafe fn push(&self, value: T) -> bool {
        let begin = self.begin.load(Ordering::Acquire);
        let end   = self.end.load(Ordering::Acquire);

        let mut buffer = self.buffer.read();

        let len = Self::queue_len(begin, end);
        if len >= buffer.cap() as isize {
            debug_assert_eq!(len, buffer.cap() as isize);

            // can't wrap cause cap <= isize::MAX.
            let new_cap = buffer.cap()*2;
            let new_buffer = buffer.resize(new_cap, begin, end);

            // replace buffer, holding lock only for the swap.
            drop(buffer);
            let mut lock = self.buffer.write();
            let old_buffer = core::mem::replace(&mut *lock, new_buffer);
            drop(lock);
            drop(old_buffer);

            buffer = self.buffer.read();
        }

        unsafe { buffer.at(end).write(MaybeUninit::new(value)) }
        drop(buffer);

        self.end.store(end.wrapping_add(1), Ordering::Release);

        return len > 0;
    }

    pub unsafe fn pop(&self) -> Option<T> {
        let end = self.end.fetch_sub(1, Ordering::AcqRel);
        let begin = self.begin.load(Ordering::Acquire);

        let len = Self::queue_len(begin, end);
        if len <= 0 {
            self.end.store(begin, Ordering::Release);
            return None;
        }

        let buffer = self.buffer.read();
        let result = unsafe { buffer.at(end.wrapping_sub(1)).read() };
        drop(buffer);

        if len > 1 {
            return unsafe { Some(result.assume_init()) };
        }

        let result = match
            self.begin.compare_exchange(
                begin, begin.wrapping_add(1),
                Ordering::SeqCst, Ordering::Acquire)
        {
            Ok(_) => unsafe { Some(result.assume_init()) },
            Err(_) => None,
        };

        self.end.store(begin.wrapping_add(1), Ordering::Release);
        return result;
    }

    pub fn steal(&self) -> Result<T, bool> {
        let begin = self.begin.load(Ordering::Acquire);
        let end = self.end.load(Ordering::Acquire);

        let len = Self::queue_len(begin, end);
        if len <= 0 {
            return Err(true);
        }

        let buffer = self.buffer.read();
        let result = unsafe { buffer.at(begin).read() };
        drop(buffer);

        if let Err(_) = self.begin.compare_exchange(
            begin, begin.wrapping_add(1),
            Ordering::SeqCst, Ordering::Relaxed)
        {
            return Err(false);
        }

        return unsafe { Ok(result.assume_init()) };
    }
}

struct Buffer<T> {
    inner: Box<[UnsafeCell<MaybeUninit<T>>]>,
}

const MIN_CAP: usize = 16;

impl<T> Buffer<T> {
    fn new() -> Self { unsafe {
        use core::ptr::NonNull;
        // @todo: box slice from fn.
        let slice = core::slice::from_raw_parts_mut(NonNull::dangling().as_ptr(), 0);
        Self { inner: Box::from_raw_parts(NonNull::from(slice)) }
    }}

    #[inline(always)]
    fn cap(&self) -> usize {
        self.inner.len()
    }

    #[inline(always)]
    fn at(&self, i: usize) -> *mut MaybeUninit<T> { unsafe {
        self.inner.get_unchecked(i & (self.cap() - 1)).get()
    }}

    fn resize(&self, new_cap: usize, begin: usize, end: usize) -> Self {
        let new_cap = new_cap.max(MIN_CAP);
        assert!(new_cap.is_power_of_two());

        let new_buffer = unsafe {
            use sti::alloc::GlobalAlloc;
            use core::ptr::NonNull;
            let ptr = sti::alloc::alloc_array(&GlobalAlloc, new_cap).expect("oom");
            let slice = core::slice::from_raw_parts_mut(ptr.as_ptr(), new_cap);
            Self { inner: Box::from_raw_parts(NonNull::from(slice)) }
        };

        let mut i = begin;
        while i != end { unsafe {
            new_buffer.at(i).write(self.at(i).read());
            i = i.wrapping_add(1);
        }}

        return new_buffer;
    }
}


#[cfg(test)]
mod tests {
    use super::Deque;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    #[test]
    fn deque_basic() {
        let state = Arc::new((AtomicU32::new(0), Deque::new()));

        const NUM_THREADS: usize = 24;

        fn stealer(count: &AtomicU32, deque: &Deque<u32>) {
            loop {
                let Ok(v) = deque.steal() else { continue };
                if v == u32::MAX {
                    break;
                }

                count.fetch_add(v, Ordering::Relaxed);
            }
        }

        let mut handles = Vec::new();
        for _ in 0..NUM_THREADS {
            handles.push(std::thread::spawn(sti::enclose!(state; move ||
                stealer(&state.0, &state.1))));
        }

        for i in 0..100 {
            unsafe { state.1.push(i) };
        }
        for _ in 0..NUM_THREADS {
            unsafe { state.1.push(u32::MAX) };
        }

        for handle in handles {
            handle.join().unwrap();
        }

        let result = state.0.load(Ordering::Relaxed);
        assert_eq!(result, 99*100/2);
    }

    #[test]
    fn deque_growing() {
        let state = Arc::new((AtomicU32::new(0), Deque::new()));

        fn stealer(count: &AtomicU32, deque: &Deque<u32>) {
            loop {
                let Ok(v) = deque.steal() else { continue };
                //print!("steal {v}\n");
                if v == u32::MAX {
                    break;
                }

                count.fetch_add(v, Ordering::Relaxed);

                let n = if cfg!(miri) { 5 } else { 100 };
                for i in 0..n {
                    core::hint::black_box(i);
                }
            }
        }

        let handle = std::thread::spawn(sti::enclose!(state; move ||
            stealer(&state.0, &state.1)));

        for i in 0..1000 {
            unsafe { state.1.push(i) };
        }

        let mut delta = 0;
        while let Some(v) = unsafe { state.1.pop() } {
            //print!("pop {v}\n");
            delta += v;
        }
        state.0.fetch_add(delta, Ordering::Relaxed);

        unsafe { state.1.push(u32::MAX) };

        handle.join().unwrap();

        assert!(state.1.buffer.read().cap() > 4*super::MIN_CAP);

        let result = state.0.load(Ordering::Relaxed);
        assert_eq!(result, 999*1000/2);
    }

    #[test]
    fn deque_pop_race() {
        let state = Arc::new((AtomicBool::new(false), AtomicU32::new(0), Deque::new()));

        fn stealer(done: &AtomicBool, total: &AtomicU32, deque: &Deque<u32>) {
            let mut race_wins: u64 = 0;
            let mut race_losses: u64 = 0;
            let mut race_slows: u64 = 0;

            while race_wins < 10
            || race_losses < 10
            || race_slows < 10 {
                match deque.steal() {
                    Ok(v) => {
                        //print!("steal {v}\n");
                        total.fetch_add(v, Ordering::Relaxed);
                        race_wins += 1;
                    }
                    Err(false) => {
                        race_losses += 1;
                    }

                    Err(true) => {
                        race_slows += 1;
                    }
                }
            }

            dbg!(race_wins, race_losses, race_slows);

            done.store(true, Ordering::Relaxed);
        }

        let handle = std::thread::spawn(sti::enclose!(state; move ||
            stealer(&state.0, &state.1, &state.2)));

        let mut v = 0;
        while !state.0.load(Ordering::Relaxed) {
            unsafe { state.2.push(v) };

            // slightly delay the pop to give the stealer more time.
            // the test only terminates when races were detected, so it's fine.
            for i in 0..2 {
                core::hint::black_box(i);
            }

            if let Some(v) = unsafe { state.2.pop() } {
                //print!("pop {v}\n");
                state.1.fetch_add(v, Ordering::Relaxed);
            }

            v += 1;
        }

        while let Some(v) = unsafe { state.2.pop() } {
            state.1.fetch_add(v, Ordering::Relaxed);
        }

        handle.join().unwrap();

        // the formula doesn't seem to be correct when wrapping.
        let mut expected = 0u32;
        for i in 0..v {
            expected = expected.wrapping_add(i);
        }

        let result = state.1.load(Ordering::Relaxed);
        assert_eq!(result, expected);
    }
}

