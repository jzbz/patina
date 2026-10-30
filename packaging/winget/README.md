# winget

Manifests for the Microsoft winget community repository. They do not live here
in any operational sense — winget reads them from `microsoft/winget-pkgs` — but
the first submission has to come from somewhere, and package metadata that
strangers read before installing is worth writing deliberately rather than
generating blind. Nothing has been submitted yet. `manifest/` holds 0.1.0's,
written to the checklist below, with two placeholders (`InstallerSha256` and
`ReleaseDate`) that can only be filled in once that release is published and
its `SHA256SUMS` signed.

## What ships

A release carries two Windows files: the portable
`patina-vX.Y.Z-windows-x86_64.zip`, a `Patina` folder holding `patina.exe`
and the DLLs it needs, and `patina-vX.Y.Z-x86_64-setup.exe`, a per-user Inno
Setup installer that release.yml builds around that same folder
(`packaging/windows/`). winget installs the second one, as
`InstallerType: inno` with `Scope: user`. It asks for no administrator
rights, puts the folder in `%LOCALAPPDATA%\Programs\Patina`, and adds Patina
to the Start Menu and to Apps & Features. `winget uninstall` runs its
uninstaller, which first has Patina remove the file types and `patina://`
links it registered from Preferences › Utilities, then takes the rest away
again provided Patina is closed first, and leaves the settings, history and
the rest of `%APPDATA%\Patina` where they are either way. With Patina still
running, the uninstaller also leaves `patina.exe`, the DLLs and the folder
behind, to be deleted by hand once Patina is closed
(`packaging/windows/README.md`). The zip is still on every release for
whoever downloads it directly; winget does not install it.

