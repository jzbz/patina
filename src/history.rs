//! The playback history window, after IINA's: every file played, newest
//! first, grouped by day or by folder, searchable, with how far each one was
//! watched. Files can be played again, shown in the file manager, or
//! removed.

use crate::app::App;
use crate::player::state::file_name;
use crate::settings::HistoryEntry;
use crate::util::{self, format_time};
use crate::{HistoryRow, HistoryWindow};
use jiff::civil::Date;
use jiff::{Timestamp, Zoned};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::{Rc, Weak};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    Date,
    Folder,
}

pub struct HistoryController {
    app: Weak<App>,
    window: HistoryWindow,
    rows: Rc<VecModel<HistoryRow>>,
    /// The file each row stands for; None for headings.
    paths: RefCell<Vec<Option<String>>>,
    query: RefCell<String>,
    group: Cell<Group>,
    selected: RefCell<Option<String>>,
    /// Local files in the history that no longer exist.
    missing: RefCell<HashSet<String>>,
    checking: Cell<bool>,
}

impl HistoryController {
    pub fn new(app: &Rc<App>) -> Result<Rc<Self>, String> {
        let window = HistoryWindow::new().map_err(|error| error.to_string())?;
        let rows = Rc::new(VecModel::default());
        window.set_rows(ModelRc::from(rows.clone()));
        let controller = Rc::new(Self {
            app: Rc::downgrade(app),
            window,
            rows,
            paths: RefCell::new(Vec::new()),
            query: RefCell::new(String::new()),
            group: Cell::new(Group::Date),
            selected: RefCell::new(None),
            missing: RefCell::new(HashSet::new()),
            checking: Cell::new(false),
        });
        controller.wire();
        Ok(controller)
    }

    pub fn show(self: &Rc<Self>) {
        self.refresh();
        self.check_missing();
        if let Err(error) = self.window.show() {
            log::warn!("could not show the playback history: {error}");
        }
    }

    fn app(&self) -> Rc<App> {
        self.app
            .upgrade()
            .expect("the app outlives its history window")
    }

    fn wire(self: &Rc<Self>) {
        macro_rules! on {
            ($method:ident, |$this:ident $(, $arg:ident)*| $body:expr) => {{
                let weak = Rc::downgrade(self);
                self.window.$method(move |$($arg),*| {
                    if let Some($this) = weak.upgrade() {
                        $body
                    }
                });
            }};
        }
        on!(on_search, |this, text| {
            *this.query.borrow_mut() = text.to_string();
            this.refresh();
        });
        on!(on_set_group, |this, index| {
            this.group.set(if index == 1 {
                Group::Folder
            } else {
                Group::Date
            });
            this.window.set_group(index);
            this.refresh();
        });
        on!(on_select, |this, index| this.select(Some(index as usize)));
        on!(on_move_selection, |this, delta| this.move_selection(delta));
        on!(on_play, |this, index, new_window| {
            if let Some(path) = this.path_at(index) {
                this.app().play_from_history(path, new_window);
            }
        });
        on!(on_reveal, |this, index| {
            if let Some(path) = this.path_at(index) {
                crate::platform::reveal(std::path::Path::new(&path));
            }
        });
        on!(on_remove, |this, index| this.remove(index));
        on!(on_clear, |this| this.app().clear_history());
    }

    fn path_at(&self, index: i32) -> Option<String> {
        let index = usize::try_from(index).ok()?;
        self.paths.borrow().get(index).cloned().flatten()
    }

    /// Rebuilds the list from the history, keeping the selection.
    pub fn refresh(&self) {
        let app = self.app();
        let entries = app.history_entries();
        let query = self.query.borrow().clone();
        let built = build(
            &entries,
            &query,
            self.group.get(),
            &Zoned::now(),
            &self.missing.borrow(),
        );
        let matched = built.iter().filter(|(_, path)| path.is_some()).count();
        let (rows, paths): (Vec<_>, Vec<_>) = built.into_iter().unzip();
        self.rows.set_vec(rows);
        *self.paths.borrow_mut() = paths;

        let files = |count: usize| {
            if count == 1 {
                "1 file".to_owned()
            } else {
                format!("{count} files")
            }
        };
        let summary = if entries.is_empty() {
            String::new()
        } else if matched == entries.len() {
            files(entries.len())
        } else {
            format!("{matched} of {}", files(entries.len()))
        };
        self.window.set_summary(summary.into());
        let empty = if !entries.is_empty() {
            format!("Nothing matches “{}”.", query.trim())
        } else if app.prefs().record_history {
            "Files you play appear here.".to_owned()
        } else {
            "Playback history is turned off in Preferences.".to_owned()
        };
        self.window.set_empty_text(empty.into());

        let selected = self.selected.borrow().clone();
        let index = selected.and_then(|selected| {
            self.paths
                .borrow()
                .iter()
                .position(|path| path.as_deref() == Some(selected.as_str()))
        });
        self.select(index);
    }

