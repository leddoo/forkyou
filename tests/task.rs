use std::sync::{Arc, Mutex};
use sti::sync::spin_lock::SpinLock;


#[test]
fn untracked() {
    let result = Arc::new(Mutex::new(None));

    forkyou::spawn_untracked(sti::enclose!(result; move || {
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


#[test]
fn scoped() {
    let the_result = 42;
    let result = forkyou::scope(|scope| {
        let result = &*scope.alloc().alloc_new(SpinLock::new(None));

        scope.spawn(|| {
            *result.lock() = Some(the_result);
        }).join();

        scope.spawn(|| {
            return result.lock().unwrap();
        }).join()
    });
    assert_eq!(result, 42);

    /*
    let mut foo = None;
    forkyou::scope(|scope| {
        foo = Some(scope.alloc().alloc_new(42));
    });
    */
}

