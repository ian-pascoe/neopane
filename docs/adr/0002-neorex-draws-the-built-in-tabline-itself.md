# neorex draws the built-in tabline itself

While `'tabline'` is empty, Neovim draws its built-in tabline, and under neorex
it is wrong twice over (#63). It can't be fitted: it lays out across all of
Neovim's screen, with no expression to evaluate at the tabline width or pad, so
beside a foreign pane its later labels and close button are never shown. And
its labels are wrong even with no foreign pane: every external window sits in
the current tab page, so its label counts the anchor and every tab page's
windows and shows `+` for any of their buffers, while other tab pages, holding
only their anchors, never show a count or `+`, and are named after the
anchor's scratch buffer once their remembered window closes. So while
`'tabline'` is empty, neorex sets it to a `%!` Lua tabline of its own, the
copy: Neovim 0.12's `draw_tabline` ported cell by cell onto a row as wide as
the tabline width (else the screen's), labelling each tab page from its own
windows, those whose home tab page it is, and emitted as a format string padded
as every fitted tabline is.

## Considered Options

- **Leave the built-in tabline.** Nothing to keep in step with Neovim, but it
  stays clipped beside a foreign pane and its counts and names stay wrong.
- **Use the copy only while there's a tabline width.** Keeps the built-in
  whenever nothing is hidden, but the labels would change, counts and names
  and all, as foreign panes come and go along the top of the Rex window.
- **`ext_tabline`, drawn by the daemon.** The UI gets only each tab page's
  name, no counts or `+`, and it turns off every `'tabline'` plugin too.

## Consequences

- The copy follows Neovim 0.12's `draw_tabline` (`statusline.c`): label
  width, dropped tab pages, name shortening and trimming, the close button,
  showcmd and click targets. It must be checked against `draw_tabline` again on
  every Neovim upgrade; the tests compare it, cell for cell, with the built-in
  tabline of a Neovim without neorex.
- `'tabline'` reads the copy's one fixed value, which neorex treats as empty: a
  plugin that saved it and puts it back gets the live built-in tabline. Any
  other value is shown instead, fitted as before, and empty brings the copy
  back. `:set tabline?` shows neorex's value. As Neovim quits, neorex sets it
  back to empty.
- A `'tabline'` draws every highlight combined over TabLineFill, so the copy
  draws its labels in groups of its own, `NeorexTabLineSel` and
  `NeorexTabLine`: TabLineSel's and TabLine's attributes, colors they leave
  unset taken from Normal, and `nocombine`. A color unset in Normal too still
  comes from TabLineFill. A label window's `'winhighlight'` is ignored.
- A format string can't say the built-in's click target after the last label,
  so a `%@` handler stands in for it: a click goes to the next tab page, a
  double click opens one at the end, the middle button closes the current one.
- A wide character whose right half the built-in overwrites, as with the close
  button, is left half-drawn by Neovim; no format string can draw that, and
  the copy draws a blank there.
- Neovim redraws no tabline for an external window closing, as it would for a
  split, so neorex redraws the copy when one closes, and when it reopens one
  in a scheduled callback.
