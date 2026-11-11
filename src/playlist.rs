//! Playlist logic that doesn't need mpv: which rows are selected, how to
//! turn a reordering into mpv's one-entry moves, where file names share a
//! series prefix, and what a pasted text means as sources.

use std::collections::HashSet;

/// The selected playlist entries, by mpv's entry ids, which stay put when
/// entries move, so the selection follows them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Selection {
    pub ids: HashSet<i64>,
    /// Where a Shift-click range starts.
    anchor: Option<i64>,
}

impl Selection {
    /// A click on row `index` of `order` (entry ids in playlist order): on
    /// its own it selects just that row, with `toggle` (Ctrl, or Command on
    /// macOS) it adds or removes the row, with `extend` (Shift) it selects
    /// everything from the last clicked row.
    pub fn click(&mut self, order: &[i64], index: usize, toggle: bool, extend: bool) {
        let Some(&id) = order.get(index) else {
            return;
        };
        let anchor = self
            .anchor
            .and_then(|anchor| order.iter().position(|&other| other == anchor));
        match (extend, anchor) {
            (true, Some(anchor)) => {
                let range = anchor.min(index)..=anchor.max(index);
                if !toggle {
                    self.ids.clear();
                }
                self.ids.extend(order[range].iter().copied());
                return;
            }
            _ if toggle => {
                if !self.ids.remove(&id) {
                    self.ids.insert(id);
                }
            }
            _ => {
                self.ids.clear();
                self.ids.insert(id);
            }
        }
        self.anchor = Some(id);
    }

    /// Selects row `index` for a context menu, unless it is already part of
    /// the selection, which the menu then acts on as a whole.
    pub fn for_menu(&mut self, order: &[i64], index: usize) {
        if let Some(&id) = order.get(index)
            && !self.ids.contains(&id)
        {
            self.ids.clear();
            self.ids.insert(id);
            self.anchor = Some(id);
        }
    }

    pub fn select_all(&mut self, order: &[i64]) {
        self.ids = order.iter().copied().collect();
    }

    /// Forgets entries that are no longer in the playlist.
    pub fn retain(&mut self, order: &[i64]) {
        let present: HashSet<i64> = order.iter().copied().collect();
        self.ids.retain(|id| present.contains(id));
        if self.anchor.is_some_and(|anchor| !present.contains(&anchor)) {
            self.anchor = None;
        }
    }

    /// Selected rows, in playlist order.
    pub fn indices(&self, order: &[i64]) -> Vec<usize> {
        order
            .iter()
            .enumerate()
            .filter(|(_, id)| self.ids.contains(id))
            .map(|(index, _)| index)
            .collect()
    }
}

/// The order after moving the `moving` entries, keeping their own order,
/// to just before row `target` of `order` (`order.len()` for the end).
pub fn moved(order: &[i64], moving: &HashSet<i64>, target: usize) -> Vec<i64> {
    let target = target.min(order.len());
    let before = order[..target]
        .iter()
        .filter(|id| !moving.contains(id))
        .count();
    let mut rest: Vec<i64> = order
        .iter()
        .copied()
        .filter(|id| !moving.contains(id))
        .collect();
    let group: Vec<i64> = order
        .iter()
        .copied()
        .filter(|id| moving.contains(id))
        .collect();
    rest.splice(before..before, group);
    rest
}

/// The `playlist-move FROM TO` commands that turn `current` into `desired`
/// (the same ids, reordered). mpv puts the entry at FROM before the entry
/// now at TO.
pub fn moves(current: &[i64], desired: &[i64]) -> Vec<(usize, usize)> {
    let mut order = current.to_vec();
    let mut commands = Vec::new();
    for (to, id) in desired.iter().enumerate() {
        let Some(from) = order.iter().position(|other| other == id) else {
            continue;
        };
        if from != to {
            // Everything before `to` is already in place, so `from > to`.
            let entry = order.remove(from);
            order.insert(to, entry);
            commands.push((from, to));
        }
    }
    commands
}

/// The shortest prefix worth folding, as in IINA.
const MIN_PREFIX: usize = 7;

/// What separates the parts of a title Patina makes itself, as a Jellyfin
/// episode's.
const PART: &str = " · ";

