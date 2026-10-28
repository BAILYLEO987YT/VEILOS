// -----| VEILOS Checkpoint Manager |-----
//
// Provides step-boundary checkpointing for native VEILOS worker threads
// that have been offloaded to a mesh node.  Win32 threads never use this
// path — the `is_native` flag on the capsule gates every write.
//
// Storage model
// -------------
//   Checkpoints are written to the DFS-visible path:
//
//       /net/<local_node_hex>/tmp/<job_id_hex>_<step>.ckpt
//
//   The "local node" here is the *executing* node — the one that actually
//   ran the step.  The OffloadBroker on the originating node can read them
//   back via DistributedFs::read_file() using that path.
//
//   If the job runs locally (no offload) the same path is used but
//   DistributedFs::write_file falls back to the local VFS directly,
//   so the same API works in both cases.
//
// File format
// -----------
//   [0..4]   magic:       u32 LE = 0x434B5054  ("CKPT")
//   [4..12]  job_id:      u64 LE
//   [12..16] step_index:  u32 LE
//   [16..24] timestamp:   u64 LE  (PIT ticks at write time)
//   [24..28] payload_len: u32 LE
//   [28..]   payload:     [u8; payload_len]  (opaque app-defined data)
//
// Cleanup
// -------
//   Call `purge_checkpoints(job_id)` once the job has completed
//   successfully.  This removes every .ckpt file for that job from the
//   local VFS /tmp/ directory, keeping it tidy.

#![allow(dead_code)]

extern crate alloc;
use alloc::vec::Vec;
use alloc::string::String;
use alloc::format;
use core::sync::atomic::Ordering;

// Magic bytes: "CKPT"
pub const CKPT_MAGIC: u32 = 0x434B_5054;

// =====================================================================
// -----| Checkpoint Manager |-----
// =====================================================================

pub struct CheckpointManager;

impl CheckpointManager {
    // ----------------------------------------------------------------
    // Write
    // ----------------------------------------------------------------

    /// Write a step-boundary checkpoint to the local VFS.
    ///
    /// The file is stored at `/tmp/<job_id_hex>_<step>.ckpt` so the
    /// DFS server on this node can serve it to the OffloadBroker on
    /// the originating node via the `/net/<node>/tmp/...` path.
    ///
    /// This is a no-op if the VFS write fails — checkpointing is
    /// best-effort; a failed write just means a retry starts over
    /// from step 0 rather than the last checkpoint.
    pub fn write_checkpoint(job_id: u64, step: u32, payload: &[u8]) {
        let path = Self::local_path(job_id, step);
        let data = Self::serialise(job_id, step, payload);
        match crate::vfs::VFS.lock().write_file(&path, &data) {
            Ok(_) => {
                crate::serial_println!(
                    "[CKPT] Wrote checkpoint job={:#x} step={} path={}",
                    job_id, step, path
                );
            }
            Err(e) => {
                crate::serial_println!(
                    "[CKPT] WARNING: Failed to write checkpoint {}: {:?}",
                    path, e
                );
            }
        }
    }

    // ----------------------------------------------------------------
    // Read
    // ----------------------------------------------------------------

    /// Read a checkpoint back from *this* node's local VFS.
    ///
    /// Returns the opaque payload bytes on success.
    pub fn read_checkpoint_local(job_id: u64, step: u32) -> Option<Vec<u8>> {
        let path = Self::local_path(job_id, step);
        let raw = crate::vfs::VFS.lock().read_file(&path).ok()?;
        Self::deserialise(&raw).map(|(_, _, payload)| payload)
    }

    /// Read a checkpoint from a *remote* node via the DFS layer.
    ///
    /// `from_node_hex` is the 16-char hex NodeId string of the node
    /// that ran the step (and therefore wrote the checkpoint).
    pub fn read_checkpoint_remote(
        job_id: u64,
        step: u32,
        from_node_hex: &str,
    ) -> Option<Vec<u8>> {
        let path = format!("/net/{}/tmp/{:#x}_{}.ckpt", from_node_hex, job_id, step);
        match crate::distributed::DistributedFs::read_file(&path) {
            Ok(raw) => Self::deserialise(&raw).map(|(_, _, payload)| payload),
            Err(e) => {
                crate::serial_println!(
                    "[CKPT] Remote read failed {}: {:?}", path, e
                );
                None
            }
        }
    }

    // ----------------------------------------------------------------
    // Discovery
    // ----------------------------------------------------------------

