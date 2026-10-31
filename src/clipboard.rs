// -----| VEILOS Clipboard Subsystem |-----
//
// Multi-channel clipboard with configurable security and persistence.
// Config driven by VScript (config.vsh):
//   CLIPBOARD_CHANNELS_DEFAULT, CLIPBOARD_LOG_SIZE, CLIPBOARD_LOCATION,
//   CLIPBOARD_FILE_SIZE_LIMIT, CLIPBOARD_MAX_SIZE, CLIPBOARD_STORAGE_DEFAULT

#![allow(dead_code)]

use alloc::vec::Vec;
use alloc::string::String;
use alloc::format;
use spin::Mutex;

// =====================================================================
// -----| Enums |-----
// =====================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardChannel {
    Public,
    Private,
    Sensitive,
    AppRestricted,
}

impl ClipboardChannel {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_uppercase().as_str() {
            "PUBLIC" => Some(Self::Public),
            "PRIVATE" => Some(Self::Private),
            "SENSITIVE" => Some(Self::Sensitive),
            "APP_RESTRICTED" => Some(Self::AppRestricted),
            _ => None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Public => "PUBLIC",
            Self::Private => "PRIVATE",
            Self::Sensitive => "SENSITIVE",
            Self::AppRestricted => "APP_RESTRICTED",
        }
    }
}

/// Persistence behaviour for clipboard entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardStorage {
    /// Stored in the .dat file. Survives reboots.
    Permanent,
    /// Stored in memory only. Lost on reboot.
    Transient,
    /// Cleared after first paste.
    Clear,
}

impl ClipboardStorage {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_uppercase().as_str() {
            "PERMINANT" | "PERMANENT" => Some(Self::Permanent),
            "TRANSIENT" => Some(Self::Transient),
            "CLEAR" => Some(Self::Clear),
            _ => None,
        }
    }
}

// =====================================================================
// -----| Clipboard Data |-----
// =====================================================================

#[derive(Debug, Clone)]
pub enum ClipboardData {
    Text(String),
    FilePaths(Vec<String>),
    Binary(Vec<u8>),
    Empty,
}

impl ClipboardData {
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Text(s) => s.is_empty(),
            Self::FilePaths(v) => v.is_empty(),
            Self::Binary(b) => b.is_empty(),
            Self::Empty => true,
        }
    }
    pub fn preview(&self, max_len: usize) -> String {
        match self {
            Self::Text(s) => {
                if s.len() <= max_len { s.clone() }
                else { let mut p = String::from(&s[..max_len.min(s.len())]); p.push_str("..."); p }
            }
            Self::FilePaths(paths) => {
                if paths.len() == 1 { paths[0].clone() } else { format!("{} files", paths.len()) }
            }
            Self::Binary(b) => format!("[{} bytes]", b.len()),
            Self::Empty => String::from("(empty)"),
        }
    }
}

// =====================================================================
// -----| Clipboard Entry |-----
// =====================================================================

pub struct ClipboardEntry {
    pub data: ClipboardData,
    pub channel: ClipboardChannel,
    pub storage: ClipboardStorage,
    pub timestamp_ms: u64,
    pub source_pid: u32,
    pub key_handle: Option<crate::encryption::KeyHandle>,
    pub is_cut: bool,
    /// Set to true after a CLEAR-mode entry has been pasted once.
    pub consumed: bool,
    /// If compressed, holds the index to the compression manifest.
    pub compressed_manifest: Option<usize>,
}

// =====================================================================
// -----| Clipboard Manager |-----
// =====================================================================

pub struct ClipboardManager {
    pub history: Vec<ClipboardEntry>,
    pub max_history: usize,
    pub storage_path: String,
    pub file_size_limit: usize,
    pub max_entry_size: usize,
    pub active_channel: ClipboardChannel,
    pub default_storage: ClipboardStorage,
}

impl ClipboardManager {
    pub const fn new() -> Self {
        Self {
            history: Vec::new(),
            max_history: 20,
            storage_path: String::new(),
            file_size_limit: 2048,
            max_entry_size: 2048,
            active_channel: ClipboardChannel::Public,
            default_storage: ClipboardStorage::Permanent,
        }
    }

