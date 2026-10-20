//! Files on the system clipboard, the way other file managers do it.
//! Linux (Wayland): `x-special/gnome-copied-files` (copy or cut), `text/uri-list`
//! and the plain paths. Windows: a file list plus "Preferred DropEffect", like
//! Explorer. macOS: file URLs like Finder; Finder has no "cut", so Guppy
//! remembers its own cut.

#[cfg(not(target_os = "linux"))]
pub use other::{get, put};

#[cfg(target_os = "linux")]
use std::io::Read;
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
use wl_clipboard_rs::copy::{self, MimeSource, Source};
#[cfg(target_os = "linux")]
use wl_clipboard_rs::paste::{self, ClipboardType, Seat};

#[cfg(target_os = "linux")]
const GNOME: &str = "x-special/gnome-copied-files";
#[cfg(target_os = "linux")]
const URI_LIST: &str = "text/uri-list";

#[cfg(target_os = "linux")]
/// Puts files on the clipboard; `cut` marks them to be moved on paste.
pub fn put(paths: &[PathBuf], cut: bool) -> Result<(), String> {
    let uris: Vec<String> = paths.iter().map(|p| uri(p)).collect();
    let text = |s: String| Source::Bytes(s.into_bytes().into_boxed_slice());
    // Plain paths first: `wl-paste` without a type (used by terminal apps such
    // as Claude Code) takes the first text format offered.
    let sources = vec![
        MimeSource {
            source: text(paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n")),
            mime_type: copy::MimeType::Text,
        },
        MimeSource { source: text(uris.iter().map(|u| format!("{u}\r\n")).collect()), mime_type: copy::MimeType::Specific(URI_LIST.into()) },
        MimeSource {
            source: text(format!("{}\n{}", if cut { "cut" } else { "copy" }, uris.join("\n"))),
            mime_type: copy::MimeType::Specific(GNOME.into()),
        },
    ];

    copy::Options::new().copy_multi(sources).map_err(|e| e.to_string())
}

#[cfg(target_os = "linux")]
/// Files on the clipboard and whether they were cut; `None` if it holds no files.
pub fn get() -> Option<(Vec<PathBuf>, bool)> {
    let read = |mime: &str| {
        let (mut pipe, _) = paste::get_contents(ClipboardType::Regular, Seat::Unspecified, paste::MimeType::Specific(mime)).ok()?;
        let mut s = String::new();
        pipe.read_to_string(&mut s).ok()?;
        Some(s)
    };
    let (cut, list) = match read(GNOME) {
        Some(s) => {
            let mut lines = s.lines();
            (lines.next() == Some("cut"), lines.map(str::to_string).collect::<Vec<_>>())
        }
        None => (false, read(URI_LIST)?.lines().map(str::to_string).collect()),
    };
    let paths: Vec<PathBuf> = list.iter().filter(|l| !l.starts_with('#')).filter_map(|l| from_uri(l.trim())).collect();
    (!paths.is_empty()).then_some((paths, cut))
}

#[cfg(target_os = "linux")]
pub(crate) fn uri(p: &Path) -> String {
    let mut out = String::from("file://");
    for b in p.as_os_str().as_encoded_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => out.push(*b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(target_os = "linux")]
pub(crate) fn from_uri(u: &str) -> Option<PathBuf> {
    let rest = u.strip_prefix("file://")?;
    // Skip an optional host part ("file://host/path").
    let rest = &rest[rest.find('/')?..];
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&rest[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
    }
    #[cfg(not(unix))]
    Some(PathBuf::from(String::from_utf8_lossy(&out).into_owned()))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn uri_round_trip() {
        for p in ["/home/a b/ä.jpg", "/x/100%.txt", "/plain"] {
            assert_eq!(from_uri(&uri(Path::new(p))), Some(PathBuf::from(p)));
        }
        assert_eq!(from_uri("file://host/tmp/x"), Some(PathBuf::from("/tmp/x")));
    }
}

#[cfg(all(test, target_os = "linux"))]
mod live {
    use super::*;

    /// Needs a Wayland session: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn put_then_get() {
        let p = vec![PathBuf::from("/tmp/a b.jpg"), PathBuf::from("/etc/hosts")];
        put(&p, true).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let out = std::process::Command::new("wl-paste").args(["--list-types"]).output().unwrap();
        eprintln!("{}", String::from_utf8_lossy(&out.stdout));
        for args in [vec![], vec!["--no-newline"], vec!["--type", "text/plain"], vec!["--type", "text"]] {
            let out = std::process::Command::new("wl-paste").args(&args).output().unwrap();
            eprintln!("{args:?} -> {:?}", String::from_utf8_lossy(&out.stdout));
        }
        assert_eq!(get(), Some((p, true)));
    }
}

#[cfg(windows)]
mod other {
    use std::path::PathBuf;

    use clipboard_win::{Clipboard, raw};

    /// Explorer's flag for cut (2 = move) or copy (5).
    const DROP_EFFECT: &str = "Preferred DropEffect";

    pub fn put(paths: &[PathBuf], cut: bool) -> Result<(), String> {
        let names: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        let _open = Clipboard::new_attempts(10).map_err(|e| e.to_string())?;
        raw::empty().map_err(|e| e.to_string())?;
        raw::set_file_list_with(&names, clipboard_win::options::NoClear).map_err(|e| e.to_string())?;
        if let Some(f) = raw::register_format(DROP_EFFECT) {
            let effect: u32 = if cut { 2 } else { 5 };
            raw::set_without_clear(f.get(), &effect.to_le_bytes()).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn get() -> Option<(Vec<PathBuf>, bool)> {
        let _open = Clipboard::new_attempts(10).ok()?;
        let mut paths = Vec::new();
        raw::get_file_list_path(&mut paths).ok()?;
        let mut effect = Vec::new();
        let cut = raw::register_format(DROP_EFFECT)
            .and_then(|f| raw::get_vec(f.get(), &mut effect).ok())
            .is_some_and(|_| effect.first().is_some_and(|b| b & 2 != 0));
        (!paths.is_empty()).then_some((paths, cut))
    }
}

#[cfg(target_os = "macos")]
mod other {
    use std::path::PathBuf;
    use std::sync::Mutex;

    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, ProtocolObject};
    use objc2::ClassType;
    use objc2_app_kit::{NSPasteboard, NSPasteboardWriting};
    use objc2_foundation::{NSArray, NSString, NSURL};

    /// Clipboard change count right after Guppy's own cut, so paste moves.
    static CUT_AT: Mutex<Option<isize>> = Mutex::new(None);

    pub fn put(paths: &[PathBuf], cut: bool) -> Result<(), String> {
        let urls: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = paths
            .iter()
            .map(|p| ProtocolObject::from_retained(NSURL::fileURLWithPath(&NSString::from_str(&p.to_string_lossy()))))
            .collect();
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        if !pb.writeObjects(&NSArray::from_retained_slice(&urls)) {
            return Err("the clipboard refused the files".into());
        }
        *CUT_AT.lock().unwrap() = cut.then(|| pb.changeCount());
        Ok(())
    }

    pub fn get() -> Option<(Vec<PathBuf>, bool)> {
        let pb = NSPasteboard::generalPasteboard();
        let classes = NSArray::from_slice(&[NSURL::class() as &AnyClass]);
        // SAFETY: NSURL conforms to NSPasteboardReading; no options.
        let objects = unsafe { pb.readObjectsForClasses_options(&classes, None) }?;
        let paths: Vec<PathBuf> = objects
            .iter()
            .filter_map(|o| o.downcast::<NSURL>().ok())
            .filter(|u| u.isFileURL())
            .filter_map(|u| u.path())
            .map(|s| PathBuf::from(s.to_string()))
            .collect();
        let cut = *CUT_AT.lock().unwrap() == Some(pb.changeCount());
        (!paths.is_empty()).then_some((paths, cut))
    }
}
