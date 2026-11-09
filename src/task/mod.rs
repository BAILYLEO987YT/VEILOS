pub mod simple_executor;
pub mod executor;
pub mod keyboard;
pub mod mouse;
pub mod terminal;

use core::{future::Future, pin::Pin};
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::task::{Context, Poll};
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

/// Global queue for fibers spawned outside the executor's run() loop
/// (e.g. from VNR's spawn_fiber()). The executor drains this queue at
/// the start of each run_ready_tasks() pass.
pub static PENDING_SPAWNS: Mutex<Vec<Task>> = Mutex::new(Vec::new());

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct TaskId(u64);

impl TaskId {
    fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        TaskId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }
}

/// A Task is a wrapper over any asynchronous block of code (Future).
/// It assigns a unique ID to the code block and pins it in memory
/// so that the CPU can safely execute it piecemeal without data moving.
pub struct Task {
    id: TaskId,
    future: Pin<Box<dyn Future<Output = ()>>>,
}

// SAFETY: VEILOS is a single-core bare-metal kernel. Tasks are never
// actually moved between threads — this impl exists solely so that
// Vec<Task> satisfies the Sync bound required by a static Mutex.
unsafe impl Send for Task {}

impl Task {
    pub fn new(future: impl Future<Output = ()> + 'static) -> Task {
        Task {
            id: TaskId::new(),
            future: Box::pin(future), // This allocates the task's state on our new Heap!
        }
    }

    /// Progresses the task forward until it yields or completes.
    fn poll(&mut self, context: &mut Context) -> Poll<()> {
        self.future.as_mut().poll(context)
    }
}

// -----| Utilities |-----

/// An awaitable future that yields execution back to the executor once.
/// This allows other tasks to run before this context resumes, 
/// creating cooperative multitasking!
pub struct YieldNow {
    yielded: bool,
}

impl Future for YieldNow {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<()> {
        if self.yielded {
            Poll::Ready(())
        } else {
            self.yielded = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

/// Call this function using `task::yield_now().await` to pause the current async 
/// function and let the CPU run something else for a moment.
pub fn yield_now() -> YieldNow {
    YieldNow { yielded: false }
}
