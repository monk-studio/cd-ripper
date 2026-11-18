cask "cdr" do
  version "0.1.0"
  sha256 "fabefa669a42283f9dbe8763698b395c463414b7d846b02bd1500dbf8007e1a5"

  url "https://gitlab.com/monk-studio/cd-ripper/-/releases/v#{version}/downloads/CDR-#{version}.dmg"
  name "CDR"
  desc "CD ripper that verifies every track against AccurateRip"
  homepage "https://gitlab.com/monk-studio/cd-ripper"

  livecheck do
    url "https://gitlab.com/monk-studio/cd-ripper.git"
    strategy :git
  end

  depends_on macos: ">= :sonoma"

  app "CDR.app"

  uninstall quit: "com.monk-studio.cdr"

  zap trash: "~/Library/Application Support/com.monk-studio.cdr"
end
