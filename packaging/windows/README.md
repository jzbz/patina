# Windows installer

`patina.iss` is the Inno Setup script for `patina-vX.Y.Z-x86_64-setup.exe`,
the per-user installer each release carries beside the portable
`patina-vX.Y.Z-windows-x86_64.zip`. It installs the folder that zip holds,
file for file: `patina.exe`, the `libmpv-2.dll` it needs beside it and its
licences, `LICENSE-libmpv.txt` and `LICENSE-LGPLv3.txt`, the `yt-dlp` folder
(yt-dlp's folder build: `yt-dlp.exe` and its `_internal`) and `ffmpeg.exe` it
runs for online video and downloads, `LICENSE.txt`, `LICENSE-lucide.txt`,
`LICENSE-ibm-plex.txt`, `THIRD-PARTY.md`, `THIRD-PARTY-rust.txt` (the
licences of the Rust crates compiled into `patina.exe`), and those two
programs' notices, `THIRD-PARTY-yt-dlp.txt`, `LICENSE-GPLv3.txt` and
`LICENSE-ffmpeg.txt`. To that it adds what an unpacked folder cannot have: a
Start Menu entry, an Apps & Features entry, an uninstaller that removes both
along with the file types Patina registered, and an App Paths entry so that
Win+R `patina` finds the program from a normal, not elevated, Run box. It is
what winget installs; `packaging/winget/README.md` covers that side.

It is the script rPGP and Azzurro use (`packaging/windows/rpgp.iss` and
`azzurro.iss` there), with Patina's names, icon and folders and three real
differences: it wraps a folder, named file by file, rather than one exe; its
uninstaller first asks Patina to remove the file types Patina registered;
and it turns off Inno Setup's RedirectionGuard, which the script says why.

## What it does, and does not

- Installs for the current user only, into `%LOCALAPPDATA%\Programs\Patina`,
  and never asks for an administrator. Per user, so that the account that
  installs Patina is the account that runs it, keeps its settings and
  registers its file types. Not `%LOCALAPPDATA%\Patina`, which is Patina's
  own: its single-instance folder lives there.
- One shortcut, in the Start Menu. No desktop shortcut, no PATH change.
- A "Launch Patina" box at the end of an interactive install; a silent
  install, which is how winget runs it, starts nothing.
- Registers no file types and no `patina://` links. Preferences › Utilities
  does that, as it does for the portable folder, and points them at the
  installed `patina.exe`; Windows lets no program make itself the default
  silently in any case, and Patina sends you to Default apps for that. Every
  version installs into the same folder, so the registered paths keep
  working across upgrades.
- Upgrades in place: the same `AppId` finds the installed copy. A running
  Patina is closed first, by the request Windows also makes of it at
  sign-out, and without asking anyone when the upgrade is silent, as
  winget's is. Restart Manager finds it by the files it holds open,
  `patina.exe` and `libmpv-2.dll`; Patina has no named mutex for Setup to
  check instead. A `yt-dlp.exe` or `ffmpeg.exe` still running for a
  download holds its own exe open too, and `yt-dlp.exe` the DLLs it loaded
  from `yt-dlp\_internal`; whether Restart Manager can close those, console
  programs with no window, has not been tried. An upgrade replaces
  `yt-dlp\_internal` whole rather than file by file, so nothing an older
  yt-dlp carried stays among the new one's files. Where it cannot be closed that way, the upgrade fails and
  leaves the old version installed and running rather than ending it
  (`CloseApplications` in the script says why). Setup then exits with code
  5, which winget's default return codes for an Inno installer map to
  cancelled by the user, so winget says the upgrade was cancelled, though
  nobody cancelled it. Closing Patina and running the upgrade again fixes
  it. That is how rPGP's identical setting behaved on a test machine;
  Patina has not yet been upgraded over a running copy.
- Uninstalling starts with `patina.exe --remove-file-types`, run hidden from
  the installed folder before anything is deleted. It does what Preferences
  › Utilities › Remove Patina's File Types does: it removes the keys under
  `HKCU\Software\Classes`, `HKCU\Software\Patina` and
  `RegisteredApplications` that would otherwise stay in Open With, Default
  apps and the handler for `patina://` links, naming an exe that is gone.
  Those keys are Patina's by name, not by path, so this also removes what a
  portable copy registered, which that copy's Preferences can register
  again. If it fails, the uninstall goes on and the keys stay. release.yml
  writes those keys for a few file types and the link before uninstalling
  on its runner, and fails a build that leaves any of them behind.
- Leaves alone what the bundled yt-dlp writes outside the folder, as any
  yt-dlp does: its cache, `%USERPROFILE%\.cache\yt-dlp`, which the
  YouTube section's own calls never write.
- Removes `%LOCALAPPDATA%\Patina\instance`, the lock that keeps Patina to
  one at a time and the id of the copy holding it, which every copy makes
  again when it starts; and only while that folder is plainly Patina's own:
  reached through no link or junction, holding nothing but lock and id files
  named as Patina names them, and none of the locks in use by a running
  Patina, this one or a portable copy. Anything else there keeps it, and the
  uninstall log says why. `%LOCALAPPDATA%\Patina` itself stays. On a
  Windows 11 VM it removed a folder holding one idle lock; the branches
  that keep it, a lock in use or a file Patina would not have made, have
  run only on the workflow's runner.
- Never touches `%APPDATA%\Patina`: `config` (settings, key bindings, mpv's
  configuration, scripts and fonts) and `data` (history, the place in each
  file, thumbnails, downloaded subtitles). Nor the OpenSubtitles password in
  Credential Manager, nor the screenshots in `Pictures\Patina`. The
  uninstaller removes what the installer wrote, the file types and the
  single instance's folder as above, and nothing else, and release.yml fails
  a build whose uninstaller disturbs a file placed in `%APPDATA%\Patina` or
  `%LOCALAPPDATA%\Patina` first, or that removes the single instance's
  folder when it holds a file Patina would not have made or a lock held
  open, or leaves it when it holds only an idle lock and id. The portable
  folder keeps its
  files in the same places, so moving from it to the installer, or back,
  finds everything where it was.

Close Patina before uninstalling it. Inno's uninstaller has no way to close
a running copy: it removes the file types, the shortcut, the App Paths entry
and the uninstall entry, and leaves the files Patina has open (`patina.exe`
and `libmpv-2.dll`) and the folder behind, which can be deleted once Patina is
closed.

`patina.exe` is the portable folder's, so the Visual C++ runtime stays linked
into it (`.cargo/config.toml`) and the installer carries no redistributable.
`ffmpeg.exe` is linked statically with MinGW's runtime and imports only
DLLs that are part of Windows itself, which `release.yml` checks in its
import table, and yt-dlp carries its own in `yt-dlp\_internal`.

## yt-dlp and ffmpeg.exe

Both are pinned by digest in `packaging/bundled-tools.env`, whose comment
says how to move them; yt-dlp should move to its newest release for every
Patina release. The `yt-dlp` folder is yt-dlp's own folder build,
`yt-dlp_win.zip` unpacked as it is: `yt-dlp.exe`, about 10 MB, and
`_internal`, 142 files and 31 MB, Python and the libraries and runtime DLLs
`yt-dlp.exe` loads from there. `release.yml` also checks the zip against
the attestation of yt-dlp's immutable release. `ffmpeg.exe` is built in `release.yml` from FFmpeg's source
tarball by `packaging/ffmpeg.sh`, cross-compiled with mingw-w64, a few
MB: it joins video and audio and makes yt-dlp's fixes by copying
streams, and can convert nothing, so yt-dlp options such as
`--extract-audio` or `--embed-subs` fail with ffmpeg's own message; nor
is there an ffprobe, which yt-dlp needs to join a video that a site other
than YouTube serves in several parts. For those, name a full ffmpeg, with
its ffprobe beside it, with `ffmpeg-location=` in Patina's yt-dlp
options. `release.yml` joins two files with the two programs on the
runner, as a download does, before staging the folder.

The folder's licences change with them. The `yt-dlp` folder is
GPL-3.0-or-later as a whole, though yt-dlp's own code is Unlicense: the
build carries mutagen
(GPL-2.0-or-later) and certifi (MPL-2.0) among Python's libraries, as
yt-dlp's README says. So the folder holds a GPL program, run as a program of
its own, which leaves `patina.exe` MIT and `libmpv-2.dll` LGPL; its licence
is `LICENSE-GPLv3.txt`. `ffmpeg.exe` is LGPL-2.1-or-later as configured,
and its licence is `LICENSE-ffmpeg.txt`, the text from FFmpeg's tarball.
`THIRD-PARTY.md` says where each one's source is.

Patina finds both by itself, `yt-dlp\yt-dlp.exe` and `ffmpeg.exe` beside
`patina.exe`, after a yt-dlp the user named in
Preferences › Network, in mpv's own `script-opts/ytdl_hook.conf`, or put in
mpv's configuration folder (a `youtube-dl` there doesn't count). A
`yt-dlp.exe` the user installed on the `PATH` (winget's, pip's) is used
instead of the bundled one when its version is newer; each is asked once,
and asked again only when its file changes (for Scoop's shim, the program
the shim starts). Patina passes yt-dlp `--ffmpeg-location`,
which wins over an `ffmpeg-location` in the user's own yt-dlp configuration
file; Patina's yt-dlp options are the place to name another.

