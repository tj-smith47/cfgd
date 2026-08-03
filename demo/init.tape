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
Set FontSize 16
Set Width 1100
Set Height 640
Set TypingSpeed 60ms
Set Padding 20

# 20 divides 100 exactly, so every GIF frame delay lands on a whole
# centisecond and playback does not drift against the recorded timing.
Set Framerate 20

Hide
Type "clear"
Enter
Show

# `-it` because the recording needs cfgd's TTY output (spinners, colour), and
# `bash -l` so the container's own login PATH is what resolves `nvim` — before
# the install it resolves to nothing, after it to the brew-installed binary in
# the same shell.
Type "docker run --rm -it cfgd-demo:latest bash -l"
Enter
# The container prompt ends in the workdir name; matching `cfgd$` instead of a
# bare `$` keeps package-manager output from satisfying the wait early.
Wait@2m /cfgd\$$/
Sleep 1s

Type "nvim ."
Enter
Sleep 3s

Type "cfgd init --from https://github.com/tj-smith47/cfgd-config.git --apply-module nvim --yes"
Sleep 700ms
Enter
# apt + brew + rustup + Lazy/Mason/treesitter bootstrap; minutes, not seconds.
Wait@30m /cfgd\$$/
# Long enough to read the apply summary before the alt screen covers it. It does
# not settle the file tree: nvim-tree's first scan still loses its race about
# half the time and paints an empty pane, which no amount of waiting here fixes.
Sleep 3s

Type "nvim ."
Enter
# The tree's first scan loses its race about half the time and paints an
# empty pane; waiting on real tree content turns a bad take into a hard
# failure instead of a silently-wrong GIF.
Wait+Screen@20s /Cargo\.toml/
Sleep 8s

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
Sleep 3s

# The spans above and below the install wait are the only footage played at
# 1:1 (~14s and ~17s). The ffmpeg pass that builds the GIF decimates
# everything between them, so its head/tail seconds must stay comfortably
# ABOVE these — 17/20 today. Set them lower and the typing or the editor
# beat gets sped up, which is the one thing the ramp must never do.
