; Patina's Windows installer: a per-user Inno Setup script around the Patina
; folder every release already ships as its portable zip: patina.exe, the
; libmpv-2.dll and vulkan-1.dll it needs beside it, and the license files.
;
; The windows job of .github/workflows/release.yml compiles it with the Inno
; Setup it pins, 7.1.0, and the script uses directives that compiler has and
; 6.x does not, so an older one stops at the first rather than building
; something else. To compile it by hand, from the root of the repository:
;
;   ISCC.exe --define=AppVersion=0.1.0
;            --define=SourceDir=C:\full\path\to\Patina
;            --output-dir=C:\somewhere\outside\the\tree
;            packaging\windows\patina.iss
;
; (one line). Without --output-dir ISCC writes into packaging\windows\Output.
; AppVersion has to be the version patina.exe carries, X.Y.Z with nothing
; after it, since it also becomes the setup's numeric file version; the
; workflow reads it from the exe's ProductVersion. SourceDir is the folder
; the portable zip holds, by its full path: a relative one would be read
; from this script's folder, not from where ISCC was started.
; packaging/windows/README.md says how to check that a given setup exe wraps
; the released folder byte for byte.
;
; SourceDir is a preprocessor define, and has nothing to do with the [Setup]
; directive of the same name. That one is left at its default, the folder
; this script is in, which SetupIconFile below is relative to.
;
; ASCII only, so the question of which encoding ISCC reads it in never
; arises: it has taken UTF-8 without a byte-order mark since 6.3.0, and since
; 7.0.2 it refuses to compile a byte that is not valid in the encoding it
; settles on. Comments sit on lines of their own: a trailing ";" is part of a
; directive's value.

#ifndef AppVersion
  #error Pass --define=AppVersion=X.Y.Z, the version of the patina.exe being wrapped
#endif
#ifndef SourceDir
  #error Pass --define=SourceDir= with the full path of the Patina folder to wrap
#endif

