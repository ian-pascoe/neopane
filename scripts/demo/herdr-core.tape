# neopane in herdr: the core demo. Record it with
#   scripts/demo/record.sh herdr-core
# The `# caption:`, `# keys:`, `# focus:`, `# highlight:` and `# speed:`
# comments drive both cuts, at the times this tape's timing gives them
# (tape.py): so the shown part uses Sleep, never Wait. Rectangles are
# fractions of the recording: X Y W H, here just outside herdr's own pane
# borders, below its tab bar and right of its collapsed sidebar.

Require herdr
Set Shell "bash"
Set FontFamily "GeistMono Nerd Font"
Set FontSize 22
Set Width 1920
Set Height 996
Set Padding 12
Set Theme "Horizon"
Set TypingSpeed 55ms
# VHS can't take 1920x996 screenshots much faster: at more, the video it
# writes runs fast
Set Framerate 10

Hide
Type "scripts/demo/session.sh herdr"
Enter
# herdr's sidebar, collapsed to a strip, to give the panes the room: once
# its client has drawn, or the keys go before it reads them
Wait+Screen /spaces/
Sleep 1s
Ctrl+B
Sleep 500ms
Type "b"
Sleep 1s
Show

# caption: In herdr, `neopane` starts one Neovim in the background
# speed: 2.5
Sleep 1s
Type "neopane src/main.rs"
# start:
Sleep 400ms
Enter
Sleep 3s

# caption: `:vsplit` opens a real herdr pane
# speed: 2
Type ":vsplit src/name.rs"
Sleep 300ms
Enter
# speed: 1.4
# herdr opens a pane in a tab of its own, then moves it (ADR 0017): the
# outline waits for it
Sleep 1s
# highlight: 0.515 0.052 0.477 0.922
Sleep 2s

# caption: `:split` too
# speed: 2
Type ":split README.md"
Sleep 300ms
Enter
# speed: 1.4
Sleep 1s
# highlight: 0.515 0.539 0.477 0.435
Sleep 2s

# caption: herdr's own keys move Neovim's focus
# keys: Ctrl-b h
Ctrl+B
Type "h"
# highlight: 0.036 0.052 0.477 0.922
Sleep 2s

# caption: Pickers float over their own pane
# keys: Space Space
Space
Space
# highlight: 0.036 0.052 0.477 0.922
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

# caption: …split a pane with herdr itself…
# focus: all
# keys: Ctrl-b -
Ctrl+B
Type "-"
# highlight: 0.036 0.539 0.477 0.435
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

# caption: Close a herdr pane, and its window closes
# focus: all
# keys: Ctrl-b x
Ctrl+B
Type "x"
Sleep 3s
