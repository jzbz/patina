#!/bin/sh
# Write the cask for a published release, with the real hash filled in.
#
#   ./packaging/homebrew/update-cask.sh v0.1.0 ../homebrew-tap/Casks/patina.rb
#
# Run after the release is published, its SHA256SUMS signed and the notarised
# macOS zip attached — the hash has to be of the artifact users will actually
# download, which is the stapled one, not the zip CI produced.
#
# No cask comes out until SHA256SUMS.asc is exactly one good signature by the
# release key over SHA256SUMS and the zip's one line in that file matches the
# download. So this needs gpg with the release key's public half already in its
# keyring, as it is on the machine holding the key. It imports nothing and
# fetches no key; on any other machine, import the key as README.md beside this
# script says under "The release key", and check its fingerprint first.
#
# The output is a path rather than stdout on purpose. `> Casks/patina.rb`
# truncates the cask before this script has run a line, so any failure below
# would leave an empty file for the commit that follows to publish. Here the
# cask is built in a temporary file beside the real one and renamed over it
# only after every check has passed; a failed run leaves it untouched.
#
# It writes the file and stops: the tap commit is made and signed by hand, as
# README.md beside this script describes. By hand rather than from CI, too:
# updating the tap from here would need a token with write access to another
# repository, and a two-line change once a release does not justify holding
# one.
set -eu

# The release key, in full: the one key whose signature over SHA256SUMS counts.
# A 16-digit key ID (249738C8641C3359) is short enough for someone to generate
# a key that shares it.
FPR=252B901C88853CF9F9392559249738C8641C3359

TAG="${1:-}"
OUT="${2:-}"
[ -n "$TAG" ] && [ -n "$OUT" ] || {
    echo "usage: $0 <tag> <cask>   e.g. $0 v0.1.0 ../homebrew-tap/Casks/patina.rb" >&2
    exit 1
}
case "$TAG" in v*) ;; *) echo "error: tag should start with v, got '$TAG'" >&2; exit 1 ;; esac
VERSION="${TAG#v}"
OUT_DIR=$(dirname "$OUT")
[ -d "$OUT_DIR" ] || { echo "error: no directory $OUT_DIR to write the cask into" >&2; exit 1; }
TEMPLATE="$(dirname "$0")/patina.rb"
[ -f "$TEMPLATE" ] || { echo "error: no template at $TEMPLATE" >&2; exit 1; }
command -v curl >/dev/null 2>&1 || { echo "error: curl not found" >&2; exit 1; }
if ! command -v gpg >/dev/null 2>&1; then
    echo "error: gpg not found. No cask is written without checking SHA256SUMS.asc," >&2
    echo "  which needs gpg with key $FPR in its keyring." >&2
    exit 1
fi

NAME="patina-$TAG-macos-universal.zip"
BASE="https://github.com/jzbz/patina/releases/download/$TAG"

