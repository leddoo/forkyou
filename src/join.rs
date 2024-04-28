use crate::Runtime;
use crate::runtime::{Worker, StackTask};


#[inline]
pub fn join<R1, F1, R2, F2>(f1: F1, f2: F2) -> (R1, R2)
where R1: Send, F1: FnOnce() -> R1 + Send,
      R2: Send, F2: FnOnce() -> R2 + Send
{
    Runtime::on_worker(move |worker, _|
        join_on_worker(worker,
            move |_, _| f1(),
            move |_, _| f2()))
}

pub(crate) fn join_on_worker<R1, F1, R2, F2>(worker: &Worker, f1: F1, f2: F2) -> (R1, R2)
where F1: FnOnce(&Worker, bool) -> R1 + Send,
      F2: FnOnce(&Worker, bool) -> R2 + Send
{
    let f2 = StackTask::new_on_worker(worker, f2);
    worker.submit_task(unsafe { f2.create_task() });

    // @panic.
    let r1 = f1(worker, false);

    let r2 = unsafe { f2.handle().join().expect("todo") };

    return (r1, r2);
}


#[derive(Clone, Copy)]
pub(crate) struct Splitter {
    num_tasks: usize,
}

impl Splitter {
    #[inline]
    pub fn new(num_tasks: usize) -> Self {
        Self { num_tasks }
    }

    #[inline]
    pub fn try_split(&mut self, len: usize, stolen: Option<usize>) -> bool {
        if len < 2 {
            return false;
        }

        if let Some(num_tasks) = stolen {
            self.num_tasks = num_tasks;
            return true;
        }

        if self.num_tasks >= 2 {
            self.num_tasks /= 2;
            return true;
        }

        return false;
    }
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


