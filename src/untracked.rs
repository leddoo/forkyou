use sti::alloc::GlobalAlloc;
use sti::boks::Box;
use crate::{Runtime, Task, box_into_inner};


pub fn spawn_untracked<F: FnOnce() + Send + 'static>(f: F) {
    let task = Task {
        ptr: Box::new(f).into_raw_parts().0.cast(),
        call: |ptr| {
            let f = unsafe { Box::from_raw_parts(ptr.cast(), GlobalAlloc) };
            // @temp
            let f: F = box_into_inner(f); //f.into_inner();
            // @panic.
            f();
        },
    };
    Runtime::submit_task(task);
}


#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    //use sti::vec::Vec;
    //use core::sync::atomic::{Ordering, AtomicU32};


    #[test]
    fn untracked() {
        let _t = crate::Terminator::new();

        let result = Arc::new(Mutex::new(None));

        crate::spawn_untracked(sti::enclose!(result; move || {
            let mut lock = result.lock().unwrap();
            assert!(lock.is_none());
            *lock = Some(42);
        }));

        let result = loop {
            let lock = result.lock().unwrap();
            if let Some(result) = *lock {
                break result;
            }
        };
        assert_eq!(result, 42);
    }
}

