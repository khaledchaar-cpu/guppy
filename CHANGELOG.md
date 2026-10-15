# Changelog

## [Unreleased]

### Added
- Key bar along the bottom edge: the most used keys as buttons, each with what it does and
  the key below it. A click acts like pressing the key. When the window is too narrow, the
  less used ones are left out first. Off by default: `[view] keybar = true` or
  Settings → Appearance.
- Four favorite folder buttons at the top of each pane. Click opens the folder in that pane;
  a free slot (+) takes the pane's folder on click. Right-click puts the pane's folder on a
  button, or frees it when you're already there. The row only shows once a favorite is set;
  the first one comes from the command palette (Ctrl+P, "Favorite button for this folder").
  They are kept as `[[favorite]]` in the config file, where the names can be changed too.

## [0.3.0] - 2026-10-14

### Added
- Tabs in each pane: Ctrl+T opens one, Ctrl+W closes it, Ctrl+Tab / Ctrl+Shift+Tab step
  through them, Ctrl+1…9 jumps to one, or click. Each tab keeps its folder, cursor and
  history, and they come back at the next start.
- Space map (Ctrl+U): the other pane shows the current folder as tiles sized by what they
  hold, largest first, each in its own color. Click a folder tile to go into it.
- Text search in files (Ctrl+F): searches everything below the current folder in the
  background. Hits come in while it runs, with the lines around each one; Enter takes you
  to the file. Binary files, files over 20 MB and folders like `.git` or `node_modules` are
  skipped.
- The path line is now made of clickable parts, and each part shows the size of that folder,
  from `~` down to where you are.
- Small animations: new files glow briefly, removed ones fold away, a folder pulses when you
  drop something on it, dialogs and pictures fade in, the space map grows in, the frame of
  the active pane fades in.
- rsync sync profiles are on the idea list.

## [0.2.0] - 2026-10-14

### Added
- Drag and drop inside Guppy: drag files onto the other pane or onto a folder. It moves by
  default, Ctrl copies. A label next to the pointer shows what you are dragging and what
  letting go will do.

### Changed
- The README describes the features in more detail.

## [0.1.0] - 2026-10-14

First public release.

- Two panes, filter as you type, fuzzy jumping to folders (also those zoxide knows), history
- Copy, move, trash and delete as background jobs with progress, a conflict dialog, pause,
  resume and a queue you can reorder; Ctrl+Z undoes the last move, rename or trash
- Folder sizes computed in the background and cached, with a size bar when sorting by size
- Zip and tar archives (also .gz, .bz2, .xz, .zst) open like folders without extra programs;
  7z, rar and iso with bsdtar
- Quick view (Ctrl+Q), viewer (F3), built-in editor (F4)
- Git status per file and the branch in the pane header
- Mark by pattern, permissions and owner, symlinks
- Files on the system clipboard, so copy and paste work with other file managers
- Drives and bookmarks (Alt+F1 / Alt+F2), command palette (Ctrl+P), your own commands on keys
- Settings (F12) with a list of optional helper programs and one-key install for missing ones
- Looks: `paper` (follows the Omarchy theme), `matte-black` and `neon`
- Builds for Linux, Windows and macOS; downloads for all three on the release page

[0.3.0]: https://github.com/khaledchaar-cpu/guppy/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/khaledchaar-cpu/guppy/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/khaledchaar-cpu/guppy/releases/tag/v0.1.0
