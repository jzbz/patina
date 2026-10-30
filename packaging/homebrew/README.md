# Homebrew

A cask, in a shared tap of our own, until homebrew-cask will take it.

## Why a tap first

homebrew-cask applies a notability bar, tripled for a self-submission because
the PR author would own the project: 225 stars, 90 forks or 90 watchers, any one
of which suffices — plus a 30-day minimum repository age. A tap has none of
that. It is a git repository named `homebrew-<something>` under your account,
and `brew` finds it by name.

## What a tap does not escape

Gatekeeper. `brew` applies `com.apple.quarantine` to a cask's app on install
regardless of which tap it came from; `--no-quarantine` was removed in Homebrew
4.7 and the `quarantine` stanza no longer exists in the DSL. An unsigned app
would therefore install cleanly and then refuse to open, which is a worse
experience than not offering it at all.

So no cask goes out before `packaging/macos-sign.sh` has produced a notarised,
stapled zip and that zip is on a published release. CI's
`patina-<tag>-macos-universal-unsigned.zip` never reaches the tap: the cask's
URL is the signed `patina-<tag>-macos-universal.zip`, and the script below
refuses to write a cask until that file is public and listed in a signed
`SHA256SUMS`. The 2026-09-01 deadline that disables unsigned casks applies only
to the main repository, not to a tap — but Gatekeeper applies everywhere.

## The tap

`github.com/jzbz/homebrew-tap` — one tap for everything published this way,
rather than one per app. A tap is a public repository named `homebrew-<name>`
with a `Casks/` directory, and that is the whole of it: no registration, no
review, no Homebrew involvement. Nothing about it is per-project, so Patina's
cask sits beside Azzurro's and rPGP's as `Casks/patina.rb`.

    brew tap jzbz/tap
    brew install --cask patina

or in one step, without tapping first:

    brew install --cask jzbz/tap/patina

Besides `Patina.app`, the cask puts a `patina` command on the `PATH`, a link to
the bundle's executable, for opening files from a terminal.

## Two copies of the cask

`patina.rb` here is the template: `version` and a `sha256` of 64 zeros, and
everything else the published cask says. The tap's `Casks/patina.rb` is that
template with a release's version and the hash of its notarised zip filled in,
and nothing else changed, header comment included. Edit the template, never the
tap's copy: the next release overwrites the tap's copy from the template.

## Per release

Only after the release is published, its `SHA256SUMS` signed and the notarised
zip attached. From this repository's root, with the tap checked out beside it:

    ./packaging/homebrew/update-cask.sh v0.1.0 ../homebrew-tap/Casks/patina.rb

The script is handed the cask's path rather than redirected into it. A `>`
empties the cask before the script has even started, so a failed download
would leave an empty file to be committed. Instead the script builds the cask
in a temporary file beside the real one and moves it into place only once every
check has passed; a failure leaves the tap's copy exactly as it was.

It writes the file and stops. The tap commit is the maintainer's own, made and
signed by hand like every commit in these repositories, and never by the script
or by a tool acting on the maintainer's behalf. Stage the one file and read
what changed, which for a bump is the `version` and `sha256` lines:

    git -C ../homebrew-tap add Casks/patina.rb
    git -C ../homebrew-tap diff --staged

then sign the commit:

    cd ../homebrew-tap && git commit -S -F - <<'EOF'
    patina 0.1.0
    EOF

A bump's message is the bare `patina <version>`, as the tap's other bumps read.
Name the file rather than reaching for `git commit -a`: the tap is shared, and a
bump for one app has no business carrying another app's in-flight change.

The first time, the tap's `README.md` changes too, since its introduction and
install lines name every app the tap holds: stage it alongside the cask, and
give that commit a subject saying it adds Patina rather than the bare bump.

Push once every commit the push would publish is confirmed as signed by the
maintainer's own key. After a fetch, this lists exactly those: it must show
the one bump (two the first time, if the tap's README went in its own commit),
each `G` with that key's fingerprint, and nothing else. The tap is shared, so
an earlier commit for another app may be waiting there unpushed.

    git -C ../homebrew-tap fetch origin
    git -C ../homebrew-tap log --format='%h %G? %GF %s' HEAD --not --remotes
    git -C ../homebrew-tap push origin master

There is no bot. BrewTestBot autobumps casks in the official repositories only,
so a tap is a hand-written commit each release — two lines, but they are yours.

## What the script checks

It downloads the release's `SHA256SUMS` and `SHA256SUMS.asc` first and checks
the signature from gpg's status lines rather than its exit status or its "Good
signature": exactly one signature, a good one, none that is bad, expired,
revoked or unverifiable, and a `VALIDSIG` naming the same key, whose last field
is the release key's full fingerprint. A second signature is refused rather
than ignored, as is one from a revoked or expired key, so another key's good
signature cannot carry a withdrawn one through. Only then does it download the
zip, hash it, and compare that with the zip's one line in the `SHA256SUMS` it
has just verified. A missing file, a missing or doubled line or a mismatch is a
refusal, never a skip, and no cask comes out. Nor does one if the template's
`version` or `sha256` line was not rewritten.

That check is the only point at which this project's signing discipline touches
a Homebrew user, because the cask itself carries no signature: a cask user
trusts the tap's git history and Apple's notary, not the release key.

## The release key

The script needs gpg with the release key's public half in its keyring, which
on the machine holding the key it already is. It imports nothing and fetches no
key, whatever `gpg.conf` says. To run it anywhere else, import the key once,
from a keyserver:

    gpg --locate-keys jz@jz.bz

or from GitHub, which is the same host as the release and so the weaker of the
two:

    curl -sL https://github.com/jzbz.gpg | gpg --import

Either way the fingerprint is what to trust, not where the key came from:

    gpg --fingerprint 252B901C88853CF9F9392559249738C8641C3359

has to show `252B 901C 8885 3CF9 F939  2559 2497 38C8 641C 3359`.

## Moving to homebrew-cask later

When the project clears the notability bar, the cask can be submitted upstream
and this one file deleted from the tap — the tap itself stays, because it holds
other apps. Users who tapped will keep working either way; `brew` prefers the
official cask once both exist, and the fully qualified `jzbz/tap/patina` goes on
resolving until the file is removed.
