// Release builds on Windows are GUI applications without a console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod associations;
mod backend;
mod cli;
#[cfg(all(unix, not(target_os = "macos")))]
mod clipboard;
mod credentials;
mod downloads;
mod fonts;
mod formats;
mod history;
mod inhibit;
mod input;
mod inspector;
mod instance;
mod jellyfin;
#[cfg(target_os = "macos")]
mod macos;
mod media_controls;
mod mpv;
mod opensubtitles;
mod platform;
mod player;
mod playlist;
mod preferences;
mod probe;
mod scheme;
mod settings;
mod subtitle_match;
mod thumbnails;
mod util;
mod video;
#[cfg(all(unix, not(target_os = "macos")))]
mod wayland;
mod window;
mod youtube;
mod ytdl;

use std::process::ExitCode;

slint::include_modules!();

/// Patina's website, from Cargo.toml.
pub const WEBSITE: &str = env!("CARGO_PKG_HOMEPAGE");
/// Patina's application ID: patina.video, reversed, plus the name. It names
/// the settings folder on macOS, and the desktop entry the system media
/// controls point at on Linux.
pub const APP_ID: &str = "video.patina.Patina";

fn main() -> ExitCode {
    let options = match cli::parse(std::env::args().skip(1)) {
        Ok(cli::Command::Run(options)) => options,
        Ok(cli::Command::Help) => {
            print!("{}", cli::HELP);
            return ExitCode::SUCCESS;
        }
        Ok(cli::Command::Version) => {
            println!("patina {}", env!("CARGO_PKG_VERSION"));
            // Which libmpv a build would play with, which is what a check of
            // a packaged build wants to know.
            return match mpv::describe() {
                Ok(libmpv) => {
                    println!("{libmpv}");
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    println!("libmpv: {error}");
                    ExitCode::FAILURE
                }
            };
        }
        Ok(cli::Command::RemoveFileTypes) => return remove_file_types(),
        Err(error) => {
            eprintln!("patina: {error}\n\n{}", cli::HELP);
            return ExitCode::from(2);
        }
    };

    let default_level = if options.verbose { "debug" } else { "warn" };
    Logger::new(env_logger::Env::default().default_filter_or(default_level)).init();

    // A Patina already running opens what this one was asked to, and this
    // one is done. Standard input can't be handed over.
    let paths = settings::Paths::discover();
    let mut request = options.request();
    let primary = if options.new_instance || options.reads_stdin() {
        None
    } else {
        match instance::claim(&paths.config, &request, !started_by_launch_services()) {
            instance::Claim::Primary(primary) => Some(primary),
            instance::Claim::Forwarded => return ExitCode::SUCCESS,
            instance::Claim::Standalone(reason) => {
                log::warn!("{reason}; starting a separate Patina");
                None
            }
        }
    };

    // Ended by a signal (Ctrl+C, the terminal closing, the session ending),
    // Patina still closes its windows, which saves where each file was. It
    // quits from inside the event loop, as the Quit menu item does, so that
    // a request from another launch already waiting there is refused rather
    // than opened in a window about to close.
    if let Err(error) = ctrlc::set_handler(|| {
        let quit = slint::invoke_from_event_loop(|| app::App::with_current(|app| app.quit()));
        if quit.is_err() {
            let _ = slint::quit_event_loop();
        }
    }) {
        log::warn!("could not handle termination signals: {error}");
    }

    // mpv draws through OpenGL, so Slint renders with OpenGL too, unless
    // this is the software fallback (see the end of this function).
    let software = video::software::enabled();
    if let Err(error) = backend::select(software, CloseOnExit) {
        eprintln!("patina: could not start the user interface: {error}");
        return ExitCode::FAILURE;
    }
    // Names the windows after the desktop entry, which is how Linux desktops
    // give them its icon and name.
    if let Err(error) = slint::set_xdg_app_id(APP_ID) {
        log::debug!("no application ID: {error}");
    }
    #[cfg(target_os = "macos")]
    macos::listen_for_opens();
    // Requests from later launches go to the interface's thread.
    let server = primary.map(|primary| {
        primary.serve(|request, reply| {
            slint::invoke_from_event_loop(move || {
                app::App::with_current(|app| {
                    if reply.take() {
                        reply.send(app.open_request(request));
                    }
                });
            })
            .is_ok()
        })
    });

    if let Err(error) = mpv::ffi::api() {
        eprintln!("patina: {error}");
        return ExitCode::FAILURE;
    }

    // This Patina's own mpv options apply to every window it opens.
    let app = app::App::new(
        paths,
        options.verbose,
        std::mem::take(&mut request.mpv_options),
    );
    if let Some(server) = server {
        app.set_instance(server);
    }
    {
        let app = app.clone();
        slint::Timer::single_shot(std::time::Duration::ZERO, move || app.launch(request));
    }

    let mut result = slint::run_event_loop_until_quit();
    // While winit's display is still open, which it is until Slint's
    // platform goes, as this function returns.
    #[cfg(all(unix, not(target_os = "macos")))]
    clipboard::shut_down();
    app.quit();
    if result.is_ok()
        && let Some(reason) = video::software::restart_reason()
    {
        result = Err(slint::PlatformError::Other(reason.into()));
    }
    match result {
        Ok(()) => ExitCode::SUCCESS,
        // The window could not be drawn with OpenGL: a machine without a GPU
        // driver, a virtual machine or a remote desktop. Slint picks its
        // renderer once per process, so start again with the software one.
        Err(error) if !software => {
            log::warn!("OpenGL failed ({error}); starting again without it");
            let status = relaunch_without_opengl(&error.to_string());
            // What other launches and the system sent this Patina goes to
            // the one starting in its place.
            let accepted = app.accepted_requests();
            if status == ExitCode::SUCCESS
                && !accepted.is_empty()
                && let Err(error) = instance::hand_over(&app.paths().config, &accepted)
            {
                log::warn!("could not hand over what was opened: {error}");
            }
            status
        }
        Err(error) => {
            eprintln!("patina: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Closes the windows, which saves where each file was, however the event
/// loop ends. On macOS the app menu's Quit, which Slint adds, ends the
/// process from inside the event loop, so nothing after it would run. On
/// Linux it tells the Wayland clipboard about the keyboard focus too.
struct CloseOnExit;

impl slint::winit_030::CustomApplicationHandler for CloseOnExit {
    /// Tells the Wayland clipboard which window has the keyboard focus: it
    /// starts as the first gets it, a moment before the first copy or paste
    /// can come, and says why a paste got nothing in the one that has it
    /// (see clipboard.rs).
    #[cfg(all(unix, not(target_os = "macos")))]
    fn window_event(
        &mut self,
        _event_loop: &slint::winit_030::winit::event_loop::ActiveEventLoop,
        window_id: slint::winit_030::winit::window::WindowId,
        winit_window: Option<&slint::winit_030::winit::window::Window>,
        _slint_window: Option<&slint::Window>,
        event: &slint::winit_030::winit::event::WindowEvent,
    ) -> slint::winit_030::EventResult {
        use slint::winit_030::winit::event::WindowEvent;
        match (event, winit_window) {
            (WindowEvent::Focused(true), Some(window)) => clipboard::focused(window),
            (WindowEvent::Focused(false) | WindowEvent::Destroyed, _) => {
                clipboard::unfocused(window_id);
            }
            _ => {}
        }
        slint::winit_030::EventResult::Propagate
    }

    /// Lets the Wayland clipboard see, once a batch of events is through,
    /// whether the focus has left Patina's windows, after which another
    /// program may own what is on it; the focus only moving from one of
    /// them to another comes as a loss and a gain in the same batch.
    #[cfg(all(unix, not(target_os = "macos")))]
    fn about_to_wait(
        &mut self,
        _event_loop: &slint::winit_030::winit::event_loop::ActiveEventLoop,
    ) -> slint::winit_030::EventResult {
        clipboard::settle();
        slint::winit_030::EventResult::Propagate
    }

    fn exiting(
        &mut self,
        _event_loop: &slint::winit_030::winit::event_loop::ActiveEventLoop,
    ) -> slint::winit_030::EventResult {
        app::App::with_current(|app| app.quit());
        slint::winit_030::EventResult::Propagate
    }
}

/// Removes what Preferences › Utilities registered for this user's file
/// types, as its Remove button does, without a window or a word with a
/// running Patina. The Windows uninstaller runs it, so that nothing is left
/// pointing at the deleted executable. On macOS there is nothing to remove:
/// the defaults name the app by its bundle ID, not by a path, and the
/// system passes over an app that is gone.
fn remove_file_types() -> ExitCode {
    #[cfg(not(target_os = "macos"))]
    match associations::remove() {
        Ok(done) => println!("{done}"),
        Err(error) => {
            eprintln!("patina: {error}");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

/// macOS: whether LaunchServices started this Patina, as `open -n` does, or
/// a Finder open while another copy of Patina runs. What it is to open then
/// arrives as Apple events after startup, too late to hand to the running
/// Patina, so it opens here.
fn started_by_launch_services() -> bool {
    cfg!(target_os = "macos")
        && std::env::var_os("__CFBundleIdentifier").is_some_and(|id| id == APP_ID)
}

/// Runs Patina again, with the same arguments, in software rendering.
fn relaunch_without_opengl(error: &str) -> ExitCode {
    let spawned = std::env::current_exe().and_then(|exe| {
        std::process::Command::new(exe)
            .args(std::env::args_os().skip(1))
            .env("PATINA_RENDERER", "software")
            // Started by this Patina, not by LaunchServices: it takes what
            // this one hands over.
            .env_remove("__CFBundleIdentifier")
            .spawn()
    });
    match spawned {
        Ok(_) => ExitCode::SUCCESS,
        Err(spawn_error) => {
            eprintln!("patina: {error}; could not start again without OpenGL: {spawn_error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    /// The macOS menu bar in app.slint is a copy of MainMenu's menus, made
    /// by tools/menubar.py (reading Playback and Media where the menus read
    /// MenuPlayback and MenuMedia); this fails until it is rerun after a
    /// change.
    #[test]
    fn menu_bar_matches_the_menu() {
        // A Windows checkout has CRLF line endings.
        let menus = include_str!("../ui/menus.slint").replace("\r\n", "\n");
        let app = include_str!("../ui/app.slint").replace("\r\n", "\n");
        let start = menus.find("export component MainMenu").unwrap();
        let start = start + menus[start..].find("    Menu {\n").unwrap() + "    Menu {\n".len();
        let end = start + menus[start..].find("\n    }\n}").unwrap();
        let expected = format!(
            "{}\n",
            menus[start..end]
                .replace("MenuMedia.playlist-count", "Media.playlist.length")
                .replace("MenuPlayback.", "Playback.")
                .replace("MenuMedia.", "Media.")
        );

        let begin = "        // BEGIN tools/menubar.py\n";
        let begin = app.find(begin).unwrap() + begin.len();
        let end = app.find("        // END tools/menubar.py\n").unwrap();
        assert!(
            app[begin..end] == expected,
            "ui/app.slint's menu bar differs from ui/menus.slint; run tools/menubar.py"
        );
    }
}

/// The logger, from `RUST_LOG` or the level given, with ureq's and
/// ureq-proto's trace output left out whatever it says: that output is the
/// raw HTTP exchange, every `Authorization` header (a Jellyfin token) and
/// every sign-in's answer included. At debug ureq hides headers and paths,
/// so that much still comes through when asked for.
struct Logger(env_logger::Logger);

impl Logger {
    fn new(env: env_logger::Env) -> Self {
        Self(env_logger::Builder::from_env(env).build())
    }

    fn init(self) {
        let level = self.0.filter();
        if log::set_boxed_logger(Box::new(self)).is_ok() {
            log::set_max_level(level);
        }
    }
}

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        let target = metadata.target();
        let http = ["ureq", "ureq_proto"].iter().any(|name| {
            target
                .strip_prefix(name)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with("::"))
        });
        !(http && metadata.level() > log::Level::Debug) && self.0.enabled(metadata)
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            self.0.log(record);
        }
    }

    fn flush(&self) {
        self.0.flush();
    }
}

#[cfg(test)]
mod logger_tests {
    use log::{Level, Log, Metadata};

    fn enabled(filter: &str, target: &str, level: Level) -> bool {
        let env = env_logger::Env::new().filter_or("PATINA_TEST_NO_SUCH_VARIABLE", filter);
        super::Logger::new(env).enabled(&Metadata::builder().target(target).level(level).build())
    }

    #[test]
    fn http_traces_never_reach_the_log() {
        for filter in ["trace", "ureq_proto=trace,ureq=trace", "patina=trace,trace"] {
            assert!(
                !enabled(filter, "ureq_proto::util", Level::Trace),
                "{filter}"
            );
            assert!(
                !enabled(filter, "ureq::unversioned::transport", Level::Trace),
                "{filter}"
            );
            assert!(enabled(filter, "ureq::run", Level::Debug), "{filter}");
        }
        assert!(enabled("trace", "patina::jellyfin", Level::Trace));
        assert!(
            enabled("trace", "ureqx", Level::Trace),
            "only ureq's own modules"
        );
        assert!(
            !enabled("warn", "ureq::run", Level::Debug),
            "the cap allows nothing more"
        );
    }
}
