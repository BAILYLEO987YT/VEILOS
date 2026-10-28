use conquer_once::spin::Lazy;
use crossbeam_queue::ArrayQueue;
use x86_64::instructions::port::Port;
use spin::Mutex;
use core::sync::atomic::{AtomicI32, Ordering};

static MOUSE_BYTE_QUEUE: Lazy<ArrayQueue<u8>> = Lazy::new(|| ArrayQueue::new(256));

pub struct MouseState {
    pub x: i32,
    pub y: i32,
    pub left_button:  bool,
    pub right_button: bool,
    /// Accumulated delta since last get_state() call.
    pub dx: i32,
    pub dy: i32,
}

pub static MOUSE_STATE: Mutex<MouseState> = Mutex::new(MouseState {
    x: 512, // Start in center
    y: 384,
    left_button:  false,
    right_button: false,
    dx: 0,
    dy: 0,
});

pub fn add_byte(byte: u8) {
    if let Err(_) = MOUSE_BYTE_QUEUE.push(byte) {
        // Queue full, ignore
    }
}

// -----| ISR-safe immediate packet processing |-----
//
// These three statics hold the in-progress PS/2 packet for the ISR path.
// They are only ever written from the mouse ISR (single CPU, interrupts
// disabled during ISR), so no locking is needed.
/// Movement from packets that arrived while MOUSE_STATE was locked by the interrupted code.
/// Folded into the next packet that does get the lock, so no movement is lost.
static PENDING_DX: AtomicI32 = AtomicI32::new(0);
static PENDING_DY: AtomicI32 = AtomicI32::new(0);
static mut ISR_PACKET: [u8; 3] = [0u8; 3];
static mut ISR_IDX: usize = 0;

/// Called directly from the mouse ISR instead of `add_byte`.
///
/// Feeds the byte into MOUSE_BYTE_QUEUE (so the async task still works when
/// the executor is alive) AND assembles + processes the packet inline so
/// MOUSE_STATE is updated immediately — even when the async executor is
/// permanently frozen after `iretq` launches a user process.
pub fn add_byte_and_process(byte: u8) {
    // NOTE: the byte is deliberately NOT also pushed to MOUSE_BYTE_QUEUE. `mouse_tick` pops that
    // queue and runs `process_packet`, which applies the same dx/dy to MOUSE_STATE a second time:
    // every movement used to be counted twice (cursor travelled 2x the real pointer distance).
    // This ISR path is the single place that applies motion and buttons.
    unsafe {
        // Packet synchronisation: bit 3 of the first byte is always set.
        // If we're at index 0 and it isn't, we're out of sync — drop and wait.
        if ISR_IDX == 0 && (byte & 0x08) == 0 { return; }

        ISR_PACKET[ISR_IDX] = byte;
        ISR_IDX += 1;
        if ISR_IDX < 3 { return; } // Packet not yet complete.
        ISR_IDX = 0;               // Reset for next packet.

        let flags = ISR_PACKET[0];
        let mut dx = ISR_PACKET[1] as i32;
        let mut dy = ISR_PACKET[2] as i32;

        // PS/2 uses 9-bit signed values — sign-extend if the overflow bits are set.
        if (flags & 0x10) != 0 { dx -= 256; }
        if (flags & 0x20) != 0 { dy -= 256; }

        // try_lock: non-blocking. If the async task holds the lock right now
        // we skip this packet rather than spinning inside an ISR. The next
        // packet (a few milliseconds later) will catch up on position.
        if let Some(mut state) = MOUSE_STATE.try_lock() {
            // Fold in movement from any packets that arrived while the lock was busy.
            let dx = dx + PENDING_DX.swap(0, Ordering::Relaxed);
            let dy = dy + PENDING_DY.swap(0, Ordering::Relaxed);
            state.x = (state.x + dx).clamp(0, 1023);
            state.y = (state.y - dy).clamp(0, 767); // PS/2 Y is inverted
            state.left_button  = (flags & 0x01) != 0;
            state.right_button = (flags & 0x02) != 0;
        } else {
            // Lock busy: remember the movement instead of dropping it.
            PENDING_DX.fetch_add(dx, Ordering::Relaxed);
            PENDING_DY.fetch_add(dy, Ordering::Relaxed);
        }
    }
}

// -----| PS/2 low-level helpers |-----

/// Spins until the PS/2 controller input buffer is empty (safe to write).
/// Returns false on timeout — caller logs and continues rather than hanging.
fn wait_input() -> bool {
    let mut port = Port::<u8>::new(0x64);
    let mut timeout = 0x100000;
    unsafe {
        while (port.read() & 0x02) != 0 && timeout > 0 {
            core::hint::spin_loop();
            timeout -= 1;
        }
    }
    timeout > 0
}

