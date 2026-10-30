//! Command-line arguments.

use crate::instance::Request;

#[derive(Debug, Default, PartialEq)]
pub struct Options {
    pub sources: Vec<String>,
    pub enqueue: bool,
    pub separate_windows: bool,
    pub fullscreen: bool,
    pub pip: bool,
    pub music_mode: bool,
    pub verbose: bool,
    pub new_instance: bool,
    /// `--mpv-NAME=VALUE` options passed straight to mpv, as with iina-cli.
    pub mpv_options: Vec<(String, String)>,
}

#[derive(Debug, PartialEq)]
pub enum Command {
    Run(Options),
    Help,
    Version,
    /// `--remove-file-types`, which the Windows uninstaller runs, and which
    /// HELP leaves out.
    RemoveFileTypes,
}

pub const HELP: &str = "\
Patina — a media player for Linux, macOS and Windows, powered by mpv

Usage: patina [OPTIONS] [FILE | URL | -] ...

Options:
  -e, --enqueue           Add the inputs to the playlist of the newest window
  -w, --separate-windows  Open each input in a window of its own
  -f, --fullscreen        Play in full screen
      --pip               Play in picture-in-picture
      --music-mode        Play in music mode
      --mpv-NAME=VALUE    Set an mpv option (e.g. --mpv-hwdec=no)
      --new-instance      Start another Patina rather than use the running one
  -v, --verbose           Log mpv and Patina diagnostics to stderr
  -h, --help              Print this help
  -V, --version           Print the version, and which libmpv loads

While Patina is running, the inputs open in it. Use -- before inputs that
start with '-'. A lone '-' reads from standard input, in a Patina of its own.
";

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Command, String> {
    let mut options = Options::default();
    let mut only_inputs = false;
    let mut link: Option<Vec<String>> = None;
    for arg in args {
        if only_inputs || arg == "-" || !arg.starts_with('-') {
            match &mut link {
                Some(parts) => parts.push(arg),
                None => options.sources.push(resolve(arg)),
            }
            continue;
        }
        match arg.as_str() {
            "--" => only_inputs = true,
            "-h" | "--help" => return Ok(Command::Help),
            "-V" | "--version" => return Ok(Command::Version),
            "--remove-file-types" => return Ok(Command::RemoveFileTypes),
            "-e" | "--enqueue" => options.enqueue = true,
            "-w" | "--separate-windows" => options.separate_windows = true,
            "-f" | "--fullscreen" => options.fullscreen = true,
            "--pip" => options.pip = true,
            "--music-mode" => options.music_mode = true,
            "-v" | "--verbose" => options.verbose = true,
            "--new-instance" => options.new_instance = true,
            // How Windows starts Patina for a patina:// link: what follows
            // is the link, never files, even if a quote in it split it into
            // several arguments.
            "--open-link" => link = Some(Vec::new()),
            // The process serial number old versions of macOS gave an app
            // that Finder opened.
            _ if cfg!(target_os = "macos") && arg.starts_with("-psn_") => {}
            _ => match arg.strip_prefix("--mpv-") {
                Some(option) => {
                    let (name, value) = option.split_once('=').unwrap_or((option, "yes"));
                    if name.is_empty() {
                        return Err(format!("missing mpv option name in {arg}"));
                    }
                    options
                        .mpv_options
                        .push((name.to_owned(), value.to_owned()));
                }
                None => return Err(format!("unknown option {arg}")),
            },
        }
    }
    // As iina-cli has it; music mode has no full screen either.
    if options.music_mode && (options.pip || options.fullscreen) {
        return Err("--music-mode can't be used with --pip or --fullscreen".into());
    }
    if let Some(parts) = link {
        let link = parts.join(" ");
        if !options.sources.is_empty() || !crate::scheme::is_link(&link) {
            return Err("--open-link takes one patina:// link".into());
        }
        options.sources = vec![link];
    }
    Ok(Command::Run(options))
}

