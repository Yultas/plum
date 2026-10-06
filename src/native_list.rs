//! Native archive listing & extraction — no ouch, no WSL.
//! Supports tar.* (gz/xz/zst/lz4/bz2) and zip; raw single-file streams
//! (file.gz etc) are treated as one pseudo-entry.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// One entry of an archive listing.
pub struct Entry {
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: Option<i64>, // unix seconds
}

fn wrap<R: Read + Send + 'static>(
    reader: R,
    exts: &[&str],
) -> Result<Box<dyn Read + Send>, String> {
    let mut tail: &[&str] = exts;
    let mut cur: Box<dyn Read + Send> = Box::new(BufReader::new(reader));
    // Outermost first: for tar.gz exts = ["gz", "tar"] → gunzip, then tar sees plain.
    while let Some((first, rest)) = tail.split_first() {
        tail = rest;
        cur = match *first {
            "gz" | "tgz" => Box::new(flate2::read::MultiGzDecoder::new(cur)),
            "xz" => Box::new(xz2::read::XzDecoder::new(cur)),
            "bz2" => Box::new(bzip2::read::MultiBzDecoder::new(cur)),
            "zst" => Box::new(zstd::stream::read::Decoder::new(cur).map_err(|e| e.to_string())?),
            "lz4" => Box::new(lz4_flex::frame::FrameDecoder::new(cur)),
            _ => break,
        };
        if *first == "lz4" {
            break; // lz4_flex FrameDecoder reads a single frame.
        }
    }
    Ok(cur)
}

