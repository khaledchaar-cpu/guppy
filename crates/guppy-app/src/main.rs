// Release builds on Windows open no console window next to the app.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod clip;
#[cfg(target_os = "linux")]
mod dnd;
mod icons;
mod paper;
mod preview;
mod treemap;
mod watch;

use paper::{Paper, Scanlines};
use preview::ViewBody;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use guppy_core::{actions, archive, attrs};
use guppy_core::fs::{glob_match, human_date, human_size};
use std::time::SystemTime;
use guppy_core::ops::{self, Job, OpKind, Resolve};
use guppy_core::config::{Action, Bookmark, Look, SortBy};
use guppy_core::{Cancel, Config, DirSize, DirSizes, Entry, FileClass, Jumper};
use iced::keyboard::{self, Key, key::Named};
use iced::widget::operation::{self, AbsoluteOffset};
use iced::widget::{
    text_editor,
    Id, button, column, container, mouse_area, row, scrollable, svg, text,
};
use iced::{Background, Border, Color, Element, Font, Length, Subscription, Task};

const ROW_H: f32 = 26.0;
const SHEET_RADIUS: f32 = 6.0;
const JUMP_LIMIT: usize = 14;
/// Embedded so Guppy looks the same on every system (license: fonts/OFL.txt).
const FONT_REGULAR: &[u8] = include_bytes!("../fonts/JetBrainsMono-Regular.ttf");
const FONT_BOLD: &[u8] = include_bytes!("../fonts/JetBrainsMono-Bold.ttf");
const FONT: Font = Font::with_name("JetBrains Mono");
const BOLD: Font = Font {
    weight: iced::font::Weight::Bold,
    ..FONT
};

/// Animation lengths: a glowing row, a row folding away.
const GLOW: Duration = Duration::from_millis(900);
const FOLD: Duration = Duration::from_millis(260);
/// Dialogs, the active pane's frame and pictures fade in; the space map grows in.
const APPEAR: Duration = Duration::from_millis(160);
const MAP_IN: Duration = Duration::from_millis(320);

/// Mix of two colors; `k` = 0 gives `a`, 1 gives `b`.
fn mix(a: Color, b: Color, k: f32) -> Color {
    Color { r: a.r + (b.r - a.r) * k, g: a.g + (b.g - a.g) * k, b: a.b + (b.b - a.b) * k, a: a.a + (b.a - a.a) * k }
}

/// Process start, for `GUPPY_BENCH`.
static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

fn main() -> iced::Result {
    START.get_or_init(std::time::Instant::now);
    guppy_core::platform::extend_path();
    prefer_vulkan();
    #[cfg(target_os = "linux")]
    dnd::start();
    iced::application(Guppy::new, Guppy::update, Guppy::view)
        .title(Guppy::title)
        .theme(Guppy::theme)
        .subscription(Guppy::subscription)
        .window_size((1200.0, 760.0))
        .font(FONT_REGULAR)
        .font(FONT_BOLD)
        .default_font(FONT)
        // On close, save the size cache first.
        .exit_on_close_request(false)
        .run()
}

/// Without a choice, wgpu starts Vulkan *and* OpenGL to pick an adapter, which
/// loads both drivers (~25 MB more, slower start). With a Vulkan driver installed,
/// only Vulkan is used; `WGPU_BACKEND` still overrides this.
fn prefer_vulkan() {
    #[cfg(target_os = "linux")]
    if std::env::var_os("WGPU_BACKEND").is_none() {
        let has_icd = ["/usr/share/vulkan/icd.d", "/etc/vulkan/icd.d"]
            .iter()
            .any(|d| std::fs::read_dir(d).is_ok_and(|mut rd| rd.any(|e| e.is_ok_and(|e| e.path().extension().is_some_and(|x| x == "json")))));
        if has_icd {
            // Safe: called first thing in main, before any other thread exists.
            unsafe { std::env::set_var("WGPU_BACKEND", "vulkan") };
        }
    }
}

/// Colors for a look: neon and matte black are fixed, paper follows the Omarchy theme.
fn load_theme(look: Look) -> guppy_core::Theme {
    match look {
        Look::Paper => guppy_core::Theme::load(),
        Look::MatteBlack => guppy_core::Theme::default(),
        Look::Neon => guppy_core::Theme::neon(),
    }
}