Neither carries an Authenticode signature, like `patina.exe` itself. Files
the installer writes carry no mark of the web, and Patina starts both
directly rather than through the shell, so SmartScreen's prompt, which the
shell shows, is not expected for them; from a zip unpacked with Explorer
they do carry the mark. Antivirus programs have flagged PyInstaller-built
programs such as `yt-dlp.exe` as malware from time to time; a quarantined
`yt-dlp.exe` shows in Patina as yt-dlp missing. On a Windows 11 VM with
Smart App Control off, Defender found nothing in either, installed or
unpacked from a marked zip. SmartScreen on a double-click of a marked
`patina.exe` in Explorer has not been tried, nor Smart App Control
enforcing.

The folder build, unlike yt-dlp's single-file `yt-dlp.exe`, unpacks
nothing when it starts: it loads Python from `_internal` where it sits, in
one process. That is why it is the one carried. Patina stops a yt-dlp it no
longer needs with `taskkill /T /F`, the only way known to work on a
console program with no window (`taskkill` without `/F` refused one on a
test machine), and a single-file `yt-dlp.exe` stopped that way left the
29 MB `_MEI` folder it had unpacked in `%TEMP%` every time, which Windows
doesn't clear by itself. The folder build leaves nothing when stopped so.
A single-file `yt-dlp.exe` the user installed, as winget's is, and which
Patina uses when it is the newer, still leaves one each time it is
stopped; that is a matter for that copy. On a Windows 11 VM with no GPU and
no audio device, the folder build started in about 0.4 seconds, against 1.3
to 1.5 for the single-file one, and in 0.6 the first time after installing.
A portable copy just unpacked from a marked zip took 6 seconds to start
once, and 1.4 and 2 seconds on two more copies, one without the mark; why is
not known. Stopped by Patina, through Cancel in the Downloads window, a new
search or Back in the YouTube section, or quitting during a download, it
left no process and no `_MEI` folder in `%TEMP%`. Killing Patina itself while
yt-dlp runs has not been tried.

