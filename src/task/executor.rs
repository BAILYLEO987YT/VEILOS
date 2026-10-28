use crate::task::{Task, TaskId};
use alloc::{collections::BTreeMap, sync::Arc, task::Wake};
use core::task::{Context, Poll, Waker};
use crossbeam_queue::ArrayQueue;

/// A preemptive, event-driven task scheduler. 
/// Instead of busy-polling tasks (100% CPU usage), this Executor puts the CPU 
/// to sleep (`hlt`) until a hardware interrupt sends a "Wake" signal to a specific task.
pub struct Executor {
    tasks: BTreeMap<TaskId, Task>,
    task_queue: Arc<ArrayQueue<TaskId>>,
    waker_cache: BTreeMap<TaskId, Waker>,
}

impl Executor {
    pub fn new() -> Self {
        Executor {
            tasks: BTreeMap::new(),
            task_queue: Arc::new(ArrayQueue::new(512)), // 512 concurrent tasks
            waker_cache: BTreeMap::new(),
        }
    }

    /// Spawns a new task into the executor.
    pub fn spawn(&mut self, task: Task) {
        let task_id = task.id;
        if self.tasks.insert(task.id, task).is_some() {
            panic!("Task with same ID already exists!");
        }
        self.task_queue.push(task_id).expect("Task queue is full");
    }

    /// Starts the infinite execution loop.
    pub fn run(&mut self) -> ! {
        loop {
            self.run_ready_tasks();
            self.sleep_if_idle();
        }
    }

    /// Process all tasks that the hardware/kernel has flagged as "Ready".
    fn run_ready_tasks(&mut self) {
        // Drain any fibers queued by VNR's spawn_fiber() or other external callers.
        let pending: alloc::vec::Vec<_> = crate::task::PENDING_SPAWNS.lock().drain(..).collect();
        for task in pending { self.spawn(task); }

        // We extract the task ID from the Waker queue
        while let Some(task_id) = self.task_queue.pop() {
            let mut task = match self.tasks.remove(&task_id) {
                Some(task) => task,
                None => continue, // Task no longer exists
            };

            // Grab the specialized Waker for this specific TaskId
            let waker = self.waker_cache
                .entry(task_id)
                .or_insert_with(|| TaskWaker::new(task_id, self.task_queue.clone()));
                
            let mut context = Context::from_waker(waker);

            // Execute the task until it voluntarily yields (Pending) or finishes (Ready)
            match task.poll(&mut context) {
                Poll::Ready(()) => {
                    // Task is finished. Remove its waker from the cache and let it drop.
                    self.waker_cache.remove(&task_id);
                }
                Poll::Pending => {
                    // Task is not finished but has yielded. Put it back in the task map.
                    // It will NOT be executed again until a hardware interrupt calls 
                    // this task's specific Waker to put it back into the `task_queue`.
                    if self.tasks.insert(task_id, task).is_some() {
                        panic!("Task with same ID already exists!");
                    }
                }
            }
        }
    }

    /// Uses the CPU's `hlt` (Halt) instruction to put exactly this thread/vCPU to
    /// sleep until the next hardware interrupt fires, reducing CPU usage to 0%.
    fn sleep_if_idle(&self) {
        use x86_64::instructions::interrupts;
        
        // We must check if the queue is empty AND halt as a single atomic operation.
        // If we didn't disable interrupts first, an interrupt could sneak in right
        // after `is_empty()`, wake a task, and then we'd halt indefinitely!
        interrupts::disable();
        if self.task_queue.is_empty() {
            let t0 = unsafe { core::arch::x86_64::_rdtsc() };
            interrupts::enable_and_hlt();
            crate::shell::note_idle(unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(t0));
        } else {
            interrupts::enable();
        }
    }
}

// -----| The Custom Waker |-----

/// A custom Waker that pushes the associated TaskId back into the execution queue 
/// so the Executor knows to poll it again on the next tick.
struct TaskWaker {
    task_id: TaskId,
    task_queue: Arc<ArrayQueue<TaskId>>,
}

impl TaskWaker {
    fn new(task_id: TaskId, task_queue: Arc<ArrayQueue<TaskId>>) -> Waker {
        Waker::from(Arc::new(TaskWaker {
            task_id,
            task_queue,
        }))
    }

    fn wake_task(&self) {
        self.task_queue.push(self.task_id).expect("Task queue is full");
    }
}

// Implement the standard library's Wake trait so Rust knows how to use this Waker
impl Wake for TaskWaker {
    fn wake(self: Arc<Self>) {
        self.wake_task();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.wake_task();
    }
}