fn rgb(c: u32) -> Color {
    Color::from_rgb8((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

#[derive(Debug, Clone)]
enum Message {
    Key(keyboard::Event),
    /// Click on a button in the key bar: acts like pressing that key.
    KeyBar(Key, keyboard::Modifiers),
    Select(usize, usize),
    /// Click on a tab: side, position.
    TabClick(usize, usize),
    /// Click on a part of the path in a pane header.
    GoTo(usize, PathBuf),
    /// Click on a favorite button: side, slot. Opens it, or fills a free slot.
    Favorite(usize, usize),
    /// Right-click on a favorite button: puts the pane's folder on it, or clears it.
    SetFavorite(usize, usize),
    /// Mouse while a row is held: where it is, and the button let go.
    DragMoved(iced::Point),
    DragReleased,
    /// The pointer is over a row (side, index) during a drag, or left it.
    DragOver(usize, usize),
    DragLeft(usize, usize),
    /// Files from another app hover over the window or were dropped on it.
    #[cfg(target_os = "linux")]
    Dnd(dnd::Event),
    /// Where a pane's list sits in the window (asked while files from other apps hover).
    #[cfg(target_os = "linux")]
    ListBounds(usize, Option<iced::Rectangle>),
    /// Mouse entered a pane: it becomes the active one.
    HoverPane(usize),
    Open(usize, usize),
    JumpTo(usize),
    Scrolled(usize, scrollable::Viewport),
    /// Folder sizes still running: redraw.
    Tick,
    /// Frame tick during smooth scrolling.
    Frame(std::time::Instant),
    Resized(iced::Size),
    /// Files in an open folder or the theme changed.
    FsChanged(Vec<PathBuf>),
    /// Git status read for a pane's folder.
    Git(usize, PathBuf, Option<std::sync::Arc<guppy_core::git::Status>>),
    CloseRequested(iced::window::Id),
    Back,
    Forward,
    Parent,
    Home,
    ToggleMark,
    SwitchSide,
    Op(OpKind),
    NewDir,
    DialogConfirm,
    DialogCancel,
    ToggleHidden,
    OpenSettings,
    Viewed(PathBuf, ViewBody),
    /// Built-in editor (F4): file read, typing, save, close.
    EditLoaded(PathBuf, Result<String, String>),
    EditAction(text_editor::Action),
    EditSave,
    EditClose,
    /// Clipboard text for the text field that has the keyboard.
    Paste(Option<String>),
    /// Files from the clipboard, and whether they were cut.
    FilesPasted(Option<(Vec<PathBuf>, bool)>),
    /// Click on a settings row: select it and change it one step.
    SettingClick(usize),
    /// Checkbox on the startup help: show it at every start.
    ToggleStartHelp,
    /// Ctrl+Q: show the entry under the cursor in the other pane.
    ToggleQuick,
    /// Space map of the active folder in the other pane.
    ToggleMap,
    /// Click on a tile: folders are entered, files get the cursor.
    MapClick(PathBuf),
    /// The cursor rested long enough: load the quick view (if still current).
    QuickDue(u64),
    QuickLoaded(PathBuf, ViewBody),
    /// Click on a palette row: run that command.
    PaletteRun(usize),
    /// Icon bar: list the trash and ask before emptying it.
    EmptyTrash,
    /// Click on a drive in the drive picker.
    DrivePick(usize),
    /// Answer to "target already exists".
    Resolve(Resolve),
    /// Conflict dialog: the answer holds for all later conflicts of the job.
    ToggleAll,
    /// Open the job list (also Ctrl+J).
    OpenJobs,
    /// Cancel the running job, or with `true` also everything queued.
    CancelJobs(bool),
    /// Pause the running job, or resume it.
    TogglePause,
    /// Waiting job `n`: move it to the front of the queue.
    QueueFirst(usize),
    /// Waiting job `n`: take it out of the queue.
    QueueDrop(usize),
    /// A file from an archive was extracted for opening (or failed).
    Opened(Result<(), String>),
    /// Trash contents read for "Empty trash".
    TrashListed(Result<Vec<PathBuf>, String>),
    /// Trash emptied (or failed).
    TrashEmptied(Result<(), String>),
    /// Developer aid: `GUPPY_SCREENSHOT=file.ppm` saves an image once sizes are
    /// computed, then exits.
    Shot(iced::window::Screenshot),
}

/// Shortens a name that is too long in the middle, so its end (`… v1.zip`) stays readable.
fn cut_middle(name: &str, max: usize) -> String {
    let n = name.chars().count();
    if n <= max {
        return name.to_string();
    }
    let keep = max.saturating_sub(1);
    let tail = (keep * 2 / 5).max(1);
    let head: String = name.chars().take(keep - tail).collect();
    let end: String = name.chars().skip(n - tail).collect();
    format!("{head}…{end}")
}

/// Input starting with `~` (on Unix also `/`) jumps instead of filtering.
fn is_jump(query: &str) -> bool {
    query.chars().next().is_some_and(guppy_core::platform::is_jump_prefix)
}

/// Modal dialog over the window; while open, it receives all keys.
enum Dialog {
    Confirm { kind: OpKind, sources: Vec<PathBuf>, target: Option<PathBuf> },
    NewDir { name: String, error: Option<String> },
    /// Built-in editor (F4); `content` is `None` while the file loads.
    Edit {
        path: PathBuf,
        content: Option<text_editor::Content>,
        dirty: bool,
        /// Esc with unsaved changes asks once; a second Esc discards.
        closing: bool,
        error: Option<String>,
    },
    /// Viewer (F3) for the file under the cursor.
    View { path: PathBuf, body: ViewBody },
    /// The running job hit an existing target; `all` = apply the answer to the rest.
    Conflict { conflict: ops::Conflict, all: bool },
    /// Running and queued jobs.
    Jobs,
    /// Command palette (Ctrl+P): typed query and selected hit.
    Palette { query: String, cur: usize },
    /// Text search in the files below `root`; hits live in `Guppy::search_hits`.
    Search { root: PathBuf, query: String, cur: usize },
    /// Drive picker (Alt+F1 / Alt+F2) for the pane `side`.
    Drives { side: usize, list: Vec<guppy_core::drives::Drive>, cur: usize },
    /// Name for a bookmark of `path`: new (Ctrl+D), or renaming bookmark `edit` (F2 in the picker).
    Bookmark { path: PathBuf, name: String, edit: Option<usize> },
    /// Mark (`+`) or unmark (`-`) by wildcard pattern.
    Mark { add: bool, pattern: String },
    /// Permissions and owner (Alt+Enter); `field`: 0 mode, 1 owner, 2 recursive.
    Attrs { paths: Vec<PathBuf>, mode: String, owner: String, was: (String, String), field: usize, recursive: bool, error: Option<String> },
    Report { title: &'static str, problems: Vec<String> },
    /// Empty the trash: original paths of what's in it.
    EmptyTrash { items: Vec<PathBuf> },
    /// Quick help listing all keys (F1); at startup with a "show again" checkbox.
    Help { startup: bool },
    /// Settings (F12): selected row, and the text being typed for path rows.
    Settings { row: usize, edit: Option<String> },
}

/// F2: the name being edited in place, in the cursor row of `side`.
struct Rename {
    side: usize,
    from: PathBuf,
    name: String,
    /// Caret position in chars.
    caret: usize,
    /// Other end of the selection (in chars), if text is selected.
    anchor: Option<usize>,
    error: Option<String>,
}

impl Rename {
    fn byte(&self, i: usize) -> usize {
        self.name.char_indices().nth(i).map_or(self.name.len(), |(b, _)| b)
    }

    /// Selected range in chars, if not empty.
    fn selection(&self) -> Option<(usize, usize)> {
        let a = self.anchor?;
        (a != self.caret).then(|| (a.min(self.caret), a.max(self.caret)))
    }

    fn selected(&self) -> Option<&str> {
        self.selection().map(|(lo, hi)| &self.name[self.byte(lo)..self.byte(hi)])
    }

    /// Removes the selection; false if nothing was selected.
    fn delete_selection(&mut self) -> bool {
        let Some((lo, hi)) = self.selection() else { return false };
        let range = self.byte(lo)..self.byte(hi);
        self.name.replace_range(range, "");
        self.caret = lo;
        self.anchor = None;
        true
    }

    /// Types `t` at the caret, replacing the selection.
    fn insert(&mut self, t: &str) {
        self.delete_selection();
        let b = self.byte(self.caret);
        self.name.insert_str(b, t);
        self.caret += t.chars().count();
        self.anchor = None;
        self.error = None;
    }

    /// Moves the caret; with Shift the selection grows, without it ends.
    fn move_to(&mut self, to: usize, select: bool) {
        if select {
            self.anchor.get_or_insert(self.caret);
        } else {
            self.anchor = None;
        }
        self.caret = to;
    }
}

/// Ctrl+C / Ctrl+X / Ctrl+V and Shift+Insert, as 'c', 'x', 'v'.
fn clip_key(key: &Key, m: keyboard::Modifiers) -> Option<char> {
    match key.as_ref() {
        Key::Character(c) if m.command() && !m.shift() && !m.alt() => match c.to_lowercase().as_str() {
            "c" => Some('c'),
            "x" => Some('x'),
            "v" => Some('v'),
            _ => None,
        },
        Key::Named(Named::Insert) if m.shift() => Some('v'),
        _ => None,
    }
}

fn editor_id() -> Id {
    Id::new("editor")
}

/// Reads a text file for the editor; refuses binary and very large files.
fn load_edit(path: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() as usize > 4 * preview::VIEW_MAX {
        return Err(format!("Too large for the editor · {}", human_size(meta.len())));
    }
    let buf = std::fs::read(path).map_err(|e| e.to_string())?;
    if buf.iter().take(8192).any(|&b| b == 0) {
        return Err("Binary file, not editable here".into());
    }
    String::from_utf8(buf).map_err(|_| "Not UTF-8 text, not editable here".to_string())
}

fn quick_id() -> Id {
    Id::new("quick")
}

/// Size and date of a file on disk, for viewer headers.
fn file_info(path: &Path) -> String {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => "folder".to_string(),
        Ok(m) => {
            let date = m.modified().ok().map(|t: SystemTime| human_date(t)).unwrap_or_default();
            format!("{} · {date}", human_size(m.len()))
        }
        Err(_) => String::new(),
    }
}

fn view_id() -> Id {
    Id::new("viewer")
}

/// Extracts a file from an archive into a temporary folder and opens it there.
fn open_from_archive(path: &Path) -> Result<(), String> {
    let (archive, inner) = archive::split(path).ok_or("not inside an archive")?;
    let dir = temp_dir().map_err(|e| e.to_string())?;
    archive::extract(&archive, std::slice::from_ref(&inner), &dir, |_, _| true).map_err(|e| e.to_string())?;
    guppy_core::platform::open(&dir.join(inner)).map_err(|e| e.to_string())
}

/// Fresh temporary folder for files taken out of archives.
fn temp_dir() -> std::io::Result<PathBuf> {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("guppy-{}/{n}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Rows of the settings overlay, in display order: (section, label).
const SETTINGS: &[(&str, &str)] = &[
    ("View", "Show hidden files"),
    ("View", "Default sort"),
    ("View", "Folder sizes automatically"),
    ("Ask before", "Copy"),
    ("Ask before", "Move"),
    ("Ask before", "Move to trash"),
    ("Ask before", "Delete permanently"),
    ("Start folders", "Left pane"),
    ("Start folders", "Right pane"),
    ("Network drives", "Parallel accesses"),
    ("Startup", "Show key help"),
    ("Appearance", "Look"),
    ("Appearance", "Key bar at the bottom"),
];

/// Rows 7 and 8 hold paths and are edited as text.
fn setting_is_text(row: usize) -> bool {
    matches!(row, 7 | 8)
}

fn setting_value(c: &Config, row: usize) -> String {
    let on = |b: bool| if b { "on" } else { "off" }.to_string();
    let path = |p: &str| if p.trim().is_empty() { "startup folder".to_string() } else { p.to_string() };
    match row {
        0 => on(c.view.show_hidden),
        1 => sort_from(c.view.sort).label().trim_end_matches(" ↓").to_string(),
        2 => on(c.view.dir_sizes),
        3 => on(c.confirm.copy),
        4 => on(c.confirm.move_),
        5 => on(c.confirm.trash),
        6 => on(c.confirm.delete),
        7 => path(&c.start.left),
        8 => path(&c.start.right),
        9 => c.network.threads.to_string(),
        10 => on(c.start.help),
        11 => c.view.look.name().to_string(),
        _ => on(c.view.keybar),
    }
}

/// Changes a non-text setting one step; `dir` is -1 or +1.
fn setting_step(c: &mut Config, row: usize, dir: i32) {
    let flip = |b: &mut bool| *b = !*b;
    match row {
        0 => flip(&mut c.view.show_hidden),
        1 => {
            let order = [SortBy::Name, SortBy::Size, SortBy::Date];
            let i = order.iter().position(|s| *s == c.view.sort).unwrap_or(0) as i32;
            c.view.sort = order[(i + dir).rem_euclid(3) as usize];
        }
        2 => flip(&mut c.view.dir_sizes),
        3 => flip(&mut c.confirm.copy),
        4 => flip(&mut c.confirm.move_),
        5 => flip(&mut c.confirm.trash),
        6 => flip(&mut c.confirm.delete),
        9 => c.network.threads = (c.network.threads as i32 + dir).clamp(1, 32) as usize,
        10 => flip(&mut c.start.help),
        11 => c.view.look = c.view.look.step(dir),
        12 => flip(&mut c.view.keybar),
        _ => {}
    }
}

/// Everything the command palette offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cmd {
    NewTab,
    CloseTab,
    NextTab,
    Copy,
    Move,
    Trash,
    Delete,
    NewDir,
    Rename,
    Undo,
    Attrs,
    EmptyTrash,
    Link,
    View,
    Edit,
    EditExternal,
    Quick,
    Map,
    Search,
    Mark,
    Unmark,
    Invert,
    Hidden,
    Sort,
    Sizes,
    Jump,
    Back,
    Forward,
    Parent,
    Home,
    Switch,
    DrivesLeft,
    DrivesRight,
    Bookmark,
    Favorite,
    Jobs,
    PauseJob,
    Look,
    Settings,
    Help,
}

/// Palette entries: (label, key, command).
const COMMANDS: &[(&str, &str, Cmd)] = &[
    ("Copy to other pane", "F5", Cmd::Copy),
    ("Move to other pane", "F6", Cmd::Move),
    ("Move to trash", "F8", Cmd::Trash),
    ("Delete permanently", "Shift+F8", Cmd::Delete),
    ("New folder", "F7", Cmd::NewDir),
    ("Rename", "F2", Cmd::Rename),
    ("Undo last move, rename or trash", "Ctrl+Z", Cmd::Undo),
    ("Permissions & owner", "Alt+Enter", Cmd::Attrs),
    ("Create symlink in other pane", "Ctrl+Shift+F5", Cmd::Link),
    ("View file", "F3", Cmd::View),
    ("Edit file", "F4", Cmd::Edit),
    ("Edit in external editor", "Shift+F4", Cmd::EditExternal),
    ("Quick view in other pane", "Ctrl+Q", Cmd::Quick),
    ("Space map in other pane", "Ctrl+U", Cmd::Map),
    ("Search text in files", "Ctrl+F", Cmd::Search),
    ("Mark by pattern", "+", Cmd::Mark),
    ("Unmark by pattern", "-", Cmd::Unmark),
    ("Invert marks", "*", Cmd::Invert),
    ("Show / hide hidden files", "Ctrl+H", Cmd::Hidden),
    ("Sort: name, size, date", "Ctrl+S", Cmd::Sort),
    ("Recompute folder sizes", "Ctrl+R", Cmd::Sizes),
    ("Jump to folder", "Ctrl+G", Cmd::Jump),
    ("New tab", "Ctrl+T", Cmd::NewTab),
    ("Close tab", "Ctrl+W", Cmd::CloseTab),
    ("Next tab", "Ctrl+Tab", Cmd::NextTab),
    ("Back", "Alt+←", Cmd::Back),
    ("Forward", "Alt+→", Cmd::Forward),
    ("Parent folder", "Backspace", Cmd::Parent),
    ("Home folder", "", Cmd::Home),
    ("Switch pane", "Tab", Cmd::Switch),
    ("Drives & bookmarks: left pane", "Alt+F1", Cmd::DrivesLeft),
    ("Drives & bookmarks: right pane", "Alt+F2", Cmd::DrivesRight),
    ("Bookmark this folder", "Ctrl+D", Cmd::Bookmark),
    ("Favorite button for this folder (add / remove)", "", Cmd::Favorite),
    ("Running jobs", "Ctrl+J", Cmd::Jobs),
    ("Pause / resume job", "Ctrl+Shift+J", Cmd::PauseJob),
    ("Switch look: paper / matte black / neon", "", Cmd::Look),
    ("Settings", "F12", Cmd::Settings),
    ("Key help", "F1", Cmd::Help),
    ("Empty trash", "", Cmd::EmptyTrash),
];

/// Fuzzy score: every word of the query must appear in order somewhere in the
/// label (words may match in any place); consecutive and word-start hits count
/// more. `None` if a word doesn't match.
fn fuzzy(query: &str, label: &str) -> Option<i32> {
    let label: Vec<char> = label.to_lowercase().chars().collect();
    let word_start = |i: usize| i == 0 || !label[i - 1].is_alphanumeric();
    // One word matched as a subsequence starting at `from`.
    let score_from = |word: &[char], from: usize| -> Option<i32> {
        let (mut score, mut pos, mut last) = (0, from, None::<usize>);
        for &q in word {
            let i = pos + label[pos..].iter().position(|&c| c == q)?;
            score += 1 + if last.is_some_and(|l| l + 1 == i) { 3 } else { 0 } + if word_start(i) { 5 } else { 0 };
            last = Some(i);
            pos = i + 1;
        }
        Some(score * 10 - from as i32)
    };
    let mut total = 0;
    for word in query.to_lowercase().split_whitespace() {
        let word: Vec<char> = word.chars().collect();
        // Best start among the word starts that begin with the right letter, else anywhere.
        let best = (0..label.len()).filter(|&i| word_start(i) && label[i] == word[0]).filter_map(|i| score_from(&word, i)).max();
        total += best.or_else(|| score_from(&word, 0))?;
    }
    Some(total - label.len() as i32 / 4)
}

/// Palette hits for `query`, best first (all commands when empty).
/// Key names as shown: the Mac uses Cmd where Linux and Windows use Ctrl
/// (`modifiers.command()`).
fn keys_label(s: &str) -> String {
    if cfg!(target_os = "macos") { s.replace("Ctrl+", "Cmd+") } else { s.to_string() }
}

/// Indices past `COMMANDS` are the user's actions.
fn palette_hits(query: &str, actions: &[Action]) -> Vec<usize> {
    let labels = COMMANDS.iter().map(|c| c.0).chain(actions.iter().map(|a| a.name.as_str()));
    let mut hits: Vec<(i32, usize)> = labels.enumerate().filter_map(|(i, label)| fuzzy(query, label).map(|s| (s, i))).collect();
    if !query.trim().is_empty() {
        hits.sort_by_key(|(s, i)| (std::cmp::Reverse(*s), *i));
    }
    hits.into_iter().map(|(_, i)| i).collect()
}

/// Quick help sections: heading and (key, action).
const HELP: &[(&str, &[(&str, &str)])] = &[
    ("Navigation", &[
        ("↑ ↓", "move cursor"),
        ("PgUp PgDn", "page up / down"),
        ("Home End", "first / last item"),
        ("Enter", "open folder, archive, file"),
        ("Backspace", "shorten search, else folder up"),
        ("Alt+← Alt+→", "history back / forward"),
        ("Tab", "switch pane"),
        ("Ctrl+T / W", "new / close tab"),
        ("Ctrl+Tab", "next tab (Shift: previous)"),
        ("Ctrl+1 … 9", "go to tab"),
    ]),
    ("Search", &[
        ("a–z …", "filter active pane"),
        ("~ or /", "jump to folder (fuzzy)"),
        ("Ctrl+G", "open jump mode"),
        ("Esc", "clear search"),
    ]),
    ("Files", &[
        ("Insert", "mark and advance"),
        ("Space", "mark (when search is empty)"),
        ("+ / -", "mark / unmark by pattern"),
        ("*", "invert marks"),
        ("F2", "rename"),
        ("F3", "view"),
        ("F4", "edit"),
        ("Shift+F4", "external editor"),
        ("F5", "copy to other pane"),
        ("F6", "move to other pane"),
        ("Drag", "move to pane or folder (Ctrl copies)"),
        ("F7", "new folder"),
        ("F8 Del", "move to trash"),
        ("Ctrl+Z", "undo move, rename, trash"),
        ("Alt+Enter", "permissions & owner"),
        ("Ctrl+Shift+F5", "symlink in other pane"),
        ("Shift+F8/Del", "delete permanently"),
        ("Ctrl+C / X", "copy / cut files to clipboard"),
        ("Ctrl+V", "paste files here"),
        ("Ctrl+J", "running jobs (P pauses, Del cancels)"),
        ("Ctrl+Shift+J", "pause / resume job"),
        ("Ctrl+Q", "quick view in other pane"),
        ("Ctrl+U", "space map in other pane"),
        ("Ctrl+F", "search text in files below"),
        ("Ctrl+P", "command palette"),
        ("Alt+F1 / F2", "drives & bookmarks"),
        ("Ctrl+D", "bookmark this folder"),
    ]),
    ("View", &[
        ("F1", "this help"),
        ("F12", "settings (also Ctrl+Comma)"),
        ("Ctrl+H", "hidden files"),
        ("Ctrl+S", "sort: name, size, date"),
        ("Ctrl+R", "recompute folder sizes"),
    ]),
];

fn op_verb(kind: OpKind) -> &'static str {
    match kind {
        OpKind::Copy => "copy",
        OpKind::Move => "move",
        OpKind::Trash => "trash",
        OpKind::Delete => "permanently delete",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sort {
    /// Folders first, then alphabetical.
    Name,
    /// Largest first, folders and files mixed.
    Size,
    /// Newest first.
    Date,
}

impl Sort {
    fn next(self) -> Self {
        match self {
            Sort::Name => Sort::Size,
            Sort::Size => Sort::Date,
            Sort::Date => Sort::Name,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Sort::Name => "Name",
            Sort::Size => "Size ↓",
            Sort::Date => "Date ↓",
        }
    }
}

/// Size of an entry for display and sorting; folders come from the size computation.
fn entry_size(e: &Entry, sizes: &DirSizes) -> Option<u64> {
    if !e.is_dir() || e.is_virtual() {
        return Some(e.size);
    }
    match sizes.get(&e.path) {
        Some(DirSize::Done(n) | DirSize::Cached(n)) => Some(n),
        _ => None,
    }
}

struct Pane {
    path: PathBuf,
    entries: Vec<Entry>,
    /// Index into the *visible* list (after filter/hidden files).
    cursor: usize,
    marked: HashSet<PathBuf>,
    back: Vec<PathBuf>,
    forward: Vec<PathBuf>,
    /// Typed input: a filter, or a jump target if it starts with `~`/`/`.
    query: String,
    sort: Sort,
    /// Cancels size computation when the pane leaves the folder.
    sizing: Cancel,
    scroll_y: f32,
    view_h: f32,
    /// Where the list sits in the window, for drops from other apps.
    list_bounds: iced::Rectangle,
    /// Smooth scroll target; `None` = no animation.
    scroll_target: Option<f32>,
    id: Id,
    /// Located on a network drive.
    network: bool,
    /// Total and free bytes of the drive (local folders only).
    space: Option<(u64, u64)>,
    /// Showing the inside of an archive (read-only).
    archive: bool,
}

/// What happens after a folder has been read.
#[derive(Clone)]
enum After {
    /// Initial folder at startup.
    Start,
    Nav,
    Back,
    Forward,
    /// Up, with the cursor on the folder we came from.
    Parent(PathBuf),
    /// Same folder, re-read; cursor stays on its entry.
    Reload,
}

/// Folder currently being read in the background.
struct Loading {
    path: PathBuf,
    rx: std::sync::mpsc::Receiver<std::io::Result<Vec<Entry>>>,
    since: std::time::Instant,
    after: After,
}

/// File operations run one after another in the background.
#[derive(Default)]
struct Jobs {
    running: Option<(OpKind, Job, String)>,
    queue: std::collections::VecDeque<(OpKind, Vec<PathBuf>, Option<PathBuf>, String)>,
    /// Notes collected from finished jobs, shown once the queue is empty.
    problems: Vec<String>,
    /// Jobs started since the queue was last empty, for "2 of 3".
    started: usize,
    /// The running job is an undo.
    undoing: bool,
    /// The last move, rename or trashing: label and what to reverse.
    last: Option<(String, Vec<ops::Step>)>,
}

impl Jobs {
    fn busy(&self) -> bool {
        self.running.is_some() || !self.queue.is_empty()
    }
}

/// How long to wait synchronously for a read; local folders are almost always
/// faster and thus appear without an intermediate state.
const LOAD_WAIT: Duration = Duration::from_millis(120);
/// After this, a folder counts as "not responding".
const LOAD_SLOW: Duration = Duration::from_secs(3);

impl Pane {
    fn new(path: PathBuf, id: &'static str) -> Self {
        let mut p = Self {
            path: PathBuf::new(),
            entries: Vec::new(),
            cursor: 0,
            marked: HashSet::new(),
            back: Vec::new(),
            forward: Vec::new(),
            query: String::new(),
            sort: Sort::Name,
            sizing: Cancel::default(),
            scroll_y: 0.0,
            view_h: 400.0,
            list_bounds: iced::Rectangle::default(),
            scroll_target: None,
            id: Id::new(id),
            network: false,
            space: None,
            archive: false,
        };
        p.network = guppy_core::platform::is_network(&path);
        p.path = path;
        p
    }

    /// Shows a freshly read folder.
    fn enter(&mut self, path: PathBuf, entries: Vec<Entry>) {
        self.network = guppy_core::platform::is_network(&path);
        self.archive = !path.is_dir();
        self.space = if self.network || self.archive { None } else { guppy_core::drives::space(&path) };
        self.entries = entries;
        self.path = path;
        self.cursor = 0;
        self.marked.clear();
        self.query.clear();
        // A new folder starts at the top, without animation.
        self.scroll_y = 0.0;
        self.scroll_target = Some(0.0);
    }

    fn visible(&self, hidden: bool, sizes: &DirSizes) -> Vec<&Entry> {
        let f = if is_jump(&self.query) { String::new() } else { self.query.to_lowercase() };
        let mut v: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|e| hidden || !e.hidden)
            .filter(|e| f.is_empty() || e.name.to_lowercase().contains(&f))
            .collect();
        // `entries` is already sorted by name; sort_by is stable.
        match self.sort {
            Sort::Name => {}
            Sort::Size => v.sort_by_key(|e| std::cmp::Reverse(entry_size(e, sizes).unwrap_or(0))),
            Sort::Date => v.sort_by_key(|e| std::cmp::Reverse(e.modified)),
        }
        v
    }
}

struct Guppy {
    theme: guppy_core::Theme,
    panes: [Pane; 2],
    /// The other tabs of each side, in order; the shown one belongs at `tab_at`.
    tabs: [Vec<Pane>; 2],
    tab_at: [usize; 2],
    active: usize,
    show_hidden: bool,
    papers: [Paper; 2],
    scanlines: [Scanlines; 2],
    sizes: DirSizes,
    jumper: Jumper,
    /// Jump mode hits and the selected row.
    jump: Vec<PathBuf>,
    jump_cur: usize,
    shot_wait: u8,
    /// Jump hits still need refreshing after indexing.
    jump_pending: bool,
    dialog: Option<Dialog>,
    rename: Option<Rename>,
    last_frame: Option<std::time::Instant>,
    /// Window width, for the responsive layout.
    width: f32,
    /// Last cursor move, to detect key repeat.
    last_move: Option<std::time::Instant>,
    /// Folders currently being read, per side.
    loads: [Option<Loading>; 2],
    /// Tasks started where no task can be returned, run after the current message.
    later: Vec<Task<Message>>,
    /// Which helper programs were found, checked whenever the settings open.
    tools_found: Vec<bool>,
    drag: Option<Drag>,
    /// Row (side, index) under files dragged in from another app.
    ext_over: Option<(usize, usize)>,
    /// Modifier keys held right now (Ctrl while dropping copies).
    mods: keyboard::Modifiers,
    /// Outcome of the last install started from the settings.
    tools_note: Option<String>,
    /// Git status per pane, and whether it must be read again.
    git: [Option<std::sync::Arc<guppy_core::git::Status>>; 2],
    git_stale: [bool; 2],
    config: Config,
    jobs: Jobs,
    /// Quick view on: the other pane shows the entry under the cursor.
    quick: bool,
    /// The other pane shows the space map.
    map: bool,
    /// Running or finished text search, what it looked for, and its hits.
    search: Option<guppy_core::search::Search>,
    search_for: String,
    search_hits: Vec<guppy_core::search::Hit>,
    /// Lines around the selected hit.
    search_ctx: Vec<(usize, String)>,
    /// Folder the last search ran in.
    search_root_cache: Option<PathBuf>,
    /// Rows that just appeared or took a drop: they glow and fade out.
    glow: Vec<(PathBuf, std::time::Instant)>,
    /// Rows that just disappeared, per side: former position, entry, when; they fold away.
    ghosts: [Vec<(usize, Entry, std::time::Instant)>; 2],
    /// When a dialog opened, a pane became active, the map opened, a picture came in.
    dialog_at: Option<std::time::Instant>,
    active_at: Option<std::time::Instant>,
    map_at: Option<std::time::Instant>,
    picture_at: Option<std::time::Instant>,
    /// Entry the quick view should show, and what it shows now.
    quick_want: Option<PathBuf>,
    quick_view: Option<(PathBuf, ViewBody)>,
    /// Counts cursor changes, so only the latest one loads.
    quick_gen: u64,
    /// Last pattern used with `+` / `-`.
    last_pattern: String,
    /// Window close was requested while jobs run; the next request quits.
    close_armed: bool,
    /// Most folders waiting for their size at once since sizes were last idle, for a progress fraction.
    sizes_peak: usize,
    /// A file being fetched out of an archive to open it.
    opening: Option<String>,
    /// The trash is being read or emptied.
    trash_busy: Option<&'static str>,
}

/// Something that takes a while, shown in the status bar.
/// Files dragged with the mouse from one pane (only inside Guppy).
struct Drag {
    side: usize,
    paths: Vec<PathBuf>,
    /// Where the pointer was first seen after the press; the drag starts 6 px away.
    start: Option<iced::Point>,
    /// Where the pointer is now, for the label that follows it.
    at: Option<iced::Point>,
    active: bool,
    /// Row under the pointer.
    over: Option<(usize, usize)>,
}

struct Activity {
    label: String,
    /// Known progress; `None` shows a moving bar.
    frac: Option<f32>,
    /// Paused, not responding or waiting for an answer.
    stalled: bool,
    on_press: Option<Message>,
}

fn sort_from(s: SortBy) -> Sort {
    match s {
        SortBy::Name => Sort::Name,
        SortBy::Size => Sort::Size,
        SortBy::Date => Sort::Date,
    }
}

impl Guppy {
    fn new() -> (Self, Task<Message>) {
        let home = dirs_home();
        let cwd = std::env::current_dir().unwrap_or_else(|_| home.clone());
        let (config, config_error) = Config::load();
        guppy_core::du::set_network_threads(config.network.threads);
        let mut g = Self {
            theme: load_theme(config.view.look),
            panes: [
                Pane::new(Config::resolve(&config.start.left, &cwd), "pane-left"),
                Pane::new(Config::resolve(&config.start.right, &cwd), "pane-right"),
            ],
            tabs: [Vec::new(), Vec::new()],
            tab_at: [0, 0],
            active: 0,
            show_hidden: config.view.show_hidden,
            papers: [Paper::new(1), Paper::new(2)],
            scanlines: [Scanlines::default(), Scanlines::default()],
            sizes: DirSizes::default_cache_file().map(DirSizes::with_cache).unwrap_or_default(),
            jumper: Jumper::load(),
            jump: Vec::new(),
            jump_cur: 0,
            shot_wait: 0,
            jump_pending: false,
            dialog: None,
            rename: None,
            last_frame: None,
            width: 1200.0,
            last_move: None,
            loads: [None, None],
            later: Vec::new(),
            tools_found: Vec::new(),
            drag: None,
            ext_over: None,
            mods: keyboard::Modifiers::default(),
            tools_note: None,
            git: [None, None],
            git_stale: [true, true],
            config,
            jobs: Jobs::default(),
            quick: false,
            map: false,
            search: None,
            search_for: String::new(),
            search_hits: Vec::new(),
            search_ctx: Vec::new(),
            search_root_cache: None,
            glow: Vec::new(),
            ghosts: [Vec::new(), Vec::new()],
            dialog_at: None,
            active_at: None,
            map_at: None,
            picture_at: None,
            quick_want: None,
            quick_view: None,
            quick_gen: 0,
            last_pattern: "*".into(),
            close_armed: false,
            sizes_peak: 0,
            opening: None,
            trash_busy: None,
        };
        let sort = sort_from(g.config.view.sort);
        g.panes.iter_mut().for_each(|p| p.sort = sort);
        g.restore_tabs();
        // Developer aid for screenshots: `GUPPY_SCREENSHOT_VIEW=map` opens the space map,
        // `search:text` the text search for "text".
        let shot_view = std::env::var("GUPPY_SCREENSHOT_VIEW").unwrap_or_default();
        g.map = shot_view == "map";
        if let Some(q) = shot_view.strip_prefix("search:") {
            g.open_search();
            if let Some(Dialog::Search { query, .. }) = &mut g.dialog {
                *query = q.to_string();
            }
            g.on_search_key(Key::Named(Named::Enter), None);
        }
        if let Some(e) = config_error {
            g.dialog = Some(Dialog::Report { title: "Settings not loaded", problems: vec![e] });
        } else if g.config.start.help && std::env::var_os("GUPPY_SCREENSHOT").is_none() {
            g.dialog = Some(Dialog::Help { startup: true });
        }
        for side in 0..2 {
            let path = g.panes[side].path.clone();
            g.load(side, path, After::Start);
        }
        // Developer aid: preset input, e.g. `GUPPY_QUERY=~gu`.
        if let Ok(q) = std::env::var("GUPPY_QUERY") {
            g.set_query(q);
        }
        // `GUPPY_QUICK=1` starts with the quick view open.
        g.quick = std::env::var_os("GUPPY_QUICK").is_some();
        if std::env::var("GUPPY_SORT").as_deref() == Ok("size") {
            g.panes.iter_mut().for_each(|p| p.sort = Sort::Size);
        }
        // `GUPPY_DIALOG=copy` opens the copy dialog, `=run` also confirms it.
        if let Ok(d) = std::env::var("GUPPY_DIALOG") {
            if d == "view" {
                let task = g.open_view();
                return (g, task);
            }
            if d == "rename" {
                g.start_rename();
                return (g, Task::none());
            }
            if d == "attrs" {
                g.open_attrs();
                return (g, Task::none());
            }
            if d == "settings" {
                g.dialog = Some(Dialog::Settings { row: 2, edit: None });
                return (g, Task::none());
            }
            if d == "splash" {
                g.dialog = Some(Dialog::Help { startup: true });
                return (g, Task::none());
            }
            if let Some(q) = d.strip_prefix("palette") {
                g.dialog = Some(Dialog::Palette { query: q.trim_start_matches(':').to_string(), cur: 0 });
                return (g, Task::none());
            }
            if d == "drives" {
                g.open_drives(0);
                return (g, Task::none());
            }
            if d == "help" {
                g.dialog = Some(Dialog::Help { startup: false });
                return (g, Task::none());
            }
            g.ask(OpKind::Copy);
            if d == "run" {
                g.confirm_dialog();
            }
        }
        (g, Task::none())
    }

    fn title(&self) -> String {
        format!("Guppy — {}", self.panes[self.active].path.display())
    }

    fn theme(&self) -> iced::Theme {
        let t = &self.theme;
        iced::Theme::custom(
            "Guppy",
            iced::theme::Palette {
                background: rgb(t.background),
                text: rgb(t.foreground),
                primary: rgb(t.accent),
                success: rgb(t.marked),
                warning: rgb(t.accent),
                danger: rgb(t.red),
            },
        )
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut dirs: Vec<PathBuf> = self.panes.iter().map(|p| p.path.clone()).collect();
        if let Some(theme_dir) = guppy_core::Theme::omarchy_path().and_then(|p| p.parent().map(Path::to_path_buf)) {
            // The theme directory and its parent (Omarchy replaces it on switch).
            dirs.extend(theme_dir.parent().map(Path::to_path_buf));
            dirs.push(theme_dir);
        }
        dirs.extend(Config::path().and_then(|p| p.parent().map(Path::to_path_buf)));
        let mut keys = Subscription::batch([
            keyboard::listen().map(Message::Key),
            iced::window::resize_events().map(|(_, size)| Message::Resized(size)),
            iced::window::close_requests().map(Message::CloseRequested),
            watch::changes(watch::Targets { dirs }).map(Message::FsChanged),
        ]);
        #[cfg(target_os = "linux")]
        {
            keys = Subscription::batch([keys, dnd::events().map(Message::Dnd)]);
        }
        // The pointer is only followed while a row is held.
        if self.drag.is_some() {
            keys = Subscription::batch([
                keys,
                iced::event::listen_with(|e, _, _| match e {
                    iced::Event::Mouse(iced::mouse::Event::CursorMoved { position }) => Some(Message::DragMoved(position)),
                    iced::Event::Mouse(iced::mouse::Event::ButtonReleased(iced::mouse::Button::Left)) => Some(Message::DragReleased),
                    _ => None,
                }),
            ]);
        }
        if self.panes.iter().any(|p| p.scroll_target.is_some()) || self.animating() {
            keys = Subscription::batch([keys, iced::window::frames().map(Message::Frame)]);
        }
        // Only while sizes are computed; no idle ticking otherwise.
        let running = self.jobs.busy() || !self.jobs.problems.is_empty();
        let loading = self.loads.iter().any(Option::is_some) || self.opening.is_some() || self.trash_busy.is_some();
        let searching = self.search.as_ref().is_some_and(|s| !s.finished());
        if self.sizes.busy() || self.jump_pending || running || loading || searching || std::env::var_os("GUPPY_SCREENSHOT").is_some() {
            Subscription::batch([keys, iced::time::every(Duration::from_millis(120)).map(|_| Message::Tick)])
        } else {
            keys
        }
    }

    fn current(&self) -> Option<&Entry> {
        let p = &self.panes[self.active];
        p.visible(self.show_hidden, &self.sizes).get(p.cursor).copied()
    }

    /// In a narrow window the date column is dropped.
    fn show_dates(&self) -> bool {
        self.width >= 760.0
    }

    fn jumping(&self) -> bool {
        is_jump(&self.panes[self.active].query)
    }

    fn update(&mut self, msg: Message) -> Task<Message> {
        let dialog = self.dialog.as_ref().map(std::mem::discriminant);
        let (active, map) = (self.active, self.map);
        let map_path = self.panes[self.active].path.clone();
        let task = self.handle(msg);
        // Start the small transitions when something appears.
        let now = std::time::Instant::now();
        let dialog_now = self.dialog.as_ref().map(std::mem::discriminant);
        if dialog_now.is_some() && dialog_now != dialog {
            self.dialog_at = Some(now);
        }
        if self.active != active {
            self.active_at = Some(now);
        }
        if self.map && (!map || self.panes[self.active].path != map_path) {
            self.map_at = Some(now);
        }
        let later = Task::batch(std::mem::take(&mut self.later));
        Task::batch([task, later, self.sync_quick(), self.sync_git()])
    }

    /// Follows the cursor: after a short rest, loads the entry for the quick view.
    fn sync_quick(&mut self) -> Task<Message> {
        if !self.quick {
            return Task::none();
        }
        let want = if self.jumping() { None } else { self.current().map(|e| e.path.clone()) };
        if want == self.quick_want {
            return Task::none();
        }
        self.quick_want = want;
        self.quick_gen += 1;
        let generation = self.quick_gen;
        Task::perform(smol::Timer::after(Duration::from_millis(90)), move |_| Message::QuickDue(generation))
    }

    /// Reads the git status of folders that changed, off the UI thread.
    fn sync_git(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        for side in 0..2 {
            if !std::mem::take(&mut self.git_stale[side]) {
                continue;
            }
            let p = &self.panes[side];
            if p.archive || p.network {
                self.git[side] = None;
                continue;
            }
            let path = p.path.clone();
            let dir = path.clone();
            tasks.push(Task::perform(smol::unblock(move || guppy_core::git::status(&dir).map(std::sync::Arc::new)), move |st| {
                Message::Git(side, path.clone(), st)
            }));
        }
        Task::batch(tasks)
    }

    fn handle(&mut self, msg: Message) -> Task<Message> {
        if let Message::Key(keyboard::Event::ModifiersChanged(m)) = &msg {
            self.mods = *m;
        }
        if let Message::Key(keyboard::Event::KeyPressed { key: Key::Named(Named::Escape), .. }) = &msg
            && self.drag.take().is_some_and(|d| d.active)
        {
            return Task::none();
        }
        if matches!(msg, Message::TrashListed(_) | Message::TrashEmptied(_)) {
            self.trash_busy = None;
        }
        if matches!(msg, Message::Tick) {
            self.poll_jobs();
            self.poll_search();
            let pending = self.sizes.pending();
            self.sizes_peak = if pending == 0 { 0 } else { self.sizes_peak.max(pending) };
        }
        match msg {
            // On the file list (empty search): files to / from the clipboard.
            Message::Key(keyboard::Event::KeyPressed { key, modifiers, .. })
                if self.dialog.is_none()
                    && self.rename.is_none()
                    && self.panes[self.active].query.is_empty()
                    && clip_key(&key, modifiers).is_some() =>
            {
                if clip_key(&key, modifiers) == Some('v') {
                    return Task::perform(smol::unblock(clip::get), Message::FilesPasted);
                }
                if self.read_only() {
                    return Task::none();
                }
                let paths = self.selection();
                if !paths.is_empty()
                    && let Err(e) = clip::put(&paths, clip_key(&key, modifiers) == Some('x'))
                {
                    self.dialog = Some(Dialog::Report { title: "Clipboard", problems: vec![e] });
                }
                return Task::none();
            }
            Message::FilesPasted(None) => {}
            Message::FilesPasted(Some((sources, cut))) => {
                let target = self.panes[self.active].path.clone();
                if self.read_only() {
                } else if sources.iter().all(|s| s.parent() == Some(target.as_path())) {
                    self.dialog = Some(Dialog::Report { title: "Not possible", problems: vec!["The files are already in this folder.".into()] });
                } else {
                    self.ask_with(if cut { OpKind::Move } else { OpKind::Copy }, sources, Some(target));
                }
            }
            // Copy / cut / paste in every one-line text field.
            Message::Key(keyboard::Event::KeyPressed { key, modifiers, .. })
                if clip_key(&key, modifiers).is_some() && self.field_mut().is_some() =>
            {
                let op = clip_key(&key, modifiers).expect("checked above");
                if op == 'v' {
                    return iced::clipboard::read().map(Message::Paste);
                }
                // Rename with a selection: only the selected part.
                let partial = self.rename.as_mut().and_then(|r| {
                    let part = r.selected()?.to_string();
                    if op == 'x' {
                        r.delete_selection();
                        r.error = None;
                    }
                    Some(part)
                });
                let copied = match partial {
                    Some(p) => p,
                    None => {
                        let field = self.field_mut().expect("checked above");
                        let all = if op == 'x' { std::mem::take(field) } else { field.clone() };
                        if let Some(r) = &mut self.rename {
                            r.caret = r.caret.min(r.name.chars().count());
                            r.anchor = None;
                        }
                        all
                    }
                };
                // Empty field: leave the clipboard alone.
                if copied.is_empty() {
                    return Task::none();
                }
                return iced::clipboard::write(copied);
            }
            Message::Paste(Some(clip)) => {
                // One-line fields: newlines become spaces.
                let clip = clip.trim_end_matches(['\n', '\r']).replace(['\n', '\r'], " ");
                if let Some(r) = &mut self.rename {
                    r.insert(&clip);
                } else if self.dialog.is_none() {
                    let q = format!("{}{clip}", self.panes[self.active].query);
                    self.set_query(q);
                } else if let Some(field) = self.field_mut() {
                    field.push_str(&clip);
                }
            }
            Message::Paste(None) => {}
            Message::Key(keyboard::Event::KeyPressed { key, modifiers, text, .. }) if self.rename.is_some() => {
                self.on_rename_key(key, modifiers, text.as_deref());
                return Task::none();
            }
            Message::Key(keyboard::Event::KeyPressed { key, modifiers, .. }) if matches!(self.dialog, Some(Dialog::Edit { .. })) => {
                // Only reaches here when the editor itself is not focused.
                return match key.as_ref() {
                    Key::Named(Named::Escape) => self.update(Message::EditClose),
                    Key::Character("s") if modifiers.command() => self.update(Message::EditSave),
                    _ => operation::focus(editor_id()),
                };
            }
            Message::Key(keyboard::Event::KeyPressed { key, modifiers, text, .. }) if self.dialog.is_some() => {
                if matches!(self.dialog, Some(Dialog::View { .. })) {
                    return self.on_view_key(key);
                }
                if matches!(self.dialog, Some(Dialog::Palette { .. })) {
                    return self.on_palette_key(key, text.as_deref());
                }
                if matches!(self.dialog, Some(Dialog::Search { .. })) {
                    self.on_search_key(key, text.as_deref());
                    return Task::none();
                }
                if matches!(self.dialog, Some(Dialog::Drives { .. })) {
                    return self.on_drives_key(key, modifiers, text.as_deref());
                }
                self.on_dialog_key(key, text.as_deref());
                return Task::none();
            }
            Message::Key(keyboard::Event::KeyPressed { key, modifiers, text, .. }) => {
                return self.on_key(key, modifiers, text.as_deref());
            }
            Message::KeyBar(key, modifiers) => return self.on_key(key, modifiers, None),
            Message::Tick if self.jump_pending => {
                // Subfolder index is growing: refresh hits, keep selection.
                // One last time after indexing ends.
                self.jump_pending = self.jumper.indexing();
                if self.jumping() {
                    let cur = self.jump_cur;
                    self.update_jump();
                    self.jump_cur = cur.min(self.jump.len().saturating_sub(1));
                }
                return Task::none();
            }
            Message::Tick if self.loads.iter().any(Option::is_some) => {
                self.poll_loads();
                return Task::none();
            }
            Message::Tick if std::env::var_os("GUPPY_SCREENSHOT").is_some() && !self.sizes.busy() && !self.jump_pending => {
                // Wait a few ticks so the final state is drawn.
                self.shot_wait += 1;
                if self.shot_wait < 4 {
                    return Task::none();
                }
                return iced::window::latest().and_then(iced::window::screenshot).map(Message::Shot);
            }
            Message::Key(_) | Message::Tick => return Task::none(),
            // Not while typing a name or with a dialog open (the mouse may just pass by).
            Message::HoverPane(side) if self.rename.is_none() && self.dialog.is_none() && !self.quick && !self.map => self.active = side,
            Message::HoverPane(_) => {}
            Message::GoTo(side, path) => {
                self.active = side;
                self.navigate(path);
            }
            Message::Favorite(side, slot) => {
                self.active = side;
                let cwd = self.panes[side].path.clone();
                match self.config.favorites.get(slot).filter(|f| !f.path.is_empty()) {
                    Some(f) => self.navigate(Config::resolve(&f.path, &cwd)),
                    None => self.set_favorite(side, slot),
                }
            }
            Message::SetFavorite(side, slot) => {
                self.active = side;
                self.set_favorite(side, slot);
            }
            Message::TabClick(side, k) => {
                self.active = side;
                self.switch_tab(side, k);
            }
            Message::Select(side, i) => {
                self.rename = None;
                self.active = side;
                self.panes[side].cursor = i;
                self.drag = self.drag_sources(side, i).map(|paths| Drag { side, paths, start: None, at: None, active: false, over: None });
            }
            Message::DragMoved(at) => {
                if let Some(d) = &mut self.drag {
                    let start = *d.start.get_or_insert(at);
                    d.at = Some(at);
                    d.active |= (at.x - start.x).hypot(at.y - start.y) > 6.0;
                }
            }
            Message::DragOver(side, i) => {
                if let Some(d) = &mut self.drag {
                    d.over = Some((side, i));
                }
            }
            Message::DragLeft(side, i) => {
                if let Some(d) = &mut self.drag
                    && d.over == Some((side, i))
                {
                    d.over = None;
                }
            }
            Message::DragReleased => {
                if let Some(d) = self.drag.take().filter(|d| d.active)
                    && let Some((kind, target)) = self.drop_plan(&d)
                {
                    // The folder that took the drop pulses once.
                    self.glow.push((target.clone(), std::time::Instant::now()));
                    self.ask_with(kind, d.paths, Some(target));
                }
            }
            Message::Open(side, i) => {
                self.rename = None;
                self.active = side;
                self.panes[side].cursor = i;
                return self.open_current();
            }
            Message::JumpTo(i) => {
                self.jump_cur = i;
                self.jump_selected();
            }
            Message::Frame(now) => {
                bench_report();
                self.glow.retain(|(_, t)| t.elapsed() < GLOW);
                self.ghosts.iter_mut().for_each(|g| g.retain(|(_, _, t)| t.elapsed() < FOLD));
                return self.animate_scroll(now);
            }
            Message::CloseRequested(id) => {
                // Running jobs: the first request shows them, the second quits.
                if self.jobs.busy() && !self.close_armed {
                    self.close_armed = true;
                    self.dialog = Some(Dialog::Jobs);
                    return Task::none();
                }
                if let Some((_, job, _)) = &self.jobs.running {
                    job.cancel();
                }
                let _ = self.sizes.save();
                self.save_tabs();
                return iced::window::close(id);
            }
            Message::Git(side, path, st) => {
                if self.panes[side].path == path {
                    self.git[side] = st;
                }
            }
            Message::FsChanged(paths) => {
                self.on_fs_changed(&paths);
                return Task::none();
            }
            #[cfg(target_os = "linux")]
            Message::Dnd(dnd::Event::Hover(at)) => {
                // The pane under the files becomes the active one, so its frame shows the
                // target; a folder row under them gets the drop outline.
                self.ext_over = at.and_then(|at| {
                    self.active = self.side_at(at.x);
                    self.row_at(self.active, at)
                });
                // The list only reports its place when it scrolls, so ask for it.
                if at.is_some() {
                    return Task::batch((0..2).map(|side| {
                        iced::widget::selector::find(self.panes[side].id.clone()).map(move |t| Message::ListBounds(side, t.map(|t| t.bounds())))
                    }));
                }
            }
            #[cfg(target_os = "linux")]
            Message::ListBounds(side, bounds) => {
                if let Some(b) = bounds {
                    self.panes[side].list_bounds = b;
                }
            }
            #[cfg(target_os = "linux")]
            Message::Dnd(dnd::Event::Drop { at, paths, copy }) => {
                self.ext_over = None;
                let side = self.side_at(at.x);
                self.active = side;
                if self.panes[side].archive {
                    self.dialog = Some(Dialog::Report { title: "Archive is read-only", problems: vec!["Files can't be copied or moved into an archive.".into()] });
                    return Task::none();
                }
                let d = Drag { side, paths, start: None, at: None, active: true, over: self.row_at(side, at) };
                if std::env::var_os("GUPPY_DND_DEBUG").is_some() {
                    eprintln!("guppy-dnd: drop at {at:?} list {:?} scroll {} row {:?}", self.panes[side].list_bounds, self.panes[side].scroll_y, d.over);
                }
                if let Some((_, target)) = self.drop_plan(&d) {
                    self.glow.push((target.clone(), std::time::Instant::now()));
                    self.ask_with(if copy { OpKind::Copy } else { OpKind::Move }, d.paths, Some(target));
                }
            }
            Message::Resized(size) => {
                self.width = size.width;
                return Task::none();
            }
            Message::Scrolled(side, vp) => {
                let p = &mut self.panes[side];
                p.view_h = vp.bounds().height;
                p.list_bounds = vp.bounds();
                // During animation the list reports the position of an earlier frame;
                // our own value is authoritative then.
                if p.scroll_target.is_none() {
                    p.scroll_y = vp.absolute_offset().y;
                }
                return Task::none();
            }
            Message::Back => self.history(true),
            Message::Forward => self.history(false),
            Message::Parent => self.go_parent(),
            Message::Home => self.navigate(dirs_home()),
            Message::ToggleMark => self.toggle_mark(),
            Message::SwitchSide => self.active = 1 - self.active,
            Message::Op(kind) => self.ask(kind),
            Message::NewDir => self.new_dir(),
            Message::DialogConfirm => self.confirm_dialog(),
            Message::DialogCancel => self.cancel_dialog(),
            Message::OpenSettings => self.open_settings(),
            Message::Viewed(path, body) => {
                if let Some(Dialog::View { path: p, body: b }) = &mut self.dialog
                    && *p == path
                {
                    *b = body;
                }
            }
            Message::EditLoaded(path, result) => {
                if let Some(Dialog::Edit { path: p, content, .. }) = &mut self.dialog
                    && *p == path
                {
                    match result {
                        Ok(s) => {
                            *content = Some(text_editor::Content::with_text(&s));
                            return operation::focus(editor_id());
                        }
                        Err(e) => self.dialog = Some(Dialog::Report { title: "Not editable", problems: vec![e] }),
                    }
                }
            }
            Message::EditAction(action) => {
                if let Some(Dialog::Edit { content: Some(c), dirty, closing, error, .. }) = &mut self.dialog {
                    if action.is_edit() {
                        *dirty = true;
                        *closing = false;
                        *error = None;
                    }
                    c.perform(action);
                }
            }
            Message::EditSave => {
                if let Some(Dialog::Edit { path, content: Some(c), dirty, closing, error }) = &mut self.dialog {
                    match std::fs::write(&*path, c.text()) {
                        Ok(()) => {
                            *dirty = false;
                            *closing = false;
                            *error = None;
                        }
                        Err(e) => *error = Some(e.to_string()),
                    }
                }
            }
            Message::EditClose => {
                if let Some(Dialog::Edit { dirty, closing, .. }) = &mut self.dialog {
                    if *dirty && !*closing {
                        *closing = true;
                    } else {
                        self.dialog = None;
                        self.reload(self.active);
                    }
                }
            }
            Message::Resolve(r) => {
                if let Some(Dialog::Conflict { all, .. }) = self.dialog.take()
                    && let Some((_, job, _)) = &self.jobs.running
                {
                    job.answer(r, all);
                }
            }
            Message::ToggleAll => {
                if let Some(Dialog::Conflict { all, .. }) = &mut self.dialog {
                    *all = !*all;
                }
            }
            Message::OpenJobs => self.dialog = Some(Dialog::Jobs),
            Message::TogglePause => {
                if let Some((_, job, _)) = &self.jobs.running {
                    if job.paused() { job.resume() } else { job.pause() }
                }
            }
            Message::QueueFirst(n) => {
                if let Some(j) = self.jobs.queue.remove(n) {
                    self.jobs.queue.push_front(j);
                }
            }
            Message::QueueDrop(n) => {
                self.jobs.queue.remove(n);
            }
            Message::EmptyTrash => return self.run_cmd(Cmd::EmptyTrash),
            Message::PaletteRun(i) => {
                self.dialog = None;
                return self.run_palette(i);
            }
            Message::DrivePick(i) => {
                if let Some(Dialog::Drives { cur, .. }) = &mut self.dialog {
                    *cur = i;
                }
                self.pick_drive(false);
            }
            Message::ToggleMap => {
                self.map = !self.map;
                self.quick = false;
            }
            Message::MapClick(path) => {
                let p = &self.panes[self.active];
                if path.is_dir() {
                    self.navigate(path);
                } else if let Some(i) = p.visible(self.show_hidden, &self.sizes).iter().position(|e| e.path == path) {
                    self.panes[self.active].cursor = i;
                    return self.scroll_into_view();
                }
            }
            Message::ToggleQuick => {
                self.map = false;
                self.quick = !self.quick;
                self.quick_want = None;
                self.quick_view = None;
            }
            Message::QuickDue(generation) => {
                if generation == self.quick_gen
                    && let Some(path) = self.quick_want.clone()
                {
                    return Task::perform(
                        async move {
                            let p = path.clone();
                            (path, smol::unblock(move || preview::load(&p, true)).await)
                        },
                        |(p, b)| Message::QuickLoaded(p, b),
                    );
                }
            }
            Message::QuickLoaded(path, body) => {
                if self.quick_want.as_ref() == Some(&path) {
                    if matches!(body, ViewBody::Picture { .. } | ViewBody::Image) {
                        self.picture_at = Some(std::time::Instant::now());
                    }
                    self.quick_view = Some((path, body));
                    return operation::scroll_to(quick_id(), AbsoluteOffset { x: None, y: Some(0.0) });
                }
            }
            Message::CancelJobs(everything) => {
                if everything {
                    self.jobs.queue.clear();
                }
                if let Some((_, job, _)) = &self.jobs.running {
                    job.cancel();
                }
                if matches!(self.dialog, Some(Dialog::Conflict { .. })) {
                    self.dialog = None;
                }
            }
            Message::TrashListed(Ok(items)) if items.is_empty() => {
                self.dialog = Some(Dialog::Report { title: "Trash is empty", problems: Vec::new() });
            }
            Message::TrashListed(Ok(items)) => {
                self.dialog = Some(Dialog::EmptyTrash { items });
            }
            Message::TrashListed(Err(e)) | Message::TrashEmptied(Err(e)) => {
                self.dialog = Some(Dialog::Report { title: "Trash not emptied", problems: vec![e] });
            }
            Message::TrashEmptied(Ok(())) => {
                self.jobs.last = self.jobs.last.take().filter(|(_, steps)| !steps.iter().any(|s| matches!(s, ops::Step::Trashed(_))));
                self.reload(0);
                self.reload(1);
            }
            Message::Opened(Ok(())) => self.opening = None,
            Message::Opened(Err(e)) => {
                self.opening = None;
                self.dialog = Some(Dialog::Report { title: "Not opened", problems: vec![e] });
            }
            Message::ToggleStartHelp => {
                let mut new = self.config.clone();
                new.start.help = !new.start.help;
                self.set_config(new);
            }
            Message::SettingClick(row) => {
                if let Some(Dialog::Settings { row: sel, edit }) = &mut self.dialog {
                    *sel = row;
                    *edit = None;
                }
                self.settings_activate(1);
            }
            Message::ToggleHidden => {
                self.show_hidden = !self.show_hidden;
                self.panes.iter_mut().for_each(|p| p.cursor = 0);
            }
            Message::Shot(shot) => {
                if let Some(path) = std::env::var_os("GUPPY_SCREENSHOT") {
                    save_ppm(&path, &shot);
                }
                return iced::exit();
            }
        }
        self.scroll_into_view()
    }

    /// Keyboard model: letters filter the active pane, a leading `~`/`/` jumps.
    fn on_key(&mut self, key: Key, m: keyboard::Modifiers, typed: Option<&str>) -> Task<Message> {
        let jumping = self.jumping();
        let query_empty = self.panes[self.active].query.is_empty();
        let len = if jumping { self.jump.len() } else { self.panes[self.active].visible(self.show_hidden, &self.sizes).len() };
        let page = (self.panes[self.active].view_h / ROW_H).max(1.0) as usize;
        let cur = if jumping { self.jump_cur } else { self.panes[self.active].cursor };
        if let Some(i) = self.action_for(&key, m) {
            self.run_action(i);
            return Task::none();
        }
        let mut move_to = |i: usize| {
            let i = i.min(len.saturating_sub(1));
            if jumping { self.jump_cur = i } else { self.panes[self.active].cursor = i }
        };
        match key.as_ref() {
            Key::Named(Named::ArrowDown) => move_to(cur + 1),
            Key::Named(Named::ArrowUp) => move_to(cur.saturating_sub(1)),
            Key::Named(Named::PageDown) => move_to(cur + page),
            Key::Named(Named::PageUp) => move_to(cur.saturating_sub(page)),
            Key::Named(Named::Home) => move_to(0),
            Key::Named(Named::End) => move_to(usize::MAX),
            Key::Named(Named::F1) if m.alt() => self.open_drives(0),
            Key::Named(Named::F2) if m.alt() => self.open_drives(1),
            Key::Named(Named::F1) => self.dialog = Some(Dialog::Help { startup: false }),
            Key::Named(Named::F2) => {
                if !self.read_only() {
                    self.start_rename();
                }
            }
            Key::Named(Named::F3) => return self.open_view(),
            Key::Named(Named::F4) if m.shift() => self.edit_external(),
            Key::Named(Named::F4) => return self.edit_current(),
            Key::Named(Named::F12) => return self.update(Message::OpenSettings),
            Key::Character(",") if m.command() => return self.update(Message::OpenSettings),
            Key::Named(Named::Enter) if m.alt() && !jumping => self.open_attrs(),
            Key::Named(Named::Enter) if jumping => self.jump_selected(),
            Key::Named(Named::Enter) => return self.open_current(),
            Key::Named(Named::Backspace) if !query_empty => {
                let mut q = self.panes[self.active].query.clone();
                q.pop();
                self.set_query(q);
            }
            Key::Named(Named::Backspace) => self.go_parent(),
            // Esc first cancels a pending folder change, then the search.
            Key::Named(Named::Escape) if self.loads[self.active].is_some() => self.loads[self.active] = None,
            Key::Named(Named::Escape) => self.set_query(String::new()),
            Key::Named(Named::Tab) if m.command() => self.step_tab(if m.shift() { -1 } else { 1 }),
            Key::Named(Named::Tab) => self.active = 1 - self.active,
            Key::Character(c) if m.command() && !m.shift() && matches!(c.as_ref(), "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9") => {
                let k = c.parse::<usize>().unwrap_or(1) - 1;
                self.switch_tab(self.active, k);
            }
            Key::Character("t") if m.command() => self.new_tab(),
            Key::Character("w") if m.command() => self.close_tab(),
            Key::Named(Named::Insert) => self.toggle_mark(),
            Key::Named(Named::Space) if query_empty => self.toggle_mark(),
            Key::Named(Named::ArrowLeft) if m.alt() => self.history(true),
            Key::Named(Named::ArrowRight) if m.alt() => self.history(false),
            Key::Named(Named::F5) if m.command() && m.shift() => self.make_links(),
            Key::Named(Named::F5) => self.ask(OpKind::Copy),
            Key::Named(Named::F6) => self.ask(OpKind::Move),
            Key::Named(Named::F7) => self.new_dir(),
            Key::Named(Named::F8 | Named::Delete) if m.shift() => self.ask(OpKind::Delete),
            Key::Named(Named::F8 | Named::Delete) => self.ask(OpKind::Trash),
            Key::Character("g") if m.command() => self.set_query("~".into()),
            // The cursor keeps its row; the entries reorder beneath it.
            Key::Character("s") if m.command() => self.cycle_sort(),
            Key::Character("h") if m.command() => return self.update(Message::ToggleHidden),
            Key::Character("j" | "J") if m.command() && m.shift() => {
                let _ = self.update(Message::TogglePause);
            }
            Key::Character("j") if m.command() => self.dialog = Some(Dialog::Jobs),
            Key::Character("q") if m.command() => return self.update(Message::ToggleQuick),
            Key::Character("u") if m.command() => return self.update(Message::ToggleMap),
            Key::Character("f") if m.command() => self.open_search(),
            Key::Character("d") if m.command() => self.bookmark_here(),
            Key::Character("p") if m.command() => self.dialog = Some(Dialog::Palette { query: String::new(), cur: 0 }),
            // On the file list (empty search): `+` / `-` mark by pattern, `*` inverts.
            _ if query_empty && !m.command() && !m.control() && !m.alt() && matches!(typed, Some("+" | "-")) => {
                self.dialog = Some(Dialog::Mark { add: typed == Some("+"), pattern: self.last_pattern.clone() });
            }
            _ if query_empty && !m.command() && !m.control() && !m.alt() && typed == Some("*") => self.invert_marks(),
            Key::Character("r") if m.command() => self.recompute_sizes(),
            Key::Character("z") if m.command() => self.undo(),
            // Anything printable without Ctrl/Alt goes into the input.
            _ => match typed {
                Some(t) if !m.control() && !m.alt() && !m.logo() && t.chars().all(|c| !c.is_control()) => {
                    let q = format!("{}{t}", self.panes[self.active].query);
                    self.set_query(q);
                }
                _ => return Task::none(),
            },
        }
        self.scroll_into_view()
    }

    /// Action target: marked visible entries, otherwise the one under the cursor.
    fn selection(&self) -> Vec<PathBuf> {
        let p = &self.panes[self.active];
        let visible = p.visible(self.show_hidden, &self.sizes);
        let marked: Vec<PathBuf> = visible.iter().filter(|e| p.marked.contains(&e.path)).map(|e| e.path.clone()).collect();
        if !marked.is_empty() {
            return marked;
        }
        visible.get(p.cursor).map(|e| vec![e.path.clone()]).unwrap_or_default()
    }

    /// Takes over a changed config file. View settings only apply when they
    /// changed in the file, so toggles made in the app are not overwritten.
    fn apply_config(&mut self) {
        let (new, error) = Config::load();
        if let Some(e) = error {
            self.dialog = Some(Dialog::Report { title: "Settings not loaded", problems: vec![e] });
            return;
        }
        self.use_config(new);
    }

    /// Saves settings changed in the overlay and applies them right away.
    fn set_config(&mut self, new: Config) {
        if let Err(e) = new.save() {
            self.dialog = Some(Dialog::Report { title: "Settings not saved", problems: vec![e.to_string()] });
        }
        self.use_config(new);
    }

    /// Enter/Space/←/→ on the selected settings row.
    fn open_settings(&mut self) {
        self.tools_found = guppy_core::tools::TOOLS.iter().map(|t| guppy_core::tools::find(t.bin).is_some()).collect();
        self.tools_note = None;
        self.dialog = Some(Dialog::Settings { row: 0, edit: None });
    }

    /// Enter on a helper program: installs it in a terminal or opens its download page.
    fn tool_activate(&mut self, k: usize) {
        let Some(t) = guppy_core::tools::TOOLS.get(k) else { return };
        if self.tools_found.get(k) == Some(&true) {
            return;
        }
        self.tools_note = Some(match guppy_core::tools::install(t) {
            Ok(()) if guppy_core::tools::install_command(t).is_some() => format!("Installing {} in a terminal · open the settings again afterwards", t.name),
            Ok(()) => format!("Download page for {} opened in the browser", t.name),
            Err(e) => format!("{}: {e}", t.name),
        });
    }

    fn settings_activate(&mut self, dir: i32) {
        let Some(Dialog::Settings { row, edit }) = &mut self.dialog else { return };
        let row = *row;
        if row >= SETTINGS.len() {
            self.tool_activate(row - SETTINGS.len());
            return;
        }
        if setting_is_text(row) {
            let c = &self.config.start;
            *edit = Some(if row == 7 { c.left.clone() } else { c.right.clone() });
            return;
        }
        let mut new = self.config.clone();
        setting_step(&mut new, row, dir);
        self.set_config(new);
    }

    fn on_settings_key(&mut self, key: Key, typed: Option<&str>) {
        let Some(Dialog::Settings { row, edit }) = &mut self.dialog else { return };
        // Typing a start folder.
        if let Some(text) = edit {
            match key.as_ref() {
                Key::Named(Named::Enter) => {
                    let value = std::mem::take(text);
                    let row = *row;
                    *edit = None;
                    let mut new = self.config.clone();
                    if row == 7 { new.start.left = value } else { new.start.right = value }
                    self.set_config(new);
                }
                Key::Named(Named::Escape) => *edit = None,
                Key::Named(Named::Backspace) => {
                    text.pop();
                }
                _ => {
                    if let Some(t) = typed.filter(|t| t.chars().all(|c| !c.is_control())) {
                        text.push_str(t);
                    }
                }
            }
            return;
        }
        let last = SETTINGS.len() + guppy_core::tools::TOOLS.len() - 1;
        let Some(Dialog::Settings { row, .. }) = &mut self.dialog else { return };
        match key.as_ref() {
            Key::Named(Named::ArrowDown) => *row = (*row + 1).min(last),
            Key::Named(Named::ArrowUp) => *row = row.saturating_sub(1),
            Key::Named(Named::Home) => *row = 0,
            Key::Named(Named::End) => *row = last,
            Key::Named(Named::Enter | Named::Space) | Key::Named(Named::ArrowRight) => self.settings_activate(1),
            Key::Named(Named::ArrowLeft) => self.settings_activate(-1),
            Key::Named(Named::Escape | Named::F12) => self.dialog = None,
            // On a start-folder row, "." takes the folder currently shown in that pane.
            Key::Character(".") if setting_is_text(*row) => {
                let row = *row;
                let mut new = self.config.clone();
                let here = self.jumper.display(&self.panes[row - 7].path);
                if row == 7 { new.start.left = here } else { new.start.right = here }
                self.set_config(new);
            }
            _ => {}
        }
    }

    /// Applies new settings. View settings only change what differs from the
    /// previous settings, so toggles made in the app are not overwritten.
    fn use_config(&mut self, new: Config) {
        let old = std::mem::replace(&mut self.config, new);
        let v = &self.config.view;
        if v.show_hidden != old.view.show_hidden {
            self.show_hidden = v.show_hidden;
        }
        if v.sort != old.view.sort {
            let sort = sort_from(v.sort);
            self.panes.iter_mut().for_each(|p| p.sort = sort);
        }
        if v.look != old.view.look {
            self.theme = load_theme(v.look);
        }
        if v.dir_sizes && !old.view.dir_sizes {
            self.request_sizes(0);
            self.request_sizes(1);
        }
    }

    /// What a press on row `i` would drag: the marked files if the row is one
    /// of them, else just that row.
    fn drag_sources(&self, side: usize, i: usize) -> Option<Vec<PathBuf>> {
        let p = &self.panes[side];
        let visible = p.visible(self.show_hidden, &self.sizes);
        let e = visible.get(i)?;
        if !p.marked.contains(&e.path) {
            return Some(vec![e.path.clone()]);
        }
        Some(visible.iter().filter(|e| p.marked.contains(&e.path)).map(|e| e.path.clone()).collect())
    }

    /// Where a drop would go and whether it copies or moves; `None` if it can't
    /// go there (same folder, into itself, into an archive). Ctrl copies, and
    /// out of an archive it always copies.
    fn drop_plan(&self, d: &Drag) -> Option<(OpKind, PathBuf)> {
        let side = d.over.map_or(self.active, |(s, _)| s);
        let pane = &self.panes[side];
        if pane.archive {
            return None;
        }
        let row = d.over.and_then(|(s, i)| self.panes[s].visible(self.show_hidden, &self.sizes).get(i).map(|e| (e.path.clone(), e.is_dir())));
        let target = match row {
            Some((path, true)) if !d.paths.contains(&path) => path,
            _ => pane.path.clone(),
        };
        if d.paths.iter().any(|p| p.parent() == Some(target.as_path()) || target.starts_with(p)) {
            return None;
        }
        let copy = self.mods.command() || self.panes[d.side].archive;
        Some((if copy { OpKind::Copy } else { OpKind::Move }, target))
    }

    /// The pane at window x position `x`.
    #[cfg(target_os = "linux")]
    fn side_at(&self, x: f32) -> usize {
        let b = self.panes[1].list_bounds;
        if b.width > 0.0 { usize::from(x >= b.x) } else { usize::from(x > self.width / 2.0) }
    }

    /// The row of pane `side` at a window position, if there is one.
    #[cfg(target_os = "linux")]
    fn row_at(&self, side: usize, at: iced::Point) -> Option<(usize, usize)> {
        let p = &self.panes[side];
        if !p.list_bounds.contains(at) {
            return None;
        }
        let i = ((at.y - p.list_bounds.y + p.scroll_y) / ROW_H) as usize;
        (i < p.visible(self.show_hidden, &self.sizes).len()).then_some((side, i))
    }

    fn ask(&mut self, kind: OpKind) {
        let sources = self.selection();
        if sources.is_empty() {
            return;
        }
        // Archives are read-only: copying out of one extracts, nothing goes in.
        if self.panes[self.active].archive && kind != OpKind::Copy {
            self.read_only();
            return;
        }
        if self.panes[1 - self.active].archive && matches!(kind, OpKind::Copy | OpKind::Move) {
            self.dialog = Some(Dialog::Report { title: "Archive is read-only", problems: vec!["Files can't be copied or moved into an archive.".into()] });
            return;
        }
        let target = matches!(kind, OpKind::Copy | OpKind::Move).then(|| self.panes[1 - self.active].path.clone());
        if target.as_deref() == Some(self.panes[self.active].path.as_path()) {
            self.dialog = Some(Dialog::Report { title: "Not possible", problems: vec!["Source and target are the same folder.".into()] });
            return;
        }
        self.ask_with(kind, sources, target);
    }

    /// Asks for confirmation (if configured), then runs the operation.
    fn ask_with(&mut self, kind: OpKind, sources: Vec<PathBuf>, target: Option<PathBuf>) {
        // The trash can't take itself or its contents: delete permanently, always asking first.
        if kind == OpKind::Trash && sources.iter().any(|p| ops::in_trash(p)) {
            self.dialog = Some(Dialog::Confirm { kind: OpKind::Delete, sources, target });
            return;
        }
        let c = &self.config.confirm;
        let confirm = match kind {
            OpKind::Copy => c.copy,
            OpKind::Move => c.move_,
            OpKind::Trash => c.trash,
            OpKind::Delete => c.delete,
        };
        self.dialog = Some(Dialog::Confirm { kind, sources, target });
        if !confirm {
            self.confirm_dialog();
        }
    }

    fn confirm_dialog(&mut self) {
        match self.dialog.take() {
            Some(Dialog::Confirm { kind, sources, target }) => self.enqueue(kind, sources, target),
            Some(Dialog::EmptyTrash { .. }) => {
                self.trash_busy = Some("Emptying trash");
                self.later.push(Task::perform(
                    smol::unblock(ops::empty_trash),
                    Message::TrashEmptied,
                ));
            }
            Some(Dialog::Bookmark { path, name, edit }) => self.save_bookmark(path, name, edit),
            Some(Dialog::Mark { add, pattern }) => {
                self.mark_pattern(add, &pattern);
                self.last_pattern = pattern;
            }
            // Enter on a conflict: the safe answer.
            Some(Dialog::Conflict { all, .. }) => {
                if let Some((_, job, _)) = &self.jobs.running {
                    job.answer(Resolve::Skip, all);
                }
            }
            Some(Dialog::NewDir { name, .. }) => match ops::make_dir(&self.panes[self.active].path, name.trim()) {
                Ok(created) => {
                    self.reload(self.active);
                    let p = &self.panes[self.active];
                    if let Some(i) = p.visible(self.show_hidden, &self.sizes).iter().position(|e| e.path == created) {
                        self.panes[self.active].cursor = i;
                    }
                }
                Err(e) => self.dialog = Some(Dialog::NewDir { name, error: Some(e.to_string()) }),
            },
            Some(Dialog::Attrs { paths, mode, owner, was, field, recursive, .. }) => {
                let mut change = attrs::Change { recursive, ..Default::default() };
                let error = match attrs::parse_owner(&owner) {
                    _ if mode != was.0 && attrs::parse_mode(&mode).is_none() => Some("Permissions: octal digits like 755 or 0644".to_string()),
                    Ok((uid, gid)) => {
                        change.mode = attrs::parse_mode(&mode).filter(|_| mode != was.0 || recursive);
                        if owner != was.1 || recursive {
                            (change.uid, change.gid) = (uid, gid);
                        }
                        None
                    }
                    Err(e) => Some(e),
                };
                if error.is_some() {
                    self.dialog = Some(Dialog::Attrs { paths, mode, owner, was, field, recursive, error });
                    return;
                }
                let problems = attrs::apply(&paths, &change);
                self.reload(self.active);
                if !problems.is_empty() {
                    self.dialog = Some(Dialog::Report { title: "Not all changed", problems });
                }
            }
            Some(d @ Dialog::Settings { .. }) => self.dialog = Some(d),
            Some(d @ (Dialog::View { .. } | Dialog::Edit { .. })) => self.dialog = Some(d),
            Some(d @ (Dialog::Palette { .. } | Dialog::Drives { .. } | Dialog::Search { .. })) => self.dialog = Some(d),
            Some(Dialog::Report { .. } | Dialog::Help { .. } | Dialog::Jobs) | None => {}
        }
    }

    fn cancel_dialog(&mut self) {
        match &self.dialog {
            // Esc on a conflict stops the whole job.
            Some(Dialog::Conflict { .. }) => {
                let _ = self.update(Message::CancelJobs(false));
            }
            _ => self.dialog = None,
        }
    }

    fn on_dialog_key(&mut self, key: Key, typed: Option<&str>) {
        if matches!(self.dialog, Some(Dialog::Settings { .. })) {
            self.on_settings_key(key, typed);
            return;
        }
        match key.as_ref() {
            Key::Named(Named::Enter) => self.confirm_dialog(),
            Key::Named(Named::Escape) => self.cancel_dialog(),
            Key::Named(Named::F1) if matches!(self.dialog, Some(Dialog::Help { .. })) => self.dialog = None,
            Key::Named(Named::Delete) if matches!(self.dialog, Some(Dialog::Jobs)) => {
                let _ = self.update(Message::CancelJobs(false));
            }
            Key::Character("j") if matches!(self.dialog, Some(Dialog::Jobs)) => self.dialog = None,
            Key::Character("p") | Key::Named(Named::Space) if matches!(self.dialog, Some(Dialog::Jobs)) => {
                let _ = self.update(Message::TogglePause);
            }
            Key::Character(c) if matches!(self.dialog, Some(Dialog::Conflict { .. })) => {
                let msg = match c {
                    "o" => Message::Resolve(Resolve::Overwrite),
                    "s" => Message::Resolve(Resolve::Skip),
                    "k" => Message::Resolve(Resolve::KeepBoth),
                    "a" => Message::ToggleAll,
                    _ => return,
                };
                let _ = self.update(msg);
            }
            Key::Named(Named::Tab) if matches!(self.dialog, Some(Dialog::Attrs { .. })) => {
                if let Some(Dialog::Attrs { field, .. }) = &mut self.dialog {
                    *field = (*field + 1) % 3;
                }
            }
            Key::Named(Named::Space) if matches!(self.dialog, Some(Dialog::Attrs { field: 2, .. })) => {
                if let Some(Dialog::Attrs { recursive, .. }) = &mut self.dialog {
                    *recursive = !*recursive;
                }
            }
            Key::Named(Named::Space) if matches!(self.dialog, Some(Dialog::Help { startup: true })) => {
                let _ = self.update(Message::ToggleStartHelp);
            }
            _ => {
                let (name, error) = match &mut self.dialog {
                    Some(Dialog::NewDir { name, error }) => (name, Some(error)),
                    Some(Dialog::Attrs { mode, field: 0, error, .. }) => (mode, Some(error)),
                    Some(Dialog::Attrs { owner, field: 1, error, .. }) => (owner, Some(error)),
                    Some(Dialog::Mark { pattern, .. } | Dialog::Bookmark { name: pattern, .. }) => (pattern, None),
                    _ => return,
                };
                match (key.as_ref(), typed) {
                    (Key::Named(Named::Backspace), _) => {
                        name.pop();
                    }
                    (_, Some(t)) if t.chars().all(|c| !c.is_control()) => name.push_str(t),
                    _ => return,
                }
                if let Some(e) = error {
                    *e = None;
                }
            }
        }
    }

    /// Starts editing the name in place with the name (without extension) selected.
    fn start_rename(&mut self) {
        let Some(e) = self.current() else { return };
        let stem = match e.name.rfind('.') {
            Some(dot) if dot > 0 && !e.is_dir() => e.name[..dot].chars().count(),
            _ => e.name.chars().count(),
        };
        self.rename = Some(Rename { side: self.active, from: e.path.clone(), name: e.name.clone(), caret: stem, anchor: Some(0), error: None });
    }

    /// The one-line text field that currently has the keyboard, if any.
    fn field_mut(&mut self) -> Option<&mut String> {
        if let Some(r) = &mut self.rename {
            return Some(&mut r.name);
        }
        match &mut self.dialog {
            None => Some(&mut self.panes[self.active].query),
            Some(Dialog::NewDir { name, .. } | Dialog::Mark { pattern: name, .. } | Dialog::Palette { query: name, .. } | Dialog::Bookmark { name, .. }) => Some(name),
            Some(Dialog::Settings { edit: Some(text), .. }) => Some(text),
            Some(Dialog::Attrs { mode, field: 0, .. }) => Some(mode),
            Some(Dialog::Attrs { owner, field: 1, .. }) => Some(owner),
            _ => None,
        }
    }

    fn on_rename_key(&mut self, key: Key, m: keyboard::Modifiers, typed: Option<&str>) {
        let Some(r) = &mut self.rename else { return };
        let len = r.name.chars().count();
        let sel = m.shift();
        match key.as_ref() {
            Key::Named(Named::Escape) => self.rename = None,
            Key::Named(Named::Enter) => self.finish_rename(),
            Key::Character("a") if m.command() => {
                r.anchor = Some(0);
                r.caret = len;
            }
            // Left/Right without Shift on a selection: go to its edge.
            Key::Named(Named::ArrowLeft) if !sel && r.selection().is_some() => {
                let lo = r.selection().map_or(0, |s| s.0);
                r.move_to(lo, false);
            }
            Key::Named(Named::ArrowRight) if !sel && r.selection().is_some() => {
                let hi = r.selection().map_or(0, |s| s.1);
                r.move_to(hi, false);
            }
            Key::Named(Named::ArrowLeft) => r.move_to(r.caret.saturating_sub(1), sel),
            Key::Named(Named::ArrowRight) => r.move_to((r.caret + 1).min(len), sel),
            Key::Named(Named::Home) => r.move_to(0, sel),
            Key::Named(Named::End) => r.move_to(len, sel),
            Key::Named(Named::Backspace | Named::Delete) if r.delete_selection() => r.error = None,
            Key::Named(Named::Backspace) if r.caret > 0 => {
                r.caret -= 1;
                let b = r.byte(r.caret);
                r.name.remove(b);
                r.anchor = None;
                r.error = None;
            }
            Key::Named(Named::Delete) if r.caret < len => {
                let b = r.byte(r.caret);
                r.name.remove(b);
                r.anchor = None;
                r.error = None;
            }
            _ => {
                if let Some(t) = typed.filter(|t| !m.command() && t.chars().all(|c| !c.is_control())) {
                    r.insert(t);
                }
            }
        }
    }

    fn finish_rename(&mut self) {
        let Some(r) = &mut self.rename else { return };
        match ops::rename(&r.from, r.name.trim()) {
            Ok(to) => {
                if to != r.from {
                    let label = format!("Rename {} to {}", r.from.file_name().unwrap_or_default().to_string_lossy(), r.name.trim());
                    self.jobs.last = Some((label, vec![ops::Step::Moved { from: r.from.clone(), to: to.clone() }]));
                }
                let side = r.side;
                self.rename = None;
                self.reload(side);
                let p = &self.panes[side];
                if let Some(i) = p.visible(self.show_hidden, &self.sizes).iter().position(|e| e.path == to) {
                    self.panes[side].cursor = i;
                }
            }
            Err(e) => r.error = Some(e.to_string()),
        }
    }

    /// Opens the built-in editor for the file under the cursor.
    fn edit_current(&mut self) -> Task<Message> {
        if self.read_only() {
            return Task::none();
        }
        let Some(e) = self.current() else { return Task::none() };
        if e.is_dir() {
            return Task::none();
        }
        let path = e.path.clone();
        self.dialog = Some(Dialog::Edit { path: path.clone(), content: None, dirty: false, closing: false, error: None });
        Task::perform(
            async move {
                let p = path.clone();
                (path, smol::unblock(move || load_edit(&p)).await)
            },
            |(p, r)| Message::EditLoaded(p, r),
        )
    }

    /// Shift+F4: the configured external editor.
    fn edit_external(&mut self) {
        if self.read_only() {
            return;
        }
        let Some(e) = self.current() else { return };
        if e.is_dir() {
            return;
        }
        if let Err(err) = guppy_core::platform::edit(&e.path, &self.config.edit.command) {
            self.dialog = Some(Dialog::Report { title: "Editor not started", problems: vec![err.to_string()] });
        }
    }

    /// Opens the viewer for the entry under the cursor and loads it in the background.
    fn open_view(&mut self) -> Task<Message> {
        let Some(path) = self.current().map(|e| e.path.clone()) else { return Task::none() };
        self.dialog = Some(Dialog::View { path: path.clone(), body: ViewBody::Loading });
        Task::batch([
            operation::scroll_to(view_id(), AbsoluteOffset { x: None, y: Some(0.0) }),
            Task::perform(
                async move {
                    let p = path.clone();
                    (path, smol::unblock(move || preview::load(&p, false)).await)
                },
                |(p, b)| Message::Viewed(p, b),
            ),
        ])
    }

    fn on_view_key(&mut self, key: Key) -> Task<Message> {
        let line = 18.0;
        let page = self.panes[self.active].view_h.max(200.0);
        let by = |dy: f32| operation::scroll_by(view_id(), AbsoluteOffset { x: 0.0, y: dy });
        match key.as_ref() {
            Key::Named(Named::Escape | Named::F3 | Named::Enter) => self.dialog = None,
            Key::Named(Named::ArrowDown) => return by(3.0 * line),
            Key::Named(Named::ArrowUp) => return by(-3.0 * line),
            Key::Named(Named::PageDown | Named::Space) => return by(page),
            Key::Named(Named::PageUp) => return by(-page),
            Key::Named(Named::Home) => return operation::scroll_to(view_id(), AbsoluteOffset { x: None, y: Some(0.0) }),
            Key::Named(Named::End) => return operation::scroll_to(view_id(), AbsoluteOffset { x: None, y: Some(f32::MAX) }),
            // Previous / next entry of the pane, like flipping through a folder.
            Key::Named(dir @ (Named::ArrowLeft | Named::ArrowRight)) => {
                let len = self.panes[self.active].visible(self.show_hidden, &self.sizes).len();
                let p = &mut self.panes[self.active];
                p.cursor = if dir == Named::ArrowRight { (p.cursor + 1).min(len.saturating_sub(1)) } else { p.cursor.saturating_sub(1) };
                let scroll = self.scroll_into_view();
                return Task::batch([self.open_view(), scroll]);
            }
            Key::Named(Named::F4) => return self.edit_current(),
            _ => {}
        }
        Task::none()
    }

    /// Queues an operation; it starts right away if nothing else runs.
    fn enqueue(&mut self, kind: OpKind, sources: Vec<PathBuf>, target: Option<PathBuf>) {
        let what = if sources.len() == 1 {
            sources[0].file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
        } else {
            format!("{} items", sources.len())
        };
        let label = match &target {
            Some(t) => format!("{} {what} to {}", capitalize(op_verb(kind)), self.jumper.display(t)),
            None => format!("{} {what}", capitalize(op_verb(kind))),
        };
        self.jobs.queue.push_back((kind, sources, target, label));
        self.start_next_job();
    }

    fn start_next_job(&mut self) {
        if self.jobs.running.is_some() {
            return;
        }
        if let Some((kind, sources, target, label)) = self.jobs.queue.pop_front() {
            self.jobs.started += 1;
            self.jobs.undoing = false;
            self.jobs.running = Some((kind, ops::start(kind, sources, target), label));
        }
    }

    /// On every tick: shows conflicts, collects finished jobs, starts the next one.
    fn poll_jobs(&mut self) {
        let was_busy = self.jobs.busy();
        if let Some((_, job, _)) = &self.jobs.running {
            let progress = job.progress();
            match job.conflict() {
                Some(c) if self.dialog.is_none() || matches!(self.dialog, Some(Dialog::Jobs)) => {
                    self.dialog = Some(Dialog::Conflict { conflict: c, all: false });
                }
                None if matches!(self.dialog, Some(Dialog::Conflict { .. })) => self.dialog = None,
                _ => {}
            }
            if progress.finished {
                let steps = job.steps();
                if let Some((_, _, label)) = self.jobs.running.take()
                    && !self.jobs.undoing
                    && !steps.is_empty()
                {
                    self.jobs.last = Some((label, steps));
                }
                self.jobs.problems.extend(progress.problems);
                for side in 0..2 {
                    let path = self.panes[side].path.clone();
                    self.sizes.invalidate(&path);
                    self.reload(side);
                }
                self.start_next_job();
            }
        }
        if !self.jobs.busy() {
            self.jobs.started = 0;
            self.close_armed = false;
            // Closes once the last job is done; opened while idle it stays.
            if was_busy && matches!(self.dialog, Some(Dialog::Jobs)) {
                self.dialog = None;
            }
            if !self.jobs.problems.is_empty() && self.dialog.is_none() {
                self.dialog = Some(Dialog::Report { title: "Done, with notes", problems: std::mem::take(&mut self.jobs.problems) });
            }
        }
    }

    /// Reverses the last move, rename or trashing (Ctrl+Z).
    fn undo(&mut self) {
        if self.jobs.busy() {
            self.dialog = Some(Dialog::Report { title: "Not yet", problems: vec!["Wait until the running jobs are done.".into()] });
            return;
        }
        let Some((label, steps)) = self.jobs.last.take() else {
            self.dialog = Some(Dialog::Report { title: "Nothing to undo", problems: vec!["Moves, renames and trashing can be undone; copies and permanent deletes can't.".into()] });
            return;
        };
        self.jobs.started = 1;
        self.jobs.undoing = true;
        self.jobs.running = Some((OpKind::Move, ops::undo(steps), format!("Undo: {label}")));
    }

    fn cycle_sort(&mut self) {
        let p = &mut self.panes[self.active];
        p.sort = p.sort.next();
    }

    fn recompute_sizes(&mut self) {
        let path = self.panes[self.active].path.clone();
        self.sizes.invalidate(&path);
        self.request_sizes(self.active);
        self.reload(self.active);
    }

    /// Ctrl+F: search the active folder; the last search stays until a new one.
    fn open_search(&mut self) {
        let root = self.panes[self.active].path.clone();
        if self.panes[self.active].archive {
            return;
        }
        // Hits of the last search stay while the folder is the same.
        if self.search_root_cache.as_ref() != Some(&root) {
            self.search = None;
            self.search_root_cache = None;
            self.search_hits.clear();
            self.search_for.clear();
            self.search_ctx.clear();
        }
        let query = self.search_for.clone();
        self.dialog = Some(Dialog::Search { root, query, cur: 0 });
        self.refresh_search_ctx();
    }

    fn poll_search(&mut self) {
        let Some(s) = &self.search else { return };
        let before = self.search_hits.len();
        self.search_hits.extend(s.rx.try_iter());
        if before == 0 && !self.search_hits.is_empty() {
            self.refresh_search_ctx();
        }
    }

    fn refresh_search_ctx(&mut self) {
        let cur = match &self.dialog {
            Some(Dialog::Search { cur, .. }) => *cur,
            _ => return,
        };
        self.search_ctx = self.search_hits.get(cur).map(|h| guppy_core::search::context(&h.path, h.line, 3)).unwrap_or_default();
    }

    fn on_search_key(&mut self, key: Key, typed: Option<&str>) {
        let Some(Dialog::Search { root, query, cur }) = &mut self.dialog else { return };
        let len = self.search_hits.len();
        let mut moved = true;
        match key.as_ref() {
            Key::Named(Named::Escape) => {
                // A running search stops; its hits stay for the next Ctrl+F.
                if let Some(s) = self.search.take() {
                    drop(s);
                }
                self.dialog = None;
                return;
            }
            Key::Named(Named::ArrowDown) => *cur = (*cur + 1).min(len.saturating_sub(1)),
            Key::Named(Named::ArrowUp) => *cur = cur.saturating_sub(1),
            Key::Named(Named::PageDown) => *cur = (*cur + 10).min(len.saturating_sub(1)),
            Key::Named(Named::PageUp) => *cur = cur.saturating_sub(10),
            Key::Named(Named::Enter) if *query != self.search_for && !query.trim().is_empty() => {
                self.search_for = query.clone();
                self.search_root_cache = Some(root.clone());
                self.search_hits.clear();
                self.search_ctx.clear();
                *cur = 0;
                self.search = Some(guppy_core::search::start(root.clone(), query.trim(), self.show_hidden));
                return;
            }
            // Enter on a hit: go to the file in the active pane.
            Key::Named(Named::Enter) => {
                let Some(hit) = self.search_hits.get(*cur).cloned() else { return };
                self.dialog = None;
                if let Some(dir) = hit.path.parent() {
                    self.load(self.active, dir.to_path_buf(), After::Parent(hit.path));
                }
                return;
            }
            Key::Named(Named::Backspace) => {
                query.pop();
                moved = false;
            }
            _ => {
                moved = false;
                if let Some(t) = typed.filter(|t| t.chars().all(|c| !c.is_control())) {
                    query.push_str(t);
                }
            }
        }
        if moved {
            self.refresh_search_ctx();
        }
    }

    fn on_palette_key(&mut self, key: Key, typed: Option<&str>) -> Task<Message> {
        let Some(Dialog::Palette { query, cur }) = &mut self.dialog else { return Task::none() };
        let len = palette_hits(query, &self.config.actions).len();
        match key.as_ref() {
            Key::Named(Named::Escape) => self.dialog = None,
            Key::Named(Named::ArrowDown) => *cur = (*cur + 1).min(len.saturating_sub(1)),
            Key::Named(Named::ArrowUp) => *cur = cur.saturating_sub(1),
            Key::Named(Named::Enter) => {
                let hit = palette_hits(query, &self.config.actions).get(*cur).copied();
                self.dialog = None;
                if let Some(i) = hit {
                    return self.run_palette(i);
                }
            }
            Key::Named(Named::Backspace) => {
                query.pop();
                *cur = 0;
            }
            _ => {
                if let Some(t) = typed.filter(|t| t.chars().all(|c| !c.is_control())) {
                    query.push_str(t);
                    *cur = 0;
                }
            }
        }
        Task::none()
    }

    fn run_palette(&mut self, i: usize) -> Task<Message> {
        match COMMANDS.get(i) {
            Some(&(_, _, cmd)) => self.run_cmd(cmd),
            None => {
                self.run_action(i - COMMANDS.len());
                Task::none()
            }
        }
    }

    /// The user action bound to this key press, if any.
    fn action_for(&self, key: &Key, m: keyboard::Modifiers) -> Option<usize> {
        let name = match key.as_ref() {
            Key::Character(c) => c.to_lowercase(),
            Key::Named(n) => format!("{n:?}").to_lowercase(),
            _ => return None,
        };
        self.config.actions.iter().position(|a| {
            actions::KeySpec::parse(&a.key)
                .is_some_and(|k| k.key == name && k.ctrl == m.command() && k.alt == m.alt() && k.shift == m.shift())
        })
    }

    /// Runs the user action `i` on the active pane.
    fn run_action(&mut self, i: usize) {
        let Some(a) = self.config.actions.get(i) else { return };
        let p = &self.panes[self.active];
        if p.archive {
            self.dialog = Some(Dialog::Report { title: "Not possible", problems: vec!["Actions only run on local folders.".into()] });
            return;
        }
        let visible = p.visible(self.show_hidden, &self.sizes);
        let file = visible.get(p.cursor).map(|e| e.path.as_path());
        let marked: Vec<PathBuf> = visible.iter().filter(|e| p.marked.contains(&e.path)).map(|e| e.path.clone()).collect();
        let line = actions::expand(&a.command, file, &marked, &p.path, &self.panes[1 - self.active].path);
        if let Err(e) = actions::run(&line, &p.path, a.terminal) {
            self.dialog = Some(Dialog::Report { title: "Action not started", problems: vec![format!("{}: {e}", a.name)] });
        }
    }

    /// Runs a palette command as if its key had been pressed.
    fn run_cmd(&mut self, cmd: Cmd) -> Task<Message> {
        match cmd {
            Cmd::Copy => self.ask(OpKind::Copy),
            Cmd::Move => self.ask(OpKind::Move),
            Cmd::Trash => self.ask(OpKind::Trash),
            Cmd::Delete => self.ask(OpKind::Delete),
            Cmd::EmptyTrash => {
                self.trash_busy = Some("Reading trash");
                return Task::perform(smol::unblock(ops::trash_contents), Message::TrashListed);
            }
            Cmd::NewDir => self.new_dir(),
            Cmd::Rename => {
                if !self.read_only() {
                    self.start_rename();
                }
            }
            Cmd::Undo => self.undo(),
            Cmd::Attrs => self.open_attrs(),
            Cmd::Link => self.make_links(),
            Cmd::View => return self.open_view(),
            Cmd::Edit => return self.edit_current(),
            Cmd::EditExternal => self.edit_external(),
            Cmd::Quick => return self.update(Message::ToggleQuick),
            Cmd::Map => return self.update(Message::ToggleMap),
            Cmd::Search => self.open_search(),
            Cmd::Mark | Cmd::Unmark => self.dialog = Some(Dialog::Mark { add: cmd == Cmd::Mark, pattern: self.last_pattern.clone() }),
            Cmd::Invert => self.invert_marks(),
            Cmd::Hidden => return self.update(Message::ToggleHidden),
            Cmd::Sort => self.cycle_sort(),
            Cmd::Sizes => self.recompute_sizes(),
            Cmd::Jump => self.set_query("~".into()),
            Cmd::NewTab => self.new_tab(),
            Cmd::CloseTab => self.close_tab(),
            Cmd::NextTab => self.step_tab(1),
            Cmd::Back => self.history(true),
            Cmd::Forward => self.history(false),
            Cmd::Parent => self.go_parent(),
            Cmd::Home => self.navigate(dirs_home()),
            Cmd::Switch => self.active = 1 - self.active,
            Cmd::DrivesLeft => self.open_drives(0),
            Cmd::DrivesRight => self.open_drives(1),
            Cmd::Bookmark => self.bookmark_here(),
            Cmd::Favorite => {
                // Its own button again if it has one (clears it), else the first free one.
                let (side, cwd) = (self.active, self.panes[self.active].path.clone());
                let favs = &self.config.favorites;
                let mine = favs.iter().position(|f| !f.path.is_empty() && Config::resolve(&f.path, &cwd) == cwd);
                let free = (0..4).find(|&i| favs.get(i).is_none_or(|f| f.path.is_empty()));
                if let Some(slot) = mine.or(free) {
                    self.set_favorite(side, slot);
                }
            }
            Cmd::Jobs => self.dialog = Some(Dialog::Jobs),
            Cmd::PauseJob => {
                let _ = self.update(Message::TogglePause);
            }
            Cmd::Look => {
                let mut new = self.config.clone();
                new.view.look = new.view.look.step(1);
                self.set_config(new);
            }
            Cmd::Settings => self.open_settings(),
            Cmd::Help => self.dialog = Some(Dialog::Help { startup: false }),
        }
        self.scroll_into_view()
    }

    /// Opens the drive picker for `side`, with the drive holding its folder selected.
    fn open_drives(&mut self, side: usize) {
        let list = guppy_core::drives::list();
        let here = &self.panes[side].path;
        let cur = list
            .iter()
            .enumerate()
            .filter(|(_, d)| here.starts_with(&d.path))
            .max_by_key(|(_, d)| d.path.as_os_str().len())
            .map_or(0, |(i, _)| i);
        // Bookmarks come first in the list.
        let cur = self.config.bookmarks.len() + cur;
        self.dialog = Some(Dialog::Drives { side, list, cur });
    }

    fn on_drives_key(&mut self, key: Key, m: keyboard::Modifiers, typed: Option<&str>) -> Task<Message> {
        let marks = self.config.bookmarks.len();
        let Some(Dialog::Drives { side, list, cur }) = &mut self.dialog else { return Task::none() };
        let total = list.len() + marks;
        // Selected bookmark, if the cursor is on one.
        let mark = Some(*cur).filter(|&b| b < marks);
        match key.as_ref() {
            Key::Named(Named::Escape) => self.dialog = None,
            // Alt+↑/↓ move a bookmark within the list.
            Key::Named(dir @ (Named::ArrowUp | Named::ArrowDown)) if m.alt() => {
                if let Some(b) = mark {
                    let to = if dir == Named::ArrowUp { b.checked_sub(1) } else { Some(b + 1).filter(|&t| t < marks) };
                    if let Some(to) = to {
                        *cur = to;
                        let mut new = self.config.clone();
                        new.bookmarks.swap(b, to);
                        self.set_config(new);
                    }
                }
            }
            Key::Named(Named::ArrowDown) => *cur = (*cur + 1).min(total.saturating_sub(1)),
            Key::Named(Named::ArrowUp) => *cur = cur.saturating_sub(1),
            Key::Named(Named::Home) => *cur = 0,
            Key::Named(Named::End) => *cur = total.saturating_sub(1),
            Key::Named(Named::Enter) => self.pick_drive(m.shift()),
            Key::Named(Named::Delete) => {
                if let Some(b) = mark {
                    *cur = (*cur).min(total.saturating_sub(2));
                    let mut new = self.config.clone();
                    new.bookmarks.remove(b);
                    self.set_config(new);
                }
            }
            Key::Named(Named::F2) => {
                if let Some(b) = mark {
                    let side = *side;
                    let bm = &self.config.bookmarks[b];
                    let path = Config::resolve(&bm.path, &self.panes[side].path);
                    self.dialog = Some(Dialog::Bookmark { path, name: bm.name.clone(), edit: Some(b) });
                }
            }
            // 1–9 pick directly.
            _ => {
                if let Some(n) = typed.and_then(|t| t.parse::<usize>().ok()).filter(|n| (1..=total).contains(n)) {
                    *cur = n - 1;
                    self.pick_drive(false);
                }
            }
        }
        self.scroll_into_view()
    }

    /// Opens the selected drive or bookmark in its pane, or with `other` in the other one.
    fn pick_drive(&mut self, other: bool) {
        let Some(Dialog::Drives { side, list, cur }) = self.dialog.take() else { return };
        let marks = self.config.bookmarks.len();
        let target = match self.config.bookmarks.get(cur) {
            Some(b) => Some(Config::resolve(&b.path, &self.panes[side].path)),
            None => list.get(cur - marks).map(|d| d.path.clone()),
        };
        if let Some(path) = target {
            self.active = if other { 1 - side } else { side };
            self.navigate(path);
        }
    }

    /// Puts the pane's folder on favorite `slot`; if it is already there, frees the slot.
    fn set_favorite(&mut self, side: usize, slot: usize) {
        let here = self.panes[side].path.clone();
        let mut new = self.config.clone();
        if new.favorites.len() <= slot {
            new.favorites.resize(slot + 1, Bookmark { name: String::new(), path: String::new() });
        }
        let f = &mut new.favorites[slot];
        if !f.path.is_empty() && Config::resolve(&f.path, &here) == here {
            *f = Bookmark { name: String::new(), path: String::new() };
        } else {
            let name = here.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| self.jumper.display(&here));
            *f = Bookmark { name, path: self.jumper.display(&here) };
        }
        // Free slots at the end aren't worth keeping in the file.
        while new.favorites.last().is_some_and(|f| f.path.is_empty()) {
            new.favorites.pop();
        }
        self.set_config(new);
    }

    /// Ctrl+D: names a new bookmark for the active folder; if it has one, shows it in the list.
    fn bookmark_here(&mut self) {
        let here = self.panes[self.active].path.clone();
        let cwd = here.clone();
        if let Some(b) = self.config.bookmarks.iter().position(|b| Config::resolve(&b.path, &cwd) == here) {
            self.open_drives(self.active);
            if let Some(Dialog::Drives { cur, .. }) = &mut self.dialog {
                *cur = b;
            }
            return;
        }
        let name = here.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into());
        self.dialog = Some(Dialog::Bookmark { path: here, name, edit: None });
    }

    fn save_bookmark(&mut self, path: PathBuf, name: String, edit: Option<usize>) {
        let name = name.trim().to_string();
        if name.is_empty() {
            self.dialog = Some(Dialog::Bookmark { path, name, edit });
            return;
        }
        let mut new = self.config.clone();
        match edit {
            Some(b) if b < new.bookmarks.len() => new.bookmarks[b].name = name,
            _ => new.bookmarks.push(Bookmark { name, path: self.jumper.display(&path) }),
        }
        let index = edit.unwrap_or(new.bookmarks.len() - 1);
        self.set_config(new);
        // Back to the list, with this bookmark selected.
        if self.dialog.is_none() {
            self.open_drives(self.active);
            if let Some(Dialog::Drives { cur, .. }) = &mut self.dialog {
                *cur = index;
            }
        }
    }

    /// Inside an archive nothing can change; says so and returns true.
    fn read_only(&mut self) -> bool {
        if !self.panes[self.active].archive {
            return false;
        }
        self.dialog = Some(Dialog::Report {
            title: "Archive is read-only",
            problems: vec!["Copy (F5) extracts files to the other pane; Enter opens a file.".into()],
        });
        true
    }

    /// Alt+Enter: permissions and owner of the selection.
    fn open_attrs(&mut self) {
        if self.read_only() {
            return;
        }
        let paths = self.selection();
        let Some(first) = paths.first() else { return };
        match attrs::read(first) {
            Ok((mode, owner)) => {
                let mode = format!("{mode:o}");
                self.dialog = Some(Dialog::Attrs { paths, mode: mode.clone(), owner: owner.clone(), was: (mode, owner), field: 0, recursive: false, error: None });
            }
            Err(e) => self.dialog = Some(Dialog::Report { title: "Not possible", problems: vec![e.to_string()] }),
        }
    }

    /// Ctrl+Shift+F5: links to the selection in the other pane's folder.
    fn make_links(&mut self) {
        if self.read_only() {
            return;
        }
        if self.panes[1 - self.active].archive {
            self.dialog = Some(Dialog::Report { title: "Archive is read-only", problems: vec!["Links can't be created inside an archive.".into()] });
            return;
        }
        let dir = self.panes[1 - self.active].path.clone();
        let problems: Vec<String> = self
            .selection()
            .iter()
            .filter_map(|p| attrs::symlink(p, &dir).err().map(|e| format!("{}: {e}", self.jumper.display(p))))
            .collect();
        self.reload(1 - self.active);
        if !problems.is_empty() {
            self.dialog = Some(Dialog::Report { title: "Not all linked", problems });
        }
    }

    fn new_dir(&mut self) {
        if !self.read_only() {
            self.dialog = Some(Dialog::NewDir { name: String::new(), error: None });
        }
    }

    /// `+` / `-`: marks or unmarks visible files matching any of the patterns
    /// (separated by spaces, commas or semicolons); a pattern ending in `/` matches folders.
    fn mark_pattern(&mut self, add: bool, pattern: &str) {
        let pats: Vec<&str> = pattern.split([' ', ',', ';']).filter(|s| !s.is_empty()).collect();
        let p = &self.panes[self.active];
        let hits: Vec<PathBuf> = p
            .visible(self.show_hidden, &self.sizes)
            .iter()
            .filter(|e| {
                pats.iter().any(|pat| match pat.strip_suffix('/') {
                    Some(dir) => e.is_dir() && glob_match(dir, &e.name),
                    None => !e.is_dir() && glob_match(pat, &e.name),
                })
            })
            .map(|e| e.path.clone())
            .collect();
        let p = &mut self.panes[self.active];
        for h in hits {
            if add {
                p.marked.insert(h);
            } else {
                p.marked.remove(&h);
            }
        }
    }

    /// `*`: inverts the marks of the visible files.
    fn invert_marks(&mut self) {
        let p = &self.panes[self.active];
        let files: Vec<PathBuf> = p.visible(self.show_hidden, &self.sizes).iter().filter(|e| !e.is_dir()).map(|e| e.path.clone()).collect();
        let p = &mut self.panes[self.active];
        for f in files {
            if !p.marked.remove(&f) {
                p.marked.insert(f);
            }
        }
    }

    fn on_fs_changed(&mut self, paths: &[PathBuf]) {
        let theme_dir = guppy_core::Theme::omarchy_path().and_then(|p| p.parent().map(Path::to_path_buf));
        if theme_dir.is_some_and(|t| paths.iter().any(|p| p.starts_with(&t) || t.starts_with(p))) {
            self.theme = load_theme(self.config.view.look);
        }
        if let Some(cfg) = Config::path()
            && paths.contains(&cfg)
        {
            self.apply_config();
        }
        for side in 0..2 {
            let dir = self.panes[side].path.clone();
            // Events on the folder itself (e.g. timestamps) don't change sizes.
            let touched: Vec<&PathBuf> = paths.iter().filter(|p| p.parent() == Some(dir.as_path())).collect();
            if touched.is_empty() {
                continue;
            }
            for p in touched {
                self.sizes.invalidate(p);
            }
            self.reload(side);
        }
    }

    /// Re-reads the folder and keeps the cursor on its entry.
    fn pane_id(side: usize) -> &'static str {
        if side == 0 { "pane-left" } else { "pane-right" }
    }

    fn tab_count(&self, side: usize) -> usize {
        self.tabs[side].len() + 1
    }

    /// Ctrl+T: a new tab next to this one, in the same folder.
    fn new_tab(&mut self) {
        let side = self.active;
        let mut p = Pane::new(self.panes[side].path.clone(), Self::pane_id(side));
        p.sort = self.panes[side].sort;
        let old = std::mem::replace(&mut self.panes[side], p);
        self.tabs[side].insert(self.tab_at[side], old);
        self.tab_at[side] += 1;
        self.loads[side] = None;
        self.reload(side);
    }

    /// Shows tab `k` of `side`; the tab it leaves keeps its folder, cursor and history.
    fn switch_tab(&mut self, side: usize, k: usize) {
        if k == self.tab_at[side] || k >= self.tab_count(side) {
            return;
        }
        self.loads[side] = None;
        self.rename = None;
        let old = std::mem::replace(&mut self.panes[side], Pane::new(PathBuf::new(), Self::pane_id(side)));
        self.tabs[side].insert(self.tab_at[side], old);
        self.panes[side] = self.tabs[side].remove(k);
        self.tab_at[side] = k;
        self.git_stale[side] = true;
        self.reload(side);
    }

    fn step_tab(&mut self, dir: i32) {
        let side = self.active;
        let n = self.tab_count(side) as i32;
        let k = (self.tab_at[side] as i32 + dir).rem_euclid(n) as usize;
        self.switch_tab(side, k);
    }

    /// Ctrl+W: closes this tab; the last one stays.
    fn close_tab(&mut self) {
        let side = self.active;
        if self.tabs[side].is_empty() {
            return;
        }
        // The tab to the right takes its place, at the end the one to the left.
        let k = self.tab_at[side].min(self.tabs[side].len() - 1);
        self.loads[side] = None;
        self.panes[side].sizing.cancel();
        self.panes[side] = self.tabs[side].remove(k);
        self.tab_at[side] = k;
        self.git_stale[side] = true;
        self.reload(side);
    }

    fn tabs_file() -> Option<PathBuf> {
        dirs::state_dir().or_else(dirs::data_local_dir).map(|d| d.join("guppy/tabs"))
    }

    /// Writes the tabs as `side<TAB>shown<TAB>path` lines.
    fn save_tabs(&self) {
        let Some(file) = Self::tabs_file() else { return };
        let mut out = String::new();
        for side in 0..2 {
            let mut list: Vec<&Pane> = self.tabs[side].iter().collect();
            list.insert(self.tab_at[side], &self.panes[side]);
            for (k, p) in list.iter().enumerate() {
                out += &format!("{side}\t{}\t{}\n", u8::from(k == self.tab_at[side]), p.path.display());
            }
        }
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(file, out);
    }

    /// The shown tab of each side starts where the settings say; the other tabs
    /// come back from last time around it (they are read when first shown).
    fn restore_tabs(&mut self) {
        let Some(text) = Self::tabs_file().and_then(|f| std::fs::read_to_string(f).ok()) else { return };
        for side in 0..2 {
            let mut list: Vec<(bool, PathBuf)> = text
                .lines()
                .filter_map(|l| {
                    let mut it = l.splitn(3, '\t');
                    let (s, shown, path) = (it.next()?, it.next()?, it.next()?);
                    (s == side.to_string()).then(|| (shown == "1", PathBuf::from(path)))
                })
                .collect();
            let Some(at) = list.iter().position(|(shown, _)| *shown) else { continue };
            list.remove(at);
            let sort = self.panes[side].sort;
            self.tabs[side] = list
                .into_iter()
                .filter(|(_, p)| p.is_dir())
                .map(|(_, p)| {
                    let mut pane = Pane::new(p, Self::pane_id(side));
                    pane.sort = sort;
                    pane
                })
                .collect();
            self.tab_at[side] = at.min(self.tabs[side].len());
        }
    }

    fn reload(&mut self, side: usize) {
        let path = self.panes[side].path.clone();
        self.load(side, path, After::Reload);
    }

    /// Reads a folder in the background. If it arrives quickly (almost always locally),
    /// it is shown immediately; otherwise the UI stays usable until it arrives.
    fn load(&mut self, side: usize, path: PathBuf, after: After) {
        // A reload never replaces a pending folder change.
        if matches!(after, After::Reload) && self.loads[side].is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let p = path.clone();
        std::thread::spawn(move || {
            let _ = tx.send(guppy_core::archive::read_path(&p));
        });
        match rx.recv_timeout(LOAD_WAIT) {
            Ok(res) => {
                self.loads[side] = None;
                self.finish_load(side, path, res, after);
            }
            Err(_) => self.loads[side] = Some(Loading { path, rx, since: std::time::Instant::now(), after }),
        }
    }

    fn poll_loads(&mut self) {
        for side in 0..2 {
            let Some(l) = &self.loads[side] else { continue };
            if let Ok(res) = l.rx.try_recv() {
                let l = self.loads[side].take().unwrap();
                self.finish_load(side, l.path, res, l.after);
            }
        }
    }

    fn finish_load(&mut self, side: usize, path: PathBuf, res: std::io::Result<Vec<Entry>>, after: After) {
        let entries = match res {
            Ok(entries) => {
                self.git_stale[side] = true;
                entries
            }
            Err(e) => {
                // Unreadable (permissions, NAS gone): view stays as it was.
                if !matches!(after, After::Reload | After::Start) {
                    self.dialog = Some(Dialog::Report { title: "Folder not readable", problems: vec![format!("{}: {e}", self.jumper.display(&path))] });
                }
                return;
            }
        };
        if let After::Reload = after {
            let before: Vec<Entry> = self.panes[side].visible(self.show_hidden, &self.sizes).into_iter().cloned().collect();
            let keep = before.get(self.panes[side].cursor).map(|e| e.path.clone());
            if self.panes[side].path != path {
                return;
            }
            // Gone rows fold away, new ones glow (not when everything changed at once).
            let now = std::time::Instant::now();
            let new: HashSet<&PathBuf> = entries.iter().map(|e| &e.path).collect();
            let old: HashSet<&PathBuf> = before.iter().map(|e| &e.path).collect();
            let gone: Vec<(usize, Entry, std::time::Instant)> = before.iter().enumerate().filter(|(_, e)| !new.contains(&e.path)).map(|(i, e)| (i, e.clone(), now)).collect();
            let added: Vec<PathBuf> = entries.iter().filter(|e| !old.contains(&e.path)).map(|e| e.path.clone()).collect();
            if gone.len() <= 50 {
                self.ghosts[side].extend(gone);
            }
            if added.len() <= 50 && !before.is_empty() {
                self.glow.extend(added.into_iter().map(|p| (p, now)));
            }
            let p = &mut self.panes[side];
            p.entries = entries;
            let names: HashSet<&PathBuf> = p.entries.iter().map(|e| &e.path).collect();
            p.marked.retain(|m| names.contains(m));
            let visible = p.visible(self.show_hidden, &self.sizes);
            let pos = keep.and_then(|k| visible.iter().position(|e| e.path == k));
            let len = visible.len();
            p.cursor = pos.unwrap_or(p.cursor).min(len.saturating_sub(1));
            // Let running computations continue, request only what's missing.
            if !self.config.view.dir_sizes || self.panes[side].archive {
                return;
            }
            let p = &self.panes[side];
            let dirs = p.entries.iter().filter(|e| e.is_dir()).map(|e| e.path.clone()).collect();
            self.sizes.request(dirs, p.sizing.clone());
            return;
        }
        let p = &mut self.panes[side];
        let old = std::mem::take(&mut p.path);
        if old != path {
            self.git[side] = None;
        }
        p.enter(path.clone(), entries);
        let changed = old != path && !old.as_os_str().is_empty();
        match &after {
            After::Nav | After::Parent(_) if changed => {
                p.back.push(old);
                p.forward.clear();
            }
            After::Back => p.forward.push(old),
            After::Forward => p.back.push(old),
            _ => {}
        }
        if let After::Parent(came_from) = &after {
            let pos = self.panes[side].visible(self.show_hidden, &self.sizes).iter().position(|e| e.path == *came_from);
            self.panes[side].cursor = pos.unwrap_or(0);
        }
        if matches!(after, After::Nav | After::Parent(_)) && !self.panes[side].archive {
            self.jumper.visit(&path);
        }
        self.request_sizes(side);
    }

    fn set_query(&mut self, q: String) {
        let entering_jump = is_jump(&q) && !self.jumping();
        let p = &mut self.panes[self.active];
        p.query = q;
        p.cursor = 0;
        if entering_jump {
            self.jumper.index_nearby(&self.panes[self.active].path);
            self.jump_pending = true;
        }
        if self.jumping() {
            self.update_jump();
        }
    }

    fn update_jump(&mut self) {
        let q = &self.panes[self.active].query;
        let pattern = &q[1..];
        let mut hits = self.jumper.search(pattern, JUMP_LIMIT);
        // Bookmarks whose name fits come first.
        if !pattern.trim().is_empty() {
            let cwd = self.panes[self.active].path.clone();
            let mut marks: Vec<(i32, PathBuf)> = self
                .config
                .bookmarks
                .iter()
                .filter_map(|b| fuzzy(pattern, &b.name).map(|s| (s, Config::resolve(&b.path, &cwd))))
                .collect();
            marks.sort_by_key(|(s, _)| std::cmp::Reverse(*s));
            for (_, p) in marks.into_iter().rev() {
                hits.retain(|h| *h != p);
                hits.insert(0, p);
            }
            hits.truncate(JUMP_LIMIT);
        }
        // A typed path that exists goes to the top.
        let typed = match q.strip_prefix('~') {
            Some(rest) => dirs_home().join(rest.trim_start_matches('/')),
            None => PathBuf::from(q),
        };
        if q.len() > 1 && typed.is_dir() {
            hits.retain(|h| *h != typed);
            hits.insert(0, typed);
            hits.truncate(JUMP_LIMIT);
        }
        self.jump = hits;
        self.jump_cur = 0;
    }

    fn jump_selected(&mut self) {
        if let Some(target) = self.jump.get(self.jump_cur).cloned() {
            self.navigate(target);
        }
        self.jump.clear();
    }

    fn request_sizes(&mut self, side: usize) {
        let p = &mut self.panes[side];
        p.sizing.cancel();
        p.sizing = Cancel::default();
        if !self.config.view.dir_sizes || p.archive {
            return;
        }
        let mut dirs: Vec<PathBuf> = p.entries.iter().filter(|e| e.is_dir()).map(|e| e.path.clone()).collect();
        // The folder itself and the ones above it (up to home) for the path line;
        // what is below is mostly known already, so they add up quickly.
        let home = dirs_home();
        let top = if p.path.starts_with(&home) { home } else { p.path.clone() };
        dirs.extend(p.path.ancestors().take_while(|a| a.starts_with(&top)).map(Path::to_path_buf));
        self.sizes.request(dirs, p.sizing.clone());
    }

    fn navigate(&mut self, path: PathBuf) {
        if path != self.panes[self.active].path {
            self.load(self.active, path, After::Nav);
        }
    }

    fn history(&mut self, back: bool) {
        let p = &mut self.panes[self.active];
        let target = if back { p.back.pop() } else { p.forward.pop() };
        if let Some(t) = target {
            self.load(self.active, t, if back { After::Back } else { After::Forward });
        }
    }

    fn go_parent(&mut self) {
        let p = &self.panes[self.active];
        let Some(parent) = p.path.parent().filter(|q| !q.as_os_str().is_empty()).map(Path::to_path_buf) else { return };
        let came_from = p.path.clone();
        self.load(self.active, parent, After::Parent(came_from));
    }

    /// Enter: folders and archives are entered, files open in their application;
    /// a file inside an archive is extracted to a temporary folder first.
    fn open_current(&mut self) -> Task<Message> {
        let Some(e) = self.current() else { return Task::none() };
        let path = e.path.clone();
        if e.is_dir() || (!e.is_virtual() && guppy_core::archive::is_archive_name(&e.name)) {
            self.navigate(path);
        } else if e.in_archive {
            self.opening = Some(e.name.clone());
            return Task::perform(smol::unblock(move || open_from_archive(&path)), Message::Opened);
        } else {
            let _ = guppy_core::platform::open(&path);
        }
        Task::none()
    }

    fn toggle_mark(&mut self) {
        let Some(path) = self.current().map(|e| e.path.clone()) else { return };
        let p = &mut self.panes[self.active];
        if !p.marked.remove(&path) {
            p.marked.insert(path);
        }
        p.cursor += 1;
        let len = p.visible(self.show_hidden, &self.sizes).len();
        p.cursor = p.cursor.min(len.saturating_sub(1));
    }

    /// Keeps the cursor visible with two rows of margin; scrolls smoothly.
    fn scroll_into_view(&mut self) -> Task<Message> {
        if self.jumping() {
            return Task::none();
        }
        const MARGIN: f32 = 2.0 * ROW_H;
        let now = std::time::Instant::now();
        let p = &mut self.panes[self.active];
        // Compute against the running target, else key repeat makes it jump back.
        let base = p.scroll_target.unwrap_or(p.scroll_y);
        let top = p.cursor as f32 * ROW_H;
        let margin = MARGIN.min(((p.view_h - ROW_H) / 2.0).max(0.0));
        // Round targets to whole rows so rows don't jump a few pixels
        // (window height modulo row height) when scrolling starts.
        let y = if top - margin < base {
            ((top - margin) / ROW_H).floor().max(0.0) * ROW_H
        } else if top + ROW_H + margin > base + p.view_h {
            ((top + ROW_H + margin - p.view_h) / ROW_H).ceil() * ROW_H
        } else {
            self.last_move = Some(now);
            return Task::none();
        };
        // Key repeat: the list moves in step with the keys, the cursor stays put.
        // Animating here causes sawtooth jitter (cursor jumps, list catches up).
        let repeating = self.last_move.is_some_and(|t| now.duration_since(t) < Duration::from_millis(90));
        self.last_move = Some(now);
        if repeating {
            p.scroll_target = None;
            p.scroll_y = y;
            return operation::scroll_to(p.id.clone(), AbsoluteOffset { x: None, y: Some(y) });
        }
        p.scroll_target = Some(y);
        self.last_frame = None;
        Task::none()
    }

    /// One animation step: approaches the target exponentially, independent of frame rate.
    fn animating(&self) -> bool {
        let running = |t: Option<std::time::Instant>, d: Duration| t.is_some_and(|t| t.elapsed() < d);
        !self.glow.is_empty()
            || self.ghosts.iter().any(|g| !g.is_empty())
            || running(self.dialog_at, APPEAR)
            || running(self.active_at, APPEAR)
            || running(self.map_at, MAP_IN)
            || running(self.picture_at, APPEAR)
    }

    /// Eased progress (0 to 1) of a transition that started at `t`; 1 when there is none.
    fn eased(t: Option<std::time::Instant>, length: Duration) -> f32 {
        let k = t.map_or(1.0, |t| Self::progress(t, length));
        1.0 - (1.0 - k).powi(3)
    }

    /// How far along an animation that started at `t` is, 0 to 1.
    fn progress(t: std::time::Instant, length: Duration) -> f32 {
        (t.elapsed().as_secs_f32() / length.as_secs_f32()).min(1.0)
    }

    fn animate_scroll(&mut self, now: std::time::Instant) -> Task<Message> {
        const TAU: f32 = 0.03; // seconds; smaller = faster
        let dt = self.last_frame.map_or(1.0 / 60.0, |t| now.duration_since(t).as_secs_f32()).min(0.1);
        self.last_frame = Some(now);
        let k = 1.0 - (-dt / TAU).exp();
        let mut tasks = Vec::new();
        for p in &mut self.panes {
            let Some(target) = p.scroll_target else { continue };
            let mut y = p.scroll_y + (target - p.scroll_y) * k;
            // Never lag behind the cursor: its row stays fully visible.
            let top = p.cursor as f32 * ROW_H;
            y = y.clamp((top + ROW_H - p.view_h).min(top), top);
            if (target - y).abs() < 0.5 {
                y = target;
                p.scroll_target = None;
            }
            p.scroll_y = y;
            tasks.push(operation::scroll_to(p.id.clone(), AbsoluteOffset { x: None, y: Some(y) }));
        }
        Task::batch(tasks)
    }

    // ---------- View ----------

    fn view(&self) -> Element<'_, Message> {
        let t = &self.theme;
        let side_view = |side: usize| match side != self.active {
            true if self.quick => self.quick_pane(side),
            true if self.map => self.map_pane(side),
            _ => self.pane_view(side),
        };
        let body = row![side_view(0), side_view(1)].spacing(16);
        let desk = container(body)
            .padding([12, 13])
            .height(Length::Fill)
            .style(move |_| container::Style {
                background: Some(rgb(t.dark_background).into()),
                ..Default::default()
            });
        let main: Element<'_, Message> = column![self.menubar(), desk, self.statusbar()]
            .push(self.config.view.keybar.then(|| self.keybar()))
            .into();
        let main = match self.drag.as_ref().filter(|d| d.active) {
            Some(d) => {
                let grab = mouse_area(main).interaction(iced::mouse::Interaction::Grabbing);
                match d.at {
                    // The label sits beside the pointer; it takes no mouse input, so the rows below still see the pointer.
                    Some(at) => iced::widget::stack![grab, iced::widget::pin(self.drag_label(d)).position(iced::Point::new(at.x + 16.0, at.y + 12.0))].into(),
                    None => grab.into(),
                }
            }
            None => main,
        };
        match self.dialog_view() {
            Some(d) => iced::widget::stack![main, iced::widget::opaque(d)].into(),
            None => main.into(),
        }
    }

    /// What is being dragged and what letting go would do, next to the pointer.
    fn drag_label(&self, d: &Drag) -> Element<'_, Message> {
        let t = &self.theme;
        let name = |p: &PathBuf| {
            let n = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if p.is_dir() { format!("/{n}") } else { n }
        };
        let what = match d.paths.as_slice() {
            [one] => name(one),
            [first, rest @ ..] => format!("{} +{}", name(first), rest.len()),
            [] => String::new(),
        };
        let (verb, color) = match self.drop_plan(d) {
            Some((OpKind::Copy, _)) => ("COPY", rgb(t.marked)),
            Some(_) => ("MOVE", rgb(t.accent)),
            None => ("✕", rgb(t.red)),
        };
        container(
            row![
                text(verb).size(11).font(BOLD).color(color),
                text(what).size(13).color(rgb(t.bright)).wrapping(iced::widget::text::Wrapping::None),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
        )
        .padding([5, 10])
        .max_width(360)
        .clip(true)
        .style(move |_| container::Style {
            background: Some(rgb(t.lighter_background).into()),
            border: Border { color, width: 1.0, radius: (if self.neon() { 0 } else { 4 }).into() },
            shadow: iced::Shadow { color: Color { a: 0.45, ..Color::BLACK }, offset: iced::Vector::new(0.0, 3.0), blur_radius: 10.0 },
            ..Default::default()
        })
        .into()
    }

    fn dialog_view(&self) -> Option<Element<'_, Message>> {
        let t = &self.theme;
        let dialog = self.dialog.as_ref()?;
        let fg = rgb(t.foreground);
        let title = |s: String| text(s).size(15).font(BOLD).color(rgb(t.bright));
        let small = |s: String| text(s).size(12).color(fg);
        let btn = |label: &'static str, msg: Message, primary: bool| {
            button(text(label).size(13))
                .padding([8, 16])
                .on_press(msg)
                .style(move |_, status| button::Style {
                    background: Some(if primary { rgb(t.accent) } else if matches!(status, button::Status::Hovered) {
                        Color { a: 0.08, ..Color::WHITE }
                    } else {
                        Color::TRANSPARENT
                    }.into()),
                    text_color: if primary { rgb(t.background) } else { fg },
                    border: Border { color: if primary { rgb(t.accent) } else { Color { a: 0.2, ..Color::WHITE } }, width: 1.0, radius: 6.0.into() },
                    ..Default::default()
                })
        };
        let buttons = |ok: &'static str| {
            row![iced::widget::Space::new().width(Length::Fill), btn("Cancel", Message::DialogCancel, false), btn(ok, Message::DialogConfirm, true)]
                .spacing(8)
        };
        let progress_bar = |frac: f32| {
            iced::widget::progress_bar(0.0..=1.0, frac).girth(6).style(move |_| iced::widget::progress_bar::Style {
                background: Color { a: 0.25, ..Color::BLACK }.into(),
                bar: rgb(t.accent).into(),
                border: Border::default().rounded(3),
            })
        };
        let names = |paths: &[PathBuf]| {
            let mut lines: Vec<String> = paths.iter().take(6).map(|p| {
                let n = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if p.is_dir() { format!("· {n}/") } else { format!("· {n}") }
            }).collect();
            if paths.len() > 6 {
                lines.push(format!("  and {} more", paths.len() - 6));
            }
            small(lines.join("\n"))
        };

        let content: Element<'_, Message> = match dialog {
            Dialog::Confirm { kind, sources, target } => {
                let what = if sources.len() == 1 { "1 item".to_string() } else { format!("{} items", sources.len()) };
                let mut c = column![title(format!("{} {what}", capitalize(op_verb(*kind))))].spacing(14);
                if let Some(target) = target {
                    c = c.push(small(format!("to {}", self.jumper.display(target))));
                }
                c = c.push(names(sources));
                if *kind == OpKind::Delete {
                    c = c.push(text("This cannot be undone.").size(12).color(rgb(t.red)));
                }
                let ok = match kind {
                    OpKind::Copy => "Copy",
                    OpKind::Move => "Move",
                    OpKind::Trash => "Trash",
                    OpKind::Delete => "Delete",
                };
                c.push(buttons(ok)).into()
            }
            Dialog::EmptyTrash { items } => {
                let what = if items.len() == 1 { "1 item".to_string() } else { format!("{} items", items.len()) };
                column![
                    title(format!("Empty trash: {what}")),
                    names(items),
                    text("This cannot be undone.").size(12).color(rgb(t.red)),
                    buttons("Empty"),
                ]
                .spacing(14)
                .into()
            }
            Dialog::View { path, body } => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
                column![
                    row![title(name), iced::widget::Space::new().width(Length::Fill), small(file_info(path))].align_y(iced::Alignment::Center).spacing(12),
                    self.body_view(path, body, view_id()),
                    small("↑↓ PgUp PgDn scroll · ←→ previous / next · F4 edit · Esc closes".into()),
                ]
                .spacing(14)
                .into()
            }
            Dialog::Edit { path, content, dirty, closing, error } => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let body: Element<'_, Message> = match content {
                    None => small("loading …".into()).into(),
                    Some(c) => text_editor(c)
                        .id(editor_id())
                        .on_action(Message::EditAction)
                        .key_binding(|k| {
                            let ctrl_s = k.modifiers.command() && matches!(k.key.as_ref(), Key::Character("s"));
                            match k.key.as_ref() {
                                Key::Named(Named::Escape) => Some(text_editor::Binding::Custom(Message::EditClose)),
                                _ if ctrl_s => Some(text_editor::Binding::Custom(Message::EditSave)),
                                Key::Named(Named::Tab) if matches!(k.status, text_editor::Status::Focused { .. }) => {
                                    Some(text_editor::Binding::Sequence(vec![text_editor::Binding::Insert(' '); 4]))
                                }
                                _ => text_editor::Binding::from_key_press(k),
                            }
                        })
                        .font(FONT)
                        .size(13)
                        .height(Length::Fill)
                        .padding(10)
                        .style(move |_, _| text_editor::Style {
                            background: Color { a: 0.25, ..Color::BLACK }.into(),
                            border: Border::default().rounded(4),
                            placeholder: rgb(t.foreground),
                            value: rgb(t.bright),
                            selection: Color { a: 0.35, ..rgb(t.accent) },
                        })
                        .into(),
                };
                let (status, status_color) = match (error, closing, dirty) {
                    (Some(e), _, _) => (e.clone(), rgb(t.red)),
                    (None, true, _) => ("Unsaved changes · Esc again discards · Ctrl+S saves".to_string(), rgb(t.red)),
                    (None, false, true) => ("modified".to_string(), rgb(t.accent)),
                    (None, false, false) => (String::new(), fg),
                };
                let pos = content.as_ref().map(|c| {
                    let p = c.cursor().position;
                    format!("{}:{}", p.line + 1, p.column + 1)
                }).unwrap_or_default();
                column![
                    row![title(name), iced::widget::Space::new().width(Length::Fill), text(status).size(12).color(status_color), small(pos)]
                        .align_y(iced::Alignment::Center)
                        .spacing(12),
                    body,
                    small(keys_label("Ctrl+S saves · Esc closes")),
                ]
                .spacing(14)
                .into()
            }
            Dialog::NewDir { name, error } => {
                let field = container(text(format!("{name}▏")).size(13).color(rgb(t.bright)))
                    .padding([8, 10])
                    .width(Length::Fill)
                    .style(move |_| container::Style {
                        background: Some(Color { a: 0.25, ..Color::BLACK }.into()),
                        border: Border::default().rounded(4),
                        ..Default::default()
                    });
                let mut c = column![
                    title("New folder".into()),
                    small(format!("in {}", self.jumper.display(&self.panes[self.active].path))),
                    field,
                ]
                .spacing(14);
                if let Some(e) = error {
                    c = c.push(text(e.clone()).size(12).color(rgb(t.red)));
                }
                c.push(buttons("Create")).into()
            }
            Dialog::Jobs => {
                let mut c = column![title("Jobs".into())].spacing(14);
                match &self.jobs.running {
                    Some((kind, job, label)) => {
                        let p = job.progress();
                        let frac = if p.total == 0 { 0.0 } else { p.done as f32 / p.total as f32 };
                        let amount = if matches!(kind, OpKind::Copy | OpKind::Move) {
                            format!("{} of {}", human_size(p.done), human_size(p.total))
                        } else {
                            format!("{} of {}", p.done, p.total)
                        };
                        let label = if job.paused() { format!("{label} · paused") } else { label.clone() };
                        c = c.push(text(label).size(13).color(rgb(if job.paused() { t.accent } else { t.bright })));
                        c = c.push(progress_bar(frac));
                        c = c.push(row![small(p.current), iced::widget::Space::new().width(Length::Fill), small(amount)]);
                    }
                    None => c = c.push(small("Nothing running.".into())),
                }
                if !self.jobs.queue.is_empty() {
                    let chip = |label: &'static str, msg: Message| {
                        button(text(label).size(11)).padding([2, 8]).on_press(msg).style(move |_, status| button::Style {
                            background: matches!(status, button::Status::Hovered).then(|| Color { a: 0.08, ..Color::WHITE }.into()),
                            text_color: fg,
                            border: Border { color: Color { a: 0.2, ..Color::WHITE }, width: 1.0, radius: 4.0.into() },
                            ..Default::default()
                        })
                    };
                    let mut waiting = column![text("WAITING").size(11).font(BOLD).color(rgb(t.accent))].spacing(6);
                    for (n, (.., label)) in self.jobs.queue.iter().enumerate() {
                        let mut line = row![small(format!("· {label}")).width(Length::Fill)].spacing(6).align_y(iced::Alignment::Center);
                        if n > 0 {
                            line = line.push(chip("Next", Message::QueueFirst(n)));
                        }
                        waiting = waiting.push(line.push(chip("Remove", Message::QueueDrop(n))));
                    }
                    c = c.push(waiting);
                }
                if self.close_armed {
                    c = c.push(text("Jobs are still running. Close the window again to quit and cancel them.").size(12).color(rgb(t.red)));
                }
                c = c.push(text("P pauses · Del cancels · Esc closes").size(11).color(rgb(t.muted)));
                let mut buttons = row![iced::widget::Space::new().width(Length::Fill)].spacing(8).align_y(iced::Alignment::Center);
                if !self.jobs.queue.is_empty() {
                    buttons = buttons.push(btn("Cancel all", Message::CancelJobs(true), false));
                }
                if let Some((_, job, _)) = &self.jobs.running {
                    buttons = buttons.push(btn(if job.paused() { "Resume" } else { "Pause" }, Message::TogglePause, false));
                    buttons = buttons.push(btn("Cancel job", Message::CancelJobs(false), false));
                }
                c.push(buttons.push(btn("Close", Message::DialogCancel, true))).into()
            }
            Dialog::Conflict { conflict, all } => {
                let describe = |p: &Path| match std::fs::symlink_metadata(p) {
                    Ok(m) if m.is_dir() => "folder".to_string(),
                    Ok(m) => format!("{} · {}", human_size(m.len()), m.modified().ok().map(human_date).unwrap_or_default()),
                    Err(_) => String::new(),
                };
                let name = conflict.dst.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let dir = conflict.dst.parent().map(|d| self.jumper.display(d)).unwrap_or_default();
                let line = |label: &'static str, what: String| row![text(label).size(12).color(fg).width(80), text(what).size(12).color(rgb(t.bright))];
                let on = *all;
                let tick = container(text(if on { "✓" } else { "" }).size(11).color(rgb(t.background)))
                    .center_x(15)
                    .center_y(15)
                    .style(move |_| container::Style {
                        background: on.then(|| rgb(t.accent).into()),
                        border: Border { color: rgb(t.accent), width: 1.0, radius: 3.0.into() },
                        ..Default::default()
                    });
                let toggle = mouse_area(row![tick, small("Same answer for the rest · A".into())].spacing(8).align_y(iced::Alignment::Center))
                    .on_press(Message::ToggleAll)
                    .interaction(iced::mouse::Interaction::Pointer);
                column![
                    title(format!("“{name}” already exists")),
                    small(format!("in {dir}")),
                    column![line("New", describe(&conflict.src)), line("Existing", describe(&conflict.dst))].spacing(4),
                    toggle,
                    row![
                        small("Esc cancels the job".into()),
                        iced::widget::Space::new().width(Length::Fill),
                        btn("Overwrite · O", Message::Resolve(Resolve::Overwrite), false),
                        btn("Keep both · K", Message::Resolve(Resolve::KeepBoth), false),
                        btn("Skip · S", Message::Resolve(Resolve::Skip), true),
                    ]
                    .spacing(8)
                    .align_y(iced::Alignment::Center),
                ]
                .spacing(14)
                .into()
            }
            Dialog::Search { root, query, cur } => {
                let field = container(text(if query.is_empty() { "type text, Enter searches …".to_string() } else { format!("{query}▏") }).size(13).color(if query.is_empty() { fg } else { rgb(t.bright) }))
                    .padding([8, 10])
                    .width(Length::Fill)
                    .style(move |_| container::Style {
                        background: Some(Color { a: 0.25, ..Color::BLACK }.into()),
                        border: Border::default().rounded(4),
                        ..Default::default()
                    });
                let running = self.search.as_ref().is_some_and(|s| !s.finished());
                let scanned = self.search.as_ref().map_or(0, |s| s.scanned());
                let n = self.search_hits.len();
                let status = match () {
                    _ if self.search_for.is_empty() => format!("in {}", self.jumper.display(root)),
                    _ if running => format!("searching … {scanned} files · {n} hits"),
                    _ if n >= guppy_core::search::MAX_HITS => format!("first {n} hits · {scanned} files"),
                    _ => format!("{n} hits in {scanned} files"),
                };
                const SHOWN: usize = 10;
                let first = cur.saturating_sub(SHOWN - 1);
                let mut list = column![].spacing(1);
                for (k, h) in self.search_hits.iter().enumerate().skip(first).take(SHOWN) {
                    let on = k == *cur;
                    let rel = h.path.strip_prefix(root).unwrap_or(&h.path).display().to_string();
                    let c = if on { rgb(t.background) } else { rgb(t.bright) };
                    list = list.push(
                        container(
                            row![
                                container(text(format!("{rel}:{}", h.line)).size(12).color(if on { c } else { rgb(t.accent) }).wrapping(iced::widget::text::Wrapping::None)).width(Length::FillPortion(2)).clip(true),
                                container(text(h.text.clone()).size(12).color(c).wrapping(iced::widget::text::Wrapping::None)).width(Length::FillPortion(3)).clip(true),
                            ]
                            .spacing(12),
                        )
                        .padding([4, 10])
                        .style(move |_| container::Style {
                            background: on.then(|| Background::Color(rgb(t.accent))),
                            border: Border::default().rounded(if self.neon() { 0 } else { 4 }),
                            ..Default::default()
                        }),
                    );
                }
                // The place itself, a few lines around the hit.
                let line = self.search_hits.get(*cur).map(|h| h.line);
                let mut preview = column![].spacing(1);
                for (no, l) in &self.search_ctx {
                    let hit = Some(*no) == line;
                    preview = preview.push(
                        row![
                            text(format!("{no:>5}")).size(11).color(if hit { rgb(t.accent) } else { Color { a: 0.5, ..fg } }),
                            text(l.clone()).size(12).color(if hit { rgb(t.bright) } else { fg }).wrapping(iced::widget::text::Wrapping::None),
                        ]
                        .spacing(10),
                    );
                }
                let preview = container(preview).padding([8, 10]).width(Length::Fill).clip(true).style(move |_| container::Style {
                    background: Some(Color { a: 0.18, ..Color::BLACK }.into()),
                    border: Border::default().rounded(4),
                    ..Default::default()
                });
                let mut body = column![field, small(status), list].spacing(10);
                if !self.search_ctx.is_empty() {
                    body = body.push(preview);
                }
                body.push(small("Enter searches, then opens the hit · ↑↓ choose · Esc closes".into())).into()
            }
            Dialog::Palette { query, cur } => {
                let hits = palette_hits(query, &self.config.actions);
                let field = container(text(if query.is_empty() { "type a command …".to_string() } else { format!("{query}▏") }).size(13).color(if query.is_empty() { fg } else { rgb(t.bright) }))
                    .padding([8, 10])
                    .width(Length::Fill)
                    .style(move |_| container::Style {
                        background: Some(Color { a: 0.25, ..Color::BLACK }.into()),
                        border: Border::default().rounded(4),
                        ..Default::default()
                    });
                // A window of rows that keeps the selection visible.
                const SHOWN: usize = 12;
                let first = cur.saturating_sub(SHOWN - 1);
                let mut list = column![].spacing(2);
                for (n, &i) in hits.iter().enumerate().skip(first).take(SHOWN) {
                    let (label, keys) = match COMMANDS.get(i) {
                        Some(&(label, keys, _)) => (label, keys),
                        None => {
                            let a = &self.config.actions[i - COMMANDS.len()];
                            (a.name.as_str(), a.key.as_str())
                        }
                    };
                    let on = n == *cur;
                    let color = if on { rgb(t.background) } else { rgb(t.bright) };
                    list = list.push(
                        button(row![text(label).size(13).color(color).width(Length::Fill), text(keys_label(keys)).size(12).color(if on { color } else { fg })].spacing(12))
                            .padding([5, 10])
                            .width(Length::Fill)
                            .on_press(Message::PaletteRun(i))
                            .style(move |_, status| button::Style {
                                background: Some(if on {
                                    rgb(t.accent)
                                } else if matches!(status, button::Status::Hovered) {
                                    Color { a: 0.06, ..Color::WHITE }
                                } else {
                                    Color::TRANSPARENT
                                }
                                .into()),
                                border: Border::default().rounded(if self.neon() { 0 } else { 4 }),
                                ..Default::default()
                            }),
                    );
                }
                if hits.is_empty() {
                    list = list.push(small("No matching command".into()));
                }
                column![field, list, small("↑↓ choose · Enter runs · Esc closes".into())].spacing(12).into()
            }
            Dialog::Drives { side, list, cur } => {
                let radius = if self.neon() { 0 } else { 4 };
                let row_style = move |on: bool| {
                    move |_: &iced::Theme, status: button::Status| button::Style {
                        background: Some(if on {
                            rgb(t.accent)
                        } else if matches!(status, button::Status::Hovered) {
                            Color { a: 0.06, ..Color::WHITE }
                        } else {
                            Color::TRANSPARENT
                        }
                        .into()),
                        border: Border::default().rounded(radius),
                        ..Default::default()
                    }
                };
                let marks = self.config.bookmarks.len();
                let heading = |label: &'static str, top: f32| {
                    container(text(label).size(11).font(BOLD).color(rgb(t.accent))).padding(iced::Padding { top, bottom: 2.0, left: 10.0, right: 0.0 })
                };
                let mut rows = column![].spacing(4);
                for (n, d) in list.iter().enumerate() {
                    let i = marks + n;
                    let on = i == *cur;
                    let main = if on { rgb(t.background) } else { rgb(t.bright) };
                    let dim = if on { rgb(t.background) } else { fg };
                    let (frac, space) = match d.space {
                        Some((total, free)) if total > 0 => (
                            (total - free) as f32 / total as f32,
                            format!("{} free of {}", human_size(free), human_size(total)),
                        ),
                        _ => (0.0, if d.network { "network".to_string() } else { String::new() }),
                    };
                    let bar = iced::widget::progress_bar(0.0..=1.0, frac).girth(4).length(120).style(move |_| iced::widget::progress_bar::Style {
                        background: Color { a: 0.25, ..Color::BLACK }.into(),
                        bar: if on { rgb(t.background) } else if frac > 0.9 { rgb(t.red) } else { rgb(t.accent) }.into(),
                        border: Border::default().rounded(2),
                    });
                    let info = column![
                        row![
                            text(format!("{}  {}", i + 1, d.label)).size(13).font(BOLD).color(main),
                            text(d.fs.clone()).size(11).color(dim),
                        ]
                        .spacing(10)
                        .align_y(iced::Alignment::Center),
                        text(self.jumper.display(&d.path)).size(11).color(dim).wrapping(iced::widget::text::Wrapping::None),
                    ]
                    .spacing(2)
                    .width(Length::Fill);
                    rows = rows.push(
                        button(
                            row![info, column![bar, text(space).size(11).color(dim)].spacing(4).align_x(iced::Alignment::End)]
                                .spacing(12)
                                .align_y(iced::Alignment::Center),
                        )
                        .padding([6, 10])
                        .width(Length::Fill)
                        .on_press(Message::DrivePick(i))
                        .style(row_style(on)),
                    );
                }
                let drive_rows = rows;
                let mut rows = column![heading("BOOKMARKS", 0.0)].spacing(4);
                if self.config.bookmarks.is_empty() {
                    rows = rows.push(container(small("None yet · Ctrl+D bookmarks the current folder".into())).padding([2, 10]));
                }
                for (b, bm) in self.config.bookmarks.iter().enumerate() {
                    let i = b;
                    let on = i == *cur;
                    let path = Config::resolve(&bm.path, &self.panes[*side].path);
                    // Unreachable targets are dimmed; network paths aren't probed (could hang).
                    let there = guppy_core::platform::is_network(&path) || path.exists() || guppy_core::archive::split(&path).is_some();
                    let main = if on { rgb(t.background) } else if there { rgb(t.bright) } else { rgb(t.muted) };
                    let dim = if on { rgb(t.background) } else if there { fg } else { rgb(t.muted) };
                    let number = if i < 9 { format!("{}  ", i + 1) } else { "   ".to_string() };
                    rows = rows.push(
                        button(
                            column![
                                text(format!("{number}{}", bm.name)).size(13).font(BOLD).color(main),
                                text(if there { bm.path.clone() } else { format!("{} · not reachable", bm.path) }).size(11).color(dim).wrapping(iced::widget::text::Wrapping::None),
                            ]
                            .spacing(2),
                        )
                        .padding([6, 10])
                        .width(Length::Fill)
                        .on_press(Message::DrivePick(i))
                        .style(row_style(on)),
                    );
                }
                let rows = rows.push(heading("DRIVES", 10.0)).push(drive_rows);
                let hint = if *cur < marks {
                    "Enter opens · Shift+Enter in other pane · Del removes · F2 renames · Alt+↑↓ moves"
                } else {
                    "↑↓ choose · Enter or 1–9 opens · Shift+Enter in other pane · Esc closes"
                };
                column![
                    title(format!("Drives & bookmarks · {} pane", if *side == 0 { "left" } else { "right" })),
                    scrollable(rows).height(Length::Shrink),
                    small(hint.into()),
                ]
                .spacing(14)
                .into()
            }
            Dialog::Bookmark { path, name, edit } => {
                let field = container(text(format!("{name}▏")).size(13).color(rgb(t.bright)))
                    .padding([8, 10])
                    .width(Length::Fill)
                    .style(move |_| container::Style {
                        background: Some(Color { a: 0.25, ..Color::BLACK }.into()),
                        border: Border::default().rounded(4),
                        ..Default::default()
                    });
                column![
                    title(if edit.is_some() { "Rename bookmark" } else { "Bookmark this folder" }.into()),
                    small(self.jumper.display(path)),
                    field,
                    buttons("Save"),
                ]
                .spacing(14)
                .into()
            }
            Dialog::Attrs { paths, mode, owner, field, recursive, error, .. } => {
                let input = |value: &str, on: bool| {
                    container(text(if on { format!("{value}▏") } else { value.to_string() }).size(13).color(rgb(t.bright)))
                        .padding([8, 10])
                        .width(Length::Fill)
                        .style(move |_| container::Style {
                            background: Some(Color { a: 0.25, ..Color::BLACK }.into()),
                            border: Border { color: if on { rgb(t.accent) } else { Color::TRANSPARENT }, width: 1.0, radius: 4.0.into() },
                            ..Default::default()
                        })
                };
                let what = match paths.as_slice() {
                    [one] => self.jumper.display(one),
                    many => format!("{} items", many.len()),
                };
                let hint = attrs::parse_mode(mode).map(attrs::symbolic).unwrap_or_else(|| "?".into());
                let check = if *recursive { "☑" } else { "☐" };
                let check_color = if *field == 2 { rgb(t.accent) } else { fg };
                let mut c = column![
                    title("Permissions & owner".into()),
                    small(what),
                    row![
                        column![small("Permissions".into()), input(mode, *field == 0), small(hint)].spacing(6).width(Length::Fill),
                        column![small("Owner (user:group)".into()), input(owner, *field == 1)].spacing(6).width(Length::Fill),
                    ]
                    .spacing(12),
                    text(format!("{check} Also everything inside folders (files keep their execute bits)")).size(12).color(check_color),
                    small("Tab next field · Space toggles · changing the owner usually needs root".into()),
                ]
                .spacing(14);
                if let Some(e) = error {
                    c = c.push(text(e.clone()).size(12).color(rgb(t.red)));
                }
                c.push(buttons("Apply")).into()
            }
            Dialog::Mark { add, pattern } => {
                let field = container(text(format!("{pattern}▏")).size(13).color(rgb(t.bright)))
                    .padding([8, 10])
                    .width(Length::Fill)
                    .style(move |_| container::Style {
                        background: Some(Color { a: 0.25, ..Color::BLACK }.into()),
                        border: Border::default().rounded(4),
                        ..Default::default()
                    });
                column![
                    title(if *add { "Mark files" } else { "Unmark files" }.into()),
                    small("Wildcards * and ?, several patterns separated by spaces. End a pattern with / for folders.".into()),
                    field,
                    buttons(if *add { "Mark" } else { "Unmark" }),
                ]
                .spacing(14)
                .into()
            }
            Dialog::Settings { row: sel, edit } => {
                let mut list = column![].spacing(2);
                let mut section = "";
                // Fixed rows, then "Helper programs" with their status.
                let tool_rows = guppy_core::tools::TOOLS.iter().enumerate().map(|(k, tool)| {
                    let value = if self.tools_found.get(k) == Some(&true) {
                        "found"
                    } else if guppy_core::tools::install_command(tool).is_some() {
                        "missing · Enter installs"
                    } else {
                        "missing · Enter: download page"
                    };
                    (SETTINGS.len() + k, "Helper programs", tool.name, Some(value.to_string()))
                });
                let rows = SETTINGS.iter().enumerate().map(|(i, &(head, label))| (i, head, label, None)).chain(tool_rows);
                for (i, head, label, fixed) in rows {
                    if head != section {
                        section = head;
                        let gap = if i == 0 { 0 } else { 12 };
                        list = list.push(
                            container(text(head).size(11).font(BOLD).color(rgb(t.accent))).padding(iced::Padding { top: gap as f32, bottom: 4.0, ..Default::default() }),
                        );
                    }
                    let selected = i == *sel;
                    let value = match (edit, fixed) {
                        (Some(e), _) if selected => format!("{e}▏"),
                        (_, Some(v)) => v,
                        _ => setting_value(&self.config, i),
                    };
                    let on = !matches!(value.as_str(), "off") && !value.starts_with("missing");
                    let value_color = if selected { rgb(t.background) } else if on { rgb(t.bright) } else { fg };
                    let label_color = if selected { rgb(t.background) } else { fg };
                    list = list.push(
                        button(
                            row![
                                text(label).size(13).color(label_color).width(Length::Fill),
                                text(value).size(13).font(BOLD).color(value_color).wrapping(iced::widget::text::Wrapping::None),
                            ]
                            .spacing(12),
                        )
                        .padding([5, 10])
                        .width(Length::Fill)
                        .on_press(Message::SettingClick(i))
                        .style(move |_, status| button::Style {
                            background: Some(if selected {
                                rgb(t.accent)
                            } else if matches!(status, button::Status::Hovered) {
                                Color { a: 0.06, ..Color::WHITE }
                            } else {
                                Color::TRANSPARENT
                            }.into()),
                            border: Border::default().rounded(4),
                            ..Default::default()
                        }),
                    );
                }
                let tool = sel.checked_sub(SETTINGS.len()).and_then(|k| guppy_core::tools::TOOLS.get(k));
                let hint = match (edit, tool) {
                    (Some(_), _) => "type a path · empty = startup folder · Enter saves · Esc cancels".to_string(),
                    (None, Some(t)) => match guppy_core::tools::install_command(t) {
                        _ if t.builtin && guppy_core::tools::find(t.bin).is_some() => format!("For {} · part of the system", t.purpose),
                        Some(cmd) => format!("For {} · Enter runs: {cmd}", t.purpose),
                        None => format!("For {} · Enter opens {}", t.purpose, t.url),
                    },
                    (None, None) if setting_is_text(*sel) => "Enter edits · . takes this pane's folder · Esc closes".to_string(),
                    (None, None) => "↑↓ choose · Space/←→ change · Esc closes".to_string(),
                };
                let hint = match &self.tools_note {
                    Some(note) => format!("{note}\n{hint}"),
                    None => hint,
                };
                let file = Config::path().map(|p| self.jumper.display(&p)).unwrap_or_default();
                column![
                    title("Settings".into()),
                    list,
                    column![small(hint), text(format!("Saved to {file}")).size(11).color(Color { a: 0.6, ..fg })].spacing(4),
                ]
                .spacing(16)
                .into()
            }
            Dialog::Help { startup } => {
                let section = |(head, keys): &(&'static str, &'static [(&'static str, &'static str)])| {
                    let mut c = column![text(*head).size(11).font(BOLD).color(rgb(t.accent))].spacing(5);
                    for (k, what) in keys.iter() {
                        c = c.push(row![
                            text(keys_label(k)).size(12).font(BOLD).color(rgb(t.bright)).width(118),
                            text(*what).size(12).color(rgb(t.foreground)),
                        ]);
                    }
                    c
                };
                column![
                    title("Guppy · Keys".into()),
                    small("Everything works from the keyboard. Typing filters the active pane, ~ or / jumps to a folder. Without marks, file operations act on the entry under the cursor; Enter confirms, Esc cancels.".into()),
                    row![
                        column![section(&HELP[0]), section(&HELP[1])].spacing(18).width(Length::Fill),
                        column![section(&HELP[2]), section(&HELP[3])].spacing(18).width(Length::Fill),
                    ]
                    .spacing(24),
                    row![
                        if *startup {
                            // Flat checkbox: small bordered box, filled when on.
                            let on = self.config.start.help;
                            let tick = container(text(if on { "✓" } else { "" }).size(11).color(rgb(t.background)))
                                .center_x(15)
                                .center_y(15)
                                .style(move |_| container::Style {
                                    background: on.then(|| rgb(t.accent).into()),
                                    border: Border { color: rgb(t.accent), width: 1.0, radius: 3.0.into() },
                                    ..Default::default()
                                });
                            Element::from(
                                mouse_area(row![tick, small("Show at every start · Space".into())].spacing(8).align_y(iced::Alignment::Center))
                                    .on_press(Message::ToggleStartHelp)
                                    .interaction(iced::mouse::Interaction::Pointer),
                            )
                        } else {
                            small("F1 or Esc closes".into()).into()
                        },
                        iced::widget::Space::new().width(Length::Fill),
                        btn("OK", Message::DialogCancel, true)
                    ]
                        .align_y(iced::Alignment::Center),
                ]
                .spacing(18)
                .into()
            }
            Dialog::Report { title: heading, problems } => {
                let mut lines: Vec<String> = problems.iter().take(8).cloned().collect();
                if problems.len() > 8 {
                    lines.push(format!("and {} more", problems.len() - 8));
                }
                column![
                    title(heading.to_string()),
                    small(lines.join("\n")),
                    row![iced::widget::Space::new().width(Length::Fill), btn("OK", Message::DialogCancel, true)],
                ]
                .spacing(14)
                .into()
            }
        };

        let viewer = matches!(dialog, Dialog::View { .. } | Dialog::Edit { .. });
        let card = container(content)
            .padding(22)
            .width(match dialog {
                Dialog::View { .. } | Dialog::Edit { .. } => Length::Fill,
                Dialog::Help { .. } | Dialog::Search { .. } => Length::Fixed(760.0),
                Dialog::Settings { .. } | Dialog::Jobs | Dialog::Conflict { .. } | Dialog::Palette { .. } | Dialog::Drives { .. } | Dialog::Attrs { .. } => Length::Fixed(560.0),
                _ => Length::Fixed(480.0),
            })
            .height(if viewer { Length::Fill } else { Length::Shrink })
            .max_width(1400)
        .style(move |_| container::Style {
            background: Some(rgb(t.sheet).into()),
            border: Border { color: rgb(t.accent), width: 1.5, radius: self.radius().into() },
            ..Default::default()
        });
        // The backdrop darkens and the card rises a few pixels as it opens.
        let k = Self::eased(self.dialog_at, APPEAR);
        let pad = if viewer { 32.0 } else { 0.0 };
        Some(
            container(card)
                .center(Length::Fill)
                .padding(iced::Padding { top: pad + 14.0 * (1.0 - k), bottom: pad, left: pad, right: pad })
                .style(move |_| container::Style {
                    background: Some(Color { a: 0.6 * k, ..Color::BLACK }.into()),
                    ..Default::default()
                })
                .into(),
        )
    }

    /// The content of a viewer: pictures fit the area, text scrolls.
    fn body_view<'a>(&'a self, path: &'a Path, body: &'a ViewBody, id: Id) -> Element<'a, Message> {
        let t = &self.theme;
        let small = |s: String| text(s).size(12).color(rgb(t.foreground));
        let mono = |s: &str| text(s.to_string()).size(12).font(FONT).color(rgb(t.bright));
        let scroll = |c: Element<'a, Message>| scrollable(c).id(id.clone()).height(Length::Fill).width(Length::Fill).into();
        let fit = |c: Element<'a, Message>| container(c).center(Length::Fill).into();
        match body {
            ViewBody::Loading => small("loading …".into()).into(),
            ViewBody::Text { text: s, truncated } | ViewBody::Hex { text: s, truncated } => {
                let mut c = column![mono(s)];
                if *truncated {
                    c = c.push(small("… the rest is not shown · F3 shows more".into()));
                }
                scroll(c.into())
            }
            ViewBody::Dir(s) => scroll(mono(s).into()),
            ViewBody::Image => fit(iced::widget::image(iced::widget::image::Handle::from_path(path)).content_fit(iced::ContentFit::ScaleDown).into()),
            ViewBody::Svg => fit(svg(svg::Handle::from_path(path)).content_fit(iced::ContentFit::ScaleDown).into()),
            ViewBody::Picture { image, caption } => column![
                container(iced::widget::image(image.clone()).content_fit(iced::ContentFit::ScaleDown).opacity(Self::eased(self.picture_at, APPEAR))).center(Length::Fill),
                small(caption.clone()),
            ]
            .spacing(10)
            .into(),
            ViewBody::Other(s) => fit(small(s.clone()).into()),
        }
    }

    /// Quick view: the other pane's sheet showing the entry under the cursor.
    fn quick_pane(&self, side: usize) -> Element<'_, Message> {
        let t = &self.theme;
        let (name, info) = match &self.quick_view {
            Some((p, _)) => (p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), file_info(p)),
            None => (String::new(), String::new()),
        };
        let header = container(
            row![
                text(if self.neon() { "» PREVIEW" } else { "PREVIEW" }).size(11).font(BOLD).color(rgb(t.accent)),
                container(text(name).size(12).color(rgb(t.bright)).wrapping(iced::widget::text::Wrapping::None)).width(Length::Fill).clip(true),
                text(info).size(11).color(rgb(t.foreground)),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center),
        )
        .padding([10, 10]);
        let body: Element<'_, Message> = match &self.quick_view {
            Some((p, b)) => container(self.body_view(p, b, quick_id())).padding([4, 14]).height(Length::Fill).into(),
            None => container(text(keys_label("Ctrl+Q closes the preview")).size(12).color(rgb(t.foreground))).center(Length::Fill).into(),
        };
        let outline = self.neon().then(|| rgb(t.muted));
        self.sheet(column![header, body].into(), side, t.sheet_dim, outline).width(Length::FillPortion(3)).into()
    }

    /// The active folder as tiles sized by their content, largest first.
    fn map_pane(&self, side: usize) -> Element<'_, Message> {
        let t = &self.theme;
        let p = &self.panes[self.active];
        let visible = p.visible(self.show_hidden, &self.sizes);
        let mut items: Vec<(&Entry, u64)> = visible.iter().map(|e| (*e, entry_size(e, &self.sizes).unwrap_or(0))).filter(|(_, n)| *n > 0).collect();
        items.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        let total: u64 = items.iter().map(|(_, n)| n).sum();
        // The smallest ones together as one tile, so the map stays readable.
        const MAX: usize = 60;
        let rest: Vec<(&Entry, u64)> = if items.len() > MAX { items.split_off(MAX) } else { Vec::new() };
        // Neighbouring tiles get clearly different hues, all derived from the theme's accent.
        let mut tiles: Vec<treemap::Tile> = items
            .iter()
            .enumerate()
            .map(|(i, (e, n))| treemap::Tile {
                path: Some(e.path.clone()),
                name: if e.is_dir() { format!("/{}", e.name) } else { e.name.clone() },
                size: *n,
                color: treemap::hue(rgb(t.accent), i),
            })
            .collect();
        if !rest.is_empty() {
            tiles.push(treemap::Tile { path: None, name: format!("{} more", rest.len()), size: rest.iter().map(|(_, n)| n).sum(), color: rgb(t.light_foreground) });
        }
        let pending = self.sizes.busy();
        let header = container(
            row![
                text(if self.neon() { "» SPACE" } else { "SPACE" }).size(11).font(BOLD).color(rgb(t.accent)),
                container(text(self.jumper.display(&p.path)).size(12).color(rgb(t.bright)).wrapping(iced::widget::text::Wrapping::None)).width(Length::Fill).clip(true),
                text(format!("{}{}", human_size(total), if pending { " · counting …" } else { "" })).size(11).color(rgb(t.foreground)),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center),
        )
        .padding([10, 10]);
        let cursor = visible.get(p.cursor).map(|e| e.path.clone());
        let body: Element<'_, Message> = if tiles.is_empty() {
            container(text(if pending { "counting …" } else { "Nothing here takes up space" }).size(12).color(rgb(t.foreground))).center(Length::Fill).into()
        } else {
            let map = treemap::Map { tiles, cursor, text: rgb(t.bright), gap: rgb(t.sheet_dim), grow: Self::eased(self.map_at, MAP_IN) };
            container(iced::widget::canvas(map).width(Length::Fill).height(Length::Fill)).padding(iced::Padding { top: 0.0, left: 8.0, right: 8.0, bottom: 8.0 }).into()
        };
        let outline = self.neon().then(|| rgb(t.muted));
        self.sheet(column![header, body].into(), side, t.sheet_dim, outline).width(Length::FillPortion(3)).into()
    }

    fn icon_button(&self, data: &'static str, label: &'static str, msg: Option<Message>) -> Element<'_, Message> {
        let t = &self.theme;
        let enabled = msg.is_some();
        let icon = svg(svg::Handle::from_memory(data.as_bytes()))
            .width(18)
            .height(18)
            .style(move |_, status| svg::Style {
                color: Some(match (enabled, status) {
                    (false, _) => rgb(t.muted),
                    (true, svg::Status::Hovered) => rgb(t.bright),
                    _ => rgb(t.light_foreground),
                }),
            });
        let b = button(container(icon).center(34))
            .padding(0)
            .on_press_maybe(msg)
            .style(move |_, status| button::Style {
                background: matches!(status, button::Status::Hovered)
                    .then(|| rgb(t.lighter_background).into()),
                border: Border::default().rounded(6),
                ..Default::default()
            });
        iced::widget::tooltip(b, text(label).size(12), iced::widget::tooltip::Position::Bottom)
            .style(move |_| container::Style {
                background: Some(rgb(t.lighter_background).into()),
                text_color: Some(rgb(t.foreground)),
                border: Border::default().rounded(4),
                ..Default::default()
            })
            .padding(6)
            .into()
    }

    fn menubar(&self) -> Element<'_, Message> {
        let t = &self.theme;
        let sep = || {
            let line = container(iced::widget::Space::new().width(1).height(18)).style(move |_| container::Style {
                background: Some(rgb(t.selection).into()),
                ..Default::default()
            });
            container(line).padding([0, 6])
        };
        let p = &self.panes[self.active];
        let bar = row![
            text(if self.neon() { ">_" } else { "◆" }).color(rgb(t.accent)).font(BOLD).size(13),
            iced::widget::Space::new().width(10),
            self.icon_button(icons::BACK, "Back (Alt ←)", (!p.back.is_empty()).then_some(Message::Back)),
            self.icon_button(icons::FORWARD, "Forward (Alt →)", (!p.forward.is_empty()).then_some(Message::Forward)),
            self.icon_button(icons::UP, "Parent folder (⌫)", Some(Message::Parent)),
            self.icon_button(icons::HOME, "Home", Some(Message::Home)),
            sep(),
            self.icon_button(icons::NEW_DIR, "New folder (F7)", Some(Message::NewDir)),
            self.icon_button(icons::COPY, "Copy (F5)", Some(Message::Op(OpKind::Copy))),
            self.icon_button(icons::MOVE, "Move (F6)", Some(Message::Op(OpKind::Move))),
            self.icon_button(icons::DELETE, "Trash (F8 · Del), permanently with Shift", Some(Message::Op(OpKind::Trash))),
            self.icon_button(icons::EMPTY_TRASH, "Empty trash (asks first)", Some(Message::EmptyTrash)),
            sep(),
            self.icon_button(icons::MARK, "Mark (Ins)", Some(Message::ToggleMark)),
            self.icon_button(icons::SWAP, "Switch side (Tab)", Some(Message::SwitchSide)),
            iced::widget::Space::new().width(Length::Fill),
            self.search_field(),
            iced::widget::Space::new().width(Length::Fill),
            self.icon_button(icons::HIDDEN, "Hidden files (Ctrl H)", Some(Message::ToggleHidden)),
            self.icon_button(icons::SETTINGS, "Settings (F12)", Some(Message::OpenSettings)),
        ]
        .spacing(2)
        .align_y(iced::Alignment::Center);
        container(bar)
            .padding([0, 10])
            .height(46)
            .center_y(46)
            .width(Length::Fill)
            .style(move |_| container::Style {
                background: Some(rgb(t.dark_background).into()),
                ..Default::default()
            })
            .into()
    }

    fn neon(&self) -> bool {
        self.config.view.look == Look::Neon
    }

    fn radius(&self) -> f32 {
        if self.neon() { 0.0 } else { SHEET_RADIUS }
    }

    fn class_color(&self, c: FileClass) -> Color {
        let t = &self.theme;
        match c {
            FileClass::Dir => rgb(t.bright),
            FileClass::Code => rgb(t.code),
            FileClass::Doc | FileClass::Other => rgb(t.foreground),
            FileClass::Image => rgb(t.image),
            FileClass::Archive => rgb(t.red),
        }
    }

    fn pane_view(&self, side: usize) -> Element<'_, Message> {
        let t = &self.theme;
        let p = &self.panes[side];
        let active = side == self.active;
        let list = p.visible(self.show_hidden, &self.sizes);
        let max_size = list.iter().filter_map(|e| entry_size(e, &self.sizes)).max().unwrap_or(0);

        let row_radius = if self.neon() { 0 } else { 3 };
        // Characters that fit in the name column (monospace, 13 px ≈ 7.8 px per char):
        // pane minus frame, scrollbar, row padding, dot, size and the optional columns.
        let pane_w = (self.width - 26.0 - 16.0) / 2.0;
        let mut name_w = pane_w - 2.0 - 12.0 - 10.0 - 20.0 - 14.0 - 8.0 - 8.0 - 84.0;
        if self.show_dates() {
            name_w -= 108.0;
        }
        if self.git[side].is_some() {
            name_w -= 20.0;
        }
        if p.sort == Sort::Size {
            name_w -= 64.0;
        }
        let name_chars = (name_w / 7.8).floor().max(3.0) as usize;
        let rows = list.iter().enumerate().map(|(i, e)| {
            let marked = p.marked.contains(&e.path);
            let is_cur = i == p.cursor;
            let on_cursor = is_cur && active;
            let fg = if on_cursor {
                rgb(t.background)
            } else if marked {
                rgb(t.marked)
            } else {
                self.class_color(e.class())
            };
            let dot = if marked { "●" } else { " " };
            let dot_color = if on_cursor { fg } else if marked { rgb(t.marked) } else { self.class_color(e.class()) };
            let name = if e.is_dir() { format!("/{}", e.name) } else { e.name.clone() };
            let name = cut_middle(&name, name_chars);
            let (size, pending) = match (e.is_dir() && !e.is_virtual(), self.sizes.get(&e.path)) {
                (false, _) => (human_size(e.size), false),
                (true, Some(DirSize::Done(n))) => (human_size(n), false),
                // From the last run's cache, currently being verified.
                (true, Some(DirSize::Cached(n))) => (format!("≈ {}", human_size(n)), true),
                (true, Some(DirSize::Partial(n))) => (format!("… {}", human_size(n)), true),
                (true, None) => ("…".to_string(), true),
            };
            let share = (p.sort == Sort::Size).then(|| {
                let frac = entry_size(e, &self.sizes).unwrap_or(0) as f32 / max_size.max(1) as f32;
                let fill = (frac * 1000.0).round() as u16;
                let bar_color = if on_cursor { Color { a: 0.55, ..rgb(t.background) } } else { Color { a: 0.55, ..rgb(t.accent) } };
                let track = Color { a: if on_cursor { 0.15 } else { 0.12 }, ..Color::BLACK };
                container(row![
                    container(iced::widget::Space::new().height(4)).width(Length::FillPortion(fill.max(1))).style(move |_| container::Style {
                        background: Some(if fill == 0 { track } else { bar_color }.into()),
                        border: Border::default().rounded(2),
                        ..Default::default()
                    }),
                    container(iced::widget::Space::new().height(4)).width(Length::FillPortion(1000 - fill.min(999))),
                ])
                .width(56)
                .style(move |_| container::Style {
                    background: Some(track.into()),
                    border: Border::default().rounded(2),
                    ..Default::default()
                })
            });
            let font = if e.is_dir() { BOLD } else { FONT };
            let editing = self.rename.as_ref().filter(|r| r.side == side && is_cur && r.from == e.path);
            let name: Element<'_, Message> = match editing {
                // Name being edited: plain field with a caret, inside the row.
                Some(r) => {
                    let part = |s: &str| text(s.to_string()).color(rgb(t.bright)).size(13).font(font).wrapping(iced::widget::text::Wrapping::None);
                    let caret = || container(iced::widget::Space::new().width(2).height(15)).style(move |_| container::Style {
                        background: Some(rgb(t.accent).into()),
                        ..Default::default()
                    });
                    let (lo, hi) = r.selection().unwrap_or((r.caret, r.caret));
                    let (lo_b, hi_b) = (r.byte(lo), r.byte(hi));
                    let marked = container(part(&r.name[lo_b..hi_b])).style(move |_| container::Style {
                        background: Some(Color { a: 0.45, ..rgb(t.accent) }.into()),
                        ..Default::default()
                    });
                    let mut field = row![part(&r.name[..lo_b])];
                    if r.caret == lo {
                        field = field.push(caret());
                    }
                    if hi > lo {
                        field = field.push(marked);
                    }
                    if r.caret == hi && hi > lo {
                        field = field.push(caret());
                    }
                    field = field.push(part(&r.name[hi_b..]));
                    container(field.align_y(iced::Alignment::Center))
                        .padding([2, 6])
                        .style(move |_| container::Style {
                            background: Some(rgb(t.dark_background).into()),
                            border: Border::default().rounded(3),
                            ..Default::default()
                        })
                        .into()
                }
                None => text(name).color(fg).size(13).font(font).wrapping(iced::widget::text::Wrapping::None).into(),
            };
            let (size, size_color) = match editing.and_then(|r| r.error.clone()) {
                Some(err) => (err, rgb(t.red)),
                None => (size, if on_cursor { fg } else if pending { rgb(t.light_foreground) } else { rgb(t.foreground) }),
            };
            let git = self.git[side].as_ref().and_then(|g| g.mark(&e.name)).map(|m| {
                use guppy_core::git::Mark;
                let color = match m {
                    _ if on_cursor => fg,
                    Mark::Conflict => rgb(t.red),
                    Mark::Modified => rgb(t.code),
                    Mark::Added | Mark::Untracked => rgb(t.marked),
                    Mark::Inside => rgb(t.accent),
                    Mark::Ignored => rgb(t.light_foreground),
                };
                text(m.symbol()).color(color).size(11).font(BOLD).width(12).align_x(iced::alignment::Horizontal::Center)
            });
            let mut line = row![
                text(dot).color(dot_color).size(12).width(14),
                container(name)
                .width(Length::Fill)
                .clip(true),
            ];
            if let Some(g) = git {
                line = line.push(g);
            }
            let mut line = line.push(
                text(size)
                    .color(size_color)
                    .size(12)
                    .width(if editing.is_some_and(|r| r.error.is_some()) { Length::Shrink } else { Length::Fixed(84.0) })
                    .align_x(iced::alignment::Horizontal::Right),
            )
            .spacing(8)
            .align_y(iced::Alignment::Center);
            if let Some(bar) = share {
                line = line.push(bar);
            }
            if self.show_dates() {
                line = line.push(
                text(e.modified.map(human_date).unwrap_or_default())
                    .color(if on_cursor { fg } else { rgb(t.foreground) })
                    .size(12)
                    .width(100)
                    .align_x(iced::alignment::Horizontal::Right)

                );
            }
            // A folder that would take the dragged files.
            let drop_here = self.drag.as_ref().is_some_and(|d| d.active && d.over == Some((side, i)) && e.is_dir() && !d.paths.contains(&e.path))
                || (self.ext_over == Some((side, i)) && e.is_dir());
            let glow = self.glow.iter().filter(|(p, _)| *p == e.path).map(|(_, t)| Self::progress(*t, GLOW)).fold(None, |a: Option<f32>, x| Some(a.map_or(x, |a| a.min(x))));
            let bg = if on_cursor {
                Some(rgb(t.accent))
            } else if let Some(k) = glow {
                // Bright at first, then fading out (ease-out).
                Some(Color { a: 0.45 * (1.0 - k) * (1.0 - k), ..rgb(t.accent) })
            } else if is_cur {
                Some(Color { a: 0.09, ..Color::WHITE })
            } else {
                None
            };
            let border = if drop_here { Border { color: rgb(t.accent), width: 1.5, radius: row_radius.into() } } else { Border::default().rounded(row_radius) };
            let area = mouse_area(
                container(line)
                    .height(ROW_H)
                    .padding([0, 10])
                    .center_y(ROW_H)
                    .style(move |_| container::Style {
                        background: bg.map(Background::Color),
                        border,
                        ..Default::default()
                    }),
            )
            .on_press(Message::Select(side, i))
            .on_double_click(Message::Open(side, i));
            // Rows report the pointer only during a drag, so moving the mouse costs nothing otherwise.
            if self.drag.as_ref().is_some_and(|d| d.active) {
                area.on_enter(Message::DragOver(side, i)).on_exit(Message::DragLeft(side, i)).into()
            } else {
                area.into()
            }
        });

        let header = container(
            row![
                container(match &self.loads[side] {
                    // Folder change still waiting on the filesystem.
                    Some(l) if !matches!(l.after, After::Reload) => {
                        let state = if l.since.elapsed() >= LOAD_SLOW { "not responding · Esc cancels" } else { "loading …" };
                        text(format!("{} · {state}", l.path.display()))
                            .size(12)
                            .color(if active { rgb(t.accent) } else { rgb(t.foreground) })
                            .wrapping(iced::widget::text::Wrapping::None)
                            .into()
                    }
                    _ => self.crumbs(side, active),
                })
                .width(Length::Fill)
                .clip(true),
                text(self.git[side].as_ref().map(|g| {
                    let mut s = format!("⎇ {}", g.branch);
                    if g.dirty {
                        s.push('*');
                    }
                    if g.ahead > 0 {
                        s += &format!(" ↑{}", g.ahead);
                    }
                    if g.behind > 0 {
                        s += &format!(" ↓{}", g.behind);
                    }
                    s
                }).unwrap_or_default()).size(11).font(BOLD).color(rgb(t.accent)),
                text(if p.archive { "ARCHIVE · " } else if p.network { "NET · " } else { "" }).size(11).font(BOLD).color(rgb(t.foreground)),
                text(p.sort.label()).size(11).color(rgb(t.foreground)),
            ]
            .spacing(10),
        )
        .padding([10, 10])
        .width(Length::Fill)
        .style(move |_| container::Style {
            border: Border { color: rgb(t.muted), width: 0.0, radius: 0.0.into() },
            ..Default::default()
        });

        // Rows that just went away fold up where they were.
        let mut rows: Vec<Element<'_, Message>> = rows.collect();
        let mut ghosts: Vec<&(usize, Entry, std::time::Instant)> = self.ghosts[side].iter().collect();
        ghosts.sort_by_key(|g| std::cmp::Reverse(g.0));
        for (at, e, start) in ghosts {
            let k = Self::progress(*start, FOLD);
            let h = ROW_H * (1.0 - k) * (1.0 - k);
            let name = if e.is_dir() { format!("/{}", e.name) } else { e.name.clone() };
            let ghost = container(text(name).size(13).color(Color { a: 1.0 - k, ..self.class_color(e.class()) }).wrapping(iced::widget::text::Wrapping::None))
                .height(h)
                .padding(iced::Padding { left: 32.0, ..Default::default() })
                .center_y(h)
                .clip(true);
            rows.insert((*at).min(rows.len()), ghost.into());
        }
        let list = scrollable(column(rows).padding([0, 6]))
            .direction(scrollable::Direction::Vertical(scrollable::Scrollbar::new().width(4).scroller_width(4).margin(3)))
            .style(move |_, status| {
                let hot = matches!(status, scrollable::Status::Hovered { .. } | scrollable::Status::Dragged { .. });
                let rail = scrollable::Rail {
                    background: None,
                    border: Border::default(),
                    scroller: scrollable::Scroller {
                        background: Color { a: if hot { 0.45 } else { 0.22 }, ..Color::WHITE }.into(),
                        border: Border::default().rounded(2),
                    },
                };
                scrollable::Style {
                    container: container::Style::default(),
                    vertical_rail: rail,
                    horizontal_rail: rail,
                    gap: None,
                    auto_scroll: scrollable::AutoScroll {
                        background: rgb(t.lighter_background).into(),
                        border: Border::default().rounded(4),
                        shadow: iced::Shadow::default(),
                        icon: rgb(t.foreground),
                    },
                }
            })
            .id(p.id.clone())
            .on_scroll(move |vp| Message::Scrolled(side, vp))
            .height(Length::Fill);

        let body: Element<'_, Message> = if active && self.jumping() { self.jump_view() } else { list.into() };
        let tone = if active { t.sheet } else { t.sheet_dim };
        // Neon frames both panes; paper outlines only the active one.
        let k = Self::eased(self.active_at, APPEAR);
        let rest = if self.neon() { rgb(t.muted) } else { Color { a: 0.0, ..rgb(t.accent) } };
        let outline = if active { Some(mix(rest, rgb(t.accent), k)) } else { self.neon().then(|| rgb(t.muted)) };
        let content: Element<'_, Message> = column![]
            .push(self.tab_bar(side, active))
            .push(self.favorite_bar(side, tone))
            .push(header)
            .push(body)
            .into();
        let sheet = self.sheet(content, side, tone, outline)
            .width(Length::FillPortion(3));
        mouse_area(sheet).on_enter(Message::HoverPane(side)).into()
    }

    /// The path as clickable parts, each with its size when known.
    fn crumbs(&self, side: usize, active: bool) -> Element<'_, Message> {
        let t = &self.theme;
        let p = &self.panes[side];
        let color = if active { rgb(t.accent) } else { rgb(t.foreground) };
        let dim = Color { a: 0.65, ..color };
        let home = dirs_home();
        let mut parts: Vec<PathBuf> = p.path.ancestors().map(Path::to_path_buf).collect();
        parts.reverse();
        // Under home the path starts at `~`.
        if p.path.starts_with(&home) {
            parts.retain(|a| a.starts_with(&home));
        }
        let mut line = row![].align_y(iced::Alignment::Center);
        if self.neon() {
            line = line.push(text("» ").size(12).color(color));
        }
        let last = parts.len().saturating_sub(1);
        for (k, part) in parts.into_iter().enumerate() {
            let name = if part == home {
                "~".to_string()
            } else {
                part.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| part.display().to_string())
            };
            let size = match self.sizes.get(&part) {
                Some(DirSize::Done(n)) => Some(human_size(n)),
                Some(DirSize::Cached(n)) => Some(format!("≈{}", human_size(n))),
                Some(DirSize::Partial(n)) => Some(format!("…{}", human_size(n))),
                None => None,
            };
            let mut seg = row![text(name).size(12).color(color).font(if k == last { BOLD } else { FONT }).wrapping(iced::widget::text::Wrapping::None)]
                .spacing(4)
                .align_y(iced::Alignment::Center);
            if let Some(size) = size {
                let badge = container(text(size).size(11).font(BOLD).color(rgb(t.bright)).wrapping(iced::widget::text::Wrapping::None))
                    .padding([1, 6])
                    .style(move |_| container::Style {
                        background: Some(Color { a: 0.28, ..Color::BLACK }.into()),
                        border: Border::default().rounded(if self.neon() { 0 } else { 3 }),
                        ..Default::default()
                    });
                seg = seg.push(badge);
            }
            if k > 0 && !(k == 1 && line_starts_root(&p.path, &home)) {
                line = line.push(text(" / ").size(12).color(dim));
            }
            line = line.push(mouse_area(seg).on_press(Message::GoTo(side, part)).interaction(iced::mouse::Interaction::Pointer));
        }
        line.into()
    }

    /// Four folder buttons at the top of a pane, once at least one is set.
    /// Click opens, right-click sets or clears.
    fn favorite_bar(&self, side: usize, tone: u32) -> Option<Element<'_, Message>> {
        let t = &self.theme;
        if self.config.favorites.iter().all(|f| f.path.is_empty()) {
            return None;
        }
        let (sheet, ink) = (rgb(tone), rgb(t.foreground));
        let here = &self.panes[side].path;
        let buttons = (0..4).map(|slot| {
            let fav = self.config.favorites.get(slot).filter(|f| !f.path.is_empty());
            let path = fav.map(|f| Config::resolve(&f.path, here));
            let shown = path.as_ref() == Some(here);
            let (name, color) = match fav {
                Some(f) => (f.name.clone(), if shown { rgb(t.accent) } else { rgb(t.bright) }),
                None => ("+".to_string(), rgb(t.foreground)),
            };
            let label = row![
                text(format!("{}", slot + 1)).size(10).font(BOLD).color(Color { a: 0.6, ..color }),
                text(name).size(if fav.is_some() { 12 } else { 14 }).font(if shown || fav.is_none() { BOLD } else { FONT }).color(color).wrapping(iced::widget::text::Wrapping::None),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center);
            let b = button(container(label).center_x(Length::Fill).center_y(Length::Fill).clip(true))
                .padding([0, 6])
                .width(Length::Fill)
                .height(26)
                .on_press(Message::Favorite(side, slot))
                .style(move |_, status| button::Style {
                    background: Some(match status {
                        button::Status::Hovered | button::Status::Pressed => mix(sheet, ink, 0.14),
                        _ if shown => mix(sheet, ink, 0.08),
                        _ => mix(sheet, ink, 0.04),
                    }.into()),
                    border: Border {
                        color: if shown { rgb(t.accent) } else { Color { a: 0.35, ..rgb(t.foreground) } },
                        width: 1.0,
                        radius: 5.0.into(),
                    },
                    ..Default::default()
                });
            let tip = match fav {
                Some(f) => format!("{}\nRight-click: put this folder here (again: clear)", f.path),
                None => "Click or right-click: put this folder here".to_string(),
            };
            let b = mouse_area(b).on_right_press(Message::SetFavorite(side, slot));
            iced::widget::tooltip(b, text(tip).size(12), iced::widget::tooltip::Position::Bottom)
                .style(move |_| container::Style {
                    background: Some(rgb(t.lighter_background).into()),
                    text_color: Some(rgb(t.foreground)),
                    border: Border::default().rounded(4),
                    ..Default::default()
                })
                .padding(6)
                .delay(Duration::from_millis(600))
                .into()
        });
        Some(container(row(buttons).spacing(4)).padding(iced::Padding { top: 6.0, left: 6.0, right: 6.0, bottom: 0.0 }).into())
    }

    /// Tabs above the pane header, only when there is more than one.
    fn tab_bar(&self, side: usize, active: bool) -> Option<Element<'_, Message>> {
        if self.tabs[side].is_empty() {
            return None;
        }
        let t = &self.theme;
        let mut list: Vec<&Pane> = self.tabs[side].iter().collect();
        list.insert(self.tab_at[side], &self.panes[side]);
        let tabs = list.into_iter().enumerate().map(|(k, p)| {
            let shown = k == self.tab_at[side];
            let name = p.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.path.display().to_string());
            let color = if shown && active { rgb(t.accent) } else if shown { rgb(t.bright) } else { rgb(t.foreground) };
            let line = if shown { color } else { Color::TRANSPARENT };
            // The number is the Ctrl+number that shows the tab.
            let number = text(if k < 9 { format!("{}", k + 1) } else { String::new() }).size(10).font(BOLD).color(Color { a: 0.6, ..color });
            mouse_area(
                container(row![number, text(name).size(12).font(if shown { BOLD } else { FONT }).color(color).wrapping(iced::widget::text::Wrapping::None)].spacing(6).align_y(iced::Alignment::Center))
                    .padding([5, 10])
                    .max_width(180)
                    .clip(true)
                    .style(move |_| container::Style {
                        border: Border { color: line, width: 0.0, radius: 0.0.into() },
                        background: shown.then(|| Background::Color(Color { a: 0.07, ..Color::WHITE })),
                        ..Default::default()
                    }),
            )
            .on_press(Message::TabClick(side, k))
            .interaction(iced::mouse::Interaction::Pointer)
            .into()
        });
        Some(container(row(tabs).spacing(2)).padding(iced::Padding { top: 6.0, left: 6.0, right: 6.0, bottom: 0.0 }).into())
    }

    /// Jump mode hit list, in place of the active pane's file list.
    fn jump_view(&self) -> Element<'_, Message> {
        let t = &self.theme;
        let rows = self.jump.iter().enumerate().map(|(i, path)| {
            let on = i == self.jump_cur;
            let fg = if on { rgb(t.background) } else { rgb(t.bright) };
            mouse_area(
                container(text(self.jumper.display(path)).color(fg).size(13).wrapping(iced::widget::text::Wrapping::None))
                    .clip(true)
                    .width(Length::Fill)
                    .height(ROW_H)
                    .padding([0, 10])
                    .center_y(ROW_H)
                    .style(move |_| container::Style {
                        background: on.then(|| rgb(t.accent).into()),
                        border: Border::default().rounded(3),
                        ..Default::default()
                    }),
            )
            .on_press(Message::JumpTo(i))
            .into()
        });
        let hint = if self.jump.is_empty() { "No matches" } else { "↑↓ select · Enter jump · Esc back" };
        column![
            container(text("JUMP TO").size(11).color(rgb(t.foreground))).padding([4, 16]),
            column(rows).padding([0, 6]),
            container(text(hint).size(11).color(rgb(t.foreground))).padding([8, 16]),
        ]
        .into()
    }

    /// A sheet of paper: grain texture under the content, optionally with a colored border.
    fn sheet<'a>(&'a self, content: Element<'a, Message>, side: usize, tone: u32, outline: Option<Color>) -> container::Container<'a, Message> {
        let texture: Element<'a, Message> = if self.neon() {
            iced::widget::canvas(&self.scanlines[side]).width(Length::Fill).height(Length::Fill).into()
        } else {
            iced::widget::canvas(&self.papers[side]).width(Length::Fill).height(Length::Fill).into()
        };
        container(iced::widget::stack![texture, content])
        .clip(true)
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: Some(rgb(tone).into()),
            border: Border {
                color: outline.unwrap_or(Color::TRANSPARENT),
                width: if self.neon() { 1.0 } else { 1.5 },
                radius: self.radius().into(),
            },
            ..Default::default()
        })
    }

    /// The search field, shown in the icon bar.
    fn search_field(&self) -> Element<'_, Message> {
        let t = &self.theme;
        let p = &self.panes[self.active];
        let dim = rgb(t.light_foreground);
        let search_icon = svg(svg::Handle::from_memory(icons::SEARCH.as_bytes()))
            .width(15)
            .height(15)
            .style(move |_, _| svg::Style { color: Some(dim) });
        let (mode, mode_color) = if self.jumping() { ("JUMP", rgb(t.accent)) } else { ("FILTER", dim) };
        let query: Element<'_, Message> = if p.query.is_empty() {
            text("type to filter · ~ to jump").size(12).color(if self.neon() { rgb(t.light_foreground) } else { rgb(0x555555) }).wrapping(iced::widget::text::Wrapping::None).into()
        } else {
            text(format!("{}▏", p.query)).size(12).color(rgb(t.bright)).into()
        };
        let input = row![text(mode).size(10).font(BOLD).color(mode_color), query]
            .spacing(10)
            .align_y(iced::Alignment::Center)
            .width(Length::Fill);
        let active = !p.query.is_empty();
        container(row![search_icon, input].spacing(8).align_y(iced::Alignment::Center))
            .width(Length::Fixed(if self.width < 900.0 { 220.0 } else { 340.0 }))
            .padding([6, 10])
            .style(move |_| container::Style {
                background: Some(if self.neon() { rgb(t.sheet) } else { rgb(0x1a1a1a) }.into()),
                border: Border {
                    color: if active { rgb(t.accent) } else if self.neon() { rgb(t.muted) } else { Color::TRANSPARENT },
                    width: 1.0,
                    radius: if self.neon() { 0.0.into() } else { 6.0.into() },
                },
                ..Default::default()
            })
            .into()
    }

    /// Everything that is busy right now, most important first.
    fn activities(&self) -> Vec<Activity> {
        let mut out = Vec::new();
        if let Some((kind, job, _)) = &self.jobs.running {
            let p = job.progress();
            let verb = match kind {
                _ if self.jobs.undoing => "Undoing",
                OpKind::Copy => "Copying",
                OpKind::Move => "Moving",
                OpKind::Trash => "Trashing",
                OpKind::Delete => "Deleting",
            };
            let count = self.jobs.started + self.jobs.queue.len();
            let of = if count > 1 { format!(" · {} of {count}", self.jobs.started) } else { String::new() };
            let (state, stalled) = if job.conflict().is_some() {
                (" · waiting for you".to_string(), true)
            } else if job.paused() {
                (" · paused".to_string(), true)
            } else if p.current.is_empty() {
                (String::new(), false)
            } else {
                (format!(" · {}", p.current), false)
            };
            out.push(Activity {
                label: format!("{verb}{of}{state}"),
                frac: (p.total > 0).then(|| (p.done as f32 / p.total as f32).min(1.0)),
                stalled,
                on_press: Some(Message::OpenJobs),
            });
        }
        if let Some(d) = self.drag.as_ref().filter(|d| d.active) {
            let what = match d.paths.as_slice() {
                [one] => one.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                many => format!("{} items", many.len()),
            };
            let label = match self.drop_plan(d) {
                Some((kind, to)) => {
                    let verb = if kind == OpKind::Copy { "Copy" } else { "Move" };
                    let to = to.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| to.display().to_string());
                    keys_label(&format!("{verb} {what} to {to} · Ctrl copies · Esc cancels"))
                }
                None => format!("{what}: can't drop here"),
            };
            out.push(Activity { label, frac: None, stalled: false, on_press: None });
        }
        if let Some(name) = &self.opening {
            out.push(Activity { label: format!("Fetching {name}"), frac: None, stalled: false, on_press: None });
        }
        if let Some(what) = self.trash_busy {
            out.push(Activity { label: what.into(), frac: None, stalled: false, on_press: None });
        }
        for l in self.loads.iter().flatten() {
            let name = l.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| self.jumper.display(&l.path));
            // Short reads stay invisible, so local folders don't flicker.
            if l.since.elapsed() < Duration::from_millis(300) {
                continue;
            }
            let slow = l.since.elapsed() >= LOAD_SLOW;
            out.push(Activity {
                label: if slow { format!("{name} not responding · Esc") } else { format!("Reading {name}") },
                frac: None,
                stalled: slow,
                on_press: None,
            });
        }
        let pending = self.sizes.pending();
        if pending > 0 {
            let peak = self.sizes_peak.max(pending);
            out.push(Activity {
                label: format!("Folder sizes · {pending} left"),
                frac: (peak > 1).then(|| 1.0 - pending as f32 / peak as f32),
                stalled: false,
                on_press: None,
            });
        }
        if self.jump_pending {
            out.push(Activity { label: "Indexing folders".into(), frac: None, stalled: false, on_press: None });
        }
        out
    }

    /// A tiny bar: filled to `frac`, or a segment sliding back and forth.
    fn mini_bar(&self, frac: Option<f32>, color: Color) -> Element<'_, Message> {
        let t = &self.theme;
        const W: f32 = 64.0;
        let radius = if self.neon() { 0.0 } else { 2.0 };
        let fill = move |w: f32, c: Color| {
            container(iced::widget::Space::new().width(w).height(4)).style(move |_| container::Style {
                background: Some(c.into()),
                border: Border::default().rounded(radius),
                ..Default::default()
            })
        };
        let (offset, width) = match frac {
            Some(f) => (0.0, (W * f).max(2.0)),
            None => {
                // Triangle wave over 1.6 s.
                let s = START.get().map_or(0.0, |s| s.elapsed().as_secs_f32()) / 0.8;
                let phase = if (s as u32) % 2 == 0 { s.fract() } else { 1.0 - s.fract() };
                (phase * (W - 18.0), 18.0)
            }
        };
        container(row![iced::widget::Space::new().width(offset), fill(width, color)])
            .width(W)
            .height(4)
            .style(move |_| container::Style {
                background: Some(Color { a: 0.35, ..rgb(t.muted) }.into()),
                border: Border::default().rounded(radius),
                ..Default::default()
            })
            .into()
    }

    /// The most used keys as buttons along the bottom edge: what it does, the key below.
    fn keybar(&self) -> Element<'_, Message> {
        let t = &self.theme;
        let none = keyboard::Modifiers::empty();
        let ctrl = keyboard::Modifiers::COMMAND;
        let f = |n: Named| Key::Named(n);
        let c = |s: &str| Key::Character(s.into());
        // The number says which go first when the window is too narrow for all of them.
        let keys: [(u8, &str, &str, Key, keyboard::Modifiers); 13] = [
            (7, "Help", "F1", f(Named::F1), none),
            (6, "Rename", "F2", f(Named::F2), none),
            (4, "View", "F3", f(Named::F3), none),
            (5, "Edit", "F4", f(Named::F4), none),
            (0, "Copy", "F5", f(Named::F5), none),
            (1, "Move", "F6", f(Named::F6), none),
            (2, "New folder", "F7", f(Named::F7), none),
            (3, "Trash", "F8", f(Named::F8), none),
            (10, "Quick view", "Ctrl+Q", c("q"), ctrl),
            (9, "Search", "Ctrl+F", c("f"), ctrl),
            (11, "Commands", "Ctrl+P", c("p"), ctrl),
            (8, "Undo", "Ctrl+Z", c("z"), ctrl),
            (12, "Settings", "F12", f(Named::F12), none),
        ];
        let fit = ((self.width - 12.0) / 86.0).max(1.0) as u8;
        let buttons = keys.into_iter().filter(|k| k.0 < fit).map(|(_, name, combo, key, m)| {
            let label = column![
                text(name).size(12).color(rgb(t.foreground)).wrapping(iced::widget::text::Wrapping::None),
                text(if cfg!(target_os = "macos") { combo.replace("Ctrl", "Cmd") } else { combo.to_string() }).size(10).font(BOLD).color(rgb(t.accent)),
            ]
            .spacing(1)
            .align_x(iced::Alignment::Center);
            button(container(label).center_x(Length::Fill).center_y(Length::Fill).clip(true))
                .padding(0)
                .width(Length::Fill)
                .height(Length::Fill)
                .on_press(Message::KeyBar(key, m))
                .style(move |_, status| button::Style {
                    background: Some(rgb(if matches!(status, button::Status::Hovered | button::Status::Pressed) { t.lighter_background } else { t.sheet_dim }).into()),
                    border: Border::default().rounded(6),
                    ..Default::default()
                })
                .into()
        });
        container(row(buttons).spacing(4))
            .padding(iced::Padding { top: 0.0, bottom: 6.0, left: 6.0, right: 6.0 })
            .height(46)
            .width(Length::Fill)
            .style(move |_| container::Style {
                background: Some(rgb(t.dark_background).into()),
                ..Default::default()
            })
            .into()
    }

    fn statusbar(&self) -> Element<'_, Message> {
        let t = &self.theme;
        let p = &self.panes[self.active];
        let visible = p.visible(self.show_hidden, &self.sizes);
        let dirs = visible.iter().filter(|e| e.is_dir()).count();
        let files = visible.len() - dirs;
        let hidden_marked = p.marked.iter().filter(|m| !visible.iter().any(|e| &e.path == *m)).count();
        let size_of = |e: &Entry| match (e.is_dir() && !e.is_virtual(), self.sizes.get(&e.path)) {
            (false, _) => Some(e.size),
            (true, Some(DirSize::Done(n))) => Some(n),
            _ => None,
        };
        // Total of visible entries, once all folder sizes are known.
        let total = visible.iter().try_fold(0u64, |acc, e| size_of(e).map(|n| acc + n));
        let marked = p.marked.len();
        let marked_size = visible.iter().filter(|e| p.marked.contains(&e.path)).try_fold(0u64, |acc, e| size_of(e).map(|n| acc + n));
        let dim = rgb(t.light_foreground);
        let label = |s: String, c: Color| text(s).size(12).color(c).wrapping(iced::widget::text::Wrapping::None);
        let key = |s: &'static str| text(s).size(10).font(BOLD).color(dim);
        let sep = || {
            container(iced::widget::Space::new().width(1).height(14)).style(move |_| container::Style {
                background: Some(rgb(t.selection).into()),
                ..Default::default()
            })
        };
        let narrow = self.width < 1100.0;

        // Left: what is busy, each with its bar; more than two collapse into "+n".
        let acts = self.activities();
        let mut left = row![].spacing(18).align_y(iced::Alignment::Center);
        for a in acts.iter().take(if narrow { 1 } else { 2 }) {
            let color = if a.stalled { rgb(t.red) } else { rgb(t.accent) };
            let pct = a.frac.map(|f| format!("{:.0}%", f * 100.0)).unwrap_or_default();
            let mut name = a.label.clone();
            if name.chars().count() > 46 {
                name = name.chars().take(45).collect::<String>() + "…";
            }
            let chip = row![self.mini_bar(a.frac, color), label(pct, color), label(name, rgb(t.foreground))]
                .spacing(8)
                .align_y(iced::Alignment::Center);
            left = left.push(match &a.on_press {
                Some(m) => Element::from(mouse_area(chip).on_press(m.clone()).interaction(iced::mouse::Interaction::Pointer)),
                None => chip.into(),
            });
        }
        let shown = if narrow { 1 } else { 2 };
        if acts.len() > shown {
            left = left.push(label(format!("+{}", acts.len() - shown), dim));
        }
        if acts.is_empty() {
            left = left.push(label("Ready".into(), rgb(t.muted)));
        }

        // Right: the entry under the cursor, then the folder, the marks and the drive.
        let mut right = row![].spacing(12).align_y(iced::Alignment::Center);
        if let Some(e) = self.current() {
            let mut info = Vec::new();
            if let Some(n) = size_of(e) {
                info.push(human_size(n));
            }
            if !narrow && let Some(m) = e.modified {
                info.push(human_date(m));
            }
            let mut name = if e.is_dir() { format!("{}/", e.name) } else { e.name.clone() };
            if name.chars().count() > 36 {
                name = name.chars().take(35).collect::<String>() + "…";
            }
            right = right.push(label(name, rgb(t.bright)));
            if !info.is_empty() {
                right = right.push(label(info.join(" · "), dim));
            }
            right = right.push(sep());
        }
        right = right.push(row![label(format!("{dirs}"), rgb(t.foreground)), key("DIRS"), label(format!("{files}"), rgb(t.foreground)), key("FILES")].spacing(5).align_y(iced::Alignment::Center));
        right = right.push(label(match total {
            Some(n) => format!("Σ {}", human_size(n)),
            None => "Σ …".into(),
        }, dim));
        if marked > 0 {
            let size = marked_size.map(|n| format!(" · {}", human_size(n))).unwrap_or_default();
            let hidden = if hidden_marked > 0 { format!(" ({hidden_marked} hidden)") } else { String::new() };
            right = right.push(sep());
            right = right.push(label(format!("● {marked} marked{size}{hidden}"), rgb(t.marked)));
        }
        if let Some((all, free)) = p.space.filter(|(all, _)| *all > 0) {
            let used = 1.0 - free as f32 / all as f32;
            let color = if used > 0.9 { rgb(t.red) } else { dim };
            right = right.push(sep());
            right = right.push(row![self.mini_bar(Some(used), color), label(format!("{} free of {}", human_size(free), human_size(all)), dim)].spacing(8).align_y(iced::Alignment::Center));
        }

        let bar = row![left, iced::widget::Space::new().width(Length::Fill), right].spacing(14).align_y(iced::Alignment::Center);
        container(bar)
            .padding([0, 16])
            .height(34)
            .center_y(34)
            .width(Length::Fill)
            .style(move |_| container::Style {
                background: Some(rgb(t.dark_background).into()),
                ..Default::default()
            })
            .into()
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// Developer aid: with `GUPPY_BENCH=1`, prints the time to the first frame,
/// then the memory use after three idle seconds, and exits.
fn bench_report() {
    static DONE: std::sync::Once = std::sync::Once::new();
    if std::env::var_os("GUPPY_BENCH").is_none() {
        return;
    }
    DONE.call_once(|| {
        let ms = START.get().map_or(0, |s| s.elapsed().as_millis());
        println!("first frame: {ms} ms");
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(3));
            let field = |file: &str, key: &str| {
                std::fs::read_to_string(file).ok()?.lines().find(|l| l.starts_with(key)).map(|l| l[key.len()..].trim().to_string())
            };
            println!("rss: {}", field("/proc/self/status", "VmRSS:").unwrap_or_default());
            println!("pss: {}", field("/proc/self/smaps_rollup", "Pss:").unwrap_or_default());
            println!("private: {}", field("/proc/self/smaps_rollup", "Private_Dirty:").unwrap_or_default());
            std::process::exit(0);
        });
    });
}

