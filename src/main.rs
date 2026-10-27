// Release builds on Windows are GUI applications without a console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod cli;
mod history;
mod inhibit;
mod input;
mod inspector;
#[cfg(target_os = "macos")]
mod macos;
mod media_controls;
mod mpv;
mod platform;
mod player;
mod playlist;
mod preferences;
mod probe;
mod settings;
mod subtitle_match;
mod thumbnails;
mod util;
mod video;
mod window;

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
        Err(error) => {
            eprintln!("patina: {error}\n\n{}", cli::HELP);
            return ExitCode::from(2);
        }
    };

    let default_level = if options.verbose { "debug" } else { "warn" };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(default_level))
        .init();

    // mpv draws through OpenGL, so Slint renders with OpenGL too, unless
    // this is the software fallback (see the end of this function).
    let software = video::software::enabled();
    if let Err(error) = slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name(if software { "software" } else { "femtovg" }.into())
        .with_winit_custom_application_handler(CloseOnExit)
        .select()
    {
        eprintln!("patina: could not start the user interface: {error}");
        return ExitCode::FAILURE;
    }

    if let Err(error) = mpv::ffi::api() {
        eprintln!("patina: {error}");
        return ExitCode::FAILURE;
    }

    let app = app::App::new(
        settings::Paths::discover(),
        options.verbose,
        options.mpv_options,
    );
    {
        let app = app.clone();
        let sources = options.sources;
        let (enqueue, fullscreen) = (options.enqueue, options.fullscreen);
        slint::Timer::single_shot(std::time::Duration::ZERO, move || {
            let window = if sources.is_empty() {
                let window = app.new_window();
                if let Some(window) = &window
                    && app.prefs().launch_action == "open-dialog"
                {
                    window.run_action("open-file");
                }
                window
            } else {
                app.open_sources(sources, enqueue)
            };
            if fullscreen && let Some(window) = window {
                window.run_action("toggle-fullscreen");
            }
        });
    }

    let result = slint::run_event_loop_until_quit();
    app.quit();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        // The window could not be drawn with OpenGL: a machine without a GPU
        // driver, a virtual machine or a remote desktop. Slint picks its
        // renderer once per process, so start again with the software one.
        Err(error) if !software => {
            log::warn!("OpenGL failed ({error}); starting again without it");
            relaunch_without_opengl(&error.to_string())
        }
        Err(error) => {
            eprintln!("patina: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Closes the windows, which saves where each file was, however the event
/// loop ends. On macOS the app menu's Quit, which Slint adds, ends the
/// process from inside the event loop, so nothing after it would run.
struct CloseOnExit;

impl slint::winit_030::CustomApplicationHandler for CloseOnExit {
    fn exiting(
        &mut self,
        _event_loop: &slint::winit_030::winit::event_loop::ActiveEventLoop,
    ) -> slint::winit_030::EventResult {
        app::App::with_current(|app| app.quit());
        slint::winit_030::EventResult::Propagate
    }
}

/// Runs Patina again, with the same arguments, in software rendering.
fn relaunch_without_opengl(error: &str) -> ExitCode {
    let spawned = std::env::current_exe().and_then(|exe| {
        std::process::Command::new(exe)
            .args(std::env::args_os().skip(1))
            .env("PATINA_RENDERER", "software")
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
    /// by tools/menubar.py; this fails until it is rerun after a change.
    #[test]
    fn menu_bar_matches_the_menu() {
        let menus = include_str!("../ui/menus.slint");
        let app = include_str!("../ui/app.slint");
        let start = menus.find("export component MainMenu").unwrap();
        let start = start + menus[start..].find("    Menu {\n").unwrap() + "    Menu {\n".len();
        let end = start + menus[start..].find("\n    }\n}").unwrap();
        let expected = format!("{}\n", &menus[start..end]);

        let begin = "        // BEGIN tools/menubar.py\n";
        let begin = app.find(begin).unwrap() + begin.len();
        let end = app.find("        // END tools/menubar.py\n").unwrap();
        assert!(
            app[begin..end] == expected,
            "ui/app.slint's menu bar differs from ui/menus.slint; run tools/menubar.py"
        );
    }
}