The manifest lists only the installer, from the first submission on. A
portable package would give no Start Menu entry, and nothing in winget can
give one: no manifest schema from 1.6 to 1.28 has a field for a shortcut,
winget's own code creates none, and the request for it
(microsoft/winget-cli#2299) has been open since 2022. Nor can it be swapped
for an installer later. rPGP shipped through winget as a portable package up
to 0.1.3, and winget will not upgrade a portable install to an installer:
since winget 1.29.280 it fails with error 0x8A15008E, neither `--force` nor
`--uninstall-previous` gets past that, and every user had to uninstall and
install again by hand. Patina avoids that by never listing a portable entry.

`Commands: [patina]` is there for search only. For a portable package it
would make winget register an alias; for an installer winget does nothing
with it. The setup exe registers an App Paths entry for `patina.exe` under
HKCU instead, which is what Win+R and `start patina` look up from a normal,
not elevated, Run box or prompt. It does not put `patina` on PATH, and a
terminal that runs plain `patina` will not find it.

## File types and links

The installer manifest lists `FileExtensions`, the 63 extensions in the first
column of `packaging/media-types.tsv`, and `Protocols: [patina]`. Both are
metadata only: winget registers nothing for them, and neither does the
installer. Patina registers its file types and `patina://` links itself, per
user, from Preferences › Utilities, and the uninstaller has it remove them
again. Keep the list in step with the table; from the repository root, this
prints it in the manifest's form:

    grep -v '^#' packaging/media-types.tsv | cut -f1 | sed 's/^/- /'

## The identifier

`Patina.Patina`, naming the project twice, as Azzurro's `Azzurro.Azzurro`
does; that one was merged without a request to change it. rPGP's moderators
asked for `jzbz.rPGP` instead of `rPGP.rPGP`, partly to keep it apart from an
unrelated project of the same name, and "Patina" is a common enough word that
they may ask the same here, for `jzbz.Patina`. The identifier is effectively
permanent once a version is merged, since changing it later means a new
package and an orphaned old one that silently stops updating, so if it is
going to change, it has to be settled before the first release: the
installer's `AppId` is the same name, and that can't change once an installer
has shipped. In winget-pkgs the manifests live at
`manifests/p/Patina/Patina/<version>/`.

The installer's `AppId`, `Patina.Patina`, names the uninstall entry,
`Patina.Patina_is1`, which the manifest gives as its `ProductCode`; that is
how winget recognizes the installed copy, and how a newer installer finds the
old one to replace it. (rPGP's and Azzurro's installers are `jzbz.rPGP` and
`jzbz.Azzurro`, whatever their packages are called; Patina keeps the two names
the same.)

## The order matters

winget validation downloads the asset and checks its hash, so **the GitHub
release must be published, not a draft**. release.yml deliberately creates a
draft, so the winget step comes after the release is complete and public —
after the signature over `SHA256SUMS`, not before. The locale manifest's
`PackageUrl` and `PrivacyUrl` point at `https://patina.video/` and
`https://patina.video/privacy/`; both pages have to be live before
submitting, and a moderator asked rPGP for its privacy page before merging.

## Submitting

By hand, when asked, from the machine that holds the signing key. In a new
directory holding the published release's `SHA256SUMS` and `SHA256SUMS.asc`:

    gpg --batch --no-auto-key-retrieve --status-fd 1 --verify SHA256SUMS.asc SHA256SUMS 2>/dev/null | grep -E '^\[GNUPG:\] (GOODSIG|BADSIG|ERRSIG|EXPSIG|EXPKEYSIG|REVKEYSIG|VALIDSIG) '
    awk '$2 == "patina-v0.1.0-x86_64-setup.exe" { print toupper($1) }' SHA256SUMS

The first has to print exactly one `GOODSIG`, none of the others, and a
`VALIDSIG` whose last field is `252B901C88853CF9F9392559249738C8641C3359`;
"Good signature" alone holds for any key the keyring trusts. The second
prints the value `InstallerSha256` must equal, in the upper case the
manifests use. The setup exe's line is the one that matters to winget. The
installed files are the portable zip's bytes exactly, which release.yml
checks by installing the setup on its runner, so the zip's line vouches for
what ends up on disk as well.

The first submission's manifests are the ones in `manifest/`, finished by
hand rather than generated: they carry the comments that say why each field
is what it is, and the hash has to come from the signed file anyway. Komac
could not have read this installer until recently. 2.16.0 reads Inno
installers through the `inno` crate 0.4.2, which refuses anything newer than
Inno Setup 6.7; 2.17.0 (2026-10-14) moved to `inno` 0.7.0 and reads Inno
Setup up to 7.1.0. Whether it reads this installer's scope and ProductCode
as the list below has them has not been tried. Before sending the manifests,
check that they read as follows:

- one installer, `Architecture: x64`, and `InstallerType: inno`, with no
  `portable` installer type anywhere, at the top of the file or under the
  installer;
- `Scope: user`, and no `ElevationRequirement`, since the installer never
  asks for one;
- `ProductCode: Patina.Patina_is1`, which is `AppId` in
  `packaging/windows/patina.iss` with the `_is1` Inno adds;
- `InstallerUrl` the setup exe on the published release, and
  `InstallerSha256` its line in the signed `SHA256SUMS`, checked as above;
- `ReleaseDate` the day the release was published;
- `Commands: [patina]`, `UpgradeBehavior: install`, `Protocols: [patina]`
  and `FileExtensions` matching `packaging/media-types.tsv`;
- the version in all three files, and the locale file's `LicenseUrl` and
  `ReleaseNotesUrl` naming its tag.

`AppsAndFeaturesEntries` can be left out: the uninstall entry's name,
publisher and version are the manifest's own. If it is written, it names
`Patina`, publisher `Jonathan Zeppettini`, the version being submitted and
the same ProductCode, and `InstallationMetadata`, if written, has
`DefaultInstallLocation: '%LocalAppData%\Programs\Patina'`.

Then, on Windows, `winget validate --manifest packaging\winget\manifest` has
to pass; the placeholders make it fail until both are filled in. rPGP's
first installer manifest went in this way: written by hand, validated, and
committed, signed, on a branch of a fork of winget-pkgs, as
`manifests/j/jzbz/rPGP/0.1.4/`; the same for Patina means the three files in
`manifests/p/Patina/Patina/0.1.0/` and a pull request titled "New package:
Patina.Patina version 0.1.0". `komac submit <dir>`, which analyzes no
installer and keeps every value (though it writes the files out again in its
own layout, without their comments), is the other way to send them. The
Microsoft CLA is a one-time checkbox on that PR. A new package waits for a
moderator's manual review: rPGP's first took four weeks, Azzurro's five.

Once a version is merged, later ones can go through Komac, with one URL, the
setup exe's, writing the manifests to a folder:

    komac update Patina.Patina --version 0.1.1 \
      --urls https://github.com/jzbz/patina/releases/download/v0.1.1/patina-v0.1.1-x86_64-setup.exe \
      --output <dir>

Komac downloads the installer, reads what it needs from it, computes the hash
and fills the schema, then stops at a Submit / Edit / Exit menu with Submit
chosen: **choose Exit**. The manifests are in `<dir>` by then. That needs
Komac 2.17.0 or newer (its `inno` 0.7.0), for Inno Setup 7; on Komac's main
branch before that, `inno` 0.6.0 read rPGP's and Azzurro's identical settings
as lowest privileges, which Komac turns into `Scope: user`, and their
ProductCode as `<AppId>_is1`. Check what it wrote against the list above, and
its `InstallerSha256` against the setup exe's line in the verified
`SHA256SUMS`; correct anything by hand, then send them with
`komac submit <dir>`, which keeps every value, InstallerSha256 included, and
asks once more before sending. Not Submit in `komac update`, nor its
`--submit`: that sends the hash Komac computed from its own download, which
nobody has compared with the signed one. Keep every field the published version has:
winget-pkgs' bot labels a pull request `Manifest-Metadata-Consistency` and
lists what the new version lacks.

`manifest/` here holds what was actually submitted, for review before it is
sent and as the starting point for the next version; replace it each time.
Keep `InstallerSha256` in step with the release's signed `SHA256SUMS` rather
than recomputing it: pinning the hash that signature covers is the only
thread connecting a winget install back to the key.

## Why there is no workflow for this

rPGP and Azzurro each had one, and it never ran. `winget.yml` fired on every
published release, found no `WINGET_PAT`, skipped itself and reported success
— three releases across the two projects, each with a green check for having
done nothing. Setting the token would have been worse than leaving it unset:
the job called a third-party action and would have handed it that credential,
and a third-party action runs with the same access to the workflow as
anything else in it. Both workflows were deleted.

So the submission is a command, run by a person, from the machine that
already holds the signing key. At this release cadence that is a smaller cost
than a credential in CI, and it puts the person who signed the checksums in
the same place as the person who pins the hash.

## What a winget user actually trusts

Not the PGP signature. winget pins `InstallerSha256`, verifies it client-side,
and refuses to install on a mismatch — but nothing in that chain reads
`SHA256SUMS.asc` or knows about key 249738C8641C3359. A winget user is
trusting Microsoft's validation pipeline, its moderators, and TLS to GitHub.

That is not an argument against winget. winget does give what it downloads a
Mark-of-the-Web, marked as from the Internet the way a browser marks it, but
once the file matches the manifest's hash and the source is a trusted one, as
the community repository is, it rewrites the mark to the Trusted zone before
running the installer, so the unsigned setup exe starts without a SmartScreen
prompt. That makes it the least unpleasant way to get unsigned code onto a
Windows machine, and strictly better than the browser download it replaces.
The exception is Smart App Control: on a Windows 11 machine where it is
enforcing, unsigned code from an unknown publisher is blocked whatever the
mark says, through winget or not. What all this is an argument for is keeping
the signed checksum file prominent: it is the artifact that survives a
compromise of any of the above, and it lets anyone audit a packager's hash
line years later.
