// -----| VEILOS Task Capsule |-----
//
// A TaskCapsule is the unit of work the OffloadBroker sends across the
// mesh.  It is entirely self-contained: the remote node needs nothing
// beyond the capsule itself (plus any DFS-fetched assets listed in
// `asset_paths`) to begin execution.
//
// Wire serialisation format (little-endian throughout):
//
//   [0..8]   magic:               u64 = 0x564C_5441_534B_0001  ("VLTASK\x00\x01")
//   [8..16]  job_id:              u64
//   [16..20] owner_pid:           u32
//   [20..24] thread_id:           u32
//   [24..28] step_index:          u32
//   [28]     is_native:           u8  (1 = native, 0 = Win32)
//   [29..31] _pad:                [u8; 2]
//   [31..32] _pad:                u8
//   [32..40] timeout_ms:          u64
//   [40..48] capability_requirements: u64
//   [48..52] input_data_len:      u32
//   [52..]   input_data:          [u8; input_data_len]
//   after input_data:
//     asset_path_count:           u16
//     for each path:
//       path_len:                 u16
//       path_bytes:               [u8; path_len]
//
// TaskResult wire format:
//   [0..8]   magic:               u64 = 0x564C_5241_5355_0001  ("VLRASU\x00\x01")
//   [8..16]  job_id:              u64
//   [16..20] step_index:          u32
//   [20]     success:             u8
//   [21..23] _pad:                [u8; 2]
//   [23..24] _pad:                u8
//   [24..28] output_data_len:     u32
//   [28..]   output_data:         [u8; output_data_len]
//   after output_data:
//     error_msg_len:              u16
//     error_msg:                  [u8; error_msg_len]  (empty when success==1)

#![allow(dead_code)]

extern crate alloc;
use alloc::vec::Vec;
use alloc::string::String;

// =====================================================================
// -----| Magic constants |-----
// =====================================================================

pub const CAPSULE_MAGIC: u64 = 0x564C_5441_534B_0001; // "VLTASK\x00\x01"
pub const RESULT_MAGIC:  u64 = 0x564C_5241_5355_0001; // "VLRASU\x00\x01"

// =====================================================================
// -----| TaskCapsule |-----
// =====================================================================

/// A self-contained, wire-serialisable unit of offloadable work.
#[derive(Debug, Clone)]
pub struct TaskCapsule {
    /// Unique job identifier assigned by the OffloadBroker.
    pub job_id: u64,
    /// PID of the process that owns this work.
    pub owner_pid: u32,
    /// Thread ID from the ThreadManager.
    pub thread_id: u32,
    /// Which step of a multi-step job this capsule represents.
    /// 0 for single-step jobs or the first step of a sequence.
    pub step_index: u32,
    /// true = native VEILOS thread (full offload + checkpoint);
    /// false = Win32 thread (best-effort, no checkpoint).
    pub is_native: bool,
    /// Small input data bundled directly in the capsule.
    /// For large inputs use `asset_paths` and let the remote node
    /// fetch from DFS on demand.
    pub input_data: Vec<u8>,
    /// DFS paths of large assets the remote node must fetch before
    /// executing (e.g. "/net/<node>/data/texture.bin").
    pub asset_paths: Vec<String>,
    /// Maximum wall-clock time the remote node should allow for this
    /// step before returning a timeout error.  0 = no limit.
    pub timeout_ms: u64,
    /// Bitfield of distributed::CAP_* flags the receiving node must
    /// advertise in its capability mask.
    pub capability_requirements: u64,
}

impl TaskCapsule {
    /// Create a new capsule.  `job_id` is assigned by the OffloadBroker.
    pub fn new(
        job_id:       u64,
        owner_pid:    u32,
        thread_id:    u32,
        step_index:   u32,
        is_native:    bool,
        input_data:   Vec<u8>,
        asset_paths:  Vec<String>,
        timeout_ms:   u64,
        cap_reqs:     u64,
    ) -> Self {
        Self {
            job_id,
            owner_pid,
            thread_id,
            step_index,
            is_native,
            input_data,
            asset_paths,
            timeout_ms,
            capability_requirements: cap_reqs,
        }
    }

    // ----------------------------------------------------------------
    // Serialisation
    // ----------------------------------------------------------------

