#!/bin/sh
# Sign, notarise and staple the macOS bundle, on the machine that holds the key.
#
# CI builds Patina.app and stops there: the Developer ID key is not on the
# runner and is not going to be. That is not caution for its own sake. A leaked
# Developer ID key cannot be quietly rotated — Apple does not let you revoke one
# from the account portal (it is an email to product-security@apple.com), and a
# revocation stops every already-shipped copy from launching on every machine
# that has one, including correctly notarised ones. Expiry is survivable;
# revocation is not. So the key stays on this machine, next to the PGP key that
# signs the release's SHA256SUMS, and never goes near a runner.
#
#   ./packaging/macos-sign.sh patina-v0.1.2-macos-universal-unsigned.zip
#
# Takes the unsigned zip CI produced (the macos-universal workflow artifact;
# `gh run download` unwraps the artifact and leaves this zip) or an unpacked
# Patina.app. Signs every library in the bundle and then the bundle, notarises
# and staples it, zips it again, and checks that new zip the way a stranger's
# Mac will check it. The new zip goes beside the input under the input's name
# without -unsigned, patina-v0.1.2-macos-universal.zip: the name the draft
# release, SHA256SUMS and the Homebrew cask all expect. It takes that name only
# once every check has passed, and the input is never modified, so a run that
# fails can simply be run again. A zip already named without -unsigned is
# replaced in place under the same rule; a Patina.app gives Patina.zip beside
# it.
#
# The entitlements are packaging/macos-entitlements.plist from the checkout this
# script runs from, so run it from the release's tag.
#
# One-time setup on this machine — see the block printed by --setup.
set -eu

PROFILE="${PATINA_NOTARY_PROFILE:-patina-notary}"
ENTITLEMENTS="$(cd "$(dirname "$0")" && pwd)/macos-entitlements.plist"

die() { printf '\nerror: %s\n' "$*" >&2; exit 1; }
step() { printf '\n== %s\n' "$*"; }

# Run a command, indent what it says, and KEEP ITS EXIT STATUS.
#
# `cmd | sed` reports sed's status, not cmd's, so `set -e` never sees a failure
# and an `|| die` after it is unreachable. That is not hypothetical: the first
# real run of the script this one is modelled on had codesign fail with
# errSecInternalComponent, print "code object is not signed at all", and carry
# on to spend four minutes notarising an unsigned bundle before Apple rejected
# it.
run() {
    _out=$("$@" 2>&1); _rc=$?
    [ -n "$_out" ] && printf '%s\n' "$_out" | sed 's/^/  /'
    return $_rc
}

# Sign one piece of code with the Developer ID: one line when that works,
# everything codesign said when it does not.
#
# --options runtime enables the hardened runtime, which notarisation requires.
# --timestamp gets a secure timestamp from Apple, which is what keeps already
# shipped copies working after the certificate expires.
#
# The keychain advice on failure is for errSecInternalComponent alone. Given
# for every failure, it pointed one with another cause, a timestamp server that
# did not answer or a certificate name that matched two identities, at the
# command that wants the login password on the command line. codesign's own
# message is printed either way.
sign() {
    _what=$1
    shift
    if _out=$(codesign --sign "$IDENTITY" --force --timestamp --options runtime "$@" 2>&1); then
        echo "  signed:    $_what"
        return 0
    fi
    [ -z "$_out" ] || printf '%s\n' "$_out" | sed 's/^/  /'
    case "$_out" in
        *errSecInternalComponent*) ;;
        *) die "codesign failed on $_what; what it said is above." ;;
    esac
    die "codesign failed on $_what.

  errSecInternalComponent almost always means codesign could not use the
  private key without asking, and could not ask: over SSH there is no way to
  show the keychain's 'allow access' prompt. Two ways round it —

    1. Run this from Terminal ON the Mac, once. macOS asks whether codesign may
       use the key; choose 'Always Allow'. Afterwards SSH runs work too.

    2. Or authorise it without the prompt, which needs your login password on
       the command line:

           security set-key-partition-list -S apple-tool:,apple:,codesign: \\
             -s -k '<login password>' ~/Library/Keychains/login.keychain-db"
}

if [ "${1:-}" = "--setup" ]; then
    cat <<'SETUP'
One-time setup on the signing Mac
---------------------------------

