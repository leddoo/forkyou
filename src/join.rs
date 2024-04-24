use sti::mem::{NonNull, MaybeUninit, ManuallyDrop};
use sti::sync::atomic::{AtomicU8, Ordering};

use crate::{Runtime, Task};


#[inline]
pub fn join<R1, F1, R2, F2>(f1: F1, f2: F2) -> (R1, R2)
where R1: Send, F1: FnOnce() -> R1 + Send,
      R2: Send, F2: FnOnce() -> R2 + Send
{
    Runtime::on_worker(move || join_on_worker(f1, f2))
}

pub fn join_on_worker<R1, F1, R2, F2>(f1: F1, f2: F2) -> (R1, R2)
where F1: FnOnce() -> R1 + Send,
      F2: FnOnce() -> R2 + Send
{
    const STATE_WAIT: u8 = 0;
    const STATE_DONE: u8 = 1;
    const STATE_PANIC: u8 = 2;

    let state = AtomicU8::new(STATE_WAIT);


    struct StackTask<'a, R2, F2> {
        f2: ManuallyDrop<F2>,
        result: MaybeUninit<R2>,
        state: &'a AtomicU8,
    }

    let mut task = StackTask {
        f2: ManuallyDrop::new(f2),
        result: <MaybeUninit<R2>>::uninit(),
        state: &state,
    };

    let ptr = NonNull::from(&mut task).cast();

    let call = |ptr: NonNull<u8>| {
        let task = unsafe { ptr.cast::<StackTask<R2, F2>>().as_mut() };
        let f2 = unsafe { ManuallyDrop::take(&mut task.f2) };
        // @panic
        let result = f2();
        task.result = MaybeUninit::new(result);
        task.state.store(STATE_DONE, Ordering::Release);
    };

    Runtime::submit_task_on_worker(unsafe { Task::new(ptr, call) });


    // @panic.
    let r1 = f1();

    Runtime::work_while(|| state.load(Ordering::Acquire) == STATE_WAIT);

    let state = state.load(Ordering::Acquire);
    if state != STATE_DONE {
        todo!()
    }

    let r2 = unsafe { task.result.assume_init() };

    return (r1, r2);
}


#[cfg(test)]
mod tests {
    use crate::join;

    #[test]
    fn sum() {
        let _t = crate::Terminator::new();

        fn sum(values: &[i32]) -> i32 {
            if values.len() > 3 {
                let m1 = values.len() / 3;
                let m2 = 2*m1;

                let (l, (m, r)) = join(
                    move || sum(&values[..m1]),
                    move || join(
                        move || sum(&values[m1..m2]),
                        move || sum(&values[m2..])));

                return l + m + r;
            }
            else {
                values.iter().sum()
            }
        }

        let values = Vec::from_iter(0..100);
        let result = sum(&values);
        assert_eq!(result, 99*100/2);
    }
}


