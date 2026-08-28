# A trailing slash and no extension is what makes vhs keep the take as its two
# lossless PNG sequences (frame-text-NNNNN.png, frame-cursor-NNNNN.png) instead
# of encoding it; it moves its scratch directory here by renaming it, so the
# parent has to exist and has to be on the same filesystem as $TMPDIR — see
# demo/scripts/record.sh, which arranges both. No mp4 is asked for on purpose:
# h264 is 4:2:0, and the chroma smear on thin coloured glyphs would be baked in
# before the GIF's palette pass ever ran.
Output "demo/.out/raw/"

Require docker

Set Shell "bash"
# LazyVim's tree, tabline and statusline are drawn with Nerd Font glyphs; the
# default font renders every one of them as a tofu box.
Set FontFamily "FiraCode Nerd Font Mono"
# The width, height and font size together fix the terminal grid: wide and
# tall enough that LazyVim's tree, tabline and statusline frame a real editing
# view rather than a cropped one. The rows buy nothing for the install log,
# whose tail is a fixed count of lines the terminal size cannot move; they are
# spent on the editor. Changing any of the three changes the
# grid; measure it with `echo $(tput cols)x$(tput lines)` inside a take rather
# than deriving it. The GIF is not rescaled, so this width is also the GIF's
# width and the README img width.
Set FontSize 15
Set Width 1320
Set Height 1260
# 50ms lands exactly two keystrokes in every 100ms frame. A speed that does not
# divide the frame period evenly (40ms lands 2.5 chars per frame) alternates 2
# and 3 characters per frame, which reads as stuttering rather than typing.
Set TypingSpeed 50ms
Set Padding 20

# The GIF is encoded at 50fps, so recording any faster only makes vhs's
# headless chromium screenshot the viewport twice as often for frames
# the encoder throws away — and a take that cannot sustain its declared rate
# plays back faster than the session really ran. 50 also divides 100 exactly,
# so every GIF frame delay lands on a whole centisecond and playback does not
# drift against the recorded timing.
Set Framerate 50

# Everything up to the container's first prompt is hidden: the demo's claim is
# that one command takes a bare machine to a working nvim, and `docker run` is
# the harness that produces the bare machine, not a step the viewer performs.
# Recording resumes with the cursor already sitting at the container prompt.
Hide
Type "clear"
Enter

# `-it` because the recording needs cfgd's TTY output (spinners, colour), and
# `bash -l` because the source line cfgd injects lands in the login shell's own
# rc. The image bakes no brew PATH: before the apply `nvim` resolves to nothing,
# after it to the binary cfgd's own ~/.cfgd.env put on PATH. `--hostname
# jarvispro` names the container the same way the machine this demo stands in
# for is named, so the prompt reads as a real host rather than Docker's random
# hex id.
Type "docker run --rm -it --hostname jarvispro cfgd-demo:latest bash -l"
Enter
# The container prompt ends in the workdir name; matching `cfgd$` instead of a
# bare `$` keeps package-manager output from satisfying the wait early.
Wait@2m /cfgd\$$/
# `clear` inside the container: `Hide` pauses the recording without erasing
# the terminal, so without it the `docker run` line would open frame 0.
Type "clear"
Enter
Sleep 300ms
Show
Sleep 1s

Type "nvim"
Enter
Sleep 3s

Type "cfgd init --from tj-smith47/cfgd-config --apply-module nvim --theme dracula --yes"
Sleep 700ms
Enter
# apt + brew + rustup + Lazy/Mason/treesitter bootstrap; minutes, not seconds.
# The ceiling has to clear the module's own worst case, or vhs gives up
# mid-provision and records a truncated run instead of the failure the module
# would have reported. It is sized as the sum of the nvim module's postApply
# step timeouts (see the module's scripts) plus the package phase ahead of
# them, so it covers a run where every step burns its whole budget. A healthy
# take spends a fraction of it; the apply's own rollup line states what this
# one took.
Wait@60m /cfgd\$$/
# Everything from here plays at 1:1 — the ramp's tail starts at the moment the
# install finishes, so the payoff is soaked at real speed rather than ramping up
# only once the editor is already open. Long enough to read the apply summary
# before the next beat pushes it up.
Sleep 5s

# The one beat the apply cannot do for us: this shell was started before
# ~/.bashrc learned to source ~/.cfgd.env, so its PATH is still the pre-apply
# one. cfgd says exactly this in its post-apply reminder — the tape follows the
# instruction on screen rather than working around it.
Type "source ~/.cfgd.env"
Enter
Wait@30s /cfgd\$$/
Sleep 1s

# Bare `nvim`, not `nvim .`: a directory argument opens a file buffer, and the
# beat this frames is the configured start screen — alpha's dashboard, with
# nvim-tree opened beside it by alpha's own VimEnter hook.
Type "nvim"
Enter
Wait+Screen@20s /Welcome, TJ Smith!/
# The tree's first scan loses its race about half the time and paints an
# empty pane; waiting on real tree content turns a bad take into a hard
# failure instead of a silently-wrong GIF.
Wait+Screen@20s /Cargo\.toml/
# The first interactive start raises notifications that cover half the editor.
# They expire on their own, so this settles the screen rather than showing
# anything. The span is sized for the LAST of them, which is not a startup
# toast at all: lazy.nvim's update check fires once startup has finished, lands
# a couple of seconds after the tree populates and takes several more to
# animate out. Sized to the startup toasts alone, the settle ended while the
# update panel still covered the logo and the hero hold caught it mid-dismiss.
Sleep 12s
# The hero frames: LazyVim with the repo tree, no toasts left on top of it.
# Long enough to read the dashboard, short enough that the loop does not sit on
# a still image — at 6s the hold read as the recording having stalled.
Sleep 4s

Type ":qa"
Enter
# Quitting the alt screen sometimes restores a blank primary screen, which
# would leave the loop ending on black; the version line both refills the
# screen and closes on the fact the whole recording exists to establish.
# Restoring the primary screen takes a beat, and anything typed before it
# lands is swallowed rather than echoed.
Wait@30s /cfgd\$$/
Sleep 500ms
Type "nvim --version | head -1"
Enter
# The closing frame is the claim the whole recording exists to prove, and a
# loop that cuts away from it the instant it appears reads as an accident.
# The extra 3s over the read-time minimum is deliberate breathing room before
# the loop snaps back to the opening prompt.
Sleep 8s

# The ffmpeg pass that builds the GIF plays a head and a tail at 1:1 and
# decimates everything between them. Two spans must land inside those ends:
# the opening typing (~11s at 50ms plus container start, under a 22s head) and
# everything from the install finishing onward — the 5s summary read, the env
# source, nvim's start, the 12s settle, the 4s hero hold, the quit and the
# version beats (34.5s to 37.0s measured across takes, under a 41s tail). Only the
# install itself is compressed. Lengthen a beat here and the matching head/tail
# second in demo/scripts/make-gif.sh moves with it, or the ramp eats footage
# that has to play at real speed.
