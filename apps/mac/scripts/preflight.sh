#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_DIR="$(cd "$APP_DIR/../.." && pwd)"
XCODE_DIR="$APP_DIR/xcode"

RELEASE_VERSION="${1:-}"
if [[ -z "$RELEASE_VERSION" ]]; then
  RELEASE_VERSION="$(tr -d '[:space:]' < "$APP_DIR/VERSION")"
fi

if [[ -z "$RELEASE_VERSION" ]]; then
  echo "Release version is empty. Pass one explicitly or set apps/mac/VERSION." >&2
  exit 1
fi

if [[ ! "$RELEASE_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "Release version '$RELEASE_VERSION' must be strict semver (X.Y.Z)." >&2
  exit 1
fi

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "This preflight must run on macOS." >&2
  exit 1
fi

for cmd in aws xcodegen xcodebuild codesign hdiutil xcrun ditto python3 security git create-dmg just cargo; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "Missing required command: $cmd" >&2
    exit 1
  fi
done

# if ! git -C "$REPO_DIR" diff-index --quiet HEAD --; then
#   echo "Working tree is not clean (tracked changes present). Commit or stash before releasing." >&2
#   exit 1
# fi
#
# if [[ -n "$(git -C "$REPO_DIR" ls-files --others --exclude-standard)" ]]; then
#   echo "Working tree is not clean (untracked files present). Commit, stash, or clean before releasing." >&2
#   exit 1
# fi

NOTARY_PROFILE="${VX_MAC_NOTARY_PROFILE:-}"
SPARKLE_ACCOUNT="${VX_MAC_SPARKLE_KEY_ACCOUNT:-ed25519}"
SPARKLE_PUBLIC_ED_KEY="${VX_MAC_SPARKLE_PUBLIC_ED_KEY:-${VX_MAC_SPARKLE_PUBLIC_KEY:-}}"
R2_BUCKET_URL="${VX_MAC_R2_BUCKET_URL:-https://4051116950f6ef4b78cb96847a2221f9.r2.cloudflarestorage.com/vixen-mac-app}"
PUBLIC_BASE_URL="${VX_MAC_PUBLIC_BASE_URL:-}"
R2_ACCESS_KEY_ID="${AWS_ACCESS_KEY_ID:-${ACCESS_KEY_ID:-}}"
R2_SECRET_ACCESS_KEY="${AWS_SECRET_ACCESS_KEY:-${SECRET_ACCESS_KEY:-}}"
TAG="v${RELEASE_VERSION}"

if [[ -z "$NOTARY_PROFILE" ]]; then
  echo "Set VX_MAC_NOTARY_PROFILE to your notarytool keychain profile name." >&2
  exit 1
fi

if [[ -z "$SPARKLE_PUBLIC_ED_KEY" ]]; then
  echo "Set VX_MAC_SPARKLE_PUBLIC_ED_KEY, or legacy VX_MAC_SPARKLE_PUBLIC_KEY, to the Sparkle Ed25519 public key used by your private keychain key." >&2
  exit 1
fi

if [[ -z "$PUBLIC_BASE_URL" ]]; then
  echo "Set VX_MAC_PUBLIC_BASE_URL to the public HTTPS root for the mac app bucket." >&2
  exit 1
fi

if [[ -z "$R2_BUCKET_URL" ]]; then
  echo "Set VX_MAC_R2_BUCKET_URL to the R2 S3 endpoint including the bucket name." >&2
  exit 1
fi

if [[ -z "$R2_ACCESS_KEY_ID" || -z "$R2_SECRET_ACCESS_KEY" ]]; then
  echo "Set ACCESS_KEY_ID and SECRET_ACCESS_KEY, or AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY, for the Cloudflare R2 token." >&2
  exit 1
fi

if ! xcrun notarytool history --keychain-profile "$NOTARY_PROFILE" >/dev/null 2>&1; then
  echo "notarytool keychain profile '$NOTARY_PROFILE' is unavailable or invalid." >&2
  exit 1
fi

if ! security find-generic-password \
  -a "$SPARKLE_ACCOUNT" \
  -s "https://sparkle-project.org" \
  >/dev/null 2>&1; then
  echo "Sparkle keychain account '$SPARKLE_ACCOUNT' is missing. Run Sparkle generate_keys first." >&2
  exit 1
fi

if [[ ! -f "$XCODE_DIR/project.yml" ]]; then
  echo "Missing $XCODE_DIR/project.yml" >&2
  exit 1
fi

echo "Preflight passed:"
echo "  version:        $RELEASE_VERSION"
echo "  notary profile: $NOTARY_PROFILE"
echo "  sparkle acct:   $SPARKLE_ACCOUNT"
echo "  r2 bucket url:  $R2_BUCKET_URL"
echo "  public base:    $PUBLIC_BASE_URL"
