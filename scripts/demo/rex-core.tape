# neopane in Rex: the core demo. Record it with
#   scripts/demo/record.sh rex-core
# VHS can't record Rex, a GUI app: rex/take.sh plays this tape in Rex.app
# instead, through the rex CLI (rex/play.py). It plays Type, keys and
# Sleep as VHS does, and a `Rex ARGS` line runs `rex ARGS`, for what Rex's
# own keys do: the `# keys:` mark above it names the key, Rex for Mac's
# default. The `# caption:`, `# keys:`, `# focus:`, `# highlight:` and
# `# speed:` comments drive both cuts, at the times this tape's timing
# gives them (tape.py): so the shown part uses Sleep, never Wait.
# Rectangles are fractions of the recording: X Y W H, which take.sh pads
# a little at the sides.

Set TypingSpeed 55ms

Hide
Ctrl+L
Sleep 500ms
Show

# caption: In Rex, `neopane` starts one Neovim in the background
# speed: 2.5
Sleep 1s
Type "neopane src/main.rs"
# start:
Sleep 400ms
Enter
Sleep 3s

# caption: `:vsplit` opens a real Rex block
# speed: 2
Type ":vsplit src/name.rs"
Sleep 300ms
Enter
# speed: 1.4
# highlight: 0.502 0.003 0.49 0.994
Sleep 2.5s

# caption: `:split` too
# speed: 2
Type ":split README.md"
Sleep 300ms
Enter
# speed: 1.4
# highlight: 0.502 0.5 0.49 0.497
Sleep 2.5s

# caption: Rex's own keys move Neovim's focus
# keys: Opt-Cmd ←
Rex focus -s {session} left
# highlight: 0.008 0.003 0.49 0.994
Sleep 2s

# caption: Pickers open as Rex floating panes
# keys: Space Space
Space
Space
# highlight: 0.088 0.052 0.832 0.836
Sleep 1.5s
# speed: 1.5
Type "pane_view"
Sleep 1.2s
Enter
# speed: 1.4
Sleep 2s

# caption: Every block shares one rust-analyzer
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

# caption: …split a block with Rex itself…
# focus: all
# keys: Shift-Cmd D
Rex split -s {session} --split=below --shell=none -- {shell}
# highlight: 0.008 0.5 0.49 0.497
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

# caption: Close a Rex block, and its window closes
# focus: all
# keys: Cmd W
Rex api call -s {session} block.close '{{"window_id": "{window_id}"}}'
Sleep 3s