/// For each name, the length in bytes of the longest series prefix it
/// shares with another name in the list ("[Group] Show - " in
/// "[Group] Show - 01 [1080p].mkv"), or 0. The prefix ends after a
/// separator, never inside a word or a number. Copies of the same name
/// don't count: they share everything.
pub fn series_prefixes(names: &[&str]) -> Vec<usize> {
    let common = |a: &str, b: &str| -> usize {
        let shared: usize = a
            .char_indices()
            .zip(b.chars())
            .take_while(|((_, x), y)| x == y)
            .map(|((_, x), _)| x.len_utf8())
            .sum();
        // A title in parts, as Jellyfin's episodes are ("Show · S1 E2 ·
        // Name"), folds whole parts only: cut inside "S1", episodes of two
        // seasons would read alike.
        if let Some(part) = a[..shared].rfind(PART) {
            return part + PART.len();
        }
        // Back off to the end of the last separator.
        a[..shared]
            .char_indices()
            .rev()
            .find(|(_, c)| !c.is_alphanumeric())
            .map_or(0, |(index, c)| index + c.len_utf8())
    };
    // In sorted order, the longest prefix a name shares with any other is
    // the one it shares with the nearest different name on either side.
    let mut distinct: Vec<&str> = names.to_vec();
    distinct.sort_unstable();
    distinct.dedup();
    names
        .iter()
        .map(|name| {
            let Ok(at) = distinct.binary_search(name) else {
                return 0;
            };
            let before = at.checked_sub(1).map_or(0, |i| common(name, distinct[i]));
            let after = distinct.get(at + 1).map_or(0, |other| common(name, other));
            let prefix = before.max(after);
            // Something has to be left to show.
            if prefix >= MIN_PREFIX && prefix < name.len() {
                prefix
            } else {
                0
            }
        })
        .collect()
}

/// The text of an M3U playlist of (title, path) pairs, for Save Playlist.
/// A line ends where a title or path breaks: a title's line breaks become
/// spaces, and a path with one is left out, since it would be read back as
/// other entries.
pub fn m3u(entries: &[(String, String)]) -> String {
    let mut text = String::from("#EXTM3U\n");
    for (title, path) in entries {
        if path.contains(['\r', '\n']) {
            log::warn!("leaving {path:?} out of the playlist: an M3U can't hold its line break");
            continue;
        }
        let title = title.replace(['\r', '\n'], " ");
        text.push_str(&format!("#EXTINF:-1,{title}\n{path}\n"));
    }
    text
}

/// The sources in pasted text: one per line, as paths, file:// URIs or
/// URLs; blank lines and anything else are skipped.
pub fn pasted_sources(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| {
            if let Some(path) = file_uri_path(line) {
                Some(path)
            } else {
                let source = crate::util::is_url(line) || std::path::Path::new(line).is_absolute();
                source.then(|| line.to_owned())
            }
        })
        .collect()
}

/// The local path of a file:// URI, percent-decoded.
pub fn file_uri_path(uri: &str) -> Option<String> {
    uri_path(uri, cfg!(windows))
}

