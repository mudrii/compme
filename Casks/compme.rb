cask "compme" do
  version "0.1.1"
  # sha256 is rewritten from the published artifact by
  # tools/release/update-cask.sh during the tag release (see docs/RELEASING.md);
  # the value below is the previous release's until v0.1.1 finalizes.
  sha256 "de60fbf0cf2ca96b1d98b42e6e4b54cd5ddd406b9a1499619f1787a0f2778660"

  url "https://github.com/mudrii/compme/releases/download/v#{version}/compme-#{version}-macos.zip"
  name "Compme"
  desc "Open-source local inline text-completion engine for macOS"
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
