#!/usr/bin/env bash
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

TEAM_ID="B2N6FSRTPV"
APP_IDENTIFIER="eu.bearcove.vixen"
DAEMON_LABEL="eu.bearcove.vixen.management"

SOURCE_APP="${VX_MAC_PKG_APP:-$APP_BUNDLE_PATH}"
PKG_BUILD="$DIST_DIR/pkg"
PAYLOAD_ROOT="$PKG_BUILD/payload"
PKG_SCRIPTS="$PKG_BUILD/scripts"
COMPONENT_PLIST="$PKG_BUILD/component.plist"
COMPONENT_PKG="$PKG_BUILD/vixen-component.pkg"
DIST_XML="$PKG_BUILD/Distribution.xml"
UNSIGNED_PKG="$DIST_DIR/Vixen-Installer-unsigned.pkg"
SIGNED_PKG="$DIST_DIR/Vixen-Installer.pkg"
NOTARY_OUT="$DIST_DIR/notary-pkg.log"
POSTINSTALL_SRC="$APP_DIR/installer/scripts/postinstall"

fail() {
  echo "error: $*" >&2
  exit 1
}

detect_installer_identity() {
  local requested="${VX_MAC_INSTALLER_IDENTITY:-Developer ID Installer}"
  resolve_codesign_identity "$requested" "Developer ID Installer" || true
}

resolve_notary_auth() {
  if [[ -n "${VX_MAC_NOTARY_KEY:-}" && -n "${VX_MAC_NOTARY_KEY_ID:-}" && -n "${VX_MAC_NOTARY_ISSUER:-}" ]]; then
    NOTARY_AUTH=(--key "$VX_MAC_NOTARY_KEY" --key-id "$VX_MAC_NOTARY_KEY_ID" --issuer "$VX_MAC_NOTARY_ISSUER")
    NOTARY_METHOD="App Store Connect API key"
    return 0
  fi
  if [[ -n "${VX_MAC_NOTARY_APPLE_ID:-}" && -n "${VX_MAC_NOTARY_PASSWORD:-}" ]]; then
    NOTARY_AUTH=(--apple-id "$VX_MAC_NOTARY_APPLE_ID" --team-id "${VX_MAC_NOTARY_TEAM_ID:-$TEAM_ID}" --password "$VX_MAC_NOTARY_PASSWORD")
    NOTARY_METHOD="Apple ID + app-specific password"
    return 0
  fi
  if [[ -n "$NOTARY_PROFILE" ]]; then
    NOTARY_AUTH=(--keychain-profile "$NOTARY_PROFILE")
    NOTARY_METHOD="keychain profile '$NOTARY_PROFILE'"
    return 0
  fi
  return 1
}

print_installer_cert_help() {
  cat >&2 <<EOF
No "Developer ID Installer" certificate found.

Create it once:
  1. https://developer.apple.com/account/resources/certificates/list
  2. "+" -> "Developer ID Installer" -> create or upload a CSR.
  3. Download and double-click the .cer to install it into the login keychain.
  4. Verify: security find-identity -v | grep "Developer ID Installer"
  5. Re-run this script.

Built the unsigned pkg for inspection:
  $UNSIGNED_PKG
EOF
}

for cmd in pkgbuild productbuild codesign ditto security xcrun; do
  command -v "$cmd" >/dev/null 2>&1 || fail "missing required command: $cmd"
done

[[ -d "$SOURCE_APP" ]] || fail "missing Vixen.app at $SOURCE_APP; run just release-build $RELEASE_VERSION first"
[[ -x "$SOURCE_APP/Contents/MacOS/vixen-management" ]] || fail "missing bundled helper at $SOURCE_APP/Contents/MacOS/vixen-management"
[[ -x "$POSTINSTALL_SRC" ]] || fail "missing executable $POSTINSTALL_SRC"

app_sig="$(codesign -dvvv "$SOURCE_APP" 2>&1 || true)"
case "$app_sig" in
  *"Authority=Developer ID Application"*) ;;
  *) fail "$SOURCE_APP is not Developer ID-signed; run just release-build $RELEASE_VERSION first" ;;
esac

