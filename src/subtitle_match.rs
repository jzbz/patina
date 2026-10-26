//! Finds the subtitle files that belong to a video, the way IINA does.
//!
//! Candidates are subtitle files in the video's folder and its immediate
//! subfolders ("Subs/", "Subtitles/"...). Each candidate is scored against
//! every video in the folder, so a season folder's "Show - 02.srt" goes to
//! episode 2 and not to episode 3, and a subtitle only goes to the video it
//! matches best. Subtitles in the preferred languages come first.

use crate::mpv::{Mpv, Value};
use crate::player::state::language_name;
use std::path::{Path, PathBuf};

const SUBTITLE_EXTENSIONS: &[&str] = &[
    "ass", "idx", "lrc", "mks", "smi", "srt", "ssa", "sub", "sup", "vtt", "utf", "utf8", "utf-8",
    "rt", "aqt", "jss", "scc",
];
const VIDEO_EXTENSIONS: &[&str] = &[
    "3gp", "asf", "avi", "divx", "f4v", "flv", "m2ts", "m4v", "mk3d", "mkv", "mov", "mp4", "mpeg",
    "mpg", "mts", "mxf", "ogm", "ogv", "rm", "rmvb", "ts", "vob", "webm", "wmv",
];
const AUDIO_EXTENSIONS: &[&str] = &[
    "aac", "ac3", "aiff", "alac", "ape", "dsf", "dts", "flac", "m4a", "mka", "mp2", "mp3", "oga",
    "ogg", "opus", "tak", "tta", "wav", "wma", "wv",
];
const IMAGE_EXTENSIONS: &[&str] = &[
    "avif", "bmp", "gif", "heic", "jpeg", "jpg", "jxl", "png", "tif", "tiff", "webp",
];
/// Tags after a subtitle's name that say nothing about which video it is for.
const TAGS: &[&str] = &[
    "forced",
    "sdh",
    "cc",
    "hi",
    "default",
    "full",
    "signs",
    "songs",
    "subs",
    "sub",
    "subtitles",
];
/// Codes whose English names also count as language tags ("English").
const NAMED_LANGUAGES: &[&str] = &[
    "en", "ja", "zh", "ko", "fr", "de", "es", "it", "pt", "ru", "ar", "hi", "nl", "sv", "no", "da",
    "fi", "pl", "cs", "hu", "el", "tr", "he", "th", "vi", "id", "ms", "uk", "ro", "bg", "hr", "sr",
];

