//! Native compression — tar.*, zip. No ouch, no WSL.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub const FORMATS: [&str; 8] = ["zip", "7z", "tar.zst", "tar.gz", "tar.xz", "tar.lz4", "tar", "tar.bz2"];

pub fn ext_of(format: &str) -> &'static str {
    match format {
        "zip" => ".zip",
        "7z" => ".7z",
        "tar" => ".tar",
        "tar.gz" => ".tar.gz",
        "tar.xz" => ".tar.xz",
        "tar.zst" => ".tar.zst",
        "tar.bz2" => ".tar.bz2",
        "tar.lz4" => ".tar.lz4",
        _ => ".zip",
    }
}

/// Recursively collect (absolute_path, relative_name) for all files.
pub fn walk_files(input: &Path, files: &mut Vec<(PathBuf, String)>, depth: u8) -> Result<(), std::io::Error> {
    walk_files_with_prefix(input, files, depth, false)
}

pub fn walk_files_with_prefix(
    input: &Path,
    files: &mut Vec<(PathBuf, String)>,
    depth: u8,
    preserve_dir_name: bool,
) -> Result<(), std::io::Error> {
    if depth > crate::tree::MAX_DEPTH {
        return Ok(());
    }
    if input.is_file() {
        if let Some(name) = input.file_name() {
            files.push((input.to_path_buf(), name.to_string_lossy().to_string()));
        }
        return Ok(());
    }
    fn walk_dir(
        dir: &Path,
        rel: String,
        files: &mut Vec<(PathBuf, String)>,
        depth: u8,
    ) -> Result<(), std::io::Error> {
        if depth > crate::tree::MAX_DEPTH {
            return Ok(());
        }
        match std::fs::read_dir(dir) {
            Ok(rd) => {
                for e in rd {
                    let e = match e {
                        Ok(entry) => entry,
                        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                            return Err(err);
                        }
                        Err(_) => continue,
                    };
                    let fname = e.file_name().to_string_lossy().to_string();
                    let rel_path = if rel.is_empty() {
                        fname.clone()
                    } else {
                        format!("{rel}/{fname}")
                    };
                    match e.file_type() {
                        Ok(ft) if ft.is_dir() => {
                            walk_dir(&e.path(), rel_path, files, depth + 1)?;
                        }
                        Ok(ft) if ft.is_file() => {
                            files.push((e.path(), rel_path));
                        }
                        _ => {}
                    }
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                return Err(err);
            }
            Err(_) => {}
        }
        Ok(())
    }
    let base_rel = if preserve_dir_name {
        input
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    } else {
        String::new()
    };
    walk_dir(input, base_rel, files, 0)
}

/// Reader that counts consumed input bytes with batched atomic updates
/// to minimize CPU cache-line bouncing during high-speed I/O.
struct CountingReader<R> {
    inner: R,
    done: Arc<AtomicU64>,
    pending: u64,
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.pending += n as u64;
        if self.pending >= 256 * 1024 {
            self.done.fetch_add(self.pending, Ordering::Relaxed);
            self.pending = 0;
        }
        Ok(n)
    }
}

impl<R> Drop for CountingReader<R> {
    fn drop(&mut self) {
        if self.pending > 0 {
            self.done.fetch_add(self.pending, Ordering::Relaxed);
        }
    }
}