    /// Scan the local /tmp/ directory for the highest step checkpoint
    /// belonging to `job_id`.  Returns `None` when no checkpoints exist.
    ///
    /// This is called by the OffloadBroker's retry path to determine
    /// where a re-submitted job should resume.
    pub fn find_latest_checkpoint_local(job_id: u64) -> Option<u32> {
        let entries = match crate::vfs::VFS.lock().list_dir("/tmp") {
            Ok(e) => e,
            Err(_) => return None,
        };

        let prefix = format!("{:#x}_", job_id);
        let suffix = ".ckpt";
        let mut best: Option<u32> = None;

        for entry in &entries {
            if !entry.name.starts_with(&*prefix) { continue; }
            if !entry.name.ends_with(suffix)      { continue; }

            // Name is "<job_hex>_<step>.ckpt"
            let inner = &entry.name[prefix.len()..entry.name.len() - suffix.len()];
            if let Ok(step) = u32::from_str_radix(inner.trim_start_matches("0x"), 16)
                .or_else(|_| inner.parse::<u32>()) {
                best = Some(match best {
                    Some(prev) => prev.max(step),
                    None       => step,
                });
            }
        }
        best
    }

    // ----------------------------------------------------------------
    // Cleanup
    // ----------------------------------------------------------------

    /// Delete all checkpoint files for `job_id` from the local /tmp/ dir.
    ///
    /// Called by the OffloadBroker when a job completes successfully so
    /// checkpoint files don't accumulate indefinitely.
    pub fn purge_checkpoints(job_id: u64) {
        let entries = match crate::vfs::VFS.lock().list_dir("/tmp") {
            Ok(e) => e,
            Err(_) => return,
        };

        let prefix = format!("{:#x}_", job_id);
        let suffix = ".ckpt";
        let mut deleted = 0u32;

        for entry in &entries {
            if entry.name.starts_with(&*prefix) && entry.name.ends_with(suffix) {
                let path = format!("/tmp/{}", entry.name);
                if crate::vfs::VFS.lock().delete_file(&path).is_ok() {
                    deleted += 1;
                }
            }
        }

        if deleted > 0 {
            crate::serial_println!(
                "[CKPT] Purged {} checkpoint(s) for job {:#x}", deleted, job_id
            );
        }
    }

    // ----------------------------------------------------------------
    // Private helpers
    // ----------------------------------------------------------------

    fn local_path(job_id: u64, step: u32) -> String {
        format!("/tmp/{:#x}_{}.ckpt", job_id, step)
    }

    fn serialise(job_id: u64, step: u32, payload: &[u8]) -> Vec<u8> {
        let ts = crate::interrupts::TICKS.load(Ordering::Relaxed);
        let plen = payload.len() as u32;
        let mut out = Vec::with_capacity(28 + payload.len());
        out.extend_from_slice(&CKPT_MAGIC.to_le_bytes());   // [0..4]
        out.extend_from_slice(&job_id.to_le_bytes());        // [4..12]
        out.extend_from_slice(&step.to_le_bytes());          // [12..16]
        out.extend_from_slice(&ts.to_le_bytes());            // [16..24]
        out.extend_from_slice(&plen.to_le_bytes());          // [24..28]
        out.extend_from_slice(payload);                      // [28..]
        out
    }

    /// Returns `(job_id, step_index, payload)` or `None` on parse error.
    fn deserialise(data: &[u8]) -> Option<(u64, u32, Vec<u8>)> {
        if data.len() < 28 { return None; }
        let magic = u32::from_le_bytes(data[0..4].try_into().ok()?);
        if magic != CKPT_MAGIC { return None; }
        let job_id     = u64::from_le_bytes(data[4..12].try_into().ok()?);
        let step       = u32::from_le_bytes(data[12..16].try_into().ok()?);
        // [16..24] timestamp — parsed but not returned; caller doesn't need it.
        let plen       = u32::from_le_bytes(data[24..28].try_into().ok()?) as usize;
        if data.len() < 28 + plen { return None; }
        Some((job_id, step, data[28..28 + plen].to_vec()))
    }

    // ----------------------------------------------------------------
    // Terminal command helper
    // ----------------------------------------------------------------

    /// List all checkpoint files currently on this node's /tmp/ dir.
    /// Returns a Vec of human-readable summary strings for the CHECKPOINT LIST
    /// terminal command.
    pub fn list_all() -> Vec<String> {
        let entries = match crate::vfs::VFS.lock().list_dir("/tmp") {
            Ok(e) => e,
            Err(_) => return alloc::vec![String::from("(cannot read /tmp)")],
        };

        let mut out = Vec::new();
        for entry in &entries {
            if !entry.name.ends_with(".ckpt") { continue; }
            let path = format!("/tmp/{}", entry.name);
            let summary = match crate::vfs::VFS.lock().read_file(&path) {
                Ok(raw) => match Self::deserialise(&raw) {
                    Some((job_id, step, payload)) => {
                        format!(
                            "  {} — job={:#x} step={} payload={}B",
                            entry.name, job_id, step, payload.len()
                        )
                    }
                    None => format!("  {} — (corrupt)", entry.name),
                },
                Err(_) => format!("  {} — (unreadable)", entry.name),
            };
            out.push(summary);
        }

        if out.is_empty() {
            out.push(String::from("  (no checkpoints)"));
        }
        out
    }
}