/// Hands mpv the subtitle files that match the file it is about to open,
/// from mpv's `on_load` hook, as that file's `sub-files`: mpv then loads
/// and selects them as it would its own auto-loaded files. Blocks on mpv.
pub fn load_for_current_file(mpv: &Mpv, languages: &[String]) {
    let Ok(path) = mpv.get_property("path") else {
        return;
    };
    let Some(path) = path.as_str().filter(|path| !crate::util::is_url(path)) else {
        return;
    };
    let video = std::path::absolute(path).unwrap_or_else(|_| PathBuf::from(path));
    let found = if has_extension(&video, IMAGE_EXTENSIONS) {
        return;
    } else if has_extension(&video, AUDIO_EXTENSIONS) {
        // Lyrics for a song, only by its exact name.
        find(&video, languages, false)
    } else {
        find(&video, languages, true)
    };
    if found.is_empty() {
        return;
    }
    let mut files: Vec<String> = mpv
        .get_property("options/sub-files")
        .map(|files| {
            files
                .as_array()
                .iter()
                .filter_map(|file| file.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    for subtitle in found {
        log::debug!("matched subtitle {}", subtitle.display());
        files.push(subtitle.to_string_lossy().into_owned());
    }
    if let Err(error) = mpv.set_property("file-local-options/sub-files", Value::List(files)) {
        log::warn!("{error}");
    }
}

/// Subtitle files near `video` that belong to it, best first.
/// `languages` are preferred language codes, most preferred first;
/// `lone_video` lets the only video in a folder take subtitles nothing else
/// claims, as IINA does.
fn find(video: &Path, languages: &[String], lone_video: bool) -> Vec<PathBuf> {
    let Some(dir) = video.parent() else {
        return Vec::new();
    };
    let list = |dir: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
            .unwrap_or_default()
    };
    let entries = list(dir);
    let mut videos: Vec<PathBuf> = entries
        .iter()
        .filter(|path| has_extension(path, VIDEO_EXTENSIONS))
        .cloned()
        .collect();
    if !videos.iter().any(|path| path == video) {
        videos.push(video.to_path_buf());
    }
    let mut candidates: Vec<PathBuf> = entries
        .iter()
        .filter(|path| has_extension(path, SUBTITLE_EXTENSIONS))
        .cloned()
        .collect();
    for subdir in entries.iter().filter(|path| path.is_dir()) {
        let hidden = subdir
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with('.'));
        if !hidden {
            candidates.extend(
                list(subdir)
                    .into_iter()
                    .filter(|path| has_extension(path, SUBTITLE_EXTENSIONS)),
            );
        }
    }
    let single = lone_video && videos.len() <= 1;
    rank(video, &videos, candidates, languages, single)
}

fn rank(
    video: &Path,
    videos: &[PathBuf],
    candidates: Vec<PathBuf>,
    languages: &[String],
    single: bool,
) -> Vec<PathBuf> {
    let mut matches: Vec<(usize, f64, PathBuf)> = candidates
        .into_iter()
        .filter_map(|subtitle| {
            let mine = score(video, &subtitle, single);
            // Only the video the subtitle fits best gets it.
            let best = videos
                .iter()
                .map(|other| score(other, &subtitle, single))
                .fold(mine, f64::max);
            (mine > 0.0 && mine >= best)
                .then(|| (language_rank(&subtitle, languages), mine, subtitle))
        })
        .collect();
    matches.sort_by(|a, b| {
        a.0.cmp(&b.0).then(b.1.total_cmp(&a.1)).then_with(|| {
            crate::window::natural_cmp(&a.2.to_string_lossy(), &b.2.to_string_lossy())
        })
    });
    matches.into_iter().map(|(_, _, path)| path).collect()
}

/// How well `subtitle` fits `video` (0 = not at all). `single` says the
/// folder holds only one video, which then takes subtitles nothing else
/// claims, as IINA does.
fn score(video: &Path, subtitle: &Path, single: bool) -> f64 {
    let video_stem = stem(video);
    let subtitle_stem = stem(subtitle);
    let video_words = words(&video_stem);
    let (subtitle_words, _) = split_tags(&subtitle_stem);
    if subtitle_words == video_words {
        return 100.0;
    }
    if subtitle_stem.starts_with(&video_stem) {
        return 95.0;
    }
    if !video_words.is_empty()
        && subtitle_words
            .windows(video_words.len())
            .any(|window| window == video_words)
    {
        return 85.0;
    }
    let video_name = without_brackets(&video_stem);
    let subtitle_name = without_brackets(&subtitle_stem);
    if let (Some(v), Some(s)) = (episode(&video_name), episode(&subtitle_name))
        && v.number == s.number
        && (v.season.is_none() || s.season.is_none() || v.season == s.season)
    {
        let likeness = similarity(&series(&video_name, &v), &series(&subtitle_name, &s));
        if likeness >= 0.5 || single {
            return 60.0 + 30.0 * likeness;
        }
    }
    if single { 40.0 } else { 0.0 }
}

/// The position of the subtitle's language among the preferred ones, or
/// after all of them.
fn language_rank(subtitle: &Path, languages: &[String]) -> usize {
    let stem = stem(subtitle);
    let (core, tags) = split_tags(&stem);
    let mut found: Vec<&str> = tags.iter().filter_map(|word| language(word)).collect();
    // "English.srt", "eng.srt"
    if let [word] = core.as_slice()
        && let Some(name) = language(word)
    {
        found.push(name);
    }
    languages
        .iter()
        .position(|wanted| {
            language(wanted.trim()).is_some_and(|wanted| {
                // "zh" takes "Chinese (Simplified)" too.
                found.iter().any(|name| name.starts_with(wanted))
            })
        })
        .unwrap_or(languages.len())
}

/// A season and episode number found in a name: "S01E02", "1x02", "E02",
/// "Episode 2", or a lone number such as the "02" in "Show - 02".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Episode {
    season: Option<u32>,
    number: u32,
    /// Where it starts, in words; what comes before names the series.
    at: usize,
}

