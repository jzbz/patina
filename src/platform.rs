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

/// Whether Patina runs in a Flatpak sandbox, where it reaches the desktop
/// through the portals. Told by `/.flatpak-info`, which Flatpak puts in
/// every sandbox and the portals themselves go by, rather than by
/// FLATPAK_ID, which as an environment variable can be handed on to a
/// program started outside the sandbox.
pub fn flatpak() -> bool {
    static FLATPAK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    cfg!(all(unix, not(target_os = "macos")))
        && *FLATPAK.get_or_init(|| Path::new("/.flatpak-info").exists())
}

/// Moves files to the trash (the Recycle Bin on Windows) on a thread of its
/// own; failures are logged.
/// Whether Patina may move the file to the trash, or why not. Only a Flatpak
/// says no up front: its Trash portal moves only what the sandbox may write,
/// and follows a link to the file it points at rather than moving the link.
pub fn can_trash(path: &str) -> Result<(), String> {
    #[cfg(all(unix, not(target_os = "macos")))]
    if flatpak() {
        let link = std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink());
        if link {
            return Err("a link can't be moved to the trash from the Flatpak".into());
        }
        let c_path = std::ffi::CString::new(path).map_err(|error| error.to_string())?;
        // SAFETY: a NUL-terminated path that outlives the call.
        if unsafe { libc::access(c_path.as_ptr(), libc::W_OK) } != 0 {
            return Err("Patina may only read it in the Flatpak".into());
        }
    }
    let _ = path;
    Ok(())
}

/// Calls `done`, on the thread that moved them, with the files that
/// couldn't be moved and why.
pub fn move_to_trash(
    paths: Vec<String>,
    done: impl FnOnce(Vec<(String, String)>) + Send + 'static,
) {
    let spawned = std::thread::Builder::new()
        .name("trash".into())
        .spawn(move || {
            // The trash crate would find only the sandbox's own trash, in
            // the app's private data folder, which no file manager shows.
            #[cfg(all(unix, not(target_os = "macos")))]
            if flatpak() {
                done(bus::trash(&paths));
                return;
            }
            let mut failed = Vec::new();
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
                        failed.push((path, error.to_string()));
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
            }
            done(failed);
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
        reveal_on_bus(path.to_path_buf());
    }
}

/// Linux: the FileManager1 D-Bus interface can select the file, and most
/// file managers implement it; otherwise the folder opens. It runs on a
/// thread of its own, since the file manager may have to start first.
#[cfg(all(unix, not(target_os = "macos")))]
fn reveal_on_bus(path: PathBuf) {
    let spawned = std::thread::Builder::new()
        .name("reveal".into())
        .spawn(move || {
            if let Err(error) = zbus::block_on(bus::show_item(&path)) {
                log::debug!(
                    "could not select {} in a file manager: {error}",
                    path.display()
                );
                if let Some(parent) = path.parent() {
                    open_path(parent);
                }
            }
        });
    if let Err(error) = spawned {
        log::warn!("could not start showing the file: {error}");
    }
}

#[cfg(not(all(unix, not(target_os = "macos"))))]
fn reveal_on_bus(_path: PathBuf) {}

/// Linux: the calls that go over the session bus.
#[cfg(all(unix, not(target_os = "macos")))]
mod bus {
    use std::path::Path;
    use zbus::Connection;

    pub async fn show_item(path: &Path) -> zbus::Result<()> {
        let connection = Connection::session().await?;
        connection
            .call_method(
                Some("org.freedesktop.FileManager1"),
                "/org/freedesktop/FileManager1",
                Some("org.freedesktop.FileManager1"),
                "ShowItems",
                // The file, and no startup notification ID.
                &(vec![super::file_uri(path)], ""),
            )
            .await?;
        Ok(())
    }

    /// Moves files to the user's trash through the desktop portal, which
    /// takes a descriptor for each file rather than its path. Returns the
    /// files it couldn't move, and why.
    pub fn trash(paths: &[String]) -> Vec<(String, String)> {
        let trashed = zbus::block_on(async {
            let connection = Connection::session().await?;
            let mut failed = Vec::new();
            for path in paths {
                if let Err(error) = trash_file(&connection, path).await {
                    log::warn!("could not move {path} to the trash: {error}");
                    failed.push((path.clone(), error));
                }
            }
            zbus::Result::Ok(failed)
        });
        trashed.unwrap_or_else(|error| {
            log::warn!("could not move files to the trash: {error}");
            paths
                .iter()
                .map(|path| (path.clone(), error.to_string()))
                .collect()
        })
    }

    async fn trash_file(connection: &Connection, path: &str) -> Result<(), String> {
        use std::os::unix::fs::OpenOptionsExt;
        // O_PATH, which neither reads nor writes the file. The portal then
        // checks through the descriptor that the sandbox could write the
        // file, as it can't where Patina's access is read-only.
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH)
            .open(path)
            .map_err(|error| error.to_string())?;
        let reply = connection
            .call_method(
                Some("org.freedesktop.portal.Desktop"),
                "/org/freedesktop/portal/desktop",
                Some("org.freedesktop.portal.Trash"),
                "TrashFile",
                &(zbus::Fd::from(&file),),
            )
            .await
            .map_err(|error| error.to_string())?;
        // 1 is success, 0 failure; why, the portal logs only for itself.
        match reply.body().deserialize::<u32>() {
            Ok(1) => Ok(()),
            Ok(_) => Err("the portal refused, as it does for a file Patina may only read".into()),
            Err(error) => Err(error.to_string()),
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