1. Developer ID Application certificate, in this Mac's keychain.

   In Keychain Access it must appear under "My Certificates" with a disclosure
   triangle — the triangle means the private key is present. Under plain
   "Certificates" with no triangle means the key is on some other machine, and
   Apple will not re-issue it; you would have to create a new certificate from
   a fresh CSR. Note the cap: five unexpired Developer ID Application
   certificates per team.

   Check what this Mac has:

       security find-identity -v -p codesigning

   You want a line reading "Developer ID Application: <name> (<TEAMID>)".
   "Apple Development" is a different certificate and will notarise-reject.

   Renewing is making a new certificate, from a new CSR, and the old one stays
   valid until it expires. Leave it to expire: revoking it would stop every
   copy it signed from launching. Until then this Mac holds two identities
   under the same name, and the script stops rather than pick one. Choose the
   newer (Keychain Access shows each certificate's expiry date, and its SHA-1
   under Fingerprints) and name it by the 40-digit hash that find-identity
   prints beside it:

       PATINA_SIGN_IDENTITY=<SHA-1> ./packaging/macos-sign.sh <zip>

   PATINA_SIGN_IDENTITY takes that hash or the identity's whole name, and only
   ever a Developer ID Application identity.

2. An App Store Connect API key of this app's own, stored as a notarytool
   profile.

   Create it at App Store Connect > Users and Access > Integrations > App Store
   Connect API. It must be a TEAM key, not an Individual one: an Individual key
   cannot drive notarytool. Start at the Developer role. Download the .p8 once —
   Apple will not let you download it again — and note the Issuer ID and Key ID
   from the same page.

   A key per app rather than one shared across them. They cost nothing, and the
   point is the blast radius: a key that only ever notarises Patina can be
   revoked the day this repository is suspected of anything without stopping
   any other app from shipping. Name it for the app in App Store Connect too,
   or a year from now the list is four keys called "notary".

   Then store it in the keychain so nothing needs to sit in this script or in
   your shell history:

       xcrun notarytool store-credentials patina-notary \
         --key ~/path/to/AuthKey_XXXXXXXXXX.p8 \
         --key-id XXXXXXXXXX \
         --issuer aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee

   An API key is preferred over an Apple ID plus app-specific password because
   it is a machine credential: it does not carry your account's privileges, and
   revoking it is one click that breaks nothing already shipped.

3. Nothing else to create. The entitlements are in the repository, in
   packaging/macos-entitlements.plist, which says why each one is there or is
   not, and this script reads them from the checkout it runs from: run it from
   the release's tag.
SETUP
    exit 0
fi

INPUT="${1:-}"
[ -n "$INPUT" ] || die "usage: $0 <unsigned .zip | Patina.app>   (or --setup)"
[ -e "$INPUT" ] || die "no such file: $INPUT"

command -v xcrun >/dev/null 2>&1 || die "xcrun not found — this must run on macOS with the command line tools installed"

# ---------------------------------------------------------------- preflight
# All of it before anything is modified, because the failures here are the ones
# that otherwise surface ten minutes into a notarisation wait.
step "Preflight"

# The identity is picked by its certificate's SHA-1, and codesign is handed
# that hash, never a name. A Developer ID Application certificate is not
# renewed in place: the new one comes from a new CSR, the old one stays valid
# until it expires, and both carry the same name, "Developer ID Application:
# <name> (<TEAMID>)". codesign refuses a name that matches two identities, and
# no name can tell those two apart; a hash can. So where more than one would
# do, this stops and lists them rather than guess, before anything is unpacked.
#
# find-identity -v lists the valid identities whose private key is on this
# Mac, one per line:
#
#   1) 0123456789ABCDEF0123456789ABCDEF01234567 "Developer ID Application: ..."
#
# The same certificate in two keychains can be listed twice, which codesign
# treats as harmless. So does this: DEVIDS holds one "HASH NAME" line per
# distinct certificate, and only Developer ID Application ones, which is what
# keeps an Apple Development or Apple Distribution certificate out whichever
# way it is named.
IDENTITIES=$(security find-identity -v -p codesigning 2>&1) \
    || die "security find-identity failed:
$(printf '%s\n' "$IDENTITIES" | sed 's/^/    /')"
DEVIDS=$(printf '%s\n' "$IDENTITIES" \
    | sed -n 's/^ *[0-9][0-9]*) \([0-9A-Fa-f]\{40\}\) "\(Developer ID Application: .*\)"$/\1 \2/p' \
    | awk '{ print toupper(substr($0, 1, 40)) substr($0, 41) }' | sort -u)

