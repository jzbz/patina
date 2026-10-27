//! Command-line arguments.

#[derive(Debug, Default, PartialEq)]
pub struct Options {
    pub sources: Vec<String>,
    pub enqueue: bool,
    pub fullscreen: bool,
    pub verbose: bool,
    /// `--mpv-NAME=VALUE` options passed straight to mpv, as with iina-cli.
    pub mpv_options: Vec<(String, String)>,
}

#[derive(Debug, PartialEq)]
pub enum Command {
    Run(Options),
    Help,
    Version,
}

pub const HELP: &str = "\
Patina — a media player for Linux, macOS and Windows, powered by mpv

Usage: patina [OPTIONS] [FILE | URL | -] ...

Options:
  -e, --enqueue          Add the inputs to the playlist of the newest window
  -f, --fullscreen       Start in fullscreen
      --mpv-NAME=VALUE   Set an mpv option (e.g. --mpv-hwdec=no)
  -v, --verbose          Log mpv and Patina diagnostics to stderr
  -h, --help             Print this help
  -V, --version          Print the version, and which libmpv loads

Use -- before inputs that start with '-'. A lone '-' reads from standard input.
";

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Command, String> {
    let mut options = Options::default();
    let mut only_inputs = false;
    for arg in args {
        if only_inputs || arg == "-" || !arg.starts_with('-') {
            options.sources.push(resolve(arg));
            continue;
        }
        match arg.as_str() {
            "--" => only_inputs = true,
            "-h" | "--help" => return Ok(Command::Help),
            "-V" | "--version" => return Ok(Command::Version),
            "-e" | "--enqueue" => options.enqueue = true,
            "-f" | "--fullscreen" => options.fullscreen = true,
            "-v" | "--verbose" => options.verbose = true,
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
    Ok(Command::Run(options))
}

/// Makes local paths absolute, so they still work from another window.
fn resolve(source: String) -> String {
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
            "--mpv-hwdec=no",
            "--mpv-fs",
            "https://example.com/v.m3u8",
            "-",
        ]);
        assert!(options.fullscreen);
        assert_eq!(
            options.mpv_options,
            [("hwdec".into(), "no".into()), ("fs".into(), "yes".into())]
        );
        assert_eq!(options.sources, ["https://example.com/v.m3u8", "-"]);
    }

    #[test]
    fn double_dash_ends_options_and_paths_become_absolute() {
        let options = run(&["--", "-odd.mkv"]);
        assert!(std::path::Path::new(&options.sources[0]).is_absolute());
        assert!(options.sources[0].ends_with("-odd.mkv"));
    }

    #[test]
    fn unknown_options_and_help() {
        assert!(parse(["--nope".to_string()]).is_err());
        assert_eq!(parse(["-h".to_string()]), Ok(Command::Help));
        assert_eq!(parse(["--version".to_string()]), Ok(Command::Version));
    }
}
