cask "compme" do
  version "0.1.2"
  # sha256 is rewritten from the published artifact by
  # tools/release/update-cask.sh during each tag release (see docs/RELEASING.md).
  sha256 "3ccd3320fc881031185489b9cafb4a16bd2d609ff0c18b8eeb4ab50b585f15b2"

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
