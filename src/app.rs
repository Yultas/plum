//! plum-tui application: state, event loop, rendering.

use crate::native_list;
use crate::theme;
use crate::tree::{fmt_date, fmt_size, resolve, subtree_stats, tree_insert, Node};

use crossterm::event::{
    self, Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind, DisableMouseCapture,
    EnableMouseCapture,
};
use crossterm::execute;
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
    DefaultTerminal, Frame,
};
use std::io::stdout;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::Duration;

const POLL_MS: u64 = 120;
const WHEEL_SCROLL: usize = 3;
const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

struct PreviewState {
    name: String,
    lines: Vec<String>,
    scroll: usize,
    is_binary: bool,
    size: u64,
}

struct State {
    tree: Node,
    path: Vec<String>,
    sel: usize,
    scroll: usize,
    error: Option<String>,
    archive_name: String,
    archive_path: PathBuf,
    is_archive: bool,
    /// --compress mode: pick format + pack the input.
    compress_mode: bool,
    compress_targets: Vec<PathBuf>,
    fmt_sel: usize,
    lvl_sel: usize,
    out_path: String,
    confirm_overwrite: bool,
    reading: bool,
    extracting: bool,
    tick: usize,
    entries_read: usize,
    extract_total: u64,
    extract_progress: Option<Arc<AtomicU64>>,
    status: String,
    status_kind: u8, // 0 dim, 1 ok, 2 err
    extract_rx: Option<Receiver<Result<String, String>>>,
    listing_rx: Option<Receiver<native_list::Entry>>,
    /// Cached current-directory view, rebuilt only when the tree or path changes.
    view: Vec<ViewEntry>,
    view_gen: u64,
    view_path: Vec<String>,
    /// Increments on every inserted entry — invalidates the view cache.
    gen: u64,
    quit: bool,
    list_area: Rect,
    preview: Option<PreviewState>,
    search_query: String,
    searching: bool,
    show_help: bool,
    password: String,
    editing_password: bool,
    show_password: bool,
    need_elevation: bool,
    auto_close: bool,
    close_delay: Option<u8>,
}

/// One row of the visible directory list (owned — cacheable across frames).
struct ViewEntry {
    name: String,
    full_rel_path: String,
    size: u64,
    mtime: Option<i64>,
    is_dir: bool,
}

impl State {
    fn new(archive_name: String, archive_path: PathBuf, is_archive: bool) -> Self {
        State {
            tree: Node::default(),
            path: Vec::new(),
            sel: 0,
            scroll: 0,
            error: None,
            archive_name,
            archive_path,
            is_archive,
            compress_mode: false,
            compress_targets: Vec::new(),
            fmt_sel: 0,
            lvl_sel: 2,
            out_path: String::new(),
            confirm_overwrite: false,
            reading: true,
            extracting: false,
            tick: 0,
            entries_read: 0,
            extract_total: 0,
            extract_progress: None,
            status: String::new(),
            status_kind: 0,
            extract_rx: None,
            listing_rx: None,
            view: Vec::new(),
            view_gen: 0,
            view_path: Vec::new(),
            gen: 0,
            quit: false,
            list_area: Rect::default(),
            preview: None,
            search_query: String::new(),
            searching: false,
            show_help: false,
            password: String::new(),
            editing_password: false,
            show_password: false,
            need_elevation: false,
            auto_close: false,
            close_delay: None,
        }
    }

    fn navigate(&mut self, push: Option<String>) {
        if let Some(name) = push {
            self.path.push(name);
        } else {
            self.path.pop();
        }
        self.sel = 0;
        self.scroll = 0;
    }

    /// Rebuild the current-directory view only when the tree or path changed.
    fn refresh_view(&mut self) {
        if self.view_gen == self.gen && self.view_path == self.path && !self.searching && self.search_query.is_empty() {
            return; // cache hit — nothing to do
        }
        let mut out: Vec<ViewEntry> = Vec::new();
        if self.searching || !self.search_query.is_empty() {
            let q = self.search_query.to_lowercase();
            fn search_node(node: &Node, prefix: &str, query: &str, out: &mut Vec<ViewEntry>) {
                for (k, v) in &node.dirs {
                    let full = if prefix.is_empty() { k.clone() } else { format!("{prefix}/{k}") };
                    if k.to_lowercase().contains(query) {
                        out.push(ViewEntry {
                            name: full.clone(),
                            full_rel_path: full.clone(),
                            size: 0,
                            mtime: None,
                            is_dir: true,
                        });
                    }
                    search_node(v, &full, query, out);
                }
                for (s, v) in &node.files {
                    let full = if prefix.is_empty() { s.clone() } else { format!("{prefix}/{s}") };
                    if s.to_lowercase().contains(query) {
                        out.push(ViewEntry {
                            name: full.clone(),
                            full_rel_path: full,
                            size: v.0,
                            mtime: v.1,
                            is_dir: false,
                        });
                    }
                }
            }
            search_node(&self.tree, "", &q, &mut out);
        } else if let Some(node) = crate::tree::resolve(&self.tree, &self.path) {
            let prefix = self.path.join("/");
            for (k, _) in &node.dirs {
                let full = if prefix.is_empty() { k.clone() } else { format!("{prefix}/{k}") };
                out.push(ViewEntry {
                    name: k.clone(),
                    full_rel_path: full,
                    size: 0,
                    mtime: None,
                    is_dir: true,
                });
            }
            for (s, v) in &node.files {
                let full = if prefix.is_empty() { s.clone() } else { format!("{prefix}/{s}") };
                out.push(ViewEntry {
                    name: s.clone(),
                    full_rel_path: full,
                    size: v.0,
                    mtime: v.1,
                    is_dir: false,
                });
            }
        }
        self.view = out;
        self.view_path = self.path.clone();
        self.view_gen = self.gen;
    }
}

