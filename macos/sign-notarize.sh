#!/bin/bash
# Codesigning + notarization for build-dmg.sh.
#
# Set SIGNING_IDENTITY to a "Developer ID Application: ..." identity (and
# store notarytool credentials under NOTARY_PROFILE, default
# mac-gliff-notary) to sign for distribution: hardened runtime, secure
# timestamp, notarization and a stapled ticket on both the app and the DMG.
# Without SIGNING_IDENTITY everything stays ad-hoc signed, as before:
# Gatekeeper asks on the first launch of a downloaded copy.
#
# Written for macOS's bash 3.2.
set -eu

NOTARY_PROFILE=${NOTARY_PROFILE:-mac-gliff-notary}
IDENTITY=${SIGNING_IDENTITY:-}

say() { echo "sign-notarize: $*"; }

# Submit for notarization and wait for the verdict; print the log on
# rejection so the offending file is named right in the CI output.
notarize() { # <archive>
    local submission
    submission=$(xcrun notarytool submit "$1" --keychain-profile "$NOTARY_PROFILE" --wait 2>&1) || {
        echo "$submission" >&2
        die "notarytool rejected or failed to submit $1"
    }
    say "$submission" | tail -1
}

die() { echo "sign-notarize: $*" >&2; exit 1; }

cmd_machos() { # <file...> — inner Mach-Os, signed before the bundle
    if [ -n "$IDENTITY" ]; then
        codesign --force --timestamp --options runtime --sign "$IDENTITY" "$@"
    else
        codesign --force --sign - "$@"
    fi
}

cmd_app() { # <Gliff.app> — bundle sign; notarize + staple when distributing
    if [ -n "$IDENTITY" ]; then
        codesign --force --timestamp --options runtime --sign "$IDENTITY" "$1"
    else
        codesign --force --sign - "$1"
    fi
    codesign --verify --strict "$1"
    if [ -z "$IDENTITY" ]; then
        say "ad-hoc signed only (SIGNING_IDENTITY unset)"
        return 0
    fi
    local zip
    zip=$(mktemp -t notary).zip
    trap 'rm -f "$zip"' RETURN
    ditto -c -k --keepParent "$1" "$zip"
    notarize "$zip"
    xcrun stapler staple "$1"
}

cmd_dmg() { # <image.dmg> — sign, notarize and staple the image itself
    if [ -z "$IDENTITY" ]; then
        return 0
    fi
    codesign --force --timestamp --sign "$IDENTITY" "$1"
    notarize "$1"
    xcrun stapler staple "$1"
}

case "${1:-}" in
    machos) shift; [ $# -gt 0 ] || die "machos: no files"; cmd_machos "$@";;
    app)    [ -n "${2:-}" ] || die "app: missing path";     cmd_app "$2";;
    dmg)    [ -n "${2:-}" ] || die "dmg: missing path";     cmd_dmg "$2";;
    *) die "usage: sign-notarize.sh machos <file...> | app <Gliff.app> | dmg <image.dmg>";;
esac