impl Options {
    /// What to open, as a request to this Patina or the running one.
    pub fn request(&self) -> Request {
        Request {
            sources: self.sources.clone(),
            enqueue: self.enqueue,
            new_window: false,
            separate_windows: self.separate_windows,
            fullscreen: self.fullscreen,
            pip: self.pip,
            music_mode: self.music_mode,
            mpv_options: self.mpv_options.clone(),
            // Lets the running Patina come to the front on Wayland.
            activation_token: std::env::var("XDG_ACTIVATION_TOKEN")
                .ok()
                .filter(|token| !token.is_empty()),
        }
    }

    /// Standard input can't be handed to another process.
    pub fn reads_stdin(&self) -> bool {
        self.sources.iter().any(|source| source == "-")
    }
}

/// Makes local paths absolute, so they still work from another window or
/// another process, and file:// URIs, which file managers pass, paths.
fn resolve(source: String) -> String {
    if let Some(path) = crate::playlist::file_uri_path(&source) {
        return path;
    }
    if source == "-" || crate::util::is_url(&source) {
        return source;
    }
    std::path::absolute(&source)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or(source)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Options {
        match parse(args.iter().map(|arg| arg.to_string())).unwrap() {
            Command::Run(options) => options,
            other => panic!("expected options, got {other:?}"),
        }
    }

    #[test]
    fn flags_inputs_and_mpv_options() {
        let options = run(&[
            "-f",
            "-w",
            "--pip",
            "--new-instance",
            "--mpv-hwdec=no",
            "--mpv-fs",
            "https://example.com/v.m3u8",
            "-",
        ]);
        assert!(options.fullscreen && options.separate_windows && options.pip);
        assert!(options.new_instance && options.reads_stdin());
        assert_eq!(
            options.mpv_options,
            [("hwdec".into(), "no".into()), ("fs".into(), "yes".into())]
        );
        assert_eq!(options.sources, ["https://example.com/v.m3u8", "-"]);
        let request = options.request();
        assert_eq!(request.sources, options.sources);
        assert!(request.separate_windows && !request.new_window);
    }

    #[test]
    fn double_dash_ends_options_and_paths_become_absolute() {
        let options = run(&["--", "-odd.mkv", "patina://open?url=https://a"]);
        assert!(std::path::Path::new(&options.sources[0]).is_absolute());
        assert!(options.sources[0].ends_with("-odd.mkv"));
        assert_eq!(options.sources[1], "patina://open?url=https://a");
    }

    #[test]
    fn file_uris_become_paths() {
        let options = run(&["--", "file:///media/a%20b.mkv"]);
        assert_eq!(options.sources, ["/media/a b.mkv"]);
    }

    #[test]
    fn a_link_from_windows_stays_one_link() {
        let options = run(&[
            "--open-link",
            "--",
            "patina://open?url=https://x",
            r"\\evil\share\a.mkv",
        ]);
        assert_eq!(
            options.sources,
            [r"patina://open?url=https://x \\evil\share\a.mkv"]
        );
        assert!(parse(["--open-link", "--", "/etc/passwd"].map(String::from)).is_err());
    }

    #[test]
    fn music_mode_goes_alone() {
        let options = run(&["--music-mode", "a.flac"]);
        assert!(options.music_mode && options.request().music_mode);
        for other in ["--pip", "-f"] {
            assert!(parse(["--music-mode", other].map(String::from)).is_err());
        }
    }

    #[test]
    fn removing_file_types_is_a_command_of_its_own() {
        assert_eq!(
            parse(["--remove-file-types".to_string()]),
            Ok(Command::RemoveFileTypes)
        );
        assert!(!HELP.contains("--remove-file-types"));
        // After --, it is a file like any other.
        let options = run(&["--", "--remove-file-types"]);
        assert!(options.sources[0].ends_with("--remove-file-types"));
    }

    #[test]
    fn unknown_options_and_help() {
        assert!(parse(["--nope".to_string()]).is_err());
        assert_eq!(parse(["-h".to_string()]), Ok(Command::Help));
        assert_eq!(parse(["--version".to_string()]), Ok(Command::Version));
    }
}