/// Spins until the PS/2 controller output buffer has data ready to read.
/// Returns false on timeout.
fn wait_output() -> bool {
    let mut port = Port::<u8>::new(0x64);
    let mut timeout = 0x100000;
    unsafe {
        while (port.read() & 0x01) == 0 && timeout > 0 {
            core::hint::spin_loop();
            timeout -= 1;
        }
    }
    timeout > 0
}

/// Sends a byte to the mouse (PS/2 second port).
fn mouse_write(data: u8) {
    let mut command_port = Port::<u8>::new(0x64);
    let mut data_port    = Port::<u8>::new(0x60);
    unsafe {
        if wait_input() {
            command_port.write(0xD4); // Route next byte to second PS/2 port
            if wait_input() {
                data_port.write(data);
            }
        }
    }
}

/// Reads one byte from the PS/2 output buffer with a timeout.
/// Logs the result (or a timeout warning) so every step is visible on serial.
fn mouse_read_logged(label: &str) -> u8 {
    if wait_output() {
        let mut data_port = Port::<u8>::new(0x60);
        let val = unsafe { data_port.read() };
        crate::serial_println!("[MOUSE] {} = {:#04X}", label, val);
        val
    } else {
        crate::serial_println!("[MOUSE] TIMEOUT waiting for {}", label);
        0x00
    }
}

// -----| async hardware initialization |-----
//
// Previously this was a synchronous `initialize()` called from kernel_main
// before executor.run().  Each mouse_read() could spin for up to ~1M iterations
// and there were 8+ such calls in sequence — enough to hang the boot visibly.
//
// Now the entire PS/2 setup runs inside mouse_tick() at the top of the async
// task, before the packet loop.  The executor is already running, so:
//   * Other tasks (gui_refresh_task, focus_manager) keep ticking normally.
//   * SleepFuture yields between the heavy steps so the timer ISR stays fed.
//   * If the PS/2 controller never responds, we log a warning and move on
//     instead of silently blocking kernel_main forever.

async fn hw_init() {
    crate::serial_println!("[MOUSE] Async PS/2 init starting (executor is live).");

    let mut command_port = Port::<u8>::new(0x64);
    let mut data_port    = Port::<u8>::new(0x60);

    // 0. Mask IRQ12 for the whole init. Firmware may have left it unmasked,
    //    and if the mouse ISR runs during init it reads port 0x60 itself and
    //    swallows every ACK/self-test byte this sequence is polling for.
    //    Interrupts must be off while PICS is held: the timer ISR takes the
    //    same lock for its EOI and would spin forever on this CPU.
    x86_64::instructions::interrupts::without_interrupts(|| unsafe {
        let mut pics = crate::interrupts::PICS.lock();
        let masks = pics.read_masks();
        pics.write_masks(masks[0], masks[1] | 0x10);
    });

    unsafe {
        // 1. Disable both PS/2 ports for a clean slate.
        crate::serial_println!("[MOUSE] Disabling PS/2 ports...");
        wait_input();
        command_port.write(0xAD); // Disable keyboard port
        wait_input();
        command_port.write(0xA7); // Disable mouse port

        // 2. Flush the output buffer (discard any stale bytes).
        while (command_port.read() & 0x01) != 0 {
            data_port.read();
        }
    }

    // Yield so gui_refresh_task gets a tick while we pause between steps.
    crate::interrupts::SleepFuture::new(5).await;

    let config = unsafe {
        // 3. Read, patch, and write the Controller Configuration Byte.
        crate::serial_println!("[MOUSE] Reading controller config...");
        wait_input();
        command_port.write(0x20); // Request config byte
        let config_raw = mouse_read_logged("config byte");
        // Enable IRQ1 (keyboard), clear both port-disable bits. The mouse
        // IRQ bit stays OFF until init is finished (step 9) so ACK bytes are
        // left in the output buffer for the polling reads below.
        let config = (config_raw | 0x01) & !0x32;

        wait_input();
        command_port.write(0x60); // Write config byte
        wait_input();
        data_port.write(config);
        crate::serial_println!("[MOUSE] Config written: {:#04X} -> {:#04X}", config_raw, config);

        // 4. Re-enable the mouse port.
        crate::serial_println!("[MOUSE] Enabling mouse port...");
        wait_input();
        command_port.write(0xA8);
        config
    };

    crate::interrupts::SleepFuture::new(5).await;

    // 5. Reset the mouse device and collect the three-byte response.
    crate::serial_println!("[MOUSE] Sending reset (0xFF)...");
    mouse_write(0xFF);

    // The mouse takes a few ms to self-test — yield instead of spinning.
    crate::interrupts::SleepFuture::new(10).await;

    let ack  = mouse_read_logged("reset ACK  (expect 0xFA)");
    let test = mouse_read_logged("self-test  (expect 0xAA)");
    let id   = mouse_read_logged("device ID  (expect 0x00)");

    if ack != 0xFA {
        crate::serial_println!("[MOUSE] WARN: unexpected reset ACK {:#04X} -- continuing anyway.", ack);
    }
    if test != 0xAA {
        crate::serial_println!("[MOUSE] WARN: self-test result {:#04X} (expected 0xAA) -- continuing anyway.", test);
    }
    let _ = id; // logged above; value is informational only

    crate::interrupts::SleepFuture::new(5).await;

    // 6. Set sample rate to 200 Hz for better precision.
    crate::serial_println!("[MOUSE] Setting sample rate 200...");
    mouse_write(0xF3);
    mouse_read_logged("sample-rate ACK");
    mouse_write(200);
    mouse_read_logged("sample-rate value ACK");

    // 7. Set resolution to 4 counts/mm.
    crate::serial_println!("[MOUSE] Setting resolution 4 counts/mm...");
    mouse_write(0xE8);
    mouse_read_logged("resolution ACK");
    mouse_write(0x03);
    mouse_read_logged("resolution value ACK");

    crate::interrupts::SleepFuture::new(5).await;

    // 8. Enable data reporting (start the packet stream).
    crate::serial_println!("[MOUSE] Enabling data reporting...");
    mouse_write(0xF4);
    mouse_read_logged("data-report ACK (expect 0xFA)");

    // 9. Unmask IRQ12 in the PIC now that the device is fully configured.
    //    Doing this last prevents spurious interrupts during init.
    unsafe {
        // Turn on the controller's mouse IRQ (config bit 1) first. Reuse the
        // byte from step 3 rather than re-reading it: with the keyboard IRQ
        // live, the keyboard ISR would consume the 0x20 response byte.
        let mut command_port = Port::<u8>::new(0x64);
        let mut data_port    = Port::<u8>::new(0x60);
        wait_input();
        command_port.write(0x60);
        wait_input();
        data_port.write(config | 0x02);
        crate::serial_println!("[MOUSE] Mouse IRQ enabled: config -> {:#04X}", config | 0x02);
    }
    // Interrupts off while PICS is held — see step 0.
    x86_64::instructions::interrupts::without_interrupts(|| unsafe {
        let mut pics = crate::interrupts::PICS.lock();
        let masks = pics.read_masks();
        pics.write_masks(masks[0], masks[1] & !0x10);
    });

    crate::serial_println!("[MOUSE] PS/2 init complete. IRQ12 unmasked.");
}

