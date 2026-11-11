#!/usr/bin/env bash
# Write packaging/THIRD-PARTY-rust.txt: every Rust crate compiled into
# patina, the licence it is passed on under, and that licence's text, in the
# crate's own copy where it ships one. Most of those licences (MIT, BSD, ISC,
# Unicode) ask that the notice go with every copy of a program built from
# the crate, so all three packages carry the file beside THIRD-PARTY.md.
#
#   packaging/rust-licenses.sh           write it
#   packaging/rust-licenses.sh check     make it afresh and fail if the
#                                        committed one differs
#   packaging/rust-licenses.sh version   print the cargo-about version it needs
#
# Run it after anything that changes Cargo.lock, and commit what it writes.
# Committed rather than made at build time, because the Flatpak builds
# offline from the commit, with no cargo-about; release.yml runs `check` and
# fails a release tag whose file is stale, and ci.yml warns about one.
#
# It needs cargo-about at exactly the version below, as
#
#   cargo install --locked --features cli cargo-about@<version>
#
# installs it (the binary needs the cli feature), on PATH or named by
# $CARGO_ABOUT. Another version may find or word things differently, which
# would make every `check` against a file it wrote fail, so it is refused.
#
# cargo-about reads the crates as cargo fetched them and nothing else
# (--offline: no licence text from a crate's git repository), so the file
# depends only on Cargo.lock, packaging/about.toml and packaging/about.hbs,
# whoever makes it and wherever. `cargo fetch --locked` first gets any crate
# not already fetched, for every platform.
#
# Any warning from cargo-about fails it, such as a crate whose licence file
# no longer matches the checksum about.toml gives for it. The one expected
# is that Slint's licence has no text, which cargo-about cannot attach
# (about.toml says why); the text is added here, from the slint crate.
set -euo pipefail
version=0.9.3
cd "$(dirname "$0")/.."

mode=${1:-write}
case "$mode" in
    version) echo "$version"; exit 0 ;;
    write | check) ;;
    *) echo "usage: $0 [check | version]" >&2; exit 2 ;;
esac

about=${CARGO_ABOUT:-cargo-about}
if ! have=$("$about" --version 2>/dev/null); then
    echo "$about not found; install it with: cargo install --locked --features cli cargo-about@$version" >&2
    exit 1
fi
if [ "$have" != "cargo-about $version" ]; then
    echo "$about is $have, not cargo-about $version, which this file is made with" >&2
    exit 1
fi

cargo fetch --locked --quiet

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
if ! "$about" -L warn --color never generate --locked --offline --fail \
    -c packaging/about.toml -o "$work/generated.txt" packaging/about.hbs 2> "$work/log"; then
    cat "$work/log" >&2
    echo "cargo-about failed" >&2
    exit 1
fi
# Its log is coloured whatever --color says, so the pattern allows for the
# escapes around the level and at the end of the line.
slint_warning="WARN[^ ]* LicenseRef-Slint-Royalty-free-2\.0 has no license file for crate '(i-)?slint(-[a-z-]+)? [^ ']+'[^ ]*$"
if grep -vE "^$|$slint_warning" "$work/log" >&2; then
    echo "cargo-about warned; see packaging/about.toml for what each warning asks" >&2
    exit 1
fi

# Slint's text, from the slint crate the lockfile names: about.hbs ends with
# the heading it goes under.
slint=$(cargo metadata --format-version 1 --locked --offline | python3 -c '
import json, os, sys
dirs = [os.path.dirname(p["manifest_path"]) for p in json.load(sys.stdin)["packages"] if p["name"] == "slint"]
assert len(dirs) == 1, f"expected one slint crate, found {len(dirs)}"
print(dirs[0])
')
{ echo; cat "$slint/LICENSES/LicenseRef-Slint-Royalty-free-2.0.md"; } >> "$work/generated.txt"

# Some crates' copies have Windows line ends or spaces at the ends of lines,
# and the texts end in any number of blank lines. Plain line ends, and one
# blank line at most, read the same everywhere and keep git's whitespace
# checks quiet.
sed 's/[[:space:]]*$//' "$work/generated.txt" | cat -s > "$work/THIRD-PARTY-rust.txt"

# Every crate listed has to be under some licence's text. about.hbs finds
# Slint's crates by their whole licence expression, and one that changes
# with a Slint release would otherwise leave them under none, with no
# warning, since the new expression still offers the licence about.toml
# accepts.
if ! awk '
    /^Crates$/ { section = 1; next }
    /^Licence texts$/ { section = 2; next }
    section == 1 && /^[^ -][^ ]* [^ ]+$/ { listed[$0] = 1 }
    section == 2 && /^Used by:/ { used = 1; next }
    section == 2 && used && /^    [^ ]+ [^ ,]+/ {
        sub(/^    /, ""); sub(/,.*/, ""); covered[$0] = 1; next
    }
    { used = 0 }
    END {
        for (crate in listed) {
            if (!(crate in covered)) {
                print "no licence text for " crate > "/dev/stderr"
                missing = 1
            }
        }
        exit missing
    }' "$work/THIRD-PARTY-rust.txt"; then
    echo "a crate is under no licence text; if it is one of Slint's, its licence expression no longer matches the one in packaging/about.hbs" >&2
    exit 1
fi

# The file is published in every package, and nothing of the machine that
# made it belongs there. The template names no path, so a path here is a
# mistake in it.
for path in "$PWD" "${CARGO_HOME:-$HOME/.cargo}"; do
    if grep -qF "$path" "$work/THIRD-PARTY-rust.txt"; then
        echo "the notice names $path; packaging/about.hbs must print no path" >&2
        exit 1
    fi
done

if [ "$mode" = check ]; then
    if ! diff -u packaging/THIRD-PARTY-rust.txt "$work/THIRD-PARTY-rust.txt" > "$work/diff"; then
        head -n 100 "$work/diff"
        echo "packaging/THIRD-PARTY-rust.txt is stale for this Cargo.lock: run packaging/rust-licenses.sh and commit what it writes" >&2
        exit 1
    fi
    echo "packaging/THIRD-PARTY-rust.txt is current"
else
    cp "$work/THIRD-PARTY-rust.txt" packaging/THIRD-PARTY-rust.txt
    echo "wrote packaging/THIRD-PARTY-rust.txt"
fi
