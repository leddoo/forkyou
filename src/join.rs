use crate::Runtime;
use crate::runtime::StackTask;


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
    let f2 = StackTask::new_on_worker(f2);
    Runtime::submit_task_on_worker(unsafe { f2.create_task() });

    // @panic.
    let r1 = f1();

    let r2 = unsafe { f2.handle().join().expect("todo") };

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


