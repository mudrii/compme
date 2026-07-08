cask "compme" do
  version "0.1.0"
  # Placeholder until the first tagged release; tools/release/update-cask.sh
  # rewrites both lines from the published artifact (see docs/RELEASING.md).
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"

  url "https://github.com/mudrii/compme/releases/download/v#{version}/compme-#{version}-macos.zip"
  name "Compme"
  desc "Open-source local inline text-completion engine for macOS"
  homepage "https://github.com/mudrii/compme"

  depends_on macos: ">= :sonoma"

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