/// Entry point: argument handling, hidden CLI modes, then the TUI loop.
pub fn run_app() -> i32 {
    let mut args: Vec<String> = std::env::args().skip(1).collect();

    // Hidden mode: `plum --check <archive>` prints listing stats and exits.
    if args.first().map(|s| s.as_str()) == Some("--check") {
        return match args.get(1) {
            Some(path) => match native_list::list(Path::new(path)) {
                Ok(entries) => {
                    let files = entries.iter().filter(|e| !e.is_dir).count();
                    let dirs = entries.iter().filter(|e| e.is_dir).count();
                    let bytes: u64 = entries.iter().map(|e| e.size).sum();
                    println!("OK: {files} files, {dirs} dirs, total {bytes} bytes");
                    for e in entries.iter().take(5) {
                        println!("  {} {}", if e.is_dir { "◆" } else { "◇" }, e.path);
                    }
                    0
                }
                Err(msg) => {
                    println!("FAILED: {msg}");
                    1
                }
            },
            None => {
                eprintln!("Usage: plum --check <archive>");
                2
            }
        };
    }

    // Hidden mode: `plum --extract <archive> <dest>` extracts and exits.
    if args.first().map(|s| s.as_str()) == Some("--extract") {
        let (Some(path), Some(dest)) = (args.get(1), args.get(2)) else {
            eprintln!("Usage: plum --extract <archive> <dest>");
            return 2;
        };
        if !crate::elevation::is_elevated() && !crate::elevation::can_write(Path::new(dest)) {
            let relaunch_args: Vec<String> = std::env::args().skip(1).collect();
            match crate::elevation::relaunch_elevated(&relaunch_args) {
                Ok(()) => return 0,
                Err(e) => {
                    eprintln!("Administrator permission denied: {e}");
                    return 1;
                }
            }
        }
        return match native_list::extract(Path::new(path), Path::new(dest), None) {
            Ok(()) => {
                println!("Extracted to {dest}");
                0
            }
            Err(e) => {
                eprintln!("FAILED: {e}");
                1
            }
        };
    }

    // Hidden mode: `plum --extract-single <archive> <entry> <dest>` extracts a single item and exits.
    if args.first().map(|s| s.as_str()) == Some("--extract-single") {
        let (Some(path), Some(entry), Some(dest)) = (args.get(1), args.get(2), args.get(3)) else {
            eprintln!("Usage: plum --extract-single <archive> <entry> <dest>");
            return 2;
        };
        if !crate::elevation::is_elevated() && !crate::elevation::can_write(Path::new(dest)) {
            let relaunch_args: Vec<String> = std::env::args().skip(1).collect();
            match crate::elevation::relaunch_elevated(&relaunch_args) {
                Ok(()) => return 0,
                Err(e) => {
                    eprintln!("Administrator permission denied: {e}");
                    return 1;
                }
            }
        }
        return match native_list::extract_single(Path::new(path), entry, Path::new(dest)) {
            Ok(_) => {
                println!("Extracted to {dest}");
                0
            }
            Err(e) => {
                eprintln!("FAILED: {e}");
                1
            }
        };
    }

    // Hidden mode: `plum --pack <input> <out> <format> [level] [--password <pass>]` compresses and exits.
    if args.first().map(|s| s.as_str()) == Some("--pack") {
        let (Some(input), Some(out), Some(format)) = (args.get(1), args.get(2), args.get(3))
        else {
            eprintln!("Usage: plum --pack <input> <out> <format> [level] [--password <pass>]");
            return 2;
        };
        let level = args.get(4).map(|s| s.as_str()).unwrap_or("normal");
        let mut password: Option<String> = None;
        if let Some(pos) = args.iter().position(|a| a == "--password") {
            if let Some(val) = args.get(pos + 1) {
                password = Some(val.clone());
            }
        }
        let input_path = PathBuf::from(input);
        let out_path = PathBuf::from(out);

        if !crate::elevation::is_elevated()
            && crate::elevation::check_permission_needed(&[input_path.clone()], Some(&out_path))
        {
            let relaunch_args: Vec<String> = std::env::args().skip(1).collect();
            match crate::elevation::relaunch_elevated(&relaunch_args) {
                Ok(()) => return 0,
                Err(e) => {
                    eprintln!("Administrator permission denied: {e}");
                    return 1;
                }
            }
        }

        let mut files: Vec<(PathBuf, String)> = Vec::new();
        if let Err(e) = crate::compress::walk_files(&input_path, &mut files, 0) {
            if e.kind() == std::io::ErrorKind::PermissionDenied && !crate::elevation::is_elevated() {
                let relaunch_args: Vec<String> = std::env::args().skip(1).collect();
                match crate::elevation::relaunch_elevated(&relaunch_args) {
                    Ok(()) => return 0,
                    Err(err) => {
                        eprintln!("Administrator permission denied: {err}");
                        return 1;
                    }
                }
            }
            eprintln!("FAILED: {e}");
            return 1;
        }
        crate::compress::filter_out_target(&mut files, &out_path);
        let total = crate::compress::total_size(&files);
        let counter = Arc::new(AtomicU64::new(0));
        return match crate::compress::compress(&files, total, &out_path, format, level, password.as_deref(), counter) {
            Ok(()) => {
                println!("Written to {out}");
                0
            }
            Err(e) => {
                println!("FAILED: {e}");
                1
            }
        };
    }

    // --compress <path...>: interactive TUI compression mode.
    let compress_request = args.first().map(|s| s.as_str()) == Some("--compress");
    if compress_request {
        args.remove(0);
    }

    let mut auto_start = false;
    let mut auto_close = false;
    let mut custom_out: Option<String> = None;
    let mut custom_format: Option<String> = None;
    let mut custom_level: Option<String> = None;
    let mut custom_password: Option<String> = None;

    let mut remaining_args = Vec::new();
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--auto-start" => auto_start = true,
            "--auto-close" => auto_close = true,
            "--out" => {
                if let Some(val) = iter.next() {
                    custom_out = Some(val);
                }
            }
            "--format" => {
                if let Some(val) = iter.next() {
                    custom_format = Some(val);
                }
            }
            "--level" => {
                if let Some(val) = iter.next() {
                    custom_level = Some(val);
                }
            }
            "--password" => {
                if let Some(val) = iter.next() {
                    custom_password = Some(val);
                }
            }
            _ => remaining_args.push(arg),
        }
    }
    let args = remaining_args;

    if args.is_empty() {
        eprintln!("Usage: plum <archive-or-folder>");
        return 2;
    }

    let targets: Vec<PathBuf> = args
        .iter()
        .map(|s| PathBuf::from(s.trim_matches('"')))
        .filter(|p| p.exists())
        .collect();

    if targets.is_empty() {
        eprintln!("No valid target files or directories found.");
        return 2;
    }

    let target_path = &targets[0];

    // If viewing archive or folder, check if target requires elevation:
    if !compress_request
        && !crate::elevation::is_elevated()
        && crate::elevation::check_permission_needed(&[target_path.clone()], None)
    {
        let raw_args: Vec<String> = std::env::args().skip(1).collect();
        match crate::elevation::relaunch_elevated(&raw_args) {
            Ok(()) => return 0,
            Err(e) => {
                eprintln!("Administrator permission denied: {e}");
                return 1;
            }
        }
    }

    let name = if targets.len() > 1 {
        format!("{} items", targets.len())
    } else {
        target_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "archive".into())
    };

    // Theme config: exe-dir/theme.toml, then %APPDATA%\plum\theme.toml.
    let exe_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.to_path_buf()));
    let appdata = std::env::var("APPDATA")
        .ok()
        .map(|a| Path::new(&a).join("plum"));
    let theme_path = theme::config_candidates(exe_dir.as_deref(), appdata.as_deref());
    theme::init(theme_path.as_deref());

    let is_archive = target_path.is_file();

    let mut st = State::new(name, target_path.to_path_buf(), is_archive);
    st.compress_mode = compress_request;
    st.compress_targets = targets.clone();
    if compress_request {
        // Compression screen: no listing, prefill output path.
        st.reading = false;
        let dir = target_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let stem = if targets.len() == 1 {
            target_path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "archive".into())
        } else {
            dir.file_name()
                .map(|s| s.to_string_lossy().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "archive".into())
        };
        st.out_path = dir
            .join(format!("{}{}", stem, crate::compress::ext_of(crate::compress::FORMATS[0])))
            .to_string_lossy()
            .to_string();
        st.archive_path = target_path.to_path_buf();

        st.auto_close = auto_close;
        if let Some(fmt) = custom_format {
            if let Some(idx) = crate::compress::FORMATS.iter().position(|f| *f == fmt) {
                st.fmt_sel = idx;
                if custom_out.is_none() {
                    update_out_extension(&mut st);
                }
            }
        }
        if let Some(lvl) = custom_level {
            if let Some(idx) = crate::compress::LEVELS.iter().position(|l| *l == lvl) {
                st.lvl_sel = idx;
            }
        }
        if let Some(out) = custom_out {
            st.out_path = out;
        }
        if let Some(pass) = custom_password {
            st.password = pass;
        }
        st.need_elevation = !crate::elevation::is_elevated()
            && crate::elevation::check_permission_needed(&st.compress_targets, Some(Path::new(&st.out_path)));
        if auto_start {
            start_compress(&mut st);
        }
    }

    let terminal = ratatui::init();
    if !compress_request {
        st.listing_rx = Some(if is_archive {
            spawn_archive_listing(target_path.to_path_buf())
        } else {
            spawn_dir_listing(target_path.to_path_buf())
        });
    }
    let result = run(terminal, &mut st);
    let _ = execute!(stdout(), DisableMouseCapture);
    ratatui::restore();
    if result.is_err() {
        return 1;
    }
    0
}

