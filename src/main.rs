// Release builds on Windows are GUI applications without a console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod cli;
mod history;
mod inhibit;
mod input;
mod inspector;
mod media_controls;
mod mpv;
mod platform;
mod player;
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

fn main() -> ExitCode {
    let options = match cli::parse(std::env::args().skip(1)) {
        Ok(cli::Command::Run(options)) => options,
        Ok(cli::Command::Help) => {
            print!("{}", cli::HELP);
            return ExitCode::SUCCESS;
        }
        Ok(cli::Command::Version) => {
            println!("patina {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("patina: {error}\n\n{}", cli::HELP);
            return ExitCode::from(2);
        }
    };

    let default_level = if options.verbose { "debug" } else { "warn" };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(default_level))
        .init();

    // mpv draws through OpenGL, so Slint must render with OpenGL too.
    if let Err(error) = slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("femtovg".into())
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
        Err(error) => {
            eprintln!("patina: {error}");
            ExitCode::FAILURE
        }
    }
}
