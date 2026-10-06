//! Archive tree built from streamed entries, with cached subtree stats.

use crate::native_list;
use std::cell::Cell;

/// Directory recursion depth cap for plain-folder browsing.
pub const MAX_DEPTH: u8 = 64;

#[derive(Default)]
pub struct Node {
    pub dirs: std::collections::BTreeMap<String, Node>,
    pub files: std::collections::BTreeMap<String, (u64, Option<i64>)>,
    /// Cached (files_count, total_bytes) for the whole subtree — computed once.
    pub stats: Cell<Option<(usize, u64)>>,
}

/// Insert an entry; clears cached subtree stats along the insertion path.
pub fn tree_insert(root: &mut Node, entry: &native_list::Entry) {
    let parts: Vec<&str> = entry.path.split('/').filter(|s| !s.is_empty()).collect();
    let Some((last, parents)) = parts.split_last() else { return };
    let mut node = root;
    node.stats.set(None);
    for p in parents {
        node = node.dirs.entry((*p).to_string()).or_default();
        node.stats.set(None);
    }
    if entry.is_dir {
        node.dirs.entry((*last).to_string()).or_default();
    } else {
        node.files
            .entry((*last).to_string())
            .or_insert((entry.size, entry.mtime));
    }
}

pub fn resolve<'a>(root: &'a Node, path: &[String]) -> Option<&'a Node> {
    let mut node: &Node = root;
    for part in path {
        node = node.dirs.get(part)?;
    }
    Some(node)
}

/// Cached recursive subtree stats: O(1) after first computation per node.
pub fn subtree_stats(node: &Node) -> (usize, u64) {
    if let Some(c) = node.stats.get() {
        return c;
    }
    let mut files = node.files.len();
    let mut bytes: u64 = node.files.values().map(|(s, _)| *s).sum();
    for d in node.dirs.values() {
        let (f, b) = subtree_stats(d);
        files += f;
        bytes += b;
    }
    let result = (files, bytes);
    node.stats.set(Some(result));
    result
}

pub fn fmt_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0usize;
    while v >= 1024.0 && u < 4 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

pub fn fmt_date(ts: Option<i64>) -> String {
    use chrono::Datelike;
    match ts {
        Some(t) if t > 0 => chrono::DateTime::from_timestamp(t, 0)
            .map(|d| format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day()))
            .unwrap_or_default(),
        _ => String::new(),
    }
}