    fn select(&self, index: Option<usize>) {
        let path = index.and_then(|index| self.paths.borrow().get(index).cloned().flatten());
        let index = if path.is_some() { index } else { None };
        self.window
            .set_selected(index.map_or(-1, |index| index as i32));
        let can_reveal = path
            .as_ref()
            .is_some_and(|path| !util::is_url(path) && !self.missing.borrow().contains(path));
        self.window.set_can_reveal(can_reveal);
        *self.selected.borrow_mut() = path;
    }

    /// Moves the selection to the next file up or down, past headings.
    fn move_selection(&self, delta: i32) {
        let paths = self.paths.borrow();
        let current = usize::try_from(self.window.get_selected()).ok();
        let mut candidates: Box<dyn Iterator<Item = usize>> = match (current, delta > 0) {
            (Some(current), true) => Box::new(current + 1..paths.len()),
            (Some(current), false) => Box::new((0..current).rev()),
            (None, _) => Box::new(0..paths.len()),
        };
        let next = candidates.find(|index| paths[*index].is_some());
        drop(paths);
        if next.is_some() {
            self.select(next);
        }
    }

    fn remove(&self, index: i32) {
        let Some(path) = self.path_at(index) else {
            return;
        };
        // Keep a selection where the removed file was.
        {
            let paths = self.paths.borrow();
            let index = index as usize;
            let next = paths[index + 1..]
                .iter()
                .flatten()
                .next()
                .or_else(|| paths[..index].iter().rev().flatten().next())
                .cloned();
            *self.selected.borrow_mut() = next;
        }
        self.app().remove_from_history(&path);
    }

    /// Finds local files that no longer exist, away from the UI thread
    /// (a file on an unreachable network share can take long to check).
    fn check_missing(self: &Rc<Self>) {
        if self.checking.replace(true) {
            return;
        }
        let paths: Vec<String> = self
            .app()
            .history_entries()
            .into_iter()
            .map(|entry| entry.path)
            .filter(|path| !util::is_url(path))
            .collect();
        let (sender, receiver) = async_channel::bounded(1);
        let spawned = std::thread::Builder::new()
            .name("history-check".into())
            .spawn(move || {
                let missing: HashSet<String> = paths
                    .into_iter()
                    .filter(|path| !std::path::Path::new(path).exists())
                    .collect();
                let _ = sender.send_blocking(missing);
            });
        if spawned.is_err() {
            self.checking.set(false);
            return;
        }
        let weak = Rc::downgrade(self);
        let received = slint::spawn_local(async move {
            if let Ok(missing) = receiver.recv().await
                && let Some(this) = weak.upgrade()
            {
                this.checking.set(false);
                *this.missing.borrow_mut() = missing;
                this.refresh();
            }
        });
        if let Err(error) = received {
            log::warn!("could not check history files: {error}");
        }
    }
}

/// The rows to show, each with the file it stands for (None for headings).
fn build(
    entries: &[HistoryEntry],
    query: &str,
    group: Group,
    now: &Zoned,
    missing: &HashSet<String>,
) -> Vec<(HistoryRow, Option<String>)> {
    let query = query.trim().to_lowercase();
    let mut matching: Vec<&HistoryEntry> = entries
        .iter()
        .filter(|entry| {
            query.is_empty()
                || entry.title.to_lowercase().contains(&query)
                || entry.path.to_lowercase().contains(&query)
        })
        .collect();
    matching.sort_by_key(|entry| std::cmp::Reverse(entry.played_at));

    let today = now.date();
    let played = |entry: &HistoryEntry| {
        Timestamp::from_second(i64::try_from(entry.played_at).unwrap_or(0))
            .unwrap_or(Timestamp::UNIX_EPOCH)
            .to_zoned(now.time_zone().clone())
    };
    // Groups in order of their most recent file.
    let mut groups: Vec<(String, Vec<&HistoryEntry>)> = Vec::new();
    for entry in matching {
        let key = match group {
            Group::Date => day_label(played(entry).date(), today),
            Group::Folder => util::parent_directory(&entry.path),
        };
        match groups.iter_mut().find(|(existing, _)| *existing == key) {
            Some((_, members)) => members.push(entry),
            None => groups.push((key, vec![entry])),
        }
    }

    let mut rows = Vec::new();
    for (heading, members) in groups {
        rows.push((
            HistoryRow {
                header: true,
                title: heading.into(),
                ..Default::default()
            },
            None,
        ));
        for entry in members {
            let url = util::is_url(&entry.path);
            let when = played(entry);
            let time = match group {
                Group::Date => when.strftime("%H:%M").to_string(),
                Group::Folder if when.date() == today => when.strftime("%H:%M").to_string(),
                Group::Folder => when.strftime("%b %-d, %Y").to_string(),
            };
            let detail = match group {
                _ if url => entry.path.clone(),
                Group::Date => util::parent_directory(&entry.path),
                Group::Folder => file_name(&entry.path),
            };
            let (progress, progress_text) = if entry.duration > 0.0 {
                let fraction = (entry.position / entry.duration).clamp(0.0, 1.0);
                let text = if entry.position >= 1.0 {
                    format!(
                        "{} / {}",
                        format_time(entry.position),
                        format_time(entry.duration)
                    )
                } else {
                    format_time(entry.duration)
                };
                (fraction as f32, text)
            } else if entry.position >= 1.0 {
                (-1.0, format_time(entry.position))
            } else {
                (-1.0, String::new())
            };
            let title = if entry.title.is_empty() {
                file_name(&entry.path)
            } else {
                entry.title.clone()
            };
            rows.push((
                HistoryRow {
                    header: false,
                    title: title.into(),
                    detail: detail.into(),
                    time: time.into(),
                    progress,
                    progress_text: SharedString::from(progress_text),
                    url,
                    missing: missing.contains(&entry.path),
                },
                Some(entry.path.clone()),
            ));
        }
    }
    rows
}