# PATINA_SIGN_IDENTITY narrows the choice: forty hex digits are a SHA-1, as
# they are to codesign, and anything else has to be an identity's whole name.
# Keychain Access shows a SHA-1 in pairs, "3E 1F 5A ...", so spaces, and the
# colons other tools put there, are left out when looking for one.
WANT="${PATINA_SIGN_IDENTITY:-}"
HEX=$(printf '%s' "$WANT" | tr -d ' :')
if [ -z "$WANT" ]; then
    MATCHES=$DEVIDS
elif [ ${#HEX} -eq 40 ] && [ -z "$(printf '%s' "$HEX" | tr -d '0-9A-Fa-f')" ]; then
    WANT=$(printf '%s' "$HEX" | tr 'a-f' 'A-F')
    MATCHES=$(printf '%s\n' "$DEVIDS" \
        | WANT="$WANT" awk 'substr($0, 1, 40) == ENVIRON["WANT"]')
else
    MATCHES=$(printf '%s\n' "$DEVIDS" \
        | WANT="$WANT" awk 'substr($0, 42) == ENVIRON["WANT"]')
fi

case $(printf '%s' "$MATCHES" | awk 'END { print NR }') in
    1)
        IDENTITY=${MATCHES%% *}
        IDENTITY_NAME=${MATCHES#* }
        ;;
    0)
        [ -z "$WANT" ] || die "PATINA_SIGN_IDENTITY names no Developer ID Application identity on this Mac:
    '$PATINA_SIGN_IDENTITY'
  It takes the SHA-1, or the whole name, of an identity whose name starts
  'Developer ID Application: '. Found:
$(printf '%s\n' "$IDENTITIES" | sed 's/^/    /')
  Apple Development and Apple Distribution certificates are for other purposes
  and notarisation will reject a bundle signed with one."
        die "no 'Developer ID Application' identity in the keychain.
  Found instead:
$(printf '%s\n' "$IDENTITIES" | sed 's/^/    /')
  Run '$0 --setup' for what this needs."
        ;;
    *)
        die "more than one Developer ID Application identity would do, and this will not
  guess between them:
$(printf '%s\n' "$MATCHES" | sed 's/^/    /')
  A renewed certificate leaves two until the old one expires. Choose the newer
  (Keychain Access shows each one's expiry date, and its SHA-1 under
  Fingerprints) and run this again with its hash:

      PATINA_SIGN_IDENTITY=<SHA-1> $0 $INPUT"
        ;;
esac
echo "  identity:  $IDENTITY_NAME"
echo "             SHA-1 $IDENTITY"

[ -f "$ENTITLEMENTS" ] || die "no $ENTITLEMENTS; run this from a checkout of the repository"
LINT=$(plutil -lint "$ENTITLEMENTS" 2>&1) \
    || die "$ENTITLEMENTS is not a valid property list: $LINT"
echo "  entitlements: $ENTITLEMENTS"

# The login keychain unlocks with the password at console login and stays locked
# in an SSH session, so this is the normal state when signing remotely. Checked
# here rather than left to codesign, which fails partway through with an error
# about the identity rather than about the lock.
#
# Before the notary check below, and that order is load-bearing: a locked
# keychain makes `notarytool history` fail too, and the message it earns
# there is "the profile is missing or its credentials are rejected" — which
# sends you to App Store Connect to re-make a key that was never the
# problem. Measured with a sibling app's copy of this script: a freshly stored,
# working profile reported exactly that over SSH, and `notarytool` said
# `keychainLocked` once asked directly.
if ! security show-keychain-info 2>&1 | grep -qi "no-timeout\|timeout"; then
    die "the login keychain is locked, so codesign cannot reach the private key.
  Unlock it and run this again:

      security unlock-keychain

  This is the usual state over SSH: the keychain unlocks with your password
  when you log in at the console, not when you connect remotely."
fi
echo "  keychain:  unlocked"

xcrun notarytool history --keychain-profile "$PROFILE" >/dev/null 2>&1 \
    || die "notarytool profile '$PROFILE' is missing or its credentials are rejected.
  Run '$0 --setup' for how to create it, or set PATINA_NOTARY_PROFILE."
