//! Registering Patina for its file types and patina:// links, as IINA's
//! "Set IINA as the default application" does. Each system wants something
//! else: Linux a desktop entry and defaults in mimeapps.list, macOS a word
//! with LaunchServices (the bundle declares its types itself), Windows keys
//! among the user's own classes, after which the user picks Patina in
//! Settings, since Windows lets no app make itself the default.

use crate::formats::{self, Kind};

/// The kind of file, as a sentence names it.
#[cfg(not(windows))]
fn plural(kind: Kind) -> &'static str {
    match kind {
        Kind::Video => "videos",
        Kind::Audio => "music",
        Kind::Playlist => "playlists",
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
pub use linux::{remove, set_default};
#[cfg(target_os = "macos")]
pub use macos::set_default;
#[cfg(windows)]
pub use windows::{open_default_apps, register, remove};

/// The running executable, as the system should start it: as it was
/// started, so that a mapped drive or a folder link that names it stays in
/// the path.
#[cfg(not(target_os = "macos"))]
fn executable() -> Result<String, String> {
    let exe = std::env::current_exe()
        .map_err(|error| format!("could not find Patina's executable: {error}"))?;
    // Linux names a replaced executable "… (deleted)".
    if !exe.exists() {
        return Err("Patina was replaced since it started; start it again first.".into());
    }
    Ok(exe.to_string_lossy().into_owned())
}

#[cfg(all(unix, not(target_os = "macos")))]
mod linux {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    const ENTRY: &str = include_str!("../packaging/linux/video.patina.Patina.desktop");
    const ICON: &[u8] = include_bytes!("../ui/icons/patina.svg");
    /// Marks an entry Patina wrote, which it may then replace or remove.
    const MARK: &str = "# Written by Patina's preferences, which remove it again.";

    /// Where desktop entries go and are found, and what the commands Patina
    /// runs should see; a test's own folders in tests.
    pub struct Dirs {
        pub data_home: PathBuf,
        pub data_dirs: Vec<PathBuf>,
        pub env: Vec<(&'static str, Option<String>)>,
    }

    impl Dirs {
        fn system() -> Result<Self, String> {
            let base = directories::BaseDirs::new().ok_or("no home folder")?;
            let data_dirs = std::env::var("XDG_DATA_DIRS")
                .ok()
                .filter(|dirs| !dirs.is_empty())
                .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
            Ok(Self {
                data_home: base.data_dir().to_path_buf(),
                data_dirs: std::env::split_paths(&data_dirs).collect(),
                env: Vec::new(),
            })
        }

        fn entry(&self) -> PathBuf {
            self.data_home
                .join("applications")
                .join(format!("{}.desktop", crate::APP_ID))
        }

        /// An environment variable as xdg-mime will see it.
        fn variable(&self, name: &str) -> Option<String> {
            match self.env.iter().find(|(set, _)| *set == name) {
                Some((_, value)) => value.clone(),
                None => std::env::var(name).ok(),
            }
            .filter(|value| !value.is_empty())
        }

        /// The desktop xdg-mime will see.
        fn desktop(&self) -> String {
            self.variable("XDG_CURRENT_DESKTOP").unwrap_or_default()
        }

        fn icon(&self) -> PathBuf {
            self.data_home
                .join("icons/hicolor/scalable/apps")
                .join(format!("{}.svg", crate::APP_ID))
        }

        fn run(&self, program: &str, args: &[&str]) -> Result<(), String> {
            let mut command = Command::new(program);
            command.args(args);
            for (name, value) in &self.env {
                match value {
                    Some(value) => command.env(name, value),
                    None => command.env_remove(name),
                };
            }
            match command.output() {
                Ok(output) if output.status.success() => Ok(()),
                Ok(output) => Err(format!(
                    "{program} failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                )),
                Err(error) => Err(format!("could not run {program}: {error}")),
            }
        }
    }

    /// Makes Patina the default app for files of `kind`, and for links.
    pub fn set_default(kind: Kind) -> Result<String, String> {
        if Path::new("/.flatpak-info").exists() {
            return Err("Choose Patina in the system's default applications settings.".into());
        }
        set_default_in(&Dirs::system()?, kind, &executable()?)
    }

    pub fn set_default_in(dirs: &Dirs, kind: Kind, exe: &str) -> Result<String, String> {
        let name = format!("{}.desktop", crate::APP_ID);
        // A packaged Patina's own entry, or else one of Patina's writing,
        // started the way this Patina was.
        let packaged = dirs
            .data_dirs
            .iter()
            .any(|dir| dir.join("applications").join(&name).is_file());
        let entry = dirs.entry();
        if packaged {
            // One written for another Patina earlier would hide the package's.
            if is_ours(&entry) {
                remove_in(dirs)?;
            }
        } else if !entry.exists() || is_ours(&entry) {
            write_entry(dirs, exe)?;
        }
        let mut types = formats::mime_types(&[kind]);
        types.push("x-scheme-handler/patina");
        // xdg-mime stops after the first type under LXQt, which it also
        // finds by LXQT_SESSION_CONFIG.
        let lxqt = dirs
            .desktop()
            .split(':')
            .any(|desktop| desktop.eq_ignore_ascii_case("LXQt"))
            || dirs.variable("LXQT_SESSION_CONFIG").is_some();
        let batches: Vec<&[&str]> = if lxqt {
            types.chunks(1).collect()
        } else {
            vec![&types]
        };
        for batch in batches {
            let mut args = vec!["default", name.as_str()];
            args.extend(batch);
            dirs.run("xdg-mime", &args)?;
        }
        Ok(format!("Patina now opens {}.", plural(kind)))
    }

    fn is_ours(entry: &Path) -> bool {
        std::fs::read_to_string(entry).is_ok_and(|text| text.starts_with(MARK))
    }

    fn write_entry(dirs: &Dirs, exe: &str) -> Result<(), String> {
        if exe.contains(['\n', '\r']) {
            return Err("Patina's path has a line break in it.".into());
        }
        // Exec's quotes take a backslash before " ` $ and \, and then the
        // desktop entry's string escape doubles every backslash, as TryExec's.
        let escaped = exe.replace('\\', "\\\\");
        let quoting = exe
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('`', "\\`")
            .replace('$', "\\$");
        let quoted = format!("\"{}\"", quoting.replace('\\', "\\\\").replace('%', "%%"));
        let mut text = format!("{MARK}\n");
        for line in ENTRY.lines() {
            match line {
                "TryExec=patina" => text.push_str(&format!("TryExec={escaped}")),
                "Exec=patina -- %U" => text.push_str(&format!("Exec={quoted} -- %U")),
                line => text.push_str(line),
            }
            text.push('\n');
        }
        let write = |path: &Path, contents: &[u8]| {
            std::fs::create_dir_all(path.parent().unwrap_or(path))
                .and_then(|()| std::fs::write(path, contents))
                .map_err(|error| format!("could not write {}: {error}", path.display()))
        };
        write(&dirs.icon(), ICON)?;
        let entry = dirs.entry();
        write(&entry, text.as_bytes())?;
        refresh(dirs, &entry);
        Ok(())
    }

    /// The desktop learns of what changed in the applications folder.
    fn refresh(dirs: &Dirs, entry: &Path) {
        if let Some(applications) = entry.parent()
            && let Err(error) = dirs.run(
                "update-desktop-database",
                &[&applications.to_string_lossy()],
            )
        {
            log::debug!("{error}");
        }
    }

    /// Removes the desktop entry and icon Patina wrote.
    pub fn remove() -> Result<String, String> {
        remove_in(&Dirs::system()?)
    }

    pub fn remove_in(dirs: &Dirs) -> Result<String, String> {
        let entry = dirs.entry();
        if !is_ours(&entry) {
            return Ok("Patina had added nothing.".into());
        }
        std::fs::remove_file(&entry)
            .map_err(|error| format!("could not remove {}: {error}", entry.display()))?;
        let icon = dirs.icon();
        if std::fs::read(&icon).is_ok_and(|contents| contents == ICON) {
            let _ = std::fs::remove_file(&icon);
        }
        refresh(dirs, &entry);
        Ok("Removed the desktop entry Patina had added.".into())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn writes_an_entry_sets_defaults_and_removes_it() {
            let Some(_) = crate::platform::find_program("xdg-mime") else {
                return;
            };
            let root = std::env::temp_dir()
                .join(format!("patina-associations-test-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let config = root.join("config");
            std::fs::create_dir_all(&config).unwrap();
            let dirs = Dirs {
                data_home: root.join("data"),
                data_dirs: vec![root.join("system")],
                // xdg-mime then writes only the generic mimeapps.list.
                env: vec![
                    ("XDG_CONFIG_HOME", Some(config.to_string_lossy().into())),
                    (
                        "XDG_DATA_HOME",
                        Some(root.join("data").to_string_lossy().into()),
                    ),
                    (
                        "XDG_DATA_DIRS",
                        Some(root.join("system").to_string_lossy().into()),
                    ),
                    ("XDG_CURRENT_DESKTOP", None),
                    ("KDE_SESSION_VERSION", None),
                    ("DESKTOP_SESSION", None),
                    ("LXQT_SESSION_CONFIG", None),
                ],
            };
            let exe = "/opt/My Apps/$patina";
            set_default_in(&dirs, Kind::Video, exe).unwrap();
            let entry = std::fs::read_to_string(dirs.entry()).unwrap();
            assert!(
                entry.contains("\nExec=\"/opt/My Apps/\\\\$patina\" -- %U\n"),
                "{entry}"
            );
            assert!(entry.contains("\nTryExec=/opt/My Apps/$patina\n"));
            let defaults = std::fs::read_to_string(config.join("mimeapps.list")).unwrap();
            assert!(defaults.contains("video/matroska=video.patina.Patina.desktop"));
            assert!(defaults.contains("x-scheme-handler/patina=video.patina.Patina.desktop"));
            assert!(!defaults.contains("audio/flac"));
            assert!(dirs.icon().is_file());

            remove_in(&dirs).unwrap();
            assert!(!dirs.entry().exists() && !dirs.icon().exists());

            // Once a package installs its entry, the one written before goes.
            set_default_in(&dirs, Kind::Audio, exe).unwrap();
            let packaged = root.join("system/applications/video.patina.Patina.desktop");
            std::fs::create_dir_all(packaged.parent().unwrap()).unwrap();
            std::fs::write(&packaged, ENTRY).unwrap();
            set_default_in(&dirs, Kind::Audio, exe).unwrap();
            assert!(!dirs.entry().exists());
            std::fs::remove_dir_all(&root).unwrap();
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use objc2::rc::Retained;
    use objc2_foundation::{NSArray, NSString, ns_string};
    use std::ffi::c_void;

    #[link(name = "CoreServices", kind = "framework")]
    unsafe extern "C" {
        fn UTTypeCreateAllIdentifiersForTag(
            tag_class: *const c_void,
            tag: *const c_void,
            conforming_to: *const c_void,
        ) -> *const c_void;
        fn LSSetDefaultRoleHandlerForContentType(
            content_type: *const c_void,
            role: u32,
            handler: *const c_void,
        ) -> i32;
        fn LSSetDefaultHandlerForURLScheme(scheme: *const c_void, handler: *const c_void) -> i32;
        fn UTTypeConformsTo(content_type: *const c_void, conforms_to: *const c_void) -> u8;
    }

    /// Whether `content_type` is media of `kind`, rather than another
    /// app's type that shares an extension (TypeScript's .ts). A dynamic
    /// type is one macOS knows nothing more about.
    fn is_media(content_type: &NSString, kind: Kind) -> bool {
        if content_type.to_string().starts_with("dyn.") {
            return true;
        }
        let parents: &[&NSString] = match kind {
            Kind::Video | Kind::Audio => &[ns_string!("public.audiovisual-content")],
            Kind::Playlist => &[
                ns_string!("public.playlist"),
                ns_string!("public.audiovisual-content"),
            ],
        };
        parents.iter().any(|parent| {
            // SAFETY: two CFStrings that outlive the call.
            unsafe {
                UTTypeConformsTo(
                    std::ptr::from_ref(content_type).cast(),
                    std::ptr::from_ref(*parent).cast(),
                ) != 0
            }
        })
    }

    const ALL_ROLES: u32 = 0xFFFF_FFFF;

    /// Makes Patina the default app for files of `kind`: for every type
    /// macOS knows each extension by, so that a .ts file is caught whether
    /// it counts as MPEG-2 or as a type of its own.
    pub fn set_default(kind: Kind) -> Result<String, String> {
        let bundle = NSString::from_str(crate::APP_ID);
        let mut failed = None;
        for extension in formats::extensions(&[kind]) {
            let extension = NSString::from_str(extension);
            // SAFETY: CFString and CFArray are NSString and NSArray; the
            // array is created, so owned.
            let types: Option<Retained<NSArray<NSString>>> = unsafe {
                Retained::from_raw(
                    UTTypeCreateAllIdentifiersForTag(
                        std::ptr::from_ref(ns_string!("public.filename-extension")).cast(),
                        Retained::as_ptr(&extension).cast(),
                        std::ptr::null(),
                    )
                    .cast_mut()
                    .cast(),
                )
            };
            for content_type in types
                .iter()
                .flat_map(|types| types.iter())
                .filter(|content_type| is_media(content_type, kind))
            {
                // SAFETY: two CFStrings that outlive the call.
                let status = unsafe {
                    LSSetDefaultRoleHandlerForContentType(
                        Retained::as_ptr(&content_type).cast(),
                        ALL_ROLES,
                        Retained::as_ptr(&bundle).cast(),
                    )
                };
                if status != 0 {
                    log::warn!("could not make Patina the default for {content_type}: {status}");
                    failed = Some(status);
                }
            }
        }
        // patina:// links come with videos, as the button says.
        if kind == Kind::Video {
            // SAFETY: two CFStrings that outlive the call.
            let status = unsafe {
                LSSetDefaultHandlerForURLScheme(
                    std::ptr::from_ref(ns_string!("patina")).cast(),
                    Retained::as_ptr(&bundle).cast(),
                )
            };
            if status != 0 {
                log::warn!("could not make Patina the handler of patina:// links: {status}");
                failed = Some(status);
            }
        }
        match failed {
            // kLSApplicationNotFoundErr
            Some(-10814) => {
                Err("macOS doesn't know Patina yet: open it from Applications first.".into())
            }
            Some(status) => Err(format!("macOS refused some of the types ({status}).")),
            None => Ok(format!("Patina now opens {}.", plural(kind))),
        }
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
        RegCreateKeyExW, RegDeleteKeyValueW, RegDeleteKeyW, RegDeleteTreeW, RegOpenKeyExW,
        RegQueryInfoKeyW, RegSetValueExW,
    };

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Sets `name` (the default value for None) of key `path` under
    /// HKEY_CURRENT_USER, making the key if need be.
    fn set(path: &str, name: Option<&str>, value: &str) -> Result<(), String> {
        let mut key: HKEY = std::ptr::null_mut();
        let path_w = wide(path);
        // SAFETY: NUL-terminated strings and a key this function closes.
        unsafe {
            let status = RegCreateKeyExW(
                HKEY_CURRENT_USER,
                path_w.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                std::ptr::null(),
                &mut key,
                std::ptr::null_mut(),
            );
            if status != ERROR_SUCCESS {
                return Err(format!("could not create {path}: error {status}"));
            }
            let name_w = name.map(wide);
            let value_w = wide(value);
            let status = RegSetValueExW(
                key,
                name_w
                    .as_ref()
                    .map_or(std::ptr::null(), |name| name.as_ptr()),
                0,
                REG_SZ,
                value_w.as_ptr().cast(),
                (value_w.len() * 2) as u32,
            );
            RegCloseKey(key);
            if status != ERROR_SUCCESS {
                return Err(format!("could not write {path}: error {status}"));
            }
        }
        Ok(())
    }

    fn delete_tree(path: &str) -> Result<(), String> {
        let path_w = wide(path);
        // SAFETY: a NUL-terminated string.
        let status = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, path_w.as_ptr()) };
        match status {
            ERROR_SUCCESS | ERROR_FILE_NOT_FOUND => Ok(()),
            status => Err(format!("could not remove {path}: error {status}")),
        }
    }

    fn delete_value(path: &str, name: &str) -> Result<(), String> {
        let (path_w, name_w) = (wide(path), wide(name));
        // SAFETY: NUL-terminated strings.
        let status =
            unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, path_w.as_ptr(), name_w.as_ptr()) };
        match status {
            ERROR_SUCCESS | ERROR_FILE_NOT_FOUND => Ok(()),
            status => Err(format!("could not remove {path}\\{name}: error {status}")),
        }
    }

    /// How many subkeys and values the key at `path` has; None if there is
    /// no such key.
    fn contents(path: &str) -> Option<(u32, u32)> {
        let path_w = wide(path);
        let mut key: HKEY = std::ptr::null_mut();
        let (mut subkeys, mut values) = (0u32, 0u32);
        // SAFETY: a NUL-terminated string, counters to fill in, and a key
        // this function closes.
        unsafe {
            if RegOpenKeyExW(HKEY_CURRENT_USER, path_w.as_ptr(), 0, KEY_READ, &mut key)
                != ERROR_SUCCESS
            {
                return None;
            }
            let status = RegQueryInfoKeyW(
                key,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
                &mut subkeys,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut values,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            RegCloseKey(key);
            (status == ERROR_SUCCESS).then_some((subkeys, values))
        }
    }

    /// Removes the key at `path` if nothing is left in it: registering made
    /// keys such as an extension's Open With list that held only Patina.
    fn delete_if_empty(path: &str) -> Result<(), String> {
        if contents(path) != Some((0, 0)) {
            return Ok(());
        }
        let path_w = wide(path);
        // SAFETY: a NUL-terminated string.
        let status = unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, path_w.as_ptr()) };
        match status {
            ERROR_SUCCESS | ERROR_FILE_NOT_FOUND => Ok(()),
            status => Err(format!("could not remove {path}: error {status}")),
        }
    }

    fn kind_name(kind: Kind) -> &'static str {
        match kind {
            Kind::Video => "video",
            Kind::Audio => "audio",
            Kind::Playlist => "playlist",
        }
    }

    fn tell_the_shell() {
        use windows_sys::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
        // SAFETY: a notice without items.
        unsafe {
            SHChangeNotify(
                SHCNE_ASSOCCHANGED as i32,
                SHCNF_IDLIST,
                std::ptr::null(),
                std::ptr::null(),
            )
        };
    }

    /// Adds Patina to Open With for its file types, and lists it under
    /// Default apps; registering again after Patina's folder has moved
    /// points everything at the new place.
    pub fn register() -> Result<String, String> {
        register_in("Software", &executable()?)?;
        tell_the_shell();
        Ok("Patina is in Open With, and can be chosen in Default apps.".into())
    }

    /// `software` is HKEY_CURRENT_USER's Software key, or a test's own.
    pub fn register_in(software: &str, exe: &str) -> Result<(), String> {
        let classes = format!(r"{software}\Classes");
        let capabilities = format!(r"{software}\Patina\Capabilities");
        // Options before -- are Patina's; what Windows puts in "%1" never is,
        // and a link stays one link (see --open-link in cli.rs).
        let command = format!("\"{exe}\" -- \"%1\"");
        let link_command = format!("\"{exe}\" --open-link -- \"%1\"");
        let icon = format!("\"{exe}\",0");
        set(&capabilities, Some("ApplicationName"), "Patina")?;
        set(
            &capabilities,
            Some("ApplicationDescription"),
            env!("CARGO_PKG_DESCRIPTION"),
        )?;
        set(&capabilities, Some("ApplicationIcon"), &icon)?;
        let application = format!(r"{classes}\Applications\patina.exe");
        set(&application, Some("FriendlyAppName"), "Patina")?;
        set(
            &format!(r"{application}\shell\open\command"),
            None,
            &command,
        )?;
        for format in formats::all() {
            let extension = format!(".{}", format.extension);
            let id = format!("Patina{extension}");
            let description = format!(
                "{} {}",
                format.extension.to_ascii_uppercase(),
                kind_name(format.kind)
            );
            set(&format!(r"{classes}\{id}"), None, &description)?;
            set(&format!(r"{classes}\{id}\DefaultIcon"), None, &icon)?;
            set(
                &format!(r"{classes}\{id}\shell\open\command"),
                None,
                &command,
            )?;
            set(
                &format!(r"{classes}\{extension}\OpenWithProgids"),
                Some(&id),
                "",
            )?;
            set(
                &format!(r"{application}\SupportedTypes"),
                Some(&extension),
                "",
            )?;
            set(
                &format!(r"{capabilities}\FileAssociations"),
                Some(&extension),
                &id,
            )?;
        }
        let link = format!(r"{classes}\patina");
        set(&link, None, "URL:Patina link")?;
        set(&link, Some("URL Protocol"), "")?;
        set(&format!(r"{link}\DefaultIcon"), None, &icon)?;
        set(&format!(r"{link}\shell\open\command"), None, &link_command)?;
        set(
            &format!(r"{capabilities}\URLAssociations"),
            Some("patina"),
            "patina",
        )?;
        set(
            &format!(r"{software}\RegisteredApplications"),
            Some("Patina"),
            r"Software\Patina\Capabilities",
        )
    }

    /// Removes what register added.
    pub fn remove() -> Result<String, String> {
        remove_in("Software")?;
        tell_the_shell();
        Ok("Removed Patina's file types.".into())
    }

    pub fn remove_in(software: &str) -> Result<(), String> {
        let classes = format!(r"{software}\Classes");
        for format in formats::all() {
            let extension = format!(".{}", format.extension);
            let id = format!("Patina{extension}");
            delete_tree(&format!(r"{classes}\{id}"))?;
            delete_value(&format!(r"{classes}\{extension}\OpenWithProgids"), &id)?;
            delete_if_empty(&format!(r"{classes}\{extension}\OpenWithProgids"))?;
            delete_if_empty(&format!(r"{classes}\{extension}"))?;
        }
        delete_tree(&format!(r"{classes}\Applications\patina.exe"))?;
        delete_if_empty(&format!(r"{classes}\Applications"))?;
        delete_tree(&format!(r"{classes}\patina"))?;
        delete_tree(&format!(r"{software}\Patina"))?;
        delete_value(&format!(r"{software}\RegisteredApplications"), "Patina")
    }

    /// Windows' Default apps, at Patina's page where Windows has one.
    pub fn open_default_apps() {
        crate::platform::open_url("ms-settings:defaultapps?registeredAppUser=Patina");
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Registers under a key of the test's own, which the shell doesn't
        /// read, and removes it again, leaving no key behind but those that
        /// were there before, and what Windows put in them.
        #[test]
        fn registers_and_removes() {
            let software = format!(r"Software\PatinaTest-{}", std::process::id());
            let classes = format!(r"{software}\Classes");
            // An extension that has an Open With list of another app's.
            set(
                &format!(r"{classes}\.mkv\OpenWithProgids"),
                Some("Other.mkv"),
                "",
            )
            .unwrap();
            register_in(&software, r"C:\Program Files\Patina\patina.exe").unwrap();
            assert_eq!(
                contents(&format!(r"{classes}\.mkv\OpenWithProgids")),
                Some((0, 2))
            );
            remove_in(&software).unwrap();
            assert_eq!(
                contents(&format!(r"{classes}\.mkv\OpenWithProgids")),
                Some((0, 1))
            );
            // Only .mkv is left in the classes; under the root, only the
            // classes and the list of registered applications, which the
            // real Software key has anyway.
            assert_eq!(contents(&classes), Some((1, 0)));
            assert_eq!(contents(&software), Some((2, 0)));
            delete_tree(&software).unwrap();
        }
    }
}