fn episode(name: &str) -> Option<Episode> {
    let words = words(name);
    let number = |text: &str| -> Option<u32> {
        (!text.is_empty() && text.len() <= 3 && text.bytes().all(|b| b.is_ascii_digit()))
            .then(|| text.parse().ok())
            .flatten()
    };
    let found = |season, number, at| Some(Episode { season, number, at });
    // Explicit forms first.
    for (index, word) in words.iter().enumerate() {
        if let Some((season, episode)) =
            word.strip_prefix('s').and_then(|rest| rest.split_once('e'))
            && let (Some(season), Some(episode)) = (number(season), number(episode))
        {
            return found(Some(season), episode, index);
        }
        if let Some((season, episode)) = word.split_once('x')
            && season.len() <= 2
            && episode.len() >= 2
            && let (Some(season), Some(episode)) = (number(season), number(episode))
        {
            return found(Some(season), episode, index);
        }
        if let Some(episode) = ["ep", "e"]
            .iter()
            .find_map(|prefix| word.strip_prefix(prefix).and_then(number))
        {
            return found(None, episode, index);
        }
        if matches!(word.as_str(), "episode" | "ep" | "e")
            && let Some(episode) = words.get(index + 1).and_then(|next| number(next))
        {
            return found(None, episode, index);
        }
    }
    // Otherwise the first lone number of up to three digits after the
    // series name (four digits are a year or a resolution).
    words
        .iter()
        .enumerate()
        .skip(1)
        .find_map(|(index, word)| number(word).and_then(|episode| found(None, episode, index)))
}

/// The series name: the words before the episode number.
fn series(name: &str, episode: &Episode) -> String {
    words(name)[..episode.at].join(" ")
}

fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| extensions.contains(&ext.to_ascii_lowercase().as_str()))
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// Lowercase words, split at anything that isn't a letter or digit.
fn words(name: &str) -> Vec<String> {
    name.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// The name without "[Group]" and "(1080p)" parts, which release groups
/// add and which say nothing about the episode.
fn without_brackets(name: &str) -> String {
    let mut depth = 0usize;
    let mut kept = String::with_capacity(name.len());
    for c in name.chars() {
        match c {
            '[' | '(' | '{' => depth += 1,
            ']' | ')' | '}' => depth = depth.saturating_sub(1),
            _ if depth == 0 => kept.push(c),
            _ => {}
        }
    }
    kept
}

/// A language a word names, by ISO code ("en", "jpn") or English name.
fn language(word: &str) -> Option<&'static str> {
    language_name(word).or_else(|| {
        NAMED_LANGUAGES
            .iter()
            .filter_map(|code| language_name(code))
            .find(|name| name.eq_ignore_ascii_case(word))
    })
}