/// Background thread: parse an archive natively, stream entries to the UI.
fn spawn_archive_listing(path: PathBuf) -> Receiver<native_list::Entry> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let dead = std::cell::Cell::new(false);
        let _res = native_list::list_stream(&path, &mut |e| {
            if dead.get() {
                return;
            }
            if tx.send(e).is_err() {
                dead.set(true); // UI closed — stop sinking, stream to the void
            }
        });
        // tx dropped here → UI sees EOF.
    });
    rx
}

/// Background thread: walk a plain directory with the same entry stream.
fn spawn_dir_listing(path: PathBuf) -> Receiver<native_list::Entry> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        fn walk(dir: &Path, rel: String, tx: &mpsc::Sender<native_list::Entry>, depth: u8) {
            if depth > crate::tree::MAX_DEPTH {
                return;
            }
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    let fname = e.file_name().to_string_lossy().to_string();
                    let rel_path = if rel.is_empty() {
                        fname.clone()
                    } else {
                        format!("{rel}/{fname}")
                    };
                    // file_type() avoids PathBuf allocation per entry.
                    match e.file_type() {
                        Ok(ft) if ft.is_dir() => {
                            let _ = tx.send(native_list::Entry {
                                path: rel_path.clone(),
                                is_dir: true,
                                size: 0,
                                mtime: None,
                            });
                            walk(&e.path(), rel_path, tx, depth + 1);
                        }
                        Ok(ft) if ft.is_file() => {
                            if let Ok(md) = e.metadata() {
                                let mtime = md
                                    .modified()
                                    .ok()
                                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                                    .map(|d| d.as_secs() as i64);
                                let _ = tx.send(native_list::Entry {
                                    path: rel_path,
                                    is_dir: false,
                                    size: md.len(),
                                    mtime,
                                });
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        walk(&path, String::new(), &tx, 0);
    });
    rx
}

/// Background thread: native extraction with byte progress.
fn spawn_extract(
    path: PathBuf,
    dest: PathBuf,
    progress: Arc<AtomicU64>,
) -> Receiver<Result<String, String>> {
    let (tx, rx) = mpsc::channel();
    let dest_name = dest
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    std::thread::spawn(move || {
        let res = native_list::extract(&path, &dest, Some(progress)).map(|_| dest_name);
        let _ = tx.send(res);
    });
    rx
}

fn run(mut terminal: DefaultTerminal, st: &mut State) -> Result<(), ()> {
    let _ = execute!(stdout(), EnableMouseCapture);
    loop {
        // Extraction finished?
        if st.extracting {
            if let Some(ex) = st.extract_rx.take() {
                match ex.try_recv() {
                    Ok(Ok(dest)) => {
                        st.extracting = false;
                        let verb = if st.compress_mode { "Compressed to" } else { "Extracted to" };
                        st.status = format!("{verb} {dest}");
                        st.status_kind = 1;
                        if st.auto_close {
                            st.close_delay = Some(4);
                        }
                    }
                    Ok(Err(e)) => {
                        st.extracting = false;
                        let msg = if e.contains("Access is denied") || e.contains("отказано") || e.contains("PermissionDenied") {
                            format!("{e} (требуются права администратора)")
                        } else {
                            format!("Extract failed: {e}")
                        };
                        st.status = msg;
                        st.status_kind = 2;
                    }
                    Err(TryRecvError::Empty) => st.extract_rx = Some(ex),
                    Err(TryRecvError::Disconnected) => {
                        st.extracting = false;
                        st.status = "Extract failed (internal)".into();
                        st.status_kind = 2;
                    }
                }
            } else {
                st.extracting = false;
            }
        }
        // Listing stream progressing?
        if st.reading {
            if let Some(lx) = st.listing_rx.take() {
                let mut done = false;
                loop {
                    match lx.try_recv() {
                        Ok(entry) => {
                            tree_insert(&mut st.tree, &entry);
                            st.entries_read += 1;
                        }
                        Err(TryRecvError::Empty) => {
                            st.listing_rx = Some(lx);
                            break;
                        }
                        Err(TryRecvError::Disconnected) => {
                            st.reading = false;
                            let total = subtree_stats(&st.tree);
                            st.status = format!("{} entries · {}", total.0, fmt_size(total.1));
                            st.status_kind = 1;
                            done = true;
                            break;
                        }
                    }
                    st.gen += 1; // invalidate view cache per inserted entry
                }
                if done {
                    continue; // redraw immediately with final state
                }
            } else {
                st.reading = false;
            }
        }

        // Rebuild the visible-directory cache only when tree or path changed.
        if st.view_gen != st.gen || st.view_path != st.path {
            st.refresh_view();
        }

        terminal.draw(|f| draw(f, st)).map_err(|_| ())?;
        if let Some(delay) = st.close_delay {
            if delay == 0 {
                st.quit = true;
            } else {
                st.close_delay = Some(delay - 1);
            }
        }
        if st.quit {
            return Ok(());
        }
        if event::poll(Duration::from_millis(POLL_MS)).map_err(|_| ())? {
            match event::read().map_err(|_| ())? {
                Event::Key(k) if k.kind == KeyEventKind::Press => on_key(st, k.code),
                Event::Mouse(m) => on_mouse(st, m.kind, m.row),
                _ => {}
            }
        }
        // Spinner advances every loop iteration while busy (even with events
        // flowing), so it never freezes.
        if st.reading || st.extracting {
            st.tick += 1;
        }
    }
}

fn on_key(st: &mut State, code: KeyCode) {
    if st.show_help {
        match code {
            KeyCode::Char('?')
            | KeyCode::Esc
            | KeyCode::Char('q')
            | KeyCode::Char('Q')
            | KeyCode::Enter
            | KeyCode::Char(' ') => {
                st.show_help = false;
            }
            _ => {}
        }
        return;
    }

    if st.compress_mode {
        return on_key_compress(st, code);
    }

    if let Some(prev) = &mut st.preview {
        let vis = (st.list_area.height * 80 / 100).saturating_sub(3) as usize;
        let max_scroll = prev.lines.len().saturating_sub(vis.max(1));
        match code {
            KeyCode::Esc
            | KeyCode::Char('q')
            | KeyCode::Char('Q')
            | KeyCode::Char('p')
            | KeyCode::Char('P')
            | KeyCode::Char(' ') => {
                st.preview = None;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                prev.scroll = (prev.scroll + 1).min(max_scroll);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                prev.scroll = prev.scroll.saturating_sub(1);
            }
            KeyCode::PageDown => {
                prev.scroll = (prev.scroll + 15).min(max_scroll);
            }
            KeyCode::PageUp => {
                prev.scroll = prev.scroll.saturating_sub(15);
            }
            KeyCode::Home | KeyCode::Char('g') => {
                prev.scroll = 0;
            }
            KeyCode::End => {
                prev.scroll = max_scroll;
            }
            _ => {}
        }
        return;
    }

    if st.searching {
        match code {
            KeyCode::Esc => {
                st.searching = false;
                st.search_query.clear();
                st.refresh_view();
                st.sel = 0;
                st.scroll = 0;
            }
            KeyCode::Enter => {
                st.searching = false;
            }
            KeyCode::Backspace => {
                st.search_query.pop();
                st.refresh_view();
                st.sel = 0;
                st.scroll = 0;
            }
            KeyCode::Char(c) => {
                st.search_query.push(c);
                st.refresh_view();
                st.sel = 0;
                st.scroll = 0;
            }
            KeyCode::Down => {
                st.sel = (st.sel + 1).min(st.view.len().saturating_sub(1));
                keep_sel_visible(st);
            }
            KeyCode::Up => {
                st.sel = st.sel.saturating_sub(1);
                keep_sel_visible(st);
            }
            _ => {}
        }
        return;
    }

    let n = st.view.len();
    match code {
        KeyCode::Char('q') | KeyCode::Char('Q') => st.quit = true,
        KeyCode::Esc => {
            if !st.search_query.is_empty() {
                st.search_query.clear();
                st.refresh_view();
                st.sel = 0;
                st.scroll = 0;
            } else if st.path.is_empty() {
                st.quit = true;
            } else {
                st.navigate(None);
            }
        }
        KeyCode::Char('/') => {
            st.searching = true;
        }
        KeyCode::Char(' ') | KeyCode::Char('p') | KeyCode::Char('P') => {
            start_preview(st);
        }
        KeyCode::Char('?') => {
            st.show_help = true;
        }
        KeyCode::Char('x') | KeyCode::Char('X') => {
            if !st.extracting {
                start_extract_single(st);
            }
        }
        KeyCode::Char('e') | KeyCode::Char('E') => {
            if !st.extracting && st.is_archive {
                start_extract(st);
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            st.sel = (st.sel + 1).min(n.saturating_sub(1));
            keep_sel_visible(st);
        }
        KeyCode::Up | KeyCode::Char('k') => {
            st.sel = st.sel.saturating_sub(1);
            keep_sel_visible(st);
        }
        KeyCode::Home | KeyCode::Char('g') => {
            st.sel = 0;
            keep_sel_visible(st);
        }
        KeyCode::End => {
            st.sel = n.saturating_sub(1);
            keep_sel_visible(st);
        }
        KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => match st.view.get(st.sel) {
            Some(e) if e.is_dir => {
                if !st.search_query.is_empty() {
                    let parts: Vec<String> = e.full_rel_path.split('/').map(|s| s.to_string()).collect();
                    st.search_query.clear();
                    st.searching = false;
                    st.path = parts;
                    st.sel = 0;
                    st.scroll = 0;
                    st.refresh_view();
                } else {
                    st.navigate(Some(e.name.clone()));
                }
            }
            Some(_) => {
                start_preview(st);
            }
            None => {}
        },
        KeyCode::Left | KeyCode::Backspace => {
            if !st.search_query.is_empty() {
                st.search_query.clear();
                st.refresh_view();
                st.sel = 0;
                st.scroll = 0;
            } else if !st.path.is_empty() {
                st.navigate(None);
            }
        }
        _ => {}
    }
}

/// Keep the keyboard-selected row inside the visible window (arrows only —
/// wheel scrolling is free).
fn keep_sel_visible(st: &mut State) {
    let vis = st.list_area.height as usize;
    if vis == 0 {
        return;
    }
    if st.sel < st.scroll {
        st.scroll = st.sel;
    } else if st.sel >= st.scroll + vis {
        st.scroll = st.sel + 1 - vis;
    }
}

fn start_preview(st: &mut State) {
    if st.view.is_empty() || st.sel >= st.view.len() {
        return;
    }
    let entry = &st.view[st.sel];
    if entry.is_dir {
        st.status = format!("Folder: {} (Enter to open)", entry.name);
        st.status_kind = 0;
        return;
    }

    let rel_path = entry.full_rel_path.clone();
    let name = entry.name.clone();
    let size = entry.size;

    let res = if st.is_archive {
        native_list::read_entry_preview(&st.archive_path, &rel_path, 64 * 1024)
    } else {
        use std::io::Read;
        let full = st.archive_path.join(&rel_path);
        std::fs::File::open(&full)
            .map_err(|e| e.to_string())
            .and_then(|mut f| {
                let mut buf = vec![0u8; 64 * 1024];
                let mut total = 0;
                while total < buf.len() {
                    let n = f.read(&mut buf[total..]).map_err(|e| e.to_string())?;
                    if n == 0 {
                        break;
                    }
                    total += n;
                }
                buf.truncate(total);
                Ok(buf)
            })
    };

    match res {
        Ok(bytes) => {
            let is_binary = bytes.iter().take(1024).any(|&b| b == 0);
            let lines: Vec<String> = if is_binary {
                let mut out = vec![
                    format!("[ Binary file — {} ]", fmt_size(size)),
                    "".into(),
                ];
                let dump_len = bytes.len().min(256);
                for chunk_idx in (0..dump_len).step_by(16) {
                    let chunk = &bytes[chunk_idx..(chunk_idx + 16).min(dump_len)];
                    let mut hex_part = String::new();
                    let mut ascii_part = String::new();
                    for b in chunk {
                        use std::fmt::Write;
                        let _ = write!(hex_part, "{:02x} ", b);
                        if b.is_ascii_graphic() || *b == b' ' {
                            ascii_part.push(*b as char);
                        } else {
                            ascii_part.push('.');
                        }
                    }
                    out.push(format!("{:04x}: {:<48} |{}", chunk_idx, hex_part, ascii_part));
                }
                if bytes.len() > 256 {
                    out.push(format!("... ({} more bytes)", bytes.len() - 256));
                }
                out
            } else {
                let s = String::from_utf8_lossy(&bytes);
                s.lines().map(|l| l.to_string()).collect()
            };

            st.preview = Some(PreviewState {
                name,
                lines,
                scroll: 0,
                is_binary,
                size,
            });
        }
        Err(e) => {
            st.status = format!("Preview error: {e}");
            st.status_kind = 2;
        }
    }
}

fn start_extract_single(st: &mut State) {
    if st.view.is_empty() || st.sel >= st.view.len() {
        return;
    }
    let entry = &st.view[st.sel];
    let rel_path = entry.full_rel_path.clone();
    let item_name = entry.name.clone();

    let Some(dest) = rfd::FileDialog::new()
        .set_title(&format!("Extract '{item_name}' to..."))
        .pick_folder()
    else {
        return;
    };

    let dest_name = dest
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| dest.to_string_lossy().to_string());

    let archive_path = st.archive_path.clone();
    let is_archive = st.is_archive;

    // Check if destination requires administrator privileges
    if !crate::elevation::is_elevated() && !crate::elevation::can_write(&dest) {
        st.extracting = true;
        st.extract_total = 0;
        st.extract_progress = None;
        st.status = "Запрос разрешения администратора (UAC)…".into();
        st.status_kind = 0;

        let (tx, rx) = mpsc::channel();
        let dest_str = dest.to_string_lossy().to_string();
        let archive_str = archive_path.to_string_lossy().to_string();
        let dest_display = dest_name.clone();

        std::thread::spawn(move || {
            let res = if is_archive {
                let relaunch_args = vec![
                    "--extract-single".to_string(),
                    archive_str,
                    rel_path,
                    dest_str,
                ];
                crate::elevation::run_elevated_wait(&relaunch_args)
                    .map(|_| dest_display)
            } else {
                Err("Требуются права администратора для копирования".into())
            };
            let _ = tx.send(res);
        });

        st.extract_rx = Some(rx);
        return;
    }

    st.extracting = true;
    st.extract_total = entry.size;
    let counter = Arc::new(AtomicU64::new(0));
    st.extract_progress = Some(counter.clone());
    st.status = format!("Extracting '{item_name}'…");
    st.status_kind = 0;

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let res = if is_archive {
            native_list::extract_single(&archive_path, &rel_path, &dest)
                .map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default())
        } else {
            let src = archive_path.join(&rel_path);
            let fname = Path::new(&rel_path).file_name().unwrap_or(Path::new(&rel_path).as_os_str());
            let target = dest.join(fname);
            if src.is_dir() {
                std::fs::create_dir_all(&target)
                    .map(|_| target.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default())
                    .map_err(|e| e.to_string())
            } else {
                std::fs::copy(&src, &target)
                    .map(|_| target.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default())
                    .map_err(|e| e.to_string())
            }
        };
        let _ = tx.send(res);
    });

    st.extract_rx = Some(rx);
}