    pub fn push(&mut self, entry: ClipboardEntry) {
        SEQUENCE.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        self.history.insert(0, entry);
        while self.history.len() > self.max_history { self.history.pop(); }
    }

    /// Get latest non-consumed entry accessible by pid.
    pub fn latest_for_pid(&self, pid: u32) -> Option<usize> {
        self.history.iter().position(|e| {
            if e.consumed { return false; }
            match e.channel {
                ClipboardChannel::AppRestricted => e.source_pid == pid,
                _ => true,
            }
        })
    }

    pub fn copy_text(&mut self, text: String, pid: u32, is_cut: bool) {
        if text.is_empty() { return; }
        let ms = crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed);
        let channel = self.active_channel;
        let storage = self.default_storage;

        let text_bytes = text.as_bytes();
        let use_compression = text_bytes.len() >= 256;

        // Encrypt for non-PUBLIC channels.
        let (data, key_handle, compressed_manifest) = if channel != ClipboardChannel::Public {
            match crate::encryption::cen_generate_key(
                crate::encryption::FeatureTag::new("clipboard"),
                crate::encryption::SecurityLevel::Standard, None,
            ) {
                Ok(kh) => match crate::encryption::cen_encrypt(kh, text_bytes, None) {
                    Ok(enc) => (ClipboardData::Binary(enc), Some(kh), None),
                    Err(_) => (ClipboardData::Text(text), None, None),
                },
                Err(_) => (ClipboardData::Text(text), None, None),
            }
        } else {
            if use_compression {
                let m_idx = crate::compression::compress(text_bytes, "clipboard_text");
                (ClipboardData::Empty, None, Some(m_idx))
            } else {
                (ClipboardData::Text(text), None, None)
            }
        };

        self.push(ClipboardEntry {
            data, channel, storage, timestamp_ms: ms,
            source_pid: pid, key_handle, is_cut, consumed: false,
            compressed_manifest,
        });

        // Persist if PERMANENT storage.
        if storage == ClipboardStorage::Permanent { self.persist(); }

