//! Which of two yt-dlps is newer: the one a macOS or Windows package carries,
//! or one the user installed themselves (Homebrew, winget, pip). The bundled
//! copy is only as new as the Patina release, and YouTube's changes are fixed
//! in new yt-dlp releases, so a user who keeps their own up to date should get
//! theirs without having to say so; and one whose own is older than the
//! package's should get the package's.
//!
//! Each is asked with `--version`, which touches no network, and the answer is
//! kept until the file changes: by path, size and modification time, in memory
//! and in a small file beside the history, so a copy is asked once, not once a
//! run. yt-dlp takes a second or more to start, and many more the first time a
//! machine's malware scan sees it, so the asking is done on a thread of its own
//! and never by the interface thread: until both have answered, the package's
//! copy is the one used. Whenever the choice
//! handed out changes, as when the user's turns out newer once both have
//! answered, or theirs is removed or replaced by an older one, `settled` tells
//! the app, which hands the new choice to mpv's hook, the YouTube section and
//! the Downloads window (`App::ytdl_choice_changed`). Everything asks
//! `downloads::find_ytdl`, which asks here, so all of them agree.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

/// How long a copy has to say its version; one that takes longer is passed
/// over for the run, which for the package's copy means the user's wins
/// even when it is the older. Starting is what takes the time: yt-dlp's
/// single-file builds took about 10 s for every start on a test Mac, while
/// XProtect scanned what each one unpacked, and the folder builds the
/// packages carry, which unpack nothing, can still take as long the first
/// time macOS or Windows scans their files, perhaps while the YouTube
/// section starts another. So this leaves several times that.
const ASK_LIMIT: Duration = Duration::from_secs(45);

/// More than any version line; garbage beyond it is garbage anyway.
const OUTPUT_LIMIT: u64 = 4096;

/// A yt-dlp version, which is its release date, and for a nightly build the
/// time it was built as well: `2026.10.26`, `2026.10.26.232111`. Compared as
/// numbers, so that a nightly comes after the release of its day, and a
/// month or day written without its zero (as pip normalises them) reads the
/// same.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Release {
    year: u16,
    month: u8,
    day: u8,
    build: u64,
}

impl Release {
    /// The first line of `--version`'s output, or None for anything that isn't
    /// a version.
    pub fn read(text: &str) -> Option<Self> {
        let line = text.lines().next()?.trim();
        let parts: Vec<&str> = line.split('.').collect();
        if !(3..=4).contains(&parts.len())
            || parts
                .iter()
                .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
        {
            return None;
        }
        let year: u16 = parts[0].parse().ok()?;
        let month: u8 = parts[1].parse().ok()?;
        let day: u8 = parts[2].parse().ok()?;
        let build: u64 = match parts.get(3) {
            Some(build) => build.parse().ok()?,
            None => 0,
        };
        let valid = parts[0].len() == 4
            && (1..=12).contains(&month)
            && (1..=31).contains(&day)
            && parts[1].len() <= 2
            && parts[2].len() <= 2;
        valid.then_some(Self {
            year,
            month,
            day,
            build,
        })
    }
}

impl std::fmt::Display for Release {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{:02}.{:02}", self.year, self.month, self.day)?;
        if self.build != 0 {
            write!(f, ".{}", self.build)?;
        }
        Ok(())
    }
}

/// What a file was when it was asked: a change of either means another
/// yt-dlp, as an update replaces the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    size: u64,
    modified: SystemTime,
}

impl Stamp {
    /// Of the yt-dlp that running `path` starts: the program a Scoop shim
    /// names rather than the shim, which stays the same file through every
    /// update (`shim_target`). A shim whose program has gone is stamped
    /// itself, and fails to answer as that program would.
    fn of(path: &Path) -> Option<Self> {
        let metadata = shim_target(path)
            .and_then(|target| std::fs::metadata(target).ok())
            .or_else(|| std::fs::metadata(path).ok())?;
        Some(Self {
            size: metadata.len(),
            modified: metadata.modified().ok()?,
        })
    }
}