fn copy_buffered<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    progress: &Arc<AtomicU64>,
    buf: &mut [u8],
) -> std::io::Result<u64> {
    let mut total = 0u64;
    let mut batch = 0u64;
    loop {
        let n = match reader.read(buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        writer.write_all(&buf[..n])?;
        total += n as u64;
        batch += n as u64;
        if batch >= 256 * 1024 {
            progress.fetch_add(batch, Ordering::Relaxed);
            batch = 0;
        }
    }
    if batch > 0 {
        progress.fetch_add(batch, Ordering::Relaxed);
    }
    Ok(total)
}

/// Generic tar packer with 256 KB buffered reads per file.
fn pack_tar<W: Write>(
    w: W,
    inputs: &[(PathBuf, String)],
    done: &Arc<AtomicU64>,
) -> Result<W, String> {
    let mut b = tar::Builder::new(w);
    for (path, rel) in inputs {
        let f = File::open(path).map_err(|e| e.to_string())?;
        let size = f.metadata().map(|m| m.len()).unwrap_or(0);
        let mut header = tar::Header::new_gnu();
        header.set_size(size);
        header.set_mode(0o644);
        let reader = BufReader::with_capacity(256 * 1024, f);
        let mut cr = CountingReader { inner: reader, done: done.clone(), pending: 0 };
        b.append_data(&mut header, rel, &mut cr)
            .map_err(|e| e.to_string())?;
    }
    let w = b.into_inner().map_err(|e| e.to_string())?;
    Ok(w)
}

pub const LEVELS: [&str; 4] = ["store", "fast", "normal", "max"];

pub fn level_label(level: &str) -> &'static str {
    match level {
        "store" => "Store",
        "fast" => "Fast",
        "normal" => "Normal",
        "max" => "Max",
        _ => "Normal",
    }
}