echo "  notary:    profile '$PROFILE' authenticates"

# ------------------------------------------------------------------- unpack
WORK=$(mktemp -d) || die "could not create a working directory"
# PARTIAL is the new zip before it has passed its checks; see Re-packing.
PARTIAL=
trap 'rm -rf "$WORK"; [ -z "$PARTIAL" ] || rm -f "$PARTIAL"' EXIT
trap 'exit 1' HUP INT TERM

case "$INPUT" in
    *.zip)
        step "Unpacking $INPUT"
        # ditto, not unzip: it is the only extractor that restores a bundle's
        # metadata faithfully, and it is what Archive Utility uses.
        ditto -x -k "$INPUT" "$WORK" || die "could not unpack $INPUT"
        APP=$(find "$WORK" -maxdepth 1 -name '*.app' | head -1)
        [ -n "$APP" ] || die "no .app inside $INPUT"
        OUTDIR=$(cd "$(dirname "$INPUT")" && pwd)
        BASE=$(basename "$INPUT" .zip)
        ;;
    *.app)
        APP="$WORK/$(basename "$INPUT")"
        cp -R "$INPUT" "$APP" || die "could not copy $INPUT"
        OUTDIR=$(cd "$(dirname "$INPUT")" && pwd)
        BASE=$(basename "$INPUT" .app)
        ;;
    *) die "expected a .zip or a .app, got: $INPUT" ;;
esac
EXE="$APP/Contents/MacOS/patina"
[ -f "$EXE" ] || die "$(basename "$APP") has no Contents/MacOS/patina; this is for Patina.app"
echo "  bundle:    $(basename "$APP")"
echo "  arches:    $(lipo -archs "$EXE" 2>/dev/null || echo '?')"

# Anything the download picked up would be sealed into the signature.
xattr -cr "$APP"