// -----| packet processing task |-----

pub async fn mouse_tick() {
    crate::serial_println!("[MOUSE] mouse_tick started -- running async PS/2 init.");

    // Run the full hardware init here, inside the executor, so kernel_main
    // is never blocked waiting for PS/2 responses.
    hw_init().await;

    crate::serial_println!("[MOUSE] Entering packet loop.");

    let mut packet = [0u8; 3];
    let mut i = 0;

    loop {
        // 1. Drain the entire accumulated packet queue instantly.
        while let Some(byte) = MOUSE_BYTE_QUEUE.pop() {
            // -----| packet synchronisation |-----
            // The first byte of every 3-byte PS/2 packet always has bit 3 set.
            // If we are at index 0 and this bit is clear, we are out of sync --
            // skip the byte and wait for a valid packet start.
            if i == 0 && (byte & 0x08) == 0 {
                continue;
            }

            packet[i] = byte;
            i += 1;

            if i == 3 {
                process_packet(packet);
                i = 0;
            }
        }
        
        // 2. Queue is now empty. Sleep for 1ms instead of `yield_now`.
        // `yield_now()` immediately queues the task again, which would prevent
        // the executor's `is_empty()` check from ever succeeding, keeping CPU at 100%.
        crate::interrupts::SleepFuture::new(1).await;
    }
}

fn process_packet(p: [u8; 3]) {
    let flags = p[0];
    let mut dx = p[1] as i32;
    let mut dy = p[2] as i32;

    // Apply sign extension (PS/2 uses 9-bit signed values).
    if (flags & 0x10) != 0 { dx -= 256; }
    if (flags & 0x20) != 0 { dy -= 256; }

    let left  = (flags & 0x01) != 0;
    let right = (flags & 0x02) != 0;

    let mut state = MOUSE_STATE.lock();
    state.x = (state.x + dx).clamp(0, 1023);
    state.y = (state.y - dy).clamp(0, 767); // Y is inverted in PS/2
    state.left_button  = left;
    state.right_button = right;
    state.dx = state.dx.saturating_add(dx);
    state.dy = state.dy.saturating_add(-dy);
}

/// Returns (x, y, dx, dy, buttons) for a single low-latency input snapshot.
/// dx/dy are the deltas since the last call to this function.
/// buttons: bit 0 = left, bit 1 = right.
pub fn get_state() -> (i32, i32, i32, i32, u8) {
    let mut state = MOUSE_STATE.lock();
    let dx = state.dx;
    let dy = state.dy;
    state.dx = 0;
    state.dy = 0;
    let buttons = (state.left_button  as u8)
                | ((state.right_button as u8) << 1);
    (state.x, state.y, dx, dy, buttons)
}
