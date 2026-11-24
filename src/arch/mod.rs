//! CPU and platform: descriptor tables, interrupts, APICs, SMP, the timebase.

pub mod gdt;
pub mod interrupts;
pub mod ioapic;
pub mod smp;
pub mod time;
