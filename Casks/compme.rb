cask "compme" do
  version "0.1.4"
  # sha256 is rewritten from the published artifact by
  # tools/release/update-cask.sh during each tag release (see docs/RELEASING.md).
  sha256 "90309ab37da849548a8b653919421dd6aecf0b216d80b7018b97a7b9295a58d9"

  url "https://github.com/mudrii/compme/releases/download/v#{version}/compme-#{version}-macos.zip"
  name "Compme"
  desc "Open-source local inline text-completion engine"
  homepage "https://github.com/mudrii/compme"

  depends_on macos: :sonoma

  app "Compme.app"

  caveats <<~EOS
    Releases are currently ad-hoc signed (no Apple Developer ID yet), so
    Gatekeeper will block the first launch: approve it under
    System Settings -> Privacy & Security ("Open Anyway"), or install with
    --no-quarantine if you accept the trade-off.

    Open Compme and grant it Accessibility access in
    System Settings -> Privacy & Security -> Accessibility.

    Use the menu-bar item "Check for Updates…" to open the latest GitHub
    release. All inference is local; nothing is sent off the machine.
  EOS
end
