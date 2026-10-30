# Homebrew cask for Patina.
#
# This file exists twice and update-cask.sh keeps the two in step: the template
# at packaging/homebrew/patina.rb in the app repo, which versions with the thing
# it describes, and the published cask at github.com/jzbz/homebrew-tap in
# Casks/patina.rb, which is that template with the release's version and hash
# filled in.
#
#   brew install --cask jzbz/tap/patina
#
# A tap rather than homebrew-cask because homebrew-cask applies a notability
# bar, and at 3x for a self-submission that is 225 stars, 90 forks or 90
# watchers, plus a 30-day repository age. A tap has none of that. What a tap
# does NOT escape is Gatekeeper: brew applies com.apple.quarantine on install
# whatever tap a cask came from, --no-quarantine was removed in Homebrew 4.7,
# and the `quarantine` stanza no longer exists in the DSL. So the notarised
# zip is what makes this work — an unsigned one would install and then refuse
# to open, which is worse than not offering it.
cask "patina" do
  version "0.1.0"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"

  url "https://github.com/jzbz/patina/releases/download/v#{version}/patina-v#{version}-macos-universal.zip",
      verified: "github.com/jzbz/patina/"
  name "Patina"
  desc "Media player powered by mpv"
  homepage "https://patina.video/"

  livecheck do
    url :url
    strategy :github_latest
  end

  # Matches LSMinimumSystemVersion in the bundle's Info.plist, set by
  # packaging/macos-bundle.sh: the libmpv and libraries in Contents/Frameworks
  # are built for macOS 11 and later.
  depends_on macos: :big_sur

  app "Patina.app"
  # `patina` on the PATH, to open files from a terminal. Patina resolves this
  # link to the bundle's own executable before looking for the libmpv in
  # Contents/Frameworks (src/mpv/ffi.rs), so it runs exactly as the app does.
  binary "#{appdir}/Patina.app/Contents/MacOS/patina"

  # Complete: --zap is a request for everything to go. Application Support
  # holds settings, history and resume positions, thumbnails and downloaded
  # subtitles, all of which using the app rebuilds, and also the mpv.conf,
  # scripts and key binding sets someone may have written by hand under its
  # mpv/ and input_conf/; whoever wants those kept copies them out before
  # zapping. The two AppKit files are its saved window state and its defaults.
  #
  # Not listed, because they are the person's own files or not files at all:
  # screenshots (~/Pictures/Patina unless moved), downloaded videos, and the
  # OpenSubtitles password, a "Patina OpenSubtitles" item in the login
  # keychain, which a cask cannot remove.
  zap trash: [
    "~/Library/Application Support/video.patina.Patina",
    "~/Library/Preferences/video.patina.Patina.plist",
    "~/Library/Saved Application State/video.patina.Patina.savedState",
  ]
end