INSTALLER_IDENTITY="$(detect_installer_identity)"
NOTARY_AUTH=()
NOTARY_METHOD=""
if ! resolve_notary_auth && [[ -n "$INSTALLER_IDENTITY" ]]; then
  fail "have an installer certificate but no notarization credentials configured"
fi

echo "Installer identity : ${INSTALLER_IDENTITY:-<none; unsigned pkg only>}"
echo "Notarization auth  : ${NOTARY_METHOD:-<none>}"
echo "Version            : $RELEASE_VERSION"
echo "Vixen.app          : $SOURCE_APP"

rm -rf "$PKG_BUILD" "$UNSIGNED_PKG" "$SIGNED_PKG"
mkdir -p "$PAYLOAD_ROOT/Applications" "$PKG_SCRIPTS"

ditto "$SOURCE_APP" "$PAYLOAD_ROOT/Applications/Vixen.app"
install -m 755 "$POSTINSTALL_SRC" "$PKG_SCRIPTS/postinstall"

pkgbuild --analyze --root "$PAYLOAD_ROOT" "$COMPONENT_PLIST"
/usr/libexec/PlistBuddy -c 'Set :0:BundleIsRelocatable false' "$COMPONENT_PLIST" 2>/dev/null || true

pkgbuild \
  --root "$PAYLOAD_ROOT" \
  --component-plist "$COMPONENT_PLIST" \
  --identifier "$APP_IDENTIFIER" \
  --version "$RELEASE_VERSION" \
  --install-location "/" \
  --scripts "$PKG_SCRIPTS" \
  "$COMPONENT_PKG"

cat > "$DIST_XML" <<EOF
<?xml version="1.0" encoding="utf-8"?>
<installer-gui-script minSpecVersion="2">
  <title>Vixen</title>
  <options customize="never" require-scripts="true" hostArchitectures="arm64"/>
  <os-version min="15.0"/>
  <choices-outline>
    <line choice="default">
      <line choice="$APP_IDENTIFIER"/>
    </line>
  </choices-outline>
  <choice id="default"/>
  <choice id="$APP_IDENTIFIER" visible="false">
    <pkg-ref id="$APP_IDENTIFIER"/>
  </choice>
  <pkg-ref id="$APP_IDENTIFIER" version="$RELEASE_VERSION" onConclusion="none">$(basename "$COMPONENT_PKG")</pkg-ref>
</installer-gui-script>
EOF

productbuild \
  --distribution "$DIST_XML" \
  --package-path "$PKG_BUILD" \
  "$UNSIGNED_PKG"

if [[ -z "$INSTALLER_IDENTITY" ]]; then
  print_installer_cert_help
  exit 0
fi

productsign --sign "$INSTALLER_IDENTITY" "$UNSIGNED_PKG" "$SIGNED_PKG"

set +e
xcrun notarytool submit "$SIGNED_PKG" "${NOTARY_AUTH[@]}" --wait 2>&1 | tee "$NOTARY_OUT"
notary_status=${PIPESTATUS[0]}
set -e
if [[ "$notary_status" -ne 0 ]]; then
  fail "notarytool submit failed; see $NOTARY_OUT"
fi

SUBMIT_ID="$(awk '/^[[:space:]]*id:/{print $2; exit}' "$NOTARY_OUT")"
STATUS="$(awk -F': ' '/^[[:space:]]+status:/{s=$2} END{print s}' "$NOTARY_OUT" | tr -d '[:space:]')"
if [[ -z "$SUBMIT_ID" || "$STATUS" != "Accepted" ]]; then
  if [[ -n "$SUBMIT_ID" ]]; then
    xcrun notarytool log "$SUBMIT_ID" "${NOTARY_AUTH[@]}" || true
  fi
  fail "notarization status: ${STATUS:-unknown}; see $NOTARY_OUT"
fi

xcrun stapler staple "$SIGNED_PKG"

spctl -a -vvv -t install "$SIGNED_PKG" 2>&1 || true
pkgutil --check-signature "$SIGNED_PKG" 2>&1 || true
xcrun stapler validate "$SIGNED_PKG" 2>&1 || true

echo "Signed + notarized installer: $SIGNED_PKG"