/// Blocks the event loop while the dialog is open — acceptable for TUI.
fn start_extract(st: &mut State) {
    let Some(dest) = rfd::FileDialog::new()
        .set_title("Choose destination folder")
        .pick_folder()
    else {
        return;
    };

    let dest_name = dest
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| dest.to_string_lossy().to_string());

    let archive_path = st.archive_path.clone();

    // Check if destination requires administrator privileges
    if !crate::elevation::is_elevated() && !crate::elevation::can_write(&dest) {
        st.extracting = true;
        st.extract_total = 0;
        st.extract_progress = None;
        st.status = "Запрос разрешения администратора (UAC)…".into();
        st.status_kind = 0;

        let (tx, rx) = mpsc::channel();
        let dest_str = dest.to_string_lossy().to_string();
        let archive_str = archive_path.to_string_lossy().to_string();
        let dest_display = dest_name.clone();

        std::thread::spawn(move || {
            let relaunch_args = vec![
                "--extract".to_string(),
                archive_str,
                dest_str,
            ];
            let res = crate::elevation::run_elevated_wait(&relaunch_args)
                .map(|_| dest_display);
            let _ = tx.send(res);
        });

        st.extract_rx = Some(rx);
        return;
    }

    let counter = Arc::new(AtomicU64::new(0));
    st.extract_total = std::fs::metadata(&st.archive_path)
        .map(|m| m.len())
        .unwrap_or(0);
    st.extracting = true;
    st.extract_progress = Some(counter.clone());
    st.status = format!("Extracting to {}…", dest_name);
    st.status_kind = 0;
    st.extract_rx = Some(spawn_extract(st.archive_path.clone(), dest, counter));
}

