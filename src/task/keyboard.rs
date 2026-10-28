// Removed unused import
use conquer_once::spin::OnceCell;
use crossbeam_queue::ArrayQueue;
use core::{pin::Pin, task::{Poll, Context}};
use futures_util::stream::{Stream, StreamExt};
use futures_util::task::AtomicWaker;
use pc_keyboard::{layouts, DecodedKey, HandleControl, Keyboard, ScancodeSet1};

// Create a globally accessible lock-free queue that the Hardware Interrupt 
// can stuff keyboard scancodes into without blocking the CPU or causing deadlocks.
static SCANCODE_QUEUE: OnceCell<ArrayQueue<u8>> = OnceCell::uninit();
// Waker that the Hardware Interrupt triggers to wake up our Keyboard Task
static WAKER: AtomicWaker = AtomicWaker::new();

/// Called by the `main.rs` initialization routine to quickly set up the Scancode Queue.
pub(crate) fn initialize() {
    SCANCODE_QUEUE.try_init_once(|| ArrayQueue::new(100))
        .expect("Keyboard Scancode_Queue already initialized");
}

/// Called ONLY by the specific Hardware Interrupt Handler in `interrupts.rs`.
/// Pushes the raw scancode byte into the queue and instantly wakes the Keyboard Task.
pub(crate) fn add_scancode(scancode: u8) {
    if let Ok(queue) = SCANCODE_QUEUE.try_get() {
        if queue.push(scancode).is_err() {
            // Use serial_println! — println! locks VGA_WRITER which may already be
            // held by interrupted code, causing a spinlock deadlock in the ISR.
            crate::serial_println!("WARNING: Keyboard queue is full; dropping keystrokes.");
        } else {
            // Wake up the Keyboard Task! (This removes the CPU from the `hlt` state!)
            WAKER.wake();
        }
    } else {
        crate::serial_println!("WARNING: Keyboard Scancode_Queue uninitialized");
    }
}

// -----| Game SDK low-latency poll |-----

/// Pop one raw scancode from the queue without blocking.
/// Returns None if the queue is empty.
/// Used by InputSnapshot::capture() in the Game SDK.
pub fn pop_scancode() -> Option<u8> {
    SCANCODE_QUEUE.try_get().ok()?.pop()
}

// -----| The Asynchronous Scancode Stream |-----

pub struct ScancodeStream {
    _private: (),
}

impl ScancodeStream {
    pub fn new() -> Self {
        SCANCODE_QUEUE.try_get().expect("Keyboard Scancode_Queue not initialized");
        ScancodeStream { _private: () }
    }
}

impl Stream for ScancodeStream {
    type Item = u8;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<u8>> {
        let queue = SCANCODE_QUEUE.try_get().expect("Scancode_Queue not initialized");

        // Fast path: if there is a scancode waiting, grab it immediately!
        if let Some(scancode) = queue.pop() {
            return Poll::Ready(Some(scancode));
        }

        // If the queue is empty, register the Executor's Waker with our AtomicWaker.
        // When the Hardware Interrupt fires next time, it will call this exactly!
        WAKER.register(&cx.waker());
        
        // Check the queue one last time in case an interrupt snuck in while we were registering
        match queue.pop() {
            Some(scancode) => {
                WAKER.take(); // Unregister the waker since we didn't need it
                Poll::Ready(Some(scancode))
            }
            None => Poll::Pending, // The queue is truly empty. Put this task to sleep.
        }
    }
}

// -----| The Main Keyboard Application Task |-----

pub async fn print_keypresses() {
    let mut scancodes = ScancodeStream::new();
    let mut keyboard = Keyboard::new(
        ScancodeSet1::new(),
        layouts::Us104Key,
        HandleControl::Ignore,
    );

    // This loop essentially runs "forever", but because we are fetching `.next().await`,
    // it yields the CPU immediately every time the list of pressed keys runs dry!
    while let Some(scancode) = scancodes.next().await {
        if let Ok(Some(key_event)) = keyboard.add_byte(scancode) {
            if let Some(key) = keyboard.process_keyevent(key_event) {
                match key {
                    DecodedKey::Unicode(character) => crate::print!("{}", character),
                    DecodedKey::RawKey(key) => crate::print!("{:?}", key),
                }
            }
        }
    }
}