## Building it by hand

On Windows, with Inno Setup 7.1.0's x64 edition (`innosetup-7.1.0-x64.exe`
from jrsoftware/issrc's GitHub release; the workflow pins its SHA-256) and the
folder to wrap. The folder is the one inside a portable zip, either the
release's or a workflow run's `windows-x86_64` artifact:

    Expand-Archive patina-v0.1.0-windows-x86_64.zip -DestinationPath C:\scratch\zip

That gives `C:\scratch\zip\Patina`. Then, from the root of a checkout of the
same commit:

    ISCC.exe --define=AppVersion=0.1.0 --define=SourceDir=C:\scratch\zip\Patina --output-dir=C:\scratch packaging\windows\patina.iss

- `AppVersion` is the version `patina.exe` carries, X.Y.Z with nothing after
  it, since it is also the setup's numeric file version. The workflow reads
  it from the exe:
  `(Get-Item C:\scratch\zip\Patina\patina.exe).VersionInfo.ProductVersion`.
- `SourceDir` is the folder, by its full path; a relative one is read from
  `packaging\windows`, not from where ISCC was started. The thirteen files
  and `yt-dlp\yt-dlp.exe` are named one by one in the script, and a missing
  one stops the compile; `yt-dlp\_internal` is taken whole.
- `--output-dir` keeps the result out of the tree; without it ISCC writes
  into `packaging\windows\Output`. The name is
  `patina-v<AppVersion>-x86_64-setup.exe` unless `--output-filename` gives
  another. The workflow passes
  `--output-filename=patina-$LABEL-x86_64-setup`, where `LABEL` is the tag,
  or for a build of a commit its short SHA, so that it is named as its zip
  is.

To try the result as winget would, on a machine where losing nothing matters,
in PowerShell:

    & C:\scratch\patina-v0.1.0-x86_64-setup.exe /SP- /VERYSILENT /SUPPRESSMSGBOXES /NORESTART

and remove it again with

    & "$env:LOCALAPPDATA\Programs\Patina\unins000.exe" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART

## The switches winget uses