/// The program a Scoop shim starts, from the `<name>.shim` beside it. Scoop
/// puts a copy of one small launcher on the `PATH` for each program, and
/// `scoop update` copies the same launcher again, keeping its time, while
/// what changes is the program its `path = "…"` line names, through Scoop's
/// `current` folder. Nothing but Scoop writes such a file, so elsewhere this
/// finds nothing, at the cost of a failed open.
fn shim_target(path: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(path.with_extension("shim")).ok()?;
    text.lines()
        .filter_map(|line| line.split_once('='))
        .find(|(key, _)| key.trim() == "path")
        .map(|(_, value)| value.trim().trim_matches('"').trim())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Answer {
    Asking,
    Known(Release),
    /// It said nothing usable in time. Kept for the run only: a copy that was
    /// slow once, on a cold start, may well answer the next time.
    Failed,
}

struct Entry {
    stamp: Stamp,
    answer: Answer,
}

/// An answer as it is saved.
#[derive(Serialize, Deserialize)]
struct Saved {
    size: u64,
    modified_secs: u64,
    modified_nanos: u32,
    version: String,
}

#[derive(Default)]
struct State {
    /// Where answers are kept between runs; None keeps them in memory only.
    file: Option<PathBuf>,
    loaded: bool,
    known: BTreeMap<PathBuf, Entry>,
    asking: usize,
    /// The package's copy and the user's the choice was last made between,
    /// which an answer still to come may change it for.
    pair: Option<(PathBuf, PathBuf)>,
    /// The choice last handed out, which mpv's hook and the rest were given.
    handed: Option<PathBuf>,
    /// Counts the times that changed (`changes`).
    changes: u64,
}

pub struct Versions {
    state: Mutex<State>,
    saving: Mutex<()>,
    idle: Condvar,
    limit: Duration,
    /// Called when the choice handed out changes: on the asking thread when
    /// an answer changes it, on the choosing one when what is installed does.
    settled: Box<dyn Fn() + Send + Sync>,
}

/// The app's one set of answers.
pub fn global() -> &'static Arc<Versions> {
    static GLOBAL: OnceLock<Arc<Versions>> = OnceLock::new();
    GLOBAL.get_or_init(|| {
        Arc::new(Versions::new(
            ASK_LIMIT,
            Box::new(|| {
                let _ = slint::invoke_from_event_loop(|| {
                    crate::app::App::with_current(|app| app.ytdl_choice_changed());
                });
            }),
        ))
    })
}

/// Keeps the app's answers in `file` from now on. Read the first time a
/// choice is made, which happens only where a package carries yt-dlp, so a
/// Linux build never reads or writes it.
pub fn remember_in(file: PathBuf) {
    let mut state = global().lock();
    state.file = Some(file);
    state.loaded = false;
}