enum Detected {
    Tar(Vec<&'static str>),
    Zip,
    SevenZip,
    Rar,
    /// Raw single-file stream + bare base name (original case).
    Raw(Vec<&'static str>, String),
}

/// Scan extensions from the END: [gz|xz|zst|lz4|bz2]+ over (tar | zip | tgz | 7z | rar).
/// e.g. "archive.tar.gz": parts = [archive, tar, gz]; gz → comp, base = tar.
fn detect(file_name: &str) -> Detected {
    let lower = file_name.to_lowercase();
    let parts: Vec<&str> = lower.split('.').collect();
    const COMPRESSIONS: [&str; 5] = ["gz", "xz", "zst", "lz4", "bz2"];
    let mut comp: Vec<&'static str> = Vec::new();
    let mut end = parts.len();
    while end >= 2 && COMPRESSIONS.contains(&parts[end - 1]) {
        comp.insert(
            0,
            COMPRESSIONS
                .iter()
                .find(|c| **c == parts[end - 1])
                .unwrap(),
        );
        end -= 1;
    }
    let base = parts.get(end.wrapping_sub(1)).copied().unwrap_or("");
    match base {
        "tar" => Detected::Tar(comp),
        "tgz" => {
            comp.insert(0, "gz");
            Detected::Tar(comp)
        }
        "zip" => Detected::Zip,
        "7z" => Detected::SevenZip,
        "rar" => Detected::Rar,
        _ => {
            // Bare name with original case: strip compression suffixes.
            let mut name = file_name.to_string();
            for c in &comp {
                let suffix = format!(".{c}");
                if name.len() >= suffix.len()
                    && name[name.len() - suffix.len()..].eq_ignore_ascii_case(&suffix)
                {
                    name.truncate(name.len() - suffix.len());
                }
            }
            Detected::Raw(comp, name)
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// List an archive natively, collecting all entries (for CLI checks).
pub fn list(path: &Path) -> Result<Vec<Entry>, String> {
    let mut out = Vec::new();
    list_stream(path, &mut |e| out.push(e))?;
    Ok(out)
}

/// Stream entries one-by-one without buffering the whole listing.
pub fn list_stream(path: &Path, sink: &mut dyn FnMut(Entry)) -> Result<(), String> {
    match detect(&file_name(path)) {
        Detected::Tar(comp) => {
            let f = File::open(path).map_err(|e| e.to_string())?;
            let reader = wrap(f, &comp)?;
            let mut ar = tar::Archive::new(reader);
            for e in ar.entries().map_err(|e| e.to_string())? {
                let e = e.map_err(|e| e.to_string())?;
                let p = e
                    .path()
                    .map_err(|e| e.to_string())?
                    .to_string_lossy()
                    .trim_end_matches('/')
                    .replace('\\', "/");
                if p.is_empty() {
                    continue;
                }
                sink(Entry {
                    path: p,
                    is_dir: e.header().entry_type().is_dir(),
                    size: e.header().size().unwrap_or(0),
                    mtime: e.header().mtime().ok().map(|m| m as i64),
                });
            }
            Ok(())
        }
        Detected::Zip => {
            let f = File::open(path).map_err(|e| e.to_string())?;
            let mut ar = zip::ZipArchive::new(BufReader::new(f)).map_err(|e| e.to_string())?;
            for i in 0..ar.len() {
                let zf = ar.by_index_raw(i).map_err(|e| e.to_string())?;
                let p = zf.name().trim_end_matches('/').replace('\\', "/");
                if p.is_empty() {
                    continue;
                }
                let mtime = zf.last_modified().map(|d| {
                    chrono::NaiveDate::from_ymd_opt(
                        d.year().max(1970) as i32,
                        d.month().max(1) as u32,
                        d.day().max(1) as u32,
                    )
                    .and_then(|date| {
                        date.and_hms_opt(d.hour() as u32, d.minute() as u32, d.second() as u32)
                    })
                    .map(|t| t.and_utc().timestamp())
                    .unwrap_or(0)
                });
                sink(Entry {
                    path: p,
                    is_dir: zf.is_dir(),
                    size: zf.size(),
                    mtime,
                });
            }
            Ok(())
        }
        Detected::SevenZip => {
            let sz = sevenz_rust::SevenZReader::open(path, sevenz_rust::Password::empty())
                .map_err(|e| e.to_string())?;
            for entry in sz.archive().files.iter() {
                let p = entry.name.trim_end_matches('/').replace('\\', "/");
                if p.is_empty() {
                    continue;
                }
                let mtime = if entry.has_last_modified_date {
                    Some(entry.last_modified_date.to_unix_time())
                } else {
                    None
                };
                sink(Entry {
                    path: p,
                    is_dir: entry.is_directory,
                    size: entry.size,
                    mtime,
                });
            }
            Ok(())
        }
        Detected::Rar => {
            let file = File::open(path).map_err(|e| e.to_string())?;
            let archive = unrar_rs::RarArchive::open(file).map_err(|e| e.to_string())?;
            for member in archive.entries() {
                let p = member.name.trim_end_matches('/').replace('\\', "/");
                if p.is_empty() {
                    continue;
                }
                let mtime = member.mtime.and_then(|t| {
                    t.duration_since(std::time::UNIX_EPOCH)
                        .ok()
                        .map(|d| d.as_secs() as i64)
                });
                sink(Entry {
                    path: p,
                    is_dir: member.is_directory,
                    size: member.unpacked_size.unwrap_or(0),
                    mtime,
                });
            }
            Ok(())
        }
        Detected::Raw(_, bare) => Err(format!("single-file:{bare}")),
    }
}

/// Reader that counts bytes read through it (progress proxy: compressed bytes).
pub struct CountingReader<R> {
    inner: R,
    count: Arc<AtomicU64>,
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.count.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

impl<R: std::io::Seek> std::io::Seek for CountingReader<R> {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        let n = self.inner.seek(pos)?;
        self.count.store(n, Ordering::Relaxed);
        Ok(n)
    }
}

/// Extract the whole archive (or decompress a raw stream) into `dest`.
/// `progress` (if given) accumulates compressed bytes read — the UI divides
/// it by the file size to show a percentage.
pub fn extract(path: &Path, dest: &Path, progress: Option<Arc<AtomicU64>>) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let f = File::open(path).map_err(|e| e.to_string())?;
    let counted = CountingReader {
        inner: f,
        count: progress.unwrap_or_else(|| Arc::new(AtomicU64::new(0))),
    };
    match detect(&file_name(path)) {
        Detected::Tar(comp) => {
            let reader = wrap(counted, &comp)?;
            let mut ar = tar::Archive::new(reader);
            ar.unpack(dest).map_err(|e| e.to_string())
        }
        Detected::Zip => {
            let mut ar = zip::ZipArchive::new(BufReader::new(counted)).map_err(|e| e.to_string())?;
            ar.extract(dest).map_err(|e| e.to_string())
        }
        Detected::SevenZip => {
            sevenz_rust::decompress_file(path, dest).map_err(|e| e.to_string())
        }
        Detected::Rar => {
            let file = File::open(path).map_err(|e| e.to_string())?;
            let mut archive = unrar_rs::RarArchive::open(file).map_err(|e| e.to_string())?;
            for index in 0..archive.len() {
                let member = archive.by_index(index).map_err(|e| e.to_string())?;
                member.unpack_in(dest).map_err(|e| e.to_string())?;
            }
            Ok(())
        }
        Detected::Raw(comp, bare) => {
            let mut reader = wrap(counted, &comp)?;
            let out_path = dest.join(bare.trim_end_matches('.'));
            let mut out = File::create(&out_path).map_err(|e| e.to_string())?;
            std::io::copy(&mut reader, &mut out)
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
    }
}

/// Read up to `max_bytes` of a specific entry inside an archive for preview.
pub fn read_entry_preview(path: &Path, entry_path: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
    match detect(&file_name(path)) {
        Detected::Zip => {
            let f = File::open(path).map_err(|e| e.to_string())?;
            let mut ar = zip::ZipArchive::new(BufReader::new(f)).map_err(|e| e.to_string())?;
            let mut zf = ar.by_name(entry_path).map_err(|e| e.to_string())?;
            let mut buf = vec![0u8; max_bytes];
            let mut total = 0;
            while total < max_bytes {
                let n = zf.read(&mut buf[total..]).map_err(|e| e.to_string())?;
                if n == 0 { break; }
                total += n;
            }
            buf.truncate(total);
            Ok(buf)
        }
        Detected::Tar(comp) => {
            let f = File::open(path).map_err(|e| e.to_string())?;
            let reader = wrap(f, &comp)?;
            let mut ar = tar::Archive::new(reader);
            for e in ar.entries().map_err(|e| e.to_string())? {
                let mut e = e.map_err(|e| e.to_string())?;
                let p = e.path().map_err(|e| e.to_string())?.to_string_lossy().trim_end_matches('/').replace('\\', "/");
                if p == entry_path {
                    let mut buf = vec![0u8; max_bytes];
                    let mut total = 0;
                    while total < max_bytes {
                        let n = e.read(&mut buf[total..]).map_err(|e| e.to_string())?;
                        if n == 0 { break; }
                        total += n;
                    }
                    buf.truncate(total);
                    return Ok(buf);
                }
            }
            Err("Entry not found in archive".into())
        }
        Detected::SevenZip => {
            let mut buf = Vec::new();
            sevenz_rust::decompress_file_with_extract_fn(path, "", |entry, reader, _| {
                let p = entry.name().trim_end_matches('/').replace('\\', "/");
                if p == entry_path {
                    let mut temp = vec![0u8; max_bytes];
                    if let Ok(n) = reader.read(&mut temp) {
                        temp.truncate(n);
                        buf = temp;
                    }
                    Ok(false)
                } else {
                    Ok(true)
                }
            }).map_err(|e| e.to_string())?;
            Ok(buf)
        }
        Detected::Rar => {
            let file = File::open(path).map_err(|e| e.to_string())?;
            let mut archive = unrar_rs::RarArchive::open(file).map_err(|e| e.to_string())?;
            for index in 0..archive.len() {
                let mut member = archive.by_index(index).map_err(|e| e.to_string())?;
                let p = member.name().trim_end_matches('/').replace('\\', "/");
                if p == entry_path {
                    let mut buf = vec![0u8; max_bytes];
                    let mut total = 0;
                    while total < max_bytes {
                        let n = member.read(&mut buf[total..]).map_err(|e| e.to_string())?;
                        if n == 0 { break; }
                        total += n;
                    }
                    buf.truncate(total);
                    return Ok(buf);
                }
            }
            Err("Entry not found in archive".into())
        }
        Detected::Raw(comp, _) => {
            let f = File::open(path).map_err(|e| e.to_string())?;
            let mut reader = wrap(f, &comp)?;
            let mut buf = vec![0u8; max_bytes];
            let mut total = 0;
            while total < max_bytes {
                let n = reader.read(&mut buf[total..]).map_err(|e| e.to_string())?;
                if n == 0 { break; }
                total += n;
            }
            buf.truncate(total);
            Ok(buf)
        }
    }
}

fn single_rel_path(entry_path: &str, current_path: &str) -> PathBuf {
    let folder_base = Path::new(entry_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| entry_path.to_string());
    if current_path == entry_path {
        PathBuf::from(folder_base)
    } else {
        let sub = current_path
            .strip_prefix(&format!("{entry_path}/"))
            .unwrap_or(current_path);
        PathBuf::from(folder_base).join(sub)
    }
}

/// Extract a single entry (or single directory subtree) into `dest`.
pub fn extract_single(path: &Path, entry_path: &str, dest: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    match detect(&file_name(path)) {
        Detected::Zip => {
            let f = File::open(path).map_err(|e| e.to_string())?;
            let mut ar = zip::ZipArchive::new(BufReader::new(f)).map_err(|e| e.to_string())?;
            for i in 0..ar.len() {
                let mut zf = ar.by_index(i).map_err(|e| e.to_string())?;
                let p = zf.name().trim_end_matches('/').replace('\\', "/");
                if p == entry_path || p.starts_with(&format!("{entry_path}/")) {
                    let outpath = dest.join(single_rel_path(entry_path, &p));
                    if zf.is_dir() {
                        std::fs::create_dir_all(&outpath).map_err(|e| e.to_string())?;
                    } else {
                        if let Some(parent) = outpath.parent() {
                            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                        }
                        let mut outfile = File::create(&outpath).map_err(|e| e.to_string())?;
                        std::io::copy(&mut zf, &mut outfile).map_err(|e| e.to_string())?;
                    }
                }
            }
            Ok(dest.to_path_buf())
        }
        Detected::Tar(comp) => {
            let f = File::open(path).map_err(|e| e.to_string())?;
            let reader = wrap(f, &comp)?;
            let mut ar = tar::Archive::new(reader);
            for e in ar.entries().map_err(|e| e.to_string())? {
                let mut e = e.map_err(|e| e.to_string())?;
                let p = e.path().map_err(|e| e.to_string())?.to_string_lossy().trim_end_matches('/').replace('\\', "/");
                if p == entry_path || p.starts_with(&format!("{entry_path}/")) {
                    let outpath = dest.join(single_rel_path(entry_path, &p));
                    if e.header().entry_type().is_dir() {
                        std::fs::create_dir_all(&outpath).map_err(|e| e.to_string())?;
                    } else {
                        if let Some(parent) = outpath.parent() {
                            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                        }
                        let mut outfile = File::create(&outpath).map_err(|e| e.to_string())?;
                        std::io::copy(&mut e, &mut outfile).map_err(|e| e.to_string())?;
                    }
                }
            }
            Ok(dest.to_path_buf())
        }
        Detected::SevenZip => {
            sevenz_rust::decompress_file_with_extract_fn(path, dest, |entry, reader, _| {
                let p = entry.name().trim_end_matches('/').replace('\\', "/");
                if p == entry_path || p.starts_with(&format!("{entry_path}/")) {
                    let outpath = dest.join(single_rel_path(entry_path, &p));
                    if entry.is_directory() {
                        let _ = std::fs::create_dir_all(&outpath);
                    } else {
                        if let Some(parent) = outpath.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        if let Ok(mut outfile) = File::create(&outpath) {
                            let _ = std::io::copy(reader, &mut outfile);
                        }
                        if p == entry_path {
                            return Ok(false);
                        }
                    }
                }
                Ok(true)
            }).map_err(|e| e.to_string())?;
            Ok(dest.to_path_buf())
        }
        Detected::Rar => {
            let file = File::open(path).map_err(|e| e.to_string())?;
            let mut archive = unrar_rs::RarArchive::open(file).map_err(|e| e.to_string())?;
            for index in 0..archive.len() {
                let member = archive.by_index(index).map_err(|e| e.to_string())?;
                let p = member.name().trim_end_matches('/').replace('\\', "/");
                if p == entry_path || p.starts_with(&format!("{entry_path}/")) {
                    let outpath = dest.join(single_rel_path(entry_path, &p));
                    if let Some(parent) = outpath.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    if member.is_dir() {
                        let _ = std::fs::create_dir_all(&outpath);
                    } else {
                        member.unpack_to(&outpath).map_err(|e| e.to_string())?;
                        if p == entry_path {
                            return Ok(dest.to_path_buf());
                        }
                    }
                }
            }
            Ok(dest.to_path_buf())
        }
        Detected::Raw(comp, bare) => {
            let f = File::open(path).map_err(|e| e.to_string())?;
            let mut reader = wrap(f, &comp)?;
            let outpath = dest.join(&bare);
            let mut outfile = File::create(&outpath).map_err(|e| e.to_string())?;
            std::io::copy(&mut reader, &mut outfile).map_err(|e| e.to_string())?;
            Ok(dest.to_path_buf())
        }
    }
}