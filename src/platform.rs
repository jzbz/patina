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

/// Starts a program so that `kill_group` reaches what it starts as well. On
/// Windows the process tree already says that.
pub fn in_own_group(command: &mut Command) -> &mut Command {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
}

/// How long a program `end_group` asked to end is given before `kill_group`.
/// yt-dlp itself ends at once; this is for the launcher of a standalone yt-dlp
/// to remove what it unpacked, and for an ffmpeg to close its file.
pub const END_GRACE: std::time::Duration = std::time::Duration::from_millis(1500);

/// Asks a program started with `in_own_group`, and whatever it has started, to
/// end, and gives whether it asked, in which case the caller gives it
/// `END_GRACE` and then calls `kill_group` if it is still running. On Unix
/// that is SIGTERM to the group. It matters for the standalone yt-dlp the
/// macOS package carries: a PyInstaller launcher that unpacks the real
/// yt-dlp into a folder of its own in $TMPDIR (about 85 MB) on every start.
/// Its launcher passes a signal it can catch on to that yt-dlp, waits for it
/// to end and removes the folder; SIGKILL can't be caught, and leaves the
/// folder behind. On Windows there is nothing known to ask with: taskkill
/// without /F refuses a console program with no window, which is how Patina
/// starts yt-dlp, and the launcher's one window, a hidden one, handles only
/// the session ending; so this kills the group at once and gives false (see
/// packaging/windows/README.md). Call it, like
/// `kill_group`, before the program has been waited for.
pub fn end_group(child: &mut std::process::Child) -> bool {
    #[cfg(unix)]
    {
        if let Ok(id) = libc::pid_t::try_from(child.id()) {
            // SAFETY: kill takes no pointers. A negative id names the group,
            // which is this child's own, as `in_own_group` made it.
            if unsafe { libc::kill(-id, libc::SIGTERM) } == 0 {
                return true;
            }
        }
        kill_group(child);
        false
    }
    #[cfg(not(unix))]
    {
        kill_group(child);
        false
    }
}

