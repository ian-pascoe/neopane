# neopane in zellij: the core demo. Record it with
#   scripts/demo/record.sh zellij-core
# The `# caption:`, `# keys:`, `# focus:`, `# highlight:` and `# speed:`
# comments drive both cuts, at the times this tape's timing gives them
# (tape.py): so the shown part uses Sleep, never Wait. Rectangles are
# fractions of the recording: X Y W H.

Require zellij
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
Type "scripts/demo/session.sh zellij"
Enter
Sleep 3s
# zellij's welcome float, dismissed for this session only. Once it has
# been dismissed it no longer shows, and the Escape reaches bash instead:
# Ctrl+C drops whatever that started, and Ctrl+L clears the screen
Escape
Sleep 1s
Ctrl+C
Sleep 300ms
Ctrl+L
Sleep 500ms
Show

# caption: In zellij, `neopane` starts one Neovim in the background
# speed: 2.5
Sleep 1s
Type "neopane src/main.rs"
# start:
Sleep 400ms
Enter
Sleep 3s

# caption: `:vsplit` opens a real zellij pane
# speed: 2
Type ":vsplit src/name.rs"
Sleep 300ms
Enter
# speed: 1.4
# highlight: 0.5 0 0.5 0.97
Sleep 2.5s

# caption: `:split` too
# speed: 2
Type ":split README.md"
Sleep 300ms
Enter
# speed: 1.4
# highlight: 0.5 0.47 0.5 0.5
Sleep 2.5s

# caption: zellij's own keys move Neovim's focus
# keys: Ctrl-p h
# VHS's Alt+h reaches zellij as Escape then h, which it doesn't take for
# Alt+h: pane mode's h moves the focus, and Escape leaves the mode
Ctrl+P
Type "h"
Sleep 300ms
Escape
# highlight: 0 0 0.5 0.97
Sleep 2s

# caption: Pickers open as zellij floating panes
# keys: Space Space
Space
Space
# highlight: 0.03 0.03 0.9 0.91
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

# caption: …split a pane with zellij itself…
# focus: all
# keys: Ctrl-p d
Ctrl+P
Type "d"
# highlight: 0 0.48 0.5 0.49
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

# caption: Close a zellij pane, and its window closes
# focus: all
# keys: Ctrl-p x
Ctrl+P
Type "x"
Sleep 3s