impl Versions {
    fn new(limit: Duration, settled: Box<dyn Fn() + Send + Sync>) -> Self {
        Self {
            state: Mutex::new(State::default()),
            saving: Mutex::new(()),
            idle: Condvar::new(),
            limit,
            settled,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The newer of the package's yt-dlp and the user's, `own`, by what is
    /// known now; the package's while either is still being asked, which
    /// starts asking any not yet asked, and when the user has none. Never
    /// waits for an answer. A tie goes to the package's copy, which the
    /// release was checked with; a copy that can't say its version loses to
    /// one that can.
    pub fn newer(self: &Arc<Self>, bundled: PathBuf, own: Option<PathBuf>) -> PathBuf {
        let mut state = self.lock();
        let chosen = match own {
            Some(own) => {
                self.load(&mut state);
                let of_bundled = self.answer(&mut state, &bundled);
                let of_own = self.answer(&mut state, &own);
                let chosen = match choose(of_bundled, of_own) {
                    Some(Pick::Own) => own.clone(),
                    Some(Pick::Bundled) | None => bundled.clone(),
                };
                state.pair = Some((bundled, own));
                chosen
            }
            None => {
                state.pair = None;
                bundled
            }
        };
        let changed = hand(&mut state, &chosen);
        drop(state);
        if changed {
            (self.settled)();
        }
        chosen
    }

    /// How many times the choice handed out has changed, which tells the app
    /// whether it has handed on the latest.
    pub fn changes(&self) -> u64 {
        self.lock().changes
    }

    /// What is known of `path`, asking it on a thread of its own if nothing
    /// is, or what is known is of a file since changed.
    fn answer(self: &Arc<Self>, state: &mut State, path: &Path) -> Answer {
        let Some(stamp) = Stamp::of(path) else {
            return Answer::Failed;
        };
        if let Some(entry) = state.known.get(path)
            && entry.stamp == stamp
        {
            return entry.answer;
        }
        state.known.insert(
            path.to_owned(),
            Entry {
                stamp,
                answer: Answer::Asking,
            },
        );
        state.asking += 1;
        let this = Arc::clone(self);
        let asked = path.to_owned();
        let spawned = std::thread::Builder::new()
            .name("yt-dlp version".into())
            .spawn(move || {
                let answer = match ask(&asked, this.limit) {
                    Ok(release) => {
                        log::debug!("{} is yt-dlp {release}", asked.display());
                        Answer::Known(release)
                    }
                    Err(error) => {
                        log::warn!(
                            "{} did not say which yt-dlp it is ({error}), so it is passed over \
                             for the other",
                            asked.display()
                        );
                        Answer::Failed
                    }
                };
                this.answered(&asked, stamp, answer);
            });
        match spawned {
            Ok(_) => Answer::Asking,
            Err(error) => {
                log::warn!("could not ask {} its version: {error}", path.display());
                state.asking -= 1;
                if let Some(entry) = state.known.get_mut(path) {
                    entry.answer = Answer::Failed;
                }
                Answer::Failed
            }
        }
    }

    fn answered(&self, path: &Path, stamp: Stamp, answer: Answer) {
        let mut state = self.lock();
        if let Some(entry) = state.known.get_mut(path)
            && entry.stamp == stamp
        {
            entry.answer = answer;
        }
        // The pair the choice was last made between, handed the package's
        // copy for want of an answer, now that both of its answers may be in.
        // Not an older pair, as the user's copy found before another took
        // its place on the PATH, which nothing would choose now.
        let of = |path: &Path| {
            state
                .known
                .get(path)
                .map_or(Answer::Failed, |entry| entry.answer)
        };
        let chosen = state.pair.as_ref().and_then(|(bundled, own)| {
            choose(of(bundled), of(own)).map(|pick| match pick {
                Pick::Bundled => bundled.clone(),
                Pick::Own => own.clone(),
            })
        });
        let changed = chosen.is_some_and(|chosen| hand(&mut state, &chosen));
        drop(state);
        if matches!(answer, Answer::Known(_)) {
            // One save at a time, each of everything known when it starts,
            // so that two threads answering together can't leave the older
            // list, or half of one, behind.
            let _saving = self
                .saving
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let list = {
                let state = self.lock();
                state.file.clone().map(|file| (file, saved(&state)))
            };
            if let Some((file, mut list)) = list {
                // Not the copies that have gone since, as an older Patina.app
                // moved to the bin, so that the list doesn't only grow.
                list.retain(|path, _| path.is_file());
                crate::settings::save_json(&file, &list);
            }
        }
        if changed {
            (self.settled)();
        }
        // Last, so that nothing waiting for the asking to end sees it end
        // before the app has been told.
        self.lock().asking -= 1;
        self.idle.notify_all();
    }

    /// Reads the saved answers, once.
    fn load(&self, state: &mut State) {
        if state.loaded {
            return;
        }
        state.loaded = true;
        let Some(file) = &state.file else {
            return;
        };
        let saved: BTreeMap<PathBuf, Saved> = crate::settings::load_json(file);
        for (path, saved) in saved {
            let Some(release) = Release::read(&saved.version) else {
                continue;
            };
            let modified = SystemTime::UNIX_EPOCH
                + Duration::new(saved.modified_secs, saved.modified_nanos.min(999_999_999));
            state.known.entry(path).or_insert(Entry {
                stamp: Stamp {
                    size: saved.size,
                    modified,
                },
                answer: Answer::Known(release),
            });
        }
    }

    /// Answers of their own, for a test, which no app hears of. The tests
    /// that ask stand-ins are for Unix, where those are shell scripts.
    #[cfg(all(test, unix))]
    pub(crate) fn for_tests(limit: Duration) -> Arc<Self> {
        Arc::new(Self::new(limit, Box::new(|| {})))
    }

    /// Waits until nothing is being asked, for at most `limit`; whether
    /// nothing is.
    #[cfg(all(test, unix))]
    pub(crate) fn wait_idle(&self, limit: Duration) -> bool {
        let state = self.lock();
        let (state, _) = self
            .idle
            .wait_timeout_while(state, limit, |state| state.asking > 0)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.asking == 0
    }
}

/// Notes `chosen` as the choice handed out; whether that is a change, which
/// the first choice is not, as everything starts with it.
fn hand(state: &mut State, chosen: &Path) -> bool {
    let changed = state
        .handed
        .as_deref()
        .is_some_and(|handed| handed != chosen);
    if changed {
        state.changes += 1;
    }
    state.handed = Some(chosen.to_owned());
    changed
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Pick {
    Bundled,
    Own,
}

/// The newer by these answers, or None while that depends on one still
/// being asked.
fn choose(bundled: Answer, own: Answer) -> Option<Pick> {
    match (bundled, own) {
        (_, Answer::Failed) => Some(Pick::Bundled),
        (Answer::Asking, _) | (_, Answer::Asking) => None,
        (Answer::Failed, Answer::Known(_)) => Some(Pick::Own),
        (Answer::Known(bundled), Answer::Known(own)) => Some(if own > bundled {
            Pick::Own
        } else {
            Pick::Bundled
        }),
    }
}

/// The answers worth keeping: versions read, of files whose times can be
/// written down.
fn saved(state: &State) -> BTreeMap<PathBuf, Saved> {
    state
        .known
        .iter()
        .filter_map(|(path, entry)| {
            let Answer::Known(release) = entry.answer else {
                return None;
            };
            let since = entry
                .stamp
                .modified
                .duration_since(SystemTime::UNIX_EPOCH)
                .ok()?;
            Some((
                path.clone(),
                Saved {
                    size: entry.stamp.size,
                    modified_secs: since.as_secs(),
                    modified_nanos: since.subsec_nanos(),
                    version: release.to_string(),
                },
            ))
        })
        .collect()
}

/// Runs `program --ignore-config --version` and reads its answer, within `limit`.
fn ask(program: &Path, limit: Duration) -> Result<Release, String> {
    let mut command = Command::new(program);
    // Without the user's configuration files, as the section runs it, so
    // that an option there yt-dlp doesn't know can't spoil the answer.
    command
        .args(["--ignore-config", "--version"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    crate::platform::no_console(&mut command);
    // So that one that doesn't answer is stopped with whatever it started,
    // as a single-file build's launcher starts the real yt-dlp.
    crate::platform::in_own_group(&mut command);
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let Some(mut stdout) = child.stdout.take() else {
        crate::platform::kill_group(&mut child);
        let _ = child.wait();
        return Err("no output".into());
    };
    let (send, output) = std::sync::mpsc::channel();
    let reader = std::thread::Builder::new()
        .name("yt-dlp version output".into())
        .spawn(move || {
            let mut bytes = Vec::new();
            let _ = (&mut stdout).take(OUTPUT_LIMIT).read_to_end(&mut bytes);
            let _ = send.send(bytes);
        });
    if let Err(error) = reader {
        crate::platform::kill_group(&mut child);
        let _ = child.wait();
        return Err(error.to_string());
    }
    let until = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                crate::platform::kill_group(&mut child);
                let _ = child.wait();
                return Err(error.to_string());
            }
        }
        if Instant::now() >= until {
            // Asked to end first, so that a single-file build's launcher
            // removes what it unpacked (`platform::end_group`).
            crate::platform::stop_group(&mut child);
            let _ = child.wait();
            return Err(format!(
                "slow to answer: nothing within {} s",
                limit.as_secs_f32()
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    // Whatever it started may still hold the pipe; not for long.
    let bytes = output
        .recv_timeout(Duration::from_secs(1))
        .map_err(|_| "its output did not end".to_owned())?;
    if !status.success() {
        return Err(format!("it ended with {status}"));
    }
    let text = String::from_utf8_lossy(&bytes);
    Release::read(&text).ok_or_else(|| {
        let first = text.lines().next().unwrap_or_default();
        format!(
            "{:?} is not a version",
            first.chars().take(80).collect::<String>()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_as_dates() {
        let release = Release::read("2026.10.26\n").unwrap();
        let nightly = Release::read("2026.10.26.232111").unwrap();
        let older = Release::read("2026.09.30.235959").unwrap();
        let newer = Release::read("2026.11.01").unwrap();
        assert!(older < release && release < nightly && nightly < newer);
        // Not as text: a later month written without its zero is still later.
        assert!(Release::read("2026.9.30").unwrap() < Release::read("2026.10.01").unwrap());
        assert_eq!(Release::read("2026.9.3"), Release::read("2026.09.03"));
        assert_eq!(release.to_string(), "2026.10.26");
        assert_eq!(nightly.to_string(), "2026.10.26.232111");
        // Only the first line counts, as yt-dlp may warn after it.
        assert_eq!(Release::read("2026.10.26\nWARNING: x"), Some(release));
        for garbage in [
            "",
            "yt-dlp 2026.10.26",
            "2026.10",
            "2026.10.26.1.2",
            "2026.13.01",
            "2026.10.32",
            "26.10.26",
            "2026.10.26-1",
            "2026..26",
            "2026.010.26",
            "Traceback (most recent call last):",
            "１２３４.10.26",
        ] {
            assert_eq!(Release::read(garbage), None, "{garbage:?}");
        }
    }

    #[test]
    fn the_newer_is_chosen_and_a_tie_goes_to_the_package() {
        let at = |text| Answer::Known(Release::read(text).unwrap());
        let old = at("2026.08.01");
        let new = at("2026.10.26");
        assert_eq!(choose(old, new), Some(Pick::Own));
        assert_eq!(choose(new, old), Some(Pick::Bundled));
        assert_eq!(choose(new, new), Some(Pick::Bundled));
        assert_eq!(choose(new, at("2026.10.26.000001")), Some(Pick::Own));
        // One that can't answer loses, whatever the other.
        assert_eq!(choose(Answer::Failed, new), Some(Pick::Own));
        assert_eq!(choose(new, Answer::Failed), Some(Pick::Bundled));
        assert_eq!(choose(Answer::Asking, Answer::Failed), Some(Pick::Bundled));
        assert_eq!(choose(Answer::Failed, Answer::Failed), Some(Pick::Bundled));
        // Otherwise the answer waits on both.
        assert_eq!(choose(Answer::Asking, new), None);
        assert_eq!(choose(new, Answer::Asking), None);
        assert_eq!(choose(Answer::Failed, Answer::Asking), None);
    }

    #[cfg(unix)]
    mod programs {
        use super::super::*;
        use crate::youtube::ytdlp::testing::Fake;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct Folder(PathBuf);

        impl Folder {
            fn new(name: &str) -> Self {
                let path = std::env::temp_dir().join(format!(
                    "patina-ytdl-versions-{name}-{}",
                    std::process::id()
                ));
                let _ = std::fs::remove_dir_all(&path);
                std::fs::create_dir_all(&path).unwrap();
                Self(path)
            }
        }

        impl Drop for Folder {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        /// Versions that count the times they told the app, and give up on a
        /// copy after `limit`.
        fn fresh(limit: Duration) -> (Arc<Versions>, Arc<AtomicUsize>) {
            let told = Arc::new(AtomicUsize::new(0));
            let counted = Arc::clone(&told);
            let versions = Arc::new(Versions::new(
                limit,
                Box::new(move || {
                    counted.fetch_add(1, Ordering::SeqCst);
                }),
            ));
            (versions, told)
        }

        fn idle(versions: &Versions) {
            assert!(versions.wait_idle(Duration::from_secs(20)), "still asking");
        }

        /// How many times a stand-in was asked its version.
        fn asked(fake: &Fake) -> usize {
            fake.runs()
                .iter()
                .filter(|run| run.as_slice() == ["--ignore-config", "--version"])
                .count()
        }

        #[test]
        fn the_users_newer_copy_wins_once_both_have_answered() {
            let folder = Folder::new("newer");
            let bundled = Fake::new(&folder.0.join("bundled"), "echo 2026.10.26");
            let own = Fake::new(&folder.0.join("own"), "echo 2026.11.01.232111");
            let (versions, told) = fresh(Duration::from_secs(10));
            let started = Instant::now();
            // Nothing known yet: the package's, at once.
            assert_eq!(
                versions.newer(bundled.program.clone(), Some(own.program.clone())),
                bundled.program
            );
            assert!(started.elapsed() < Duration::from_millis(500), "it waited");
            idle(&versions);
            assert_eq!(told.load(Ordering::SeqCst), 1, "the app was not told");
            assert_eq!(
                versions.newer(bundled.program.clone(), Some(own.program.clone())),
                own.program
            );
            // Asked once each, however often the choice is made.
            for _ in 0..5 {
                versions.newer(bundled.program.clone(), Some(own.program.clone()));
            }
            idle(&versions);
            assert_eq!(asked(&bundled), 1);
            assert_eq!(asked(&own), 1);
            assert_eq!(told.load(Ordering::SeqCst), 1);
        }

        #[test]
        fn an_older_copy_of_the_users_leaves_the_package_s_and_says_nothing() {
            let folder = Folder::new("older");
            let bundled = Fake::new(&folder.0.join("bundled"), "echo 2026.10.26");
            let own = Fake::new(&folder.0.join("own"), "echo 2026.06.02");
            let (versions, told) = fresh(Duration::from_secs(10));
            versions.newer(bundled.program.clone(), Some(own.program.clone()));
            idle(&versions);
            assert_eq!(
                versions.newer(bundled.program.clone(), Some(own.program.clone())),
                bundled.program
            );
            assert_eq!(told.load(Ordering::SeqCst), 0);
        }

        /// A changed file is another yt-dlp, and is asked again; an unchanged
        /// one isn't, even by a new run, which reads what the last one saved.
        #[test]
        fn answers_are_kept_until_the_file_changes() {
            let folder = Folder::new("cache");
            let file = folder.0.join("state").join("yt-dlp-versions.json");
            let bundled = Fake::new(&folder.0.join("bundled"), "echo 2026.10.26");
            let own = Fake::new(&folder.0.join("own"), "echo 2026.06.02");
            let (versions, _) = fresh(Duration::from_secs(10));
            versions.lock().file = Some(file.clone());
            versions.newer(bundled.program.clone(), Some(own.program.clone()));
            idle(&versions);
            let text = std::fs::read_to_string(&file).unwrap();
            assert!(
                text.contains("\"2026.10.26\"") && text.contains("\"2026.06.02\""),
                "{text}"
            );

            // The user updates theirs: a new file, a new answer.
            let own = Fake::new(&folder.0.join("own"), "echo 2026.11.02.010203");
            assert_eq!(
                versions.newer(bundled.program.clone(), Some(own.program.clone())),
                bundled.program
            );
            idle(&versions);
            assert_eq!(
                versions.newer(bundled.program.clone(), Some(own.program.clone())),
                own.program
            );
            assert_eq!(asked(&bundled), 1);
            assert_eq!(asked(&own), 2);

            // The next run asks neither.
            let (next, told) = fresh(Duration::from_secs(10));
            next.lock().file = Some(file.clone());
            assert_eq!(
                next.newer(bundled.program.clone(), Some(own.program.clone())),
                own.program
            );
            idle(&next);
            assert_eq!(asked(&bundled), 1);
            assert_eq!(asked(&own), 2);
            assert_eq!(told.load(Ordering::SeqCst), 0);

            // A saved file of rubbish is ignored, and the copies asked again.
            std::fs::write(&file, b"{not json").unwrap();
            let (again, _) = fresh(Duration::from_secs(10));
            again.lock().file = Some(file);
            again.newer(bundled.program.clone(), Some(own.program.clone()));
            idle(&again);
            assert_eq!(asked(&bundled), 2);
            assert_eq!(asked(&own), 3);
        }

        /// One that doesn't answer in time, or answers nonsense, or fails, is
        /// passed over for the other, without the choice ever waiting for it.
        #[test]
        fn a_slow_or_broken_copy_is_passed_over() {
            let folder = Folder::new("broken");
            let bundled = Fake::new(&folder.0.join("bundled"), "echo 2026.10.26");
            let limit = Duration::from_millis(600);
            for (name, body) in [
                ("slow", "sleep 30"),
                ("garbage", "echo 'Traceback (most recent call last):'"),
                ("failing", "echo 2026.11.01; exit 1"),
                ("silent", "true"),
            ] {
                let own = Fake::new(&folder.0.join(name), body);
                let (versions, told) = fresh(limit);
                let started = Instant::now();
                assert_eq!(
                    versions.newer(bundled.program.clone(), Some(own.program.clone())),
                    bundled.program,
                    "{name}"
                );
                assert!(started.elapsed() < Duration::from_millis(300), "{name}");
                idle(&versions);
                if name == "slow" {
                    assert!(started.elapsed() >= limit, "not given its time");
                }
                assert!(
                    started.elapsed() < limit + crate::platform::END_GRACE + Duration::from_secs(5),
                    "{name}"
                );
                assert_eq!(
                    versions.newer(bundled.program.clone(), Some(own.program.clone())),
                    bundled.program,
                    "{name}"
                );
                assert_eq!(told.load(Ordering::SeqCst), 0, "{name}");
                // Not asked again for the rest of the run.
                idle(&versions);
                assert_eq!(asked(&own), 1, "{name}");
            }

            // And the other way round: a broken package's copy loses to the
            // user's, whatever its age.
            let broken = Fake::new(&folder.0.join("broken-bundled"), "echo nonsense");
            let own = Fake::new(&folder.0.join("own"), "echo 2025.01.01");
            let (versions, told) = fresh(limit);
            versions.newer(broken.program.clone(), Some(own.program.clone()));
            idle(&versions);
            assert_eq!(told.load(Ordering::SeqCst), 1);
            assert_eq!(
                versions.newer(broken.program.clone(), Some(own.program.clone())),
                own.program
            );
        }

        /// A copy that has gone is no choice, and is never waited for.
        #[test]
        fn a_missing_copy_is_passed_over() {
            let folder = Folder::new("missing");
            let bundled = Fake::new(&folder.0.join("bundled"), "echo 2026.10.26");
            let gone = folder.0.join("gone").join("yt-dlp");
            let (versions, told) = fresh(Duration::from_secs(10));
            let started = Instant::now();
            assert_eq!(
                versions.newer(bundled.program.clone(), Some(gone)),
                bundled.program
            );
            assert!(started.elapsed() < Duration::from_millis(300), "it waited");
            idle(&versions);
            assert_eq!(told.load(Ordering::SeqCst), 0);
        }

        /// The choice can move back to the package's copy too, and the app
        /// hears of that as well, or mpv's hook would go on running a copy
        /// that has gone: when the user's is removed, and when it is replaced
        /// by an older one.
        #[test]
        fn the_app_hears_when_the_choice_moves_back_to_the_package_s() {
            let folder = Folder::new("back");
            let bundled = Fake::new(&folder.0.join("bundled"), "echo 2026.10.26");
            let own = Fake::new(&folder.0.join("own"), "echo 2026.11.01");
            let (versions, told) = fresh(Duration::from_secs(10));
            versions.newer(bundled.program.clone(), Some(own.program.clone()));
            idle(&versions);
            assert_eq!(told.load(Ordering::SeqCst), 1);

            // Removed: find_ytdl finds none installed.
            assert_eq!(
                versions.newer(bundled.program.clone(), None),
                bundled.program
            );
            assert_eq!(told.load(Ordering::SeqCst), 2);
            // Said once, not each time the choice is made.
            versions.newer(bundled.program.clone(), None);
            assert_eq!(told.load(Ordering::SeqCst), 2);

            // Installed again, still newer: back to it, which wasn't asked
            // again, being the same file.
            assert_eq!(
                versions.newer(bundled.program.clone(), Some(own.program.clone())),
                own.program
            );
            assert_eq!(told.load(Ordering::SeqCst), 3);

            // Replaced by an older one: the package's at once, while it is
            // asked, and still the package's once it has answered, which is
            // no further change.
            let own = Fake::new(&folder.0.join("own"), "echo 2026.6.2");
            assert_eq!(
                versions.newer(bundled.program.clone(), Some(own.program.clone())),
                bundled.program
            );
            assert_eq!(told.load(Ordering::SeqCst), 4);
            idle(&versions);
            assert_eq!(
                versions.newer(bundled.program.clone(), Some(own.program.clone())),
                bundled.program
            );
            assert_eq!(told.load(Ordering::SeqCst), 4);
            assert_eq!(versions.changes(), 4);
        }

        /// A Scoop shim stays the same file through `scoop update`; the
        /// program it starts is what changes, and what the answer is kept by.
        #[test]
        fn a_scoop_shim_is_asked_again_when_its_program_changes() {
            let folder = Folder::new("shim");
            let bundled = Fake::new(&folder.0.join("bundled"), "echo 2026.10.26");
            let current = folder.0.join("apps").join("yt-dlp").join("current");
            let target = Fake::new(&current, "echo 2026.06.02");
            let shim = Fake::new(
                &folder.0.join("shims"),
                &format!("exec '{}' \"$@\"", target.program.display()),
            );
            std::fs::write(
                shim.program.with_extension("shim"),
                format!("path = \"{}\"\n", target.program.display()),
            )
            .unwrap();
            let before = std::fs::metadata(&shim.program).unwrap();
            let (versions, told) = fresh(Duration::from_secs(10));
            versions.newer(bundled.program.clone(), Some(shim.program.clone()));
            idle(&versions);
            assert_eq!(
                versions.newer(bundled.program.clone(), Some(shim.program.clone())),
                bundled.program
            );

            let target = Fake::new(&current, "echo 2026.11.02.010203");
            let after = std::fs::metadata(&shim.program).unwrap();
            assert_eq!(
                (before.len(), before.modified().unwrap()),
                (after.len(), after.modified().unwrap()),
                "the shim was touched"
            );
            versions.newer(bundled.program.clone(), Some(shim.program.clone()));
            idle(&versions);
            assert_eq!(
                versions.newer(bundled.program.clone(), Some(shim.program.clone())),
                shim.program
            );
            assert_eq!(told.load(Ordering::SeqCst), 1);
            assert_eq!(asked(&shim), 2);
            assert_eq!(asked(&target), 2);

            // Unquoted, as older shims were written, and with spaces anywhere.
            std::fs::write(
                shim.program.with_extension("shim"),
                format!("args = -v\n path={} \n", target.program.display()),
            )
            .unwrap();
            assert_eq!(shim_target(&shim.program), Some(target.program.clone()));
            std::fs::write(shim.program.with_extension("shim"), "args = -v\n").unwrap();
            assert_eq!(shim_target(&shim.program), None);
            assert_eq!(shim_target(&bundled.program), None);
        }
    }
}