WORK=$(mktemp -d) || exit 1
NEW=""
cleanup() {
    rm -rf "$WORK"
    [ -z "$NEW" ] || rm -f "$NEW"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

# Every file is required, and a failed download stops the run. A release
# missing SHA256SUMS or its signature is one whose checksums have not been
# through the signing step yet, which is exactly the release that must not
# reach a Homebrew user; and a check that is skipped when a file will not
# download is one that anyone able to delete a release asset can turn off.
fetch() {
    echo "fetching $BASE/$1" >&2
    if ! curl -sSLf --max-time "$2" -o "$WORK/$1" "$BASE/$1"; then
        echo "error: could not download $BASE/$1" >&2
        echo "  The zip, SHA256SUMS and SHA256SUMS.asc are all required, and a" >&2
        echo "  draft release's assets are not public — publish it first." >&2
        exit 1
    fi
}

fetch SHA256SUMS 60
fetch SHA256SUMS.asc 60

# The signature first, so the checksum file is trusted before a line of it is
# read, and before the zip is even downloaded. gpg's exit status and its "Good
# signature" hold for a good signature by any key in the keyring, so the
# verdict comes from its machine-readable status lines, and a non-zero exit is
# a refusal on top of that.
#
# Checking that a GOODSIG exists and that a VALIDSIG names FPR is not enough on
# its own, because an .asc can hold more than one signature and nothing ties
# those two lines to the same one. gpg writes VALIDSIG for a signature from a
# revoked or expired key too, labelled REVKEYSIG or EXPKEYSIG instead of
# GOODSIG. So the file must hold exactly one signature, its result must be
# GOODSIG, and any other result (BADSIG, ERRSIG, EXPSIG, EXPKEYSIG, REVKEYSIG)
# refuses outright. That one GOODSIG's key ID has to be the key that VALIDSIG
# says made the signature, and VALIDSIG's last field, the primary key's
# fingerprint, has to be FPR, which holds even if a signing subkey is added
# later.
#
# --no-auto-key-retrieve and --no-auto-key-import hold gpg to the keyring as it
# stands, whatever gpg.conf says: a key the signature names or carries is
# neither fetched nor imported.
RC=0
gpg --batch --no-auto-key-retrieve --no-auto-key-import --status-fd 1 \
    --verify "$WORK/SHA256SUMS.asc" "$WORK/SHA256SUMS" \
    >"$WORK/status" 2>"$WORK/gpg.err" || RC=$?
if [ "$RC" -ne 0 ] || ! awk -v fpr="$FPR" '
        $1 != "[GNUPG:]" { next }
        $2 ~ /^(GOODSIG|BADSIG|ERRSIG|EXPSIG|EXPKEYSIG|REVKEYSIG)$/ { results++ }
        $2 ~ /^(BADSIG|ERRSIG|EXPSIG|EXPKEYSIG|REVKEYSIG)$/ { refused = 1 }
        $2 == "GOODSIG" { goods++; keyid = toupper($3) }
        $2 == "VALIDSIG" {
            valids++
            signer = toupper($3)
            primary = toupper((NF >= 12) ? $12 : $3)
        }
        END {
            if (refused || results != 1 || goods != 1 || valids != 1) exit 1
            if (primary != fpr) exit 1
            # GOODSIG carries a 64-bit key ID or a whole fingerprint depending
            # on the gpg version; either way it is the tail of the signer.
            n = length(keyid)
            if (n < 16 || substr(signer, length(signer) - n + 1) != keyid) exit 1
            exit 0
        }' "$WORK/status"; then
    echo "error: SHA256SUMS.asc is not exactly one good signature over SHA256SUMS by" >&2
    echo "  $FPR. gpg reported:" >&2
    sed 's/^/  /' "$WORK/gpg.err" >&2
    grep -E '^\[GNUPG:\] (GOODSIG|BADSIG|ERRSIG|EXPSIG|EXPKEYSIG|REVKEYSIG|VALIDSIG) ' \
        "$WORK/status" | sed 's/^/  /' >&2 || true
    # ERRSIG's rc 9 is a missing key, and its last field the fingerprint of the
    # key that made the signature. Only a hint: the verdict is already given.
    if awk -v fpr="$FPR" '$1 == "[GNUPG:]" && $2 == "ERRSIG" && $8 == 9 &&
            toupper($NF) == fpr { found = 1 } END { exit !found }' "$WORK/status"; then
        echo "  The release key is not in this keyring, and this fetches no key:" >&2
        echo "  import it as packaging/homebrew/README.md says under \"The release" >&2
        echo "  key\", check its fingerprint, and run this again." >&2
    fi
    exit 1
fi
echo "  SHA256SUMS signed by $FPR" >&2

fetch "$NAME" 300

# sha256sum on Linux, shasum on macOS: this runs on whichever machine is to hand.
if command -v sha256sum >/dev/null 2>&1; then
    SUM=$(sha256sum "$WORK/$NAME" | awk '{print $1}')
else
    SUM=$(shasum -a 256 "$WORK/$NAME" | awk '{print $1}')
fi
echo "  sha256 $SUM" >&2

# The zip's line from the SHA256SUMS just verified, so a corrupted or
# substituted zip cannot quietly become the cask's pinned hash. This is the one
# place the project's own signing discipline reaches a Homebrew user at all;
# the cask itself carries no signature. Exactly one line, or no cask: the zip's
# name carries the tag, so a signed SHA256SUMS lifted from another release has
# no line for it, and reading a missing line as nothing to compare would let
# that swap through.
WANT=$(awk -v n="$NAME" '$2 == n || $2 == "*" n { c++; h = $1 }
    END { if (c == 1) print h }' "$WORK/SHA256SUMS")
if [ -z "$WANT" ]; then
    echo "error: the signed SHA256SUMS does not list $NAME exactly once" >&2
    exit 1
fi
if [ "$WANT" != "$SUM" ]; then
    echo "error: hash does not match the signed SHA256SUMS for $NAME" >&2
    echo "  downloaded: $SUM" >&2
    echo "  SHA256SUMS: $WANT" >&2
    exit 1
fi
echo "  matches the signed SHA256SUMS" >&2

NEW=$(mktemp "$OUT_DIR/.$(basename "$OUT").XXXXXX")
sed -e "s/^  version \".*\"$/  version \"$VERSION\"/" \
    -e "s/^  sha256 \".*\"$/  sha256 \"$SUM\"/" \
    "$TEMPLATE" >"$NEW"

# sed reports success whether or not a pattern matched, so a template whose
# stanzas were reformatted would otherwise pass through with the old values.
if ! grep -qx "  version \"$VERSION\"" "$NEW" || ! grep -qx "  sha256 \"$SUM\"" "$NEW"; then
    echo "error: the template's version or sha256 line was not rewritten" >&2
    exit 1
fi

# mktemp makes the file private; a cask is meant to be read.
chmod 644 "$NEW"
mv -f "$NEW" "$OUT"
NEW=""
echo "wrote $OUT" >&2