fn update_out_extension(st: &mut State) {
    let target_ext = crate::compress::ext_of(crate::compress::FORMATS[st.fmt_sel]);
    for fmt in crate::compress::FORMATS {
        let ext = crate::compress::ext_of(fmt);
        if st.out_path.ends_with(ext) {
            st.out_path.truncate(st.out_path.len() - ext.len());
            break;
        }
    }
    st.out_path.push_str(target_ext);
}

fn on_key_compress(st: &mut State, code: KeyCode) {
    if st.editing_password {
        match code {
            KeyCode::Enter | KeyCode::Esc => {
                st.editing_password = false;
            }
            KeyCode::Backspace => {
                st.password.pop();
            }
            KeyCode::Char(c) => {
                if c != '\n' && c != '\r' {
                    st.password.push(c);
                } else {
                    st.editing_password = false;
                }
            }
            _ => {}
        }
        return;
    }

    match code {
        KeyCode::Char('?') => st.show_help = true,
        KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => st.quit = true,
        KeyCode::Char('p') | KeyCode::Char('P') => {
            st.editing_password = true;
        }
        KeyCode::Char('v') | KeyCode::Char('V') => {
            st.show_password = !st.show_password;
        }
        KeyCode::Char('c') | KeyCode::Char('C') => {
            st.password.clear();
        }
        KeyCode::Down | KeyCode::Char('j') => {
            st.fmt_sel = (st.fmt_sel + 1).min(crate::compress::FORMATS.len() - 1);
            update_out_extension(st);
        }
        KeyCode::Up | KeyCode::Char('k') => {
            st.fmt_sel = st.fmt_sel.saturating_sub(1);
            update_out_extension(st);
        }
        KeyCode::Left | KeyCode::Char('h') => {
            st.lvl_sel = st.lvl_sel.saturating_sub(1);
        }
        KeyCode::Right | KeyCode::Char('l') => {
            st.lvl_sel = (st.lvl_sel + 1).min(crate::compress::LEVELS.len() - 1);
        }
        KeyCode::Enter => start_compress(st),
        _ => {}
    }
}

