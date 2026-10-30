//! What the preferences window shows: every setting, on which page, with
//! which control. Keys are the kebab-case field names of `Preferences`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    General,
    Interface,
    Video,
    Audio,
    Subtitles,
    Network,
    Control,
    KeyBindings,
    Advanced,
    Utilities,
}

impl Page {
    pub const ALL: [Page; 10] = [
        Page::General,
        Page::Interface,
        Page::Video,
        Page::Audio,
        Page::Subtitles,
        Page::Network,
        Page::Control,
        Page::KeyBindings,
        Page::Advanced,
        Page::Utilities,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Page::General => "General",
            Page::Interface => "Interface",
            Page::Video => "Video",
            Page::Audio => "Audio",
            Page::Subtitles => "Subtitles",
            Page::Network => "Network",
            Page::Control => "Control",
            Page::KeyBindings => "Key Bindings",
            Page::Advanced => "Advanced",
            Page::Utilities => "Utilities",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Control {
    Switch,
    Number {
        min: f64,
        max: f64,
        step: f64,
        unit: &'static str,
    },
    /// Stored values and their labels.
    Choice(&'static [(&'static str, &'static str)]),
    /// Choices that come from the running player (audio devices).
    DynamicChoice,
    Text,
    /// A folder (true) or file (false) path, with a Browse button.
    Path(bool),
    /// Several lines of text.
    Lines,
    /// A button that runs an action.
    Button,
    /// A password, kept in the system's store rather than the settings.
    Password,
}

#[derive(Debug, Clone, Copy)]
pub struct Item {
    pub page: Page,
    pub section: &'static str,
    pub key: &'static str,
    pub label: &'static str,
    pub detail: &'static str,
    pub control: Control,
    /// Only editable while this switch is on.
    pub requires: Option<&'static str>,
}

const fn item(
    page: Page,
    section: &'static str,
    key: &'static str,
    label: &'static str,
    detail: &'static str,
    control: Control,
) -> Item {
    Item {
        page,
        section,
        key,
        label,
        detail,
        control,
        requires: None,
    }
}

const fn number(min: f64, max: f64, step: f64, unit: &'static str) -> Control {
    Control::Number {
        min,
        max,
        step,
        unit,
    }
}

const fn needs(mut item: Item, key: &'static str) -> Item {
    item.requires = Some(key);
    item
}

const CLICK_ACTIONS: &[(&str, &str)] = &[
    ("none", "Do nothing"),
    ("pause", "Pause / resume"),
    ("fullscreen", "Enter / exit full screen"),
    ("toggle-osc", "Show / hide the controller"),
    ("pip", "Enter / exit picture in picture"),
    ("ab-loop", "Set the A-B loop"),
    ("reset-speed", "Reset the speed"),
    ("context-menu", "Show the menu"),
];

const SCROLL_ACTIONS: &[(&str, &str)] = &[
    ("volume", "Adjust the volume"),
    ("seek", "Seek"),
    ("speed", "Adjust the speed"),
    ("none", "Do nothing"),
];

const ENCODINGS: &[(&str, &str)] = &[
    ("auto", "Detect automatically"),
    ("utf-8", "Unicode (UTF-8)"),
    ("utf-16", "Unicode (UTF-16)"),
    ("windows-1250", "Central European (Windows-1250)"),
    ("windows-1251", "Cyrillic (Windows-1251)"),
    ("windows-1252", "Western (Windows-1252)"),
    ("windows-1253", "Greek (Windows-1253)"),
    ("windows-1254", "Turkish (Windows-1254)"),
    ("windows-1255", "Hebrew (Windows-1255)"),
    ("windows-1256", "Arabic (Windows-1256)"),
    ("windows-1257", "Baltic (Windows-1257)"),
    ("windows-1258", "Vietnamese (Windows-1258)"),
    ("iso-8859-1", "Western (ISO 8859-1)"),
    ("iso-8859-2", "Central European (ISO 8859-2)"),
    ("iso-8859-5", "Cyrillic (ISO 8859-5)"),
    ("iso-8859-7", "Greek (ISO 8859-7)"),
    ("iso-8859-9", "Turkish (ISO 8859-9)"),
    ("iso-8859-15", "Western (ISO 8859-15)"),
    ("koi8-r", "Russian (KOI8-R)"),
    ("koi8-u", "Ukrainian (KOI8-U)"),
    ("shift_jis", "Japanese (Shift JIS)"),
    ("euc-jp", "Japanese (EUC-JP)"),
    ("iso-2022-jp", "Japanese (ISO 2022-JP)"),
    ("gbk", "Simplified Chinese (GBK)"),
    ("gb18030", "Simplified Chinese (GB 18030)"),
    ("big5", "Traditional Chinese (Big 5)"),
    ("big5-hkscs", "Traditional Chinese (Big 5 HKSCS)"),
    ("euc-kr", "Korean (EUC-KR)"),
    ("tis-620", "Thai (TIS-620)"),
];

use Control::{Button, Choice, Lines, Password, Path, Switch, Text};
use Page::{Advanced, Audio, General, Interface, Network, Subtitles, Utilities, Video};

pub const ITEMS: &[Item] = &[
    // General.
    item(
        General,
        "Launch",
        "launch-action",
        "When Patina starts",
        "",
        Choice(&[
            ("welcome", "Show the welcome screen"),
            ("open-dialog", "Show an Open dialog"),
        ]),
    ),
    item(
        General,
        "When media is opened",
        "open-in-new-window",
        "Allow several player windows",
        "Media opens in a new window unless one is idle.",
        Switch,
    ),
    item(
        General,
        "When media is opened",
        "pause-when-open",
        "Start paused",
        "",
        Switch,
    ),
    item(
        General,
        "When media is opened",
        "fullscreen-when-open",
        "Enter full screen",
        "",
        Switch,
    ),
    item(
        General,
        "Playback",
        "auto-play-next",
        "Play the next item automatically",
        "",
        Switch,
    ),
    item(
        General,
        "Playback",
        "keep-open",
        "Keep the window open after playback finishes",
        "Otherwise the welcome screen comes back.",
        Switch,
    ),
    item(
        General,
        "Playback",
        "playlist-auto-add",
        "Add other files in the folder to the playlist",
        "Opening one file queues the rest of the folder.",
        Switch,
    ),
    item(
        General,
        "Playback",
        "playlist-show-metadata",
        "Show artist and title for audio files",
        "From their tags, in the playlist.",
        Switch,
    ),
    item(
        General,
        "Playback",
        "resume-playback",
        "Resume where you left off",
        "",
        Switch,
    ),
    item(
        General,
        "Playback",
        "pause-when-minimized",
        "Pause when the window is minimized",
        "",
        Switch,
    ),
    item(
        General,
        "Playback",
        "prevent-screensaver",
        "Keep the screen awake while playing",
        "",
        Switch,
    ),
    item(
        General,
        "History",
        "record-history",
        "Remember playback history",
        "",
        Switch,
    ),
    item(
        General,
        "History",
        "recent-limit",
        "Recent files to show",
        "",
        number(0.0, 50.0, 1.0, ""),
    ),
    item(
        General,
        "Screenshots",
        "screenshot-dir",
        "Save screenshots to",
        "Empty for your Pictures folder.",
        Path(true),
    ),
    item(
        General,
        "Screenshots",
        "screenshot-format",
        "Format",
        "",
        Choice(&[
            ("png", "PNG"),
            ("jpg", "JPEG"),
            ("webp", "WebP"),
            ("jxl", "JPEG XL"),
            ("avif", "AVIF"),
        ]),
    ),
    item(
        General,
        "Screenshots",
        "screenshot-subtitles",
        "Include subtitles",
        "",
        Switch,
    ),
    // Interface.
    item(
        Interface,
        "Window",
        "titlebar",
        "Title bar",
        "Applies to new windows.",
        Choice(&[
            ("auto", "Platform default"),
            ("custom", "Patina's, over the video"),
            ("system", "The system's"),
        ]),
    ),
    item(
        Interface,
        "Window",
        "resize-window",
        "Fit the window to the video",
        "",
        Choice(&[
            ("on-open", "When media is opened"),
            ("always", "For every file"),
            ("never", "Never"),
        ]),
    ),
    item(
        Interface,
        "Window",
        "window-size",
        "Window size",
        "",
        Choice(&[
            ("fit", "Fit the screen"),
            ("0.5", "Half the video size"),
            ("1", "The video size"),
            ("1.5", "1.5× the video size"),
            ("2", "Double the video size"),
        ]),
    ),
    item(
        Interface,
        "Window",
        "always-on-top",
        "Float on top",
        "",
        Switch,
    ),
    item(
        Interface,
        "On-screen controller",
        "osc-timeout",
        "Hide the controls after",
        "",
        number(0.5, 30.0, 0.5, "s"),
    ),
    item(
        Interface,
        "On-screen controller",
        "arrow-action",
        "The ◀◀ and ▶▶ buttons",
        "",
        Choice(&[
            ("speed", "Change the speed"),
            ("playlist", "Go to the previous or next item"),
            ("seek", "Seek 10 seconds"),
        ]),
    ),
    item(
        Interface,
        "On-screen controller",
        "toolbar.pip",
        "Picture in picture button",
        "",
        Switch,
    ),
    item(
        Interface,
        "On-screen controller",
        "toolbar.playlist",
        "Playlist button",
        "",
        Switch,
    ),
    item(
        Interface,
        "On-screen controller",
        "toolbar.settings",
        "Quick settings button",
        "",
        Switch,
    ),
    item(
        Interface,
        "On-screen controller",
        "toolbar.subtitles",
        "Subtitles button",
        "",
        Switch,
    ),
    item(
        Interface,
        "On-screen controller",
        "toolbar.screenshot",
        "Screenshot button",
        "",
        Switch,
    ),
    item(
        Interface,
        "On-screen controller",
        "toolbar.fullscreen",
        "Full screen button",
        "",
        Switch,
    ),
    item(
        Interface,
        "On-screen controller",
        "show-chapter-marks",
        "Show chapters on the seek bar",
        "",
        Switch,
    ),
    item(
        Interface,
        "On-screen controller",
        "show-remaining-time",
        "Show the remaining time",
        "",
        Switch,
    ),
    item(
        Interface,
        "On-screen display",
        "osd-enabled",
        "Show on-screen messages",
        "",
        Switch,
    ),
    needs(
        item(
            Interface,
            "On-screen display",
            "osd-timeout",
            "Hide messages after",
            "",
            number(0.3, 10.0, 0.1, "s"),
        ),
        "osd-enabled",
    ),
    needs(
        item(
            Interface,
            "On-screen display",
            "osd-font-size",
            "Text size",
            "",
            number(8.0, 80.0, 1.0, "pt"),
        ),
        "osd-enabled",
    ),
    item(
        Interface,
        "Seek-bar thumbnails",
        "thumbnails",
        "Show thumbnails while hovering the seek bar",
        "Made in the background for local videos.",
        Switch,
    ),
    needs(
        item(
            Interface,
            "Seek-bar thumbnails",
            "thumbnail-width",
            "Thumbnail width",
            "",
            number(40.0, 600.0, 10.0, "px"),
        ),
        "thumbnails",
    ),
    needs(
        item(
            Interface,
            "Seek-bar thumbnails",
            "thumbnail-cache-mb",
            "Largest thumbnail cache",
            "",
            number(0.0, 100000.0, 50.0, "MB"),
        ),
        "thumbnails",
    ),
    // Video.
    item(
        Video,
        "Decoding",
        "hwdec",
        "Hardware decoding",
        "",
        Choice(&[
            ("auto-safe", "Automatic"),
            (
                "auto-copy-safe",
                "Automatic, copying frames back (works with filters)",
            ),
            ("no", "Off"),
        ]),
    ),
    item(
        Video,
        "Decoding",
        "deinterlace",
        "Deinterlacing",
        "",
        Choice(&[
            ("no", "Off"),
            ("auto", "When the video is interlaced"),
            ("yes", "Always"),
        ]),
    ),
    item(
        Video,
        "HDR",
        "tone-mapping",
        "Tone mapping",
        "How HDR video is fitted to the display.",
        Choice(&[
            ("auto", "Automatic"),
            ("bt.2390", "BT.2390"),
            ("bt.2446a", "BT.2446a"),
            ("spline", "Spline"),
            ("hable", "Hable"),
            ("mobius", "Möbius"),
            ("reinhard", "Reinhard"),
            ("st2094-40", "ST 2094-40"),
            ("st2094-10", "ST 2094-10"),
            ("clip", "Clip"),
        ]),
    ),
    item(
        Video,
        "HDR",
        "target-peak",
        "Display peak brightness",
        "0 lets mpv decide.",
        number(0.0, 10000.0, 10.0, "nits"),
    ),
    // Audio.
    item(
        Audio,
        "Volume",
        "use-initial-volume",
        "Always start at the same volume",
        "Otherwise Patina starts at the last volume.",
        Switch,
    ),
    needs(
        item(
            Audio,
            "Volume",
            "initial-volume",
            "Start volume",
            "",
            number(0.0, 1000.0, 1.0, "%"),
        ),
        "use-initial-volume",
    ),
    item(
        Audio,
        "Volume",
        "max-volume",
        "Maximum volume",
        "Above 100% amplifies.",
        number(100.0, 1000.0, 10.0, "%"),
    ),
    item(
        Audio,
        "Output",
        "audio-device",
        "Audio device",
        "",
        Control::DynamicChoice,
    ),
    item(
        Audio,
        "Output",
        "gapless-audio",
        "Gapless playback",
        "",
        Choice(&[
            ("weak", "When the formats match"),
            ("yes", "Always"),
            ("no", "Off"),
        ]),
    ),
    item(
        Audio,
        "Output",
        "audio-language",
        "Preferred languages",
        "Comma-separated codes, e.g. ja,en.",
        Text,
    ),
    item(
        Audio,
        "ReplayGain",
        "replaygain",
        "ReplayGain",
        "",
        Choice(&[
            ("no", "Off"),
            ("track", "Track gain"),
            ("album", "Album gain"),
        ]),
    ),
    item(
        Audio,
        "ReplayGain",
        "replaygain-preamp",
        "Pre-amplification",
        "",
        number(-15.0, 15.0, 0.5, "dB"),
    ),
    item(
        Audio,
        "ReplayGain",
        "replaygain-clip",
        "Allow clipping",
        "",
        Switch,
    ),
    // Subtitles.
    item(
        Subtitles,
        "Loading",
        "sub-auto",
        "Load subtitle files",
        "",
        Choice(&[
            ("smart", "That match the video, by name or episode"),
            ("fuzzy", "Whose names contain the video's"),
            ("exact", "With exactly the video's name"),
            ("all", "Every subtitle file in the folder"),
            ("no", "Don't load automatically"),
        ]),
    ),
    item(
        Subtitles,
        "Loading",
        "sub-language",
        "Preferred languages",
        "Comma-separated codes, e.g. en,ja.",
        Text,
    ),
    item(
        Subtitles,
        "Loading",
        "sub-codepage",
        "Text encoding",
        "",
        Choice(ENCODINGS),
    ),
    item(
        Subtitles,
        "Online subtitles",
        "opensubtitles-api-key",
        "OpenSubtitles API key",
        "Make one free in your opensubtitles.com profile.",
        Text,
    ),
    item(
        Subtitles,
        "Online subtitles",
        "opensubtitles-username",
        "OpenSubtitles account",
        "Optional; signed in, you can download more a day.",
        Text,
    ),
    item(
        Subtitles,
        "Online subtitles",
        "opensubtitles-password",
        "Password",
        "Saved on Enter, in the system's password store.",
        Password,
    ),
    item(
        Subtitles,
        "Online subtitles",
        "online-sub-auto",
        "Search online when a video has no subtitles",
        "For videos of 20 minutes or more; loads one made for the file.",
        Switch,
    ),
    item(
        Subtitles,
        "Text style",
        "sub-font",
        "Font",
        "Any installed font name.",
        Text,
    ),
    item(
        Subtitles,
        "Text style",
        "sub-font-size",
        "Size",
        "",
        number(10.0, 150.0, 1.0, ""),
    ),
    item(Subtitles, "Text style", "sub-bold", "Bold", "", Switch),
    item(Subtitles, "Text style", "sub-italic", "Italic", "", Switch),
    item(
        Subtitles,
        "Text style",
        "sub-color",
        "Text color",
        "#RRGGBB or #AARRGGBB.",
        Text,
    ),
    item(
        Subtitles,
        "Text style",
        "sub-border-color",
        "Outline color",
        "",
        Text,
    ),
    item(
        Subtitles,
        "Text style",
        "sub-border-size",
        "Outline width",
        "",
        number(0.0, 10.0, 0.25, ""),
    ),
    item(
        Subtitles,
        "Text style",
        "sub-shadow-offset",
        "Shadow offset",
        "",
        number(0.0, 10.0, 0.5, ""),
    ),
    item(
        Subtitles,
        "Text style",
        "sub-back-color",
        "Background color",
        "",
        Text,
    ),
    item(
        Subtitles,
        "Position",
        "sub-pos",
        "Vertical position",
        "100 is the bottom.",
        number(0.0, 150.0, 1.0, "%"),
    ),
    item(
        Subtitles,
        "Position",
        "sub-scale-by-window",
        "Scale with the window",
        "",
        Switch,
    ),
    item(
        Subtitles,
        "Position",
        "sub-use-margins",
        "Place subtitles in the black bars",
        "",
        Switch,
    ),
    item(
        Subtitles,
        "Position",
        "sub-ass-override",
        "Styled (ASS) subtitles",
        "How much of the settings above applies to them.",
        Choice(&[
            ("scale", "Apply the scale only"),
            ("no", "Keep their own style"),
            ("yes", "Apply the font and size"),
            ("force", "Apply all text settings"),
            ("strip", "Remove their styling"),
        ]),
    ),
    // Network.
    item(
        Network,
        "Cache",
        "cache",
        "Buffer network streams",
        "",
        Switch,
    ),
    needs(
        item(
            Network,
            "Cache",
            "cache-secs",
            "Seconds to buffer ahead",
            "",
            number(1.0, 100000.0, 10.0, "s"),
        ),
        "cache",
    ),
    needs(
        item(
            Network,
            "Cache",
            "demuxer-max-mb",
            "Buffer size",
            "",
            number(1.0, 4096.0, 10.0, "MB"),
        ),
        "cache",
    ),
    needs(
        item(
            Network,
            "Cache",
            "cache-pause-initial",
            "Buffer before starting playback",
            "",
            Switch,
        ),
        "cache",
    ),
    needs(
        item(
            Network,
            "Cache",
            "cache-pause-wait",
            "Seconds to buffer before resuming",
            "",
            number(0.0, 60.0, 0.5, "s"),
        ),
        "cache",
    ),
    item(
        Network,
        "Connection",
        "user-agent",
        "User agent",
        "Empty for mpv's.",
        Text,
    ),
    item(
        Network,
        "Connection",
        "http-proxy",
        "HTTP proxy",
        "e.g. http://proxy:8080",
        Text,
    ),
    item(
        Network,
        "Connection",
        "rtsp-transport",
        "RTSP transport",
        "",
        Choice(&[
            ("tcp", "TCP"),
            ("udp", "UDP"),
            ("http", "HTTP"),
            ("lavf", "FFmpeg's default"),
        ]),
    ),
    item(
        Network,
        "Online video",
        "ytdl",
        "Play videos from websites with yt-dlp",
        "yt-dlp must be installed.",
        Switch,
    ),
    needs(
        item(
            Network,
            "Online video",
            "ytdl-path",
            "yt-dlp program",
            "Empty to find it on the PATH.",
            Path(false),
        ),
        "ytdl",
    ),
    needs(
        item(
            Network,
            "Online video",
            "ytdl-quality",
            "Video quality",
            "Each video's own choices are in Video › Video Quality.",
            Choice(&[
                ("best", "Best available"),
                ("2160", "Up to 2160p (4K)"),
                ("1440", "Up to 1440p"),
                ("1080", "Up to 1080p"),
                ("720", "Up to 720p"),
                ("480", "Up to 480p"),
                ("360", "Up to 360p"),
            ]),
        ),
        "ytdl",
    ),
    needs(
        item(
            Network,
            "Online video",
            "ytdl-format",
            "yt-dlp format",
            "In yt-dlp's -f syntax; takes the place of the quality when set.",
            Text,
        ),
        "ytdl",
    ),
    needs(
        item(
            Network,
            "Online video",
            "download-dir",
            "Save downloads to",
            "Empty for your Downloads folder.",
            Path(true),
        ),
        "ytdl",
    ),
    needs(
        item(
            Network,
            "Online video",
            "ytdl-raw-options",
            "yt-dlp options",
            "key=value pairs, comma-separated.",
            Text,
        ),
        "ytdl",
    ),
    // Control.
    item(
        Page::Control,
        "Mouse",
        "single-click",
        "Click",
        "",
        Choice(CLICK_ACTIONS),
    ),
    item(
        Page::Control,
        "Mouse",
        "double-click",
        "Double click",
        "",
        Choice(CLICK_ACTIONS),
    ),
    item(
        Page::Control,
        "Mouse",
        "right-click",
        "Right click",
        "",
        Choice(CLICK_ACTIONS),
    ),
    item(
        Page::Control,
        "Mouse",
        "middle-click",
        "Middle click",
        "",
        Choice(CLICK_ACTIONS),
    ),
    item(
        Page::Control,
        "Mouse",
        "vertical-scroll",
        "Scroll up and down",
        "",
        Choice(SCROLL_ACTIONS),
    ),
    item(
        Page::Control,
        "Mouse",
        "horizontal-scroll",
        "Scroll sideways",
        "",
        Choice(SCROLL_ACTIONS),
    ),
    item(
        Page::Control,
        "Seeking and volume",
        "seek-mode",
        "Seeking by key",
        "",
        Choice(&[
            ("keyframes", "Fast (to the nearest keyframe)"),
            ("exact", "Exact"),
        ]),
    ),
    item(
        Page::Control,
        "Seeking and volume",
        "seek-step",
        "Scroll seek step",
        "",
        number(0.5, 600.0, 0.5, "s"),
    ),
    item(
        Page::Control,
        "Seeking and volume",
        "volume-step",
        "Scroll volume step",
        "",
        number(0.5, 20.0, 0.5, "%"),
    ),
    item(
        Page::Control,
        "System",
        "media-keys",
        "Use media keys and the desktop's player controls",
        "",
        Switch,
    ),
    // Advanced.
    item(
        Advanced,
        "mpv",
        "use-mpv-config",
        "Load mpv.conf and scripts from Patina's mpv folder",
        "Applies to new windows.",
        Switch,
    ),
    item(
        Advanced,
        "mpv",
        "open-mpv-folder",
        "Open the mpv Folder",
        "mpv.conf, scripts/, script-opts/ and fonts/ go here.",
        Button,
    ),
    item(
        Advanced,
        "mpv",
        "mpv-options",
        "Extra mpv options",
        "One name=value per line; applies to new windows.",
        Lines,
    ),
    // Utilities. Each system has some of the file type buttons (see
    // available).
    item(
        Utilities,
        "File types",
        "files-video",
        "Open Videos with Patina",
        "Makes Patina the default for video files and patina:// links.",
        Button,
    ),
    item(
        Utilities,
        "File types",
        "files-audio",
        "Open Music with Patina",
        "Makes Patina the default for audio files.",
        Button,
    ),
    item(
        Utilities,
        "File types",
        "files-playlists",
        "Open Playlists with Patina",
        "Makes Patina the default for M3U, PLS and CUE files.",
        Button,
    ),
    item(
        Utilities,
        "File types",
        "files-register",
        "Add Patina to Open With",
        "For video, audio and playlist files, and patina:// links.",
        Button,
    ),
    item(
        Utilities,
        "File types",
        "files-defaults",
        "Choose Default Apps…",
        "Windows has you choose Patina there yourself.",
        Button,
    ),
    item(
        Utilities,
        "File types",
        "files-remove",
        "Remove Patina's File Types",
        if cfg!(windows) {
            "Do this before deleting Patina's folder."
        } else {
            "Removes the desktop entry Patina added."
        },
        Button,
    ),
    item(
        Utilities,
        "Data",
        "clear-history",
        "Clear Playback History",
        "Also clears the recent files.",
        Button,
    ),
    item(
        Utilities,
        "Data",
        "clear-resume",
        "Clear Saved Positions",
        "Forgets where every file was left off.",
        Button,
    ),
    item(
        Utilities,
        "Data",
        "clear-thumbnails",
        "Clear Thumbnail Cache",
        "",
        Button,
    ),
    item(
        Utilities,
        "Settings",
        "open-config-folder",
        "Open the Settings Folder",
        "",
        Button,
    ),
    item(
        Utilities,
        "Settings",
        "restore-defaults",
        "Restore Default Settings",
        "Key binding sets are kept.",
        Button,
    ),
];

/// Whether this system has the row: the file type buttons differ.
pub fn available(item: &Item) -> bool {
    match item.key {
        "files-video" | "files-audio" | "files-playlists" => !cfg!(windows),
        "files-register" | "files-defaults" => cfg!(windows),
        "files-remove" => !cfg!(target_os = "macos"),
        _ => true,
    }
}

/// The toolbar buttons, in the order they appear.
pub const TOOLBAR: &[&str] = &[
    "pip",
    "playlist",
    "settings",
    "subtitles",
    "screenshot",
    "fullscreen",
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Preferences;

    #[test]
    fn every_setting_names_a_real_preference() {
        let json = serde_json::to_value(Preferences::default()).unwrap();
        for item in ITEMS {
            if matches!(item.control, Button | Password) || item.key.starts_with("toolbar.") {
                continue;
            }
            assert!(
                json.get(item.key).is_some(),
                "{} is not a preference",
                item.key
            );
            if let Some(required) = item.requires {
                assert!(
                    json.get(required).is_some_and(|v| v.is_boolean()),
                    "{required}"
                );
            }
            if let Choice(choices) = item.control {
                let value = json[item.key].as_str().unwrap_or_default();
                assert!(
                    choices.iter().any(|(choice, _)| *choice == value),
                    "{}'s default {value:?} is not one of its choices",
                    item.key
                );
            }
        }
        assert!(
            Preferences::default()
                .toolbar_buttons
                .iter()
                .all(|b| TOOLBAR.contains(&b.as_str()))
        );
    }
}
