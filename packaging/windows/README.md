# Windows installer

`patina.iss` is the Inno Setup script for `patina-vX.Y.Z-x86_64-setup.exe`,
the per-user installer each release carries beside the portable
`patina-vX.Y.Z-windows-x86_64.zip`. It installs the folder that zip holds,
file for file: `patina.exe`, the `libmpv-2.dll` and `vulkan-1.dll` it needs
beside it, `LICENSE.txt`, `LICENSE-lucide.txt` and `THIRD-PARTY.md`. To that
it adds what an unpacked folder cannot have: a Start Menu entry, an Apps &
Features entry, an uninstaller that removes both along with the file types
Patina registered, and an App Paths entry so that Win+R `patina` finds the
program from a normal, not elevated, Run box. It is what winget installs;
`packaging/winget/README.md` covers that side.

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
  own: its single-instance files live there.
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
  `patina.exe` and the two DLLs; Patina has no named mutex for Setup to
  check instead. Where it cannot be closed that way, the upgrade fails and
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
- Never touches `%APPDATA%\Patina`: `config` (settings, key bindings, mpv's
  configuration, scripts and fonts) and `data` (history, the place in each
  file, thumbnails, downloaded subtitles). Nor the OpenSubtitles password in
  Credential Manager, nor the screenshots in `Pictures\Patina`. The
  uninstaller removes what the installer wrote, the file types as above,
  and nothing else, and release.yml fails a build whose uninstaller disturbs
  a file placed in `%APPDATA%\Patina` or `%LOCALAPPDATA%\Patina` first. The
  portable folder keeps its
  files in the same places, so moving from it to the installer, or back,
  finds everything where it was.

Close Patina before uninstalling it. Inno's uninstaller has no way to close
a running copy: it removes the file types, the shortcut, the App Paths entry
and the uninstall entry, and leaves the files Patina has open (`patina.exe`
and the two DLLs) and the folder behind, which can be deleted once Patina is
closed.

`patina.exe` is the portable folder's, so the Visual C++ runtime stays linked
into it (`.cargo/config.toml`) and the installer carries no redistributable.

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
  `packaging\windows`, not from where ISCC was started. The six files are
  named one by one in the script, and a missing one stops the compile.
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
installed files against the folder's, file by file and with nothing extra,
then the shortcut, the App Paths entry and the uninstall entry, and that the
installed `patina --version` loads the `libmpv-2.dll` beside it. It writes Patina's
file-type keys for a few types and the link, uninstalls, and checks that
everything is gone, keys included, apart from the stand-ins for Patina's own
files. Last, before the upload, it checks that the zip and the setup exe are
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
trips a bug in 0.6.0), then `files()`, yields the six `{app}\…` files and
their bytes, each of whose SHA-256 should equal the same file's in the
unpacked zip. That shows the payload is the signed folder, though not what
else the setup does with it; only the rebuild above compares every byte.
