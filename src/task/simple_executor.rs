use super::Task;
use alloc::collections::VecDeque;
use core::task::{Waker, RawWaker, RawWakerVTable, Context, Poll};

/// A basic First-In-First-Out (FIFO) Task Scheduler.
pub struct SimpleExecutor {
    task_queue: VecDeque<Task>,
}

impl SimpleExecutor {
    pub fn new() -> SimpleExecutor {
        SimpleExecutor {
            task_queue: VecDeque::new(),
        }
    }

    /// Add a new task to the back of the queue.
    pub fn spawn(&mut self, task: Task) {
        self.task_queue.push_back(task)
    }

    /// Consume the queue, running tasks cooperatively until all are fully completed.
    pub fn run(&mut self) {
        while let Some(mut task) = self.task_queue.pop_front() {
            let waker = dummy_waker();
            let mut context = Context::from_waker(&waker);
            
            match task.poll(&mut context) {
                Poll::Ready(()) => {
                    // Task is completely finished, let it drop and clear from memory!
                }
                Poll::Pending => {
                    // Task yielded (paused itself). Push it to the back of the line so 
                    // other tasks get a turn, and we'll resume it later.
                    self.task_queue.push_back(task);
                }
            }
        }
    }
}

// Rust's Future polling requires a Waker. Since our SimpleExecutor doesn't 
// yet support hardware-interrupt-based wakeups, we provide a dummy waker 
// that satisfies the compiler without doing anything complex.
fn dummy_raw_waker() -> RawWaker {
    fn no_op(_: *const ()) {}
    fn clone(_: *const ()) -> RawWaker {
        dummy_raw_waker()
    }

    let vtable = &RawWakerVTable::new(clone, no_op, no_op, no_op);
    RawWaker::new(core::ptr::null(), vtable)
}

fn dummy_waker() -> Waker {
    unsafe { Waker::from_raw(dummy_raw_waker()) }
}