/// Blocks the event loop while the dialog is open — acceptable for TUI.
fn start_compress(st: &mut State) {
    let format = crate::compress::FORMATS[st.fmt_sel];
    let level = crate::compress::LEVELS[st.lvl_sel];
    let out = PathBuf::from(&st.out_path);
    let inputs = if !st.compress_targets.is_empty() {
        st.compress_targets.clone()
    } else {
        vec![st.archive_path.clone()]
    };

    if !st.password.is_empty() && format.starts_with("tar") {
        st.status = "Формат tar.* не поддерживает шифрование. Выберите zip или 7z.".into();
        st.status_kind = 2;
        return;
    }

    // Before attempting to archive, test if Administrator permission is required
    if !crate::elevation::is_elevated()
        && crate::elevation::check_permission_needed(&inputs, Some(&out))
    {
        st.status = "Запрос разрешения администратора (UAC)…".into();
        st.status_kind = 0;

        let mut relaunch_args = vec!["--compress".to_string()];
        for inp in &inputs {
            relaunch_args.push(inp.to_string_lossy().to_string());
        }
        relaunch_args.push("--out".to_string());
        relaunch_args.push(st.out_path.clone());
        relaunch_args.push("--format".to_string());
        relaunch_args.push(format.to_string());
        relaunch_args.push("--level".to_string());
        relaunch_args.push(level.to_string());
        if !st.password.is_empty() {
            relaunch_args.push("--password".to_string());
            relaunch_args.push(st.password.clone());
        }
        relaunch_args.push("--auto-start".to_string());

        match crate::elevation::relaunch_elevated(&relaunch_args) {
            Ok(()) => {
                st.quit = true;
                return;
            }
            Err(_) => {
                st.status = "Отказано в разрешении администратора".into();
                st.status_kind = 2;
                return;
            }
        }
    }

    if out.exists() && !st.confirm_overwrite {
        st.confirm_overwrite = true;
        st.status = "Output exists — press Enter again to overwrite".into();
        st.status_kind = 2;
        return;
    }
    st.confirm_overwrite = false;
    st.extracting = true;
    st.status = format!("Compressing to {}…", out.to_string_lossy());
    st.status_kind = 0;
    let counter = Arc::new(AtomicU64::new(0));
    st.extract_progress = Some(counter.clone());
    st.extract_total = 0; // computed in the worker after the walk
    let password_opt = if st.password.is_empty() {
        None
    } else {
        Some(st.password.clone())
    };
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut files: Vec<(PathBuf, String)> = Vec::new();
        let preserve_dir = inputs.len() > 1;
        for input in &inputs {
            if let Err(e) = crate::compress::walk_files_with_prefix(input, &mut files, 0, preserve_dir) {
                if e.kind() == std::io::ErrorKind::PermissionDenied {
                    let is_unc = inputs.iter().any(|p| crate::elevation::is_unc_or_network(p))
                        || crate::elevation::is_unc_or_network(&out);
                    let msg = if is_unc {
                        "отказано в доступе (проверьте права доступа к сетевой папке/WSL)".to_string()
                    } else {
                        "отказано в доступе (требуются права администратора)".to_string()
                    };
                    let _ = tx.send(Err(msg));
                    return;
                }
            }
        }
        crate::compress::filter_out_target(&mut files, &out);
        let total = crate::compress::total_size(&files);
        let res = crate::compress::compress(&files, total, &out, format, level, password_opt.as_deref(), counter)
            .map(|_| out.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| out.to_string_lossy().to_string()));
        let _ = tx.send(res);
    });
    st.extract_rx = Some(rx);
}

fn on_mouse(st: &mut State, kind: MouseEventKind, row: u16) {
    if st.show_help {
        if let MouseEventKind::Down(_) = kind {
            st.show_help = false;
        }
        return;
    }
    if st.compress_mode {
        return; // wheel/mouse on the format list — arrows are enough
    }
    if let Some(prev) = &mut st.preview {
        let vis = (st.list_area.height * 80 / 100).saturating_sub(3) as usize;
        let max_scroll = prev.lines.len().saturating_sub(vis.max(1));
        match kind {
            MouseEventKind::ScrollUp => {
                prev.scroll = prev.scroll.saturating_sub(WHEEL_SCROLL);
            }
            MouseEventKind::ScrollDown => {
                prev.scroll = (prev.scroll + WHEEL_SCROLL).min(max_scroll);
            }
            MouseEventKind::Down(button) => {
                if button == MouseButton::Right {
                    st.preview = None;
                }
            }
            _ => {}
        }
        return;
    }
    match kind {
        MouseEventKind::Down(button) => {
            let area = st.list_area;
            if area.height > 0 && row >= area.y {
                if button == MouseButton::Right {
                    // Right-click anywhere in the list = navigate up.
                    if !st.path.is_empty() {
                        st.navigate(None);
                    }
                    return;
                }
                let idx = st.scroll + (row - area.y) as usize;
                if idx < st.view.len() {
                    if st.view[idx].is_dir {
                        let name = st.view[idx].name.clone();
                        st.navigate(Some(name));
                    } else {
                        st.sel = idx;
                    }
                }
            }
        }
        MouseEventKind::ScrollUp => st.scroll = st.scroll.saturating_sub(WHEEL_SCROLL),
        MouseEventKind::ScrollDown => {
            let vis = st.list_area.height as usize;
            if vis > 0 {
                let max = st.view.len().saturating_sub(vis);
                st.scroll = (st.scroll + WHEEL_SCROLL).min(max);
            }
        }
        _ => {}
    }
}