/// `end_group`, then `kill_group` if the program is still running after
/// `END_GRACE`; for a caller that may wait that long, as a worker thread may.
/// One that ended within it has been waited for, so its group is left alone:
/// its id may already be someone else's.
pub fn stop_group(child: &mut std::process::Child) {
    if !end_group(child) {
        return;
    }
    let until = std::time::Instant::now() + END_GRACE;
    while std::time::Instant::now() < until {
        if !matches!(child.try_wait(), Ok(None)) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    if matches!(child.try_wait(), Ok(None)) {
        kill_group(child);
    }
}

/// Stops a program started with `in_own_group`, and whatever it has started: a
/// launcher's real worker, or a JavaScript runtime, would otherwise go on making
/// requests, and hold the program's pipes open. Call it before waiting for the
/// program, while its id can't be anyone else's.
pub fn kill_group(child: &mut std::process::Child) {
    #[cfg(unix)]
    if let Ok(id) = libc::pid_t::try_from(child.id()) {
        // SAFETY: kill takes no pointers. A negative id names the group, which
        // is this child's own, as `in_own_group` made it.
        unsafe {
            libc::kill(-id, libc::SIGKILL);
        }
    }
    // From the system's own folder, not whatever the PATH finds first.
    #[cfg(windows)]
    if let Some(root) = std::env::var_os("SystemRoot") {
        let mut command = Command::new(PathBuf::from(root).join("System32").join("taskkill.exe"));
        command
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if let Err(error) = no_console(&mut command).status() {
            log::warn!("could not stop a process tree: {}", error.kind());
        }
    }
    let _ = child.kill();
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

/// A program the macOS or Windows package carries for Patina: `yt-dlp` or
/// `ffmpeg`, in Patina.app/Contents/Helpers or beside patina.exe. None where
/// it isn't there, or on Linux, where a build plays with the system's
/// programs and the Flatpak's are on its PATH.
///
/// On macOS it is found from the executable itself rather than a link to it,
/// as libmpv is, since the Homebrew cask's `patina` command is a link. On
/// Windows the path is left as Windows gives it: resolving it would turn it
/// into a `\\?\` path, which then reaches mpv's hook and yt-dlp, and shows.
pub fn bundled(name: &str) -> Option<PathBuf> {
    #[cfg(any(target_os = "macos", windows))]
    {
        let exe = std::env::current_exe().ok()?;
        #[cfg(target_os = "macos")]
        let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
        bundled_beside(&exe, name).filter(|path| path.is_file())
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = name;
        None
    }
}

/// Where the package puts `name`, given Patina's executable: Contents/Helpers
/// of the bundle (Apple's place for helper tools) on macOS, the executable's
/// own folder on Windows.
#[cfg(any(target_os = "macos", windows, test))]
fn bundled_beside(exe: &Path, name: &str) -> Option<PathBuf> {
    let dir = exe.parent()?;
    if cfg!(windows) {
        Some(dir.join(format!("{name}.exe")))
    } else {
        Some(dir.parent()?.join("Helpers").join(name))
    }
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

/// Whether this is Linux without a PipeWire server to connect to. When
/// mpv's PipeWire audio output fails to connect, PipeWire's cleanup can
/// wait forever for its own thread (`pw_thread_loop_stop`, seen with
/// libpipewire 1.6), and mpv's core with it, and with that Patina's window.
/// mpv tries that output before every file unless told otherwise, and
/// lists audio devices through every output it has, whatever `ao` says.
/// The Flatpak's libmpv has no PipeWire output.
pub fn pipewire_missing() -> bool {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        !flatpak()
            && pipewire_socket(|name| std::env::var_os(name)).is_none_or(|path| !path.exists())
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    false
}

/// Where a PipeWire client would connect, as libpipewire finds it:
/// `PIPEWIRE_REMOTE` (a path, or a name in the runtime folder), else
/// `pipewire-0`, in `PIPEWIRE_RUNTIME_DIR` or else `XDG_RUNTIME_DIR`.
#[cfg(all(unix, not(target_os = "macos")))]
fn pipewire_socket(var: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    let set = |name: &str| var(name).filter(|value| !value.is_empty());
    let remote = set("PIPEWIRE_REMOTE").unwrap_or_else(|| "pipewire-0".into());
    if Path::new(&remote).is_absolute() {
        return Some(PathBuf::from(remote));
    }
    let dir = set("PIPEWIRE_RUNTIME_DIR").or_else(|| set("XDG_RUNTIME_DIR"))?;
    Some(Path::new(&dir).join(remote))
}

static WINDOWS_FLOAT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Whether a window can float on top of the others. A Wayland window can't:
/// Wayland leaves that to the compositor alone, and winit ignores the
/// request there. Assumed until a window shows otherwise.
pub fn windows_float() -> bool {
    WINDOWS_FLOAT.load(std::sync::atomic::Ordering::Relaxed)
}

/// A window turned out to be a Wayland window.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn windows_cannot_float() {
    WINDOWS_FLOAT.store(false, std::sync::atomic::Ordering::Relaxed);
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
    use std::path::{Path, PathBuf};

    /// Contents/Helpers on macOS, beside the exe on Windows; Linux has no
    /// bundled programs at all.
    #[test]
    fn bundled_programs_are_looked_for_where_the_packages_put_them() {
        if cfg!(windows) {
            assert_eq!(
                super::bundled_beside(Path::new(r"C:\Apps\Patina\patina.exe"), "yt-dlp"),
                Some(PathBuf::from(r"C:\Apps\Patina\yt-dlp.exe"))
            );
        } else {
            assert_eq!(
                super::bundled_beside(
                    Path::new("/Applications/Patina.app/Contents/MacOS/patina"),
                    "ffmpeg"
                ),
                Some(PathBuf::from(
                    "/Applications/Patina.app/Contents/Helpers/ffmpeg"
                ))
            );
        }
        if cfg!(not(any(target_os = "macos", windows))) {
            assert_eq!(super::bundled("yt-dlp"), None);
            assert_eq!(super::bundled("ffmpeg"), None);
        }
    }

    /// A program asked to end gets to run what it does on SIGTERM, as a
    /// standalone yt-dlp's launcher removes what it unpacked; one that won't
    /// end is killed once the grace is up.
    #[cfg(unix)]
    #[test]
    fn a_group_is_asked_to_end_and_then_killed() {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};
        let dir = std::env::temp_dir().join(format!("patina-stop-group-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let start = |script: &str| {
            let ready = dir.join("ready");
            let _ = std::fs::remove_file(&ready);
            let mut command = Command::new("sh");
            command
                .arg("-c")
                .arg(script)
                .arg(&dir)
                .stdout(Stdio::null());
            super::in_own_group(&mut command);
            let child = command.spawn().unwrap();
            let started = Instant::now();
            while !ready.exists() {
                assert!(started.elapsed() < Duration::from_secs(10), "never ready");
                std::thread::sleep(Duration::from_millis(10));
            }
            child
        };

        let unpacked = dir.join("unpacked");
        std::fs::write(&unpacked, b"").unwrap();
        let mut child =
            start("trap 'rm -f \"$0/unpacked\"; exit 0' TERM; sleep 30 & touch \"$0/ready\"; wait");
        super::stop_group(&mut child);
        let status = child.wait().unwrap();
        assert!(status.success(), "{status:?}: killed rather than asked");
        assert!(!unpacked.exists(), "it did not get to tidy up");

        let mut child = start("trap '' TERM; sleep 30 & touch \"$0/ready\"; wait");
        let started = Instant::now();
        super::stop_group(&mut child);
        let status = child.wait().unwrap();
        assert!(!status.success(), "{status:?}");
        assert!(
            started.elapsed() >= super::END_GRACE,
            "killed before the grace"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn the_pipewire_socket_is_found_as_libpipewire_finds_it() {
        use std::ffi::OsString;
        use std::path::PathBuf;
        let socket = |vars: &[(&str, &str)]| {
            let vars: Vec<(String, OsString)> = vars
                .iter()
                .map(|(name, value)| ((*name).to_owned(), OsString::from(value)))
                .collect();
            super::pipewire_socket(|name| {
                vars.iter()
                    .find(|(wanted, _)| wanted == name)
                    .map(|(_, value)| value.clone())
            })
        };
        assert_eq!(
            socket(&[("XDG_RUNTIME_DIR", "/run/user/1000")]),
            Some(PathBuf::from("/run/user/1000/pipewire-0"))
        );
        assert_eq!(
            socket(&[
                ("XDG_RUNTIME_DIR", "/run/user/1000"),
                ("PIPEWIRE_RUNTIME_DIR", "/run/pipewire"),
                ("PIPEWIRE_REMOTE", "other"),
            ]),
            Some(PathBuf::from("/run/pipewire/other"))
        );
        assert_eq!(
            socket(&[("PIPEWIRE_REMOTE", "/tmp/pw.sock"), ("XDG_RUNTIME_DIR", "")]),
            Some(PathBuf::from("/tmp/pw.sock"))
        );
        // Empty values count as unset; with no folder there is no socket.
        assert_eq!(
            socket(&[("PIPEWIRE_REMOTE", ""), ("XDG_RUNTIME_DIR", "")]),
            None
        );
        assert_eq!(socket(&[]), None);
    }

    #[test]
    fn file_uris_are_percent_encoded() {
        assert_eq!(
            super::file_uri(std::path::Path::new("/a b/c#1.mkv")),
            "file:///a%20b/c%231.mkv"
        );
    }
}
