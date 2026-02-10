#!/usr/bin/env bash
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

if [[ ! -f "$DMG_PATH" ]]; then
  echo "DMG not found at $DMG_PATH — run 'just build $RELEASE_VERSION' first." >&2
  exit 1
fi

SUBMISSION_ID="${VX_MAC_SUBMISSION_ID:-}"

if [[ -n "$SUBMISSION_ID" ]]; then
  echo "Resuming with existing submission ID: $SUBMISSION_ID"
else
  echo "Submitting DMG for notarization..."
  SUBMIT_OUTPUT="$(xcrun notarytool submit "$DMG_PATH" --keychain-profile "$NOTARY_PROFILE" 2>&1)" || true
  echo "$SUBMIT_OUTPUT"

  if echo "$SUBMIT_OUTPUT" | grep -qi 'Error:'; then
    echo "Submission failed. Output above contains an error." >&2
    exit 1
  fi

  if ! echo "$SUBMIT_OUTPUT" | grep -q 'Successfully uploaded file'; then
    echo "Submission did not complete successfully (missing 'Successfully uploaded file')." >&2
    exit 1
  fi

  SUBMISSION_ID="$(echo "$SUBMIT_OUTPUT" | awk '/  id:/{print $2; exit}')"

  if [[ -z "${SUBMISSION_ID:-}" ]]; then
    echo "No submission ID returned; cannot proceed." >&2
    exit 1
  fi
fi

echo "Submission ID: $SUBMISSION_ID"
echo "Polling for notarization result (timeout: 4 hours)..."

DEADLINE=$(( $(date +%s) + 14400 ))
POLL_INTERVAL=15

while :; do
  if [[ $(date +%s) -ge $DEADLINE ]]; then
    echo "Timed out waiting for notarization after 4 hours." >&2
    echo "Fetching log for submission $SUBMISSION_ID..."
    xcrun notarytool log "$SUBMISSION_ID" --keychain-profile "$NOTARY_PROFILE" 2>/dev/null || true
    exit 1
  fi

  INFO="$(xcrun notarytool info "$SUBMISSION_ID" --keychain-profile "$NOTARY_PROFILE" 2>&1)" || true

  if echo "$INFO" | grep -q 'status: Accepted'; then
    echo "$INFO"
    echo "Notarization accepted."
    break
  fi

  if echo "$INFO" | grep -q 'status: Invalid'; then
    echo "$INFO"
    echo ""
    echo "Notarization failed. Fetching log for submission $SUBMISSION_ID..."
    xcrun notarytool log "$SUBMISSION_ID" --keychain-profile "$NOTARY_PROFILE"
    exit 1
  fi

  if echo "$INFO" | grep -q 'status: In Progress'; then
    printf "."
  else
    # Transient error (connectTimeout, etc.) — keep polling
    printf "x"
  fi

  sleep "$POLL_INTERVAL"
done

echo "Stapling notarization ticket..."
xcrun stapler staple "$DMG_PATH"

ditto "$DMG_PATH" "$SPARKLE_DIR/"

echo ""
echo "Notarization complete:"
echo "  DMG:     $DMG_PATH"
echo "  Staged:  $SPARKLE_DIR/"
echo ""
echo "Run 'just github-release $RELEASE_VERSION' to create the GitHub release."
