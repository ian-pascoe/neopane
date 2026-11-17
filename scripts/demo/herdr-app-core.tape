# neopane in the Herdr app: the core demo. Record it with
#   scripts/demo/record.sh herdr-app-core
# VHS can't record the Herdr app, a GUI app: herdr-app/take.sh plays this
# tape in a private Herdr app window instead, on a private hc core, and
# records it hidden (herdr-app/play.py). It plays Type, keys and Sleep as
# VHS does, into the focused pane's terminal through the core; and a
# `HerdrApp ARGS` line runs `herdr-app ARGS`, for what the app's own keys
# do: the `# keys:` mark above it names the key, the app's default on
# macOS. The `# caption:`, `# keys:`, `# focus:`, `# highlight:` and
# `# speed:` comments drive both cuts, at the times this tape's timing
# gives them (tape.py): so the shown part uses Sleep, never Wait.
# Rectangles are fractions of the recording: X Y W H, here on
# the app's own pane borders, which take.sh pads a little at the sides.

Set TypingSpeed 55ms

Hide
Ctrl+L
Sleep 500ms
Show

# caption: In the Herdr app, `neopane` starts one Neovim in the background
# speed: 2.5
Sleep 1s
Type "neopane src/main.rs"
# start:
Sleep 400ms
Enter
Sleep 3s

# caption: `:vsplit` opens a real Herdr app pane
# speed: 2
Type ":vsplit src/name.rs"
Sleep 300ms
Enter
# speed: 1.4
# highlight: 0.503 0.002 0.482 0.965
Sleep 2.5s

# caption: `:split` too
# speed: 2
Type ":split README.md"
Sleep 300ms
Enter
# speed: 1.4
# highlight: 0.503 0.491 0.482 0.476
Sleep 2.5s

# caption: The app's own keys move Neovim's focus
# keys: Opt-Cmd ←
HerdrApp do pane.go --direction left
# highlight: 0.009 0.002 0.486 0.965
Sleep 2s

# caption: Pickers draw over their window, in its own pane
# keys: Space Space
Space
Space
# highlight: 0.009 0.002 0.486 0.965
Sleep 1.5s
# speed: 1.5
Type "pane_view"
Sleep 1.2s
Enter
# speed: 1.4
Sleep 2s

# caption: Every pane shares one rust-analyzer
# speed: 2
Type "/Grids, MAX"
Enter
Sleep 600ms
# speed: 1.4
# focus: 0 0.3 0.5 0.5
# keys: K
Type "K"
Sleep 3s

# caption: Yank a line here…
# keys: y y
Type "yy"
Sleep 1.5s

# caption: …split a pane with the app itself…
# focus: all
# keys: Shift-Cmd D
HerdrApp do pane.split --direction down
# highlight: 0.009 0.491 0.486 0.476
Sleep 1.5s

# caption: …and `neopane attach` makes it Neovim's
# focus: 0 0.4 0.6 0.6
# speed: 2
Type "neopane attach notes.md"
Sleep 400ms
Enter
# speed: 1.4
Sleep 2.5s

# caption: …then paste: one Neovim, one set of registers
# keys: p
Type "p"
Sleep 3s

# caption: Close a Herdr app pane, and its window closes
# focus: all
# keys: Shift-Cmd W
HerdrApp do pane.close
Sleep 3s
