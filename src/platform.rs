//! Hand-offs to the desktop: file managers and browsers.

use std::path::Path;
use std::process::Command;

fn spawn(command: &mut Command) {
    if let Err(error) = command.spawn() {
        log::warn!("could not run {:?}: {error}", command.get_program());
    }
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
        spawn(Command::new("cmd").args(["/C", "start", "", url]));
    } else {
        spawn(Command::new("xdg-open").arg(url));
    }
}

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
