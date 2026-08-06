# `Output <dir>/` is a no-op on the installed vhs (v0.11.0) — it exits 0 and
# writes nothing, with the real frames landing in a /tmp scratch dir that vhs
# deletes on cleanup. The mp4 is therefore the only artifact this tape
# actually produces, and it is what the GIF ramp reads.
Output "demo/.out/raw.mp4"

Require docker

Set Shell "bash"
# LazyVim's tree, tabline and statusline are drawn with Nerd Font glyphs; the
# default font renders every one of them as a tofu box.
Set FontFamily "JetBrainsMono Nerd Font Mono"
# 1320x1000 at font 15 is a measured 125x47 terminal (cell 10.24x20.43 after
# the 20px padding each side) — wide and tall enough that LazyVim's tree,
# tabline and statusline frame a real editing view rather than a cropped one,
# with the extra rows going to the install log so more of it is on screen at
# once. Changing any of the three changes the grid; re-measure with
# `echo $(tput cols)x$(tput lines)` rather than deriving it. The GIF is not
# rescaled, so this width is also the GIF's width and the README img width.
Set FontSize 15
Set Width 1320
Set Height 1000
# 50ms lands exactly two keystrokes in every 100ms frame. A speed that does not
# divide the frame period evenly (40ms lands 2.5 chars per frame) alternates 2
# and 3 characters per frame, which reads as stuttering rather than typing.
Set TypingSpeed 50ms
Set Padding 20

# The GIF is encoded at 10fps, so recording any faster only makes vhs's
# headless chromium screenshot a 1320x1000 viewport twice as often for frames
# the encoder throws away — and a take that cannot sustain its declared rate
# plays back faster than the session really ran. 10 also divides 100 exactly,
# so every GIF frame delay lands on a whole centisecond and playback does not
# drift against the recorded timing.
Set Framerate 10

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
# after it to the binary cfgd's own ~/.cfgd.env put on PATH.
Type "docker run --rm -it cfgd-demo:latest bash -l"
Enter
# The container prompt ends in the workdir name; matching `cfgd$` instead of a
# bare `$` keeps package-manager output from satisfying the wait early.
Wait@2m /cfgd\$$/
Sleep 1s
Show

Type "nvim"
Enter
Sleep 3s

Type "cfgd init --from https://github.com/tj-smith47/cfgd-config.git --apply-module nvim --theme dracula --yes"
Sleep 700ms
Enter
# apt + brew + rustup + Lazy/Mason/treesitter bootstrap; minutes, not seconds.
# The ceiling has to clear the module's own worst case, or vhs gives up
# mid-provision and records a truncated run instead of the failure the module
# would have reported. The nvim module's postApply steps allot 50m between them
# (120s + 900s + 600s + 900s + 300s + 180s) and the package phase ahead of them
# has measured ~6m, so 60m covers a run where every step burns its whole budget.
# A healthy take spends nowhere near it: the take this GIF was cut from ran
# 7m03s end to end, with MasonToolsInstallSync — the step the ceiling was
# sized around — returning in 70s.
Wait@60m /cfgd\$$/
# Everything from here plays at 1:1 — the ramp's tail starts at the moment the
# install finishes, so the payoff is soaked at real speed rather than ramping up
# only once the editor is already open. Long enough to read the apply summary
# before the alt screen covers it. It does not settle the file tree: nvim-tree's
# first scan still loses its race about half the time and paints an empty pane,
# which no amount of waiting here fixes.
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
# anything. It now plays at 1:1 inside the tail, so it is cut to the shortest
# span that still outlasts the toasts rather than the ramp-compressed 14s.
Sleep 5s
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
Sleep 5s

# The ffmpeg pass that builds the GIF plays a head and a tail at 1:1 and
# decimates everything between them. Two spans must land inside those ends:
# the opening typing (~11s at 50ms plus container start, under a 22s head) and
# everything from the install finishing onward — the 5s summary read, the env
# source, nvim's start, the 5s settle, the 4s hero hold, the quit and the
# version beats (~31s plus nvim's variable start, under a 34s tail). Only the
# install itself is compressed. Lengthen a beat here and the matching head/tail
# second in demo/scripts/make-gif.sh moves with it, or the ramp eats footage
# that has to play at real speed.