    /// Serialise the capsule to a byte vector suitable for sending over
    /// the mesh TCP connection.  The format is described in the module
    /// header comment.
    pub fn serialise(&self) -> Vec<u8> {
        let input_len = self.input_data.len() as u32;
        // Pre-calculate total size so we can reserve in one go.
        let asset_section_len: usize = 2 + self.asset_paths.iter()
            .map(|p| 2 + p.len())
            .sum::<usize>();
        let total = 52 + input_len as usize + asset_section_len;

        let mut out = Vec::with_capacity(total);

        // Header
        out.extend_from_slice(&CAPSULE_MAGIC.to_le_bytes());      // [0..8]
        out.extend_from_slice(&self.job_id.to_le_bytes());         // [8..16]
        out.extend_from_slice(&self.owner_pid.to_le_bytes());      // [16..20]
        out.extend_from_slice(&self.thread_id.to_le_bytes());      // [20..24]
        out.extend_from_slice(&self.step_index.to_le_bytes());     // [24..28]
        out.push(if self.is_native { 1 } else { 0 });              // [28]
        out.extend_from_slice(&[0u8; 3]);                          // [29..32] pad
        out.extend_from_slice(&self.timeout_ms.to_le_bytes());     // [32..40]
        out.extend_from_slice(                                      // [40..48]
            &self.capability_requirements.to_le_bytes());
        out.extend_from_slice(&input_len.to_le_bytes());           // [48..52]

        // Input data
        out.extend_from_slice(&self.input_data);                   // [52..]

        // Asset paths
        let path_count = self.asset_paths.len().min(u16::MAX as usize) as u16;
        out.extend_from_slice(&path_count.to_le_bytes());
        for path in self.asset_paths.iter().take(path_count as usize) {
            let pb = path.as_bytes();
            let plen = pb.len().min(u16::MAX as usize) as u16;
            out.extend_from_slice(&plen.to_le_bytes());
            out.extend_from_slice(&pb[..plen as usize]);
        }

        out
    }

    /// Deserialise a capsule from raw bytes.  Returns `None` if the
    /// buffer is malformed, too short, or the magic is wrong.
    pub fn deserialise(data: &[u8]) -> Option<Self> {
        if data.len() < 52 { return None; }

        let magic = u64::from_le_bytes(data[0..8].try_into().ok()?);
        if magic != CAPSULE_MAGIC { return None; }

        let job_id   = u64::from_le_bytes(data[8..16].try_into().ok()?);
        let owner    = u32::from_le_bytes(data[16..20].try_into().ok()?);
        let thread   = u32::from_le_bytes(data[20..24].try_into().ok()?);
        let step     = u32::from_le_bytes(data[24..28].try_into().ok()?);
        let native   = data[28] != 0;
        // [29..32] padding — skip
        let timeout  = u64::from_le_bytes(data[32..40].try_into().ok()?);
        let cap_reqs = u64::from_le_bytes(data[40..48].try_into().ok()?);
        let inp_len  = u32::from_le_bytes(data[48..52].try_into().ok()?) as usize;

        let mut cursor = 52usize;
        if data.len() < cursor + inp_len { return None; }
        let input_data = data[cursor..cursor + inp_len].to_vec();
        cursor += inp_len;

        // Asset paths
        if data.len() < cursor + 2 { return None; }
        let path_count = u16::from_le_bytes(data[cursor..cursor+2].try_into().ok()?) as usize;
        cursor += 2;

        let mut asset_paths = Vec::with_capacity(path_count);
        for _ in 0..path_count {
            if data.len() < cursor + 2 { return None; }
            let plen = u16::from_le_bytes(data[cursor..cursor+2].try_into().ok()?) as usize;
            cursor += 2;
            if data.len() < cursor + plen { return None; }
            let path_str: String = core::str::from_utf8(&data[cursor..cursor+plen])
                .unwrap_or("")
                .chars()
                .collect();
            asset_paths.push(path_str);
            cursor += plen;
        }

        Some(Self {
            job_id,
            owner_pid: owner,
            thread_id: thread,
            step_index: step,
            is_native: native,
            input_data,
            asset_paths,
            timeout_ms: timeout,
            capability_requirements: cap_reqs,
        })
    }