/// [`file_uri_path`], with `windows` saying whether paths are Windows ones,
/// so each platform's are tested everywhere.
fn uri_path(uri: &str, windows: bool) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    // file:///path, or file://localhost/path.
    let path = rest.strip_prefix("localhost").unwrap_or(rest);
    let mut bytes = Vec::with_capacity(path.len());
    let mut iter = path.bytes();
    while let Some(byte) = iter.next() {
        if byte == b'%' {
            let hex: Vec<u8> = iter.by_ref().take(2).collect();
            let value = std::str::from_utf8(&hex)
                .ok()
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())?;
            bytes.push(value);
        } else {
            bytes.push(byte);
        }
    }
    let path = String::from_utf8(bytes).ok()?;
    // file:///C:/Videos on Windows: a slash, an ASCII drive letter and a
    // colon. Looking at the bytes keeps the slice on a character boundary,
    // whatever the URI decoded to.
    if windows
        && let [b'/', drive, b':', ..] = path.as_bytes()
        && drive.is_ascii_alphabetic()
    {
        return Some(path[1..].replace('/', "\\"));
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORDER: [i64; 5] = [10, 11, 12, 13, 14];

    fn selected(selection: &Selection) -> Vec<usize> {
        selection.indices(&ORDER)
    }

    #[test]
    fn clicks_select_toggle_and_extend() {
        let mut selection = Selection::default();
        selection.click(&ORDER, 1, false, false);
        assert_eq!(selected(&selection), [1]);
        selection.click(&ORDER, 3, false, true);
        assert_eq!(selected(&selection), [1, 2, 3]);
        selection.click(&ORDER, 2, true, false);
        assert_eq!(selected(&selection), [1, 3]);
        // The toggled row is the new anchor.
        selection.click(&ORDER, 4, false, true);
        assert_eq!(selected(&selection), [2, 3, 4]);
        selection.click(&ORDER, 0, false, false);
        assert_eq!(selected(&selection), [0]);
    }

    #[test]
    fn a_menu_acts_on_the_selection_it_was_opened_on() {
        let mut selection = Selection::default();
        selection.click(&ORDER, 1, false, false);
        selection.click(&ORDER, 2, true, false);
        selection.for_menu(&ORDER, 2);
        assert_eq!(selected(&selection), [1, 2]);
        selection.for_menu(&ORDER, 4);
        assert_eq!(selected(&selection), [4]);
    }

    #[test]
    fn selection_follows_entries_and_forgets_removed_ones() {
        let mut selection = Selection::default();
        selection.click(&ORDER, 1, false, false);
        selection.click(&ORDER, 3, true, false);
        let reordered = [13, 10, 11, 14];
        selection.retain(&reordered);
        assert_eq!(selection.indices(&reordered), [0, 2]);
    }

    #[test]
    fn moves_rebuild_the_desired_order() {
        let apply = |order: &[i64], commands: &[(usize, usize)]| {
            let mut order = order.to_vec();
            for &(from, to) in commands {
                // mpv's playlist-move: FROM goes before the entry at TO.
                let entry = order.remove(from);
                let to = if from < to { to - 1 } else { to };
                order.insert(to, entry);
            }
            order
        };
        let moving: HashSet<i64> = [11, 13].into();
        for target in 0..=ORDER.len() {
            let desired = moved(&ORDER, &moving, target);
            assert_eq!(apply(&ORDER, &moves(&ORDER, &desired)), desired);
        }
        assert_eq!(moved(&ORDER, &moving, 0), [11, 13, 10, 12, 14]);
        assert_eq!(moved(&ORDER, &moving, 5), [10, 12, 14, 11, 13]);
        // Play next: after the current entry, 12.
        assert_eq!(moved(&ORDER, &[14].into(), 3), [10, 11, 12, 14, 13]);
    }

    #[test]
    fn series_prefixes_stop_at_separators() {
        let names = [
            "[Grp] Show - 01 [1080p].mkv",
            "[Grp] Show - 02 [1080p].mkv",
            "[Grp] Show - 10 [1080p].mkv",
            "Other.mkv",
            "Movie 2019.mkv",
            "Movie 2020.mkv",
        ];
        let prefixes = series_prefixes(&names);
        assert_eq!(&names[0][..prefixes[0]], "[Grp] Show - ");
        assert_eq!(&names[2][..prefixes[2]], "[Grp] Show - ");
        assert_eq!(prefixes[3], 0);
        // "Movie " is too short to be worth folding.
        assert_eq!(prefixes[4], 0);
        assert_eq!(
            series_prefixes(&["Ünïcödé — 1.mkv", "Ünïcödé — 2.mkv"])[0],
            "Ünïcödé — ".len()
        );
        // Not only next to each other, and not between copies of one name.
        let names = [
            "Show - 01.mkv",
            "Bonus.mkv",
            "Show - 02.mkv",
            "Show - 02.mkv",
        ];
        let show = "Show - ".len();
        assert_eq!(series_prefixes(&names), [show, 0, show, show]);
        assert_eq!(series_prefixes(&["Same name.mkv", "Same name.mkv"]), [0, 0]);
        // A title in parts keeps the season in what shows.
        let episodes = [
            "Lantern Street · S1 E1 · Opening Night",
            "Lantern Street · S1 E2 · Night Market",
            "Lantern Street · S2 E1 · Back Again",
            "Lantern Street · S2 E2 · Last Stop",
        ];
        for (name, prefix) in episodes.iter().zip(series_prefixes(&episodes)) {
            assert_eq!(&name[..prefix], "Lantern Street · ", "{name}");
        }
    }

    #[test]
    fn pasted_text_becomes_sources() {
        // Absolute on every platform.
        let path = std::env::temp_dir().join("a b.mkv");
        let path = path.to_string_lossy();
        let text =
            format!("{path}\n\nfile:///media/c%20d.mkv\nhttps://example.com/v\nnot a path\n");
        assert_eq!(
            pasted_sources(&text),
            [&*path, "/media/c d.mkv", "https://example.com/v"]
        );
    }

    #[test]
    fn windows_file_uris_become_drive_paths_and_nothing_else_does() {
        assert_eq!(
            uri_path("file:///C:/Videos/a%20b.mkv", true).as_deref(),
            Some("C:\\Videos\\a b.mkv")
        );
        assert_eq!(
            uri_path("file://localhost/d:/x.mkv", true).as_deref(),
            Some("d:\\x.mkv")
        );
        assert_eq!(
            uri_path("file:///C:/x.mkv", false).as_deref(),
            Some("/C:/x.mkv")
        );
        // A first character that takes more than one byte, before a colon.
        assert_eq!(
            uri_path("file://\u{e9}:x", true).as_deref(),
            Some("\u{e9}:x")
        );
        assert_eq!(
            uri_path("file://%C3%A9:C:/x", true).as_deref(),
            Some("\u{e9}:C:/x")
        );
        // Two letters before the colon are no drive.
        assert_eq!(uri_path("file://ab:x", true).as_deref(), Some("ab:x"));
    }

    #[test]
    fn a_saved_playlist_has_a_line_for_each_title_and_path() {
        let entries = [
            ("Song".to_owned(), "/music/song.flac".to_owned()),
            // A server's title with line breaks, which would have made
            // lines of their own and been read back as entries.
            (
                "Episode\n/etc/passwd\r\nTwo".to_owned(),
                "https://a/v.mp4".to_owned(),
            ),
            ("Odd".to_owned(), "/videos/a\n/etc/passwd".to_owned()),
        ];
        assert_eq!(
            m3u(&entries),
            "#EXTM3U\n\
             #EXTINF:-1,Song\n/music/song.flac\n\
             #EXTINF:-1,Episode /etc/passwd  Two\nhttps://a/v.mp4\n"
        );
    }
}