fn save_ppm(path: &std::ffi::OsStr, shot: &iced::window::Screenshot) {
    let mut out = format!("P6 {} {} 255\n", shot.size.width, shot.size.height).into_bytes();
    out.extend(shot.rgba.chunks_exact(4).flat_map(|px| [px[0], px[1], px[2]]));
    let _ = std::fs::write(path, out);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cut_middle_keeps_the_end() {
        assert_eq!(cut_middle("short.txt", 20), "short.txt");
        assert_eq!(cut_middle("Flux1-Dev Studio Claude v1.json", 19), "Flux1-Dev S…v1.json");
        assert_eq!(cut_middle("Flux1-Dev Studio Claude v1.json", 19).chars().count(), 19);
    }

    #[test]
    fn palette_finds_commands() {
        let label = |q: &str| COMMANDS[palette_hits(q, &[])[0]].0;
        assert_eq!(label("copy"), "Copy to other pane");
        assert_eq!(label("move tr"), "Move to trash");
        assert_eq!(label("empty"), "Empty trash");
        assert_eq!(label("hid"), "Show / hide hidden files");
        assert_eq!(label("drv r"), "Drives & bookmarks: right pane");
        assert_eq!(label("bookm"), "Bookmark this folder");
        assert!(palette_hits("zzzq", &[]).is_empty());
        assert_eq!(palette_hits("", &[]).len(), COMMANDS.len());
    }
}

/// Outside home the first part is `/` itself, which needs no separator after it.
fn line_starts_root(path: &Path, home: &Path) -> bool {
    !path.starts_with(home) && path.has_root()
}