/// Compress collected inputs into `out`. `progress` accumulates INPUT bytes
/// consumed — the UI divides it by `total` (sum of input sizes).
pub fn compress(
    inputs: &[(PathBuf, String)],
    _total: u64,
    out: &Path,
    format: &str,
    level: &str,
    password: Option<&str>,
    progress: Arc<AtomicU64>,
) -> Result<(), String> {
    if inputs.is_empty() {
        return Err("nothing to compress (input is empty or missing)".into());
    }
    if let Some(pass) = password {
        if !pass.is_empty() && format.starts_with("tar") {
            return Err("Формат tar.* не поддерживает пароли. Для создания защищенного архива выберите 'zip' или '7z'.".into());
        }
    }
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let raw_file = File::create(out).map_err(|e| e.to_string())?;
    let f = BufWriter::with_capacity(512 * 1024, raw_file);

    match format {
        "zip" => {
            let mut zf = zip::ZipWriter::new(f);
            let method = match level {
                "store" => zip::CompressionMethod::Stored,
                _ => zip::CompressionMethod::Deflated,
            };
            let comp_level = match level {
                "fast" => Some(1),
                "max" => Some(9),
                "store" => None,
                _ => Some(6),
            };
            let mut opts = zip::write::SimpleFileOptions::default()
                .compression_method(method)
                .compression_level(comp_level);
            if let Some(pass) = password {
                if !pass.is_empty() {
                    opts = opts.with_aes_encryption(zip::AesMode::Aes256, pass);
                }
            }
            let mut buf = vec![0u8; 256 * 1024];
            for (path, rel) in inputs {
                zf.start_file(rel.clone(), opts)
                    .map_err(|e| e.to_string())?;
                let mut file = BufReader::with_capacity(256 * 1024, File::open(path).map_err(|e| e.to_string())?);
                copy_buffered(&mut file, &mut zf, &progress, &mut buf)
                    .map_err(|e| e.to_string())?;
            }
            let mut out_writer = zf.finish().map_err(|e| e.to_string())?;
            out_writer.flush().map_err(|e| e.to_string())?;
            Ok(())
        }
        "7z" => {
            let mut z = sevenz_rust::SevenZWriter::new(f).map_err(|e| e.to_string())?;
            if let Some(pass) = password {
                if !pass.is_empty() {
                    z.set_content_methods(vec![
                        sevenz_rust::AesEncoderOptions::new(sevenz_rust::Password::from(pass)).into(),
                        sevenz_rust::SevenZMethod::LZMA2.into(),
                    ]);
                }
            }
            for (path, rel) in inputs {
                let file = File::open(path).map_err(|e| e.to_string())?;
                let entry = sevenz_rust::SevenZArchiveEntry::from_path(path, rel.clone());
                let reader = BufReader::with_capacity(256 * 1024, file);
                let mut cr = CountingReader { inner: reader, done: progress.clone(), pending: 0 };
                z.push_archive_entry(entry, Some(&mut cr)).map_err(|e| e.to_string())?;
            }
            z.finish().map_err(|e| e.to_string())?;
            Ok(())
        }
        "tar" => {
            let mut w = pack_tar(f, inputs, &progress)?;
            w.flush().map_err(|e| e.to_string())?;
            Ok(())
        }
        "tar.gz" => {
            let gz_level = match level {
                "store" => flate2::Compression::none(),
                "fast" => flate2::Compression::fast(),
                "max" => flate2::Compression::best(),
                _ => flate2::Compression::default(),
            };
            let gz = pack_tar(
                flate2::write::GzEncoder::new(f, gz_level),
                inputs,
                &progress,
            )?;
            let mut w = gz.finish().map_err(|e| e.to_string())?;
            w.flush().map_err(|e| e.to_string())?;
            Ok(())
        }
        "tar.xz" => {
            let xz_level = match level {
                "store" => 0,
                "fast" => 1,
                "max" => 9,
                _ => 6,
            };
            let threads = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1) as u32;
            let encoder = match xz2::stream::MtStreamBuilder::new()
                .threads(threads)
                .preset(xz_level)
                .encoder()
            {
                Ok(stream) => xz2::write::XzEncoder::new_stream(f, stream),
                Err(_) => xz2::write::XzEncoder::new(f, xz_level),
            };
            let xz = pack_tar(encoder, inputs, &progress)?;
            let mut w = xz.finish().map_err(|e| e.to_string())?;
            w.flush().map_err(|e| e.to_string())?;
            Ok(())
        }
        "tar.zst" => {
            let zstd_level = match level {
                "store" => 1,
                "fast" => 1,
                "max" => 19,
                _ => 3,
            };
            let mut enc = zstd::stream::write::Encoder::new(f, zstd_level).map_err(|e| e.to_string())?;
            let threads = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1) as u32;
            let _ = enc.multithread(threads);
            let enc = pack_tar(enc, inputs, &progress)?;
            let mut w = enc.finish().map_err(|e| e.to_string())?;
            w.flush().map_err(|e| e.to_string())?;
            Ok(())
        }
        "tar.bz2" => {
            let bz_level = match level {
                "store" => bzip2::Compression::fast(),
                "fast" => bzip2::Compression::fast(),
                "max" => bzip2::Compression::best(),
                _ => bzip2::Compression::default(),
            };
            let bz = pack_tar(
                bzip2::write::BzEncoder::new(f, bz_level),
                inputs,
                &progress,
            )?;
            let mut w = bz.finish().map_err(|e| e.to_string())?;
            w.flush().map_err(|e| e.to_string())?;
            Ok(())
        }
        "tar.lz4" => {
            let mut frame_info = lz4_flex::frame::FrameInfo::new();
            frame_info.block_size = lz4_flex::frame::BlockSize::Max4MB;
            frame_info.block_mode = lz4_flex::frame::BlockMode::Linked;
            let enc = lz4_flex::frame::FrameEncoder::with_frame_info(frame_info, f);
            let enc = pack_tar(enc, inputs, &progress)?;
            let mut w = enc.finish().map_err(|e| e.to_string())?;
            w.flush().map_err(|e| e.to_string())?;
            Ok(())
        }
        _ => Err(format!("unsupported format: {format}")),
    }
}

pub fn is_same_file(candidate: &Path, target: &Path) -> bool {
    if candidate == target {
        return true;
    }
    if let (Ok(c1), Ok(c2)) = (candidate.canonicalize(), target.canonicalize()) {
        return c1 == c2;
    }
    if let (Some(tp), Some(tname)) = (target.parent(), target.file_name()) {
        if let Ok(cp) = tp.canonicalize() {
            if let Ok(c1) = candidate.canonicalize() {
                return c1 == cp.join(tname);
            }
        }
    }
    false
}

/// Strip the output destination file from inputs to prevent self-compression.
pub fn filter_out_target(files: &mut Vec<(PathBuf, String)>, out_path: &Path) {
    files.retain(|(p, _)| !is_same_file(p, out_path));
}

/// Total size of the input set (for progress percentage).
pub fn total_size(files: &[(PathBuf, String)]) -> u64 {
    files
        .iter()
        .filter_map(|(p, _)| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum()
}