        let label = if is_cut { "CUT" } else { "COPIED" };
        crate::desktop::notify(
            crate::desktop::NotifCategory::Info, "CLIPBOARD",
            &format!("{} to {} channel", label, channel.as_str()),
        );
    }

    pub fn copy_files(&mut self, paths: Vec<String>, pid: u32, is_cut: bool) {
        if paths.is_empty() { return; }
        let ms = crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed);
        let count = paths.len();
        self.push(ClipboardEntry {
            data: ClipboardData::FilePaths(paths),
            channel: self.active_channel,
            storage: self.default_storage,
            timestamp_ms: ms, source_pid: pid,
            key_handle: None, is_cut, consumed: false,
            compressed_manifest: None,
        });
        if self.default_storage == ClipboardStorage::Permanent { self.persist(); }
        let label = if is_cut { "CUT" } else { "COPIED" };
        crate::desktop::notify(
            crate::desktop::NotifCategory::Info, "CLIPBOARD",
            &format!("{} {} file(s)", label, count),
        );
    }

    pub fn paste_text(&mut self, pid: u32) -> Option<String> {
        let idx = self.latest_for_pid(pid)?;
        self.paste_text_at(idx)
    }

    pub fn paste_text_at(&mut self, idx: usize) -> Option<String> {
        if idx >= self.history.len() { return None; }
        if self.history[idx].consumed { return None; }
        
        let result = match &self.history[idx].data {
            ClipboardData::Text(s) => Some(s.clone()),
            ClipboardData::Empty => {
                if let Some(m_idx) = self.history[idx].compressed_manifest {
                    if let Some(dec) = crate::compression::decompress(m_idx) {
                        String::from_utf8(dec).ok()
                    } else { None }
                } else { None }
            }
            ClipboardData::Binary(enc) => {
                if let Some(kh) = self.history[idx].key_handle {
                    crate::encryption::cen_decrypt(kh, enc, None).ok()
                        .and_then(|b| core::str::from_utf8(&b).ok().map(String::from))
                } else { None }
            }
            _ => None,
        };
        if self.history[idx].storage == ClipboardStorage::Clear {
            self.history[idx].consumed = true;
        }
        result
    }

    pub fn paste_files(&mut self, pid: u32) -> Option<(Vec<String>, bool)> {
        let idx = self.latest_for_pid(pid)?;
        let result = match &self.history[idx].data {
            ClipboardData::FilePaths(p) => Some((p.clone(), self.history[idx].is_cut)),
            _ => None,
        };
        if self.history[idx].storage == ClipboardStorage::Clear {
            self.history[idx].consumed = true;
        }
        result
    }

    // -----| Persistence |-----

    /// Write PERMANENT entries to disk at the configured storage_path.
    pub fn persist(&self) {
        if self.storage_path.is_empty() { return; }
        // Serialize: each entry as a line "CHANNEL|TIMESTAMP|PREVIEW"
        // For v1 we persist only plaintext PUBLIC entries.
        let mut buf = Vec::new();
        buf.extend_from_slice(b"VCLIP1\n");
        for entry in &self.history {
            if entry.storage != ClipboardStorage::Permanent { continue; }
            if entry.consumed { continue; }
            match &entry.data {
                ClipboardData::Text(s) => {
                    let line = format!("T|{}|{}|{}\n",
                        entry.channel.as_str(), entry.timestamp_ms,
                        s.replace('\n', "\\n"));
                    if buf.len() + line.len() <= self.file_size_limit {
                        buf.extend_from_slice(line.as_bytes());
                    }
                }
                ClipboardData::FilePaths(paths) => {
                    let joined = paths.join(";");
                    let line = format!("F|{}|{}|{}|{}\n",
                        entry.channel.as_str(), entry.timestamp_ms,
                        if entry.is_cut { "CUT" } else { "COPY" }, joined);
                    if buf.len() + line.len() <= self.file_size_limit {
                        buf.extend_from_slice(line.as_bytes());
                    }
                }
                ClipboardData::Empty => {
                    if let Some(m_idx) = entry.compressed_manifest {
                        if let Some(dec) = crate::compression::decompress(m_idx) {
                            if let Ok(s) = core::str::from_utf8(&dec) {
                                let line = format!("T|{}|{}|{}\n",
                                    entry.channel.as_str(), entry.timestamp_ms,
                                    s.replace('\n', "\\n"));
                                if buf.len() + line.len() <= self.file_size_limit {
                                    buf.extend_from_slice(line.as_bytes());
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        let _ = crate::vfs::VFS.write_file(&self.storage_path, &buf);
    }

    /// Load persisted entries from disk on boot.
    pub fn load_persisted(&mut self) {
        if self.storage_path.is_empty() { return; }
        let data = match crate::vfs::VFS.read_file(&self.storage_path) {
            Ok(d) => d,
            Err(_) => return,
        };
        let text = match core::str::from_utf8(&data) {
            Ok(s) => s,
            Err(_) => return,
        };
        let mut lines = text.lines();
        // Check header.
        if lines.next() != Some("VCLIP1") { return; }

        for line in lines {
            let parts: Vec<&str> = line.splitn(4, '|').collect();
            if parts.len() < 4 { continue; }
            let channel = ClipboardChannel::from_str(parts[1]).unwrap_or(ClipboardChannel::Public);
            let timestamp: u64 = parts[2].parse().unwrap_or(0);

            match parts[0] {
                "T" => {
                    let text_content = parts[3].replace("\\n", "\n");
                    self.history.push(ClipboardEntry {
                        data: ClipboardData::Text(text_content),
                        channel, storage: ClipboardStorage::Permanent,
                        timestamp_ms: timestamp, source_pid: 0,
                        key_handle: None, is_cut: false, consumed: false,
                        compressed_manifest: None,
                    });
                }
                "F" => {
                    let fparts: Vec<&str> = parts[3].splitn(2, '|').collect();
                    if fparts.len() == 2 {
                        let is_cut = fparts[0] == "CUT";
                        let paths: Vec<String> = fparts[1].split(';').map(String::from).collect();
                        self.history.push(ClipboardEntry {
                            data: ClipboardData::FilePaths(paths),
                            channel, storage: ClipboardStorage::Permanent,
                            timestamp_ms: timestamp, source_pid: 0,
                            key_handle: None, is_cut, consumed: false,
                            compressed_manifest: None,
                        });
                    }
                }
                _ => {}
            }
        }
        while self.history.len() > self.max_history { self.history.pop(); }
    }
}

pub static CLIPBOARD: Mutex<ClipboardManager> = Mutex::new(ClipboardManager::new());

/// Counts the entries ever copied (GetClipboardSequenceNumber).
pub static SEQUENCE: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(1);

// =====================================================================
// -----| Public API — keyboard shortcuts |-----
// =====================================================================

pub fn clipboard_copy() {
    let focused_id = crate::desktop::DESKTOP_WM.lock().focused_id;
    let Some(id) = focused_id else { return };

    if let Some(idx) = crate::apps::text_editor_slot_for_window(id) {
        let ed = crate::apps::TEXT_EDITOR_POOL[idx].lock();
        if let (Some(start), Some(end)) = (ed.selection_start, ed.selection_end) {
            let (lo, hi) = if start <= end { (start, end) } else { (end, start) };
            let selected: String = ed.content.chars().skip(lo).take(hi - lo).collect();
            if !selected.is_empty() {
                drop(ed);
                CLIPBOARD.lock().copy_text(selected, 0, false);
            }
        }
        return;
    }
    if let Some(idx) = crate::apps::fm_slot_for_window(id) {
        let fm = crate::apps::FILE_MANAGER_POOL[idx].lock();
        let paths = collect_selected_file_paths(&fm);
        if !paths.is_empty() { drop(fm); CLIPBOARD.lock().copy_files(paths, 0, false); }
        return;
    }
    if let Some(idx) = crate::apps::terminal_slot_for_window(id) {
        let term = crate::apps::TERMINAL_POOL[idx].lock();
        if let (Some(start), Some(end)) = (term.selection_start, term.selection_end) {
            let selected = collect_terminal_selection(&term, start, end);
            if !selected.is_empty() { drop(term); CLIPBOARD.lock().copy_text(selected, 0, false); }
        }
        return;
    }
}

pub fn clipboard_cut() {
    let focused_id = crate::desktop::DESKTOP_WM.lock().focused_id;
    let Some(id) = focused_id else { return };

    if let Some(idx) = crate::apps::text_editor_slot_for_window(id) {
        let mut ed = crate::apps::TEXT_EDITOR_POOL[idx].lock();
        if let (Some(start), Some(end)) = (ed.selection_start, ed.selection_end) {
            let (lo, hi) = if start <= end { (start, end) } else { (end, start) };
            let selected: String = ed.content.chars().skip(lo).take(hi - lo).collect();
            if !selected.is_empty() {
                let mut chars: Vec<char> = ed.content.chars().collect();
                chars.drain(lo..hi);
                ed.content = chars.into_iter().collect();
                ed.cursor_pos = lo;
                ed.selection_start = None;
                ed.selection_end = None;
                drop(ed);
                CLIPBOARD.lock().copy_text(selected, 0, true);
            }
        }
        return;
    }
    if let Some(idx) = crate::apps::fm_slot_for_window(id) {
        let fm = crate::apps::FILE_MANAGER_POOL[idx].lock();
        let paths = collect_selected_file_paths(&fm);
        if !paths.is_empty() { drop(fm); CLIPBOARD.lock().copy_files(paths, 0, true); }
        return;
    }
}

pub fn clipboard_paste() {
    clipboard_paste_idx(None);
}

pub fn clipboard_paste_at(idx: usize) {
    clipboard_paste_idx(Some(idx));
}

fn clipboard_paste_idx(opt_idx: Option<usize>) {
    let focused_id = crate::desktop::DESKTOP_WM.lock().focused_id;
    let Some(id) = focused_id else { return };

    if let Some(idx) = crate::apps::text_editor_slot_for_window(id) {
        let text = if let Some(history_idx) = opt_idx {
            CLIPBOARD.lock().paste_text_at(history_idx)
        } else {
            CLIPBOARD.lock().paste_text(0)
        };
        if let Some(text) = text {
            let mut ed = crate::apps::TEXT_EDITOR_POOL[idx].lock();
            // Delete active selection first.
            if let (Some(s), Some(e)) = (ed.selection_start, ed.selection_end) {
                let (lo, hi) = if s <= e { (s, e) } else { (e, s) };
                let mut chars: Vec<char> = ed.content.chars().collect();
                chars.drain(lo..hi);
                ed.content = chars.into_iter().collect();
                ed.cursor_pos = lo;
                ed.selection_start = None;
                ed.selection_end = None;
            }
            let mut chars: Vec<char> = ed.content.chars().collect();
            let paste_chars: Vec<char> = text.chars().collect();
            let pos = ed.cursor_pos.min(chars.len());
            for (i, c) in paste_chars.iter().enumerate() { chars.insert(pos + i, *c); }
            ed.content = chars.into_iter().collect();
            ed.cursor_pos = pos + paste_chars.len();
        }
        return;
    }
    if let Some(idx) = crate::apps::terminal_slot_for_window(id) {
        let text = if let Some(history_idx) = opt_idx {
            CLIPBOARD.lock().paste_text_at(history_idx)
        } else {
            CLIPBOARD.lock().paste_text(0)
        };
        if let Some(text) = text {
            crate::apps::TERMINAL_POOL[idx].lock().input_buf.push_str(&text);
        }
        return;
    }
    if let Some(idx) = crate::apps::fm_slot_for_window(id) {
        let result = CLIPBOARD.lock().paste_files(0);
        if let Some((paths, is_cut)) = result {
            let fm = crate::apps::FILE_MANAGER_POOL[idx].lock();
            let dest_dir = fm.cwd.clone();
            drop(fm);
            for src_path in &paths {
                let file_name = src_path.split('/').last().unwrap_or("file");
                let dest_path = if dest_dir == "/" {
                    format!("/{}", file_name)
                } else {
                    format!("{}/{}", dest_dir, file_name)
                };
                let data = match crate::vfs::VFS.read_file(src_path) {
                    Ok(d) => d, Err(_) => continue,
                };
                let _ = crate::vfs::VFS.write_file(&dest_path, &data);
                // For cut, we'd delete source but VFS has no delete_file yet.
                let _ = is_cut;
            }
            crate::apps::FILE_MANAGER_POOL[idx].lock().refresh();
            crate::desktop::notify(
                crate::desktop::NotifCategory::Success, "CLIPBOARD",
                &format!("Pasted {} file(s)", paths.len()),
            );
        }
        return;
    }
}

pub fn clipboard_select_all() {
    let focused_id = crate::desktop::DESKTOP_WM.lock().focused_id;
    let Some(id) = focused_id else { return };
    if let Some(idx) = crate::apps::text_editor_slot_for_window(id) {
        let mut ed = crate::apps::TEXT_EDITOR_POOL[idx].lock();
        let len = ed.content.chars().count();
        ed.selection_start = Some(0);
        ed.selection_end = Some(len);
    }
}

// =====================================================================
// -----| Helpers |-----
// =====================================================================

fn collect_selected_file_paths(fm: &crate::apps::FileManagerState) -> Vec<String> {
    let mut paths = Vec::new();
    if let Some(sel) = fm.selected {
        if sel < fm.entries.len() && fm.entries[sel].name != ".." {
            let path = if fm.cwd == "/" { format!("/{}", fm.entries[sel].name) }
                       else { format!("{}/{}", fm.cwd, fm.entries[sel].name) };
            paths.push(path);
        }
    }
    for &idx in &fm.multi_selected {
        if idx < fm.entries.len() && fm.entries[idx].name != ".." {
            let path = if fm.cwd == "/" { format!("/{}", fm.entries[idx].name) }
                       else { format!("{}/{}", fm.cwd, fm.entries[idx].name) };
            if !paths.contains(&path) { paths.push(path); }
        }
    }
    paths
}

fn collect_terminal_selection(
    term: &crate::apps::TerminalEmulator,
    start: (usize, usize), end: (usize, usize),
) -> String {
    let (sr, sc) = start;
    let (er, ec) = end;
    let (lr, lc, hr, hc) = if sr < er || (sr == er && sc <= ec) { (sr, sc, er, ec) } else { (er, ec, sr, sc) };
    let mut result = String::new();
    for row in lr..=hr {
        if row >= term.history.len() { break; }
        let line = &term.history[row];
        let from = if row == lr { lc } else { 0 };
        let to = if row == hr { hc.min(line.len()) } else { line.len() };
        if from < line.len() { result.push_str(&line[from..to.min(line.len())]); }
        if row < hr { result.push('\n'); }
    }
    result
}
