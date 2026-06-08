# Homebrew cask for Vixen.
#
# This is the canonical source for the cask; copy/sync it into the Homebrew tap repo
# (e.g. `homebrew-vixen/Casks/vixen.rb`) so users can `brew install --cask vixen`.
#
# Vixen updates itself in place via Sparkle, so the cask tracks the "latest" DMG rather
# than pinning a version+sha — `auto_updates true` tells Homebrew not to fight Sparkle.
cask "vixen" do
  version :latest
  sha256 :no_check

  url "https://mac.app.vixen.rs/latest/Vixen.dmg"
  name "Vixen"
  desc "Hermetic, reproducible build cache backed by the VixenFS virtual filesystem"
  homepage "https://vixen.rs/"

  auto_updates true
  depends_on macos: ">= :sequoia"

  app "Vixen.app"

  # Vixen mounts VixenFS at /Volumes/Vixen/vfs (created once, as root, via an in-app
  # admin prompt) and unmounts it on quit. The mountpoint lives under root-owned
  # /Volumes, so `brew uninstall` can't remove it — the app's own uninstall handles
  # that. `zap` clears the user-owned state we can reach without privilege.
  zap trash: [
    "~/Library/Application Support/Vixen",
    "~/Library/Group Containers/B2N6FSRTPV.vixenfs",
    "~/Library/Logs/Vixen",
    "~/Library/Preferences/eu.bearcove.vixen.plist",
    "~/Library/Caches/eu.bearcove.vixen",
  ]
end
