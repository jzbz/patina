//! Hand-offs to the desktop: file managers and browsers, and other
//! programs Patina runs.

use std::path::{Path, PathBuf};
use std::process::Command;

fn spawn(command: &mut Command) {
    if let Err(error) = no_console(command).spawn() {
        log::warn!("could not run {:?}: {error}", command.get_program());
    }
}

/// Keeps a console program Patina runs (`cmd`, yt-dlp) from opening a
/// console window on Windows, where Patina itself has none.
pub fn no_console(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// Finds a program as a shell would: a path as it is, a bare name on the
/// `PATH`. On Windows `.exe` is added to a name without it.
pub fn find_program(name: &str) -> Option<PathBuf> {
    let name = if cfg!(windows) && !name.to_ascii_lowercase().ends_with(".exe") {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    let path = Path::new(&name);
    if path.components().count() > 1 {
        return path.is_file().then(|| path.to_path_buf());
    }
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(path))
        .find(|candidate| candidate.is_file())
}

/// Moves files to the trash (the Recycle Bin on Windows) on a thread of its
/// own; failures are logged.
pub fn move_to_trash(paths: Vec<String>) {
    let spawned = std::thread::Builder::new()
        .name("trash".into())
        .spawn(move || {
            #[allow(unused_mut)]
            let mut context = trash::TrashContext::default();
            // Finder's own method asks the user to let Patina control Finder.
            #[cfg(target_os = "macos")]
            {
                use trash::macos::{DeleteMethod, TrashContextExtMacos};
                context.set_delete_method(DeleteMethod::NsFileManager);
            }
            for path in paths {
                // mpv may still be closing a file it was playing, and
                // Windows won't move an open file.
                let mut attempts = 0;
                while let Err(error) = context.delete(&path) {
                    attempts += 1;
                    if attempts == 10 {
                        log::warn!("could not move {path} to the trash: {error}");
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
            }
        });
    if let Err(error) = spawned {
        log::warn!("could not start moving files to the trash: {error}");
    }
}

/// Opens a file or directory with its default application.
pub fn open_path(path: &Path) {
    if cfg!(target_os = "macos") {
        spawn(Command::new("open").arg(path));
    } else if cfg!(target_os = "windows") {
        spawn(Command::new("explorer").arg(path));
    } else {
        spawn(Command::new("xdg-open").arg(path));
    }
}

pub fn open_url(url: &str) {
    if cfg!(target_os = "macos") {
        spawn(Command::new("open").arg(url));
    } else if cfg!(target_os = "windows") {
        open_url_windows(url);
    } else {
        spawn(Command::new("xdg-open").arg(url));
    }
}

/// Through the shell's own call, not cmd's `start`, which would read `&`
/// and the like in a URL as more commands.
#[cfg(windows)]
fn open_url_windows(url: &str) {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let wide = |text: &str| -> Vec<u16> { text.encode_utf16().chain(std::iter::once(0)).collect() };
    let (operation, file) = (wide("open"), wide(url));
    // SAFETY: NUL-terminated strings that outlive the call.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // Values up to 32 are errors.
    if result as usize <= 32 {
        log::warn!("could not open {url:?}: error {}", result as usize);
    }
}

#[cfg(not(windows))]
fn open_url_windows(_url: &str) {}

/// Shows a file selected in the file manager.
pub fn reveal(path: &Path) {
    if cfg!(target_os = "macos") {
        spawn(Command::new("open").arg("-R").arg(path));
    } else if cfg!(target_os = "windows") {
        let mut select = std::ffi::OsString::from("/select,");
        select.push(path);
        spawn(Command::new("explorer").arg(select));
    } else {
        // The FileManager1 D-Bus interface can select the file; dbus-send is
        // part of every desktop that implements it. Otherwise open the folder.
        let uri = file_uri(path);
        let shown = Command::new("dbus-send")
            .args([
                "--session",
                "--dest=org.freedesktop.FileManager1",
                "--type=method_call",
                "/org/freedesktop/FileManager1",
                "org.freedesktop.FileManager1.ShowItems",
                &format!("array:string:{uri}"),
                "string:",
            ])
            .status()
            .is_ok_and(|status| status.success());
        if !shown && let Some(parent) = path.parent() {
            open_path(parent);
        }
    }
}

/// A `file://` URI with everything but unreserved characters and `/`
/// percent-encoded.
pub fn file_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            uri.push(byte as char);
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri
}

#[cfg(test)]
mod tests {
    #[test]
    fn file_uris_are_percent_encoded() {
        assert_eq!(
            super::file_uri(std::path::Path::new("/a b/c#1.mkv")),
            "file:///a%20b/c%231.mkv"
        );
    }
}