fn draw(f: &mut Frame, st: &mut State) {
    let area = f.area();
    let t = theme::t();
    let (bg, surface2, surface_text, text, dim, meta, accent, ok, err) =
        (t.bg, t.surface2, t.surface_text, t.text, t.dim, t.meta, t.accent, t.ok, t.err);

    // Header: wordmark + breadcrumb + busy indicator.
    let crumbs = if st.path.is_empty() {
        st.archive_name.clone()
    } else {
        st.path.join(" / ")
    };
    let busy = st.reading || st.extracting;
    let spinner = if busy {
        format!(" {} ", SPINNER[st.tick % SPINNER.len()])
    } else {
        String::new()
    };
    let busy_label = if st.extracting {
        "  extracting…"
    } else if st.reading {
        "  unpacking…"
    } else {
        ""
    };
    let search_bar = if st.searching {
        format!("  / {}█ ", st.search_query)
    } else if !st.search_query.is_empty() {
        format!("  / {} ({} matches) ", st.search_query, st.view.len())
    } else {
        String::new()
    };
    let mut header_spans = vec![
        Span::styled(
            " plum ",
            Style::new().bg(accent).fg(bg).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" ", Style::new().bg(accent)),
        Span::styled(crumbs, Style::new().fg(dim).bg(accent)),
    ];
    if !search_bar.is_empty() {
        header_spans.push(Span::styled(
            search_bar,
            Style::new().fg(accent).bg(surface2).add_modifier(Modifier::BOLD),
        ));
    }
    header_spans.push(Span::styled(
        busy_label,
        Style::new().fg(bg).bg(accent).add_modifier(Modifier::BOLD),
    ));
    header_spans.push(Span::styled(spinner, Style::new().fg(accent)));
    let header = Line::from(header_spans);
    f.render_widget(Paragraph::new(header), Rect { height: 1, ..area });

    let list = Rect {
        y: 1,
        height: area.height.saturating_sub(3),
        ..area
    };
    st.list_area = list;

    // ---- Compress mode: format picker + output path ----------------------
    if st.compress_mode {
        let mut lines: Vec<Line> = Vec::new();
        if st.compress_targets.len() > 1 {
            lines.push(Line::styled(
                format!(" Input: {} items selected", st.compress_targets.len()),
                Style::new().fg(dim),
            ));
        } else {
            lines.push(Line::styled(
                format!(" Input: {}", st.archive_path.to_string_lossy()),
                Style::new().fg(dim),
            ));
        }
        lines.push(Line::styled(
            format!(" Output: {}", st.out_path),
            Style::new().fg(meta),
        ));
        if st.need_elevation {
            lines.push(Line::styled(
                " 🛡 Папка защищена администратором (при сжатии будет запрошен UAC)",
                Style::new().fg(err),
            ));
        }
        lines.push(Line::from(""));
        for (i, fmt) in crate::compress::FORMATS.iter().enumerate() {
            let selected = i == st.fmt_sel;
            let style = Style::new()
                .fg(if selected { surface_text } else { dim })
                .bg(if selected { surface2 } else { bg })
                .add_modifier(if selected { Modifier::BOLD } else { Modifier::empty() });
            let icon = if i == st.fmt_sel { "◆ " } else { "◇ " };
            lines.push(Line::from(vec![
                Span::styled(" ", style),
                Span::styled(icon, Style::new().fg(accent).bg(if selected { surface2 } else { bg })),
                Span::styled(fmt.to_string(), style),
            ]));
        }

        lines.push(Line::from(""));
        let mut lvl_spans = vec![
            Span::styled(" Level:  ", Style::new().fg(dim)),
        ];
        let current_fmt = crate::compress::FORMATS[st.fmt_sel];
        if current_fmt == "tar" {
            lvl_spans.push(Span::styled("N/A (tar is uncompressed)", Style::new().fg(dim)));
        } else {
            for (i, &lvl) in crate::compress::LEVELS.iter().enumerate() {
                let selected = i == st.lvl_sel;
                let label = crate::compress::level_label(lvl);
                let style = Style::new()
                    .fg(if selected { surface_text } else { dim })
                    .bg(if selected { surface2 } else { bg })
                    .add_modifier(if selected { Modifier::BOLD } else { Modifier::empty() });
                let icon = if selected { "◆ " } else { "◇ " };
                lvl_spans.push(Span::styled(format!("[ {icon}{label} ]"), style));
                lvl_spans.push(Span::styled("  ", Style::new().bg(bg)));
            }
        }
        lines.push(Line::from(lvl_spans));

        lines.push(Line::from(""));
        let mut pass_spans = vec![
            Span::styled(" Password: ", Style::new().fg(dim)),
        ];
        if st.editing_password {
            let display_pass = if st.show_password {
                st.password.clone()
            } else {
                "•".repeat(st.password.chars().count())
            };
            pass_spans.push(Span::styled(
                format!("[ {display_pass}█ ]"),
                Style::new().fg(accent).bg(surface2).add_modifier(Modifier::BOLD),
            ));
            pass_spans.push(Span::styled(" (нажмите Enter для сохранения)", Style::new().fg(dim)));
        } else if st.password.is_empty() {
            pass_spans.push(Span::styled(
                "[ Без пароля ]",
                Style::new().fg(dim).bg(surface2),
            ));
            pass_spans.push(Span::styled(" (нажмите 'p' для установки)", Style::new().fg(dim)));
        } else {
            let display_pass = if st.show_password {
                st.password.clone()
            } else {
                "•".repeat(st.password.chars().count())
            };
            pass_spans.push(Span::styled(
                format!("[ {display_pass} ]"),
                Style::new().fg(accent).bg(surface2).add_modifier(Modifier::BOLD),
            ));
            if current_fmt == "zip" || current_fmt == "7z" {
                pass_spans.push(Span::styled(" 🔒 AES-256", Style::new().fg(accent).add_modifier(Modifier::BOLD)));
            } else {
                pass_spans.push(Span::styled(" ⚠ пароль не поддерживается для tar.*", Style::new().fg(err).add_modifier(Modifier::BOLD)));
            }
            pass_spans.push(Span::styled(
                if st.show_password { " ('v' скрыть, 'c' удалить, 'p' изменить)" } else { " ('v' показать, 'c' удалить, 'p' изменить)" },
                Style::new().fg(dim),
            ));
        }
        lines.push(Line::from(pass_spans));

        if st.extracting {
            lines.push(Line::from(""));
            lines.push(Line::styled(
                format!(
                    "{} Compressing…",
                    SPINNER[st.tick % SPINNER.len()]
                ),
                Style::new().fg(accent),
            ));
        }
        f.render_widget(Paragraph::new(lines), list);

        let foot_y = area.height.saturating_sub(2);
        let (status, color) = if st.extracting {
            let done = st
                .extract_progress
                .as_ref()
                .map(|c| c.load(Ordering::Relaxed))
                .unwrap_or(0);
            let pct = if st.extract_total > 0 {
                (done * 100 / st.extract_total).min(100)
            } else {
                0
            };
            (
                format!(" {} / {} ({}%)", fmt_size(done), fmt_size(st.extract_total), pct),
                accent,
            )
        } else {
            (st.status.clone(), if st.status_kind == 2 { err } else { dim })
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" {status}"),
                Style::new().fg(color),
            ))),
            Rect { y: foot_y, height: 1, ..area },
        );
        if st.editing_password {
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    " Режим ввода пароля · Enter сохранить · Esc отмена · Backspace стереть",
                    Style::new().fg(accent).add_modifier(Modifier::BOLD),
                ))),
                Rect { y: foot_y + 1, height: 1, ..area },
            );
        } else {
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    " ↑↓ формат · ←→ степень · p пароль · Enter сжать · ? справка · Esc выход".to_string(),
                    Style::new().fg(dim),
                ))),
                Rect { y: foot_y + 1, height: 1, ..area },
            );
        }
        if st.show_help {
            render_help_modal(f, st, area);
        }
        return;
    }

    let ents = &st.view;
    // Free-scroll: clamp into the valid range, never snap to the selection
    // here — the wheel must be able to move the list independently.
    let vis = list.height as usize;
    let max_scroll = ents.len().saturating_sub(vis);
    st.scroll = st.scroll.min(max_scroll);

    let mut lines: Vec<Line> = Vec::new();
    if let Some(e) = &st.error {
        lines.push(Line::styled(
            format!("Error reading archive: {e}"),
            Style::new().fg(err),
        ));
    } else if ents.is_empty() {
        let msg = if st.reading {
            format!(
                "{} Reading archive… {} entries so far",
                SPINNER[st.tick % SPINNER.len()],
                st.entries_read
            )
        } else {
            "Empty directory".into()
        };
        lines.push(Line::styled(msg, Style::new().fg(dim)));
    } else {
        let end = (st.scroll + vis).min(ents.len());
        for (idx, e) in ents[st.scroll..end].iter().enumerate() {
            let selected = st.scroll + idx == st.sel;
            let info = if e.is_dir {
                // Cached: O(1) per node after first computation.
                let stats = resolve(&st.tree, &st.path)
                    .and_then(|n| n.dirs.get(&e.name))
                    .map(subtree_stats)
                    .unwrap_or((0, 0));
                format!("{} items · {}", stats.0, fmt_size(stats.1))
            } else {
                fmt_size(e.size)
            };
            let date = fmt_date(e.mtime);
            let base = Style::new()
                .fg(if selected { surface_text } else if e.is_dir { text } else { dim })
                .bg(if selected { surface2 } else { bg })
                .add_modifier(if e.is_dir { Modifier::BOLD } else { Modifier::empty() });
            let meta = Style::new()
                .fg(if selected { surface_text } else { meta })
                .bg(if selected { surface2 } else { bg });
            lines.push(Line::from(vec![
                Span::styled(" ", base),
                Span::styled(
                    if e.is_dir { "◆ " } else { "◇ " },
                    Style::new()
                        .fg(if selected { surface_text } else { accent })
                        .bg(if selected { surface2 } else { bg }),
                ),
                Span::styled(e.name.to_string(), base),
                Span::styled(format!("  {info:>14}  {date:<10}"), meta),
            ]));
        }
    }
    f.render_widget(Paragraph::new(lines), list);

    // Footer.
    let foot_y = area.height.saturating_sub(2);
    let (status, kind_ok) = if let Some(e) = &st.error {
        (e.clone(), false)
    } else if st.extracting {
        let done = st
            .extract_progress
            .as_ref()
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(0);
        let pct = if st.extract_total > 0 {
            (done * 100 / st.extract_total).min(100)
        } else {
            0
        };
        (
            format!(
                "{} · {} / {} ({}%)",
                st.status,
                fmt_size(done),
                fmt_size(st.extract_total),
                pct
            ),
            true,
        )
    } else if st.reading {
        (format!("Reading… {} entries", st.entries_read), true)
    } else {
        (st.status.clone(), st.status_kind != 2)
    };
    let status_color = if !kind_ok {
        err
    } else if busy {
        accent
    } else {
        ok
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {}/{}  ·  {}", st.sel + 1, ents.len(), status),
            Style::new().fg(status_color),
        ))),
        Rect { y: foot_y, height: 1, ..area },
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " ↑↓ move · Enter open · Space preview · / search · x extract · ? help · E extract all · q quit".to_string(),
            Style::new().fg(dim),
        ))),
        Rect { y: foot_y + 1, height: 1, ..area },
    );

    // Floating preview modal popup
    if let Some(prev) = &st.preview {
        let modal_width = (area.width * 85 / 100).max(30).min(area.width);
        let modal_height = (area.height * 80 / 100).max(10).min(area.height);
        let modal_x = area.x + (area.width.saturating_sub(modal_width)) / 2;
        let modal_y = area.y + (area.height.saturating_sub(modal_height)) / 2;
        let modal_area = Rect {
            x: modal_x,
            y: modal_y,
            width: modal_width,
            height: modal_height,
        };

        f.render_widget(Clear, modal_area);

        let title = format!(
            " Preview: {} ({}) [{}] ",
            prev.name,
            fmt_size(prev.size),
            if prev.is_binary { "Binary" } else { "Text" }
        );
        let block = Block::bordered()
            .title(Span::styled(title, Style::new().fg(accent).add_modifier(Modifier::BOLD)))
            .border_style(Style::new().fg(accent));
        f.render_widget(block, modal_area);

        let inner = Rect {
            x: modal_area.x + 1,
            y: modal_area.y + 1,
            width: modal_area.width.saturating_sub(2),
            height: modal_area.height.saturating_sub(3),
        };

        let vis_lines = inner.height as usize;
        let end_idx = (prev.scroll + vis_lines).min(prev.lines.len());
        let slice = if prev.lines.is_empty() {
            &[]
        } else {
            &prev.lines[prev.scroll..end_idx]
        };

        let mut p_lines: Vec<Line> = Vec::new();
        for (i, line_str) in slice.iter().enumerate() {
            let line_num = prev.scroll + i + 1;
            let num_span = Span::styled(format!("{:4} │ ", line_num), Style::new().fg(dim));
            let text_span = Span::styled(line_str.clone(), Style::new().fg(text));
            p_lines.push(Line::from(vec![num_span, text_span]));
        }
        f.render_widget(Paragraph::new(p_lines), inner);

        let modal_foot = Rect {
            x: modal_area.x + 1,
            y: modal_area.y + modal_area.height.saturating_sub(2),
            width: modal_area.width.saturating_sub(2),
            height: 1,
        };
        let pct = if prev.lines.len() > vis_lines {
            (prev.scroll * 100 / (prev.lines.len() - vis_lines)).min(100)
        } else {
            100
        };
        let foot_text = format!(
            " Line {}/{} ({}%)  ·  ↑↓/PgUp/PgDn scroll · Esc/Space/q close ",
            (prev.scroll + 1).min(prev.lines.len()),
            prev.lines.len(),
            pct
        );
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(foot_text, Style::new().fg(meta)))),
            modal_foot,
        );
    }

    // Floating help modal popup
    if st.show_help {
        render_help_modal(f, st, area);
    }
}