# ----------------------------------------------------------------- contents
# The code in a Patina.app is its executable and the libraries in
# Contents/Frameworks (libmpv and everything it links, from macos-libmpv.txt),
# which is all macos-bundle.sh puts there and all this signs. Anything else
# that is code would go out unsigned and come back from the notary as a
# rejection, ten minutes later, so it stops the run here; so does anything in a
# directory under Frameworks, since a nested framework or bundle is signed as a
# bundle, which this does not do.
step "Checking what the bundle holds"
find "$APP/Contents" -type f >"$WORK/files"
LIBS=0
STRAY=
while IFS= read -r f; do
    case "$f" in
        "$EXE") ;;
        "$APP/Contents/Frameworks/"*/*) STRAY="$STRAY
    ${f#"$APP"/}" ;;
        "$APP/Contents/Frameworks/"*.dylib) LIBS=$((LIBS + 1)) ;;
        *)
            case $(file -b "$f") in
                *Mach-O*) STRAY="$STRAY
    ${f#"$APP"/}" ;;
            esac
            ;;
    esac
done <"$WORK/files"
[ -z "$STRAY" ] || die "the bundle holds code this does not know how to sign:$STRAY
  It signs Contents/MacOS/patina and the libraries directly in
  Contents/Frameworks, which is everything macos-bundle.sh puts in a bundle.
  Anything else has to be taught to it first; left unsigned, it fails
  notarisation."
[ -f "$APP/Contents/Frameworks/libmpv.2.dylib" ] \
    || die "no Contents/Frameworks/libmpv.2.dylib, the libmpv Patina loads from the bundle"
echo "  libraries: $LIBS in Contents/Frameworks"

# The one entitlement depends on how the bundled LuaJIT was built; the comment
# in packaging/macos-entitlements.plist has the whole story. A LuaJIT that calls
# pthread_jit_write_protect_np maps its machine code with MAP_JIT and needs
# allow-jit; one that does not makes ordinary memory executable and needs
# allow-unsigned-executable-memory. The wrong one is a notarised app that macOS
# kills once a Lua script runs hot enough to be compiled, which nothing after
# signing, notary and Gatekeeper included, would notice.
for lib in "$APP"/Contents/Frameworks/libluajit*.dylib; do
    [ -f "$lib" ] || continue
    SYMS=$(nm -u "$lib" 2>&1) || die "nm could not read $(basename "$lib"):
$(printf '%s\n' "$SYMS" | sed 's/^/    /')"
    case "$SYMS" in
        *_pthread_jit_write_protect_np*) NEED=com.apple.security.cs.allow-jit ;;
        *) NEED=com.apple.security.cs.allow-unsigned-executable-memory ;;
    esac
    grep -qF "<key>$NEED</key>" "$ENTITLEMENTS" \
        || die "$(basename "$lib") needs $NEED,
  and $ENTITLEMENTS does not grant it.
  The comment in that file says how to tell which one LuaJIT needs, and why."
    echo "  luajit:    $(basename "$lib") needs $NEED, which is granted"
done

# --------------------------------------------------------------------- sign
# Inside-out, and deliberately no --deep. Apple deprecates --deep for signing:
# it applies one identity and one set of options to whatever it finds, which is
# the wrong model. Each library is signed first, on its own, and the bundle
# last: its seal records the signature of every piece of code nested in it, so
# signing anything inside it afterwards would break the seal. (--deep IS still
# correct for verifying, below.)
#
# The libraries are IINA's builds, signed ad hoc by macos-bundle.sh, which
# --force replaces. Signed with the app's own Developer ID, they pass the
# hardened runtime's library validation when Patina loads libmpv and libmpv
# loads the rest, which is why the entitlements can leave
# disable-library-validation out. They get no entitlements: those belong to a
# process, are read from its main executable, and mean nothing on a library.
step "Signing"
for lib in "$APP"/Contents/Frameworks/*.dylib; do
    if [ ! -f "$lib" ] || [ -L "$lib" ]; then continue; fi
    sign "Frameworks/$(basename "$lib")" "$lib"
done
sign "$(basename "$APP")" --entitlements "$ENTITLEMENTS" "$APP"

run codesign --verify --deep --strict --verbose=2 "$APP" \
    || die "the signature did not verify immediately after signing"

# ----------------------------------------------------------------- notarise
# The zip submitted here is scaffolding, never the artifact that ships: the
# ticket is stapled into the .app afterwards, so this zip is already stale by
# the time notarisation returns.
step "Notarising (this usually takes a few minutes)"
SUBMIT="$WORK/submit.zip"
ditto -c -k --keepParent "$APP" "$SUBMIT"

set +e
OUT=$(xcrun notarytool submit "$SUBMIT" --keychain-profile "$PROFILE" --wait 2>&1)
RC=$?
set -e
echo "$OUT" | sed 's/^/  /'

ID=$(echo "$OUT" | sed -n 's/.*id: \([0-9a-f-][0-9a-f-]*\).*/\1/p' | head -1)
if [ $RC -ne 0 ] || ! echo "$OUT" | grep -q "status: Accepted"; then
    if [ -n "$ID" ]; then
        step "Notarisation log for $ID"
        xcrun notarytool log "$ID" --keychain-profile "$PROFILE" 2>&1 | sed 's/^/  /'
    fi
    die "notarisation did not return Accepted — see the log above"
fi

# ------------------------------------------------------------------- staple
# The .app, never the zip: stapling an archive is not a weaker form of
# stapling, it is unsupported. The ticket is keyed by cdhash rather than by
# container, which is why stapling the bundle works after submitting a zip of
# it, and why a universal binary gets an entry per architecture.
#
# Stapling is what makes the app open on a Mac that is offline or that cannot
# reach Apple: without it Gatekeeper has to ask, and a machine behind a captive
# portal or a firewall gets the same refusal as an unsigned build.
step "Stapling the ticket into the bundle"
run xcrun stapler staple "$APP" || die "stapling failed"

# The order matters and is the step most often missed: the zip that ships has
# to be made AFTER stapling. Re-using the submission zip ships an unstapled app.
#
# Named for the input without -unsigned, and made under a hidden name beside
# that, moved there only once every check below has passed. A zip may already
# have the shipping name, from an earlier run or because the input itself does,
# and writing straight to it would destroy that before anything was known
# about the result, and leave a rejected zip under the name the release
# expects. Beside it rather than in $WORK so that the move is a rename within
# one volume: the zip under the shipping name is only ever the old one or the
# whole new one.
step "Re-packing"
FINAL="$OUTDIR/${BASE%-unsigned}.zip"
PARTIAL="$OUTDIR/.${BASE%-unsigned}.zip.partial"
rm -f "$PARTIAL"
ditto -c -k --keepParent "$APP" "$PARTIAL" || die "could not write $PARTIAL"