/// Splits a subtitle name into the words that name its video and the
/// trailing language codes and tags ("movie.en.forced" → "movie" and
/// "en forced"). At least one word stays in the first part.
fn split_tags(name: &str) -> (Vec<String>, Vec<String>) {
    let mut core = words(name);
    let mut tags = Vec::new();
    while core.len() > 1
        && core
            .last()
            .is_some_and(|word| language(word).is_some() || TAGS.contains(&word.as_str()))
    {
        tags.insert(0, core.pop().unwrap_or_default());
    }
    (core, tags)
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut current = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            current.push(
                (previous[j] + cost)
                    .min(previous[j + 1] + 1)
                    .min(current[j] + 1),
            );
        }
        previous = current;
    }
    previous[b.len()]
}

/// 1 for identical strings, 0 for nothing in common.
fn similarity(a: &str, b: &str) -> f64 {
    let longest = a.chars().count().max(b.chars().count());
    if longest == 0 {
        return 1.0;
    }
    1.0 - levenshtein(a, b) as f64 / longest as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mpv::testing::{headless, wait_for};
    use crate::mpv::{Event, Node};

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names
            .iter()
            .map(|name| PathBuf::from(format!("/media/{name}")))
            .collect()
    }

    fn found(video: &str, videos: &[&str], subtitles: &[&str], languages: &[&str]) -> Vec<String> {
        let languages: Vec<String> = languages.iter().map(|code| (*code).to_owned()).collect();
        let video = PathBuf::from(format!("/media/{video}"));
        let videos = paths(videos);
        let single = videos.len() <= 1;
        rank(&video, &videos, paths(subtitles), &languages, single)
            .into_iter()
            .map(|path| file_name(&path))
            .collect()
    }

    fn file_name(path: &Path) -> String {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    #[test]
    fn episode_numbers_are_found_in_common_forms() {
        let number = |name: &str| {
            episode(&without_brackets(&name.to_lowercase())).map(|e| (e.season, e.number))
        };
        assert_eq!(number("Show.Name.S01E02.1080p"), Some((Some(1), 2)));
        assert_eq!(number("show name 1x05"), Some((Some(1), 5)));
        assert_eq!(number("[Group] Show - 07 [1080p]"), Some((None, 7)));
        assert_eq!(number("Show Episode 12"), Some((None, 12)));
        assert_eq!(number("Show EP03"), Some((None, 3)));
        assert_eq!(number("Movie 2019 1080p x264"), None);
    }

    #[test]
    fn same_name_and_language_variants_match() {
        let subtitles = [
            "Movie.srt",
            "Movie.en.srt",
            "Movie.ja.forced.ass",
            "Other.srt",
        ];
        assert_eq!(
            found("Movie.mkv", &["Movie.mkv", "Other.mkv"], &subtitles, &[]),
            ["Movie.en.srt", "Movie.ja.forced.ass", "Movie.srt"]
        );
    }

    #[test]
    fn episodes_get_their_own_subtitles() {
        let videos = [
            "[G] Show - 01 [1080p].mkv",
            "[G] Show - 02 [1080p].mkv",
            "[G] Show - 03 [1080p].mkv",
        ];
        let subtitles = [
            "Show 01.ass",
            "Show 02.ass",
            "Show 03.ass",
            "Unrelated Thing 02.ass",
        ];
        assert_eq!(found(videos[1], &videos, &subtitles, &[]), ["Show 02.ass"]);
        let videos = ["Show.S01E01.mkv", "Show.S01E02.mkv"];
        let subtitles = ["Show - 1x01.srt", "Show - 1x02.en.srt", "Show.S02E02.srt"];
        assert_eq!(
            found(videos[1], &videos, &subtitles, &[]),
            ["Show - 1x02.en.srt"]
        );
    }

    #[test]
    fn a_lone_video_takes_the_folder_subtitles() {
        assert_eq!(
            found(
                "film.mp4",
                &["film.mp4"],
                &["English.srt", "subs.ja.ass"],
                &[]
            ),
            ["English.srt", "subs.ja.ass"]
        );
    }

    #[test]
    fn preferred_languages_come_first() {
        let subtitles = [
            "Movie.en.srt",
            "Movie.ja.srt",
            "Movie.chs.srt",
            "Movie.fr.srt",
        ];
        assert_eq!(
            found("Movie.mkv", &["Movie.mkv"], &subtitles, &["zh", "ja", "fr"]),
            [
                "Movie.chs.srt",
                "Movie.ja.srt",
                "Movie.fr.srt",
                "Movie.en.srt"
            ]
        );
        assert_eq!(
            found(
                "Movie.mkv",
                &["Movie.mkv"],
                &["English.srt", "Japanese.srt"],
                &["jpn"]
            ),
            ["Japanese.srt", "English.srt"]
        );
    }

    #[test]
    fn subfolders_are_searched() {
        let dir = std::env::temp_dir().join(format!("patina-subs-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Subs")).unwrap();
        for file in ["Movie.mkv", "Subs/Movie.en.srt", "Movie.txt"] {
            std::fs::write(dir.join(file), b"").unwrap();
        }
        let found = find(&dir.join("Movie.mkv"), &[], true);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(found, [dir.join("Subs/Movie.en.srt")]);
    }

    #[test]
    fn matched_subtitles_are_loaded_and_selected_by_mpv() {
        let Some(mpv) = headless() else { return };
        let dir = std::env::temp_dir().join(format!("patina-subs-mpv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let video = dir.join("Clip.S01E03.mkv");
        let made = std::process::Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .args(["testsrc=duration=2:size=64x48:rate=10"])
            .arg(&video)
            .status()
            .is_ok_and(|status| status.success());
        if !made {
            eprintln!("skipping: needs ffmpeg");
            return;
        }
        let cue = "1\n00:00:00,000 --> 00:00:02,000\nHello\n";
        for name in ["Clip - 03.en.srt", "Clip - 04.en.srt", "Clip - 03.ja.srt"] {
            std::fs::write(dir.join(name), cue).unwrap();
        }
        std::fs::write(dir.join("Clip.S01E04.mkv"), b"").unwrap();

        let subtitles = |languages: &[String]| -> Vec<(String, bool)> {
            mpv.command(&["loadfile", &video.to_string_lossy()])
                .unwrap();
            let Event::Hook { id, .. } = wait_for(&mpv, |event| {
                matches!(event, Event::Hook { userdata: 7, .. })
            }) else {
                unreachable!()
            };
            load_for_current_file(&mpv, languages);
            mpv.hook_continue(id).unwrap();
            wait_for(&mpv, |event| matches!(event, Event::FileLoaded));
            let tracks = mpv.get_property("track-list").unwrap();
            tracks
                .as_array()
                .iter()
                .filter(|track| track.get("type").and_then(Node::as_str) == Some("sub"))
                .map(|track| {
                    let path = track.str_or_default("external-filename");
                    (
                        file_name(Path::new(&path)),
                        track.bool_or("selected", false),
                    )
                })
                .collect()
        };
        mpv.hook_add(7, "on_load", 0).unwrap();
        let english = subtitles(&[]);
        mpv.set_property("slang", "ja").unwrap();
        let japanese = subtitles(&["ja".to_owned()]);
        mpv.command(&["stop"]).unwrap();
        wait_for(&mpv, |event| matches!(event, Event::EndFile { .. }));
        let options = mpv.get_property("options/sub-files").unwrap();
        std::fs::remove_dir_all(&dir).unwrap();

        // Only episode 3's subtitles, and mpv selects one as it would its
        // own auto-loaded files, following the preferred language.
        assert_eq!(
            english,
            [
                ("Clip - 03.en.srt".to_owned(), true),
                ("Clip - 03.ja.srt".to_owned(), false)
            ]
        );
        assert_eq!(
            japanese,
            [
                ("Clip - 03.ja.srt".to_owned(), true),
                ("Clip - 03.en.srt".to_owned(), false)
            ]
        );
        // The list only applied to those files.
        assert!(options.as_array().is_empty());
    }
}