/// "Today", "Yesterday", a weekday within the last week, or a date.
fn day_label(date: Date, today: Date) -> String {
    let days = date.until(today).map_or(i32::MAX, |span| span.get_days());
    match days {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        2..=6 => date.strftime("%A").to_string(),
        _ => date.strftime("%B %-d, %Y").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, played_at: &str, position: f64, duration: f64) -> HistoryEntry {
        let played_at: Zoned = format!("{played_at}[UTC]").parse().unwrap();
        HistoryEntry {
            path: path.into(),
            title: file_name(path),
            played_at: played_at.timestamp().as_second() as u64,
            duration,
            position,
        }
    }

    fn titles(rows: &[(HistoryRow, Option<String>)]) -> Vec<String> {
        rows.iter()
            .map(|(row, path)| {
                if path.is_some() {
                    format!("  {}", row.title)
                } else {
                    row.title.to_string()
                }
            })
            .collect()
    }

    #[test]
    fn rows_are_grouped_by_day_and_folder() {
        let now: Zoned = "2026-10-26T20:00:00[UTC]".parse().unwrap();
        let entries = [
            entry("/media/show/02.mkv", "2026-10-26T19:00:00", 600.0, 1200.0),
            entry("/media/film.mp4", "2026-10-26T09:30:00", 0.0, 0.0),
            entry("https://example.com/live", "2026-10-25T22:00:00", 0.0, 0.0),
            entry("/media/show/01.mkv", "2026-10-22T12:00:00", 1200.0, 1200.0),
            entry("/media/old.avi", "2026-09-01T12:00:00", 0.0, 0.0),
        ];
        let none = HashSet::new();
        let rows = build(&entries, "", Group::Date, &now, &none);
        assert_eq!(
            titles(&rows),
            [
                "Today",
                "  02.mkv",
                "  film.mp4",
                "Yesterday",
                "  https://example.com/live",
                "Thursday",
                "  01.mkv",
                "September 1, 2026",
                "  old.avi",
            ]
        );
        let (first, _) = &rows[1];
        assert_eq!(first.time, "19:00");
        assert_eq!(first.progress, 0.5);
        assert_eq!(first.progress_text, "10:00 / 20:00");

        let rows = build(&entries, "", Group::Folder, &now, &none);
        assert_eq!(
            titles(&rows),
            [
                "/media/show",
                "  02.mkv",
                "  01.mkv",
                "/media",
                "  film.mp4",
                "  old.avi",
                "example.com",
                "  https://example.com/live",
            ]
        );
        assert_eq!(rows[2].0.time, "Oct 22, 2026");
    }

    #[test]
    fn search_matches_titles_and_paths() {
        let now: Zoned = "2026-10-26T20:00:00[UTC]".parse().unwrap();
        let entries = [
            entry("/media/show/02.mkv", "2026-10-26T19:00:00", 0.0, 0.0),
            entry("/media/film.mp4", "2026-10-26T09:30:00", 0.0, 0.0),
        ];
        let missing = HashSet::from(["/media/film.mp4".to_owned()]);
        let rows = build(&entries, " SHOW ", Group::Date, &now, &missing);
        assert_eq!(titles(&rows), ["Today", "  02.mkv"]);
        let rows = build(&entries, "film", Group::Date, &now, &missing);
        assert!(rows[1].0.missing);
    }
}
