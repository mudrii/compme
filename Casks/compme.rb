cask "compme" do
  version "0.1.7"
  # version and sha256 are rewritten from the published artifact by
  # tools/release/update-cask.sh during each tag release (see docs/RELEASING.md).
  sha256 "762afa19ea8c19cc45e62fde6dd41c1ded2939bc0e3f36eec8550aaa8d5d4fbb"

  url "https://github.com/mudrii/compme/releases/download/v#{version}/compme-#{version}-macos.zip"
  name "Compme"
  desc "Open-source local inline text-completion engine"
  homepage "https://github.com/mudrii/compme"

  depends_on macos: :sonoma
  depends_on arch: :arm64

  app "Compme.app"

  caveats <<~EOS
    Open Compme and grant it Accessibility access in
    System Settings -> Privacy & Security -> Accessibility.

    Use the menu-bar item "Check for Updates…" to open the latest GitHub
    release. Inference and prompt context stay local; model downloads and the
    update link use the network. Compme sends no typed text or telemetry.
  EOS
end
