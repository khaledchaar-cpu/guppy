//! Searching file contents below a folder, in the background.
//!
//! Text only: files with a NUL byte near the start count as binary and are
//! skipped, as are files over [`MAX_FILE`]. The match ignores case.
//!
//! A needle with `*` or `?` (like `*.png`) looks for file and folder names
//! instead; those hits have line 0.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

/// Larger files are skipped (logs, dumps, databases).
pub const MAX_FILE: u64 = 20 * 1024 * 1024;
/// The search stops after this many hits.
pub const MAX_HITS: usize = 2000;

/// Folders nobody wants to search through.
const SKIP_DIRS: &[&str] = &[".git", "node_modules", "target", ".cache", "__pycache__", ".venv"];

#[derive(Debug, Clone)]
pub struct Hit {
    pub path: PathBuf,
    /// 1-based; 0 for a name hit.
    pub line: usize,
    /// The line, trimmed and cut to a readable length.
    pub text: String,
}

/// A running search; dropping it stops the search.
pub struct Search {
    pub rx: Receiver<Hit>,
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    scanned: Arc<AtomicUsize>,
}

impl Search {
    pub fn finished(&self) -> bool {
        self.done.load(Ordering::Relaxed)
    }

    /// Files looked at so far.
    pub fn scanned(&self) -> usize {
        self.scanned.load(Ordering::Relaxed)
    }
}

impl Drop for Search {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Starts searching `root` for `needle`; hidden files and folders only with `hidden`.
pub fn start(root: PathBuf, needle: &str, hidden: bool) -> Search {
    let (tx, rx) = channel();
    let stop = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let scanned = Arc::new(AtomicUsize::new(0));
    let (s, d, n) = (stop.clone(), done.clone(), scanned.clone());
    let needle = needle.to_lowercase();
    let names = needle.contains(['*', '?']);
    std::thread::spawn(move || {
        let found = AtomicUsize::new(0);
        let ctx = Ctx { needle: &needle, names, hidden, stop: &s, scanned: &n, found: &found, tx: &tx };
        let dev = std::fs::metadata(&root).map(|m| crate::platform::device(&m)).unwrap_or(0);
        walk(&ctx, &root, dev);
        d.store(true, Ordering::Relaxed);
    });
    Search { rx, stop, done, scanned }
}

struct Ctx<'a> {
    needle: &'a str,
    /// The needle is a wildcard pattern for names.
    names: bool,
    hidden: bool,
    stop: &'a AtomicBool,
    scanned: &'a AtomicUsize,
    found: &'a AtomicUsize,
    tx: &'a Sender<Hit>,
}

impl Ctx<'_> {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed) || self.found.load(Ordering::Relaxed) >= MAX_HITS
    }
}

fn walk(ctx: &Ctx, dir: &Path, dev: u64) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    let mut subdirs = Vec::new();
    for e in entries {
        if ctx.stopped() {
            return;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        if !ctx.hidden && crate::platform::is_hidden(&name, None) {
            continue;
        }
        // DirEntry::metadata does not follow symlinks.
        let Ok(m) = e.metadata() else { continue };
        if ctx.names {
            if crate::fs::glob_match(ctx.needle, &name) {
                if ctx.tx.send(Hit { path: e.path(), line: 0, text: String::new() }).is_err() {
                    return;
                }
                ctx.found.fetch_add(1, Ordering::Relaxed);
            }
            ctx.scanned.fetch_add(1, Ordering::Relaxed);
            if m.is_dir() && !SKIP_DIRS.contains(&name.as_str()) && crate::platform::device(&m) == dev {
                subdirs.push(e.path());
            }
        } else if m.is_dir() {
            if !SKIP_DIRS.contains(&name.as_str()) && crate::platform::device(&m) == dev {
                subdirs.push(e.path());
            }
        } else if m.is_file() && m.len() <= MAX_FILE {
            search_file(ctx, &e.path());
        }
    }
    // Files of a folder first, then its subfolders.
    for d in subdirs {
        walk(ctx, &d, dev);
    }
}

fn search_file(ctx: &Ctx, path: &Path) {
    ctx.scanned.fetch_add(1, Ordering::Relaxed);
    let Ok(f) = std::fs::File::open(path) else { return };
    let mut r = BufReader::new(f);
    let Ok(head) = r.fill_buf() else { return };
    if head[..head.len().min(8000)].contains(&0) {
        return;
    }
    let mut buf = Vec::new();
    let mut line = 0;
    loop {
        buf.clear();
        // Lines are read as bytes, so other encodings don't end the search.
        match r.by_ref().take(64 * 1024).read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        line += 1;
        let text = String::from_utf8_lossy(&buf);
        if text.to_lowercase().contains(ctx.needle) {
            let t = text.trim();
            let t = if t.chars().count() > 200 { t.chars().take(200).chain(std::iter::once('…')).collect() } else { t.to_string() };
            if ctx.tx.send(Hit { path: path.to_path_buf(), line, text: t }).is_err() {
                return;
            }
            if ctx.found.fetch_add(1, Ordering::Relaxed) + 1 >= MAX_HITS || ctx.stop.load(Ordering::Relaxed) {
                return;
            }
        }
    }
}

/// Lines around `line` (1-based) for a preview of the place.
pub fn context(path: &Path, line: usize, around: usize) -> Vec<(usize, String)> {
    let Ok(f) = std::fs::File::open(path) else { return Vec::new() };
    let first = line.saturating_sub(around).max(1);
    BufReader::new(f)
        .split(b'\n')
        .map_while(Result::ok)
        .enumerate()
        .map(|(i, l)| (i + 1, String::from_utf8_lossy(&l).trim_end().replace('\t', "    ")))
        .skip(first - 1)
        .take(2 * around + 1)
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn finds_text_skips_binary() {
        let root = std::env::temp_dir().join(format!("guppy-search-{}", std::process::id()));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("a.txt"), "one\nThe Needle here\nthree\n").unwrap();
        std::fs::write(root.join("sub").join("b.md"), "needle again").unwrap();
        std::fs::write(root.join("bin.dat"), b"needle\0binary").unwrap();
        std::fs::write(root.join(".git").join("x"), "needle").unwrap();
        let s = super::start(root.clone(), "NEEDLE", true);
        while !s.finished() {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let mut hits: Vec<_> = s.rx.try_iter().map(|h| (h.path.strip_prefix(&root).unwrap().to_path_buf(), h.line)).collect();
        hits.sort();
        assert_eq!(hits, [(std::path::PathBuf::from("a.txt"), 2), (std::path::Path::new("sub").join("b.md"), 1)]);
        let s = super::start(root.clone(), "*.MD", true);
        while !s.finished() {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let names: Vec<_> = s.rx.try_iter().map(|h| (h.path.strip_prefix(&root).unwrap().to_path_buf(), h.line)).collect();
        assert_eq!(names, [(std::path::Path::new("sub").join("b.md"), 0)]);
        let ctx = super::context(&root.join("a.txt"), 2, 1);
        assert_eq!(ctx.iter().map(|(n, _)| *n).collect::<Vec<_>>(), [1, 2, 3]);
        std::fs::remove_dir_all(root).unwrap();
    }
}