[Setup]
; Never change this once an installer has shipped. Inno names the uninstall
; entry after it (Patina.Patina_is1 under HKCU), which is how a newer
; installer finds the installed one and replaces it in place, and the winget
; manifest pins that name as the ProductCode. The same as the winget package
; identifier, so the two can't be confused.
AppId=Patina.Patina
AppName=Patina
AppVersion={#AppVersion}
AppPublisher=Jonathan Zeppettini
AppPublisherURL=https://jz.bz/
AppSupportURL=https://github.com/jzbz/patina/issues
AppUpdatesURL=https://github.com/jzbz/patina/releases
AppCopyright=Copyright (c) 2026 Jonathan Zeppettini
; The Apps & Features name, kept to the bare name rather than "Patina 0.1.0":
; winget matches the entry against the manifest's PackageName and Publisher,
; and reads the version from DisplayVersion, which is AppVersion.
UninstallDisplayName=Patina
UninstallDisplayIcon={app}\patina.exe
VersionInfoVersion={#AppVersion}

; Per user, and never elevated. Setup asks for no administrator rights, so it
; runs as the person who will use Patina, and the "Launch Patina" box at the
; end starts the app as them too: an elevated installer run by a standard user
; through an administrator's password would start it as the administrator, and
; the settings, history and file types it saved would be in the wrong profile.
; Patina's file types are per user in any case: Preferences > Utilities writes
; them under HKCU, and the uninstaller can remove them only for the account
; that runs it, which here is the account that installed Patina. In this mode
; the {auto...} constants mean the current user's folders, the uninstall entry
; and the App Paths key below go under HKCU, and no admin is needed to remove
; it. PrivilegesRequiredOverridesAllowed is left unset, so Setup ignores
; /ALLUSERS rather than turning this into a per-machine install.
PrivilegesRequired=lowest
; RedirectionGuard keeps an elevated Setup from following junctions and links
; a less privileged user planted. This one never elevates and writes only to
; the user's own folders and HKCU, and with lowest privileges Inno Setup 7.1
; lets the Patina that "Launch Patina" starts inherit the mitigation: that
; Patina, which then takes every file opened in Explorer for the session,
; couldn't open media or yt-dlp reached through a junction or link the user
; made (mklink /J, Scoop's "current", winget's Links).
RedirectionGuard=no
; %LOCALAPPDATA%\Programs\Patina. Not %LOCALAPPDATA%\Patina, which is
; Patina's own (its single-instance files live there) and no installer's
; business. The same folder for every version, upgraded in place: the file
; types Patina registers name patina.exe by its full path, and keep working
; across an upgrade only if that path stays put.
DefaultDirName={autopf}\Patina
DisableDirPage=yes
DisableProgramGroupPage=yes

; A 64-bit Setup, new in Inno Setup 7, for a 64-bit program. It also makes
; ArchitecturesAllowed and ArchitecturesInstallIn64BitMode default to
; x64compatible: x64 Windows, and Arm64 Windows 11, which runs x64 programs
; under emulation (Patina has not been tried there).
SetupArchitecture=x64
; Windows 10, the oldest Windows Rust's x86_64-pc-windows-msvc target still
; supports. Inno's default would let Windows 7 install a program that cannot
; start there.
MinVersion=10.0

SetupIconFile=..\icons\patina.ico
WizardStyle=modern dynamic
; The default, written out because the installer's bytes depend on it, and
; the release checks that two compiles of the same folder come out identical.
Compression=lzma2/max
; Overridden by --output-filename in the workflow, which names the setup
; after the tag or commit being built, as it names the portable zip.
OutputBaseFilename=patina-v{#AppVersion}-x86_64-setup

; An upgrade has to replace a patina.exe, and the libmpv-2.dll and
; vulkan-1.dll it loads, that may be in use, so Setup asks Restart Manager to
; close the Patina holding them first. Restart Manager finds it by those open
; files, which the default CloseApplicationsFilter (*.exe,*.dll,*.chm)
; covers; Patina creates no named mutex, so AppMutex would have nothing to
; wait on and is not set. A Patina run from a portable folder elsewhere holds
; none of these files and is left alone.
;
; The request is made with the messages Windows sends every program at
; sign-out. Patina has no handling of its own for them and leaves the answer
; to Windows' default, as rPGP does. rPGP's installer, which makes the same
; request, closed a running rPGP that way on Windows 11, in a silent upgrade
; as winget runs one, and replaced it in under two seconds; Patina's has not
; yet been tried over a running copy, and whether Patina still saves its place
; in what was playing on the way out, as it does when it quits, is unchecked.
; A silent setup closes it without asking; an interactive one names it and
; asks first. It is not started again afterwards, since Setup restarts only
; programs registered for that, and Patina is not.
;
; yes, the default, written out because force was tried for rPGP and is the
; wrong choice. The two differ only where the request fails: for a copy that
; does not answer it, or one it cannot reach (in rPGP's test, one started
; from another non-interactive window station). force ends that copy, along
; with whatever was being done in it, and nobody is asked. With yes Setup
; gives up instead, with exit code 5, and changes nothing: the old version
; stays installed and that copy keeps running. winget's default return codes
; for an Inno installer map exit code 5 to cancelled by the user, so winget
; says the upgrade was cancelled, though nobody cancelled it; closing Patina
; and running the upgrade again fixes it. An upgrade that fails until Patina
; is closed is the better of the two.
CloseApplications=yes

[Files]
; The folder the portable zip carries, file by file. Named one by one rather
; than with a wildcard, so that a file missing from SourceDir stops the
; compile instead of shipping an installer without it, and nothing else that
; happens to sit in the folder is swept in. A file added to the folder has
; to be added here too. One the folder stops carrying needs an [InstallDelete]
; entry then, or an upgrade leaves the old copy in {app} until Patina is
; uninstalled. The two DLLs stay beside patina.exe: that is where Patina
; looks for libmpv, and where Windows finds the vulkan-1.dll libmpv imports.
;
; ignoreversion: an upgrade, a reinstall and a winget downgrade all replace
; each file whatever version it carries; the DLLs' own versions, where they
; have one, need not rise with Patina's. notimestamp: a file's time on the
; runner is the moment it was built or copied, and storing it would make two
; compiles of the same bytes differ.
Source: "{#SourceDir}\patina.exe"; DestDir: "{app}"; Flags: ignoreversion notimestamp
Source: "{#SourceDir}\libmpv-2.dll"; DestDir: "{app}"; Flags: ignoreversion notimestamp
Source: "{#SourceDir}\vulkan-1.dll"; DestDir: "{app}"; Flags: ignoreversion notimestamp
Source: "{#SourceDir}\LICENSE.txt"; DestDir: "{app}"; Flags: ignoreversion notimestamp
Source: "{#SourceDir}\LICENSE-lucide.txt"; DestDir: "{app}"; Flags: ignoreversion notimestamp
Source: "{#SourceDir}\THIRD-PARTY.md"; DestDir: "{app}"; Flags: ignoreversion notimestamp

[Icons]
; The Start Menu entry, and the reason this installer exists. No desktop
; shortcut. The icon is the one embedded in the exe, and no AppUserModelID is
; set, because Patina sets none at run time and the two have to agree for the
; taskbar to group a pinned shortcut with the running window.
Name: "{autoprograms}\Patina"; Filename: "{app}\patina.exe"; WorkingDir: "{app}"; Comment: "Play videos and music"

[Registry]
; The patina command: an App Paths entry makes Win+R "patina" and `start
; patina` find the exe, from a normal, not elevated, Run box or prompt. When
; rPGP's was tried from elevated, non-interactive processes on a test
; machine, an HKCU App Paths name did not resolve, so nothing is promised
; there. It does not put patina on PATH for a shell, and nothing here changes
; PATH. Removed, key and all, on uninstall.
;
; It is the only key this script writes. Patina's file types and patina://
; links are Patina's own: Preferences > Utilities registers them and Patina
; removes them (see [UninstallRun]), and Windows lets no program make itself
; the default silently in any case. Nothing here goes under
; HKCU\Software\Patina either, since removing Patina's file types deletes
; that whole tree.
Root: HKA; Subkey: "Software\Microsoft\Windows\CurrentVersion\App Paths\patina.exe"; ValueType: string; ValueName: ""; ValueData: "{app}\patina.exe"; Flags: uninsdeletekey

[Run]
; Offered at the end of an interactive install only. skipifsilent covers
; /SILENT as well as /VERYSILENT, so winget, which passes one or the other,
; never starts the app.
Filename: "{app}\patina.exe"; WorkingDir: "{app}"; Description: "{cm:LaunchProgram,Patina}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; The first step of uninstalling, before any file goes: the installed
; patina.exe removes the file types and patina:// links Preferences >
; Utilities registered for this account, the keys under
; HKCU\Software\Classes, HKCU\Software\Patina and RegisteredApplications that
; would otherwise stay behind in Open With, Default apps and the handler for
; patina:// links, naming an exe that is no longer there.
; --remove-file-types does what the "Remove Patina's File Types" button does,
; opens no window and leaves a running Patina alone, and finding nothing to
; remove is not a failure. The uninstaller waits for it and does not check
; its exit code, so if it fails the keys stay and the uninstall goes on.
; RunOnceId: every upgrade adds this entry to the uninstall log again, and
; with an id only the latest one runs. No skipifdoesntexist: if patina.exe
; was deleted by hand, the keys cannot be removed, and the uninstaller's
; warning that some elements could not be removed is then the truth.
Filename: "{app}\patina.exe"; Parameters: "--remove-file-types"; Flags: runhidden waituntilterminated; RunOnceId: "RemoveFileTypes"

; No [UninstallDelete] section, and there must never be one. The uninstaller
; removes what this script installed, then the folder if it is empty, and
; asks Patina to remove its file types as above; nothing else. Patina's own
; files live in %APPDATA%\Patina: config (settings.json, key bindings, mpv's
; configuration, scripts and fonts) and data (history, the place in each
; file, thumbnails, downloaded subtitles). %LOCALAPPDATA%\Patina holds its
; single-instance files. All are written by the app and all outside {app}.
; Any other copy of Patina the same account runs, the portable zip's
; included, reads the same folders, and removing this one is not a request
; to delete them, nor the OpenSubtitles password in Credential Manager or the
; screenshots in Pictures\Patina. The release workflow installs and
; uninstalls this on its runner and fails if a file placed in
; %APPDATA%\Patina or %LOCALAPPDATA%\Patina beforehand is gone.