    /// Human-readable summary for terminal output.
    pub fn describe(&self) -> alloc::string::String {
        alloc::format!(
            "job={:#x} pid={} tid={} step={} native={} input={}B assets={} caps={:#x} timeout={}ms",
            self.job_id, self.owner_pid, self.thread_id, self.step_index,
            self.is_native, self.input_data.len(), self.asset_paths.len(),
            self.capability_requirements, self.timeout_ms
        )
    }
}

// =====================================================================
// -----| TaskResult |-----
// =====================================================================

/// The result returned by a remote node after executing a TaskCapsule.
#[derive(Debug, Clone)]
pub struct TaskResult {
    /// Matches the `job_id` of the originating capsule.
    pub job_id:      u64,
    /// Which step this result corresponds to.
    pub step_index:  u32,
    /// true = step completed without error.
    pub success:     bool,
    /// Output produced by the step (may be empty).
    pub output_data: Vec<u8>,
    /// Human-readable error message when `success == false`.
    pub error_msg:   Option<String>,
}

impl TaskResult {
    pub fn ok(job_id: u64, step_index: u32, output: Vec<u8>) -> Self {
        Self { job_id, step_index, success: true, output_data: output, error_msg: None }
    }

    pub fn err(job_id: u64, step_index: u32, msg: &str) -> Self {
        Self {
            job_id, step_index, success: false,
            output_data: Vec::new(),
            error_msg: Some(String::from(msg)),
        }
    }

    // ----------------------------------------------------------------
    // Serialisation
    // ----------------------------------------------------------------

    pub fn serialise(&self) -> Vec<u8> {
        let out_len  = self.output_data.len() as u32;
        let err_bytes = self.error_msg.as_deref().unwrap_or("").as_bytes();
        let err_len  = err_bytes.len().min(u16::MAX as usize) as u16;
        let total    = 8 + 8 + 4 + 1 + 3 + 4 + out_len as usize + 2 + err_len as usize;

        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(&RESULT_MAGIC.to_le_bytes());         // [0..8]
        out.extend_from_slice(&self.job_id.to_le_bytes());          // [8..16]
        out.extend_from_slice(&self.step_index.to_le_bytes());      // [16..20]
        out.push(if self.success { 1 } else { 0 });                 // [20]
        out.extend_from_slice(&[0u8; 3]);                           // [21..24] pad
        out.extend_from_slice(&out_len.to_le_bytes());              // [24..28]
        out.extend_from_slice(&self.output_data);
        out.extend_from_slice(&err_len.to_le_bytes());
        out.extend_from_slice(&err_bytes[..err_len as usize]);
        out
    }

    pub fn deserialise(data: &[u8]) -> Option<Self> {
        if data.len() < 28 { return None; }
        let magic = u64::from_le_bytes(data[0..8].try_into().ok()?);
        if magic != RESULT_MAGIC { return None; }

        let job_id     = u64::from_le_bytes(data[8..16].try_into().ok()?);
        let step_index = u32::from_le_bytes(data[16..20].try_into().ok()?);
        let success    = data[20] != 0;
        // [21..24] padding
        let out_len    = u32::from_le_bytes(data[24..28].try_into().ok()?) as usize;

        let mut cursor = 28usize;
        if data.len() < cursor + out_len { return None; }
        let output_data = data[cursor..cursor + out_len].to_vec();
        cursor += out_len;

        if data.len() < cursor + 2 { return None; }
        let err_len = u16::from_le_bytes(data[cursor..cursor+2].try_into().ok()?) as usize;
        cursor += 2;
        if data.len() < cursor + err_len { return None; }
        let error_msg = if err_len > 0 {
            Some(String::from(core::str::from_utf8(&data[cursor..cursor+err_len]).unwrap_or("?")))
        } else {
            None
        };

        Some(Self { job_id, step_index, success, output_data, error_msg })
    }
}

// =====================================================================
// -----| Next job ID generator |-----
// =====================================================================

use core::sync::atomic::{AtomicU64, Ordering};

static NEXT_JOB_ID: AtomicU64 = AtomicU64::new(1);

/// Allocate a globally unique job ID.
pub fn alloc_job_id() -> u64 {
    NEXT_JOB_ID.fetch_add(1, Ordering::Relaxed)
}
