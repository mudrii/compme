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
    Compme is ad-hoc signed (no Apple Developer ID / notarization yet), so macOS
    Gatekeeper quarantines it on first launch. Clear the quarantine flag once:

      xattr -dr com.apple.quarantine "#{appdir}/Compme.app"

    Then open Compme and grant it Accessibility access in
    System Settings -> Privacy & Security -> Accessibility. All inference is
    local; nothing is sent off the machine.
  EOS
end
