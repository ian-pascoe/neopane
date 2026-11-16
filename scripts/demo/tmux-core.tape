# neopane in tmux: the core demo. Record it with
#   scripts/demo/record.sh tmux-core
# The `# caption:`, `# keys:`, `# focus:`, `# highlight:` and `# speed:`
# comments drive both cuts, at the times this tape's timing gives them
# (tape.py): so the shown part uses Sleep, never Wait. Rectangles are
# fractions of the recording: X Y W H.

Require tmux
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
Type "scripts/demo/session.sh tmux"
Enter
Sleep 2s
Show

# caption: In tmux, `neopane` starts one Neovim in the background
# speed: 2.5
Sleep 1s
Type "neopane src/main.rs"
Sleep 400ms
Enter
Sleep 3s

# caption: `:vsplit` opens a real tmux pane
# speed: 2
Type ":vsplit src/name.rs"
Sleep 300ms
Enter
# speed: 1.4
# highlight: 0.5 0 0.5 0.97
Sleep 2.5s

# caption: `:split` too, where Neovim puts it
# speed: 2
Type ":split README.md"
Sleep 300ms
Enter
# speed: 1.4
# highlight: 0.5 0.47 0.5 0.5
Sleep 2.5s

# caption: tmux's own keys move Neovim's focus
# keys: Ctrl-b ←
Ctrl+B
Left
# highlight: 0 0 0.5 0.97
Sleep 2s

# caption: Pickers open as tmux floating panes
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

# caption: …split a pane with tmux itself…
# focus: all
# keys: Ctrl-b "
Ctrl+B
Type `"`
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

# caption: Close a tmux pane, and its window closes
# focus: all
# keys: Ctrl-b x y
Ctrl+B
Type "x"
Sleep 800ms
Type "y"
Sleep 3s