# ------------------------------------------------------------------- verify
# The checks a stranger's Mac will make, run here so a bad bundle is caught now
# rather than by the first person to download it. They run on the zip that
# will ship, unpacked afresh, not on the bundle it was made from, so a re-pack
# that lost the ticket or broke the seal is caught too. Each one stops the run
# through run(), because a check piped straight into sed has its failure
# swallowed like any other; and when one does, nothing has been replaced.
#
# spctl's exit status alone is not enough: on a Mac where Gatekeeper
# assessments have been turned off it accepts anything, with an override where
# the source would be. So its verdict has to name the source as well,
# "Notarized Developer ID". Neither says the ticket is in the bundle, since
# online Gatekeeper fetches it from Apple instead; stapler validate is the
# check for that, which is why it has to be able to stop the run too.
#
# None of them looks at entitlements, and a missing one shows only when macOS
# kills the running app, so the shipped executable's are compared with the
# file's here as well.
step "Verifying the zip that will ship"
mkdir "$WORK/check"
ditto -x -k "$PARTIAL" "$WORK/check" || die "could not unpack $PARTIAL"
SHIP="$WORK/check/$(basename "$APP")"
[ -d "$SHIP" ] || die "$PARTIAL does not hold $(basename "$APP")"
KEPT="Nothing was written to $FINAL."

echo "  --- stapler validate (is the ticket actually in the bundle?) ---"
run xcrun stapler validate "$SHIP" \
    || die "the zip that would ship has no valid stapled ticket.
  $KEPT"
echo "  --- codesign (is the seal intact, including nested content?) ---"
run codesign --verify --deep --strict --verbose=2 "$SHIP" \
    || die "the signature in the zip that would ship does not verify.
  $KEPT"
echo "  --- entitlements (exactly those in packaging/macos-entitlements.plist?) ---"
run codesign -d --entitlements - --xml "$SHIP" \
    || die "could not read the entitlements of the bundle that would ship.
  $KEPT"
GOT=$(printf '%s\n' "$_out" | grep -o '<key>[^<]*</key>' | sort)
EXPECTED=$(grep -o '<key>[^<]*</key>' "$ENTITLEMENTS" | sort)
[ "$GOT" = "$EXPECTED" ] \
    || die "the bundle that would ship does not carry the entitlements in
  $ENTITLEMENTS
  and no others. What codesign read is above.
  $KEPT"
echo "  --- spctl (what Gatekeeper decides) ---"
run spctl -a -vvv -t exec "$SHIP" \
    || die "Gatekeeper rejects the bundle — do not ship this.
  $KEPT"
case "$_out" in
    *"source=Notarized Developer ID"*) ;;
    *) die "Gatekeeper let the bundle through, but not as notarised Developer ID;
  what it said instead is above. Do not ship this.
  $KEPT" ;;
esac

mv -f "$PARTIAL" "$FINAL" || die "could not move $PARTIAL to $FINAL"
PARTIAL=

printf '\n'
printf 'done: %s\n' "$FINAL"
printf '  shasum: %s\n' "$(shasum -a 256 "$FINAL" | awk '{print $1}')"
printf '\nVerify it elsewhere before shipping — on a Mac that has never held the\n'
printf 'signing key, quarantine it the way a download would and open it:\n\n'
# shellcheck disable=SC2016  # deliberately literal: this is text to paste, not to run here
printf '  xattr -w com.apple.quarantine "0083;00000000;Safari;$(uuidgen)" Patina.app\n'
printf '  spctl -a -vvv -t exec Patina.app && open Patina.app\n'
printf '\n%s\n%s\n%s\n' \
    "Then use it for a while, an online video included: mpv's Lua scripts run" \
    "under LuaJIT, and an entitlement LuaJIT lacks shows only as macOS killing" \
    "Patina once a script runs hot enough to be compiled."

# The tag is in the name, for a release build; a build named by its commit is
# not for a release.
NAME=$(basename "$FINAL")
case "$NAME" in
    patina-v*-macos-universal.zip)
        TAG=${NAME#patina-}
        TAG=${TAG%-macos-universal.zip}
        printf '\nThen attach it to the draft release:\n\n'
        printf '  gh release upload %s %s --repo jzbz/patina\n' "$TAG" "$FINAL"
        ;;
esac
