//! User settings from `~/.config/guppy/config.toml`.
//!
//! Every key is optional; missing keys keep their default. On first start a
//! commented template is written so the options are easy to discover.

use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub start: Start,
    pub view: View,
    pub confirm: Confirm,
    pub network: Network,
    pub edit: Edit,
    /// Folders the user keeps at hand, in their order (`[[bookmark]]`).
    #[serde(rename = "bookmark")]
    pub bookmarks: Vec<Bookmark>,
    /// Shell commands on a key (`[[action]]`), see [`crate::actions`].
    #[serde(rename = "action")]
    pub actions: Vec<Action>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub name: String,
    /// E.g. `"Ctrl+Alt+T"` or `"F9"`; empty = only in the command palette.
    #[serde(default)]
    pub key: String,
    /// Run by `sh -c` in the active pane's folder; placeholders in [`crate::actions::expand`].
    pub command: String,
    /// Run inside a terminal window (for interactive or text-output commands).
    #[serde(default)]
    pub terminal: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bookmark {
    pub name: String,
    /// As written: absolute or `~/…`; see [`Config::resolve`].
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Edit {
    /// Editor command for F4; empty = automatic.
    pub command: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Start {
    /// Left pane; empty = the directory Guppy was started in.
    pub left: String,
    /// Right pane; empty = the directory Guppy was started in.
    pub right: String,
    /// Show the key help when Guppy starts.
    pub help: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortBy {
    Name,
    Size,
    Date,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct View {
    pub show_hidden: bool,
    pub sort: SortBy,
    /// Compute folder sizes automatically; otherwise only on Ctrl+R.
    pub dir_sizes: bool,
    pub look: Look,
    /// Row of the most used keys as buttons along the bottom edge.
    pub keybar: bool,
}

/// Overall appearance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Look {
    /// Warm paper sheets in the Omarchy theme's colors.
    #[default]
    Paper,
    /// Paper sheets in Omarchy's "Matte Black", whatever theme is active.
    #[serde(rename = "matte-black")]
    MatteBlack,
    /// Cyberpunk terminal: black glass, neon lines, scanlines.
    Neon,
}

impl Look {
    pub fn name(self) -> &'static str {
        match self {
            Look::Paper => "paper",
            Look::MatteBlack => "matte-black",
            Look::Neon => "neon",
        }
    }

    /// The next look forward (`dir` > 0) or back, round in a circle.
    pub fn step(self, dir: i32) -> Self {
        const ALL: [Look; 3] = [Look::Paper, Look::MatteBlack, Look::Neon];
        let i = ALL.iter().position(|&l| l == self).unwrap_or(0) as i32;
        ALL[(i + dir.signum()).rem_euclid(ALL.len() as i32) as usize]
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Confirm {
    pub copy: bool,
    #[serde(rename = "move")]
    pub move_: bool,
    pub trash: bool,
    pub delete: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Network {
    /// Parallel accesses per folder-size computation on network drives.
    pub threads: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self { start: Start::default(), view: View::default(), confirm: Confirm::default(), network: Network::default(), edit: Edit::default(), bookmarks: Vec::new(), actions: Vec::new() }
    }
}

impl Default for Start {
    fn default() -> Self {
        Self { left: String::new(), right: "~".into(), help: true }
    }
}

impl Default for View {
    fn default() -> Self {
        Self { show_hidden: false, sort: SortBy::Name, dir_sizes: true, look: Look::Paper, keybar: false }
    }
}

impl Default for Confirm {
    fn default() -> Self {
        Self { copy: true, move_: true, trash: true, delete: true }
    }
}

impl Default for Network {
    fn default() -> Self {
        Self { threads: 4 }
    }
}

const TEMPLATE: &str = r#"# Guppy settings. Every key is optional; delete a line to use the default.
# Changes take effect immediately, except [start] and [network] (used at startup).

[start]
left = ""          # left pane: path, "~/…", or "" for the directory Guppy was started in
right = "~"        # right pane
help = true        # show the key help at startup

[view]
show_hidden = false
sort = "name"      # "name", "size" or "date"
dir_sizes = true   # compute folder sizes automatically (otherwise only on Ctrl+R)
look = "paper"     # "paper" (Omarchy colors), "matte-black" or "neon" (cyberpunk terminal)
keybar = false     # the most used keys as buttons along the bottom edge

[confirm]
# Ask before these operations. Progress is shown either way.
copy = true
move = true
trash = true
delete = true      # permanent delete; turning this off is not recommended

[network]
threads = 4        # parallel accesses per folder-size computation on network drives

[edit]
command = ""       # editor for F4, e.g. "code" or "kitty -e nvim"; "" = automatic

# Bookmarks, shown under the drives (Alt+F1 / Alt+F2); Ctrl+D adds the current folder.
# [[bookmark]]
# name = "Projects"
# path = "~/Work/projects"

# Your own commands, also in the command palette (Ctrl+P). They run in the active
# pane's folder. %f = file under the cursor, %F = marked files (or %f),
# %d = this folder, %D = the other pane's folder; all quoted for the shell.
# [[action]]
# name = "Open terminal here"
# key = "Ctrl+Alt+T"
# command = "xdg-terminal-exec"
# [[action]]
# name = "Pack as tar.gz"
# command = "tar czf archive.tar.gz %F"
"#;

impl SortBy {
    pub fn name(self) -> &'static str {
        match self {
            SortBy::Name => "name",
            SortBy::Size => "size",
            SortBy::Date => "date",
        }
    }
}

impl Config {
    /// The file contents for these settings, in the commented template layout.
    pub fn to_toml(&self) -> String {
        let q = |s: &str| toml::Value::String(s.to_string()).to_string();
        let mut out = String::new();
        // Walk the template and substitute each value, keeping all comments.
        let mut section = "";
        for line in TEMPLATE.lines() {
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                section = match name {
                    "start" => "start",
                    "view" => "view",
                    "confirm" => "confirm",
                    "edit" => "edit",
                    _ => "network",
                };
                out.push_str(line);
                out.push('\n');
                continue;
            }
            let Some((key, rest)) = line.split_once(" = ") else {
                out.push_str(line);
                out.push('\n');
                continue;
            };
            let key = key.trim();
            let value = match (section, key) {
                ("start", "left") => q(&self.start.left),
                ("start", "right") => q(&self.start.right),
                ("start", "help") => self.start.help.to_string(),
                ("view", "show_hidden") => self.view.show_hidden.to_string(),
                ("view", "sort") => q(self.view.sort.name()),
                ("view", "dir_sizes") => self.view.dir_sizes.to_string(),
                ("view", "look") => q(self.view.look.name()),
                ("view", "keybar") => self.view.keybar.to_string(),
                ("confirm", "copy") => self.confirm.copy.to_string(),
                ("confirm", "move") => self.confirm.move_.to_string(),
                ("confirm", "trash") => self.confirm.trash.to_string(),
                ("confirm", "delete") => self.confirm.delete.to_string(),
                ("network", "threads") => self.network.threads.to_string(),
                ("edit", "command") => q(&self.edit.command),
                _ => rest.split('#').next().unwrap_or("").trim().to_string(),
            };
            let comment = rest.find('#').map(|i| &rest[i..]);
            let head = format!("{key} = {value}");
            match comment {
                Some(c) => out.push_str(&format!("{head:<18} {c}\n")),
                None => {
                    out.push_str(&head);
                    out.push('\n');
                }
            }
        }
        for b in &self.bookmarks {
            out.push_str(&format!("\n[[bookmark]]\nname = {}\npath = {}\n", q(&b.name), q(&b.path)));
        }
        for a in &self.actions {
            out.push_str(&format!("\n[[action]]\nname = {}\nkey = {}\ncommand = {}\n", q(&a.name), q(&a.key), q(&a.command)));
            if a.terminal {
                out.push_str("terminal = true\n");
            }
        }
        out
    }

    /// Writes the settings (atomically via a temporary file).
    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = Self::path() else { return Ok(()) };
        std::fs::create_dir_all(path.parent().unwrap_or(Path::new("/")))?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, self.to_toml())?;
        std::fs::rename(tmp, path)
    }

    pub fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("guppy/config.toml"))
    }

    /// Loads the settings; writes the template if there is no file yet.
    /// A broken file yields the defaults plus an error message for the user.
    pub fn load() -> (Self, Option<String>) {
        let Some(path) = Self::path() else { return (Self::default(), None) };
        match std::fs::read_to_string(&path) {
            Ok(s) => match toml::from_str(&s) {
                Ok(c) => (c, None),
                Err(e) => (Self::default(), Some(format!("{}: {}", path.display(), e.message()))),
            },
            Err(_) => {
                let _ = std::fs::create_dir_all(path.parent().unwrap_or(Path::new("/")));
                let _ = std::fs::write(&path, TEMPLATE);
                (Self::default(), None)
            }
        }
    }

    /// Resolves a start path; `~` means home, empty means `cwd`.
    pub fn resolve(p: &str, cwd: &Path) -> PathBuf {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        match p.trim() {
            "" => cwd.to_path_buf(),
            "~" => home,
            p => match p.strip_prefix("~/") {
                Some(rest) => home.join(rest),
                None => PathBuf::from(p),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_parses_to_defaults() {
        let c: Config = toml::from_str(TEMPLATE).unwrap();
        assert!(c.confirm.delete && c.view.dir_sizes);
        assert_eq!(c.view.sort, SortBy::Name);
        assert_eq!(c.network.threads, 4);
        assert!(toml::from_str::<Config>("[view]\nbogus = 1").is_err());
        let mut b = Config::default();
        b.bookmarks = vec![Bookmark { name: "Pro \"jekte\"".into(), path: "~/Work".into() }, Bookmark { name: "Root".into(), path: "/".into() }];
        b.actions = vec![Action { name: "Pack".into(), key: "F9".into(), command: "tar czf a.tgz %F".into(), terminal: true }];
        assert_eq!(toml::from_str::<Config>(&b.to_toml()).unwrap(), b);
        // Round trip through the commented layout.
        let mut c = Config::default();
        c.start.left = "~/a \"b\"".into();
        c.view.sort = SortBy::Date;
        c.confirm.trash = false;
        c.network.threads = 7;
        c.edit.command = "kitty -e nvim".into();
        assert_eq!(toml::from_str::<Config>(&c.to_toml()).unwrap(), c);
    }
}
