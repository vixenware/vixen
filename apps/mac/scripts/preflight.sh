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

for cmd in gh xcodegen xcodebuild codesign hdiutil xcrun ditto python3 security git create-dmg; do
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
RELEASE_REPO="${VX_MAC_RELEASE_REPO:-bearcove/vixen-mac-app}"
SPARKLE_ACCOUNT="${VX_MAC_SPARKLE_KEY_ACCOUNT:-ed25519}"
SPARKLE_PUBLIC_ED_KEY="${VX_MAC_SPARKLE_PUBLIC_ED_KEY:-}"
TAG="v${RELEASE_VERSION}"

if [[ -z "$NOTARY_PROFILE" ]]; then
  echo "Set VX_MAC_NOTARY_PROFILE to your notarytool keychain profile name." >&2
  exit 1
fi

if [[ -z "$SPARKLE_PUBLIC_ED_KEY" ]]; then
  echo "Set VX_MAC_SPARKLE_PUBLIC_ED_KEY to the Sparkle Ed25519 public key used by your private keychain key." >&2
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

if ! gh repo view "$RELEASE_REPO" >/dev/null 2>&1; then
  echo "Cannot access GitHub repo '$RELEASE_REPO' with current gh auth." >&2
  exit 1
fi

if gh release view "$TAG" --repo "$RELEASE_REPO" >/dev/null 2>&1; then
  echo "Release $TAG already exists in $RELEASE_REPO. Use a new version before releasing." >&2
  exit 1
fi

if gh api "repos/${RELEASE_REPO}/git/ref/tags/${TAG}" >/dev/null 2>&1; then
  echo "Tag $TAG already exists in $RELEASE_REPO. Use a new version before releasing." >&2
  exit 1
fi

if [[ ! -f "$XCODE_DIR/project.yml" ]]; then
  echo "Missing $XCODE_DIR/project.yml" >&2
  exit 1
fi

echo "Preflight passed:"
echo "  version:        $RELEASE_VERSION"
echo "  release repo:   $RELEASE_REPO"
echo "  notary profile: $NOTARY_PROFILE"
echo "  sparkle acct:   $SPARKLE_ACCOUNT"