fn render_help_modal(f: &mut Frame, st: &State, area: Rect) {
    let t = theme::t();
    let (accent, text, meta) = (t.accent, t.text, t.meta);

    let help_width = 72.min(area.width.saturating_sub(4));
    let help_height = if st.compress_mode { 14 } else { 17 }.min(area.height.saturating_sub(2));
    let modal_x = area.x + (area.width.saturating_sub(help_width)) / 2;
    let modal_y = area.y + (area.height.saturating_sub(help_height)) / 2;
    let modal_area = Rect {
        x: modal_x,
        y: modal_y,
        width: help_width,
        height: help_height,
    };

    f.render_widget(Clear, modal_area);

    let title = " Справка по горячим клавишам ";
    let block = Block::bordered()
        .title(Span::styled(title, Style::new().fg(accent).add_modifier(Modifier::BOLD)))
        .border_style(Style::new().fg(accent));
    f.render_widget(block, modal_area);

    let inner = Rect {
        x: modal_area.x + 2,
        y: modal_area.y + 1,
        width: modal_area.width.saturating_sub(4),
        height: modal_area.height.saturating_sub(3),
    };

    let mut lines: Vec<Line> = Vec::new();
    if st.compress_mode {
        let shortcuts = [
            ("↑ / ↓, j / k", "Выбор формата архива (zip, 7z, tar.zst...)"),
            ("← / →, h / l", "Степень сжатия (fastest, normal, ultra...)"),
            ("p / P", "Установить или изменить пароль (AES-256)"),
            ("v / V", "Показать / скрыть символы пароля"),
            ("c / C", "Удалить установленный пароль"),
            ("Enter", "Запустить сжатие"),
            ("?", "Показать / скрыть эту справку"),
            ("Esc / q", "Отмена и выход"),
        ];
        for (key, desc) in shortcuts {
            lines.push(Line::from(vec![
                Span::styled(format!("{:<18}", key), Style::new().fg(accent).add_modifier(Modifier::BOLD)),
                Span::styled(desc, Style::new().fg(text)),
            ]));
        }
    } else {
        let shortcuts = [
            ("↑ / ↓, j / k", "Перемещение по списку файлов"),
            ("Home / End", "В начало / в конец списка"),
            ("Enter / → / l", "Войти в папку / открыть предпросмотр"),
            ("← / Backspace", "На уровень вверх"),
            ("Space / p", "Быстрый предпросмотр (текст / HEX-дамп)"),
            ("/", "Поиск и фильтрация по содержимому"),
            ("x", "Извлечь только выбранный файл или папку"),
            ("e / E", "Извлечь весь архив целиком"),
            ("RMB (ПКМ)", "На уровень вверх (или закрыть окно)"),
            ("?", "Показать / скрыть эту справку"),
            ("Esc / q", "Очистить поиск / закрыть окно / выход"),
        ];
        for (key, desc) in shortcuts {
            lines.push(Line::from(vec![
                Span::styled(format!("{:<18}", key), Style::new().fg(accent).add_modifier(Modifier::BOLD)),
                Span::styled(desc, Style::new().fg(text)),
            ]));
        }
    }

    f.render_widget(Paragraph::new(lines), inner);

    let modal_foot = Rect {
        x: modal_area.x + 1,
        y: modal_area.y + modal_area.height.saturating_sub(2),
        width: modal_area.width.saturating_sub(2),
        height: 1,
    };
    let foot_text = " Нажмите ?, Esc, Enter или Space для закрытия ";
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(foot_text, Style::new().fg(meta)))),
        modal_foot,
    );
}

// ---- app.rs ends here ------                                                 