The manifest gives no `InstallerSwitches`, so winget passes an Inno installer
its built-in ones:

| Command | Switches |
| --- | --- |
| `winget install` or `upgrade` | `/SP- /SILENT /SUPPRESSMSGBOXES /NORESTART` |
| the same with `--silent` | `/SP- /VERYSILENT /SUPPRESSMSGBOXES /NORESTART` |
| the same with `--interactive` | none: the full wizard, with the "Launch Patina" box |
| any of them | `/LOG="<a path of winget's>"`, and `/DIR="<path>"` when `--location` is given |

`/SILENT` shows a progress window and `/VERYSILENT` shows nothing; neither
asks a question, and with either the "Launch Patina" box is skipped. A
running Patina is closed without asking in both. `winget uninstall` runs the
command Inno writes into the uninstall entry as `QuietUninstallString`, the
uninstaller with `/SILENT`, or with `--interactive` the plain
`UninstallString`. The uninstaller's first step, `--remove-file-types`, runs
hidden either way.

## How it is built

The windows job of `.github/workflows/release.yml` builds the folder as the
portable zip lays it out and downloads Inno Setup 7.1.0 (x64) from
jrsoftware's GitHub release, checked against a pinned SHA-256. It compiles
this script twice: once from the staged folder, and once from a copy of it at
another path, into another directory. The two outputs have to be identical or
the build fails, so the setup exe is a function of the folder, this script and
that compiler and nothing else. It then installs the result on the runner,
silently, with the switches winget's `--silent` passes, and checks the
installed files against the folder's, file by file, those in `yt-dlp`
included, and with nothing extra, then the shortcut, the App Paths entry
and the uninstall entry, and that the installed `patina --version` loads
the `libmpv-2.dll` beside it and finds the yt-dlp and ffmpeg installed with
it. It writes Patina's file-type keys for a few types and the link,
uninstalls, and checks that everything is gone, keys and the single
instance's folder included, apart from the stand-ins for Patina's own
files; then installs and uninstalls twice more to check that the single
instance's folder stays when it holds something Patina would not have made
there, or a lock held open. Last, before the upload, it checks that the zip and the setup exe are
still the bytes their steps hashed. The comment above the Inno Setup step
says how to move the pin.

## Checking a setup exe

The setup exe has its own line in the release's signed `SHA256SUMS`, so the
signature covers it like any other asset. That it wraps the signed portable
zip's folder and nothing else can be checked on Windows, by rebuilding it or
installing it, and the payload alone from Linux. Neither needs the libmpv
download the build started from, whose pinned URL expires
(`packaging/windows-deps.env`): the zip carries the same bytes.

Rebuild it. With the portable zip from the release, this script from the
release's tag, and the same Inno Setup, 7.1.0's x64 edition, from the
repository root:

    Expand-Archive patina-v0.1.0-windows-x86_64.zip -DestinationPath C:\scratch\zip
    ISCC.exe --define=AppVersion=0.1.0 --define=SourceDir=C:\scratch\zip\Patina --output-dir=C:\scratch packaging\windows\patina.iss
    certutil -hashfile C:\scratch\patina-v0.1.0-x86_64-setup.exe SHA256

The hash should equal the setup exe's line in `SHA256SUMS`. The script stores
no time stamps and no source paths, and Inno takes no attributes from the
source files, so neither where the folder or the checkout sits nor when the
zip was unpacked should enter the setup's bytes. rPGP's and Azzurro's
single-exe setups have been shown to rebuild identically on one machine, from
the exe at two paths under two names and from checkouts at two paths with LF
or CRLF line endings; for Patina's folder, a rebuild from the unpacked zip
matching the runner's output has not been tried yet.

Or install it, on a machine where losing nothing matters, and hash each file
in `%LOCALAPPDATA%\Programs\Patina` other than `unins000.exe` and
`unins000.dat`: each should equal the same file in the unpacked zip.

From Linux, innoextract is no help, since its newest code knows Inno Setup
only up to 6.4, but the Rust `inno` crate reads Inno Setup 7 from 0.6.0
(Komac 2.17 uses 0.7.0). With its `extract` feature, `inno::Inno::new`
on the setup exe opened as a plain `std::fs::File` (a `BufReader` around it
trips a bug in 0.6.0), then `files()`, yields the `{app}\…` files, those
in `{app}\yt-dlp` among them, and their bytes, each of whose SHA-256 should equal the same file's in the
unpacked zip. That shows the payload is the signed folder, though not what
else the setup does with it; only the rebuild above compares every byte.
